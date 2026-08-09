//! An HTTP-triggered Azure Function App on the Consumption plan.
//!
//! The JavaScript in `function/` is zipped into a blob, and a Function App
//! running on a Y1/Dynamic App Service plan is pointed at that zip through
//! the `WEBSITE_RUN_FROM_PACKAGE` app setting. The host needs a storage
//! account to keep its own state in, so the program also reads the account's
//! primary key back out of Azure with the `storage:listStorageAccountKeys`
//! invoke and folds it into the `AzureWebJobsStorage` connection string.
//!
//! The program depends on a generated azure-native SDK, so generate that
//! first:
//!
//! ```sh
//! pulumi package add azure-native@3.25.0
//! pulumi config set azure-native:location WestUS
//! pulumi up
//! ```

use pulumi_azure_native::{resources, storage, types, web};

/// The local directory holding the function's source, relative to the
/// project root. `pulumi up` resolves asset and archive paths against the
/// directory holding `Pulumi.yaml`.
const FUNCTION_DIR: &str = "function";

/// The window the read-only SAS on the code zip is valid for. The Functions
/// host re-reads the package whenever the app restarts, so the token has to
/// outlive the deployment rather than just the `pulumi up` that made it.
const SAS_START: &str = "2024-01-01";
const SAS_EXPIRY: &str = "2034-01-01";

