use crate::dto::instance::GameVersionOption;
use crate::dto::project_detail::{BodyFormat, GalleryItem, ModDetail, ModVersionSummary, suggest_instance_from_mod};
use crate::dto::{
    ContentType, ModLoader, ModSearchQuery, ModSearchResult, ModSource, ModSummary, SortIndex,
};
use crate::instances::{ResolvedDependency, ResolvedDownload};
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
    /// The user picked (or the resolver fell through to) a version that does
    /// not list this instance's game version / loader — e.g. the Fabric build
    /// of a mod into a NeoForge instance. Distinct from NotFound so the
    /// message can point at the fix instead of implying the mod has no builds.
    #[error("that version is not built for this instance's Minecraft version and loader")]
    Incompatible,
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
        // Same reasoning as CurseForgeClient::new: this client only ever
        // fetches small JSON payloads, so a hard timeout can't break a
        // legitimate call — it only turns a silent hang into a clear error
        // the caller's existing error handling can surface.
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
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

        let mut updated_summary = summary.clone();
        updated_summary.uid = format!("modrinth:{}", project.id);
        updated_summary.modrinth_id = Some(project.id);
        updated_summary.loaders = loaders.clone();
        let (mc, loader) = pick_suggested_mc_loader(&versions, &loaders);
        let suggested_instance = suggest_instance_from_mod(&updated_summary, &mc, loader);
        let body = project.body.unwrap_or_else(|| project.description.clone());
        let external_url = project.url.or_else(|| {
            Some(format!("https://modrinth.com/project/{project_id}"))
        });

        Ok(ModDetail {
            summary: updated_summary,
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
            suggested_instance,
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
        mc_version: &str,
        loader: ModLoader,
        content_type: ContentType,
    ) -> Result<ResolvedDownload, ModrinthError> {
        let version = self.fetch_version_detail(version_id).await?;
        resolve_version_detail(&version, mc_version, loader, content_type)
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

        let required_loader = (content_type == ContentType::Mod).then(|| loader.as_modrinth());
        if let Ok(download) = self
            .query_versions(project_id, Some(mc_version), required_loader)
            .await
        {
            return Ok(download);
        }

        // Widen only the upstream query, never local compatibility checks.
        let versions = self.fetch_all_versions(project_id).await?;
        pick_compatible_version(&versions, Some(mc_version), required_loader)
            .ok_or(ModrinthError::NotFound)
    }

    /// Every file's own project name + icon, resolved by content hash —
    /// used by the `.mrpack` importer, whose index carries only download
    /// URLs and per-file hashes, no project id, name, or icon at all (unlike
    /// CurseForge's manifest, which lists `projectID` directly). Two batch
    /// calls total regardless of file count: hash -> project id, then
    /// project id -> name/icon.
    /// Full-version variant of the `project_meta_by_sha1` lookup below:
    /// `POST /version_files` returns the complete version object per hash
    /// (project id, version id/number, files), which is what identifying an
    /// untracked local jar needs — project + exact installed version in one
    /// round trip. Strict (errors propagate): callers use this for a
    /// deliberate user action, not background enrichment.
    pub async fn lookup_versions_by_hashes(
        &self,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, ModrinthVersion>, ModrinthError> {
        #[derive(Serialize)]
        struct HashLookupBody<'a> {
            hashes: &'a [String],
            algorithm: &'a str,
        }
        if hashes.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let response = self
            .http
            .post(format!("{BASE_URL}/version_files"))
            .json(&HashLookupBody { hashes, algorithm: "sha1" })
            .send()
            .await?
            .error_for_status()?;
        response
            .json::<std::collections::HashMap<String, ModrinthVersion>>()
            .await
            .map_err(|e| ModrinthError::Decode(e.to_string()))
    }

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
        if let Some(loader) = loader.filter(|v| !v.is_empty()) {
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
        let sorted = sort_versions_newest_first(versions);
        pick_compatible_version(&sorted, mc_version, loader)
            .ok_or(ModrinthError::NotFound)
    }

    /// A minimal `ModSummary` for one project id, enough to install it and
    /// record it against the instance. Used for dependency resolution, where
    /// all we start with is an id from another version's dependency list —
    /// unlike the Browse path, there's no search hit to carry the metadata.
    pub(crate) async fn fetch_project_summary(
        &self,
        project_id: &str,
    ) -> Result<ModSummary, ModrinthError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/project/{project_id}"))
            .send()
            .await?
            .error_for_status()?;
        let project: ModrinthProjectSummary = decode_json(response).await?;

        Ok(ModSummary {
            // Same shape `map_hit` produces, so a dependency-installed mod
            // is indistinguishable from a directly-installed one afterwards
            // (update checks, "open project page", removal all key off it).
            uid: format!("modrinth:{}", project.id),
            slug: project.slug,
            name: project.title,
            description: project.description,
            author: String::new(),
            icon_url: project.icon_url.filter(|url| !url.is_empty()),
            downloads: project.downloads,
            project_type: ContentType::from_modrinth_categories(
                &project.project_type,
                &project.categories,
            ),
            loaders: from_modrinth_categories(&project.categories),
            sources: vec![ModSource::Modrinth],
            updated_at: project.updated.unwrap_or_default(),
            curseforge_id: None,
            modrinth_id: Some(project.id),
        })
    }

    async fn fetch_all_versions(&self, project_id: &str) -> Result<Vec<ModrinthVersion>, ModrinthError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/project/{project_id}/version"))
            .send()
            .await?
            .error_for_status()?;
        let versions: Vec<ModrinthVersion> = decode_json(response).await?;
        Ok(sort_versions_newest_first(versions))
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

