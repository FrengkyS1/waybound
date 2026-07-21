mod docker_env;
mod protected;

pub use docker_env::{
    default_docker_env_path, default_docker_env_path_string, read_docker_env_key_from_path,
};

use crate::auth::Account;
use crate::settings::McOptions;
use crate::sources::curseforge_key::{
    curseforge_api_key_from_environment, normalize_curseforge_api_key,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;
use thiserror::Error;

const CONFIG_DIR_NAME: &str = "dev.waybound";
const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CurseForgeKeySource {
    Config,
    Environment,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not resolve config directory")]
    NoConfigDir,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid config: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppConfigFile {
    #[serde(default)]
    curseforge_api_key: Option<String>,
    #[serde(default)]
    default_mc_options: Option<McOptions>,
    #[serde(default = "default_true")]
    apply_default_mc_options_to_new_instances: bool,
    /// Legacy plaintext account (pre-DPAPI configs only; migrated on load).
    #[serde(default)]
    account: Option<Account>,
    /// DPAPI-protected JSON of the signed-in `Account` (see `protected.rs`).
    #[serde(default)]
    account_protected: Option<String>,
    /// Explicit Java executable to launch with (overrides auto-detection).
    #[serde(default)]
    java_path: Option<String>,
    /// Max heap in MB passed as `-Xmx`.
    #[serde(default)]
    max_memory_mb: Option<u32>,
    /// Extra JVM arguments applied to every instance (unless the instance
    /// overrides them).
    #[serde(default)]
    jvm_args: Option<String>,
    /// Whether the one-time "log into CurseForge" prompt has already been
    /// shown in the missing-mods sandboxed browser. CurseForge doesn't
    /// require a session to download (an incognito tab works), but a logged
    /// in session avoids that domain's own login/SSO interstitials from
    /// popping up mid-flow the first time a restricted mod is hit.
    #[serde(default)]
    curseforge_login_prompted: bool,
}

fn default_true() -> bool {
    true
}

pub struct ConfigStore {
    path: PathBuf,
    inner: RwLock<AppConfigFile>,
}

impl ConfigStore {
    pub fn load() -> Result<Self, ConfigError> {
        let path = config_path()?;
        let inner = if path.exists() {
            let raw = fs::read_to_string(&path)?;
            toml::from_str(&raw).map_err(|err| ConfigError::Parse(err.to_string()))?
        } else {
            AppConfigFile::default()
        };

        let store = Self {
            path,
            inner: RwLock::new(inner),
        };
        store.encrypt_legacy_secrets();
        Ok(store)
    }

    /// One-time migration: configs written before DPAPI support hold plaintext
    /// secrets; re-persist them encrypted. No-op when nothing is plaintext or
    /// DPAPI is unavailable.
    fn encrypt_legacy_secrets(&self) {
        let mut changed = false;
        if let Ok(mut config) = self.inner.write() {
            if let Some(key) = config.curseforge_api_key.as_deref() {
                if !protected::is_protected(key) {
                    if let Some(blob) = protected::protect(key) {
                        config.curseforge_api_key = Some(blob);
                        changed = true;
                    }
                }
            }
            if let Some(account) = config.account.take() {
                let blob = serde_json::to_string(&account)
                    .ok()
                    .and_then(|json| protected::protect(&json));
                match blob {
                    Some(blob) => {
                        config.account_protected = Some(blob);
                        changed = true;
                    }
                    // DPAPI unavailable: keep the pre-existing plaintext behavior.
                    None => config.account = Some(account),
                }
            }
        }
        if changed {
            let _ = self.persist();
        }
    }

    pub fn curseforge_configured(&self) -> bool {
        self.resolve_curseforge_api_key().is_some()
    }

    pub fn curseforge_key_source(&self) -> Option<CurseForgeKeySource> {
        self.resolve_curseforge_api_key()
            .map(|(_, source)| source)
    }

    pub fn environment_curseforge_available(&self) -> bool {
        curseforge_api_key_from_environment().is_some()
    }

    /// Saved config key only (not environment fallback).
    pub fn stored_curseforge_api_key(&self) -> Option<String> {
        let config = self.inner.read().ok()?;
        let stored = config.curseforge_api_key.as_deref()?;
        let plain = protected::reveal(stored)?;
        Some(normalize_curseforge_api_key(&plain)).filter(|key| !key.is_empty())
    }

    pub fn curseforge_api_key(&self) -> Option<String> {
        self.resolve_curseforge_api_key()
            .map(|(key, _)| key)
    }

    pub fn resolve_curseforge_api_key(&self) -> Option<(String, CurseForgeKeySource)> {
        if let Some(key) = self.stored_curseforge_api_key() {
            return Some((key, CurseForgeKeySource::Config));
        }

        curseforge_api_key_from_environment().map(|(key, _)| (key, CurseForgeKeySource::Environment))
    }

    pub fn set_curseforge_api_key(&self, api_key: String) -> Result<(), ConfigError> {
        if api_key.trim().is_empty() {
            return Err(ConfigError::Parse(
                "CurseForge API key cannot be empty.".to_string(),
            ));
        }

        {
            let mut config = self
                .inner
                .write()
                .map_err(|_| ConfigError::Parse("config lock poisoned".to_string()))?;
            config.curseforge_api_key = Some(protected::protect_or_plain(&api_key));
        }

        self.persist()
    }

    pub fn clear_curseforge_api_key(&self) -> Result<(), ConfigError> {
        {
            let mut config = self
                .inner
                .write()
                .map_err(|_| ConfigError::Parse("config lock poisoned".to_string()))?;
            config.curseforge_api_key = None;
        }

        self.persist()
    }

    pub fn global_mc_options(&self) -> Option<McOptions> {
        let config = self.inner.read().ok()?;
        config.default_mc_options.clone()
    }

    pub fn apply_global_mc_options_to_new_instances(&self) -> bool {
        self.inner
            .read()
            .map(|config| config.apply_default_mc_options_to_new_instances)
            .unwrap_or(true)
    }

    pub fn set_global_mc_options(
        &self,
        options: McOptions,
        apply_to_new_instances: bool,
    ) -> Result<(), ConfigError> {
        {
            let mut config = self
                .inner
                .write()
                .map_err(|_| ConfigError::Parse("config lock poisoned".to_string()))?;
            config.default_mc_options = Some(options);
            config.apply_default_mc_options_to_new_instances = apply_to_new_instances;
        }
        self.persist()
    }

    // ---- Launch / account settings -------------------------------------

    pub fn account(&self) -> Option<Account> {
        let config = self.inner.read().ok()?;
        if let Some(blob) = config.account_protected.as_deref() {
            if let Some(account) = protected::reveal(blob)
                .and_then(|json| serde_json::from_str(&json).ok())
            {
                return Some(account);
            }
        }
        config.account.clone()
    }

    pub fn set_account(&self, account: Option<Account>) -> Result<(), ConfigError> {
        {
            let mut config = self
                .inner
                .write()
                .map_err(|_| ConfigError::Parse("config lock poisoned".to_string()))?;
            config.account = None;
            config.account_protected = None;
            if let Some(account) = account {
                let blob = serde_json::to_string(&account)
                    .ok()
                    .and_then(|json| protected::protect(&json));
                match blob {
                    Some(blob) => config.account_protected = Some(blob),
                    // DPAPI unavailable (non-Windows): plaintext as before.
                    None => config.account = Some(account),
                }
            }
        }
        self.persist()
    }

    pub fn java_path(&self) -> Option<String> {
        self.inner
            .read()
            .ok()?
            .java_path
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn max_memory_mb(&self) -> u32 {
        self.inner
            .read()
            .map(|c| c.max_memory_mb.unwrap_or(2048))
            .unwrap_or(2048)
            .clamp(512, 32768)
    }

    pub fn jvm_args(&self) -> Option<String> {
        self.inner
            .read()
            .ok()?
            .jvm_args
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn set_launch_settings(
        &self,
        java_path: Option<String>,
        max_memory_mb: Option<u32>,
        jvm_args: Option<String>,
    ) -> Result<(), ConfigError> {
        {
            let mut config = self
                .inner
                .write()
                .map_err(|_| ConfigError::Parse("config lock poisoned".to_string()))?;
            config.java_path = java_path.filter(|s| !s.trim().is_empty());
            config.jvm_args = jvm_args.filter(|s| !s.trim().is_empty());
            if let Some(mem) = max_memory_mb {
                config.max_memory_mb = Some(mem.clamp(512, 32768));
            }
        }
        self.persist()
    }

    pub fn curseforge_login_prompted(&self) -> bool {
        self.inner
            .read()
            .map(|c| c.curseforge_login_prompted)
            .unwrap_or(false)
    }

    /// Marks the one-time CurseForge login prompt as shown. Idempotent by
    /// design — called every time the missing-mods browser opens, but only
    /// actually persists the first time (`curseforge_login_prompted` was
    /// already false only once).
    pub fn mark_curseforge_login_prompted(&self) -> Result<(), ConfigError> {
        {
            let mut config = self
                .inner
                .write()
                .map_err(|_| ConfigError::Parse("config lock poisoned".to_string()))?;
            config.curseforge_login_prompted = true;
        }
        self.persist()
    }

    fn persist(&self) -> Result<(), ConfigError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let config = self
            .inner
            .read()
            .map_err(|_| ConfigError::Parse("config lock poisoned".to_string()))?;

        let raw = toml::to_string_pretty(&*config)
            .map_err(|err| ConfigError::Parse(err.to_string()))?;
        fs::write(&self.path, raw)?;
        Ok(())
    }
}

fn config_path() -> Result<PathBuf, ConfigError> {
    let base = dirs::config_dir().ok_or(ConfigError::NoConfigDir)?;
    Ok(base.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
}
