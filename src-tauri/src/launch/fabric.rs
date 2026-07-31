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

#[cfg(test)]
mod profile_merge_tests {
    use super::*;
    use crate::launch::manifest::{ArgValue, Argument};

    fn json(raw: &str) -> VersionJson {
        serde_json::from_str(raw).expect("fixture is valid version JSON")
    }

    fn vanilla() -> VersionJson {
        json(r#"{
            "id": "1.20.1",
            "type": "release",
            "mainClass": "net.minecraft.client.main.Main",
            "javaVersion": { "component": "java-runtime-gamma", "majorVersion": 17 },
            "assetIndex": { "id": "5", "url": "https://example.invalid/5.json", "sha1": "abc" },
            "downloads": { "client": { "url": "https://example.invalid/client.jar", "sha1": "deadbeef" } },
            "libraries": [{ "name": "com.mojang:logging:1.1.1" }],
            "arguments": { "game": ["--username", "${auth_player_name}"], "jvm": ["-cp", "${classpath}"] }
        }"#)
    }

    fn fabric_profile() -> VersionJson {
        json(r#"{
            "id": "fabric-loader-0.15.7-1.20.1",
            "inheritsFrom": "1.20.1",
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "libraries": [
                { "name": "net.fabricmc:fabric-loader:0.15.7", "url": "https://maven.fabricmc.net/" }
            ],
            "arguments": { "game": [], "jvm": ["-DFabricMcEmu=net.minecraft.client.main.Main"] }
        }"#)
    }

    #[test]
    fn the_merged_profile_takes_the_childs_identity_and_stops_inheriting() {
        let merged = merge_onto_parent(fabric_profile(), vanilla());
        assert_eq!(merged.id, "fabric-loader-0.15.7-1.20.1");
        assert!(
            merged.inherits_from.is_none(),
            "a merged profile must not be resolved a second time"
        );
        assert_eq!(
            merged.main_class.as_deref(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
        );
    }

    #[test]
    fn assets_client_download_and_java_version_still_come_from_vanilla() {
        let merged = merge_onto_parent(fabric_profile(), vanilla());
        assert_eq!(merged.asset_index.as_ref().unwrap().id, "5");
        assert_eq!(
            merged.downloads.as_ref().unwrap().client.as_ref().unwrap().sha1.as_deref(),
            Some("deadbeef")
        );
        assert_eq!(merged.java_version.as_ref().unwrap().major_version, Some(17));
        // The child declared no `type`, so vanilla's survives.
        assert_eq!(merged.version_type.as_deref(), Some("release"));
    }

    #[test]
    fn loader_libraries_are_listed_before_vanilla_ones() {
        let merged = merge_onto_parent(fabric_profile(), vanilla());
        let names: Vec<&str> = merged.libraries.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["net.fabricmc:fabric-loader:0.15.7", "com.mojang:logging:1.1.1"]);
    }

    #[test]
    fn arguments_from_both_sides_are_concatenated_parent_first() {
        let merged = merge_onto_parent(fabric_profile(), vanilla());
        let args = merged.arguments.as_ref().unwrap();
        let jvm: Vec<String> = args
            .jvm
            .iter()
            .map(|a| match a {
                Argument::Plain(s) => s.clone(),
                Argument::Conditional { value: ArgValue::Single(s), .. } => s.clone(),
                Argument::Conditional { value: ArgValue::Many(v), .. } => v.join(" "),
            })
            .collect();
        assert_eq!(jvm, ["-cp", "${classpath}", "-DFabricMcEmu=net.minecraft.client.main.Main"]);
        assert_eq!(args.game.len(), 2, "vanilla's game args must survive an empty child list");
    }

    #[test]
    fn a_child_without_arguments_keeps_the_parents_untouched() {
        let child = json(r#"{ "id": "child", "inheritsFrom": "1.20.1" }"#);
        let merged = merge_onto_parent(child, vanilla());
        let args = merged.arguments.as_ref().unwrap();
        assert_eq!(args.game.len(), 2);
        assert_eq!(args.jvm.len(), 2);
        // No mainClass on the child means vanilla's stays in place.
        assert_eq!(merged.main_class.as_deref(), Some("net.minecraft.client.main.Main"));
    }

    #[test]
    fn a_child_with_arguments_over_a_legacy_parent_without_them_supplies_them() {
        let parent = json(r#"{ "id": "1.12.2", "minecraftArguments": "--username ${auth_player_name}" }"#);
        let merged = merge_onto_parent(fabric_profile(), parent);
        assert!(merged.arguments.is_some());
        // The parent's legacy string is untouched when the child has none.
        assert_eq!(
            merged.minecraft_arguments.as_deref(),
            Some("--username ${auth_player_name}")
        );
    }

    #[test]
    fn a_childs_legacy_argument_string_replaces_the_parents_wholesale() {
        let parent = json(r#"{ "id": "1.12.2", "minecraftArguments": "--username ${auth_player_name}" }"#);
        let child = json(r#"{ "id": "1.12.2-forge", "minecraftArguments": "--tweakClass forge" }"#);
        let merged = merge_onto_parent(child, parent);
        assert_eq!(merged.minecraft_arguments.as_deref(), Some("--tweakClass forge"));
    }
}
