//! External plugin manifest validation seam.
//!
//! No arbitrary native code is loaded in the daemon. A later process/WASI adapter may
//! implement invocation, but every plugin must first pass this protocol/capability
//! validation and communicate through versioned JSON-RPC.

use harness_protocol::{Capability, PluginKind, PluginManifest, PROTOCOL_VERSION};
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PluginError {
    #[error("plugin `{0}` requests an unsupported protocol version")]
    UnsupportedProtocol(String),
    #[error("plugin name or version is empty")]
    InvalidIdentity,
    #[error("plugin `{plugin}` requests forbidden capability `{capability:?}`")]
    CapabilityDenied { plugin: String, capability: Capability },
    #[error("plugin kind is not enabled by this host")]
    KindDisabled,
}

#[derive(Debug, Clone)]
pub struct PluginHostConfig {
    pub enabled_kinds: BTreeSet<PluginKind>,
    pub allowed_capabilities: BTreeSet<Capability>,
}

impl Default for PluginHostConfig {
    fn default() -> Self {
        Self {
            enabled_kinds: BTreeSet::from([PluginKind::Provider, PluginKind::Tool]),
            allowed_capabilities: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PluginHost {
    config: PluginHostConfig,
}

impl PluginHost {
    pub fn new(config: PluginHostConfig) -> Self {
        Self { config }
    }

    pub fn validate_manifest(&self, manifest: &PluginManifest) -> Result<(), PluginError> {
        if manifest.name.trim().is_empty() || manifest.version.trim().is_empty() {
            return Err(PluginError::InvalidIdentity);
        }
        if manifest.protocol_version != PROTOCOL_VERSION {
            return Err(PluginError::UnsupportedProtocol(manifest.name.clone()));
        }
        if !self.config.enabled_kinds.contains(&manifest.kind) {
            return Err(PluginError::KindDisabled);
        }
        for capability in &manifest.requested_capabilities {
            if !self.config.allowed_capabilities.contains(capability) {
                return Err(PluginError::CapabilityDenied {
                    plugin: manifest.name.clone(),
                    capability: capability.clone(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_are_deny_by_default() {
        let host = PluginHost::new(PluginHostConfig::default());
        let manifest = PluginManifest {
            name: "example".into(),
            version: "1.0.0".into(),
            protocol_version: PROTOCOL_VERSION,
            kind: PluginKind::Tool,
            requested_capabilities: vec![Capability::WorkspaceWrite],
        };
        assert!(matches!(
            host.validate_manifest(&manifest),
            Err(PluginError::CapabilityDenied { .. })
        ));
    }
}
