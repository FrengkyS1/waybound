//! Content-hash mod identification — PrismLauncher's core trick, ported:
//! match an on-disk jar back to its source project + exact version without
//! knowing either beforehand. Modrinth: `POST /version_files` by sha1
//! (exact version object per hash). CurseForge: `POST /fingerprints` by
//! MurmurHash2 (see `fingerprint.rs`). Powers version/update flows for jars
//! Browse never installed (modpack drops, manual adds, renames) instead of
//! rejecting them as untrackable.

use std::future::Future;
use std::path::{Path, PathBuf};

use serde::Serialize;

use sha1::Digest;

use crate::config::ConfigStore;
use crate::dto::{ContentType, ModSource, ModSummary};
use crate::sources::curseforge::CurseForgeClient;
use crate::sources::modrinth::{map_version_summary, ModrinthClient};

/// One identified local jar: who it is upstream and exactly which version
/// the bytes correspond to.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentifiedMod {
    /// Exact physical basename, including `.disabled` when disabled.
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

/// Resolve only the supplied physical filename, never its enabled/disabled twin.
fn resolve_mod_path(instance_id: &str, file_name: &str) -> Result<(PathBuf, String), String> {
    let root = crate::instances::paths::instance_root(instance_id).map_err(|e| e.to_string())?;
    let mods = root.join("mods");
    let path = crate::download::contained_join(&mods, file_name).map_err(|e| e.to_string())?;
    if path.is_file() { return Ok((path, file_name.to_string())); }
    Err(format!("'{file_name}' is not present in this instance."))
}

/// Identify exact file bytes. A known install source constrains lookup so
/// missing legacy identity never silently substitutes another distributor.
pub async fn identify_mod_file(
    modrinth: &ModrinthClient,
    curseforge: &CurseForgeClient,
    config: &ConfigStore,
    instance_id: &str,
    file_name: &str,
    source: Option<ModSource>,
) -> Result<IdentifiedMod, String> {
    let (path, _on_disk_name) = resolve_mod_path(instance_id, file_name)?;
    let hash = sha1_file(&path)?;
    let identified = identify_with_fallback(
        async {
            if source == Some(ModSource::Curseforge) { return Ok(None); }
            let versions = modrinth.lookup_versions_by_hashes(std::slice::from_ref(&hash))
                .await.map_err(|e| e.to_string())?;
            let Some(version) = versions.get(&hash) else { return Ok(None); };
            if version.project_id.is_empty() {
                return Err("Modrinth matched the file but returned no project.".to_string());
            }
            let summary = modrinth.fetch_project_summary(&version.project_id)
                .await.map_err(|e| e.to_string())?;
            Ok(Some(IdentifiedMod {
                file_name: file_name.to_string(),
                summary,
                version_id: version.id.clone(),
                version_number: version.version_number.clone(),
                matched_file_name: map_version_summary(version).file_name,
            }))
        },
        async {
            if source == Some(ModSource::Modrinth) { return Ok(None); }
            let Some(api_key) = config.curseforge_api_key() else { return Ok(None); };
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let fingerprint = crate::fingerprint::curseforge_fingerprint(&bytes);
            let matches = curseforge.match_fingerprints(&[fingerprint], &api_key)
                .await.map_err(|e| e.to_string())?;
            let Some(m) = matches.first() else { return Ok(None); };
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
            let detail = curseforge.fetch_mod_detail(&minimal, &api_key)
                .await.map_err(|e| e.to_string())?;
            let version_number = detail.versions.iter()
                .find(|v| v.id == m.id.to_string())
                .map(|v| v.version_number.clone())
                .unwrap_or_else(|| m.file.file_name.clone());
            Ok(Some(IdentifiedMod {
                file_name: file_name.to_string(),
                summary: detail.summary,
                version_id: m.id.to_string(),
                version_number,
                matched_file_name: Some(m.file.file_name.clone()),
            }))
        },
    ).await?;
    identified.ok_or_else(|| "Couldn't identify this file on Modrinth or CurseForge — it may be a renamed file or a manually-built jar.".to_string())
}

/// Network failures do not prevent trying an independent configured source.
async fn identify_with_fallback<T>(
    primary: impl Future<Output = Result<Option<T>, String>>,
    fallback: impl Future<Output = Result<Option<T>, String>>,
) -> Result<Option<T>, String> {
    let primary_error = match primary.await {
        Ok(Some(value)) => return Ok(Some(value)),
        Ok(None) => None,
        Err(error) => Some(error),
    };
    match fallback.await {
        Ok(Some(value)) => Ok(Some(value)),
        Ok(None) => match primary_error {
            Some(error) => Err(error),
            None => Ok(None),
        },
        Err(error) => Err(match primary_error {
            Some(primary) => format!("Modrinth: {primary}; CurseForge: {error}"),
            None => error,
        }),
    }
}

#[cfg(test)]
mod identify_tests {
    use super::identify_with_fallback;

    #[tokio::test]
    async fn failed_modrinth_still_attempts_curseforge() {
        let attempted = std::cell::Cell::new(false);
        let identified = identify_with_fallback(
            async { Err::<Option<&str>, _>("Modrinth unavailable".to_string()) },
            async { attempted.set(true); Ok(Some("curseforge:42")) },
        ).await.unwrap();
        assert!(attempted.get());
        assert_eq!(identified, Some("curseforge:42"));
    }

    #[tokio::test]
    async fn successful_modrinth_does_not_query_curseforge() {
        let identified = identify_with_fallback(
            async { Ok::<_, String>(Some("modrinth:known")) },
            async { panic!("fallback must remain lazy"); #[allow(unreachable_code)] Ok(None) },
        ).await.unwrap();
        assert_eq!(identified, Some("modrinth:known"));
    }
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
