// Copyright 2025, Pulumi Corporation.  All rights reserved.
//
// Port of sdk/go/pulumi-language-go/testdata/policies/config-schema.

use std::collections::BTreeMap;

use pulumi::{
    ConfigSchema, EnforcementLevel, Policy, PolicyPack, PropertyValue, ResourceValidationArgs,
};

/// A JSON schema fragment, expressed as a property-value object.
fn fragment(entries: Vec<(&str, PropertyValue)>) -> PropertyValue {
    PropertyValue::Object(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

fn config_schema() -> ConfigSchema {
    let mut properties = BTreeMap::new();
    properties.insert(
        "value".to_string(),
        fragment(vec![("type", PropertyValue::String("boolean".into()))]),
    );
    properties.insert(
        "names".to_string(),
        fragment(vec![
            ("type", PropertyValue::String("array".into())),
            (
                "items",
                fragment(vec![("type", PropertyValue::String("string".into()))]),
            ),
            ("minItems", PropertyValue::Number(1.0)),
        ]),
    );
    ConfigSchema {
        properties,
        required: vec!["value".to_string(), "names".to_string()],
    }
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

async fn run() -> pulumi::Result<()> {
    let pack = PolicyPack::new(
        "config-schema",
        "3.0.0",
        EnforcementLevel::Advisory,
        vec![Policy::resource_validation(
            "validator",
            "Verifies property matches config",
            EnforcementLevel::Advisory,
            |args: ResourceValidationArgs| {
                Box::pin(async move {
                    if args.resource.type_ != "simple:index:Resource" {
                        return Ok(());
                    }

                    let Some(PropertyValue::Bool(expected)) = args.config.get("value") else {
                        return Ok(());
                    };
                    let Some(PropertyValue::Array(names)) = args.config.get("names") else {
                        return Ok(());
                    };
                    let named = names
                        .iter()
                        .any(|n| matches!(n, PropertyValue::String(s) if *s == args.resource.name));

                    if named {
                        if let Some(PropertyValue::Bool(actual)) =
                            args.resource.properties.get("value")
                        {
                            if actual != expected {
                                args.manager
                                    .report_violation(format!("Property was {actual}"), "");
                            }
                        }
                    }
                    Ok(())
                })
            },
        )
        .with_config_schema(config_schema())],
    )?;

    pulumi::policy_main(pack).await
}
