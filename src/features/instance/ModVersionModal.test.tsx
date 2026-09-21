import { mockIPC } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ModDetail, ModVersionSummary } from "../browse/detailTypes";
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
      return { installed: null, message: "Installed", instance: null };
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
      if (cmd === "install_mod_to_instance")
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
          instance: null,
        };
      if (cmd === "remove_content_file") return undefined;
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

    // Untracked install goes through the normal path pinned to the picked
    // version, then drops the superseded jar.
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "install_mod_to_instance")).toHaveLength(1),
    );
    const input = calls.find((c) => c.cmd === "install_mod_to_instance")!.args.input as Record<
      string,
      unknown
    >;
    expect(input).toMatchObject({ versionId: "v-new", instanceId: "inst-1" });
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "remove_content_file")).toHaveLength(1),
    );
  });
});
