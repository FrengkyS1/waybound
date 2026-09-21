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
use std::path::Path;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use reqwest::Client;
use serde::Deserialize;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

use crate::dto::ModLoader;

use super::files::{download_verified, maven_path, GamePaths};
use super::manifest::VersionJson;
use super::{LaunchError, ProgressUpdate};

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
    report: &F,
) -> Result<VersionJson, LaunchError>
where
    F: Fn(ProgressUpdate),
{
    let id = install_id(loader, game_version, loader_version);
    let install_dir = paths.version_dir(&id);
    let version_json_path = install_dir.join(format!("{id}.json"));
    let marker = install_dir.join(".installed");

    // Fast path: already installed.
    if marker.exists() && version_json_path.exists() {
        let raw = std::fs::read(&version_json_path)?;
        let profile: VersionJson = serde_json::from_slice(&raw)
            .map_err(|e| LaunchError::Parse(format!("cached loader version: {e}")))?;
        if profile.inherits_from.as_deref() != Some(game_version) {
            return Err(LaunchError::Parse(format!("Installed loader does not match Minecraft {game_version}")));
        }
        return Ok(super::fabric::merge_onto_parent(profile, vanilla.clone()));
    }

    report(ProgressUpdate::stage("Downloading loader installer", 0, 1));
    std::fs::create_dir_all(&install_dir)?;

    // download_verified re-downloads only when the jar is absent or unreadable,
    // recovering interrupted preparations without touching the live install.
    let url = installer_url(loader, game_version, loader_version);
    let installer_path = install_dir.join("installer.jar");
    download_verified(client, &url, &installer_path, None, false).await?;
    let installer_bytes = std::fs::read(&installer_path)?;

    // Extract the two JSON descriptors.
    let install_profile: InstallProfile =
        read_json_from_zip(&installer_bytes, "install_profile.json")?;
    let version_profile: VersionJson = read_json_from_zip(&installer_bytes, "version.json")?;

    // Ensure the vanilla client jar exists (processors patch it).
    if let Some(downloads) = &vanilla.downloads {
        if let Some(client_dl) = &downloads.client {
            let dest = paths.client_jar(game_version);
            download_verified(client, &client_dl.url, &dest, client_dl.sha1.as_deref(), false).await?;
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
        fetch_loader_library(client, lib, &libs_root, &installer_bytes).await?;
    }

    // Resolve `data` entries (extract bundled files, resolve maven refs).
    let data_dir = install_dir.join("data");
    let mut data: HashMap<String, String> = HashMap::new();
    for (key, sided) in &install_profile.data {
        let resolved = resolve_data_value(&sided.client, &libs_root, &installer_bytes, &data_dir)?;
        data.insert(key.clone(), resolved);
    }

    // Run each client-side processor in order. Each one is a real subprocess
    // (deobfuscation/patching) that can take several seconds, with several
    // processors per install — running them via spawn_blocking gives a
    // genuine yield point between processors so a cancelled launch actually
    // stops between them instead of only after every processor has finished,
    // and keeps this blocking work off the async runtime's worker threads.
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
        let proc = proc.clone();
        let libs_root = libs_root.clone();
        let data = data.clone();
        let paths = paths.clone();
        let game_version = game_version.to_string();
        let java_path = java_path.to_string();
        tokio::task::spawn_blocking(move || {
            run_processor(&proc, &libs_root, &data, &paths, &game_version, &java_path)
        })
        .await
        .map_err(|e| LaunchError::Spawn(format!("processor task panicked: {e}")))??;
    }
    let raw = raw_version_json(&installer_bytes)?;
    std::fs::write(&version_json_path, serde_json::to_vec_pretty(&raw)
        .map_err(|e| LaunchError::Parse(e.to_string()))?)?;
    std::fs::write(&marker, b"ok")?;

    Ok(super::fabric::merge_onto_parent(version_profile, vanilla.clone()))
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
fn extract_from_zip(zip_bytes: &[u8], name: &str, dest: &Path) -> Result<(), LaunchError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| LaunchError::Extract(e.to_string()))?;
    let mut file = archive
        .by_name(name)
        .map_err(|_| LaunchError::Parse(format!("installer has no {name}")))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = std::fs::File::create(dest)?;
    std::io::copy(&mut file, &mut out)?;
    Ok(())
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
        // Empty URL means either bundled inside the installer under
        // `maven/<path>`, or a processor *output* produced later. Extract it if
        // present; if not, skip silently (a processor will create it).
        if !dest.exists() {
            let _ = extract_from_zip(installer_bytes, &format!("maven/{rel}"), &dest);
        }
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
        extract_from_zip(installer_bytes, path, &dest)?;
        Ok(dest.to_string_lossy().to_string())
    } else {
        Ok(value.to_string())
    }
}

fn run_processor(
    proc: &Processor,
    libs_root: &Path,
    data: &HashMap<String, String>,
    paths: &GamePaths,
    game_version: &str,
    java_path: &str,
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
    let output = processor_cmd
        .output()
        .map_err(|e| LaunchError::Spawn(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
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
