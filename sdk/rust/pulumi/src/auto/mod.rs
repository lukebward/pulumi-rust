//! The Pulumi Automation API for Rust: drive deployments from Rust code
//! instead of the `pulumi` command line.
//!
//! This is the embedding surface of the SDK. Where the rest of the crate
//! is for *writing* Pulumi programs, this module is for *running* them —
//! from a service, an operator, a CLI of your own — with the
//! [`pulumi` CLI](https://www.pulumi.com/docs/install/) doing the heavy
//! lifting underneath. It mirrors the Go SDK's `auto` package.
//!
//! Programs come in two shapes:
//!
//! - **Local programs** are ordinary Pulumi projects on disk, in any
//!   language. The workspace points at the project directory:
//!
//! ```no_run
//! use pulumi::auto::{Stack, UpOptions};
//!
//! # async fn demo() -> pulumi::auto::Result<()> {
//! let stack = Stack::create_or_select_local_source("dev", "/path/to/project").await?;
//! let up = stack.up(UpOptions::default()).await?;
//! println!("outputs: {:?}", up.outputs);
//! # Ok(())
//! # }
//! ```
//!
//! - **Inline programs** are Rust closures running in this process; the
//!   engine talks to them over an in-process language host, so the IaC
//!   itself lives inside your application:
//!
//! ```no_run
//! use pulumi::auto::{self, Stack, LocalWorkspaceOptions, UpOptions};
//!
//! # async fn demo() -> pulumi::auto::Result<()> {
//! let program = auto::program(|ctx| async move {
//!     ctx.export("greeting", pulumi::pv::string("hello"));
//!     Ok(())
//! });
//! let stack = Stack::create_or_select_inline_source(
//!     "dev",
//!     "my-project",
//!     program,
//!     LocalWorkspaceOptions::default(),
//! )
//! .await?;
//! let up = stack.up(UpOptions::default()).await?;
//! println!("greeting: {:?}", up.outputs["greeting"].value);
//! # Ok(())
//! # }
//! ```
//!
//! Operations stream the engine's structured events (see [`events`]) and
//! return typed results; failures classify themselves the way the Go
//! Automation API's error predicates do (see [`Error`]).
//!
//! Inline programs in one process are serialized: concurrent stack
//! operations whose programs are closures queue and run one at a time
//! (the SDK keeps one active program context per process). Operations on
//! local programs run concurrently without restriction. Do not start a
//! stack operation from inside an inline program.
//!
//! Not yet ported from Go: remote workspaces and git sources, the
//! `pulumi import`/`stack rename` operations, preview-only refresh and
//! destroy variants, and a handful of smaller options —
//! `docs/known-limitations.md` keeps the complete list.

pub mod cmd;
pub mod errors;
pub mod events;
mod server;
mod stack;
mod workspace;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub use cmd::{CommandSpec, LocalPulumiCommand, PulumiCommand, PulumiCommandOptions};
pub use errors::{CommandResult, Error, Result};
pub use events::EngineEvent;
pub use stack::{
    fully_qualified_stack_name, DebugLoggingOptions, DestroyOptions, DestroyResult, PreviewOptions,
    PreviewResult, RefreshOptions, RefreshResult, Stack, UpOptions, UpResult, UpdateSummary,
};
pub use workspace::{
    ConfigMap, ConfigOptions, ConfigValue, InstallOptions, LocalWorkspace, LocalWorkspaceOptions,
    OutputMap, OutputValue, PluginInfo, ProjectRuntimeInfo, ProjectSettings, StackDeployment,
    StackSettings, StackSettingsConfigValue, StackSummary, WhoAmIResult,
};

/// An inline Pulumi program: a closure the engine runs in-process during
/// stack operations. Build one with [`program`].
pub type ProgramFn = Arc<
    dyn Fn(crate::Context) -> futures::future::BoxFuture<'static, crate::error::Result<()>>
        + Send
        + Sync,
>;

/// Wrap an async closure as an inline program.
///
/// The closure runs once per stack operation, receiving a fresh connected
/// [`Context`](crate::Context) each time.
pub fn program<F, Fut>(f: F) -> ProgramFn
where
    F: Fn(crate::Context) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = crate::error::Result<()>> + Send + 'static,
{
    Arc::new(move |ctx| Box::pin(f(ctx)))
}

/// A fresh directory under the system temp dir. The name carries the pid
/// and a counter so concurrent workspaces never collide.
pub(crate) fn scratch_dir(prefix: &str) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::setup(format!("failed to create scratch dir: {e}")))?;
    Ok(dir)
}
