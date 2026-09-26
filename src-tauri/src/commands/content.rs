//! Commands for viewing and managing the content files inside an instance —
//! mods, resource packs, and shader packs — directly on disk. This is the source
//! of truth (it also surfaces files a modpack dropped in that aren't tracked in
//! the database), so enable/disable/remove operate on the files themselves.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;

use crate::download::safe_join;
use crate::dto::instance::{
    ConfigFileEntry, ContentEntry, ContentMeta, InstanceContent, LaunchReadiness, MissingDep,
    ServerEntry, WorldEntry, WrongGameVersionFile, WrongLoaderFile,
};
use crate::dto::ModLoader;
use crate::instances::paths::instance_root;
use tauri::State;

use super::search::AppState;
use crate::instances::operations::acquire;

pub(crate) const DISABLED_SUFFIX: &str = ".disabled";

/// A mod's own declared display name, embedded icon, and modId, read from
/// its jar metadata in one pass. Best-effort: any missing/unreadable/
/// malformed metadata just leaves the field `None` (name falls back to the
/// filename-derived name on the frontend; icon falls back to a DB-recorded
/// one, if any; modId falls back to fuzzy name-based config matching).
struct ModMeta {
    name: Option<String>,
    icon: Option<String>,
    mod_id: Option<String>,
    launch: ModLaunchMeta,
    /// Mod ids nested inside this jar via Jar-in-Jar — the loader loads
    /// them as real mods, so they satisfy dependencies instance-wide.
    embedded_ids: Vec<String>,
    /// EVERY mod id this jar declares (a jar can ship several [[mods]]
    /// entries — CyclopsMC jars carry the main mod plus a `-compat`
    /// submodule that siblings depend on). First entry stays `mod_id`
    /// for display/tracking; all of them count as installed.
    all_mod_ids: Vec<String>,
}

fn push_mod_id(list: &mut Vec<String>, id: Option<&str>) {
    if let Some(id) = non_empty(id) {
        if !list.iter().any(|e| e == &id) {
            list.push(id);
        }
    }
}

/// Loader/Minecraft-version/dependency signals from the same jar metadata —
/// what the pre-launch readiness check uses to catch "NeoForge jar on a
/// Forge instance" and missing required deps before the game dies on them.
/// Raw range strings are kept as-is (never evaluated here — the loader
/// itself is the authority at runtime); only presence and loader family
/// are judged.
///
/// One entry per metadata file found: multiloader ("merged") jars ship
/// fabric + forge + neoforge metadata side by side and each loader reads
/// only its own, so judging the jar by the first file found false-flags it
/// everywhere else. The readiness check uses the entry matching the
/// instance's loader and ignores the rest.
#[derive(Debug, Default, Clone)]
pub(crate) struct ModLaunchMeta {
    pub loaders: Vec<LoaderMetaEntry>,
}

#[derive(Debug, Clone)]
pub(crate) struct LoaderMetaEntry {
    pub loader: String,
    pub mc_versions: Vec<String>,
    pub dependencies: Vec<ModDependency>,
}

impl ModLaunchMeta {
    fn entry_mut(&mut self, loader: &str) -> &mut LoaderMetaEntry {
        if !self.loaders.iter().any(|e| e.loader == loader) {
            self.loaders.push(LoaderMetaEntry {
                loader: loader.to_string(),
                mc_versions: Vec::new(),
                dependencies: Vec::new(),
            });
        }
        self.loaders.iter_mut().find(|e| e.loader == loader).expect("just inserted")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ModDependency {
    pub mod_id: String,
    pub version_range: Option<String>,
}

/// Entries that appear in dependency maps but are never installable mods:
/// the game itself, the loader, and the language runtimes.
fn is_pseudo_dependency(id: &str) -> bool {
    matches!(
        id.to_ascii_lowercase().as_str(),
        "minecraft" | "forge" | "neoforge" | "fabric" | "fabricloader" | "quilt" | "quilt_loader"
            | "java"
    )
}

/// Reads name + icon + modId from a jar's Fabric/Quilt, Forge/NeoForge, or
/// legacy Forge metadata. Opens the zip archive once and reuses it for both
/// lookups instead of the two separate full re-parses this used to do per
/// mod.
fn read_mod_metadata(jar_path: &Path) -> ModMeta {
    let mut meta = ModMeta { name: None, icon: None, mod_id: None, launch: ModLaunchMeta::default(), embedded_ids: Vec::new(), all_mod_ids: Vec::new() };
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
            meta.mod_id = non_empty(value.get("id").and_then(|v| v.as_str()));
            push_mod_id(&mut meta.all_mod_ids, value.get("id").and_then(|v| v.as_str()));
            let entry = meta.launch.entry_mut("fabric");
            // `depends` maps mod id -> range ("*" included). Only required
            // deps live here (breaks/conflicts are separate keys).
            if let Some(depends) = value.get("depends").and_then(|v| v.as_object()) {
                for (id, range) in depends {
                    if id == "minecraft" {
                        if let Some(r) = range.as_str() {
                            entry.mc_versions.push(r.to_string());
                        }
                    } else if !is_pseudo_dependency(id) {
                        entry.dependencies.push(ModDependency {
                            mod_id: id.clone(),
                            version_range: range.as_str().map(str::to_string),
                        });
                    }
                }
            }
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

    // Quilt (`quilt.mod.json`, schema 1): loader + `quilt_loader.depends`
    // entries of {id, versions} where versions is a string or an array.
    // Parsed whenever present (multiloader jars carry it next to fabric's).
    if let Some(contents) = read_zip_entry(&mut archive, "quilt.mod.json") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
            let loader_obj = value.get("quilt_loader");
            if loader_obj.is_some() {
                push_mod_id(
                    &mut meta.all_mod_ids,
                    loader_obj.and_then(|l| l.get("id")).and_then(|v| v.as_str()),
                );
                let entry = meta.launch.entry_mut("quilt");
                if let Some(depends) = loader_obj.and_then(|l| l.get("depends")).and_then(|v| v.as_array()) {
                    for dep in depends {
                        let Some(id) = dep.get("id").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        let range = match dep.get("versions") {
                            Some(serde_json::Value::String(r)) => Some(r.clone()),
                            Some(serde_json::Value::Array(arr)) => {
                                let joined = arr
                                    .iter()
                                    .filter_map(|v| v.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                if joined.is_empty() { None } else { Some(joined) }
                            }
                            _ => None,
                        };
                        if id == "minecraft" {
                            if let Some(r) = range {
                                entry.mc_versions.push(r);
                            }
                        } else if !is_pseudo_dependency(id) {
                            entry.dependencies.push(ModDependency {
                                mod_id: id.to_string(),
                                version_range: range,
                            });
                        }
                    }
                }
            }
        }
    }

    // Forge / NeoForge. NeoForge 1.20.5+ renamed this file to
    // `neoforge.mods.toml`; mods built only for modern NeoForge (the vast
    // majority of a current NeoForge pack) never ship the old `mods.toml` at
    // all, so both names need checking. Same schema either way — NeoForge is
    // a Forge fork — and neoforge.mods.toml takes priority when both exist,
    // same as the loader itself resolves it.
    //
    // Display fields stay first-wins (gated below), but launch metadata is
    // ALWAYS parsed per file: multiloader jars carry fabric + forge +
    // neoforge side by side, and skipping the tomls just because Fabric
    // already supplied a name is exactly what false-flagged them.
    for entry_name in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        let Some(contents) = read_zip_entry(&mut archive, entry_name) else {
            continue;
        };
        let Ok(value) = toml::from_str::<toml::Value>(&contents) else {
            continue;
        };
        let loader = if entry_name.contains("neoforge") { "neoforge" } else { "forge" };
        let entry = meta.launch.entry_mut(loader);
        // `[[dependencies.<modid>]]` entries carry the required-dep
        // graph the loader enforces at runtime. Only `mandatory` ones
        // (the default) count — optional deps missing is not a problem.
        if let Some(deps) = value.get("dependencies").and_then(|d| d.as_table()) {
            for (key, entries) in deps {
                let Some(list) = entries.as_array() else {
                    continue;
                };
                for dep in list {
                    let dep_id = dep
                        .get("modId")
                        .and_then(|v| v.as_str())
                        .unwrap_or(key.as_str());
                    let mandatory = match dep.get("type").and_then(|v| v.as_str()) {
                        // Newer files use type="required"|"optional"|... —
                        // it wins over `mandatory` when both are present.
                        Some(t) => t.eq_ignore_ascii_case("required"),
                        None => dep.get("mandatory").and_then(|v| v.as_bool()).unwrap_or(true),
                    };
                    if dep_id == "minecraft" {
                        if let Some(r) = dep.get("versionRange").and_then(|v| v.as_str()) {
                            if !entry.mc_versions.iter().any(|v| v == r) {
                                entry.mc_versions.push(r.to_string());
                            }
                        }
                    } else if mandatory && !is_pseudo_dependency(dep_id) {
                        let range = dep
                            .get("versionRange")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        if !entry.dependencies.iter().any(|d| d.mod_id == dep_id) {
                            entry.dependencies.push(ModDependency {
                                mod_id: dep_id.to_string(),
                                version_range: range,
                            });
                        }
                    }
                }
            }
        }
        if meta.name.is_some() && meta.icon.is_some() && meta.mod_id.is_some() {
            continue;
        }
        let first_mod = value.get("mods").and_then(|m| m.as_array()).and_then(|a| a.first());
        // Every [[mods]] entry provides a real mod id (main mod plus
        // -compat submodules); only the first feeds display/tracking.
        if let Some(mods) = value.get("mods").and_then(|m| m.as_array()) {
            for m in mods {
                push_mod_id(&mut meta.all_mod_ids, m.get("modId").and_then(|v| v.as_str()));
            }
        }
        if meta.name.is_none() {
            meta.name = non_empty(
                first_mod
                    .and_then(|m| m.get("displayName"))
                    .and_then(|v| v.as_str()),
            );
        }
        if meta.mod_id.is_none() {
            meta.mod_id =
                non_empty(first_mod.and_then(|m| m.get("modId")).and_then(|v| v.as_str()));
        }
        if meta.icon.is_none() {
                // `logoFile` is documented as a top-level key (applies to
                // the whole file), but some mods — notably ones generated by
                // Modrinth's packwiz/template tooling — declare it per-mod
                // inside the `[[mods]]` entry instead. Both are seen in the
                // wild, so check the per-mod entry as a fallback.
                let logo = value
                    .get("logoFile")
                    .and_then(|v| v.as_str())
                    .or_else(|| first_mod.and_then(|m| m.get("logoFile")).and_then(|v| v.as_str()));
                if let Some(logo) = non_empty(logo) {
                    // The toml comment calls this "root of the jar", but in
                    // practice a lot of build tooling (ForgeGradle/NeoGradle)
                    // packages the logo next to mods.toml itself instead —
                    // i.e. under META-INF/ — so a plain filename commonly
                    // only resolves there, not at the jar root.
                    meta.icon = read_zip_image_entry(&mut archive, &logo)
                        .or_else(|| read_zip_image_entry(&mut archive, &format!("META-INF/{logo}")));
                }
            }
        }

