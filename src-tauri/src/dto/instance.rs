use serde::{Deserialize, Serialize};

use super::{ModLoader, ModSource, ModSummary};

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
}

/// All content in an instance, grouped by category.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceContent {
    pub mods: Vec<ContentEntry>,
    pub resource_packs: Vec<ContentEntry>,
    pub shader_packs: Vec<ContentEntry>,
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
