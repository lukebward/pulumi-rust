//! [`Stack`]: one updatable unit of a workspace, and the deployment
//! operations — `up`, `preview`, `refresh`, `destroy` — that act on it.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use super::cmd::CommandSpec;
use super::errors::{CommandResult, Error, Result};
use super::events::{EngineEvent, EventLogWatcher, OpType, SummaryEvent};
use super::server::LanguageServer;
use super::workspace::{svec, ConfigMap, ConfigOptions, ConfigValue, LocalWorkspace, OutputMap};
use super::ProgramFn;

/// A stack bound to a [`LocalWorkspace`]. The Rust analogue of the Go
/// SDK's `auto.Stack`.
#[derive(Debug, Clone)]
pub struct Stack {
    name: String,
    workspace: LocalWorkspace,
}

/// Build a fully qualified `org/project/stack` name.
pub fn fully_qualified_stack_name(org: &str, project: &str, stack: &str) -> String {
    format!("{org}/{project}/{stack}")
}

impl Stack {
    /// Create a new stack in the workspace.
    pub async fn create(stack_name: impl Into<String>, workspace: LocalWorkspace) -> Result<Self> {
        let name = stack_name.into();
        workspace.create_stack(&name).await?;
        Ok(Stack { name, workspace })
    }

    /// Select an existing stack in the workspace.
    pub async fn select(stack_name: impl Into<String>, workspace: LocalWorkspace) -> Result<Self> {
        let name = stack_name.into();
        workspace.select_stack(&name).await?;
        Ok(Stack { name, workspace })
    }

