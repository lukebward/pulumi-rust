// Copyright 2025, Pulumi Corporation.  All rights reserved.
//
// Port of sdk/go/pulumi-language-go/testdata/policies/config.

use pulumi::{EnforcementLevel, Policy, PolicyPack, PropertyValue, ResourceValidationArgs};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

async fn run() -> pulumi::Result<()> {
    let pack = PolicyPack::new(
        "config",
        "2.0.0",
        EnforcementLevel::Mandatory,
        vec![Policy::resource_validation(
            "allowed",
            "Verifies properties",
            EnforcementLevel::Mandatory,
            |args: ResourceValidationArgs| {
                Box::pin(async move {
                    if args.resource.type_ != "simple:index:Resource" {
                        return Ok(());
                    }

                    let Some(PropertyValue::Bool(expected)) = args.config.get("value") else {
                        return Ok(());
                    };
                    if let Some(PropertyValue::Bool(actual)) = args.resource.properties.get("value") {
                        if actual != expected {
                            args.manager.report_violation(format!("Property was {actual}"), "");
                        }
                    }
                    Ok(())
                })
            },
        )],
    )?;

    pulumi::policy_main(pack).await
}
