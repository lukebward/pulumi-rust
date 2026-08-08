# pulumi-rust

An experimental Pulumi language implementation for **Rust**: SDK code
generation, program generation, a language host plugin, and a core Rust
runtime SDK — validated against Pulumi's official [language conformance
test suite](https://github.com/pulumi/pulumi/tree/master/pkg/testing/pulumi-test-language).

> Status: experimental. Built as an exploration of what a conformance-tested
> Rust language implementation looks like. Not an official Pulumi project.

**Conformance status** (pulumi/pulumi v3.256.0 suite, 180 tests): **119
pass**, 47 skipped for named unimplemented features (see
`expectedFailures` in `language_test.go`), and the `policy-*`/`provider-*`
families (14) are skipped wholesale like other out-of-tree languages do
while onboarding. Every non-skipped test passes, including the full
`l1-*` output/config/builtin set, `l2` resources/invokes/options/secrets/
assets/reads, and `l3` for/splat programs.

## What's here

| Component | Path | Description |
|---|---|---|
| Core SDK | `sdk/rust/pulumi` | The `pulumi` crate: gRPC resource-monitor client (tonic), `Output<T>` with unknown/secret/dependency propagation, property-value wire encoding, resource registration, invokes, config, stack lifecycle |
| Language host | `pulumi-language-rust` | Go gRPC plugin implementing Pulumi's `LanguageRuntime` interface: `Run` (cargo), `Pack`, `InstallDependencies`, dependency introspection |
| SDK codegen | `pulumi-language-rust/codegen/gen.go` | `GeneratePackage`: Pulumi schema → Rust SDK crate (typed resources, args structs, object types, invokes) |
| Program codegen | `pulumi-language-rust/codegen/gen_program.go` | `GenerateProject` / `GenerateProgram`: PCL → Rust program with typed resource construction and dynamic expression evaluation |
| Conformance entry | `pulumi-language-rust/language_test.go` | Runs the official `pulumi-test-language` suite against this implementation |
| Snapshots | `pulumi-language-rust/testdata` | Committed golden outputs of generated SDKs and projects, validated byte-for-byte by the harness |

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

Tests that the implementation does not support yet are listed with reasons
in `expectedFailures` in `language_test.go` — the same mechanism
pulumi-dotnet and pulumi-java use while onboarding conformance. The skip
list is feature-shaped: resource methods (`call`), package
parameterization/namespaces, resource hooks, local (in-language)
components, the `range` resource option, `try`/`can`/`recover`,
resource-reference hydration, byte strings, and a handful of resource
options (`aliases`, `hideDiffs`, `replaceWith`, `replacementTrigger`,
`envVarMappings`).

Builds share a cargo target directory (`$TMPDIR/pulumi-language-rust-target`)
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
            value: pulumi::pv::bool(true).cast(),
        }, pulumi::ResourceOptions::default());
        Ok(())
    });
}
```

against a generated `pulumi_simple` crate whose `Resource::new` registers
the resource with the engine and exposes typed `Output` accessors for its
properties.

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

## License

Apache-2.0
