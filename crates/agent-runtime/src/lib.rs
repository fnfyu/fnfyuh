//! Model/tool orchestration over protocol and event-store seams.
//!
//! The runtime records an intent before every external operation. Providers return
//! provider-neutral responses, while policy, backend authorization, inspection, and
//! durable artifact/terminal facts remain outside the model adapter.

use harness_execution_broker::{
    audit_result, ApprovalGrant, BackendOperation, BrokerError, ExecutionBroker,
    ExecutionContext,
};
use harness_prompt_compiler::{
    compile_with_tool_outputs, CompiledPrompt, PromptLayers, ToolDefinition,
};
use harness_protocol::{BackendOperationState, EventPayload, ModelParameters, ToolIntent, ToolResult};
use harness_session_engine::{ArtifactInput, OperationStatus, SessionEngine, SessionError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("session error: {0}")]
    Session(#[from] SessionError),
    #[error("model provider error: {0}")]
    Model(String),
    #[error("turn was stopped")]
    Stopped,
    #[error("execution broker error: {0}")]
    Broker(#[from] BrokerError),
    #[error("approval is required for operation `{0}`")]
    ApprovalRequired(String),
    #[error("operation `{0}` is already terminal; retry with a new operation id")]
    OperationTerminal(String),
    #[error("operation `{0}` needs explicit recovery before it can continue")]
    RecoveryRequired(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAttachment {
    pub name: String,
    pub media_type: String,
    pub content_base64: String,
}

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub session_id: String,
    pub turn_id: String,
    pub run_id: String,
    pub request_id: String,
    pub model: String,
    pub prompt: CompiledPrompt,
    pub tools: Vec<ToolDefinition>,
    pub attachments: Vec<ModelAttachment>,
    pub parameters: ModelParameters,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposedTool {
    /// Filled by the runtime before the proposal is emitted. A client can use this
    /// stable operation id to submit approval/recovery without creating a duplicate.
    pub operation_id: Option<String>,
    pub tool_name: String,
    pub intent: ToolIntent,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelResponse {
    pub request_id: String,
    pub content: String,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub tools: Vec<ProposedTool>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

pub trait ModelProvider: Send + Sync {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, AgentError>;
}

impl<T: ModelProvider + ?Sized> ModelProvider for Arc<T> {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, AgentError> {
        (**self).complete(request)
    }
}

/// Shared by the daemon input reader and the serial runtime owner. A stop request
/// remains pending until the owner records the terminal state; no reader writes SQLite.
#[derive(Debug, Default)]
pub struct TurnCancellation {
    state: Mutex<TurnCancellationState>,
}

#[derive(Debug, Default)]
struct TurnCancellationState {
    active: Option<(String, String, Option<String>)>,
    requested: bool,
}

impl TurnCancellation {
    pub fn activate(&self, session_id: &str, turn_id: &str) {
        let mut state = self.state.lock().unwrap();
        state.active = Some((session_id.into(), turn_id.into(), None));
        state.requested = false;
    }

    pub fn set_run_id(&self, run_id: &str) {
        let mut state = self.state.lock().unwrap();
        if let Some((_, _, active_run_id)) = &mut state.active {
            *active_run_id = Some(run_id.into());
        }
    }

    /// Returns whether cancellation was accepted for the currently active run.
    /// A matching stop with no run id can be accepted before the run is created.
    pub fn request(&self, session_id: &str, turn_id: &str, run_id: Option<&str>) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.active.as_ref().is_some_and(|(session, turn, run)| {
            session == session_id && turn == turn_id &&
                run_id.is_none_or(|requested| run.as_deref() == Some(requested))
        }) {
            state.requested = true;
            true
        } else {
            false
        }
    }

    pub fn is_requested(&self) -> bool {
        self.state.lock().unwrap().requested
    }

    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap();
        state.active = None;
        state.requested = false;
    }
}

pub struct AgentRuntime<M> {
    model: Arc<M>,
    sessions: SessionEngine,
    prompt_layers: PromptLayers,
    tools: Vec<ToolDefinition>,
    cancellation: Option<Arc<TurnCancellation>>,
}

impl<M: ModelProvider> AgentRuntime<M> {
    fn compile_prompt(&self, session_id: &str, events: &[harness_protocol::EventEnvelope]) -> Result<CompiledPrompt, AgentError> {
        let mut artifacts = BTreeMap::new();
        for event in events {
            if let EventPayload::ToolFinished { result, .. } = &event.payload {
                for artifact_id in [result.stdout_artifact_id.as_ref(), result.stderr_artifact_id.as_ref()].into_iter().flatten() {
                    if let Some(artifact) = self.sessions.get_artifact(session_id, artifact_id)? {
                        artifacts.insert(artifact_id.clone(), bounded_tool_output(&artifact.content, 32 * 1024));
                    }
                }
            }
        }
        let mut prompt = compile_with_tool_outputs(events, &self.prompt_layers, &self.tools, &artifacts);
        const MAX_CONTEXT_BYTES: usize = 192 * 1024;
        let mut bytes: usize = prompt.messages.iter().map(|message| message.content.len()).sum();
        if bytes > MAX_CONTEXT_BYTES {
            // Keep the stable project rules and recent messages; show an honest
            // omission marker instead of pretending to summarize unseen history.
            let keep_system = usize::from(prompt.messages.first().is_some_and(|message| message.role == "system"));
            let mut start = keep_system;
            while bytes > MAX_CONTEXT_BYTES && start + 1 < prompt.messages.len() {
                bytes -= prompt.messages[start].content.len();
                start += 1;
            }
            let recent = prompt.messages.drain(start..).collect::<Vec<_>>();
            prompt.messages.truncate(keep_system);
            prompt.messages.push(harness_prompt_compiler::ModelMessage {
                role: "user".into(), content: "[Earlier session messages omitted to fit context; inspect workspace or artifacts if needed.]".into(),
            });
            prompt.messages.extend(recent);
            prompt.dynamic_tail = prompt.messages.iter().skip(keep_system).cloned().collect();
            let digest = sha2::Sha256::digest(harness_protocol::canonical_json(&prompt.messages).map_err(|error| AgentError::Model(error.to_string()))?.as_bytes());
            prompt.digest = format!("sha256:{digest:x}");
        }
        Ok(prompt)
    }

    pub fn new(
        model: Arc<M>,
        sessions: SessionEngine,
        prompt_layers: PromptLayers,
        tools: Vec<ToolDefinition>,
    ) -> Self {
        Self {
            model,
            sessions,
            prompt_layers,
            tools,
            cancellation: None,
        }
    }

    pub fn with_cancellation(mut self, cancellation: Arc<TurnCancellation>) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    /// Only the serial runtime owner records terminal events; the input reader
    /// merely signals a request and never races an SQLite write against execution.
    pub fn check_stopped(&self, session_id: &str, turn_id: &str, run_id: &str) -> Result<(), AgentError> {
        if self.cancellation.as_ref().is_some_and(|cancellation| cancellation.is_requested()) {
            self.sessions.append(session_id, EventPayload::RecoveryRequired {
                operation_id: None,
                reason: "turn stopped by user; in-flight model or tool completion was discarded".into(),
            }, Some(turn_id.into()), None)?;
            self.sessions.append(session_id, EventPayload::RunCompleted {
                run_id: run_id.into(), success: false,
            }, Some(turn_id.into()), None)?;
            return Err(AgentError::Stopped);
        }
        Ok(())
    }

    pub fn run_turn(
        &self,
        session_id: &str,
        turn_id: impl Into<String>,
        user_text: impl Into<String>,
        model_name: impl Into<String>,
        attachments: Vec<ModelAttachment>,
        parameters: ModelParameters,
    ) -> Result<ModelResponse, AgentError> {
        parameters
            .validate()
            .map_err(|error| AgentError::Model(format!("invalid model parameters: {error}")))?;
        let turn_id = turn_id.into();
        let run_id = Uuid::new_v4().to_string();
        if let Some(cancellation) = &self.cancellation {
            cancellation.set_run_id(&run_id);
        }
        let user_text = user_text.into();
        let model_name = model_name.into();
        self.sessions.append(
            session_id,
            EventPayload::TurnStarted {
                turn_id: turn_id.clone(),
                run_id: run_id.clone(),
            },
            Some(turn_id.clone()),
            None,
        )?;
        self.sessions.append(
            session_id,
            EventPayload::UserMessage {
                turn_id: turn_id.clone(),
                content: user_text,
            },
            Some(turn_id.clone()),
            None,
        )?;
        self.sessions.append(
            session_id,
            EventPayload::RunStarted {
                turn_id: turn_id.clone(),
                run_id: run_id.clone(),
            },
            Some(turn_id.clone()),
            None,
        )?;

        self.check_stopped(session_id, &turn_id, &run_id)?;
        let history = self.sessions.effective_events(session_id)?;
        let prompt = self.compile_prompt(session_id, &history)?;
        let prompt_digest = prompt.digest.clone();
        let request_id = Uuid::new_v4().to_string();
        self.sessions.append(
            session_id,
            EventPayload::ModelRequested {
                turn_id: turn_id.clone(),
                run_id: run_id.clone(),
                request_id: request_id.clone(),
                model: model_name.clone(),
                prompt_digest,
                parameters: parameters.clone(),
            },
            Some(turn_id.clone()),
            None,
        )?;

        self.check_stopped(session_id, &turn_id, &run_id)?;
        let mut response = match self.model.complete(ModelRequest {
            session_id: session_id.to_string(),
            turn_id: turn_id.clone(),
            run_id: run_id.clone(),
            request_id: request_id.clone(),
            model: model_name,
            prompt,
            tools: self.tools.clone(),
            attachments,
            parameters,
        }) {
            Ok(response) => response,
            Err(error) => {
                let _ = self.sessions.append(
                    session_id,
                    EventPayload::RecoveryRequired {
                        operation_id: None,
                        reason: error.to_string(),
                    },
                    Some(turn_id.clone()),
                    Some(request_id.clone()),
                );
                let _ = self.sessions.append(
                    session_id,
                    EventPayload::RunCompleted {
                        run_id: run_id.clone(),
                        success: false,
                    },
                    Some(turn_id.clone()),
                    None,
                );
                return Err(error);
            }
        };
        self.check_stopped(session_id, &turn_id, &run_id)?;
        response.request_id = request_id.clone();
        self.sessions.append(
            session_id,
            EventPayload::ModelResponded {
                turn_id: turn_id.clone(),
                run_id: run_id.clone(),
                request_id: request_id.clone(),
                content: response.content.clone(),
                stop_reason: response.stop_reason.clone(),
            },
            Some(turn_id.clone()),
            None,
        )?;
        let provider_tool_calls = std::mem::take(&mut response.tool_calls);
        for call in provider_tool_calls {
            match parse_tool_call(call) {
                Ok(tool) => response.tools.push(tool),
                Err(error) => {
                    let _ = self.sessions.append(
                        session_id,
                        EventPayload::RecoveryRequired {
                            operation_id: None,
                            reason: error.to_string(),
                        },
                        Some(turn_id.clone()),
                        Some(request_id.clone()),
                    );
                    let _ = self.sessions.append(
                        session_id,
                        EventPayload::RunCompleted {
                            run_id: run_id.clone(),
                            success: false,
                        },
                        Some(turn_id.clone()),
                        None,
                    );
                    return Err(error);
                }
            }
        }
        for tool in &mut response.tools {
            self.check_stopped(session_id, &turn_id, &run_id)?;
            let operation_id = tool
                .operation_id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            tool.operation_id = Some(operation_id.clone());
            self.sessions.append(
                session_id,
                EventPayload::ToolProposed {
                    turn_id: turn_id.clone(),
                    run_id: run_id.clone(),
                    operation_id,
                    tool_name: tool.tool_name.clone(),
                    intent: tool.intent.clone(),
                },
                Some(turn_id.clone()),
                Some(request_id.clone()),
            )?;
        }
        if response.tools.is_empty() {
            self.check_stopped(session_id, &turn_id, &run_id)?;
            self.sessions.append(
                session_id,
                EventPayload::RunCompleted {
                    run_id,
                    success: true,
                },
                Some(turn_id),
                None,
            )?;
        }
        Ok(response)
    }

    /// Continues the existing turn/run after its proposed tools have reached a
    /// terminal state. It reuses the latest request's model parameters
    /// and does not emit turn, user-message, or run-start events again.
    pub fn continue_run(
        &self,
        session_id: &str,
        turn_id: &str,
        run_id: &str,
    ) -> Result<ModelResponse, AgentError> {
        self.check_stopped(session_id, turn_id, run_id)?;
        let events = self.sessions.effective_events(session_id)?;
        let projection = self.sessions.replay(session_id)?;
        let turn = projection.turns.get(turn_id).ok_or_else(|| {
            SessionError::InvalidState(format!("turn `{turn_id}` does not exist"))
        })?;
        if turn.run_id != run_id || turn.status != "running" {
            return Err(SessionError::InvalidState(
                "continuation requires the active matching turn/run".to_string(),
            )
            .into());
        }
        let run_operations = projection
            .operations
            .values()
            .filter(|operation| operation.run_id == run_id)
            .collect::<Vec<_>>();
        if run_operations.iter().any(|operation| !operation.is_terminal()) {
            return Err(SessionError::InvalidState(
                "continuation requires all run operations to be terminal".to_string(),
            )
            .into());
        }
        if events.iter().filter(|event| matches!(&event.payload,
            EventPayload::ModelRequested { run_id: requested_run, .. } if requested_run == run_id
        )).count() >= 32 {
            self.sessions.append(session_id, EventPayload::RunCompleted {
                run_id: run_id.to_string(), success: false,
            }, Some(turn_id.to_string()), None)?;
            return Err(AgentError::Model("run reached the 32 model-step limit".into()));
        }
        let (model_name, parameters) = events
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                EventPayload::ModelRequested {
                    turn_id: event_turn,
                    run_id: event_run,
                    model,
                    parameters,
                    ..
                } if event_turn == turn_id && event_run == run_id => {
                    Some((model.clone(), parameters.clone()))
                }
                _ => None,
            })
            .ok_or_else(|| {
                SessionError::InvalidState(
                    "run has no prior model request to continue".to_string(),
                )
            })?;

        let prompt = self.compile_prompt(session_id, &events)?;
        let request_id = Uuid::new_v4().to_string();
        self.sessions.append(
            session_id,
            EventPayload::ModelRequested {
                turn_id: turn_id.to_string(),
                run_id: run_id.to_string(),
                request_id: request_id.clone(),
                model: model_name.clone(),
                prompt_digest: prompt.digest.clone(),
                parameters: parameters.clone(),
            },
            Some(turn_id.to_string()),
            None,
        )?;
        self.check_stopped(session_id, turn_id, run_id)?;
        let mut response = match self.model.complete(ModelRequest {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            run_id: run_id.to_string(),
            request_id: request_id.clone(),
            model: model_name,
            prompt,
            tools: self.tools.clone(),
            attachments: Vec::new(),
            parameters,
        }) {
            Ok(response) => response,
            Err(error) => {
                let _ = self.sessions.append(
                    session_id,
                    EventPayload::RecoveryRequired {
                        operation_id: None,
                        reason: error.to_string(),
                    },
                    Some(turn_id.to_string()),
                    Some(request_id.clone()),
                );
                let _ = self.sessions.append(
                    session_id,
                    EventPayload::RunCompleted {
                        run_id: run_id.to_string(),
                        success: false,
                    },
                    Some(turn_id.to_string()),
                    None,
                );
                return Err(error);
            }
        };
        self.check_stopped(session_id, &turn_id, &run_id)?;
        response.request_id = request_id.clone();
        self.sessions.append(
            session_id,
            EventPayload::ModelResponded {
                turn_id: turn_id.to_string(),
                run_id: run_id.to_string(),
                request_id: request_id.clone(),
                content: response.content.clone(),
                stop_reason: response.stop_reason.clone(),
            },
            Some(turn_id.to_string()),
            None,
        )?;
        let provider_tool_calls = std::mem::take(&mut response.tool_calls);
        for call in provider_tool_calls {
            match parse_tool_call(call) {
                Ok(tool) => response.tools.push(tool),
                Err(error) => {
                    let _ = self.sessions.append(
                        session_id,
                        EventPayload::RecoveryRequired {
                            operation_id: None,
                            reason: error.to_string(),
                        },
                        Some(turn_id.to_string()),
                        Some(request_id.clone()),
                    );
                    let _ = self.sessions.append(
                        session_id,
                        EventPayload::RunCompleted {
                            run_id: run_id.to_string(),
                            success: false,
                        },
                        Some(turn_id.to_string()),
                        None,
                    );
                    return Err(error);
                }
            }
        }
        for tool in &mut response.tools {
            self.check_stopped(session_id, &turn_id, &run_id)?;
            let operation_id = tool
                .operation_id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            tool.operation_id = Some(operation_id.clone());
            self.sessions.append(
                session_id,
                EventPayload::ToolProposed {
                    turn_id: turn_id.to_string(),
                    run_id: run_id.to_string(),
                    operation_id,
                    tool_name: tool.tool_name.clone(),
                    intent: tool.intent.clone(),
                },
                Some(turn_id.to_string()),
                Some(request_id.clone()),
            )?;
        }
        if response.tools.is_empty() {
            self.check_stopped(session_id, &turn_id, &run_id)?;
            self.sessions.append(
                session_id,
                EventPayload::RunCompleted {
                    run_id: run_id.to_string(),
                    success: true,
                },
                Some(turn_id.to_string()),
                None,
            )?;
        }
        Ok(response)
    }
}

fn bounded_tool_output(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let mut end = max_bytes;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated: tool output exceeded {max_bytes} bytes]", &content[..end])
}

