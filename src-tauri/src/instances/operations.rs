use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

static ACTIVE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// Instance mutation ownership until dropped or handed to a verified game marker.
#[derive(Debug)]
pub struct OperationGuard {
    instance_id: String,
    owns_mutation: bool,
}

pub fn acquire(instance_id: &str) -> Result<OperationGuard, String> {
    let root = crate::instances::paths::instance_root(instance_id).map_err(|e| e.to_string())?;
    acquire_at(instance_id, &root)
}

/// Config saves may coexist with Minecraft, but not preparation, installs or
/// another config save. The game phase no longer holds this mutation registry.
pub fn acquire_config_edit(instance_id: &str) -> Result<OperationGuard, String> {
    let root = crate::instances::paths::instance_root(instance_id).map_err(|e| e.to_string())?;
    acquire_config_at(instance_id, &root)
}

fn acquire_config_at(instance_id: &str, root: &std::path::Path) -> Result<OperationGuard, String> {
    let mut active = ACTIVE.lock().map_err(|_| "Instance operation registry is unavailable.".to_string())?;
    // Only a PID + creation-time marker can authorize coexistence with a game.
    // Malformed, unreadable and live legacy markers fail closed.
    crate::commands::launch::instance_process_running(root)?;
    if !active.insert(instance_id.to_string()) {
        return Err("This instance is busy installing, preparing, or being modified. Finish that operation first.".into());
    }
    Ok(OperationGuard { instance_id: instance_id.to_string(), owns_mutation: true })
}

fn acquire_at(instance_id: &str, root: &std::path::Path) -> Result<OperationGuard, String> {
    let mut active = ACTIVE.lock().map_err(|_| {
        "Instance operation registry is unavailable. Restart Waybound before modifying instances.".to_string()
    })?;
    // A game child left over from a previous Waybound session still owns
    // this instance's world/save files — reject before granting any
    // exclusive operation. Missing marker simply means not running; an
    // unreadable or malformed marker fails closed in the launch helper.
    if crate::commands::launch::instance_process_running(root)? {
        return Err("Minecraft is still running for this instance. Quit the game before modifying or launching it.".to_string());
    }
    if !active.insert(instance_id.to_string()) {
        return Err("This instance is busy installing, being modified, or running. Finish that operation first.".to_string());
    }
    Ok(OperationGuard { instance_id: instance_id.to_string(), owns_mutation: true })
}

impl OperationGuard {
    /// Called only after the verified game marker has been persisted.
    pub fn mark_running(&mut self) {
        if let Ok(mut active) = ACTIVE.lock() {
            active.remove(&self.instance_id);
            self.owns_mutation = false;
        }
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        if !self.owns_mutation { return; }
        if let Ok(mut active) = ACTIVE.lock() {
            active.remove(&self.instance_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::acquire_at;

    #[test]
    fn acquire_rejects_live_persisted_game_process() {
        let id = "operation-pid-live-test";
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        // Our own process id is trivially alive; with the marker present
        // acquire must refuse to grant exclusivity to anyone else.
        std::fs::write(root.join(crate::commands::launch::PID_FILE_NAME), std::process::id().to_string()).unwrap();
        let guard = acquire_at(id, root);
        assert!(guard.is_err(), "a live persisted game PID must block mutations");
        // Marker removed → nothing is running, so the same id becomes usable.
        std::fs::remove_file(root.join(crate::commands::launch::PID_FILE_NAME)).unwrap();
        let result = acquire_at(id, root);
        assert!(result.is_ok(), "acquire after marker removal failed: {:?}", result.err());
    }
    #[test]
    fn exclusive_until_guard_drops() {
        let id = "operation-exclusive-test";
        let directory = tempfile::tempdir().unwrap();
        let first = acquire_at(id, directory.path()).unwrap();
        assert!(acquire_at(id, directory.path()).is_err());
        drop(first);
        assert!(acquire_at(id, directory.path()).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn verified_game_allows_config_but_prep_install_world_and_other_saves_are_excluded() {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let mut created: FILETIME = unsafe { std::mem::zeroed() };
        let mut exited = created;
        let mut kernel = created;
        let mut user = created;
        assert_ne!(unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }, 0);
        let identity = crate::commands::launch::ProcessIdentity {
            pid: std::process::id(),
            created: ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64,
        };
        let id = "operation-game-config-test";
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let marker = root.join(crate::commands::launch::PID_FILE_NAME);
        let mut game = acquire_at(id, root).unwrap();
        assert!(super::acquire_config_at(id, root).is_err(), "preparation excludes config saves");
        std::fs::write(&marker, serde_json::to_vec(&identity).unwrap()).unwrap();
        game.mark_running();
        assert!(acquire_at(id, root).is_err(), "verified running game excludes destructive/world writes");
        let config = super::acquire_config_at(id, root).unwrap();
        assert!(super::acquire_config_at(id, root).is_err(), "config saves remain exclusive");
        drop(game);
        assert!(super::acquire_config_at(id, root).is_err(), "old game guard cannot release new save");
        std::fs::remove_file(&marker).unwrap();
        assert!(acquire_at(id, root).is_err(), "config save excludes prep/install after game exit");
        drop(config);
        assert!(acquire_at(id, root).is_ok());
    }

    #[test]
    fn config_save_rejects_unverified_legacy_running_marker() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(crate::commands::launch::PID_FILE_NAME), std::process::id().to_string()).unwrap();
        assert!(super::acquire_config_at("legacy-config-save", directory.path()).is_err());
    }
}
