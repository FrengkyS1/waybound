//! Support for the "download missing mods" flow: mods a CurseForge author
//! blocked from third-party/API download. The user still has to click
//! "Download" on the mod's own page themselves (that's the point — it's the
//! manual step the author's restriction asks for), but Waybound opens that
//! page in its own sandboxed window instead of the system browser, and
//! auto-places whatever lands in Downloads into the right instance folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindowBuilder};

use crate::dto::instance::MissingMod;
use crate::instances::paths::instance_root;
use crate::download::{contained_join, ensure_contained_path, CancelToken, DownloadError, MAX_DOWNLOAD_BYTES};
use crate::instances::operations::acquire;

const BROWSER_WINDOW_LABEL: &str = "missing-mods-browser";
const LOGIN_WINDOW_LABEL: &str = "curseforge-login";
const POLL_INTERVAL: Duration = Duration::from_secs(1);
// A flat 20 minutes was tuned for the single-mod flow — "Open all" hands the
// user several pages to click "Download" on in turn (ads, wait timers, a
// CAPTCHA here and there), and once this watch gives up, any file that lands
// afterward is never detected: it's not moved into the instance, and its
// window — having done its job as far as the user can tell — never gets
// closed. Scaling with mod count keeps the single-mod case's existing
// generous window while giving a big "Open all" batch room to actually
// finish.
const MIN_WATCH_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const WATCH_TIMEOUT_PER_MOD: Duration = Duration::from_secs(5 * 60);

static ACTIVE_WATCHES: LazyLock<Mutex<HashMap<String, CancelToken>>> = LazyLock::new(Default::default);

fn replace_watch(instance_id: &str, cancel: Option<CancelToken>) -> Result<(), String> {
    let mut watches = ACTIVE_WATCHES.lock().map_err(|_| "Downloads watcher registry is unavailable.".to_string())?;
    let previous = match cancel {
        Some(cancel) => watches.insert(instance_id.to_string(), cancel),
        None => watches.remove(instance_id),
    };
    if let Some(previous) = previous { previous.cancel(); }
    Ok(())
}

struct WatchGuard {
    instance_id: String,
    cancel: CancelToken,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        if let Ok(mut watches) = ACTIVE_WATCHES.lock() {
            if watches.get(&self.instance_id).is_some_and(|token| token.same_control(&self.cancel)) {
                watches.remove(&self.instance_id);
            }
        }
    }
}

fn watch_timeout(mod_count: usize) -> Duration {
    MIN_WATCH_TIMEOUT.max(WATCH_TIMEOUT_PER_MOD * mod_count as u32)
}

/// Opens a separate window at CurseForge's own login page alongside whatever
/// mod page was actually requested — a session isn't required to download
/// (an incognito tab works fine), but logging in once avoids CurseForge's
/// own login/SSO interstitials surprising the user mid-download later.
/// Skippable: the user can just ignore or close this window. Must run on the
/// spawned task like every other window creation in this file — WebView2
/// window creation isn't safe to call directly from a command's dispatcher
/// thread.
fn open_curseforge_login_window(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LOGIN_WINDOW_LABEL) {
        return window.set_focus().map_err(|err| format!("Could not focus CurseForge login window: {err}"));
    }
    let url = Url::parse("https://www.curseforge.com/account/login").map_err(|err| err.to_string())?;
    WebviewWindowBuilder::new(app, LOGIN_WINDOW_LABEL, WebviewUrl::External(url))
        .title("Log in to CurseForge (optional) \u{2014} Waybound")
        .inner_size(900.0, 700.0)
        .build()
        .map_err(|err| format!("Could not open CurseForge login window: {err}"))?;
    Ok(())
}

/// Same curseforge.com-only restriction the single-window flow uses below —
/// factored out so the "open all" command validates every URL with it too.
fn validate_curseforge_url(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url).map_err(|_| "Invalid URL.".to_string())?;
    let host = parsed.host_str().unwrap_or_default();
    if parsed.scheme() != "https" || !(host == "curseforge.com" || host.ends_with(".curseforge.com")) {
        return Err("Only curseforge.com links can be opened here.".to_string());
    }
    Ok(parsed)
}

