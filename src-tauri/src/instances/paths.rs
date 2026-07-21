use std::path::PathBuf;
use thiserror::Error;

use crate::download::safe_join;

const APP_DIR: &str = "dev.waybound";

#[derive(Debug, Error)]
pub enum PathError {
    #[error("could not resolve data directory")]
    NoDataDir,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid instance id: {0}")]
    UnsafeInstanceId(String),
}

pub fn app_data_dir() -> Result<PathBuf, PathError> {
    Ok(dirs::data_dir()
        .ok_or(PathError::NoDataDir)?
        .join(APP_DIR))
}

pub fn instances_root() -> Result<PathBuf, PathError> {
    Ok(app_data_dir()?.join("instances"))
}

// `instance_id` reaches every content/instance command straight from the
// frontend's `invoke()` call, so it's treated as untrusted input here too —
// a `..`-laced id must not be able to point outside `instances/`.
pub fn instance_root(instance_id: &str) -> Result<PathBuf, PathError> {
    safe_join(&instances_root()?, instance_id)
        .map_err(|_| PathError::UnsafeInstanceId(instance_id.to_string()))
}

pub fn instance_mods_dir(instance_id: &str) -> Result<PathBuf, PathError> {
    Ok(instance_root(instance_id)?.join("mods"))
}

pub fn ensure_instance_dirs(instance_id: &str) -> Result<PathBuf, PathError> {
    let mods_dir = instance_mods_dir(instance_id)?;
    std::fs::create_dir_all(&mods_dir)?;
    Ok(mods_dir)
}
