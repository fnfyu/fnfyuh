//! The only execution seam for file and process side effects.
//!
//! The trusted-workspace local adapter is intentionally explicit and is not a hostile
//! sandbox. The Docker adapter is selected separately and reports its enforceable
//! isolation facts; unavailable engines fail closed rather than falling back to host.

use base64::Engine;
use harness_policy_engine::{PolicyDecision, PolicyEngine, PolicyError};
use harness_protocol::{
    canonical_json, now_ms, BackendOperationState, ToolAuditResult, ToolIntent, ToolResult,
    ToolStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PROCESS_TIMEOUT_MS: u64 = 120_000;
const MAX_IMAGE_BYTES: u64 = 1 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum BrokerError {
    #[error("policy denied execution: {0}")]
    Policy(#[from] PolicyError),
    #[error("approval is required for request {0}")]
    ApprovalRequired(String),
    #[error("approval does not match the exact request digest")]
    ApprovalDigestMismatch,
    #[error("approval has expired")]
    ApprovalExpired,
    #[error("approval nonce has already been consumed")]
    ApprovalAlreadyUsed,
    #[error("approval principal or session does not match the request")]
    ApprovalScopeMismatch,
    #[error("approval nonce is empty")]
    ApprovalNonceMissing,
    #[error("session workspace does not match the policy-bound workspace")]
    WorkspaceMismatch,
    #[error("operation `{0}` is already authorized")]
    AlreadyAuthorized(String),
    #[error("filesystem error: {0}")]
    Io(#[from] io::Error),
    #[error("execution backend is unavailable and failed closed")]
    BackendUnavailable,
    #[error("file is too large to load through the local adapter")]
    FileTooLarge,
    #[error("expected text was not found exactly once")]
    EditMismatch,
    #[error("expected file digest does not match")]
    FileDigestMismatch,
    #[error("process output reader failed")]
    OutputReader,
    #[error("execution approval state was poisoned")]
    Poisoned,
    #[error("the requested backend does not match the configured adapter")]
    BackendMismatch,
    #[error("backend inspection is not supported for this operation")]
    BackendInspectUnsupported,
    #[error("backend operation is still running")]
    BackendOperationRunning,
    #[error("backend runtime error: {0}")]
    Runtime(String),
}

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub session_id: String,
    pub principal: String,
    pub backend: String,
    pub workspace_roots: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedExecution {
    operation_id: String,
    intent: ToolIntent,
    decision: PolicyDecision,
    context: ExecutionContext,
    bound_request_digest: String,
}

impl PreparedExecution {
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn intent(&self) -> &ToolIntent {
        &self.intent
    }

    pub fn decision(&self) -> &PolicyDecision {
        &self.decision
    }

    pub fn request_digest(&self) -> &str {
        &self.bound_request_digest
    }

    pub fn context(&self) -> &ExecutionContext {
        &self.context
    }
}

#[derive(Debug, Clone)]
pub struct ApprovalGrant {
    pub session_id: String,
    pub operation_id: String,
    pub request_digest: String,
    pub principal: String,
    pub actor: String,
    pub nonce: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct AuthorizedExecution {
    prepared: PreparedExecution,
    grant: Option<ApprovalGrant>,
}

impl AuthorizedExecution {
    pub fn operation_id(&self) -> &str {
        &self.prepared.operation_id
    }

    pub fn decision(&self) -> &PolicyDecision {
        &self.prepared.decision
    }

    pub fn request_digest(&self) -> &str {
        &self.prepared.bound_request_digest
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendCapabilities {
    pub name: String,
    pub available: bool,
    pub reason: Option<String>,
    pub trusted_workspace_only: bool,
    pub handle_relative_fs: bool,
    pub no_symlink: bool,
    pub process_isolation: bool,
    pub network_isolation: bool,
    pub secret_injection: bool,
}

#[derive(Debug, Clone)]
pub struct BackendOperation {
    pub operation_id: String,
    pub backend: String,
    pub external_id: Option<String>,
    pub intent: ToolIntent,
    pub context: ExecutionContext,
}

#[derive(Debug, Clone)]
pub struct BackendInspection {
    pub state: BackendOperationState,
    pub external_id: Option<String>,
    pub detail: Option<String>,
    pub result: Option<ToolResult>,
}

/// Adapter seam shared by local, container and future VM backends. A backend must
/// report the controls it can actually enforce; policy must not infer isolation
/// from a command allowlist.
pub trait ExecutionAdapter: Send + Sync {
    fn capabilities(&self) -> BackendCapabilities;
    fn execute(&self, authorized: AuthorizedExecution) -> Result<ToolResult, BrokerError>;

    fn external_id(&self, _authorized: &AuthorizedExecution) -> Option<String> {
        None
    }

    fn inspect(&self, _operation: &BackendOperation) -> Result<BackendInspection, BrokerError> {
        Ok(BackendInspection {
            state: BackendOperationState::NotTracked,
            external_id: None,
            detail: Some("backend does not retain an external operation handle".to_string()),
            result: None,
        })
    }

    fn continue_operation(
        &self,
        _authorized: AuthorizedExecution,
    ) -> Result<ToolResult, BrokerError> {
        Err(BrokerError::BackendInspectUnsupported)
    }
}

/// Placeholder for the isolated backend seam. It intentionally has no ambient
/// powers yet; callers cannot accidentally mistake a missing sandbox for safety.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableContainerBackend;

impl ExecutionAdapter for UnavailableContainerBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            name: "container-unavailable".to_string(),
            available: false,
            reason: Some("isolated backend is not configured".to_string()),
            trusted_workspace_only: false,
            handle_relative_fs: false,
            no_symlink: false,
            process_isolation: false,
            network_isolation: false,
            secret_injection: false,
        }
    }

    fn execute(&self, _authorized: AuthorizedExecution) -> Result<ToolResult, BrokerError> {
        Err(BrokerError::BackendUnavailable)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerBackendConfig {
    pub runtime: String,
    pub image: String,
    pub vm_isolation: bool,
    pub memory: String,
    pub cpus: String,
    pub pids_limit: u64,
}

impl Default for ContainerBackendConfig {
    fn default() -> Self {
        Self {
            runtime: "docker".to_string(),
            image: "node:22-bookworm-slim".to_string(),
            vm_isolation: false,
            memory: "512m".to_string(),
            cpus: "2".to_string(),
            pids_limit: 128,
        }
    }
}

impl ContainerBackendConfig {
    pub fn preflight(&self) -> Result<(), String> {
        let runtime = Command::new(&self.runtime)
            .args(["version", "--format", "{{.Server.Version}}"])
            .output()
            .map_err(|error| format!("container runtime is unavailable: {error}"))?;
        if !runtime.status.success() {
            return Err(String::from_utf8_lossy(&runtime.stderr).trim().to_string());
        }
        let image = Command::new(&self.runtime)
            .args(["image", "inspect", &self.image])
            .output()
            .map_err(|error| format!("container image preflight failed: {error}"))?;
        if !image.status.success() {
            return Err(format!(
                "container image `{}` is not available locally; use an immutable digest and pull it explicitly",
                self.image
            ));
        }
        Ok(())
    }

    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            runtime: std::env::var("HARNESS_CONTAINER_RUNTIME")
                .unwrap_or(defaults.runtime),
            image: std::env::var("HARNESS_CONTAINER_IMAGE").unwrap_or(defaults.image),
            vm_isolation: std::env::var("HARNESS_CONTAINER_ISOLATION")
                .map(|value| value.eq_ignore_ascii_case("vm") || value.eq_ignore_ascii_case("hyperv"))
                .unwrap_or(defaults.vm_isolation),
            memory: std::env::var("HARNESS_CONTAINER_MEMORY").unwrap_or(defaults.memory),
            cpus: std::env::var("HARNESS_CONTAINER_CPUS").unwrap_or(defaults.cpus),
            pids_limit: std::env::var("HARNESS_CONTAINER_PIDS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(defaults.pids_limit),
        }
    }
}

#[derive(Clone)]
pub struct DockerContainerBackend {
    policy: PolicyEngine,
    config: ContainerBackendConfig,
    used_approvals: Arc<Mutex<BTreeSet<String>>>,
    write_lock: Arc<Mutex<()>>,
}

impl DockerContainerBackend {
    pub fn new(
        policy: PolicyEngine,
        config: ContainerBackendConfig,
        used_approvals: Arc<Mutex<BTreeSet<String>>>,
        write_lock: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            policy,
            config,
            used_approvals,
            write_lock,
        }
    }

    pub fn name(&self) -> String {
        if self.config.vm_isolation {
            "docker-hyperv-vm".to_string()
        } else {
            "docker-isolated".to_string()
        }
    }

    fn validate_and_consume(&self, authorized: &AuthorizedExecution) -> Result<(), BrokerError> {
        let reevaluated = self.policy.evaluate(&authorized.prepared.intent)?;
        let expected_digest = bound_request_digest(
            &reevaluated.request_digest,
            &authorized.prepared.context,
            &authorized.prepared.operation_id,
            &authorized.prepared.intent,
        );
        if expected_digest != authorized.prepared.bound_request_digest {
            return Err(BrokerError::ApprovalDigestMismatch);
        }
        if reevaluated.requires_approval {
            let Some(grant) = authorized.grant.as_ref() else {
                return Err(BrokerError::ApprovalRequired(
                    authorized.prepared.operation_id.clone(),
                ));
            };
            validate_grant(&authorized.prepared, grant)?;
            let mut used = self
                .used_approvals
                .lock()
                .map_err(|_| BrokerError::Poisoned)?;
            if !used.insert(grant.nonce.clone()) {
                return Err(BrokerError::ApprovalAlreadyUsed);
            }
        }
        Ok(())
    }

    fn operation_name(&self, operation_id: &str, session_id: &str) -> String {
        let digest = Sha256::digest(format!("{session_id}:{operation_id}").as_bytes());
        let name = format!("harness-op-{:x}", digest);
        name[..32].to_string()
    }

    fn common_args(&self, authorized: &AuthorizedExecution) -> Result<Vec<String>, BrokerError> {
        let root = self
            .policy
            .config()
            .workspace_roots
            .first()
            .ok_or(BrokerError::WorkspaceMismatch)?;
        let operation_name = self.operation_name(
            &authorized.prepared.operation_id,
            &authorized.prepared.context.session_id,
        );
        let workdir = match authorized.prepared.decision.resolved_cwd.as_ref() {
            Some(cwd) => self.container_path(authorized, Some(cwd))?,
            None => "/workspace".to_string(),
        };
        let mut args = vec![
            "run".to_string(),
            "--interactive".to_string(),
            "--name".to_string(),
            operation_name,
            "--label".to_string(),
            format!("com.local-first-harness.operation={}", authorized.prepared.operation_id),
            "--network".to_string(),
            "none".to_string(),
            "--user".to_string(),
            "1000:1000".to_string(),
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "--read-only".to_string(),
            "--tmpfs".to_string(),
            "/tmp:rw,noexec,nosuid,nodev".to_string(),
            "--pids-limit".to_string(),
            self.config.pids_limit.to_string(),
            "--memory".to_string(),
            self.config.memory.clone(),
            "--cpus".to_string(),
            self.config.cpus.clone(),
            "--mount".to_string(),
            format!("type=bind,source={},destination=/workspace,rw", root.display()),
            "--workdir".to_string(),
            workdir,
        ];
        if self.config.vm_isolation {
            args.push("--isolation=hyperv".to_string());
        }
        args.push("--pull=never".to_string());
        args.push(self.config.image.clone());
        Ok(args)
    }

    fn container_path(
        &self,
        _authorized: &AuthorizedExecution,
        resolved: Option<&String>,
    ) -> Result<String, BrokerError> {
        let root = self
            .policy
            .config()
            .workspace_roots
            .first()
            .ok_or(BrokerError::WorkspaceMismatch)?;
        let path = Path::new(resolved.ok_or_else(|| {
            BrokerError::Runtime("policy did not bind a path".to_string())
        })?);
        let relative = path
            .strip_prefix(root)
            .map_err(|_| BrokerError::WorkspaceMismatch)?
            .to_string_lossy()
            .replace('\\', "/");
        if relative.is_empty() {
            Ok("/workspace".to_string())
        } else {
            Ok(format!("/workspace/{relative}"))
        }
    }

    fn program_path(&self, _authorized: &AuthorizedExecution, program: &str) -> Result<String, BrokerError> {
        let root = self
            .policy
            .config()
            .workspace_roots
            .first()
            .ok_or(BrokerError::WorkspaceMismatch)?;
        let path = Path::new(program);
        if let Ok(relative) = path.strip_prefix(root) {
            let relative = relative.to_string_lossy().replace('\\', "/");
            return Ok(format!("/workspace/{relative}"));
        }
        if program.starts_with('/') {
            Ok(program.to_string())
        } else {
            Err(BrokerError::Runtime(
                "container programs must be absolute image paths".to_string(),
            ))
        }
    }

    fn run_container(
        &self,
        authorized: &AuthorizedExecution,
        mut command: Vec<String>,
        input: Option<&[u8]>,
        started: Instant,
    ) -> Result<ToolResult, BrokerError> {
        let result = run_process_with_input(
            &self.config.runtime,
            &mut command,
            input,
            None,
            started,
        )?;
        let container_name = self.operation_name(
            &authorized.prepared.operation_id,
            &authorized.prepared.context.session_id,
        );
        let _ = Command::new(&self.config.runtime)
            .args(["rm", "-f", container_name.as_str()])
            .output();
        Ok(result)
    }

    fn read_container_file(
        &self,
        authorized: &AuthorizedExecution,
        path: String,
    ) -> Result<ToolResult, BrokerError> {
        let mut command = self.common_args(authorized)?;
        command.extend(["/bin/cat".to_string(), path]);
        self.run_container(authorized, command, None, Instant::now())
    }

    fn read_container_image(
        &self,
        authorized: &AuthorizedExecution,
        path: String,
        media_type: String,
    ) -> Result<ToolResult, BrokerError> {
        let mut command = self.common_args(authorized)?;
        command.extend(["/usr/bin/base64".to_string(), "-w0".to_string(), path]);
        let mut result = self.run_container(authorized, command, None, Instant::now())?;
        result.output_media_type = Some(media_type);
        result.output_encoding = Some("base64".to_string());
        Ok(result)
    }
}

impl ExecutionAdapter for DockerContainerBackend {
    fn capabilities(&self) -> BackendCapabilities {
        let preflight = self.config.preflight();
        BackendCapabilities {
            name: self.name(),
            available: preflight.is_ok(),
            reason: preflight.err(),
            trusted_workspace_only: false,
            handle_relative_fs: true,
            no_symlink: true,
            process_isolation: true,
            network_isolation: true,
            secret_injection: false,
        }
    }

    fn external_id(&self, authorized: &AuthorizedExecution) -> Option<String> {
        Some(self.operation_name(
            &authorized.prepared.operation_id,
            &authorized.prepared.context.session_id,
        ))
    }

    fn execute(&self, authorized: AuthorizedExecution) -> Result<ToolResult, BrokerError> {
        self.validate_and_consume(&authorized)?;
        let started = Instant::now();
        match &authorized.prepared.intent {
            ToolIntent::ReadFile { .. } => {
                let path = self.container_path(
                    &authorized,
                    authorized.prepared.decision.resolved_path.as_ref(),
                )?;
                self.read_container_file(&authorized, path)
            }
            ToolIntent::Search { query, .. } => {
                let root = self.container_path(
                    &authorized,
                    authorized.prepared.decision.resolved_path.as_ref(),
                )?;
                let mut command = self.common_args(&authorized)?;
                command.extend([
                    "/bin/grep".to_string(),
                    "-RInF".to_string(),
                    "--".to_string(),
                    query.clone(),
                    root,
                ]);
                self.run_container(&authorized, command, None, started)
            }
            ToolIntent::ListFiles { depth, include_hidden, .. } => {
                let root = self.container_path(
                    &authorized,
                    authorized.prepared.decision.resolved_path.as_ref(),
                )?;
                let mut command = self.common_args(&authorized)?;
                command.extend([
                    "/usr/bin/find".to_string(),
                    root,
                    "-mindepth".to_string(),
                    "1".to_string(),
                    "-maxdepth".to_string(),
                    (depth.saturating_add(1)).to_string(),
                ]);
                if !include_hidden {
                    command.extend(["!".to_string(), "-path".to_string(), "*/.*".to_string()]);
                }
                command.push("-print".to_string());
                self.run_container(&authorized, command, None, started)
            }
            ToolIntent::ReadImage { .. } => {
                let root = self.container_path(
                    &authorized,
                    authorized.prepared.decision.resolved_path.as_ref(),
                )?;
                let media_type = image_media_type(
                    authorized
                        .prepared
                        .decision
                        .resolved_path
                        .as_deref()
                        .map(Path::new)
                        .ok_or(BrokerError::WorkspaceMismatch)?,
                )
                .ok_or_else(|| BrokerError::Io(io::Error::new(io::ErrorKind::InvalidInput, "unsupported image type")))?;
                self.read_container_image(&authorized, root, media_type.to_string())
            }
            ToolIntent::WriteFile { content, .. } => {
                let _write_guard = self.write_lock.lock().map_err(|_| BrokerError::Poisoned)?;
                let path = self.container_path(
                    &authorized,
                    authorized.prepared.decision.resolved_path.as_ref(),
                )?;
                let mut command = self.common_args(&authorized)?;
                command.extend(["/usr/bin/tee".to_string(), path]);
                let mut result = self.run_container(
                    &authorized,
                    command,
                    Some(content.as_bytes()),
                    started,
                )?;
                result.stdout.clear();
                Ok(result)
            }
            ToolIntent::EditFile {
                old_text,
                new_text,
                expected_sha256,
                ..
            } => {
                let _write_guard = self.write_lock.lock().map_err(|_| BrokerError::Poisoned)?;
                let path = self.container_path(
                    &authorized,
                    authorized.prepared.decision.resolved_path.as_ref(),
                )?;
                let read = self.read_container_file(&authorized, path.clone())?;
                if !matches!(read.status, ToolStatus::Succeeded) {
                    return Ok(read);
                }
                if let Some(expected) = expected_sha256 {
                    if sha256(&read.stdout) != *expected {
                        return Err(BrokerError::FileDigestMismatch);
                    }
                }
                if read.stdout.match_indices(old_text).count() != 1 {
                    return Err(BrokerError::EditMismatch);
                }
                let updated = read.stdout.replacen(old_text, new_text, 1);
                let mut command = self.common_args(&authorized)?;
                command.extend(["/usr/bin/tee".to_string(), path]);
                let mut result = self.run_container(
                    &authorized,
                    command,
                    Some(updated.as_bytes()),
                    started,
                )?;
                result.stdout.clear();
                result.bytes_written = Some(updated.len() as u64);
                Ok(result)
            }
            ToolIntent::Exec {
                program,
                args,
                ..
            }
            | ToolIntent::Test {
                program,
                args,
                ..
            } => {
                let mut command = self.common_args(&authorized)?;
                command.push(self.program_path(&authorized, program)?);
                command.extend(args.clone());
                self.run_container(&authorized, command, None, started)
            }
            ToolIntent::Git { args, .. } => {
                let mut command = self.common_args(&authorized)?;
                command.push("/usr/bin/git".to_string());
                command.extend(args.clone());
                self.run_container(&authorized, command, None, started)
            }
        }
    }

    fn inspect(&self, operation: &BackendOperation) -> Result<BackendInspection, BrokerError> {
        let Some(external_id) = operation.external_id.as_deref() else {
            return Ok(BackendInspection {
                state: BackendOperationState::NotTracked,
                external_id: None,
                detail: Some("operation has no container handle".to_string()),
                result: None,
            });
        };
        let output = Command::new(&self.config.runtime)
            .args(["inspect", "--format", "{{json .State}}", external_id])
            .output()
            .map_err(|error| BrokerError::Runtime(error.to_string()))?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let lower = detail.to_ascii_lowercase();
            let state = if lower.contains("no such object") || lower.contains("not found") {
                BackendOperationState::NotFound
            } else {
                BackendOperationState::Unknown
            };
            return Ok(BackendInspection {
                state,
                external_id: Some(external_id.to_string()),
                detail: Some(detail),
                result: None,
            });
        }
        let state: Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| BrokerError::Runtime(format!("invalid docker inspect response: {error}")))?;
        let status = state.get("Status").and_then(Value::as_str).unwrap_or("unknown");
        if matches!(status, "running" | "created" | "paused" | "restarting") {
            return Ok(BackendInspection {
                state: BackendOperationState::Running,
                external_id: Some(external_id.to_string()),
                detail: Some(status.to_string()),
                result: None,
            });
        }
        if !matches!(status, "exited" | "dead") {
            return Ok(BackendInspection {
                state: BackendOperationState::Unknown,
                external_id: Some(external_id.to_string()),
                detail: Some(status.to_string()),
                result: None,
            });
        }
        let logs = Command::new(&self.config.runtime)
            .args(["logs", external_id])
            .output()
            .map_err(|error| BrokerError::Runtime(error.to_string()))?;
        let exit_code = state.get("ExitCode").and_then(Value::as_i64).map(|code| code as i32);
        let stdout = String::from_utf8_lossy(&logs.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&logs.stderr).into_owned();
        let result = ToolResult {
            status: if exit_code == Some(0) {
                ToolStatus::Succeeded
            } else {
                ToolStatus::Failed
            },
            exit_code,
            stdout,
            stderr,
            bytes_written: None,
            output_truncated: false,
            duration_ms: 0,
            output_media_type: None,
            output_encoding: None,
        };
        Ok(BackendInspection {
            state: if exit_code == Some(0) {
                BackendOperationState::Succeeded
            } else {
                BackendOperationState::Failed
            },
            external_id: Some(external_id.to_string()),
            detail: Some(status.to_string()),
            result: Some(result),
        })
    }

    fn continue_operation(
        &self,
        authorized: AuthorizedExecution,
    ) -> Result<ToolResult, BrokerError> {
        let operation = BackendOperation {
            operation_id: authorized.prepared.operation_id.clone(),
            backend: self.name(),
            external_id: self.external_id(&authorized),
            intent: authorized.prepared.intent.clone(),
            context: authorized.prepared.context.clone(),
        };
        match self.inspect(&operation)?.state {
            BackendOperationState::Running => Err(BrokerError::BackendOperationRunning),
            BackendOperationState::Succeeded | BackendOperationState::Failed => {
                self.inspect(&operation)?.result.ok_or_else(|| {
                    BrokerError::Runtime("backend completed without a result".to_string())
                })
            }
            BackendOperationState::NotFound => self.execute(authorized),
            BackendOperationState::Unknown | BackendOperationState::NotTracked => {
                Err(BrokerError::BackendInspectUnsupported)
            }
        }
    }
}

fn process_approval_registry() -> Arc<Mutex<BTreeSet<String>>> {
    static USED_APPROVALS: OnceLock<Arc<Mutex<BTreeSet<String>>>> = OnceLock::new();
    USED_APPROVALS
        .get_or_init(|| Arc::new(Mutex::new(BTreeSet::new())))
        .clone()
}

#[derive(Clone)]
pub struct ExecutionBroker {
    policy: PolicyEngine,
    adapter: Arc<dyn ExecutionAdapter>,
}

impl ExecutionBroker {
    pub fn new(policy: PolicyEngine) -> Self {
        let used_approvals = process_approval_registry();
        let write_lock = Arc::new(Mutex::new(()));
        let adapter = Arc::new(LocalRestrictedBackend::new(
            policy.clone(),
            used_approvals.clone(),
            write_lock.clone(),
        ));
        Self { policy, adapter }
    }

    pub fn with_adapter(
        policy: PolicyEngine,
        adapter: Arc<dyn ExecutionAdapter>,
    ) -> Self {
        Self { policy, adapter }
    }

    pub fn with_container_backend(
        policy: PolicyEngine,
        config: ContainerBackendConfig,
    ) -> Self {
        let used_approvals = process_approval_registry();
        let write_lock = Arc::new(Mutex::new(()));
        let adapter = Arc::new(DockerContainerBackend::new(
            policy.clone(),
            config,
            used_approvals.clone(),
            write_lock.clone(),
        ));
        Self { policy, adapter }
    }

    pub fn policy(&self) -> &PolicyEngine {
        &self.policy
    }

    pub fn capabilities(&self) -> BackendCapabilities {
        self.adapter.capabilities()
    }

    pub fn external_id(&self, authorized: &AuthorizedExecution) -> Option<String> {
        self.adapter.external_id(authorized)
    }

    pub fn inspect(&self, operation: &BackendOperation) -> Result<BackendInspection, BrokerError> {
        self.adapter.inspect(operation)
    }

    pub fn continue_operation(
        &self,
        authorized: AuthorizedExecution,
    ) -> Result<ToolResult, BrokerError> {
        self.adapter.continue_operation(authorized)
    }

    pub fn workspace_matches(&self, workspace_roots: &[String]) -> bool {
        if workspace_roots.len() != 1 {
            return false;
        }
        let requested = Path::new(&workspace_roots[0])
            .canonicalize()
            .ok();
        requested.as_ref() == self.policy.config().workspace_roots.first()
    }

    pub fn prepare(
        &self,
        context: ExecutionContext,
        operation_id: impl Into<String>,
        intent: ToolIntent,
    ) -> Result<PreparedExecution, BrokerError> {
        if !self.workspace_matches(&context.workspace_roots) {
            return Err(BrokerError::WorkspaceMismatch);
        }
        let capabilities = self.capabilities();
        if context.backend != capabilities.name {
            return Err(BrokerError::BackendMismatch);
        }
        if !capabilities.available {
            return Err(BrokerError::BackendUnavailable);
        }
        let operation_id = operation_id.into();
        let decision = self.policy.evaluate(&intent)?;
        let bound_request_digest = bound_request_digest(
            &decision.request_digest,
            &context,
            &operation_id,
            &intent,
        );
        Ok(PreparedExecution {
            operation_id,
            intent,
            decision,
            context,
            bound_request_digest,
        })
    }

    pub fn authorize(
        &self,
        prepared: PreparedExecution,
        grant: Option<ApprovalGrant>,
    ) -> Result<AuthorizedExecution, BrokerError> {
        if prepared.decision.requires_approval {
            let Some(grant) = grant.as_ref() else {
                return Err(BrokerError::ApprovalRequired(
                    prepared.operation_id.clone(),
                ));
            };
            validate_grant(&prepared, grant)?;
        }
        Ok(AuthorizedExecution { prepared, grant })
    }

    pub fn validate(&self, authorized: &AuthorizedExecution) -> Result<(), BrokerError> {
        let reevaluated = self.policy.evaluate(&authorized.prepared.intent)?;
        let expected_digest = bound_request_digest(
            &reevaluated.request_digest,
            &authorized.prepared.context,
            &authorized.prepared.operation_id,
            &authorized.prepared.intent,
        );
        if expected_digest != authorized.prepared.bound_request_digest {
            return Err(BrokerError::ApprovalDigestMismatch);
        }
        if reevaluated.requires_approval {
            let Some(grant) = authorized.grant.as_ref() else {
                return Err(BrokerError::ApprovalRequired(
                    authorized.prepared.operation_id.clone(),
                ));
            };
            validate_grant(&authorized.prepared, grant)?;
        }
        Ok(())
    }

    pub fn execute(
        &self,
        authorized: AuthorizedExecution,
    ) -> Result<ToolResult, BrokerError> {
        self.adapter.execute(authorized)
    }
}

#[derive(Clone)]
pub struct LocalRestrictedBackend {
    policy: PolicyEngine,
    used_approvals: Arc<Mutex<BTreeSet<String>>>,
    write_lock: Arc<Mutex<()>>,
}

impl LocalRestrictedBackend {
    pub fn capabilities() -> BackendCapabilities {
        BackendCapabilities {
            name: "local-trusted-host".to_string(),
            available: true,
            reason: None,
            trusted_workspace_only: true,
            handle_relative_fs: false,
            no_symlink: false,
            process_isolation: false,
            network_isolation: false,
            secret_injection: false,
        }
    }

    pub fn new(
        policy: PolicyEngine,
        used_approvals: Arc<Mutex<BTreeSet<String>>>,
        write_lock: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            policy,
            used_approvals,
            write_lock,
        }
    }

    pub fn execute(&self, authorized: AuthorizedExecution) -> Result<ToolResult, BrokerError> {
        // Re-evaluate at the broker seam. A caller cannot broaden a previously
        // prepared decision by mutating an adapter or reusing a stale grant.
        let reevaluated = self.policy.evaluate(&authorized.prepared.intent)?;
        let expected_digest = bound_request_digest(
            &reevaluated.request_digest,
            &authorized.prepared.context,
            &authorized.prepared.operation_id,
            &authorized.prepared.intent,
        );
        if expected_digest != authorized.prepared.bound_request_digest {
            return Err(BrokerError::ApprovalDigestMismatch);
        }
        if reevaluated.requires_approval {
            let Some(grant) = authorized.grant.as_ref() else {
                return Err(BrokerError::ApprovalRequired(
                    authorized.prepared.operation_id.clone(),
                ));
            };
            validate_grant(&authorized.prepared, grant)?;
            let mut used = self
                .used_approvals
                .lock()
                .map_err(|_| BrokerError::Poisoned)?;
            if !used.insert(grant.nonce.clone()) {
                return Err(BrokerError::ApprovalAlreadyUsed);
            }
        }
        execute_intent(
            &reevaluated,
            &authorized.prepared.intent,
            &self.write_lock,
        )
    }
}