/// Opens (or re-points, if already open) a small in-app browser window at a
/// mod's CurseForge page. Restricted to curseforge.com so this can't be
/// turned into a way to load an arbitrary/local URL — the window is never
/// added to any capability, so the page loaded in it has zero access to
/// Waybound's own commands, same as opening it in a real browser tab would.
///
/// The actual window creation/navigation runs on a spawned task rather than
/// inline: it has to round-trip through the main event loop, and a slow
/// first-time WebView2 init (or anything else that stalls it) must not block
/// this command's dispatcher thread — that thread pool is shared with every
/// other command, so a stuck window creation previously froze the whole app.
#[tauri::command]
pub async fn open_missing_mods_browser(
    app: AppHandle,
    state: tauri::State<'_, super::search::AppState>,
    url: String,
) -> Result<(), String> {
    let parsed = validate_curseforge_url(&url)?;
    let prompt_login = !state.config.curseforge_login_prompted();

    tauri::async_runtime::spawn(async move {
        if prompt_login {
            open_curseforge_login_window(&app)?;
        }
        if let Some(window) = app.get_webview_window(BROWSER_WINDOW_LABEL) {
            window.navigate(parsed).map_err(|err| format!("Could not navigate missing-mods browser: {err}"))?;
            window.set_focus().map_err(|err| format!("Could not focus missing-mods browser: {err}"))?;
        } else {
            WebviewWindowBuilder::new(&app, BROWSER_WINDOW_LABEL, WebviewUrl::External(parsed))
                .title("Download mod \u{2014} Waybound")
                .inner_size(1000.0, 800.0)
                .build()
                .map_err(|err| format!("Could not open missing-mods browser: {err}"))?;
        }
        Ok::<(), String>(())
    }).await.map_err(|err| format!("Could not open missing-mods browser: {err}"))??;
    if prompt_login {
        state.config.mark_curseforge_login_prompted().map_err(|err| format!("Could not save CurseForge login prompt preference: {err}"))?;
    }

    Ok(())
}

/// Opens every missing mod's page at once, each in its own sandboxed window
/// (same restriction and zero-capability sandboxing as the single-window
/// flow above), cascaded so they don't stack exactly on top of each other.
/// Lets the user work through "click Download" on each page without
/// round-tripping to the app's prev/next stepper between every one.
#[tauri::command]
pub async fn open_all_missing_mods_browsers(
    app: AppHandle,
    state: tauri::State<'_, super::search::AppState>,
    urls: Vec<String>,
) -> Result<(), String> {
    let parsed: Vec<Url> = urls
        .iter()
        .map(|u| validate_curseforge_url(u))
        .collect::<Result<_, _>>()?;
    if parsed.is_empty() { return Ok(()); }
    let prompt_login = !state.config.curseforge_login_prompted();

    tauri::async_runtime::spawn(async move {
        if prompt_login {
            open_curseforge_login_window(&app)?;
        }
        let mut errors = Vec::new();
        for (i, url) in parsed.into_iter().enumerate() {
            let label = format!("{BROWSER_WINDOW_LABEL}-{i}");
            let offset = (i as f64) * 30.0;
            let result = if let Some(window) = app.get_webview_window(&label) {
                window.navigate(url).and_then(|()| window.set_focus()).map(|()| ())
            } else {
                WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(url))
                    .title("Download mod \u{2014} Waybound")
                    .inner_size(1000.0, 800.0)
                    .position(80.0 + offset, 80.0 + offset)
                    .build().map(|_| ())
            };
            if let Err(err) = result {
                errors.push(format!("Could not open missing-mods browser {}: {err}", i + 1));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(errors.join("\n")) }
    }).await.map_err(|err| format!("Could not open missing-mods browsers: {err}"))??;
    if prompt_login {
        state.config.mark_curseforge_login_prompted().map_err(|err| format!("Could not save CurseForge login prompt preference: {err}"))?;
    }

    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MissingModPlacedEvent {
    instance_id: String,
    name: String,
    remaining: u32,
    total: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MissingModsWatchDoneEvent {
    instance_id: String,
    placed: Vec<String>,
    still_missing: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MissingModsWatchErrorEvent {
    instance_id: String,
    error: String,
}

fn report_watch_error(app: &AppHandle, instance_id: &str, error: String) {
    let _ = app.emit("missing-mods://error", MissingModsWatchErrorEvent {
        instance_id: instance_id.to_string(),
        error,
    });
}

/// A retry/dismiss can change page indices. Match the URL, never close another
/// download page merely because its old index was reused.
fn close_missing_mods_window(app: &AppHandle, url: &str) {
    for (label, window) in app.webview_windows() {
        if (label == BROWSER_WINDOW_LABEL || label.starts_with("missing-mods-browser-"))
            && window.url().is_ok_and(|current| current.as_str() == url)
        {
            let _ = window.close();
        }
    }
}

/// Extensions a manually-downloaded mod/resourcepack file can plausibly
/// have — used to skip hashing everything else sitting in Downloads (an
/// installer, a screenshot, a video) on every poll tick.
fn is_candidate_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("jar") | Some("zip")
    )
}

/// Starts watching the user's Downloads folder for the given files and moves
/// each one into the instance's mods/resourcepacks folder the moment it
/// shows up. Matched by content (Sha1) when CurseForge reported one for the
/// file — a browser silently renaming a duplicate save ("mod (1).jar")
/// doesn't break placement — falling back to CurseForge's exact filename
/// only for the rare file with no reported hash. Returns immediately;
/// progress comes through `missing-mods://placed` and `missing-mods://done`
/// events, since a single grab can take the user several minutes across
/// multiple mod pages.
/// Same signal `sniff_is_shaderpack` in `modpack::curseforge` uses (a
/// top-level `shaders/` directory) — needed here too because CurseForge's
/// "missing mods" list carries no content-type info, just a filename and
/// project id, and a plain `.zip` extension doesn't distinguish a shader pack
/// from a resource pack.
fn is_shaderpack_file(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return false;
    };
    (0..archive.len()).any(|i| {
        archive
            .by_index(i)
            .is_ok_and(|f| f.name().to_ascii_lowercase().starts_with("shaders/"))
    })
}

