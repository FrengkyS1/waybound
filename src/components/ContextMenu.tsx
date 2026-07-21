import { useEffect, useRef } from "react";
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
  useEscapeKey(onClose);

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
    left: Math.min(x, window.innerWidth - 200),
    top: Math.min(y, window.innerHeight - items.length * 36 - 16),
  };

  return (
    <div
      className={styles.menu}
      role="menu"
      ref={ref}
      style={style}
    >
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          role="menuitem"
          className={`${styles.item} ${item.danger ? styles.itemDanger : ""}`}
          onClick={() => {
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
