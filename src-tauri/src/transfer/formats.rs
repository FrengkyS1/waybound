use std::path::{Path, PathBuf};
use serde_json::Value;
use crate::dto::ModLoader;
use super::files::{read_bounded, reject_links};

#[derive(Debug)]
pub(super) struct Metadata {
    pub name: String,
    pub minecraft: String,
    pub loader: ModLoader,
    pub loader_version: Option<String>,
}

fn version(value: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > 128 || !value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+')) {
        return Err("Missing or unsupported game/loader version. Re-export this profile with a pinned version.".into());
    }
    Ok(value.to_owned())
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| format!("Missing {key} in launcher metadata."))
}
fn set_loader(meta: &mut Metadata, loader: ModLoader, build: &str) -> Result<(), String> {
    if meta.loader != ModLoader::Vanilla { return Err("Multiple active loaders are unsupported. Export a profile with one loader.".into()); }
    meta.loader = loader;
    meta.loader_version = Some(version(build)?);
    Ok(())
}
pub(super) fn json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&read_bounded(path, 4 * 1024 * 1024)?).map_err(|e| format!("Invalid launcher metadata: {e}"))
}

pub(super) fn mrpack(index: &Value) -> Result<Metadata, String> {
    if index.get("formatVersion").and_then(Value::as_u64) != Some(1) || index.get("game").and_then(Value::as_str) != Some("minecraft") {
        return Err("Unsupported Modrinth pack format. Expected Minecraft formatVersion 1.".into());
    }
    let deps = index.get("dependencies").and_then(Value::as_object).ok_or("Missing mrpack dependencies.")?;
    let mut meta = Metadata { name: required(index, "name")?.to_owned(), minecraft: version(deps.get("minecraft").and_then(Value::as_str).ok_or("Missing Minecraft dependency.")?)?, loader: ModLoader::Vanilla, loader_version: None };
    for (key, value) in deps {
        let loader = match key.as_str() {
            "minecraft" => continue,
            "fabric-loader" => ModLoader::Fabric,
            "quilt-loader" => ModLoader::Quilt,
            "forge" => ModLoader::Forge,
            "neoforge" => ModLoader::NeoForge,
            _ => return Err(format!("Unsupported mrpack dependency {key}.")),
        };
        set_loader(&mut meta, loader, value.as_str().ok_or("Invalid loader version.")?)?;
    }
    Ok(meta)
}

pub(super) fn curseforge(manifest: &Value) -> Result<Metadata, String> {
    if manifest.get("manifestType").and_then(Value::as_str) != Some("minecraftModpack") || manifest.get("manifestVersion").and_then(Value::as_u64) != Some(1) {
        return Err("Unsupported CurseForge manifest. Export a Minecraft modpack ZIP (manifestVersion 1).".into());
    }
    let minecraft = manifest.get("minecraft").ok_or("Missing CurseForge Minecraft metadata.")?;
    let mut meta = Metadata { name: required(manifest, "name")?.to_owned(), minecraft: version(required(minecraft, "version")?)?, loader: ModLoader::Vanilla, loader_version: None };
    let loaders = minecraft.get("modLoaders").and_then(Value::as_array).ok_or("Missing CurseForge modLoaders.")?;
    for loader in loaders {
        let id = required(loader, "id")?;
        let (kind, build) = id.split_once('-').ok_or("Invalid CurseForge loader ID.")?;
        let kind = match kind { "forge" => ModLoader::Forge, "neoforge" => ModLoader::NeoForge, "fabric" => ModLoader::Fabric, "quilt" => ModLoader::Quilt, _ => return Err(format!("Unsupported CurseForge loader {kind}.")) };
        set_loader(&mut meta, kind, build)?;
    }
    Ok(meta)
}

