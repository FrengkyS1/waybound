//! Auto-download a Mojang-provided Java runtime when no suitable local JDK
//! exists. This mirrors what the official launcher, Prism, and Modrinth do:
//! each version JSON names a runtime "component" (e.g. `java-runtime-delta`),
//! and Mojang publishes a per-OS manifest of runtimes and their file lists.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::files::download_verified;
use super::offline;
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

#[derive(Deserialize, Serialize)]
struct FilesManifest {
    files: HashMap<String, FileEntry>,
}

#[derive(Deserialize, Serialize)]
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

#[derive(Deserialize, Serialize)]
struct FileDownloads {
    raw: RawDownload,
}

#[derive(Deserialize, Serialize)]
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
    let marker = dest_root.join(".complete");
    let files_key = format!("java/{}/{component}.json", os_key());
    // A completion record contains the manifest, not permission to skip file
    // verification. It also preserves repair metadata if the cache is removed.
    let cached_files = offline::read_json::<FilesManifest>(runtimes_root, &files_key)
        .or_else(|| std::fs::read(&marker).ok().and_then(|raw| serde_json::from_slice(&raw).ok()));
    let files = if let Some(files) = cached_files {
        files
    } else {
        if java_exe.is_file() && local_java_works(java_exe.clone()).await? {
            // Legacy installs have no hashes to audit. A successful JVM probe
            // permits local reuse, but does not certify the entire file tree.
            // Do not stamp completion or force a network request for migration.
            report(ProgressUpdate::stage(
                "Using existing Java runtime (full file verification unavailable)", 1, 1,
            ));
            return Ok(java_exe);
        }
        report(ProgressUpdate::stage("Locating Java runtime", 0, 1));
        let all: AllManifest = offline::fetch_json_cached(
            client, runtimes_root, "java/all.json", JAVA_MANIFEST_URL,
        ).await?;
        let os = os_key();
        let entry = all
            .get(os)
            .and_then(|components| components.get(component))
            .and_then(|entries| entries.first())
            .ok_or_else(|| LaunchError::Parse(format!(
                "Mojang has no Java runtime '{component}' for {os}"
            )))?;
        offline::fetch_json_cached(client, runtimes_root, &files_key, &entry.manifest.url).await?
    };
    validate_manifest(&files)?;
    let record = serde_json::to_vec(&files)
        .map_err(|e| LaunchError::Parse(format!("java completion record: {e}")))?;
    // Preserve the only known manifest before removing a completion record.
    // A failed offline repair must remain repairable on the next launch.
    let cache_path = offline::cache_root(runtimes_root).join(&files_key);
    std::fs::create_dir_all(cache_path.parent().expect("runtime cache has a parent"))?;
    std::fs::write(cache_path, &record)?;
    // Invalidate any previous completion record before repair. Cancellation or
    // any failed file/link/permission operation must not leave a success stamp.
    remove_if_present(&marker)?;

    let mut jobs = Vec::new();
    for (rel, entry) in &files.files {
        let path = dest_root.join(rel);
        match entry.kind.as_str() {
            "file" => {
                let raw = &entry.downloads.as_ref().expect("validated file entry").raw;
                jobs.push((raw.url.clone(), raw.sha1.clone(), path, entry.executable));
            }
            "directory" => std::fs::create_dir_all(path)?,
            _ => {}
        }
    }

    let total = jobs.len() as u64;
    let mut done = 0u64;
    let mut stream = futures::stream::iter(jobs.into_iter().map(|(url, sha1, path, exec)| async move {
        download_verified(client, &url, &path, Some(&sha1), false).await?;
        set_executable(&path, exec)?;
        Ok::<(), LaunchError>(())
    }))
    .buffer_unordered(DOWNLOAD_CONCURRENCY);
    while let Some(result) = stream.next().await {
        result?;
        done += 1;
        if done % 20 == 0 || done == total {
            report(ProgressUpdate::stage("Verifying Java runtime", done, total));
        }
    }
    // Materialize links after files, so Windows can determine their type.
    for (rel, entry) in &files.files {
        if entry.kind == "link" {
            make_link(&dest_root.join(rel), entry.target.as_deref().expect("validated link entry"))?;
        }
    }
    for (rel, entry) in &files.files {
        let path = dest_root.join(rel);
        let valid = match entry.kind.as_str() {
            "directory" => path.is_dir(),
            "link" => std::fs::read_link(&path)? == Path::new(entry.target.as_deref().expect("validated link entry"))
                && path.exists(),
            _ => path.is_file(),
        };
        if !valid {
            return Err(LaunchError::Parse(format!("Java runtime entry {} is incomplete", path.display())));
        }
    }
    if !java_exe.is_file() {
        return Err(LaunchError::Parse(format!(
            "Java runtime '{component}' downloaded but {} is missing", java_exe.display()
        )));
    }
    std::fs::write(&marker, record)?;
    Ok(java_exe)
}

