use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum McOptionsError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsMode {
    Fast,
    Fancy,
    Fabulous,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NarratorMode {
    Off,
    All,
    Chat,
    System,
}

// `#[serde(default)]` at the struct level, not per-field, and it is load-
// bearing: this type is persisted inside `config.toml`, so every field added
// later is absent from every config written before it. Without this, adding
// one field makes the whole file fail to deserialize, and `ConfigStore::load`
// reasonably treats an unparseable config as corrupt — renaming it to `.bak`
// and starting fresh. That silently destroyed a real user's saved account and
// launch settings when the accessibility/comfort options were added. Missing
// keys must fall back to their default instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct McOptions {
    pub customize: bool,
    pub fullscreen: bool,
    pub view_bobbing: bool,
    pub gui_scale: i32,
    pub gamma: i32,
    pub render_distance: i32,
    pub simulation_distance: i32,
    pub fov: i32,
    pub entity_shadows: bool,
    pub vsync: bool,
    pub max_fps: i32,
    pub graphics_mode: GraphicsMode,
    pub clouds: CloudsMode,
    pub particles: ParticleMode,
    pub mipmap_levels: i32,
    pub entity_distance_scaling: i32,
    pub biome_blend_radius: i32,
    pub ao: bool,
    // 0-1 fractions in options.txt, stored here as 0-100 ints (see `gamma`).
    pub screen_effect_scale: i32,
    pub fov_effect_scale: i32,
    pub darkness_effect_scale: i32,
    pub auto_jump: bool,
    /// Maps to `invertYMouse`.
    pub invert_mouse: bool,
    pub invert_x_mouse: bool,
    pub mouse_sensitivity: i32,
    pub raw_mouse_input: bool,
    pub discrete_mouse_scroll: bool,
    pub toggle_sprint: bool,
    pub toggle_crouch: bool,
    pub show_subtitles: bool,
    pub reduced_debug_info: bool,
    pub high_contrast: bool,
    pub directional_audio: bool,
    pub hide_lightning_flashes: bool,
    pub force_unicode_font: bool,
    pub damage_tilt_strength: i32,
    pub narrator: NarratorMode,
    pub language: String,
    pub master_volume: i32,
    pub music_volume: i32,
    pub jukebox_volume: i32,
    pub weather_volume: i32,
    pub blocks_volume: i32,
    pub hostile_volume: i32,
    pub neutral_volume: i32,
    pub player_volume: i32,
    pub ambient_volume: i32,
    pub voice_volume: i32,
    pub ui_volume: i32,
    // No field-level `#[serde(default)]` here on purpose: it would shadow the
    // struct-level one above with `HashMap::default()`, i.e. an *empty* map,
    // so a config omitting this key would silently come back with no key
    // bindings at all instead of the vanilla set.
    pub key_bindings: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CloudsMode {
    Off,
    Fast,
    Fancy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ParticleMode {
    Minimal,
    Decreased,
    All,
}

impl Default for McOptions {
    fn default() -> Self {
        Self {
            customize: true,
            fullscreen: false,
            view_bobbing: true,
            gui_scale: 2,
            gamma: 50,
            render_distance: 12,
            simulation_distance: 12,
            fov: 70,
            entity_shadows: true,
            vsync: true,
            max_fps: 260,
            graphics_mode: GraphicsMode::Fancy,
            clouds: CloudsMode::Fancy,
            particles: ParticleMode::All,
            mipmap_levels: 4,
            entity_distance_scaling: 100,
            biome_blend_radius: 2,
            ao: true,
            screen_effect_scale: 100,
            fov_effect_scale: 100,
            darkness_effect_scale: 100,
            auto_jump: false,
            invert_mouse: false,
            invert_x_mouse: false,
            mouse_sensitivity: 50,
            raw_mouse_input: true,
            discrete_mouse_scroll: false,
            toggle_sprint: false,
            toggle_crouch: false,
            show_subtitles: false,
            reduced_debug_info: false,
            high_contrast: false,
            directional_audio: false,
            hide_lightning_flashes: false,
            force_unicode_font: false,
            damage_tilt_strength: 100,
            narrator: NarratorMode::Off,
            language: "en_us".to_string(),
            master_volume: 100,
            music_volume: 100,
            jukebox_volume: 100,
            weather_volume: 100,
            blocks_volume: 100,
            hostile_volume: 100,
            neutral_volume: 100,
            player_volume: 100,
            ambient_volume: 100,
            voice_volume: 100,
            ui_volume: 100,
            key_bindings: default_key_bindings(),
        }
    }
}

fn default_key_bindings() -> HashMap<String, String> {
    [
        ("attack", "key.mouse.left"),
        ("use", "key.mouse.right"),
        ("forward", "key.keyboard.w"),
        ("left", "key.keyboard.a"),
        ("back", "key.keyboard.s"),
        ("right", "key.keyboard.d"),
        ("jump", "key.keyboard.space"),
        ("sneak", "key.keyboard.left.shift"),
        ("sprint", "key.keyboard.left.control"),
        ("drop", "key.keyboard.q"),
        ("inventory", "key.keyboard.e"),
        ("chat", "key.keyboard.t"),
        ("playerlist", "key.keyboard.tab"),
        ("pickItem", "key.mouse.middle"),
        ("command", "key.keyboard.slash"),
        ("socialInteractions", "key.keyboard.p"),
        ("screenshot", "key.keyboard.f2"),
        ("togglePerspective", "key.keyboard.f5"),
        ("smoothCamera", "key.keyboard.unknown"),
        ("fullscreen", "key.keyboard.f11"),
        ("spectatorOutlines", "key.keyboard.unknown"),
        ("swapOffhand", "key.keyboard.f"),
        ("saveToolbarActivator", "key.keyboard.c"),
        ("loadToolbarActivator", "key.keyboard.x"),
        ("advancements", "key.keyboard.l"),
        ("hotbar.1", "key.keyboard.1"),
        ("hotbar.2", "key.keyboard.2"),
        ("hotbar.3", "key.keyboard.3"),
        ("hotbar.4", "key.keyboard.4"),
        ("hotbar.5", "key.keyboard.5"),
        ("hotbar.6", "key.keyboard.6"),
        ("hotbar.7", "key.keyboard.7"),
        ("hotbar.8", "key.keyboard.8"),
        ("hotbar.9", "key.keyboard.9"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect()
}

pub fn options_path(instance_root: &Path) -> std::path::PathBuf {
    instance_root.join("options.txt")
}

pub fn read_options(instance_root: &Path) -> Result<McOptions, McOptionsError> {
    let path = options_path(instance_root);
    if !path.exists() {
        return Ok(McOptions::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(parse_options(&content))
}

pub fn write_options(instance_root: &Path, options: &McOptions) -> Result<(), McOptionsError> {
    if !options.customize {
        return Ok(());
    }
    let path = options_path(instance_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(path, merge_options(&existing, options))?;
    Ok(())
}

// Vanilla treats an options.txt with no `version:` marker as pre-1.13 and
// runs datafixers that expect numeric key codes, so modern names like
// "key.keyboard.d" throw NumberFormatException and the game discards every
// setting written here. Waybound always writes the modern format, so a file
// it creates is stamped with a data version above any real game's: the
// datafixer then leaves the file untouched, and Minecraft writes back its
// own real data version on next save.
const OPTIONS_DATA_VERSION: i32 = 99_999_999;

/// Merges Waybound-managed settings into an existing options.txt instead of
/// overwriting it outright. Modpacks ship their own options.txt (via
/// overrides) with mod-specific keybinds (e.g. "key_key.confluence.hook")
/// and settings Waybound doesn't model (e.g. "resourcePacks"). A blind
/// overwrite would silently delete those. Only the fixed set of keys
/// serialize_options knows about, plus the fixed vanilla keybind names, are
/// replaced; every other line from the existing file passes through as-is.
fn merge_options(existing: &str, options: &McOptions) -> String {
    let managed_lines = serialize_options(options);
    let mut managed: HashMap<String, String> = HashMap::new();
    for line in managed_lines.lines() {
        if let Some((key, _)) = line.split_once(':') {
            managed.insert(key.to_string(), line.to_string());
        }
    }
    let mut output = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            output.push(line.to_string());
            continue;
        }
        let Some((key, _)) = trimmed.split_once(':') else {
            output.push(line.to_string());
            continue;
        };
        if let Some(binding_name) = key.strip_prefix("key_key.") {
            // Any keybind the incoming options carry (vanilla or
            // mod-added) replaces the file's line and marks it seen; one
            // without an incoming value is kept as-is. (The vanilla-name
            // check that used to gate this meant mod-added binds were kept
            // WITHOUT marking seen, so the tail loops re-emitted them —
            // and on a fresh file, where nothing is ever marked seen, the
            // two tail loops each emitted every binding, doubling the
            // whole key block and crashing NeoForge's strict early-display
            // key parser with "Duplicate key".)
            if let Some(value) = options.key_bindings.get(binding_name) {
                output.push(format!("key_key.{binding_name}:{value}"));
                seen.insert(key.to_string());
            } else {
                output.push(line.to_string());
            }
            continue;
        }
        if let Some(managed_line) = managed.get(key) {
            output.push(managed_line.clone());
            seen.insert(key.to_string());
        } else {
            // Setting Waybound doesn't model (resourcePacks, lastServer, ...): keep as-is.
            output.push(line.to_string());
        }
    }

    for line in managed_lines.lines() {
        if let Some((key, _)) = line.split_once(':') {
            if !seen.contains(key) {
                output.push(line.to_string());
            }
        }
    }
    // NOTE: no second pass over options.key_bindings here — serialize_options
    // already emits every binding into managed_lines above, so a separate
    // bindings loop re-emits the whole key block on any file where the
    // first loop marked nothing seen (i.e. every fresh instance).

    // An existing version line (e.g. from modpack overrides) passed through
    // above; only a file without one gets the stamp.
    let has_version = output
        .iter()
        .any(|line| line.trim().split_once(':').is_some_and(|(k, _)| k == "version"));
    if !has_version {
        output.insert(0, format!("version:{OPTIONS_DATA_VERSION}"));
    }

    output.join("\n")
}

fn parse_options(content: &str) -> McOptions {
    let mut map = HashMap::new();
    let mut parsed_bindings = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if key.starts_with("key_key.") {
                parsed_bindings.insert(key.trim_start_matches("key_key.").to_string(), value);
            } else {
                map.insert(key, value);
            }
        }
    }

    let mut options = McOptions::default();
    options.customize = true;
    options.fullscreen = parse_bool(map.get("fullscreen"), options.fullscreen);
    options.view_bobbing = parse_bool(map.get("bobView"), options.view_bobbing);
    options.gui_scale = parse_i32(map.get("guiScale"), options.gui_scale);
    options.gamma = (parse_f64(map.get("gamma"), options.gamma as f64 / 100.0) * 100.0) as i32;
    options.render_distance = parse_i32(map.get("renderDistance"), options.render_distance);
    options.simulation_distance =
        parse_i32(map.get("simulationDistance"), options.simulation_distance);
    options.fov = fov_from_file(parse_f64(map.get("fov"), 0.0));
    options.entity_shadows = parse_bool(map.get("entityShadows"), options.entity_shadows);
    options.vsync = parse_bool(map.get("enableVsync"), options.vsync);
    options.max_fps = parse_i32(map.get("maxFps"), options.max_fps);
    options.graphics_mode = match parse_i32(map.get("graphicsMode"), 1) {
        0 => GraphicsMode::Fast,
        2 => GraphicsMode::Fabulous,
        _ => GraphicsMode::Fancy,
    };
    options.clouds = match map.get("renderClouds").map(String::as_str) {
        Some("false") => CloudsMode::Off,
        Some("fast") => CloudsMode::Fast,
        _ => CloudsMode::Fancy,
    };
    options.particles = match map.get("particles").map(String::as_str) {
        Some("1") => ParticleMode::Decreased,
        Some("2") => ParticleMode::Minimal,
        _ => ParticleMode::All,
    };
    options.auto_jump = parse_bool(map.get("autoJump"), options.auto_jump);
    options.invert_mouse = parse_bool(map.get("invertYMouse"), options.invert_mouse);
    options.invert_x_mouse = parse_bool(map.get("invertXMouse"), options.invert_x_mouse);
    // Vanilla stores sensitivity as a 0.0-1.0 fraction where displayed
    // percent = value * 200 (0.5 -> 100%, 1.0 -> 200%), not value * 100.
    options.mouse_sensitivity =
        (parse_f64(map.get("mouseSensitivity"), options.mouse_sensitivity as f64 / 200.0) * 200.0)
            as i32;
    options.toggle_sprint = parse_bool(map.get("toggleSprint"), options.toggle_sprint);
    options.toggle_crouch = parse_bool(map.get("toggleCrouch"), options.toggle_crouch);
    options.show_subtitles = parse_bool(map.get("showSubtitles"), options.show_subtitles);
    if let Some(lang) = map.get("lang") {
        options.language = lang.clone();
    }
    options.master_volume = percent_from_file(map.get("soundCategory_master"), options.master_volume);
    options.music_volume = percent_from_file(map.get("soundCategory_music"), options.music_volume);
    options.jukebox_volume = percent_from_file(map.get("soundCategory_record"), options.jukebox_volume);
    options.weather_volume = percent_from_file(map.get("soundCategory_weather"), options.weather_volume);
    options.blocks_volume = percent_from_file(map.get("soundCategory_block"), options.blocks_volume);
    options.hostile_volume = percent_from_file(map.get("soundCategory_hostile"), options.hostile_volume);
    options.neutral_volume = percent_from_file(map.get("soundCategory_neutral"), options.neutral_volume);
    options.player_volume = percent_from_file(map.get("soundCategory_player"), options.player_volume);
    options.ambient_volume = percent_from_file(map.get("soundCategory_ambient"), options.ambient_volume);
    options.voice_volume = percent_from_file(map.get("soundCategory_voice"), options.voice_volume);
    options.ui_volume = percent_from_file(map.get("soundCategory_ui"), options.ui_volume);
    options.mipmap_levels = parse_i32(map.get("mipmapLevels"), options.mipmap_levels);
    options.entity_distance_scaling = (parse_f64(
        map.get("entityDistanceScaling"),
        options.entity_distance_scaling as f64 / 100.0,
    ) * 100.0) as i32;
    options.biome_blend_radius = parse_i32(map.get("biomeBlendRadius"), options.biome_blend_radius);
    options.raw_mouse_input = parse_bool(map.get("rawMouseInput"), options.raw_mouse_input);
    options.discrete_mouse_scroll =
        parse_bool(map.get("discreteMouseScroll"), options.discrete_mouse_scroll);
    options.reduced_debug_info = parse_bool(map.get("reducedDebugInfo"), options.reduced_debug_info);
    options.high_contrast = parse_bool(map.get("highContrast"), options.high_contrast);
    options.directional_audio = parse_bool(map.get("directionalAudio"), options.directional_audio);
    options.hide_lightning_flashes =
        parse_bool(map.get("hideLightningFlashes"), options.hide_lightning_flashes);
    options.force_unicode_font = parse_bool(map.get("forceUnicodeFont"), options.force_unicode_font);
    options.damage_tilt_strength =
        percent_from_file(map.get("damageTiltStrength"), options.damage_tilt_strength);
    options.ao = parse_bool(map.get("ao"), options.ao);
    options.screen_effect_scale =
        percent_from_file(map.get("screenEffectScale"), options.screen_effect_scale);
    options.fov_effect_scale = percent_from_file(map.get("fovEffectScale"), options.fov_effect_scale);
    options.darkness_effect_scale =
        percent_from_file(map.get("darknessEffectScale"), options.darkness_effect_scale);
    options.narrator = match parse_i32(map.get("narrator"), 0) {
        1 => NarratorMode::All,
        2 => NarratorMode::Chat,
        3 => NarratorMode::System,
        _ => NarratorMode::Off,
    };
    if !parsed_bindings.is_empty() {
        for (key, value) in parsed_bindings {
            options.key_bindings.insert(key, value);
        }
    }
    let defaults = default_key_bindings();
    for (key, value) in defaults {
        options.key_bindings.entry(key).or_insert(value);
    }
    options
}

fn serialize_options(options: &McOptions) -> String {
    let mut lines = vec![
        format!("fullscreen:{}", options.fullscreen),
        format!("bobView:{}", options.view_bobbing),
        format!("guiScale:{}", options.gui_scale),
        format!("gamma:{:.2}", options.gamma as f64 / 100.0),
        format!("renderDistance:{}", options.render_distance),
        format!("simulationDistance:{}", options.simulation_distance),
        format!("fov:{:.1}", fov_to_file(options.fov)),
        format!("entityShadows:{}", options.entity_shadows),
        format!("enableVsync:{}", options.vsync),
        format!("maxFps:{}", options.max_fps),
        format!(
            "graphicsMode:{}",
            match options.graphics_mode {
                GraphicsMode::Fast => 0,
                GraphicsMode::Fancy => 1,
                GraphicsMode::Fabulous => 2,
            }
        ),
        format!(
            "renderClouds:{}",
            match options.clouds {
                CloudsMode::Off => "false",
                CloudsMode::Fast => "fast",
                CloudsMode::Fancy => "true",
            }
        ),
        format!(
            "particles:{}",
            match options.particles {
                ParticleMode::All => "0",
                ParticleMode::Decreased => "1",
                ParticleMode::Minimal => "2",
            }
        ),
        format!("autoJump:{}", options.auto_jump),
        format!("invertYMouse:{}", options.invert_mouse),
        format!("invertXMouse:{}", options.invert_x_mouse),
        format!("mouseSensitivity:{:.2}", options.mouse_sensitivity as f64 / 200.0),
        format!("toggleSprint:{}", options.toggle_sprint),
        format!("toggleCrouch:{}", options.toggle_crouch),
        format!("showSubtitles:{}", options.show_subtitles),
        format!("lang:{}", options.language),
        format!(
            "soundCategory_master:{:.2}",
            options.master_volume as f64 / 100.0
        ),
        format!("soundCategory_music:{:.2}", options.music_volume as f64 / 100.0),
        format!("soundCategory_record:{:.2}", options.jukebox_volume as f64 / 100.0),
        format!("soundCategory_weather:{:.2}", options.weather_volume as f64 / 100.0),
        format!("soundCategory_block:{:.2}", options.blocks_volume as f64 / 100.0),
        format!(
            "soundCategory_hostile:{:.2}",
            options.hostile_volume as f64 / 100.0
        ),
        format!(
            "soundCategory_neutral:{:.2}",
            options.neutral_volume as f64 / 100.0
        ),
        format!("soundCategory_player:{:.2}", options.player_volume as f64 / 100.0),
        format!("soundCategory_ambient:{:.2}", options.ambient_volume as f64 / 100.0),
        format!("soundCategory_voice:{:.2}", options.voice_volume as f64 / 100.0),
        format!("soundCategory_ui:{:.2}", options.ui_volume as f64 / 100.0),
        format!("mipmapLevels:{}", options.mipmap_levels),
        format!(
            "entityDistanceScaling:{:.2}",
            options.entity_distance_scaling as f64 / 100.0
        ),
        format!("biomeBlendRadius:{}", options.biome_blend_radius),
        format!("rawMouseInput:{}", options.raw_mouse_input),
        format!("discreteMouseScroll:{}", options.discrete_mouse_scroll),
        format!("reducedDebugInfo:{}", options.reduced_debug_info),
        format!("highContrast:{}", options.high_contrast),
        format!("directionalAudio:{}", options.directional_audio),
        format!("hideLightningFlashes:{}", options.hide_lightning_flashes),
        format!("forceUnicodeFont:{}", options.force_unicode_font),
        format!(
            "damageTiltStrength:{:.2}",
            options.damage_tilt_strength as f64 / 100.0
        ),
        format!("ao:{}", options.ao),
        format!(
            "screenEffectScale:{:.2}",
            options.screen_effect_scale as f64 / 100.0
        ),
        format!("fovEffectScale:{:.2}", options.fov_effect_scale as f64 / 100.0),
        format!(
            "darknessEffectScale:{:.2}",
            options.darkness_effect_scale as f64 / 100.0
        ),
        format!(
            "narrator:{}",
            match options.narrator {
                NarratorMode::Off => 0,
                NarratorMode::All => 1,
                NarratorMode::Chat => 2,
                NarratorMode::System => 3,
            }
        ),
        // Vanilla shows its "Welcome to Minecraft! Would you like to enable
        // Narrator..." first-run prompt on any options.txt that's missing
        // this flag — which is every instance Waybound creates, since it
        // never wrote it. Not a real user preference (nobody wants to see
        // this on purpose), so it's just always marked dismissed.
        "onboardAccessibility:false".to_string(),
    ];
    let mut binding_keys: Vec<_> = options.key_bindings.keys().collect();
    binding_keys.sort();
    for key in binding_keys {
        if let Some(value) = options.key_bindings.get(key) {
            lines.push(format!("key_key.{key}:{value}"));
        }
    }
    lines.join("\n")
}

fn parse_bool(value: Option<&String>, default: bool) -> bool {
    value.map(|v| v == "true").unwrap_or(default)
}

fn parse_i32(value: Option<&String>, default: i32) -> i32 {
    value.and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn parse_f64(value: Option<&String>, default: f64) -> f64 {
    value.and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// options.txt stores these as 0.0-1.0 fractions; Waybound stores 0-100 ints.
fn percent_from_file(value: Option<&String>, default: i32) -> i32 {
    (parse_f64(value, default as f64 / 100.0) * 100.0).round() as i32
}

// Minecraft stores FOV as a normalized value where 0.0 = 70° (default),
// -1.0 = 30°, and 1.0 = 110°, i.e. degrees = 70 + value * 40. A raw value
// clearly outside [-1, 1] is treated as already being in degrees.
fn fov_from_file(raw: f64) -> i32 {
    if raw.abs() <= 1.0 {
        (70.0 + raw * 40.0).round() as i32
    } else {
        raw.round() as i32
    }
}

fn fov_to_file(fov: i32) -> f64 {
    ((fov as f64 - 70.0) / 40.0).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_defaults() {
        // Whole-struct equality, so every field added later is covered for
        // free: forget a serialize/parse line and this fails.
        let options = McOptions::default();
        assert_eq!(parse_options(&serialize_options(&options)), options);
    }

    #[test]
    fn roundtrip_non_default_values() {
        let mut options = McOptions::default();
        options.invert_x_mouse = true;
        options.high_contrast = true;
        options.directional_audio = true;
        options.hide_lightning_flashes = true;
        options.force_unicode_font = true;
        options.damage_tilt_strength = 35;
        options.ao = false;
        options.screen_effect_scale = 0;
        options.fov_effect_scale = 20;
        options.darkness_effect_scale = 75;
        options.ui_volume = 40;

        assert_eq!(parse_options(&serialize_options(&options)), options);
    }

    #[test]
    fn zero_to_one_floats_scale_at_the_file_boundary() {
        for (stored, written) in [(0, "0.00"), (50, "0.50"), (100, "1.00")] {
            let mut options = McOptions::default();
            options.damage_tilt_strength = stored;
            options.screen_effect_scale = stored;
            options.fov_effect_scale = stored;
            options.darkness_effect_scale = stored;
            options.ui_volume = stored;

            let serialized = serialize_options(&options);
            for key in [
                "damageTiltStrength",
                "screenEffectScale",
                "fovEffectScale",
                "darknessEffectScale",
                "soundCategory_ui",
            ] {
                assert!(
                    serialized.contains(&format!("{key}:{written}")),
                    "{key} at {stored} should write {written}: {serialized}"
                );
            }

            let parsed = parse_options(&serialized);
            assert_eq!(parsed.damage_tilt_strength, stored);
            assert_eq!(parsed.screen_effect_scale, stored);
            assert_eq!(parsed.fov_effect_scale, stored);
            assert_eq!(parsed.darkness_effect_scale, stored);
            assert_eq!(parsed.ui_volume, stored);
        }
    }

    #[test]
    fn fov_matches_minecraft_convention() {
        // Default 70° serializes to Minecraft's `fov:0.0`.
        assert!((fov_to_file(70)).abs() < 1e-9);
        assert_eq!(fov_from_file(0.0), 70);
        assert_eq!(fov_from_file(1.0), 110);
        assert_eq!(fov_from_file(-1.0), 30);
    }

    #[test]
    fn mouse_sensitivity_matches_minecraft_percent_convention() {
        // Vanilla displays value*200 as the percent, e.g. file value 0.5 -> 100%.
        let mut options = McOptions::default();
        options.mouse_sensitivity = 60;
        let serialized = serialize_options(&options);
        assert!(
            serialized.contains("mouseSensitivity:0.30"),
            "60% should serialize to file value 0.30: {serialized}"
        );
        let parsed = parse_options(&serialized);
        assert_eq!(parsed.mouse_sensitivity, 60);
    }

    #[test]
    fn merge_preserves_mod_keybinds_and_unmodeled_settings() {
        let pack_options = "fullscreen:false\n\
             resourcePacks:[\"vanilla\",\"confluence:terraria_art\"]\n\
             lang:en_us\n\
             key_key.confluence.hook:key.keyboard.r\n\
             key_key.jump:key.keyboard.space\n";

        let mut incoming = McOptions::default();
        incoming.fullscreen = true; // global setting the user wants enforced
        incoming
            .key_bindings
            .insert("jump".to_string(), "key.keyboard.space".to_string());

        let merged = merge_options(pack_options, &incoming);

        assert!(merged.contains("fullscreen:true"), "global setting should win");
        assert!(
            merged.contains("resourcePacks:[\"vanilla\",\"confluence:terraria_art\"]"),
            "unmodeled pack setting must survive: {merged}"
        );
        assert!(
            merged.contains("key_key.confluence.hook:key.keyboard.r"),
            "mod-added keybind must survive: {merged}"
        );
    }

    #[test]
    fn merge_still_preserves_unmanaged_keys_alongside_the_new_options() {
        // Keys that neighbour the ones Waybound now manages but aren't
        // managed themselves: the new options must not swallow them.
        let existing = "ao:false\n\
             attackIndicator:1\n\
             chatOpacity:1.0\n\
             skipMultiplayerWarning:true\n\
             glintSpeed:0.5\n";

        let merged = merge_options(existing, &McOptions::default());

        assert!(merged.contains("ao:true"), "managed key wins: {merged}");
        for unmanaged in [
            "attackIndicator:1",
            "chatOpacity:1.0",
            "skipMultiplayerWarning:true",
            "glintSpeed:0.5",
        ] {
            assert!(merged.contains(unmanaged), "{unmanaged} must survive: {merged}");
        }
        // And the new keys the file didn't have get appended.
        assert!(merged.contains("soundCategory_ui:1.00"), "{merged}");
        assert!(merged.contains("damageTiltStrength:1.00"), "{merged}");
    }

    #[test]
    fn fresh_file_gets_version_stamp_existing_version_survives() {        // No existing file: the stamp must be present or vanilla datafixes
        // the modern key names and discards everything.
        let fresh = merge_options("", &McOptions::default());
        assert!(
            fresh.starts_with(&format!("version:{OPTIONS_DATA_VERSION}")),
            "fresh options.txt needs a version stamp: {fresh}"
        );

        // A pack-shipped version line wins over the stamp.
        let merged = merge_options("version:3465\nfullscreen:false\n", &McOptions::default());
        assert!(merged.contains("version:3465"), "{merged}");
        assert!(!merged.contains("version:99999999"), "{merged}");
    }

    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        haystack.lines().filter(|l| l.trim() == needle).count()
    }

    #[test]
    fn merge_never_duplicates_keys_no_matter_the_history() {
        // Double-apply (create, then modpack re-apply) must be a fixed
        // point: every key exactly once. (This caught a real crash: the
        // second tail loop re-emitted the whole key block on fresh files,
        // and NeoForge's early-display parser throws on duplicate keys.)
        let once = merge_options("", &McOptions::default());
        let twice = merge_options(&once, &McOptions::default());
        assert_eq!(once, twice, "second merge must be a no-op");
        for line in once.lines() {
            if line.trim().is_empty() || !line.contains(':') {
                continue;
            }
            assert_eq!(
                count_occurrences(&twice, line.trim()),
                1,
                "duplicated line: {line}"
            );
        }
    }

    #[test]
    fn mod_added_binding_present_on_both_sides_is_not_tripled() {
        // A mod keybind the pack ships AND the incoming options carry (e.g.
        // parsed back from a previous merge): the keep-as-is path doesn't
        // mark seen, so both tail loops must still not re-emit it.
        let existing = "key_key.confluence.hook:key.keyboard.r\n";
        let mut incoming = McOptions::default();
        incoming
            .key_bindings
            .insert("confluence.hook".to_string(), "key.keyboard.r".to_string());
        let merged = merge_options(existing, &incoming);
        assert_eq!(
            count_occurrences(&merged, "key_key.confluence.hook:key.keyboard.r"),
            1,
            "mod binding duplicated: {merged}"
        );
    }
}

#[cfg(test)]
mod config_forward_compat_tests {
    use super::McOptions;

    #[test]
    fn a_config_written_before_new_fields_existed_still_deserializes() {
        // Verbatim shape of a real pre-existing `[defaultMcOptions]` table:
        // none of the accessibility/comfort/skin-era keys are present. Adding
        // a required field here once made the entire config.toml unparseable,
        // which `ConfigStore::load` treats as corruption — it renamed the
        // file to `.bak` and started fresh, destroying the saved account and
        // launch settings. Missing keys must fall back to defaults.
        let legacy = r#"
            customize = true
            fullscreen = false
            viewBobbing = true
            guiScale = 2
            gamma = 50
            renderDistance = 12
            simulationDistance = 12
            fov = 70
            entityShadows = true
            vsync = true
            maxFps = 120
            graphicsMode = "fancy"
            clouds = "fancy"
            particles = "all"
            mipmapLevels = 4
            entityDistanceScaling = 100
            biomeBlendRadius = 2
            autoJump = false
            invertMouse = false
            mouseSensitivity = 100
            rawMouseInput = true
            discreteMouseScroll = false
            toggleSprint = false
            toggleCrouch = false
            showSubtitles = false
            reducedDebugInfo = false
            narrator = "off"
            language = "en_us"
            masterVolume = 100
            musicVolume = 100
            jukeboxVolume = 100
            weatherVolume = 100
            blocksVolume = 100
            hostileVolume = 100
            neutralVolume = 100
            playerVolume = 100
            ambientVolume = 100
            voiceVolume = 100
        "#;

        let parsed: McOptions =
            toml::from_str(legacy).expect("a pre-existing config must still load");

        // Values actually present are honoured...
        assert_eq!(parsed.gui_scale, 2);
        assert_eq!(parsed.language, "en_us");
        // ...and every field added afterwards falls back to its default
        // rather than failing the whole parse.
        let defaults = McOptions::default();
        assert_eq!(parsed.ao, defaults.ao);
        assert_eq!(parsed.high_contrast, defaults.high_contrast);
        assert_eq!(parsed.screen_effect_scale, defaults.screen_effect_scale);
        assert_eq!(parsed.ui_volume, defaults.ui_volume);
        assert_eq!(parsed.invert_x_mouse, defaults.invert_x_mouse);
    }

    #[test]
    fn an_empty_table_yields_defaults_rather_than_an_error() {
        // The degenerate case of the same rule: nothing present at all.
        assert_eq!(
            toml::from_str::<McOptions>("").expect("empty table must load"),
            McOptions::default(),
        );
    }
}
