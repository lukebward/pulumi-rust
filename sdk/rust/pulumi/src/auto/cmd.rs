//! Running the `pulumi` CLI: locating the binary, validating its version,
//! and executing commands with the environment the automation API requires.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    /// For [`LocalPulumiCommand::install`], the version to install.
    pub version: Option<Version>,
    /// An installation root to use instead of searching `PATH`; the binary
    /// is expected at `<root>/bin/pulumi`, which is also where
    /// [`LocalPulumiCommand::install`] puts it.
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

    /// Download and install the Pulumi CLI, the Rust analogue of Go's
    /// `InstallPulumiCommand`. An installation already under the root that
    /// satisfies `options.version` is reused untouched. `options.root`
    /// defaults to `$HOME/.pulumi/versions/<version>`. `options.version`
    /// defaults to the latest released CLI — the Rust SDK's own version
    /// does not track CLI releases the way the other SDKs' versions do.
    pub async fn install(options: PulumiCommandOptions) -> Result<Self> {
        let version = match &options.version {
            Some(v) => v.clone(),
            None => latest_cli_version().await?,
        };
        let root = match &options.root {
            Some(r) => r.clone(),
            None => default_install_root(&version)?,
        };
        let options = PulumiCommandOptions {
            version: Some(version.clone()),
            root: Some(root.clone()),
            ..options
        };
        if let Ok(existing) = Self::new(options.clone()).await {
            return Ok(existing);
        }
        if cfg!(windows) {
            // The Windows release is a zip; the SDK only extracts tarballs.
            return Err(Error::setup(
                "installing the Pulumi CLI is not yet supported on Windows",
            ));
        }
        download_and_extract_cli(&version, &root).await?;
        Self::new(options).await
    }
}

/// Where an installed CLI lives by default, shared with the other SDKs.
fn default_install_root(version: &Version) -> Result<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let home = std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            Error::setup(format!(
                "failed to determine the home directory: {var} is not set"
            ))
        })?;
    Ok(PathBuf::from(home)
        .join(".pulumi")
        .join("versions")
        .join(version.to_string()))
}

/// The release artifact for a CLI version — the same get.pulumi.com
/// tarballs (zip on Windows) that Go's install script downloads.
fn download_url(version: &Version, os: &str, arch: &str) -> Result<String> {
    let os_name = match os {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        other => {
            return Err(Error::setup(format!(
                "no Pulumi CLI release for OS {other:?}"
            )))
        }
    };
    let arch_name = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => {
            return Err(Error::setup(format!(
                "no Pulumi CLI release for architecture {other:?}"
            )))
        }
    };
    let ext = if os_name == "windows" {
        "zip"
    } else {
        "tar.gz"
    };
    Ok(format!(
        "https://get.pulumi.com/releases/sdk/pulumi-v{version}-{os_name}-{arch_name}.{ext}"
    ))
}

/// An HTTP client with timeouts, so a stalled connection fails instead of
/// hanging `install` forever. `read_timeout` bounds each chunk, not the
/// whole transfer, so a slow tarball download still completes.
fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| Error::setup(format!("failed to build an HTTP client: {e}")))
}

/// Status failures get a distinct message from transport failures: tests
/// classify "failed to download" as a network problem worth skipping on,
/// and a bad URL (an HTTP error status) must not read as one.
fn http_error(url: &str, e: reqwest::Error) -> Error {
    match e.status() {
        Some(status) => Error::setup(format!("unexpected HTTP status {status} for {url}")),
        None => Error::setup(format!("failed to download {url}: {e}")),
    }
}

/// The CLI version get.pulumi.com considers current.
async fn latest_cli_version() -> Result<Version> {
    let url = "https://www.pulumi.com/latest-version";
    let wrap = |e: reqwest::Error| http_error(url, e);
    let response = http_client()?
        .get(url)
        .send()
        .await
        .map_err(wrap)?
        .error_for_status()
        .map_err(wrap)?;
    let text = response.text().await.map_err(wrap)?;
    parse_tolerant(&text)
        .ok_or_else(|| Error::setup(format!("unexpected response from {url}: {text:?}")))
}

