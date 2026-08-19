//! Error-classification, event-stream, and isolation integration tests
//! for the automation API, driving a real `pulumi` CLI against a local
//! file backend. Tests skip when `pulumi` (or, where noted, `go`) is not
//! on PATH.

mod common;

use std::path::Path;
use std::time::Duration;

use pulumi::auto::{
    self, ConfigValue, DestroyOptions, EngineEvent, LocalWorkspaceOptions, PreviewOptions,
    ProjectSettings, RefreshOptions, Stack, UpOptions,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::time::timeout;

use common::TestEnv;

macro_rules! require_go {
    () => {
        if std::process::Command::new("go")
            .arg("version")
            .output()
            .is_err()
        {
            eprintln!("skipping: go toolchain not on PATH");
            return;
        }
    };
}

fn write_go_project(dir: &Path, project: &str, main_go: &str) {
    std::fs::write(
        dir.join("Pulumi.yaml"),
        format!("name: {project}\nruntime: go\n"),
    )
    .unwrap();
    // No dependencies, so the toolchain never touches the network.
    std::fs::write(dir.join("go.mod"), "module fixture\n\ngo 1.21\n").unwrap();
    std::fs::write(dir.join("main.go"), main_go).unwrap();
}

/// Drain an event channel until it closes, reporting whether a summary
/// event arrived; bounded so a leaked channel cannot hang the test.
async fn saw_summary_event(mut rx: UnboundedReceiver<EngineEvent>) -> bool {
    let drain = async move {
        let mut saw = false;
        while let Some(event) = rx.recv().await {
            saw |= event.summary_event.is_some();
        }
        saw
    };
    timeout(Duration::from_secs(60), drain)
        .await
        .unwrap_or(false)
}

/// Two simultaneous ups on one stack: the loser classifies as a
/// concurrent-update error from the file-backend lock, the winner completes.
#[tokio::test]
async fn concurrent_up_loser_classifies_and_winner_completes() {
    require_cli!();
    let env = TestEnv::new();

    // The winner must hold the backend lock until the loser has hit its
    // lock check: an inline program that signals `started` and then waits
    // for an explicit release. (A plain YAML up releases the lock in about
    // a second, and the CLI ignores locks whose owner has exited.) The
    // fallback timeout keeps a failed assertion from hanging the winner.
    let (started_tx, mut started_rx) = unbounded_channel::<()>();
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_rx = release.clone();
    let program = auto::program(move |ctx| {
        let started_tx = started_tx.clone();
        let release_rx = release_rx.clone();
        async move {
            let _ = started_tx.send(());
            let _ = timeout(Duration::from_secs(120), release_rx.notified()).await;
            ctx.export("winner", pulumi::pv::string("done"));
            Ok(())
        }
    });
    let winner_ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("race", "rust")),
            ..Default::default()
        })
        .await;
    let winner = Stack::create_or_select("dev", winner_ws)
        .await
        .expect("winner stack");

    // The loser is a local-source up on the same project and stack.
    std::fs::write(
        env.project_dir().join("Pulumi.yaml"),
        "name: race\nruntime: yaml\noutputs:\n  fixed: hello\n",
    )
    .unwrap();
    let loser_ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;
    let loser = Stack::select("dev", loser_ws).await.expect("loser stack");

    let racing = winner.clone();
    let first = tokio::spawn(async move { racing.up(UpOptions::default()).await });
    timeout(Duration::from_secs(120), started_rx.recv())
        .await
        .expect("winner program never started")
        .expect("started signal");

    let err = timeout(Duration::from_secs(120), loser.up(UpOptions::default()))
        .await
        .expect("loser up hung")
        .expect_err("the loser should hit the backend lock");
    assert!(err.is_concurrent_update_error(), "unexpected error: {err}");

    release.notify_one();
    let up = timeout(Duration::from_secs(180), first)
        .await
        .expect("winner up hung")
        .expect("join winner")
        .expect("winner up");
    assert_eq!(up.outputs["winner"].value, serde_json::json!("done"));

    winner
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// up on a Go program that fails to compile classifies as a compilation error.
#[tokio::test]
async fn go_compilation_error_classifies() {
    require_cli!();
    require_go!();
    let env = TestEnv::new();
    write_go_project(
        &env.project_dir(),
        "compile-err",
        "package main\n\nfunc main() {\n\tvar x =\n}\n",
    );
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    let err = timeout(Duration::from_secs(300), stack.up(UpOptions::default()))
        .await
        .expect("up hung")
        .expect_err("up must fail to compile");
    assert!(err.is_compilation_error(), "unexpected error: {err}");

    stack
        .workspace()
        .remove_stack("dev", false)
        .await
        .expect("remove");
}