fn required_string(arguments: &Value, key: &str) -> Result<String, AgentError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AgentError::Model(format!("tool argument `{key}` must be a non-empty string")))
}

fn optional_string(arguments: &Value, key: &str) -> Option<String> {
    arguments.get(key).and_then(Value::as_str).map(ToOwned::to_owned)
}

fn optional_u64(arguments: &Value, key: &str) -> Option<u64> {
    arguments.get(key).and_then(Value::as_u64)
}

fn optional_bool(arguments: &Value, key: &str, default: bool) -> bool {
    arguments.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn string_array(arguments: &Value, key: &str) -> Result<Vec<String>, AgentError> {
    arguments
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| AgentError::Model(format!("tool argument `{key}` must be an array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| AgentError::Model(format!("tool argument `{key}` must contain strings")))
        })
        .collect()
}

fn parse_tool_call(call: ToolCall) -> Result<ProposedTool, AgentError> {
    let name = call.name.trim().to_ascii_lowercase();
    let intent = match name.as_str() {
        "read" | "read_file" => ToolIntent::ReadFile {
            path: required_string(&call.arguments, "path")?,
        },
        "search" => ToolIntent::Search {
            root: required_string(&call.arguments, "root")?,
            query: required_string(&call.arguments, "query")?,
        },
        "list" | "list_files" | "ls" => ToolIntent::ListFiles {
            root: required_string(&call.arguments, "root")?,
            depth: optional_u64(&call.arguments, "depth").unwrap_or(2).min(8) as u32,
            include_hidden: optional_bool(&call.arguments, "include_hidden", false),
        },
        "read_image" | "image" | "view_image" => ToolIntent::ReadImage {
            path: required_string(&call.arguments, "path")?,
            max_bytes: optional_u64(&call.arguments, "max_bytes"),
        },
        "write" | "write_file" => ToolIntent::WriteFile {
            path: required_string(&call.arguments, "path")?,
            content: call
                .arguments
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| AgentError::Model("tool argument `content` must be a string".to_string()))?
                .to_string(),
        },
        "edit" | "edit_file" => ToolIntent::EditFile {
            path: required_string(&call.arguments, "path")?,
            old_text: required_string(&call.arguments, "old_text")?,
            new_text: call
                .arguments
                .get("new_text")
                .and_then(Value::as_str)
                .ok_or_else(|| AgentError::Model("tool argument `new_text` must be a string".to_string()))?
                .to_string(),
            expected_sha256: optional_string(&call.arguments, "expected_sha256"),
        },
        "exec" | "run" => ToolIntent::Exec {
            program: required_string(&call.arguments, "program")?,
            args: string_array(&call.arguments, "args")?,
            cwd: optional_string(&call.arguments, "cwd"),
            timeout_ms: call.arguments.get("timeout_ms").and_then(Value::as_u64),
        },
        "test" => ToolIntent::Test {
            program: required_string(&call.arguments, "program")?,
            args: string_array(&call.arguments, "args")?,
            cwd: optional_string(&call.arguments, "cwd"),
            timeout_ms: call.arguments.get("timeout_ms").and_then(Value::as_u64),
        },
        "git" => ToolIntent::Git {
            args: string_array(&call.arguments, "args")?,
            cwd: optional_string(&call.arguments, "cwd"),
        },
        _ => {
            return Err(AgentError::Model(format!(
                "model requested unsupported tool `{}`",
                call.name
            )))
        }
    };
    Ok(ProposedTool {
        operation_id: (!call.call_id.is_empty()).then_some(call.call_id),
        tool_name: name,
        intent,
    })
}

