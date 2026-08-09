//! An nginx Deployment fronted by a Service.
//!
//! The Rust port of
//! [`kubernetes-ts-nginx`](https://github.com/pulumi/examples/tree/master/kubernetes-ts-nginx):
//! a `kubernetes:apps/v1:Deployment` running the nginx image with a
//! configurable replica count, selected by the conventional `app: nginx`
//! label, and a `kubernetes:core/v1:Service` routing to the same label.
//!
//! Generate the SDK the program depends on, then run it:
//!
//! ```sh
//! pulumi package gen-sdk kubernetes@4.33.0 --language rust --out ./sdks/kubernetes
//! pulumi config set replicas 3
//! pulumi up
//! ```

use pulumi_kubernetes::{apps_v1, core_v1, types};
use std::collections::BTreeMap;

/// The name shared by the Deployment, the Service, and the container.
const APP_NAME: &str = "nginx";

/// A pinned tag rather than `nginx:latest`, so a `pulumi up` months from now
/// rolls out the same image it rolled out today.
const IMAGE: &str = "nginx:1.27-alpine";

/// The label the Deployment selects on and the Service routes to. Kubernetes
/// matches pods by label, not by name, so the same map has to appear in three
/// places: the pod template's metadata (which stamps it onto every pod), the
/// Deployment's selector, and the Service's selector.
fn app_labels() -> BTreeMap<String, String> {
    BTreeMap::from([("app".to_string(), APP_NAME.to_string())])
}

