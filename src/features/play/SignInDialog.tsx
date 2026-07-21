import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";

import { usePlayStore } from "./store";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import styles from "./SignInDialog.module.css";

interface SignInDialogProps {
  onClose: () => void;
  onSignedIn?: () => void;
}

export function SignInDialog({ onClose, onSignedIn }: SignInDialogProps) {
  const signingIn = usePlayStore((s) => s.signingIn);
  const devicePrompt = usePlayStore((s) => s.devicePrompt);
  const signIn = usePlayStore((s) => s.signIn);
  const clearDevicePrompt = usePlayStore((s) => s.clearDevicePrompt);

  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => () => clearDevicePrompt(), [clearDevicePrompt]);
  useEscapeKey(onClose, !signingIn);

  // Once a code arrives, open the Microsoft sign-in page automatically.
  useEffect(() => {
    if (devicePrompt) void openUrl(devicePrompt.verificationUri);
  }, [devicePrompt]);

  async function handleSignIn() {
    setError(null);
    try {
      await signIn();
      onSignedIn?.();
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function copyCode() {
    if (!devicePrompt) return;
    try {
      await navigator.clipboard.writeText(devicePrompt.userCode);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard may be unavailable; ignore */
    }
  }

  return (
    <div
      className={styles.backdrop}
      role="presentation"
      onClick={signingIn ? undefined : onClose}
    >
      <div
        className={styles.dialog}
        role="dialog"
        aria-modal="true"
        aria-labelledby="signin-title"
        onClick={(e) => e.stopPropagation()}
      >
        <header className={styles.header}>
          <h2 id="signin-title" className={styles.title}>
            Sign in to Minecraft
          </h2>
          {!signingIn && (
            <button
              type="button"
              className={styles.close}
              onClick={onClose}
              aria-label="Close"
            >
              ×
            </button>
          )}
        </header>

        {devicePrompt ? (
          <div className={styles.body}>
            <p className={styles.lead}>
              Enter this code on the Microsoft sign-in page (we opened it for
              you), then approve the request.
            </p>
            <div className={styles.codeRow}>
              <code className={styles.code}>{devicePrompt.userCode}</code>
              <button
                type="button"
                className={styles.copyBtn}
                onClick={() => void copyCode()}
              >
                {copied ? "Copied" : "Copy"}
              </button>
            </div>
            <button
              type="button"
              className={styles.primary}
              onClick={() => void openUrl(devicePrompt.verificationUri)}
            >
              Reopen Microsoft sign-in →
            </button>
            <p className={styles.waiting}>
              <span className={styles.spinner} aria-hidden /> Waiting for you to
              finish…
            </p>
          </div>
        ) : (
          <div className={styles.body}>
            <p className={styles.lead}>
              Waybound signs you in with your own Microsoft account to launch
              Minecraft. It never sees your password — you approve access on
              Microsoft's page, then close it. No setup needed.
            </p>
            <button
              type="button"
              className={styles.primary}
              disabled={signingIn}
              onClick={() => void handleSignIn()}
            >
              {signingIn
                ? "Contacting Microsoft…"
                : "Get device code & sign in"}
            </button>
          </div>
        )}

        {error && <p className={styles.error}>{error}</p>}
      </div>
    </div>
  );
}
