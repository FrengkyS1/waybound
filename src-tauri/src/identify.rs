//! Content-hash mod identification — PrismLauncher's core trick, ported:
//! match an on-disk jar back to its source project + exact version without
//! knowing either beforehand. Modrinth: `POST /version_files` by sha1
//! (exact version object per hash). CurseForge: `POST /fingerprints` by
//! MurmurHash2 (see `fingerprint.rs`). Powers version/update flows for jars
//! Browse never installed (modpack drops, manual adds, renames) instead of
//! rejecting them as untrackable.

use std::path::{Path, PathBuf};

use serde::Serialize;

use sha1::Digest;

use crate::commands::content::DISABLED_SUFFIX;
use crate::config::ConfigStore;
use crate::download::safe_join;
use crate::dto::{ContentType, ModSource, ModSummary};
use crate::sources::curseforge::CurseForgeClient;
use crate::sources::modrinth::{map_version_summary, ModrinthClient};

/// One identified local jar: who it is upstream and exactly which version
/// the bytes correspond to.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentifiedMod {
    /// The installed file's name as found on disk (no `.disabled` suffix).
    pub file_name: String,
    /// A real project summary — the frontend can feed it straight into the
    /// normal detail/versions/install path.
    pub summary: ModSummary,
    pub version_id: String,
    pub version_number: String,
    /// Installer filename of the matched version (primary-or-first file).
    pub matched_file_name: Option<String>,
}

/// Hex sha1 of a file's exact bytes — the hash Modrinth's `version_files`
/// endpoint matches on.
pub fn sha1_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = sha1::Sha1::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Resolves an instance-relative mods-folder path, tolerating the
/// `.disabled` suffix the Content tab strips for display.
fn resolve_mod_path(instance_id: &str, file_name: &str) -> Result<(PathBuf, String), String> {
    let root = crate::instances::paths::instance_root(instance_id).map_err(|e| e.to_string())?;
    let mods = root.join("mods");
    for candidate in [file_name.to_string(), format!("{file_name}{DISABLED_SUFFIX}")] {
        let path = safe_join(&mods, &candidate).map_err(|e| e.to_string())?;
        if path.is_file() {
            return Ok((path, candidate));
        }
    }
    Err(format!("'{file_name}' is not present in this instance."))
}

/// Identifies one on-disk jar by content hash: Modrinth first (no key, full
/// version object in one call), then CurseForge fingerprints for whatever
/// Modrinth doesn't know. Tracked files never reach here — the DB row is
/// cheaper and more precise — this is the fallback for untracked ones.
pub async fn identify_mod_file(
    modrinth: &ModrinthClient,
    curseforge: &CurseForgeClient,
    config: &ConfigStore,
    instance_id: &str,
    file_name: &str,
) -> Result<IdentifiedMod, String> {
    let (path, _on_disk_name) = resolve_mod_path(instance_id, file_name)?;

    // Modrinth first: no key needed, and the response is the full version
    // object (project + exact version + files) in one call. Streaming hash
    // — no need to hold the whole jar in memory.
    let hash = sha1_file(&path)?;
    let versions = modrinth
        .lookup_versions_by_hashes(std::slice::from_ref(&hash))
        .await
        .map_err(|e| e.to_string())?;
    if let Some(version) = versions.get(&hash) {
        if version.project_id.is_empty() {
            return Err("Modrinth matched the file but returned no project.".to_string());
        }
        let summary = modrinth
            .fetch_project_summary(&version.project_id)
            .await
            .map_err(|e| e.to_string())?;
        let matched_file_name = map_version_summary(version).file_name;
        return Ok(IdentifiedMod {
            file_name: file_name.to_string(),
            summary: ModSummary {
                project_type: ContentType::Mod,
                sources: vec![ModSource::Modrinth],
                ..summary
            },
            version_id: version.id.clone(),
            version_number: version.version_number.clone(),
            matched_file_name,
        });
    }

    // Whatever Modrinth doesn't know, CurseForge fingerprints may — same
    // per-file cost, but needs the API key.
    if let Some(api_key) = config.curseforge_api_key() {
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let fingerprint = crate::fingerprint::curseforge_fingerprint(&bytes);
        if let Ok(matches) = curseforge.match_fingerprints(&[fingerprint], &api_key).await {
            if let Some(m) = matches.first() {
                let minimal = ModSummary {
                    uid: format!("curseforge:{}", m.file.mod_id),
                    slug: String::new(),
                    name: String::new(),
                    description: String::new(),
                    author: String::new(),
                    icon_url: None,
                    downloads: 0,
                    project_type: ContentType::Mod,
                    loaders: Vec::new(),
                    sources: vec![ModSource::Curseforge],
                    updated_at: String::new(),
                    curseforge_id: Some(m.file.mod_id),
                    modrinth_id: None,
                };
                // One call for the project's real name/icon — the versions
                // list it fetches also covers the "which version is this"
                // display when the matched file is recent enough to be in it.
                if let Ok(detail) = curseforge.fetch_mod_detail(&minimal, &api_key).await {
                    let version_number = detail
                        .versions
                        .iter()
                        .find(|v| v.id == m.id.to_string())
                        .map(|v| v.version_number.clone())
                        .unwrap_or_else(|| m.file.file_name.clone());
                    return Ok(IdentifiedMod {
                        file_name: file_name.to_string(),
                        summary: detail.summary,
                        version_id: m.id.to_string(),
                        version_number,
                        matched_file_name: Some(m.file.file_name.clone()),
                    });
                }
            }
        }
    }

    Err("Couldn't identify this file on Modrinth or CurseForge — it may be a renamed file or a manually-built jar.".to_string())
}

#[cfg(test)]
mod identify_tests {
    use super::sha1_file;

    #[test]
    fn sha1_matches_known_vector() {
        let dir = std::env::temp_dir().join("waybound-identify-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("abc.bin");
        std::fs::write(&path, b"abc").unwrap();
        // Cross-checked with Python's hashlib.
        assert_eq!(
            sha1_file(&path).unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha1_missing_file_errors() {
        assert!(sha1_file(std::path::Path::new("C:/waybound-test-nonexistent/missing.jar")).is_err());
    }
}
