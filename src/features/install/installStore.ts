import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";

import {
  cancelInstall,
  pauseInstall,
  resumeInstall,
  dismissMissingMod as dismissMissingModApi,
  fetchPendingMissingMods,
  installMod,
  openAllMissingModsBrowsers,
  openMissingModsBrowser,
  watchForMissingMods,
} from "../browse/api";
import type { InstallModInput, InstallModResult, MissingMod } from "../browse/detailTypes";

export type InstallStatus = "installing" | "done" | "error" | "cancelled";

const CANCELLED_MESSAGE = "Install cancelled";

// Names only, comma-separated, capped so a pack with a long tail of missing
// mods still reads as a sentence instead of a wall of text.
function describeMissingMods(mods: MissingMod[]): string {
  const MAX_NAMED = 3;
  const names = mods.slice(0, MAX_NAMED).map((m) => m.name);
  const rest = mods.length - names.length;
  return rest > 0 ? `${names.join(", ")}, +${rest} more` : names.join(", ");
}

export interface InstallEntry {
  id: string;
  name: string;
  status: InstallStatus;
  message?: string;
  error?: string;
  instanceId?: string;
  paused?: boolean;
  controlPending?: boolean;
  controlError?: string;
  progressSample?: { current: number; total: number; time: number };
  etaSeconds?: number;
  /** File-count progress, when known (modpack installs report this). */
  current?: number;
  total?: number;
  /** Most recently completed file's name, for "downloading X" instead of
   * just a bare counter. Empty until the first file finishes. */
  currentName?: string;
  /** Files CurseForge won't hand out automatically — present once the
   * "Download missing mods" flow has been offered for this install. */
  missingMods?: MissingMod[];
  /** Index into `missingMods` currently shown in the in-app browser. */
  missingModsIndex?: number;
  /** True once the Downloads-folder watcher has been started. */
  missingModsWatching?: boolean;
  /** Names placed into the instance so far by the watcher. */
  missingModsPlaced?: string[];
  missingModsBrowserPending?: boolean;
  missingModsBrowserError?: string;
  missingModsWatchError?: string;
  missingModsDismissError?: string;
  missingModsDismissPending?: boolean;
  missingModsOpenAll?: boolean;
}

interface InstallProgressEvent {
  installId: string;
  current: number;
  total: number;
  currentName: string;
}

interface MissingModPlacedEvent {
  instanceId: string;
  name: string;
  remaining: number;
  total: number;
}

interface MissingModsWatchDoneEvent {
  instanceId: string;
  placed: string[];
  stillMissing: string[];
}

/** A single-line, auto-dismissing toast for the manual-download watcher —
 * the only feedback the "Open all" flow gets, since it has no stepper UI to
 * show a running placed-count in. */
export interface ModNotification {
  id: string;
  text: string;
}

interface InstallStore {
  installs: InstallEntry[];
  notifications: ModNotification[];
  /** Bumped when an install finishes, so the instance list can refresh. */
  refreshTick: number;
  instanceRefreshTicks: Record<string, number>;
  pendingMissingModsError?: string;
  pendingMissingModsLoading: boolean;
  reloadPendingMissingMods: () => void;
  /** Collapses the bottom-right dock to a peek tab (session-only). */
  dockMinimized: boolean;
  setDockMinimized: (minimized: boolean) => void;
  dismissNotification: (id: string) => void;
  /** Start an install in the background — the UI is never blocked. */
  startInstall: (name: string, input: InstallModInput) => void;
  /** Track every install/update result, including manual-download outcomes. */
  runInstall: (
    name: string,
    operation: (installId: string) => Promise<InstallModResult>,
    instanceId?: string,
  ) => Promise<InstallModResult | null>;
  /** Signals the backend to stop at its next chunk/file boundary. */
  cancel: (id: string) => void;
  setPaused: (id: string, paused: boolean) => void;
  dismiss: (id: string) => void;
  /** Opens the in-app browser at the first missing mod and starts watching
   * Downloads for all of them. */
  startMissingModsDownload: (id: string) => void;
  /** Steps the in-app browser to the next (or previous) missing mod. */
  stepMissingMods: (id: string, direction: 1 | -1) => void;
  /** Opens every missing mod's page at once (each its own window) and starts
   * watching Downloads for all of them. */
  openAllMissingMods: (id: string) => void;
  /** Marks one missing mod as "not getting this" — removed from the list for
   * good (persisted backend-side), not just hidden until the next restart. */
  dismissMissingMod: (id: string, projectId: number) => void;
  retryMissingModsBrowser: (id: string) => void;
  retryMissingModsWatch: (id: string) => void;
}

