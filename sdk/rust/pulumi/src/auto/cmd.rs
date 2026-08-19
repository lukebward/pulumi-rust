//! Running the `pulumi` CLI: locating the binary, validating its version,
//! and executing commands with the environment the automation API requires.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use futures::future::BoxFuture;
use semver::Version;

use super::errors::{CommandResult, Error, Result, UNKNOWN_ERR_CODE};

/// The oldest CLI the automation API will drive, matching the Go SDK's
/// `minimumVersion`.
const MINIMUM_VERSION: Version = Version::new(3, 2, 0);

/// Opting out of the version check, shared with every other Pulumi SDK.
pub const SKIP_VERSION_CHECK_VAR: &str = "PULUMI_AUTOMATION_API_SKIP_VERSION_CHECK";

/// One `pulumi` invocation, fully described. What a [`PulumiCommand`]
/// implementation receives; assembled by the workspace and stack layers.
#[derive(Debug, Clone, Default)]
pub struct CommandSpec {
    /// Arguments, without the leading program name.
    pub args: Vec<String>,
    /// Working directory for the invocation.
    pub workdir: PathBuf,
    /// Extra environment on top of the inherited process environment.
    pub env: Vec<(String, String)>,
    /// Text piped to the CLI's stdin, for the few commands that read it.
    pub stdin: Option<String>,
}

/// The seam every CLI invocation goes through. The default implementation
/// is [`LocalPulumiCommand`]; tests substitute a recorder to assert on the
/// exact arguments without running a CLI.
pub trait PulumiCommand: Send + Sync {
    /// The CLI version, used to gate flags that newer CLIs added.
    fn version(&self) -> Version;

    /// Run the CLI to completion, capturing both streams.
    ///
    /// `Ok` means exit code zero; any other outcome is an
    /// [`Error::Command`] carrying the captured streams.
    fn run(&self, spec: CommandSpec) -> BoxFuture<'static, Result<CommandResult>>;
}

/// Options for locating and validating the `pulumi` binary.
#[derive(Debug, Clone, Default)]
pub struct PulumiCommandOptions {
    /// Require at least this CLI version instead of the SDK's own floor.
    pub version: Option<Version>,
    /// An installation root to use instead of searching `PATH`; the binary
    /// is expected at `<root>/bin/pulumi`.
    pub root: Option<PathBuf>,
    /// Skip the version validation. The `PULUMI_AUTOMATION_API_SKIP_VERSION_CHECK`
    /// environment variable (set to `1` or `true`) does the same.
    pub skip_version_check: bool,
}

/// The `pulumi` CLI on this machine.
#[derive(Debug, Clone)]
pub struct LocalPulumiCommand {
    command: PathBuf,
    version: Version,
}

impl LocalPulumiCommand {
    /// Locate `pulumi` (on `PATH`, or under `options.root`) and validate
    /// its version.
    pub async fn new(options: PulumiCommandOptions) -> Result<Self> {
        let command = match &options.root {
            Some(root) => root.join("bin").join(exe_name()),
            None => PathBuf::from(exe_name()),
        };

        let output = tokio::process::Command::new(&command)
            .arg("version")
            .env("PULUMI_SKIP_UPDATE_CHECK", "true")
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| Error::setup(format!("failed to run `pulumi version`: {e}")))?;
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();

        let skip = options.skip_version_check || skip_version_check_from_env(&HashMap::new());
        let minimum = match &options.version {
            Some(v) if *v > MINIMUM_VERSION => v.clone(),
            _ => MINIMUM_VERSION,
        };
        let version = parse_and_validate_version(&raw, &minimum, skip)?;
        Ok(LocalPulumiCommand { command, version })
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "pulumi.exe"
    } else {
        "pulumi"
    }
}

/// Whether the skip-version-check variable is truthy in the process
/// environment or in `extra` (a workspace's own environment map).
pub(crate) fn skip_version_check_from_env(extra: &HashMap<String, String>) -> bool {
    let truthy = |v: &str| v == "1" || v.eq_ignore_ascii_case("true");
    std::env::var(SKIP_VERSION_CHECK_VAR)
        .map(|v| truthy(&v))
        .unwrap_or(false)
        || extra.get(SKIP_VERSION_CHECK_VAR).is_some_and(|v| truthy(v))
}

