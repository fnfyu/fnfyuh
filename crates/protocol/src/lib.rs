//! Versioned wire types shared by the daemon, clients, and external adapters.
//!
//! This crate deliberately has no filesystem, process, or database dependency. The
//! event envelope is the stable seam; implementations may change behind it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const PROTOCOL_NAME: &str = "local-first-harness";
pub const PROTOCOL_VERSION: u32 = 1;
pub const EVENT_SCHEMA_VERSION: u16 = 2;

pub type SessionId = String;
pub type TurnId = String;
pub type RunId = String;
pub type OperationId = String;
pub type ArtifactId = String;

/// An immutable committed event. `sequence` is local to a session branch and
/// `global_sequence` is a database-wide cursor. Hashes are assigned by the event
/// store, never by an RPC caller.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventEnvelope {
    pub protocol: String,
    pub version: u32,
    pub schema_version: u16,
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub global_sequence: i64,
    pub recorded_at_ms: i64,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub prev_hash: String,
    pub hash: String,
    pub payload: EventPayload,
}

impl EventEnvelope {
    pub fn new(
        session_id: impl Into<String>,
        sequence: i64,
        payload: EventPayload,
        correlation_id: Option<String>,
        causation_id: Option<String>,
    ) -> Self {
        Self {
            protocol: PROTOCOL_NAME.to_string(),
            version: PROTOCOL_VERSION,
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            sequence,
            global_sequence: 0,
            recorded_at_ms: now_ms(),
            correlation_id,
            causation_id,
            prev_hash: String::new(),
            hash: String::new(),
            payload,
        }
    }

