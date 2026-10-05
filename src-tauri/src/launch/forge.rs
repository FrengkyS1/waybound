//! Forge and NeoForge support.
//!
//! Unlike Fabric (a plain profile we merge), Forge/NeoForge ship an *installer*
//! whose `install_profile.json` lists "processors" — Java tools that must be run
//! to binary-patch the vanilla client into the modded client and generate
//! mappings. We replicate the installer client flow (the same thing Prism does):
//! download the installer, extract `install_profile.json` + `version.json`,
//! fetch libraries (including the ones bundled inside the installer), run each
//! processor, and cache the result so subsequent launches skip straight to the
//! merged version JSON.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

use reqwest::Client;
use serde::{Deserialize, Serialize};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

use crate::dto::ModLoader;
use crate::download::CancelToken;

use super::files::{download_verified, file_sha1, maven_path, GamePaths};
use super::manifest::VersionJson;
use super::{cancellable, check_cancelled, LaunchError, ProgressUpdate};

const FORGE_MAVEN: &str = "https://maven.minecraftforge.net";
const FORGE_PROMOTIONS: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases";
const NEOFORGE_META: &str =
    "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml";
// The original 1.20.1 fork was published under a separate `forge` artifact
// whose versions carry the game version Forge-promotions style
// (`1.20.1-47.1.11`), not the stripped form the modern artifact uses.
const NEOFORGE_LEGACY_META: &str =
    "https://maven.neoforged.net/releases/net/neoforged/forge/maven-metadata.xml";

#[derive(Deserialize)]
struct Promotions {
    promos: HashMap<String, String>,
}

#[derive(Deserialize)]
struct InstallProfile {
    #[serde(default)]
    data: HashMap<String, SidedData>,
    #[serde(default)]
    processors: Vec<Processor>,
    #[serde(default)]
    libraries: Vec<super::manifest::Library>,
}

#[derive(Deserialize)]
struct SidedData {
    client: String,
}

#[derive(Deserialize, Clone)]
struct Processor {
    #[serde(default)]
    sides: Option<Vec<String>>,
    jar: String,
    #[serde(default)]
    classpath: Vec<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    outputs: HashMap<String, String>,
}

/// Resolve the newest loader version for a game version, or use the caller's.
pub async fn resolve_version(
    client: &Client,
    loader: ModLoader,
    game_version: &str,
    requested: Option<String>,
) -> Result<String, LaunchError> {
    if let Some(v) = requested.filter(|s| !s.trim().is_empty()) {
        return Ok(v);
    }
    match loader {
        ModLoader::Forge => latest_forge(client, game_version).await,
        ModLoader::NeoForge => latest_neoforge(client, game_version).await,
        other => Err(LaunchError::UnsupportedLoader(format!("{other:?}"))),
    }
}

/// Unpinned instances reuse a prepared matching loader before consulting remote
/// promotions. The inherited game version is checked exactly, not by prefix.
pub async fn resolve_cached_version(
    client: &Client, paths: &GamePaths, loader: ModLoader,
    game_version: &str, requested: Option<String>,
) -> Result<String, LaunchError> {
    if let Some(version) = requested.filter(|v| !v.trim().is_empty()) { return Ok(version); }
    let prefix = if loader == ModLoader::NeoForge { "neoforge-".to_string() } else { format!("forge-{game_version}-") };
    let mut installed = Vec::new();
    if let Ok(entries) = std::fs::read_dir(paths.versions()) {
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            let Some(version) = id.strip_prefix(&prefix) else { continue };
            if !entry.path().join(".installed").is_file() { continue; }
            let Ok(raw) = std::fs::read(entry.path().join(format!("{id}.json"))) else { continue };
            let Ok(profile) = serde_json::from_slice::<VersionJson>(&raw) else { continue };
            if profile.inherits_from.as_deref() == Some(game_version) {
                installed.push(version.to_owned());
            }
        }
    }
    if let Some(version) = installed.into_iter().max_by_key(|v| version_key(v)) { return Ok(version); }
    tokio::time::timeout(std::time::Duration::from_secs(8), resolve_version(client, loader, game_version, None))
        .await.map_err(|_| LaunchError::Parse(format!("No installed loader metadata for Minecraft {game_version}. Connect to the internet to prepare it once.")))?
}

async fn fetch_forge_promotions(client: &Client) -> Result<Promotions, LaunchError> {
    client
        .get(FORGE_PROMOTIONS)
        .send()
        .await?
        .json()
        .await
        .map_err(|e| LaunchError::Parse(format!("forge promotions: {e}")))
}

