use std::io::{Cursor, Read};
use std::collections::HashMap;
use std::path::Path;

use futures::stream::StreamExt;
use serde::Deserialize;

use super::{ModpackError, ModpackImportResult};
use super::transaction::{validate_pack_path, PackTransaction};
use crate::download::{download_bytes_with_retry, http_client, safe_join, verify_hashes, CancelToken, DOWNLOAD_CONCURRENCY};
use crate::sources::modrinth::ModrinthClient;

#[derive(Debug, Deserialize)]
pub struct ModrinthPackIndex {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "versionId")]
    pub version_id: String,
    pub files: Vec<ModrinthPackFile>,
    /// `{"minecraft": "1.21.1", "neoforge": "21.1.172", ...}` — the only
    /// loader signal an `.mrpack` carries. Same role as CurseForge's
    /// `manifest.minecraft.modLoaders`; see `declared_loader_from_bytes`.
    #[serde(default)]
    pub dependencies: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModrinthPackFile {
    pub path: String,
    pub downloads: Vec<String>,
    #[serde(default)]
    pub env: Option<ModrinthPackEnv>,
    #[serde(default)]
    pub hashes: Option<HashMap<String, String>>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct ModrinthPackEnv {
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub server: Option<String>,
}

pub async fn import_modrinth_mrpack_bytes(
    bytes: &[u8],
    instance_root: &Path,
    modrinth: &ModrinthClient,
    cancel: &CancelToken,
    report: &impl Fn(u32, u32, &str),
) -> Result<ModpackImportResult, ModpackError> {
    let index = read_index_from_mrpack(bytes)?;
    let client = http_client()?;
    let client = &client;
    let mut transaction = PackTransaction::new(instance_root)?;
    let mut jobs = Vec::new();
    for file in index.files.iter().filter(|file| !should_skip_file(file)).cloned() {
        validate_pack_path(&file.path)?;
        safe_join(instance_root, &file.path)?;
        jobs.push(file);
    }
    let total = jobs.len() as u32;
    report(0, total, "");
    let mut stream = futures::stream::iter(jobs.into_iter().map(|file| async move {
        cancel.checkpoint().await?;
        // Every mirror in order: the first URL is usually the CDN, the rest
        // author-provided fallbacks. A dead or corrupt mirror must not fail
        // a file another mirror can serve — but a cancel stops everything.
        for url in &file.downloads {
            match download_bytes_with_retry(client, url, cancel).await {
                Err(crate::download::DownloadError::Cancelled) => {
                    return Err::<_, ModpackError>(
                        crate::download::DownloadError::Cancelled.into(),
                    );
                }
                Err(_) => continue,
                Ok(data) => {
                    if let Some(hashes) = &file.hashes {
                        if verify_hashes(&data, hashes).is_err() {
                            continue;
                        }
                    }
                    return Ok((file, data));
                }
            }
        }
        Err(ModpackError::Other(format!(
            "No download for {}; previous pack retained",
            file.path
        )))
    })).buffer_unordered(DOWNLOAD_CONCURRENCY);
    let mut downloaded = Vec::new();
    while let Some(result) = stream.next().await {
        let (file, data) = result?;
        cancel.checkpoint().await?;
        transaction.stage(&file.path, &data)?;
        let path = file.path.clone();
        downloaded.push(file);
        report(downloaded.len() as u32, total, &path);
    }
    let sha1_hashes: Vec<String> = downloaded.iter().filter_map(|file| file.hashes.as_ref()?.get("sha1").cloned()).collect();
    let meta_by_hash = modrinth.project_meta_by_sha1(&sha1_hashes).await;
    let mut icons = HashMap::new();
    let mut content_names = HashMap::new();
    let mut project_uids = HashMap::new();
    for file in &downloaded {
        let Some(meta) = file.hashes.as_ref().and_then(|hashes| hashes.get("sha1")).and_then(|hash| meta_by_hash.get(hash)) else { continue };
        let Some(filename) = std::path::Path::new(&file.path).file_name().and_then(|name| name.to_str()) else { continue };
        content_names.insert(filename.to_string(), meta.name.clone());
        project_uids.insert(filename.to_string(), format!("modrinth:{}", meta.project_id));
        if let Some(icon) = &meta.icon { icons.insert(filename.to_string(), icon.clone()); }
    }
    let overrides_applied = transaction.overrides(bytes, &["overrides", "client-overrides"], cancel).await?;
    let manifest_path = instance_root.join(".modrinth-pack-manifest.json");
    let old_paths: Vec<String> = match std::fs::read(&manifest_path) {
        Ok(data) => serde_json::from_slice(&data)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let paths: Vec<&str> = downloaded.iter().map(|file| file.path.as_str()).collect();
    for old in old_paths {
        if !paths.contains(&old.as_str()) {
            validate_pack_path(&old)?;
            // Only tracked content is reconciled, never saves or configuration.
            if ["mods/", "resourcepacks/", "shaderpacks/"].iter().any(|prefix| old.starts_with(prefix)) {
                let path = safe_join(instance_root, &old)?;
                if path.exists() { transaction.remove(&path)?; }
                let disabled = safe_join(instance_root, &format!("{old}.disabled"))?;
                if disabled.exists() { transaction.remove(&disabled)?; }
            }
        }
    }
    transaction.stage(".modrinth-pack-manifest.json", &serde_json::to_vec_pretty(&paths)?)?;
    transaction.commit(cancel).await?;
    let label = if index.name.is_empty() { "Modrinth modpack".to_string() } else { format!("{} {}", index.name, index.version_id) };
    Ok(ModpackImportResult {
        message: format!("Imported {label}: {total} files downloaded, {overrides_applied} override files applied."),
        has_skipped: false,
        icons,
        content_names,
        project_uids,
        missing_mods: Vec::new(),
        version_label: Some(index.version_id.clone()).filter(|v| !v.is_empty()),
    })
}

fn should_skip_file(file: &ModrinthPackFile) -> bool {
    if let Some(env) = &file.env {
        if env.client == "unsupported" {
            return true;
        }
    }
    false
}

fn read_index_from_mrpack(bytes: &[u8]) -> Result<ModrinthPackIndex, ModpackError> {
    read_mrpack_index(bytes)
}

pub fn read_mrpack_index(bytes: &[u8]) -> Result<ModrinthPackIndex, ModpackError> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;
    let mut index_file = archive.by_name("modrinth.index.json")?;
    let mut json = String::new();
    index_file.read_to_string(&mut json)?;
    Ok(serde_json::from_str(&json)?)
}


pub fn is_mrpack_bytes(bytes: &[u8]) -> bool {
    let cursor = Cursor::new(bytes);
    if let Ok(mut archive) = zip::ZipArchive::new(cursor) {
        return archive.by_name("modrinth.index.json").is_ok();
    }
    false
}

#[cfg(test)]
mod mrpack_index_tests {
    use super::{is_mrpack_bytes, read_mrpack_index, should_skip_file};
    use std::io::Write;

    /// Builds a `.mrpack` in memory — same zip-fixture approach the
    /// CurseForge importer's tests use, extended to write entry contents.
    fn mrpack_with(entries: &[(&str, &str)]) -> Vec<u8> {
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

    const INDEX_JSON: &str = r#"{
      "formatVersion": 1,
      "game": "minecraft",
      "versionId": "2.1.0",
      "name": "Ascendra",
      "files": [
        {
          "path": "mods/sodium.jar",
          "hashes": { "sha1": "aabbcc", "sha512": "ignored" },
          "downloads": ["https://cdn.modrinth.com/data/AANobbMI/versions/x/sodium.jar"],
          "fileSize": 123
        },
        {
          "path": "mods/server-only.jar",
          "downloads": ["https://example.com/s.jar"],
          "env": { "client": "unsupported", "server": "required" }
        }
      ]
    }"#;

    #[test]
    fn index_json_round_trips_out_of_an_mrpack() {
        let bytes = mrpack_with(&[
            ("modrinth.index.json", INDEX_JSON),
            ("overrides/config/foo.toml", "a = 1"),
        ]);

        let index = read_mrpack_index(&bytes).unwrap();
        assert_eq!(index.name, "Ascendra");
        assert_eq!(index.version_id, "2.1.0", "`versionId` is what becomes ModpackImportResult::version_label");
        assert_eq!(index.files.len(), 2);
        assert_eq!(index.files[0].path, "mods/sodium.jar");
        assert_eq!(index.files[0].hashes.as_ref().unwrap()["sha1"], "aabbcc");
        assert!(index.files[1].hashes.is_none(), "a file with no hashes block is still valid");
        assert_eq!(index.files[1].env.as_ref().unwrap().client, "unsupported");
    }

    #[test]
    fn index_without_optional_fields_still_parses() {
        let bytes = mrpack_with(&[(
            "modrinth.index.json",
            r#"{ "files": [ { "path": "mods/a.jar", "downloads": [] } ] }"#,
        )]);
        let index = read_mrpack_index(&bytes).unwrap();
        assert_eq!(index.name, "");
        assert_eq!(index.version_id, "", "an absent versionId leaves version_label empty, not an error");
        assert!(index.files[0].env.is_none());
    }

    #[test]
    fn client_unsupported_files_are_skipped() {
        let index = read_mrpack_index(&mrpack_with(&[("modrinth.index.json", INDEX_JSON)])).unwrap();
        assert!(!should_skip_file(&index.files[0]), "a file with no env block installs");
        assert!(should_skip_file(&index.files[1]), "client-unsupported files are server-side only");
    }

    #[test]
    fn mrpack_detection_requires_the_index_entry() {
        assert!(is_mrpack_bytes(&mrpack_with(&[("modrinth.index.json", "{}")])));
        assert!(!is_mrpack_bytes(&mrpack_with(&[("manifest.json", "{}")])), "a CurseForge pack is not an mrpack");
        assert!(!is_mrpack_bytes(b"not a zip at all"));
    }

    #[test]
    fn a_pack_without_an_index_is_an_error_not_a_panic() {
        let bytes = mrpack_with(&[("manifest.json", "{}")]);
        assert!(read_mrpack_index(&bytes).is_err());
    }

    #[test]
    fn malformed_index_json_is_an_error() {
        let bytes = mrpack_with(&[("modrinth.index.json", "{ not json")]);
        assert!(read_mrpack_index(&bytes).is_err());
    }
}

