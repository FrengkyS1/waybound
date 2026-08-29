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

/**
 * A lightweight, silent check-only probe for use at startup — never installs
 * anything. Returns the version string of an available update, or `null`
 * when the app is current (or when the check fails quietly, e.g. an
 * unconfigured pubkey or endpoint). Any error is swallowed: a startup ping
 * must never disrupt the user.
 */
export async function fetchAvailableUpdate(): Promise<string | null> {
  try {
    const update = await check();
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
 * Reports progress/phase changes through `onStatus`. Returns the final state
 * once the flow finishes or errors. Never throws: any failure (including an
 * unconfigured updater, e.g. when no pubkey/endpoint is set yet) is surfaced
 * as an `"error"` state with a readable `detail`.
 */
export async function checkAndInstall(
  onStatus: (s: UpdateStatus) => void,
): Promise<UpdateStatus> {
  onStatus({ state: "checking" });

  let update: Update | null;
  try {
    update = await check();
  } catch (err) {
    return fail(err, "Couldn't reach the update server.");
  }

  if (!update) {
    onStatus({ state: "uptodate" });
    return { state: "uptodate" };
  }

  onStatus({
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
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength;
        onStatus({
          state: "downloading",
          version: update.version,
          progress: total > 0 ? downloaded / total : undefined,
        });
      } else if (event.event === "Finished") {
        onStatus({ state: "installing", version: update.version });
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