pub(crate) async fn latest_forge(client: &Client, game_version: &str) -> Result<String, LaunchError> {
    let promos = fetch_forge_promotions(client).await?;
    promos
        .promos
        .get(&format!("{game_version}-recommended"))
        .or_else(|| promos.promos.get(&format!("{game_version}-latest")))
        .cloned()
        .ok_or_else(|| LaunchError::Parse(format!("no Forge build for {game_version}")))
}

/// Forge's "recommended" build lags behind "latest" — sometimes for months,
/// as with 1.20.1 staying on 47.4.10 while 47.4.22 was out — and a mod can
/// (and did, in practice) raise its own minimum Forge requirement past
/// whatever "recommended" currently is. `latest_forge` above stays
/// recommended-first for automatic/fresh-instance resolution, where the more
/// conservative default is the right call; this is specifically for the
/// user's own explicit "check for a newer build" action, where surfacing
/// only "recommended" can never actually offer the fix for that exact case.
pub(crate) async fn latest_forge_build(client: &Client, game_version: &str) -> Result<String, LaunchError> {
    let promos = fetch_forge_promotions(client).await?;
    promos
        .promos
        .get(&format!("{game_version}-latest"))
        .or_else(|| promos.promos.get(&format!("{game_version}-recommended")))
        .cloned()
        .ok_or_else(|| LaunchError::Parse(format!("no Forge build for {game_version}")))
}

pub(crate) async fn latest_neoforge(
    client: &Client,
    game_version: &str,
) -> Result<String, LaunchError> {
    // NeoForge versions embed the targeted game version, but the mapping has
    // changed twice (PrismLauncher reads the authoritative value out of each
    // installer's install_profile.json; prefix matching is our lighter
    // equivalent):
    //
    // - Legacy `1.x.y` builds strip the leading `1.`: MC `1.20.4` ->
    //   `20.4.190`, and a bare `1.21` maps to the `21.0.` lineage. The
    //   separate 1.20.1 fork instead uses long forms: MC `1.20.1` ->
    //   `1.20.1-47.1.11` under the legacy artifact.
    // - Since Mojang dropped the `1.` prefix (year-based versions), NeoForge
    //   embeds the FULL game version: MC `26.2` -> `26.2.0.67`, MC
    //   `26.1.2` -> `26.1.2.97`.
    let prefixes = neoforge_prefix_candidates(game_version);

    let mut pool = fetch_maven_versions(client, NEOFORGE_META).await?;
    if game_version.starts_with("1.") {
        pool.extend(fetch_maven_versions(client, NEOFORGE_LEGACY_META).await?);
    }

    newest_matching(&pool, &prefixes)
        .ok_or_else(|| LaunchError::Parse(format!("no NeoForge build for {game_version}")))
}

/// Prefix candidates for `game_version`, most specific first.
fn neoforge_prefix_candidates(game_version: &str) -> Vec<String> {
    let mut parts = game_version.split('.');
    let first = parts.next().unwrap_or("");
    if first.is_empty() {
        return Vec::new();
    }
    if first == "1" {
        let major = parts.next().unwrap_or("");
        let minor = parts.next().unwrap_or("0");
        return vec![format!("{major}.{minor}."), format!("{game_version}-")];
    }

    let mut candidates = Vec::new();
    if game_version.matches('.').count() == 1 {
        // A bare two-component id like `26.1` pins the `.0` lineage; try it
        // before the broader prefix so a later point-release build
        // (`26.1.2.x`, which targets MC `26.1.2`) is never picked for `26.1`.
        candidates.push(format!("{game_version}.0."));
    }
    candidates.push(format!("{game_version}."));
    candidates.push(format!("{game_version}-"));
    candidates
}

async fn fetch_maven_versions(client: &Client, meta_url: &str) -> Result<Vec<String>, LaunchError> {
    let xml = client.get(meta_url).send().await?.text().await?;
    Ok(xml
        .split("<version>")
        .skip(1)
        .filter_map(|chunk| chunk.find("</version>").map(|end| chunk[..end].trim().to_string()))
        .collect())
}

