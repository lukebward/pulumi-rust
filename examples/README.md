# Examples

Pulumi programs written in Rust, laid out the way the [pulumi/examples][ex]
repository does: one directory per example, named `<cloud>-rs-<scenario>`,
each with its own `README.md`, `Pulumi.yaml`, `Cargo.toml` and `src/`.

[ex]: https://github.com/pulumi/examples

## Cloud examples

These mirror the canonical examples the other Pulumi languages ship.

| Example | What it deploys |
|---|---|
| [`aws-rs-s3-folder`](./aws-rs-s3-folder) | A static website served from an S3 bucket, one `BucketObject` per local file |
| [`aws-rs-webserver`](./aws-rs-webserver) | An EC2 instance behind a security group, running a tiny HTTP server |
| [`azure-rs-static-website`](./azure-rs-static-website) | A static website on Azure Blob Storage |
| [`gcp-rs-functions`](./gcp-rs-functions) | An HTTP-triggered Google Cloud Function |
| [`kubernetes-rs-nginx`](./kubernetes-rs-nginx) | An nginx Deployment and Service |

Each needs a **generated provider SDK**. `pulumi package gen-sdk` writes to
`<out>/<language>`, so generate into a per-package directory and the paths
in each `Cargo.toml` line up:

```sh
cd aws-rs-s3-folder
pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
pulumi stack init dev
pulumi config set aws:region us-west-2
pulumi up
```

The generated crate's own `Cargo.toml` declares `pulumi = "0.1"`, which is
not published — repoint it at your checkout before building:

```sh
# in ./sdks/aws/rust/Cargo.toml
pulumi = { path = "../../../../../sdk/rust/pulumi" }
```

## What is and isn't verified

Only the two examples below that need no provider are compiled in this
repository. The cloud examples are **reference programs**: they are written
against the SDK shapes our generator produces, and their Rust-level API use
(the `pulumi` crate, `Output` handling, args-literal patterns) was
type-checked against stub SDKs shaped the way the generator emits. Their
**provider property names are not verified** — reconcile them against the
SDK your own `gen-sdk` run emits, and expect to adjust.

That caveat matters most where a program has to name every field of a large
args struct: the generator derives `Default` only when every field of a
struct is optional, so any struct with a required input must be written out
in full, and a provider version that adds or drops an optional input will
break the literal. Each cloud example pins a provider version for that
reason.

| Example | Needs a provider SDK | Compiled here |
|---|---|---|
| [`config-and-outputs`](./config-and-outputs) | no | yes |
| [`component`](./component) | no | yes |
| [`random-password`](./random-password) | yes (`random`) | no |
| the five cloud examples above | yes | no |

## Language examples

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
