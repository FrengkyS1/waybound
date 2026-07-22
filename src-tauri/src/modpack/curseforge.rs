use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};

use super::{ModpackError, ModpackImportResult};
use crate::commands::content::DISABLED_SUFFIX;
use crate::download::{download_bytes_with_retry, http_client, safe_join, CancelToken, DOWNLOAD_CONCURRENCY};
use crate::dto::{ContentType, ModLoader, ModSearchQuery, SortIndex};
use crate::sources::curseforge::CurseForgeClient;
use crate::sources::modrinth::ModrinthClient;

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
        safe_join(dir, filename)
            .ok()
            .filter(|p| p.exists())
            .or_else(|| safe_join(dir, &disabled_name).ok().filter(|p| p.exists()))
    })
}

fn already_on_disk(filename: &str, mods_dir: &Path, resourcepacks_dir: &Path, shaderpacks_dir: &Path) -> bool {
    find_existing(filename, mods_dir, resourcepacks_dir, shaderpacks_dir).is_some()
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
}

const PACK_MANIFEST_FILENAME: &str = ".curseforge-pack-manifest.json";

fn load_pack_manifest(instance_root: &Path) -> Vec<PackManifestEntry> {
    std::fs::read_to_string(instance_root.join(PACK_MANIFEST_FILENAME))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_pack_manifest(instance_root: &Path, entries: &[PackManifestEntry]) {
    if let Ok(json) = serde_json::to_string_pretty(entries) {
        let _ = std::fs::write(instance_root.join(PACK_MANIFEST_FILENAME), json);
    }
}

/// Drops one project from the pack's tracked manifest — for a mod the user
/// has decided not to grab (its author blocks automatic download and the
/// user doesn't want it manually either). Membership in `pending_missing_mods`
/// is decided purely by "does the manifest still list this file," so without
/// this, a mod the user explicitly opted out of nags forever on every
/// restart, indistinguishable from one they just haven't gotten to yet.
pub fn remove_pack_manifest_entry(instance_root: &Path, project_id: u32) {
    let entries = load_pack_manifest(instance_root);
    if !entries.iter().any(|e| e.project_id == project_id) {
        return;
    }
    let kept: Vec<PackManifestEntry> = entries.into_iter().filter(|e| e.project_id != project_id).collect();
    save_pack_manifest(instance_root, &kept);
}

/// Removes files the pack dropped since the last import of this same
/// instance — the ones this update's manifest no longer lists by file id.
/// Never touches anything not in `old_manifest`, so a mod the user added
/// afterward through Browse is never a candidate here in the first place.
fn remove_files_pack_dropped(
    old_manifest: &[PackManifestEntry],
    new_file_ids: &std::collections::HashSet<u32>,
    mods_dir: &Path,
    resourcepacks_dir: &Path,
    shaderpacks_dir: &Path,
) {
    for entry in old_manifest {
        if new_file_ids.contains(&entry.file_id) {
            continue;
        }
        if let Some(path) = find_existing(&entry.filename, mods_dir, resourcepacks_dir, shaderpacks_dir) {
            let _ = std::fs::remove_file(path);
        }
    }
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
    let mods_dir = instance_root.join("mods");
    let resourcepacks_dir = instance_root.join("resourcepacks");
    let shaderpacks_dir = instance_root.join("shaderpacks");
    load_pack_manifest(instance_root)
        .into_iter()
        .filter(|entry| !already_on_disk(&entry.filename, &mods_dir, &resourcepacks_dir, &shaderpacks_dir))
        .map(|entry| crate::dto::instance::MissingMod {
            project_id: entry.project_id,
            name: entry.name,
            filename: entry.filename,
            url: entry.url,
            sha1: entry.sha1,
        })
        .collect()
}

/// Loose enough to match "Entity Culling Fabric/Forge" (CurseForge's title)
/// against "EntityCulling" (Modrinth's), strict enough that two unrelated
/// mods essentially never collide — punctuation/case/whitespace stripped,
/// then one name must fully contain the other.
fn normalize_mod_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn names_plausibly_match(a: &str, b: &str) -> bool {
    let na = normalize_mod_name(a);
    let nb = normalize_mod_name(b);
    !na.is_empty() && !nb.is_empty() && (na.contains(&nb) || nb.contains(&na))
}

/// CurseForge titles routinely carry loader/platform decoration the base
/// mod's Modrinth listing never has — "Entity Culling Fabric/Forge",
/// "(ARCHIVE) Faster Random", "Better World Loading ([Neo]Forge)". Verified
/// against Modrinth's search directly: sent as-is, every one of those
/// returns zero hits; stripped down to "Entity Culling" / "Faster Random" /
/// "Better World Loading", each finds the real project as the top result.
/// Modrinth's search apparently doesn't do partial/fuzzy matching well
/// against a query cluttered with extra tokens, so this strips parenthesized
/// segments and any word that's purely loader names (plain or slash-joined).
fn search_query_from_cf_name(name: &str) -> String {
    let mut without_brackets = String::new();
    let mut depth = 0i32;
    for c in name.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => without_brackets.push(c),
            _ => {}
        }
    }

    const LOADER_WORDS: [&str; 4] = ["fabric", "forge", "neoforge", "quilt"];
    let cleaned = without_brackets
        .split_whitespace()
        .filter(|word| {
            let normalized = word.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_ascii_lowercase();
            !normalized.split('/').all(|part| LOADER_WORDS.contains(&part))
        })
        .collect::<Vec<_>>()
        .join(" ");

    if cleaned.trim().is_empty() {
        name.to_string()
    } else {
        cleaned
    }
}

