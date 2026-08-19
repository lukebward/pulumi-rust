# Inline Program

This example demonstrates how to use the Automation API with an `inline` Pulumi program. Unlike traditional Pulumi programs, inline programs do not require a separate project on disk with a `Pulumi.yaml`. The program is just a Rust closure, compiled into the same binary as the automation driver.

The Go original deploys an AWS S3 website. This port keeps the exact same flow (create/select stack, set config, refresh, up with streamed progress, print outputs, destroy on the `destroy` argument) but the inline program is provider-less, so it runs anywhere the Pulumi CLI runs, with no cloud credentials and no plugins:

- reads the `siteName` config value,
- registers a `examples:index:StaticSite` component resource,
- exports a plain output (`websiteUrl`), the site content, and a secret output (`deployToken`).

## Prerequisites

1. The Pulumi CLI on `PATH` ([install](https://www.pulumi.com/docs/install/)).
2. A state backend. The commands below use a throwaway local file backend, so no Pulumi Cloud account is needed.

## Run

Deploying and running the program is just `cargo run`. No invocation through the Pulumi CLI is required:

```shell
$ export PULUMI_BACKEND_URL=file://$(mktemp -d)
$ export PULUMI_CONFIG_PASSPHRASE=test
$ cargo run
Created/Selected stack "dev"
Successfully set config
Starting refresh
Refresh succeeded!
Starting update
    create pulumi:pulumi:Stack inline-program-dev...
    create examples:index:StaticSite hello-world...
    create examples:index:StaticSite hello-world done
    create pulumi:pulumi:Stack inline-program-dev done
    resources: create 2
Update succeeded!
URL: https://hello-world.example.com
deployToken is secret: true
```

To destroy the stack when you are done, invoke the program with an additional `destroy` argument (in the same shell, so it targets the same backend):

```shell
$ cargo run -- destroy
Created/Selected stack "dev"
Successfully set config
Starting refresh
Refresh succeeded!
Starting stack destroy
    delete examples:index:StaticSite hello-world...
    delete examples:index:StaticSite hello-world done
    delete pulumi:pulumi:Stack inline-program-dev...
    delete pulumi:pulumi:Stack inline-program-dev done
    resources: delete 2
Stack successfully destroyed and removed
```

The destroy path also removes the stack, so the next `cargo run` starts clean.
