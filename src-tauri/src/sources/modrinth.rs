use crate::dto::instance::GameVersionOption;
use crate::dto::project_detail::{BodyFormat, GalleryItem, ModDetail, ModVersionSummary, suggest_instance_from_mod};
use crate::dto::{
    ContentType, ModLoader, ModSearchQuery, ModSearchResult, ModSource, ModSummary, SortIndex,
};
use crate::instances::ResolvedDownload;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const BASE_URL: &str = "https://api.modrinth.com/v2";
const USER_AGENT: &str = "Waybound/0.1.0 (personal mod manager; contact: local)";

#[derive(Debug, Error)]
pub enum ModrinthError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("failed to parse Modrinth response: {0}")]
    Decode(String),
    #[error("no compatible Modrinth file found")]
    NotFound,
}

pub struct ModrinthClient {
    http: Client,
}

/// A Modrinth project's own id + name + icon, resolved by content hash —
/// the `.mrpack` index has none of these itself, just download URLs and
/// hashes. `project_id` is what lets an `.mrpack`-installed mod be tracked
/// against its real project afterward (for "check for updates"), instead of
/// falling back to an untrackable bare-filename record.
#[derive(Debug, Clone)]
pub struct ModrinthProjectMeta {
    pub project_id: String,
    pub name: String,
    pub icon: Option<String>,
}

