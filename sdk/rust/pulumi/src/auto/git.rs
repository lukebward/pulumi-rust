//! Git-sourced workspaces: cloning a repository into a
//! [`LocalWorkspace`](super::LocalWorkspace)'s work dir, mirroring the Go
//! SDK's `GitRepo` support.
//!
//! Where Go clones in-process with go-git, this implementation shells out
//! to the system `git` binary, which therefore must be on `PATH`. Ref
//! semantics match Go's: `branch` accepts a plain branch name (slashes
//! included), `refs/heads/<branch>`, `refs/remotes/origin/<branch>`, or
//! `refs/tags/<tag>`; a bare tag name does not resolve, and a remote ref
//! for any remote other than `origin` is an error.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;

use super::errors::{Error, Result};
use super::workspace::LocalWorkspace;

/// A function to execute after enlisting in a git repo. It receives the
/// workspace once settings files are in place, before first use.
pub type SetupFn = Arc<dyn Fn(LocalWorkspace) -> BoxFuture<'static, Result<()>> + Send + Sync>;

/// A git repository holding a Pulumi program, cloned into the workspace's
/// work dir when a [`LocalWorkspace`] is created. The Rust analogue of the
/// Go SDK's `auto.GitRepo`; requires a `git` binary on `PATH`.
#[derive(Clone, Default)]
pub struct GitRepo {
    /// URL to clone; anything the system git accepts.
    pub url: String,
    /// Path relative to the repository root where the Pulumi program
    /// lives; the workspace's work dir points there after the clone.
    pub project_path: Option<PathBuf>,
    /// Branch (or full ref) to check out — see the module docs for the
    /// accepted shapes.
    pub branch: Option<String>,
    /// Commit to check out, as a full hash.
    pub commit_hash: Option<String>,
    /// Function to execute after enlisting in the repo.
    pub setup: Option<SetupFn>,
    /// Authentication for a private repository.
    pub auth: Option<GitAuth>,
    /// Clone with `--depth 1` instead of the full history.
    pub shallow: bool,
}

impl std::fmt::Debug for GitRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitRepo")
            .field("url", &self.url)
            .field("project_path", &self.project_path)
            .field("branch", &self.branch)
            .field("commit_hash", &self.commit_hash)
            .field("setup", &self.setup.as_ref().map(|_| "<setup>"))
            .field("shallow", &self.shallow)
            .finish_non_exhaustive()
    }
}

/// Authentication for a private git repository. The options are mutually
/// exclusive: a personal access token, a username with password, or an
/// SSH private key (by path or by contents).
///
/// One divergence from Go: `password` cannot serve as an SSH key
/// passphrase, because the `ssh` binary has no non-interactive way to
/// receive one — a passphrase alongside a key is an error here. SSH runs
/// with `BatchMode=yes`, so the remote host key must already be in
/// `known_hosts`: `ssh` fails fast instead of prompting.
#[derive(Clone, Default)]
pub struct GitAuth {
    /// Absolute path to a private key. The repository URL must be in SSH
    /// form (`git@host:org/repo.git`).
    pub ssh_private_key_path: Option<PathBuf>,
    /// The contents of a private key. Written to a temporary file with
    /// `0600` permissions for the duration of the clone, then removed.
    pub ssh_private_key: Option<String>,
    /// The password paired with `username`.
    pub password: Option<String>,
    /// A personal access token, sent as an HTTP basic-auth password with
    /// username `git`.
    pub personal_access_token: Option<String>,
    /// The username paired with `password`.
    pub username: Option<String>,
}

impl std::fmt::Debug for GitAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn redact<T>(secret: &Option<T>) -> &'static str {
            if secret.is_some() {
                "<redacted>"
            } else {
                "None"
            }
        }
        f.debug_struct("GitAuth")
            .field("ssh_private_key_path", &self.ssh_private_key_path)
            .field("ssh_private_key", &redact(&self.ssh_private_key))
            .field("password", &redact(&self.password))
            .field(
                "personal_access_token",
                &redact(&self.personal_access_token),
            )
            .field("username", &self.username)
            .finish()
    }
}

/// The `branch` field, resolved the way Go's setupGitRepo resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedRef {
    /// No branch given: clone the remote's default branch.
    Default,
    Branch(String),
    Tag(String),
}

