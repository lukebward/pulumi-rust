# Examples

Pulumi programs written in Rust, laid out the way the [pulumi/examples][ex]
repository does: one directory per example, named `<cloud>-rs-<scenario>`,
each with its own `README.md`, `Pulumi.yaml`, `Cargo.toml` and `src/`.

[ex]: https://github.com/pulumi/examples

## Cloud examples

The scenarios pulumi/examples ships for every language, across six
providers.

### AWS

| Example | What it deploys |
|---|---|
| [`aws-rs-webserver`](./aws-rs-webserver) | An EC2 instance behind a security group, running a tiny HTTP server |
| [`aws-rs-s3-folder`](./aws-rs-s3-folder) | A static website served from an S3 bucket, one `BucketObject` per local file |
| [`aws-rs-static-website`](./aws-rs-static-website) | The same site in a *private* bucket, served through CloudFront with an origin access identity |
| [`aws-rs-lambda-apigateway`](./aws-rs-lambda-apigateway) | A serverless HTTP API: API Gateway v2 in front of a Lambda function |
| [`aws-rs-fargate`](./aws-rs-fargate) | An nginx container on ECS Fargate behind an Application Load Balancer |
| [`aws-rs-eks`](./aws-rs-eks) | A managed Kubernetes cluster with a node group, exporting a secret kubeconfig |

### Azure

| Example | What it deploys |
|---|---|
| [`azure-rs-webserver`](./azure-rs-webserver) | A Linux VM with a virtual network, NSG and public IP |
| [`azure-rs-static-website`](./azure-rs-static-website) | A static website on Blob Storage |
| [`azure-rs-functions`](./azure-rs-functions) | An HTTP-triggered Function App on a Consumption plan |
| [`azure-rs-appservice`](./azure-rs-appservice) | A web app on App Service backed by an Azure SQL database |
| [`azure-rs-aks`](./azure-rs-aks) | A managed Kubernetes cluster, exporting a secret kubeconfig |

### Google Cloud

| Example | What it deploys |
|---|---|
| [`gcp-rs-webserver`](./gcp-rs-webserver) | A Compute Engine instance with a firewall rule |
| [`gcp-rs-functions`](./gcp-rs-functions) | An HTTP-triggered Cloud Function |
| [`gcp-rs-cloudrun`](./gcp-rs-cloudrun) | A container on Cloud Run, reachable without credentials |
| [`gcp-rs-gke`](./gcp-rs-gke) | A GKE cluster with a separately-managed node pool |

### Kubernetes

| Example | What it deploys |
|---|---|
| [`kubernetes-rs-nginx`](./kubernetes-rs-nginx) | An nginx Deployment and Service |
| [`kubernetes-rs-guestbook`](./kubernetes-rs-guestbook) | The Guestbook: a PHP frontend over a Redis leader and its followers |

### DigitalOcean and Docker

| Example | What it deploys |
|---|---|
| [`digitalocean-rs-loadbalanced-droplets`](./digitalocean-rs-loadbalanced-droplets) | Tagged nginx Droplets behind a Load Balancer |
| [`docker-rs-multi-container-app`](./docker-rs-multi-container-app) | A Redis backend and an nginx frontend on a user-defined Docker network |

## Running a cloud example

Each needs a **generated provider SDK**. `pulumi package gen-sdk` writes to
`<out>/<language>`, so generate into a per-package directory and the paths
in each `Cargo.toml` line up:

```sh
cd aws-rs-s3-folder
rm -rf ./sdks
pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
pulumi stack init dev
pulumi config set aws:region us-west-2
pulumi up
```

`gen-sdk` writes into whatever is already at `--out` rather than replacing
it, so start from a clean `./sdks`. An `./sdks` left over from an earlier
layout leaves `pulumi_aws = { path = "./sdks/aws/rust" }` pointing at
nothing, and cargo reports the dependency as unresolved rather than as a
stale directory.

The generated crate's own `Cargo.toml` declares `pulumi = "0.1"`, which is
not published — repoint it at your checkout before building:

```sh
# in ./sdks/aws/rust/Cargo.toml
pulumi = { path = "../../../../../sdk/rust/pulumi" }
```

Every cloud example **pins a provider version**, and its `Cargo.toml`
carries the exact `gen-sdk` line to run. The pin is what makes the
property names in each program checkable. Every generated args struct
derives `Default` and every field is an `Option`, so a program names the
inputs it sets and closes the literal with `..Default::default()`: a
provider version that adds an optional input will not break an example,
though one that renames or removes an input still will.

## What is and isn't verified

**Every example in this directory compiles.** Each one was `cargo check`ed
against a `pulumi_<provider>` crate produced by this repository's own
generator from the provider's real published schema, at the version the
example pins — not against a stub, and not by inspection.

| Provider | Examples | Schema |
|---|---|---|
| aws 7.41.0 | all six `aws-rs-*` | published schema |
| azure-native 3.25.0 | all five `azure-rs-*` | published schema |
| gcp 9.33.0 | all four `gcp-rs-*` | published schema |
| kubernetes 4.33.0 | both `kubernetes-rs-*` | published schema |
| digitalocean 4.78.1, docker 5.1.0, random 4.18.4 | the rest | published schema |
| — | `component`, `config-and-outputs` | no provider; built directly in this repo |

Each example is checked against a crate generated from the **subset** of its
provider's schema that the example touches, because the large crates are
tens of megabytes of Rust apiece and the loop is much faster. Separately,
and as its own check, the **whole** schema of every provider in the table
is generated and compiled:

| Provider | Generated types | `lib.rs` |
|---|---|---|
| azure-native 3.25.0 | 27,777 | 42.9 MB |
| aws 7.41.0 | 22,142 | 28.2 MB |
| gcp 9.33.0 | 19,290 | 25.0 MB |
| kubernetes 4.33.0 | 4,701 | 5.9 MB |
| digitalocean 4.78.1 | 1,489 | 2.0 MB |
| docker 5.1.0 | 200 | 0.3 MB |
| random 4.18.4 | 18 | 43 KB |

Both checks are needed, and neither substitutes for the other. A subset
crate cannot surface a defect that only two members *together* produce —
the one that motivated this section was two schema tokens deriving the same
Rust type name, which is invisible unless both are generated at once. A
whole-schema crate, in turn, says nothing about whether an example names
the right properties.

What neither covers is cloud *semantics*. That an IAM trust policy grants
the right principal, that an image tag exists, that a SKU is available in a
region, that an integration wants the invoke ARN rather than the plain one
— none of that is checked by a compiler, and none of these examples has
been deployed. Each README calls out its own riskiest assumptions in a
Notes section.

CI does not run any of this, because it needs `pulumi package gen-sdk` and a
network. The whole-schema half is scripted —

```sh
make check_full_sdks              # every provider the examples pin
scripts/check-full-sdks.sh aws@7.41.0
```

— and the per-example half is reproducible by hand: generate the SDK, point
its `pulumi` dependency at your checkout, and `cargo check`.

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
