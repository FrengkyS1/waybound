use std::path::{Path, PathBuf};

use crate::sources::curseforge_key::{
    docker_env_format_warnings, read_curseforge_api_key_from_env_file,
};

const DOCKER_ENV_RELATIVE: &str = "docker/prominence2/.env";

pub struct DockerEnvReadResult {
    pub key: String,
    pub warnings: Vec<String>,
}

/// Default `.env` path next to `docker/prominence2/docker-compose.yml` (your compose template).
pub fn default_docker_env_path() -> Option<PathBuf> {
    for base in candidate_project_roots() {
        let path = base.join(DOCKER_ENV_RELATIVE);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

pub fn default_docker_env_path_string() -> Option<String> {
    default_docker_env_path().and_then(|path| path.into_os_string().into_string().ok())
}

fn candidate_project_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.clone());
        if let Some(parent) = cwd.parent() {
            roots.push(parent.to_path_buf());
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.to_path_buf());
            if let Some(parent) = dir.parent() {
                roots.push(parent.to_path_buf());
            }
        }
    }

    roots.sort();
    roots.dedup();
    roots
}

pub fn read_docker_env_key_from_path(path: &Path) -> Result<DockerEnvReadResult, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("Could not read `.env` file at {}: {err}", path.display()))?;
    let warnings = docker_env_format_warnings(&raw);
    let key = read_curseforge_api_key_from_env_file(&raw)?;
    Ok(DockerEnvReadResult { key, warnings })
}

#[cfg(test)]
mod tests {
    use super::DOCKER_ENV_RELATIVE;

    #[test]
    fn docker_env_relative_matches_compose_layout() {
        assert!(DOCKER_ENV_RELATIVE.ends_with("prominence2/.env"));
    }
}