fn resolve_reference(branch: Option<&str>) -> Result<ResolvedRef> {
    let Some(branch) = branch.filter(|b| !b.is_empty()) else {
        return Ok(ResolvedRef::Default);
    };
    if let Some(rest) = branch.strip_prefix("refs/remotes/") {
        return match rest.strip_prefix("origin/") {
            Some(name) => Ok(ResolvedRef::Branch(name.to_string())),
            None => Err(Error::setup(format!(
                "a remote ref must begin with 'refs/remote/origin/', but got {branch:?}"
            ))),
        };
    }
    if let Some(name) = branch.strip_prefix("refs/tags/") {
        return Ok(ResolvedRef::Tag(name.to_string()));
    }
    if let Some(name) = branch.strip_prefix("refs/heads/") {
        return Ok(ResolvedRef::Branch(name.to_string()));
    }
    // Anything else — a plain name, slashes or not, even a malformed
    // refs/... path — is treated as a branch name, exactly as go-git's
    // NewBranchReferenceName does. A bare tag name therefore fails.
    Ok(ResolvedRef::Branch(branch.to_string()))
}

/// One planned `git` invocation. `args` carries no secrets; auth travels
/// via environment and `-c` config added at execution time.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitCommand {
    args: Vec<String>,
    /// A failed exit is tolerated: matches Go tolerating servers that
    /// cannot fetch an exact commit hash already present locally.
    allow_failure: bool,
    error_context: &'static str,
}

impl GitCommand {
    fn new<const N: usize>(args: [&str; N]) -> Self {
        GitCommand {
            args: args.iter().map(|s| s.to_string()).collect(),
            allow_failure: false,
            error_context: CLONE_CONTEXT,
        }
    }
}

const CLONE_CONTEXT: &str = "unable to clone repo";
const CHECKOUT_CONTEXT: &str = "unable to checkout commit";

/// The full command sequence for a clone, relative to the (existing,
/// empty) target directory: each command runs with the target as cwd, and
/// clones name `.` as the destination.
fn clone_commands(repo: &GitRepo) -> Result<Vec<GitCommand>> {
    let mut commands = vec![];
    let resolved = resolve_reference(repo.branch.as_deref())?;
    let targeted_fetch = resolved != ResolvedRef::Default;
    match resolved {
        ResolvedRef::Default => {
            let mut clone = GitCommand::new(["clone", "--quiet"]);
            if repo.shallow {
                // Depth 1, single branch, no tags: what Go's Shallow sets.
                clone
                    .args
                    .extend(svec(["--depth", "1", "--single-branch", "--no-tags"]));
            }
            clone.args.extend(svec(["--", &repo.url, "."]));
            commands.push(clone);
        }
        ResolvedRef::Branch(name) => {
            commands.push(GitCommand::new(["init", "--quiet"]));
            commands.push(GitCommand::new(["remote", "add", "origin", &repo.url]));
            commands.push(fetch_ref(repo, &format!("refs/heads/{name}")));
            commands.push(GitCommand::new([
                "checkout",
                "--quiet",
                "-b",
                &name,
                "FETCH_HEAD",
            ]));
        }
        ResolvedRef::Tag(name) => {
            commands.push(GitCommand::new(["init", "--quiet"]));
            commands.push(GitCommand::new(["remote", "add", "origin", &repo.url]));
            commands.push(fetch_ref(repo, &format!("refs/tags/{name}")));
            commands.push(GitCommand::new([
                "checkout",
                "--quiet",
                "--detach",
                "FETCH_HEAD",
            ]));
        }
    }

    if let Some(hash) = repo.commit_hash.as_deref().filter(|h| !h.is_empty()) {
        if targeted_fetch && !repo.shallow {
            // Go's non-shallow clone fetches every head and tag even when
            // a ReferenceName is set, so any reachable commit is available
            // without a bare-hash fetch. Match that before the checkout.
            commands.push(GitCommand::new([
                "fetch",
                "--quiet",
                "origin",
                "+refs/heads/*:refs/remotes/origin/*",
                "+refs/tags/*:refs/tags/*",
            ]));
        }
        // Ensure the commit is present; a full clone already has it, and
        // not every transport can fetch a bare hash, so failure here is
        // tolerated and the checkout below is the arbiter. Divergence:
        // Go reports genuine fetch errors as "fetching commit: ..."; here
        // they surface from the checkout instead.
        let mut fetch = fetch_ref(repo, hash);
        fetch.allow_failure = true;
        commands.push(fetch);
        commands.push(GitCommand {
            args: svec(["checkout", "--quiet", "--force", hash]),
            allow_failure: false,
            error_context: CHECKOUT_CONTEXT,
        });
    }

    Ok(commands)
}

