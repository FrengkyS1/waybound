use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::download::CancelToken;

pub(super) const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_TOTAL: u64 = 8 * 1024 * 1024 * 1024;
const MAX_FILES: usize = 50_000;
static NEXT: AtomicU64 = AtomicU64::new(0);

pub(super) struct Stage(pub PathBuf);
impl Stage {
    pub fn new(parent: &Path) -> Result<Self, String> {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        reject_links(parent)?;
        loop {
            let path = parent.join(format!(".transfer-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}
impl Drop for Stage {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

pub(super) fn reject_links(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() { return Err("Linked paths are not supported. Select a real local folder or archive.".into()); }
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 { return Err("Reparse-point paths are not supported. Select a real local folder or archive.".into()); }
        }
    }
    Ok(())
}

pub(super) fn relative_path(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.len() > 1024 || value.contains('\\') { return Err("Invalid archive path.".into()); }
    let mut result = PathBuf::new();
    for part in value.trim_end_matches('/').split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_lowercase();
        if part.is_empty() || part == "." || part == ".." || part.ends_with(['.', ' '])
            || part.chars().any(|c| c.is_control() || ":<>\"|?*".contains(c))
            || matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
            || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && matches!(stem.as_bytes()[3], b'1'..=b'9')) {
            return Err("Archive contains an unsupported or unsafe path.".into());
        }
        result.push(part);
    }
    if result.components().count() > 32 { return Err("Archive directory nesting exceeds limit.".into()); }
    Ok(result)
}

pub(super) fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    reject_links(path)?;
    let file = File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > limit { return Err("Transfer file exceeds size limit.".into()); }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit { return Err("Transfer file exceeds size limit.".into()); }
    Ok(bytes)
}

pub(super) fn check_cancel(cancel: &CancelToken) -> Result<(), String> {
    if cancel.is_cancelled() { Err("Download cancelled.".into()) } else { Ok(()) }
}

/// Validate every entry before extraction, including ignored launcher metadata.
/// Extraction writes only into a newly-created private staging directory.
pub(super) fn extract(bytes: &[u8], destination: &Path, cancel: &CancelToken) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    if zip.len() > MAX_FILES { return Err("Archive contains too many entries (limit 50,000).".into()); }
    let mut total = 0u64;
    let mut names = HashSet::new();
    for i in 0..zip.len() {
        check_cancel(cancel)?;
        let entry = zip.by_index(i).map_err(|e| e.to_string())?;
        relative_path(entry.name())?;
        if !names.insert(entry.name().trim_end_matches('/').to_ascii_lowercase()) { return Err("Archive contains duplicate or case-colliding paths.".into()); }
        let kind = entry.unix_mode().unwrap_or(0) & 0o170000;
        if kind != 0 && kind != 0o100000 && kind != 0o040000 { return Err("Archive contains links or special files.".into()); }
        total = total.checked_add(entry.size()).ok_or("Archive size overflow.")?;
        if entry.size() > MAX_FILE || total > MAX_TOTAL { return Err("Archive exceeds extraction limit (512 MiB per file, 8 GiB total).".into()); }
    }
    for i in 0..zip.len() {
        check_cancel(cancel)?;
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let target = destination.join(relative_path(entry.name())?);
        if entry.is_dir() { fs::create_dir_all(target).map_err(|e| e.to_string())?; continue; }
        fs::create_dir_all(target.parent().ok_or("Invalid archive entry.")?).map_err(|e| e.to_string())?;
        let expected = entry.size();
        let mut output = File::options().write(true).create_new(true).open(target).map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut (&mut entry).take(expected + 1), &mut output).map_err(|e| e.to_string())?;
        if copied != expected { return Err("Archive entry size mismatch.".into()); }
    }
    Ok(())
}

pub(super) fn excluded(relative: &Path, export: bool) -> bool {
    let parts: Vec<String> = relative.iter().map(|p| p.to_string_lossy().to_ascii_lowercase()).collect();
    if parts.iter().any(|p| p.starts_with('.') || p.contains("account") || p.contains("token") || p.contains("credential") || p.contains("secret") || p == "session.lock") { return true; }
    let top = parts.first().map(String::as_str).unwrap_or("");
    matches!(top, "logs" | "crash-reports" | "cache" | "caches" | "webcache" | "libraries" | "assets" | "versions" | "natives" | "runtime" | "launcher_profiles.json" | "launcher_settings.json" | "minecraftinstance.json" | "instance.cfg" | "mmc-pack.json" | "patches" | "icon.png" | "manifest.json")
        || (export && matches!(top, "saves" | "worlds" | "backups" | "screenshots" | "replay_recordings" | "servers.dat" | "servers.dat_old" | "usercache.json" | "usernamecache.json"))
}