/// up on a Go program that panics at runtime classifies as a runtime error.
#[tokio::test]
async fn go_runtime_error_classifies() {
    require_cli!();
    require_go!();
    let env = TestEnv::new();
    write_go_project(
        &env.project_dir(),
        "runtime-err",
        "package main\n\nfunc main() {\n\txs := []int{}\n\t_ = xs[len(xs)]\n}\n",
    );
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    let err = timeout(Duration::from_secs(300), stack.up(UpOptions::default()))
        .await
        .expect("up hung")
        .expect_err("up must fail at runtime");
    assert!(err.is_runtime_error(), "unexpected error: {err}");
    assert!(!err.is_compilation_error(), "misclassified: {err}");

    stack
        .workspace()
        .remove_stack("dev", false)
        .await
        .expect("remove");
}

/// A failed up does not poison the stack: flip the config and the next
/// up on the same stack succeeds.
#[tokio::test]
async fn failed_up_does_not_poison_the_stack() {
    require_cli!();
    let env = TestEnv::new();

    let program = auto::program(|ctx| async move {
        if ctx.config().get("fail").as_deref() == Some("true") {
            return Err(pulumi::Error::new("deliberate config-driven failure"));
        }
        ctx.export("status", pulumi::pv::string("recovered"));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("poison", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    stack
        .set_config("poison:fail", &ConfigValue::plain("true"))
        .await
        .expect("set config");
    let err = timeout(Duration::from_secs(180), stack.up(UpOptions::default()))
        .await
        .expect("first up hung")
        .expect_err("first up must fail");
    assert!(
        err.to_string().contains("deliberate config-driven failure"),
        "error was: {err}"
    );

    stack
        .set_config("poison:fail", &ConfigValue::plain("false"))
        .await
        .expect("set config");
    let up = timeout(Duration::from_secs(180), stack.up(UpOptions::default()))
        .await
        .expect("second up hung")
        .expect("second up");
    assert_eq!(up.outputs["status"].value, serde_json::json!("recovered"));

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// As in Go, a nested local-source operation started from inside an
/// inline program completes without deadlocking, while a nested inline
/// operation (which used to deadlock) fails fast with Go's
/// nested-operation error.
#[tokio::test]
async fn nested_stack_operations_match_go_inside_inline_programs() {
    require_cli!();
    let env = TestEnv::new();

    let inner_dir = env.root.join("inner-project");
    std::fs::create_dir_all(&inner_dir).unwrap();
    std::fs::write(
        inner_dir.join("Pulumi.yaml"),
        "name: inner\nruntime: yaml\noutputs:\n  fixed: hello\n",
    )
    .unwrap();
    let inner_ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(inner_dir),
            ..Default::default()
        })
        .await;
    let inner = Stack::create_or_select("dev", inner_ws)
        .await
        .expect("inner stack");

    let inline_program = auto::program(|ctx| async move {
        ctx.export("n", pulumi::pv::string("1"));
        Ok(())
    });
    let inner_inline_ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(inline_program),
            project_settings: Some(ProjectSettings::new("inner-inline", "rust")),
            ..Default::default()
        })
        .await;
    let inner_inline = Stack::create_or_select("dev", inner_inline_ws)
        .await
        .expect("inner inline stack");

    let (outcome_tx, mut outcome_rx) = unbounded_channel::<(String, String)>();
    let nested_local = inner.clone();
    let nested_inline = inner_inline.clone();
    let program = auto::program(move |ctx| {
        let nested_local = nested_local.clone();
        let nested_inline = nested_inline.clone();
        let outcome_tx = outcome_tx.clone();
        async move {
            let describe = |outcome: Result<_, auto::Error>| match outcome {
                Ok(_) => "success".to_string(),
                Err(e) => e.to_string(),
            };
            let local = describe(nested_local.up(UpOptions::default()).await);
            let inline = describe(nested_inline.up(UpOptions::default()).await);
            let _ = outcome_tx.send((local, inline));
            ctx.export("outer", pulumi::pv::string("done"));
            Ok(())
        }
    });
    let outer_ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("outer", "rust")),
            ..Default::default()
        })
        .await;
    let outer = Stack::create_or_select("dev", outer_ws)
        .await
        .expect("outer stack");

    let up = timeout(Duration::from_secs(300), outer.up(UpOptions::default()))
        .await
        .expect("nested operation deadlocked")
        .expect("outer up");
    assert_eq!(up.outputs["outer"].value, serde_json::json!("done"));
    let (local, inline) = outcome_rx.recv().await.expect("nested outcomes");
    assert_eq!(local, "success", "local-source outcome: {local}");
    assert!(
        inline.contains("nested stack operations are not supported"),
        "inline outcome: {inline}"
    );

    inner
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("inner destroy");
    // The nested inline op never ran, so removing its stack suffices.
    inner_inline
        .workspace()
        .remove_stack("dev", false)
        .await
        .expect("inner inline remove");
    outer
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("outer destroy");
}

