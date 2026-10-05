mod instances;
pub use instances::CachedContentMeta;
pub(crate) use instances::pack_filenames;

use crate::dto::ModSummary;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

const DB_DIR_NAME: &str = "dev.waybound";
const DB_FILE_NAME: &str = "library.db";
pub const SEARCH_CACHE_TTL_SECS: u64 = 900; // 15 minutes

#[derive(Debug, Error)]
pub enum DbError {
    #[error("could not resolve data directory")]
    NoDataDir,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    pub fn open() -> Result<Self, DbError> {
        let path = db_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        match Self::open_at(&path) {
            Ok(db) => Ok(db),
            Err(err) if matches!(&err, DbError::Sqlite(rusqlite::Error::SqliteFailure(code, _))
                if matches!(code.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)) => {
                // A corrupted library.db (crash mid-write, truncated file,
                // disk full, ...) used to `?` straight out of here into an
                // `.expect()` in lib.rs, panicking before any window ever
                // opened — a GUI app's console output goes nowhere, so the
                // user just saw it silently fail to launch. There's no way
                // to automatically salvage a genuinely corrupt SQLite file,
                // so back it up (in case manual recovery is ever worth
                // attempting) and start fresh instead of refusing to launch.
                let mut backup = path.as_os_str().to_os_string();
                backup.push(".bak");
                let _ = std::fs::rename(&path, PathBuf::from(&backup));
                crate::activity::append_log(
                    &format!(
                        "library.db was unreadable ({err}) — backed up to {} and started a fresh database",
                        PathBuf::from(&backup).display()
                    ),
                    "warn",
                    None,
                );
                Self::open_at(&path)
            }
            Err(err) => Err(err),
        }
    }

