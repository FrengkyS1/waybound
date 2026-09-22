mod detect;
pub(crate) use detect::{detect_launchers, DetectedLauncher};

mod export;
mod files;
mod formats;

use std::path::{Path, PathBuf};
use crate::commands::AppState;
use crate::download::CancelToken;
use crate::dto::instance::InstanceSummary;
use crate::instances::InstanceService;
use files::{check_cancel, Stage};

pub(crate) async fn export_instance(state: &AppState, instance_id: &str, destination: &Path) -> Result<String, String> {
    let _operation = crate::instances::operations::acquire(instance_id)?;
    let instance = state.db.get_instance(instance_id).map_err(|e| e.to_string())?.ok_or("Instance not found.")?;
    export::export(&instance, destination, &CancelToken::new()).await
}

pub(crate) async fn import_instance(
    state: &AppState,
    source: &Path,
    name: Option<&str>,
    cancel: &CancelToken,
    report: &impl Fn(u32, u32, &str),
) -> Result<InstanceSummary, String> {
    files::reject_links(source)?;
    let instances = crate::instances::paths::instances_root().map_err(|e| e.to_string())?;
    let stage = Stage::new(&instances)?;
    let game = stage.0.join("game");
    std::fs::create_dir(&game).map_err(|e| e.to_string())?;
    let metadata;
    let mut version_label = None;
    if source.is_dir() {
        let (meta, game_source) = formats::directory(source)?;
        if game_source.canonicalize().map_err(|e| e.to_string())?.starts_with(instances.canonicalize().map_err(|e| e.to_string())?) {
            return Err("Use Duplicate for a Waybound instance. Imports must come from another launcher's folder.".into());
        }
        metadata = meta;
        files::copy_game(&game_source, &game, cancel)?;
    } else {
        let extension = source.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
        if extension != "mrpack" && extension != "zip" { return Err("Select a .mrpack or .zip (CurseForge / FTB App / Technic / Prism-MultiMC export).".into()); }
        let bytes = files::read_bounded(source, files::MAX_ARCHIVE)?;
        let unpacked = stage.0.join("archive");
        std::fs::create_dir(&unpacked).map_err(|e| e.to_string())?;
        files::extract(&bytes, &unpacked, cancel)?;
        let index = unpacked.join("modrinth.index.json");
        let manifest = unpacked.join("manifest.json");
        if index.is_file() {
            let index = formats::json(&index)?;
            metadata = formats::mrpack(&index)?;
            validate_mrpack_files(&index)?;
            validate_overrides(&unpacked.join("overrides"), cancel)?;
            validate_overrides(&unpacked.join("client-overrides"), cancel)?;
            let result = crate::modpack::import_modrinth_mrpack_bytes(&bytes, &game, &state.modrinth, cancel, report).await.map_err(|e| e.to_string())?;
            if result.has_skipped { return Err(format!("Import was not published because some files are unavailable. {}", result.message)); }
            version_label = result.version_label;
            std::fs::write(game.join(".waybound-transfer-index.json"), serde_json::to_vec(&index).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        } else if manifest.is_file() {
            let manifest = formats::json(&manifest)?;
            metadata = formats::curseforge(&manifest)?;
            let prefix = manifest.get("overrides").and_then(serde_json::Value::as_str).unwrap_or("overrides");
            let relative = files::relative_path(prefix)?;
            validate_overrides(&unpacked.join(relative), cancel)?;
            let key = state.config.curseforge_api_key().unwrap_or_default();
            if key.is_empty() && manifest.get("files").and_then(serde_json::Value::as_array).is_some_and(|f| !f.is_empty()) {
                return Err("Configure a CurseForge API key in Settings before importing this ZIP, or import its already-installed CurseForge App folder offline.".into());
            }
            let result = crate::modpack::import_curseforge_modpack_zip(&bytes, &game, &key, cancel, report).await.map_err(|e| e.to_string())?;
            if result.has_skipped { return Err(format!("Import was not published because some files need manual download. Import the completed CurseForge App instance folder instead. {}", result.message)); }
            version_label = result.version_label;
        } else {
            let root = unpacked_instance_root(&unpacked)?;
            let (meta, game_source) = formats::directory(&root)?;
            metadata = meta;
            files::copy_game(&game_source, &game, cancel)?;
        }
    }
    check_cancel(cancel)?;
    let final_name = name.unwrap_or(&metadata.name).trim();
    let mut instance = InstanceService::publish_staged(&state.db, final_name, &metadata.minecraft, metadata.loader, metadata.loader_version, &game).map_err(|e| e.to_string())?;
    // Pack labels are optional presentation metadata, not a reason to leave a
    // successfully published import appearing failed to the caller.
    if let Some(label) = version_label {
        if state.db.set_modpack_version_label(&instance.id, Some(&label)).is_ok() { instance.modpack_version_label = Some(label); }
    }
    report(1, 1, &instance.name);
    Ok(instance)
}

/// Locates the single importable instance inside an extracted archive: the
/// archive root itself when it parses as a supported instance, else exactly
/// one parseable subfolder. Trying `formats::directory` (not just a
/// filename probe) means FTB App / Technic / Prism exports are all found by
/// the same rule instead of one rule per launcher.
fn unpacked_instance_root(unpacked: &Path) -> Result<PathBuf, String> {
    if formats::directory(unpacked).is_ok() {
        return Ok(unpacked.to_path_buf());
    }
    let mut roots = Vec::new();
    for entry in std::fs::read_dir(unpacked).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir()
            && formats::directory(&entry.path()).is_ok()
        {
            roots.push(entry.path());
        }
    }
    if roots.len() == 1 {
        Ok(roots.remove(0))
    } else {
        Err("Unsupported ZIP. Expected a Modrinth index, CurseForge manifest, or an FTB App / Technic / Prism-MultiMC instance (at the archive root or in exactly one subfolder).".into())
    }
}

