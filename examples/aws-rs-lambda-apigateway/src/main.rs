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
        // The execution role. `assume_role_policy` is required, so the
        // generator does not derive `Default` for `RoleArgs` and Rust needs
        // every field named — the ones this program leaves alone are `None`.
        let role = pulumi_aws::iam::Role::new(
            &ctx,
            "api-handler-role",
            pulumi_aws::iam::RoleArgs {
                assume_role_policy: pulumi::pv::string(ASSUME_ROLE_POLICY).cast(),
                description: Some(
                    pulumi::pv::string("Execution role for the Rust example's API handler").cast(),
                ),

                force_detach_policies: None,
                // The basic-execution policy is attached as its own resource
                // below rather than listed here: `managed_policy_arns` is
                // exclusive, and setting it would detach anything attached
                // out of band.
                inline_policies: None,
                managed_policy_arns: None,
                max_session_duration: None,
                // Unset so the provider auto-names the role from the Pulumi
                // resource name plus a random suffix.
                name: None,
                name_prefix: None,
                path: None,
                permissions_boundary: None,
                tags: None,
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
                role: role.name().cast(),
                policy_arn: pulumi::pv::string(BASIC_EXECUTION_POLICY_ARN).cast(),
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
                role: role.arn().cast(),
                code: Some(pulumi::pv::file_archive(pulumi::pv::string(APP_DIR)).cast()),
                // `<file>.<exported member>` — `app/index.js` exports
                // `handler`.
                handler: Some(pulumi::pv::string("index.handler").cast()),
                runtime: Some(pulumi::pv::string("nodejs20.x").cast()),
                memory_size: Some(pulumi::pv::number(128.0).cast()),
                timeout: Some(pulumi::pv::number(10.0).cast()),
                description: Some(
                    pulumi::pv::string("Handles every route of the HTTP API.").cast(),
                ),

                architectures: None,
                capacity_provider_config: None,
                code_sha256: None,
                code_signing_config_arn: None,
                dead_letter_config: None,
                durable_config: None,
                environment: None,
                ephemeral_storage: None,
                file_system_config: None,
                image_config: None,
                // `image_uri`, `package_type`, and the `s3*` inputs are the
                // other two ways to supply code — a container image, or a
                // zip already in S3. This example uploads the archive.
                image_uri: None,
                kms_key_arn: None,
                layers: None,
                logging_config: None,
                name: None,
                package_type: None,
                publish: None,
                publish_to: None,
                region: None,
                replace_security_groups_on_destroy: None,
                replacement_security_group_ids: None,
                reserved_concurrent_executions: None,
                s3bucket: None,
                s3key: None,
                s3object_version: None,
                skip_destroy: None,
                snap_start: None,
                source_code_hash: None,
                source_kms_key_arn: None,
                tags: None,
                tenancy_config: None,
                tracing_config: None,
                use_resource_timeout_for_propagation: None,
                vpc_config: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // An HTTP API — the lighter, cheaper API Gateway flavour. The
        // `protocol_type` input is required.
        let api = pulumi_aws::apigatewayv2::Api::new(
            &ctx,
            "api",
            pulumi_aws::apigatewayv2::ApiArgs {
                protocol_type: pulumi::pv::string("HTTP").cast(),
                description: Some(
                    pulumi::pv::string("A serverless HTTP API deployed from Rust.").cast(),
                ),

                api_key_selection_expression: None,
                // `body` imports an OpenAPI document instead of declaring
                // routes as resources; this program declares them.
                body: None,
                cors_configuration: None,
                credentials_arn: None,
                disable_execute_api_endpoint: None,
                fail_on_warnings: None,
                ip_address_type: None,
                name: None,
                region: None,
                // `route_key` and `target` are the quick-create shorthand
                // for a one-route API. The route and integration below are
                // the explicit form of the same thing.
                route_key: None,
                route_selection_expression: None,
                tags: None,
                target: None,
                version: None,
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
                api_id: api.id().cast(),
                integration_type: pulumi::pv::string("AWS_PROXY").cast(),
                integration_uri: Some(handler.invoke_arn().cast()),
                // API Gateway always POSTs to the Lambda invoke endpoint,
                // whatever method the caller used.
                integration_method: Some(pulumi::pv::string("POST").cast()),
                // Version 2.0 of the event shape, which is what a modern
                // Node.js handler expects.
                payload_format_version: Some(pulumi::pv::string("2.0").cast()),
                description: Some(pulumi::pv::string("Lambda proxy integration").cast()),

                connection_id: None,
                connection_type: None,
                content_handling_strategy: None,
                credentials_arn: None,
                integration_subtype: None,
                passthrough_behavior: None,
                region: None,
                request_parameters: None,
                request_templates: None,
                response_parameters: None,
                template_selection_expression: None,
                timeout_milliseconds: None,
                tls_config: None,
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
                api_id: api.id().cast(),
                route_key: pulumi::pv::string("$default").cast(),
                target: Some(
                    pulumi::pv::concat(vec![
                        pulumi::pv::string("integrations/"),
                        integration.id().cast(),
                    ])
                    .cast(),
                ),

                api_key_required: None,
                authorization_scopes: None,
                authorization_type: None,
                authorizer_id: None,
                model_selection_expression: None,
                operation_name: None,
                region: None,
                request_models: None,
                request_parameters: None,
                route_response_selection_expression: None,
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
                api_id: api.id().cast(),
                name: Some(pulumi::pv::string("$default").cast()),
                auto_deploy: Some(pulumi::pv::bool(true).cast()),
                description: Some(pulumi::pv::string("Auto-deployed default stage").cast()),

                access_log_settings: None,
                client_certificate_id: None,
                default_route_settings: None,
                // Unset because `auto_deploy` manages deployments.
                deployment_id: None,
                region: None,
                route_settings: None,
                stage_variables: None,
                tags: None,
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
                action: pulumi::pv::string("lambda:InvokeFunction").cast(),
                function: handler.name().cast(),
                principal: pulumi::pv::string("apigateway.amazonaws.com").cast(),
                source_arn: Some(
                    pulumi::pv::concat(vec![
                        api.execution_arn().cast(),
                        pulumi::pv::string("/*/*"),
                    ])
                    .cast(),
                ),

                event_source_token: None,
                function_url_auth_type: None,
                invoked_via_function_url: None,
                principal_org_id: None,
                qualifier: None,
                region: None,
                source_account: None,
                statement_id: None,
                statement_id_prefix: None,
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
