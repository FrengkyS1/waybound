//! Mojang piston-meta types: the version manifest and per-version JSON that
//! describe every file and argument needed to launch a given Minecraft version.
//!
//! Only the fields we actually consume are modeled. `arguments` (1.13+) and the
//! legacy `minecraftArguments` string (<=1.12) are both supported, as is
//! `inheritsFrom` so that a Fabric profile can layer onto the vanilla JSON.

use serde::Deserialize;

pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const RESOURCES_BASE_URL: &str = "https://resources.download.minecraft.net";

#[derive(Debug, Deserialize)]
pub struct VersionManifest {
    pub versions: Vec<ManifestVersion>,
}

#[derive(Debug, Deserialize)]
pub struct ManifestVersion {
    pub id: String,
    pub url: String,
}

impl VersionManifest {
    pub fn find(&self, id: &str) -> Option<&ManifestVersion> {
        self.versions.iter().find(|v| v.id == id)
    }
}

/// A per-version JSON. Most fields are optional because a Fabric profile that
/// `inheritsFrom` vanilla only carries the overriding pieces.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    #[serde(default)]
    pub inherits_from: Option<String>,
    #[serde(default)]
    pub main_class: Option<String>,
    #[serde(rename = "type", default)]
    pub version_type: Option<String>,

    #[serde(default)]
    pub asset_index: Option<AssetIndexRef>,
    #[serde(default)]
    pub downloads: Option<Downloads>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub java_version: Option<JavaVersion>,

    /// 1.13+ structured arguments.
    #[serde(default)]
    pub arguments: Option<Arguments>,
    /// <=1.12 flat argument template.
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexRef {
    pub id: String,
    pub url: String,
    pub sha1: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Downloads {
    pub client: Option<DownloadEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadEntry {
    pub url: String,
    #[serde(default)]
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    #[serde(default)]
    pub major_version: Option<u32>,
    /// Mojang runtime component name (e.g. "java-runtime-delta") used to
    /// auto-download a matching JRE when none is installed.
    #[serde(default)]
    pub component: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Library {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<LibraryDownloads>,
    /// Maven base URL for libraries without an explicit `downloads` block
    /// (common in Fabric loader profiles).
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// OS -> classifier key for legacy natives (<=1.18).
    #[serde(default)]
    pub natives: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibraryDownloads {
    #[serde(default)]
    pub artifact: Option<Artifact>,
    #[serde(default)]
    pub classifiers: Option<std::collections::HashMap<String, Artifact>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    #[serde(default)]
    pub path: Option<String>,
    pub url: String,
    #[serde(default)]
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<Argument>,
    #[serde(default)]
    pub jvm: Vec<Argument>,
}

/// An argument is either a bare string or a conditional `{ rules, value }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    Plain(String),
    Conditional { rules: Vec<Rule>, value: ArgValue },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ArgValue {
    Single(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub action: String,
    #[serde(default)]
    pub os: Option<OsRule>,
    /// Feature flags (demo mode, custom resolution, ...). We enable none, so any
    /// rule that requires a feature is treated as not-applicable.
    #[serde(default)]
    pub features: Option<std::collections::HashMap<String, bool>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
}

/// Evaluate Mojang OS/feature rules for the current platform with no features
/// enabled. Returns whether the associated element should be included.
pub fn rules_allow(rules: &[Rule]) -> bool {
    if rules.is_empty() {
        return true;
    }
    // Default deny when rules are present; the last matching rule wins.
    let mut allowed = false;
    for rule in rules {
        if rule_matches(rule) {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

fn rule_matches(rule: &Rule) -> bool {
    // Any feature requirement fails: we enable no optional features.
    if let Some(features) = &rule.features {
        if features.values().any(|v| *v) {
            return false;
        }
    }
    if let Some(os) = &rule.os {
        if let Some(name) = &os.name {
            if name != current_os_name() {
                return false;
            }
        }
        if let Some(arch) = &os.arch {
            if !arch_matches(arch) {
                return false;
            }
        }
    }
    true
}

pub fn current_os_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

/// The classifier key Mojang uses for this platform's legacy natives.
pub fn natives_classifier_key() -> &'static str {
    if cfg!(windows) {
        if cfg!(target_arch = "aarch64") {
            "natives-windows-arm64"
        } else {
            "natives-windows"
        }
    } else if cfg!(target_os = "macos") {
        "natives-macos"
    } else {
        "natives-linux"
    }
}

fn arch_matches(arch: &str) -> bool {
    match arch {
        "x86" => cfg!(target_arch = "x86"),
        "x64" | "x86_64" => cfg!(target_arch = "x86_64"),
        "arm64" | "aarch64" => cfg!(target_arch = "aarch64"),
        _ => true,
    }
}
