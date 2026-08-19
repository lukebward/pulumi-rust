//! Integration tests for git-sourced local workspaces, porting Go's
//! TestGitClone against a local fixture repository built on disk — no
//! network, fully deterministic — plus a shallow clone and a full stack
//! lifecycle from a cloned workspace against the file backend (standing
//! in for Go's network-bound TestNewStackRemoteSource). Skips without
//! `pulumi` or `git` on `PATH`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use common::TestEnv;
use pulumi::auto::{
    DestroyOptions, GitRepo, LocalWorkspace, LocalWorkspaceOptions, Stack, UpOptions,
};

/// Whether a runnable `git` is on PATH; tests skip quietly otherwise.
fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("version")
        .output()
        .is_ok()
}

macro_rules! require_git {
    () => {
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
    };
}

/// Run `git` in `dir` with identity and signing pinned, so fixture
/// building never depends on the machine's git config; panics on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=testo",
            "-c",
            "user.email=testo@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn head(dir: &Path) -> String {
    git(dir, &["rev-parse", "HEAD"])
}

/// The fixture: `default` is HEAD (so it clones as the default branch)
/// and carries a YAML program at the root and under `proj/`; `nondefault`
/// and `branch/with/slashes` sit on an earlier commit, tagged `v0.0.1` —
/// the same shape Go's TestGitClone builds.
struct FixtureRepo {
    root: PathBuf,
    default_head: String,
    nondefault_head: String,
}

