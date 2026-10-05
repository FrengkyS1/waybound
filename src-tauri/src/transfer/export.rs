use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use sha2::Sha512;

use crate::download::{http_client, CancelToken};
use crate::dto::{instance::InstanceSummary, ModLoader};
use super::files::{check_cancel, inventory, read_bounded, reject_links, relative_path, Stage};

const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_TEXT: u64 = 8 * 1024 * 1024;
const MAX_OVERRIDES: u64 = 256 * 1024 * 1024;
const MAX_JSON: u64 = 16 * 1024 * 1024;
const HASH_BATCH: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Hashes {
    sha1: String,
    sha512: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PackFile {
    path: String,
    hashes: Hashes,
    downloads: Vec<String>,
    file_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    env: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize)]
struct Provenance {
    files: Vec<PackFile>,
}

#[derive(Deserialize)]
struct Version {
    files: Vec<VersionFile>,
}

#[derive(Deserialize)]
struct VersionFile {
    hashes: Hashes,
    url: String,
    size: u64,
}

struct LocalFile {
    path: String,
    hashes: Hashes,
    size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Index<'a> {
    format_version: u32,
    game: &'static str,
    version_id: &'a str,
    name: &'a str,
    dependencies: BTreeMap<&'static str, String>,
    files: Vec<PackFile>,
}

fn dependencies(minecraft: &str, loader: ModLoader, version: Option<&str>) -> Result<BTreeMap<&'static str, String>, String> {
    fn pinned(value: &str) -> bool {
        !value.is_empty() && value == value.trim() && !value.chars().any(char::is_whitespace)
            && !value.contains(['*', '<', '>', '=', '^', '~', ',', '|'])
            && !matches!(value.to_ascii_lowercase().as_str(), "latest" | "stable" | "release" | "recommended" | "auto")
    }
    if !pinned(minecraft) { return Err("Export requires an exact Minecraft version.".into()); }
    let mut result = BTreeMap::from([("minecraft", minecraft.to_owned())]);
    let key = match loader {
        ModLoader::Vanilla => return Ok(result),
        ModLoader::Fabric => "fabric-loader",
        ModLoader::Quilt => "quilt-loader",
        ModLoader::Forge => "forge",
        ModLoader::NeoForge => "neoforge",
    };
    let version = version.filter(|value| pinned(value)).ok_or_else(|| format!("Export requires a pinned {key} version. Select an exact loader version in instance settings before exporting."))?;
    result.insert(key, version.to_owned());
    Ok(result)
}

// These are the mrpack specification's permitted download hosts. CurseForge
// URLs are accepted only from exact imported provenance, never constructed.
fn allowed_url(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else { return false; };
    url.scheme() == "https" && url.username().is_empty() && url.password().is_none()
        && url.port_or_known_default() == Some(443) && url.fragment().is_none()
        && matches!(url.host_str(), Some("cdn.modrinth.com" | "github.com" | "raw.githubusercontent.com" | "objects.githubusercontent.com" | "mediafilez.forgecdn.net" | "edge.forgecdn.net"))
}

fn hashes_match(local: &LocalFile, hashes: &Hashes, size: u64) -> bool {
    size == local.size && hashes.sha1.eq_ignore_ascii_case(&local.hashes.sha1)
        && hashes.sha512.eq_ignore_ascii_case(&local.hashes.sha512)
}

fn from_provenance(local: &LocalFile, entry: &PackFile) -> Option<PackFile> {
    if entry.path != local.path || !hashes_match(local, &entry.hashes, entry.file_size) { return None; }
    let downloads: Vec<_> = entry.downloads.iter().filter(|url| allowed_url(url)).cloned().collect();
    if downloads.is_empty() { return None; }
    // The local file exists and is therefore part of this exported installation;
    // do not retain a stale client=unsupported/optional flag from its old pack.
    Some(PackFile { path: local.path.clone(), hashes: local.hashes.clone(), downloads, file_size: local.size, env: None })
}

fn from_version(local: &LocalFile, version: &Version) -> Option<PackFile> {
    let file = version.files.iter().find(|file| hashes_match(local, &file.hashes, file.size) && allowed_url(&file.url))?;
    Some(PackFile { path: local.path.clone(), hashes: local.hashes.clone(), downloads: vec![file.url.clone()], file_size: local.size, env: None })
}

