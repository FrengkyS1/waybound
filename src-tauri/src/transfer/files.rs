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
        reject_links(parent)?;
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
    crate::download::ensure_contained_path(path, path)
        .map_err(|_| "Linked, reparse-point, or unsafe paths are not supported. Select a real local folder or archive.".to_string())
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

// Read in bounded chunks and reject excess bytes before writing them. Declared
// sizes alone do not bound a growing source or malformed compressed stream.
fn copy_bounded(
    input: &mut impl Read,
    output: &mut impl Write,
    limit: u64,
    total: &mut u64,
    total_limit: u64,
    cancel: &CancelToken,
) -> Result<u64, String> {
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        check_cancel(cancel)?;
        let remaining = limit.saturating_sub(size).min(total_limit.saturating_sub(*total));
        let capacity = (remaining.min(buffer.len() as u64 - 1) + 1) as usize;
        let count = input.read(&mut buffer[..capacity]).map_err(|e| e.to_string())?;
        if count == 0 { return Ok(size); }
        if count as u64 > remaining { return Err("Transfer exceeds actual-byte size limit.".into()); }
        size += count as u64;
        *total += count as u64;
        output.write_all(&buffer[..count]).map_err(|e| e.to_string())?;
    }
}

/// Validate every entry before extraction, including ignored launcher metadata.
/// Extraction writes only into a newly-created private staging directory.
pub(super) fn extract(bytes: &[u8], destination: &Path, cancel: &CancelToken) -> Result<(), String> {
    if bytes.len() as u64 > MAX_ARCHIVE { return Err("Archive exceeds the 512 MiB received-byte limit.".into()); }
    reject_links(destination)?;
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
    let mut received = 0u64;
    for i in 0..zip.len() {
        check_cancel(cancel)?;
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let target = destination.join(relative_path(entry.name())?);
        crate::download::ensure_contained_path(destination, &target).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            reject_links(&target)?;
            continue;
        }
        fs::create_dir_all(target.parent().ok_or("Invalid archive entry.")?).map_err(|e| e.to_string())?;
        crate::download::ensure_contained_path(destination, &target).map_err(|e| e.to_string())?;
        let expected = entry.size();
        let mut output = File::options().write(true).create_new(true).open(target).map_err(|e| e.to_string())?;
        let copied = copy_bounded(&mut entry, &mut output, expected.min(MAX_FILE), &mut received, MAX_TOTAL, cancel)?;
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
    reject_links(target)?;
    let mut total = 0u64;
    for relative in inventory(source, false, cancel)? {
        check_cancel(cancel)?;
        let dest = target.join(&relative);
        crate::download::ensure_contained_path(target, &dest).map_err(|e| e.to_string())?;
        fs::create_dir_all(dest.parent().ok_or("Invalid file path.")?).map_err(|e| e.to_string())?;
        crate::download::ensure_contained_path(target, &dest).map_err(|e| e.to_string())?;
        reject_links(&source.join(&relative))?;
        let mut input = File::open(source.join(relative)).map_err(|e| e.to_string())?;
        let mut output = File::options().write(true).create_new(true).open(dest).map_err(|e| e.to_string())?;
        copy_bounded(&mut input, &mut output, MAX_FILE, &mut total, MAX_TOTAL, cancel)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(path: &str, contents: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer.start_file(path, zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)).unwrap();
        writer.write_all(contents).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn actual_bytes_are_bounded_before_writing_excess() {
        let mut input = std::io::Cursor::new(b"oversized");
        let mut output = Vec::new();
        let mut total = 0;
        assert!(copy_bounded(&mut input, &mut output, 4, &mut total, 8, &CancelToken::new()).is_err());
        assert!(output.len() <= 4);
        assert!(total <= 4);
    }

    #[test]
    fn actual_total_budget_is_shared_across_files() {
        let cancel = CancelToken::new();
        let mut total = 0;
        let mut first = Vec::new();
        assert_eq!(copy_bounded(&mut b"1234".as_slice(), &mut first, 4, &mut total, 6, &cancel).unwrap(), 4);
        let mut second = Vec::new();
        assert!(copy_bounded(&mut b"567".as_slice(), &mut second, 4, &mut total, 6, &cancel).is_err());
        assert_eq!(first, b"1234");
        assert!(second.len() <= 2);
        assert!(total <= 6);
    }

    #[test]
    fn bounded_reads_and_cancelled_copy_never_publish_excess() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.zip");
        fs::write(&path, b"12345").unwrap();
        assert!(read_bounded(&path, 4).is_err());
        assert_eq!(read_bounded(&path, 5).unwrap(), b"12345");
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut output = Vec::new();
        let mut total = 0;
        assert!(copy_bounded(&mut b"12345".as_slice(), &mut output, 5, &mut total, 5, &cancel).is_err());
        assert!(output.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn extraction_validates_all_paths_before_any_write() {
        let target = tempfile::tempdir().unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer.start_file("config/good.toml", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(b"enabled = true").unwrap();
        writer.start_file("../escaped", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(b"unsafe").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(extract(&bytes, target.path(), &CancelToken::new()).is_err());
        assert_eq!(fs::read_dir(target.path()).unwrap().count(), 0);
    }

    #[test]
    fn extraction_rejects_forged_uncompressed_size() {
        let mut bytes = archive("config/test.txt", b"payload");
        let central = bytes.windows(4).position(|header| header == b"PK\x01\x02").unwrap();
        // ZIP central-directory uncompressed-size field disagrees with stream.
        bytes[central + 24..central + 28].copy_from_slice(&1u32.to_le_bytes());
        let target = tempfile::tempdir().unwrap();
        assert!(extract(&bytes, target.path(), &CancelToken::new()).is_err());
        if let Ok(metadata) = fs::metadata(target.path().join("config/test.txt")) {
            assert!(metadata.len() <= 1, "never write beyond advertised budget");
        }
    }

    #[test]
    fn extraction_preserves_existing_files_and_honors_cancellation() {
        let target = tempfile::tempdir().unwrap();
        let path = target.path().join("existing.txt");
        fs::write(&path, b"original").unwrap();
        assert!(extract(&archive("existing.txt", b"replacement"), target.path(), &CancelToken::new()).is_err());
        assert_eq!(fs::read(path).unwrap(), b"original");
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(extract(&archive("new.txt", b"new"), target.path(), &cancel).is_err());
        assert!(!target.path().join("new.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn extraction_and_copy_reject_linked_destination_ancestors() {
        let destination = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), destination.path().join("config")).unwrap();
        assert!(extract(&archive("config/test.txt", b"private"), destination.path(), &CancelToken::new()).is_err());
        assert!(!outside.path().join("test.txt").exists());
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("config")).unwrap();
        fs::write(source.path().join("config/test.txt"), b"private").unwrap();
        assert!(copy_game(source.path(), destination.path(), &CancelToken::new()).is_err());
        assert!(!outside.path().join("test.txt").exists());
    }

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
