use crate::dto::instance::{
    CreateInstanceInput, GameVersionOption, InstallModInput, InstallModResult, InstalledMod,
    InstanceSummary, MissingMod,
};
use crate::dto::{ContentType, ModSource, ModSummary};
use crate::download::safe_join;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use super::search::AppState;
use crate::instances::{operations::acquire, InstanceError, InstanceService};
use crate::modpack::{pending_missing_mods, remove_pack_manifest_entry};

/// Emitted while a modpack downloads its files, so the frontend can show
/// "X / Y files" instead of an indeterminate spinner.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallProgressEvent {
    install_id: String,
    current: u32,
    total: u32,
    /// The file just finished (or, for the initial 0/total event, empty) —
    /// downloads run concurrently, so this is "most recently completed,"
    /// not a strict single "downloading now," but it's what actually gives
    /// the user a sense of what's happening instead of a bare counter.
    current_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub ok: bool,
}

#[tauri::command]
pub fn list_instances(state: State<'_, AppState>) -> Result<Vec<InstanceSummary>, String> {
    InstanceService::list(&state.db).map_err(map_error)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMissingMods {
    pub instance_id: String,
    pub instance_name: String,
    pub missing_mods: Vec<MissingMod>,
}

/// The missing-mods flow's progress used to live only in the frontend's
/// in-memory install list — restarting the app (or just closing the toast)
/// lost all memory of "this instance still has N mods to grab manually."
/// Called once at startup so HomePage can re-surface exactly the same
/// "Download missing mods"/"Open all" UI for anything still pending,
/// reading the same per-instance manifest sidecar the reconciliation logic
/// already maintains — nothing new to keep in sync.
#[tauri::command]
pub fn list_pending_missing_mods(state: State<'_, AppState>) -> Result<Vec<PendingMissingMods>, String> {
    let instances = InstanceService::list(&state.db).map_err(map_error)?;
    Ok(instances
        .into_iter()
        .filter_map(|instance| {
            let missing_mods = pending_missing_mods(std::path::Path::new(&instance.root_path));
            if missing_mods.is_empty() {
                None
            } else {
                Some(PendingMissingMods {
                    instance_id: instance.id,
                    instance_name: instance.name,
                    missing_mods,
                })
            }
        })
        .collect())
}

/// Permanently stops tracking one project as missing for this instance — for
/// a mod the user has decided not to get, not one they just haven't gotten to
/// yet. `pending_missing_mods` otherwise has no way to tell those two cases
/// apart: it only ever looks at "does the file exist on disk," so an opted-out
/// mod would nag on every restart forever without this.
#[tauri::command]
pub fn dismiss_missing_mod(instance_id: String, project_id: u32) -> Result<(), String> {
    let _operation = acquire(&instance_id)?;
    let root = crate::instances::paths::instance_root(&instance_id).map_err(|e| e.to_string())?;
    remove_pack_manifest_entry(&root, project_id);
    Ok(())
}

#[tauri::command]
pub fn create_instance(
    state: State<'_, AppState>,
    input: CreateInstanceInput,
) -> Result<InstanceSummary, String> {
    let instance = InstanceService::create(
        &state.db,
        &input.name,
        &input.minecraft_version,
        input.loader,
        input.loader_version,
    )
    .map_err(map_error)?;

    apply_global_mc_options_if_configured(&state, &instance.id);

    Ok(instance)
}

#[tauri::command]
pub fn rename_instance(
    state: State<'_, AppState>,
    instance_id: String,
    name: String,
) -> Result<(), String> {
    let _operation = acquire(&instance_id)?;
    let name = name.trim();
    if name.len() < 2 {
        return Err("Instance name must be at least 2 characters.".to_string());
    }
    if name.chars().count() > crate::instances::MAX_INSTANCE_NAME_LEN {
        return Err(format!(
            "Instance name must be {} characters or fewer.",
            crate::instances::MAX_INSTANCE_NAME_LEN
        ));
    }
    state.db.rename_instance(&instance_id, name).map_err(|err| {
        // The name column is UNIQUE; surface a clear message on collision.
        if err.to_string().contains("UNIQUE") {
            "An instance with that name already exists.".to_string()
        } else {
            err.to_string()
        }
    })
}

/// Cached loader-version index for an explicit loader + game version:
/// latest + recommended builds with a daily TTL (see `loader_meta`), so
/// version displays rarely touch the network and survive offline from
/// yesterday's answers. Unlike `get_latest_loader_version` below this
/// covers Fabric and Quilt too — not just Forge/NeoForge.
#[tauri::command]
pub async fn get_loader_version_info(
    state: State<'_, AppState>,
    loader: crate::dto::ModLoader,
    mc_version: String,
) -> Result<crate::loader_meta::LoaderVersionInfo, String> {
    crate::loader_meta::get_loader_version_info(&state.db, loader, &mc_version).await
}

/// Looks up what the loader's own "recommended" build currently is for this
/// instance's Minecraft version — the exact same lookup `resolve_version`
/// falls back to at launch time when no build is pinned. `None` for
/// Fabric/Quilt/Vanilla, which have no equivalent "latest recommended build"
/// concept in this codebase (Fabric's loader versions have no "recommended"
/// notion the way Forge/NeoForge's promotions do, and Quilt isn't a
/// supported launch loader at all).
#[tauri::command]
pub async fn get_latest_loader_version(    state: State<'_, AppState>,
    instance_id: String,
) -> Result<Option<String>, String> {
    let instance = state
        .db
        .get_instance(&instance_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Instance not found.".to_string())?;

    let client = crate::download::http_client().map_err(|err| err.to_string())?;
    match instance.loader {
        crate::dto::ModLoader::Forge => {
            crate::launch::forge::latest_forge_build(&client, &instance.minecraft_version)
                .await
                .map(Some)
                .map_err(|err| err.to_string())
        }
        crate::dto::ModLoader::NeoForge => {
            crate::launch::forge::latest_neoforge(&client, &instance.minecraft_version)
                .await
                .map(Some)
                .map_err(|err| err.to_string())
        }
        _ => Ok(None),
    }
}

/// Pins (or, with `None`, un-pins) the instance's loader build. Takes effect
/// on the next launch — there's no separate "install" step, since
/// `forge::prepare`/the Fabric equivalent already download and cache
/// whatever build is resolved fresh on every launch.
#[tauri::command]
pub fn set_instance_loader_version(
    state: State<'_, AppState>,
    instance_id: String,
    loader_version: Option<String>,
) -> Result<(), String> {
    let _operation = acquire(&instance_id)?;
    state
        .db
        .set_instance_loader_version(&instance_id, loader_version.as_deref())
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub fn set_instance_icon(
    state: State<'_, AppState>,
    instance_id: String,
    icon: Option<String>,
) -> Result<(), String> {
    let _operation = acquire(&instance_id)?;
    state
        .db
        .set_instance_icon(&instance_id, icon.as_deref())
        .map_err(|err| err.to_string())
}

/// Async so the potentially large file copy runs off the main thread.
#[tauri::command]
pub async fn duplicate_instance(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<InstanceSummary, String> {
    let _operation = acquire(&instance_id)?;
    InstanceService::duplicate(&state.db, &instance_id).map_err(map_error)
}

#[tauri::command]
pub fn delete_instance(state: State<'_, AppState>, instance_id: String) -> Result<DeleteResult, String> {
    let _operation = acquire(&instance_id)?;
    InstanceService::delete(&state.db, &instance_id).map_err(map_error)?;
    Ok(DeleteResult { ok: true })
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Opens a folder in the user's default file manager via one direct shell
/// call. `@tauri-apps/plugin-opener`'s `openPath` was observed opening the
/// real default handler (e.g. a third-party manager like File Pilot)
/// correctly, then Explorer *again* several seconds later — some fallback
/// path inside the plugin firing when it doesn't get a fast/definite success
/// signal back from a non-standard registered handler. `cmd /C start ""`
/// resolves through the exact same OS folder-open association Explorer
/// itself uses when you double-click a folder, with no secondary fallback
/// of our own to misfire.
#[tauri::command]
pub fn open_in_file_manager(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &path])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        std::process::Command::new(opener)
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn list_instance_mods(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<Vec<InstalledMod>, String> {
    InstanceService::list_mods(&state.db, &instance_id).map_err(map_error)
}

#[tauri::command]
pub async fn install_mod_to_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    input: InstallModInput,
    install_id: String,
) -> Result<InstallModResult, String> {
    let instance_id = resolve_install_target(&state, &input)?;
    let _operation = acquire(&instance_id)?;

    let cancel = crate::download::CancelToken::new();
    state
        .installs
        .lock()
        .unwrap()
        .insert(install_id.clone(), cancel.clone());
    // Removes the registry entry on every exit path, including a panic
    // unwinding through InstanceService::install_mod (e.g. malformed pack
    // data) — without this, the manual `.remove()` below is skipped on panic
    // and the entry leaks in `state.installs` for the rest of the app's
    // lifetime (inert, but the launch registry already guards against the
    // same class of leak via its own task-join mechanism).
    struct InstallGuard<'a> {
        installs: &'a std::sync::Mutex<std::collections::HashMap<String, crate::download::CancelToken>>,
        id: &'a str,
    }
    impl Drop for InstallGuard<'_> {
        fn drop(&mut self) {
            self.installs.lock().unwrap().remove(self.id);
        }
    }
    let _install_guard = InstallGuard {
        installs: &state.installs,
        id: &install_id,
    };

    let report = {
        let install_id = install_id.clone();
        move |current: u32, total: u32, current_name: &str| {
            let _ = app.emit(
                "install://progress",
                InstallProgressEvent {
                    install_id: install_id.clone(),
                    current,
                    total,
                    current_name: current_name.to_string(),
                },
            );
        }
    };

    let install_result = InstanceService::install_mod(
        &state.db,
        &state.config,
        &state.modrinth,
        &state.curseforge,
        &instance_id,
        &input.mod_summary,
        input.source,
        input.version_id.as_deref(),
        false,
        input.origin,
        &cancel,
        &report,
    )
    .await;

    let mut result = install_result.map_err(map_error)?;

    // Modpacks ship their own overrides/options.txt, which clobbers whatever
    // global settings were applied at instance creation — whether the instance
    // was just created for this install or already existed (e.g. a blank
    // instance created earlier, then a modpack installed into it afterward).
    // Re-apply after every modpack install, not just newly-created instances,
    // so global settings always win over the pack's own options.txt.
    if input.mod_summary.project_type == crate::dto::ContentType::Modpack {
        apply_global_mc_options_if_configured(&state, &instance_id);
    }

    result.instance = state
        .db
        .get_instance(&instance_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Instance not found after install.".to_string())?;

    crate::activity::append_log(
        &result.message,
        "info",
        Some(&input.mod_summary.uid),
    );

    Ok(result)
}

/// Builds a `ModSummary` good enough to identify the project (uid, source,
/// ids) and show something reasonable immediately (name, icon) from a
/// tracked content row — without any network round trip, since everything
/// needed already lives in `instance_mods`. Errs for a row with no real
/// project behind it (an internal `file:<name>` record — a modpack-dropped
/// or manually-added file Waybound never resolved to a CurseForge/Modrinth
/// project).
fn mod_summary_from_row(row: &InstalledMod, not_tracked_msg: &str) -> Result<ModSummary, String> {
    let Some((source_str, id_str)) = row.mod_uid.split_once(':') else {
        return Err(not_tracked_msg.to_string());
    };

    let mut summary = ModSummary {
        uid: row.mod_uid.clone(),
        slug: String::new(),
        name: row.mod_name.clone(),
        description: String::new(),
        author: String::new(),
        icon_url: row.icon_url.clone(),
        downloads: 0,
        project_type: ContentType::Mod,
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
                .map_err(|_| "Invalid CurseForge id on record for this mod.".to_string())?;
            summary.curseforge_id = Some(id);
            summary.sources.push(ModSource::Curseforge);
        }
        "modrinth" => {
            summary.modrinth_id = Some(id_str.to_string());
            summary.sources.push(ModSource::Modrinth);
        }
        _ => return Err(not_tracked_msg.to_string()),
    };
    Ok(summary)
}

/// Resolves a Content-tab file back to a `ModSummary` so the frontend can
/// open its project page — the same "isn't tracked from Browse" rejection
/// as `update_mod_in_instance`, for the same reason (a `file:` record has no
/// real project to open a page for).
#[tauri::command]
pub fn get_mod_summary_for_content(
    state: State<'_, AppState>,
    instance_id: String,
    file_name: String,
) -> Result<ModSummary, String> {
    let existing = state.db.list_instance_mods(&instance_id).map_err(|e| e.to_string())?;
    let row = existing
        .into_iter()
        .find(|m| m.file_name == file_name)
        .ok_or_else(|| format!("'{file_name}' is not tracked in this instance."))?;
    mod_summary_from_row(
        &row,
        "This file isn't tracked from Browse, so there's no project page to open.",
    )
}

/// Identifies an on-disk jar by content hash (Modrinth `version_files`, then
/// CurseForge fingerprints) — the fallback for files with no DB tracking
/// row (modpack drops, manual adds, renames), which
/// `get_mod_summary_for_content` rejects. Returns a real project summary
/// plus the exact matched version, so the frontend can offer the same
/// versions/update flow.
#[tauri::command]
pub async fn identify_mod_file(
    state: State<'_, AppState>,
    instance_id: String,
    file_name: String,
) -> Result<crate::identify::IdentifiedMod, String> {
    crate::identify::identify_mod_file(
        &state.modrinth,
        &state.curseforge,
        &state.config,
        &instance_id,
        &file_name,
    )
    .await
}

/// Re-resolves a Content-tab mod against its own project and installs
/// whatever the instance's Minecraft version + loader currently resolve to
/// — the same thing Browse's install button does, just re-triggered for a
/// mod already on disk. Only works for a mod actually tracked back to a
/// CurseForge/Modrinth project; a modpack-dropped or manually-added file
/// (tracked, if at all, under an internal `file:` id) has no project to
/// check against, and this rejects it clearly rather than silently no-op'ing.
#[tauri::command]
pub async fn update_mod_in_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_id: String,
    file_name: String,
    install_id: String,
    version_id: Option<String>,
) -> Result<InstallModResult, String> {
    let _operation = acquire(&instance_id)?;
    let existing = state.db.list_instance_mods(&instance_id).map_err(|e| e.to_string())?;
    let row = existing
        .into_iter()
        .find(|m| m.file_name == file_name)
        .ok_or_else(|| format!("'{file_name}' is not tracked in this instance."))?;

    let summary = mod_summary_from_row(
        &row,
        "This file isn't tracked from Browse, so there's nothing to check for updates against.",
    )?;
    let preferred_source = summary.sources[0];


    let cancel = crate::download::CancelToken::new();
    state
        .installs
        .lock()
        .unwrap()
        .insert(install_id.clone(), cancel.clone());
    struct InstallGuard<'a> {
        installs: &'a std::sync::Mutex<std::collections::HashMap<String, crate::download::CancelToken>>,
        id: &'a str,
    }
    impl Drop for InstallGuard<'_> {
        fn drop(&mut self) {
            self.installs.lock().unwrap().remove(self.id);
        }
    }
    let _install_guard = InstallGuard {
        installs: &state.installs,
        id: &install_id,
    };

    let report = {
        let install_id = install_id.clone();
        move |current: u32, total: u32, current_name: &str| {
            let _ = app.emit(
                "install://progress",
                InstallProgressEvent {
                    install_id: install_id.clone(),
                    current,
                    total,
                    current_name: current_name.to_string(),
                },
            );
        }
    };

    let install_result = InstanceService::install_mod(
        &state.db,
        &state.config,
        &state.modrinth,
        &state.curseforge,
        &instance_id,
        &summary,
        Some(preferred_source),
        version_id.as_deref(),
        true,
        // Updating (or version-switching) a mod is maintenance, not adding:
        // the row's origin survives the delete-and-reinstall below.
        Some(row.origin),
        &cancel,
        &report,
    )
    .await;

    let mut result = install_result.map_err(map_error)?;
    if result.installed.is_some() {
        let _ = state.db.delete_content_meta_cache(&instance_id, "mod", &file_name);
    }
    if result.installed.is_some() {
        // The replacement is verified on disk and tracked; only now is the
        // superseded file deleted. Same filename means install_mod overwrote
        // it in place, so there is nothing to remove.
        if result.installed.as_ref().map(|m| m.file_name.as_str()) != Some(file_name.as_str()) {
            if let Ok(root) = crate::instances::paths::instance_root(&instance_id) {
                if let Ok(old_path) = safe_join(&root.join("mods"), &file_name) {
                    let _ = std::fs::remove_file(old_path);
                }
            }
        }
    }
    result.instance = state.db.get_instance(&instance_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Instance not found after update.".to_string())?;
    Ok(result)
}

