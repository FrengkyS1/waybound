use crate::download::{download_bytes, http_client, CancelToken};
use crate::dto::project_detail::{
    ModpackContentItem, ModpackContentKind, ModpackContentResponse,
};
use crate::modpack::modrinth::read_mrpack_index;
use crate::modpack::preview::{count_by_kind, file_name_from_path, kind_from_path};
use crate::sources::modrinth::ModrinthClient;

const BASE_URL: &str = "https://api.modrinth.com/v2";

pub async fn preview_modrinth_modpack(
    client: &ModrinthClient,
    version_id: &str,
) -> Result<ModpackContentResponse, String> {
    let t0 = std::time::Instant::now();
    let version = client
        .fetch_version_detail(version_id)
        .await
        .map_err(|err| err.to_string())?;
    let t1 = std::time::Instant::now();

    let http = http_client().map_err(|err| err.to_string())?;

    // The `.mrpack` index is the authoritative content list. Build items from it
    // (deduped by filename) and resolve friendly names from each file's Modrinth
    // download URL. Only fall back to the version's dependency list if we can't
    // read the index — merging both sources caused the same mod to appear twice.
    let mut items: Vec<ModpackContentItem> = Vec::new();
    let mut download_ms = 0u128;
    let mut build_ms = 0u128;
    let mut byte_len = 0usize;
    if let Some(download_url) = client.version_download_url(&version) {
        let t_dl = std::time::Instant::now();
        if let Ok(bytes) = download_bytes(&http, &download_url, &CancelToken::new()).await {
            download_ms = t_dl.elapsed().as_millis();
            byte_len = bytes.len();
            if let Ok(index) = read_mrpack_index(&bytes) {
                let t_build = std::time::Instant::now();
                items = build_items_from_index(&http, &version.id, &index).await;
                build_ms = t_build.elapsed().as_millis();
            }
        }
    }

    if items.is_empty() {
        let project_names = fetch_project_names(&http, &version.dependencies).await;
        items = items_from_dependencies(&version.dependencies, &project_names);
    }

    if items.is_empty() {
        return Err("This modpack version has no listable content.".to_string());
    }

    items.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));

    crate::activity::append_log(
        &format!(
            "Modrinth modpack preview timing: fetch-version={}ms download={}ms ({}KB) build-items(batch-names+dedupe)={}ms items={}",
            (t1 - t0).as_millis(),
            download_ms,
            byte_len / 1024,
            build_ms,
            items.len(),
        ),
        "debug",
        None,
    );

    let counts = count_by_kind(&items);
    Ok(ModpackContentResponse {
        version_id: version.id,
        version_name: version.name,
        items,
        counts,
    })
}

/// Build content items from the authoritative `.mrpack` index, resolving each
/// file's friendly project name from its Modrinth download URL.
async fn build_items_from_index(
    http: &reqwest::Client,
    version_id: &str,
    index: &crate::modpack::modrinth::ModrinthPackIndex,
) -> Vec<ModpackContentItem> {
    let mut ids: Vec<String> = Vec::new();
    for file in &index.files {
        if let Some(pid) = file.downloads.first().and_then(|u| project_id_from_download(u)) {
            if !ids.contains(&pid) {
                ids.push(pid);
            }
        }
    }
    let names = fetch_names_by_ids(http, &ids).await;

    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, file) in index.files.iter().enumerate() {
        if file.path.starts_with("overrides") {
            continue;
        }
        let file_name = file_name_from_path(&file.path);
        if file_name.is_empty() {
            continue;
        }
        if !seen.insert(file_name.to_ascii_lowercase()) {
            continue;
        }
        let env_client = file.env.as_ref().map(|env| env.client.clone());
        if env_client.as_deref() == Some("unsupported") {
            continue;
        }
        let kind = kind_from_path(&file.path);
        if kind == ModpackContentKind::Other && file.path.ends_with(".mrpack") {
            continue;
        }
        let name = file
            .downloads
            .first()
            .and_then(|u| project_id_from_download(u))
            .and_then(|id| names.get(&id).cloned())
            .unwrap_or_else(|| humanize_file_name(&file_name));
        items.push(ModpackContentItem {
            id: format!("{version_id}-file-{i}"),
            name,
            file_name,
            author: None,
            kind,
            required: env_client.as_deref() != Some("optional"),
            env_client,
            env_server: file.env.as_ref().and_then(|env| env.server.clone()),
        });
    }
    items
}

