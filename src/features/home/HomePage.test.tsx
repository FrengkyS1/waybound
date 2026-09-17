import { mockIPC } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { HomePage as HomePageComponent } from "./HomePage";

const saved = {
  id: "saved-world", name: "Offline survival", minecraftVersion: "1.20.1",
  loader: "fabric", modCount: 3, createdAt: 1_700_000_000,
  rootPath: "C:/isolated/instances/saved-world", totalPlaySeconds: 0, lastPlayed: null,
};
let discover: () => Promise<unknown>;
let library: () => Promise<unknown>;
let HomePage: typeof HomePageComponent;

beforeEach(() => {
  // HomePage reads the dismiss flag at mount; keep storage local to the test run.
  const storage: Record<string, string> = {};
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => storage[key] ?? null,
    setItem: (key: string, value: string) => { storage[key] = value; },
    removeItem: (key: string) => { delete storage[key]; },
    clear: () => { for (const key of Object.keys(storage)) delete storage[key]; },
  });
  library = () => Promise.resolve([saved]);
  discover = () => Promise.reject(new Error("network unavailable"));
  mockIPC((command) => {
    if (command === "list_instances") return library();
    if (command === "list_minecraft_versions") return discover();
    if (command === "get_curseforge_status") return { configured: false };
    if (command === "plugin:event|listen") return 1;
    return null;
  });
});

async function showLibrary() {
  // Stores register IPC listeners at module load, after mockIPC is installed.
  if (!HomePage) ({ HomePage } = await import("./HomePage"));
  const select = vi.fn();
  render(<HomePage onAddMods={() => {}} onOpenMod={() => {}}
    onOpenSettings={() => {}} selectedId={null} onSelectId={select}
    instanceTab="overview" onInstanceTabChange={() => {}} />);
  return select;
}

describe("local library without version discovery", () => {
  it("keeps saved instances searchable and selectable when metadata fails", async () => {
    const select = await showLibrary();
    const title = await screen.findByText(saved.name);
    expect(screen.queryByText("No instances yet")).not.toBeInTheDocument();
    expect(screen.queryByText("network unavailable")).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Search instances"), { target: { value: "Offline" } });
    fireEvent.click(title);
    expect(select).toHaveBeenCalledWith(saved.id);
  });

  it("does not wait for a pending metadata request to display saved instances", async () => {
    discover = () => new Promise(() => {});
    await showLibrary();
    expect(await screen.findByText(saved.name)).toBeInTheDocument();
    expect(screen.queryByText("No instances yet")).not.toBeInTheDocument();
  });

  it("still reports local database failures independently of remote metadata", async () => {
    library = () => Promise.reject(new Error("library database unreadable"));
    await showLibrary();
    await waitFor(() => expect(screen.getByText("library database unreadable")).toBeInTheDocument());
  });
});
