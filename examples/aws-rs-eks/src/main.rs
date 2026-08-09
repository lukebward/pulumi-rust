//! A managed Kubernetes cluster on AWS (EKS).
//!
//! The AWS counterpart to `azure-rs-aks` and `gcp-rs-gke`, built out of the
//! plain `aws:eks` resources rather than the `eks` component package. The
//! account's default VPC and its subnets are looked up with the
//! `aws:ec2:getVpc` and `aws:ec2:getSubnets` invokes, so the program creates
//! no network of its own; on top of that sit two IAM roles — one the control
//! plane assumes, one the nodes assume — an `eks:Cluster`, and an
//! `eks:NodeGroup` whose size and instance type are configurable.
//!
//! The cluster's endpoint and certificate authority are folded into a
//! kubeconfig and exported as a secret stack output.
//!
//! The program depends on a generated AWS SDK, so generate that first:
//!
//! ```sh
//! pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
//! pulumi config set aws:region us-west-2
//! pulumi up
//! ```

use pulumi::{Output, PropertyValue};
use pulumi_aws::{ec2, eks, iam, types};

/// How many nodes the managed node group runs when `nodeCount` is not
/// configured.
const DEFAULT_NODE_COUNT: f64 = 2.0;

/// The instance type used when `instanceType` is not configured. Two vCPUs
/// and 4 GiB is the smallest shape that comfortably runs the EKS system pods
/// (CoreDNS, kube-proxy and the VPC CNI) alongside anything of your own.
const DEFAULT_INSTANCE_TYPE: &str = "t3.medium";

/// Lets the EKS control plane assume the cluster role. Note the principal:
/// `eks.amazonaws.com`, which is the service that manages the API server —
/// not `ec2.amazonaws.com`, which is what the *nodes* run as.
///
/// `sts:TagSession` sits alongside `sts:AssumeRole` because EKS passes
/// session tags when it assumes the role; the AWS provider's own
/// documentation for `aws:eks:Cluster` grants both.
const CLUSTER_ASSUME_ROLE_POLICY: &str = r#"{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": ["sts:AssumeRole", "sts:TagSession"],
      "Principal": { "Service": "eks.amazonaws.com" }
    }
  ]
}"#;

/// Lets an EC2 instance assume the node role. Managed node group members are
/// ordinary EC2 instances, so the principal here is the EC2 service and the
/// role is delivered to each node through an instance profile that EKS
/// creates on your behalf.
const NODE_ASSUME_ROLE_POLICY: &str = r#"{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": "sts:AssumeRole",
      "Principal": { "Service": "ec2.amazonaws.com" }
    }
  ]
}"#;

/// What the control plane needs to manage AWS resources on the cluster's
/// behalf — network interfaces, security groups and load balancers.
const CLUSTER_POLICY_ARN: &str = "arn:aws:iam::aws:policy/AmazonEKSClusterPolicy";

/// The three policies every managed node group's role needs: to join the
/// cluster and describe it, to hand pods VPC IP addresses through the CNI
/// plugin, and to pull images out of ECR.
const WORKER_NODE_POLICY_ARN: &str = "arn:aws:iam::aws:policy/AmazonEKSWorkerNodePolicy";
const CNI_POLICY_ARN: &str = "arn:aws:iam::aws:policy/AmazonEKS_CNI_Policy";
const ECR_READ_ONLY_POLICY_ARN: &str = "arn:aws:iam::aws:policy/AmazonEC2ContainerRegistryReadOnly";

/// The kubeconfig this program hands back, with `%s` marking each place a
/// cluster output is spliced in. There are four holes, filled in order by the
/// cluster's certificate authority, its API server endpoint, the region it
/// lives in, and its name.
///
/// No token is embedded. The `exec` block shells out to `aws eks get-token`,
/// which mints a short-lived one from whatever AWS credentials the caller
/// already has — the same arrangement `aws eks update-kubeconfig` writes, and
/// what keeps an exported kubeconfig usable from more than one machine.
const KUBECONFIG_TEMPLATE: &str = r#"apiVersion: v1
clusters:
- cluster:
    certificate-authority-data: %s
    server: %s
  name: kubernetes
contexts:
- context:
    cluster: kubernetes
    user: aws
  name: aws
current-context: aws
kind: Config
preferences: {}
users:
- name: aws
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: aws
      args:
        - --region
        - %s
        - eks
        - get-token
        - --cluster-name
        - %s
        - --output
        - json
"#;

