//! A GKE cluster with a separately-managed node pool.
//!
//! The Rust port of
//! [`gcp-ts-gke`](https://github.com/pulumi/examples/tree/master/gcp-ts-gke):
//! a `gcp:container:Cluster` whose default node pool is thrown away
//! immediately, plus a `gcp:container:NodePool` this program owns outright,
//! with a configurable node count and machine type. The cluster's endpoint
//! and CA certificate are folded into a kubeconfig, exported as a secret so
//! `pulumi stack output` does not print cluster credentials by accident.
//!
//! Generate the SDK the program depends on, then run it:
//!
//! ```sh
//! pulumi package gen-sdk gcp@9.33.0 --language rust --out ./sdks/gcp
//! pulumi config set gcp:project $(gcloud config get-value project)
//! pulumi config set gcp:zone us-central1-a
//! pulumi up
//! ```

use pulumi::{Output, PropertyValue};
use pulumi_gcp::{container, types};

/// How many nodes the managed pool runs when `nodeCount` is not configured.
const DEFAULT_NODE_COUNT: f64 = 2.0;

/// The machine type used when `machineType` is not configured. `e2-medium`
/// is the smallest shape that comfortably runs the GKE system pods.
const DEFAULT_MACHINE_TYPE: &str = "e2-medium";

/// The scope GKE nodes need to pull images, write logs, and report metrics.
/// The cloud-platform scope is what `gcloud container clusters create` grants
/// by default.
const NODE_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// The kubeconfig this program hands back, with `%s` marking each place a
/// cluster output is spliced in. There are eight holes, filled in order by
/// the CA certificate, the API server endpoint, and then the context name
/// six times over.
///
/// GKE tokens are not embedded here: `gke-gcloud-auth-plugin` mints one on
/// demand from whatever gcloud credentials the caller already has, which is
/// how a kubeconfig checked into a stack output stays usable across machines.
const KUBECONFIG_TEMPLATE: &str = r#"apiVersion: v1
clusters:
- cluster:
    certificate-authority-data: %s
    server: https://%s
  name: %s
contexts:
- context:
    cluster: %s
    user: %s
  name: %s
current-context: %s
kind: Config
preferences: {}
users:
- name: %s
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: gke-gcloud-auth-plugin
      installHint: Install gke-gcloud-auth-plugin for use with kubectl by following https://cloud.google.com/kubernetes-engine/docs/how-to/cluster-access-for-kubectl#install_plugin
      provideClusterInfo: true
"#;

