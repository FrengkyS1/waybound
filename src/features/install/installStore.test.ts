import { mockIPC } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import { describe, expect, it, vi } from "vitest";

import type { InstallModResult, MissingMod } from "../browse/detailTypes";
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
  }, { shouldMockEvents: true });
  const mod = await import("./installStore");
  // Let the module-scope `fetchPendingMissingMods()` promise settle.
  await vi.waitFor(() => {
    expect(mod.useInstallStore.getState().pendingMissingModsLoading).toBe(false);
    expect(mod.useInstallStore.getState().installs).toHaveLength(pending.length);
  });
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
    await vi.waitFor(() => expect(store.getState().installs).toEqual([]));

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
    await vi.waitFor(() => expect(store.getState().installs).toEqual([]));

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

  it("keeps failed dismissals actionable instead of losing durable work", async () => {
    const { store } = await loadStore();
    mockIPC((cmd) => {
      if (cmd === "dismiss_missing_mod") return Promise.reject(new Error("disk full"));
      return undefined;
    });
    store.setState({
      installs: [entry({ id: "e1", instanceId: "inst-1", missingMods: [missing(1, "alpha")] })],
    });

    store.getState().dismiss("e1");
    await vi.waitFor(() => expect(store.getState().installs[0].missingModsDismissPending).toBe(false));
    expect(store.getState().installs[0].missingMods).toEqual([missing(1, "alpha")]);
    expect(store.getState().installs[0].missingModsDismissError).toContain("disk full");
  });

  it("retains only failed items after a partial batch dismissal", async () => {
    const { store } = await loadStore();
    mockIPC((cmd, args) => {
      if (cmd === "dismiss_missing_mod" && args && typeof args === "object" && "projectId" in args && args.projectId === 2) {
        throw new Error("beta manifest write failed");
      }
    });
    store.setState({ installs: [entry({ instanceId: "inst-1", missingMods: [missing(1, "alpha"), missing(2, "beta")] })] });
    store.getState().dismiss("e1");
    await vi.waitFor(() => expect(store.getState().installs[0].missingModsDismissPending).toBe(false));
    expect(store.getState().installs[0].missingMods).toEqual([missing(2, "beta")]);
    expect(store.getState().installs[0].missingModsDismissError).toContain("beta manifest write failed");
  });
});

