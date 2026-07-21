//! Downloading and verifying the files a version needs: client jar, libraries,
//! natives (extracted), and assets (including legacy/virtual layouts).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures::stream::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use sha1::{Digest, Sha1};

use super::manifest::{
    natives_classifier_key, rules_allow, Artifact, VersionJson, RESOURCES_BASE_URL,
};
use super::{LaunchError, ProgressUpdate};
use crate::download::DOWNLOAD_CONCURRENCY;

const MAX_ATTEMPTS: usize = 4;

/// Fetch a URL's body, retrying transient failures (network errors, 5xx, 429).
/// A single flaky asset request used to abort an entire launch.
async fn fetch_with_retry(client: &Client, url: &str) -> Result<Vec<u8>, LaunchError> {
    let mut last_err: Option<LaunchError> = None;
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(300 * attempt as u64)).await;
        }
        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.bytes().await {
                        Ok(bytes) => return Ok(bytes.to_vec()),
                        Err(e) => last_err = Some(LaunchError::Network(e)),
                    }
                } else if status.as_u16() == 429 || status.is_server_error() {
                    last_err = Some(LaunchError::Download {
                        url: url.to_string(),
                        status: status.as_u16(),
                    });
                } else {
                    // 4xx (other than 429) won't succeed on retry.
                    return Err(LaunchError::Download {
                        url: url.to_string(),
                        status: status.as_u16(),
                    });
                }
            }
            Err(e) => last_err = Some(LaunchError::Network(e)),
        }
    }
    Err(last_err.unwrap_or_else(|| LaunchError::Download {
        url: url.to_string(),
        status: 0,
    }))
}

/// Where all shared game files live (libraries, assets, version metadata),
/// independent of any single instance.
#[derive(Clone)]
pub struct GamePaths {
    pub root: PathBuf,
}

impl GamePaths {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn libraries(&self) -> PathBuf {
        self.root.join("libraries")
    }
    pub fn assets(&self) -> PathBuf {
        self.root.join("assets")
    }
    pub fn versions(&self) -> PathBuf {
        self.root.join("versions")
    }
    pub fn runtimes(&self) -> PathBuf {
        self.root.join("runtimes")
    }
    pub fn version_dir(&self, id: &str) -> PathBuf {
        self.versions().join(id)
    }
    pub fn client_jar(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.jar"))
    }
    pub fn natives_dir(&self, id: &str) -> PathBuf {
        self.version_dir(id).join("natives")
    }
}

/// Compute the SHA-1 of a file, if it exists.
pub(crate) fn file_sha1(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes);
    Some(hex::encode(hasher.finalize()))
}

/// Download `url` to `dest`, skipping the transfer entirely if the file
/// already exists and passes the cache-hit check. Verifies the hash after a
/// fresh download when one is provided.
///
/// `trust_cache_hit`: when true, an existing file is accepted without
/// re-hashing it — sound only for content-addressed destinations (the asset
/// object store, where `dest`'s own path is `objects/<hash prefix>/<hash>`,
/// so the filename itself guarantees correctness) where re-hashing every
/// object on every launch was pure synchronous CPU/disk work with no
/// `.await` point in it: across thousands of asset files that made every
/// relaunch look like a fresh download, and — because a task with no yield
/// point can't be preempted — made `cancel_launch`'s abort unable to take
/// effect until the whole re-hash pass finished on its own. Every other
/// destination (client jar, library/loader jars, natives, the asset index
/// itself, Java runtime files) is addressed by a version id or Maven
/// coordinate, not by content hash, so a stale or corrupted file sitting at
/// that path would otherwise go undetected forever — those call sites pass
/// `false` and re-verify on every cache hit, which is cheap at their scale
/// (dozens of files, not thousands).
pub(crate) async fn download_verified(
    client: &Client,
    url: &str,
    dest: &Path,
    expected_sha1: Option<&str>,
    trust_cache_hit: bool,
) -> Result<(), LaunchError> {
    if dest.exists() {
        if trust_cache_hit {
            return Ok(());
        }
        if let Some(want) = expected_sha1 {
            if file_sha1(dest).as_deref() == Some(want) {
                return Ok(());
            }
        } else {
            return Ok(());
        }
    }

    let bytes = fetch_with_retry(client, url).await?;

    if let Some(want) = expected_sha1 {
        let mut hasher = Sha1::new();
        hasher.update(&bytes);
        let got = hex::encode(hasher.finalize());
        if got != want {
            return Err(LaunchError::HashMismatch {
                url: url.to_string(),
            });
        }
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, &bytes)?;
    Ok(())
}

