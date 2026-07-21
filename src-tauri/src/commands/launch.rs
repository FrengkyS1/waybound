//! Tauri commands for preparing files and launching an instance.

use std::io::{BufRead, BufReader};
use std::process::Stdio;

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

/// Ensure the stored account has a usable Minecraft token, silently refreshing
/// via the Microsoft refresh token when it has expired.
async fn ensure_account(
    state: &AppState,
    client: &reqwest::Client,
) -> Result<Account, String> {
    let account = state
        .config
        .account()
        .ok_or_else(|| "Sign in with your Microsoft account before playing.".to_string())?;

    if !account.is_token_expired() {
        return Ok(account);
    }

    let (access, refresh, _exp) = refresh_msa_token(client, &account.msa_refresh_token)
        .await
        .map_err(|_| "Your session expired. Please sign in again.".to_string())?;

    let fresh = complete_minecraft_login(client, &access, refresh)
        .await
        .map_err(|e| e.to_string())?;

    state
        .config
        .set_account(Some(fresh.clone()))
        .map_err(|e| e.to_string())?;
    Ok(fresh)
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
    let task_instance_id = instance_id.clone();
    let handle = tokio::spawn(async move { run_launch(task_app, task_instance_id).await });
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

async fn run_launch(app: AppHandle, instance_id: String) -> Result<(), String> {
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
        instance_dir,
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

    let _ = app.emit(
        "launch://started",
        LaunchStartedEvent {
            instance_id: instance_id.clone(),
        },
    );

    // Stream stdout/stderr as log events.
    if let Some(stdout) = child.stdout.take() {
        spawn_log_reader(app.clone(), instance_id.clone(), stdout, "stdout");
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_reader(app.clone(), instance_id.clone(), stderr, "stderr");
    }

    // Reap the process and report its exit code. Poll with try_wait rather
    // than a single blocking wait() — simpler to reason about and avoids
    // relying on one long blocking OS call to always resolve correctly.
    let exit_app = app.clone();
    let exit_id = instance_id.clone();
    let exit_name = instance.name.clone();
    std::thread::spawn(move || {
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(500)),
                Err(_) => break None,
            }
        };
        crate::activity::append_log(
            &format!("{exit_name} closed (exit {code:?})"),
            "info",
            None,
        );
        let _ = exit_app.emit(
            "launch://exited",
            LaunchExitedEvent {
                instance_id: exit_id,
                code,
            },
        );
    });

    Ok(())
}

fn spawn_log_reader<R>(app: AppHandle, instance_id: String, reader: R, stream: &'static str)
where
    R: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line in buffered.lines().map_while(Result::ok) {
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
}