fn validate_manifest(files: &FilesManifest) -> Result<(), LaunchError> {
    let java_name = format!("bin/{}", java_exe_name());
    if !files.files.get(&java_name).is_some_and(|entry| matches!(entry.kind.as_str(), "file" | "link")) {
        return Err(LaunchError::Parse("Java files manifest has no Java executable".into()));
    }
    for (rel, entry) in &files.files {
        let valid = match entry.kind.as_str() {
            "file" => entry.downloads.as_ref().is_some_and(|dl| {
                !dl.raw.url.is_empty() && dl.raw.sha1.len() == 40
                    && dl.raw.sha1.bytes().all(|b| b.is_ascii_hexdigit())
            }),
            "directory" => true,
            "link" => entry.target.as_ref().is_some_and(|target| !target.is_empty()),
            _ => false,
        };
        if !valid {
            return Err(LaunchError::Parse(format!("Invalid Java runtime entry '{rel}' ({})", entry.kind)));
        }
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn local_java_works(path: PathBuf) -> Result<bool, LaunchError> {
    tokio::task::spawn_blocking(move || {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let mut command = Command::new(path);
        command.arg("-version").stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let Ok(mut child) = command.spawn() else { return Ok(false); };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status.success());
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }).await.map_err(|e| LaunchError::Spawn(format!("Java probe failed: {e}")))?
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
fn set_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if executable {
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> std::io::Result<()> { Ok(()) }

#[cfg(unix)]
fn make_link(path: &Path, target: &str) -> std::io::Result<()> {
    if std::fs::read_link(path).ok().as_deref() == Some(Path::new(target)) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    remove_if_present(path)?;
    std::os::unix::fs::symlink(target, path)
}

#[cfg(windows)]
fn make_link(path: &Path, target: &str) -> std::io::Result<()> {
    if std::fs::read_link(path).ok().as_deref() == Some(Path::new(target)) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    remove_if_present(path)?;
    if path.parent().unwrap_or(Path::new(".")).join(target).is_dir() {
        std::os::windows::fs::symlink_dir(target, path)
    } else {
        std::os::windows::fs::symlink_file(target, path)
    }
}

#[cfg(not(any(unix, windows)))]
fn make_link(_path: &Path, _target: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Java runtime requires symbolic links"))
}

#[cfg(test)]
mod java_runtime_manifest_tests {
    use super::{AllManifest, FilesManifest, java_exe_name, os_key};

    #[test]
    fn os_key_and_exe_name_agree_on_the_build_target() {
        // These two are derived from the same cfg! flags but in separate
        // functions — a platform added to one and not the other would
        // silently download a runtime whose executable is then looked up
        // under the wrong name.
        let key = os_key();
        if cfg!(windows) {
            assert!(key.starts_with("windows-"), "unexpected key {key}");
            assert_eq!(java_exe_name(), "java.exe");
        } else {
            assert!(!key.starts_with("windows-"), "unexpected key {key}");
            assert_eq!(java_exe_name(), "java");
        }
    }

    #[test]
    fn parses_all_json_shape_mojang_actually_serves() {
        // Trimmed to the two fields this code reads, but the nesting
        // (os -> component -> array) is the real shape.
        let raw = r#"{
            "windows-x64": {
                "java-runtime-delta": [
                    { "manifest": { "url": "https://example.invalid/delta.json" } }
                ],
                "java-runtime-gamma": []
            }
        }"#;

        let all: AllManifest = serde_json::from_str(raw).expect("should parse");
        let delta = &all["windows-x64"]["java-runtime-delta"];
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].manifest.url, "https://example.invalid/delta.json");
        // A component present but with no builds for this OS is normal, not
        // an error — the caller has to treat it as "not available here".
        assert!(all["windows-x64"]["java-runtime-gamma"].is_empty());
    }

    #[test]
    fn file_entry_defaults_cover_directories_and_links() {
        // Directory and link entries legitimately omit `downloads`, and most
        // file entries omit `executable`. Missing != malformed here; if
        // these stopped defaulting, every runtime download would fail to
        // parse partway through.
        let raw = r#"{
            "files": {
                "bin": { "type": "directory" },
                "bin/java": {
                    "type": "file",
                    "executable": true,
                    "downloads": {
                        "raw": { "url": "https://example.invalid/java", "sha1": "abc123" }
                    }
                },
                "lib/link": { "type": "link", "target": "../real" }
            }
        }"#;

        let manifest: FilesManifest = serde_json::from_str(raw).expect("should parse");

        let dir = &manifest.files["bin"];
        assert_eq!(dir.kind, "directory");
        assert!(dir.downloads.is_none());
        assert!(!dir.executable, "absent `executable` must default to false");
        assert!(dir.target.is_none());

        let exe = &manifest.files["bin/java"];
        assert!(exe.executable);
        let raw_dl = &exe.downloads.as_ref().expect("file entry has downloads").raw;
        assert_eq!(raw_dl.sha1, "abc123");

        assert_eq!(manifest.files["lib/link"].target.as_deref(), Some("../real"));
    }
}