    // Legacy Forge (1.12 and earlier) — name only, no icon convention, and
    // no dependency graph worth reading. Only consulted when nothing else
    // identified the mod, same as the display fields.
    if meta.name.is_none() {
        if let Some(contents) = read_zip_entry(&mut archive, "mcmod.info") {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
                let list: Vec<&serde_json::Value> = match &value {
                    serde_json::Value::Array(arr) => arr.iter().collect(),
                    _ => value
                        .get("modList")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().collect())
                        .unwrap_or_default(),
                };
                for m in &list {
                    push_mod_id(&mut meta.all_mod_ids, m.get("modid").and_then(|v| v.as_str()));
                }
                let first = list.first().copied();
                meta.name = non_empty(first.and_then(|m| m.get("name")).and_then(|v| v.as_str()));
                if meta.name.is_some() && meta.launch.loaders.is_empty() {
                    meta.launch.entry_mut("forge");
                }
            }
        }
    }

    meta.embedded_ids = embedded_jar_mod_ids(&mut archive);

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

/// Mod ids embedded via NeoForge's Jar-in-Jar (`META-INF/jarjar/
/// metadata.json` + nested jars): the loader adds every nested jar to the
/// mod collection, so their ids satisfy `[[dependencies]]` exactly like
/// top-level jars do. Without this, Create's flywheel/ponder, EnderIO's
/// endercore, AE2WTLib's api and friends all false-flag as missing.
fn embedded_jar_mod_ids<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Vec<String> {
    let mut ids = Vec::new();
    let Ok(mut meta_entry) = archive.by_name("META-INF/jarjar/metadata.json") else {
        return ids;
    };
    if meta_entry.size() > 1024 * 1024 {
        return ids;
    }
    let mut raw = String::new();
    if meta_entry.read_to_string(&mut raw).is_err() {
        return ids;
    }
    drop(meta_entry);
    let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return ids;
    };
    let Some(jars) = meta.get("jars").and_then(|j| j.as_array()) else {
        return ids;
    };
    for jar in jars {
        let Some(path) = jar.get("path").and_then(|p| p.as_str()) else {
            continue;
        };
        let Ok(mut nested_entry) = archive.by_name(path) else {
            continue;
        };
        if nested_entry.size() > 64 * 1024 * 1024 {
            continue;
        }
        let mut bytes = Vec::new();
        if nested_entry.read_to_end(&mut bytes).is_err() {
            continue;
        }
        drop(nested_entry);
        let Ok(mut nested) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
            continue;
        };
        if let Some(id) = first_mod_id(&mut nested) {
            ids.push(id);
        }
    }
    ids
}

/// First mod id out of a jar's metadata, NeoForge-first like the loader
/// resolves it. Display parsing has its own richer version; this is just
/// for dependency-satisfaction bookkeeping.
fn first_mod_id<R: std::io::Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>) -> Option<String> {
    for entry_name in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        if let Some(contents) = read_zip_entry(archive, entry_name) {
            if let Ok(value) = toml::from_str::<toml::Value>(&contents) {
                if let Some(id) = value
                    .get("mods")
                    .and_then(|m| m.as_array())
                    .and_then(|a| a.first())
                    .and_then(|m| m.get("modId"))
                    .and_then(|v| v.as_str())
                {
                    return non_empty(Some(id));
                }
            }
        }
    }
    if let Some(contents) = read_zip_entry(archive, "fabric.mod.json") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
            if let Some(id) = non_empty(value.get("id").and_then(|v| v.as_str())) {
                return Some(id);
            }
        }
    }
    None
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

/// A directory entry before any metadata-cache join — just what a plain
/// `read_dir` + `metadata()` call gives us. `mtime_unix` never leaves the
/// backend (not part of `ContentEntry`'s wire shape); it exists purely to
/// fingerprint the file against the cache below.
struct ScannedFile {
    file_name: String,
    enabled: bool,
    size_bytes: u64,
    mtime_unix: i64,
}

/// Lists files in a content directory with no jar/zip parsing at all — just
/// names, sizes, and mtimes off the filesystem, so this is effectively
/// instant even for an instance with hundreds of mods. Only files with
/// `expected_ext` (the extension the loader itself actually reads from this
/// folder — `.jar` for mods, `.zip` for resource/shader packs) are listed;
/// a stray readme or changelog a shader/resource pack's own CurseForge page
/// bundles alongside the real archive is never something Forge/Iris loads,
/// so it's clutter, not content.
fn scan_dir(dir: &Path, expected_ext: &str) -> Vec<ScannedFile> {
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
        // A resource/shader pack can be an unzipped folder — that's still
        // real content, just not archived — so only files (never
        // directories) are held to the extension check.
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir && !file_name.to_ascii_lowercase().ends_with(expected_ext) {
            continue;
        }
        let metadata = entry.metadata().ok();
        let size_bytes = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime_unix = metadata
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(ScannedFile { file_name, enabled, size_bytes, mtime_unix });
    }
    out.sort_by(|a, b| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()));
    out
}

/// Joins a directory's scanned files against the instance's metadata cache —
/// a file whose current size+mtime match a cached row gets its name/icon
/// filled in right here, with zero jar/zip parsing, and is marked
/// `meta_resolved` so the frontend never fetches it again. A file that's new
/// or has changed since it was last cached is left unresolved for the
/// existing per-row lazy fetch to pick up (which then populates the cache
/// for next time).
///
/// `config_top_entries`/`db_names` are `Some` only for the "mod" category —
/// resource/shader packs have no per-file config convention to match
/// against. Both are computed once per `list_instance_content` call and
/// shared across every row, not fetched per-row, the same reasoning as the
/// metadata cache itself.
fn apply_cache(
    files: Vec<ScannedFile>,
    category: &str,
    cache: &std::collections::HashMap<(String, String), crate::db::CachedContentMeta>,
    config_top_entries: Option<&[ConfigTopEntry]>,
    db_names: &std::collections::HashMap<String, String>,
    origins: &std::collections::HashMap<String, bool>,
) -> Vec<ContentEntry> {
    files
        .into_iter()
        .map(|f| {
            let hit = cache
                .get(&(category.to_string(), f.file_name.clone()))
                .filter(|c| c.size_bytes == f.size_bytes && c.mtime_unix == f.mtime_unix);
            let name = hit.and_then(|c| c.name.clone());
            let mod_id_norm = hit.and_then(|c| c.mod_id.as_deref()).map(normalize_for_match);
            let has_config = config_top_entries.is_some_and(|entries| {
                let mut terms = vec![normalize_for_match(&file_stem(&f.file_name))];
                if let Some(n) = &name {
                    terms.push(normalize_for_match(n));
                }
                if let Some(n) = db_names.get(&f.file_name) {
                    terms.push(normalize_for_match(n));
                }
                entries.iter().any(|e| {
                    let mod_id_match = mod_id_norm.as_deref().is_some_and(|mid| {
                        e.normalized == mid || strip_config_side_suffix(&e.normalized) == Some(mid)
                    });
                    (mod_id_match || config_entry_matches(&e.normalized, &terms))
                        && (e.is_dir || is_text_config_file(&e.raw_name))
                })
            });
            ContentEntry {
                file_name: f.file_name.clone(),
                name,
                icon: hit.and_then(|c| c.icon.clone()),
                enabled: f.enabled,
                size_bytes: f.size_bytes,
                meta_resolved: hit.is_some(),
                has_config,
                // Tracked rows carry the verdict; untracked files fall back
                // to the sidecar check done by the caller; anything left
                // over is user-added by elimination. Both spellings cover
                // `.disabled`-suffixed rows, whose display name is stripped.
                added_by_you: origins
                    .get(&f.file_name)
                    .or_else(|| {
                        f.file_name
                            .strip_suffix(DISABLED_SUFFIX)
                            .and_then(|base| origins.get(base))
                    })
                    .copied()
                    .unwrap_or(true),
            }
        })
        .collect()
}

fn file_stem(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| file_name.to_string())
}

/// Extensions plain-text enough to open in the in-app config editor.
/// Deliberately excludes binaries (`.png`, `.zip`, `.jar`) even though some
/// mods bundle a resourcepack or preview image right alongside their real
/// config in the same folder.
const TEXT_CONFIG_EXTENSIONS: &[&str] = &[
    "toml", "json", "json5", "yaml", "yml", "cfg", "conf", "ini", "properties", "txt", "snbt",
    "lang", "mcmeta", "omniconf",
];

fn is_text_config_file(file_name: &str) -> bool {
    Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| TEXT_CONFIG_EXTENSIONS.iter().any(|ext| ext.eq_ignore_ascii_case(e)))
}

