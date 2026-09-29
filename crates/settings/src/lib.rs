//! Local mutable provider/model settings.
//!
//! The settings store owns configuration metadata only. Credentials are represented
//! by environment-variable names and resolved by the provider adapter at call time.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAiCompatible,
    Anthropic,
    #[serde(rename = "openai_codex", alias = "open_ai_codex", alias = "codex_cli")]
    OpenAiCodex,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelProfile {
    pub id: String,
    pub label: String,
    pub provider_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub supports_vision: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub models: Vec<ModelProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessSettings {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default = "default_providers")]
    pub providers: Vec<ProviderProfile>,
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("settings file read failed: {0}")]
    Read(#[source] io::Error),
    #[error("settings file write failed: {0}")]
    Write(#[source] io::Error),
    #[error("settings JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("settings validation failed: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<HarnessSettings, SettingsError> {
        match fs::read_to_string(&self.path) {
            Ok(content) => {
                let mut raw: Value = serde_json::from_str(&content)?;
                let migrated = migrate_legacy_echo(&mut raw);
                let settings: HarnessSettings = serde_json::from_value(raw)?;
                settings.validate()?;
                if migrated {
                    self.save(settings.clone())?;
                }
                Ok(settings)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(HarnessSettings::default()),
            Err(error) => Err(SettingsError::Read(error)),
        }
    }

    pub fn save(&self, mut settings: HarnessSettings) -> Result<HarnessSettings, SettingsError> {
        settings.version = SETTINGS_VERSION;
        settings.validate()?;
        let content = serde_json::to_string_pretty(&settings)? + "\n";
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(SettingsError::Write)?;
        }
        let temp_path = self.path.with_extension(format!("json.tmp.{}", std::process::id()));
        fs::write(&temp_path, content).map_err(SettingsError::Write)?;
        atomic_replace(&temp_path, &self.path).map_err(SettingsError::Write)?;
        Ok(settings)
    }
}

#[cfg(not(windows))]
fn atomic_replace(temp: &Path, target: &Path) -> io::Result<()> {
    fs::rename(temp, target)
}