impl ExecutionAdapter for LocalRestrictedBackend {
    fn capabilities(&self) -> BackendCapabilities {
        Self::capabilities()
    }

    fn execute(&self, authorized: AuthorizedExecution) -> Result<ToolResult, BrokerError> {
        LocalRestrictedBackend::execute(self, authorized)
    }
}

fn validate_grant(
    prepared: &PreparedExecution,
    grant: &ApprovalGrant,
) -> Result<(), BrokerError> {
    if grant.operation_id != prepared.operation_id
        || grant.request_digest != prepared.bound_request_digest
    {
        return Err(BrokerError::ApprovalDigestMismatch);
    }
    if grant.session_id != prepared.context.session_id
        || grant.principal != prepared.context.principal
        || grant.nonce.is_empty()
        || grant.actor.is_empty()
    {
        return Err(if grant.nonce.is_empty() {
            BrokerError::ApprovalNonceMissing
        } else {
            BrokerError::ApprovalScopeMismatch
        });
    }
    if grant.expires_at_ms <= now_ms() {
        return Err(BrokerError::ApprovalExpired);
    }
    Ok(())
}

fn bound_request_digest(
    policy_digest: &str,
    context: &ExecutionContext,
    operation_id: &str,
    intent: &ToolIntent,
) -> String {
    let value = (
        policy_digest,
        &context.session_id,
        &context.principal,
        &context.backend,
        &context.workspace_roots,
        operation_id,
        intent,
    );
    let encoded = canonical_json(&value).expect("execution request is serializable");
    let digest = Sha256::digest(encoded.as_bytes());
    format!("sha256:{digest:x}")
}