impl ModrinthClient {
    pub fn new() -> Result<Self, ModrinthError> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .build()?;
        Ok(Self { http })
    }

    pub async fn search(&self, query: &ModSearchQuery) -> Result<ModSearchResult, ModrinthError> {
        let facets = build_facets(query);
        let index = sort_to_index(query.sort);

        let response = self
            .http
            .get(format!("{BASE_URL}/search"))
            .query(&[
                ("query", query.query.as_str()),
                ("facets", &facets),
                ("index", index),
                ("offset", &query.offset.to_string()),
                ("limit", &query.limit.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?;

        let payload: ModrinthSearchResponse = response.json().await?;

        Ok(ModSearchResult {
            hits: payload
                .hits
                .into_iter()
                .map(map_hit)
                .collect(),
            offset: payload.offset,
            limit: payload.limit,
            total_hits: payload.total_hits,
            warnings: Vec::new(),
        })
    }

    pub async fn list_game_versions(&self) -> Result<Vec<GameVersionOption>, ModrinthError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/tag/game_version"))
            .send()
            .await?
            .error_for_status()?;

        let tags: Vec<ModrinthGameVersionTag> = response.json().await?;
        let mut versions: Vec<GameVersionOption> = tags
            .into_iter()
            .filter(|tag| tag.version_type == "release")
            .filter(|tag| is_release_version_id(&tag.version))
            .map(|tag| GameVersionOption {
                version: tag.version,
                version_type: tag.version_type,
            })
            .collect();

        versions.sort_by(|a, b| compare_mc_versions(&b.version, &a.version));
        versions.truncate(50);
        Ok(versions)
    }

    pub async fn fetch_project_detail(&self, summary: &ModSummary) -> Result<ModDetail, ModrinthError> {
        let project_id = summary
            .modrinth_id
            .as_deref()
            .unwrap_or(summary.slug.as_str());
        let response = self
            .http
            .get(format!("{BASE_URL}/project/{project_id}"))
            .send()
            .await?
            .error_for_status()?;
        let project: ModrinthProject = decode_json(response).await?;
        let versions = self.fetch_all_versions(project_id).await?;

        let version_summaries: Vec<ModVersionSummary> = versions
            .iter()
            .take(25)
            .map(map_version_summary)
            .collect();

        let mut game_versions: Vec<String> = versions
            .iter()
            .flat_map(|v| v.game_versions.clone())
            .filter(|v| is_release_version_id(v))
            .collect();
        game_versions.sort_by(|a, b| compare_mc_versions(b, a));
        game_versions.dedup();

        let mut loaders = from_modrinth_categories(&project.categories);
        if loaders.is_empty() {
            loaders = versions
                .iter()
                .flat_map(|v| {
                    v.loaders
                        .iter()
                        .filter_map(|l| ModLoader::from_modrinth(l))
                        .collect::<Vec<_>>()
                })
                .collect();
            loaders.sort_by_key(|l| format!("{l:?}"));
            loaders.dedup();
        }

        let (mc, loader) = pick_suggested_mc_loader(&versions, &loaders);
        let body = project.body.unwrap_or_else(|| project.description.clone());
        let external_url = project.url.or_else(|| {
            Some(format!("https://modrinth.com/project/{project_id}"))
        });

        Ok(ModDetail {
            summary: summary.clone(),
            body,
            body_format: BodyFormat::Markdown,
            categories: project.categories,
            game_versions,
            loaders,
            external_url: external_url.clone(),
            comments_url: external_url,
            gallery: project
                .gallery
                .into_iter()
                .map(|item| GalleryItem {
                    url: item.url,
                    title: item.title,
                    description: item.description,
                    thumbnail_url: None,
                })
                .collect(),
            versions: version_summaries,
            suggested_instance: suggest_instance_from_mod(summary, &mc, loader),
        })
    }

    pub async fn fetch_version_detail(
        &self,
        version_id: &str,
    ) -> Result<ModrinthVersion, ModrinthError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/version/{version_id}"))
            .send()
            .await?
            .error_for_status()?;
        Ok(decode_json(response).await?)
    }

    pub async fn fetch_version_changelog(
        &self,
        version_id: &str,
    ) -> Result<Option<String>, ModrinthError> {
        Ok(self.fetch_version_detail(version_id).await?.changelog)
    }

    pub fn version_download_url(&self, version: &ModrinthVersion) -> Option<String> {
        version_to_download(version).map(|download| download.url)
    }

    pub async fn resolve_version_by_id(
        &self,
        version_id: &str,
    ) -> Result<ResolvedDownload, ModrinthError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/version/{version_id}"))
            .send()
            .await?
            .error_for_status()?;
        let version: ModrinthVersion = decode_json(response).await?;
        version_to_download(&version).ok_or(ModrinthError::NotFound)
    }

    pub async fn resolve_download(
        &self,
        project_id: &str,
        mc_version: &str,
        loader: ModLoader,
        content_type: ContentType,
    ) -> Result<ResolvedDownload, ModrinthError> {
        if content_type == ContentType::Modpack {
            if let Ok(download) = self
                .query_versions(project_id, Some(mc_version), None)
                .await
            {
                return Ok(download);
            }
            return self.query_versions(project_id, None, None).await;
        }

        let loader_name = loader.as_modrinth();
        if let Ok(download) = self
            .query_versions(project_id, Some(mc_version), Some(loader_name))
            .await
        {
            return Ok(download);
        }
        if let Ok(download) = self
            .query_versions(project_id, Some(mc_version), None)
            .await
        {
            return Ok(download);
        }

        let versions = self.fetch_all_versions(project_id).await?;
        pick_mod_version(&versions, mc_version, loader)
            .ok_or(ModrinthError::NotFound)
    }

    /// Every file's own project name + icon, resolved by content hash —
    /// used by the `.mrpack` importer, whose index carries only download
    /// URLs and per-file hashes, no project id, name, or icon at all (unlike
    /// CurseForge's manifest, which lists `projectID` directly). Two batch
    /// calls total regardless of file count: hash -> project id, then
    /// project id -> name/icon.
    pub async fn project_meta_by_sha1(&self, sha1_hashes: &[String]) -> std::collections::HashMap<String, ModrinthProjectMeta> {
        if sha1_hashes.is_empty() {
            return std::collections::HashMap::new();
        }
        #[derive(Serialize)]
        struct HashLookupBody<'a> {
            hashes: &'a [String],
            algorithm: &'a str,
        }
        #[derive(Deserialize)]
        struct VersionFileLookup {
            project_id: String,
        }
        #[derive(Deserialize)]
        struct ProjectLookup {
            id: String,
            title: String,
            icon_url: Option<String>,
        }

        // Modrinth documents no hard cap on either endpoint below, but a
        // single request for a whole large modpack's worth of hashes/ids is
        // exactly the shape that silently lost data on CurseForge's batch
        // endpoints (see BATCH_CHUNK_SIZE in sources/curseforge.rs) — chunk
        // both requests the same way rather than assume Modrinth is immune.
        const CHUNK_SIZE: usize = 200;

        let mut by_hash: std::collections::HashMap<String, VersionFileLookup> = std::collections::HashMap::new();
        for chunk in sha1_hashes.chunks(CHUNK_SIZE) {
            let Ok(response) = self
                .http
                .post(format!("{BASE_URL}/version_files"))
                .json(&HashLookupBody { hashes: chunk, algorithm: "sha1" })
                .send()
                .await
            else {
                continue;
            };
            if let Ok(part) = response.json::<std::collections::HashMap<String, VersionFileLookup>>().await {
                by_hash.extend(part);
            }
        }

        let mut project_ids: Vec<&str> = by_hash.values().map(|v| v.project_id.as_str()).collect();
        project_ids.sort_unstable();
        project_ids.dedup();
        if project_ids.is_empty() {
            return std::collections::HashMap::new();
        }

        let mut project_meta: std::collections::HashMap<String, ModrinthProjectMeta> = std::collections::HashMap::new();
        for chunk in project_ids.chunks(CHUNK_SIZE) {
            let Ok(ids_json) = serde_json::to_string(chunk) else { continue };
            let Ok(response) = self
                .http
                .get(format!("{BASE_URL}/projects"))
                .query(&[("ids", ids_json.as_str())])
                .send()
                .await
            else {
                continue;
            };
            let Ok(projects) = response.json::<Vec<ProjectLookup>>().await else { continue };
            project_meta.extend(projects.into_iter().map(|p| {
                (
                    p.id.clone(),
                    ModrinthProjectMeta { project_id: p.id, name: p.title, icon: p.icon_url },
                )
            }));
        }

        if project_meta.len() < project_ids.len() {
            crate::activity::append_log(
                &format!(
                    "Modrinth project_meta_by_sha1: requested {} project ids, got metadata for {} — some names/icons may fall back to filenames",
                    project_ids.len(),
                    project_meta.len()
                ),
                "warn",
                None,
            );
        }

        by_hash
            .into_iter()
            .filter_map(|(hash, v)| project_meta.get(&v.project_id).map(|meta| (hash, meta.clone())))
            .collect()
    }

    pub(crate) async fn query_versions(
        &self,
        project_id: &str,
        mc_version: Option<&str>,
        loader: Option<&str>,
    ) -> Result<ResolvedDownload, ModrinthError> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(version) = mc_version.filter(|v| !v.is_empty()) {
            query.push((
                "game_versions",
                serde_json::to_string(&[version]).unwrap_or_else(|_| "[]".into()),
            ));
        }
        if let Some(loader) = loader.filter(|v| !v.is_empty() && *v != "minecraft") {
            query.push((
                "loaders",
                serde_json::to_string(&[loader]).unwrap_or_else(|_| "[]".into()),
            ));
        }

        let mut request = self
            .http
            .get(format!("{BASE_URL}/project/{project_id}/version"));
        for (key, value) in &query {
            request = request.query(&[(key, value.as_str())]);
        }

        let response = request.send().await?.error_for_status()?;
        let versions: Vec<ModrinthVersion> = decode_json(response).await?;
        versions
            .first()
            .and_then(version_to_download)
            .ok_or(ModrinthError::NotFound)
    }

    async fn fetch_all_versions(&self, project_id: &str) -> Result<Vec<ModrinthVersion>, ModrinthError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/project/{project_id}/version"))
            .send()
            .await?
            .error_for_status()?;
        Ok(decode_json(response).await?)
    }
}

