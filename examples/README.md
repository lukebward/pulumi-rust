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

`gen-sdk` empties and recreates `<out>/<language>` — here `./sdks/aws/rust` —
but touches nothing else under `--out`. So a directory left behind by a
*different* `--out` layout survives, and if that is where `Cargo.toml` points,
`pulumi_aws = { path = "./sdks/aws/rust" }` resolves to nothing. Cargo reports
an unresolved dependency rather than a stale directory, which is not a useful
hint. Starting from `rm -rf ./sdks` costs one regeneration and rules it out.

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

The everyday check builds each example against a crate generated from the
**subset** of its provider's schema that the example touches, because the
large crates are tens of megabytes of Rust apiece and the loop is much
faster. Separately, the **whole** schema of every provider in the table is
generated, compiled, and then every example that pins that provider is
compiled against the whole crate — all 22 do:

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
— none of that is checked by a compiler. Each README calls out its own
riskiest assumptions in a Notes section.

That gap is not hypothetical. `aws-rs-s3-folder` was deployed against a real
AWS account and failed: it set `acl = "public-read"` on every object, and
AWS changed the default Object Ownership for new buckets to
`BucketOwnerEnforced` in April 2023, which disables ACLs outright. The
program compiled, the engine and the SDK did exactly what it asked, and
every `PutObject` was rejected with `AccessControlListNotSupported`.

That one deployment prompted an audit of all 22, which found the same shape
in five more places — each one a program that still compiles against a
provider that still offers the property, aimed at something the cloud has
since withdrawn:

| Example | What expired | When |
|---|---|---|
| `aws-rs-s3-folder` | object ACLs, via `BucketOwnerEnforced` | Apr 2023 |
| `azure-rs-webserver` | Basic-SKU public IPs | uncreatable Mar 2025, retired Sep 2025 |
| `aws-rs-lambda-apigateway` | the `nodejs20.x` Lambda runtime | create blocked Jun 2026 |
| `gcp-rs-functions` | 1st gen Cloud Functions in a new project | — |
| `gcp-rs-gke`, `gcp-rs-webserver`, `gcp-rs-cloudrun` | the `default` VPC, external IPs, `allUsers` bindings — all withdrawn by the organization policies applied by default to organizations created on or after 3 May 2024 | May 2024 |
| `kubernetes-rs-guestbook` | nothing expired; the images were only ever built for amd64 | — |

All are fixed or documented. The pattern worth taking away is that the
compiler, the provider schema, and the conformance suite all agree a program
is fine right up until the moment a cloud provider retires something, and
none of the three will ever tell you. Treat each example's Notes section as
a claim to re-check, not a guarantee — including after the dates above.

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