/// Looks for the same mod on Modrinth as a legitimate alternate source when
/// CurseForge won't hand out a download for it — Modrinth is a fully
/// separate, openly-licensed platform with no equivalent "third-party
/// download disabled" flag, so this isn't a workaround for CurseForge's
/// restriction, it's checking whether the author also publishes there.
///
/// Deliberately strict: the name has to plausibly match, and the returned
/// file must be for the *exact* Minecraft version and loader this instance
/// is running — no falling back to "closest available," since a
/// wrong-version substitute silently dropped into a modpack is worse than
/// just telling the user to grab it manually.
async fn find_modrinth_replacement(
    modrinth: &ModrinthClient,
    mc_version: &str,
    loader: ModLoader,
    mod_name: &str,
) -> Option<crate::instances::ResolvedDownload> {
    let query = ModSearchQuery {
        query: search_query_from_cf_name(mod_name),
        content_type: Some(ContentType::Mod),
        loader: None,
        sort: SortIndex::Relevance,
        offset: 0,
        limit: 5,
    };
    let results = modrinth.search(&query).await.ok()?;
    for hit in results.hits.iter().filter(|h| names_plausibly_match(&h.name, mod_name)) {
        let Some(project_id) = hit.modrinth_id.as_deref() else { continue };
        if let Ok(download) = modrinth
            .query_versions(project_id, Some(mc_version), Some(loader.as_modrinth()))
            .await
        {
            crate::activity::append_log(
                &format!(
                    "CF modpack import: found \"{mod_name}\" on Modrinth as \"{}\" (project {project_id}), using it instead of CurseForge",
                    hit.name
                ),
                "debug",
                None,
            );
            return Some(download);
        }
    }
    None
}