async fn decode_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ModrinthError> {
    let url = response.url().to_string();
    response
        .json::<T>()
        .await
        .map_err(|err| ModrinthError::Decode(format!("{url}: {err}")))
}

fn build_facets(query: &ModSearchQuery) -> String {
    let mut facets: Vec<Vec<String>> = Vec::new();

    if let Some(content_type) = query.content_type {
        facets.push(vec![format!("project_type:{}", content_type.as_modrinth())]);
    }

    if let Some(loader) = query.loader {
        facets.push(vec![format!("categories:{}", loader.as_modrinth())]);
    }

    serde_json::to_string(&facets).unwrap_or_else(|_| "[]".to_string())
}

fn sort_to_index(sort: SortIndex) -> &'static str {
    match sort {
        SortIndex::Relevance => "relevance",
        SortIndex::Downloads => "downloads",
        SortIndex::Updated => "updated",
        SortIndex::New => "newest",
    }
}

fn map_hit(hit: ModrinthHit) -> ModSummary {
    ModSummary {
        uid: format!("modrinth:{}", hit.project_id),
        slug: hit.slug,
        name: hit.title,
        description: hit.description,
        author: hit.author,
        icon_url: if hit.icon_url.is_empty() {
            None
        } else {
            Some(hit.icon_url)
        },
        downloads: hit.downloads,
        project_type: ContentType::from_modrinth_categories(&hit.project_type, &hit.categories),
        loaders: from_modrinth_categories(&hit.categories),
        sources: vec![ModSource::Modrinth],
        updated_at: hit.date_modified,
        curseforge_id: None,
        modrinth_id: Some(hit.project_id),
    }
}

