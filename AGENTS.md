# Pulumi Rust

Experimental Pulumi Rust support: a Go language host (`pulumi-language-rust`), a
Rust code generator, and the Rust SDK runtime crate (`pulumi`). The host implements
Pulumi's `LanguageRuntime` gRPC interface so the CLI can run Rust programs; the
codegen turns Pulumi Package schemas into typed Rust SDK crates, and PCL into Rust
programs. Runtime name is `rust`; the plugin binary is `pulumi-language-rust`.

## Repository structure

| Path | Contents |
|---|---|
| `sdk/rust/pulumi/` | The `pulumi` crate (v0.1.0): resource-monitor client, `Output<T>`, property-value encoding, config, policy packs (`policy.rs`), component providers (`provider.rs`), automation API (`auto/` — drives the CLI; integration tests in `tests/auto.rs` skip without `pulumi` on PATH) |
| `pulumi-language-rust/` | Go module — language host binary (`main.go`), conformance entry point (`language_test.go`) |
| `pulumi-language-rust/codegen/` | `gen.go` (schema → Rust SDK crate), `gen_program.go` (PCL → Rust program), plus the fast generator tests |
| `pulumi-language-rust/testdata/` | Committed golden output: `sdks/`, `projects/`, plus `policies/` and `providers/` (real crates the engine launches as plugins) |
| `pulumi-language-rust/version/` | Version symbol stamped by the linker at build time |
| `examples/` | 22 example programs (19 cloud across six providers, plus component/config/random-password); see `examples/README.md` for what is and is not verified |
| `templates/rust/` | Starting point for a new Rust Pulumi project (`pulumi new`-style scaffold) |
| `docs/known-limitations.md` | What a green conformance suite does *not* cover |
| `scripts/check-full-sdks.sh` | The real-schema canary (see below) — the only script in the repo |

## Command canon

All commands run from the repo root. `make` targets are canonical; see `Makefile`.

```sh
make build            # cargo build the SDK + go build the host. Network: crates.io.
make test_sdk         # cargo test --locked in sdk/rust/pulumi. No plugins, no CLI.
make test_codegen     # go test ./codegen/... — no cargo, no plugins, no network. Seconds.
make test_conformance # Full pulumi-test-language suite; builds first. Needs Go, Rust,
                      #   crates.io. Slow (120m timeout). No provider plugins.
make accept           # test_conformance with PULUMI_ACCEPT=1: rewrites testdata/ snapshots.
make check_full_sdks  # The canary. Needs `pulumi` on PATH, network, ~20 minutes.
make test_fast        # test_sdk + test_codegen. The pair to run before pushing.
make test_all         # test_sdk + test_codegen + test_conformance. Excludes the canary.
make lint             # go vet + golangci-lint + go mod tidy -diff; rustfmt --check + clippy
make format           # rustfmt + golangci-lint fmt. Excludes generated testdata.
make changelog        # changie new
make clean            # cargo clean plus the shared cargo target dir
```

Narrow conformance with `TEST_FILTER`, e.g.
`TEST_FILTER='l2-resource-simple' make test_conformance`; same flag for `make accept`.

Tool versions come from `.mise.toml` (Go, golangci-lint, changie, pulumi): run
`mise install`, then `make` directly or prefixed with `mise exec --`. The Rust
toolchain is deliberately **not** there — `rust-toolchain.toml` owns it, because
rustup honours that file for every cargo invocation the host and harness spawn.

## The conformance suite is the primary correctness gate

`pulumi-language-rust/language_test.go` runs the official `pulumi-test-language`
suite. **All 179 tests pass with no skips**, and `expectedFailures` — the map that
skips a test with a reason, as pulumi-dotnet and pulumi-java use during onboarding —
**is currently empty**. Keep it that way: adding an entry is taking on debt, not
fixing a test. CI runs a representative subset on pull requests and the full suite on
push. The suite snapshot-checks generated SDKs against `testdata/sdks/` and generated
programs against `testdata/projects/`, byte for byte, before it ever deploys.