impl FixtureRepo {
    fn build(env: &TestEnv) -> Self {
        let root = env.root.join("origin");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        git(
            &root,
            &["commit", "--allow-empty", "-m", "nondefault branch"],
        );
        let nondefault_head = head(&root);
        git(&root, &["checkout", "--quiet", "-b", "nondefault"]);
        git(&root, &["tag", "v0.0.1"]);
        git(&root, &["checkout", "--quiet", "-b", "branch/with/slashes"]);
        git(&root, &["checkout", "--quiet", "-b", "default"]);
        std::fs::write(
            root.join("Pulumi.yaml"),
            "name: gitproj\nruntime: yaml\noutputs:\n  fixed: hello\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("proj")).unwrap();
        std::fs::write(
            root.join("proj").join("Pulumi.yaml"),
            "name: gitprojsub\nruntime: yaml\noutputs:\n  where: subdir\n",
        )
        .unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-m", "default branch"]);
        let default_head = head(&root);
        FixtureRepo {
            root,
            default_head,
            nondefault_head,
        }
    }

    fn url(&self) -> String {
        self.root.display().to_string()
    }
}

async fn clone_into(
    env: &TestEnv,
    tag: &str,
    repo: GitRepo,
) -> pulumi::auto::Result<LocalWorkspace> {
    let target = env.root.join(format!("clone-{tag}"));
    std::fs::create_dir_all(&target).unwrap();
    LocalWorkspace::new(LocalWorkspaceOptions {
        work_dir: Some(target),
        env_vars: env.env_vars(),
        repo: Some(repo),
        ..Default::default()
    })
    .await
}

/// Ports go:TestGitClone's success table: every branch spelling checks
/// out the expected commit, and a commit hash checks out exactly.
#[tokio::test]
async fn git_clone_checks_out_each_ref_shape() {
    require_cli!();
    require_git!();
    let env = TestEnv::new();
    let fixture = FixtureRepo::build(&env);

    let default = fixture.default_head.as_str();
    let nondefault = fixture.nondefault_head.as_str();
    let cases: Vec<(&str, Option<&str>, Option<&str>, &str)> = vec![
        // (name, branch, commit_hash, expected head)
        ("plain-default", Some("default"), None, default),
        ("plain-nondefault", Some("nondefault"), None, nondefault),
        ("slashes", Some("branch/with/slashes"), None, nondefault),
        ("heads-default", Some("refs/heads/default"), None, default),
        (
            "heads-nondefault",
            Some("refs/heads/nondefault"),
            None,
            nondefault,
        ),
        (
            "heads-slashes",
            Some("refs/heads/branch/with/slashes"),
            None,
            nondefault,
        ),
        (
            "remotes-default",
            Some("refs/remotes/origin/default"),
            None,
            default,
        ),
        (
            "remotes-nondefault",
            Some("refs/remotes/origin/nondefault"),
            None,
            nondefault,
        ),
        (
            "remotes-slashes",
            Some("refs/remotes/origin/branch/with/slashes"),
            None,
            nondefault,
        ),
        ("tag", Some("refs/tags/v0.0.1"), None, nondefault),
        ("no-ref", None, None, default),
        ("hash-default", None, Some(default), default),
        ("hash-nondefault", None, Some(nondefault), nondefault),
    ];

    for (name, branch, commit_hash, expected) in cases {
        let ws = clone_into(
            &env,
            name,
            GitRepo {
                url: fixture.url(),
                branch: branch.map(|b| b.to_string()),
                commit_hash: commit_hash.map(|h| h.to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap_or_else(|e| panic!("case {name}: {e}"));
        assert_eq!(head(ws.work_dir()), expected, "case {name}");
    }
}

/// Ports go:TestGitClone's error table.
#[tokio::test]
async fn git_clone_rejects_invalid_refs() {
    require_cli!();
    require_git!();
    let env = TestEnv::new();
    let fixture = FixtureRepo::build(&env);

    let cases: Vec<(&str, &str, &str)> = vec![
        // (name, branch, expected error fragment)
        ("missing-branch", "doesnotexist", "unable to clone repo"),
        (
            "missing-full-branch",
            "refs/heads/doesnotexist",
            "unable to clone repo",
        ),
        (
            "malformed-ref",
            "refs/notathing/default",
            "unable to clone repo",
        ),
        ("bare-tag-name", "v1.0.0", "unable to clone repo"),
        (
            "wrong-remote",
            "refs/remotes/upstream/default",
            "a remote ref must begin with 'refs/remote/origin/', \
             but got \"refs/remotes/upstream/default\"",
        ),
    ];

    for (name, branch, expected) in cases {
        let err = clone_into(
            &env,
            name,
            GitRepo {
                url: fixture.url(),
                branch: Some(branch.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err(name);
        let message = err.to_string();
        assert!(
            message.contains(expected),
            "case {name}: expected {expected:?} in {message:?}"
        );
        assert!(
            message.contains("unable to enlist in git repo"),
            "case {name}: message {message:?}"
        );
    }
}

/// A shallow clone has depth 1 and still lands on the default head.
#[tokio::test]
async fn git_clone_shallow_produces_a_depth_one_clone() {
    require_cli!();
    require_git!();
    let env = TestEnv::new();
    let fixture = FixtureRepo::build(&env);

    let ws = clone_into(
        &env,
        "shallow",
        GitRepo {
            // git silently ignores --depth over the local-path transport;
            // a file:// URL exercises the real shallow protocol.
            url: format!("file://{}", fixture.root.display()),
            shallow: true,
            ..Default::default()
        },
    )
    .await
    .expect("shallow clone");
    assert_eq!(head(ws.work_dir()), fixture.default_head);
    assert_eq!(
        git(ws.work_dir(), &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
}

/// The replacement for Go's network-bound TestNewStackRemoteSource: a
/// stack created from a cloned workspace runs the committed YAML program
/// end to end against the file backend, honoring `project_path` and the
/// setup callback.
#[tokio::test]
async fn stack_from_cloned_workspace_full_lifecycle() {
    require_cli!();
    require_git!();
    let env = TestEnv::new();
    let fixture = FixtureRepo::build(&env);

    let setup_ran = Arc::new(AtomicBool::new(false));
    let observed = setup_ran.clone();
    let ws = clone_into(
        &env,
        "lifecycle",
        GitRepo {
            url: fixture.url(),
            branch: Some("default".to_string()),
            project_path: Some(PathBuf::from("proj")),
            setup: Some(Arc::new(move |ws: LocalWorkspace| {
                let observed = observed.clone();
                let fut: futures::future::BoxFuture<'static, pulumi::auto::Result<()>> =
                    Box::pin(async move {
                        assert!(ws.has_project_settings(), "clone precedes setup");
                        observed.store(true, Ordering::SeqCst);
                        Ok(())
                    });
                fut
            })),
            ..Default::default()
        },
    )
    .await
    .expect("cloned workspace");
    assert!(setup_ran.load(Ordering::SeqCst), "setup callback ran");
    assert!(
        ws.work_dir().ends_with("proj"),
        "work dir honors project_path"
    );
    assert_eq!(ws.project_settings().expect("settings").name, "gitprojsub");

    let stack = Stack::create("dev", ws).await.expect("stack");
    let up = stack.up(UpOptions::default()).await.expect("up");
    assert_eq!(up.outputs["where"].value, serde_json::json!("subdir"));

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}
