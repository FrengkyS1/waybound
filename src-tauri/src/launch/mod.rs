//! Minecraft launch pipeline: resolve the version (vanilla or Fabric), download
//! and verify every needed file, then assemble the exact Java command line.
//!
//! Actually spawning the process and streaming its output is left to the
//! command layer so this module stays free of Tauri types and easy to test.

pub mod fabric;
pub mod files;
pub mod forge;
pub mod java;
pub mod java_runtime;
pub mod manifest;

use std::collections::HashMap;
use std::path::PathBuf;

use reqwest::Client;
use serde::Serialize;
use thiserror::Error;

use crate::auth::Account;
use crate::dto::ModLoader;

use files::GamePaths;
use manifest::{
    rules_allow, ArgValue, Argument, VersionJson, VersionManifest, VERSION_MANIFEST_URL,
};

const LAUNCHER_NAME: &str = "Waybound";
const LAUNCHER_VERSION: &str = "0.1.0";

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to download {url} (status {status})")]
    Download { url: String, status: u16 },
    #[error("hash mismatch for {url}")]
    HashMismatch { url: String },
    #[error("parse error: {0}")]
    Parse(String),
    #[error("could not extract native library: {0}")]
    Extract(String),
    #[error("Minecraft {version} needs Java {required}, but no matching Java runtime was found. Install a JDK (Adoptium Temurin {required}) or set a Java path in Settings.")]
    NoJava { version: String, required: u32 },
    #[error("Minecraft version '{0}' was not found in the Mojang manifest")]
    VersionNotFound(String),
    #[error("launching {0} instances is not supported yet — use Vanilla or Fabric")]
    UnsupportedLoader(String),
    #[error("failed to start Java: {0}")]
    Spawn(String),
}

/// A progress tick emitted while files are being prepared.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressUpdate {
    pub stage: String,
    pub current: u64,
    pub total: u64,
}

impl ProgressUpdate {
    pub fn stage(stage: &str, current: u64, total: u64) -> Self {
        Self {
            stage: stage.to_string(),
            current,
            total,
        }
    }
}

/// Everything needed to spawn the game, produced by [`prepare_launch`].
#[derive(Debug, Clone)]
pub struct PreparedLaunch {
    pub java_path: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    /// Java major version chosen, for diagnostics.
    pub java_major: u32,
}