/// Extract the Modrinth project id from a CDN download URL
/// (`https://cdn.modrinth.com/data/<projectId>/versions/...`).
fn project_id_from_download(url: &str) -> Option<String> {
    url.split("/data/")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

// Modrinth documents no hard cap on `/projects?ids=[...]`, but a whole large
// modpack's worth of ids in one request is exactly the shape that silently
// lost data on CurseForge's batch endpoints — chunk it the same way rather
// than assume Modrinth is immune (see BATCH_CHUNK_SIZE in sources/curseforge.rs).
const PROJECT_LOOKUP_CHUNK_SIZE: usize = 200;

async fn fetch_names_by_ids(
    http: &reqwest::Client,
    ids: &[String],
) -> std::collections::HashMap<String, String> {
    if ids.is_empty() {
        return std::collections::HashMap::new();
    }
    let mut names = std::collections::HashMap::new();
    for chunk in ids.chunks(PROJECT_LOOKUP_CHUNK_SIZE) {
        let Ok(ids_json) = serde_json::to_string(chunk) else { continue };
        let Ok(response) = http
            .get(format!("{BASE_URL}/projects"))
            .query(&[("ids", ids_json.as_str())])
            .send()
            .await
        else {
            continue;
        };
        let Ok(projects) = response.json::<Vec<ModrinthProjectBrief>>().await else { continue };
        names.extend(projects.into_iter().map(|project| (project.id, project.title)));
    }
    if names.len() < ids.len() {
        crate::activity::append_log(
            &format!(
                "Modrinth project name lookup: requested {} ids, got {} back",
                ids.len(),
                names.len()
            ),
            "warn",
            None,
        );
    }
    names
}

fn items_from_dependencies(
    dependencies: &[crate::sources::modrinth::ModrinthVersionDependency],
    project_names: &std::collections::HashMap<String, String>,
) -> Vec<ModpackContentItem> {
    let mut items = Vec::new();
    for (index, dep) in dependencies.iter().enumerate() {
        if dep.dependency_type != "embedded" {
            continue;
        }
        let file_name = dep.file_name.clone().unwrap_or_default();
        let name = dep
            .project_id
            .as_ref()
            .and_then(|id| project_names.get(id).cloned())
            .or_else(|| dep.file_name.clone())
            .unwrap_or_else(|| format!("Mod {}", index + 1));
        let kind = if file_name.is_empty() {
            ModpackContentKind::Mod
        } else {
            kind_from_path(&format!("mods/{file_name}"))
        };
        items.push(ModpackContentItem {
            id: dep
                .project_id
                .as_ref()
                .map(|id| format!("dep-{id}"))
                .unwrap_or_else(|| format!("dep-file-{index}")),
            name,
            file_name,
            author: None,
            kind,
            required: true,
            env_client: None,
            env_server: None,
        });
    }
    items
}

async fn fetch_project_names(
    http: &reqwest::Client,
    dependencies: &[crate::sources::modrinth::ModrinthVersionDependency],
) -> std::collections::HashMap<String, String> {
    let ids: Vec<String> = dependencies
        .iter()
        .filter_map(|dep| dep.project_id.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    fetch_names_by_ids(http, &ids).await
}

fn humanize_file_name(file_name: &str) -> String {
    file_name
        .trim_end_matches(".jar")
        .replace(['-', '_'], " ")
}

#[derive(Debug, serde::Deserialize)]
struct ModrinthProjectBrief {
    id: String,
    title: String,
}