## The real-schema canary is not a gate

`make check_full_sdks` (`scripts/check-full-sdks.sh`) generates and compiles the
*whole* SDK for every provider the examples pin, then compiles every example against
it. The conformance suite uses small synthetic schemas; real ones are larger by four
orders of magnitude and contain shapes the suite never produces.

**Run it periodically, not per-change.** It needs `pulumi` on `PATH`, a network and
about twenty minutes. Nothing in CI depends on it, and a red run is a prompt to go
and look, not a broken build. Worth running when the generator changes shape, when a
provider pin moves, and before a release.

**A defect it finds does not stay there.** Discovery is the canary's whole job, but
it is far too slow and network-dependent to be what keeps a defect from coming back.
When it surfaces one, shrink the schema to the smallest shape that still reproduces
it and land that as a test under `pulumi-language-rust/codegen/`, where it runs on
every pull request in seconds. All four defects it has found were retired that way —
into `recursive_test.go`, `typenames_test.go` and `naming_vectors_test.go`. See "The
real-schema canary" in `CONTRIBUTING.md` for the full table.

## Key invariants

- The Go module path is `github.com/pulumi-labs/pulumi-rust/pulumi-language-rust`. The
  repo has **not** moved to the pulumi org — never write `github.com/pulumi/...` as
  if it had.
- `VERSION_PKG` in the `Makefile` must match `pulumi-language-rust/go.mod` exactly:
  the linker silently ignores a `-X` whose symbol it cannot resolve, producing an
  unstamped binary. Update both together.
- Go code lives in `pulumi-language-rust/` (nested module, no root `go.mod`). Run
  `cd pulumi-language-rust && go test ./...`, not from the root.
- Generated SDKs and programs consume the core SDK and each other as Cargo **path**
  dependencies; both sides must resolve to the same checkout or cargo reports
  `package collision in the lockfile`.
- Conformance builds share a cargo target dir under `$XDG_CACHE_HOME`, deliberately
  not `/tmp`; `make clean` removes it.
- Copyright headers are required on Go files, enforced by `goheader` in `.golangci.yml`.

## Forbidden patterns

- Do not hand-edit `pulumi-language-rust/testdata/{sdks,projects}` — they are
  snapshots of generator output. Regenerate with `make accept`.
- Do not add entries to `expectedFailures` to make a test pass.
- Do not add a `rust` key to `.mise.toml` (see above).
- Do not run `git push --force` or `git reset --hard` without explicit approval.
- Do not fabricate test output or changelog entries.
- Do not commit `target/` directories or the built `pulumi-language-rust` binary.

## Escalate immediately if

- A change affects the `LanguageRuntime` gRPC protocol or the property-value encoding.
- `make accept` produces an unexpectedly large snapshot diff.
- A passing conformance test starts failing and two debugging attempts have not explained why.
- A change would require renaming the Go module or the `pulumi` crate.

## If you change...

| What changed | Run |
|---|---|
| Any `.rs` file in `sdk/rust/pulumi/` | `make lint_rust && make test_sdk` |
| `pulumi-language-rust/codegen/*.go` | `make test_codegen`, then `make accept` if the snapshot change is intended — review the diff |
| `pulumi-language-rust/main.go` | `make lint_go && make test_conformance` |
| `go.mod` / `go.sum` | `cd pulumi-language-rust && go mod tidy`, then `make lint_go` |
| A provider pin in `examples/` | `make check_full_sdks` (periodic, not blocking) |
| Anything user-visible | `make changelog` (see below) |

## Changelog

`make changelog` runs `changie new`, writing a fragment to `.changes/unreleased/`.
Component is `sdk`, `language` or `codegen`; kind is `Improvements` (minor) or
`Bug Fixes` (patch). `Dependencies` is reserved for automated Renovate updates.
