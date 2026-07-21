use crate::instances::paths::instance_root;
use crate::settings::{read_options, write_options, McOptions, McOptionsError};
use serde::Serialize;
use tauri::State;

use super::search::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalMcOptionsStatus {
    pub configured: bool,
    pub apply_to_new_instances: bool,
    pub options: McOptions,
}

#[tauri::command]
pub fn get_instance_options(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<McOptions, String> {
    if state.db.get_instance(&instance_id).map_err(|e| e.to_string())?.is_none() {
        return Err("Instance not found.".to_string());
    }
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    read_options(&root).map_err(map_options_error)
}

#[tauri::command]
pub fn save_instance_options(
    state: State<'_, AppState>,
    instance_id: String,
    options: McOptions,
) -> Result<(), String> {
    if state.db.get_instance(&instance_id).map_err(|e| e.to_string())?.is_none() {
        return Err("Instance not found.".to_string());
    }
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    write_options(&root, &options).map_err(map_options_error)
}

#[tauri::command]
pub fn get_global_mc_options(state: State<'_, AppState>) -> GlobalMcOptionsStatus {
    let options = state
        .config
        .global_mc_options()
        .unwrap_or_else(default_mc_options);
    GlobalMcOptionsStatus {
        configured: state.config.global_mc_options().is_some(),
        apply_to_new_instances: state.config.apply_global_mc_options_to_new_instances(),
        options,
    }
}

#[tauri::command]
pub fn save_global_mc_options(
    state: State<'_, AppState>,
    options: McOptions,
    apply_to_new_instances: bool,
) -> Result<GlobalMcOptionsStatus, String> {
    state
        .config
        .set_global_mc_options(options, apply_to_new_instances)
        .map_err(|err| err.to_string())?;
    Ok(get_global_mc_options(state))
}

#[tauri::command]
pub fn apply_global_mc_options_to_all_instances(state: State<'_, AppState>) -> Result<u32, String> {
    let options = state
        .config
        .global_mc_options()
        .ok_or_else(|| "No global game settings saved yet.".to_string())?;
    if !options.customize {
        return Err("Enable “Customize game settings” in global defaults first.".to_string());
    }

    let instances = state.db.list_instances().map_err(|e| e.to_string())?;
    let mut applied = 0u32;
    for instance in instances {
        let root = instance_root(&instance.id).map_err(|e| e.to_string())?;
        write_options(&root, &options).map_err(map_options_error)?;
        applied += 1;
    }
    Ok(applied)
}

fn default_mc_options() -> McOptions {
    McOptions::default()
}

fn map_options_error(err: McOptionsError) -> String {
    err.to_string()
}
