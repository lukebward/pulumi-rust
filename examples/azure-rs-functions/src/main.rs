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
        //
        // Every input of `ResourceGroupArgs` is optional in azure-native
        // 3.25.0 — the generator would derive `Default` — but the fields are
        // written out anyway so that a provider version which promotes one
        // of them to required does not silently change the program's shape.
        let resource_group = resources::ResourceGroup::new(
            &ctx,
            "functions-rg",
            resources::ResourceGroupArgs {
                location: None,
                managed_by: None,
                resource_group_name: None,
                tags: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // A Function App always needs a storage account: the host keeps
        // timers, leases and logs there. This one doubles as the place the
        // deployment zip lives.
        //
        // `StorageAccountArgs` has required inputs (`kind`,
        // `resourceGroupName`, `sku`), so the generator does not derive
        // `Default` for it. Rust therefore needs every field named; the ones
        // this program leaves alone are `None`.
        let account = storage::StorageAccount::new(
            &ctx,
            "functionssa",
            storage::StorageAccountArgs {
                resource_group_name: resource_group.name(),
                kind: pulumi::pv::string("StorageV2").cast(),
                // `Sku` is a nested object type, so it arrives as a plain
                // args struct rather than an output.
                sku: types::StorageSkuArgs {
                    name: pulumi::pv::string("Standard_LRS").cast(),
                },
                // The zip is read through a SAS token, never anonymously.
                allow_blob_public_access: Some(pulumi::pv::bool(false).cast()),

                access_tier: None,
                account_name: None,
                allow_cross_tenant_replication: None,
                allow_shared_key_access: None,
                allowed_copy_scope: None,
                azure_files_identity_based_authentication: None,
                custom_domain: None,
                default_to_oauth_authentication: None,
                dns_endpoint_type: None,
                enable_extended_groups: None,
                enable_https_traffic_only: None,
                enable_nfs_v3: None,
                encryption: None,
                extended_location: None,
                identity: None,
                immutable_storage_with_versioning: None,
                is_hns_enabled: None,
                is_local_user_enabled: None,
                is_sftp_enabled: None,
                key_policy: None,
                large_file_shares_state: None,
                location: None,
                minimum_tls_version: None,
                network_rule_set: None,
                public_network_access: None,
                routing_preference: None,
                sas_policy: None,
                tags: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // Function code archives go in their own container.
        let code_container = storage::BlobContainer::new(
            &ctx,
            "zips",
            storage::BlobContainerArgs {
                resource_group_name: resource_group.name(),
                account_name: account.name(),

                container_name: None,
                default_encryption_scope: None,
                deny_encryption_scope_override: None,
                enable_nfs_v3all_squash: None,
                enable_nfs_v3root_squash: None,
                immutable_storage_with_versioning: None,
                metadata: None,
                // Left unset, which means "no public access": the blob is
                // only reachable with the SAS token built below.
                public_access: None,
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
                resource_group_name: resource_group.name(),
                account_name: account.name(),
                // Taking the container name from the container resource
                // rather than writing a literal is what orders the upload
                // after the container exists.
                container_name: code_container.name(),
                source: Some(pulumi::pv::file_archive(pulumi::pv::string(FUNCTION_DIR)).cast()),
                content_type: Some(pulumi::pv::string("application/zip").cast()),

                access_tier: None,
                blob_name: None,
                content_md5: None,
                metadata: None,
                r#type: None,
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
                account_name: account.name(),
                resource_group_name: resource_group.name(),
                expand: None,
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
                account_name: account.name(),
                resource_group_name: resource_group.name(),
                canonicalized_resource: pulumi::pv::concat(vec![
                    pulumi::pv::string("/blob/"),
                    account.name().cast(),
                    pulumi::pv::string("/"),
                    code_container.name().cast(),
                ])
                .cast(),
                resource: Some(pulumi::pv::string("c").cast()),
                permissions: Some(pulumi::pv::string("r").cast()),
                protocols: Some(pulumi::pv::string("https").cast()),
                shared_access_start_time: Some(pulumi::pv::string(SAS_START).cast()),
                shared_access_expiry_time: Some(pulumi::pv::string(SAS_EXPIRY).cast()),

                cache_control: None,
                content_disposition: None,
                content_encoding: None,
                content_language: None,
                content_type: None,
                // Not a typo: the schema property is `iPAddressOrRange`, and
                // the generator's snake_casing breaks before each capital
                // that follows a lowercase letter.
                i_paddress_or_range: None,
                identifier: None,
                key_to_sign: None,
                partition_key_end: None,
                partition_key_start: None,
                row_key_end: None,
                row_key_start: None,
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

        // The Consumption plan: `Y1` on the `Dynamic` tier is what makes
        // this serverless — instances appear per request and the plan costs
        // nothing while idle. Swapping the SKU for `B1`/`Basic` or
        // `EP1`/`ElasticPremium` is the only change needed to move the same
        // app onto a dedicated or premium plan.
        //
        // `AppServicePlanArgs` requires `resourceGroupName`, so it has no
        // `Default` and every field is named.
        let plan = web::AppServicePlan::new(
            &ctx,
            "functions-plan",
            web::AppServicePlanArgs {
                resource_group_name: resource_group.name(),
                // `SkuDescriptionArgs` is all-optional, so it *does* derive
                // `Default` and the rest of its fields can be elided.
                sku: Some(types::WebSkuDescriptionArgs {
                    name: Some(pulumi::pv::string("Y1").cast()),
                    tier: Some(pulumi::pv::string("Dynamic").cast()),
                    ..Default::default()
                }),

                async_scaling_enabled: None,
                elastic_scale_enabled: None,
                extended_location: None,
                free_offer_expiration_time: None,
                hosting_environment_profile: None,
                hyper_v: None,
                identity: None,
                install_scripts: None,
                is_custom_mode: None,
                is_spot: None,
                is_xenon: None,
                kind: None,
                kube_environment_profile: None,
                location: None,
                maximum_elastic_worker_count: None,
                name: None,
                network: None,
                per_site_scaling: None,
                plan_default_identity: None,
                rdp_enabled: None,
                registry_adapters: None,
                reserved: None,
                spot_expiration_time: None,
                storage_mounts: None,
                tags: None,
                target_worker_count: None,
                target_worker_size_id: None,
                worker_tier_name: None,
                zone_redundant: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // The Function App. A `WebApp` with `kind: "functionapp"` is what
        // Azure calls a Function App — there is no separate resource type.
        //
        // `WebAppArgs` requires `resourceGroupName`, so again every field is
        // named. `SiteConfigArgs` and `NameValuePairArgs` are all-optional
        // and do derive `Default`.
        let app = web::WebApp::new(
            &ctx,
            "fa",
            web::WebAppArgs {
                resource_group_name: resource_group.name(),
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

                auto_generated_domain_name_label_scope: None,
                client_affinity_enabled: None,
                client_affinity_partitioning_enabled: None,
                client_affinity_proxy_enabled: None,
                client_cert_enabled: None,
                client_cert_exclusion_paths: None,
                client_cert_mode: None,
                cloning_info: None,
                container_size: None,
                custom_domain_verification_id: None,
                daily_memory_time_quota: None,
                dapr_config: None,
                dns_configuration: None,
                enabled: None,
                end_to_end_encryption_enabled: None,
                extended_location: None,
                function_app_config: None,
                host_name_ssl_states: None,
                host_names_disabled: None,
                hosting_environment_profile: None,
                hyper_v: None,
                identity: None,
                ip_mode: None,
                is_xenon: None,
                key_vault_reference_identity: None,
                location: None,
                managed_environment_id: None,
                name: None,
                outbound_vnet_routing: None,
                public_network_access: None,
                redundancy_mode: None,
                reserved: None,
                resource_config: None,
                scm_site_also_stopped: None,
                ssh_enabled: None,
                storage_account_required: None,
                tags: None,
                virtual_network_subnet_id: None,
                workload_profile_name: None,
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
