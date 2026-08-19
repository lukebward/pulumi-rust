//! Automation API: orchestrate two dependent stacks, propagating one
//! stack's outputs into the other's inline program, and destroying them
//! in reverse dependency order.

use pulumi::auto::{
    self, DestroyOptions, EngineEvent, LocalWorkspaceOptions, ProgramFn, RefreshOptions, Stack,
    UpOptions,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::task::JoinHandle;

const STACK_NAME: &str = "dev";

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

fn website_program() -> ProgramFn {
    auto::program(|ctx| async move {
        let bucket_id = format!("website-bucket-{}-{}", ctx.project(), ctx.stack());
        ctx.export(
            "websiteUrl",
            pulumi::pv::string(format!("http://{bucket_id}.example.com")),
        );
        // The object stack reads this from our stack outputs.
        ctx.export("bucketID", pulumi::pv::string(bucket_id));
        Ok(())
    })
}

// The bucket ID is curried into the program, read earlier from the
// website stack's outputs.
fn object_program(bucket_id: String) -> ProgramFn {
    auto::program(move |ctx| {
        let bucket_id = bucket_id.clone();
        async move {
            ctx.export(
                "objectKey",
                pulumi::pv::string(format!("{bucket_id}/index.html")),
            );
            Ok(())
        }
    })
}

async fn create_or_select_stack(project_name: &str, program: ProgramFn) -> auto::Result<Stack> {
    let stack = Stack::create_or_select_inline_source(
        STACK_NAME,
        project_name,
        program,
        LocalWorkspaceOptions::default(),
    )
    .await?;
    println!("Created/Selected stack {STACK_NAME:?}");
    println!("Starting refresh");
    stack.refresh(RefreshOptions::default()).await?;
    println!("Refresh succeeded!");
    Ok(stack)
}

#[tokio::main]
async fn main() -> auto::Result<()> {
    println!("preparing website stack");
    let website_stack = create_or_select_stack("multiStackWebsite", website_program()).await?;
    println!("website stack ready to deploy");

    println!("Starting website stack update");
    let (events, printer) = progress_stream();
    let web_res = website_stack
        .up(UpOptions {
            event_senders: vec![events],
            ..Default::default()
        })
        .await?;
    let _ = printer.await;
    println!("Website stack update succeeded!");

    let bucket_id = web_res.outputs["bucketID"]
        .value
        .as_str()
        .expect("bucketID output")
        .to_string();
    println!("got bucketID {bucket_id:?} for object stack");

    println!("preparing object stack");
    let object_stack =
        create_or_select_stack("multiStackObject", object_program(bucket_id)).await?;
    println!("object stack ready to deploy");

    println!("Starting object stack update");
    let (events, printer) = progress_stream();
    let obj_res = object_stack
        .up(UpOptions {
            event_senders: vec![events],
            ..Default::default()
        })
        .await?;
    let _ = printer.await;
    println!("Object stack update succeeded!");
    println!("objectKey: {}", obj_res.outputs["objectKey"].value);
    println!("URL: {}", web_res.outputs["websiteUrl"].value);

    // Destroy the dependent stack first, then the stack it reads from.
    println!("Starting object stack destroy");
    let (events, printer) = progress_stream();
    object_stack
        .destroy(DestroyOptions {
            remove: true,
            event_senders: vec![events],
            ..Default::default()
        })
        .await?;
    let _ = printer.await;
    println!("Object stack successfully destroyed");

    println!("Starting website stack destroy");
    let (events, printer) = progress_stream();
    website_stack
        .destroy(DestroyOptions {
            remove: true,
            event_senders: vec![events],
            ..Default::default()
        })
        .await?;
    let _ = printer.await;
    println!("Website stack successfully destroyed");
    Ok(())
}
