use crate::dto::instance::{InstalledMod, InstanceSummary};
use crate::dto::{ModLoader, ModSource};
use rusqlite::{params, OptionalExtension, Row};

use super::{DbError, Database};

impl Database {
    pub fn list_instances(&self) -> Result<Vec<InstanceSummary>, DbError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT i.id, i.name, i.minecraft_version, i.loader, i.loader_version,
                    i.created_at, i.root_path, i.icon, i.last_played, i.total_play_seconds
             FROM instances i
             ORDER BY i.created_at DESC",
        )?;

        let rows = stmt.query_map([], map_instance_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    }

    pub fn get_instance(&self, id: &str) -> Result<Option<InstanceSummary>, DbError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT i.id, i.name, i.minecraft_version, i.loader, i.loader_version,
                    i.created_at, i.root_path, i.icon, i.last_played, i.total_play_seconds
             FROM instances i
             WHERE i.id = ?1",
        )?;

        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(map_instance_row(row)?));
        }
        Ok(None)
    }

    pub fn insert_instance(&self, instance: &InstanceSummary) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO instances (id, name, minecraft_version, loader, loader_version, root_path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                instance.id,
                instance.name,
                instance.minecraft_version,
                loader_to_str(instance.loader),
                instance.loader_version,
                instance.root_path,
                instance.created_at as i64,
            ],
        )?;
        Ok(())
    }

    pub fn delete_instance_mod_by_file(
        &self,
        instance_id: &str,
        file_name: &str,
    ) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM instance_mods WHERE instance_id = ?1 AND file_name = ?2",
            params![instance_id, file_name],
        )?;
        Ok(())
    }

    pub fn rename_instance(&self, id: &str, name: &str) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instances SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    pub fn set_instance_icon(&self, id: &str, icon: Option<&str>) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instances SET icon = ?2 WHERE id = ?1",
            params![id, icon],
        )?;
        Ok(())
    }

    pub fn get_instance_launch_config(
        &self,
        id: &str,
    ) -> Result<crate::dto::instance::InstanceLaunchConfig, DbError> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT java_path, max_memory_mb, jvm_args FROM instances WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(crate::dto::instance::InstanceLaunchConfig {
                java_path: row.get(0)?,
                max_memory_mb: row.get::<_, Option<i64>>(1)?.map(|v| v as u32),
                jvm_args: row.get(2)?,
            })
        } else {
            Ok(Default::default())
        }
    }

    pub fn set_instance_launch_config(
        &self,
        id: &str,
        config: &crate::dto::instance::InstanceLaunchConfig,
    ) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instances SET java_path = ?2, max_memory_mb = ?3, jvm_args = ?4 WHERE id = ?1",
            params![
                id,
                config.java_path,
                config.max_memory_mb.map(|v| v as i64),
                config.jvm_args,
            ],
        )?;
        Ok(())
    }

    /// Stamp the instance as launched now.
    pub fn mark_played(&self, id: &str) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instances SET last_played = ?2 WHERE id = ?1",
            params![id, super::now_unix() as i64],
        )?;
        Ok(())
    }

    /// Add elapsed play time (seconds) to the running total.
    pub fn add_play_time(&self, id: &str, seconds: u64) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instances SET total_play_seconds = total_play_seconds + ?2 WHERE id = ?1",
            params![id, seconds as i64],
        )?;
        Ok(())
    }

    /// Copies all tracked mod rows from one instance to another, rewriting the
    /// stored absolute file paths from the old instance root to the new one.
    pub fn duplicate_instance_mods(
        &self,
        from_id: &str,
        to_id: &str,
        old_root: &str,
        new_root: &str,
    ) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO instance_mods (instance_id, mod_uid, mod_name, source, file_name, file_path, installed_at, icon_url)
             SELECT ?2, mod_uid, mod_name, source, file_name, REPLACE(file_path, ?3, ?4), installed_at, icon_url
             FROM instance_mods WHERE instance_id = ?1",
            params![from_id, to_id, old_root, new_root],
        )?;
        Ok(())
    }

    pub fn delete_instance(&self, id: &str) -> Result<bool, DbError> {
        let conn = self.conn()?;
        let deleted = conn.execute("DELETE FROM instances WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    }

    pub fn list_instance_mods(&self, instance_id: &str) -> Result<Vec<InstalledMod>, DbError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, instance_id, mod_uid, mod_name, source, file_name, installed_at, icon_url
             FROM instance_mods
             WHERE instance_id = ?1
             ORDER BY mod_name ASC",
        )?;

        let rows = stmt.query_map(params![instance_id], map_installed_mod_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    }

    pub fn insert_instance_mod(
        &self,
        instance_id: &str,
        mod_uid: &str,
        mod_name: &str,
        source: ModSource,
        file_name: &str,
        file_path: &str,
        icon_url: Option<&str>,
    ) -> Result<InstalledMod, DbError> {
        let conn = self.conn()?;
        let installed_at = super::now_unix() as i64;
        conn.execute(
            "INSERT INTO instance_mods (instance_id, mod_uid, mod_name, source, file_name, file_path, installed_at, icon_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(instance_id, mod_uid) DO UPDATE SET
               mod_name = excluded.mod_name,
               source = excluded.source,
               file_name = excluded.file_name,
               file_path = excluded.file_path,
               installed_at = excluded.installed_at,
               icon_url = COALESCE(excluded.icon_url, instance_mods.icon_url)",
            params![
                instance_id,
                mod_uid,
                mod_name,
                source_to_str(source),
                file_name,
                file_path,
                installed_at,
                icon_url,
            ],
        )?;

        let id = conn.last_insert_rowid();
        Ok(InstalledMod {
            id,
            instance_id: instance_id.to_string(),
            mod_uid: mod_uid.to_string(),
            mod_name: mod_name.to_string(),
            source,
            file_name: file_name.to_string(),
            installed_at: installed_at as u64,
            icon_url: icon_url.map(str::to_string),
        })
    }

    /// Same upsert as `insert_instance_mod`, but for many mods in one
    /// transaction — used when scanning a mods folder after a modpack
    /// install (can be a few hundred jars). WAL mode already makes each
    /// individual commit cheap, but batching still avoids a few hundred
    /// separate implicit transactions for what's conceptually one operation.
    pub fn insert_instance_mods_batch(
        &self,
        instance_id: &str,
        mods: &[(String, String, ModSource, String, String, Option<String>)],
    ) -> Result<(), DbError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let installed_at = super::now_unix() as i64;
        for (mod_uid, mod_name, source, file_name, file_path, icon_url) in mods {
            tx.execute(
                "INSERT INTO instance_mods (instance_id, mod_uid, mod_name, source, file_name, file_path, installed_at, icon_url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(instance_id, mod_uid) DO UPDATE SET
                   mod_name = excluded.mod_name,
                   source = excluded.source,
                   file_name = excluded.file_name,
                   file_path = excluded.file_path,
                   installed_at = excluded.installed_at,
                   icon_url = COALESCE(excluded.icon_url, instance_mods.icon_url)",
                params![
                    instance_id,
                    mod_uid,
                    mod_name,
                    source_to_str(*source),
                    file_name,
                    file_path,
                    installed_at,
                    icon_url,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Backfills an icon onto an already-tracked row that doesn't have one
    /// yet — e.g. a mod first synced by an older Modrinth import, before
    /// icon lookup existed for that path, catching up once a later
    /// sync/re-import can actually resolve one. Never overwrites an icon
    /// already on record.
    pub fn update_instance_mod_icon(&self, instance_id: &str, mod_uid: &str, icon_url: &str) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instance_mods SET icon_url = ?1 WHERE instance_id = ?2 AND mod_uid = ?3 AND icon_url IS NULL",
            params![icon_url, instance_id, mod_uid],
        )?;
        Ok(())
    }

    /// Just the icon for one mod file, without materializing every other
    /// mod row in the instance. The Content tab's per-row metadata fetch
    /// used to call `list_instance_mods` (the whole table) here — fine for
    /// one row, but O(n) work repeated for every one of a few hundred rows
    /// as they scroll into view made a big instance's Content tab visibly
    /// slow to fill in.
    pub fn get_instance_mod_icon_by_file(
        &self,
        instance_id: &str,
        file_name: &str,
    ) -> Result<Option<String>, DbError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT icon_url FROM instance_mods WHERE instance_id = ?1 AND file_name = ?2 LIMIT 1",
            params![instance_id, file_name],
            |row| row.get(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(DbError::from)
    }

    pub fn get_instance_mod(&self, instance_id: &str, mod_uid: &str) -> Result<Option<(InstalledMod, String)>, DbError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, instance_id, mod_uid, mod_name, source, file_name, installed_at, icon_url, file_path
             FROM instance_mods
             WHERE instance_id = ?1 AND mod_uid = ?2",
        )?;

        let mut rows = stmt.query(params![instance_id, mod_uid])?;
        if let Some(row) = rows.next()? {
            let file_path: String = row.get(8)?;
            return Ok(Some((map_installed_mod_row_full(row)?, file_path)));
        }
        Ok(None)
    }

    pub fn delete_instance_mod(&self, instance_id: &str, mod_uid: &str) -> Result<Option<String>, DbError> {
        let Some((_, file_path)) = self.get_instance_mod(instance_id, mod_uid)? else {
            return Ok(None);
        };

        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM instance_mods WHERE instance_id = ?1 AND mod_uid = ?2",
            params![instance_id, mod_uid],
        )?;

        Ok(Some(file_path))
    }

    /// Every cached jar/zip metadata entry for one instance, in a single
    /// query — the Content tab's initial load joins this against the disk
    /// scan by (category, file_name) so a file whose size+mtime still match
    /// what was cached needs no jar/zip parsing at all to show its name and
    /// icon.
    pub fn get_content_meta_cache(&self, instance_id: &str) -> Result<Vec<CachedContentMeta>, DbError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT category, file_name, size_bytes, mtime_unix, name, icon
             FROM content_meta_cache WHERE instance_id = ?1",
        )?;
        let rows = stmt.query_map(params![instance_id], |row| {
            Ok(CachedContentMeta {
                category: row.get(0)?,
                file_name: row.get(1)?,
                size_bytes: row.get::<_, i64>(2)? as u64,
                mtime_unix: row.get(3)?,
                name: row.get(4)?,
                icon: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    }

    /// Drops one file's cached metadata — used when something *other* than
    /// the file itself changed what its name/icon should be (e.g. its
    /// `instance_mods.icon_url` just got backfilled), since the cache is
    /// fingerprinted by the file's own size+mtime and has no way to know
    /// that kind of change happened. The next `list_instance_content` call
    /// treats it as unresolved and re-parses it fresh.
    pub fn delete_content_meta_cache(&self, instance_id: &str, category: &str, file_name: &str) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM content_meta_cache WHERE instance_id = ?1 AND category = ?2 AND file_name = ?3",
            params![instance_id, category, file_name],
        )?;
        Ok(())
    }

    /// Records one file's parsed name/icon (or the fact that parsing found
    /// neither — still worth caching, so a jar with no embedded icon isn't
    /// re-opened forever trying to find one) against its current size+mtime
    /// fingerprint.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_content_meta_cache(
        &self,
        instance_id: &str,
        category: &str,
        file_name: &str,
        size_bytes: u64,
        mtime_unix: i64,
        name: Option<&str>,
        icon: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO content_meta_cache (instance_id, category, file_name, size_bytes, mtime_unix, name, icon)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(instance_id, category, file_name) DO UPDATE SET
               size_bytes = excluded.size_bytes,
               mtime_unix = excluded.mtime_unix,
               name = excluded.name,
               icon = excluded.icon",
            params![instance_id, category, file_name, size_bytes as i64, mtime_unix, name, icon],
        )?;
        Ok(())
    }
}

