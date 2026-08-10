//! Creating resources from a generated provider SDK.
//!
//! Generate the SDK the program depends on, then run it:
//!
//! ```sh
//! rm -rf ./sdks
//! pulumi package gen-sdk random@4.18.4 --language rust --out ./sdks/random
//!
//! # The generated crate declares `pulumi = "0.1"`, which is not published,
//! # so repoint it at this repository — otherwise cargo cannot resolve the
//! # dependency and nothing builds:
//! #     in ./sdks/random/rust/Cargo.toml
//! #     pulumi = { path = "../../../../../sdk/rust/pulumi" }
//!
//! pulumi up
//! ```

fn main() {
    pulumi::run(|ctx| async move {
        let length = ctx
            .config()
            .get_int_or("length", pulumi::PropertyValue::Number(16.0));

        let password = pulumi_random::RandomPassword::new(
            &ctx,
            "password",
            pulumi_random::RandomPasswordArgs {
                length: Some(length.cast()),
                special: Some(pulumi::pv::bool(true).cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions::default(),
        );

        // A resource output feeding another resource's input makes the
        // engine order the two, and carries the dependency into state.
        let pet = pulumi_random::RandomPet::new(
            &ctx,
            "pet",
            pulumi_random::RandomPetArgs {
                length: Some(pulumi::pv::number(2.0).cast()),
                ..Default::default()
            },
            pulumi::ResourceOptions {
                // Recreate the pet whenever the password is replaced.
                replace_with: vec![password.pulumi_resource().clone()],
                ..Default::default()
            },
        );

        ctx.export("petName", pet.id().cast::<pulumi::PropertyValue>());
        // The provider marks this output secret, so it is redacted.
        ctx.export("password", password.result().cast::<pulumi::PropertyValue>());

        Ok(())
    });
}