fn execute_intent(
    decision: &PolicyDecision,
    intent: &ToolIntent,
    write_lock: &Mutex<()>,
) -> Result<ToolResult, BrokerError> {
    let started = Instant::now();
    match intent {
        ToolIntent::ReadFile { .. } => {
            let path = decision_path(decision)?;
            let metadata = fs::metadata(&path)?;
            if metadata.len() > MAX_FILE_BYTES {
                return Err(BrokerError::FileTooLarge);
            }
            let content = fs::read_to_string(path)?;
            Ok(success_result(content, String::new(), None, started))
        }
        ToolIntent::Search { query, .. } => {
            let root = decision_path(decision)?;
            let mut matches = Vec::new();
            search_directory(&root, query, &mut matches, 200)?;
            Ok(success_result(matches.join("\n"), String::new(), None, started))
        }
        ToolIntent::ListFiles { depth, include_hidden, .. } => {
            let root = decision_path(decision)?;
            let mut entries = Vec::new();
            list_directory(&root, *depth, *include_hidden, &mut entries, 2_000)?;
            Ok(success_result(entries.join("\n"), String::new(), None, started))
        }
        ToolIntent::ReadImage { max_bytes, .. } => {
            let path = decision_path(decision)?;
            read_image_result(&path, *max_bytes, started)
        }
        ToolIntent::WriteFile { content, .. } => {
            let _write_guard = write_lock.lock().map_err(|_| BrokerError::Poisoned)?;
            if content.len() as u64 > MAX_FILE_BYTES {
                return Err(BrokerError::FileTooLarge);
            }
            let path = decision_path(decision)?;
            if let Some(parent) = path.parent() {
                if !parent.exists() {
                    return Err(BrokerError::Io(io::Error::new(
                        io::ErrorKind::NotFound,
                        "write parent does not exist",
                    )));
                }
            }
            fs::write(&path, content.as_bytes())?;
            Ok(success_result(
                String::new(),
                String::new(),
                Some(content.len() as u64),
                started,
            ))
        }
        ToolIntent::EditFile {
            old_text,
            new_text,
            expected_sha256,
            ..
        } => {
            let _write_guard = write_lock.lock().map_err(|_| BrokerError::Poisoned)?;
            let path = decision_path(decision)?;
            if fs::metadata(&path)?.len() > MAX_FILE_BYTES {
                return Err(BrokerError::FileTooLarge);
            }
            let original = fs::read_to_string(&path)?;
            if let Some(expected) = expected_sha256 {
                if sha256(&original) != *expected {
                    return Err(BrokerError::FileDigestMismatch);
                }
            }
            let occurrences = original.match_indices(old_text).count();
            if occurrences != 1 {
                return Err(BrokerError::EditMismatch);
            }
            let updated = original.replacen(old_text, new_text, 1);
            if updated.len() as u64 > MAX_FILE_BYTES {
                return Err(BrokerError::FileTooLarge);
            }
            fs::write(&path, updated.as_bytes())?;
            Ok(success_result(
                String::new(),
                String::new(),
                Some(updated.len() as u64),
                started,
            ))
        }
        ToolIntent::Exec {
            program,
            args,
            timeout_ms,
            ..
        }
        | ToolIntent::Test {
            program,
            args,
            timeout_ms,
            ..
        } => run_process(
            program,
            args,
            decision_cwd(decision)?,
            *timeout_ms,
            started,
        ),
        ToolIntent::Git { args, .. } => run_process(
            "git",
            args,
            decision_cwd(decision)?,
            Some(120_000),
            started,
        ),
    }
}

