import { check, type Update } from "@tauri-apps/plugin-updater";

export interface UpdateStatus {
  /** "idle" | "checking" | "available" | "downloading" | "installing" | "uptodate" | "error" */
  state: UpdateState;
  /** The version an update would install (or is installing), when known. */
  version?: string;
  /** Release notes / body text for the available update. */
  notes?: string;
  /** Human-readable explanation for the current state (notably errors). */
  detail?: string;
  /** Fraction of the download completed, 0..1, while `state === "downloading"`. */
  progress?: number;
}

export type UpdateState =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "installing"
  | "uptodate"
  | "error";

// Updater work belongs to the app session, not a Settings component mount.
let status: UpdateStatus = { state: "idle" };
let installJob: Promise<UpdateStatus> | null = null;
let checkJob: Promise<Update | null> | null = null;
const subscribers = new Set<() => void>();

export function getUpdateStatus(): UpdateStatus {
  return status;
}

export function subscribeToUpdates(listener: () => void): () => void {
  subscribers.add(listener);
  return () => {
    subscribers.delete(listener);
  };
}

function publish(next: UpdateStatus) {
  status = next;
  subscribers.forEach((listener) => listener());
}

function checkForUpdate(): Promise<Update | null> {
  if (!checkJob) {
    checkJob = Promise.resolve().then(() => check()).finally(() => {
      checkJob = null;
    });
  }
  return checkJob;
}

/**
 * A lightweight, silent check-only probe for use at startup — never installs
 * anything. Returns the version string of an available update, or `null`
 * when the app is current (or when the check fails quietly, e.g. an
 * unconfigured pubkey or endpoint). Any error is swallowed: a startup ping
 * must never disrupt the user.
 */
export async function fetchAvailableUpdate(): Promise<string | null> {
  try {
    if (installJob) return (await installJob).version ?? null;
    const update = await checkForUpdate();
    return update ? update.version : null;
  } catch {
    return null;
  }
}

/**
 * Checks the configured updater endpoint and, when a newer release exists,
 * downloads and installs it (the convenience `downloadAndInstall` — the
 * installer runs and the app relaunches into the new version afterwards).
 *
 * State and the single in-flight operation survive Settings navigation.
 * Subscribers receive phase changes; the returned promise resolves to the
 * final state. Failures become an `"error"` state with readable `detail`.
 */
export function checkAndInstall(): Promise<UpdateStatus> {
  if (installJob) return installJob;
  installJob = Promise.resolve().then(runCheckAndInstall).then((result) => {
    // A successful passive installer is terminal until the app restarts.
    if (result.state !== "installing") installJob = null;
    publish(result);
    return result;
  });
  publish({ state: "checking" });
  return installJob;
}

async function runCheckAndInstall(): Promise<UpdateStatus> {

  let update: Update | null;
  try {
    update = await checkForUpdate();
  } catch (err) {
    return fail(err, "Couldn't reach the update server.");
  }

  if (!update) {
    return { state: "uptodate" };
  }

  publish({
    state: "available",
    version: update.version,
    notes: update.body,
  });

  // Total size isn't fixed until the transfer's `Started` event arrives, so
  // accumulate the byte count from each progress chunk and report progress as
  // a share of `contentLength` when the server told us one (it may not).
  let total = 0;
  let downloaded = 0;

  try {
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        total = event.data.contentLength ?? 0;
        publish({ state: "downloading", version: update.version });
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength;
        publish({
          state: "downloading",
          version: update.version,
          progress: total > 0 ? downloaded / total : undefined,
        });
      } else if (event.event === "Finished") {
        publish({ state: "installing", version: update.version });
      }
    });
  } catch (err) {
    return fail(err, "The update downloaded but couldn't be installed.");
  }

  // `downloadAndInstall` for NSIS runs a passive install that relaunches the
  // app on exit; this state is effectively terminal until the restart.
  return { state: "installing", version: update.version };
}

function fail(err: unknown, fallback: string): UpdateStatus {
  const detail = err instanceof Error ? err.message : String(err);
  return {
    state: "error",
    // Tauri's updater reports "public key not found" / similar when the app
    // was built without a configured pubkey — a setup gap, not a transient
    // network hiccup. Surface the raw message so it's clear which config key
    // is missing (or that the endpoint is unreachable).
    detail: detail || fallback,
  };
}
