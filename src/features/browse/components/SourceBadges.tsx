import type { ModSource } from "../types";
import styles from "./SourceBadges.module.css";

const LABELS: Record<ModSource, string> = {
  modrinth: "Modrinth",
  curseforge: "CurseForge",
};

interface SourceBadgesProps {
  sources: ModSource[];
}

export function SourceBadges({ sources }: SourceBadgesProps) {
  return (
    <div className={styles.wrap}>
      {sources.map((source) => (
        <span
          key={source}
          className={`${styles.badge} ${source === "curseforge" ? styles.curseforge : styles.modrinth}`}
          title={`Available on ${LABELS[source]}`}
        >
          {LABELS[source]}
        </span>
      ))}
    </div>
  );
}
