pub mod paths;
pub mod operations;






use std::path::Path;



use crate::config::ConfigStore;

use crate::db::Database;

use crate::download::{
    download_bytes_with_retry, download_to_file_verified, http_client, safe_join,
    verify_hashes, CancelToken,
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
        let mut name = format!("{} (copy)", source.name);
        let mut n = 2;
        while existing.contains(&name) {
            name = format!("{} (copy {n})", source.name);
            n += 1;
        }

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

        if !db.delete_instance(id)? {

            return Err(InstanceError::NotFound);

        }

        if let Ok(path) = instance_root(id) {

            // Recycle Bin, not a permanent wipe — a deleted instance can
            // carry hundreds of hours of world saves, and `remove_dir_all`
            // gave no way back from a misclick.
            let _ = trash::delete(path);

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



        if !update_existing && db.get_instance_mod(instance_id, &summary.uid)?.is_some() {

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

        download_to_file_verified(&client, &download.url, &dest_path, cancel, &download.hashes)

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

        // Origin for the new row: explicit caller override wins, else a
        // sidecar-claimed filename means reinstalling pack content.
        let sidecar_claimed = instance_root(instance_id)
            .ok()
            .map(|root| crate::db::pack_filenames(&root).contains(&download.filename))
            .unwrap_or(false);
        let installed = db.insert_instance_mod(

            instance_id,

            &summary.uid,

            &summary.name,

            source,

            &download.filename,

            &dest_path.display().to_string(),

            icon.as_deref(),

            resolve_install_origin(origin, sidecar_claimed),

        )?;



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
                    source,
                    version_id,
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



/// Ceiling on how many dependencies one install may pull in. A real mod's
/// required-dependency closure is small (a library or two, occasionally a
/// chain of three); anything approaching this means the graph is cyclic in a
/// way the visited-set missed, or a project mislabels optional deps as
/// required. Stopping is better than silently downloading a hundred files
/// the user never asked for.
const MAX_DEPENDENCY_INSTALLS: usize = 24;

/// Installs one already-identified Modrinth dependency: resolve, download,
/// record. Deliberately narrower than `install_mod` — a dependency is always
/// a plain mod from Modrinth, so none of that function's modpack branch or
/// CurseForge distribution-restriction handling can apply, and reusing it
/// would mean recursing through dependency resolution a second time.
async fn install_dependency(
    db: &Database,
    modrinth: &ModrinthClient,
    instance: &InstanceSummary,
    summary: &ModSummary,
    cancel: &crate::download::CancelToken,
) -> Result<(), InstanceError> {
    let download = modrinth
        .query_versions(
            summary.modrinth_id.as_deref().unwrap_or_default(),
            Some(&instance.minecraft_version),
            Some(instance.loader.as_modrinth()),
        )
        .await
        .map_err(map_modrinth_install_error)?;

    let dest_dir = ensure_instance_dirs(&instance.id)?;
    let dest_path = safe_join(&dest_dir, &download.filename).map_err(map_download_error)?;

    let client = http_client().map_err(map_download_error)?;
    download_to_file_verified(&client, &download.url, &dest_path, cancel, &download.hashes)
        .await
        .map_err(map_download_error)?;

    let icon = match summary.icon_url.as_deref() {
        Some(icon_url) => Some(
            download_icon_data_url(icon_url, cancel)
                .await
                .unwrap_or_else(|| icon_url.to_string()),
        ),
        None => None,
    };

    db.insert_instance_mod(
        &instance.id,
        &summary.uid,
        &summary.name,
        crate::dto::ModSource::Modrinth,
        &download.filename,
        &dest_path.display().to_string(),
        icon.as_deref(),
        // Dependency pulled in by a user install — travels with it.
        crate::dto::ModOrigin::User,
    )?;

    Ok(())
}

/// One pending dependency, tagged by source so a single queue can carry both
/// kinds. The tag decides how it's resolved, installed, and recorded.
#[derive(Debug, Clone, PartialEq)]
enum DependencyRef {
    Modrinth(String),
    Curseforge(u32),
}

/// The required dependencies of whichever exact version/root selection the
/// install just used. For a user-picked version id this reads THAT version's
/// dependency list (not whatever the auto-resolver would pick now); for
/// auto-resolution the source-specific helper re-derives the same choice the
/// download just made. Best-effort throughout — an empty list just means no
/// dependency walk, never a failed install.
async fn root_dependency_refs(
    modrinth: &ModrinthClient,
    curseforge: &CurseForgeClient,
    config: &ConfigStore,
    root: &ModSummary,
    instance: &InstanceSummary,
    source: ModSource,
    version_id: Option<&str>,
) -> Vec<DependencyRef> {
    match source {
        ModSource::Modrinth => {
            let project_id = root
                .modrinth_id
                .clone()
                .unwrap_or_else(|| root.slug.clone());
            if let Some(vid) = version_id {
                if let Ok(version) = modrinth.fetch_version_detail(vid).await {
                    return crate::sources::modrinth::required_dependency_ids_of(&version)
                        .into_iter()
                        .map(DependencyRef::Modrinth)
                        .collect();
                }
            }
            modrinth
                .required_dependency_ids(
                    &project_id,
                    &instance.minecraft_version,
                    instance.loader.as_modrinth(),
                )
                .await
                .into_iter()
                .map(DependencyRef::Modrinth)
                .collect()
        }
        ModSource::Curseforge => {
            let Some(mod_id) = root.curseforge_id else {
                return Vec::new();
            };
            let Some(api_key) = config.curseforge_api_key() else {
                return Vec::new();
            };
            if let Some(vid) = version_id.and_then(|v| v.parse::<u32>().ok()) {
                return curseforge
                    .file_dependency_mod_ids(mod_id, vid, &api_key)
                    .await
                    .into_iter()
                    .map(DependencyRef::Curseforge)
                    .collect();
            }
            curseforge
                .required_dependency_mod_ids(
                    mod_id,
                    &instance.minecraft_version,
                    instance.loader,
                    &api_key,
                )
                .await
                .into_iter()
                .map(DependencyRef::Curseforge)
                .collect()
        }
    }
}

/// Walks the required-dependency graph of a just-installed mod and installs
/// whatever the instance is missing, returning the names actually added plus
/// any dependencies that need a manual download (CurseForge authors can
/// disable third-party distribution on individual files).
///
/// Breadth-first with an explicit queue rather than recursion, so the
/// visited-set is trivially correct and there's no boxed async recursion.
/// Every failure is swallowed on purpose: this runs *after* the mod the user
/// asked for is already on disk, so a dependency that can't be resolved must
/// leave them with a working install and a note, not an error that undoes it.
async fn install_required_dependencies(
    db: &Database,
    modrinth: &ModrinthClient,
    curseforge: &CurseForgeClient,
    config: &ConfigStore,
    instance: &InstanceSummary,
    root: &ModSummary,
    source: ModSource,
    version_id: Option<&str>,
    cancel: &crate::download::CancelToken,
    report: &impl Fn(u32, u32, &str),
) -> (Vec<String>, Vec<crate::dto::instance::MissingMod>) {
    let mut queue: std::collections::VecDeque<DependencyRef> =
        root_dependency_refs(modrinth, curseforge, config, root, instance, source, version_id)
            .await
            .into_iter()
            .collect();
    let mut visited: Vec<DependencyRef> = Vec::new();
    let mut installed: Vec<String> = Vec::new();
    let mut missing: Vec<crate::dto::instance::MissingMod> = Vec::new();

    while let Some(dep) = queue.pop_front() {
        if cancel.is_cancelled() || installed.len() >= MAX_DEPENDENCY_INSTALLS {
            break;
        }
        // Successful installs are recorded in the DB immediately (which breaks
        // cycles on its own), but a dependency that FAILED to install leaves
        // no DB row — without this set, two mods sharing a failing dependency
        // would each retry it.
        if visited.contains(&dep) {
            continue;
        }
        visited.push(dep.clone());

        match dep {
            DependencyRef::Modrinth(project_id) => {
                // Already present: its own dependencies came with it, so
                // there's nothing further to walk down this branch.
                if db
                    .get_instance_mod(&instance.id, &format!("modrinth:{project_id}"))
                    .ok()
                    .flatten()
                    .is_some()
                {
                    continue;
                }

                let Ok(summary) = modrinth.fetch_project_summary(&project_id).await else {
                    crate::activity::append_log(
                        &format!("Dependency lookup failed for Modrinth project {project_id}"),
                        "warn",
                        None,
                    );
                    continue;
                };

                report(installed.len() as u32, MAX_DEPENDENCY_INSTALLS as u32, &summary.name);

                match install_dependency(db, modrinth, instance, &summary, cancel).await {
                    Ok(()) => {
                        crate::activity::append_log(
                            &format!(
                                "Installed {} to {} as a required dependency of {}",
                                summary.name, instance.name, root.name
                            ),
                            "info",
                            Some(&summary.uid),
                        );
                        installed.push(summary.name.clone());
                        let loader = instance.loader.as_modrinth();
                        queue.extend(
                            modrinth
                                .required_dependency_ids(
                                    &project_id,
                                    &instance.minecraft_version,
                                    loader,
                                )
                                .await
                                .into_iter()
                                .map(DependencyRef::Modrinth),
                        );
                    }
                    Err(err) => {
                        // Most commonly: the dependency has no build for this
                        // exact MC version + loader. Worth telling the user
                        // about, not worth failing their install over.
                        crate::activity::append_log(
                            &format!(
                                "Could not install required dependency {} for {}: {err}",
                                summary.name, root.name
                            ),
                            "warn",
                            None,
                        );
                    }
                }
            }
            DependencyRef::Curseforge(mod_id) => {
                let uid = format!("curseforge:{mod_id}");
                if db
                    .get_instance_mod(&instance.id, &uid)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    continue;
                }

                let Some(api_key) = config.curseforge_api_key() else {
                    continue;
                };
                let (name, slug, icon_url, website_url) = curseforge
                    .mods_batch(&[mod_id], &api_key)
                    .await
                    .remove(&mod_id)
                    .unwrap_or((
                        format!("CurseForge project {mod_id}"),
                        String::new(),
                        None,
                        None,
                    ));

                report(installed.len() as u32, MAX_DEPENDENCY_INSTALLS as u32, &name);

                match curseforge
                    .fetch_file(mod_id, &instance.minecraft_version, instance.loader, &api_key)
                    .await
                {
                    Ok(download) => {
                        let recorded: Result<(), InstanceError> = async {
                            let dest_dir = ensure_instance_dirs(&instance.id)?;
                            let dest_path = safe_join(&dest_dir, &download.filename)
                                .map_err(map_download_error)?;
                            let client = http_client().map_err(map_download_error)?;
                            download_to_file_verified(&client, &download.url, &dest_path, cancel, &download.hashes)
                                .await
                                .map_err(map_download_error)?;

                            let icon = match &icon_url {
                                Some(icon_url) => Some(
                                    download_icon_data_url(icon_url, cancel)
                                        .await
                                        .unwrap_or_else(|| icon_url.clone()),
                                ),
                                None => None,
                            };

                            db.insert_instance_mod(
                                &instance.id,
                                &uid,
                                &name,
                                crate::dto::ModSource::Curseforge,
                                &download.filename,
                                &dest_path.display().to_string(),
                                icon.as_deref(),
                                // Dependency pulled in by a user install — travels with it.
                                crate::dto::ModOrigin::User,
                            )?;
                            Ok(())
                        }
                        .await;

                        match recorded {
                            Ok(()) => {
                                crate::activity::append_log(
                                    &format!(
                                        "Installed {} to {} as a required dependency of {}",
                                        name, instance.name, root.name
                                    ),
                                    "info",
                                    Some(&uid),
                                );
                                installed.push(name.clone());
                                queue.extend(
                                    curseforge
                                        .required_dependency_mod_ids(
                                            mod_id,
                                            &instance.minecraft_version,
                                            instance.loader,
                                            &api_key,
                                        )
                                        .await
                                        .into_iter()
                                        .map(DependencyRef::Curseforge),
                                );
                            }
                            Err(err) => {
                                crate::activity::append_log(
                                    &format!(
                                        "Could not install required dependency {name} for {}: {err}",
                                        root.name
                                    ),
                                    "warn",
                                    None,
                                );
                            }
                        }
                    }
                    Err(crate::sources::curseforge::CurseForgeError::DistributionRestricted { file_id, filename, sha1 }) => {
                        // Same handling as a restricted direct install: hand
                        // the user an exact manual-download link instead of
                        // failing or silently skipping.
                        missing.push(crate::dto::instance::MissingMod {
                            project_id: mod_id,
                            name: name.clone(),
                            filename,
                            url: curseforge_file_url(website_url.as_deref(), &slug, file_id),
                            sha1,
                        });
                        crate::activity::append_log(
                            &format!(
                                "Required dependency {name} of {} needs a manual download",
                                root.name
                            ),
                            "warn",
                            None,
                        );
                    }
                    Err(err) => {
                        crate::activity::append_log(
                            &format!(
                                "Could not install required dependency {name} for {}: {err}",
                                root.name
                            ),
                            "warn",
                            None,
                        );
                    }
                }
            }
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

    let bytes = download_bytes_with_retry(&client, &download.url, cancel)
        .await
        .map_err(map_download_error)?;
    verify_hashes(&bytes, &download.hashes).map_err(map_download_error)?;

    // A pack archive declares its own loader (CurseForge `manifest.json` /
    // the mrpack index) — the only reliable signal. The Browse suggestion
    // that created this instance is a category guess defaulting to Forge,
    // which is how a NeoForge pack ends up on a Forge instance: every
    // NeoForge jar then fails to register and the game dies on "missing"
    // mandatory dependencies that are all sitting in `mods/`. Correct the
    // instance BEFORE importing so the launch uses the pack's real loader.
    let mut loader_note: Option<String> = None;
    if let Some(declared) = crate::modpack::declared_loader_from_bytes(&bytes) {
        if declared.loader != instance.loader {
            db.set_instance_loader(&instance.id, declared.loader)?;
            // A pin for the old loader is meaningless under the new one.
            db.set_instance_loader_version(&instance.id, None)?;
        }
        // Pin the pack's exact build when it declares one and the instance
        // has no explicit pin — the pack was built and tested against
        // exactly this. An existing user pin for the same loader is left
        // alone.
        if instance.loader_version.is_none() {
            if let Some(build) = declared.version.as_deref() {
                db.set_instance_loader_version(&instance.id, Some(build))?;
            }
        }
        if declared.loader != instance.loader {
            let name = match declared.loader {
                ModLoader::Fabric => "Fabric",
                ModLoader::Forge => "Forge",
                ModLoader::NeoForge => "NeoForge",
                ModLoader::Quilt => "Quilt",
                ModLoader::Vanilla => "Vanilla",
            };
            loader_note = Some(format!(
                "Instance loader set to {name}{} from the pack itself.",
                declared
                    .version
                    .as_deref()
                    .map(|b| format!(" {b}"))
                    .unwrap_or_default()
            ));
        }
    }

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

    // Only reached for a real modpack import (the plain-mod-jar path returns
    // early above) — record the pack's own version signal for display on the
    // Overview tab. The mrpack index's `versionId` is more reliable when
    // present (CurseForge's manifest.json has no version field at all), so
    // it wins; otherwise fall back to the downloaded archive's own filename.
    let modpack_version_label = import
        .version_label
        .clone()
        .unwrap_or_else(|| strip_pack_extension(&download.filename));
    let _ = db.set_modpack_version_label(&instance.id, Some(&modpack_version_label));
    // Remember which pack project this came from — without the uid the
    // instance can show the pack's version label but never offer its other
    // versions for in-place switching.
    let _ = db.set_modpack_project_uid(&instance.id, Some(&summary.uid));



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

        // The pack archive itself — definitionally pack-placed.
        crate::dto::ModOrigin::Pack,

    )?;



    let mut message = import.message;
    if let Some(note) = loader_note {
        message = format!("{message} {note}");
    }

    Ok(InstallModResult {
        message,
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
                    // Same file, better id — the row's origin survives the swap.
                    row.origin,
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



    let _ = db.insert_instance_mods_batch(instance_id, &mods, crate::dto::ModOrigin::Pack);

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

                    .resolve_version_by_id(vid, mc_version, loader)

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

                    .resolve_file_by_id(mod_id, file_id, &api_key, mc_version, loader)

                    .await

                    .map(|(download, _)| download)

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
    pub hashes: std::collections::HashMap<String, String>,

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


