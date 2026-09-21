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

/// URL of the profile JSON for a (game, loader) pair. Split out so the launch
/// pipeline can cache the same URL it would have fetched.
pub fn profile_url(game_version: &str, loader_version: &str) -> String {
    format!("{FABRIC_META}/loader/{game_version}/{loader_version}/profile/json")
}

/// Fabric and Quilt both publish vanilla-compatible inheritance profiles.
pub async fn cached_profile(
    client: &Client,
    root: &std::path::Path,
    game_version: &str,
    requested: Option<String>,
    quilt: bool,
) -> Result<VersionJson, LaunchError> {
    let kind = if quilt { "quilt" } else { "fabric" };
    let base = if quilt { "https://meta.quiltmc.org/v3/versions" } else { FABRIC_META };
    let coordinate = if quilt { "org.quiltmc:quilt-loader:" } else { "net.fabricmc:fabric-loader:" };
    let selection_key = format!("{kind}/{game_version}/selected.json");
    let requested = requested.filter(|v| !v.trim().is_empty());
    let mut selected = requested.clone().or_else(|| super::offline::read_json(root, &selection_key));
    // Retrofit standard launcher profiles and the cache from earlier builds.
    let candidates = [root.join("versions"), super::offline::cache_root(root).join(kind).join(game_version)];
    for directory in candidates {
        let Ok(entries) = std::fs::read_dir(directory) else { continue };
        for entry in entries.flatten() {
            let path = if entry.path().is_dir() {
                entry.path().join(format!("{}.json", entry.file_name().to_string_lossy()))
            } else { entry.path() };
            let Ok(raw) = std::fs::read(path) else { continue };
            let Ok(profile) = serde_json::from_slice::<VersionJson>(&raw) else { continue };
            if profile.inherits_from.as_deref() != Some(game_version) { continue; }
            let Some(version) = profile.libraries.iter().find_map(|lib| lib.name.strip_prefix(coordinate)) else { continue };
            if selected.as_deref().is_some_and(|wanted| wanted != version) { continue; }
            super::offline::put_bytes(root, &format!("{kind}/{game_version}/{version}.json"), &raw)?;
            selected = Some(version.to_owned());
            break;
        }
        if selected.is_some() { break; }
    }
    let version = match selected {
        Some(version) => version,
        None => {
            let entries: Vec<LoaderEntry> = super::offline::fetch_json_cached(
                client, root, &format!("{kind}/{game_version}/loaders.json"),
                &format!("{base}/loader/{game_version}"),
            ).await?;
            entries.into_iter().map(|entry| entry.loader.version)
                .max_by_key(|version| (!version.contains('-'), version.split('.').map(|part| part.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>()))
                .ok_or_else(|| LaunchError::Parse(format!("No {kind} loader supports Minecraft {game_version}")))?
        }
    };
    let profile: VersionJson = super::offline::fetch_json_cached(
        client, root, &format!("{kind}/{game_version}/{version}.json"),
        &format!("{base}/loader/{game_version}/{version}/profile/json"),
    ).await?;
    validate_profile(&profile, game_version, &format!("{coordinate}{version}"))?;
    super::offline::put_bytes(root, &selection_key, &serde_json::to_vec(&version).map_err(|e| LaunchError::Parse(e.to_string()))?)?;
    Ok(profile)
}

fn validate_profile(profile: &VersionJson, game_version: &str, coordinate: &str) -> Result<(), LaunchError> {
    if profile.inherits_from.as_deref() != Some(game_version)
        || profile.main_class.as_deref().is_none_or(str::is_empty)
        || !profile.libraries.iter().any(|lib| lib.name == coordinate)
    {
        return Err(LaunchError::Parse(format!("Loader profile does not match Minecraft {game_version} and {coordinate}")));
    }
    Ok(())
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
#[cfg(test)]
mod loader_cache_tests {
    use super::*;
    use super::super::offline;

    fn loader_version() -> String {
        "1.0".into()
    }

    fn profile(game_version: &str, coordinate: &str) -> String {
        format!(
            r#"{{"id":"loader-{game_version}","inheritsFrom":"{game_version}","mainClass":"knot.KnotClient","libraries":[{{"name":"{coordinate}"}}]}}"#
        )
    }

    #[tokio::test]
    async fn restart_reuses_selected_loader_cache_without_network() {
        let dir = tempfile::tempdir().unwrap();
        offline::put_bytes(
            dir.path(),
            "quilt/1.20.1/1.0.json",
            profile("1.20.1", "org.quiltmc:quilt-loader:1.0").as_bytes(),
        )
        .unwrap();
        offline::put_bytes(dir.path(), "quilt/1.20.1/selected.json", br#""1.0""#).unwrap();
        let client = reqwest::Client::new();
        let resolved = cached_profile(&client, dir.path(), "1.20.1", None, true).await.unwrap();
        assert_eq!(resolved.main_class.as_deref(), Some("knot.KnotClient"));
        assert_eq!(resolved.id, "loader-1.20.1");
        assert!(!offline::cache_root(dir.path()).join("quilt/1.20.1/loaders.json").exists());
    }

    #[tokio::test]
    async fn profile_mismatch_is_rejected_not_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        offline::put_bytes(
            dir.path(),
            "fabric/1.20.1/1.0.json",
            profile("1.19.4", "net.fabricmc:fabric-loader:1.0").as_bytes(),
        )
        .unwrap();
        offline::put_bytes(dir.path(), "fabric/1.20.1/selected.json", br#""1.0""#).unwrap();
        let error = cached_profile(&reqwest::Client::new(), dir.path(), "1.20.1", None, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not match"), "{error}");
    }

    #[tokio::test]
    async fn pinned_request_must_not_accept_a_different_loader_profile() {
        let dir = tempfile::tempdir().unwrap();
        offline::put_bytes(
            dir.path(),
            "fabric/1.20.1/1.0.json",
            profile("1.20.1", "net.fabricmc:fabric-loader:2.0").as_bytes(),
        )
        .unwrap();
        let client = reqwest::Client::new();
        let error = cached_profile(&client, dir.path(), "1.20.1", Some(loader_version()), false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Loader profile does not match"), "{error}");
    }
}
