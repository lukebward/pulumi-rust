# Roadmap

The conformance suite passes in full, so this file records only the
behaviors that differ from the Go SDK without a test covering them.

## Known divergences

Two behaviors differ from the Go SDK without a conformance test covering
them:

- `Context::register_resource` does not fold the singular `provider` option
  into the inherited providers map, so a child does not inherit a parent's
  single explicit provider the way Go's `mergeProviders` arranges. Fixing it
  needs the provider's package name recorded on `Resource`.
- `do_call` seeds the result's secretness and dependencies from the call
  arguments; Go takes both solely from the `Call` response.

## Deliberate omissions

`Construct` drops two `ConstructRequest` fields that no test exercises and
that our `ResourceOptions` has no engine-side path for: `inputDependencies`
(per-property dependency URNs) and `accepts_output_values`. Conversely
`ResourceOptions::hide_diffs` and `env_var_mappings` have no
`ConstructRequest` counterpart, so a component cannot receive them.

Stack-level policies are not modelled: `AnalyzeStack` returns no
diagnostics, matching the Go SDK, since resource policies have already run
per resource.
