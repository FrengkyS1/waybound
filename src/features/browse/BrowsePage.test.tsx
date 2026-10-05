import { mockIPC } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BrowsePage as BrowsePageComponent } from "./BrowsePage";
import type { ModSearchQuery, ModSearchResult, ModSummary } from "./types";

let BrowsePage: typeof BrowsePageComponent;
const originalScrollTo = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollTo");

const earlier: ModSummary = {
  uid: "modrinth:earlier",
  slug: "earlier",
  name: "Earlier project",
  description: "Earlier page result",
  author: "Author",
  iconUrl: null,
  downloads: 1,
  projectType: "mod",
  loaders: ["fabric"],
  sources: ["modrinth"],
  updatedAt: "2026-10-03T00:00:00Z",
  modrinthId: "earlier",
};

beforeEach(() => {
  vi.resetModules();
  Object.defineProperty(HTMLElement.prototype, "scrollTo", {
    configurable: true,
    value: vi.fn(),
  });
});

afterEach(() => {
  if (originalScrollTo) {
    Object.defineProperty(HTMLElement.prototype, "scrollTo", originalScrollTo);
  } else {
    const prototype: Partial<HTMLElement> = HTMLElement.prototype;
    delete prototype.scrollTo;
  }
});

describe("Browse pagination recovery", () => {
  it.each([false, true])("keeps Previous usable after empty or failed later page (failed=%s)", async (failed) => {
    const queries: ModSearchQuery[] = [];
    mockIPC((command, args) => {
      if (command !== "search_mods") return undefined;
      // Payload comes from this test's typed browse-store invoke, not external IPC.
      const payload = args as { query: ModSearchQuery };
      const query = payload.query;
      queries.push(query);
      if (query.offset > 0 && failed) throw new Error("Both sources unavailable.");
      const result: ModSearchResult = query.offset > 0
        ? { hits: [], offset: 50, limit: 50, totalHits: 1, warnings: [] }
        : { hits: [earlier], offset: 0, limit: 50, totalHits: 100, warnings: [] };
      return result;
    });
    // Exercise import-time Tauri listeners only after installing the IPC boundary.
    ({ BrowsePage } = await import("./BrowsePage"));
    render(<BrowsePage />);
    expect(await screen.findByText("Earlier project")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Next →" }));
    expect(await screen.findByText(failed ? "Both sources unavailable." : "No results on this page"))
      .toBeInTheDocument();
    const previous = screen.getByRole("button", { name: "← Previous" });
    expect(previous).toBeEnabled();
    expect(screen.getByRole("button", { name: "Next →" })).toBeDisabled();
    fireEvent.click(previous);
    expect(await screen.findByText("Earlier project")).toBeInTheDocument();
    await waitFor(() => expect(queries.map((query) => query.offset)).toEqual([0, 50, 0]));
  });
});
