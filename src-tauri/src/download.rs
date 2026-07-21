use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use reqwest::Client;
use thiserror::Error;

const USER_AGENT: &str = "Waybound/0.1.0 (personal mod manager; contact: local)";

/// Shared concurrency cap for any per-file download loop (asset objects,
/// library jars, modpack files) — one place to tune instead of a
/// re-guessed magic number per call site.
pub const DOWNLOAD_CONCURRENCY: usize = 8;

/// A cheap, cloneable flag checked between download chunks so an in-flight
/// install can be interrupted from a Tauri command (`cancel_install`)
/// without needing to abort the whole async task from outside.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("download failed with status {0}")]
    Status(u16),
    #[error("refusing to write outside the instance folder: {0}")]
    UnsafePath(String),
    #[error("cancelled")]
    Cancelled,
    #[error("response exceeded the {0}-byte limit")]
    TooLarge(usize),
}

/// Joins `relative` onto `base`, rejecting anything that could escape
/// `base` (`..`, an absolute path, a Windows drive prefix). `relative`
/// comes from third-party data (modpack manifests, zip entry names, mod
/// filenames from CurseForge/Modrinth) so it must never be trusted as-is.
pub fn safe_join(base: &Path, relative: &str) -> Result<PathBuf, DownloadError> {
    let mut result = base.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            _ => return Err(DownloadError::UnsafePath(relative.to_string())),
        }
    }
    Ok(result)
}

pub fn http_client() -> Result<Client, DownloadError> {
    Ok(Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?)
}

pub async fn download_to_file(
    client: &Client,
    url: &str,
    dest: &Path,
    cancel: &CancelToken,
) -> Result<(), DownloadError> {
    let bytes = download_bytes(client, url, cancel).await?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, bytes)?;
    Ok(())
}

pub async fn download_bytes(
    client: &Client,
    url: &str,
    cancel: &CancelToken,
) -> Result<Vec<u8>, DownloadError> {
    if cancel.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(DownloadError::Status(response.status().as_u16()));
    }
    let mut buf = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        buf.extend_from_slice(&chunk?);
    }
    Ok(buf)
}

/// Like `download_bytes`, but retries once after a short pause on a 401/403 —
/// CurseForge's CDN (edge.forgecdn.net via CloudFront) intermittently returns
/// one of these for a freshly-resolved, genuinely valid URL, then serves the
/// same URL fine moments later. A 401/403 there is a transient edge/cache
/// blip, not proof the URL is bad, so retrying beats surfacing an error that
/// tells the user to do the exact same retry themselves.
pub async fn download_bytes_with_retry(
    client: &Client,
    url: &str,
    cancel: &CancelToken,
) -> Result<Vec<u8>, DownloadError> {
    match download_bytes(client, url, cancel).await {
        Err(DownloadError::Status(401 | 403)) if !cancel.is_cancelled() => {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            download_bytes(client, url, cancel).await
        }
        result => result,
    }
}

/// Like `download_bytes`, but aborts once the response exceeds `max_bytes`
/// instead of buffering it all — for downloads whose URL wasn't resolved by
/// our own trusted code (e.g. an icon URL taken verbatim off the Tauri IPC
/// boundary), so a large or slow response can't be used to make this process
/// buffer unbounded data in memory.
pub async fn download_bytes_capped(
    client: &Client,
    url: &str,
    cancel: &CancelToken,
    max_bytes: usize,
) -> Result<Vec<u8>, DownloadError> {
    if cancel.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(DownloadError::Status(response.status().as_u16()));
    }
    let mut buf = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let chunk = chunk?;
        if buf.len() + chunk.len() > max_bytes {
            return Err(DownloadError::TooLarge(max_bytes));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::{download_bytes, http_client, safe_join, CancelToken, DownloadError};
    use std::path::Path;

    #[test]
    fn cancel_token_clone_shares_state() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(!token.is_cancelled());
        clone.cancel();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn download_bytes_returns_cancelled_when_pre_cancelled() {
        let client = http_client().unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        // Cancelled before the request is even sent — must not touch the network.
        let result = download_bytes(&client, "https://example.invalid/never-fetched", &cancel).await;
        assert!(matches!(result, Err(DownloadError::Cancelled)));
    }

    #[test]
    fn safe_join_allows_normal_relative_paths() {
        let base = Path::new("/instances/abc");
        let joined = safe_join(base, "mods/cool-mod.jar").unwrap();
        assert_eq!(joined, base.join("mods").join("cool-mod.jar"));
    }

    #[test]
    fn safe_join_rejects_parent_dir_traversal() {
        let base = Path::new("/instances/abc");
        assert!(safe_join(base, "../../../../Startup/evil.jar").is_err());
        assert!(safe_join(base, "mods/../../evil.jar").is_err());
    }

    #[test]
    fn safe_join_rejects_absolute_paths() {
        let base = Path::new("/instances/abc");
        assert!(safe_join(base, "/etc/passwd").is_err());
    }
}
