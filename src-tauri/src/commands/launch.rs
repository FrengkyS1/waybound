//! Tauri commands for preparing files and launching an instance.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::auth::microsoft::{complete_minecraft_login, refresh_msa_token};
use crate::auth::Account;
use crate::dto::instance::InstanceLaunchConfig;
use crate::launch::java::{detect_java_runtimes, JavaRuntime};
use crate::launch::{prepare_launch, split_jvm_args, ProgressUpdate};

use super::search::AppState;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Marker file dropped in an instance's own game directory while its Java
/// child process is alive. The instance's `AppState.launches` entry only
/// covers the prepare/download phase and is gone within moments of the game
/// actually starting (see `launch_instance`), and the whole map lives in this
/// process's memory — closing Waybound while Minecraft keeps running (it's an
/// independent OS process) loses all trace of it, so the Play button resets
/// to launchable on the next start and lets a second copy be launched
/// concurrently onto the same world files. This file plus [`is_pid_alive`] is
/// what `get_running_instances` checks on startup to rebuild that state.
pub(crate) const PID_FILE_NAME: &str = ".waybound-pid";

/// Where a run's captured stdout/stderr is persisted, relative to the
/// instance directory. Deliberately NOT `logs/latest.log`: the game's own
/// working directory is the instance directory, so Minecraft already owns
/// that name and we'd be fighting it for the same file.
const LOG_FILE_NAME: &str = "logs/waybound-latest.log";
const PREV_LOG_FILE_NAME: &str = "logs/waybound-previous.log";

/// A single spammy mod can print without pause for as long as the game runs;
/// past this the file stops growing (one final note is written) rather than
/// eating the disk.
const LOG_FILE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Lines kept in memory for crash analysis and for `read_launch_log`. The
/// mod-loader error block is printed within the last few dozen lines of a
/// failed start, so this is generous.
const LOG_TAIL_LINES: usize = 500;

/// The destination for every captured log line: the on-disk file plus a
/// bounded tail kept in memory for crash analysis. Shared by the stdout and
/// stderr reader threads, so both streams land in one interleaved file in the
/// order they actually arrived.
struct LogSink {
    file: Option<std::fs::File>,
    written: u64,
    tail: VecDeque<String>,
}

impl LogSink {
    fn push(&mut self, line: &str) {
        // Take the file out so a write failure or the size cap can simply
        // drop it: from then on this run only keeps the in-memory tail, and
        // logging never becomes a reason a launch misbehaves.
        if let Some(mut file) = self.file.take() {
            if writeln!(file, "{line}").is_ok() {
                self.written += line.len() as u64 + 1;
                if self.written < LOG_FILE_MAX_BYTES {
                    self.file = Some(file);
                } else {
                    let _ = writeln!(
                        file,
                        "[waybound] log size cap reached, later output is not being saved to disk"
                    );
                }
            }
        }
        if self.tail.len() == LOG_TAIL_LINES {
            self.tail.pop_front();
        }
        self.tail.push_back(line.to_string());
    }
}

/// Rotate the previous run's log aside and open a fresh one, mirroring how
/// Minecraft keeps a `latest.log` plus one older copy. Best-effort: if the
/// directory or file can't be created the launch proceeds with in-memory
/// logging only, exactly as it did before.
fn open_log_sink(instance_dir: &Path) -> LogSink {
    let latest = instance_dir.join(LOG_FILE_NAME);
    let file = latest.parent().and_then(|dir| {
        std::fs::create_dir_all(dir).ok()?;
        let _ = std::fs::rename(&latest, instance_dir.join(PREV_LOG_FILE_NAME));
        std::fs::File::create(&latest).ok()
    });
    LogSink {
        file,
        written: 0,
        tail: VecDeque::new(),
    }
}

/// The persisted log of an instance's most recent run, newest lines last.
/// Lets the Logs tab show what happened before the app was last closed
/// instead of starting empty. An unreadable or absent file is simply "no
/// stored log", not an error the UI has to handle.
#[tauri::command]
pub fn read_launch_log(instance_id: String) -> Result<Vec<String>, String> {
    let dir = crate::instances::paths::instance_root(&instance_id).map_err(|e| e.to_string())?;
    let Ok(text) = std::fs::read_to_string(dir.join(LOG_FILE_NAME)) else {
        return Ok(Vec::new());
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(LOG_TAIL_LINES);
    Ok(lines[start..].iter().map(|l| l.to_string()).collect())
}

/// Whether a process with this PID is still alive. Shells out to `tasklist`
/// rather than adding a process-inspection crate: this project already treats
/// a Windows-only helper process as an acceptable answer elsewhere (e.g. Java
/// version probing), and this is a startup-only, once-per-instance check.
#[cfg(windows)]
fn is_pid_alive(pid: u32) -> bool {    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(out) => String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()),
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn is_pid_alive(pid: u32) -> bool {
    // Signal 0 sends no signal but still errors if the process doesn't exist
    // or isn't ours; `kill` is universally available without a crate.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Force-kills a game process for `stop_game`. Same no-new-crate policy as
/// `is_pid_alive` above; hard kill, like Prism — there is no graceful
/// "please save and quit" channel to a running game.
#[cfg(windows)]
fn kill_pid(pid: u32) -> std::io::Result<()> {
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F", "/T"])
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| std::io::Error::other(format!("taskkill exited {status}")))
}

#[cfg(not(windows))]
fn kill_pid(pid: u32) -> std::io::Result<()> {
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| std::io::Error::other(format!("kill exited {status}")))
}

