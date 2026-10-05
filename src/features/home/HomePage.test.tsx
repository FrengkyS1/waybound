import { mockIPC } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { HomePage as HomePageComponent } from "./HomePage";
import { useState } from "react";

const saved = {
  id: "saved-world", name: "Offline survival", minecraftVersion: "1.20.1",
  loader: "fabric", modCount: 3, createdAt: 1_700_000_000,
  rootPath: "C:/isolated/instances/saved-world", totalPlaySeconds: 0, lastPlayed: null,
};
let discover: () => Promise<unknown>;
let library: () => Promise<unknown>;
let HomePage: typeof HomePageComponent;

let createdInputs: unknown[];
beforeEach(async () => {
  // HomePage reads the dismiss flag at mount; keep storage local to the test run.
  const storage: Record<string, string> = {};
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => storage[key] ?? null,
    setItem: (key: string, value: string) => { storage[key] = value; },
    removeItem: (key: string) => { delete storage[key]; },
    clear: () => { for (const key of Object.keys(storage)) delete storage[key]; },
  });
  library = () => Promise.resolve([saved]);
  createdInputs = [];
  discover = () => Promise.reject(new Error("network unavailable"));
  mockIPC((command, args) => {
    if (command === "create_instance") {
      if (args && typeof args === "object" && "input" in args) {
        createdInputs.push(args.input);
      }
      return { ...saved, id: "created-offline", name: "Offline Quilt" };
    }
    if (command === "list_instances") return library();
    if (command === "list_minecraft_versions") return discover();
    if (command === "get_curseforge_status") return { configured: false };
    if (command === "list_pending_missing_mods") return [];
    if (command === "plugin:event|listen") return 1;
    return null;
  });
  // Module-scope IPC listeners require the mock before loading these stores.
  const { useInstallStore } = await import("../install/installStore");
  const { usePlayStore } = await import("../play/store");
  useInstallStore.setState({ refreshTick: 0, instanceRefreshTicks: {} });
  usePlayStore.setState({ refreshTick: 0 });
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

  it("creates offline from a saved version without resolving a loader first", async () => {
    await showLibrary();
    await screen.findByText(saved.name);
    fireEvent.click(screen.getByRole("button", { name: /Create$/ }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Offline Quilt" } });
    fireEvent.click(screen.getByRole("button", { name: "Quilt" }));
    fireEvent.click(screen.getByRole("button", { name: "Create instance" }));
    await waitFor(() => expect(createdInputs).toEqual([{ name: "Offline Quilt", minecraftVersion: "1.20.1", loader: "quilt" }]));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("retries failed metadata in the open dialog while keeping available versions", async () => {
    await showLibrary();
    await screen.findByText(saved.name);
    fireEvent.click(screen.getByRole("button", { name: /Create$/ }));
    expect(screen.getByRole("button", { name: "1.20.1" })).toBeInTheDocument();
    discover = () => Promise.resolve([{ version: "1.21.1" }]);
    fireEvent.click(await screen.findByRole("button", { name: "Retry versions" }));
    expect(await screen.findByRole("button", { name: "1.21.1" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "1.20.1" })).toBeInTheDocument();
  });

  it("opens instance actions from Shift-F10", async () => {
    await showLibrary();
    const card = (await screen.findByText(saved.name)).closest('[role="button"]')!;
    (card as HTMLElement).focus();
    fireEvent.keyDown(card, { key: "F10", shiftKey: true });
    expect(await screen.findByRole("menuitem", { name: "Open" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "End" });
    expect(screen.getByRole("menuitem", { name: "Delete instance" })).toHaveFocus();
  });
});

describe("background library refresh races", () => {
  async function renderSelection(initialId: string | null = null) {
    if (!HomePage) ({ HomePage } = await import("./HomePage"));
    const select = vi.fn();
    function Library() {
      const [selectedId, setSelectedId] = useState(initialId);
      return <HomePage onAddMods={() => {}} onOpenMod={() => {}} onOpenSettings={() => {}}
        selectedId={selectedId} onSelectId={(id) => { select(id); setSelectedId(id); }}
        instanceTab="overview" onInstanceTabChange={() => {}} />;
    }
    render(<Library />);
    return select;
  }

  it("does not reopen an instance when an install refresh finishes after Back", async () => {
    const select = await renderSelection(saved.id);
    await screen.findByRole("button", { name: /My Instances/ });
    let finish!: (value: unknown) => void;
    library = () => new Promise((resolve) => { finish = resolve; });
    const { useInstallStore } = await import("../install/installStore");
    await act(async () => { useInstallStore.setState((s) => ({ refreshTick: s.refreshTick + 1 })); });
    fireEvent.click(screen.getByRole("button", { name: /My Instances/ }));
    await act(async () => { finish([saved]); });
    expect(screen.getByLabelText("Search instances")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /My Instances/ })).not.toBeInTheDocument();
    expect(select).toHaveBeenCalledExactlyOnceWith(null);
  });

  it("discards an older list response after a newer refresh", async () => {
    await renderSelection();
    await screen.findByText(saved.name);
    const requests: ((value: unknown) => void)[] = [];
    library = () => new Promise((resolve) => { requests.push(resolve); });
    const { useInstallStore } = await import("../install/installStore");
    await act(async () => { useInstallStore.setState((s) => ({ refreshTick: s.refreshTick + 1 })); });
    await act(async () => { useInstallStore.setState((s) => ({ refreshTick: s.refreshTick + 1 })); });
    expect(requests).toHaveLength(2);
    await act(async () => { requests[1]([{ ...saved, name: "Latest list" }]); });
    await screen.findByText("Latest list");
    await act(async () => { requests[0]([]); });
    expect(screen.getByText("Latest list")).toBeInTheDocument();
    expect(screen.queryByText("No instances yet")).not.toBeInTheDocument();
  });
});
