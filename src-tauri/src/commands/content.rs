//! Commands for viewing and managing the content files inside an instance —
//! mods, resource packs, and shader packs — directly on disk. This is the source
//! of truth (it also surfaces files a modpack dropped in that aren't tracked in
//! the database), so enable/disable/remove operate on the files themselves.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;

use crate::download::safe_join;
use crate::dto::instance::{ContentEntry, ContentMeta, InstanceContent};
use crate::instances::paths::instance_root;
use tauri::State;

use super::search::AppState;

const DISABLED_SUFFIX: &str = ".disabled";

/// A mod's own declared display name and embedded icon, read from its jar
/// metadata in one pass. Best-effort: any missing/unreadable/malformed
/// metadata just leaves the field `None` (name falls back to the
/// filename-derived name on the frontend; icon falls back to a DB-recorded
/// one, if any).
struct ModMeta {
    name: Option<String>,
    icon: Option<String>,
}

/// Reads name + icon from a jar's Fabric/Quilt, Forge/NeoForge, or legacy
/// Forge metadata. Opens the zip archive once and reuses it for both lookups
/// instead of the two separate full re-parses this used to do per mod.
fn read_mod_metadata(jar_path: &Path) -> ModMeta {
    let mut meta = ModMeta { name: None, icon: None };
    let Ok(file) = fs::File::open(jar_path) else {
        return meta;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return meta;
    };

    // Fabric / Quilt.
    if let Some(contents) = read_zip_entry(&mut archive, "fabric.mod.json") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
            meta.name = non_empty(value.get("name").and_then(|v| v.as_str()));
            let icon_path = match value.get("icon") {
                Some(serde_json::Value::String(path)) => non_empty(Some(path)),
                Some(serde_json::Value::Object(sizes)) => sizes
                    .values()
                    .filter_map(|v| v.as_str())
                    .last()
                    .and_then(|path| non_empty(Some(path))),
                _ => None,
            };
            if let Some(path) = icon_path {
                meta.icon = read_zip_image_entry(&mut archive, &path);
            }
        }
    }

    // Forge / NeoForge.
    if meta.name.is_none() || meta.icon.is_none() {
        if let Some(contents) = read_zip_entry(&mut archive, "META-INF/mods.toml") {
            if let Ok(value) = toml::from_str::<toml::Value>(&contents) {
                let first_mod = value.get("mods").and_then(|m| m.as_array()).and_then(|a| a.first());
                if meta.name.is_none() {
                    meta.name = non_empty(
                        first_mod
                            .and_then(|m| m.get("displayName"))
                            .and_then(|v| v.as_str()),
                    );
                }
                if meta.icon.is_none() {
                    if let Some(logo) = non_empty(value.get("logoFile").and_then(|v| v.as_str())) {
                        meta.icon = read_zip_image_entry(&mut archive, &logo);
                    }
                }
            }
        }
    }

    // Legacy Forge (1.12 and earlier) — name only, no icon convention.
    if meta.name.is_none() {
        if let Some(contents) = read_zip_entry(&mut archive, "mcmod.info") {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
                let first = value.as_array().and_then(|arr| arr.first()).or_else(|| {
                    value.get("modList").and_then(|v| v.as_array()).and_then(|arr| arr.first())
                });
                meta.name = non_empty(first.and_then(|m| m.get("name")).and_then(|v| v.as_str()));
            }
        }
    }

    meta
}

fn read_zip_entry<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    entry_name: &str,
) -> Option<String> {
    let mut entry = archive.by_name(entry_name).ok()?;
    let mut contents = String::new();
    entry.read_to_string(&mut contents).ok()?;
    Some(contents)
}

/// Reads a resource pack's `pack.png`, the standard convention for its icon.
fn read_resourcepack_icon(zip_path: &Path) -> Option<String> {
    let file = fs::File::open(zip_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    read_zip_image_entry(&mut archive, "pack.png")
}

fn read_zip_image_entry<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    entry_name: &str,
) -> Option<String> {
    let mut entry = archive.by_name(entry_name).ok()?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let mime = match Path::new(entry_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        _ => "image/png",
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{mime};base64,{encoded}"))
}

fn non_empty(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Map a category slug to its folder name inside the instance.
fn category_dir(category: &str) -> Result<&'static str, String> {
    match category {
        "mod" => Ok("mods"),
        "resourcepack" => Ok("resourcepacks"),
        "shaderpack" => Ok("shaderpacks"),
        other => Err(format!("Unknown content category '{other}'.")),
    }
}

/// Lists files in a content directory with no jar/zip parsing at all — just
/// names and sizes off the filesystem, so this is effectively instant even
/// for an instance with hundreds of mods. Display name and icon are resolved
/// lazily per row via `get_content_meta`, once a row actually scrolls into
/// view, instead of opening every file up front.
fn scan_dir(dir: &Path) -> Vec<ContentEntry> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let raw = entry.file_name().to_string_lossy().to_string();
        // Skip our own staging file and hidden dotfiles.
        if raw.starts_with('.') {
            continue;
        }
        let (file_name, enabled) = match raw.strip_suffix(DISABLED_SUFFIX) {
            Some(base) => (base.to_string(), false),
            None => (raw.clone(), true),
        };
        let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(ContentEntry {
            file_name,
            name: None,
            icon: None,
            enabled,
            size_bytes,
        });
    }
    out.sort_by(|a, b| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()));
    out
}

