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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

// Written out rather than derived: `#[derive(Default)]` would give
// `apply_default_mc_options_to_new_instances = false`, disagreeing with the
// `#[serde(default = "default_true")]` above it. `load()` falls back to
// `Default::default()` for a missing *or* unreadable config, so the derived
// version meant a fresh install (and any corrupt-config reset) silently
// started with global game options NOT applied to new instances, while every
// config file that merely omitted the key got `true`. Same defaults on both
// paths now; keep them in sync if a field with a non-`Default` serde default
// is ever added.
impl Default for AppConfigFile {
    fn default() -> Self {
        Self {
            curseforge_api_key: None,
            default_mc_options: None,
            apply_default_mc_options_to_new_instances: default_true(),
            account: None,
            account_protected: None,
            java_path: None,
            max_memory_mb: None,
            jvm_args: None,
            curseforge_login_prompted: false,
        }
    }
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
            match toml::from_str(&raw) {
                Ok(parsed) => parsed,
                Err(err) => {
                    // A truncated/corrupted config.toml (crash mid-write, disk
                    // full, antivirus lock, ...) used to `?` straight out of
                    // here into an `.expect()` in lib.rs, panicking before any
                    // window ever opened — a GUI app's console output goes
                    // nowhere, so the user just saw it silently fail to
                    // launch, with no way back short of manually finding and
                    // deleting this file. Back it up (in case anything in it
                    // is worth recovering by hand) and start fresh instead.
                    let mut backup = path.as_os_str().to_os_string();
                    backup.push(".bak");
                    let _ = fs::rename(&path, PathBuf::from(&backup));
                    crate::activity::append_log(
                        &format!(
                            "config.toml was invalid ({err}) — backed up to {} and reset to defaults",
                            PathBuf::from(&backup).display()
                        ),
                        "warn",
                        None,
                    );
                    AppConfigFile::default()
                }
            }
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

#[cfg(test)]
mod config_store_tests {
    use super::*;

    // `ConfigStore::load()` resolves its own path from `dirs::config_dir()`,
    // which cannot be redirected, so these tests never call it — running it
    // would read (and `encrypt_legacy_secrets` would rewrite) the developer's
    // real `dev.waybound/config.toml`. Everything reachable without that
    // hardcoded path is exercised against a store pointed at a temp file.

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("waybound-test-config-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_at(path: &std::path::Path) -> ConfigStore {
        ConfigStore {
            path: path.to_path_buf(),
            inner: RwLock::new(AppConfigFile::default()),
        }
    }

    /// Re-read a persisted file the same way `load()` would, minus the fixed path.
    fn reload(path: &std::path::Path) -> ConfigStore {
        let raw = fs::read_to_string(path).unwrap();
        ConfigStore {
            path: path.to_path_buf(),
            inner: RwLock::new(toml::from_str(&raw).unwrap()),
        }
    }

