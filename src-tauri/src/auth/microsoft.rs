//! Microsoft sign-in via the legacy **`login.live.com` device flow** -> Xbox
//! Live -> XSTS -> Minecraft services.
//!
//! This is the exact flow the CurseForge app and Prism/MultiMC use: it relies on
//! the well-known **public** Xbox/Minecraft client ID `00000000402b5328`, so the
//! user needs no Azure app registration. They get an 8-character code, enter it
//! at <https://login.live.com/oauth20_remoteconnect.srf>, and we poll for
//! completion.

use serde::Deserialize;
use thiserror::Error;

use super::{now_secs, Account};

// Public Xbox Live client ID used by the official Minecraft launcher and most
// third-party launchers. Needs no registration; scope is the classic MBI_SSL
// Xbox ticket.
const CLIENT_ID: &str = "00000000402b5328";
const SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";

const DEVICE_CODE_URL: &str = "https://login.live.com/oauth20_connect.srf";
const TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";
const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("login was not completed in time; please try again")]
    Expired,
    #[error("login was declined on the Microsoft page")]
    Declined,
    #[error("this Microsoft account has no Xbox profile. Sign in once at minecraft.net first.")]
    NoXboxAccount,
    #[error("this account is a child account and must be added to a Microsoft Family")]
    ChildAccount,
    #[error("this Microsoft account does not own Minecraft: Java Edition")]
    NoMinecraft,
    #[error("unexpected response from {0}: {1}")]
    Unexpected(&'static str, String),
}

/// The device-code prompt shown to the user while we poll for completion.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCodePrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub message: String,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    #[serde(alias = "verification_url")]
    verification_uri: String,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct TokenErrorResponse {
    error: String,
}

#[derive(Deserialize)]
struct XboxResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: DisplayClaims,
}

#[derive(Deserialize)]
struct DisplayClaims {
    xui: Vec<Xui>,
}

#[derive(Deserialize)]
struct Xui {
    uhs: String,
}

#[derive(Deserialize)]
struct XstsErrorResponse {
    #[serde(rename = "XErr")]
    xerr: Option<u64>,
}

#[derive(Deserialize)]
struct MinecraftLoginResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct MinecraftProfile {
    id: String,
    name: String,
}

/// Ask Microsoft for a device code. The caller shows the prompt to the user,
/// then calls [`poll_for_token`] until it resolves. No client ID needed — the
/// public Xbox client is used.
pub async fn request_device_code(
    client: &reqwest::Client,
) -> Result<(DeviceCodePrompt, DevicePoll), AuthError> {
    let resp = client
        .post(DEVICE_CODE_URL)
        .form(&[
            ("client_id", CLIENT_ID),
            ("scope", SCOPE),
            ("response_type", "device_code"),
        ])
        .send()
        .await?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::Unexpected("device code endpoint", body));
    }

    let dc: DeviceCodeResponse = resp.json().await?;
    let prompt = DeviceCodePrompt {
        message: format!(
            "Go to {} and enter the code {}",
            dc.verification_uri, dc.user_code
        ),
        user_code: dc.user_code,
        verification_uri: dc.verification_uri,
    };
    let poll = DevicePoll {
        device_code: dc.device_code,
        interval: dc.interval.max(1),
        deadline: now_secs() + dc.expires_in,
    };
    Ok((prompt, poll))
}

/// State needed to poll the token endpoint for an in-progress device login.
pub struct DevicePoll {
    device_code: String,
    interval: u64,
    deadline: u64,
}

impl DevicePoll {
    pub fn interval_secs(&self) -> u64 {
        self.interval
    }
    pub fn is_expired(&self) -> bool {
        now_secs() >= self.deadline
    }
}

