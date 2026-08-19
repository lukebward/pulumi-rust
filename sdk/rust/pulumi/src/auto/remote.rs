//! Remote workspaces: stacks whose deployment operations run in Pulumi
//! Deployments rather than on this machine, mirroring the Go SDK's
//! `RemoteWorkspace`/`RemoteStack` surface.
//!
//! A [`RemoteStack`] is built from a git source ([`RemoteGitRepo`]) and
//! [`RemoteWorkspaceOptions`]; `up`, `preview`, `refresh` and `destroy`
//! then serialize the source and options as the CLI's `--remote*` flags,
//! exactly as Go's `remoteArgs` does, and the service executes the
//! operation. Nothing is cloned locally.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use semver::Version;

use super::cmd::{skip_version_check_from_env, CommandSpec};
use super::errors::{Error, Result};
use super::git::GitAuth;
use super::stack::{
    DestroyOptions, DestroyResult, PreviewOptions, PreviewResult, RefreshOptions, RefreshResult,
    Stack, UpOptions, UpResult, UpdateSummary,
};
use super::workspace::{
    svec, LocalWorkspace, LocalWorkspaceOptions, OutputMap, ProjectSettings, StackDeployment,
};

/// Whether `stack_name` is fully qualified, i.e. has owner, project, and
/// stack components (`owner/project/stack`).
pub fn is_fully_qualified_stack_name(stack_name: &str) -> bool {
    let parts: Vec<&str> = stack_name.split('/').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty())
}

/// A git source for a remote workspace: where Pulumi Deployments fetches
/// the program from. The Rust analogue of the Go SDK's `GitRepo` as remote
/// workspaces use it — no `setup` function and no local-clone options,
/// which cannot apply when the service does the fetching.
#[derive(Debug, Clone, Default)]
pub struct RemoteGitRepo {
    /// URL of the repository to fetch.
    pub url: String,
    /// Path relative to the repository root where the Pulumi program lives.
    pub project_path: Option<PathBuf>,
    /// Branch to fetch; exclusive with `commit_hash`.
    pub branch: Option<String>,
    /// Commit to fetch, as a full hash; exclusive with `branch`.
    pub commit_hash: Option<String>,
    /// Authentication for a private repository.
    pub auth: Option<GitAuth>,
}

/// The value of an environment variable a remote operation runs with.
#[derive(Clone, Default)]
pub struct EnvVarValue {
    pub value: String,
    /// Marks the value as a secret on the service (`--remote-env-secret`).
    pub secret: bool,
}

impl EnvVarValue {
    pub fn plain(value: impl Into<String>) -> Self {
        EnvVarValue {
            value: value.into(),
            secret: false,
        }
    }

    pub fn secret(value: impl Into<String>) -> Self {
        EnvVarValue {
            value: value.into(),
            secret: true,
        }
    }
}

/// Manual so a secret value never prints.
impl std::fmt::Debug for EnvVarValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvVarValue")
            .field(
                "value",
                if self.secret {
                    &"<redacted>"
                } else {
                    &self.value
                },
            )
            .field("secret", &self.secret)
            .finish()
    }
}

/// The image the remote operation's executor runs in.
#[derive(Debug, Clone, Default)]
pub struct ExecutorImage {
    pub image: String,
    /// Credentials for a private image registry.
    pub credentials: Option<DockerImageCredentials>,
}

/// Credentials for a private Docker image registry.
#[derive(Clone, Default)]
pub struct DockerImageCredentials {
    pub username: String,
    pub password: String,
}

/// Manual so the password never prints.
impl std::fmt::Debug for DockerImageCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DockerImageCredentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Options for constructing a [`RemoteStack`], mirroring Go's
/// `RemoteWorkspaceOption`s.
#[derive(Debug, Clone, Default)]
pub struct RemoteWorkspaceOptions {
    /// Environment variables the remote operation runs with. A `BTreeMap`
    /// so the serialized flags are deterministic, where Go's map is not.
    pub env_vars: BTreeMap<String, EnvVarValue>,
    /// Commands to run before the remote Pulumi operation is invoked.
    pub pre_run_commands: Vec<String>,
    /// Skip the default dependency installation step.
    pub skip_install_dependencies: bool,
    /// Inherit deployment settings from the stack; with this set, the git
    /// source may be left empty.
    pub inherit_settings: bool,
    /// The image to use for the remote executor.
    pub executor_image: Option<ExecutorImage>,
    /// The agent pool (deployment runner pool) to run the operation on.
    pub agent_pool_id: Option<String>,
}

fn is_set(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|v| !v.is_empty())
}

