use serde::{Deserialize, Serialize};

use super::{ModLoader, ModOrigin, ModSource, ModSummary};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSummary {
    pub id: String,
    pub name: String,
    pub minecraft_version: String,
    pub loader: ModLoader,
    pub loader_version: Option<String>,
    pub mod_count: u32,
    pub created_at: u64,
    pub root_path: String,
    /// Optional instance icon as a data URL (small PNG).
    #[serde(default)]
    pub icon: Option<String>,
    /// Unix seconds of the most recent launch, if ever launched.
    #[serde(default)]
    pub last_played: Option<u64>,
    /// Total accumulated play time in seconds.
    #[serde(default)]
    pub total_play_seconds: u64,
    /// The installed modpack's own version/filename label, recorded at
    /// import time (e.g. "Ascendra-2.1.0"). Absent for a manually-created
    /// instance or one that only ever had individual mods installed.
    #[serde(default)]
    pub modpack_version_label: Option<String>,
    /// The installed modpack's project uid (e.g. `"curseforge:12345"`),
    /// recorded at import time. Lets the instance offer the pack's other
    /// versions for in-place switching — without it there's no project to
    /// re-resolve against, only a display label.
    #[serde(default)]
    pub modpack_project_uid: Option<String>,
}

/// A single content file inside an instance (mod, resource pack, or shader).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentEntry {
    /// Display file name (without the `.disabled` suffix).
    pub file_name: String,
    /// The mod's own declared display name (from fabric.mod.json / mods.toml /
    /// mcmod.info), when it could be read from the jar. Falls back to a
    /// filename-derived guess on the frontend when absent.
    #[serde(default)]
    pub name: Option<String>,
    /// A `data:` URL for an icon embedded in the file, or a remote URL
    /// recorded when the mod was installed via Browse. Absent when neither
    /// source has one — the frontend falls back to a letter avatar.
    #[serde(default)]
    pub icon: Option<String>,
    /// False when the file is `.disabled` (present but not loaded by the game).
    pub enabled: bool,
    pub size_bytes: u64,
    /// True when `name`/`icon` were already filled in from the on-disk
    /// metadata cache (a prior `get_content_meta` call for this exact file,
    /// unchanged since). Lets the frontend skip its per-row fetch entirely
    /// for anything already resolved — including a file with no icon at all,
    /// which would otherwise look identical to "not fetched yet".
    #[serde(default)]
    pub meta_resolved: bool,
    /// True when at least one file/folder under `config/` looks like it
    /// belongs to this mod - computed once per `list_instance_content` call
    /// (one scan of `config/`'s top level shared across every mod row)
    /// rather than per-row, so showing the "Config" button doesn't cost a
    /// separate lookup for each of a few hundred mods.
    #[serde(default)]
    pub has_config: bool,
    /// True when the file was added by the user (Browse install, manual
    /// drop) rather than placed by a modpack import. Resolved from the
    /// tracked row's origin; untracked files read as user-added since no
    /// pack claims them.
    #[serde(default)]
    pub added_by_you: bool,
}

/// All content in an instance, grouped by category.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceContent {
    pub mods: Vec<ContentEntry>,
    pub resource_packs: Vec<ContentEntry>,
    pub shader_packs: Vec<ContentEntry>,
}

/// One singleplayer world (`saves/<folder>/level.dat`). Best-effort: a
/// corrupt or unreadable level.dat degrades to the folder name with
/// everything else absent, never an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldEntry {
    /// The folder name under `saves/` — the stable identifier.
    pub folder_name: String,
    pub name: Option<String>,
    /// Unix millis of last play (`Data.LastPlayed`), when present.
    pub last_played_ms: Option<i64>,
    /// Human game mode ("Survival", "Creative", ...), when present.
    pub game_mode: Option<String>,
    /// Game version the world was last saved with, when present.
    pub game_version: Option<String>,
    /// The world's `icon.png` as a data URL, when present.
    pub icon: Option<String>,
}

/// One saved multiplayer server (`servers.dat` entry). Read-only view —
/// no pinging, no editing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerEntry {
    pub name: String,
    pub address: String,
    /// The server's icon as a data URL, when the entry carries one.
    pub icon: Option<String>,
}