fn decision_path(decision: &PolicyDecision) -> Result<std::path::PathBuf, BrokerError> {
    decision
        .resolved_path
        .as_ref()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            BrokerError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "policy did not bind a path",
            ))
        })
}

fn decision_cwd(decision: &PolicyDecision) -> Result<std::path::PathBuf, BrokerError> {
    decision
        .resolved_cwd
        .as_ref()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            BrokerError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "policy did not bind a cwd",
            ))
        })
}

fn list_directory(
    root: &std::path::Path,
    depth: u32,
    include_hidden: bool,
    entries: &mut Vec<String>,
    limit: usize,
) -> Result<(), io::Error> {
    if entries.len() >= limit {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if !metadata.is_dir() {
        entries.push(format!("{}\\tfile", root.display()));
        return Ok(());
    }
    let mut children = fs::read_dir(root)?.filter_map(Result::ok).collect::<Vec<_>>();
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        if entries.len() >= limit {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !include_hidden && name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let kind = if metadata.is_dir() { "directory" } else { "file" };
        entries.push(format!("{}\\t{kind}", path.display()));
        if metadata.is_dir() && depth > 0 {
            list_directory(&path, depth - 1, include_hidden, entries, limit)?;
        }
    }
    Ok(())
}

fn image_media_type(path: &std::path::Path) -> Option<&'static str> {
    match path.extension().and_then(|extension| extension.to_str()).map(|value| value.to_ascii_lowercase()).as_deref() {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("bmp") => Some("image/bmp"),
        Some("svg") => Some("image/svg+xml"),
        _ => None,
    }
}