/// Coordinates the durable audit chain around one tool side effect. Callers do
/// not receive a raw shell/file adapter; they receive only this event-backed seam.
pub struct ToolCoordinator {
    sessions: SessionEngine,
    broker: ExecutionBroker,
}

impl ToolCoordinator {
    pub fn new(sessions: SessionEngine, broker: ExecutionBroker) -> Self {
        Self { sessions, broker }
    }

    fn finish_tool(
        &self,
        session_id: &str,
        operation_id: &str,
        turn_id: &str,
        run_id: &str,
        result: &ToolResult,
    ) -> Result<(), AgentError> {
        let stdout_artifact = result.output_media_type.as_ref().map(|media_type| ArtifactInput {
            kind: "image_base64".to_string(),
            content: result.stdout.clone(),
            media_type: media_type.clone(),
        });
        let mut artifacts = [
            ("stdout", result.stdout.as_str(), "text/plain; charset=utf-8"),
            ("stderr", result.stderr.as_str(), "text/plain; charset=utf-8"),
        ]
        .into_iter()
        .filter(|(_, content, _)| !content.is_empty())
        .map(|(kind, content, media_type)| ArtifactInput {
            kind: kind.to_string(),
            content: content.to_string(),
            media_type: media_type.to_string(),
        })
        .collect::<Vec<_>>();
        if let Some(artifact) = stdout_artifact {
            artifacts.retain(|artifact| artifact.kind != "stdout");
            artifacts.push(artifact);
        }
        // A successful final tool is not the end of the run: the model must see
        // its durable artifact body and decide whether to answer or propose tools.
        self.sessions.finish_operation(
            session_id,
            operation_id,
            turn_id,
            run_id,
            audit_result(result),
            artifacts,
            false,
        )?;
        Ok(())
    }

    pub fn execute(
        &self,
        session_id: &str,
        turn_id: &str,
        run_id: &str,
        principal: &str,
        operation_id: impl Into<String>,
        tool_name: &str,
        intent: ToolIntent,
        approval: Option<ApprovalGrant>,
    ) -> Result<ToolResult, AgentError> {
        let operation_id = operation_id.into();
        let proposal_exists = if let Some((
            existing_turn_id,
            existing_run_id,
            existing_tool_name,
            existing_intent,
        )) = self.sessions.find_tool_proposal(session_id, &operation_id)? {
            if existing_turn_id != turn_id
                || existing_run_id != run_id
                || existing_tool_name != tool_name
                || existing_intent != intent
            {
                return Err(SessionError::InvalidState(
                    "operation id is bound to a different exact tool request".to_string(),
                )
                .into());
            }
            let projection = self.sessions.replay(session_id)?;
            let status = projection
                .operations
                .get(&operation_id)
                .ok_or_else(|| {
                    SessionError::InvalidState("tool proposal has no projection".to_string())
                })?
                .status
                .clone();
            match status {
                OperationStatus::Proposed
                | OperationStatus::ApprovalRequested
                | OperationStatus::Approved => true,
                OperationStatus::ExecutionRequested
                | OperationStatus::Started
                | OperationStatus::Unknown
                | OperationStatus::RecoveryRequired => {
                    return Err(AgentError::RecoveryRequired(operation_id));
                }
                OperationStatus::Denied
                | OperationStatus::Succeeded
                | OperationStatus::Failed => {
                    return Err(AgentError::OperationTerminal(operation_id));
                }
            }
        } else {
            false
        };
        if !proposal_exists {
            self.sessions.append(
                session_id,
                EventPayload::ToolProposed {
                    turn_id: turn_id.to_string(),
                    run_id: run_id.to_string(),
                    operation_id: operation_id.clone(),
                    tool_name: tool_name.to_string(),
                    intent: intent.clone(),
                },
                Some(turn_id.to_string()),
                None,
            )?;
        }

        let workspace_roots = self.sessions.replay(session_id)?.workspace_roots;
        let prepared = match self.broker.prepare(
            ExecutionContext {
                session_id: session_id.to_string(),
                principal: principal.to_string(),
                backend: self.broker.capabilities().name.clone(),
                workspace_roots,
            },
            operation_id.clone(),
            intent.clone(),
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = self.sessions.append(
                    session_id,
                    EventPayload::PolicyEvaluated {
                        operation_id: operation_id.clone(),
                        request_digest: self.broker.policy().denied_request_digest(&intent),
                        policy_version: self.broker.policy().policy_version().to_string(),
                        outcome: harness_protocol::PolicyOutcome::Denied,
                        reason: error.to_string(),
                    },
                    Some(turn_id.to_string()),
                    None,
                )?;
                return Err(error.into());
            }
        };
        self.sessions.append(
            session_id,
            EventPayload::PolicyEvaluated {
                operation_id: operation_id.clone(),
                request_digest: prepared.request_digest().to_string(),
                policy_version: prepared.decision().policy_version.clone(),
                outcome: prepared.decision().outcome.clone(),
                reason: prepared.decision().reason.clone(),
            },
            Some(turn_id.to_string()),
            None,
        )?;