fn launcher_metadata(path: &str) -> bool {
    let name = path.to_ascii_lowercase();
    let top = name.split('/').next().unwrap_or("");
    matches!(top, "modrinth.index.json" | "modrinth.json" | "profile.json" | "instance.json" | "instance.json.bak" | "pack.toml" | "pack.index.json" | "installation.json" | "minecraftinstance.json" | "launcher_log.txt" | "launcher_log.txt.0" | "hs_err_pid.log")
        || top.starts_with("launcher_") || top.starts_with("hs_err_pid") || top.starts_with("replay_pid")
}

// Only known game customization locations can become bundled overrides.
// Text under mods/resourcepacks/shaderpacks still requires verified provenance.
fn text_override_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let parts: Vec<_> = path.split('/').collect();
    if parts.iter().any(|part| part.contains("password") || part.contains("passwd") || part.contains("private")
        || part.contains("auth") || part.contains("session") || part.contains("webhook") || part.ends_with(".env")) { return false; }
    let top = parts[0];
    let allowed_root = parts.len() > 1 && matches!(top, "config" | "defaultconfigs" | "scripts" | "kubejs" | "crafttweaker" | "openloader" | "global_packs");
    if !allowed_root && !matches!(path.as_str(), "options.txt" | "optionsof.txt" | "optionsshaders.txt") { return false; }
    matches!(Path::new(&path).extension().and_then(|v| v.to_str()), Some("json" | "json5" | "toml" | "cfg" | "conf" | "properties" | "yaml" | "yml" | "txt" | "xml" | "snbt" | "mcmeta" | "js" | "zs" | "groovy" | "lua" | "csv"))
}

fn safe_string(text: &str) -> bool {
    if text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) { return false; }
    // Normalize separators so access_token, accessToken and access-token agree.
    // Never return matched keys, source lines, or values to callers.
    let folded: String = text.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect();
    !["token", "password", "passwd", "secret", "credential", "apikey", "authorization", "bearer", "privatekey", "webhook", "sessionid", "clientkey"].iter().any(|needle| folded.contains(needle))
        && !text.contains("://") // URLs can embed credentials, signed queries, or private endpoints.
}

// Check JSON while decoding, rather than first collecting a map: duplicate
// keys must not discard an earlier credential-bearing value.
struct ShareableJson(bool);

impl<'de> Deserialize<'de> for ShareableJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ShareableJson;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a JSON value")
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> { Ok(ShareableJson(true)) }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> { Ok(ShareableJson(true)) }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> { Ok(ShareableJson(true)) }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> { Ok(ShareableJson(true)) }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> { Ok(ShareableJson(true)) }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> { Ok(ShareableJson(safe_string(value))) }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
                let mut safe = true;
                while let Some(value) = sequence.next_element::<ShareableJson>()? { safe &= value.0; }
                Ok(ShareableJson(safe))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut safe = true;
                while let Some(key) = map.next_key::<String>()? {
                    safe &= safe_string(&key);
                    safe &= map.next_value::<ShareableJson>()?.0;
                }
                Ok(ShareableJson(safe))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

fn safe_toml(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(value) => safe_string(value),
        toml::Value::Array(values) => values.iter().all(safe_toml),
        toml::Value::Table(table) => table.iter().all(|(key, value)| safe_string(key) && safe_toml(value)),
        _ => true,
    }
}

fn safe_text(path: &str, bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else { return false; };
    // Parsed TOML omits comments. Inspect source too, so comments containing
    // plaintext credentials or private URLs cannot slip into shared overrides.
    if !safe_string(text) { return false; }
    let extension = Path::new(path).extension().and_then(|value| value.to_str()).unwrap_or("");
    if extension.eq_ignore_ascii_case("json") || extension.eq_ignore_ascii_case("mcmeta") {
        serde_json::from_slice::<ShareableJson>(bytes).is_ok_and(|value| value.0)
    } else if extension.eq_ignore_ascii_case("toml") {
        toml::from_str::<toml::Value>(text).is_ok_and(|value| safe_toml(&value))
    } else {
        // Arbitrary languages cannot be proven secret-free. This is only a
        // conservative lexical guard, not semantic parsing; refuse all escape
        // sequences rather than pretending unsupported decoding was checked.
        !text.contains('\\')
            && !(extension.eq_ignore_ascii_case("xml") && text.contains('&'))
    }
}

