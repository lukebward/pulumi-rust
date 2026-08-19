//! Integration test for CLI installation (`LocalPulumiCommand::install`):
//! one real download of a pinned older release, exercised end to end.

mod common;

use std::sync::Arc;

use pulumi::auto::{
    CommandSpec, LocalPulumiCommand, LocalWorkspace, LocalWorkspaceOptions, PulumiCommand,
    PulumiCommandOptions,
};
use semver::Version;

use common::TestEnv;

/// An old, small-footprint release; the download is ~100MB, so the whole
/// flow (install, reuse, run through a workspace) runs in this one test.
const PINNED: &str = "3.200.0";

#[cfg(unix)]
#[tokio::test]
async fn install_reuse_and_run() {
    // No require_cli!(): install provisions its own CLI, so this test must
    // run on a clean machine — exactly where the installer matters most.
    let env = TestEnv::new();
    let root = env.root.join("cli");
    let version = Version::parse(PINNED).unwrap();
    let options = PulumiCommandOptions {
        version: Some(version.clone()),
        root: Some(root.clone()),
        ..Default::default()
    };

    // The install downloads from get.pulumi.com; skip only on transport
    // failures so an installer regression cannot pass vacuously. An HTTP
    // error status (a mangled release URL 404s) reports as "unexpected
    // HTTP status", matches no needle, and fails the test.
    let cmd = match LocalPulumiCommand::install(options.clone()).await {
        Ok(cmd) => cmd,
        Err(err) => {
            let text = err.to_string().to_lowercase();
            let network_failure = [
                "failed to download",
                "dns",
                "connection",
                "lookup",
                "timeout",
            ]
            .iter()
            .any(|needle| text.contains(needle));
            if !network_failure {
                panic!("install failed for a non-network reason: {err}");
            }
            eprintln!("skipping: CLI download failed: {err}");
            return;
        }
    };
    assert_eq!(cmd.version(), version);

    // The installed binary runs and reports the pinned version.
    let result = cmd
        .run(CommandSpec {
            args: vec!["version".to_string()],
            workdir: env.root.clone(),
            env: vec![("PULUMI_SKIP_UPDATE_CHECK".to_string(), "true".to_string())],
            ..Default::default()
        })
        .await
        .expect("installed pulumi runs");
    assert_eq!(result.stdout.trim().trim_start_matches('v'), PINNED);

    // Installing again reuses the existing binary: same mtime, and a
    // marker dropped beside it survives (no wipe-and-redownload).
    let binary = root.join("bin").join("pulumi");
    let before = std::fs::metadata(&binary)
        .expect("binary")
        .modified()
        .unwrap();
    let marker = root.join("bin").join(".reuse-marker");
    std::fs::write(&marker, "untouched").unwrap();
    let cmd = LocalPulumiCommand::install(options)
        .await
        .expect("re-install");
    assert_eq!(cmd.version(), version);
    let after = std::fs::metadata(&binary).unwrap().modified().unwrap();
    assert_eq!(before, after, "re-install must not replace the binary");
    assert!(marker.exists(), "re-install must not touch the install dir");

    // The installed binary works end to end through a workspace. (The PATH
    // composition itself is pinned by the fixup_path unit test; whoami
    // spawns the CLI by absolute path and makes no PATH lookup.)
    let ws = LocalWorkspace::new(LocalWorkspaceOptions {
        work_dir: Some(env.project_dir()),
        env_vars: env.env_vars(),
        pulumi_command: Some(Arc::new(cmd)),
        ..Default::default()
    })
    .await
    .expect("workspace over the installed CLI");
    let who = ws.whoami().await.expect("whoami over the installed CLI");
    assert!(!who.user.is_empty(), "whoami: {who:?}");
}