/// Newest build matching the most specific candidate prefix that yields
/// anything, preferring stable builds over `-beta`/`-alpha` prereleases
/// within that tier (the maven metadata interleaves both).
fn newest_matching(pool: &[String], prefixes: &[String]) -> Option<String> {
    for prefix in prefixes {
        let matches: Vec<&str> = pool
            .iter()
            .map(String::as_str)
            .filter(|v| v.starts_with(prefix.as_str()))
            .collect();
        if matches.is_empty() {
            continue;
        }
        let pick =
            |candidates: &[&str]| -> Option<String> {
                candidates
                    .iter()
                    .copied()
                    .max_by(|a, b| version_key(a).cmp(&version_key(b)))
                    .map(str::to_string)
            };
        let stable: Vec<&str> = matches.iter().copied().filter(|v| !is_prerelease(v)).collect();
        return pick(&stable).or_else(|| pick(&matches));
    }
    None
}

/// A prerelease when some dash-separated segment isn't purely numeric:
/// `26.2.0.43-beta` and `26.1.0.0-alpha.2+snapshot-1` are, but the legacy
/// artifact's `1.20.1-47.1.11` (the dash separates the game version) is not.
fn is_prerelease(version: &str) -> bool {
    version.split('-').skip(1).any(|seg| {
        seg.is_empty()
            || !seg
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    })
}

/// Numeric component vector for semver-ish ordering ("21.1.66" -> [21, 1, 66]).
/// Non-numeric pieces parse as 0; lexicographic comparison on the vectors
/// avoids string-ordering mistakes like "100" < "66".
fn version_key(s: &str) -> Vec<u64> {
    s.split(|c: char| c == '.' || c == '-')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect()
}

/// True when `a` is a newer semver-ish version than `b`.
#[cfg(test)]
fn newer(a: &str, b: &str) -> bool {
    version_key(a) > version_key(b)
}

fn installer_url(loader: ModLoader, game_version: &str, loader_version: &str) -> String {
    match loader {
        ModLoader::NeoForge if game_version == "1.20.1" => format!(
            "{NEOFORGE_MAVEN}/net/neoforged/forge/{loader_version}/forge-{loader_version}-installer.jar"
        ),
        ModLoader::NeoForge => format!(
            "{NEOFORGE_MAVEN}/net/neoforged/neoforge/{loader_version}/neoforge-{loader_version}-installer.jar"
        ),
        _ => {
            let full = format!("{game_version}-{loader_version}");
            format!("{FORGE_MAVEN}/net/minecraftforge/forge/{full}/forge-{full}-installer.jar")
        }
    }
}

fn install_id(loader: ModLoader, game_version: &str, loader_version: &str) -> String {
    match loader {
        ModLoader::NeoForge => format!("neoforge-{loader_version}"),
        _ => format!("forge-{game_version}-{loader_version}"),
    }
}