    pub(crate) fn open_at(path: &Path) -> Result<Self, DbError> {
        let mut conn = Connection::open(path)?;
        let had_origins = {
            let mut stmt = conn.prepare("PRAGMA table_info(instance_mods)")?;
            let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
            names.collect::<Result<Vec<_>, _>>()?.iter().any(|name| name == "origin")
        };
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS mod_identity (
                mod_uid TEXT PRIMARY KEY NOT NULL,
                slug TEXT NOT NULL,
                name TEXT NOT NULL,
                curseforge_id INTEGER,
                modrinth_id TEXT,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_mod_identity_curseforge
                ON mod_identity(curseforge_id) WHERE curseforge_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_mod_identity_modrinth
                ON mod_identity(modrinth_id) WHERE modrinth_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_mod_identity_slug
                ON mod_identity(slug);

            CREATE TABLE IF NOT EXISTS search_cache (
                cache_key TEXT PRIMARY KEY NOT NULL,
                payload_json TEXT NOT NULL,
                fetched_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS instances (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL UNIQUE,
                minecraft_version TEXT NOT NULL,
                loader TEXT NOT NULL,
                loader_version TEXT,
                root_path TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS instance_mods (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id TEXT NOT NULL REFERENCES instances(id) ON DELETE CASCADE,
                mod_uid TEXT NOT NULL,
                mod_name TEXT NOT NULL,
                source TEXT NOT NULL,
                file_name TEXT NOT NULL,
                file_path TEXT NOT NULL,
                installed_at INTEGER NOT NULL,
                origin TEXT NOT NULL DEFAULT 'user',
                UNIQUE(instance_id, mod_uid)
            );

            CREATE INDEX IF NOT EXISTS idx_instance_mods_instance
                ON instance_mods(instance_id);

            -- Jar/zip metadata (display name + embedded icon) is expensive to
            -- read (opening and parsing the archive) but never changes for a
            -- given file's exact bytes, so it's cached here keyed by the
            -- file's size+mtime fingerprint. A cache hit means the Content
            -- tab never has to open that file again — instant name/icon on
            -- every load after the first.
            CREATE TABLE IF NOT EXISTS content_meta_cache (
                instance_id TEXT NOT NULL,
                category TEXT NOT NULL,
                file_name TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                mtime_unix INTEGER NOT NULL,
                name TEXT,
                icon TEXT,
                PRIMARY KEY (instance_id, category, file_name)
            );

            -- Loader-version index (latest + recommended builds per loader
            -- and game version, refreshed at most daily). Served from here
            -- so version displays work offline from yesterday's answers.
            CREATE TABLE IF NOT EXISTS loader_meta_cache (
                loader TEXT NOT NULL,
                mc TEXT NOT NULL,
                latest TEXT,
                recommended TEXT,
                fetched_at INTEGER NOT NULL,
                PRIMARY KEY (loader, mc)
            );
            ",
        )?;

        // Migrations: added columns on `instances`. Each ignores the error when
        // the column already exists on an older database.
        for stmt in [
            "ALTER TABLE instances ADD COLUMN icon TEXT",
            "ALTER TABLE instances ADD COLUMN java_path TEXT",
            "ALTER TABLE instances ADD COLUMN max_memory_mb INTEGER",
            "ALTER TABLE instances ADD COLUMN jvm_args TEXT",
            "ALTER TABLE instances ADD COLUMN last_played INTEGER",
            "ALTER TABLE instances ADD COLUMN total_play_seconds INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE instance_mods ADD COLUMN icon_url TEXT",
            "ALTER TABLE content_meta_cache ADD COLUMN written_version TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE content_meta_cache ADD COLUMN mod_id TEXT",
            "ALTER TABLE instances ADD COLUMN modpack_version_label TEXT",
            "ALTER TABLE instances ADD COLUMN modpack_project_uid TEXT",
            "ALTER TABLE instance_mods ADD COLUMN origin TEXT NOT NULL DEFAULT 'user'",
        ] {
            let _ = conn.execute(stmt, []);
        }

        migrate_project_uids(&mut conn)?;
        migrate_content_filenames(&mut conn)?;

        // Upstream version identifiers are not processed search results. Move
        // historical keys before pruning so upgrades retain offline choices.
        conn.execute(
            "INSERT OR IGNORE INTO search_cache (cache_key, payload_json, fetched_at)
             SELECT 'durable:game-versions', payload_json, fetched_at FROM search_cache
             WHERE cache_key = 'game-versions' OR cache_key LIKE '%:game-versions'
             ORDER BY fetched_at DESC LIMIT 1",
            [],
        )?;
        // Every cache row this version writes is prefixed/tagged with its
        // own version (see `cache_key_prefix`/`APP_VERSION`) — anything left
        // over from a previous version was computed by different processing
        // logic and is never read back under the new version anyway, so it
        // would just sit here forever without this.
        let _ = conn.execute(
            "DELETE FROM search_cache WHERE cache_key NOT LIKE ?1 AND cache_key != 'durable:game-versions'",
            params![format!("{}%", cache_key_prefix())],
        );
        let _ = conn.execute(
            "DELETE FROM content_meta_cache WHERE written_version != ?1",
            params![APP_VERSION],
        );

        let db = Self {
            conn: Mutex::new(conn),
        };
        if !had_origins {
            db.backfill_mod_origins();
        }
        Ok(db)
    }

    pub fn get_search_cache(&self, cache_key: &str) -> Result<Option<CachedSearch>, DbError> {
        let conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::InvalidParameterName("database lock poisoned".into())
        })?;

        let mut stmt = conn.prepare(
            "SELECT payload_json, fetched_at FROM search_cache WHERE cache_key = ?1",
        )?;

        let mut rows = stmt.query(params![cache_key])?;
        if let Some(row) = rows.next()? {
            let payload_json: String = row.get(0)?;
            let fetched_at: i64 = row.get(1)?;
            let result = serde_json::from_str(&payload_json)?;
            return Ok(Some(CachedSearch {
                result,
                fetched_at: fetched_at as u64,
            }));
        }

        Ok(None)
    }

