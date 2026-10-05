pub mod paths;
pub mod operations;






use std::path::Path;



use crate::config::ConfigStore;

use crate::db::Database;

use crate::download::{
    download_bytes_capped_with_retry, ensure_contained_path, http_client, safe_join,
    verify_hashes, CancelToken, MAX_DOWNLOAD_BYTES,
};
use base64::Engine;

/// An instance's display name (not the on-disk slug, which is separately
/// truncated to 60 chars) had no upper bound at all — a several-hundred
/// character name overflowed the instance header, the install-confirmation
/// toast, and the instance picker dropdown. 100 is generous for any real
/// pack/profile name while keeping every one of those layouts intact.
pub(crate) const MAX_INSTANCE_NAME_LEN: usize = 100;

use crate::dto::instance::{InstallModResult, InstalledMod, InstanceSummary};

use crate::dto::{ContentType, ModLoader, ModSource, ModSummary};

use crate::modpack::{
    curseforge_file_url, prepare_curseforge_modpack_zip, prepare_modrinth_mrpack_bytes,
    is_curseforge_modpack_zip, is_mrpack_bytes, ModpackError, PackDeclaredLoader, PackTransaction,
};

use crate::sources::curseforge::CurseForgeClient;

use crate::sources::modrinth::{ModrinthClient, ModrinthError};

use thiserror::Error;



use paths::{instance_root, instances_root, PathError};



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

    DistributionRestricted { file_id: u32, filename: String, sha1: Option<String>, dependencies: Vec<ResolvedDependency> },

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