/// A targeted fetch of one ref (or hash) from `origin`, so a wrong ref
/// fails loudly instead of falling back to something else.
fn fetch_ref(repo: &GitRepo, refspec: &str) -> GitCommand {
    let mut fetch = GitCommand::new(["fetch", "--quiet"]);
    if repo.shallow {
        fetch.args.extend(svec(["--depth", "1"]));
    }
    fetch.args.push("origin".to_string());
    fetch.args.push(refspec.to_string());
    fetch
}

/// A private-key file created for the duration of a clone, `0600`,
/// removed (with its directory) on drop.
struct TempKeyFile {
    dir: PathBuf,
    path: PathBuf,
}

impl TempKeyFile {
    fn write(contents: &str) -> Result<Self> {
        use std::io::Write;

        let dir = super::scratch_dir("pulumi-auto-git-key")?;
        let path = dir.join("id");
        // Removed even if writing the key below fails.
        let this = TempKeyFile { dir, path };
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            std::fs::set_permissions(&this.dir, std::fs::Permissions::from_mode(0o700))?;
            // 0600 at creation, so the key is never readable by others.
            options.mode(0o600);
        }
        options.open(&this.path)?.write_all(contents.as_bytes())?;
        Ok(this)
    }
}

impl Drop for TempKeyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Quote `s` for a POSIX shell: git runs `GIT_SSH_COMMAND` through one,
/// and the key path is caller-supplied.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Credential helper that reads the values from the environment, so
/// secrets appear in neither the argv nor any file on disk.
const CREDENTIAL_HELPER: &str = r#"!f() { if [ "$1" = get ]; then printf 'username=%s\npassword=%s\n' "$PULUMI_GIT_USERNAME" "$PULUMI_GIT_PASSWORD"; fi; }; f"#;

/// Environment and `-c` config implementing a [`GitAuth`]; holds the
/// temporary key file alive for the duration of the clone.
struct AuthSetup {
    env: Vec<(String, String)>,
    config: Vec<String>,
    _key_file: Option<TempKeyFile>,
}

/// Manual so the environment's secret values never print.
impl std::fmt::Debug for AuthSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthSetup")
            .field("env", &self.env.iter().map(|(k, _)| k).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

fn auth_setup(auth: Option<&GitAuth>) -> Result<AuthSetup> {
    let mut setup = AuthSetup {
        env: vec![],
        config: vec![],
        _key_file: None,
    };
    let Some(auth) = auth else { return Ok(setup) };

    let is_set = |o: &Option<String>| o.as_deref().is_some_and(|s| !s.is_empty());
    let key_path_set = auth
        .ssh_private_key_path
        .as_deref()
        .is_some_and(|p| !p.as_os_str().is_empty());
    let selected = [
        is_set(&auth.personal_access_token),
        is_set(&auth.username),
        is_set(&auth.ssh_private_key),
        key_path_set,
    ]
    .iter()
    .filter(|set| **set)
    .count();
    if selected > 1 {
        return Err(Error::setup(
            "please specify one authentication option of `Personal Access Token`, \
             `Username\\Password`, `SSH Private Key Path` or `SSH Private Key`",
        ));
    }

    if key_path_set || is_set(&auth.ssh_private_key) {
        if is_set(&auth.password) {
            return Err(Error::setup(
                "SSH private key passphrases are not supported: the system `ssh` \
                 cannot receive one non-interactively; use an unencrypted key",
            ));
        }
        let key_path = match &auth.ssh_private_key {
            Some(contents) => {
                let key_file = TempKeyFile::write(contents)?;
                let path = key_file.path.clone();
                setup._key_file = Some(key_file);
                path
            }
            None => auth.ssh_private_key_path.clone().unwrap_or_default(),
        };
        setup.env.push((
            "GIT_SSH_COMMAND".to_string(),
            // BatchMode makes ssh fail instead of prompting for a host key
            // or passphrase; GIT_TERMINAL_PROMPT does not reach ssh.
            format!(
                "ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes",
                shell_single_quote(&key_path.display().to_string()),
            ),
        ));
        return Ok(setup);
    }

    // A PAT's basic-auth username can be anything non-empty; `git`
    // matches what the Go SDK sends.
    let credentials = if is_set(&auth.personal_access_token) {
        Some((
            "git".to_string(),
            auth.personal_access_token.clone().unwrap_or_default(),
        ))
    } else if is_set(&auth.username) && is_set(&auth.password) {
        Some((
            auth.username.clone().unwrap_or_default(),
            auth.password.clone().unwrap_or_default(),
        ))
    } else {
        None
    };
    if let Some((username, password)) = credentials {
        setup
            .env
            .push(("PULUMI_GIT_USERNAME".to_string(), username));
        setup
            .env
            .push(("PULUMI_GIT_PASSWORD".to_string(), password));
        setup.config.push("-c".to_string());
        setup
            .config
            .push(format!("credential.helper={CREDENTIAL_HELPER}"));
    }
    Ok(setup)
}

async fn run_git(work_dir: &Path, auth: &AuthSetup, command: &GitCommand) -> Result<()> {
    let output = tokio::process::Command::new("git")
        .args(&auth.config)
        .args(&command.args)
        .current_dir(work_dir)
        .envs(auth.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::setup(
                    "git-sourced workspaces require a `git` binary on PATH, and none was found",
                )
            } else {
                Error::setup(format!("failed to run git: {e}"))
            }
        })?;
    if output.status.success() || command.allow_failure {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = match stderr.trim() {
        "" => format!("git exited with {}", output.status),
        stderr => stderr.to_string(),
    };
    Err(Error::setup(format!("{}: {detail}", command.error_context)))
}

