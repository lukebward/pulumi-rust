//! Error type for automation-API operations.

use std::fmt;

/// Everything a finished `pulumi` invocation left behind. Carried inside
/// [`Error`] so a caller can classify a failure from the raw streams the
/// same way the Go Automation API does.
#[derive(Debug, Clone, Default)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    /// The process exit code; [`UNKNOWN_ERR_CODE`] when the process died
    /// without one (killed by a signal, failed to spawn).
    pub code: i32,
}

/// The exit-code sentinel for "the CLI never produced an exit code",
/// mirroring the Go SDK's `unknownErrorCode`.
pub const UNKNOWN_ERR_CODE: i32 = -2;

/// The error type for automation-API operations.
///
/// A failure of the `pulumi` CLI itself is [`Error::Command`], which keeps
/// the captured streams so the classification predicates
/// ([`Error::is_concurrent_update_error`] and friends) can match on them.
/// Everything else — I/O, serialization, gRPC setup for inline programs —
/// is [`Error::Setup`].
#[derive(Debug)]
pub enum Error {
    /// A `pulumi` invocation failed, or its output could not be understood.
    Command {
        message: String,
        result: CommandResult,
    },
    /// A failure on the way to (or from) running the CLI.
    Setup(String),
}

/// Convenient result alias for automation-API operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn command(message: impl Into<String>, result: CommandResult) -> Self {
        Error::Command {
            message: message.into(),
            result,
        }
    }

    pub(crate) fn setup(message: impl Into<String>) -> Self {
        Error::Setup(message.into())
    }

    /// Wrap the error's message with an operation-level context prefix,
    /// keeping the captured CLI streams intact.
    pub(crate) fn with_context(self, context: &str) -> Error {
        match self {
            Error::Command { message, result } => Error::Command {
                message: format!("{context}: {message}"),
                result,
            },
            Error::Setup(message) => Error::Setup(format!("{context}: {message}")),
        }
    }

    /// The captured CLI streams, when this error came from a CLI run.
    pub fn command_result(&self) -> Option<&CommandResult> {
        match self {
            Error::Command { result, .. } => Some(result),
            Error::Setup(_) => None,
        }
    }

    fn stdout(&self) -> &str {
        self.command_result().map_or("", |r| &r.stdout)
    }

    fn stderr(&self) -> &str {
        self.command_result().map_or("", |r| &r.stderr)
    }

    /// Another update to this stack was already in progress.
    pub fn is_concurrent_update_error(&self) -> bool {
        // The service backend reports a 409; DIY backends report the lock.
        self.stderr()
            .contains("[409] Conflict: Another update is currently in progress.")
            || self.stderr().contains("the stack is currently locked by")
    }

    /// The selected stack does not exist.
    pub fn is_select_stack_404_error(&self) -> bool {
        contains_in_order(self.stderr(), &["no stack named", "found"])
    }

    /// A stack with this name already exists.
    pub fn is_create_stack_409_error(&self) -> bool {
        contains_in_order(self.stderr(), &["stack", "already exists"])
    }

    /// The program failed to compile. The patterns are the ones the Go SDK
    /// matches for each language runtime a local program might use.
    pub fn is_compilation_error(&self) -> bool {
        let stdout = self.stdout();
        // dotnet
        stdout.contains("Build FAILED.")
            // go — the Go SDK's own comment calls these "flimsy"
            || stdout.contains(": syntax error:")
            || stdout.contains(": undefined:")
            // typescript
            || stdout.contains("Unable to compile TypeScript")
    }

    /// The program compiled but failed at runtime.
    pub fn is_runtime_error(&self) -> bool {
        if self.is_compilation_error() {
            return false;
        }
        let stdout = self.stdout();
        stdout.contains("failed with an unhandled exception:")
            || stdout.contains("panic: runtime error:")
            || stdout.contains("an unhandled error occurred:")
            // An inline Rust program's failure is reported by the in-process
            // language server, so the marker can land in either stream.
            || self.to_string().contains("rust inline source runtime error")
    }

    /// The engine itself crashed.
    pub fn is_unexpected_engine_error(&self) -> bool {
        self.stdout()
            .contains("The Pulumi CLI encountered a fatal error. This is a bug!")
    }
}

/// Whether `haystack` contains every needle, each strictly after the
/// previous match — the same shape as the Go SDK's `no stack named.*found`
/// style regexes, without a regex dependency.
fn contains_in_order(haystack: &str, needles: &[&str]) -> bool {
    let mut rest = haystack;
    for needle in needles {
        match rest.find(needle) {
            Some(at) => rest = &rest[at + needle.len()..],
            None => return false,
        }
    }
    true
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The same shape as the Go SDK's autoError, so failures read
            // identically across the two Automation APIs.
            Error::Command { message, result } => write!(
                f,
                "{message}\ncode: {}\nstdout: {}\nstderr: {}\n",
                result.code, result.stdout, result.stderr
            ),
            Error::Setup(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::setup(format!("io error: {e}"))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::setup(format!("json error: {e}"))
    }
}

impl From<serde_yaml_ng::Error> for Error {
    fn from(e: serde_yaml_ng::Error) -> Self {
        Error::setup(format!("yaml error: {e}"))
    }
}

impl From<crate::error::Error> for Error {
    fn from(e: crate::error::Error) -> Self {
        Error::Setup(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_err(stdout: &str, stderr: &str) -> Error {
        Error::command(
            "failed to run update",
            CommandResult {
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
                code: 255,
            },
        )
    }

    #[test]
    fn classifies_concurrent_update() {
        let e = command_err(
            "",
            "error: [409] Conflict: Another update is currently in progress.",
        );
        assert!(e.is_concurrent_update_error());
        let e = command_err("", "error: the stack is currently locked by 1 lock(s)");
        assert!(e.is_concurrent_update_error());
        assert!(!command_err("", "some other error").is_concurrent_update_error());
    }

    #[test]
    fn classifies_stack_lifecycle_errors() {
        let e = command_err("", "error: no stack named 'dev' found");
        assert!(e.is_select_stack_404_error());
        assert!(!e.is_create_stack_409_error());

        let e = command_err("", "error: stack 'dev' already exists");
        assert!(e.is_create_stack_409_error());
        assert!(!e.is_select_stack_404_error());
    }

    #[test]
    fn classifies_compilation_and_runtime_errors() {
        assert!(command_err("Build FAILED.", "").is_compilation_error());
        assert!(command_err("main.go:2:1: syntax error: oops", "").is_compilation_error());
        assert!(command_err("Unable to compile TypeScript", "").is_compilation_error());

        let runtime = command_err("error: an unhandled error occurred: oh no", "");
        assert!(runtime.is_runtime_error());
        // A compilation error is not also a runtime error.
        let compile = command_err("Build FAILED. an unhandled error occurred:", "");
        assert!(!compile.is_runtime_error());
    }

    #[test]
    fn classifies_engine_errors() {
        let e = command_err(
            "The Pulumi CLI encountered a fatal error. This is a bug!",
            "",
        );
        assert!(e.is_unexpected_engine_error());
    }

    #[test]
    fn setup_errors_classify_as_nothing() {
        let e = Error::setup("no stack named x found stack already exists");
        assert!(!e.is_select_stack_404_error());
        assert!(!e.is_concurrent_update_error());
    }

    #[test]
    fn display_matches_go_shape() {
        let text = command_err("out", "err").to_string();
        assert_eq!(
            text,
            "failed to run update\ncode: 255\nstdout: out\nstderr: err\n"
        );
    }
}
