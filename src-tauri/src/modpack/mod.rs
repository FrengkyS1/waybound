mod curseforge;
mod modrinth;
mod preview;
mod transaction;

pub use curseforge::import_curseforge_modpack_zip;
pub use curseforge::is_curseforge_modpack_zip;
pub(crate) use curseforge::{curseforge_file_url, pending_missing_mods, remove_pack_manifest_entry};
pub use modrinth::import_modrinth_mrpack_bytes;
pub use modrinth::is_mrpack_bytes;
pub use preview::{preview_curseforge_modpack, preview_modrinth_modpack};

use thiserror::Error;

use crate::dto::ModLoader;

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

pub struct ModpackImportResult {    pub message: String,
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
    /// The pack's own version string, when the source format actually
    /// carries one. Modrinth's `.mrpack` index has a `versionId` field;
    /// CurseForge's manifest.json has no version field at all (only
    /// `name`/`files`/`overrides`), so its importer always leaves this
    /// `None` and the caller falls back to the downloaded archive's
    /// filename instead.
    pub version_label: Option<String>,
}

/// The loader (and exact build) a pack archive declares for itself.
///
/// A CurseForge pack's per-file list carries no loader signal — the truth
/// lives in `manifest.json`'s `minecraft.modLoaders[].id` (e.g.
/// `"neoforge-21.1.172"`); an `.mrpack`'s lives in `modrinth.index.json`'s
/// `dependencies` map (`"neoforge": "21.1.172"`). The Browse detail page
/// can't see either without downloading the archive, so its suggested
/// loader is a category guess that defaults to Forge — this is what put a
/// NeoForge pack (ATM10) on a Forge instance, where every NeoForge jar
/// silently fails to register and the game dies on "missing" mandatory
/// dependencies that are all sitting in `mods/`. The installer calls this
/// on the downloaded bytes and corrects the instance before importing.
pub struct PackDeclaredLoader {
    pub loader: ModLoader,
    /// Exact build from the manifest, when the pack pins one.
    pub version: Option<String>,
}

pub fn declared_loader_from_bytes(bytes: &[u8]) -> Option<PackDeclaredLoader> {
    if let Ok(manifest) = curseforge::read_cf_manifest(bytes) {
        let mut loaders = manifest.minecraft.map(|m| m.mod_loaders).unwrap_or_default();
        // Prefer the author's primary loader; fall back to the first listed.
        loaders.sort_by_key(|l| !l.primary);
        for entry in &loaders {
            let Some((kind, build)) = entry.id.split_once('-') else {
                continue;
            };
            let loader = match kind {
                "fabric" => ModLoader::Fabric,
                "forge" => ModLoader::Forge,
                "neoforge" => ModLoader::NeoForge,
                "quilt" => ModLoader::Quilt,
                _ => continue,
            };
            return Some(PackDeclaredLoader {
                loader,
                version: (!build.is_empty()).then(|| build.to_string()),
            });
        }
    }
    if let Ok(index) = modrinth::read_mrpack_index(bytes) {
        for (key, value) in &index.dependencies {
            let loader = match key.as_str() {
                "fabric-loader" => ModLoader::Fabric,
                "forge" => ModLoader::Forge,
                "neoforge" => ModLoader::NeoForge,
                "quilt-loader" => ModLoader::Quilt,
                // "minecraft" and anything future — not a loader.
                _ => continue,
            };
            return Some(PackDeclaredLoader {
                loader,
                version: (!value.is_empty()).then(|| value.clone()),
            });
        }
    }
    None
}

#[cfg(test)]
mod declared_loader_tests {
    use super::{declared_loader_from_bytes, PackDeclaredLoader};
    use crate::dto::ModLoader;
    use std::io::Write;

    fn zip_with(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            for (name, contents) in entries {
                writer
                    .start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(contents.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    fn check(bytes: &[u8]) -> Option<(ModLoader, Option<String>)> {
        declared_loader_from_bytes(bytes)
            .map(|PackDeclaredLoader { loader, version }| (loader, version))
    }

    #[test]
    fn cf_manifest_primary_loader_wins() {
        let bytes = zip_with(&[(
            "manifest.json",
            r#"{"manifestType":"minecraftModpack","manifestVersion":1,"name":"ATM10",
                "minecraft":{"version":"1.21.1","modLoaders":[
                    {"id":"forge-1.21.1-52.0.0","primary":false},
                    {"id":"neoforge-21.1.172","primary":true}]},
                "files":[]}"#,
        )]);
        let (loader, version) = check(&bytes).expect("declares a loader");
        assert_eq!(loader, ModLoader::NeoForge);
        assert_eq!(version.as_deref(), Some("21.1.172"));
    }

    #[test]
    fn cf_manifest_without_minecraft_section_declares_nothing() {
        let bytes = zip_with(&[(
            "manifest.json",
            r#"{"manifestType":"minecraftModpack","manifestVersion":1,"name":"Pack","files":[]}"#,
        )]);
        assert!(check(&bytes).is_none());
    }

    #[test]
    fn mrpack_dependencies_declare_loader() {
        let bytes = zip_with(&[(
            "modrinth.index.json",
            r#"{"formatVersion":1,"game":"minecraft","versionId":"8.1","name":"ATM10",
                "dependencies":{"minecraft":"1.21.1","neoforge":"21.1.172"},"files":[]}"#,
        )]);
        let (loader, version) = check(&bytes).expect("declares a loader");
        assert_eq!(loader, ModLoader::NeoForge);
        assert_eq!(version.as_deref(), Some("21.1.172"));
    }

    #[test]
    fn garbage_bytes_declare_nothing() {
        assert!(check(b"definitely not a zip").is_none());
        // A zip with neither manifest is not a pack at all.
        assert!(check(&zip_with(&[("overrides/x.txt", "hi")])).is_none());
    }
}