#[derive(Debug, Deserialize)]
struct ModrinthSearchResponse {
    hits: Vec<ModrinthHit>,
    offset: u32,
    limit: u32,
    total_hits: u32,
}

#[derive(Debug, Deserialize)]
struct ModrinthHit {
    project_id: String,
    slug: String,
    title: String,
    description: String,
    author: String,
    icon_url: String,
    downloads: u64,
    project_type: String,
    categories: Vec<String>,
    date_modified: String,
}

#[derive(Debug, Deserialize)]
struct ModrinthGameVersionTag {
    version: String,
    #[serde(rename = "version_type")]
    version_type: String,
}

#[derive(Debug, Deserialize)]
pub struct ModrinthVersion {
    pub id: String,
    pub name: String,
    pub version_number: String,
    pub date_published: String,
    #[serde(default)]
    pub changelog: Option<String>,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<ModrinthVersionDependency>,
    #[serde(default)]
    pub files: Vec<ModrinthVersionFile>,
}

#[derive(Debug, Deserialize)]
pub struct ModrinthVersionDependency {
    pub project_id: Option<String>,
    #[serde(default)]
    pub dependency_type: String,
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModrinthProject {
    description: String,
    body: Option<String>,
    #[serde(default)]
    categories: Vec<String>,
    url: Option<String>,
    #[serde(default)]
    gallery: Vec<ModrinthGalleryItem>,
}

#[derive(Debug, Deserialize)]
struct ModrinthGalleryItem {
    url: String,
    title: Option<String>,
    description: Option<String>,
}

fn map_version_summary(version: &ModrinthVersion) -> ModVersionSummary {
    ModVersionSummary {
        id: version.id.clone(),
        name: version.name.clone(),
        version_number: version.version_number.clone(),
        published_at: version.date_published.clone(),
        game_versions: version.game_versions.clone(),
        loaders: version
            .loaders
            .iter()
            .filter_map(|l| ModLoader::from_modrinth(l))
            .collect(),
        downloads: version.downloads,
        changelog: version.changelog.clone(),
    }
}

fn pick_suggested_mc_loader(
    versions: &[ModrinthVersion],
    loaders: &[ModLoader],
) -> (String, ModLoader) {
    let mc = versions
        .iter()
        .flat_map(|v| v.game_versions.iter())
        .find(|v| is_release_version_id(v))
        .cloned()
        .unwrap_or_else(|| "1.21.1".to_string());
    let loader = loaders
        .first()
        .copied()
        .or_else(|| {
            versions
                .iter()
                .flat_map(|v| v.loaders.iter())
                .find_map(|l| ModLoader::from_modrinth(l))
        })
        .unwrap_or(ModLoader::Fabric);
    (mc, loader)
}

fn version_to_download(version: &ModrinthVersion) -> Option<ResolvedDownload> {
    let file = version
        .files
        .iter()
        .find(|file| file.primary)
        .or_else(|| version.files.first())?;
    Some(ResolvedDownload {
        url: file.url.clone(),
        filename: file.filename.clone(),
    })
}

fn pick_mod_version(
    versions: &[ModrinthVersion],
    mc_version: &str,
    loader: ModLoader,
) -> Option<ResolvedDownload> {
    let loader_name = loader.as_modrinth();
    for version in versions {
        if !version.game_versions.iter().any(|v| v == mc_version) {
            continue;
        }
        if version.loaders.iter().any(|l| l == loader_name)
            || loader == ModLoader::Vanilla
        {
            if let Some(download) = version_to_download(version) {
                return Some(download);
            }
        }
    }
    for version in versions {
        if version.game_versions.iter().any(|v| v == mc_version) {
            if let Some(download) = version_to_download(version) {
                return Some(download);
            }
        }
    }
    versions.first().and_then(version_to_download)
}

#[derive(Debug, Deserialize)]
pub struct ModrinthVersionFile {
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
}

impl ContentType {
    fn as_modrinth(self) -> &'static str {
        match self {
            ContentType::Mod => "mod",
            ContentType::Modpack => "modpack",
            ContentType::Resourcepack => "resourcepack",
            ContentType::Shader => "shader",
        }
    }