    /// Select the stack, creating it if it does not exist yet.
    pub async fn create_or_select(
        stack_name: impl Into<String>,
        workspace: LocalWorkspace,
    ) -> Result<Self> {
        let name = stack_name.into();
        match workspace.select_stack(&name).await {
            Ok(()) => Ok(Stack { name, workspace }),
            Err(e) if e.is_select_stack_404_error() => {
                workspace.create_stack(&name).await?;
                Ok(Stack { name, workspace })
            }
            Err(e) => Err(e),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn workspace(&self) -> &LocalWorkspace {
        &self.workspace
    }

    pub fn workspace_mut(&mut self) -> &mut LocalWorkspace {
        &mut self.workspace
    }

    // ---- deployment operations ----

    /// Create or update the stack's resources (`pulumi up`).
    pub async fn up(&self, options: UpOptions) -> Result<UpResult> {
        let mut args = svec(["up", "--yes", "--skip-preview"]);
        args.extend(options.debug.args());

        let server = self.start_language_server_unchecked().await?;
        push_exec_kind(&mut args, &server);

        let watcher = if options.event_senders.is_empty() {
            None
        } else {
            Some(EventLogWatcher::start("up", options.event_senders.clone())?)
        };
        if let Some(watcher) = &watcher {
            args.push("--event-log".to_string());
            args.push(watcher.path().display().to_string());
        }

        push_shared_options(
            &mut args,
            SharedOptions {
                message: &options.message,
                expect_no_changes: options.expect_no_changes,
                diff: options.diff,
                replace: &options.replace,
                target: &options.target,
                exclude: &options.exclude,
                policy_packs: &options.policy_packs,
                policy_pack_configs: &options.policy_pack_configs,
                target_dependents: options.target_dependents,
                exclude_dependents: options.exclude_dependents,
                parallel: options.parallel,
                color: &options.color,
                plan: options
                    .plan
                    .as_ref()
                    .map(|p| format!("--plan={}", p.display())),
                refresh: options.refresh,
                suppress_outputs: options.suppress_outputs,
                suppress_progress: options.suppress_progress,
                continue_on_error: options.continue_on_error,
                config_file: &options.config_file,
                run_program: options.run_program,
            },
        );

        let run = self.run_stack_cmd(args).await;
        if let Some(watcher) = watcher {
            watcher.close().await;
        }
        if let Some(server) = server {
            server.close().await;
        }
        let result = run.map_err(|e| e.with_context("failed to run update"))?;

        let outputs = self.outputs().await?;
        let summary = self
            .history(Some(1), 1, options.show_secrets)
            .await?
            .into_iter()
            .next();
        Ok(UpResult {
            stdout: result.stdout,
            stderr: result.stderr,
            outputs,
            summary,
        })
    }

    /// Preview the changes an `up` would perform (`pulumi preview`).
    pub async fn preview(&self, options: PreviewOptions) -> Result<PreviewResult> {
        let mut shared = options.debug.args();
        push_shared_options(
            &mut shared,
            SharedOptions {
                message: &options.message,
                expect_no_changes: options.expect_no_changes,
                diff: options.diff,
                replace: &options.replace,
                target: &options.target,
                exclude: &options.exclude,
                policy_packs: &options.policy_packs,
                policy_pack_configs: &options.policy_pack_configs,
                target_dependents: options.target_dependents,
                exclude_dependents: options.exclude_dependents,
                parallel: options.parallel,
                color: &options.color,
                plan: options
                    .save_plan
                    .as_ref()
                    .map(|p| format!("--save-plan={}", p.display())),
                refresh: options.refresh,
                suppress_outputs: options.suppress_outputs,
                suppress_progress: options.suppress_progress,
                continue_on_error: false,
                config_file: &options.config_file,
                run_program: options.run_program,
            },
        );

        let mut args = svec(["preview"]);
        let server = self.start_language_server_unchecked().await?;
        push_exec_kind(&mut args, &server);
        args.extend(shared);

        // The preview's change summary only exists as an engine event, so
        // the event log is always tailed, with an internal subscription
        // ahead of the caller's. The subscription is drained as events
        // arrive — a large preview must not accumulate in memory.
        let (summary_tx, mut summary_rx) = tokio::sync::mpsc::unbounded_channel::<EngineEvent>();
        let collector = tokio::spawn(async move {
            let mut summaries: Vec<SummaryEvent> = vec![];
            while let Some(event) = summary_rx.recv().await {
                if let Some(summary) = event.summary_event {
                    summaries.push(summary);
                }
            }
            summaries
        });
        let mut senders = vec![summary_tx];
        senders.extend(options.event_senders.clone());
        let watcher = EventLogWatcher::start("preview", senders)?;
        args.push("--event-log".to_string());
        args.push(watcher.path().display().to_string());

        let run = self.run_stack_cmd(args).await;
        watcher.close().await;
        if let Some(server) = server {
            server.close().await;
        }
        let summaries = collector.await.unwrap_or_default();
        let result = run.map_err(|e| e.with_context("failed to run preview"))?;

        let summary = single_summary(summaries, &result, "preview")?;
        Ok(PreviewResult {
            stdout: result.stdout,
            stderr: result.stderr,
            change_summary: summary.resource_changes.into_iter().collect(),
        })
    }

    /// Compare the stack's state against the real resources and update the
    /// state to match (`pulumi refresh`).
    pub async fn refresh(&self, options: RefreshOptions) -> Result<RefreshResult> {
        let mut args = svec(["refresh"]);
        args.extend(options.debug.args());
        args.extend(svec(["--yes", "--skip-preview"]));
        push_shared_options(
            &mut args,
            SharedOptions {
                message: &options.message,
                expect_no_changes: options.expect_no_changes,
                diff: false,
                replace: &[],
                target: &options.target,
                exclude: &options.exclude,
                policy_packs: &[],
                policy_pack_configs: &[],
                target_dependents: options.target_dependents,
                exclude_dependents: options.exclude_dependents,
                parallel: options.parallel,
                color: &options.color,
                plan: None,
                refresh: false,
                suppress_outputs: options.suppress_outputs,
                suppress_progress: options.suppress_progress,
                continue_on_error: false,
                config_file: &options.config_file,
                run_program: options.run_program,
            },
        );
        if options.diff {
            args.push("--diff".to_string());
        }

        let server = self.start_language_server(&["refresh"]).await?;
        push_exec_kind(&mut args, &server);

        let watcher = if options.event_senders.is_empty() {
            None
        } else {
            Some(EventLogWatcher::start(
                "refresh",
                options.event_senders.clone(),
            )?)
        };
        if let Some(watcher) = &watcher {
            args.push("--event-log".to_string());
            args.push(watcher.path().display().to_string());
        }

        let run = self.run_stack_cmd(args).await;
        if let Some(watcher) = watcher {
            watcher.close().await;
        }
        if let Some(server) = server {
            server.close().await;
        }
        let result = run.map_err(|e| e.with_context("failed to refresh stack"))?;

        let summary = self
            .history(Some(1), 1, options.show_secrets)
            .await?
            .into_iter()
            .next();
        Ok(RefreshResult {
            stdout: result.stdout,
            stderr: result.stderr,
            summary,
        })
    }

    /// Delete every resource in the stack (`pulumi destroy`).
    pub async fn destroy(&self, options: DestroyOptions) -> Result<DestroyResult> {
        let mut args = svec(["destroy"]);
        args.extend(options.debug.args());
        push_shared_options(
            &mut args,
            SharedOptions {
                message: &options.message,
                expect_no_changes: false,
                diff: false,
                replace: &[],
                target: &options.target,
                exclude: &options.exclude,
                policy_packs: &[],
                policy_pack_configs: &[],
                target_dependents: options.target_dependents,
                exclude_dependents: options.exclude_dependents,
                parallel: options.parallel,
                color: &options.color,
                plan: None,
                refresh: options.refresh,
                suppress_outputs: options.suppress_outputs,
                suppress_progress: options.suppress_progress,
                continue_on_error: options.continue_on_error,
                config_file: &options.config_file,
                run_program: options.run_program,
            },
        );
        if options.diff {
            args.push("--diff".to_string());
        }

        let server = self.start_language_server(&["destroy"]).await?;
        push_exec_kind(&mut args, &server);
        args.extend(svec(["--yes", "--skip-preview"]));

        let watcher = if options.event_senders.is_empty() {
            None
        } else {
            Some(EventLogWatcher::start(
                "destroy",
                options.event_senders.clone(),
            )?)
        };
        if let Some(watcher) = &watcher {
            args.push("--event-log".to_string());
            args.push(watcher.path().display().to_string());
        }

        let run = self.run_stack_cmd(args).await;
        if let Some(watcher) = watcher {
            watcher.close().await;
        }
        if let Some(server) = server {
            server.close().await;
        }
        let result = run.map_err(|e| e.with_context("failed to destroy stack"))?;

        let summary = self
            .history(Some(1), 1, options.show_secrets)
            .await?
            .into_iter()
            .next();
        if options.remove {
            self.workspace
                .remove_stack(&self.name, false)
                .await
                .map_err(|e| e.with_context("failed to remove stack"))?;
        }
        Ok(DestroyResult {
            stdout: result.stdout,
            stderr: result.stderr,
            summary,
        })
    }

    // ---- state and information ----

    /// The stack's outputs.
    pub async fn outputs(&self) -> Result<OutputMap> {
        self.workspace.stack_outputs(&self.name).await
    }

    /// `pulumi stack history --json`.
    pub async fn history(
        &self,
        page_size: Option<u32>,
        page: u32,
        show_secrets: Option<bool>,
    ) -> Result<Vec<UpdateSummary>> {
        let mut args = svec(["stack", "history", "--json"]);
        // Secrets are shown unless explicitly declined, as in Go.
        if show_secrets.unwrap_or(true) {
            args.push("--show-secrets".to_string());
        }
        if let Some(size) = page_size {
            args.push("--page-size".to_string());
            args.push(size.to_string());
            args.push("--page".to_string());
            args.push(page.max(1).to_string());
        }
        let result = self
            .run_stack_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to get stack history"))?;
        // A stack that has never updated reports `null`, not `[]`.
        let history: Option<Vec<UpdateSummary>> = serde_json::from_str(&result.stdout)
            .map_err(|e| Error::setup(format!("unable to unmarshal history result: {e}")))?;
        Ok(history.unwrap_or_default())
    }

    /// Cancel the stack's currently running update, if any. Leaves the
    /// stack in an inconsistent state; use only when an update is stuck.
    pub async fn cancel(&self) -> Result<()> {
        self.run_stack_cmd(svec(["cancel", "--yes"]))
            .await
            .map_err(|e| e.with_context("failed to cancel update"))?;
        Ok(())
    }

    /// Export the stack's deployment state.
    pub async fn export(&self) -> Result<super::workspace::StackDeployment> {
        self.workspace.export_stack(&self.name).await
    }

    /// Import previously exported deployment state.
    pub async fn import(&self, state: &super::workspace::StackDeployment) -> Result<()> {
        self.workspace.import_stack(&self.name, state).await
    }

    // ---- config conveniences, delegating to the workspace ----

    pub async fn get_config(&self, key: &str) -> Result<ConfigValue> {
        self.workspace
            .get_config(&self.name, key, &ConfigOptions::default())
            .await
    }

    pub async fn get_all_config(&self) -> Result<ConfigMap> {
        self.workspace.get_all_config(&self.name).await
    }

    pub async fn set_config(&self, key: &str, value: &ConfigValue) -> Result<()> {
        self.workspace
            .set_config(&self.name, key, value, &ConfigOptions::default())
            .await
    }

    pub async fn set_all_config(&self, config: &ConfigMap) -> Result<()> {
        self.workspace
            .set_all_config(&self.name, config, &ConfigOptions::default())
            .await
    }

    pub async fn remove_config(&self, key: &str) -> Result<()> {
        self.workspace
            .remove_config(&self.name, key, &ConfigOptions::default())
            .await
    }

    pub async fn refresh_config(&self) -> Result<ConfigMap> {
        self.workspace.refresh_config(&self.name).await
    }

    // ---- plumbing ----

    /// Start the in-process language server when the workspace holds an
    /// inline program; no version requirement (up and preview accept
    /// `--client` on every CLI this SDK supports).
    async fn start_language_server_unchecked(&self) -> Result<Option<LanguageServer>> {
        match self.workspace.program() {
            Some(program) => Ok(Some(LanguageServer::start(program.clone()).await?)),
            None => Ok(None),
        }
    }

    /// As above, for the operations whose `--client` support arrived in
    /// CLI 3.181.0.
    async fn start_language_server(&self, gated_op: &[&str]) -> Result<Option<LanguageServer>> {
        if self.workspace.program().is_some()
            && self.workspace.pulumi_version() < semver::Version::new(3, 181, 0)
        {
            return Err(Error::setup(format!(
                "Pulumi CLI version >= 3.181.0 is required to use --client with {}",
                gated_op.join(" ")
            )));
        }
        self.start_language_server_unchecked().await
    }

    /// Run a stack-scoped CLI command: `--stack <name>` is appended before
    /// any `--` positional section, and the engine's debug variables are
    /// set the way every automation SDK sets them.
    pub(crate) async fn run_stack_cmd(&self, mut args: Vec<String>) -> Result<CommandResult> {
        let tail = match args.iter().position(|a| a == "--") {
            Some(at) => args.split_off(at),
            None => vec![],
        };
        args.push("--stack".to_string());
        args.push(self.name.clone());
        args.extend(tail);

        let mut env = vec![("PULUMI_DEBUG_COMMANDS".to_string(), "true".to_string())];
        env.extend(self.workspace.base_env());
        self.workspace
            .command()
            .run(CommandSpec {
                args,
                workdir: self.workspace.work_dir().to_path_buf(),
                env,
                stdin: None,
            })
            .await
    }
}

fn push_exec_kind(args: &mut Vec<String>, server: &Option<LanguageServer>) {
    match server {
        Some(server) => {
            args.push(format!("--client={}", server.address()));
            args.push("--exec-kind=auto.inline".to_string());
        }
        None => args.push("--exec-kind=auto.local".to_string()),
    }
}

/// The option fields `up`, `preview`, `refresh` and `destroy` share, in
/// the flag order the Go SDK emits them.
struct SharedOptions<'a> {
    message: &'a Option<String>,
    expect_no_changes: bool,
    diff: bool,
    replace: &'a [String],
    target: &'a [String],
    exclude: &'a [String],
    policy_packs: &'a [String],
    policy_pack_configs: &'a [String],
    target_dependents: bool,
    exclude_dependents: bool,
    parallel: i32,
    color: &'a Option<String>,
    /// Pre-formatted: `--plan=` for up, `--save-plan=` for preview.
    plan: Option<String>,
    refresh: bool,
    suppress_outputs: bool,
    suppress_progress: bool,
    continue_on_error: bool,
    config_file: &'a Option<PathBuf>,
    run_program: Option<bool>,
}

fn push_shared_options(args: &mut Vec<String>, options: SharedOptions<'_>) {
    if let Some(message) = options.message {
        args.push(format!("--message={message}"));
    }
    if options.expect_no_changes {
        args.push("--expect-no-changes".to_string());
    }
    if options.diff {
        args.push("--diff".to_string());
    }
    for urn in options.replace {
        args.push(format!("--replace={urn}"));
    }
    for urn in options.target {
        args.push(format!("--target={urn}"));
    }
    for urn in options.exclude {
        args.push(format!("--exclude={urn}"));
    }
    for pack in options.policy_packs {
        args.push(format!("--policy-pack={pack}"));
    }
    for config in options.policy_pack_configs {
        args.push(format!("--policy-pack-config={config}"));
    }
    if options.target_dependents {
        args.push("--target-dependents".to_string());
    }
    if options.exclude_dependents {
        args.push("--exclude-dependents".to_string());
    }
    if options.parallel > 0 {
        args.push(format!("--parallel={}", options.parallel));
    }
    if let Some(color) = options.color {
        args.push(format!("--color={color}"));
    }
    if let Some(plan) = options.plan {
        args.push(plan);
    }
    if options.refresh {
        args.push("--refresh".to_string());
    }
    if options.suppress_outputs {
        args.push("--suppress-outputs".to_string());
    }
    if options.suppress_progress {
        args.push("--suppress-progress".to_string());
    }
    if options.continue_on_error {
        args.push("--continue-on-error".to_string());
    }
    if let Some(file) = options.config_file {
        args.push(format!("--config-file={}", file.display()));
    }
    if let Some(run_program) = options.run_program {
        args.push(format!("--run-program={run_program}"));
    }
}

/// Options for [`Stack::up`].
#[derive(Clone, Default)]
pub struct UpOptions {
    pub message: Option<String>,
    /// Fail if the update would change anything.
    pub expect_no_changes: bool,
    pub diff: bool,
    /// URNs to replace.
    pub replace: Vec<String>,
    /// URNs to restrict the operation to.
    pub target: Vec<String>,
    /// URNs to leave out of the operation.
    pub exclude: Vec<String>,
    pub policy_packs: Vec<String>,
    pub policy_pack_configs: Vec<String>,
    pub target_dependents: bool,
    pub exclude_dependents: bool,
    /// Maximum concurrent resource operations; unlimited when zero.
    pub parallel: i32,
    pub color: Option<String>,
    /// Apply a plan `preview` saved earlier.
    pub plan: Option<PathBuf>,
    /// Refresh state before updating.
    pub refresh: bool,
    pub suppress_outputs: bool,
    pub suppress_progress: bool,
    pub continue_on_error: bool,
    pub config_file: Option<PathBuf>,
    pub run_program: Option<bool>,
    /// Whether the returned summary decrypts secrets; defaults to yes.
    pub show_secrets: Option<bool>,
    /// Live engine events are cloned into each sender; the channels close
    /// when the operation's event stream ends.
    pub event_senders: Vec<UnboundedSender<EngineEvent>>,
    pub debug: DebugLoggingOptions,
}

/// Options for [`Stack::preview`].
#[derive(Clone, Default)]
pub struct PreviewOptions {
    pub message: Option<String>,
    pub expect_no_changes: bool,
    pub diff: bool,
    pub replace: Vec<String>,
    pub target: Vec<String>,
    pub exclude: Vec<String>,
    pub policy_packs: Vec<String>,
    pub policy_pack_configs: Vec<String>,
    pub target_dependents: bool,
    pub exclude_dependents: bool,
    pub parallel: i32,
    pub color: Option<String>,
    /// Save the computed plan for a later `up` to apply.
    pub save_plan: Option<PathBuf>,
    pub refresh: bool,
    pub suppress_outputs: bool,
    pub suppress_progress: bool,
    pub config_file: Option<PathBuf>,
    pub run_program: Option<bool>,
    pub event_senders: Vec<UnboundedSender<EngineEvent>>,
    pub debug: DebugLoggingOptions,
}

/// Options for [`Stack::refresh`].
#[derive(Clone, Default)]
pub struct RefreshOptions {
    pub message: Option<String>,
    pub expect_no_changes: bool,
    pub target: Vec<String>,
    pub exclude: Vec<String>,
    pub target_dependents: bool,
    pub exclude_dependents: bool,
    pub parallel: i32,
    pub color: Option<String>,
    pub suppress_outputs: bool,
    pub suppress_progress: bool,
    pub config_file: Option<PathBuf>,
    pub diff: bool,
    pub run_program: Option<bool>,
    pub show_secrets: Option<bool>,
    pub event_senders: Vec<UnboundedSender<EngineEvent>>,
    pub debug: DebugLoggingOptions,
}

/// Options for [`Stack::destroy`].
#[derive(Clone, Default)]
pub struct DestroyOptions {
    pub message: Option<String>,
    pub target: Vec<String>,
    pub exclude: Vec<String>,
    pub target_dependents: bool,
    pub exclude_dependents: bool,
    pub parallel: i32,
    pub color: Option<String>,
    /// Refresh state before destroying.
    pub refresh: bool,
    pub suppress_outputs: bool,
    pub suppress_progress: bool,
    pub continue_on_error: bool,
    pub config_file: Option<PathBuf>,
    pub run_program: Option<bool>,
    pub diff: bool,
    /// Remove the stack itself after destroying its resources.
    pub remove: bool,
    pub show_secrets: Option<bool>,
    pub event_senders: Vec<UnboundedSender<EngineEvent>>,
    pub debug: DebugLoggingOptions,
}

/// Engine debug-logging flags, shared by every operation.
#[derive(Debug, Clone, Default)]
pub struct DebugLoggingOptions {
    /// Verbosity for `-v`; a zero is treated as 1.
    pub log_level: Option<u32>,
    pub log_to_std_err: bool,
    pub flow_to_plugins: bool,
    pub tracing: Option<String>,
    pub debug: bool,
}

impl DebugLoggingOptions {
    fn args(&self) -> Vec<String> {
        let mut args = vec![];
        if self.log_to_std_err {
            args.push("--logtostderr".to_string());
        }
        if let Some(level) = self.log_level {
            args.push(format!("-v={}", level.max(1)));
        }
        if self.flow_to_plugins {
            args.push("--logflow".to_string());
        }
        if let Some(tracing) = &self.tracing {
            args.push(format!("--tracing={tracing}"));
        }
        if self.debug {
            args.push("--debug".to_string());
        }
        args
    }
}

/// One entry of `pulumi stack history --json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSummary {
    #[serde(default)]
    pub version: i64,
    /// The operation kind: `update`, `refresh`, `destroy`, ...
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub start_time: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub environment: HashMap<String, String>,
    #[serde(default)]
    pub config: ConfigMap,
    /// `succeeded`, `failed` or `in-progress`; absent while running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_changes: Option<HashMap<String, i64>>,
}

