//! The Kubernetes Guestbook: a PHP frontend over a Redis leader/follower pair.
//!
//! The Rust port of
//! [`kubernetes-ts-guestbook`](https://github.com/pulumi/examples/tree/master/kubernetes-ts-guestbook/simple).
//! Three tiers, each a `kubernetes:apps/v1:Deployment` plus a
//! `kubernetes:core/v1:Service`: `redis-leader` takes the writes,
//! `redis-follower` replicates them for reads, and `frontend` serves the
//! guestbook page and talks to both by DNS name.
//!
//! The frontend Service is `ClusterIP` by default, which works on any cluster
//! including minikube and kind. Set `useLoadBalancer` on a cloud cluster to
//! get an externally reachable address instead.
//!
//! Generate the SDK the program depends on, then run it:
//!
//! ```sh
//! pulumi package gen-sdk kubernetes@4.33.0 --language rust --out ./sdks/kubernetes
//! pulumi config set useLoadBalancer true   # on a cloud cluster
//! pulumi up
//! ```

use pulumi_kubernetes::{apps_v1, core_v1, types};
use std::collections::BTreeMap;

/// The port both Redis tiers listen on.
const REDIS_PORT: i32 = 6379;

/// The port the PHP frontend listens on inside its pod, and the port its
/// Service publishes.
const FRONTEND_PORT: i32 = 80;

/// Pinned image tags rather than `:latest`, so a `pulumi up` months from now
/// rolls out the images it rolls out today.
const REDIS_LEADER_IMAGE: &str = "redis:6.0.5";
const REDIS_FOLLOWER_IMAGE: &str = "gcr.io/google-samples/gb-redis-follower:v2";
const FRONTEND_IMAGE: &str = "gcr.io/google-samples/gb-frontend:v5";

/// One tier of the guestbook. All three tiers are the same shape — a
/// Deployment of identical pods and a Service in front of them — so they are
/// described as data here and built by [`deploy_tier`] below, rather than
/// written out three times.
struct Tier {
    /// The Pulumi resource name, the container name, and the value of the
    /// `app` label the Deployment selects on and the Service routes to.
    name: &'static str,
    /// The container image.
    image: &'static str,
    /// How many pods the Deployment runs.
    replicas: i32,
    /// The port the container listens on. The Service publishes the same
    /// number and targets it.
    port: i32,
    /// Environment variables for the container, as `(name, value)`.
    env: &'static [(&'static str, &'static str)],
    /// The Service's `metadata.name`, when it has to be a fixed string.
    /// Setting it turns off Pulumi's auto-naming: whatever is here is exactly
    /// what lands on the cluster, and exactly what in-cluster DNS resolves.
    /// `None` lets Pulumi auto-name the Service.
    service_name: Option<&'static str>,
    /// The Service's `spec.type`: `ClusterIP` or `LoadBalancer`.
    service_type: &'static str,
}

