[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/digitalocean-rs-loadbalanced-droplets)

# Load-Balanced Web Servers on DigitalOcean

A small fleet of DigitalOcean Droplets behind a Load Balancer. The program
creates `dropletCount` Droplets (three by default) of size `s-1vcpu-1gb`
running Ubuntu 24.04, each booting a `user_data` script that installs nginx
and serves a page naming that Droplet's own hostname. All of them carry a
shared `digitalocean:index:Tag`, and a `digitalocean:index:LoadBalancer`
picks its backends up by that tag, forwards port 80 to them, and health-checks
them over HTTP. The Load Balancer's IP address and the generated Droplet names
come back as stack outputs, so `curl`-ing the IP repeatedly shows requests
landing on different backends.

This is the Rust version of
[`digitalocean-ts-loadbalanced-droplets`](https://github.com/pulumi/examples/tree/master/digitalocean-ts-loadbalanced-droplets).

Two things it is worth reading `src/main.rs` for:

- **Creating N resources in a `for` loop.** How many Droplets to create is
  ordinary local data — it decides how many resources the program registers at
  all, which cannot wait on a value the engine has not produced yet — so the
  program reads it with `Config::get`, which returns `Option<String>`
  synchronously, and loops with a plain Rust `for`. Nothing output-shaped is
  involved.
- **Collecting a `Vec<Output<..>>` into one output.** The loop accumulates a
  `Vec<Output<String>>` of Droplet names; a bare `.cast()` on each takes it to
  the dynamic form, and `pulumi::pv::array` folds the lot into the single
  `Output<PropertyValue>` that `ctx.export` wants. That helper is a thin
  wrapper over `pulumi::output::all`:

  ```rust
  pub fn all(outputs: Vec<Output<PropertyValue>>) -> Output<Vec<PropertyValue>>
  ```

  which is this SDK's spelling of what the other Pulumi SDKs call
  `pulumi.all([..])`. Dependencies from every element carry into the combined
  output.

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

4. Get a DigitalOcean API token. In the
   [DigitalOcean control panel](https://cloud.digitalocean.com/account/api/tokens),
   go to **API → Tokens → Generate New Token**, give it **Write** scope (the
   provider creates and destroys resources), and copy the value — it is shown
   once. A [DigitalOcean account](https://www.digitalocean.com/) is needed
   first; Droplets and Load Balancers are billed by the hour, so tear the
   stack down when you are finished with it.

   Hand the token to the provider either as an environment variable:

   ```bash
   $ export DIGITALOCEAN_TOKEN=dop_v1_***
   ```

   or as encrypted stack configuration, once the stack exists:

   ```bash
   $ pulumi config set --secret digitalocean:token dop_v1_***
   ```

   The second form keeps the token in the stack's config file encrypted with
   the stack's secrets provider, which is the better option for a shared
   stack. See
   [the provider's installation & configuration docs](https://www.pulumi.com/registry/packages/digitalocean/installation-configuration/)
   for the full list of settings.

   The `doctl` CLI is not required, but it is handy for checking what got
   created; `doctl auth init` reads the same token.

**The DigitalOcean SDK is not checked in.** `Cargo.toml` points
`pulumi_digitalocean` at `./sdks/digitalocean/rust`, which does not exist until
you run the `pulumi package gen-sdk` command in step 3 below. The crate does
not build before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init dev
    ```

1.  Optionally pick a region and a fleet size. The defaults are `nyc3` and
    three Droplets; every Droplet and the Load Balancer share the region,
    because DigitalOcean Load Balancers are regional:

    ```bash
    $ pulumi config set region sfo3
    $ pulumi config set dropletCount 5
    ```

1.  Generate the DigitalOcean provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk digitalocean@4.78.1 --language rust --out ./sdks/digitalocean
    ```

    The version is pinned because the property names in `src/main.rs` were
    checked against that schema. Every generated args struct derives
    `Default`, so a provider version that adds an optional input will not
    break this program; one that renames or removes an input still will.

    The generated crate's own `Cargo.toml` declares `pulumi = "0.1"`, which is
    not published to crates.io yet, so repoint it at this repository's copy of
    the core SDK:

    ```toml
    # in ./sdks/digitalocean/rust/Cargo.toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue. Provisioning the Load Balancer
    takes a few minutes.

    ```bash
    $ pulumi up
    Updating (dev)

         Type                               Name                                       Status
     +   pulumi:pulumi:Stack                digitalocean-rs-loadbalanced-droplets-dev  created
     +   ├─ digitalocean:index:Tag          web                                        created
     +   ├─ digitalocean:index:Droplet      web-0                                      created
     +   ├─ digitalocean:index:Droplet      web-1                                      created
     +   ├─ digitalocean:index:Droplet      web-2                                      created
     +   └─ digitalocean:index:LoadBalancer web                                        created

    Outputs:
        dropletNames  : [
            [0]: "web-0-***"
            [1]: "web-1-***"
            [2]: "web-2-***"
        ]
        loadBalancerIp: "***"

    Resources:
        + 5 created

    Duration: ***
    ```

    The Droplet names carry a random suffix because the program leaves the
    `name` input unset and lets Pulumi auto-name them, which keeps two stacks
    in the same DigitalOcean account from colliding.

1.  The stack outputs are the Load Balancer's address and the names of the
    backends behind it:

    ```bash
    $ pulumi stack output
    Current stack outputs (2):
        OUTPUT          VALUE
        dropletNames    ["web-0-***","web-1-***","web-2-***"]
        loadBalancerIp  ***
    ```

1.  Curl the Load Balancer. It reports each Droplet's own hostname, so
    repeated requests show the traffic being spread across the fleet. Give the
    Droplets a minute after `pulumi up` returns: cloud-init still has to
    install nginx, and until the health check passes the Load Balancer has no
    backends to send to and answers `503`.

    ```bash
    $ for i in $(seq 5); do curl -sS "http://$(pulumi stack output loadBalancerIp)"; done
    Hello from web-1-***
    Hello from web-2-***
    Hello from web-0-***
    Hello from web-1-***
    Hello from web-2-***
    ```

1.  Scale the fleet by changing the config and running `pulumi up` again. New
    Droplets pick up the tag, and the Load Balancer notices them without being
    updated itself — tag membership is evaluated by DigitalOcean, not recorded
    in the Load Balancer's inputs:

    ```bash
    $ pulumi config set dropletCount 5
    $ pulumi up
     +   digitalocean:index:Droplet  web-3  created
     +   digitalocean:index:Droplet  web-4  created
    ```

1.  Clean up when you are done. Droplets and Load Balancers bill by the hour:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm dev
    ```

## Notes on the generated API

`pulumi package gen-sdk digitalocean` produces a `pulumi_digitalocean` crate.
Every member of this package lives in the schema's `index` module, and members
of `index` sit at the crate root with no module segment, so the paths are
short:

- Resources: `pulumi_digitalocean::Droplet`, `pulumi_digitalocean::Tag`,
  `pulumi_digitalocean::LoadBalancer`.
- Nested object types live in one flat `types` module:
  `pulumi_digitalocean::types::LoadBalancerForwardingRuleArgs`,
  `pulumi_digitalocean::types::LoadBalancerHealthcheckArgs`.

Two shapes in this program are worth calling out:

- **Unions surface as `PropertyValue`.** A Droplet's `size` and `region` are
  each declared in the schema as "a string, or one of the provider's slug
  enums". Anything the generator cannot narrow flows through the dynamic
  `pulumi::Output<pulumi::PropertyValue>`, which is why those two are built
  with `pulumi::pv::string(..).cast()` rather than `Output::known(..)`. A bare
  `.cast()` is the habit worth keeping generally: it infers, so it compiles
  whether the field ends up typed or dynamic.
- **`ipv6Address` folds to `ipv6address`.** The snake-case conversion treats a
  digit as ending a word without starting a new one, so the capital `A` does
  not get an underscore in front of it. The same rule turns AWS's
  `ipv6CidrBlocks` into `ipv6cidr_blocks`.

## Selecting backends by tag versus by id

The Load Balancer sets `droplet_tag` and leaves `droplet_ids` unset. Tags are
the better fit here for two reasons:

- Scaling the fleet does not touch the Load Balancer at all, as the `pulumi up`
  above shows.
- `dropletIds` is a list of **integers** in this schema, while
  `Droplet::id()`, like every Pulumi resource id, is a string. Wiring the ids
  in directly would mean parsing each one inside a `map` before collecting
  them.

The cost is that nothing in the Load Balancer's inputs mentions a Droplet, so
the engine would otherwise be free to create it first, with an empty backend
pool. `src/main.rs` therefore collects each `droplet.pulumi_resource().clone()`
into a `Vec<pulumi::Resource>` and passes it as `depends_on`.
