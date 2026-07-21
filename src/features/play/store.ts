import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { create } from "zustand";

import {
  addPlayTime,
  cancelLaunch,
  getAccount,
  launchInstance,
  logout,
  microsoftLogin,
  type AccountPublic,
  type DeviceCodePrompt,
  type LaunchExitedEvent,
  type LaunchLogEvent,
  type LaunchProgressEvent,
} from "./api";

export type LaunchPhase =
  | "idle"
  | "preparing"
  | "running"
  | "exited"
  | "error"
  | "cancelled";

const LAUNCH_CANCELLED_MESSAGE = "Launch cancelled";

export interface LaunchState {
  instanceId: string;
  instanceName: string;
  phase: LaunchPhase;
  stage: string;
  current: number;
  total: number;
  logs: string[];
  exitCode: number | null;
  error: string | null;
  startedAtMs: number | null;
}

interface PlayStore {
  account: AccountPublic | null;
  accountLoaded: boolean;
  signingIn: boolean;
  devicePrompt: DeviceCodePrompt | null;

  /** Keyed by instanceId, so multiple instances can show launch progress at once. */
  launches: Record<string, LaunchState>;
  /** Logs kept per instance for this session, so the Logs tab persists them. */
  logsByInstance: Record<string, string[]>;
  /** Bumped whenever a play session ends, so views can refresh instance stats. */
  refreshTick: number;

  init: () => Promise<void>;
  signIn: () => Promise<AccountPublic | null>;
  signOut: () => Promise<void>;
  refreshAccount: () => Promise<void>;
  play: (instanceId: string, instanceName: string) => Promise<void>;
  /** Signals the backend to stop an in-flight prepare/download at its next await point. */
  cancelLaunch: (instanceId: string) => void;
  dismissLaunch: (instanceId: string) => void;
  clearDevicePrompt: () => void;
  clearLogs: (instanceId: string) => void;
}

const MAX_LOG_LINES = 500;
const LOG_FLUSH_MS = 150;
let listenersReady = false;

// A chatty modpack can emit hundreds of log lines per second. Applying each
// one as its own store update floods React with re-renders (the always-mounted
// LaunchOverlay re-renders on every line) and can visibly starve unrelated
// updates elsewhere in the app. Batch lines and flush at a capped rate instead.
let logBuffer: LaunchLogEvent[] = [];
let logFlushTimer: ReturnType<typeof setTimeout> | null = null;

export const usePlayStore = create<PlayStore>((set, get) => ({
  account: null,
  accountLoaded: false,
  signingIn: false,
  devicePrompt: null,
  launches: {},
  logsByInstance: {},
  refreshTick: 0,

  init: async () => {
    if (!listenersReady) {
      listenersReady = true;
      await registerListeners(set, get);
    }
    try {
      const account = await getAccount();
      set({ account, accountLoaded: true });
    } catch {
      set({ accountLoaded: true });
    }
  },

  signIn: async () => {
    set({ signingIn: true, devicePrompt: null });
    try {
      const account = await microsoftLogin();
      set({ account, signingIn: false, devicePrompt: null });
      return account;
    } catch (err) {
      set({ signingIn: false });
      throw err;
    }
  },

  signOut: async () => {
    await logout();
    set({ account: null });
  },

  refreshAccount: async () => {
    const account = await getAccount();
    set({ account });
  },

  play: async (instanceId, instanceName) => {
    set((state) => ({
      launches: {
        ...state.launches,
        [instanceId]: {
          instanceId,
          instanceName,
          phase: "preparing",
          stage: "Starting…",
          current: 0,
          total: 0,
          logs: [],
          exitCode: null,
          error: null,
          startedAtMs: null,
        },
      },
      logsByInstance: { ...state.logsByInstance, [instanceId]: [] },
    }));
    try {
      await launchInstance(instanceId);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      const cancelled = message === LAUNCH_CANCELLED_MESSAGE;
      set((state) => {
        const launch = state.launches[instanceId];
        if (!launch) return state;
        return {
          launches: {
            ...state.launches,
            [instanceId]: {
              ...launch,
              phase: cancelled ? "cancelled" : "error",
              error: message,
            },
          },
        };
      });
    }
  },

  cancelLaunch: (instanceId) => void cancelLaunch(instanceId).catch(() => {}),

  dismissLaunch: (instanceId) =>
    set((state) => {
      const { [instanceId]: _removed, ...rest } = state.launches;
      return { launches: rest };
    }),
  clearDevicePrompt: () => set({ devicePrompt: null }),
  clearLogs: (instanceId) =>
    set((state) => {
      const launch = state.launches[instanceId];
      return {
        logsByInstance: { ...state.logsByInstance, [instanceId]: [] },
        launches: launch
          ? { ...state.launches, [instanceId]: { ...launch, logs: [] } }
          : state.launches,
      };
    }),
}));