/// One cached jar/zip metadata row, fingerprinted by the file's size+mtime at
/// the time it was parsed.
pub struct CachedContentMeta {
    pub category: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub mtime_unix: i64,
    pub name: Option<String>,
    pub icon: Option<String>,
}

fn map_instance_row(row: &Row<'_>) -> Result<InstanceSummary, rusqlite::Error> {
    let loader_raw: String = row.get(3)?;
    let root_path: String = row.get(6)?;
    Ok(InstanceSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        minecraft_version: row.get(2)?,
        loader: parse_loader(&loader_raw),
        loader_version: row.get(4)?,
        created_at: row.get::<_, i64>(5)? as u64,
        mod_count: count_mod_files(&root_path),
        root_path,
        icon: row.get(7)?,
        last_played: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
        total_play_seconds: row.get::<_, i64>(9)? as u64,
    })
}

/// Counts mod files actually on disk rather than rows in `instance_mods`, so
/// this always agrees with the Content tab's own directory scan — the DB
/// table only tracks mods installed via Browse and can drift (modpack-dropped
/// files, manual deletes, failed cleanup on remove) from what's really there.
fn count_mod_files(root_path: &str) -> u32 {
    std::fs::read_dir(std::path::Path::new(root_path).join("mods"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_file())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".jar"))
                .count() as u32
        })
        .unwrap_or(0)
}

