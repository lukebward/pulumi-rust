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
        let resource_group = resources::ResourceGroup::new(
            &ctx,
            "appservice-rg",
            resources::ResourceGroupArgs {
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Somewhere to keep the deployment zip.
        let account = storage::StorageAccount::new(
            &ctx,
            "appsa",
            storage::StorageAccountArgs {
                resource_group_name: Some(resource_group.name()),
                kind: Some(pulumi::pv::string("StorageV2").cast()),
                // `Sku` is a nested object type, so it arrives as a plain args
                // struct rather than an output.
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

        // Deployment archives go in their own container, with no anonymous
        // access: the site reads the zip with the SAS token signed below.
        let code_container = storage::BlobContainer::new(
            &ctx,
            "zips",
            storage::BlobContainerArgs {
                resource_group_name: Some(resource_group.name()),
                account_name: Some(account.name()),
                public_access: Some(pulumi::pv::string("None").cast()),
                ..Default::default()
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
                resource_group_name: Some(resource_group.name()),
                account_name: Some(account.name()),
                // Taking the container name from the container resource
                // rather than writing a literal is what orders the upload
                // after the container exists.
                container_name: Some(code_container.name()),
                source: Some(pulumi::pv::file_archive(pulumi::pv::string(APP_DIR)).cast()),
                content_type: Some(pulumi::pv::string("application/zip").cast()),
                ..Default::default()
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
                account_name: Some(account.name()),
                resource_group_name: Some(resource_group.name()),
                canonicalized_resource: Some(
                    pulumi::pv::concat(vec![
                        pulumi::pv::string("/blob/"),
                        account.name().cast(),
                        pulumi::pv::string("/"),
                        code_container.name().cast(),
                    ])
                    .cast(),
                ),
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

        // A real, dedicated plan rather than the serverless Consumption tier
        // that `azure-rs-functions` uses: `B1` on the `Basic` tier is the
        // cheapest App Service SKU that runs a site continuously. `kind:
        // "App"` marks it as a plan for web apps.
        let plan = web::AppServicePlan::new(
            &ctx,
            "asp",
            web::AppServicePlanArgs {
                resource_group_name: Some(resource_group.name()),
                kind: Some(pulumi::pv::string("App").cast()),
                sku: Some(types::WebSkuDescriptionArgs {
                    name: Some(pulumi::pv::string("B1").cast()),
                    tier: Some(pulumi::pv::string("Basic").cast()),
                    ..Default::default()
                }),
                ..Default::default()
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
                resource_group_name: Some(resource_group.name()),
                administrator_login: Some(admin_login.cast()),
                administrator_login_password: Some(admin_password.cast()),
                // "12.0" is the only version modern Azure SQL accepts; it
                // means "SQL Database v12", not a SQL Server release.
                version: Some(pulumi::pv::string("12.0").cast()),
                minimal_tls_version: Some(pulumi::pv::string("1.2").cast()),
                ..Default::default()
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
                resource_group_name: Some(resource_group.name()),
                server_name: Some(sql_server.name()),
                firewall_rule_name: Some(pulumi::pv::string("AllowAllWindowsAzureIps").cast()),
                start_ip_address: Some(pulumi::pv::string("0.0.0.0").cast()),
                end_ip_address: Some(pulumi::pv::string("0.0.0.0").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The database itself. The tier is left unset because the service
        // objective name implies it. `DatabaseArgs` requires
        // `resourceGroupName` *and* `serverName`. Taking the server name from
        // the resource is what orders the database after the server.
        let database = sql::Database::new(
            &ctx,
            "db",
            sql::DatabaseArgs {
                resource_group_name: Some(resource_group.name()),
                server_name: Some(sql_server.name()),
                sku: Some(types::SqlSkuArgs {
                    name: Some(pulumi::pv::string("S0").cast()),
                    ..Default::default()
                }),
                ..Default::default()
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

        // The web app.
        let app = web::WebApp::new(
            &ctx,
            "app",
            web::WebAppArgs {
                resource_group_name: Some(resource_group.name()),
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
                ..Default::default()
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
        ctx.export(
            "sqlServerName",
            sql_server.name().cast::<pulumi::PropertyValue>(),
        );
        ctx.export(
            "databaseName",
            database.name().cast::<pulumi::PropertyValue>(),
        );

        Ok(())
    });
}
