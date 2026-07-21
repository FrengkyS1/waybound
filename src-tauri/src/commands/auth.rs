//! Tauri commands for Microsoft / Minecraft account sign-in.

use std::time::Duration;

use tauri::{AppHandle, Emitter, State};

use crate::auth::microsoft::{complete_minecraft_login, poll_for_token, request_device_code};
use crate::auth::AccountPublic;

use super::search::AppState;

/// The currently signed-in account (no tokens), if any.
#[tauri::command]
pub fn get_account(state: State<'_, AppState>) -> Option<AccountPublic> {
    state.config.account().map(|a| a.to_public())
}

#[tauri::command]
pub fn logout(state: State<'_, AppState>) -> Result<(), String> {
    state.config.set_account(None).map_err(|e| e.to_string())
}

/// Run the full device-code login. Emits `auth://device-code` with the code the
/// user must enter, then resolves once they finish (or errors on timeout). No
/// setup required — uses the public Xbox client, exactly like the CurseForge app.
#[tauri::command]
pub async fn microsoft_login(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AccountPublic, String> {
    let client = crate::download::http_client().map_err(|e| e.to_string())?;

    let (prompt, poll) = request_device_code(&client)
        .await
        .map_err(|e| e.to_string())?;

    // Tell the UI which code to show and where to enter it.
    let _ = app.emit("auth://device-code", &prompt);

    // Poll until the user completes (or the code expires).
    let tokens = loop {
        tokio::time::sleep(Duration::from_secs(poll.interval_secs())).await;
        match poll_for_token(&client, &poll).await {
            Ok(Some(tokens)) => break tokens,
            Ok(None) => continue,
            Err(e) => return Err(e.to_string()),
        }
    };

    let (msa_access, msa_refresh, _expires) = tokens;
    let account = complete_minecraft_login(&client, &msa_access, msa_refresh)
        .await
        .map_err(|e| e.to_string())?;

    let public = account.to_public();
    state
        .config
        .set_account(Some(account))
        .map_err(|e| e.to_string())?;

    let _ = app.emit("auth://signed-in", &public);
    Ok(public)
}
