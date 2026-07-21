use std::collections::HashMap;

use serde::Deserialize;

use crate::download::{download_bytes_with_retry, http_client, CancelToken};
use crate::dto::project_detail::{
    ModpackContentItem, ModpackContentKind, ModpackContentResponse,
};
use crate::modpack::curseforge::read_cf_manifest;
use crate::modpack::preview::count_by_kind;
use crate::sources::curseforge::CurseForgeClient;

const BASE_URL: &str = "https://api.curseforge.com/v1";

pub async fn preview_curseforge_modpack(
    client: &CurseForgeClient,
    mod_id: u32,
    file_id: u32,
    api_key: &str,
) -> Result<ModpackContentResponse, String> {
    let t0 = std::time::Instant::now();
    let url = client
        .file_download_url(mod_id, file_id, api_key)
        .await
        .map_err(|err| err.to_string())?;
    let http = http_client().map_err(|err| err.to_string())?;
    let t1 = std::time::Instant::now();
    let bytes = download_bytes_with_retry(&http, &url, &CancelToken::new())
        .await
        .map_err(|err| err.to_string())?;
    let t2 = std::time::Instant::now();
    let manifest = read_cf_manifest(&bytes).map_err(|err| err.to_string())?;
    let t3 = std::time::Instant::now();

    let mod_ids: Vec<u32> = manifest
        .files
        .iter()
        .map(|entry| entry.project_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let file_ids: Vec<u32> = manifest.files.iter().map(|entry| entry.file_id).collect();

    // Both are single batch requests regardless of pack size — this used to
    // be one HTTP round trip per file (300+ for a big pack, sequential),
    // which was the real cost of opening a modpack's content preview.
    let (mod_meta, file_names) = tokio::join!(
        fetch_mods_batch(&http, api_key, &mod_ids),
        client.file_names_batch(&file_ids, api_key),
    );
    let t4 = std::time::Instant::now();

    crate::activity::append_log(
        &format!(
            "CF modpack preview timing: resolve-url={}ms download={}ms ({}KB) parse-manifest={}ms batch-fetch={}ms files={}",
            (t1 - t0).as_millis(),
            (t2 - t1).as_millis(),
            bytes.len() / 1024,
            (t3 - t2).as_millis(),
            (t4 - t3).as_millis(),
            file_ids.len(),
        ),
        "debug",
        None,
    );

    let mut items = Vec::new();
    for entry in manifest.files {
        let meta = mod_meta.get(&entry.project_id);
        let file_name = file_names
            .get(&entry.file_id)
            .cloned()
            .unwrap_or_else(|| format!("mod-{}-{}.jar", entry.project_id, entry.file_id));
        items.push(ModpackContentItem {
            id: format!("{}-{}", entry.project_id, entry.file_id),
            name: meta
                .map(|m| m.name.clone())
                .unwrap_or_else(|| format!("Project {}", entry.project_id)),
            file_name,
            author: meta.and_then(|m| m.authors.first().map(|a| a.name.clone())),
            kind: ModpackContentKind::Mod,
            required: entry.required,
            env_client: None,
            env_server: None,
        });
    }

    let counts = count_by_kind(&items);
    Ok(ModpackContentResponse {
        version_id: file_id.to_string(),
        version_name: manifest.name,
        items,
        counts,
    })
}

async fn fetch_mods_batch(
    http: &reqwest::Client,
    api_key: &str,
    mod_ids: &[u32],
) -> HashMap<u32, CfModBrief> {
    if mod_ids.is_empty() {
        return HashMap::new();
    }
    // CurseForge's batch "Get Mods" lookup is POST /v1/mods with a JSON body
    // ({"modIds": [...]}), not a GET with a comma-joined query string — the
    // latter silently 4xxs, which is why every item fell back to "Project
    // <id>" instead of its real name.
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Body<'a> {
        mod_ids: &'a [u32],
    }
    let Ok(response) = http
        .post(format!("{BASE_URL}/mods"))
        .header("x-api-key", api_key)
        .header("Accept", "application/json")
        .json(&Body { mod_ids })
        .send()
        .await
    else {
        return HashMap::new();
    };
    let Ok(payload) = response.json::<CfModsResponse>().await else {
        return HashMap::new();
    };
    payload
        .data
        .into_iter()
        .map(|item| (item.id, item))
        .collect()
}

#[derive(Debug, Deserialize)]
struct CfModsResponse {
    data: Vec<CfModBrief>,
}

#[derive(Debug, Deserialize)]
struct CfModBrief {
    id: u32,
    name: String,
    #[serde(default)]
    authors: Vec<CfAuthor>,
}

#[derive(Debug, Deserialize)]
struct CfAuthor {
    name: String,
}
