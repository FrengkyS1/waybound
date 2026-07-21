use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::dto::project_detail::ActivityLogEntry;

const APP_DIR: &str = "dev.waybound";
const LOG_FILE: &str = "activity.log";

pub fn append_log(message: &str, level: &str, project_uid: Option<&str>) {
    let Ok(path) = log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let timestamp = crate::db::now_unix();
    // The log format is one entry per line, fields separated by tabs. A
    // value containing a raw `\t` or `\n` (error text from a network
    // response, for instance, has both) would otherwise misalign fields or
    // split into an unparsable continuation line that read_logs silently
    // drops. Every variable-length field gets sanitized, not just message —
    // project_uid is currently always None at every call site, but a future
    // caller passing a real mod/project slug would reopen the same gap this
    // was written to close.
    let uid = sanitize_field(project_uid.unwrap_or(""));
    let message = sanitize_field(message);
    let line = format!("{timestamp}\t{level}\t{uid}\t{message}\n");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

fn sanitize_field(value: &str) -> std::borrow::Cow<'_, str> {
    if value.contains(['\t', '\n', '\r']) {
        std::borrow::Cow::Owned(
            value
                .chars()
                .map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c })
                .collect(),
        )
    } else {
        std::borrow::Cow::Borrowed(value)
    }
}

pub fn read_logs(limit: usize) -> Vec<ActivityLogEntry> {
    let Ok(path) = log_path() else {
        return Vec::new();
    };
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .rev()
        .filter_map(parse_line)
        .take(limit)
        .collect()
}

fn parse_line(line: &str) -> Option<ActivityLogEntry> {
    let mut parts = line.splitn(4, '\t');
    let timestamp = parts.next()?.parse().ok()?;
    let level = parts.next()?.to_string();
    let project_uid = parts.next().filter(|v| !v.is_empty()).map(str::to_string);
    let message = parts.next()?.to_string();
    Some(ActivityLogEntry {
        timestamp,
        level,
        message,
        project_uid,
    })
}

fn log_path() -> Result<PathBuf, ()> {
    Ok(dirs::data_dir()
        .ok_or(())?
        .join(APP_DIR)
        .join(LOG_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_with_embedded_tab_and_newline_stays_one_parsable_line() {
        let message = "Modrinth returned an error\n\tHTTP 502 Bad Gateway";
        let sanitized = sanitize_field(message);
        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains('\t'));

        let line = format!("1700000000\tinfo\tsome-uid\t{sanitized}");
        assert_eq!(line.lines().count(), 1, "must stay a single line: {line}");

        let parsed = parse_line(&line).expect("line must parse back");
        assert_eq!(parsed.project_uid.as_deref(), Some("some-uid"));
        assert!(parsed.message.contains("HTTP 502 Bad Gateway"));
    }

    #[test]
    fn plain_message_is_not_reallocated() {
        assert!(matches!(
            sanitize_field("no special characters here"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
