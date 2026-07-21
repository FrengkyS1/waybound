import { useEffect, useState } from "react";
import type { ModLoader } from "../../instances/types";
import { fetchInstances } from "../../instances/api";
import type { InstanceSummary } from "../../instances/types";
import { useInstallStore } from "../../install/installStore";
import type { ModDetail } from "../detailTypes";
import type { VersionPrefill } from "../../settings/types";
import { installVerb } from "../detailTypes";
import { useEscapeKey } from "../../../hooks/useEscapeKey";
import styles from "./InstallTargetDialog.module.css";

const LOADERS: ModLoader[] = ["fabric", "forge", "neoforge", "quilt"];

interface InstallTargetDialogProps {
  detail: ModDetail;
  versionPrefill?: VersionPrefill;
  fixedInstanceId?: string;
  onClose: () => void;
  onSuccess: (message: string) => void;
}

export function InstallTargetDialog({
  detail,
  versionPrefill,
  fixedInstanceId,
  onClose,
  onSuccess,
}: InstallTargetDialogProps) {
  const isModpack = detail.summary.projectType === "modpack";
  const lockedToInstance = Boolean(fixedInstanceId);
  const suggestedMc =
    versionPrefill?.minecraftVersion ??
    detail.suggestedInstance.minecraftVersion;
  const suggestedLoader =
    versionPrefill?.loader ?? detail.suggestedInstance.loader;
  const [mode, setMode] = useState<"create" | "existing">(
    lockedToInstance || !isModpack ? "existing" : "create",
  );
  const [instances, setInstances] = useState<InstanceSummary[]>([]);
  const [existingId, setExistingId] = useState(fixedInstanceId ?? "");
  const [name, setName] = useState(detail.suggestedInstance.name);
  const [mcVersion, setMcVersion] = useState(suggestedMc);
  const [loader, setLoader] = useState<ModLoader>(suggestedLoader);
  const startInstall = useInstallStore((s) => s.startInstall);

  useEscapeKey(onClose);

  const versions =
    detail.gameVersions.length > 0 ? detail.gameVersions : [suggestedMc];

  useEffect(() => {
    void fetchInstances()
      .then((list) => {
        setInstances(list);
        if (fixedInstanceId) {
          setExistingId(fixedInstanceId);
          setMode("existing");
        } else if (list[0]) {
          setExistingId(list[0].id);
        } else {
          // No instances to pick from yet — "existing" mode would be a dead
          // end (empty dropdown, disabled submit). Fall back to creating one.
          setMode("create");
        }
      })
      .catch(() => setInstances([]));
  }, [fixedInstanceId]);

  function handleSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    // Runs in the background — the bottom-right notification tracks it while the
    // user keeps browsing. For modpacks the loader comes from the pack itself.
    startInstall(detail.summary.name, {
      modSummary: detail.summary,
      source: null,
      versionId: versionPrefill?.versionId,
      instanceId:
        mode === "existing" ? (fixedInstanceId ?? existingId) : undefined,
      createInstance:
        mode === "create"
          ? {
              name: name.trim(),
              minecraftVersion: mcVersion,
              loader: isModpack ? suggestedLoader : loader,
            }
          : undefined,
    });
    onSuccess(`Installing ${detail.summary.name}…`);
    onClose();
  }

  const canSubmit =
    (mode === "create" && name.trim().length >= 2) ||
    (mode === "existing" && (fixedInstanceId ?? existingId).length > 0);

  const selectedInstance =
    instances.find((item) => item.id === (fixedInstanceId ?? existingId)) ??
    null;

  return (
    <div className={styles.backdrop} onClick={onClose} role="presentation">
      <div
        className={styles.dialog}
        role="dialog"
        aria-modal="true"
        aria-labelledby="install-target-title"
        onClick={(e) => e.stopPropagation()}
      >
        <header className={styles.header}>
          <h2 id="install-target-title" className={styles.title}>
            {installVerb(detail.summary.projectType)}
          </h2>
          <p className={styles.subtitle}>
            {detail.summary.name}
            {versionPrefill && ` · ${versionPrefill.minecraftVersion}`}
          </p>
        </header>

        <div className={styles.modeRow}>
          {!lockedToInstance && (
            <>
              <button
                type="button"
                className={`${styles.modeBtn} ${mode === "create" ? styles.modeBtnActive : ""}`}
                onClick={() => setMode("create")}
              >
                Create new instance
              </button>
              <button
                type="button"
                className={`${styles.modeBtn} ${mode === "existing" ? styles.modeBtnActive : ""}`}
                onClick={() => setMode("existing")}
                disabled={instances.length === 0}
              >
                Use existing
              </button>
            </>
          )}
        </div>

        <form className={styles.form} onSubmit={(e) => void handleSubmit(e)}>
          {mode === "create" && !lockedToInstance ? (
            <>
              <label className={styles.field}>
                <span className={styles.label}>Instance name</span>
                <input
                  className={styles.input}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  required
                  minLength={2}
                />
              </label>
              {!versionPrefill && (
                <>
                  <fieldset className={styles.fieldset}>
                    <legend className={styles.label}>Minecraft version</legend>
                    <div className={styles.chips}>
                      {versions.slice(0, 12).map((version) => (
                        <button
                          key={version}
                          type="button"
                          className={`${styles.chip} ${version === mcVersion ? styles.chipActive : ""}`}
                          onClick={() => setMcVersion(version)}
                        >
                          {version}
                        </button>
                      ))}
                    </div>
                  </fieldset>
                  {isModpack ? (
                    <p className={styles.versionHint}>
                      Loader is set by the modpack ({suggestedLoader}).
                    </p>
                  ) : (
                    <fieldset className={styles.fieldset}>
                      <legend className={styles.label}>Mod loader</legend>
                      <div className={styles.loaderRow}>
                        {LOADERS.map((item) => (
                          <button
                            key={item}
                            type="button"
                            className={`${styles.loaderBtn} ${item === loader ? styles.loaderBtnActive : ""}`}
                            onClick={() => setLoader(item)}
                          >
                            {item}
                          </button>
                        ))}
                      </div>
                    </fieldset>
                  )}
                </>
              )}
              {versionPrefill && (
                <p className={styles.versionHint}>
                  Using version {versionPrefill.minecraftVersion}
                  {versionPrefill.loader && ` · ${versionPrefill.loader}`}
                </p>
              )}
            </>
          ) : (
            <>
              {lockedToInstance && selectedInstance ? (
                <p className={styles.versionHint}>
                  Installing into <strong>{selectedInstance.name}</strong> (
                  {selectedInstance.minecraftVersion} ·{" "}
                  {selectedInstance.loader})
                </p>
              ) : (
                <label className={styles.field}>
                  <span className={styles.label}>Instance</span>
                  <select
                    className={styles.input}
                    value={existingId}
                    onChange={(e) => setExistingId(e.target.value)}
                  >
                    {instances.map((instance) => (
                      <option key={instance.id} value={instance.id}>
                        {instance.name} ({instance.minecraftVersion} ·{" "}
                        {instance.loader})
                      </option>
                    ))}
                  </select>
                </label>
              )}
            </>
          )}

          <footer className={styles.footer}>
            <button
              type="button"
              className={styles.cancelBtn}
              onClick={onClose}
            >
              Cancel
            </button>
            <button
              type="submit"
              className={styles.submitBtn}
              disabled={!canSubmit}
            >
              {installVerb(detail.summary.projectType)}
            </button>
          </footer>
        </form>
      </div>
    </div>
  );
}