fn main() {
    pulumi::run(|ctx| async move {
        // `pulumi config set replicas 3` to scale out; one replica by default.
        let replicas = ctx
            .config()
            .get_int_or("replicas", pulumi::PropertyValue::Number(1.0));

        // Kubernetes args are typed all the way down: `spec` is a generated
        // `AppsV1DeploymentSpecArgs`, not an untyped bag, so this program uses
        // the generated structs rather than `pulumi::pv::object(..)`. The two
        // places it does reach for `pv` are the Service's `type` and
        // `targetPort` below, which are unions in the schema (`targetPort` is
        // Kubernetes' int-or-string) and so surface as `Output<PropertyValue>`.
        //
        // `CoreV1ContainerArgs` has a required `name`, so the generator does
        // not derive `Default` for it and Rust needs every field named. The
        // ones this program leaves alone are `None`.
        let container = types::CoreV1ContainerArgs {
            name: pulumi::Output::known(APP_NAME.to_string()),
            image: Some(pulumi::Output::known(IMAGE.to_string())),
            // The port nginx listens on inside the pod. This is documentation
            // for humans and for `kubectl`; it is the Service that actually
            // routes traffic here.
            ports: Some(vec![types::CoreV1ContainerPortArgs {
                container_port: pulumi::Output::known(80),
                name: Some(pulumi::Output::known("http".to_string())),
                host_ip: None,
                host_port: None,
                protocol: None,
            }]),

            args: None,
            command: None,
            env: None,
            env_from: None,
            image_pull_policy: None,
            lifecycle: None,
            liveness_probe: None,
            readiness_probe: None,
            resize_policy: None,
            resources: None,
            restart_policy: None,
            restart_policy_rules: None,
            security_context: None,
            startup_probe: None,
            stdin: None,
            stdin_once: None,
            termination_message_path: None,
            termination_message_policy: None,
            tty: None,
            volume_devices: None,
            volume_mounts: None,
            working_dir: None,
        };

        // `CoreV1PodSpecArgs` requires `containers`, so it too has no
        // `Default`. Pulling it out into its own binding keeps the Deployment
        // literal below readable.
        let pod_spec = types::CoreV1PodSpecArgs {
            containers: vec![container],

            active_deadline_seconds: None,
            affinity: None,
            automount_service_account_token: None,
            dns_config: None,
            dns_policy: None,
            enable_service_links: None,
            ephemeral_containers: None,
            host_aliases: None,
            host_ipc: None,
            host_network: None,
            host_pid: None,
            host_users: None,
            hostname: None,
            hostname_override: None,
            image_pull_secrets: None,
            init_containers: None,
            node_name: None,
            node_selector: None,
            os: None,
            overhead: None,
            preemption_policy: None,
            priority: None,
            priority_class_name: None,
            readiness_gates: None,
            resource_claims: None,
            resources: None,
            restart_policy: None,
            runtime_class_name: None,
            scheduler_name: None,
            scheduling_gates: None,
            scheduling_group: None,
            security_context: None,
            service_account: None,
            service_account_name: None,
            set_hostname_as_fqdn: None,
            share_process_namespace: None,
            subdomain: None,
            termination_grace_period_seconds: None,
            tolerations: None,
            topology_spread_constraints: None,
            volumes: None,
        };

        // `DeploymentArgs` is all-optional (`apiVersion` and `kind` are filled
        // in by the provider from the resource token), so it derives `Default`
        // and `..Default::default()` is legal here. `DeploymentSpecArgs` is
        // not — `selector` and `template` are required — so that one is spelled
        // out in full.
        let deployment = apps_v1::Deployment::new(
            &ctx,
            APP_NAME,
            apps_v1::DeploymentArgs {
                spec: Some(types::AppsV1DeploymentSpecArgs {
                    replicas: Some(replicas.cast()),
                    // Which pods this Deployment owns.
                    selector: types::MetaV1LabelSelectorArgs {
                        match_labels: Some(pulumi::Output::known(app_labels())),
                        ..Default::default()
                    },
                    template: types::CoreV1PodTemplateSpecArgs {
                        // The labels stamped onto each pod. They have to match
                        // the selector above or the API server rejects the
                        // Deployment.
                        metadata: Some(types::MetaV1ObjectMetaArgs {
                            labels: Some(pulumi::Output::known(app_labels())),
                            ..Default::default()
                        }),
                        spec: Some(pod_spec),
                    },

                    min_ready_seconds: None,
                    paused: None,
                    progress_deadline_seconds: None,
                    revision_history_limit: None,
                    strategy: None,
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // A stable in-cluster address for the pods. `ServiceSpecArgs` is
        // all-optional, so only the interesting fields appear.
        //
        // `ClusterIP` keeps this example runnable on any cluster, including a
        // local minikube or kind. Switch `type` to `LoadBalancer` on a cloud
        // cluster to get an external address instead — the cluster IP is still
        // allocated either way.
        let service = core_v1::Service::new(
            &ctx,
            APP_NAME,
            core_v1::ServiceArgs {
                metadata: Some(types::MetaV1ObjectMetaArgs {
                    labels: Some(pulumi::Output::known(app_labels())),
                    ..Default::default()
                }),
                spec: Some(types::CoreV1ServiceSpecArgs {
                    // Same label the Deployment stamps on its pods.
                    selector: Some(pulumi::Output::known(app_labels())),
                    // `type` is a union in the schema and a Rust keyword, so it
                    // arrives as `r#type: Option<Output<PropertyValue>>`.
                    r#type: Some(pulumi::pv::string("ClusterIP").cast()),
                    ports: Some(vec![types::CoreV1ServicePortArgs {
                        port: pulumi::Output::known(80),
                        // Kubernetes' int-or-string: a port number or the
                        // container port's name. `pv` builds the dynamic value.
                        target_port: Some(pulumi::pv::string("http").cast()),
                        name: Some(pulumi::Output::known("http".to_string())),
                        protocol: Some(pulumi::Output::known("TCP".to_string())),
                        app_protocol: None,
                        node_port: None,
                    }]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions {
                // Nothing in the Service's inputs refers to the Deployment, so
                // without this the engine is free to create them in parallel.
                // Ordering them means the Service never briefly points at
                // nothing.
                depends_on: vec![deployment.pulumi_resource().clone()],
                ..Default::default()
            },
        );

        // `metadata` and `spec` come back as typed structs, so reading a field
        // off them is an ordinary field access inside `map`. Every field of
        // `ObjectMeta` is optional in the schema, hence the `Option`.
        ctx.export(
            "deployment_name",
            deployment.metadata().map(|m| m.name).cast(),
        );
        ctx.export("serviceName", service.metadata().map(|m| m.name).cast());

        // The API server allocates this when the Service is created; on a
        // preview, or for a headless Service, it stays unset.
        ctx.export("clusterIp", service.spec().map(|s| s.cluster_ip).cast());

        Ok(())
    });
}
