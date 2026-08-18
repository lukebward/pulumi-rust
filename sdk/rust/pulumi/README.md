# Pulumi Rust SDK

The `pulumi` crate is the core runtime used when writing Pulumi programs in
Rust. It contains what a program needs to talk to the Pulumi engine and to
express infrastructure as Rust code: the program entrypoint, resource
registration, `Output` values, stack configuration and exports. Generated
provider SDKs depend on this crate and express their resources in terms of the
types defined here.

> **Status: experimental.** This is not an official Pulumi project. There are
> no prebuilt binaries of the `pulumi-language-rust` language plugin, so it has
> to be built from a checkout of the repository before a Rust program can run —
> see "Getting Started" below. Provider SDKs and programs consume
> this crate as a Cargo **path dependency** pointing at that checkout. The API
> is unstable and may change without notice.

## Example

Create an S3 bucket and export its name:

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

Every generated args struct derives `Default` and every field is an `Option`,
so a program names the inputs it sets and elides the rest.

`pulumi_aws` above is not a published crate. Provider SDKs are generated from a
provider's schema, per project, with `pulumi package gen-sdk aws@7.41.0
--language rust --out ./sdks/aws`, and added to the program's `Cargo.toml` as a
path dependency.

## Automation API

The `pulumi::auto` module is the other direction: instead of the CLI running
your program, your program runs deployments — from a service, an operator, a
CLI of your own. It drives the `pulumi` CLI underneath, against either a
Pulumi project on disk (in any language) or an **inline program**, a Rust
closure the engine calls back into over an in-process language host:

```rust
use pulumi::auto::{self, Stack, LocalWorkspaceOptions, UpOptions};

let program = auto::program(|ctx| async move {
    ctx.export("greeting", pulumi::pv::string("hello"));
    Ok(())
});
let stack = Stack::create_or_select_inline_source(
    "dev", "my-project", program, LocalWorkspaceOptions::default(),
).await?;
let up = stack.up(UpOptions::default()).await?;
println!("greeting: {:?}", up.outputs["greeting"].value);
```

Stack lifecycle, configuration, `up`/`preview`/`refresh`/`destroy` with typed
results, outputs with secret marking, history, state export/import and
structured engine events are covered; the module documentation lists what is
not yet ported from the Go `auto` package. Unlike writing programs, embedding
needs no language plugin on `PATH` — the CLI alone is enough.

## Getting Started

1. Install the Pulumi CLI — see
   [Download & Install](https://www.pulumi.com/docs/install/).

2. Build the language plugin. Pulumi finds a language by looking for
   `pulumi-language-<runtime>` on `PATH`:

   ```bash
   $ git clone https://github.com/pulumi-labs/pulumi-rust
   $ (cd pulumi-rust/pulumi-language-rust && go build .)
   $ export PATH="$PWD/pulumi-rust/pulumi-language-rust:$PATH"
   ```

3. Start from
   [`templates/rust`](https://github.com/pulumi-labs/pulumi-rust/tree/main/templates/rust).
   A project is an ordinary crate with a `Pulumi.yaml` beside its `Cargo.toml`;
   Pulumi recognizes `runtime: rust` and runs `cargo` against it. Point the
   `pulumi` dependency at the checkout from step 2:

   ```toml
   pulumi = { path = "../pulumi-rust/sdk/rust/pulumi" }
   ```

The full walkthrough — generating a provider SDK, wiring up the path
dependencies and deploying — is in the
[repository README](https://github.com/pulumi-labs/pulumi-rust#readme).

## Requirements

Rust 1.85 or higher, and Go 1.25 or higher to build the language plugin.

## Learn More

* [Pulumi Documentation](https://www.pulumi.com/docs/) — concepts, user guides
  and reference documentation.
* [Programming model concepts](https://www.pulumi.com/docs/concepts/) —
  resources, inputs and outputs, stacks and configuration.
* [Repository](https://github.com/pulumi-labs/pulumi-rust) — source, known
  limitations and contributing guide.
* [Examples](https://github.com/pulumi-labs/pulumi-rust/tree/main/examples) —
  cloud programs across AWS, Azure, GCP, Kubernetes, DigitalOcean and Docker.

## License

Apache-2.0