/// Ensure the loader is installed (running processors once), returning the
/// merged, ready-to-launch version JSON layered onto `vanilla`.
pub async fn prepare<F>(
    client: &Client,
    paths: &GamePaths,
    loader: ModLoader,
    game_version: &str,
    loader_version: &str,
    vanilla: &VersionJson,
    java_path: &str,
    cancel: &CancelToken,
    report: &F,
) -> Result<VersionJson, LaunchError>
where
    F: Fn(ProgressUpdate),
{
    check_cancelled(cancel)?;
    let id = install_id(loader, game_version, loader_version);
    let install_dir = paths.version_dir(&id);
    let version_json_path = install_dir.join(format!("{id}.json"));
    let marker = install_dir.join(".installed");

    // A marker alone cannot prove processor outputs remain intact. Old "ok"
    // markers and damaged profiles are rebuilt from the cached installer.
    if let Some(profile) = installed_profile(&version_json_path, &marker, paths, game_version) {
        return Ok(super::fabric::merge_onto_parent(profile, vanilla.clone()));
    }
    if marker.exists() {
        std::fs::remove_file(&marker)?;
    }

    report(ProgressUpdate::stage("Downloading loader installer", 0, 1));
    std::fs::create_dir_all(&install_dir)?;

    // download_verified re-downloads only when the jar is absent or unreadable,
    // recovering interrupted preparations without touching the live install.
    let url = installer_url(loader, game_version, loader_version);
    let installer_path = install_dir.join("installer.jar");
    cancellable(cancel, download_verified(client, &url, &installer_path, None, false)).await?;
    let installer_bytes = std::fs::read(&installer_path)?;

    // Extract the two JSON descriptors.
    let install_profile: InstallProfile =
        read_json_from_zip(&installer_bytes, "install_profile.json")?;
    let version_profile: VersionJson = read_json_from_zip(&installer_bytes, "version.json")?;

    // Ensure the vanilla client jar exists (processors patch it).
    if let Some(downloads) = &vanilla.downloads {
        if let Some(client_dl) = &downloads.client {
            let dest = paths.client_jar(game_version);
            cancellable(cancel, download_verified(client, &client_dl.url, &dest, client_dl.sha1.as_deref(), false)).await?;
        }
    }

    // Download every library (installer + version), extracting the ones bundled
    // inside the installer jar (empty download URL).
    let libs_root = paths.libraries();
    let all_libs = install_profile
        .libraries
        .iter()
        .chain(version_profile.libraries.iter());
    report(ProgressUpdate::stage("Downloading loader libraries", 0, 1));
    for lib in all_libs {
        check_cancelled(cancel)?;
        cancellable(cancel, fetch_loader_library(client, lib, &libs_root, &installer_bytes)).await?;
    }

    // Resolve `data` entries (extract bundled files, resolve maven refs).
    let data_dir = install_dir.join("data");
    let mut data: HashMap<String, String> = HashMap::new();
    for (key, sided) in &install_profile.data {
        check_cancelled(cancel)?;
        let resolved = resolve_data_value(&sided.client, &libs_root, &installer_bytes, &data_dir)?;
        data.insert(key.clone(), resolved);
    }

    // Await every processor directly: cancellation kills and reaps its child
    // before the preparation future returns and releases the instance guard.
    let total = install_profile.processors.len();
    for (i, proc) in install_profile.processors.iter().enumerate() {
        if let Some(sides) = &proc.sides {
            if !sides.iter().any(|s| s == "client") {
                continue;
            }
        }
        report(ProgressUpdate::stage(
            "Running loader processors",
            (i + 1) as u64,
            total as u64,
        ));
        run_processor(proc, &libs_root, &data, paths, game_version, java_path, cancel).await?;
    }
    check_cancelled(cancel)?;
    let receipt = installed_artifacts(&version_profile, &install_profile, &data, paths, game_version)?;
    let raw = raw_version_json(&installer_bytes)?;
    super::offline::write_atomic(&version_json_path, &serde_json::to_vec_pretty(&raw)
        .map_err(|e| LaunchError::Parse(e.to_string()))?)?;
    super::offline::write_atomic(&marker, &serde_json::to_vec(&receipt)
        .map_err(|e| LaunchError::Parse(e.to_string()))?)?;

    Ok(super::fabric::merge_onto_parent(version_profile, vanilla.clone()))
}

#[derive(Deserialize, Serialize)]
struct InstalledArtifact {
    path: PathBuf,
    sha1: String,
}

fn installed_profile(
    version_path: &Path,
    marker: &Path,
    paths: &GamePaths,
    game_version: &str,
) -> Option<VersionJson> {
    let profile: VersionJson = serde_json::from_slice(&std::fs::read(version_path).ok()?).ok()?;
    if profile.inherits_from.as_deref() != Some(game_version) { return None; }
    let receipt: Vec<InstalledArtifact> = serde_json::from_slice(&std::fs::read(marker).ok()?).ok()?;
    if receipt.iter().any(|artifact| file_sha1(&artifact.path).as_deref() != Some(artifact.sha1.as_str())) {
        return None;
    }
    for lib in &profile.libraries {
        let Some(artifact) = lib.downloads.as_ref().and_then(|downloads| downloads.artifact.as_ref()) else { continue };
        if !artifact.url.is_empty() { continue; }
        let relative = artifact.path.clone().or_else(|| maven_path(&lib.name))?;
        let path = paths.libraries().join(relative);
        let entry = receipt.iter().find(|entry| entry.path == path)?;
        if artifact.sha1.as_ref().is_some_and(|expected| expected != &entry.sha1) { return None; }
    }
    Some(profile)
}

