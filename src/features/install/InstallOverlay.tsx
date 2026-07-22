import { useInstallStore } from "./installStore";
import styles from "./InstallOverlay.module.css";

const URL_PATTERN = /(https?:\/\/\S+)/g;

// Backend messages embed manual-download links as plain URLs in the text
// (e.g. the CurseForge distribution-restricted note) — this turns those into
// clickable links without the backend needing to know anything about HTML.
function renderStatusText(text: string) {
  return text.split("\n").map((line, lineIndex) => (
    <span key={lineIndex}>
      {lineIndex > 0 && <br />}
      {line.split(URL_PATTERN).map((part, partIndex) =>
        part.startsWith("http") ? (
          <a
            key={partIndex}
            className={styles.statusLink}
            href={part}
            target="_blank"
            rel="noreferrer"
          >
            {part}
          </a>
        ) : (
          part
        ),
      )}
    </span>
  ));
}

export function InstallOverlay() {
  const installs = useInstallStore((s) => s.installs);
  const notifications = useInstallStore((s) => s.notifications);
  const dismissNotification = useInstallStore((s) => s.dismissNotification);
  const cancel = useInstallStore((s) => s.cancel);
  const dismiss = useInstallStore((s) => s.dismiss);
  const startMissingModsDownload = useInstallStore((s) => s.startMissingModsDownload);
  const stepMissingMods = useInstallStore((s) => s.stepMissingMods);
  const openAllMissingMods = useInstallStore((s) => s.openAllMissingMods);
  const dismissMissingMod = useInstallStore((s) => s.dismissMissingMod);

  if (installs.length === 0 && notifications.length === 0) return null;

  return (
    <div className={styles.dock} role="status" aria-live="polite">
      {notifications.map((n) => (
        <button
          key={n.id}
          type="button"
          className={styles.notification}
          onClick={() => dismissNotification(n.id)}
        >
          {n.text}
        </button>
      ))}
      {installs.map((entry) => {
        const hasProgress =
          entry.status === "installing" && entry.total !== undefined && entry.total > 0;
        const pct = hasProgress ? Math.round((entry.current! / entry.total!) * 100) : null;
        const statusText =
          entry.status === "installing"
            ? hasProgress
              ? `${entry.current}/${entry.total} files${entry.currentName ? ` — ${entry.currentName}` : ""}`
              : "Installing…"
            : entry.status === "done"
              ? entry.message ?? "Installed"
              : entry.status === "cancelled"
                ? "Cancelled"
                : entry.error ?? "Install failed";

        // "Open all" has no stepper to hide behind, so without filtering
        // placed mods out here, the buttons kept showing the original total
        // forever — never shrinking as the watcher placed files, never
        // disappearing once every one of them landed.
        const placedNames = new Set(entry.missingModsPlaced ?? []);
        const remainingMissingMods = entry.missingMods?.filter((m) => !placedNames.has(m.name)) ?? [];

        return (
          <div key={entry.id} className={styles.card} data-status={entry.status}>
            <span className={`${styles.dot} ${styles[`dot_${entry.status}`]}`} aria-hidden />
            <div className={styles.body}>
              <span className={styles.name}>{entry.name}</span>
              {entry.status === "installing" && (
                <div className={styles.track}>
                  <div
                    className={styles.fill}
                    data-indeterminate={!hasProgress}
                    style={hasProgress ? { transform: `scaleX(${pct! / 100})` } : undefined}
                  />
                </div>
              )}
              <span className={styles.status}>{renderStatusText(statusText)}</span>
              {entry.status === "done" && entry.missingMods && remainingMissingMods.length > 0 && (
                entry.missingModsIndex === undefined ? (
                  <div className={styles.missingModsActions}>
                    {remainingMissingMods.length === 1 && (
                      <span className={styles.missingModsLabel}>{remainingMissingMods[0].name}</span>
                    )}
                    <button
                      type="button"
                      className={styles.missingModsButton}
                      onClick={() => startMissingModsDownload(entry.id)}
                    >
                      Download missing mods ({remainingMissingMods.length})
                    </button>
                    {remainingMissingMods.length > 1 && (
                      <button
                        type="button"
                        className={styles.missingModsButton}
                        onClick={() => openAllMissingMods(entry.id)}
                      >
                        Open all ({remainingMissingMods.length})
                      </button>
                    )}
                    {remainingMissingMods.length === 1 && (
                      <button
                        type="button"
                        className={styles.missingModsDismiss}
                        onClick={() => dismissMissingMod(entry.id, remainingMissingMods[0].projectId)}
                      >
                        Not installing this
                      </button>
                    )}
                  </div>
                ) : (
                  <div className={styles.missingModsProgress}>
                    <button
                      type="button"
                      className={styles.stepButton}
                      disabled={entry.missingModsIndex === 0}
                      onClick={() => stepMissingMods(entry.id, -1)}
                      aria-label="Previous mod"
                    >
                      ‹
                    </button>
                    <span className={styles.missingModsLabel}>
                      {entry.missingModsPlaced?.length ?? 0}/{entry.missingMods.length} placed —{" "}
                      {entry.missingMods[entry.missingModsIndex].name}
                    </span>
                    <button
                      type="button"
                      className={styles.stepButton}
                      disabled={entry.missingModsIndex >= entry.missingMods.length - 1}
                      onClick={() => stepMissingMods(entry.id, 1)}
                      aria-label="Next mod"
                    >
                      ›
                    </button>
                    <button
                      type="button"
                      className={styles.missingModsDismiss}
                      onClick={() => dismissMissingMod(entry.id, entry.missingMods![entry.missingModsIndex!].projectId)}
                    >
                      Not installing this
                    </button>
                  </div>
                )
              )}
            </div>
            {entry.status === "installing" ? (
              <button
                type="button"
                className={styles.close}
                aria-label="Cancel install"
                title="Cancel"
                onClick={() => cancel(entry.id)}
              >
                ×
              </button>
            ) : (
              <button
                type="button"
                className={styles.close}
                aria-label="Dismiss"
                onClick={() => dismiss(entry.id)}
              >
                ×
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}
