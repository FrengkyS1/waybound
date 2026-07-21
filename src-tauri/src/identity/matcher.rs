use crate::dto::ModSummary;

/// Merge Modrinth + CurseForge hits into deduplicated rows (in-memory; SQLite in step 6).
pub fn dedupe_mods(hits: Vec<ModSummary>) -> Vec<ModSummary> {
    let mut merged: Vec<ModSummary> = Vec::new();

    for hit in hits {
        if let Some(index) = find_match_index(&merged, &hit) {
            merge_into(&mut merged[index], hit);
        } else {
            merged.push(hit);
        }
    }

    merged
}

fn find_match_index(existing: &[ModSummary], candidate: &ModSummary) -> Option<usize> {
    existing.iter().position(|item| mods_match(item, candidate))
}

fn mods_match(a: &ModSummary, b: &ModSummary) -> bool {
    if a.project_type != b.project_type {
        return false;
    }

    if slug_match(&a.slug, &b.slug) {
        return true;
    }

    if name_match(&a.name, &b.name) && author_match(&a.author, &b.author) {
        return true;
    }

    normalize_name(&a.name) == normalize_name(&b.name)
}

fn merge_into(target: &mut ModSummary, incoming: ModSummary) {
    for source in incoming.sources {
        if !target.sources.contains(&source) {
            target.sources.push(source);
        }
    }

    if target.curseforge_id.is_none() {
        target.curseforge_id = incoming.curseforge_id;
    }
    if target.modrinth_id.is_none() {
        target.modrinth_id = incoming.modrinth_id;
    }

    if target.description.len() < incoming.description.len() {
        target.description = incoming.description;
    }

    if target.icon_url.is_none() {
        target.icon_url = incoming.icon_url;
    }

    target.downloads = target.downloads.max(incoming.downloads);

    for loader in incoming.loaders {
        if !target.loaders.contains(&loader) {
            target.loaders.push(loader);
        }
    }

    if incoming.updated_at > target.updated_at {
        target.updated_at = incoming.updated_at;
    }

    target.uid = stable_uid(target);
}

fn stable_uid(item: &ModSummary) -> String {
    if let Some(id) = item.modrinth_id.as_ref() {
        return format!("mod:{id}");
    }
    if let Some(id) = item.curseforge_id {
        return format!("mod:cf:{id}");
    }
    format!("mod:slug:{}", normalize_slug(&item.slug))
}

fn slug_match(a: &str, b: &str) -> bool {
    let na = normalize_slug(a);
    let nb = normalize_slug(b);
    !na.is_empty() && na == nb
}

fn name_match(a: &str, b: &str) -> bool {
    let na = normalize_name(a);
    let nb = normalize_name(b);
    !na.is_empty() && na == nb
}

fn author_match(a: &str, b: &str) -> bool {
    let na = normalize_name(a);
    let nb = normalize_name(b);
    na.is_empty() || nb.is_empty() || na == nb
}

fn normalize_slug(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn normalize_name(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::dedupe_mods;
    use crate::dto::{ContentType, ModSource, ModSummary};

    fn sample(slug: &str, name: &str, source: ModSource) -> ModSummary {
        ModSummary {
            uid: format!("{slug}-uid"),
            slug: slug.to_string(),
            name: name.to_string(),
            description: String::new(),
            author: "Dev".to_string(),
            icon_url: None,
            downloads: 100,
            project_type: ContentType::Mod,
            loaders: vec![],
            sources: vec![source],
            updated_at: "2024-01-01".to_string(),
            curseforge_id: if source == ModSource::Curseforge {
                Some(1)
            } else {
                None
            },
            modrinth_id: if source == ModSource::Modrinth {
                Some("abc".to_string())
            } else {
                None
            },
        }
    }

    #[test]
    fn merges_same_slug_from_both_sources() {
        let hits = vec![
            sample("sodium", "Sodium", ModSource::Modrinth),
            sample("sodium", "Sodium", ModSource::Curseforge),
        ];
        let merged = dedupe_mods(hits);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].sources.contains(&ModSource::Modrinth));
        assert!(merged[0].sources.contains(&ModSource::Curseforge));
    }
}
