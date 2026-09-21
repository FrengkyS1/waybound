import { useEffect, useLayoutEffect, useRef } from "react";
import { useEscapeKey } from "../hooks/useEscapeKey";
import styles from "./ContextMenu.module.css";

export interface ContextMenuItem {
  label: string;
  onClick: () => void;
  danger?: boolean;
}

interface ContextMenuProps {
  x: number;
  y: number;
  items: ContextMenuItem[];
  onClose: () => void;
}

export function ContextMenu({ x, y, items, onClose }: ContextMenuProps) {
  const ref = useRef<HTMLDivElement>(null);
  const opener = useRef(document.activeElement as HTMLElement | null);
  useEscapeKey(onClose);
  useLayoutEffect(() => {
    ref.current?.querySelector<HTMLButtonElement>("button")?.focus();
    return () => { if (opener.current?.isConnected) opener.current.focus(); };
  }, []);

  useEffect(() => {
    function onPointer(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    // Attach next tick: the right-click that opens this menu is still
    // bubbling/settling (WebView2 can follow it with a trailing native
    // contextmenu/mousedown), and an immediate listener catches that and
    // closes the menu the instant it opens.
    const timer = setTimeout(() => {
      document.addEventListener("mousedown", onPointer);
      document.addEventListener("contextmenu", onPointer);
    }, 0);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("contextmenu", onPointer);
    };
  }, [onClose]);

  // Keep the menu on-screen when it's opened near the right/bottom edge.
  const style = {
    left: Math.max(0, Math.min(x, window.innerWidth - 200)),
    top: Math.max(0, Math.min(y, window.innerHeight - items.length * 36 - 16)),
  };

  return (
    <div
      className={styles.menu}
      role="menu"
      ref={ref}
      style={style}
      onKeyDown={(event) => {
        if (event.key === "Tab") { onClose(); return; }
        if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        const buttons = Array.from(ref.current?.querySelectorAll<HTMLButtonElement>("button") ?? []);
        const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
        const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1
          : (index + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
        buttons[next]?.focus();
      }}
    >
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          role="menuitem"
          className={`${styles.item} ${item.danger ? styles.itemDanger : ""}`}
          onClick={() => {
            opener.current?.focus();
            onClose();
            item.onClick();
          }}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}