#[tauri::command]
pub async fn list_instance_content(instance_id: String) -> Result<InstanceContent, String> {
    let t0 = std::time::Instant::now();
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;

    // Just a directory listing now — no jar/zip parsing — so this stays fast
    // no matter how big the pack is. Still off the async runtime since it's
    // real (if now trivial) disk I/O.
    let result = tauri::async_runtime::spawn_blocking(move || InstanceContent {
        mods: scan_dir(&root.join("mods")),
        resource_packs: scan_dir(&root.join("resourcepacks")),
        shader_packs: scan_dir(&root.join("shaderpacks")),
    })
    .await
    .map_err(|e| e.to_string());

    match &result {
        Ok(content) => crate::activity::append_log(
            &format!(
                "list_instance_content OK elapsed={}ms mods={} packs={} shaders={} instance={instance_id}",
                t0.elapsed().as_millis(),
                content.mods.len(),
                content.resource_packs.len(),
                content.shader_packs.len(),
            ),
            "debug",
            None,
        ),
        Err(e) => crate::activity::append_log(
            &format!(
                "list_instance_content ERR elapsed={}ms err={e} instance={instance_id}",
                t0.elapsed().as_millis(),
            ),
            "debug",
            None,
        ),
    }
    result
}

/// Locate a content file on disk given its display name (enabled or disabled).
/// `file_name` is frontend-supplied, so it's resolved through `safe_join`
/// rather than trusted as a plain path segment.
fn resolve_file(dir: &Path, file_name: &str) -> Option<PathBuf> {
    let enabled = safe_join(dir, file_name).ok()?;
    if enabled.exists() {
        return Some(enabled);
    }
    let disabled = safe_join(dir, &format!("{file_name}{DISABLED_SUFFIX}")).ok()?;
    if disabled.exists() {
        return Some(disabled);
    }
    None
}

/// Resolves one file's display name + icon by opening just that jar/zip —
/// the frontend calls this per row as it scrolls into view, instead of the
/// whole instance paying for every file's metadata up front.
#[tauri::command]
pub async fn get_content_meta(
    state: State<'_, AppState>,
    instance_id: String,
    category: String,
    file_name: String,
) -> Result<ContentMeta, String> {
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let dir = root.join(category_dir(&category)?);
    let Some(path) = resolve_file(&dir, &file_name) else {
        return Ok(ContentMeta::default());
    };
    let is_jar = file_name.to_lowercase().ends_with(".jar");
    let is_zip = file_name.to_lowercase().ends_with(".zip");

    // Icon recorded when this mod was installed via Browse, used as a
    // fallback when the jar doesn't embed its own icon.
    let db_icon = if category == "mod" {
        state
            .db
            .list_instance_mods(&instance_id)
            .unwrap_or_default()
            .into_iter()
            .find(|m| m.file_name == file_name)
            .and_then(|m| m.icon_url)
    } else {
        None
    };

    tauri::async_runtime::spawn_blocking(move || match category.as_str() {
        "mod" if is_jar => {
            let meta = read_mod_metadata(&path);
            ContentMeta {
                name: meta.name,
                icon: meta.icon.or(db_icon),
            }
        }
        "resourcepack" if is_zip => ContentMeta {
            name: None,
            icon: read_resourcepack_icon(&path),
        },
        _ => ContentMeta::default(),
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_content_enabled(
    _state: State<'_, AppState>,
    instance_id: String,
    category: String,
    file_name: String,
    enabled: bool,
) -> Result<(), String> {
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let dir = root.join(category_dir(&category)?);
    let current = resolve_file(&dir, &file_name)
        .ok_or_else(|| format!("'{file_name}' was not found in this instance."))?;

    let target = if enabled {
        safe_join(&dir, &file_name).map_err(|e| e.to_string())?
    } else {
        safe_join(&dir, &format!("{file_name}{DISABLED_SUFFIX}")).map_err(|e| e.to_string())?
    };
    if current != target {
        fs::rename(&current, &target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn remove_content_file(
    state: State<'_, AppState>,
    instance_id: String,
    category: String,
    file_name: String,
) -> Result<(), String> {
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let dir = root.join(category_dir(&category)?);
    let file = resolve_file(&dir, &file_name)
        .ok_or_else(|| format!("'{file_name}' was not found in this instance."))?;

    if file.is_dir() {
        fs::remove_dir_all(&file).map_err(|e| e.to_string())?;
    } else {
        fs::remove_file(&file).map_err(|e| e.to_string())?;
    }

    // Best-effort: drop any tracked mod row pointing at this file.
    if category == "mod" {
        let _ = state.db.delete_instance_mod_by_file(&instance_id, &file_name);
    }
    Ok(())
}
