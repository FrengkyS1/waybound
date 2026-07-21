use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use futures::stream::StreamExt;
use serde::Deserialize;

use super::{ModpackError, ModpackImportResult};
use crate::download::{download_bytes, http_client, safe_join, CancelToken, DOWNLOAD_CONCURRENCY};

#[derive(Debug, Deserialize)]
pub struct ModrinthPackIndex {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "versionId")]
    pub version_id: String,
    pub files: Vec<ModrinthPackFile>,
}

#[derive(Debug, Deserialize)]
pub struct ModrinthPackFile {
    pub path: String,
    pub downloads: Vec<String>,
    #[serde(default)]
    pub env: Option<ModrinthPackEnv>,
}

#[derive(Debug, Deserialize)]
pub struct ModrinthPackEnv {
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub server: Option<String>,
}

pub async fn import_modrinth_mrpack_bytes(
    bytes: &[u8],
    instance_root: &Path,
    cancel: &CancelToken,
    report: &impl Fn(u32, u32, &str),
) -> Result<ModpackImportResult, ModpackError> {
    let index = read_index_from_mrpack(bytes)?;
    let client = http_client()?;
    let client = &client;

    // Fully-owned job per file (url + resolved dest) so downloads can run
    // concurrently instead of one-at-a-time — a pack with hundreds of files
    // downloading sequentially could take minutes; buffer_unordered caps
    // concurrency the same way the launch pipeline's asset/library downloads
    // already do.
    let jobs: Vec<(String, PathBuf)> = index
        .files
        .iter()
        .filter(|f| !should_skip_file(f))
        .filter_map(|file| {
            let url = file.downloads.first()?.clone();
            Some((url, file.path.clone()))
        })
        .map(|(url, path)| Ok::<_, ModpackError>((url, safe_join(instance_root, &path)?)))
        .collect::<Result<Vec<_>, _>>()?;

    let total = jobs.len() as u32;
    let mut files_installed = 0u32;
    report(0, total, "");

    let mut stream = futures::stream::iter(jobs.into_iter().map(|(url, dest)| async move {
        if cancel.is_cancelled() {
            return Err(ModpackError::from(crate::download::DownloadError::Cancelled));
        }
        let data = download_bytes(client, &url, cancel).await?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, data)?;
        Ok::<PathBuf, ModpackError>(dest)
    }))
    .buffer_unordered(DOWNLOAD_CONCURRENCY);

    while let Some(result) = stream.next().await {
        let dest = result?;
        files_installed += 1;
        let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("");
        report(files_installed, total, name);
    }

    // Runs on a blocking-pool thread (large packs' overrides can be many MB
    // of resource packs/configs) so it doesn't stall the async runtime, and
    // checks `cancel` periodically like the download loop above it — this
    // sync zip-extraction loop previously had no cancellation awareness at
    // all despite CancelToken being passed in.
    let owned_bytes = bytes.to_vec();
    let owned_root = instance_root.to_path_buf();
    let owned_cancel = cancel.clone();
    let overrides_applied =
        tokio::task::spawn_blocking(move || extract_overrides(&owned_bytes, &owned_root, &owned_cancel))
            .await
            .map_err(|e| ModpackError::Other(format!("override extraction task panicked: {e}")))??;

    let label = if index.name.is_empty() {
        "Modrinth modpack".to_string()
    } else {
        format!("{} {}", index.name, index.version_id)
    };

    Ok(ModpackImportResult {
        message: format!(
            "Imported {label}: {files_installed} files downloaded, {overrides_applied} override files applied."
        ),
        has_skipped: false,
        icons: std::collections::HashMap::new(),
        missing_mods: Vec::new(),
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

fn extract_overrides(
    bytes: &[u8],
    instance_root: &Path,
    cancel: &CancelToken,
) -> Result<u32, ModpackError> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;
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

        let relative = if let Some(path) = name.strip_prefix("overrides/") {
            path
        } else if let Some(path) = name.strip_prefix("overrides-client/") {
            path
        } else {
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

pub fn is_mrpack_bytes(bytes: &[u8]) -> bool {
    let cursor = Cursor::new(bytes);
    if let Ok(mut archive) = zip::ZipArchive::new(cursor) {
        return archive.by_name("modrinth.index.json").is_ok();
    }
    false
}