    #[test]
    fn the_curseforge_key_survives_a_save_and_reload() {
        let dir = temp_dir("cf_key_roundtrip");
        let path = dir.join("config.toml");

        store_at(&path).set_curseforge_api_key("test-key-abc123".to_string()).unwrap();
        assert_eq!(
            reload(&path).stored_curseforge_api_key().as_deref(),
            Some("test-key-abc123")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_curseforge_key_is_never_written_to_disk_in_the_clear_on_windows() {
        let dir = temp_dir("cf_key_protected");
        let path = dir.join("config.toml");

        store_at(&path).set_curseforge_api_key("super-secret-key".to_string()).unwrap();
        let raw = fs::read_to_string(&path).unwrap();

        if cfg!(windows) {
            // DPAPI is available: the file holds `dpapi:<base64>`, not the key.
            assert!(
                !raw.contains("super-secret-key"),
                "the API key was persisted in plaintext:\n{raw}"
            );
            assert!(raw.contains("dpapi:"), "expected a DPAPI blob in:\n{raw}");
        } else {
            // Documented fallback: no DPAPI off Windows, so plaintext as before.
            assert!(raw.contains("super-secret-key"));
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_legacy_plaintext_key_on_disk_is_still_readable() {
        let dir = temp_dir("cf_key_legacy");
        let path = dir.join("config.toml");
        // Pre-DPAPI configs stored the raw key; `reveal` passes it through.
        fs::write(&path, "curseforgeApiKey = \"  legacy-key  \"\n").unwrap();

        // Also covers the trim done by `normalize_curseforge_api_key`.
        assert_eq!(reload(&path).stored_curseforge_api_key().as_deref(), Some("legacy-key"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_curseforge_key_is_rejected_and_nothing_is_written() {
        let dir = temp_dir("cf_key_empty");
        let path = dir.join("config.toml");

        let store = store_at(&path);
        assert!(matches!(
            store.set_curseforge_api_key("   ".to_string()),
            Err(ConfigError::Parse(_))
        ));
        assert!(!path.exists(), "a rejected key must not create a config file");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_the_key_removes_it_from_the_persisted_file() {
        let dir = temp_dir("cf_key_clear");
        let path = dir.join("config.toml");

        let store = store_at(&path);
        store.set_curseforge_api_key("test-key-abc123".to_string()).unwrap();
        store.clear_curseforge_api_key().unwrap();

        assert!(!fs::read_to_string(&path).unwrap().contains("curseforgeApiKey"));
        assert!(reload(&path).stored_curseforge_api_key().is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn launch_settings_round_trip_and_blank_strings_become_none() {
        let dir = temp_dir("launch_settings");
        let path = dir.join("config.toml");

        let store = store_at(&path);
        store
            .set_launch_settings(
                Some("C:/java/bin/java.exe".to_string()),
                Some(4096),
                Some("   ".to_string()),
            )
            .unwrap();

        let reloaded = reload(&path);
        assert_eq!(reloaded.java_path().as_deref(), Some("C:/java/bin/java.exe"));
        assert_eq!(reloaded.max_memory_mb(), 4096);
        assert!(reloaded.jvm_args().is_none(), "whitespace-only jvm args should not persist");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_is_clamped_into_a_launchable_range_on_write_and_read() {
        let dir = temp_dir("memory_clamp");
        let path = dir.join("config.toml");

        let store = store_at(&path);
        store.set_launch_settings(None, Some(1), None).unwrap();
        assert_eq!(store.max_memory_mb(), 512);
        store.set_launch_settings(None, Some(999_999), None).unwrap();
        assert_eq!(store.max_memory_mb(), 32768);
        assert_eq!(reload(&path).max_memory_mb(), 32768);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unset_memory_limit_falls_back_to_the_default_heap() {
        let dir = temp_dir("memory_default");
        let path = dir.join("config.toml");
        assert_eq!(store_at(&path).max_memory_mb(), 2048);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_login_prompt_flag_persists_once_marked() {
        let dir = temp_dir("login_prompt");
        let path = dir.join("config.toml");

        let store = store_at(&path);
        assert!(!store.curseforge_login_prompted());
        store.mark_curseforge_login_prompted().unwrap();
        assert!(store.curseforge_login_prompted());
        assert!(reload(&path).curseforge_login_prompted());
        // Idempotent: marking again keeps it set.
        store.mark_curseforge_login_prompted().unwrap();
        assert!(reload(&path).curseforge_login_prompted());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_creates_the_config_directory_when_it_does_not_exist_yet() {
        let dir = temp_dir("nested_persist");
        let path = dir.join("not").join("created").join("yet").join("config.toml");

        store_at(&path).mark_curseforge_login_prompted().unwrap();
        assert!(path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_config_file_fails_to_parse_rather_than_yielding_junk() {
        // The precondition for `load()`'s recovery path: this is the exact
        // shape of a config truncated mid-write, and it must be an Err so the
        // backup-and-reset branch runs instead of a partial config loading.
        for corrupt in [
            "curseforgeApiKey = \"unterminated",
            "= no key\n",
            "\u{0}\u{0}\u{0}\u{0}",
            "maxMemoryMb = \"not a number\"",
        ] {
            assert!(
                toml::from_str::<AppConfigFile>(corrupt).is_err(),
                "{corrupt:?} should not parse as a config"
            );
        }
        // An empty file is valid, not corrupt — every field is optional.
        assert!(toml::from_str::<AppConfigFile>("").is_ok());
    }

    #[test]
    fn a_missing_config_and_an_empty_config_agree_on_the_mc_options_default() {
        // `#[serde(default = ...)]` only applies when deserializing, while
        // `load()` uses `AppConfigFile::default()` for a missing or corrupt
        // file. With `Default` derived those two disagreed, so "apply my
        // default options to new instances" was on after any config file was
        // read but off on first run and after a corrupt-config reset. The
        // hand-written `impl Default` keeps both paths identical.
        let from_empty_file: AppConfigFile = toml::from_str("").unwrap();
        assert!(from_empty_file.apply_default_mc_options_to_new_instances);
        assert!(AppConfigFile::default().apply_default_mc_options_to_new_instances);
    }

    #[test]
    fn every_default_field_matches_between_derive_path_and_deserialize_path() {
        // Guards the whole struct, not just the one field that regressed:
        // any future field whose serde default isn't its `Default` value
        // would reintroduce the same split-brain between a fresh install
        // and one that has ever written a config.
        let from_empty_file: AppConfigFile = toml::from_str("").unwrap();
        let from_default = AppConfigFile::default();

        assert_eq!(
            toml::to_string(&from_empty_file).unwrap(),
            toml::to_string(&from_default).unwrap(),
        );
    }

    #[test]
    fn global_mc_options_round_trip_through_the_file() {
        let dir = temp_dir("mc_options");
        let path = dir.join("config.toml");

        let store = store_at(&path);
        assert!(store.global_mc_options().is_none());
        store.set_global_mc_options(McOptions::default(), false).unwrap();

        let reloaded = reload(&path);
        assert!(reloaded.global_mc_options().is_some());
        assert!(!reloaded.apply_global_mc_options_to_new_instances());

        let _ = fs::remove_dir_all(&dir);
    }
}