/// A Minecraft process detected as still running from a previous Waybound
/// session, so the frontend can restore its Play button to the disabled
/// "Running" state instead of allowing a second concurrent launch.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningInstance {
    pub instance_id: String,
    pub instance_name: String,
}

/// Called once when the frontend starts up. Scans every instance for a
/// leftover PID file and checks whether that process is genuinely still
/// alive (a crash or forced kill can leave a stale file behind). Anything
/// still alive gets a watcher thread so it's cleaned up and reported via the
/// normal `launch://exited` event once the player actually closes it, exactly
/// like a launch started in this session.
#[tauri::command]
pub fn get_running_instances(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<RunningInstance>, String> {
    let instances = state.db.list_instances().map_err(|e| e.to_string())?;
    let mut running = Vec::new();
    for inst in instances {
        let Ok(dir) = crate::instances::paths::instance_root(&inst.id) else {
            continue;
        };
        let pid_file = dir.join(PID_FILE_NAME);
        let Ok(contents) = std::fs::read_to_string(&pid_file) else {
            continue;
        };
        let Ok(pid) = contents.trim().parse::<u32>() else {
            let _ = std::fs::remove_file(&pid_file);
            continue;
        };
        if is_pid_alive(pid) {
            spawn_orphan_watcher(app.clone(), inst.id.clone(), pid_file.clone());
            running.push(RunningInstance {
                instance_id: inst.id,
                instance_name: inst.name,
            });
        } else {
            let _ = std::fs::remove_file(&pid_file);
        }
    }
    Ok(running)
}

/// Poll until a previous session's Minecraft process finally exits, then
/// clean up its PID file and emit the same `launch://exited` event a launch
/// started in this session would, so the frontend flips back to launchable
/// through its existing listener with no new event type to handle.
fn spawn_orphan_watcher(app: AppHandle, instance_id: String, pid_file: std::path::PathBuf) {
    std::thread::spawn(move || {
        let pid: Option<u32> = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse().ok());
        let Some(pid) = pid else { return };
        while is_pid_alive(pid) {
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        let _ = std::fs::remove_file(&pid_file);
        let user_stopped = app
            .state::<AppState>()
            .stop_requests
            .lock()
            .map(|mut set| set.remove(&instance_id))
            .unwrap_or(false);
        let _ = app.emit(
            "launch://exited",
            LaunchExitedEvent {
                instance_id,
                code: None,
                // A process adopted from a previous session was never ours to
                // read output from, so there's nothing to diagnose.
                crashed: false,
                crash_reason: None,
                stopped_by_user: user_stopped,
            },
        );
    });
}

/// Kills a running game: the live PID from this session when present, else
/// the PID file (covers games adopted from a previous session). Records a
/// stop request first so whichever reaper notices reports "stopped" rather
/// than "crashed". No-op when nothing is running — the desired state
/// already holds, so there is nothing to report as an error.
#[tauri::command]
pub fn stop_game(state: State<'_, AppState>, instance_id: String) -> Result<(), String> {
    let pid = state
        .game_pids
        .lock()
        .map_err(|e| e.to_string())?
        .get(&instance_id)
        .copied()
        .or_else(|| {
            let root = crate::instances::paths::instance_root(&instance_id).ok()?;
            std::fs::read_to_string(root.join(PID_FILE_NAME))
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
        });
    let Some(pid) = pid else {
        return Ok(());
    };
    if let Ok(mut stops) = state.stop_requests.lock() {
        stops.insert(instance_id.clone());
    }
    if let Err(err) = kill_pid(pid) {
        // Lost the race with a natural exit: the reaper reports it normally,
        // so withdraw the stop request rather than mislabel a future run.
        if is_pid_alive(pid) {
            if let Ok(mut stops) = state.stop_requests.lock() {
                stops.remove(&instance_id);
            }
            return Err(format!("Couldn't stop the game: {err}"));
        }
        if let Ok(mut stops) = state.stop_requests.lock() {
            stops.remove(&instance_id);
        }
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSettings {
    pub detected: Vec<JavaRuntime>,
    pub java_path: Option<String>,
    pub max_memory_mb: u32,
    pub jvm_args: Option<String>,
}

#[tauri::command]
pub fn list_java_runtimes() -> Vec<JavaRuntime> {
    detect_java_runtimes()
}

#[tauri::command]
pub fn get_launch_settings(state: State<'_, AppState>) -> LaunchSettings {
    LaunchSettings {
        detected: detect_java_runtimes(),
        java_path: state.config.java_path(),
        max_memory_mb: state.config.max_memory_mb(),
        jvm_args: state.config.jvm_args(),
    }
}

#[tauri::command]
pub fn set_launch_settings(
    state: State<'_, AppState>,
    java_path: Option<String>,
    max_memory_mb: Option<u32>,
    jvm_args: Option<String>,
) -> Result<(), String> {
    state
        .config
        .set_launch_settings(java_path, max_memory_mb, jvm_args)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_instance_launch_config(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<InstanceLaunchConfig, String> {
    state
        .db
        .get_instance_launch_config(&instance_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_instance_launch_config(
    state: State<'_, AppState>,
    instance_id: String,
    config: InstanceLaunchConfig,
) -> Result<(), String> {
    state
        .db
        .set_instance_launch_config(&instance_id, &config)
        .map_err(|e| e.to_string())
}

/// Add elapsed play time (seconds) to an instance's running total.
#[tauri::command]
pub fn add_play_time(
    state: State<'_, AppState>,
    instance_id: String,
    seconds: u64,
) -> Result<(), String> {
    state
        .db
        .add_play_time(&instance_id, seconds)
        .map_err(|e| e.to_string())
}

/// Refresh only persisted authenticated identities. Outages preserve the saved
/// account; invalid credentials and ownership denials still require sign-in.
async fn ensure_account(
    state: &AppState,
    client: &reqwest::Client,
) -> Result<Account, String> {
    let account = state
        .config
        .account()
        .ok_or_else(|| "Sign in with your Microsoft account before playing.".to_string())?;
    if !saved_identity_is_valid(&account) {
        return Err("Sign in with an owned Minecraft: Java Edition account before playing.".into());
    }

    if !account.is_token_expired() {
        return Ok(account);
    }

    let refresh = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let (access, refresh, _) = refresh_msa_token(client, &account.msa_refresh_token).await?;
        complete_minecraft_login(client, &access, refresh).await
    }).await;
    let fresh = match refresh {
        Ok(Ok(fresh)) => fresh,
        Ok(Err(error)) if error.is_transient() => return Ok(account),
        Err(_) => return Ok(account),
        Ok(Err(error)) => return Err(format!("Sign in again before playing: {error}")),
    };

    state
        .config
        .set_account(Some(fresh.clone()))
        .map_err(|e| e.to_string())?;
    Ok(fresh)
}

fn saved_identity_is_valid(account: &Account) -> bool {
    account.uuid.len() == 32 && account.uuid.bytes().all(|b| b.is_ascii_hexdigit())
        && !account.username.is_empty() && !account.minecraft_token.is_empty()
        && !account.msa_refresh_token.is_empty() && account.expires_at > 0
}

/// Check the real profile immediately before spawning. A 401/403/404 is never
/// treated as offline; only transport interruption, 429 and 5xx permit fallback.
async fn revalidate_account(client: &reqwest::Client, account: &Account) -> Result<(), String> {
    let response = client.get("https://api.minecraftservices.com/minecraft/profile")
        .timeout(std::time::Duration::from_secs(8))
        .bearer_auth(&account.minecraft_token).send().await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            if crate::auth::microsoft::AuthError::Network(error).is_transient() {
                return Ok(());
            }
            return Err("Could not validate your account. Please retry sign-in.".into());
        }
    };
    let status = response.status();
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(());
    }
    if !status.is_success() {
        return Err("Your Minecraft session is no longer valid. Sign in again before playing.".into());
    }
    #[derive(serde::Deserialize)]
    struct Profile { id: String }
    let profile: Profile = response.json().await
        .map_err(|_| "Could not verify the Minecraft profile. Retry when the account service is available.".to_string())?;
    if profile.id != account.uuid {
        return Err("Minecraft account changed. Sign in again before playing.".into());
    }
    Ok(())
}

/// Whether a persisted PID marker describes a genuinely running game child.
/// An unreadable or malformed marker fails closed: the caller must not allow
/// a second Minecraft process onto the same world files based on nothing.
pub(crate) fn instance_process_running(instance_root: &std::path::Path) -> Result<bool, String> {
    let path = instance_root.join(PID_FILE_NAME);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let pid: u32 = raw.trim().parse().map_err(|_| {
        format!("Corrupted launch marker in {}: delete the file before launching.", path.display())
    })?;
    Ok(is_pid_alive(pid))
}

/// Prepare all files for an instance and launch Minecraft. Emits
/// `launch://progress`, `launch://log`, `launch://started`, and
/// `launch://exited` events keyed by `instanceId`.
///
/// The prepare/download work runs in a spawned task so `cancel_launch` can
/// abort it via `AbortHandle` — this interrupts every `.await` point in the
/// pipeline (version manifest, libraries, assets, Java runtime, Forge/NeoForge
/// installer) without needing a cancel flag threaded through each of those
/// modules individually.
#[tauri::command]
pub async fn launch_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<(), String> {
    let task_app = app.clone();
    let operation = crate::instances::operations::acquire(&instance_id)?;
    let task_instance_id = instance_id.clone();
    let handle = tokio::spawn(async move { run_launch(task_app, task_instance_id, operation).await });
    let abort_handle = handle.abort_handle();

    state
        .launches
        .lock()
        .unwrap()
        .insert(instance_id.clone(), abort_handle.clone());

    let result = handle.await;
    // Compare-and-remove: the frontend disables Play while a launch for this
    // instance is in flight, so two concurrent launches of the same instance
    // shouldn't happen today — but if that guard were ever bypassed, a plain
    // `.remove()` here could delete a second, still-running launch's registry
    // entry (inserted after this one) instead of this one's, silently making
    // `cancel_launch` a no-op for it.
    let mut launches = state.launches.lock().unwrap();
    if launches.get(&instance_id).map(|h| h.id()) == Some(abort_handle.id()) {
        launches.remove(&instance_id);
    }
    drop(launches);

    match result {
        Ok(inner) => inner,
        Err(join_err) if join_err.is_cancelled() => Err("Launch cancelled".to_string()),
        Err(join_err) => Err(join_err.to_string()),
    }
}

/// Signals an in-flight launch (still preparing/downloading) to stop at its
/// next await point. A no-op if the launch already finished or is already
/// running the game — cancelling a running Minecraft process is a different
/// concern (close the window / task-manager it), not covered here.
#[tauri::command]
pub fn cancel_launch(state: State<'_, AppState>, instance_id: String) -> Result<(), String> {
    if let Some(handle) = state.launches.lock().unwrap().get(&instance_id) {
        handle.abort();
    }
    Ok(())
}

async fn run_launch(
    app: AppHandle,
    instance_id: String,
    operation: crate::instances::operations::OperationGuard,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let instance = state
        .db
        .get_instance(&instance_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Instance not found.".to_string())?;

    let client = crate::download::http_client().map_err(|e| e.to_string())?;
    let account = ensure_account(&state, &client).await?;

    let game_root = crate::instances::paths::app_data_dir()
        .map_err(|e| e.to_string())?
        .join("minecraft");
    let instance_dir =
        crate::instances::paths::instance_root(&instance_id).map_err(|e| e.to_string())?;

    // Resolve launch settings: per-instance overrides win over global config.
    let inst_cfg = state
        .db
        .get_instance_launch_config(&instance_id)
        .unwrap_or_default();
    let java_override = inst_cfg.java_path.or_else(|| state.config.java_path());
    let max_memory = inst_cfg
        .max_memory_mb
        .unwrap_or_else(|| state.config.max_memory_mb());
    let extra_jvm_args = inst_cfg
        .jvm_args
        .or_else(|| state.config.jvm_args())
        .map(|s| split_jvm_args(&s))
        .unwrap_or_default();

    // Stamp last-played now that we're committed to launching.
    let _ = state.db.mark_played(&instance_id);

    // Progress events during download/prepare.
    let progress_app = app.clone();
    let progress_id = instance_id.clone();
    let report = move |update: ProgressUpdate| {
        let _ = progress_app.emit(
            "launch://progress",
            LaunchProgressEvent {
                instance_id: progress_id.clone(),
                stage: update.stage,
                current: update.current,
                total: update.total,
            },
        );
    };

    let prepared = prepare_launch(
        &client,
        game_root,
        instance_dir.clone(),
        &instance.minecraft_version,
        instance.loader,
        instance.loader_version.clone(),
        &account,
        java_override,
        max_memory,
        extra_jvm_args,
        &report,
    )
    .await
    .map_err(|e| e.to_string())?;

    revalidate_account(&client, &account).await?;
    crate::activity::append_log(
        &format!(
            "Launching {} (Minecraft {}, Java {})",
            instance.name, instance.minecraft_version, prepared.java_major
        ),
        "info",
        None,
    );

    // Spawn the game process.
    let mut command = std::process::Command::new(&prepared.java_path);
    command
        .args(&prepared.args)
        .current_dir(&prepared.working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to start Java: {e}"))?;

    // Record the PID so a startup check in a future Waybound session (after
    // this one closes with the game still running) can tell this instance is
    // still live rather than assuming it's launchable again. Best-effort:
    // if this write fails, launching still proceeds exactly as before.
    let pid_file = prepared.working_dir.join(PID_FILE_NAME);
    let _ = std::fs::write(&pid_file, child.id().to_string());
    // Remember the live PID so `stop_game` can kill a running game (the
    // reaper thread below takes ownership of the Child itself, so without
    // this there would be no handle left to stop it with).
    if let Ok(mut pids) = state.game_pids.lock() {
        pids.insert(instance_id.clone(), child.id());
    }

    let _ = app.emit(
        "launch://started",
        LaunchStartedEvent {
            instance_id: instance_id.clone(),
        },
    );

    // Stream stdout/stderr as log events, and mirror every line into the
    // instance's own log file so a crash can still be investigated after the
    // in-memory store is wiped by a relaunch or an app restart. Both readers
    // share one sink, so the file interleaves the two streams in arrival
    // order and the crash analysis below sees the whole tail.
    let launched_at = SystemTime::now();
    let sink = Arc::new(Mutex::new(open_log_sink(&instance_dir)));
    if let Some(stdout) = child.stdout.take() {
        spawn_log_reader(app.clone(), instance_id.clone(), stdout, "stdout", sink.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_reader(app.clone(), instance_id.clone(), stderr, "stderr", sink.clone());
    }

    // Reap the process and report its exit code. Poll with try_wait rather
    // than a single blocking wait() — simpler to reason about and avoids
    // relying on one long blocking OS call to always resolve correctly.
    let exit_app = app.clone();
    let exit_id = instance_id.clone();
    let exit_name = instance.name.clone();
    std::thread::spawn(move || {
        let _operation = operation;
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(500)),
                Err(_) => break None,
            }
        };
        let _ = std::fs::remove_file(&pid_file);
        let exit_state = exit_app.state::<AppState>();
        if let Ok(mut pids) = exit_state.game_pids.lock() {
            pids.remove(&exit_id);
        }
        // A deliberate stop reports as stopped, not crashed: the non-zero
        // exit code a kill produces would otherwise mislabel it.
        let user_stopped = exit_state
            .stop_requests
            .lock()
            .map(|mut set| set.remove(&exit_id))
            .unwrap_or(false);

        // Any non-zero code is a crash, unless the user stopped it
        // deliberately: a normal quit from the game's own menu exits 0,
        // and the player closing the window does too.
        let crashed = !user_stopped && code.is_some_and(|code| code != 0);
        let crash_reason = crashed.then(|| {
            // The reader threads are still draining the pipe at the moment
            // try_wait returns, and the mod-loader error that explains the
            // crash is in those very last lines. A short wait is the
            // difference between a specific reason and "unknown".
            std::thread::sleep(std::time::Duration::from_millis(400));
            let tail = sink
                .lock()
                .map(|sink| sink.tail.iter().cloned().collect::<Vec<_>>().join("\n"))
                .unwrap_or_default();
            explain_crash(&instance_dir, &exit_name, code, &tail, launched_at)
        });

        match &crash_reason {
            Some(reason) => crate::activity::append_log(reason, "error", None),
            None if user_stopped => crate::activity::append_log(
                &format!("{exit_name} stopped by the player"),
                "info",
                None,
            ),
            None => crate::activity::append_log(
                &format!("{exit_name} closed (exit {code:?})"),
                "info",
                None,
            ),
        }
        let _ = exit_app.emit(
            "launch://exited",
            LaunchExitedEvent {
                instance_id: exit_id,
                code,
                crashed,
                crash_reason,
                stopped_by_user: user_stopped,
            },
        );
    });

    Ok(())
}

fn spawn_log_reader<R>(
    app: AppHandle,
    instance_id: String,
    reader: R,
    stream: &'static str,
    sink: Arc<Mutex<LogSink>>,
) where
    R: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line in buffered.lines().map_while(Result::ok) {
            if let Ok(mut sink) = sink.lock() {
                sink.push(&line);
            }
            let _ = app.emit(
                "launch://log",
                LaunchLogEvent {
                    instance_id: instance_id.clone(),
                    stream,
                    line,
                },
            );
        }
    });
}

