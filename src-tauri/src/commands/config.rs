use crate::config::{
    default_docker_env_path, default_docker_env_path_string, read_docker_env_key_from_path,
    ConfigError, CurseForgeKeySource,
};
use crate::sources::curseforge::CurseForgeProbeResult;
use crate::sources::curseforge_key::{
    curseforge_api_key_from_environment, extract_curseforge_api_key,
};
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeStatus {
    pub configured: bool,
    pub source: Option<CurseForgeKeySource>,
    pub environment_available: bool,
    pub default_docker_env_path: Option<String>,
}

#[tauri::command]
pub fn get_curseforge_status(state: State<'_, super::search::AppState>) -> CurseForgeStatus {
    status_from_state(&state)
}

#[tauri::command]
pub async fn test_curseforge_api_key(
    state: State<'_, super::search::AppState>,
) -> Result<CurseForgeProbeResult, String> {
    let Some((api_key, source)) = state.config.resolve_curseforge_api_key() else {
        return Ok(CurseForgeProbeResult {
            ok: false,
            http_status: 0,
            key_length: 0,
            key_prefix: String::new(),
            message: "No CurseForge API key is saved yet.".to_string(),
            log: vec![
                "No API key found in Waybound config.".to_string(),
                "Tip: import from your Docker `.env` file (CF_API_KEY=...) or set CF_API_KEY in the process environment.".to_string(),
            ],
        });
    };

    Ok(state
        .curseforge
        .probe_api_key(&api_key, Some(key_source_label(source)))
        .await)
}

#[tauri::command]
pub async fn test_curseforge_docker_env_key(
    state: State<'_, super::search::AppState>,
    env_file_path: Option<String>,
) -> Result<CurseForgeProbeResult, String> {
    let (api_key, source_label, docker_warnings) =
        if let Some(path) = env_file_path.filter(|p| !p.trim().is_empty()) {
            let read = read_docker_env_key_from_path(std::path::Path::new(path.trim()))?;
            (
                read.key,
                format!("`.env` file ({path})"),
                read.warnings,
            )
        } else if let Some(path) = default_docker_env_path() {
            let display = path.display().to_string();
            let read = read_docker_env_key_from_path(&path)?;
            (
                read.key,
                format!("`.env` file ({display})"),
                read.warnings,
            )
        } else if let Some((key, var)) = curseforge_api_key_from_environment() {
            (key, format!("process environment ({var})"), Vec::new())
        } else {
            return Ok(CurseForgeProbeResult {
                ok: false,
                http_status: 0,
                key_length: 0,
                key_prefix: String::new(),
                message: "No Docker-style CF_API_KEY found.".to_string(),
                log: vec![
                    "No CF_API_KEY in process environment.".to_string(),
                    "Tip: add docker/prominence2/.env next to docker-compose.yml, or paste the path.".to_string(),
                    "Docker requires doubled `$` in `.env`: CF_API_KEY=$$2a$$10$$... (https://github.com/itzg/docker-minecraft-server/discussions/2588)".to_string(),
                ],
            });
        };

    let mut result = state
        .curseforge
        .probe_api_key(&api_key, Some(&source_label))
        .await;
    prepend_probe_warnings(&mut result, &docker_warnings);
    Ok(result)
}

#[tauri::command]
pub async fn set_curseforge_api_key(
    state: State<'_, super::search::AppState>,
    api_key: String,
    skip_validation: Option<bool>,
) -> Result<CurseForgeStatus, String> {
    let normalized = extract_curseforge_api_key(&api_key)?;

    if !looks_like_curseforge_key(&normalized) {
        return Err(
            "This doesn't look like a CurseForge API key. Keys from console.curseforge.com usually start with \"$2a$10$\" and are about 60 characters long.".to_string(),
        );
    }

    if !skip_validation.unwrap_or(true) {
        let probe = state
            .curseforge
            .probe_api_key(&normalized, Some("pasted import"))
            .await;
        if !probe.ok {
            return Err(probe.message);
        }
    }

    state
        .config
        .set_curseforge_api_key(normalized)
        .map_err(map_config_error)?;

    Ok(status_from_state(&state))
}

#[tauri::command]
pub async fn import_curseforge_api_key_from_env_file(
    state: State<'_, super::search::AppState>,
    path: String,
    skip_validation: Option<bool>,
) -> Result<CurseForgeStatus, String> {
    let resolved = resolve_env_file_path(&path)?;
    let read = read_docker_env_key_from_path(&resolved)?;
    state
        .config
        .set_curseforge_api_key(read.key.clone())
        .map_err(map_config_error)?;

    let key = read.key;

    if !looks_like_curseforge_key(&key) {
        return Err("Imported CF_API_KEY does not look like a valid CurseForge key.".to_string());
    }

    if !skip_validation.unwrap_or(true) {
        let probe = state
            .curseforge
            .probe_api_key(&key, Some("imported `.env` file"))
            .await;
        if !probe.ok {
            return Err(probe.message);
        }
    }

    Ok(status_from_state(&state))
}

#[tauri::command]
pub fn clear_curseforge_api_key(
    state: State<'_, super::search::AppState>,
) -> Result<CurseForgeStatus, String> {
    state
        .config
        .clear_curseforge_api_key()
        .map_err(map_config_error)?;

    Ok(status_from_state(&state))
}

fn status_from_state(state: &super::search::AppState) -> CurseForgeStatus {
    CurseForgeStatus {
        configured: state.config.curseforge_configured(),
        source: state.config.curseforge_key_source(),
        environment_available: state.config.environment_curseforge_available(),
        default_docker_env_path: default_docker_env_path_string(),
    }
}

fn looks_like_curseforge_key(key: &str) -> bool {
    key.starts_with("$2a$") && key.len() >= 50
}

fn key_source_label(source: CurseForgeKeySource) -> &'static str {
    match source {
        CurseForgeKeySource::Config => "Waybound config.toml",
        CurseForgeKeySource::Environment => "process environment (CF_API_KEY)",
    }
}

fn prepend_probe_warnings(result: &mut CurseForgeProbeResult, warnings: &[String]) {
    for (index, warning) in warnings.iter().enumerate() {
        result.log.insert(index + 1, warning.clone());
    }
}

fn resolve_env_file_path(path: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = path.trim();
    if !trimmed.is_empty() {
        return Ok(std::path::PathBuf::from(trimmed));
    }
    default_docker_env_path().ok_or_else(|| {
        "No `.env` path given and docker/prominence2/.env was not found.".to_string()
    })
}

fn map_config_error(error: ConfigError) -> String {
    error.to_string()
}