/// Signals the in-flight install (if any) to stop at its next chunk/file
/// boundary. A no-op if the install already finished — the frontend can
/// call this without racing to check whether it's too late.
#[tauri::command]
pub fn cancel_install(state: State<'_, AppState>, install_id: String) -> Result<(), String> {
    if let Some(token) = state.installs.lock().unwrap().get(&install_id) {
        token.cancel();
    }
    Ok(())
}

#[tauri::command]
pub fn pause_install(state: State<'_, AppState>, install_id: String) -> Result<(), String> {
    if let Some(token) = state.installs.lock().map_err(|e| e.to_string())?.get(&install_id) {
        token.pause();
    }
    Ok(())
}

#[tauri::command]
pub fn resume_install(state: State<'_, AppState>, install_id: String) -> Result<(), String> {
    if let Some(token) = state.installs.lock().map_err(|e| e.to_string())?.get(&install_id) {
        token.resume();
    }
    Ok(())
}

#[tauri::command]
pub fn remove_mod_from_instance(
    state: State<'_, AppState>,
    instance_id: String,
    mod_uid: String,
) -> Result<DeleteResult, String> {
    let _operation = acquire(&instance_id)?;
    InstanceService::remove_mod(&state.db, &instance_id, &mod_uid).map_err(map_error)?;
    Ok(DeleteResult { ok: true })
}

