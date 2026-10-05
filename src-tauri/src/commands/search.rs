use crate::config::ConfigStore;

use crate::db::{build_search_cache_key, Database, SEARCH_CACHE_TTL_SECS};

use crate::download::CancelToken;

use crate::dto::{ModSearchQuery, ModSearchResult, ModSummary, SortIndex};

use crate::identity::dedupe_mods;

use crate::sources::curseforge::{CurseForgeClient, CurseForgeError};

use crate::sources::modrinth::{ModrinthClient, ModrinthError};
use crate::sources::updated_key;

use std::collections::{HashMap, HashSet};
use std::future::Future;

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

    /// Cooperative cancellation for launch preparation, keyed by instance id.
    pub launches: Mutex<HashMap<String, CancelToken>>,

    /// Verified process identities, removed by the reaper when the game exits.
    pub game_pids: Mutex<HashMap<String, crate::commands::launch::ProcessIdentity>>,

    /// Single active Microsoft login, correlated with its frontend request.
    pub auth_login: Mutex<Option<(String, tokio::sync::watch::Sender<bool>)>>,

    /// Instances the user asked to stop: consulted (and cleared) by the
    /// reaper so a deliberate stop reports as stopped rather than crashed.
    pub stop_requests: Mutex<HashSet<String>>,

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



    // Empty queries browse both sources with the selected sort. Include key
    // availability in the cache key so enabling CurseForge cannot reuse a
    // previously cached Modrinth-only page.
    let curseforge_api_key = state.config.curseforge_api_key();
    let curseforge_included = curseforge_api_key.is_some();

    let cache_key = format!("merged-search-v3:{}", build_search_cache_key(&normalized, !curseforge_included));



    if let Ok(Some(cached)) = state.db.get_search_cache(&cache_key) {

        if cached.is_fresh(SEARCH_CACHE_TTL_SECS) {

            return Ok(cached.result);

        }

    }



    let state_ref = &*state;
    let (mut result, cacheable) = search_sources(
        &normalized,
        curseforge_included,
        |page| async move {
            state_ref.modrinth.search(&page).await.map_err(map_modrinth_error)
        },
        |page| {
            let api_key = curseforge_api_key.as_deref();
            async move {
                match api_key {
                    Some(api_key) => state_ref.curseforge.search(api_key, &page).await
                        .map_err(map_curseforge_search_warning),
                    None => Err("CurseForge: API key is not configured.".to_string()),
                }
            }
        },
    ).await?;


    if let Err(error) = state.db.upsert_identities(&result.hits) {

        result

            .warnings

            .push(format!("Could not save mod identities locally: {error}"));

    }



    if cacheable {
        if let Err(error) = state.db.put_search_cache(&cache_key, &result) {
            result.warnings.push(format!("Could not cache search results: {error}"));
        }
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

#[derive(Default)]
struct SourcePrefix {
    hits: Vec<ModSummary>,
    seen: HashSet<String>,
    next_offset: u32,
    total_hits: u32,
    exhausted: bool,
    succeeded: bool,
    error: Option<String>,
    warnings: Vec<String>,
}

impl SourcePrefix {
    fn finished(&self) -> bool {
        self.exhausted || self.error.is_some()
    }

    fn remaining(&self) -> u32 {
        if self.finished() { 0 } else { self.total_hits.saturating_sub(self.next_offset).max(1) }
    }
}

// Source offsets are always raw-source offsets. Each global page reconstructs
// cumulative prefixes from zero, so discarded merge overflow is never skipped.
async fn fill_source_prefix<F, Fut>(
    query: &ModSearchQuery,
    target: usize,
    prefix: &mut SourcePrefix,
    fetch: &F,
) where
    F: Fn(ModSearchQuery) -> Fut,
    Fut: Future<Output = Result<ModSearchResult, String>>,
{
    while !prefix.finished() && prefix.hits.len() < target {
        let mut page_query = query.clone();
        page_query.offset = prefix.next_offset;
        // CurseForge caps requests at 50; using one size also makes source
        // ranks independent of the user's selected global page size.
        page_query.limit = 50;
        match fetch(page_query).await {
            Ok(page) => {
                prefix.succeeded = true;
                prefix.warnings.extend(page.warnings);
                let raw_count = page.hits.len() as u32;
                prefix.total_hits = page.total_hits;
                prefix.next_offset = prefix.next_offset.saturating_add(raw_count);
                prefix.exhausted = raw_count == 0
                    || (page.total_hits > 0 && prefix.next_offset >= page.total_hits);
                for hit in page.hits {
                    if loader_matches(&hit, query.loader) && prefix.seen.insert(hit.uid.clone()) {
                        prefix.hits.push(hit);
                    }
                }
            }
            Err(error) => prefix.error = Some(error),
        }
    }
}

async fn search_sources<M, MFut, C, CFut>(
    query: &ModSearchQuery,
    curseforge_enabled: bool,
    modrinth_fetch: M,
    curseforge_fetch: C,
) -> Result<(ModSearchResult, bool), String>
where
    M: Fn(ModSearchQuery) -> MFut,
    MFut: Future<Output = Result<ModSearchResult, String>>,
    C: Fn(ModSearchQuery) -> CFut,
    CFut: Future<Output = Result<ModSearchResult, String>>,
{
    let mut modrinth = SourcePrefix::default();
    let mut curseforge = SourcePrefix {
        exhausted: !curseforge_enabled,
        warnings: if curseforge_enabled { Vec::new() } else {
            vec!["CurseForge: API key is not configured.".to_string()]
        },
        ..Default::default()
    };
    let page_end = query.offset.saturating_add(query.limit) as usize;
    // One eligible lookahead hit makes Next truthful at page boundaries.
    let required = page_end.saturating_add(1);
    let mut target = required;

    let mut hits = loop {
        tokio::join!(
            fill_source_prefix(query, target, &mut modrinth, &modrinth_fetch),
            fill_source_prefix(query, target, &mut curseforge, &curseforge_fetch),
        );
        if !modrinth.succeeded && !curseforge.succeeded {
            let errors: Vec<_> = [&modrinth.error, &curseforge.error]
                .into_iter().filter_map(|error| error.as_deref()).collect();
            let unavailable = if curseforge_enabled { "" } else {
                " CurseForge: API key is not configured."
            };
            return Err(format!("Search failed. {}{unavailable}", errors.join(" ")));
        }

        let hits = merge_prefixes(&modrinth.hits, &curseforge.hits, query);
        if hits.len() >= required || (modrinth.finished() && curseforge.finished()) {
            break hits;
        }
        // Loader postfilter and identity overlap can thin even full raw
        // prefixes. Keep fetching while any source still has raw pages.
        target = target.saturating_add(50);
    };

    let total_hits = (hits.len().min(u32::MAX as usize) as u32)
        .saturating_add(modrinth.remaining())
        .saturating_add(curseforge.remaining());
    // Source failure is explicit state, not inferred from user-facing wording.
    let cacheable = modrinth.error.is_none() && curseforge.error.is_none();
    let mut warnings = modrinth.warnings;
    warnings.extend(curseforge.warnings);
    warnings.extend(modrinth.error);
    warnings.extend(curseforge.error);
    let mut seen_warnings = HashSet::new();
    warnings.retain(|warning| seen_warnings.insert(warning.clone()));
    let offset = query.offset as usize;
    let page_hits = if offset < hits.len() {
        hits.drain(offset..page_end.min(hits.len())).collect()
    } else {
        Vec::new()
    };
    Ok((ModSearchResult {
        hits: page_hits,
        offset: query.offset,
        limit: query.limit,
        total_hits,
        warnings,
    }, cacheable))
}

fn loader_matches(hit: &ModSummary, loader: Option<crate::dto::ModLoader>) -> bool {
    loader.map_or(true, |loader| hit.loaders.is_empty() || hit.loaders.contains(&loader))
}


fn merge_prefixes(
    modrinth: &[ModSummary],
    curseforge: &[ModSummary],
    query: &ModSearchQuery,
) -> Vec<ModSummary> {
    let mut hits = Vec::with_capacity(modrinth.len() + curseforge.len());
    // Prefixes contain only loader-eligible hits. Unsupported early hits
    // cannot acquire a later counterpart's loader and jump into issued pages.
    let mut modrinth = modrinth.iter();
    let mut curseforge = curseforge.iter();
    // Interleave upstream ranks for Relevance/New: their scores/creation
    // dates are not comparable in ModSummary. Never replace them with
    // popularity or updated_at. Stable metric-sort ties use the same rank.
    loop {
        let left = modrinth.next();
        let right = curseforge.next();
        if left.is_none() && right.is_none() { break; }
        hits.extend(left.cloned());
        hits.extend(right.cloned());
    }
    let mut hits = filter_by_loader(dedupe_mods(hits), query.loader);
    match query.sort {
        SortIndex::Downloads => hits.sort_by(|a, b| b.downloads.cmp(&a.downloads)),
        SortIndex::Updated => hits.sort_by(|a, b| updated_key(&b.updated_at).cmp(updated_key(&a.updated_at))),
        SortIndex::New | SortIndex::Relevance => {}
    }
    hits
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

        // Only reachable from the install path; the search path never picks a
        // specific version, but the match arm is still required here.
        ModrinthError::Incompatible => {
            "That version is not built for this instance's Minecraft version and loader.".to_string()
        }

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

        CurseForgeError::WrongGameVersion { filename, expected, .. } => {
            format!("CurseForge: {filename} is not built for Minecraft {expected}.")
        }

        CurseForgeError::DistributionRestricted { filename, .. } => {

            format!("CurseForge: {filename} requires a manual download.")

        }

    }
}