        if prepared.decision().requires_approval && approval.is_none() {
            self.sessions.append(
                session_id,
                EventPayload::ApprovalRequested {
                    operation_id: operation_id.clone(),
                    request_digest: prepared.request_digest().to_string(),
                    reason: prepared.decision().reason.clone(),
                },
                Some(turn_id.to_string()),
                None,
            )?;
            return Err(AgentError::ApprovalRequired(operation_id));
        }

        let approval_for_event = approval.clone();
        let authorized = match self.broker.authorize(prepared, approval) {
            Ok(authorized) => authorized,
            Err(error) => {
                self.sessions.append(
                    session_id,
                    EventPayload::ApprovalDenied {
                        operation_id: operation_id.clone(),
                        request_digest: approval_for_event
                            .as_ref()
                            .map(|grant| grant.request_digest.clone())
                            .unwrap_or_else(|| "unknown".to_string()),
                        actor: approval_for_event
                            .as_ref()
                            .map(|grant| grant.actor.clone())
                            .unwrap_or_else(|| "broker".to_string()),
                        reason: error.to_string(),
                    },
                    Some(turn_id.to_string()),
                    None,
                )?;
                return Err(error.into());
            }
        };
        if authorized.decision().requires_approval {
            if let Some(grant) = approval_for_event {
                self.sessions.append(
                    session_id,
                    EventPayload::ApprovalGranted {
                        operation_id: operation_id.clone(),
                        request_digest: grant.request_digest,
                        actor: grant.actor,
                        expires_at_ms: Some(grant.expires_at_ms),
                    },
                    Some(turn_id.to_string()),
                    None,
                )?;
            }
        }
        if let Err(error) = self.broker.validate(&authorized) {
            let _ = self.sessions.append(
                session_id,
                EventPayload::RecoveryRequired {
                    operation_id: Some(operation_id.clone()),
                    reason: format!("authorization changed before start: {error}"),
                },
                Some(turn_id.to_string()),
                None,
            );
            return Err(AgentError::RecoveryRequired(operation_id));
        }
        let backend_name = self.broker.capabilities().name.clone();
        let external_id = self.broker.external_id(&authorized);
        self.sessions.append(
            session_id,
            EventPayload::ExecutionRequested {
                operation_id: operation_id.clone(),
                request_digest: authorized.request_digest().to_string(),
                backend: backend_name.clone(),
                external_id: external_id.clone(),
            },
            Some(turn_id.to_string()),
            None,
        )?;
        self.sessions.append(
            session_id,
            EventPayload::ToolStarted {
                operation_id: operation_id.clone(),
                backend: backend_name,
                external_id,
            },
            Some(turn_id.to_string()),
            None,
        )?;

