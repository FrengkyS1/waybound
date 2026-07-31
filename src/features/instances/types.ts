export type ModLoader = "fabric" | "forge" | "neoforge" | "quilt" | "vanilla";

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