/// The result of [`Stack::up`].
#[derive(Debug, Clone, Default)]
pub struct UpResult {
    pub stdout: String,
    pub stderr: String,
    pub outputs: OutputMap,
    pub summary: Option<UpdateSummary>,
}

/// The result of [`Stack::preview`].
#[derive(Debug, Clone, Default)]
pub struct PreviewResult {
    pub stdout: String,
    pub stderr: String,
    /// Planned operations by kind, e.g. `create` → 3.
    pub change_summary: HashMap<OpType, i64>,
}

/// The result of [`Stack::refresh`].
#[derive(Debug, Clone, Default)]
pub struct RefreshResult {
    pub stdout: String,
    pub stderr: String,
    pub summary: Option<UpdateSummary>,
}

/// The result of [`Stack::destroy`].
#[derive(Debug, Clone, Default)]
pub struct DestroyResult {
    pub stdout: String,
    pub stderr: String,
    pub summary: Option<UpdateSummary>,
}

impl UpResult {
    /// The update's permalink on the backend, when the output carried one.
    pub fn permalink(&self) -> Result<String> {
        permalink_from(&self.stdout)
    }
}

impl PreviewResult {
    pub fn permalink(&self) -> Result<String> {
        permalink_from(&self.stdout)
    }
}

impl RefreshResult {
    pub fn permalink(&self) -> Result<String> {
        permalink_from(&self.stdout)
    }
}