/// Prism/MultiMC format is defined by PackProfile.cpp's componentFromJsonV1.
/// Cached versions are not pins; custom component patches need their original launcher.
pub(super) fn prism(pack: &Value, cfg: &str) -> Result<Metadata, String> {
    if pack.get("formatVersion").and_then(Value::as_u64) != Some(1) { return Err("Unsupported Prism/MultiMC pack version. Export a current instance ZIP.".into()); }
    let name = cfg.lines().find_map(|line| line.strip_prefix("name=")).filter(|s| !s.trim().is_empty()).ok_or("Missing name in instance.cfg.")?;
    let mut meta = Metadata { name: name.to_owned(), minecraft: String::new(), loader: ModLoader::Vanilla, loader_version: None };
    for component in pack.get("components").and_then(Value::as_array).ok_or("Missing Prism components.")? {
        if component.get("disabled").and_then(Value::as_bool) == Some(true) { continue; }
        let uid = required(component, "uid")?;
        match uid {
            "net.minecraft" => {
                if !meta.minecraft.is_empty() { return Err("Duplicate Minecraft components.".into()); }
                meta.minecraft = version(required(component, "version")?)?;
            }
            "org.lwjgl" | "org.lwjgl3" => {},
            "net.fabricmc.fabric-loader" => set_loader(&mut meta, ModLoader::Fabric, required(component, "version")?)?,
            "org.quiltmc.quilt-loader" => set_loader(&mut meta, ModLoader::Quilt, required(component, "version")?)?,
            "net.minecraftforge" => set_loader(&mut meta, ModLoader::Forge, required(component, "version")?)?,
            "net.neoforged" => set_loader(&mut meta, ModLoader::NeoForge, required(component, "version")?)?,
            _ => return Err(format!("Custom Prism component {uid} is unsupported. Use a standard loader profile or export .mrpack.")),
        }
    }
    version(&meta.minecraft)?;
    Ok(meta)
}

// ATLauncher and legacy GDLauncher keep game files beside their metadata,
// not under .minecraft. Keep these adapters shared by discovery and import.
fn launcher_loader(meta: &mut Metadata, kind: &str, build: Option<&str>) -> Result<(), String> {
    let loader = match kind.to_ascii_lowercase().as_str() {
        "vanilla" => ModLoader::Vanilla,
        "fabric" => ModLoader::Fabric,
        "forge" => ModLoader::Forge,
        "neoforge" => ModLoader::NeoForge,
        "quilt" => ModLoader::Quilt,
        _ => return Err(format!("Unsupported launcher loader {kind}.")),
    };
    if loader != ModLoader::Vanilla {
        set_loader(meta, loader, build.ok_or("Missing pinned loader version.")?)?;
    }
    Ok(())
}

fn atlauncher(value: &Value) -> Result<Metadata, String> {
    let launcher = value.get("launcher").ok_or("Missing ATLauncher metadata.")?;
    let mut meta = Metadata {
        name: required(launcher, "name")?.to_owned(),
        minecraft: version(required(value, "id")?)?,
        loader: ModLoader::Vanilla,
        loader_version: None,
    };
    let loader = launcher.get("loaderVersion").ok_or("Missing ATLauncher loader metadata.")?;
    launcher_loader(&mut meta, required(loader, "type")?, loader.get("version").and_then(Value::as_str))?;
    Ok(meta)
}

fn gdlauncher(value: &Value, root: &Path) -> Result<Metadata, String> {
    let loader = value.get("loader").ok_or("Missing legacy GDLauncher loader metadata.")?;
    let name = loader.get("sourceName").and_then(Value::as_str).filter(|name| !name.trim().is_empty())
        .or_else(|| root.file_name().and_then(|name| name.to_str())).ok_or("Missing GDLauncher instance name.")?;
    let mut meta = Metadata {
        name: name.to_owned(),
        minecraft: version(required(loader, "mcVersion")?)?,
        loader: ModLoader::Vanilla,
        loader_version: None,
    };
    launcher_loader(&mut meta, required(loader, "loaderType")?, loader.get("loaderVersion").and_then(Value::as_str))?;
    Ok(meta)
}

