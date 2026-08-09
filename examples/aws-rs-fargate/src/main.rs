//! Run a container on AWS Fargate behind an Application Load Balancer.
//!
//! Fargate is ECS without any EC2 instances to manage: the task definition
//! says what to run, the service keeps N copies of it alive, and AWS finds
//! somewhere to put them. The program looks the account's default VPC and
//! its subnets up with invokes rather than creating a network, so the whole
//! thing is a handful of resources.
//!
//! The program depends on a generated AWS SDK, so generate that first:
//!
//! ```sh
//! pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
//! pulumi up
//! ```

/// The container the service runs, and the port it listens on. These three
/// values appear in the task definition's JSON, in the load balancer's
/// target group, and in the service's load-balancer block, so they are
/// constants rather than three copies of the same literal.
const CONTAINER_NAME: &str = "nginx";
const CONTAINER_IMAGE: &str = "nginx:1.27-alpine";
const CONTAINER_PORT: i32 = 80;

/// Lets the ECS service assume the task execution role. Note the principal:
/// `ecs-tasks.amazonaws.com`, not `ecs.amazonaws.com`.
const ASSUME_ROLE_POLICY: &str = r#"{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": "sts:AssumeRole",
      "Principal": { "Service": "ecs-tasks.amazonaws.com" }
    }
  ]
}"#;

/// The AWS-managed policy that lets the *ECS agent* — not the container —
/// pull the image and write the task's logs.
const TASK_EXECUTION_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