/// Clone `repo` into `work_dir` and return the directory the workspace
/// should use: `work_dir` itself, or `project_path` under it.
pub(crate) async fn setup_git_repo(work_dir: &Path, repo: &GitRepo) -> Result<PathBuf> {
    let auth = auth_setup(repo.auth.as_ref())?;
    for command in clone_commands(repo)? {
        run_git(work_dir, &auth, &command).await?;
    }
    Ok(match &repo.project_path {
        Some(path) => work_dir.join(path),
        None => work_dir.to_path_buf(),
    })
}

/// Build a `Vec<String>` from string literals.
fn svec<const N: usize>(items: [&str; N]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(branch: Option<&str>) -> GitRepo {
        GitRepo {
            url: "https://example.com/org/repo.git".to_string(),
            branch: branch.map(|b| b.to_string()),
            ..Default::default()
        }
    }

    fn argv(commands: &[GitCommand]) -> Vec<Vec<String>> {
        commands.iter().map(|c| c.args.clone()).collect()
    }

    #[test]
    fn default_ref_is_a_plain_clone() {
        let commands = clone_commands(&repo(None)).unwrap();
        assert_eq!(
            argv(&commands),
            vec![svec([
                "clone",
                "--quiet",
                "--",
                "https://example.com/org/repo.git",
                "."
            ])]
        );
    }

    #[test]
    fn shallow_clone_adds_depth_single_branch_no_tags() {
        let mut r = repo(None);
        r.shallow = true;
        let commands = clone_commands(&r).unwrap();
        assert_eq!(
            argv(&commands),
            vec![svec([
                "clone",
                "--quiet",
                "--depth",
                "1",
                "--single-branch",
                "--no-tags",
                "--",
                "https://example.com/org/repo.git",
                "."
            ])]
        );
    }

    /// Every branch spelling Go accepts maps to a refs/heads fetch of the
    /// same branch: plain, slashed, refs/heads/, refs/remotes/origin/.
    #[test]
    fn branch_shapes_fetch_the_heads_ref() {
        for (given, want) in [
            ("default", "default"),
            ("nondefault", "nondefault"),
            ("branch/with/slashes", "branch/with/slashes"),
            ("refs/heads/default", "default"),
            ("refs/heads/branch/with/slashes", "branch/with/slashes"),
            ("refs/remotes/origin/default", "default"),
            (
                "refs/remotes/origin/branch/with/slashes",
                "branch/with/slashes",
            ),
        ] {
            let commands = clone_commands(&repo(Some(given))).unwrap();
            assert_eq!(
                argv(&commands),
                vec![
                    svec(["init", "--quiet"]),
                    svec([
                        "remote",
                        "add",
                        "origin",
                        "https://example.com/org/repo.git"
                    ]),
                    svec(["fetch", "--quiet", "origin", &format!("refs/heads/{want}")]),
                    svec(["checkout", "--quiet", "-b", want, "FETCH_HEAD"]),
                ],
                "branch {given:?}"
            );
        }
    }

    #[test]
    fn tag_ref_fetches_the_tag_and_detaches() {
        let commands = clone_commands(&repo(Some("refs/tags/v0.0.1"))).unwrap();
        assert_eq!(
            argv(&commands),
            vec![
                svec(["init", "--quiet"]),
                svec([
                    "remote",
                    "add",
                    "origin",
                    "https://example.com/org/repo.git"
                ]),
                svec(["fetch", "--quiet", "origin", "refs/tags/v0.0.1"]),
                svec(["checkout", "--quiet", "--detach", "FETCH_HEAD"]),
            ]
        );
    }

    /// A malformed ref is treated as a branch name, as go-git does, so it
    /// fails at fetch time with "couldn't find remote ref".
    #[test]
    fn malformed_ref_becomes_a_branch_fetch() {
        let commands = clone_commands(&repo(Some("refs/notathing/default"))).unwrap();
        assert_eq!(
            commands[2].args,
            svec([
                "fetch",
                "--quiet",
                "origin",
                "refs/heads/refs/notathing/default"
            ])
        );
    }

    /// A bare tag name resolves as a branch, so it cannot check out a tag:
    /// the same "simple tag name won't work" semantics as Go.
    #[test]
    fn bare_tag_name_resolves_as_a_branch() {
        let commands = clone_commands(&repo(Some("v1.0.0"))).unwrap();
        assert_eq!(
            commands[2].args,
            svec(["fetch", "--quiet", "origin", "refs/heads/v1.0.0"])
        );
    }

    #[test]
    fn wrong_remote_is_an_error() {
        let err = clone_commands(&repo(Some("refs/remotes/upstream/default"))).unwrap_err();
        assert_eq!(
            err.to_string(),
            "a remote ref must begin with 'refs/remote/origin/', \
             but got \"refs/remotes/upstream/default\""
        );
    }

    #[test]
    fn commit_hash_appends_tolerated_fetch_then_forced_checkout() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let mut r = repo(None);
        r.commit_hash = Some(hash.to_string());
        let commands = clone_commands(&r).unwrap();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[1].args, svec(["fetch", "--quiet", "origin", hash]));
        assert!(commands[1].allow_failure);
        assert_eq!(
            commands[2].args,
            svec(["checkout", "--quiet", "--force", hash])
        );
        assert!(!commands[2].allow_failure);
        assert_eq!(commands[2].error_context, CHECKOUT_CONTEXT);
    }

    /// A branch plus a hash fetches all heads and tags first, matching
    /// Go's non-shallow clone, which fetches every ref even with a
    /// ReferenceName set — the hash may not be reachable from the branch.
    #[test]
    fn branch_with_commit_hash_widens_the_fetch() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let mut r = repo(Some("dev"));
        r.commit_hash = Some(hash.to_string());
        let commands = clone_commands(&r).unwrap();
        assert_eq!(
            commands[4].args,
            svec([
                "fetch",
                "--quiet",
                "origin",
                "+refs/heads/*:refs/remotes/origin/*",
                "+refs/tags/*:refs/tags/*"
            ])
        );
        assert!(!commands[4].allow_failure);
        assert_eq!(commands[5].args, svec(["fetch", "--quiet", "origin", hash]));
        assert_eq!(
            commands[6].args,
            svec(["checkout", "--quiet", "--force", hash])
        );
    }

    /// Shallow stays single-branch, as Go's Shallow does, so no widening.
    #[test]
    fn shallow_branch_with_commit_hash_does_not_widen() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let mut r = repo(Some("dev"));
        r.shallow = true;
        r.commit_hash = Some(hash.to_string());
        let commands = clone_commands(&r).unwrap();
        assert_eq!(
            commands[4].args,
            svec(["fetch", "--quiet", "--depth", "1", "origin", hash])
        );
    }

    #[test]
    fn shallow_branch_fetch_carries_depth() {
        let mut r = repo(Some("dev"));
        r.shallow = true;
        let commands = clone_commands(&r).unwrap();
        assert_eq!(
            commands[2].args,
            svec([
                "fetch",
                "--quiet",
                "--depth",
                "1",
                "origin",
                "refs/heads/dev"
            ])
        );
    }

    #[test]
    fn auth_options_are_mutually_exclusive() {
        let auth = GitAuth {
            personal_access_token: Some("token".to_string()),
            username: Some("user".to_string()),
            ..Default::default()
        };
        let err = auth_setup(Some(&auth)).unwrap_err();
        assert!(
            err.to_string().contains("one authentication option"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn personal_access_token_flows_through_env_not_argv() {
        let auth = GitAuth {
            personal_access_token: Some("hunter2".to_string()),
            ..Default::default()
        };
        let setup = auth_setup(Some(&auth)).unwrap();
        assert!(setup
            .env
            .contains(&("PULUMI_GIT_USERNAME".to_string(), "git".to_string())));
        assert!(setup
            .env
            .contains(&("PULUMI_GIT_PASSWORD".to_string(), "hunter2".to_string())));
        assert!(!setup.config.join(" ").contains("hunter2"));
        assert!(setup.config[1].starts_with("credential.helper="));
    }

    #[test]
    fn username_password_flow_through_env() {
        let auth = GitAuth {
            username: Some("user".to_string()),
            password: Some("pass".to_string()),
            ..Default::default()
        };
        let setup = auth_setup(Some(&auth)).unwrap();
        assert!(setup
            .env
            .contains(&("PULUMI_GIT_USERNAME".to_string(), "user".to_string())));
        assert!(setup
            .env
            .contains(&("PULUMI_GIT_PASSWORD".to_string(), "pass".to_string())));
    }

    #[test]
    fn ssh_key_path_becomes_the_ssh_command() {
        let auth = GitAuth {
            ssh_private_key_path: Some(PathBuf::from("/keys/id_ed25519")),
            ..Default::default()
        };
        let setup = auth_setup(Some(&auth)).unwrap();
        let (name, value) = &setup.env[0];
        assert_eq!(name, "GIT_SSH_COMMAND");
        assert_eq!(
            value,
            "ssh -i '/keys/id_ed25519' -o IdentitiesOnly=yes -o BatchMode=yes"
        );
    }

    /// The key path is caller-supplied and lands in a shell string; a
    /// single quote in it must not break out of the quoting.
    #[test]
    fn ssh_key_path_with_quote_is_escaped() {
        let auth = GitAuth {
            ssh_private_key_path: Some(PathBuf::from("/keys/o'brien/id")),
            ..Default::default()
        };
        let setup = auth_setup(Some(&auth)).unwrap();
        let (_, value) = &setup.env[0];
        assert!(value.contains(r"'/keys/o'\''brien/id'"), "value: {value}");
    }

    #[test]
    fn inline_ssh_key_lands_in_a_0600_temp_file() {
        let auth = GitAuth {
            ssh_private_key: Some("KEY MATERIAL".to_string()),
            ..Default::default()
        };
        let setup = auth_setup(Some(&auth)).unwrap();
        let key_file = setup._key_file.as_ref().unwrap();
        assert_eq!(
            std::fs::read_to_string(&key_file.path).unwrap(),
            "KEY MATERIAL"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key_file.path)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "mode: {mode:o}");
        }
        let path = key_file.path.clone();
        drop(setup);
        assert!(!path.exists(), "key file removed on drop");
    }

    #[test]
    fn ssh_key_passphrase_is_rejected() {
        let auth = GitAuth {
            ssh_private_key_path: Some(PathBuf::from("/keys/id_ed25519")),
            password: Some("passphrase".to_string()),
            ..Default::default()
        };
        let err = auth_setup(Some(&auth)).unwrap_err();
        assert!(err.to_string().contains("passphrase"), "unexpected: {err}");
    }

    #[test]
    fn git_auth_debug_redacts_secrets() {
        let auth = GitAuth {
            personal_access_token: Some("hunter2".to_string()),
            password: Some("hunter3".to_string()),
            ssh_private_key: Some("KEY".to_string()),
            ..Default::default()
        };
        let debug = format!("{auth:?}");
        for secret in ["hunter2", "hunter3", "KEY"] {
            assert!(!debug.contains(secret), "leaked {secret}: {debug}");
        }
    }
}
