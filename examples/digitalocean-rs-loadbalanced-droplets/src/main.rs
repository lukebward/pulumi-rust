//! Load-balanced web servers on DigitalOcean.
//!
//! The Rust port of
//! [`digitalocean-ts-loadbalanced-droplets`](https://github.com/pulumi/examples/tree/master/digitalocean-ts-loadbalanced-droplets):
//! a configurable number of Droplets, each running nginx over a page that
//! reports the Droplet's own hostname, all carrying a shared Tag, with a
//! Load Balancer that selects its backends by that tag and forwards port 80.
//!
//! Generate the SDK the program depends on, then run it:
//!
//! ```sh
//! pulumi package gen-sdk digitalocean@4.78.1 --language rust --out ./sdks/digitalocean
//! pulumi config set dropletCount 3
//! pulumi up
//! ```

/// How many Droplets to create when `dropletCount` is not configured.
const DEFAULT_DROPLET_COUNT: usize = 3;

/// The DigitalOcean region used when `region` is not configured. Load
/// Balancers are regional, so every Droplet has to live in the same one.
const DEFAULT_REGION: &str = "nyc3";

/// The smallest shared-CPU Droplet slug — enough for nginx.
const DROPLET_SIZE: &str = "s-1vcpu-1gb";

/// Ubuntu 24.04 LTS. DigitalOcean distribution images are addressed by slug;
/// `doctl compute image list-distribution` lists the current set.
const IMAGE: &str = "ubuntu-24-04-x64";

/// Cloud-init script: install nginx and serve a page naming this Droplet, so
/// repeated requests through the Load Balancer visibly land on different
/// backends.
///
/// `DPkg::Lock::Timeout` is not decoration. Ubuntu runs `apt-daily` and
/// `unattended-upgrades` at boot, so a cloud-init script reaching for apt in
/// the first minute is racing them for the dpkg lock. Losing that race under
/// `set -e` aborts the script — and the failure is invisible from Pulumi's
/// side, because the Droplet resource itself created successfully. `pulumi
/// up` reports success, and the Load Balancer just never sees a healthy
/// backend. Waiting for the lock is the difference between an example that
/// works and one that fails silently.
const USER_DATA: &str = r#"#!/bin/bash
set -eux
export DEBIAN_FRONTEND=noninteractive
apt-get -o DPkg::Lock::Timeout=600 update
apt-get -o DPkg::Lock::Timeout=600 install -y nginx
echo "Hello from $(hostname)" > /var/www/html/index.html
systemctl enable --now nginx
"#;

