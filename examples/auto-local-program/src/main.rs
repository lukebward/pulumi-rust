//! The Automation API driving a `local` Pulumi program: an ordinary
//! on-disk project (here the YAML program in ./project) that could just as
//! well be run with the Pulumi CLI directly. The flow mirrors the Go
//! `local_program` example: create/select stack over the project dir, set
//! config, refresh, up with streamed engine events, outputs, destroy.

use std::path::Path;

use pulumi::auto::{ConfigValue, DestroyOptions, EngineEvent, RefreshOptions, Stack, UpOptions};
use tokio::sync::mpsc;

fn print_event(event: &EngineEvent) {
    if let Some(pre) = &event.resource_pre_event {
        let m = &pre.metadata;
        println!("    {} {} {}...", m.op.as_str(), m.r#type, urn_name(&m.urn));
    }
    if let Some(out) = &event.res_outputs_event {
        let m = &out.metadata;
        println!(
            "    {} {} {} done",
            m.op.as_str(),
            m.r#type,
            urn_name(&m.urn)
        );
    }
    if let Some(diag) = &event.diagnostic_event {
        if diag.severity == "error" {
            eprintln!("    error: {}", diag.message.trim_end());
        }
    }
    if let Some(summary) = &event.summary_event {
        let mut changes: Vec<String> = summary
            .resource_changes
            .iter()
            .map(|(op, n)| format!("{} {n}", op.as_str()))
            .collect();
        changes.sort();
        println!("    resources: {}", changes.join(", "));
    }
}

fn urn_name(urn: &str) -> &str {
    urn.rsplit("::").next().unwrap_or(urn)
}

fn stream_events() -> (
    mpsc::UnboundedSender<EngineEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            print_event(&event);
        }
    });
    (tx, printer)
}

fn fail(context: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("{context}: {err}");
    std::process::exit(1);
}

#[tokio::main]
async fn main() {
    // To destroy the stack, run `cargo run -- destroy`.
    let destroy = std::env::args().nth(1).as_deref() == Some("destroy");

    let stack_name = "dev";
    // The on-disk Pulumi project this driver deploys.
    let work_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("project");

    let stack = Stack::create_or_select_local_source(stack_name, work_dir)
        .await
        .unwrap_or_else(|err| fail("Failed to create or select stack", err));

    println!("Created/Selected stack {stack_name:?}");

    stack
        .set_config("siteName", &ConfigValue::plain("my-site"))
        .await
        .unwrap_or_else(|err| fail("Failed to set config", err));

    println!("Successfully set config");
    println!("Starting refresh");

    stack
        .refresh(RefreshOptions::default())
        .await
        .unwrap_or_else(|err| fail("Failed to refresh stack", err));

    println!("Refresh succeeded!");

    if destroy {
        println!("Starting stack destroy");
        let (tx, printer) = stream_events();
        stack
            .destroy(DestroyOptions {
                event_senders: vec![tx],
                remove: true,
                ..Default::default()
            })
            .await
            .unwrap_or_else(|err| fail("Failed to destroy stack", err));
        let _ = printer.await;
        println!("Stack successfully destroyed and removed");
        return;
    }

    println!("Starting update");
    let (tx, printer) = stream_events();
    let up = stack
        .up(UpOptions {
            event_senders: vec![tx],
            ..Default::default()
        })
        .await
        .unwrap_or_else(|err| fail("Failed to update stack", err));
    let _ = printer.await;

    println!("Update succeeded!");

    let url = up.outputs["url"]
        .value
        .as_str()
        .unwrap_or_else(|| fail("Failed to unmarshal output URL", "not a string"));
    println!("URL: {url}");
}
