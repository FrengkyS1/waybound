# ADR-005: Modpack import

## Status

Accepted

## Date

2026-07-08

## Context

Step 8 requires importing CurseForge and Modrinth modpacks into local instances. Users also reported mod downloads failing when version/loader filters were too strict.

## Decision

- **Mod install:** resolve file with fallback chain (exact MC+loader → MC only → best match from all versions).
- **Downloads:** shared HTTP client with User-Agent (not bare `reqwest::get`).
- **Modrinth modpack:** download `.mrpack`, parse `modrinth.index.json`, fetch referenced files, apply `overrides/`.
- **CurseForge modpack:** download zip, parse `manifest.json`, fetch each listed mod via CF download-url API, apply overrides folder.
- **UI:** Browse shows "Import to…" for modpacks; instance option shows MC version + loader to reduce mismatch.

## Consequences

- Modpack import into an existing instance folder; does not auto-create instances from pack metadata yet.
- CurseForge modpack import requires a configured API key.
- Shaders remain out of scope.
