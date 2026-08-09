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
        // Kubernetes' int-or-string) and so surface as
        // `Output<PropertyValue>`.
        let container = types::CoreV1ContainerArgs {
            name: Some(pulumi::Output::known(APP_NAME.to_string())),
            image: Some(pulumi::Output::known(IMAGE.to_string())),
            // The port nginx listens on inside the pod. This is documentation
            // for humans and for `kubectl`; it is the Service that actually
            // routes traffic here.
            ports: Some(vec![types::CoreV1ContainerPortArgs {
                container_port: Some(pulumi::Output::known(80)),
                name: Some(pulumi::Output::known("http".to_string())),
                ..Default::default()
            }]),
            ..Default::default()
        };

        // Pulling it out into its own binding keeps the Deployment literal
        // below readable.
        let pod_spec = types::CoreV1PodSpecArgs {
            containers: Some(vec![container]),
            ..Default::default()
        };

        // `DeploymentSpecArgs` is not — `selector` and `template` are required
        // — so that one is spelled out in full.
        let deployment = apps_v1::Deployment::new(
            &ctx,
            APP_NAME,
            apps_v1::DeploymentArgs {
                spec: Some(types::AppsV1DeploymentSpecArgs {
                    replicas: Some(replicas.cast()),
                    // Which pods this Deployment owns.
                    selector: Some(types::MetaV1LabelSelectorArgs {
                        match_labels: Some(pulumi::Output::known(app_labels())),
                        ..Default::default()
                    }),
                    template: Some(types::CoreV1PodTemplateSpecArgs {
                        // The labels stamped onto each pod. They have to match
                        // the selector above or the API server rejects the
                        // Deployment.
                        metadata: Some(types::MetaV1ObjectMetaArgs {
                            labels: Some(pulumi::Output::known(app_labels())),
                            ..Default::default()
                        }),
                        spec: Some(pod_spec),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // A stable in-cluster address for the pods. `ClusterIP` keeps this
        // example runnable on any cluster, including a local minikube or kind.
        // Switch `type` to `LoadBalancer` on a cloud cluster to get an
        // external address instead — the cluster IP is still allocated either
        // way.
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
                        port: Some(pulumi::Output::known(80)),
                        // Kubernetes' int-or-string: a port number or the
                        // container port's name. `pv` builds the dynamic value.
                        target_port: Some(pulumi::pv::string("http").cast()),
                        name: Some(pulumi::Output::known("http".to_string())),
                        protocol: Some(pulumi::Output::known("TCP".to_string())),
                        ..Default::default()
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
            "deploymentName",
            deployment.metadata().map(|m| m.name).cast(),
        );
        ctx.export("serviceName", service.metadata().map(|m| m.name).cast());

        // The API server allocates this when the Service is created; on a
        // preview, or for a headless Service, it stays unset.
        ctx.export("clusterIp", service.spec().map(|s| s.cluster_ip).cast());

        Ok(())
    });
}
