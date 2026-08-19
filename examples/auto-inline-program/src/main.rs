//! The Automation API driving an `inline` Pulumi program: the program is a
//! Rust closure in this same binary, no separate project on disk. The flow
//! mirrors the Go `inline_program` example (create/select stack, set
//! config, refresh, up with streamed engine events) with a provider-less
//! program: config in, a component resource, a plain and a secret output.

use pulumi::auto::{
    self, ConfigValue, DestroyOptions, EngineEvent, LocalWorkspaceOptions, RefreshOptions, Stack,
    UpOptions,
};
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

    // The inline program: runs in-process during every stack operation.
    let program = auto::program(|ctx| async move {
        let site_name = ctx
            .config()
            .get("siteName")
            .unwrap_or_else(|| "hello-world".to_string());

        let content = pulumi::pv::string(format!(
            "<html><body><p>Hello from {site_name}!</p></body></html>"
        ));
        let site = ctx.register_resource(pulumi::RegisterRequest {
            type_: "examples:index:StaticSite".to_string(),
            name: site_name.clone(),
            custom: false,
            remote: false,
            version: String::new(),
            plugin_download_url: String::new(),
            inputs: vec![("content".to_string(), content.clone())],
            options: pulumi::ResourceOptions::default(),
            package: None,
            deferred_inputs: vec![],
            required: &[],
        });
        let url = pulumi::pv::string(format!("https://{site_name}.example.com"));
        ctx.register_resource_outputs(&site, vec![("url".to_string(), url.clone())]);

        ctx.export("websiteUrl", url);
        ctx.export("websiteContent", content);
        ctx.export(
            "deployToken",
            pulumi::Output::secret(pulumi::PropertyValue::String(format!("token-{site_name}"))),
        );
        Ok(())
    });

    let project_name = "inline-program";
    let stack_name = "dev";

    let stack = Stack::create_or_select_inline_source(
        stack_name,
        project_name,
        program,
        LocalWorkspaceOptions::default(),
    )
    .await
    .unwrap_or_else(|err| fail("Failed to set up a workspace", err));

    println!("Created/Selected stack {stack_name:?}");

    stack
        .set_config("siteName", &ConfigValue::plain("hello-world"))
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

    let url = up.outputs["websiteUrl"]
        .value
        .as_str()
        .unwrap_or_else(|| fail("Failed to unmarshal output URL", "not a string"));
    println!("URL: {url}");
    println!(
        "deployToken is secret: {}",
        up.outputs["deployToken"].secret
    );
}