/// The classpath entries and natives an instance needs, resolved from a
/// (possibly merged) version JSON.
pub struct ResolvedLibraries {
    /// Absolute paths to jars that belong on the classpath.
    pub classpath: Vec<PathBuf>,
    /// (url, sha1, dest) for every classpath jar that needs downloading.
    downloads: Vec<PendingDownload>,
    /// Native artifacts to download then extract.
    natives: Vec<PendingDownload>,
}

struct PendingDownload {
    url: String,
    sha1: Option<String>,
    dest: PathBuf,
}

/// Resolve which libraries apply to this platform and where each jar lives.
/// Does not download anything yet — see [`fetch_libraries`].
pub fn resolve_libraries(version: &VersionJson, paths: &GamePaths) -> ResolvedLibraries {
    let libs_root = paths.libraries();
    let mut classpath = Vec::new();
    let mut downloads = Vec::new();
    let mut natives = Vec::new();

    for lib in &version.libraries {
        if !rules_allow(&lib.rules) {
            continue;
        }

        // Legacy natives: a `natives` map points at a classifier to extract.
        if let Some(natives_map) = &lib.natives {
            let key = natives_map
                .get(super::manifest::current_os_name())
                .cloned()
                // Some entries key by classifier directly.
                .or_else(|| Some(natives_classifier_key().to_string()));
            if let (Some(key), Some(downloads_block)) = (key, &lib.downloads) {
                let key = key.replace("${arch}", if cfg!(target_pointer_width = "64") { "64" } else { "32" });
                if let Some(classifiers) = &downloads_block.classifiers {
                    if let Some(artifact) = classifiers.get(&key) {
                        if let Some(dest) = artifact_dest(&libs_root, &lib.name, artifact) {
                            natives.push(PendingDownload {
                                url: artifact.url.clone(),
                                sha1: artifact.sha1.clone(),
                                dest,
                            });
                        }
                    }
                }
            }
            continue;
        }

        // Modern natives arrive as ordinary artifacts whose maven classifier
        // contains "natives-" — extract those rather than class-pathing them.
        let is_native = lib.name.contains(":natives-");

        if let Some(dl) = &lib.downloads {
            if let Some(artifact) = &dl.artifact {
                if let Some(dest) = artifact_dest(&libs_root, &lib.name, artifact) {
                    let pending = PendingDownload {
                        url: artifact.url.clone(),
                        sha1: artifact.sha1.clone(),
                        dest: dest.clone(),
                    };
                    if is_native {
                        // Modern (1.19+) natives ship as classifier jars. They
                        // must be on the classpath so Minecraft's
                        // NativeLibrariesBootstrap can load them, and we also
                        // extract them for the legacy -Djava.library.path path.
                        classpath.push(dest);
                        // Forge/NeoForge bundle some artifacts inside their
                        // installer (empty url); those are already on disk.
                        if !artifact.url.is_empty() {
                            natives.push(pending);
                        }
                    } else {
                        classpath.push(dest);
                        if !artifact.url.is_empty() {
                            downloads.push(pending);
                        }
                    }
                }
            }
        } else if let Some(base) = &lib.url {
            // Fabric-style: build the maven path from the coordinate.
            if let Some(rel) = maven_path(&lib.name) {
                let dest = libs_root.join(&rel);
                let url = format!("{}{}", base.trim_end_matches('/'), format!("/{rel}"));
                classpath.push(dest.clone());
                downloads.push(PendingDownload {
                    url,
                    sha1: None,
                    dest,
                });
            }
        } else {
            // No download info at all — assume a maven-central layout jar the
            // caller already has, still add to classpath so ordering holds.
            if let Some(rel) = maven_path(&lib.name) {
                classpath.push(libs_root.join(rel));
            }
        }
    }

    // Merged version JSONs (e.g. NeoForge/Forge layered on vanilla) can list
    // the same library twice; NeoForge's UnionFileSystem crashes with
    // "Duplicate key" if the same jar path appears on the classpath twice.
    let mut seen = std::collections::HashSet::new();
    classpath.retain(|p| seen.insert(p.clone()));

    ResolvedLibraries {
        classpath,
        downloads,
        natives,
    }
}

fn artifact_dest(libs_root: &Path, name: &str, artifact: &Artifact) -> Option<PathBuf> {
    if let Some(path) = &artifact.path {
        Some(libs_root.join(path))
    } else {
        maven_path(name).map(|rel| libs_root.join(rel))
    }
}