// ---- Crash explanation ---------------------------------------------------
//
// A non-zero exit tells the player nothing on its own. These helpers turn the
// two places the game does explain itself — its crash report file and the
// mod-loader's own error block in the console output — into one sentence.
// Every one of them is pure and total: no panics, no unwraps on user data, and
// "couldn't work it out" is always a valid answer (`None`), because a failure
// to explain a crash must never become a second failure.

/// The value of a `Key: 'value'` field inside a mod-loader error line.
fn quoted_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.split_once(key)?.1.trim_start();
    rest.strip_prefix('\'')?.split_once('\'').map(|(v, _)| v)
}

/// The lower bound of a Maven-style version range like `[7.4.1,8.0.0)`.
fn range_lower_bound(range: &str) -> Option<&str> {
    let inner = range.trim().trim_start_matches(['[', '(']);
    let end = inner.find([',', ']', ')']).unwrap_or(inner.len());
    let lower = inner[..end].trim();
    (!lower.is_empty()).then_some(lower)
}

/// "7.4.1 or newer" — the half of a version range a player can act on.
fn requirement_phrase(range: &str) -> String {
    match range_lower_bound(range) {
        Some(lower) => format!("{lower} or newer"),
        None => "a different version".to_string(),
    }
}

/// One `Mod ID: '…', Requested by: '…', Expected range: '…', Actual version: '…'`
/// line from Forge/NeoForge's missing-dependency block.
fn dependency_entry_reason(line: &str) -> Option<String> {
    let mod_id = quoted_field(line, "Mod ID:")?;
    let requested_by = quoted_field(line, "Requested by:")?;
    let range = quoted_field(line, "Expected range:")?;
    let need = requirement_phrase(range);
    match quoted_field(line, "Actual version:") {
        Some(actual) if !actual.is_empty() && actual != "[MISSING]" => Some(format!(
            "{mod_id} {actual} is installed, but {requested_by} needs {need}."
        )),
        _ => Some(format!(
            "{mod_id} isn't installed, but {requested_by} needs {need}."
        )),
    }
}