/// Client-side validation, with Go's exact error strings and precedence
/// (`remoteToLocalOptions`). One check has no Rust counterpart: Go rejects
/// a `repo.Setup` function, which [`RemoteGitRepo`] does not carry at all.
fn validate(repo: &RemoteGitRepo, options: &RemoteWorkspaceOptions) -> Result<()> {
    if !options.inherit_settings {
        const IF_NOT_SET: &str = " if RemoteInheritSettings(true) is not set";
        if repo.url.is_empty() {
            return Err(Error::setup(format!("repo.URL is required{IF_NOT_SET}")));
        }
        if !is_set(&repo.branch) && !is_set(&repo.commit_hash) {
            return Err(Error::setup(format!(
                "either repo.Branch or repo.CommitHash is required{IF_NOT_SET}"
            )));
        }
    }
    if is_set(&repo.branch) && is_set(&repo.commit_hash) {
        return Err(Error::setup(
            "repo.Branch and repo.CommitHash cannot both be specified",
        ));
    }
    if let Some(auth) = &repo.auth {
        let key_path_set = auth
            .ssh_private_key_path
            .as_deref()
            .is_some_and(|p| !p.as_os_str().is_empty());
        if is_set(&auth.ssh_private_key) && key_path_set {
            return Err(Error::setup(
                "repo.Auth.SSHPrivateKey and repo.Auth.SSHPrivateKeyPath cannot both be specified",
            ));
        }
    }

    for (key, value) in &options.env_vars {
        if key.is_empty() {
            return Err(Error::setup("envvar cannot be empty"));
        }
        if value.value.is_empty() {
            return Err(Error::setup(format!(
                "envvar {key:?} cannot have an empty value"
            )));
        }
    }

    for (index, command) in options.pre_run_commands.iter().enumerate() {
        if command.is_empty() {
            return Err(Error::setup(format!(
                "pre run command at index {index} cannot be empty"
            )));
        }
    }

    if let Some(image) = &options.executor_image {
        if image.image.is_empty() {
            return Err(Error::setup("executorImage.Image cannot be empty"));
        }
        if let Some(credentials) = &image.credentials {
            if credentials.username.is_empty() {
                return Err(Error::setup(
                    "executorImage.Credentials.Username cannot be empty",
                ));
            }
            if credentials.password.is_empty() {
                return Err(Error::setup(
                    "executorImage.Credentials.Password cannot be empty",
                ));
            }
        }
    }

    Ok(())
}

/// The `--remote*` arguments a remote operation carries, in the exact
/// order and spelling of Go's `Stack.remoteArgs`.
fn remote_args(repo: &RemoteGitRepo, options: &RemoteWorkspaceOptions) -> Vec<String> {
    let mut args = vec!["--remote".to_string()];
    if !repo.url.is_empty() {
        args.push(repo.url.clone());
    }
    if let Some(branch) = repo.branch.as_deref().filter(|b| !b.is_empty()) {
        args.push(format!("--remote-git-branch={branch}"));
    }
    if let Some(hash) = repo.commit_hash.as_deref().filter(|h| !h.is_empty()) {
        args.push(format!("--remote-git-commit={hash}"));
    }
    if let Some(path) = repo
        .project_path
        .as_deref()
        .filter(|p| !p.as_os_str().is_empty())
    {
        args.push(format!("--remote-git-repo-dir={}", path.display()));
    }
    if let Some(auth) = &repo.auth {
        if let Some(token) = auth
            .personal_access_token
            .as_deref()
            .filter(|t| !t.is_empty())
        {
            args.push(format!("--remote-git-auth-access-token={token}"));
        }
        if let Some(key) = auth.ssh_private_key.as_deref().filter(|k| !k.is_empty()) {
            args.push(format!("--remote-git-auth-ssh-private-key={key}"));
        }
        if let Some(path) = auth
            .ssh_private_key_path
            .as_deref()
            .filter(|p| !p.as_os_str().is_empty())
        {
            args.push(format!(
                "--remote-git-auth-ssh-private-key-path={}",
                path.display()
            ));
        }
        if let Some(password) = auth.password.as_deref().filter(|p| !p.is_empty()) {
            args.push(format!("--remote-git-auth-password={password}"));
        }
        if let Some(username) = auth.username.as_deref().filter(|u| !u.is_empty()) {
            args.push(format!("--remote-git-auth-username={username}"));
        }
    }

    for (key, value) in &options.env_vars {
        let flag = if value.secret {
            "--remote-env-secret"
        } else {
            "--remote-env"
        };
        args.push(format!("{flag}={key}={}", value.value));
    }

    for command in &options.pre_run_commands {
        args.push(format!("--remote-pre-run-command={command}"));
    }

    if let Some(image) = &options.executor_image {
        args.push(format!("--remote-executor-image={}", image.image));
        if let Some(credentials) = &image.credentials {
            if !credentials.username.is_empty() {
                args.push(format!(
                    "--remote-executor-image-username={}",
                    credentials.username
                ));
            }
            if !credentials.password.is_empty() {
                args.push(format!(
                    "--remote-executor-image-password={}",
                    credentials.password
                ));
            }
        }
    }

    if let Some(pool) = options.agent_pool_id.as_deref().filter(|p| !p.is_empty()) {
        args.push(format!("--remote-agent-pool-id={pool}"));
    }

    if options.skip_install_dependencies {
        args.push("--remote-skip-install-dependencies".to_string());
    }

    if options.inherit_settings {
        args.push("--remote-inherit-settings".to_string());
    }

    args
}

/// Fail unless the CLI supports remote operations, detected the way Go
/// does: `--remote` appearing in `pulumi preview --help`. `skip` (the
/// `PULUMI_AUTOMATION_API_SKIP_VERSION_CHECK` opt-out) bypasses the gate,
/// and so does the `0.0.0` version sentinel an explicit skip stores, as
/// with `require_version`.
async fn ensure_remote_support(workspace: &LocalWorkspace, skip: bool) -> Result<()> {
    if skip || workspace.pulumi_version() == Version::new(0, 0, 0) {
        return Ok(());
    }
    let result = workspace
        .command()
        .run(CommandSpec {
            args: svec(["preview", "--help"]),
            workdir: workspace.work_dir().to_path_buf(),
            env: vec![
                ("PULUMI_DEBUG_COMMANDS".to_string(), "true".to_string()),
                ("PULUMI_EXPERIMENTAL".to_string(), "true".to_string()),
            ],
            stdin: None,
        })
        .await?;
    if result.stdout.contains("--remote") {
        Ok(())
    } else {
        Err(Error::setup(
            "Pulumi CLI does not support remote operations; please upgrade",
        ))
    }
}

