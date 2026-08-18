# Known limitations

The conformance suite passes in full, so this file records what a green
suite does not: behaviours that differed from the Go SDK, defects the real
provider schemas surfaced, and what is deliberately left out.

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

  This file previously claimed no conformance test covered this. That was
  wrong: `l2-resource-provider-inheritance` asserts exactly the child case.
  It passed anyway, because the engine's own `inheritFromParent` copies a
  parent's provider onto a child, and the monitor falls back to the
  receiver's goal state for `Call`. What was genuinely unmasked — and still
  untested by the suite — is an invoke parented to a resource whose provider
  came from the singular option, plus the package-match rule, which was a
  second divergence this file never recorded.

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

  Worth stating plainly, because it *reduces* secret propagation: a method
  called with a secret argument whose provider does not mark the return
  secret now produces a non-secret result. Go puts that responsibility on
  the provider deliberately, and a Rust-only rule here would be exactly the
  kind of divergence that disqualifies a language from being official.

A third divergence surfaced while verifying those two, and is fixed as well:
`read_resource` never sent a provider at all, so a resource read through an
explicit provider was read by the default one instead, and its children
inherited nothing. Registration and read now share one `resolve_providers`.

## Generator limitations found against real provider schemas

Four generator defects surfaced against the providers' published schemas,
none of them exercised by any conformance schema. All four are now fixed.

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
- **Type names were not unique.** A Rust type name was derived from a
  schema token's module and member alone, which is not enough to tell two
  tokens apart: `aws:iam/getPrincipalPolicySimulationResult:getPrincipalPolicySimulationResult`
  and the result type the binder synthesizes for the function
  `aws:iam/getPrincipalPolicySimulation:getPrincipalPolicySimulation` both
  derive `IamGetPrincipalPolicySimulationResult`, so the full `pulumi_aws`
  crate declared that struct twice and failed to compile. `resolveTypeNames`
  now assigns every discovered token a name no other token holds, reserving
  a name together with its `Args` form and appending a numeric suffix on
  collision. Schema-declared types are named before synthesized function
  results, so the type that can be referenced from anywhere in the schema
  keeps the undecorated name and the invoke's own result type takes the
  suffix. The program generator resolves names the same way, so a program
  and the SDK it builds against agree. Covered by
  `codegen/typenames_test.go`.

  Two collision classes exist and both are covered: a declared type against
  a synthesized function result, and two declared types in sibling
  submodules of one module (`ec2/instance:Filter` and `ec2/volume:Filter`
  under aws' `moduleFormat`). This is also why the whole schema of every
  provider is now generated and compiled, not just the subsets the examples
  need — a collision is invisible unless both colliding members are
  generated at once, which is precisely what a subset does not do.
  `scripts/check-full-sdks.sh` is that check. One boundary remains:
  azure-native is checked from the default-version schema its provider
  checks into its repository, because the all-API-versions schema the
  plugin serves generates a 441 MB `lib.rs` that rustc cannot compile as
  one crate. Whether the versioned surface hides further collisions is
  therefore unverified; compiling it at all needs the crate split by
  module or gated by features first.

  Go's generator has the same problem and resolves it in the same order —
  it registers resource names, then type names, then function names, so a
  type also wins there — but with different repairs: a colliding type takes
  a `Type` then `Typ` suffix, a colliding function is renamed (`GetX` →
  `LookupX`), and an irreparable collision panics. The renames do not carry
  over. A Rust function and a Rust struct do not share a namespace, so
  renaming `get_principal_policy_simulation` would change the public API to
  fix a collision Rust does not have; only the type needs a new name, and a
  numeric suffix says "this is the second thing that wanted this name"
  without implying anything about the schema.

## Deliberate omissions

`Construct` drops two `ConstructRequest` fields that no test exercises and
that our `ResourceOptions` has no engine-side path for: `inputDependencies`
(per-property dependency URNs) and `accepts_output_values`. Conversely
`ResourceOptions::hide_diffs` and `env_var_mappings` have no
`ConstructRequest` counterpart, so a component cannot receive them.

Stack-level policies are not modelled: `AnalyzeStack` returns no
diagnostics, matching the Go SDK, since resource policies have already run
per resource.

## Automation API coverage

The conformance suite exercises the SDK as a program the CLI runs; the
automation API (`pulumi::auto`) inverts that relationship and is therefore
outside the suite entirely. Its own gates are the unit tests beside the
module (argument assembly against a recorded mock, event-grammar and
settings serialization, error classification — no CLI involved) and
`sdk/rust/pulumi/tests/auto.rs`, integration tests that drive a real
`pulumi` CLI against a local file backend, exercising local YAML programs
and inline Rust programs end to end. Those integration tests skip
themselves when `pulumi` is not on `PATH`, so `make test_sdk` stays
hermetic; run them with the CLI installed to get the full check.

Deliberately not ported from the Go `auto` package, in rough order of
likely demand: remote workspaces and git-sourced programs, preview-only
refresh/destroy variants, `pulumi import` (`Stack.ImportResources`) and
`stack rename`, per-command tee'd progress writers (captured output and
engine events cover the same need), the gRPC event transport newer CLIs
offer — the file-based `--event-log` works on every CLI version the SDK
supports — and a tail of smaller options: `UserAgent` (`--exec-agent`),
`AttachDebugger`, preview's `ImportFile`, refresh's
`ClearPendingCreates`/`ImportPendingCreates`, `SetAllConfigJson`,
`org get-default`/`set-default`, `stack ls --all`, and installing a
plugin from a custom server.

One behavior is stricter than Go's: inline programs in a single process
are serialized, because the SDK keeps one active program context per
process for resource-reference hydration. Concurrent inline stack
operations queue rather than cross-wire; local-program operations run
concurrently without restriction.
