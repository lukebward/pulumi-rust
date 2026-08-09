//! A component resource: a named group of resources with its own inputs and
//! outputs, appearing as a single node in `pulumi up` and in the state.

mod bucket_with_policy;

use bucket_with_policy::{BucketWithPolicy, BucketWithPolicyArgs};

fn main() {
    pulumi::run(|ctx| async move {
        let public = BucketWithPolicy::new(
            &ctx,
            "public",
            BucketWithPolicyArgs { public: Some(pulumi::pv::bool(true)) },
            pulumi::ResourceOptions::default(),
        )
        .await?;

        let private = BucketWithPolicy::new(
            &ctx,
            "private",
            BucketWithPolicyArgs { public: Some(pulumi::pv::bool(false)) },
            pulumi::ResourceOptions::default(),
        )
        .await?;

        ctx.export("publicUrl", public.url());
        ctx.export("privateUrl", private.url());

        // `pulumi_resource()` hands back the underlying component resource,
        // which is what you pass as a `parent` to nest something else under
        // it — and what the generated SDKs expose for the same purpose.
        ctx.export("publicUrn", public.pulumi_resource().urn().cast());

        Ok(())
    });
}
