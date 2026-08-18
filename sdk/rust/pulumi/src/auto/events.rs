//! The engine's structured event stream: the JSON grammar every Pulumi
//! operation emits (one event per line of the `--event-log` file), and the
//! tailer that turns that file into a channel of typed events.
//!
//! The shapes mirror `apitype` in pulumi/pulumi, field for field; unknown
//! fields are ignored so a newer CLI never breaks an older SDK.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use super::errors::{Error, Result};

/// One engine event. Exactly one payload field is set.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EngineEvent {
    /// Unique, monotonically increasing position in the stream.
    #[serde(default)]
    pub sequence: i64,
    /// Unix seconds.
    #[serde(default)]
    pub timestamp: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_event: Option<CancelEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_event: Option<StdoutEngineEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_event: Option<DiagnosticEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prelude_event: Option<PreludeEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_event: Option<SummaryEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_pre_event: Option<ResourcePreEvent>,
    #[serde(rename = "resOutputsEvent", skip_serializing_if = "Option::is_none")]
    pub res_outputs_event: Option<ResOutputsEvent>,
    #[serde(rename = "resOpFailedEvent", skip_serializing_if = "Option::is_none")]
    pub res_op_failed_event: Option<ResOpFailedEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_event: Option<PolicyEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_remediation_event: Option<PolicyRemediationEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_load_event: Option<PolicyLoadEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_analyze_summary_event: Option<PolicyAnalyzeSummaryEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_remediate_summary_event: Option<PolicyRemediateSummaryEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_analyze_stack_summary_event: Option<PolicyAnalyzeStackSummaryEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_debugging_event: Option<StartDebuggingEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_event: Option<ProgressEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_event: Option<ErrorEvent>,
}

/// Emitted on cancellation — and also once on successful completion.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CancelEvent {}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StdoutEngineEvent {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub color: String,
    /// `info`, `info#err`, `warning` or `error`.
    #[serde(default)]
    pub severity: String,
    #[serde(rename = "streamID", default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreludeEvent {
    /// The stack's config, with encrypted values blinded.
    #[serde(default)]
    pub config: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SummaryEvent {
    #[serde(default)]
    pub maybe_corrupt: bool,
    #[serde(default)]
    pub duration_seconds: i64,
    /// Counts keyed by operation, e.g. `create` → 3.
    #[serde(default)]
    pub resource_changes: HashMap<OpType, i64>,
    /// PascalCase in the wire format, a backwards-compatibility lock-in.
    #[serde(rename = "PolicyPacks", default)]
    pub policy_packs: HashMap<String, String>,
    #[serde(default)]
    pub is_preview: bool,
    /// `succeeded`, `failed` or `canceled`; absent on older CLIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourcePreEvent {
    #[serde(default)]
    pub metadata: StepEventMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResOutputsEvent {
    #[serde(default)]
    pub metadata: StepEventMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResOpFailedEvent {
    #[serde(default)]
    pub metadata: StepEventMetadata,
    #[serde(default)]
    pub status: i32,
    #[serde(default)]
    pub steps: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PolicyEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_urn: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub policy_name: String,
    #[serde(default)]
    pub policy_pack_name: String,
    #[serde(default)]
    pub policy_pack_version: String,
    #[serde(default)]
    pub policy_pack_version_tag: String,
    /// `warning`, `mandatory`, `remediate` or `none`.
    #[serde(default)]
    pub enforcement_level: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRemediationEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_urn: Option<String>,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub policy_name: String,
    #[serde(default)]
    pub policy_pack_name: String,
    #[serde(default)]
    pub policy_pack_version: String,
    #[serde(default)]
    pub policy_pack_version_tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Map<String, Value>>,
}

/// A policy pack was loaded.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicyLoadEvent {}

/// Which policies of a pack passed and failed for one resource.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PolicyAnalyzeSummaryEvent {
    #[serde(default)]
    pub resource_urn: String,
    #[serde(default)]
    pub policy_pack_name: String,
    #[serde(default)]
    pub policy_pack_version: String,
    #[serde(default)]
    pub policy_pack_version_tag: String,
    #[serde(default)]
    pub passed: Vec<String>,
    #[serde(default)]
    pub failed: Vec<String>,
}

/// Which remediations of a pack ran for one resource.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRemediateSummaryEvent {
    #[serde(default)]
    pub resource_urn: String,
    #[serde(default)]
    pub policy_pack_name: String,
    #[serde(default)]
    pub policy_pack_version: String,
    #[serde(default)]
    pub policy_pack_version_tag: String,
    #[serde(default)]
    pub passed: Vec<String>,
    #[serde(default)]
    pub failed: Vec<String>,
}

