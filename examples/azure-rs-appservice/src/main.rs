//! A web application on Azure App Service, backed by an Azure SQL database.
//!
//! The static site under `app/` is zipped into a blob and served to the app
//! through the `WEBSITE_RUN_FROM_PACKAGE` setting — the same mechanism
//! `azure-rs-functions` uses, including the read-only SAS token that lets
//! the site fetch the package from a private container. What is new here is
//! the database: a `sql:Server` and a `sql:Database` whose administrator
//! credentials come from stack configuration, reachable from the app through
//! an ADO.NET connection string assembled with `pulumi::pv::concat` and
//! marked secret before it is handed to the site.
//!
//! The program depends on a generated azure-native SDK, so generate that
//! first:
//!
//! ```sh
//! pulumi package add azure-native@3.25.0
//! pulumi config set azure-native:location WestUS
//! pulumi config set sqlAdmin pulumi
//! pulumi config set --secret sqlPassword '<a strong password>'
//! pulumi up
//! ```

use pulumi_azure_native::{resources, sql, storage, types, web};

/// The local directory holding the site's content, relative to the project
/// root. `pulumi up` resolves asset and archive paths against the directory
/// holding `Pulumi.yaml`.
const APP_DIR: &str = "app";

/// The window the read-only SAS on the code zip is valid for. App Service
/// re-reads the package whenever the site restarts or scales out, so the
/// token has to outlive the deployment rather than just the `pulumi up` that
/// made it.
const SAS_START: &str = "2024-01-01";
const SAS_EXPIRY: &str = "2034-01-01";

