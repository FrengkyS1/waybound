import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type {
  ContentType,
  ModLoader,
  ModSearchQuery,
  ModSearchResult,
  ModSummary,
  SortIndex,
} from "../types";

interface BrowseState {
  query: string;
  contentType: ContentType | undefined;
  loader: ModLoader | undefined;
  sort: SortIndex;
  results: ModSummary[];
  totalHits: number;
  offset: number;
  limit: number;
  loading: boolean;
  error: string | null;
  warnings: string[];
  setQuery: (query: string) => void;
  setContentType: (contentType: ContentType | undefined) => void;
  setLoader: (loader: ModLoader | undefined) => void;
  setSort: (sort: SortIndex) => void;
  nextPage: () => Promise<void>;
  prevPage: () => Promise<void>;
  search: (resetOffset?: boolean) => Promise<void>;
}

// Not store state on purpose — bumping it must never trigger a re-render, it
// only exists to let an in-flight search() detect it's been superseded.
let latestSearchId = 0;

function buildSearchQuery(
  state: BrowseState,
  resetOffset: boolean,
): ModSearchQuery {
  return {
    query: state.query.trim(),
    contentType: state.contentType,
    loader: state.loader,
    sort: state.sort,
    offset: resetOffset ? 0 : state.offset,
    limit: state.limit,
  };
}

export const useBrowseStore = create<BrowseState>((set, get) => ({
  query: "",
  contentType: "mod",
  loader: undefined,
  sort: "downloads",
  results: [],
  totalHits: 0,
  offset: 0,
  limit: 50,
  loading: false,
  error: null,
  warnings: [],

  setQuery: (query) => set({ query }),
  setContentType: (contentType) => set({ contentType }),
  setLoader: (loader) => set({ loader }),
  setSort: (sort) => set({ sort }),

  nextPage: async () => {
    const { offset, limit, totalHits } = get();
    if (offset + limit >= totalHits) return;
    set({ offset: offset + limit });
    await get().search(false);
  },

  prevPage: async () => {
    const { offset, limit } = get();
    if (offset === 0) return;
    set({ offset: Math.max(0, offset - limit) });
    await get().search(false);
  },

  search: async (resetOffset = true) => {
    const requestId = ++latestSearchId;
    const state = get();
    set({
      loading: true,
      error: null,
      warnings: [],
      offset: resetOffset ? 0 : state.offset,
    });

    try {
      const payload = buildSearchQuery(get(), resetOffset);
      const result = await invoke<ModSearchResult>("search_mods", {
        query: payload,
      });

      // A newer search (different filter, next page, ...) started and
      // possibly already resolved while this one was in flight — applying
      // this stale response now would silently overwrite the correct,
      // more-recent results.
      if (requestId !== latestSearchId) return;

      set({
        results: result.hits,
        totalHits: result.totalHits,
        offset: result.offset,
        limit: result.limit,
        loading: false,
        warnings: result.warnings ?? [],
      });
    } catch (error) {
      if (requestId !== latestSearchId) return;
      set({
        loading: false,
        error: error instanceof Error ? error.message : String(error),
        results: [],
        totalHits: 0,
        warnings: [],
      });
    }
  },
}));
