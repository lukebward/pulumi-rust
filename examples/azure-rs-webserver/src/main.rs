//! A tiny HTTP server running on an Azure Linux virtual machine.
//!
//! The Rust port of
//! [`azure-ts-webserver`](https://github.com/pulumi/examples/tree/master/azure-ts-webserver)
//! and
//! [`azure-py-webserver`](https://github.com/pulumi/examples/tree/master/azure-py-webserver):
//! a resource group holding a virtual network with one subnet, a dynamically
//! allocated public IP, a network security group opening ports 80 and 22, a
//! NIC that binds those three together, and an Ubuntu VM whose cloud-init
//! `custom_data` serves a page on port 80.
//!
//! Generate the SDK the program depends on, then run it:
//!
//! ```sh
//! pulumi package gen-sdk azure-native@3.25.0 --language rust --out ./sdks/azure-native
//! pulumi config set azure-native:location WestUS2
//! pulumi config set username webmaster
//! pulumi config set --secret password '<a strong password>'
//! pulumi up
//! ```

use pulumi_azure_native::{compute, network, resources, types};

/// Cloud-init user data: write a page and serve the directory on port 80.
/// Azure hands `customData` to cloud-init, which runs a `#!`-prefixed
/// payload as root once the VM has booted.
const INIT_SCRIPT: &str = r#"#!/bin/bash
mkdir -p /var/www
echo "Hello, World from Pulumi!" > /var/www/index.html
cd /var/www
nohup python3 -m http.server 80 &
"#;

/// Ubuntu 22.04 LTS, generation 2. Marketplace images are identified by a
/// (publisher, offer, sku, version) tuple rather than a single id;
/// `az vm image list --publisher Canonical --all --output table` lists what
/// is currently available in a subscription.
const IMAGE_PUBLISHER: &str = "Canonical";
const IMAGE_OFFER: &str = "0001-com-ubuntu-server-jammy";
const IMAGE_SKU: &str = "22_04-lts-gen2";

/// One inbound "allow" rule for a single TCP port.
///
/// Building the rules in a helper keeps the shared shape in one place
/// instead of two.
///
/// `access`, `direction` and `protocol` are `string | enum` unions in the schema, so
/// they surface as `Output<PropertyValue>` rather than `Output<String>`;
/// `pulumi::pv::string(..).cast()` is how a dynamic value is spelled.
/// `priority` is optional in the schema but Azure rejects a rule without
/// one, and rules are evaluated lowest-priority-number first.
fn allow_inbound_tcp(name: &str, port: &str, priority: i32) -> types::NetworkSecurityRuleArgs {
    types::NetworkSecurityRuleArgs {
        name: Some(pulumi::Output::known(name.to_string())),
        priority: Some(pulumi::Output::known(priority)),
        access: Some(pulumi::pv::string("Allow").cast()),
        direction: Some(pulumi::pv::string("Inbound").cast()),
        protocol: Some(pulumi::pv::string("Tcp").cast()),
        // "*" is Azure's any-address / any-port wildcard.
        source_address_prefix: Some(pulumi::Output::known("*".to_string())),
        source_port_range: Some(pulumi::Output::known("*".to_string())),
        destination_address_prefix: Some(pulumi::Output::known("*".to_string())),
        destination_port_range: Some(pulumi::Output::known(port.to_string())),
        ..Default::default()
    }
}