#[derive(Debug, Deserialize)]
pub struct CurseForgeManifest {
    #[serde(default)]
    pub name: String,
    pub files: Vec<CurseForgeManifestFile>,
    #[serde(default)]
    overrides: String,
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

pub async fn import_curseforge_modpack_zip(
    bytes: &[u8],
    instance_root: &Path,
    api_key: &str,
    modrinth: &ModrinthClient,
    mc_version: &str,
    loader: ModLoader,
    cancel: &CancelToken,
    report: &impl Fn(u32, u32, &str),
) -> Result<ModpackImportResult, ModpackError> {
    let manifest = read_manifest(bytes)?;
    let client = http_client()?;
    let client = &client;
    let cf = CurseForgeClient::new().map_err(|err| ModpackError::Other(err.to_string()))?;
    let cf = &cf;

    let mods_dir = instance_root.join("mods");
    std::fs::create_dir_all(&mods_dir)?;
    let mods_dir = &mods_dir;
    let resourcepacks_dir = instance_root.join("resourcepacks");
    std::fs::create_dir_all(&resourcepacks_dir)?;
    let resourcepacks_dir = &resourcepacks_dir;
    let shaderpacks_dir = instance_root.join("shaderpacks");
    std::fs::create_dir_all(&shaderpacks_dir)?;
    let shaderpacks_dir = &shaderpacks_dir;

    // Same concurrency fix as the Modrinth importer: this used to resolve and
    // download each mod strictly one at a time (three sequential round trips
    // per entry — download URL, download, then filename), which serializes
    // to minutes of pure network wait for a few-hundred-mod pack.
    let jobs: Vec<(u32, u32)> = manifest
        .files
        .iter()
        .filter(|f| f.required)
        .map(|f| (f.project_id, f.file_id))
        .collect();

    // One batch call for every file's name + download URL, instead of two
    // per-file API calls (name, download-url) times a few hundred files —
    // that per-file burst is what was tripping CurseForge's rate limit on
    // install. Only a file the batch didn't resolve (missing, or a null
    // downloadUrl for a distribution-restricted file) falls back to an
    // individual lookup, resolved below at low concurrency before any
    // downloading starts.
    let file_ids: Vec<u32> = jobs.iter().map(|(_, file_id)| *file_id).collect();
    let project_ids: Vec<u32> = {
        let mut ids: Vec<u32> = jobs.iter().map(|(project_id, _)| *project_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };

    // Fetched alongside files_batch (not just for the skipped-mods message
    // built later) so every resolved mod gets its CurseForge icon recorded —
    // modpack installs write straight to disk without ever touching a
    // ModSummary, so without this no modpack-installed mod had an icon on
    // record unless its own jar happened to embed one.
    let (file_meta, mod_meta) =
        tokio::join!(cf.files_batch(&file_ids, api_key), cf.mods_batch(&project_ids, api_key));

    // file_id -> (project_id, filename, url, sha1). `url: None` means the
    // file is already sitting in mods/ (re-running "Add content" on a pack
    // that partly installed shouldn't re-fetch — and re-checking a
    // downloaded file skips both the CDN GET and, for a file that needed the
    // fallback lookup below, the rate-limit-prone API call too).
    let mut resolved: HashMap<u32, (u32, String, Option<String>, Option<String>)> = HashMap::new();
    // Files the batch itself already said have no download URL — CurseForge
    // told us this directly, so an individual /download-url retry for these
    // isn't "maybe it clears up," it's the same rejection every time (the
    // author disabled third-party distribution). Retrying it repeatedly
    // with backoff was purely wasted time — same 13 files, every run.
    let mut restricted: Vec<(u32, u32, String, Option<String>)> = Vec::new();
    // Files missing from the batch response entirely (rare) — no direct
    // signal either way, worth a real attempt with retry.
    let mut needs_fallback: Vec<(u32, u32)> = Vec::new();
    for (project_id, file_id) in &jobs {
        match file_meta.get(file_id) {
            Some((name, Some(url), sha1)) => {
                let already_present = already_on_disk(name, mods_dir, resourcepacks_dir, shaderpacks_dir);
                resolved.insert(
                    *file_id,
                    (*project_id, name.clone(), (!already_present).then(|| url.clone()), sha1.clone()),
                );
            }
            Some((name, None, sha1)) => restricted.push((*project_id, *file_id, name.clone(), sha1.clone())),
            None => needs_fallback.push((*project_id, *file_id)),
        }
    }

    // Files we ultimately can't fetch — reported to the user with a
    // manual-download link instead of failing the whole pack over a handful
    // of author-restricted mods. Filename, file_id and sha1 travel alongside
    // the project id so a manually-downloaded replacement can later be
    // matched by content hash (filename as fallback), and the manual-
    // download link can point at this exact file instead of the mod's
    // project page (which may list many versions across MC versions/loaders).
    let mut skipped_files: Vec<(u32, u32, String, Option<String>)> = Vec::new();

    for (project_id, file_id, name, sha1) in restricted {
        if already_on_disk(&name, mods_dir, resourcepacks_dir, shaderpacks_dir) {
            resolved.insert(file_id, (project_id, name, None, sha1));
        } else {
            skipped_files.push((project_id, file_id, name, sha1));
        }
    }

    let mut fallback_stream = futures::stream::iter(needs_fallback.into_iter().map(
        |(project_id, file_id)| async move {
            if cancel.is_cancelled() {
                return (project_id, file_id, String::new(), None, None);
            }
            let (filename, sha1) = cf
                .file_meta(project_id, file_id, api_key)
                .await
                .unwrap_or_else(|_| (format!("mod-{project_id}-{file_id}.jar"), None));
            if already_on_disk(&filename, mods_dir, resourcepacks_dir, shaderpacks_dir) {
                return (project_id, file_id, filename, sha1, Some(None));
            }
            match cf.file_download_url(project_id, file_id, api_key).await {
                Ok(url) => (project_id, file_id, filename, sha1, Some(Some(url))),
                Err(err) => {
                    crate::activity::append_log(
                        &format!(
                            "CF modpack import: file lookup failed for project={project_id} file={file_id}: {err}"
                        ),
                        "debug",
                        None,
                    );
                    (project_id, file_id, filename, sha1, None)
                }
            }
        },
    ))
    .buffer_unordered(METADATA_FALLBACK_CONCURRENCY);

    while let Some((project_id, file_id, filename, sha1, outcome)) = fallback_stream.next().await {
        match outcome {
            Some(url) => {
                resolved.insert(file_id, (project_id, filename, url, sha1));
            }
            None => skipped_files.push((project_id, file_id, filename, sha1)),
        }
    }
    if cancel.is_cancelled() {
        return Err(ModpackError::from(crate::download::DownloadError::Cancelled));
    }

    // One pass over every resolved file (freshly downloaded or already on
    // disk) to record its CurseForge icon and project name, keyed by
    // filename since that's how `sync_mods_folder` matches files back to DB
    // rows after the fact — and, for resource/shader packs (no embedded name
    // convention of their own), the only source of a real display name at
    // all.
    let mut icons: HashMap<String, String> = HashMap::new();
    let mut content_names: HashMap<String, String> = HashMap::new();
    let mut project_uids: HashMap<String, String> = HashMap::new();
    for (project_id, filename, _, _) in resolved.values() {
        project_uids.insert(filename.clone(), format!("curseforge:{project_id}"));
        if let Some((name, _, icon, _)) = mod_meta.get(project_id) {
            content_names.insert(filename.clone(), name.clone());
            if let Some(icon) = icon {
                icons.insert(filename.clone(), icon.clone());
            }
        }
    }

    let total = resolved.len() as u32;
    let mut files_installed = 0u32;
    let mut processed = 0u32;
    report(0, total, "");

    let file_ids_to_download: Vec<u32> = resolved.keys().copied().collect();
    let resolved = &resolved;
    let mut stream = futures::stream::iter(file_ids_to_download.into_iter().map(|file_id| async move {
        if cancel.is_cancelled() {
            return Err(ModpackError::from(crate::download::DownloadError::Cancelled));
        }
        let (project_id, filename, url, sha1) = resolved.get(&file_id).expect("resolved during metadata phase");
        let Some(url) = url else {
            // Already on disk from a previous run — nothing to do.
            return Ok((None, filename.clone()));
        };
        match download_bytes_with_retry(client, url, cancel).await {
            Ok(data) => {
                let dest = dest_dir_for(filename, &data, mods_dir, resourcepacks_dir, shaderpacks_dir);
                std::fs::write(safe_join(&dest, filename)?, data)?;
                Ok((None, filename.clone()))
            }
            Err(crate::download::DownloadError::Cancelled) => {
                Err(ModpackError::from(crate::download::DownloadError::Cancelled))
            }
            // A download that still fails after the built-in retry (e.g. a
            // dead link) is treated the same as an unresolvable file: skip
            // it and keep going, rather than losing the rest of the pack.
            Err(_) => Ok((Some((*project_id, file_id, sha1.clone())), filename.clone())),
        }
    }))
    .buffer_unordered(DOWNLOAD_CONCURRENCY);

    while let Some(result) = stream.next().await {
        let (skip, filename) = result?;
        match skip {
            Some((project_id, file_id, sha1)) => skipped_files.push((project_id, file_id, filename.clone(), sha1)),
            None => files_installed += 1,
        }
        processed += 1;
        report(processed, total, &filename);
    }

    // Runs on a blocking-pool thread (large packs' overrides can be many MB
    // of resource packs/configs) so it doesn't stall the async runtime, and
    // checks `cancel` periodically like the download loop above it — this
    // sync zip-extraction loop previously had no cancellation awareness at
    // all despite CancelToken being passed in.
    let overrides_applied = if manifest.overrides.is_empty() {
        0
    } else {
        let owned_bytes = bytes.to_vec();
        let owned_root = instance_root.to_path_buf();
        let owned_prefix = manifest.overrides.clone();
        let owned_cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            extract_overrides(&owned_bytes, &owned_root, &owned_prefix, &owned_cancel)
        })
        .await
        .map_err(|e| ModpackError::Other(format!("override extraction task panicked: {e}")))??
    };