    pub fn put_search_cache(&self, cache_key: &str, result: &crate::dto::ModSearchResult) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::InvalidParameterName("database lock poisoned".into())
        })?;

        let payload_json = serde_json::to_string(result)?;
        let fetched_at = now_unix() as i64;

        conn.execute(
            "INSERT INTO search_cache (cache_key, payload_json, fetched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(cache_key) DO UPDATE SET
               payload_json = excluded.payload_json,
               fetched_at = excluded.fetched_at",
            params![cache_key, payload_json, fetched_at],
        )?;

        Ok(())
    }

    /// Generic cache read/write reusing the `search_cache` table's schema
    /// (cache_key/payload_json/fetched_at) for any JSON-serializable payload,
    /// keyed distinctly from search results by cache_key prefix.
    pub fn get_cached_json(&self, cache_key: &str) -> Result<Option<(String, u64)>, DbError> {
        let conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::InvalidParameterName("database lock poisoned".into())
        })?;

        let mut stmt = conn.prepare(
            "SELECT payload_json, fetched_at FROM search_cache WHERE cache_key = ?1",
        )?;
        let mut rows = stmt.query(params![cache_key])?;
        if let Some(row) = rows.next()? {
            let payload_json: String = row.get(0)?;
            let fetched_at: i64 = row.get(1)?;
            return Ok(Some((payload_json, fetched_at as u64)));
        }
        Ok(None)
    }

    pub fn put_cached_json(&self, cache_key: &str, payload_json: &str) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::InvalidParameterName("database lock poisoned".into())
        })?;

        let fetched_at = now_unix() as i64;
        conn.execute(
            "INSERT INTO search_cache (cache_key, payload_json, fetched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(cache_key) DO UPDATE SET
               payload_json = excluded.payload_json,
               fetched_at = excluded.fetched_at",
            params![cache_key, payload_json, fetched_at],
        )?;
        Ok(())
    }

    pub fn upsert_identities(&self, hits: &[ModSummary]) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::InvalidParameterName("database lock poisoned".into())
        })?;

        for hit in hits {
            conn.execute(
                "INSERT INTO mod_identity (mod_uid, slug, name, curseforge_id, modrinth_id, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(mod_uid) DO UPDATE SET
                   slug = excluded.slug,
                   name = excluded.name,
                   curseforge_id = COALESCE(excluded.curseforge_id, mod_identity.curseforge_id),
                   modrinth_id = COALESCE(excluded.modrinth_id, mod_identity.modrinth_id),
                   updated_at = excluded.updated_at",
                params![
                    hit.uid,
                    hit.slug,
                    hit.name,
                    hit.curseforge_id,
                    hit.modrinth_id,
                    hit.updated_at,
                ],
            )?;
        }

        Ok(())
    }

    pub(crate) fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, DbError> {
        self.conn.lock().map_err(|_| {
            DbError::Sqlite(rusqlite::Error::InvalidParameterName("database lock poisoned".into()))
        })
    }
}

