import { mockIPC } from "@tauri-apps/api/mocks";
import { describe, expect, it, vi } from "vitest";

import type { MissingMod } from "../browse/detailTypes";
import type { InstallEntry } from "./installStore";

interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

/**
 * `installStore.ts` calls `listen(...)` and `list_pending_missing_mods` at
 * module scope, so it has to be imported *after* an IPC mock exists — a
 * static import at the top of this file runs during collection, before any
 * `beforeEach`, and throws. Every test therefore loads its own fresh copy of
 * the module, which also keeps the module-scope singleton from leaking state
 * between tests.
 */
async function loadStore(pending: unknown[] = []) {
  const calls: Call[] = [];
  vi.resetModules();
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
    if (cmd === "list_pending_missing_mods") return pending;
    return undefined;
  });
  const mod = await import("./installStore");
  // Let the module-scope `fetchPendingMissingMods()` promise settle.
  await vi.waitFor(() =>
    expect(mod.useInstallStore.getState().installs).toHaveLength(pending.length),
  );
  // Ignore the module-scope wiring (event listeners + the pending-mods
  // restore) so assertions only see what the action under test triggered.
  const backendCalls = () =>
    calls.filter((c) => !c.cmd.startsWith("plugin:") && c.cmd !== "list_pending_missing_mods");
  return { store: mod.useInstallStore, backendCalls };
}

function missing(projectId: number, name: string): MissingMod {
  return {
    projectId,
    name,
    filename: `${name}.jar`,
    url: `https://www.curseforge.com/minecraft/mc-mods/${name}`,
  };
}

function entry(over: Partial<InstallEntry> = {}): InstallEntry {
  return { id: "e1", name: "Some Pack", status: "done", ...over };
}

describe("dismiss", () => {
  it("persists a dismissal for every still-missing mod before dropping the entry", async () => {
    const { store, backendCalls } = await loadStore();
    store.setState({
      installs: [
        entry({
          id: "e1",
          instanceId: "inst-1",
          missingMods: [missing(1, "alpha"), missing(2, "beta")],
        }),
      ],
    });

    store.getState().dismiss("e1");
    await vi.waitFor(() => expect(backendCalls()).toHaveLength(2));

    // Without this, the card is only gone until the next restart —
    // list_pending_missing_mods recomputes it straight from the on-disk
    // manifest and re-adds an identical entry.
    expect(backendCalls()).toEqual([
      { cmd: "dismiss_missing_mod", args: { instanceId: "inst-1", projectId: 1 } },
      { cmd: "dismiss_missing_mod", args: { instanceId: "inst-1", projectId: 2 } },
    ]);
    expect(store.getState().installs).toEqual([]);
  });

  it("skips mods the watcher already placed on disk", async () => {
    const { store, backendCalls } = await loadStore();
    store.setState({
      installs: [
        entry({
          id: "e1",
          instanceId: "inst-1",
          missingMods: [missing(1, "alpha"), missing(2, "beta"), missing(3, "gamma")],
          missingModsPlaced: ["beta"],
        }),
      ],
    });

    store.getState().dismiss("e1");
    await vi.waitFor(() => expect(backendCalls()).toHaveLength(2));

    // "beta" is already in the instance — dismissing it would permanently
    // untrack a mod the user actually has.
    expect(backendCalls().map((c) => c.args.projectId)).toEqual([1, 3]);
    expect(store.getState().installs).toEqual([]);
  });

  it("touches the backend not at all for a plain install toast", async () => {
    const { store, backendCalls } = await loadStore();
    store.setState({
      installs: [entry({ id: "e1", instanceId: "inst-1", message: "Installed Sodium" })],
    });

    store.getState().dismiss("e1");
    await Promise.resolve();

    expect(backendCalls()).toEqual([]);
    expect(store.getState().installs).toEqual([]);
  });

  it("stays local for an entry with an empty missingMods array", async () => {
    const { store, backendCalls } = await loadStore();
    store.setState({ installs: [entry({ id: "e1", instanceId: "inst-1", missingMods: [] })] });

    store.getState().dismiss("e1");
    await Promise.resolve();

    expect(backendCalls()).toEqual([]);
    expect(store.getState().installs).toEqual([]);
  });

  it("stays local when there is no instance to dismiss against", async () => {
    const { store, backendCalls } = await loadStore();
    store.setState({ installs: [entry({ id: "e1", missingMods: [missing(1, "alpha")] })] });

    store.getState().dismiss("e1");
    await Promise.resolve();

    expect(backendCalls()).toEqual([]);
    expect(store.getState().installs).toEqual([]);
  });

  it("removes only the entry it was given", async () => {
    const { store } = await loadStore();
    store.setState({
      installs: [entry({ id: "e1" }), entry({ id: "e2" }), entry({ id: "e3" })],
    });

    store.getState().dismiss("e2");

    expect(store.getState().installs.map((e) => e.id)).toEqual(["e1", "e3"]);
  });

  it("is a no-op for an id that is no longer in the list", async () => {
    const { store, backendCalls } = await loadStore();
    store.setState({ installs: [entry({ id: "e1" })] });

    store.getState().dismiss("gone");
    await Promise.resolve();

    expect(backendCalls()).toEqual([]);
    expect(store.getState().installs.map((e) => e.id)).toEqual(["e1"]);
  });

  it("still clears the entry when the backend dismissal rejects", async () => {
    const { store } = await loadStore();
    mockIPC((cmd) => {
      if (cmd === "dismiss_missing_mod") return Promise.reject(new Error("disk full"));
      return undefined;
    });
    store.setState({
      installs: [entry({ id: "e1", instanceId: "inst-1", missingMods: [missing(1, "alpha")] })],
    });

    store.getState().dismiss("e1");
    await Promise.resolve();
    await Promise.resolve();

    expect(store.getState().installs).toEqual([]);
  });
});