fn main() {
    pulumi::run(|ctx| async move {
        // How many Droplets to create is *local* data, not an output: the
        // loop below decides how many resources the program registers at all,
        // which cannot wait on a value the engine has not produced yet.
        // `Config::get` hands back the raw string synchronously, which is
        // exactly what a `for` loop needs.
        let droplet_count = droplet_count(&ctx)?;

        // The region, by contrast, is only ever *passed* to a resource, so
        // the ordinary output-shaped getter is fine.
        // `pulumi config set region sfo3` to override.
        let region = ctx
            .config()
            .get_string_or("region", pulumi::PropertyValue::String(DEFAULT_REGION.into()));

        // The tag that ties the fleet together. Leaving the name unset hands
        // it to Pulumi's auto-naming, which suffixes the resource name with
        // random characters — DigitalOcean tag names are account-wide, so that
        // is what keeps two stacks from colliding.
        let web_tag = pulumi_digitalocean::Tag::new(
            &ctx,
            "web",
            pulumi_digitalocean::TagArgs::default(),
            pulumi::ResourceOptions::default(),
        );

        // One Droplet per iteration. Because `droplet_count` is a plain
        // `usize`, this is an ordinary Rust `for` loop — nothing about
        // creating N resources needs an output-shaped construct.
        let mut droplet_names: Vec<pulumi::Output<String>> = Vec::with_capacity(droplet_count);
        let mut droplet_resources: Vec<pulumi::Resource> = Vec::with_capacity(droplet_count);

        for i in 0..droplet_count {
            let droplet = pulumi_digitalocean::Droplet::new(
                &ctx,
                &format!("web-{i}"),
                pulumi_digitalocean::DropletArgs {
                    image: Some(pulumi::pv::string(IMAGE).cast()),
                    // `size` and `region` are unions in the schema (a plain
                    // string or one of the provider's slug enums), so they
                    // surface as `Output<PropertyValue>`. A bare `.cast()`
                    // works either way.
                    size: Some(pulumi::pv::string(DROPLET_SIZE).cast()),
                    region: Some(region.cast()),
                    // Changing this replaces the Droplet: cloud-init only
                    // runs on first boot, so the provider marks `userData`
                    // as forcing a new resource.
                    user_data: Some(pulumi::pv::string(USER_DATA).cast()),
                    // Feeding the tag's own output in here makes the engine
                    // create the tag first and records the dependency in
                    // state. A DigitalOcean tag is referenced by its name.
                    tags: Some(web_tag.name().map(|t: String| vec![t])),
                    ..Default::default()
                },
                pulumi::ResourceOptions::default(),
            );

            droplet_names.push(droplet.name());
            droplet_resources.push(droplet.pulumi_resource().clone());
        }

        let load_balancer = pulumi_digitalocean::LoadBalancer::new(
            &ctx,
            "web",
            pulumi_digitalocean::LoadBalancerArgs {
                region: Some(region.cast()),
                // Select backends by tag rather than by id. The alternative,
                // `droplet_ids`, is a list of *integers* in this schema while
                // `Droplet::id()` is a string, so the tag is both the simpler
                // and the better-typed edge here.
                droplet_tag: Some(web_tag.name().cast()),
                forwarding_rules: Some(vec![
                    pulumi_digitalocean::types::LoadBalancerForwardingRuleArgs {
                        entry_port: Some(pulumi::Output::known(80)),
                        entry_protocol: Some(pulumi::pv::string("http").cast()),
                        target_port: Some(pulumi::Output::known(80)),
                        target_protocol: Some(pulumi::pv::string("http").cast()),
                        ..Default::default()
                    },
                ]),
                // Likewise `LoadBalancerHealthcheckArgs`: `port` and
                // `protocol` are required. Without a health check the Load
                // Balancer would keep sending traffic to a Droplet whose
                // nginx has not finished installing.
                healthcheck: Some(pulumi_digitalocean::types::LoadBalancerHealthcheckArgs {
                    port: Some(pulumi::Output::known(80)),
                    protocol: Some(pulumi::pv::string("http").cast()),
                    path: Some(pulumi::pv::string("/").cast()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions {
                // Matching on a tag is a late binding: nothing in the Load
                // Balancer's inputs refers to a Droplet, so without this the
                // engine is free to create them in parallel and the Load
                // Balancer would come up with an empty backend pool.
                depends_on: droplet_resources,
                ..Default::default()
            },
        );

        ctx.export("loadBalancerIp", load_balancer.ip().cast::<pulumi::PropertyValue>());

        // Collapsing a `Vec<Output<..>>` into one output: `pulumi::pv::array`
        // takes `Vec<Output<PropertyValue>>` and returns a single
        // `Output<PropertyValue>` holding the array. It is a thin wrapper over
        // `pulumi::output::all`, whose signature is
        //
        //     pub fn all(outputs: Vec<Output<PropertyValue>>) -> Output<Vec<PropertyValue>>
        //
        // and which is what other Pulumi SDKs spell `pulumi.all([..])`. The
        // dependencies of every element carry into the combined output, so
        // the stack output waits for all of the Droplets.
        //
        // The elements here are `Output<String>`, so each one gets a bare
        // `.cast()` into the dynamic form the combinator takes.
        ctx.export(
            "dropletNames",
            pulumi::pv::array(droplet_names.iter().map(|name| name.cast()).collect()),
        );

        Ok(())
    });
}

/// How many Droplets to create: the `dropletCount` config value, or
/// [`DEFAULT_DROPLET_COUNT`].
///
/// `Config::get` returns `Option<String>` rather than an `Output`, which is
/// what makes the count usable as a loop bound. The typed getters
/// (`get_int_or` and friends) return `Output<PropertyValue>` instead, and an
/// output cannot decide how many resources a program registers.
fn droplet_count(ctx: &pulumi::Context) -> pulumi::Result<usize> {
    let raw = match ctx.config().get("dropletCount") {
        Some(raw) => raw,
        None => return Ok(DEFAULT_DROPLET_COUNT),
    };
    let count: usize = raw.trim().parse().map_err(|e| {
        pulumi::Error::new(format!("config key \"dropletCount\" is not a number: {e}"))
    })?;
    if count == 0 {
        return Err(pulumi::Error::new(
            "config key \"dropletCount\" must be at least 1",
        ));
    }
    Ok(count)
}
