# Git Repo Program (Automation API)

This example demonstrates how to run a Pulumi program from a git repository with the
Automation API: set `repo` on `LocalWorkspaceOptions` to a `GitRepo` and the workspace
clones the repository, checks out the requested branch, and points the work dir at the
project inside it.

To stay offline and deterministic, the example first creates a small local git
repository in a temp directory (a provider-less Pulumi YAML program, committed on a
`main` branch), then clones it by branch and runs the full lifecycle: set config,
refresh, up, print the stack output, destroy, and remove the stack. A real https URL
works identically; the only change is the `url` field:

```rust
let repo = GitRepo {
    url: "https://github.com/pulumi/examples.git".to_string(),
    project_path: Some(PathBuf::from("aws-go-s3-folder")),
    ..Default::default()
};
```

## Prerequisites

1. The `pulumi` CLI on your `PATH`.
2. A `git` binary on your `PATH` (git-sourced workspaces shell out to it; the fixture
   repo creation uses `git init -b`, which needs git 2.28 or newer).
3. A state backend. The commands below use a throwaway local file backend, so no
   Pulumi Cloud account is needed.

## Run it

This is a plain Rust binary; no invocation through the Pulumi CLI is required:

```shell
export PULUMI_BACKEND_URL=file://$(mktemp -d)
export PULUMI_CONFIG_PASSPHRASE=test
cargo run
```

```
Created fixture git repo at <temp-dir>/auto-git-repo-program-...
Created/Selected stack "dev", and cloned program from git
Successfully set config
Starting refresh
Refresh succeeded!
Starting update
  create pulumi:pulumi:Stack ...
  create pulumi:pulumi:Stack done
  1 resource change(s) in 1s
Update succeeded!
message: hello from a cloned program
Starting stack destroy
  delete pulumi:pulumi:Stack ...
  delete pulumi:pulumi:Stack done
  1 resource change(s) in 1s
Stack successfully destroyed and removed
```

The run ends with the stack destroyed and removed, so a rerun starts clean.
