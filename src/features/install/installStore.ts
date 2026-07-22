import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";

import {
  cancelInstall,
  dismissMissingMod as dismissMissingModApi,
  fetchPendingMissingMods,
  installMod,
  openAllMissingModsBrowsers,
  openMissingModsBrowser,
  watchForMissingMods,
} from "../browse/api";
import type { InstallModInput, MissingMod } from "../browse/detailTypes";

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
  dismissNotification: (id: string) => void;
  /** Start an install in the background — the UI is never blocked. */
  startInstall: (name: string, input: InstallModInput) => void;
  /** Signals the backend to stop at its next chunk/file boundary. */
  cancel: (id: string) => void;
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
}

export const useInstallStore = create<InstallStore>((set, get) => ({
  installs: [],
  notifications: [],
  refreshTick: 0,

  dismissNotification: (id) =>
    set((s) => ({ notifications: s.notifications.filter((n) => n.id !== id) })),

  startInstall: (name, input) => {
    const id =
      typeof crypto !== "undefined" && crypto.randomUUID
        ? crypto.randomUUID()
        : `${Date.now()}-${Math.random()}`;
    set((s) => ({ installs: [...s.installs, { id, name, status: "installing" }] }));

    void (async () => {
      try {
        const result = await installMod(input, id);
        set((s) => ({
          installs: s.installs.map((e) =>
            e.id === id
              ? {
                  ...e,
                  status: "done",
                  message: result.message,
                  instanceId: result.instance.id,
                  missingMods: result.missingMods.length > 0 ? result.missingMods : undefined,
                }
              : e,
          ),
          refreshTick: s.refreshTick + 1,
        }));
        // Auto-dismiss successful installs after a few seconds — but not
        // when some files need a manual download, since that's an action
        // item the user still needs to read and act on.
        if (!result.hasSkipped) {
          setTimeout(() => get().dismiss(id), 7000);
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        const cancelled = message === CANCELLED_MESSAGE;
        set((s) => ({
          installs: s.installs.map((e) =>
            e.id === id
              ? { ...e, status: cancelled ? "cancelled" : "error", error: message }
              : e,
          ),
        }));
        if (cancelled) setTimeout(() => get().dismiss(id), 4000);
      }
    })();
  },

  cancel: (id) => void cancelInstall(id).catch(() => {}),

  dismiss: (id) => set((s) => ({ installs: s.installs.filter((e) => e.id !== id) })),

  startMissingModsDownload: (id) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry?.missingMods?.length || !entry.instanceId) return;

    set((s) => ({
      installs: s.installs.map((e) =>
        e.id === id ? { ...e, missingModsIndex: 0, missingModsWatching: true, missingModsPlaced: [] } : e,
      ),
    }));
    void openMissingModsBrowser(entry.missingMods[0].url).catch(() => {});
    void watchForMissingMods(entry.instanceId, entry.missingMods).catch(() => {});
  },

  stepMissingMods: (id, direction) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry?.missingMods?.length) return;
    const nextIndex = (entry.missingModsIndex ?? 0) + direction;
    if (nextIndex < 0 || nextIndex >= entry.missingMods.length) return;

    set((s) => ({
      installs: s.installs.map((e) => (e.id === id ? { ...e, missingModsIndex: nextIndex } : e)),
    }));
    void openMissingModsBrowser(entry.missingMods[nextIndex].url).catch(() => {});
  },

  openAllMissingMods: (id) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry?.missingMods?.length || !entry.instanceId) return;

    set((s) => ({
      installs: s.installs.map((e) =>
        e.id === id ? { ...e, missingModsWatching: true, missingModsPlaced: [] } : e,
      ),
    }));
    void openAllMissingModsBrowsers(entry.missingMods.map((m) => m.url)).catch(() => {});
    void watchForMissingMods(entry.instanceId, entry.missingMods).catch(() => {});
  },

  dismissMissingMod: (id, projectId) => {
    const entry = get().installs.find((e) => e.id === id);
    if (!entry?.missingMods?.length || !entry.instanceId) return;
    const stillHasIt = entry.missingMods.some((m) => m.projectId === projectId);
    if (!stillHasIt) return;
    const nextMissingMods = entry.missingMods.filter((m) => m.projectId !== projectId);

    set((s) => ({
      // Nothing left to act on for this entry once its last missing mod is
      // dismissed — same as the watcher placing the final one.
      installs:
        nextMissingMods.length === 0
          ? s.installs.filter((e) => e.id !== id)
          : s.installs.map((e) => {
              if (e.id !== id) return e;
              const index = e.missingModsIndex;
              return {
                ...e,
                missingMods: nextMissingMods,
                missingModsIndex: index === undefined ? undefined : Math.min(index, nextMissingMods.length - 1),
              };
            }),
    }));
    void dismissMissingModApi(entry.instanceId, projectId).catch(() => {});
  },
}));

void listen<InstallProgressEvent>("install://progress", (event) => {
  const { installId, current, total, currentName } = event.payload;
  useInstallStore.setState((s) => ({
    installs: s.installs.map((e) =>
      e.id === installId ? { ...e, current, total, currentName } : e,
    ),
    // HomePage's mod-count badge only watches this counter — without
    // bumping it here too, a modpack install (which can take minutes) left
    // the count frozen at its pre-install value until the whole thing
    // finished, instead of climbing as files actually land.
    refreshTick: s.refreshTick + 1,
  }));
});

// 5s auto-dismiss, same lifetime as the install store's other transient
// toasts (cancelled-install message, etc.) — long enough to read, short
// enough not to pile up during an "Open all" batch of several mods landing
// close together.
const NOTIFICATION_LIFETIME = 5000;

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
      e.instanceId === instanceId && e.missingModsWatching && e.missingMods?.some((m) => m.name === name)
        ? { ...e, missingModsPlaced: [...(e.missingModsPlaced ?? []), name] }
        : e,
    ),
    // The watcher writes straight to the instance's mods folder, bypassing
    // the normal install flow entirely — without this, HomePage's mod count
    // (which only refreshes on refreshTick) would sit stale until the user
    // navigated away and back.
    refreshTick: s.refreshTick + 1,
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
      e.instanceId === instanceId && e.missingModsWatching ? { ...e, missingModsWatching: false } : e,
    ),
  }));
  if (placed.length === 0 && stillMissing.length === 0) return;
  pushNotification(
    stillMissing.length > 0
      ? `${placed.length} mod(s) placed, ${stillMissing.length} still missing`
      : `All ${placed.length} mod(s) placed`,
  );
});

// Re-surfaces the missing-mods flow for anything still outstanding from
// before the app was last closed — that state otherwise only ever lived in
// this same in-memory `installs` array, so a restart silently dropped it and
// the user had no way back to "Download missing mods" short of re-running
// the whole modpack import.
void fetchPendingMissingMods()
  .then((pending) => {
    if (pending.length === 0) return;
    useInstallStore.setState((s) => ({
      installs: [
        ...s.installs,
        ...pending.map(({ instanceId, instanceName, missingMods }) => ({
          id: `pending-missing-mods-${instanceId}`,
          name: instanceName,
          status: "done" as const,
          message: `${missingMods.length} mod(s) from a previous import still need a manual download: ${describeMissingMods(missingMods)}`,
          instanceId,
          missingMods,
        })),
      ],
    }));
  })
  .catch(() => {});