/// Strips everything but ASCII letters/digits and lowercases — the common
/// ground between a mod's declared name ("Nature's Compass"), its jar
/// filename ("NaturesCompass-1.21.1-3.0.3-neoforge.jar"), and its config
/// entry ("naturescompass.toml" / "naturescompass/"), each using completely
/// different punctuation, casing, and versioning for the same mod.
fn normalize_for_match(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether a config entry's normalized name plausibly belongs to a mod
/// identified by `terms` (its resolved name and/or jar filename, each
/// already normalized). Matches on substring containment either direction —
/// a mod's declared name is often a superset or subset of its actual modid
/// (e.g. "Nature's Compass" vs config folder "naturescompass") — gated by a
/// minimum length so a short common substring doesn't false-positive across
/// unrelated mods.
///
/// Forge/NeoForge's per-side config convention names files `<modid>-common`,
/// `<modid>-client`, `<modid>-server` — none of which is a substring of a
/// mod's declared name whenever that name has an extra word the modid
/// doesn't (Curios API's modid is `curios`, so `curios-common.toml` shares no
/// full-containment relationship with "curiosapi"). Stripping a trailing
/// side name off the entry recovers the bare modid — but only checked as a
/// *prefix* of a term, not a substring anywhere in it: a plain-containment
/// check let the stripped "curios" match unrelated "Apothic Curios" (which
/// merely ends with that word) and stripped "forge" match every single
/// NeoForge-suffixed mod's jar filename (".../neoforge-...").
const CONFIG_SIDE_SUFFIXES: &[&str] = &["common", "client", "server"];

fn strip_config_side_suffix(entry_norm: &str) -> Option<&str> {
    CONFIG_SIDE_SUFFIXES
        .iter()
        .find_map(|suffix| entry_norm.strip_suffix(suffix))
        .filter(|stripped| !stripped.is_empty())
}

/// A bare top-level config entry named exactly after the modloader itself
/// (a `config/fabric/` folder — Fabric API's own per-module configs like
/// `indigo-renderer.properties` — or a stray `forge`/`neoforge`/`quilt`
/// entry) is either loader-owned or too generic to attribute to any single
/// mod. Almost every mod built for a given loader embeds that loader's name
/// in its own jar filename (`ftbessentials-fabric-1.20.1.jar`), so without
/// this, `full_match`'s plain substring containment made *every* Fabric mod
/// fuzzy-match the loader's own "fabric" folder. A real mod's modId is never
/// literally one of these, so they're only ever matched via the exact modId
/// check elsewhere, never fuzzily here.
const GENERIC_RESERVED_NAMES: &[&str] =
    &["fabric", "quilt", "forge", "neoforge", "common", "client", "server"];

fn config_entry_matches(entry_norm: &str, terms: &[String]) -> bool {
    const MIN_MATCH_LEN: usize = 4;
    if entry_norm.is_empty() || GENERIC_RESERVED_NAMES.contains(&entry_norm) {
        return false;
    }

    let full_match = terms.iter().any(|term| {
        if term.is_empty() {
            return false;
        }
        if term.len() < MIN_MATCH_LEN || entry_norm.len() < MIN_MATCH_LEN {
            return term == entry_norm;
        }
        entry_norm.contains(term.as_str()) || term.contains(entry_norm)
    });
    if full_match {
        return true;
    }

    let Some(stripped) = strip_config_side_suffix(entry_norm) else {
        return false;
    };
    if stripped.len() < MIN_MATCH_LEN {
        return false;
    }
    terms.iter().any(|term| term.starts_with(stripped))
}

/// One top-level entry directly inside `config/`, fingerprinted for
/// matching once per `list_instance_content`/`list_mod_configs` call instead
/// of re-reading the directory for every mod.
struct ConfigTopEntry {
    raw_name: String,
    is_dir: bool,
    normalized: String,
}

fn scan_config_top_level(config_dir: &Path) -> Vec<ConfigTopEntry> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(config_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let raw_name = entry.file_name().to_string_lossy().to_string();
        if raw_name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        // A directory's whole name is its identity; a file's is its name
        // minus extension — config files essentially never carry version
        // numbers the way mod jars do, so no further stripping is needed.
        let stem = if is_dir { raw_name.clone() } else { file_stem(&raw_name) };
        out.push(ConfigTopEntry { raw_name, is_dir, normalized: normalize_for_match(&stem) });
    }
    out
}

/// Recursively collects every text-editable config file under `dir`,
/// building `relative_prefix`-prefixed paths relative to `config/` itself —
/// what the frontend passes back to `read_config_file`/`write_config_file`.
fn collect_text_configs_recursive(dir: &Path, relative_prefix: &str, out: &mut Vec<ConfigFileEntry>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let relative = format!("{relative_prefix}/{name}");
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_text_configs_recursive(&entry.path(), &relative, out);
        } else if is_text_config_file(&name) {
            out.push(ConfigFileEntry { relative_path: relative.clone(), display_name: relative });
        }
    }
}

