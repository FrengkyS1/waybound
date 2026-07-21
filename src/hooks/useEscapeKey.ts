import { useEffect } from "react";

export function useEscapeKey(onEscape: (() => void) | undefined, active = true) {
  useEffect(() => {
    if (!active || !onEscape) return;
    const handler = onEscape;
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") handler();
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onEscape, active]);
}