// FTB App instance folder: `instance.json` carrying uuid/id/versionId/name/
// version/mcVersion/modLoader ("forge-47.3.0") — same filename ATLauncher
// uses, told apart by the FTB-only `uuid` + `mcVersion` keys (see
// `directory`). Legacy loader falls back to `.ftbapp/version.json` (else
// `version.json`) `targets[]`, mirroring Prism's PackHelpers.
fn ftb(value: &Value, root: &Path) -> Result<Metadata, String> {
    let name = required(value, "name")?;
    let minecraft = version(required(value, "mcVersion")?)?;
    let mut meta = Metadata { name: name.to_owned(), minecraft, loader: ModLoader::Vanilla, loader_version: None };
    if let Some(id) = value.get("modLoader").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let (kind, build) = id.split_once('-').ok_or("Invalid FTB modLoader ID.")?;
        let loader = match kind.to_ascii_lowercase().as_str() {
            "forge" => ModLoader::Forge,
            "neoforge" => ModLoader::NeoForge,
            "fabric" => ModLoader::Fabric,
            "quilt" => ModLoader::Quilt,
            _ => return Err(format!("Unsupported FTB loader {kind}.")),
        };
        set_loader(&mut meta, loader, build)?;
        return Ok(meta);
    }
    let targets_path = [root.join(".ftbapp/version.json"), root.join("version.json")]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or("FTB instance has no modLoader and no version.json targets.")?;
    let targets_doc = json(&targets_path)?;
    let targets = targets_doc.get("targets").and_then(Value::as_array).ok_or("Missing FTB targets.")?;
    for target in targets {
        let tname = target.get("name").and_then(Value::as_str).unwrap_or("");
        let tver = target.get("version").and_then(Value::as_str).unwrap_or("");
        let loader = match tname.to_ascii_lowercase().as_str() {
            "forge" => ModLoader::Forge,
            "neoforge" => ModLoader::NeoForge,
            "fabric" => ModLoader::Fabric,
            "quilt" => ModLoader::Quilt,
            _ => continue,
        };
        set_loader(&mut meta, loader, tver)?;
        return Ok(meta);
    }
    Err("FTB version.json names no supported loader.".into())
}

fn is_ftb_instance(value: &Value) -> bool {
    // FTB App's instance.json carries uuid + a NUMERIC versionId + mcVersion
    // together; ATLauncher's same-named file carries a `launcher` object and
    // string ids instead. All three FTB markers are required so an
    // ATLauncher folder can never misroute here.
    value.get("uuid").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
        && value.get("versionId").and_then(Value::as_u64).is_some()
        && value.get("mcVersion").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
}

// Technic game folder: `bin/modpack.jar` (with an embedded OneSix
// `version.json`, else legacy `fmlversion.properties` /
// `forgeversion.properties`) or a loose `bin/version.json`. Loader is
// derived from the version JSON's libraries exactly like Prism's
// TechnicPackProcessor does; game files sit at the folder root.
fn technic(root: &Path) -> Result<Metadata, String> {
    let bin = root.join("bin");
    if bin.join("version.json").is_file() {
        let doc = json(&bin.join("version.json"))?;
        let (minecraft, loader) = technic_version_json(&doc)?;
        return technic_meta(root, minecraft, loader);
    }
    if bin.join("modpack.jar").is_file() {
        let file = std::fs::File::open(bin.join("modpack.jar")).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Invalid Technic modpack.jar: {e}"))?;
        if let Some(doc) = technic_zip_json(&mut archive, "version.json")? {
            let fml = technic_zip_properties(&mut archive, "fmlversion.properties")?;
            let (minecraft, loader) = technic_version_json_fallback(&doc, fml.as_deref())?;
            return technic_meta(root, minecraft, loader);
        }
        // Pre-OneSix jar-mod packs: MC from fmlversion, Forge iff the
        // forge properties pin a build. The jar-mods themselves can't run
        // on a modern loader — the game files still import honestly.
        let props = technic_zip_properties(&mut archive, "fmlversion.properties")?
            .ok_or("Technic modpack.jar has no version.json and no fmlversion.properties.")?;
        let minecraft = props
            .lines()
            .find_map(|l| l.trim().strip_prefix("fmlbuild.mcversion").and_then(|v| v.strip_prefix('=')))
            .filter(|s| !s.trim().is_empty())
            .ok_or("Technic fmlversion.properties names no Minecraft version.")?;
        let loader = technic_zip_properties(&mut archive, "forgeversion.properties")?
            .and_then(|p| technic_forge_properties(&p))
            .map(|v| (ModLoader::Forge, v));
        return technic_meta(root, minecraft.to_string(), loader);
    }
    Err("Not a Technic folder: expected bin/version.json or bin/modpack.jar.".into())
}

