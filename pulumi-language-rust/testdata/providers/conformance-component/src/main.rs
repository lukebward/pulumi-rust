//! The `conformance-component` provider used by the
//! `provider-resource-component` conformance test.
//!
//! It serves one component, `conformance-component:index:Simple`, which takes
//! a boolean `value`, registers a `simple:index:Resource` child holding the
//! negation of that value, and reports `value` unchanged as its own output.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The package schema. The conformance loader binds the test's PCL against
/// this and generates the program's SDK from it, so it must describe the
/// component exactly as the engine will see it. `isComponent` is what makes a
/// program register `Simple` remotely, i.e. through this provider's Construct.
const SCHEMA: &str = r#"{
  "name": "conformance-component",
  "version": "22.0.0",
  "resources": {
    "conformance-component:index:Simple": {
      "type": "object",
      "isComponent": true,
      "properties": {
        "value": { "type": "boolean" }
      },
      "required": ["value"],
      "inputProperties": {
        "value": { "type": "boolean" }
      },
      "requiredInputs": ["value"]
    }
  }
}"#;

/// The boxed future a construct callback returns.
type ConstructFuture =
    Pin<Box<dyn Future<Output = pulumi::Result<pulumi::ConstructResult>> + Send>>;

#[tokio::main]
async fn main() {
    let result = pulumi::component_provider_host(pulumi::ComponentProviderOptions {
        name: "conformance-component".to_string(),
        version: "22.0.0".to_string(),
        schema: SCHEMA.to_string(),
        construct: Arc::new(|args: pulumi::ConstructArgs| -> ConstructFuture {
            Box::pin(construct(args))
        }),
    })
    .await;
    if let Err(err) = result {
        eprintln!("conformance-component: {err}");
        std::process::exit(1);
    }
}

async fn construct(args: pulumi::ConstructArgs) -> pulumi::Result<pulumi::ConstructResult> {
    if args.type_ != "conformance-component:index:Simple" {
        return Err(pulumi::Error::new(format!("unknown resource type {}", args.type_)));
    }

    let value = pulumi::Output::from_value(
        args.inputs.get("value").cloned().unwrap_or(pulumi::PropertyValue::Null),
    );

    let component = args.ctx.register_resource(pulumi::RegisterRequest {
        type_: args.type_.clone(),
        name: args.name.clone(),
        custom: false,
        remote: false,
        version: "22.0.0".to_string(),
        plugin_download_url: String::new(),
        inputs: vec![("value".to_string(), value.clone())],
        options: args.options,
        package: None,
        deferred_inputs: vec![],
        required: &[],
    });

    // The child holds the negation of the component's input, so the test can
    // tell the two registrations apart.
    let negated: pulumi::Output<bool> = value.cast::<bool>().map(|v: bool| !v);
    let _child = pulumi_simple::Resource::new(
        &args.ctx,
        &format!("{}-child", args.name),
        pulumi_simple::ResourceArgs { value: negated },
        pulumi::ResourceOptions { parent: Some(component.clone()), ..Default::default() },
    );

    args.ctx.register_resource_outputs(&component, vec![("value".to_string(), value.clone())]);

    let urn = match component.urn().data().await.value {
        pulumi::PropertyValue::String(urn) => urn,
        other => {
            return Err(pulumi::Error::new(format!("component URN was not a string: {other:?}")))
        }
    };

    let mut state = pulumi::PropertyMap::new();
    state.insert("value".to_string(), value.into_property_value().await);

    Ok(pulumi::ConstructResult { urn, state })
}