/// Windows reserves these names at the filesystem-namespace level (with or
/// without an extension) — a folder literally named `con` can't be reliably
/// addressed by most APIs (including `remove_dir_all`/`trash::delete`) without
/// the `\\?\` extended-length prefix, so a display name that slugifies down to
/// one of these would create an instance whose folder can never be properly
/// deleted again. Checked as an immediate "taken" collision below so it's
/// silently disambiguated to `con-2` etc. instead of ever being used bare —
/// the display name itself is untouched, only the folder id changes.
const RESERVED_WINDOWS_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Appends `-2`, `-3`, ... until the candidate doesn't collide with an
/// existing instance folder. Names are already unique per the DB's UNIQUE
/// constraint, but two different names can slugify to the same string (e.g.
/// "My Pack!" and "My Pack?"), so this is the real uniqueness guarantee.
fn is_reserved_windows_name(candidate: &str) -> bool {
    RESERVED_WINDOWS_NAMES.contains(&candidate)
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
        Self::publish_prepared(db, name, minecraft_version, loader, loader_version, None, None)
    }

    /// Copy into private, same-volume staging; publish files before any DB row.
    /// The caller retains ownership of `staged_root`, including on failure.
    pub fn publish_staged(
        db: &Database,
        name: &str,
        minecraft_version: &str,
        loader: ModLoader,
        loader_version: Option<String>,
        staged_root: &Path,
    ) -> Result<InstanceSummary, InstanceError> {
        Self::publish_prepared(db, name, minecraft_version, loader, loader_version, Some(staged_root), None)
    }

    fn publish_prepared(
        db: &Database,
        name: &str,
        minecraft_version: &str,
        loader: ModLoader,
        loader_version: Option<String>,
        staged_root: Option<&Path>,
        duplicate: Option<&InstanceSummary>,
    ) -> Result<InstanceSummary, InstanceError> {
        let name = name.trim();
        if name.len() < 2 || name.chars().count() > MAX_INSTANCE_NAME_LEN {
            return Err(InstanceError::InvalidName);
        }
        let reservation = paths::InstanceReservation::reserve(&instances_root()?, &slugify(name))?;
        let staging = reservation.staging_root();
        if let Some(source) = staged_root {
            copy_dir_recursive(source, &staging)?;
        }
        std::fs::create_dir_all(staging.join("mods"))?;
        reservation.publish(&staging)?;
        let instance = InstanceSummary {
            id: reservation.id.clone(),
            name: name.to_string(),
            minecraft_version: minecraft_version.to_string(),
            loader,
            loader_version,
            mod_count: duplicate.map_or(0, |source| source.mod_count),
            created_at: crate::db::now_unix(),
            root_path: reservation.root.display().to_string(),
            icon: duplicate.and_then(|source| source.icon.clone()),
            last_played: None,
            total_play_seconds: 0,
            modpack_version_label: duplicate.and_then(|source| source.modpack_version_label.clone()),
            modpack_project_uid: duplicate.and_then(|source| source.modpack_project_uid.clone()),
        };
        let result = match duplicate {
            Some(source) => db.insert_duplicate_instance(source, &instance),
            None => db.insert_instance(&instance),
        };
        if let Err(err) = result {
            if err.to_string().contains("UNIQUE") {
                return Err(InstanceError::NameTaken);
            }
            return Err(err.into());
        }
        reservation.commit();
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
        let mut n = 1;
        let name = loop {
            let candidate = duplicate_name(&source.name, n);
            if !existing.contains(&candidate) { break candidate; }
            n += 1;
        };

        Self::publish_prepared(
            db,
            &name,
            &source.minecraft_version,
            source.loader,
            source.loader_version.clone(),
            Some(&instance_root(id)?),
            Some(&source),
        )
    }

    pub fn delete(db: &Database, id: &str) -> Result<(), InstanceError> {
        if db.get_instance(id)?.is_none() { return Err(InstanceError::NotFound); }
        let root = instance_root(id)?;
        ensure_contained_path(&instances_root()?, &root).map_err(map_download_error)?;
        if root.exists() {
            trash::delete(&root).map_err(|error| InstanceError::Other(format!(
                "Could not move instance to Recycle Bin; instance retained: {error}"
            )))?;
        }
        if !db.delete_instance(id)? { return Err(InstanceError::NotFound); }
        Ok(())
    }

    pub fn clear_pack_loader_pin(instance_id: &str) -> Result<(), InstanceError> {
        let root = instance_root(instance_id)?;
        let path = crate::download::contained_join(&root, PACK_LOADER_PROVENANCE).map_err(map_download_error)?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
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

        update_existing: bool,

        // Explicit origin override. When absent, a sidecar-claimed
        // filename means pack (reinstalling or version-switching pack
        // content); anything else is a genuine add.
        origin: Option<crate::dto::ModOrigin>,

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



        let existing = db.get_instance_mod(instance_id, &summary.uid)?;
        let existing_pack = existing.as_ref()
            .is_some_and(|(row, _)| row.origin == crate::dto::ModOrigin::Pack);
        if !update_existing && existing.is_some() {

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
            Err(InstanceError::DistributionRestricted { file_id, filename, sha1, dependencies }) => {

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
                let missing = crate::dto::instance::MissingMod {
                    project_id, name: summary.name.clone(), filename,
                    url: curseforge_file_url(website_url.as_deref(), &summary.slug, file_id), sha1,
                };
                if let Some((_, previous_path)) = existing.as_ref() {
                    if existing_pack {
                        let root = instance_root(instance_id)?;
                        crate::modpack::prepare_pending_pack_update(&root, &missing, file_id, Path::new(previous_path))
                            .map_err(map_modpack_error)?.commit(cancel).await.map_err(map_modpack_error)?;
                    }
                }
                let (_, mut missing_mods) = if summary.project_type == ContentType::Mod {
                    install_required_dependencies(db, modrinth, curseforge, config, &instance, summary, dependencies, cancel, report).await
                } else { (Vec::new(), Vec::new()) };
                missing_mods.insert(0, missing);

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
                    missing_mods,
                });

            }

            Err(err) => return Err(err),

        };



        let root = instance_root(instance_id)?;
        let dest_dir = root.join("mods");

        let mut dest_path = match summary.project_type {

            ContentType::Resourcepack => {

                let dir = instance_root(instance_id)?.join("resourcepacks");


                safe_join(&dir, &download.filename).map_err(map_download_error)?

            }

            _ => safe_join(&dest_dir, &download.filename).map_err(map_download_error)?,

        };



        if existing.as_ref().is_some_and(|(_, path)| path.ends_with(crate::commands::content::DISABLED_SUFFIX)) {
            dest_path = dest_path.with_file_name(format!("{}{}", download.filename, crate::commands::content::DISABLED_SUFFIX));
        }
        ensure_contained_path(&root, &dest_path).map_err(map_download_error)?;
        let client = http_client().map_err(map_download_error)?;
        let bytes = download_bytes_capped_with_retry(&client, &download.url, cancel, MAX_DOWNLOAD_BYTES)
            .await.map_err(map_download_error)?;
        verify_hashes(&bytes, &download.hashes).map_err(map_download_error)?;
        let mut transaction = PackTransaction::new(&root).map_err(map_modpack_error)?;
        let relative = dest_path.strip_prefix(&root).map_err(|_| InstanceError::Other("Install path outside instance".into()))?;
        transaction.stage(&relative.to_string_lossy(), &bytes).map_err(map_modpack_error)?;
        if let Some((_, previous)) = &existing {
            let previous = Path::new(previous);
            ensure_contained_path(&root, previous).map_err(map_download_error)?;
            if previous != dest_path && previous.is_file() {
                transaction.remove(previous).map_err(map_modpack_error)?;
            }
        }
        // Standalone updates of pack-owned files publish the new receipt
        // in the same rollback boundary as the replacement and DB row.
        if source == ModSource::Curseforge && existing_pack {
            let file_id = download.curseforge_file_id.ok_or_else(|| InstanceError::Other("Resolved CurseForge file id missing".into()))?;
            let project_id = summary.curseforge_id.ok_or_else(|| InstanceError::Other("CurseForge project id missing".into()))?;
            let receipt = crate::dto::instance::MissingMod {
                project_id, name: summary.name.clone(), filename: download.filename.clone(),
                url: curseforge_file_url(None, &summary.slug, file_id), sha1: None,
            };
            let actual_sha1 = match download.hashes.get("sha1") {
                Some(hash) => hash.clone(), // Proven against these bytes above.
                None => {
                    use sha1::Digest;
                    hex::encode(sha1::Sha1::digest(&bytes))
                }
            };
            crate::modpack::stage_completed_pack_update(&mut transaction, &root, &receipt, file_id, actual_sha1)
                .map_err(map_modpack_error)?;
        }



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

        // Origin for the new row: explicit caller override wins, else a
        // sidecar-claimed filename means reinstalling pack content.
        let sidecar_claimed = instance_root(instance_id)
            .ok()
            .map(|root| crate::db::pack_filenames(&root).contains(&download.filename))
            .unwrap_or(false);
        let filename = dest_path.file_name().and_then(|name| name.to_str())
            .ok_or_else(|| InstanceError::Other("Invalid install filename".into()))?;
        let mut installed = None;
        transaction.commit_with(cancel, || {
            installed = Some(db.insert_instance_mod(
                instance_id, &summary.uid, &summary.name, source, filename,
                &dest_path.display().to_string(), icon.as_deref(),
                resolve_install_origin(origin, sidecar_claimed),
            ).map_err(|error| ModpackError::Other(format!("Could not record install: {error}")))?);
            Ok(())
        }).await.map_err(map_modpack_error)?;
        let installed = installed.expect("committed install recorded");



        // Runs only after the requested mod is safely on disk and recorded,
        // so nothing here can cost the user the install they asked for.
        // Both sources are walked: Modrinth via its version dependency list,
        // CurseForge via the file's relation metadata (relationType 3).
        let (dependency_names, dependency_missing) =
            if summary.project_type == ContentType::Mod {
                install_required_dependencies(
                    db,
                    modrinth,
                    curseforge,
                    config,
                    &instance,
                    summary,
                    download.dependencies.clone(),
                    cancel,
                    report,
                )
                .await
            } else {
                (Vec::new(), Vec::new())
            };

        let mut message = if dependency_names.is_empty() {
            format!("Installed {} to {}", summary.name, instance.name)
        } else {
            format!(
                "Installed {} to {}, plus {} required {}: {}",
                summary.name,
                instance.name,
                dependency_names.len(),
                if dependency_names.len() == 1 { "dependency" } else { "dependencies" },
                dependency_names.join(", "),
            )
        };
        if !dependency_missing.is_empty() {
            message.push_str(&format!(
                " {} required {} need{} a manual download (author disabled third-party downloads) — use \"Download missing mods\".",
                dependency_missing.len(),
                if dependency_missing.len() == 1 { "dependency" } else { "dependencies" },
                if dependency_missing.len() == 1 { "s" } else { "" },
            ));
        }

        Ok(InstallModResult {
            message,
            installed: Some(installed),
            instance: instance.clone(),
            has_skipped: !dependency_missing.is_empty(),
            missing_mods: dependency_missing,
        })

    }



    pub fn remove_mod(
        db: &Database,
        instance_id: &str,
        mod_uid: &str,
    ) -> Result<(), InstanceError> {
        let Some((_, file_path)) = db.get_instance_mod(instance_id, mod_uid)? else {
            return Err(InstanceError::NotFound);
        };
        let root = instance_root(instance_id)?;
        let path = Path::new(&file_path);
        ensure_contained_path(&root, path).map_err(map_download_error)?;
        remove_tracked_file(&root, path, || {
            if db.delete_instance_mod(instance_id, mod_uid)?.is_none() {
                return Err(InstanceError::NotFound);
            }
            Ok(())
        })
    }

}



