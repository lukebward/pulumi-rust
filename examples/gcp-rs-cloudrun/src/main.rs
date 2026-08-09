//! Run a container on Google Cloud Run.
//!
//! The Rust port of
//! [`gcp-ts-cloudrun`](https://github.com/pulumi/examples/tree/master/gcp-ts-cloudrun):
//! a `gcp:cloudrun:Service` running a public image, plus a
//! `gcp:cloudrun:IamMember` granting `roles/run.invoker` to `allUsers` so the
//! endpoint answers without credentials. The service's URL comes back as a
//! stack output.
//!
//! Generate the SDK the program depends on, then run it:
//!
//! ```sh
//! pulumi package gen-sdk gcp@9.33.0 --language rust --out ./sdks/gcp
//! pulumi config set gcp:project $(gcloud config get-value project)
//! pulumi up
//! ```

use pulumi_gcp::{cloudrun, types};

/// Google's sample container. It serves an HTML page on whatever port the
/// `PORT` environment variable names, which Cloud Run sets to 8080.
const DEFAULT_IMAGE: &str = "gcr.io/cloudrun/hello";

/// Where the service runs when `pulumi config set region ...` is not used.
const DEFAULT_REGION: &str = "us-central1";

/// The port the container listens on. Cloud Run injects `PORT` with this
/// value, and the `ports` block below has to agree with it.
const CONTAINER_PORT: f64 = 8080.0;

fn main() {
    pulumi::run(|ctx| async move {
        // `location` is a required input on the Service, so unlike the other
        // GCP examples in this repository this one cannot fall back to the
        // provider's `gcp:region`. It reads a project-scoped `region` key
        // instead, defaulting to us-central1.
        let region = ctx.config().get_string_or(
            "region",
            pulumi::PropertyValue::String(DEFAULT_REGION.to_string()),
        );

        // Any image Cloud Run can pull works here — swap in one from Artifact
        // Registry with `pulumi config set image ...`.
        let image = ctx.config().get_string_or(
            "image",
            pulumi::PropertyValue::String(DEFAULT_IMAGE.to_string()),
        );

        // `CloudrunServiceTemplateSpecContainerArgs` requires `image`, so the
        // generator does not derive `Default` for it and Rust needs every
        // field named. The ones this program leaves alone are `None`.
        let container = types::CloudrunServiceTemplateSpecContainerArgs {
            image: image.cast(),
            // Telling Knative which port to route to. Cloud Run defaults to
            // 8080, so this is documentation as much as configuration — but
            // it is also the one nested list this example needs.
            ports: Some(vec![types::CloudrunServiceTemplateSpecContainerPortArgs {
                container_port: Some(pulumi::pv::number(CONTAINER_PORT).cast()),
                name: None,
                protocol: None,
            }]),
            // `limits` is a plain string map in the schema, so `pv::object`
            // builds it and `cast` reinterprets the dynamic value as the
            // `BTreeMap<String, String>` the field is typed as.
            resources: Some(types::CloudrunServiceTemplateSpecContainerResourcesArgs {
                limits: Some(
                    pulumi::pv::object(vec![
                        ("cpu".to_string(), pulumi::pv::string("1000m")),
                        ("memory".to_string(), pulumi::pv::string("256Mi")),
                    ])
                    .cast(),
                ),
                requests: None,
            }),

            args: None,
            commands: None,
            env_froms: None,
            envs: None,
            liveness_probe: None,
            name: None,
            readiness_probe: None,
            startup_probe: None,
            volume_mounts: None,
            working_dir: None,
        };

        // The service itself. `ServiceArgs` requires `location`, so again
        // every field is named. `project` is left unset and comes from the
        // provider's `gcp:project` configuration.
        //
        // `CloudrunServiceTemplateArgs` and
        // `CloudrunServiceTemplateSpecArgs` are all-optional, so both derive
        // `Default` — they are still written out in full here because the
        // three-deep nesting reads better when nothing is hidden.
        let service = cloudrun::Service::new(
            &ctx,
            "hello",
            cloudrun::ServiceArgs {
                location: region.cast(),
                template: Some(types::CloudrunServiceTemplateArgs {
                    metadata: None,
                    spec: Some(types::CloudrunServiceTemplateSpecArgs {
                        containers: Some(vec![container]),

                        container_concurrency: None,
                        node_selector: None,
                        service_account_name: None,
                        serving_state: None,
                        timeout_seconds: None,
                        volumes: None,
                    }),
                }),
                // Let Cloud Run name each revision. Without this, changing the
                // image with a fixed `template.metadata.name` is rejected: a
                // Knative revision name cannot be reused.
                autogenerate_revision_name: Some(pulumi::pv::bool(true).cast()),

                deletion_policy: None,
                metadata: None,
                name: None,
                project: None,
                // Unset means all traffic goes to the latest revision, which
                // is what a single-revision service wants.
                traffics: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // A new Cloud Run service rejects unauthenticated requests. Granting
        // `run.invoker` to `allUsers` is what makes the `curl` in the README
        // work without a token. Reading `location` and `project` back off the
        // service rather than restating them keeps the binding attached to
        // wherever the service actually landed.
        cloudrun::IamMember::new(
            &ctx,
            "invoker",
            cloudrun::IamMemberArgs {
                service: service.name().cast(),
                role: pulumi::pv::string("roles/run.invoker").cast(),
                member: pulumi::pv::string("allUsers").cast(),
                location: Some(service.location().cast()),
                project: Some(service.project().cast()),

                condition: None,
            },
            pulumi::ResourceOptions::default(),
        );

        ctx.export("serviceName", service.name().cast::<pulumi::PropertyValue>());

        // The URL is nested two levels down in the service's outputs. The
        // GCP provider surfaces Knative's `status` block as a *list* named
        // `statuses`, so the accessor hands back
        // `Output<Vec<CloudrunServiceStatus>>` and reaching the URL means
        // indexing twice: position 0, then the `url` key.
        //
        // `Output::index` takes anything that converts into a `PropIndex`
        // (`usize` for array positions, `&str` for object keys) and returns
        // `Output<PropertyValue>`, propagating unknownness, secretness, and
        // dependencies. During a preview the whole thing is unknown, so
        // neither index runs.
        ctx.export(
            "serviceUrl",
            service
                .statuses()
                .index(0usize)
                .index("url")
                .cast::<pulumi::PropertyValue>(),
        );

        Ok(())
    });
}