fn hash_file(path: &Path, cancel: &CancelToken) -> Result<(Hashes, u64), String> {
    reject_links(path)?;
    let mut input = File::open(path).map_err(|e| e.to_string())?;
    let mut sha1 = Sha1::new();
    let mut sha512 = Sha512::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0;
    loop {
        check_cancel(cancel)?;
        let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 { break; }
        size += count as u64;
        if size > MAX_FILE { return Err("Source changed beyond the export size limit.".into()); }
        sha1.update(&buffer[..count]);
        sha512.update(&buffer[..count]);
    }
    Ok((Hashes { sha1: hex::encode(sha1.finalize()), sha512: hex::encode(sha512.finalize()) }, size))
}

fn output_path(root: &Path, destination: &Path, name: &str) -> Result<PathBuf, String> {
    let target = if destination.is_dir() {
        let sanitized: String = name.chars().take(100).map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' }).collect();
        let stem = sanitized.trim_matches('_');
        // Prefix avoids Windows reserved device names without discarding pack name.
        destination.join(format!("pack-{}.mrpack", if stem.is_empty() { "instance" } else { stem }))
    } else {
        if !destination.extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("mrpack")) {
            return Err("Export filename must end in .mrpack, or select an existing destination folder.".into());
        }
        destination.to_owned()
    };
    let filename = target.file_name().and_then(|v| v.to_str()).ok_or("Invalid export filename.")?;
    relative_path(filename)?;
    let parent = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    reject_links(parent)?;
    let parent = fs::canonicalize(parent).map_err(|e| format!("Export destination folder must already exist: {e}"))?;
    if parent.starts_with(root) { return Err("Export destination must be outside the instance folder.".into()); }
    let target = parent.join(filename);
    match fs::symlink_metadata(&target) {
        Ok(_) => return Err("Export destination already exists. Choose a new filename; existing files are never overwritten.".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e.to_string()),
    }
    Ok(target)
}

async fn lookup(client: &reqwest::Client, files: &[&LocalFile], cancel: &CancelToken) -> Result<HashMap<String, Version>, String> {
    check_cancel(cancel)?;
    let hashes: Vec<_> = files.iter().map(|file| &file.hashes.sha1).collect();
    let response = client.post("https://api.modrinth.com/v2/version_files")
        .timeout(Duration::from_secs(45))
        .json(&serde_json::json!({ "hashes": hashes, "algorithm": "sha1" }))
        .send().await.map_err(|_| "Modrinth hash lookup failed. Check your connection and retry export.".to_string())?
        .error_for_status().map_err(|e| format!("Modrinth hash lookup failed ({}). Retry later.", e.status().map(|s| s.as_u16()).unwrap_or(0)))?;
    if response.content_length().is_some_and(|n| n > MAX_JSON) { return Err("Modrinth hash response exceeds size limit.".into()); }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        check_cancel(cancel)?;
        let chunk = chunk.map_err(|_| "Modrinth hash response was interrupted. Retry export.".to_string())?;
        if bytes.len() as u64 + chunk.len() as u64 > MAX_JSON { return Err("Modrinth hash response exceeds size limit.".into()); }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Modrinth returned an invalid hash response.".into())
}

