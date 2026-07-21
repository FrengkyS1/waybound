import type { ContentType, ModLoader, ModSource, ModSummary } from "./types";

export type BodyFormat = "markdown" | "html" | "plain";

export interface GalleryItem {
  url: string;
  title?: string;
  description?: string;
  thumbnailUrl?: string;
}

export interface ModVersionSummary {
  id: string;
  name: string;
  versionNumber: string;
  publishedAt: string;
  gameVersions: string[];
  loaders: ModLoader[];
  downloads: number;
  changelog?: string;
}

export type ModpackContentKind =
  "mod" | "datapack" | "resourcepack" | "shader" | "world" | "other";

export interface ModpackContentItem {
  id: string;
  name: string;
  fileName: string;
  author?: string;
  kind: ModpackContentKind;
  required: boolean;
  envClient?: string;
  envServer?: string;
}

export interface ModpackContentCounts {
  mods: number;
  datapacks: number;
  resourcepacks: number;
  shaders: number;
  worlds: number;
  other: number;
}

export interface ModpackContentResponse {
  versionId: string;
  versionName: string;
  items: ModpackContentItem[];
  counts: ModpackContentCounts;
}

export interface ActivityLogEntry {
  timestamp: number;
  level: string;
  message: string;
  projectUid?: string;
}

export interface SuggestedInstance {
  name: string;
  minecraftVersion: string;
  loader: ModLoader;
}

export interface ModDetail {
  summary: ModSummary;
  body: string;
  bodyFormat: BodyFormat;
  categories: string[];
  gameVersions: string[];
  loaders: ModLoader[];
  externalUrl?: string;
  commentsUrl?: string;
  gallery: GalleryItem[];
  versions: ModVersionSummary[];
  suggestedInstance: SuggestedInstance;
}

export interface InstallModInput {
  modSummary: ModSummary;
  source?: ModSource | null;
  instanceId?: string;
  versionId?: string;
  createInstance?: {
    name: string;
    minecraftVersion: string;
    loader: ModLoader;
  };
}

export interface InstallModResult {
  /** Null when the file needs a manual download (see missingMods) — nothing
   * was actually installed yet in that case. */
  installed: {
    id: number;
    instanceId: string;
    modUid: string;
    modName: string;
    source: ModSource;
    fileName: string;
    installedAt: number;
  } | null;
  message: string;
  instance: {
    id: string;
    name: string;
    minecraftVersion: string;
    loader: ModLoader;
    modCount: number;
    rootPath: string;
  };
  hasSkipped: boolean;
  missingMods: MissingMod[];
}

export interface MissingMod {
  projectId: number;
  name: string;
  filename: string;
  url: string;
  sha1?: string | null;
}

export function isInstallableType(type: ContentType): boolean {
  return type === "mod" || type === "modpack" || type === "resourcepack";
}

export function installVerb(type: ContentType): string {
  if (type === "modpack") return "Install modpack";
  return "Install";
}
