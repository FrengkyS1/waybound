import { mockIPC } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ModDetail, ModVersionSummary } from "../browse/detailTypes";
import type { ModSummary } from "../browse/types";

/**
 * Same module-scope constraint as the other install/instance specs: the
 * install store wires listeners on import, so everything loads dynamically
 * after the IPC mock exists.
 */
type ModalModule = typeof import("./ModpackVersionModal");

let ModpackVersionModal: ModalModule["ModpackVersionModal"];

interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

let calls: Call[];

function version(over: Partial<ModVersionSummary> = {}): ModVersionSummary {
  return {
    id: "v1",
    name: "Pack 8.1",
    versionNumber: "8.1",
    publishedAt: "2026-01-01T00:00:00Z",
    gameVersions: ["1.21.1"],
    loaders: [],
    downloads: 100,
    fileName: "All the Mods 10-8.1.zip",
    ...over,
  };
}

const summary: ModSummary = {
  uid: "curseforge:999",
  slug: "all-the-mods-10",
  name: "All the Mods 10",
  description: "d",
  author: "a",
  iconUrl: null,
  downloads: 1,
  projectType: "modpack",
  loaders: ["neoforge"],
  sources: ["curseforge"],
  updatedAt: "",
  curseforgeId: 999,
};

function detail(versions: ModVersionSummary[]): ModDetail {
  return {
    summary,
    body: "",
    bodyFormat: "plain",
    categories: [],
    gameVersions: ["1.21.1"],
    loaders: ["neoforge"],
    gallery: [],
    versions,
    suggestedInstance: { name: "x", minecraftVersion: "1.21.1", loader: "neoforge" },
  };
}

const noop = () => {};

const installResult = {
  installed: null,
  message: "Installed",
  instance: {
    id: "inst-1",
    name: "ATM10",
    minecraftVersion: "1.21.1",
    loader: "neoforge",
    modCount: 1,
    rootPath: "C:/x",
  },
  hasSkipped: false,
  missingMods: [],
};

beforeEach(async () => {
  vi.resetModules();
  calls = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
    if (cmd === "list_pending_missing_mods") return [];
    if (cmd === "get_modpack_detail_for_instance")
      return detail([
        version({ id: "v81", name: "Pack 8.1" }),
        version({
          id: "v80",
          name: "Pack 8.0",
          versionNumber: "8.0",
          fileName: "All the Mods 10-8.0.zip",
        }),
      ]);
    if (cmd === "install_mod_to_instance") return installResult;
    return undefined;
  });
  ({ ModpackVersionModal } = await import("./ModpackVersionModal"));
});

function renderModal(onClose: () => void = noop) {
  render(
    <ModpackVersionModal
      instanceId="inst-1"
      minecraftVersion="1.21.1"
      packLabel="All the Mods 10-8.1"
      onClose={onClose}
    />,
  );
}

describe("ModpackVersionModal", () => {
  it("marks the installed pack version and switches on pick", async () => {
    const onClose = vi.fn();
    renderModal(onClose);

    await waitFor(() => expect(screen.getByText("Pack 8.1")).toBeInTheDocument());
    // v81's archive filename matches the recorded label → current.
    expect(screen.getAllByText("Installed")).toHaveLength(2);

    fireEvent.click(screen.getByRole("button", { name: /^switch$/i }));

    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "install_mod_to_instance")).toHaveLength(1),
    );
    const input = calls.find((c) => c.cmd === "install_mod_to_instance")!.args
      .input as Record<string, unknown>;
    expect(input).toMatchObject({ versionId: "v80", instanceId: "inst-1" });
    // Switching runs in the background dock — the modal closes right away.
    expect(onClose).toHaveBeenCalled();
  });

  it("shows the backend error when the instance has no recorded pack", async () => {
    mockIPC((cmd) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "get_modpack_detail_for_instance")
        throw new Error("wasn't installed from a modpack");
      return undefined;
    });
    vi.resetModules();
    ({ ModpackVersionModal } = await import("./ModpackVersionModal"));
    renderModal();

    await waitFor(() =>
      expect(screen.getByText("wasn't installed from a modpack")).toBeInTheDocument(),
    );
  });
});