/// Resolve, download, and assemble the launch command for an instance.
pub async fn prepare_launch<F>(
    client: &Client,
    game_root: PathBuf,
    instance_dir: PathBuf,
    game_version: &str,
    loader: ModLoader,
    loader_version: Option<String>,
    account: &Account,
    java_override: Option<String>,
    max_memory_mb: u32,
    extra_jvm_args: Vec<String>,
    report: &F,
) -> Result<PreparedLaunch, LaunchError>
where
    F: Fn(ProgressUpdate),
{
    let paths = GamePaths::new(game_root);
    std::fs::create_dir_all(&instance_dir)?;

    report(ProgressUpdate::stage("Resolving version", 0, 1));

    // Vanilla base version JSON.
    let manifest: VersionManifest = client
        .get(VERSION_MANIFEST_URL)
        .send()
        .await?
        .json()
        .await
        .map_err(|e| LaunchError::Parse(format!("version manifest: {e}")))?;
    let entry = manifest
        .find(game_version)
        .ok_or_else(|| LaunchError::VersionNotFound(game_version.to_string()))?;

    let vanilla: VersionJson = client
        .get(&entry.url)
        .send()
        .await?
        .json()
        .await
        .map_err(|e| LaunchError::Parse(format!("version json: {e}")))?;

    // Resolve Java up front: loaders inherit vanilla's Java requirement, and
    // Forge/NeoForge processors need a JVM to run during install.
    let required_major = vanilla
        .java_version
        .as_ref()
        .and_then(|j| j.major_version)
        .unwrap_or(8);
    let component = vanilla
        .java_version
        .as_ref()
        .and_then(|j| j.component.clone());
    let (java_path, java_major) = resolve_java(
        java_override,
        required_major,
        component.as_deref(),
        game_version,
        &paths,
        client,
        report,
    )
    .await?;

    // Layer the loader on top when requested.
    let version = match loader {
        ModLoader::Vanilla => vanilla,
        ModLoader::Fabric => {
            report(ProgressUpdate::stage("Resolving Fabric", 0, 1));
            let lv = match loader_version {
                Some(v) if !v.trim().is_empty() => v,
                _ => fabric::latest_loader_version(client, game_version).await?,
            };
            let profile = fabric::fetch_profile(client, game_version, &lv).await?;
            let mut merged = fabric::merge_onto_parent(profile, vanilla);
            // Keep vanilla's id so client jar / natives / assets paths line up.
            merged.id = game_version.to_string();
            merged
        }
        ModLoader::Forge | ModLoader::NeoForge => {
            let lv = forge::resolve_version(client, loader, game_version, loader_version).await?;
            let mut merged = forge::prepare(
                client, &paths, loader, game_version, &lv, &vanilla, &java_path, report,
            )
            .await?;
            merged.id = game_version.to_string();
            merged
        }
        ModLoader::Quilt => {
            return Err(LaunchError::UnsupportedLoader("Quilt".to_string()));
        }
    };

    // Download client jar, libraries, natives.
    let resolved = files::resolve_libraries(&version, &paths);
    files::fetch_libraries(client, &version, &resolved, &paths, report).await?;

    // Download assets (handles legacy/virtual layouts).
    let asset_layout = files::fetch_assets(client, &version, &paths, &instance_dir, report).await?;

    // Build the classpath: libraries first, then the client jar.
    let mut classpath = resolved.classpath.clone();
    classpath.push(paths.client_jar(&version.id));
    let cp_sep = if cfg!(windows) { ";" } else { ":" };
    let classpath_str = classpath
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(cp_sep);

    let natives_dir = paths.natives_dir(&version.id);

    // Placeholder table shared by JVM and game arguments.
    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("auth_player_name", account.username.clone());
    vars.insert("version_name", version.id.clone());
    vars.insert("game_directory", instance_dir.to_string_lossy().to_string());
    vars.insert("assets_root", paths.assets().to_string_lossy().to_string());
    vars.insert(
        "assets_index_name",
        version
            .asset_index
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_default(),
    );
    vars.insert("auth_uuid", account.uuid.clone());
    vars.insert("auth_access_token", account.minecraft_token.clone());
    vars.insert(
        "auth_session",
        format!("token:{}:{}", account.minecraft_token, account.uuid),
    );
    vars.insert("clientid", String::new());
    vars.insert("auth_xuid", String::new());
    vars.insert("user_type", "msa".to_string());
    vars.insert(
        "version_type",
        version.version_type.clone().unwrap_or_else(|| "release".into()),
    );
    vars.insert("user_properties", "{}".to_string());
    vars.insert(
        "game_assets",
        asset_layout
            .virtual_dir
            .clone()
            .unwrap_or_else(|| paths.assets())
            .to_string_lossy()
            .to_string(),
    );
    vars.insert(
        "natives_directory",
        natives_dir.to_string_lossy().to_string(),
    );
    vars.insert("launcher_name", LAUNCHER_NAME.to_string());
    vars.insert("launcher_version", LAUNCHER_VERSION.to_string());
    vars.insert("classpath", classpath_str.clone());
    // Forge/NeoForge JVM args reference these.
    vars.insert(
        "library_directory",
        paths.libraries().to_string_lossy().to_string(),
    );
    vars.insert("classpath_separator", cp_sep.to_string());

    let mut args: Vec<String> = Vec::new();

    // Memory + a couple of quality-of-life JVM flags first.
    args.push(format!("-Xmx{max_memory_mb}M"));
    args.push("-Xms512M".to_string());
    if cfg!(target_os = "macos") {
        args.push("-XstartOnFirstThread".to_string());
    }
    // User-supplied JVM args (later flags win over our defaults, e.g. -Xmx).
    args.extend(extra_jvm_args);

    let main_class = version
        .main_class
        .clone()
        .ok_or_else(|| LaunchError::Parse("version JSON has no mainClass".into()))?;

    match &version.arguments {
        // Modern (1.13+): structured, rule-gated argument lists.
        Some(arguments) => {
            for arg in &arguments.jvm {
                push_argument(&mut args, arg, &vars);
            }
            args.push(main_class);
            for arg in &arguments.game {
                push_argument(&mut args, arg, &vars);
            }
        }
        // Legacy (<=1.12): supply our own JVM args, then the flat game string.
        None => {
            args.push(format!(
                "-Djava.library.path={}",
                natives_dir.to_string_lossy()
            ));
            args.push("-cp".to_string());
            args.push(classpath_str);
            args.push(main_class);
            if let Some(template) = &version.minecraft_arguments {
                for token in template.split_whitespace() {
                    args.push(substitute(token, &vars));
                }
            }
        }
    }

    report(ProgressUpdate::stage("Ready", 1, 1));

    Ok(PreparedLaunch {
        java_path,
        args,
        working_dir: instance_dir,
        java_major,
    })
}