/// Which stack-level policies of a pack passed and failed.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PolicyAnalyzeStackSummaryEvent {
    #[serde(default)]
    pub policy_pack_name: String,
    #[serde(default)]
    pub policy_pack_version: String,
    #[serde(default)]
    pub policy_pack_version_tag: String,
    #[serde(default)]
    pub passed: Vec<String>,
    #[serde(default)]
    pub failed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StartDebuggingEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    /// `plugin-download` or `plugin-install`.
    #[serde(rename = "type", default)]
    pub r#type: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub message: String,
    /// Bytes received so far.
    #[serde(rename = "received", default)]
    pub completed: i64,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub done: bool,
}

/// An internal engine error, surfaced for debugging.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ErrorEvent {
    #[serde(default)]
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StepEventMetadata {
    #[serde(default)]
    pub op: OpType,
    #[serde(default)]
    pub urn: String,
    #[serde(rename = "type", default)]
    pub r#type: String,
    pub old: Option<StepEventStateMetadata>,
    pub new: Option<StepEventStateMetadata>,
    /// Keys that caused a replacement; only on create/replace steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
    /// Keys whose values changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffs: Option<Vec<String>>,
    /// `None` (absent or `null`) and `Some(empty)` are deliberately
    /// distinct, as in the wire format.
    #[serde(default)]
    pub detailed_diff: Option<HashMap<String, PropertyDiff>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical: Option<bool>,
    #[serde(default)]
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StepEventStateMetadata {
    #[serde(rename = "type", default)]
    pub r#type: String,
    #[serde(default)]
    pub urn: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<bool>,
    /// Pending deletion after a replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<bool>,
    /// Blank until the resource has been created.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protect: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_on_delete: Option<bool>,
    /// Secrets are filtered out; large assets appear as hashes.
    #[serde(default)]
    pub inputs: serde_json::Map<String, Value>,
    #[serde(default)]
    pub outputs: serde_json::Map<String, Value>,
    #[serde(default)]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_errors: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PropertyDiff {
    /// `add`, `add-replace`, `delete`, `delete-replace`, `update` or
    /// `update-replace`.
    #[serde(default)]
    pub diff_kind: String,
    /// The diff compares old inputs to new inputs, rather than old state
    /// to new inputs.
    #[serde(default)]
    pub input_diff: bool,
}

/// The kind of step the engine performed on a resource. Open-ended: values
/// a newer engine adds land in [`OpType::Other`] rather than failing to
/// parse.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum OpType {
    #[default]
    Same,
    Create,
    Update,
    Delete,
    Replace,
    CreateReplacement,
    DeleteReplaced,
    Read,
    ReadReplacement,
    Refresh,
    ReadDiscard,
    DiscardReplaced,
    RemovePendingReplace,
    Import,
    ImportReplacement,
    Other(String),
}

impl OpType {
    pub fn as_str(&self) -> &str {
        match self {
            OpType::Same => "same",
            OpType::Create => "create",
            OpType::Update => "update",
            OpType::Delete => "delete",
            OpType::Replace => "replace",
            OpType::CreateReplacement => "create-replacement",
            OpType::DeleteReplaced => "delete-replaced",
            OpType::Read => "read",
            OpType::ReadReplacement => "read-replacement",
            OpType::Refresh => "refresh",
            OpType::ReadDiscard => "discard",
            OpType::DiscardReplaced => "discard-replaced",
            OpType::RemovePendingReplace => "remove-pending-replace",
            OpType::Import => "import",
            OpType::ImportReplacement => "import-replacement",
            OpType::Other(s) => s,
        }
    }
}

