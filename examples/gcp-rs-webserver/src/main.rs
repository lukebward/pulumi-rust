//! A tiny HTTP server running on a Google Compute Engine instance.
//!
//! The program creates a VPC network of its own, opens TCP 22 and 80 on it
//! with a firewall rule, and boots a Debian VM whose startup script serves a
//! page on port 80. The instance's name and the ephemeral external address
//! GCE handed its network interface come back as stack outputs.
//!
//! The program depends on a generated GCP SDK, so generate that first:
//!
//! ```sh
//! pulumi package gen-sdk gcp@9.33.0 --language rust --out ./sdks/gcp
//! pulumi up
//! ```

/// Startup script for the VM. GCE runs the value of the `startup-script`
/// metadata key as root on every boot, and Debian's images already ship
/// `python3`, so no package installation is needed.
const STARTUP_SCRIPT: &str = r#"#!/bin/bash
mkdir -p /var/www
cd /var/www
echo "Hello, World from Pulumi!" > index.html
nohup python3 -m http.server 80 &
"#;

/// The boot image. `<project>/<family>` names an image *family* rather than
/// a dated image, so a rebuild picks up the newest Debian 12 image instead
/// of one pinned in this file.
const BOOT_IMAGE: &str = "debian-cloud/debian-12";

fn main() {
    pulumi::run(|ctx| async move {
        // `pulumi config set zone us-east1-b` and
        // `pulumi config set machineType e2-small` override these. The one
        // piece of configuration with no default is `gcp:project`, which the
        // provider requires; see the README.
        let zone = ctx.config().get_string_or(
            "zone",
            pulumi::PropertyValue::String("us-central1-a".into()),
        );
        let machine_type = ctx.config().get_string_or(
            "machineType",
            pulumi::PropertyValue::String("e2-micro".into()),
        );

        // A network of this stack's own rather than the project's `default`
        // network: the firewall rule below opens ports on whatever network it
        // names, and doing that to `default` would affect every other VM in
        // the project.
        let network = pulumi_gcp::compute::Network::new(
            &ctx,
            "webserver-network",
            pulumi_gcp::compute::NetworkArgs {
                // Auto mode creates one subnet per region, which is what
                // gives the instance an address in whichever zone it lands
                // in without this program declaring a subnet itself.
                auto_create_subnetworks: Some(pulumi::pv::bool(true).cast()),
                description: Some(
                    pulumi::pv::string("Network for the Pulumi Rust webserver example").cast(),
                ),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Open SSH and HTTP to the world.
        let firewall = pulumi_gcp::compute::Firewall::new(
            &ctx,
            "webserver-firewall",
            pulumi_gcp::compute::FirewallArgs {
                // Passing the network's own output here makes the engine
                // create the network first and records the dependency in
                // state.
                network: Some(network.self_link().cast()),
                // `ComputeFirewallAllowArgs` has a required `protocol`, so
                // it too spells out all of its fields.
                allows: Some(vec![pulumi_gcp::types::ComputeFirewallAllowArgs {
                    protocol: Some(pulumi::pv::string("tcp").cast()),
                    ports: Some(pulumi::Output::known(vec![
                        "22".to_string(),
                        "80".to_string(),
                    ])),
                    ..Default::default()
                }]),
                source_ranges: Some(pulumi::Output::known(vec!["0.0.0.0/0".to_string()])),
                description: Some(pulumi::pv::string("Allow SSH and HTTP from anywhere").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The VM.
        let server = pulumi_gcp::compute::Instance::new(
            &ctx,
            "webserver",
            pulumi_gcp::compute::InstanceArgs {
                machine_type: Some(machine_type.cast()),
                zone: Some(zone.cast()),
                metadata_startup_script: Some(pulumi::pv::string(STARTUP_SCRIPT).cast()),
                description: Some(
                    pulumi::pv::string("Pulumi Rust webserver example").cast(),
                ),
                // Changing `machineType` on an existing instance needs the
                // VM stopped; without this the update fails instead.
                allow_stopping_for_update: Some(pulumi::pv::bool(true).cast()),

                boot_disk: Some(pulumi_gcp::types::ComputeInstanceBootDiskArgs {
                    initialize_params: Some(
                        pulumi_gcp::types::ComputeInstanceBootDiskInitializeParamsArgs {
                            image: Some(pulumi::pv::string(BOOT_IMAGE).cast()),
                            ..Default::default()
                        },
                    ),
                    ..Default::default()
                }),

                network_interfaces: Some(vec![
                    pulumi_gcp::types::ComputeInstanceNetworkInterfaceArgs {
                        network: Some(network.self_link().cast()),
                        // One access config with nothing set in it asks GCE
                        // for an ephemeral external address. Setting
                        // `nat_ip` here instead would attach a reserved
                        // `compute:Address`; leaving it unset is what makes
                        // the address something only the *outputs* know,
                        // which is the point of the traversal below.
                        access_configs: Some(vec![
                            pulumi_gcp::types::ComputeInstanceNetworkInterfaceAccessConfigArgs::default(),
                        ]),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            pulumi::ResourceOptions {
                // The firewall rule is not an input to the instance, so
                // nothing in the graph orders them. Without this the VM can
                // finish booting before the rule exists and the first
                // request to port 80 hangs.
                depends_on: vec![firewall.pulumi_resource().clone()],
                ..Default::default()
            },
        );

        // The external address is not a top-level output of the instance:
        // it lives at `networkInterfaces[0].accessConfigs[0].natIp` in the
        // resource's state, three levels down through a list, an object, and
        // another list.
        //
        // `Output::index` navigates that. It is defined for every
        // `Output<T>` and returns an `Output<PropertyValue>`, so the calls
        // chain: it takes anything that converts into a `PropIndex`, which
        // is a `&str` for an object key and a `usize` for a list position —
        // hence `0usize` rather than a bare `0`, which would be an `i32`.
        // Casting to `PropertyValue` first drops the accessor's static
        // element type, which no longer describes the value once the
        // traversal starts.
        //
        // The keys are the schema's own property names, so they are
        // camelCase here even though the corresponding Rust fields are
        // snake_case: this is indexing into the dynamic value the engine
        // returned, not into a Rust struct. Unknown-ness, secretness, and
        // resource dependencies all carry through each step, so during a
        // preview this stays unknown rather than failing.
        let public_ip = server
            .network_interfaces()
            .cast::<pulumi::PropertyValue>()
            .index(0usize)
            .index("accessConfigs")
            .index(0usize)
            .index("natIp");

        ctx.export(
            "instanceName",
            server.name().cast::<pulumi::PropertyValue>(),
        );
        ctx.export("publicIp", public_ip.clone());
        ctx.export(
            "url",
            pulumi::pv::concat(vec![pulumi::pv::string("http://"), public_ip]),
        );

        Ok(())
    });
}
