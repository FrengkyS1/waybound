use crate::dto::project_detail::{BodyFormat, GalleryItem, ModDetail, ModVersionSummary, suggest_instance_from_mod};
use crate::dto::{
    ContentType, ModLoader, ModSearchQuery, ModSearchResult, ModSource, ModSummary, SortIndex,
};
use crate::instances::ResolvedDownload;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use thiserror::Error;

const BASE_URL: &str = "https://api.curseforge.com/v1";
const MINECRAFT_GAME_ID: u32 = 432;
const USER_AGENT: &str = "Waybound/0.1.0 (Minecraft mod manager; personal use)";

/// CurseForge doesn't document a hard cap on `/mods` or `/mods/files` batch
/// body size, but a single request for a whole large modpack's worth of ids
/// (300+) was observed silently coming back short — some ids' names/icons
/// just never appear in `data`, with no error, no matter which ids they are.
/// Chunking keeps every request well under any plausible undocumented limit;
/// `mods_batch`/`files_batch` log if the merged result is still short.
const BATCH_CHUNK_SIZE: usize = 200;

#[derive(Debug, Error)]
pub enum CurseForgeError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("CurseForge API key is not configured")]
    NotConfigured,
    #[error("no compatible CurseForge file found")]
    NotFound,
    /// The newest available file targets a different game version than the
    /// instance (e.g. a 1.20.1-only project installed into a 1.21.1
    /// instance). Distinct from NotFound so callers can name the mismatch
    /// instead of a bare "no compatible file".
    #[error("no file for Minecraft {expected}")]
    WrongGameVersion {
        filename: String,
        file_versions: Vec<String>,
        expected: String,
    },
    #[error("{message}")]
    Rejected { status: u16, message: String },
    /// The file's author disabled third-party/API distribution — CurseForge
    /// will never hand this out automatically, no matter how many times it's
    /// retried. Carries what's needed to build a manual-download link
    /// pointing at this exact file (not just the mod's project page).
    #[error("{filename} requires a manual download (author disabled third-party downloads)")]
    DistributionRestricted { file_id: u32, filename: String, sha1: Option<String> },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeProbeResult {
    pub ok: bool,
    pub http_status: u16,
    pub key_length: usize,
    pub key_prefix: String,
    pub message: String,
    /// Full step-by-step diagnostic log (safe to display — never includes the full API key).
    pub log: Vec<String>,
}

pub struct CurseForgeClient {
    http: Client,
}

