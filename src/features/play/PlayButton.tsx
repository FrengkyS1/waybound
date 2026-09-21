import { useState } from "react";

import { usePlayStore } from "./store";
import { checkLaunchReadiness, type LaunchReadiness } from "./api";
import { SignInDialog } from "./SignInDialog";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import styles from "./PlayButton.module.css";

interface PlayButtonProps {
  instanceId: string;
  instanceName: string;
  /** "primary" for the big detail-panel button, "compact" for inline use. */
  variant?: "primary" | "compact";
  disabled?: boolean;
}

export function PlayButton({
  instanceId,
  instanceName,
  variant = "primary",
  disabled = false,
}: PlayButtonProps) {
  const account = usePlayStore((s) => s.account);
  const play = usePlayStore((s) => s.play);
  const launch = usePlayStore((s) => s.launches[instanceId]);
  const [signInOpen, setSignInOpen] = useState(false);

  const isBusyHere =
    !!launch && (launch.phase === "preparing" || launch.phase === "running");

  const [checking, setChecking] = useState(false);
  const [readiness, setReadiness] = useState<LaunchReadiness | null>(null);

  // The game is the final judge of whether it starts, but a jar built for
  // the wrong loader (or a missing required dep) fails 100% of the time —
  // surfacing that BEFORE the minutes-long launch saves a crash-log hunt.
  // A failed check itself never blocks: launch anyway and let the game speak.
  async function startPlay() {
    if (checking || isBusyHere) return;
    setChecking(true);
    try {
      const report = await checkLaunchReadiness(instanceId);
      if (report.wrongLoader.length === 0 && report.missingDeps.length === 0) {
        void play(instanceId, instanceName);
      } else {
        setReadiness(report);
      }
    } catch {
      void play(instanceId, instanceName);
    } finally {
      setChecking(false);
    }
  }

  function handleClick() {
    if (!account) {
      setSignInOpen(true);
      return;
    }
    void startPlay();
  }

  const label = isBusyHere
    ? launch?.phase === "running"
      ? "Running"
      : "Launching."
    : checking
      ? "Checking…"
      : account
        ? "Play"
        : "Sign in to play";

  return (
    <>
      <button
        type="button"
        className={variant === "primary" ? styles.primary : styles.compact}
        onClick={handleClick}
        disabled={disabled || isBusyHere || checking}
      >
        <span className={styles.icon} aria-hidden>
          ?
        </span>
        {label}
      </button>
      {signInOpen && (
        <SignInDialog
          onClose={() => setSignInOpen(false)}
          onSignedIn={() => void startPlay()}
        />
      )}
      {readiness && (
        <ConfirmDialog
          title="Possible mod problems"
          message={readinessMessage(readiness)}
          confirmLabel="Launch anyway"
          onConfirm={() => {
            setReadiness(null);
            void play(instanceId, instanceName);
          }}
          onCancel={() => setReadiness(null)}
        />
      )}
    </>
  );
}

const MAX_LISTED = 3;

function capitalize(s: string): string {
  return s ? s[0].toUpperCase() + s.slice(1) : s;
}

function readinessMessage(r: LaunchReadiness): string {
  const lines: string[] = [];
  for (const w of r.wrongLoader.slice(0, MAX_LISTED)) {
    lines.push(
      `"${w.modName ?? w.fileName}" is a ${capitalize(w.detectedLoader)} mod — it won't load here and anything depending on it will fail.`,
    );
  }
  for (const m of r.missingDeps.slice(0, MAX_LISTED)) {
    lines.push(
      `"${m.modName ?? m.fileName}" needs "${m.depModId}"${m.versionRange ? ` (${m.versionRange})` : ""}, which isn't installed.`,
    );
  }
  const hidden =
    Math.max(0, r.wrongLoader.length - MAX_LISTED) +
    Math.max(0, r.missingDeps.length - MAX_LISTED);
  if (hidden > 0) lines.push(`…and ${hidden} more.`);
  return lines.join("\n");
}
