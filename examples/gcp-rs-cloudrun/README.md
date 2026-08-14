[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/pulumi-labs/pulumi-rust/tree/main/examples/gcp-rs-cloudrun)

# Run a Container on Google Cloud Run

A container served by [Cloud Run](https://cloud.google.com/run), Google's
managed platform for running stateless containers behind an HTTPS endpoint.
The program creates a `gcp:cloudrun:Service` in a configurable region — the
default is `us-central1` — whose `template.spec.containers` block points at a
public image, and a `gcp:cloudrun:IamMember` granting `roles/run.invoker` to
`allUsers` so the endpoint answers without credentials. The service's URL
comes back as a stack output.

This is the Rust version of
[`gcp-ts-cloudrun`](https://github.com/pulumi/examples/tree/master/gcp-ts-cloudrun).

## Prerequisites

1. [Install Pulumi](https://www.pulumi.com/docs/install/).
2. [Install Rust](https://rustup.rs/) (1.85 or newer) — `cargo` builds the
   program.
3. Build the experimental Rust language plugin from this repository and put
   it on your `PATH`, so that `runtime: rust` resolves:

   ```bash
   $ (cd ../../pulumi-language-rust && go build .)
   $ export PATH="$(cd ../../pulumi-language-rust && pwd):$PATH"
   ```

4. [Configure Google Cloud credentials](https://www.pulumi.com/registry/packages/gcp/installation-configuration/),
   for example with `gcloud auth application-default login`.
5. Enable the Cloud Run API on your project:

   ```bash
   $ gcloud services enable run.googleapis.com
   ```

**The GCP SDK is not checked in.** `Cargo.toml` points `pulumi_gcp` at
`./sdks/gcp/rust`, which does not exist until you run the
`pulumi package gen-sdk` command in step 4 below. The crate does not build
before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init cloudrun-dev
    ```

1.  Set the GCP project:

    ```bash
    $ pulumi config set gcp:project $(gcloud config get-value project)
    ```

    The program leaves `project` unset on both resources, so it comes from
    this provider configuration.

1.  Optionally choose a region and an image. The defaults are `us-central1`
    and Google's `gcr.io/cloudrun/hello` sample container:

    ```bash
    $ pulumi config set region us-east1
    $ pulumi config set image gcr.io/my-project/my-app:v1
    ```

    Note the key is `region`, not `gcp:region`. A Cloud Run service requires
    an explicit `location`, so this program reads its own project-scoped
    config key rather than the provider's.

1.  Generate the GCP provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk gcp@9.33.0 --language rust --out ./sdks/gcp
    ```

    The `pulumi` crate is not published to crates.io yet, so edit the
    dependency in the generated `sdks/gcp/rust/Cargo.toml` to point at this
    repository's copy of the core SDK:

    ```toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

    The version is pinned because the property names in `src/main.rs` were
    checked against that schema. Every generated args struct derives
    `Default`, so a provider version that adds an optional input will not
    break this program; one that renames or removes an input still will.

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue.

    ```bash
    $ pulumi up
    Updating (cloudrun-dev)

         Type                      Name                   Status
     +   pulumi:pulumi:Stack       gcp-rs-cloudrun-cloudrun-dev  created
     +   ├─ gcp:cloudrun:Service   hello                  created
     +   └─ gcp:cloudrun:IamMember invoker                created

    Outputs:
        serviceName: "hello-***"
        serviceUrl:  "https://hello-***-uc.a.run.app"

    Resources:
        + 3 created

    Duration: ***
    ```

1.  The stack outputs name the service and its URL:

    ```bash
    $ pulumi stack output
    Current stack outputs (2):
        OUTPUT       VALUE
        serviceName  hello-***
        serviceUrl   https://hello-***-uc.a.run.app
    ```

1.  Call it. The IAM binding means no credentials are needed:

    ```bash
    $ curl -sS $(pulumi stack output serviceUrl) | head -3
    <!DOCTYPE html>
    <html>
      <head>
    ```

    `gcloud` can confirm what actually got deployed:

    ```bash
    $ gcloud run services describe $(pulumi stack output serviceName) \
        --region us-central1 \
        --format 'value(status.url, status.latestReadyRevisionName)'
    https://hello-***-uc.a.run.app  hello-***-00001-abc
    ```

    (Use whatever `region` is set to, if you changed it. `pulumi config get
    region` errors when the key is unset and the program is running on its
    default.)

1.  Point the service at a different image and run `pulumi up` again. Cloud
    Run rolls out a new revision and shifts traffic to it, and the URL is
    unchanged:

    ```bash
    $ pulumi config set image gcr.io/google-samples/hello-app:1.0
    $ pulumi up
        ~ gcp:cloudrun:Service  hello  updated
    ```

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm cloudrun-dev
    ```

## A note on reading the URL out of the outputs

Knative — and therefore Cloud Run — reports the endpoint in the service's
`status` block, and the GCP provider surfaces that block as a *list* named
`statuses`. So the URL is not `service.url()`; it is the `url` key of the
first element of `statuses`, and the generated accessor hands back
`Output<Vec<CloudrunServiceStatus>>`:

```rust
service.statuses().index(0usize).index("url")
```

`Output::index` (in `sdk/rust/pulumi/src/output.rs`) takes anything that
converts into a `PropIndex` — `usize` for an array position, `&str` for an
object key — and returns `Output<PropertyValue>`:

```rust
pub fn index(&self, key: impl Into<PropIndex>) -> Output<PropertyValue>
```

Because it returns a dynamic value, the two calls chain without naming any
intermediate type. Unknownness, secretness, and dependencies propagate
through both steps: on a preview the whole expression is unknown and neither
index is evaluated, and in state the export still records its dependency on
the service.

## A note on public access

`allUsers` on `roles/run.invoker` makes the endpoint reachable by anyone who
knows the URL, which is what makes the `curl` step above work without
credentials. Organizations with the `iam.allowedPolicyMemberDomains`
constraint in force reject that binding, and `pulumi up` fails with a policy
violation on the `invoker` resource. That constraint is applied by default to
organizations created on or after 3 May 2024, so this is the common case for
a recently created account rather than an unusual one. Where it applies, drop
the `IamMember`
and call the service with an identity token instead:

```bash
$ curl -sS -H "Authorization: Bearer $(gcloud auth print-identity-token)" \
    $(pulumi stack output serviceUrl)
```