fn installed_artifacts(
    version: &VersionJson,
    install: &InstallProfile,
    data: &HashMap<String, String>,
    paths: &GamePaths,
    game_version: &str,
) -> Result<Vec<InstalledArtifact>, LaunchError> {
    let libs_root = paths.libraries();
    let mut outputs: HashMap<PathBuf, Option<String>> = HashMap::new();
    for lib in &version.libraries {
        let Some(artifact) = lib.downloads.as_ref().and_then(|downloads| downloads.artifact.as_ref()) else { continue };
        if !artifact.url.is_empty() { continue; }
        let relative = artifact.path.clone().or_else(|| maven_path(&lib.name))
            .ok_or_else(|| LaunchError::Parse(format!("bad generated library: {}", lib.name)))?;
        outputs.insert(libs_root.join(relative), artifact.sha1.clone());
    }
    for processor in &install.processors {
        if processor.sides.as_ref().is_some_and(|sides| !sides.iter().any(|side| side == "client")) { continue; }
        for (path, hash) in &processor.outputs {
            let path = substitute_processor_arg(path, data, &libs_root, paths, game_version);
            let path = PathBuf::from(path.trim_matches('\''));
            let path = if path.is_absolute() { path } else { paths.root.join(path) };
            let hash = substitute_processor_arg(hash, data, &libs_root, paths, game_version);
            outputs.insert(path, Some(hash.trim_matches('\'').to_string()));
        }
    }
    outputs.into_iter().map(|(path, expected)| {
        let sha1 = file_sha1(&path).ok_or_else(|| LaunchError::Parse(format!("loader processor did not create {}", path.display())))?;
        if expected.as_ref().is_some_and(|expected| !sha1.eq_ignore_ascii_case(expected)) {
            return Err(LaunchError::HashMismatch { url: path.to_string_lossy().to_string() });
        }
        Ok(InstalledArtifact { path, sha1 })
    }).collect()
}

/// Read the raw version.json value (preserving all fields) for caching.
fn raw_version_json(installer_bytes: &[u8]) -> Result<serde_json::Value, LaunchError> {
    read_json_from_zip(installer_bytes, "version.json")
}

fn read_json_from_zip<T: for<'de> Deserialize<'de>>(
    zip_bytes: &[u8],
    name: &str,
) -> Result<T, LaunchError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| LaunchError::Extract(e.to_string()))?;
    let mut file = archive
        .by_name(name)
        .map_err(|_| LaunchError::Parse(format!("installer has no {name}")))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    serde_json::from_str(&contents).map_err(|e| LaunchError::Parse(format!("{name}: {e}")))
}

/// Extract a single file from the installer jar to `dest`.
fn extract_from_zip(zip_bytes: &[u8], name: &str, dest: &Path) -> Result<bool, LaunchError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| LaunchError::Extract(e.to_string()))?;
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(false),
        Err(error) => return Err(LaunchError::Extract(error.to_string())),
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let parent = dest.parent().unwrap_or(Path::new("."));
    let mut out = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut file, &mut out)?;
    out.as_file().sync_all()?;
    out.persist(dest).map_err(|error| LaunchError::Io(error.error))?;
    Ok(true)
}

async fn fetch_loader_library(
    client: &Client,
    lib: &super::manifest::Library,
    libs_root: &Path,
    installer_bytes: &[u8],
) -> Result<(), LaunchError> {
    let Some(downloads) = &lib.downloads else {
        return Ok(());
    };
    let Some(artifact) = &downloads.artifact else {
        return Ok(());
    };
    let Some(rel) = artifact
        .path
        .clone()
        .or_else(|| maven_path(&lib.name))
    else {
        return Ok(());
    };
    let dest = libs_root.join(&rel);

    if artifact.url.is_empty() {
        // Re-extract bundled files during repair, even when damaged files
        // already exist. Missing members are processor outputs, validated
        // before publishing the completion receipt.
        extract_from_zip(installer_bytes, &format!("maven/{rel}"), &dest)?;
    } else {
        download_verified(client, &artifact.url, &dest, artifact.sha1.as_deref(), false).await?;
    }
    Ok(())
}

/// Resolve one `data` value to a concrete string per the installer spec:
/// `[maven]` -> library path, `'literal'` -> literal, `/path` -> extracted file.
fn resolve_data_value(
    value: &str,
    libs_root: &Path,
    installer_bytes: &[u8],
    data_dir: &Path,
) -> Result<String, LaunchError> {
    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        let rel = maven_path(inner)
            .ok_or_else(|| LaunchError::Parse(format!("bad maven coord: {inner}")))?;
        Ok(libs_root.join(rel).to_string_lossy().to_string())
    } else if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        Ok(inner.to_string())
    } else if let Some(path) = value.strip_prefix('/') {
        let dest = crate::download::safe_join(data_dir, path)
            .map_err(|e| LaunchError::Parse(e.to_string()))?;
        if !extract_from_zip(installer_bytes, path, &dest)? {
            return Err(LaunchError::Parse(format!("installer has no {path}")));
        }
        Ok(dest.to_string_lossy().to_string())
    } else {
        Ok(value.to_string())
    }
}

