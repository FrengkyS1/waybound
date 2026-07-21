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
}
