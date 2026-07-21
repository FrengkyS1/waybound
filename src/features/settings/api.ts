import { invoke } from "@tauri-apps/api/core";
import type { McOptions } from "./types";

export type CurseForgeKeySource = "config" | "environment";

export interface CurseForgeStatus {
  configured: boolean;
  source?: CurseForgeKeySource;
  environmentAvailable: boolean;
  defaultDockerEnvPath?: string;
}

export interface CurseForgeProbeResult {
  ok: boolean;
  httpStatus: number;
  keyLength: number;
  keyPrefix: string;
  message: string;
  log: string[];
}

export async function fetchCurseForgeStatus(): Promise<CurseForgeStatus> {
  return invoke<CurseForgeStatus>("get_curseforge_status");
}

export async function saveCurseForgeApiKey(
  apiKey: string,
  skipValidation = true,
): Promise<CurseForgeStatus> {
  return invoke<CurseForgeStatus>("set_curseforge_api_key", {
    apiKey,
    skipValidation,
  });
}

export async function clearCurseForgeApiKey(): Promise<CurseForgeStatus> {
  return invoke<CurseForgeStatus>("clear_curseforge_api_key");
}

export async function importCurseForgeApiKeyFromEnvFile(
  path: string,
  skipValidation = true,
): Promise<CurseForgeStatus> {
  return invoke<CurseForgeStatus>("import_curseforge_api_key_from_env_file", {
    path,
    skipValidation,
  });
}

export async function testSavedCurseForgeApiKey(): Promise<CurseForgeProbeResult> {
  return invoke<CurseForgeProbeResult>("test_curseforge_api_key");
}

export async function testDockerCurseForgeEnvKey(
  envFilePath?: string,
): Promise<CurseForgeProbeResult> {
  return invoke<CurseForgeProbeResult>("test_curseforge_docker_env_key", {
    envFilePath,
  });
}

export async function fetchInstanceOptions(
  instanceId: string,
): Promise<McOptions> {
  return invoke<McOptions>("get_instance_options", { instanceId });
}

export async function saveInstanceOptions(
  instanceId: string,
  options: McOptions,
): Promise<void> {
  await invoke("save_instance_options", { instanceId, options });
}

export interface GlobalMcOptionsStatus {
  configured: boolean;
  applyToNewInstances: boolean;
  options: McOptions;
}

export async function fetchGlobalMcOptions(): Promise<GlobalMcOptionsStatus> {
  return invoke<GlobalMcOptionsStatus>("get_global_mc_options");
}

export async function saveGlobalMcOptions(
  options: McOptions,
  applyToNewInstances: boolean,
): Promise<GlobalMcOptionsStatus> {
  return invoke<GlobalMcOptionsStatus>("save_global_mc_options", {
    options,
    applyToNewInstances,
  });
}