#[cfg(test)]
mod tests {
    use super::{filter_by_loader, map_curseforge_search_warning, search_sources};
    use crate::dto::{ContentType, ModLoader, ModSearchQuery, ModSearchResult, ModSource, ModSummary, SortIndex};
    use std::collections::HashSet;
    use std::future::ready;
    use crate::sources::curseforge::CurseForgeError;

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
    fn keeps_hits_with_no_recognized_loaders_unfiltered() {        // Empty `loaders` means the source's categories didn't map to a
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

    fn mod_named(
        slug: &str,
        name: &str,
        author: &str,
        downloads: u64,
        source: ModSource,
    ) -> ModSummary {
        ModSummary {
            uid: match source {
                ModSource::Modrinth => format!("modrinth:{slug}"),
                ModSource::Curseforge => format!("curseforge:{}", slug.bytes()
                    .fold(0u32, |id, byte| id.wrapping_mul(31).wrapping_add(byte as u32))),
            },
            slug: slug.to_string(),
            name: name.to_string(),
            description: String::new(),
            author: author.to_string(),
            icon_url: None,
            downloads,
            project_type: ContentType::Mod,
            loaders: vec![],
            sources: vec![source],
            updated_at: String::new(),
            curseforge_id: if source == ModSource::Curseforge {
                Some(slug.bytes().fold(0u32, |id, byte| id.wrapping_mul(31).wrapping_add(byte as u32)))
            } else {
                None
            },
            modrinth_id: if source == ModSource::Modrinth {
                Some(slug.to_string())
            } else {
                None
            },
        }
    }

    fn query(sort: SortIndex, offset: u32, limit: u32) -> ModSearchQuery {
        ModSearchQuery {
            query: "test".to_string(),
            content_type: Some(ContentType::Mod),
            loader: None,
            sort,
            offset,
            limit,
        }
    }

    fn fixture_page(hits: &[ModSummary], query: &ModSearchQuery) -> ModSearchResult {
        ModSearchResult {
            hits: hits.iter().skip(query.offset as usize).take(query.limit as usize).cloned().collect(),
            offset: query.offset,
            limit: query.limit,
            total_hits: hits.len() as u32,
            warnings: Vec::new(),
        }
    }

    async fn fixture_search(
        query: &ModSearchQuery,
        modrinth: &[ModSummary],
        curseforge: &[ModSummary],
    ) -> ModSearchResult {
        search_sources(
            query,
            true,
            |query| ready(Ok(fixture_page(modrinth, &query))),
            |query| ready(Ok(fixture_page(curseforge, &query))),
        ).await.unwrap().0
    }

    fn projects(prefix: &str, start: u32, end: u32, source: ModSource) -> Vec<ModSummary> {
        (start..end).map(|index| {
            let name = format!("{prefix}-{index:03}");
            mod_named(&name, &name, "Author", 10_000 - index as u64, source)
        }).collect()
    }

    #[tokio::test]
    async fn popular_curseforge_only_hit_survives_page_truncate() {
        let modrinth = vec![
            mod_named("c", "C", "AuthorC", 30, ModSource::Modrinth),
            mod_named("b", "B", "AuthorB", 20, ModSource::Modrinth),
            mod_named("a", "A", "AuthorA", 10, ModSource::Modrinth),
        ];
        let curseforge = vec![
            mod_named("configured-cf", "Configured", "MrCrayFish", 1_000, ModSource::Curseforge),
        ];
        let result = fixture_search(&query(SortIndex::Downloads, 0, 3), &modrinth, &curseforge).await;
        assert_eq!(result.hits[0].slug, "configured-cf");
        assert_eq!(result.hits.len(), 3);
        let next = fixture_search(&query(SortIndex::Downloads, 3, 3), &modrinth, &curseforge).await;
        assert_eq!(next.hits[0].slug, "a");
        assert_eq!(next.total_hits, 4);
    }

    #[tokio::test]
    async fn overlap_and_truncation_never_skip_unique_source_hits() {
        let modrinth = projects("project", 0, 80, ModSource::Modrinth);
        let curseforge = projects("project", 50, 130, ModSource::Curseforge);
        let mut all = Vec::new();
        for offset in (0..150).step_by(25) {
            let page = fixture_search(&query(SortIndex::Downloads, offset, 25), &modrinth, &curseforge).await;
            assert_eq!(page.offset, offset);
            assert_eq!(page.limit, 25);
            all.extend(page.hits.into_iter().map(|hit| hit.slug));
        }
        assert_eq!(all, (0..130).map(|index| format!("project-{index:03}")).collect::<Vec<_>>());
        assert_eq!(all.iter().collect::<HashSet<_>>().len(), 130);
    }

    #[tokio::test]
    async fn one_source_cannot_starve_other_sources_later_pages() {
        let modrinth = projects("mr", 200, 275, ModSource::Modrinth);
        let curseforge = projects("cf", 0, 75, ModSource::Curseforge);
        let mut all = Vec::new();
        for offset in (0..150).step_by(25) {
            let page = fixture_search(&query(SortIndex::Downloads, offset, 25), &modrinth, &curseforge).await;
            all.extend(page.hits.into_iter().map(|hit| hit.slug));
        }
        assert_eq!(all.len(), 150);
        assert!(all[..75].iter().all(|slug| slug.starts_with("cf-")));
        assert!(all[75..].iter().all(|slug| slug.starts_with("mr-")));
        assert_eq!(all.iter().collect::<HashSet<_>>().len(), 150);
    }

    #[tokio::test]
    async fn global_limit_100_fetches_multiple_curseforge_raw_pages() {
        let curseforge = projects("cf", 0, 105, ModSource::Curseforge);
        let first = fixture_search(&query(SortIndex::Downloads, 0, 100), &[], &curseforge).await;
        assert_eq!(first.hits.len(), 100);
        assert_eq!((first.offset, first.limit, first.total_hits), (0, 100, 105));
        let last = fixture_search(&query(SortIndex::Downloads, 100, 100), &[], &curseforge).await;
        assert_eq!(last.hits.len(), 5);
        assert_eq!(last.hits[0].slug, "cf-100");
        assert_eq!((last.offset, last.limit, last.total_hits), (100, 100, 105));
        let beyond = fixture_search(&query(SortIndex::Downloads, 200, 100), &[], &curseforge).await;
        assert!(beyond.hits.is_empty());
        assert_eq!(beyond.total_hits, 105);
    }

    #[tokio::test]
    async fn loader_postfilter_fetches_past_empty_raw_pages() {
        let mut curseforge = projects("cf", 0, 105, ModSource::Curseforge);
        for (index, hit) in curseforge.iter_mut().enumerate() {
            hit.loaders = vec![if index < 100 { ModLoader::Fabric } else { ModLoader::Quilt }];
        }
        let mut query = query(SortIndex::Downloads, 0, 3);
        query.loader = Some(ModLoader::Quilt);
        let first = fixture_search(&query, &[], &curseforge).await;
        assert_eq!(first.hits.iter().map(|hit| hit.slug.as_str()).collect::<Vec<_>>(), ["cf-100", "cf-101", "cf-102"]);
        assert_eq!(first.total_hits, 5);
        query.offset = 3;
        let last = fixture_search(&query, &[], &curseforge).await;
        assert_eq!(last.hits.len(), 2);
        assert_eq!(last.total_hits, 5);
    }

    #[tokio::test]
    async fn cross_source_dedupe_refills_until_global_page_is_full() {
        let modrinth = projects("same", 0, 55, ModSource::Modrinth);
        let curseforge = projects("same", 0, 80, ModSource::Curseforge);
        let page = fixture_search(&query(SortIndex::Downloads, 50, 25), &modrinth, &curseforge).await;
        assert_eq!(page.hits.len(), 25);
        assert_eq!(page.hits[0].slug, "same-050");
        assert_eq!(page.hits[24].slug, "same-074");
        assert_eq!(page.total_hits, 80);
    }

    #[tokio::test]
    async fn selected_sort_does_not_collapse_to_downloads() {
        let mut modrinth = vec![
            mod_named("a", "A", "A", 20, ModSource::Modrinth),
            mod_named("b", "B", "B", 999, ModSource::Modrinth),
        ];
        let mut curseforge = vec![
            mod_named("c", "C", "C", 10, ModSource::Curseforge),
            mod_named("d", "D", "D", 800, ModSource::Curseforge),
        ];
        modrinth[0].updated_at = "2026-10-02T00:00:00Z".to_string();
        modrinth[1].updated_at = "2026-10-01T00:00:00Z".to_string();
        curseforge[0].updated_at = "2026-10-02T00:00:00.001Z".to_string();
        curseforge[1].updated_at = "2026-09-01T00:00:00Z".to_string();
        for (sort, expected) in [
            (SortIndex::Downloads, ["b", "d", "a", "c"]),
            (SortIndex::Updated, ["c", "a", "b", "d"]),
            (SortIndex::New, ["a", "c", "b", "d"]),
            (SortIndex::Relevance, ["a", "c", "b", "d"]),
        ] {
            let page = fixture_search(&query(sort, 0, 4), &modrinth, &curseforge).await;
            assert_eq!(page.hits.iter().map(|hit| hit.slug.as_str()).collect::<Vec<_>>(), expected, "{sort:?}");
        }
    }

    #[tokio::test]
    async fn relevance_ranks_remain_stable_across_page_boundaries() {
        let modrinth = projects("mr", 0, 65, ModSource::Modrinth);
        let curseforge = projects("cf", 0, 65, ModSource::Curseforge);
        for sort in [SortIndex::Relevance, SortIndex::New] {
            let mut all = Vec::new();
            for offset in (0..150).step_by(25) {
                let page = fixture_search(&query(sort, offset, 25), &modrinth, &curseforge).await;
                all.extend(page.hits.into_iter().map(|hit| hit.slug));
            }
            let expected: Vec<_> = (0..65).flat_map(|index|
                [format!("mr-{index:03}"), format!("cf-{index:03}")]).collect();
            assert_eq!(all, expected);
        }
    }

    #[tokio::test]
    async fn late_compatible_counterpart_never_moves_issued_page_boundaries() {
        let mut modrinth = projects("same", 0, 49, ModSource::Modrinth);
        for hit in &mut modrinth {
            hit.loaders = vec![ModLoader::Fabric];
        }
        modrinth.extend(projects("mr", 0, 101, ModSource::Modrinth));
        let mut curseforge = projects("cf", 0, 100, ModSource::Curseforge);
        curseforge.extend(projects("same", 0, 49, ModSource::Curseforge));
        for hit in modrinth.iter_mut().skip(49).chain(curseforge.iter_mut()) {
            hit.loaders = vec![ModLoader::Quilt];
        }
        // Equal comparable metrics exercise deterministic source-rank ties.
        for hit in modrinth.iter_mut().chain(curseforge.iter_mut()) {
            hit.downloads = 1;
            hit.updated_at = "2026-10-03T00:00:00Z".to_string();
        }
        let expected: Vec<_> = (0..149).flat_map(|index| {
            let mut slugs = Vec::new();
            if index < 101 { slugs.push(format!("mr-{index:03}")); }
            slugs.push(if index < 100 {
                format!("cf-{index:03}")
            } else {
                format!("same-{:03}", index - 100)
            });
            slugs
        }).collect();
        for sort in [SortIndex::Downloads, SortIndex::Updated, SortIndex::New, SortIndex::Relevance] {
            let mut all = Vec::new();
            for offset in (0..250).step_by(25) {
                let mut query = query(sort, offset, 25);
                query.loader = Some(ModLoader::Quilt);
                let page = fixture_search(&query, &modrinth, &curseforge).await;
                assert_eq!(page.hits.len(), 25, "{sort:?} offset {offset}");
                all.extend(page.hits.into_iter().map(|hit| hit.slug));
            }
            assert_eq!(all, expected, "{sort:?}");
            assert_eq!(all.iter().collect::<HashSet<_>>().len(), 250);
        }
    }

    #[tokio::test]
    async fn disabled_curseforge_warns_but_healthy_search_remains_cacheable() {
        let healthy = projects("mr", 0, 3, ModSource::Modrinth);
        let cf_calls = std::cell::Cell::new(0);
        let (page, cacheable) = search_sources(
            &query(SortIndex::Downloads, 0, 2), false,
            |page| ready(Ok(fixture_page(&healthy, &page))),
            |_| {
                cf_calls.set(cf_calls.get() + 1);
                ready(Err("Unexpected fetch".to_string()))
            },
        ).await.unwrap();
        assert_eq!(cf_calls.get(), 0);
        assert!(cacheable);
        assert_eq!(page.hits.len(), 2);
        assert_eq!(page.warnings, ["CurseForge: API key is not configured."]);
    }

    #[tokio::test]
    async fn degraded_cache_status_does_not_depend_on_warning_wording() {
        let healthy = projects("cf", 0, 3, ModSource::Curseforge);
        let (page, cacheable) = search_sources(
            &query(SortIndex::Downloads, 0, 2), true,
            |_| ready(Err("Could not reach Modrinth. Check your connection.".to_string())),
            |page| ready(Ok(fixture_page(&healthy, &page))),
        ).await.unwrap();
        assert!(!cacheable);
        assert_eq!(page.hits.len(), 2);
        assert_eq!(page.warnings, ["Could not reach Modrinth. Check your connection."]);
    }

    #[tokio::test]
    async fn either_source_can_fail_without_hiding_working_source() {
        let healthy = projects("healthy", 0, 3, ModSource::Curseforge);
        let query = query(SortIndex::Downloads, 0, 2);
        let mr_failed = search_sources(
            &query, true,
            |_| ready(Err("Modrinth request timed out.".to_string())),
            |page| {
                let mut result = fixture_page(&healthy, &page);
                result.warnings.push("Source metadata warning.".to_string());
                ready(Ok(result))
            },
        ).await.unwrap().0;
        assert_eq!(mr_failed.hits.len(), 2);
        assert_eq!(mr_failed.total_hits, 3);
        assert_eq!(mr_failed.warnings, ["Source metadata warning.", "Modrinth request timed out."]);
        let cf_failed = search_sources(
            &query, true,
            |page| ready(Ok(fixture_page(&healthy, &page))),
            |_| ready(Err("CurseForge: HTTP 403.".to_string())),
        ).await.unwrap().0;
        assert_eq!(cf_failed.hits.len(), 2);
        assert_eq!(cf_failed.warnings, ["CurseForge: HTTP 403."]);
        let both_failed = search_sources(
            &query, true,
            |_| ready(Err("Modrinth request timed out.".to_string())),
            |_| ready(Err("CurseForge: HTTP 403.".to_string())),
        ).await.unwrap_err();
        assert!(both_failed.contains("Modrinth request timed out."));
        assert!(both_failed.contains("CurseForge: HTTP 403."));
    }

    #[tokio::test]
    async fn failed_later_source_page_keeps_already_fetched_hits_and_warning() {
        let healthy = projects("healthy", 0, 75, ModSource::Modrinth);
        let page = search_sources(
            &query(SortIndex::Downloads, 25, 50), true,
            |page| ready(if page.offset == 0 {
                Ok(fixture_page(&healthy, &page))
            } else {
                Err("Modrinth request timed out.".to_string())
            }),
            |_| ready(Err("CurseForge: network error.".to_string())),
        ).await.unwrap().0;
        assert_eq!(page.hits.len(), 25);
        assert_eq!(page.total_hits, 50);
        assert_eq!(page.warnings, ["Modrinth request timed out.", "CurseForge: network error."]);
    }

    #[test]
    fn wrong_game_version_warning_names_file_and_version() {
        let msg = map_curseforge_search_warning(CurseForgeError::WrongGameVersion {
            filename: "biggerstacks-1.20.1-2026.06.17-all.jar".to_string(),
            file_versions: vec!["1.20.1".to_string()],
            expected: "1.21.1".to_string(),
        });
        assert!(msg.contains("biggerstacks-1.20.1-2026.06.17-all.jar"), "got: {msg}");
        assert!(msg.contains("1.21.1"), "got: {msg}");
    }
}