/// Parse a `pulumi version` output leniently (a leading `v` and missing
/// minor/patch components are tolerated, as with Go's `semver.ParseTolerant`).
fn parse_tolerant(raw: &str) -> Option<Version> {
    let s = raw.trim().trim_start_matches('v');
    if let Ok(v) = Version::parse(s) {
        return Some(v);
    }
    // Pad partial versions like "3" or "3.2" before any -pre/+build suffix.
    let split = s.find(['-', '+']).unwrap_or(s.len());
    let (core, suffix) = s.split_at(split);
    let padded = match core.split('.').count() {
        1 => format!("{core}.0.0{suffix}"),
        2 => format!("{core}.0{suffix}"),
        _ => return None,
    };
    Version::parse(&padded).ok()
}

fn parse_and_validate_version(raw: &str, minimum: &Version, skip: bool) -> Result<Version> {
    let version = match parse_tolerant(raw) {
        Some(v) => v,
        None if skip => Version::new(0, 0, 0),
        None => {
            return Err(Error::setup(format!(
            "Unable to parse Pulumi CLI version (skip with {SKIP_VERSION_CHECK_VAR}=true): {raw:?}"
        )))
        }
    };
    if skip {
        return Ok(version);
    }
    if minimum.major < version.major {
        return Err(Error::setup(format!(
            "Major version mismatch. You are using Pulumi CLI version {version} with \
             Automation SDK for major version {}. Please update the SDK.",
            minimum.major
        )));
    }
    if *minimum > version {
        return Err(Error::setup(format!(
            "Minimum version requirement failed. The minimum CLI version requirement is \
             {minimum}, your current CLI version is {version}. Please update the Pulumi CLI."
        )));
    }
    Ok(version)
}

impl PulumiCommand for LocalPulumiCommand {
    fn version(&self) -> Version {
        self.version.clone()
    }

    fn run(&self, spec: CommandSpec) -> BoxFuture<'static, Result<CommandResult>> {
        let command = self.command.clone();
        Box::pin(async move {
            let args = with_non_interactive(spec.args);
            let mut cmd = tokio::process::Command::new(&command);
            cmd.args(&args)
                .current_dir(&spec.workdir)
                // Tells the CLI it is being driven programmatically.
                .env("PULUMI_AUTOMATION_API", "true")
                // Cancelling an operation future (a timeout, a dropped
                // task) must not leave the CLI running invisibly with the
                // stack lock held. Go interrupts and then kills; a kill is
                // the portable half of that, and blunt beats orphaned.
                .kill_on_drop(true)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // An explicitly located binary must win over whatever `pulumi`
            // is on PATH, because the CLI re-invokes itself and its plugins
            // through PATH lookups. A PATH the workspace itself sets still
            // takes precedence below.
            if command.is_absolute() {
                if let Some(bin) = command.parent() {
                    cmd.env("PATH", prepend_path(bin));
                }
            }
            for (k, v) in &spec.env {
                cmd.env(k, v);
            }
            cmd.stdin(if spec.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });

            let mut child = cmd
                .spawn()
                .map_err(|e| Error::setup(format!("failed to spawn pulumi: {e}")))?;
            if let Some(input) = spec.stdin {
                use tokio::io::AsyncWriteExt;
                let mut stdin = child.stdin.take().expect("stdin was requested piped");
                stdin
                    .write_all(input.as_bytes())
                    .await
                    .map_err(|e| Error::setup(format!("failed to write to pulumi stdin: {e}")))?;
                drop(stdin);
            }
            let output = child
                .wait_with_output()
                .await
                .map_err(|e| Error::setup(format!("failed to wait for pulumi: {e}")))?;

            let result = CommandResult {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                code: output.status.code().unwrap_or(UNKNOWN_ERR_CODE),
            };
            if result.code == 0 {
                Ok(result)
            } else {
                Err(Error::command(
                    format!("exit status {}", result.code),
                    result,
                ))
            }
        })
    }
}

/// Prepend `dir` to the current `PATH`.
fn prepend_path(dir: &Path) -> std::ffi::OsString {
    let mut paths = vec![dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).unwrap_or_else(|_| dir.into())
}

