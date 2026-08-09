//! A static website behind the CloudFront CDN.
//!
//! The content lives in a private S3 bucket — one object per file in
//! `www/` — and a CloudFront distribution sits in front of it. The bucket
//! is not public: an origin access identity gives the CDN, and only the
//! CDN, permission to read the objects, granted by a bucket policy. The
//! bucket's name and the CDN's URL come back as stack outputs.
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

/// The name the distribution uses for its one origin. It appears twice —
/// on the origin and on the cache behaviour that targets it — and the two
/// must agree, so it is a constant rather than two string literals.
const ORIGIN_ID: &str = "s3-content-origin";

fn main() {
    pulumi::run(|ctx| async move {
        // The content bucket. The bucket is reachable only through the CDN.
        let content_bucket = pulumi_aws::s3::Bucket::new(
            &ctx,
            "content-bucket",
            pulumi_aws::s3::BucketArgs::default(),
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
                    bucket: Some(content_bucket.bucket().cast()),
                    key: Some(pulumi::pv::string(key.clone()).cast()),
                    // The file's bytes travel to the provider as an asset;
                    // the path is resolved relative to the project root.
                    source: Some(pulumi::pv::file_asset(pulumi::pv::string(file)).cast()),
                    // Without this, S3 serves everything as
                    // application/octet-stream and browsers download the
                    // page instead of rendering it. CloudFront passes the
                    // origin's content type straight through.
                    content_type: Some(pulumi::pv::string(content_type(&path)).cast()),
                    ..Default::default()
                },
                pulumi::ResourceOptions::default(),
            );
        }

        // An origin access identity is a principal CloudFront signs its
        // requests to S3 as. Giving that principal — and nobody else — read
        // access is what keeps the bucket private while the site is public.
        let origin_access_identity = pulumi_aws::cloudfront::OriginAccessIdentity::new(
            &ctx,
            "content-origin-access-identity",
            pulumi_aws::cloudfront::OriginAccessIdentityArgs {
                comment: Some(
                    pulumi::pv::string("Lets the CDN, and only the CDN, read the bucket").cast(),
                ),
            },
            pulumi::ResourceOptions::default(),
        );

        // The policy is built as a value and serialized, rather than
        // spliced together as a string: `to_json` waits for the identity's
        // ARN, and an unknown value inside stays unknown all the way out,
        // so a preview shows the policy as unknown instead of as JSON with
        // a hole in it.
        let read_policy = pulumi::pv::to_json(pulumi::pv::object(vec![
            ("Version".to_string(), pulumi::pv::string("2012-10-17")),
            (
                "Statement".to_string(),
                pulumi::pv::array(vec![pulumi::pv::object(vec![
                    ("Effect".to_string(), pulumi::pv::string("Allow")),
                    (
                        "Principal".to_string(),
                        pulumi::pv::object(vec![(
                            "AWS".to_string(),
                            origin_access_identity.iam_arn().cast(),
                        )]),
                    ),
                    ("Action".to_string(), pulumi::pv::string("s3:GetObject")),
                    (
                        // Every object in the bucket, not the bucket itself.
                        "Resource".to_string(),
                        pulumi::pv::concat(vec![
                            content_bucket.arn().cast(),
                            pulumi::pv::string("/*"),
                        ]),
                    ),
                ])]),
            ),
        ]));

        pulumi_aws::s3::BucketPolicy::new(
            &ctx,
            "content-bucket-policy",
            pulumi_aws::s3::BucketPolicyArgs {
                bucket: Some(content_bucket.bucket().cast()),
                policy: Some(read_policy.cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The distribution.
        let cdn = pulumi_aws::cloudfront::Distribution::new(
            &ctx,
            "cdn",
            pulumi_aws::cloudfront::DistributionArgs {
                enabled: Some(pulumi::pv::bool(true).cast()),
                comment: Some(
                    pulumi::pv::string("CDN for a static website deployed from Rust").cast(),
                ),
                // What CloudFront serves for a request to `/`. An S3 REST
                // origin has no notion of an index document of its own.
                default_root_object: Some(pulumi::pv::string("index.html").cast()),

                origins: Some(vec![pulumi_aws::types::CloudfrontDistributionOriginArgs {
                    // The regional REST endpoint, not the website endpoint:
                    // an origin access identity only works against the REST
                    // API.
                    domain_name: Some(content_bucket.bucket_regional_domain_name().cast()),
                    origin_id: Some(pulumi::pv::string(ORIGIN_ID).cast()),
                    s3_origin_config: Some(
                        pulumi_aws::types::CloudfrontDistributionOriginS3OriginConfigArgs {
                            // This wants the identity's *path* —
                            // `origin-access-identity/cloudfront/***` — not
                            // its id or its ARN.
                            origin_access_identity: Some(origin_access_identity
                                .cloudfront_access_identity_path()
                                .cast()),
                            ..Default::default()
                        },
                    ),
                    ..Default::default()
                }]),

                default_cache_behavior:
                    Some(pulumi_aws::types::CloudfrontDistributionDefaultCacheBehaviorArgs {
                        target_origin_id: Some(pulumi::pv::string(ORIGIN_ID).cast()),
                        // A static site only reads.
                        allowed_methods: Some(pulumi::Output::known(vec![
                            "GET".to_string(),
                            "HEAD".to_string(),
                            "OPTIONS".to_string(),
                        ])),
                        cached_methods: Some(pulumi::Output::known(vec![
                            "GET".to_string(),
                            "HEAD".to_string(),
                        ])),
                        // Send anyone arriving over HTTP to HTTPS.
                        viewer_protocol_policy: Some(pulumi::pv::string("redirect-to-https").cast()),
                        compress: Some(pulumi::pv::bool(true).cast()),
                        // Nothing about the request varies the response, so
                        // every viewer shares one cache entry per path.
                        forwarded_values: Some(
                            pulumi_aws::types::CloudfrontDistributionDefaultCacheBehaviorForwardedValuesArgs {
                                query_string: Some(pulumi::pv::bool(false).cast()),
                                cookies: Some(pulumi_aws::types::CloudfrontDistributionDefaultCacheBehaviorForwardedValuesCookiesArgs {
                                    forward: Some(pulumi::pv::string("none").cast()),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        ),
                        min_ttl: Some(pulumi::Output::known(0)),
                        default_ttl: Some(pulumi::Output::known(600)),
                        max_ttl: Some(pulumi::Output::known(86400)),
                        ..Default::default()
                    }),

                // Required, even when there is nothing to restrict.
                restrictions: Some(pulumi_aws::types::CloudfrontDistributionRestrictionsArgs {
                    geo_restriction:
                        Some(pulumi_aws::types::CloudfrontDistributionRestrictionsGeoRestrictionArgs {
                            restriction_type: Some(pulumi::pv::string("none").cast()),
                            ..Default::default()
                        }),
                    ..Default::default()
                }),

                // Serve HTTPS with CloudFront's own `*.cloudfront.net`
                // certificate. A custom domain would set `aliases` above and
                // point `acm_certificate_arn` at a certificate issued in
                // us-east-1.
                viewer_certificate:
                    Some(pulumi_aws::types::CloudfrontDistributionViewerCertificateArgs {
                        cloudfront_default_certificate: Some(pulumi::pv::bool(true).cast()),
                        ..Default::default()
                    }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        ctx.export(
            "bucketName",
            content_bucket.bucket().cast::<pulumi::PropertyValue>(),
        );
        ctx.export(
            "cdnUrl",
            pulumi::pv::concat(vec![
                pulumi::pv::string("https://"),
                cdn.domain_name().cast(),
            ]),
        );
        // Handy for `aws cloudfront create-invalidation` after an update.
        ctx.export("distributionId", cdn.id().cast::<pulumi::PropertyValue>());

        Ok(())
    });
}

/// The files to publish, sorted so that resource names — and therefore
/// URNs — are stable from run to run. `read_dir` yields entries in whatever
/// order the filesystem hands back, which is not stable across machines.
///
/// The listing is deliberately shallow: `www/` is flat, and a nested site
/// would want a recursive walk here, with the key built from the path
/// relative to `www/` rather than from the file name alone.
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
