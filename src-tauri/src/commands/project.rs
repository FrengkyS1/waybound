use crate::activity;
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
        let cache_key = format!("modpackcontent:{}:{version}", summary.uid);
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

        let cache_key = format!("modpackcontent:{}:{file_id}", summary.uid);
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