let pendingMissingModsReloadQueued = false;

export const useInstallStore = create<InstallStore>((set, get) => ({
  installs: [],
  notifications: [],
  refreshTick: 0,
  instanceRefreshTicks: {},
  pendingMissingModsLoading: false,
  dockMinimized: false,

  setDockMinimized: (minimized) => set({ dockMinimized: minimized }),

  dismissNotification: (id) =>
    set((s) => ({ notifications: s.notifications.filter((n) => n.id !== id) })),

  startInstall: (name, input) => {
    void get().runInstall(name, (id) => installMod(input, id), input.instanceId);
  },

  runInstall: async (name, operation, instanceId) => {
    const id = crypto.randomUUID();
    set((s) => ({ installs: [...s.installs, { id, name, instanceId, status: "installing" }] }));
    try {
      const result = await operation(id);
      set((s) => ({
        installs: s.installs.map((e) => e.id === id ? {
          ...e,
          status: "done",
          message: result.message,
          instanceId: result.instance.id,
          missingMods: result.missingMods.length ? result.missingMods : undefined,
        } : e),
        refreshTick: s.refreshTick + 1,
        instanceRefreshTicks: {
          ...s.instanceRefreshTicks,
          [result.instance.id]: (s.instanceRefreshTicks[result.instance.id] ?? 0) + 1,
        },
      }));
      if (result.missingMods.length && get().installs.some((e) =>
        e.instanceId === result.instance.id && e.missingModsWatching)) {
        syncMissingModsWatch(result.instance.id);
      }
      if (!result.hasSkipped && result.missingMods.length === 0) {
        setTimeout(() => get().dismiss(id), 7000);
      }
      return result;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      const cancelled = message === CANCELLED_MESSAGE;
      set((s) => ({
        installs: s.installs.map((e) => e.id === id
          ? { ...e, status: cancelled ? "cancelled" : "error", error: message } : e),
        refreshTick: s.refreshTick + 1,
        instanceRefreshTicks: instanceId ? {
          ...s.instanceRefreshTicks,
          [instanceId]: (s.instanceRefreshTicks[instanceId] ?? 0) + 1,
        } : s.instanceRefreshTicks,
      }));
      // Failed installs may still have durable manual-download work.
      get().reloadPendingMissingMods();
      if (cancelled) setTimeout(() => get().dismiss(id), 4000);
      return null;
    }
  },

  cancel: (id) => {
    void cancelInstall(id).catch((error) => set((s) => ({
      installs: s.installs.map((e) => e.id === id ? { ...e, controlError: String(error) } : e),
    })));
  },

  setPaused: (id, paused) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry || entry.status !== "installing" || entry.controlPending) return;
    set((s) => ({ installs: s.installs.map((e) => e.id === id ? { ...e, controlPending: true, controlError: undefined } : e) }));
    void (paused ? pauseInstall(id) : resumeInstall(id)).then(() => {
      set((s) => ({ installs: s.installs.map((e) => e.id === id && e.status === "installing"
        ? { ...e, paused, controlPending: false, progressSample: undefined, etaSeconds: undefined } : e) }));
    }).catch((error) => {
      set((s) => ({ installs: s.installs.map((e) => e.id === id
        ? { ...e, controlPending: false, controlError: String(error) } : e) }));
    });
  },

  dismiss: (id) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry || entry.missingModsDismissPending) return;
    const remaining = unresolvedMods(entry);
    if (!remaining.length || !entry.instanceId) {
      set((s) => ({ installs: s.installs.filter((e) => e.id !== id) }));
      if (entry.instanceId && entry.missingModsWatching) syncMissingModsWatch(entry.instanceId);
      return;
    }
    updateEntry(id, { missingModsDismissPending: true, missingModsDismissError: undefined });
    void Promise.allSettled(remaining.map((m) => dismissMissingModApi(entry.instanceId!, m.projectId)))
      .then((results) => {
        const failed = remaining.filter((_, index) => results[index].status === "rejected");
        if (!failed.length) {
          set((s) => ({ installs: s.installs.filter((e) => e.id !== id) }));
        } else {
          const failure = results.find((r) => r.status === "rejected");
          updateEntry(id, {
            missingMods: failed,
            missingModsIndex: entry.missingModsIndex === undefined ? undefined : 0,
            missingModsDismissPending: false,
            missingModsDismissError: `Could not dismiss missing mods: ${failure?.status === "rejected" ? String(failure.reason) : "unknown error"}. Retry dismissing.`,
          });
          if (entry.missingModsIndex !== undefined) get().retryMissingModsBrowser(id);
        }
        if (get().installs.some((e) => e.instanceId === entry.instanceId && e.missingModsWatching) || entry.missingModsWatching) {
          syncMissingModsWatch(entry.instanceId!);
        }
      });
  },

  startMissingModsDownload: (id) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry?.missingMods?.length || !entry.instanceId) return;
    const first = entry.missingMods.findIndex((m) => !(entry.missingModsPlaced ?? []).includes(m.name));
    if (first < 0) return;
    updateEntry(id, { missingModsIndex: first, missingModsOpenAll: false });
    get().retryMissingModsBrowser(id);
    get().retryMissingModsWatch(id);
  },

  stepMissingMods: (id, direction) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry?.missingMods?.length || entry.missingModsBrowserPending || entry.missingModsDismissPending) return;
    const placed = new Set(entry.missingModsPlaced ?? []);
    let nextIndex = (entry.missingModsIndex ?? 0) + direction;
    while (nextIndex >= 0 && nextIndex < entry.missingMods.length && placed.has(entry.missingMods[nextIndex].name)) {
      nextIndex += direction;
    }
    if (nextIndex < 0 || nextIndex >= entry.missingMods.length) return;
    updateEntry(id, { missingModsIndex: nextIndex, missingModsOpenAll: false });
    get().retryMissingModsBrowser(id);
  },

  openAllMissingMods: (id) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry?.missingMods?.length || !entry.instanceId || entry.missingModsBrowserPending) return;
    updateEntry(id, { missingModsOpenAll: true });
    get().retryMissingModsBrowser(id);
    get().retryMissingModsWatch(id);
  },

  retryMissingModsBrowser: (id) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry?.missingMods?.length || entry.missingModsBrowserPending) return;
    const remaining = unresolvedMods(entry);
    if (!remaining.length) return;
    const current = entry.missingMods[entry.missingModsIndex ?? 0];
    if (!entry.missingModsOpenAll && (!current || !remaining.includes(current))) return;
    updateEntry(id, { missingModsBrowserPending: true, missingModsBrowserError: undefined });
    const opening = entry.missingModsOpenAll
      ? openAllMissingModsBrowsers(remaining.map((m) => m.url))
      : openMissingModsBrowser(current.url);
    void opening.catch((err) => {
      updateEntry(id, { missingModsBrowserError: `Could not open download page(s): ${String(err)}` });
    }).finally(() => updateEntry(id, { missingModsBrowserPending: false }));
  },

  retryMissingModsWatch: (id) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry?.instanceId || entry.missingModsWatching || !unresolvedMods(entry).length) return;
    syncMissingModsWatch(entry.instanceId);
  },

  dismissMissingMod: (id, projectId) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry?.missingMods?.some((m) => m.projectId === projectId) || !entry.instanceId ||
        entry.missingModsDismissPending || entry.missingModsBrowserPending) return;
    updateEntry(id, { missingModsDismissPending: true, missingModsDismissError: undefined });
    void dismissMissingModApi(entry.instanceId, projectId).then(() => {
      const current = get().installs.find((e) => e.id === id);
      if (!current?.missingMods) return;
      const nextMissingMods = current.missingMods.filter((m) => m.projectId !== projectId);
      if (!nextMissingMods.length) {
        set((s) => ({ installs: s.installs.filter((e) => e.id !== id) }));
        if (current.missingModsWatching) syncMissingModsWatch(entry.instanceId!);
        return;
      }
      const index = current.missingModsIndex;
      const currentProject = index === undefined ? undefined : current.missingMods[index]?.projectId;
      const placed = new Set(current.missingModsPlaced ?? []);
      const remainingIndices = nextMissingMods.flatMap((m, i) => placed.has(m.name) ? [] : [i]);
      const nextIndex = index === undefined || remainingIndices.length === 0 ? undefined : currentProject === projectId
        ? remainingIndices.find((i) => i >= index) ?? remainingIndices[remainingIndices.length - 1]
        : nextMissingMods.findIndex((m) => m.projectId === currentProject);
      updateEntry(id, {
        missingMods: nextMissingMods,
        missingModsIndex: nextIndex,
        missingModsDismissPending: false,
      });
      if (nextIndex !== undefined && currentProject === projectId) get().retryMissingModsBrowser(id);
      if (current.missingModsWatching) syncMissingModsWatch(entry.instanceId!);
    }).catch((err) => updateEntry(id, {
      missingModsDismissPending: false,
      missingModsDismissError: `Could not dismiss this mod: ${String(err)}. Retry “Not installing this”.`,
    }));
  },

  reloadPendingMissingMods: () => {
    if (get().pendingMissingModsLoading) {
      pendingMissingModsReloadQueued = true;
      return;
    }
    set({ pendingMissingModsLoading: true, pendingMissingModsError: undefined });
    void fetchPendingMissingMods().then((pending) => {
      set((s) => ({
        installs: [...s.installs, ...pending.flatMap(({ instanceId, instanceName, missingMods }) => {
          const tracked = new Set(s.installs.filter((e) => e.instanceId === instanceId)
            .flatMap((e) => e.missingMods?.map((m) => m.projectId) ?? []));
          const remaining = missingMods.filter((m) => !tracked.has(m.projectId));
          return remaining.length ? [{
            id: s.installs.some((e) => e.id === `pending-missing-mods-${instanceId}`)
              ? `pending-missing-mods-${instanceId}-${crypto.randomUUID()}`
              : `pending-missing-mods-${instanceId}`,
            name: instanceName,
            status: "done" as const,
            message: `${remaining.length} mod(s) from a previous import still need a manual download: ${describeMissingMods(remaining)}`,
            instanceId,
            missingMods: remaining,
          }] : [];
        })],
      }));
      const watchingInstances = new Set(get().installs
        .filter((e) => e.instanceId && e.missingModsWatching)
        .map((e) => e.instanceId!));
      for (const instanceId of watchingInstances) {
        if (get().installs.some((e) => e.instanceId === instanceId &&
          !e.missingModsWatching && unresolvedMods(e).length)) syncMissingModsWatch(instanceId);
      }
    }).catch((err) => set({ pendingMissingModsError: `Could not restore pending manual downloads: ${String(err)}` }))
      .finally(() => {
        set({ pendingMissingModsLoading: false });
        if (pendingMissingModsReloadQueued) {
          pendingMissingModsReloadQueued = false;
          get().reloadPendingMissingMods();
        }
      });
  },
}));

