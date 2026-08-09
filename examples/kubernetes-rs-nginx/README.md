# NGINX Deployment on Kubernetes

Deploys nginx to whatever Kubernetes cluster your kubeconfig currently points
at. A `kubernetes:apps/v1:Deployment` runs the nginx image with a configurable
replica count, selecting its pods with the conventional `app: nginx` label, and
a `kubernetes:core/v1:Service` of type `ClusterIP` routes port 80 to the same
label. The Deployment's name, the Service's name, and the Service's allocated
cluster IP come back as stack outputs — the names matter because Pulumi
auto-names Kubernetes objects with a random suffix, so `nginx` on the cluster is
really `nginx-a1b2c3d4`.

This is the Rust version of
[`kubernetes-ts-nginx`](https://github.com/pulumi/examples/tree/master/kubernetes-ts-nginx).

## Prerequisites

- [Install Pulumi](https://www.pulumi.com/docs/install/)
- [Install Rust](https://www.rust-lang.org/tools/install) (1.85 or newer)
- The `pulumi-language-rust` plugin from this repository on your `PATH`
- A Kubernetes cluster and a working `kubectl` — [minikube](https://minikube.sigs.k8s.io/),
  [kind](https://kind.sigs.k8s.io/), or a managed cluster all work. Pulumi uses
  the same kubeconfig and current context `kubectl` does.

## Deploying and running the program

1.  Create a new stack:

    ```bash
    $ pulumi stack init dev
    ```

2.  Confirm which cluster you are about to deploy to:

    ```bash
    $ kubectl config current-context
    minikube
    ```

    To target a different context without switching `kubectl`, set it on the
    provider instead:

    ```bash
    $ pulumi config set kubernetes:context my-cluster
    ```

3.  Optionally scale the deployment (the default is one replica):

    ```bash
    $ pulumi config set replicas 3
    ```

4.  Generate the Kubernetes provider SDK. The program does not compile until
    this exists — `Cargo.toml` depends on it at `./sdks/kubernetes`, and the
    directory is gitignored because it is a build product:

    ```bash
    $ pulumi package gen-sdk kubernetes@4.33.0 --language rust --out ./sdks/kubernetes
    ```

    The `pulumi` crate is not published to crates.io yet, so edit the
    dependency in the generated `sdks/kubernetes/Cargo.toml` to point at this
    repository's copy of the core SDK:

    ```toml
    pulumi = { path = "../../../../sdk/rust/pulumi" }
    ```

5.  Preview and deploy:

    ```bash
    $ pulumi up
    Previewing update (dev)

         Type                              Name                     Plan
     +   pulumi:pulumi:Stack               kubernetes-rs-nginx-dev  create
     +   ├─ kubernetes:apps/v1:Deployment  nginx                    create
     +   └─ kubernetes:core/v1:Service     nginx                    create

    Resources:
        + 3 to create

    Do you want to perform this update? yes
    ```

    Pulumi waits for the Deployment to report its pods ready before it reports
    success, so a green `pulumi up` means nginx is actually serving.

6.  Check what landed on the cluster. The stack outputs carry the auto-named
    object names, so feed them to `kubectl` rather than typing `nginx`:

    ```bash
    $ kubectl get deployment $(pulumi stack output deploymentName)
    NAME              READY   UP-TO-DATE   AVAILABLE   AGE
    nginx-a1b2c3d4    1/1     1            1           38s

    $ pulumi stack output clusterIp
    10.96.117.204
    ```

    The cluster IP is only reachable from inside the cluster, so port-forward
    to hit the Service from your machine:

    ```bash
    $ kubectl port-forward service/$(pulumi stack output serviceName) 8080:80 &
    $ curl -sI http://localhost:8080 | head -n 1
    HTTP/1.1 200 OK
    ```

7.  Tear everything down:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm dev
    ```

## Notes on the generated API

`pulumi package gen-sdk kubernetes` produces a `pulumi_kubernetes` crate whose
layout follows the package's schema modules, which for Kubernetes are the
API groups:

- Resources live under their module, snake_cased: `kubernetes:apps/v1:Deployment`
  becomes `pulumi_kubernetes::apps_v1::Deployment`, and
  `kubernetes:core/v1:Service` becomes `pulumi_kubernetes::core_v1::Service`.
- Nested object types live in one flat `types` module with the module name
  folded into the type name, so `kubernetes:core/v1:PodSpec` becomes
  `pulumi_kubernetes::types::CoreV1PodSpecArgs` on the input side and
  `CoreV1PodSpec` on the output side.

Kubernetes args are typed all the way down — `spec` is a generated
`AppsV1DeploymentSpecArgs`, not an untyped bag — so this program builds them
from the generated structs rather than from `pulumi::pv::object(..)`. The two
places it does reach for `pv` are the Service's `type` and `targetPort`, which
are unions in the schema (`targetPort` is Kubernetes' int-or-string) and so
arrive as `Output<PropertyValue>`.

An args struct only derives `Default` when every one of its fields is optional.
`DeploymentArgs`, `ServiceArgs`, `ServiceSpecArgs`, `ObjectMetaArgs`,
`LabelSelectorArgs`, and `PodTemplateSpecArgs` all qualify, so
`..Default::default()` works for them. `DeploymentSpecArgs` (`selector`,
`template`), `PodSpecArgs` (`containers`), `ContainerArgs` (`name`),
`ContainerPortArgs` (`containerPort`), and `ServicePortArgs` (`port`) have
required fields, so they name every field and leave the unused ones `None` —
which is why `PodSpecArgs` in `src/main.rs` is a long list. Those lists track
one schema version: this program is written against **kubernetes 4.33.0**, and
a different provider version may add or drop optional fields on exactly those
structs. Regenerate and recheck them if you pin something else.

## Known issue

As of this writing the Rust SDK generator emits a `pulumi_kubernetes` crate that
does not compile, for two reasons — both confined to the
`apiextensions.k8s.io` `JSONSchemaProps` types, which this example never
touches, but both fatal to the crate as a whole:

- `JSONSchemaProps` has properties named `$ref` and `$schema`, and the
  generator's `snakeCase` passes `$` through, producing `pub $ref:` — not a
  legal Rust identifier.
- `JSONSchemaProps.not` is a direct self-reference, which needs a `Box` to have
  a finite size in Rust.

Until the generator handles both, the workaround is to patch the generated
`sdks/kubernetes/src/lib.rs`: rename the two `$`-prefixed fields (leaving the
`"$ref"` / `"$schema"` wire names alone) and box or dynamically type the four
`not` fields.