fn technic_meta(root: &Path, minecraft: String, loader: Option<(ModLoader, String)>) -> Result<Metadata, String> {
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .ok_or("Technic folder has no usable name.")?;
    let mut meta = Metadata {
        name: name.to_owned(),
        minecraft: version(&minecraft)?,
        loader: ModLoader::Vanilla,
        loader_version: None,
    };
    if let Some((kind, build)) = loader {
        set_loader(&mut meta, kind, &build)?;
    }
    Ok(meta)
}

/// OneSix version JSON: MC from `inheritsFrom`, loader from the library
/// coordinates. First match wins.
fn technic_version_json(doc: &Value) -> Result<(String, Option<(ModLoader, String)>), String> {
    technic_version_json_fallback(doc, None)
}

fn technic_version_json_fallback(
    doc: &Value,
    fml_mc_version: Option<&str>,
) -> Result<(String, Option<(ModLoader, String)>), String> {
    let minecraft = doc
        .get("inheritsFrom")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or(fml_mc_version)
        .ok_or("Technic version.json has no inheritsFrom.")?;
    if let Some(libs) = doc.get("libraries").and_then(Value::as_array) {
        for lib in libs {
            let name = lib.get("name").and_then(Value::as_str).unwrap_or("");
            // NeoForge hides its version in the game args, not the
            // coordinate — stop scanning either way, like Prism does.
            if name.starts_with("net.neoforged.fancymodloader:") {
                return Ok((
                    minecraft.to_string(),
                    technic_neoforge_version(doc).map(|v| (ModLoader::NeoForge, v)),
                ));
            }
            if (name.starts_with("net.minecraftforge:forge:")
                || name.starts_with("net.minecraftforge:fmlloader:"))
                && name.contains('-')
            {
                let coordinate = name.split(':').nth(2).unwrap_or("");
                // 1.7.10 files look like 1.7.10-10.13.4.1614-1.7.10: the
                // build is the middle segment, not "everything past the
                // first dash".
                let build = if coordinate.starts_with("1.7.10-") {
                    coordinate.split('-').nth(1).unwrap_or(coordinate)
                } else {
                    coordinate.split_once('-').map(|(_, b)| b).unwrap_or(coordinate)
                };
                return Ok((minecraft.to_string(), Some((ModLoader::Forge, build.to_string()))));
            }
            for (prefix, loader) in [
                ("net.minecraftforge:minecraftforge:", ModLoader::Forge),
                ("net.fabricmc:fabric-loader:", ModLoader::Fabric),
                ("org.quiltmc:quilt-loader:", ModLoader::Quilt),
            ] {
                if name.starts_with(prefix) {
                    let build = name.split(':').nth(2).unwrap_or("");
                    return Ok((minecraft.to_string(), Some((loader, build.to_string()))));
                }
            }
        }
    }
    Ok((minecraft.to_string(), None))
}

fn technic_neoforge_version(doc: &Value) -> Option<String> {
    let game = doc.get("arguments")?.get("game")?.as_array()?;
    let mut take_next = false;
    for arg in game {
        if take_next {
            if let Some(s) = arg.as_str().filter(|s| !s.is_empty()) {
                return Some(s.to_string());
            }
            break;
        }
        if let Some(s) = arg.as_str() {
            take_next = s == "--fml.neoForgeVersion" || s == "--fml.forgeVersion";
        }
    }
    None
}

