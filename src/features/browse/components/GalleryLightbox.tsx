import { useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import type { GalleryItem } from "../detailTypes";
import { useEscapeKey } from "../../../hooks/useEscapeKey";
import styles from "./GalleryLightbox.module.css";

interface GalleryLightboxProps {
  items: GalleryItem[];
  initialIndex: number;
  onClose: () => void;
}

const ZOOM_SCALE = 2.5;
// Below this, a pointerdown->up counts as a click (toggle zoom) rather than
// a drag — real pointer input never lands back on the exact same pixel.
const DRAG_THRESHOLD = 4;

export function GalleryLightbox({ items, initialIndex, onClose }: GalleryLightboxProps) {
  const [index, setIndex] = useState(initialIndex);
  const [zoomed, setZoomed] = useState(false);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [panning, setPanning] = useState(false);
  const dragRef = useRef<{ startX: number; startY: number; panX: number; panY: number; dragged: boolean } | null>(
    null,
  );
  const item = items[index];

  useEscapeKey(onClose);

  function step(direction: 1 | -1) {
    setIndex((i) => (i + direction + items.length) % items.length);
    setZoomed(false);
    setPan({ x: 0, y: 0 });
  }

  function handlePointerDown(e: ReactPointerEvent<HTMLImageElement>) {
    if (!zoomed) return;
    // Best-effort: capture keeps the drag tracking even if the pointer
    // leaves the image mid-gesture, but its failure shouldn't stop the pan
    // itself from being tracked below.
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* not a capturable pointer session — pan still works without it */
    }
    setPanning(true);
    dragRef.current = { startX: e.clientX, startY: e.clientY, panX: pan.x, panY: pan.y, dragged: false };
  }

  function handlePointerMove(e: ReactPointerEvent<HTMLImageElement>) {
    const drag = dragRef.current;
    if (!drag) return;
    const dx = e.clientX - drag.startX;
    const dy = e.clientY - drag.startY;
    if (Math.abs(dx) > DRAG_THRESHOLD || Math.abs(dy) > DRAG_THRESHOLD) drag.dragged = true;
    setPan({ x: drag.panX + dx, y: drag.panY + dy });
  }

  function handlePointerUp() {
    const wasDrag = dragRef.current?.dragged ?? false;
    dragRef.current = null;
    setPanning(false);
    if (!wasDrag) {
      setZoomed((z) => !z);
      setPan({ x: 0, y: 0 });
    }
  }

  return (
    <div className={styles.backdrop} role="presentation" onClick={onClose}>
      <div
        className={styles.dialog}
        role="dialog"
        aria-modal="true"
        aria-label={item.title ?? "Screenshot"}
        onClick={(e) => e.stopPropagation()}
      >
        <button type="button" className={styles.closeBtn} aria-label="Close" onClick={onClose}>
          ✕
        </button>

        {items.length > 1 && (
          <button
            type="button"
            className={`${styles.navBtn} ${styles.navPrev}`}
            aria-label="Previous screenshot"
            onClick={() => step(-1)}
          >
            ‹
          </button>
        )}

        <div className={styles.viewport}>
          <img
            className={`${styles.image} ${zoomed ? styles.imageZoomed : ""}`}
            style={{
              transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoomed ? ZOOM_SCALE : 1})`,
              transition: panning ? "none" : undefined,
            }}
            src={item.url}
            alt={item.title ?? "Screenshot"}
            onPointerDown={handlePointerDown}
            onPointerMove={handlePointerMove}
            onPointerUp={handlePointerUp}
            onPointerCancel={handlePointerUp}
            draggable={false}
          />
        </div>

        {items.length > 1 && (
          <button
            type="button"
            className={`${styles.navBtn} ${styles.navNext}`}
            aria-label="Next screenshot"
            onClick={() => step(1)}
          >
            ›
          </button>
        )}

        {(item.title || item.description || items.length > 1) && (
          <div className={styles.caption}>
            {item.title && <strong>{item.title}</strong>}
            {item.description && <p>{item.description}</p>}
            {items.length > 1 && (
              <span className={styles.counter}>
                {index + 1} / {items.length}
              </span>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