const VERSION_CACHE_KEY: &str = "durable:game-versions";
const VERSION_REFRESH_SECS: u64 = 24 * 60 * 60;
static VERSION_REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

async fn refresh_minecraft_versions(state: &AppState) -> Result<Vec<GameVersionOption>, String> {
    let versions = tokio::time::timeout(
        std::time::Duration::from_secs(5), state.modrinth.list_game_versions(),
    ).await.map_err(|_| "Minecraft version lookup timed out.".to_string())?
        .map_err(|e| e.to_string())?;
    if versions.is_empty() { return Err("Minecraft version list was empty.".to_string()); }
    if let Ok(json) = serde_json::to_string(&versions) {
        let _ = state.db.put_cached_json(VERSION_CACHE_KEY, &json);
    }
    Ok(versions)
}

fn refresh_versions_in_background(app: AppHandle) {
    use std::sync::atomic::Ordering;
    if VERSION_REFRESHING.swap(true, Ordering::SeqCst) { return; }
    tauri::async_runtime::spawn(async move {
        struct RefreshGuard;
        impl Drop for RefreshGuard {
            fn drop(&mut self) { VERSION_REFRESHING.store(false, Ordering::SeqCst); }
        }
        let _guard = RefreshGuard;
        let state = app.state::<AppState>();
        let _ = refresh_minecraft_versions(&state).await;
    });
}