impl CurseForgeClient {
    pub fn new() -> Result<Self, CurseForgeError> {
        // No timeout here left CurseForge API hiccups (a connection accepted
        // but never answered) hang the "Loading project…" spinner
        // indefinitely instead of surfacing an error the existing
        // try/catch/finally in ProjectDetailPage.tsx could actually clear —
        // this is a metadata client only (small JSON responses), never a
        // multi-hundred-MB file transfer, so a hard ceiling is always safe.
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { http })
    }

    pub async fn search(
        &self,
        api_key: &str,
        query: &ModSearchQuery,
    ) -> Result<ModSearchResult, CurseForgeError> {
        self.search_inner(api_key, query, 0).await
    }

    async fn search_inner(
        &self,
        api_key: &str,
        query: &ModSearchQuery,
        attempt: u32,
    ) -> Result<ModSearchResult, CurseForgeError> {
        if api_key.trim().is_empty() {
            return Err(CurseForgeError::NotConfigured);
        }

        let response = self.send_search(api_key, query).await?;
        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN
        {
            let headers = response.headers().clone();
            let body = response.text().await.unwrap_or_default();

            if is_likely_rate_limit(status.as_u16(), &body, &headers) && attempt == 0 {
                eprintln!(
                    "CurseForge search returned HTTP {} — retrying once after rate-limit pause",
                    status.as_u16()
                );
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                return Box::pin(self.search_inner(api_key, query, attempt + 1)).await;
            }

            return Err(CurseForgeError::Rejected {
                status: status.as_u16(),
                message: rejection_message(status.as_u16(), &body, &headers),
            });
        }

        let response = response.error_for_status()?;
        let payload: CurseForgeApiResponse<Vec<CurseForgeMod>> = response.json().await?;

        Ok(ModSearchResult {
            hits: payload.data.into_iter().map(map_mod).collect(),
            offset: payload.pagination.as_ref().map(|p| p.index).unwrap_or(0),
            limit: payload.pagination.as_ref().map(|p| p.page_size).unwrap_or(0),
            total_hits: payload
                .pagination
                .as_ref()
                .map(|p| p.total_count)
                .unwrap_or(0),
            warnings: Vec::new(),
        })
    }

    pub async fn resolve_download_with_key(
        &self,
        mod_id: u32,
        mc_version: &str,
        loader: ModLoader,
        content_type: ContentType,
        api_key: &str,
    ) -> Result<ResolvedDownload, CurseForgeError> {
        if api_key.trim().is_empty() {
            return Err(CurseForgeError::NotConfigured);
        }

        // Dead in the current call graph — `install_mod` redirects any
        // `ContentType::Modpack` summary to `install_modpack` (its own,
        // separately-hardened resolution path in `modpack/curseforge.rs`)
        // before `resolve_download`/this function ever runs — but guarded
        // the same way as the exact-match attempt below anyway, in case a
        // future refactor ever does route a modpack-type resolution through
        // here directly.
        if content_type == ContentType::Modpack {
            match self.fetch_file(mod_id, mc_version, ModLoader::Vanilla, api_key).await {
                Ok(download) => return Ok(download),
                Err(err @ CurseForgeError::DistributionRestricted { .. }) => return Err(err),
                Err(_) => {}
            }
        }

        // The exact match (this MC version + this loader) is the only
        // attempt whose file is guaranteed to actually be right for the
        // user's instance — the fallbacks below try other loaders/versions
        // as a last resort. So if this file exists but is restricted, that
        // has to come back to the caller as-is instead of falling through:
        // silently swallowing it here would let a fallback quietly install a
        // wrong-loader or wrong-version substitute instead of telling the
        // user this exact file needs a manual download.
        match self.fetch_file(mod_id, mc_version, loader, api_key).await {
            Ok(download) => return Ok(download),
            Err(err @ CurseForgeError::DistributionRestricted { .. }) => return Err(err),
            Err(_) => {}
        }

        if loader != ModLoader::Vanilla {
            if let Ok(download) = self
                .fetch_file_any_loader(mod_id, mc_version, loader, mc_version, api_key)
                .await
            {
                return Ok(download);
            }
        }

        // Last resort: widen the API query past this game version (catches
        // files whose CurseForge metadata is mistagged), but the picked file
        // is still validated against the instance's REAL version below — a
        // wrong-version jar is never silently installed, it comes back as
        // WrongGameVersion naming the mismatch.
        self.fetch_file_any_loader(mod_id, "", loader, mc_version, api_key)
            .await
    }

    /// Newest file matching this MC version + loader exactly. The returned
    /// file's loader tags are validated against `loader`, so a Fabric-tagged
    /// file can never satisfy a NeoForge query even if CurseForge's category
    /// data would let it through the API filter.
    pub(crate) async fn fetch_file(
        &self,
        mod_id: u32,
        mc_version: &str,
        loader: ModLoader,
        api_key: &str,
    ) -> Result<ResolvedDownload, CurseForgeError> {
        self.fetch_file_inner(mod_id, mc_version, Some(loader), loader, Some(mc_version), api_key)
            .await
            .map(|(download, _)| download)
    }

    /// Fallback attempt: widen the query (any loader / any game version) but
    /// still validate whatever comes back against the instance's real loader
    /// AND game version — an untagged file is accepted, a file tagged for
    /// another loader or game version is refused, so the fallback can only
    /// ever return something usable.
    async fn fetch_file_any_loader(
        &self,
        mod_id: u32,
        query_mc_version: &str,
        validate_loader: ModLoader,
        validate_mc_version: &str,
        api_key: &str,
    ) -> Result<ResolvedDownload, CurseForgeError> {
        self.fetch_file_inner(
            mod_id,
            query_mc_version,
            None,
            validate_loader,
            Some(validate_mc_version),
            api_key,
        )
        .await
        .map(|(download, _)| download)
    }

    /// `query_loader = None` skips the modLoaderType filter entirely; files
    /// that declare no loader categories always pass validation.
    async fn fetch_file_inner(
        &self,
        mod_id: u32,
        mc_version: &str,
        query_loader: Option<ModLoader>,
        validate_loader: ModLoader,
        validate_mc_version: Option<&str>,
        api_key: &str,
    ) -> Result<(ResolvedDownload, Vec<u32>), CurseForgeError> {
        let mut request = self
            .http
            .get(format!("{BASE_URL}/mods/{mod_id}/files"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .query(&[
                ("pageSize", "25"),
                ("index", "0"),
                ("sortField", "2"),
                ("sortOrder", "desc"),
            ]);

        if !mc_version.is_empty() {
            request = request.query(&[("gameVersion", mc_version)]);
        }
        if let Some(loader) =
            query_loader.filter(|l| *l != ModLoader::Vanilla)
        {
            request = request.query(&[(
                "modLoaderType",
                loader.as_curseforge_loader_type().to_string(),
            )]);
        }

        let response = request.send().await?.error_for_status()?;
        let payload: CurseForgeApiResponse<Vec<CurseForgeModFile>> = response.json().await?;
        // Newest-first from the API; prefer an available stable file, then
        // any available file, then whatever came back (whose failure then
        // surfaces the real reason — restricted, loader mismatch — instead
        // of a bare NotFound).
        let files = payload.data;
        let file = files
            .iter()
            .filter(|f| f.is_available)
            .find(|f| is_stable_release(f.release_type))
            .or_else(|| files.iter().filter(|f| f.is_available).next())
            .or_else(|| files.first())
            .cloned()
            .ok_or(CurseForgeError::NotFound)?;

        ensure_file_loader_matches(&file.loaders, validate_loader)?;
        // The query above may have been deliberately widened past the game
        // version (any-version fallback), so the picked file's own tags get
        // checked against the instance's real version here. Files with no
        // version tags pass (metadata is occasionally incomplete); a file
        // tagged for another version is refused with the mismatch named.
        if let Some(expected) = validate_mc_version {
            if !expected.is_empty()
                && !file.game_versions.is_empty()
                && !file.game_versions.iter().any(|v| v == expected)
            {
                return Err(CurseForgeError::WrongGameVersion {
                    filename: file.file_name.clone(),
                    file_versions: file.game_versions.clone(),
                    expected: expected.to_string(),
                });
            }
        }
        let required_dependencies = required_dependency_mod_ids_of_file(&file);

        let download_url = match file.download_url.filter(|u| !u.is_empty()) {
            Some(url) => url,
            None => match self.file_download_url(mod_id, file.id, api_key).await {
                Ok(url) => url,
                // A 403 here (after the retry logic above has already ruled
                // out a transient rate limit) means the author disabled
                // third-party distribution for this file — not a fluke that
                // a retry would fix.
                Err(CurseForgeError::Rejected { status: 403, .. }) => {
                    return Err(CurseForgeError::DistributionRestricted {
                        file_id: file.id,
                        filename: file.file_name,
                        sha1: sha1_of(&file.hashes),
                    });
                }
                Err(e) => return Err(e),
            },
        };

        Ok((
            ResolvedDownload {
                url: download_url,
                filename: file.file_name,
                hashes: download_hashes(&file.hashes),
            },
            required_dependencies,
        ))
    }

    /// The dependency mod ids declared by the newest file matching this MC
    /// version + loader — the same selection rule as `fetch_file`'s exact
    /// attempt, so the dependencies belong to the file that gets installed.
    /// Best-effort like the Modrinth counterpart: any failure yields empty.
    pub(crate) async fn required_dependency_mod_ids(
        &self,
        mod_id: u32,
        mc_version: &str,
        loader: ModLoader,
        api_key: &str,
    ) -> Vec<u32> {
        if api_key.trim().is_empty() {
            return Vec::new();
        }
        let response = self
            .http
            .get(format!("{BASE_URL}/mods/{mod_id}/files"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .query(&[
                ("pageSize", "1"),
                ("index", "0"),
                ("sortField", "2"),
                ("sortOrder", "desc"),
            ])
            .query(&[("gameVersion", mc_version)])
            .query(&[(
                "modLoaderType",
                loader.as_curseforge_loader_type().to_string(),
            )])
            .send()
            .await;
        let Ok(response) = response else {
            return Vec::new();
        };
        let Ok(payload) = response.json::<CurseForgeApiResponse<Vec<CurseForgeModFile>>>().await
        else {
            return Vec::new();
        };
        payload
            .data
            .first()
            .map(required_dependency_mod_ids_of_file)
            .unwrap_or_default()
    }

    /// Dependency mod ids of one specific file (a version the user picked by
    /// hand). Best-effort.
    pub(crate) async fn file_dependency_mod_ids(
        &self,
        mod_id: u32,
        file_id: u32,
        api_key: &str,
    ) -> Vec<u32> {
        self.file_meta_inner(mod_id, file_id, api_key, 0)
            .await
            .map(|file| required_dependency_mod_ids_of_file(&file))
            .unwrap_or_default()
    }

    pub async fn file_download_url(
        &self,
        mod_id: u32,
        file_id: u32,
        api_key: &str,
    ) -> Result<String, CurseForgeError> {
        self.file_download_url_inner(mod_id, file_id, api_key, 0).await
    }

    // A modpack install fires this for whatever files the batch lookup
    // couldn't resolve — usually a handful, sometimes down to zero once
    // already-downloaded files are skipped. Still rate-limit-prone right
    // after a big batch/download burst, so this gets real patience (a few
    // retries with growing backoff) rather than the one quick retry that
    // proved insufficient in practice — CurseForge kept rejecting these with
    // the same "rate limit" response even after a single 2s pause.
    async fn file_download_url_inner(
        &self,
        mod_id: u32,
        file_id: u32,
        api_key: &str,
        attempt: u32,
    ) -> Result<String, CurseForgeError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/mods/{mod_id}/files/{file_id}/download-url"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN
        {
            let headers = response.headers().clone();
            let body = response.text().await.unwrap_or_default();

            if is_likely_rate_limit(status.as_u16(), &body, &headers) && attempt < RATE_LIMIT_MAX_RETRIES {
                tokio::time::sleep(rate_limit_backoff(attempt)).await;
                return Box::pin(self.file_download_url_inner(mod_id, file_id, api_key, attempt + 1))
                    .await;
            }

            return Err(CurseForgeError::Rejected {
                status: status.as_u16(),
                message: rejection_message(status.as_u16(), &body, &headers),
            });
        }

        let response = response.error_for_status()?;
        let payload: CurseForgeApiResponse<String> = response.json().await?;
        Ok(payload.data)
    }

    /// Filename + Sha1 together, one request instead of two — for callers
    /// that need both (identifying a manually-downloaded replacement by
    /// content instead of filename).
    pub async fn file_meta(
        &self,
        mod_id: u32,
        file_id: u32,
        api_key: &str,
    ) -> Result<(String, Option<String>), CurseForgeError> {
        let file = self.file_meta_inner(mod_id, file_id, api_key, 0).await?;
        Ok((file.file_name, sha1_of(&file.hashes)))
    }

    async fn file_meta_inner(
        &self,
        mod_id: u32,
        file_id: u32,
        api_key: &str,
        attempt: u32,
    ) -> Result<CurseForgeModFile, CurseForgeError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/mods/{mod_id}/files/{file_id}"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN
        {
            let headers = response.headers().clone();
            let body = response.text().await.unwrap_or_default();

            if is_likely_rate_limit(status.as_u16(), &body, &headers) && attempt < RATE_LIMIT_MAX_RETRIES {
                tokio::time::sleep(rate_limit_backoff(attempt)).await;
                return Box::pin(self.file_meta_inner(mod_id, file_id, api_key, attempt + 1)).await;
            }

            return Err(CurseForgeError::Rejected {
                status: status.as_u16(),
                message: rejection_message(status.as_u16(), &body, &headers),
            });
        }

        let response = response.error_for_status()?;
        let payload: CurseForgeApiResponse<CurseForgeModFile> = response.json().await?;
        Ok(payload.data)
    }

    /// Looks up many files' names in one request instead of one round trip
    /// per file — CurseForge's batch `/mods/files` endpoint. A modpack
    /// manifest can list 300+ files; fetching those one at a time was the
    /// actual cost behind a slow modpack-content preview.
    pub async fn file_names_batch(
        &self,
        file_ids: &[u32],
        api_key: &str,
    ) -> std::collections::HashMap<u32, String> {
        self.files_batch(file_ids, api_key)
            .await
            .into_iter()
            .map(|(id, (name, _, _))| (id, name))
            .collect()
    }

    /// Same batch `/mods/files` endpoint as `file_names_batch`, but also
    /// returns each file's `downloadUrl` — CurseForge includes it on this
    /// same object, so a modpack install can skip the separate per-file
    /// `/download-url` call for every one of its (often 100-400) files. That
    /// per-file call is what was tripping CurseForge's rate limit on install
    /// (hundreds of individual requests in one burst); one batch call avoids
    /// generating the burst in the first place instead of just retrying it.
    /// Maps file id -> (filename, downloadUrl, sha1).
    pub async fn files_batch(
        &self,
        file_ids: &[u32],
        api_key: &str,
    ) -> std::collections::HashMap<u32, (String, Option<String>, Option<String>)> {
        if file_ids.is_empty() {
            return std::collections::HashMap::new();
        }
        let mut merged = std::collections::HashMap::new();
        for chunk in file_ids.chunks(BATCH_CHUNK_SIZE) {
            merged.extend(self.files_batch_chunk(chunk, api_key).await);
        }
        if merged.len() < file_ids.len() {
            crate::activity::append_log(
                &format!(
                    "CurseForge files_batch: requested {} file ids, got metadata for {} — some downloads may fall back to individual lookups",
                    file_ids.len(),
                    merged.len()
                ),
                "warn",
                None,
            );
        }
        merged
    }

    async fn files_batch_chunk(
        &self,
        file_ids: &[u32],
        api_key: &str,
    ) -> std::collections::HashMap<u32, (String, Option<String>, Option<String>)> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            file_ids: &'a [u32],
        }
        let Ok(response) = self
            .http
            .post(format!("{BASE_URL}/mods/files"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .json(&Body { file_ids })
            .send()
            .await
        else {
            return std::collections::HashMap::new();
        };
        let Ok(payload) = response
            .json::<CurseForgeApiResponse<Vec<CurseForgeModFile>>>()
            .await
        else {
            return std::collections::HashMap::new();
        };
        payload
            .data
            .into_iter()
            .map(|f| {
                let sha1 = sha1_of(&f.hashes);
                (f.id, (f.file_name, f.download_url.filter(|u| !u.is_empty()), sha1))
            })
            .collect()
    }

    /// Matches local files to CurseForge projects by content fingerprint
    /// (`POST /fingerprints`) — PrismLauncher's update/match flow on the CF
    /// side. Strict (errors propagate): used for a deliberate user action.
    /// Returns one entry per exactly-matched file.
    pub async fn match_fingerprints(
        &self,
        fingerprints: &[u32],
        api_key: &str,
    ) -> Result<Vec<FingerprintExactMatch>, CurseForgeError> {
        #[derive(Serialize)]
        struct Body<'a> {
            fingerprints: &'a [u32],
        }
        #[derive(Deserialize)]
        struct Response {
            data: FingerprintData,
        }
        #[derive(Deserialize)]
        struct FingerprintData {
            #[serde(default, rename = "exactMatches")]
            exact_matches: Vec<FingerprintExactMatch>,
        }
        if fingerprints.is_empty() {
            return Ok(Vec::new());
        }
        let response = self
            .http
            .post(format!("{BASE_URL}/fingerprints"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .json(&Body { fingerprints })
            .send()
            .await?
            .error_for_status()
            .map_err(|e| {
                CurseForgeError::Rejected {
                    status: e.status().map(|s| s.as_u16()).unwrap_or(0),
                    message: e.to_string(),
                }
            })?;
        let payload = response.json::<Response>().await?;
        Ok(payload.data.exact_matches)
    }

    /// Batch mod name + slug + icon lookup (`/mods`, the mod-level
    /// counterpart to `files_batch`) — used both to build a manual-download
    /// link for files a modpack install couldn't resolve automatically, and
    /// to give every modpack-installed mod an icon. Modpack installs write
    /// files straight to disk without ever touching a `ModSummary` (unlike a
    /// single mod installed via Browse), so without this every one of them
    /// had no icon on record at all — the Content tab could only show one
    /// when the jar happened to embed its own, which most don't.
    /// Maps project id -> (name, slug, icon, website URL). The website URL
    /// comes straight from CurseForge's own `links.websiteUrl` — not
    /// reconstructed from the slug — because the URL path segment differs
    /// by content type (`mc-mods`, `texture-packs`, `shaders`, ...) and a
    /// hardcoded one 404s for anything that isn't a plain mod.
    pub async fn mods_batch(
        &self,
        mod_ids: &[u32],
        api_key: &str,
    ) -> std::collections::HashMap<u32, (String, String, Option<String>, Option<String>)> {
        if mod_ids.is_empty() {
            return std::collections::HashMap::new();
        }
        let mut merged = std::collections::HashMap::new();
        for chunk in mod_ids.chunks(BATCH_CHUNK_SIZE) {
            merged.extend(self.mods_batch_chunk(chunk, api_key).await);
        }
        if merged.len() < mod_ids.len() {
            crate::activity::append_log(
                &format!(
                    "CurseForge mods_batch: requested {} project ids, got metadata for {} — some names/icons may fall back to filenames",
                    mod_ids.len(),
                    merged.len()
                ),
                "warn",
                None,
            );
        }
        merged
    }

    async fn mods_batch_chunk(
        &self,
        mod_ids: &[u32],
        api_key: &str,
    ) -> std::collections::HashMap<u32, (String, String, Option<String>, Option<String>)> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            mod_ids: &'a [u32],
        }
        let Ok(response) = self
            .http
            .post(format!("{BASE_URL}/mods"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .json(&Body { mod_ids })
            .send()
            .await
        else {
            return std::collections::HashMap::new();
        };
        let Ok(payload) = response.json::<CurseForgeApiResponse<Vec<CurseForgeMod>>>().await else {
            return std::collections::HashMap::new();
        };
        payload
            .data
            .into_iter()
            .map(|m| {
                let icon = logo_icon_url(m.logo);
                let website_url = m.links.and_then(|l| l.website_url);
                (m.id, (m.name, m.slug, icon, website_url))
            })
            .collect()
    }

    pub async fn resolve_file_by_id(
        &self,
        mod_id: u32,
        file_id: u32,
        api_key: &str,
        mc_version: &str,
        loader: ModLoader,
    ) -> Result<(ResolvedDownload, Vec<u32>), CurseForgeError> {
        if api_key.trim().is_empty() {
            return Err(CurseForgeError::NotConfigured);
        }
        // This is a version the user explicitly picked from the file list —
        // no ambiguity about which file is "correct" — so a 403 here is
        // reported as a restriction on this exact file rather than a bare
        // rejection, same as the version/loader-matching path above.
        let file = self.file_meta_inner(mod_id, file_id, api_key, 0).await?;
        // The picker shows every version; this guard is what stops an easy
        // mis-click ("Add" on a row tagged Fabric) from installing it into a
        // NeoForge instance.
        ensure_file_loader_matches(&file.loaders, loader)?;
        if !mc_version.is_empty()
            && !file.game_versions.is_empty()
            && !file.game_versions.iter().any(|v| v == mc_version)
        {
            return Err(CurseForgeError::WrongGameVersion {
                filename: file.file_name.clone(),
                file_versions: file.game_versions.clone(),
                expected: mc_version.to_string(),
            });
        }
        let required_dependencies = required_dependency_mod_ids_of_file(&file);

        let url = match self.file_download_url(mod_id, file_id, api_key).await {
            Ok(url) => url,
            Err(CurseForgeError::Rejected { status: 403, .. }) => {
                let sha1 = sha1_of(&file.hashes);
                return Err(CurseForgeError::DistributionRestricted {
                    file_id,
                    filename: file.file_name,
                    sha1,
                });
            }
            Err(e) => return Err(e),
        };
        Ok((
            ResolvedDownload {
                url,
                filename: file.file_name,
                hashes: download_hashes(&file.hashes),
            },
            required_dependencies,
        ))
    }

    pub async fn fetch_mod_detail(
        &self,
        summary: &ModSummary,
        api_key: &str,
    ) -> Result<ModDetail, CurseForgeError> {
        if api_key.trim().is_empty() {
            return Err(CurseForgeError::NotConfigured);
        }
        let mod_id = summary.curseforge_id.ok_or(CurseForgeError::NotFound)?;

        let response = self
            .http
            .get(format!("{BASE_URL}/mods/{mod_id}"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .send()
            .await?;

        // A 403 straight from GetMod (not a file download) means the author
        // disabled third-party API access for the whole project, not just
        // downloads — CurseForge will never serve this project's details, no
        // matter how many times it's retried. Without this check, `?` on
        // `error_for_status()` below turns it into an opaque
        // `CurseForgeError::Network(reqwest::Error)` whose message is just
        // the raw "HTTP status client error (403 Forbidden) for url (...)" —
        // exactly what a user sees with no explanation of why.
        //
        // Deliberately not reusing `is_likely_rate_limit` here: its
        // "empty body -> probably rate limited" default is tuned for the
        // search/batch flows, where a burst of calls really is the common
        // cause. A single GetMod call for one project returning an *empty*
        // 403 is CurseForge's actual response shape for a distribution-
        // disabled project (confirmed against real project IDs) — treating
        // that as "temporary, wait a minute" is actively wrong, since it
        // never clears no matter how long you wait. Only escalate to the
        // rate-limit message on concrete evidence of one.
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.text().await.unwrap_or_default();
            let message = if looks_like_rate_limit_block(&body, &headers) {
                rejection_message(status.as_u16(), &body, &headers)
            } else {
                "This mod's page isn't available here — its author has disabled third-party access on CurseForge.".to_string()
            };
            return Err(CurseForgeError::Rejected { status: 403, message });
        }

        let response = response.error_for_status()?;
        let payload: CurseForgeApiResponse<CurseForgeModDetail> = response.json().await?;
        let item = payload.data;

        // The mod-info endpoint above only ever carries `summary` (a one-line
        // tagline) — CurseForge's actual long-form description lives behind
        // this separate endpoint entirely. Best-effort: falling back to the
        // tagline here just means a shorter Overview, not a failed page load.
        let full_description = async {
            let response = self
                .http
                .get(format!("{BASE_URL}/mods/{mod_id}/description"))
                .header("x-api-key", api_key)
                .header("Accept", "application/json")
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?;
            let payload: CurseForgeApiResponse<String> = response.json().await.ok()?;
            Some(payload.data).filter(|d| !d.is_empty())
        }
        .await;

        let files_response = self
            .http
            .get(format!("{BASE_URL}/mods/{mod_id}/files"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .query(&[
                ("pageSize", "25"),
                ("index", "0"),
                ("sortField", "2"),
                ("sortOrder", "desc"),
            ])
            .send()
            .await?
            .error_for_status()?;
        let files_payload: CurseForgeApiResponse<Vec<CurseForgeFileDetail>> =
            files_response.json().await?;

        // Unavailable files are dead entries: never suggest from them and
        // never list them as installable versions.
        let files: Vec<CurseForgeFileDetail> = files_payload
            .data
            .into_iter()
            .filter(|f| f.is_available)
            .collect();

        let mut updated_summary = summary.clone();
        updated_summary.name = item.name.clone();
        updated_summary.description = strip_html(&item.summary);
        updated_summary.downloads = item.download_count as u64;
        updated_summary.updated_at = item.date_modified.clone();

        let loaders = ModLoader::from_curseforge_categories(&item.categories);
        let mut game_versions: Vec<String> = files
            .iter()
            .flat_map(|f| f.game_versions.clone())
            .filter(|v| is_real_game_version(v))
            .collect();
        game_versions.sort_by(|a, b| b.cmp(a));
        game_versions.dedup();

        let version_summaries: Vec<ModVersionSummary> = files
            .iter()
            .map(map_cf_version_summary)
            .collect();

        let (mc, loader) = pick_cf_suggested(&files, &loaders);
        let external_url = item
            .links
            .as_ref()
            .and_then(|l| l.website_url.clone())
            .or_else(|| Some(format!("https://www.curseforge.com/minecraft/mc-mods/{}/", item.slug)));
        let comments_url = Some(format!(
            "https://www.curseforge.com/minecraft/mc-mods/{}/comments",
            item.slug
        ));
        let gallery = item
            .screenshots
            .iter()
            .map(|shot| GalleryItem {
                url: shot.url.clone().unwrap_or_default(),
                title: shot.title.clone(),
                description: shot.description.clone(),
                thumbnail_url: shot.thumbnail_url.clone(),
            })
            .filter(|item| !item.url.is_empty())
            .collect();

        Ok(ModDetail {
            summary: updated_summary.clone(),
            body: full_description.unwrap_or(item.description),
            body_format: BodyFormat::Html,
            categories: item
                .categories
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            game_versions,
            loaders,
            external_url,
            comments_url,
            gallery,
            versions: version_summaries,
            suggested_instance: suggest_instance_from_mod(&updated_summary, &mc, loader),
        })
    }

    pub async fn fetch_file_changelog(
        &self,
        mod_id: u32,
        file_id: u32,
        api_key: &str,
    ) -> Result<Option<String>, CurseForgeError> {
        let response = self
            .http
            .get(format!("{BASE_URL}/mods/{mod_id}/files/{file_id}"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?;
        let payload: CurseForgeApiResponse<CurseForgeFileWithNotes> = response.json().await?;
        Ok(payload.data.release_notes)
    }

    pub async fn probe_api_key(&self, api_key: &str, key_source: Option<&str>) -> CurseForgeProbeResult {
        let mut log = Vec::new();
        let key_length = api_key.len();
        let key_prefix = redact_key(api_key);

        log.push(format!("Waybound CurseForge probe started at {}", now_iso()));
        if let Some(source) = key_source {
            log.push(format!("Key source: {source}"));
        }
        log.push(format!("Saved key length: {key_length} chars"));
        log.push(format!("Saved key prefix (redacted): {key_prefix}"));

        if api_key.trim().is_empty() {
            log.push("FAIL: API key is empty after load from config.".to_string());
            return fail_probe(0, key_length, key_prefix, log, "CurseForge API key is empty.");
        }

        if !api_key.starts_with("$2a$") {
            log.push(format!(
                "WARN: Key does not start with \"$2a$\" — got prefix \"{}\"",
                &api_key.chars().take(7).collect::<String>()
            ));
        }

        let query = ModSearchQuery {
            query: "sodium".to_string(),
            content_type: Some(ContentType::Mod),
            loader: None,
            sort: SortIndex::Downloads,
            offset: 0,
            limit: 1,
        };

        let params = build_search_params(&query);
        let request_url = build_probe_url(&params);
        log.push("Probe uses GET /v1/mods/search (same endpoint as Browse).".to_string());
        log.push(format!("Request URL (no API key in URL): {request_url}"));
        log.push(format!("User-Agent: {USER_AGENT}"));
        log.push(format!(
            "Request header: x-api-key: [REDACTED — length {key_length} chars]"
        ));
        log.push("Request header: Accept: application/json".to_string());
        log.push(format!("Query params: {}", format_params(&params)));

        let started = Instant::now();
        log.push("Sending request…".to_string());

        let response_result = self.send_search(api_key, &query).await;
        let elapsed_ms = started.elapsed().as_millis();

        match response_result {
            Ok(response) => {
                let status = response.status();
                log.push(format!("Response received in {elapsed_ms} ms"));
                log.push(format!("HTTP status: {} {}", status.as_u16(), status.canonical_reason().unwrap_or("")));

                for (name, value) in response.headers().iter() {
                    if let Ok(v) = value.to_str() {
                        log.push(format!("Response header: {name}: {v}"));
                    }
                }

                if status.is_success() {
                    match response.text().await {
                        Ok(body) => {
                            log.push(format!("Response body length: {} bytes", body.len()));
                            log.push(format!(
                                "Response body preview: {}",
                                truncate_body(&body, 500)
                            ));
                            log.push("SUCCESS: CurseForge accepted the API key.".to_string());
                            emit_probe_log(&log);
                            return CurseForgeProbeResult {
                                ok: true,
                                http_status: status.as_u16(),
                                key_length,
                                key_prefix,
                                message: "CurseForge accepted the API key.".to_string(),
                                log,
                            };
                        }
                        Err(err) => {
                            log.push(format!("FAIL: Could not read response body: {err}"));
                            return fail_probe(
                                status.as_u16(),
                                key_length,
                                key_prefix,
                                log,
                                "Could not read CurseForge response body.",
                            );
                        }
                    }
                } else {
                    let status_code = status.as_u16();
                    let body = response.text().await.unwrap_or_default();
                    log.push(format!("Response body length: {} bytes", body.len()));
                    if body.is_empty() {
                        log.push("Response body: (empty)".to_string());
                    } else {
                        log.push(format!("Response body preview: {}", truncate_body(&body, 800)));
                    }
                    if body.contains("<!DOCTYPE") || body.contains("CloudFront") {
                        log.push(
                            "Diagnosis: CloudFront/WAF HTML response — often rate limit or edge block, not a malformed key.".to_string(),
                        );
                    } else if status_code == 403 {
                        log.push(
                            "Diagnosis: HTTP 403 on /mods/search usually means CurseForge rejected the key OR rate-limited you.".to_string(),
                        );
                        log.push(
                            "Diagnosis: Key format looks fine if length ~60 and prefix $2a$10$ — try regenerating at console.curseforge.com or a new developer account.".to_string(),
                        );
                    } else if status_code == 401 {
                        log.push("Diagnosis: HTTP 401 — key missing or invalid for this endpoint.".to_string());
                    }
                    log.push(format!("FAIL: CurseForge returned HTTP {status_code}"));
                    return fail_probe(status_code, key_length, key_prefix, log, &format!(
                        "CurseForge returned HTTP {status_code}. See probe log below."
                    ));
                }
            }
            Err(CurseForgeError::Network(err)) => {
                log.push(format!("Response failed after {elapsed_ms} ms"));
                log.push(format!("Network error type: {err}"));
                if err.is_timeout() {
                    log.push("Diagnosis: Request timed out.".to_string());
                } else if err.is_connect() {
                    log.push("Diagnosis: Could not connect to api.curseforge.com.".to_string());
                } else if let Some(status) = err.status() {
                    log.push(format!("HTTP status from error: {}", status.as_u16()));
                }
                if err.is_request() {
                    log.push("Diagnosis: Invalid request (check header encoding).".to_string());
                }
            }
            Err(other) => {
                log.push(format!("Unexpected error: {other}"));
            }
        }

        let message = log
            .iter()
            .rev()
            .find(|line| line.starts_with("FAIL:") || line.starts_with("Diagnosis:"))
            .cloned()
            .unwrap_or_else(|| "CurseForge probe failed. See log below.".to_string());

        fail_probe(0, key_length, key_prefix, log, &message.replace("FAIL: ", ""))
    }

    async fn send_search(
        &self,
        api_key: &str,
        query: &ModSearchQuery,
    ) -> Result<reqwest::Response, CurseForgeError> {
        let mut request = self
            .http
            .get(format!("{BASE_URL}/mods/search"))
            .header("x-api-key", api_key)
            .header("Accept", "application/json");

        for (key, value) in build_search_params(query) {
            request = request.query(&[(key.as_str(), value.as_str())]);
        }

        Ok(request.send().await?)
    }
}

fn fail_probe(
    status: u16,
    key_length: usize,
    key_prefix: String,
    log: Vec<String>,
    message: &str,
) -> CurseForgeProbeResult {
    emit_probe_log(&log);
    CurseForgeProbeResult {
        ok: false,
        http_status: status,
        key_length,
        key_prefix,
        message: message.to_string(),
        log,
    }
}

fn emit_probe_log(log: &[String]) {
    eprintln!("=== Waybound CurseForge probe ===");
    for line in log {
        eprintln!("{line}");
    }
    eprintln!("=== end probe ===");
}

fn redact_key(key: &str) -> String {
    if key.len() <= 12 {
        return "[too short]".to_string();
    }
    format!("{}…{} ({} chars)", &key[..7], &key[key.len() - 4..], key.len())
}

fn build_probe_url(params: &[(String, String)]) -> String {
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{BASE_URL}/mods/search?{query}")
}

fn urlencoding_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn format_params(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn truncate_body(body: &str, max: usize) -> String {
    let collapsed = body.replace('\n', " ").replace('\r', " ");
    if collapsed.len() <= max {
        collapsed
    } else {
        format!("{}…", &collapsed[..max])
    }
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

fn build_search_params(query: &ModSearchQuery) -> Vec<(String, String)> {
    let class_id = query
        .content_type
        .map(content_type_to_class_id)
        .unwrap_or(6);

    let mut params = vec![
        ("gameId".to_string(), MINECRAFT_GAME_ID.to_string()),
        ("classId".to_string(), class_id.to_string()),
        ("sortField".to_string(), sort_to_field(query.sort).to_string()),
        ("sortOrder".to_string(), "desc".to_string()),
        ("index".to_string(), query.offset.to_string()),
        // CurseForge caps pageSize at 50; Modrinth handles the rest of a 100 page.
        ("pageSize".to_string(), query.limit.min(50).to_string()),
    ];

    // Apply the mod-loader filter (Forge=1, Fabric=4, Quilt=5, NeoForge=6).
    if let Some(loader) = query.loader {
        let loader_type = loader.as_curseforge_loader_type();
        if loader_type != 0 {
            params.push(("modLoaderType".to_string(), loader_type.to_string()));
        }
    }

    let trimmed = query.query.trim();
    if !trimmed.is_empty() {
        params.push(("searchFilter".to_string(), trimmed.to_string()));
    }

    params
}

fn rejection_message(status: u16, body: &str, headers: &reqwest::header::HeaderMap) -> String {
    if is_likely_rate_limit(status, body, headers) {
        return format!(
            "CurseForge rate limit or temporary block (HTTP {status}). Your key may be valid — wait a minute and search again."
        );
    }

    if body.contains("<!DOCTYPE") || body.contains("CloudFront") {
        return format!(
            "CurseForge blocked the request (HTTP {status}). This is often rate limiting, not a bad key."
        );
    }

    if status == 403 {
        return "CurseForge returned HTTP 403. If Test saved key succeeds, wait a minute — CurseForge rate limits are very aggressive.".to_string();
    }

    format!("CurseForge rejected the request (HTTP {status}).")
}

// A single 2s retry proved insufficient in practice: a modpack install's
// last stretch of per-file lookups (the ones the batch call couldn't
// resolve) kept getting rejected as rate-limited even after one pause,
// because they land right after the batch/download burst that likely
// caused the limit in the first place. A few retries with growing backoff
// gives CurseForge's window more realistic time to clear — these calls are
// now rare enough (down to zero once already-downloaded files are skipped)
// that the extra wall-clock cost per call is worth it.
const RATE_LIMIT_MAX_RETRIES: u32 = 3;

fn rate_limit_backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(2u64.saturating_pow(attempt + 1))
}

/// Stricter than `is_likely_rate_limit`: only true on concrete rate-limit
/// evidence (a WAF/CDN block page, explicit wording, or a `Retry-After`
/// header), never just because the body happened to be empty — an empty
/// body is what a single-project 403 looks like either way, and defaulting
/// to "rate limited" there misdiagnoses a permanent per-project restriction
/// as a transient one.
fn looks_like_rate_limit_block(body: &str, headers: &reqwest::header::HeaderMap) -> bool {
    if headers.contains_key("retry-after") {
        return true;
    }
    if headers
        .get("x-cache")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("Error"))
    {
        return true;
    }
    if body.contains("<!DOCTYPE") || body.contains("CloudFront") {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("rate") || lower.contains("too many")
}

fn is_likely_rate_limit(status: u16, body: &str, headers: &reqwest::header::HeaderMap) -> bool {
    if status != 403 {
        return false;
    }

    if body.trim().is_empty() {
        return true;
    }

    if headers
        .get("x-cache")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("Error"))
    {
        return true;
    }

    let lower = body.to_ascii_lowercase();
    lower.contains("rate") || lower.contains("too many")
}

#[cfg(test)]
mod rate_limit_classification_tests {
    use super::looks_like_rate_limit_block;
    use reqwest::header::HeaderMap;

    #[test]
    fn empty_body_is_not_treated_as_rate_limit() {
        // The actual regression: a distribution-restricted project's GetMod
        // 403 has an empty body too — this must default to "not a rate
        // limit" (the caller then shows the permanent-restriction message),
        // unlike the looser `is_likely_rate_limit` used elsewhere.
        assert!(!looks_like_rate_limit_block("", &HeaderMap::new()));
    }

    #[test]
    fn retry_after_header_is_rate_limit() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "60".parse().unwrap());
        assert!(looks_like_rate_limit_block("", &headers));
    }

    #[test]
    fn waf_block_page_is_rate_limit() {
        assert!(looks_like_rate_limit_block("<!DOCTYPE html>blocked", &HeaderMap::new()));
    }

    #[test]
    fn explicit_rate_wording_is_rate_limit() {
        assert!(looks_like_rate_limit_block("Too many requests", &HeaderMap::new()));
    }
}

fn sort_to_field(sort: SortIndex) -> u32 {
    match sort {
        SortIndex::Downloads => 6,
        SortIndex::Updated => 3,
        SortIndex::Relevance => 2,
        SortIndex::New => 11,
    }
}

fn content_type_to_class_id(content_type: ContentType) -> u32 {
    match content_type {
        ContentType::Mod => 6,
        ContentType::Modpack => 4471,
        ContentType::Resourcepack => 12,
        ContentType::Shader => 6552,
    }
}

fn map_mod(item: CurseForgeMod) -> ModSummary {
    ModSummary {
        uid: format!("curseforge:{}", item.id),
        slug: item.slug,
        name: item.name,
        description: strip_html(&item.summary),
        author: item
            .authors
            .first()
            .map(|author| author.name.clone())
            .unwrap_or_else(|| "Unknown".to_string()),
        icon_url: logo_icon_url(item.logo),
        downloads: item.download_count as u64,
        project_type: content_type_from_class_id(item.class_id),
        loaders: ModLoader::from_curseforge_categories(&item.categories),
        sources: vec![ModSource::Curseforge],
        updated_at: item.date_modified,
        curseforge_id: Some(item.id),
        modrinth_id: None,
    }
}

fn content_type_from_class_id(class_id: Option<u32>) -> ContentType {
    match class_id {
        Some(4471) => ContentType::Modpack,
        Some(12) => ContentType::Resourcepack,
        Some(6552) => ContentType::Shader,
        _ => ContentType::Mod,
    }
}

fn strip_html(input: &str) -> String {
    input
        .replace("<br>", " ")
        .replace("<br/>", " ")
        .replace("<br />", " ")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

#[derive(Debug, Deserialize)]
struct CurseForgeApiResponse<T> {
    data: T,
    #[serde(default)]
    pagination: Option<CurseForgePagination>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgePagination {
    index: u32,
    page_size: u32,
    total_count: u32,
}

/// One `exactMatches[]` entry from `POST /fingerprints`: the file id plus
/// the file object carrying its project id and filename. Unknown fields
/// are ignored — the response carries far more than matching needs.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintExactMatch {
    pub id: u32,
    pub file: FingerprintMatchedFile,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintMatchedFile {
    #[serde(default)]
    pub mod_id: u32,
    #[serde(default)]
    pub file_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeModDetail {
    slug: String,
    name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    download_count: f64,
    #[serde(default)]
    date_modified: String,
    #[serde(default)]
    categories: Vec<CurseForgeCategory>,
    #[serde(default)]
    links: Option<CurseForgeLinks>,
    #[serde(default)]
    screenshots: Vec<CurseForgeScreenshot>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeScreenshot {
    url: Option<String>,
    thumbnail_url: Option<String>,
    title: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeFileWithNotes {
    release_notes: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeLinks {
    website_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeFileDetail {
    id: u32,
    display_name: String,
    file_name: String,
    file_date: String,
    #[serde(default)]
    download_count: f64,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    mod_loaders: Vec<CurseForgeFileLoader>,
    #[serde(default = "default_true")]
    is_available: bool,
    #[serde(default)]
    release_type: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeFileLoader {
    name: String,
}

fn map_cf_version_summary(file: &CurseForgeFileDetail) -> ModVersionSummary {
    ModVersionSummary {
        id: file.id.to_string(),
        name: file.display_name.clone(),
        version_number: file.file_name.clone(),
        published_at: file.file_date.clone(),
        game_versions: file.game_versions.clone(),
        loaders: file
            .mod_loaders
            .iter()
            .filter_map(|l| match l.name.to_ascii_lowercase().as_str() {
                "fabric" => Some(ModLoader::Fabric),
                "forge" => Some(ModLoader::Forge),
                "neoforge" => Some(ModLoader::NeoForge),
                "quilt" => Some(ModLoader::Quilt),
                _ => None,
            })
            .collect(),
        downloads: file.download_count as u64,
        changelog: None,
        file_name: Some(file.file_name.clone()),
        channel: match file.release_type {
            2 => Some("beta".to_string()),
            3 => Some("alpha".to_string()),
            _ => None,
        },
    }
}

/// CurseForge's `gameVersions` array on a file mixes real MC versions
/// ("1.20.1") with loader/side tags ("Forge", "Client", "Server") in the same
/// list — real versions always start with a digit, tags never do, so this is
/// enough to tell them apart without hardcoding every tag CF might add.
fn is_real_game_version(v: &str) -> bool {
    v.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn pick_cf_suggested(files: &[CurseForgeFileDetail], loaders: &[ModLoader]) -> (String, ModLoader) {
    let mc = files
        .iter()
        .flat_map(|f| f.game_versions.iter())
        .find(|v| is_real_game_version(v))
        .cloned()
        .unwrap_or_else(|| "1.21.1".to_string());
    let loader = loaders
        .first()
        .copied()
        .or_else(|| {
            files
                .iter()
                .flat_map(|f| f.mod_loaders.iter())
                .find_map(|l| match l.name.to_ascii_lowercase().as_str() {
                    "fabric" => Some(ModLoader::Fabric),
                    "forge" => Some(ModLoader::Forge),
                    "neoforge" => Some(ModLoader::NeoForge),
                    "quilt" => Some(ModLoader::Quilt),
                    _ => None,
                })
        })
        .unwrap_or(ModLoader::Forge);
    (mc, loader)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeMod {
    id: u32,
    slug: String,
    name: String,
    #[serde(default)]
    summary: String,
    // CurseForge returns downloadCount as a float for some projects, so parse
    // it as f64 and cast — parsing as u64 fails ("error decoding response body").
    #[serde(default)]
    download_count: f64,
    #[serde(default)]
    date_modified: String,
    #[serde(default)]
    class_id: Option<u32>,
    #[serde(default)]
    logo: Option<CurseForgeLogo>,
    #[serde(default)]
    authors: Vec<CurseForgeAuthor>,
    #[serde(default)]
    categories: Vec<CurseForgeCategory>,
    #[serde(default)]
    links: Option<CurseForgeLinks>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeModFile {
    id: u32,
    file_name: String,
    // `null` (not just `""`) for files the author blocked from third-party
    // distribution — deserializing that into a bare `String` used to fail
    // the whole batch response for every file in the same request.
    download_url: Option<String>,
    #[serde(default)]
    hashes: Vec<CurseForgeFileHash>,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<CurseForgeFileLoader>,
    #[serde(default)]
    dependencies: Vec<CurseForgeFileDependency>,
    /// Explicit `false` means CurseForge pulled the file — never resolve to
    /// it when picking "newest". Missing means available (fail-open: older
    /// cached shapes predate the field).
    #[serde(default = "default_true")]
    is_available: bool,
    /// 1 = release, 2 = beta, 3 = alpha. 0/unknown reads as release so a
    /// missing field never demotes a file.
    #[serde(default)]
    release_type: u8,
}

fn default_true() -> bool {
    true
}

/// Stable channel for "newest" picking: releases first, everything else
/// only when no release exists (a beta-only mod still installs — Prism
/// parity — rather than erroring).
fn is_stable_release(release_type: u8) -> bool {
    release_type != 2 && release_type != 3
}

#[derive(Debug, Clone, Deserialize)]
struct CurseForgeFileHash {
    value: String,
    // 1 = Sha1, 2 = Md5 per CurseForge's API.
    algo: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeFileDependency {
    mod_id: u32,
    // 1 = embedded, 2 = optional, 3 = required, 4 = tool, 5 = incompatible.
    relation_type: u8,
}

/// CurseForge's display name for a loader's category, used to check whether a
/// file is tagged for the instance's loader (`loaders` on File objects).
fn curseforge_loader_name(loader: ModLoader) -> Option<&'static str> {
    match loader {
        ModLoader::Forge => Some("Forge"),
        ModLoader::NeoForge => Some("NeoForge"),
        ModLoader::Fabric => Some("Fabric"),
        ModLoader::Quilt => Some("Quilt"),
        ModLoader::Vanilla => None,
    }
}

/// Refuses a file explicitly tagged for a *different* loader than the target.
/// Untagged files pass (plenty of older CurseForge files never declare a
/// loader category), and Vanilla targets pass (nothing to be wrong against).
/// This is what stops the fallback chain from quietly installing a Fabric jar
/// into a NeoForge instance when no exact-loader file exists.
fn ensure_file_loader_matches(
    file_loaders: &[CurseForgeFileLoader],
    target: ModLoader,
) -> Result<(), CurseForgeError> {
    if target == ModLoader::Vanilla || file_loaders.is_empty() {
        return Ok(());
    }
    let Some(want) = curseforge_loader_name(target) else {
        return Ok(());
    };
    if file_loaders
        .iter()
        .any(|l| l.name.eq_ignore_ascii_case(want))
    {
        return Ok(());
    }
    Err(CurseForgeError::NotFound)
}

/// The mod ids this file declares as hard requirements (relationType 3),
/// deduplicated. Optional/embedded/incompatible relations are ignored —
/// installing optional deps unasked is wrong, and incompatible ones must
/// obviously never be installed.
fn required_dependency_mod_ids_of_file(file: &CurseForgeModFile) -> Vec<u32> {
    let mut out = Vec::new();
    for dep in &file.dependencies {
        if dep.relation_type == 3 && !out.contains(&dep.mod_id) {
            out.push(dep.mod_id);
        }
    }
    out
}

/// Pulls the Sha1 out of a file's hash list, if CurseForge reported one —
/// used to identify a manually-downloaded replacement by content instead of
/// filename, which a browser can silently change ("mod (1).jar") on a
/// duplicate save.
fn sha1_of(hashes: &[CurseForgeFileHash]) -> Option<String> {
    hashes.iter().find(|h| h.algo == 1).map(|h| h.value.to_lowercase())
}

fn download_hashes(hashes: &[CurseForgeFileHash]) -> std::collections::HashMap<String, String> {
    hashes.iter().filter_map(|hash| {
        let algorithm = match hash.algo {
            1 => "sha1",
            2 => "md5",
            _ => return None,
        };
        Some((algorithm.to_string(), hash.value.clone()))
    }).collect()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeLogo {
    url: Option<String>,
    thumbnail_url: Option<String>,
}

/// Prefers the thumbnail, falling back to the full-size logo — but some
/// CurseForge projects (mostly modpacks, seen so far) report `thumbnailUrl`
/// as `Some("")` rather than `null`, and a plain `.or()` only falls through
/// on `None`. Without filtering the empty string out first, those projects'
/// icons resolve to an unusable blank string instead of the real `url`.
fn logo_icon_url(logo: Option<CurseForgeLogo>) -> Option<String> {
    logo.and_then(|l| l.thumbnail_url.filter(|s| !s.is_empty()).or(l.url))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeAuthor {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeCategory {
    name: String,
}

impl ModLoader {
    fn from_curseforge_categories(categories: &[CurseForgeCategory]) -> Vec<Self> {
        let mut loaders = Vec::new();
        for category in categories {
            let loader = match category.name.to_ascii_lowercase().as_str() {
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
}

#[cfg(test)]
mod distribution_restriction_tests {
    use super::{sha1_of, CurseForgeFileHash};

    // Real shape of CurseForge's `hashes` array: Sha1 is algo 1, Md5 is algo
    // 2 — mixed order here on purpose since the API doesn't guarantee it.
    #[test]
    fn picks_sha1_out_of_mixed_hash_list() {
        let hashes = vec![
            CurseForgeFileHash { value: "AABBCCDD".to_string(), algo: 2 },
            CurseForgeFileHash { value: "0123456789ABCDEF0123456789ABCDEF01234567".to_string(), algo: 1 },
        ];
        assert_eq!(sha1_of(&hashes).as_deref(), Some("0123456789abcdef0123456789abcdef01234567"));
    }

    #[test]
    fn no_sha1_entry_returns_none() {
        let hashes = vec![CurseForgeFileHash { value: "AABBCCDD".to_string(), algo: 2 }];
        assert_eq!(sha1_of(&hashes), None);
    }

    #[test]
    fn empty_hash_list_returns_none() {
        assert_eq!(sha1_of(&[]), None);
    }
}

#[cfg(test)]
mod fingerprint_match_tests {
    use super::FingerprintExactMatch;

    #[test]
    fn parses_exact_match_shape() {
        // Shape of POST /fingerprints data.exactMatches[] (trimmed — the
        // real objects carry far more fields, all ignored here).
        let raw = r#"{
            "id": 1234567,
            "file": {"modId": 56789, "fileName": "sodium-fabric-0.5.8.jar", "downloadUrl": "https://edge.forgecdn.net/…"}
        }"#;
        let m: FingerprintExactMatch = serde_json::from_str(raw).unwrap();
        assert_eq!(m.id, 1234567);
        assert_eq!(m.file.mod_id, 56789);
        assert_eq!(m.file.file_name, "sodium-fabric-0.5.8.jar");
    }
}

#[cfg(test)]
mod logo_icon_url_tests {
    use super::{logo_icon_url, CurseForgeLogo};

    #[test]
    fn prefers_thumbnail_when_present() {
        let logo = CurseForgeLogo {
            url: Some("https://example.com/full.png".to_string()),
            thumbnail_url: Some("https://example.com/thumb.png".to_string()),
        };
        assert_eq!(logo_icon_url(Some(logo)).as_deref(), Some("https://example.com/thumb.png"));
    }

    #[test]
    fn falls_back_to_full_logo_when_thumbnail_is_empty_string() {
        // Real shape seen from CurseForge for some modpacks: thumbnailUrl is
        // `""`, not `null` — a plain `.or()` never falls through for that.
        let logo = CurseForgeLogo {
            url: Some("https://example.com/full.png".to_string()),
            thumbnail_url: Some(String::new()),
        };
        assert_eq!(logo_icon_url(Some(logo)).as_deref(), Some("https://example.com/full.png"));
    }

    #[test]
    fn falls_back_to_full_logo_when_thumbnail_is_absent() {
        let logo = CurseForgeLogo { url: Some("https://example.com/full.png".to_string()), thumbnail_url: None };
        assert_eq!(logo_icon_url(Some(logo)).as_deref(), Some("https://example.com/full.png"));
    }

    #[test]
    fn no_logo_returns_none() {
        assert_eq!(logo_icon_url(None), None);
    }
}
