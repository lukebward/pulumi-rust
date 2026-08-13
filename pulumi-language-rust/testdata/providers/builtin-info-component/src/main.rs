//! The `builtin-info-component` provider used by the
//! `provider-builtin-info-component` conformance test.
//!
//! It serves one component, `builtin-info-component:index:BuiltinInfo`, which
//! takes no inputs and reports the deployment information the SDK knows about
//! the stack it is constructing into.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The package schema. The conformance loader binds the test's PCL against
/// this and generates the program's SDK from it. `isComponent` is what makes a
/// program register `BuiltinInfo` remotely, i.e. through this provider's
/// Construct.
const SCHEMA: &str = r#"{
  "name": "builtin-info-component",
  "version": "37.0.0",
  "resources": {
    "builtin-info-component:index:BuiltinInfo": {
      "type": "object",
      "isComponent": true,
      "properties": {
        "organization": { "type": "string" },
        "project": { "type": "string" },
        "stack": { "type": "string" },
        "isDryRun": { "type": "boolean" }
      }
    }
  }
}"#;

/// The boxed future a construct callback returns.
type ConstructFuture =
    Pin<Box<dyn Future<Output = pulumi::Result<pulumi::ConstructResult>> + Send>>;

#[tokio::main]
async fn main() {
    let result = pulumi::component_provider_host(pulumi::ComponentProviderOptions {
        name: "builtin-info-component".to_string(),
        version: "37.0.0".to_string(),
        schema: SCHEMA.to_string(),
        construct: Arc::new(|args: pulumi::ConstructArgs| -> ConstructFuture {
            Box::pin(construct(args))
        }),
    })
    .await;
    if let Err(err) = result {
        eprintln!("builtin-info-component: {err}");
        std::process::exit(1);
    }
}

async fn construct(args: pulumi::ConstructArgs) -> pulumi::Result<pulumi::ConstructResult> {
    if args.type_ != "builtin-info-component:index:BuiltinInfo" {
        return Err(pulumi::Error::new(format!(
            "unknown resource type {}",
            args.type_
        )));
    }

    let component = args.ctx.register_resource(pulumi::RegisterRequest {
        type_: args.type_.clone(),
        name: args.name.clone(),
        custom: false,
        remote: false,
        version: "37.0.0".to_string(),
        plugin_download_url: String::new(),
        inputs: vec![],
        options: args.options,
        package: None,
        deferred_inputs: vec![],
        required: &[],
    });

    // The engine hands these to Construct, so a component provider sees the
    // same deployment information a program does.
    let outputs = vec![
        (
            "organization".to_string(),
            pulumi::pv::string(args.ctx.organization()),
        ),
        (
            "project".to_string(),
            pulumi::pv::string(args.ctx.project()),
        ),
        ("stack".to_string(), pulumi::pv::string(args.ctx.stack())),
        ("isDryRun".to_string(), pulumi::pv::bool(args.ctx.dry_run())),
    ];

    args.ctx
        .register_resource_outputs(&component, outputs.clone());

    let urn = match component.urn().data().await.value {
        pulumi::PropertyValue::String(urn) => urn,
        other => {
            return Err(pulumi::Error::new(format!(
                "component URN was not a string: {other:?}"
            )))
        }
    };

    let mut state = pulumi::PropertyMap::new();
    for (name, out) in outputs {
        state.insert(name, out.into_property_value().await);
    }

    Ok(pulumi::ConstructResult { urn, state })
}