/// How a constructor binds the stack name on the service.
#[derive(Debug, Clone, Copy)]
enum InitMode {
    Create,
    Select,
    CreateOrSelect,
}

/// A stack whose deployment operations (`up`, `preview`, `refresh`,
/// `destroy`) run remotely in Pulumi Deployments. The Rust analogue of the
/// Go SDK's `auto.RemoteStack`.
#[derive(Debug, Clone)]
pub struct RemoteStack {
    stack: Stack,
}

impl RemoteStack {
    /// Create a new remote stack sourced from a git repository, the
    /// analogue of Go's `NewRemoteStackGitSource`.
    pub async fn create_git_source(
        stack_name: impl Into<String>,
        repo: RemoteGitRepo,
        options: RemoteWorkspaceOptions,
    ) -> Result<Self> {
        Self::from_git_source(stack_name.into(), repo, options, InitMode::Create).await
    }

    /// Select an existing remote stack sourced from a git repository, the
    /// analogue of Go's `SelectRemoteStackGitSource`.
    pub async fn select_git_source(
        stack_name: impl Into<String>,
        repo: RemoteGitRepo,
        options: RemoteWorkspaceOptions,
    ) -> Result<Self> {
        Self::from_git_source(stack_name.into(), repo, options, InitMode::Select).await
    }

    /// [`RemoteStack::create_git_source`], selecting the stack if it
    /// already exists — the analogue of Go's `UpsertRemoteStackGitSource`.
    pub async fn create_or_select_git_source(
        stack_name: impl Into<String>,
        repo: RemoteGitRepo,
        options: RemoteWorkspaceOptions,
    ) -> Result<Self> {
        Self::from_git_source(stack_name.into(), repo, options, InitMode::CreateOrSelect).await
    }

    async fn from_git_source(
        name: String,
        repo: RemoteGitRepo,
        options: RemoteWorkspaceOptions,
        mode: InitMode,
    ) -> Result<Self> {
        if !is_fully_qualified_stack_name(&name) {
            return Err(Error::setup(format!(
                "stack name {name:?} must be fully qualified"
            )));
        }
        validate(&repo, &options)?;

        let context = match mode {
            InitMode::Select => "failed to select stack",
            _ => "failed to create stack",
        };
        let workspace = LocalWorkspace::new(LocalWorkspaceOptions::default())
            .await
            .map_err(|e| e.with_context(context))?;
        ensure_remote_support(&workspace, skip_version_check_from_env(&HashMap::new()))
            .await
            .map_err(|e| e.with_context(context))?;
        Self::init(workspace, name, remote_args(&repo, &options), mode).await
    }

    async fn init(
        workspace: LocalWorkspace,
        name: String,
        remote_args: Vec<String>,
        mode: InitMode,
    ) -> Result<Self> {
        // CLI v3.211 through v3.255 read the project before honoring
        // --remote on up/preview/refresh (pulumi/pulumi#24050), so the
        // otherwise-empty workspace gets a stub project file for the
        // stack's project. Remote operations never read past its presence.
        let project = name.split('/').nth(1).unwrap_or_default();
        workspace.save_project_settings(&ProjectSettings::new(project, "yaml"))?;
        match mode {
            InitMode::Create => create_remote_stack(&workspace, &name).await?,
            InitMode::Select => select_remote_stack(&workspace, &name).await?,
            // Select-first with a 404 fallback, as Go's UpsertStack and the
            // local Stack::create_or_select do.
            InitMode::CreateOrSelect => match select_remote_stack(&workspace, &name).await {
                Ok(()) => {}
                Err(e) if e.is_select_stack_404_error() => {
                    create_remote_stack(&workspace, &name).await?
                }
                Err(e) => return Err(e),
            },
        }
        Ok(RemoteStack {
            stack: Stack::remote(name, workspace, remote_args),
        })
    }

    /// The stack's fully qualified name.
    pub fn name(&self) -> &str {
        self.stack.name()
    }

    /// Create or update the stack's resources (`pulumi up`). This
    /// operation runs remotely.
    pub async fn up(&self, options: RemoteUpOptions) -> Result<UpResult> {
        self.stack
            .up(UpOptions {
                event_senders: options.event_senders,
                ..Default::default()
            })
            .await
    }

    /// Preview the changes an `up` would perform (`pulumi preview`). This
    /// operation runs remotely.
    pub async fn preview(&self, options: RemotePreviewOptions) -> Result<PreviewResult> {
        self.stack
            .preview(PreviewOptions {
                event_senders: options.event_senders,
                ..Default::default()
            })
            .await
    }

    /// Compare the stack's state against the real resources and update the
    /// state to match (`pulumi refresh`). This operation runs remotely.
    pub async fn refresh(&self, options: RemoteRefreshOptions) -> Result<RefreshResult> {
        self.stack
            .refresh(RefreshOptions {
                event_senders: options.event_senders,
                ..Default::default()
            })
            .await
    }

