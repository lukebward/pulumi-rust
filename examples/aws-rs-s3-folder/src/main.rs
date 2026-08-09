//! Host a static website on Amazon S3.
//!
//! Every file in `www/` becomes a publicly readable object in a
//! website-enabled bucket, and the bucket's website endpoint is exported so
//! the site can be opened straight from `pulumi stack output`.
//!
//! The program depends on a generated AWS SDK, so generate that first:
//!
//! ```sh
//! pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks
//! pulumi up
//! ```

use std::path::{Path, PathBuf};

/// The directory whose contents are published, relative to the project root.
const SITE_DIR: &str = "www";

fn main() {
    pulumi::run(|ctx| async move {
        // The bucket, configured to serve `index.html` at the root of its
        // website endpoint. Every input of `BucketArgs` is optional, so the
        // generated struct derives `Default` and unset fields can be elided.
        let site_bucket = pulumi_aws::s3::Bucket::new(
            &ctx,
            "s3-website-bucket",
            pulumi_aws::s3::BucketArgs {
                website: Some(pulumi_aws::types::S3BucketWebsiteArgs {
                    index_document: Some(pulumi::pv::string("index.html").cast()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Which files to upload is ordinary local data: it is known before
        // the program says anything to the engine, so this is plain Rust
        // rather than anything output-shaped.
        for path in site_files()? {
            let key = path
                .file_name()
                .expect("a directory entry always has a file name")
                .to_string_lossy()
                .into_owned();
            let file = path.to_string_lossy().into_owned();

            // `BucketObjectArgs` has a required input (`bucket`), so the
            // generator does not derive `Default` for it. Rust therefore
            // needs every field named; the ones this program leaves alone
            // are `None`.
            pulumi_aws::s3::BucketObject::new(
                &ctx,
                &key,
                pulumi_aws::s3::BucketObjectArgs {
                    // Feeding the bucket's own output into the object makes
                    // the engine order the two registrations and records the
                    // dependency in state.
                    bucket: site_bucket.bucket().cast(),
                    key: Some(pulumi::pv::string(key.clone()).cast()),
                    // The file's bytes travel to the provider as an asset;
                    // the path is resolved relative to the project root.
                    source: Some(pulumi::pv::file_asset(pulumi::pv::string(file)).cast()),
                    // Without this, S3 serves everything as
                    // application/octet-stream and browsers download the
                    // page instead of rendering it.
                    content_type: Some(pulumi::pv::string(content_type(&path)).cast()),
                    acl: Some(pulumi::pv::string("public-read").cast()),

                    bucket_key_enabled: None,
                    cache_control: None,
                    content: None,
                    content_base64: None,
                    content_disposition: None,
                    content_encoding: None,
                    content_language: None,
                    etag: None,
                    force_destroy: None,
                    kms_key_id: None,
                    metadata: None,
                    object_lock_legal_hold_status: None,
                    object_lock_mode: None,
                    object_lock_retain_until_date: None,
                    region: None,
                    server_side_encryption: None,
                    source_hash: None,
                    storage_class: None,
                    tags: None,
                    website_redirect: None,
                },
                pulumi::ResourceOptions::default(),
            );
        }

        ctx.export("bucket_name", site_bucket.bucket().cast::<pulumi::PropertyValue>());
        ctx.export(
            "website_url",
            site_bucket.website_endpoint().cast::<pulumi::PropertyValue>(),
        );

        Ok(())
    });
}

/// The files to publish, sorted so that resource names — and therefore
/// URNs — are stable from run to run. `read_dir` yields entries in whatever
/// order the filesystem hands back, which is not stable across machines.
fn site_files() -> pulumi::Result<Vec<PathBuf>> {
    let dir = std::fs::read_dir(SITE_DIR)
        .map_err(|e| pulumi::Error::new(format!("reading {SITE_DIR}/: {e}")))?;

    let mut files = Vec::new();
    for entry in dir {
        let entry = entry.map_err(|e| pulumi::Error::new(format!("reading {SITE_DIR}/: {e}")))?;
        let path = entry.path();
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// The MIME type to serve a file as, from its extension. A real site would
/// reach for a crate such as `mime_guess`; a short table keeps the example
/// free of third-party dependencies.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") | Some("htm") => "text/html",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
}