async fn run_processor(
    proc: &Processor,
    libs_root: &Path,
    data: &HashMap<String, String>,
    paths: &GamePaths,
    game_version: &str,
    java_path: &str,
    cancel: &CancelToken,
) -> Result<(), LaunchError> {
    let sep = if cfg!(windows) { ";" } else { ":" };

    // Classpath = processor jar + declared classpath libraries.
    let mut cp_entries = Vec::new();
    for coord in std::iter::once(&proc.jar).chain(proc.classpath.iter()) {
        let rel = maven_path(coord)
            .ok_or_else(|| LaunchError::Parse(format!("bad processor coord: {coord}")))?;
        cp_entries.push(libs_root.join(rel).to_string_lossy().to_string());
    }
    let classpath = cp_entries.join(sep);

    let jar_path = libs_root.join(
        maven_path(&proc.jar)
            .ok_or_else(|| LaunchError::Parse(format!("bad processor jar: {}", proc.jar)))?,
    );
    let main_class = main_class_of(&jar_path)?;

    let args: Vec<String> = proc
        .args
        .iter()
        .map(|arg| substitute_processor_arg(arg, data, libs_root, paths, game_version))
        .collect();

    let mut processor_cmd = Command::new(java_path);
    processor_cmd
        .arg("-cp")
        .arg(&classpath)
        .arg(&main_class)
        .args(&args)
        .current_dir(&paths.root);
    #[cfg(windows)]
    processor_cmd.creation_flags(CREATE_NO_WINDOW);
    let stdout_file = tempfile::NamedTempFile::new()?;
    let stderr_file = tempfile::NamedTempFile::new()?;
    processor_cmd
        .stdout(Stdio::from(stdout_file.reopen()?))
        .stderr(Stdio::from(stderr_file.reopen()?))
        .kill_on_drop(true);
    check_cancelled(cancel)?;
    let mut child = processor_cmd.spawn().map_err(|e| LaunchError::Spawn(e.to_string()))?;
    let status = wait_processor(&mut child, cancel).await?;

    if !status.success() {
        let stderr_bytes = std::fs::read(stderr_file.path())?;
        let stdout_bytes = std::fs::read(stdout_file.path())?;
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        let tail: String = stderr
            .lines()
            .chain(stdout.lines())
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .join(" | ");
        return Err(LaunchError::Parse(format!(
            "loader processor {main_class} failed: {tail}"
        )));
    }
    Ok(())
}

pub(super) async fn wait_processor(
    child: &mut tokio::process::Child,
    cancel: &CancelToken,
) -> Result<std::process::ExitStatus, LaunchError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            // kill() waits too, but always call wait() so even a raced exit or
            // kill failure cannot return while the child remains unreaped.
            let _ = child.kill().await;
            // A kill can race natural exit. Successful wait, not kill's return
            // value, proves cancellation may release shared output ownership.
            child.wait().await?;
            Err(LaunchError::Cancelled)
        }
        result = child.wait() => result.map_err(LaunchError::Io),
    }
}

fn substitute_processor_arg(
    arg: &str,
    data: &HashMap<String, String>,
    libs_root: &Path,
    paths: &GamePaths,
    game_version: &str,
) -> String {
    // `{KEY}` -> data value / builtin; `[maven]` -> library path; else literal.
    if let Some(key) = arg.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
        if let Some(value) = data.get(key) {
            return value.clone();
        }
        return match key {
            "SIDE" => "client".to_string(),
            "MINECRAFT_JAR" => paths.client_jar(game_version).to_string_lossy().to_string(),
            "ROOT" => paths.root.to_string_lossy().to_string(),
            "LIBRARY_DIR" => libs_root.to_string_lossy().to_string(),
            other => format!("{{{other}}}"),
        };
    }
    if let Some(inner) = arg.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        if let Some(rel) = maven_path(inner) {
            return libs_root.join(rel).to_string_lossy().to_string();
        }
    }
    arg.to_string()
}

