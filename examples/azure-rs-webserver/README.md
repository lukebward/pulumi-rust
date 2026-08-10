[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/azure-rs-webserver)

# Web Server Using an Azure Virtual Machine

Starts a tiny HTTP server on a single Azure Linux VM. The program creates a
resource group, a virtual network with one subnet, a Standard-SKU static
public IP, a network security group that opens ports 80 and 22, and a network
interface binding those three to an Ubuntu 22.04 VM. The VM's `os_profile`
takes an administrator username and password from stack configuration and
hands cloud-init a `custom_data` script that serves a "Hello, World from
Pulumi!" page on port 80. The VM size is configurable, and the public IP
address and VM name come back as stack outputs.

This is the Rust version of
[`azure-ts-webserver`](https://github.com/pulumi/examples/tree/master/azure-ts-webserver)
and
[`azure-py-webserver`](https://github.com/pulumi/examples/tree/master/azure-py-webserver).

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

4. [Configure Azure credentials](https://www.pulumi.com/registry/packages/azure-native/installation-configuration/),
   most simply by running `az login` and selecting a subscription with
   `az account set --subscription <id>`.

**The azure-native SDK is not checked in.** `Cargo.toml` points
`pulumi_azure_native` at `./sdks/azure-native/rust`, which does not exist
until you run the `pulumi package gen-sdk` command in step 4 below. The crate
does not build before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init dev
    ```

1.  Set the Azure region. The resource group and everything in it inherit
    it, so nothing in `src/main.rs` hard-codes a location:

    ```bash
    $ pulumi config set azure-native:location WestUS2
    ```

1.  Set the VM's administrator credentials. Azure requires the password to
    be 12–123 characters and to use three of the four character classes
    (lowercase, uppercase, digit, symbol); it also rejects a handful of
    reserved usernames, `admin` and `root` among them:

    ```bash
    $ pulumi config set username webmaster
    $ pulumi config set --secret password '<a strong password>'
    ```

    Optionally pick a different VM size (the default is `Standard_B1s`):

    ```bash
    $ pulumi config set vmSize Standard_B2s
    ```

1.  Generate the azure-native provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk azure-native@3.25.0 --language rust --out ./sdks/azure-native
    ```

    `gen-sdk` writes to `<out>/<language>`, so the crate lands in
    `./sdks/azure-native/rust` — which is where `Cargo.toml` already points.
    The generated crate's own `Cargo.toml` declares `pulumi = "0.1"`, which
    is not published to crates.io yet, so repoint it at this repository's
    copy of the core SDK:

    ```toml
    # in ./sdks/azure-native/rust/Cargo.toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

    The version is pinned because the property names in `src/main.rs` were
    checked against that schema. Every generated args struct derives
    `Default`, so a provider version that adds an optional input will not
    break this program; one that renames or removes an input still will.

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue.

    ```bash
    $ pulumi up
    Updating (dev)

         Type                                          Name                    Status
     +   pulumi:pulumi:Stack                           azure-rs-webserver-dev  created
     +   ├─ azure-native:resources:ResourceGroup       server-rg               created
     +   ├─ azure-native:network:VirtualNetwork        server-network          created
     +   ├─ azure-native:network:PublicIPAddress       server-ip               created
     +   ├─ azure-native:network:NetworkSecurityGroup  server-nsg              created
     +   ├─ azure-native:network:NetworkInterface      server-nic              created
     +   └─ azure-native:compute:VirtualMachine        server-vm               created

    Outputs:
        publicIp: "***"
        vmName:   "server-vm***"

    Resources:
        + 7 created

    Duration: ***
    ```

1.  Check that the server is up. It takes a minute or two after the VM
    reaches `running` for cloud-init to install the page and start the
    listener:

    ```bash
    $ curl http://$(pulumi stack output publicIp)
    Hello, World from Pulumi!
    ```

    The same credentials work over SSH, since the security group opens
    port 22 as well:

    ```bash
    $ ssh $(pulumi config get username)@$(pulumi stack output publicIp)
    ```

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm dev
    ```

## The public IP is Standard and Static, and why that matters

`server-ip` sets `sku.name = "Standard"` and
`publicIPAllocationMethod = "Static"`. Neither is a preference:

```rust
sku: Some(types::NetworkPublicIPAddressSkuArgs {
    name: Some(pulumi::pv::string("Standard")),
    ..Default::default()
}),
public_ip_allocation_method: Some(pulumi::pv::string("Static").cast()),
```

Leaving `sku` unset asks for the **Basic** SKU. Azure stopped allowing new
Basic public IP addresses on **31 March 2025** and retired the SKU on
**30 September 2025**, so the older shape of this example — no `sku`, and
`"Dynamic"` allocation, which only Basic supports — cannot be deployed at
all any more. It fails on `server-ip`, before the VM is ever reached.

Two consequences worth knowing:

- **Standard is deny-by-default for inbound traffic.** `server-nsg` is not
  belt-and-braces; without it the VM is unreachable on 80 and 22 even though
  it has a public address.
- **A Static address is assigned when the address resource is created**,
  not when a VM attaches to it, so `publicIp` is exported by reading
  `public_ip.ip_address()` directly.

That second point used to be the interesting part of this example. A
*Dynamic* address has no value until a running VM claims it through a NIC,
so the TypeScript and Python versions call `vm.id.apply(...)` and look the
address up afterwards, and this program did the same with an invoke
sequenced behind the VM by `InvokeOptions::depends_on`. Static allocation
removes the problem, and the indirection went with it.

## Notes on the generated API

`pulumi package gen-sdk azure-native` produces a `pulumi_azure_native` crate
whose layout follows the package's schema modules:

- Resources live under their module: `resources::ResourceGroup`,
  `network::VirtualNetwork`, `compute::VirtualMachine`.
- Invokes are free functions taking `(&ctx, args, InvokeOptions)`:
  `network::get_public_ip_address`.
- Nested object types live in one flat `types` module, with the module name
  folded into the type name — so `azure-native:compute:OSProfile` becomes
  `types::ComputeOSProfileArgs` and
  `azure-native:network:NetworkInterfaceIPConfiguration` becomes the
  double-barrelled `types::NetworkNetworkInterfaceIPConfigurationArgs`.

Three things about this provider are worth knowing before reading
`src/main.rs`:

- **A run of capitals is one word, until a lowercase letter ends it.**
  `publicIPAllocationMethod` becomes `public_ip_allocation_method`,
  `enableIPForwarding` becomes `enable_ip_forwarding`, and `privateIPAddress`
  becomes `private_ip_address` — the last capital of each run starts the word
  that follows. `publicIpAddressName`, spelled with a lowercase `p` in the
  schema, lands on `public_ip_address_name` too. Both spellings appear in
  `PublicIPAddressArgs`.
- **`Default` is derived only for all-optional structs.** Every resource here
  requires `resourceGroupName`, so every resource args literal is written out
  in full. Most of the nested profile types are all-optional and take
  `..Default::default()`; the two exceptions are
  `types::NetworkSecurityRuleArgs` (`access`, `direction` and `protocol` are
  required) and `types::ComputeOSDiskArgs` (`createOption`), which name every
  field. The security rules are built by a small helper so that list is
  written once rather than twice.
- **Enum-valued inputs arrive as dynamic values.** azure-native declares them
  as a union of `string` and the enum type, which the generator renders as
  `Output<PropertyValue>` — hence `pulumi::pv::string("Static").cast()` for
  `public_ip_allocation_method`, `"FromImage"` for `create_option`, and so on.
  Everything else in the program is strongly typed: the VM's four nested
  profiles are generated args structs, so this example never needs
  `pulumi::pv::object(vec![...]).cast()`.

## Notes on the deployment itself

- The subnet is declared inline on the `VirtualNetwork` rather than as a
  separate `network:Subnet` resource, matching the other language versions.
  The two forms conflict — Azure will fight over the network's subnet list if
  both are used — so the NIC reads the subnet's id back off
  `virtual_network.subnets()` instead of from a resource of its own.
- The image is a marketplace `(publisher, offer, sku, version)` tuple rather
  than an id, pinned to Ubuntu 22.04 LTS gen 2. `az vm image list --publisher
  Canonical --all --output table` lists what a subscription can currently
  see; the upstream examples still name `UbuntuServer` `16.04-LTS`, which
  Canonical has withdrawn.
- `custom_data` must be base64-encoded, which `pulumi::pv::to_base64` does
  for the cloud-init script.
- Password authentication is on because `disable_password_authentication` is
  set to `false`; Linux images default it to `true`. A real deployment would
  keep that default and supply an SSH public key through
  `linux_configuration.ssh` instead.
