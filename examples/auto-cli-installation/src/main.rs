//! Install a pinned Pulumi CLI into a versioned root and drive a stack
//! with it, instead of whatever `pulumi` is on PATH.

use std::error::Error;
use std::sync::Arc;

use pulumi::auto::{
    self, DestroyOptions, EngineEvent, LocalPulumiCommand, LocalWorkspaceOptions, PulumiCommand,
    PulumiCommandOptions, Stack, UpOptions,
};
use semver::Version;

fn event_printer() -> (
    tokio::sync::mpsc::UnboundedSender<EngineEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<EngineEvent>();
    let printer = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Some(pre) = event.resource_pre_event {
                println!("  {} {} ...", pre.metadata.op, pre.metadata.r#type);
            } else if let Some(done) = event.res_outputs_event {
                println!("  {} {} done", done.metadata.op, done.metadata.r#type);
            } else if let Some(summary) = event.summary_event {
                println!(
                    "  {} resource change(s) in {}s",
                    summary.resource_changes.values().sum::<i64>(),
                    summary.duration_seconds
                );
            }
        }
    });
    (tx, printer)
}

async fn run() -> Result<(), Box<dyn Error>> {
    let version = Version::new(3, 200, 0);
    let root = std::env::temp_dir()
        .join("auto-cli-installation")
        .join(version.to_string());

    println!("Installing Pulumi v{version} into {}", root.display());
    println!("(roughly a 100MB download from get.pulumi.com on the first run)");
    let installed = match LocalPulumiCommand::install(PulumiCommandOptions {
        version: Some(version.clone()),
        root: Some(root.clone()),
        ..Default::default()
    })
    .await
    {
        Ok(command) => command,
        Err(e) => {
            eprintln!("Failed to install the Pulumi CLI: {e}");
            eprintln!("The install downloads from get.pulumi.com and needs network access.");
            std::process::exit(1);
        }
    };
    println!(
        "Installed CLI at {} reports version v{}",
        root.join("bin").join("pulumi").display(),
        installed.version()
    );

    let program = auto::program(|ctx| async move {
        ctx.export(
            "greeting",
            pulumi::pv::string("hello from an installed CLI"),
        );
        Ok(())
    });

    let stack = Stack::create_or_select_inline_source(
        "dev",
        "auto-cli-installation",
        program,
        LocalWorkspaceOptions {
            pulumi_command: Some(Arc::new(installed)),
            ..Default::default()
        },
    )
    .await?;
    println!("Created/Selected stack \"dev\"");
    println!(
        "Workspace drives Pulumi v{}, not the CLI on PATH",
        stack.workspace().pulumi_version()
    );

    println!("Starting update");
    let (events, printer) = event_printer();
    let up = stack
        .up(UpOptions {
            event_senders: vec![events],
            ..Default::default()
        })
        .await?;
    printer.await?;
    println!("Update succeeded!");
    println!(
        "greeting: {}",
        up.outputs["greeting"].value.as_str().unwrap_or_default()
    );

    println!("Starting stack destroy");
    let (events, printer) = event_printer();
    stack
        .destroy(DestroyOptions {
            remove: true,
            event_senders: vec![events],
            ..Default::default()
        })
        .await?;
    printer.await?;
    println!("Stack successfully destroyed and removed");
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
