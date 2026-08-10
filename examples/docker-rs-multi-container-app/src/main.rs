//! Two containers and a network, on the Docker daemon you already have.
//!
//! The Rust port of
//! [`docker-ts-multi-container-app`](https://github.com/pulumi/examples/tree/master/docker-ts-multi-container-app):
//! a user-defined `docker:index:Network`, a Redis backend that is only
//! reachable on that network, and an nginx frontend published on a
//! configurable host port. Each container pairs with a
//! `docker:index:RemoteImage` that pulls its image first.
//!
//! Nothing here touches a cloud provider — the only requirement is a running
//! Docker daemon.
//!
//! Generate the SDK the program depends on, then run it:
//!
//! ```sh
//! pulumi package gen-sdk docker@5.1.0 --language rust --out ./sdks/docker
//! pulumi up
//! ```

use pulumi::{Output, PropertyValue};
use pulumi_docker::{types, Container, ContainerArgs, Network, NetworkArgs, RemoteImage,
    RemoteImageArgs};

/// Pinned tags rather than `:latest`, so a `pulumi up` months from now pulls
/// the same bytes it pulls today.
const BACKEND_IMAGE: &str = "redis:7.4-alpine";
const FRONTEND_IMAGE: &str = "nginx:1.27-alpine";

/// The name the backend answers to on the user-defined network. Docker's
/// embedded DNS resolves container aliases for every container attached to
/// the same non-default network, which is the entire reason the `Network`
/// exists: on the default bridge, `redis` would not resolve.
const BACKEND_ALIAS: &str = "redis";

/// Ports inside the containers. Only the frontend's is published to the host.
const BACKEND_PORT: f64 = 6379.0;
const FRONTEND_PORT: f64 = 80.0;

/// The host port used when `hostPort` is not configured.
const DEFAULT_HOST_PORT: f64 = 8080.0;

fn main() {
    pulumi::run(|ctx| async move {
        // `pulumi config set hostPort 9000` to publish somewhere else.
        let host_port = ctx
            .config()
            .get_int_or("hostPort", PropertyValue::Number(DEFAULT_HOST_PORT));

        // A user-defined bridge network.
        let network = Network::new(
            &ctx,
            "app-net",
            NetworkArgs {
                driver: Some(pulumi::pv::string("bridge").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // A `RemoteImage` is a pull, not a build: it makes the image present
        // on the daemon and hands back the digest it resolved to.
        let backend_image = RemoteImage::new(
            &ctx,
            "backend-image",
            RemoteImageArgs {
                name: Some(pulumi::pv::string(BACKEND_IMAGE).cast()),
                // Leave the pulled image in the local cache on destroy, so
                // tearing the stack down and bringing it back up does not
                // re-download it.
                keep_locally: Some(pulumi::pv::bool(true).cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        let frontend_image = RemoteImage::new(
            &ctx,
            "frontend-image",
            RemoteImageArgs {
                name: Some(pulumi::pv::string(FRONTEND_IMAGE).cast()),
                keep_locally: Some(pulumi::pv::bool(true).cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Redis. No `ports`, so nothing is published to the host: the only
        // way to it is from another container on `app-net`. Feeding the
        // image's `repo_digest` in rather than the tag pins the container to
        // the exact bytes that were pulled, and makes the engine order the
        // pull before the run.
        let backend = Container::new(
            &ctx,
            "backend",
            ContainerArgs {
                networks_advanced: Some(vec![network_attachment(
                    network.name().cast(),
                    Some(BACKEND_ALIAS),
                )]),

                image: Some(backend_image.repo_digest().cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // nginx, published on the host. `ContainerPortArgs` requires
        // `internal`, so that one is spelled out in full too.
        let frontend = Container::new(
            &ctx,
            "frontend",
            ContainerArgs {
                ports: Some(vec![types::ContainerPortArgs {
                    internal: Some(pulumi::pv::number(FRONTEND_PORT).cast()),
                    external: Some(host_port.clone().cast()),
                    ..Default::default()
                }]),
                networks_advanced: Some(vec![network_attachment(
                    network.name().cast(),
                    None,
                )]),
                // Where a real frontend would look for its backend. The
                // stock nginx image ignores this variable; it is here to show
                // the wiring, and the hostname in it is exactly the alias the
                // backend was attached under. `format!` is enough because
                // every part is ordinary local data — contrast the container
                // images above, which are `Output`s and have to be threaded
                // through the resource graph.
                envs: Some(
                    pulumi::pv::array(vec![pulumi::pv::string(format!(
                        "REDIS_URL=redis://{BACKEND_ALIAS}:{}",
                        BACKEND_PORT as i64
                    ))])
                    .cast(),
                ),

                image: Some(frontend_image.repo_digest().cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions {
                // Nothing in the frontend's inputs refers to the backend, so
                // without this the engine is free to start them in parallel.
                depends_on: vec![backend.pulumi_resource().clone()],
                // This container publishes a fixed host port, and changing
                // the image replaces the container rather than updating it
                // in place. Pulumi's default is to create the replacement
                // before deleting the original, which here means the new
                // container tries to bind a port the old one still holds:
                //
                //   Error starting userland proxy: listen tcp4 0.0.0.0:3000:
                //   bind: address already in use
                //
                // The first `pulumi up` is fine. The second one, after the
                // image changes, is what fails. Deleting first costs a few
                // seconds of downtime and is the only order that can work
                // while the host port is pinned.
                delete_before_replace: Some(true),
                ..Default::default()
            },
        );

        ctx.export("networkName", network.name().cast::<PropertyValue>());
        ctx.export("backendName", backend.name().cast::<PropertyValue>());
        ctx.export("frontendName", frontend.name().cast::<PropertyValue>());

        // The published address. `pv::concat` renders the port number the way
        // string interpolation does — `8080`, not `8080.0` — so the URL comes
        // out well-formed even though config numbers are `f64` underneath.
        ctx.export(
            "url",
            pulumi::pv::concat(vec![
                pulumi::pv::string("http://localhost:"),
                host_port,
            ]),
        );

        Ok(())
    });
}

/// Attach a container to a network, optionally under a DNS alias.
fn network_attachment(
    name: Output<std::string::String>,
    alias: Option<&str>,
) -> types::ContainerNetworksAdvancedArgs {
    types::ContainerNetworksAdvancedArgs {
        name: Some(name),
        aliases: alias.map(|a| pulumi::pv::array(vec![pulumi::pv::string(a)]).cast()),
        ..Default::default()
    }
}
