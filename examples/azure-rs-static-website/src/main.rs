//! Host a static website on Azure Blob Storage.
//!
//! A resource group holds a StorageV2 account with Blob storage's static
//! website feature turned on. Enabling the feature creates the `$web`
//! container; `www/index.html` is uploaded into it, and the account's
//! primary web endpoint is exported so the site can be opened straight from
//! `pulumi stack output`.
//!
//! The program depends on a generated azure-native SDK, so generate that
//! first:
//!
//! ```sh
//! pulumi package add azure-native@3.25.0
//! pulumi config set azure-native:location WestUS
//! pulumi up
//! ```

use pulumi_azure_native::{resources, storage, types};

/// The page to publish, relative to the project root. `pulumi up` resolves
/// asset paths against the directory holding `Pulumi.yaml`.
const INDEX_HTML: &str = "www/index.html";

fn main() {
    pulumi::run(|ctx| async move {
        // Everything lands in one resource group.
        let resource_group = resources::ResourceGroup::new(
            &ctx,
            "static-website-rg",
            resources::ResourceGroupArgs::default(),
            pulumi::ResourceOptions::default(),
        );

        // A general-purpose v2 account. Static website hosting is a feature of
        // Blob storage, and only StorageV2 (or BlockBlobStorage) accounts have
        // it — a StorageV1 account cannot serve a site.
        let account = storage::StorageAccount::new(
            &ctx,
            "staticwebsite",
            storage::StorageAccountArgs {
                // Feeding the group's own output into the account makes the
                // engine order the two registrations and records the
                // dependency in state.
                resource_group_name: Some(resource_group.name()),
                kind: Some(pulumi::pv::string("StorageV2").cast()),
                // `Sku` is a nested object type, so it arrives as a plain
                // args struct rather than an output. Standard locally
                // redundant storage is the cheapest tier that supports
                // static websites.
                sku: Some(types::StorageSkuArgs {
                    name: Some(pulumi::pv::string("Standard_LRS").cast()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Turning the feature on creates the `$web` container and tells
        // Azure which page to serve for requests to a directory root.
        //
        // The generator snake_cases property names but does not insert a
        // separator after a digit, so `error404Document` folds to
        // `error404document` — not `error404_document`.
        let website = storage::StorageAccountStaticWebsite::new(
            &ctx,
            "static-website",
            storage::StorageAccountStaticWebsiteArgs {
                account_name: Some(account.name()),
                resource_group_name: Some(resource_group.name()),
                index_document: Some(pulumi::pv::string("index.html").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The page itself. Taking `containerName` from the static-website
        // resource rather than writing the literal `$web` is what orders the
        // upload after the feature is enabled: the container does not exist
        // until then.
        storage::Blob::new(
            &ctx,
            "index.html",
            storage::BlobArgs {
                resource_group_name: Some(resource_group.name()),
                account_name: Some(account.name()),
                container_name: Some(website.container_name()),
                blob_name: Some(pulumi::pv::string("index.html").cast()),
                // The file's bytes travel to the provider as an asset.
                source: Some(pulumi::pv::file_asset(pulumi::pv::string(INDEX_HTML)).cast()),
                // Blobs default to application/octet-stream, which makes a
                // browser download the page instead of rendering it.
                content_type: Some(pulumi::pv::string("text/html").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        let account_name = account.name();

        // `primaryEndpoints` is an object output, so the generated accessor
        // hands back a typed struct and the web endpoint is an ordinary
        // field access inside `map`. Azure only fills the value in once the
        // account exists, so during a preview this stays unknown.
        let endpoint = account.primary_endpoints().map(|endpoints| endpoints.web);

        ctx.export(
            "storage_account_name",
            account_name.cast::<pulumi::PropertyValue>(),
        );
        ctx.export("staticEndpoint", endpoint.cast::<pulumi::PropertyValue>());

        Ok(())
    });
}