pub(super) fn inventory(root: &Path, export: bool, cancel: &CancelToken) -> Result<Vec<PathBuf>, String> {
    reject_links(root)?;
    let mut pending = vec![PathBuf::new()];
    let mut files = Vec::new();
    let mut entries = 0;
    let mut total = 0u64;
    while let Some(relative) = pending.pop() {
        for entry in fs::read_dir(root.join(&relative)).map_err(|e| e.to_string())? {
            check_cancel(cancel)?;
            let entry = entry.map_err(|e| e.to_string())?;
            entries += 1;
            if entries > MAX_FILES { return Err("Folder contains too many entries (limit 50,000).".into()); }
            let relative = relative.join(entry.file_name());
            if excluded(&relative, export) { continue; }
            let text = relative.to_str().ok_or("Non-Unicode paths are unsupported.")?.replace('\\', "/");
            relative_path(&text)?;
            reject_links(&entry.path())?;
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            if meta.is_dir() { pending.push(relative); }
            else if meta.is_file() {
                total = total.checked_add(meta.len()).ok_or("Folder size overflow.")?;
                if meta.len() > MAX_FILE || total > MAX_TOTAL { return Err("Folder exceeds transfer size limit.".into()); }
                files.push(relative);
            } else { return Err("Special files are not supported.".into()); }
        }
    }
    files.sort();
    Ok(files)
}

pub(super) fn copy_game(source: &Path, target: &Path, cancel: &CancelToken) -> Result<(), String> {
    for relative in inventory(source, false, cancel)? {
        check_cancel(cancel)?;
        let dest = target.join(&relative);
        fs::create_dir_all(dest.parent().ok_or("Invalid file path.")?).map_err(|e| e.to_string())?;
        reject_links(&source.join(&relative))?;
        let mut input = File::open(source.join(relative)).map_err(|e| e.to_string())?.take(MAX_FILE + 1);
        let mut output = File::options().write(true).create_new(true).open(dest).map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 64 * 1024];
        let mut size = 0;
        loop {
            check_cancel(cancel)?;
            let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 { break; }
            size += n as u64;
            if size > MAX_FILE { return Err("Source changed beyond transfer size limit.".into()); }
            output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_copy_preserves_user_content_without_account_metadata() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        for (path, contents) in [("mods/local.jar.disabled", "local mod"), ("config/mod.toml", "enabled=true"), ("saves/My world/level.dat", "world"), ("resourcepacks/local.zip", "local pack"), ("accounts.json", "private"), ("logs/latest.log", "log")] {
            let path = source.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        copy_game(source.path(), target.path(), &CancelToken::new()).unwrap();
        assert_eq!(fs::read(target.path().join("mods/local.jar.disabled")).unwrap(), b"local mod");
        assert_eq!(fs::read(target.path().join("saves/My world/level.dat")).unwrap(), b"world");
        assert_eq!(fs::read(target.path().join("resourcepacks/local.zip")).unwrap(), b"local pack");
        assert!(!target.path().join("accounts.json").exists());
        assert!(!target.path().join("logs").exists());
        assert_eq!(fs::read(source.path().join("accounts.json")).unwrap(), b"private");
        let exported = inventory(target.path(), true, &CancelToken::new()).unwrap();
        assert!(exported.contains(&PathBuf::from("config/mod.toml")));
        assert!(!exported.iter().any(|p| p.starts_with("saves")));
    }

    #[test]
    fn failed_or_cancelled_transfer_stage_is_removed() {
        let parent = tempfile::tempdir().unwrap();
        let path;
        {
            let stage = Stage::new(parent.path()).unwrap();
            path = stage.0.clone();
            fs::write(path.join("partial"), b"partial").unwrap();
            let cancel = CancelToken::new();
            cancel.cancel();
            assert!(check_cancel(&cancel).is_err());
        }
        assert!(!path.exists());
    }
}