async function registerListeners(
  set: (
    partial: Partial<PlayStore> | ((s: PlayStore) => Partial<PlayStore>),
  ) => void,
  get: () => PlayStore,
): Promise<UnlistenFn[]> {
  const unlisten: UnlistenFn[] = [];

  unlisten.push(
    await listen<DeviceCodePrompt>("auth://device-code", (event) => {
      set({ devicePrompt: event.payload });
    }),
  );

  unlisten.push(
    await listen<LaunchProgressEvent>("launch://progress", (event) => {
      const { launches } = get();
      const launch = launches[event.payload.instanceId];
      if (!launch) return;
      set({
        launches: {
          ...launches,
          [event.payload.instanceId]: {
            ...launch,
            phase: "preparing",
            stage: event.payload.stage,
            current: event.payload.current,
            total: event.payload.total,
          },
        },
      });
    }),
  );

  unlisten.push(
    await listen<{ instanceId: string }>("launch://started", (event) => {
      const { launches } = get();
      const launch = launches[event.payload.instanceId];
      if (!launch) return;
      set({
        launches: {
          ...launches,
          [event.payload.instanceId]: {
            ...launch,
            phase: "running",
            stage: "Running",
            startedAtMs: Date.now(),
          },
        },
      });
    }),
  );

  unlisten.push(
    await listen<LaunchLogEvent>("launch://log", (event) => {
      logBuffer.push(event.payload);
      if (logFlushTimer === null) {
        logFlushTimer = setTimeout(() => flushLogBuffer(set), LOG_FLUSH_MS);
      }
    }),
  );

  unlisten.push(
    await listen<LaunchExitedEvent>("launch://exited", (event) => {
      // Apply any lines still sitting in the buffer before flipping to
      // "exited", so the final output isn't silently dropped.
      if (logFlushTimer !== null) {
        clearTimeout(logFlushTimer);
        logFlushTimer = null;
      }
      flushLogBuffer(set);

      const { launches, refreshTick } = get();
      const launch = launches[event.payload.instanceId];
      if (!launch) return;
      // Persist the session's play time.
      if (launch.startedAtMs) {
        const seconds = Math.round((Date.now() - launch.startedAtMs) / 1000);
        if (seconds > 0) void addPlayTime(launch.instanceId, seconds);
      }
      set({
        launches: {
          ...launches,
          [event.payload.instanceId]: {
            ...launch,
            phase: "exited",
            stage: "Closed",
            exitCode: event.payload.code,
          },
        },
        refreshTick: refreshTick + 1,
      });
    }),
  );

  return unlisten;
}

function flushLogBuffer(
  set: (
    partial: Partial<PlayStore> | ((s: PlayStore) => Partial<PlayStore>),
  ) => void,
) {
  logFlushTimer = null;
  if (logBuffer.length === 0) return;
  const batch = logBuffer;
  logBuffer = [];

  const linesByInstance = new Map<string, string[]>();
  for (const { instanceId, line } of batch) {
    const lines = linesByInstance.get(instanceId);
    if (lines) lines.push(line);
    else linesByInstance.set(instanceId, [line]);
  }

  set((state) => {
    let logsByInstance = state.logsByInstance;
    let launches = state.launches;
    for (const [instanceId, newLines] of linesByInstance) {
      const merged = [...(logsByInstance[instanceId] ?? []), ...newLines].slice(
        -MAX_LOG_LINES,
      );
      logsByInstance = { ...logsByInstance, [instanceId]: merged };
      const launch = launches[instanceId];
      if (launch) {
        launches = { ...launches, [instanceId]: { ...launch, logs: merged } };
      }
    }
    return { logsByInstance, launches };
  });
}
