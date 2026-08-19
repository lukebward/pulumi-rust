//! Integration tests for stack operations and per-operation lifecycle
//! options: create-or-select, force removal, workspace stack CRUD, tags,
//! plugins, preview-only refresh/destroy, and the refresh/plan/run-program
//! flags on up, preview, refresh, and destroy. Same harness as auto.rs: a
//! real `pulumi` CLI against an isolated file backend, skipping when the
//! CLI is absent.

mod common;

use common::TestEnv;
use pulumi::auto::{
    self, ConfigValue, DestroyOptions, ImportOptions, ImportResource, ListOptions,
    LocalWorkspaceOptions, NewOptions, PreviewOptions, ProjectSettings, RefreshOptions,
    RenameOptions, Stack, UpOptions,
};

/// Refresh on an inline-program stack returns a succeeded summary.
#[tokio::test]
async fn inline_program_refresh_succeeds() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.export("n", pulumi::PropertyValue::Number(1.0));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("refresh-inline", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    stack.up(UpOptions::default()).await.expect("up");

    let refresh = stack
        .refresh(RefreshOptions::default())
        .await
        .expect("refresh");
    let summary = refresh.summary.expect("refresh summary");
    assert_eq!(summary.kind, "refresh");
    assert_eq!(summary.result.as_deref(), Some("succeeded"));

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// create_or_select creates a missing stack and selects an existing one.
#[tokio::test]
async fn create_or_select_creates_then_selects() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.export("greeting", pulumi::pv::string("hello"));
        Ok(())
    });
    let options = |env: &TestEnv| LocalWorkspaceOptions {
        work_dir: Some(env.project_dir()),
        env_vars: env.env_vars(),
        ..Default::default()
    };

    Stack::create_or_select_inline_source("dev", "cos-proj", program.clone(), options(&env))
        .await
        .expect("first call creates the stack");
    let stack =
        Stack::create_or_select_inline_source("dev", "cos-proj", program.clone(), options(&env))
            .await
            .expect("second call selects without error");

    let stacks = stack.workspace().list_stacks().await.expect("list");
    assert_eq!(stacks.len(), 1, "stacks: {stacks:?}");
    assert!(stacks[0].name.contains("dev"), "stacks: {stacks:?}");

    stack
        .workspace()
        .remove_stack("dev", false)
        .await
        .expect("remove");
}

/// remove_stack without force fails while resources exist; force removes
/// the stack, and a later select classifies as a 404.
#[tokio::test]
async fn remove_stack_requires_force_while_resources_exist() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        // A child resource keeps the stack non-empty: the CLI ignores the
        // root stack resource when deciding whether removal is safe.
        ctx.register_resource(pulumi::RegisterRequest {
            type_: "test:index:Child".to_string(),
            name: "child".to_string(),
            custom: false,
            ..Default::default()
        });
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("rm-force", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    stack.up(UpOptions::default()).await.expect("up");

    let err = stack
        .workspace()
        .remove_stack("dev", false)
        .await
        .expect_err("remove without force must fail");
    assert!(
        err.to_string().to_lowercase().contains("resources"),
        "error was: {err}"
    );

    stack
        .workspace()
        .remove_stack("dev", true)
        .await
        .expect("forced remove");

    let missing = stack
        .workspace()
        .select_stack("dev")
        .await
        .expect_err("select after removal");
    assert!(
        missing.is_select_stack_404_error(),
        "unexpected error: {missing}"
    );
}

