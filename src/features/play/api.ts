import { invoke } from "@tauri-apps/api/core";

export interface AccountPublic {
  uuid: string;
  username: string;
}

export interface DeviceCodePrompt {
  userCode: string;
  verificationUri: string;
  message: string;
}

export interface JavaRuntime {
  path: string;
  majorVersion: number;
  versionString: string;
}

export interface LaunchSettings {
  detected: JavaRuntime[];
  javaPath: string | null;
  maxMemoryMb: number;
  jvmArgs: string | null;
}

export interface InstanceLaunchConfig {
  javaPath: string | null;
  maxMemoryMb: number | null;
  jvmArgs: string | null;
}

// ---- Account -------------------------------------------------------------

export async function getAccount(): Promise<AccountPublic | null> {
  return invoke<AccountPublic | null>("get_account");
}

/** Runs the full device-code flow; resolves once the user finishes signing in. */
export async function microsoftLogin(): Promise<AccountPublic> {
  return invoke<AccountPublic>("microsoft_login");
}

export async function logout(): Promise<void> {
  await invoke("logout");
}

// ---- Launch settings -----------------------------------------------------

export async function getLaunchSettings(): Promise<LaunchSettings> {
  return invoke<LaunchSettings>("get_launch_settings");
}

export async function saveLaunchSettings(
  javaPath: string | null,
  maxMemoryMb: number | null,
  jvmArgs: string | null,
): Promise<void> {
  await invoke("set_launch_settings", { javaPath, maxMemoryMb, jvmArgs });
}

export async function getInstanceLaunchConfig(
  instanceId: string,
): Promise<InstanceLaunchConfig> {
  return invoke<InstanceLaunchConfig>("get_instance_launch_config", {
    instanceId,
  });
}

export async function setInstanceLaunchConfig(
  instanceId: string,
  config: InstanceLaunchConfig,
): Promise<void> {
  await invoke("set_instance_launch_config", { instanceId, config });
}

export async function addPlayTime(
  instanceId: string,
  seconds: number,
): Promise<void> {
  await invoke("add_play_time", { instanceId, seconds });
}

// ---- Launch --------------------------------------------------------------

export async function launchInstance(instanceId: string): Promise<void> {
  await invoke("launch_instance", { instanceId });
}

export async function cancelLaunch(instanceId: string): Promise<void> {
  await invoke("cancel_launch", { instanceId });
}

/** Force-kills a running game. No-op when nothing is running. The backend
 * reports the exit as user-stopped rather than crashed. */
export async function stopGame(instanceId: string): Promise<void> {
  await invoke("stop_game", { instanceId });
}

export interface RunningInstance {
  instanceId: string;
  instanceName: string;
}

/** Instances whose Minecraft process is still alive from a previous Waybound
 * session (the game outlives the launcher closing) — checked once on
 * startup so the Play button doesn't reset to launchable for them. */
export async function getRunningInstances(): Promise<RunningInstance[]> {
  return invoke<RunningInstance[]>("get_running_instances");
}

/** The persisted console output of an instance's most recent run. Empty when
 * it has never been launched, so a crash stays readable after a restart. */
export async function readLaunchLog(instanceId: string): Promise<string[]> {
  return invoke<string[]>("read_launch_log", { instanceId });
}

export interface WrongLoaderFile {
  fileName: string;
  modName?: string;
  detectedLoader: string;
}

export interface MissingDep {
  fileName: string;
  modName?: string;
  depModId: string;
  versionRange?: string;
}

export interface WrongGameVersionFile {
  fileName: string;
  modName?: string;
  /** Game version(s) the jar declares (metadata ranges and/or filename). */
  declared: string;
  /** The instance's game version it was judged against. */
  expected: string;
}

export interface LaunchReadiness {
  checkedFiles: number;
  wrongLoader: WrongLoaderFile[];
  missingDeps: MissingDep[];
  wrongGameVersion: WrongGameVersionFile[];
}

/** Reads every enabled jar's own metadata and reports loader mismatches
 * plus unsatisfied required deps. Advisory — empty lists don't guarantee
 * the game starts, and a failed check never blocks launching. */
export async function checkLaunchReadiness(instanceId: string): Promise<LaunchReadiness> {
  return invoke<LaunchReadiness>("check_launch_readiness", { instanceId });
}

// ---- Event payloads ------------------------------------------------------

export interface LaunchProgressEvent {
  instanceId: string;
  stage: string;
  current: number;
  total: number;
}

export interface LaunchLogEvent {
  instanceId: string;
  stream: "stdout" | "stderr";
  line: string;
}

export interface LaunchExitedEvent {
  instanceId: string;
  code: number | null;
  /** The process exited non-zero, i.e. it crashed rather than being quit. */
  crashed: boolean;
  /** A complete sentence naming the instance and, where the backend could
   * work it out from the crash report or the mod-loader's error block, the
   * actual cause. Null unless `crashed`. */
  crashReason: string | null;
  /** The player pressed Stop (as opposed to the game exiting on its own). */
  stoppedByUser: boolean;
}
