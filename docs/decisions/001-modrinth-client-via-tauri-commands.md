# ADR-001: Modrinth API access via Rust Tauri commands

## Status

Accepted

## Date

2026-07-08

## Context

Waybound aggregates mods from CurseForge and Modrinth. External APIs must not be called from the React frontend — API keys (CurseForge) stay on the Rust side, and we want a single typed boundary between UI and network.

Step 2 adds Modrinth search (no API key required), which establishes the pattern for all future source integrations.

## Options Considered

### Option A: Frontend fetch to Modrinth directly
- Pros: Fast to prototype, less Rust code
- Cons: Violates security boundary; CurseForge key would need a separate pattern; raw API shapes leak into UI; harder to add caching/dedup later

### Option B: Rust `reqwest` client + Tauri commands + internal DTOs
- Pros: Matches project spec; single typed boundary; ready for SQLite cache and dedup pipeline; secrets stay in Rust
- Cons: More boilerplate upfront

### Option C: Local HTTP proxy server inside the app
- Pros: Frontend could use familiar fetch
- Cons: Extra port/process complexity; still need Rust API layer; unnecessary for Tauri

## Decision

We choose **Option B**: a `ModrinthClient` in `src-tauri/src/sources/modrinth.rs` that maps Modrinth responses into internal DTOs (`ModSummary`, `ModSearchResult`), exposed via async Tauri commands.

## Consequences

- Frontend only imports types mirroring Rust DTOs and calls `invoke("search_mods", …)`
- CurseForge will follow the same pattern in step 4
- Network errors surface as structured command errors, not raw HTTP responses
- `reqwest` + `tokio` become core dependencies