#[tauri::command]
pub async fn list_minecraft_versions(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<GameVersionOption>, String> {
    if let Ok(Some((json, fetched))) = state.db.get_cached_json(VERSION_CACHE_KEY) {
        if let Ok(versions) = serde_json::from_str::<Vec<GameVersionOption>>(&json) {
            if !versions.is_empty() {
                if crate::db::now_unix().saturating_sub(fetched) >= VERSION_REFRESH_SECS {
                    refresh_versions_in_background(app);
                }
                return Ok(versions);
            }
        }
    }
    let mut known = std::collections::BTreeSet::new();
    for instance in state.db.list_instances().map_err(|e| e.to_string())? {
        if !instance.minecraft_version.is_empty() { known.insert(instance.minecraft_version); }
    }
    if !known.is_empty() {
        refresh_versions_in_background(app);
        return Ok(known.into_iter().rev().map(|version| GameVersionOption {
            version, version_type: "local".to_string(),
        }).collect());
    }
    refresh_minecraft_versions(&state).await
}

fn resolve_install_target(
    state: &AppState,
    input: &InstallModInput,
) -> Result<String, String> {
    match (&input.instance_id, &input.create_instance) {
        (Some(id), None) => Ok(id.clone()),
        (None, Some(create)) => {
            let created = InstanceService::create(
                &state.db,
                &create.name,
                &create.minecraft_version,
                create.loader,
                create.loader_version.clone(),
            )
            .map_err(map_error)?;
            apply_global_mc_options_if_configured(state, &created.id);
            Ok(created.id)
        }
        (Some(_), Some(_)) => {
            Err("Choose either an existing instance or create a new one, not both.".to_string())
        }
        (None, None) => Err("Choose an existing instance or create a new one.".to_string()),
    }
}

/// Writes the saved global Minecraft options into a freshly created instance,
/// if the user has opted in to auto-applying them to new instances. Shared by
/// both the plain "Create instance" flow and modpack installs that create a
/// new instance on the fly, since only the former used to call this.
fn apply_global_mc_options_if_configured(state: &AppState, instance_id: &str) {
    if !state.config.apply_global_mc_options_to_new_instances() {
        return;
    }
    let Some(options) = state.config.global_mc_options() else {
        return;
    };
    if !options.customize {
        return;
    }
    if let Ok(root) = crate::instances::paths::instance_root(instance_id) {
        if let Err(err) = crate::settings::write_options(&root, &options) {
            crate::activity::append_log(
                &format!("Could not apply global game settings: {err}"),
                "warn",
                None,
            );
        }
    }
}

fn map_error(error: InstanceError) -> String {
    error.to_string()
}

#[cfg(test)]
mod mod_summary_from_row_tests {
    use super::mod_summary_from_row;
    use crate::dto::instance::InstalledMod;
    use crate::dto::{ContentType, ModSource};

    const NOT_TRACKED: &str = "not tracked";

    fn row(mod_uid: &str) -> InstalledMod {
        InstalledMod {
            id: 1,
            instance_id: "inst".to_string(),
            mod_uid: mod_uid.to_string(),
            mod_name: "Some Mod".to_string(),
            source: ModSource::Curseforge,
            file_name: "somemod.jar".to_string(),
            installed_at: 0,
            icon_url: Some("https://example.invalid/icon.png".to_string()),
            origin: crate::dto::ModOrigin::User,
        }
    }

    #[test]
    fn resolves_a_curseforge_row_to_its_numeric_project_id() {
        let summary = mod_summary_from_row(&row("curseforge:238222"), NOT_TRACKED).unwrap();
        assert_eq!(summary.curseforge_id, Some(238222));
        assert_eq!(summary.modrinth_id, None);
        assert_eq!(summary.sources, vec![ModSource::Curseforge]);
        assert_eq!(summary.project_type, ContentType::Mod);
        assert_eq!(summary.name, "Some Mod");
        assert_eq!(summary.icon_url.as_deref(), Some("https://example.invalid/icon.png"));
    }

    #[test]
    fn resolves_a_modrinth_row_keeping_its_id_as_an_opaque_string() {
        // Modrinth ids are base62, not numbers — parsing them would break
        // every project whose id happens not to be all digits.
        let summary = mod_summary_from_row(&row("modrinth:AABBccdd"), NOT_TRACKED).unwrap();
        assert_eq!(summary.modrinth_id.as_deref(), Some("AABBccdd"));
        assert_eq!(summary.curseforge_id, None);
        assert_eq!(summary.sources, vec![ModSource::Modrinth]);
    }

    #[test]
    fn rejects_untracked_and_unknown_sources_with_the_callers_message() {
        // `file:` is what a modpack-dropped or hand-added jar is recorded
        // as; it has no project page, and the caller supplies the wording.
        for uid in ["file:somemod.jar", "steam:12345", "no-separator"] {
            let err = mod_summary_from_row(&row(uid), NOT_TRACKED).unwrap_err();
            assert_eq!(err, NOT_TRACKED, "uid {uid} should be rejected");
        }
    }

    #[test]
    fn rejects_a_curseforge_row_whose_id_is_not_a_number() {
        // Distinct from "not tracked": the record claims CurseForge but is
        // corrupt, so the user gets a different, accurate message.
        let err = mod_summary_from_row(&row("curseforge:not-a-number"), NOT_TRACKED).unwrap_err();
        assert_ne!(err, NOT_TRACKED);
        assert!(err.contains("Invalid CurseForge id"), "unexpected message: {err}");
    }

    #[test]
    fn keeps_only_the_first_separator_so_ids_containing_colons_survive() {
        // `split_once` (not `split`) matters: a Modrinth id is opaque and a
        // future one containing ':' must not be silently truncated.
        let summary = mod_summary_from_row(&row("modrinth:ab:cd"), NOT_TRACKED).unwrap();
        assert_eq!(summary.modrinth_id.as_deref(), Some("ab:cd"));
    }
}