    let label = if manifest.name.is_empty() {
        "CurseForge modpack".to_string()
    } else {
        manifest.name
    };

    skipped_files.sort_unstable_by_key(|(id, _, _, _)| *id);
    skipped_files.dedup_by_key(|(id, _, _, _)| *id);

    // CurseForge won't hand these out at all — before giving up, check
    // whether the same mod is also published on Modrinth (a separate,
    // openly-licensed platform with no equivalent restriction) for this
    // exact Minecraft version and loader. Only an exact-version match
    // counts; anything looser risks silently swapping in an incompatible
    // file, so a miss here still falls through to the manual-download list.
    let mut still_skipped: Vec<(u32, u32, String, Option<String>)> = Vec::new();
    let mut substituted: Vec<String> = Vec::new();
    for (project_id, file_id, filename, sha1) in &skipped_files {
        let Some((name, _slug, _icon, _website_url)) = mod_meta.get(project_id) else {
            still_skipped.push((*project_id, *file_id, filename.clone(), sha1.clone()));
            continue;
        };
        let Some(download) = find_modrinth_replacement(modrinth, mc_version, loader, name).await else {
            still_skipped.push((*project_id, *file_id, filename.clone(), sha1.clone()));
            continue;
        };
        if cancel.is_cancelled() {
            return Err(ModpackError::from(crate::download::DownloadError::Cancelled));
        }
        match download_bytes_with_retry(client, &download.url, cancel).await {
            Ok(data) => {
                let dest = dest_dir_for(&download.filename, &data, mods_dir, resourcepacks_dir, shaderpacks_dir);
                std::fs::write(safe_join(&dest, &download.filename)?, data)?;
                files_installed += 1;
                substituted.push(name.clone());
            }
            Err(_) => still_skipped.push((*project_id, *file_id, filename.clone(), sha1.clone())),
        }
    }

