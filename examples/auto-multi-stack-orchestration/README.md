# Automation API: Multi-Stack Orchestration

This program demonstrates how to use the Automation API to orchestrate multiple stacks,
propagating one stack's outputs as inputs to a dependent stack. Two projects with inline
programs stand in for the Go original's website/object pair:

1. `multiStackWebsite`: exports a `bucketID` and a `websiteUrl`.
2. `multiStackObject`: consumes `bucketID` and exports an `objectKey` derived from it.

The mechanism matches the Go example: the automation program reads `bucketID` from the first
stack's `up` outputs and curries it into the second stack's inline program closure. No
`StackReference` is involved. At the end the stacks are destroyed in reverse dependency order
(object first, then website) and removed, so a rerun starts clean.

## Prerequisites

1. The `pulumi` CLI on your `PATH` (v3.0.0 or later).
2. A state backend. The commands below use a throwaway local file backend, so no cloud
   account or credentials are needed.

## Running

```shell
$ export PULUMI_BACKEND_URL=file://$(mktemp -d)
$ export PULUMI_CONFIG_PASSPHRASE=test
$ cargo run
preparing website stack
Created/Selected stack "dev"
Starting refresh
Refresh succeeded!
website stack ready to deploy
Starting website stack update
  create pulumi:pulumi:Stack multiStackWebsite-dev
  create: 1 (1s)
Website stack update succeeded!
got bucketID "website-bucket-multiStackWebsite-dev" for object stack
preparing object stack
Created/Selected stack "dev"
Starting refresh
Refresh succeeded!
object stack ready to deploy
Starting object stack update
  create pulumi:pulumi:Stack multiStackObject-dev
  create: 1 (1s)
Object stack update succeeded!
objectKey: "website-bucket-multiStackWebsite-dev/index.html"
URL: "http://website-bucket-multiStackWebsite-dev.example.com"
Starting object stack destroy
  delete pulumi:pulumi:Stack multiStackObject-dev
  delete: 1 (1s)
Object stack successfully destroyed
Starting website stack destroy
  delete pulumi:pulumi:Stack multiStackWebsite-dev
  delete: 1 (1s)
Website stack successfully destroyed
```