impl DestroyResult {
    pub fn permalink(&self) -> Result<String> {
        permalink_from(&self.stdout)
    }
}

/// The labels a permalink follows in CLI output.
const PERMALINK_PREFIXES: [&str; 4] = [
    "View Live: ",
    "View in Browser: ",
    "View in Browser (Ctrl+O): ",
    "Permalink: ",
];

fn permalink_from(stdout: &str) -> Result<String> {
    let mut earliest: Option<usize> = None;
    for prefix in PERMALINK_PREFIXES {
        if let Some(at) = stdout.find(prefix) {
            let start = at + prefix.len();
            earliest = Some(match earliest {
                Some(existing) if existing <= start => existing,
                _ => start,
            });
        }
    }
    let start = earliest.ok_or_else(|| Error::setup("failed to get permalink"))?;
    let rest = &stdout[start..];
    let end = rest
        .find('\n')
        .ok_or_else(|| Error::setup("failed to get permalink"))?;
    Ok(rest[..end].trim_end_matches('\r').to_string())
}

/// Pull exactly one summary event out of the collected set; zero or
/// several is an error, reported with the operation's streams attached.
fn single_summary(
    mut summaries: Vec<SummaryEvent>,
    result: &CommandResult,
    op: &str,
) -> Result<SummaryEvent> {
    match summaries.len() {
        1 => Ok(summaries.remove(0)),
        0 => Err(Error::command(
            format!("failed to get {op} summary"),
            result.clone(),
        )),
        _ => Err(Error::command(
            format!("got multiple {op} summaries"),
            result.clone(),
        )),
    }
}

