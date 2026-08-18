//! Integration tests for the automation API, driving a real `pulumi` CLI
//! against a local file backend — no cloud account, no provider plugins.
//!
//! The tests skip themselves when `pulumi` is not on `PATH`, keeping
//! `make test_sdk` hermetic. With a CLI present (CI's conformance jobs,
//! a developer machine) they exercise the full loop: workspace setup,
//! stack lifecycle, config, up/preview/refresh/destroy, outputs, event
//! streaming, and inline programs served from this process.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pulumi::auto::{
    self, ConfigValue, DestroyOptions, LocalWorkspace, LocalWorkspaceOptions, PreviewOptions,
    ProjectSettings, Stack, UpOptions,
};

/// Whether a runnable `pulumi` is on PATH; tests skip quietly otherwise.
fn pulumi_available() -> bool {
    std::process::Command::new("pulumi")
        .arg("version")
        .env("PULUMI_SKIP_UPDATE_CHECK", "true")
        .output()
        .is_ok()
}

macro_rules! require_cli {
    () => {
        if !pulumi_available() {
            eprintln!("skipping: pulumi CLI not on PATH");
            return;
        }
    };
}

/// A scratch area with its own state backend and `PULUMI_HOME`, so tests
/// touch nothing of the user's and nothing of each other's.
struct TestEnv {
    root: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "pulumi-rust-auto-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("project")).unwrap();
        TestEnv { root }
    }

    fn project_dir(&self) -> PathBuf {
        self.root.join("project")
    }

    fn env_vars(&self) -> HashMap<String, String> {
        HashMap::from([
            (
                "PULUMI_BACKEND_URL".to_string(),
                format!("file://{}", self.root.join("state").display()),
            ),
            (
                "PULUMI_CONFIG_PASSPHRASE".to_string(),
                "correct horse battery staple".to_string(),
            ),
            (
                "PULUMI_HOME".to_string(),
                self.root.join("home").display().to_string(),
            ),
        ])
    }

    async fn workspace(&self, options: LocalWorkspaceOptions) -> LocalWorkspace {
        LocalWorkspace::new(LocalWorkspaceOptions {
            env_vars: self.env_vars(),
            ..options
        })
        .await
        .expect("workspace")
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The full lifecycle against a YAML program on disk: config, up,
/// outputs, preview, refresh, destroy, remove.
#[tokio::test]
async fn local_source_full_lifecycle() {
    require_cli!();
    let env = TestEnv::new();
    std::fs::write(
        env.project_dir().join("Pulumi.yaml"),
        "name: autotest\nruntime: yaml\nconfig:\n  bar:\n    type: string\n    default: unset\noutputs:\n  fixed: hello\n  fromConfig: ${bar}\n",
    )
    .unwrap();

    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    stack
        .set_config("bar", &ConfigValue::plain("abc"))
        .await
        .expect("set config");
    let got = stack.get_config("bar").await.expect("get config");
    assert_eq!(got.value, "abc");
    assert!(!got.secret);

    let up = stack.up(UpOptions::default()).await.expect("up");
    assert_eq!(up.outputs["fixed"].value, serde_json::json!("hello"));
    assert_eq!(up.outputs["fromConfig"].value, serde_json::json!("abc"));
    let summary = up.summary.expect("up summary");
    assert_eq!(summary.kind, "update");
    assert_eq!(summary.result.as_deref(), Some("succeeded"));

    // A second deployment changes nothing; expect-no-changes agrees.
    let preview = stack
        .preview(PreviewOptions {
            expect_no_changes: true,
            ..Default::default()
        })
        .await
        .expect("preview");
    assert_eq!(
        preview.change_summary.get(&auto::events::OpType::Same),
        Some(&1),
        "changes: {:?}",
        preview.change_summary
    );

    let refresh = stack.refresh(Default::default()).await.expect("refresh");
    assert_eq!(
        refresh.summary.expect("refresh summary").result.as_deref(),
        Some("succeeded")
    );

    let history = stack.history(None, 0, None).await.expect("history");
    assert!(history.len() >= 2, "expected up+refresh in history");

    let destroy = stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
    assert_eq!(destroy.summary.expect("destroy summary").kind, "destroy");
    // The stack was removed along with the destroy.
    let stacks = stack.workspace().list_stacks().await.expect("list");
    assert!(stacks.iter().all(|s| !s.name.contains("dev")));
}

/// An inline program served from this process: the engine calls back into
/// the closure over gRPC, twice (up, then preview), including config flow,
/// secret outputs, and engine-event streaming.
#[tokio::test]
async fn inline_program_up_preview_destroy() {
    require_cli!();
    let env = TestEnv::new();

    let program = auto::program(|ctx| async move {
        let who = ctx
            .config()
            .get("who")
            .unwrap_or_else(|| "world".to_string());
        ctx.export("greeting", pulumi::pv::string(format!("hello, {who}")));
        ctx.export(
            "shh",
            pulumi::Output::secret(pulumi::PropertyValue::String("s3cret".to_string())),
        );
        Ok(())
    });

    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("inline-test", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    stack
        .set_config("inline-test:who", &ConfigValue::plain("automation"))
        .await
        .expect("set config");

    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let up = stack
        .up(UpOptions {
            event_senders: vec![events_tx],
            ..Default::default()
        })
        .await
        .expect("up");
    assert_eq!(
        up.outputs["greeting"].value,
        serde_json::json!("hello, automation")
    );
    assert!(!up.outputs["greeting"].secret);
    assert_eq!(up.outputs["shh"].value, serde_json::json!("s3cret"));
    assert!(up.outputs["shh"].secret, "secret output must be marked");

    // The event stream saw the update start and finish.
    let mut saw_summary = false;
    while let Some(event) = events_rx.recv().await {
        saw_summary |= event.summary_event.is_some();
    }
    assert!(saw_summary, "expected a summary event on the stream");

    let preview = stack
        .preview(PreviewOptions::default())
        .await
        .expect("preview");
    assert_eq!(
        preview.change_summary.get(&auto::events::OpType::Same),
        Some(&1),
        "changes: {:?}",
        preview.change_summary
    );

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// Inline program failures surface as classified errors, not hangs.
#[tokio::test]
async fn inline_program_failure_is_reported() {
    require_cli!();
    let env = TestEnv::new();

    let failing = auto::program(|_ctx| async move {
        Err(pulumi::Error::new("deliberate failure from the program"))
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(failing),
            project_settings: Some(ProjectSettings::new("inline-fail", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    let err = stack.up(UpOptions::default()).await.expect_err("up fails");
    assert!(
        err.to_string()
            .contains("deliberate failure from the program"),
        "error was: {err}"
    );

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// Lifecycle errors classify the way the Go predicates do.
#[tokio::test]
async fn stack_lifecycle_errors_classify() {
    require_cli!();
    let env = TestEnv::new();
    let ws = env
        .workspace(LocalWorkspaceOptions {
            project_settings: Some(ProjectSettings::new("classify", "rust")),
            ..Default::default()
        })
        .await;

    let missing = ws.select_stack("never-created").await.expect_err("select");
    assert!(
        missing.is_select_stack_404_error(),
        "unexpected error: {missing}"
    );

    ws.create_stack("dupe").await.expect("create");
    let dupe = ws.create_stack("dupe").await.expect_err("create again");
    assert!(dupe.is_create_stack_409_error(), "unexpected error: {dupe}");
    ws.remove_stack("dupe", false).await.expect("remove");
}

/// Workspace-level state operations: export/import round trip, whoami,
/// stack settings on disk.
#[tokio::test]
async fn workspace_state_operations() {
    require_cli!();
    let env = TestEnv::new();

    let program = auto::program(|ctx| async move {
        ctx.export("n", pulumi::PropertyValue::Number(1.0));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("stateops", "rust")),
            ..Default::default()
        })
        .await;

    let who = ws.whoami().await.expect("whoami");
    assert!(!who.user.is_empty());

    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    stack.up(UpOptions::default()).await.expect("up");

    let deployment = stack.export().await.expect("export");
    assert!(
        deployment.deployment.get("resources").is_some(),
        "deployment JSON should carry resources"
    );
    stack.import(&deployment).await.expect("import");

    let outputs = stack.outputs().await.expect("outputs");
    // The CLI may print a whole number without a fraction.
    assert_eq!(outputs["n"].value.as_f64(), Some(1.0));

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}
