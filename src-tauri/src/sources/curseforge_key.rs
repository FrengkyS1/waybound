/// Normalize a CurseForge API key value (bcrypt-style, often starts with `$2a$10$`).
pub fn normalize_curseforge_api_key(raw: &str) -> String {
    let mut key = raw.trim().to_string();

    key = key
        .trim_matches('\u{feff}')
        .trim_matches('\u{200b}')
        .trim_matches('\u{2060}')
        .to_string();

    if (key.starts_with('"') && key.ends_with('"'))
        || (key.starts_with('\'') && key.ends_with('\''))
    {
        key = key[1..key.len() - 1].trim().to_string();
    }

    // Common when copied from Docker Compose `.env` files.
    if key.contains("$$") {
        key = key.replace("$$", "$");
    }

    key.trim().to_string()
}

/// Encode a key for Docker Compose `.env` files — each `$` must be doubled.
/// See https://github.com/itzg/docker-minecraft-server/discussions/2588
#[allow(dead_code)]
pub fn encode_curseforge_api_key_for_docker_env(key: &str) -> String {
    normalize_curseforge_api_key(key).replace('$', "$$")
}

/// Warnings when `.env` may not work with Docker Compose variable substitution.
pub fn docker_env_format_warnings(content: &str) -> Vec<String> {
    let mut warnings = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some(raw_value) = parse_env_assignment(line) else {
            continue;
        };

        let trimmed = raw_value.trim().trim_matches('"').trim_matches('\'');
        if is_compose_placeholder(trimmed) {
            continue;
        }

        if trimmed.starts_with("$2a$") && !trimmed.starts_with("$$") {
            warnings.push(
                "Note: `.env` uses single `$` in CF_API_KEY. If Docker fails with forbidden, double each `$`: CF_API_KEY=$$2a$$10$$... (https://github.com/itzg/docker-minecraft-server/discussions/2588)".to_string(),
            );
        }

        let normalized = normalize_curseforge_api_key(trimmed);
        if !normalized.starts_with("$2a$") && trimmed.contains('$') {
            warnings.push(format!(
                "CF_API_KEY after normalization does not start with \"$2a$\" (got prefix \"{}\") — the key may already be corrupted by Docker.",
                &normalized.chars().take(8).collect::<String>()
            ));
        }
    }

    warnings
}

const ENV_KEY_NAMES: [&str; 2] = ["CF_API_KEY", "CURSEFORGE_API_KEY"];

/// Extract a CurseForge key from a raw paste: plain key, `.env` line, or multi-line `.env` / compose file.
pub fn extract_curseforge_api_key(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Paste is empty.".to_string());
    }

    if let Some(value) = parse_env_assignment(trimmed) {
        return finalize_extracted_value(&value);
    }

    if looks_like_bare_key(trimmed) && !trimmed.contains('\n') {
        return Ok(normalize_curseforge_api_key(trimmed));
    }

    let mut found_placeholder = false;
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(value) = parse_env_assignment(line) {
            if is_compose_placeholder(&value) {
                found_placeholder = true;
                continue;
            }
            return finalize_extracted_value(&value);
        }

        if line.contains("CF_API_KEY:") && line.contains("${CF_API_KEY}") {
            found_placeholder = true;
        }
    }

    if found_placeholder {
        return Err(
            "That looks like docker-compose.yml (`CF_API_KEY: '${CF_API_KEY}'`). Paste the line from your `.env` file instead, e.g. `CF_API_KEY=$2a$10$...` (use `$$` for each `$` if Docker escaped it).".to_string(),
        );
    }

    Err(
        "Could not find CF_API_KEY in the pasted text. Paste your `.env` line or the raw key.".to_string(),
    )
}

/// Read `CF_API_KEY` from a `.env` file (same format Docker Compose uses).
pub fn read_curseforge_api_key_from_env_file(content: &str) -> Result<String, String> {
    let mut found_placeholder = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(value) = parse_env_assignment(line) {
            if is_compose_placeholder(&value) {
                found_placeholder = true;
                continue;
            }
            return finalize_extracted_value(&value);
        }
    }

    if found_placeholder {
        return Err(
            "`.env` file contains `${CF_API_KEY}` placeholder, not the actual key. Set CF_API_KEY=$2a$10$... in the file.".to_string(),
        );
    }

    Err("No CF_API_KEY= line found in `.env` file.".to_string())
}