fn read_image_result(
    path: &std::path::Path,
    requested_max_bytes: Option<u64>,
    started: Instant,
) -> Result<ToolResult, BrokerError> {
    let media_type = image_media_type(path).ok_or_else(|| {
        BrokerError::Io(io::Error::new(io::ErrorKind::InvalidInput, "unsupported image type"))
    })?;
    let max_bytes = requested_max_bytes.unwrap_or(MAX_IMAGE_BYTES).min(MAX_IMAGE_BYTES);
    let bytes = fs::read(path)?;
    if bytes.len() as u64 > max_bytes {
        return Err(BrokerError::FileTooLarge);
    }
    Ok(ToolResult {
        status: ToolStatus::Succeeded,
        exit_code: Some(0),
        stdout: base64::engine::general_purpose::STANDARD.encode(bytes),
        stderr: String::new(),
        bytes_written: None,
        output_truncated: false,
        duration_ms: started.elapsed().as_millis() as u64,
        output_media_type: Some(media_type.to_string()),
        output_encoding: Some("base64".to_string()),
    })
}

fn search_directory(
    root: &std::path::Path,
    query: &str,
    matches: &mut Vec<String>,
    limit: usize,
) -> Result<(), io::Error> {
    if matches.len() >= limit {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        if metadata.len() <= MAX_FILE_BYTES {
            let content = fs::read_to_string(root).unwrap_or_default();
            for (line_number, line) in content.lines().enumerate() {
                if line.contains(query) {
                    matches.push(format!("{}:{}:{}", root.display(), line_number + 1, line));
                    if matches.len() >= limit {
                        break;
                    }
                }
            }
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        if matches.len() >= limit {
            break;
        }
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            continue;
        }
        search_directory(&entry.path(), query, matches, limit)?;
    }
    Ok(())
}