/// Download the release tarball for `version` and unpack its binaries
/// into `<root>/bin`.
async fn download_and_extract_cli(version: &Version, root: &Path) -> Result<()> {
    let url = download_url(version, std::env::consts::OS, std::env::consts::ARCH)?;
    tokio::fs::create_dir_all(root)
        .await
        .map_err(|e| Error::setup(format!("failed to create {}: {e}", root.display())))?;

    let wrap = |e: reqwest::Error| http_error(&url, e);
    let mut response = http_client()?
        .get(url.as_str())
        .send()
        .await
        .map_err(wrap)?
        .error_for_status()
        .map_err(wrap)?;

    // Stream to a sibling of the destination so the extract's rename stays
    // on one filesystem. Scratch names are unique per attempt so concurrent
    // installs of the same version into the shared root never interleave;
    // the final rename into <root>/bin stays the single racy step, and a
    // same-version winner there is equivalent.
    static ATTEMPT: AtomicU64 = AtomicU64::new(0);
    let scratch = format!(
        "{}-{}",
        std::process::id(),
        ATTEMPT.fetch_add(1, Ordering::Relaxed)
    );
    let tarball = root.join(format!("pulumi-v{version}-{scratch}.tar.gz.partial"));
    let staging = root.join(format!(".extract-{scratch}.partial"));
    let io_err =
        |e: std::io::Error| Error::setup(format!("failed to write {}: {e}", tarball.display()));
    let result = async {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::File::create(&tarball).await.map_err(io_err)?;
        while let Some(chunk) = response.chunk().await.map_err(wrap)? {
            file.write_all(&chunk).await.map_err(io_err)?;
        }
        file.flush().await.map_err(io_err)?;
        drop(file);
        let tarball = tarball.clone();
        let staging = staging.clone();
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || extract_cli_tarball(&tarball, &staging, &root))
            .await
            .map_err(|e| Error::setup(format!("extraction task failed: {e}")))?
    }
    .await;
    let _ = tokio::fs::remove_file(&tarball).await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    result
}