    fn from_modrinth_categories(project_type: &str, categories: &[String]) -> Self {
        match project_type {
            "modpack" => ContentType::Modpack,
            "shader" => ContentType::Shader,
            _ if categories.iter().any(|c| c == "resourcepack") => ContentType::Resourcepack,
            _ => ContentType::Mod,
        }
    }
}

fn from_modrinth_categories(categories: &[String]) -> Vec<ModLoader> {
    let mut loaders = Vec::new();
    for category in categories {
        let loader = match category.as_str() {
            "fabric" => Some(ModLoader::Fabric),
            "forge" => Some(ModLoader::Forge),
            "neoforge" => Some(ModLoader::NeoForge),
            "quilt" => Some(ModLoader::Quilt),
            _ => None,
        };
        if let Some(loader) = loader {
            if !loaders.contains(&loader) {
                loaders.push(loader);
            }
        }
    }
    loaders
}

fn is_release_version_id(version: &str) -> bool {
    version
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_digit())
}

fn compare_mc_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parts_a = parse_mc_version(a);
    let parts_b = parse_mc_version(b);
    parts_a.cmp(&parts_b)
}

fn parse_mc_version(version: &str) -> (u32, u32, u32) {
    let mut numbers = version.split('.').filter_map(|part| part.parse::<u32>().ok());
    (
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
    )
}

#[cfg(test)]
mod version_tests {
    use super::{compare_mc_versions, is_release_version_id};

    #[test]
    fn rejects_beta_style_versions() {
        assert!(!is_release_version_id("b1.8.1"));
        assert!(is_release_version_id("1.21.1"));
    }

    #[test]
    fn sorts_versions_newest_first() {
        assert_eq!(
            compare_mc_versions("1.21.1", "1.20.4"),
            std::cmp::Ordering::Greater
        );
    }
}
