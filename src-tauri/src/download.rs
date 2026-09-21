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

/// Shared cancellation and pause control checked at download boundaries.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<DownloadControl>);

#[derive(Debug, Default)]
struct DownloadControl {
    cancelled: AtomicBool,
    paused: AtomicBool,
    changed: tokio::sync::Notify,
}

impl CancelToken {
    pub fn new() -> Self { Self::default() }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }

    pub fn pause(&self) {
        self.0.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.0.paused.store(false, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub async fn checkpoint(&self) -> Result<(), DownloadError> {
        loop {
            // Register before checking flags so resume/cancel cannot be lost.
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() { return Err(DownloadError::Cancelled); }
            if !self.0.paused.load(Ordering::SeqCst) { return Ok(()); }
            notified.await;
        }
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
    #[error("download hash mismatch ({0})")]
    HashMismatch(String),
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
        // Only bounds the initial connect/TLS handshake, not the transfer
        // itself — this client also serves multi-hundred-MB modpack/mod
        // downloads that legitimately take minutes, so no full `.timeout()`
        // here. It only stops a server that never answers the connection at
        // all from hanging the caller forever.
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?)
}


pub async fn download_bytes(
    client: &Client,
    url: &str,
    cancel: &CancelToken,
) -> Result<Vec<u8>, DownloadError> {
    cancel.checkpoint().await?;
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(DownloadError::Status(response.status().as_u16()));
    }
    let mut buf = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        cancel.checkpoint().await?;
        let Some(chunk) = stream.next().await else { break };
        cancel.checkpoint().await?;
        buf.extend_from_slice(&chunk?);
    }
    Ok(buf)
}

/// Like `download_bytes`, but retries once after a short pause on transient
/// failures — CurseForge's CDN (edge.forgecdn.net via CloudFront)
/// intermittently returns a 401/403 for a freshly-resolved, genuinely valid
/// URL, then serves the same URL fine moments later; and any host can drop
/// a connection or 5xx mid-pack. A retry beats surfacing an error that tells
/// the user to do the exact same retry themselves. Non-transient statuses
/// (e.g. 404) and cancellations never retry.
pub async fn download_bytes_with_retry(
    client: &Client,
    url: &str,
    cancel: &CancelToken,
) -> Result<Vec<u8>, DownloadError> {
    match download_bytes(client, url, cancel).await {
        Err(DownloadError::Status(401 | 403 | 429 | 500 | 502 | 503 | 504)) if !cancel.is_cancelled() => {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            download_bytes(client, url, cancel).await
        }
        Err(DownloadError::Network(_)) if !cancel.is_cancelled() => {
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
    cancel.checkpoint().await?;
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(DownloadError::Status(response.status().as_u16()));
    }
    let mut buf = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        cancel.checkpoint().await?;
        let Some(chunk) = stream.next().await else { break };
        cancel.checkpoint().await?;
        let chunk = chunk?;
        if buf.len() + chunk.len() > max_bytes {
            return Err(DownloadError::TooLarge(max_bytes));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

pub fn verify_hashes(bytes: &[u8], hashes: &std::collections::HashMap<String, String>) -> Result<(), DownloadError> {
    use sha1::Digest;
    for (algorithm, expected) in hashes {
        let actual = match algorithm.as_str() {
            "sha1" => hex::encode(sha1::Sha1::digest(bytes)),
            "sha256" => hex::encode(sha2::Sha256::digest(bytes)),
            "sha512" => hex::encode(sha2::Sha512::digest(bytes)),
            _ => continue,
        };
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(DownloadError::HashMismatch(algorithm.clone()));
        }
    }
    Ok(())
}

pub async fn download_to_file_verified(
    client: &Client, url: &str, dest: &Path, cancel: &CancelToken,
    hashes: &std::collections::HashMap<String, String>,
) -> Result<(), DownloadError> {
    let bytes = download_bytes(client, url, cancel).await?;
    verify_hashes(&bytes, hashes)?;
    cancel.checkpoint().await?;
    atomic_write(dest, &bytes)
}

/// Prepare a complete sibling file before replacing the destination.
pub fn atomic_write(dest: &Path, bytes: &[u8]) -> Result<(), DownloadError> {
    use std::io::Write;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let parent = dest.parent().ok_or_else(|| DownloadError::UnsafePath(dest.display().to_string()))?;
    std::fs::create_dir_all(parent)?;
    let (temp, mut file) = loop {
        let temp = parent.join(format!(".waybound-download-{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    };
    let result = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, dest)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temp); }
    result.map_err(DownloadError::Io)
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
