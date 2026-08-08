# Examples

Runnable Pulumi programs written in Rust. Each is a standalone crate that
depends on the core SDK by path, so it builds straight from a checkout.

| Example | What it shows |
|---|---|
| [`config-and-outputs`](./config-and-outputs) | Reading required and optional configuration, secrets, and exporting stack outputs. Nothing to install — the fastest way to see a program run. |
| [`component`](./component) | Grouping child resources behind a component resource with its own inputs and outputs. Also a readable stand-in for what the program generator emits for a PCL `component` block. |
| [`random-password`](./random-password) | Using a generated provider SDK, passing an output of one resource into another, and the `replaceWith` resource option. |

## Running one

```sh
cd config-and-outputs
pulumi stack init dev
pulumi config set greeting Hello
pulumi config set --secret apiKey s3cret
pulumi up
```

`random-password` needs its provider SDK generated first:

```sh
cd random-password
pulumi package gen-sdk random --language rust --out ./sdks
pulumi up
```

## Starting a new project

Copy [`../templates/rust`](../templates/rust), replacing `${PROJECT}` and
`${DESCRIPTION}`, and point the `pulumi` dependency at your checkout of this
repository.
