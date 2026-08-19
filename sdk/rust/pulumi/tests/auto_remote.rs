//! Integration test for remote workspaces, porting Go's
//! TestNewRemoteStackGitSource. Execution happens in Pulumi Deployments,
//! so the test needs a Pulumi Cloud access token with Deployments access;
//! it skips quietly unless `PULUMI_ACCESS_TOKEN` is set, keeping
//! `make test_sdk` hermetic. Its value is CI running with a token.

mod common;

use pulumi::auto::events::OpType;
use pulumi::auto::{
    LocalWorkspace, LocalWorkspaceOptions, RemoteDestroyOptions, RemoteGitRepo,
    RemotePreviewOptions, RemoteRefreshOptions, RemoteStack, RemoteUpOptions,
    RemoteWorkspaceOptions,
};

const TEST_REPO: &str = "https://github.com/pulumi/test-repo.git";
const TEST_BRANCH: &str = "refs/heads/master";

fn test_org() -> String {
    std::env::var("PULUMI_TEST_ORG").unwrap_or_else(|_| "pulumi-test".to_string())
}

fn random_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{}{nanos}", std::process::id())
}

#[tokio::test]
async fn remote_git_source_full_lifecycle() {
    require_cli!();
    if std::env::var("PULUMI_ACCESS_TOKEN")
        .unwrap_or_default()
        .is_empty()
    {
        eprintln!("skipping: PULUMI_ACCESS_TOKEN is not set");
        return;
    }

    // The project must be goproj's own project name (test-repo pins it as
    // go_remote_proj); Deployments rejects a stack whose project component
    // does not match the program's Pulumi.yaml.
    let stack_name = format!("{}/go_remote_proj/int_test{}", test_org(), random_suffix());
    let repo = RemoteGitRepo {
        url: TEST_REPO.to_string(),
        project_path: Some("goproj".into()),
        branch: Some(TEST_BRANCH.to_string()),
        ..Default::default()
    };
    let options = RemoteWorkspaceOptions {
        pre_run_commands: vec![
            format!("pulumi config set bar abc --stack {stack_name}"),
            format!("pulumi config set --secret buzz secret --stack {stack_name}"),
        ],
        skip_install_dependencies: true,
        ..Default::default()
    };

    let stack = RemoteStack::create_git_source(&stack_name, repo, options)
        .await
        .expect("failed to initialize stack");

    // The lifecycle runs in a task so a failed assertion still reaches
    // the stack removal below, as Go's deferred RemoveStack does.
    let lifecycle = tokio::spawn(async move {
        // -- pulumi up --
        let up = stack.up(RemoteUpOptions::default()).await.expect("up");
        assert_eq!(up.outputs.len(), 3, "outputs: {:?}", up.outputs);
        assert_eq!(up.outputs["exp_static"].value, serde_json::json!("foo"));
        assert!(!up.outputs["exp_static"].secret);
        assert_eq!(up.outputs["exp_cfg"].value, serde_json::json!("abc"));
        assert!(!up.outputs["exp_cfg"].secret);
        assert_eq!(up.outputs["exp_secret"].value, serde_json::json!("secret"));
        assert!(up.outputs["exp_secret"].secret);
        let summary = up.summary.expect("up summary");
        assert_eq!(summary.kind, "update");
        assert_eq!(summary.result.as_deref(), Some("succeeded"));

        // -- pulumi preview --
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let preview = stack
            .preview(RemotePreviewOptions {
                event_senders: vec![event_tx],
            })
            .await
            .expect("preview");
        assert_eq!(preview.change_summary.get(&OpType::Same), Some(&1));
        let mut steps = 0;
        while let Some(event) = event_rx.recv().await {
            if event.resource_pre_event.is_some() {
                steps += 1;
            }
        }
        assert_eq!(steps, 1, "expected one preview step");

        // -- pulumi refresh --
        let refresh = stack
            .refresh(RemoteRefreshOptions::default())
            .await
            .expect("refresh");
        let summary = refresh.summary.expect("refresh summary");
        assert_eq!(summary.kind, "refresh");
        assert_eq!(summary.result.as_deref(), Some("succeeded"));

        // -- pulumi destroy --
        let destroy = stack
            .destroy(RemoteDestroyOptions::default())
            .await
            .expect("destroy");
        let summary = destroy.summary.expect("destroy summary");
        assert_eq!(summary.kind, "destroy");
        assert_eq!(summary.result.as_deref(), Some("succeeded"));
    });
    let outcome = lifecycle.await;

    // -- pulumi stack rm --
    let workspace = LocalWorkspace::new(LocalWorkspaceOptions::default())
        .await
        .expect("workspace");
    workspace
        .remove_stack(&stack_name, false)
        .await
        .expect("failed to remove stack. Resources have leaked.");

    if let Err(err) = outcome {
        std::panic::resume_unwind(err.into_panic());
    }
}