// ---- convenience constructors, mirroring the Go stack-source helpers ----

impl Stack {
    /// Create a stack for a Pulumi program on disk: the directory holds
    /// `Pulumi.yaml` and the program the CLI should run.
    pub async fn create_local_source(
        stack_name: impl Into<String>,
        work_dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        let workspace = LocalWorkspace::new(super::workspace::LocalWorkspaceOptions {
            work_dir: Some(work_dir.into()),
            ..Default::default()
        })
        .await?;
        Stack::create(stack_name, workspace).await
    }

    /// Select an existing stack for a Pulumi program on disk.
    pub async fn select_local_source(
        stack_name: impl Into<String>,
        work_dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        let workspace = LocalWorkspace::new(super::workspace::LocalWorkspaceOptions {
            work_dir: Some(work_dir.into()),
            ..Default::default()
        })
        .await?;
        Stack::select(stack_name, workspace).await
    }

    /// [`Stack::create_local_source`], selecting the stack if it already
    /// exists.
    pub async fn create_or_select_local_source(
        stack_name: impl Into<String>,
        work_dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        let workspace = LocalWorkspace::new(super::workspace::LocalWorkspaceOptions {
            work_dir: Some(work_dir.into()),
            ..Default::default()
        })
        .await?;
        Stack::create_or_select(stack_name, workspace).await
    }

