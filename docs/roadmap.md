# Roadmap

The conformance suite passes in full, so this file records what a green
suite does not: behaviours that differed from the Go SDK, defects the
example programs surfaced, and what is deliberately left out.

## Known divergences

None outstanding. Two were recorded here and both are fixed:

- `Context::register_resource` did not fold the singular `provider` option
  into the inherited providers map, so a child could not inherit a parent's
  single explicit provider. It now does what Go's `mergeProviders` and
  `getProvider` do together: the singular provider joins the map under the
  package it serves unless the map already names that package, and the
  provider actually sent for a resource is the singular one only when its
  package matches the resource's — otherwise the map decides. `Resource`
  records the package a provider serves to make that possible.

  The roadmap previously claimed no conformance test covered this. That was
  wrong: `l2-resource-provider-inheritance` asserts exactly the child case.
  It passed anyway, because the engine's own `inheritFromParent` copies a
  parent's provider onto a child, and the monitor falls back to the
  receiver's goal state for `Call`. What was genuinely unmasked — and still
  untested by the suite — is an invoke parented to a resource whose provider
  came from the singular option, plus the package-match rule, which was a
  second divergence the roadmap never recorded.

- `do_call` seeded the result's secretness and dependencies from the call
  arguments. Go and Python both take both solely from the `Call` response:
  the provider decides what its return value depends on and whether it is
  sensitive, and the arguments' own dependencies travel separately in
  `argDependencies` so the provider can see them. Marking a plain return
  value secret because an argument was is a state-file correctness problem,
  so this now matches the reference.

  `do_invoke` deliberately keeps the argument-seeded behaviour, because Go's
  invoke path does the same. The asymmetry is in the reference, not an
  oversight, and is commented in the code so it does not get "fixed".

## Generator limitations found writing the examples

Three generator defects surfaced while writing the cloud examples, none of
them exercised by any conformance schema. All three are now fixed.

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
- **Field names did not word-break like the other SDKs.** `snakeCase` started
  a new word only at an uppercase letter whose predecessor was lowercase, so
  `ipv4Address` became `ipv4address` and `publicIPAllocationMethod` became
  `public_ipallocation_method`. Python is the only other Pulumi generator
  that word-breaks at all — Go, Node and .NET emit the schema name with at
  most a first-letter change — so `PyName` is the one reference, and
  `snakeCase` now implements the same rule: a capital after a lowercase
  letter or a digit opens a word, a run of capitals is one word, and a
  lowercase letter ending a run hands the run's last capital to the next
  word.

  Two lookaheads deliberately diverge from Python, in both cases because
  Python's own output is indefensible. A trailing `s` closing a run of
  capitals stays in the acronym only when it is not itself starting a word,
  so `podCIDRs` is `pod_cidrs` while `openXJsonSerDe` is `open_x_json_ser_de`
  — Python folds the `s` unconditionally and carries a hard-coded override
  list to escape the consequences (pulumi/pulumi#5199). And a single
  lowercase letter wedged between a run of capitals and a digit belongs to
  the acronym, so `isIPv6Enabled` is `is_ipv6_enabled` rather than Python's
  `is_i_pv6_enabled`.

  Checked for collisions across all seven providers the examples use —
  kubernetes, gcp, docker, digitalocean, aws, azure-native and random —
  covering 59,291 structs and 310,334 property occurrences: no two distinct
  schema property names fold onto one Rust identifier under either the old
  rule or the new one. The new rule can only insert separators into the old
  output, so it can split an existing collision apart but never create one.

## Deliberate omissions

`Construct` drops two `ConstructRequest` fields that no test exercises and
that our `ResourceOptions` has no engine-side path for: `inputDependencies`
(per-property dependency URNs) and `accepts_output_values`. Conversely
`ResourceOptions::hide_diffs` and `env_var_mappings` have no
`ConstructRequest` counterpart, so a component cannot receive them.

Stack-level policies are not modelled: `AnalyzeStack` returns no
diagnostics, matching the Go SDK, since resource policies have already run
per resource.
