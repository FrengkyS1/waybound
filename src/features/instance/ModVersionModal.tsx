import { useEffect, useRef, useState } from "react";
import { fetchModDetails } from "../browse/api";
import {
  fetchModSummaryForContent,
  identifyModFile,
  updateModInInstance,
  type IdentifiedMod,
} from "../instances/api";
import type { ModLoader } from "../instances/types";
import type { ModVersionSummary } from "../browse/detailTypes";
import { useInstallStore } from "../install/installStore";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import { useModalFocus } from "../../hooks/useModalFocus";
import styles from "./ModVersionModal.module.css";

interface ModVersionModalProps {
  instanceId: string;
  minecraftVersion: string;
  loader: ModLoader;
  /** Exact physical filename, including `.disabled`; upstream version
   * comparisons use the unsuffixed logical filename. */
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
  // Hash identification records the exact file before version updates use
  // the same atomic tracked path, including disabled-state preservation.
  const [identified, setIdentified] = useState<IdentifiedMod | null>(null);
  const [installingId, setInstallingId] = useState<string | null>(null);
  const [installError, setInstallError] = useState<string | null>(null);
  const generation = useRef(0);
  const runInstall = useInstallStore((s) => s.runInstall);
  const modalRef = useModalFocus();
  useEscapeKey(onClose);

  useEffect(() => {
    let active = true;
    generation.current++;
    setState({ stage: "loading" });
    setIdentified(null);
    setInstallingId(null);
    setInstallError(null);
    void fetchModSummaryForContent(instanceId, fileName)
      .catch((err) => {
        if (!active) throw err;
        return identifyModFile(instanceId, fileName).then((found) => {
        // Untracked file, identified by content hash — same versions list,
        // installed through the normal path below.
        if (active) setIdentified(found);
        return found.summary;
        });
      })
      .then((summary) => {
        if (!active) throw new Error("Versions request superseded");
        return fetchModDetails(summary);
      })
      .then((detail) => {
        if (!active) return;
        setState({ stage: "ready", versions: detail.versions });
      })
      .catch((err) => {
        if (!active) return;
        setState({
          stage: "error",
          message: err instanceof Error ? err.message : String(err),
        });
      });
    return () => {
      active = false;
      generation.current++;
    };
  }, [instanceId, fileName]);

  async function handleInstall(versionId: string) {
    if (installingId) return;
    setInstallError(null);
    setInstallingId(versionId);
    const request = generation.current;
    const result = await runInstall(modLabel,
      (installId) => updateModInInstance(instanceId, fileName, installId, versionId), instanceId);
    if (request !== generation.current) return;
    setInstallingId(null);
    if (result) {
      onInstalled(result.message);
      onClose();
    } else {
      setInstallError("Install did not finish. See the background notification for details, then retry.");
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
                  : v.fileName === fileName.replace(/\.disabled$/, "");
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
