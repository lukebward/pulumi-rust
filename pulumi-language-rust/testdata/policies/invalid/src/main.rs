// Copyright 2025, Pulumi Corporation.  All rights reserved.
//
// Port of sdk/go/pulumi-language-go/testdata/policies/invalid. The pack is
// deliberately invalid: "all" is a reserved policy name, so the pack must
// fail to start rather than serve.
//
// `PolicyPack::new` rejects the reserved name, exactly as the Go pack's
// `policyx.NewPolicyPack` does, so the pack just builds itself and reports
// whatever the constructor says.

use pulumi::{EnforcementLevel, Error, Policy, PolicyPack, ResourceValidationArgs};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

async fn run() -> pulumi::Result<()> {
    let pack = PolicyPack::new(
        "invalid-policy",
        "1.0.0",
        EnforcementLevel::Advisory,
        vec![Policy::resource_validation(
            "all",
            "Invalid policy name",
            EnforcementLevel::Advisory,
            |_args: ResourceValidationArgs| {
                Box::pin(async move { Err(Error::new("Should never run.")) })
            },
        )],
    )?;

    pulumi::policy_main(pack).await
}
