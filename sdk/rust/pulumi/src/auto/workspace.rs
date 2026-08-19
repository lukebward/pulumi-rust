//! [`LocalWorkspace`]: a Pulumi project rooted in a directory on disk (or
//! a generated one, for inline programs), and every operation that works
//! on the project rather than on one update.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::cmd::{
    skip_version_check_from_env, CommandSpec, LocalPulumiCommand, PulumiCommand,
    PulumiCommandOptions, SharedCommand,
};
use super::errors::{CommandResult, Error, Result};
use super::ProgramFn;

/// `Pulumi.yaml`, the project file. Named fields cover what the automation
/// API itself touches; everything else a user's project file carries
/// survives a load/save round trip through `extra`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<ProjectRuntimeInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml_ng::Value>,
}

impl ProjectSettings {
    pub fn new(name: impl Into<String>, runtime: impl Into<String>) -> Self {
        ProjectSettings {
            name: name.into(),
            runtime: Some(ProjectRuntimeInfo::Name(runtime.into())),
            main: None,
            description: None,
            extra: BTreeMap::new(),
        }
    }
}

/// The `runtime` key of `Pulumi.yaml`: either a bare runtime name, or a
/// name with options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProjectRuntimeInfo {
    Name(String),
    Full {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<serde_yaml_ng::Value>,
    },
}

impl ProjectRuntimeInfo {
    pub fn name(&self) -> &str {
        match self {
            ProjectRuntimeInfo::Name(name) => name,
            ProjectRuntimeInfo::Full { name, .. } => name,
        }
    }
}

/// `Pulumi.<stack>.yaml`, a stack's settings file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StackSettings {
    #[serde(
        rename = "secretsprovider",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub secrets_provider: Option<String>,
    #[serde(
        rename = "encryptedkey",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub encrypted_key: Option<String>,
    #[serde(
        rename = "encryptionsalt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub encryption_salt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<BTreeMap<String, StackSettingsConfigValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<serde_yaml_ng::Value>,
}

/// One stack-settings config entry: a `{secure: ciphertext}` object for a
/// secret, any other YAML value for plaintext.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum StackSettingsConfigValue {
    Secure { secure: String },
    Plain(serde_yaml_ng::Value),
}

/// The CLI treats a mapping as a secret only when it is *exactly*
/// `{secure: <string>}`; a mapping that merely contains a `secure` key
/// among others is an ordinary object value and must keep every key.
impl<'de> Deserialize<'de> for StackSettingsConfigValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = serde_yaml_ng::Value::deserialize(deserializer)?;
        if let serde_yaml_ng::Value::Mapping(mapping) = &value {
            if mapping.len() == 1 {
                if let Some(serde_yaml_ng::Value::String(ciphertext)) = mapping.get("secure") {
                    return Ok(StackSettingsConfigValue::Secure {
                        secure: ciphertext.clone(),
                    });
                }
            }
        }
        Ok(StackSettingsConfigValue::Plain(value))
    }
}

/// One stack config value, as `pulumi config --json` reports it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigValue {
    /// The CLI omits the value entirely for a secret it was not asked to
    /// decrypt (`stack history --json` without `--show-secrets`), which
    /// reads as empty here.
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub secret: bool,
}

impl ConfigValue {
    pub fn plain(value: impl Into<String>) -> Self {
        ConfigValue {
            value: value.into(),
            secret: false,
        }
    }

    pub fn secret(value: impl Into<String>) -> Self {
        ConfigValue {
            value: value.into(),
            secret: true,
        }
    }
}

/// Stack configuration keyed by `namespace:name`.
pub type ConfigMap = BTreeMap<String, ConfigValue>;

/// Options for the config operations.
#[derive(Debug, Clone, Default)]
pub struct ConfigOptions {
    /// Treat the key as a path into an object value.
    pub path: bool,
    /// Operate on this config file instead of the stack's default.
    pub config_file: Option<PathBuf>,
}

/// One stack output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputValue {
    pub value: Value,
    pub secret: bool,
}

/// Stack outputs keyed by name.
pub type OutputMap = BTreeMap<String, OutputValue>;

/// One entry of `pulumi stack ls --json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackSummary {
    pub name: String,
    #[serde(default)]
    pub current: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_update: Option<String>,
    #[serde(default)]
    pub update_in_progress: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The result of `pulumi whoami --json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhoAmIResult {
    pub user: String,
    #[serde(default)]
    pub organizations: Vec<String>,
    #[serde(default)]
    pub url: String,
}

/// One entry of `pulumi plugin ls --json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// An exported stack state, round-tripped opaquely.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StackDeployment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub deployment: Value,
}

/// Options for [`LocalWorkspace::list_stacks_with_options`], mirroring
/// Go's `optlist.Options`.
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// List every stack on the backend, not just the current project's.
    pub all: bool,
}

/// Options for [`LocalWorkspace::new_project`] (`pulumi new`), mirroring
/// Go's `NewOptions`.
#[derive(Debug, Clone, Default)]
pub struct NewOptions {
    /// The template name or URL; a local template directory also works.
    pub template_or_url: Option<String>,
    /// The prompt to use for Pulumi AI.
    pub ai: Option<String>,
    /// Config values to save, as `key=value` strings.
    pub config: Vec<String>,
    /// Config keys contain a path to a property in a map or list to set.
    pub config_path: bool,
    pub description: Option<String>,
    /// Where to place the generated project; defaults to the work dir.
    pub dir: Option<PathBuf>,
    /// Generate content even if it would change existing files.
    pub force: bool,
    /// Generate the project only: no stack, no config, no dependencies.
    pub generate_only: bool,
    /// The language to use for Pulumi AI.
    pub language: Option<String>,
    /// List locally installed templates and exit.
    pub list_templates: bool,
    /// The project name.
    pub name: Option<String>,
    /// Use locally cached templates without network requests.
    pub offline: bool,
    /// Store stack configuration remotely.
    pub remote_stack_config: bool,
    /// Additional language-runtime options, as `key=value` strings.
    pub runtime_options: Vec<String>,
    pub secrets_provider: Option<String>,
    /// The stack name: an existing stack or one to create.
    pub stack: Option<String>,
    /// Skip prompting for AI or template functionality.
    pub template_mode: bool,
}

/// The result of [`LocalWorkspace::new_project`].
#[derive(Debug, Clone, Default)]
pub struct NewResult {
    pub stdout: String,
    pub stderr: String,
}

/// Options for `pulumi install`.
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Use language version tools (`pyenv`, `fnm`) to set up the runtime;
    /// requires CLI >= 3.130.0.
    pub use_language_version_tools: bool,
    pub no_plugins: bool,
    pub no_dependencies: bool,
    pub reinstall: bool,
}

