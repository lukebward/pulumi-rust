# Local Program

This example demonstrates adding an Automation API driver to an existing `local` Pulumi program: an ordinary on-disk project that could just as well be deployed with the Pulumi CLI directly.

The Go original drives a separate Fargate project (ECS cluster, ECR registry, Docker build, load balancer). This port keeps the same flow (workspace over the project directory, create/select stack, set config, refresh, up with streamed progress, outputs, destroy on the `destroy` argument) but drives the provider-less Pulumi YAML project embedded at [`./project`](./project), so it runs anywhere the Pulumi CLI runs, with no cloud credentials and no plugins:

- `/project`: the Pulumi program (`Pulumi.yaml`, runtime `yaml`) with a `siteName` config value and `url`/`siteName` outputs.
- `/src`: the Automation API deployment driver, run like any normal Rust program.

## Prerequisites

1. The Pulumi CLI on `PATH` ([install](https://www.pulumi.com/docs/install/)).
2. A state backend. The commands below use a throwaway local file backend, so no Pulumi Cloud account is needed.

## Run

Deploying is just `cargo run`. No invocation through the Pulumi CLI is required:

```shell
$ export PULUMI_BACKEND_URL=file://$(mktemp -d)
$ export PULUMI_CONFIG_PASSPHRASE=test
$ cargo run
Created/Selected stack "dev"
Successfully set config
Starting refresh
Refresh succeeded!
Starting update
    create pulumi:pulumi:Stack local-program-dev...
    create pulumi:pulumi:Stack local-program-dev done
    resources: create 1
Update succeeded!
URL: http://my-site.example.com
```

To destroy the stack when you are done, invoke the program with an additional `destroy` argument (in the same shell, so it targets the same backend):

```shell
$ cargo run -- destroy
Created/Selected stack "dev"
Successfully set config
Starting refresh
Refresh succeeded!
Starting stack destroy
    delete pulumi:pulumi:Stack local-program-dev...
    delete pulumi:pulumi:Stack local-program-dev done
    resources: delete 1
Stack successfully destroyed and removed
```

The destroy path also removes the stack (including the `Pulumi.dev.yaml` stack settings in `./project`), so the next `cargo run` starts clean.
