# ADR-004: Instance storage and mod install

## Status

Accepted

## Date

2026-07-08

## Context

Step 7 adds named Minecraft instances (version + loader) with per-instance mod folders. Mods are installed from Browse search results by downloading compatible files from Modrinth (preferred) or CurseForge.

## Decision

- **On disk:** `%APPDATA%/dev.waybound/instances/{uuid}/mods/*.jar`
- **In SQLite:** `instances` and `instance_mods` tables in `library.db`
- **Install flow:** Resolve latest compatible file for instance MC version + loader → download to `mods/` → record in DB
- **Remove flow:** Delete DB row + remove jar from disk

Modpack/shader install deferred to step 8.

## Consequences

- Instances are launcher-agnostic folders Prism/MultiMC-style apps can point at.
- Update checking (step 10) can compare installed files against source APIs using stored `mod_uid` and source ids.
