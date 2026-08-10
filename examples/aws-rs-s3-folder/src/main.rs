//! Host a static website on Amazon S3.
//!
//! Every file in `www/` becomes a publicly readable object in a
//! website-enabled bucket, and the bucket's website endpoint is exported so
//! the site can be opened straight from `pulumi stack output`.
//!
//! The program depends on a generated AWS SDK, so generate that first:
//!
//! ```sh
//! pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
//! pulumi up
//! ```

use std::path::{Path, PathBuf};

/// The directory whose contents are published, relative to the project root.
const SITE_DIR: &str = "www";

fn main() {
    pulumi::run(|ctx| async move {
        // The bucket, configured to serve `index.html` at the root of its
        // website endpoint.
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

            pulumi_aws::s3::BucketObject::new(
                &ctx,
                &key,
                pulumi_aws::s3::BucketObjectArgs {
                    // Feeding the bucket's own output into the object makes
                    // the engine order the two registrations and records the
                    // dependency in state.
                    bucket: Some(site_bucket.bucket().cast()),
                    key: Some(pulumi::pv::string(key.clone()).cast()),
                    // The file's bytes travel to the provider as an asset;
                    // the path is resolved relative to the project root.
                    source: Some(pulumi::pv::file_asset(pulumi::pv::string(file)).cast()),
                    // Without this, S3 serves everything as
                    // application/octet-stream and browsers download the
                    // page instead of rendering it.
                    content_type: Some(pulumi::pv::string(content_type(&path)).cast()),
                    ..Default::default()
                },
                pulumi::ResourceOptions::default(),
            );
        }

        // An object cannot make itself public. AWS changed the default
        // Object Ownership for new buckets to `BucketOwnerEnforced` in
        // April 2023, which disables ACLs outright, so a `PutObject`
        // carrying `acl = "public-read"` is rejected with
        // `AccessControlListNotSupported: The bucket does not allow ACLs`.
        // Public read has to come from a bucket policy instead.
        //
        // ACLs stay blocked here, because nothing in this program uses one;
        // only the two settings that gate a public *policy* are relaxed.
        let public_access = pulumi_aws::s3::BucketPublicAccessBlock::new(
            &ctx,
            "s3-website-bucket-access",
            pulumi_aws::s3::BucketPublicAccessBlockArgs {
                bucket: Some(site_bucket.bucket().cast()),
                block_public_acls: Some(pulumi::pv::bool(true).cast()),
                ignore_public_acls: Some(pulumi::pv::bool(true).cast()),
                block_public_policy: Some(pulumi::pv::bool(false).cast()),
                restrict_public_buckets: Some(pulumi::pv::bool(false).cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // `depends_on`, because the policy names the bucket and not the
        // access block, so nothing in the inputs tells the engine these two
        // are ordered — and putting a public policy on a bucket before the
        // block permits one fails with `AccessDenied`.
        pulumi_aws::s3::BucketPolicy::new(
            &ctx,
            "s3-website-bucket-policy",
            pulumi_aws::s3::BucketPolicyArgs {
                bucket: Some(site_bucket.bucket().cast()),
                policy: Some(pulumi::pv::to_json(pulumi::pv::object(vec![
                    ("Version".to_string(), pulumi::pv::string("2012-10-17")),
                    (
                        "Statement".to_string(),
                        pulumi::pv::array(vec![pulumi::pv::object(vec![
                            ("Sid".to_string(), pulumi::pv::string("PublicReadGetObject")),
                            ("Effect".to_string(), pulumi::pv::string("Allow")),
                            ("Principal".to_string(), pulumi::pv::string("*")),
                            ("Action".to_string(), pulumi::pv::string("s3:GetObject")),
                            (
                                // The objects, not the bucket: `<arn>/*`.
                                "Resource".to_string(),
                                pulumi::pv::concat(vec![
                                    site_bucket.arn().cast(),
                                    pulumi::pv::string("/*"),
                                ]),
                            ),
                        ])]),
                    ),
                ]))),
                ..Default::default()
            },
            pulumi::ResourceOptions {
                depends_on: vec![public_access.pulumi_resource().clone()],
                ..Default::default()
            },
        );

        let bucket_name = site_bucket.bucket();
        let website_url = site_bucket.website_endpoint();
        ctx.export("bucketName", bucket_name.cast::<pulumi::PropertyValue>());
        ctx.export("websiteUrl", website_url.cast::<pulumi::PropertyValue>());

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