fn run_process(
    program: &str,
    args: &[String],
    cwd: std::path::PathBuf,
    timeout_ms: Option<u64>,
    started: Instant,
) -> Result<ToolResult, BrokerError> {
    run_process_with_input_and_cwd(program, args, None, Some(cwd), timeout_ms, started)
}

fn run_process_with_input(
    program: &str,
    args: &mut [String],
    input: Option<&[u8]>,
    timeout_ms: Option<u64>,
    started: Instant,
) -> Result<ToolResult, BrokerError> {
    run_process_with_input_and_cwd(program, args, input, None, timeout_ms, started)
}

fn run_process_with_input_and_cwd(
    program: &str,
    args: &[String],
    input: Option<&[u8]>,
    cwd: Option<std::path::PathBuf>,
    timeout_ms: Option<u64>,
    started: Instant,
) -> Result<ToolResult, BrokerError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    // The executable is an absolute allowlisted path. Do not inherit PATH/PATHEXT
    // or arbitrary ambient environment into the trusted-host adapter.
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }

    let mut child = command.spawn()?;
    if let Some(input) = input {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input)?;
            drop(stdin);
        }
    }
    let stdout = child.stdout.take().ok_or(BrokerError::OutputReader)?;
    let stderr = child.stderr.take().ok_or(BrokerError::OutputReader)?;
    let output_limit_hit = Arc::new(AtomicBool::new(false));
    let stdout_flag = output_limit_hit.clone();
    let stderr_flag = output_limit_hit.clone();
    let stdout_thread = thread::spawn(move || read_bounded(stdout, stdout_flag));
    let stderr_thread = thread::spawn(move || read_bounded(stderr, stderr_flag));

    let effective_timeout = timeout_ms
        .unwrap_or(MAX_PROCESS_TIMEOUT_MS)
        .min(MAX_PROCESS_TIMEOUT_MS);
    let mut timed_out = false;
    let exit_code;
    loop {
        if output_limit_hit.load(Ordering::Acquire) {
            child.kill()?;
            exit_code = child.wait()?.code();
            break;
        }
        if let Some(status) = child.try_wait()? {
            exit_code = status.code();
            break;
        }
        if started.elapsed() >= Duration::from_millis(effective_timeout) {
            timed_out = true;
            child.kill()?;
            exit_code = child.wait()?.code();
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let stdout = stdout_thread
        .join()
        .map_err(|_| BrokerError::OutputReader)??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| BrokerError::OutputReader)??;
    Ok(process_result(
        exit_code,
        stdout,
        stderr,
        timed_out,
        output_limit_hit.load(Ordering::Acquire),
        started,
    ))
}

