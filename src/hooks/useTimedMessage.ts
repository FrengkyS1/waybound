import { useEffect, useRef, useState } from "react";

const DEFAULT_DURATION_MS = 4000;

/**
 * A success-confirmation string that clears itself after a few seconds,
 * matching the auto-dismissing install/launch toasts instead of lingering
 * until the next action. Errors should use a separate, persistent `useState`
 * — only transient confirmations belong here.
 */
export function useTimedMessage(durationMs = DEFAULT_DURATION_MS) {
  const [message, setMessage] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    return () => {
      if (timer.current) clearTimeout(timer.current);
    };
  }, []);

  function show(text: string) {
    if (timer.current) clearTimeout(timer.current);
    setMessage(text);
    timer.current = setTimeout(() => setMessage(null), durationMs);
  }

  function clear() {
    if (timer.current) clearTimeout(timer.current);
    setMessage(null);
  }

  return { message, showMessage: show, clearMessage: clear };
}
