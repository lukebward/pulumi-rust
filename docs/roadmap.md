# Roadmap

What the implementation does not cover yet, and what each piece would take.
These notes come from reading the engine and the conformance suite, so they
name real RPCs and files rather than sketching.

## The `provider-*` family (5 tests)

These tests are not about *consuming* remote components — that already
works, and the generator emits `remote: true` registrations. They are about
*authoring* a component provider in Rust: the harness copies a provider
project from disk, links it against the packed core SDK, builds it, and the
engine launches it as a resource plugin and drives `Construct`.

Three pieces are missing:

1. **`LanguageRuntime.Link`** in the Go host. The runner calls it with the
   packed core artifact and expects the provider's `Cargo.toml` to be
   rewritten so its `pulumi` dependency points at that path.
2. **`LanguageRuntime.RunPlugin`** in the Go host. It builds and execs the
   provider binary, streaming stdout, stderr and finally an exit code. The
   engine reads stdout one byte at a time until the first newline and parses
   that as the plugin's gRPC port, so nothing may precede the port line.
3. **A `ResourceProvider` server in the Rust SDK** implementing at least
   `Construct`, plus `GetSchema` returning the package schema — the
   conformance loader binds PCL against *this* schema and generates the SDK
   from it, so it must match the stub provider's shape exactly.

Then two provider projects get checked in under
`pulumi-language-rust/testdata/providers/`, and `language_test.go` passes
`ProvidersDirectory`.

The SDK already serves gRPC for resource hooks (`sdk/rust/pulumi/src/callbacks.rs`),
so the server plumbing exists; what is missing is the provider service itself.

## The `policy-*` family (9 tests)

A policy pack is a separate program the engine runs as an analyzer plugin.
This needs:

1. **An `Analyzer` gRPC server in the core crate** (`Handshake`,
   `GetAnalyzerInfo`, `Configure`, `Analyze`, `AnalyzeStack`, `Remediate`,
   …) plus a `PolicyPack`/`Policy` authoring API, the Rust analogue of Go's
   `sdk/go/pulumi/policyx`. It has to live in the core crate, because the
   runner links only that one artifact into a pack.
2. **The same `Link` and `RunPlugin` host RPCs** the provider family needs.
3. **Nine policy packs** checked in under
   `pulumi-language-rust/testdata/policies/<name>/`, each a crate with
   `PulumiPolicy.yaml` (`runtime: rust`), `Cargo.toml` and `src/main.rs`.

Nothing engine-side needs reimplementing: config reconciliation, schema
validation, violation formatting and the mandatory-violation bail are all
handled by the engine.

## `l3-deferred-outputs` (1 test)

Two mutually dependent components, where one side also uses `range`. The
SDK already has the cycle-breaking primitives — `pulumi::deferred_output()`
and `RegisterRequest.deferred_inputs`, which omits an input from a
component's own registration while still letting the value reach its
children. Two things are missing in the generator:

- `genComponent` never calls `pcl.ExtractDeferredOutputVariables`, so
  nothing declares a deferred output or resolves its resolver.
- Components do not support the `range` option; only resources do.

Every in-tree Pulumi language skips this test as well.

## Known divergences

Two behaviors differ from the Go SDK without a conformance test covering
them:

- `Context::register_resource` does not fold the singular `provider` option
  into the inherited providers map, so a child does not inherit a parent's
  single explicit provider the way Go's `mergeProviders` arranges. Fixing it
  needs the provider's package name recorded on `Resource`.
- `do_call` seeds the result's secretness and dependencies from the call
  arguments; Go takes both solely from the `Call` response.