/// Prepend `--non-interactive` unless it already appears before a literal
/// `--` separator; commands must fail rather than hang on a prompt.
fn with_non_interactive(args: Vec<String>) -> Vec<String> {
    let flags = args.split(|a| a == "--").next().unwrap_or(&[]);
    if flags.iter().any(|a| a == "--non-interactive") {
        return args;
    }
    let mut out = Vec::with_capacity(args.len() + 1);
    out.push("--non-interactive".to_string());
    out.extend(args);
    out
}

/// A convenient alias for the shared, dynamically-dispatched command.
pub(crate) type SharedCommand = Arc<dyn PulumiCommand>;

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    use super::*;

    /// Records every [`CommandSpec`] and replays canned results, so tests
    /// can assert on exact CLI arguments without a CLI.
    pub struct RecordingCommand {
        pub specs: Mutex<Vec<CommandSpec>>,
        pub results: Mutex<Vec<Result<CommandResult>>>,
        pub version: Version,
    }

    impl Default for RecordingCommand {
        fn default() -> Self {
            RecordingCommand {
                specs: Mutex::new(vec![]),
                results: Mutex::new(vec![]),
                version: Version::new(3, 256, 0),
            }
        }
    }

    impl RecordingCommand {
        /// Queue the result the next `run` call returns. With the queue
        /// empty, runs succeed with empty output.
        pub fn push_result(&self, result: Result<CommandResult>) {
            self.results.lock().unwrap().push(result);
        }

        pub fn recorded_args(&self) -> Vec<Vec<String>> {
            self.specs
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.args.clone())
                .collect()
        }
    }

    impl PulumiCommand for RecordingCommand {
        fn version(&self) -> Version {
            self.version.clone()
        }

        fn run(&self, spec: CommandSpec) -> BoxFuture<'static, Result<CommandResult>> {
            self.specs.lock().unwrap().push(spec);
            let mut results = self.results.lock().unwrap();
            let result = if results.is_empty() {
                Ok(CommandResult::default())
            } else {
                results.remove(0)
            };
            Box::pin(async move { result })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions_tolerantly() {
        assert_eq!(parse_tolerant("3.256.0"), Some(Version::new(3, 256, 0)));
        assert_eq!(parse_tolerant("v3.256.0\n"), Some(Version::new(3, 256, 0)));
        assert_eq!(parse_tolerant("3.2"), Some(Version::new(3, 2, 0)));
        assert_eq!(
            parse_tolerant("3.101.0-dev.1"),
            Some(Version::parse("3.101.0-dev.1").unwrap())
        );
        assert_eq!(parse_tolerant("not-a-version"), None);
    }

    #[test]
    fn validates_minimum_version() {
        let min = Version::new(3, 2, 0);
        assert!(parse_and_validate_version("3.256.0", &min, false).is_ok());
        // A prerelease of the minimum is below the minimum.
        let err = parse_and_validate_version("3.2.0-alpha.1", &min, false).unwrap_err();
        assert!(err
            .to_string()
            .contains("Minimum version requirement failed"));
        // Skipping accepts anything, including garbage.
        assert_eq!(
            parse_and_validate_version("garbage", &min, true).unwrap(),
            Version::new(0, 0, 0)
        );
        let err = parse_and_validate_version("garbage", &min, false).unwrap_err();
        assert!(err
            .to_string()
            .contains("Unable to parse Pulumi CLI version"));
    }

    #[test]
    fn version_parsing_tolerates_surrounding_whitespace() {
        assert_eq!(
            parse_tolerant("\n\n  v3.242.0  \n\n"),
            Some(Version::new(3, 242, 0))
        );
        assert!(parse_and_validate_version("\n3.242.0\n", &Version::new(3, 2, 0), false).is_ok());
    }

    #[test]
    fn non_interactive_is_prepended_once() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            with_non_interactive(args(&["up", "--yes"])),
            args(&["--non-interactive", "up", "--yes"])
        );
        assert_eq!(
            with_non_interactive(args(&["up", "--non-interactive"])),
            args(&["up", "--non-interactive"])
        );
        // A --non-interactive after the positional separator does not count.
        assert_eq!(
            with_non_interactive(args(&["config", "set", "k", "--", "--non-interactive"])),
            args(&[
                "--non-interactive",
                "config",
                "set",
                "k",
                "--",
                "--non-interactive"
            ])
        );
    }
}
