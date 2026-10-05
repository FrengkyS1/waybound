use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::ModpackError;
use crate::download::{atomic_write, contained_join, CancelToken, DownloadError, MAX_DOWNLOAD_BYTES};

static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);

pub(crate) const MAX_PACK_FILE_BYTES: usize = 512 * 1024 * 1024;
pub(super) const MAX_PACK_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub(super) const MAX_PACK_FILES: usize = 50_000;
pub(super) const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

/// Files and tracking share one rollback boundary, including DB publication.
pub(crate) struct PackTransaction {
    root: PathBuf,
    stage: PathBuf,
    changes: Vec<(PathBuf, Option<PathBuf>)>,
    kept_files: std::collections::HashSet<PathBuf>,
    retain_recovery: bool,
    staged_bytes: u64,
}

impl PackTransaction {
    pub fn new(root: &Path) -> Result<Self, ModpackError> {
        contained_join(root, ".")?;
        std::fs::create_dir_all(root)?;
        let stage = loop {
            let path = root.join(format!(
                ".waybound-pack-stage-{}-{}",
                std::process::id(), NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        };
        Ok(Self { root: root.to_path_buf(), stage, changes: Vec::new(), kept_files: std::collections::HashSet::new(), retain_recovery: false, staged_bytes: 0 })
    }

    fn destination(&self, relative: &str) -> Result<PathBuf, ModpackError> {
        let dest = contained_join(&self.root, relative)?;
        if dest == self.root {
            return Err(ModpackError::Other("empty pack destination".into()));
        }
        Ok(dest)
    }

    pub fn stage(&mut self, relative: &str, bytes: &[u8]) -> Result<(), ModpackError> {
        let dest = self.destination(relative)?;
        let slot = self.changes.iter().position(|(path, _)| path == &dest);
        let staged = slot.and_then(|index| self.changes[index].1.clone())
            .unwrap_or_else(|| self.stage.join(format!("new-{}", NEXT_STAGE.fetch_add(1, Ordering::Relaxed))));
        let previous_bytes = std::fs::metadata(&staged).map(|meta| meta.len()).unwrap_or(0);
        let staged_bytes = self.staged_bytes - previous_bytes + bytes.len() as u64;
        if bytes.len() > MAX_DOWNLOAD_BYTES || staged_bytes > MAX_PACK_BYTES || (slot.is_none() && self.changes.len() >= MAX_PACK_FILES) {
            return Err(ModpackError::Other("Pack exceeds file count or expanded size limit".into()));
        }
        atomic_write(&staged, bytes)?;
        self.staged_bytes = staged_bytes;
        if let Some(index) = slot {
            self.changes[index].1 = Some(staged);
        } else {
            self.changes.push((dest, Some(staged)));
        }
        Ok(())
    }

    pub fn stage_pack_file(&mut self, relative: &str, bytes: &[u8]) -> Result<bool, ModpackError> {
        validate_pack_path(relative)?;
        let dest = self.destination(relative)?;
        let normalized = relative.replace('\\', "/");
        let mut parts = normalized.split('/');
        let first = parts.next().unwrap_or("").to_ascii_lowercase();
        let content = matches!(first.as_str(), "mods" | "resourcepacks" | "shaderpacks");
        if !content && dest.exists() { return Ok(false); }
        if first == "saves" && parts.next().is_some_and(|world| self.root.join("saves").join(world).exists()) {
            return Ok(false);
        }
        let disabled = format!("{normalized}{}", crate::commands::content::DISABLED_SUFFIX);
        if content && !normalized.ends_with(crate::commands::content::DISABLED_SUFFIX)
            && self.destination(&disabled)?.is_file() {
            self.stage(&disabled, bytes)?;
        } else {
            self.stage(&normalized, bytes)?;
        }
        Ok(true)
    }

    /// Move an already verified, same-volume download into private staging.
    /// No large buffer or second byte copy; source is consumed on success.
    pub fn stage_file(&mut self, relative: &str, source: &Path) -> Result<(), ModpackError> {
        validate_pack_path(relative)?;
        crate::download::ensure_contained_path(&self.root, source)?;
        let meta = std::fs::symlink_metadata(source)?;
        if !meta.is_file() { return Err(ModpackError::Other("Staged download is not a file".into())); }
        let dest = self.destination(relative)?;
        let slot = self.changes.iter().position(|(path, _)| path == &dest);
        let staged = slot.and_then(|index| self.changes[index].1.clone())
            .unwrap_or_else(|| self.stage.join(format!("new-{}", NEXT_STAGE.fetch_add(1, Ordering::Relaxed))));
        let previous_bytes = std::fs::metadata(&staged).map(|meta| meta.len()).unwrap_or(0);
        let staged_bytes = self.staged_bytes - previous_bytes + meta.len();
        if meta.len() > MAX_DOWNLOAD_BYTES as u64 || staged_bytes > MAX_PACK_BYTES || (slot.is_none() && self.changes.len() >= MAX_PACK_FILES) {
            return Err(ModpackError::Other("Pack exceeds file count or expanded size limit".into()));
        }
        if staged.exists() { std::fs::remove_file(&staged)?; }
        std::fs::rename(source, &staged)?;
        self.staged_bytes = staged_bytes;
        if let Some(index) = slot { self.changes[index].1 = Some(staged); }
        else { self.changes.push((dest, Some(staged))); }
        Ok(())
    }

    pub fn preserve_disabled(&mut self, relative: &str) -> Result<(), ModpackError> {
        let enabled = self.destination(relative)?;
        let disabled = self.destination(&format!("{relative}{}", crate::commands::content::DISABLED_SUFFIX))?;
        if self.changes.iter().any(|(path, staged)| path == &enabled && staged.is_some()) {
            if self.changes.iter().any(|(path, staged)| path == &disabled && staged.is_some()) {
                return Err(ModpackError::Other("Pack has conflicting enabled and disabled destinations".into()));
            }
            self.changes.retain(|(path, staged)| path != &disabled || staged.is_some());
            let index = self.changes.iter().position(|(path, staged)| path == &enabled && staged.is_some()).unwrap();
            self.changes[index].0 = disabled;
        }
        Ok(())
    }

    /// An already verified file remains owned by the new pack, even if an
    /// old receipt or override list also claims its path.
    pub fn keep_file(&mut self, path: &Path) -> Result<(), ModpackError> {
        let relative = path.strip_prefix(&self.root)
            .map_err(|_| ModpackError::Other("retained file outside pack root".into()))?;
        let dest = self.destination(&relative.to_string_lossy())?;
        self.changes.retain(|(path, staged)| path != &dest || staged.is_some());
        self.kept_files.insert(dest);
        Ok(())
    }

    pub fn remove(&mut self, path: &Path) -> Result<(), ModpackError> {
        let relative = path.strip_prefix(&self.root)
            .map_err(|_| ModpackError::Other("removal outside pack root".into()))?;
        let dest = self.destination(&relative.to_string_lossy())?;
        // A replacement at the same path wins over removing its old file id.
        if !self.kept_files.contains(&dest) && !self.changes.iter().any(|(path, _)| path == &dest) {
            self.changes.push((dest, None));
        }
        Ok(())
    }

    pub async fn overrides(&mut self, bytes: &[u8], prefixes: &[&str], cancel: &CancelToken) -> Result<(u32, Vec<String>), ModpackError> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
        if archive.len() > MAX_PACK_FILES {
            return Err(ModpackError::Other("Pack has too many archive entries".into()));
        }
        let mut applied = 0;
        let mut paths = Vec::new();
        // Prefix order is deliberate: client overrides win over common files.
        for prefix in prefixes {
            let prefix = format!("{}/", prefix.trim_end_matches('/'));
            for i in 0..archive.len() {
                cancel.checkpoint().await?;
                let mut entry = archive.by_index(i)?;
                if entry.is_dir() { continue; }
                let Some(relative) = entry.name().strip_prefix(&prefix).map(str::to_owned) else { continue };
                if entry.size() > MAX_PACK_FILE_BYTES as u64 {
                    return Err(ModpackError::Other("Pack override exceeds expanded size limit".into()));
                }
                let expected = entry.size();
                let mut data = Vec::new();
                entry.by_ref().take(expected + 1).read_to_end(&mut data)?;
                if data.len() as u64 != expected {
                    return Err(ModpackError::Other("Pack override size disagrees with archive".into()));
                }
                if !self.stage_pack_file(&relative, &data)? { continue; }
                applied += 1;
                let normalized = relative.replace('\\', "/");
                if !paths.contains(&normalized) { paths.push(normalized); }
            }
        }
        Ok((applied, paths))
    }

    /// Remove only dropped tracked content. Preserved configs/worlds keep
    /// provenance, even when this update skipped their existing bytes.
    pub fn reconcile_overrides(&mut self, paths: &mut Vec<String>) -> Result<(), ModpackError> {
        let manifest = contained_join(&self.root, ".pack-overrides-manifest.json")?;
        let old: Vec<String> = match std::fs::read(manifest) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        for old in old {
            validate_pack_path(&old)?;
            let normalized = old.replace('\\', "/");
            if paths.contains(&normalized) { continue; }
            let path = self.destination(&normalized)?;
            let content = ["mods/", "resourcepacks/", "shaderpacks/"].iter()
                .any(|prefix| normalized.to_ascii_lowercase().starts_with(prefix));
            if content {
                if path.exists() { self.remove(&path)?; }
                let disabled = self.destination(&format!("{normalized}{}", crate::commands::content::DISABLED_SUFFIX))?;
                if disabled.exists() { self.remove(&disabled)?; }
            } else if path.is_file() { paths.push(normalized); }
        }
        Ok(())
    }

    pub async fn commit(self, cancel: &CancelToken) -> Result<(), ModpackError> {
        self.commit_with(cancel, || Ok(())).await
    }

    pub async fn commit_with(
        mut self,
        cancel: &CancelToken,
        publish: impl FnOnce() -> Result<(), ModpackError>,
    ) -> Result<(), ModpackError> {
        let mut applied: Vec<(PathBuf, Option<PathBuf>, bool)> = Vec::new();
        let result: Result<(), ModpackError> = async {
            for (index, (dest, staged)) in self.changes.iter().enumerate() {
                cancel.checkpoint().await?;
                let relative = dest.strip_prefix(&self.root).expect("validated destination");
                self.destination(&relative.to_string_lossy())?;
                if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent)?; }
                let backup = match std::fs::symlink_metadata(dest) {
                    Ok(meta) if meta.is_file() => {
                        let backup = self.stage.join(format!("old-{index}"));
                        std::fs::rename(dest, &backup)?;
                        Some(backup)
                    }
                    Ok(_) => return Err(ModpackError::Other(format!("pack destination is not a file: {}", dest.display()))),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e.into()),
                };
                applied.push((dest.clone(), backup, false));
                if let Some(staged) = staged {
                    std::fs::rename(staged, dest)?;
                    applied.last_mut().unwrap().2 = true;
                }
            }
            cancel.checkpoint().await?;
            publish()
        }.await;
        if let Err(error) = result {
            let mut rollback_errors = Vec::new();
            for (dest, backup, installed) in applied.into_iter().rev() {
                if installed {
                    if let Err(e) = std::fs::remove_file(&dest) {
                        rollback_errors.push(format!("{}: {e}", dest.display()));
                        continue;
                    }
                }
                if let Some(backup) = backup {
                    if let Err(e) = std::fs::rename(&backup, &dest) {
                        rollback_errors.push(format!("{}: {e}", dest.display()));
                    }
                }
            }
            if !rollback_errors.is_empty() {
                self.retain_recovery = true;
                return Err(ModpackError::Other(format!(
                    "{error}; rollback incomplete ({}). Original files retained in {}",
                    rollback_errors.join("; "), self.stage.display()
                )));
            }
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for PackTransaction {
    fn drop(&mut self) {
        if !self.retain_recovery { let _ = std::fs::remove_dir_all(&self.stage); }
    }
}

pub(super) fn validate_pack_path(relative: &str) -> Result<(), ModpackError> {
    let normalized = relative.replace('\\', "/");
    if normalized.split('/').any(|part| part.to_ascii_lowercase().starts_with(".waybound")
        || part.eq_ignore_ascii_case(".curseforge-pack-manifest.json")
        || part.eq_ignore_ascii_case(".modrinth-pack-manifest.json")
        || part.eq_ignore_ascii_case(".pack-overrides-manifest.json")) {
        return Err(DownloadError::UnsafePath(relative.into()).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("waybound-pack-transaction-{}-{}", std::process::id(), NEXT_STAGE.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[tokio::test]
    async fn late_commit_failure_restores_old_files_and_manifest() {
        let root = root();
        std::fs::write(root.join("mod.jar"), b"working").unwrap();
        std::fs::write(root.join("manifest.json"), b"old tracking").unwrap();
        std::fs::create_dir(root.join("blocked")).unwrap();
        let mut tx = PackTransaction::new(&root).unwrap();
        tx.stage("mod.jar", b"new").unwrap();
        tx.stage("added.jar", b"new file").unwrap();
        tx.stage("manifest.json", b"new tracking").unwrap();
        tx.stage("blocked", b"cannot replace directory").unwrap();
        assert!(tx.commit(&CancelToken::new()).await.is_err());
        assert_eq!(std::fs::read(root.join("mod.jar")).unwrap(), b"working");
        assert_eq!(std::fs::read(root.join("manifest.json")).unwrap(), b"old tracking");
        assert!(!root.join("added.jar").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancellation_discards_staging_without_replacing_old_bytes() {
        let root = root();
        std::fs::write(root.join("mod.jar"), b"working").unwrap();
        let mut tx = PackTransaction::new(&root).unwrap();
        tx.stage("mod.jar", b"new").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(tx.commit(&cancel).await, Err(ModpackError::Download(DownloadError::Cancelled))));
        assert_eq!(std::fs::read(root.join("mod.jar")).unwrap(), b"working");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn metadata_failure_rolls_back_replacements_removals_and_sidecars() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("working.jar"), b"old").unwrap();
        std::fs::write(root.path().join("dropped.jar"), b"drop").unwrap();
        std::fs::write(root.path().join(".waybound-pack-loader.json"), b"old loader").unwrap();
        let mut tx = PackTransaction::new(root.path()).unwrap();
        tx.stage("working.jar", b"new").unwrap();
        tx.stage("added.jar", b"new").unwrap();
        tx.remove(&root.path().join("dropped.jar")).unwrap();
        tx.stage(".waybound-pack-loader.json", b"new loader").unwrap();
        assert!(tx.commit_with(&CancelToken::new(), || Err(ModpackError::Other("metadata publish failed".into()))).await.is_err());
        assert_eq!(std::fs::read(root.path().join("working.jar")).unwrap(), b"old");
        assert_eq!(std::fs::read(root.path().join("dropped.jar")).unwrap(), b"drop");
        assert_eq!(std::fs::read(root.path().join(".waybound-pack-loader.json")).unwrap(), b"old loader");
        assert!(!root.path().join("added.jar").exists());
    }

    #[tokio::test]
    async fn replacement_wins_over_same_path_removal() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("same.jar"), b"old").unwrap();
        let mut tx = PackTransaction::new(root.path()).unwrap();
        tx.remove(&root.path().join("same.jar")).unwrap();
        tx.stage("same.jar", b"verified new").unwrap();
        tx.remove(&root.path().join("same.jar")).unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();
        assert_eq!(std::fs::read(root.path().join("same.jar")).unwrap(), b"verified new");
    }

    #[tokio::test]
    async fn disabled_redirection_cancels_prior_removal_of_that_path() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("mods")).unwrap();
        std::fs::write(root.path().join("mods/new.jar.disabled"), b"old").unwrap();
        let mut tx = PackTransaction::new(root.path()).unwrap();
        tx.stage("mods/new.jar", b"new").unwrap();
        tx.remove(&root.path().join("mods/new.jar.disabled")).unwrap();
        tx.preserve_disabled("mods/new.jar").unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();
        assert_eq!(std::fs::read(root.path().join("mods/new.jar.disabled")).unwrap(), b"new");
        assert!(!root.path().join("mods/new.jar").exists());
    }

    #[tokio::test]
    async fn pack_files_preserve_configs_existing_worlds_and_disabled_state() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("config")).unwrap();
        std::fs::create_dir_all(root.path().join("saves/world")).unwrap();
        std::fs::create_dir_all(root.path().join("mods")).unwrap();
        std::fs::write(root.path().join("config/user.toml"), b"user setting").unwrap();
        std::fs::write(root.path().join("saves/world/level.dat"), b"user world").unwrap();
        std::fs::write(root.path().join("mods/off.jar.disabled"), b"old").unwrap();
        let mut tx = PackTransaction::new(root.path()).unwrap();
        assert!(!tx.stage_pack_file("config/user.toml", b"pack setting").unwrap());
        assert!(!tx.stage_pack_file("saves/world/new.dat", b"pack world").unwrap());
        assert!(tx.stage_pack_file("config/new.toml", b"new config").unwrap());
        tx.stage_pack_file("mods/off.jar", b"new").unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();
        assert_eq!(std::fs::read(root.path().join("config/user.toml")).unwrap(), b"user setting");
        assert_eq!(std::fs::read(root.path().join("saves/world/level.dat")).unwrap(), b"user world");
        assert!(!root.path().join("saves/world/new.dat").exists());
        assert!(!root.path().join("mods/off.jar").exists());
        assert_eq!(std::fs::read(root.path().join("mods/off.jar.disabled")).unwrap(), b"new");
    }

    #[tokio::test]
    async fn dropped_override_content_removed_but_configs_and_worlds_retained() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["mods", "config", "saves/world"] { std::fs::create_dir_all(root.path().join(directory)).unwrap(); }
        for path in ["mods/old.jar.disabled", "config/user.toml", "saves/world/level.dat"] {
            std::fs::write(root.path().join(path), b"old").unwrap();
        }
        std::fs::write(root.path().join(".pack-overrides-manifest.json"),
            br#"["mods/old.jar","config/user.toml","saves/world/level.dat"]"#).unwrap();
        let mut tx = PackTransaction::new(root.path()).unwrap();
        let mut paths = Vec::new();
        tx.reconcile_overrides(&mut paths).unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();
        assert!(!root.path().join("mods/old.jar.disabled").exists());
        assert!(root.path().join("config/user.toml").exists());
        assert!(root.path().join("saves/world/level.dat").exists());
        assert!(paths.contains(&"config/user.toml".into()));
        assert!(paths.contains(&"saves/world/level.dat".into()));
    }

    #[tokio::test]
    async fn verified_existing_file_survives_overlapping_receipt_removals() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("mods")).unwrap();
        let path = root.path().join("mods/shared.jar.disabled");
        std::fs::write(&path, b"verified current").unwrap();
        std::fs::write(root.path().join(".pack-overrides-manifest.json"), br#"["mods/shared.jar"]"#).unwrap();
        let mut tx = PackTransaction::new(root.path()).unwrap();
        tx.remove(&path).unwrap();
        tx.keep_file(&path).unwrap();
        tx.reconcile_overrides(&mut Vec::new()).unwrap();
        tx.remove(&path).unwrap();
        tx.commit(&CancelToken::new()).await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"verified current");
    }

    #[tokio::test]
    async fn cancelled_removals_do_not_reuse_staged_sidecar_paths() {
        for use_download in [false, true] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("mods")).unwrap();
            let retained = root.path().join("mods/retained.jar");
            std::fs::write(&retained, b"retained").unwrap();
            let mut tx = PackTransaction::new(root.path()).unwrap();
            tx.stage("mods/new.jar", b"new").unwrap();
            tx.remove(&root.path().join("mods/new.jar.disabled")).unwrap();
            tx.remove(&retained).unwrap();
            tx.stage(".modrinth-pack-manifest.json", br#"{"files":[]}"#).unwrap();
            tx.stage(".pack-overrides-manifest.json", br#"["config/new.toml"]"#).unwrap();
            tx.preserve_disabled("mods/new.jar").unwrap();
            tx.keep_file(&retained).unwrap();
            if use_download {
                let download = root.path().join("download");
                std::fs::write(&download, br#"{"loader":"fabric"}"#).unwrap();
                tx.stage_file("loader.json", &download).unwrap();
            } else {
                tx.stage("loader.json", br#"{"loader":"fabric"}"#).unwrap();
            }
            // Replacing a sidecar after compaction must keep its own staging slot.
            tx.stage(".modrinth-pack-manifest.json", br#"{"files":["new.jar"]}"#).unwrap();
            tx.commit(&CancelToken::new()).await.unwrap();
            assert_eq!(std::fs::read(root.path().join("mods/new.jar.disabled")).unwrap(), b"new");
            assert_eq!(std::fs::read(&retained).unwrap(), b"retained");
            assert_eq!(std::fs::read(root.path().join(".modrinth-pack-manifest.json")).unwrap(), br#"{"files":["new.jar"]}"#);
            assert_eq!(std::fs::read(root.path().join("loader.json")).unwrap(), br#"{"loader":"fabric"}"#);
            let mut next = PackTransaction::new(root.path()).unwrap();
            let mut paths = Vec::new();
            next.reconcile_overrides(&mut paths).unwrap();
            assert!(paths.is_empty());
        }
    }
}