/// Resolve `CF_API_KEY` / `CURSEFORGE_API_KEY` from the process environment.
pub fn curseforge_api_key_from_environment() -> Option<(String, &'static str)> {
    for name in ENV_KEY_NAMES {
        if let Ok(raw) = std::env::var(name) {
            let key = normalize_curseforge_api_key(&raw);
            if !key.is_empty() {
                return Some((key, name));
            }
        }
    }
    None
}

fn finalize_extracted_value(value: &str) -> Result<String, String> {
    if is_compose_placeholder(value) {
        return Err(
            "That is a Docker Compose placeholder (`${CF_API_KEY}`), not the key itself. Open your `.env` file next to docker-compose.yml.".to_string(),
        );
    }

    let key = normalize_curseforge_api_key(value);
    if key.is_empty() {
        return Err("CF_API_KEY value is empty.".to_string());
    }

    Ok(key)
}

fn parse_env_assignment(line: &str) -> Option<String> {
    for name in ENV_KEY_NAMES {
        let prefix = format!("{name}=");
        if let Some(rest) = line.strip_prefix(&prefix) {
            return Some(rest.to_string());
        }
    }
    None
}

fn is_compose_placeholder(value: &str) -> bool {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    trimmed == "${CF_API_KEY}" || trimmed.contains("${CF_API_KEY}")
}

fn looks_like_bare_key(value: &str) -> bool {
    value.starts_with("$2a$") && value.len() >= 50
}

#[cfg(test)]
mod tests {
    use super::{
        docker_env_format_warnings, encode_curseforge_api_key_for_docker_env,
        extract_curseforge_api_key, normalize_curseforge_api_key, read_curseforge_api_key_from_env_file,
    };

    #[test]
    fn strips_quotes_and_docker_escaping() {
        let raw = "\"$$2a$$10$$abc123\"";
        assert_eq!(normalize_curseforge_api_key(raw), "$2a$10$abc123");
    }

    #[test]
    fn toml_roundtrip_preserves_bcrypt_key() {
        use serde::{Deserialize, Serialize};

        #[derive(Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Config {
            curseforge_api_key: Option<String>,
        }

        let key = "$2a$10$abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0";
        let raw = toml::to_string(&Config {
            curseforge_api_key: Some(key.to_string()),
        })
        .unwrap();
        let parsed: Config = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.curseforge_api_key.as_deref(), Some(key));
    }

    #[test]
    fn extracts_docker_env_line() {
        let line = "CF_API_KEY=$$2a$$10$$abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0";
        let key = extract_curseforge_api_key(line).unwrap();
        assert!(key.starts_with("$2a$10$"));
        assert_eq!(key.len(), 60);
    }

    #[test]
    fn encodes_docker_env_dollars() {
        let key = "$2a$10$abc";
        assert_eq!(
            encode_curseforge_api_key_for_docker_env(key),
            "$$2a$$10$$abc"
        );
    }

    #[test]
    fn warns_on_unescaped_docker_env() {
        let content = "CF_API_KEY=$2a$10$abcdefghijklmnopqrstuvwxyz0123456789ABCD\n";
        let warnings = docker_env_format_warnings(content);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("If Docker fails"));
    }

    #[test]
    fn no_warn_when_docker_escaped() {
        let content = "CF_API_KEY=$$2a$$10$$abcdefghijklmnopqrstuvwxyz0123456789ABCD\n";
        let warnings = docker_env_format_warnings(content);
        assert!(warnings.is_empty());
    }

    #[test]
    fn reads_dotenv_file() {
        let content = "# docker env\nCF_API_KEY=$2a$10$abcdefghijklmnopqrstuvwxyz0123456789ABCD\nMEMORY=16G\n";
        let key = read_curseforge_api_key_from_env_file(content).unwrap();
        assert!(key.starts_with("$2a$10$"));
    }

    #[test]
    fn rejects_compose_placeholder() {
        let compose = "CF_API_KEY: '${CF_API_KEY}'";
        assert!(extract_curseforge_api_key(compose).is_err());
    }
}