function updateEntry(id: string, patch: Partial<InstallEntry>) {
  useInstallStore.setState((s) => ({
    installs: s.installs.map((e) => e.id === id ? { ...e, ...patch } : e),
  }));
}

function unresolvedMods(entry: InstallEntry): MissingMod[] {
  const placed = new Set(entry.missingModsPlaced ?? []);
  return entry.missingMods?.filter((m) => !placed.has(m.name)) ?? [];
}

// One backend watcher owns an instance. Replacing it must include every active
// manual-download card, not silently abandon another install's pending files.
const watchRequests = new Map<string, number>();

function syncMissingModsWatch(instanceId: string) {
  const entries = useInstallStore.getState().installs.filter((e) => e.instanceId === instanceId);
  const byProject = new Map<number, MissingMod>();
  for (const entry of entries) {
    for (const mod of unresolvedMods(entry)) byProject.set(mod.projectId, mod);
  }
  const missing = Array.from(byProject.values());
  const request = (watchRequests.get(instanceId) ?? 0) + 1;
  watchRequests.set(instanceId, request);
  useInstallStore.setState((s) => ({
    installs: s.installs.map((e) => e.instanceId === instanceId ? {
      ...e,
      missingModsWatching: unresolvedMods(e).length > 0,
      missingModsWatchError: undefined,
    } : e),
  }));
  // Empty lists stop the existing watch after the last item is dismissed.
  void watchForMissingMods(instanceId, missing).catch((err) => {
    if (watchRequests.get(instanceId) !== request) return;
    useInstallStore.setState((s) => ({
      installs: s.installs.map((e) => e.instanceId === instanceId && unresolvedMods(e).length ? {
        ...e,
        missingModsWatching: false,
        missingModsWatchError: `Could not watch Downloads: ${String(err)}`,
      } : e),
    }));
  });
}