    pub fn event_type(&self) -> &'static str {
        self.payload.event_type()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModelParameters {
    /// Provider-neutral thinking/reasoning level. `auto` leaves the provider default in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Sampling temperature in the provider-neutral 0..=2 range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Maximum number of output tokens requested from the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

impl ModelParameters {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(level) = self.reasoning_effort.as_deref() {
            if !matches!(level, "auto" | "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max") {
                return Err(format!("unsupported reasoning_effort `{level}`"));
            }
        }
        if let Some(temperature) = self.temperature {
            if !temperature.is_finite() || !(0.0..=2.0).contains(&temperature) {
                return Err("temperature must be a finite number between 0 and 2".to_string());
            }
        }
        if let Some(max_output_tokens) = self.max_output_tokens {
            if !(16..=131_072).contains(&max_output_tokens) {
                return Err("max_output_tokens must be between 16 and 131072".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum EventPayload {
    SessionCreated {
        workspace_roots: Vec<String>,
        model: Option<String>,
    },
    SessionForked {
        source_session_id: SessionId,
        source_sequence: i64,
        source_hash: String,
    },
    TurnStarted {
        turn_id: TurnId,
        run_id: RunId,
    },
    UserMessage {
        turn_id: TurnId,
        content: String,
    },
    RunStarted {
        turn_id: TurnId,
        run_id: RunId,
    },
    ModelRequested {
        turn_id: TurnId,
        run_id: RunId,
        request_id: String,
        model: String,
        prompt_digest: String,
        #[serde(default)]
        parameters: ModelParameters,
    },
    ModelResponded {
        turn_id: TurnId,
        run_id: RunId,
        request_id: String,
        content: String,
        stop_reason: Option<String>,
    },
    ToolProposed {
        turn_id: TurnId,
        run_id: RunId,
        operation_id: OperationId,
        tool_name: String,
        intent: ToolIntent,
    },
    PolicyEvaluated {
        operation_id: OperationId,
        request_digest: String,
        policy_version: String,
        outcome: PolicyOutcome,
        reason: String,
    },
    ApprovalRequested {
        operation_id: OperationId,
        request_digest: String,
        reason: String,
    },
    ApprovalGranted {
        operation_id: OperationId,
        request_digest: String,
        actor: String,
        expires_at_ms: Option<i64>,
    },
    ApprovalDenied {
        operation_id: OperationId,
        request_digest: String,
        actor: String,
        reason: String,
    },
    ExecutionRequested {
        operation_id: OperationId,
        request_digest: String,
        backend: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_id: Option<String>,
    },
    ToolStarted {
        operation_id: OperationId,
        backend: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_id: Option<String>,
    },
    ToolFinished {
        operation_id: OperationId,
        result: ToolAuditResult,
    },
    ToolFailed {
        operation_id: OperationId,
        error_code: String,
        message: String,
    },
    BackendInspected {
        operation_id: OperationId,
        backend: String,
        #[serde(default)]
        external_id: Option<String>,
        state: BackendOperationState,
        detail: Option<String>,
        #[serde(default)]
        result: Option<ToolAuditResult>,
    },
    ExecutionContinuationRequested {
        operation_id: OperationId,
        backend: String,
        #[serde(default)]
        external_id: Option<String>,
    },
    ArtifactCreated {
        artifact_id: ArtifactId,
        operation_id: OperationId,
        kind: String,
        sha256: String,
        bytes: u64,
        media_type: String,
    },
    ExecutionUnknown {
        operation_id: OperationId,
        reason: String,
    },
    RecoveryRequired {
        operation_id: Option<OperationId>,
        reason: String,
    },
    RunCompleted {
        run_id: RunId,
        success: bool,
    },
    ContextCompacted {
        turn_id: TurnId,
        summary: String,
        summarized_through_sequence: i64,
    },
    CheckpointCreated {
        checkpoint_id: String,
        label: String,
        vcs_ref: Option<String>,
    },
    SessionCompleted {
        reason: String,
    },
}

impl EventPayload {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::SessionCreated { .. } => "session_created",
            Self::SessionForked { .. } => "session_forked",
            Self::TurnStarted { .. } => "turn_started",
            Self::UserMessage { .. } => "user_message",
            Self::RunStarted { .. } => "run_started",
            Self::ModelRequested { .. } => "model_requested",
            Self::ModelResponded { .. } => "model_responded",
            Self::ToolProposed { .. } => "tool_proposed",
            Self::PolicyEvaluated { .. } => "policy_evaluated",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalGranted { .. } => "approval_granted",
            Self::ApprovalDenied { .. } => "approval_denied",
            Self::ExecutionRequested { .. } => "execution_requested",
            Self::ToolStarted { .. } => "tool_started",
            Self::ToolFinished { .. } => "tool_finished",
            Self::ToolFailed { .. } => "tool_failed",
            Self::BackendInspected { .. } => "backend_inspected",
            Self::ExecutionContinuationRequested { .. } => "execution_continuation_requested",
            Self::ArtifactCreated { .. } => "artifact_created",
            Self::ExecutionUnknown { .. } => "execution_unknown",
            Self::RecoveryRequired { .. } => "recovery_required",
            Self::RunCompleted { .. } => "run_completed",
            Self::ContextCompacted { .. } => "context_compacted",
            Self::CheckpointCreated { .. } => "checkpoint_created",
            Self::SessionCompleted { .. } => "session_completed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackendOperationState {
    NotTracked,
    NotFound,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
    Allowed,
    NeedsApproval,
    Denied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "data")]
pub enum ToolIntent {
    ReadFile {
        path: String,
    },
    Search {
        root: String,
        query: String,
    },
    ListFiles {
        root: String,
        depth: u32,
        include_hidden: bool,
    },
    ReadImage {
        path: String,
        max_bytes: Option<u64>,
    },
    WriteFile {
        path: String,
        content: String,
    },
    EditFile {
        path: String,
        old_text: String,
        new_text: String,
        expected_sha256: Option<String>,
    },
    /// This is a direct program invocation, never a shell string. A local host
    /// adapter must be explicitly enabled because command allowlists alone are
    /// not an isolation boundary.
    Exec {
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
        timeout_ms: Option<u64>,
    },
    Git {
        args: Vec<String>,
        cwd: Option<String>,
    },
    Test {
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
        timeout_ms: Option<u64>,
    },
}

impl ToolIntent {
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::ReadFile { .. } | Self::Search { .. } | Self::ListFiles { .. } | Self::ReadImage { .. }
        )
    }

    pub fn operation_name(&self) -> &'static str {
        match self {
            Self::ReadFile { .. }
            | Self::Search { .. }
            | Self::ListFiles { .. }
            | Self::ReadImage { .. } => "workspace.read",
            Self::WriteFile { .. } | Self::EditFile { .. } => "workspace.write",
            Self::Exec { .. } | Self::Test { .. } => "process.run",
            Self::Git { .. } => "git.write",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    pub status: ToolStatus,
    pub exit_code: Option<i32>,
    /// Raw output may be returned to the immediate caller, but must not be put in
    /// the append-only event log. The coordinator converts it to `ToolAuditResult`.
    pub stdout: String,
    pub stderr: String,
    pub bytes_written: Option<u64>,
    pub output_truncated: bool,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_encoding: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolAuditResult {
    pub status: ToolStatus,
    pub exit_code: Option<i32>,
    pub stdout_digest: String,
    pub stderr_digest: String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub bytes_written: Option<u64>,
    pub output_truncated: bool,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_artifact_id: Option<ArtifactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_artifact_id: Option<ArtifactId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Succeeded,
    Failed,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub protocol_version: u32,
    pub kind: PluginKind,
    pub requested_capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum PluginKind {
    Provider,
    Tool,
    ExecutionBackend,
    Storage,
    ClientExtension,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    WorkspaceRead,
    WorkspaceWrite,
    ProcessRun,
    NetworkConnect,
    SecretsRead,
    McpCall,
    GitWrite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RpcId {
    Number(i64),
    String(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<RpcId>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: RpcId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn success(id: RpcId, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: RpcId, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

pub fn event_notification(event: &EventEnvelope) -> JsonRpcNotification {
    JsonRpcNotification {
        jsonrpc: "2.0".to_string(),
        method: "session/event".to_string(),
        params: serde_json::to_value(event).expect("event envelope is serializable"),
    }
}

/// Serialize with recursively sorted object keys. This is the small canonical JSON
/// contract shared with the TypeScript SDK; it is not a claim of full RFC 8785
/// number normalization, so protocol digests must avoid ambiguous floating values.
pub fn canonical_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let value = serde_json::to_value(value)?;
    serde_json::to_string(&canonicalize_value(value))
}

fn canonicalize_value(value: Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.into_iter().map(canonicalize_value).collect())
        }
        Value::Object(values) => {
            let sorted: BTreeMap<_, _> = values
                .into_iter()
                .map(|(key, value)| (key, canonicalize_value(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        other => other,
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_is_stable_and_payload_round_trips() {
        let payload = EventPayload::UserMessage {
            turn_id: "turn-1".into(),
            content: "hello".into(),
        };
        assert_eq!(payload.event_type(), "user_message");
        let encoded = serde_json::to_string(&payload).unwrap();
        let decoded: EventPayload = serde_json::from_str(&encoded).unwrap();
        assert_eq!(payload, decoded);
    }

    #[test]
    fn shell_metacharacters_are_data_in_exec_intent() {
        let intent = ToolIntent::Exec {
            program: "git".into(),
            args: vec!["status; echo escaped".into()],
            cwd: Some(".".into()),
            timeout_ms: Some(1000),
        };
        let encoded = serde_json::to_string(&intent).unwrap();
        assert!(encoded.contains("status; echo escaped"));
    }

    #[test]
    fn model_parameters_validate_and_round_trip() {
        let parameters = ModelParameters {
            reasoning_effort: Some("high".into()),
            temperature: Some(0.7),
            max_output_tokens: Some(4096),
        };
        parameters.validate().unwrap();
        let encoded = serde_json::to_string(&parameters).unwrap();
        assert_eq!(serde_json::from_str::<ModelParameters>(&encoded).unwrap(), parameters);
        assert!(ModelParameters { temperature: Some(2.1), ..Default::default() }.validate().is_err());
        assert!(ModelParameters { max_output_tokens: Some(8), ..Default::default() }.validate().is_err());
    }
}
