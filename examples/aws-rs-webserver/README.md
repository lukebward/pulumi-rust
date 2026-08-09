[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/aws-rs-webserver)

# Web Server Using Amazon EC2

Starts a tiny HTTP server on a single EC2 instance. The program looks up the
newest Amazon Linux 2023 AMI with the `aws:ec2:getAmi` invoke instead of
hard-coding an image ID, opens port 80 with a security group, and boots the
instance with a `user_data` script that serves a "Hello, World from Pulumi!"
page. The instance type is configurable, and the instance's public IP, public
DNS name, and URL come back as stack outputs.

This is the Rust version of
[`aws-ts-webserver`](https://github.com/pulumi/examples/tree/master/aws-ts-webserver)
and [`aws-go-webserver`](https://github.com/pulumi/examples/tree/master/aws-go-webserver).

## Prerequisites

- [Install Pulumi](https://www.pulumi.com/docs/install/)
- [Install Rust](https://www.rust-lang.org/tools/install) (1.85 or newer)
- The `pulumi-language-rust` plugin from this repository on your `PATH`
- [Configure AWS credentials](https://www.pulumi.com/registry/packages/aws/installation-configuration/)

## Deploying and running the program

1.  Create a new stack:

    ```bash
    $ pulumi stack init dev
    ```

2.  Set the AWS region to deploy into:

    ```bash
    $ pulumi config set aws:region us-west-2
    ```

3.  Optionally pick a different instance type (the default is `t3.micro`):

    ```bash
    $ pulumi config set instanceType t3.small
    ```

4.  Generate the AWS provider SDK. The program does not compile until this
    exists — `Cargo.toml` depends on it at `./sdks/aws/rust`, and the directory is
    gitignored because it is a build product:

    ```bash
    $ pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
    ```

    The `pulumi` crate is not published to crates.io yet, so edit the
    dependency in the generated `sdks/aws/Cargo.toml` to point at this
    repository's copy of the core SDK:

    ```toml
    pulumi = { path = "../../../../sdk/rust/pulumi" }
    ```

5.  Preview and deploy:

    ```bash
    $ pulumi up
    Previewing update (dev)

         Type                      Name                    Plan
     +   pulumi:pulumi:Stack       aws-rs-webserver-dev     create
     +   ├─ aws:ec2:SecurityGroup  web-secgrp               create
     +   └─ aws:ec2:Instance       web-server-www           create

    Resources:
        + 3 to create

    Do you want to perform this update? yes
    ```

6.  Check that the server is up. It takes a few seconds after the instance
    reaches `running` for cloud-init to start the listener:

    ```bash
    $ curl $(pulumi stack output url)
    Hello, World from Pulumi!
    ```

    The individual outputs are available too:

    ```bash
    $ pulumi stack output publicIp
    54.190.13.201
    $ pulumi stack output publicDns
    ec2-54-190-13-201.us-west-2.compute.amazonaws.com
    ```

7.  Tear everything down:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm dev
    ```

## Notes on the generated API

`pulumi package gen-sdk aws` produces a `pulumi_aws` crate whose layout
follows the package's schema modules:

- Resources live under their module: `pulumi_aws::ec2::Instance`,
  `pulumi_aws::ec2::SecurityGroup`.
- Invokes are free functions taking `(&ctx, args, InvokeOptions)`:
  `pulumi_aws::ec2::get_ami`.
- Nested object types live in one flat `types` module, with the module name
  folded into the type name: `pulumi_aws::types::Ec2SecurityGroupIngressArgs`,
  `pulumi_aws::types::Ec2GetAmiFilterArgs`.

An args struct only derives `Default` when every one of its fields is
optional, so `..Default::default()` works for `SecurityGroupArgs`,
`InstanceArgs`, and `GetAmiArgs`, but the ingress and egress rules spell out
all of their fields — `from_port`, `to_port`, and `protocol` are required
there.
