pub mod curseforge;
pub mod curseforge_key;
pub mod modrinth;

/// Both source APIs emit UTC timestamps with optional fractional seconds.
/// Normalize their suffixes so whole seconds precede fractions and .10 == .1.
pub(crate) fn updated_key(timestamp: &str) -> &str {
    let timestamp = timestamp.trim_end_matches('Z');
    if timestamp.contains('.') {
        timestamp.trim_end_matches('0').trim_end_matches('.')
    } else {
        timestamp
    }
}