    // CurseForge's exact file-download page (or, lacking a slug, a search
    // link) for each file the user still has to grab themselves. Pointing at
    // `/download/{file_id}` rather than the bare project page lands the user
    // straight on the specific file already matched to this pack's MC
    // version and loader, instead of the project's full file list where
    // picking the wrong version/loader is an easy mistake. `filename` is
    // CurseForge's own exact name for it and `sha1` (when reported) is the
    // reliable match, so whatever the user saves from that page can be
    // placed with no fuzzy guessing even if the browser renamed it.
    let missing_mods: Vec<crate::dto::instance::MissingMod> = still_skipped
        .iter()
        .map(|(project_id, file_id, filename, sha1)| match mod_meta.get(project_id) {
            Some((name, slug, _icon, website_url)) => crate::dto::instance::MissingMod {
                project_id: *project_id,
                name: name.clone(),
                filename: filename.clone(),
                url: curseforge_file_url(website_url.as_deref(), slug, *file_id),
                sha1: sha1.clone(),
            },
            None => crate::dto::instance::MissingMod {
                project_id: *project_id,
                name: format!("Project {project_id}"),
                filename: filename.clone(),
                url: format!("https://www.curseforge.com/minecraft/search?search={project_id}"),
                sha1: sha1.clone(),
            },
        })
        .collect();