/// Options for constructing a [`LocalWorkspace`]; unset fields take the
/// documented defaults.
#[derive(Clone, Default)]
pub struct LocalWorkspaceOptions {
    /// The project root. Defaults to a freshly created scratch directory,
    /// which suits inline programs.
    pub work_dir: Option<PathBuf>,
    /// `PULUMI_HOME` for every CLI invocation; defaults to the CLI's own
    /// default (`~/.pulumi`).
    pub pulumi_home: Option<PathBuf>,
    /// An inline program. When set, stack operations run this closure
    /// in-process instead of a program from `work_dir`.
    pub program: Option<ProgramFn>,
    /// Extra environment for every CLI invocation.
    pub env_vars: HashMap<String, String>,
    /// Passed to `pulumi stack init` as `--secrets-provider`.
    pub secrets_provider: Option<String>,
    /// Project settings to write into the workspace before first use.
    pub project_settings: Option<ProjectSettings>,
    /// Per-stack settings to write into the workspace before first use.
    pub stack_settings: HashMap<String, StackSettings>,
    /// Replaces the real CLI; used to inject a mock or a specific
    /// [`LocalPulumiCommand`].
    pub pulumi_command: Option<Arc<dyn PulumiCommand>>,
    /// Skip validating the CLI version. The
    /// `PULUMI_AUTOMATION_API_SKIP_VERSION_CHECK` environment variable
    /// (here or in the process environment) does the same.
    pub skip_version_check: bool,
}

impl std::fmt::Debug for LocalWorkspaceOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalWorkspaceOptions")
            .field("work_dir", &self.work_dir)
            .field("pulumi_home", &self.pulumi_home)
            .field("program", &self.program.as_ref().map(|_| "<program>"))
            .field("env_vars", &self.env_vars)
            .field("secrets_provider", &self.secrets_provider)
            .finish_non_exhaustive()
    }
}

/// A Pulumi workspace rooted in a local directory, driving the `pulumi`
/// CLI. The Rust analogue of the Go SDK's `auto.LocalWorkspace`.
///
/// Cloning is cheap and shares the CLI handle and any inline program;
/// mutations to environment variables after a clone do not propagate
/// between clones.
#[derive(Clone)]
pub struct LocalWorkspace {
    work_dir: PathBuf,
    pulumi_home: Option<PathBuf>,
    env_vars: HashMap<String, String>,
    secrets_provider: Option<String>,
    program: Option<ProgramFn>,
    command: SharedCommand,
}

impl std::fmt::Debug for LocalWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalWorkspace")
            .field("work_dir", &self.work_dir)
            .field("pulumi_home", &self.pulumi_home)
            .field("program", &self.program.as_ref().map(|_| "<program>"))
            .finish_non_exhaustive()
    }
}

/// The file extensions settings may live under; saves always use `.yaml`.
const SETTINGS_EXTENSIONS: [&str; 3] = ["yaml", "yml", "json"];

impl LocalWorkspace {
    /// Create a workspace from `options`.
    pub async fn new(options: LocalWorkspaceOptions) -> Result<Self> {
        let work_dir = match &options.work_dir {
            // An explicit work_dir must already exist, as in Node.
            Some(dir) if !dir.exists() => {
                return Err(Error::setup(format!(
                    "invalid work_dir passed to local workspace: '{}' does not exist",
                    dir.display()
                )))
            }
            Some(dir) if !dir.is_dir() => {
                return Err(Error::setup(format!(
                    "invalid work_dir passed to local workspace: '{}' is not a directory",
                    dir.display()
                )))
            }
            Some(dir) => dir.clone(),
            None => super::scratch_dir("pulumi-auto")?,
        };

        let command: SharedCommand = match options.pulumi_command {
            Some(command) => command,
            None => Arc::new(
                LocalPulumiCommand::new(PulumiCommandOptions {
                    skip_version_check: options.skip_version_check
                        || skip_version_check_from_env(&options.env_vars),
                    ..Default::default()
                })
                .await?,
            ),
        };

        let ws = LocalWorkspace {
            work_dir,
            pulumi_home: options.pulumi_home,
            env_vars: options.env_vars,
            secrets_provider: options.secrets_provider,
            program: options.program,
            command,
        };
        if let Some(project) = &options.project_settings {
            ws.save_project_settings(project)?;
        }
        for (stack, settings) in &options.stack_settings {
            ws.save_stack_settings(stack, settings)?;
        }
        Ok(ws)
    }

    /// The directory the CLI runs in.
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// The `PULUMI_HOME` override, if any.
    pub fn pulumi_home(&self) -> Option<&Path> {
        self.pulumi_home.as_deref()
    }

    /// The CLI version being driven.
    pub fn pulumi_version(&self) -> Version {
        self.command.version()
    }

    /// The inline program, if this workspace runs one.
    pub fn program(&self) -> Option<&ProgramFn> {
        self.program.as_ref()
    }

    /// Set or replace the inline program.
    pub fn set_program(&mut self, program: ProgramFn) {
        self.program = Some(program);
    }

    /// The extra environment applied to every CLI invocation.
    pub fn env_vars(&self) -> &HashMap<String, String> {
        &self.env_vars
    }

    pub fn set_env_var(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.env_vars.insert(key.into(), value.into());
    }

    pub fn unset_env_var(&mut self, key: &str) {
        self.env_vars.remove(key);
    }

    // ---- settings files ----

    fn find_settings_file(&self, base: &str) -> Option<PathBuf> {
        SETTINGS_EXTENSIONS
            .iter()
            .map(|ext| self.work_dir.join(format!("{base}.{ext}")))
            .find(|p| p.exists())
    }

