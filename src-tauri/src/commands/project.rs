use crate::activity;
use crate::db::cache_key_prefix;
use crate::dto::project_detail::{
    ActivityLogEntry, ModDetail, ModpackContentResponse,
};
use crate::dto::{ModSource, ModSummary, ContentType};
use crate::modpack::{preview_curseforge_modpack, preview_modrinth_modpack};
use tauri::State;

use super::search::AppState;

#[tauri::command]
pub async fn get_mod_details(
    state: State<'_, AppState>,
    summary: ModSummary,
) -> Result<ModDetail, String> {
    if summary.modrinth_id.is_some() || summary.sources.contains(&ModSource::Modrinth) {
        return state
            .modrinth
            .fetch_project_detail(&summary)
            .await
            .map_err(|err| err.to_string());
    }

    if summary.curseforge_id.is_some() {
        let api_key = state
            .config
            .curseforge_api_key()
            .ok_or_else(|| "CurseForge API key is required to view this project.".to_string())?;
        return state
            .curseforge
            .fetch_mod_detail(&summary, &api_key)
            .await
            .map_err(|err| err.to_string());
    }

    Err("Project source is not supported.".to_string())
}

/// Fetches the installed modpack's project detail (including its version
/// list) for in-place pack switching. The instance only records the pack's
/// project uid at import time, so this rebuilds a minimal `ModSummary`
/// from it — same `source:id` parsing as `mod_summary_from_row`, but typed
/// as a Modpack. Rejects for instances with no recorded pack (manual or
/// mods-only instances have nothing to switch between).
#[tauri::command]
pub async fn get_modpack_detail_for_instance(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<ModDetail, String> {
    let instance = state
        .db
        .get_instance(&instance_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Instance not found.".to_string())?;
    let uid = instance.modpack_project_uid.as_deref().ok_or_else(|| {
        "This instance wasn't installed from a modpack, so there's no pack version to switch to.".to_string()
    })?;
    let Some((source_str, id_str)) = uid.split_once(':') else {
        return Err("Recorded modpack reference is invalid.".to_string());
    };

    let mut summary = ModSummary {
        uid: uid.to_string(),
        slug: String::new(),
        name: instance.name.clone(),
        description: String::new(),
        author: String::new(),
        icon_url: instance.icon.clone(),
        downloads: 0,
        project_type: ContentType::Modpack,
        loaders: Vec::new(),
        sources: Vec::new(),
        updated_at: String::new(),
        curseforge_id: None,
        modrinth_id: None,
    };
    match source_str {
        "curseforge" => {
            let id: u32 = id_str
                .parse()
                .map_err(|_| "Recorded CurseForge modpack id is invalid.".to_string())?;
            summary.curseforge_id = Some(id);
            summary.sources.push(ModSource::Curseforge);
        }
        "modrinth" => {
            summary.modrinth_id = Some(id_str.to_string());
            summary.sources.push(ModSource::Modrinth);
        }
        _ => return Err("Recorded modpack reference is invalid.".to_string()),
    }

    if summary.modrinth_id.is_some() {
        return state
            .modrinth
            .fetch_project_detail(&summary)
            .await
            .map_err(|err| err.to_string());
    }
    if summary.curseforge_id.is_some() {
        let api_key = state
            .config
            .curseforge_api_key()
            .ok_or_else(|| "CurseForge API key is required to view this project.".to_string())?;
        return state
            .curseforge
            .fetch_mod_detail(&summary, &api_key)
            .await
            .map_err(|err| err.to_string());
    }
    Err("Project source is not supported.".to_string())
}

#[tauri::command]
pub async fn get_modpack_content(
    state: State<'_, AppState>,
    summary: ModSummary,
    version_id: Option<String>,
) -> Result<ModpackContentResponse, String> {
    let t0 = std::time::Instant::now();
    crate::activity::append_log(
        &format!("get_modpack_content CALLED uid={}", summary.uid),
        "debug",
        None,
    );
    let result = get_modpack_content_inner(&state, &summary, version_id).await;
    match &result {
        Ok(r) => crate::activity::append_log(
            &format!(
                "get_modpack_content OK elapsed={}ms items={} uid={}",
                t0.elapsed().as_millis(),
                r.items.len(),
                summary.uid,
            ),
            "debug",
            None,
        ),
        Err(e) => crate::activity::append_log(
            &format!(
                "get_modpack_content ERR elapsed={}ms err={e} uid={}",
                t0.elapsed().as_millis(),
                summary.uid,
            ),
            "debug",
            None,
        ),
    }
    result
}

async fn get_modpack_content_inner(
    state: &AppState,
    summary: &ModSummary,
    version_id: Option<String>,
) -> Result<ModpackContentResponse, String> {
    if summary.project_type != ContentType::Modpack {
        return Err("Content listing is only available for modpacks.".to_string());
    }

    if summary.modrinth_id.is_some() || summary.sources.contains(&ModSource::Modrinth) {
        let version = match version_id {
            Some(id) => id,
            None => state
                .modrinth
                .fetch_project_detail(summary)
                .await
                .map_err(|err| err.to_string())?
                .versions
                .first()
                .map(|v| v.id.clone())
                .ok_or_else(|| "No modpack versions found.".to_string())?,
        };

        // A published version's file list never changes, so cache it
        // indefinitely — repeat views (the slow part is downloading the whole
        // .mrpack just to read its file index) become instant.
        let cache_key = format!("{}modpackcontent:{}:{version}", cache_key_prefix(), summary.uid);
        if let Ok(Some((json, _))) = state.db.get_cached_json(&cache_key) {
            if let Ok(cached) = serde_json::from_str(&json) {
                return Ok(cached);
            }
        }

        let result = preview_modrinth_modpack(&state.modrinth, &version).await?;
        if let Ok(json) = serde_json::to_string(&result) {
            let _ = state.db.put_cached_json(&cache_key, &json);
        }
        return Ok(result);
    }

    if summary.curseforge_id.is_some() {
        let api_key = state
            .config
            .curseforge_api_key()
            .ok_or_else(|| "CurseForge API key is required.".to_string())?;
        let mod_id = summary.curseforge_id.ok_or_else(|| "Missing CurseForge id.".to_string())?;
        let file_id: u32 = match version_id {
            Some(id) => id.parse().map_err(|_| "Invalid CurseForge file id.".to_string())?,
            None => {
                let detail = state
                    .curseforge
                    .fetch_mod_detail(summary, &api_key)
                    .await
                    .map_err(|err| err.to_string())?;
                detail
                    .versions
                    .first()
                    .and_then(|v| v.id.parse().ok())
                    .ok_or_else(|| "No modpack versions found.".to_string())?
            }
        };

        let cache_key = format!("{}modpackcontent:{}:{file_id}", cache_key_prefix(), summary.uid);
        if let Ok(Some((json, _))) = state.db.get_cached_json(&cache_key) {
            if let Ok(cached) = serde_json::from_str(&json) {
                return Ok(cached);
            }
        }

        let result = preview_curseforge_modpack(&state.curseforge, mod_id, file_id, &api_key).await?;
        if let Ok(json) = serde_json::to_string(&result) {
            let _ = state.db.put_cached_json(&cache_key, &json);
        }
        return Ok(result);
    }

    Err("This modpack source is not supported for content preview.".to_string())
}

#[tauri::command]
pub async fn get_version_changelog(
    state: State<'_, AppState>,
    summary: ModSummary,
    version_id: String,
) -> Result<Option<String>, String> {
    if summary.modrinth_id.is_some() || summary.sources.contains(&ModSource::Modrinth) {
        return state
            .modrinth
            .fetch_version_changelog(&version_id)
            .await
            .map_err(|err| err.to_string());
    }

    if let Some(mod_id) = summary.curseforge_id {
        let api_key = state
            .config
            .curseforge_api_key()
            .ok_or_else(|| "CurseForge API key is required.".to_string())?;
        let file_id: u32 = version_id
            .parse()
            .map_err(|_| "Invalid CurseForge file id.".to_string())?;
        return state
            .curseforge
            .fetch_file_changelog(mod_id, file_id, &api_key)
            .await
            .map_err(|err| err.to_string());
    }

    Ok(None)
}

#[tauri::command]
pub fn get_activity_logs(limit: Option<usize>) -> Vec<ActivityLogEntry> {
    activity::read_logs(limit.unwrap_or(100))
}
