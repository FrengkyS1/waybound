import { mockIPC } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { InstallModResult, ModDetail, ModVersionSummary } from "../browse/detailTypes";
import type { ModSummary } from "../browse/types";

/**
 * `ModVersionModal` pulls in the browse + instances API layers (both call
 * `invoke` only when used, but the install store's module-scope listener
 * wiring comes along transitively), so it loads dynamically after the IPC
 * mock exists.
 */
type ModalModule = typeof import("./ModVersionModal");

let ModVersionModal: ModalModule["ModVersionModal"];

interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

let calls: Call[];

function version(over: Partial<ModVersionSummary> = {}): ModVersionSummary {
  return {
    id: "v1",
    name: "Mod 2.0",
    versionNumber: "2.0",
    publishedAt: "2026-01-01T00:00:00Z",
    gameVersions: ["1.21.1"],
    loaders: ["neoforge"],
    downloads: 100,
    ...over,
  };
}

const summary: ModSummary = {
  uid: "curseforge:123",
  slug: "some-mod",
  name: "Some Mod",
  description: "d",
  author: "a",
  iconUrl: null,
  downloads: 1,
  projectType: "mod",
  loaders: ["neoforge"],
  sources: ["curseforge"],
  updatedAt: "",
  curseforgeId: 123,
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

function installResult(over: Partial<InstallModResult> = {}): InstallModResult {
  return {
    installed: null, message: "Installed", hasSkipped: false, missingMods: [],
    instance: { id: "inst-1", name: "Test", minecraftVersion: "1.21.1", loader: "neoforge", modCount: 1, rootPath: "C:/isolated/inst-1" },
    ...over,
  };
}

function renderModal() {
  render(
    <ModVersionModal
      instanceId="inst-1"
      minecraftVersion="1.21.1"
      loader="neoforge"
      fileName="somemod-1.0.jar"
      modLabel="Some Mod"
      onClose={noop}
      onInstalled={noop}
    />,
  );
}

beforeEach(async () => {
  vi.resetModules();
  calls = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
    if (cmd === "list_pending_missing_mods") return [];
    if (cmd === "get_mod_summary_for_content") return summary;
    if (cmd === "get_mod_details")
      return detail([
        version({ id: "v-new", name: "Mod 2.0", fileName: "somemod-2.0.jar" }),
        version({
          id: "v-old",
          name: "Mod 1.0",
          versionNumber: "1.0",
          fileName: "somemod-1.0.jar",
        }),
        version({
          id: "v-other",
          name: "Mod 3.0-fabric",
          versionNumber: "3.0",
          fileName: "somemod-3.0.jar",
          gameVersions: ["1.21.1"],
          loaders: ["fabric"],
        }),
      ]);
    if (cmd === "update_mod_in_instance")
      return installResult();
    return undefined;
  });
  ({ ModVersionModal } = await import("./ModVersionModal"));
});

