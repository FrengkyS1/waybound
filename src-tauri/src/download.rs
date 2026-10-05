use std::future::Future;
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

/// Allows large pack archives/resource packs while keeping every buffered
/// download finite. Manifest-declared sizes should use a smaller explicit cap.
pub const MAX_DOWNLOAD_BYTES: usize = 2 * 1024 * 1024 * 1024;

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
        self.0.changed.notify_waiters();
    }

    pub fn resume(&self) {
        self.0.paused.store(false, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub fn same_control(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Wakes even when the server is connected but sends no headers/chunks.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() { return; }
            notified.await;
        }
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

    /// Keep the same request/timer alive while paused, without polling it.
    pub(crate) async fn wait<F: Future>(&self, future: F) -> Result<F::Output, DownloadError> {
        tokio::pin!(future);
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.checkpoint().await?;
            tokio::select! {
                biased;
                _ = changed => {}
                output = &mut future => return Ok(output),
            }
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
    #[error("file changed outside Waybound; reload before saving")]
    Conflict,
}

/// Joins `relative` onto `base`, rejecting anything that could escape
/// `base` (`..`, an absolute path, a Windows drive prefix). `relative`
/// comes from third-party data (modpack manifests, zip entry names, mod
/// filenames from CurseForge/Modrinth) so it must never be trusted as-is.
pub fn safe_join(base: &Path, relative: &str) -> Result<PathBuf, DownloadError> {
    let mut result = base.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => {
                #[cfg(windows)]
                {
                    let name = part.to_str().ok_or_else(|| DownloadError::UnsafePath(relative.to_string()))?;
                    let stem = name.split('.').next().unwrap_or("");
                    let device = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].iter()
                        .any(|device| stem.eq_ignore_ascii_case(device))
                        || (stem.get(..3).is_some_and(|prefix|
                            prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT"))
                            && matches!(stem.get(3..), Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")));
                    if name.contains(':') || name.ends_with('.') || name.ends_with(' ') || device {
                        return Err(DownloadError::UnsafePath(relative.to_string()));
                    }
                }
                result.push(part);
            }
            Component::CurDir => {}
            _ => return Err(DownloadError::UnsafePath(relative.to_string())),
        }
    }
    Ok(result)
}

/// Rejects links in every existing component, including the trusted root's
/// ancestors. Windows junctions and other reparse points are links too.
/// Missing leaf/parent components are allowed for writes.
pub fn ensure_contained_path(base: &Path, path: &Path) -> Result<(), DownloadError> {
    fn absolute(path: &Path) -> Result<PathBuf, DownloadError> {
        let path = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
        if path.components().any(|part| matches!(part, Component::ParentDir)) {
            return Err(DownloadError::UnsafePath(path.display().to_string()));
        }
        Ok(path.components().collect())
    }
    let base = absolute(base)?;
    let path = absolute(path)?;
    if !path.starts_with(&base) {
        return Err(DownloadError::UnsafePath(path.display().to_string()));
    }
    reject_link_components(&path)
}

pub fn contained_join(base: &Path, relative: &str) -> Result<PathBuf, DownloadError> {
    let path = safe_join(base, relative)?;
    ensure_contained_path(base, &path)?;
    Ok(path)
}

fn reject_link_components(path: &Path) -> Result<(), DownloadError> {
    for current in path.ancestors().filter(|part| !part.as_os_str().is_empty()) {
        match std::fs::symlink_metadata(current) {
            Ok(meta) => {
                #[cfg(windows)]
                let reparse = {
                    use std::os::windows::fs::MetadataExt;
                    meta.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
                };
                #[cfg(not(windows))]
                let reparse = false;
                if meta.file_type().is_symlink() || reparse {
                    return Err(DownloadError::UnsafePath(current.display().to_string()));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
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
    download_bytes_capped(client, url, cancel, MAX_DOWNLOAD_BYTES).await
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
    download_bytes_capped_with_retry(client, url, cancel, MAX_DOWNLOAD_BYTES).await
}

pub async fn download_bytes_capped_with_retry(
    client: &Client,
    url: &str,
    cancel: &CancelToken,
    max_bytes: usize,
) -> Result<Vec<u8>, DownloadError> {
    let result = download_bytes_capped(client, url, cancel, max_bytes).await;
    if matches!(&result, Err(DownloadError::Status(401 | 403 | 429 | 500 | 502 | 503 | 504) | DownloadError::Network(_))) {
        cancel.wait(tokio::time::sleep(std::time::Duration::from_secs(2))).await?;
        download_bytes_capped(client, url, cancel, max_bytes).await
    } else {
        result
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
    let max_bytes = max_bytes.min(MAX_DOWNLOAD_BYTES);
    cancel.checkpoint().await?;
    let response = cancel.wait(client.get(url).send()).await??;
    cancel.checkpoint().await?;
    if !response.status().is_success() {
        return Err(DownloadError::Status(response.status().as_u16()));
    }
    if response.content_length().is_some_and(|length| length > max_bytes as u64) {
        return Err(DownloadError::TooLarge(max_bytes));
    }
    let mut buf = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        cancel.checkpoint().await?;
        let chunk = cancel.wait(stream.next()).await?;
        let Some(chunk) = chunk else { break };
        cancel.checkpoint().await?;
        append_chunk(&mut buf, &chunk?, max_bytes)?;
    }
    Ok(buf)
}

fn append_chunk(buf: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> Result<(), DownloadError> {
    if chunk.len() > max_bytes.saturating_sub(buf.len()) {
        return Err(DownloadError::TooLarge(max_bytes));
    }
    let needed = buf.len() + chunk.len();
    if needed > buf.capacity() {
        let capacity = needed.max(buf.capacity().saturating_mul(2)).min(max_bytes);
        buf.reserve_exact(capacity - buf.len());
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

/// Streaming SHA-1 avoids buffering large manually downloaded packs/jars.
pub fn file_sha1(path: &Path) -> Result<String, DownloadError> {
    use sha1::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha1::Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 { break; }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
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

/// Prepare and sync a complete sibling before atomically replacing the target.
/// Failed replacement never removes the original (including on Windows).
pub fn atomic_write(dest: &Path, bytes: &[u8]) -> Result<(), DownloadError> {
    let staged = stage_write(dest, bytes)?;
    reject_link_components(dest)?;
    staged.persist(dest).map_err(|err| DownloadError::Io(err.error))?;
    Ok(())
}

/// Compare after staging, immediately before publication. This detects an
/// external edit without buffering another copy of the existing file.
pub fn atomic_write_checked(dest: &Path, bytes: &[u8], expected_bytes: &[u8]) -> Result<(), DownloadError> {
    let staged = stage_write(dest, bytes)?;
    reject_link_components(dest)?;
    if !file_matches_bytes(dest, expected_bytes)? { return Err(DownloadError::Conflict); }
    staged.persist(dest).map_err(|err| DownloadError::Io(err.error))?;
    Ok(())
}

fn stage_write(dest: &Path, bytes: &[u8]) -> Result<tempfile::NamedTempFile, DownloadError> {
    use std::io::Write;
    reject_link_components(dest)?;
    let parent = dest.parent().filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    reject_link_components(dest)?;
    let mut staged = tempfile::Builder::new().prefix(".waybound-download-").tempfile_in(parent)?;
    staged.write_all(bytes)?;
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(dest) {
        staged.as_file().set_permissions(meta.permissions())?;
    }
    staged.as_file().sync_all()?;
    Ok(staged)
}

fn file_matches_bytes(path: &Path, expected: &[u8]) -> Result<bool, DownloadError> {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    let mut offset = 0;
    let mut buffer = [0u8; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { return Ok(offset == expected.len()); }
        if count > expected.len().saturating_sub(offset) || buffer[..count] != expected[offset..offset + count] {
            return Ok(false);
        }
        offset += count;
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Duration;

    fn server(response: &'static [u8]) -> (String, tokio::sync::oneshot::Receiver<()>, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).unwrap();
                if read == 0 { return; }
                request.extend_from_slice(&buffer[..read]);
            }
            socket.write_all(response).unwrap();
            let _ = ready_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(3));
        });
        (url, ready_rx, release_tx, thread)
    }

    async fn assert_stalled_cancel(response: &'static [u8]) {
        let (url, ready, release, thread) = server(response);
        let client = Client::builder().no_proxy().build().unwrap();
        let cancel = CancelToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move { download_bytes(&client, &url, &task_cancel).await });
        tokio::time::timeout(Duration::from_secs(2), ready).await.unwrap().unwrap();
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), task).await;
        let _ = release.send(());
        thread.join().unwrap();
        assert!(matches!(result.unwrap().unwrap(), Err(DownloadError::Cancelled)));
    }

    #[tokio::test]
    async fn cancel_stops_connected_server_without_response_headers() {
        assert_stalled_cancel(b"").await;
    }

    #[tokio::test]
    async fn cancel_stops_stalled_response_body() {
        assert_stalled_cancel(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n").await;
    }

    #[tokio::test]
    async fn paused_download_sends_nothing_until_resume() {
        let (url, mut ready, release, thread) = server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let client = Client::builder().no_proxy().build().unwrap();
        let cancel = CancelToken::new();
        cancel.pause();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move { download_bytes(&client, &url, &task_cancel).await });
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut ready).await.is_err());
        cancel.resume();
        let result = tokio::time::timeout(Duration::from_secs(2), task).await;
        let _ = release.send(());
        thread.join().unwrap();
        assert_eq!(result.unwrap().unwrap().unwrap(), b"ok");
    }

    #[tokio::test]
    async fn cancelling_paused_download_does_not_wait_for_resume() {
        let client = Client::builder().no_proxy().build().unwrap();
        let cancel = CancelToken::new();
        cancel.pause();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            download_bytes(&client, "http://127.0.0.1:1/never-requested", &task_cancel).await
        });
        tokio::task::yield_now().await;
        cancel.cancel();
        assert!(matches!(tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap(), Err(DownloadError::Cancelled)));
    }

    #[tokio::test]
    async fn transient_failure_retries_exactly_once() {
        use std::sync::atomic::AtomicUsize;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let server_count = count.clone();
        let server_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            while !server_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        let mut request = Vec::new();
                        let mut buffer = [0u8; 1024];
                        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                            let read = socket.read(&mut buffer).unwrap();
                            if read == 0 { break; }
                            request.extend_from_slice(&buffer[..read]);
                        }
                        server_count.fetch_add(1, Ordering::SeqCst);
                        socket.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                    Err(err) => panic!("{err}"),
                }
            }
        });
        let client = Client::builder().no_proxy().build().unwrap();
        let result = tokio::time::timeout(Duration::from_secs(6),
            download_bytes_capped_with_retry(&client, &url, &CancelToken::new(), 4)).await;
        stop.store(true, Ordering::SeqCst);
        thread.join().unwrap();
        assert!(matches!(result.unwrap(), Err(DownloadError::Status(503))));
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancellation_interrupts_retry_backoff() {
        let (url, ready, release, thread) = server(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n");
        let client = Client::builder().no_proxy().build().unwrap();
        let cancel = CancelToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            download_bytes_capped_with_retry(&client, &url, &task_cancel, 4).await
        });
        tokio::time::timeout(Duration::from_secs(2), ready).await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_millis(500), task).await;
        let _ = release.send(());
        thread.join().unwrap();
        assert!(matches!(result.unwrap().unwrap(), Err(DownloadError::Cancelled)));
    }

    #[tokio::test]
    async fn caller_cannot_raise_shared_received_byte_bound() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2147483649\r\n\r\n";
        let (url, _, release, thread) = server(response);
        let client = Client::builder().no_proxy().build().unwrap();
        let result = download_bytes_capped(&client, &url, &CancelToken::new(), usize::MAX).await;
        let _ = release.send(());
        thread.join().unwrap();
        assert!(matches!(result, Err(DownloadError::TooLarge(MAX_DOWNLOAD_BYTES))));
    }

    #[tokio::test]
    async fn received_chunked_bytes_enforce_cap_without_content_length() {
        let (url, _, release, thread) = server(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n12345678\r\n0\r\n\r\n");
        let client = Client::builder().no_proxy().build().unwrap();
        let result = download_bytes_capped_with_retry(&client, &url, &CancelToken::new(), 4).await;
        let _ = release.send(());
        thread.join().unwrap();
        assert!(matches!(result, Err(DownloadError::TooLarge(4))));
    }

    #[test]
    fn oversized_chunk_never_changes_buffer_and_growth_stays_capped() {
        let mut buffer = Vec::new();
        append_chunk(&mut buffer, b"123456", 10).unwrap();
        let capacity = buffer.capacity();
        assert!(matches!(append_chunk(&mut buffer, b"abcde", 10), Err(DownloadError::TooLarge(10))));
        assert_eq!(buffer, b"123456");
        assert_eq!(buffer.capacity(), capacity);
        append_chunk(&mut buffer, b"789", 10).unwrap();
        assert!(buffer.capacity() <= 10);
    }

    #[test]
    fn atomic_replacement_and_conflict_preserve_latest_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("options.txt");
        atomic_write(&dest, b"old").unwrap();
        atomic_write_checked(&dest, b"new", b"old").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert!(matches!(atomic_write_checked(&dest, b"stale save", b"old"), Err(DownloadError::Conflict)));
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_atomic_publish_preserves_destination_and_cleans_stage() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("existing-directory");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("original"), b"keep").unwrap();
        assert!(atomic_write(&dest, b"replacement").is_err());
        assert_eq!(std::fs::read(dest.join("original")).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn locked_windows_target_survives_failed_replace() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("locked.jar");
        std::fs::write(&dest, b"original").unwrap();
        let locked = std::fs::OpenOptions::new().read(true).share_mode(0).open(&dest).unwrap();
        assert!(atomic_write(&dest, b"replacement").is_err());
        drop(locked);
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn containment_allows_missing_targets_but_not_parent_escape() {
        let dir = tempfile::tempdir().unwrap();
        assert!(contained_join(dir.path(), "new/config/options.txt").is_ok());
        assert!(ensure_contained_path(dir.path(), &dir.path().join("../outside")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn containment_rejects_symlink_components_and_symlink_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("config")).unwrap();
        assert!(contained_join(dir.path(), "config/new.txt").is_err());
        assert!(contained_join(&dir.path().join("config"), "new.txt").is_err());
        assert!(atomic_write(&dir.path().join("config/new.txt"), b"outside").is_err());
        assert!(!outside.path().join("new.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn containment_rejects_windows_junction_components_and_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let junction = dir.path().join("config");
        let status = std::process::Command::new("cmd").args(["/C", "mklink", "/J"])
            .arg(&junction).arg(outside.path()).status().unwrap();
        assert!(status.success());
        assert!(contained_join(dir.path(), "config/new.txt").is_err());
        assert!(contained_join(&junction, "new.txt").is_err());
        assert!(atomic_write(&junction.join("new.txt"), b"outside").is_err());
        assert!(!outside.path().join("new.txt").exists());
        std::fs::remove_dir(junction).unwrap();
    }
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

    #[cfg(windows)]
    #[test]
    fn safe_join_rejects_windows_aliases_and_alternate_streams() {
        let base = Path::new(r"C:\instances\fixture");
        for relative in ["mods/file.jar:stream", "mods/.. /outside.jar", "mods/file.jar.",
            "mods/CON.jar", "mods/LPT1.zip", "mods/COM¹.jar"] {
            assert!(safe_join(base, relative).is_err(), "{relative}");
        }
    }
}
