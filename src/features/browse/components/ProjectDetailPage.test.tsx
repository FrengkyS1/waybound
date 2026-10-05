import { mockIPC } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ModDetail, ModpackContentResponse } from "../detailTypes";
import { ModpackContentTab } from "./ModpackContentTab";

// Load the real heavy renderer before timing request-ownership assertions.
import "./MarkdownBody";
const detail: ModDetail = {
  summary: {
    uid: "curseforge:123", curseforgeId: 123, slug: "pack", name: "Pack", description: "Pack details",
    author: "Author", iconUrl: null, downloads: 0, projectType: "modpack", loaders: ["fabric"],
    sources: ["curseforge"], updatedAt: "2026-01-01",
  },
  body: "", bodyFormat: "plain", categories: [], gameVersions: ["1.20.1"], loaders: ["fabric"], gallery: [],
  versions: ["one", "two"].map((id) => ({
    id, name: `Version ${id}`, versionNumber: id, publishedAt: "2026-01-01", gameVersions: ["1.20.1"], loaders: [], downloads: 0,
  })),
  suggestedInstance: { name: "Pack", minecraftVersion: "1.20.1", loader: "fabric" },
};

function content(name: string): ModpackContentResponse {
  return {
    versionId: "two", versionName: "Version two",
    items: [{ id: name, name, fileName: `${name}.jar`, kind: "mod", required: true }],
    counts: { mods: 1, datapacks: 0, resourcepacks: 0, shaders: 0, worlds: 0, other: 0 },
  };
}

// Repo targets ES2020, whose Promise type does not expose withResolvers.
describe("pack content request identity", () => {
  it.each(["resolve", "reject"] as const)("ignores stale %s and finally while the next version loads", async (outcome) => {
    let finish!: (value: ModpackContentResponse) => void;
    let fail!: (reason: Error) => void;
    let finishNext!: (value: ModpackContentResponse) => void;
    mockIPC((cmd, args) => {
      if (cmd !== "get_modpack_content") return null;
      return args && typeof args === "object" && "versionId" in args && args.versionId === "one"
        ? new Promise<ModpackContentResponse>((resolve, reject) => { finish = resolve; fail = reject; })
        : new Promise<ModpackContentResponse>((resolve) => { finishNext = resolve; });
    });
    const page = render(<ModpackContentTab detail={detail} selectedVersionId="one" />);
    page.rerender(<ModpackContentTab detail={detail} selectedVersionId="two" />);
    await act(async () => { if (outcome === "resolve") finish(content("Old content")); else fail(new Error("Old request failed")); });
    expect(screen.getByText("Loading modpack content…")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.queryByText("Old content")).not.toBeInTheDocument();
    await act(async () => { finishNext(content("Latest content")); });
    expect(await screen.findByText("Latest content")).toBeInTheDocument();
  });

  it("shows content errors and refetches the selected version on retry", async () => {
    let failed = true;
    mockIPC((cmd) => {
      if (cmd === "get_modpack_content") {
        if (failed) throw new Error("Content unavailable");
        return content("Recovered content");
      }
      return null;
    });
    render(<ModpackContentTab detail={detail} selectedVersionId="two" />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Content unavailable");
    failed = false;
    fireEvent.click(screen.getByRole("button", { name: "Retry content" }));
    expect(await screen.findByText("Recovered content")).toBeInTheDocument();
  });
});

describe("project changelog request identity", () => {
  it.each(["resolve", "reject"] as const)("ignores stale %s and finally after changing versions", async (outcome) => {
    let finish!: (value: string) => void;
    let fail!: (reason: Error) => void;
    let finishNext!: (value: string) => void;
    mockIPC((cmd, args) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "get_mod_details") return detail;
      if (cmd === "get_version_changelog") return args && typeof args === "object" && "versionId" in args && args.versionId === "one"
        ? new Promise<string>((resolve, reject) => { finish = resolve; fail = reject; })
        : new Promise<string>((resolve) => { finishNext = resolve; });
      return null;
    });
    // ProjectDetailPage transitively registers IPC listeners at module load.
    const { ProjectDetailPage } = await import("./ProjectDetailPage");
    render(<ProjectDetailPage summary={detail.summary} onBack={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Changelog" }));
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "two" } });
    await act(async () => { if (outcome === "resolve") finish("Old changelog"); else fail(new Error("Old changelog failed")); });
    expect(screen.getByText("Loading changelog…")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.queryByText("Old changelog")).not.toBeInTheDocument();
    await act(async () => { finishNext("Latest changelog"); });
    expect(await screen.findByText("Latest changelog")).toBeInTheDocument();
  });

  it("shows changelog failures instead of an empty-state message and supports retry", async () => {
    let failed = true;
    mockIPC((cmd) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "get_mod_details") return detail;
      if (cmd === "get_version_changelog") {
        if (failed) throw new Error("Changelog unavailable");
        return "Recovered changelog";
      }
      return null;
    });
    // Import follows mockIPC because installStore wires listeners at import.
    const { ProjectDetailPage } = await import("./ProjectDetailPage");
    render(<ProjectDetailPage summary={detail.summary} onBack={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Changelog" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Changelog unavailable");
    expect(screen.queryByText("No changelog for this version.")).not.toBeInTheDocument();
    failed = false;
    fireEvent.click(screen.getByRole("button", { name: "Retry changelog" }));
    expect(await screen.findByText("Recovered changelog")).toBeInTheDocument();
  });
});