    /// Delete every resource in the stack (`pulumi destroy`). This
    /// operation runs remotely.
    pub async fn destroy(&self, options: RemoteDestroyOptions) -> Result<DestroyResult> {
        self.stack
            .destroy(DestroyOptions {
                event_senders: options.event_senders,
                ..Default::default()
            })
            .await
    }

    /// The stack's outputs from the last `up`.
    pub async fn outputs(&self) -> Result<OutputMap> {
        self.stack.outputs().await
    }

    /// `pulumi stack history --json`. Secrets stay encrypted, as in Go:
    /// the workspace holds only a stub project file, never the stack's
    /// secrets configuration.
    pub async fn history(&self, page_size: Option<u32>, page: u32) -> Result<Vec<UpdateSummary>> {
        self.stack.history(page_size, page, Some(false)).await
    }

    /// Cancel the stack's currently running update, if any. Leaves the
    /// stack in an inconsistent state; use only when an update is stuck.
    pub async fn cancel(&self) -> Result<()> {
        self.stack.cancel().await
    }

    /// Export the stack's deployment state.
    pub async fn export(&self) -> Result<StackDeployment> {
        self.stack.export().await
    }

    /// Import previously exported deployment state.
    pub async fn import(&self, state: &StackDeployment) -> Result<()> {
        self.stack.import(state).await
    }
}

/// `pulumi stack init --no-select`: a remote workspace must not modify the
/// CLI's global stack selection.
async fn create_remote_stack(workspace: &LocalWorkspace, name: &str) -> Result<()> {
    workspace
        .run_cmd(svec(["stack", "init", name, "--no-select"]))
        .await
        .map_err(|e| e.with_context("failed to create stack"))?;
    Ok(())
}

/// `pulumi stack --stack <name>`: verifies the stack exists without
/// selecting it, as Go's remote SelectStack does.
async fn select_remote_stack(workspace: &LocalWorkspace, name: &str) -> Result<()> {
    workspace
        .run_cmd(svec(["stack", "--stack", name]))
        .await
        .map_err(|e| e.with_context("failed to select stack"))?;
    Ok(())
}

/// Options for [`RemoteStack::up`]. Remote operations accept none of the
/// local deployment flags; only event streaming, as in Go's `optremoteup`.
#[derive(Clone, Default)]
pub struct RemoteUpOptions {
    /// Live engine events are cloned into each sender; the channels close
    /// when the operation's event stream ends.
    pub event_senders: Vec<tokio::sync::mpsc::UnboundedSender<super::events::EngineEvent>>,
}

/// Options for [`RemoteStack::preview`].
#[derive(Clone, Default)]
pub struct RemotePreviewOptions {
    pub event_senders: Vec<tokio::sync::mpsc::UnboundedSender<super::events::EngineEvent>>,
}

/// Options for [`RemoteStack::refresh`].
#[derive(Clone, Default)]
pub struct RemoteRefreshOptions {
    pub event_senders: Vec<tokio::sync::mpsc::UnboundedSender<super::events::EngineEvent>>,
}

/// Options for [`RemoteStack::destroy`].
#[derive(Clone, Default)]
pub struct RemoteDestroyOptions {
    pub event_senders: Vec<tokio::sync::mpsc::UnboundedSender<super::events::EngineEvent>>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::cmd::test_support::RecordingCommand;
    use super::super::errors::CommandResult;
    use super::*;

    const TEST_REPO: &str = "https://github.com/pulumi/test-repo.git";
    const STACK: &str = "owner/project/stack";

    fn ok_json(json: &str) -> Result<CommandResult> {
        Ok(CommandResult {
            stdout: json.to_string(),
            ..Default::default()
        })
    }

    async fn recording_workspace(recorder: &Arc<RecordingCommand>) -> LocalWorkspace {
        LocalWorkspace::new(LocalWorkspaceOptions {
            pulumi_command: Some(recorder.clone()),
            ..Default::default()
        })
        .await
        .unwrap()
    }

    async fn recording_remote_stack(
        remote_args: Vec<String>,
    ) -> (Arc<RecordingCommand>, RemoteStack) {
        let recorder = Arc::new(RecordingCommand::default());
        let ws = recording_workspace(&recorder).await;
        (
            recorder,
            RemoteStack {
                stack: Stack::remote(STACK.to_string(), ws, remote_args),
            },
        )
    }

