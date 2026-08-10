//! A serverless HTTP API: API Gateway v2 in front of an AWS Lambda function.
//!
//! The function's code is the local `app/` directory, zipped up and uploaded
//! as an archive asset. An HTTP API routes every request (`$default`) to the
//! function through an `AWS_PROXY` integration, and a `$default` stage with
//! auto-deploy publishes it at a URL with no stage prefix. The stage's
//! invoke URL comes back as a stack output.
//!
//! The program depends on a generated AWS SDK, so generate that first:
//!
//! ```sh
//! pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
//! pulumi up
//! ```

/// The local directory holding the function's code, relative to the project
/// root. `file_archive` over a directory puts that directory's contents at
/// the root of the zip, which is the layout Lambda expects: `index.js` sits
/// at the top level so the `index.handler` handler resolves.
const APP_DIR: &str = "app";

/// Lets the Lambda service assume the function's execution role. This is a
/// trust policy, not a permissions policy — what the function may *do*
/// comes from the managed policy attached below.
const ASSUME_ROLE_POLICY: &str = r#"{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": "sts:AssumeRole",
      "Principal": { "Service": "lambda.amazonaws.com" }
    }
  ]
}"#;

/// The AWS-managed policy every Lambda function wants: permission to create
/// its CloudWatch log group and write log events into it.
const BASIC_EXECUTION_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

fn main() {
    pulumi::run(|ctx| async move {
        // The execution role.
        let role = pulumi_aws::iam::Role::new(
            &ctx,
            "api-handler-role",
            pulumi_aws::iam::RoleArgs {
                assume_role_policy: Some(pulumi::pv::string(ASSUME_ROLE_POLICY).cast()),
                description: Some(
                    pulumi::pv::string("Execution role for the Rust example's API handler").cast(),
                ),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Both inputs are required here, so there is nothing to elide.
        // Feeding the role's own output in makes the engine create the role
        // first and records the dependency in state.
        pulumi_aws::iam::RolePolicyAttachment::new(
            &ctx,
            "api-handler-basic-execution",
            pulumi_aws::iam::RolePolicyAttachmentArgs {
                role: Some(role.name().cast()),
                policy_arn: Some(pulumi::pv::string(BASIC_EXECUTION_POLICY_ARN).cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The function itself. `role` is the one required input, and the
        // code travels to the provider as an archive asset built from the
        // local directory — the path is resolved relative to the project
        // root, and the engine re-zips it whenever the contents change.
        let handler = pulumi_aws::lambda::Function::new(
            &ctx,
            "api-handler",
            pulumi_aws::lambda::FunctionArgs {
                role: Some(role.arn().cast()),
                code: Some(pulumi::pv::file_archive(pulumi::pv::string(APP_DIR)).cast()),
                // `<file>.<exported member>` — `app/index.js` exports
                // `handler`.
                handler: Some(pulumi::pv::string("index.handler").cast()),
                // Runtimes expire. AWS blocked `CreateFunction` on
                // `nodejs20.x` on 1 June 2026 and blocked updates a month
                // later, so a program pinning it simply stopped deploying —
                // and neither the schema nor the compiler says a word.
                runtime: Some(pulumi::pv::string("nodejs22.x").cast()),
                memory_size: Some(pulumi::pv::number(128.0).cast()),
                timeout: Some(pulumi::pv::number(10.0).cast()),
                description: Some(
                    pulumi::pv::string("Handles every route of the HTTP API.").cast(),
                ),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // An HTTP API — the lighter, cheaper API Gateway flavour. The
        // `protocol_type` input is required.
        let api = pulumi_aws::apigatewayv2::Api::new(
            &ctx,
            "api",
            pulumi_aws::apigatewayv2::ApiArgs {
                protocol_type: Some(pulumi::pv::string("HTTP").cast()),
                description: Some(
                    pulumi::pv::string("A serverless HTTP API deployed from Rust.").cast(),
                ),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The integration says what the API talks to. `AWS_PROXY` hands the
        // whole request to Lambda and takes the whole response back, which
        // is why the handler can return a status code and headers itself.
        //
        // `integration_uri` wants the function's *invoke* ARN, not its ARN:
        // the invoke ARN is the `apigateway:.../invocations` URI that API
        // Gateway calls.
        let integration = pulumi_aws::apigatewayv2::Integration::new(
            &ctx,
            "api-integration",
            pulumi_aws::apigatewayv2::IntegrationArgs {
                api_id: Some(api.id().cast()),
                integration_type: Some(pulumi::pv::string("AWS_PROXY").cast()),
                integration_uri: Some(handler.invoke_arn().cast()),
                // API Gateway always POSTs to the Lambda invoke endpoint,
                // whatever method the caller used.
                integration_method: Some(pulumi::pv::string("POST").cast()),
                // Version 2.0 of the event shape, which is what a modern
                // Node.js handler expects.
                payload_format_version: Some(pulumi::pv::string("2.0").cast()),
                description: Some(pulumi::pv::string("Lambda proxy integration").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // `$default` catches every method and path that no other route
        // matches — with no other routes, that is everything. The target is
        // the string `integrations/<integration id>`, so building it from
        // the integration's own id both points at the right integration and
        // orders the two registrations.
        pulumi_aws::apigatewayv2::Route::new(
            &ctx,
            "api-default-route",
            pulumi_aws::apigatewayv2::RouteArgs {
                api_id: Some(api.id().cast()),
                route_key: Some(pulumi::pv::string("$default").cast()),
                target: Some(
                    pulumi::pv::concat(vec![
                        pulumi::pv::string("integrations/"),
                        integration.id().cast(),
                    ])
                    .cast(),
                ),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // A stage is a deployed snapshot of the API. The `$default` stage is
        // special: it is served at the root of the API's endpoint, so the
        // URL has no `/stage` suffix. `auto_deploy` republishes it whenever
        // a route or integration changes, which saves declaring a
        // `Deployment` resource and re-pointing it by hand.
        let stage = pulumi_aws::apigatewayv2::Stage::new(
            &ctx,
            "api-stage",
            pulumi_aws::apigatewayv2::StageArgs {
                api_id: Some(api.id().cast()),
                name: Some(pulumi::pv::string("$default").cast()),
                auto_deploy: Some(pulumi::pv::bool(true).cast()),
                description: Some(pulumi::pv::string("Auto-deployed default stage").cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // Without this, API Gateway's call into the function is rejected
        // with a 500: an integration is permission to *route*, not
        // permission to *invoke*. The source ARN narrows the grant to this
        // API — `<execution arn>/*/*` is every stage and every route of it.
        pulumi_aws::lambda::Permission::new(
            &ctx,
            "api-invoke-permission",
            pulumi_aws::lambda::PermissionArgs {
                action: Some(pulumi::pv::string("lambda:InvokeFunction").cast()),
                function: Some(handler.name().cast()),
                principal: Some(pulumi::pv::string("apigateway.amazonaws.com").cast()),
                source_arn: Some(
                    pulumi::pv::concat(vec![
                        api.execution_arn().cast(),
                        pulumi::pv::string("/*/*"),
                    ])
                    .cast(),
                ),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // The `$default` stage's invoke URL is the API's own endpoint, so
        // this is the address to curl.
        ctx.export("url", stage.invoke_url().cast::<pulumi::PropertyValue>());
        ctx.export("apiId", api.id().cast::<pulumi::PropertyValue>());
        ctx.export(
            "functionName",
            handler.name().cast::<pulumi::PropertyValue>(),
        );

        Ok(())
    });
}
