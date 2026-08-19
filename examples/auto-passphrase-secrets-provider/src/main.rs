//! Automation API: an inline program whose stack uses a passphrase
//! secrets provider, with the passphrase supplied through the workspace
//! environment — plus a rotation to a new passphrase.

use pulumi::auto::{
    self, ConfigValue, DestroyOptions, EngineEvent, LocalWorkspaceOptions, ProjectSettings, Stack,
    StackSettings, UpOptions,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::task::JoinHandle;

const PASSPHRASE: &str = "password";
const NEW_PASSPHRASE: &str = "a-brand-new-passphrase";

fn progress_stream() -> (UnboundedSender<EngineEvent>, JoinHandle<()>) {
    let (tx, mut rx) = unbounded_channel::<EngineEvent>();
    let printer = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Some(pre) = event.resource_pre_event {
                let name = pre.metadata.urn.rsplit("::").next().unwrap_or_default();
                println!("  {} {} {}", pre.metadata.op, pre.metadata.r#type, name);
            } else if let Some(summary) = event.summary_event {
                let changes: Vec<String> = summary
                    .resource_changes
                    .iter()
                    .map(|(op, n)| format!("{op}: {n}"))
                    .collect();
                println!("  {} ({}s)", changes.join(", "), summary.duration_seconds);
            }
        }
    });
    (tx, printer)
}

#[tokio::main]
async fn main() -> auto::Result<()> {
    let project_name = "passphraseSecretsProject";
    let stack_name = "dev";

    let program = auto::program(|ctx| async move {
        // The secret config value flows into a secret output.
        ctx.export("secretValue", ctx.config().require("myPassword")?);
        ctx.export(
            "greeting",
            pulumi::pv::string("hello from a passphrase-encrypted stack"),
        );
        Ok(())
    });

    let mut options = LocalWorkspaceOptions {
        secrets_provider: Some("passphrase".to_string()),
        project_settings: Some(ProjectSettings::new(project_name, "rust")),
        ..Default::default()
    };
    // In a real program, feed the passphrase in securely.
    options.env_vars.insert(
        "PULUMI_CONFIG_PASSPHRASE".to_string(),
        PASSPHRASE.to_string(),
    );
    options.stack_settings.insert(
        stack_name.to_string(),
        StackSettings {
            secrets_provider: Some("passphrase".to_string()),
            ..Default::default()
        },
    );

    let mut stack =
        Stack::create_or_select_inline_source(stack_name, project_name, program, options).await?;
    println!("Created/Selected stack {stack_name:?}");

    stack
        .set_config("myPassword", &ConfigValue::secret("s3cret-hunter2"))
        .await?;
    println!("Successfully set config");

    let read_back = stack.get_config("myPassword").await?;
    println!(
        "Read myPassword back decrypted: value={:?} secret={}",
        read_back.value, read_back.secret
    );

    println!("Refreshing stack");
    stack.refresh(Default::default()).await?;
    println!("Refresh succeeded!");

    println!("Starting update");
    let (events, printer) = progress_stream();
    let up = stack
        .up(UpOptions {
            event_senders: vec![events],
            ..Default::default()
        })
        .await?;
    let _ = printer.await;
    println!("Update succeeded!");
    println!(
        "secretValue stays marked secret: {}",
        up.outputs["secretValue"].secret
    );
    println!("greeting: {}", up.outputs["greeting"].value);

    println!("Rotating the stack to a new passphrase");
    stack
        .workspace()
        .change_stack_secrets_provider(stack_name, "passphrase", Some(NEW_PASSPHRASE))
        .await?;
    stack
        .workspace_mut()
        .set_env_var("PULUMI_CONFIG_PASSPHRASE", NEW_PASSPHRASE);
    let after = stack.get_config("myPassword").await?;
    println!(
        "After rotation myPassword still decrypts: value={:?} secret={}",
        after.value, after.secret
    );

    println!("Starting stack destroy");
    let (events, printer) = progress_stream();
    stack
        .destroy(DestroyOptions {
            remove: true,
            event_senders: vec![events],
            ..Default::default()
        })
        .await?;
    let _ = printer.await;
    println!("Stack successfully destroyed and removed");
    Ok(())
}