void listen<InstallProgressEvent>("install://progress", (event) => {
  const { installId, current, total, currentName } = event.payload;
  useInstallStore.setState((s) => ({
    installs: s.installs.map((e) => {
      if (e.id !== installId || e.status !== "installing") return e;
      const time = Date.now();
      const previous = e.progressSample;
      const sample = !previous || previous.total !== total || current < previous.current
        ? { current, total, time } : previous;
      const completed = current - sample.current;
      const elapsed = (time - sample.time) / 1000;
      const etaSeconds = !e.paused && completed > 0 && elapsed >= 1 && total > current
        ? Math.ceil((total - current) * elapsed / completed) : undefined;
      return { ...e, current, total, currentName, progressSample: e.paused ? undefined : sample, etaSeconds };
    }),
    // HomePage's mod-count badge only watches this counter — without
    // bumping it here too, a modpack install (which can take minutes) left
    // the count frozen at its pre-install value until the whole thing
    // finished, instead of climbing as files actually land.
    refreshTick: s.refreshTick + 1,
  }));
});

// 5s wasn't enough to actually read a mod's name before it vanished. 8s
// still keeps a big "Open all" batch's toasts from stacking up forever, but
// gives an individual one a real chance to be read.
const NOTIFICATION_LIFETIME = 8000;