fn missing_dependency_reason(tail: &str) -> Option<String> {
    if !tail.contains("Missing or unsupported mandatory dependencies") {
        return None;
    }
    let entries: Vec<String> = tail.lines().filter_map(dependency_entry_reason).collect();
    let first = entries.first()?;
    Some(match entries.len() {
        1 => format!("crashed while loading mods. {first}"),
        2 => format!("crashed while loading mods. {first} One other mod dependency is unmet too."),
        n => format!(
            "crashed while loading mods. {first} {} other mod dependencies are unmet too.",
            n - 1
        ),
    })
}

/// The version reported by a `Currently, <mod> is <version>` line.
fn current_version<'a>(tail: &'a str, dependency: &str) -> Option<&'a str> {
    let needle = format!("Currently, {dependency} is ");
    tail.lines()
        .find_map(|line| line.split_once(&needle))
        .map(|(_, version)| version.trim().trim_end_matches('.'))
        .filter(|version| !version.is_empty())
}

/// The older/simpler `Mod <x> requires <y> <range>` phrasing.
fn requires_line_reason(tail: &str) -> Option<String> {
    for line in tail.lines() {
        // Matched anywhere in the line, never anchored: every real console
        // line arrives behind a `[12:00:01] [main/ERROR] [FML]: ` prefix.
        let Some((before, rest)) = line.split_once(" requires ") else {
            continue;
        };
        let Some(dependant) = before.rsplit_once("Mod ").map(|(_, name)| name.trim()) else {
            continue;
        };
        if dependant.contains(char::is_whitespace) {
            continue;
        }
        let mut parts = rest.trim().splitn(2, ' ');
        let dependency = parts.next().unwrap_or("").trim();
        let need = parts.next().unwrap_or("").trim().trim_end_matches('.');
        if dependant.is_empty() || dependency.is_empty() || need.is_empty() {
            continue;
        }
        return Some(match current_version(tail, dependency) {
            Some(installed) => format!(
                "crashed while loading mods. {dependency} {installed} is installed, but {dependant} needs {need}."
            ),
            None => format!("crashed while loading mods. {dependant} needs {dependency} {need}."),
        });
    }
    None
}

