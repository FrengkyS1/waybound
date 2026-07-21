use crate::config::ConfigStore;

use crate::db::{build_search_cache_key, Database, SEARCH_CACHE_TTL_SECS};

use crate::download::CancelToken;

use crate::dto::{ModSearchQuery, ModSearchResult, ModSummary};

use crate::identity::dedupe_mods;

use crate::sources::curseforge::{CurseForgeClient, CurseForgeError};

use crate::sources::modrinth::{ModrinthClient, ModrinthError};

use std::collections::HashMap;

use std::sync::Mutex;

use tauri::State;



pub struct AppState {

    pub modrinth: ModrinthClient,

    pub curseforge: CurseForgeClient,

    pub config: ConfigStore,

    pub db: Database,

    /// Cancel tokens for in-flight mod/modpack installs, keyed by the
    /// frontend-generated install id, so `cancel_install` can reach the
    /// right download loop.
    pub installs: Mutex<HashMap<String, CancelToken>>,

    /// Abort handles for in-flight launches (preparing/downloading phase),
    /// keyed by instance id, so `cancel_launch` can stop one.
    pub launches: Mutex<HashMap<String, tokio::task::AbortHandle>>,

}



#[tauri::command]

pub async fn search_mods(

    state: State<'_, AppState>,

    query: ModSearchQuery,

) -> Result<ModSearchResult, String> {

    let limit = query.limit.clamp(1, 100);

    let offset = query.offset;



    let mut normalized = query;

    normalized.limit = limit;

    normalized.offset = offset;



    // CurseForge's search endpoint sorts by popularity with no searchFilter
    // param when the query is blank (see build_search_params), so an empty
    // query is a legitimate "browse" request there too, same as Modrinth's
    // default listing — not a reason to skip the source. The cache key must
    // still reflect whether a key is configured, or a cached Modrinth-only
    // result (from before a key was added) gets served back after the key is
    // configured, silently hiding CurseForge hits until the cache expires.
    let curseforge_api_key = state.config.curseforge_api_key();
    let curseforge_included = curseforge_api_key.is_some();

    let cache_key = build_search_cache_key(&normalized, !curseforge_included);



    if let Ok(Some(cached)) = state.db.get_search_cache(&cache_key) {

        if cached.is_fresh(SEARCH_CACHE_TTL_SECS) {

            return Ok(cached.result);

        }

    }



    let modrinth_result = state

        .modrinth

        .search(&normalized)

        .await

        .map_err(map_modrinth_error)?;



    let curseforge_result = if let Some(api_key) = curseforge_api_key.filter(|_| curseforge_included) {

        state.curseforge.search(&api_key, &normalized).await

    } else {

        Ok(empty_result())

    };



    let mut modrinth_result = modrinth_result;



    let mut result = match curseforge_result {

        Ok(curseforge) => merge_results(modrinth_result, curseforge),

        Err(error) => {

            modrinth_result

                .warnings

                .push(map_curseforge_search_warning(error));

            modrinth_result

        }

    };



    result = dedupe_with_stats(result);
    result.hits = filter_by_loader(result.hits, normalized.loader);

    if let Err(error) = state.db.upsert_identities(&result.hits) {

        result

            .warnings

            .push(format!("Could not save mod identities locally: {error}"));

    }



    if let Err(error) = state.db.put_search_cache(&cache_key, &result) {

        result

            .warnings

            .push(format!("Could not cache search results: {error}"));

    }



    Ok(result)

}



/// CurseForge's modLoaderType filter (esp. Quilt=5) is unreliable server-side
/// and can return results that don't actually declare the requested loader.
/// Modrinth's facets are accurate, but re-checking both here costs nothing and
/// guarantees the UI never shows a loader the user didn't ask for.
///
/// A hit with an *empty* `loaders` list is let through unfiltered rather than
/// dropped: an empty list here means the source's categories didn't map to
/// any loader we recognize (a metadata gap on their end), not evidence the
/// mod doesn't support the requested one — dropping it would silently hide
/// an otherwise-matching result with no way for the user to know why.
fn filter_by_loader(hits: Vec<ModSummary>, loader: Option<crate::dto::ModLoader>) -> Vec<ModSummary> {
    match loader {
        Some(loader) => hits
            .into_iter()
            .filter(|hit| hit.loaders.is_empty() || hit.loaders.contains(&loader))
            .collect(),
        None => hits,
    }
}

