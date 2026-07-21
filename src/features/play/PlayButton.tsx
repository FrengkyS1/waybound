import { useState } from "react";

import { usePlayStore } from "./store";
import { SignInDialog } from "./SignInDialog";
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

  function handleClick() {
    if (!account) {
      setSignInOpen(true);
      return;
    }
    void play(instanceId, instanceName);
  }

  const label = isBusyHere
    ? launch?.phase === "running"
      ? "Running"
      : "Launching…"
    : account
      ? "Play"
      : "Sign in to play";

  return (
    <>
      <button
        type="button"
        className={variant === "primary" ? styles.primary : styles.compact}
        onClick={handleClick}
        disabled={disabled || isBusyHere}
      >
        <span className={styles.icon} aria-hidden>
          ▶
        </span>
        {label}
      </button>
      {signInOpen && (
        <SignInDialog
          onClose={() => setSignInOpen(false)}
          onSignedIn={() => void play(instanceId, instanceName)}
        />
      )}
    </>
  );
}
