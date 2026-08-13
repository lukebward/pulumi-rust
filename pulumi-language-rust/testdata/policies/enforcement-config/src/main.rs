// Copyright 2025, Pulumi Corporation.  All rights reserved.
//
// Port of sdk/go/pulumi-language-go/testdata/policies/enforcement-config.

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
        "enforcement-config",
        "3.0.0",
        EnforcementLevel::Advisory,
        vec![Policy::resource_validation(
            "false",
            "Verifies property is false",
            EnforcementLevel::Advisory,
            |args: ResourceValidationArgs| {
                Box::pin(async move {
                    if args.resource.type_ != "simple:index:Resource" {
                        return Ok(());
                    }

                    if let Some(PropertyValue::Bool(actual)) = args.resource.properties.get("value")
                    {
                        if *actual {
                            args.manager
                                .report_violation(format!("Property was {actual}"), "");
                        }
                    }
                    Ok(())
                })
            },
        )],
    )?;

    pulumi::policy_main(pack).await
}