#[cfg(windows)]
fn atomic_replace(temp: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    let target_exists = target.exists();
    let temp = wide(temp);
    let target = wide(target);
    unsafe {
        if target_exists {
            if ReplaceFileW(target.as_ptr(), temp.as_ptr(), ptr::null(), 0, ptr::null_mut(), ptr::null_mut()) == 0 {
                return Err(io::Error::last_os_error());
            }
        } else if MoveFileExW(temp.as_ptr(), target.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn ReplaceFileW(
        replaced_file_name: *const u16,
        replacement_file_name: *const u16,
        backup_file_name: *const u16,
        replace_flags: u32,
        exclude: *mut std::ffi::c_void,
        reserved: *mut std::ffi::c_void,
    ) -> i32;
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

fn migrate_legacy_echo(raw: &mut Value) -> bool {
    let default_model = raw
        .get("default_model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Some(providers) = raw.get_mut("providers").and_then(Value::as_array_mut) else {
        return false;
    };
    let is_echo = |provider: &Value| {
        provider.get("id").and_then(Value::as_str) == Some("echo")
            || provider.get("kind").and_then(Value::as_str) == Some("echo")
    };
    if !providers.iter().any(is_echo) {
        return false;
    }
    const LEGACY_DEFAULT_IDS: [&str; 6] = [
        "echo",
        "openai",
        "deepseek",
        "ollama",
        "anthropic",
        "openai-codex",
    ];
    providers.retain(|provider| {
        let id = provider.get("id").and_then(Value::as_str);
        !is_echo(provider) && !id.is_some_and(|value| LEGACY_DEFAULT_IDS.contains(&value))
    });
    let default_still_exists = default_model.as_deref().is_some_and(|model_id| {
        providers.iter().any(|provider| {
            provider
                .get("models")
                .and_then(Value::as_array)
                .is_some_and(|models| models.iter().any(|model| model.get("id").and_then(Value::as_str) == Some(model_id)))
        })
    });
    if !default_still_exists {
        raw["default_model"] = Value::String(String::new());
    }
    raw["version"] = Value::from(SETTINGS_VERSION);
    true
}

impl HarnessSettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.version != SETTINGS_VERSION {
            return Err(SettingsError::Invalid(format!(
                "unsupported settings version {}",
                self.version
            )));
        }
        let mut provider_ids = BTreeSet::new();
        let mut model_ids = BTreeSet::new();
        for provider in &self.providers {
            validate_id("provider", &provider.id)?;
            if !provider_ids.insert(provider.id.clone()) {
                return Err(SettingsError::Invalid(format!(
                    "duplicate provider id `{}`",
                    provider.id
                )));
            }
            if matches!(
                provider.kind,
                ProviderKind::OpenAiCompatible | ProviderKind::Anthropic
            ) && provider.endpoint.as_deref().unwrap_or_default().trim().is_empty()
            {
                return Err(SettingsError::Invalid(format!(
                    "provider `{}` requires an endpoint",
                    provider.id
                )));
            }
            for model in &provider.models {
                validate_id("model", &model.id)?;
                if model.provider_id != provider.id {
                    return Err(SettingsError::Invalid(format!(
                        "model `{}` belongs to `{}`, not `{}`",
                        model.id, model.provider_id, provider.id
                    )));
                }
                if !model_ids.insert(model.id.clone()) {
                    return Err(SettingsError::Invalid(format!(
                        "duplicate model id `{}`",
                        model.id
                    )));
                }
            }
        }
        if !self.default_model.trim().is_empty() {
            let (provider, selected) = self
                .model(&self.default_model)
                .ok_or_else(|| SettingsError::Invalid("default_model is not configured".into()))?;
            if !provider.enabled || !selected.enabled {
                return Err(SettingsError::Invalid("default_model is disabled".into()));
            }
        }
        Ok(())
    }

    pub fn model(&self, model_id: &str) -> Option<(&ProviderProfile, &ModelProfile)> {
        self.providers.iter().find_map(|provider| {
            provider
                .models
                .iter()
                .find(|model| model.id == model_id)
                .map(|model| (provider, model))
        })
    }

    pub fn public_view(&self) -> PublicSettings {
        PublicSettings {
            version: self.version,
            default_model: self.default_model.clone(),
            providers: self
                .providers
                .iter()
                .map(|provider| PublicProviderProfile {
                    id: provider.id.clone(),
                    name: provider.name.clone(),
                    kind: provider.kind.clone(),
                    endpoint: provider.endpoint.clone(),
                    api_key_env: provider.api_key_env.clone(),
                    api_key_configured: provider
                        .api_key_env
                        .as_deref()
                        .map(|name| std::env::var(name).is_ok())
                        .unwrap_or(matches!(provider.kind, ProviderKind::OpenAiCodex)),
                    enabled: provider.enabled,
                    models: provider.models.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicSettings {
    pub version: u32,
    pub default_model: String,
    pub providers: Vec<PublicProviderProfile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicProviderProfile {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub endpoint: Option<String>,
    pub api_key_env: Option<String>,
    pub api_key_configured: bool,
    pub enabled: bool,
    pub models: Vec<ModelProfile>,
}

impl Default for HarnessSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            default_model: default_model(),
            providers: default_providers(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_version() -> u32 {
    SETTINGS_VERSION
}

fn default_model() -> String {
    String::new()
}

fn default_providers() -> Vec<ProviderProfile> {
    Vec::new()
}

fn validate_id(kind: &str, value: &str) -> Result<(), SettingsError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':'))
    {
        return Err(SettingsError::Invalid(format!(
            "{kind} id `{value}` must use ASCII letters, digits, `-`, `_`, `.`, or `:`"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_secret_free() {
        let settings = HarnessSettings::default();
        settings.validate().unwrap();
        assert!(settings.default_model.is_empty());
        assert!(settings.providers.is_empty());
        let view = settings.public_view();
        let encoded = serde_json::to_string(&view).unwrap();
        assert!(!encoded.contains("secret-value"));
        assert!(encoded.contains("default_model"));
    }

    #[test]
    fn legacy_echo_settings_are_migrated_to_empty_defaults() {
        let path = std::env::temp_dir().join(format!("fnfyuh-legacy-settings-{}.json", std::process::id()));
        fs::write(
            &path,
            r#"{
                "version": 1,
                "default_model": "echo",
                "providers": [{
                    "id": "echo",
                    "name": "Echo",
                    "kind": "echo",
                    "endpoint": null,
                    "api_key_env": null,
                    "enabled": true,
                    "models": [{
                        "id": "echo",
                        "label": "Echo",
                        "provider_id": "echo",
                        "enabled": true,
                        "supports_vision": false
                    }]
                }]
            }"#,
        )
        .unwrap();
        let loaded = SettingsStore::new(&path).load().unwrap();
        assert!(loaded.default_model.is_empty());
        assert!(loaded.providers.is_empty());
        let persisted = fs::read_to_string(&path).unwrap();
        assert!(!persisted.contains("\"kind\": \"echo\""));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn save_and_load_validate_model_ownership() {
        let path = std::env::temp_dir().join(format!("fnfyuh-settings-{}.json", std::process::id()));
        let store = SettingsStore::new(&path);
        let mut settings = HarnessSettings::default();
        settings.providers.push(ProviderProfile {
            id: "openai-codex".into(),
            name: "OpenAI Codex".into(),
            kind: ProviderKind::OpenAiCodex,
            endpoint: None,
            api_key_env: None,
            enabled: true,
            models: Vec::new(),
        });
        settings.providers[0].models.push(ModelProfile {
            id: "gpt-5.4-test".into(),
            label: "GPT-5.4 Test".into(),
            provider_id: "openai-codex".into(),
            enabled: true,
            supports_vision: false,
        });
        settings.default_model = "gpt-5.4-test".into();
        store.save(settings.clone()).unwrap();
        assert_eq!(store.load().unwrap(), settings);
        let _ = fs::remove_file(path);
    }
}
