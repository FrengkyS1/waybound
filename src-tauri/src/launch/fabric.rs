//! Fabric loader support.
//!
//! Fabric publishes, per (game version, loader version), a "profile JSON" in the
//! exact same shape as a vanilla version JSON but with `inheritsFrom` set to the
//! game version, its own `mainClass`, and extra `libraries` (with maven `url`s
//! instead of `downloads` blocks). We fetch it and merge onto vanilla.

use reqwest::Client;

use super::manifest::VersionJson;
use super::LaunchError;

const FABRIC_META: &str = "https://meta.fabricmc.net/v2/versions";

#[derive(serde::Deserialize)]
struct LoaderEntry {
    loader: LoaderInfo,
}

#[derive(serde::Deserialize)]
struct LoaderInfo {
    version: String,
}

/// Resolve the newest stable Fabric loader version for a game version.
pub async fn latest_loader_version(
    client: &Client,
    game_version: &str,
) -> Result<String, LaunchError> {
    let url = format!("{FABRIC_META}/loader/{game_version}");
    let entries: Vec<LoaderEntry> = client
        .get(&url)
        .send()
        .await?
        .json()
        .await
        .map_err(|e| LaunchError::Parse(format!("fabric loader list: {e}")))?;
    entries
        .into_iter()
        .next()
        .map(|e| e.loader.version)
        .ok_or_else(|| LaunchError::Parse(format!("no Fabric loader for {game_version}")))
}

/// Fetch the Fabric profile JSON that layers onto vanilla for the given
/// game + loader versions.
pub async fn fetch_profile(
    client: &Client,
    game_version: &str,
    loader_version: &str,
) -> Result<VersionJson, LaunchError> {
    let url = format!("{FABRIC_META}/loader/{game_version}/{loader_version}/profile/json");
    let profile: VersionJson = client
        .get(&url)
        .send()
        .await?
        .json()
        .await
        .map_err(|e| LaunchError::Parse(format!("fabric profile: {e}")))?;
    Ok(profile)
}

/// Merge a Fabric (or any `inheritsFrom`) profile onto its parent vanilla JSON.
/// Child values win for scalars; libraries and arguments are concatenated with
/// the child taking precedence (its mainClass, its libs listed first).
pub fn merge_onto_parent(child: VersionJson, parent: VersionJson) -> VersionJson {
    let mut merged = parent;

    merged.id = child.id;
    merged.inherits_from = None;
    if child.main_class.is_some() {
        merged.main_class = child.main_class;
    }
    if child.version_type.is_some() {
        merged.version_type = child.version_type;
    }
    // Assets / client downloads / javaVersion come from the parent (vanilla).

    // Child libraries first so the loader's classes shadow vanilla where needed.
    let mut libraries = child.libraries;
    libraries.extend(merged.libraries);
    merged.libraries = libraries;

    // Merge structured arguments if either side has them.
    match (child.arguments, merged.arguments.take()) {
        (Some(child_args), Some(mut parent_args)) => {
            parent_args.game.extend(child_args.game);
            parent_args.jvm.extend(child_args.jvm);
            merged.arguments = Some(parent_args);
        }
        (Some(child_args), None) => merged.arguments = Some(child_args),
        (None, parent_args) => merged.arguments = parent_args,
    }

    if child.minecraft_arguments.is_some() {
        merged.minecraft_arguments = child.minecraft_arguments;
    }

    merged
}
