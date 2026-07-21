# ADR-003: SQLite for local library index and search cache

## Status

Accepted

## Date

2026-07-08

## Context

Waybound needs local-first browsing: cross-source mod identity mappings and cached search results so re-browsing does not always hit Modrinth/CurseForge. Step 6 adds persistence before instance management (step 7).

## Decision

Use **rusqlite** (bundled SQLite) with a single database at `%APPDATA%/dev.waybound/library.db` (via `dirs::data_dir()`).

### Tables (v1)

| Table | Purpose |
|-------|---------|
| `mod_identity` | Maps `mod_uid` → `curseforge_id`, `modrinth_id`, slug, name |
| `search_cache` | Serialized `ModSearchResult` keyed by query hash, 15-minute TTL |

Search flow:

1. Build cache key (includes `modrinth_only` flag when query is empty).
2. Return cached result if fresh.
3. On network search, upsert identities from deduped hits and write cache.

CurseForge is skipped when the browse query is empty (Modrinth-only default); cache keys reflect that.

## Consequences

- Positive: Faster repeat searches; identity table ready for step 7 instance mod lists.
- Negative: Cache invalidation is TTL-only for now (no manual clear UI).
- Future: Instance/mod install tables in the same DB; optional cache bust command.