/// `/project/{id}` — the identity fields only. `ModrinthProject` above
/// deserializes the same endpoint for the detail page but deliberately skips
/// these, since that path always already has a `ModSummary` in hand.
#[derive(Debug, Deserialize)]
struct ModrinthProjectSummary {
    id: String,
    slug: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    project_type: String,
    #[serde(default)]
    updated: Option<String>,
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
    /// Present on `version_files` / version-list responses; absent nowhere
    /// that matters thanks to the default.
    #[serde(default)]
    pub project_id: String,
    /// "release" | "beta" | "alpha". Empty reads as release (fail-open).
    #[serde(default)]
    pub version_type: String,
}

#[derive(Debug, Deserialize)]
pub struct ModrinthVersionDependency {
    pub project_id: Option<String>,
    pub version_id: Option<String>,
    #[serde(default)]
    pub dependency_type: String,
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModrinthProject {
    id: String,
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

pub(crate) fn map_version_summary(version: &ModrinthVersion) -> ModVersionSummary {
    // Mirror `version_to_download`'s file choice so the exposed filename is
    // exactly what installing this version would place on disk.
    let file_name = version
        .files
        .iter()
        .find(|file| file.primary)
        .or_else(|| version.files.first())
        .map(|file| file.filename.clone());
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
        file_name,
        channel: match version.version_type.as_str() {
            "beta" => Some("beta".to_string()),
            "alpha" => Some("alpha".to_string()),
            _ => None,
        },
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

/// Requirements from this exact version. Keep version-only dependencies and
/// distinct pins for the same project; closure resolution must not pick latest.
pub(crate) fn required_dependencies_of(version: &ModrinthVersion) -> Vec<ResolvedDependency> {
    let mut out = Vec::new();
    for dep in &version.dependencies {
        if dep.dependency_type != "required" {
            continue;
        }
        let project_id = dep.project_id.clone().filter(|id| !id.is_empty());
        let version_id = dep.version_id.clone().filter(|id| !id.is_empty());
        if project_id.is_none() && version_id.is_none() {
            continue;
        }
        let required = ResolvedDependency::Modrinth { project_id, version_id };
        if !out.contains(&required) {
            out.push(required);
        }
    }
    out
}

/// Stable channel for "newest" picking: releases first, beta/alpha only
/// when no release exists (a beta-only mod still installs rather than
/// erroring). Empty reads as release so responses lacking the field never
/// demote a version.
fn is_stable_channel(version_type: &str) -> bool {
    version_type != "beta" && version_type != "alpha"
}


/// The API has no sort guarantee; normalize UTC fractions before ordering.
fn sort_versions_newest_first(mut versions: Vec<ModrinthVersion>) -> Vec<ModrinthVersion> {
    versions.sort_by(|a, b| crate::sources::updated_key(&b.date_published).cmp(&crate::sources::updated_key(&a.date_published)));
    versions
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
        curseforge_file_id: None,
        hashes: file.hashes.clone(),
        dependencies: required_dependencies_of(version),
    })
}

/// Resolve an already-fetched exact version without another API request.
pub(crate) fn resolve_version_detail(
    version: &ModrinthVersion,
    mc_version: &str,
    loader: ModLoader,
    content_type: ContentType,
) -> Result<ResolvedDownload, ModrinthError> {
    ensure_version_matches(version, mc_version, loader, content_type)?;
    version_to_download(version).ok_or(ModrinthError::NotFound)
}

fn pick_compatible_version(
    versions: &[ModrinthVersion],
    mc_version: Option<&str>,
    loader: Option<&str>,
) -> Option<ResolvedDownload> {
    // Input is newest-first. Prefer stable only within compatible versions.
    for stable_only in [true, false] {
        for version in versions {
            if stable_only && !is_stable_channel(&version.version_type) {
                continue;
            }
            if version_matches(version, mc_version, loader) {
                if let Some(download) = version_to_download(version) {
                    return Some(download);
                }
            }
        }
    }
    None
}

fn version_matches(version: &ModrinthVersion, mc_version: Option<&str>, loader: Option<&str>) -> bool {
    mc_version.filter(|value| !value.is_empty()).map_or(true, |expected| {
        version.game_versions.iter().any(|value| value == expected)
    }) && loader.filter(|value| !value.is_empty()).map_or(true, |expected| {
        version.loaders.iter().any(|value| value == expected)
    })
}

/// Pinned mods require a matching loader. Loader-agnostic content is validated
/// only against the game version, not the instance's mod loader.
fn ensure_version_matches(
    version: &ModrinthVersion,
    mc_version: &str,
    loader: ModLoader,
    content_type: ContentType,
) -> Result<(), ModrinthError> {
    let required_loader = (content_type == ContentType::Mod).then(|| loader.as_modrinth());
    if !version_matches(version, Some(mc_version), required_loader) {
        return Err(ModrinthError::Incompatible);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct ModrinthVersionFile {
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub hashes: std::collections::HashMap<String, String>,
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
            "resourcepack" => ContentType::Resourcepack,
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
    use super::{compare_mc_versions, is_release_version_id, sort_versions_newest_first, ModrinthVersion};

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

    fn version_with_date(date_published: &str) -> ModrinthVersion {
        ModrinthVersion {
            id: date_published.to_string(),
            name: String::new(),
            version_number: String::new(),
            date_published: date_published.to_string(),
            changelog: None,
            downloads: 0,
            game_versions: Vec::new(),
            loaders: Vec::new(),
            dependencies: Vec::new(),
            files: Vec::new(),
            project_id: String::new(),
            version_type: String::new(),
        }
    }

    #[test]
    fn sort_versions_newest_first_ignores_api_response_order() {
        // The actual regression: the API's own listing order can't be
        // trusted, so an out-of-order response (oldest first here) must
        // still come out newest-first.
        let versions = vec![
            version_with_date("2026-01-01T00:00:00Z"),
            version_with_date("2026-07-24T00:00:00Z"),
            version_with_date("2026-03-01T00:00:00Z"),
        ];
        let sorted = sort_versions_newest_first(versions);
        let dates: Vec<&str> = sorted.iter().map(|v| v.date_published.as_str()).collect();
        assert_eq!(
            dates,
            vec!["2026-07-24T00:00:00Z", "2026-03-01T00:00:00Z", "2026-01-01T00:00:00Z"]
        );
    }

    #[test]
    fn source_timestamp_precision_does_not_reverse_version_order() {
        let sorted = sort_versions_newest_first(vec![
            version_with_date("2026-01-01T00:00:00Z"),
            version_with_date("2026-01-01T00:00:00.001Z"),
            version_with_date("2026-01-01T00:00:00.01Z"),
        ]);
        assert_eq!(sorted[0].date_published, "2026-01-01T00:00:00.01Z");
        assert_eq!(sorted[2].date_published, "2026-01-01T00:00:00Z");
    }
}

#[cfg(test)]
mod required_dependency_tests {
    use super::{ensure_version_matches, pick_compatible_version, required_dependencies_of, resolve_version_detail, version_to_download, ContentType, ModLoader, ModrinthVersion, ResolvedDependency};

    fn fixture() -> ModrinthVersion {
        serde_json::from_value(serde_json::json!({
            "id": "chosen", "project_id": "parent", "name": "Chosen",
            "version_number": "1.0", "date_published": "2026-01-01T00:00:00Z",
            "game_versions": ["1.21.1"], "loaders": ["fabric"],
            "files": [{"url": "https://cdn.modrinth.com/chosen.jar", "filename": "chosen.jar", "primary": true}],
            "dependencies": [
                {"project_id": "library", "version_id": "exact-old-beta", "dependency_type": "required"},
                {"project_id": null, "version_id": "version-only", "dependency_type": "required"},
                {"project_id": "library", "version_id": "exact-old-beta", "dependency_type": "required"},
                {"project_id": "library", "version_id": "another-pin", "dependency_type": "required"},
                {"project_id": "optional", "version_id": "optional-pin", "dependency_type": "optional"},
                {"project_id": "bad", "dependency_type": "incompatible"},
                {"file_name": "external.jar", "dependency_type": "required"}
            ]
        })).unwrap()
    }

    #[test]
    fn resolved_download_preserves_exact_and_version_only_requirements() {
        let version = fixture();
        let expected = vec![
            ResolvedDependency::Modrinth { project_id: Some("library".into()), version_id: Some("exact-old-beta".into()) },
            ResolvedDependency::Modrinth { project_id: None, version_id: Some("version-only".into()) },
            ResolvedDependency::Modrinth { project_id: Some("library".into()), version_id: Some("another-pin".into()) },
        ];
        assert_eq!(required_dependencies_of(&version), expected);
        assert_eq!(version_to_download(&version).unwrap().dependencies, expected);
        assert_eq!(resolve_version_detail(&version, "1.21.1", ModLoader::Fabric, ContentType::Mod)
            .unwrap().dependencies, expected);
    }

    #[test]
    fn pinned_mods_reject_wrong_or_missing_loader() {
        let mut version = fixture();
        assert!(ensure_version_matches(&version, "1.21.1", ModLoader::Forge, ContentType::Mod).is_err());
        assert!(resolve_version_detail(&version, "1.21.1", ModLoader::Forge, ContentType::Mod).is_err());
        version.loaders.clear();
        assert!(ensure_version_matches(&version, "1.21.1", ModLoader::Fabric, ContentType::Mod).is_err());
        version.loaders.push("minecraft".into());
        assert!(ensure_version_matches(&version, "1.21.1", ModLoader::Fabric, ContentType::Resourcepack).is_ok());
        assert!(ensure_version_matches(&version, "1.20.1", ModLoader::Fabric, ContentType::Resourcepack).is_err());
        version.game_versions.clear();
        assert!(ensure_version_matches(&version, "1.21.1", ModLoader::Fabric, ContentType::Resourcepack).is_err());
        version.game_versions.push("1.21.1".into());
        version.loaders = vec!["fabric".into()];
        assert!(ensure_version_matches(&version, "1.21.1", ModLoader::Vanilla, ContentType::Mod).is_err());
    }

    #[test]
    fn resourcepack_project_type_needs_no_category_hint() {
        assert_eq!(ContentType::from_modrinth_categories("resourcepack", &[]), ContentType::Resourcepack);
    }

    #[test]
    fn unpinned_compatibility_is_checked_locally_before_stable_preference() {
        let mut wrong_loader = fixture();
        wrong_loader.loaders = vec!["forge".into()];
        wrong_loader.files[0].filename = "wrong-loader.jar".into();
        let mut beta = fixture();
        beta.version_type = "beta".into();
        beta.files[0].filename = "beta.jar".into();
        let mut stable = fixture();
        stable.files[0].filename = "stable.jar".into();
        let versions = [wrong_loader, beta, stable];
        let download = pick_compatible_version(&versions, Some("1.21.1"), Some("fabric")).unwrap();
        assert_eq!(download.filename, "stable.jar");
        assert_eq!(download.dependencies, required_dependencies_of(&versions[2]));
        assert!(pick_compatible_version(&versions, Some("1.20.1"), Some("fabric")).is_none());
        assert!(pick_compatible_version(&versions, Some("1.21.1"), Some("minecraft")).is_none());
        // A specific beta pin is not silently replaced with the stable release.
        assert!(ensure_version_matches(&versions[1], "1.21.1", ModLoader::Fabric, ContentType::Mod).is_ok());
        assert_eq!(resolve_version_detail(&versions[1], "1.21.1", ModLoader::Fabric, ContentType::Mod)
            .unwrap().filename, "beta.jar");
    }
}

#[cfg(test)]
mod version_files_lookup_tests {
    use super::{map_version_summary, ModrinthVersion};

    /// Shape of `POST /version_files` (hash -> full version object): the
    /// project id plus files array are what identification needs.
    const RESPONSE: &str = r#"{
        "a9993e364706816aba3e25717850c26c9cd0d4d": {
            "id": "Rvx75lGq",
            "project_id": "AANobbMI",
            "name": "Sodium 0.5.8",
            "version_number": "0.5.8",
            "date_published": "2024-01-01T00:00:00Z",
            "downloads": 42,
            "game_versions": ["1.20.1"],
            "loaders": ["fabric"],
            "files": [
                {"url": "https://cdn.modrinth.com/x.jar", "filename": "sodium-0.5.8.jar", "primary": true, "hashes": {}},
                {"url": "https://cdn.modrinth.com/x-sources.jar", "filename": "sodium-0.5.8-sources.jar", "primary": false, "hashes": {}}
            ]
        }
    }"#;

    #[test]
    fn parses_hash_to_version_map_with_project_id() {
        let map: std::collections::HashMap<String, ModrinthVersion> =
            serde_json::from_str(RESPONSE).unwrap();
        let version = map
            .get("a9993e364706816aba3e25717850c26c9cd0d4d")
            .expect("hash key present");
        assert_eq!(version.project_id, "AANobbMI");
        assert_eq!(version.id, "Rvx75lGq");
        assert_eq!(version.version_number, "0.5.8");
    }

    #[test]
    fn summary_prefers_the_primary_file_for_matching() {
        let map: std::collections::HashMap<String, ModrinthVersion> =
            serde_json::from_str(RESPONSE).unwrap();
        let version = map.values().next().unwrap();
        // The -sources.jar must not win: installs place the primary file.
        assert_eq!(
            map_version_summary(version).file_name.as_deref(),
            Some("sodium-0.5.8.jar")
        );
    }
}

#[cfg(test)]
mod channel_tests {
    use super::{is_stable_channel, pick_compatible_version, ModrinthVersion, ModrinthVersionFile};

    fn version(id: &str, version_type: &str) -> ModrinthVersion {
        ModrinthVersion {
            id: id.to_string(),
            name: String::new(),
            version_number: String::new(),
            date_published: "2026-01-01T00:00:00Z".to_string(),
            changelog: None,
            downloads: 0,
            game_versions: Vec::new(),
            loaders: Vec::new(),
            dependencies: Vec::new(),
            files: vec![ModrinthVersionFile {
                url: format!("https://example.com/{id}.jar"),
                filename: format!("{id}.jar"),
                primary: true,
                hashes: Default::default(),
            }],
            project_id: String::new(),
            version_type: version_type.to_string(),
        }
    }

    #[test]
    fn stable_wins_over_newer_prerelease_but_prerelease_still_resolves() {
        // Newest-first input with a beta on top: the release still wins.
        let versions = vec![version("beta", "beta"), version("release", "release")];
        assert_eq!(pick_compatible_version(&versions, None, None).unwrap().filename, "release.jar");
        // Beta-only project: resolves rather than erroring.
        let versions = vec![version("beta", "beta")];
        assert_eq!(pick_compatible_version(&versions, None, None).unwrap().filename, "beta.jar");
        assert!(pick_compatible_version(&[], None, None).is_none());
    }

    #[test]
    fn empty_channel_reads_as_stable() {
        assert!(is_stable_channel(""));
        assert!(is_stable_channel("release"));
        assert!(!is_stable_channel("beta"));
        assert!(!is_stable_channel("alpha"));
    }
}