fn hash_download(path: &Path, cancel: &CancelToken) -> Result<String, DownloadError> {
    use sha1::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = sha1::Sha1::new();
    let mut received = 0usize;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.is_cancelled() { return Err(DownloadError::Cancelled); }
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        if count > MAX_DOWNLOAD_BYTES.saturating_sub(received) {
            return Err(DownloadError::TooLarge(MAX_DOWNLOAD_BYTES));
        }
        hash.update(&buffer[..count]);
        received += count;
    }
    Ok(hex::encode(hash.finalize()))
}

fn map_pack_error(error: crate::modpack::ModpackError) -> DownloadError {
    match error {
        crate::modpack::ModpackError::Download(error) => error,
        crate::modpack::ModpackError::Io(error) => DownloadError::Io(error),
        error => DownloadError::Io(std::io::Error::other(error.to_string())),
    }
}

fn record_manual_pack_download(
    db: &crate::db::Database,
    instance_id: &str,
    item: &MissingMod,
    target: &Path,
) -> Result<(), crate::modpack::ModpackError> {
    let uid = format!("curseforge:{}", item.project_id);
    let existing = db.get_instance_mod(instance_id, &uid)
        .map_err(|error| crate::modpack::ModpackError::Other(format!("Could not read manual-download tracking: {error}")))?;
    let name = existing.as_ref().map_or(item.name.as_str(), |(row, _)| row.mod_name.as_str());
    let icon = existing.as_ref().and_then(|(row, _)| row.icon_url.as_deref());
    let origin = existing.as_ref().map_or(crate::dto::ModOrigin::Pack, |(row, _)| row.origin);
    let filename = target.file_name().and_then(|name| name.to_str())
        .ok_or_else(|| crate::modpack::ModpackError::Other("Manual-download target has an invalid filename".into()))?;
    db.insert_instance_mod(
        instance_id, &uid, name, crate::dto::ModSource::Curseforge, filename,
        &target.display().to_string(), icon, origin,
    ).map_err(|error| crate::modpack::ModpackError::Other(format!("Could not record manual download: {error}")))?;
    Ok(())
}

