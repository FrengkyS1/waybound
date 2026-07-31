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

#[cfg(test)]
mod kind_tests {
    use super::*;

    fn item(kind: ModpackContentKind) -> ModpackContentItem {
        ModpackContentItem {
            id: "id".to_string(),
            name: "name".to_string(),
            file_name: "file.jar".to_string(),
            author: None,
            kind,
            required: true,
            env_client: None,
            env_server: None,
        }
    }

    #[test]
    fn kind_is_classified_from_the_top_level_folder() {
        assert_eq!(kind_from_path("mods/jei.jar"), ModpackContentKind::Mod);
        assert_eq!(kind_from_path("datapacks/pack.zip"), ModpackContentKind::Datapack);
        assert_eq!(kind_from_path("resourcepacks/faithful.zip"), ModpackContentKind::Resourcepack);
        assert_eq!(kind_from_path("shaderpacks/bsl.zip"), ModpackContentKind::Shader);
        assert_eq!(kind_from_path("saves/MyWorld/level.dat"), ModpackContentKind::World);
        assert_eq!(kind_from_path("config/foo.toml"), ModpackContentKind::Other);
        assert_eq!(kind_from_path("jei.jar"), ModpackContentKind::Other, "bare filename has no folder to classify by");
    }

    #[test]
    fn kind_normalizes_separators_and_case() {
        assert_eq!(kind_from_path("Mods\\JEI.jar"), ModpackContentKind::Mod);
        assert_eq!(kind_from_path("ShaderPacks\\bsl.zip"), ModpackContentKind::Shader);
        assert_eq!(kind_from_path("mods/nested/deep.jar"), ModpackContentKind::Mod);
        // A folder that merely starts with the same letters is not a match.
        assert_eq!(kind_from_path("modsomething/x.jar"), ModpackContentKind::Other);
    }

    #[test]
    fn counts_aggregate_per_kind() {
        let items = vec![
            item(ModpackContentKind::Mod),
            item(ModpackContentKind::Mod),
            item(ModpackContentKind::Shader),
            item(ModpackContentKind::World),
            item(ModpackContentKind::Other),
        ];
        let counts = count_by_kind(&items);
        assert_eq!(counts.mods, 2);
        assert_eq!(counts.shaders, 1);
        assert_eq!(counts.worlds, 1);
        assert_eq!(counts.other, 1);
        assert_eq!(counts.datapacks, 0);
        assert_eq!(counts.resourcepacks, 0);

        let empty = count_by_kind(&[]);
        assert_eq!(empty.mods, 0);
        assert_eq!(empty.other, 0);
    }

    #[test]
    fn file_name_is_taken_from_the_last_path_segment() {
        assert_eq!(file_name_from_path("mods/jei.jar"), "jei.jar");
        assert_eq!(file_name_from_path("mods\\sub\\jei.jar"), "jei.jar");
        assert_eq!(file_name_from_path("jei.jar"), "jei.jar");
        assert_eq!(file_name_from_path("mods/"), "", "a directory entry yields no file name");
    }
}
