[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/aws-rs-lambda-apigateway)

# Serverless REST API on AWS Lambda and API Gateway

An HTTP API with no servers to run. The program creates an IAM execution
role for the function and attaches the AWS-managed
`AWSLambdaBasicExecutionRole` policy to it, uploads the local `app/`
directory as the function's deployment package, and puts an
[API Gateway v2 HTTP API](https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api.html)
in front of it: an `AWS_PROXY` integration, a `$default` route that catches
every method and path, and a `$default` stage with auto-deploy so the URL
has no stage prefix. A `aws:lambda:Permission` lets API Gateway actually
invoke the function. The endpoint comes back as the `url` stack output.

The function's code is an ordinary archive asset built from a local
directory, so editing `app/index.js` and re-running `pulumi up` redeploys
the code and nothing else.

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

4. [Configure AWS credentials](https://www.pulumi.com/registry/packages/aws/installation-configuration/),
   for example by setting `AWS_PROFILE` or running `aws configure`.

**The AWS SDK is not checked in.** `Cargo.toml` points `pulumi_aws` at
`./sdks/aws/rust`, which does not exist until you run the `pulumi package gen-sdk`
command in step 3 below. The crate does not build before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init api-testing
    ```

1.  Set the AWS region:

    ```bash
    $ pulumi config set aws:region us-west-2
    ```

1.  Generate the AWS provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
    ```

    Note that `gen-sdk` writes to `<out>/<language>`, so the crate lands in
    `./sdks/aws/rust` — which is the path `Cargo.toml` already points at.
    The generated crate's own `Cargo.toml` depends on `pulumi = "0.1"`,
    which is not published yet; repoint it at this repository:

    ```toml
    # in ./sdks/aws/rust/Cargo.toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

    The version is pinned deliberately. `RoleArgs`, `FunctionArgs`,
    `ApiArgs`, `IntegrationArgs`, `RouteArgs`, `StageArgs` and
    `PermissionArgs` all have required inputs, so the generator does not
    derive `Default` for them and `src/main.rs` names every field
    explicitly — including the ones set to `None`. A different provider
    version can add or remove inputs, in which case `cargo` will name the
    fields to add or drop.

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue.

    ```bash
    $ pulumi up
    Updating (api-testing)

         Type                              Name                          Status
     +   pulumi:pulumi:Stack               aws-rs-lambda-apigateway-***  created
     +   ├─ aws:iam:Role                   api-handler-role              created
     +   ├─ aws:iam:RolePolicyAttachment   api-handler-basic-execution   created
     +   ├─ aws:lambda:Function            api-handler                   created
     +   ├─ aws:apigatewayv2:Api           api                           created
     +   ├─ aws:apigatewayv2:Integration   api-integration               created
     +   ├─ aws:apigatewayv2:Route         api-default-route             created
     +   ├─ aws:apigatewayv2:Stage         api-stage                     created
     +   └─ aws:lambda:Permission          api-invoke-permission         created

    Outputs:
        apiId:        "***"
        functionName: "api-handler-***"
        url:          "https://***.execute-api.us-west-2.amazonaws.com/"

    Resources:
        + 9 created

    Duration: ***
    ```

1.  The stack outputs name the API, the function, and the endpoint:

    ```bash
    $ pulumi stack output
    Current stack outputs (3):
        OUTPUT        VALUE
        apiId         ***
        functionName  api-handler-***
        url           https://***.execute-api.us-west-2.amazonaws.com/
    ```

1.  Call the API. Every path reaches the same handler, because the only
    route is `$default`:

    ```bash
    $ curl -sS $(pulumi stack output url)
    {
      "message": "Hello, world! Served by AWS Lambda, deployed with Pulumi from Rust.",
      "method": "GET",
      "path": "/",
      "time": "***"
    }

    $ curl -sS $(pulumi stack output url)hello/world | head -4
    {
      "message": "Hello, world! Served by AWS Lambda, deployed with Pulumi from Rust.",
      "method": "GET",
      "path": "/hello/world",
    ```

1.  Edit `app/index.js` and run `pulumi up` again: the archive's hash
    changes, so only the `aws:lambda:Function` is updated and the endpoint
    stays the same.

1.  The function's logs go to CloudWatch, under a log group named after the
    function — that is what the basic-execution policy grants:

    ```bash
    $ aws logs tail /aws/lambda/$(pulumi stack output functionName) --since 5m
    ```

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm api-testing
    ```

## Notes

- `integration_uri` takes the function's **invoke ARN**
  (`handler.invoke_arn()`), not its ARN. The invoke ARN is the
  `arn:aws:apigateway:...:lambda:path/.../invocations` URI that API Gateway
  calls; passing the plain function ARN there is the usual first mistake.
- The `aws:lambda:Permission` is not optional. An integration is permission
  to *route* to the function, not permission to *invoke* it, and without the
  permission every request comes back as a 500 with an
  `Internal Server Error` body. `source_arn` is
  `<the API's execution ARN>/*/*` — every stage, every route of this API and
  no other.
- Naming the stage `$default` is what removes the stage prefix from the URL.
  A stage called `dev` would be served at
  `https://***.execute-api.***.amazonaws.com/dev`, and the `url` output
  would need the suffix appended.
- `auto_deploy` on the stage replaces the `aws:apigatewayv2:Deployment`
  resource that the API would otherwise need, along with the dance of
  re-pointing the stage at each new deployment.