    /// The validation matrix of Go's TestNewRemoteStackGitSourceErrors
    /// (and its Select/Upsert variants), plus the envvar and pre-run
    /// command cases Go validates and Python's parameterized tests pin,
    /// run against all three constructors with Go's exact messages.
    #[tokio::test]
    async fn git_source_constructors_validate_with_go_messages() {
        let with_branch = || RemoteGitRepo {
            url: TEST_REPO.to_string(),
            branch: Some("branch".to_string()),
            ..Default::default()
        };
        let with_credentials = |username: &str, password: &str| {
            Some(DockerImageCredentials {
                username: username.to_string(),
                password: password.to_string(),
            })
        };

        struct Case {
            name: &'static str,
            stack: &'static str,
            repo: RemoteGitRepo,
            options: RemoteWorkspaceOptions,
            err: &'static str,
        }
        let case = |name, stack, repo, options, err| Case {
            name,
            stack,
            repo,
            options,
            err,
        };
        let none = RemoteWorkspaceOptions::default;

        let cases = [
            case(
                "stack empty",
                "",
                RemoteGitRepo::default(),
                none(),
                r#"stack name "" must be fully qualified"#,
            ),
            case(
                "stack just name",
                "name",
                RemoteGitRepo::default(),
                none(),
                r#"stack name "name" must be fully qualified"#,
            ),
            case(
                "stack just name & owner",
                "owner/name",
                RemoteGitRepo::default(),
                none(),
                r#"stack name "owner/name" must be fully qualified"#,
            ),
            case(
                "stack just sep",
                "/",
                RemoteGitRepo::default(),
                none(),
                r#"stack name "/" must be fully qualified"#,
            ),
            case(
                "stack just two seps",
                "//",
                RemoteGitRepo::default(),
                none(),
                r#"stack name "//" must be fully qualified"#,
            ),
            case(
                "stack just three seps",
                "///",
                RemoteGitRepo::default(),
                none(),
                r#"stack name "///" must be fully qualified"#,
            ),
            case(
                "stack invalid",
                "owner/project/stack/wat",
                RemoteGitRepo::default(),
                none(),
                r#"stack name "owner/project/stack/wat" must be fully qualified"#,
            ),
            case(
                "no url",
                STACK,
                RemoteGitRepo::default(),
                none(),
                "repo.URL is required if RemoteInheritSettings(true) is not set",
            ),
            case(
                "no branch or commit",
                STACK,
                RemoteGitRepo {
                    url: TEST_REPO.to_string(),
                    ..Default::default()
                },
                none(),
                "either repo.Branch or repo.CommitHash is required \
                 if RemoteInheritSettings(true) is not set",
            ),
            case(
                "both branch and commit",
                STACK,
                RemoteGitRepo {
                    url: TEST_REPO.to_string(),
                    branch: Some("branch".to_string()),
                    commit_hash: Some("commit".to_string()),
                    ..Default::default()
                },
                none(),
                "repo.Branch and repo.CommitHash cannot both be specified",
            ),
            case(
                "both ssh private key and path",
                STACK,
                RemoteGitRepo {
                    auth: Some(GitAuth {
                        ssh_private_key: Some("key".to_string()),
                        ssh_private_key_path: Some(PathBuf::from("path")),
                        ..Default::default()
                    }),
                    ..with_branch()
                },
                none(),
                "repo.Auth.SSHPrivateKey and repo.Auth.SSHPrivateKeyPath \
                 cannot both be specified",
            ),
            case(
                "empty envvar",
                STACK,
                with_branch(),
                RemoteWorkspaceOptions {
                    env_vars: BTreeMap::from([(String::new(), EnvVarValue::plain("bar"))]),
                    ..Default::default()
                },
                "envvar cannot be empty",
            ),
            case(
                "envvar with empty value",
                STACK,
                with_branch(),
                RemoteWorkspaceOptions {
                    env_vars: BTreeMap::from([("foo".to_string(), EnvVarValue::plain(""))]),
                    ..Default::default()
                },
                r#"envvar "foo" cannot have an empty value"#,
            ),
            case(
                "empty pre run command",
                STACK,
                with_branch(),
                RemoteWorkspaceOptions {
                    pre_run_commands: vec![String::new()],
                    ..Default::default()
                },
                "pre run command at index 0 cannot be empty",
            ),
            case(
                "executor creds with no image",
                STACK,
                with_branch(),
                RemoteWorkspaceOptions {
                    executor_image: Some(ExecutorImage {
                        image: String::new(),
                        credentials: with_credentials("user", "password"),
                    }),
                    ..Default::default()
                },
                "executorImage.Image cannot be empty",
            ),
            case(
                "executor image with username and no password",
                STACK,
                with_branch(),
                RemoteWorkspaceOptions {
                    executor_image: Some(ExecutorImage {
                        image: "image".to_string(),
                        credentials: with_credentials("username", ""),
                    }),
                    ..Default::default()
                },
                "executorImage.Credentials.Password cannot be empty",
            ),
            case(
                "executor image with password and no username",
                STACK,
                with_branch(),
                RemoteWorkspaceOptions {
                    executor_image: Some(ExecutorImage {
                        image: "image".to_string(),
                        credentials: with_credentials("", "password"),
                    }),
                    ..Default::default()
                },
                "executorImage.Credentials.Username cannot be empty",
            ),
        ];

        for c in &cases {
            for constructor in ["create", "select", "create_or_select"] {
                let result = match constructor {
                    "create" => {
                        RemoteStack::create_git_source(c.stack, c.repo.clone(), c.options.clone())
                            .await
                    }
                    "select" => {
                        RemoteStack::select_git_source(c.stack, c.repo.clone(), c.options.clone())
                            .await
                    }
                    _ => {
                        RemoteStack::create_or_select_git_source(
                            c.stack,
                            c.repo.clone(),
                            c.options.clone(),
                        )
                        .await
                    }
                };
                let err = result.expect_err(c.name);
                assert_eq!(err.to_string(), c.err, "{} via {constructor}", c.name);
            }
        }
    }

    /// The accept/reject table shared by the Go, Node and Python SDKs.
    #[test]
    fn fully_qualified_stack_name_table() {
        for (input, expected) in [
            ("owner/project/stack", true),
            ("", false),
            ("name", false),
            ("owner/name", false),
            ("/", false),
            ("//", false),
            ("///", false),
            ("owner/project/stack/wat", false),
        ] {
            assert_eq!(
                is_fully_qualified_stack_name(input),
                expected,
                "input {input:?}"
            );
        }
    }