/// A reason clause read out of the run's console output, or `None` if nothing
/// in it is recognisable. Reads as a sentence following the instance name.
fn crash_reason_from_log(tail: &str) -> Option<String> {
    missing_dependency_reason(tail).or_else(|| requires_line_reason(tail))
}

/// Whether a line is the "top" exception of a stack trace rather than one of
/// its `at …` frames or the crash report's joke comment.
fn is_exception_line(line: &str) -> bool {
    if line.starts_with("//") || line.starts_with("at ") || line.starts_with("Caused by") {
        return false;
    }
    let head = line.split_once(':').map_or(line, |(head, _)| head);
    head.contains('.')
        && (head.ends_with("Exception") || head.ends_with("Error") || head.ends_with("Throwable"))
}

/// Keeps a wall-of-text exception message down to something a card can show.
fn shorten(line: &str) -> String {
    const MAX_CHARS: usize = 220;
    if line.chars().count() <= MAX_CHARS {
        return line.to_string();
    }
    let mut short: String = line.chars().take(MAX_CHARS).collect();
    short.push('…');
    short
}

/// A reason clause read out of a Minecraft crash report: its `Description:`
/// line plus the exception at the top of the stack trace.
fn crash_reason_from_report(text: &str) -> Option<String> {
    let description = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Description:"))
        .map(str::trim)
        .filter(|d| !d.is_empty());
    let exception = text
        .lines()
        .map(str::trim)
        .find(|line| is_exception_line(line))
        .map(shorten);
    match (description, exception) {
        (Some(description), Some(exception)) => Some(format!("crashed. {description}: {exception}")),
        (Some(description), None) => Some(format!("crashed. {description}.")),
        (None, Some(exception)) => Some(format!("crashed. {exception}")),
        (None, None) => None,
    }
}

