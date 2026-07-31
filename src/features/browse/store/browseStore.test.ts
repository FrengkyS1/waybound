import { mockIPC } from "@tauri-apps/api/mocks";
import { beforeEach, describe, expect, it } from "vitest";

import { useBrowseStore } from "./browseStore";
import type { ModSearchQuery, ModSearchResult, ModSummary } from "../types";

function hit(name: string): ModSummary {
  return {
    uid: `mod:slug:${name}`,
    slug: name,
    name,
    description: "",
    author: "",
    iconUrl: null,
    downloads: 0,
    projectType: "mod",
    loaders: ["fabric"],
    sources: ["modrinth"],
    updatedAt: "",
  };
}

function result(over: Partial<ModSearchResult> = {}): ModSearchResult {
  return { hits: [hit("sodium")], offset: 0, limit: 50, totalHits: 500, ...over };
}

/** Records every `search_mods` payload and replies with canned results. */
function serve(...replies: ModSearchResult[]) {
  const queries: ModSearchQuery[] = [];
  let i = 0;
  mockIPC((cmd, args) => {
    if (cmd !== "search_mods") throw new Error(`unexpected command: ${cmd}`);
    queries.push((args as { query: ModSearchQuery }).query);
    return replies[Math.min(i++, replies.length - 1)];
  });
  return queries;
}

const INITIAL = useBrowseStore.getState();

beforeEach(() => {
  useBrowseStore.setState({
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
  });
});

describe("search", () => {
  it("sends the trimmed query and current filters, and stores the response", async () => {
    const queries = serve(
      result({ hits: [hit("sodium"), hit("lithium")], totalHits: 2, warnings: ["cf down"] }),
    );
    useBrowseStore.setState({ query: "  sodium  ", loader: "fabric", sort: "updated" });

    await useBrowseStore.getState().search();

    expect(queries).toEqual([
      {
        query: "sodium",
        contentType: "mod",
        loader: "fabric",
        sort: "updated",
        offset: 0,
        limit: 50,
      },
    ]);
    const s = useBrowseStore.getState();
    expect(s.results.map((r) => r.name)).toEqual(["sodium", "lithium"]);
    expect(s.totalHits).toBe(2);
    expect(s.loading).toBe(false);
    expect(s.error).toBeNull();
    expect(s.warnings).toEqual(["cf down"]);
  });

  it("resets the offset by default and keeps it when asked not to", async () => {
    const queries = serve(result({ offset: 0 }), result({ offset: 100 }));
    useBrowseStore.setState({ offset: 100 });

    await useBrowseStore.getState().search();
    expect(queries[0].offset).toBe(0);
    expect(useBrowseStore.getState().offset).toBe(0);

    useBrowseStore.setState({ offset: 100 });
    await useBrowseStore.getState().search(false);
    expect(queries[1].offset).toBe(100);
    expect(useBrowseStore.getState().offset).toBe(100);
  });

  it("adopts the offset and limit the backend actually answered with", async () => {
    serve(result({ offset: 100, limit: 20 }));
    await useBrowseStore.getState().search(false);

    expect(useBrowseStore.getState().offset).toBe(100);
    expect(useBrowseStore.getState().limit).toBe(20);
  });

  it("defaults warnings to an empty array when the backend omits them", async () => {
    serve({ hits: [hit("a")], offset: 0, limit: 50, totalHits: 1 });
    await useBrowseStore.getState().search();
    expect(useBrowseStore.getState().warnings).toEqual([]);
  });

  it("clears results and reports the message when the search fails", async () => {
    useBrowseStore.setState({ results: [hit("stale")], totalHits: 99, warnings: ["old"] });
    mockIPC(() => {
      throw new Error("network is down");
    });

    await useBrowseStore.getState().search();

    const s = useBrowseStore.getState();
    expect(s.error).toBe("network is down");
    expect(s.results).toEqual([]);
    expect(s.totalHits).toBe(0);
    expect(s.warnings).toEqual([]);
    expect(s.loading).toBe(false);
  });
});

