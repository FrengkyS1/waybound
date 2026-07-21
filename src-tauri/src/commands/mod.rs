pub mod auth;
pub mod config;
pub mod content;
pub mod instances;
pub mod launch;
pub mod missing_mods;
pub mod project;
pub mod search;
pub mod settings;

pub use auth::{get_account, logout, microsoft_login};
pub use content::{
    get_content_meta, list_instance_content, list_mod_configs, read_config_file,
    remove_content_file, set_content_enabled, write_config_file,
};
pub use config::{
    clear_curseforge_api_key, get_curseforge_status, import_curseforge_api_key_from_env_file,
    set_curseforge_api_key, test_curseforge_api_key, test_curseforge_docker_env_key,
};
pub use launch::{
    add_play_time, cancel_launch, get_instance_launch_config, get_launch_settings,
    launch_instance, list_java_runtimes, set_instance_launch_config, set_launch_settings,
};
pub use instances::{
    cancel_install, create_instance, delete_instance, duplicate_instance, get_mod_summary_for_content,
    install_mod_to_instance, list_instance_mods, list_instances, list_minecraft_versions,
    list_pending_missing_mods, remove_mod_from_instance, rename_instance, set_instance_icon,
    update_mod_in_instance,
};
pub use missing_mods::{open_all_missing_mods_browsers, open_missing_mods_browser, watch_for_missing_mods};
pub use project::{
    get_activity_logs, get_mod_details, get_modpack_content, get_version_changelog,
};
pub use settings::{
    apply_global_mc_options_to_all_instances, get_global_mc_options, get_instance_options,
    save_global_mc_options, save_instance_options,
};
pub use search::{search_mods, AppState};
