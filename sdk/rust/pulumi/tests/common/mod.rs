//! Shared harness for the automation-API integration tests: an isolated
//! file backend, `PULUMI_HOME`, and passphrase per test, mirroring the
//! `TestEnv` in auto.rs (kept separate so auto.rs stays self-contained).

// Each test binary includes this module; not all of them use every helper.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pulumi::auto::{LocalWorkspace, LocalWorkspaceOptions};

const PASSPHRASE: &str = "correct horse battery staple";

/// Whether a runnable `pulumi` is on PATH; tests skip quietly otherwise.
pub fn cli_available() -> bool {
    std::process::Command::new("pulumi")
        .arg("version")
        .env("PULUMI_SKIP_UPDATE_CHECK", "true")
        .output()
        .is_ok()
}

#[macro_export]
macro_rules! require_cli {
    () => {
        if !$crate::common::cli_available() {
            eprintln!("skipping: pulumi CLI not on PATH");
            return;
        }
    };
}

pub struct TestEnv {
    pub root: PathBuf,
}

impl TestEnv {
    pub fn new() -> Self {
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

    pub fn project_dir(&self) -> PathBuf {
        self.root.join("project")
    }

    fn backend_url(&self) -> String {
        format!("file://{}", self.root.join("state").display())
    }

    pub fn env_vars(&self) -> HashMap<String, String> {
        HashMap::from([
            ("PULUMI_BACKEND_URL".to_string(), self.backend_url()),
            (
                "PULUMI_CONFIG_PASSPHRASE".to_string(),
                PASSPHRASE.to_string(),
            ),
            (
                "PULUMI_HOME".to_string(),
                self.root.join("home").display().to_string(),
            ),
        ])
    }

    pub async fn workspace(&self, options: LocalWorkspaceOptions) -> LocalWorkspace {
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