fn main() {
    pulumi::run(|ctx| async move {
        // Everything lands in one resource group. The region comes from the
        // provider's `azure-native:location` config rather than being
        // hard-coded, so nothing here names a location.
        let resource_group = resources::ResourceGroup::new(
            &ctx,
            "functions-rg",
            resources::ResourceGroupArgs {
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // A Function App always needs a storage account: the host keeps
        // timers, leases and logs there. This one doubles as the place the
        // deployment zip lives.
        let account = storage::StorageAccount::new(
            &ctx,
            "functionssa",
            storage::StorageAccountArgs {
                resource_group_name: Some(resource_group.name()),
                kind: Some(pulumi::pv::string("StorageV2").cast()),
                // `Sku` is a nested object type, so it arrives as a plain
                // args struct rather than an output.
                sku: Some(types::StorageSkuArgs {
                    name: Some(pulumi::pv::string("Standard_LRS").cast()),
                    ..Default::default()
                }),
                // The zip is read through a SAS token, never anonymously.
                allow_blob_public_access: Some(pulumi::pv::bool(false).cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Function code archives go in their own container.
        let code_container = storage::BlobContainer::new(
            &ctx,
            "zips",
            storage::BlobContainerArgs {
                resource_group_name: Some(resource_group.name()),
                account_name: Some(account.name()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The zip itself. `file_archive` over a directory puts that
        // directory's contents at the root of the archive, which is the
        // layout the Functions host expects: `host.json` at the top level
        // with one directory per function beside it.
        //
        // The blob's contents are part of its inputs, so editing anything
        // under `function/` re-uploads the zip on the next `pulumi up`.
        let code_blob = storage::Blob::new(
            &ctx,
            "zip",
            storage::BlobArgs {
                resource_group_name: Some(resource_group.name()),
                account_name: Some(account.name()),
                // Taking the container name from the container resource
                // rather than writing a literal is what orders the upload
                // after the container exists.
                container_name: Some(code_container.name()),
                source: Some(pulumi::pv::file_archive(pulumi::pv::string(FUNCTION_DIR)).cast()),
                content_type: Some(pulumi::pv::string("application/zip").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // ---------------------------------------------------------------
        // Reading the account keys back out of Azure.
        //
        // `listStorageAccountKeys` is a provider *invoke*, which the
        // generator emits as a free function taking `(&ctx, args,
        // InvokeOptions)`. Its arguments are `Output`s, not plain values, so
        // feeding it the account's own name is legal and makes this the
        // output-versioned form of the invoke: the engine waits for the
        // account to exist before calling it, records the dependency, and
        // during a preview — when the account name is still unknown — skips
        // the call entirely and leaves the result unknown.
        //
        // (Other Pulumi SDKs expose that as a separate
        // `listStorageAccountKeysOutput` function. The Rust SDK has one
        // form, because every invoke argument is already an `Output`.)
        // ---------------------------------------------------------------
        let account_keys = storage::list_storage_account_keys(
            &ctx,
            storage::ListStorageAccountKeysArgs {
                account_name: Some(account.name()),
                resource_group_name: Some(resource_group.name()),
                ..Default::default()
            },
            pulumi::InvokeOptions::default(),
        );

        // The invoke resolves to a typed result struct, so pulling out a key
        // is ordinary Rust inside `map`: `keys` is a `Vec` of key records,
        // and Azure returns the primary key first — the same `keys[0].value`
        // the TypeScript and Go versions of this example index. `map` does
        // not run at all while the value is unknown, so the indexing here
        // cannot panic during a preview.
        let primary_key = account_keys
            .map(|result: types::StorageListStorageAccountKeysResult| result.keys[0].value.clone());

        // The connection string the Functions host authenticates with.
        // Marking it secret keeps the account key out of plaintext state and
        // out of `pulumi up` diffs; secretness rides along into the app
        // setting built from it.
        let connection_string = pulumi::pv::concat(vec![
            pulumi::pv::string("DefaultEndpointsProtocol=https;AccountName="),
            account.name().cast(),
            pulumi::pv::string(";AccountKey="),
            primary_key.cast(),
            pulumi::pv::string(";EndpointSuffix=core.windows.net"),
        ])
        .as_secret();

        // A second output-versioned invoke, for the same reason: the
        // canonicalized resource it signs names the account and container,
        // both of which are outputs. The result is a read-only,
        // container-scoped SAS token — `resource: "c"`, `permissions: "r"` —
        // that lets the Functions host fetch the zip over HTTPS.
        let code_sas = storage::list_storage_account_service_sas(
            &ctx,
            storage::ListStorageAccountServiceSASArgs {
                account_name: Some(account.name()),
                resource_group_name: Some(resource_group.name()),
                canonicalized_resource: Some(pulumi::pv::concat(vec![
                    pulumi::pv::string("/blob/"),
                    account.name().cast(),
                    pulumi::pv::string("/"),
                    code_container.name().cast(),
                ])
                .cast()),
                resource: Some(pulumi::pv::string("c").cast()),
                permissions: Some(pulumi::pv::string("r").cast()),
                protocols: Some(pulumi::pv::string("https").cast()),
                shared_access_start_time: Some(pulumi::pv::string(SAS_START).cast()),
                shared_access_expiry_time: Some(pulumi::pv::string(SAS_EXPIRY).cast()),
                ..Default::default()
            },
            pulumi::InvokeOptions::default(),
        );

        // `url` is an output of the blob resource, so the package URL is the
        // blob's own URL with the signature appended. It is secret because
        // the token in it grants read access to the container.
        let package_url = pulumi::pv::concat(vec![
            code_blob.url().cast(),
            pulumi::pv::string("?"),
            code_sas
                .map(|sas: types::StorageListStorageAccountServiceSASResult| sas.service_sas_token)
                .cast(),
        ])
        .as_secret();

        // The Consumption plan: `Y1` on the `Dynamic` tier is what makes this
        // serverless — instances appear per request and the plan costs nothing
        // while idle. Swapping the SKU for `B1`/`Basic` or
        // `EP1`/`ElasticPremium` is the only change needed to move the same
        // app onto a dedicated or premium plan.
        let plan = web::AppServicePlan::new(
            &ctx,
            "functions-plan",
            web::AppServicePlanArgs {
                resource_group_name: Some(resource_group.name()),
                sku: Some(types::WebSkuDescriptionArgs {
                    name: Some(pulumi::pv::string("Y1").cast()),
                    tier: Some(pulumi::pv::string("Dynamic").cast()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The Function App. A `WebApp` with `kind: "functionapp"` is what
        // Azure calls a Function App — there is no separate resource type.
        let app = web::WebApp::new(
            &ctx,
            "fa",
            web::WebAppArgs {
                resource_group_name: Some(resource_group.name()),
                // Passing the plan's id is what places the app on the plan
                // and orders the two registrations.
                server_farm_id: Some(plan.id()),
                kind: Some(pulumi::pv::string("functionapp").cast()),
                https_only: Some(pulumi::pv::bool(true).cast()),
                site_config: Some(types::WebSiteConfigArgs {
                    app_settings: Some(vec![
                        // Where the host keeps its own state.
                        app_setting("AzureWebJobsStorage", connection_string),
                        // The Functions runtime generation, and the language
                        // worker the host loads. These two are what make the
                        // app a Node.js function app rather than a plain
                        // web app.
                        app_setting("FUNCTIONS_EXTENSION_VERSION", pulumi::pv::string("~4")),
                        app_setting("FUNCTIONS_WORKER_RUNTIME", pulumi::pv::string("node")),
                        app_setting("WEBSITE_NODE_DEFAULT_VERSION", pulumi::pv::string("~20")),
                        // Run straight from the zip instead of unpacking it
                        // into the app's filesystem. This is the setting
                        // that actually deploys the code.
                        app_setting("WEBSITE_RUN_FROM_PACKAGE", package_url),
                    ]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The resource group's Azure name is auto-generated from the Pulumi
        // resource name plus a suffix, so export it: `az` commands against
        // the deployed app need it.
        ctx.export(
            "resourceGroupName",
            resource_group.name().cast::<pulumi::PropertyValue>(),
        );
        ctx.export("functionAppName", app.name().cast::<pulumi::PropertyValue>());

        // The app's default hostname, with the function's route on the end.
        // `HelloNode` is the name of the directory inside `function/`, which
        // is how the Functions host names and routes it.
        ctx.export(
            "endpoint",
            pulumi::pv::concat(vec![
                pulumi::pv::string("https://"),
                app.default_host_name().cast(),
                pulumi::pv::string("/api/HelloNode?name=Pulumi"),
            ]),
        );

        Ok(())
    });
}

/// One `name = value` pair in the Function App's application settings.
///
/// `NameValuePairArgs` has two fields and both are optional, so this is only
/// here to keep the settings list readable — it saves repeating the `Some(…)`
/// and the `.cast()` five times.
fn app_setting(
    name: &str,
    value: pulumi::Output<pulumi::PropertyValue>,
) -> types::WebNameValuePairArgs {
    types::WebNameValuePairArgs {
        name: Some(pulumi::pv::string(name).cast()),
        value: Some(value.cast()),
    }
}
