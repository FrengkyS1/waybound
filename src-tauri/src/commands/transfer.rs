use std::path::Path;
use tauri::{AppHandle, Emitter, State};
use crate::commands::AppState;
use crate::download::CancelToken;
use crate::dto::instance::InstanceSummary;

#[tauri::command]
pub async fn detect_importable_launchers(root_path: Option<String>) -> Result<Vec<crate::transfer::DetectedLauncher>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::transfer::detect_launchers(root_path.as_deref().map(Path::new))
    }).await.map_err(|e| format!("Launcher detection failed: {e}"))?
}

struct InstallGuard<'a> {
    state: &'a AppState,
    id: String,
}
impl Drop for InstallGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut installs) = self.state.installs.lock() { installs.remove(&self.id); }
    }
}

#[tauri::command]
pub async fn import_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    source_path: String,
    name: Option<String>,
) -> Result<InstanceSummary, String> {
    let install_id = format!("import:{source_path}");
    let cancel = CancelToken::new();
    {
        let mut installs = state.installs.lock().map_err(|_| "Install registry unavailable.")?;
        if installs.contains_key(&install_id) { return Err("This source is already being imported.".into()); }
        installs.insert(install_id.clone(), cancel.clone());
    }
    let _guard = InstallGuard { state: &state, id: install_id.clone() };
    let report = |current: u32, total: u32, current_name: &str| {
        let _ = app.emit("install://progress", serde_json::json!({
            "installId": install_id,
            "current": current,
            "total": total,
            "currentName": current_name,
        }));
    };
    crate::transfer::import_instance(&state, Path::new(&source_path), name.as_deref(), &cancel, &report).await
}

#[tauri::command]
pub async fn export_instance(
    state: State<'_, AppState>,
    instance_id: String,
    destination_path: String,
) -> Result<String, String> {
    crate::transfer::export_instance(&state, &instance_id, Path::new(&destination_path)).await
}
