use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};

use super::{ModpackError, ModpackImportResult, PreparedModpackImport};
use super::transaction::{validate_pack_path, PackTransaction, MAX_INDEX_BYTES, MAX_PACK_FILES, MAX_PACK_FILE_BYTES};
use crate::commands::content::DISABLED_SUFFIX;
use crate::download::{contained_join, download_bytes_capped_with_retry, http_client, safe_join, CancelToken, DOWNLOAD_CONCURRENCY};
use crate::sources::curseforge::CurseForgeClient;

// CurseForge's CDN (the actual file bytes) isn't rate-limited the way
// api.curseforge.com is, so downloads stay at the normal concurrency. Only
// the *metadata* fallback below — individual /download-url and /files calls
// for files the batch lookup couldn't resolve — hits the API repeatedly, and
// that's what needs a much smaller concurrency to avoid tripping the limit.
const METADATA_FALLBACK_CONCURRENCY: usize = 2;

/// A shader pack's defining trait — what Iris/OptiFine themselves key off of
/// — is a top-level `shaders/` directory holding the actual shader programs;
/// a plain resource pack has `assets/` instead. CurseForge's manifest files
/// both under the same generic "file" entry with no type distinction, so the
/// only reliable signal is the zip's own contents, not the filename or the
/// manifest.
fn sniff_is_shaderpack(bytes: &[u8]) -> bool {
    let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    (0..archive.len()).any(|i| {
        archive
            .by_index(i)
            .is_ok_and(|f| f.name().to_ascii_lowercase().starts_with("shaders/"))
    })
}

/// Picks the right destination folder for a downloaded file by its own
/// extension/contents rather than trusting the manifest. Modpacks routinely
/// list resource packs and shader packs as regular required "files"
/// (CurseForge doesn't distinguish mod jars from other content in
/// `manifest.files`) — Forge only loads `.jar` from `mods/`, and Iris/OptiFine
/// only find shaders in `shaderpacks/`, so anything landing in the wrong
/// folder is dead weight at best and silently never loaded at worst.
fn dest_dir_for(filename: &str, bytes: &[u8], mods_dir: &Path, resourcepacks_dir: &Path, shaderpacks_dir: &Path) -> PathBuf {
    if filename.to_ascii_lowercase().ends_with(".jar") {
        mods_dir.to_path_buf()
    } else if sniff_is_shaderpack(bytes) {
        shaderpacks_dir.to_path_buf()
    } else {
        resourcepacks_dir.to_path_buf()
    }
}

/// Locates a file that may have been placed in either resourcepacks/ or
/// shaderpacks/ — used everywhere a caller only needs "is this already here"
/// or "where is this so I can remove it" and doesn't have the file's bytes on
/// hand to sniff its real type (nothing to download again, or the file
/// already exists from some earlier run).
/// Checks both the enabled filename and its `.disabled`-suffixed form — a
/// mod the user toggled off is renamed on disk, not removed, so treating
/// only the bare name as "present" made every disabled file look identical
/// to one that was never installed at all (see `pending_missing_mods`).
fn find_existing(filename: &str, mods_dir: &Path, resourcepacks_dir: &Path, shaderpacks_dir: &Path) -> Option<PathBuf> {
    let disabled_name = format!("{filename}{DISABLED_SUFFIX}");
    let dirs: &[&Path] = if filename.to_ascii_lowercase().ends_with(".jar") {
        &[mods_dir]
    } else {
        &[resourcepacks_dir, shaderpacks_dir]
    };
    dirs.iter().find_map(|dir| {
        contained_join(dir, filename)
            .ok()
            .filter(|p| p.is_file())
            .or_else(|| contained_join(dir, &disabled_name).ok().filter(|p| p.is_file()))
    })
}

fn file_sha1(path: &Path) -> Option<String> {
    use sha1::Digest;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hash = sha1::Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).ok()?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    Some(hex::encode(hash.finalize()))
}

fn matching_existing(entry: &PackManifestEntry, root: &Path) -> bool {
    let Some(path) = find_existing(&entry.filename, &root.join("mods"), &root.join("resourcepacks"), &root.join("shaderpacks")) else { return false };
    if let Some(expected) = entry.sha1.as_deref().filter(|hash| !hash.is_empty()) {
        return file_sha1(&path).is_some_and(|actual| actual.eq_ignore_ascii_case(expected));
    }
    // With no upstream hash, pending same-name bytes cannot prove that
    // the requested version was placed. Only the manual transaction clears
    // pending and records its observed hash.
    !entry.pending
}

/// One file this instance's CurseForge pack manifest wanted, as of the last
/// import — the record that lets a later update tell "the pack dropped this"
/// (safe to remove) apart from "the user added this themselves" (never
/// tracked here, so never touched). Same approach PrismLauncher's Flame
/// importer uses (`<instance>/flame/manifest.json`): reconciliation only
/// ever considers files this list remembers, nothing else in the folder.
///
/// Deliberately self-sufficient (carries `name`/`url`/`sha1`, not just the
/// ids needed for reconciliation) so it doubles as the source of truth for
/// "what's still missing" after an app restart, when the in-memory install
/// list from the original import is long gone — see `pending_missing_mods`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackManifestEntry {
    project_id: u32,
    file_id: u32,
    name: String,
    filename: String,
    url: String,
    sha1: Option<String>,
    /// From the manifest's `required` flag. Optional files install like
    /// everything else (what the author shipped is what lands — Prism
    /// parity), but the flag is recorded so later flows can tell "pack
    /// core" from "pack extra". Old sidecars predate the field and read
    /// back as required.
    #[serde(default = "default_true")]
    required: bool,
    #[serde(default)]
    pending: bool,
    #[serde(default)]
    retained_files: Vec<RetainedPackFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RetainedPackFile {
    filename: String,
    sha1: Option<String>,
}

const PACK_MANIFEST_FILENAME: &str = ".curseforge-pack-manifest.json";

