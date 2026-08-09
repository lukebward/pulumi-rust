[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/kubernetes-rs-guestbook)

# Kubernetes Guestbook

The classic [Kubernetes Guestbook](https://kubernetes.io/docs/tutorials/stateless-application/guestbook/):
a PHP web frontend that writes to a single Redis leader and reads from a pool
of Redis followers. Three tiers, each a `kubernetes:apps/v1:Deployment` and a
`kubernetes:core/v1:Service` in front of it — `redis-leader`,
`redis-follower`, and `frontend`. The three tiers are the same shape, so the
program describes each one as a small `Tier` value and builds all three with
one `deploy_tier` function instead of writing the Deployment/Service pair out
three times.

The frontend's Service is a `ClusterIP` by default, so the example runs on any
cluster including minikube and kind. Setting the `useLoadBalancer` config flag
makes it a `LoadBalancer` instead, which is the interesting part of the
program: a plain Rust `if` picks the Service type, and the same `if` decides
where the exported address comes from — a `LoadBalancer`'s external address
arrives asynchronously in `status.loadBalancer.ingress`, while a `ClusterIP`
Service only ever has the in-cluster `spec.clusterIP`.

The frontend Service's name and address come back as the `frontendName` and
`frontendIp` stack outputs. The name matters because Pulumi auto-names
Kubernetes objects with a random suffix, so `frontend` on the cluster is
really `frontend-a1b2c3d4`. The two Redis Services are the exception: the
frontend resolves them by DNS as `redis-leader` and `redis-follower`, so the
program sets their `metadata.name` explicitly, which turns auto-naming off.

This is the Rust version of
[`kubernetes-ts-guestbook`](https://github.com/pulumi/examples/tree/master/kubernetes-ts-guestbook/simple).

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

4. A Kubernetes cluster and a working `kubectl` —
   [minikube](https://minikube.sigs.k8s.io/), [kind](https://kind.sigs.k8s.io/),
   or a managed cluster all work. Pulumi uses the same kubeconfig and current
   context `kubectl` does.

**The Kubernetes SDK is not checked in.** `Cargo.toml` points
`pulumi_kubernetes` at `./sdks/kubernetes/rust`, which does not exist until you
run the `pulumi package gen-sdk` command in step 4 below. The crate does not
build before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init dev
    ```

1.  Confirm which cluster you are about to deploy to:

    ```bash
    $ kubectl config current-context
    minikube
    ```

    To target a different context without switching `kubectl`, set it on the
    provider instead:

    ```bash
    $ pulumi config set kubernetes:context my-cluster
    ```

1.  On a cloud cluster, ask for an externally reachable frontend. Leave this
    unset on minikube or kind — neither implements `LoadBalancer`, and the
    Service would sit in `<pending>` forever:

    ```bash
    $ pulumi config set useLoadBalancer true
    ```

1.  Generate the Kubernetes provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk kubernetes@4.33.0 --language rust --out ./sdks/kubernetes
    ```

    The version is pinned deliberately. `CoreV1ContainerArgs` and
    `CoreV1PodSpecArgs` have required inputs, so the generator does not derive
    `Default` for them and `src/main.rs` names every field explicitly —
    including the ones set to `None`. A different provider version can add or
    remove inputs, in which case `cargo` will name the fields to add or drop.

    The `pulumi` crate is not published to crates.io yet, so edit the
    dependency in the generated `sdks/kubernetes/rust/Cargo.toml` to point at
    this repository's copy of the core SDK:

    ```toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue.

    ```bash
    $ pulumi up
    Updating (dev)

         Type                              Name                         Status
     +   pulumi:pulumi:Stack               kubernetes-rs-guestbook-dev  created
     +   ├─ kubernetes:apps/v1:Deployment  redis-leader                 created
     +   ├─ kubernetes:core/v1:Service     redis-leader                 created
     +   ├─ kubernetes:apps/v1:Deployment  redis-follower               created
     +   ├─ kubernetes:core/v1:Service     redis-follower               created
     +   ├─ kubernetes:apps/v1:Deployment  frontend                     created
     +   └─ kubernetes:core/v1:Service     frontend                     created

    Outputs:
        frontendIp:   "10.96.***.***"
        frontendName: "frontend-***"

    Resources:
        + 7 created

    Duration: ***
    ```

    Pulumi waits for each Deployment to report its pods ready before moving
    on, so a green `pulumi up` means all three tiers are actually running.

1.  Check what landed on the cluster. The stack output carries the auto-named
    Service name, so feed it to `kubectl` rather than typing `frontend`:

    ```bash
    $ kubectl get deployments
    NAME                READY   UP-TO-DATE   AVAILABLE   AGE
    frontend-***        3/3     3            3           41s
    redis-follower-***  2/2     2            2           47s
    redis-leader-***    1/1     1            1           52s

    $ pulumi stack output frontendName
    frontend-***
    ```

1.  Open the guestbook.

    With `useLoadBalancer` set, `frontendIp` is the external address the cloud
    provider handed out, and the app is on port 80:

    ```bash
    $ pulumi stack output frontendIp
    ***.***.***.***
    $ open http://$(pulumi stack output frontendIp)
    ```

    Without it, `frontendIp` is the cluster IP, which is only reachable from
    inside the cluster — port-forward to hit it from your machine:

    ```bash
    $ kubectl port-forward service/$(pulumi stack output frontendName) 8080:80 &
    $ curl -sI http://localhost:8080 | head -n 1
    HTTP/1.1 200 OK
    ```

    Typing a message into the page writes it to `redis-leader`; reloading
    reads it back through `redis-follower`.

1.  Flip the frontend between the two Service types and watch the output
    change. Only the Service is updated; the pods are left alone:

    ```bash
    $ pulumi config set useLoadBalancer true
    $ pulumi up
    ```

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm dev
    ```

## Notes on the generated API

`pulumi package gen-sdk kubernetes` produces a `pulumi_kubernetes` crate whose
layout follows the package's schema modules, which for Kubernetes are the API
groups:

- Resources live under their module, snake_cased: `kubernetes:apps/v1:Deployment`
  becomes `pulumi_kubernetes::apps_v1::Deployment`, and
  `kubernetes:core/v1:Service` becomes `pulumi_kubernetes::core_v1::Service`.
- Nested object types live in one flat `types` module with the module name
  folded into the type name, so `kubernetes:core/v1:PodSpec` becomes
  `pulumi_kubernetes::types::CoreV1PodSpecArgs` on the input side and
  `CoreV1PodSpec` on the output side.

Kubernetes args are typed all the way down — `spec` is a generated
`AppsV1DeploymentSpecArgs`, not an untyped bag — so this program builds them
from the generated structs rather than from `pulumi::pv::object(vec![..])`.
Nothing this program sets is an any-shaped field, so that escape hatch never
appears here; the two places it does reach for `pv` are the Service's `type`
and `targetPort`, which are unions in the schema (`targetPort` is Kubernetes'
int-or-string) and so arrive as `Output<PropertyValue>`.

An args struct only derives `Default` when every one of its fields is
optional. `DeploymentArgs`, `ServiceArgs`, `MetaV1ObjectMetaArgs`,
`MetaV1LabelSelectorArgs`, `CoreV1PodTemplateSpecArgs`,
`CoreV1ServiceSpecArgs`, and `CoreV1ResourceRequirementsArgs` all qualify, so
`..Default::default()` works for them. `AppsV1DeploymentSpecArgs` (`selector`,
`template`), `CoreV1PodSpecArgs` (`containers`), `CoreV1ContainerArgs`
(`name`), `CoreV1EnvVarArgs` (`name`), `CoreV1ContainerPortArgs`
(`containerPort`), and `CoreV1ServicePortArgs` (`port`) have required fields,
so they name every field and leave the unused ones `None` — which is why
`CoreV1PodSpecArgs` in `src/main.rs` is a long list. Those lists track one
schema version: this program is written against **kubernetes 4.33.0**, and a
different provider version may add or drop optional fields on exactly those
structs. Regenerate and recheck them if you pin something else.

Output-side properties are only wrapped in `Option` when the schema marks them
optional. A Service's `spec` and `metadata` are required outputs, so
`service.spec()` is an `Output<CoreV1ServiceSpec>`; `status` is not, so
`service.status()` is an `Output<Option<CoreV1ServiceStatus>>` and reading a
load balancer address out of it is a chain of `and_then` down through
`loadBalancer`, `ingress`, and the ingress entry's `ip`.

Configuration is resolved before the program starts, so awaiting a config
output resolves immediately. That is what lets `useLoadBalancer` become an
ordinary Rust `bool` and the whole conditional be a plain `if`, instead of
having to be threaded through `Output::map`.