/// Resolve a Java executable for `required_major`: use the override, else an
/// installed runtime new enough, else auto-download Mojang's `component`.
async fn resolve_java<F>(
    java_override: Option<String>,
    required_major: u32,
    component: Option<&str>,
    game_version: &str,
    paths: &GamePaths,
    client: &Client,
    report: &F,
) -> Result<(String, u32), LaunchError>
where
    F: Fn(ProgressUpdate),
{
    // Both detect_java_runtimes and probe_major spawn `java -version`
    // subprocesses and block on their output with no internal `.await` —
    // this runs on every launch that doesn't have an explicit Java override,
    // not just Forge/NeoForge. spawn_blocking keeps that off the async
    // runtime's worker threads so it doesn't stall unrelated concurrent work
    // (other launches, UI-triggered commands) while it probes every
    // candidate JDK on the system.
    if let Some(explicit) = java_override.filter(|p| !p.trim().is_empty()) {
        let probe_path = explicit.clone();
        let major = tokio::task::spawn_blocking(move || java::probe_major(&probe_path))
            .await
            .ok()
            .flatten()
            .unwrap_or(required_major);
        return Ok((explicit, major));
    }

    let runtimes = tokio::task::spawn_blocking(java::detect_java_runtimes)
        .await
        .unwrap_or_default();
    if let Some(rt) = java::select_at_least(&runtimes, required_major) {
        return Ok((rt.path, rt.major_version));
    }

    // Fall back to a component derived from the required major for old versions
    // (<=1.12) whose JSON carries no `javaVersion.component`.
    let component = component
        .map(str::to_string)
        .or_else(|| default_component_for(required_major).map(str::to_string));

    if let Some(component) = component {
        let path =
            java_runtime::ensure_component(client, &paths.runtimes(), &component, report).await?;
        return Ok((path.to_string_lossy().to_string(), required_major));
    }

    Err(LaunchError::NoJava {
        version: game_version.to_string(),
        required: required_major,
    })
}

/// Map a required Java major to the Mojang runtime component that provides it,
/// used when the version JSON doesn't name one (older Minecraft).
fn default_component_for(major: u32) -> Option<&'static str> {
    Some(match major {
        0..=8 => "jre-legacy",
        9..=16 => "java-runtime-alpha",
        17..=20 => "java-runtime-gamma",
        21..=24 => "java-runtime-delta",
        _ => "java-runtime-epsilon",
    })
}

