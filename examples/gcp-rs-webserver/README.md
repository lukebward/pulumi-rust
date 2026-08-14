[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/pulumi-labs/pulumi-rust/tree/main/examples/gcp-rs-webserver)

# Web Server Using Google Compute Engine

Starts a tiny HTTP server on a single Compute Engine VM. The program creates a
VPC network of its own, opens TCP 22 and 80 on it with a `gcp:compute:Firewall`
rule, and boots a Debian instance whose `metadata_startup_script` serves a
"Hello, World from Pulumi!" page on port 80. The machine type and the zone are
configurable, and the instance's name and its external IP come back as stack
outputs.

The external IP is the interesting part: an ephemeral address is not a
top-level property of the instance, it sits at
`networkInterfaces[0].accessConfigs[0].natIp` in the instance's state. Reaching
it means walking down through a list, an object, and another list of a resource
output — `src/main.rs` does that with `Output::index`, and the
[traversing nested outputs](#traversing-nested-outputs) section below explains
the call.

This is the Rust version of
[`gcp-py-webserver`](https://github.com/pulumi/examples/tree/master/gcp-py-webserver)
and [`gcp-go-webserver`](https://github.com/pulumi/examples/tree/master/gcp-go-webserver).

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

4. [Configure Google Cloud credentials](https://www.pulumi.com/registry/packages/gcp/installation-configuration/),
   for example with `gcloud auth application-default login`.
5. Enable the Compute Engine API on your project:

   ```bash
   $ gcloud services enable compute.googleapis.com
   ```

**The GCP SDK is not checked in.** `Cargo.toml` points `pulumi_gcp` at
`./sdks/gcp/rust`, which does not exist until you run the `pulumi package gen-sdk`
command in step 4 below. The crate does not build before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init webserver-dev
    ```

1.  Set the GCP project. This one has no default — the provider needs it and
    fails without it:

    ```bash
    $ pulumi config set gcp:project $(gcloud config get-value project)
    ```

    `gcp:region` is not needed here: the network and the firewall rule are
    global resources, and the instance is placed by its zone.

1.  Optionally pick a different zone or machine type. The defaults are
    `us-central1-a` and `e2-micro`; these are the program's own config keys,
    not the provider's, so they carry no `gcp:` prefix:

    ```bash
    $ pulumi config set zone us-east1-b
    $ pulumi config set machineType e2-small
    ```

1.  Generate the GCP provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk gcp@9.33.0 --language rust --out ./sdks/gcp
    ```

    The version is pinned because the property names in `src/main.rs` were
    checked against that schema. Every generated args struct derives
    `Default`, so a provider version that adds an optional input will not
    break this program; one that renames or removes an input still will.

    `gen-sdk` writes to `<out>/<language>`, so the crate lands in
    `./sdks/gcp/rust` — which is the path `Cargo.toml` depends on. The
    `pulumi` crate is not published to crates.io yet, and the generated
    manifest asks for it by version, so edit `sdks/gcp/rust/Cargo.toml` to
    point at this repository's copy of the core SDK instead:

    ```toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue.

    ```bash
    $ pulumi up
    Updating (webserver-dev)

         Type                      Name                            Status
     +   pulumi:pulumi:Stack       gcp-rs-webserver-webserver-dev  created
     +   ├─ gcp:compute:Network    webserver-network               created
     +   ├─ gcp:compute:Firewall   webserver-firewall              created
     +   └─ gcp:compute:Instance   webserver                       created

    Outputs:
        instanceName: "webserver-***"
        publicIp:     "34.***.***.***"
        url:          "http://34.***.***.***"

    Resources:
        + 4 created

    Duration: ***
    ```

1.  The stack outputs name the instance and its external address:

    ```bash
    $ pulumi stack output
    Current stack outputs (3):
        OUTPUT        VALUE
        instanceName  webserver-***
        publicIp      34.***.***.***
        url           http://34.***.***.***
    ```

1.  Check that the server is up. The instance reports `RUNNING` before the
    startup script has finished, so give it a few seconds:

    ```bash
    $ curl -sS $(pulumi stack output url)
    Hello, World from Pulumi!
    ```

    `gcloud` can confirm what actually got created, and that the address it
    reports is the one the traversal in `src/main.rs` pulled out:

    ```bash
    $ gcloud compute instances describe $(pulumi stack output instanceName) \
        --zone $(pulumi config get zone) \
        --format 'value(machineType.basename(), status, networkInterfaces[0].accessConfigs[0].natIP)'
    e2-micro  RUNNING  34.***.***.***
    ```

1.  Edit `STARTUP_SCRIPT` in `src/main.rs` and run `pulumi up` again. Changing
    the startup script replaces the instance, so it comes back with a new
    external address — `publicIp` and `url` update to match.

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm webserver-dev
    ```

## Traversing nested outputs

The instance's external IP only exists after GCE has assigned one, and it is
reported as part of the instance's network interfaces rather than as a property
of its own. In the schema that is a list of objects, each holding a list of
access-config objects, each holding a `natIp`:

```
networkInterfaces[0].accessConfigs[0].natIp
```

`Output::index` walks that path one step at a time:

```rust
let public_ip = server
    .network_interfaces()
    .cast::<pulumi::PropertyValue>()
    .index(0usize)
    .index("accessConfigs")
    .index(0usize)
    .index("natIp");
```

Four things are worth calling out:

- `index` is defined on every `Output<T>` and returns an
  `Output<pulumi::PropertyValue>`, so the calls chain. It takes anything that
  converts into a `pulumi::PropIndex`: a `&str` for an object key, a `usize`
  for a list position. The `usize` suffix on `0usize` matters — a bare `0`
  is an `i32` and does not convert.
- The `.cast::<pulumi::PropertyValue>()` first drops the accessor's static
  element type. `network_interfaces()` is typed
  `Output<Vec<ComputeInstanceNetworkInterface>>`, and that type stops
  describing the value as soon as the traversal steps inside it.
- The keys are the *schema's* property names, so they are camelCase —
  `accessConfigs`, `natIp` — even though the corresponding fields on the Rust
  args structs are `access_configs` and `nat_ip`. Indexing reads the dynamic
  value the engine sent back, not a Rust struct.
- Unknown values, secretness, and resource dependencies propagate through
  every step. During `pulumi preview` the whole chain is simply unknown
  instead of erroring, and the export still records its dependency on the
  instance.

## A note on the `default` network

The program creates its own `gcp:compute:Network` rather than using the
project's `default` one. A firewall rule opens ports on whatever network it
names, so attaching this rule to `default` would open SSH and HTTP to every
other VM in the project that sits on it. The cost is a second network per
stack; `pulumi destroy` removes it along with everything else.

The rule sets no `target_tags`, so it applies to every instance on this
network — which is only ever the one this program creates. Narrowing it means
setting `target_tags` on the firewall and the matching `tags` on the instance.

## A note on public access

`source_ranges = ["0.0.0.0/0"]` is what makes the `curl` step above work from
anywhere, and it also exposes port 22 to the internet. Organizations with a
constraint against open ingress reject the rule and `pulumi up` fails on the
`webserver-firewall` resource. Where that applies, narrow `source_ranges` to
your own address, or drop port 22 from `ports` and reach the VM with
`gcloud compute ssh`, which tunnels through IAP.

A second constraint bites before that one. The instance asks for an
ephemeral external IP — that is what the empty `access_configs` entry means —
and `constraints/compute.vmExternalIpAccess` denies external IPs to any VM
not on its allow-list. It is applied by default to organizations created on
or after 3 May 2024, so on a recent account this fails on the instance
itself, not on the firewall:

```
Constraint constraints/compute.vmExternalIpAccess violated for project ***.
Add instance projects/***/zones/***/instances/webserver to the constraint to
use external IP with it.
```

There is no provider-side input that overrides an organization policy. Either
allow-list the instance, or drop `access_configs` and reach the VM over IAP,
which needs no external address at all.
