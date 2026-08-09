//! Stack references: reading outputs of other stacks.

use crate::context::{Context, RegisterRequest, Resource, ResourceOptions};
use crate::output::{Output, OutputData};
use crate::value::PropertyValue;

/// A reference to another stack, exposing its outputs.
#[derive(Clone)]
pub struct StackReference {
    resource: Resource,
}

impl StackReference {
    /// Reference the stack named by `stack_name`
    /// (`"organization/project/stack"`).
    pub fn new(
        ctx: &Context,
        name: &str,
        stack_name: Output<PropertyValue>,
        options: ResourceOptions,
    ) -> StackReference {
        let resource = ctx.register_resource(RegisterRequest {
            type_: "pulumi:pulumi:StackReference".to_string(),
            name: name.to_string(),
            custom: true,
            remote: false,
            version: String::new(),
            plugin_download_url: String::new(),
            inputs: vec![("name".to_string(), stack_name)],
            options,
            package: None,
            deferred_inputs: vec![],
            required: &[],
        });
        StackReference { resource }
    }

    pub fn pulumi_resource(&self) -> &Resource {
        &self.resource
    }

    /// Raw access to an output property of the stack-reference resource.
    pub fn output(&self, name: &str) -> Output<PropertyValue> {
        self.resource.output(name)
    }

    pub fn urn(&self) -> Output<String> {
        self.resource.urn()
    }

    pub fn id(&self) -> Output<String> {
        self.resource.id()
    }

    /// Fetch a single output of the referenced stack. Values named by the
    /// stack's `secretOutputNames` are marked secret.
    pub fn get_output(&self, key: Output<PropertyValue>) -> Output<PropertyValue> {
        let value = crate::ops::index(self.resource.output("outputs"), key.clone());
        let secret_names = self.resource.output("secretOutputNames");
        Output::from_data_future(async move {
            let d = value.data().await;
            let key = key.data().await;
            let names = secret_names.data().await;
            let is_secret = match (&key.value, &names.value) {
                (PropertyValue::String(k), PropertyValue::Array(names)) => {
                    names.iter().any(|n| matches!(n, PropertyValue::String(s) if s == k))
                }
                _ => false,
            };
            OutputData { value: d.value, secret: d.secret || is_secret, deps: d.deps }
        })
    }
}
