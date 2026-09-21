import { useEffect, useRef, useState } from "react";
import { fetchModDetails, installMod } from "../browse/api";
import {
  fetchModSummaryForContent,
  identifyModFile,
  removeContentFile,
  updateModInInstance,
  type IdentifiedMod,
} from "../instances/api";
import type { ModLoader } from "../instances/types";
import type { ModVersionSummary } from "../browse/detailTypes";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import { useModalFocus } from "../../hooks/useModalFocus";
import styles from "./ModVersionModal.module.css";

interface ModVersionModalProps {
  instanceId: string;
  minecraftVersion: string;
  loader: ModLoader;
  /** The installed jar's filename — exact-matched against each version's
   * file to highlight what's on disk right now. */
  fileName: string;
  modLabel: string;
  onClose: () => void;
  /** Surfaced to the parent so it can toast + reload the Content tab. */
  onInstalled: (message: string) => void;
}

type LoadState =
  | { stage: "loading" }
  | { stage: "error"; message: string }
  | { stage: "ready"; versions: ModVersionSummary[] };

/**
 * Per-mod version picker: lists every version the source project offers,
 * highlights the one matching the installed file, and reinstalls the mod
 * at whichever version is picked — same backend path as Update, just
 * pinned instead of latest. A version built for a different MC version or
 * loader can't be picked (installing it would drop a dead jar).
 */
export function ModVersionModal({
  instanceId,
  minecraftVersion,
  loader,
  fileName,
  modLabel,
  onClose,
  onInstalled,
}: ModVersionModalProps) {
  const [state, setState] = useState<LoadState>({ stage: "loading" });
  // Set when the file has no tracking row and was identified by content
  // hash instead — install then goes through the normal install path (plus
  // old-file cleanup) rather than the tracked update path.
  const [identified, setIdentified] = useState<IdentifiedMod | null>(null);
  const [installingId, setInstallingId] = useState<string | null>(null);
  const [installError, setInstallError] = useState<string | null>(null);
  const cancelled = useRef(false);
  const modalRef = useModalFocus();
  useEscapeKey(onClose);

  useEffect(() => {
    cancelled.current = false;
    setState({ stage: "loading" });
    setIdentified(null);
    void fetchModSummaryForContent(instanceId, fileName)
      .catch(() => identifyModFile(instanceId, fileName).then((found) => {
        // Untracked file, identified by content hash — same versions list,
        // installed through the normal path below.
        setIdentified(found);
        return found.summary;
      }))
      .then((summary) => fetchModDetails(summary))
      .then((detail) => {
        if (cancelled.current) return;
        setState({ stage: "ready", versions: detail.versions });
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
  }, [instanceId, fileName]);

  async function handleInstall(versionId: string) {
    if (installingId) return;
    setInstallError(null);
    setInstallingId(versionId);
    const installId =
      typeof crypto !== "undefined" && crypto.randomUUID
        ? crypto.randomUUID()
        : `${Date.now()}-${Math.random()}`;
    try {
      if (identified) {
        // Untracked file: install the picked version normally, then drop
        // the old jar when the new one lands under a different name (same
        // cleanup the tracked update path does backend-side).
        const result = await installMod(
          {
            modSummary: identified.summary,
            source: identified.summary.sources[0] ?? null,
            versionId,
            instanceId,
          },
          installId,
        );
        const landed = result.installed?.fileName;
        if (landed && landed !== fileName) {
          await removeContentFile(instanceId, "mod", fileName);
        }
        onInstalled(result.message);
      } else {
        const result = await updateModInInstance(instanceId, fileName, installId, versionId);
        onInstalled(result.message);
      }
      onClose();
    } catch (err) {
      setInstallError(err instanceof Error ? err.message : String(err));
    } finally {
      setInstallingId(null);
    }
  }

  return (
    <div className={styles.backdrop} onClick={onClose}>
      <div
        className={styles.dialog}
        role="dialog"
        aria-modal="true"
        aria-label={`Versions of ${modLabel}`}
        ref={modalRef}
        onClick={(e) => e.stopPropagation()}
      >
        <div className={styles.header}>
          <div>
            <h2 className={styles.title}>Versions — {modLabel}</h2>
            <p className={styles.subtitle}>
              {fileName} · {minecraftVersion} · {loader}
              {identified && ` · identified as ${identified.summary.name}`}
            </p>
          </div>
          <button
            type="button"
            className={styles.closeBtn}
            onClick={onClose}
            aria-label="Close versions"
          >
            ✕
          </button>
        </div>
        <div className={styles.body}>
          {state.stage === "loading" && <p className={styles.hint}>Loading versions…</p>}
          {state.stage === "error" && <p className={styles.error}>{state.message}</p>}
          {state.stage === "ready" && state.versions.length === 0 && (
            <p className={styles.hint}>No versions found for this mod.</p>
          )}
          {state.stage === "ready" && state.versions.length > 0 && (
            <ul className={styles.list}>
              {state.versions.map((v) => {
                // Tracked files match by installed filename; hash-identified
                // ones match by the version the bytes correspond to (the
                // file may have been renamed since).
                const installed = identified
                  ? v.id === identified.versionId
                  : v.fileName === fileName;
                const compatible =
                  v.gameVersions.includes(minecraftVersion) && v.loaders.includes(loader);
                const busy = installingId === v.id;
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
                      disabled={installed || !compatible || installingId !== null}
                      title={
                        installed
                          ? "This version is installed"
                          : !compatible
                            ? "Not built for this instance's Minecraft version and loader"
                            : `Install ${v.name}`
                      }
                      onClick={() => void handleInstall(v.id)}
                    >
                      {busy ? "Installing…" : installed ? "Installed" : "Install"}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
          {installError && (
            <p className={styles.error} role="alert">
              {installError}
            </p>
          )}
        </div>
      </div>
    </div>
  );
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
