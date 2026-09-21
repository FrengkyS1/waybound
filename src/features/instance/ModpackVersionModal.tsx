import { useEffect, useRef, useState } from "react";
import { fetchModpackDetailForInstance } from "../browse/api";
import type { ModDetail, ModVersionSummary } from "../browse/detailTypes";
import { useInstallStore } from "../install/installStore";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import { useModalFocus } from "../../hooks/useModalFocus";
// Shares the version-list presentation with ModVersionModal — same rows,
// tags, and buttons, just a different data source and install path.
import styles from "./ModVersionModal.module.css";

interface ModpackVersionModalProps {
  instanceId: string;
  minecraftVersion: string;
  /** The installed pack's version label, for highlighting the current row. */
  packLabel: string;
  onClose: () => void;
}

type LoadState =
  | { stage: "loading" }
  | { stage: "error"; message: string }
  | { stage: "ready"; detail: ModDetail };

/**
 * In-place modpack switching: lists the pack's published versions and
 * installs the picked one into this same instance (the backend reconciles
 * against the previous import — files the new version drops are removed,
 * everything else stays). Progress lands in the bottom-right dock.
 */
export function ModpackVersionModal({
  instanceId,
  minecraftVersion,
  packLabel,
  onClose,
}: ModpackVersionModalProps) {
  const [state, setState] = useState<LoadState>({ stage: "loading" });
  const [startingId, setStartingId] = useState<string | null>(null);
  const cancelled = useRef(false);
  const modalRef = useModalFocus();
  useEscapeKey(onClose);
  const startInstall = useInstallStore((s) => s.startInstall);

  useEffect(() => {
    cancelled.current = false;
    setState({ stage: "loading" });
    void fetchModpackDetailForInstance(instanceId)
      .then((detail) => {
        if (cancelled.current) return;
        setState({ stage: "ready", detail });
      })
      .catch((err) => {
        if (cancelled.current) return;
        setState({
          stage: "error",
          message: err instanceof Error ? err.message : String(err),
        });
      });
    return () => {
      cancelled.current = true;
    };
  }, [instanceId]);

  function handleInstall(detail: ModDetail, version: ModVersionSummary) {
    if (startingId) return;
    setStartingId(version.id);
    // Same background path as Browse's install dialog — the dock tracks
    // progress, and the backend reconciles with the previous import.
    startInstall(detail.summary.name, {
      modSummary: detail.summary,
      source: null,
      versionId: version.id,
      instanceId,
    });
    onClose();
  }

  const detail = state.stage === "ready" ? state.detail : null;

  return (
    <div className={styles.backdrop} onClick={onClose}>
      <div
        className={styles.dialog}
        role="dialog"
        aria-modal="true"
        aria-label="Modpack versions"
        ref={modalRef}
        onClick={(e) => e.stopPropagation()}
      >
        <div className={styles.header}>
          <div>
            <h2 className={styles.title}>
              Modpack versions{detail ? ` — ${detail.summary.name}` : ""}
            </h2>
            <p className={styles.subtitle}>
              Installed: {packLabel || "unknown"} · switches this instance in place
            </p>
          </div>
          <button
            type="button"
            className={styles.closeBtn}
            onClick={onClose}
            aria-label="Close modpack versions"
          >
            ✕
          </button>
        </div>
        <div className={styles.body}>
          {state.stage === "loading" && <p className={styles.hint}>Loading versions…</p>}
          {state.stage === "error" && <p className={styles.error}>{state.message}</p>}
          {detail && detail.versions.length === 0 && (
            <p className={styles.hint}>No versions found for this modpack.</p>
          )}
          {detail && detail.versions.length > 0 && (
            <ul className={styles.list}>
              {detail.versions.map((v) => {
                const installed = isCurrentVersion(v, packLabel);
                const compatible =
                  v.gameVersions.length === 0 || v.gameVersions.includes(minecraftVersion);
                const busy = startingId === v.id;
                return (
                  <li key={v.id} className={styles.row}>
                    <div className={styles.rowMain}>
                      <span className={styles.rowName}>
                        {v.name}
                        {installed && <span className={styles.installedTag}>Installed</span>}
                        {v.channel && (
                          <span className={styles.channelTag}>{v.channel}</span>
                        )}
                        {!compatible && !installed && (
                          <span className={styles.incompatibleTag}>Incompatible</span>
                        )}
                      </span>
                      <span className={styles.rowMeta}>
                        {formatDate(v.publishedAt)} · {formatDownloads(v.downloads)}
                        {v.gameVersions.length > 0 && ` · MC ${v.gameVersions.slice(0, 3).join(", ")}`}
                      </span>
                    </div>
                    <button
                      type="button"
                      className={styles.installBtn}
                      disabled={installed || !compatible || startingId !== null}
                      title={
                        installed
                          ? "This version is installed"
                          : !compatible
                            ? "Not built for this instance's Minecraft version"
                            : `Switch to ${v.name}`
                      }
                      onClick={() => handleInstall(detail, v)}
                    >
                      {busy ? "Starting…" : installed ? "Installed" : "Switch"}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
}

/** The recorded label is the archive filename minus its extension (or the
 * index version id), so match loosely across the version's own signals. */
function isCurrentVersion(v: ModVersionSummary, packLabel: string): boolean {
  if (!packLabel) return false;
  if (v.versionNumber === packLabel || v.name === packLabel) return true;
  const file = v.fileName?.replace(/\.(zip|mrpack)$/i, "");
  return file === packLabel;
}

function formatDate(iso: string): string {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return iso;
  return new Date(t).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

function formatDownloads(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M downloads`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}K downloads`;
  return `${n} downloads`;
}