/// The current stack follows create then select; list and remove complete
/// the workspace-level CRUD.
#[tokio::test]
async fn workspace_stack_crud_tracks_the_current_stack() {
    require_cli!();
    let env = TestEnv::new();
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            project_settings: Some(ProjectSettings::new("crud", "rust")),
            ..Default::default()
        })
        .await;

    ws.create_stack("first").await.expect("create first");
    ws.create_stack("second").await.expect("create second");

    let names: Vec<String> = ws
        .list_stacks()
        .await
        .expect("list")
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names.len(), 2, "names: {names:?}");
    assert!(
        names.iter().any(|n| n.contains("first")),
        "names: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.contains("second")),
        "names: {names:?}"
    );

    // stack init selects the new stack; select moves the pointer back.
    let current = ws.stack().await.expect("stack").expect("a current stack");
    assert!(current.name.contains("second"), "current: {current:?}");
    ws.select_stack("first").await.expect("select first");
    let current = ws.stack().await.expect("stack").expect("a current stack");
    assert!(current.name.contains("first"), "current: {current:?}");

    ws.remove_stack("first", false).await.expect("remove first");
    ws.remove_stack("second", false)
        .await
        .expect("remove second");
    let stacks = ws.list_stacks().await.expect("list after removal");
    assert!(stacks.is_empty(), "stacks: {stacks:?}");
}