fn main() {
    pulumi::run(|ctx| async move {
        let config = ctx.config();

        // The SQL server's administrator. `sqlAdmin` is an ordinary config
        // value; `sqlPassword` is read with `require_string` from a key that
        // is meant to be set with `--secret`, and is then wrapped in
        // `pv::secret` as well. `require_string` already returns a secret
        // output when the key is marked secret in the stack config — the
        // extra wrap makes the program correct even if someone set the key
        // in plaintext, so the password cannot reach the state file
        // unencrypted either way.
        //
        // Nothing downstream ever unwraps it: the `sql:Server` input takes
        // it directly, and the connection string built from it is marked
        // secret as a whole.
        let admin_login = config.require_string("sqlAdmin")?;
        let admin_password = pulumi::pv::secret(config.require_string("sqlPassword")?);

        // Everything lands in one resource group. The region comes from the
        // provider's `azure-native:location` config rather than being
        // hard-coded, so nothing here names a location.
        //
        // Every input of `ResourceGroupArgs` is optional in azure-native
        // 3.25.0 — the generator derives `Default` — but the fields are
        // written out anyway so that a provider version which promotes one
        // of them to required does not silently change the program's shape.
        let resource_group = resources::ResourceGroup::new(
            &ctx,
            "appservice-rg",
            resources::ResourceGroupArgs {
                location: None,
                managed_by: None,
                resource_group_name: None,
                tags: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // Somewhere to keep the deployment zip.
        //
        // `StorageAccountArgs` has required inputs (`kind`,
        // `resourceGroupName`, `sku`), so the generator does not derive
        // `Default` for it. Rust therefore needs every field named; the ones
        // this program leaves alone are `None`.
        let account = storage::StorageAccount::new(
            &ctx,
            "appsa",
            storage::StorageAccountArgs {
                resource_group_name: resource_group.name(),
                kind: pulumi::pv::string("StorageV2").cast(),
                // `Sku` is a nested object type, so it arrives as a plain
                // args struct rather than an output. Its one field, `name`,
                // is required, so it has no `Default` either.
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

        // Deployment archives go in their own container, with no anonymous
        // access: the site reads the zip with the SAS token signed below.
        let code_container = storage::BlobContainer::new(
            &ctx,
            "zips",
            storage::BlobContainerArgs {
                resource_group_name: resource_group.name(),
                account_name: account.name(),
                public_access: Some(pulumi::pv::string("None").cast()),

                container_name: None,
                default_encryption_scope: None,
                deny_encryption_scope_override: None,
                enable_nfs_v3all_squash: None,
                enable_nfs_v3root_squash: None,
                immutable_storage_with_versioning: None,
                metadata: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // The zip itself. `file_archive` over a directory puts that
        // directory's contents at the root of the archive, which is what
        // App Service expects: `index.html` at the top level, not inside an
        // `app/` folder.
        //
        // The blob's contents are part of its inputs, so editing anything
        // under `app/` re-uploads the zip on the next `pulumi up`.
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
                source: Some(pulumi::pv::file_archive(pulumi::pv::string(APP_DIR)).cast()),
                content_type: Some(pulumi::pv::string("application/zip").cast()),

                access_tier: None,
                blob_name: None,
                content_md5: None,
                metadata: None,
                r#type: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // A read-only, container-scoped SAS token — `resource: "c"`,
        // `permissions: "r"` — so App Service can fetch the zip over HTTPS
        // without the container being public.
        //
        // `listStorageAccountServiceSAS` is a provider *invoke*, which the
        // generator emits as a free function taking `(&ctx, args,
        // InvokeOptions)`. Its arguments are `Output`s, so feeding it the
        // account's own name makes this the output-versioned form: the
        // engine waits for the account to exist, records the dependency, and
        // during a preview — when the name is still unknown — skips the call
        // and leaves the result unknown. (Other Pulumi SDKs expose that as a
        // separate `…Output` function; the Rust SDK has one form, because
        // every invoke argument is already an `Output`.)
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

        // A real, dedicated plan rather than the serverless Consumption tier
        // that `azure-rs-functions` uses: `B1` on the `Basic` tier is the
        // cheapest App Service SKU that runs a site continuously. `kind:
        // "App"` marks it as a plan for web apps.
        //
        // `AppServicePlanArgs` requires `resourceGroupName`, so it has no
        // `Default` and every field is named. `SkuDescriptionArgs` is
        // all-optional, so it *does* derive `Default` and the rest of its
        // fields can be elided.
        let plan = web::AppServicePlan::new(
            &ctx,
            "asp",
            web::AppServicePlanArgs {
                resource_group_name: resource_group.name(),
                kind: Some(pulumi::pv::string("App").cast()),
                sku: Some(types::WebSkuDescriptionArgs {
                    name: Some(pulumi::pv::string("B1").cast()),
                    tier: Some(pulumi::pv::string("Basic").cast()),
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

        // ---------------------------------------------------------------
        // The database.
        //
        // A logical SQL server is the container that databases live in and
        // the thing that has a hostname and an administrator login. Both
        // `administratorLogin` and `administratorLoginPassword` are optional
        // in the schema — Azure only requires them when the server is being
        // created, which is what this program is doing — so both are
        // `Some(..)` here.
        //
        // Neither is flagged secret by the azure-native schema, so the
        // program is what keeps the password out of plaintext state: it was
        // marked secret above, and secretness travels with the value into
        // this input.
        // ---------------------------------------------------------------
        let sql_server = sql::Server::new(
            &ctx,
            "sqlserver",
            sql::ServerArgs {
                resource_group_name: resource_group.name(),
                administrator_login: Some(admin_login.cast()),
                administrator_login_password: Some(admin_password.cast()),
                // "12.0" is the only version modern Azure SQL accepts; it
                // means "SQL Database v12", not a SQL Server release.
                version: Some(pulumi::pv::string("12.0").cast()),
                minimal_tls_version: Some(pulumi::pv::string("1.2").cast()),

                administrators: None,
                federated_client_id: None,
                identity: None,
                // `isIPv6Enabled` snake-cases to `is_ipv6enabled`. A capital
                // only starts a new word when the character before it is
                // lowercase, so the `P` of `IPv6` is swallowed by the `I`,
                // and the `E` of `Enabled` — which follows a digit — is
                // swallowed too.
                is_ipv6enabled: None,
                key_id: None,
                location: None,
                primary_user_assigned_identity_id: None,
                public_network_access: None,
                restrict_outbound_network_access: None,
                server_name: None,
                tags: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // A brand-new logical server rejects every connection until a
        // firewall rule allows one. The all-zero start/end pair is Azure's
        // special "allow other Azure services" rule — it is what the portal
        // checkbox of the same name writes — and it is what lets the App
        // Service outbound addresses through without pinning them.
        //
        // It is not a substitute for real network controls: it admits *any*
        // Azure tenant's outbound traffic, so a production deployment would
        // use a private endpoint or a VNet integration instead.
        let _firewall = sql::FirewallRule::new(
            &ctx,
            "allow-azure-services",
            sql::FirewallRuleArgs {
                resource_group_name: resource_group.name(),
                server_name: sql_server.name(),
                firewall_rule_name: Some(pulumi::pv::string("AllowAllWindowsAzureIps").cast()),
                start_ip_address: Some(pulumi::pv::string("0.0.0.0").cast()),
                end_ip_address: Some(pulumi::pv::string("0.0.0.0").cast()),

                name: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // The database itself. `S0` is the smallest DTU-based Standard
        // database; `sql:Sku` requires `name`, so — unlike the web plan's
        // `SkuDescription` — it does not derive `Default` and every field is
        // written out. The tier is left unset because the service objective
        // name implies it.
        //
        // `DatabaseArgs` requires `resourceGroupName` *and* `serverName`.
        // Taking the server name from the resource is what orders the
        // database after the server.
        let database = sql::Database::new(
            &ctx,
            "db",
            sql::DatabaseArgs {
                resource_group_name: resource_group.name(),
                server_name: sql_server.name(),
                sku: Some(types::SqlSkuArgs {
                    name: pulumi::pv::string("S0").cast(),

                    capacity: None,
                    family: None,
                    size: None,
                    tier: None,
                }),

                auto_pause_delay: None,
                availability_zone: None,
                catalog_collation: None,
                collation: None,
                create_mode: None,
                database_name: None,
                elastic_pool_id: None,
                encryption_protector: None,
                encryption_protector_auto_rotation: None,
                federated_client_id: None,
                free_limit_exhaustion_behavior: None,
                high_availability_replica_count: None,
                identity: None,
                is_ledger_on: None,
                keys: None,
                license_type: None,
                location: None,
                long_term_retention_backup_resource_id: None,
                maintenance_configuration_id: None,
                manual_cutover: None,
                max_size_bytes: None,
                min_capacity: None,
                perform_cutover: None,
                preferred_enclave_type: None,
                read_scale: None,
                recoverable_database_id: None,
                recovery_services_recovery_point_id: None,
                requested_backup_storage_redundancy: None,
                restorable_dropped_database_id: None,
                restore_point_in_time: None,
                sample_name: None,
                secondary_type: None,
                source_database_deletion_date: None,
                source_database_id: None,
                source_resource_id: None,
                tags: None,
                use_free_limit: None,
                zone_redundant: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // The ADO.NET connection string, in the shape the Azure portal hands
        // out for a SQL Database. `fullyQualifiedDomainName` is read off the
        // server rather than assembled from its name, so the program does
        // not hard-code the `.database.windows.net` suffix — sovereign
        // clouds use a different one.
        //
        // `concat` propagates secretness from its parts, so the password
        // alone would already make this secret; `.as_secret()` says so
        // explicitly, because the whole string is a credential and not just
        // the substring the password occupies.
        let connection_string = pulumi::pv::concat(vec![
            pulumi::pv::string("Server=tcp:"),
            sql_server.fully_qualified_domain_name().cast(),
            pulumi::pv::string(",1433;Initial Catalog="),
            database.name().cast(),
            pulumi::pv::string(";Persist Security Info=False;User ID="),
            admin_login.cast(),
            pulumi::pv::string(";Password="),
            admin_password.cast(),
            pulumi::pv::string(
                ";MultipleActiveResultSets=False;Encrypt=True;\
                 TrustServerCertificate=False;Connection Timeout=30;",
            ),
        ])
        .as_secret();

        // The web app. `WebAppArgs` requires `resourceGroupName`, so again
        // every field is named. `SiteConfigArgs`, `NameValuePairArgs` and
        // `ConnStringInfoArgs` are all-optional and do derive `Default`.
        let app = web::WebApp::new(
            &ctx,
            "app",
            web::WebAppArgs {
                resource_group_name: resource_group.name(),
                // Passing the plan's id is what places the app on the plan
                // and orders the two registrations.
                server_farm_id: Some(plan.id()),
                https_only: Some(pulumi::pv::bool(true).cast()),
                site_config: Some(types::WebSiteConfigArgs {
                    // Run straight from the zip instead of unpacking it into
                    // the site's filesystem. This is the setting that
                    // actually deploys the content. Both fields of
                    // `NameValuePairArgs` are optional, hence the `Some(..)`.
                    app_settings: Some(vec![types::WebNameValuePairArgs {
                        name: Some(pulumi::pv::string("WEBSITE_RUN_FROM_PACKAGE").cast()),
                        value: Some(package_url.cast()),
                    }]),
                    // App Service keeps connection strings in a slot of
                    // their own, separate from app settings: the portal
                    // masks them, and the runtime exposes them to the app
                    // under a type-specific environment variable rather than
                    // by their bare name (`SQLAZURECONNSTR_db` here). That
                    // is why the connection string goes in `connection_strings`
                    // rather than being appended to `app_settings` — it is
                    // the difference between a credential the platform knows
                    // is a credential and one it does not.
                    connection_strings: Some(vec![types::WebConnStringInfoArgs {
                        name: Some(pulumi::pv::string("db").cast()),
                        connection_string: Some(connection_string.cast()),
                        // `type` is a Rust keyword, so the generator escapes
                        // the field as `r#type`.
                        r#type: Some(pulumi::pv::string("SQLAzure").cast()),
                    }]),
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
                kind: None,
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

        // The site's URL. `defaultHostName` is the `***.azurewebsites.net`
        // name Azure assigns; the app is HTTPS-only, so the scheme is fixed.
        ctx.export(
            "endpoint",
            pulumi::pv::concat(vec![
                pulumi::pv::string("https://"),
                app.default_host_name().cast(),
            ]),
        );

        // Both names are auto-generated from the Pulumi resource names plus
        // a random suffix, so export them: `az sql` and `sqlcmd` against the
        // deployed database need the real ones. Neither is sensitive — the
        // credential is the password, which is never exported.
        ctx.export("sqlServerName", sql_server.name().cast::<pulumi::PropertyValue>());
        ctx.export("databaseName", database.name().cast::<pulumi::PropertyValue>());

        Ok(())
    });
}