/// Source IDs never share a namespace. Run transactionally: a conflicting
/// tracked row must not silently replace another file or destroy the library.
fn migrate_project_uids(conn: &mut Connection) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        "INSERT INTO mod_identity (mod_uid, slug, name, curseforge_id, modrinth_id, updated_at)
         SELECT 'modrinth:' || modrinth_id, slug, name, NULL, modrinth_id, updated_at
         FROM mod_identity WHERE mod_uid LIKE 'mod:%' AND modrinth_id IS NOT NULL
         ON CONFLICT(mod_uid) DO UPDATE SET
           modrinth_id = COALESCE(mod_identity.modrinth_id, excluded.modrinth_id);
         INSERT INTO mod_identity (mod_uid, slug, name, curseforge_id, modrinth_id, updated_at)
         SELECT 'curseforge:' || curseforge_id, slug, name, curseforge_id, NULL, updated_at
         FROM mod_identity WHERE mod_uid LIKE 'mod:%' AND curseforge_id IS NOT NULL
         ON CONFLICT(mod_uid) DO UPDATE SET
           curseforge_id = COALESCE(mod_identity.curseforge_id, excluded.curseforge_id);
         UPDATE instances SET modpack_project_uid =
           CASE
             WHEN modpack_project_uid LIKE 'mod:cf:%' THEN 'curseforge:' || substr(modpack_project_uid, 8)
             WHEN modpack_project_uid LIKE 'mod:%' THEN 'modrinth:' || substr(modpack_project_uid, 5)
             WHEN modpack_project_uid LIKE 'modpack:cf:%' THEN 'curseforge:' || substr(modpack_project_uid, 12)
             WHEN modpack_project_uid LIKE 'modpack:%' THEN 'modrinth:' || substr(modpack_project_uid, 9)
             ELSE modpack_project_uid
           END;",
    )?;
    let legacy = {
        let mut stmt = tx.prepare(
            "SELECT m.id, m.instance_id, m.mod_uid, m.source, m.file_name, i.curseforge_id
             FROM instance_mods m LEFT JOIN mod_identity i ON i.mod_uid=m.mod_uid
             WHERE m.mod_uid LIKE 'mod:%' ORDER BY m.installed_at DESC, m.id DESC",
        )?;
        let rows = stmt.query_map([], |row| Ok((
            row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?,
            row.get::<_, Option<u32>>(5)?,
        )))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (id, instance, uid, source, filename, curseforge_id) in legacy {
        let canonical = if let Some(project) = uid.strip_prefix("mod:cf:") {
            format!("curseforge:{project}")
        } else if let Some(project) = uid.strip_prefix("mod:slug:") {
            // Unresolved slug identities never had a source project ID.
            let _ = project;
            format!("file:{filename}")
        } else {
            if source == "curseforge" && curseforge_id.is_some() {
                format!("curseforge:{}", curseforge_id.unwrap())
            } else {
                // Historical fused identities used the Modrinth ID, regardless
                // of which source supplied the installed bytes.
                format!("modrinth:{}", &uid[4..])
            }
        };
        let mut target = canonical;
        let existing = {
            let mut stmt = tx.prepare("SELECT id, file_name FROM instance_mods WHERE instance_id=?1 AND mod_uid=?2")?;
            let mut rows = stmt.query(params![instance, target])?;
            match rows.next()? {
                Some(row) => Some((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                None => None,
            }
        };
        if existing.as_ref().is_some_and(|(_, name)| name != &filename) {
            // Multiple versions are real files, not duplicate project metadata.
            target = format!("file:{filename}");
        }
        let existing_id = tx.query_row(
            "SELECT id FROM instance_mods WHERE instance_id=?1 AND mod_uid=?2",
            params![instance, target], |row| row.get::<_, i64>(0),
        );
        match existing_id {
            Ok(other) => {
                tx.execute(
                    "UPDATE instance_mods SET
                     mod_name=CASE WHEN installed_at < (SELECT installed_at FROM instance_mods WHERE id=?2)
                       THEN (SELECT mod_name FROM instance_mods WHERE id=?2) ELSE mod_name END,
                     file_path=CASE WHEN installed_at < (SELECT installed_at FROM instance_mods WHERE id=?2)
                       THEN (SELECT file_path FROM instance_mods WHERE id=?2) ELSE file_path END,
                     icon_url=COALESCE(icon_url,(SELECT icon_url FROM instance_mods WHERE id=?2)),
                     installed_at=max(installed_at,(SELECT installed_at FROM instance_mods WHERE id=?2)),
                     origin=CASE WHEN origin='user' OR (SELECT origin FROM instance_mods WHERE id=?2)='user'
                       THEN 'user' ELSE origin END WHERE id=?1",
                    params![other, id],
                )?;
                tx.execute("DELETE FROM instance_mods WHERE id=?1", params![id])?;
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                tx.execute("UPDATE instance_mods SET mod_uid=?2 WHERE id=?1", params![id, target])?;
            }
            Err(err) => return Err(err.into()),
        }
    }
    tx.execute("DELETE FROM mod_identity WHERE mod_uid LIKE 'mod:%'
        AND (curseforge_id IS NOT NULL OR modrinth_id IS NOT NULL)", [])?;
    tx.commit()?;
    Ok(())
}

/// Content and tracking use exact physical basenames, including `.disabled`.
/// Normalize metadata only; never rename/delete genuine duplicate disk files.
fn migrate_content_filenames(conn: &mut Connection) -> Result<(), DbError> {
    let rows = {
        let mut stmt = conn.prepare(
            "SELECT m.id, m.file_name, m.file_path, i.root_path FROM instance_mods m
             JOIN instances i ON i.id=m.instance_id"
        )?;
        let rows = stmt.query_map([], |row| Ok((
            row.get::<_, i64>(0)?, row.get::<_, String>(1)?,
            row.get::<_, String>(2)?, row.get::<_, String>(3)?,
        )))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let tx = conn.transaction()?;
    for (id, filename, path, root) in rows {
        let logical = filename.strip_suffix(crate::commands::content::DISABLED_SUFFIX).unwrap_or(&filename);
        if !logical.to_ascii_lowercase().ends_with(".jar") { continue; }
        let root = Path::new(&root);
        let mut physical = PathBuf::from(&path);
        if crate::download::ensure_contained_path(root, &physical).is_err() { continue; }
        if !physical.exists() && !path.ends_with(crate::commands::content::DISABLED_SUFFIX) {
            let mut disabled = physical.as_os_str().to_os_string();
            disabled.push(crate::commands::content::DISABLED_SUFFIX);
            let disabled = PathBuf::from(disabled);
            if crate::download::ensure_contained_path(root, &disabled).is_ok() && disabled.is_file() {
                physical = disabled;
            }
        }
        let Some(name) = physical.file_name().and_then(|name| name.to_str()) else { continue };
        if name != filename || physical != Path::new(&path) {
            tx.execute("UPDATE instance_mods SET file_name=?2, file_path=?3 WHERE id=?1",
                params![id, name, physical.display().to_string()])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub struct CachedSearch {
    pub result: crate::dto::ModSearchResult,
    pub fetched_at: u64,
}

impl CachedSearch {
    pub fn is_fresh(&self, ttl_secs: u64) -> bool {
        now_unix().saturating_sub(self.fetched_at) <= ttl_secs
    }
}

/// Shared by every cache that stores the *result of Waybound's own
/// processing* of upstream/on-disk data (icon resolution, jar metadata
/// parsing, field mapping, ...) rather than a plain copy of it — a bug fix
/// to that processing can't take effect for anything already cached until
/// it expires (or, for a cache with no TTL at all, never). Tagging cache
/// rows with the version that wrote them means a version bump can never
/// read back a previous version's differently-processed rows.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn cache_key_prefix() -> &'static str {
    concat!(env!("CARGO_PKG_VERSION"), ":")
}

pub fn build_search_cache_key(query: &crate::dto::ModSearchQuery, modrinth_only: bool) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    query.query.trim().hash(&mut hasher);
    query.content_type.hash(&mut hasher);
    query.loader.hash(&mut hasher);
    query.sort.hash(&mut hasher);
    query.offset.hash(&mut hasher);
    query.limit.hash(&mut hasher);
    modrinth_only.hash(&mut hasher);
    format!("{}search:{:x}", cache_key_prefix(), hasher.finish())
}

fn db_path() -> Result<PathBuf, DbError> {
    let base = dirs::data_dir().ok_or(DbError::NoDataDir)?;
    Ok(base.join(DB_DIR_NAME).join(DB_FILE_NAME))
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::build_search_cache_key;
    use crate::dto::{ContentType, ModSearchQuery, SortIndex};

    #[test]
    fn cache_key_differs_for_modrinth_only() {
        let query = ModSearchQuery {
            query: String::new(),
            content_type: Some(ContentType::Mod),
            loader: None,
            sort: SortIndex::Downloads,
            offset: 0,
            limit: 24,
        };
        let a = build_search_cache_key(&query, true);
        let b = build_search_cache_key(&query, false);
        assert_ne!(a, b);
    }
}

/// Schema-level behaviour of `open_at`: the ALTER TABLE migrations and the
/// version-based cache pruning that both run on every open. Always against a
/// throwaway file in the temp dir — never the real `library.db`.
#[cfg(test)]
mod schema_tests {
    use super::{cache_key_prefix, Database, APP_VERSION};
    use rusqlite::params;

    struct TempDb {
        dir: std::path::PathBuf,
    }

    impl TempDb {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("waybound-schema-test-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn path(&self) -> std::path::PathBuf {
            self.dir.join("library.db")
        }

        fn open(&self) -> Database {
            Database::open_at(&self.path()).unwrap()
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn game_versions_migrate_and_survive_restarts_and_upgrades() {
        let temp = TempDb::new("durable-game-versions");
        let old = r#"[{"version":"1.20.1","versionType":"release"}]"#;
        let latest = r#"[{"version":"1.21.1","versionType":"release"}]"#;
        {
            let db = temp.open();
            let conn = db.conn().unwrap();
            conn.execute(
                "INSERT INTO search_cache (cache_key, payload_json, fetched_at) VALUES (?1, ?2, ?3)",
                params!["game-versions", old, 1],
            ).unwrap();
            conn.execute(
                "INSERT INTO search_cache (cache_key, payload_json, fetched_at) VALUES (?1, ?2, ?3)",
                params!["previous-app:game-versions", latest, 2],
            ).unwrap();
        }
        for _ in 0..2 {
            let db = temp.open();
            let (json, fetched) = db.get_cached_json("durable:game-versions").unwrap().unwrap();
            let versions: Vec<crate::dto::instance::GameVersionOption> = serde_json::from_str(&json).unwrap();
            assert_eq!(versions[0].version, "1.21.1");
            assert_eq!(fetched, 2);
            assert!(db.get_cached_json("previous-app:game-versions").unwrap().is_none());
        }
    }

    #[test]
    fn migrations_are_idempotent_across_reopens() {
        let temp = TempDb::new("idempotent");
        {
            let db = temp.open();
            let conn = db.conn().unwrap();
            conn.execute(
                "INSERT INTO instances
                   (id, name, minecraft_version, loader, root_path, created_at, modpack_version_label)
                 VALUES ('i1', 'One', '1.20.1', 'forge', 'C:/nowhere', 1, 'Pack 1.0')",
                [],
            )
            .unwrap();
        }

        // Every reopen re-runs the `ALTER TABLE ... ADD COLUMN` migrations,
        // which are expected to fail harmlessly once the columns exist —
        // and must not disturb the data already there.
        for _ in 0..2 {
            let db = temp.open();
            let conn = db.conn().unwrap();
            let label: Option<String> = conn
                .query_row("SELECT modpack_version_label FROM instances WHERE id = 'i1'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(label.as_deref(), Some("Pack 1.0"));
        }
    }

    #[test]
    fn stale_version_cache_rows_are_pruned_on_open() {
        let temp = TempDb::new("prune");
        let current_key = format!("{}search:keepme", cache_key_prefix());
        {
            let db = temp.open();
            db.put_cached_json(&current_key, "{\"fresh\":true}").unwrap();

            let conn = db.conn().unwrap();
            // A row written by some earlier app version: different key prefix,
            // different written_version.
            conn.execute(
                "INSERT INTO search_cache (cache_key, payload_json, fetched_at) VALUES (?1, ?2, ?3)",
                params!["0.0.0-old:search:dropme", "{}", 0i64],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO content_meta_cache
                   (instance_id, category, file_name, size_bytes, mtime_unix, name, icon, written_version, mod_id)
                 VALUES ('inst', 'mod', 'stale.jar', 1, 1, 'Stale', NULL, '0.0.0-old', 'stale')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO content_meta_cache
                   (instance_id, category, file_name, size_bytes, mtime_unix, name, icon, written_version, mod_id)
                 VALUES ('inst', 'mod', 'fresh.jar', 1, 1, 'Fresh', NULL, ?1, 'fresh')",
                params![APP_VERSION],
            )
            .unwrap();
        }

        let db = temp.open();
        assert!(
            db.get_cached_json(&current_key).unwrap().is_some(),
            "current-version search_cache row must survive"
        );
        assert!(
            db.get_cached_json("0.0.0-old:search:dropme").unwrap().is_none(),
            "previous-version search_cache row must be pruned"
        );

        let cached = db.get_content_meta_cache("inst").unwrap();
        let names: Vec<&str> = cached.iter().map(|c| c.file_name.as_str()).collect();
        assert_eq!(names, vec!["fresh.jar"], "only the current version's rows survive");
    }
}
