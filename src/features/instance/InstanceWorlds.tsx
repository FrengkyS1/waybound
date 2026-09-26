import { useEffect, useState } from "react";
import {
  fetchInstanceServers,
  fetchInstanceWorlds,
  type ServerEntry,
  type WorldEntry,
} from "../instances/api";
import { ConfigEditorModal } from "./ConfigEditorModal";
import styles from "./InstancePage.module.css";

/**
 * Worlds tab: singleplayer saves with name, last played, mode and
 * version. Each world has a Files button opening its text files
 * (stats, advancements, …) in the shared config editor — level.dat and
 * friends are binary and never listed.
 */
export function WorldsTab({ instanceId }: { instanceId: string }) {
  const [worlds, setWorlds] = useState<WorldEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editorFor, setEditorFor] = useState<WorldEntry | null>(null);

  useEffect(() => {
    let cancelled = false;
    setWorlds(null);
    setError(null);
    void fetchInstanceWorlds(instanceId)
      .then((list) => {
        if (!cancelled) setWorlds(list);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [instanceId]);

  if (error) return <p className={styles.error}>{error}</p>;
  if (!worlds) return <p className={styles.note}>Loading worlds…</p>;
  if (worlds.length === 0) {
    return (
      <div className={styles.emptyState}>
        <p className={styles.emptyTitle}>No worlds yet</p>
        <p className={styles.emptyHint}>
          Singleplayer worlds you create in-game will show up here.
        </p>
      </div>
    );
  }

  return (
    <>
    <ul className={styles.modList} aria-label="Singleplayer worlds">
      {worlds.map((w) => (
        <li key={w.folderName} className={styles.modRow}>
          <div className={styles.modIconWrap}>
            {w.icon ? (
              <img src={w.icon} alt="" className={styles.modIcon} loading="lazy" />
            ) : (
              <div className={styles.modIconFallback} aria-hidden />
            )}
          </div>
          <div className={styles.modInfo}>
            <span className={styles.modNameLine}>
              <span className={styles.modName}>{w.name ?? w.folderName}</span>
            </span>
            <span className={styles.modMeta}>
              {[w.gameMode, w.gameVersion, w.lastPlayedMs ? formatLastPlayed(w.lastPlayedMs) : null]
                .filter(Boolean)
                .join(" · ") || w.folderName}
            </span>
          </div>
          <button
            type="button"
            className={styles.configBtn}
            onClick={() => setEditorFor(w)}
            title="Browse and edit this world's text files"
          >
            Files
          </button>
        </li>
      ))}
    </ul>
    {editorFor && (
      <ConfigEditorModal
        instanceId={instanceId}
        scope="world"
        worldFolder={editorFor.folderName}
        title={editorFor.name ?? editorFor.folderName}
        emptyHint="No text files in this world to edit in-app."
        onClose={() => setEditorFor(null)}
      />
    )}
    </>
  );
}

/**
 * Read-only Servers tab: saved multiplayer servers with name + address.
 * No pinging, no joining, no editing — a list.
 */
export function ServersTab({ instanceId }: { instanceId: string }) {
  const [servers, setServers] = useState<ServerEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setServers(null);
    setError(null);
    void fetchInstanceServers(instanceId)
      .then((list) => {
        if (!cancelled) setServers(list);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [instanceId]);

  if (error) return <p className={styles.error}>{error}</p>;
  if (!servers) return <p className={styles.note}>Loading servers…</p>;
  if (servers.length === 0) {
    return (
      <div className={styles.emptyState}>
        <p className={styles.emptyTitle}>No servers yet</p>
        <p className={styles.emptyHint}>
          Multiplayer servers you add in-game will show up here.
        </p>
      </div>
    );
  }

  return (
    <ul className={styles.modList} aria-label="Multiplayer servers">
      {servers.map((s) => (
        <li key={`${s.name}\u0000${s.address}`} className={styles.modRow}>
          <div className={styles.modIconWrap}>
            {s.icon ? (
              <img src={s.icon} alt="" className={styles.modIcon} loading="lazy" />
            ) : (
              <div className={styles.modIconFallback} aria-hidden />
            )}
          </div>
          <div className={styles.modInfo}>
            <span className={styles.modNameLine}>
              <span className={styles.modName}>{s.name}</span>
            </span>
            <span className={styles.modMeta}>{s.address}</span>
          </div>
        </li>
      ))}
    </ul>
  );
}

function formatLastPlayed(ms: number): string {
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}
