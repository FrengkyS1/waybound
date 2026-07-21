use crate::dto::instance::{
    CreateInstanceInput, GameVersionOption, InstallModInput, InstallModResult, InstalledMod,
    InstanceSummary, MissingMod,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use super::search::AppState;
use crate::instances::{InstanceError, InstanceService};
use crate::modpack::pending_missing_mods;

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
    let name = name.trim();
    if name.len() < 2 {
        return Err("Instance name must be at least 2 characters.".to_string());
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

#[tauri::command]
pub fn set_instance_icon(
    state: State<'_, AppState>,
    instance_id: String,
    icon: Option<String>,
) -> Result<(), String> {
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
    InstanceService::duplicate(&state.db, &instance_id).map_err(map_error)
}

#[tauri::command]
pub fn delete_instance(state: State<'_, AppState>, instance_id: String) -> Result<DeleteResult, String> {
    InstanceService::delete(&state.db, &instance_id).map_err(map_error)?;
    Ok(DeleteResult { ok: true })
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
pub fn remove_mod_from_instance(
    state: State<'_, AppState>,
    instance_id: String,
    mod_uid: String,
) -> Result<DeleteResult, String> {
    InstanceService::remove_mod(&state.db, &instance_id, &mod_uid).map_err(map_error)?;
    Ok(DeleteResult { ok: true })
}

#[tauri::command]
pub async fn list_minecraft_versions(
    state: State<'_, AppState>,
) -> Result<Vec<GameVersionOption>, String> {
    state
        .modrinth
        .list_game_versions()
        .await
        .map_err(|err| err.to_string())
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
