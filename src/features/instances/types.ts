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
