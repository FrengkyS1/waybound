pub mod paths;






use std::path::Path;



use crate::config::ConfigStore;

use crate::db::Database;

use crate::download::{
    download_bytes, download_bytes_with_retry, download_to_file, http_client, safe_join,
    CancelToken,
};
use base64::Engine;

use crate::dto::instance::{InstallModResult, InstalledMod, InstanceSummary};

use crate::dto::{ContentType, ModLoader, ModSource, ModSummary};

use crate::modpack::{
    curseforge_file_url, import_curseforge_modpack_zip, import_modrinth_mrpack_bytes,
    is_curseforge_modpack_zip, is_mrpack_bytes, ModpackError,
};

use crate::sources::curseforge::CurseForgeClient;

use crate::sources::modrinth::{ModrinthClient, ModrinthError};

use thiserror::Error;



use paths::{ensure_instance_dirs, instance_root, instances_root, PathError};



#[derive(Debug, Error)]

pub enum InstanceError {

    #[error("instance not found")]

    NotFound,

    #[error("instance name already exists")]

    NameTaken,

    #[error("invalid instance name")]

    InvalidName,

    #[error("{0}")]

    Path(#[from] PathError),

    #[error("database error: {0}")]

    Db(#[from] crate::db::DbError),

    #[error("mod is already installed in this instance")]

    AlreadyInstalled,

    #[error("only mods, modpacks, and resource packs can be installed")]

    NotInstallable,

    #[error("CurseForge API key is required to install from CurseForge")]

    CurseForgeNotConfigured,

    #[error("io error: {0}")]

    Io(#[from] std::io::Error),

    #[error("network error: {0}")]

    Network(#[from] reqwest::Error),

    #[error("Install cancelled")]

    Cancelled,

    #[error("{filename} requires a manual download (author disabled third-party downloads)")]

    DistributionRestricted { file_id: u32, filename: String, sha1: Option<String> },

    #[error("{0}")]

    Other(String),

}



/// Turns an instance/modpack name into a filesystem- and ID-safe slug, so the
/// instance folder on disk (`%APPDATA%/dev.waybound/instances/<id>`) reads as
/// e.g. `better-mc-fabric-bmc2` instead of a random UUID. Non-alphanumeric
/// runs collapse to a single dash; falls back to "instance" if nothing
/// alphanumeric survives (e.g. an all-emoji name).
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    if slug.ends_with('-') {
        slug.pop();
    }
    slug.truncate(60);
    if slug.is_empty() {
        "instance".to_string()
    } else {
        slug
    }
}

/// Appends `-2`, `-3`, ... until the candidate doesn't collide with an
/// existing instance folder. Names are already unique per the DB's UNIQUE
/// constraint, but two different names can slugify to the same string (e.g.
/// "My Pack!" and "My Pack?"), so this is the real uniqueness guarantee.
fn unique_instance_id(base_slug: &str) -> Result<String, InstanceError> {
    let root_dir = instances_root()?;
    let mut candidate = base_slug.to_string();
    let mut suffix = 2;
    while root_dir.join(&candidate).exists() {
        candidate = format!("{base_slug}-{suffix}");
        suffix += 1;
    }
    Ok(candidate)
}

pub struct InstanceService;



impl InstanceService {

    pub fn list(db: &Database) -> Result<Vec<InstanceSummary>, InstanceError> {

        Ok(db.list_instances()?)

    }



    pub fn create(

        db: &Database,

        name: &str,

        minecraft_version: &str,

        loader: ModLoader,

        loader_version: Option<String>,

    ) -> Result<InstanceSummary, InstanceError> {

        let name = name.trim();

        if name.len() < 2 {

            return Err(InstanceError::InvalidName);

        }



        let id = unique_instance_id(&slugify(name))?;

        let root = instance_root(&id)?;

        ensure_instance_dirs(&id)?;



        let instance = InstanceSummary {

            id: id.clone(),

            name: name.to_string(),

            minecraft_version: minecraft_version.to_string(),

            loader,

            loader_version,

            mod_count: 0,

            created_at: crate::db::now_unix(),

            root_path: root.display().to_string(),

            icon: None,

            last_played: None,

            total_play_seconds: 0,

        };



        if let Err(err) = db.insert_instance(&instance) {

            if let Ok(path) = instance_root(&id) {

                let _ = std::fs::remove_dir_all(path);

            }

            if err.to_string().contains("UNIQUE") {

                return Err(InstanceError::NameTaken);

            }

            return Err(InstanceError::Db(err));

        }



        Ok(instance)

    }



    /// Clones an instance: same version/loader/launch config/icon, a full copy
    /// of its files on disk, fresh play stats. Named "<name> (copy)", bumped
    /// with a counter until it's unique.
    pub fn duplicate(db: &Database, id: &str) -> Result<InstanceSummary, InstanceError> {
        let Some(source) = db.get_instance(id)? else {
            return Err(InstanceError::NotFound);
        };

        let existing: std::collections::HashSet<String> =
            db.list_instances()?.into_iter().map(|i| i.name).collect();
        let mut name = format!("{} (copy)", source.name);
        let mut n = 2;
        while existing.contains(&name) {
            name = format!("{} (copy {n})", source.name);
            n += 1;
        }

        let copy = Self::create(
            db,
            &name,
            &source.minecraft_version,
            source.loader,
            source.loader_version.clone(),
        )?;

        let from_root = instance_root(id)?;
        let to_root = instance_root(&copy.id)?;
        if let Err(err) = copy_dir_recursive(&from_root, &to_root) {
            let _ = Self::delete(db, &copy.id);
            return Err(err.into());
        }

        if let Err(err) = db.duplicate_instance_mods(id, &copy.id, &source.root_path, &copy.root_path) {
            // Same rollback as the file-copy failure above — without it the
            // user is left with a new instance row plus fully copied files on
            // disk, reported as a failed duplicate, with no mods tracked and
            // no obvious way to know it "half-exists".
            let _ = Self::delete(db, &copy.id);
            return Err(err.into());
        }
        if let Some(icon) = source.icon.as_deref() {
            if db.set_instance_icon(&copy.id, Some(icon)).is_err() {
                crate::activity::append_log(
                    &format!("Duplicated {} but couldn't copy its icon.", source.name),
                    "warn",
                    None,
                );
            }
        }
        if let Ok(launch) = db.get_instance_launch_config(id) {
            if db.set_instance_launch_config(&copy.id, &launch).is_err() {
                crate::activity::append_log(
                    &format!(
                        "Duplicated {} but couldn't copy its launch settings.",
                        source.name
                    ),
                    "warn",
                    None,
                );
            }
        }

        Ok(db.get_instance(&copy.id)?.ok_or(InstanceError::NotFound)?)
    }

    pub fn delete(db: &Database, id: &str) -> Result<(), InstanceError> {

        if !db.delete_instance(id)? {

            return Err(InstanceError::NotFound);

        }

        if let Ok(path) = instance_root(id) {

            let _ = std::fs::remove_dir_all(path);

        }

        Ok(())

    }



    pub fn list_mods(db: &Database, instance_id: &str) -> Result<Vec<InstalledMod>, InstanceError> {

        if db.get_instance(instance_id)?.is_none() {

            return Err(InstanceError::NotFound);

        }

        Ok(db.list_instance_mods(instance_id)?)

    }



    pub async fn install_mod(

        db: &Database,

        config: &ConfigStore,

        modrinth: &ModrinthClient,

        curseforge: &CurseForgeClient,

        instance_id: &str,

        summary: &ModSummary,

        preferred_source: Option<ModSource>,

        version_id: Option<&str>,

        cancel: &crate::download::CancelToken,

        report: &impl Fn(u32, u32, &str),

    ) -> Result<InstallModResult, InstanceError> {

        let Some(instance) = db.get_instance(instance_id)? else {

            return Err(InstanceError::NotFound);

        };



        if !is_installable(summary.project_type) {

            return Err(InstanceError::NotInstallable);

        }



        if summary.project_type == ContentType::Modpack {

            return install_modpack(

                db,

                config,

                modrinth,

                curseforge,

                &instance,

                summary,

                preferred_source,

                version_id,

                cancel,

                report,

            )

            .await;

        }



        if db.get_instance_mod(instance_id, &summary.uid)?.is_some() {

            return Err(InstanceError::AlreadyInstalled);

        }



        let source = pick_source(summary, preferred_source)?;

        let download = match resolve_download(

            modrinth,

            curseforge,

            config,

            summary,

            source,

            &instance.minecraft_version,

            instance.loader,

            version_id,

        )

        .await
        {

            Ok(download) => download,

            // The exact file for this instance's MC version + loader exists
            // but its author disabled third-party downloads — same handling
            // as a modpack's per-file restriction: report it as a manual
            // download pointing at this exact file/version/loader instead of
            // failing the install outright.
            Err(InstanceError::DistributionRestricted { file_id, filename, sha1 }) => {

                // The real project URL, not a hardcoded `mc-mods` guess —
                // that 404s for anything CurseForge categorizes outside
                // plain mods (a resourcepack or shader browsed and installed
                // directly, not just ones bundled in a modpack).
                let project_id = summary.curseforge_id.unwrap_or(0);
                let website_url = match config.curseforge_api_key() {
                    Some(api_key) => curseforge
                        .mods_batch(&[project_id], &api_key)
                        .await
                        .get(&project_id)
                        .and_then(|(_, _, _, website_url)| website_url.clone()),
                    None => None,
                };

                return Ok(InstallModResult {
                    message: format!(
                        "{} requires a manual download from CurseForge (its author disabled \
                         third-party downloads). Click \"Download missing mods\" to grab it \
                         yourself and Waybound will place it automatically.",
                        summary.name
                    ),
                    installed: None,
                    instance: instance.clone(),
                    has_skipped: true,
                    missing_mods: vec![crate::dto::instance::MissingMod {
                        project_id,
                        name: summary.name.clone(),
                        filename,
                        url: curseforge_file_url(website_url.as_deref(), &summary.slug, file_id),
                        sha1,
                    }],
                });

            }

            Err(err) => return Err(err),

        };



        let dest_dir = ensure_instance_dirs(instance_id)?;

        let dest_path = match summary.project_type {

            ContentType::Resourcepack => {

                let dir = instance_root(instance_id)?.join("resourcepacks");

                std::fs::create_dir_all(&dir)?;

                safe_join(&dir, &download.filename).map_err(map_download_error)?

            }

            _ => safe_join(&dest_dir, &download.filename).map_err(map_download_error)?,

        };



        let client = http_client().map_err(map_download_error)?;

        download_to_file(&client, &download.url, &dest_path, cancel)

            .await

            .map_err(map_download_error)?;



        // Cache the icon locally instead of hotlinking the CDN — same reasoning
        // as the modpack path below: a raw remote URL stored here breaks if
        // the project's asset is later moved/removed, or is simply offline.
        let icon = match summary.icon_url.as_deref() {
            Some(icon_url) => Some(
                download_icon_data_url(icon_url, cancel)
                    .await
                    .unwrap_or_else(|| icon_url.to_string()),
            ),
            None => None,
        };

        let installed = db.insert_instance_mod(

            instance_id,

            &summary.uid,

            &summary.name,

            source,

            &download.filename,

            &dest_path.display().to_string(),

            icon.as_deref(),

        )?;



        Ok(InstallModResult {
            message: format!("Installed {} to {}", summary.name, instance.name),
            installed: Some(installed),
            instance: instance.clone(),
            has_skipped: false,
            missing_mods: Vec::new(),
        })

    }



    pub fn remove_mod(

        db: &Database,

        instance_id: &str,

        mod_uid: &str,

    ) -> Result<(), InstanceError> {

        let Some(file_path) = db.delete_instance_mod(instance_id, mod_uid)? else {

            return Err(InstanceError::NotFound);

        };

        let path = Path::new(&file_path);

        if path.exists() {

            std::fs::remove_file(path)?;

        }

        Ok(())

    }

}



fn copy_dir_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

async fn install_modpack(

    db: &Database,

    config: &ConfigStore,

    modrinth: &ModrinthClient,

    curseforge: &CurseForgeClient,

    instance: &InstanceSummary,

    summary: &ModSummary,

    preferred_source: Option<ModSource>,

    version_id: Option<&str>,

    cancel: &crate::download::CancelToken,

    report: &impl Fn(u32, u32, &str),

) -> Result<InstallModResult, InstanceError> {

    let source = pick_source(summary, preferred_source)?;

    let download = resolve_download(

        modrinth,

        curseforge,

        config,

        summary,

        source,

        &instance.minecraft_version,

        instance.loader,

        version_id,

    )

    .await?;



    let client = http_client().map_err(map_download_error)?;

    let bytes = if source == crate::dto::ModSource::Curseforge {
        download_bytes_with_retry(&client, &download.url, cancel).await
    } else {
        download_bytes(&client, &download.url, cancel).await
    }
    .map_err(map_download_error)?;



    let instance_root = instance_root(&instance.id)?;

    let import = if is_mrpack_bytes(&bytes) {

        import_modrinth_mrpack_bytes(&bytes, &instance_root, modrinth, cancel, report)

            .await

            .map_err(map_modpack_error)?

    } else if is_curseforge_modpack_zip(&bytes) {

        let api_key = config

            .curseforge_api_key()

            .ok_or(InstanceError::CurseForgeNotConfigured)?;

        import_curseforge_modpack_zip(
            &bytes,
            &instance_root,
            &api_key,
            modrinth,
            &instance.minecraft_version,
            instance.loader,
            cancel,
            report,
        )

            .await

            .map_err(map_modpack_error)?

    } else {

        let staging = safe_join(&instance_root, &download.filename).map_err(map_download_error)?;

        std::fs::write(&staging, &bytes)?;

        return Err(InstanceError::Other(format!(

            "Downloaded modpack archive to {} but could not recognize the format. Expected .mrpack or CurseForge manifest.json.",

            staging.display()

        )));

    };



    sync_mods_folder(
        db,
        &instance.id,
        &instance_root.join("mods"),
        source,
        &import.icons,
        &import.content_names,
        &import.project_uids,
    )?;
    seed_content_meta_cache(
        db,
        &instance.id,
        &instance_root.join("resourcepacks"),
        "resourcepack",
        &import.icons,
        &import.content_names,
    );
    seed_content_meta_cache(
        db,
        &instance.id,
        &instance_root.join("shaderpacks"),
        "shaderpack",
        &import.icons,
        &import.content_names,
    );



    // Give the instance the modpack's own artwork. Downloaded and embedded as a
    // data URL (matching how manually-uploaded icons are stored) so the card
    // renders instantly instead of hotlinking the CDN and showing the loader
    // placeholder until that request resolves. Falls back to the raw URL if
    // the download fails, so a network hiccup here doesn't lose the icon.
    if let Some(icon_url) = summary.icon_url.as_deref() {

        let icon = download_icon_data_url(icon_url, cancel)
            .await
            .unwrap_or_else(|| icon_url.to_string());

        let _ = db.set_instance_icon(&instance.id, Some(&icon));

    }



    // Auto-apply a recommended heap size sized to the pack, unless the user

    // already set one for this instance.

    if let Ok(mut launch_config) = db.get_instance_launch_config(&instance.id) {

        if launch_config.max_memory_mb.is_none() {

            let mod_count = std::fs::read_dir(instance_root.join("mods"))

                .map(|entries| {

                    entries

                        .filter_map(|e| e.ok())

                        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jar"))

                        .count()

                })

                .unwrap_or(0) as u32;

            launch_config.max_memory_mb = Some(recommended_memory_mb(mod_count));

            let _ = db.set_instance_launch_config(&instance.id, &launch_config);

        }

    }



    let installed = db.insert_instance_mod(

        &instance.id,

        &summary.uid,

        &summary.name,

        source,

        &download.filename,

        &instance_root.display().to_string(),

        summary.icon_url.as_deref(),

    )?;



    Ok(InstallModResult {
        message: import.message,
        installed: Some(installed),
        instance: instance.clone(),
        has_skipped: import.has_skipped,
        missing_mods: import.missing_mods,
    })

}



/// Recommended max heap for a pack, by mod count.
/// ponytail: naive mod-count heuristic, not real memory profiling — revisit if
/// packs with few but memory-hungry mods (e.g. shader-heavy) report OOMs.
pub fn recommended_memory_mb(mod_count: u32) -> u32 {
    match mod_count {
        0..=40 => 2048,
        41..=80 => 3072,
        81..=150 => 4096,
        _ => 6144,
    }
}

fn sync_mods_folder(

    db: &Database,

    instance_id: &str,

    mods_dir: &Path,

    source: ModSource,

    icons: &std::collections::HashMap<String, String>,

    content_names: &std::collections::HashMap<String, String>,

    project_uids: &std::collections::HashMap<String, String>,

) -> Result<(), InstanceError> {

    if !mods_dir.exists() {

        return Ok(());

    }



    // Existing rows for this instance, from whatever install path put them
    // there (a Browse install's real `mod:`/`curseforge:` uid, or a previous
    // sync's `file:` uid) — used below to (a) skip inserting a duplicate
    // `file:` row for a filename a real uid already tracks, since the unique
    // constraint is on `(instance_id, mod_uid)` not `(instance_id,
    // file_name)`, and a modpack sync finding a Browse-installed jar would
    // otherwise double-count it; and (b) prune rows for files this scan
    // didn't find, since this insert-only batch never used to prune deleted
    // jars, leaving phantom entries (and an inflated count) behind forever.
    // ponytail: no per-instance lock, so this scan racing a concurrent
    // `remove_mod` (which deletes the DB row before the file) in the narrow
    // window between those two steps could re-insert a `file:` row for a
    // file that's about to disappear. Add a per-instance mutex around
    // instance-mutating commands if that phantom-row case ever actually
    // shows up in practice.
    let existing = db.list_instance_mods(instance_id).unwrap_or_default();
    let existing_filenames: std::collections::HashSet<&str> =
        existing.iter().map(|m| m.file_name.as_str()).collect();

    // A project whose pinned version just changed resolves to a new
    // filename this import — CurseForge's importer tracks a project-keyed
    // manifest specifically to delete the file that superseded, but the
    // Modrinth importer had no equivalent at all, and this scan is the one
    // place both paths meet. Without this, an old jar for the same project
    // (a stale duplicate of whatever `.jar` name the last version happened
    // to use) is silently never removed and stays loaded by Forge/NeoForge
    // right alongside the new one.
    let uid_to_new_filename: std::collections::HashMap<&str, &str> =
        project_uids.iter().map(|(fname, uid)| (uid.as_str(), fname.as_str())).collect();
    for row in &existing {
        let Some(&new_filename) = uid_to_new_filename.get(row.mod_uid.as_str()) else {
            continue;
        };
        if new_filename == row.file_name {
            continue;
        }
        let _ = std::fs::remove_file(mods_dir.join(&row.file_name));
    }

    // Catches up a row this scan already tracks on two things a later re-sync
    // can know that an earlier one couldn't: (a) an icon it didn't have
    // before (an older import, before hash-based icon lookup existed), and
    // (b) — the bigger one — its real project id, when it's currently just
    // an untrackable `file:<name>` record. Without (b), "check for updates"
    // has nothing to re-resolve against for anything synced before this
    // lookup existed, which is most of a typical modpack-installed library.
    // The content-tab metadata cache is fingerprinted by the file's own
    // size+mtime, so it has no way to know either of these side-channel
    // changes happened — drop its cached row too, or the Content tab keeps
    // showing the stale result forever even though the DB now has better data.
    for row in &existing {
        let icon = icons.get(&row.file_name).cloned().or_else(|| row.icon_url.clone());
        let resolved_name = content_names.get(&row.file_name);
        let name = resolved_name.cloned().unwrap_or_else(|| row.mod_name.clone());
        let needs_icon_backfill = row.icon_url.is_none() && icon.is_some();
        // The pre-fix default was always the filename with `.jar` stripped —
        // a real resolved name is always worth taking over that, not just
        // when the row had none at all.
        let needs_name_backfill = resolved_name.is_some_and(|n| n != &row.mod_name);
        let real_uid = project_uids.get(&row.file_name).filter(|_| row.mod_uid.starts_with("file:"));

        if !needs_icon_backfill && !needs_name_backfill && real_uid.is_none() {
            continue;
        }

        if let Some(real_uid) = real_uid {
            if let Ok(Some(file_path)) = db.delete_instance_mod(instance_id, &row.mod_uid) {
                let _ = db.insert_instance_mod(
                    instance_id,
                    real_uid,
                    &name,
                    source,
                    &row.file_name,
                    &file_path,
                    icon.as_deref(),
                );
            }
        } else {
            if needs_icon_backfill {
                let _ = db.update_instance_mod_icon(instance_id, &row.mod_uid, icon.as_deref().unwrap());
            }
            if needs_name_backfill {
                let _ = db.update_instance_mod_name(instance_id, &row.mod_uid, &name);
            }
        }
        let _ = db.delete_content_meta_cache(instance_id, "mod", &row.file_name);
    }

    let mut mods = Vec::new();
    let mut seen_filenames = std::collections::HashSet::new();

    for entry in std::fs::read_dir(mods_dir)? {

        let entry = entry?;

        let path = entry.path();

        if !path.is_file() {

            continue;

        }

        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {

            continue;

        };

        if !file_name.ends_with(".jar") {

            continue;

        }

        seen_filenames.insert(file_name.to_string());

        if existing_filenames.contains(file_name) {
            continue;
        }

        let mod_uid = project_uids
            .get(file_name)
            .cloned()
            .unwrap_or_else(|| format!("file:{file_name}"));

        let mod_name = content_names
            .get(file_name)
            .cloned()
            .unwrap_or_else(|| file_name.trim_end_matches(".jar").to_string());

        mods.push((

            mod_uid,

            mod_name,

            source,

            file_name.to_string(),

            path.display().to_string(),

            icons.get(file_name).cloned(),

        ));

    }



    let _ = db.insert_instance_mods_batch(instance_id, &mods);

    for stale in existing.iter().filter(|m| !seen_filenames.contains(&m.file_name)) {
        let _ = db.delete_instance_mod_by_file(instance_id, &stale.file_name);
    }

    Ok(())
}

/// Seeds the Content tab's metadata cache for resource/shader packs a
/// modpack import resolved. Unlike a mod jar, these have no embedded-name
/// convention of their own (no `mods.toml` equivalent), so the source
/// platform's project name/icon — already fetched during import, for
/// exactly this reason — is the only real display data available for them.
/// Without this they'd show their raw filename forever, since there's
/// nothing inside the file itself to read.
fn seed_content_meta_cache(
    db: &Database,
    instance_id: &str,
    dir: &Path,
    category: &str,
    icons: &std::collections::HashMap<String, String>,
    content_names: &std::collections::HashMap<String, String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let name = content_names.get(&file_name);
        let icon = icons.get(&file_name);
        if name.is_none() && icon.is_none() {
            continue;
        }
        let mtime_unix = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let _ = db.upsert_content_meta_cache(
            instance_id,
            category,
            &file_name,
            metadata.len(),
            mtime_unix,
            name.map(String::as_str),
            icon.map(String::as_str),
        );
    }
}



async fn resolve_download(

    modrinth: &ModrinthClient,

    curseforge: &CurseForgeClient,

    config: &ConfigStore,

    summary: &ModSummary,

    source: ModSource,

    mc_version: &str,

    loader: ModLoader,

    version_id: Option<&str>,

) -> Result<ResolvedDownload, InstanceError> {

    match source {

        ModSource::Modrinth => {

            if let Some(vid) = version_id {

                return modrinth

                    .resolve_version_by_id(vid)

                    .await

                    .map_err(map_modrinth_install_error);

            }

            let project_id = summary

                .modrinth_id

                .as_deref()

                .unwrap_or(summary.slug.as_str());

            modrinth

                .resolve_download(project_id, mc_version, loader, summary.project_type)

                .await

                .map_err(map_modrinth_install_error)

        }

        ModSource::Curseforge => {

            let mod_id = summary.curseforge_id.ok_or_else(|| {

                InstanceError::Other("CurseForge id missing on mod summary.".to_string())

            })?;

            let api_key = config

                .curseforge_api_key()

                .ok_or(InstanceError::CurseForgeNotConfigured)?;

            if let Some(vid) = version_id {

                let file_id: u32 = vid.parse().map_err(|_| {

                    InstanceError::Other("Invalid CurseForge file id.".to_string())

                })?;

                return curseforge

                    .resolve_file_by_id(mod_id, file_id, &api_key)

                    .await

                    .map_err(map_curseforge_install_error);

            }

            curseforge

                .resolve_download_with_key(

                    mod_id,

                    mc_version,

                    loader,

                    summary.project_type,

                    &api_key,

                )

                .await

                .map_err(map_curseforge_install_error)

        }

    }

}



fn is_installable(content_type: ContentType) -> bool {

    matches!(

        content_type,

        ContentType::Mod | ContentType::Modpack | ContentType::Resourcepack

    )

}



fn pick_source(summary: &ModSummary, preferred: Option<ModSource>) -> Result<ModSource, InstanceError> {

    if let Some(source) = preferred {

        if summary.sources.contains(&source) {

            return Ok(source);

        }

        return Err(InstanceError::Other(

            "Mod is not available on the selected source.".to_string(),

        ));

    }



    if summary.modrinth_id.is_some() {

        return Ok(ModSource::Modrinth);

    }

    if summary.curseforge_id.is_some() {

        return Ok(ModSource::Curseforge);

    }



    if summary.sources.contains(&ModSource::Modrinth) {

        return Ok(ModSource::Modrinth);

    }

    if summary.sources.contains(&ModSource::Curseforge) {

        return Ok(ModSource::Curseforge);

    }



    Err(InstanceError::Other("No install source available.".to_string()))

}



fn map_curseforge_install_error(err: crate::sources::curseforge::CurseForgeError) -> InstanceError {

    match err {

        crate::sources::curseforge::CurseForgeError::NotConfigured => {

            InstanceError::CurseForgeNotConfigured

        }

        crate::sources::curseforge::CurseForgeError::NotFound => InstanceError::Other(format!(

            "No compatible file found for this Minecraft version and loader. Try matching your instance version/loader to the mod, or pick a different mod version."

        )),

        crate::sources::curseforge::CurseForgeError::Rejected { message, .. } => {

            InstanceError::Other(message)

        }

        crate::sources::curseforge::CurseForgeError::Network(err) => InstanceError::Network(err),

        crate::sources::curseforge::CurseForgeError::DistributionRestricted { file_id, filename, sha1 } => {

            InstanceError::DistributionRestricted { file_id, filename, sha1 }

        }

    }

}



fn map_modrinth_install_error(err: ModrinthError) -> InstanceError {

    match err {

        ModrinthError::NotFound => InstanceError::Other(format!(
            "No compatible file found for this Minecraft version and loader. Try matching your instance version/loader to the mod, or pick a different mod version."
        )),

        ModrinthError::Decode(message) => InstanceError::Other(message),

        ModrinthError::Network(err) => InstanceError::Network(err),

    }

}



/// Icons are small CDN thumbnails; this is generous headroom, not a real
/// expected size — it exists to bound worst case, not to pass legitimate
/// icons through with margin to spare.
const ICON_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Downloads an icon URL and embeds it as a small base64 data URL, so
/// rendering the instance card never depends on a live network fetch. `None`
/// on any failure (network, cancelled, empty body) — callers should fall
/// back to the raw URL.
///
/// `icon_url` comes from a `ModSummary` on the Tauri IPC boundary — normally
/// that's data the backend itself fetched from Modrinth/CurseForge, but the
/// webview is untrusted, so this restricts to https and caps the response
/// size rather than trusting the URL and length implicitly.
async fn download_icon_data_url(url: &str, cancel: &CancelToken) -> Option<String> {
    if !url.starts_with("https://") {
        return None;
    }
    let client = http_client().ok()?;
    let bytes = crate::download::download_bytes_capped(&client, url, cancel, ICON_MAX_BYTES)
        .await
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    let mime = match Path::new(url)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/png",
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{mime};base64,{encoded}"))
}

fn map_download_error(err: crate::download::DownloadError) -> InstanceError {

    match err {

        crate::download::DownloadError::Network(err) => InstanceError::Network(err),

        crate::download::DownloadError::Io(err) => InstanceError::Io(err),

        crate::download::DownloadError::Status(status) => InstanceError::Other(format!(

            "Download failed with HTTP {status}. The file URL may have expired — try again."

        )),

        crate::download::DownloadError::UnsafePath(path) => InstanceError::Other(format!(

            "Refusing to install file with unsafe path: {path}"

        )),

        crate::download::DownloadError::Cancelled => InstanceError::Cancelled,

        crate::download::DownloadError::TooLarge(max) => InstanceError::Other(format!(

            "Download exceeded the {max}-byte limit."

        )),

    }

}



fn map_modpack_error(err: ModpackError) -> InstanceError {

    match err {

        ModpackError::Network(err) => InstanceError::Network(err),

        ModpackError::Io(err) => InstanceError::Io(err),

        ModpackError::Download(err) => map_download_error(err),

        ModpackError::Zip(err) => InstanceError::Other(format!("Invalid modpack archive: {err}")),

        ModpackError::Json(err) => InstanceError::Other(format!("Invalid modpack metadata: {err}")),

        ModpackError::Other(message) => InstanceError::Other(message),

    }

}





pub struct ResolvedDownload {

    pub url: String,

    pub filename: String,

}



#[cfg(test)]

mod tests {

    use super::is_installable;

    use crate::dto::ContentType;



    #[test]

    fn modpacks_are_installable() {

        assert!(is_installable(ContentType::Modpack));

    }

}


