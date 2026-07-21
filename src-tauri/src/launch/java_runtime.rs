//! Auto-download a Mojang-provided Java runtime when no suitable local JDK
//! exists. This mirrors what the official launcher, Prism, and Modrinth do:
//! each version JSON names a runtime "component" (e.g. `java-runtime-delta`),
//! and Mojang publishes a per-OS manifest of runtimes and their file lists.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures::stream::StreamExt;
use reqwest::Client;
use serde::Deserialize;

use super::files::download_verified;
use super::{LaunchError, ProgressUpdate};

const JAVA_MANIFEST_URL: &str = "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";
const DOWNLOAD_CONCURRENCY: usize = 8;

/// `all.json`: `{ "<os>": { "<component>": [ RuntimeEntry, ... ] } }`.
type AllManifest = HashMap<String, HashMap<String, Vec<RuntimeEntry>>>;

#[derive(Deserialize)]
struct RuntimeEntry {
    manifest: ManifestRef,
}

#[derive(Deserialize)]
struct ManifestRef {
    url: String,
}

#[derive(Deserialize)]
struct FilesManifest {
    files: HashMap<String, FileEntry>,
}

#[derive(Deserialize)]
struct FileEntry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    downloads: Option<FileDownloads>,
    #[serde(default)]
    executable: bool,
    #[serde(default)]
    target: Option<String>,
}

#[derive(Deserialize)]
struct FileDownloads {
    raw: RawDownload,
}

#[derive(Deserialize)]
struct RawDownload {
    url: String,
    sha1: String,
}

/// Ensure the named runtime component is present under `runtimes_root`,
/// downloading it if necessary, and return the path to its `java` executable.
pub async fn ensure_component<F>(
    client: &Client,
    runtimes_root: &Path,
    component: &str,
    report: &F,
) -> Result<PathBuf, LaunchError>
where
    F: Fn(ProgressUpdate),
{
    let dest_root = runtimes_root.join(component);
    let java_exe = dest_root.join("bin").join(java_exe_name());
    if java_exe.exists() {
        return Ok(java_exe);
    }

    report(ProgressUpdate::stage("Locating Java runtime", 0, 1));

    let all: AllManifest = client
        .get(JAVA_MANIFEST_URL)
        .send()
        .await?
        .json()
        .await
        .map_err(|e| LaunchError::Parse(format!("java runtime manifest: {e}")))?;

    let os = os_key();
    let entry = all
        .get(os)
        .and_then(|components| components.get(component))
        .and_then(|entries| entries.first())
        .ok_or_else(|| {
            LaunchError::Parse(format!(
                "Mojang has no Java runtime '{component}' for {os}"
            ))
        })?;

    let files: FilesManifest = client
        .get(&entry.manifest.url)
        .send()
        .await?
        .json()
        .await
        .map_err(|e| LaunchError::Parse(format!("java files manifest: {e}")))?;

    // Split into downloadable files and symlinks (links only occur on unix).
    let mut jobs: Vec<(String, String, PathBuf, bool)> = Vec::new();
    for (rel, entry) in &files.files {
        let path = dest_root.join(rel);
        match entry.kind.as_str() {
            "file" => {
                if let Some(dl) = &entry.downloads {
                    jobs.push((dl.raw.url.clone(), dl.raw.sha1.clone(), path, entry.executable));
                }
            }
            "link" => {
                if let Some(target) = &entry.target {
                    let _ = make_link(&path, target);
                }
            }
            _ => {}
        }
    }

    let total = jobs.len() as u64;
    let mut done = 0u64;
    let mut stream = futures::stream::iter(jobs.into_iter().map(|(url, sha1, path, exec)| async move {
        download_verified(client, &url, &path, Some(&sha1), false).await?;
        set_executable(&path, exec);
        Ok::<(), LaunchError>(())
    }))
    .buffer_unordered(DOWNLOAD_CONCURRENCY);

    while let Some(result) = stream.next().await {
        result?;
        done += 1;
        if done % 20 == 0 || done == total {
            report(ProgressUpdate::stage("Downloading Java runtime", done, total));
        }
    }

    if !java_exe.exists() {
        return Err(LaunchError::Parse(format!(
            "Java runtime '{component}' downloaded but {} is missing",
            java_exe.display()
        )));
    }
    Ok(java_exe)
}

fn java_exe_name() -> &'static str {
    if cfg!(windows) {
        "java.exe"
    } else {
        "java"
    }
}

fn os_key() -> &'static str {
    if cfg!(windows) {
        if cfg!(target_arch = "aarch64") {
            "windows-arm64"
        } else if cfg!(target_arch = "x86") {
            "windows-x86"
        } else {
            "windows-x64"
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "mac-os-arm64"
        } else {
            "mac-os"
        }
    } else if cfg!(target_arch = "x86") {
        "linux-i386"
    } else {
        "linux"
    }
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) {
    use std::os::unix::fs::PermissionsExt;
    if executable {
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o755);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) {}

#[cfg(unix)]
fn make_link(path: &Path, target: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(path);
    std::os::unix::fs::symlink(target, path)
}

#[cfg(not(unix))]
fn make_link(_path: &Path, _target: &str) -> std::io::Result<()> {
    Ok(())
}
