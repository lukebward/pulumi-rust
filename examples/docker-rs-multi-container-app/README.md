[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/docker-rs-multi-container-app)

# Multi-Container App on Docker

**This is the one example here that needs no cloud account.** There is no AWS,
Azure, or Google Cloud to sign up for and nothing to be billed for: if you have
a Docker daemon running locally, `pulumi up` works. It is the quickest way to
see the Rust language plugin drive a real provider end to end.

The program creates a user-defined `docker:index:Network`, a Redis backend
attached to it under the DNS alias `redis` and published nowhere, and an nginx
frontend attached to the same network and published on a configurable host
port — 8080 by default. Each container is paired with a
`docker:index:RemoteImage` that pulls its image first. The frontend's URL
comes back as a stack output.

This is the Rust version of
[`docker-ts-multi-container-app`](https://github.com/pulumi/examples/tree/master/docker-ts-multi-container-app).

## Prerequisites

1. [Install Pulumi](https://www.pulumi.com/docs/install/).
2. [Install Rust](https://rustup.rs/) (1.85 or newer) — `cargo` builds the
   program.
3. Build the experimental Rust language plugin from this repository and put
   it on your `PATH`, so that `runtime: rust` resolves:

   ```bash
   $ (cd ../../pulumi-language-rust && go build .)
   $ export PATH="$(cd ../../pulumi-language-rust && pwd):$PATH"
   ```

4. A running Docker daemon that your user can talk to:

   ```bash
   $ docker version --format '{{.Server.Version}}'
   27.3.1
   ```

   [Docker Desktop](https://docs.docker.com/desktop/), Colima, Rancher
   Desktop, or a plain `dockerd` on Linux all work. The provider talks to
   whatever `DOCKER_HOST` points at, falling back to the local socket; to
   target a different daemon, set `pulumi config set docker:host ...`.

   No credentials are needed. Both images come from Docker Hub and are
   public.

**The Docker SDK is not checked in.** `Cargo.toml` points `pulumi_docker` at
`./sdks/docker/rust`, which does not exist until you run the
`pulumi package gen-sdk` command in step 3 below. The crate does not build
before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init docker-dev
    ```

1.  Optionally pick a different host port (the default is `8080`):

    ```bash
    $ pulumi config set hostPort 9000
    ```

1.  Generate the Docker provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk docker@5.1.0 --language rust --out ./sdks/docker
    ```

    The `pulumi` crate is not published to crates.io yet, so edit the
    dependency in the generated `sdks/docker/rust/Cargo.toml` to point at
    this repository's copy of the core SDK:

    ```toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

    The version is pinned because the property names in `src/main.rs` were
    checked against that schema. Every generated args struct derives
    `Default`, so a provider version that adds an optional input will not
    break this program; one that renames or removes an input still will.

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue. The first run pulls both
    images, so it takes as long as the download does.

    ```bash
    $ pulumi up
    Updating (docker-dev)

         Type                        Name             Status
     +   pulumi:pulumi:Stack         docker-rs-multi-container-app-docker-dev  created
     +   ├─ docker:index:Network     app-net          created
     +   ├─ docker:index:RemoteImage backend-image    created
     +   ├─ docker:index:RemoteImage frontend-image   created
     +   ├─ docker:index:Container   backend          created
     +   └─ docker:index:Container   frontend         created

    Outputs:
        backendName:  "backend-***"
        frontendName: "frontend-***"
        networkName:  "app-net-***"
        url:          "http://localhost:8080"

    Resources:
        + 6 created

    Duration: ***
    ```

1.  Hit the frontend:

    ```bash
    $ curl -sS $(pulumi stack output url) | head -4
    <!DOCTYPE html>
    <html>
    <head>
    <title>Welcome to nginx!</title>
    ```

1.  Check that the two containers really share a network, and that the
    backend answers to `redis` on it. The backend publishes no ports, so
    this only works from inside the network:

    ```bash
    $ docker network inspect $(pulumi stack output networkName) \
        --format '{{range .Containers}}{{.Name}} {{end}}'
    backend-*** frontend-***

    $ docker exec $(pulumi stack output frontendName) nslookup redis
    Name:      redis
    Address 1: 172.18.0.2 redis
    ```

    Redis answers, but only from inside the network — nothing is published
    to the host, so `docker port` prints nothing for it:

    ```bash
    $ docker exec $(pulumi stack output backendName) redis-cli ping
    PONG
    $ docker port $(pulumi stack output backendName)
    $ docker port $(pulumi stack output frontendName)
    80/tcp -> 0.0.0.0:8080
    ```

1.  Move the frontend to another port and run `pulumi up` again. Changing a
    published port replaces the container — Docker cannot re-map a running
    one — but the network and the pulled images are untouched:

    ```bash
    $ pulumi config set hostPort 9000
    $ pulumi up
        +- docker:index:Container  frontend  replaced

    $ curl -sSI $(pulumi stack output url) | head -1
    HTTP/1.1 200 OK
    ```

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm docker-dev
    ```

    `keep_locally` is `true` on both images, so `pulumi destroy` removes the
    containers and the network but leaves the pulled images in the local
    cache. Bringing the stack back up does not re-download them. Drop that
    flag if you would rather have `destroy` reclaim the disk.

## Notes on the generated API

`pulumi package gen-sdk docker` produces a `pulumi_docker` crate. Everything
in the Docker schema lives in the `index` module, and index-module members sit
at the crate root with no module segment, so the resources are
`pulumi_docker::Network`, `pulumi_docker::RemoteImage`, and
`pulumi_docker::Container`. Nested object types still go in the flat `types`
module: `pulumi_docker::types::ContainerPortArgs`,
`pulumi_docker::types::ContainerNetworksAdvancedArgs`.

Two details of that generated API shaped how `src/main.rs` is written:

- **`ContainerArgs` has about seventy inputs**, and the program sets four
  of them. Every generated args struct derives `Default`, so the rest are
  elided with `..Default::default()` rather than written out.
- **`ipv4Address` becomes `ipv4address`, not `ipv4_address`.** The
  generator's `snakeCase` inserts a separator before an uppercase letter only
  when the previous character was lowercase, and here it is a digit. The same
  goes for `ipv6Address`. Both appear in
  `ContainerNetworksAdvancedArgs`, which this program has to name in full.

## How the two containers find each other

Docker's embedded DNS server resolves container names and aliases, but *only*
on user-defined networks — on the default `bridge` network it does not, which
is why the `Network` resource exists at all rather than the containers just
sharing the default.

The backend is attached with `aliases: ["redis"]`, so anything else on
`app-net` can reach it at `redis:6379`. The frontend gets the resulting URL in
a `REDIS_URL` environment variable. The stock nginx image ignores that
variable — it is there to show the wiring a real frontend would use.

The frontend also carries an explicit `depends_on` for the backend. Nothing in
the frontend's inputs refers to the backend, so without it the engine is free
to start the two in parallel, and a real frontend could come up before the
thing it connects to exists.
