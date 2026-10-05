//! Tauri commands for Microsoft / Minecraft account sign-in.

use std::future::Future;
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use crate::auth::microsoft::{complete_minecraft_login, poll_for_token, request_device_code};
use crate::auth::AccountPublic;
use super::search::AppState;

const CANCELLED: &str = "Sign-in cancelled";

#[tauri::command]
pub fn get_account(state: State<'_, AppState>) -> Option<AccountPublic> {
    state.config.account().map(|a| a.to_public())
}

#[tauri::command]
pub fn logout(state: State<'_, AppState>) -> Result<(), String> {
    state.config.set_account(None).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cancel_microsoft_login(state: State<'_, AppState>, login_id: String) -> Result<(), String> {
    let mut login = state.auth_login.lock().map_err(|_| "Sign-in registry unavailable")?;
    cancel_login(&mut login, login_id);
    Ok(())
}

type LoginRegistry = Option<(String, tokio::sync::watch::Sender<bool>)>;

fn cancel_login(login: &mut LoginRegistry, login_id: String) {
    match login.as_ref() {
        Some((id, cancel)) if id == &login_id => { cancel.send_replace(true); },
        Some(_) => {},
        None => {
            // Cancellation may arrive before the async login command is polled.
            // Keep a receiver-less tombstone; never discard that cancellation.
            let (sender, receiver) = tokio::sync::watch::channel(true);
            drop(receiver);
            *login = Some((login_id, sender));
        }
    }
}

fn register_login(login: &mut LoginRegistry, login_id: &str, sender: tokio::sync::watch::Sender<bool>) -> Result<(), String> {
    if let Some((id, existing)) = login.as_ref() {
        if existing.is_closed() && *existing.borrow() {
            let cancelled = id == login_id;
            *login = None;
            if cancelled { return Err(CANCELLED.into()); }
        } else {
            return Err("Another sign-in is still finishing. Please retry.".into());
        }
    }
    *login = Some((login_id.to_string(), sender));
    Ok(())
}

async fn until_cancelled<T>(
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    future: impl Future<Output = T>,
) -> Result<T, String> {
    if *cancel.borrow() { return Err(CANCELLED.into()); }
    tokio::select! {
        biased;
        _ = cancel.changed() => Err(CANCELLED.into()),
        value = future => Ok(value),
    }
}

/// Runs one correlated device login. Dropping any network wait on cancellation
/// prevents a dismissed dialog from signing in later in the background.
#[tauri::command]
pub async fn microsoft_login(
    app: AppHandle,
    state: State<'_, AppState>,
    login_id: String,
) -> Result<AccountPublic, String> {
    let (sender, mut cancel) = tokio::sync::watch::channel(false);
    {
        let mut login = state.auth_login.lock().map_err(|_| "Sign-in registry unavailable")?;
        register_login(&mut login, &login_id, sender)?;
    }
    struct LoginGuard<'a> { state: &'a AppState, id: &'a str }
    impl Drop for LoginGuard<'_> {
        fn drop(&mut self) {
            if let Ok(mut login) = self.state.auth_login.lock() {
                if login.as_ref().is_some_and(|(id, _)| id == self.id) { *login = None; }
            }
        }
    }
    let _guard = LoginGuard { state: &state, id: &login_id };
    let client = crate::download::http_client().map_err(|e| e.to_string())?;
    let (prompt, mut poll) = until_cancelled(&mut cancel, request_device_code(&client))
        .await?.map_err(|e| e.to_string())?;
    let _ = app.emit("auth://device-code", serde_json::json!({ "loginId": login_id, "prompt": prompt }));
    let tokens = loop {
        until_cancelled(&mut cancel, tokio::time::sleep(Duration::from_secs(poll.interval_secs()))).await?;
        match until_cancelled(&mut cancel, poll_for_token(&client, &mut poll)).await? {
            Ok(Some(tokens)) => break tokens,
            Ok(None) => continue,
            Err(e) => return Err(e.to_string()),
        }
    };
    let (msa_access, msa_refresh, _) = tokens;
    let account = until_cancelled(&mut cancel, complete_minecraft_login(&client, &msa_access, msa_refresh))
        .await?.map_err(|e| e.to_string())?;
    let public = account.to_public();
    {
        // Serialize cancellation with persistence: a cancelled flow never saves credentials.
        let login = state.auth_login.lock().map_err(|_| "Sign-in registry unavailable")?;
        if login.as_ref().is_none_or(|(_, sender)| *sender.borrow()) { return Err(CANCELLED.into()); }
        state.config.set_account(Some(account)).map_err(|e| e.to_string())?;
    }
    Ok(public)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_before_command_registration_blocks_that_attempt_but_not_retry() {
        let mut login = None;
        cancel_login(&mut login, "first".into());
        let (sender, _receiver) = tokio::sync::watch::channel(false);
        assert_eq!(register_login(&mut login, "first", sender).unwrap_err(), CANCELLED);
        let (sender, receiver) = tokio::sync::watch::channel(false);
        register_login(&mut login, "retry", sender).unwrap();
        cancel_login(&mut login, "first".into());
        assert!(!*receiver.borrow(), "stale cancellation must not cancel retry");
        cancel_login(&mut login, "retry".into());
        assert!(*receiver.borrow());
    }

    #[test]
    fn late_cancellation_tombstone_does_not_block_fresh_attempt() {
        let mut login = None;
        cancel_login(&mut login, "finished".into());
        let (sender, receiver) = tokio::sync::watch::channel(false);
        register_login(&mut login, "fresh", sender).unwrap();
        assert!(!*receiver.borrow());
    }

    #[tokio::test]
    async fn cancellation_interrupts_pending_work() {
        let (sender, mut receiver) = tokio::sync::watch::channel(false);
        let work = until_cancelled(&mut receiver, std::future::pending::<()>());
        let cancel = async { sender.send(true).unwrap(); };
        let (result, ()) = tokio::join!(work, cancel);
        assert_eq!(result.unwrap_err(), CANCELLED);
    }

    #[tokio::test]
    async fn prior_cancellation_prevents_work_and_new_attempt_is_fresh() {
        let (sender, mut receiver) = tokio::sync::watch::channel(false);
        sender.send(true).unwrap();
        let result: Result<(), String> = until_cancelled(&mut receiver, async { panic!("cancelled work polled") }).await;
        assert_eq!(result.unwrap_err(), CANCELLED);
        let (_sender, mut fresh) = tokio::sync::watch::channel(false);
        assert_eq!(until_cancelled(&mut fresh, async { 42 }).await.unwrap(), 42);
    }
}