/// Unpack a CLI release tarball via `staging`: the archive holds one
/// top-level `pulumi/` directory whose contents become `<root>/bin`.
fn extract_cli_tarball(tarball: &Path, staging: &Path, root: &Path) -> Result<()> {
    let io_err =
        |e: std::io::Error| Error::setup(format!("failed to extract {}: {e}", tarball.display()));
    let file = std::fs::File::open(tarball).map_err(io_err)?;
    let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    tar::Archive::new(decoder).unpack(staging).map_err(io_err)?;
    let unpacked = staging.join("pulumi");
    if !unpacked.is_dir() {
        return Err(Error::setup(format!(
            "unexpected layout in {}: no top-level pulumi/ directory",
            tarball.display()
        )));
    }
    let bin = root.join("bin");
    if bin.exists() {
        std::fs::remove_dir_all(&bin).map_err(io_err)?;
    }
    std::fs::rename(&unpacked, &bin).map_err(io_err)?;
    let _ = std::fs::remove_dir_all(staging);
    Ok(())
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
            for (k, v) in &spec.env {
                cmd.env(k, v);
            }
            // An explicitly located binary must win over whatever `pulumi`
            // is on PATH, because the CLI re-invokes itself and its plugins
            // through PATH lookups. Go's fixupPath rewrites only the
            // inherited PATH entry, and a workspace-set PATH then shadows
            // it verbatim; match that effective behavior: prepend the
            // bundled bin dir to the inherited PATH, and leave a
            // workspace-set PATH untouched.
            if command.is_absolute() {
                if let Some(bin) = command.parent() {
                    let workspace_sets_path = spec
                        .env
                        .iter()
                        // Case-insensitive: on Windows the entry is "Path".
                        .any(|(k, _)| k.eq_ignore_ascii_case("PATH"));
                    if !workspace_sets_path {
                        let inherited = std::env::var_os("PATH");
                        cmd.env("PATH", fixup_path(bin, inherited.as_deref()));
                    }
                }
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

/// Compose the `PATH` for an explicitly located binary: `bin` first, then
/// whatever was already in effect.
fn fixup_path(bin: &Path, existing: Option<&OsStr>) -> OsString {
    match existing {
        Some(path) if !path.is_empty() => {
            let mut paths = vec![bin.to_path_buf()];
            paths.extend(std::env::split_paths(path));
            std::env::join_paths(paths).unwrap_or_else(|_| bin.into())
        }
        _ => bin.into(),
    }
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
    fn builds_release_download_urls() {
        let v = Version::new(3, 200, 0);
        let url = |os, arch| download_url(&v, os, arch).unwrap();
        let base = "https://get.pulumi.com/releases/sdk";
        assert_eq!(
            url("macos", "aarch64"),
            format!("{base}/pulumi-v3.200.0-darwin-arm64.tar.gz")
        );
        assert_eq!(
            url("macos", "x86_64"),
            format!("{base}/pulumi-v3.200.0-darwin-x64.tar.gz")
        );
        assert_eq!(
            url("linux", "x86_64"),
            format!("{base}/pulumi-v3.200.0-linux-x64.tar.gz")
        );
        assert_eq!(
            url("linux", "aarch64"),
            format!("{base}/pulumi-v3.200.0-linux-arm64.tar.gz")
        );
        assert_eq!(
            url("windows", "x86_64"),
            format!("{base}/pulumi-v3.200.0-windows-x64.zip")
        );
        assert!(download_url(&v, "freebsd", "x86_64").is_err());
        assert!(download_url(&v, "linux", "riscv64").is_err());
    }

    #[test]
    fn default_root_is_under_home_versions() {
        let root = default_install_root(&Version::new(3, 200, 0)).unwrap();
        let tail = Path::new(".pulumi").join("versions").join("3.200.0");
        assert!(root.ends_with(&tail), "unexpected root: {root:?}");
    }

    #[test]
    fn fixup_path_prepends_the_bundled_bin_dir() {
        let bin = Path::new("/opt/pulumi/bin");
        let sep = if cfg!(windows) { ';' } else { ':' };
        assert_eq!(
            fixup_path(bin, Some(OsStr::new("/usr/bin"))),
            OsString::from(format!("/opt/pulumi/bin{sep}/usr/bin"))
        );
        // An empty or absent PATH becomes just the bundled dir.
        assert_eq!(
            fixup_path(bin, Some(OsStr::new(""))),
            OsString::from("/opt/pulumi/bin")
        );
        assert_eq!(fixup_path(bin, None), OsString::from("/opt/pulumi/bin"));
    }

    /// A fake `<root>/bin/pulumi` that reports `reports` as its version.
    #[cfg(unix)]
    fn fake_cli(root: &Path, reports: &str) {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = bin.join("pulumi");
        std::fs::write(&path, format!("#!/bin/sh\necho {reports}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn install_reuses_a_matching_existing_binary() {
        let root = crate::auto::scratch_dir("pulumi-install-reuse").unwrap();
        fake_cli(&root, "3.200.0");
        let binary = root.join("bin").join("pulumi");
        let before = std::fs::metadata(&binary).unwrap().modified().unwrap();
        // Must return without touching the install (or the network).
        let cmd = LocalPulumiCommand::install(PulumiCommandOptions {
            version: Some(Version::new(3, 200, 0)),
            root: Some(root.clone()),
            ..Default::default()
        })
        .await
        .expect("existing matching install is reused");
        assert_eq!(cmd.version(), Version::new(3, 200, 0));
        let after = std::fs::metadata(&binary).unwrap().modified().unwrap();
        assert_eq!(before, after);

        // A newer existing binary also satisfies the request, as in Go.
        fake_cli(&root, "3.250.0");
        let cmd = LocalPulumiCommand::install(PulumiCommandOptions {
            version: Some(Version::new(3, 200, 0)),
            root: Some(root.clone()),
            ..Default::default()
        })
        .await
        .expect("newer existing install is reused");
        assert_eq!(cmd.version(), Version::new(3, 250, 0));
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn verification_rejects_a_binary_reporting_the_wrong_version() {
        let root = crate::auto::scratch_dir("pulumi-install-verify").unwrap();
        fake_cli(&root, "3.100.0");
        let err = LocalPulumiCommand::new(PulumiCommandOptions {
            version: Some(Version::new(3, 200, 0)),
            root: Some(root.clone()),
            ..Default::default()
        })
        .await
        .expect_err("a binary below the requested version must fail validation");
        assert!(
            err.to_string()
                .contains("Minimum version requirement failed"),
            "{err}"
        );
        std::fs::remove_dir_all(&root).ok();
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
