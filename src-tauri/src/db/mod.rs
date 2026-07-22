mod instances;
pub use instances::CachedContentMeta;

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
            Err(err) => {
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
        }
    }

    fn open_at(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
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
        ] {
            let _ = conn.execute(stmt, []);
        }

        // Every cache row this version writes is prefixed/tagged with its
        // own version (see `cache_key_prefix`/`APP_VERSION`) — anything left
        // over from a previous version was computed by different processing
        // logic and is never read back under the new version anyway, so it
        // would just sit here forever without this.
        let _ = conn.execute(
            "DELETE FROM search_cache WHERE cache_key NOT LIKE ?1",
            params![format!("{}%", cache_key_prefix())],
        );
        let _ = conn.execute(
            "DELETE FROM content_meta_cache WHERE written_version != ?1",
            params![APP_VERSION],
        );

        Ok(Self {
            conn: Mutex::new(conn),
        })
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