describe("pending missing-mod restoration", () => {
  const pending = (instanceId: string, names: string[]) => ({
    instanceId,
    instanceName: `Pack ${instanceId}`,
    missingMods: names.map((n, i) => missing(i + 1, n)),
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
    await vi.waitFor(() => expect(store.getState().installs).toEqual([]));

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

describe("manual install result tracking", () => {
  const result: InstallModResult = {
    installed: null,
    message: "Manual download needed",
    instance: { id: "inst-1", name: "Pack", minecraftVersion: "1.21.1", loader: "neoforge", modCount: 0, rootPath: "C:/isolated/inst-1" },
    hasSkipped: true,
    missingMods: [missing(1, "alpha")],
  };

  it("retains complete update outcomes and refreshes only their instance", async () => {
    const { store } = await loadStore();
    const operation = vi.fn(async () => result);
    expect(await store.getState().runInstall("Alpha update", operation, "inst-1")).toEqual(result);
    expect(operation).toHaveBeenCalledWith(expect.any(String));
    expect(store.getState().installs[0]).toMatchObject({
      status: "done", instanceId: "inst-1", message: result.message, missingMods: result.missingMods,
    });
    expect(store.getState().instanceRefreshTicks).toEqual({ "inst-1": 1 });
    expect(store.getState().refreshTick).toBe(1);
  });

  it("restores durable missing downloads after failed installs without duplicating existing cards", async () => {
    const { store } = await loadStore();
    mockIPC((cmd) => cmd === "list_pending_missing_mods" ? [{
      instanceId: "inst-1", instanceName: "Pack", missingMods: result.missingMods,
    }] : undefined);
    expect(await store.getState().runInstall("Failed update", async () => { throw new Error("disk full"); }, "inst-1")).toBeNull();
    await vi.waitFor(() => expect(store.getState().pendingMissingModsLoading).toBe(false));
    expect(store.getState().installs[0]).toMatchObject({ status: "error", error: "disk full" });
    expect(store.getState().installs[1]).toMatchObject({ status: "done", missingMods: result.missingMods });
    store.getState().reloadPendingMissingMods();
    await vi.waitFor(() => expect(store.getState().pendingMissingModsLoading).toBe(false));
    expect(store.getState().installs).toHaveLength(2);
  });

  it("queues recovery when an install fails during an older pending-list request", async () => {
    const { store } = await loadStore();
    let finish!: (value: unknown[]) => void;
    let requests = 0;
    mockIPC((cmd) => {
      if (cmd !== "list_pending_missing_mods") return undefined;
      requests++;
      if (requests === 1) return new Promise<unknown[]>((resolve) => { finish = resolve; });
      return [{ instanceId: "inst-1", instanceName: "Pack", missingMods: result.missingMods }];
    });
    store.getState().reloadPendingMissingMods();
    await store.getState().runInstall("Failed update", async () => { throw new Error("disk full"); }, "inst-1");
    finish([]);
    await vi.waitFor(() => expect(store.getState().installs).toHaveLength(2));
    expect(store.getState().installs[0]).toMatchObject({ status: "error", error: "disk full" });
    expect(store.getState().installs[1]).toMatchObject({ status: "done", missingMods: result.missingMods });
    expect(store.getState().pendingMissingModsError).toBeUndefined();
  });

  it("includes newly completed manual outcomes in an active instance watch", async () => {
    const { store } = await loadStore();
    store.setState({ installs: [entry({
      instanceId: "inst-1", missingMods: [missing(2, "beta")], missingModsWatching: true,
    })] });
    await store.getState().runInstall("Alpha update", async () => result, "inst-1");
    expect(store.getState().installs[1]).toMatchObject({
      missingMods: result.missingMods, missingModsWatching: true,
    });
    await emit("missing-mods://placed", { instanceId: "inst-1", name: "alpha", remaining: 1, total: 2 });
    expect(store.getState().installs[1].missingModsPlaced).toEqual(["alpha"]);
    expect(store.getState().installs[0].missingModsPlaced).toBeUndefined();
  });
});

describe("missing download recovery", () => {
  it("opens the next unresolved URL after dismissing the current step", async () => {
    const { store, backendCalls } = await loadStore();
    store.setState({ installs: [entry({
      instanceId: "inst-1", missingMods: [missing(1, "alpha"), missing(2, "beta"), missing(3, "gamma")],
      missingModsPlaced: ["beta"], missingModsIndex: 0,
    })] });
    store.getState().dismissMissingMod("e1", 1);
    await vi.waitFor(() => expect(backendCalls().some((c) => c.cmd === "open_missing_mods_browser")).toBe(true));
    expect(backendCalls().find((c) => c.cmd === "open_missing_mods_browser")?.args).toEqual({ url: missing(3, "gamma").url });
    expect(store.getState().installs[0].missingModsIndex).toBe(1);
  });

  it("does not reopen an already placed mod after dismissing the last unresolved step", async () => {
    const { store, backendCalls } = await loadStore();
    store.setState({ installs: [entry({
      instanceId: "inst-1", missingMods: [missing(1, "alpha"), missing(2, "beta")],
      missingModsPlaced: ["beta"], missingModsIndex: 0, missingModsWatching: true,
    })] });
    store.getState().dismissMissingMod("e1", 1);
    await vi.waitFor(() => expect(store.getState().installs[0].missingModsDismissPending).toBe(false));
    expect(store.getState().installs[0].missingModsIndex).toBeUndefined();
    expect(store.getState().installs[0].missingModsWatching).toBe(false);
    expect(backendCalls().some((c) => c.cmd === "open_missing_mods_browser")).toBe(false);
  });

  it("keeps browser and watcher failures independent and retryable, including Open all", async () => {
    const { store } = await loadStore();
    let fail = true;
    const calls: Call[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      if (fail && cmd === "open_all_missing_mods_browsers") throw new Error("browser unavailable");
      if (fail && cmd === "watch_for_missing_mods") throw new Error("Downloads unreadable");
    });
    store.setState({ installs: [entry({ instanceId: "inst-1", missingMods: [missing(1, "alpha"), missing(2, "beta")] })] });
    store.getState().openAllMissingMods("e1");
    await vi.waitFor(() => {
      expect(store.getState().installs[0].missingModsBrowserPending).toBe(false);
      expect(store.getState().installs[0].missingModsWatchError).toContain("Downloads unreadable");
    });
    expect(store.getState().installs[0].missingModsBrowserError).toContain("browser unavailable");
    expect(store.getState().installs[0].missingModsWatching).toBe(false);
    fail = false;
    store.getState().retryMissingModsBrowser("e1");
    store.getState().retryMissingModsWatch("e1");
    await vi.waitFor(() => expect(store.getState().installs[0].missingModsBrowserPending).toBe(false));
    expect(store.getState().installs[0].missingModsBrowserError).toBeUndefined();
    expect(store.getState().installs[0].missingModsWatchError).toBeUndefined();
    expect(store.getState().installs[0].missingModsWatching).toBe(true);
    expect(calls.filter((c) => c.cmd === "open_all_missing_mods_browsers")).toHaveLength(2);
    expect(calls.filter((c) => c.cmd === "watch_for_missing_mods")).toHaveLength(2);
  });

  it("watches the union of same-instance cards and replaces it after dismissal", async () => {
    const { store, backendCalls } = await loadStore();
    store.setState({ installs: [
      entry({ instanceId: "inst-1", missingMods: [missing(1, "alpha")] }),
      entry({ id: "e2", instanceId: "inst-1", missingMods: [missing(2, "beta")] }),
      entry({ id: "other", instanceId: "inst-2", missingMods: [missing(3, "gamma")] }),
    ] });
    store.getState().retryMissingModsWatch("e1");
    await vi.waitFor(() => expect(backendCalls().filter((c) => c.cmd === "watch_for_missing_mods")).toHaveLength(1));
    expect(backendCalls()[0].args).toEqual({ instanceId: "inst-1", mods: [missing(1, "alpha"), missing(2, "beta")] });
    expect(store.getState().installs[1].missingModsWatching).toBe(true);
    expect(store.getState().installs[2].missingModsWatching).not.toBe(true);
    store.getState().dismissMissingMod("e1", 1);
    await vi.waitFor(() => expect(backendCalls().filter((c) => c.cmd === "watch_for_missing_mods")).toHaveLength(2));
    expect(backendCalls().filter((c) => c.cmd === "watch_for_missing_mods")[1].args).toEqual({ instanceId: "inst-1", mods: [missing(2, "beta")] });
    store.getState().dismissMissingMod("e2", 2);
    await vi.waitFor(() => expect(backendCalls().filter((c) => c.cmd === "watch_for_missing_mods")).toHaveLength(3));
    expect(backendCalls().filter((c) => c.cmd === "watch_for_missing_mods")[2].args).toEqual({ instanceId: "inst-1", mods: [] });
  });

  it("preserves asynchronous watcher errors and filters completion to each card", async () => {
    const { store } = await loadStore();
    store.setState({ installs: [
      entry({ instanceId: "inst-1", missingMods: [missing(1, "alpha")], missingModsWatching: true }),
      entry({ id: "e2", instanceId: "inst-1", missingMods: [missing(2, "beta")], missingModsWatching: true }),
      entry({ id: "other", instanceId: "inst-2", missingMods: [missing(3, "gamma")], missingModsWatching: true }),
    ] });
    await emit("missing-mods://error", { instanceId: "inst-1", error: "Downloads became unreadable" });
    await emit("missing-mods://done", { instanceId: "inst-1", placed: ["beta"], stillMissing: ["alpha"] });
    expect(store.getState().installs[0]).toMatchObject({ missingModsWatching: false, missingModsWatchError: "Downloads became unreadable", missingModsPlaced: [] });
    expect(store.getState().installs[1]).toMatchObject({ missingModsWatching: false, missingModsPlaced: ["beta"] });
    expect(store.getState().installs[1].missingModsWatchError).toBeUndefined();
    expect(store.getState().installs[2].missingModsWatching).toBe(true);
    expect(store.getState().instanceRefreshTicks).toEqual({ "inst-1": 1 });
  });

  it("shows failed pending-list loads until retry succeeds", async () => {
    const { store } = await loadStore();
    mockIPC((cmd) => {
      if (cmd === "list_pending_missing_mods") throw new Error("manifest unreadable");
    });
    store.getState().reloadPendingMissingMods();
    await vi.waitFor(() => expect(store.getState().pendingMissingModsLoading).toBe(false));
    expect(store.getState().pendingMissingModsError).toContain("manifest unreadable");
    mockIPC((cmd) => cmd === "list_pending_missing_mods" ? [] : undefined);
    store.getState().reloadPendingMissingMods();
    await vi.waitFor(() => expect(store.getState().pendingMissingModsLoading).toBe(false));
    expect(store.getState().pendingMissingModsError).toBeUndefined();
  });
});