/// Export exact installed files without modifying the instance. The caller owns
/// the instance operations lock. Binary redistribution requires a verified URL.
pub(super) async fn export(instance: &InstanceSummary, destination: &Path, cancel: &CancelToken) -> Result<String, String> {
    check_cancel(cancel)?;
    let dependencies = dependencies(&instance.minecraft_version, instance.loader, instance.loader_version.as_deref())?;
    let root = Path::new(&instance.root_path);
    reject_links(root)?;
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let target = output_path(&root, destination, &instance.name)?;
    let parent = target.parent().ok_or("Export destination has no parent folder.")?;
    let stage = Stage::new(parent)?;
    let sidecar = root.join(".waybound-transfer-index.json");
    let provenance: HashMap<String, PackFile> = match fs::symlink_metadata(&sidecar) {
        Ok(_) => {
            let parsed: Provenance = serde_json::from_slice(&read_bounded(&sidecar, MAX_JSON)?).map_err(|_| "Imported transfer provenance is invalid. Re-import the original pack before exporting.".to_string())?;
            if parsed.files.len() > 50_000 { return Err("Imported transfer provenance exceeds file limit.".into()); }
            let mut entries = HashMap::new();
            for entry in parsed.files {
                relative_path(&entry.path)?;
                if entries.insert(entry.path.clone(), entry).is_some() { return Err("Imported transfer provenance contains duplicate paths.".into()); }
            }
            entries
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => return Err(e.to_string()),
    };
    let mut local = Vec::new();
    let mut overrides = Vec::new();
    let mut override_size = 0;
    let mut seen = HashSet::new();
    for relative in inventory(&root, true, cancel)? {
        check_cancel(cancel)?;
        let path = relative.to_str().ok_or("Non-Unicode export path.")?.replace('\\', "/");
        if launcher_metadata(&path) { continue; }
        if !seen.insert(path.to_ascii_lowercase()) { return Err(format!("Case-colliding export path: {path}")); }
        let source = root.join(&relative);
        if text_override_path(&path) {
            let bytes = read_bounded(&source, MAX_TEXT).map_err(|_| format!("Cannot export override {path}: unreadable or larger than 8 MiB."))?;
            if !safe_text(&path, &bytes) { return Err(format!("Cannot export {path}: config/script may contain credentials, private URLs, invalid structured data, non-text data, or escapes in a format without a supported decoder. JSON/TOML keys and values are decoded; other formats receive conservative text checks only. Review private settings in a separate shareable copy before exporting.")); }
            override_size += bytes.len() as u64;
            if override_size > MAX_OVERRIDES { return Err("Text overrides exceed the 256 MiB export limit.".into()); }
            let copy = stage.0.join(&relative);
            fs::create_dir_all(copy.parent().ok_or("Invalid override path.")?).map_err(|e| e.to_string())?;
            fs::write(copy, bytes).map_err(|e| e.to_string())?;
            overrides.push(path);
        } else {
            let (hashes, size) = hash_file(&source, cancel)?;
            local.push(LocalFile { path, hashes, size });
        }
    }
    let mut files = Vec::new();
    let mut pending = Vec::new();
    for file in &local {
        if let Some(entry) = provenance.get(&file.path).and_then(|entry| from_provenance(file, entry)) { files.push(entry); }
        else { pending.push(file); }
    }
    let mut unresolved = Vec::new();
    if !pending.is_empty() {
        let client = http_client().map_err(|e| e.to_string())?;
        for batch in pending.chunks(HASH_BATCH) {
            let versions = lookup(&client, batch, cancel).await?;
            for file in batch {
                if let Some(entry) = versions.get(&file.hashes.sha1).and_then(|version| from_version(file, version)) { files.push(entry); }
                else { unresolved.push(file.path.as_str()); }
            }
        }
    }
    if !unresolved.is_empty() {
        return Err(format!("Cannot export files without exact Modrinth hashes and an approved download URL or verified imported provenance:\n{}\nReplace these with distributable Modrinth versions, re-import their original mrpack, or remove them from a separate shareable copy. Unresolved binaries are never bundled or silently omitted.", unresolved.join("\n")));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let index = Index {
        format_version: 1, game: "minecraft",
        version_id: instance.modpack_version_label.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or("1.0.0"),
        name: &instance.name, dependencies, files,
    };
    let index = serde_json::to_vec(&index).map_err(|e| e.to_string())?;
    if index.len() as u64 > MAX_JSON { return Err("Export index exceeds 16 MiB size limit.".into()); }
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    {
        let mut zip = zip::ZipWriter::new(output.as_file_mut());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).unix_permissions(0o644);
        zip.start_file("modrinth.index.json", options).map_err(|e| e.to_string())?;
        zip.write_all(&index).map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 64 * 1024];
        for path in &overrides {
            check_cancel(cancel)?;
            zip.start_file(format!("overrides/{path}"), options).map_err(|e| e.to_string())?;
            let mut input = File::open(stage.0.join(relative_path(path)?)).map_err(|e| e.to_string())?;
            loop {
                check_cancel(cancel)?;
                let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
                if count == 0 { break; }
                zip.write_all(&buffer[..count]).map_err(|e| e.to_string())?;
            }
        }
        zip.finish().map_err(|e| e.to_string())?;
    }
    // Network lookup can outlive an external editor or game process. Refuse a
    // mixed snapshot rather than silently describing bytes no longer installed.
    for file in &local {
        let (hashes, size) = hash_file(&root.join(relative_path(&file.path)?), cancel)?;
        if !hashes_match(file, &hashes, size) { return Err(format!("Source changed during export: {}. Close the game and retry.", file.path)); }
    }
    check_cancel(cancel)?;
    output.as_file().sync_all().map_err(|e| e.to_string())?;
    output.persist_noclobber(&target).map_err(|e| format!("Could not publish export without overwriting an existing file: {}", e.error))?;
    Ok(target.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependencies_keep_exact_loader_versions_and_use_mrpack_keys() {
        for (loader, key, version) in [
            (ModLoader::Fabric, "fabric-loader", "0.16.9"),
            (ModLoader::Quilt, "quilt-loader", "0.27.1"),
            (ModLoader::Forge, "forge", "47.3.0"),
            (ModLoader::NeoForge, "neoforge", "21.1.80"),
        ] {
            assert_eq!(dependencies("1.21.1", loader, Some(version)).unwrap(), BTreeMap::from([("minecraft", "1.21.1".into()), (key, version.into())]));
            assert!(dependencies("1.21.1", loader, None).is_err());
            assert!(dependencies("1.21.1", loader, Some("latest")).is_err());
        }
        assert_eq!(dependencies("24w14a", ModLoader::Vanilla, None).unwrap(), BTreeMap::from([("minecraft", "24w14a".into())]));
        assert!(dependencies("1.*", ModLoader::Vanilla, None).is_err());
        assert!(dependencies("1.21.1", ModLoader::Fabric, Some(">=0.16")).is_err());
    }

    #[test]
    fn unresolved_files_cannot_become_overrides_by_extension_alone() {
        assert!(text_override_path("kubejs/server_scripts/recipes.js"));
        assert!(text_override_path("config/example.toml"));
        assert!(!text_override_path("mods/claimed-license.txt"));
        assert!(!text_override_path("resourcepacks/custom/pack.mcmeta"));
        assert!(!text_override_path("config/native.dll"));
        assert!(!text_override_path("documents/personal.txt"));
        assert!(!text_override_path("config/auth/settings.json"));
        assert!(launcher_metadata("launcher_profiles.json"));
        assert!(launcher_metadata("modrinth.index.json"));
    }

    #[test]
    fn suspicious_config_is_refused_without_requiring_a_specific_format() {
        assert!(safe_text("config/example.json", b"{\"difficulty\": 3, \"enabled\": true}"));
        assert!(safe_text("scripts/recipes.js", b"ServerEvents.recipes(event => { event.remove({id: 'mod:recipe'}); });"));
        for content in [b"access_token = 'example'".as_slice(), b"{\"apiKey\":\"example\"}", b"PASSWORD: example", b"endpoint=https://user:pass@example.test", b"\xff\x00"] {
            assert!(!safe_text("config/example.cfg", content));
        }
    }

    #[test]
    fn structured_credentials_are_checked_after_unicode_decoding() {
        for json in [
            r#"{"\u0074oken": "sensitive-value"}"#,
            r#"{"\u0070ass\u0077ord": "sensitive-value"}"#,
            r#"{"setting": "\u0070ass\u0077ord sensitive-value"}"#,
            r#"{"nested": [{"api\u004bey": "sensitive-value"}]}"#,
            r#"{"setting": "https\u003a//user:pass@example.test"}"#,
            r#"{"setting": "\u0062earer sensitive-value", "setting": false}"#,
        ] {
            assert!(!safe_text("config/example.JSON", json.as_bytes()));
        }
        for toml in [
            r#""\u0074oken" = "sensitive-value""#,
            r#""\u0070ass\u0077ord" = "sensitive-value""#,
            r#"setting = "\u0070ass\u0077ord sensitive-value""#,
            "[nested]\n\"api\\u004bey\" = \"sensitive-value\"",
            r#"setting = "https\u003a//user:pass@example.test""#,
            r#"setting = "\u0062earer sensitive-value""#,
        ] {
            assert!(!safe_text("config/example.TOML", toml.as_bytes()));
        }
        assert!(safe_text("config/example.json", br#"{"\u0065nabled": true, "list": [null, 1, 2.5, "recipe"]}"#));
        assert!(safe_text("config/example.toml", b"[general]\nenabled = true\nitems = [1, 2]\n"));
        assert!(!safe_text("config/example.json", b"{bad json"));
        assert!(!safe_text("config/example.toml", b"enabled = ["));
        assert!(!safe_text("scripts/example.js", br#"settings["\u0074oken"] = "sensitive-value";"#));
        assert!(!safe_text("config/example.toml", b"enabled = true\n# password = private-value\n"));
        assert!(!safe_text("config/example.toml", b"enabled = true\n# https://private.example.test\n"));
        assert!(!safe_text("config/example.xml", br#"<pass&#x77;ord>sensitive-value</pass&#x77;ord>"#));
        for content in [
            r#"setting = "\U00000070assword""#,
            r#"setting = "\160assword""#,
            r#"setting = "\N{LATIN SMALL LETTER P}assword""#,
        ] {
            assert!(!safe_text("scripts/example.lua", content.as_bytes()));
        }
    }

    #[tokio::test]
    async fn escaped_credentials_abort_export_without_echo_or_publication() {
        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let config = source.path().join("config");
        fs::create_dir(&config).unwrap();
        let instance = InstanceSummary {
            id: "private-export-test".into(), name: "Private export".into(),
            minecraft_version: "1.21.1".into(), loader: ModLoader::Vanilla,
            loader_version: None, mod_count: 0, created_at: 0,
            root_path: source.path().to_string_lossy().into_owned(), icon: None,
            last_played: None, total_play_seconds: 0,
            modpack_version_label: None, modpack_project_uid: None,
        };
        for (filename, contents) in [
            ("example.json", r#"{"\u0074oken": "credential-value-that-must-stay-private"}"#),
            ("password.json", r#"{"\u0070ass\u0077ord": "credential-value-that-must-stay-private"}"#),
            ("example.toml", r#""\u0074oken" = "credential-value-that-must-stay-private""#),
            ("password.toml", r#""\u0070ass\u0077ord" = "credential-value-that-must-stay-private""#),
        ] {
            let file = config.join(filename);
            fs::write(&file, contents).unwrap();
            let target = output.path().join("shared.mrpack");
            let error = export(&instance, &target, &CancelToken::new()).await.unwrap_err();
            assert!(error.contains("Cannot export"));
            assert!(!error.contains("credential-value-that-must-stay-private"));
            assert!(!error.contains(contents));
            assert!(!target.exists());
            assert_eq!(fs::read_to_string(&file).unwrap(), contents);
            fs::remove_file(file).unwrap();
        }
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }

    fn local() -> LocalFile {
        LocalFile { path: "mods/example.jar".into(), hashes: Hashes { sha1: "a".repeat(40), sha512: "b".repeat(128) }, size: 42 }
    }

    #[test]
    fn provenance_requires_path_both_hashes_size_and_approved_url() {
        let local = local();
        let mut entry = PackFile { path: local.path.clone(), hashes: local.hashes.clone(), file_size: 42, downloads: vec!["https://cdn.modrinth.com/data/example.jar".into()], env: None };
        assert!(from_provenance(&local, &entry).is_some());
        entry.hashes.sha512 = "c".repeat(128);
        assert!(from_provenance(&local, &entry).is_none());
        entry.hashes = local.hashes.clone();
        entry.file_size += 1;
        assert!(from_provenance(&local, &entry).is_none());
        entry.file_size = local.size;
        entry.path = "mods/other.jar".into();
        assert!(from_provenance(&local, &entry).is_none());
        entry.path = local.path.clone();
        entry.downloads = vec!["https://example.com/example.jar".into()];
        assert!(from_provenance(&local, &entry).is_none());
    }

    #[test]
    fn hash_lookup_selects_exact_file_not_primary_or_similar_version() {
        let local = local();
        let mut version = Version { files: vec![VersionFile { hashes: local.hashes.clone(), size: local.size + 1, url: "https://cdn.modrinth.com/data/wrong.jar".into() }] };
        assert!(from_version(&local, &version).is_none());
        version.files.push(VersionFile { hashes: local.hashes.clone(), size: local.size, url: "https://cdn.modrinth.com/data/exact.jar".into() });
        assert_eq!(from_version(&local, &version).unwrap().downloads, ["https://cdn.modrinth.com/data/exact.jar"]);
    }

    #[test]
    fn download_hosts_reject_credentials_cleartext_and_lookalikes() {
        assert!(allowed_url("https://cdn.modrinth.com/data/file.jar"));
        assert!(allowed_url("https://edge.forgecdn.net/files/1/2/file.jar"));
        for url in ["http://cdn.modrinth.com/file", "https://cdn.modrinth.com.evil.test/file", "https://user:pass@cdn.modrinth.com/file", "https://cdn.modrinth.com:444/file", "https://example.com/file"] {
            assert!(!allowed_url(url));
        }
    }
}
