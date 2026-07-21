import { useEffect, useRef, useState } from "react";

import { usePlayStore, type LaunchState } from "./store";
import styles from "./LaunchOverlay.module.css";

export function LaunchOverlay() {
  const launches = usePlayStore((s) => s.launches);
  const entries = Object.values(launches);
  if (entries.length === 0) return null;

  return (
    <div className={styles.dock} role="status" aria-live="polite">
      {entries.map((launch) => (
        <LaunchCard key={launch.instanceId} launch={launch} />
      ))}
    </div>
  );
}

function LaunchCard({ launch }: { launch: LaunchState }) {
  const dismiss = usePlayStore((s) => s.dismissLaunch);
  const play = usePlayStore((s) => s.play);
  const cancelLaunch = usePlayStore((s) => s.cancelLaunch);
  const [showLog, setShowLog] = useState(false);
  const logRef = useRef<HTMLPreElement>(null);

  // Auto-scroll the log to the newest line.
  useEffect(() => {
    if (showLog && logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [launch.logs, showLog]);

  const { phase, stage, current, total, logs, exitCode, error, instanceName } =
    launch;
  const pct = total > 0 ? Math.round((current / total) * 100) : null;
  // "running" is deliberately excluded: dismissing removes the launches[]
  // entry entirely, which is also what PlayButton checks to disable Play and
  // what the exit-event listener keys off of. Dismissing a running game would
  // let the user click Play again and start a second process for the same
  // instance, and would orphan the eventual launch://exited event.
  const dismissable =
    phase === "exited" || phase === "error" || phase === "cancelled";

  const statusText =
    phase === "error"
      ? "Failed to launch"
      : phase === "cancelled"
        ? "Cancelled"
        : phase === "exited"
          ? exitCode === 0 || exitCode === null
            ? "Minecraft closed"
            : `Minecraft closed (exit ${exitCode})`
          : phase === "running"
            ? "Minecraft is running"
            : stage;

  return (
    <div className={styles.cardWrap}>
      <div className={styles.card} data-phase={phase}>
        <div className={styles.main}>
          <div className={styles.headline}>
            <span
              className={`${styles.dot} ${styles[`dot_${phase}`]}`}
              aria-hidden
            />
            <span className={styles.instance}>{instanceName}</span>
            <span className={styles.status}>{statusText}</span>
          </div>

          {phase === "preparing" && (
            <div className={styles.progressWrap}>
              <div className={styles.track}>
                <div
                  className={styles.fill}
                  style={
                    pct === null
                      ? undefined
                      : { transform: `scaleX(${pct / 100})` }
                  }
                  data-indeterminate={pct === null}
                />
              </div>
              <span className={styles.progressLabel}>
                {stage}
                {pct !== null && ` · ${pct}%`}
              </span>
            </div>
          )}

          {phase === "error" && error && (
            <p className={styles.error}>{error}</p>
          )}
        </div>

        <div className={styles.actions}>
          {logs.length > 0 && (
            <button
              type="button"
              className={styles.ghost}
              onClick={() => setShowLog((v) => !v)}
            >
              {showLog ? "Hide log" : "Log"}
            </button>
          )}
          {(phase === "exited" || phase === "error") && (
            <button
              type="button"
              className={styles.retry}
              onClick={() => void play(launch.instanceId, instanceName)}
            >
              Play again
            </button>
          )}
          {phase === "preparing" && (
            <button
              type="button"
              className={styles.ghost}
              onClick={() => cancelLaunch(launch.instanceId)}
            >
              Cancel
            </button>
          )}
          {dismissable && (
            <button
              type="button"
              className={styles.ghost}
              onClick={() => dismiss(launch.instanceId)}
              aria-label="Dismiss"
            >
              Close
            </button>
          )}
        </div>
      </div>

      {showLog && logs.length > 0 && (
        <pre className={styles.log} ref={logRef}>
          {logs.join("\n")}
        </pre>
      )}
    </div>
  );
}