#[tauri::command]
pub async fn list_instance_content(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<InstanceContent, String> {
    let t0 = std::time::Instant::now();
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let cache_rows = state.db.get_content_meta_cache(&instance_id).unwrap_or_default();
    let cache: std::collections::HashMap<(String, String), _> = cache_rows
        .into_iter()
        .map(|c| ((c.category.clone(), c.file_name.clone()), c))
        .collect();
    // For the "has this mod got a config?" check below — a mod's tracked
    // display name is a better match term than a not-yet-cache-resolved
    // row's filename alone, and this is one query for the whole instance,
    // not one per mod.
    let tracked = state.db.list_instance_mods(&instance_id).unwrap_or_default();
    let db_names: std::collections::HashMap<String, String> = tracked
        .iter()
        .map(|m| (m.file_name.clone(), m.mod_name.clone()))
        .collect();
    // Origin by filename for the "added by you" marking: tracked rows carry
    // it directly; untracked files fall back to the pack sidecars (a
    // watcher-placed manual download has no row yet but is still pack).
    // Anything else reads as user-added — no pack claims it.
    let mut origins: std::collections::HashMap<String, bool> = tracked
        .iter()
        .map(|m| (m.file_name.clone(), m.origin == crate::dto::ModOrigin::User))
        .collect();
    for name in crate::db::pack_filenames(&root) {
        origins.entry(name).or_insert(false);
    }

    // Directory listing + cache join — no jar/zip parsing for anything
    // already seen before — so this stays fast no matter how big the pack
    // is. Still off the async runtime since it's real disk + DB I/O.
    let result = tauri::async_runtime::spawn_blocking(move || {
        let config_top_entries = scan_config_top_level(&root.join("config"));
        InstanceContent {
            mods: apply_cache(
                scan_dir(&root.join("mods"), ".jar"),
                "mod",
                &cache,
                Some(&config_top_entries),
                &db_names,
                &origins,
            ),
            resource_packs: apply_cache(
                scan_dir(&root.join("resourcepacks"), ".zip"),
                "resourcepack",
                &cache,
                None,
                &db_names,
                &origins,
            ),
            shader_packs: apply_cache(
                scan_dir(&root.join("shaderpacks"), ".zip"),
                "shaderpack",
                &cache,
                None,
                &db_names,
                &origins,
            ),
        }
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

/// One enabled jar's launch-relevant metadata for readiness assessment.
#[derive(Debug, Clone)]
pub(crate) struct FileLaunchMeta {
    pub file_name: String,
    pub mod_name: Option<String>,
    pub mod_id: Option<String>,
    /// Every mod id the jar declares (main + submodules) — all count as
    /// installed for dependency satisfaction.
    pub all_mod_ids: Vec<String>,
    pub launch: ModLaunchMeta,
}

/// Judges parsed jar metadata against the instance's loader and game
/// version: loader-family mismatches, game-version ranges the instance
/// falls outside of, plus required dependency ids no installed jar
/// provides.
/// Only the metadata entry matching the instance's loader counts — a
/// multiloader jar's Fabric deps are meaningless on a NeoForge instance
/// (and vice versa). A jar with no matching entry is flagged wrong-loader
/// and its deps are skipped to avoid piling noise on the real problem.
/// File names are never consulted for versions (mod versions share the MC
/// namespace). A jar whose entry carries no usable MC range stays silent
/// on game version.
/// Pure (no I/O) so the rules are unit-testable without instance fixtures.
pub(crate) fn assess_readiness(
    instance_loader: ModLoader,
    instance_mc_version: &str,
    files: &[FileLaunchMeta],
    embedded_ids: &[String],
) -> LaunchReadiness {
    let instance_loader_str = match instance_loader {
        ModLoader::Fabric => "fabric",
        ModLoader::Forge => "forge",
        ModLoader::NeoForge => "neoforge",
        ModLoader::Quilt => "quilt",
        ModLoader::Vanilla => "vanilla",
    };
    let mut installed_ids: std::collections::HashSet<String> = files
        .iter()
        .flat_map(|f| {
            f.mod_id
                .iter()
                .map(|id| id.to_ascii_lowercase())
                .chain(f.all_mod_ids.iter().map(|id| id.to_ascii_lowercase()))
        })
        .collect();
    installed_ids.extend(embedded_ids.iter().map(|id| id.to_ascii_lowercase()));
    let mut wrong_loader = Vec::new();
    let mut missing_deps = Vec::new();
    let mut wrong_game_version = Vec::new();
    for file in files {
        let matching = file
            .launch
            .loaders
            .iter()
            .find(|e| e.loader == instance_loader_str);
        let Some(entry) = matching else {
            if !file.launch.loaders.is_empty() {
                let detected = file
                    .launch
                    .loaders
                    .iter()
                    .map(|e| e.loader.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                wrong_loader.push(WrongLoaderFile {
                    file_name: file.file_name.clone(),
                    mod_name: file.mod_name.clone(),
                    detected_loader: detected,
                });
            }
            continue;
        };
        for dep in &entry.dependencies {
            if !installed_ids.contains(&dep.mod_id.to_ascii_lowercase()) {
                missing_deps.push(MissingDep {
                    file_name: file.file_name.clone(),
                    mod_name: file.mod_name.clone(),
                    dep_mod_id: dep.mod_id.clone(),
                    version_range: dep.version_range.clone(),
                });
            }
        }
        // Game version: the matching loader entry's metadata ranges are
        // the only signal. File names are deliberately NOT consulted —
        // mod versions share the MC namespace (`alexsmobs-1.22.9.jar` is
        // Alex's Mobs' own 1.22.9 built for MC 1.20.1), so name-based
        // flagging cried wolf on working packs. A jar with no usable range
        // stays silent; the install-time validation guards that class.
        let shaped: Vec<&String> = entry
            .mc_versions
            .iter()
            .filter(|r| range_mentions_mc_version(r))
            .collect();
        let declared = if shaped.is_empty()
            || shaped
                .iter()
                .any(|r| mc_range_matches(r, instance_mc_version))
        {
            None
        } else {
            Some(entry.mc_versions.join(", "))
        };
        if let Some(declared) = declared {
            wrong_game_version.push(WrongGameVersionFile {
                file_name: file.file_name.clone(),
                mod_name: file.mod_name.clone(),
                declared,
                expected: instance_mc_version.to_string(),
            });
        }
    }
    LaunchReadiness {
        checked_files: files.len() as u32,
        wrong_loader,
        missing_deps,
        wrong_game_version,
    }
}

/// Numeric triple for game-version ordering across both Mojang schemes:
/// legacy `1.x.y` and year-based `26.x`. Non-numeric tails (snapshots like
/// `26.3-snapshot-5`) parse by leading digits; missing parts are zero.
fn mc_triplet(version: &str) -> (u32, u32, u32) {
    let mut parts = version.split('.');
    let num = |p: Option<&str>| {
        p.unwrap_or("")
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u32>()
            .unwrap_or(0)
    };
    (num(parts.next()), num(parts.next()), num(parts.next()))
}

/// Exact-or-line equality: only the major.minor line must match — Mojang
/// keeps patches wire-compatible, so a jar declaring 1.20 (or 1.20.0)
/// runs on 1.20.1, while 1.20.x on 1.21.y genuinely breaks.
fn mc_exact_matches(declared: &str, mc: (u32, u32, u32)) -> bool {
    let base = declared.trim().trim_end_matches(".x").trim_end_matches('.');
    // No digits at all ("*"-adjacent garbage): unparseable, passes.
    if !base.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    let t = mc_triplet(base);
    t.0 == mc.0 && t.1 == mc.1
}

/// Whether an instance game version is accepted by one minecraft dependency
/// range from jar metadata. Covers the shapes mods actually ship: "*"
/// (anything), exact versions ("1.20.1"), maven intervals ("[1.20,1.21)"),
/// Fabric/npm comparators (">=1.20", ">1.20 <1.21", "1.20.x") and "||"
/// unions. Anything unparseable matches — readiness is advisory and must
/// not cry wolf on exotic ranges.
///
/// Comparison is at major.minor LINE granularity with inclusive bounds:
/// patches stay wire-compatible, and — decisively — the game loader itself
/// accepts the MDK-boilerplate `[1.21,1.21.1)` on 1.21.1 (NeoForge's own
/// loading screen named only the truly wrong file when a 1.20.1 jar sat
/// next to forty such mods). A checker stricter than the loader is just
/// another wolf cry.
fn mc_range_matches(range: &str, mc_version: &str) -> bool {
    let mc = mc_triplet(mc_version);
    let mc_line = (mc.0, mc.1);
    // Maven multi-ranges (`[1.21],[1.21.1]` — an exact-pin union) split on
    // commas that are NOT inside brackets; interval commas are handled
    // by the branch logic below. An empty range matches (no constraint).
    let parts = split_top_level(range);
    if parts.is_empty() {
        return true;
    }
    parts.into_iter().any(|part| {
        part.split("||").any(|branch| {
            let branch = branch.trim();
        if branch.is_empty() || branch == "*" {
            return true;
        }
        // Maven interval: bounds name LINES, and both ends count as
        // inclusive no matter the bracket flavor — see the doc comment.
        if (branch.starts_with('[') || branch.starts_with('('))
            && (branch.ends_with(']') || branch.ends_with(')'))
        {
            let inner = &branch[1..branch.len() - 1];
            // No comma: an exact pin (`[1.20]`), matched by line.
            if !inner.contains(',') {
                return mc_exact_matches(inner, mc);
            }
            let mut bounds = inner.splitn(2, ',');
            let lo = bounds.next().unwrap_or("").trim();
            let hi = bounds.next().unwrap_or("").trim();
            // Digitless bounds ("(,)") are open-ended, like empty ones.
            if !lo.is_empty() && lo.chars().any(|c| c.is_ascii_digit()) {
                let t = mc_triplet(lo);
                if mc_line < (t.0, t.1) {
                    return false;
                }
            }
            if !hi.is_empty() && hi.chars().any(|c| c.is_ascii_digit()) {
                let t = mc_triplet(hi);
                if mc_line > (t.0, t.1) {
                    return false;
                }
            }
            return true;
        }
        // Space/comma-separated AND of comparator atoms. Comparators keep
        // full-triplet strictness (`<1.21.2` on 1.21.1 passes, on 1.21.2
        // fails) — only intervals and bare versions compare by line, since
        // only those are written line-wise by authors and tooling.
        branch
            .split(|c| c == ' ' || c == ',')
            .filter(|p| !p.is_empty())
            .all(|atom| {
                let (op, ver) = if let Some(v) = atom.strip_prefix(">=") {
                    (">=", v)
                } else if let Some(v) = atom.strip_prefix("<=") {
                    ("<=", v)
                } else if let Some(v) = atom.strip_prefix('>') {
                    (">", v)
                } else if let Some(v) = atom.strip_prefix('<') {
                    ("<", v)
                } else if let Some(v) = atom.strip_prefix('=') {
                    ("=", v)
                } else if let Some(v) = atom.strip_prefix('~') {
                    // npm `~1.20` (patch-level): >=1.20.0.
                    (">=", v)
                } else if let Some(v) = atom.strip_prefix('^') {
                    // npm `^1.20` (compatible-within-line): >=1.20.0.
                    (">=", v)
                } else {
                    ("=", atom)
                };
                let ver = ver.trim();
                if ver.is_empty() || ver == "*" {
                    return true;
                }
                // Digitless comparator bounds are unparseable: pass.
                if !ver.chars().any(|c| c.is_ascii_digit()) {
                    return true;
                }
                let t = mc_triplet(ver.trim_end_matches(".x").trim_end_matches('.'));
                match op {
                    ">=" => mc >= t,
                    "<=" => mc <= t,
                    ">" => mc > t,
                    "<" => mc < t,
                    _ => mc_exact_matches(ver, mc),
                }
            })
        })
    })
}

/// Splits a range on commas outside any brackets: maven multi-ranges like
/// `[1.21],[1.21.1]` are unions, while the comma inside `[1.20,1.21)` is
/// the interval separator the branch logic consumes.
fn split_top_level(range: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in range.char_indices() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(range[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(range[start..].trim());
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

/// Whether a range string mentions any Minecraft-shaped version (a version
/// part starting with 1 or 26). Jar metadata occasionally carries non-MC
/// junk in version fields; without this, such junk would evaluate against
/// the instance version and cry wolf.
fn range_mentions_mc_version(range: &str) -> bool {
    range
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter(|p| !p.is_empty())
        .any(|p| {
            let mut nums = p.split('.');
            matches!(nums.next(), Some("1") | Some("26"))
        })
}

/// Pre-launch readiness check: reads every ENABLED jar's own metadata and
/// reports loader-family mismatches plus required deps no installed jar
/// provides. Disabled (`.disabled`) files don't load, so they're skipped.
/// Advisory only — empty lists are not a guarantee the game will start.
#[tauri::command]
pub async fn check_launch_readiness(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<LaunchReadiness, String> {
    let instance = state
        .db
        .get_instance(&instance_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Instance not found.".to_string())?;
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let files = tauri::async_runtime::spawn_blocking(move || {
        let mut out = Vec::new();
        let mut embedded_ids = Vec::new();
        let Ok(entries) = std::fs::read_dir(root.join("mods")) else {
            return (out, embedded_ids);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Same `.jar`-only convention as the mod count: `.disabled`
            // files don't load, so they can't break (or fix) a launch.
            if !name.ends_with(".jar") {
                continue;
            }
            let meta = read_mod_metadata(&path);
            embedded_ids.extend(meta.embedded_ids.clone());
            out.push(FileLaunchMeta {
                file_name: name,
                mod_name: meta.name,
                mod_id: meta.mod_id,
                all_mod_ids: meta.all_mod_ids,
                launch: meta.launch,
            });
        }
        (out, embedded_ids)
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(assess_readiness(instance.loader, &instance.minecraft_version, &files.0, &files.1))
}

/// Lists singleplayer worlds (`saves/*/level.dat`) oldest detail omitted:
/// name, last played, game mode, game version, icon. A corrupt or
/// unreadable level.dat degrades to its folder name rather than failing
/// the whole list — one broken world must not hide the rest.
#[tauri::command]
pub async fn list_instance_worlds(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<Vec<WorldEntry>, String> {
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let _ = state.db.get_instance(&instance_id).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(root.join("saves")) else {
            return out;
        };
        let mut folders: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        folders.sort();
        for folder in folders {
            let folder_name = folder
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push(read_world_entry(&folder, &folder_name));
        }
        // Most recently played first; undated (or corrupt) worlds sink.
        out.sort_by_key(|w| std::cmp::Reverse(w.last_played_ms.unwrap_or(-1)));
        out
    })
    .await
    .map_err(|e| e.to_string())
}

/// Lists saved multiplayer servers (`servers.dat`). No pinging, no
/// editing — a read-only view. A missing file is an empty list, not an
/// error (fresh instances have none).
#[tauri::command]
pub async fn list_instance_servers(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<Vec<ServerEntry>, String> {
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let _ = state.db.get_instance(&instance_id).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || read_servers_file(&root.join("servers.dat")))
        .await
        .map_err(|e| e.to_string())
}

fn read_world_entry(folder: &std::path::Path, folder_name: &str) -> WorldEntry {
    let mut entry = WorldEntry {
        folder_name: folder_name.to_string(),
        name: None,
        last_played_ms: None,
        game_mode: None,
        game_version: None,
        icon: None,
    };
    if let Ok(icon_bytes) = std::fs::read(folder.join("icon.png")) {
        if !icon_bytes.is_empty() {
            entry.icon = Some(format!("data:image/png;base64,{}", base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                icon_bytes
            )));
        }
    }
    let Ok(bytes) = std::fs::read(folder.join("level.dat")) else {
        return entry;
    };
    let Some(data) = nbt_root_data(&bytes).and_then(|root| match root.get("Data") {
        Some(valence_nbt::Value::Compound(data)) => Some(data.clone()),
        _ => None,
    }) else {
        return entry;
    };
    entry.name = data.get("LevelName").and_then(|v| match v {
        valence_nbt::Value::String(s) => Some(s.clone()),
        _ => None,
    });
    entry.last_played_ms = data.get("LastPlayed").and_then(|v| v.as_i64());
    entry.game_mode = data.get("GameType").and_then(|v| v.as_i32()).map(|mode| match mode {
        0 => "Survival".to_string(),
        1 => "Creative".to_string(),
        2 => "Adventure".to_string(),
        3 => "Spectator".to_string(),
        _ => format!("Mode {mode}"),
    });
    entry.game_version = data.get("Version").and_then(|v| match v {
        valence_nbt::Value::Compound(ver) => ver.get("Name").and_then(|n| match n {
            valence_nbt::Value::String(s) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    });
    entry
}

fn read_servers_file(path: &std::path::Path) -> Vec<ServerEntry> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    // Vanilla writes servers.dat uncompressed; tolerate a gzipped one.
    let root = nbt_root_data(&bytes);
    let Some(list) = root.as_ref().and_then(|r| r.get("servers")) else {
        return Vec::new();
    };
    let valence_nbt::Value::List(valence_nbt::List::Compound(entries)) = list else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|e| {
            let name = e.get("name").and_then(|v| match v {
                valence_nbt::Value::String(s) => Some(s.clone()),
                _ => None,
            })?;
            let address = e.get("ip").and_then(|v| match v {
                valence_nbt::Value::String(s) => Some(s.clone()),
                _ => None,
            })?;
            let icon = e.get("icon").and_then(|v| match v {
                valence_nbt::Value::String(s) if !s.is_empty() => {
                    Some(format!("data:image/png;base64,{s}"))
                }
                _ => None,
            });
            Some(ServerEntry { name, address, icon })
        })
        .collect()
}

/// Decodes NBT bytes that may be gzipped (level.dat always is) or raw
/// (servers.dat is). Returns the root compound.
fn nbt_root_data(bytes: &[u8]) -> Option<valence_nbt::Compound> {
    use std::io::Read;
    let raw: Vec<u8> = if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut decoder = flate2::read::GzDecoder::new(bytes);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).ok()?;
        out
    } else {
        bytes.to_vec()
    };
    valence_nbt::from_binary(&mut raw.as_slice()).ok().map(|(root, _)| root)
}

/// Resolves one file's display name + icon by opening just that jar/zip —
// the frontend calls this per row as it scrolls into view, instead of the
// whole instance paying for every file's metadata up front.
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
    // fallback when the jar doesn't embed its own icon. A single-row lookup
    // rather than `list_instance_mods` — this runs once per row as the
    // Content tab scrolls, and re-fetching the whole instance's mod table
    // for every single row was O(n) work repeated n times.
    let db_icon = if category == "mod" {
        state
            .db
            .get_instance_mod_icon_by_file(&instance_id, &file_name)
            .unwrap_or(None)
    } else {
        None
    };

    // Fingerprinted before the parse so the cache write below lines up with
    // exactly the bytes that were just read — a file that changes between
    // this stat and the parse just means one stale cache write, corrected
    // the next time this file's row is fetched.
    let fingerprint = fs::metadata(&path).ok().map(|m| {
        let mtime_unix = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        (m.len(), mtime_unix)
    });
    let category_for_cache = category.clone();
    let file_name_for_cache = file_name.clone();

    let (meta, mod_id) = tauri::async_runtime::spawn_blocking(move || match category.as_str() {
        "mod" if is_jar => {
            let meta = read_mod_metadata(&path);
            (
                ContentMeta {
                    name: meta.name,
                    icon: meta.icon.or(db_icon),
                },
                meta.mod_id,
            )
        }
        "resourcepack" if is_zip => (
            ContentMeta {
                name: None,
                icon: read_resourcepack_icon(&path),
            },
            None,
        ),
        _ => (ContentMeta::default(), None),
    })
    .await
    .map_err(|e| e.to_string())?;

    // Cached so the next time this instance's Content tab opens, this exact
    // file (unchanged) needs no jar/zip parsing at all — including when
    // parsing found nothing, so a jar with no embedded icon isn't reopened
    // forever looking for one it doesn't have.
    if let Some((size_bytes, mtime_unix)) = fingerprint {
        let _ = state.db.upsert_content_meta_cache(
            &instance_id,
            &category_for_cache,
            &file_name_for_cache,
            size_bytes,
            mtime_unix,
            meta.name.as_deref(),
            meta.icon.as_deref(),
            mod_id.as_deref(),
        );
    }

    Ok(meta)
}

#[tauri::command]
pub fn set_content_enabled(
    _state: State<'_, AppState>,
    instance_id: String,
    category: String,
    file_name: String,
    enabled: bool,
) -> Result<(), String> {
    let _operation = acquire(&instance_id)?;
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
    let _operation = acquire(&instance_id)?;

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

/// Every editable config file that plausibly belongs to one mod — a single
/// top-level match returns just that file; a matched folder returns every
/// text-config file inside it (recursively), flattened with paths relative
/// to `config/`. Empty (not an error) when nothing matches, which the
/// frontend reads as "no Config button for this mod".
#[tauri::command]
pub fn list_mod_configs(
    state: State<'_, AppState>,
    instance_id: String,
    file_name: String,
) -> Result<Vec<ConfigFileEntry>, String> {
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let config_dir = root.join("config");

    let mut terms = vec![normalize_for_match(&file_stem(&file_name))];
    if let Ok(mods) = state.db.list_instance_mods(&instance_id) {
        if let Some(m) = mods.into_iter().find(|m| m.file_name == file_name) {
            terms.push(normalize_for_match(&m.mod_name));
        }
    }
    if let Ok(Some(name)) = state.db.get_content_meta_name_by_file(&instance_id, &file_name) {
        terms.push(normalize_for_match(&name));
    }
    // The loader's own per-side config system names files after exactly this
    // string by default (see `strip_config_side_suffix`'s doc comment) — an
    // exact match against it is precise where the name-based `terms` above
    // are only ever a plausibility guess.
    let mod_id_norm = state
        .db
        .get_content_meta_mod_id_by_file(&instance_id, &file_name)
        .ok()
        .flatten()
        .map(|id| normalize_for_match(&id));

    let mut results = Vec::new();
    for entry in scan_config_top_level(&config_dir) {
        let mod_id_match = mod_id_norm.as_deref().is_some_and(|mid| {
            entry.normalized == mid || strip_config_side_suffix(&entry.normalized) == Some(mid)
        });
        if !mod_id_match && !config_entry_matches(&entry.normalized, &terms) {
            continue;
        }
        let entry_path = config_dir.join(&entry.raw_name);
        if entry.is_dir {
            collect_text_configs_recursive(&entry_path, &entry.raw_name, &mut results);
        } else if is_text_config_file(&entry.raw_name) {
            results.push(ConfigFileEntry {
                relative_path: entry.raw_name.clone(),
                display_name: entry.raw_name,
            });
        }
    }
    results.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    Ok(results)
}

/// Config files are typically a few KB; this is generous headroom for a
/// verbose one while still refusing anything a plain textarea shouldn't
/// try to hold in memory and diff on every keystroke.
const MAX_EDITABLE_CONFIG_BYTES: u64 = 2 * 1024 * 1024;

#[tauri::command]
pub fn read_config_file(instance_id: String, relative_path: String) -> Result<String, String> {
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let path = safe_join(&root.join("config"), &relative_path).map_err(|e| e.to_string())?;
    let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
    if metadata.len() > MAX_EDITABLE_CONFIG_BYTES {
        return Err(format!(
            "This file is {:.1} MB — too large to edit in-app. Use \"Open folder\" and a text editor instead.",
            metadata.len() as f64 / (1024.0 * 1024.0)
        ));
    }
    fs::read_to_string(&path)
        .map_err(|_| "Couldn't read this file as text — it may not be a plain-text config.".to_string())
}

#[tauri::command]
pub fn write_config_file(
    instance_id: String,
    relative_path: String,
    contents: String,
) -> Result<(), String> {
    let _operation = acquire(&instance_id)?;
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let path = safe_join(&root.join("config"), &relative_path).map_err(|e| e.to_string())?;
    fs::write(&path, contents).map_err(|e| e.to_string())
}

/// Recursively collects every text-editable file under one world folder,
/// paths relative to the world folder itself — what the frontend passes
/// back to `read_world_file`/`write_world_file`. `level.dat`/`session.lock`
/// and friends are binary and never match the text-extension filter, so no
/// special-casing is needed beyond the shared dotfile skip.
fn collect_world_text_files(dir: &Path, relative_prefix: &str, out: &mut Vec<ConfigFileEntry>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let relative = if relative_prefix.is_empty() {
            name.clone()
        } else {
            format!("{relative_prefix}/{name}")
        };
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_world_text_files(&entry.path(), &relative, out);
        } else if is_text_config_file(&name) {
            out.push(ConfigFileEntry { relative_path: relative.clone(), display_name: relative });
        }
    }
}

/// Resolve `saves/<world_folder>` for frontend-supplied folder names:
/// safe_join containment plus an is_dir check, so a stale or hostile name
/// is an error rather than an empty list or an escape.
fn resolve_world_dir(root: &Path, world_folder: &str) -> Result<PathBuf, String> {
    let dir = safe_join(&root.join("saves"), world_folder).map_err(|e| e.to_string())?;
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err("World not found.".to_string())
    }
}

/// Every text-editable file inside one world folder. Empty (not an error)
/// when the world has none editable in-app — the frontend then offers no
/// file browser for that world.
#[tauri::command]
pub fn list_world_files(
    state: State<'_, AppState>,
    instance_id: String,
    world_folder: String,
) -> Result<Vec<ConfigFileEntry>, String> {
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let _ = state.db.get_instance(&instance_id).map_err(|e| e.to_string())?;
    let world_dir = resolve_world_dir(&root, &world_folder)?;
    let mut out = Vec::new();
    collect_world_text_files(&world_dir, "", &mut out);
    out.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    Ok(out)
}

#[tauri::command]
pub fn read_world_file(
    instance_id: String,
    world_folder: String,
    relative_path: String,
) -> Result<String, String> {
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let world_dir = resolve_world_dir(&root, &world_folder)?;
    let path = safe_join(&world_dir, &relative_path).map_err(|e| e.to_string())?;
    let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
    if metadata.len() > MAX_EDITABLE_CONFIG_BYTES {
        return Err(format!(
            "This file is {:.1} MB — too large to edit in-app. Use \"Open folder\" and a text editor instead.",
            metadata.len() as f64 / (1024.0 * 1024.0)
        ));
    }
    fs::read_to_string(&path)
        .map_err(|_| "Couldn't read this file as text — it may not be a plain-text file.".to_string())
}

#[tauri::command]
pub fn write_world_file(
    instance_id: String,
    world_folder: String,
    relative_path: String,
    contents: String,
) -> Result<(), String> {
    // Same operation lock as config writes: rejects while the game runs so
    // Minecraft can't overwrite the edit (or vice versa) mid-session.
    let _operation = acquire(&instance_id)?;
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let world_dir = resolve_world_dir(&root, &world_folder)?;
    let path = safe_join(&world_dir, &relative_path).map_err(|e| e.to_string())?;
    fs::write(&path, contents).map_err(|e| e.to_string())
}

#[cfg(test)]
mod content_meta_cache_tests {
    use super::{apply_cache, normalize_for_match, ConfigTopEntry, ScannedFile};
    use crate::db::CachedContentMeta;
    use std::collections::HashMap;

    fn cache_row(category: &str, file_name: &str, size_bytes: u64, mtime_unix: i64, name: Option<&str>, icon: Option<&str>) -> CachedContentMeta {
        CachedContentMeta {
            category: category.to_string(),
            file_name: file_name.to_string(),
            size_bytes,
            mtime_unix,
            name: name.map(str::to_string),
            icon: icon.map(str::to_string),
            mod_id: None,
        }
    }

    #[test]
    fn has_config_uses_cached_mod_id_when_name_based_matching_would_miss() {
        // Curios API's declared name doesn't share a full-containment
        // relationship with "curios-common", so this only passes because
        // has_config now also checks the cached modId, not just the name.
        let scanned = vec![ScannedFile {
            file_name: "curios-neoforge-5.11.0.jar".to_string(),
            enabled: true,
            size_bytes: 10,
            mtime_unix: 10,
        }];
        let mut cache = HashMap::new();
        cache.insert(
            ("mod".to_string(), "curios-neoforge-5.11.0.jar".to_string()),
            CachedContentMeta {
                category: "mod".to_string(),
                file_name: "curios-neoforge-5.11.0.jar".to_string(),
                size_bytes: 10,
                mtime_unix: 10,
                name: Some("Curios API".to_string()),
                icon: None,
                mod_id: Some("curios".to_string()),
            },
        );
        let config_entries = vec![ConfigTopEntry {
            raw_name: "curios-common.toml".to_string(),
            is_dir: false,
            normalized: normalize_for_match("curios-common"),
        }];

        let entries = apply_cache(scanned, "mod", &cache, Some(&config_entries), &HashMap::new(), &HashMap::new());
        assert!(entries[0].has_config);
    }

    #[test]
    fn matching_fingerprint_is_resolved_from_cache_with_no_parsing() {
        let scanned = vec![ScannedFile {
            file_name: "Foo.jar".to_string(),
            enabled: true,
            size_bytes: 100,
            mtime_unix: 1000,
        }];
        let mut cache = HashMap::new();
        cache.insert(
            ("mod".to_string(), "Foo.jar".to_string()),
            cache_row("mod", "Foo.jar", 100, 1000, Some("Foo Mod"), Some("data:image/png;base64,x")),
        );

        let entries = apply_cache(scanned, "mod", &cache, None, &HashMap::new(), &HashMap::new());

        assert!(entries[0].meta_resolved, "matching size+mtime should count as a cache hit");
        assert_eq!(entries[0].name.as_deref(), Some("Foo Mod"));
        assert_eq!(entries[0].icon.as_deref(), Some("data:image/png;base64,x"));
    }

    #[test]
    fn changed_file_invalidates_the_cache_entry() {
        let scanned = vec![ScannedFile {
            file_name: "Foo.jar".to_string(),
            enabled: true,
            size_bytes: 200, // different size than the cached row
            mtime_unix: 1000,
        }];
        let mut cache = HashMap::new();
        cache.insert(
            ("mod".to_string(), "Foo.jar".to_string()),
            cache_row("mod", "Foo.jar", 100, 1000, Some("Foo Mod"), None),
        );

        let entries = apply_cache(scanned, "mod", &cache, None, &HashMap::new(), &HashMap::new());

        assert!(!entries[0].meta_resolved, "a changed file must not be served stale cached metadata");
        assert_eq!(entries[0].name, None);
    }

    #[test]
    fn cached_no_icon_result_still_counts_as_resolved() {
        // A jar with no embedded icon at all was already parsed once and the
        // cache correctly recorded "nothing found" — that must still skip
        // the per-row fetch, not look indistinguishable from "never checked".
        let scanned = vec![ScannedFile {
            file_name: "NoIcon.jar".to_string(),
            enabled: true,
            size_bytes: 50,
            mtime_unix: 500,
        }];
        let mut cache = HashMap::new();
        cache.insert(
            ("mod".to_string(), "NoIcon.jar".to_string()),
            cache_row("mod", "NoIcon.jar", 50, 500, Some("No Icon Mod"), None),
        );

        let entries = apply_cache(scanned, "mod", &cache, None, &HashMap::new(), &HashMap::new());

        assert!(entries[0].meta_resolved);
        assert_eq!(entries[0].icon, None);
    }

    #[test]
    fn unseen_file_is_left_unresolved() {
        let scanned = vec![ScannedFile {
            file_name: "New.jar".to_string(),
            enabled: true,
            size_bytes: 10,
            mtime_unix: 10,
        }];
        let entries = apply_cache(scanned, "mod", &HashMap::new(), None, &HashMap::new(), &HashMap::new());

        assert!(!entries[0].meta_resolved);
        assert_eq!(entries[0].name, None);
        assert_eq!(entries[0].icon, None);
    }
}

#[cfg(test)]
mod config_matching_tests {
    use super::{
        config_entry_matches, is_text_config_file, normalize_for_match, strip_config_side_suffix,
    };

    #[test]
    fn normalize_strips_punctuation_case_and_keeps_digits() {
        assert_eq!(normalize_for_match("Nature's Compass"), "naturescompass");
        assert_eq!(normalize_for_match("map_atlases-client"), "mapatlasesclient");
        assert_eq!(normalize_for_match("jade"), "jade");
    }

    #[test]
    fn real_mod_name_matches_its_real_config_entry() {
        // Cases pulled directly from an actual instance's config/ folder.
        let jade = normalize_for_match("jade");
        assert!(config_entry_matches(&normalize_for_match("jade"), &[jade]));

        let compass = normalize_for_match("Nature's Compass");
        assert!(config_entry_matches(&normalize_for_match("naturescompass"), &[compass]));

        let map_atlases = normalize_for_match("map_atlases");
        assert!(config_entry_matches(
            &normalize_for_match("map_atlases-client"),
            &[map_atlases]
        ));

        let enigmatic_legacy = normalize_for_match("Enigmatic Legacy");
        assert!(config_entry_matches(
            &normalize_for_match("enigmaticlegacy-client"),
            &[enigmatic_legacy.clone()]
        ));
        assert!(config_entry_matches(
            &normalize_for_match("enigmaticlegacy-common"),
            &[enigmatic_legacy]
        ));
    }

    #[test]
    fn side_config_matches_mod_name_with_extra_words() {
        // Curios API's modid is "curios", but its declared name has an extra
        // word the config filenames don't — was the actual bug report.
        let curios_api = normalize_for_match("Curios API");
        assert!(config_entry_matches(&normalize_for_match("curios-common"), &[curios_api.clone()]));
        assert!(config_entry_matches(&normalize_for_match("curios-client"), &[curios_api]));
    }

    #[test]
    fn side_config_does_not_match_mod_whose_name_merely_ends_with_the_modid() {
        // "Apothic Curios" is a *different* mod that just happens to end
        // with the word "curios" — it must not also claim Curios API's
        // curios-common/curios-client.toml.
        let apothic_curios = normalize_for_match("Apothic Curios");
        assert!(!config_entry_matches(&normalize_for_match("curios-common"), &[apothic_curios.clone()]));
        assert!(!config_entry_matches(&normalize_for_match("curios-client"), &[apothic_curios]));
    }

    #[test]
    fn side_config_does_not_match_every_neoforge_suffixed_jar() {
        // forge-client.toml is the modloader's own core config, not any
        // mod's — stripping it down to "forge" must not then match every
        // mod whose jar filename happens to contain "...-neoforge-...".
        let curios_jar = normalize_for_match("curios-neoforge-5.11.0+1.21.1");
        assert!(!config_entry_matches(&normalize_for_match("forge-client"), &[curios_jar]));
    }

    #[test]
    fn bare_loader_named_entry_never_fuzzy_matches_any_mod() {
        // FTB Essentials' jar filename embeds "-fabric-" (loader tag), which
        // used to make it fuzzy-match the loader's own top-level "fabric"
        // config folder (Fabric API's unrelated indigo-renderer.properties
        // etc.) via plain substring containment. The bare folder name must
        // never match, no matter what the mod's own terms look like.
        let ftb_essentials_jar = normalize_for_match("ftbessentials-fabric-1.20.1");
        assert!(!config_entry_matches(&normalize_for_match("fabric"), &[ftb_essentials_jar]));

        let some_forge_mod = normalize_for_match("somemod-forge-1.20.1");
        assert!(!config_entry_matches(&normalize_for_match("forge"), &[some_forge_mod]));
    }

    #[test]
    fn exact_mod_id_matches_bare_or_side_suffixed_entry() {
        // Mirrors list_mod_configs's exact-modId check: an entry matches
        // when its normalized name equals the modId directly, or with a
        // side suffix stripped off — the precise counterpart to the fuzzy
        // name-based `config_entry_matches` used when no modId is known.
        let mod_id = normalize_for_match("curios");
        let exact = |entry_norm: &str| {
            entry_norm == mod_id || strip_config_side_suffix(entry_norm) == Some(mod_id.as_str())
        };
        assert!(exact(&normalize_for_match("curios")));
        assert!(exact(&normalize_for_match("curios-common")));
        assert!(exact(&normalize_for_match("curios-client")));
        // "Apothic Curios" is a different mod (different modId entirely) —
        // its config folder would never normalize down to bare "curios".
        assert!(!exact(&normalize_for_match("apothiccurios")));
    }

    #[test]
    fn short_unrelated_names_do_not_false_positive() {
        // "js" (a hypothetical short modid) must not match "starterkit" just
        // because it's a short substring floating around somewhere.
        let short_term = normalize_for_match("js");
        assert!(!config_entry_matches(&normalize_for_match("starterkit"), &[short_term]));
    }

    #[test]
    fn unrelated_mod_and_config_entry_do_not_match() {
        let sodium = normalize_for_match("Sodium");
        assert!(!config_entry_matches(&normalize_for_match("starterkit"), &[sodium]));
    }

    #[test]
    fn text_config_extensions_recognized_binary_ones_excluded() {
        assert!(is_text_config_file("common.toml"));
        assert!(is_text_config_file("settings.json"));
        assert!(is_text_config_file("plugins.yaml"));
        assert!(is_text_config_file("enigmaticlegacy-client.omniconf"));
        assert!(!is_text_config_file("icon.png"));
        assert!(!is_text_config_file("resourcepack.zip"));
    }
}

#[cfg(test)]
mod launch_meta_tests {
    use super::{assess_readiness, mc_range_matches, range_mentions_mc_version, read_mod_metadata, FileLaunchMeta, ModLaunchMeta};
    use crate::dto::ModLoader;
    use std::io::Write;

    fn jar_with(entries: &[(&str, &str)]) -> std::path::PathBuf {
        // Unique filename per call — tests run in parallel threads and
        // would otherwise all stomp the same fixture jar mid-write.
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join("waybound-launch-meta-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("mod-{id}.jar"));
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, contents) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    const FABRIC_JSON: &str = r#"{
        "schemaVersion": 1, "id": "sodium", "name": "Sodium",
        "depends": {"minecraft": ">=1.20", "fabricloader": ">=0.15", "fabric-api": "*", "java": ">=17"}
    }"#;

    const NEOFORGE_TOML: &str = r#"
        [[mods]]
        modId = "formations"
        version = "1.0.4"
        displayName = "Formations"
        [[dependencies.formations]]
        modId = "neoforge"
        versionRange = "[21.0.113-beta,)"
        mandatory = true
        [[dependencies.formations]]
        modId = "minecraft"
        versionRange = "[1.21,1.22)"
        mandatory = true
        [[dependencies.formations]]
        modId = "somelib"
        versionRange = "[2.0,)"
        mandatory = true
        [[dependencies.formations]]
        modId = "optionalthing"
        versionRange = "[1.0,)"
        mandatory = false
        [[dependencies.formations]]
        modId = "newstyleoptional"
        type = "optional"
        versionRange = "[3.0,)"
    "#;

    const QUILT_JSON: &str = r#"{
        "schema_version": 1,
        "quilt_loader": {
            "id": "qmod", "version": "1.0",
            "depends": [
                {"id": "minecraft", "versions": ">=1.20"},
                {"id": "quilt_loader", "versions": ">=0.20"},
                {"id": "qsl", "versions": "*"}
            ]
        }
    }"#;

    #[test]
    fn fabric_jar_reports_loader_mc_and_required_deps() {
        let meta = read_mod_metadata(&jar_with(&[("fabric.mod.json", FABRIC_JSON)]));
        let entry = meta.launch.loaders.iter().find(|e| e.loader == "fabric").expect("fabric entry");
        assert_eq!(entry.mc_versions, vec![">=1.20"]);
        // fabricloader/java are runtime, not mods; fabric-api is a real dep.
        let ids: Vec<&str> = entry.dependencies.iter().map(|d| d.mod_id.as_str()).collect();
        assert_eq!(ids, vec!["fabric-api"]);
    }

    #[test]
    fn neoforge_toml_reports_loader_and_mandatory_deps_only() {
        let meta = read_mod_metadata(&jar_with(&[("META-INF/neoforge.mods.toml", NEOFORGE_TOML)]));
        let entry = meta.launch.loaders.iter().find(|e| e.loader == "neoforge").expect("neoforge entry");
        assert_eq!(entry.mc_versions, vec!["[1.21,1.22)"]);
        let ids: Vec<&str> = entry.dependencies.iter().map(|d| d.mod_id.as_str()).collect();
        // neoforge itself is pseudo; both optional styles are dropped.
        assert_eq!(ids, vec!["somelib"]);
    }

    #[test]
    fn quilt_jar_reports_quilt_loader() {
        let meta = read_mod_metadata(&jar_with(&[("quilt.mod.json", QUILT_JSON)]));
        assert!(meta.launch.loaders.iter().any(|e| e.loader == "quilt"));
        let entry = meta.launch.loaders.iter().find(|e| e.loader == "quilt").unwrap();
        let ids: Vec<&str> = entry.dependencies.iter().map(|d| d.mod_id.as_str()).collect();
        assert_eq!(ids, vec!["qsl"]);
    }

    #[test]
    fn multiloader_jar_registers_every_loader_it_ships() {
        // Cursee-style "merged" jars carry fabric + forge + neoforge
        // metadata side by side; the loader reads only its own. Judging by
        // the first file found false-flagged these on every other loader.
        let meta = read_mod_metadata(&jar_with(&[
            ("fabric.mod.json", FABRIC_JSON),
            ("META-INF/neoforge.mods.toml", NEOFORGE_TOML),
        ]));
        let loaders: Vec<&str> = meta.launch.loaders.iter().map(|e| e.loader.as_str()).collect();
        assert!(loaders.contains(&"fabric"), "fabric entry missing: {loaders:?}");
        assert!(loaders.contains(&"neoforge"), "neoforge entry missing: {loaders:?}");
        // The Fabric-only dep (fabric-api) lives in the fabric entry; the
        // NeoForge entry carries only the toml's own dep.
        let neo = meta.launch.loaders.iter().find(|e| e.loader == "neoforge").unwrap();
        let ids: Vec<&str> = neo.dependencies.iter().map(|d| d.mod_id.as_str()).collect();
        assert_eq!(ids, vec!["somelib"]);
    }

    #[test]
    fn multiloader_jar_is_clean_on_each_of_its_loaders() {
        // fabric-api (fabric entry) must not flag on a NeoForge instance,
        // and somelib (neoforge entry) must not flag on a Fabric one —
        // only the matching entry's deps count.
        let meta = read_mod_metadata(&jar_with(&[
            ("fabric.mod.json", FABRIC_JSON),
            ("META-INF/neoforge.mods.toml", NEOFORGE_TOML),
        ]));
        let merged = file("merged.jar", Some("merged"), meta.launch.clone());
        let neo_files = vec![
            merged.clone(),
            file("somelib.jar", Some("somelib"), launch_meta_single("neoforge", &[])),
        ];
        let neo_report = assess_readiness(ModLoader::NeoForge, "1.21.1", &neo_files, &[]);
        assert!(neo_report.wrong_loader.is_empty(), "{:?}", neo_report.wrong_loader);
        assert!(neo_report.missing_deps.is_empty(), "{:?}", neo_report.missing_deps);
        let fabric_files = vec![
            merged,
            file("fabric-api.jar", Some("fabric-api"), launch_meta_single("fabric", &[])),
        ];
        let fabric_report = assess_readiness(ModLoader::Fabric, "1.21.1", &fabric_files, &[]);
        assert!(fabric_report.wrong_loader.is_empty(), "{:?}", fabric_report.wrong_loader);
        assert!(fabric_report.missing_deps.is_empty(), "{:?}", fabric_report.missing_deps);
    }

    fn file(name: &str, mod_id: Option<&str>, launch: ModLaunchMeta) -> FileLaunchMeta {
        FileLaunchMeta {
            file_name: name.to_string(),
            mod_name: None,
            mod_id: mod_id.map(str::to_string),
            all_mod_ids: mod_id.map(|id| vec![id.to_string()]).unwrap_or_default(),
            launch,
        }
    }

    fn launch_with(loader: &str, deps: &[&str]) -> ModLaunchMeta {
        launch_meta_single(loader, deps)
    }

    fn launch_meta_single(loader: &str, deps: &[&str]) -> ModLaunchMeta {
        ModLaunchMeta {
            loaders: vec![super::LoaderMetaEntry {
                loader: loader.to_string(),
                mc_versions: Vec::new(),
                dependencies: deps
                    .iter()
                    .map(|d| super::ModDependency { mod_id: d.to_string(), version_range: None })
                    .collect(),
            }],
        }
    }

    #[test]
    fn neoforge_jar_on_forge_instance_is_flagged() {
        let files = vec![file("formations.jar", Some("formations"), launch_with("neoforge", &[]))];
        let report = assess_readiness(ModLoader::Forge, "1.21.1", &files, &[]);
        assert_eq!(report.checked_files, 1);
        assert_eq!(report.wrong_loader.len(), 1);
        assert_eq!(report.wrong_loader[0].detected_loader, "neoforge");
        assert!(report.missing_deps.is_empty());
    }

    #[test]
    fn matching_loader_and_satisfied_dep_is_clean() {
        let files = vec![
            file("a.jar", Some("moda"), launch_with("neoforge", &["somelib"])),
            file("b.jar", Some("somelib"), launch_with("neoforge", &[])),
        ];
        let report = assess_readiness(ModLoader::NeoForge, "1.21.1", &files, &[]);
        assert!(report.wrong_loader.is_empty());
        assert!(report.missing_deps.is_empty());
    }

    #[test]
    fn missing_dep_names_the_requiring_file() {
        let files = vec![file("a.jar", Some("moda"), launch_with("neoforge", &["somelib"]))];
        let report = assess_readiness(ModLoader::NeoForge, "1.21.1", &files, &[]);
        assert_eq!(report.missing_deps.len(), 1);
        assert_eq!(report.missing_deps[0].dep_mod_id, "somelib");
        assert_eq!(report.missing_deps[0].file_name, "a.jar");
    }

    fn launch_meta_mc(loader: &str, mc: &[&str]) -> ModLaunchMeta {
        ModLaunchMeta {
            loaders: vec![super::LoaderMetaEntry {
                loader: loader.to_string(),
                mc_versions: mc.iter().map(|s| s.to_string()).collect(),
                dependencies: Vec::new(),
            }],
        }
    }

    #[test]
    fn mc_range_matching_covers_shipped_shapes() {
        // Exact versions match by major.minor line — patches stay
        // wire-compatible, so 1.20.0 runs on 1.20.1.
        assert!(mc_range_matches("1.21.1", "1.21.1"));
        assert!(mc_range_matches("1.21", "1.21.1"));
        assert!(mc_range_matches("1.20.0", "1.20.1"));
        assert!(!mc_range_matches("1.20.1", "1.21.1"));
        assert!(!mc_range_matches("1.20", "1.21.1"));
        // Maven intervals are line-inclusive: the MDK-boilerplate
        // `[1.21,1.21.1)` loads on 1.21.1 (the loader itself accepts it),
        // while a range outside the instance's line still flags.
        assert!(mc_range_matches("[1.20,1.21)", "1.20.4"));
        assert!(mc_range_matches("[1.20,1.21)", "1.21.1"));
        assert!(!mc_range_matches("[1.20,1.21)", "1.22"));
        assert!(!mc_range_matches("[1.20,1.21)", "1.19.4"));
        assert!(mc_range_matches("[1.21,1.21.1)", "1.21.1"));
        assert!(mc_range_matches("[1.21,1.22)", "1.21.1"));
        assert!(mc_range_matches("(,1.21]", "1.21"));
        assert!(mc_range_matches("(,1.21]", "1.21.1"));
        assert!(mc_range_matches("[1.21,)", "1.21.1"));
        // Fabric comparators + unions + wildcards. Comparators keep
        // full-triplet strictness: `<1.21.2` still accepts 1.21.1.
        assert!(mc_range_matches(">=1.20", "1.21.1"));
        assert!(!mc_range_matches(">=1.22", "1.21.1"));
        assert!(mc_range_matches(">=1.20 <1.21", "1.20.4"));
        assert!(!mc_range_matches(">=1.20 <1.21", "1.21.1"));
        assert!(mc_range_matches(">=1.21.1 <1.21.2", "1.21.1"));
        assert!(!mc_range_matches(">=1.21.1 <1.21.2", "1.21.2"));
        assert!(mc_range_matches("~1.20", "1.20.1"));
        assert!(mc_range_matches("^1.20", "1.21.1"));
        assert!(mc_range_matches("1.20.x", "1.20.4"));
        assert!(mc_range_matches(">=1.20 || >=1.21.1", "1.21.1"));
        assert!(mc_range_matches("26.2", "26.2"));
        // Maven multi-range unions (exact-pin lists).
        assert!(mc_range_matches("[1.21],[1.21.1]", "1.21.1"));
        assert!(!mc_range_matches("[1.20],[1.20.4]", "1.21.1"));
        // Anything + garbage passes: advisory must not cry wolf.
        assert!(mc_range_matches("*", "1.21.1"));
        assert!(mc_range_matches("", "1.21.1"));
        assert!(mc_range_matches("garbage", "1.21.1"));
    }

    #[test]
    fn range_shape_guard_ignores_non_mc_junk() {
        assert!(range_mentions_mc_version(">=1.20"));
        assert!(range_mentions_mc_version("[1.20,1.21)"));
        assert!(range_mentions_mc_version("1.21.1"));
        assert!(!range_mentions_mc_version("2.64"));
        assert!(!range_mentions_mc_version("garbage"));
        assert!(!range_mentions_mc_version(""));
    }

    #[test]
    fn outdated_jar_is_flagged_by_metadata_range() {
        let files = vec![FileLaunchMeta {
            file_name: "somemod-3.0.jar".to_string(),
            mod_name: Some("SomeMod".to_string()),
            mod_id: Some("somemod".to_string()),
            all_mod_ids: vec!["somemod".to_string()],
            launch: launch_meta_mc("neoforge", &["[1.19,1.20]"]),
        }];
        let report = assess_readiness(ModLoader::NeoForge, "1.21.1", &files, &[]);
        assert!(report.wrong_loader.is_empty());
        assert_eq!(report.wrong_game_version.len(), 1);
        assert_eq!(report.wrong_game_version[0].expected, "1.21.1");
        assert_eq!(report.wrong_game_version[0].declared, "[1.19,1.20]");
    }

    #[test]
    fn versioned_file_name_without_metadata_stays_silent() {
        // Incident regression test: `alexsmobs-1.22.9.jar` is Alex's Mobs'
        // own 1.22.9 built for MC 1.20.1 — a file-name-only signal flagged
        // it (plus ATi Structures, BetterThanMending) on a 1.20.1 instance.
        // Names are never consulted now, so all such jars stay silent.
        for name in [
            "alexsmobs-1.22.9.jar",
            "ATi Structures V1.4.6.jar",
            "BetterThanMending-1.7.2.jar",
            "biggerstacks-1.20.1-2026.06.17-all.jar",
        ] {
            let files = vec![file(name, Some("somemod"), launch_with("forge", &[]))];
            let report = assess_readiness(ModLoader::Forge, "1.20.1", &files, &[]);
            assert!(report.wrong_loader.is_empty(), "{name}");
            assert!(report.wrong_game_version.is_empty(), "{name}");
        }
    }

    #[test]
    fn matching_game_version_stays_silent() {
        let ranged = vec![FileLaunchMeta {
            file_name: "somemod-3.0.jar".to_string(),
            mod_name: Some("SomeMod".to_string()),
            mod_id: Some("somemod".to_string()),
            all_mod_ids: vec!["somemod".to_string()],
            launch: launch_meta_mc("neoforge", &["[1.21,1.22)"]),
        }];
        let report = assess_readiness(ModLoader::NeoForge, "1.21.1", &ranged, &[]);
        assert!(report.wrong_game_version.is_empty());
        let named = vec![file(
            "somemod-1.21.jar",
            Some("somemod"),
            launch_with("neoforge", &[]),
        )];
        let report = assess_readiness(ModLoader::NeoForge, "1.21.1", &named, &[]);
        assert!(report.wrong_game_version.is_empty());
    }

    #[test]
    fn jarjar_embedded_ids_satisfy_dependencies() {
        use std::io::Write;
        // A host jar whose dep only exists nested inside it (Create's
        // flywheel pattern): the nested id must count as installed.
        let nested = {
            let mut buf = Vec::new();
            {
                let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
                w.start_file("META-INF/neoforge.mods.toml", zip::write::SimpleFileOptions::default()).unwrap();
                w.write_all(b"[[mods]]\nmodId = \"flywheel\"\nversion = \"1.0.6\"\n").unwrap();
                w.finish().unwrap();
            }
            buf
        };
        let dir = std::env::temp_dir().join("waybound-launch-meta-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("host-dep-test.jar");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut w = zip::ZipWriter::new(file);
            w.start_file("META-INF/neoforge.mods.toml", zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(
                b"[[mods]]\nmodId = \"create\"\nversion = \"6.0.10\"\n[[dependencies.create]]\nmodId = \"flywheel\"\ntype = \"required\"\n",
            )
            .unwrap();
            w.start_file("META-INF/jarjar/metadata.json", zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b"{\"jars\": [{\"path\": \"META-INF/jarjar/flywheel.jar\"}]}").unwrap();
            w.start_file("META-INF/jarjar/flywheel.jar", zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(&nested).unwrap();
            w.finish().unwrap();
        }
        let meta = read_mod_metadata(&path);
        assert!(meta.embedded_ids.iter().any(|id| id == "flywheel"), "{:?}", meta.embedded_ids);
        let files = vec![file("host.jar", Some("create"), meta.launch.clone())];
        let report = assess_readiness(ModLoader::NeoForge, "1.21.1", &files, &meta.embedded_ids);
        assert!(report.missing_deps.is_empty(), "{:?}", report.missing_deps);
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod worlds_servers_tests {
    use super::{collect_world_text_files, read_servers_file, read_world_entry};
    use std::io::Write;
    use valence_nbt::compound;

    fn gzip_nbt(root: valence_nbt::Compound) -> Vec<u8> {
        let mut raw = Vec::new();
        valence_nbt::to_binary(&root, &mut raw, "").unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap()
    }

    fn world_dir(label: &str, level_dat: Option<Vec<u8>>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("waybound-worlds-test-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        let world = dir.join("My World");
        std::fs::create_dir_all(&world).unwrap();
        if let Some(bytes) = level_dat {
            std::fs::write(world.join("level.dat"), bytes).unwrap();
        }
        dir
    }

    fn level_bytes() -> Vec<u8> {
        gzip_nbt(valence_nbt::compound! {
            "Data" => valence_nbt::compound! {
                "LevelName" => "Overworld Adventures",
                "LastPlayed" => 1_700_000_000_000_i64,
                "GameType" => 1,
                "Version" => valence_nbt::compound! { "Name" => "1.21.1" },
            },
        })
    }

    #[test]
    fn world_entry_reads_name_mode_version_and_play_time() {
        let dir = world_dir("full", Some(level_bytes()));
        let entry = read_world_entry(&dir.join("My World"), "My World");
        assert_eq!(entry.folder_name, "My World");
        assert_eq!(entry.name.as_deref(), Some("Overworld Adventures"));
        assert_eq!(entry.last_played_ms, Some(1_700_000_000_000));
        assert_eq!(entry.game_mode.as_deref(), Some("Creative"));
        assert_eq!(entry.game_version.as_deref(), Some("1.21.1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_level_dat_degrades_to_folder_name() {
        let dir = world_dir("corrupt", Some(b"definitely not nbt".to_vec()));
        let entry = read_world_entry(&dir.join("My World"), "My World");
        assert_eq!(entry.folder_name, "My World");
        assert!(entry.name.is_none());
        assert!(entry.last_played_ms.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_level_dat_degrades_to_folder_name() {
        let dir = world_dir("missing", None);
        let entry = read_world_entry(&dir.join("My World"), "My World");
        assert_eq!(entry.folder_name, "My World");
        assert!(entry.name.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn servers_file_lists_name_and_address_with_icon() {
        let dir = std::env::temp_dir().join("waybound-servers-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut raw = Vec::new();
        valence_nbt::to_binary(
            &valence_nbt::compound! {
                "servers" => valence_nbt::List::Compound(vec![
                    valence_nbt::compound! { "name" => "Home", "ip" => "play.example.com:25565" },
                    valence_nbt::compound! { "name" => "LAN", "ip" => "192.168.1.2:25565", "icon" => "aGVsbG8=" },
                ]),
            },
            &mut raw,
            "",
        )
        .unwrap();
        // Vanilla writes servers.dat uncompressed.
        std::fs::write(dir.join("servers.dat"), &raw).unwrap();
        let entries = read_servers_file(&dir.join("servers.dat"));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Home");
        assert_eq!(entries[0].address, "play.example.com:25565");
        assert!(entries[0].icon.is_none());
        assert_eq!(
            entries[1].icon.as_deref(),
            Some("data:image/png;base64,aGVsbG8=")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_servers_file_is_empty_not_error() {
        let dir = std::env::temp_dir().join("waybound-servers-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_servers_file(&dir.join("servers.dat")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn world_text_files_list_json_and_skip_binaries() {
        let dir = std::env::temp_dir().join("waybound-world-files-test");
        let _ = std::fs::remove_dir_all(&dir);
        let world = dir.join("My World");
        std::fs::create_dir_all(world.join("stats")).unwrap();
        std::fs::create_dir_all(world.join("advancements")).unwrap();
        std::fs::write(world.join("level.dat"), b"\x1fbeing-binary").unwrap();
        std::fs::write(world.join("session.lock"), b"1234").unwrap();
        std::fs::write(world.join(".hidden.json"), b"{}").unwrap();
        std::fs::write(world.join("stats").join("uuid.json"), b"{}").unwrap();
        std::fs::write(
            world.join("advancements").join("done.json"),
            b"{\"x\":1}",
        )
        .unwrap();
        let mut out = Vec::new();
        collect_world_text_files(&world, "", &mut out);
        let mut paths: Vec<_> = out.iter().map(|e| e.relative_path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec!["advancements/done.json", "stats/uuid.json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
