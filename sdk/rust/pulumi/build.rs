use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("proto");
    let files = [
        "pulumi/resource.proto",
        "pulumi/engine.proto",
        "pulumi/provider.proto",
        "pulumi/plugin.proto",
        "pulumi/alias.proto",
        "pulumi/source.proto",
        "pulumi/callback.proto",
        "pulumi/analyzer.proto",
    ]
    .iter()
    .map(|f| proto_dir.join(f))
    .collect::<Vec<_>>();

    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }

    // Compile the vendored Pulumi protos without requiring a system protoc.
    let descriptors = protox::compile(&files, [&proto_dir])?;
    tonic_build::configure()
                // The SDK serves the Callbacks service so the engine can invoke
        // resource hooks back inside the program.
        .build_server(true)
        .build_client(true)
        .compile_fds(descriptors)?;
    Ok(())
}