/// Stage and verify a complete sibling before publication. Pack receipts own
/// pending replacements; unrelated existing destinations are never clobbered.
fn place_download(
    downloads_dir: &Path,
    source: &Path,
    root: &Path,
    dest_dir: &Path,
    dest: &Path,
    item: &MissingMod,
    cancel: &CancelToken,
    publish: impl FnOnce(&Path) -> Result<(), crate::modpack::ModpackError>,
) -> Result<(), DownloadError> {
    use sha1::Digest;
    use std::io::{Read, Write};

    ensure_contained_path(downloads_dir, source)?;
    ensure_contained_path(root, dest_dir)?;
    ensure_contained_path(dest_dir, dest)?;
    let mut input = std::fs::File::open(source)?;
    let mut staged = tempfile::Builder::new().prefix(".waybound-manual-").tempfile_in(dest_dir)?;
    let mut digest = sha1::Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut received = 0usize;
    loop {
        if cancel.is_cancelled() { return Err(DownloadError::Cancelled); }
        let count = input.read(&mut buffer)?;
        if count == 0 { break; }
        if count > MAX_DOWNLOAD_BYTES.saturating_sub(received) {
            return Err(DownloadError::TooLarge(MAX_DOWNLOAD_BYTES));
        }
        staged.write_all(&buffer[..count])?;
        digest.update(&buffer[..count]);
        received += count;
    }
    let actual_sha1 = hex::encode(digest.finalize());
    if item.sha1.as_deref().is_some_and(|expected| !actual_sha1.eq_ignore_ascii_case(expected)) {
        return Err(DownloadError::HashMismatch("sha1".into()));
    }
    zip::ZipArchive::new(staged.reopen()?).map_err(|err| {
        DownloadError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Downloaded archive is incomplete or invalid: {err}")))
    })?;
    staged.as_file().sync_all()?;
    if cancel.is_cancelled() { return Err(DownloadError::Cancelled); }
    ensure_contained_path(dest_dir, dest)?;
    if let Some((transaction, target)) = crate::modpack::prepare_manual_pack_replacement(root, item.project_id, &item.filename, staged.path())
        .map_err(map_pack_error)?
    {
        tauri::async_runtime::block_on(transaction.commit_with(cancel, || publish(&target))).map_err(map_pack_error)?;
    } else {
        let disabled = contained_join(dest_dir, &format!("{}{}", item.filename, crate::commands::content::DISABLED_SUFFIX))?;
        let dest = if disabled.exists() { disabled.as_path() } else { dest };
        match std::fs::symlink_metadata(dest) {
            Ok(_) => {
                if !hash_download(dest, cancel)?.eq_ignore_ascii_case(&actual_sha1) {
                    return Err(DownloadError::Io(std::io::Error::new(std::io::ErrorKind::AlreadyExists,
                        "Existing destination differs from this download; move it aside before retrying.")));
                }
                // A verified existing destination is already placed; preserve it.
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                staged.persist_noclobber(dest).map_err(|err| DownloadError::Io(err.error))?;
            }
            Err(err) => return Err(err.into()),
        }
    }
    // Source cleanup is best effort, only for the same bytes we published.
    if ensure_contained_path(downloads_dir, source).is_ok()
        && hash_download(source, cancel).is_ok_and(|hash| hash.eq_ignore_ascii_case(&actual_sha1))
    {
        let _ = std::fs::remove_file(source);
    }
    Ok(())
}

