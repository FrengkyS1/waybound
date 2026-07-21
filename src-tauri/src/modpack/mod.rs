mod curseforge;
mod modrinth;
mod preview;

pub use curseforge::import_curseforge_modpack_zip;
pub use curseforge::is_curseforge_modpack_zip;
pub(crate) use curseforge::{curseforge_file_url, pending_missing_mods};
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
    /// file name -> icon URL, for mods this import resolved via CurseForge's
    /// API. `sync_mods_folder` uses this to give modpack-installed mods an
    /// icon on record — empty for the Modrinth importer (its manifest
    /// doesn't carry per-file icon data).
    pub icons: std::collections::HashMap<String, String>,
    /// Files the author blocked from third-party download — empty for the
    /// Modrinth importer, which has no equivalent restriction.
    pub missing_mods: Vec<crate::dto::instance::MissingMod>,
}
