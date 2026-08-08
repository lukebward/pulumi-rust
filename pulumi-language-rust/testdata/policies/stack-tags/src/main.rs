// Copyright 2025, Pulumi Corporation.  All rights reserved.
//
// Port of sdk/go/pulumi-language-go/testdata/policies/stack-tags. The Go pack
// reads `args.StackTags`; the same map arrives here on `args.stack.tags`.
//
// The Go pack parses the tag with `json.Unmarshal` into a bool; this pack
// matches the two JSON boolean literals directly rather than taking a JSON
// dependency, which accepts exactly the same inputs.

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
        "stack-tags",
        "2.0.0",
        EnforcementLevel::Mandatory,
        vec![Policy::resource_validation(
            "allowed",
            "Verifies property equals the stack tag value",
            EnforcementLevel::Mandatory,
            |args: ResourceValidationArgs| {
                Box::pin(async move {
                    if args.resource.type_ != "simple:index:Resource" {
                        return Ok(());
                    }

                    let Some(tag) = args.stack.tags.get("value") else {
                        args.manager.report_violation("Stack tag 'value' is required", "");
                        return Ok(());
                    };
                    let expected = match tag.trim() {
                        "true" => true,
                        "false" => false,
                        _ => {
                            args.manager.report_violation(
                                format!("Stack tag 'value' must be a boolean, got '{tag}'"),
                                "",
                            );
                            return Ok(());
                        }
                    };

                    if let Some(PropertyValue::Bool(actual)) = args.resource.properties.get("value")
                    {
                        if *actual != expected {
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
