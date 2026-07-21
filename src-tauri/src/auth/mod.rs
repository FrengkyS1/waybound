pub mod microsoft;

use serde::{Deserialize, Serialize};

/// A fully-authenticated Minecraft account, ready to launch the game.
///
/// The `msa_refresh_token` is the sensitive long-lived credential used to
/// silently re-acquire a Minecraft access token when the short-lived one
/// expires. Stored locally (this is a personal-use tool); treat with care.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// Minecraft profile UUID (no dashes, as returned by the profile API).
    pub uuid: String,
    /// Minecraft username / gamertag.
    pub username: String,
    /// Short-lived Minecraft access token (Bearer) for launch.
    pub minecraft_token: String,
    /// Long-lived Microsoft refresh token for silent re-auth.
    pub msa_refresh_token: String,
    /// Unix seconds when the Minecraft token expires.
    pub expires_at: u64,
}

impl Account {
    pub fn is_token_expired(&self) -> bool {
        now_secs() + 60 >= self.expires_at
    }

    /// A public projection safe to hand to the frontend (no tokens).
    pub fn to_public(&self) -> AccountPublic {
        AccountPublic {
            uuid: self.uuid.clone(),
            username: self.username.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPublic {
    pub uuid: String,
    pub username: String,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
