// Copyright 2025, Pulumi Corporation.  All rights reserved.
//
// Port of sdk/go/pulumi-language-go/testdata/policies/dryrun. The Go pack
// reads `pctx.DryRun()`; the same bit arrives here on `args.stack.dry_run`.

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
        "dryrun",
        "1.0.0",
        EnforcementLevel::Advisory,
        vec![Policy::resource_validation(
            "dry",
            "Verifies properties are true on dryrun",
            EnforcementLevel::Mandatory,
            |args: ResourceValidationArgs| {
                Box::pin(async move {
                    if args.resource.type_ != "simple:index:Resource" {
                        return Ok(());
                    }

                    if args.stack.dry_run {
                        if let Some(PropertyValue::Bool(false)) =
                            args.resource.properties.get("value")
                        {
                            args.manager.report_violation("This is a test error", "");
                        }
                    }
                    Ok(())
                })
            },
        )],
    )?;

    pulumi::policy_main(pack).await
}
