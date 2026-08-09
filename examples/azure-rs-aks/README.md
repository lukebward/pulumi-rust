[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/azure-rs-aks)

# Azure Kubernetes Service (AKS) Cluster

A managed Kubernetes cluster on
[AKS](https://learn.microsoft.com/azure/aks/what-is-aks). The program creates
a resource group and one `azure-native:containerservice:ManagedCluster` with
a single agent pool backed by a virtual machine scale set. The node count and
VM size are configurable and default to two `Standard_DS2_v2` nodes.

The cluster authenticates to Azure with a **system-assigned managed
identity**, so this example needs only the azure-native provider — there is
no Entra ID application, service principal or client secret to create first,
and no dependency on the `azuread` or `tls` providers.

The cluster's kubeconfig is fetched with the
`containerservice:listManagedClusterUserCredentials` invoke, base64 decoded,
and exported as a **secret** stack output.

This is the Rust version of
[`azure-ts-aks`](https://github.com/pulumi/examples/tree/master/azure-ts-aks).

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

4. [Configure Azure credentials](https://www.pulumi.com/registry/packages/azure-native/installation-configuration/),
   most simply by running `az login` and selecting a subscription with
   `az account set --subscription <id>`.
5. [`kubectl`](https://kubernetes.io/docs/tasks/tools/), to talk to the
   cluster once it exists.

**The azure-native SDK is not checked in.** `Cargo.toml` points
`pulumi_azure_native` at `./sdks/azure-native/rust`, which does not exist
until you run the command in step 4 below. The crate does not build before
then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init aks-dev
    ```

1.  Set the Azure region. The resource group and the cluster both inherit
    it, so nothing in `src/main.rs` hard-codes a location:

    ```bash
    $ pulumi config set azure-native:location WestUS
    ```

1.  Optionally size the agent pool. The defaults are two nodes of
    `Standard_DS2_v2`:

    ```bash
    $ pulumi config set nodeCount 3
    $ pulumi config set nodeVmSize Standard_D4s_v5
    ```

1.  Generate the azure-native provider SDK and wire it into `Cargo.toml`:

    ```bash
    $ pulumi package add azure-native@3.25.0
    ```

    `package add` writes the generated crate under `./sdks` and rewrites the
    `pulumi_azure_native` path in `Cargo.toml` to point at it. The
    equivalent generate-only command is

    ```bash
    $ pulumi package gen-sdk azure-native@3.25.0 --language rust --out ./sdks/azure-native
    ```

    but note that `gen-sdk` writes to `<out>/<language>`, so the crate lands
    in `./sdks/azure-native/rust` and you have to repoint `Cargo.toml`
    yourself if the path differs.

    The generated crate's own `Cargo.toml` depends on `pulumi = "0.1"`,
    which is not published yet; repoint it at this repository:

    ```toml
    # in ./sdks/azure-native/rust/Cargo.toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

    The version is pinned because the property names in `src/main.rs` were
    checked against that schema. Every generated args struct derives
    `Default`, so a provider version that adds an optional input will not
    break this program; one that renames or removes an input still will.

1.  Run `pulumi up` to preview and deploy changes. Creating a cluster takes
    several minutes. After the preview is shown you will be prompted whether
    to continue.

    ```bash
    $ pulumi up
    Updating (aks-dev)

         Type                                            Name             Status
     +   pulumi:pulumi:Stack                             azure-rs-aks-*** created
     +   ├─ azure-native:resources:ResourceGroup         aks-rg           created
     +   └─ azure-native:containerservice:ManagedCluster aks-cluster      created

    Outputs:
        clusterName:       "aks-cluster***"
        kubeconfig:        [secret]
        resourceGroupName: "aks-rg***"

    Resources:
        + 3 created

    Duration: 6m23s
    ```

1.  The kubeconfig is a secret, so reading it takes `--show-secrets`. Write
    it to a file and point `kubectl` at it:

    ```bash
    $ pulumi stack output kubeconfig --show-secrets > kubeconfig.yaml
    $ chmod 600 kubeconfig.yaml
    $ KUBECONFIG=./kubeconfig.yaml kubectl get nodes
    NAME                             STATUS   ROLES   AGE   VERSION
    aks-agentpool-***-vmss000000     Ready    <none>  3m    v1.***
    aks-agentpool-***-vmss000001     Ready    <none>  3m    v1.***
    ```

    Without the flag the CLI prints `[secret]`. `az aks get-credentials`
    fetches the same file straight from Azure if you would rather not have
    it on disk from Pulumi:

    ```bash
    $ az aks get-credentials \
        --name $(pulumi stack output clusterName) \
        --resource-group $(pulumi stack output resourceGroupName)
    ```

1.  Resize the pool by changing the config and running `pulumi up` again.
    Node count is an in-place update; changing the VM size replaces the
    pool:

    ```bash
    $ pulumi config set nodeCount 4
    $ pulumi up
    ```

1.  Clean up when you are done. Deleting the cluster also deletes the
    `MC_*` resource group Azure created to hold its nodes, disks and load
    balancers:

    ```bash
    $ rm -f kubeconfig.yaml
    $ pulumi destroy
    $ pulumi stack rm aks-dev
    ```

## Notes

- **The kubeconfig is marked secret in the program, not by the provider.**
  Nothing in the azure-native schema flags
  `listManagedClusterUserCredentials`' result as sensitive, so
  `src/main.rs` calls `.as_secret()` on it before exporting. That is what
  encrypts it in the state file and redacts it in CLI output. A kubeconfig
  embeds a client certificate and key — whoever holds it is an
  authenticated cluster user.
- The invoke returns the file base64 encoded. The core SDK already ships a
  decoder — `pulumi::pv::from_base64`, the helper generated programs use for
  PCL's `fromBase64` — so the program does not need a `base64` dependency of
  its own. It hands the encoded string to that helper and exports the
  result.
- **Using a service principal instead of the managed identity.** Set
  `service_principal_profile` to a
  `types::ContainerserviceManagedClusterServicePrincipalProfileArgs` — its
  `client_id` is required and its `secret` is optional — and leave
  `identity` as `None`. That is what the
  TypeScript version of this example does, and it is why that version pulls
  in the `azuread` provider to create the application, service principal and
  password first. A system-assigned identity avoids managing a credential
  entirely, and is what Azure recommends for new clusters.
- **SSH access to the nodes** is not configured. Setting `linux_profile` to
  a `types::ContainerserviceContainerServiceLinuxProfileArgs` turns it on;
  both of its fields — `admin_username` and `ssh` — are required by the
  schema. Generating the key it wants is what pulls the `tls` provider into
  the TypeScript version.
- `kubernetes_version` is left unset, so Azure picks its current default.
  Pin it if you want upgrades to be an explicit change to the program.
- The agent pool is a `System` pool: it hosts cluster-critical pods such as
  CoreDNS and metrics-server, and every cluster needs exactly one. Adding
  application nodes means either raising `nodeCount` or adding a second
  profile with `mode: "User"`.

## Notes on the generated API

`pulumi package gen-sdk azure-native` produces a `pulumi_azure_native` crate
whose layout follows the package's schema modules:

- Resources live under their module:
  `containerservice::ManagedCluster`, `resources::ResourceGroup`.
- Invokes are free functions taking `(&ctx, args, InvokeOptions)`:
  `containerservice::list_managed_cluster_user_credentials`. Their argument
  structs hold `Output`s, so there is no separate `…Output` variant the way
  there is in TypeScript, Go and Python — every invoke is already
  output-versioned, and passing `cluster.name()` straight in is what orders
  the call after the cluster and records the dependency.
- An invoke's result is a typed struct in the flat `types` module, named
  after the function:
  `types::ContainerserviceListManagedClusterUserCredentialsResult`, whose
  `kubeconfigs` field is a `Vec` of
  `types::ContainerserviceCredentialResultResponse`.
- Nested object types live in that same `types` module, with the module name
  folded into the type name:
  `types::ContainerserviceManagedClusterAgentPoolProfileArgs`.

Every generated args struct derives `Default` and every field is an
`Option`, so a program names the inputs it sets and closes the literal with
`..Default::default()`. Required inputs are not a compile-time constraint: a
missing one is reported when the resource registers, the same as in the Go,
C#, Java and Python SDKs.

The generator snake_cases property names but does not insert a separator
inside a run of capitals or after a digit, which produces some names worth
watching for in the agent pool profile: `osSKU` becomes `os_sku`,
`enableFIPS` becomes `enable_fips`, `linuxOSConfig` becomes `linux_osconfig`,
`podIPAllocationMode` becomes `pod_ipallocation_mode`, and
`nodePublicIPPrefixID` becomes `node_public_ipprefix_id`. `type` is a Rust
keyword, so it is emitted as `r#type`.
