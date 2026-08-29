import { useEffect, useRef, useState } from "react";
import styles from "./CopyNameButton.module.css";

interface CopyNameButtonProps {
  name: string;
}

export function CopyNameButton({ name }: CopyNameButtonProps) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number>(0);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  async function copy(event: React.MouseEvent) {
    event.stopPropagation();
    try {
      await navigator.clipboard.writeText(name);
    } catch {
      return;
    }
    setCopied(true);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 1500);
  }

  return (
    <button
      type="button"
      className={`${styles.copyBtn} ${copied ? styles.copied : ""}`}
      onClick={(e) => void copy(e)}
      onKeyDown={(e) => e.stopPropagation()}
      title={copied ? "Copied!" : `Copy "${name}"`}
      aria-label={`Copy mod name: ${name}`}
    >
      {copied ? (
        <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden focusable="false">
          <path
            d="M2.5 8.5 6.5 12.5 13.5 4"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.8"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      ) : (
        <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden focusable="false">
          <rect
            x="5.2"
            y="5.2"
            width="8.3"
            height="9.3"
            rx="1.4"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.4"
          />
          <path
            d="M10.8 5.2V3.4A1.4 1.4 0 0 0 9.4 2H3.6a1.4 1.4 0 0 0-1.4 1.4v7.2a1.4 1.4 0 0 0 1.4 1.4h1.6"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.4"
          />
        </svg>
      )}
    </button>
  );
}
