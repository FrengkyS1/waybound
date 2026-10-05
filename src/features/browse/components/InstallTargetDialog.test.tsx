import { mockIPC } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { ModDetail, ModVersionSummary } from "../detailTypes";
import type { InstanceSummary, ModLoader } from "../../instances/types";
import type { ModSummary } from "../types";
import type { VersionPrefill } from "../../settings/types";

/**
 * `compatibleInstances` is module-private, so it is exercised through what
 * the dialog actually renders: the "Instance" dropdown is populated from it.
 * The component pulls in `installStore`, which wires event listeners at
 * module scope, so it has to be imported after the IPC mock exists — hence
 * the dynamic import inside each test.
 */
async function renderDialog(instances: InstanceSummary[], detail: ModDetail, versionPrefill?: VersionPrefill, fixedInstanceId?: string) {
  mockIPC((cmd) => {
    if (cmd === "list_instances") return instances;
    if (cmd === "list_pending_missing_mods") return [];
    return undefined;
  });
  const { InstallTargetDialog } = await import("./InstallTargetDialog");
  render(
    <InstallTargetDialog
      detail={detail}
      versionPrefill={versionPrefill}
      fixedInstanceId={fixedInstanceId}
      onClose={() => {}}
      onSuccess={() => {}}
    />,
  );
}

function instance(id: string, loader: ModLoader, minecraftVersion: string): InstanceSummary {
  return {
    id,
    name: `${id} (${loader} ${minecraftVersion})`,
    minecraftVersion,
    loader,
    modCount: 0,
    createdAt: 1_700_000_000,
    rootPath: `C:/instances/${id}`,
    totalPlaySeconds: 0,
  };
}

function version(loaders: ModLoader[], gameVersions: string[]): ModVersionSummary {
  return {
    id: `${loaders.join("-")}-${gameVersions.join("-")}`,
    name: "v1",
    versionNumber: "1.0.0",
    publishedAt: "2024-01-01",
    gameVersions,
    loaders,
    downloads: 0,
  };
}

function detailWith(versions: ModVersionSummary[]): ModDetail {
  const summary: ModSummary = {
    uid: "modrinth:sodium",
    slug: "sodium",
    name: "Sodium",
    description: "",
    author: "",
    iconUrl: null,
    downloads: 0,
    projectType: "mod",
    loaders: ["fabric"],
    sources: ["modrinth"],
    updatedAt: "",
  };
  return {
    summary,
    body: "",
    bodyFormat: "plain",
    categories: [],
    gameVersions: ["1.20.1", "1.21"],
    loaders: ["fabric", "forge"],
    gallery: [],
    versions,
    suggestedInstance: { name: "Sodium", minecraftVersion: "1.20.1", loader: "fabric" },
  };
}

async function optionLabels() {
  const select = await screen.findByRole("combobox");
  return Array.from(select.querySelectorAll("option")).map((o) => o.getAttribute("value"));
}