/// Create one tier's Deployment and Service, and return the Service.
///
/// Kubernetes args are typed all the way down — `spec` is a generated
/// `AppsV1DeploymentSpecArgs`, not an untyped bag — so this program builds
/// them from the generated structs, exactly as `kubernetes-rs-nginx` does.
/// Nothing here is an any-shaped field, so `pulumi::pv::object(vec![..])`
/// never appears; the only reach for `pv` is the Service's `type` and
/// `targetPort`, which are unions in the schema (`targetPort` is Kubernetes'
/// int-or-string) and so arrive as `Output<PropertyValue>`.
fn deploy_tier(ctx: &pulumi::Context, tier: &Tier) -> core_v1::Service {
    // Kubernetes matches pods by label, not by name, so the same map has to
    // appear in three places: the pod template's metadata (which stamps it
    // onto every pod), the Deployment's selector, and the Service's selector.
    let labels = BTreeMap::from([("app".to_string(), tier.name.to_string())]);

    let container = types::CoreV1ContainerArgs {
        name: Some(pulumi::Output::known(tier.name.to_string())),
        image: Some(pulumi::Output::known(tier.image.to_string())),
        ports: Some(vec![types::CoreV1ContainerPortArgs {
            container_port: Some(pulumi::Output::known(tier.port)),
            ..Default::default()
        }]),
        // The frontend and the follower both read `GET_HOSTS_FROM`: `dns`
        // tells them to resolve `redis-leader` and `redis-follower` through
        // the cluster's DNS service, which is why those two Services have
        // fixed names below. A tier with no environment leaves the field unset
        // rather than sending an empty list, so the manifest matches what
        // `kubectl` would have applied.
        env: if tier.env.is_empty() {
            None
        } else {
            Some(
                tier.env
                    .iter()
                    .map(|(name, value)| types::CoreV1EnvVarArgs {
                        name: Some(pulumi::Output::known(name.to_string())),
                        value: Some(pulumi::Output::known(value.to_string())),
                        ..Default::default()
                    })
                    .collect(),
            )
        },
        // Modest requests so all three tiers fit on a one-node cluster.
        resources: Some(types::CoreV1ResourceRequirementsArgs {
            requests: Some(pulumi::Output::known(BTreeMap::from([
                ("cpu".to_string(), "100m".to_string()),
                ("memory".to_string(), "100Mi".to_string()),
            ]))),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Pulling it out into its own binding keeps the Deployment literal below
    // readable.
    let pod_spec = types::CoreV1PodSpecArgs {
        containers: Some(vec![container]),
        // Two of the three images are published as single-architecture
        // amd64 manifests — `gb-frontend:v5` and `gb-redis-follower:v2` are
        // `manifest.v2+json` carrying `"architecture": "amd64"` rather than
        // manifest lists — and no arm64 build of either exists. On a
        // mixed-architecture cluster, saying so is the difference between
        // the pods landing somewhere they can run and an `ImagePullBackOff`
        // whose message ("no match for platform in manifest") points at the
        // registry rather than at the scheduler.
        //
        // All three tiers carry it, not just the two that need it, so the
        // guestbook runs as one unit rather than split across
        // architectures. On an arm64-only cluster the pods stay Pending
        // with a node-selector message, which is at least a true
        // description of the problem.
        node_selector: Some(pulumi::Output::known(BTreeMap::from([(
            "kubernetes.io/arch".to_string(),
            "amd64".to_string(),
        )]))),
        ..Default::default()
    };

    // `AppsV1DeploymentSpecArgs` is not — `selector` and `template` are
    // required — so that one is spelled out.
    let deployment = apps_v1::Deployment::new(
        ctx,
        tier.name,
        apps_v1::DeploymentArgs {
            spec: Some(types::AppsV1DeploymentSpecArgs {
                replicas: Some(pulumi::Output::known(tier.replicas)),
                // Which pods this Deployment owns.
                selector: Some(types::MetaV1LabelSelectorArgs {
                    match_labels: Some(pulumi::Output::known(labels.clone())),
                    ..Default::default()
                }),
                template: Some(types::CoreV1PodTemplateSpecArgs {
                    // The labels stamped onto each pod. They have to match the
                    // selector above or the API server rejects the Deployment.
                    metadata: Some(types::MetaV1ObjectMetaArgs {
                        labels: Some(pulumi::Output::known(labels.clone())),
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

    core_v1::Service::new(
        ctx,
        tier.name,
        core_v1::ServiceArgs {
            metadata: Some(types::MetaV1ObjectMetaArgs {
                // `Option<&str> -> Option<Output<String>>`: a tier that names
                // its Service gets that exact name on the cluster, and one
                // that does not gets Pulumi's auto-generated name.
                name: tier
                    .service_name
                    .map(|name| pulumi::Output::known(name.to_string())),
                labels: Some(pulumi::Output::known(labels.clone())),
                ..Default::default()
            }),
            spec: Some(types::CoreV1ServiceSpecArgs {
                // Same label the Deployment stamps on its pods.
                selector: Some(pulumi::Output::known(labels)),
                // `type` is a union in the schema and a Rust keyword, so it
                // arrives as `r#type: Option<Output<PropertyValue>>`.
                r#type: Some(pulumi::pv::string(tier.service_type).cast()),
                // `targetPort` is Kubernetes' int-or-string union;
                // `pv::number` builds the dynamic value.
                ports: Some(vec![types::CoreV1ServicePortArgs {
                    port: Some(pulumi::Output::known(tier.port)),
                    target_port: Some(pulumi::pv::number(f64::from(tier.port)).cast()),
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
            // Ordering them means the Service never briefly points at nothing.
            depends_on: vec![deployment.pulumi_resource().clone()],
            ..Default::default()
        },
    )
}

fn main() {
    pulumi::run(|ctx| async move {
        // Whether to expose the frontend with a `LoadBalancer` Service.
        // minikube and kind do not implement one, so the default is
        // `ClusterIP`; on EKS/GKE/AKS run `pulumi config set useLoadBalancer
        // true` to get an external address.
        //
        // Configuration is known before the program starts, so awaiting this
        // output resolves immediately — which is what lets the rest of the
        // program branch on an ordinary Rust `bool` instead of threading the
        // choice through `Output::map`.
        let use_load_balancer_config = ctx
            .config()
            .get_bool_or("useLoadBalancer", pulumi::PropertyValue::Bool(false));
        let use_load_balancer = matches!(
            use_load_balancer_config.data().await.value,
            pulumi::PropertyValue::Bool(true)
        );

        // Redis leader: the single writable Redis instance. Its Service must
        // be called exactly `redis-leader`, because that is the hostname the
        // follower and the frontend look up.
        deploy_tier(
            &ctx,
            &Tier {
                name: "redis-leader",
                image: REDIS_LEADER_IMAGE,
                replicas: 1,
                port: REDIS_PORT,
                env: &[],
                service_name: Some("redis-leader"),
                service_type: "ClusterIP",
            },
        );

        // Redis followers: read replicas of the leader. Same fixed-name
        // requirement — the frontend reads from `redis-follower`.
        deploy_tier(
            &ctx,
            &Tier {
                name: "redis-follower",
                image: REDIS_FOLLOWER_IMAGE,
                replicas: 2,
                port: REDIS_PORT,
                env: &[("GET_HOSTS_FROM", "dns")],
                service_name: Some("redis-follower"),
                service_type: "ClusterIP",
            },
        );

        // The PHP guestbook itself. Nothing resolves this Service by name, so
        // it is left auto-named and its real name is exported below.
        let frontend = deploy_tier(
            &ctx,
            &Tier {
                name: "frontend",
                image: FRONTEND_IMAGE,
                replicas: 3,
                port: FRONTEND_PORT,
                env: &[("GET_HOSTS_FROM", "dns")],
                service_name: None,
                service_type: if use_load_balancer {
                    "LoadBalancer"
                } else {
                    "ClusterIP"
                },
            },
        );

        // Pulumi auto-names this Service, so `frontend` on the cluster is
        // really `frontend-a1b2c3d4`. Export the real name to feed `kubectl`.
        // Every field of `ObjectMeta` is optional in the schema, hence the
        // `Option`.
        ctx.export("frontendName", frontend.metadata().map(|m| m.name).cast());

        // The frontend's address, and the reason the flag exists. A
        // `LoadBalancer` Service gets its external address asynchronously, in
        // `status.loadBalancer.ingress`; a `ClusterIP` Service only ever has
        // the in-cluster address in `spec.clusterIP`. `status` is an optional
        // output, and every level below it is optional too, so the
        // `LoadBalancer` branch is a chain of `and_then`. Clouds that hand out
        // a hostname rather than an IP (AWS does) populate `hostname`, so fall
        // back to it.
        let frontend_ip: pulumi::Output<pulumi::PropertyValue> = if use_load_balancer {
            frontend
                .status()
                .map(|status: Option<types::CoreV1ServiceStatus>| {
                    status
                        .and_then(|s| s.load_balancer)
                        .and_then(|lb| lb.ingress)
                        .and_then(|ingress| ingress.into_iter().next())
                        .and_then(|ingress| ingress.ip.or(ingress.hostname))
                })
                .cast()
        } else {
            frontend.spec().map(|spec| spec.cluster_ip).cast()
        };
        ctx.export("frontendIp", frontend_ip);

        Ok(())
    });
}