/// Convert a maven coordinate (`group:artifact:version[:classifier][@ext]`) to
/// its relative path.
pub(crate) fn maven_path(coord: &str) -> Option<String> {
    // Optional `@ext` overrides the default `.jar` extension.
    let (main, ext) = match coord.split_once('@') {
        Some((m, e)) => (m, e),
        None => (coord, "jar"),
    };
    let parts: Vec<&str> = main.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts.get(3);
    let file = match classifier {
        Some(c) => format!("{artifact}-{version}-{c}.{ext}"),
        None => format!("{artifact}-{version}.{ext}"),
    };
    Some(format!("{group}/{artifact}/{version}/{file}"))
}

/// Download the client jar, all libraries, and extract natives.
pub async fn fetch_libraries<F>(
    client: &Client,
    version: &VersionJson,
    resolved: &ResolvedLibraries,
    paths: &GamePaths,
    report: &F,
) -> Result<(), LaunchError>
where
    F: Fn(ProgressUpdate),
{
    // Client jar.
    if let Some(downloads) = &version.downloads {
        if let Some(client_dl) = &downloads.client {
            let dest = paths.client_jar(&version.id);
            report(ProgressUpdate::stage("Downloading client", 0, 1));
            download_verified(client, &client_dl.url, &dest, client_dl.sha1.as_deref(), false).await?;
            report(ProgressUpdate::stage("Downloading client", 1, 1));
        }
    }

    // Libraries. Each job owns its data so the concurrent futures don't borrow
    // across the stream (which trips higher-ranked-lifetime inference).
    let jobs: Vec<(String, Option<String>, PathBuf)> = resolved
        .downloads
        .iter()
        .map(|p| (p.url.clone(), p.sha1.clone(), p.dest.clone()))
        .collect();
    let total = jobs.len() as u64;
    let mut done = 0u64;
    let mut stream = futures::stream::iter(jobs.into_iter().map(|(url, sha1, dest)| async move {
        download_verified(client, &url, &dest, sha1.as_deref(), false).await
    }))
    .buffer_unordered(DOWNLOAD_CONCURRENCY);
    while let Some(result) = stream.next().await {
        result?;
        done += 1;
        report(ProgressUpdate::stage("Downloading libraries", done, total));
    }

    // Natives: download then extract into the version's natives dir.
    let natives_dir = paths.natives_dir(&version.id);
    std::fs::create_dir_all(&natives_dir)?;
    let native_total = resolved.natives.len() as u64;
    for (i, pending) in resolved.natives.iter().enumerate() {
        download_verified(client, &pending.url, &pending.dest, pending.sha1.as_deref(), false).await?;
        extract_native(&pending.dest, &natives_dir)?;
        report(ProgressUpdate::stage(
            "Preparing natives",
            (i + 1) as u64,
            native_total,
        ));
    }

    Ok(())
}

/// Extract the platform libraries (.dll/.so/.dylib) from a native jar,
/// skipping metadata files.
fn extract_native(jar: &Path, dest_dir: &Path) -> Result<(), LaunchError> {
    let file = std::fs::File::open(jar)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| LaunchError::Extract(format!("{}: {e}", jar.display())))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LaunchError::Extract(e.to_string()))?;
        let name = entry.name().to_string();
        if entry.is_dir() || name.starts_with("META-INF") {
            continue;
        }
        let file_name = match Path::new(&name).file_name() {
            Some(n) => n,
            None => continue,
        };
        let out_path = dest_dir.join(file_name);
        let mut out = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Assets
// ---------------------------------------------------------------------------

// Built manually via `from_value` because one JSON field (`virtual`) is a Rust
// keyword and can't be a struct field name.
struct AssetIndex {
    objects: HashMap<String, AssetObject>,
    virtual_: bool,
    map_to_resources: bool,
}

impl AssetIndex {
    fn from_value(value: serde_json::Value) -> Result<Self, LaunchError> {
        let objects = value
            .get("objects")
            .cloned()
            .unwrap_or_default();
        let objects: HashMap<String, AssetObject> = serde_json::from_value(objects)
            .map_err(|e| LaunchError::Parse(format!("asset index: {e}")))?;
        let virtual_ = value
            .get("virtual")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let map_to_resources = value
            .get("map_to_resources")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(Self {
            objects,
            virtual_,
            map_to_resources,
        })
    }
}

#[derive(Deserialize)]
struct AssetObject {
    hash: String,
}

/// Result of preparing assets: the virtual dir (if any) used for legacy
/// `${game_assets}` substitution. `None` for the modern hashed-object layout.
pub struct AssetLayout {
    pub virtual_dir: Option<PathBuf>,
}