/// Append a (possibly conditional) argument, applying rules and substitution.
fn push_argument(out: &mut Vec<String>, arg: &Argument, vars: &HashMap<&str, String>) {
    match arg {
        Argument::Plain(value) => out.push(substitute(value, vars)),
        Argument::Conditional { rules, value } => {
            if !rules_allow(rules) {
                return;
            }
            match value {
                ArgValue::Single(v) => out.push(substitute(v, vars)),
                ArgValue::Many(vs) => {
                    for v in vs {
                        out.push(substitute(v, vars));
                    }
                }
            }
        }
    }
}

/// Split a user-entered JVM argument string into tokens, honoring single and
/// double quotes so paths with spaces survive (e.g. `-Dfoo="a b"`).
pub fn split_jvm_args(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;

    for ch in input.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '"' | '\'' => {
                    quote = Some(ch);
                    has_token = true;
                }
                c if c.is_whitespace() => {
                    if has_token {
                        tokens.push(std::mem::take(&mut current));
                        has_token = false;
                    }
                }
                c => {
                    current.push(c);
                    has_token = true;
                }
            },
        }
    }
    if has_token {
        tokens.push(current);
    }
    tokens
}

/// Replace every `${name}` token using the placeholder table (unknown tokens
/// are left as-is).
fn substitute(input: &str, vars: &HashMap<&str, String>) -> String {
    let mut result = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find('}') {
            let key = &after[..end];
            match vars.get(key) {
                Some(value) => result.push_str(value),
                None => {
                    result.push_str("${");
                    result.push_str(key);
                    result.push('}');
                }
            }
            rest = &after[end + 1..];
        } else {
            result.push_str(&rest[start..]);
            rest = "";
        }
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_known_tokens() {
        let mut vars = HashMap::new();
        vars.insert("auth_player_name", "Steve".to_string());
        assert_eq!(
            substitute("--username ${auth_player_name}", &vars),
            "--username Steve"
        );
        assert_eq!(substitute("${unknown}", &vars), "${unknown}");
    }

    /// Network test: parse real Mojang JSON for a modern (structured arguments)
    /// and a legacy (minecraftArguments) version, and resolve their libraries.
    /// Run with `cargo test -- --ignored` (needs internet).
    #[tokio::test]
    #[ignore]
    async fn resolves_real_versions() {
        let client = crate::download::http_client().unwrap();
        let manifest: VersionManifest = client
            .get(VERSION_MANIFEST_URL)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        for id in ["1.20.1", "1.8.9"] {
            let entry = manifest.find(id).unwrap_or_else(|| panic!("no {id}"));
            let vj: VersionJson = client
                .get(&entry.url)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap_or_else(|e| panic!("{id} parse: {e}"));

            assert!(vj.main_class.is_some(), "{id} missing mainClass");
            assert!(
                vj.arguments.is_some() || vj.minecraft_arguments.is_some(),
                "{id} missing arguments"
            );
            assert!(vj.asset_index.is_some(), "{id} missing assetIndex");

            let paths = files::GamePaths::new(std::env::temp_dir().join("waybound-test"));
            let resolved = files::resolve_libraries(&vj, &paths);
            assert!(!resolved.classpath.is_empty(), "{id} empty classpath");
        }
    }

    /// Network test: fetch the latest Fabric profile for 1.20.1 and merge it.
    #[tokio::test]
    #[ignore]
    async fn merges_real_fabric_profile() {
        let client = crate::download::http_client().unwrap();
        let manifest: VersionManifest = client
            .get(VERSION_MANIFEST_URL)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let entry = manifest.find("1.20.1").unwrap();
        let vanilla: VersionJson =
            client.get(&entry.url).send().await.unwrap().json().await.unwrap();

        let lv = fabric::latest_loader_version(&client, "1.20.1").await.unwrap();
        let profile = fabric::fetch_profile(&client, "1.20.1", &lv).await.unwrap();
        let merged = fabric::merge_onto_parent(profile, vanilla);

        // Fabric supplies its own launch entrypoint and keeps vanilla's assets.
        assert!(merged
            .main_class
            .as_deref()
            .unwrap()
            .contains("fabric"));
        assert!(merged.asset_index.is_some());
    }
}
