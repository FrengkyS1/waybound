//! Support for the "download missing mods" flow: mods a CurseForge author
//! blocked from third-party/API download. The user still has to click
//! "Download" on the mod's own page themselves (that's the point — it's the
//! manual step the author's restriction asks for), but Waybound opens that
//! page in its own sandboxed window instead of the system browser, and
//! auto-places whatever lands in Downloads into the right instance folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindowBuilder};

use crate::dto::instance::MissingMod;
use crate::instances::paths::instance_root;
use crate::launch::files::file_sha1;

const BROWSER_WINDOW_LABEL: &str = "missing-mods-browser";
const LOGIN_WINDOW_LABEL: &str = "curseforge-login";
const WATCH_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Checks (and immediately flips) the one-time "prompt CurseForge login"
/// flag. Pure config read/write — safe to call synchronously from the
/// command's dispatcher thread, unlike the actual window creation below.
fn should_prompt_curseforge_login(config: &crate::config::ConfigStore) -> bool {
    if config.curseforge_login_prompted() {
        return false;
    }
    let _ = config.mark_curseforge_login_prompted();
    true
}

/// Opens a separate window at CurseForge's own login page alongside whatever
/// mod page was actually requested — a session isn't required to download
/// (an incognito tab works fine), but logging in once avoids CurseForge's
/// own login/SSO interstitials surprising the user mid-download later.
/// Skippable: the user can just ignore or close this window. Must run on the
/// spawned task like every other window creation in this file — WebView2
/// window creation isn't safe to call directly from a command's dispatcher
/// thread.
fn open_curseforge_login_window(app: &AppHandle) {
    let Ok(url) = Url::parse("https://www.curseforge.com/account/login") else {
        return;
    };
    if let Err(err) = WebviewWindowBuilder::new(app, LOGIN_WINDOW_LABEL, WebviewUrl::External(url))
        .title("Log in to CurseForge (optional) \u{2014} Waybound")
        .inner_size(900.0, 700.0)
        .build()
    {
        crate::activity::append_log(&format!("Could not open CurseForge login window: {err}"), "warn", None);
    }
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
pub fn open_missing_mods_browser(
    app: AppHandle,
    state: tauri::State<'_, super::search::AppState>,
    url: String,
) -> Result<(), String> {
    let parsed = validate_curseforge_url(&url)?;
    let prompt_login = should_prompt_curseforge_login(&state.config);

    tauri::async_runtime::spawn(async move {
        if prompt_login {
            open_curseforge_login_window(&app);
        }
        if let Some(window) = app.get_webview_window(BROWSER_WINDOW_LABEL) {
            if let Err(err) = window.navigate(parsed) {
                crate::activity::append_log(&format!("Could not navigate missing-mods browser: {err}"), "warn", None);
                return;
            }
            let _ = window.set_focus();
        } else if let Err(err) = WebviewWindowBuilder::new(&app, BROWSER_WINDOW_LABEL, WebviewUrl::External(parsed))
            .title("Download mod \u{2014} Waybound")
            .inner_size(1000.0, 800.0)
            .build()
        {
            crate::activity::append_log(&format!("Could not open missing-mods browser: {err}"), "warn", None);
        }
    });

    Ok(())
}

/// Opens every missing mod's page at once, each in its own sandboxed window
/// (same restriction and zero-capability sandboxing as the single-window
/// flow above), cascaded so they don't stack exactly on top of each other.
/// Lets the user work through "click Download" on each page without
/// round-tripping to the app's prev/next stepper between every one.
#[tauri::command]
pub fn open_all_missing_mods_browsers(
    app: AppHandle,
    state: tauri::State<'_, super::search::AppState>,
    urls: Vec<String>,
) -> Result<(), String> {
    let parsed: Vec<Url> = urls
        .iter()
        .map(|u| validate_curseforge_url(u))
        .collect::<Result<_, _>>()?;
    let prompt_login = should_prompt_curseforge_login(&state.config);

    tauri::async_runtime::spawn(async move {
        if prompt_login {
            open_curseforge_login_window(&app);
        }
        for (i, url) in parsed.into_iter().enumerate() {
            let label = format!("{BROWSER_WINDOW_LABEL}-{i}");
            let offset = (i as f64) * 30.0;
            if let Some(window) = app.get_webview_window(&label) {
                if let Err(err) = window.navigate(url) {
                    crate::activity::append_log(&format!("Could not navigate missing-mods browser: {err}"), "warn", None);
                    continue;
                }
                let _ = window.set_focus();
            } else if let Err(err) = WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(url))
                .title("Download mod \u{2014} Waybound")
                .inner_size(1000.0, 800.0)
                .position(80.0 + offset, 80.0 + offset)
                .build()
            {
                crate::activity::append_log(&format!("Could not open missing-mods browser: {err}"), "warn", None);
            }
        }
    });

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

/// Closes whichever window was showing this mod's page, now that its file
/// has landed — there's nothing left to do on that page. Closes the
/// per-index window from `open_all_missing_mods_browsers` unconditionally
/// (it only ever shows this one mod), and the single shared stepper window
/// only if it's still pointed at this exact mod's URL — it's reused across
/// mods, so closing it unconditionally could yank the page out from under
/// the user mid-step on a different one.
fn close_missing_mods_window(app: &AppHandle, original_index: usize, url: &str) {
    let indexed_label = format!("{BROWSER_WINDOW_LABEL}-{original_index}");
    if let Some(window) = app.get_webview_window(&indexed_label) {
        let _ = window.close();
    }
    if let Some(window) = app.get_webview_window(BROWSER_WINDOW_LABEL) {
        if window.url().map(|u| u.as_str() == url).unwrap_or(false) {
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
#[tauri::command]
pub fn watch_for_missing_mods(app: AppHandle, instance_id: String, mods: Vec<MissingMod>) -> Result<(), String> {
    let root = instance_root(&instance_id).map_err(|e| e.to_string())?;
    let mods_dir = root.join("mods");
    let resourcepacks_dir = root.join("resourcepacks");
    std::fs::create_dir_all(&mods_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&resourcepacks_dir).map_err(|e| e.to_string())?;

    let Some(downloads_dir) = dirs::download_dir() else {
        return Err("Could not locate your Downloads folder.".to_string());
    };

    tauri::async_runtime::spawn(async move {
        // Original index travels with each mod (not just its position in
        // `remaining`, which shifts on every removal) — it's the same index
        // `open_all_missing_mods_browsers` used for that mod's window label,
        // so a placement can close the one window that's done its job.
        let mut remaining: Vec<(usize, MissingMod)> = mods.into_iter().enumerate().collect();
        let total = remaining.len() as u32;
        let mut placed_names = Vec::new();
        let deadline = Instant::now() + WATCH_TIMEOUT;
        // path -> (mtime, sha1) — a file already hashed and not matched
        // doesn't need re-hashing every second until it changes; this is
        // what keeps a large unrelated file in Downloads from being read
        // and hashed once per poll tick for the whole watch window.
        let mut hash_cache: HashMap<PathBuf, (SystemTime, String)> = HashMap::new();

        while !remaining.is_empty() && Instant::now() < deadline {
            let Ok(entries) = std::fs::read_dir(&downloads_dir) else {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            };

            for entry in entries.flatten() {
                if remaining.is_empty() {
                    break;
                }
                let source = entry.path();
                if !source.is_file() || !is_candidate_file(&source) {
                    continue;
                }

                let matched_index = remaining.iter().position(|(_, m)| {
                    match &m.sha1 {
                        Some(expected) => {
                            let mtime = entry.metadata().and_then(|md| md.modified()).ok();
                            let sha1 = match (mtime, hash_cache.get(&source)) {
                                (Some(mtime), Some((cached_mtime, cached_sha1))) if mtime == *cached_mtime => {
                                    Some(cached_sha1.clone())
                                }
                                _ => {
                                    let sha1 = file_sha1(&source);
                                    if let (Some(mtime), Some(sha1)) = (mtime, &sha1) {
                                        hash_cache.insert(source.clone(), (mtime, sha1.clone()));
                                    }
                                    sha1
                                }
                            };
                            sha1.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(expected))
                        }
                        // No hash reported for this file (rare/old CurseForge
                        // entries) — fall back to the exact filename match.
                        None => source.file_name().and_then(|n| n.to_str()) == Some(m.filename.as_str()),
                    }
                });

                let Some(idx) = matched_index else { continue };
                let is_jar = remaining[idx].1.filename.to_ascii_lowercase().ends_with(".jar");
                let dest_dir = if is_jar { &mods_dir } else { &resourcepacks_dir };
                // `filename` ultimately comes from CurseForge's API — never
                // trust it as a bare path component. `safe_join` rejects any
                // `..`/root/prefix component, same as every other write site
                // in this codebase (`modpack/curseforge.rs`'s `safe_join`
                // calls), so a crafted `fileName` can't redirect this write
                // outside the instance's mods/resourcepacks folder.
                let Ok(dest) = crate::download::safe_join(dest_dir, &remaining[idx].1.filename) else {
                    continue;
                };

                // A same-volume rename is the common case. Falling back to
                // copy+delete (needed if Downloads and the instance folder
                // are on different volumes) counts as placed on the copy
                // alone — the destination file is what actually matters, and
                // gating "placed" on the source cleanup too meant a
                // transient lock on the just-downloaded file (a real
                // possibility on Windows: AV scan, indexer) left a correctly
                // placed mod reported as still missing.
                let placed = std::fs::rename(&source, &dest).is_ok() || {
                    let copied = std::fs::copy(&source, &dest).is_ok();
                    if copied {
                        let _ = std::fs::remove_file(&source);
                    }
                    copied
                };

                if placed {
                    let (original_index, entry) = remaining.remove(idx);
                    hash_cache.remove(&source);
                    close_missing_mods_window(&app, original_index, &entry.url);
                    placed_names.push(entry.name.clone());
                    let _ = app.emit(
                        "missing-mods://placed",
                        MissingModPlacedEvent {
                            instance_id: instance_id.clone(),
                            name: entry.name,
                            remaining: remaining.len() as u32,
                            total,
                        },
                    );
                }
            }

            if remaining.is_empty() {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        let _ = app.emit(
            "missing-mods://done",
            MissingModsWatchDoneEvent {
                instance_id,
                placed: placed_names,
                still_missing: remaining.into_iter().map(|(_, m)| m.name).collect(),
            },
        );
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_candidate_file, validate_curseforge_url};
    use std::path::Path;

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
