**Pulumi Rust SDK** lets you leverage the full power of the [Pulumi
Infrastructure as Code Platform](https://pulumi.com) using the Rust
programming language.

> **Status: experimental.** Not an official Pulumi project. The `pulumi`
> crate is not published to crates.io and there are no prebuilt language
> plugin binaries, so both have to be built from a checkout — see
> [Getting Started](#getting-started). All 179 tests of Pulumi's language
> conformance suite pass.

Simply write Rust code in your favorite editor and Pulumi automatically
provisions and manages your AWS, Azure, Google Cloud Platform, and/or
Kubernetes resources, using an infrastructure-as-code approach. Every
generated args struct derives `Default` and every field is an `Option`, so a
program names the inputs it sets and elides the rest.

For example, create an S3 bucket:

```rust
fn main() {
    pulumi::run(|ctx| async move {
        let bucket = pulumi_aws::s3::Bucket::new(
            &ctx,
            "my-bucket",
            pulumi_aws::s3::BucketArgs::default(),
            pulumi::ResourceOptions::default(),
        );

        ctx.export("bucketName", bucket.bucket().cast::<pulumi::PropertyValue>());

        Ok(())
    });
}
```

## Welcome

* **[Get Started with Pulumi using Rust](#getting-started)**: Deploy a
  simple application in AWS, Azure, Google Cloud or Kubernetes using Pulumi
  to describe the desired infrastructure using Rust.

* **[Examples](./examples)**: Nineteen cloud programs across AWS, Azure,
  GCP, Kubernetes, DigitalOcean and Docker, plus language examples for
  config, outputs and components.

* **[Docs](https://www.pulumi.com/docs/)**: Learn about Pulumi concepts,
  follow user-guides, and consult the reference documentation.

* **[Known limitations](./docs/known-limitations.md)**: What a green
  conformance suite does not cover, and what is deliberately left out.

* **[Community Slack](https://slack.pulumi.com)**: Join us in Pulumi
  Community Slack. All things Pulumi are discussed there.

* **[GitHub Discussions](https://github.com/pulumi/pulumi/discussions)**:
  Ask questions and share ideas with the Pulumi community.

* **[Contributing](./CONTRIBUTING.md)**: How this is built, how to run the
  conformance suite, and how the pieces fit together.

## <a name="getting-started"></a>Getting Started

1. **Install Pulumi**:

    ```bash
    $ curl -fsSL https://get.pulumi.com/ | sh
    ```

2. **Build the language plugin**:

    Pulumi finds a language by looking for `pulumi-language-<runtime>` on
    `PATH`. There are no released binaries yet, so build it from a checkout
    of this repository:

    ```bash
    $ git clone https://github.com/lukebward/pulumi-rust
    $ (cd pulumi-rust/pulumi-language-rust && go build .)
    $ export PATH="$PWD/pulumi-rust/pulumi-language-rust:$PATH"
    ```

3. **Create a Project**:

    Copy [`templates/rust`](./templates/rust) alongside your checkout — its
    `pulumi` dependency is a relative path, so a sibling directory lines up:

    ```bash
    $ mkdir pulumi-rust-demo && cd pulumi-rust-demo
    $ cp -r ../pulumi-rust/templates/rust/. .
    $ sed -i 's/${PROJECT}/pulumi-rust-demo/' Cargo.toml Pulumi.yaml
    $ sed -i 's/${DESCRIPTION}/A Rust Pulumi program/' Pulumi.yaml
    $ pulumi stack init dev
    ```

4. **Generate a Provider SDK**:

    Provider SDKs are generated from the provider's schema, per project:

    ```bash
    $ pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
    ```

    Two edits make it usable. Add the crate to your `Cargo.toml` — note that
    `gen-sdk` writes to `<out>/<language>`, so the crate is one level below
    `--out`:

    ```toml
    pulumi_aws = { path = "./sdks/aws/rust" }

    [workspace]                 # the generated crate declares its own
    exclude = ["sdks"]
    ```

    Then repoint the generated crate's own `pulumi` dependency, which is
    declared as an unpublished `"0.1"`. Nothing builds until you do:

    ```toml
    # in ./sdks/aws/rust/Cargo.toml
    pulumi = { path = "../../../../pulumi-rust/sdk/rust/pulumi" }
    ```

    Both paths have to resolve to the *same* checkout. If they do not — a
    symlink on one side, an absolute path on the other — cargo sees two
    different crates that happen to share a name and refuses to build:
    `package collision in the lockfile`.

5. **Deploy to the Cloud**:

    ```bash
    $ pulumi config set aws:region us-west-2
    $ pulumi up
    ```

    This makes all cloud resources declared in your code. Simply make edits
    to your project, and subsequent `pulumi up`s will compute the minimal
    diff to deploy your changes.

6. **Use Your Program**:

    Now that your code is deployed, you can interact with it. In the above
    example, we can find the name of the newly provisioned S3 bucket:

    ```bash
    $ pulumi stack output bucketName
    ```

To learn more, head over to [pulumi.com](https://pulumi.com) for much more
information, including [tutorials](https://www.pulumi.com/tutorials/),
[examples](https://github.com/pulumi/examples), and details of the core
Pulumi CLI and [programming model
concepts](https://www.pulumi.com/docs/concepts/).

## Requirements

Rust 1.85 or higher is required, and Go 1.25 or higher to build the language
plugin.

Cargo is the build tool. Pulumi recognizes a `runtime: rust` program and
runs `cargo` against it without further configuration; a project is an
ordinary crate with a `Pulumi.yaml` beside its `Cargo.toml`.

Generated SDKs and programs consume the core SDK and each other as Cargo
**path dependencies**, so a project's `Cargo.toml` names the checkout it was
built against.

## Contributing

Visit [CONTRIBUTING.md](./CONTRIBUTING.md) for information on building
Pulumi Rust support from source, running the conformance suite, or
contributing improvements.

## License

Apache-2.0
