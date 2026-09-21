export type ModLoader = "fabric" | "forge" | "neoforge" | "quilt" | "vanilla";

export interface DetectedLauncherInstance {
  path: string;
  folder: string;
  name: string;
  minecraft: string;
  loader: ModLoader;
  loaderVersion: string | null;
}

export interface DetectedLauncher {
  name: string;
  root: string;
  instances: DetectedLauncherInstance[];
}

export interface InstanceSummary {
  id: string;
  name: string;
  minecraftVersion: string;
  loader: ModLoader;
  loaderVersion?: string;
  modCount: number;
  createdAt: number;
  rootPath: string;
  icon?: string | null;
  lastPlayed?: number | null;
  totalPlaySeconds: number;
  /** The installed modpack's own version/filename label, recorded at import
   * time. Absent for a manually-created instance or one that only ever had
   * individual mods installed. */
  modpackVersionLabel?: string | null;
  /** The installed modpack's project uid (e.g. `"curseforge:12345"`),
   * recorded at import time — the handle for offering the pack's other
   * versions for in-place switching. */
  modpackProjectUid?: string | null;
}

export interface CreateInstanceInput {
  name: string;
  minecraftVersion: string;
  loader: ModLoader;
  loaderVersion?: string;
}

export interface InstalledMod {
  id: number;
  instanceId: string;
  modUid: string;
  modName: string;
  source: "modrinth" | "curseforge";
  fileName: string;
  installedAt: number;
}

export interface GameVersionOption {
  version: string;
  versionType: string;
}
