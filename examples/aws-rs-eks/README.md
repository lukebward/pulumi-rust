[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/aws-rs-eks)

# Amazon EKS Cluster

A managed Kubernetes cluster on [EKS](https://aws.amazon.com/eks/), the AWS
counterpart to this repository's `azure-rs-aks` and `gcp-rs-gke` examples.
The program creates two IAM roles — one the control plane assumes, one the
nodes assume — an `aws:eks:Cluster`, and an `aws:eks:NodeGroup` whose node
count and instance type are configurable and default to two `t3.medium`
instances.

It does not build a network. The account's default VPC and its subnets are
looked up with the `aws:ec2:getVpc` and `aws:ec2:getSubnets` invokes, the
same way `aws-rs-fargate` does, which keeps the whole thing to eight
resources.

The cluster's endpoint and certificate authority are then folded into a
kubeconfig and exported as a **secret** stack output, so `kubectl` can be
pointed at the new cluster without running `aws eks update-kubeconfig` and
without the credential landing in plaintext in state.

This is the plain-provider version of what
[`aws-ts-eks`](https://github.com/pulumi/examples/tree/master/aws-ts-eks)
does with the `@pulumi/eks` component package. That component wraps roughly
this set of resources — plus a VPC, an OIDC provider and the aws-auth
wiring — behind a single `eks.Cluster`. There is no Rust build of the `eks`
component here, so this example spells the pieces out.

## Prerequisites

1. [Install Pulumi](https://www.pulumi.com/docs/install/).
2. [Install Rust](https://rustup.rs/) (1.85 or newer) — `cargo` builds the
   program.
3. Build the experimental Rust language plugin from this repository and put
   it on your `PATH`, so that `runtime: rust` resolves:

   ```bash
   $ (cd ../../pulumi-language-rust && go build .)
   $ export PATH="$(cd ../../pulumi-language-rust && pwd):$PATH"
   ```

4. [Configure AWS credentials](https://www.pulumi.com/registry/packages/aws/installation-configuration/),
   for example by setting `AWS_PROFILE` or running `aws configure`.
5. [`kubectl`](https://kubernetes.io/docs/tasks/tools/) and the
   [AWS CLI v2](https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html),
   to talk to the cluster once it exists. The exported kubeconfig shells out
   to `aws eks get-token` for its credentials, so the CLI is not optional.

The region you deploy into needs a **default VPC** — the program looks one
up rather than building a network, and EKS wants subnets in at least two
availability zones, which a default VPC has. Every region has one unless it
has been deleted; `aws ec2 describe-vpcs --filters Name=isDefault,Values=true`
says whether yours does.

**The AWS SDK is not checked in.** `Cargo.toml` points `pulumi_aws` at
`./sdks/aws/rust`, which does not exist until you run the
`pulumi package gen-sdk` command in step 4 below. The crate does not build
before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init eks-dev
    ```

1.  Set the AWS region:

    ```bash
    $ pulumi config set aws:region us-west-2
    ```

1.  Optionally size the node group. The defaults are two `t3.medium` nodes:

    ```bash
    $ pulumi config set nodeCount 3
    $ pulumi config set instanceType t3.large
    ```

1.  Generate the AWS provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
    ```

    Note that `gen-sdk` writes to `<out>/<language>`, so the crate lands in
    `./sdks/aws/rust` — which is the path `Cargo.toml` already points at.
    The generated crate's own `Cargo.toml` depends on `pulumi = "0.1"`,
    which is not published yet; repoint it at this repository:

    ```toml
    # in ./sdks/aws/rust/Cargo.toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

    The version is pinned because the property names in `src/main.rs` were
    checked against that schema. Every generated args struct derives
    `Default`, so a provider version that adds an optional input will not
    break this program; one that renames or removes an input still will.

1.  Run `pulumi up` to preview and deploy changes. After the preview is
    shown you will be prompted whether to continue. Creating an EKS control
    plane takes about ten minutes, and the node group a few more on top of
    that.

    ```bash
    $ pulumi up
    Updating (eks-dev)

         Type                             Name                    Status
     +   pulumi:pulumi:Stack              aws-rs-eks-eks-dev      created
     +   ├─ aws:iam:Role                  eks-cluster-role        created
     +   ├─ aws:iam:Role                  eks-node-role           created
     +   ├─ aws:iam:RolePolicyAttachment  eks-cluster-policy      created
     +   ├─ aws:iam:RolePolicyAttachment  eks-worker-node-policy  created
     +   ├─ aws:iam:RolePolicyAttachment  eks-cni-policy          created
     +   ├─ aws:iam:RolePolicyAttachment  eks-ecr-policy          created
     +   ├─ aws:eks:Cluster               eks-cluster             created
     +   └─ aws:eks:NodeGroup             eks-nodegroup           created

    Outputs:
        clusterEndpoint: "https://***.gr7.us-west-2.eks.amazonaws.com"
        clusterName:     "eks-cluster-***"
        kubeconfig:      [secret]
        nodeGroupName:   "eks-nodegroup-***"

    Resources:
        + 9 created

    Duration: ***
    ```

1.  The kubeconfig is a secret, so `pulumi stack output` redacts it until
    asked not to. Write it out and point `kubectl` at it:

    ```bash
    $ pulumi stack output kubeconfig --show-secrets > kubeconfig.yaml
    $ chmod 600 kubeconfig.yaml
    $ KUBECONFIG=./kubeconfig.yaml kubectl get nodes
    NAME                                        STATUS  ROLES   AGE  VERSION
    ip-172-31-***.us-west-2.compute.internal    Ready   <none>  2m   v1.***
    ip-172-31-***.us-west-2.compute.internal    Ready   <none>  2m   v1.***
    ```

    `aws eks update-kubeconfig` writes an equivalent file straight from AWS
    if you would rather not have this one on disk from Pulumi:

    ```bash
    $ aws eks update-kubeconfig --name $(pulumi stack output clusterName)
    ```

1.  Resize the node group by changing the config and running `pulumi up`
    again. Only the `aws:eks:NodeGroup` changes; the control plane and its
    endpoint are untouched. Node count is an in-place update, and changing
    the instance type rolls the group's instances:

    ```bash
    $ pulumi config set nodeCount 3
    $ pulumi up
        ~ aws:eks:NodeGroup  eks-nodegroup  updated
    ```

    `aws` can confirm what actually got built:

    ```bash
    $ aws eks describe-nodegroup \
        --cluster-name $(pulumi stack output clusterName) \
        --nodegroup-name $(pulumi stack output nodeGroupName) \
        --query 'nodegroup.[status,scalingConfig.desiredSize]'
    [
        "ACTIVE",
        3
    ]
    ```

1.  Clean up when you are done. The node group is deleted before the
    cluster, which is what the dependencies in the program buy you:

    ```bash
    $ rm -f kubeconfig.yaml
    $ pulumi destroy
    $ pulumi stack rm eks-dev
    ```

## Building the kubeconfig

A kubeconfig is one document assembled out of values that do not exist yet —
the API server endpoint and the cluster's certificate authority are both
`Output`s — so `format!` cannot build it. The string has to come together
inside a combinator that waits on those outputs, and the one this program
uses is `pulumi::pv::concat`, in `sdk/rust/pulumi/src/pv.rs`:

```rust
pub fn concat(parts: Vec<Output<PropertyValue>>) -> Output<PropertyValue>
```

which wraps

```rust
pub fn concat(parts: Vec<Output<PropertyValue>>) -> Output<String>
```

in `sdk/rust/pulumi/src/output.rs`. It is the same machinery PCL's `${...}`
string interpolation compiles down to, and it carries the three things that
make outputs outputs: if any part is unknown the whole string is unknown, so
nothing is half-rendered during a preview; if any part is secret the whole
string is secret; and the dependencies of every part end up on the result.

`src/main.rs` wraps it in the same small `interpolate` helper `gcp-rs-gke`
uses, so the kubeconfig can be written out as one readable YAML template
with `%s` where a value goes, rather than as a list of string fragments.

The other combinator in `output.rs` is

```rust
pub fn all(outputs: Vec<Output<PropertyValue>>) -> Output<Vec<PropertyValue>>
```

which is the right choice when several outputs feed a computation rather
than a string. It is deliberately *not* used here: `all` keeps element-level
secretness inside the array as `PropertyValue::Secret` wrappers, so a
`format!` over the resulting elements would render the wrapper instead of
the certificate.

The four holes in the template are reached differently:

- `cluster.endpoint()` is a plain `Output<String>` accessor, and unlike
  GKE's it is already a full `https://...` URL, so nothing is prefixed.
- `cluster.name()` and `cluster.region()` are plain accessors too. The
  region is baked into the `aws eks get-token` arguments so the file works
  for a reader whose default AWS region is a different one.
- The certificate authority is nested. `certificateAuthority` is a single
  object in the AWS schema, so `cluster.certificate_authority()` hands back
  `Output<EksClusterCertificateAuthority>` and the base64 PEM bundle is one
  key lookup away: `cluster.certificate_authority().index("data")`.

## A note on the exported credential

`kubeconfig` is exported through `.as_secret()`, which marks the value so
the engine encrypts it in state and the CLI prints `[secret]` for it. Treat
`--show-secrets` output accordingly.

The kubeconfig does not embed a token. Its `users` entry names `aws eks
get-token` as an exec credential plugin, which mints a short-lived token on
demand from whatever AWS credentials the caller already has — the same
arrangement `aws eks update-kubeconfig` writes, and what lets the exported
file work on more than one machine. What it *does* embed is the cluster's
endpoint and CA certificate, which is enough to be worth keeping encrypted.

Whether a given caller is then authorized inside the cluster is a separate
question, answered by EKS access entries rather than by the file. The
program leaves `access_config` unset, so the cluster keeps the default that
grants the IAM principal which ran `pulumi up` cluster-admin. Anyone else
needs an `aws:eks:AccessEntry` of their own.

## Notes

- **Two roles, not one.** The control plane's role is assumed by
  `eks.amazonaws.com` and carries `AmazonEKSClusterPolicy`; the nodes' role
  is assumed by `ec2.amazonaws.com` — managed node group members are
  ordinary EC2 instances — and carries `AmazonEKSWorkerNodePolicy`,
  `AmazonEKS_CNI_Policy` and `AmazonEC2ContainerRegistryReadOnly`. Mixing
  the two principals up is the classic first EKS mistake: the cluster comes
  up and then no node can ever join it.
- **The cluster role's trust policy grants `sts:TagSession` as well as
  `sts:AssumeRole`.** EKS passes session tags when it assumes the role. This
  is what the AWS provider's own documentation for `aws:eks:Cluster` grants,
  and it is a superset of the older `sts:AssumeRole`-only policy.
- **The node group waits on all three policy attachments.** Nothing in the
  node group's inputs mentions them — it references the role by ARN, which
  orders the *role* but not its attachments — so without an explicit
  `depends_on` the engine is free to create them in parallel. A node group
  whose role has no policies yet produces instances that cannot register
  with the cluster, and the create fails after a fifteen-minute timeout.
  The cluster gets the same treatment for its one attachment.
- **`managed_policy_arns` on the role is deliberately left `None`.** That
  input is exclusive: setting it makes Pulumi the sole owner of the role's
  managed policy list and detaches anything attached out of band. Separate
  `RolePolicyAttachment` resources are the additive form, and are what the
  provider's own examples use.
- **The default VPC's subnets are public.** Nodes get public IPs and reach
  the internet through the VPC's internet gateway, which is why no NAT
  gateway appears here. A production cluster puts nodes in private subnets
  and pays for the NAT.
- `version` is left unset on both the cluster and the node group, so AWS
  picks its current default Kubernetes version and the nodes track the
  control plane. Pin `version` on the cluster if you want upgrades to be an
  explicit change to the program.
- `min_size` is fixed at 1 while `desired_size` and `max_size` both follow
  `nodeCount`. There is no cluster autoscaler installed, so nothing moves
  the group within that range on its own; the range exists so that adding
  one later has somewhere to go.
- **No EBS CSI driver, no OIDC provider, no aws-auth entries.** Those are
  the next three things a real cluster needs, and each is a resource or two
  more: `aws:eks:Addon` for the driver, `aws:iam:OpenIdConnectProvider` plus
  `aws:eks:PodIdentityAssociation` for pod-level IAM. The `eks` component
  package sets them up for you; this example does not.

## Notes on the generated API

`pulumi package gen-sdk aws` produces a `pulumi_aws` crate whose layout
follows the package's schema modules:

- Resources live under their module: `eks::Cluster`, `eks::NodeGroup`,
  `iam::Role`, `iam::RolePolicyAttachment`.
- Invokes are free functions taking `(&ctx, args, InvokeOptions)`:
  `ec2::get_vpc`, `ec2::get_subnets`. Their argument structs hold `Output`s,
  so every invoke is already output-versioned — passing `vpc`'s id straight
  into the subnet filter is what orders the two calls and records the
  dependency.
- An invoke's result is a typed struct in the flat `types` module, named
  after the function with its module folded in: `types::Ec2GetVpcResult`,
  `types::Ec2GetSubnetsResult`.
- Nested object types live in that same `types` module, with the module
  name folded into the type name: `types::EksClusterVpcConfigArgs`,
  `types::EksNodeGroupScalingConfigArgs`, `types::Ec2GetSubnetsFilterArgs`.

Every generated args struct derives `Default` and every field is an
`Option`, so a program names the inputs it sets and closes the literal with
`..Default::default()`. Required inputs are not a compile-time constraint: a
missing one is reported when the resource registers, the same as in the Go,
C#, Java and Python SDKs.

Nested object inputs are *not* wrapped in `Output`: `vpc_config` on
`ClusterArgs` is a bare `types::EksClusterVpcConfigArgs`, and it is the
fields inside it that are `Output`s.

Two more details worth knowing:

- Property names snake-case the way Python's do: `roleArn` becomes
  `role_arn`, `vpcConfig` becomes `vpc_config`, and on the `getVpc` result
  `ipv6CidrBlock` becomes `ipv6_cidr_block`.
- A few inputs the schema types loosely come through as
  `Output<PropertyValue>` rather than `Output<String>` —
  `RoleArgs::assume_role_policy` and `RolePolicyAttachmentArgs::role` are
  both examples. A bare `.cast()` on the value covers either shape, which is
  why the program never names the type it is casting to.