fn dedupe_with_stats(mut result: ModSearchResult) -> ModSearchResult {

    let before = result.hits.len();

    result.hits = dedupe_mods(result.hits);

    let after = result.hits.len();

    if before > after {

        result.warnings.push(format!(

            "Merged {before} results into {after} unique mods (cross-source dedup)."

        ));

    }

    result

}



fn merge_results(mut modrinth: ModSearchResult, curseforge: ModSearchResult) -> ModSearchResult {

    let total_hits = modrinth.total_hits.saturating_add(curseforge.total_hits);

    modrinth.hits.extend(curseforge.hits);

    modrinth.total_hits = total_hits;

    modrinth.warnings.extend(curseforge.warnings);

    modrinth

}



fn empty_result() -> ModSearchResult {

    ModSearchResult {

        hits: Vec::<ModSummary>::new(),

        offset: 0,

        limit: 0,

        total_hits: 0,

        warnings: Vec::new(),

    }

}



fn map_modrinth_error(error: ModrinthError) -> String {

    match error {

        ModrinthError::Network(req_err) if req_err.is_timeout() => {

            "Modrinth request timed out. Check your connection.".to_string()

        }

        ModrinthError::Network(req_err) if req_err.is_connect() => {

            "Could not reach Modrinth. Check your connection.".to_string()

        }

        ModrinthError::Network(req_err) if req_err.status().is_some() => {

            format!("Modrinth returned an error ({})", req_err.status().unwrap())

        }

        ModrinthError::Network(_) => "Network error while contacting Modrinth.".to_string(),

        ModrinthError::NotFound => "Modrinth returned no compatible file.".to_string(),

        ModrinthError::Decode(message) => format!("Modrinth response parse error: {message}"),

    }

}



fn map_curseforge_search_warning(error: CurseForgeError) -> String {

    match error {

        CurseForgeError::Rejected { message, .. } => format!("CurseForge: {message}"),

        CurseForgeError::NotConfigured => "CurseForge: API key is not configured.".to_string(),

        CurseForgeError::Network(req_err) if req_err.is_connect() => {

            "CurseForge: could not connect.".to_string()

        }

        CurseForgeError::Network(_) => "CurseForge: network error.".to_string(),

        CurseForgeError::NotFound => "CurseForge: no compatible file.".to_string(),

        CurseForgeError::DistributionRestricted { filename, .. } => {

            format!("CurseForge: {filename} requires a manual download.")

        }

    }
}

#[cfg(test)]
mod tests {
    use super::filter_by_loader;
    use crate::dto::{ContentType, ModLoader, ModSource, ModSummary};

    fn mod_with_loaders(loaders: Vec<ModLoader>) -> ModSummary {
        ModSummary {
            uid: "test".to_string(),
            slug: "test".to_string(),
            name: "Test Mod".to_string(),
            description: String::new(),
            author: "author".to_string(),
            icon_url: None,
            downloads: 0,
            project_type: ContentType::Mod,
            loaders,
            sources: vec![ModSource::Modrinth],
            updated_at: String::new(),
            curseforge_id: None,
            modrinth_id: None,
        }
    }

    #[test]
    fn drops_hits_missing_the_requested_loader() {
        let hits = vec![
            mod_with_loaders(vec![ModLoader::Fabric]),
            mod_with_loaders(vec![ModLoader::Fabric, ModLoader::Quilt]),
        ];
        let filtered = filter_by_loader(hits, Some(ModLoader::Quilt));
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].loaders.contains(&ModLoader::Quilt));
    }

    #[test]
    fn no_loader_filter_keeps_everything() {
        let hits = vec![mod_with_loaders(vec![ModLoader::Fabric])];
        assert_eq!(filter_by_loader(hits, None).len(), 1);
    }

    #[test]
    fn keeps_hits_with_no_recognized_loaders_unfiltered() {
        // Empty `loaders` means the source's categories didn't map to a
        // known loader (a metadata gap), not proof the mod lacks the
        // requested one — must not be dropped.
        let hits = vec![
            mod_with_loaders(vec![]),
            mod_with_loaders(vec![ModLoader::Forge]),
        ];
        let filtered = filter_by_loader(hits, Some(ModLoader::Quilt));
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].loaders.is_empty());
    }
}


