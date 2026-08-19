//! A Pulumi program cloned from a git repository by the Automation API.
//! To stay offline and deterministic, the example first creates the
//! repository locally; a real https URL works identically.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use pulumi::auto::{
    ConfigValue, DestroyOptions, EngineEvent, GitRepo, LocalWorkspace, LocalWorkspaceOptions,
    RefreshOptions, Stack, UpOptions,
};

const PROGRAM_YAML: &str = r#"name: git-repo-program
runtime: yaml
description: A provider-less program cloned from git by the Automation API.
config:
  greeting:
    type: string
    default: hello
outputs:
  message: ${greeting}
"#;

fn git(repo_dir: &Path, args: &[&str]) -> Result<(), Box<dyn Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("failed to run git (is it on PATH?): {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

fn create_fixture_repo() -> Result<PathBuf, Box<dyn Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let repo_dir = std::env::temp_dir().join(format!("auto-git-repo-program-{nanos}"));
    let project_dir = repo_dir.join("project");
    std::fs::create_dir_all(&project_dir)?;
    std::fs::write(project_dir.join("Pulumi.yaml"), PROGRAM_YAML)?;
    git(&repo_dir, &["init", "--quiet", "-b", "main"])?;
    git(&repo_dir, &["add", "."])?;
    git(
        &repo_dir,
        &[
            "-c",
            "user.name=automation",
            "-c",
            "user.email=automation@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "initial commit",
        ],
    )?;
    Ok(repo_dir)
}

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
    let repo_dir = create_fixture_repo()?;
    println!("Created fixture git repo at {}", repo_dir.display());

    // For a remote repository, replace the url with e.g.
    // "https://github.com/pulumi/examples.git".
    let repo = GitRepo {
        url: format!("file://{}", repo_dir.display()),
        project_path: Some(PathBuf::from("project")),
        branch: Some("main".to_string()),
        ..Default::default()
    };

    let workspace = LocalWorkspace::new(LocalWorkspaceOptions {
        repo: Some(repo),
        ..Default::default()
    })
    .await?;
    let stack = Stack::create_or_select("dev", workspace).await?;
    println!("Created/Selected stack \"dev\", and cloned program from git");

    stack
        .set_config(
            "greeting",
            &ConfigValue::plain("hello from a cloned program"),
        )
        .await?;
    println!("Successfully set config");

    println!("Starting refresh");
    stack.refresh(RefreshOptions::default()).await?;
    println!("Refresh succeeded!");

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
        "message: {}",
        up.outputs["message"].value.as_str().unwrap_or_default()
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

    std::fs::remove_dir_all(&repo_dir).ok();
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
