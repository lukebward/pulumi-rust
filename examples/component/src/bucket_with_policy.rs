//! A component groups child resources under one logical node. Children are
//! parented to the component, so their URNs nest beneath it, and the
//! component publishes its own outputs.
//!
//! This mirrors what the program generator emits for a PCL `component`
//! block, so it is also a useful reference for reading generated code.

pub struct BucketWithPolicyArgs {
    pub public: Option<pulumi::Output<pulumi::PropertyValue>>,
}

impl Default for BucketWithPolicyArgs {
    fn default() -> Self {
        BucketWithPolicyArgs { public: None }
    }
}

pub struct BucketWithPolicy {
    resource: pulumi::Resource,
    url: pulumi::Output<pulumi::PropertyValue>,
}

impl BucketWithPolicy {
    pub async fn new(
        ctx: &pulumi::Context,
        name: &str,
        args: BucketWithPolicyArgs,
        options: pulumi::ResourceOptions,
    ) -> pulumi::Result<BucketWithPolicy> {
        let ctx = pulumi::Context::clone(ctx);
        let public = args
            .public
            .clone()
            .unwrap_or_else(|| pulumi::pv::bool(false));

        // Register the component itself. It is not a custom resource: no
        // provider creates it, it only groups its children.
        let component = ctx.register_resource(pulumi::RegisterRequest {
            type_: "examples:index:BucketWithPolicy".to_string(),
            name: name.to_string(),
            custom: false,
            remote: false,
            version: String::new(),
            plugin_download_url: String::new(),
            inputs: vec![("public".to_string(), public.clone())],
            options,
            package: None,
            deferred_inputs: vec![],
            required: &[],
        });

        // Children are parented to the component. In a real program these
        // would be provider resources (an S3 bucket and its policy); here
        // the shape is what matters.
        let _child_options = pulumi::ResourceOptions {
            parent: Some(component.clone()),
            ..Default::default()
        };

        let url = public.map(|v: pulumi::PropertyValue| match v {
            pulumi::PropertyValue::Bool(true) => {
                pulumi::PropertyValue::String("https://example.com/public".to_string())
            }
            _ => pulumi::PropertyValue::String("s3://example/private".to_string()),
        });

        // Publish the component's outputs; these land in the state file
        // against the component's own URN.
        ctx.register_resource_outputs(&component, vec![("url".to_string(), url.clone())]);

        Ok(BucketWithPolicy { resource: component, url })
    }

    pub fn pulumi_resource(&self) -> &pulumi::Resource {
        &self.resource
    }

    pub fn url(&self) -> pulumi::Output<pulumi::PropertyValue> {
        self.url.clone()
    }
}
