//! Configuration and stack outputs.
//!
//! ```sh
//! pulumi config set greeting Hello
//! pulumi config set --secret apiKey s3cret
//! pulumi config set replicas 3
//! ```

fn main() {
    pulumi::run(|ctx| async move {
        let config = ctx.config();

        // Required: the program fails with a clear error if unset.
        let greeting = config.require_string("greeting")?;

        // Optional, with a default.
        let replicas = config.get_int_or("replicas", pulumi::PropertyValue::Number(1.0));

        // Secrets stay secret: anything derived from apiKey is marked secret
        // in the state file and redacted in the CLI.
        let api_key = config.require_string("apiKey")?;

        ctx.export("greeting", greeting.clone());
        ctx.export("replicas", replicas.clone());

        // Outputs compose; the result is secret because api_key is.
        ctx.export(
            "maskedKey",
            pulumi::pv::concat(vec![pulumi::pv::string("key:"), api_key]),
        );

        // Outputs are futures: map runs once the value is known, and during
        // a preview of an unknown value it is skipped entirely.
        ctx.export(
            "shouted",
            greeting.map(|v: pulumi::PropertyValue| match v {
                pulumi::PropertyValue::String(s) => {
                    pulumi::PropertyValue::String(s.to_uppercase())
                }
                other => other,
            }),
        );

        Ok(())
    });
}
