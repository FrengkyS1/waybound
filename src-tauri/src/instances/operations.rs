use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

static ACTIVE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// Exclusive ownership of one instance, including its running game lifetime.
#[derive(Debug)]
pub struct OperationGuard {
    instance_id: String,
}

pub fn acquire(instance_id: &str) -> Result<OperationGuard, String> {
    let root = crate::instances::paths::instance_root(instance_id).map_err(|e| e.to_string())?;
    acquire_at(instance_id, &root)
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
    Ok(OperationGuard { instance_id: instance_id.to_string() })
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
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
}
