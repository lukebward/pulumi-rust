# Examples

Pulumi programs written in Rust, laid out like the [pulumi/examples][ex]
repository: one directory per example, named `<cloud>-rs-<scenario>`, each
with its own `README.md`, `Pulumi.yaml`, `Cargo.toml` and `src/`.

[ex]: https://github.com/pulumi/examples

## Cloud examples

These mirror the canonical examples other Pulumi languages ship.

| Example | What it deploys |
|---|---|
| [`aws-rs-s3-folder`](./aws-rs-s3-folder) | A static website served from an S3 bucket, one `BucketObject` per local file |
| [`aws-rs-webserver`](./aws-rs-webserver) | An EC2 instance behind a security group, running a tiny HTTP server |

Each depends on a **generated provider SDK**. Generate it before running:

```sh
cd aws-rs-s3-folder
pulumi package gen-sdk aws --language rust --out ./sdks
pulumi stack init dev
pulumi config set aws:region us-west-2
pulumi up
```

> These cloud examples are written against the SDK shapes our generator
> produces, but they are **not compiled in this repository** — doing so
> needs the provider schemas, which means a Pulumi CLI and network access.
> Treat them as reference programs to adapt, and expect to reconcile
> property names against the SDK your `gen-sdk` run actually emits.

## Language examples

These need no provider and build straight from a checkout, so they are
verified to compile.

| Example | What it shows |
|---|---|
| [`config-and-outputs`](./config-and-outputs) | Required and optional configuration, secrets, and stack outputs |
| [`component`](./component) | Grouping child resources behind a component resource with its own inputs and outputs — also a readable stand-in for what the program generator emits for a PCL `component` block |
| [`random-password`](./random-password) | A generated provider SDK, feeding one resource's output into another, and the `replaceWith` option |

```sh
cd config-and-outputs
pulumi stack init dev
pulumi config set greeting Hello
pulumi config set --secret apiKey s3cret
pulumi up
```

## Starting a new project

Copy [`../templates/rust`](../templates/rust), replacing `${PROJECT}` and
`${DESCRIPTION}`, and point the `pulumi` dependency at your checkout of
this repository.
