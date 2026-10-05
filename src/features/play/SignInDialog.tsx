import { useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";

import { usePlayStore } from "./store";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import { useModalFocus } from "../../hooks/useModalFocus";
import styles from "./SignInDialog.module.css";

interface SignInDialogProps {
  onClose: () => void;
  onSignedIn?: () => void;
}

export function SignInDialog({ onClose, onSignedIn }: SignInDialogProps) {
  const signingIn = usePlayStore((s) => s.signingIn);
  const devicePrompt = usePlayStore((s) => s.devicePrompt);
  const signIn = usePlayStore((s) => s.signIn);
  const cancelSignIn = usePlayStore((s) => s.cancelSignIn);

  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const active = useRef(true);
  const closing = useRef(false);
  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
      void cancelSignIn().catch(() => {});
    };
  }, [cancelSignIn]);
  async function close() {
    closing.current = true;
    try {
      await cancelSignIn();
      if (active.current) onClose();
    } catch (err) {
      closing.current = false;
      if (active.current) setError(String(err));
    }
  }
  useEscapeKey(() => void close());
  const modalRef = useModalFocus();

  // Once a code arrives, open the Microsoft sign-in page automatically.
  useEffect(() => {
    if (devicePrompt) void openUrl(devicePrompt.verificationUri);
  }, [devicePrompt]);

  async function handleSignIn() {
    setError(null);
    setCopied(false);
    closing.current = false;
    try {
      const account = await signIn();
      if (!active.current || closing.current || !account) return;
      onSignedIn?.();
      onClose();
    } catch (err) {
      if (active.current && !closing.current) {
        setError(err instanceof Error ? err.message : String(err));
      }
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
      onClick={() => void close()}
    >
      <div
        className={styles.dialog}
        ref={modalRef}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-labelledby="signin-title"
        onClick={(e) => e.stopPropagation()}
      >
        <header className={styles.header}>
          <h2 id="signin-title" className={styles.title}>
            Sign in to Minecraft
          </h2>
          <button
            type="button"
            className={styles.close}
            onClick={() => void close()}
            aria-label={signingIn ? "Cancel sign-in" : "Close"}
          >
            ×
          </button>
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
                : error ? "Retry sign-in" : "Get device code & sign in"}
            </button>
          </div>
        )}

        {error && <p className={styles.error} role="alert">{error}</p>}
      </div>
    </div>
  );
}