    /// Create a stack whose program is a Rust closure running in this
    /// process. The workspace lands in a scratch directory with generated
    /// project settings, unless `options` says otherwise.
    pub async fn create_inline_source(
        stack_name: impl Into<String>,
        project_name: &str,
        program: ProgramFn,
        options: super::workspace::LocalWorkspaceOptions,
    ) -> Result<Self> {
        let workspace = Self::inline_workspace(project_name, program, options).await?;
        Stack::create(stack_name, workspace).await
    }

    /// Select an existing stack, with an inline program for its operations.
    pub async fn select_inline_source(
        stack_name: impl Into<String>,
        project_name: &str,
        program: ProgramFn,
        options: super::workspace::LocalWorkspaceOptions,
    ) -> Result<Self> {
        let workspace = Self::inline_workspace(project_name, program, options).await?;
        Stack::select(stack_name, workspace).await
    }

    /// [`Stack::create_inline_source`], selecting the stack if it already
    /// exists.
    pub async fn create_or_select_inline_source(
        stack_name: impl Into<String>,
        project_name: &str,
        program: ProgramFn,
        options: super::workspace::LocalWorkspaceOptions,
    ) -> Result<Self> {
        let workspace = Self::inline_workspace(project_name, program, options).await?;
        Stack::create_or_select(stack_name, workspace).await
    }