/// Download the asset index and every object, materializing the legacy
/// (`virtual` / `map_to_resources`) layouts when required.
pub async fn fetch_assets<F>(
    client: &Client,
    version: &VersionJson,
    paths: &GamePaths,
    instance_dir: &Path,
    report: &F,
) -> Result<AssetLayout, LaunchError>
where
    F: Fn(ProgressUpdate),
{
    let index_ref = version
        .asset_index
        .as_ref()
        .ok_or_else(|| LaunchError::Parse("version JSON has no assetIndex".into()))?;

    let assets_root = paths.assets();
    let index_path = assets_root
        .join("indexes")
        .join(format!("{}.json", index_ref.id));
    download_verified(client, &index_ref.url, &index_path, Some(&index_ref.sha1), false).await?;

    let raw = std::fs::read(&index_path)?;
    let value: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| LaunchError::Parse(format!("asset index: {e}")))?;
    let index = AssetIndex::from_value(value)?;

    let objects_dir = assets_root.join("objects");
    let virtual_dir = if index.virtual_ {
        Some(assets_root.join("virtual").join(&index_ref.id))
    } else {
        None
    };

    let total = index.objects.len() as u64;
    let mut done = 0u64;
    report(ProgressUpdate::stage("Downloading assets", 0, total));

    // Fully-owned job per object so the concurrent futures borrow nothing but
    // the shared HTTP client (which is `Copy` as a reference).
    struct AssetJob {
        url: String,
        hash: String,
        dest: PathBuf,
        virtual_target: Option<PathBuf>,
        resource_target: Option<PathBuf>,
    }
    let jobs: Vec<AssetJob> = index
        .objects
        .iter()
        .map(|(name, obj)| {
            let hash = obj.hash.clone();
            let sub = hash[0..2].to_string();
            AssetJob {
                url: format!("{RESOURCES_BASE_URL}/{sub}/{hash}"),
                dest: objects_dir.join(&sub).join(&hash),
                virtual_target: virtual_dir.as_ref().map(|v| v.join(name)),
                resource_target: if index.map_to_resources {
                    Some(instance_dir.join("resources").join(name))
                } else {
                    None
                },
                hash,
            }
        })
        .collect();

    let mut stream = futures::stream::iter(jobs.into_iter().map(|job| async move {
        // Content-addressed path (objects/<hash prefix>/<hash>) — safe to
        // trust an existing file without re-hashing it.
        download_verified(client, &job.url, &job.dest, Some(&job.hash), true).await?;
        // Reconstruct legacy layouts by copying to the real filename.
        if let Some(target) = &job.virtual_target {
            copy_if_absent(&job.dest, target)?;
        }
        if let Some(target) = &job.resource_target {
            copy_if_absent(&job.dest, target)?;
        }
        Ok::<(), LaunchError>(())
    }))
    .buffer_unordered(DOWNLOAD_CONCURRENCY);

    while let Some(result) = stream.next().await {
        result?;
        done += 1;
        if done % 25 == 0 || done == total {
            report(ProgressUpdate::stage("Downloading assets", done, total));
        }
    }

    Ok(AssetLayout { virtual_dir })
}

fn copy_if_absent(src: &Path, dest: &Path) -> Result<(), LaunchError> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("waybound-test-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn trusted_cache_hit_skips_network_and_hash_check() {
        let dir = temp_dir("trusted_cache_hit");
        let dest = dir.join("cached.bin");
        std::fs::write(&dest, b"already here").unwrap();

        let client = Client::new();
        // An address nothing listens on: if the cache-hit path ever fell
        // through to a real request, this would fail fast instead of the
        // test silently passing.
        let result = download_verified(
            &client,
            "http://127.0.0.1:1/unreachable",
            &dest,
            Some("deadbeef"), // deliberately wrong — must not matter when trusted
            true,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(std::fs::read(&dest).unwrap(), b"already here");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn untrusted_cache_hit_accepts_a_file_matching_its_hash() {
        let dir = temp_dir("untrusted_cache_hit_match");
        let dest = dir.join("cached.bin");
        std::fs::write(&dest, b"already here").unwrap();
        let real_hash = file_sha1(&dest).unwrap();

        let client = Client::new();
        let result =
            download_verified(&client, "http://127.0.0.1:1/unreachable", &dest, Some(&real_hash), false)
                .await;

        assert!(result.is_ok());
        assert_eq!(std::fs::read(&dest).unwrap(), b"already here");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn untrusted_cache_hit_redownloads_on_hash_mismatch() {
        let dir = temp_dir("untrusted_cache_hit_mismatch");
        let dest = dir.join("cached.bin");
        std::fs::write(&dest, b"stale or corrupted content").unwrap();

        let client = Client::new();
        // Wrong hash forces a redownload attempt; hitting an address nothing
        // listens on proves it actually tried the network this time, unlike
        // the trusted case above.
        let result = download_verified(
            &client,
            "http://127.0.0.1:1/unreachable",
            &dest,
            Some("deadbeef"),
            false,
        )
        .await;

        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