describe("ModVersionModal", () => {
  it("highlights the installed version and disables incompatible rows", async () => {
    renderModal();

    await waitFor(() => expect(screen.getByText("Mod 2.0")).toBeInTheDocument());

    // Current file exact-matches v-old → badge + disabled "Installed" button.
    expect(screen.getAllByText("Installed")).toHaveLength(2);
    // Fabric-only row on a NeoForge instance → flagged, not installable.
    expect(screen.getByText("Incompatible")).toBeInTheDocument();
    const buttons = screen.getAllByRole("button", { name: /install/i });
    expect(buttons).toHaveLength(3);
    expect(buttons[0]).not.toBeDisabled();
    expect(buttons[1]).toBeDisabled();
    expect(buttons[2]).toBeDisabled();
  });

  it("installs the picked version pinned to its id", async () => {
    renderModal();
    await waitFor(() => expect(screen.getByText("Mod 2.0")).toBeInTheDocument());

    fireEvent.click(screen.getAllByRole("button", { name: /^install$/i })[0]);

    await waitFor(() =>
      expect(
        calls.filter((c) => c.cmd === "update_mod_in_instance"),
      ).toHaveLength(1),
    );
    expect(calls.find((c) => c.cmd === "update_mod_in_instance")!.args).toMatchObject({
      instanceId: "inst-1",
      fileName: "somemod-1.0.jar",
      versionId: "v-new",
    });
  });

  it("shows the backend error for an untracked file", async () => {
    mockIPC((cmd) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "get_mod_summary_for_content")
        throw new Error("not tracked in this instance.");
      if (cmd === "identify_mod_file")
        throw new Error("Couldn't identify this file on Modrinth or CurseForge.");
      return undefined;
    });
    vi.resetModules();
    ({ ModVersionModal } = await import("./ModVersionModal"));
    renderModal();

    await waitFor(() =>
      expect(
        screen.getByText("Couldn't identify this file on Modrinth or CurseForge."),
      ).toBeInTheDocument(),
    );
  });

  it("falls back to hash identification for untracked files", async () => {
    mockIPC((cmd, args) => {
      calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "get_mod_summary_for_content")
        throw new Error("not tracked in this instance.");
      if (cmd === "identify_mod_file")
        return {
          fileName: "somemod-1.0.jar",
          summary,
          versionId: "v-old",
          versionNumber: "1.0",
          matchedFileName: "somemod-1.0.jar",
        };
      if (cmd === "get_mod_details")
        return detail([
          version({ id: "v-new", name: "Mod 2.0", fileName: "somemod-2.0.jar" }),
          version({
            id: "v-old",
            name: "Mod 1.0",
            versionNumber: "1.0",
            fileName: "somemod-1.0.jar",
          }),
        ]);
      if (cmd === "update_mod_in_instance")
        return {
          installed: {
            id: 1,
            instanceId: "inst-1",
            modUid: "curseforge:123",
            modName: "Some Mod",
            source: "curseforge",
            fileName: "somemod-2.0.jar",
            installedAt: 1,
          },
          message: "Installed",
          instance: installResult().instance,
          hasSkipped: false,
          missingMods: [],
        };
      return undefined;
    });
    vi.resetModules();
    ({ ModVersionModal } = await import("./ModVersionModal"));
    renderModal();

    // The hash-matched version highlights even though the file was never
    // tracked, and the modal names the identified project.
    await waitFor(() => expect(screen.getByText("Mod 2.0")).toBeInTheDocument());
    expect(screen.getAllByText("Installed")).toHaveLength(2);

    fireEvent.click(screen.getAllByRole("button", { name: /^install$/i })[0]);

    // Identification records the actual file; updates use one atomic path.
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "update_mod_in_instance")).toHaveLength(1),
    );
    expect(calls.find((c) => c.cmd === "update_mod_in_instance")!.args).toMatchObject({
      versionId: "v-new", instanceId: "inst-1", fileName: "somemod-1.0.jar",
    });
    expect(calls.filter((c) => c.cmd === "install_mod_to_instance" || c.cmd === "remove_content_file")).toEqual([]);
  });

  it("retains manual-download update results in the install dock", async () => {
    const manual = { projectId: 123, name: "Some Mod", filename: "somemod-2.0.jar", url: "https://www.curseforge.com/minecraft/mc-mods/some-mod/download/123" };
    mockIPC((cmd) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "get_mod_summary_for_content") return summary;
      if (cmd === "get_mod_details") return detail([version({ id: "picked" })]);
      if (cmd === "update_mod_in_instance") return installResult({ message: "Download Some Mod manually", hasSkipped: true, missingMods: [manual] });
    });
    const { useInstallStore } = await import("../install/installStore");
    renderModal();
    fireEvent.click(await screen.findByRole("button", { name: "Install" }));
    await waitFor(() => expect(useInstallStore.getState().installs[0]).toMatchObject({
      status: "done", missingMods: [manual], message: "Download Some Mod manually",
    }));
  });

  it("does not remove an identified old jar until its replacement actually lands", async () => {
    const removed = vi.fn();
    mockIPC((cmd) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "get_mod_summary_for_content") throw new Error("not tracked");
      if (cmd === "identify_mod_file") return { summary, fileName: "somemod-1.0.jar", versionId: "old", versionNumber: "1.0", matchedFileName: "somemod-1.0.jar" };
      if (cmd === "get_mod_details") return detail([version({ id: "picked" })]);
      if (cmd === "update_mod_in_instance") return installResult({ hasSkipped: true, missingMods: [{ projectId: 123, name: "Some Mod", filename: "somemod-2.0.jar", url: "https://www.curseforge.com/minecraft/mc-mods/some-mod/download/123" }] });
      if (cmd === "remove_content_file") return removed();
    });
    const { useInstallStore } = await import("../install/installStore");
    renderModal();
    fireEvent.click(await screen.findByRole("button", { name: "Install" }));
    await waitFor(() => expect(useInstallStore.getState().installs[0].status).toBe("done"));
    expect(removed).not.toHaveBeenCalled();
  });

  it("keeps atomic replacement failures retryable for identified installs", async () => {
    mockIPC((cmd) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "get_mod_summary_for_content") throw new Error("not tracked");
      if (cmd === "identify_mod_file") return { summary, fileName: "somemod-1.0.jar", versionId: "old", versionNumber: "1.0", matchedFileName: "somemod-1.0.jar" };
      if (cmd === "get_mod_details") return detail([version({ id: "picked" })]);
      if (cmd === "update_mod_in_instance") throw new Error("old jar locked");
    });
    const { useInstallStore } = await import("../install/installStore");
    renderModal();
    fireEvent.click(await screen.findByRole("button", { name: "Install" }));
    await waitFor(() => expect(useInstallStore.getState().installs[0].status).toBe("error"));
    expect(useInstallStore.getState().installs[0].error).toContain("old jar locked");
    expect(screen.getByRole("button", { name: "Install" })).toBeEnabled();
  });

  it("finishes through the store without calling a closed modal's callbacks", async () => {
    let finish!: (result: InstallModResult) => void;
    mockIPC((cmd) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "get_mod_summary_for_content") return summary;
      if (cmd === "get_mod_details") return detail([version({ id: "picked" })]);
      if (cmd === "update_mod_in_instance") return new Promise<InstallModResult>((resolve) => { finish = resolve; });
    });
    const installed = vi.fn();
    const close = vi.fn();
    const { useInstallStore } = await import("../install/installStore");
    const page = render(<ModVersionModal instanceId="inst-1" minecraftVersion="1.21.1" loader="neoforge" fileName="somemod-1.0.jar" modLabel="Some Mod" onClose={close} onInstalled={installed} />);
    fireEvent.click(await screen.findByRole("button", { name: "Install" }));
    await waitFor(() => expect(useInstallStore.getState().installs[0].status).toBe("installing"));
    page.unmount();
    await act(async () => { finish(installResult()); });
    expect(useInstallStore.getState().installs[0].status).toBe("done");
    expect(installed).not.toHaveBeenCalled();
    expect(close).not.toHaveBeenCalled();
  });

  it("marks a disabled physical file installed and updates that exact file", async () => {
    const updates: unknown[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "get_mod_summary_for_content") return summary;
      if (cmd === "get_mod_details") return detail([
        version({ id: "old", name: "Current", fileName: "somemod-1.0.jar" }),
        version({ id: "next", name: "Next", fileName: "somemod-2.0.jar" }),
      ]);
      if (cmd === "update_mod_in_instance") { updates.push(args); return installResult(); }
      return null;
    });
    const completed = vi.fn();
    render(<ModVersionModal instanceId="inst-1" minecraftVersion="1.21.1" loader="neoforge"
      fileName="somemod-1.0.jar.disabled" modLabel="Some Mod" onClose={noop} onInstalled={completed} />);
    const current = (await screen.findByText("Current")).closest("li")!;
    expect(within(current).getByRole("button", { name: "Installed" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Install" }));
    await waitFor(() => expect(completed).toHaveBeenCalled());
    expect(updates[0]).toMatchObject({ fileName: "somemod-1.0.jar.disabled", versionId: "next" });
  });
});