    let mut skipped_note = String::new();
    if !substituted.is_empty() {
        skipped_note.push_str(&format!(
            "\n\n{} mod(s) weren't available from CurseForge (author disabled third-party \
             downloads) but were found on Modrinth for this exact Minecraft version and loader, \
             and installed from there instead: {}",
            substituted.len(),
            substituted.join(", ")
        ));
    }
    let has_skipped = !missing_mods.is_empty();
    if has_skipped {
        skipped_note.push_str(&format!(
            "\n\n{} mod(s) have third-party downloads disabled by their author on CurseForge \
             and aren't published on Modrinth either — click \"Download missing mods\" to grab \
             them yourself and Waybound will place them automatically:",
            missing_mods.len()
        ));
        for mod_ in &missing_mods {
            skipped_note.push_str(&format!("\n  - {}: {}", mod_.name, mod_.url));
        }
    }

    // Reconciliation: if this instance was already imported from a CurseForge
    // pack before, remove whatever that import's manifest listed but this
    // one's manifest (by file id) no longer does — an update dropping a mod,
    // not a downgrade of one still-listed file to another. Never touches
    // anything outside that prior manifest, so mods the user added
    // afterward through Browse are never candidates for removal here.
    //
    // Deliberately deferred to here (everything above has already resolved,
    // downloaded, or given up and recorded a manual-download entry for every
    // file the new manifest wants) rather than running right after computing
    // `file_ids` — deleting eagerly, before knowing whether each dropped
    // file's replacement actually downloaded, meant a single flaky CDN call
    // could leave a working mod deleted with nothing to replace it, and any
    // hard error on the way here would return early with the sidecar never
    // updated to match what was actually deleted.
    let old_pack_manifest = load_pack_manifest(instance_root);
    if !old_pack_manifest.is_empty() {
        let new_file_ids: std::collections::HashSet<u32> = file_ids.iter().copied().collect();
        remove_files_pack_dropped(&old_pack_manifest, &new_file_ids, mods_dir, resourcepacks_dir, shaderpacks_dir);
    }

    // Persist this import's manifest for the *next* update to reconcile
    // against. Covers files placed on disk (`resolved`) and ones still
    // pending a manual download (`still_skipped`/`missing_mods`) alike, so a
    // mod the user manually places later is still tracked correctly if the
    // pack drops it in a subsequent update.
    let new_pack_manifest: Vec<PackManifestEntry> = resolved
        .iter()
        .map(|(file_id, (project_id, filename, _, sha1))| {
            let (name, url) = match mod_meta.get(project_id) {
                Some((name, slug, _icon, website_url)) => {
                    (name.clone(), curseforge_file_url(website_url.as_deref(), slug, *file_id))
                }
                None => (
                    format!("Project {project_id}"),
                    format!("https://www.curseforge.com/minecraft/search?search={project_id}"),
                ),
            };
            PackManifestEntry {
                project_id: *project_id,
                file_id: *file_id,
                name,
                filename: filename.clone(),
                url,
                sha1: sha1.clone(),
            }
        })
        // `still_skipped` and `missing_mods` were built by mapping over the
        // same slice in the same order, so zipping them pairs each entry
        // with the MissingMod already carrying its resolved name/url —
        // avoiding a second mod_meta lookup for the exact same data.
        .chain(still_skipped.iter().zip(missing_mods.iter()).map(|((project_id, file_id, filename, sha1), mm)| {
            PackManifestEntry {
                project_id: *project_id,
                file_id: *file_id,
                name: mm.name.clone(),
                filename: filename.clone(),
                url: mm.url.clone(),
                sha1: sha1.clone(),
            }
        }))
        .collect();
    save_pack_manifest(instance_root, &new_pack_manifest);

