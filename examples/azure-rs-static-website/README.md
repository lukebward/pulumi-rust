[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/azure-rs-static-website)

# Host a Static Website on Azure Blob Storage

A static website served straight out of an Azure Storage account, using
[Blob storage's static website hosting](https://learn.microsoft.com/azure/storage/blobs/storage-blob-static-website).
The program creates a resource group, a StorageV2 account on the Standard
LRS tier, and an `azure-native:storage:StorageAccountStaticWebsite` that
turns the feature on and names `index.html` as the index document. Enabling
the feature creates the account's `$web` container, and `www/index.html` is
uploaded into it as an `azure-native:storage:Blob` with a `text/html`
content type. The account's primary web endpoint comes back as a stack
output.

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
`pulumi_azure_native` at `./sdks/azure-native/rust`, which does not exist until
you run the command in step 3 below. The crate does not build before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init website-testing
    ```

1.  Set the Azure region. The resource group and the storage account both
    inherit it, so nothing in `src/main.rs` hard-codes a location:

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
    Updating (website-testing)

         Type                                                Name                                 Status
     +   pulumi:pulumi:Stack                                 azure-rs-static-website-website-***  created
     +   ├─ azure-native:resources:ResourceGroup             static-website-rg                    created
     +   ├─ azure-native:storage:StorageAccount              staticwebsite                        created
     +   ├─ azure-native:storage:StorageAccountStaticWebsite static-website                       created
     +   └─ azure-native:storage:Blob                        index.html                           created

    Outputs:
        static_endpoint:      "https://staticwebsite***.z22.web.core.windows.net/"
        storage_account_name: "staticwebsite***"

    Resources:
        + 5 created

    Duration: ***
    ```

1.  The stack outputs name the account and the website endpoint:

    ```bash
    $ pulumi stack output
    Current stack outputs (2):
        OUTPUT                VALUE
        static_endpoint       https://staticwebsite***.z22.web.core.windows.net/
        storage_account_name  staticwebsite***
    ```

1.  Check that the blob landed in the `$web` container, then fetch the page:

    ```bash
    $ az storage blob list \
        --account-name $(pulumi stack output storageAccountName) \
        --container-name '$web' --auth-mode login --output table \
        --query "[].{name:name, size:properties.contentLength, type:properties.contentSettings.contentType}"
    Name        Size    Type
    ----------  ------  ---------
    index.html  1572    text/html

    $ curl -sS $(pulumi stack output staticEndpoint) | head -3
    <!doctype html>
    <html lang="en">
      <head>
    ```

    Opening `$(pulumi stack output staticEndpoint)` in a browser shows the
    page.

1.  Edit `www/index.html` and run `pulumi up` again: only the blob is
    updated, and the endpoint is unchanged.

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm website-testing
    ```

## Notes

- The storage account is auto-named from the Pulumi resource name
  `staticwebsite` plus a random suffix. Azure storage account names must be
  3–24 lowercase alphanumeric characters, which is why the logical name here
  has no hyphens or capitals — unlike the other resources in the program.
- `container_name` on the `Blob` is taken from the static website resource
  rather than the literal `$web`. That is what makes the engine upload the
  page only after the feature has been enabled and the container exists.
- The web endpoint is an HTTPS URL on a `*.web.core.windows.net` hostname
  Azure owns. Static website hosting has no custom-domain HTTPS support of
  its own; putting Azure Front Door or a CDN profile in front of the endpoint
  is the usual next step, and would be another resource in `src/main.rs`.
