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

## Generator limitations found writing the examples

Two generator defects surfaced while writing the cloud examples, neither
exercised by any conformance schema. Both are now fixed.

- **Self-referential object types.** An object type that refers to itself,
  directly or through a chain, generated a Rust struct with an infinitely
  sized field — `JSONSchemaProps.not` in the Kubernetes schema is the
  canonical case, and it made the whole `pulumi_kubernetes` crate fail to
  compile. `findRecursiveTypes` now runs Tarjan's algorithm over the
  direct-containment graph and the emitters box any field whose type shares
  its owner's strongly connected component. A collection is already a
  separate allocation, so a type reached through a `Vec` or `BTreeMap` is
  not part of its container's size and is left unboxed. Covered by
  `codegen/recursive_test.go`; the generated Kubernetes SDK compiles.
- **Schema properties like `$ref`** produced invalid Rust identifiers.
  `snakeCase` now drops runes that cannot appear in an identifier, while
  the wire name is preserved separately.

One cosmetic defect is recorded but deliberately not fixed. `snakeCase`
starts a new word only at an uppercase letter whose predecessor is
lowercase, so a capital following a digit or another capital does not
separate: `ipv4Address` becomes `ipv4address` rather than `ipv4_address`,
and `publicIPAllocationMethod` becomes `public_ipallocation_method`. Python
and Go both word-break there, so the names a Rust user sees diverge from
every other Pulumi SDK for exactly those properties. The rule is at least
deterministic and total, and the fix is a small one in `naming.go` — but it
would rewrite field names throughout every committed snapshot and
invalidate the field lists in all sixteen cloud examples, each of which was
checked against the current rule. It is worth doing in a change of its own,
with the snapshots and examples regenerated together.

## Deliberate omissions

`Construct` drops two `ConstructRequest` fields that no test exercises and
that our `ResourceOptions` has no engine-side path for: `inputDependencies`
(per-property dependency URNs) and `accepts_output_values`. Conversely
`ResourceOptions::hide_diffs` and `env_var_mappings` have no
`ConstructRequest` counterpart, so a component cannot receive them.

Stack-level policies are not modelled: `AnalyzeStack` returns no
diagnostics, matching the Go SDK, since resource policies have already run
per resource.