/// Ceiling on how many dependencies one install may pull in. A real mod's
/// required-dependency closure is small (a library or two, occasionally a
/// chain of three); anything approaching this means the graph is cyclic in a
/// way the visited-set missed, or a project mislabels optional deps as
/// required. Stopping is better than silently downloading a hundred files
/// the user never asked for.
const MAX_DEPENDENCY_INSTALLS: usize = 24;

fn duplicate_name(name: &str, copy: u32) -> String {
    let suffix = if copy == 1 { " (copy)".to_string() } else { format!(" (copy {copy})") };
    let base: String = name.chars().take(MAX_INSTANCE_NAME_LEN - suffix.chars().count()).collect();
    format!("{base}{suffix}")
}

pub(crate) fn remove_tracked_file(
    root: &Path,
    path: &Path,
    publish: impl FnOnce() -> Result<(), InstanceError>,
) -> Result<(), InstanceError> {
    ensure_contained_path(root, path).map_err(map_download_error)?;
    if !path.exists() { return publish(); }
    if !path.is_file() {
        return Err(InstanceError::Other("Tracked content is not a file; no tracking was removed".into()));
    }
    let staging = tempfile::Builder::new().prefix(".waybound-remove-").tempdir_in(root)?;
    let backup = staging.path().join("original");
    std::fs::rename(path, &backup)?;
    if let Err(error) = publish() {
        if let Err(restore) = std::fs::rename(&backup, path) {
            let recovery = staging.keep();
            return Err(InstanceError::Other(format!("{error}; restore failed: {restore}. Original retained at {}", recovery.display())));
        }
        return Err(error);
    }
    Ok(())
}

