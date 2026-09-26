use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::ModpackError;
use crate::download::{atomic_write, safe_join, CancelToken, DownloadError};

static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);

/// Files and the tracking sidecar share one rollback boundary. Nothing in the
/// instance is replaced until every download and override has been staged.
pub(super) struct PackTransaction {
    root: PathBuf,
    stage: PathBuf,
    changes: Vec<(PathBuf, Option<PathBuf>)>,
    retain_recovery: bool,
}

impl PackTransaction {
    pub fn new(root: &Path) -> Result<Self, ModpackError> {
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
        Ok(Self { root: root.to_path_buf(), stage, changes: Vec::new(), retain_recovery: false })
    }

    fn destination(&self, relative: &str) -> Result<PathBuf, ModpackError> {
        let dest = safe_join(&self.root, relative)?;
        if dest == self.root {
            return Err(ModpackError::Other("empty pack destination".into()));
        }
        // Lexical containment alone does not protect user-created links/junctions.
        let mut path = Some(dest.as_path());
        while let Some(current) = path {
            match std::fs::symlink_metadata(current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(ModpackError::Other(format!("pack destination is a link: {}", current.display())));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            if current == self.root { break; }
            path = current.parent();
        }
        Ok(dest)
    }

    pub fn stage(&mut self, relative: &str, bytes: &[u8]) -> Result<(), ModpackError> {
        let dest = self.destination(relative)?;
        let slot = self.changes.iter().position(|(path, _)| path == &dest);
        let index = slot.unwrap_or(self.changes.len());
        let staged = self.stage.join(format!("new-{index}"));
        atomic_write(&staged, bytes)?;
        if let Some(index) = slot {
            self.changes[index].1 = Some(staged);
        } else {
            self.changes.push((dest, Some(staged)));
        }
        Ok(())
    }

    pub fn remove(&mut self, path: &Path) -> Result<(), ModpackError> {
        let relative = path.strip_prefix(&self.root)
            .map_err(|_| ModpackError::Other("removal outside pack root".into()))?;
        let dest = self.destination(&relative.to_string_lossy())?;
        // A replacement at the same path wins over removing its old file id.
        if !self.changes.iter().any(|(path, _)| path == &dest) {
            self.changes.push((dest, None));
        }
        Ok(())
    }

    pub async fn overrides(&mut self, bytes: &[u8], prefixes: &[&str], cancel: &CancelToken) -> Result<(u32, Vec<String>), ModpackError> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
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
                validate_pack_path(&relative)?;
                let dest = self.destination(&relative)?;
                let normalized = relative.replace('\\', "/");
                let mut parts = normalized.split('/');
                let first = parts.next().unwrap_or("").to_ascii_lowercase();
                let content = matches!(first.as_str(), "mods" | "resourcepacks" | "shaderpacks");
                // Existing configuration/settings are user-owned. Never merge a
                // shipped world into an existing save, even at a new file path.
                if !content && dest.exists() { continue; }
                if first == "saves" && parts.next().is_some_and(|world| self.root.join("saves").join(world).exists()) {
                    continue;
                }
                let mut data = Vec::new();
                entry.read_to_end(&mut data)?;
                self.stage(&relative, &data)?;
                applied += 1;
                paths.push(normalized);
            }
        }
        Ok((applied, paths))
    }

    pub async fn commit(mut self, cancel: &CancelToken) -> Result<(), ModpackError> {
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
            Ok(())
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
    if normalized.split('/').any(|part| part.starts_with(".waybound") || part.eq_ignore_ascii_case(".curseforge-pack-manifest.json") || part.eq_ignore_ascii_case(".modrinth-pack-manifest.json")) {
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
}
