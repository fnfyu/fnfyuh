use harness_agent_runtime::{
    AgentRuntime, AnthropicProvider, CodexOAuthProvider, ModelAttachment, ModelProvider,
    ModelRequest, ModelResponse, OpenAiCompatibleProvider, ToolCoordinator, TurnCancellation,
};
use harness_execution_broker::{
    ApprovalGrant, ContainerBackendConfig, ExecutionAdapter, ExecutionBroker,
    LocalRestrictedBackend, UnavailableContainerBackend,
};
use harness_policy_engine::{ApprovalMode, PolicyConfig, PolicyEngine};
use harness_prompt_compiler::{compile, PromptLayers, ToolDefinition};
use harness_protocol::{
    canonical_json, event_notification, now_ms, EventEnvelope, JsonRpcRequest, JsonRpcResponse,
    EventPayload, ModelParameters, RpcId, ToolStatus,
};
use harness_session_engine::{
    CommandClaim, CommandHead, CommandReceipt, CommandState, PureCommandResult, SessionEngine,
    SessionError, SqliteEventStore,
};
use harness_settings::{HarnessSettings, ProviderKind, SettingsError, SettingsStore};
use serde::Deserialize;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

const JSON_RPC_INVALID_REQUEST: i32 = -32600;
const JSON_RPC_METHOD_NOT_FOUND: i32 = -32601;
const JSON_RPC_INVALID_PARAMS: i32 = -32602;
const JSON_RPC_INTERNAL_ERROR: i32 = -32603;
const JSON_RPC_COMMAND_PENDING: i32 = -32012;
const JSON_RPC_IDEMPOTENCY_CONFLICT: i32 = -32013;
const JSON_RPC_HEAD_CONFLICT: i32 = -32010;
const MAX_ARTIFACT_CONTENT_SIZE: usize = 1_500_000;
const MAX_MODEL_ATTACHMENT_CONTENT_SIZE: usize = 1_100_000;

#[derive(Debug)]
enum AppError {
    InvalidParams(String),
    MethodNotFound(String),
    Session(SessionError),
    Settings(SettingsError),
    Agent(String),
}

impl From<SessionError> for AppError {
    fn from(value: SessionError) -> Self {
        Self::Session(value)
    }
}

impl From<SettingsError> for AppError {
    fn from(value: SettingsError) -> Self {
        Self::Settings(value)
    }
}