/// A fresh stack has an empty update history.
#[tokio::test]
async fn fresh_stack_has_empty_history() {
    require_cli!();
    let env = TestEnv::new();
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            project_settings: Some(ProjectSettings::new("hist", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create("dev", ws).await.expect("stack");
    let history = stack.history(None, 0, None).await.expect("history");
    assert!(history.is_empty(), "history: {history:?}");
}

/// Inline-source stack creation keeps an existing Pulumi.yaml's project
/// name and description instead of generating settings over them.
#[tokio::test]
async fn existing_project_settings_are_respected() {
    require_cli!();
    let env = TestEnv::new();
    std::fs::write(
        env.project_dir().join("Pulumi.yaml"),
        "name: on-disk-project\nruntime: rust\ndescription: A description\n",
    )
    .unwrap();

    let program = auto::program(|_ctx| async move { Ok(()) });
    let stack = Stack::create_inline_source(
        "dev",
        "generated-name-not-used",
        program,
        LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            env_vars: env.env_vars(),
            ..Default::default()
        },
    )
    .await
    .expect("stack");

    let settings = stack
        .workspace()
        .project_settings()
        .expect("project settings");
    assert_eq!(settings.name, "on-disk-project");
    assert_eq!(settings.description.as_deref(), Some("A description"));

    stack
        .workspace()
        .remove_stack("dev", false)
        .await
        .expect("remove");
}

/// Outputs are empty before up, equal the up result (values and secret
/// flags) after up, and empty again after destroy.
#[tokio::test]
async fn outputs_follow_up_and_destroy() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.export("plain", pulumi::pv::string("open"));
        ctx.export(
            "shh",
            pulumi::Output::secret(pulumi::PropertyValue::String("hidden".to_string())),
        );
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("outs", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    let before = stack.outputs().await.expect("outputs before up");
    assert!(before.is_empty(), "outputs: {before:?}");

    let up = stack.up(UpOptions::default()).await.expect("up");
    let outputs = stack.outputs().await.expect("outputs after up");
    assert_eq!(outputs.len(), 2, "outputs: {outputs:?}");
    for (key, output) in &outputs {
        assert_eq!(output.value, up.outputs[key].value, "value for {key}");
        assert_eq!(output.secret, up.outputs[key].secret, "secrecy of {key}");
    }
    assert_eq!(outputs["plain"].value, serde_json::json!("open"));
    assert!(!outputs["plain"].secret);
    assert_eq!(outputs["shh"].value, serde_json::json!("hidden"));
    assert!(outputs["shh"].secret);

    stack
        .destroy(DestroyOptions::default())
        .await
        .expect("destroy");
    let after = stack.outputs().await.expect("outputs after destroy");
    assert!(after.is_empty(), "outputs: {after:?}");

    stack
        .workspace()
        .remove_stack("dev", false)
        .await
        .expect("remove");
}

/// refresh=true on up, preview, and destroy runs a refresh first, and the
/// preview's change summary stays one Same.
#[tokio::test]
async fn refresh_option_runs_a_refresh() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.export("greeting", pulumi::pv::string("hello"));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("ref-opt", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    stack.up(UpOptions::default()).await.expect("first up");

    let up = stack
        .up(UpOptions {
            refresh: true,
            ..Default::default()
        })
        .await
        .expect("up with refresh");
    assert!(
        up.stdout.to_lowercase().contains("refresh"),
        "stdout: {}",
        up.stdout
    );

    let preview = stack
        .preview(PreviewOptions {
            refresh: true,
            ..Default::default()
        })
        .await
        .expect("preview with refresh");
    assert!(
        preview.stdout.to_lowercase().contains("refresh"),
        "stdout: {}",
        preview.stdout
    );
    assert_eq!(
        preview.change_summary.get(&auto::events::OpType::Same),
        Some(&1),
        "changes: {:?}",
        preview.change_summary
    );

    let destroy = stack
        .destroy(DestroyOptions {
            refresh: true,
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy with refresh");
    assert!(
        destroy.stdout.to_lowercase().contains("refresh"),
        "stdout: {}",
        destroy.stdout
    );
}

/// preview_refresh reports the up'd stack as one Same and adopts nothing:
/// the history still holds only the update.
#[tokio::test]
async fn preview_refresh_reports_sames_without_refreshing() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.export("n", pulumi::PropertyValue::Number(1.0));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("prev-refresh", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    stack.up(UpOptions::default()).await.expect("up");

    let preview = stack
        .preview_refresh(RefreshOptions::default())
        .await
        .expect("preview refresh");
    assert_eq!(
        preview.change_summary.len(),
        1,
        "{:?}",
        preview.change_summary
    );
    assert_eq!(
        preview.change_summary.get(&auto::events::OpType::Same),
        Some(&1),
        "changes: {:?}",
        preview.change_summary
    );

    // Nothing ran: no refresh entry joined the history.
    let history = stack.history(None, 0, None).await.expect("history");
    assert_eq!(history.len(), 1, "history: {history:?}");
    assert_eq!(history[0].kind, "update");

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// preview_refresh on a stack with a component resource counts the stack
/// root and the component as Sames.
#[tokio::test]
async fn preview_refresh_with_resource_counts_all_sames() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.register_resource(pulumi::RegisterRequest {
            type_: "my:module:MyResource".to_string(),
            name: "res".to_string(),
            custom: false,
            ..Default::default()
        });
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("prev-refresh-res", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    stack.up(UpOptions::default()).await.expect("up");

    let preview = stack
        .preview_refresh(RefreshOptions {
            expect_no_changes: true,
            ..Default::default()
        })
        .await
        .expect("preview refresh");
    assert_eq!(
        preview.change_summary.len(),
        1,
        "{:?}",
        preview.change_summary
    );
    assert_eq!(
        preview.change_summary.get(&auto::events::OpType::Same),
        Some(&2),
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

/// preview_destroy reports the pending delete and deletes nothing: the
/// outputs are still there afterward, and the real destroy then reports
/// the same count.
#[tokio::test]
async fn preview_destroy_reports_deletes_without_destroying() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.export("n", pulumi::PropertyValue::Number(1.0));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("prev-destroy", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    stack.up(UpOptions::default()).await.expect("up");

    let preview = stack
        .preview_destroy(DestroyOptions::default())
        .await
        .expect("preview destroy");
    assert_eq!(
        preview.change_summary.len(),
        1,
        "{:?}",
        preview.change_summary
    );
    assert_eq!(
        preview.change_summary.get(&auto::events::OpType::Delete),
        Some(&1),
        "changes: {:?}",
        preview.change_summary
    );

    let outputs = stack.outputs().await.expect("outputs");
    assert!(!outputs.is_empty(), "the preview must not delete anything");

    let destroy = stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
    let summary = destroy.summary.expect("destroy summary");
    assert_eq!(summary.kind, "destroy");
    assert_eq!(summary.result.as_deref(), Some("succeeded"));
    assert_eq!(
        summary
            .resource_changes
            .as_ref()
            .and_then(|c| c.get("delete")),
        Some(&1),
        "changes: {:?}",
        summary.resource_changes
    );
}

/// preview_destroy on a stack with a component resource counts the stack
/// root and the component as Deletes.
#[tokio::test]
async fn preview_destroy_with_resource_counts_all_deletes() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.register_resource(pulumi::RegisterRequest {
            type_: "my:module:MyResource".to_string(),
            name: "res".to_string(),
            custom: false,
            ..Default::default()
        });
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("prev-destroy-res", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    stack.up(UpOptions::default()).await.expect("up");

    let preview = stack
        .preview_destroy(DestroyOptions::default())
        .await
        .expect("preview destroy");
    assert_eq!(
        preview.change_summary.len(),
        1,
        "{:?}",
        preview.change_summary
    );
    assert_eq!(
        preview.change_summary.get(&auto::events::OpType::Delete),
        Some(&2),
        "changes: {:?}",
        preview.change_summary
    );

    let destroy = stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
    assert_eq!(
        destroy
            .summary
            .expect("destroy summary")
            .resource_changes
            .as_ref()
            .and_then(|c| c.get("delete")),
        Some(&2)
    );
}

/// preview with save_plan writes a non-empty plan file that a later up
/// applies.
#[tokio::test]
async fn preview_saves_a_plan_that_up_applies() {
    require_cli!();
    // Update plans are not supported on Windows; Go skips there too.
    if cfg!(windows) {
        eprintln!("skipping: update plans are unsupported on Windows");
        return;
    }
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.export("greeting", pulumi::pv::string("hello"));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("plans", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    let plan = env.root.join("plan.json");
    stack
        .preview(PreviewOptions {
            save_plan: Some(plan.clone()),
            ..Default::default()
        })
        .await
        .expect("preview with save_plan");
    let metadata = std::fs::metadata(&plan).expect("plan file exists");
    assert!(metadata.len() > 0, "plan file is empty");

    let up = stack
        .up(UpOptions {
            plan: Some(plan),
            ..Default::default()
        })
        .await
        .expect("up with plan");
    assert_eq!(
        up.summary.expect("up summary").result.as_deref(),
        Some("succeeded")
    );

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// run_program=true is accepted on up, preview, refresh, and destroy, and
/// each operation completes against an inline program with a component.
#[tokio::test]
async fn run_program_flag_on_every_operation() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.register_resource(pulumi::RegisterRequest {
            type_: "test:index:Component".to_string(),
            name: "comp".to_string(),
            custom: false,
            ..Default::default()
        });
        ctx.export("ok", pulumi::pv::string("yes"));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("run-prog", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    let up = stack
        .up(UpOptions {
            run_program: Some(true),
            ..Default::default()
        })
        .await
        .expect("up");
    assert_eq!(
        up.summary.expect("up summary").result.as_deref(),
        Some("succeeded")
    );

    let preview = stack
        .preview(PreviewOptions {
            run_program: Some(true),
            ..Default::default()
        })
        .await
        .expect("preview");
    // The stack root and the component are both unchanged.
    assert_eq!(
        preview.change_summary.get(&auto::events::OpType::Same),
        Some(&2),
        "changes: {:?}",
        preview.change_summary
    );

    let refresh = stack
        .refresh(RefreshOptions {
            run_program: Some(true),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert_eq!(
        refresh.summary.expect("refresh summary").result.as_deref(),
        Some("succeeded")
    );

    let destroy = stack
        .destroy(DestroyOptions {
            run_program: Some(true),
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
    assert_eq!(
        destroy.summary.expect("destroy summary").result.as_deref(),
        Some("succeeded")
    );
}

/// destroy with exclude_protected removes only unprotected resources;
/// unprotecting via a re-up lets a plain destroy clean up fully.
#[tokio::test]
async fn destroy_with_exclude_protected_spares_protected_resources() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        let protect = ctx.config().get("protect").as_deref() == Some("true");
        ctx.register_resource(pulumi::RegisterRequest {
            type_: "my:module:MyResource".to_string(),
            name: "protected".to_string(),
            custom: false,
            options: pulumi::ResourceOptions {
                protect: Some(protect),
                ..Default::default()
            },
            ..Default::default()
        });
        ctx.register_resource(pulumi::RegisterRequest {
            type_: "my:module:MyResource".to_string(),
            name: "unprotected".to_string(),
            custom: false,
            ..Default::default()
        });
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("excl-prot", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    stack
        .set_config("excl-prot:protect", &ConfigValue::plain("true"))
        .await
        .expect("set config");
    stack.up(UpOptions::default()).await.expect("protected up");

    let destroy = stack
        .destroy(DestroyOptions {
            exclude_protected: true,
            ..Default::default()
        })
        .await
        .expect("destroy with exclude_protected");
    let summary = destroy.summary.expect("destroy summary");
    assert_eq!(summary.kind, "destroy");
    assert_eq!(summary.result.as_deref(), Some("succeeded"));
    assert!(
        destroy
            .stdout
            .contains("All unprotected resources were destroyed"),
        "stdout: {}",
        destroy.stdout
    );

    stack
        .remove_config("excl-prot:protect")
        .await
        .expect("remove config");
    stack
        .up(UpOptions::default())
        .await
        .expect("unprotecting up");
    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("final destroy");
}

/// preview with json=true emits stdout that parses as JSON, while the
/// event-log change summary keeps working.
#[tokio::test]
async fn preview_json_output_parses() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.export("greeting", pulumi::pv::string("hello"));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("prev-json", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    stack.up(UpOptions::default()).await.expect("up");

    let preview = stack
        .preview(PreviewOptions {
            json: true,
            ..Default::default()
        })
        .await
        .expect("preview with json");
    let parsed: serde_json::Value =
        serde_json::from_str(&preview.stdout).expect("stdout parses as JSON");
    assert!(parsed.is_object(), "stdout was: {}", preview.stdout);
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

/// Stack tags round-trip on the file backend; the v3.242.0 CLI reports no
/// built-in tags there and silently ignores `stack tag rm`.
#[tokio::test]
async fn stack_tags_on_the_file_backend() {
    require_cli!();
    let env = TestEnv::new();
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            project_settings: Some(ProjectSettings::new("tags", "rust")),
            ..Default::default()
        })
        .await;
    ws.create_stack("dev").await.expect("create");

    ws.set_tag("dev", "team", "acme").await.expect("set tag");
    assert_eq!(ws.get_tag("dev", "team").await.expect("get tag"), "acme");

    let tags = ws.list_tags("dev").await.expect("list tags");
    assert_eq!(tags.get("team").map(String::as_str), Some("acme"));
    // The service backend also lists built-ins such as pulumi:project; the
    // file backend on CLI 3.242.0 lists only explicitly set tags.
    if !tags.contains_key("pulumi:project") {
        eprintln!("note: the file backend reports no pulumi:project tag");
    }

    ws.remove_tag("dev", "team").await.expect("remove tag");
    let tags = ws.list_tags("dev").await.expect("list tags after rm");
    match tags.get("team").map(String::as_str) {
        // Service-like behavior: the tag is gone.
        None => {}
        // File-backend behavior on CLI 3.242.0: rm exits 0, tag persists.
        Some("acme") => eprintln!("note: the file backend kept the tag after rm"),
        Some(other) => panic!("unexpected tag value after rm: {other}"),
    }

    ws.remove_stack("dev", false).await.expect("remove stack");
}

/// install_plugin makes the plugin appear in list_plugins and
/// remove_plugin removes it again.
#[tokio::test]
async fn plugin_install_list_remove() {
    require_cli!();
    let env = TestEnv::new();
    let ws = env.workspace(LocalWorkspaceOptions::default()).await;

    // The install downloads from the registry; skip only on network
    // failures so an argv regression cannot pass vacuously.
    if let Err(err) = ws.install_plugin("random", "4.16.3").await {
        let text = err.to_string().to_lowercase();
        let network_failure = ["download", "dial", "connection", "lookup", "timeout"]
            .iter()
            .any(|needle| text.contains(needle));
        if !network_failure {
            panic!("install_plugin failed for a non-network reason: {err}");
        }
        eprintln!("skipping: plugin download failed: {err}");
        return;
    }

    let plugins = ws.list_plugins().await.expect("list plugins");
    assert!(
        plugins
            .iter()
            .any(|p| p.name == "random" && p.version.as_deref() == Some("4.16.3")),
        "plugins: {plugins:?}"
    );

    ws.remove_plugin("random", "4.16.3")
        .await
        .expect("remove plugin");
    let plugins = ws.list_plugins().await.expect("list plugins after rm");
    assert!(
        !plugins
            .iter()
            .any(|p| p.name == "random" && p.version.as_deref() == Some("4.16.3")),
        "plugins: {plugins:?}"
    );
}

/// Whether an error reads as a network failure; steps that need the
/// network skip on these instead of failing the suite. The text comes
/// from the CLI, so no transport-versus-status split is possible here.
fn is_network_failure(err: &pulumi::auto::Error) -> bool {
    let text = err.to_string().to_lowercase();
    ["download", "dial", "connection", "lookup", "timeout"]
        .iter()
        .any(|needle| text.contains(needle))
}

/// A local template directory in the Go test-fixture shape: `${PROJECT}`
/// and `${DESCRIPTION}` fill in from --name/--description.
fn write_template(env: &TestEnv) -> std::path::PathBuf {
    let template_dir = env.root.join("template");
    std::fs::create_dir_all(&template_dir).unwrap();
    std::fs::write(
        template_dir.join("Pulumi.yaml"),
        "name: ${PROJECT}\ndescription: ${DESCRIPTION}\nruntime: yaml\n",
    )
    .unwrap();
    template_dir
}

/// new_project with generate_only creates Pulumi.yaml in the workdir and
/// nothing else: no stack, no config. Ports go:TestNewGenerateOnly.
#[tokio::test]
async fn new_generate_only_creates_the_project_file() {
    require_cli!();
    let env = TestEnv::new();
    let template_dir = write_template(&env);
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;

    // A local template directory resolves offline, so any failure here is
    // real; no network gate, as in Go's TestNewGenerateOnly.
    let result = ws
        .new_project(&NewOptions {
            template_or_url: Some(template_dir.display().to_string()),
            name: Some("test-new-project".to_string()),
            generate_only: true,
            force: true,
            ..Default::default()
        })
        .await
        .expect("new_project");
    assert!(!result.stdout.is_empty(), "stdout must carry CLI output");

    let contents = std::fs::read_to_string(env.project_dir().join("Pulumi.yaml"))
        .expect("Pulumi.yaml was created");
    assert!(
        contents.contains("name: test-new-project"),
        "unexpected project file: {contents}"
    );
    let stacks = ws.list_stacks().await.expect("list stacks");
    assert!(stacks.is_empty(), "generate-only made a stack: {stacks:?}");
}

/// new_project with dir places the generated project in a subdirectory.
/// Ports go:TestNewGenerateOnlyInSubDir.
#[tokio::test]
async fn new_generate_only_respects_the_dir_option() {
    require_cli!();
    let env = TestEnv::new();
    let template_dir = write_template(&env);
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;

    let sub_dir = env.project_dir().join("subproject");
    let result = ws
        .new_project(&NewOptions {
            template_or_url: Some(template_dir.display().to_string()),
            name: Some("sub-project".to_string()),
            description: Some("A sub-project for testing".to_string()),
            dir: Some(sub_dir.clone()),
            generate_only: true,
            force: true,
            ..Default::default()
        })
        .await
        .expect("new_project");
    assert!(!result.stdout.is_empty(), "stdout must carry CLI output");

    let contents =
        std::fs::read_to_string(sub_dir.join("Pulumi.yaml")).expect("Pulumi.yaml in the sub dir");
    assert!(
        contents.contains("name: sub-project"),
        "unexpected project file: {contents}"
    );
    assert!(
        contents.contains("description: A sub-project for testing"),
        "unexpected project file: {contents}"
    );
}

/// After rename the stack is manageable under the new name and the old
/// name 404s. Ports py:test_stack_rename.
#[tokio::test]
async fn stack_rename_moves_the_stack_to_the_new_name() {
    require_cli!();
    let env = TestEnv::new();
    std::fs::write(
        env.project_dir().join("Pulumi.yaml"),
        "name: renametest\nruntime: yaml\n",
    )
    .unwrap();
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;
    let mut stack = Stack::create("dev", ws).await.expect("stack");

    stack
        .rename(RenameOptions {
            stack_name: "dev-renamed".to_string(),
            ..Default::default()
        })
        .await
        .expect("rename");
    assert_eq!(stack.name(), "dev-renamed");

    // The old name is gone...
    let missing = stack
        .workspace()
        .select_stack("dev")
        .await
        .expect_err("old name must not select");
    assert!(
        missing.is_select_stack_404_error(),
        "unexpected error: {missing}"
    );

    // ...and the stack is fully manageable under the new one.
    stack
        .set_config("key", &ConfigValue::plain("value"))
        .await
        .expect("config under the new name");
    stack
        .workspace()
        .remove_stack("dev-renamed", false)
        .await
        .expect("remove under the new name");
}

/// list_stacks sees only the current project; the all option sees stacks
/// of both projects sharing the backend. Ports go:TestListAllStacks
/// against a real backend.
#[tokio::test]
async fn list_stacks_all_spans_projects() {
    require_cli!();
    let env = TestEnv::new();
    std::fs::write(
        env.project_dir().join("Pulumi.yaml"),
        "name: lsproja\nruntime: yaml\n",
    )
    .unwrap();
    let other_dir = env.root.join("other-project");
    std::fs::create_dir_all(&other_dir).unwrap();
    std::fs::write(
        other_dir.join("Pulumi.yaml"),
        "name: lsprojb\nruntime: yaml\n",
    )
    .unwrap();

    let ws_a = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;
    let ws_b = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(other_dir),
            ..Default::default()
        })
        .await;
    ws_a.create_stack("deva").await.expect("create deva");
    ws_b.create_stack("devb").await.expect("create devb");

    let own = ws_a.list_stacks().await.expect("list");
    assert_eq!(own.len(), 1, "own stacks: {own:?}");
    assert!(own[0].name.contains("deva"), "own stacks: {own:?}");

    let all = ws_a
        .list_stacks_with_options(&ListOptions { all: true })
        .await
        .expect("list all");
    assert!(
        all.iter().any(|s| s.name.contains("deva")),
        "all stacks: {all:?}"
    );
    assert!(
        all.iter().any(|s| s.name.contains("devb")),
        "all stacks: {all:?}"
    );

    ws_a.remove_stack("deva", false).await.expect("remove deva");
    ws_b.remove_stack("devb", false).await.expect("remove devb");
}

/// A component placeholder imports without any provider plugin: the
/// plugin-free import shape.
#[tokio::test]
async fn import_resources_imports_a_component_placeholder() {
    require_cli!();
    let env = TestEnv::new();
    std::fs::write(
        env.project_dir().join("Pulumi.yaml"),
        "name: importcomp\nruntime: yaml\n",
    )
    .unwrap();
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;
    let stack = Stack::create("dev", ws).await.expect("stack");

    let import = stack
        .import_resources(ImportOptions {
            resources: Some(vec![ImportResource {
                type_: "my:module:MyResource".to_string(),
                name: "imported-resource".to_string(),
                component: true,
                ..Default::default()
            }]),
            protect: Some(false),
            // The CLI cannot generate code for a bare component; state
            // still imports fine.
            generate_code: Some(false),
            ..Default::default()
        })
        .await
        .expect("import");
    let summary = import.summary.expect("import summary");
    assert_eq!(summary.kind, "resource-import");
    assert_eq!(summary.result.as_deref(), Some("succeeded"));
    assert_eq!(
        summary
            .resource_changes
            .as_ref()
            .and_then(|c| c.get("import")),
        Some(&1),
        "changes: {:?}",
        summary.resource_changes
    );
    assert!(import.generated_code.is_empty());

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// preview_only previews the import without touching state. Ports
/// go:TestPreviewImportResources.
#[tokio::test]
async fn import_resources_preview_only_changes_nothing() {
    require_cli!();
    let env = TestEnv::new();
    std::fs::write(
        env.project_dir().join("Pulumi.yaml"),
        "name: importprev\nruntime: yaml\n",
    )
    .unwrap();
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;
    let stack = Stack::create("dev", ws).await.expect("stack");

    let import_file = env.root.join("import.json");
    std::fs::write(
        &import_file,
        r#"{"resources":[{"type":"my:module:MyResource","name":"imported-resource","component":true}]}"#,
    )
    .unwrap();
    let import = stack
        .import_resources(ImportOptions {
            import_file: Some(import_file),
            protect: Some(false),
            generate_code: Some(true),
            preview_only: true,
            ..Default::default()
        })
        .await
        .expect("preview import");
    assert!(
        import.stdout.contains("Previewing"),
        "stdout: {}",
        import.stdout
    );
    assert!(
        !import.stdout.contains("Importing"),
        "stdout: {}",
        import.stdout
    );

    // Nothing landed in the state or the history.
    let history = stack.history(None, 0, None).await.expect("history");
    assert!(history.is_empty(), "history: {history:?}");

    stack
        .workspace()
        .remove_stack("dev", false)
        .await
        .expect("remove");
}

/// Importing a random-provider resource returns generated code, and the
/// generate_code opt-out suppresses it. Ports go:TestStackImportResources
/// and ...WithoutGenerateCode; skips only on genuine network failures.
#[tokio::test]
async fn import_resources_with_the_random_provider() {
    require_cli!();
    let env = TestEnv::new();
    std::fs::write(
        env.project_dir().join("Pulumi.yaml"),
        "name: importrandom\nruntime: yaml\n",
    )
    .unwrap();
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;

    // The install downloads from the registry; skip only on network
    // failures so an argv regression cannot pass vacuously.
    if let Err(err) = ws.install_plugin("random", "4.16.3").await {
        if !is_network_failure(&err) {
            panic!("install_plugin failed for a non-network reason: {err}");
        }
        eprintln!("skipping: plugin download failed: {err}");
        return;
    }
    let stack = Stack::create("dev", ws).await.expect("stack");

    let resources = || {
        Some(vec![ImportResource {
            type_: "random:index/randomPassword:RandomPassword".to_string(),
            id: "supersecret".to_string(),
            name: "randomPassword".to_string(),
            ..Default::default()
        }])
    };
    let import = stack
        .import_resources(ImportOptions {
            resources: resources(),
            protect: Some(false),
            ..Default::default()
        })
        .await
        .expect("import");
    let summary = import.summary.expect("import summary");
    assert_eq!(summary.result.as_deref(), Some("succeeded"));
    assert!(
        import.generated_code.contains("randomPassword"),
        "generated code: {}",
        import.generated_code
    );

    stack
        .destroy(DestroyOptions::default())
        .await
        .expect("destroy after first import");

    // The same import without code generation returns no code.
    let import = stack
        .import_resources(ImportOptions {
            resources: resources(),
            protect: Some(false),
            generate_code: Some(false),
            ..Default::default()
        })
        .await
        .expect("import without generate code");
    assert_eq!(
        import.summary.expect("import summary").result.as_deref(),
        Some("succeeded")
    );
    assert!(
        import.generated_code.is_empty(),
        "generated code: {}",
        import.generated_code
    );

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// The user_agent option is accepted end to end: the backend records the
/// agent in the update's environment.
#[tokio::test]
async fn user_agent_reaches_the_update_environment() {
    require_cli!();
    let env = TestEnv::new();
    let program = auto::program(|ctx| async move {
        ctx.export("ok", pulumi::pv::string("yes"));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("agent", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    let up = stack
        .up(UpOptions {
            user_agent: Some("rust-test-agent".to_string()),
            ..Default::default()
        })
        .await
        .expect("up");
    let summary = up.summary.expect("up summary");
    assert_eq!(
        summary.environment.get("exec.agent").map(String::as_str),
        Some("rust-test-agent"),
        "environment: {:?}",
        summary.environment
    );

    stack
        .destroy(DestroyOptions {
            user_agent: Some("rust-test-agent".to_string()),
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}