fn main() {
    pulumi::run(|ctx| async move {
        // `pulumi config set nodeCount 3` to resize the pool.
        let node_count = ctx
            .config()
            .get_int_or("nodeCount", PropertyValue::Number(DEFAULT_NODE_COUNT));
        let machine_type = ctx.config().get_string_or(
            "machineType",
            PropertyValue::String(DEFAULT_MACHINE_TYPE.to_string()),
        );

        // `location` is among the optional ones: left unset it comes from the
        // provider's `gcp:zone` (or `gcp:region`, for a regional cluster).
        let cluster = container::Cluster::new(
            &ctx,
            "gke-cluster",
            container::ClusterArgs {
                // GKE insists on a node pool at creation time, so the usual
                // shape is to create the smallest possible default pool and
                // delete it in the same operation, leaving the `NodePool`
                // below as the only one this stack manages. Managing node
                // pools separately is what makes resizing them a normal
                // update rather than a cluster replacement.
                initial_node_count: Some(pulumi::pv::number(1.0).cast()),
                remove_default_node_pool: Some(pulumi::pv::bool(true).cast()),

                // The provider defaults this to `true`, which makes
                // `pulumi destroy` fail with "deletion_protection is set".
                // An example has to be tearable down.
                deletion_protection: Some(pulumi::pv::bool(false).cast()),

                description: Some(
                    pulumi::pv::string("GKE cluster deployed from Rust.").cast(),
                ),

                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Feeding the cluster's own outputs in — rather than restating the
        // zone — makes the engine order the two registrations and records the
        // dependency in state, so `pulumi destroy` removes the pool first.
        let node_pool = container::NodePool::new(
            &ctx,
            "gke-nodepool",
            container::NodePoolArgs {
                cluster: Some(cluster.name().cast()),
                location: Some(cluster.location().cast()),
                project: Some(cluster.project().cast()),
                node_count: Some(node_count.cast()),
                node_config: Some(types::ContainerNodePoolNodeConfigArgs {
                    machine_type: Some(machine_type.cast()),
                    oauth_scopes: Some(
                        pulumi::pv::array(vec![pulumi::pv::string(NODE_SCOPE)]).cast(),
                    ),
                    ..Default::default()
                }),
                // Both fields are written out because there are only two
                // and both matter.
                management: Some(types::ContainerNodePoolManagementArgs {
                    auto_repair: Some(pulumi::pv::bool(true).cast()),
                    auto_upgrade: Some(pulumi::pv::bool(true).cast()),
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The kubeconfig's context name. Matching what `gcloud container
        // clusters get-credentials` writes means a kubeconfig produced here
        // and one produced by gcloud name the same context.
        let context_name = pulumi::pv::concat(vec![
            cluster.project().cast(),
            pulumi::pv::string("_"),
            cluster.location().cast(),
            pulumi::pv::string("_"),
            cluster.name().cast(),
        ]);

        // `masterAuth` comes back as a single object, so the CA certificate
        // is one key lookup away. `Output::index` returns
        // `Output<PropertyValue>`, which is exactly what the interpolation
        // helper below wants.
        let ca_certificate = cluster.master_auth().index("clusterCaCertificate");

        let kubeconfig = interpolate(
            KUBECONFIG_TEMPLATE,
            vec![
                ca_certificate,
                cluster.endpoint().cast(),
                context_name.clone(),
                context_name.clone(),
                context_name.clone(),
                context_name.clone(),
                context_name.clone(),
                context_name.clone(),
            ],
        );

        ctx.export("clusterName", cluster.name().cast::<PropertyValue>());
        ctx.export("nodePoolName", node_pool.name().cast::<PropertyValue>());

        // A kubeconfig is a credential: anyone holding it can talk to the API
        // server as whoever ran `pulumi up`. `pv::secret` marks the value, so
        // the engine encrypts it in state and `pulumi stack output` prints
        // `[secret]` unless `--show-secrets` is passed.
        ctx.export("kubeconfig", pulumi::pv::secret(kubeconfig));

        Ok(())
    });
}

/// Build one string out of literal text and several outputs, splicing a value
/// into each `%s` in `template`.
///
/// A kubeconfig is a single document assembled from values that do not exist
/// yet, so `format!` cannot build it: the endpoint and the CA certificate are
/// `Output`s, and the string can only come together inside a combinator that
/// waits on them. That combinator is
///
/// ```text
/// pub fn concat(parts: Vec<Output<PropertyValue>>) -> Output<PropertyValue>
/// ```
///
/// in `sdk/rust/pulumi/src/pv.rs`, which wraps
/// `pulumi::output::concat(parts: Vec<Output<PropertyValue>>) -> Output<String>`.
/// It is the same machinery PCL's `${...}` interpolation compiles down to,
/// and it carries the three things that make outputs outputs: if any part is
/// unknown the whole string is unknown (so nothing is half-rendered during a
/// preview), if any part is secret the whole string is secret, and the
/// dependencies of every part end up on the result.
///
/// The alternative combinator, `pulumi::output::all(Vec<Output<PropertyValue>>)
/// -> Output<Vec<PropertyValue>>`, hands back an array to `map` over. It is
/// the wrong tool here: `all` keeps element-level secretness *inside* the
/// array as `PropertyValue::Secret` wrappers, so a `format!` over the
/// elements would render the wrapper rather than the certificate.
///
/// Panics if `template` has more `%s` holes than there are values.
fn interpolate(template: &str, values: Vec<Output<PropertyValue>>) -> Output<PropertyValue> {
    let mut values = values.into_iter();
    let mut parts: Vec<Output<PropertyValue>> = Vec::new();
    let mut chunks = template.split("%s").peekable();
    while let Some(chunk) = chunks.next() {
        parts.push(pulumi::pv::string(chunk));
        if chunks.peek().is_some() {
            parts.push(values.next().expect("interpolate: not enough values for template"));
        }
    }
    pulumi::pv::concat(parts)
}