fn map_installed_mod_row(row: &Row<'_>) -> Result<InstalledMod, rusqlite::Error> {
    map_installed_mod_row_full(row)
}

fn map_installed_mod_row_full(row: &Row<'_>) -> Result<InstalledMod, rusqlite::Error> {
    let source_raw: String = row.get(4)?;
    Ok(InstalledMod {
        id: row.get(0)?,
        instance_id: row.get(1)?,
        mod_uid: row.get(2)?,
        mod_name: row.get(3)?,
        source: parse_source(&source_raw),
        file_name: row.get(5)?,
        installed_at: row.get::<_, i64>(6)? as u64,
        icon_url: row.get(7)?,
    })
}

fn loader_to_str(loader: ModLoader) -> &'static str {
    match loader {
        ModLoader::Fabric => "fabric",
        ModLoader::Forge => "forge",
        ModLoader::NeoForge => "neoforge",
        ModLoader::Quilt => "quilt",
        ModLoader::Vanilla => "vanilla",
    }
}

fn parse_loader(raw: &str) -> ModLoader {
    match raw {
        "fabric" => ModLoader::Fabric,
        "forge" => ModLoader::Forge,
        "neoforge" => ModLoader::NeoForge,
        "quilt" => ModLoader::Quilt,
        _ => ModLoader::Vanilla,
    }
}

fn source_to_str(source: ModSource) -> &'static str {
    match source {
        ModSource::Modrinth => "modrinth",
        ModSource::Curseforge => "curseforge",
    }
}

fn parse_source(raw: &str) -> ModSource {
    match raw {
        "curseforge" => ModSource::Curseforge,
        _ => ModSource::Modrinth,
    }
}