/// Publish a verified dependency and its tracking together. Exact pins may
/// replace an existing version, retaining its disabled state and origin.
async fn install_dependency(
    db: &Database,
    instance: &InstanceSummary,
    summary: &ModSummary,
    source: ModSource,
    download: &ResolvedDownload,
    cancel: &CancelToken,
) -> Result<(), InstanceError> {
    let root = instance_root(&instance.id)?;
    let existing = db.get_instance_mod(&instance.id, &summary.uid)?;
    let origin = existing.as_ref()
        .map_or(crate::dto::ModOrigin::User, |(row, _)| row.origin);
    let filename = if existing.as_ref().is_some_and(|(_, path)| path.ends_with(crate::commands::content::DISABLED_SUFFIX)) {
        format!("{}{}", download.filename, crate::commands::content::DISABLED_SUFFIX)
    } else {
        download.filename.clone()
    };
    let dest = crate::download::contained_join(&root.join("mods"), &filename).map_err(map_download_error)?;
    let client = http_client().map_err(map_download_error)?;
    let bytes = download_bytes_capped_with_retry(&client, &download.url, cancel, MAX_DOWNLOAD_BYTES)
        .await.map_err(map_download_error)?;
    verify_hashes(&bytes, &download.hashes).map_err(map_download_error)?;
    let mut transaction = PackTransaction::new(&root).map_err(map_modpack_error)?;
    transaction.stage(&format!("mods/{filename}"), &bytes).map_err(map_modpack_error)?;
    if let Some((_, previous)) = existing {
        let previous = Path::new(&previous);
        ensure_contained_path(&root, previous).map_err(map_download_error)?;
        if previous != dest && previous.is_file() {
            transaction.remove(previous).map_err(map_modpack_error)?;
        }
    }
    let icon = match summary.icon_url.as_deref() {
        Some(url) => Some(download_icon_data_url(url, cancel).await.unwrap_or_else(|| url.to_string())),
        None => None,
    };
    transaction.commit_with(cancel, || {
        db.insert_instance_mod(
            &instance.id, &summary.uid, &summary.name, source, &filename,
            &dest.display().to_string(), icon.as_deref(), origin,
        ).map_err(|error| ModpackError::Other(format!("Could not record dependency: {error}")))?;
        Ok(())
    }).await.map_err(map_modpack_error)
}

