import { invoke } from "@tauri-apps/api/core";
import type {
  CreateInstanceInput,
  GameVersionOption,
  InstalledMod,
  InstanceSummary,
} from "./types";

export async function fetchInstances(): Promise<InstanceSummary[]> {
  return invoke<InstanceSummary[]>("list_instances");
}

export async function createInstance(input: CreateInstanceInput): Promise<InstanceSummary> {
  return invoke<InstanceSummary>("create_instance", { input });
}

export async function deleteInstance(instanceId: string): Promise<void> {
  await invoke("delete_instance", { instanceId });
}

export async function renameInstance(instanceId: string, name: string): Promise<void> {
  await invoke("rename_instance", { instanceId, name });
}

/** Clones an instance (files, mods, launch config, icon) with fresh play stats. */
export async function duplicateInstance(instanceId: string): Promise<InstanceSummary> {
  return invoke<InstanceSummary>("duplicate_instance", { instanceId });
}

export type ContentCategory = "mod" | "resourcepack" | "shaderpack";

export interface ContentEntry {
  fileName: string;
  /** The mod's own declared display name, when readable from its jar. */
  name?: string;
  /** A `data:` URL (embedded) or remote URL (from a tracked install). */
  icon?: string;
  enabled: boolean;
  sizeBytes: number;
  /** True when `name`/`icon` came from the backend's on-disk metadata cache
   * already — no need to fetch this row's metadata again. */
  metaResolved: boolean;
  /** True when at least one file/folder under `config/` looks like it
   * belongs to this mod — gates showing the Config button at all. */
  hasConfig: boolean;
}

export interface InstanceContent {
  mods: ContentEntry[];
  resourcePacks: ContentEntry[];
  shaderPacks: ContentEntry[];
}

export async function fetchInstanceContent(instanceId: string): Promise<InstanceContent> {
  return invoke<InstanceContent>("list_instance_content", { instanceId });
}

export interface ContentMeta {
  name?: string;
  icon?: string;
}

/** Resolves one file's display name + icon on demand — call as a row scrolls into view. */
export async function fetchContentMeta(
  instanceId: string,
  category: ContentCategory,
  fileName: string,
): Promise<ContentMeta> {
  return invoke<ContentMeta>("get_content_meta", { instanceId, category, fileName });
}

export async function setContentEnabled(
  instanceId: string,
  category: ContentCategory,
  fileName: string,
  enabled: boolean,
): Promise<void> {
  await invoke("set_content_enabled", { instanceId, category, fileName, enabled });
}

export async function removeContentFile(
  instanceId: string,
  category: ContentCategory,
  fileName: string,
): Promise<void> {
  await invoke("remove_content_file", { instanceId, category, fileName });
}

/** Re-resolves a mod already on disk against its own CurseForge/Modrinth
 * project and installs whatever the instance's version+loader currently
 * resolve to. Rejects with a clear error for a file with no tracked project
 * (a modpack-dropped or manually-added jar). */
export async function updateModInInstance(
  instanceId: string,
  fileName: string,
  installId: string,
): Promise<import("../browse/detailTypes").InstallModResult> {
  return invoke("update_mod_in_instance", { instanceId, fileName, installId });
}

/** Resolves a Content-tab mod back to a `ModSummary` for opening its project
 * page in Browse. Rejects for a file with no tracked project (a
 * modpack-dropped or manually-added jar has nothing to open). */
export async function fetchModSummaryForContent(
  instanceId: string,
  fileName: string,
): Promise<import("../browse/types").ModSummary> {
  return invoke("get_mod_summary_for_content", { instanceId, fileName });
}

export interface ConfigFileEntry {
  /** Relative to the instance's config/ folder — pass straight back to
   * readConfigFile/writeConfigFile, never construct a path yourself. */
  relativePath: string;
  displayName: string;
}

/** Every editable config file that plausibly belongs to one mod. Empty
 * (not an error) when nothing matches. */
export async function listModConfigs(
  instanceId: string,
  fileName: string,
): Promise<ConfigFileEntry[]> {
  return invoke("list_mod_configs", { instanceId, fileName });
}

export async function readConfigFile(
  instanceId: string,
  relativePath: string,
): Promise<string> {
  return invoke("read_config_file", { instanceId, relativePath });
}

export async function writeConfigFile(
  instanceId: string,
  relativePath: string,
  contents: string,
): Promise<void> {
  await invoke("write_config_file", { instanceId, relativePath, contents });
}

export async function setInstanceIcon(
  instanceId: string,
  icon: string | null,
): Promise<void> {
  await invoke("set_instance_icon", { instanceId, icon });
}

export async function fetchInstanceMods(instanceId: string): Promise<InstalledMod[]> {
  return invoke<InstalledMod[]>("list_instance_mods", { instanceId });
}

export async function removeModFromInstance(
  instanceId: string,
  modUid: string,
): Promise<void> {
  await invoke("remove_mod_from_instance", { instanceId, modUid });
}

export async function fetchMinecraftVersions(): Promise<GameVersionOption[]> {
  return invoke<GameVersionOption[]>("list_minecraft_versions");
}