    Ok(ModpackImportResult {
        message: format!(
            "Imported {label}: {files_installed} mods downloaded, {overrides_applied} override files applied.{skipped_note}"
        ),
        has_skipped,
        icons,
        content_names,
        project_uids,
        missing_mods,
    })
}

fn read_manifest(bytes: &[u8]) -> Result<CurseForgeManifest, ModpackError> {
    read_cf_manifest(bytes)
}

pub fn read_cf_manifest(bytes: &[u8]) -> Result<CurseForgeManifest, ModpackError> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;
    let mut manifest_file = archive.by_name("manifest.json")?;
    let mut json = String::new();
    manifest_file.read_to_string(&mut json)?;
    Ok(serde_json::from_str(&json)?)
}

fn extract_overrides(
    bytes: &[u8],
    instance_root: &Path,
    prefix: &str,
    cancel: &CancelToken,
) -> Result<u32, ModpackError> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;
    let normalized = prefix.trim_end_matches('/');
    let mut applied = 0u32;

    for i in 0..archive.len() {
        if i % 20 == 0 && cancel.is_cancelled() {
            return Err(crate::download::DownloadError::Cancelled.into());
        }
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if entry.is_dir() {
            continue;
        }
        let Some(relative) = name.strip_prefix(&format!("{normalized}/")) else {
            continue;
        };
        let dest = safe_join(instance_root, relative)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buffer = Vec::new();
        entry.read_to_end(&mut buffer)?;
        std::fs::write(&dest, buffer)?;
        applied += 1;
    }

    Ok(applied)
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
    use super::{
        load_pack_manifest, remove_files_pack_dropped, save_pack_manifest, PackManifestEntry,
    };
    use std::collections::HashSet;
    use std::fs;

    fn temp_instance_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("waybound-pack-reconcile-test-{name}"));
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
        }
    }

    #[test]
    fn manifest_round_trips_through_disk() {
        let dir = temp_instance_dir("roundtrip");
        let entries = vec![test_entry(1, 10, "a.jar"), test_entry(2, 20, "b.jar")];
        save_pack_manifest(&dir, &entries);
        let loaded = load_pack_manifest(&dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].file_id, 10);
        assert_eq!(loaded[1].filename, "b.jar");
    }

    #[test]
    fn missing_manifest_loads_as_empty_not_error() {
        let dir = temp_instance_dir("missing");
        assert!(load_pack_manifest(&dir).is_empty());
    }

    #[test]
    fn dropped_file_is_removed_kept_file_is_not() {
        let dir = temp_instance_dir("removal");
        let mods_dir = dir.join("mods");
        let rp_dir = dir.join("resourcepacks");
        let sp_dir = dir.join("shaderpacks");
        fs::write(mods_dir.join("dropped.jar"), b"old mod").unwrap();
        fs::write(mods_dir.join("kept.jar"), b"still wanted").unwrap();

        let old_manifest = vec![test_entry(1, 10, "dropped.jar"), test_entry(2, 20, "kept.jar")];
        // New manifest only still wants file_id 20 — 10 was dropped by the update.
        let new_file_ids: HashSet<u32> = [20].into_iter().collect();

        remove_files_pack_dropped(&old_manifest, &new_file_ids, &mods_dir, &rp_dir, &sp_dir);

        assert!(!mods_dir.join("dropped.jar").exists(), "dropped file should be removed");
        assert!(mods_dir.join("kept.jar").exists(), "still-wanted file must survive");
    }

    #[test]
    fn user_added_mod_never_tracked_never_touched() {
        let dir = temp_instance_dir("user-added");
        let mods_dir = dir.join("mods");
        let rp_dir = dir.join("resourcepacks");
        let sp_dir = dir.join("shaderpacks");
        // Simulates a mod the user installed via Browse after the pack import —
        // it was never part of any manifest, so it can't appear in `old_manifest`.
        fs::write(mods_dir.join("user-added.jar"), b"manually installed").unwrap();

        let old_manifest = vec![test_entry(1, 10, "something-else.jar")];
        let new_file_ids: HashSet<u32> = HashSet::new(); // pack update drops everything it had

        remove_files_pack_dropped(&old_manifest, &new_file_ids, &mods_dir, &rp_dir, &sp_dir);

        assert!(mods_dir.join("user-added.jar").exists(), "untracked file must never be removed");
    }

    #[test]
    fn pending_missing_mods_excludes_files_already_on_disk() {
        let dir = temp_instance_dir("pending");
        fs::write(dir.join("mods").join("present.jar"), b"already placed").unwrap();
        save_pack_manifest(
            &dir,
            &[test_entry(1, 10, "present.jar"), test_entry(2, 20, "absent.jar")],
        );

        let pending = super::pending_missing_mods(&dir);

        assert_eq!(pending.len(), 1, "only the file not yet on disk should be reported");
        assert_eq!(pending[0].filename, "absent.jar");
    }

    #[test]
    fn pending_missing_mods_excludes_disabled_files() {
        let dir = temp_instance_dir("pending-disabled");
        fs::write(dir.join("mods").join("disabled.jar.disabled"), b"toggled off").unwrap();
        save_pack_manifest(&dir, &[test_entry(1, 10, "disabled.jar")]);

        let pending = super::pending_missing_mods(&dir);

        assert!(pending.is_empty(), "a merely-disabled mod must not be reported as missing");
    }

    #[test]
    fn dismissed_mod_stops_appearing_as_pending() {
        let dir = temp_instance_dir("dismiss");
        save_pack_manifest(
            &dir,
            &[test_entry(1, 10, "absent-a.jar"), test_entry(2, 20, "absent-b.jar")],
        );

        super::remove_pack_manifest_entry(&dir, 1);
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
}