#[tauri::command]
pub fn watch_for_missing_mods(app: AppHandle, instance_id: String, mods: Vec<MissingMod>) -> Result<(), String> {
    if mods.is_empty() {
        return replace_watch(&instance_id, None);
    }
    // Reserve ownership before validation: even a failed restart silences the
    // old watcher, and a slower earlier request cannot replace a newer one.
    let cancel = CancelToken::new();
    replace_watch(&instance_id, Some(cancel.clone()))?;
    let watch = WatchGuard { instance_id: instance_id.clone(), cancel: cancel.clone() };
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let mods_dir = contained_join(&root, "mods").map_err(|e| e.to_string())?;
    let resourcepacks_dir = contained_join(&root, "resourcepacks").map_err(|e| e.to_string())?;
    let shaderpacks_dir = contained_join(&root, "shaderpacks").map_err(|e| e.to_string())?;
    for dir in [&mods_dir, &resourcepacks_dir, &shaderpacks_dir] {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        ensure_contained_path(&root, dir).map_err(|e| e.to_string())?;
    }
    for item in &mods {
        contained_join(&mods_dir, &item.filename).map_err(|e| e.to_string())?;
    }
    let downloads_dir = dirs::download_dir().ok_or_else(|| "Could not locate your Downloads folder.".to_string())?;
    ensure_contained_path(&downloads_dir, &downloads_dir).map_err(|e| e.to_string())?;
    std::fs::read_dir(&downloads_dir).map_err(|e| format!("Could not read Downloads: {e}. Check folder access and retry."))?;

    // Hashing/copying large archives must not block async command workers.
    tauri::async_runtime::spawn_blocking(move || {
        let _watch = watch;
        let mut remaining = mods;
        let total = remaining.len() as u32;
        let mut placed_names = Vec::new();
        let deadline = Instant::now() + watch_timeout(remaining.len());
        let mut hash_cache: HashMap<PathBuf, (SystemTime, u64, String)> = HashMap::new();
        let result = (|| -> Result<(), String> {
            while !remaining.is_empty() && Instant::now() < deadline {
                if cancel.is_cancelled() { return Ok(()); }
                let entries = std::fs::read_dir(&downloads_dir)
                    .map_err(|e| format!("Could not read Downloads: {e}. Check folder access and retry watching."))?;
                for entry in entries {
                    if cancel.is_cancelled() || remaining.is_empty() { break; }
                    let entry = entry.map_err(|e| format!("Could not inspect Downloads: {e}. Retry watching."))?;
                    let source = entry.path();
                    if !is_candidate_file(&source) { continue; }
                    ensure_contained_path(&downloads_dir, &source).map_err(|e| format!("Unsafe download path: {e}"))?;
                    let metadata = std::fs::symlink_metadata(&source).map_err(|e| format!("Could not inspect downloaded file: {e}. Retry watching."))?;
                    if !metadata.is_file() { continue; }
                    if metadata.len() > MAX_DOWNLOAD_BYTES as u64 {
                        if remaining.iter().any(|item| source.file_name().and_then(|name| name.to_str()) == Some(item.filename.as_str())) {
                            return Err(format!("Downloaded file exceeds the {MAX_DOWNLOAD_BYTES}-byte limit. Choose the expected file and retry watching."));
                        }
                        continue;
                    }
                    let source_hash = if remaining.iter().any(|item| item.sha1.is_some()) {
                        let mtime = metadata.modified().map_err(|e| e.to_string())?;
                        if !hash_cache.get(&source).is_some_and(|(cached_time, cached_len, _)| *cached_time == mtime && *cached_len == metadata.len()) {
                            let hash = hash_download(&source, &cancel).map_err(|e| format!("Could not verify downloaded file: {e}. Wait for browser download to finish and retry watching."))?;
                            hash_cache.insert(source.clone(), (mtime, metadata.len(), hash));
                        }
                        hash_cache.get(&source).map(|(_, _, hash)| hash.as_str())
                    } else {
                        None
                    };
                    let matched_index = remaining.iter().position(|item| match &item.sha1 {
                        Some(expected) => source_hash.is_some_and(|hash| hash.eq_ignore_ascii_case(expected)),
                        None => source.file_name().and_then(|name| name.to_str()) == Some(item.filename.as_str()),
                    });
                    let Some(idx) = matched_index else { continue };
                    let dest_dir = if remaining[idx].filename.to_ascii_lowercase().ends_with(".jar") {
                        &mods_dir
                    } else if is_shaderpack_file(&source) {
                        &shaderpacks_dir
                    } else {
                        &resourcepacks_dir
                    };
                    let dest = contained_join(dest_dir, &remaining[idx].filename).map_err(|e| e.to_string())?;
                    let _operation = acquire(&instance_id).map_err(|e| format!("Could not place {}: {e} Retry watching after the instance is idle.", remaining[idx].name))?;
                    match place_download(&downloads_dir, &source, &root, dest_dir, &dest, &remaining[idx], &cancel, |target| {
                        record_manual_pack_download(&app.state::<super::search::AppState>().db, &instance_id, &remaining[idx], target)
                    }) {
                        Err(DownloadError::Cancelled) => return Ok(()),
                        Err(err) => return Err(format!("Could not place {}: {err}. Original download and existing destination are preserved; retry watching after resolving this.", remaining[idx].name)),
                        Ok(()) => {}
                    }
                    let watches = ACTIVE_WATCHES.lock().map_err(|_| "Downloads watcher registry is unavailable.".to_string())?;
                    if cancel.is_cancelled() || !watches.get(&instance_id).is_some_and(|token| token.same_control(&cancel)) {
                        return Ok(());
                    }
                    let entry = remaining.remove(idx);
                    hash_cache.remove(&source);
                    close_missing_mods_window(&app, &entry.url);
                    placed_names.push(entry.name.clone());
                    let _ = app.emit("missing-mods://placed", MissingModPlacedEvent {
                        instance_id: instance_id.clone(), name: entry.name,
                        remaining: remaining.len() as u32, total,
                    });
                }
                if !remaining.is_empty() {
                    let wait = tauri::async_runtime::block_on(async {
                        cancel.wait(tokio::time::sleep(POLL_INTERVAL)).await
                    });
                    if matches!(wait, Err(DownloadError::Cancelled)) { return Ok(()); }
                }
            }
            Ok(())
        })();
        let Ok(watches) = ACTIVE_WATCHES.lock() else { return };
        if cancel.is_cancelled() || !watches.get(&instance_id).is_some_and(|token| token.same_control(&cancel)) { return; }
        if let Err(error) = result { report_watch_error(&app, &instance_id, error); }
        let _ = app.emit("missing-mods://done", MissingModsWatchDoneEvent {
            instance_id, placed: placed_names,
            still_missing: remaining.into_iter().map(|item| item.name).collect(),
        });
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::io::Write;

    #[test]
    fn replacing_watch_cancels_old_without_old_guard_removing_new_watch() {
        let id = "manual-watch-replacement-fixture";
        let first = CancelToken::new();
        replace_watch(id, Some(first.clone())).unwrap();
        let old_guard = WatchGuard { instance_id: id.into(), cancel: first.clone() };
        let second = CancelToken::new();
        replace_watch(id, Some(second.clone())).unwrap();
        assert!(first.is_cancelled());
        drop(old_guard);
        assert!(ACTIVE_WATCHES.lock().unwrap().get(id).unwrap().same_control(&second));
        replace_watch(id, None).unwrap();
        assert!(second.is_cancelled());
        assert!(!ACTIVE_WATCHES.lock().unwrap().contains_key(id));
    }

    fn archive(bytes: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer.start_file("fixture.txt", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(bytes).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn missing(bytes: &[u8]) -> MissingMod {
        use sha1::Digest;
        MissingMod {
            project_id: 123, name: "Fixture".into(), filename: "fixture.jar".into(),
            url: "https://www.curseforge.com/minecraft/mc-mods/fixture/download/123".into(),
            sha1: Some(hex::encode(sha1::Sha1::digest(bytes))),
        }
    }

    #[test]
    fn manual_copy_verifies_sibling_before_publish_and_cleans_source() {
        let downloads = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let bytes = archive(b"complete");
        let source = downloads.path().join("fixture (1).jar");
        let dest = root.path().join("fixture.jar");
        std::fs::write(&source, &bytes).unwrap();
        place_download(downloads.path(), &source, root.path(), root.path(), &dest, &missing(&bytes), &CancelToken::new(), |_| Ok(())).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
        assert!(!source.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn mismatched_or_partial_manual_copy_preserves_existing_and_source() {
        let downloads = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let source = downloads.path().join("fixture.jar");
        let dest = root.path().join("fixture.jar");
        let expected = archive(b"complete");
        std::fs::write(&source, b"partial").unwrap();
        std::fs::write(&dest, b"original").unwrap();
        assert!(matches!(
            place_download(downloads.path(), &source, root.path(), root.path(), &dest, &missing(&expected), &CancelToken::new(), |_| Ok(())),
            Err(DownloadError::HashMismatch(_))
        ));
        assert_eq!(std::fs::read(&source).unwrap(), b"partial");
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        let mut no_hash = missing(&expected);
        no_hash.sha1 = None;
        assert!(place_download(downloads.path(), &source, root.path(), root.path(), &dest, &no_hash, &CancelToken::new(), |_| Ok(())).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn unrelated_existing_destination_is_not_clobbered_by_valid_download() {
        let downloads = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let bytes = archive(b"replacement");
        let source = downloads.path().join("fixture.jar");
        let dest = root.path().join("fixture.jar");
        std::fs::write(&source, &bytes).unwrap();
        std::fs::write(&dest, b"unrelated").unwrap();
        assert!(place_download(downloads.path(), &source, root.path(), root.path(), &dest, &missing(&bytes), &CancelToken::new(), |_| Ok(())).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert_eq!(std::fs::read(&dest).unwrap(), b"unrelated");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn cancelled_manual_copy_never_publishes_or_removes_source() {
        let downloads = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let bytes = archive(b"complete");
        let source = downloads.path().join("fixture.jar");
        let dest = root.path().join("fixture.jar");
        std::fs::write(&source, &bytes).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(place_download(downloads.path(), &source, root.path(), root.path(), &dest, &missing(&bytes), &cancel, |_| Ok(())), Err(DownloadError::Cancelled)));
        assert!(!dest.exists());
        assert!(source.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    fn pending_receipt(root: &Path, old_bytes: &[u8], new_bytes: &[u8]) -> PathBuf {
        use sha1::Digest;
        let receipt = root.join(".curseforge-pack-manifest.json");
        let item = missing(new_bytes);
        let entries = serde_json::json!([{
            "project_id": item.project_id, "file_id": 456, "name": item.name,
            "filename": item.filename, "url": item.url, "sha1": item.sha1,
            "pending": true,
            "retained_files": [{
                "filename": "old.jar",
                "sha1": hex::encode(sha1::Sha1::digest(old_bytes))
            }]
        }]);
        std::fs::write(&receipt, serde_json::to_vec(&entries).unwrap()).unwrap();
        receipt
    }

    fn fixture_database(path: &Path, root: &Path) -> crate::db::Database {
        let db = crate::db::Database::open_at(path).unwrap();
        db.insert_instance(&crate::dto::instance::InstanceSummary {
            id: "manual-fixture".into(), name: "Manual fixture".into(),
            minecraft_version: "1.20.1".into(), loader: crate::dto::ModLoader::Forge,
            loader_version: None, mod_count: 0, created_at: 1,
            root_path: root.display().to_string(), icon: None, last_played: None,
            total_play_seconds: 0, modpack_version_label: None, modpack_project_uid: None,
        }).unwrap();
        db
    }

    fn record_old_fixture(db: &crate::db::Database, old: &Path) -> crate::dto::instance::InstalledMod {
        db.insert_instance_mod(
            "manual-fixture", "curseforge:123", "Tracked project name",
            crate::dto::ModSource::Curseforge,
            old.file_name().unwrap().to_str().unwrap(), &old.display().to_string(),
            Some("data:image/png;base64,fixture"), crate::dto::ModOrigin::Pack,
        ).unwrap()
    }

    #[test]
    fn manual_pack_replacement_preserves_disabled_state_and_publishes_receipt() {
        let downloads = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mods = root.path().join("mods");
        std::fs::create_dir(&mods).unwrap();
        let old_bytes = archive(b"old pack version");
        let new_bytes = archive(b"new pack version");
        let old = mods.join(format!("old.jar{}", crate::commands::content::DISABLED_SUFFIX));
        std::fs::write(&old, &old_bytes).unwrap();
        let dbdir = tempfile::tempdir().unwrap();
        let db = fixture_database(&dbdir.path().join("library.db"), root.path());
        let previous = record_old_fixture(&db, &old);
        let receipt = pending_receipt(root.path(), &old_bytes, &new_bytes);
        let source = downloads.path().join("fixture (1).jar");
        std::fs::write(&source, &new_bytes).unwrap();
        let dest = mods.join("fixture.jar");
        place_download(downloads.path(), &source, root.path(), &mods, &dest, &missing(&new_bytes), &CancelToken::new(), |target| {
            record_manual_pack_download(&db, "manual-fixture", &missing(&new_bytes), target)
        }).unwrap();
        let disabled = mods.join(format!("fixture.jar{}", crate::commands::content::DISABLED_SUFFIX));
        assert_eq!(std::fs::read(disabled).unwrap(), new_bytes);
        assert!(!dest.exists());
        assert!(!old.exists());
        assert!(!source.exists());
        let entries: serde_json::Value = serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
        assert_eq!(entries[0]["pending"], false);
        assert_eq!(entries[0]["retained_files"], serde_json::json!([]));
        assert_eq!(entries[0]["sha1"], serde_json::json!(missing(&new_bytes).sha1.unwrap()));
        let (tracked, path) = db.get_instance_mod("manual-fixture", "curseforge:123").unwrap().unwrap();
        assert_eq!(tracked.id, previous.id);
        assert_eq!(tracked.mod_name, previous.mod_name);
        assert_eq!(tracked.icon_url, previous.icon_url);
        assert_eq!(tracked.origin, previous.origin);
        assert_eq!(tracked.file_name, format!("fixture.jar{}", crate::commands::content::DISABLED_SUFFIX));
        assert_eq!(Path::new(&path), mods.join(&tracked.file_name));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn manual_database_publication_failure_rolls_back_files_receipt_and_tracking() {
        let downloads = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let dbdir = tempfile::tempdir().unwrap();
        let db = fixture_database(&dbdir.path().join("library.db"), root.path());
        let mods = root.path().join("mods");
        std::fs::create_dir(&mods).unwrap();
        let old_bytes = archive(b"old pack version");
        let new_bytes = archive(b"new pack version");
        let old = mods.join("old.jar");
        std::fs::write(&old, &old_bytes).unwrap();
        let previous = record_old_fixture(&db, &old);
        let receipt = pending_receipt(root.path(), &old_bytes, &new_bytes);
        let original_receipt = std::fs::read(&receipt).unwrap();
        let source = downloads.path().join("fixture.jar");
        std::fs::write(&source, &new_bytes).unwrap();
        let dest = mods.join("fixture.jar");
        db.conn().unwrap().execute_batch(
            "CREATE TRIGGER reject_manual_metadata BEFORE UPDATE ON instance_mods
             BEGIN SELECT RAISE(ABORT, 'metadata publication fixture'); END;"
        ).unwrap();
        let result = place_download(
            downloads.path(), &source, root.path(), &mods, &dest, &missing(&new_bytes), &CancelToken::new(),
            |target| {
                assert_eq!(target, dest);
                assert!(dest.exists());
                assert!(!old.exists());
                record_manual_pack_download(&db, "manual-fixture", &missing(&new_bytes), target)
            },
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&old).unwrap(), old_bytes);
        assert_eq!(std::fs::read(&receipt).unwrap(), original_receipt);
        assert_eq!(std::fs::read(&source).unwrap(), new_bytes);
        assert!(!dest.exists());
        let (tracked, path) = db.get_instance_mod("manual-fixture", "curseforge:123").unwrap().unwrap();
        assert_eq!(tracked.id, previous.id);
        assert_eq!(tracked.file_name, previous.file_name);
        assert_eq!(tracked.mod_name, previous.mod_name);
        assert_eq!(tracked.icon_url, previous.icon_url);
        assert_eq!(tracked.origin, previous.origin);
        assert_eq!(Path::new(&path), old);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn manual_pack_removal_failure_rolls_back_new_file_and_receipt() {
        use std::os::windows::fs::OpenOptionsExt;
        let downloads = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mods = root.path().join("mods");
        std::fs::create_dir(&mods).unwrap();
        let old_bytes = archive(b"old pack version");
        let new_bytes = archive(b"new pack version");
        let old = mods.join("old.jar");
        std::fs::write(&old, &old_bytes).unwrap();
        let receipt = pending_receipt(root.path(), &old_bytes, &new_bytes);
        let original_receipt = std::fs::read(&receipt).unwrap();
        let source = downloads.path().join("fixture.jar");
        std::fs::write(&source, &new_bytes).unwrap();
        let dest = mods.join("fixture.jar");
        // Allow receipt verification to read the retained file, but deny its
        // rename/removal after the new download has been published.
        let locked = std::fs::OpenOptions::new().read(true).share_mode(1).open(&old).unwrap();
        assert!(place_download(downloads.path(), &source, root.path(), &mods, &dest, &missing(&new_bytes), &CancelToken::new(), |_| Ok(())).is_err());
        drop(locked);
        assert_eq!(std::fs::read(old).unwrap(), old_bytes);
        assert_eq!(std::fs::read(receipt).unwrap(), original_receipt);
        assert_eq!(std::fs::read(source).unwrap(), new_bytes);
        assert!(!dest.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn already_placed_disabled_manual_download_stays_disabled() {
        let downloads = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let bytes = archive(b"disabled user version");
        let source = downloads.path().join("fixture.jar");
        let dest = root.path().join("fixture.jar");
        let disabled = root.path().join(format!("fixture.jar{}", crate::commands::content::DISABLED_SUFFIX));
        std::fs::write(&source, &bytes).unwrap();
        std::fs::write(&disabled, &bytes).unwrap();
        place_download(downloads.path(), &source, root.path(), root.path(), &dest, &missing(&bytes), &CancelToken::new(), |_| Ok(())).unwrap();
        assert_eq!(std::fs::read(disabled).unwrap(), bytes);
        assert!(!dest.exists());
        assert!(!source.exists());
    }

    #[test]
    fn watch_timeout_scales_with_mod_count_but_has_a_floor() {
        // A single mod (or the empty case) keeps the original flat window —
        // this must not get shorter than it used to be.
        assert_eq!(watch_timeout(1), MIN_WATCH_TIMEOUT);
        assert_eq!(watch_timeout(0), MIN_WATCH_TIMEOUT);
        // A big "Open all" batch gets real extra time instead of racing the
        // same flat 20 minutes regardless of how many pages there are.
        assert!(watch_timeout(11) > MIN_WATCH_TIMEOUT);
    }

    #[test]
    fn accepts_bare_and_subdomain_curseforge_hosts() {
        assert!(validate_curseforge_url("https://curseforge.com/minecraft/mc-mods/x/download/1").is_ok());
        assert!(validate_curseforge_url("https://www.curseforge.com/minecraft/mc-mods/x/download/1").is_ok());
        assert!(validate_curseforge_url("https://forums.curseforge.com/x").is_ok());
    }

    #[test]
    fn rejects_domain_spoofing_attempts() {
        assert!(validate_curseforge_url("https://www.curseforge.com.evil.com/x").is_err());
        assert!(validate_curseforge_url("https://curseforge.com.attacker.net/x").is_err());
        assert!(validate_curseforge_url("https://notcurseforge.com/x").is_err());
        assert!(validate_curseforge_url("https://evil.com/curseforge.com/x").is_err());
    }

    #[test]
    fn rejects_non_https_and_malformed_urls() {
        assert!(validate_curseforge_url("http://www.curseforge.com/x").is_err());
        assert!(validate_curseforge_url("not-a-url-at-all").is_err());
    }

    #[test]
    fn candidate_file_extension_filter() {
        assert!(is_candidate_file(Path::new("mod.jar")));
        assert!(is_candidate_file(Path::new("MOD.JAR")));
        assert!(is_candidate_file(Path::new("resourcepack.zip")));
        assert!(!is_candidate_file(Path::new("installer.exe")));
        assert!(!is_candidate_file(Path::new("screenshot.png")));
        assert!(!is_candidate_file(Path::new("no-extension")));
    }
}