/// One config file a mod's "Config" button can open — `relative_path` is
/// relative to the instance's `config/` folder and is the stable identifier
/// used to read/write it (safe_join'd against `config/`, never trusted as a
/// literal filesystem path).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFileEntry {
    pub relative_path: String,
    pub display_name: String,
}

/// A single entry's display name + icon, resolved on demand (opening and
/// parsing the jar/zip) once the row actually scrolls into view, instead of
/// up front for every file in the instance.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMeta {
    pub name: Option<String>,
    pub icon: Option<String>,
}

/// A jar whose embedded metadata says it's built for a different loader
/// than the instance runs — e.g. a NeoForge jar on a Forge instance, which
/// the game silently refuses to load (then dies on "missing" dependencies
/// that are all sitting in `mods/`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrongLoaderFile {
    pub file_name: String,
    pub mod_name: Option<String>,
    pub detected_loader: String,
}

/// A required dependency (by mod id) no installed jar provides. Version
/// ranges are NOT evaluated — only presence — the loader itself judges
/// ranges at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingDep {
    pub file_name: String,
    pub mod_name: Option<String>,
    pub dep_mod_id: String,
    pub version_range: Option<String>,
}

/// Pre-launch readiness: blockers found by reading every enabled jar's own
/// metadata. Empty lists mean "nothing obviously wrong" — never a guarantee
/// the game will start.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchReadiness {
    pub checked_files: u32,
    pub wrong_loader: Vec<WrongLoaderFile>,
    pub missing_deps: Vec<MissingDep>,
}

