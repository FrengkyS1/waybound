//! Persisted process identity: PID alone is never authority to terminate.
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

static MARKERS: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub created: u64,
}

pub fn read_marker(path: &Path) -> Result<Option<ProcessIdentity>, String> {
    let _guard = MARKERS.lock().map_err(|_| "Launch marker registry is unavailable".to_string())?;
    read_marker_unlocked(path)
}

fn read_marker_unlocked(path: &Path) -> Result<Option<ProcessIdentity>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if let Ok(identity) = serde_json::from_str::<ProcessIdentity>(&raw) {
        if identity.pid != 0 && identity.created != 0 { return Ok(Some(identity)); }
    }
    if let Ok(pid) = raw.trim().parse::<u32>() {
        // Legacy markers lack birth identity. Block mutation while PID exists,
        // but never adopt it or grant permission to stop an unrelated process.
        if current_identity(pid).map_err(|e| e.to_string())?.is_some() {
            return Err("Legacy launch marker cannot verify Minecraft identity. Quit that game, then retry; no process will be stopped from this marker.".into());
        }
        let _ = std::fs::remove_file(path);
        return Ok(None);
    }
    Err(format!("Corrupted launch marker in {}. Resolve the marker before modifying this instance.", path.display()))
}

#[cfg(windows)]
mod platform {
    use super::ProcessIdentity;
    use std::io;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, PROCESS_SYNCHRONIZE};
    struct Handle(HANDLE);
    impl Drop for Handle { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }
    fn open(pid: u32, rights: u32) -> io::Result<Option<Handle>> {
        let handle = unsafe { OpenProcess(rights, 0, pid) };
        if handle.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(87) { return Ok(None); }
            return Err(error);
        }
        Ok(Some(Handle(handle)))
    }
    fn identity(handle: &Handle, pid: u32) -> io::Result<Option<ProcessIdentity>> {
        match unsafe { WaitForSingleObject(handle.0, 0) } {
            WAIT_OBJECT_0 => return Ok(None),
            WAIT_TIMEOUT => {},
            _ => return Err(io::Error::last_os_error()),
        }
        let mut creation: FILETIME = unsafe { std::mem::zeroed() };
        let mut exit = creation;
        let mut kernel = creation;
        let mut user = creation;
        if unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(ProcessIdentity { pid, created: ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64 }))
    }
    pub fn current(pid: u32) -> io::Result<Option<ProcessIdentity>> {
        let Some(handle) = open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)? else { return Ok(None) };
        identity(&handle, pid)
    }
    pub fn child_identity(child: &std::process::Child) -> io::Result<Option<ProcessIdentity>> {
        use std::os::windows::io::AsRawHandle;
        // Borrow actual spawned process object, never reopen a possibly reused
        // PID while recording ownership. Child retains sole handle ownership.
        let borrowed = std::mem::ManuallyDrop::new(Handle(child.as_raw_handle()));
        identity(&borrowed, child.id())
    }
    pub fn stop(expected: ProcessIdentity) -> io::Result<()> {
        let Some(handle) = open(expected.pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE)? else { return Ok(()) };
        match identity(&handle, expected.pid)? {
            None => return Ok(()),
            Some(found) if found == expected => {},
            Some(_) => return Err(io::Error::other("Process identity changed; refusing to stop reused PID")),
        }
        // Terminate through the same verified kernel handle, never taskkill /T.
        if unsafe { TerminateProcess(handle.0, 1) } == 0 { return Err(io::Error::last_os_error()); }
        if unsafe { WaitForSingleObject(handle.0, u32::MAX) } != WAIT_OBJECT_0 { return Err(io::Error::last_os_error()); }
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::ProcessIdentity;
    use std::io;
    pub fn current(pid: u32) -> io::Result<Option<ProcessIdentity>> {
        #[cfg(target_os = "linux")]
        {
            let text = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(text) => text,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            let fields = text.rsplit_once(')').ok_or_else(|| io::Error::other("Invalid process stat"))?.1;
            let mut fields = fields.split_whitespace();
            if fields.next() == Some("Z") { return Ok(None); }
            let created = fields.nth(18).ok_or_else(|| io::Error::other("Missing process birth time"))?.parse().map_err(io::Error::other)?;
            return Ok(Some(ProcessIdentity { pid, created }));
        }
        #[cfg(not(target_os = "linux"))]
        { let _ = pid; Err(io::Error::other("Verified process identity is unavailable on this platform")) }
    }
    pub fn child_identity(child: &std::process::Child) -> io::Result<Option<ProcessIdentity>> {
        current(child.id())
    }
    pub fn stop(_expected: ProcessIdentity) -> io::Result<()> {
        // No PID-only kill fallback: platforms need an identity-bound handle.
        Err(io::Error::other("Safe adopted-process termination is unavailable on this platform"))
    }
}

pub use platform::{child_identity, current as current_identity, stop};
pub fn matches(identity: ProcessIdentity) -> Result<bool, String> {
    current_identity(identity.pid).map(|current| current == Some(identity)).map_err(|e| e.to_string())
}
pub fn write_marker(path: &Path, identity: ProcessIdentity) -> Result<(), String> {
    let _guard = MARKERS.lock().map_err(|_| "Launch marker registry is unavailable".to_string())?;
    let bytes = serde_json::to_vec(&identity).map_err(|e| e.to_string())?;
    crate::download::atomic_write(path, &bytes).map_err(|e| e.to_string())
}

pub fn remove_marker_if_owned(path: &Path, identity: ProcessIdentity) {
    let Ok(_guard) = MARKERS.lock() else { return };
    if read_marker_unlocked(path).ok().flatten() == Some(identity) { let _ = std::fs::remove_file(path); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn current_process_matches_only_its_recorded_birth() {
        let identity = current_identity(std::process::id()).unwrap().unwrap();
        assert!(matches(identity).unwrap());
        let reused = ProcessIdentity { created: identity.created.wrapping_add(1), ..identity };
        assert!(!matches(reused).unwrap());
    }
    #[cfg(windows)]
    #[test]
    fn birth_mismatch_never_stops_isolated_child() {
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
        }
        // Test binary itself waits in an ignored worker test: no unrelated
        // process, shell tree or real game is touched by this regression.
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "commands::launch::process::tests::isolated_identity_worker"])
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn().unwrap();
        let mut child = ChildGuard(child);
        let actual = current_identity(child.0.id()).unwrap().unwrap();
        let wrong = ProcessIdentity { created: actual.created.wrapping_add(1), ..actual };
        assert!(!matches(wrong).unwrap());
        assert!(stop(wrong).is_err());
        assert!(child.0.try_wait().unwrap().is_none());
        stop(actual).unwrap();
        assert!(child.0.wait().is_ok());
        assert!(!matches(actual).unwrap());
    }

    #[test]
    #[ignore = "isolated subprocess worker"]
    fn isolated_identity_worker() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
    #[test]
    fn legacy_live_marker_blocks_without_adoption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pid");
        std::fs::write(&path, std::process::id().to_string()).unwrap();
        assert!(read_marker(&path).unwrap_err().contains("Legacy launch marker"));
    }
    #[test]
    fn reaper_cannot_remove_new_run_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pid");
        let old = ProcessIdentity { pid: 10, created: 20 };
        let new = ProcessIdentity { pid: 10, created: 21 };
        write_marker(&path, new).unwrap();
        remove_marker_if_owned(&path, old);
        assert_eq!(read_marker(&path).unwrap(), Some(new));
    }
}