    async fn inline_workspace(
        project_name: &str,
        program: ProgramFn,
        mut options: super::workspace::LocalWorkspaceOptions,
    ) -> Result<LocalWorkspace> {
        options.program = Some(program);
        let workspace = LocalWorkspace::new(options).await?;
        // Give the workspace project settings unless it already has some —
        // a work_dir pointing at an existing project keeps its own. A file
        // that exists but does not load is an error, never something to
        // overwrite.
        if workspace.has_project_settings() {
            workspace
                .project_settings()
                .map_err(|e| e.with_context("found project settings, but failed to load"))?;
        } else {
            let mut settings = super::workspace::ProjectSettings::new(project_name, "rust");
            settings.main = std::env::current_dir()
                .ok()
                .map(|d| d.display().to_string());
            workspace.save_project_settings(&settings)?;
        }
        Ok(workspace)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::cmd::test_support::RecordingCommand;
    use super::super::workspace::LocalWorkspaceOptions;
    use super::*;

    async fn recording_stack() -> (Arc<RecordingCommand>, Stack) {
        let recorder = Arc::new(RecordingCommand::default());
        let ws = LocalWorkspace::new(LocalWorkspaceOptions {
            pulumi_command: Some(recorder.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
        (
            recorder.clone(),
            Stack {
                name: "dev".to_string(),
                workspace: ws,
            },
        )
    }

    fn ok_json(json: &str) -> super::super::errors::Result<CommandResult> {
        Ok(CommandResult {
            stdout: json.to_string(),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn up_assembles_expected_args() {
        let (recorder, stack) = recording_stack().await;
        recorder.push_result(Ok(CommandResult::default())); // up
        recorder.push_result(ok_json("{}")); // outputs masked
        recorder.push_result(ok_json("{}")); // outputs shown
        recorder.push_result(ok_json("[]")); // history

        stack
            .up(UpOptions {
                message: Some("hello".to_string()),
                target: vec!["urn:a".to_string(), "urn:b".to_string()],
                parallel: 8,
                refresh: true,
                ..Default::default()
            })
            .await
            .unwrap();

        let args = recorder.recorded_args();
        assert_eq!(
            args[0],
            svec([
                "up",
                "--yes",
                "--skip-preview",
                "--exec-kind=auto.local",
                "--message=hello",
                "--target=urn:a",
                "--target=urn:b",
                "--parallel=8",
                "--refresh",
                "--stack",
                "dev"
            ])
        );
        // The follow-ups: two outputs runs, then history page 1.
        assert_eq!(
            args[3],
            svec([
                "stack",
                "history",
                "--json",
                "--show-secrets",
                "--page-size",
                "1",
                "--page",
                "1",
                "--stack",
                "dev"
            ])
        );
    }

    #[tokio::test]
    async fn up_omits_show_secrets_when_declined() {
        let (recorder, stack) = recording_stack().await;
        recorder.push_result(Ok(CommandResult::default()));
        recorder.push_result(ok_json("{}"));
        recorder.push_result(ok_json("{}"));
        recorder.push_result(ok_json("[]"));
        stack
            .up(UpOptions {
                show_secrets: Some(false),
                ..Default::default()
            })
            .await
            .unwrap();
        let history = &recorder.recorded_args()[3];
        assert!(!history.contains(&"--show-secrets".to_string()));
    }

    #[tokio::test]
    async fn preview_tails_events_and_reports_missing_summary() {
        let (recorder, stack) = recording_stack().await;
        // The preview command "succeeds" but writes no events, so the
        // summary is missing and the op must fail like Go's does.
        let err = stack.preview(PreviewOptions::default()).await.unwrap_err();
        assert!(err.to_string().contains("failed to get preview summary"));

        let args = &recorder.recorded_args()[0];
        assert_eq!(args[0], "preview");
        assert_eq!(args[1], "--exec-kind=auto.local");
        let at = args.iter().position(|a| a == "--event-log").unwrap();
        assert!(args[at + 1].ends_with("eventlog.txt"));
        assert_eq!(&args[args.len() - 2..], &["--stack", "dev"]);
    }

    #[tokio::test]
    async fn destroy_orders_yes_after_exec_kind_and_removes_stack() {
        let (recorder, stack) = recording_stack().await;
        recorder.push_result(Ok(CommandResult::default())); // destroy
        recorder.push_result(ok_json("[]")); // history
        recorder.push_result(Ok(CommandResult::default())); // stack rm

        stack
            .destroy(DestroyOptions {
                remove: true,
                ..Default::default()
            })
            .await
            .unwrap();

        let args = recorder.recorded_args();
        assert_eq!(
            args[0],
            svec([
                "destroy",
                "--exec-kind=auto.local",
                "--yes",
                "--skip-preview",
                "--stack",
                "dev"
            ])
        );
        assert_eq!(args[2], svec(["stack", "rm", "--yes", "dev"]));
    }

    #[tokio::test]
    async fn refresh_places_yes_before_options() {
        let (recorder, stack) = recording_stack().await;
        recorder.push_result(Ok(CommandResult::default()));
        recorder.push_result(ok_json("[]"));
        stack
            .refresh(RefreshOptions {
                expect_no_changes: true,
                diff: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec([
                "refresh",
                "--yes",
                "--skip-preview",
                "--expect-no-changes",
                "--diff",
                "--exec-kind=auto.local",
                "--stack",
                "dev"
            ])
        );
    }

    #[tokio::test]
    async fn debug_logging_flags_come_first_in_order() {
        let opts = DebugLoggingOptions {
            log_level: Some(0),
            log_to_std_err: true,
            flow_to_plugins: true,
            tracing: Some("file:./trace".to_string()),
            debug: true,
        };
        assert_eq!(
            opts.args(),
            svec([
                "--logtostderr",
                "-v=1",
                "--logflow",
                "--tracing=file:./trace",
                "--debug"
            ])
        );
    }

    #[test]
    fn permalink_parses_cli_output() {
        let out = "Updating (dev)\n\nView in Browser (Ctrl+O): https://app.pulumi.com/org/proj/dev/updates/3\n\nResources:\n";
        assert_eq!(
            permalink_from(out).unwrap(),
            "https://app.pulumi.com/org/proj/dev/updates/3"
        );
        assert!(permalink_from("no link here\n").is_err());
        // A permalink must be newline-terminated.
        assert!(permalink_from("Permalink: https://x").is_err());
    }

    #[test]
    fn update_summary_parses_history_json() {
        let json = r#"[{"version":3,"kind":"update","startTime":"2026-08-18T00:00:00.000Z","message":"","environment":{"exec.kind":"auto.local"},"config":{"proj:key":{"value":"v","secret":false,"object":false}},"result":"succeeded","endTime":"2026-08-18T00:00:10.000Z","resourceChanges":{"create":1}}]"#;
        let history: Vec<UpdateSummary> = serde_json::from_str(json).unwrap();
        assert_eq!(history[0].version, 3);
        assert_eq!(history[0].kind, "update");
        assert_eq!(history[0].config["proj:key"].value, "v");
        assert_eq!(history[0].resource_changes.as_ref().unwrap()["create"], 1);
    }

    #[tokio::test]
    async fn stack_cmd_inserts_stack_before_positional_tail() {
        let (recorder, stack) = recording_stack().await;
        stack
            .run_stack_cmd(svec(["config", "set", "k", "--", "--value"]))
            .await
            .unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec(["config", "set", "k", "--stack", "dev", "--", "--value"])
        );
        let spec = &recorder.specs.lock().unwrap()[0];
        assert!(spec
            .env
            .iter()
            .any(|(k, v)| k == "PULUMI_DEBUG_COMMANDS" && v == "true"));
    }
}
