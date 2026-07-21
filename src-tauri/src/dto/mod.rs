use serde::{Deserialize, Serialize};

pub mod instance;
pub mod project_detail;
pub mod settings;


#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModSummary {
    pub uid: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub icon_url: Option<String>,
    pub downloads: u64,
    pub project_type: ContentType,
    pub loaders: Vec<ModLoader>,
    pub sources: Vec<ModSource>,
    pub updated_at: String,
    #[serde(default)]
    pub curseforge_id: Option<u32>,
    #[serde(default)]
    pub modrinth_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModSource {
    Modrinth,
    Curseforge,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Mod,
    Modpack,
    Resourcepack,
    Shader,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ModLoader {
    Fabric,
    Forge,
    NeoForge,
    Quilt,
    Vanilla,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SortIndex {
    Relevance,
    Downloads,
    Updated,
    New,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModSearchQuery {
    pub query: String,
    pub content_type: Option<ContentType>,
    pub loader: Option<ModLoader>,
    pub sort: SortIndex,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModSearchResult {
    pub hits: Vec<ModSummary>,
    pub offset: u32,
    pub limit: u32,
    pub total_hits: u32,
    #[serde(default)]
    pub warnings: Vec<String>,
}