fn read_bounded(mut reader: impl Read, output_limit_hit: Arc<AtomicBool>) -> io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(MAX_OUTPUT_BYTES);
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if output.len() < MAX_OUTPUT_BYTES {
            let remaining = MAX_OUTPUT_BYTES - output.len();
            let retained = read.min(remaining);
            output.extend_from_slice(&buffer[..retained]);
            if retained < read {
                output_limit_hit.store(true, Ordering::Release);
                break;
            }
        } else {
            output_limit_hit.store(true, Ordering::Release);
            break;
        }
    }
    Ok(output)
}

fn process_result(
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    output_limit_hit: bool,
    started: Instant,
) -> ToolResult {
    let (stdout, stdout_truncated) = truncate_output(stdout);
    let (stderr, stderr_truncated) = truncate_output(stderr);
    ToolResult {
        status: if timed_out {
            ToolStatus::TimedOut
        } else if exit_code == Some(0) {
            ToolStatus::Succeeded
        } else {
            ToolStatus::Failed
        },
        exit_code,
        stdout,
        stderr,
        bytes_written: None,
        output_truncated: output_limit_hit || stdout_truncated || stderr_truncated,
        duration_ms: started.elapsed().as_millis() as u64,
        output_media_type: None,
        output_encoding: None,
    }
}

fn truncate_output(bytes: Vec<u8>) -> (String, bool) {
    let truncated = bytes.len() > MAX_OUTPUT_BYTES;
    let bytes = if truncated {
        &bytes[..MAX_OUTPUT_BYTES]
    } else {
        &bytes[..]
    };
    (String::from_utf8_lossy(bytes).into_owned(), truncated)
}

