//! A tiny HTTP server running on an EC2 instance.
//!
//! Generate the AWS SDK the program depends on, then run it:
//!
//! ```sh
//! pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
//! pulumi up
//! ```

/// Cloud-init script: write a page and serve the directory on port 80.
const USER_DATA: &str = r#"#!/bin/bash
echo "Hello, World from Pulumi!" > index.html
nohup python3 -m http.server 80 &
"#;

fn main() {
    pulumi::run(|ctx| async move {
        // `pulumi config set instanceType t3.small` to override.
        let instance_type = ctx
            .config()
            .get_string_or("instanceType", pulumi::PropertyValue::String("t3.micro".into()));

        // Look up the newest Amazon Linux 2023 AMI in the current region
        // rather than hard-coding an image ID that only exists in one.
        let ami = pulumi_aws::ec2::get_ami(
            &ctx,
            pulumi_aws::ec2::GetAmiArgs {
                most_recent: Some(pulumi::Output::known(true)),
                owners: Some(pulumi::Output::known(vec!["amazon".to_string()])),
                filters: Some(vec![pulumi_aws::types::Ec2GetAmiFilterArgs {
                    name: Some(pulumi::Output::known("name".to_string())),
                    values: Some(pulumi::Output::known(vec!["al2023-ami-2023.*-x86_64".to_string()])),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            pulumi::InvokeOptions::default(),
        );

        // Open port 80 to the world, and let the instance talk out.
        let group = pulumi_aws::ec2::SecurityGroup::new(
            &ctx,
            "web-secgrp",
            pulumi_aws::ec2::SecurityGroupArgs {
                description: Some(pulumi::Output::known("Enable HTTP access".to_string())),
                ingress: Some(vec![pulumi_aws::types::Ec2SecurityGroupIngressArgs {
                    description: Some(pulumi::Output::known("HTTP from anywhere".to_string())),
                    protocol: Some(pulumi::Output::known("tcp".to_string())),
                    from_port: Some(pulumi::Output::known(80)),
                    to_port: Some(pulumi::Output::known(80)),
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

        let server = pulumi_aws::ec2::Instance::new(
            &ctx,
            "web-server-www",
            pulumi_aws::ec2::InstanceArgs {
                // The invoke resolves to a struct; pull the one field out.
                ami: Some(ami.map(|a: pulumi_aws::types::Ec2GetAmiResult| a.id)),
                instance_type: Some(instance_type.cast()),
                // Passing the group's id here makes the engine create the
                // security group first and records the dependency in state.
                vpc_security_group_ids: Some(group.id().map(|id: String| vec![id])),
                user_data: Some(pulumi::Output::known(USER_DATA.to_string())),
                tags: Some(pulumi::Output::known(std::collections::BTreeMap::from([(
                    "Name".to_string(),
                    "web-server-www".to_string(),
                )]))),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        ctx.export("publicIp", server.public_ip().cast::<pulumi::PropertyValue>());
        ctx.export("publicDns", server.public_dns().cast::<pulumi::PropertyValue>());
        ctx.export(
            "url",
            pulumi::pv::concat(vec![
                pulumi::pv::string("http://"),
                server.public_dns().cast(),
            ]),
        );

        Ok(())
    });
}