        match self.broker.execute(authorized) {
            Ok(result) => {
                if matches!(&result.status, harness_protocol::ToolStatus::TimedOut) {
                    self.sessions.append(
                        session_id,
                        EventPayload::ExecutionUnknown {
                            operation_id: operation_id.clone(),
                            reason: "backend execution timed out; external state requires inspection".to_string(),
                        },
                        Some(turn_id.to_string()),
                        None,
                    )?;
                    return Ok(result);
                }
                self.finish_tool(session_id, &operation_id, turn_id, run_id, &result)?;
                Ok(result)
            }
            Err(error) => {
                if matches!(
                    &error,
                    BrokerError::Runtime(_) | BrokerError::BackendUnavailable
                ) {
                    let _ = self.sessions.append(
                        session_id,
                        EventPayload::ExecutionUnknown {
                            operation_id: operation_id.clone(),
                            reason: error.to_string(),
                        },
                        Some(turn_id.to_string()),
                        None,
                    );
                    return Err(error.into());
                }
                match self.sessions.append(
                    session_id,
                    EventPayload::ToolFailed {
                        operation_id: operation_id.clone(),
                        error_code: "execution_failed".to_string(),
                        message: error.to_string(),
                    },
                    Some(turn_id.to_string()),
                    None,
                ) {
                    Ok(_) => {
                        if !self.sessions.run_has_pending_operations(session_id, run_id)? {
                            self.sessions.append(
                                session_id,
                                EventPayload::RunCompleted {
                                    run_id: run_id.to_string(),
                                    success: false,
                                },
                                Some(turn_id.to_string()),
                                None,
                            )?;
                        }
                        Err(error.into())
                    }
                    Err(log_error) => {
                        let _ = self.sessions.append(
                            session_id,
                            EventPayload::RecoveryRequired {
                                operation_id: Some(operation_id),
                                reason: format!("tool failure audit append failed: {log_error}"),
                            },
                            Some(turn_id.to_string()),
                            None,
                        );
                        Err(AgentError::RecoveryRequired(
                            "tool failure was not durably recorded".to_string(),
                        ))
                    }
                }
            }
        }
    }

    pub fn inspect_operation(
        &self,
        session_id: &str,
        operation_id: &str,
    ) -> Result<BackendInspectionView, AgentError> {
        let projection = self.sessions.replay(session_id)?;
        let operation = projection
            .operations
            .get(operation_id)
            .cloned()
            .ok_or_else(|| SessionError::InvalidState("operation does not exist".to_string()))?;
        if operation.is_terminal() {
            let state = operation.last_inspection.clone().unwrap_or_else(|| match &operation.status {
                OperationStatus::Succeeded => BackendOperationState::Succeeded,
                OperationStatus::Failed | OperationStatus::Denied => BackendOperationState::Failed,
                _ => BackendOperationState::NotTracked,
            });
            return Ok(BackendInspectionView {
                state,
                external_id: operation.external_id,
                detail: Some("operation is already terminal in the durable event log".to_string()),
                result: operation.last_result,
            });
        }
        let backend = operation
            .backend
            .clone()
            .unwrap_or_else(|| self.broker.capabilities().name.clone());
        let inspection = self.broker.inspect(&BackendOperation {
            operation_id: operation_id.to_string(),
            backend: backend.clone(),
            external_id: operation.external_id.clone(),
            intent: operation.intent.clone(),
            context: ExecutionContext {
                session_id: session_id.to_string(),
                principal: "backend-inspector".to_string(),
                backend: backend.clone(),
                workspace_roots: projection.workspace_roots.clone(),
            },
        })?;
        let _marker = self.sessions.append(
            session_id,
            EventPayload::BackendInspected {
                operation_id: operation_id.to_string(),
                backend,
                external_id: inspection.external_id.clone(),
                state: inspection.state.clone(),
                detail: inspection.detail.clone(),
                result: None,
            },
            Some(operation.turn_id.clone()),
            None,
        )?;
        if let Some(result) = inspection.result.as_ref() {
            self.finish_tool(
                session_id,
                operation_id,
                &operation.turn_id,
                &operation.run_id,
                result,
            )?;
        }
        Ok(BackendInspectionView {
            state: inspection.state,
            external_id: inspection.external_id,
            detail: inspection.detail,
            result: inspection.result.map(|result| audit_result(&result)),
        })
    }

    pub fn continue_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        principal: &str,
        approval: Option<ApprovalGrant>,
    ) -> Result<ToolResult, AgentError> {
        let projection = self.sessions.replay(session_id)?;
        let operation = projection
            .operations
            .get(operation_id)
            .cloned()
            .ok_or_else(|| SessionError::InvalidState("operation does not exist".to_string()))?;
        if !matches!(
            &operation.status,
            OperationStatus::Unknown | OperationStatus::RecoveryRequired
        ) {
            return Err(AgentError::OperationTerminal(operation_id.to_string()));
        }
        if !operation.intent.is_read_only() {
            return Err(AgentError::RecoveryRequired(
                "non-read-only unknown operation requires backend confirmation before retry".to_string(),
            ));
        }
        let backend = operation
            .backend
            .clone()
            .unwrap_or_else(|| self.broker.capabilities().name.clone());
        let backend_inspection = self.broker.inspect(&BackendOperation {
            operation_id: operation_id.to_string(),
            backend: backend.clone(),
            external_id: operation.external_id.clone(),
            intent: operation.intent.clone(),
            context: ExecutionContext {
                session_id: session_id.to_string(),
                principal: principal.to_string(),
                backend: backend.clone(),
                workspace_roots: projection.workspace_roots.clone(),
            },
        })?;
        match backend_inspection.state {
            BackendOperationState::NotFound => {}
            BackendOperationState::Running => {
                return Err(AgentError::RecoveryRequired(
                    "backend operation is still running; no new attempt was started".to_string(),
                ))
            }
            BackendOperationState::Succeeded | BackendOperationState::Failed => {
                return Err(AgentError::RecoveryRequired(
                    "backend already has a terminal result; inspect it to finalize the durable event".to_string(),
                ))
            }
            BackendOperationState::Unknown | BackendOperationState::NotTracked => {
                return Err(AgentError::RecoveryRequired(
                    "backend cannot prove that retry is safe".to_string(),
                ))
            }
        }
        let prepared = self.broker.prepare(
            ExecutionContext {
                session_id: session_id.to_string(),
                principal: principal.to_string(),
                backend: backend.clone(),
                workspace_roots: projection.workspace_roots.clone(),
            },
            operation_id.to_string(),
            operation.intent.clone(),
        )?;
        let authorized = self.broker.authorize(prepared, approval)?;
        let external_id = self.broker.external_id(&authorized);
        self.sessions.append(
            session_id,
            EventPayload::ExecutionContinuationRequested {
                operation_id: operation_id.to_string(),
                backend: backend.clone(),
                external_id: external_id.clone(),
            },
            Some(operation.turn_id.clone()),
            None,
        )?;
        self.sessions.append(
            session_id,
            EventPayload::ToolStarted {
                operation_id: operation_id.to_string(),
                backend,
                external_id,
            },
            Some(operation.turn_id.clone()),
            None,
        )?;
        match self.broker.continue_operation(authorized) {
            Ok(result) => {
                self.finish_tool(
                    session_id,
                    operation_id,
                    &operation.turn_id,
                    &operation.run_id,
                    &result,
                )?;
                Ok(result)
            }
            Err(error) => {
                let _ = self.sessions.append(
                    session_id,
                    EventPayload::ExecutionUnknown {
                        operation_id: operation_id.to_string(),
                        reason: error.to_string(),
                    },
                    Some(operation.turn_id),
                    None,
                );
                Err(error.into())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BackendInspectionView {
    pub state: BackendOperationState,
    pub external_id: Option<String>,
    pub detail: Option<String>,
    pub result: Option<harness_protocol::ToolAuditResult>,
}

const CODEX_MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct CodexOAuthProvider {
    node_command: String,
    bridge_script: String,
    timeout_ms: u64,
}

impl CodexOAuthProvider {
    pub fn new(
        node_command: impl Into<String>,
        bridge_script: impl Into<String>,
        timeout_ms: u64,
    ) -> Result<Self, AgentError> {
        let node_command = node_command.into();
        let bridge_script = bridge_script.into();
        if node_command.trim().is_empty() || bridge_script.trim().is_empty() {
            return Err(AgentError::Model(
                "Codex OAuth bridge command and script must not be empty".to_string(),
            ));
        }
        Ok(Self {
            node_command,
            bridge_script,
            timeout_ms: timeout_ms.clamp(1_000, 600_000),
        })
    }
}

/// Converts the compiled role/content history into the plain text prompt passed to
/// `codex exec`. The role labels keep system, user, assistant, and tool context
/// distinguishable without asking the CLI adapter to understand the prompt schema.
pub fn codex_prompt(prompt: &CompiledPrompt) -> String {
    prompt
        .messages
        .iter()
        .map(|message| format!("[{}]\n{}", message.role, message.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn json_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        let text = text.trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    let content = value.get("content")?;
    match content {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then_some(text.to_string())
        }
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            let text = text.trim();
            (!text.is_empty()).then_some(text.to_string())
        }
        _ => None,
    }
}

fn json_message_text(value: &Value) -> Option<String> {
    let event_type = value.get("type").and_then(Value::as_str);
    if event_type == Some("item.completed") {
        let item = value.get("item")?;
        if item.get("type").and_then(Value::as_str) != Some("agent_message") {
            return None;
        }
        return json_text(item);
    }
    if event_type == Some("agent_message") {
        return json_text(value);
    }
    if event_type == Some("turn.completed") {
        for key in ["message", "content", "output_text", "final_message"] {
            if let Some(candidate) = value.get(key) {
                if let Some(text) = candidate.as_str() {
                    let text = text.trim();
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                } else if let Some(text) = json_text(candidate) {
                    return Some(text);
                }
            }
        }
        return None;
    }
    if let Some(message) = value.get("message") {
        if let Some(text) = message.as_str() {
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
        if message
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role != "assistant")
        {
            return None;
        }
        return json_text(message);
    }
    if event_type == Some("message")
        || value.get("role").and_then(Value::as_str) == Some("assistant")
        || (event_type.is_none() && value.get("content").is_some())
    {
        return json_text(value);
    }
    None
}

/// Extracts the final assistant text from Codex JSONL output. Codex versions have
/// emitted both `item.completed` agent messages and simpler message/content JSON;
/// unknown non-JSON output remains a safe last-resort fallback.
pub fn parse_codex_jsonl(output: &str) -> Option<String> {
    let mut final_text = None;
    let mut plain_lines = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => {
                if let Some(text) = json_message_text(&value) {
                    final_text = Some(text);
                }
            }
            Err(_) => plain_lines.push(trimmed),
        }
    }
    final_text.or_else(|| {
        let fallback = if plain_lines.is_empty() {
            output.trim().to_string()
        } else {
            plain_lines.join("\n")
        };
        (!fallback.is_empty()).then_some(fallback)
    })
}

fn effective_reasoning_effort(parameters: &ModelParameters) -> Option<&str> {
    parameters
        .reasoning_effort
        .as_deref()
        .filter(|level| *level != "auto")
}

fn thinking_budget(level: &str) -> u32 {
    match level {
        "minimal" => 1_024,
        "low" => 2_048,
        "medium" => 4_096,
        "high" => 8_192,
        "xhigh" => 16_384,
        "max" => 24_576,
        _ => 4_096,
    }
}

impl ModelProvider for CodexOAuthProvider {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, AgentError> {
        if request.model.trim().is_empty() {
            return Err(AgentError::Model(
                "OpenAI Codex model id must not be empty".to_string(),
            ));
        }
        let messages = request
            .prompt
            .messages
            .iter()
            .map(|message| {
                serde_json::json!({
                    "role": message.role,
                    "content": message.content,
                })
            })
            .collect::<Vec<_>>();
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            })
            .collect::<Vec<_>>();
        let payload = serde_json::json!({
            "request_id": request.request_id,
            "model": request.model,
            "messages": messages,
            "tools": tools,
            "attachments": &request.attachments,
            "parameters": &request.parameters,
        });
        let body = serde_json::to_vec(&payload)
            .map_err(|error| AgentError::Model(format!("Codex bridge request encode failed: {error}")))?;
        let mut child = Command::new(&self.node_command)
            .arg(&self.bridge_script)
            .arg("complete")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                AgentError::Model(format!(
                    "failed to start OpenAI Codex OAuth bridge `{}`: {error}",
                    self.bridge_script
                ))
            })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(&body)
                .map_err(|error| AgentError::Model(format!("Codex bridge request write failed: {error}")))?;
        }
        let mut stdout_pipe = child.stdout.take().ok_or_else(|| {
            AgentError::Model("Codex bridge stdout pipe was not available".to_string())
        })?;
        let mut stderr_pipe = child.stderr.take().ok_or_else(|| {
            AgentError::Model("Codex bridge stderr pipe was not available".to_string())
        })?;
        let stdout_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout_pipe
                .by_ref()
                .take((CODEX_MAX_OUTPUT_BYTES + 1) as u64)
                .read_to_end(&mut bytes);
            bytes
        });
        let stderr_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr_pipe
                .by_ref()
                .take((CODEX_MAX_OUTPUT_BYTES + 1) as u64)
                .read_to_end(&mut bytes);
            bytes
        });
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let mut timed_out = false;
        let mut wait_error = None;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() >= deadline => {
                    timed_out = true;
                    let _ = child.kill();
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    wait_error = Some(error);
                    let _ = child.kill();
                    break;
                }
            }
        }
        let status = child.wait().map_err(|error| {
            AgentError::Model(format!("failed to collect Codex bridge status: {error}"))
        })?;
        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();
        if timed_out {
            return Err(AgentError::Model(format!(
                "OpenAI Codex OAuth request timed out after {} ms",
                self.timeout_ms
            )));
        }
        if let Some(error) = wait_error {
            return Err(AgentError::Model(format!(
                "failed while waiting for Codex bridge: {error}"
            )));
        }
        if stdout.len() > CODEX_MAX_OUTPUT_BYTES || stderr.len() > CODEX_MAX_OUTPUT_BYTES {
            return Err(AgentError::Model(
                "Codex bridge output exceeded the configured limit".to_string(),
            ));
        }
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("exit status {status}")
            } else {
                stderr
            };
            return Err(AgentError::Model(format!("OpenAI Codex OAuth bridge failed: {detail}")));
        }
        let envelope: Value = serde_json::from_slice(&stdout)
            .map_err(|error| AgentError::Model(format!("Codex bridge response was not JSON: {error}")))?;
        if envelope.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(AgentError::Model(
                envelope
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("OpenAI Codex OAuth request failed")
                    .to_string(),
            ));
        }
        let response = envelope.get("result").cloned().unwrap_or(Value::Null);
        Ok(ModelResponse {
            request_id: response
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or(&request.request_id)
                .to_string(),
            content: response
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stop_reason: response
                .get("stop_reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            tools: Vec::new(),
            tool_calls: response
                .get("tool_calls")
                .cloned()
                .map(|value| serde_json::from_value(value).unwrap_or_default())
                .unwrap_or_default(),
        })
    }
}

