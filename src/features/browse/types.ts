export type ModSource = "modrinth" | "curseforge";

export type ContentType = "mod" | "modpack" | "resourcepack" | "shader";

export type ModLoader = "fabric" | "forge" | "neoforge" | "quilt" | "vanilla";

export type SortIndex = "relevance" | "downloads" | "updated" | "new";

export interface ModSummary {
  /** Canonical project identity: modrinth:<project ID> or curseforge:<project ID>. */
  uid: string;
  slug: string;
  name: string;
  description: string;
  author: string;
  iconUrl: string | null;
  downloads: number;
  projectType: ContentType;
  loaders: ModLoader[];
  sources: ModSource[];
  updatedAt: string;
  curseforgeId?: number | null;
  modrinthId?: string | null;
}

export interface ModSearchQuery {
  query: string;
  contentType?: ContentType;
  loader?: ModLoader;
  sort: SortIndex;
  offset: number;
  limit: number;
}

export interface ModSearchResult {
  hits: ModSummary[];
  offset: number;
  limit: number;
  totalHits: number;
  warnings?: string[];
}
