# Automation API: Passphrase Secrets Provider

This program demonstrates how to use the Automation API with an inline Pulumi program and a
passphrase secrets provider. The workspace is created with `secrets_provider: "passphrase"`,
the passphrase travels through the workspace environment (`PULUMI_CONFIG_PASSPHRASE`), and the
stack settings carry the provider so subsequent runs reuse it. The program:

1. Creates a stack whose secrets provider is `passphrase`.
2. Sets a secret config value and reads it back decrypted.
3. Runs an update; the secret output stays marked secret.
4. Rotates the stack to a new passphrase with `change_stack_secrets_provider` and shows the
   config still decrypts (the Go original does not show rotation; the Rust API supports it).
5. Destroys and removes the stack, so a rerun starts clean.

Note: the workspace's own `PULUMI_CONFIG_PASSPHRASE` overrides the one exported in your shell
for every CLI invocation the workspace makes, mirroring how the Go example passes the
passphrase via `auto.EnvVars`.

## Prerequisites

1. The `pulumi` CLI on your `PATH` (v3.0.0 or later).
2. A state backend. The commands below use a throwaway local file backend, so no cloud
   account or credentials are needed.

## Running

```shell
$ export PULUMI_BACKEND_URL=file://$(mktemp -d)
$ export PULUMI_CONFIG_PASSPHRASE=test
$ cargo run
Created/Selected stack "dev"
Successfully set config
Read myPassword back decrypted: value="s3cret-hunter2" secret=true
Starting update
  create pulumi:pulumi:Stack passphraseSecretsProject-dev
  create: 1 (1s)
Update succeeded!
secretValue stays marked secret: true
greeting: "hello from a passphrase-encrypted stack"
Rotating the stack to a new passphrase
After rotation myPassword still decrypts: value="s3cret-hunter2" secret=true
Starting stack destroy
  delete pulumi:pulumi:Stack passphraseSecretsProject-dev
  delete: 1 (1s)
Stack successfully destroyed and removed
```
