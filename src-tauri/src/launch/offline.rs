//! Raw, cache-first launch metadata. Immutable installed profiles never need
//! a network round trip; unavailable metadata fails with a reconnect action.

use std::path::{Path, PathBuf};
use serde::de::DeserializeOwned;
use super::{LaunchError, manifest::{VersionJson, VersionManifest, VERSION_MANIFEST_URL}};

pub fn cache_root(game_root: &Path) -> PathBuf {
    game_root.join("meta-cache")
}

/// A unique sibling temp file avoids concurrent launches sharing a partial file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn put_bytes(game_root: &Path, key: &str, bytes: &[u8]) -> Result<(), LaunchError> {
    write_atomic(&cache_root(game_root).join(key), bytes)?;
    Ok(())
}

pub fn get_bytes(game_root: &Path, key: &str) -> Option<Vec<u8>> {
    std::fs::read(cache_root(game_root).join(key)).ok()
}

pub fn read_json<T: DeserializeOwned>(game_root: &Path, key: &str) -> Option<T> {
    serde_json::from_slice(&get_bytes(game_root, key)?).ok()
}

/// Cache-first also covers outages returning malformed JSON or broken bodies:
/// a valid installed copy is used before making any request at all.
pub async fn fetch_json_cached<T: DeserializeOwned>(
    client: &reqwest::Client, game_root: &Path, key: &str, url: &str,
) -> Result<T, LaunchError> {
    if let Some(cached) = read_json(game_root, key) {
        return Ok(cached);
    }
    fetch_json_fresh(client, game_root, key, url).await
}

pub async fn fetch_json_fresh<T: DeserializeOwned>(
    client: &reqwest::Client, game_root: &Path, key: &str, url: &str,
) -> Result<T, LaunchError> {
    let fetch = async {
        let body = client.get(url)
            .timeout(std::time::Duration::from_secs(8))
            .send().await?.error_for_status()?.bytes().await?;
        let parsed = serde_json::from_slice(&body)
            .map_err(|e| LaunchError::Parse(format!("{key}: {e}")))?;
        put_bytes(game_root, key, &body)?;
        Ok::<T, LaunchError>(parsed)
    }.await;
    match fetch {
        Ok(value) => Ok(value),
        Err(error) => read_json(game_root, key).ok_or_else(|| LaunchError::Parse(format!(
            "Launch metadata '{key}' is missing or invalid and could not be fetched ({error}). Connect to the internet and prepare this instance once."
        ))),
    }
}

/// Adopt metadata left by older Waybound builds or another installed layout.
/// Preserve unknown JSON fields; never serialize the reduced launch structs.
pub fn adopt_json<T: DeserializeOwned>(game_root: &Path, key: &str, path: &Path) -> Result<Option<T>, LaunchError> {
    let Ok(bytes) = std::fs::read(path) else { return Ok(None) };
    let Ok(value) = serde_json::from_slice(&bytes) else { return Ok(None) };
    put_bytes(game_root, key, &bytes)?;
    Ok(Some(value))
}

pub async fn vanilla_version(client: &reqwest::Client, root: &Path, id: &str) -> Result<VersionJson, LaunchError> {
    let key = format!("versions/{id}.json");
    if let Some(version) = read_json::<VersionJson>(root, &key).filter(|v| v.id == id && v.inherits_from.is_none()) {
        return Ok(version);
    }
    let installed = root.join("versions").join(id).join(format!("{id}.json"));
    if let Some(version) = adopt_json::<VersionJson>(root, &key, &installed)? {
        if version.id == id && version.inherits_from.is_none() {
            return Ok(version);
        }
    }
    let mut manifest: VersionManifest = fetch_json_cached(client, root, "manifest.json", VERSION_MANIFEST_URL).await?;
    if manifest.find(id).is_none() {
        manifest = fetch_json_fresh(client, root, "manifest.json", VERSION_MANIFEST_URL).await?;
    }
    let entry = manifest.find(id).ok_or_else(|| LaunchError::VersionNotFound(id.to_owned()))?;
    let version: VersionJson = fetch_json_fresh(client, root, &key, &entry.url).await?;
    if version.id != id || version.inherits_from.is_some() {
        return Err(LaunchError::Parse(format!("Expected vanilla metadata for {id}")));
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restart_cache_keeps_unknown_fields_and_needs_no_network() {
        let dir = tempfile::tempdir().unwrap();
        let raw = br#"{"id":"1.20.1","unknown":{"future":true},"libraries":[]}"#;
        put_bytes(dir.path(), "versions/1.20.1.json", raw).unwrap();
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
            .timeout(std::time::Duration::from_secs(1))
            .build().unwrap();
        let version = vanilla_version(&client, dir.path(), "1.20.1").await.unwrap();
        assert_eq!(version.id, "1.20.1");
        assert_eq!(get_bytes(dir.path(), "versions/1.20.1.json").unwrap(), raw);
        assert!(!cache_root(dir.path()).join("manifest.json").exists());
    }

    #[tokio::test]
    async fn adopts_installed_version_without_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions/old/old.json");
        write_atomic(&path, br#"{"id":"old","unknown":42}"#).unwrap();
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
            .timeout(std::time::Duration::from_secs(1))
            .build().unwrap();
        let version = vanilla_version(&client, dir.path(), "old").await.unwrap();
        assert_eq!(version.id, "old");
        let raw: serde_json::Value = read_json(dir.path(), "versions/old.json").unwrap();
        assert_eq!(raw["unknown"], 42);
    }

    #[tokio::test]
    async fn bad_refresh_cannot_evict_valid_cache() {
        use std::io::{Read, Write};
        for response in [
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nbad",
            "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{",
        ] {
            let dir = tempfile::tempdir().unwrap();
            put_bytes(dir.path(), "profile.json", br#"{"id":"good"}"#).unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = [0; 2048];
                let _ = socket.read(&mut request);
                socket.write_all(response.as_bytes()).unwrap();
            });
            let value: VersionJson = fetch_json_fresh(&reqwest::Client::new(), dir.path(), "profile.json", &url).await.unwrap();
            assert_eq!(value.id, "good");
            server.join().unwrap();
        }
    }
}