describe("InstallTargetDialog instance compatibility filter", () => {
  it("keeps only instances matching a single published version's loader AND game version", async () => {
    // The bug this guards: checking the mod's *union* of loaders against the
    // union of its game versions. A mod published for fabric+1.20.1 and
    // forge+1.21 would then also look compatible with forge+1.20.1 and
    // fabric+1.21, and installing into one dropped a dead jar in.
    await renderDialog(
      [
        instance("fabric-1201", "fabric", "1.20.1"),
        instance("forge-1201", "forge", "1.20.1"),
        instance("fabric-121", "fabric", "1.21"),
        instance("forge-121", "forge", "1.21"),
      ],
      detailWith([version(["fabric"], ["1.20.1"]), version(["forge"], ["1.21"])]),
    );

    expect(await optionLabels()).toEqual(["fabric-1201", "forge-121"]);
  });

  it("matches an instance against any one of a version's loaders or game versions", async () => {
    await renderDialog(
      [
        instance("quilt-1194", "quilt", "1.19.4"),
        instance("vanilla-1201", "vanilla", "1.20.1"),
        instance("neoforge-1201", "neoforge", "1.20.1"),
      ],
      detailWith([version(["fabric", "quilt"], ["1.19.4", "1.20.1"])]),
    );

    expect(await optionLabels()).toEqual(["quilt-1194"]);
  });

  it("keeps an instance that only a later published version supports", async () => {
    await renderDialog(
      [instance("neoforge-121", "neoforge", "1.21")],
      detailWith([
        version(["fabric"], ["1.20.1"]),
        version(["fabric"], ["1.20.6"]),
        version(["neoforge"], ["1.21"]),
      ]),
    );

    expect(await optionLabels()).toEqual(["neoforge-121"]);
  });

  it("falls back to listing every instance, with a warning, when none match", async () => {
    await renderDialog(
      [instance("forge-1122", "forge", "1.12.2"), instance("vanilla-1165", "vanilla", "1.16.5")],
      detailWith([version(["fabric"], ["1.21"])]),
    );

    expect(await optionLabels()).toEqual(["forge-1122", "vanilla-1165"]);
    expect(
      screen.getByText(/No instance matches this mod's loader\/version/),
    ).toBeInTheDocument();
  });

  it("does not show the fallback warning when a compatible instance exists", async () => {
    await renderDialog(
      [instance("fabric-1201", "fabric", "1.20.1"), instance("forge-1122", "forge", "1.12.2")],
      detailWith([version(["fabric"], ["1.20.1"])]),
    );

    expect(await optionLabels()).toEqual(["fabric-1201"]);
    expect(screen.queryByText(/No instance matches/)).not.toBeInTheDocument();
  });

  it("treats a mod with no published versions as compatible with nothing", async () => {
    await renderDialog([instance("fabric-1201", "fabric", "1.20.1")], detailWith([]));

    expect(await optionLabels()).toEqual(["fabric-1201"]);
    expect(screen.getByText(/No instance matches/)).toBeInTheDocument();
  });

  it("defaults the selection to a compatible instance, not merely the first one", async () => {
    await renderDialog(
      [
        instance("forge-1122", "forge", "1.12.2"),
        instance("fabric-1201", "fabric", "1.20.1"),
      ],
      detailWith([version(["fabric"], ["1.20.1"])]),
    );

    const select = (await screen.findByRole("combobox")) as HTMLSelectElement;
    expect(select.value).toBe("fabric-1201");
  });

  it("switches to create-new mode when there are no instances at all", async () => {
    await renderDialog([], detailWith([version(["fabric"], ["1.20.1"])]));

    expect(await screen.findByLabelText(/Instance name/)).toBeInTheDocument();
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
  });

  it("uses the exact selected version instead of another compatible release", async () => {
    const pinned = version(["fabric"], ["1.20.1"]);
    await renderDialog(
      [instance("forge", "forge", "1.21"), instance("fabric", "fabric", "1.20.1")],
      detailWith([version(["forge"], ["1.21"]), pinned]),
      { versionId: pinned.id, minecraftVersion: "1.20.1", loader: "fabric" },
    );
    await waitFor(async () => expect(await optionLabels()).toEqual(["fabric"]));
    expect(screen.getByRole("button", { name: "Install" })).toBeEnabled();
  });

  it("blocks a locked instance when only another project version supports it", async () => {
    const pinned = version(["fabric"], ["1.20.1"]);
    await renderDialog(
      [instance("forge", "forge", "1.21")],
      detailWith([version(["forge"], ["1.21"]), pinned]),
      { versionId: pinned.id, minecraftVersion: "1.20.1", loader: "fabric" },
      "forge",
    );
    expect(await screen.findByText(/incompatible with the selected version/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Install" })).toBeDisabled();
  });

  it("disables all unmatched existing targets and incompatible new target selections", async () => {
    await renderDialog(
      [instance("forge", "forge", "1.20.1")],
      detailWith([version(["fabric"], ["1.20.1"])]),
    );
    const option = await screen.findByRole("option");
    expect(option).toBeDisabled();
    expect(screen.getByRole("button", { name: "Install" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Create new instance" }));
    expect(screen.getByRole("button", { name: "Install" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "forge" }));
    expect(screen.getByRole("button", { name: "Install" })).toBeDisabled();
  });

  it("does not treat missing Minecraft metadata as compatibility", async () => {
    await renderDialog([instance("fabric", "fabric", "1.20.1")], detailWith([version(["fabric"], [])]));
    expect(await screen.findByRole("option")).toBeDisabled();
    expect(screen.getByRole("button", { name: "Install" })).toBeDisabled();
  });
});