/// The final sentence shown to the player.
fn crash_message(instance_name: &str, code: Option<i32>, reason: Option<String>) -> String {
    match reason {
        Some(clause) => format!("{instance_name} {clause}"),
        None => {
            let exit = match code {
                Some(code) => format!(" (exit code {code})"),
                None => String::new(),
            };
            format!(
                "{instance_name} crashed{exit}. Waybound couldn't tell why; the Logs tab has the full output."
            )
        }
    }
}

/// The newest `crash-*.txt` written *during this run*. An older report left in
/// the folder would otherwise explain today's crash with last month's stack
/// trace.
fn newest_crash_report(instance_dir: &Path, since: SystemTime) -> Option<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(instance_dir.join("crash-reports"))
        .ok()?
        .flatten()
    {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("crash-") && name.ends_with(".txt")) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if modified < since {
            continue;
        }
        if newest.as_ref().map_or(true, |(best, _)| modified > *best) {
            newest = Some((modified, entry.path()));
        }
    }
    newest.map(|(_, path)| path)
}

/// Crash report first (it's the game's own diagnosis), console output second.
fn explain_crash(
    instance_dir: &Path,
    instance_name: &str,
    code: Option<i32>,
    tail: &str,
    since: SystemTime,
) -> String {
    let reason = newest_crash_report(instance_dir, since)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .as_deref()
        .and_then(crash_reason_from_report)
        .or_else(|| crash_reason_from_log(tail));
    crash_message(instance_name, code, reason)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LaunchProgressEvent {
    instance_id: String,
    stage: String,
    current: u64,
    total: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LaunchStartedEvent {
    instance_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LaunchLogEvent {
    instance_id: String,
    stream: &'static str,
    line: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LaunchExitedEvent {
    instance_id: String,
    code: Option<i32>,
    /// Non-zero exit. Additive: existing consumers of `code` are unaffected.
    crashed: bool,
    /// A complete, human-readable sentence naming the instance and, where it
    /// could be worked out, the actual cause. `None` unless `crashed`.
    crash_reason: Option<String>,
    /// The player pressed Stop (as opposed to the game exiting on its own).
    /// Lets the frontend report "Stopped" instead of "Crashed".
    stopped_by_user: bool,
}

#[cfg(test)]
mod crash_reason_tests {
    use super::{crash_message, crash_reason_from_log, crash_reason_from_report};

    /// Verbatim from the run the user had to screenshot to get diagnosed.
    const NEOFORGE_MISSING_DEPS: &str = "\
[20:14:03] [main/INFO] [cpw.mods.modlauncher.Launcher/MODLAUNCHER]: ModLauncher running
[20:14:31] [main/ERROR] [ne.ne.fm.lo.mo.ModListScreen/LOADING]: Missing or unsupported mandatory dependencies:
\tMod ID: 'easy_npc_config_ui', Requested by: 'easy_npc_bundle', Expected range: '[7.4.1,8.0.0)', Actual version: '6.12.0'
[20:14:31] [main/INFO] [STDERR/]: at net.neoforged.fml.ModLoader.lambda$gatherAndInitializeMods$13(ModLoader.java:214)";

    #[test]
    fn explains_the_real_easy_npc_dependency_failure() {
        let reason = crash_reason_from_log(NEOFORGE_MISSING_DEPS).expect("should be recognised");
        assert_eq!(
            crash_message("Ascendra", Some(1), Some(reason)),
            "Ascendra crashed while loading mods. easy_npc_config_ui 6.12.0 is installed, \
             but easy_npc_bundle needs 7.4.1 or newer."
        );
    }

    #[test]
    fn reports_a_dependency_that_is_not_installed_at_all() {
        let log = "Missing or unsupported mandatory dependencies:\n\
            \tMod ID: 'sophisticatedcore', Requested by: 'sophisticatedbackpacks', \
            Expected range: '[1.2.0,)', Actual version: '[MISSING]'";
        assert_eq!(
            crash_reason_from_log(log).as_deref(),
            Some(
                "crashed while loading mods. sophisticatedcore isn't installed, \
                 but sophisticatedbackpacks needs 1.2.0 or newer."
            )
        );
    }

    #[test]
    fn counts_the_remaining_unmet_dependencies_without_listing_them_all() {
        let log = "Missing or unsupported mandatory dependencies:\n\
            \tMod ID: 'a', Requested by: 'x', Expected range: '[1.0,)', Actual version: '0.9'\n\
            \tMod ID: 'b', Requested by: 'y', Expected range: '[2.0,)', Actual version: '1.0'\n\
            \tMod ID: 'c', Requested by: 'z', Expected range: '[3.0,)', Actual version: '2.0'";
        let reason = crash_reason_from_log(log).expect("should be recognised");
        assert!(
            reason.contains("a 0.9 is installed, but x needs 1.0 or newer."),
            "{reason}"
        );
        assert!(reason.ends_with("2 other mod dependencies are unmet too."), "{reason}");
    }

    #[test]
    fn explains_the_simpler_requires_phrasing() {
        let log = "\
[12:00:01] [main/ERROR] [FML]: Mod jei requires jeitweaker 1.0.0 or above
[12:00:01] [main/ERROR] [FML]: Currently, jeitweaker is 0.9.0";
        assert_eq!(
            crash_reason_from_log(log).as_deref(),
            Some("crashed while loading mods. jeitweaker 0.9.0 is installed, but jei needs 1.0.0 or above.")
        );
    }

    #[test]
    fn falls_back_to_the_requirement_alone_when_no_installed_version_is_reported() {
        let log = "Mod jei requires jeitweaker 1.0.0 or above";
        assert_eq!(
            crash_reason_from_log(log).as_deref(),
            Some("crashed while loading mods. jei needs jeitweaker 1.0.0 or above.")
        );
    }

    #[test]
    fn ordinary_output_yields_no_reason_rather_than_a_wrong_one() {
        let log = "\
[12:00:00] [main/INFO] [minecraft/Minecraft]: Setting user: Steve
[12:00:04] [Render thread/INFO] [minecraft/Minecraft]: Stopping!";
        assert_eq!(crash_reason_from_log(log), None);
        assert_eq!(crash_reason_from_log(""), None);
    }

    #[test]
    fn malformed_dependency_lines_do_not_panic_or_half_explain() {
        // Header present, entries truncated/garbled: better to say nothing
        // than to emit a sentence with holes in it.
        let log = "Missing or unsupported mandatory dependencies:\n\
            \tMod ID: 'easy_npc_config_ui', Requested by: 'easy_npc\n\
            \tMod ID: , Requested by: , Expected range: , Actual version:";
        assert_eq!(crash_reason_from_log(log), None);
    }

    #[test]
    fn reads_description_and_top_exception_out_of_a_crash_report() {
        let report = "\
---- Minecraft Crash Report ----
// Why did you do that?

Time: 2026-07-30 20:14:33
Description: Rendering overlay

java.lang.NullPointerException: Cannot invoke \"net.minecraft.client.Options.getSoundVolume()\"
\tat net.minecraft.client.gui.Gui.render(Gui.java:120)
Caused by: java.lang.IllegalStateException: nope";
        assert_eq!(
            crash_reason_from_report(report).as_deref(),
            Some(
                "crashed. Rendering overlay: java.lang.NullPointerException: \
                 Cannot invoke \"net.minecraft.client.Options.getSoundVolume()\""
            )
        );
    }

    #[test]
    fn a_report_with_only_a_description_still_explains_something() {
        let report = "---- Minecraft Crash Report ----\n\nDescription: Ticking entity\n";
        assert_eq!(
            crash_reason_from_report(report).as_deref(),
            Some("crashed. Ticking entity.")
        );
    }

    #[test]
    fn an_unrecognisable_report_yields_no_reason() {
        assert_eq!(crash_reason_from_report(""), None);
        assert_eq!(crash_reason_from_report("just some text\nand more text"), None);
    }

    #[test]
    fn unknown_crashes_still_say_so_plainly() {
        assert_eq!(
            crash_message("Ascendra", Some(1), None),
            "Ascendra crashed (exit code 1). Waybound couldn't tell why; \
             the Logs tab has the full output."
        );
        assert_eq!(
            crash_message("Ascendra", None, None),
            "Ascendra crashed. Waybound couldn't tell why; the Logs tab has the full output."
        );
    }
}

#[cfg(test)]
mod log_sink_tests {
    use super::{open_log_sink, LOG_FILE_MAX_BYTES, LOG_FILE_NAME, LOG_TAIL_LINES, PREV_LOG_FILE_NAME};

    // The pid keeps two concurrently-running `cargo test` processes (a manual
    // run overlapping a watcher, CI running two targets) off the same
    // directory. Without it they share a fixed path and one wipes it out from
    // under the other mid-test — a cross-process race, so `--test-threads=1`
    // never helped.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("waybound-log-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn rotates_the_previous_run_aside_and_keeps_a_bounded_tail() {
        let dir = temp_dir("rotate");
        let mut sink = open_log_sink(&dir);
        sink.push("first run");
        drop(sink);

        let mut sink = open_log_sink(&dir);
        sink.push("second run");
        drop(sink);

        assert_eq!(
            std::fs::read_to_string(dir.join(PREV_LOG_FILE_NAME)).unwrap().trim(),
            "first run"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(LOG_FILE_NAME)).unwrap().trim(),
            "second run"
        );

        let mut sink = open_log_sink(&dir);
        for i in 0..(LOG_TAIL_LINES + 50) {
            sink.push(&format!("line {i}"));
        }
        assert_eq!(sink.tail.len(), LOG_TAIL_LINES);
        assert_eq!(sink.tail.back().map(String::as_str), Some("line 549"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stops_growing_the_file_once_the_size_cap_is_hit() {
        let dir = temp_dir("cap");
        let mut sink = open_log_sink(&dir);
        let chunk = "x".repeat(64 * 1024);
        for _ in 0..200 {
            sink.push(&chunk);
        }
        assert!(sink.file.is_none(), "writing should have stopped at the cap");
        let size = std::fs::metadata(dir.join(LOG_FILE_NAME)).unwrap().len();
        assert!(
            size < LOG_FILE_MAX_BYTES + 128 * 1024,
            "file grew to {size} bytes past the cap"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod pid_liveness_tests {
    use super::is_pid_alive;

    #[test]
    fn reports_the_current_process_as_alive() {
        // The one PID we can assert about with certainty: our own. If this
        // regresses, every instance looks dead on startup and Waybound
        // happily allows a second concurrent launch into the same world.
        assert!(is_pid_alive(std::process::id()));
    }

    #[test]
    fn reports_an_exited_process_as_dead() {
        // Spawn something trivial, reap it, then ask. The inverse failure
        // (a dead process reported alive) permanently wedges the Play
        // button on "Running" with no way back short of editing the DB.
        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd").args(["/C", "exit"]).spawn()
        } else {
            std::process::Command::new("true").spawn()
        }
        .expect("spawning a trivial process should work");

        let pid = child.id();
        child.wait().expect("child should exit");

        // Not asserted as a hard invariant of the OS — PIDs are reusable in
        // principle — but reuse this fast is vanishingly unlikely, and a
        // flake here still points at something worth looking at.
        assert!(!is_pid_alive(pid), "pid {pid} was reaped but still reads as alive");
    }

    #[test]
    fn treats_an_implausible_pid_as_dead_rather_than_erroring() {
        // The lookup shells out; a failure to run it must degrade to
        // "not running" instead of propagating, or startup breaks entirely
        // on a machine where the helper isn't available.
        assert!(!is_pid_alive(u32::MAX));
    }
}