#[cfg(test)]
mod modrinth_fallback_tests {
    use super::{names_plausibly_match, search_query_from_cf_name};

    // Each of these CurseForge titles verified against Modrinth's real
    // search API directly: sent as-is they return zero hits; cleaned like
    // this they return the real project as the top result.
    #[test]
    fn strips_loader_decoration_that_breaks_modrinth_search() {
        assert_eq!(search_query_from_cf_name("Entity Culling Fabric/Forge"), "Entity Culling");
        assert_eq!(search_query_from_cf_name("(ARCHIVE) Faster Random"), "Faster Random");
        assert_eq!(
            search_query_from_cf_name("Better World Loading ([Neo]Forge)"),
            "Better World Loading"
        );
        assert_eq!(
            search_query_from_cf_name("Wabi-Sabi Structures (Forge)"),
            "Wabi-Sabi Structures"
        );
    }

    #[test]
    fn leaves_plain_names_untouched() {
        assert_eq!(search_query_from_cf_name("Structory"), "Structory");
        assert_eq!(search_query_from_cf_name("ServerCore"), "ServerCore");
    }

    #[test]
    fn falls_back_to_original_if_cleaning_empties_it() {
        assert_eq!(search_query_from_cf_name("Forge"), "Forge");
    }

    #[test]
    fn name_matching_is_order_independent_and_case_insensitive() {
        assert!(names_plausibly_match("Entity Culling Fabric/Forge", "EntityCulling"));
        assert!(names_plausibly_match("ServerCore", "servercore"));
        assert!(!names_plausibly_match("Better HP", "Iron's Spells 'n Spellbooks"));
    }
}
