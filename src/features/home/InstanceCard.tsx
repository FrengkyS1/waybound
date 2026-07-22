import { useState } from "react";
import type { InstanceSummary } from "../instances/types";
import { usePlayStore } from "../play/store";
import styles from "./InstanceCard.module.css";

const LAUNCHABLE = new Set(["vanilla", "fabric", "forge", "neoforge"]);

const LOADER_LABEL: Record<string, string> = {
  fabric: "Fabric",
  forge: "Forge",
  neoforge: "NeoForge",
  quilt: "Quilt",
  vanilla: "Vanilla",
};

interface InstanceCardProps {
  instance: InstanceSummary;
  onOpen: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  renaming?: boolean;
  onRenameCommit?: (name: string) => void;
  onRenameCancel?: () => void;
}

export function InstanceCard({
  instance,
  onOpen,
  onContextMenu,
  renaming = false,
  onRenameCommit,
  onRenameCancel,
}: InstanceCardProps) {
  const initial = instance.name.trim().charAt(0).toUpperCase() || "?";
  const loaderClass =
    styles[`loader_${instance.loader}`] ?? styles.loader_fabric;
  const hasCoverArt = Boolean(instance.icon);
  const [nameDraft, setNameDraft] = useState(instance.name);
  const account = usePlayStore((s) => s.account);
  const play = usePlayStore((s) => s.play);
  const launch = usePlayStore((s) => s.launches[instance.id]);
  const launchBusy =
    !!launch && (launch.phase === "preparing" || launch.phase === "running");
  const canQuickPlay = LAUNCHABLE.has(instance.loader) && !renaming;

  function handleQuickPlay(e: React.MouseEvent) {
    e.stopPropagation();
    // No account yet: open the instance page, whose Play button runs sign-in.
    if (!account) {
      onOpen();
      return;
    }
    void play(instance.id, instance.name);
  }

  function commitRename() {
    const next = nameDraft.trim();
    if (next.length >= 2 && next !== instance.name) {
      onRenameCommit?.(next);
    } else {
      onRenameCancel?.();
    }
  }

  return (
    <div
      className={styles.card}
      role="button"
      tabIndex={0}
      onClick={renaming ? undefined : onOpen}
      onKeyDown={(e) => {
        if (renaming) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen();
        }
      }}
      onContextMenu={onContextMenu}
    >
      <div className={`${styles.cover} ${hasCoverArt ? "" : loaderClass}`}>
        {instance.icon ? (
          <img
            className={styles.coverImg}
            src={instance.icon}
            alt=""
            aria-hidden
          />
        ) : (
          <span className={styles.initial} aria-hidden>
            {initial}
          </span>
        )}
        <div className={styles.scrim} aria-hidden />
        <div className={styles.badges}>
          <span className={styles.versionBadge}>
            {instance.minecraftVersion}
          </span>
          <span className={styles.loaderBadge}>
            {LOADER_LABEL[instance.loader] ?? instance.loader}
          </span>
        </div>
        {canQuickPlay && (
          <button
            type="button"
            className={styles.quickPlay}
            onClick={handleQuickPlay}
            disabled={launchBusy}
            aria-label={`Play ${instance.name}`}
            title={launchBusy ? "Already running" : "Play"}
          >
            <span aria-hidden>▶</span>
          </button>
        )}
      </div>
      <div className={styles.meta}>
        {renaming ? (
          <input
            className={styles.nameInput}
            value={nameDraft}
            autoFocus
            maxLength={100}
            aria-label="Instance name"
            onClick={(e) => e.stopPropagation()}
            onChange={(e) => setNameDraft(e.target.value)}
            onBlur={commitRename}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") commitRename();
              if (e.key === "Escape") onRenameCancel?.();
            }}
          />
        ) : (
          <span className={styles.name}>{instance.name}</span>
        )}
        <span className={styles.sub}>
          {instance.modCount} mod{instance.modCount === 1 ? "" : "s"}
        </span>
      </div>
    </div>
  );
}
