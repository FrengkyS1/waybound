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
    // `safe_join` correctly refuses to escape the base, but an id with no
    // normal components at all (`""`, `"."`, `"./"`) resolves *to* the base:
    // `instances/` itself. That's contained, yet still the wrong scope —
    // it would aim a per-instance operation (mods listing, dir creation,
    // deletion) at the folder holding every instance. Reject it here rather
    // than rely on each caller happening to pass a non-empty id.
    if instance_id
        .split(|c| c == '/' || c == '\\')
        .all(|part| part.is_empty() || part == ".")
    {
        return Err(PathError::UnsafeInstanceId(instance_id.to_string()));
    }
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

#[cfg(test)]
mod instance_id_containment_tests {
    use super::*;

    // Every assertion below is a pure path computation. Nothing in this module
    // writes to disk, so these never touch the real `dev.waybound` data
    // directory even though that is the base they resolve against.

    /// Ids that must never resolve to a path — each one would otherwise let a
    /// frontend `invoke("delete_content_file", { instanceId: ... })` reach
    /// outside `instances/`. The backslash/drive forms only parse as paths on
    /// Windows, so they are only asserted there.
    fn hostile_ids() -> Vec<&'static str> {
        let mut ids = vec![
            "..",
            "../..",
            "../../../../Windows/System32",
            "mods/../../evil",
            "good/../../../evil",
            "/etc/passwd",
            "//server/share",
        ];
        if cfg!(windows) {
            ids.extend([
                "..\\..",
                "..\\..\\..\\Windows",
                "\\Windows\\System32",
                "C:\\Windows",
                "C:/Windows/System32",
                "\\\\server\\share",
            ]);
        }
        ids
    }

    #[test]
    fn a_plain_id_resolves_directly_under_the_instances_root() {
        let root = instances_root().unwrap();
        assert_eq!(
            instance_root("6f1c2b3a").unwrap(),
            root.join("6f1c2b3a")
        );
    }

    #[test]
    fn mods_dir_is_the_mods_folder_of_the_instance_root() {
        assert_eq!(
            instance_mods_dir("abc").unwrap(),
            instance_root("abc").unwrap().join("mods")
        );
    }

    #[test]
    fn instances_root_lives_under_the_app_data_dir() {
        assert_eq!(instances_root().unwrap(), app_data_dir().unwrap().join("instances"));
        assert!(app_data_dir().unwrap().ends_with("dev.waybound"));
    }

    #[test]
    fn traversal_and_absolute_instance_ids_are_rejected() {
        for id in hostile_ids() {
            let result = instance_root(id);
            assert!(
                matches!(result, Err(PathError::UnsafeInstanceId(_))),
                "instance_root({id:?}) should have been rejected, got {result:?}"
            );
            assert!(instance_mods_dir(id).is_err(), "instance_mods_dir({id:?}) leaked through");
        }
    }

    #[test]
    fn ensure_instance_dirs_rejects_a_hostile_id_before_creating_anything() {
        for id in hostile_ids() {
            // The id is validated by `instance_mods_dir` first, so this never
            // reaches `create_dir_all` — no directory is created anywhere.
            assert!(
                matches!(ensure_instance_dirs(id), Err(PathError::UnsafeInstanceId(_))),
                "ensure_instance_dirs({id:?}) should refuse before touching the filesystem"
            );
        }
    }

    #[test]
    fn a_rejected_id_is_echoed_back_in_the_error_for_the_activity_log() {
        match instance_root("../escape") {
            Err(PathError::UnsafeInstanceId(id)) => assert_eq!(id, "../escape"),
            other => panic!("expected UnsafeInstanceId, got {other:?}"),
        }
    }

    #[test]
    fn embedded_separators_nest_inside_the_root_rather_than_escaping_it() {
        // Documenting actual behavior: an id containing separators is allowed
        // and simply becomes a nested path. It stays contained, which is what
        // matters, but it is not rejected either.
        let root = instances_root().unwrap();
        assert_eq!(instance_root("a/b").unwrap(), root.join("a").join("b"));
        assert!(instance_root("a/b").unwrap().starts_with(&root));
        #[cfg(windows)]
        assert_eq!(instance_root("a\\b").unwrap(), root.join("a").join("b"));
    }

    #[test]
    fn a_trailing_or_leading_dot_segment_is_dropped_not_rejected() {
        let root = instances_root().unwrap();
        assert_eq!(instance_root("./abc").unwrap(), root.join("abc"));
        assert_eq!(instance_root("abc/.").unwrap(), root.join("abc"));
    }

    #[test]
    fn an_id_with_no_real_components_is_rejected_not_collapsed_onto_the_root() {
        // `safe_join` alone yields the base unchanged for an id with no
        // normal components, so these used to resolve to `instances/`
        // itself — aiming a per-instance operation at the folder holding
        // every instance (`ensure_instance_dirs("")` would have created
        // `instances/mods`). Contained, but the wrong scope, so they're
        // refused outright.
        for id in ["", ".", "./", "/", ".//./", "\\"] {
            assert!(
                matches!(instance_root(id), Err(PathError::UnsafeInstanceId(_))),
                "id {id:?} should be rejected"
            );
        }
        assert!(instance_mods_dir("").is_err());
        assert!(ensure_instance_dirs("").is_err());
    }

    #[test]
    fn a_name_with_a_colon_is_not_treated_as_a_drive_prefix() {
        // Only a single-letter drive spec is a Windows path prefix; anything
        // else stays a normal component and remains inside the root.
        let root = instances_root().unwrap();
        let joined = instance_root("weird:name").unwrap();
        assert!(joined.starts_with(&root), "{joined:?} escaped {root:?}");
    }
}