describe("totalHits clamp", () => {
  it("clamps an inflated total down to the offset when a later page comes back empty", async () => {
    // The backend's totalHits is the pre-dedupe, pre-loader-filter sum, so it
    // can promise pages that don't exist. Landing on an empty one is the
    // only reliable signal of where the real end is.
    serve(result({ hits: [], offset: 100, limit: 50, totalHits: 500 }));

    await useBrowseStore.getState().search(false);

    expect(useBrowseStore.getState().totalHits).toBe(100);
    expect(useBrowseStore.getState().results).toEqual([]);
  });

  it("stops offering a Next page once the total has been clamped", async () => {
    const queries = serve(result({ hits: [], offset: 100, limit: 50, totalHits: 500 }));
    await useBrowseStore.getState().search(false);
    expect(queries).toHaveLength(1);

    await useBrowseStore.getState().nextPage();

    // Without the clamp, totalHits would still be 500 and this would fire a
    // second request for another guaranteed-empty page.
    expect(queries).toHaveLength(1);
    expect(useBrowseStore.getState().offset).toBe(100);
  });

  it("does not clamp an empty first page, which just means no results at all", async () => {
    // offset 0 with no hits is an honest "nothing matched", not a paging
    // overshoot — clamping here would be indistinguishable but pointless,
    // and must not corrupt a total the backend still stands behind.
    serve(result({ hits: [], offset: 0, limit: 50, totalHits: 42 }));

    await useBrowseStore.getState().search();

    expect(useBrowseStore.getState().totalHits).toBe(42);
  });

  it("does not clamp a non-empty page deep in the results", async () => {
    serve(result({ hits: [hit("a")], offset: 100, limit: 50, totalHits: 500 }));

    await useBrowseStore.getState().search(false);

    expect(useBrowseStore.getState().totalHits).toBe(500);
  });

  it("un-clamps when a fresh search finds a real total again", async () => {
    serve(
      result({ hits: [], offset: 100, limit: 50, totalHits: 500 }),
      result({ hits: [hit("a")], offset: 0, limit: 50, totalHits: 500 }),
    );

    await useBrowseStore.getState().search(false);
    expect(useBrowseStore.getState().totalHits).toBe(100);

    await useBrowseStore.getState().search();
    expect(useBrowseStore.getState().totalHits).toBe(500);
  });
});

describe("nextPage / prevPage", () => {
  it("advances by one page and searches at the new offset", async () => {
    const queries = serve(result({ hits: [hit("a")], offset: 50, limit: 50, totalHits: 500 }));
    useBrowseStore.setState({ offset: 0, limit: 50, totalHits: 500 });

    await useBrowseStore.getState().nextPage();

    expect(queries).toEqual([expect.objectContaining({ offset: 50 })]);
    expect(useBrowseStore.getState().offset).toBe(50);
  });

  it("refuses to page past the last page", async () => {
    const queries = serve(result());
    useBrowseStore.setState({ offset: 450, limit: 50, totalHits: 500 });

    await useBrowseStore.getState().nextPage();

    expect(queries).toEqual([]);
    expect(useBrowseStore.getState().offset).toBe(450);
  });

  it("refuses to page forward with no results at all", async () => {
    const queries = serve(result());
    useBrowseStore.setState({ offset: 0, limit: 50, totalHits: 0 });

    await useBrowseStore.getState().nextPage();

    expect(queries).toEqual([]);
  });

  it("goes back one page", async () => {
    const queries = serve(result({ hits: [hit("a")], offset: 50, limit: 50 }));
    useBrowseStore.setState({ offset: 100, limit: 50, totalHits: 500 });

    await useBrowseStore.getState().prevPage();

    expect(queries).toEqual([expect.objectContaining({ offset: 50 })]);
  });

  it("does nothing on the first page", async () => {
    const queries = serve(result());
    useBrowseStore.setState({ offset: 0, limit: 50, totalHits: 500 });

    await useBrowseStore.getState().prevPage();

    expect(queries).toEqual([]);
  });

  it("never walks the offset negative", async () => {
    const queries = serve(result({ hits: [hit("a")], offset: 0, limit: 50 }));
    // A limit change can leave the offset off-grid; going back must land on 0.
    useBrowseStore.setState({ offset: 30, limit: 50, totalHits: 500 });

    await useBrowseStore.getState().prevPage();

    expect(queries).toEqual([expect.objectContaining({ offset: 0 })]);
    expect(useBrowseStore.getState().offset).toBe(0);
  });
});