/// Read `Main-Class` from a jar's manifest.
fn main_class_of(jar: &Path) -> Result<String, LaunchError> {
    let bytes = std::fs::read(jar)?;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| LaunchError::Extract(e.to_string()))?;
    let mut manifest = archive
        .by_name("META-INF/MANIFEST.MF")
        .map_err(|_| LaunchError::Parse(format!("no manifest in {}", jar.display())))?;
    let mut text = String::new();
    manifest.read_to_string(&mut text)?;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Main-Class:") {
            return Ok(value.trim().to_string());
        }
    }
    Err(LaunchError::Parse(format!(
        "no Main-Class in {}",
        jar.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use sha1::Digest;

    fn fixture_installer(path: &Path, generated_path: &str, contents: &[u8]) {
        let profile = serde_json::to_vec(&serde_json::json!({
            "id": "forge-fixture", "inheritsFrom": "fixture",
            "mainClass": "example.Main",
            "libraries": [{
                "name": "example:generated:1",
                "downloads": {"artifact": {
                    "path": generated_path, "url": "",
                    "sha1": hex::encode(sha1::Sha1::digest(contents))
                }}
            }]
        })).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, bytes) in [
            ("version.json".to_string(), profile.as_slice()),
            ("install_profile.json".to_string(), &b"{}"[..]),
            (format!("maven/{generated_path}"), contents),
        ] {
            writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    #[tokio::test]
    async fn installed_fast_path_repairs_missing_and_corrupt_generated_artifacts() {
        let scratch = tempfile::tempdir().unwrap();
        let paths = GamePaths::new(scratch.path().join("game"));
        let install_dir = paths.version_dir(&install_id(ModLoader::Forge, "fixture", "1"));
        let relative = "example/generated/1/generated-1.jar";
        let expected = b"generated artifact fixture";
        fixture_installer(&install_dir.join("installer.jar"), relative, expected);
        let vanilla: VersionJson = serde_json::from_value(serde_json::json!({"id": "fixture"})).unwrap();
        let client = Client::builder().proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
            .timeout(std::time::Duration::from_secs(1)).build().unwrap();
        let cancel = CancelToken::new();
        let generated = paths.libraries().join(relative);
        for damage in [None, Some("missing"), Some("corrupt"), Some("legacy-marker"), Some("profile")] {
            match damage {
                Some("missing") => std::fs::remove_file(&generated).unwrap(),
                Some("corrupt") => std::fs::write(&generated, b"corrupt").unwrap(),
                Some("legacy-marker") => std::fs::write(install_dir.join(".installed"), b"ok").unwrap(),
                Some("profile") => std::fs::write(install_dir.join("forge-fixture-1.json"), b"broken JSON").unwrap(),
                _ => {}
            }
            let profile = prepare(&client, &paths, ModLoader::Forge, "fixture", "1", &vanilla, "unused", &cancel, &|_| {}).await.unwrap();
            assert_eq!(profile.main_class.as_deref(), Some("example.Main"));
            assert_eq!(std::fs::read(&generated).unwrap(), expected);
            let receipt: Vec<InstalledArtifact> = serde_json::from_slice(&std::fs::read(install_dir.join(".installed")).unwrap()).unwrap();
            assert_eq!(receipt.len(), 1);
            assert_eq!(receipt[0].path, generated);
        }
    }

    #[test]
    fn processor_output_receipt_requires_present_and_correct_hash() {
        use sha1::Digest;
        let scratch = tempfile::tempdir().unwrap();
        let paths = GamePaths::new(scratch.path().to_owned());
        let output = scratch.path().join("generated.dat");
        let hash = hex::encode(sha1::Sha1::digest(b"expected"));
        let mut data = HashMap::new();
        data.insert("OUTPUT".into(), output.to_string_lossy().to_string());
        data.insert("HASH".into(), hash);
        let install: InstallProfile = serde_json::from_value(serde_json::json!({
            "processors": [{"jar": "unused:processor:1", "outputs": {"{OUTPUT}": "{HASH}"}}]
        })).unwrap();
        let version: VersionJson = serde_json::from_value(serde_json::json!({"id": "fixture"})).unwrap();
        assert!(installed_artifacts(&version, &install, &data, &paths, "fixture").is_err());
        std::fs::write(&output, b"corrupt").unwrap();
        assert!(matches!(installed_artifacts(&version, &install, &data, &paths, "fixture"), Err(LaunchError::HashMismatch { .. })));
        std::fs::write(&output, b"expected").unwrap();
        assert_eq!(installed_artifacts(&version, &install, &data, &paths, "fixture").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancelled_processor_is_killed_and_reaped_before_return() {
        let mut command;
        #[cfg(windows)]
        {
            command = Command::new("powershell.exe");
            command.args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 30"]);
            command.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(not(windows))]
        {
            command = Command::new("sh");
            command.args(["-c", "exec sleep 30"]);
        }
        command.stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        assert!(child.id().is_some());
        let cancel = CancelToken::new();
        cancel.cancel();
        let result = wait_processor(&mut child, &cancel).await;
        assert!(matches!(result, Err(LaunchError::Cancelled)));
        assert!(child.id().is_none(), "cancel must return only after reaping child");
        assert!(child.try_wait().unwrap().is_some());
    }

    fn versions(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn owned(v: &[&str]) -> Vec<String> {
        versions(v)
    }

    #[test]
    fn version_ordering() {
        assert!(newer("47.4.0", "47.2.0"));
        assert!(newer("21.1.100", "21.1.66"));
        assert!(!newer("20.4.190", "21.1.66"));
    }

    #[test]
    fn neoforge_prefixes_legacy_and_year_based() {
        use super::neoforge_prefix_candidates;
        assert_eq!(
            neoforge_prefix_candidates("1.21.1"),
            vec!["21.1.", "1.21.1-"]
        );
        // Missing patch level defaults to the `.0` lineage (real builds exist
        // there: NeoForge 21.0.x targets MC 1.21).
        assert_eq!(neoforge_prefix_candidates("1.21"), vec!["21.0.", "1.21-"]);
        assert_eq!(
            neoforge_prefix_candidates("26.2"),
            vec!["26.2.0.", "26.2.", "26.2-"]
        );
        assert_eq!(
            neoforge_prefix_candidates("26.1"),
            vec!["26.1.0.", "26.1.", "26.1-"]
        );
        assert!(neoforge_prefix_candidates("").is_empty());
    }

    #[test]
    fn picks_newest_stable_within_tier() {
        let pool = versions(&["26.2.0.43-beta", "26.2.0.66", "26.2.0.67"]);
        let prefixes = owned(&["26.2."]);
        assert_eq!(newest_matching(&pool, &prefixes), Some("26.2.0.67".to_string()));
    }

    #[test]
    fn falls_back_to_prerelease_when_only_option() {
        let pool = versions(&["26.2.0.43-beta", "26.2.0.44-beta"]);
        let prefixes = owned(&["26.2."]);
        assert_eq!(
            newest_matching(&pool, &prefixes),
            Some("26.2.0.44-beta".to_string())
        );
    }

    #[test]
    fn more_specific_tier_wins() {
        let pool = versions(&["26.1.0.5", "26.1.2.97"]);
        let prefixes = owned(&["26.1.0.", "26.1."]);
        assert_eq!(
            newest_matching(&pool, &prefixes),
            Some("26.1.0.5".to_string())
        );
    }

    #[test]
    fn prerelease_detection_spans_both_schemes() {
        assert!(is_prerelease("26.2.0.43-beta"));
        assert!(is_prerelease("26.1.0.0-alpha.2+snapshot-1"));
        assert!(!is_prerelease("21.1.248"));
        assert!(!is_prerelease("1.20.1-47.1.11"));
        assert!(!is_prerelease("26.2.0.67"));
    }

    /// Network test: resolve the latest Forge build for 1.20.1, download its
    /// installer, and parse both descriptors. Verifies URLs + JSON shapes
    /// without running processors. `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn resolves_and_parses_forge_installer() {
        let client = crate::download::http_client().unwrap();
        let v = resolve_version(&client, ModLoader::Forge, "1.20.1", None)
            .await
            .unwrap();
        assert!(!v.is_empty(), "no forge version");

        let url = installer_url(ModLoader::Forge, "1.20.1", &v);
        let bytes = client.get(&url).send().await.unwrap().bytes().await.unwrap().to_vec();
        let profile: InstallProfile = read_json_from_zip(&bytes, "install_profile.json").unwrap();
        let version: VersionJson = read_json_from_zip(&bytes, "version.json").unwrap();

        assert!(!profile.processors.is_empty(), "no processors");
        assert!(!profile.libraries.is_empty(), "no installer libraries");
        assert!(version.main_class.is_some(), "no mainClass");
    }

    #[tokio::test]
    #[ignore]
    async fn resolves_neoforge_version() {
        let client = crate::download::http_client().unwrap();
        let v = resolve_version(&client, ModLoader::NeoForge, "1.21.1", None)
            .await
            .unwrap();
        assert!(v.starts_with("21.1."), "unexpected neoforge version: {v}");

        // Year-based scheme: the full game version is embedded.
        let v = resolve_version(&client, ModLoader::NeoForge, "26.2", None)
            .await
            .unwrap();
        assert!(v.starts_with("26.2."), "unexpected neoforge version: {v}");

        // Legacy fork lives under the separate `forge` artifact.
        let v = resolve_version(&client, ModLoader::NeoForge, "1.20.1", None)
            .await
            .unwrap();
        assert!(
            v.starts_with("1.20.1-") || v.starts_with("20.1."),
            "unexpected neoforge version: {v}"
        );
    }
}