function pushNotification(text: string) {
  const id =
    typeof crypto !== "undefined" && crypto.randomUUID ? crypto.randomUUID() : `${Date.now()}-${Math.random()}`;
  useInstallStore.setState((s) => ({ notifications: [...s.notifications, { id, text }] }));
  setTimeout(() => useInstallStore.getState().dismissNotification(id), NOTIFICATION_LIFETIME);
}

// Matched back to install entries by instance id (the watcher is started
// per-instance and the event carries it) — two "download missing mods"
// watches running at once, e.g. two modpack installs sharing a restricted
// core-API mod, would otherwise cross-contaminate: a placed/done event
// meant for one instance updating (or prematurely clearing
// `missingModsWatching` on) an entry for a different one.
void listen<MissingModPlacedEvent>("missing-mods://placed", (event) => {
  const { instanceId, name } = event.payload;
  useInstallStore.setState((s) => ({
    installs: s.installs.map((e) =>
      e.instanceId === instanceId && e.missingMods?.some((m) => m.name === name)
        ? { ...e, missingModsPlaced: [...new Set([...(e.missingModsPlaced ?? []), name])] }
        : e,
    ),
    // The watcher writes straight to the instance's mods folder, bypassing
    // the normal install flow entirely — without this, HomePage's mod count
    // (which only refreshes on refreshTick) would sit stale until the user
    // navigated away and back.
    refreshTick: s.refreshTick + 1,
    instanceRefreshTicks: {
      ...s.instanceRefreshTicks,
      [instanceId]: (s.instanceRefreshTicks[instanceId] ?? 0) + 1,
    },
  }));
  // The stepper flow already shows a running "N/M placed" label, but "Open
  // all" has no such surface — a toast is the only feedback either flow
  // gets that a page's manual download actually landed.
  pushNotification(`✓ ${name} downloaded and placed`);
});

void listen<MissingModsWatchDoneEvent>("missing-mods://done", (event) => {
  const { instanceId, placed, stillMissing } = event.payload;
  useInstallStore.setState((s) => ({
    installs: s.installs.map((e) =>
      e.instanceId === instanceId && e.missingMods?.length ? {
        ...e,
        missingModsWatching: false,
        missingModsPlaced: [...new Set([...(e.missingModsPlaced ?? []), ...placed.filter((name) => e.missingMods?.some((m) => m.name === name))])],
        missingModsWatchError: stillMissing.some((name) => e.missingMods?.some((m) => m.name === name))
          ? e.missingModsWatchError ?? "Some mods are still missing. Retry watching Downloads." : undefined,
      } : e,
    ),
    refreshTick: placed.length ? s.refreshTick + 1 : s.refreshTick,
    instanceRefreshTicks: placed.length ? {
      ...s.instanceRefreshTicks,
      [instanceId]: (s.instanceRefreshTicks[instanceId] ?? 0) + 1,
    } : s.instanceRefreshTicks,
  }));
  if (placed.length === 0 && stillMissing.length === 0) return;
  pushNotification(
    stillMissing.length > 0
      ? `${placed.length} mod(s) placed, ${stillMissing.length} still missing`
      : `All ${placed.length} mod(s) placed`,
  );
});

void listen<{ instanceId: string; error: string }>("missing-mods://error", (event) => {
  useInstallStore.setState((s) => ({
    installs: s.installs.map((e) => e.instanceId === event.payload.instanceId && e.missingMods?.length
      ? { ...e, missingModsWatching: false, missingModsWatchError: event.payload.error } : e),
  }));
});

useInstallStore.getState().reloadPendingMissingMods();