#[cfg(test)]
mod java_runtime_completion_tests {
    use super::*;
    use sha1::{Digest, Sha1};

    fn install_fixture(root: &Path) -> PathBuf {
        let runtime = root.join("test-runtime");
        std::fs::create_dir_all(runtime.join("bin")).unwrap();
        std::fs::create_dir_all(runtime.join("lib")).unwrap();
        let mut entries = serde_json::Map::new();
        for (name, contents) in [(format!("bin/{}", java_exe_name()), b"java".as_slice()), ("lib/runtime.dat".into(), b"runtime".as_slice())] {
            std::fs::write(runtime.join(&name), contents).unwrap();
            entries.insert(name, serde_json::json!({
                "type": "file", "downloads": { "raw": {
                    "url": "file:///no-network-in-tests", "sha1": hex::encode(Sha1::digest(contents))
                }}
            }));
        }
        let cache = root.join("meta-cache/java").join(os_key());
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("test-runtime.json"), serde_json::to_vec(&serde_json::json!({"files": entries})).unwrap()).unwrap();
        runtime
    }

    #[tokio::test]
    async fn markerless_cached_runtime_is_verified_without_downloading() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = install_fixture(dir.path());
        let java = ensure_component(&Client::new(), dir.path(), "test-runtime", &|_| {}).await.unwrap();
        assert_eq!(java, runtime.join("bin").join(java_exe_name()));
        // Losing the metadata cache must not destroy offline verification.
        std::fs::remove_dir_all(dir.path().join("meta-cache")).unwrap();
        ensure_component(&Client::new(), dir.path(), "test-runtime", &|_| {}).await.unwrap();
        std::fs::write(runtime.join("lib/runtime.dat"), b"corrupt").unwrap();
        assert!(ensure_component(&Client::new(), dir.path(), "test-runtime", &|_| {}).await.is_err());
        assert!(!runtime.join(".complete").exists());
    }

    #[tokio::test]
    async fn missing_runtime_file_cannot_hide_behind_completion_record() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = install_fixture(dir.path());
        ensure_component(&Client::new(), dir.path(), "test-runtime", &|_| {}).await.unwrap();
        std::fs::remove_file(runtime.join("lib/runtime.dat")).unwrap();
        assert!(ensure_component(&Client::new(), dir.path(), "test-runtime", &|_| {}).await.is_err());
        assert!(!runtime.join(".complete").exists());
        std::fs::write(runtime.join("lib/runtime.dat"), b"runtime").unwrap();
        ensure_component(&Client::new(), dir.path(), "test-runtime", &|_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn completion_write_failure_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = install_fixture(dir.path());
        let result = ensure_component(&Client::new(), dir.path(), "test-runtime", &|progress| {
            if progress.stage == "Verifying Java runtime" && progress.current == progress.total {
                std::fs::create_dir(runtime.join(".complete")).unwrap();
            }
        }).await;
        assert!(matches!(result, Err(LaunchError::Io(_))));
    }

    #[tokio::test]
    async fn missing_file_download_metadata_cannot_stamp_completion() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = install_fixture(dir.path());
        let cache = dir.path().join("meta-cache/java").join(os_key()).join("test-runtime.json");
        let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
        manifest["files"]["lib/runtime.dat"].as_object_mut().unwrap().remove("downloads");
        std::fs::write(cache, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(matches!(ensure_component(&Client::new(), dir.path(), "test-runtime", &|_| {}).await, Err(LaunchError::Parse(_))));
        assert!(!runtime.join(".complete").exists());
    }

    #[cfg(unix)]
    #[test]
    fn link_and_executable_failures_are_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::fs::create_dir(&link).unwrap();
        assert!(make_link(&link, "target").is_err());
        assert!(set_executable(&dir.path().join("missing"), true).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_working_java_is_reused_without_claiming_completion() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("test-runtime");
        std::fs::create_dir_all(runtime.join("bin")).unwrap();
        let java = runtime.join("bin/java");
        std::fs::write(&java, b"#!/bin/sh\nexit 0\n").unwrap();
        set_executable(&java, true).unwrap();
        assert_eq!(ensure_component(&Client::new(), dir.path(), "test-runtime", &|_| {}).await.unwrap(), java);
        assert!(!runtime.join(".complete").exists());
        assert!(!dir.path().join("meta-cache").exists());
    }
}
