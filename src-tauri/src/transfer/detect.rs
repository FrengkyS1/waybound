//! Fixed-root discovery, using the same metadata adapters as import.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use serde::Serialize;
use crate::dto::ModLoader;
use super::{files::{read_bounded, reject_links}, formats};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedInstance {
    pub path: String,
    pub folder: String,
    pub name: String,
    pub minecraft: String,
    pub loader: ModLoader,
    pub loader_version: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedLauncher {
    pub name: &'static str,
    pub root: String,
    pub instances: Vec<DetectedInstance>,
}

fn mmc_instances_dir(root: &Path, cfg: &str) -> Result<PathBuf, String> {
    let config = root.join(cfg);
    if !config.exists() { return Ok(root.join("instances")); }
    let text = String::from_utf8(read_bounded(&config, 1024 * 1024)?).map_err(|e| e.to_string())?;
    let configured = text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim().eq_ignore_ascii_case("InstanceDir") && !value.trim().is_empty())
            .then(|| value.trim())
    });
    Ok(root.join(configured.unwrap_or("instances")))
}

fn launcher_name(folder: &Path) -> &'static str {
    if folder.join("mmc-pack.json").is_file() { "Prism / MultiMC" }
    else if folder.join("minecraftinstance.json").is_file() { "CurseForge App" }
    else if folder.join("instance.json").is_file() {
        // Same filename, two launchers — match the import's own
        // content sniff so discovery labels what import will parse.
        let is_ftb = std::fs::read(folder.join("instance.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|v| {
                v.get("uuid").and_then(|u| u.as_str()).is_some_and(|s| !s.is_empty())
                    && v.get("versionId").and_then(|u| u.as_u64()).is_some()
                    && v.get("mcVersion").and_then(|u| u.as_str()).is_some_and(|s| !s.is_empty())
            });
        if is_ftb { "FTB App" } else { "ATLauncher" }
    }
    else if folder.join("bin").join("modpack.jar").is_file()
        || folder.join("bin").join("version.json").is_file()
    {
        "Technic"
    }
    else { "GDLauncher (legacy)" }
}

fn children(root: &Path) -> Result<Vec<PathBuf>, String> {
    reject_links(root)?;
    let entries = std::fs::read_dir(root).map_err(|e| format!("Cannot scan {}: {e}", root.display()))?;
    let mut folders = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            folders.push(entry.path());
        }
    }
    folders.sort();
    Ok(folders)
}

fn scan(root: &Path, name: Option<&'static str>, seen: &mut HashSet<PathBuf>, output: &mut Vec<DetectedLauncher>) -> Result<(), String> {
    reject_links(root)?;
    let root = root.canonicalize().map_err(|e| format!("Cannot open {}: {e}", root.display()))?;
    let mut directories = vec![root.clone()];
    for cfg in ["prismlauncher.cfg", "multimc.cfg"] {
        if root.join(cfg).is_file() { directories.push(mmc_instances_dir(&root, cfg)?); }
    }
    for child in ["instances", "Instances"] {
        let candidate = root.join(child);
        if candidate.is_dir() { directories.push(candidate); }
    }
    let mut candidates = vec![root.clone()];
    let mut scanned = HashSet::new();
    for directory in directories {
        if !directory.is_dir() { continue; }
        let canonical = directory.canonicalize().map_err(|e| e.to_string())?;
        if scanned.insert(canonical) { candidates.extend(children(&directory)?); }
    }
    for candidate in candidates {
        let Ok((meta, _)) = formats::directory(&candidate) else { continue };
        let canonical = candidate.canonicalize().map_err(|e| e.to_string())?;
        if !seen.insert(canonical.clone()) { continue; }
        let label = name.unwrap_or_else(|| launcher_name(&candidate));
        let instance = DetectedInstance {
            path: canonical.to_string_lossy().into_owned(),
            folder: candidate.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            name: meta.name, minecraft: meta.minecraft, loader: meta.loader,
            loader_version: meta.loader_version,
        };
        let root_string = root.to_string_lossy();
        if let Some(group) = output.iter_mut().find(|group| group.name == label && group.root == root_string) {
            group.instances.push(instance);
        } else {
            output.push(DetectedLauncher { name: label, root: root_string.into_owned(), instances: vec![instance] });
        }
    }
    Ok(())
}

pub fn detect_launchers(root_path: Option<&Path>) -> Result<Vec<DetectedLauncher>, String> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    if let Some(root) = root_path {
        scan(root, None, &mut seen, &mut output)?;
    } else {
        let mut roots = Vec::new();
        if let Some(data) = dirs::data_dir() {
            for (name, folder) in [("Prism Launcher", "PrismLauncher"), ("MultiMC", "MultiMC"), ("ATLauncher", "ATLauncher"), ("GDLauncher (legacy)", "gdlauncher_next")] {
                roots.push((name, data.join(folder)));
            }
        }
        if let Some(home) = dirs::home_dir() {
            roots.push(("MultiMC", home.join("MultiMC")));
            roots.push(("CurseForge App", home.join("curseforge/minecraft")));
            roots.push(("Technic", home.join(".technic/modpacks")));
        }
        if let Some(documents) = dirs::document_dir() {
            roots.push(("CurseForge App", documents.join("curseforge/minecraft")));
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            roots.push(("MultiMC", PathBuf::from(program_files).join("MultiMC")));
        }
        roots.push(("MultiMC", PathBuf::from("C:/MultiMC")));
        for (name, root) in roots {
            if root.is_dir() { scan(&root, Some(name), &mut seen, &mut output)?; }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_instance_directory_returns_exact_import_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Prism");
        let source = temp.path().join("custom/pack");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(source.join("minecraft/config")).unwrap();
        std::fs::write(root.join("prismlauncher.cfg"), format!("[General]\nInstanceDir={}\n", temp.path().join("custom").display())).unwrap();
        std::fs::write(source.join("instance.cfg"), "[General]\nname=Cedar\n").unwrap();
        std::fs::write(source.join("mmc-pack.json"), r#"{"formatVersion":1,"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"net.fabricmc.fabric-loader","version":"0.16.9"}]}"#).unwrap();
        let found = detect_launchers(Some(&root)).unwrap();
        let selected = &found[0].instances[0];
        assert_eq!(PathBuf::from(&selected.path), source.canonicalize().unwrap());
        let (meta, game) = formats::directory(Path::new(&selected.path)).unwrap();
        assert_eq!(meta.loader_version.as_deref(), Some("0.16.9"));
        assert_eq!(game.canonicalize().unwrap(), source.join("minecraft").canonicalize().unwrap());
    }
}
