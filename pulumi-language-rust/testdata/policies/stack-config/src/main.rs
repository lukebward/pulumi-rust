// Copyright 2025, Pulumi Corporation.  All rights reserved.
//
// Port of sdk/go/pulumi-language-go/testdata/policies/stack-config.
//
// The pack is derived from the stack's `value` configuration: the policy's
// name and description both embed it. `policy_main_with` builds the pack from
// the stack configuration the engine sends before it asks what policies
// exist, which is what `policyx.Main`'s factory does on the Go side.

use pulumi::{
    EnforcementLevel, Error, Policy, PolicyPack, PropertyValue, ResourceValidationArgs, StackInfo,
};

/// Read a required boolean out of the stack's configuration. Stack config
/// arrives keyed `<project>:<key>`, which is what `config.New(pctx, "")`
/// reads on the Go side.
fn require_bool(stack: &StackInfo, key: &str) -> pulumi::Result<bool> {
    let namespaced = format!("{}:{}", stack.project, key);
    let raw = stack
        .config
        .get(&namespaced)
        .or_else(|| stack.config.get(key))
        .ok_or_else(|| {
            Error::new(format!(
                "missing required configuration variable '{namespaced}'"
            ))
        })?;
    match raw.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(Error::new(format!(
            "configuration variable '{namespaced}' is not a boolean: {other}"
        ))),
    }
}

fn build(stack: StackInfo) -> pulumi::Result<PolicyPack> {
    let value = require_bool(&stack, "value")?;

    PolicyPack::new(
        "stack-config",
        "2.0.0",
        EnforcementLevel::Mandatory,
        vec![Policy::resource_validation(
            format!("validate-{value}"),
            format!("Verifies property is {value}"),
            EnforcementLevel::Mandatory,
            move |args: ResourceValidationArgs| {
                Box::pin(async move {
                    if args.resource.type_ != "simple:index:Resource" {
                        return Ok(());
                    }

                    if let Some(PropertyValue::Bool(actual)) = args.resource.properties.get("value")
                    {
                        if *actual != value {
                            args.manager
                                .report_violation(format!("Property was {actual}"), "");
                        }
                    }
                    Ok(())
                })
            },
        )],
    )
}

#[tokio::main]
async fn main() {
    if let Err(err) = pulumi::policy_main_with(build).await {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