#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    endpoint: String,
    api_key: Option<String>,
    timeout_ms: u64,
    max_response_bytes: usize,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: Option<String>,
        timeout_ms: u64,
    ) -> Result<Self, AgentError> {
        let endpoint = endpoint.into();
        if endpoint.trim().is_empty() {
            return Err(AgentError::Model("model endpoint must not be empty".to_string()));
        }
        Ok(Self {
            endpoint,
            api_key,
            timeout_ms: timeout_ms.clamp(1_000, 600_000),
            max_response_bytes: 8 * 1024 * 1024,
        })
    }

    pub fn from_env() -> Result<Self, AgentError> {
        let endpoint = std::env::var("HARNESS_MODEL_ENDPOINT").map_err(|_| {
            AgentError::Model(
                "HARNESS_MODEL_ENDPOINT is required for the configured HTTP provider".to_string(),
            )
        })?;
        let api_key = std::env::var("HARNESS_MODEL_API_KEY").ok();
        let timeout_ms = std::env::var("HARNESS_MODEL_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(120_000);
        Self::new(endpoint, api_key, timeout_ms)
    }

    fn request_body(&self, request: &ModelRequest) -> Result<Value, AgentError> {
        let last_user_index = request
            .prompt
            .messages
            .iter()
            .rposition(|message| message.role == "user");
        let messages = request
            .prompt
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let role = if message.role == "summary" {
                    "system"
                } else {
                    message.role.as_str()
                };
                let content = if Some(index) == last_user_index && !request.attachments.is_empty() {
                    let mut parts = vec![serde_json::json!({
                        "type": "text",
                        "text": message.content,
                    })];
                    parts.extend(request.attachments.iter().filter_map(|attachment| {
                        attachment.media_type.starts_with("image/").then(|| {
                            serde_json::json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!(
                                        "data:{};base64,{}",
                                        attachment.media_type, attachment.content_base64
                                    ),
                                },
                            })
                        })
                    }));
                    Value::Array(parts)
                } else {
                    Value::String(message.content.clone())
                };
                serde_json::json!({ "role": role, "content": content })
            })
            .collect::<Vec<_>>();
        let reasoning = effective_reasoning_effort(&request.parameters);
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": false,
        });
        if let Some(level) = reasoning {
            body["reasoning_effort"] = Value::String(if level == "off" {
                "none".to_string()
            } else {
                level.to_string()
            });
        }
        if let Some(temperature) = request.parameters.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }
        if let Some(max_output_tokens) = request.parameters.max_output_tokens {
            let field = if matches!(reasoning, Some(level) if level != "off") {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body[field] = serde_json::json!(max_output_tokens);
        }
        if !request.tools.is_empty() {
            let tools = request
                .tools
                .iter()
                .map(|tool| {
                    let parameters: Value = serde_json::from_str(&tool.input_schema).map_err(|_| {
                        AgentError::Model(format!("tool `{}` has invalid JSON schema", tool.name))
                    })?;
                    Ok(serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": parameters,
                        }
                    }))
                })
                .collect::<Result<Vec<_>, AgentError>>()?;
            body["tools"] = Value::Array(tools);
        }
        Ok(body)
    }
}

impl ModelProvider for OpenAiCompatibleProvider {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, AgentError> {
        let body = self.request_body(&request)?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(self.timeout_ms))
            .build();
        let mut http = agent.post(&self.endpoint).set("Content-Type", "application/json");
        http = http.set("X-Request-ID", &request.request_id);
        if let Some(api_key) = self.api_key.as_deref() {
            http = http.set("Authorization", &format!("Bearer {api_key}"));
        }
        let response = http.send_json(body).map_err(|error| match error {
            ureq::Error::Status(code, _) => AgentError::Model(format!("model provider returned HTTP {code}")),
            ureq::Error::Transport(_) => {
                AgentError::Model("model provider transport failure".to_string())
            }
        })?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(self.max_response_bytes as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| AgentError::Model(format!("model response read failed: {error}")))?;
        if bytes.len() >= self.max_response_bytes {
            return Err(AgentError::Model("model response exceeded the configured limit".to_string()));
        }
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|error| AgentError::Model(format!("model response was not valid JSON: {error}")))?;
        let choice = body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .ok_or_else(|| AgentError::Model("model response did not contain a choice".to_string()))?;
        let message = choice.get("message").cloned().unwrap_or(Value::Null);
        let content = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut tool_calls = Vec::new();
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let function = call.get("function").ok_or_else(|| {
                    AgentError::Model("model tool call did not contain a function".to_string())
                })?;
                let arguments_text = function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let arguments = serde_json::from_str(arguments_text).map_err(|error| {
                    AgentError::Model(format!("model tool arguments were invalid JSON: {error}"))
                })?;
                tool_calls.push(ToolCall {
                    call_id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments,
                });
            }
        }
        Ok(ModelResponse {
            request_id: body
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(&request.request_id)
                .to_string(),
            content,
            stop_reason: choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            tools: Vec::new(),
            tool_calls,
        })
    }
}

#[derive(Clone)]
pub struct AnthropicProvider {
    endpoint: String,
    api_key: Option<String>,
    timeout_ms: u64,
    max_response_bytes: usize,
}

impl AnthropicProvider {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: Option<String>,
        timeout_ms: u64,
    ) -> Result<Self, AgentError> {
        let endpoint = endpoint.into();
        if endpoint.trim().is_empty() {
            return Err(AgentError::Model("model endpoint must not be empty".to_string()));
        }
        Ok(Self {
            endpoint,
            api_key,
            timeout_ms: timeout_ms.clamp(1_000, 600_000),
            max_response_bytes: 8 * 1024 * 1024,
        })
    }

    fn request_body(&self, request: &ModelRequest) -> Result<Value, AgentError> {
        let mut system = Vec::new();
        let last_user_index = request
            .prompt
            .messages
            .iter()
            .rposition(|message| message.role == "user");
        let messages = request
            .prompt
            .messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                if message.role == "system" || message.role == "summary" {
                    system.push(message.content.clone());
                    None
                } else {
                    let role = if message.role == "assistant" { "assistant" } else { "user" };
                    let content = if message.role == "user"
                        && Some(index) == last_user_index
                        && !request.attachments.is_empty()
                    {
                        let mut parts = vec![serde_json::json!({
                            "type": "text",
                            "text": message.content,
                        })];
                        parts.extend(request.attachments.iter().filter_map(|attachment| {
                            attachment.media_type.starts_with("image/").then(|| {
                                serde_json::json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": attachment.media_type,
                                        "data": attachment.content_base64,
                                    },
                                })
                            })
                        }));
                        Value::Array(parts)
                    } else {
                        Value::String(message.content.clone())
                    };
                    Some(serde_json::json!({ "role": role, "content": content }))
                }
            })
            .collect::<Vec<_>>();
        let reasoning = effective_reasoning_effort(&request.parameters);
        let requested_max_tokens = request.parameters.max_output_tokens.unwrap_or(4096);
        let mut body = serde_json::json!({
            "model": request.model,
            "max_tokens": requested_max_tokens,
            "messages": messages,
            "stream": false,
        });
        if let Some(level) = reasoning {
            if level == "off" {
                body["thinking"] = serde_json::json!({ "type": "disabled" });
            } else {
                let budget = thinking_budget(level);
                let max_tokens = requested_max_tokens.max(budget.saturating_add(1024));
                body["max_tokens"] = serde_json::json!(max_tokens);
                body["thinking"] = serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": budget,
                });
            }
        }
        if !matches!(reasoning, Some(level) if level != "off") {
            if let Some(temperature) = request.parameters.temperature {
                if temperature > 1.0 {
                    return Err(AgentError::Model(
                        "Anthropic temperature must be between 0 and 1".to_string(),
                    ));
                }
                body["temperature"] = serde_json::json!(temperature);
            }
        }
        if !system.is_empty() {
            body["system"] = Value::String(system.join("\n\n"));
        }
        if !request.tools.is_empty() {
            body["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        let input_schema: Value = serde_json::from_str(&tool.input_schema).unwrap_or_else(|_| {
                            serde_json::json!({ "type": "object", "properties": {} })
                        });
                        serde_json::json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": input_schema,
                        })
                    })
                    .collect(),
            );
        }
        Ok(body)
    }
}