/// up with an unsupported color option surfaces the CLI stderr, and the
/// stack (and its event watcher) work again afterwards.
#[tokio::test]
async fn invalid_color_option_errors_and_stack_recovers() {
    require_cli!();
    let env = TestEnv::new();

    let program = auto::program(|ctx| async move {
        ctx.export("ok", pulumi::pv::string("fine"));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("colors", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    let (bad_tx, mut bad_rx) = unbounded_channel::<EngineEvent>();
    let err = timeout(
        Duration::from_secs(180),
        stack.up(UpOptions {
            color: Some("bogus".to_string()),
            event_senders: vec![bad_tx],
            ..Default::default()
        }),
    )
    .await
    .expect("bogus-color up hung")
    .expect_err("bogus color must fail");
    let result = err.command_result().expect("a command error with streams");
    assert!(
        result.stderr.contains("bogus") && result.stderr.contains("color"),
        "stderr was: {}",
        result.stderr
    );
    // The failed run still closes its event channel rather than leaking it.
    let drained = timeout(Duration::from_secs(60), async move {
        while bad_rx.recv().await.is_some() {}
    })
    .await;
    assert!(
        drained.is_ok(),
        "event channel from the failed up never closed"
    );

    let (tx, rx) = unbounded_channel::<EngineEvent>();
    let up = timeout(
        Duration::from_secs(180),
        stack.up(UpOptions {
            event_senders: vec![tx],
            ..Default::default()
        }),
    )
    .await
    .expect("recovery up hung")
    .expect("recovery up");
    assert_eq!(up.outputs["ok"].value, serde_json::json!("fine"));
    assert!(
        saw_summary_event(rx).await,
        "no summary event after recovery"
    );

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// Aborting a running up kills the pulumi child, and a later operation
/// on the same stack succeeds.
#[tokio::test]
async fn aborted_up_kills_cli_and_stack_recovers() {
    require_cli!();
    if cfg!(windows) {
        eprintln!("skipping: pgrep unavailable");
        return;
    }
    let env = TestEnv::new();
    // Unique so pgrep matches only this test's CLI process.
    let stack_name = format!("abort-{}", std::process::id());

    let (started_tx, mut started_rx) = unbounded_channel::<()>();
    let program = auto::program(move |ctx| {
        let started_tx = started_tx.clone();
        async move {
            let _ = started_tx.send(());
            tokio::time::sleep(Duration::from_secs(10)).await;
            ctx.export("late", pulumi::pv::string("done"));
            Ok(())
        }
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("abort-test", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select(&stack_name, ws)
        .await
        .expect("stack");

    let victim = stack.clone();
    let handle = tokio::spawn(async move { victim.up(UpOptions::default()).await });
    timeout(Duration::from_secs(120), started_rx.recv())
        .await
        .expect("program never started")
        .expect("started signal");

    let pattern = format!("{stack_name}$");
    let cli_alive = || {
        std::process::Command::new("pgrep")
            .args(["-f", &pattern])
            .output()
            .expect("pgrep runs")
            .status
            .success()
    };
    // The pattern must match the live child, or the death poll below
    // proves nothing when the CLI command line changes shape.
    assert!(
        cli_alive(),
        "pgrep -f {pattern} found no live pulumi child before the abort"
    );
    handle.abort();

    // kill_on_drop must take the pulumi child down with the aborted task.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if !cli_alive() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "pulumi child survived the abort"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The killed CLI cannot release its backend lock; the CLI ignores
    // locks of dead processes, and cancel covers the case where it does
    // not.
    let retry = timeout(Duration::from_secs(300), stack.up(UpOptions::default()))
        .await
        .expect("retry up hung");
    let up = match retry {
        Ok(up) => up,
        Err(e) if e.is_concurrent_update_error() => {
            stack.cancel().await.expect("cancel");
            timeout(Duration::from_secs(300), stack.up(UpOptions::default()))
                .await
                .expect("up after cancel hung")
                .expect("up after cancel")
        }
        Err(e) => panic!("unexpected retry error: {e}"),
    };
    assert_eq!(up.outputs["late"].value, serde_json::json!("done"));

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// preview, refresh, and destroy each stream a summary event through
/// event_senders (up is covered in tests/auto.rs).
#[tokio::test]
async fn preview_refresh_destroy_each_stream_a_summary_event() {
    require_cli!();
    let env = TestEnv::new();

    let program = auto::program(|ctx| async move {
        ctx.export("n", pulumi::pv::string("1"));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("event-streams", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");
    timeout(Duration::from_secs(180), stack.up(UpOptions::default()))
        .await
        .expect("up hung")
        .expect("up");

    let (tx, rx) = unbounded_channel::<EngineEvent>();
    timeout(
        Duration::from_secs(180),
        stack.preview(PreviewOptions {
            event_senders: vec![tx],
            ..Default::default()
        }),
    )
    .await
    .expect("preview hung")
    .expect("preview");
    assert!(saw_summary_event(rx).await, "no summary event from preview");

    let (tx, rx) = unbounded_channel::<EngineEvent>();
    timeout(
        Duration::from_secs(180),
        stack.refresh(RefreshOptions {
            event_senders: vec![tx],
            ..Default::default()
        }),
    )
    .await
    .expect("refresh hung")
    .expect("refresh");
    assert!(saw_summary_event(rx).await, "no summary event from refresh");

    let (tx, rx) = unbounded_channel::<EngineEvent>();
    timeout(
        Duration::from_secs(180),
        stack.destroy(DestroyOptions {
            remove: true,
            event_senders: vec![tx],
            ..Default::default()
        }),
    )
    .await
    .expect("destroy hung")
    .expect("destroy");
    assert!(saw_summary_event(rx).await, "no summary event from destroy");
}

/// Four parallel stack lifecycles keep their configs and outputs
/// isolated: local-source stacks run truly in parallel, inline stacks
/// queue by design.
#[tokio::test]
async fn parallel_stack_lifecycles_stay_isolated() {
    require_cli!();

    async fn local_lifecycle(project: &str, value: &str) {
        let env = TestEnv::new();
        std::fs::write(
            env.project_dir().join("Pulumi.yaml"),
            format!(
                "name: {project}\nruntime: yaml\nconfig:\n  bar:\n    type: string\n    default: unset\noutputs:\n  fromConfig: ${{bar}}\n"
            ),
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
            .set_config("bar", &ConfigValue::plain(value))
            .await
            .expect("set config");
        let up = stack.up(UpOptions::default()).await.expect("up");
        assert_eq!(up.outputs["fromConfig"].value, serde_json::json!(value));
        stack
            .destroy(DestroyOptions {
                remove: true,
                ..Default::default()
            })
            .await
            .expect("destroy");
    }

    async fn inline_lifecycle(project: &str, value: &str) {
        let env = TestEnv::new();
        let program = auto::program(|ctx| async move {
            let val = ctx.config().get("val").unwrap_or_default();
            ctx.export("val", pulumi::pv::string(val));
            Ok(())
        });
        let ws = env
            .workspace(LocalWorkspaceOptions {
                program: Some(program),
                project_settings: Some(ProjectSettings::new(project, "rust")),
                ..Default::default()
            })
            .await;
        let stack = Stack::create_or_select("dev", ws).await.expect("stack");
        stack
            .set_config(&format!("{project}:val"), &ConfigValue::plain(value))
            .await
            .expect("set config");
        let up = stack.up(UpOptions::default()).await.expect("up");
        assert_eq!(up.outputs["val"].value, serde_json::json!(value));
        stack
            .destroy(DestroyOptions {
                remove: true,
                ..Default::default()
            })
            .await
            .expect("destroy");
    }

    timeout(Duration::from_secs(600), async {
        tokio::join!(
            local_lifecycle("par-local-one", "alpha"),
            local_lifecycle("par-local-two", "beta"),
            inline_lifecycle("par-inline-one", "gamma"),
            inline_lifecycle("par-inline-two", "delta"),
        )
    })
    .await
    .expect("parallel lifecycles timed out");
}
