import { useEffect, useState, useSyncExternalStore } from "react";
import { getVersion } from "@tauri-apps/api/app";

import {
  checkAndInstall,
  getUpdateStatus,
  subscribeToUpdates,
  type UpdateState,
} from "./updater";

import styles from "./SettingsPage.module.css";

const STATUS_LABEL: Record<UpdateState, string> = {
  idle: "Idle",
  checking: "Checking…",
  available: "Update available",
  downloading: "Downloading…",
  installing: "Installing…",
  uptodate: "Up to date",
  error: "Error",
};

export function UpdateSection() {
  const [currentVersion, setCurrentVersion] = useState<string | null>(null);
  const status = useSyncExternalStore(subscribeToUpdates, getUpdateStatus);
  const busy = status.state === "checking" || status.state === "available"
    || status.state === "downloading" || status.state === "installing";

  useEffect(() => {
    let active = true;
    void getVersion().then((version) => {
      if (active) setCurrentVersion(version);
    }).catch(() => {});
    return () => {
      active = false;
    };
  }, []);

  const ready = status.state === "available";
  const downloading = status.state === "downloading";

  return (
    <section className={styles.section} aria-labelledby="updates-heading">
      <div className={styles.sectionHead}>
        <h2 id="updates-heading">Updates</h2>
        <span className={status.state === "error" ? styles.statusPending : styles.statusOk}>
          {STATUS_LABEL[status.state]}
        </span>
      </div>

      <p className={styles.help}>
        {currentVersion
          ? `You’re on Waybound ${currentVersion}.`
          : "Checking your current version…"}
      </p>

      {ready && status.version && (
        <p className={styles.help}>
          Version {status.version} is available.
          {status.notes ? (
            <>
              {" "}
              <span className={styles.code}>{truncate(status.notes, 240)}</span>
            </>
          ) : null}
        </p>
      )}

      {status.detail && <p className={styles.error}>{status.detail}</p>}

      {downloading && (
        <div
          className={styles.help}
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round((status.progress ?? 0) * 100)}
        >
          Downloading update…
          {typeof status.progress === "number"
            ? ` ${Math.round(status.progress * 100)}%`
            : ""}
        </div>
      )}

      <div className={styles.actions}>
        <button
          type="button"
          className={styles.primary}
          disabled={busy}
          onClick={() => void checkAndInstall()}
        >
          {busy ? STATUS_LABEL[status.state] : "Check for updates"}
        </button>
      </div>
    </section>
  );
}

function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max).trimEnd()}…` : text;
}
