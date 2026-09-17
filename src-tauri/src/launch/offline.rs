//! Disk-backed fallbacks for launch-time metadata that normally comes from
//! remote services. A version, once installed, must stay launchable with no
//! network: the Mojang manifest, the version JSON, and the Fabric loader
//! profile are all cached here the first time they are fetched, and every
//! launch-time fetch falls back to its cached copy before failing.

use std::fs;
use std::path::{Path, PathBuf};

use super::manifest::{VersionJson, VersionManifest};

/// Root for cached launch metadata, alongside the shared game files.
pub fn cache_root(game_root: &Path) -> PathBuf {
    game_root.join("meta-cache")
}

fn sanitize(component: &str) -> String {
    component
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Store bytes for a logical key (e.g. `manifest.json`,
/// `versions/1.21.1.json`, `fabric/1.21.1/0.16.9.json`).
pub fn put_bytes(game_root: &Path, key: &str, bytes: &[u8]) {
    let path = cache_root(game_root).join(key);
    let _ = write_atomic(&path, bytes);
}

pub fn get_bytes(game_root: &Path, key: &str) -> Option<Vec<u8>> {
    std::fs::read(cache_root(game_root).join(key)).ok()
}

/// Fetch a JSON URL, caching the raw body; falls back to the cached copy on
/// any network/parse failure. `key` must be filesystem-safe.
pub async fn fetch_json_cached<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    game_root: &Path,
    key: &str,
    url: &str,
    report_parse: impl FnOnce(reqwest::Error) -> String,
) -> Result<T, super::LaunchError> {
    let resp = client.get(url).send().await;
    match resp {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                if let Some(bytes) = get_bytes(game_root, key) {
                    return serde_json::from_slice(&bytes).map_err(|e| {
                        super::LaunchError::Parse(format!("cached {key}: {e}"))
                    });
                }
                return Err(super::LaunchError::Download {
                    url: url.to_string(),
                    status: status.as_u16(),
                });
            }
            let body = resp.bytes().await?;
            // Cache only after a successful parse, so a transient bad payload
            // never evicts a good cached copy.
            let parsed: T = serde_json::from_slice(&body)
                .map_err(|e| super::LaunchError::Parse(format!("{}: {e}", key)))?;
            put_bytes(game_root, key, &body);
            Ok(parsed)
        }
        Err(err) => {
            if let Some(bytes) = get_bytes(game_root, key) {
                return serde_json::from_slice(&bytes).map_err(|e| {
                    super::LaunchError::Parse(format!("cached {key}: {e}"))
                });
            }
            Err(report_parse(err))
        }
    }
}

/// Read a previously cached version JSON from disk.
pub fn read_cached_version_json(game_root: &Path, version_id: &str) -> Option<VersionJson> {
    let bytes = get_bytes(game_root, &format!("versions/{version_id}.json"))?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist a resolved version JSON (called after a successful fetch).
pub fn store_version_json(game_root: &Path, version_id: &str, json: &VersionJson) {
    if let Ok(bytes) = serde_json::to_vec(json) {
        put_bytes(game_root, &format!("versions/{version_id}.json"), &bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_bytes_and_overwrites() {
        let dir = std::env::temp_dir().join(format!(
            "waybound-meta-cache-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        put_bytes(&dir, "versions/1.21.1.json", br#"{"id":"1.21.1"}"#);
        assert_eq!(
            get_bytes(&dir, "versions/1.21.1.json").as_deref(),
            Some(&b"{\"id\":\"1.21.1\"}"[..])
        );
        put_bytes(&dir, "versions/1.21.1.json", br#"{"id":"1.21.1-2"}"#);
        assert_eq!(
            get_bytes(&dir, "versions/1.21.1.json").as_deref(),
            Some(&b"{\"id\":\"1.21.1-2\"}"[..])
        );
        assert!(get_bytes(&dir, "versions/missing.json").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitizes_hostile_keys() {
        assert_eq!(sanitize("../..\\evil"), "______evil");
    }
}