    /// Load `Pulumi.yaml` (or `.yml`/`.json`).
    pub fn project_settings(&self) -> Result<ProjectSettings> {
        // A JSON project file parses fine as YAML: JSON is a YAML subset.
        let path = self
            .find_settings_file("Pulumi")
            .ok_or_else(|| Error::setup("unable to find project settings in workspace"))?;
        Ok(serde_yaml_ng::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// Whether the workspace has a project settings file at all.
    pub fn has_project_settings(&self) -> bool {
        self.find_settings_file("Pulumi").is_some()
    }

    /// Write `Pulumi.yaml`. Always saves YAML, whatever was loaded.
    pub fn save_project_settings(&self, settings: &ProjectSettings) -> Result<()> {
        let path = self.work_dir.join("Pulumi.yaml");
        std::fs::write(path, serde_yaml_ng::to_string(settings)?)?;
        Ok(())
    }

    /// The settings-file infix for a stack name: the last `/`-separated
    /// segment, so `org/proj/dev` and `dev` both map to `Pulumi.dev.yaml`.
    fn stack_settings_name(stack_name: &str) -> &str {
        stack_name.rsplit('/').next().unwrap_or(stack_name)
    }

    /// Load `Pulumi.<stack>.yaml` (or `.yml`/`.json`).
    pub fn stack_settings(&self, stack_name: &str) -> Result<StackSettings> {
        let base = format!("Pulumi.{}", Self::stack_settings_name(stack_name));
        let path = self.find_settings_file(&base).ok_or_else(|| {
            Error::setup(format!(
                "unable to find stack settings in workspace for {stack_name}"
            ))
        })?;
        Ok(serde_yaml_ng::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// Write `Pulumi.<stack>.yaml`.
    pub fn save_stack_settings(&self, stack_name: &str, settings: &StackSettings) -> Result<()> {
        let path = self.work_dir.join(format!(
            "Pulumi.{}.yaml",
            Self::stack_settings_name(stack_name)
        ));
        std::fs::write(path, serde_yaml_ng::to_string(settings)?)?;
        Ok(())
    }

    // ---- CLI plumbing ----

    /// The environment every invocation from this workspace carries.
    pub(crate) fn base_env(&self) -> Vec<(String, String)> {
        let mut env = vec![];
        if let Some(home) = &self.pulumi_home {
            env.push(("PULUMI_HOME".to_string(), home.display().to_string()));
        }
        for (k, v) in &self.env_vars {
            env.push((k.clone(), v.clone()));
        }
        env
    }

    pub(crate) fn command(&self) -> &SharedCommand {
        &self.command
    }

    /// Run `pulumi` in this workspace.
    pub(crate) async fn run_cmd(&self, args: Vec<String>) -> Result<CommandResult> {
        self.run_cmd_with_stdin(args, None).await
    }

    pub(crate) async fn run_cmd_with_stdin(
        &self,
        args: Vec<String>,
        stdin: Option<String>,
    ) -> Result<CommandResult> {
        self.command
            .run(CommandSpec {
                args,
                workdir: self.work_dir.clone(),
                env: self.base_env(),
                stdin,
            })
            .await
    }

    pub(crate) fn require_version(&self, major: u64, minor: u64, what: &str) -> Result<()> {
        let version = self.command.version();
        // 0.0.0 is the unparsable-version sentinel an explicit
        // PULUMI_AUTOMATION_API_SKIP_VERSION_CHECK stores; the skip
        // bypasses feature gates the way it bypasses the minimum check.
        if version == Version::new(0, 0, 0) {
            return Ok(());
        }
        let minimum = Version::new(major, minor, 0);
        if version < minimum {
            return Err(Error::setup(format!(
                "{what} requires Pulumi CLI version >= {minimum}"
            )));
        }
        Ok(())
    }

    // ---- stack lifecycle ----

    /// `pulumi stack init`.
    pub async fn create_stack(&self, stack_name: &str) -> Result<()> {
        let mut args = svec(["stack", "init", stack_name]);
        if let Some(provider) = &self.secrets_provider {
            args.push("--secrets-provider".to_string());
            args.push(provider.clone());
        }
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to create stack"))?;
        Ok(())
    }

    /// `pulumi stack select`.
    pub async fn select_stack(&self, stack_name: &str) -> Result<()> {
        let args = svec(["stack", "select", "--stack", stack_name]);
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to select stack"))?;
        Ok(())
    }

    /// `pulumi stack rm --yes`.
    pub async fn remove_stack(&self, stack_name: &str, force: bool) -> Result<()> {
        let mut args = svec(["stack", "rm", "--yes", stack_name]);
        if force {
            args.push("--force".to_string());
        }
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to remove stack"))?;
        Ok(())
    }

    /// `pulumi stack ls --json`.
    pub async fn list_stacks(&self) -> Result<Vec<StackSummary>> {
        self.list_stacks_with_options(&ListOptions::default()).await
    }

    /// `pulumi stack ls --json`, with `--all` when asked for.
    pub async fn list_stacks_with_options(
        &self,
        options: &ListOptions,
    ) -> Result<Vec<StackSummary>> {
        let mut args = svec(["stack", "ls", "--json"]);
        if options.all {
            args.push("--all".to_string());
        }
        let result = self
            .run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to list stacks"))?;
        Ok(serde_json::from_str(&result.stdout)?)
    }

    /// The currently selected stack, if any.
    pub async fn stack(&self) -> Result<Option<StackSummary>> {
        Ok(self.list_stacks().await?.into_iter().find(|s| s.current))
    }

    /// `pulumi whoami`, with detail when the CLI supports `--json`.
    pub async fn whoami(&self) -> Result<WhoAmIResult> {
        if self.command.version() >= Version::new(3, 58, 0) {
            let result = self
                .run_cmd(svec(["whoami", "--json"]))
                .await
                .map_err(|e| e.with_context("failed to get current user"))?;
            Ok(serde_json::from_str(&result.stdout)?)
        } else {
            let result = self
                .run_cmd(svec(["whoami"]))
                .await
                .map_err(|e| e.with_context("failed to get current user"))?;
            Ok(WhoAmIResult {
                user: result.stdout.trim().to_string(),
                ..Default::default()
            })
        }
    }

    // ---- config ----

    /// `pulumi config get <key> --json`.
    pub async fn get_config(
        &self,
        stack_name: &str,
        key: &str,
        options: &ConfigOptions,
    ) -> Result<ConfigValue> {
        let mut args = svec(["config", "get"]);
        push_config_options(&mut args, options);
        args.extend(svec([key, "--json", "--stack", stack_name]));
        let result = self
            .run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to get config"))?;
        Ok(serde_json::from_str(&result.stdout)?)
    }

    /// `pulumi config --show-secrets --json`: the full config map.
    pub async fn get_all_config(&self, stack_name: &str) -> Result<ConfigMap> {
        let args = svec(["config", "--show-secrets", "--json", "--stack", stack_name]);
        let result = self
            .run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to get config"))?;
        Ok(serde_json::from_str(&result.stdout)?)
    }

    /// `pulumi config set`. The value travels after a `--` separator so a
    /// value beginning with a dash cannot read as a flag.
    pub async fn set_config(
        &self,
        stack_name: &str,
        key: &str,
        value: &ConfigValue,
        options: &ConfigOptions,
    ) -> Result<()> {
        let mut args = svec(["config", "set", "--stack", stack_name]);
        push_config_options(&mut args, options);
        args.push(key.to_string());
        args.push(secrecy_flag(value).to_string());
        args.push("--".to_string());
        args.push(value.value.clone());
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to set config"))?;
        Ok(())
    }

    /// `pulumi config set-all`, one `--secret`/`--plaintext` `key=value`
    /// pair per entry.
    pub async fn set_all_config(
        &self,
        stack_name: &str,
        config: &ConfigMap,
        options: &ConfigOptions,
    ) -> Result<()> {
        let mut args = svec(["config", "set-all", "--stack", stack_name]);
        push_config_options(&mut args, options);
        for (key, value) in config {
            args.push(secrecy_flag(value).to_string());
            args.push(format!("{key}={}", value.value));
        }
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to set config"))?;
        Ok(())
    }

    /// `pulumi config set-all --json`: the whole map in one JSON string,
    /// in the format `pulumi config --json` produces. Only the
    /// `config_file` option applies; the CLI rejects `--path` with
    /// `--json`.
    pub async fn set_all_config_json(
        &self,
        stack_name: &str,
        config_json: &str,
        options: &ConfigOptions,
    ) -> Result<()> {
        let mut args = svec(["config", "set-all", "--stack", stack_name, "--json"]);
        args.push(config_json.to_string());
        if let Some(file) = &options.config_file {
            args.push("--config-file".to_string());
            args.push(file.display().to_string());
        }
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to set config from JSON"))?;
        Ok(())
    }

    /// `pulumi config rm`.
    pub async fn remove_config(
        &self,
        stack_name: &str,
        key: &str,
        options: &ConfigOptions,
    ) -> Result<()> {
        let mut args = svec(["config", "rm"]);
        push_config_options(&mut args, options);
        args.extend(svec([key, "--stack", stack_name]));
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to remove config"))?;
        Ok(())
    }

    /// `pulumi config rm-all`.
    pub async fn remove_all_config(
        &self,
        stack_name: &str,
        keys: &[&str],
        options: &ConfigOptions,
    ) -> Result<()> {
        let mut args = svec(["config", "rm-all", "--stack", stack_name]);
        push_config_options(&mut args, options);
        args.extend(keys.iter().map(|k| k.to_string()));
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to remove config"))?;
        Ok(())
    }

    /// `pulumi config refresh --force`, then the refreshed map.
    pub async fn refresh_config(&self, stack_name: &str) -> Result<ConfigMap> {
        let args = svec(["config", "refresh", "--force", "--stack", stack_name]);
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to refresh config"))?;
        self.get_all_config(stack_name).await
    }

    // ---- tags ----

    /// `pulumi stack tag get`.
    pub async fn get_tag(&self, stack_name: &str, key: &str) -> Result<String> {
        let args = svec(["stack", "tag", "get", key, "--stack", stack_name]);
        let result = self
            .run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to get tag"))?;
        Ok(result.stdout.trim().to_string())
    }

    /// `pulumi stack tag set`.
    pub async fn set_tag(&self, stack_name: &str, key: &str, value: &str) -> Result<()> {
        let args = svec(["stack", "tag", "set", key, value, "--stack", stack_name]);
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to set tag"))?;
        Ok(())
    }

    /// `pulumi stack tag rm`.
    pub async fn remove_tag(&self, stack_name: &str, key: &str) -> Result<()> {
        let args = svec(["stack", "tag", "rm", key, "--stack", stack_name]);
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to remove tag"))?;
        Ok(())
    }

    /// `pulumi stack tag ls --json`.
    pub async fn list_tags(&self, stack_name: &str) -> Result<HashMap<String, String>> {
        let args = svec(["stack", "tag", "ls", "--json", "--stack", stack_name]);
        let result = self
            .run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to list tags"))?;
        Ok(serde_json::from_str(&result.stdout)?)
    }

    // ---- environments ----

    /// `pulumi config env add`; requires CLI >= 3.95.0.
    pub async fn add_environments(&self, stack_name: &str, environments: &[&str]) -> Result<()> {
        self.require_version(3, 95, "AddEnvironments")?;
        let mut args = svec(["config", "env", "add"]);
        args.extend(environments.iter().map(|e| e.to_string()));
        args.extend(svec(["--yes", "--stack", stack_name]));
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to add environments"))?;
        Ok(())
    }

    /// `pulumi config env ls --json`; requires CLI >= 3.99.0.
    pub async fn list_environments(&self, stack_name: &str) -> Result<Vec<String>> {
        self.require_version(3, 99, "ListEnvironments")?;
        let args = svec(["config", "env", "ls", "--stack", stack_name, "--json"]);
        let result = self
            .run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to list environments"))?;
        Ok(serde_json::from_str(&result.stdout)?)
    }

    /// `pulumi config env rm`; requires CLI >= 3.95.0.
    pub async fn remove_environment(&self, stack_name: &str, environment: &str) -> Result<()> {
        self.require_version(3, 95, "RemoveEnvironment")?;
        let args = svec([
            "config",
            "env",
            "rm",
            environment,
            "--yes",
            "--stack",
            stack_name,
        ]);
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to remove environment"))?;
        Ok(())
    }

    // ---- plugins ----

    /// `pulumi plugin install resource`.
    pub async fn install_plugin(&self, name: &str, version: &str) -> Result<()> {
        let args = svec(["plugin", "install", "resource", name, version]);
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to install plugin"))?;
        Ok(())
    }

    /// `pulumi plugin rm resource --yes`.
    pub async fn remove_plugin(&self, name: &str, version: &str) -> Result<()> {
        let args = svec(["plugin", "rm", "resource", name, version, "--yes"]);
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to remove plugin"))?;
        Ok(())
    }

    /// `pulumi plugin ls --json`.
    pub async fn list_plugins(&self) -> Result<Vec<PluginInfo>> {
        let result = self
            .run_cmd(svec(["plugin", "ls", "--json"]))
            .await
            .map_err(|e| e.with_context("failed to list plugins"))?;
        Ok(serde_json::from_str(&result.stdout)?)
    }

    /// `pulumi install`: restore the project's dependencies and plugins;
    /// requires CLI >= 3.91.0.
    pub async fn install(&self, options: &InstallOptions) -> Result<()> {
        self.require_version(3, 91, "Install")?;
        let mut args = svec(["install"]);
        if options.use_language_version_tools {
            self.require_version(3, 130, "InstallOptions.use_language_version_tools")?;
            args.push("--use-language-version-tools".to_string());
        }
        if options.no_plugins {
            args.push("--no-plugins".to_string());
        }
        if options.no_dependencies {
            args.push("--no-dependencies".to_string());
        }
        if options.reinstall {
            args.push("--reinstall".to_string());
        }
        self.run_cmd(args)
            .await
            .map_err(|e| e.with_context("failed to install"))?;
        Ok(())
    }

    /// `pulumi new`: create a project (and optionally a stack) from a
    /// template. Flags follow Go's serialization: `--yes` always, options
    /// in flag order, and the template as a positional after `--`.
    pub async fn new_project(&self, options: &NewOptions) -> Result<NewResult> {
        let mut args = svec(["new", "--yes"]);
        if let Some(ai) = &options.ai {
            args.extend(svec(["--ai", ai]));
        }
        for config in &options.config {
            args.extend(svec(["--config", config]));
        }
        if options.config_path {
            args.push("--config-path".to_string());
        }
        if let Some(description) = &options.description {
            args.extend(svec(["--description", description]));
        }
        if let Some(dir) = &options.dir {
            args.push("--dir".to_string());
            args.push(dir.display().to_string());
        }
        if options.force {
            args.push("--force".to_string());
        }
        if options.generate_only {
            args.push("--generate-only".to_string());
        }
        if let Some(language) = &options.language {
            args.extend(svec(["--language", language]));
        }
        if options.list_templates {
            args.push("--list-templates".to_string());
        }
        if let Some(name) = &options.name {
            args.extend(svec(["--name", name]));
        }
        if options.offline {
            args.push("--offline".to_string());
        }
        if options.remote_stack_config {
            args.push("--remote-stack-config".to_string());
        }
        for runtime_option in &options.runtime_options {
            args.extend(svec(["--runtime-options", runtime_option]));
        }
        if let Some(provider) = &options.secrets_provider {
            args.extend(svec(["--secrets-provider", provider]));
        }
        if let Some(stack) = &options.stack {
            args.extend(svec(["--stack", stack]));
        }
        if options.template_mode {
            args.push("--template-mode".to_string());
        }
        if let Some(template) = &options.template_or_url {
            args.push("--".to_string());
            args.push(template.clone());
        }
        let result = self
            .run_cmd(args)
            .await
            .map_err(|e| e.with_context("could not create new project"))?;
        Ok(NewResult {
            stdout: result.stdout,
            stderr: result.stderr,
        })
    }

    // ---- state ----

    /// `pulumi stack output --json`, twice: once masked to learn which
    /// outputs are secret, once with `--show-secrets` for the values.
    pub async fn stack_outputs(&self, stack_name: &str) -> Result<OutputMap> {
        let masked = self
            .run_cmd(svec(["stack", "output", "--json", "--stack", stack_name]))
            .await
            .map_err(|e| e.with_context("could not get outputs"))?;
        let shown = self
            .run_cmd(svec([
                "stack",
                "output",
                "--json",
                "--show-secrets",
                "--stack",
                stack_name,
            ]))
            .await
            .map_err(|e| e.with_context("could not get secret outputs"))?;

        let masked: serde_json::Map<String, Value> = serde_json::from_str(&masked.stdout)?;
        let shown: serde_json::Map<String, Value> = serde_json::from_str(&shown.stdout)?;

        let mut outputs = OutputMap::new();
        for (key, value) in shown {
            // The masked run replaces secret material (however deeply
            // nested) with a sentinel; its presence marks the output secret.
            let secret = masked
                .get(&key)
                .map(|masked_value| {
                    serde_json::to_string(masked_value)
                        .unwrap_or_default()
                        .contains("[secret]")
                })
                .unwrap_or(false);
            outputs.insert(key, OutputValue { value, secret });
        }
        Ok(outputs)
    }

    /// `pulumi stack export --show-secrets`.
    pub async fn export_stack(&self, stack_name: &str) -> Result<StackDeployment> {
        let args = svec(["stack", "export", "--show-secrets", "--stack", stack_name]);
        let result = self
            .run_cmd(args)
            .await
            .map_err(|e| e.with_context("could not export stack"))?;
        Ok(serde_json::from_str(&result.stdout)?)
    }

    /// `pulumi stack import --file <state>`.
    pub async fn import_stack(&self, stack_name: &str, state: &StackDeployment) -> Result<()> {
        let dir = super::scratch_dir("pulumi-auto-import")?;
        let file = dir.join("deployment.json");
        std::fs::write(&file, serde_json::to_vec(state)?)?;
        let args = svec([
            "stack",
            "import",
            "--file",
            &file.display().to_string(),
            "--stack",
            stack_name,
        ]);
        let result = self.run_cmd(args).await;
        let _ = std::fs::remove_dir_all(&dir);
        result.map_err(|e| e.with_context("could not import stack"))?;
        Ok(())
    }

    /// `pulumi stack change-secrets-provider`. A passphrase provider needs
    /// the new passphrase, which travels via stdin.
    pub async fn change_stack_secrets_provider(
        &self,
        stack_name: &str,
        new_secrets_provider: &str,
        new_passphrase: Option<&str>,
    ) -> Result<()> {
        let stdin = if new_secrets_provider == "passphrase" {
            let passphrase =
                new_passphrase.ok_or_else(|| Error::setup("new passphrase must be provided"))?;
            Some(passphrase.to_string())
        } else {
            None
        };
        let args = svec([
            "stack",
            "change-secrets-provider",
            "--stack",
            stack_name,
            new_secrets_provider,
        ]);
        self.run_cmd_with_stdin(args, stdin)
            .await
            .map_err(|e| e.with_context("failed to change secrets provider"))?;
        Ok(())
    }
}

fn secrecy_flag(value: &ConfigValue) -> &'static str {
    if value.secret {
        "--secret"
    } else {
        "--plaintext"
    }
}

fn push_config_options(args: &mut Vec<String>, options: &ConfigOptions) {
    if options.path {
        args.push("--path".to_string());
    }
    if let Some(file) = &options.config_file {
        args.push("--config-file".to_string());
        args.push(file.display().to_string());
    }
}

/// Build a `Vec<String>` from string literals.
pub(crate) fn svec<const N: usize>(items: [&str; N]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::super::cmd::test_support::RecordingCommand;
    use super::*;

    async fn recording_workspace() -> (Arc<RecordingCommand>, LocalWorkspace) {
        let recorder = Arc::new(RecordingCommand::default());
        let ws = LocalWorkspace::new(LocalWorkspaceOptions {
            pulumi_command: Some(recorder.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
        (recorder, ws)
    }

    #[tokio::test]
    async fn config_set_places_value_after_separator() {
        let (recorder, ws) = recording_workspace().await;
        ws.set_config(
            "dev",
            "aws:region",
            &ConfigValue::secret("--us-west-2"),
            &ConfigOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec([
                "config",
                "set",
                "--stack",
                "dev",
                "aws:region",
                "--secret",
                "--",
                "--us-west-2"
            ])
        );
    }

    #[tokio::test]
    async fn config_options_use_two_token_config_file() {
        let (recorder, ws) = recording_workspace().await;
        ws.get_config(
            "dev",
            "k",
            &ConfigOptions {
                path: true,
                config_file: Some(PathBuf::from("cfg.yaml")),
            },
        )
        .await
        .unwrap_err(); // empty stdout is not valid JSON; args still recorded
        assert_eq!(
            recorder.recorded_args()[0],
            svec([
                "config",
                "get",
                "--path",
                "--config-file",
                "cfg.yaml",
                "k",
                "--json",
                "--stack",
                "dev"
            ])
        );
    }

    #[tokio::test]
    async fn set_all_config_flags_each_pair() {
        let (recorder, ws) = recording_workspace().await;
        let mut config = ConfigMap::new();
        config.insert("ns:plain".to_string(), ConfigValue::plain("a"));
        config.insert("ns:secret".to_string(), ConfigValue::secret("b"));
        ws.set_all_config("dev", &config, &ConfigOptions::default())
            .await
            .unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec([
                "config",
                "set-all",
                "--stack",
                "dev",
                "--plaintext",
                "ns:plain=a",
                "--secret",
                "ns:secret=b"
            ])
        );
    }

    #[tokio::test]
    async fn set_all_config_json_places_json_after_flag() {
        let (recorder, ws) = recording_workspace().await;
        let json = r#"{"ns:plain":{"value":"a","secret":false}}"#;
        ws.set_all_config_json("dev", json, &ConfigOptions::default())
            .await
            .unwrap();
        ws.set_all_config_json(
            "dev",
            json,
            &ConfigOptions {
                config_file: Some(PathBuf::from("cfg.yaml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let args = recorder.recorded_args();
        assert_eq!(
            args[0],
            svec(["config", "set-all", "--stack", "dev", "--json", json])
        );
        assert_eq!(
            args[1],
            svec([
                "config",
                "set-all",
                "--stack",
                "dev",
                "--json",
                json,
                "--config-file",
                "cfg.yaml"
            ])
        );
    }

    /// Ports go:TestNewOptions: argv per option, alone and combined.
    #[tokio::test]
    async fn new_project_maps_each_option_to_its_flag() {
        let (recorder, ws) = recording_workspace().await;

        // No options: no `--` separator without a positional template.
        ws.new_project(&NewOptions::default()).await.unwrap();

        // The template travels after a `--` separator.
        ws.new_project(&NewOptions {
            template_or_url: Some("typescript".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

        // Name and generate-only; no template, so no `--`.
        ws.new_project(&NewOptions {
            name: Some("my-project".to_string()),
            generate_only: true,
            force: true,
            ..Default::default()
        })
        .await
        .unwrap();

        // Config values and template.
        ws.new_project(&NewOptions {
            template_or_url: Some("aws-typescript".to_string()),
            config: vec![
                "aws:region=us-east-1".to_string(),
                "project:env=dev".to_string(),
            ],
            config_path: true,
            description: Some("A test project".to_string()),
            stack: Some("dev".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

        // Every boolean flag at once.
        ws.new_project(&NewOptions {
            template_or_url: Some("yaml".to_string()),
            config_path: true,
            force: true,
            generate_only: true,
            list_templates: true,
            offline: true,
            remote_stack_config: true,
            template_mode: true,
            ..Default::default()
        })
        .await
        .unwrap();

        let args = recorder.recorded_args();
        assert_eq!(args[0], svec(["new", "--yes"]));
        assert_eq!(args[1], svec(["new", "--yes", "--", "typescript"]));
        assert_eq!(
            args[2],
            svec([
                "new",
                "--yes",
                "--force",
                "--generate-only",
                "--name",
                "my-project"
            ])
        );
        assert_eq!(
            args[3],
            svec([
                "new",
                "--yes",
                "--config",
                "aws:region=us-east-1",
                "--config",
                "project:env=dev",
                "--config-path",
                "--description",
                "A test project",
                "--stack",
                "dev",
                "--",
                "aws-typescript"
            ])
        );
        assert_eq!(
            args[4],
            svec([
                "new",
                "--yes",
                "--config-path",
                "--force",
                "--generate-only",
                "--list-templates",
                "--offline",
                "--remote-stack-config",
                "--template-mode",
                "--",
                "yaml"
            ])
        );
    }

    #[tokio::test]
    async fn list_stacks_all_appends_the_all_flag() {
        let (recorder, ws) = recording_workspace().await;
        recorder.push_result(Ok(CommandResult {
            stdout: "[]".to_string(),
            ..Default::default()
        }));
        ws.list_stacks_with_options(&ListOptions { all: true })
            .await
            .unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec(["stack", "ls", "--json", "--all"])
        );
    }

    #[tokio::test]
    async fn create_stack_passes_secrets_provider() {
        let recorder = Arc::new(RecordingCommand::default());
        let ws = LocalWorkspace::new(LocalWorkspaceOptions {
            pulumi_command: Some(recorder.clone()),
            secrets_provider: Some("passphrase".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        ws.create_stack("dev").await.unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec(["stack", "init", "dev", "--secrets-provider", "passphrase"])
        );
    }

    #[tokio::test]
    async fn remove_stack_orders_force_after_name() {
        let (recorder, ws) = recording_workspace().await;
        ws.remove_stack("dev", true).await.unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec(["stack", "rm", "--yes", "dev", "--force"])
        );
    }

    #[tokio::test]
    async fn stack_outputs_marks_secrets_via_sentinel() {
        let (recorder, ws) = recording_workspace().await;
        recorder.push_result(Ok(CommandResult {
            stdout: r#"{"plain":"v","hidden":"[secret]"}"#.to_string(),
            ..Default::default()
        }));
        recorder.push_result(Ok(CommandResult {
            stdout: r#"{"plain":"v","hidden":"sensitive"}"#.to_string(),
            ..Default::default()
        }));
        let outputs = ws.stack_outputs("dev").await.unwrap();
        assert!(!outputs["plain"].secret);
        assert!(outputs["hidden"].secret);
        assert_eq!(outputs["hidden"].value, serde_json::json!("sensitive"));
    }

    #[test]
    fn stack_settings_name_takes_last_segment() {
        assert_eq!(LocalWorkspace::stack_settings_name("dev"), "dev");
        assert_eq!(LocalWorkspace::stack_settings_name("org/dev"), "dev");
        assert_eq!(LocalWorkspace::stack_settings_name("org/proj/dev"), "dev");
    }

    #[test]
    fn project_settings_round_trip_preserves_unknown_keys() {
        let yaml = "name: demo\nruntime: rust\ndescription: a demo\nbackend:\n  url: file://~\n";
        let settings: ProjectSettings = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(settings.name, "demo");
        assert_eq!(settings.runtime.as_ref().unwrap().name(), "rust");
        let out = serde_yaml_ng::to_string(&settings).unwrap();
        assert!(out.contains("url: file://~"), "kept unknown keys: {out}");
    }

    #[test]
    fn runtime_info_parses_both_forms() {
        let s: ProjectSettings = serde_yaml_ng::from_str(
            "name: a\nruntime:\n  name: nodejs\n  options:\n    binary: x\n",
        )
        .unwrap();
        assert_eq!(s.runtime.as_ref().unwrap().name(), "nodejs");
        let s: ProjectSettings = serde_yaml_ng::from_str("name: a\nruntime: rust\n").unwrap();
        assert_eq!(s.runtime.as_ref().unwrap().name(), "rust");
    }

    /// A project file without a runtime key is accepted and round trips
    /// without inventing one, as in Node and Python.
    #[test]
    fn project_settings_permit_a_missing_runtime() {
        let settings: ProjectSettings = serde_yaml_ng::from_str("name: demo\n").unwrap();
        assert!(settings.runtime.is_none());
        let out = serde_yaml_ng::to_string(&settings).unwrap();
        assert!(!out.contains("runtime"), "no runtime key: {out}");
        let reloaded: ProjectSettings = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(reloaded.name, "demo");
        assert!(reloaded.runtime.is_none());
    }

    #[test]
    fn config_value_tolerates_a_hidden_secret() {
        // `stack history --json` without --show-secrets omits the value key.
        let v: ConfigValue = serde_json::from_str(r#"{"secret":true}"#).unwrap();
        assert!(v.secret);
        assert_eq!(v.value, "");
    }

    #[test]
    fn only_an_exact_secure_mapping_is_a_secret() {
        // An object that merely contains a `secure` key is plaintext, and
        // its sibling keys survive.
        let yaml = "config:\n  ns:obj:\n    a: 1\n    secure: sneaky\n";
        let settings: StackSettings = serde_yaml_ng::from_str(yaml).unwrap();
        let config = settings.config.as_ref().unwrap();
        match &config["ns:obj"] {
            StackSettingsConfigValue::Plain(value) => {
                let out = serde_yaml_ng::to_string(value).unwrap();
                assert!(out.contains("a: 1"), "kept siblings: {out}");
            }
            other => panic!("expected plain, got {other:?}"),
        }
        // A non-string `secure` value is also not a secret.
        let yaml = "config:\n  ns:obj:\n    secure: 42\n";
        let settings: StackSettings = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(matches!(
            settings.config.as_ref().unwrap()["ns:obj"],
            StackSettingsConfigValue::Plain(_)
        ));
    }

    #[test]
    fn stack_settings_secure_values_round_trip() {
        let yaml = "secretsprovider: passphrase\nconfig:\n  ns:plain: hello\n  ns:secret:\n    secure: AAAbbb==\n";
        let settings: StackSettings = serde_yaml_ng::from_str(yaml).unwrap();
        let config = settings.config.as_ref().unwrap();
        assert!(matches!(
            config["ns:secret"],
            StackSettingsConfigValue::Secure { .. }
        ));
        assert!(matches!(
            config["ns:plain"],
            StackSettingsConfigValue::Plain(_)
        ));
        let out = serde_yaml_ng::to_string(&settings).unwrap();
        assert!(out.contains("secure: AAAbbb=="));
    }

    /// A scratch directory removed on drop, so fixture files never leak.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "pulumi-rust-unit-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn workspace_in(dir: &Path) -> LocalWorkspace {
        LocalWorkspace::new(LocalWorkspaceOptions {
            work_dir: Some(dir.to_path_buf()),
            pulumi_command: Some(Arc::new(RecordingCommand::default())),
            ..Default::default()
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn get_all_config_uses_exact_args() {
        let (recorder, ws) = recording_workspace().await;
        recorder.push_result(Ok(CommandResult {
            stdout: "{}".to_string(),
            ..Default::default()
        }));
        ws.get_all_config("dev").await.unwrap();
        assert_eq!(
            recorder.recorded_args()[0],
            svec(["config", "--show-secrets", "--json", "--stack", "dev"])
        );
    }

    #[tokio::test]
    async fn list_stacks_uses_exact_args_and_parses_summaries() {
        let (recorder, ws) = recording_workspace().await;
        recorder.push_result(Ok(CommandResult {
            stdout: r#"[{"name":"dev","current":true,"url":"https://app.pulumi.com/org/proj/dev"},{"name":"prod","current":false,"lastUpdate":"2026-08-18T00:00:00.000Z","resourceCount":3}]"#.to_string(),
            ..Default::default()
        }));
        let stacks = ws.list_stacks().await.unwrap();
        assert_eq!(recorder.recorded_args()[0], svec(["stack", "ls", "--json"]));
        assert_eq!(stacks.len(), 2);
        assert_eq!(stacks[0].name, "dev");
        assert!(stacks[0].current);
        assert_eq!(
            stacks[0].url.as_deref(),
            Some("https://app.pulumi.com/org/proj/dev")
        );
        assert_eq!(stacks[1].name, "prod");
        assert!(!stacks[1].current);
        assert_eq!(stacks[1].url, None);
    }

    #[tokio::test]
    async fn project_settings_loads_all_three_extensions() {
        let cases = [
            (
                "yaml",
                "name: proj-yaml\nruntime: nodejs\ndescription: from yaml\n",
                ("proj-yaml", "nodejs", "from yaml"),
            ),
            (
                "yml",
                "name: proj-yml\nruntime: python\ndescription: from yml\n",
                ("proj-yml", "python", "from yml"),
            ),
            (
                "json",
                r#"{"name":"proj-json","runtime":"go","description":"from json"}"#,
                ("proj-json", "go", "from json"),
            ),
        ];
        for (ext, content, (name, runtime, description)) in cases {
            let dir = TempDir::new("project-settings");
            std::fs::write(dir.path().join(format!("Pulumi.{ext}")), content).unwrap();
            let ws = workspace_in(dir.path()).await;
            let settings = ws.project_settings().unwrap();
            assert_eq!(settings.name, name, "extension {ext}");
            assert_eq!(
                settings.runtime.as_ref().unwrap().name(),
                runtime,
                "extension {ext}"
            );
            assert_eq!(
                settings.description.as_deref(),
                Some(description),
                "extension {ext}"
            );
        }
    }

    #[tokio::test]
    async fn project_settings_main_round_trips_verbatim() {
        let dir = TempDir::new("main-round-trip");
        let ws = workspace_in(dir.path()).await;
        for main in [None, Some(String::new()), Some("src".to_string())] {
            let mut settings = ProjectSettings::new("demo", "rust");
            settings.main = main.clone();
            ws.save_project_settings(&settings).unwrap();
            assert_eq!(ws.project_settings().unwrap().main, main);
        }
    }

    #[tokio::test]
    async fn stack_settings_loads_all_three_extensions() {
        let yaml =
            "secretsprovider: passphrase\nconfig:\n  proj:plain: hello\n  proj:secret:\n    secure: v1:cipher==\n";
        let json = r#"{"secretsprovider":"passphrase","config":{"proj:plain":"hello","proj:secret":{"secure":"v1:cipher=="}}}"#;
        for (ext, content) in [("yaml", yaml), ("yml", yaml), ("json", json)] {
            let dir = TempDir::new("stack-settings");
            std::fs::write(dir.path().join(format!("Pulumi.dev.{ext}")), content).unwrap();
            let ws = workspace_in(dir.path()).await;
            let settings = ws.stack_settings("dev").unwrap();
            assert_eq!(
                settings.secrets_provider.as_deref(),
                Some("passphrase"),
                "extension {ext}"
            );
            let config = settings.config.as_ref().unwrap();
            assert!(
                matches!(
                    &config["proj:plain"],
                    StackSettingsConfigValue::Plain(v)
                        if v == &serde_yaml_ng::Value::String("hello".to_string())
                ),
                "extension {ext}"
            );
            assert!(
                matches!(
                    &config["proj:secret"],
                    StackSettingsConfigValue::Secure { secure } if secure == "v1:cipher=="
                ),
                "extension {ext}"
            );
        }
    }

    #[tokio::test]
    async fn workspace_new_reports_uncreatable_work_dir_as_setup_error() {
        // A path routed through a regular file cannot exist.
        let dir = TempDir::new("bad-work-dir");
        let file = dir.path().join("plain-file");
        std::fs::write(&file, "not a directory").unwrap();
        let err = LocalWorkspace::new(LocalWorkspaceOptions {
            work_dir: Some(file.join("nested")),
            pulumi_command: Some(Arc::new(RecordingCommand::default())),
            ..Default::default()
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Setup(_)), "unexpected error: {err}");
        assert!(err.to_string().contains("does not exist"));
    }

    /// A nonexistent explicit work_dir is rejected rather than created,
    /// as in Node; only the generated scratch directory is created.
    #[tokio::test]
    async fn workspace_new_rejects_a_missing_work_dir() {
        let dir = TempDir::new("missing-work-dir");
        let missing = dir.path().join("nested").join("deeper");
        let err = LocalWorkspace::new(LocalWorkspaceOptions {
            work_dir: Some(missing.clone()),
            pulumi_command: Some(Arc::new(RecordingCommand::default())),
            ..Default::default()
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Setup(_)), "unexpected error: {err}");
        assert!(err.to_string().contains("does not exist"), "error: {err}");
        assert!(!missing.exists(), "work_dir must not be created");
    }

    #[tokio::test]
    async fn workspace_new_rejects_a_work_dir_that_is_a_file() {
        let dir = TempDir::new("file-work-dir");
        let file = dir.path().join("plain-file");
        std::fs::write(&file, "not a directory").unwrap();
        let err = LocalWorkspace::new(LocalWorkspaceOptions {
            work_dir: Some(file),
            pulumi_command: Some(Arc::new(RecordingCommand::default())),
            ..Default::default()
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Setup(_)), "unexpected error: {err}");
        assert!(
            err.to_string().contains("is not a directory"),
            "error: {err}"
        );
    }

    #[tokio::test]
    async fn install_maps_each_option_to_its_flag() {
        let (recorder, ws) = recording_workspace().await;
        ws.install(&InstallOptions::default()).await.unwrap();
        ws.install(&InstallOptions {
            use_language_version_tools: true,
            ..Default::default()
        })
        .await
        .unwrap();
        ws.install(&InstallOptions {
            no_plugins: true,
            ..Default::default()
        })
        .await
        .unwrap();
        ws.install(&InstallOptions {
            no_dependencies: true,
            ..Default::default()
        })
        .await
        .unwrap();
        ws.install(&InstallOptions {
            reinstall: true,
            ..Default::default()
        })
        .await
        .unwrap();
        ws.install(&InstallOptions {
            use_language_version_tools: true,
            no_plugins: true,
            no_dependencies: true,
            reinstall: true,
        })
        .await
        .unwrap();

        let args = recorder.recorded_args();
        assert_eq!(args[0], svec(["install"]));
        assert_eq!(args[1], svec(["install", "--use-language-version-tools"]));
        assert_eq!(args[2], svec(["install", "--no-plugins"]));
        assert_eq!(args[3], svec(["install", "--no-dependencies"]));
        assert_eq!(args[4], svec(["install", "--reinstall"]));
        assert_eq!(
            args[5],
            svec([
                "install",
                "--use-language-version-tools",
                "--no-plugins",
                "--no-dependencies",
                "--reinstall"
            ])
        );
    }

    #[tokio::test]
    async fn install_requires_cli_3_91() {
        let recorder = Arc::new(RecordingCommand {
            version: Version::new(3, 90, 0),
            ..Default::default()
        });
        let ws = LocalWorkspace::new(LocalWorkspaceOptions {
            pulumi_command: Some(recorder.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
        let err = ws.install(&InstallOptions::default()).await.unwrap_err();
        assert!(err.to_string().contains(">= 3.91.0"), "unexpected: {err}");
        assert!(recorder.recorded_args().is_empty(), "no CLI run expected");
    }

    /// The 0.0.0 sentinel a skipped, unparsable version check stores must
    /// pass the feature gates: the skip is an explicit opt-out.
    #[tokio::test]
    async fn skipped_version_check_bypasses_feature_gates() {
        let recorder = Arc::new(RecordingCommand {
            version: Version::new(0, 0, 0),
            ..Default::default()
        });
        let ws = LocalWorkspace::new(LocalWorkspaceOptions {
            pulumi_command: Some(recorder.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
        ws.install(&InstallOptions::default()).await.unwrap();
        assert_eq!(recorder.recorded_args()[0][0], "install");
    }

    #[tokio::test]
    async fn install_language_version_tools_requires_cli_3_130() {
        let recorder = Arc::new(RecordingCommand {
            version: Version::new(3, 129, 0),
            ..Default::default()
        });
        let ws = LocalWorkspace::new(LocalWorkspaceOptions {
            pulumi_command: Some(recorder.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
        let err = ws
            .install(&InstallOptions {
                use_language_version_tools: true,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains(">= 3.130.0"), "unexpected: {err}");
        assert!(recorder.recorded_args().is_empty(), "no CLI run expected");
    }
}
