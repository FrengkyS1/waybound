import { useCallback, useEffect, useRef, useState } from "react";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import { useModalFocus } from "../../hooks/useModalFocus";
import { detectImportableLaunchers, exportInstance, importInstance } from "../instances/api";
import type { DetectedLauncher, DetectedLauncherInstance, InstanceSummary, ModLoader } from "../instances/types";
import { useInstallStore } from "../install/installStore";
import styles from "./CreateInstanceDialog.module.css";
import transferStyles from "./TransferDialog.module.css";

const LOADER_LABEL: Record<ModLoader, string> = {
  fabric: "Fabric",
  forge: "Forge",
  neoforge: "NeoForge",
  quilt: "Quilt",
  vanilla: "Vanilla",
};

export function TransferDialog({ instance, onClose, onImported }: {
  instance?: InstanceSummary;
  onClose: () => void;
  onImported: (instance: InstanceSummary) => void;
}) {
  const [path, setPath] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<string | null>(null);
  const [launchers, setLaunchers] = useState<DetectedLauncher[]>([]);
  const [rootPath, setRootPath] = useState("");
  const [scanning, setScanning] = useState(!instance);
  const [scanError, setScanError] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const scanRequest = useRef(0);
  const lastScanRoot = useRef<string | undefined>(undefined);
  const isExport = !!instance;
  const sourcePath = selectedPath ?? path.trim();
  const modalRef = useModalFocus();
  const entry = useInstallStore((state) => state.installs.find((item) => item.id === `import:${sourcePath}`));
  const setPaused = useInstallStore((state) => state.setPaused);
  const cancel = useInstallStore((state) => state.cancel);
  const scan = useCallback(async (root?: string) => {
    const request = ++scanRequest.current;
    lastScanRoot.current = root;
    setScanning(true);
    setScanError(null);
    setLaunchers([]);
    try {
      const detected = await detectImportableLaunchers(root);
      if (request === scanRequest.current) setLaunchers(detected);
    } catch (cause) {
      if (request === scanRequest.current) {
        setScanError(cause instanceof Error ? cause.message : String(cause));
      }
    } finally {
      if (request === scanRequest.current) setScanning(false);
    }
  }, []);

  useEffect(() => {
    if (!isExport) void scan();
    return () => { scanRequest.current += 1; };
  }, [isExport, scan]);

  function close() {
    scanRequest.current += 1;
    onClose();
  }

  function selectInstance(detected: DetectedLauncherInstance) {
    setSelectedPath(detected.path);
    setPath(detected.path);
    setName(detected.name);
    setError(null);
  }

  useEscapeKey(close, !busy);

  async function submit() {
    if (busy || !sourcePath) return;
    setBusy(true);
    setError(null);
    const id = `import:${sourcePath}`;
    if (!instance) useInstallStore.setState((s) => ({ installs: [
      ...s.installs.filter((entry) => entry.id !== id),
      { id, name: name.trim() || "Import instance", status: "installing" },
    ] }));
    try {
      if (instance) {
        setResult(await exportInstance(instance.id, sourcePath));
      } else {
        const imported = await importInstance(sourcePath, name.trim() || undefined);
        useInstallStore.setState((s) => ({
          installs: s.installs.map((entry) => entry.id === id ? { ...entry, status: "done", message: `Imported ${imported.name}`, instanceId: imported.id } : entry),
          refreshTick: s.refreshTick + 1,
        }));
        onImported(imported);
        close();
      }
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      if (!instance) useInstallStore.setState((s) => ({ installs: s.installs.map((entry) => entry.id === id
        ? { ...entry, status: message === "Install cancelled" ? "cancelled" : "error", error: message } : entry) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className={styles.backdrop} role="presentation" onClick={busy ? undefined : close}>
      <div ref={modalRef} tabIndex={-1} className={`${styles.dialog} ${instance ? "" : transferStyles.importDialog}`} role="dialog" aria-modal="true"
        aria-labelledby="transfer-title" onClick={(event) => event.stopPropagation()}>
        <header className={styles.header}>
          <h2 id="transfer-title" className={styles.title}>{instance ? `Export ${instance.name}` : "Import instance"}</h2>
          <p className={styles.subtitle}>{instance
            ? "Export a Modrinth .mrpack. Worlds and account secrets are excluded. Files without redistribution permission may prevent export."
            : "Import a Modrinth .mrpack, CurseForge / FTB App / Technic ZIP, Prism/MultiMC directory or ZIP, ATLauncher / FTB App / Technic / legacy GDLauncher directory, or CurseForge App directory with a manifest. The source is copied, never moved or changed. Launcher-directory worlds are copied too; keep your original backup."}</p>
        </header>
        <form className={styles.form} onSubmit={(event) => { event.preventDefault(); void submit(); }}>
          {!instance && <section className={transferStyles.detection} aria-labelledby="detected-title">
            <div className={transferStyles.detectionHeader}>
              <h3 id="detected-title" className={transferStyles.sectionTitle}>Instances from other launchers</h3>
              <button type="button" className={styles.cancelBtn} disabled={busy} onClick={() => void scan(lastScanRoot.current)}>Refresh</button>
            </div>
            <div className={transferStyles.scanRow}>
              <label className={`${styles.field} ${transferStyles.rootField}`}>
                <span className={styles.label}>Launcher root or instances directory (optional)</span>
                <input className={styles.input} value={rootPath} disabled={busy}
                  placeholder="Blank uses default launcher locations"
                  onChange={(event) => setRootPath(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      event.preventDefault();
                      if (!busy) void scan(rootPath.trim() || undefined);
                    }
                  }} />
              </label>
              <button type="button" className={styles.cancelBtn} disabled={busy} onClick={() => void scan(rootPath.trim() || undefined)}>Scan</button>
            </div>
            {scanning && <p className={styles.hint} role="status">Scanning launcher locations… Manual import is still available below.</p>}
            {scanError && <p className={styles.hint} role="alert">Could not scan launcher locations: {scanError}. Enter a source path below, or scan another launcher directory.</p>}
            {!scanning && !scanError && !launchers.some((launcher) => launcher.instances.length > 0) && <p className={styles.hint} role="status">No supported instances found. Scan a custom launcher directory or enter an archive or instance path below.</p>}
            {launchers.some((launcher) => launcher.instances.length > 0) && <fieldset className={transferStyles.instanceList} disabled={busy}>
              <legend className={transferStyles.selectionHint}>Choose one instance to copy</legend>
              {launchers.filter((launcher) => launcher.instances.length > 0).map((launcher, launcherIndex) => <section className={transferStyles.launcherGroup} key={`${launcher.name}:${launcher.root}`} aria-labelledby={`launcher-${launcherIndex}`}>
                <h4 className={transferStyles.launcherName} id={`launcher-${launcherIndex}`}>{launcher.name}</h4>
                <p className={transferStyles.sourcePath}>{launcher.root}</p>
                {launcher.instances.map((detected) => <label className={`${transferStyles.instanceRow} ${selectedPath === detected.path ? transferStyles.selectedRow : ""}`} key={detected.path}>
                  <input type="radio" name="detected-instance" value={detected.path} checked={selectedPath === detected.path} onChange={() => selectInstance(detected)} />
                  <span className={transferStyles.instanceDetails}>
                    <span className={transferStyles.instanceName}>{detected.name}</span>
                    <span className={transferStyles.instanceMeta}>Minecraft {detected.minecraft} · {LOADER_LABEL[detected.loader]}{detected.loaderVersion ? ` ${detected.loaderVersion}` : ""}</span>
                    <span className={transferStyles.sourcePath}>{detected.path}</span>
                  </span>
                </label>)}
              </section>)}
            </fieldset>}
          </section>}
          <label className={styles.field}>
            <span className={styles.label}>{instance ? "Destination .mrpack file path" : "Source archive or instance directory path"}</span>
            <input className={styles.input} value={path} onChange={(event) => { setPath(event.target.value); setSelectedPath(null); }} required disabled={busy || !!result} />
          </label>
          {!instance && <label className={styles.field}>
            <span className={styles.label}>Instance name (optional)</span>
            <input className={styles.input} value={name} onChange={(event) => setName(event.target.value)} maxLength={100} disabled={busy} />
          </label>}
          {error && <p className={styles.hint} role="alert">{error}</p>}
          {busy && <p className={styles.hint} role="status">{instance ? "Exporting…" : entry?.paused ? "Import paused" : "Importing… Downloads may need internet."}</p>}
          {busy && !instance && <div>
            {entry?.total !== undefined && <p className={styles.hint}>{entry.current ?? 0}/{entry.total} files</p>}
            <button type="button" className={styles.cancelBtn} disabled={entry?.controlPending} onClick={() => setPaused(`import:${sourcePath}`, !entry?.paused)}>{entry?.paused ? "Resume import" : "Pause import"}</button>
            <button type="button" className={styles.cancelBtn} onClick={() => cancel(`import:${sourcePath}`)}>Cancel import</button>
            {entry?.controlError && <p role="alert" className={styles.hint}>{entry.controlError}</p>}
          </div>}
          {result && <p className={styles.hint} role="status">Export saved to {result}</p>}
          <footer className={styles.footer}>
            <button type="button" className={styles.cancelBtn} onClick={close} disabled={busy}>{result ? "Done" : "Close"}</button>
            {!result && <button type="submit" className={styles.submitBtn} disabled={busy || !sourcePath}>{busy ? "Working…" : instance ? "Export" : "Import"}</button>}
          </footer>
        </form>
      </div>
    </div>
  );
}