impl From<harness_agent_runtime::AgentError> for AppError {
    fn from(value: harness_agent_runtime::AgentError) -> Self {
        Self::Agent(value.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct CreateParams {
    #[serde(default)]
    workspace_roots: Vec<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionParams {
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct OperationInspectParams {
    session_id: String,
    operation_id: String,
}

#[derive(Debug, Deserialize)]
struct OperationContinueParams {
    session_id: String,
    operation_id: String,
    principal: String,
    approval: Option<ApprovalParams>,
}

#[derive(Debug, Deserialize)]
struct ArtifactGetParams {
    session_id: String,
    artifact_id: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ArtifactPutParams {
    session_id: String,
    operation_id: Option<String>,
    kind: String,
    content: String,
    media_type: String,
}

#[derive(Debug, Deserialize)]
struct ArtifactListParams {
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct OutboxClaimParams {
    subscription_id: String,
    #[serde(default = "default_event_limit")]
    limit: usize,
    #[serde(default = "default_lease_ms")]
    lease_ms: i64,
}

#[derive(Debug, Deserialize)]
struct OutboxNackParams {
    subscription_id: String,
    message_id: i64,
    error: String,
    #[serde(default)]
    retry_after_ms: i64,
}

#[derive(Debug, Deserialize)]
struct SessionEventsParams {
    session_id: String,
    after_global_sequence: Option<i64>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ForkParams {
    source_session_id: String,
    source_sequence: i64,
}

#[derive(Debug, Deserialize)]
struct TurnStartParams {
    session_id: String,
    content: String,
    turn_id: Option<String>,
    model: Option<String>,
    #[serde(default)]
    parameters: ModelParameters,
    #[serde(default)]
    attachments: Vec<ModelAttachment>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnStopParams {
    session_id: String,
    turn_id: String,
    run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PromptParams {
    session_id: String,
    layers: Option<PromptLayers>,
    tools: Option<Vec<ToolDefinition>>,
}

#[derive(Debug, Deserialize)]
struct RecoverParams {
    session_id: String,
    operation_id: Option<String>,
    reason: String,
    target_command_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApprovalParams {
    request_digest: String,
    actor: String,
    nonce: String,
    expires_at_ms: i64,
}

#[derive(Debug, Deserialize)]
struct ToolExecuteParams {
    session_id: String,
    turn_id: String,
    run_id: String,
    principal: String,
    operation_id: String,
    tool_name: String,
    intent: harness_protocol::ToolIntent,
    approval: Option<ApprovalParams>,
    backend: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EventReplayParams {
    #[serde(default)]
    after_global_sequence: i64,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct CommandMeta {
    command_id: String,
    request_digest: String,
    session_id: Option<String>,
    expected_head: Option<CommandHead>,
}

#[derive(Debug, Deserialize)]
struct SubscribeParams {
    session_id: Option<String>,
    subscription_id: Option<String>,
    #[serde(default)]
    after_global_sequence: i64,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct SubscriptionAckParams {
    subscription_id: String,
    after_global_sequence: i64,
}

#[derive(Debug, Deserialize)]
struct SubscriptionParams {
    subscription_id: String,
}

fn default_event_limit() -> usize {
    100
}

fn default_lease_ms() -> i64 {
    60_000
}

fn supported_image_media_type(media_type: &str) -> bool {
    matches!(media_type, "image/png" | "image/jpeg" | "image/webp" | "image/gif")
}

fn default_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a UTF-8 file relative to the workspace.".into(),
            input_schema: r#"{"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}"#.into(),
        },
        ToolDefinition {
            name: "search".into(),
            description: "Search text below a workspace-relative root.".into(),
            input_schema: r#"{"type":"object","required":["root","query"],"properties":{"root":{"type":"string"},"query":{"type":"string"}}}"#.into(),
        },
        ToolDefinition {
            name: "list_files".into(),
            description: "List workspace files and directories without following symlinks.".into(),
            input_schema: r#"{"type":"object","required":["root"],"properties":{"root":{"type":"string"},"depth":{"type":"integer","minimum":0,"maximum":8},"include_hidden":{"type":"boolean"}}}"#.into(),
        },
        ToolDefinition {
            name: "read_image".into(),
            description: "Read a bounded supported image as a protected base64 media artifact.".into(),
            input_schema: r#"{"type":"object","required":["path"],"properties":{"path":{"type":"string"},"max_bytes":{"type":"integer","maximum":1048576}}}"#.into(),
        },
        ToolDefinition {
            name: "write_file".into(),
            description: "Write UTF-8 content after explicit approval.".into(),
            input_schema: r#"{"type":"object","required":["path","content"],"properties":{"path":{"type":"string"},"content":{"type":"string"}}}"#.into(),
        },
        ToolDefinition {
            name: "edit_file".into(),
            description: "Replace one exact file span after explicit approval.".into(),
            input_schema: r#"{"type":"object","required":["path","old_text","new_text"],"properties":{"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},"expected_sha256":{"type":"string"}}}"#.into(),
        },
        ToolDefinition {
            name: "exec".into(),
            description: "Run an allowlisted absolute program without a shell.".into(),
            input_schema: r#"{"type":"object","required":["program","args"],"properties":{"program":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"cwd":{"type":"string"},"timeout_ms":{"type":"integer"}}}"#.into(),
        },
        ToolDefinition {
            name: "test".into(),
            description: "Run tests with an allowlisted absolute program without a shell.".into(),
            input_schema: r#"{"type":"object","required":["program","args"],"properties":{"program":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"cwd":{"type":"string"},"timeout_ms":{"type":"integer"}}}"#.into(),
        },
        ToolDefinition {
            name: "git".into(),
            description: "Run a typed git argument vector.".into(),
            input_schema: r#"{"type":"object","required":["args"],"properties":{"args":{"type":"array","items":{"type":"string"}},"cwd":{"type":"string"}}}"#.into(),
        },
    ]
}

#[derive(Debug, Clone)]
struct DaemonConfig {
    execution_backend: String,
    container: ContainerBackendConfig,
}

impl DaemonConfig {
    fn from_env() -> Self {
        Self {
            execution_backend: env::var("HARNESS_EXECUTION_BACKEND")
                .unwrap_or_else(|_| "local-trusted-host".to_string()),
            container: ContainerBackendConfig::from_env(),
        }
    }
}

enum SelectedProvider {
    Http(OpenAiCompatibleProvider),
    Anthropic(AnthropicProvider),
    Codex(CodexOAuthProvider),
}

impl ModelProvider for SelectedProvider {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, harness_agent_runtime::AgentError> {
        match self {
            Self::Http(provider) => provider.complete(request),
            Self::Anthropic(provider) => provider.complete(request),
            Self::Codex(provider) => provider.complete(request),
        }
    }
}

fn configured_provider(
    settings: &SettingsStore,
    selected_model: &str,
) -> Result<SelectedProvider, AppError> {
    let configured = settings.load()?;
    if let Some((provider, model)) = configured.model(selected_model) {
        if !provider.enabled || !model.enabled {
            return Err(AppError::Agent(format!("configured model `{selected_model}` is disabled")));
        }
        let api_key = provider
            .api_key_env
            .as_deref()
            .and_then(|name| env::var(name).ok());
        let timeout_ms = env::var("HARNESS_MODEL_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(120_000);
        return match &provider.kind {
            ProviderKind::OpenAiCompatible => Ok(SelectedProvider::Http(
                OpenAiCompatibleProvider::new(
                    provider.endpoint.clone().unwrap_or_default(),
                    api_key,
                    timeout_ms,
                )
                .map_err(|error| AppError::Agent(error.to_string()))?,
            )),
            ProviderKind::Anthropic => Ok(SelectedProvider::Anthropic(
                AnthropicProvider::new(
                    provider.endpoint.clone().unwrap_or_default(),
                    api_key,
                    timeout_ms,
                )
                .map_err(|error| AppError::Agent(error.to_string()))?,
            )),
            ProviderKind::OpenAiCodex => {
                let node_command = env::var("HARNESS_CODEX_NODE").unwrap_or_else(|_| "node".to_string());
                let bridge = env::var("HARNESS_CODEX_BRIDGE")
                    .unwrap_or_else(|_| "apps/gateway/codex-bridge.mjs".to_string());
                let codex_timeout_ms = env::var("HARNESS_CODEX_TIMEOUT_MS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(120_000);
                Ok(SelectedProvider::Codex(
                    CodexOAuthProvider::new(node_command, bridge, codex_timeout_ms)
                        .map_err(|error| AppError::Agent(error.to_string()))?,
                ))
            }
        };
    }
    Err(AppError::Agent(format!(
        "configured model `{selected_model}` is not available; add it to provider settings"
    )))
}

fn main() -> anyhow::Result<()> {
    let database = env::var_os("HARNESS_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".runtime/events.sqlite"));
    if let Some(parent) = database.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let store = SqliteEventStore::open(&database)?;
    let engine = SessionEngine::new(store);
    let settings_path = env::var_os("HARNESS_SETTINGS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            database
                .parent()
                .map(|parent| parent.join("fnfyuh-settings.json"))
                .unwrap_or_else(|| PathBuf::from("fnfyuh-settings.json"))
        });
    let settings = SettingsStore::new(settings_path);
    let config = DaemonConfig::from_env();
    let cancellation = Arc::new(TurnCancellation::default());
    let stdout = Arc::new(Mutex::new(io::BufWriter::new(io::stdout())));
    let (input_tx, input_rx) = mpsc::channel::<io::Result<String>>();
    let reader_cancellation = cancellation.clone();
    let reader_stdout = stdout.clone();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(&line) {
                        if request.method == "runtime.v1.turn.stop" {
                            let response = match (request.id, request.jsonrpc == "2.0", parse_params::<TurnStopParams>(&request.params)) {
                                (Some(id), true, Ok(params)) if !params.session_id.is_empty() && !params.turn_id.is_empty() && params.run_id.as_deref() != Some("") => {
                                    let accepted = reader_cancellation.request(&params.session_id, &params.turn_id, params.run_id.as_deref());
                                    JsonRpcResponse::success(id, json!({"accepted": accepted, "stopped": false}))
                                }
                                (Some(id), true, _) => JsonRpcResponse::failure(id, JSON_RPC_INVALID_PARAMS, "stop requires session_id, turn_id and optional nonempty run_id"),
                                (Some(id), false, _) => JsonRpcResponse::failure(id, JSON_RPC_INVALID_REQUEST, "jsonrpc must be 2.0"),
                                (None, _, _) => continue,
                            };
                            if write_json(&mut *reader_stdout.lock().unwrap(), &response).is_err() { break; }
                            continue;
                        }
                    }
                    if input_tx.send(Ok(line)).is_err() { break; }
                }
                Err(error) => { let _ = input_tx.send(Err(error)); break; }
            }
        }
    });

    for line in input_rx {
        let line = line?;
        let mut stdout = SharedOutput(&stdout);
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<JsonRpcRequest>(&line);
        let request = match parsed {
            Ok(request) => request,
            Err(error) => {
                write_json(
                    &mut stdout,
                    &JsonRpcResponse::failure(
                        RpcId::String("parse-error".into()),
                        JSON_RPC_INVALID_REQUEST,
                        error.to_string(),
                    ),
                )?;
                continue;
            }
        };
        if request.jsonrpc != "2.0" {
            if let Some(id) = request.id {
                write_json(
                    &mut stdout,
                    &JsonRpcResponse::failure(id, JSON_RPC_INVALID_REQUEST, "jsonrpc must be 2.0"),
                )?;
            }
            continue;
        }
        let id = request.id.clone();
        let command_meta = match command_meta(&request) {
            Ok(meta) => meta,
            Err(error) => {
                if let Some(id) = id {
                    let (code, message) = app_error(error);
                    write_json(&mut stdout, &JsonRpcResponse::failure(id, code, message))?;
                }
                continue;
            }
        };
        if request.method == "runtime.v1.session.create" {
            if let Some(meta) = &command_meta {
                let params: CreateParams = match parse_params(&request.params) {
                    Ok(params) => params,
                    Err(error) => {
                        if let Some(id) = id {
                            let (code, message) = app_error(error);
                            write_json(
                                &mut stdout,
                                &JsonRpcResponse::failure(id, code, message),
                            )?;
                        }
                        stdout.flush()?;
                        continue;
                    }
                };
                let outcome = engine.create_session_command(
                    &meta.command_id,
                    &meta.request_digest,
                    params.workspace_roots,
                    params.model,
                );
                match outcome {
                    Ok(PureCommandResult::New { receipt, events }) => {
                        if let Some(id) = id {
                            let result = receipt.result.clone().unwrap_or(Value::Null);
                            write_json(
                                &mut stdout,
                                &JsonRpcResponse::success(
                                    id,
                                    attach_receipt(result, receipt, false),
                                ),
                            )?;
                        }
                        for event in events {
                            write_json(&mut stdout, &event_notification(&event))?;
                        }
                    }
                    Ok(PureCommandResult::Existing(receipt)) => match &receipt.state {
                        CommandState::Committed => {
                            if let Some(id) = id {
                                let result = receipt.result.clone().unwrap_or(Value::Null);
                                write_json(
                                    &mut stdout,
                                    &JsonRpcResponse::success(
                                        id,
                                        attach_receipt(result, receipt, true),
                                    ),
                                )?;
                            }
                        }
                        CommandState::Rejected | CommandState::Aborted => {
                            if let Some(id) = id {
                                let (code, message) = receipt_error(&receipt);
                                write_json(
                                    &mut stdout,
                                    &JsonRpcResponse::failure(id, code, message),
                                )?;
                            }
                        }
                        CommandState::Pending => {
                            if let Some(id) = id {
                                write_json(
                                    &mut stdout,
                                    &JsonRpcResponse::failure(
                                        id,
                                        JSON_RPC_COMMAND_PENDING,
                                        "command is pending recovery; retry after explicit recovery",
                                    ),
                                )?;
                            }
                        }
                    },
                    Err(error) => {
                        if let Some(id) = id {
                            let (code, message) = app_error(AppError::Session(error));
                            write_json(&mut stdout, &JsonRpcResponse::failure(id, code, message))?;
                        }
                    }
                }
                stdout.flush()?;
                continue;
            }
        }

        if request.method == "runtime.v1.session.fork" {
            if let Some(meta) = &command_meta {
                let params: ForkParams = match parse_params(&request.params) {
                    Ok(params) => params,
                    Err(error) => {
                        if let Some(id) = id {
                            let (code, message) = app_error(error);
                            write_json(
                                &mut stdout,
                                &JsonRpcResponse::failure(id, code, message),
                            )?;
                        }
                        stdout.flush()?;
                        continue;
                    }
                };
                let outcome = engine.fork_session_command(
                    &meta.command_id,
                    &meta.request_digest,
                    &params.source_session_id,
                    params.source_sequence,
                    meta.expected_head.clone(),
                );
                match outcome {
                    Ok(PureCommandResult::New { receipt, events }) => {
                        if let Some(id) = id {
                            let result = receipt.result.clone().unwrap_or(Value::Null);
                            write_json(
                                &mut stdout,
                                &JsonRpcResponse::success(
                                    id,
                                    attach_receipt(result, receipt, false),
                                ),
                            )?;
                        }
                        for event in events {
                            write_json(&mut stdout, &event_notification(&event))?;
                        }
                    }
                    Ok(PureCommandResult::Existing(receipt)) => match &receipt.state {
                        CommandState::Committed => {
                            if let Some(id) = id {
                                let result = receipt.result.clone().unwrap_or(Value::Null);
                                write_json(
                                    &mut stdout,
                                    &JsonRpcResponse::success(
                                        id,
                                        attach_receipt(result, receipt, true),
                                    ),
                                )?;
                            }
                        }
                        CommandState::Rejected | CommandState::Aborted => {
                            if let Some(id) = id {
                                let (code, message) = receipt_error(&receipt);
                                write_json(
                                    &mut stdout,
                                    &JsonRpcResponse::failure(id, code, message),
                                )?;
                            }
                        }
                        CommandState::Pending => {
                            if let Some(id) = id {
                                write_json(
                                    &mut stdout,
                                    &JsonRpcResponse::failure(
                                        id,
                                        JSON_RPC_COMMAND_PENDING,
                                        "command is pending recovery; retry after explicit recovery",
                                    ),
                                )?;
                            }
                        }
                    },
                    Err(SessionError::ExpectedHeadMismatch { .. }) => {
                        if let Some(id) = id {
                            write_json(
                                &mut stdout,
                                &JsonRpcResponse::failure(
                                    id,
                                    JSON_RPC_HEAD_CONFLICT,
                                    "expected session head does not match current head",
                                ),
                            )?;
                        }
                    }
                    Err(error) => {
                        if let Some(id) = id {
                            let (code, message) = app_error(AppError::Session(error));
                            write_json(&mut stdout, &JsonRpcResponse::failure(id, code, message))?;
                        }
                    }
                }
                stdout.flush()?;
                continue;
            }
        }

        if request.method == "runtime.v1.session.recover" {
            if let Some(meta) = &command_meta {
                let params: RecoverParams = match parse_params(&request.params) {
                    Ok(params) => params,
                    Err(error) => {
                        if let Some(id) = id {
                            let (code, message) = app_error(error);
                            write_json(
                                &mut stdout,
                                &JsonRpcResponse::failure(id, code, message),
                            )?;
                        }
                        stdout.flush()?;
                        continue;
                    }
                };
                let target_command_id = params.target_command_id.clone();
                let outcome = engine.recover_command(
                    &meta.command_id,
                    &meta.request_digest,
                    &params.session_id,
                    params.operation_id,
                    params.reason,
                    meta.expected_head.clone(),
                    target_command_id.as_deref(),
                );
                let response = match outcome {
                    Ok(outcome) => pure_command_outcome(outcome),
                    Err(error) => Err((session_error_code(&error), error.to_string())),
                };
                match response {
                    Ok((result, events)) => {
                        if let Some(id) = id {
                            write_json(&mut stdout, &JsonRpcResponse::success(id, result))?;
                        }
                        for event in events {
                            write_json(&mut stdout, &event_notification(&event))?;
                        }
                    }
                    Err((code, message)) => {
                        if let Some(id) = id {
                            write_json(&mut stdout, &JsonRpcResponse::failure(id, code, message))?;
                        }
                    }
                }
                stdout.flush()?;
                continue;
            }
        }

        if let Some(meta) = &command_meta {
            match engine.claim_command(
                &meta.command_id,
                &request.method,
                &meta.request_digest,
                meta.session_id.as_deref(),
                meta.expected_head.as_ref(),
            ) {
                Ok(CommandClaim::New) => {}
                Ok(CommandClaim::Existing(receipt)) => {
                    if let Some(id) = id {
                        match &receipt.state {
                            CommandState::Committed => {
                                let result = receipt.result.clone().unwrap_or(Value::Null);
                                write_json(
                                    &mut stdout,
                                    &JsonRpcResponse::success(
                                        id,
                                        attach_receipt(result, receipt, true),
                                    ),
                                )?;
                            }
                            CommandState::Rejected | CommandState::Aborted => {
                                let (code, message) = receipt_error(&receipt);
                                write_json(
                                    &mut stdout,
                                    &JsonRpcResponse::failure(id, code, message),
                                )?;
                            }
                            CommandState::Pending => {
                                write_json(
                                    &mut stdout,
                                    &JsonRpcResponse::failure(
                                        id,
                                        JSON_RPC_COMMAND_PENDING,
                                        "command is pending recovery; retry after explicit recovery",
                                    ),
                                )?;
                            }
                        }
                    }
                    continue;
                }
                Err(SessionError::ExpectedHeadMismatch { .. }) => {
                    if let Some(id) = id {
                        write_json(
                            &mut stdout,
                            &JsonRpcResponse::failure(
                                id,
                                JSON_RPC_HEAD_CONFLICT,
                                "expected session head does not match current head",
                            ),
                        )?;
                    }
                    continue;
                }
                Err(SessionError::CommandIdempotencyConflict) => {
                    if let Some(id) = id {
                        write_json(
                            &mut stdout,
                            &JsonRpcResponse::failure(
                                id,
                                JSON_RPC_IDEMPOTENCY_CONFLICT,
                                "command_id was reused with different request data",
                            ),
                        )?;
                    }
                    continue;
                }
                Err(error) => {
                    if let Some(id) = id {
                        let (code, message) = app_error(AppError::Session(error));
                        write_json(&mut stdout, &JsonRpcResponse::failure(id, code, message))?;
                    }
                    continue;
                }
            }
        }

        let handled = handle_request(&engine, &config, &settings, &cancellation, &request);
        cancellation.clear();
        match handled {
            Ok((result, events)) => {
                let result = if let Some(meta) = &command_meta {
                    let event_ids = events
                        .iter()
                        .map(|event| event.event_id.clone())
                        .collect::<Vec<_>>();
                    let receipt = engine.complete_command(&meta.command_id, result.clone(), &event_ids)?;
                    attach_receipt(result, receipt, false)
                } else {
                    result
                };
                if let Some(id) = id {
                    write_json(&mut stdout, &JsonRpcResponse::success(id, result))?;
                }
                for event in events {
                    write_json(&mut stdout, &event_notification(&event))?;
                }
            }
            Err(error) => {
                let (code, message) = app_error(error);
                if let Some(meta) = &command_meta {
                    let _ = engine.reject_command(
                        &meta.command_id,
                        json!({ "code": code, "message": message }),
                    );
                }
                if let Some(id) = id {
                    write_json(&mut stdout, &JsonRpcResponse::failure(id, code, message))?;
                }
            }
        }
        stdout.flush()?;
    }
    Ok(())
}

fn handle_request(
    engine: &SessionEngine,
    config: &DaemonConfig,
    settings: &SettingsStore,
    cancellation: &Arc<TurnCancellation>,
    request: &JsonRpcRequest,
) -> Result<(Value, Vec<EventEnvelope>), AppError> {
    match request.method.as_str() {
        "runtime.v1.health" => Ok((
            json!({ "status": "ok", "protocol": "local-first-harness.v1" }),
            Vec::new(),
        )),
        "runtime.v1.execution.capabilities" => {
            let local = LocalRestrictedBackend::capabilities();
            let unavailable = UnavailableContainerBackend.capabilities();
            let container_preflight = config.container.preflight();
            let container = json!({
                "name": if config.container.vm_isolation { "docker-hyperv-vm" } else { "docker-isolated" },
                "available": container_preflight.is_ok(),
                "unavailable_reason": container_preflight.as_ref().err(),
                "trusted_workspace_only": false,
                "handle_relative_fs": true,
                "no_symlink": true,
                "process_isolation": true,
                "network_isolation": true,
                "secret_injection": false,
                "image": &config.container.image,
                "runtime": &config.container.runtime,
                "vm_isolation": config.container.vm_isolation,
            });
            Ok((
                json!({
                    "active": if config.execution_backend == "local-trusted-host" {
                        json!({ "available": true, "capabilities": local.clone() })
                    } else {
                        container.clone()
                    },
                    "available_adapters": [local, container, json!({ "available": false, "capabilities": unavailable })]
                }),
                Vec::new(),
            ))
        }
        "runtime.v1.settings.get" => {
            let current = settings.load()?;
            Ok((json!(current.public_view()), Vec::new()))
        }
        "runtime.v1.settings.save" => {
            let requested: HarnessSettings = parse_params(&request.params)?;
            let saved = settings.save(requested)?;
            Ok((json!(saved.public_view()), Vec::new()))
        }
        "runtime.v1.models" => {
            let configured = settings.load()?.public_view();
            let providers = configured
                .providers
                .iter()
                .map(|provider| {
                    json!({
                        "id": provider.id,
                        "name": provider.name,
                        "kind": provider.kind,
                        "api_key_env": provider.api_key_env,
                        "api_key_configured": provider.api_key_configured,
                        "enabled": provider.enabled,
                        "models": provider.models,
                    })
                })
                .collect::<Vec<_>>();
            Ok((
                json!({
                    "settings": {
                        "version": configured.version,
                        "default_model": configured.default_model,
                        "providers": providers,
                    },
                    "openai_compatible": {
                        "configured": env::var("HARNESS_MODEL_ENDPOINT").is_ok(),
                        "endpoint_configured": env::var("HARNESS_MODEL_ENDPOINT").is_ok(),
                        "api_key_configured": env::var("HARNESS_MODEL_API_KEY").is_ok()
                    }
                }),
                Vec::new(),
            ))
        }
        "runtime.v1.session.create" => {
            let params: CreateParams = parse_params(&request.params)?;
            let (session_id, event) = engine.create_session(params.workspace_roots, params.model)?;
            Ok((
                json!({ "session_id": session_id, "event": event.clone() }),
                vec![event],
            ))
        }
        "runtime.v1.session.events" => {
            let params: SessionEventsParams = parse_params(&request.params)?;
            let all_events = engine.effective_events(&params.session_id)?;
            let after = params.after_global_sequence.unwrap_or(0);
            let limit = params.limit.unwrap_or(100).min(1000);
            if limit == 0 {
                return Err(AppError::InvalidParams(
                    "limit must be between 1 and 1000".to_string(),
                ));
            }
            let mut events = all_events
                .into_iter()
                .filter(|event| event.global_sequence > after)
                .take(limit + 1)
                .collect::<Vec<_>>();
            let has_more = events.len() > limit;
            if has_more {
                events.truncate(limit);
            }
            let next_global_sequence = events
                .last()
                .map(|event| event.global_sequence)
                .unwrap_or(after);
            Ok((
                json!({
                    "session_id": params.session_id,
                    "events": events,
                    "next_global_sequence": next_global_sequence,
                    "has_more": has_more
                }),
                Vec::new(),
            ))
        }
        "runtime.v1.session.replay" => {
            let params: SessionParams = parse_params(&request.params)?;
            Ok((json!(engine.replay(&params.session_id)?), Vec::new()))
        }
        "runtime.v1.session.resume" => {
            let params: SessionParams = parse_params(&request.params)?;
            Ok((json!(engine.resume(&params.session_id)?), Vec::new()))
        }
        "runtime.v1.session.operation.inspect" => {
            let params: OperationInspectParams = parse_params(&request.params)?;
            let before = engine.effective_events(&params.session_id)?;
            let broker = build_broker(engine, config, &params.session_id)?;
            let coordinator = ToolCoordinator::new(engine.clone(), broker);
            let inspection = coordinator.inspect_operation(&params.session_id, &params.operation_id)?;
            let after = engine.effective_events(&params.session_id)?;
            let events = after
                .into_iter()
                .filter(|event| !before.iter().any(|old| old.event_id == event.event_id))
                .collect::<Vec<_>>();
            Ok((
                json!({
                    "session_id": params.session_id,
                    "operation_id": params.operation_id,
                    "operation": engine.inspect_operation(&params.session_id, &params.operation_id)?,
                    "backend": inspection,
                    "external_backend_checked": true
                }),
                events,
            ))
        }
        "runtime.v1.session.operation.continue" => {
            let params: OperationContinueParams = parse_params(&request.params)?;
            let before = engine.effective_events(&params.session_id)?;
            let broker = build_broker(engine, config, &params.session_id)?;
            let coordinator = ToolCoordinator::new(engine.clone(), broker);
            let grant = params.approval.map(|approval| ApprovalGrant {
                session_id: params.session_id.clone(),
                operation_id: params.operation_id.clone(),
                request_digest: approval.request_digest,
                principal: params.principal.clone(),
                actor: approval.actor,
                nonce: approval.nonce,
                expires_at_ms: approval.expires_at_ms,
            });
            let result = coordinator.continue_operation(
                &params.session_id,
                &params.operation_id,
                &params.principal,
                grant,
            );
            let after = engine.effective_events(&params.session_id)?;
            let events = after
                .into_iter()
                .filter(|event| !before.iter().any(|old| old.event_id == event.event_id))
                .collect::<Vec<_>>();
            match result {
                Ok(result) => Ok((json!({ "accepted": true, "result": result }), events)),
                Err(error) => Ok((json!({ "accepted": false, "error": error.to_string() }), events)),
            }
        }
        "runtime.v1.session.fork" => {
            let params: ForkParams = parse_params(&request.params)?;
            let (session_id, event) = engine.fork(&params.source_session_id, params.source_sequence)?;
            Ok((
                json!({ "session_id": session_id, "event": event.clone() }),
                vec![event],
            ))
        }
        "runtime.v1.turn.start" => {
            let params: TurnStartParams = parse_params(&request.params)?;
            params.parameters.validate().map_err(AppError::InvalidParams)?;
            let before = engine.effective_events(&params.session_id)?;
            let session_model = engine.replay(&params.session_id)?.model;
            let selected_model = match params
                .model
                .or(session_model)
                .or_else(|| env::var("HARNESS_MODEL").ok())
            {
                Some(model) => model,
                None => settings.load()?.default_model,
            };
            let mut attachment_size = 0usize;
            for attachment in &params.attachments {
                if !supported_image_media_type(&attachment.media_type) {
                    return Err(AppError::InvalidParams(format!(
                        "unsupported image media type `{}`",
                        attachment.media_type
                    )));
                }
                if attachment.content_base64.len() > MAX_MODEL_ATTACHMENT_CONTENT_SIZE {
                    return Err(AppError::InvalidParams(
                        "one image attachment exceeds the configured size limit".to_string(),
                    ));
                }
                attachment_size = attachment_size.saturating_add(attachment.content_base64.len());
            }
            if attachment_size > MAX_ARTIFACT_CONTENT_SIZE {
                return Err(AppError::InvalidParams(
                    "image attachments exceed the configured request size limit".to_string(),
                ));
            }
            if !params.attachments.is_empty() {
                let vision_enabled = settings
                    .load()?
                    .model(&selected_model)
                    .map(|(_provider, model)| {
                        model.supports_vision
                    })
                    .unwrap_or(false);
                if !vision_enabled {
                    return Err(AppError::InvalidParams(
                        "selected model is not marked as vision-capable".to_string(),
                    ));
                }
            }
            let provider = configured_provider(settings, &selected_model)?;
            let turn_id = params
                .turn_id
                .unwrap_or_else(|| format!("turn-{}", now_ms()));
            cancellation.activate(&params.session_id, &turn_id);
            let runtime = AgentRuntime::new(
                Arc::new(provider),
                engine.clone(),
                project_layers(engine, &params.session_id)?,
                default_tools(),
            ).with_cancellation(cancellation.clone());
            let result = runtime.run_turn(
                &params.session_id,
                turn_id,
                params.content,
                selected_model,
                params.attachments,
                params.parameters,
            ).and_then(|response| drive_read_only(engine, config, settings, cancellation, &params.session_id, response));
            let after = engine.effective_events(&params.session_id)?;
            let events = after
                .into_iter()
                .filter(|event| !before.iter().any(|old| old.event_id == event.event_id))
                .collect::<Vec<_>>();
            match result {
                Ok(response) => Ok((json!({ "accepted": true, "response": response }), events)),
                Err(error) => Ok((
                    json!({ "accepted": false, "error": error.to_string() }),
                    events,
                )),
            }
        }
        "runtime.v1.tool.execute" => {
            let params: ToolExecuteParams = parse_params(&request.params)?;
            let ToolExecuteParams {
                session_id,
                turn_id,
                run_id,
                principal,
                operation_id,
                tool_name,
                intent,
                approval,
                backend,
            } = params;
            if let Some(requested_backend) = backend {
                let expected_backend = if config.execution_backend == "container" {
                    "docker-isolated"
                } else if config.execution_backend == "vm" {
                    "docker-hyperv-vm"
                } else {
                    "local-trusted-host"
                };
                if requested_backend != expected_backend && requested_backend != config.execution_backend {
                    return Err(AppError::InvalidParams(format!(
                        "requested backend `{requested_backend}` is not the configured backend"
                    )));
                }
            }
            let before = engine.effective_events(&session_id)?;
            let broker = build_broker(engine, config, &session_id)?;
            let coordinator = ToolCoordinator::new(engine.clone(), broker);
            let grant = approval.map(|approval| ApprovalGrant {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
                request_digest: approval.request_digest,
                principal: principal.clone(),
                actor: approval.actor,
                nonce: approval.nonce,
                expires_at_ms: approval.expires_at_ms,
            });
            cancellation.activate(&session_id, &turn_id);
            cancellation.set_run_id(&run_id);
            let result = coordinator.execute(
                &session_id,
                &turn_id,
                &run_id,
                &principal,
                operation_id,
                &tool_name,
                intent,
                grant,
            );
            let outcome = match result {
                Ok(result) if matches!(&result.status, ToolStatus::Succeeded | ToolStatus::Failed) => {
                    let response = if !engine.run_has_pending_operations(&session_id, &run_id)? {
                        let projection = engine.replay(&session_id)?;
                        let model_name = projection
                            .model
                            .or_else(|| env::var("HARNESS_MODEL").ok())
                            .unwrap_or_default();
                        // continue_run inherits the actual model and parameters from
                        // the run's latest ModelRequested event; this lookup only
                        // selects the configured provider implementation.
                        let inherited_model = engine
                            .effective_events(&session_id)?
                            .into_iter()
                            .rev()
                            .find_map(|event| match event.payload {
                                EventPayload::ModelRequested { run_id: event_run, model, .. }
                                    if event_run == run_id => Some(model),
                                _ => None,
                            })
                            .unwrap_or(model_name);
                        let provider = configured_provider(settings, &inherited_model)?;
                        let runtime = AgentRuntime::new(
                            Arc::new(provider),
                            engine.clone(),
                            project_layers(engine, &session_id)?,
                            default_tools(),
                        ).with_cancellation(cancellation.clone());
                        match runtime.continue_run(&session_id, &turn_id, &run_id)
                            .and_then(|response| drive_read_only(engine, config, settings, cancellation, &session_id, response)) {
                            Ok(response) => Some(response),
                            Err(error) => {
                                let after = engine.effective_events(&session_id)?;
                                let events = after
                                    .into_iter()
                                    .filter(|event| {
                                        !before.iter().any(|old| old.event_id == event.event_id)
                                    })
                                    .collect::<Vec<_>>();
                                return Ok((
                                    json!({
                                        "accepted": false,
                                        "result": result,
                                        "error": error.to_string()
                                    }),
                                    events,
                                ));
                            }
                        }
                    } else {
                        None
                    };
                    json!({ "accepted": true, "result": result, "response": response })
                }
                Ok(result) => {
                    if !engine.run_has_pending_operations(&session_id, &run_id)? {
                        engine.append(
                            &session_id,
                            EventPayload::RunCompleted {
                                run_id: run_id.clone(),
                                success: false,
                            },
                            Some(turn_id.clone()),
                            None,
                        )?;
                    }
                    json!({ "accepted": false, "result": result, "error": "tool execution did not succeed" })
                }
                Err(error) => json!({ "accepted": false, "error": error.to_string() }),
            };
            let after = engine.effective_events(&session_id)?;
            let events = after
                .into_iter()
                .filter(|event| !before.iter().any(|old| old.event_id == event.event_id))
                .collect::<Vec<_>>();
            Ok((outcome, events))
        }
        "runtime.v1.session.recover" => {
            let params: RecoverParams = parse_params(&request.params)?;
            let RecoverParams {
                session_id,
                operation_id,
                reason,
                ..
            } = params;
            let marker = engine.append(
                &session_id,
                harness_protocol::EventPayload::RecoveryRequired {
                    operation_id: operation_id.clone(),
                    reason: reason.clone(),
                },
                None,
                None,
            )?;
            let mut events = vec![marker.clone()];
            let projection = engine.replay(&session_id)?;
            if let Some(operation_id) = operation_id {
                if let Some(operation) = projection.operations.get(&operation_id) {
                    let failed = engine.append(
                        &session_id,
                        harness_protocol::EventPayload::ToolFailed {
                            operation_id: operation_id.clone(),
                            error_code: "recovery_abandoned".to_string(),
                            message: reason.clone(),
                        },
                        None,
                        Some(marker.event_id.clone()),
                    )?;
                    let failed_event_id = failed.event_id.clone();
                    events.push(failed);
                    if !engine.run_has_pending_operations(&session_id, &operation.run_id)? {
                        events.push(engine.append(
                            &session_id,
                            harness_protocol::EventPayload::RunCompleted {
                                run_id: operation.run_id.clone(),
                                success: false,
                            },
                            None,
                            Some(failed_event_id),
                        )?);
                    }
                }
            } else {
                for turn in projection
                    .turns
                    .values()
                    .filter(|turn| turn.status == "running")
                {
                    if !engine.run_has_pending_operations(&session_id, &turn.run_id)? {
                        events.push(engine.append(
                            &session_id,
                            harness_protocol::EventPayload::RunCompleted {
                                run_id: turn.run_id.clone(),
                                success: false,
                            },
                            None,
                            Some(marker.event_id.clone()),
                        )?);
                    }
                }
            }
            Ok((json!({ "events": events.clone() }), events))
        }
        "runtime.v1.session.prompt" => {
            let params: PromptParams = parse_params(&request.params)?;
            let events = engine.effective_events(&params.session_id)?;
            let prompt = compile(
                &events,
                &params.layers.unwrap_or_default(),
                &params.tools.unwrap_or_default(),
            );
            Ok((json!(prompt), Vec::new()))
        }
        "runtime.v1.events.replay" => {
            let params: EventReplayParams = parse_params(&request.params)?;
            let limit = params.limit.min(1000);
            if limit == 0 {
                return Err(AppError::InvalidParams(
                    "limit must be between 1 and 1000".to_string(),
                ));
            }
            let mut events = engine
                .store()
                .global_events_after(params.after_global_sequence, (limit + 1).min(1001))?;
            let has_more = events.len() > limit;
            if has_more {
                events.truncate(limit);
            }
            let next = events
                .last()
                .map(|event| event.global_sequence)
                .unwrap_or(params.after_global_sequence);
            Ok((
                json!({
                    "events": events,
                    "next_global_sequence": next,
                    "has_more": has_more
                }),
                Vec::new(),
            ))
        }
        "runtime.v1.events.subscribe" => {
            let params: SubscribeParams = parse_params(&request.params)?;
            let parent_subscription_id = params.subscription_id.clone();
            let view = if let Some(subscription_id) = params.subscription_id.as_deref() {
                engine.subscription_backlog(subscription_id, params.limit.min(1000))?
            } else {
                let Some(session_id) = params.session_id.as_deref() else {
                    return Ok((
                        json!({
                            "delivery": "committed-notifications",
                            "transport": "stdio",
                            "note": "pass session_id and after_global_sequence for a backlog; reconnect with runtime.v1.events.replay"
                        }),
                        Vec::new(),
                    ));
                };
                engine.subscribe(
                    session_id,
                    params.after_global_sequence,
                    params.limit.min(1000),
                )?
            };
            Ok((
                json!({
                    "delivery": "committed-notifications",
                    "transport": "stdio",
                    "subscription_id": view.subscription_id,
                    "parent_subscription_id": parent_subscription_id,
                    "session_id": view.session_id,
                    "events": view.events,
                    "deliveries": view.deliveries,
                    "next_global_sequence": view.next_global_sequence,
                    "has_more": view.has_more,
                    "note": "acknowledge the applied cursor with runtime.v1.events.ack; use outbox pull/nack after disconnect"
                }),
                Vec::new(),
            ))
        }
        "runtime.v1.events.pull" => {
            let params: OutboxClaimParams = parse_params(&request.params)?;
            let deliveries = engine.claim_outbox(
                &params.subscription_id,
                params.limit.min(1000),
                params.lease_ms,
            )?;
            Ok((
                json!({
                    "subscription_id": params.subscription_id,
                    "deliveries": deliveries,
                }),
                Vec::new(),
            ))
        }
        "runtime.v1.events.nack" => {
            let params: OutboxNackParams = parse_params(&request.params)?;
            engine.nack_outbox(
                &params.subscription_id,
                params.message_id,
                &params.error,
                params.retry_after_ms,
            )?;
            Ok((
                json!({
                    "subscription_id": params.subscription_id,
                    "message_id": params.message_id,
                    "requeued": true,
                }),
                Vec::new(),
            ))
        }
        "runtime.v1.events.ack" => {
            let params: SubscriptionAckParams = parse_params(&request.params)?;
            engine.acknowledge_subscription(
                &params.subscription_id,
                params.after_global_sequence,
            )?;
            Ok((
                json!({
                    "subscription_id": params.subscription_id,
                    "after_global_sequence": params.after_global_sequence,
                    "acknowledged": true
                }),
                Vec::new(),
            ))
        }
        "runtime.v1.events.unsubscribe" => {
            let params: SubscriptionParams = parse_params(&request.params)?;
            let removed = engine.delete_subscription(&params.subscription_id)?;
            Ok((
                json!({
                    "subscription_id": params.subscription_id,
                    "unsubscribed": removed
                }),
                Vec::new(),
            ))
        }
        "runtime.v1.artifact.put" => {
            let params: ArtifactPutParams = parse_params(&request.params)?;
            let bytes = params.content.as_bytes().len();
            let chars = params.content.chars().count();
            if bytes > MAX_ARTIFACT_CONTENT_SIZE || chars > MAX_ARTIFACT_CONTENT_SIZE {
                return Err(AppError::InvalidParams(format!(
                    "content must not exceed {MAX_ARTIFACT_CONTENT_SIZE} characters or bytes"
                )));
            }
            let operation_id = params.operation_id.as_deref().unwrap_or("client-upload");
            let metadata = engine.put_artifact(
                &params.session_id,
                operation_id,
                &params.kind,
                &params.content,
                &params.media_type,
            )?;
            Ok((json!({ "metadata": metadata }), Vec::new()))
        }
        "runtime.v1.artifact.list" => {
            let params: ArtifactListParams = parse_params(&request.params)?;
            let artifacts = engine.list_artifacts(&params.session_id)?;
            Ok((json!({ "artifacts": artifacts }), Vec::new()))
        }
        "runtime.v1.artifact.get" => {
            let params: ArtifactGetParams = parse_params(&request.params)?;
            let artifact = engine
                .get_artifact(&params.session_id, &params.artifact_id)?
                .ok_or_else(|| AppError::InvalidParams("artifact not found".to_string()))?;
            let bytes = artifact.content.as_bytes();
            let offset = params.offset.unwrap_or(0).min(bytes.len());
            let limit = params
                .limit
                .unwrap_or(1_048_576)
                .min(MAX_ARTIFACT_CONTENT_SIZE);
            let end = offset.saturating_add(limit).min(bytes.len());
            let content = String::from_utf8_lossy(&bytes[offset..end]).into_owned();
            Ok((
                json!({
                    "metadata": artifact.metadata,
                    "offset": offset,
                    "content": content,
                    "next_offset": end,
                    "eof": end >= bytes.len()
                }),
                Vec::new(),
            ))
        }
        method => Err(AppError::MethodNotFound(method.to_string())),
    }
}

/// Drive only read-only proposals. Other proposals remain pending for the client to
/// submit through tool.execute with an exact approval; never replay a terminal effect.
fn drive_read_only(
    engine: &SessionEngine,
    config: &DaemonConfig,
    settings: &SettingsStore,
    cancellation: &Arc<TurnCancellation>,
    session_id: &str,
    mut response: ModelResponse,
) -> Result<ModelResponse, harness_agent_runtime::AgentError> {
    for _ in 0..32 {
        if response.tools.is_empty() || response.tools.iter().any(|tool| !tool.intent.is_read_only()) {
            return Ok(response);
        }
        let events = engine.effective_events(session_id)?;
        let (turn_id, run_id) = events.iter().rev().find_map(|event| match &event.payload {
            EventPayload::ModelResponded { turn_id, run_id, request_id, .. } if request_id == &response.request_id => Some((turn_id.clone(), run_id.clone())),
            _ => None,
        }).ok_or_else(|| harness_agent_runtime::AgentError::Model("model response has no recorded turn".into()))?;
        check_stopped(engine, cancellation, session_id, &turn_id, &run_id)?;
        let broker = build_broker(engine, config, session_id).map_err(|error| harness_agent_runtime::AgentError::Model(format!("cannot build execution broker: {error:?}")))?;
        let coordinator = ToolCoordinator::new(engine.clone(), broker);
        for tool in &response.tools {
            check_stopped(engine, cancellation, session_id, &turn_id, &run_id)?;
            coordinator.execute(session_id, &turn_id, &run_id, "agent", tool.operation_id.as_deref().ok_or_else(|| harness_agent_runtime::AgentError::Model("missing operation id".into()))?, &tool.tool_name, tool.intent.clone(), None)?;
        }
        check_stopped(engine, cancellation, session_id, &turn_id, &run_id)?;
        let model = engine.effective_events(session_id)?.into_iter().rev().find_map(|event| match event.payload {
            EventPayload::ModelRequested { run_id: event_run, model, .. } if event_run == run_id => Some(model),
            _ => None,
        }).ok_or_else(|| harness_agent_runtime::AgentError::Model("run has no model".into()))?;
        let provider = configured_provider(settings, &model).map_err(|error| harness_agent_runtime::AgentError::Model(format!("cannot select provider: {error:?}")))?;
        let layers = project_layers(engine, session_id).map_err(|error| harness_agent_runtime::AgentError::Model(format!("cannot load project rules: {error:?}")))?;
        check_stopped(engine, cancellation, session_id, &turn_id, &run_id)?;
        response = AgentRuntime::new(Arc::new(provider), engine.clone(), layers, default_tools())
            .with_cancellation(cancellation.clone()).continue_run(session_id, &turn_id, &run_id)?;
    }
    Err(harness_agent_runtime::AgentError::Model("automatic read loop exceeded 32 steps".into()))
}

fn check_stopped(
    engine: &SessionEngine,
    cancellation: &TurnCancellation,
    session_id: &str,
    turn_id: &str,
    run_id: &str,
) -> Result<(), harness_agent_runtime::AgentError> {
    if cancellation.is_requested() {
        engine.append(session_id, EventPayload::RecoveryRequired {
            operation_id: None,
            reason: "turn stopped by user; in-flight model or tool completion was discarded".into(),
        }, Some(turn_id.into()), None)?;
        engine.append(session_id, EventPayload::RunCompleted {
            run_id: run_id.into(), success: false,
        }, Some(turn_id.into()), None)?;
        return Err(harness_agent_runtime::AgentError::Stopped);
    }
    Ok(())
}

fn project_layers(engine: &SessionEngine, session_id: &str) -> Result<PromptLayers, AppError> {
    let projection = engine.replay(session_id)?;
    let mut layers = PromptLayers::default();
    if let [root] = projection.workspace_roots.as_slice() {
        let path = PathBuf::from(root).join("AGENTS.md");
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 64 * 1024 {
                layers.project_rules = fs::read_to_string(&path).map_err(|error| AppError::Agent(error.to_string()))?;
            }
        }
    }
    Ok(layers)
}

fn build_broker(
    engine: &SessionEngine,
    config: &DaemonConfig,
    session_id: &str,
) -> Result<ExecutionBroker, AppError> {
    let workspace_roots = engine.replay(session_id)?.workspace_roots;
    let roots = workspace_roots.into_iter().map(PathBuf::from).collect();
    match config.execution_backend.as_str() {
        "local-trusted-host" => {
            let mut policy_config = PolicyConfig::trusted_workspace(roots);
            if env::var("HARNESS_TRUSTED_AUTO_APPROVE").as_deref() == Ok("1") {
                policy_config.approval_mode = ApprovalMode::AutoApproveTrustedWorkspace;
            }
            if env::var("HARNESS_TRUSTED_PROCESS").as_deref() == Ok("1") {
                policy_config.allow_trusted_host_process = true;
                for program in env::split_paths(&env::var_os("HARNESS_ALLOWED_PROGRAMS").unwrap_or_default()) {
                    if program.is_absolute() {
                        if let Ok(canonical) = fs::canonicalize(&program) {
                            policy_config.allowed_programs.insert(canonical);
                        }
                    }
                }
            }
            let policy = PolicyEngine::new(policy_config)
                .map_err(|error| AppError::Agent(error.to_string()))?;
            Ok(ExecutionBroker::new(policy))
        }
        "container" | "vm" => {
            let mut container = config.container.clone();
            if config.execution_backend == "vm" {
                container.vm_isolation = true;
            }
            container
                .preflight()
                .map_err(AppError::Agent)?;
            let policy = PolicyEngine::new(PolicyConfig::isolated_workspace(roots))
                .map_err(|error| AppError::Agent(error.to_string()))?;
            Ok(ExecutionBroker::with_container_backend(policy, container))
        }
        other => Err(AppError::InvalidParams(format!(
            "unsupported execution backend `{other}`; use local-trusted-host, container, or vm"
        ))),
    }
}

fn parse_params<T: for<'de> Deserialize<'de>>(value: &Value) -> Result<T, AppError> {
    serde_json::from_value(value.clone()).map_err(|error| AppError::InvalidParams(error.to_string()))
}

fn app_error(error: AppError) -> (i32, String) {
    match error {
        AppError::InvalidParams(message) => (JSON_RPC_INVALID_PARAMS, message),
        AppError::MethodNotFound(method) => (
            JSON_RPC_METHOD_NOT_FOUND,
            format!("method not found: {method}"),
        ),
        AppError::Session(error) => (JSON_RPC_INTERNAL_ERROR, error.to_string()),
        AppError::Settings(error) => (JSON_RPC_INTERNAL_ERROR, error.to_string()),
        AppError::Agent(error) => (JSON_RPC_INTERNAL_ERROR, error),
    }
}

fn session_error_code(error: &SessionError) -> i32 {
    match error {
        SessionError::ExpectedHeadMismatch { .. } => JSON_RPC_HEAD_CONFLICT,
        SessionError::CommandIdempotencyConflict => JSON_RPC_IDEMPOTENCY_CONFLICT,
        _ => JSON_RPC_INTERNAL_ERROR,
    }
}

fn command_meta(
    request: &JsonRpcRequest,
) -> Result<Option<CommandMeta>, AppError> {
    let is_mutation = matches!(
        request.method.as_str(),
        "runtime.v1.session.create"
            | "runtime.v1.session.fork"
            | "runtime.v1.turn.start"
            | "runtime.v1.tool.execute"
            | "runtime.v1.session.recover"
            | "runtime.v1.session.operation.continue"
    );
    if !is_mutation {
        return Ok(None);
    }
    let Some(params) = request.params.as_object() else {
        return Err(AppError::InvalidParams(
            "mutation params must be an object".to_string(),
        ));
    };
    let Some(command_id) = params.get("command_id").and_then(Value::as_str) else {
        // Legacy requests remain usable, but do not receive retry guarantees.
        return Ok(None);
    };
    if command_id.trim().is_empty() {
        return Err(AppError::InvalidParams(
            "command_id must not be empty".to_string(),
        ));
    }
    let request_digest = canonical_json(&request.params)
        .map_err(|error| AppError::InvalidParams(error.to_string()))?;
    let session_id = params
        .get("session_id")
        .or_else(|| params.get("source_session_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let expected_head = params
        .get("expected_head")
        .map(|value| {
            serde_json::from_value::<CommandHead>(value.clone())
                .map_err(|error| AppError::InvalidParams(error.to_string()))
        })
        .transpose()?;
    Ok(Some(CommandMeta {
        command_id: command_id.to_string(),
        request_digest,
        session_id,
        expected_head,
    }))
}

fn attach_receipt(
    result: Value,
    receipt: CommandReceipt,
    replayed: bool,
) -> Value {
    let mut receipt_value = serde_json::to_value(receipt).expect("receipt is serializable");
    if let Some(object) = receipt_value.as_object_mut() {
        object.insert("replayed".to_string(), Value::Bool(replayed));
    }
    match result {
        Value::Object(mut object) => {
            object.insert("receipt".to_string(), receipt_value);
            Value::Object(object)
        }
        value => json!({ "result": value, "receipt": receipt_value }),
    }
}

fn pure_command_outcome(
    outcome: PureCommandResult,
) -> Result<(Value, Vec<EventEnvelope>), (i32, String)> {
    match outcome {
        PureCommandResult::New { receipt, events } => {
            let result = receipt.result.clone().unwrap_or(Value::Null);
            Ok((attach_receipt(result, receipt, false), events))
        }
        PureCommandResult::Existing(receipt) => match &receipt.state {
            CommandState::Committed => {
                let result = receipt.result.clone().unwrap_or(Value::Null);
                Ok((attach_receipt(result, receipt, true), Vec::new()))
            }
            CommandState::Rejected | CommandState::Aborted => Err(receipt_error(&receipt)),
            CommandState::Pending => Err((
                JSON_RPC_COMMAND_PENDING,
                "command is pending recovery; retry after explicit recovery".to_string(),
            )),
        },
    }
}

fn receipt_error(receipt: &CommandReceipt) -> (i32, String) {
    let Some(error) = receipt.error.as_ref() else {
        return (JSON_RPC_INTERNAL_ERROR, "rejected command".to_string());
    };
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .unwrap_or(JSON_RPC_INTERNAL_ERROR as i64) as i32;
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("rejected command")
        .to_string();
    (code, message)
}

struct SharedOutput<'a>(&'a Arc<Mutex<io::BufWriter<io::Stdout>>>);

impl Write for SharedOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut output = self.0.lock().unwrap();
        output.write_all(bytes)?;
        output.flush()?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

fn write_json<T: serde::Serialize>(writer: &mut impl Write, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()
}