fn technic_forge_properties(props: &str) -> Option<String> {
    let get = |key: &str| {
        props
            .lines()
            .find_map(|l| l.trim().strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .filter(|s| !s.trim().is_empty())
    };
    Some(format!(
        "{}.{}.{}.{}",
        get("forge.major.number")?,
        get("forge.minor.number")?,
        get("forge.revision.number")?,
        get("forge.build.number")?,
    ))
}

fn technic_zip_json(
    archive: &mut zip::ZipArchive<std::fs::File>,
    entry_name: &str,
) -> Result<Option<Value>, String> {
    let mut entry = match archive.by_name(entry_name) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    if entry.size() > 4 * 1024 * 1024 {
        return Err(format!("Technic {entry_name} is suspiciously large."));
    }
    let mut contents = String::new();
    use std::io::Read;
    entry.read_to_string(&mut contents).map_err(|e| e.to_string())?;
    serde_json::from_str(&contents).map(Some).map_err(|e| format!("Invalid Technic {entry_name}: {e}"))
}

fn technic_zip_properties(
    archive: &mut zip::ZipArchive<std::fs::File>,
    entry_name: &str,
) -> Result<Option<String>, String> {
    let mut entry = match archive.by_name(entry_name) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    if entry.size() > 1024 * 1024 {
        return Err(format!("Technic {entry_name} is suspiciously large."));
    }
    let mut contents = String::new();
    use std::io::Read;
    entry.read_to_string(&mut contents).map_err(|e| e.to_string())?;
    Ok(Some(contents))
}

pub(super) fn directory(root: &Path) -> Result<(Metadata, PathBuf), String> {
    reject_links(root)?;
    if root.join("mmc-pack.json").is_file() && root.join("instance.cfg").is_file() {
        if root.join("patches").exists() && std::fs::read_dir(root.join("patches")).map_err(|e| e.to_string())?.next().is_some() {
            return Err("Custom Prism/MultiMC patches cannot be reproduced. Export a standard loader instance instead.".into());
        }
        let cfg = String::from_utf8(read_bounded(&root.join("instance.cfg"), 1024 * 1024)?).map_err(|e| e.to_string())?;
        let meta = prism(&json(&root.join("mmc-pack.json"))?, &cfg)?;
        let candidates: Vec<_> = [root.join(".minecraft"), root.join("minecraft")].into_iter().filter(|p| p.is_dir()).collect();
        if candidates.len() != 1 { return Err("Expected exactly one .minecraft or minecraft game folder in Prism/MultiMC instance.".into()); }
        reject_links(&candidates[0])?;
        return Ok((meta, candidates[0].clone()));
    }
    if root.join("minecraftinstance.json").is_file() {
        // The CurseForge App embeds the full modpack manifest inside
        // minecraftinstance.json, and third-party importers (e.g. XMCL
        // packages/instance/parsers/curseforge_parser.ts) resolve versions
        // from that manifest — the same minecraft.version / modLoaders[].id
        // shape a modpack ZIP carries. Parse it directly instead of guessing
        // at the file's outer fields.
        let value = json(&root.join("minecraftinstance.json"))?;
        let manifest = value
            .get("manifest")
            .filter(|v| v.is_object())
            .ok_or("CurseForge App instance has no embedded modpack manifest. Export it as a modpack ZIP instead.")?;
        let mut meta = curseforge(manifest)?;
        if let Some(name) = value.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            meta.name = name.to_owned();
        }
        return Ok((meta, root.to_path_buf()));
    }
    if root.join("instance.json").is_file() {
        let value = json(&root.join("instance.json"))?;
        // Same filename, two launchers: FTB App's instance.json carries
        // uuid + numeric versionId + mcVersion, ATLauncher's carries a
        // `launcher` object instead. Content-sniff so neither misroutes.
        if is_ftb_instance(&value) {
            return Ok((ftb(&value, root)?, root.to_path_buf()));
        }
        return Ok((atlauncher(&value)?, root.to_path_buf()));
    }
    if root.join("config.json").is_file() {
        return Ok((gdlauncher(&json(&root.join("config.json"))?, root)?, root.to_path_buf()));
    }
    if root.join("bin").join("modpack.jar").is_file() || root.join("bin").join("version.json").is_file() {
        return Ok((technic(root)?, root.to_path_buf()));
    }
    Err("Unsupported instance folder. Select a Prism/MultiMC, ATLauncher, FTB App, Technic, legacy GDLauncher, or CurseForge App instance with an embedded manifest. Other launchers must export .mrpack or CurseForge modpack ZIP.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn third_party_loaders_require_known_types_and_exact_pins() {
        let mut at = json!({"id":"1.12.2","launcher":{"name":"SkyFactory","loaderVersion":{"type":"Forge","version":"14.23.5.2855"}}});
        assert_eq!(atlauncher(&at).unwrap().loader_version.as_deref(), Some("14.23.5.2855"));
        at["launcher"]["loaderVersion"]["type"] = json!("unknown-loader");
        assert!(atlauncher(&at).is_err());
        let mut gd = json!({"loader":{"loaderType":"fabric","loaderVersion":"0.16.9","mcVersion":"1.20.1"}});
        assert_eq!(gdlauncher(&gd, Path::new("Cedar")).unwrap().loader, ModLoader::Fabric);
        gd["loader"]["loaderVersion"] = Value::Null;
        assert!(gdlauncher(&gd, Path::new("Cedar")).is_err());
    }

    #[test]
    fn curseforge_app_embedded_manifest_is_authoritative() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("minecraftinstance.json"), serde_json::to_vec(&json!({
            "name": "Installed From App",
            "gameVersion": "1.20.1",
            "baseModLoader": {"name": "fabric-0.99.9-1.20.1", "minecraftVersion": "1.20.1"},
            "manifest": {"manifestType": "minecraftModpack", "manifestVersion": 1, "name": "Pack", "minecraft": {"version": "1.20.1", "modLoaders": [{"id": "forge-47.3.0", "primary": true}]}}
        })).unwrap()).unwrap();
        let (meta, game) = directory(dir.path()).unwrap();
        assert_eq!(meta.name, "Installed From App");
        assert_eq!(meta.minecraft, "1.20.1");
        assert_eq!(meta.loader, ModLoader::Forge);
        assert_eq!(meta.loader_version.as_deref(), Some("47.3.0"));
        assert_eq!(game, dir.path());
    }

    #[test]
    fn quilt_prism_pin_is_preserved_and_disabled_component_ignored() {
        let value = json!({"formatVersion":1,"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"org.quiltmc.quilt-loader","version":"0.27.1"},{"uid":"custom.patch","disabled":true}]});
        let meta = prism(&value, "[General]
name=My Quilt world
").unwrap();
        assert_eq!(meta.minecraft, "1.20.1");
        assert_eq!(meta.loader, ModLoader::Quilt);
        assert_eq!(meta.loader_version.as_deref(), Some("0.27.1"));
    }

    #[test]
    fn rejects_unpinned_and_custom_components() {
        let mut value = json!({"formatVersion":1,"components":[{"uid":"net.minecraft","cachedVersion":"1.20.1"}]});
        assert!(prism(&value, "name=Pack").is_err());
        value["components"][0] = json!({"uid":"net.minecraft","version":"1.20.1"});
        value["components"].as_array_mut().unwrap().push(json!({"uid":"custom.patch","version":"1"}));
        assert!(prism(&value, "name=Pack").is_err());
    }

    #[test]
    fn mrpack_and_curseforge_dependencies_preserve_exact_build() {
        let mr = mrpack(&json!({"formatVersion":1,"game":"minecraft","name":"Pack","dependencies":{"minecraft":"1.21.1","neoforge":"21.1.172"}})).unwrap();
        let cf = curseforge(&json!({"manifestType":"minecraftModpack","manifestVersion":1,"name":"Pack","minecraft":{"version":"1.21.1","modLoaders":[{"id":"neoforge-21.1.172","primary":true}]}})).unwrap();
        assert_eq!(mr.minecraft, cf.minecraft);
        assert_eq!(mr.loader, cf.loader);
        assert_eq!(mr.loader_version, cf.loader_version);
    }

    #[test]
    fn ftb_instance_json_parses_loader_and_legacy_targets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("instance.json"),
            serde_json::to_vec(&json!({
                "uuid": "abc-123", "id": 123, "versionId": 456, "name": "FTB Pack",
                "version": "1.0.0", "mcVersion": "1.20.1", "modLoader": "forge-47.3.0",
            })).unwrap(),
        )
        .unwrap();
        let (meta, game) = directory(dir.path()).unwrap();
        assert_eq!(meta.name, "FTB Pack");
        assert_eq!(meta.minecraft, "1.20.1");
        assert_eq!(meta.loader, ModLoader::Forge);
        assert_eq!(meta.loader_version.as_deref(), Some("47.3.0"));
        assert_eq!(game, dir.path());

        // No modLoader: legacy targets[] in .ftbapp/version.json.
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(
            dir2.path().join("instance.json"),
            serde_json::to_vec(&json!({
                "uuid": "abc-123", "id": 123, "versionId": 456, "name": "FTB Legacy",
                "version": "1.0.0", "mcVersion": "1.16.5",
            })).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(dir2.path().join(".ftbapp")).unwrap();
        std::fs::write(
            dir2.path().join(".ftbapp/version.json"),
            serde_json::to_vec(&json!({"targets": [
                {"name": "minecraft", "version": "1.16.5"},
                {"name": "forge", "version": "36.2.34"},
            ]})).unwrap(),
        )
        .unwrap();
        let (meta, _) = directory(dir2.path()).unwrap();
        assert_eq!(meta.loader, ModLoader::Forge);
        assert_eq!(meta.loader_version.as_deref(), Some("36.2.34"));
    }

    #[test]
    fn atlauncher_instance_json_still_routes_to_atlauncher() {
        // Same filename as FTB — the launcher object (and no FTB markers)
        // must keep this on the ATLauncher path.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("instance.json"),
            serde_json::to_vec(&json!({
                "id": "1.20.1",
                "launcher": {"name": "Sky", "loaderVersion": {"type": "Fabric", "version": "0.16.9"}},
            })).unwrap(),
        )
        .unwrap();
        let (meta, _) = directory(dir.path()).unwrap();
        assert_eq!(meta.name, "Sky");
        assert_eq!(meta.loader, ModLoader::Fabric);
    }

    #[test]
    fn technic_version_json_derives_loader_from_libraries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(
            dir.path().join("bin/version.json"),
            serde_json::to_vec(&json!({
                "inheritsFrom": "1.20.1",
                "libraries": [{"name": "net.minecraftforge:forge:1.20.1-47.3.0"}],
            })).unwrap(),
        )
        .unwrap();
        let (meta, game) = directory(dir.path()).unwrap();
        assert_eq!(meta.minecraft, "1.20.1");
        assert_eq!(meta.loader, ModLoader::Forge);
        assert_eq!(meta.loader_version.as_deref(), Some("47.3.0"));
        assert_eq!(game, dir.path());
    }

    #[test]
    fn technic_1710_forge_build_picks_the_middle_segment() {
        let doc = json!({
            "inheritsFrom": "1.7.10",
            "libraries": [{"name": "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10"}],
        });
        let (mc, loader) = technic_version_json(&doc).unwrap();
        assert_eq!(mc, "1.7.10");
        assert_eq!(loader, Some((ModLoader::Forge, "10.13.4.1614".to_string())));
    }

    #[test]
    fn technic_neoforge_version_comes_from_game_args() {
        let doc = json!({
            "inheritsFrom": "1.21.1",
            "libraries": [{"name": "net.neoforged.fancymodloader:6.0:extra"}],
            "arguments": {"game": ["--fml.neoForgeVersion", "21.1.172"]},
        });
        let (mc, loader) = technic_version_json(&doc).unwrap();
        assert_eq!(mc, "1.21.1");
        assert_eq!(loader, Some((ModLoader::NeoForge, "21.1.172".to_string())));
    }
}
