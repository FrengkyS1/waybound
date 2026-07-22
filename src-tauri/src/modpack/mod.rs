mod curseforge;
mod modrinth;
mod preview;

pub use curseforge::import_curseforge_modpack_zip;
pub use curseforge::is_curseforge_modpack_zip;
pub(crate) use curseforge::{curseforge_file_url, pending_missing_mods, remove_pack_manifest_entry};
pub use modrinth::import_modrinth_mrpack_bytes;
pub use modrinth::is_mrpack_bytes;
pub use preview::{preview_curseforge_modpack, preview_modrinth_modpack};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModpackError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("download error: {0}")]
    Download(#[from] crate::download::DownloadError),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

pub struct ModpackImportResult {
    pub message: String,
    /// True when one or more files couldn't be resolved automatically (a
    /// CurseForge author disabled third-party distribution, or the file was
    /// otherwise unreachable) — `message` lists them with a manual-download
    /// link. Lets the frontend keep that notification on screen instead of
    /// auto-dismissing it like a routine success.
    pub has_skipped: bool,
    /// file name -> icon URL, for mods this import resolved. The CurseForge
    /// importer reads this straight off its manifest's `projectID`; the
    /// Modrinth importer has no project id in its index at all, so it
    /// resolves one via a batched hash lookup instead. `sync_mods_folder`
    /// uses this to give modpack-installed mods an icon on record.
    pub icons: std::collections::HashMap<String, String>,
    /// file name -> the source platform's own project name, for every file
    /// this import resolved — not just mods. A mod jar usually has its own
    /// embedded display name, but a resource/shader pack has no equivalent
    /// convention at all, so this is the only source of a real name (vs. a
    /// humanized guess from the raw filename) for those two categories.
    pub content_names: std::collections::HashMap<String, String>,
    /// file name -> `"curseforge:<id>"` / `"modrinth:<id>"`, for every file
    /// this import resolved a real project for. `sync_mods_folder` uses this
    /// as the file's tracking id instead of falling back to an untrackable
    /// `file:<name>` record — without it, an "update this mod" feature would
    /// have nothing to re-resolve against for anything installed via a
    /// modpack, which is the overwhelming majority of a typical library.
    pub project_uids: std::collections::HashMap<String, String>,
    /// Files the author blocked from third-party download — empty for the
    /// Modrinth importer, which has no equivalent restriction.
    pub missing_mods: Vec<crate::dto::instance::MissingMod>,
}