fn main() {
    pulumi::run(|ctx| async move {
        let config = ctx.config();

        // `pulumi config set nodeCount 3` and
        // `pulumi config set instanceType t3.large` to override.
        let node_count = config.get_int_or("nodeCount", PropertyValue::Number(DEFAULT_NODE_COUNT));
        let instance_type = config.get_string_or(
            "instanceType",
            PropertyValue::String(DEFAULT_INSTANCE_TYPE.to_string()),
        );

        // ---------------------------------------------------------------
        // The network, looked up rather than created.
        //
        // EKS needs subnets in at least two availability zones. A default
        // VPC has one subnet per zone, so `getSubnets` over it satisfies
        // that in every region — which is what lets this example stay a
        // handful of resources instead of a VPC build-out.
        //
        // Both args structs have only optional inputs, so they derive
        // `Default` and the unset fields can be elided.
        // ---------------------------------------------------------------
        let vpc = ec2::get_vpc(
            &ctx,
            ec2::GetVpcArgs {
                default: Some(Output::known(true)),
                ..Default::default()
            },
            pulumi::InvokeOptions::default(),
        );

        // Feeding the VPC's id into the subnet filter is what orders the two
        // invokes: the second cannot run until the first has resolved.
        let subnets = ec2::get_subnets(
            &ctx,
            ec2::GetSubnetsArgs {
                filters: Some(vec![types::Ec2GetSubnetsFilterArgs {
                    name: pulumi::pv::string("vpc-id").cast(),
                    values: vpc.map(|v: types::Ec2GetVpcResult| vec![v.id]),
                }]),
                ..Default::default()
            },
            pulumi::InvokeOptions::default(),
        );

        let subnet_ids = subnets.map(|s: types::Ec2GetSubnetsResult| s.ids);

        // ---------------------------------------------------------------
        // The control plane's role.
        //
        // `assume_role_policy` is required, so `RoleArgs` has no `Default`
        // and every field is named.
        // ---------------------------------------------------------------
        let cluster_role = iam::Role::new(
            &ctx,
            "eks-cluster-role",
            iam::RoleArgs {
                assume_role_policy: pulumi::pv::string(CLUSTER_ASSUME_ROLE_POLICY).cast(),
                description: Some(pulumi::pv::string("Assumed by the EKS control plane").cast()),

                force_detach_policies: None,
                inline_policies: None,
                // Attached below as its own resource rather than listed
                // here: `managed_policy_arns` is exclusive, and setting it
                // detaches anything attached out of band.
                managed_policy_arns: None,
                max_session_duration: None,
                // Left unset so Pulumi auto-names the role, which keeps two
                // stacks in one account from colliding.
                name: None,
                name_prefix: None,
                path: None,
                permissions_boundary: None,
                tags: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // `RolePolicyAttachmentArgs` has two fields and both are required.
        let cluster_policy = iam::RolePolicyAttachment::new(
            &ctx,
            "eks-cluster-policy",
            iam::RolePolicyAttachmentArgs {
                role: cluster_role.name().cast(),
                policy_arn: pulumi::pv::string(CLUSTER_POLICY_ARN).cast(),
            },
            pulumi::ResourceOptions::default(),
        );

        // ---------------------------------------------------------------
        // The nodes' role. Managed node group members are EC2 instances, so
        // this one is assumed by the EC2 service and carries the three
        // policies a node needs to register, network and pull images.
        // ---------------------------------------------------------------
        let node_role = iam::Role::new(
            &ctx,
            "eks-node-role",
            iam::RoleArgs {
                assume_role_policy: pulumi::pv::string(NODE_ASSUME_ROLE_POLICY).cast(),
                description: Some(
                    pulumi::pv::string("Assumed by the EKS managed node group's instances").cast(),
                ),

                force_detach_policies: None,
                inline_policies: None,
                managed_policy_arns: None,
                max_session_duration: None,
                name: None,
                name_prefix: None,
                path: None,
                permissions_boundary: None,
                tags: None,
            },
            pulumi::ResourceOptions::default(),
        );

        let worker_node_policy = iam::RolePolicyAttachment::new(
            &ctx,
            "eks-worker-node-policy",
            iam::RolePolicyAttachmentArgs {
                role: node_role.name().cast(),
                policy_arn: pulumi::pv::string(WORKER_NODE_POLICY_ARN).cast(),
            },
            pulumi::ResourceOptions::default(),
        );

        let cni_policy = iam::RolePolicyAttachment::new(
            &ctx,
            "eks-cni-policy",
            iam::RolePolicyAttachmentArgs {
                role: node_role.name().cast(),
                policy_arn: pulumi::pv::string(CNI_POLICY_ARN).cast(),
            },
            pulumi::ResourceOptions::default(),
        );

        let ecr_policy = iam::RolePolicyAttachment::new(
            &ctx,
            "eks-ecr-policy",
            iam::RolePolicyAttachmentArgs {
                role: node_role.name().cast(),
                policy_arn: pulumi::pv::string(ECR_READ_ONLY_POLICY_ARN).cast(),
            },
            pulumi::ResourceOptions::default(),
        );

        // ---------------------------------------------------------------
        // The cluster itself.
        //
        // `ClusterArgs` requires `roleArn` and `vpcConfig`, so the generator
        // does not derive `Default` for it and every field is named; the
        // ones this program leaves alone are `None`.
        // ---------------------------------------------------------------
        let cluster = eks::Cluster::new(
            &ctx,
            "eks-cluster",
            eks::ClusterArgs {
                role_arn: cluster_role.arn().cast(),

                // `EksClusterVpcConfigArgs` requires `subnetIds`, so it has
                // no `Default` either. Every subnet of the default VPC is
                // handed over, which puts the API server's network
                // interfaces in at least two availability zones.
                vpc_config: types::EksClusterVpcConfigArgs {
                    subnet_ids: subnet_ids.clone(),

                    // An output-only attribute of the cluster; setting it as
                    // an input is not how the cluster's own security group
                    // is chosen.
                    cluster_security_group_id: None,
                    control_plane_egress_mode: None,
                    // Left unset, so the API server keeps its default
                    // public endpoint and no private one. Flipping these
                    // two is how a cluster is made VPC-internal — at which
                    // point `kubectl` has to run from inside the VPC.
                    endpoint_private_access: None,
                    endpoint_public_access: None,
                    // Unset means 0.0.0.0/0: the public endpoint is
                    // reachable from anywhere, though still only by a caller
                    // holding valid AWS credentials.
                    public_access_cidrs: None,
                    security_group_ids: None,
                    vpc_id: None,
                },

                // Unset, so AWS picks its current default Kubernetes
                // version. Pin it here to make upgrades an explicit change
                // to the program.
                version: None,

                // Unset, so the cluster keeps the provider's default access
                // configuration — which grants the IAM principal that ran
                // `pulumi up` cluster-admin, and is what makes the exported
                // kubeconfig work without any further wiring.
                access_config: None,
                bootstrap_self_managed_addons: None,
                compute_config: None,
                control_plane_scaling_config: None,
                default_addons_to_removes: None,
                // The provider defaults this to false; an example has to be
                // tearable down, so it is left alone rather than enabled.
                deletion_protection: None,
                // Control-plane logs go to CloudWatch when named here —
                // "api", "audit", "authenticator", "controllerManager",
                // "scheduler". They cost money, so none are on.
                enabled_cluster_log_types: None,
                encryption_config: None,
                force_update_version: None,
                kubernetes_network_config: None,
                name: None,
                outpost_config: None,
                region: None,
                remote_network_config: None,
                storage_config: None,
                tags: None,
                upgrade_policy: None,
                zonal_shift_config: None,
            },
            pulumi::ResourceOptions {
                // Creating a cluster whose role has no policy yet fails with
                // an unhelpful permissions error, and nothing in the
                // cluster's inputs mentions the attachment — only the role
                // itself — so the engine would otherwise be free to create
                // them in parallel.
                depends_on: vec![cluster_policy.pulumi_resource().clone()],
                ..Default::default()
            },
        );

        // ---------------------------------------------------------------
        // The nodes.
        //
        // `NodeGroupArgs` requires `clusterName`, `nodeRoleArn`,
        // `scalingConfig` and `subnetIds`, so it has no `Default` and every
        // field is named.
        // ---------------------------------------------------------------
        let node_group = eks::NodeGroup::new(
            &ctx,
            "eks-nodegroup",
            eks::NodeGroupArgs {
                cluster_name: cluster.name().cast(),
                node_role_arn: node_role.arn().cast(),
                subnet_ids: subnet_ids.clone(),

                // All three fields of `EksNodeGroupScalingConfigArgs` are
                // required. `desired_size` is what actually gets built;
                // `max_size` leaves the cluster autoscaler room to grow into
                // if one is ever installed, and nothing shrinks the group
                // below one node.
                scaling_config: types::EksNodeGroupScalingConfigArgs {
                    desired_size: node_count.cast(),
                    max_size: node_count.cast(),
                    min_size: Output::known(1),
                },

                // A list, because a node group may be given several
                // interchangeable shapes to draw from; this one gets a
                // single entry from configuration.
                instance_types: Some(instance_type.cast::<String>().map(|t: String| vec![t])),

                // Unset, so AWS picks the default `AL2023_x86_64_STANDARD`
                // image and a Kubernetes version matching the cluster's.
                ami_type: None,
                // Unset means `ON_DEMAND`; `SPOT` is the cheaper, evictable
                // alternative.
                capacity_type: None,
                disk_size: None,
                force_update_version: None,
                labels: None,
                // A launch template is the escape hatch for anything the
                // node group's own inputs do not cover — user data, a
                // specific AMI, instance metadata options.
                launch_template: None,
                node_group_name: None,
                node_group_name_prefix: None,
                node_repair_config: None,
                region: None,
                release_version: None,
                // Unset, so the nodes get no SSH key and no inbound access.
                remote_access: None,
                tags: None,
                taints: None,
                update_config: None,
                // Unset, so the node group tracks the cluster's version.
                version: None,
                warm_pool_config: None,
            },
            pulumi::ResourceOptions {
                // A node group created before its role's policies land comes
                // up with nodes that cannot register with the cluster, and
                // the create times out after fifteen minutes. The role is
                // referenced by ARN, which orders the *role* but not its
                // attachments, so all three are named here.
                depends_on: vec![
                    worker_node_policy.pulumi_resource().clone(),
                    cni_policy.pulumi_resource().clone(),
                    ecr_policy.pulumi_resource().clone(),
                ],
                ..Default::default()
            },
        );

        // `certificateAuthority` is a single object in the AWS schema, so
        // the accessor hands back a typed struct and the base64 PEM bundle
        // is one key lookup away. `Output::index` returns
        // `Output<PropertyValue>`, which is what the interpolation helper
        // below wants.
        let ca_certificate = cluster.certificate_authority().index("data");

        let kubeconfig = interpolate(
            KUBECONFIG_TEMPLATE,
            vec![
                ca_certificate,
                // Already an `https://...` URL, unlike GKE's bare address.
                cluster.endpoint().cast(),
                // Baked into the kubeconfig so `aws eks get-token` looks in
                // the right region regardless of the reader's AWS profile.
                cluster.region().cast(),
                cluster.name().cast(),
            ],
        );

        ctx.export("clusterName", cluster.name().cast::<PropertyValue>());
        ctx.export(
            "clusterEndpoint",
            cluster.endpoint().cast::<PropertyValue>(),
        );
        ctx.export(
            "nodeGroupName",
            node_group.node_group_name().cast::<PropertyValue>(),
        );

        // A kubeconfig is a credential: anyone holding it can reach the API
        // server, and this one names the cluster's CA and endpoint outright.
        // `as_secret` marks the value, so the engine encrypts it in state and
        // `pulumi stack output` prints `[secret]` unless `--show-secrets` is
        // passed.
        ctx.export("kubeconfig", kubeconfig.as_secret());

        Ok(())
    });
}

/// Build one string out of literal text and several outputs, splicing a value
/// into each `%s` in `template`.
///
/// A kubeconfig is a single document assembled from values that do not exist
/// yet, so `format!` cannot build it: the endpoint and the certificate
/// authority are `Output`s, and the string can only come together inside a
/// combinator that waits on them. That combinator is
///
/// ```text
/// pub fn concat(parts: Vec<Output<PropertyValue>>) -> Output<PropertyValue>
/// ```
///
/// in `sdk/rust/pulumi/src/pv.rs`, which wraps
/// `pulumi::output::concat(parts: Vec<Output<PropertyValue>>) -> Output<String>`.
/// It is the same machinery PCL's `${...}` interpolation compiles down to,
/// and it carries the three things that make outputs outputs: if any part is
/// unknown the whole string is unknown (so nothing is half-rendered during a
/// preview), if any part is secret the whole string is secret, and the
/// dependencies of every part end up on the result.
///
/// The alternative combinator, `pulumi::output::all(Vec<Output<PropertyValue>>)
/// -> Output<Vec<PropertyValue>>`, hands back an array to `map` over. It is
/// the wrong tool here: `all` keeps element-level secretness *inside* the
/// array as `PropertyValue::Secret` wrappers, so a `format!` over the
/// elements would render the wrapper rather than the certificate.
///
/// This is the same helper `gcp-rs-gke` uses, for the same reason.
///
/// Panics if `template` has more `%s` holes than there are values.
fn interpolate(template: &str, values: Vec<Output<PropertyValue>>) -> Output<PropertyValue> {
    let mut values = values.into_iter();
    let mut parts: Vec<Output<PropertyValue>> = Vec::new();
    let mut chunks = template.split("%s").peekable();
    while let Some(chunk) = chunks.next() {
        parts.push(pulumi::pv::string(chunk));
        if chunks.peek().is_some() {
            parts.push(
                values
                    .next()
                    .expect("interpolate: not enough values for template"),
            );
        }
    }
    pulumi::pv::concat(parts)
}