/// One poll attempt. Returns `Ok(None)` while still pending (caller should wait
/// `interval` seconds and retry), `Ok(Some(..))` on success.
pub async fn poll_for_token(
    client: &reqwest::Client,
    poll: &DevicePoll,
) -> Result<Option<(String, String, u64)>, AuthError> {
    if poll.is_expired() {
        return Err(AuthError::Expired);
    }

    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("client_id", CLIENT_ID),
            ("device_code", &poll.device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await?;

    if resp.status().is_success() {
        let token: TokenResponse = resp.json().await?;
        return Ok(Some((
            token.access_token,
            token.refresh_token,
            now_secs() + token.expires_in,
        )));
    }

    let err: TokenErrorResponse = resp
        .json()
        .await
        .unwrap_or(TokenErrorResponse { error: String::new() });
    match err.error.as_str() {
        "authorization_pending" | "slow_down" => Ok(None),
        "expired_token" | "code_expired" => Err(AuthError::Expired),
        "authorization_declined" | "access_denied" => Err(AuthError::Declined),
        other => Err(AuthError::Unexpected("token endpoint", other.to_string())),
    }
}

/// Exchange a stored refresh token for fresh tokens (silent re-login).
pub async fn refresh_msa_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<(String, String, u64), AuthError> {
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
            ("scope", SCOPE),
        ])
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(AuthError::Declined);
    }
    let token: TokenResponse = resp.json().await?;
    Ok((
        token.access_token,
        token.refresh_token,
        now_secs() + token.expires_in,
    ))
}

/// Take an Xbox MBI_SSL access token + its refresh token all the way to a full
/// Minecraft [`Account`] (Xbox -> XSTS -> Minecraft services -> profile).
pub async fn complete_minecraft_login(
    client: &reqwest::Client,
    msa_access_token: &str,
    msa_refresh_token: String,
) -> Result<Account, AuthError> {
    // 1. Xbox Live user token. MBI_SSL tokens are passed raw (no "d=" prefix,
    //    unlike the newer microsoftonline v2 flow).
    let xbl: XboxResponse = client
        .post(XBL_URL)
        .json(&serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": msa_access_token,
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        }))
        .send()
        .await?
        .json()
        .await
        .map_err(|e| AuthError::Unexpected("Xbox Live", e.to_string()))?;

    // 2. XSTS token (authorize against Minecraft services relying party).
    let xsts_resp = client
        .post(XSTS_URL)
        .json(&serde_json::json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [xbl.token],
            },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT",
        }))
        .send()
        .await?;

    if xsts_resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        let err: XstsErrorResponse = xsts_resp
            .json()
            .await
            .unwrap_or(XstsErrorResponse { xerr: None });
        return Err(match err.xerr {
            Some(2148916233) => AuthError::NoXboxAccount,
            Some(2148916238) => AuthError::ChildAccount,
            _ => AuthError::NoXboxAccount,
        });
    }

    let xsts: XboxResponse = xsts_resp
        .json()
        .await
        .map_err(|e| AuthError::Unexpected("XSTS", e.to_string()))?;
    let uhs = xsts
        .display_claims
        .xui
        .first()
        .map(|x| x.uhs.clone())
        .ok_or_else(|| AuthError::Unexpected("XSTS", "no user hash".into()))?;

    // 3. Minecraft services login.
    let mc: MinecraftLoginResponse = client
        .post(MC_LOGIN_URL)
        .json(&serde_json::json!({
            "identityToken": format!("XBL3.0 x={uhs};{}", xsts.token),
        }))
        .send()
        .await?
        .json()
        .await
        .map_err(|e| AuthError::Unexpected("Minecraft login", e.to_string()))?;

    // 4. Fetch the profile (proves ownership; gives uuid + name).
    let profile_resp = client
        .get(MC_PROFILE_URL)
        .bearer_auth(&mc.access_token)
        .send()
        .await?;

    if !profile_resp.status().is_success() {
        return Err(AuthError::NoMinecraft);
    }

    let profile: MinecraftProfile = profile_resp
        .json()
        .await
        .map_err(|_| AuthError::NoMinecraft)?;

    Ok(Account {
        uuid: profile.id,
        username: profile.name,
        minecraft_token: mc.access_token,
        msa_refresh_token,
        expires_at: now_secs() + mc.expires_in,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network test: the real live.com endpoint must hand back a device code and
    /// the `remoteconnect` verification URL — proving the client ID/params are
    /// accepted with no Azure app. Run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn requests_real_device_code() {
        let client = crate::download::http_client().unwrap();
        let (prompt, poll) = request_device_code(&client).await.unwrap();
        assert!(!prompt.user_code.is_empty(), "empty user code");
        assert!(
            prompt.verification_uri.starts_with("https://"),
            "unexpected uri: {}",
            prompt.verification_uri
        );
        assert!(poll.interval_secs() >= 1);
    }
}
