# pulumi-rust

An experimental Pulumi language implementation for **Rust**: SDK code
generation, program generation, a language host plugin, and a core Rust
runtime SDK — validated against Pulumi's official [language conformance
test suite](https://github.com/pulumi/pulumi/tree/master/pkg/testing/pulumi-test-language).

> Status: experimental. Built as an exploration of what a conformance-tested
> Rust language implementation looks like. Not an official Pulumi project.

**Conformance status** (pulumi/pulumi v3.256.0 suite): **all 179 tests
pass**, with no skips.

(179 is the whole suite: `LanguageTests` registers 180 entries, but the
harness never hands out the one `internal-` test.)

That covers the full `l1-*` output/config/builtin set including `try`/`can`
and `recover`; `l2` resources, invokes, resource methods (`call`), every
resource option, secrets, assets, reads, resource-reference hydration,
lifecycle hooks, package parameterization and non-UTF8 byte strings; `l3`
for/splat programs, the `range` option, local components and deferred
outputs; and the `policy-*` and `provider-*` families, which require the
language to author policy packs and component providers.

## What's here

| Component | Path | Description |
|---|---|---|
| Core SDK | `sdk/rust/pulumi` | The `pulumi` crate: gRPC resource-monitor client (tonic), `Output<T>` with unknown/secret/dependency propagation, property-value wire encoding, resource registration, invokes, config, stack lifecycle |
| Language host | `pulumi-language-rust` | Go gRPC plugin implementing Pulumi's `LanguageRuntime` interface: `Run` (cargo), `Pack`, `InstallDependencies`, dependency introspection |
| SDK codegen | `pulumi-language-rust/codegen/gen.go` | `GeneratePackage`: Pulumi schema → Rust SDK crate (typed resources, args structs, object types, invokes) |
| Program codegen | `pulumi-language-rust/codegen/gen_program.go` | `GenerateProject` / `GenerateProgram`: PCL → Rust program with typed resource construction and dynamic expression evaluation |
| Conformance entry | `pulumi-language-rust/language_test.go` | Runs the official `pulumi-test-language` suite against this implementation |
| Snapshots | `pulumi-language-rust/testdata` | Committed golden outputs of generated SDKs and projects, validated byte-for-byte by the harness |
| Policy SDK | `sdk/rust/pulumi/src/policy.rs` | Authoring and serving policy packs: an `Analyzer` server plus resource validation, remediation and configuration |
| Provider host | `sdk/rust/pulumi/src/provider.rs` | Serving a component provider: `GetSchema` and `Construct` |
| Policy packs | `pulumi-language-rust/testdata/policies` | Nine Rust policy packs exercised by the `policy-*` tests |
| Providers | `pulumi-language-rust/testdata/providers` | Rust component providers exercised by the `provider-*` tests |
| Examples | `examples/` | Nineteen pulumi/examples-style cloud programs across AWS, Azure, GCP, Kubernetes, DigitalOcean and Docker, plus language examples for config, outputs and components. All of them compile against SDKs this generator produces from the providers' real schemas |
| Full-SDK check | `scripts/check-full-sdks.sh` | Generates and compiles the *whole* SDK for every provider the examples pin — aws, azure-native, gcp, kubernetes, digitalocean, docker, random — since a defect two schema members produce only together cannot appear in a subset |
| Template | `templates/rust` | Starting point for a new Rust Pulumi project |

## How it fits together

The Pulumi engine talks to a language through the `LanguageRuntime` gRPC
interface. During a conformance test the harness:

1. `Pack`s the core SDK (`sdk/rust/pulumi`) into an artifact directory.
2. Binds each test's PCL program, discovers referenced provider packages,
   and calls `GeneratePackage` to produce a Rust SDK crate per package —
   snapshot-checked against `testdata/sdks/`.
3. Calls `GenerateProject` to produce a Rust program from the PCL —
   snapshot-checked against `testdata/projects/`.
4. `InstallDependencies` (cargo build), then runs a real deployment with the
   engine; the program speaks the resource-monitor protocol via the `pulumi`
   crate.
5. Asserts on the resulting state snapshot (resources, inputs/outputs,
   secrets, dependencies).

Generated SDKs and programs consume the core SDK and each other as Cargo
**path dependencies**; `Pack` artifacts are plain crate directories.

## Running the conformance suite

Requirements: Go (≥ 1.25), Rust (≥ 1.85), network access for crates.io.

```sh
cd pulumi-language-rust
go test -run TestLanguage -timeout 120m .
# a single test:
go test -run 'TestLanguage/l2-resource-simple$' -v .
# regenerate snapshots after a codegen change:
PULUMI_ACCEPT=1 go test -run TestLanguage -timeout 120m .
```

`expectedFailures` in `language_test.go` is the mechanism for skipping a
test with a reason while onboarding conformance, as pulumi-dotnet and
pulumi-java do. It is currently empty.

The `policy-*` and `provider-*` tests build the policy packs under
`testdata/policies` and the component providers under `testdata/providers`,
each of which is a real Rust crate the engine launches as a plugin.

Builds share a cargo target directory
(`$TMPDIR/pulumi-language-rust-target-$UID`)
so the dependency graph compiles once per machine, not once per test.

## What a generated program looks like

For the PCL program

```hcl
resource "res" "simple:index:Resource" {
    value = true
}
```

the generator emits

```rust
fn main() {
    pulumi::run(|ctx| async move {
        let res = pulumi_simple::Resource::new(&ctx, "res", pulumi_simple::ResourceArgs {
            value: Some(pulumi::pv::bool(true).cast()),
            ..Default::default()
        }, pulumi::ResourceOptions::default());
        Ok(())
    });
}
```

against a generated `pulumi_simple` crate whose `Resource::new` registers
the resource with the engine and exposes typed `Output` accessors for its
properties. Every field of a generated args struct is an `Option` and every
struct derives `Default`, so a program names the inputs it sets and elides
the rest — the same bargain C#, Go, Java and Python make, since a Rust
struct literal otherwise has to name all forty-four fields of something
like Azure's `WebAppArgs` to set one.

## Design notes

- **Dynamic core, typed shell.** The wire protocol flows through a dynamic
  `PropertyValue` model (mirroring Pulumi's property-value encoding:
  secrets, unknowns, output values, assets, archives, resource refs, with
  the canonical signature keys). Generated SDKs put a typed façade on top;
  PCL expression evaluation happens in dynamic space, which sidesteps the
  typed-collection inference problems that bite other languages' program
  generators.
- **`Output<T>`** is a shared future of `(value, secret, deps)`; every
  combinator propagates all three, matching the semantics of the other
  Pulumi SDKs (unknown values short-circuit `map`, secretness is sticky,
  dependencies union).
- **Exit-code contract**: programs log unhandled errors to the engine and
  exit 32 ("already reported"), like the .NET and Go SDKs; the host maps
  that to a `bail` response.
- **The SDK serves gRPC three ways.** Resource hooks run inside the
  program, so the SDK starts an in-process `Callbacks` server; a policy pack
  serves `Analyzer`; and a component provider serves `ResourceProvider`. The
  language host grows `Link` and `RunPlugin` to build and launch the latter
  two as plugins.

## Writing a program

See [`examples/`](./examples) for runnable programs and
[`templates/rust`](./templates/rust) for a starting point. A minimal
program reads config and exports an output:

```rust
fn main() {
    pulumi::run(|ctx| async move {
        let name = ctx
            .config()
            .get_string_or("name", pulumi::PropertyValue::String("world".to_string()));
        ctx.export("greeting", pulumi::pv::concat(vec![
            pulumi::pv::string("Hello, "),
            name,
        ]));
        Ok(())
    });
}
```

## Known limitations

None outstanding. [`docs/roadmap.md`](./docs/roadmap.md) records what the
suite cannot: the two behaviours that used to differ from the Go SDK and
now do not, the four generator defects the providers' real schemas
surfaced, and what `Construct` deliberately omits.

## License

Apache-2.0