/// Per-instance launch overrides. Empty/None fields fall back to global config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceLaunchConfig {
    pub java_path: Option<String>,
    pub max_memory_mb: Option<u32>,
    pub jvm_args: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInstanceInput {
    pub name: String,
    pub minecraft_version: String,
    pub loader: ModLoader,
    pub loader_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledMod {
    pub id: i64,
    pub instance_id: String,
    pub mod_uid: String,
    pub mod_name: String,
    pub source: ModSource,
    pub file_name: String,
    pub installed_at: u64,
    /// The project's icon URL, captured at install time when known (Browse
    /// installs). Absent for mods synced in from a modpack's mods folder,
    /// since there's no project link to fetch one from.
    #[serde(default)]
    pub icon_url: Option<String>,
    /// How the mod arrived: user-installed versus pack-placed. Always
    /// written by the backend (migrated + backfilled for old rows).
    pub origin: ModOrigin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallModInput {
    pub mod_summary: ModSummary,
    pub source: Option<ModSource>,
    pub instance_id: Option<String>,
    pub create_instance: Option<CreateInstanceInput>,
    pub version_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallModResult {
    /// Absent when the file needs a manual download (see `missing_mods`
    /// below) — nothing was actually installed yet in that case.
    pub installed: Option<InstalledMod>,
    pub message: String,
    pub instance: InstanceSummary,
    /// True when the install's message includes a manual-download list for
    /// files that couldn't be fetched automatically — the frontend keeps
    /// that notification on screen instead of auto-dismissing it.
    #[serde(default)]
    pub has_skipped: bool,
    /// Files CurseForge won't hand out automatically (author disabled
    /// third-party downloads). Lets the frontend offer "open these pages so
    /// you can download them yourself" instead of just printing a message.
    #[serde(default)]
    pub missing_mods: Vec<MissingMod>,
}

/// A file skipped during a CurseForge install/modpack import because the
/// author disabled third-party/API downloads for it. `filename` is
/// CurseForge's own name for the file, used as a fallback label and initial
/// guess; `sha1`, when CurseForge reported one, is the reliable match — a
/// browser can silently rename a duplicate download ("mod (1).jar"), but its
/// content hash doesn't change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingMod {
    pub project_id: u32,
    pub name: String,
    pub filename: String,
    pub url: String,
    #[serde(default)]
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameVersionOption {
    pub version: String,
    pub version_type: String,
}

impl ModLoader {
    pub fn as_modrinth(self) -> &'static str {
        match self {
            ModLoader::Fabric => "fabric",
            ModLoader::Forge => "forge",
            ModLoader::NeoForge => "neoforge",
            ModLoader::Quilt => "quilt",
            ModLoader::Vanilla => "minecraft",
        }
    }

    pub fn as_curseforge_loader_type(self) -> u32 {
        match self {
            ModLoader::Forge => 1,
            ModLoader::Fabric => 4,
            ModLoader::Quilt => 5,
            ModLoader::NeoForge => 6,
            ModLoader::Vanilla => 0,
        }
    }

    pub fn from_modrinth(value: &str) -> Option<Self> {
        match value {
            "fabric" => Some(ModLoader::Fabric),
            "forge" => Some(ModLoader::Forge),
            "neoforge" => Some(ModLoader::NeoForge),
            "quilt" => Some(ModLoader::Quilt),
            "minecraft" => Some(ModLoader::Vanilla),
            _ => None,
        }
    }
}

#[cfg(test)]
mod loader_mapping_tests {
    use super::*;

    const ALL: [ModLoader; 5] = [
        ModLoader::Fabric,
        ModLoader::Forge,
        ModLoader::NeoForge,
        ModLoader::Quilt,
        ModLoader::Vanilla,
    ];

    #[test]
    fn every_loader_round_trips_through_the_modrinth_slug() {
        for loader in ALL {
            assert_eq!(
                ModLoader::from_modrinth(loader.as_modrinth()),
                Some(loader),
                "{loader:?} did not survive as_modrinth -> from_modrinth"
            );
        }
    }

    #[test]
    fn modrinth_slugs_are_the_exact_strings_the_api_expects() {
        // These are wire values, not display strings: changing one silently
        // breaks every Modrinth facet query built from it.
        assert_eq!(ModLoader::Fabric.as_modrinth(), "fabric");
        assert_eq!(ModLoader::Forge.as_modrinth(), "forge");
        assert_eq!(ModLoader::NeoForge.as_modrinth(), "neoforge");
        assert_eq!(ModLoader::Quilt.as_modrinth(), "quilt");
        // Vanilla is "minecraft" on Modrinth, not "vanilla".
        assert_eq!(ModLoader::Vanilla.as_modrinth(), "minecraft");
    }

    #[test]
    fn from_modrinth_is_case_sensitive_and_rejects_unknown_slugs() {
        assert_eq!(ModLoader::from_modrinth("Fabric"), None);
        assert_eq!(ModLoader::from_modrinth("FABRIC"), None);
        assert_eq!(ModLoader::from_modrinth("NeoForge"), None);
        // "vanilla" is the frontend's word, never Modrinth's.
        assert_eq!(ModLoader::from_modrinth("vanilla"), None);
        assert_eq!(ModLoader::from_modrinth(""), None);
        assert_eq!(ModLoader::from_modrinth(" fabric"), None);
        assert_eq!(ModLoader::from_modrinth("fabric "), None);
        assert_eq!(ModLoader::from_modrinth("liteloader"), None);
    }

    #[test]
    fn curseforge_loader_type_ids_match_the_documented_enum() {
        assert_eq!(ModLoader::Forge.as_curseforge_loader_type(), 1);
        assert_eq!(ModLoader::Fabric.as_curseforge_loader_type(), 4);
        assert_eq!(ModLoader::Quilt.as_curseforge_loader_type(), 5);
        assert_eq!(ModLoader::NeoForge.as_curseforge_loader_type(), 6);
        // 0 = "Any" in CurseForge's modLoaderType enum, which is what a
        // vanilla instance wants: no loader filter at all.
        assert_eq!(ModLoader::Vanilla.as_curseforge_loader_type(), 0);
    }

    #[test]
    fn curseforge_loader_type_ids_are_distinct_per_loader() {
        let mut ids: Vec<u32> = ALL.iter().map(|l| l.as_curseforge_loader_type()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "two loaders share a CurseForge type id");
    }

    #[test]
    fn loader_serde_representation_is_lowercase() {
        // The frontend sends/receives these; `neoforge` must not become
        // `neoForge` if the enum ever gains a rename attribute.
        for (loader, json) in [
            (ModLoader::Fabric, "\"fabric\""),
            (ModLoader::Forge, "\"forge\""),
            (ModLoader::NeoForge, "\"neoforge\""),
            (ModLoader::Quilt, "\"quilt\""),
            (ModLoader::Vanilla, "\"vanilla\""),
        ] {
            assert_eq!(serde_json::to_string(&loader).unwrap(), json);
            assert_eq!(serde_json::from_str::<ModLoader>(json).unwrap(), loader);
        }
    }
}