fn main() {
    pulumi::run(|ctx| async move {
        let config = ctx.config();

        // The VM's local administrator. `username` is an ordinary config
        // value; `password` is wrapped in `pv::secret` so it is encrypted in
        // the state file and redacted in the CLI even if someone set it
        // without `--secret`.
        let username = config.require_string("username")?;
        let password = pulumi::pv::secret(config.require_string("password")?);

        // `pulumi config set vmSize Standard_B2s` to override. B1s is the
        // smallest burstable size that still runs Ubuntu comfortably.
        let vm_size = config.get_string_or(
            "vmSize",
            pulumi::PropertyValue::String("Standard_B1s".into()),
        );

        // Everything lands in one resource group.
        let resource_group = resources::ResourceGroup::new(
            &ctx,
            "server-rg",
            resources::ResourceGroupArgs::default(),
            pulumi::ResourceOptions::default(),
        );

        // The subnet is declared inline on the network rather than as a
        // separate `network:Subnet` resource; the two ways of expressing it
        // conflict, and the inline form is what the TypeScript and Python
        // versions of this example use.
        let virtual_network = network::VirtualNetwork::new(
            &ctx,
            "server-network",
            network::VirtualNetworkArgs {
                // Feeding the group's own output in here makes the engine
                // order the two registrations and records the dependency.
                resource_group_name: Some(resource_group.name()),
                address_space: Some(types::NetworkAddressSpaceArgs {
                    address_prefixes: Some(pulumi::Output::known(vec!["10.0.0.0/16".to_string()])),
                    ..Default::default()
                }),
                subnets: Some(vec![types::NetworkSubnetArgs {
                    name: Some(pulumi::Output::known("default".to_string())),
                    address_prefix: Some(pulumi::Output::known("10.0.1.0/24".to_string())),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The inline subnet has no resource of its own to take an id from,
        // so it is read back off the network. `subnets` is an optional list
        // of typed response structs, so pulling the first element's ARM id
        // out is ordinary Rust inside `map`.
        let subnet_id = virtual_network.subnets().map(|subnets| {
            subnets
                .unwrap_or_default()
                .into_iter()
                .next()
                .and_then(|s| s.id)
        });

        // A dynamically allocated address: Azure does not pick the actual IP
        // until the VM it is attached to boots, which is why the export at
        // the bottom of this program looks the address up after the fact
        // rather than reading `public_ip.ip_address()`.
        //
        // `publicIPAllocationMethod` snake-cases to
        // `public_ipallocation_method`: the generator does not insert a
        // separator between two runs of capitals, so `IPAllocation` folds to
        // `ipallocation`. The same rule gives `enable_ipforwarding` and
        // `private_ipallocation_method` below.
        let public_ip = network::PublicIPAddress::new(
            &ctx,
            "server-ip",
            network::PublicIPAddressArgs {
                resource_group_name: Some(resource_group.name()),
                public_ipallocation_method: Some(pulumi::pv::string("Dynamic").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Azure denies inbound traffic from the internet by default, so the
        // web port has to be opened explicitly. Port 22 is here as well so
        // the VM can be reached over SSH with the same credentials.
        let security_group = network::NetworkSecurityGroup::new(
            &ctx,
            "server-nsg",
            network::NetworkSecurityGroupArgs {
                resource_group_name: Some(resource_group.name()),
                security_rules: Some(vec![
                    allow_inbound_tcp("allow-http", "80", 1000),
                    allow_inbound_tcp("allow-ssh", "22", 1001),
                ]),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The NIC is what actually joins the VM to the subnet, the public IP
        // and the security group. Sub-resource references are expressed as
        // the full nested type carrying only an `id` — all of their inputs
        // are optional, so `..Default::default()` covers the rest.
        //
        // The nested type's generated name folds the schema module into the
        // type name, so `azure-native:network:NetworkInterfaceIPConfiguration`
        // becomes `types::NetworkNetworkInterfaceIPConfigurationArgs`.
        let network_interface = network::NetworkInterface::new(
            &ctx,
            "server-nic",
            network::NetworkInterfaceArgs {
                resource_group_name: Some(resource_group.name()),
                ip_configurations: Some(vec![types::NetworkNetworkInterfaceIPConfigurationArgs {
                    name: Some(pulumi::Output::known("webserveripcfg".to_string())),
                    subnet: Some(types::NetworkSubnetArgs {
                        id: Some(subnet_id.cast()),
                        ..Default::default()
                    }),
                    private_ipallocation_method: Some(pulumi::pv::string("Dynamic").cast()),
                    public_ipaddress: Some(types::NetworkPublicIPAddressArgs {
                        id: Some(public_ip.id()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
                network_security_group: Some(types::NetworkNetworkSecurityGroupArgs {
                    id: Some(security_group.id()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The VM itself. azure-native's `VirtualMachine` inputs are grouped
        // into four nested profiles, each of which is a generated args
        // struct rather than an untyped bag, so this program never needs
        // `pulumi::pv::object(..)`: the only dynamically typed fields it
        // touches are scalars (`vm_size`, `create_option`, the security-rule
        // fields above), which are `string | enum` unions in the schema and
        // take `pulumi::pv::string(..).cast()`.
        let vm = compute::VirtualMachine::new(
            &ctx,
            "server-vm",
            compute::VirtualMachineArgs {
                resource_group_name: Some(resource_group.name()),
                hardware_profile: Some(types::ComputeHardwareProfileArgs {
                    vm_size: Some(vm_size.cast()),
                    ..Default::default()
                }),
                os_profile: Some(types::ComputeOSProfileArgs {
                    computer_name: Some(pulumi::Output::known("webserver".to_string())),
                    admin_username: Some(username.cast()),
                    admin_password: Some(password.cast()),
                    // Azure expects `customData` base64-encoded.
                    custom_data: Some(
                        pulumi::pv::to_base64(pulumi::pv::string(INIT_SCRIPT)).cast(),
                    ),
                    linux_configuration: Some(types::ComputeLinuxConfigurationArgs {
                        // Password login is off by default on Linux images;
                        // this example has no SSH key to fall back on.
                        disable_password_authentication: Some(pulumi::Output::known(false)),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                network_profile: Some(types::ComputeNetworkProfileArgs {
                    network_interfaces: Some(vec![types::ComputeNetworkInterfaceReferenceArgs {
                        id: Some(network_interface.id()),
                        primary: Some(pulumi::Output::known(true)),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                storage_profile: Some(types::ComputeStorageProfileArgs {
                    image_reference: Some(types::ComputeImageReferenceArgs {
                        publisher: Some(pulumi::Output::known(IMAGE_PUBLISHER.to_string())),
                        offer: Some(pulumi::Output::known(IMAGE_OFFER.to_string())),
                        sku: Some(pulumi::Output::known(IMAGE_SKU.to_string())),
                        version: Some(pulumi::Output::known("latest".to_string())),
                        ..Default::default()
                    }),
                    // `OSDisk` is the other nested type with a required
                    // field (`createOption`), so it too names everything.
                    os_disk: Some(types::ComputeOSDiskArgs {
                        create_option: Some(pulumi::pv::string("FromImage").cast()),
                        name: Some(pulumi::Output::known("server-vm-osdisk".to_string())),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // A Dynamic public IP has no address until something is attached to
        // it and running, so `public_ip.ip_address()` is still empty when
        // the address resource itself finishes creating. Reading it back
        // with the `getPublicIPAddress` invoke, sequenced after the VM with
        // `depends_on`, is what the TypeScript and Python versions do with
        // `vm.id.apply(...)`. The invoke reports `unknown` during a preview.
        let looked_up = network::get_public_ipaddress(
            &ctx,
            network::GetPublicIPAddressArgs {
                resource_group_name: Some(resource_group.name()),
                public_ip_address_name: Some(public_ip.name()),
                ..Default::default()
            },
            pulumi::InvokeOptions {
                depends_on: vec![vm.pulumi_resource().clone()],
                ..Default::default()
            },
        );

        ctx.export(
            "publicIp",
            looked_up
                .map(|ip| ip.ip_address)
                .cast::<pulumi::PropertyValue>(),
        );
        ctx.export("vmName", vm.name().cast::<pulumi::PropertyValue>());

        Ok(())
    });
}
