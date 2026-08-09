[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/gcp-rs-functions)

# Deploy a Google Cloud Function

An HTTP-triggered [Google Cloud Function](https://cloud.google.com/functions)
running JavaScript on the `nodejs20` runtime. The program stages the contents
of the local `function/` directory into a Cloud Storage bucket as a zip —
built with `pulumi::pv::file_archive` over the directory — and points a
first-generation `gcp:cloudfunctions:Function` at that object, with `handler`
as its entry point. A `gcp:cloudfunctions:FunctionIamMember` grants
`roles/cloudfunctions.invoker` to `allUsers` so the URL can be called without
credentials, and the function's `https_trigger_url` comes back as a stack
output.

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
5. Enable the APIs this example uses on your project — Cloud Functions builds
   go through Cloud Build and land in Artifact Registry:

   ```bash
   $ gcloud services enable \
       cloudfunctions.googleapis.com \
       cloudbuild.googleapis.com \
       artifactregistry.googleapis.com \
       storage.googleapis.com
   ```

**The GCP SDK is not checked in.** `Cargo.toml` points `pulumi_gcp` at
`./sdks/gcp/rust`, which does not exist until you run the `pulumi package gen-sdk`
command in step 3 below. The crate does not build before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init functions-dev
    ```

1.  Set the GCP project and region:

    ```bash
    $ pulumi config set gcp:project $(gcloud config get-value project)
    $ pulumi config set gcp:region us-central1
    ```

    The program leaves `project` and `region` unset on the `Function` itself,
    so both come from this provider configuration.

1.  Generate the GCP provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk gcp@9.33.0 --language rust --out ./sdks/gcp
    ```

    The generated crate's own `Cargo.toml` depends on `pulumi = "0.1"`,
    which is not published yet; repoint it at this repository:

    ```toml
    # in ./sdks/gcp/rust/Cargo.toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

    The version is pinned because the property names in `src/main.rs` were
    checked against that schema. Every generated args struct derives
    `Default`, so a provider version that adds an optional input will not
    break this program; one that renames or removes an input still will.

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue. The first deployment waits on a
    Cloud Build run, so it takes a couple of minutes.

    ```bash
    $ pulumi up
    Updating (functions-dev)

         Type                                    Name                       Status
     +   pulumi:pulumi:Stack                     gcp-rs-functions-functions-dev  created
     +   ├─ gcp:storage:Bucket                   function-source            created
     +   ├─ gcp:storage:BucketObject             function-source            created
     +   ├─ gcp:cloudfunctions:Function          greeting                   created
     +   └─ gcp:cloudfunctions:FunctionIamMember invoker                    created

    Outputs:
        function_name: "greeting-***"
        function_url:  "https://us-central1-***.cloudfunctions.net/greeting-***"

    Resources:
        + 5 created

    Duration: ***
    ```

1.  The stack outputs name the function and its URL:

    ```bash
    $ pulumi stack output
    Current stack outputs (2):
        OUTPUT         VALUE
        function_name  greeting-***
        function_url   https://us-central1-***.cloudfunctions.net/greeting-***
    ```

1.  Call the function. The IAM binding means no credentials are needed:

    ```bash
    $ curl -sS $(pulumi stack output functionUrl)
    Hello, world! This function was deployed with Pulumi and Rust.

    $ curl -sS "$(pulumi stack output functionUrl)?name=Pulumi"
    Hello, Pulumi! This function was deployed with Pulumi and Rust.
    ```

    `gcloud` can confirm what actually got deployed:

    ```bash
    $ gcloud functions describe $(pulumi stack output functionName) \
        --region $(pulumi config get gcp:region) \
        --format 'value(runtime, entryPoint, status)'
    nodejs20  handler  ACTIVE
    ```

1.  Edit the greeting in `function/index.js` and run `pulumi up` again. The
    new source is uploaded under a new object name and the function is
    redeployed with it:

    ```bash
    $ pulumi up
        ~ gcp:storage:BucketObject     function-source  replaced
        ~ gcp:cloudfunctions:Function  greeting         updated

    $ curl -sS $(pulumi stack output functionUrl)
    Howdy, world! ...
    ```

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm functions-dev
    ```

## A note on redeploying changed source

A Cloud Function records the *name* of the object holding its source, not the
object's contents. Uploading new bytes under the same name therefore leaves
the `Function` resource with byte-for-byte identical inputs, and the deployed
function keeps running the old code — a trap that catches most first attempts
at this example in any language.

`src/main.rs` avoids it by fingerprinting the sources and folding the result
into the object name, so the name changes whenever the code does:

- `function_files()` lists `function/` with `std::fs::read_dir`. Which files
  exist is ordinary local data, known before the program says anything to the
  engine, so it is a plain Rust function rather than anything output-shaped.
- Each path and its contents are fed through `pulumi::pv::read_file`,
  concatenated with `pulumi::pv::concat`, and hashed with
  `pulumi::pv::sha1_hex`.
- The digest is interpolated into the `BucketObject`'s `name` input, giving
  `function-source-***.zip`.

Because only the object's GCS `name` varies and the Pulumi resource name
stays `function-source`, URNs are stable from run to run: an edit replaces
the object in place in the stack rather than churning the resource graph.

The listing is deliberately shallow — `function/` is flat. A nested source
tree, or one with a `node_modules` directory, wants a recursive walk in
`function_files()` so that changes below the top level are fingerprinted too.

## A note on public access

`allUsers` on `roles/cloudfunctions.invoker` makes the endpoint reachable by
anyone who knows the URL, which is what makes the `curl` step above work
without credentials. Organizations with the `iam.allowedPolicyMemberDomains`
constraint in force reject that binding, and `pulumi up` fails with a policy
violation on the `invoker` resource. Where that applies, drop the
`FunctionIamMember` and call the function with an identity token instead:

```bash
$ curl -sS -H "Authorization: Bearer $(gcloud auth print-identity-token)" \
    $(pulumi stack output functionUrl)
```
