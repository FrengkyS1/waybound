import { useEffect, useRef, useState } from "react";
import { fetchAvailableUpdate } from "./updater";

import styles from "./UpdateNotice.module.css";

/**
 * Startup-only update ping: wait until the window is settled, run a silent
 * `check()` (never auto-installing), and if a newer release exists show a
 * single non-intrusive banner. "Update" jumps to Settings; the X dismisses
 * it for the rest of the session. Any failure is swallowed silently.
 */
export function UpdateNotice({ onOpenSettings }: { onOpenSettings: () => void }) {
  const [version, setVersion] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState(false);
  const ran = useRef(false);

  useEffect(() => {
    if (ran.current) return;
    ran.current = true;
    // Give startup (window reveal + first bundle parse) breathing room and
    // duck out of the way of whatever the user does first — a network ping
    // for an update is the least important thing on launch.
    const timer = setTimeout(() => {
      void fetchAvailableUpdate().then(setVersion);
    }, 6000);
    return () => clearTimeout(timer);
  }, []);

  if (dismissed || !version) return null;

  return (
    <div className={styles.banner} role="status">
      <span className={styles.icon}>↑</span>
      <span className={styles.text}>
        Waybound <strong>{version}</strong> is available.
      </span>
      <button
        type="button"
        className={styles.updateBtn}
        onClick={onOpenSettings}
      >
        Update
      </button>
      <button
        type="button"
        className={styles.close}
        aria-label="Dismiss update notice"
        onClick={() => setDismissed(true)}
      >
        ✕
      </button>
    </div>
  );
}
