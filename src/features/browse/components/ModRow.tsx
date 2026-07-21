import type { ModSummary } from "../types";
import { SourceBadges } from "./SourceBadges";
import styles from "./ModRow.module.css";

function formatDownloads(count: number): string {
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}K`;
  return count.toString();
}

interface ModRowProps {
  mod: ModSummary;
  onOpen: (mod: ModSummary) => void;
}

export function ModRow({ mod, onOpen }: ModRowProps) {
  return (
    <article
      className={styles.row}
      role="button"
      tabIndex={0}
      onClick={() => onOpen(mod)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen(mod);
        }
      }}
    >
      <div className={styles.iconWrap}>
        {mod.iconUrl ? (
          <img
            src={mod.iconUrl}
            alt=""
            className={styles.icon}
            loading="lazy"
          />
        ) : (
          <div className={styles.iconFallback} aria-hidden />
        )}
      </div>

      <div className={styles.body}>
        <header className={styles.header}>
          <h3 className={styles.name}>{mod.name}</h3>
          <SourceBadges sources={mod.sources} />
        </header>
        {mod.description && (
          <p className={styles.description}>{mod.description}</p>
        )}
        <footer className={styles.meta}>
          <span>{mod.author}</span>
          <span className={styles.dot} aria-hidden>
            ·
          </span>
          <span className={styles.mono}>
            {formatDownloads(mod.downloads)} downloads
          </span>
          {mod.loaders.length > 0 && (
            <>
              <span className={styles.dot} aria-hidden>
                ·
              </span>
              <span>{mod.loaders.join(", ")}</span>
            </>
          )}
        </footer>
      </div>

      <span className={styles.chevron} aria-hidden>
        ›
      </span>
    </article>
  );
}