    /// Every flag the serializer can emit, in Go's exact order and `=`
    /// spelling — the cases of Node's "remote cmd args" table.
    #[test]
    fn remote_args_serialize_as_go_does() {
        let url_repo = || RemoteGitRepo {
            url: "foo".to_string(),
            ..Default::default()
        };
        let with_auth = |auth: GitAuth| RemoteGitRepo {
            auth: Some(auth),
            ..url_repo()
        };
        let none = RemoteWorkspaceOptions::default;

        let cases: Vec<(&str, RemoteGitRepo, RemoteWorkspaceOptions, Vec<&str>)> = vec![
            (
                "just remote",
                RemoteGitRepo::default(),
                none(),
                vec!["--remote"],
            ),
            ("url", url_repo(), none(), vec!["--remote", "foo"]),
            (
                "path",
                RemoteGitRepo {
                    project_path: Some(PathBuf::from("mypath")),
                    ..url_repo()
                },
                none(),
                vec!["--remote", "foo", "--remote-git-repo-dir=mypath"],
            ),
            (
                "branch",
                RemoteGitRepo {
                    branch: Some("mybranch".to_string()),
                    ..url_repo()
                },
                none(),
                vec!["--remote", "foo", "--remote-git-branch=mybranch"],
            ),
            (
                "commit",
                RemoteGitRepo {
                    commit_hash: Some("mycommit".to_string()),
                    ..url_repo()
                },
                none(),
                vec!["--remote", "foo", "--remote-git-commit=mycommit"],
            ),
            (
                "auth access token",
                with_auth(GitAuth {
                    personal_access_token: Some("mytoken".to_string()),
                    ..Default::default()
                }),
                none(),
                vec!["--remote", "foo", "--remote-git-auth-access-token=mytoken"],
            ),
            (
                "auth ssh key",
                with_auth(GitAuth {
                    ssh_private_key: Some("mykey".to_string()),
                    ..Default::default()
                }),
                none(),
                vec!["--remote", "foo", "--remote-git-auth-ssh-private-key=mykey"],
            ),
            (
                "auth ssh key path",
                with_auth(GitAuth {
                    ssh_private_key_path: Some(PathBuf::from("mykeypath")),
                    ..Default::default()
                }),
                none(),
                vec![
                    "--remote",
                    "foo",
                    "--remote-git-auth-ssh-private-key-path=mykeypath",
                ],
            ),
            (
                "auth ssh password",
                with_auth(GitAuth {
                    username: Some("myuser".to_string()),
                    password: Some("mypass".to_string()),
                    ..Default::default()
                }),
                none(),
                vec![
                    "--remote",
                    "foo",
                    "--remote-git-auth-password=mypass",
                    "--remote-git-auth-username=myuser",
                ],
            ),
            (
                "env",
                url_repo(),
                RemoteWorkspaceOptions {
                    env_vars: BTreeMap::from([("foo".to_string(), EnvVarValue::plain("bar"))]),
                    ..Default::default()
                },
                vec!["--remote", "foo", "--remote-env=foo=bar"],
            ),
            (
                "env secret",
                url_repo(),
                RemoteWorkspaceOptions {
                    env_vars: BTreeMap::from([("foo".to_string(), EnvVarValue::secret("bar"))]),
                    ..Default::default()
                },
                vec!["--remote", "foo", "--remote-env-secret=foo=bar"],
            ),
            (
                "pre-run command",
                url_repo(),
                RemoteWorkspaceOptions {
                    pre_run_commands: vec!["whoami".to_string()],
                    ..Default::default()
                },
                vec!["--remote", "foo", "--remote-pre-run-command=whoami"],
            ),
            (
                "skip install dependencies",
                url_repo(),
                RemoteWorkspaceOptions {
                    skip_install_dependencies: true,
                    ..Default::default()
                },
                vec!["--remote", "foo", "--remote-skip-install-dependencies"],
            ),
            (
                "inherit settings",
                RemoteGitRepo::default(),
                RemoteWorkspaceOptions {
                    inherit_settings: true,
                    ..Default::default()
                },
                vec!["--remote", "--remote-inherit-settings"],
            ),
            (
                "remote image",
                RemoteGitRepo::default(),
                RemoteWorkspaceOptions {
                    executor_image: Some(ExecutorImage {
                        image: "test-image".to_string(),
                        credentials: None,
                    }),
                    ..Default::default()
                },
                vec!["--remote", "--remote-executor-image=test-image"],
            ),
            (
                "remote image credentials",
                RemoteGitRepo::default(),
                RemoteWorkspaceOptions {
                    executor_image: Some(ExecutorImage {
                        image: "test-image".to_string(),
                        credentials: Some(DockerImageCredentials {
                            username: "foo".to_string(),
                            password: "bar".to_string(),
                        }),
                    }),
                    ..Default::default()
                },
                vec![
                    "--remote",
                    "--remote-executor-image=test-image",
                    "--remote-executor-image-username=foo",
                    "--remote-executor-image-password=bar",
                ],
            ),
            (
                "agent pool id",
                RemoteGitRepo::default(),
                RemoteWorkspaceOptions {
                    agent_pool_id: Some("pool".to_string()),
                    ..Default::default()
                },
                vec!["--remote", "--remote-agent-pool-id=pool"],
            ),
        ];

        for (name, repo, options, expected) in cases {
            let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
            assert_eq!(remote_args(&repo, &options), expected, "case {name:?}");
        }
    }

