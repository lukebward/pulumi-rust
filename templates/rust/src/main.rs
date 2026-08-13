fn main() {
    pulumi::run(|ctx| async move {
        // Read configuration, falling back to a default.
        let name = ctx
            .config()
            .get_string_or("name", pulumi::PropertyValue::String("world".to_string()));

        // Export a stack output.
        ctx.export(
            "greeting",
            pulumi::pv::concat(vec![
                pulumi::pv::string("Hello, "),
                name,
                pulumi::pv::string("!"),
            ]),
        );

        Ok(())
    });
}
