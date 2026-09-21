//! Cached loader-version index — PrismLauncher's `meta/VersionList` idea, cut
//! to what the UI actually needs: latest + recommended builds per (loader,
//! game version) with a 24h TTL, so version displays rarely touch the
//! network and keep working offline from yesterday's answers.
//!
//! Deliberately read-only w.r.t. launching: the launch pipeline keeps its
//! own resolution logic untouched. This serves explicit checks and Settings
//! displays; on a network failure it falls back to the stale row, and only
//! errors when it has never successfully fetched for this pair at all.

use serde::Serialize;

use crate::db::Database;
use crate::dto::ModLoader;

/// Freshness horizon for a cached row. A day is plenty — loader builds
/// release far less often, and staleness only ever shows a slightly older
/// "latest", never a wrong one.
pub const CACHE_TTL_SECS: u64 = 24 * 3600;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoaderVersionInfo {
    pub loader: String,
    pub minecraft_version: String,
    pub latest: Option<String>,
    pub recommended: Option<String>,
    pub from_cache: bool,
    pub fetched_at_unix: Option<u64>,
}

fn loader_key(loader: ModLoader) -> &'static str {
    match loader {
        ModLoader::Fabric => "fabric",
        ModLoader::Forge => "forge",
        ModLoader::NeoForge => "neoforge",
        ModLoader::Quilt => "quilt",
        ModLoader::Vanilla => "vanilla",
    }
}

pub async fn get_loader_version_info(
    db: &Database,
    loader: ModLoader,
    mc: &str,
) -> Result<LoaderVersionInfo, String> {
    let key = loader_key(loader);
    // A vanilla instance has no loader to version.
    if matches!(loader, ModLoader::Vanilla) {
        return Ok(LoaderVersionInfo {
            loader: key.to_string(),
            minecraft_version: mc.to_string(),
            latest: None,
            recommended: None,
            from_cache: false,
            fetched_at_unix: None,
        });
    }
    let now = crate::db::now_unix();
    let cached = db.get_loader_meta(key, mc).map_err(|e| e.to_string())?;
    if let Some(row) = cached.as_ref() {
        if now.saturating_sub(row.fetched_at) < CACHE_TTL_SECS {
            return Ok(LoaderVersionInfo {
                loader: key.to_string(),
                minecraft_version: mc.to_string(),
                latest: row.latest.clone(),
                recommended: row.recommended.clone(),
                from_cache: true,
                fetched_at_unix: Some(row.fetched_at),
            });
        }
    }
    match fetch_fresh(loader, mc).await {
        Ok((latest, recommended)) => {
            let _ = db.set_loader_meta(key, mc, latest.as_deref(), recommended.as_deref(), now);
            Ok(LoaderVersionInfo {
                loader: key.to_string(),
                minecraft_version: mc.to_string(),
                latest,
                recommended,
                from_cache: false,
                fetched_at_unix: Some(now),
            })
        }
        // Offline (or upstream hiccup) with a stale row: yesterday's answer
        // beats no answer for a display. Only hard-error with nothing at all.
        Err(err) => match cached {
            Some(row) => Ok(LoaderVersionInfo {
                loader: key.to_string(),
                minecraft_version: mc.to_string(),
                latest: row.latest.clone(),
                recommended: row.recommended.clone(),
                from_cache: true,
                fetched_at_unix: Some(row.fetched_at),
            }),
            None => Err(err),
        },
    }
}

/// Returns (latest, recommended). Only Forge distinguishes the two;
/// everyone else reports latest alone.
async fn fetch_fresh(loader: ModLoader, mc: &str) -> Result<(Option<String>, Option<String>), String> {
    let client = crate::download::http_client().map_err(|e| e.to_string())?;
    match loader {
        ModLoader::Forge => {
            let promos = fetch_forge_promotions(&client).await?;
            let latest = promos
                .get(&format!("{mc}-latest"))
                .or_else(|| promos.get(&format!("{mc}-recommended")))
                .cloned();
            let recommended = promos.get(&format!("{mc}-recommended")).cloned();
            Ok((latest, recommended))
        }
        ModLoader::NeoForge => {
            // Same resolver launch itself uses — maven metadata has no
            // recommended channel, so latest stable is the whole story.
            let latest = crate::launch::forge::latest_neoforge(&client, mc)
                .await
                .map_err(|e| e.to_string())?;
            Ok((Some(latest), None))
        }
        ModLoader::Fabric => {
            // This also finally puts `latest_loader_version` (flagged
            // dead-code until now) to its intended use.
            let latest = crate::launch::fabric::latest_loader_version(&client, mc)
                .await
                .map_err(|e| e.to_string())?;
            Ok((Some(latest), None))
        }
        ModLoader::Quilt => {
            let latest = fetch_latest_quilt(&client, mc).await?;
            Ok((Some(latest), None))
        }
        ModLoader::Vanilla => Ok((None, None)),
    }
}

const FORGE_PROMOTIONS: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";

async fn fetch_forge_promotions(
    client: &reqwest::Client,
) -> Result<std::collections::HashMap<String, String>, String> {
    #[derive(serde::Deserialize)]
    struct Promotions {
        promos: std::collections::HashMap<String, String>,
    }
    client
        .get(FORGE_PROMOTIONS)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Promotions>()
        .await
        .map(|p| p.promos)
        .map_err(|e| format!("forge promotions: {e}"))
}

/// Quilt's per-MC loader list is newest-ish first but carries no stable
/// flag — prefer the first non-prerelease, fall back to the very first.
async fn fetch_latest_quilt(client: &reqwest::Client, mc: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Entry {
        loader: LoaderEntry,
    }
    #[derive(serde::Deserialize)]
    struct LoaderEntry {
        version: String,
    }
    let entries: Vec<Entry> = client
        .get(format!("https://meta.quiltmc.org/v3/versions/loader/{mc}"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| format!("quilt loader list: {e}"))?;
    let versions: Vec<String> = entries.into_iter().map(|e| e.loader.version).collect();
    fetch_latest_quilt_pick(&versions).ok_or_else(|| format!("no Quilt loader for {mc}"))
}

fn fetch_latest_quilt_pick(versions: &[String]) -> Option<String> {
    versions
        .iter()
        .find(|v| !v.contains("-alpha") && !v.contains("-beta"))
        .cloned()
        .or_else(|| versions.first().cloned())
}

#[cfg(test)]
mod loader_meta_tests {
    use super::{fetch_latest_quilt_pick, CACHE_TTL_SECS};

    #[test]
    fn quilt_prefers_stable_over_newer_beta() {
        let versions = vec![
            "0.25.0-beta.3".to_string(),
            "0.24.0".to_string(),
            "0.25.0-beta.2".to_string(),
        ];
        assert_eq!(fetch_latest_quilt_pick(&versions).as_deref(), Some("0.24.0"));
    }

    #[test]
    fn quilt_falls_back_to_first_when_all_prerelease() {
        let versions = vec!["0.25.0-beta.3".to_string(), "0.25.0-beta.2".to_string()];
        assert_eq!(fetch_latest_quilt_pick(&versions).as_deref(), Some("0.25.0-beta.3"));
    }

    #[test]
    fn empty_list_picks_nothing() {
        assert_eq!(fetch_latest_quilt_pick(&[]), None);
    }

    #[test]
    fn cache_ttl_is_one_day() {
        assert_eq!(CACHE_TTL_SECS, 24 * 3600);
    }
}