    /// Node's "just remote" case, driven end to end: the remote args land
    /// after the shared options on `up`, PULUMI_EXPERIMENTAL rides the
    /// invocation, and the follow-up history never decrypts secrets.
    #[tokio::test]
    async fn up_carries_remote_args_and_experimental_env() {
        let args = remote_args(
            &RemoteGitRepo::default(),
            &RemoteWorkspaceOptions::default(),
        );
        assert_eq!(args, svec(["--remote"]));

        let (recorder, stack) = recording_remote_stack(args).await;
        recorder.push_result(Ok(CommandResult::default())); // up
        recorder.push_result(ok_json("{}")); // outputs masked
        recorder.push_result(ok_json("{}")); // outputs shown
        recorder.push_result(ok_json("[]")); // history

        stack.up(RemoteUpOptions::default()).await.unwrap();

        let recorded = recorder.recorded_args();
        assert_eq!(
            recorded[0],
            svec([
                "up",
                "--yes",
                "--skip-preview",
                "--exec-kind=auto.local",
                "--remote",
                "--stack",
                STACK,
            ])
        );
        // The remote history never asks the CLI to decrypt secrets.
        assert!(
            !recorded[3].contains(&"--show-secrets".to_string()),
            "history args: {:?}",
            recorded[3]
        );
        let specs = recorder.specs.lock().unwrap();
        assert!(specs[0]
            .env
            .iter()
            .any(|(k, v)| k == "PULUMI_EXPERIMENTAL" && v == "true"));
    }

    /// Node's "env secret" case through `up`.
    #[tokio::test]
    async fn up_serializes_a_secret_env_var() {
        let repo = RemoteGitRepo {
            url: "foo".to_string(),
            ..Default::default()
        };
        let options = RemoteWorkspaceOptions {
            env_vars: BTreeMap::from([("foo".to_string(), EnvVarValue::secret("bar"))]),
            ..Default::default()
        };
        let (recorder, stack) = recording_remote_stack(remote_args(&repo, &options)).await;
        recorder.push_result(Ok(CommandResult::default())); // up
        recorder.push_result(ok_json("{}")); // outputs masked
        recorder.push_result(ok_json("{}")); // outputs shown
        recorder.push_result(ok_json("[]")); // history

        stack.up(RemoteUpOptions::default()).await.unwrap();

        assert_eq!(
            recorder.recorded_args()[0],
            svec([
                "up",
                "--yes",
                "--skip-preview",
                "--exec-kind=auto.local",
                "--remote",
                "foo",
                "--remote-env-secret=foo=bar",
                "--stack",
                STACK,
            ])
        );
    }

    /// Node's "remote image credentials" case through `destroy`, which
    /// places the remote args after `--exec-kind`, as Go does.
    #[tokio::test]
    async fn destroy_serializes_executor_image_credentials() {
        let options = RemoteWorkspaceOptions {
            executor_image: Some(ExecutorImage {
                image: "test-image".to_string(),
                credentials: Some(DockerImageCredentials {
                    username: "foo".to_string(),
                    password: "bar".to_string(),
                }),
            }),
            ..Default::default()
        };
        let (recorder, stack) =
            recording_remote_stack(remote_args(&RemoteGitRepo::default(), &options)).await;
        recorder.push_result(Ok(CommandResult::default())); // destroy
        recorder.push_result(ok_json("[]")); // history

        stack
            .destroy(RemoteDestroyOptions::default())
            .await
            .unwrap();

        assert_eq!(
            recorder.recorded_args()[0],
            svec([
                "destroy",
                "--exec-kind=auto.local",
                "--remote",
                "--remote-executor-image=test-image",
                "--remote-executor-image-username=foo",
                "--remote-executor-image-password=bar",
                "--yes",
                "--skip-preview",
                "--stack",
                STACK,
            ])
        );
    }