// `describeMissingMods` is module-private. Its output is reachable through
// the entry text the store builds for anything still outstanding from a
// previous session, which is the only place it is used.
describe("pending missing-mod entries (describeMissingMods)", () => {
  const pending = (instanceId: string, names: string[]) => ({
    instanceId,
    instanceName: `Pack ${instanceId}`,
    missingMods: names.map((n, i) => missing(i + 1, n)),
  });

  it("names a single mod on its own", async () => {
    const { store } = await loadStore([pending("i1", ["alpha"])]);
    expect(store.getState().installs[0].message).toBe(
      "1 mod(s) from a previous import still need a manual download: alpha",
    );
  });

  it("comma-separates two", async () => {
    const { store } = await loadStore([pending("i1", ["alpha", "beta"])]);
    expect(store.getState().installs[0].message).toContain(": alpha, beta");
  });

  it("names exactly three without an overflow tail", async () => {
    const { store } = await loadStore([pending("i1", ["alpha", "beta", "gamma"])]);
    const message = store.getState().installs[0].message ?? "";
    expect(message).toContain(": alpha, beta, gamma");
    expect(message).not.toContain("more");
  });

  it("caps the list at three and counts the rest", async () => {
    const { store } = await loadStore([pending("i1", ["alpha", "beta", "gamma", "delta"])]);
    expect(store.getState().installs[0].message).toContain(
      ": alpha, beta, gamma, +1 more",
    );
  });

  it("keeps a long tail readable", async () => {
    const names = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta"];
    const { store } = await loadStore([pending("i1", names)]);
    const message = store.getState().installs[0].message ?? "";

    expect(message).toBe(
      "7 mod(s) from a previous import still need a manual download: alpha, beta, gamma, +4 more",
    );
    expect(message).not.toContain("delta");
  });

  it("rebuilds one dismissible entry per instance", async () => {
    const { store } = await loadStore([
      pending("i1", ["alpha"]),
      pending("i2", ["beta", "gamma", "delta", "epsilon"]),
    ]);

    const installs = store.getState().installs;
    expect(installs.map((e) => e.id)).toEqual([
      "pending-missing-mods-i1",
      "pending-missing-mods-i2",
    ]);
    expect(installs[1]).toMatchObject({
      name: "Pack i2",
      status: "done",
      instanceId: "i2",
    });
    expect(installs[1].missingMods).toHaveLength(4);
  });

  it("re-dismisses a restored entry through the backend", async () => {
    // The whole point of restoring these: closing one has to persist, or it
    // comes straight back on the next launch.
    const { store, backendCalls } = await loadStore([pending("i1", ["alpha", "beta"])]);
    const before = backendCalls().length;

    store.getState().dismiss("pending-missing-mods-i1");
    await vi.waitFor(() => expect(backendCalls().length).toBe(before + 2));

    expect(backendCalls().slice(before)).toEqual([
      { cmd: "dismiss_missing_mod", args: { instanceId: "i1", projectId: 1 } },
      { cmd: "dismiss_missing_mod", args: { instanceId: "i1", projectId: 2 } },
    ]);
    expect(store.getState().installs).toEqual([]);
  });

  it("adds nothing when nothing is outstanding", async () => {
    const { store } = await loadStore([]);
    expect(store.getState().installs).toEqual([]);
  });
});

describe("dockMinimized", () => {
  it("starts expanded and toggles without touching the backend", async () => {
    const { store, backendCalls } = await loadStore([]);
    expect(store.getState().dockMinimized).toBe(false);

    store.getState().setDockMinimized(true);
    expect(store.getState().dockMinimized).toBe(true);

    store.getState().setDockMinimized(false);
    expect(store.getState().dockMinimized).toBe(false);

    expect(backendCalls()).toEqual([]);
  });
});