fn main() {
    pulumi::run(|ctx| async move {
        // `pulumi config set desiredCount 5` to run more copies.
        let desired_count = ctx
            .config()
            .get_int_or("desiredCount", pulumi::PropertyValue::Number(2.0));

        // The default VPC and its subnets, looked up instead of created.
        let vpc = pulumi_aws::ec2::get_vpc(
            &ctx,
            pulumi_aws::ec2::GetVpcArgs {
                default: Some(pulumi::Output::known(true)),
                ..Default::default()
            },
            pulumi::InvokeOptions::default(),
        );

        // Feeding the VPC's id into the subnet filter is what orders the two
        // invokes: the second cannot run until the first has resolved.
        let subnets = pulumi_aws::ec2::get_subnets(
            &ctx,
            pulumi_aws::ec2::GetSubnetsArgs {
                filters: Some(vec![pulumi_aws::types::Ec2GetSubnetsFilterArgs {
                    name: Some(pulumi::pv::string("vpc-id").cast()),
                    values: Some(vpc.map(|v: pulumi_aws::types::Ec2GetVpcResult| vec![v.id])),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            pulumi::InvokeOptions::default(),
        );

        // Both the load balancer and the tasks sit in this group: the world
        // may reach port 80, and anything inside may talk out — which the
        // tasks need in order to pull the image from Docker Hub.
        let security_group = pulumi_aws::ec2::SecurityGroup::new(
            &ctx,
            "web-secgrp",
            pulumi_aws::ec2::SecurityGroupArgs {
                description: Some(pulumi::pv::string("Allow HTTP in and everything out").cast()),
                vpc_id: Some(vpc.map(|v: pulumi_aws::types::Ec2GetVpcResult| v.id)),
                ingress: Some(vec![pulumi_aws::types::Ec2SecurityGroupIngressArgs {
                    description: Some(pulumi::Output::known("HTTP from anywhere".to_string())),
                    protocol: Some(pulumi::Output::known("tcp".to_string())),
                    from_port: Some(pulumi::Output::known(CONTAINER_PORT)),
                    to_port: Some(pulumi::Output::known(CONTAINER_PORT)),
                    cidr_blocks: Some(pulumi::Output::known(vec!["0.0.0.0/0".to_string()])),
                    ..Default::default()
                }]),
                egress: Some(vec![pulumi_aws::types::Ec2SecurityGroupEgressArgs {
                    description: Some(pulumi::Output::known("Allow all outbound".to_string())),
                    // "-1" is every protocol, which requires a 0-0 port range.
                    protocol: Some(pulumi::Output::known("-1".to_string())),
                    from_port: Some(pulumi::Output::known(0)),
                    to_port: Some(pulumi::Output::known(0)),
                    cidr_blocks: Some(pulumi::Output::known(vec!["0.0.0.0/0".to_string()])),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The cluster the service runs in. Every input is optional, so this
        // one is a `Default` away from empty.
        let cluster = pulumi_aws::ecs::Cluster::new(
            &ctx,
            "app-cluster",
            pulumi_aws::ecs::ClusterArgs::default(),
            pulumi::ResourceOptions::default(),
        );

        // The task execution role.
        let execution_role = pulumi_aws::iam::Role::new(
            &ctx,
            "task-execution-role",
            pulumi_aws::iam::RoleArgs {
                assume_role_policy: Some(pulumi::pv::string(ASSUME_ROLE_POLICY).cast()),
                description: Some(
                    pulumi::pv::string("Lets the ECS agent pull images and write logs").cast(),
                ),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        pulumi_aws::iam::RolePolicyAttachment::new(
            &ctx,
            "task-execution-policy",
            pulumi_aws::iam::RolePolicyAttachmentArgs {
                role: Some(execution_role.name().cast()),
                policy_arn: Some(pulumi::pv::string(TASK_EXECUTION_POLICY_ARN).cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // An internet-facing Application Load Balancer across every subnet
        // of the default VPC.
        let load_balancer = pulumi_aws::lb::LoadBalancer::new(
            &ctx,
            "web-lb",
            pulumi_aws::lb::LoadBalancerArgs {
                load_balancer_type: Some(pulumi::pv::string("application").cast()),
                internal: Some(pulumi::pv::bool(false).cast()),
                security_groups: Some(security_group.id().map(|id: String| vec![id])),
                subnets: Some(subnets.map(|s: pulumi_aws::types::Ec2GetSubnetsResult| s.ids)),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Fargate tasks get their own ENI, so the load balancer reaches them
        // by IP: `target_type` must be "ip", not the default "instance".
        let target_group = pulumi_aws::lb::TargetGroup::new(
            &ctx,
            "web-tg",
            pulumi_aws::lb::TargetGroupArgs {
                port: Some(pulumi::Output::known(CONTAINER_PORT)),
                protocol: Some(pulumi::pv::string("HTTP").cast()),
                target_type: Some(pulumi::pv::string("ip").cast()),
                vpc_id: Some(vpc.map(|v: pulumi_aws::types::Ec2GetVpcResult| v.id)),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The listener ties the two together: everything arriving on port 80
        // is forwarded to the target group.
        let listener = pulumi_aws::lb::Listener::new(
            &ctx,
            "web-listener",
            pulumi_aws::lb::ListenerArgs {
                load_balancer_arn: Some(load_balancer.arn().cast()),
                default_actions: Some(vec![pulumi_aws::types::LbListenerDefaultActionArgs {
                    r#type: Some(pulumi::pv::string("forward").cast()),
                    target_group_arn: Some(target_group.arn().cast()),
                    ..Default::default()
                }]),
                port: Some(pulumi::Output::known(CONTAINER_PORT)),
                protocol: Some(pulumi::pv::string("HTTP").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // ECS wants the container definitions as a JSON *string*, so this is
        // a string the program builds itself.
        //
        // `pulumi::pv::to_json` would be the obvious tool, but it renders
        // every number as a JSON float — `80.0` — and the ECS API rejects a
        // float where it expects an integer port. A formatted string keeps
        // the integers integral, and the constants above keep the container
        // name and port in step with the target group below.
        let container_definitions = format!(
            r#"[
  {{
    "name": "{name}",
    "image": "{image}",
    "cpu": 256,
    "memory": 512,
    "essential": true,
    "portMappings": [
      {{ "containerPort": {port}, "hostPort": {port}, "protocol": "tcp" }}
    ]
  }}
]"#,
            name = CONTAINER_NAME,
            image = CONTAINER_IMAGE,
            port = CONTAINER_PORT,
        );

        // What to run. `cpu` and `memory` are strings, and Fargate only
        // accepts certain pairs: 256 CPU units (a quarter vCPU) goes with 512,
        // 1024 or 2048 MiB.
        let task_definition = pulumi_aws::ecs::TaskDefinition::new(
            &ctx,
            "app-task",
            pulumi_aws::ecs::TaskDefinitionArgs {
                container_definitions: Some(pulumi::pv::string(container_definitions).cast()),
                family: Some(pulumi::pv::string("aws-rs-fargate-nginx").cast()),
                cpu: Some(pulumi::pv::string("256").cast()),
                memory: Some(pulumi::pv::string("512").cast()),
                // Fargate requires both of these.
                network_mode: Some(pulumi::pv::string("awsvpc").cast()),
                requires_compatibilities: Some(pulumi::Output::known(vec!["FARGATE".to_string()])),
                execution_role_arn: Some(execution_role.arn().cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The service keeps `desired_count` copies of the task running and
        // registers each one with the target group.
        pulumi_aws::ecs::Service::new(
            &ctx,
            "app-service",
            pulumi_aws::ecs::ServiceArgs {
                cluster: Some(cluster.arn().cast()),
                task_definition: Some(task_definition.arn().cast()),
                desired_count: Some(desired_count.cast()),
                launch_type: Some(pulumi::pv::string("FARGATE").cast()),
                network_configuration: Some(
                    pulumi_aws::types::EcsServiceNetworkConfigurationArgs {
                        subnets: Some(subnets.map(|s: pulumi_aws::types::Ec2GetSubnetsResult| s.ids)),
                        security_groups: Some(security_group.id().map(|id: String| vec![id])),
                        // The default VPC's subnets are public and have no
                        // NAT gateway, so without a public IP the task
                        // cannot reach Docker Hub to pull the image.
                        assign_public_ip: Some(pulumi::pv::bool(true).cast()),
                        ..Default::default()
                    },
                ),
                load_balancers: Some(vec![pulumi_aws::types::EcsServiceLoadBalancerArgs {
                    // Which container in the task definition to register —
                    // matched by name, hence the shared constant.
                    container_name: Some(pulumi::pv::string(CONTAINER_NAME).cast()),
                    container_port: Some(pulumi::Output::known(CONTAINER_PORT)),
                    target_group_arn: Some(target_group.arn().cast()),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            pulumi::ResourceOptions {
                // The service registers targets with the target group, and
                // AWS rejects that until the group is attached to a load
                // balancer — which is what creating the listener does.
                // Nothing in the service's inputs mentions the listener, so
                // without this the engine is free to create them in
                // parallel.
                depends_on: vec![listener.pulumi_resource().clone()],
                ..Default::default()
            },
        );

        ctx.export(
            "url",
            pulumi::pv::concat(vec![
                pulumi::pv::string("http://"),
                load_balancer.dns_name().cast(),
            ]),
        );
        ctx.export(
            "clusterName",
            cluster.name().cast::<pulumi::PropertyValue>(),
        );
        ctx.export(
            "loadBalancerDns",
            load_balancer.dns_name().cast::<pulumi::PropertyValue>(),
        );

        Ok(())
    });
}