fn validate_overrides(root: &Path, cancel: &CancelToken) -> Result<(), String> {
    if !root.exists() { return Ok(()); }
    // Imported archives are already extracted to an isolated directory. Refuse
    // account/session payloads rather than silently merging launcher credentials.
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
            check_cancel(cancel)?;
            let entry = entry.map_err(|e| e.to_string())?;
            if files::excluded(entry.path().strip_prefix(root).map_err(|e| e.to_string())?, false) {
                return Err("Pack overrides include launcher metadata, credentials, or runtime/cache files. Remove those from the export and retry.".into());
            }
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() { pending.push(entry.path()); }
        }
    }
    Ok(())
}

fn validate_mrpack_files(index: &serde_json::Value) -> Result<(), String> {
    let entries = index.get("files").and_then(serde_json::Value::as_array).ok_or("Missing mrpack files list.")?;
    if entries.len() > 50_000 { return Err("Too many mrpack files.".into()); }
    let mut total = 0u64;
    let mut seen = std::collections::HashSet::new();
    for file in entries {
        let path = file.get("path").and_then(serde_json::Value::as_str).ok_or("Missing mrpack file path.")?;
        let relative = files::relative_path(path)?;
        if files::excluded(&relative, false) || !seen.insert(path.to_ascii_lowercase()) { return Err("Unsafe, private, or duplicate mrpack file path.".into()); }
        let size = file.get("fileSize").and_then(serde_json::Value::as_u64).ok_or("Missing mrpack fileSize.")?;
        total = total.checked_add(size).ok_or("Pack size overflow.")?;
        if size > 512 * 1024 * 1024 || total > 8 * 1024 * 1024 * 1024 { return Err("Modpack exceeds transfer size limit.".into()); }
        for (hash, len) in [("sha1", 40), ("sha512", 128)] {
            let value = file.get("hashes").and_then(|v| v.get(hash)).and_then(serde_json::Value::as_str).ok_or_else(|| format!("Missing mrpack {hash} hash."))?;
            if value.len() != len || !value.bytes().all(|b| b.is_ascii_hexdigit()) { return Err(format!("Invalid mrpack {hash} hash.")); }
        }
        let urls = file.get("downloads").and_then(serde_json::Value::as_array).ok_or("Missing mrpack downloads.")?;
        if urls.is_empty() { return Err("A mrpack file has no download URL.".into()); }
        for url in urls {
            let url = reqwest::Url::parse(url.as_str().ok_or("Invalid download URL.")?).map_err(|e| e.to_string())?;
            if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some()
                || !matches!(url.host_str(), Some("cdn.modrinth.com" | "github.com" | "raw.githubusercontent.com" | "objects.githubusercontent.com" | "mediafilez.forgecdn.net" | "edge.forgecdn.net")) {
                return Err("Pack uses an unsupported download host. Use files hosted on Modrinth, GitHub, or CurseForge CDN.".into());
            }
        }
    }
    Ok(())
}
