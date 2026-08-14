[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/pulumi-labs/pulumi-rust/tree/main/examples/azure-rs-functions)

# Serverless HTTP Function on Azure

An HTTP-triggered [Azure Function App](https://learn.microsoft.com/azure/azure-functions/functions-overview)
running on the Consumption plan. The program creates a resource group, a
StorageV2 account, and an `azure-native:web:AppServicePlan` on the `Y1`
SKU / `Dynamic` tier — the serverless plan, where instances appear per
request and the plan costs nothing while idle. The JavaScript under
`function/` is uploaded as a zipped `azure-native:storage:Blob`, and an
`azure-native:web:WebApp` with `kind: "functionapp"` is pointed at it
through the `WEBSITE_RUN_FROM_PACKAGE` application setting. The function's
URL comes back as a stack output.

Two provider **invokes** do the work that cannot be expressed as resources:

- `storage:listStorageAccountKeys` reads the account's access keys back out
  of Azure, and its `keys[0].value` becomes the `AzureWebJobsStorage`
  connection string the Functions host authenticates with.
- `storage:listStorageAccountServiceSAS` signs a read-only, container-scoped
  SAS token so the host can fetch the zip over HTTPS without the container
  being public.

Both take the storage account's own name as an argument, which makes them
output-versioned: the engine orders them after the account, records the
dependency, and skips them during a preview while the name is still unknown.

This is the Rust version of
[`azure-ts-functions`](https://github.com/pulumi/examples/tree/master/azure-ts-functions).

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

**The azure-native SDK is not checked in.** `Cargo.toml` points
`pulumi_azure_native` at `./sdks/azure-native/rust`, which does not exist
until you run the command in step 3 below. The crate does not build before
then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init functions-dev
    ```

1.  Set the Azure region. Every resource inherits it, so nothing in
    `src/main.rs` hard-codes a location:

    ```bash
    $ pulumi config set azure-native:location WestUS
    ```

1.  Generate the azure-native provider SDK and wire it into `Cargo.toml`.
    Generate from the default-version schema the provider checks into its
    repository — the plugin itself serves a schema spanning every Azure API
    version, which generates more Rust than rustc can compile as one crate:

    ```bash
    $ pulumi package add azure-native@3.25.0
    ```

    `package add` writes the generated crate under `./sdks` and rewrites the
    `pulumi_azure_native` path in `Cargo.toml` to point at it. The
    equivalent generate-only command is

    ```bash
    $ curl -fsSL --create-dirs -o ./sdks/schema.json \
        https://raw.githubusercontent.com/pulumi/pulumi-azure-native/v3.25.0/provider/cmd/pulumi-resource-azure-native/schema.json
    $ pulumi package gen-sdk ./sdks/schema.json --language rust --out ./sdks/azure-native
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

1.  Run `pulumi up` to preview and deploy changes. After the preview is
    shown you will be prompted whether to continue.

    ```bash
    $ pulumi up
    Updating (functions-dev)

         Type                                    Name                       Status
     +   pulumi:pulumi:Stack                     azure-rs-functions-func***  created
     +   ├─ azure-native:resources:ResourceGroup functions-rg                created
     +   ├─ azure-native:storage:StorageAccount  functionssa                 created
     +   ├─ azure-native:storage:BlobContainer   zips                        created
     +   ├─ azure-native:storage:Blob            zip                         created
     +   ├─ azure-native:web:AppServicePlan      functions-plan              created
     +   └─ azure-native:web:WebApp              fa                          created

    Outputs:
        endpoint:          "https://fa***.azurewebsites.net/api/HelloNode?name=Pulumi"
        functionAppName:   "fa***"
        resourceGroupName: "functions-rg***"

    Resources:
        + 7 created

    Duration: ***
    ```

1.  Call the function. The first request after a deployment cold-starts the
    host, so give it a few seconds:

    ```bash
    $ curl "$(pulumi stack output endpoint)"
    Hello, Pulumi! This function was deployed with Pulumi and Rust.

    $ curl "https://$(pulumi stack output functionAppName).azurewebsites.net/api/HelloNode"
    Hello, world! This function was deployed with Pulumi and Rust.
    ```

1.  Edit `function/HelloNode/index.js` and run `pulumi up` again. The blob's
    contents are part of its inputs, so the zip is re-uploaded — but the app
    settings are unchanged, so the `WebApp` itself is not updated and the
    host keeps serving the package it already cached. Restart it to pick the
    new code up:

    ```bash
    $ pulumi up
    $ az functionapp restart \
        --name $(pulumi stack output functionAppName) \
        --resource-group $(pulumi stack output resourceGroupName)
    ```

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm functions-dev
    ```

## Notes

- **The storage connection string is a secret.** `src/main.rs` marks it with
  `.as_secret()` before it goes into the app settings, so the account key is
  encrypted in the state file and shows as `[secret]` in `pulumi up` diffs.
  The SAS-signed package URL is marked the same way. Neither is exported.
- The storage account is auto-named from the Pulumi resource name
  `functionssa` plus a random suffix. Azure storage account names must be
  3–24 lowercase alphanumeric characters, which is why that logical name has
  no hyphens or capitals — unlike the other resources in the program.
- `WEBSITE_RUN_FROM_PACKAGE` points at a URL rather than at the blob itself,
  so the Functions host fetches the zip at startup. That is also why the URL
  needs the SAS token: the container has no public access, and a host
  restart has to be able to read the package again long after `pulumi up`
  finished. The token's validity window is the `SAS_START`/`SAS_EXPIRY`
  constants at the top of `src/main.rs`.
- The function itself uses the Node.js **v3** programming model: `host.json`
  at the root of the zip, and one directory per function containing
  `function.json` (the bindings) and `index.js` (the handler). The directory
  name is the route, so `function/HelloNode/` is served at `/api/HelloNode`.
- The resource group's *Azure* name is not the same as the Pulumi resource
  name `functions-rg`; it is auto-named with a random suffix, which is why
  the program exports it — `az` commands against the deployed app need the
  real name.

## Notes on the generated API

`gen-sdk` on the azure-native schema produces a `pulumi_azure_native` crate
whose layout follows the package's schema modules:

- Resources live under their module: `resources::ResourceGroup`,
  `storage::StorageAccount`, `web::WebApp`.
- Invokes are free functions taking `(&ctx, args, InvokeOptions)`:
  `storage::list_storage_account_keys`. Their argument structs hold
  `Output`s, so there is no separate `…Output` variant the way there is in
  TypeScript, Go and Python — every invoke is already output-versioned.
- An invoke's result is a typed struct in the flat `types` module, named
  after the function: `types::StorageListStorageAccountKeysResult`, whose
  `keys` field is a `Vec` of `types::StorageStorageAccountKeyResponse`.
- Nested object types live in that same `types` module, with the module name
  folded into the type name: `types::StorageSkuArgs`,
  `types::WebSiteConfigArgs`, `types::WebNameValuePairArgs`.

Every generated args struct derives `Default` and every field is an
`Option`, so a program names the inputs it sets and closes the literal with
`..Default::default()`. Required inputs are not a compile-time constraint: a
missing one is reported when the resource registers, the same as in the Go,
C#, Java and Python SDKs.
