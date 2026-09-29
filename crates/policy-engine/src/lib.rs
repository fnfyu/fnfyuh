//! Pure workspace and capability policy decisions.
//!
//! The policy engine does not read files or start processes on behalf of a caller.
//! It binds a request to canonical workspace roots and produces an exact digest for
//! approval. Trusted-host and isolated-image executable namespaces are distinct; the
//! local adapter is not advertised as a hostile-agent sandbox.

use harness_protocol::{canonical_json, Capability, PolicyOutcome, ToolIntent};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApprovalMode {
    RequireApproval,
    AutoApproveTrustedWorkspace,
}

impl Default for ApprovalMode {
    fn default() -> Self {
        Self::RequireApproval
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyConfig {
    pub workspace_roots: Vec<PathBuf>,
    /// Canonical absolute executable paths for the trusted host adapter.
    pub allowed_programs: BTreeSet<PathBuf>,
    /// Absolute paths that are expected to exist inside the isolated image. They
    /// are never resolved against the host filesystem.
    #[serde(default)]
    pub allowed_container_programs: BTreeSet<String>,
    pub policy_version: String,
    pub approval_mode: ApprovalMode,
    pub allow_trusted_host_process: bool,
    #[serde(default)]
    pub allow_isolated_process: bool,
}

impl PolicyConfig {
    pub fn trusted_workspace(workspace_roots: Vec<PathBuf>) -> Self {
        Self {
            workspace_roots,
            allowed_programs: BTreeSet::new(),
            allowed_container_programs: BTreeSet::new(),
            policy_version: "policy.v1.trusted-workspace".to_string(),
            approval_mode: ApprovalMode::RequireApproval,
            allow_trusted_host_process: false,
            allow_isolated_process: false,
        }
    }

    pub fn isolated_workspace(workspace_roots: Vec<PathBuf>) -> Self {
        Self {
            workspace_roots,
            allowed_programs: BTreeSet::new(),
            allowed_container_programs: BTreeSet::from([
                "/usr/bin/git".to_string(),
                "/usr/bin/node".to_string(),
                "/usr/bin/python3".to_string(),
                "/bin/node".to_string(),
                "/bin/python3".to_string(),
            ]),
            policy_version: "policy.v1.isolated-container".to_string(),
            approval_mode: ApprovalMode::RequireApproval,
            allow_trusted_host_process: false,
            allow_isolated_process: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyDecision {
    pub outcome: PolicyOutcome,
    pub operation: String,
    pub capabilities: Vec<Capability>,
    pub policy_version: String,
    pub request_digest: String,
    pub resolved_path: Option<String>,
    pub resolved_cwd: Option<String>,
    pub reason: String,
    pub requires_approval: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("no workspace root is configured")]
    NoWorkspace,
    #[error("the first local adapter supports exactly one workspace root")]
    MultipleWorkspaces,
    #[error("workspace root does not exist: {0}")]
    MissingWorkspace(String),
    #[error("workspace root must not be a symlink: {0}")]
    SymlinkWorkspace(String),
    #[error("path contains a NUL byte")]
    NulPath,
    #[error("path must be a relative UTF-8 workspace path: {0}")]
    InvalidRelativePath(String),
    #[error("path escapes the workspace: {0}")]
    OutsideWorkspace(String),
    #[error("path uses a symlink or reparse point: {0}")]
    SymlinkPath(String),
    #[error("path has no existing parent: {0}")]
    MissingParent(String),
    #[error("the command is not allowed: {0}")]
    ProgramNotAllowed(String),
    #[error("host process execution is disabled because this backend is not isolated")]
    ProcessDisabled,
}

#[derive(Clone)]
pub struct PolicyEngine {
    config: PolicyConfig,
    roots: Vec<PathBuf>,
}

impl PolicyEngine {
    pub fn new(mut config: PolicyConfig) -> Result<Self, PolicyError> {
        if config.workspace_roots.is_empty() {
            return Err(PolicyError::NoWorkspace);
        }
        if config.workspace_roots.len() != 1 {
            return Err(PolicyError::MultipleWorkspaces);
        }
        let mut roots = Vec::with_capacity(config.workspace_roots.len());
        for root in &config.workspace_roots {
            let metadata = fs::symlink_metadata(root)
                .map_err(|_| PolicyError::MissingWorkspace(root.display().to_string()))?;
            if metadata.file_type().is_symlink() {
                return Err(PolicyError::SymlinkWorkspace(root.display().to_string()));
            }
            if !metadata.is_dir() {
                return Err(PolicyError::MissingWorkspace(root.display().to_string()));
            }
            let canonical = fs::canonicalize(root)
                .map_err(|_| PolicyError::MissingWorkspace(root.display().to_string()))?;
            roots.push(canonical);
        }
        let mut canonical_programs = BTreeSet::new();
        for program in &config.allowed_programs {
            if !program.is_absolute() {
                return Err(PolicyError::ProgramNotAllowed(program.display().to_string()));
            }
            let canonical = fs::canonicalize(program)
                .map_err(|_| PolicyError::ProgramNotAllowed(program.display().to_string()))?;
            if !canonical.is_file() {
                return Err(PolicyError::ProgramNotAllowed(program.display().to_string()));
            }
            canonical_programs.insert(canonical);
        }
        config.workspace_roots = roots.clone();
        config.allowed_programs = canonical_programs;
        Ok(Self { config, roots })
    }

    pub fn config(&self) -> &PolicyConfig {
        &self.config
    }

    pub fn policy_version(&self) -> &str {
        &self.config.policy_version
    }

    /// Produces an audit identity even when normalization rejects a request. The
    /// resulting PolicyEvaluated(denied) event is not an authorization token.
    pub fn denied_request_digest(&self, intent: &ToolIntent) -> String {
        request_digest(&self.config.policy_version, intent, None, None)
    }

    pub fn resolve_relative_path(&self, value: &str) -> Result<PathBuf, PolicyError> {
        validate_relative_path(value)?;
        let candidate = self.roots[0].join(value.replace('/', std::path::MAIN_SEPARATOR_STR));
        self.resolve_candidate(candidate)
    }

    pub fn resolve_cwd(&self, cwd: Option<&str>) -> Result<PathBuf, PolicyError> {
        match cwd {
            Some(value) => self.resolve_relative_path(value),
            None => Ok(self.roots[0].clone()),
        }
    }

    pub fn evaluate(&self, intent: &ToolIntent) -> Result<PolicyDecision, PolicyError> {
        let (capabilities, resolved_path, resolved_cwd, program) = match intent {
            ToolIntent::ReadFile { path } => (
                vec![Capability::WorkspaceRead],
                Some(self.resolve_relative_path(path)?),
                None,
                None,
            ),
            ToolIntent::Search { root, .. } | ToolIntent::ListFiles { root, .. } => (
                vec![Capability::WorkspaceRead],
                Some(self.resolve_relative_path(root)?),
                None,
                None,
            ),
            ToolIntent::ReadImage { path, .. } => (
                vec![Capability::WorkspaceRead],
                Some(self.resolve_relative_path(path)?),
                None,
                None,
            ),
            ToolIntent::WriteFile { path, .. } | ToolIntent::EditFile { path, .. } => (
                vec![Capability::WorkspaceWrite],
                Some(self.resolve_relative_path(path)?),
                None,
                None,
            ),
            ToolIntent::Exec {
                program, cwd, ..
            }
            | ToolIntent::Test {
                program, cwd, ..
            } => (
                vec![Capability::ProcessRun],
                None,
                Some(self.resolve_cwd(cwd.as_deref())?),
                Some(program.as_str()),
            ),
            ToolIntent::Git { cwd, .. } => (
                vec![Capability::GitWrite, Capability::ProcessRun],
                None,
                Some(self.resolve_cwd(cwd.as_deref())?),
                Some(if self.config.allow_isolated_process {
                    "/usr/bin/git"
                } else {
                    "git"
                }),
            ),
        };

        if let Some(program) = program {
            if self.config.allow_isolated_process {
                self.validate_container_program(program)?;
            } else {
                self.validate_program(program)?;
                if !self.config.allow_trusted_host_process {
                    return Err(PolicyError::ProcessDisabled);
                }
            }
        }

        let requires_approval = !intent.is_read_only()
            && matches!(&self.config.approval_mode, ApprovalMode::RequireApproval);
        let outcome = if requires_approval {
            PolicyOutcome::NeedsApproval
        } else {
            PolicyOutcome::Allowed
        };
        let request_digest = request_digest(
            &self.config.policy_version,
            intent,
            resolved_path.as_deref(),
            resolved_cwd.as_deref(),
        );
        Ok(PolicyDecision {
            outcome,
            operation: intent.operation_name().to_string(),
            capabilities,
            policy_version: self.config.policy_version.clone(),
            request_digest,
            resolved_path: resolved_path.map(|path| path.display().to_string()),
            resolved_cwd: resolved_cwd.map(|path| path.display().to_string()),
            reason: if requires_approval {
                "side effect requires explicit approval".to_string()
            } else if intent.is_read_only() {
                "read-only workspace operation allowed".to_string()
            } else {
                "trusted-workspace auto-approval is explicitly enabled".to_string()
            },
            requires_approval,
        })
    }

    fn validate_container_program(&self, program: &str) -> Result<(), PolicyError> {
        let path = Path::new(program);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .trim_end_matches(".exe")
            .to_ascii_lowercase();
        if !path.is_absolute()
            || program.as_bytes().contains(&0)
            || matches!(
                file_name.as_str(),
                "sh" | "bash" | "zsh" | "cmd" | "powershell" | "pwsh" | "sudo" | "su" | "runas"
            )
            || !self.config.allowed_container_programs.contains(program)
        {
            return Err(PolicyError::ProgramNotAllowed(program.to_string()));
        }
        Ok(())
    }

    fn validate_program(&self, program: &str) -> Result<(), PolicyError> {
        let path = Path::new(program);
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .trim_end_matches(".exe")
            .to_ascii_lowercase();
        if !path.is_absolute()
            || program.as_bytes().contains(&0)
            || matches!(extension.as_str(), "bat" | "cmd" | "ps1" | "sh")
            || matches!(
                file_name.as_str(),
                "sh" | "bash" | "zsh" | "cmd" | "powershell" | "pwsh" | "sudo" | "su" | "runas"
            )
        {
            return Err(PolicyError::ProgramNotAllowed(program.to_string()));
        }
        let canonical = fs::canonicalize(path)
            .map_err(|_| PolicyError::ProgramNotAllowed(program.to_string()))?;
        if !self.config.allowed_programs.contains(&canonical) {
            return Err(PolicyError::ProgramNotAllowed(program.to_string()));
        }
        Ok(())
    }

    fn resolve_candidate(&self, candidate: PathBuf) -> Result<PathBuf, PolicyError> {
        let root = self
            .roots
            .iter()
            .find(|root| candidate.starts_with(root))
            .ok_or_else(|| PolicyError::OutsideWorkspace(candidate.display().to_string()))?;
        reject_symlink_components(root, &candidate)?;

        let mut existing = candidate.as_path();
        while !existing.exists() {
            existing = existing
                .parent()
                .ok_or_else(|| PolicyError::MissingParent(candidate.display().to_string()))?;
            if !existing.starts_with(root) {
                return Err(PolicyError::OutsideWorkspace(candidate.display().to_string()));
            }
        }
        let canonical_existing = fs::canonicalize(existing)
            .map_err(|_| PolicyError::MissingParent(candidate.display().to_string()))?;
        if !canonical_existing.starts_with(root) {
            return Err(PolicyError::OutsideWorkspace(candidate.display().to_string()));
        }
        let suffix = candidate
            .strip_prefix(existing)
            .map_err(|_| PolicyError::OutsideWorkspace(candidate.display().to_string()))?;
        let resolved = if suffix.as_os_str().is_empty() {
            canonical_existing
        } else {
            canonical_existing.join(suffix)
        };
        if !resolved.starts_with(root) {
            return Err(PolicyError::OutsideWorkspace(candidate.display().to_string()));
        }
        Ok(resolved)
    }
}

fn validate_relative_path(value: &str) -> Result<(), PolicyError> {
    if value.as_bytes().contains(&0) {
        return Err(PolicyError::NulPath);
    }
    if value.is_empty()
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('\\')
        || value.contains(':')
    {
        return Err(PolicyError::InvalidRelativePath(value.to_string()));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PolicyError::InvalidRelativePath(value.to_string()));
    }
    Ok(())
}

fn reject_symlink_components(root: &Path, candidate: &Path) -> Result<(), PolicyError> {
    let relative = candidate
        .strip_prefix(root)
        .map_err(|_| PolicyError::OutsideWorkspace(candidate.display().to_string()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        current.push(part);
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                return Err(PolicyError::SymlinkPath(current.display().to_string()));
            }
        }
    }
    Ok(())
}

fn request_digest(
    policy_version: &str,
    intent: &ToolIntent,
    resolved_path: Option<&Path>,
    resolved_cwd: Option<&Path>,
) -> String {
    let value = (
        policy_version,
        intent,
        resolved_path.map(|path| path.display().to_string()),
        resolved_cwd.map(|path| path.display().to_string()),
    );
    let encoded = canonical_json(&value).expect("policy request is serializable");
    let digest = Sha256::digest(encoded.as_bytes());
    format!("sha256:{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::ToolIntent;
    use std::fs;
    use tempfile::tempdir;

    fn engine(root: &Path) -> PolicyEngine {
        PolicyEngine::new(PolicyConfig::trusted_workspace(vec![root.to_path_buf()])).unwrap()
    }

    #[test]
    fn read_is_allowed_but_write_requires_approval() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("file.txt"), "hello").unwrap();
        let policy = engine(directory.path());
        let read = policy
            .evaluate(&ToolIntent::ReadFile {
                path: "file.txt".into(),
            })
            .unwrap();
        assert_eq!(read.outcome, PolicyOutcome::Allowed);
        let write = policy
            .evaluate(&ToolIntent::WriteFile {
                path: "new.txt".into(),
                content: "new".into(),
            })
            .unwrap();
        assert_eq!(write.outcome, PolicyOutcome::NeedsApproval);
        assert_ne!(read.request_digest, write.request_digest);
    }

    #[test]
    fn traversal_and_backslash_paths_are_rejected() {
        let directory = tempdir().unwrap();
        let policy = engine(directory.path());
        for path in ["../outside", "a/../../outside", "a\\b", "C:foo", ""] {
            assert!(matches!(
                policy.evaluate(&ToolIntent::ReadFile { path: path.into() }),
                Err(PolicyError::InvalidRelativePath(_))
            ));
        }
    }

    #[test]
    fn symlink_is_rejected_when_platform_supports_it() {
        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        let link = directory.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(outside.path(), &link).unwrap();
        let policy = engine(directory.path());
        #[cfg(any(unix, windows))]
        assert!(matches!(
            policy.evaluate(&ToolIntent::ReadFile {
                path: "link/secret.txt".into()
            }),
            Err(PolicyError::SymlinkPath(_))
        ));
    }

    #[test]
    fn process_is_denied_without_isolation_and_allowlist() {
        let directory = tempdir().unwrap();
        let policy = engine(directory.path());
        assert_eq!(
            policy.evaluate(&ToolIntent::Exec {
                program: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                timeout_ms: None,
            }),
            Err(PolicyError::ProgramNotAllowed("echo".into()))
        );
    }
}
