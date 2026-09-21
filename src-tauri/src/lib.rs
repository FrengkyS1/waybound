mod auth;
mod commands;
mod config;
mod db;
mod download;
mod dto;
mod fingerprint;
mod identify;
mod identity;
mod loader_meta;
mod instances;
mod launch;
mod modpack;
mod settings;
mod sources;
mod activity;
mod transfer;

use commands::{
    detect_importable_launchers,
    apply_global_mc_options_to_all_instances, cancel_install, cancel_launch, check_launch_readiness,
    clear_curseforge_api_key, create_instance,
    add_play_time, delete_instance, dismiss_missing_mod, duplicate_instance, get_account, get_activity_logs, get_curseforge_status,
    get_content_meta, get_latest_loader_version, get_loader_version_info, get_mod_summary_for_content,
    identify_mod_file, list_instance_content, list_mod_configs,
    read_config_file, remove_content_file, set_content_enabled, write_config_file,
    get_global_mc_options, get_instance_launch_config, get_instance_options, get_launch_settings,
    get_mod_details, get_modpack_content, get_modpack_detail_for_instance, get_running_instances,
    get_version_changelog,
    import_curseforge_api_key_from_env_file, install_mod_to_instance, launch_instance,
    list_instance_mods, list_instances, list_java_runtimes, list_minecraft_versions,
    list_pending_missing_mods, logout, read_launch_log,
    microsoft_login, open_all_missing_mods_browsers, open_in_file_manager, open_missing_mods_browser, remove_mod_from_instance, rename_instance, save_global_mc_options,
    save_instance_options, search_mods, set_curseforge_api_key, set_instance_icon,
    set_instance_launch_config, set_instance_loader_version, set_launch_settings, test_curseforge_api_key,
    test_curseforge_docker_env_key, update_mod_in_instance, watch_for_missing_mods, AppState,
    import_instance, export_instance, pause_install, resume_install,
};
use config::ConfigStore;
use db::Database;
use sources::curseforge::CurseForgeClient;
use sources::modrinth::ModrinthClient;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Listener, Manager, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let modrinth = ModrinthClient::new().expect("failed to initialize Modrinth client");
    let curseforge = CurseForgeClient::new().expect("failed to initialize CurseForge client");
    let config = ConfigStore::load().expect("failed to load app config");
    let db = Database::open().expect("failed to open library database");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let show_item = MenuItem::with_id(app, "show", "Show Waybound", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Waybound")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            // The main window ships hidden (`visible: false` in
            // tauri.conf.json) so it only appears once the webview has fully
            // painted its first real frame — no white flash during WebView2
            // init + bundle parse, and no half-rendered window to alt-tab
            // into during startup. The frontend emits `waybound://ready`
            // after its first painted frame; this timer is the safety net if
            // it never does (broken bundle), so the app can't end up an
            // invisible process.
            let shown = Arc::new(AtomicBool::new(false));
            let handle = app.handle().clone();
            let flag = shown.clone();
            app.listen_any("waybound://ready", move |_| {
                if flag.swap(true, Ordering::SeqCst) {
                    return;
                }
                if let Some(window) = handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            });
            let handle = app.handle().clone();
            let flag = shown.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(15));
                if !flag.load(Ordering::SeqCst) {
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.show();
                    }
                }
            });

            Ok(())
        })
        // Closing the MAIN window sends the app to the tray instead of quitting —
        // the tray icon's own "Quit" menu item is the only way to actually
        // exit, so a launched Minecraft process's parent app stays reachable
        // (and the running-instance tracking in `AppState` isn't lost) while
        // the window is just hidden, not the process torn down.
        //
        // This handler is registered on the builder, so it fires for EVERY
        // window. Every other window — the CurseForge download pages from
        // `missing_mods.rs` and the login window — must genuinely close.
        // Hiding one leaves its WebView2 renderers alive and the page's
        // JavaScript running forever: one X'd download page was measured
        // holding 371 MB across twelve renderer processes and still burning
        // CPU eighteen minutes later.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    window.hide().ok();
                    api.prevent_close();
                }
            }
        })
        .manage(AppState {
            modrinth,
            curseforge,
            config,
            db,
            installs: std::sync::Mutex::new(std::collections::HashMap::new()),
            launches: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
        .invoke_handler(tauri::generate_handler![
            search_mods,
            detect_importable_launchers,
            get_curseforge_status,
            set_curseforge_api_key,
            clear_curseforge_api_key,
            test_curseforge_api_key,
            test_curseforge_docker_env_key,
            import_curseforge_api_key_from_env_file,
            list_instances,
            list_pending_missing_mods,
            dismiss_missing_mod,
            create_instance,
            rename_instance,
            set_instance_icon,
            get_latest_loader_version,
            get_loader_version_info,
            set_instance_loader_version,
            list_instance_content,
            get_content_meta,
            get_mod_summary_for_content,
            list_mod_configs,
            read_config_file,
            write_config_file,
            set_content_enabled,
            remove_content_file,
            delete_instance,
            duplicate_instance,
            list_instance_mods,
            install_mod_to_instance,
            import_instance,
            export_instance,
            pause_install,
            resume_install,
            cancel_install,
            remove_mod_from_instance,
            list_minecraft_versions,
            get_mod_details,
            get_modpack_content,
            get_modpack_detail_for_instance,
            identify_mod_file,
            get_version_changelog,
            get_activity_logs,
            get_instance_options,
            save_instance_options,
            get_global_mc_options,
            save_global_mc_options,
            apply_global_mc_options_to_all_instances,
            get_account,
            microsoft_login,
            logout,
            list_java_runtimes,
            get_launch_settings,
            set_launch_settings,
            get_instance_launch_config,
            set_instance_launch_config,
            add_play_time,
            launch_instance,
            cancel_launch,
            check_launch_readiness,
            get_running_instances,
            read_launch_log,
            open_missing_mods_browser,
            open_all_missing_mods_browsers,
            open_in_file_manager,
            watch_for_missing_mods,
            update_mod_in_instance,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