fn load_pack_manifest(instance_root: &Path) -> Result<Vec<PackManifestEntry>, ModpackError> {
    match std::fs::read(contained_join(instance_root, PACK_MANIFEST_FILENAME)?) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn save_pack_manifest(instance_root: &Path, entries: &[PackManifestEntry]) -> Result<(), ModpackError> {
    let path = contained_join(instance_root, PACK_MANIFEST_FILENAME)?;
    crate::download::atomic_write(&path, &serde_json::to_vec_pretty(entries)?)?;
    Ok(())
}

/// Drops one project from the pack's tracked manifest — for a mod the user
/// has decided not to grab (its author blocks automatic download and the
/// user doesn't want it manually either). Membership in `pending_missing_mods`
/// is decided purely by "does the manifest still list this file," so without
/// this, a mod the user explicitly opted out of nags forever on every
/// restart, indistinguishable from one they just haven't gotten to yet.
pub fn remove_pack_manifest_entry(instance_root: &Path, project_id: u32) -> Result<(), ModpackError> {
    let entries = load_pack_manifest(instance_root)?;
    if !entries.iter().any(|e| e.project_id == project_id) { return Ok(()); }
    let kept: Vec<PackManifestEntry> = entries.into_iter().filter(|e| e.project_id != project_id).collect();
    save_pack_manifest(instance_root, &kept)
}

/// Persist a single pack-origin mod update's pending relationship before
/// handing it to the manual downloader. Working files remain untouched.
pub(crate) fn prepare_pending_pack_update(
    root: &Path,
    item: &crate::dto::instance::MissingMod,
    file_id: u32,
    previous_path: &Path,
) -> Result<PackTransaction, ModpackError> {
    validate_filename(&item.filename)?;
    crate::download::ensure_contained_path(root, previous_path)?;
    let previous_name = previous_path.file_name().and_then(|name| name.to_str())
        .ok_or_else(|| ModpackError::Other("Invalid previous pack filename".into()))?;
    let previous_name = previous_name.strip_suffix(DISABLED_SUFFIX).unwrap_or(previous_name);
    validate_filename(previous_name)?;
    let previous_hash = file_sha1(previous_path)
        .ok_or_else(|| ModpackError::Other("Cannot verify the previous pack file; update left unchanged".into()))?;
    let mut entries = load_pack_manifest(root)?;
    let old = entries.iter().find(|entry| entry.project_id == item.project_id).cloned();
    let mut receipt = PackManifestEntry {
        project_id: item.project_id, file_id, name: item.name.clone(), filename: item.filename.clone(),
        url: item.url.clone(), sha1: item.sha1.clone(),
        required: old.as_ref().is_none_or(|entry| entry.required), pending: true,
        retained_files: Vec::new(),
    };
    let mut transaction = PackTransaction::new(root)?;
    if let Some(old) = old.as_ref() {
        reconcile_pack_files(&mut transaction, std::slice::from_ref(old), std::slice::from_mut(&mut receipt), root)?;
    }
    receipt.retained_files.retain(|file| file.filename != previous_name);
    receipt.retained_files.push(RetainedPackFile { filename: previous_name.into(), sha1: Some(previous_hash) });
    if let Some(index) = entries.iter().position(|entry| entry.project_id == item.project_id) {
        entries[index] = receipt;
    } else { entries.push(receipt); }
    transaction.stage(PACK_MANIFEST_FILENAME, &serde_json::to_vec_pretty(&entries)?)?;
    Ok(transaction)
}

/// Publish a downloaded pack-owned file's receipt inside its existing file
/// transaction. Caller supplies the hash proven against the downloaded bytes.
pub(crate) fn stage_completed_pack_update(
    transaction: &mut PackTransaction,
    root: &Path,
    item: &crate::dto::instance::MissingMod,
    file_id: u32,
    actual_sha1: String,
) -> Result<(), ModpackError> {
    validate_filename(&item.filename)?;
    let old = load_pack_manifest(root)?;
    let index = old.iter().position(|entry| entry.project_id == item.project_id);
    let previous = index.map(|index| &old[index]);
    let mut receipt = PackManifestEntry {
        project_id: item.project_id, file_id, name: item.name.clone(), filename: item.filename.clone(),
        url: item.url.clone(), sha1: None,
        required: previous.is_none_or(|entry| entry.required), pending: false, retained_files: Vec::new(),
    };
    // Keep the API's content-type-specific project URL when already known.
    if let Some((project_url, _)) = previous.and_then(|entry| entry.url.rsplit_once("/download/")) {
        receipt.url = format!("{project_url}/download/{file_id}");
    }
    let mut entries = old;
    let index = match index {
        Some(index) => { entries[index] = receipt; index },
        None => { entries.push(receipt); entries.len() - 1 },
    };
    // Direct install already stages/removes exact DB paths. Never infer an
    // enabled sibling's ownership from the sidecar's unsuffixed filename.
    stage_completed_pack_receipt(transaction, &mut entries, index, actual_sha1)
}

/// Direct and manual replacement share completed receipt publication.
fn stage_completed_pack_receipt(
    transaction: &mut PackTransaction,
    entries: &mut [PackManifestEntry],
    index: usize,
    actual_sha1: String,
) -> Result<(), ModpackError> {
    entries[index].pending = false;
    entries[index].sha1 = Some(actual_sha1);
    entries[index].retained_files.clear();
    transaction.stage(PACK_MANIFEST_FILENAME, &serde_json::to_vec_pretty(entries)?)?;
    Ok(())
}

/// Remove a dropped project, or superseded bytes only after a verified
/// replacement. Preserve old bytes for a pending manual replacement.
fn reconcile_pack_files(
    transaction: &mut PackTransaction,
    old_manifest: &[PackManifestEntry],
    new_manifest: &mut [PackManifestEntry],
    root: &Path,
) -> Result<(), ModpackError> {
    let mods = root.join("mods");
    let resources = root.join("resourcepacks");
    let shaders = root.join("shaderpacks");
    for old in old_manifest {
        let replacement = new_manifest.iter_mut().find(|entry| entry.project_id == old.project_id);
        let mut old_files = old.retained_files.clone();
        old_files.push(RetainedPackFile { filename: old.filename.clone(), sha1: old.sha1.clone() });
        if let Some(replacement) = replacement {
            if replacement.pending {
                for mut file in old_files {
                    if let Some(path) = find_existing(&file.filename, &mods, &resources, &shaders) {
                        file.sha1 = file_sha1(&path);
                        if !replacement.retained_files.iter().any(|entry| entry.filename == file.filename) {
                            replacement.retained_files.push(file);
                        }
                    }
                }
                continue;
            }
            for file in old_files {
                validate_filename(&file.filename)?;
                if let Some(path) = find_existing(&file.filename, &mods, &resources, &shaders) {
                    transaction.remove(&path)?;
                }
            }
        } else {
            for file in old_files {
                validate_filename(&file.filename)?;
                if let Some(path) = find_existing(&file.filename, &mods, &resources, &shaders) {
                    transaction.remove(&path)?;
                }
            }
        }
    }
    Ok(())
}

fn validate_filename(filename: &str) -> Result<(), ModpackError> {
    validate_pack_path(filename)?;
    if filename.is_empty() || filename.contains(['/', '\\']) {
        return Err(crate::download::DownloadError::UnsafePath(filename.into()).into());
    }
    safe_join(Path::new("."), filename)?;
    Ok(())
}

/// Pending receipt authorizes replacement; filename alone never does.
/// Consumes a verified same-root temporary file on success.
pub(crate) fn prepare_manual_pack_replacement(
    root: &Path,
    project_id: u32,
    filename: &str,
    staged_path: &Path,
) -> Result<Option<(PackTransaction, PathBuf)>, ModpackError> {
    validate_filename(filename)?;
    let old = load_pack_manifest(root)?;
    let Some(index) = old.iter().position(|entry| entry.project_id == project_id && entry.filename == filename) else { return Ok(None) };
    let receipt = &old[index];
    crate::download::ensure_contained_path(root, staged_path)?;
    let actual = file_sha1(staged_path).ok_or_else(|| ModpackError::Other("Cannot hash staged manual download".into()))?;
    if receipt.sha1.as_deref().is_some_and(|hash| !hash.is_empty() && !actual.eq_ignore_ascii_case(hash)) {
        return Err(crate::download::DownloadError::HashMismatch("sha1".into()).into());
    }
    if receipt.pending && receipt.sha1.as_deref().is_none_or(str::is_empty)
        && receipt.retained_files.iter().any(|file| file.sha1.as_deref().is_some_and(|hash| actual.eq_ignore_ascii_case(hash))) {
        return Err(ModpackError::Other("Download matches the retained old pack version, not a verified replacement".into()));
    }
    let mods = root.join("mods");
    let resources = root.join("resourcepacks");
    let shaders = root.join("shaderpacks");
    let existing = find_existing(filename, &mods, &resources, &shaders);
    if let Some(existing) = &existing {
        let hash = file_sha1(existing).ok_or_else(|| ModpackError::Other("Cannot hash existing pack target".into()))?;
        let target_matches = receipt.sha1.as_deref().is_some_and(|expected| hash.eq_ignore_ascii_case(expected));
        let retained_matches = receipt.retained_files.iter().any(|file| file.filename == filename
            && file.sha1.as_deref().is_some_and(|expected| hash.eq_ignore_ascii_case(expected)));
        if !target_matches && !(receipt.pending && retained_matches) {
            return Err(ModpackError::Other("Existing file is not a verified retained pack version; no replacement performed".into()));
        }
    }
    let disabled = existing.as_ref().is_some_and(|path| path.to_string_lossy().ends_with(DISABLED_SUFFIX))
        || receipt.retained_files.iter().filter_map(|file| find_existing(&file.filename, &mods, &resources, &shaders))
            .any(|path| path.to_string_lossy().ends_with(DISABLED_SUFFIX));
    let folder = if filename.to_ascii_lowercase().ends_with(".jar") { mods }
        else {
            let mut archive = zip::ZipArchive::new(std::fs::File::open(staged_path)?).ok();
            let shader = archive.as_mut().is_some_and(|archive| (0..archive.len()).any(|index|
                archive.by_index(index).is_ok_and(|entry| entry.name().to_ascii_lowercase().starts_with("shaders/"))));
            if shader { shaders } else { resources }
        };
    let target_name = if disabled { format!("{filename}{DISABLED_SUFFIX}") } else { filename.to_string() };
    let dest = contained_join(&folder, &target_name)?;
    let relative = dest.strip_prefix(root).map_err(|_| ModpackError::Other("Manual destination outside instance".into()))?;
    let mut transaction = PackTransaction::new(root)?;
    transaction.stage_file(&relative.to_string_lossy(), staged_path)?;
    if let Some(existing) = existing.as_ref().filter(|path| **path != dest) {
        transaction.remove(existing)?;
    }
    let mut entries = old.clone();
    entries[index].pending = false;
    reconcile_pack_files(&mut transaction, &old[index..=index], &mut entries[index..=index], root)?;
    stage_completed_pack_receipt(&mut transaction, &mut entries, index, actual)?;
    Ok(Some((transaction, dest)))
}

/// The exact-file CurseForge page for a project/file, preferring the API's
/// own `websiteUrl` (correct for any content type — a mod, resourcepack, or
/// shader all live under different URL path segments) over a hardcoded
/// `mc-mods` guess, which only 404s less often than not for non-mod content.
pub(crate) fn curseforge_file_url(website_url: Option<&str>, fallback_slug: &str, file_id: u32) -> String {
    let base = match website_url {
        Some(url) => url.trim_end_matches('/').to_string(),
        None => format!("https://www.curseforge.com/minecraft/mc-mods/{fallback_slug}"),
    };
    format!("{base}/download/{file_id}")
}

/// Every file this instance's last CurseForge import still hasn't managed to
/// place on disk — the ones a restarted app has no other memory of, since
/// the original install's progress lived only in the frontend's in-memory
/// store. Membership is decided purely by "is the manifest's exact filename
/// present in mods/ or resourcepacks/ right now", so a mod placed by the
/// Downloads-folder watcher after the restart is correctly not reported.
pub fn pending_missing_mods(instance_root: &Path) -> Vec<crate::dto::instance::MissingMod> {
    load_pack_manifest(instance_root)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| !matching_existing(entry, instance_root))
        .map(|entry| crate::dto::instance::MissingMod {
            project_id: entry.project_id,
            name: entry.name,
            filename: entry.filename,
            url: entry.url,
            sha1: entry.sha1,
        })
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct CurseForgeManifest {
    #[serde(default)]
    pub name: String,
    pub files: Vec<CurseForgeManifestFile>,
    #[serde(default)]
    overrides: String,
    /// The pack's own `minecraft` section — the ONLY place a CurseForge pack
    /// declares its loader (`modLoaders[].id`, e.g. `"neoforge-21.1.172"`).
    /// The per-file `projectID`/`fileID` list below carries no loader signal
    /// (pack zips are loader-agnostic archives), so without this the
    /// installer has to guess the loader from Browse categories and gets
    /// NeoForge packs wrong (see `declared_loader_from_bytes`).
    #[serde(default)]
    pub minecraft: Option<CfManifestMinecraft>,
}

#[derive(Debug, Deserialize)]
pub struct CfManifestMinecraft {
    #[serde(default, rename = "modLoaders")]
    pub mod_loaders: Vec<CfManifestModLoader>,
}

#[derive(Debug, Deserialize)]
pub struct CfManifestModLoader {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub primary: bool,
}

#[derive(Debug, Deserialize)]
pub struct CurseForgeManifestFile {
    #[serde(rename = "projectID")]
    pub project_id: u32,
    #[serde(rename = "fileID")]
    pub file_id: u32,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

fn verify_file(data: &[u8], sha1: Option<&str>) -> Result<(), ModpackError> {
    if let Some(expected) = sha1.filter(|s| !s.is_empty()) {
        use sha1::Digest;
        let actual = hex::encode(sha1::Sha1::digest(data));
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(crate::download::DownloadError::HashMismatch("sha1".to_string()).into());
        }
    }
    Ok(())
}

/// Manifest file id -> required flag (absent ids read as required, matching
/// the manifest struct's own default).
fn manifest_required_map(files: &[CurseForgeManifestFile]) -> std::collections::HashMap<u32, bool> {
    files.iter().map(|f| (f.file_id, f.required)).collect()
}

pub async fn import_curseforge_modpack_zip(
    bytes: &[u8],
    instance_root: &Path,
    api_key: &str,
    cancel: &CancelToken,
    report: &impl Fn(u32, u32, &str),
) -> Result<ModpackImportResult, ModpackError> {
    prepare_curseforge_modpack_zip(bytes, instance_root, api_key, cancel, report).await?.commit(cancel).await
}

pub(crate) async fn prepare_curseforge_modpack_zip(
    bytes: &[u8],
    instance_root: &Path,
    api_key: &str,
    cancel: &CancelToken,
    report: &impl Fn(u32, u32, &str),
) -> Result<PreparedModpackImport, ModpackError> {
    cancel.checkpoint().await?;
    let manifest = read_manifest(bytes)?;
    if manifest.files.len() > MAX_PACK_FILES {
        return Err(ModpackError::Other("Pack has too many files".into()));
    }
    let old_manifest = load_pack_manifest(instance_root)?;
    let mut transaction = PackTransaction::new(instance_root)?;
    let client = http_client()?;
    let cf = CurseForgeClient::new().map_err(|err| ModpackError::Other(err.to_string()))?;
    let file_ids: Vec<u32> = manifest.files.iter().map(|file| file.file_id).collect();
    let mut project_ids: Vec<u32> = manifest.files.iter().map(|file| file.project_id).collect();
    project_ids.sort_unstable();
    project_ids.dedup();
    let (file_meta, mod_meta) = tokio::join!(
        cf.files_batch(&file_ids, api_key), cf.mods_batch(&project_ids, api_key)
    );
    cancel.checkpoint().await?;
    let required_by_file_id = manifest_required_map(&manifest.files);
    let cf = &cf;
    let file_meta = &file_meta;
    let mut metadata = futures::stream::iter(manifest.files.into_iter().map(|file| {
        let project_id = file.project_id;
        let file_id = file.file_id;
        async move {
        cancel.checkpoint().await?;
        let (filename, url, sha1) = if let Some(meta) = file_meta.get(&file_id) {
            meta.clone()
        } else {
            let (filename, sha1) = cf.file_meta(project_id, file_id, api_key).await
                .map_err(|error| ModpackError::Other(format!("Cannot resolve pack file {file_id}: {error}")))?;
            let url = cf.file_download_url(project_id, file_id, api_key).await.ok();
            (filename, url, sha1)
        };
        validate_filename(&filename)?;
        Ok::<_, ModpackError>((project_id, file_id, filename, url, sha1))
    }})).buffer_unordered(METADATA_FALLBACK_CONCURRENCY);
    let mut resolved = Vec::new();
    while let Some(result) = metadata.next().await { resolved.push(result?); }
    let mut filenames = std::collections::HashSet::new();
    for (_, _, filename, _, _) in &resolved {
        if !filenames.insert(filename.to_ascii_lowercase()) {
            return Err(ModpackError::Other(format!("Pack assigns multiple files to {filename}")));
        }
    }
    let total = resolved.len() as u32;
    report(0, total, "");
    let client = &client;
    let old_manifest = &old_manifest;
    let mod_meta = &mod_meta;
    let required_by_file_id = &required_by_file_id;
    let mods = instance_root.join("mods");
    let resources = instance_root.join("resourcepacks");
    let shaders = instance_root.join("shaderpacks");
    let mut downloads = futures::stream::iter(resolved.into_iter().map(|(project_id, file_id, filename, url, sha1)| async move {
        cancel.checkpoint().await?;
        let (name, page) = match mod_meta.get(&project_id) {
            Some((name, slug, _, website)) => (name.clone(), curseforge_file_url(website.as_deref(), slug, file_id)),
            None => (format!("Project {project_id}"), format!("https://www.curseforge.com/minecraft/search?search={project_id}")),
        };
        let mut entry = PackManifestEntry {
            project_id, file_id, name, filename, url: page, sha1,
            required: required_by_file_id.get(&file_id).copied().unwrap_or(true),
            pending: false, retained_files: Vec::new(),
        };
        if let Some(old) = old_manifest.iter().find(|old| old.project_id == project_id && old.file_id == file_id) {
            entry.pending = old.pending;
            entry.retained_files = old.retained_files.clone();
        }
        let ambiguous_old = entry.sha1.is_none() && old_manifest.iter().any(|old|
            old.filename == entry.filename && old.file_id != file_id);
        if !ambiguous_old && matching_existing(&entry, instance_root) {
            entry.pending = false;
            return Ok::<_, ModpackError>((entry, None));
        }
        entry.pending = true;
        let data = if let Some(url) = url {
            match download_bytes_capped_with_retry(client, &url, cancel, MAX_PACK_FILE_BYTES).await {
                Ok(data) if verify_file(&data, entry.sha1.as_deref()).is_ok() => Some(data),
                Err(crate::download::DownloadError::Cancelled) => return Err(crate::download::DownloadError::Cancelled.into()),
                _ => None,
            }
        } else { None };
        if data.is_some() { entry.pending = false; }
        Ok((entry, data))
    })).buffer_unordered(DOWNLOAD_CONCURRENCY);
    let mut entries = Vec::new();
    let mut icons = HashMap::new();
    let mut content_names = HashMap::new();
    let mut project_uids = HashMap::new();
    while let Some(result) = downloads.next().await {
        let (entry, data) = result?;
        cancel.checkpoint().await?;
        if let Some(data) = data {
            let folder = dest_dir_for(&entry.filename, &data, &mods, &resources, &shaders);
            let disabled = old_manifest.iter().filter(|old| old.project_id == entry.project_id)
                .flat_map(|old| std::iter::once(old.filename.as_str()).chain(old.retained_files.iter().map(|file| file.filename.as_str())))
                .filter_map(|filename| find_existing(filename, &mods, &resources, &shaders))
                .any(|path| path.to_string_lossy().ends_with(DISABLED_SUFFIX));
            let filename = if disabled { format!("{}{DISABLED_SUFFIX}", entry.filename) } else { entry.filename.clone() };
            let dest = contained_join(&folder, &filename)?;
            let relative = dest.strip_prefix(instance_root).map_err(|_| ModpackError::Other("Pack destination outside instance".into()))?;
            transaction.stage_pack_file(&relative.to_string_lossy(), &data)?;
        } else if !entry.pending {
            if let Some(path) = find_existing(&entry.filename, &mods, &resources, &shaders) {
                transaction.keep_file(&path)?;
            }
        }
        // Pending targets must not relabel a retained working old filename.
        if !entry.pending {
            content_names.insert(entry.filename.clone(), entry.name.clone());
            project_uids.insert(entry.filename.clone(), format!("curseforge:{}", entry.project_id));
            if let Some((_, _, Some(icon), _)) = mod_meta.get(&entry.project_id) {
                icons.insert(entry.filename.clone(), icon.clone());
            }
        }
        report(entries.len() as u32 + 1, total, &entry.filename);
        entries.push(entry);
    }
    let (overrides_applied, mut override_paths) = if manifest.overrides.is_empty() {
        (0, Vec::new())
    } else {
        transaction.overrides(bytes, &[&manifest.overrides], cancel).await?
    };
    reconcile_pack_files(&mut transaction, old_manifest, &mut entries, instance_root)?;
    for entry in entries.iter().filter(|entry| entry.pending) {
        for file in &entry.retained_files {
            if let Some(path) = find_existing(&file.filename, &mods, &resources, &shaders) {
                transaction.keep_file(&path)?;
            }
        }
    }
    for entry in entries.iter_mut().filter(|entry| !entry.pending) { entry.retained_files.clear(); }
    transaction.reconcile_overrides(&mut override_paths)?;
    transaction.stage(PACK_MANIFEST_FILENAME, &serde_json::to_vec_pretty(&entries)?)?;
    transaction.stage(".pack-overrides-manifest.json", &serde_json::to_vec_pretty(&override_paths)?)?;
    let missing_mods: Vec<_> = entries.iter().filter(|entry| entry.pending).map(|entry| crate::dto::instance::MissingMod {
        project_id: entry.project_id, name: entry.name.clone(), filename: entry.filename.clone(),
        url: entry.url.clone(), sha1: entry.sha1.clone(),
    }).collect();
    let installed = entries.iter().filter(|entry| !entry.pending).count();
    let label = if manifest.name.is_empty() { "CurseForge modpack".to_string() } else { manifest.name };
    let note = if missing_mods.is_empty() { String::new() } else {
        format!(" {} files unavailable or requiring manual download; previous working versions retained where present. Use \"Download missing mods\".", missing_mods.len())
    };
    Ok(PreparedModpackImport { transaction, result: ModpackImportResult {
        message: format!("Imported {label}: {installed} files verified, {overrides_applied} override files applied.{note}"),
        has_skipped: !missing_mods.is_empty(), icons, content_names, project_uids, missing_mods, version_label: None,
    } })
}

fn read_manifest(bytes: &[u8]) -> Result<CurseForgeManifest, ModpackError> {
    read_cf_manifest(bytes)
}

pub fn read_cf_manifest(bytes: &[u8]) -> Result<CurseForgeManifest, ModpackError> {
    if bytes.len() > MAX_PACK_FILE_BYTES {
        return Err(crate::download::DownloadError::TooLarge(MAX_PACK_FILE_BYTES).into());
    }
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;
    if archive.len() > MAX_PACK_FILES {
        return Err(ModpackError::Other("Pack has too many archive entries".into()));
    }
    let mut manifest_file = archive.by_name("manifest.json")?;
    if manifest_file.size() > MAX_INDEX_BYTES {
        return Err(ModpackError::Other("Pack manifest exceeds size limit".into()));
    }
    let expected = manifest_file.size();
    let mut json = String::new();
    manifest_file.by_ref().take(expected + 1).read_to_string(&mut json)?;
    if json.len() as u64 != expected {
        return Err(ModpackError::Other("Pack manifest size disagrees with archive".into()));
    }
    Ok(serde_json::from_str(&json)?)
}


pub fn is_curseforge_modpack_zip(bytes: &[u8]) -> bool {
    let cursor = Cursor::new(bytes);
    if let Ok(mut archive) = zip::ZipArchive::new(cursor) {
        return archive.by_name("manifest.json").is_ok();
    }
    false
}

#[cfg(test)]
mod pack_reconciliation_tests {
    use super::{load_pack_manifest, reconcile_pack_files, save_pack_manifest, PackManifestEntry};
    use crate::download::CancelToken;
    use crate::modpack::PackTransaction;
    use std::fs;

    fn temp_instance_dir(name: &str) -> std::path::PathBuf {
        // Pid-scoped so two overlapping `cargo test` processes can't wipe
        // each other's fixture mid-test (see commands/launch.rs's temp_dir).
        let dir = std::env::temp_dir()
            .join(format!("waybound-pack-reconcile-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mods")).unwrap();
        fs::create_dir_all(dir.join("resourcepacks")).unwrap();
        fs::create_dir_all(dir.join("shaderpacks")).unwrap();
        dir
    }

    fn test_entry(project_id: u32, file_id: u32, filename: &str) -> PackManifestEntry {
        PackManifestEntry {
            project_id,
            file_id,
            name: filename.trim_end_matches(".jar").to_string(),
            filename: filename.to_string(),
            url: format!("https://www.curseforge.com/minecraft/mc-mods/test/download/{file_id}"),
            sha1: None,
            required: true,
            pending: false,
            retained_files: Vec::new(),
        }
    }

    #[test]
    fn manifest_round_trips_through_disk() {
        let dir = temp_instance_dir("roundtrip");
        let entries = vec![test_entry(1, 10, "a.jar"), test_entry(2, 20, "b.jar")];
        save_pack_manifest(&dir, &entries).unwrap();
        let loaded = load_pack_manifest(&dir).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].file_id, 10);
        assert_eq!(loaded[1].filename, "b.jar");
    }

    #[test]
    fn old_sidecar_without_required_reads_back_as_required() {
        // Sidecars written before the `required` field existed must still
        // parse (as required), or an update would lose all reconciliation
        // history the moment it loads them.
        let dir = temp_instance_dir("legacy-sidecar");
        let legacy = r#"[{"project_id":1,"file_id":10,"name":"a","filename":"a.jar","url":"https://example.com","sha1":null}]"#;
        std::fs::write(dir.join(".curseforge-pack-manifest.json"), legacy).unwrap();
        let loaded = load_pack_manifest(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].required);
    }

    #[test]
    fn required_map_honors_explicit_optional() {
        use super::{manifest_required_map, CurseForgeManifestFile};
        // Serde struct has no public constructor in tests; parse like the
        // importer does.
        let files: Vec<CurseForgeManifestFile> = serde_json::from_value(serde_json::json!([
            {"projectID": 1, "fileID": 10},
            {"projectID": 2, "fileID": 20, "required": false},
        ]))
        .unwrap();
        let map = manifest_required_map(&files);
        assert_eq!(map.get(&10), Some(&true));
        assert_eq!(map.get(&20), Some(&false));
        assert_eq!(map.get(&999), None);
    }

    #[test]
    fn missing_manifest_loads_as_empty_not_error() {
        let dir = temp_instance_dir("missing");
        assert!(load_pack_manifest(&dir).unwrap().is_empty());
    }

    #[tokio::test]
    async fn dropped_file_is_removed_kept_file_is_not() {
        let dir = temp_instance_dir("removal");
        let mods_dir = dir.join("mods");
        fs::write(mods_dir.join("dropped.jar"), b"old mod").unwrap();
        fs::write(mods_dir.join("kept.jar"), b"still wanted").unwrap();

        let old_manifest = vec![test_entry(1, 10, "dropped.jar"), test_entry(2, 20, "kept.jar")];
        let mut new_manifest = vec![test_entry(2, 20, "kept.jar")];
        let mut tx = PackTransaction::new(&dir).unwrap();
        tx.keep_file(&mods_dir.join("kept.jar")).unwrap();
        reconcile_pack_files(&mut tx, &old_manifest, &mut new_manifest, &dir).unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();

        assert!(!mods_dir.join("dropped.jar").exists(), "dropped file should be removed");
        assert!(mods_dir.join("kept.jar").exists(), "still-wanted file must survive");
    }

    #[tokio::test]
    async fn user_added_mod_never_tracked_never_touched() {
        let dir = temp_instance_dir("user-added");
        let mods_dir = dir.join("mods");
        // Simulates a mod the user installed via Browse after the pack import —
        // it was never part of any manifest, so it can't appear in `old_manifest`.
        fs::write(mods_dir.join("user-added.jar"), b"manually installed").unwrap();

        let old_manifest = vec![test_entry(1, 10, "something-else.jar")];
        let mut tx = PackTransaction::new(&dir).unwrap();
        reconcile_pack_files(&mut tx, &old_manifest, &mut [], &dir).unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();

        assert!(mods_dir.join("user-added.jar").exists(), "untracked file must never be removed");
    }

    #[test]
    fn pending_missing_mods_excludes_files_already_on_disk() {
        let dir = temp_instance_dir("pending");
        fs::write(dir.join("mods").join("present.jar"), b"already placed").unwrap();
        save_pack_manifest(
            &dir,
            &[test_entry(1, 10, "present.jar"), test_entry(2, 20, "absent.jar")],
        ).unwrap();

        let pending = super::pending_missing_mods(&dir);

        assert_eq!(pending.len(), 1, "only the file not yet on disk should be reported");
        assert_eq!(pending[0].filename, "absent.jar");
    }

    #[test]
    fn pending_missing_mods_excludes_disabled_files() {
        let dir = temp_instance_dir("pending-disabled");
        fs::write(dir.join("mods").join("disabled.jar.disabled"), b"toggled off").unwrap();
        save_pack_manifest(&dir, &[test_entry(1, 10, "disabled.jar")]).unwrap();

        let pending = super::pending_missing_mods(&dir);

        assert!(pending.is_empty(), "a merely-disabled mod must not be reported as missing");
    }

    #[test]
    fn dismissed_mod_stops_appearing_as_pending() {
        let dir = temp_instance_dir("dismiss");
        save_pack_manifest(
            &dir,
            &[test_entry(1, 10, "absent-a.jar"), test_entry(2, 20, "absent-b.jar")],
        ).unwrap();

        super::remove_pack_manifest_entry(&dir, 1).unwrap();
        let pending = super::pending_missing_mods(&dir);

        assert_eq!(pending.len(), 1, "dismissed project should no longer be tracked as missing");
        assert_eq!(pending[0].filename, "absent-b.jar");
    }

    fn zip_with_entries(names: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            for name in names {
                writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    #[test]
    fn shaderpack_zip_is_sniffed_correctly() {
        let shader_zip = zip_with_entries(&["shaders/composite.fsh", "shaders.properties"]);
        assert!(super::sniff_is_shaderpack(&shader_zip));

        let resourcepack_zip = zip_with_entries(&["assets/minecraft/textures/foo.png", "pack.mcmeta"]);
        assert!(!super::sniff_is_shaderpack(&resourcepack_zip));
    }

    #[test]
    fn dest_dir_for_routes_by_extension_and_content() {
        let dir = temp_instance_dir("dest-routing");
        let mods_dir = dir.join("mods");
        let rp_dir = dir.join("resourcepacks");
        let sp_dir = dir.join("shaderpacks");

        assert_eq!(super::dest_dir_for("Foo.jar", b"", &mods_dir, &rp_dir, &sp_dir), mods_dir);

        let shader_zip = zip_with_entries(&["shaders/composite.fsh"]);
        assert_eq!(
            super::dest_dir_for("Shader.zip", &shader_zip, &mods_dir, &rp_dir, &sp_dir),
            sp_dir
        );

        let resourcepack_zip = zip_with_entries(&["assets/minecraft/textures/foo.png"]);
        assert_eq!(
            super::dest_dir_for("Pack.zip", &resourcepack_zip, &mods_dir, &rp_dir, &sp_dir),
            rp_dir
        );
    }

    fn digest(bytes: &[u8]) -> String {
        use sha1::Digest;
        hex::encode(sha1::Sha1::digest(bytes))
    }

    #[tokio::test]
    async fn pending_replacement_retains_old_disabled_bytes_then_manual_commit_replaces_them() {
        let root = tempfile::tempdir().unwrap();
        let mods = root.path().join("mods");
        fs::create_dir(&mods).unwrap();
        fs::write(mods.join("old.jar.disabled"), b"working").unwrap();
        let mut old = test_entry(1, 10, "old.jar");
        old.sha1 = Some(digest(b"working"));
        let mut next = test_entry(1, 20, "new.jar");
        next.sha1 = Some(digest(b"replacement"));
        next.pending = true;
        let mut entries = vec![next];
        let mut tx = PackTransaction::new(root.path()).unwrap();
        reconcile_pack_files(&mut tx, &[old], &mut entries, root.path()).unwrap();
        tx.stage(super::PACK_MANIFEST_FILENAME, &serde_json::to_vec(&entries).unwrap()).unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();
        assert_eq!(fs::read(mods.join("old.jar.disabled")).unwrap(), b"working");
        assert_eq!(super::pending_missing_mods(root.path()).len(), 1);
        let stage = root.path().join("manual.tmp");
        fs::write(&stage, b"replacement").unwrap();
        let (tx, target) = super::prepare_manual_pack_replacement(root.path(), 1, "new.jar", &stage).unwrap().unwrap();
        assert_eq!(target, mods.join("new.jar.disabled"));
        tx.commit(&CancelToken::new()).await.unwrap();
        assert!(!mods.join("old.jar.disabled").exists());
        assert!(!mods.join("new.jar").exists());
        assert_eq!(fs::read(mods.join("new.jar.disabled")).unwrap(), b"replacement");
        assert!(super::pending_missing_mods(root.path()).is_empty());
        let receipt = load_pack_manifest(root.path()).unwrap();
        assert!(!receipt[0].pending);
        assert!(receipt[0].retained_files.is_empty());
    }

    #[tokio::test]
    async fn same_filename_manual_replacement_and_receipt_roll_back_on_publish_failure() {
        let root = tempfile::tempdir().unwrap();
        let mods = root.path().join("mods");
        fs::create_dir(&mods).unwrap();
        fs::write(mods.join("same.jar.disabled"), b"working").unwrap();
        let mut receipt = test_entry(1, 20, "same.jar");
        receipt.pending = true;
        receipt.sha1 = Some(digest(b"replacement"));
        receipt.retained_files.push(super::RetainedPackFile {
            filename: "same.jar".into(), sha1: Some(digest(b"working")),
        });
        save_pack_manifest(root.path(), &[receipt]).unwrap();
        let before = fs::read(root.path().join(super::PACK_MANIFEST_FILENAME)).unwrap();
        let stage = root.path().join("manual.tmp");
        fs::write(&stage, b"replacement").unwrap();
        let (tx, _) = super::prepare_manual_pack_replacement(root.path(), 1, "same.jar", &stage).unwrap().unwrap();
        assert!(tx.commit_with(&CancelToken::new(), || Err(super::ModpackError::Other("publish failed".into()))).await.is_err());
        assert_eq!(fs::read(mods.join("same.jar.disabled")).unwrap(), b"working");
        assert!(!mods.join("same.jar").exists());
        assert_eq!(fs::read(root.path().join(super::PACK_MANIFEST_FILENAME)).unwrap(), before);
    }

    #[tokio::test]
    async fn hashless_pending_receipt_requires_manual_completion_and_records_observed_hash() {
        let root = tempfile::tempdir().unwrap();
        let mods = root.path().join("mods");
        fs::create_dir(&mods).unwrap();
        fs::write(mods.join("same.jar"), b"working").unwrap();
        let mut receipt = test_entry(1, 20, "same.jar");
        receipt.pending = true;
        receipt.retained_files.push(super::RetainedPackFile {
            filename: "same.jar".into(), sha1: Some(digest(b"working")),
        });
        save_pack_manifest(root.path(), &[receipt]).unwrap();
        let stage = root.path().join("manual.tmp");
        fs::write(&stage, b"working").unwrap();
        assert!(super::prepare_manual_pack_replacement(root.path(), 1, "same.jar", &stage).is_err());
        fs::write(mods.join("same.jar"), b"unverified change").unwrap();
        assert_eq!(super::pending_missing_mods(root.path()).len(), 1);
        fs::write(mods.join("same.jar"), b"working").unwrap();
        fs::write(&stage, b"replacement").unwrap();
        let (tx, _) = super::prepare_manual_pack_replacement(root.path(), 1, "same.jar", &stage).unwrap().unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();
        assert_eq!(fs::read(mods.join("same.jar")).unwrap(), b"replacement");
        assert_eq!(load_pack_manifest(root.path()).unwrap()[0].sha1.as_deref(), Some(digest(b"replacement").as_str()));
        assert!(super::pending_missing_mods(root.path()).is_empty());
    }

    #[tokio::test]
    async fn verified_existing_filename_reassigned_to_new_project_survives_old_receipt_drop() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("mods")).unwrap();
        let path = root.path().join("mods/shared.jar");
        fs::write(&path, b"verified").unwrap();
        let mut old = test_entry(1, 10, "shared.jar");
        old.sha1 = Some(digest(b"verified"));
        let mut next = test_entry(2, 20, "shared.jar");
        next.sha1 = old.sha1.clone();
        assert!(super::matching_existing(&next, root.path()));
        let mut tx = PackTransaction::new(root.path()).unwrap();
        tx.keep_file(&path).unwrap();
        reconcile_pack_files(&mut tx, &[old], &mut [next], root.path()).unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();
        assert_eq!(fs::read(path).unwrap(), b"verified");
    }

    #[tokio::test]
    async fn same_filename_content_folder_switch_preserves_disabled_state_without_leaving_duplicate() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("resourcepacks")).unwrap();
        let old_path = root.path().join("resourcepacks/shared.zip.disabled");
        fs::write(&old_path, b"old resources").unwrap();
        let shader_bytes = zip_with_entries(&["shaders/composite.fsh"]);
        let mut receipt = test_entry(1, 20, "shared.zip");
        receipt.pending = true;
        receipt.sha1 = Some(digest(&shader_bytes));
        receipt.retained_files.push(super::RetainedPackFile {
            filename: "shared.zip".into(), sha1: Some(digest(b"old resources")),
        });
        save_pack_manifest(root.path(), &[receipt]).unwrap();
        let stage = root.path().join("manual.tmp");
        fs::write(&stage, &shader_bytes).unwrap();
        let (tx, _) = super::prepare_manual_pack_replacement(root.path(), 1, "shared.zip", &stage).unwrap().unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();
        assert!(!old_path.exists());
        assert_eq!(fs::read(root.path().join("shaderpacks/shared.zip.disabled")).unwrap(), shader_bytes);
        assert!(super::pending_missing_mods(root.path()).is_empty());
    }

    #[tokio::test]
    async fn single_pack_mod_update_persists_manual_relation_and_keeps_working_version_until_publish() {
        let root = tempfile::tempdir().unwrap();
        let mods = root.path().join("mods");
        fs::create_dir(&mods).unwrap();
        let previous = mods.join("Vestiges1.7.7.jar.disabled");
        fs::write(&previous, b"working old version").unwrap();
        let old = test_entry(971973, 8539603, "Vestiges1.7.7.jar");
        save_pack_manifest(root.path(), &[old]).unwrap();
        let missing = crate::dto::instance::MissingMod {
            project_id: 971973, name: "Vestiges".into(), filename: "Vestiges1.7.8.jar".into(),
            url: "https://www.curseforge.com/minecraft/mc-mods/vestiges/download/8881710".into(),
            sha1: Some(digest(b"verified replacement")),
        };
        let tx = super::prepare_pending_pack_update(root.path(), &missing, 8881710, &previous).unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();
        assert_eq!(fs::read(&previous).unwrap(), b"working old version");
        let persisted = load_pack_manifest(root.path()).unwrap();
        assert_eq!(persisted[0].file_id, 8881710);
        assert_eq!(persisted[0].retained_files[0].filename, "Vestiges1.7.7.jar");
        assert_eq!(super::pending_missing_mods(root.path())[0].filename, missing.filename);
        let before = fs::read(root.path().join(super::PACK_MANIFEST_FILENAME)).unwrap();
        let stage = root.path().join("manual.tmp");
        fs::write(&stage, b"verified replacement").unwrap();
        let (tx, target) = super::prepare_manual_pack_replacement(root.path(), 971973, &missing.filename, &stage).unwrap().unwrap();
        assert_eq!(target, mods.join("Vestiges1.7.8.jar.disabled"));
        assert!(tx.commit_with(&CancelToken::new(), || Err(super::ModpackError::Other("DB failed".into()))).await.is_err());
        assert_eq!(fs::read(&previous).unwrap(), b"working old version");
        assert!(!target.exists());
        assert_eq!(fs::read(root.path().join(super::PACK_MANIFEST_FILENAME)).unwrap(), before);
        fs::write(&stage, b"verified replacement").unwrap();
        let (tx, target) = super::prepare_manual_pack_replacement(root.path(), 971973, &missing.filename, &stage).unwrap().unwrap();
        tx.commit_with(&CancelToken::new(), || Ok(())).await.unwrap();
        assert!(!previous.exists());
        assert_eq!(fs::read(target).unwrap(), b"verified replacement");
        assert!(super::pending_missing_mods(root.path()).is_empty());
    }

    #[tokio::test]
    async fn direct_pack_update_publishes_receipt_and_exact_disabled_path_with_database_rollback() {
        use crate::db::Database;
        use crate::dto::{ModOrigin, ModSource};

        let root = tempfile::tempdir().unwrap();
        let mods = root.path().join("mods");
        fs::create_dir(&mods).unwrap();
        let previous = mods.join("Vestiges1.7.7.jar.disabled");
        let sibling = mods.join("Vestiges1.7.7.jar");
        let target = mods.join("Vestiges1.7.8.jar.disabled");
        fs::write(&previous, b"old pack bytes").unwrap();
        fs::write(&sibling, b"independent enabled sibling").unwrap();
        let mut old = test_entry(971973, 8539603, "Vestiges1.7.7.jar");
        old.sha1 = Some(digest(b"old pack bytes"));
        old.required = false;
        save_pack_manifest(root.path(), &[old]).unwrap();
        let original_receipt = fs::read(root.path().join(super::PACK_MANIFEST_FILENAME)).unwrap();
        let db = Database::open_at(&root.path().join("library.db")).unwrap();
        db.conn().unwrap().execute(
            "INSERT INTO instances (id,name,minecraft_version,loader,root_path,created_at)
             VALUES ('fixture','Fixture','1.20.1','forge',?1,0)",
            [root.path().display().to_string()],
        ).unwrap();
        db.insert_instance_mod("fixture", "curseforge:971973", "Vestiges", ModSource::Curseforge,
            "Vestiges1.7.7.jar.disabled", &previous.display().to_string(), None, ModOrigin::Pack).unwrap();
        let item = crate::dto::instance::MissingMod {
            project_id: 971973, name: "Vestiges".into(), filename: "Vestiges1.7.8.jar".into(),
            url: "https://www.curseforge.com/minecraft/mc-mods/vestiges/download/8881710".into(),
            sha1: None,
        };
        let replacement = b"verified new pack bytes";

        for allow_publish in [false, true] {
            db.conn().unwrap().execute_batch(if allow_publish {
                "PRAGMA query_only = OFF"
            } else {
                "PRAGMA query_only = ON"
            }).unwrap();
            let mut tx = PackTransaction::new(root.path()).unwrap();
            tx.stage("mods/Vestiges1.7.8.jar.disabled", replacement).unwrap();
            tx.remove(&previous).unwrap();
            super::stage_completed_pack_update(&mut tx, root.path(), &item, 8881710, digest(replacement)).unwrap();
            let result = tx.commit_with(&CancelToken::new(), || {
                db.insert_instance_mod("fixture", "curseforge:971973", "Vestiges", ModSource::Curseforge,
                    "Vestiges1.7.8.jar.disabled", &target.display().to_string(), None, ModOrigin::Pack)
                    .map_err(|error| super::ModpackError::Other(error.to_string()))?;
                Ok(())
            }).await;
            let tracked = db.get_instance_mod("fixture", "curseforge:971973").unwrap().unwrap();
            assert_eq!(fs::read(&sibling).unwrap(), b"independent enabled sibling");
            if allow_publish {
                result.unwrap();
                assert!(!previous.exists());
                assert_eq!(fs::read(&target).unwrap(), replacement);
                assert_eq!(tracked.0.file_name, "Vestiges1.7.8.jar.disabled");
                assert_eq!(tracked.1, target.display().to_string());
                let receipt = load_pack_manifest(root.path()).unwrap().remove(0);
                assert_eq!(receipt.file_id, 8881710);
                assert_eq!(receipt.filename, item.filename);
                assert_eq!(receipt.sha1.as_deref(), Some(digest(replacement).as_str()));
                assert!(!receipt.pending);
                assert!(!receipt.required);
                assert!(receipt.retained_files.is_empty());
            } else {
                assert!(result.is_err());
                assert_eq!(fs::read(&previous).unwrap(), b"old pack bytes");
                assert!(!target.exists());
                assert_eq!(tracked.0.file_name, "Vestiges1.7.7.jar.disabled");
                assert_eq!(fs::read(root.path().join(super::PACK_MANIFEST_FILENAME)).unwrap(), original_receipt);
            }
        }
    }
}
