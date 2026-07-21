//! Locating usable Java runtimes on the machine.
//!
//! Modern Minecraft (1.17+) needs Java 17+, 1.20.5+ needs Java 21, while old
//! versions (<=1.16) run on Java 8. We therefore can't assume a single JVM:
//! we scan the well-known install locations (including the runtimes the
//! official launcher downloads) and pick one matching the version's required
//! major.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::Serialize;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaRuntime {
    pub path: String,
    pub major_version: u32,
    /// Full first line of `java -version` for display.
    pub version_string: String,
}

static DETECTED_CACHE: OnceLock<Vec<JavaRuntime>> = OnceLock::new();

/// Discover Java runtimes from JAVA_HOME, PATH, and common install dirs.
///
/// Cached for the process lifetime — every candidate path spawns its own
/// `java -version` (see `probe_java`), and with several vendor dirs each
/// holding a handful of JDKs, a full scan is a dozen-plus process spawns.
/// That's cheap once; done again on every Settings-page visit or instance
/// open, it was the actual cause of "opening Settings takes a few seconds."
/// Installing a new JDK mid-session won't show up until restart — the same
/// tradeoff other launchers make, and a fine one since this only changes
/// when the user goes install a JDK, not during normal use.
pub fn detect_java_runtimes() -> Vec<JavaRuntime> {
    DETECTED_CACHE
        .get_or_init(|| {
            let mut seen: Vec<PathBuf> = Vec::new();
            let mut runtimes: Vec<JavaRuntime> = Vec::new();

            for candidate in candidate_java_paths() {
                let canonical =
                    std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
                if seen.iter().any(|p| p == &canonical) {
                    continue;
                }
                seen.push(canonical);

                if let Some(rt) = probe_java(&candidate) {
                    runtimes.push(rt);
                }
            }

            runtimes.sort_by(|a, b| a.major_version.cmp(&b.major_version));
            runtimes
        })
        .clone()
}

/// Pick an installed runtime that is *at least* `required_major`, preferring an
/// exact match, then the smallest version still new enough. Returns `None` when
/// nothing installed is new enough (the caller then auto-downloads one) — we
/// deliberately never fall back to an older Java, since that fails at launch.
pub fn select_at_least(runtimes: &[JavaRuntime], required_major: u32) -> Option<JavaRuntime> {
    if let Some(exact) = runtimes.iter().find(|r| r.major_version == required_major) {
        return Some(exact.clone());
    }
    runtimes
        .iter()
        .filter(|r| r.major_version >= required_major)
        .min_by_key(|r| r.major_version)
        .cloned()
}

/// Probe a single Java executable path for its major version.
pub fn probe_major(path: &str) -> Option<u32> {
    probe_java(Path::new(path)).map(|r| r.major_version)
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "java.exe"
    } else {
        "java"
    }
}

fn candidate_java_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let exe = exe_name();

    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        out.push(PathBuf::from(&java_home).join("bin").join(exe));
    }

    // Bare `java` on PATH — resolved by the OS.
    out.push(PathBuf::from(exe));

    // Common install roots that contain one JDK/JRE folder per version.
    for root in java_install_roots() {
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let bin = entry.path().join("bin").join(exe);
                if bin.exists() {
                    out.push(bin);
                }
            }
        }
    }

    out
}

fn java_install_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();

    if cfg!(windows) {
        for base in [
            "C:/Program Files/Java",
            "C:/Program Files/Eclipse Adoptium",
            "C:/Program Files/Microsoft",
            "C:/Program Files/Zulu",
            "C:/Program Files/Amazon Corretto",
            "C:/Program Files (x86)/Java",
        ] {
            roots.push(PathBuf::from(base));
        }

        // Runtimes downloaded by the official Minecraft launcher.
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let base = PathBuf::from(&local).join(
                "Packages/Microsoft.4297127D64EC6_8wekyb3d8bbwe/LocalCache/Local/runtime",
            );
            // runtime/java-runtime-*/windows-x64/java-runtime-*/bin
            collect_launcher_runtimes(&base, &mut roots);
        }
        roots.push(PathBuf::from(
            "C:/Program Files (x86)/Minecraft Launcher/runtime",
        ));
    } else {
        for base in ["/usr/lib/jvm", "/usr/local/opt", "/opt"] {
            roots.push(PathBuf::from(base));
        }
    }

    roots
}

/// The launcher runtime tree nests one extra level (`java-runtime-gamma/windows-x64/java-runtime-gamma`),
/// so descend two levels and treat those as install roots.
fn collect_launcher_runtimes(base: &Path, roots: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(sub) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for os_entry in sub.flatten() {
            roots.push(os_entry.path());
        }
    }
}

fn probe_java(path: &Path) -> Option<JavaRuntime> {
    let mut cmd = Command::new(path);
    cmd.arg("-version");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let output = cmd.output().ok()?;
    // `java -version` prints to stderr.
    let text = String::from_utf8_lossy(&output.stderr);
    let first_line = text.lines().next().unwrap_or("").trim().to_string();
    let major = parse_major_version(&text)?;
    Some(JavaRuntime {
        path: path.to_string_lossy().to_string(),
        major_version: major,
        version_string: first_line,
    })
}

/// Parse the major version out of a `java -version` banner.
/// Handles both `1.8.0_461` (-> 8) and `17.0.2` / `21` (-> 17 / 21).
fn parse_major_version(banner: &str) -> Option<u32> {
    let quote_start = banner.find('"')?;
    let rest = &banner[quote_start + 1..];
    let quote_end = rest.find('"')?;
    let version = &rest[..quote_end];

    let mut parts = version.split('.');
    let first = parts.next()?;
    if first == "1" {
        // Legacy scheme: 1.8.0_x -> major 8.
        parts.next()?.parse().ok()
    } else {
        // Strip any pre-release / build suffix.
        let numeric: String = first.chars().take_while(|c| c.is_ascii_digit()).collect();
        numeric.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_major_version;

    #[test]
    fn parses_legacy_and_modern() {
        assert_eq!(
            parse_major_version("java version \"1.8.0_461\""),
            Some(8)
        );
        assert_eq!(
            parse_major_version("openjdk version \"17.0.2\" 2022-01-18"),
            Some(17)
        );
        assert_eq!(parse_major_version("openjdk version \"21\" 2023"), Some(21));
    }
}
