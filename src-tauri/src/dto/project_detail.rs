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
