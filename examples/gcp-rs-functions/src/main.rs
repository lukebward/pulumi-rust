//! Deploy an HTTP-triggered Google Cloud Function.
//!
//! The JavaScript in `function/` is zipped up into a GCS bucket and a
//! first-generation Cloud Function is pointed at the resulting object, then
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

fn main() {
    pulumi::run(|ctx| async move {
        // A bucket to stage the function's source zip in. `location` is the
        // one required input of `BucketArgs`, so the generator does not
        // derive `Default` for it and Rust needs every field named — the
        // ones this program leaves alone are `None`.
        let source_bucket = pulumi_gcp::storage::Bucket::new(
            &ctx,
            "function-source",
            pulumi_gcp::storage::BucketArgs {
                location: pulumi::pv::string(BUCKET_LOCATION).cast(),
                // Let `pulumi destroy` remove the bucket even though the
                // source object is still in it.
                force_destroy: Some(pulumi::pv::bool(true).cast()),
                // Objects here are only ever read by Cloud Build, so keep
                // the legacy per-object ACLs switched off.
                uniform_bucket_level_access: Some(pulumi::pv::bool(true).cast()),

                autoclass: None,
                cors: None,
                custom_placement_config: None,
                default_event_based_hold: None,
                deletion_policy: None,
                enable_object_retention: None,
                encryption: None,
                hierarchical_namespace: None,
                ip_filter: None,
                labels: None,
                lifecycle_rules: None,
                logging: None,
                name: None,
                project: None,
                public_access_prevention: None,
                requester_pays: None,
                retention_policy: None,
                rpo: None,
                soft_delete_policy: None,
                storage_class: None,
                versioning: None,
                website: None,
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
                bucket: source_bucket.name().cast(),
                name: Some(
                    pulumi::pv::concat(vec![
                        pulumi::pv::string("function-source-"),
                        fingerprint,
                        pulumi::pv::string(".zip"),
                    ])
                    .cast(),
                ),
                source: Some(
                    pulumi::pv::file_archive(pulumi::pv::string(FUNCTION_DIR)).cast(),
                ),

                cache_control: None,
                content: None,
                content_disposition: None,
                content_encoding: None,
                content_language: None,
                content_type: None,
                contexts: None,
                customer_encryption: None,
                deletion_policy: None,
                detect_md5hash: None,
                event_based_hold: None,
                force_empty_content_type: None,
                kms_key_name: None,
                metadata: None,
                retention: None,
                source_md5hash: None,
                storage_class: None,
                temporary_hold: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // The function. `runtime` is the only required input, but the args
        // struct therefore has no `Default`, so again every field is named.
        // `project` and `region` are left unset and come from the provider's
        // `gcp:project` / `gcp:region` configuration.
        let greeting = pulumi_gcp::cloudfunctions::Function::new(
            &ctx,
            "greeting",
            pulumi_gcp::cloudfunctions::FunctionArgs {
                runtime: pulumi::pv::string("nodejs20").cast(),
                // The name of the exported member in `function/index.js`.
                entry_point: Some(pulumi::pv::string("handler").cast()),
                trigger_http: Some(pulumi::pv::bool(true).cast()),
                source_archive_bucket: Some(source_bucket.name().cast()),
                source_archive_object: Some(source_object.name().cast()),
                available_memory_mb: Some(pulumi::pv::number(256.0).cast()),
                description: Some(
                    pulumi::pv::string("An HTTP function deployed from Rust.").cast(),
                ),

                automatic_update_policy: None,
                build_environment_variables: None,
                build_service_account: None,
                build_worker_pool: None,
                deletion_policy: None,
                docker_registry: None,
                docker_repository: None,
                environment_variables: None,
                // Unset because this is an HTTP function; an event-driven
                // function sets `event_trigger` instead of `trigger_http`.
                event_trigger: None,
                https_trigger_security_level: None,
                https_trigger_url: None,
                ingress_settings: None,
                kms_key_name: None,
                labels: None,
                max_instances: None,
                min_instances: None,
                name: None,
                on_deploy_update_policy: None,
                project: None,
                region: None,
                secret_environment_variables: None,
                secret_volumes: None,
                service_account_email: None,
                source_repository: None,
                timeout: None,
                vpc_connector: None,
                vpc_connector_egress_settings: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // A freshly created function rejects unauthenticated callers, so
        // grant the invoker role to everyone. Reading `project` and `region`
        // back off the function rather than restating them keeps the binding
        // attached to wherever the function actually landed.
        pulumi_gcp::cloudfunctions::FunctionIamMember::new(
            &ctx,
            "invoker",
            pulumi_gcp::cloudfunctions::FunctionIamMemberArgs {
                cloud_function: greeting.name().cast(),
                role: pulumi::pv::string("roles/cloudfunctions.invoker").cast(),
                member: pulumi::pv::string("allUsers").cast(),
                project: Some(greeting.project().cast()),
                region: Some(greeting.region().cast()),

                condition: None,
            },
            pulumi::ResourceOptions::default(),
        );

        ctx.export("functionName", greeting.name().cast::<pulumi::PropertyValue>());
        ctx.export(
            "functionUrl",
            greeting.https_trigger_url().cast::<pulumi::PropertyValue>(),
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
