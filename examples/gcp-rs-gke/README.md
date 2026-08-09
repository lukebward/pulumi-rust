[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/gcp-rs-gke)

# Google Kubernetes Engine Cluster

A managed Kubernetes cluster on [GKE](https://cloud.google.com/kubernetes-engine).
The program creates a `gcp:container:Cluster` whose default node pool is
removed at creation time, and a `gcp:container:NodePool` this stack owns
outright, with a configurable node count and machine type. Managing the pool
separately is what makes resizing it a normal update rather than a cluster
replacement.

The cluster's endpoint and CA certificate are then folded into a kubeconfig
and exported as a **secret** stack output, so `kubectl` can be pointed at the
new cluster without `gcloud container clusters get-credentials` and without
the credential landing in plaintext in state.

This is the Rust version of
[`gcp-ts-gke`](https://github.com/pulumi/examples/tree/master/gcp-ts-gke).

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

4. [Configure Google Cloud credentials](https://www.pulumi.com/registry/packages/gcp/installation-configuration/),
   for example with `gcloud auth application-default login`.
5. Enable the GKE API on your project:

   ```bash
   $ gcloud services enable container.googleapis.com
   ```

6. To use the exported kubeconfig, install `kubectl` and the GKE auth plugin:

   ```bash
   $ gcloud components install kubectl gke-gcloud-auth-plugin
   ```

**The GCP SDK is not checked in.** `Cargo.toml` points `pulumi_gcp` at
`./sdks/gcp/rust`, which does not exist until you run the
`pulumi package gen-sdk` command in step 4 below. The crate does not build
before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init gke-dev
    ```

1.  Set the GCP project and the zone to build the cluster in:

    ```bash
    $ pulumi config set gcp:project $(gcloud config get-value project)
    $ pulumi config set gcp:zone us-central1-a
    ```

    The program leaves `location` unset on the `Cluster`, so it comes from
    this provider configuration. Setting `gcp:region` instead of `gcp:zone`
    produces a regional cluster, which runs the node pool in every zone of
    the region — three times the nodes, and three times the bill.

1.  Optionally size the node pool. The defaults are two `e2-medium` nodes:

    ```bash
    $ pulumi config set nodeCount 3
    $ pulumi config set machineType e2-standard-2
    ```

1.  Generate the GCP provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk gcp@9.33.0 --language rust --out ./sdks/gcp
    ```

    The `pulumi` crate is not published to crates.io yet, so edit the
    dependency in the generated `sdks/gcp/rust/Cargo.toml` to point at this
    repository's copy of the core SDK:

    ```toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

    The version is pinned deliberately. `NodePoolArgs` has a required input
    (`cluster`), so the generator does not derive `Default` for it and
    `src/main.rs` names every field explicitly — including the ones set to
    `None`. A different provider version can add or remove inputs, in which
    case `cargo` will name the fields to add or drop.

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue. Creating a GKE cluster takes
    several minutes.

    ```bash
    $ pulumi up
    Updating (gke-dev)

         Type                        Name           Status
     +   pulumi:pulumi:Stack         gcp-rs-gke-gke-dev  created
     +   ├─ gcp:container:Cluster    gke-cluster    created
     +   └─ gcp:container:NodePool   gke-nodepool   created

    Outputs:
        clusterName:  "gke-cluster-***"
        kubeconfig:   [secret]
        nodePoolName: "gke-nodepool-***"

    Resources:
        + 3 created

    Duration: ***
    ```

1.  The kubeconfig is a secret, so `pulumi stack output` redacts it until
    asked not to. Write it out and point `kubectl` at it:

    ```bash
    $ pulumi stack output kubeconfig --show-secrets > kubeconfig.yaml
    $ KUBECONFIG=./kubeconfig.yaml kubectl get nodes
    NAME                                       STATUS   ROLES    AGE   VERSION
    gke-gke-cluster-***-gke-nodepool-***-abcd  Ready    <none>   2m    v1.***
    gke-gke-cluster-***-gke-nodepool-***-efgh  Ready    <none>   2m    v1.***
    ```

    `gcloud` can confirm what actually got built:

    ```bash
    $ gcloud container clusters describe $(pulumi stack output clusterName) \
        --zone $(pulumi config get gcp:zone) \
        --format 'value(status, currentNodeCount)'
    RUNNING  2
    ```

1.  Resize the pool and run `pulumi up` again. Only the node pool changes;
    the cluster and its endpoint are untouched:

    ```bash
    $ pulumi config set nodeCount 3
    $ pulumi up
        ~ gcp:container:NodePool  gke-nodepool  updated
    ```

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm gke-dev
    ```

    `deletion_protection` is set to `false` in `src/main.rs` for exactly this
    reason: the GCP provider defaults it to `true`, and a cluster created with
    the default refuses to be destroyed until the flag is flipped and applied
    in a separate update.

## Building the kubeconfig

A kubeconfig is one document assembled out of values that do not exist yet —
the API server endpoint and the cluster's CA certificate are both `Output`s —
so `format!` cannot build it. The string has to come together inside a
combinator that waits on those outputs, and the one this program uses is
`pulumi::pv::concat`, in `sdk/rust/pulumi/src/pv.rs`:

```rust
pub fn concat(parts: Vec<Output<PropertyValue>>) -> Output<PropertyValue>
```

which wraps

```rust
pub fn concat(parts: Vec<Output<PropertyValue>>) -> Output<String>
```

in `sdk/rust/pulumi/src/output.rs`. It is the same machinery PCL's `${...}`
string interpolation compiles down to, and it carries the three things that
make outputs outputs: if any part is unknown the whole string is unknown, so
nothing is half-rendered during a preview; if any part is secret the whole
string is secret; and the dependencies of every part end up on the result.

`src/main.rs` wraps it in a small `interpolate` helper so the kubeconfig can
be written out as one readable YAML template with `%s` where a value goes,
rather than as a list of string fragments.

The other combinator in `output.rs` is

```rust
pub fn all(outputs: Vec<Output<PropertyValue>>) -> Output<Vec<PropertyValue>>
```

which is the right choice when several outputs feed a computation rather than
a string. It is deliberately *not* used here: `all` keeps element-level
secretness inside the array as `PropertyValue::Secret` wrappers, so a
`format!` over the resulting elements would render the wrapper instead of the
certificate.

The two cluster outputs the template needs are reached differently:

- `cluster.endpoint()` is a plain `Output<String>` accessor.
- The CA certificate is nested. `masterAuth` is a single object in the GCP
  schema, so `cluster.master_auth()` hands back
  `Output<ContainerClusterMasterAuth>` and the certificate is one key lookup
  away: `cluster.master_auth().index("clusterCaCertificate")`.

## A note on the exported credential

`kubeconfig` is exported through `pulumi::pv::secret`, which marks the value
so the engine encrypts it in state and the CLI prints `[secret]` for it.
Anyone who can read the plaintext can talk to the API server as whoever ran
`pulumi up`, so treat `--show-secrets` output the way you would treat a
private key.

The kubeconfig does not embed a token. It names `gke-gcloud-auth-plugin` as
its credential provider, which mints one on demand from whatever gcloud
credentials the caller already has — which is what lets the same exported
kubeconfig work on more than one machine.
