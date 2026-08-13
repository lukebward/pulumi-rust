//! Deploy an HTTP-triggered Google Cloud Function.
//!
//! The JavaScript in `function/` is zipped up into a GCS bucket and a
//! second-generation Cloud Function is pointed at the resulting object, then
//! opened to the world with an IAM binding. The function's URL comes back as
//! a stack output.
//!
//! The program depends on a generated GCP SDK, so generate that first:
//!
//! ```sh
//! pulumi package gen-sdk gcp@9.33.0 --language rust --out ./sdks/gcp
//! pulumi up
//! ```

use std::path::PathBuf;

/// The local directory holding the function's source, relative to the
/// project root. Its contents become the uploaded zip.
const FUNCTION_DIR: &str = "function";

/// Where the bucket lives. `US` is a multi-region; a Cloud Function can read
/// its source from any bucket location.
const BUCKET_LOCATION: &str = "US";

/// Where the function runs. A gen2 function names its region on the resource
/// rather than inheriting `gcp:region` from provider configuration.
const FUNCTION_LOCATION: &str = "us-central1";

fn main() {
    pulumi::run(|ctx| async move {
        // A bucket to stage the function's source zip in.
        let source_bucket = pulumi_gcp::storage::Bucket::new(
            &ctx,
            "function-source",
            pulumi_gcp::storage::BucketArgs {
                location: Some(pulumi::pv::string(BUCKET_LOCATION).cast()),
                // Let `pulumi destroy` remove the bucket even though the
                // source object is still in it.
                force_destroy: Some(pulumi::pv::bool(true).cast()),
                // Objects here are only ever read by Cloud Build, so keep
                // the legacy per-object ACLs switched off.
                uniform_bucket_level_access: Some(pulumi::pv::bool(true).cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // A Cloud Function is pinned to one specific object in one specific
        // bucket, so editing `function/index.js` without changing the object
        // name leaves the deployed function running the old code: the
        // `Function` resource sees identical inputs and never redeploys.
        // Folding a fingerprint of the sources into the object name makes
        // every edit produce a new object and, in turn, a new deployment.
        let mut fingerprint_parts = Vec::new();
        for path in function_files()? {
            let path = path.to_string_lossy().into_owned();
            // The name matters as well as the bytes, so that renaming a file
            // changes the fingerprint.
            fingerprint_parts.push(pulumi::pv::string(path.clone()));
            fingerprint_parts.push(pulumi::pv::read_file(pulumi::pv::string(path)));
        }
        let fingerprint = pulumi::pv::sha1_hex(pulumi::pv::concat(fingerprint_parts));

        // The zip itself. `file_archive` over a directory uploads that
        // directory's contents at the root of the archive, which is the
        // layout Cloud Functions expects: `index.js` and `package.json` sit
        // beside each other at the top level.
        let source_object = pulumi_gcp::storage::BucketObject::new(
            &ctx,
            "function-source",
            pulumi_gcp::storage::BucketObjectArgs {
                // Feeding the bucket's own output into the object makes the
                // engine order the two registrations and records the
                // dependency in state.
                bucket: Some(source_bucket.name().cast()),
                name: Some(
                    pulumi::pv::concat(vec![
                        pulumi::pv::string("function-source-"),
                        fingerprint,
                        pulumi::pv::string(".zip"),
                    ])
                    .cast(),
                ),
                source: Some(pulumi::pv::file_archive(pulumi::pv::string(FUNCTION_DIR)).cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The function, second generation. Not a preference: Google no
        // longer permits 1st gen functions to be created in a project that
        // did not already have them, so `cloudfunctions::Function` — the
        // resource this example used to build — fails outright in the new
        // project someone following this README just made.
        //
        // The gen2 shape is different rather than merely renamed. Build
        // inputs and runtime inputs are separate nested structs, the source
        // is a `storage_source` naming bucket and object instead of two flat
        // properties, memory is a quantity string rather than a count of
        // megabytes, and `location` is explicit. `project` still comes from
        // the provider's `gcp:project` configuration.
        let greeting = pulumi_gcp::cloudfunctionsv2::Function::new(
            &ctx,
            "greeting",
            pulumi_gcp::cloudfunctionsv2::FunctionArgs {
                location: Some(pulumi::pv::string(FUNCTION_LOCATION).cast()),
                description: Some(
                    pulumi::pv::string("An HTTP function deployed from Rust.").cast(),
                ),
                build_config: Some(pulumi_gcp::types::Cloudfunctionsv2FunctionBuildConfigArgs {
                    runtime: Some(pulumi::pv::string("nodejs22").cast()),
                    // The name of the exported member in `function/index.js`.
                    entry_point: Some(pulumi::pv::string("handler").cast()),
                    source: Some(
                        pulumi_gcp::types::Cloudfunctionsv2FunctionBuildConfigSourceArgs {
                            storage_source: Some(
                                pulumi_gcp::types::Cloudfunctionsv2FunctionBuildConfigSourceStorageSourceArgs {
                                    bucket: Some(source_bucket.name().cast()),
                                    object: Some(source_object.name().cast()),
                                    ..Default::default()
                                },
                            ),
                            ..Default::default()
                        },
                    ),
                    ..Default::default()
                }),
                service_config: Some(pulumi_gcp::types::Cloudfunctionsv2FunctionServiceConfigArgs {
                    available_memory: Some(pulumi::pv::string("256M").cast()),
                    max_instance_count: Some(pulumi::pv::number(3.0).cast()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // A gen2 function is a Cloud Run service wearing a hat, and it is
        // Cloud Run that decides whether an anonymous caller gets in — so
        // the binding that opens it to the world is `roles/run.invoker` on
        // that service, not `roles/cloudfunctions.invoker` on the function.
        // Granting only the latter, as the gen1 version of this example did,
        // leaves the URL answering 403.
        //
        // The service carries the function's own name, and reading
        // `location` and `project` back off the function rather than
        // restating them keeps the binding attached to wherever it landed.
        pulumi_gcp::cloudrun::IamMember::new(
            &ctx,
            "invoker",
            pulumi_gcp::cloudrun::IamMemberArgs {
                service: Some(greeting.name().cast()),
                location: Some(greeting.location().cast()),
                project: Some(greeting.project().cast()),
                role: Some(pulumi::pv::string("roles/run.invoker").cast()),
                member: Some(pulumi::pv::string("allUsers").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        ctx.export(
            "functionName",
            greeting.name().cast::<pulumi::PropertyValue>(),
        );
        // gen2 has no flat `https_trigger_url`: the URL is the Cloud Run
        // service's, reported under `service_config`.
        ctx.export(
            "functionUrl",
            greeting
                .service_config()
                .map(|c| c.and_then(|c| c.uri))
                .cast::<pulumi::PropertyValue>(),
        );

        Ok(())
    });
}

/// The files that make up the function, sorted so the fingerprint does not
/// depend on the order `read_dir` happens to hand entries back in.
///
/// Which files exist is ordinary local data — known before the program says
/// anything to the engine — so this is plain Rust rather than anything
/// output-shaped. The listing is deliberately shallow: `function/` is flat,
/// and a nested source tree would want a recursive walk here.
fn function_files() -> pulumi::Result<Vec<PathBuf>> {
    let dir = std::fs::read_dir(FUNCTION_DIR)
        .map_err(|e| pulumi::Error::new(format!("reading {FUNCTION_DIR}/: {e}")))?;

    let mut files = Vec::new();
    for entry in dir {
        let entry =
            entry.map_err(|e| pulumi::Error::new(format!("reading {FUNCTION_DIR}/: {e}")))?;
        let path = entry.path();
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}