impl From<&str> for OpType {
    fn from(s: &str) -> Self {
        match s {
            "same" => OpType::Same,
            "create" => OpType::Create,
            "update" => OpType::Update,
            "delete" => OpType::Delete,
            "replace" => OpType::Replace,
            "create-replacement" => OpType::CreateReplacement,
            "delete-replaced" => OpType::DeleteReplaced,
            "read" => OpType::Read,
            "read-replacement" => OpType::ReadReplacement,
            "refresh" => OpType::Refresh,
            "discard" => OpType::ReadDiscard,
            "discard-replaced" => OpType::DiscardReplaced,
            "remove-pending-replace" => OpType::RemovePendingReplace,
            "import" => OpType::Import,
            "import-replacement" => OpType::ImportReplacement,
            other => OpType::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for OpType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for OpType {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for OpType {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(OpType::from(s.as_str()))
    }
}

/// A live tail of one operation's `--event-log` file.
///
/// The CLI appends one JSON event per line; the watcher parses each new
/// line and forwards it to every sender. When the stream ends the senders
/// are dropped, which closes the corresponding receivers — the same
/// contract as the Go SDK closing its channels. Lines that fail to parse
/// (which indicates CLI/SDK version skew) are skipped.
pub(crate) struct EventLogWatcher {
    dir: PathBuf,
    path: PathBuf,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl EventLogWatcher {
    /// Create the log directory and start tailing `eventlog.txt` inside it.
    pub(crate) fn start(command: &str, senders: Vec<UnboundedSender<EngineEvent>>) -> Result<Self> {
        let dir = super::scratch_dir(&format!("automation-logs-{command}"))?;
        let path = dir.join("eventlog.txt");
        let (stop, stop_rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(tail(path.clone(), senders, stop_rx));
        Ok(EventLogWatcher {
            dir,
            path,
            stop,
            task,
        })
    }

    /// The path handed to the CLI as `--event-log`.
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Drain the log to EOF, stop the tail, and remove the log directory.
    /// Call after the CLI has exited so every event has been written.
    pub(crate) async fn close(mut self) {
        let _ = self.stop.send(true);
        let _ = (&mut self.task).await;
        // Drop removes the log directory.
    }
}

/// A watcher dropped without [`EventLogWatcher::close`] — an operation
/// future cancelled mid-run — must not leave the tail task or the log
/// directory behind.
impl Drop for EventLogWatcher {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn tail(
    path: PathBuf,
    senders: Vec<UnboundedSender<EngineEvent>>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut pos: u64 = 0;
    let mut pending = String::new();
    let mut stop_closed = false;
    loop {
        // A closed stop channel means the watcher was dropped without
        // close(); finish the current drain and exit rather than spin.
        let stopping = *stop.borrow() || stop_closed;
        let mut progressed = false;
        if let Ok(mut file) = tokio::fs::File::open(&path).await {
            if file.seek(std::io::SeekFrom::Start(pos)).await.is_ok() {
                let mut chunk = String::new();
                if let Ok(n) = file.read_to_string(&mut chunk).await {
                    if n > 0 {
                        pos += n as u64;
                        pending.push_str(&chunk);
                        progressed = true;
                        // Only complete lines are events; keep any trailing
                        // partial line for the next read.
                        while let Some(nl) = pending.find('\n') {
                            let line: String = pending.drain(..=nl).collect();
                            deliver(line.trim(), &senders);
                        }
                    }
                }
            }
        }
        if stopping && !progressed {
            // The CLI has exited and we have drained to EOF. A final
            // unterminated line would mean the CLI was killed mid-write;
            // deliver it if it happens to be complete JSON.
            deliver(pending.trim(), &senders);
            return;
        }
        // Wake early when asked to stop; otherwise poll.
        let sleep = tokio::time::sleep(std::time::Duration::from_millis(50));
        tokio::select! {
            _ = sleep => {}
            changed = stop.changed() => {
                if changed.is_err() {
                    stop_closed = true;
                }
            }
        }
    }
}

fn deliver(line: &str, senders: &[UnboundedSender<EngineEvent>]) {
    if line.is_empty() {
        return;
    }
    if let Ok(event) = serde_json::from_str::<EngineEvent>(line) {
        for sender in senders {
            // A dropped receiver only means that subscriber lost interest.
            let _ = sender.send(event.clone());
        }
    }
}

#[allow(dead_code)]
fn _error_is_send_sync(e: Error) -> impl Send + Sync {
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_summary_event() {
        let line = r#"{"sequence":5,"timestamp":1700000000,"summaryEvent":{"maybeCorrupt":false,"durationSeconds":2,"resourceChanges":{"create":2,"same":1},"PolicyPacks":{},"isPreview":true,"result":"succeeded"}}"#;
        let event: EngineEvent = serde_json::from_str(line).unwrap();
        let summary = event.summary_event.unwrap();
        assert_eq!(summary.resource_changes[&OpType::Create], 2);
        assert_eq!(summary.resource_changes[&OpType::Same], 1);
        assert!(summary.is_preview);
        assert_eq!(summary.result.as_deref(), Some("succeeded"));
    }

    #[test]
    fn parses_resource_pre_event_with_unknown_fields() {
        let line = r#"{"sequence":2,"timestamp":1700000000,"resourcePreEvent":{"metadata":{"op":"create","urn":"urn:pulumi:dev::p::t::n","type":"t","old":null,"new":{"type":"t","urn":"urn:pulumi:dev::p::t::n","id":"","parent":"","inputs":{"a":1},"outputs":{},"provider":""},"detailedDiff":null,"provider":"","brandNewField":42}}}"#;
        let event: EngineEvent = serde_json::from_str(line).unwrap();
        let pre = event.resource_pre_event.unwrap();
        assert_eq!(pre.metadata.op, OpType::Create);
        assert!(pre.metadata.old.is_none());
        assert_eq!(pre.metadata.new.unwrap().inputs["a"], serde_json::json!(1));
        assert!(pre.metadata.detailed_diff.is_none());
    }

    #[test]
    fn parses_policy_events() {
        let line = r#"{"sequence":1,"timestamp":1,"policyLoadEvent":{}}"#;
        let event: EngineEvent = serde_json::from_str(line).unwrap();
        assert!(event.policy_load_event.is_some());

        let line = r#"{"sequence":2,"timestamp":1,"policyAnalyzeSummaryEvent":{"resourceUrn":"urn:x","policyPackName":"pack","policyPackVersion":"1.0.0","policyPackVersionTag":"v1.0.0","passed":["a"],"failed":["b"]}}"#;
        let event: EngineEvent = serde_json::from_str(line).unwrap();
        let summary = event.policy_analyze_summary_event.unwrap();
        assert_eq!(summary.passed, ["a"]);
        assert_eq!(summary.failed, ["b"]);

        let line = r#"{"sequence":3,"timestamp":1,"policyAnalyzeStackSummaryEvent":{"policyPackName":"pack","policyPackVersion":"1.0.0","policyPackVersionTag":"v1.0.0"}}"#;
        let event: EngineEvent = serde_json::from_str(line).unwrap();
        assert!(event.policy_analyze_stack_summary_event.is_some());
    }

    #[test]
    fn op_type_round_trips_unknown_values() {
        let op: OpType = serde_json::from_str("\"discard\"").unwrap();
        assert_eq!(op, OpType::ReadDiscard);
        let op: OpType = serde_json::from_str("\"quantum-shuffle\"").unwrap();
        assert_eq!(op, OpType::Other("quantum-shuffle".to_string()));
        assert_eq!(serde_json::to_string(&op).unwrap(), "\"quantum-shuffle\"");
    }

    #[test]
    fn parses_diagnostic_and_progress_events() {
        let line = r#"{"sequence":1,"timestamp":1,"diagnosticEvent":{"message":"hi","color":"raw","severity":"info#err","streamID":3}}"#;
        let event: EngineEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.diagnostic_event.unwrap().stream_id, Some(3));

        let line = r#"{"sequence":1,"timestamp":1,"progressEvent":{"type":"plugin-download","id":"aws","message":"m","received":10,"total":100,"done":false}}"#;
        let event: EngineEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.progress_event.unwrap().completed, 10);
    }

    #[tokio::test]
    async fn watcher_tails_and_closes_channels() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let watcher = EventLogWatcher::start("test", vec![tx]).unwrap();
        let path = watcher.path().to_path_buf();

        tokio::fs::write(
            &path,
            "{\"sequence\":1,\"timestamp\":1,\"cancelEvent\":{}}\nnot json\n{\"sequence\":2,\"timestamp\":2,\"summaryEvent\":{\"maybeCorrupt\":false,\"durationSeconds\":0,\"resourceChanges\":{},\"PolicyPacks\":{},\"isPreview\":false}}\n",
        )
        .await
        .unwrap();
        watcher.close().await;

        let first = rx.recv().await.expect("first event");
        assert!(first.cancel_event.is_some());
        let second = rx.recv().await.expect("second event");
        assert!(second.summary_event.is_some());
        // The malformed line was skipped, and the channel is closed.
        assert!(rx.recv().await.is_none());
        assert!(!path.exists());
    }
}
