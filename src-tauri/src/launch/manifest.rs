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

#[cfg(test)]
mod version_json_parsing_tests {
    use super::*;

    /// A trimmed-down but structurally faithful vanilla version JSON.
    const VANILLA: &str = r#"{
        "id": "1.20.1",
        "type": "release",
        "mainClass": "net.minecraft.client.main.Main",
        "javaVersion": { "component": "java-runtime-gamma", "majorVersion": 17 },
        "assetIndex": {
            "id": "5",
            "url": "https://piston-meta.mojang.com/v1/packages/abc/5.json",
            "sha1": "abc123",
            "size": 123456,
            "totalSize": 654321
        },
        "downloads": {
            "client": {
                "url": "https://piston-data.mojang.com/v1/objects/deadbeef/client.jar",
                "sha1": "deadbeef",
                "size": 25000000
            },
            "server": {
                "url": "https://piston-data.mojang.com/v1/objects/cafe/server.jar",
                "sha1": "cafe",
                "size": 45000000
            }
        },
        "libraries": [
            {
                "name": "com.mojang:logging:1.1.1",
                "downloads": {
                    "artifact": {
                        "path": "com/mojang/logging/1.1.1/logging-1.1.1.jar",
                        "url": "https://libraries.minecraft.net/com/mojang/logging/1.1.1/logging-1.1.1.jar",
                        "sha1": "1111",
                        "size": 10
                    }
                }
            },
            {
                "name": "org.lwjgl:lwjgl:3.3.1:natives-windows",
                "rules": [{ "action": "allow", "os": { "name": "windows" } }],
                "downloads": {
                    "classifiers": {
                        "natives-windows": {
                            "path": "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar",
                            "url": "https://libraries.minecraft.net/lwjgl-natives-windows.jar",
                            "sha1": "2222"
                        }
                    }
                },
                "natives": { "windows": "natives-windows", "linux": "natives-linux" }
            }
        ],
        "arguments": {
            "game": [
                "--username",
                "${auth_player_name}",
                {
                    "rules": [{ "action": "allow", "features": { "is_demo_user": true } }],
                    "value": "--demo"
                },
                {
                    "rules": [{ "action": "allow", "features": { "has_custom_resolution": true } }],
                    "value": ["--width", "${resolution_width}"]
                }
            ],
            "jvm": [
                { "rules": [{ "action": "allow", "os": { "name": "osx" } }], "value": "-XstartOnFirstThread" },
                "-Djava.library.path=${natives_directory}",
                "-cp",
                "${classpath}"
            ]
        }
    }"#;

    /// A Forge/Fabric-shaped profile: only overrides, plus `inheritsFrom`.
    const INHERITING_PROFILE: &str = r#"{
        "id": "1.20.1-forge-47.2.0",
        "inheritsFrom": "1.20.1",
        "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
        "libraries": [
            { "name": "net.minecraftforge:fmlloader:1.20.1-47.2.0", "url": "https://maven.minecraftforge.net/" }
        ],
        "arguments": {
            "game": ["--launchTarget", "forgeclient"],
            "jvm": ["-DignoreList=${classpath}"]
        }
    }"#;

    #[test]
    fn a_vanilla_version_json_parses_every_field_the_launcher_consumes() {
        let json: VersionJson = serde_json::from_str(VANILLA).unwrap();

        assert_eq!(json.id, "1.20.1");
        assert_eq!(json.version_type.as_deref(), Some("release"));
        assert_eq!(json.main_class.as_deref(), Some("net.minecraft.client.main.Main"));
        assert!(json.inherits_from.is_none());

        let assets = json.asset_index.as_ref().unwrap();
        assert_eq!(assets.id, "5");
        assert_eq!(assets.sha1, "abc123");

        let client = json.downloads.as_ref().unwrap().client.as_ref().unwrap();
        assert!(client.url.ends_with("client.jar"));
        assert_eq!(client.sha1.as_deref(), Some("deadbeef"));

        let java = json.java_version.as_ref().unwrap();
        assert_eq!(java.major_version, Some(17));
        assert_eq!(java.component.as_deref(), Some("java-runtime-gamma"));

        assert_eq!(json.libraries.len(), 2);
        let artifact = json.libraries[0]
            .downloads
            .as_ref()
            .unwrap()
            .artifact
            .as_ref()
            .unwrap();
        assert_eq!(
            artifact.path.as_deref(),
            Some("com/mojang/logging/1.1.1/logging-1.1.1.jar")
        );
        assert!(json.libraries[0].rules.is_empty());
        assert_eq!(
            json.libraries[1].natives.as_ref().unwrap().get("windows").map(String::as_str),
            Some("natives-windows")
        );
        assert!(json.libraries[1]
            .downloads
            .as_ref()
            .unwrap()
            .classifiers
            .as_ref()
            .unwrap()
            .contains_key("natives-windows"));
    }

    #[test]
    fn unknown_fields_like_server_downloads_and_sizes_are_ignored_not_fatal() {
        // The fixture carries `size`, `totalSize` and a `server` download that
        // the structs don't model at all — parsing must still succeed.
        assert!(serde_json::from_str::<VersionJson>(VANILLA).is_ok());
    }

    #[test]
    fn structured_arguments_parse_as_a_mix_of_plain_and_conditional_entries() {
        let json: VersionJson = serde_json::from_str(VANILLA).unwrap();
        let args = json.arguments.as_ref().unwrap();

        assert!(matches!(&args.game[0], Argument::Plain(s) if s == "--username"));
        match &args.game[2] {
            Argument::Conditional { rules, value } => {
                assert_eq!(rules[0].action, "allow");
                assert!(matches!(value, ArgValue::Single(v) if v == "--demo"));
            }
            other => panic!("expected a conditional game argument, got {other:?}"),
        }
        match &args.game[3] {
            Argument::Conditional { value: ArgValue::Many(values), .. } => {
                assert_eq!(values, &["--width".to_string(), "${resolution_width}".to_string()]);
            }
            other => panic!("expected a multi-valued conditional, got {other:?}"),
        }
        assert_eq!(args.jvm.len(), 4);
        assert!(matches!(&args.jvm[1], Argument::Plain(s) if s.starts_with("-Djava.library.path")));
    }

    #[test]
    fn a_legacy_flat_argument_string_parses_when_arguments_is_absent() {
        let json: VersionJson = serde_json::from_str(
            r#"{ "id": "1.12.2", "minecraftArguments": "--username ${auth_player_name} --version ${version_name}" }"#,
        )
        .unwrap();
        assert!(json.arguments.is_none());
        assert!(json
            .minecraft_arguments
            .as_deref()
            .unwrap()
            .contains("${auth_player_name}"));
    }

    #[test]
    fn an_inheriting_profile_parses_with_only_its_overrides() {
        let json: VersionJson = serde_json::from_str(INHERITING_PROFILE).unwrap();

        assert_eq!(json.inherits_from.as_deref(), Some("1.20.1"));
        assert_eq!(
            json.main_class.as_deref(),
            Some("cpw.mods.bootstraplauncher.BootstrapLauncher")
        );
        // Everything vanilla would supply is absent, not defaulted to junk.
        assert!(json.asset_index.is_none());
        assert!(json.downloads.is_none());
        assert!(json.java_version.is_none());
        assert!(json.version_type.is_none());

        // A maven-url library with no `downloads` block is the Fabric/Forge shape.
        assert_eq!(json.libraries.len(), 1);
        assert!(json.libraries[0].downloads.is_none());
        assert_eq!(
            json.libraries[0].url.as_deref(),
            Some("https://maven.minecraftforge.net/")
        );
    }

    #[test]
    fn only_id_is_required_and_every_other_field_defaults_to_empty() {
        let json: VersionJson = serde_json::from_str(r#"{ "id": "bare" }"#).unwrap();
        assert_eq!(json.id, "bare");
        assert!(json.libraries.is_empty());
        assert!(json.arguments.is_none());
        assert!(json.minecraft_arguments.is_none());
        assert!(json.main_class.is_none());

        // Without an id there is nothing to launch, so this must fail loudly.
        assert!(serde_json::from_str::<VersionJson>(r#"{ "type": "release" }"#).is_err());
    }

    #[test]
    fn the_version_manifest_finds_a_version_by_id_and_reports_a_miss() {
        let manifest: VersionManifest = serde_json::from_str(
            r#"{
                "latest": { "release": "1.20.1", "snapshot": "23w31a" },
                "versions": [
                    { "id": "1.20.1", "url": "https://example.invalid/1.20.1.json", "type": "release" },
                    { "id": "1.19.4", "url": "https://example.invalid/1.19.4.json", "type": "release" }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(manifest.versions.len(), 2);
        assert_eq!(
            manifest.find("1.19.4").map(|v| v.url.as_str()),
            Some("https://example.invalid/1.19.4.json")
        );
        assert!(manifest.find("1.19.4 ").is_none());
        assert!(manifest.find("1.99").is_none());
    }
}

#[cfg(test)]
mod rule_evaluation_tests {
    use super::*;

    fn rule(action: &str, os_name: Option<&str>) -> Rule {
        Rule {
            action: action.to_string(),
            os: os_name.map(|name| OsRule {
                name: Some(name.to_string()),
                arch: None,
            }),
            features: None,
        }
    }

    fn other_os() -> &'static str {
        if current_os_name() == "windows" {
            "linux"
        } else {
            "windows"
        }
    }

    #[test]
    fn no_rules_means_always_included() {
        assert!(rules_allow(&[]));
    }

    #[test]
    fn an_unconditional_allow_includes_the_element() {
        assert!(rules_allow(&[rule("allow", None)]));
    }

    #[test]
    fn a_rule_set_that_never_matches_this_os_denies_by_default() {
        assert!(!rules_allow(&[rule("allow", Some(other_os()))]));
    }

    #[test]
    fn an_allow_for_the_current_os_matches() {
        assert!(rules_allow(&[rule("allow", Some(current_os_name()))]));
    }

    #[test]
    fn the_last_matching_rule_wins() {
        // Mojang's natives pattern: allow everywhere, then carve this OS out.
        assert!(!rules_allow(&[
            rule("allow", None),
            rule("disallow", Some(current_os_name())),
        ]));
        // ...and the reverse order leaves it allowed.
        assert!(rules_allow(&[
            rule("disallow", Some(current_os_name())),
            rule("allow", None),
        ]));
    }

    #[test]
    fn a_disallow_for_another_os_does_not_affect_this_one() {
        assert!(rules_allow(&[
            rule("allow", None),
            rule("disallow", Some(other_os())),
        ]));
    }

    #[test]
    fn a_rule_requiring_an_optional_feature_never_matches() {
        // We enable no features, so demo mode and custom resolution arguments
        // must always be dropped.
        let demo = Rule {
            action: "allow".to_string(),
            os: None,
            features: Some(std::collections::HashMap::from([(
                "is_demo_user".to_string(),
                true,
            )])),
        };
        assert!(!rules_allow(std::slice::from_ref(&demo)));
        // A feature explicitly required to be *off* still matches.
        let not_demo = Rule {
            action: "allow".to_string(),
            os: None,
            features: Some(std::collections::HashMap::from([(
                "is_demo_user".to_string(),
                false,
            )])),
        };
        assert!(rules_allow(&[not_demo]));
    }

    #[test]
    fn an_arch_specific_rule_matches_only_this_architecture() {
        let arm_only = Rule {
            action: "allow".to_string(),
            os: Some(OsRule {
                name: None,
                arch: Some("x86".to_string()),
            }),
            features: None,
        };
        assert_eq!(rules_allow(&[arm_only]), cfg!(target_arch = "x86"));

        // An arch string Mojang never emits is treated as "no constraint".
        let unknown = Rule {
            action: "allow".to_string(),
            os: Some(OsRule {
                name: None,
                arch: Some("riscv".to_string()),
            }),
            features: None,
        };
        assert!(rules_allow(&[unknown]));
    }

    #[test]
    fn platform_helpers_report_a_consistent_os_and_natives_key() {
        let os = current_os_name();
        assert!(matches!(os, "windows" | "osx" | "linux"));
        let key = natives_classifier_key();
        assert!(key.starts_with("natives-"), "{key} is not a natives classifier");
        // The keys use "macos" while the rules use "osx" — keep them in sync
        // with the platform they were derived from.
        let expected_family = if os == "osx" { "macos" } else { os };
        assert!(key.contains(expected_family), "{key} does not match os {os}");
    }
}
