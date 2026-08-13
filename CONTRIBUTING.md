# Contributing

Building Pulumi Rust support from source, running the tests, and how the
pieces fit together.

Please make sure to read and observe our
[Code of Conduct](https://github.com/pulumi/pulumi/blob/master/CODE-OF-CONDUCT.md).

## What's here

| Component | Path | Description |
|---|---|---|
| Core SDK | `sdk/rust/pulumi` | The `pulumi` crate: gRPC resource-monitor client (tonic), `Output<T>` with unknown/secret/dependency propagation, property-value wire encoding, resource registration, invokes, config, stack lifecycle |
| Language host | `pulumi-language-rust` | Go gRPC plugin implementing Pulumi's `LanguageRuntime` interface: `Run` (cargo), `Pack`, `InstallDependencies`, `Link`, `RunPlugin`, dependency introspection |
| SDK codegen | `pulumi-language-rust/codegen/gen.go` | `GeneratePackage`: Pulumi schema → Rust SDK crate (typed resources, args structs, object types, invokes) |
| Program codegen | `pulumi-language-rust/codegen/gen_program.go` | `GenerateProject` / `GenerateProgram`: PCL → Rust program with typed resource construction and dynamic expression evaluation |
| Conformance entry | `pulumi-language-rust/language_test.go` | Runs the official `pulumi-test-language` suite against this implementation |
| Snapshots | `pulumi-language-rust/testdata` | Committed golden outputs of generated SDKs and projects, validated byte-for-byte by the harness |
| Policy SDK | `sdk/rust/pulumi/src/policy.rs` | Authoring and serving policy packs: an `Analyzer` server plus resource validation, remediation and configuration |
| Provider host | `sdk/rust/pulumi/src/provider.rs` | Serving a component provider: `GetSchema` and `Construct` |
| Policy packs | `pulumi-language-rust/testdata/policies` | Nine Rust policy packs exercised by the `policy-*` tests |
| Providers | `pulumi-language-rust/testdata/providers` | Rust component providers exercised by the `provider-*` tests |
| Examples | `examples/` | Cloud and language examples; see [`examples/README.md`](./examples/README.md) for what is and is not verified about them |
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

## Building and testing

```sh
make build              # core SDK + language host
make test_sdk           # the Rust SDK's own tests
make test_codegen       # the generator's tests: naming, recursive types,
                        # type-name collisions, args shape, defaults
make test_fast          # both of the above: everything needing no network
make lint               # go vet, golangci-lint, go mod tidy -diff, rustfmt, clippy
make format             # format the hand-written Rust and Go
```

The toolchains are pinned: `rust-toolchain.toml` fixes the Rust compiler (so
`cargo fmt` and `cargo clippy` agree everywhere, including in the cargo runs
the conformance harness spawns), and `.mise.toml` fixes Go and the linters.
`make lint` is what CI runs; a clean run locally means a clean run there.

### The conformance suite

Requirements: Go (≥ 1.25), Rust (≥ 1.85), network access for crates.io.

```sh
make test_conformance                       # the whole suite
TEST_FILTER='l2-resource-simple' make test_conformance   # one test
make accept                                 # regenerate snapshots after a
                                            # codegen change
```

**Conformance status** (pulumi/pulumi v3.256.0 suite): **all 179 tests
pass**, with no skips. 179 is the whole suite — `LanguageTests` registers
180 entries, but the harness never hands out the one `internal-` test.

That covers the full `l1-*` output/config/builtin set including `try`/`can`
and `recover`; `l2` resources, invokes, resource methods (`call`), every
resource option, secrets, assets, reads, resource-reference hydration,
lifecycle hooks, package parameterization and non-UTF8 byte strings; `l3`
for/splat programs, the `range` option, local components and deferred
outputs; and the `policy-*` and `provider-*` families, which require the
language to author policy packs and component providers.

`expectedFailures` in `language_test.go` is the mechanism for skipping a
test with a reason while onboarding conformance, as pulumi-dotnet and
pulumi-java do. It is currently empty.

The `policy-*` and `provider-*` tests build the policy packs under
`testdata/policies` and the component providers under `testdata/providers`,
each of which is a real Rust crate the engine launches as a plugin.

Builds share a cargo target directory under the user's cache directory
(`$XDG_CACHE_HOME/pulumi-language-rust/target`, or the platform equivalent)
so the dependency graph compiles once per machine, not once per test. It is
deliberately not in `/tmp`: a predictable path in a world-writable directory
is another local user's to pre-create or symlink, and cargo would then write
— and later execute — build-script binaries from a location they control.
`make clean` removes it.

### The real-schema canary

The conformance suite uses small synthetic schemas. Real ones are larger by
four orders of magnitude and contain shapes the suite never produces:

```sh
make check_full_sdks                        # every provider the examples pin
scripts/check-full-sdks.sh aws@7.41.0       # just one
```

This generates and compiles the *whole* SDK for each provider — azure-native's
from the default-version schema its provider checks into its repository —
then compiles every example that pins it against that whole crate. See
[`examples/README.md`](./examples/README.md) for why a subset check is not a
substitute, and for what "whole" means for azure-native.

**Run it periodically, not per-change.** It needs `pulumi` on `PATH`, a
network, and about twenty minutes, so it is a canary rather than a gate:
nothing in CI depends on it, and a red run is a prompt to go and look, not a
broken build. Worth running when the generator changes shape, when a provider
pin moves, and before a release.

**A defect it finds does not stay here.** Discovery is the canary's whole
job — it is the only thing in the repo that reads schemas big enough to
contain the pathological cases — but it is far too slow and too
network-dependent to be what keeps a defect from coming back. When it
surfaces one, shrink the schema to the smallest shape that still reproduces
it and land that as a test under `pulumi-language-rust/codegen/`, where it
runs on every pull request in seconds. All four generator defects the canary
has found were retired that way:

| Found against | Retired into |
|---|---|
| `kubernetes` — `JSONSchemaProps.not`, a self-referential type, gave a struct an infinitely sized field | `recursive_test.go` |
| `aws` — two schema tokens deriving one Rust type name | `typenames_test.go` |
| property names that did not word-break like the other SDKs (`ipv4Address`, `podCIDRs`) | `naming_vectors_test.go` |
| schema properties such as `$ref`, which are not valid Rust identifiers | `TestSnakeCaseDropsNonIdentifierRunes` |

`naming_vectors_test.go` states the discipline in its header comment: every
case is "either a real provider property name or the shortest name
exhibiting a shape that appears in one." A case distilled that far is worth
more than the provider it came from — it says what the rule is, and it says
it in milliseconds.

This is the division of labour pulumi/pulumi uses. Its SDK codegen tests are
~70 hand-authored schemas under `pkg/codegen/testing/test/testdata`, several
distilled from real providers — `naming-collisions`, and
`azure-native-nested-types`, described there as a "condensed example of
nested collection types from Azure Native". Nothing regenerates a whole
published provider on every pull request, and neither should this repo.

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

against a generated `pulumi_simple` crate whose `Resource::new` registers the
resource with the engine and exposes typed `Output` accessors for its
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
- **Args are all optional.** Every generated args struct derives `Default`
  and every field is an `Option`, so a program names the inputs it sets and
  elides the rest — the same bargain C#, Go, Java and Python make, since a
  Rust struct literal otherwise has to name all forty-four fields of
  something like Azure's `WebAppArgs` to set one. Requiredness is still
  carried to the engine, from the schema rather than from the type.
- **Exit-code contract**: programs log unhandled errors to the engine and
  exit 32 ("already reported"), like the .NET and Go SDKs; the host maps
  that to a `bail` response.
- **The SDK serves gRPC three ways.** Resource hooks run inside the program,
  so the SDK starts an in-process `Callbacks` server; a policy pack serves
  `Analyzer`; and a component provider serves `ResourceProvider`. The
  language host grows `Link` and `RunPlugin` to build and launch the latter
  two as plugins.

## Changelog

Every user-visible change carries a changelog fragment, assembled by
[changie](https://changie.dev). Add one with:

```sh
make changelog          # or: changie new
```

It asks for a component (`sdk`, `language` or `codegen`), a kind, and the PR
number, then writes a YAML fragment under `.changes/unreleased/`. Commit that
fragment with the change. `CHANGELOG.md` is generated — never edit it by hand.

## Releasing

A release is cut from the changelog rather than from a tag push, so the
version in `CHANGELOG.md` and the version on the tag cannot disagree:

```sh
changie batch auto      # fold the unreleased fragments into a version
changie merge           # regenerate CHANGELOG.md

# The crate version is a literal in the manifest and nothing derives it from
# the changelog, so bump it here or the publish job will refuse the release.
version="$(changie latest)"
sed -i '0,/^version = /s//version = "'"${version#v}"'"\n/' sdk/rust/pulumi/Cargo.toml
(cd sdk/rust/pulumi && cargo update --workspace)   # refresh the locked version

git add -A .changes CHANGELOG.md sdk/rust/pulumi/Cargo.toml sdk/rust/pulumi/Cargo.lock
git commit -m "Changelog for $version"
git push
```

`git add -A .changes` matters: `changie batch` both writes a new
`.changes/<version>.md` and deletes the fragments it consumed, and the release
workflow checks that version file exists.

Landing that commit on `main` triggers `.github/workflows/release.yml`, which
re-runs CI, tags the release and has GoReleaser publish the language plugin
binaries. The archive names are load-bearing — the CLI computes the asset name
it wants and compares it for exact equality — so `.goreleaser.yml` is the one
file to change carefully. It documents the contract at the top.

## Known limitations

[`docs/known-limitations.md`](./docs/known-limitations.md) records what a green conformance
suite does not: the behaviours that used to differ from the Go SDK, the
generator defects real provider schemas surfaced, and what `Construct`
deliberately omits.