impl ModelProvider for AnthropicProvider {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, AgentError> {
        let body = self.request_body(&request)?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(self.timeout_ms))
            .build();
        let mut http = agent
            .post(&self.endpoint)
            .set("Content-Type", "application/json")
            .set("anthropic-version", "2023-06-01")
            .set("X-Request-ID", &request.request_id);
        if let Some(api_key) = self.api_key.as_deref() {
            http = http.set("x-api-key", api_key);
        }
        let response = http.send_json(body).map_err(|error| match error {
            ureq::Error::Status(code, _) => AgentError::Model(format!("model provider returned HTTP {code}")),
            ureq::Error::Transport(_) => AgentError::Model("model provider transport failure".to_string()),
        })?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(self.max_response_bytes as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| AgentError::Model(format!("model response read failed: {error}")))?;
        if bytes.len() >= self.max_response_bytes {
            return Err(AgentError::Model("model response exceeded the configured limit".to_string()));
        }
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|error| AgentError::Model(format!("model response was not valid JSON: {error}")))?;
        let mut content = String::new();
        let mut tool_calls = Vec::new();
        for block in body.get("content").and_then(Value::as_array).into_iter().flatten() {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => content.push_str(block.get("text").and_then(Value::as_str).unwrap_or_default()),
                Some("tool_use") => tool_calls.push(ToolCall {
                    call_id: block.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                    name: block.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                    arguments: block.get("input").cloned().unwrap_or_else(|| serde_json::json!({})),
                }),
                _ => {}
            }
        }
        Ok(ModelResponse {
            request_id: body
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(&request.request_id)
                .to_string(),
            content,
            stop_reason: body.get("stop_reason").and_then(Value::as_str).map(ToOwned::to_owned),
            tools: Vec::new(),
            tool_calls,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::{PolicyOutcome, ToolAuditResult, ToolStatus};
    use harness_session_engine::SqliteEventStore;
    use std::sync::Mutex;

    struct ContinuationProvider {
        requests: Mutex<Vec<ModelRequest>>,
    }

    impl ModelProvider for ContinuationProvider {
        fn complete(&self, request: ModelRequest) -> Result<ModelResponse, AgentError> {
            let mut requests = self.requests.lock().unwrap();
            let response = if requests.is_empty() {
                ModelResponse {
                    request_id: String::new(),
                    content: "I will read the file.".into(),
                    stop_reason: Some("tool_use".into()),
                    tools: vec![ProposedTool {
                        operation_id: Some("read-operation".into()),
                        tool_name: "read_file".into(),
                        intent: ToolIntent::ReadFile { path: "known.txt".into() },
                    }],
                    tool_calls: Vec::new(),
                }
            } else {
                assert_eq!(request.model, "test-model");
                assert_eq!(request.parameters.temperature, Some(0.25));
                assert!(request.prompt.messages.iter().any(|message| {
                    message.role == "user"
                        && message.content.contains("KNOWN TOOL CONTENT")
                        && message.content.contains("stdout")
                }));
                ModelResponse {
                    request_id: String::new(),
                    content: "The final answer uses KNOWN TOOL CONTENT.".into(),
                    stop_reason: Some("stop".into()),
                    tools: Vec::new(),
                    tool_calls: Vec::new(),
                }
            };
            requests.push(request);
            Ok(response)
        }
    }

    #[test]
    fn public_runtime_continues_same_run_with_tool_artifact_content() {
        let sessions = SessionEngine::new(SqliteEventStore::in_memory().unwrap());
        let (session_id, _) = sessions.create_session(vec![".".into()], None).unwrap();
        let provider = Arc::new(ContinuationProvider { requests: Mutex::new(Vec::new()) });
        let runtime = AgentRuntime::new(
            provider.clone(),
            sessions.clone(),
            PromptLayers::default(),
            vec![],
        );
        let first = runtime
            .run_turn(
                &session_id,
                "turn-1",
                "Read known.txt",
                "test-model",
                Vec::new(),
                ModelParameters {
                    temperature: Some(0.25),
                    ..ModelParameters::default()
                },
            )
            .unwrap();
        let operation_id = first.tools[0].operation_id.clone().unwrap();
        let run_id = sessions.replay(&session_id).unwrap().turns["turn-1"].run_id.clone();
        sessions
            .append(
                &session_id,
                EventPayload::PolicyEvaluated {
                    operation_id: operation_id.clone(),
                    request_digest: "digest".into(),
                    policy_version: "test".into(),
                    outcome: PolicyOutcome::Allowed,
                    reason: "test".into(),
                },
                Some("turn-1".into()),
                None,
            )
            .unwrap();
        sessions
            .append(
                &session_id,
                EventPayload::ExecutionRequested {
                    operation_id: operation_id.clone(),
                    request_digest: "digest".into(),
                    backend: "test".into(),
                    external_id: None,
                },
                Some("turn-1".into()),
                None,
            )
            .unwrap();
        sessions
            .append(
                &session_id,
                EventPayload::ToolStarted {
                    operation_id: operation_id.clone(),
                    backend: "test".into(),
                    external_id: None,
                },
                Some("turn-1".into()),
                None,
            )
            .unwrap();
        sessions
            .finish_operation(
                &session_id,
                &operation_id,
                "turn-1",
                &run_id,
                ToolAuditResult {
                    status: ToolStatus::Succeeded,
                    exit_code: Some(0),
                    stdout_digest: "sha256:test".into(),
                    stderr_digest: "sha256:empty".into(),
                    stdout_bytes: 18,
                    stderr_bytes: 0,
                    bytes_written: None,
                    output_truncated: false,
                    duration_ms: 1,
                    stdout_artifact_id: None,
                    stderr_artifact_id: None,
                },
                vec![ArtifactInput {
                    kind: "stdout".into(),
                    content: "KNOWN TOOL CONTENT".into(),
                    media_type: "text/plain; charset=utf-8".into(),
                }],
                false,
            )
            .unwrap();

        let response = runtime.continue_run(&session_id, "turn-1", &run_id).unwrap();
        assert_eq!(response.content, "The final answer uses KNOWN TOOL CONTENT.");
        let events = sessions.effective_events(&session_id).unwrap();
        assert_eq!(events.iter().filter(|event| matches!(event.payload, EventPayload::TurnStarted { .. })).count(), 1);
        assert_eq!(events.iter().filter(|event| matches!(event.payload, EventPayload::UserMessage { .. })).count(), 1);
        assert_eq!(events.iter().filter(|event| matches!(event.payload, EventPayload::RunStarted { .. })).count(), 1);
        assert_eq!(events.iter().filter(|event| matches!(event.payload, EventPayload::ModelRequested { .. })).count(), 2);
        assert!(matches!(events.last().unwrap().payload, EventPayload::RunCompleted { success: true, .. }));
    }

    #[test]
    fn coding_turn_can_read_edit_fail_test_fix_and_answer_from_real_artifacts() {
        use harness_policy_engine::{PolicyConfig, PolicyEngine};
        use std::fs;
        use tempfile::tempdir;

        struct CodingProvider(Mutex<usize>);
        impl ModelProvider for CodingProvider {
            fn complete(&self, request: ModelRequest) -> Result<ModelResponse, AgentError> {
                let mut step = self.0.lock().unwrap();
                let context = request.prompt.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n");
                let (name, intent) = match *step {
                    0 => ("read_file", Some(ToolIntent::ReadFile { path: "answer.py".into() })),
                    1 => {
                        assert!(context.contains("print('old')"));
                        ("edit_file", Some(ToolIntent::EditFile { path: "answer.py".into(), old_text: "old".into(), new_text: "broken".into(), expected_sha256: None }))
                    }
                    2 => ("test", Some(ToolIntent::Test { program: "/usr/bin/python3".into(), args: vec!["-c".into(), "import pathlib; assert pathlib.Path('answer.py').read_text() == \"print('fixed')\\n\", 'expected fixed'".into()], cwd: None, timeout_ms: Some(10_000) })),
                    3 => {
                        assert!(context.contains("expected fixed"), "model must see failed test stderr: {context}");
                        ("edit_file", Some(ToolIntent::EditFile { path: "answer.py".into(), old_text: "broken".into(), new_text: "fixed".into(), expected_sha256: None }))
                    }
                    4 => ("test", Some(ToolIntent::Test { program: "/usr/bin/python3".into(), args: vec!["-c".into(), "import pathlib; assert pathlib.Path('answer.py').read_text() == \"print('fixed')\\n\"; print('PASS')".into()], cwd: None, timeout_ms: Some(10_000) })),
                    5 => { assert!(context.contains("PASS")); ("final", None) }
                    6 => { assert!(context.contains("PASS"), "a new turn must restore persisted tool artifact bodies"); ("final", None) }
                    _ => panic!("unexpected model request"),
                };
                *step += 1;
                Ok(ModelResponse { request_id: request.request_id, content: if intent.is_some() { "working".into() } else { "Fixed and tested.".into() }, stop_reason: None, tools: intent.map(|intent| vec![ProposedTool { operation_id: None, tool_name: name.into(), intent }]).unwrap_or_default(), tool_calls: Vec::new() })
            }
        }
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("answer.py"), "print('old')\n").unwrap();
        let sessions = SessionEngine::new(SqliteEventStore::in_memory().unwrap());
        let (session_id, _) = sessions.create_session(vec![directory.path().display().to_string()], None).unwrap();
        let provider = Arc::new(CodingProvider(Mutex::new(0)));
        let runtime = AgentRuntime::new(provider.clone(), sessions.clone(), PromptLayers::default(), vec![]);
        let mut policy = PolicyConfig::trusted_workspace(vec![directory.path().to_path_buf()]);
        policy.allow_trusted_host_process = true;
        policy.allowed_programs.insert("/usr/bin/python3".into());
        let coordinator = ToolCoordinator::new(sessions.clone(), ExecutionBroker::new(PolicyEngine::new(policy).unwrap()));
        let mut response = runtime.run_turn(&session_id, "turn-1", "Fix answer.py", "fake", vec![], ModelParameters::default()).unwrap();
        let run_id = sessions.replay(&session_id).unwrap().turns["turn-1"].run_id.clone();
        for step in 0..5 {
            let proposal = &response.tools[0];
            let operation_id = proposal.operation_id.as_ref().unwrap();
            let grant = if proposal.intent.is_read_only() { None } else {
                let prepared = coordinator.broker.prepare(ExecutionContext { session_id: session_id.clone(), principal: "user".into(), backend: "local-trusted-host".into(), workspace_roots: vec![directory.path().display().to_string()] }, operation_id.clone(), proposal.intent.clone()).unwrap();
                Some(ApprovalGrant { session_id: session_id.clone(), operation_id: operation_id.clone(), request_digest: prepared.request_digest().into(), principal: "user".into(), actor: "user".into(), nonce: Uuid::new_v4().to_string(), expires_at_ms: harness_protocol::now_ms() + 60_000 })
            };
            let result = coordinator.execute(&session_id, "turn-1", &run_id, "user", operation_id, &proposal.tool_name, proposal.intent.clone(), grant).unwrap();
            if step == 2 { assert_eq!(result.status, ToolStatus::Failed); }
            response = runtime.continue_run(&session_id, "turn-1", &run_id).unwrap();
        }
        assert_eq!(response.content, "Fixed and tested.");
        assert_eq!(fs::read_to_string(directory.path().join("answer.py")).unwrap(), "print('fixed')\n");
        assert!(matches!(sessions.effective_events(&session_id).unwrap().last().unwrap().payload, EventPayload::RunCompleted { success: true, .. }));
        runtime.run_turn(&session_id, "turn-2", "What happened?", "fake", vec![], ModelParameters::default()).unwrap();
    }

    #[test]
    fn anthropic_request_uses_messages_shape_without_credentials() {
        let provider = AnthropicProvider::new(
            "https://api.anthropic.example/v1/messages",
            Some("secret-value".to_string()),
            10_000,
        )
        .unwrap();
        let body = provider
            .request_body(&ModelRequest {
                session_id: "session".into(),
                turn_id: "turn".into(),
                run_id: "run".into(),
                request_id: "request".into(),
                model: "claude-test".into(),
                prompt: CompiledPrompt {
                    compiler_version: "prompt-compiler.v1".into(),
                    messages: vec![harness_prompt_compiler::ModelMessage {
                        role: "user".into(),
                        content: "hello".into(),
                    }],
                    stable_prefix: String::new(),
                    dynamic_tail: Vec::new(),
                    digest: "digest".into(),
                },
                tools: Vec::new(),
                attachments: Vec::new(),
                parameters: ModelParameters {
                    reasoning_effort: Some("medium".into()),
                    temperature: Some(0.4),
                    max_output_tokens: Some(2048),
                },
            })
            .unwrap();
        assert_eq!(body["model"], "claude-test");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["max_tokens"], 5120);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 4096);
        assert!(body.get("temperature").is_none());
        assert!(!body.to_string().contains("secret-value"));
    }

    #[test]
    fn provider_request_is_exact_and_does_not_log_credentials() {
        let provider = OpenAiCompatibleProvider::new(
            "https://provider.example/v1/chat/completions",
            Some("secret-value".to_string()),
            10_000,
        )
        .unwrap();
        let body = provider
            .request_body(&ModelRequest {
                session_id: "session".into(),
                turn_id: "turn".into(),
                run_id: "run".into(),
                request_id: "request".into(),
                model: "model".into(),
                prompt: CompiledPrompt {
                    compiler_version: "prompt-compiler.v1".into(),
                    messages: vec![harness_prompt_compiler::ModelMessage {
                        role: "user".into(),
                        content: "hello".into(),
                    }],
                    stable_prefix: String::new(),
                    dynamic_tail: Vec::new(),
                    digest: "digest".into(),
                },
                tools: Vec::new(),
                attachments: Vec::new(),
                parameters: ModelParameters {
                    reasoning_effort: Some("high".into()),
                    temperature: Some(0.7),
                    max_output_tokens: Some(2048),
                },
            })
            .unwrap();
        assert_eq!(body["model"], "model");
        assert_eq!(body["stream"], false);
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_completion_tokens"], 2048);
        assert!(body.get("max_tokens").is_none());
        assert!(!body.to_string().contains("secret-value"));
    }

    #[test]
    fn codex_jsonl_prefers_the_final_agent_message() {
        let output = r#"
{"type":"thread.started","thread_id":"thread-1"}
{"type":"item.completed","item":{"type":"agent_message","text":"intermediate"}}
{"type":"turn.completed"}
{"type":"item.completed","item":{"type":"agent_message","text":"final answer"}}
"#;
        assert_eq!(parse_codex_jsonl(output).as_deref(), Some("final answer"));
    }

    #[test]
    fn codex_jsonl_supports_message_content_and_plain_text_fallback() {
        assert_eq!(
            parse_codex_jsonl(r#"{"message":{"role":"assistant","content":"hello"}}"#)
                .as_deref(),
            Some("hello")
        );
        assert_eq!(parse_codex_jsonl("plain codex output").as_deref(), Some("plain codex output"));
    }

    #[test]
    fn codex_prompt_keeps_message_roles() {
        let prompt = CompiledPrompt {
            compiler_version: "prompt-compiler.v1".into(),
            messages: vec![
                harness_prompt_compiler::ModelMessage {
                    role: "system".into(),
                    content: "rules".into(),
                },
                harness_prompt_compiler::ModelMessage {
                    role: "user".into(),
                    content: "question".into(),
                },
            ],
            stable_prefix: String::new(),
            dynamic_tail: Vec::new(),
            digest: "digest".into(),
        };
        assert_eq!(codex_prompt(&prompt), "[system]\nrules\n\n[user]\nquestion");
    }
}
