# ADR-002: CurseForge API key storage

## Status

Accepted

## Date

2026-07-08

## Context

CurseForge requires an API key for all REST calls. The key must never ship in source, never be exposed to the frontend after save, and must persist across app restarts.

## Options Considered

### Option A: Plain TOML in app config directory
- Pros: Simple, no extra dependencies, easy to debug, matches spec ("local config file")
- Cons: Key at rest is not encrypted (relies on OS user profile permissions)

### Option B: OS keychain via `keyring` crate
- Pros: Encrypted at rest, platform-native
- Cons: More complex, harder to reset/migrate, cross-platform quirks

### Option C: Frontend localStorage
- Pros: Trivial to implement
- Cons: Violates security boundary; key visible to webview; rejected

## Decision

We choose **Option A** for v1: `config.toml` under the OS config directory (`%APPDATA%/dev.waybound/` on Windows).

The Rust `ConfigStore` loads at startup and persists on change. Tauri commands expose only `configured: bool` to the frontend — never the key value.

## Consequences

- Users register a key at [console.curseforge.com](https://console.curseforge.com) and paste it in Settings
- `config.toml` is gitignored; document path in README
- OS keychain migration can be ADR-003 if needed later
- CurseForge loader filters require `gameVersion` per API — loader filter is skipped on CF until instance/version context exists