/// Walk the requirements carried by each chosen file, never re-query latest
/// merely to discover dependencies. Version-only requirements retain pins.
async fn install_required_dependencies(
    db: &Database,
    modrinth: &ModrinthClient,
    curseforge: &CurseForgeClient,
    config: &ConfigStore,
    instance: &InstanceSummary,
    root: &ModSummary,
    dependencies: Vec<ResolvedDependency>,
    cancel: &CancelToken,
    report: &impl Fn(u32, u32, &str),
) -> (Vec<String>, Vec<crate::dto::instance::MissingMod>) {
    let mut queue: std::collections::VecDeque<_> = dependencies.into();
    let mut visited = Vec::new();
    let mut installed = Vec::new();
    let mut missing = Vec::new();
    let mut pinned_projects = std::collections::HashMap::<String, String>::new();
    while let Some(dep) = queue.pop_front() {
        if cancel.is_cancelled() || visited.len() >= MAX_DEPENDENCY_INSTALLS { break; }
        if visited.contains(&dep) { continue; }
        visited.push(dep.clone());
        let resolved: Result<(ModSummary, ModSource, ResolvedDownload), InstanceError> = async {
            match dep {
                ResolvedDependency::Modrinth { project_id, version_id } => {
                    let exact_version = if let Some(version_id) = version_id.as_deref() {
                        Some(modrinth.fetch_version_detail(version_id).await.map_err(map_modrinth_install_error)?)
                    } else { None };
                    let project_id = if let Some(version) = exact_version.as_ref() {
                        let version_id = version_id.as_deref().expect("exact version requested");
                        if version.project_id.is_empty() || project_id.as_deref().is_some_and(|id| id != version.project_id) {
                            return Err(InstanceError::Other("Dependency version belongs to a different or unknown project".into()));
                        }
                        if let Some(previous) = pinned_projects.get(&version.project_id) {
                            if previous != version_id {
                                return Err(InstanceError::Other(format!("Conflicting required versions {previous} and {version_id} for {}", version.project_id)));
                            }
                        }
                        pinned_projects.insert(version.project_id.clone(), version_id.to_string());
                        version.project_id.clone()
                    } else {
                        project_id.ok_or_else(|| InstanceError::Other("Dependency has neither project nor version id".into()))?
                    };
                    let uid = format!("modrinth:{project_id}");
                    if uid == root.uid { return Err(InstanceError::AlreadyInstalled); }
                    if version_id.is_none() && db.get_instance_mod(&instance.id, &uid)?.is_some() {
                        return Err(InstanceError::AlreadyInstalled);
                    }
                    let summary = modrinth.fetch_project_summary(&project_id).await.map_err(map_modrinth_install_error)?;
                    let download = if let Some(version) = exact_version.as_ref() {
                        crate::sources::modrinth::resolve_version_detail(version, &instance.minecraft_version, instance.loader, ContentType::Mod)
                    } else {
                        modrinth.resolve_download(&project_id, &instance.minecraft_version, instance.loader, ContentType::Mod).await
                    }.map_err(map_modrinth_install_error)?;
                    Ok((summary, ModSource::Modrinth, download))
                }
                ResolvedDependency::Curseforge(project_id) => {
                    let uid = format!("curseforge:{project_id}");
                    if uid == root.uid || db.get_instance_mod(&instance.id, &uid)?.is_some() {
                        return Err(InstanceError::AlreadyInstalled);
                    }
                    let api_key = config.curseforge_api_key().ok_or(InstanceError::CurseForgeNotConfigured)?;
                    let (name, slug, icon_url, website_url) = curseforge.mods_batch(&[project_id], &api_key).await
                        .remove(&project_id).unwrap_or((format!("CurseForge project {project_id}"), String::new(), None, None));
                    let summary = ModSummary {
                        uid, name, slug, icon_url, project_type: ContentType::Mod,
                        sources: vec![ModSource::Curseforge], curseforge_id: Some(project_id),
                        modrinth_id: None, description: String::new(), author: String::new(),
                        downloads: 0, loaders: vec![instance.loader], updated_at: String::new(),
                    };
                    let download = match curseforge.resolve_download_with_key(project_id, &instance.minecraft_version, instance.loader, ContentType::Mod, &api_key).await {
                        Ok(download) => download,
                        Err(crate::sources::curseforge::CurseForgeError::DistributionRestricted { file_id, filename, sha1, dependencies }) => {
                            let website = website_url.as_deref();
                            queue.extend(dependencies);
                            missing.push(crate::dto::instance::MissingMod {
                                project_id, name: summary.name.clone(), filename,
                                url: curseforge_file_url(website, &summary.slug, file_id), sha1,
                            });
                            return Err(InstanceError::DistributionRestricted { file_id, filename: summary.name, sha1: None, dependencies: Vec::new() });
                        }
                        Err(error) => return Err(map_curseforge_install_error(error)),
                    };
                    Ok((summary, ModSource::Curseforge, download))
                }
            }
        }.await;
        match resolved {
            Ok((summary, source, download)) => {
                report(installed.len() as u32, MAX_DEPENDENCY_INSTALLS as u32, &summary.name);
                match install_dependency(db, instance, &summary, source, &download, cancel).await {
                    Ok(()) => {
                        queue.extend(download.dependencies);
                        installed.push(summary.name);
                    }
                    Err(error) => crate::activity::append_log(&format!("Could not install dependency of {}: {error}", root.name), "warn", None),
                }
            }
            Err(InstanceError::AlreadyInstalled) => {}
            Err(error) => crate::activity::append_log(&format!("Could not resolve dependency of {}: {error}", root.name), "warn", None),
        }
    }
    (installed, missing)
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

const PACK_LOADER_PROVENANCE: &str = ".waybound-pack-loader.json";

fn pack_loader_selection(
    loader: ModLoader,
    pin: Option<&str>,
    previous: Option<&PackDeclaredLoader>,
    declared: Option<&PackDeclaredLoader>,
) -> Result<(ModLoader, Option<String>, Option<PackDeclaredLoader>), InstanceError> {
    let Some(declared) = declared else { return Ok((loader, pin.map(str::to_owned), previous.cloned())); };
    let pack_owned = previous.is_some_and(|old| old.loader == loader && old.version.as_deref() == pin);
    if pin.is_some() && !pack_owned {
        if declared.loader != loader {
            return Err(InstanceError::Other(
                "Pack declares a different loader, but this instance has an explicit loader-version pin. Clear the pin before switching loaders; instance left unchanged.".into()
            ));
        }
        // Missing historical provenance is never proof of pack ownership.
        return Ok((loader, pin.map(str::to_owned), None));
    }
    Ok((declared.loader, declared.version.clone(), Some(declared.clone())))
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
    cancel: &CancelToken,
    report: &impl Fn(u32, u32, &str),
) -> Result<InstallModResult, InstanceError> {
    let source = pick_source(summary, preferred_source)?;
    let download = resolve_download(modrinth, curseforge, config, summary, source,
        &instance.minecraft_version, instance.loader, version_id).await?;
    let client = http_client().map_err(map_download_error)?;
    let bytes = download_bytes_capped_with_retry(&client, &download.url, cancel, crate::modpack::MAX_PACK_FILE_BYTES)
        .await.map_err(map_download_error)?;
    verify_hashes(&bytes, &download.hashes).map_err(map_download_error)?;
    let root = instance_root(&instance.id)?;
    let provenance_path = crate::download::contained_join(&root, PACK_LOADER_PROVENANCE).map_err(map_download_error)?;
    let previous: Option<PackDeclaredLoader> = match std::fs::read(provenance_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| InstanceError::Other(format!("Invalid pack loader provenance: {error}")))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let declared = crate::modpack::declared_loader_from_bytes(&bytes);
    let (loader, loader_version, provenance) = pack_loader_selection(
        instance.loader, instance.loader_version.as_deref(), previous.as_ref(), declared.as_ref())?;
    let prepared = if is_mrpack_bytes(&bytes) {
        prepare_modrinth_mrpack_bytes(&bytes, &root, modrinth, cancel, report).await
    } else if is_curseforge_modpack_zip(&bytes) {
        let api_key = config.curseforge_api_key().ok_or(InstanceError::CurseForgeNotConfigured)?;
        prepare_curseforge_modpack_zip(&bytes, &root, &api_key, cancel, report).await
    } else {
        return Err(InstanceError::Other("Unrecognized modpack format. Expected .mrpack or CurseForge manifest.json; instance left unchanged.".into()));
    }.map_err(map_modpack_error)?;
    let crate::modpack::PreparedModpackImport { mut transaction, result: import } = prepared;
    for row in db.list_instance_mods(&instance.id)? {
        if !row.file_name.ends_with(crate::commands::content::DISABLED_SUFFIX) { continue; }
        for (filename, uid) in &import.project_uids {
            if uid == &row.mod_uid && filename.to_ascii_lowercase().ends_with(".jar") {
                transaction.preserve_disabled(&format!("mods/{filename}")).map_err(map_modpack_error)?;
            }
        }
    }
    transaction.stage(PACK_LOADER_PROVENANCE, &serde_json::to_vec_pretty(&provenance)
        .map_err(|error| InstanceError::Other(error.to_string()))?).map_err(map_modpack_error)?;
    let label = import.version_label.clone().unwrap_or_else(|| strip_pack_extension(&download.filename));
    let icon = match summary.icon_url.as_deref() {
        Some(url) => Some(download_icon_data_url(url, cancel).await.unwrap_or_else(|| url.to_string())),
        None => None,
    };
    let mut installed = None;
    transaction.commit_with(cancel, || {
        installed = Some(db.publish_instance_pack(
            &instance.id, loader, loader_version.as_deref(), &label, &summary.uid,
            &summary.name, source, &download.filename, &root.display().to_string(), icon.as_deref(),
        ).map_err(|error| ModpackError::Other(format!("Could not publish pack metadata: {error}")))?);
        Ok(())
    }).await.map_err(map_modpack_error)?;
    let mut message = import.message;
    if let Err(error) = sync_mods_folder(db, &instance.id, &root.join("mods"), source,
        &import.icons, &import.content_names, &import.project_uids) {
        message.push_str(&format!(" Content tracking refresh failed: {error}. Pack files and loader metadata were installed."));
    }
    seed_content_meta_cache(db, &instance.id, &root.join("resourcepacks"), "resourcepack", &import.icons, &import.content_names);
    seed_content_meta_cache(db, &instance.id, &root.join("shaderpacks"), "shaderpack", &import.icons, &import.content_names);
    if let Some(icon) = &icon { let _ = db.set_instance_icon(&instance.id, Some(icon)); }
    if let Ok(mut launch_config) = db.get_instance_launch_config(&instance.id) {
        if launch_config.max_memory_mb.is_none() {
            let count = std::fs::read_dir(root.join("mods")).map(|entries| entries.flatten()
                .filter(|entry| entry.path().is_file() && entry.path().extension().is_some_and(|ext| ext == "jar"))
                .count()).unwrap_or(0) as u32;
            launch_config.max_memory_mb = Some(recommended_memory_mb(count));
            let _ = db.set_instance_launch_config(&instance.id, &launch_config);
        }
    }
    let mut refreshed = instance.clone();
    refreshed.loader = loader;
    refreshed.loader_version = loader_version;
    refreshed.modpack_project_uid = Some(summary.uid.clone());
    refreshed.modpack_version_label = Some(label);
    if let Some(icon) = icon { refreshed.icon = Some(icon); }
    if loader != instance.loader { message.push_str(" Instance loader changed to the pack-declared loader."); }
    Ok(InstallModResult {
        message, installed, instance: refreshed, has_skipped: import.has_skipped, missing_mods: import.missing_mods,
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

/// Strips a trailing `.zip`/`.mrpack` extension for a friendlier display
/// label (e.g. `"Ascendra-2.1.0.zip"` -> `"Ascendra-2.1.0"`). Deliberately
/// not a real parser — no attempt to pull out "just the version number",
/// since the raw filename minus its extension is an honest-enough label on
/// its own.
fn strip_pack_extension(filename: &str) -> String {
    filename
        .strip_suffix(".mrpack")
        .or_else(|| filename.strip_suffix(".zip"))
        .unwrap_or(filename)
        .to_string()
}

/// Decides the recorded origin for a fresh install row: an explicit
/// caller override wins (Browse passes User; update passes the row's),
/// otherwise a sidecar-claimed filename means pack content being
/// reinstalled or version-switched, and anything else is a genuine add.
/// Pure so the precedence is unit-testable without a database.
fn resolve_install_origin(
    explicit: Option<crate::dto::ModOrigin>,
    sidecar_claimed: bool,
) -> crate::dto::ModOrigin {
    explicit.unwrap_or(if sidecar_claimed {
        crate::dto::ModOrigin::Pack
    } else {
        crate::dto::ModOrigin::User
    })
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
    let root = mods_dir.parent().ok_or_else(|| InstanceError::Other("Mods directory has no instance root".into()))?;
    ensure_contained_path(root, mods_dir).map_err(map_download_error)?;
    if !mods_dir.exists() { return Ok(()); }
    let existing = db.list_instance_mods(instance_id)?;
    let pack_files = crate::db::pack_filenames(root);
    let mut seen = std::collections::HashSet::new();
    let mut recorded = std::collections::HashSet::new();
    for entry in std::fs::read_dir(mods_dir)? {
        let entry = entry?;
        let path = entry.path();
        ensure_contained_path(root, &path).map_err(map_download_error)?;
        if !path.is_file() { continue; }
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else { continue };
        let base = filename.strip_suffix(crate::commands::content::DISABLED_SUFFIX).unwrap_or(filename);
        if !base.to_ascii_lowercase().ends_with(".jar") { continue; }
        seen.insert(filename.to_string());
        let old_file = existing.iter().find(|row| row.file_name == filename);
        let resolved_uid = project_uids.get(base);
        let uid = resolved_uid.map(String::as_str)
            .or_else(|| old_file.filter(|row| !row.mod_uid.starts_with("file:")).map(|row| row.mod_uid.as_str()))
            .or_else(|| old_file.map(|row| row.mod_uid.as_str()))
            .map(str::to_owned).unwrap_or_else(|| format!("file:{filename}"));
        // A user-added older file for the same project is not removed or
        // allowed to steal the verified pack replacement's tracking row.
        if resolved_uid.is_none() && project_uids.values().any(|resolved| resolved == &uid) {
            continue;
        }
        let old = old_file.or_else(|| existing.iter().find(|row| row.mod_uid == uid));
        let origin = old.map_or_else(|| resolve_install_origin(None, pack_files.contains(base)), |row| row.origin);
        let name = content_names.get(base).map(String::as_str)
            .or_else(|| old.map(|row| row.mod_name.as_str())).unwrap_or_else(|| base.trim_end_matches(".jar"));
        let icon = icons.get(base).map(String::as_str).or_else(|| old.and_then(|row| row.icon_url.as_deref()));
        let row_source = if uid.starts_with("modrinth:") { ModSource::Modrinth }
            else if uid.starts_with("curseforge:") { ModSource::Curseforge }
            else { old.map_or(source, |row| row.source) };
        db.insert_instance_mod(instance_id, &uid, name, row_source, filename,
            &path.display().to_string(), icon, origin)?;
        if let Some(old) = old_file.filter(|row| row.mod_uid != uid) {
            db.delete_instance_mod(instance_id, &old.mod_uid)?;
        }
        db.delete_content_meta_cache(instance_id, "mod", filename)?;
        recorded.insert(uid);
    }
    for stale in existing.iter().filter(|row| !seen.contains(&row.file_name) && !recorded.contains(&row.mod_uid)) {
        // Keep archive metadata rows and non-mod content.
        let base = stale.file_name.strip_suffix(crate::commands::content::DISABLED_SUFFIX).unwrap_or(&stale.file_name);
        if base.to_ascii_lowercase().ends_with(".jar") {
            db.delete_instance_mod(instance_id, &stale.mod_uid)?;
        }
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
            None,
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

                    .resolve_version_by_id(vid, mc_version, loader, summary.project_type)

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

                    .resolve_file_by_id(mod_id, file_id, &api_key, mc_version, loader, summary.project_type)

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

        crate::sources::curseforge::CurseForgeError::WrongGameVersion { filename, file_versions, expected } => InstanceError::Other(format!(

            "{filename} is not built for Minecraft {expected} (it targets {}). This project has no {expected} release — pick a different version or mod.", file_versions.join(", ")

        )),

        crate::sources::curseforge::CurseForgeError::Rejected { message, .. } => {

            InstanceError::Other(message)

        }

        crate::sources::curseforge::CurseForgeError::Network(err) => InstanceError::Network(err),

        crate::sources::curseforge::CurseForgeError::DistributionRestricted { file_id, filename, sha1, dependencies } => {

            InstanceError::DistributionRestricted { file_id, filename, sha1, dependencies }

        }

    }

}

fn map_modrinth_install_error(err: ModrinthError) -> InstanceError {

    match err {

        ModrinthError::NotFound => InstanceError::Other(format!(

            "No compatible file found for this Minecraft version and loader. Try matching your instance version/loader to the mod, or pick a different mod version."

        )),

        ModrinthError::Incompatible => InstanceError::Other(

            "That version isn't built for this instance's Minecraft version and loader — pick a version row whose game version and loader match.".to_string(),

        ),

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
        crate::download::DownloadError::Conflict => InstanceError::Other(
            "File changed during installation; previous files were retained.".into()
        ),
        crate::download::DownloadError::HashMismatch(algorithm) => InstanceError::Other(format!(
            "Downloaded file failed {algorithm} integrity verification; previous files were retained."
        )),

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
    /// Exact upstream identity survives automatic and pinned resolution.
    pub curseforge_file_id: Option<u32>,
    pub hashes: std::collections::HashMap<String, String>,
    pub dependencies: Vec<ResolvedDependency>,

}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedDependency {
    Modrinth { project_id: Option<String>, version_id: Option<String> },
    Curseforge(u32),
}



#[cfg(test)]

mod tests {

    use super::{is_installable, is_reserved_windows_name, map_curseforge_install_error, resolve_install_origin, slugify, strip_pack_extension};

    use crate::dto::{ContentType, ModOrigin};



    #[test]

    fn modpacks_are_installable() {

        assert!(is_installable(ContentType::Modpack));

    }

    #[test]
    fn install_origin_explicit_wins_then_sidecar_then_user() {
        // Browse passes User explicitly: always yours, even overlapping a
        // pack file. Updates pass the row's origin through the same slot.
        assert_eq!(resolve_install_origin(Some(ModOrigin::User), true), ModOrigin::User);
        assert_eq!(resolve_install_origin(Some(ModOrigin::Pack), false), ModOrigin::Pack);
        // No context (version modal on an untracked file): the sidecar
        // decides — reinstalling pack content stays pack.
        assert_eq!(resolve_install_origin(None, true), ModOrigin::Pack);
        assert_eq!(resolve_install_origin(None, false), ModOrigin::User);
    }

    #[test]
    fn pack_loader_provenance_refreshes_only_pack_owned_pins() {
        use crate::dto::ModLoader;
        use crate::modpack::PackDeclaredLoader;
        let old = PackDeclaredLoader { loader: ModLoader::Forge, version: Some("old".into()) };
        let next = PackDeclaredLoader { loader: ModLoader::Forge, version: Some("new".into()) };
        let (_, pin, provenance) = super::pack_loader_selection(
            ModLoader::Forge, Some("old"), Some(&old), Some(&next)).unwrap();
        assert_eq!(pin.as_deref(), Some("new"));
        assert_eq!(provenance.unwrap().version.as_deref(), Some("new"));
        for previous in [None, Some(&old)] {
            let (_, pin, provenance) = super::pack_loader_selection(
                ModLoader::Forge, Some("user-pin"), previous, Some(&next)).unwrap();
            assert_eq!(pin.as_deref(), Some("user-pin"));
            assert!(provenance.is_none());
        }
        let (_, historical, provenance) = super::pack_loader_selection(
            ModLoader::Forge, Some("old"), None, Some(&next)).unwrap();
        assert_eq!(historical.as_deref(), Some("old"));
        assert!(provenance.is_none());
        let other = PackDeclaredLoader { loader: ModLoader::NeoForge, version: Some("next".into()) };
        assert!(super::pack_loader_selection(ModLoader::Forge, Some("old"), None, Some(&other)).is_err());
        let (loader, pin, _) = super::pack_loader_selection(
            ModLoader::Forge, Some("old"), Some(&old), Some(&other)).unwrap();
        assert_eq!(loader, ModLoader::NeoForge);
        assert_eq!(pin.as_deref(), Some("next"));
    }

    #[test]
    fn reserved_windows_device_names_are_flagged() {
        // Case matters here: `slugify` always lowercases before this check
        // ever runs, so only the lowercase forms need covering.
        for name in ["con", "prn", "aux", "nul", "com1", "lpt9"] {
            assert!(is_reserved_windows_name(name), "{name} should be reserved");
        }
        assert!(!is_reserved_windows_name("console"));
        assert!(!is_reserved_windows_name("con-2"));
    }

    #[test]
    fn reserved_name_survives_slugify() {
        // "CON" (a plausible real display name someone types) must still
        // collide with the reserved list after slugify lowercases it.
        assert_eq!(slugify("CON"), "con");
        assert!(is_reserved_windows_name(&slugify("CON")));
    }

    #[test]
    fn strips_known_pack_extensions() {
        assert_eq!(strip_pack_extension("Ascendra-2.1.0.zip"), "Ascendra-2.1.0");
        assert_eq!(strip_pack_extension("MyPack-1.0.mrpack"), "MyPack-1.0");
        // Unrecognized extension (or none) is left untouched rather than
        // guessed at.
        assert_eq!(strip_pack_extension("weird-pack.tar.gz"), "weird-pack.tar.gz");
    }

    #[test]
    fn wrong_game_version_error_names_the_mismatch() {
        // The Bigger Stacks case: 1.20.1-only file offered to a 1.21.1
        // instance must name both sides, not a bare "no compatible file".
        let err = map_curseforge_install_error(
            crate::sources::curseforge::CurseForgeError::WrongGameVersion {
                filename: "biggerstacks-1.20.1-2026.06.17-all.jar".to_string(),
                file_versions: vec!["1.20.1".to_string()],
                expected: "1.21.1".to_string(),
            },
        );
        let msg = err.to_string();
        assert!(msg.contains("biggerstacks-1.20.1-2026.06.17-all.jar"), "got: {msg}");
        assert!(msg.contains("1.21.1"), "got: {msg}");
        assert!(msg.contains("1.20.1"), "got: {msg}");
    }

}


