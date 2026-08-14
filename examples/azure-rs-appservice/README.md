[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/pulumi-labs/pulumi-rust/tree/main/examples/azure-rs-appservice)

# Web App on Azure App Service with a SQL Database

A web application on [Azure App Service](https://learn.microsoft.com/azure/app-service/overview)
backed by a managed [Azure SQL](https://learn.microsoft.com/azure/azure-sql/database/sql-database-paas-overview)
database. The program creates a resource group, a StorageV2 account holding
the site's deployment zip, an `azure-native:web:AppServicePlan` on the `B1`
SKU / `Basic` tier — a real, always-on plan rather than the serverless
Consumption tier — and an `azure-native:web:WebApp` on that plan. The static
site under `app/` is uploaded as a zipped `azure-native:storage:Blob` and
served through the `WEBSITE_RUN_FROM_PACKAGE` application setting.

The part that makes this example different from the other Azure examples in
this repository is the **database**. An `azure-native:sql:Server` and an
`azure-native:sql:Database` are created alongside the app, the server's
administrator credentials come from stack configuration, and the app is
handed an ADO.NET connection string assembled with `pulumi::pv::concat`. The
password is read with `require_string` from a secret config key and marked
secret again in the program, and the connection string built from it is
marked secret as a whole — so neither the password nor the string it is
embedded in is ever written to state in plaintext, and neither is exported.

The connection string goes in the site config's `connectionStrings` rather
than being appended to `appSettings`. App Service treats the two differently:
connection strings are masked in the portal, and on a Windows plan — which
this is, since `reserved` is left unset — the runtime exposes them under a
type-prefixed environment variable rather than by their bare name. The entry
here is named `db` and typed `SQLAzure`, so the app reads it as
`SQLAZURECONNSTR_db`.

This is the Rust version of
[`azure-ts-appservice`](https://github.com/pulumi/examples/tree/master/azure-ts-appservice).

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
until you run the command in step 4 below. The crate does not build before
then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init appservice-dev
    ```

1.  Set the Azure region. Every resource inherits it, so nothing in
    `src/main.rs` hard-codes a location:

    ```bash
    $ pulumi config set azure-native:location WestUS
    ```

1.  Set the SQL server's administrator credentials. Azure rejects a handful
    of reserved logins — `admin`, `administrator`, `sa`, `root` and `guest`
    among them — and requires the password to be 8–128 characters using
    three of the four character classes (lowercase, uppercase, digit,
    symbol):

    ```bash
    $ pulumi config set sqlAdmin pulumi
    $ pulumi config set --secret sqlPassword '<a strong password>'
    ```

    `--secret` is what encrypts the value in `Pulumi.appservice-dev.yaml`.
    The program calls `pv::secret` on it as well, so the password stays out
    of plaintext state even if the key was set without the flag — but the
    stack config file itself is only protected by `--secret`.

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

1.  Run `pulumi up` to preview and deploy changes. Creating the SQL server
    and database takes a few minutes. After the preview is shown you will be
    prompted whether to continue.

    ```bash
    $ pulumi up
    Updating (appservice-dev)

         Type                                    Name                     Status
     +   pulumi:pulumi:Stack                     azure-rs-appservice-***  created
     +   ├─ azure-native:resources:ResourceGroup appservice-rg            created
     +   ├─ azure-native:storage:StorageAccount  appsa                    created
     +   ├─ azure-native:storage:BlobContainer   zips                     created
     +   ├─ azure-native:storage:Blob            zip                      created
     +   ├─ azure-native:web:AppServicePlan      asp                      created
     +   ├─ azure-native:sql:Server              sqlserver                created
     +   ├─ azure-native:sql:FirewallRule        allow-azure-services     created
     +   ├─ azure-native:sql:Database            db                       created
     +   └─ azure-native:web:WebApp              app                      created

    Outputs:
        databaseName:  "db***"
        endpoint:      "https://app***.azurewebsites.net"
        sqlServerName: "sqlserver***"

    Resources:
        + 10 created

    Duration: ***
    ```

1.  Fetch the page. App Service downloads the package on the first request
    after a deployment, so give it a few seconds:

    ```bash
    $ curl "$(pulumi stack output endpoint)"
    <!doctype html>
    ...
    <h1>Hello, World from Pulumi!</h1>
    ```

1.  Check the database is real. The database and server names are stack
    outputs; the resource group and the app are auto-named too, so look
    those up by prefix:

    ```bash
    $ RG=$(az group list --query "[?starts_with(name, 'appservice-rg')].name" -o tsv)
    $ az sql db show \
        --name $(pulumi stack output databaseName) \
        --server $(pulumi stack output sqlServerName) \
        --resource-group "$RG" \
        --query "{name:name, status:status, sku:currentSku.name}" -o table
    Name    Status    Sku
    ------  --------  -----
    db***   Online    S0
    ```

    The connection string the app was given is a secret, so it is not a
    stack output. To see what the site actually received, read it back from
    App Service:

    ```bash
    $ APP=$(az webapp list --resource-group "$RG" --query "[0].name" -o tsv)
    $ az webapp config connection-string list --name "$APP" --resource-group "$RG"
    ```

1.  Edit `app/index.html` and run `pulumi up` again. The blob's contents are
    part of its inputs, so the zip is re-uploaded — but the app settings are
    unchanged, so the `WebApp` itself is not updated and App Service keeps
    serving the package it already cached. Restart it to pick the new
    content up:

    ```bash
    $ pulumi up
    $ az webapp restart --name "$APP" --resource-group "$RG"
    ```

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm appservice-dev
    ```

## The database wiring

Three things have to line up for the app to be able to talk to the database,
and each one is a separate piece of the program.

**The credentials have to reach the server.** `administratorLogin` and
`administratorLoginPassword` are both optional in the azure-native schema —
Azure only requires them when a server is being *created*, which is what
this program does — so both are `Some(..)` on `ServerArgs`. Neither is
flagged secret by the schema, which means nothing marks the password secret
unless the program does:

```rust
let admin_password = pulumi::pv::secret(config.require_string("sqlPassword")?);
```

`require_string` already returns a secret output when the config key was set
with `--secret`, so on a correctly configured stack the wrap is redundant.
It is there for the stack where it was not.

**The connection string has to be assembled without leaking.** It is built
from the server's `fullyQualifiedDomainName` rather than from its name plus a
hard-coded `.database.windows.net`, because sovereign clouds use a different
suffix:

```rust
let connection_string = pulumi::pv::concat(vec![
    pulumi::pv::string("Server=tcp:"),
    sql_server.fully_qualified_domain_name().cast(),
    pulumi::pv::string(",1433;Initial Catalog="),
    database.name().cast(),
    // … User ID, Password, and the rest of the ADO.NET options
])
.as_secret();
```

`concat` propagates secretness from its parts, so the password alone would
already have made the result secret. `.as_secret()` is still there, because
the whole string is a credential and not just the substring the password
occupies — and because the secretness of the result should not depend on
which parts happen to be secret today.

**The server has to accept the connection.** A brand-new Azure SQL logical
server denies every connection until a firewall rule allows one, so the
program creates a `sql:FirewallRule` with `startIpAddress` and
`endIpAddress` both `0.0.0.0`. That all-zero pair is Azure's special "allow
other Azure services" marker — it is what the portal checkbox of the same
name writes — and it lets App Service's outbound addresses through without
pinning them.

It is a blunt instrument: it admits outbound traffic from *any* Azure
tenant, not just yours. A production deployment would give the app a
[private endpoint](https://learn.microsoft.com/azure/azure-sql/database/private-endpoint-overview)
or VNet integration and drop the rule entirely. Deleting the
`allow-azure-services` resource from `src/main.rs` is all it takes to see
the failure mode: the site still deploys, and every query it makes times out.

## Notes

- **Nothing sensitive is exported.** `endpoint`, `sqlServerName` and
  `databaseName` are all public facts about the deployment. The password and
  the connection string stay inside the program; the SAS-signed package URL
  is marked secret too, since the token in it grants read access to the
  container.
- The storage account is auto-named from the Pulumi resource name `appsa`
  plus a random suffix. Azure storage account names must be 3–24 lowercase
  alphanumeric characters, which is why that logical name has no hyphens or
  capitals — unlike the other resources in the program. SQL server names have
  the same lowercase restriction, hence `sqlserver` rather than `sqlServer`.
- `WEBSITE_RUN_FROM_PACKAGE` points at a URL rather than at the blob itself,
  so App Service fetches the zip at startup. That is also why the URL needs
  the SAS token: the container has no public access, and a restart or a
  scale-out has to be able to read the package again long after `pulumi up`
  finished. The token's validity window is the `SAS_START`/`SAS_EXPIRY`
  constants at the top of `src/main.rs`. This is the same arrangement
  `azure-rs-functions` uses, minus the `AzureWebJobsStorage` connection
  string a Functions host also needs.
- **`B1`/`Basic` is a paid plan.** Unlike the `Y1`/`Dynamic` plan in
  `azure-rs-functions`, it bills whether or not anyone visits the site, and
  so does the `S0` database. `pulumi destroy` when you are done. Changing
  the plan's SKU to `F1`/`Free` is a one-line edit if you only want to look
  at the page, at the cost of a daily compute quota and no custom domains.
- The SKUs are the ones the TypeScript version of this example uses. SKU
  availability is per-region and changes over time; if `pulumi up` fails on
  `asp` or `db` complaining about the SKU, `az appservice list-locations
  --sku B1` and `az sql db list-editions --location <region> --output table`
  will say what your subscription can currently see.
- `version: "12.0"` on the server is not a SQL Server release. It is the
  only value modern Azure SQL Database accepts, and it means "v12" — the
  generation of the service, which every database created since 2016 is on.

## Notes on the generated API

`gen-sdk` on the azure-native schema produces a `pulumi_azure_native` crate
whose layout follows the package's schema modules:

- Resources live under their module: `resources::ResourceGroup`,
  `storage::StorageAccount`, `sql::Server`, `sql::Database`, `web::WebApp`.
- Invokes are free functions taking `(&ctx, args, InvokeOptions)`:
  `storage::list_storage_account_service_sas`. Their argument structs hold
  `Output`s, so there is no separate `…Output` variant the way there is in
  TypeScript, Go and Python — every invoke is already output-versioned.
- An invoke's result is a typed struct in the flat `types` module, named
  after the function:
  `types::StorageListStorageAccountServiceSASResult`, whose
  `service_sas_token` field this program reads.
- Nested object types live in that same `types` module, with the module name
  folded into the type name: `types::StorageSkuArgs`, `types::SqlSkuArgs`,
  `types::WebSkuDescriptionArgs`, `types::WebSiteConfigArgs`,
  `types::WebNameValuePairArgs`, `types::WebConnStringInfoArgs`. Two
  different `Sku` types coexist without colliding because of that prefix.

Three things about this provider are worth knowing before reading
`src/main.rs`:

- **`Default` is derived only for all-optional structs.** Every azure-native
  resource here requires `resourceGroupName`, and `sql:Database` requires
  `serverName` on top of it, so every resource args literal is written out
  in full. Of the nested types, `SiteConfig`, `SkuDescription`,
  `NameValuePair` and `ConnStringInfo` are all-optional and could take
  `..Default::default()`; `storage:Sku` and `sql:Sku` both require `name`,
  so they name every field.
- **A run of capitals is one word.** `isIPv6Enabled` on `ServerArgs`
  becomes `is_ipv6_enabled`, and `enableNfsV3RootSquash` on
  `BlobContainerArgs` becomes `enable_nfs_v3_root_squash`. The schema's own
  `iPAddressOrRange` on the SAS invoke is genuinely odd rather than
  mis-converted, and comes through as `i_p_address_or_range`.
- **`type` is a Rust keyword.** The generator escapes it as a raw
  identifier, so `ConnStringInfo`'s `type` property — which is where the
  `SQLAzure` connection-string kind goes — is written `r#type`. `BlobArgs`
  has the same field.
