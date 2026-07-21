mod auth;
mod commands;
mod config;
mod db;
mod download;
mod dto;
mod identity;
mod instances;
mod launch;
mod modpack;
mod settings;
mod sources;
mod activity;

use commands::{
    apply_global_mc_options_to_all_instances, cancel_install, cancel_launch, clear_curseforge_api_key, create_instance,
    add_play_time, delete_instance, duplicate_instance, get_account, get_activity_logs, get_curseforge_status,
    get_content_meta, get_mod_summary_for_content, list_instance_content, list_mod_configs,
    read_config_file, remove_content_file, set_content_enabled, write_config_file,
    get_global_mc_options, get_instance_launch_config, get_instance_options, get_launch_settings,
    get_mod_details, get_modpack_content, get_version_changelog,
    import_curseforge_api_key_from_env_file, install_mod_to_instance, launch_instance,
    list_instance_mods, list_instances, list_java_runtimes, list_minecraft_versions,
    list_pending_missing_mods, logout,
    microsoft_login, open_all_missing_mods_browsers, open_missing_mods_browser, remove_mod_from_instance, rename_instance, save_global_mc_options,
    save_instance_options, search_mods, set_curseforge_api_key, set_instance_icon,
    set_instance_launch_config, set_launch_settings, test_curseforge_api_key,
    test_curseforge_docker_env_key, update_mod_in_instance, watch_for_missing_mods, AppState,
};
use config::ConfigStore;
use db::Database;
use sources::curseforge::CurseForgeClient;
use sources::modrinth::ModrinthClient;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let modrinth = ModrinthClient::new().expect("failed to initialize Modrinth client");
    let curseforge = CurseForgeClient::new().expect("failed to initialize CurseForge client");
    let config = ConfigStore::load().expect("failed to load app config");
    let db = Database::open().expect("failed to open library database");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
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
            get_curseforge_status,
            set_curseforge_api_key,
            clear_curseforge_api_key,
            test_curseforge_api_key,
            test_curseforge_docker_env_key,
            import_curseforge_api_key_from_env_file,
            list_instances,
            list_pending_missing_mods,
            create_instance,
            rename_instance,
            set_instance_icon,
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
            cancel_install,
            remove_mod_from_instance,
            list_minecraft_versions,
            get_mod_details,
            get_modpack_content,
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
            open_missing_mods_browser,
            open_all_missing_mods_browsers,
            watch_for_missing_mods,
            update_mod_in_instance,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
