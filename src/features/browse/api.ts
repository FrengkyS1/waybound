import { invoke } from "@tauri-apps/api/core";
import type { ModSummary } from "./types";
import type {
  ActivityLogEntry,
  InstallModInput,
  InstallModResult,
  MissingMod,
  ModDetail,
  ModpackContentResponse,
} from "./detailTypes";

export async function fetchModDetails(summary: ModSummary): Promise<ModDetail> {
  return invoke<ModDetail>("get_mod_details", { summary });
}

export async function fetchModpackContent(
  summary: ModSummary,
  versionId?: string,
): Promise<ModpackContentResponse> {
  return invoke<ModpackContentResponse>("get_modpack_content", {
    summary,
    versionId,
  });
}

export async function fetchVersionChangelog(
  summary: ModSummary,
  versionId: string,
): Promise<string | null> {
  return invoke<string | null>("get_version_changelog", { summary, versionId });
}

export async function fetchActivityLogs(
  limit = 100,
): Promise<ActivityLogEntry[]> {
  return invoke<ActivityLogEntry[]>("get_activity_logs", { limit });
}

export async function installMod(
  input: InstallModInput,
  installId: string,
): Promise<InstallModResult> {
  return invoke<InstallModResult>("install_mod_to_instance", {
    input,
    installId,
  });
}

export async function cancelInstall(installId: string): Promise<void> {
  return invoke<void>("cancel_install", { installId });
}

export async function openMissingModsBrowser(url: string): Promise<void> {
  return invoke<void>("open_missing_mods_browser", { url });
}

export async function openAllMissingModsBrowsers(urls: string[]): Promise<void> {
  return invoke<void>("open_all_missing_mods_browsers", { urls });
}

export interface PendingMissingMods {
  instanceId: string;
  instanceName: string;
  missingMods: MissingMod[];
}

/** Whatever's still outstanding from a previous session's modpack imports —
 * the in-memory install list that normally tracks this is gone after a
 * restart, so this re-derives it from the same per-instance manifest the
 * backend already persists for reconciliation. */
export async function fetchPendingMissingMods(): Promise<PendingMissingMods[]> {
  return invoke<PendingMissingMods[]>("list_pending_missing_mods");
}

export async function watchForMissingMods(
  instanceId: string,
  mods: MissingMod[],
): Promise<void> {
  return invoke<void>("watch_for_missing_mods", { instanceId, mods });
}
