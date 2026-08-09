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

Every cloud example **pins a provider version**, and its `Cargo.toml`
carries the exact `gen-sdk` line to run. The pin is load-bearing: the
generator derives `Default` only when every field of an args struct is
optional, so any struct with a required input has to be written out in
full, and a provider version that adds or drops an optional input will
break the literal.

## What is and isn't verified

None of the cloud examples is deployed by CI, and none is built as part of
this repository — building one requires a provider SDK you generate
locally. They are written against the SDK shapes this repository's own
generator produces, and were checked at three different strengths:

| Strength | Examples | What was done |
|---|---|---|
| Compiled against the real generated SDK | both `kubernetes-rs-*`, `digitalocean-rs-loadbalanced-droplets`, `aws-rs-eks` | The provider's published schema was run through this repo's `GeneratePackage`, and the program `cargo check`ed against the result |
| Compiled against a stub SDK | both `gcp-rs-cloudrun`/`gcp-rs-gke`, `docker-rs-multi-container-app` | Checked against hand-built crates reproducing the generator's shapes, with field lists derived mechanically from the provider's published SDK |
| Property names machine-checked against the published schema | the remaining AWS and Azure examples, `gcp-rs-webserver`, `gcp-rs-functions` | Every args-struct literal diffed field-by-field against the provider schema at the pinned version, using a reimplementation of the generator's naming rules |

Cloud *semantics* — IAM trust policies, image names, SKU compatibility,
which ARN an integration wants — are not verified by any of that, and each
example's README calls out its own risky assumptions in a Notes section.

The two examples that need no provider **are** compiled in this repository.

| Example | Needs a provider SDK | Built here |
|---|---|---|
| [`config-and-outputs`](./config-and-outputs) | no | yes |
| [`component`](./component) | no | yes |
| [`random-password`](./random-password) | yes (`random`) | no |
| every cloud example above | yes | no |

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