fn success_result(
    stdout: String,
    stderr: String,
    bytes_written: Option<u64>,
    started: Instant,
) -> ToolResult {
    ToolResult {
        status: ToolStatus::Succeeded,
        exit_code: Some(0),
        stdout,
        stderr,
        bytes_written,
        output_truncated: false,
        duration_ms: started.elapsed().as_millis() as u64,
        output_media_type: None,
        output_encoding: None,
    }
}

/// Convert immediate output into a durable, secret-safe audit record. Raw output
/// stays at the caller seam and can be placed in a separately protected artifact
/// store later; the event log receives only digests and bounded metadata.
pub fn audit_result(result: &ToolResult) -> ToolAuditResult {
    ToolAuditResult {
        status: result.status.clone(),
        exit_code: result.exit_code,
        stdout_digest: sha256(&result.stdout),
        stderr_digest: sha256(&result.stderr),
        stdout_bytes: result.stdout.len() as u64,
        stderr_bytes: result.stderr.len() as u64,
        bytes_written: result.bytes_written,
        output_truncated: result.output_truncated,
        duration_ms: result.duration_ms,
        stdout_artifact_id: None,
        stderr_artifact_id: None,
    }
}

fn sha256(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("sha256:{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_policy_engine::{ApprovalMode, PolicyConfig};
    use std::collections::BTreeSet;
    use std::fs;
    use tempfile::tempdir;

    fn policy(root: &std::path::Path) -> PolicyEngine {
        let mut config = PolicyConfig::trusted_workspace(vec![root.to_path_buf()]);
        config.approval_mode = ApprovalMode::AutoApproveTrustedWorkspace;
        PolicyEngine::new(config).unwrap()
    }

    fn context(root: &std::path::Path) -> ExecutionContext {
        ExecutionContext {
            session_id: "session".into(),
            principal: "test".into(),
            backend: "local-trusted-host".into(),
            workspace_roots: vec![root.display().to_string()],
        }
    }

    #[test]
    fn file_write_requires_an_exact_approval_in_default_mode() {
        let directory = tempdir().unwrap();
        let mut config = PolicyConfig::trusted_workspace(vec![directory.path().to_path_buf()]);
        let policy = PolicyEngine::new(config.clone()).unwrap();
        let broker = ExecutionBroker::new(policy);
        let intent = ToolIntent::WriteFile {
            path: "out.txt".into(),
            content: "safe".into(),
        };
        let prepared = broker
            .prepare(context(directory.path()), "op-1", intent)
            .unwrap();
        assert!(matches!(
            broker.authorize(prepared.clone(), None),
            Err(BrokerError::ApprovalRequired(_))
        ));
        let grant = ApprovalGrant {
            session_id: "session".into(),
            operation_id: "op-1".into(),
            request_digest: prepared.request_digest().into(),
            principal: "test".into(),
            actor: "test".into(),
            nonce: "nonce-1".into(),
            expires_at_ms: now_ms() + 60_000,
        };
        let authorized = broker.authorize(prepared, Some(grant)).unwrap();
        broker.execute(authorized).unwrap();
        assert_eq!(fs::read_to_string(directory.path().join("out.txt")).unwrap(), "safe");
        config.allow_trusted_host_process = false;
    }

    #[test]
    fn read_file_does_not_need_approval() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("in.txt"), "hello").unwrap();
        let broker = ExecutionBroker::new(policy(directory.path()));
        let prepared = broker
            .prepare(
                context(directory.path()),
                "op-1",
                ToolIntent::ReadFile {
                    path: "in.txt".into(),
                },
            )
            .unwrap();
        let result = broker.execute(broker.authorize(prepared, None).unwrap()).unwrap();
        assert_eq!(result.stdout, "hello");
    }

    #[test]
    fn list_files_and_read_image_share_the_read_only_policy() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("nested").join("in.txt"), "hello").unwrap();
        fs::write(directory.path().join("preview.png"), b"not-a-real-png").unwrap();
        let broker = ExecutionBroker::new(policy(directory.path()));

        let list = broker
            .prepare(
                context(directory.path()),
                "list-op",
                ToolIntent::ListFiles {
                    root: ".".into(),
                    depth: 2,
                    include_hidden: false,
                },
            )
            .unwrap();
        let listed = broker.execute(broker.authorize(list, None).unwrap()).unwrap();
        assert!(listed.stdout.contains("preview.png"));
        assert!(listed.stdout.contains("nested"));

        let image = broker
            .prepare(
                context(directory.path()),
                "image-op",
                ToolIntent::ReadImage {
                    path: "preview.png".into(),
                    max_bytes: None,
                },
            )
            .unwrap();
        let image = broker.execute(broker.authorize(image, None).unwrap()).unwrap();
        assert_eq!(image.output_media_type.as_deref(), Some("image/png"));
        assert_eq!(image.output_encoding.as_deref(), Some("base64"));
        assert!(!image.stdout.is_empty());
    }

    #[test]
    fn unavailable_container_backend_reports_no_isolation_and_fails_closed() {
        let backend = UnavailableContainerBackend;
        let capabilities = backend.capabilities();
        assert_eq!(capabilities.name, "container-unavailable");
        assert!(!capabilities.process_isolation);
        assert!(!capabilities.network_isolation);
    }

    #[test]
    fn process_is_not_enabled_by_command_name_alone() {
        let directory = tempdir().unwrap();
        let executable = directory.path().join("echo");
        fs::write(&executable, "not executed").unwrap();
        let mut config = PolicyConfig::trusted_workspace(vec![directory.path().to_path_buf()]);
        config.allowed_programs = BTreeSet::from([executable.clone()]);
        let policy = PolicyEngine::new(config).unwrap();
        assert!(matches!(
            ExecutionBroker::new(policy).prepare(
                context(directory.path()),
                "op",
                ToolIntent::Exec {
                    program: executable.display().to_string(),
                    args: vec!["hello".into()],
                    cwd: None,
                    timeout_ms: None,
                }
            ),
            Err(BrokerError::Policy(PolicyError::ProcessDisabled))
        ));
    }
}
