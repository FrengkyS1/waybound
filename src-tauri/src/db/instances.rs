use crate::dto::instance::{InstalledMod, InstanceSummary};
use crate::dto::{ModLoader, ModOrigin, ModSource};
use rusqlite::{params, OptionalExtension, Row};

use super::{DbError, Database};

impl Database {
    pub fn list_instances(&self) -> Result<Vec<InstanceSummary>, DbError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT i.id, i.name, i.minecraft_version, i.loader, i.loader_version,
                    i.created_at, i.root_path, i.icon, i.last_played, i.total_play_seconds,
                    i.modpack_version_label, i.modpack_project_uid
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
                    i.created_at, i.root_path, i.icon, i.last_played, i.total_play_seconds,
                    i.modpack_version_label, i.modpack_project_uid
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

    /// Switches the instance's loader outright (e.g. Forge -> NeoForge).
    /// Used by the modpack installer when the pack archive declares a
    /// different loader than the instance was created with — launching a
    /// NeoForge pack under Forge leaves every NeoForge jar unregistered and
    /// the game fails on "missing" dependencies that are all on disk.
    pub fn set_instance_loader(&self, id: &str, loader: ModLoader) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instances SET loader = ?2 WHERE id = ?1",
            params![id, loader_to_str(loader)],
        )?;
        Ok(())
    }

    /// Overwrites the instance's pinned loader build (e.g. `"47.4.14"`).
    /// `None` clears the pin, reverting to whatever the loader's own
    /// "recommended" build resolves to at the next launch - see
    /// `launch::forge::resolve_version`.
    pub fn set_instance_loader_version(
        &self,
        id: &str,
        loader_version: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instances SET loader_version = ?2 WHERE id = ?1",
            params![id, loader_version],
        )?;
        Ok(())
    }
    /// Records the modpack's own version/filename label at import time, for
    /// display only - see `instances::install_modpack`.
    pub fn set_modpack_version_label(&self, id: &str, label: Option<&str>) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instances SET modpack_version_label = ?2 WHERE id = ?1",
            params![id, label],
        )?;
        Ok(())
    }

    /// One cached loader-version row (see `loader_meta`).
    pub fn get_loader_meta(&self, loader: &str, mc: &str) -> Result<Option<LoaderMetaRow>, DbError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT latest, recommended, fetched_at FROM loader_meta_cache WHERE loader = ?1 AND mc = ?2",
        )?;
        let mut rows = stmt.query(params![loader, mc])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(LoaderMetaRow {
                latest: row.get(0)?,
                recommended: row.get(1)?,
                fetched_at: row.get::<_, i64>(2)? as u64,
            }));
        }
        Ok(None)
    }

    pub fn set_loader_meta(
        &self,
        loader: &str,
        mc: &str,
        latest: Option<&str>,
        recommended: Option<&str>,
        fetched_at: u64,
    ) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO loader_meta_cache (loader, mc, latest, recommended, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![loader, mc, latest, recommended, fetched_at as i64],
        )?;
        Ok(())
    }

    /// Records which modpack project this instance was installed from, so
    /// the instance can offer the pack's other versions for in-place
    /// switching. `None` clears it (a manually-created instance has none).
    pub fn set_modpack_project_uid(&self, id: &str, uid: Option<&str>) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instances SET modpack_project_uid = ?2 WHERE id = ?1",
            params![id, uid],
        )?;
        Ok(())
    }

    /// One-time-ish backfill for the `origin` column: rows whose file is
    /// listed in the instance's pack sidecar (`.curseforge-pack-manifest.json`
    /// filenames, or `.modrinth-pack-manifest.json` `mods/` paths) become
    /// `pack`. Only touches rows still marked `user` (the migration
    /// default), so re-running never clobbers an explicit value — after the
    /// first pass the UPDATEs match nothing and cost a couple of indexed
    /// reads. Best-effort throughout: an unreadable sidecar just skips that
    /// instance, leaving its rows as `user` (the safe direction — a pack
    /// file misread as user-added is cosmetic, the reverse would hide real
    /// user mods).
    pub fn backfill_mod_origins(&self) {
        let pairs: Vec<(String, String)> = (|| {
            let conn = self.conn().ok()?;
            let mut stmt = conn.prepare("SELECT id, root_path FROM instances").ok()?;
            let rows = stmt
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .ok()?;
            rows.collect::<Result<Vec<_>, _>>().ok()
        })()
        .unwrap_or_default();
        for (id, root) in pairs {
            let pack_files = pack_filenames(std::path::Path::new(&root));
            if pack_files.is_empty() {
                continue;
            }
            // Match both spellings: rows may store the `.disabled`-suffixed
            // name while sidecars record the base filename.
            let mut names: Vec<String> = pack_files.iter().cloned().collect();
            names.extend(
                pack_files
                    .iter()
                    .map(|n| format!("{n}.disabled")),
            );
            let placeholders = names.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "UPDATE instance_mods SET origin = 'pack' WHERE instance_id = ?1 AND origin = 'user' AND file_name IN ({placeholders})"
            );
            if let Ok(conn) = self.conn() {
                let mut params: Vec<&dyn rusqlite::ToSql> = vec![&id];
                params.extend(names.iter().map(|s| s as &dyn rusqlite::ToSql));
                let _ = conn.execute(&sql, params.as_slice());
            }
        }
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
                config.max_memory_mb.map(|v| v.clamp(512, 32768) as i64),
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

    /// Files are published first; the instance and all tracking become visible
    /// together only after this transaction commits.
    pub fn insert_duplicate_instance(
        &self,
        source: &InstanceSummary,
        instance: &InstanceSummary,
    ) -> Result<(), DbError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO instances (id, name, minecraft_version, loader, loader_version, root_path, created_at,
                                    icon, modpack_version_label, modpack_project_uid, java_path, max_memory_mb, jvm_args)
             SELECT ?2, ?3, minecraft_version, loader, loader_version, ?4, ?5,
                    icon, modpack_version_label, modpack_project_uid, java_path, max_memory_mb, jvm_args
             FROM instances WHERE id = ?1",
            params![source.id, instance.id, instance.name, instance.root_path, instance.created_at as i64],
        )?;
        tx.execute(
            "INSERT INTO instance_mods (instance_id, mod_uid, mod_name, source, file_name, file_path, installed_at, icon_url, origin)
             SELECT ?2, mod_uid, mod_name, source, file_name, ?4 || substr(file_path, length(?3) + 1), installed_at, icon_url, origin
             FROM instance_mods WHERE instance_id = ?1",
            params![source.id, instance.id, source.root_path, instance.root_path],
        )?;
        tx.commit()?;
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
            "SELECT id, instance_id, mod_uid, mod_name, source, file_name, installed_at, icon_url, origin
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
        origin: ModOrigin,
    ) -> Result<InstalledMod, DbError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let installed_at = super::now_unix() as i64;
        tx.execute(
            "INSERT INTO instance_mods (instance_id, mod_uid, mod_name, source, file_name, file_path, installed_at, icon_url, origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(instance_id, mod_uid) DO UPDATE SET
               mod_name = excluded.mod_name,
               source = excluded.source,
               file_name = excluded.file_name,
               file_path = excluded.file_path,
               installed_at = excluded.installed_at,
               icon_url = COALESCE(excluded.icon_url, instance_mods.icon_url),
               origin = excluded.origin",
            params![
                instance_id,
                mod_uid,
                mod_name,
                source_to_str(source),
                file_name,
                file_path,
                installed_at,
                icon_url,
                origin_to_str(origin),
            ],
        )?;

        let installed = tx.query_row(
            "SELECT id, instance_id, mod_uid, mod_name, source, file_name, installed_at, icon_url, origin
             FROM instance_mods WHERE instance_id = ?1 AND mod_uid = ?2",
            params![instance_id, mod_uid],
            map_installed_mod_row,
        )?;
        tx.commit()?;
        Ok(installed)
    }

    /// Same upsert as `insert_instance_mod`, but for many mods in one
    /// transaction — used when scanning a mods folder after a modpack
    /// install (can be a few hundred jars). WAL mode already makes each
    /// individual commit cheap, but batching still avoids a few hundred
    /// separate implicit transactions for what's conceptually one operation.
    /// Every row takes the same origin (pack sync is the only caller).
    pub fn insert_instance_mods_batch(
        &self,
        instance_id: &str,
        mods: &[(String, String, ModSource, String, String, Option<String>)],
        origin: ModOrigin,
    ) -> Result<(), DbError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let installed_at = super::now_unix() as i64;
        for (mod_uid, mod_name, source, file_name, file_path, icon_url) in mods {
            tx.execute(
                "INSERT INTO instance_mods (instance_id, mod_uid, mod_name, source, file_name, file_path, installed_at, icon_url, origin)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(instance_id, mod_uid) DO UPDATE SET
                   mod_name = excluded.mod_name,
                   source = excluded.source,
                   file_name = excluded.file_name,
                   file_path = excluded.file_path,
                   installed_at = excluded.installed_at,
                   icon_url = COALESCE(excluded.icon_url, instance_mods.icon_url),
                   origin = excluded.origin",
                params![
                    instance_id,
                    mod_uid,
                    mod_name,
                    source_to_str(*source),
                    file_name,
                    file_path,
                    installed_at,
                    icon_url,
                    origin_to_str(origin),
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

    /// Overwrites a row's stored name with the project's real one — unlike
    /// the icon backfill, this isn't gated on the old value being empty: the
    /// pre-fix default was always a filename-derived guess (`"the-mod"` from
    /// `the-mod.jar`), which is always worth replacing with a real resolved
    /// name, not just when nothing was stored at all.
    pub fn update_instance_mod_name(&self, instance_id: &str, mod_uid: &str, name: &str) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE instance_mods SET mod_name = ?1 WHERE instance_id = ?2 AND mod_uid = ?3",
            params![name, instance_id, mod_uid],
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

    /// The cached jar-parsed display name for one mod file, when it's been
    /// resolved before — this is often a better config-matching term than
    /// the tracked project's own `mod_name` (a CurseForge/Modrinth listing
    /// title like "BisectHosting Server Integration Menu [FORGE]" can read
    /// nothing like the config folder's actual modid, e.g. "bhmenu", while
    /// the jar's own embedded name usually does).
    pub fn get_content_meta_name_by_file(
        &self,
        instance_id: &str,
        file_name: &str,
    ) -> Result<Option<String>, DbError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT name FROM content_meta_cache WHERE instance_id = ?1 AND category = 'mod' AND file_name = ?2 LIMIT 1",
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
            "SELECT category, file_name, size_bytes, mtime_unix, name, icon, mod_id
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
                mod_id: row.get(6)?,
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

    /// Records one file's parsed name/icon/modId (or the fact that parsing
    /// found none — still worth caching, so a jar with no embedded icon
    /// isn't re-opened forever trying to find one) against its current
    /// size+mtime fingerprint.
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
        mod_id: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO content_meta_cache (instance_id, category, file_name, size_bytes, mtime_unix, name, icon, written_version, mod_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(instance_id, category, file_name) DO UPDATE SET
               size_bytes = excluded.size_bytes,
               mtime_unix = excluded.mtime_unix,
               name = excluded.name,
               icon = excluded.icon,
               written_version = excluded.written_version,
               mod_id = excluded.mod_id",
            params![instance_id, category, file_name, size_bytes as i64, mtime_unix, name, icon, crate::db::APP_VERSION, mod_id],
        )?;
        Ok(())
    }

    /// The cached jar-parsed modId (Forge/NeoForge `modId` from
    /// `mods.toml`/`neoforge.mods.toml`, or Fabric/Quilt's `id`) for one mod
    /// file — the loader's own per-side config system names files after
    /// exactly this string by default, so it's a precise (not fuzzy) config
    /// match when present. `None` both when unresolved and when the jar
    /// genuinely has no modId (never actually true, but parsing is
    /// best-effort) — either way the caller falls back to name-based
    /// matching.
    pub fn get_content_meta_mod_id_by_file(
        &self,
        instance_id: &str,
        file_name: &str,
    ) -> Result<Option<String>, DbError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT mod_id FROM content_meta_cache WHERE instance_id = ?1 AND category = 'mod' AND file_name = ?2 LIMIT 1",
            params![instance_id, file_name],
            |row| row.get(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(DbError::from)
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
    pub mod_id: Option<String>,
}

/// One cached loader-version index row (see `loader_meta`).
pub struct LoaderMetaRow {
    pub latest: Option<String>,
    pub recommended: Option<String>,
    pub fetched_at: u64,
}

    /// Filenames the instance's pack sidecars claim: CF manifest entries'
    /// `filename`, mrpack `mods/` paths reduced to file names, plus
    /// override files recorded at import time (resource packs, configs,
    /// extra jars — none of which the manifests list). Shared by the
    /// origin backfill and the Content tab's untracked-file marking.
    /// Best-effort: unreadable sidecars contribute nothing.
    pub(crate) fn pack_filenames(root: &std::path::Path) -> std::collections::HashSet<String> {
        fn insert_basename(set: &mut std::collections::HashSet<String>, path: &str) {
            if let Some(name) = std::path::Path::new(path).file_name().and_then(|n| n.to_str()) {
                if !name.is_empty() {
                    set.insert(name.to_string());
                }
            }
        }
        let mut pack_files = std::collections::HashSet::new();
        if let Ok(text) = std::fs::read_to_string(root.join(".curseforge-pack-manifest.json")) {
            if let Ok(serde_json::Value::Array(entries)) = serde_json::from_str::<serde_json::Value>(&text) {
                for entry in &entries {
                    if let Some(name) = entry.get("filename").and_then(|v| v.as_str()) {
                        pack_files.insert(name.to_string());
                    }
                }
            }
        }
        for manifest in [".modrinth-pack-manifest.json", ".pack-overrides-manifest.json"] {
            if let Ok(text) = std::fs::read_to_string(root.join(manifest)) {
                if let Ok(serde_json::Value::Array(entries)) = serde_json::from_str::<serde_json::Value>(&text) {
                    for entry in &entries {
                        if let Some(path) = entry.as_str() {
                            insert_basename(&mut pack_files, path);
                        }
                    }
                }
            }
        }
        pack_files
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
        modpack_version_label: row.get(10)?,
        modpack_project_uid: row.get(11)?,
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
    let origin_raw: String = row.get(8)?;
    Ok(InstalledMod {
        id: row.get(0)?,
        instance_id: row.get(1)?,
        mod_uid: row.get(2)?,
        mod_name: row.get(3)?,
        source: parse_source(&source_raw),
        file_name: row.get(5)?,
        installed_at: row.get::<_, i64>(6)? as u64,
        icon_url: row.get(7)?,
        origin: parse_origin(&origin_raw),
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

fn origin_to_str(origin: ModOrigin) -> &'static str {
    match origin {
        ModOrigin::User => "user",
        ModOrigin::Pack => "pack",
    }
}

fn parse_origin(raw: &str) -> ModOrigin {
    match raw {
        "pack" => ModOrigin::Pack,
        _ => ModOrigin::User,
    }
}

#[cfg(test)]
mod instance_db_tests {
    use super::*;

    /// A throwaway database file in the temp dir, removed when the test ends.
    /// Never the real `library.db` — `Database::open()` resolves the user's
    /// app-data path, so these go through `open_at` instead.
    struct TempDb {
        dir: std::path::PathBuf,
        db: Database,
    }

    impl TempDb {
        fn new(name: &str) -> Self {
            // Pid-scoped so two overlapping `cargo test` processes can't
            // delete each other's database mid-test (see the same note in
            // commands/launch.rs's temp_dir).
            let dir = std::env::temp_dir()
                .join(format!("waybound-instance-db-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let db = Database::open_at(&dir.join("library.db")).unwrap();
            Self { dir, db }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn sample_instance(id: &str, name: &str, created_at: u64) -> InstanceSummary {
        InstanceSummary {
            id: id.to_string(),
            name: name.to_string(),
            minecraft_version: "1.20.1".to_string(),
            loader: ModLoader::Forge,
            loader_version: Some("47.2.0".to_string()),
            mod_count: 0,
            created_at,
            // Deliberately nonexistent: `mod_count` is recounted from disk on
            // read, so this must come back as 0 rather than error.
            root_path: format!("C:/waybound-test-nonexistent/{id}"),
            icon: None,
            last_played: None,
            total_play_seconds: 0,
            modpack_version_label: None,
            modpack_project_uid: None,
        }
    }

    #[test]
    fn instance_crud_round_trips() {
        let temp = TempDb::new("crud");
        let db = &temp.db;

        db.insert_instance(&sample_instance("a", "Alpha", 100)).unwrap();
        db.insert_instance(&sample_instance("b", "Beta", 200)).unwrap();

        let listed = db.list_instances().unwrap();
        assert_eq!(listed.len(), 2);
        // Ordered by created_at DESC.
        assert_eq!(listed[0].id, "b");
        assert_eq!(listed[1].id, "a");

        let got = db.get_instance("a").unwrap().expect("inserted instance must be readable");
        assert_eq!(got.name, "Alpha");
        assert_eq!(got.minecraft_version, "1.20.1");
        assert_eq!(got.loader, ModLoader::Forge);
        assert_eq!(got.loader_version.as_deref(), Some("47.2.0"));
        assert_eq!(got.created_at, 100);
        assert_eq!(got.mod_count, 0, "missing mods dir counts as zero, not an error");

        db.rename_instance("a", "Renamed").unwrap();
        assert_eq!(db.get_instance("a").unwrap().unwrap().name, "Renamed");

        assert!(db.delete_instance("a").unwrap());
        assert!(db.get_instance("a").unwrap().is_none());
        assert!(!db.delete_instance("a").unwrap(), "deleting twice reports nothing removed");
        assert_eq!(db.list_instances().unwrap().len(), 1);
    }

    #[test]
    fn loader_version_and_modpack_label_round_trip() {
        let temp = TempDb::new("columns");
        let db = &temp.db;
        db.insert_instance(&sample_instance("a", "Alpha", 100)).unwrap();

        db.set_instance_loader_version("a", Some("47.4.14")).unwrap();
        db.set_modpack_version_label("a", Some("Ascendra-2.1.0")).unwrap();
        db.set_instance_loader("a", ModLoader::NeoForge).unwrap();

        let got = db.get_instance("a").unwrap().unwrap();
        assert_eq!(got.loader, ModLoader::NeoForge);
        assert_eq!(got.loader_version.as_deref(), Some("47.4.14"));
        assert_eq!(got.modpack_version_label.as_deref(), Some("Ascendra-2.1.0"));
        // Same values must survive the list query, which selects the columns
        // separately from `get_instance`.
        assert_eq!(
            db.list_instances().unwrap()[0].modpack_version_label.as_deref(),
            Some("Ascendra-2.1.0")
        );

        // `None` clears rather than being ignored.
        db.set_instance_loader_version("a", None).unwrap();
        db.set_modpack_version_label("a", None).unwrap();
        let cleared = db.get_instance("a").unwrap().unwrap();
        assert!(cleared.loader_version.is_none());
        assert!(cleared.modpack_version_label.is_none());
    }

    #[test]
    fn loader_meta_cache_round_trip() {
        let temp = TempDb::new("loader-meta");
        let db = &temp.db;

        assert!(db.get_loader_meta("neoforge", "1.21.1").unwrap().is_none());

        db.set_loader_meta("neoforge", "1.21.1", Some("21.1.209"), None, 1000).unwrap();
        let row = db.get_loader_meta("neoforge", "1.21.1").unwrap().expect("just wrote it");
        assert_eq!(row.latest.as_deref(), Some("21.1.209"));
        assert_eq!(row.recommended, None);
        assert_eq!(row.fetched_at, 1000);

        // A refresh overwrites in place (no duplicate-key error).
        db.set_loader_meta("neoforge", "1.21.1", Some("21.1.210"), None, 2000).unwrap();
        let row = db.get_loader_meta("neoforge", "1.21.1").unwrap().unwrap();
        assert_eq!(row.latest.as_deref(), Some("21.1.210"));
        assert_eq!(row.fetched_at, 2000);

        // Different pairs are independent rows.
        db.set_loader_meta("forge", "1.20.1", Some("47.4.0"), Some("47.2.0"), 3000).unwrap();
        let row = db.get_loader_meta("forge", "1.20.1").unwrap().unwrap();
        assert_eq!(row.recommended.as_deref(), Some("47.2.0"));
    }

    #[test]
    fn content_meta_cache_upserts_and_reads_back() {
        let temp = TempDb::new("meta-cache");
        let db = &temp.db;

        db.upsert_content_meta_cache("i1", "mod", "jei.jar", 1024, 42, Some("JEI"), Some("data:png"), Some("jei"))
            .unwrap();
        db.upsert_content_meta_cache("i1", "resourcepack", "faithful.zip", 2048, 43, None, None, None)
            .unwrap();

        let mut rows = db.get_content_meta_cache("i1").unwrap();
        rows.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].file_name, "jei.jar");
        assert_eq!(rows[1].category, "mod");
        assert_eq!(rows[1].size_bytes, 1024);
        assert_eq!(rows[1].mtime_unix, 42);
        assert_eq!(rows[1].name.as_deref(), Some("JEI"));
        assert_eq!(rows[1].mod_id.as_deref(), Some("jei"));
        assert!(rows[0].name.is_none(), "a parse that found nothing is still cached");

        // Same primary key -> update in place, not a duplicate row.
        db.upsert_content_meta_cache("i1", "mod", "jei.jar", 4096, 99, Some("Just Enough Items"), None, Some("jei2"))
            .unwrap();
        assert_eq!(db.get_content_meta_cache("i1").unwrap().len(), 2);
        assert_eq!(db.get_content_meta_name_by_file("i1", "jei.jar").unwrap().as_deref(), Some("Just Enough Items"));
        assert_eq!(db.get_content_meta_mod_id_by_file("i1", "jei.jar").unwrap().as_deref(), Some("jei2"));

        // Scoped per instance.
        assert!(db.get_content_meta_cache("other").unwrap().is_empty());
        assert!(db.get_content_meta_mod_id_by_file("i1", "nope.jar").unwrap().is_none());
        // Only the `mod` category is consulted by the by-file lookups.
        assert!(db.get_content_meta_name_by_file("i1", "faithful.zip").unwrap().is_none());

        db.delete_content_meta_cache("i1", "mod", "jei.jar").unwrap();
        assert_eq!(db.get_content_meta_cache("i1").unwrap().len(), 1);
    }

    #[test]
    fn instance_mods_cascade_on_instance_delete() {
        let temp = TempDb::new("cascade");
        let db = &temp.db;
        db.insert_instance(&sample_instance("a", "Alpha", 100)).unwrap();
        db.insert_instance_mod("a", "modrinth:abc", "JEI", ModSource::Modrinth, "jei.jar", "C:/x/jei.jar", None, ModOrigin::User)
            .unwrap();
        assert_eq!(db.list_instance_mods("a").unwrap().len(), 1);

        db.delete_instance("a").unwrap();
        assert!(
            db.list_instance_mods("a").unwrap().is_empty(),
            "ON DELETE CASCADE requires PRAGMA foreign_keys to actually be on"
        );
    }

    #[test]
    fn mod_origin_round_trips_and_upsert_overwrites() {
        let temp = TempDb::new("origin");
        let db = &temp.db;
        db.insert_instance(&sample_instance("a", "Alpha", 100)).unwrap();
        let row = db
            .insert_instance_mod("a", "curseforge:1", "Pack Mod", ModSource::Curseforge, "pack.jar", "C:/x/pack.jar", None, ModOrigin::Pack)
            .unwrap();
        assert_eq!(row.origin, ModOrigin::Pack);
        // Reinstalling the same project directly flips it to user.
        let row = db
            .insert_instance_mod("a", "curseforge:1", "Pack Mod", ModSource::Curseforge, "pack.jar", "C:/x/pack.jar", None, ModOrigin::User)
            .unwrap();
        assert_eq!(row.origin, ModOrigin::User);
        assert_eq!(db.list_instance_mods("a").unwrap()[0].origin, ModOrigin::User);
    }

    #[test]
    fn backfill_marks_sidecar_files_as_pack() {
        use std::io::Write;
        let temp = TempDb::new("backfill");
        let db = &temp.db;
        // Point the instance root at a temp dir holding a CF sidecar.
        let dir = std::env::temp_dir().join("waybound-backfill-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut sidecar = std::fs::File::create(dir.join(".curseforge-pack-manifest.json")).unwrap();
        sidecar
            .write_all(br#"[{"project_id":1,"file_id":10,"name":"Pack Mod","filename":"pack.jar","url":"https://example.com","sha1":null}]"#)
            .unwrap();
        let mut inst = sample_instance("a", "Alpha", 100);
        inst.root_path = dir.to_string_lossy().to_string();
        db.insert_instance(&inst).unwrap();
        // Both rows start as user (the migration default).
        db.insert_instance_mod("a", "curseforge:1", "Pack Mod", ModSource::Curseforge, "pack.jar", "C:/x/pack.jar", None, ModOrigin::User)
            .unwrap();
        db.insert_instance_mod("a", "modrinth:2", "My Mod", ModSource::Modrinth, "mine.jar", "C:/x/mine.jar", None, ModOrigin::User)
            .unwrap();

        db.backfill_mod_origins();

        let rows = db.list_instance_mods("a").unwrap();
        let pack = rows.iter().find(|r| r.file_name == "pack.jar").unwrap();
        let mine = rows.iter().find(|r| r.file_name == "mine.jar").unwrap();
        assert_eq!(pack.origin, ModOrigin::Pack);
        assert_eq!(mine.origin, ModOrigin::User);
        // Idempotent: a second run changes nothing.
        db.backfill_mod_origins();
        let rows = db.list_instance_mods("a").unwrap();
        assert_eq!(rows.iter().find(|r| r.file_name == "pack.jar").unwrap().origin, ModOrigin::Pack);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
