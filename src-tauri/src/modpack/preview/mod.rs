mod curseforge;
mod modrinth;

pub use curseforge::preview_curseforge_modpack;
pub use modrinth::preview_modrinth_modpack;

use crate::dto::project_detail::{ModpackContentCounts, ModpackContentItem, ModpackContentKind};

pub fn count_by_kind(items: &[ModpackContentItem]) -> ModpackContentCounts {
    let mut counts = ModpackContentCounts {
        mods: 0,
        datapacks: 0,
        resourcepacks: 0,
        shaders: 0,
        worlds: 0,
        other: 0,
    };
    for item in items {
        match item.kind {
            ModpackContentKind::Mod => counts.mods += 1,
            ModpackContentKind::Datapack => counts.datapacks += 1,
            ModpackContentKind::Resourcepack => counts.resourcepacks += 1,
            ModpackContentKind::Shader => counts.shaders += 1,
            ModpackContentKind::World => counts.worlds += 1,
            ModpackContentKind::Other => counts.other += 1,
        }
    }
    counts
}

pub fn kind_from_path(path: &str) -> ModpackContentKind {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if normalized.starts_with("mods/") {
        return ModpackContentKind::Mod;
    }
    if normalized.starts_with("datapacks/") {
        return ModpackContentKind::Datapack;
    }
    if normalized.starts_with("resourcepacks/") {
        return ModpackContentKind::Resourcepack;
    }
    if normalized.starts_with("shaderpacks/") {
        return ModpackContentKind::Shader;
    }
    if normalized.starts_with("saves/") {
        return ModpackContentKind::World;
    }
    ModpackContentKind::Other
}

pub fn file_name_from_path(path: &str) -> String {
    path.replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}
