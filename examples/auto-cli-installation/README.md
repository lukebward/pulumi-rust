# CLI Installation (Automation API)

This example demonstrates the installation capabilities of the Automation API. By
default the Automation API expects the `pulumi` binary to be on your `PATH`. With
`LocalPulumiCommand::install` you can instead pin a CLI version and let the SDK
download and manage the installation.

The example installs Pulumi v3.200.0 into a versioned root under the system temp
directory, builds a workspace over that installed command, and runs a small inline
program lifecycle (up, print the output, destroy, remove the stack) with it. The
installed binary does the work, not the ambient CLI: the program prints the version
the installed binary reports and the version the workspace drives.

## Prerequisites

1. Network access on the first run: the install downloads roughly 100MB from
   get.pulumi.com. Reruns reuse the installed CLI, because the root is a stable
   versioned path and `install` leaves a matching existing installation untouched.
2. A state backend. The commands below use a throwaway local file backend, so no
   Pulumi Cloud account is needed.

A `pulumi` CLI on your `PATH` is not required; the example installs its own.

## Run it

This is a plain Rust binary; no invocation through the Pulumi CLI is required:

```shell
export PULUMI_BACKEND_URL=file://$(mktemp -d)
export PULUMI_CONFIG_PASSPHRASE=test
cargo run
```

```
Installing Pulumi v3.200.0 into <temp-dir>/auto-cli-installation/3.200.0
(roughly a 100MB download from get.pulumi.com on the first run)
Installed CLI at <temp-dir>/auto-cli-installation/3.200.0/bin/pulumi reports version v3.200.0
Created/Selected stack "dev"
Workspace drives Pulumi v3.200.0, not the CLI on PATH
Starting update
  create pulumi:pulumi:Stack ...
  create pulumi:pulumi:Stack done
  1 resource change(s) in 1s
Update succeeded!
greeting: hello from an installed CLI
Starting stack destroy
  delete pulumi:pulumi:Stack ...
  delete pulumi:pulumi:Stack done
  1 resource change(s) in 1s
Stack successfully destroyed and removed
```

The run ends with the stack destroyed and removed, so a rerun starts clean. On a
download failure the program prints a clear error message and exits non-zero.