describe("stale-response guard", () => {
  it("ignores a slow search that resolves after a newer one", async () => {
    const resolvers: ((r: ModSearchResult) => void)[] = [];
    mockIPC((cmd) => {
      if (cmd !== "search_mods") throw new Error(`unexpected command: ${cmd}`);
      return new Promise<ModSearchResult>((resolve) => resolvers.push(resolve));
    });

    const slow = useBrowseStore.getState().search();
    const fast = useBrowseStore.getState().search();
    expect(resolvers).toHaveLength(2);

    // The newer search wins the race...
    resolvers[1](result({ hits: [hit("fresh")], totalHits: 1 }));
    await fast;
    expect(useBrowseStore.getState().results.map((r) => r.name)).toEqual(["fresh"]);

    // ...and the older one landing afterwards must change nothing.
    resolvers[0](result({ hits: [hit("stale")], totalHits: 999, offset: 300 }));
    await slow;

    const s = useBrowseStore.getState();
    expect(s.results.map((r) => r.name)).toEqual(["fresh"]);
    expect(s.totalHits).toBe(1);
    expect(s.offset).toBe(0);
    expect(s.loading).toBe(false);
  });

  it("ignores a slow search that FAILS after a newer one succeeded", async () => {
    const settle: { resolve: (r: ModSearchResult) => void; reject: (e: Error) => void }[] = [];
    mockIPC((cmd) => {
      if (cmd !== "search_mods") throw new Error(`unexpected command: ${cmd}`);
      return new Promise<ModSearchResult>((resolve, reject) => settle.push({ resolve, reject }));
    });

    const slow = useBrowseStore.getState().search();
    const fast = useBrowseStore.getState().search();

    settle[1].resolve(result({ hits: [hit("fresh")], totalHits: 1 }));
    await fast;

    settle[0].reject(new Error("timed out"));
    await slow;

    const s = useBrowseStore.getState();
    // A stale failure must not blank the good results or flash an error.
    expect(s.error).toBeNull();
    expect(s.results.map((r) => r.name)).toEqual(["fresh"]);
    expect(s.totalHits).toBe(1);
  });

  it("still applies the newest response when it is the slow one", async () => {
    const resolvers: ((r: ModSearchResult) => void)[] = [];
    mockIPC((cmd) => {
      if (cmd !== "search_mods") throw new Error(`unexpected command: ${cmd}`);
      return new Promise<ModSearchResult>((resolve) => resolvers.push(resolve));
    });

    const first = useBrowseStore.getState().search();
    const second = useBrowseStore.getState().search();

    resolvers[0](result({ hits: [hit("stale")], totalHits: 9 }));
    await first;
    resolvers[1](result({ hits: [hit("fresh")], totalHits: 1 }));
    await second;

    expect(useBrowseStore.getState().results.map((r) => r.name)).toEqual(["fresh"]);
    expect(useBrowseStore.getState().totalHits).toBe(1);
  });
});

describe("filter setters", () => {
  it("update state without searching on their own", async () => {
    const queries = serve(result());

    useBrowseStore.getState().setQuery("create");
    useBrowseStore.getState().setContentType("modpack");
    useBrowseStore.getState().setLoader("forge");
    useBrowseStore.getState().setSort("relevance");

    const s = useBrowseStore.getState();
    expect(s.query).toBe("create");
    expect(s.contentType).toBe("modpack");
    expect(s.loader).toBe("forge");
    expect(s.sort).toBe("relevance");
    // Debouncing/triggering is the caller's job, not the store's.
    expect(queries).toEqual([]);

    await useBrowseStore.getState().search();
    expect(queries[0]).toMatchObject({ query: "create", contentType: "modpack", loader: "forge" });
  });

  it("ships with the defaults the Browse page expects", () => {
    expect(INITIAL.contentType).toBe("mod");
    expect(INITIAL.sort).toBe("downloads");
    expect(INITIAL.limit).toBe(50);
    expect(INITIAL.offset).toBe(0);
  });
});