    /// The remaining operations carry the remote args in Go's positions:
    /// preview after the shared options, refresh before `--exec-kind`.
    #[tokio::test]
    async fn preview_and_refresh_carry_remote_args() {
        let args = remote_args(
            &RemoteGitRepo::default(),
            &RemoteWorkspaceOptions::default(),
        );

        let (recorder, stack) = recording_remote_stack(args.clone()).await;
        // The mock writes no events, so preview fails on the missing
        // summary after recording its args.
        let _ = stack.preview(RemotePreviewOptions::default()).await;
        let preview = &recorder.recorded_args()[0];
        assert_eq!(
            preview[..3],
            svec(["preview", "--exec-kind=auto.local", "--remote"])
        );

        let (recorder, stack) = recording_remote_stack(args).await;
        recorder.push_result(Ok(CommandResult::default())); // refresh
        recorder.push_result(ok_json("[]")); // history
        stack
            .refresh(RemoteRefreshOptions::default())
            .await
            .unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec([
                "refresh",
                "--yes",
                "--skip-preview",
                "--remote",
                "--exec-kind=auto.local",
                "--stack",
                STACK,
            ])
        );
    }

    /// The raw gate error; the constructors wrap it with their "failed to
    /// create stack"/"failed to select stack" context, as Go's do.
    #[tokio::test]
    async fn support_gate_errors_when_help_lacks_remote() {
        let recorder = Arc::new(RecordingCommand::default());
        let ws = recording_workspace(&recorder).await;
        recorder.push_result(ok_json("Flags:\n      --diff\n"));

        let err = ensure_remote_support(&ws, false).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Pulumi CLI does not support remote operations; please upgrade"
        );
        assert_eq!(recorder.recorded_args()[0], svec(["preview", "--help"]));
        let specs = recorder.specs.lock().unwrap();
        for var in ["PULUMI_DEBUG_COMMANDS", "PULUMI_EXPERIMENTAL"] {
            assert!(
                specs[0].env.iter().any(|(k, v)| k == var && v == "true"),
                "missing {var}"
            );
        }
    }

    #[tokio::test]
    async fn support_gate_accepts_help_mentioning_remote() {
        let recorder = Arc::new(RecordingCommand::default());
        let ws = recording_workspace(&recorder).await;
        recorder.push_result(ok_json(
            "Flags:\n      --remote   Run the operation remotely\n",
        ));
        ensure_remote_support(&ws, false).await.unwrap();
    }

    /// Both bypasses skip the probe entirely: the explicit
    /// PULUMI_AUTOMATION_API_SKIP_VERSION_CHECK opt-out, and the 0.0.0
    /// version sentinel an explicit skip stores, as with require_version.
    #[tokio::test]
    async fn support_gate_honors_skip_and_version_sentinel() {
        let recorder = Arc::new(RecordingCommand::default());
        let ws = recording_workspace(&recorder).await;
        ensure_remote_support(&ws, true).await.unwrap();
        assert!(recorder.recorded_args().is_empty(), "no probe expected");

        let recorder = Arc::new(RecordingCommand {
            version: Version::new(0, 0, 0),
            ..Default::default()
        });
        let ws = recording_workspace(&recorder).await;
        ensure_remote_support(&ws, false).await.unwrap();
        assert!(recorder.recorded_args().is_empty(), "no probe expected");
    }

    /// Stack initialization never touches the CLI's global stack
    /// selection: create passes `--no-select`, select verifies existence
    /// with a bare `pulumi stack`, and create-or-select tries the latter
    /// first, creating only on a 404.
    #[tokio::test]
    async fn initialization_never_selects_the_stack() {
        let recorder = Arc::new(RecordingCommand::default());
        let ws = recording_workspace(&recorder).await;
        RemoteStack::init(ws, STACK.to_string(), svec(["--remote"]), InitMode::Create)
            .await
            .unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec(["stack", "init", STACK, "--no-select"])
        );

        let recorder = Arc::new(RecordingCommand::default());
        let ws = recording_workspace(&recorder).await;
        RemoteStack::init(ws, STACK.to_string(), svec(["--remote"]), InitMode::Select)
            .await
            .unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec(["stack", "--stack", STACK])
        );

        let recorder = Arc::new(RecordingCommand::default());
        let ws = recording_workspace(&recorder).await;
        recorder.push_result(Err(Error::command(
            "exit status 255",
            CommandResult {
                stderr: format!("error: no stack named '{STACK}' found"),
                code: 255,
                ..Default::default()
            },
        )));
        RemoteStack::init(
            ws,
            STACK.to_string(),
            svec(["--remote"]),
            InitMode::CreateOrSelect,
        )
        .await
        .unwrap();
        let recorded = recorder.recorded_args();
        assert_eq!(recorded[0], svec(["stack", "--stack", STACK]));
        assert_eq!(recorded[1], svec(["stack", "init", STACK, "--no-select"]));
    }

    /// Initialization gives the workspace a stub project file named after
    /// the stack's project: CLI v3.211 through v3.255 refuse a remote
    /// up/preview/refresh without one (pulumi/pulumi#24050).
    #[tokio::test]
    async fn initialization_writes_a_stub_project_file() {
        for mode in [InitMode::Create, InitMode::Select, InitMode::CreateOrSelect] {
            let recorder = Arc::new(RecordingCommand::default());
            let ws = recording_workspace(&recorder).await;
            let remote = RemoteStack::init(ws, STACK.to_string(), svec(["--remote"]), mode)
                .await
                .unwrap();
            let settings = remote
                .stack
                .workspace()
                .project_settings()
                .unwrap_or_else(|e| panic!("no stub project for {mode:?}: {e}"));
            assert_eq!(settings.name, "project", "{mode:?}");
            assert_eq!(settings.runtime.unwrap().name(), "yaml", "{mode:?}");
        }
    }

    #[tokio::test]
    async fn debug_output_redacts_secrets() {
        let repo = RemoteGitRepo {
            url: TEST_REPO.to_string(),
            branch: Some("branch".to_string()),
            auth: Some(GitAuth {
                personal_access_token: Some("hunter1".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let options = RemoteWorkspaceOptions {
            env_vars: BTreeMap::from([("token".to_string(), EnvVarValue::secret("hunter2"))]),
            executor_image: Some(ExecutorImage {
                image: "img".to_string(),
                credentials: Some(DockerImageCredentials {
                    username: "user".to_string(),
                    password: "hunter3".to_string(),
                }),
            }),
            ..Default::default()
        };
        let secrets = ["hunter1", "hunter2", "hunter3"];

        let debug = format!("{options:?}");
        for secret in secrets {
            assert!(!debug.contains(secret), "leaked {secret}: {debug}");
        }

        // The stack's stored remote args carry the plaintext secrets, so
        // its Debug must redact them too.
        let (_recorder, stack) = recording_remote_stack(remote_args(&repo, &options)).await;
        let debug = format!("{stack:?}");
        for secret in secrets {
            assert!(!debug.contains(secret), "leaked {secret}: {debug}");
        }

        // A plain env var still prints.
        let plain = format!("{:?}", EnvVarValue::plain("visible"));
        assert!(plain.contains("visible"), "unexpected: {plain}");
    }
}
