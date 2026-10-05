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
  const setPaused = useInstallStore((s) => s.setPaused);
  const dismiss = useInstallStore((s) => s.dismiss);
  const startMissingModsDownload = useInstallStore((s) => s.startMissingModsDownload);
  const stepMissingMods = useInstallStore((s) => s.stepMissingMods);
  const openAllMissingMods = useInstallStore((s) => s.openAllMissingMods);
  const dismissMissingMod = useInstallStore((s) => s.dismissMissingMod);
  const dockMinimized = useInstallStore((s) => s.dockMinimized);
  const setDockMinimized = useInstallStore((s) => s.setDockMinimized);
  const retryMissingModsBrowser = useInstallStore((s) => s.retryMissingModsBrowser);
  const retryMissingModsWatch = useInstallStore((s) => s.retryMissingModsWatch);
  const pendingMissingModsError = useInstallStore((s) => s.pendingMissingModsError);
  const pendingMissingModsLoading = useInstallStore((s) => s.pendingMissingModsLoading);
  const reloadPendingMissingMods = useInstallStore((s) => s.reloadPendingMissingMods);

  if (installs.length === 0 && notifications.length === 0 && !pendingMissingModsError && !pendingMissingModsLoading) return null;

  const total = installs.length + notifications.length + (pendingMissingModsError || pendingMissingModsLoading ? 1 : 0);

  // Minimized: just a peek tab on the bottom-right edge — the dock's
  // presence stays discoverable without covering anything. Clicking it
  // brings the full dock back.
  if (dockMinimized) {
    const anyActive = installs.some((e) => e.status === "installing");
    return (
      <button
        type="button"
        className={styles.peek}
        onClick={() => setDockMinimized(false)}
        aria-label={`Show ${total} notification${total === 1 ? "" : "s"}`}
        title="Show notifications"
      >
        <span
          className={`${styles.dot} ${anyActive ? styles.dot_installing : styles.dot_done}`}
          aria-hidden
        />
        <span aria-hidden>▴</span>
        <span>{total}</span>
      </button>
    );
  }

  return (
    <div className={styles.dock} role="status" aria-live="polite">
      <div className={styles.dockHeader}>
        <span className={styles.dockTitle}>
          {total} background {total === 1 ? "item" : "items"}
        </span>
        <button
          type="button"
          className={styles.minimize}
          onClick={() => setDockMinimized(true)}
          aria-label="Minimize notifications"
          title="Minimize"
        >
          ▾
        </button>
      </div>
      {(pendingMissingModsError || pendingMissingModsLoading) && (
        <div className={styles.card}>
          <div className={styles.body}>
            <span className={styles.name}>Pending manual downloads</span>
            {pendingMissingModsError && <span className={styles.status} role="alert">{pendingMissingModsError}</span>}
            <button type="button" className={styles.missingModsButton} disabled={pendingMissingModsLoading} onClick={reloadPendingMissingMods}>
              {pendingMissingModsLoading ? "Restoring…" : "Retry manual downloads"}
            </button>
          </div>
        </div>
      )}
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
          entry.status === "installing" && entry.current !== undefined && entry.total !== undefined && entry.total > 0;
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
              {entry.status === "installing" && entry.paused && <span className={styles.status}>Paused — resume to continue</span>}
              {entry.status === "installing" && !entry.paused && entry.etaSeconds !== undefined && (
                <span className={styles.status}>About {entry.etaSeconds < 60 ? `${entry.etaSeconds}s` : `${Math.ceil(entry.etaSeconds / 60)} min`} remaining (file-count estimate)</span>
              )}
              {entry.controlError && <span role="alert" className={styles.status}>{entry.controlError}</span>}
              {entry.missingModsBrowserError && (
                <div className={styles.missingModsActions}>
                  <span role="alert" className={styles.status}>{entry.missingModsBrowserError}</span>
                  <button type="button" className={styles.missingModsButton} disabled={entry.missingModsBrowserPending} onClick={() => retryMissingModsBrowser(entry.id)}>
                    {entry.missingModsBrowserPending ? "Opening…" : entry.missingModsOpenAll ? "Retry open all" : "Retry download page"}
                  </button>
                </div>
              )}
              {entry.missingModsWatchError && (
                <div className={styles.missingModsActions}>
                  <span role="alert" className={styles.status}>{entry.missingModsWatchError}</span>
                  <button type="button" className={styles.missingModsButton} disabled={entry.missingModsWatching} onClick={() => retryMissingModsWatch(entry.id)}>
                    Retry watching Downloads
                  </button>
                </div>
              )}
              {entry.missingModsDismissError && <span role="alert" className={styles.status}>{entry.missingModsDismissError}</span>}
              {entry.missingModsWatching && remainingMissingMods.length > 0 && <span className={styles.status}>Watching Downloads — downloaded files are placed automatically.</span>}
              {entry.status === "installing" && (
                <button type="button" className={styles.missingModsButton} disabled={entry.controlPending}
                  onClick={() => setPaused(entry.id, !entry.paused)}>
                  {entry.controlPending ? "Updating…" : entry.paused ? "Resume" : "Pause"}
                </button>
              )}
              {entry.status !== "installing" && entry.missingMods && remainingMissingMods.length > 0 && (
                entry.missingModsIndex === undefined ? (
                  <div className={styles.missingModsActions}>
                    {remainingMissingMods.length === 1 && (
                      <span className={styles.missingModsLabel}>{remainingMissingMods[0].name}</span>
                    )}
                    <button
                      type="button"
                      className={styles.missingModsButton}
                      disabled={entry.missingModsBrowserPending || entry.missingModsDismissPending}
                      onClick={() => startMissingModsDownload(entry.id)}
                    >
                      Download missing mods ({remainingMissingMods.length})
                    </button>
                    {remainingMissingMods.length > 1 && (
                      <button
                        type="button"
                        className={styles.missingModsButton}
                        onClick={() => openAllMissingMods(entry.id)}
                        disabled={entry.missingModsBrowserPending || entry.missingModsDismissPending}
                      >
                        Open all ({remainingMissingMods.length})
                      </button>
                    )}
                    {remainingMissingMods.length === 1 && (
                      <button
                        type="button"
                        className={styles.missingModsDismiss}
                        onClick={() => dismissMissingMod(entry.id, remainingMissingMods[0].projectId)}
                        disabled={entry.missingModsBrowserPending || entry.missingModsDismissPending}
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
                      disabled={entry.missingModsIndex === 0 || entry.missingModsBrowserPending || entry.missingModsDismissPending}
                      onClick={() => stepMissingMods(entry.id, -1)}
                      aria-label="Previous mod"
                    >
                      ‹
                    </button>
                    <span className={styles.missingModsLabel}>
                      {entry.missingModsPlaced?.length ?? 0}/{entry.missingMods.length} placed —{" "}
                      {entry.missingMods[entry.missingModsIndex]?.name}
                    </span>
                    <button
                      type="button"
                      className={styles.stepButton}
                      disabled={entry.missingModsIndex >= entry.missingMods.length - 1 || entry.missingModsBrowserPending || entry.missingModsDismissPending}
                      onClick={() => stepMissingMods(entry.id, 1)}
                      aria-label="Next mod"
                    >
                      ›
                    </button>
                    <button
                      type="button"
                      className={styles.missingModsDismiss}
                      onClick={() => dismissMissingMod(entry.id, entry.missingMods![entry.missingModsIndex!].projectId)}
                      disabled={entry.missingModsBrowserPending || entry.missingModsDismissPending}
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
                disabled={entry.missingModsDismissPending}
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
