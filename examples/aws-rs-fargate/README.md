[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/aws-rs-fargate)

# Containers Without Servers, on AWS Fargate

An nginx container running on [AWS Fargate](https://aws.amazon.com/fargate/)
behind an Application Load Balancer — ECS with no EC2 instances to size,
patch, or pay for while idle.

The program creates an ECS cluster, an IAM task execution role with the
managed `AmazonECSTaskExecutionRolePolicy` attached, a load balancer with a
target group and a listener on port 80, and an ECS service running a task
definition whose `container_definitions` is a JSON string. It does not
create a network: the account's default VPC and its subnets are looked up
with the `aws:ec2:getVpc` and `aws:ec2:getSubnets` invokes. The number of
copies of the container to run is configurable, and the load balancer's DNS
name comes back as the `url` stack output.

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

The region you deploy into needs a **default VPC** — the program looks one
up rather than building a network. Every region has one unless it has been
deleted; `aws ec2 describe-vpcs --filters Name=isDefault,Values=true` says
whether yours does.

**The AWS SDK is not checked in.** `Cargo.toml` points `pulumi_aws` at
`./sdks/aws/rust`, which does not exist until you run the `pulumi package gen-sdk`
command in step 4 below. The crate does not build before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init fargate-testing
    ```

1.  Set the AWS region:

    ```bash
    $ pulumi config set aws:region us-west-2
    ```

1.  Optionally choose how many copies of the container to run. The default
    is 2:

    ```bash
    $ pulumi config set desiredCount 3
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

    The version is pinned deliberately. `RoleArgs`, `ListenerArgs` and
    `TaskDefinitionArgs` all have required inputs, so the generator does not
    derive `Default` for them and `src/main.rs` names every field
    explicitly — including the ones set to `None`. A different provider
    version can add or remove inputs, in which case `cargo` will name the
    fields to add or drop.

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue. Creating the load balancer and
    waiting for the service to reach a steady state takes a few minutes.

    ```bash
    $ pulumi up
    Updating (fargate-testing)

         Type                             Name                     Status
     +   pulumi:pulumi:Stack              aws-rs-fargate-***       created
     +   ├─ aws:ec2:SecurityGroup         web-secgrp               created
     +   ├─ aws:ecs:Cluster               app-cluster              created
     +   ├─ aws:iam:Role                  task-execution-role      created
     +   ├─ aws:iam:RolePolicyAttachment  task-execution-policy    created
     +   ├─ aws:lb:LoadBalancer           web-lb                   created
     +   ├─ aws:lb:TargetGroup            web-tg                   created
     +   ├─ aws:lb:Listener               web-listener             created
     +   ├─ aws:ecs:TaskDefinition        app-task                 created
     +   └─ aws:ecs:Service               app-service              created

    Outputs:
        clusterName:     "app-cluster-***"
        loadBalancerDns: "web-lb-***.us-west-2.elb.amazonaws.com"
        url:             "http://web-lb-***.us-west-2.elb.amazonaws.com"

    Resources:
        + 10 created

    Duration: ***
    ```

1.  Fetch the page. `pulumi up` returns as soon as the service exists, but
    the load balancer only starts routing once a task has passed its health
    check — expect a minute or two of `503 Service Temporarily Unavailable`
    first:

    ```bash
    $ curl -sS $(pulumi stack output url) | head -5
    <!DOCTYPE html>
    <html>
    <head>
    <title>Welcome to nginx!</title>
    <style>
    ```

1.  Watch the tasks come up:

    ```bash
    $ aws ecs list-tasks --cluster $(pulumi stack output clusterName)
    {
        "taskArns": [
            "arn:aws:ecs:us-west-2:***:task/app-cluster-***/***",
            "arn:aws:ecs:us-west-2:***:task/app-cluster-***/***"
        ]
    }
    ```

1.  Scale the service by changing configuration and re-running `pulumi up`:
    only the `aws:ecs:Service` is updated.

    ```bash
    $ pulumi config set desiredCount 4
    $ pulumi up
    ```

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm fargate-testing
    ```

## Notes

- **`container_definitions` is a JSON string, built with `format!`.** The
  core SDK's `pulumi::pv::to_json` would be the obvious tool, but it renders
  every number as a JSON float (`80.0`), and the ECS API rejects a float
  where it expects an integer port. A formatted string keeps the integers
  integral. This example has no `serde_json` dependency of its own — the
  three values that also appear elsewhere in the program (container name,
  image, port) are constants, so the JSON and the target group cannot drift
  apart.
- **`target_type` on the target group must be `ip`.** Fargate tasks get
  their own elastic network interface rather than sharing a host, so the
  load balancer addresses them by IP. The default, `instance`, produces
  tasks that never register.
- **`assign_public_ip` must be true here.** The default VPC's subnets are
  public and have no NAT gateway, so a task without a public IP cannot reach
  Docker Hub and the deployment stalls with `CannotPullContainerError`. In a
  VPC with private subnets and a NAT gateway you would set it to false.
- **The service waits on the listener.** Registering targets fails until the
  target group is attached to a load balancer, and nothing in the service's
  inputs refers to the listener — hence the explicit `depends_on`.
- Every input of `aws:ecs:Service` and `aws:ecs:Cluster` is optional in
  provider version 7.41.0, so those args structs *do* derive `Default` and
  the program elides the fields it does not set. The nested
  `ServiceNetworkConfigurationArgs` (`subnets` required) and
  `ServiceLoadBalancerArgs` (`container_name`, `container_port`) do not, so
  those are written out in full.
