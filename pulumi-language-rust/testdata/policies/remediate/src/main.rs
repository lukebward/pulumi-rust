// Copyright 2025, Pulumi Corporation.  All rights reserved.
//
// Port of sdk/go/pulumi-language-go/testdata/policies/remediate.

use pulumi::{
    EnforcementLevel, Policy, PolicyPack, PropertyMap, PropertyValue, ResourceRemediationArgs,
};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

async fn run() -> pulumi::Result<()> {
    let pack = PolicyPack::new(
        "remediate",
        "3.0.0",
        EnforcementLevel::Advisory,
        vec![Policy::resource_remediation(
            "fixup",
            "Sets property to config",
            |args: ResourceRemediationArgs| {
                Box::pin(async move {
                    if args.resource.type_ != "simple:index:Resource" {
                        return Ok(None);
                    }

                    let Some(PropertyValue::Bool(expected)) = args.config.get("value") else {
                        return Ok(None);
                    };
                    if let Some(PropertyValue::Bool(actual)) = args.resource.properties.get("value")
                    {
                        if actual != expected {
                            let mut props = PropertyMap::new();
                            props.insert("value".to_string(), PropertyValue::Bool(*expected));
                            return Ok(Some(props));
                        }
                    }
                    Ok(None)
                })
            },
        )],
    )?;

    pulumi::policy_main(pack).await
}
