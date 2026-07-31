use serde::{Deserialize, Serialize};

use super::{ModLoader, ModSummary};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GalleryItem {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModDetail {
    pub summary: ModSummary,
    pub body: String,
    pub body_format: BodyFormat,
    pub categories: Vec<String>,
    pub game_versions: Vec<String>,
    pub loaders: Vec<ModLoader>,
    pub external_url: Option<String>,
    pub comments_url: Option<String>,
    pub gallery: Vec<GalleryItem>,
    pub versions: Vec<ModVersionSummary>,
    pub suggested_instance: SuggestedInstance,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BodyFormat {
    Markdown,
    Html,
    Plain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModVersionSummary {
    pub id: String,
    pub name: String,
    pub version_number: String,
    pub published_at: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<ModLoader>,
    pub downloads: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModpackContentKind {
    Mod,
    Datapack,
    Resourcepack,
    Shader,
    World,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackContentItem {
    pub id: String,
    pub name: String,
    pub file_name: String,
    pub author: Option<String>,
    pub kind: ModpackContentKind,
    pub required: bool,
    pub env_client: Option<String>,
    pub env_server: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackContentResponse {
    pub version_id: String,
    pub version_name: String,
    pub items: Vec<ModpackContentItem>,
    pub counts: ModpackContentCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackContentCounts {
    pub mods: u32,
    pub datapacks: u32,
    pub resourcepacks: u32,
    pub shaders: u32,
    pub worlds: u32,
    pub other: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityLogEntry {
    pub timestamp: i64,
    pub level: String,
    pub message: String,
    pub project_uid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedInstance {
    pub name: String,
    pub minecraft_version: String,
    pub loader: ModLoader,
}

pub fn suggest_instance_from_mod(summary: &ModSummary, mc: &str, loader: ModLoader) -> SuggestedInstance {
    SuggestedInstance {
        name: summary.name.clone(),
        minecraft_version: mc.to_string(),
        loader,
    }
}

#[cfg(test)]
mod suggested_instance_tests {
    use super::*;
    use crate::dto::{ContentType, ModSource};

    fn summary(name: &str) -> ModSummary {
        ModSummary {
            uid: "modrinth:AABBCCDD".to_string(),
            slug: "some-slug".to_string(),
            name: name.to_string(),
            description: "desc".to_string(),
            author: "author".to_string(),
            icon_url: None,
            downloads: 42,
            project_type: ContentType::Mod,
            loaders: vec![ModLoader::Fabric],
            sources: vec![ModSource::Modrinth],
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            curseforge_id: None,
            modrinth_id: Some("AABBCCDD".to_string()),
        }
    }

    #[test]
    fn suggestion_takes_the_project_name_and_the_caller_supplied_version_and_loader() {
        let suggested = suggest_instance_from_mod(&summary("Sodium"), "1.21.1", ModLoader::Fabric);
        assert_eq!(suggested.name, "Sodium");
        assert_eq!(suggested.minecraft_version, "1.21.1");
        assert_eq!(suggested.loader, ModLoader::Fabric);
    }

    #[test]
    fn the_mods_own_declared_loaders_do_not_override_the_requested_loader() {
        // `summary` advertises Fabric only; the caller's choice still wins so
        // the suggestion matches the instance the user is actually creating.
        let suggested = suggest_instance_from_mod(&summary("Create"), "1.20.1", ModLoader::NeoForge);
        assert_eq!(suggested.loader, ModLoader::NeoForge);
    }

    #[test]
    fn odd_project_names_are_carried_through_verbatim_without_sanitizing() {
        // Nothing here is filesystem-safe yet — instance directories are named
        // from a generated id, not this string, so the suggestion stays exactly
        // what the user sees on the project page.
        for name in [
            "",
            "   ",
            "Just Enough Items (JEI)",
            "Mod: The/Sequel\\Part 2",
            "..",
            "CON",
            "日本語のモッド",
            "emoji \u{1f9ea} pack",
        ] {
            let suggested = suggest_instance_from_mod(&summary(name), "1.20.1", ModLoader::Forge);
            assert_eq!(suggested.name, name);
        }
    }

    #[test]
    fn minecraft_version_is_copied_as_given_including_snapshots_and_empty() {
        for mc in ["1.7.10", "24w14craftmine", "1.21.4-rc1", ""] {
            let suggested = suggest_instance_from_mod(&summary("Any"), mc, ModLoader::Quilt);
            assert_eq!(suggested.minecraft_version, mc);
        }
    }

    #[test]
    fn suggestion_serializes_with_the_camel_case_keys_the_frontend_reads() {
        let suggested = suggest_instance_from_mod(&summary("Iris"), "1.21", ModLoader::Vanilla);
        let json = serde_json::to_value(&suggested).unwrap();
        assert_eq!(json["name"], "Iris");
        assert_eq!(json["minecraftVersion"], "1.21");
        assert_eq!(json["loader"], "vanilla");
    }
}
