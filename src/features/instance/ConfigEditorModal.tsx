import { useEffect, useState } from "react";
import {
  listModConfigs,
  readConfigFile,
  writeConfigFile,
  type ConfigFileEntry,
} from "../instances/api";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import styles from "./ConfigEditorModal.module.css";

interface ConfigEditorModalProps {
  instanceId: string;
  /** The mod's jar filename — what `list_mod_configs` matches against. */
  fileName: string;
  /** Display name for the modal title (the mod's resolved name, falling
   * back to a humanized filename upstream). */
  modLabel: string;
  onClose: () => void;
}

type LoadState =
  | { stage: "loading-list" }
  | { stage: "no-configs" }
  | { stage: "list-error"; message: string }
  | { stage: "ready"; configs: ConfigFileEntry[] };

export function ConfigEditorModal({
  instanceId,
  fileName,
  modLabel,
  onClose,
}: ConfigEditorModalProps) {
  const [state, setState] = useState<LoadState>({ stage: "loading-list" });
  const [selected, setSelected] = useState<ConfigFileEntry | null>(null);
  const [content, setContent] = useState("");
  const [savedContent, setSavedContent] = useState("");
  const [loadingFile, setLoadingFile] = useState(false);
  const [fileError, setFileError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  // Set instead of firing onClose/openFile directly, so a discard
  // confirmation can gate either "close the whole modal" or "switch to a
  // different file" through the same dialog.
  const [pendingAction, setPendingAction] = useState<(() => void) | null>(null);

  useEffect(() => {
    let cancelled = false;
    void listModConfigs(instanceId, fileName)
      .then((configs) => {
        if (cancelled) return;
        if (configs.length === 0) {
          setState({ stage: "no-configs" });
        } else {
          setState({ stage: "ready", configs });
          if (configs.length === 1) void openFile(configs[0]);
        }
      })
      .catch((err) => {
        if (cancelled) return;
        setState({
          stage: "list-error",
          message: err instanceof Error ? err.message : String(err),
        });
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instanceId, fileName]);

  async function openFile(entry: ConfigFileEntry) {
    setFileError(null);
    setLoadingFile(true);
    setSelected(entry);
    try {
      const text = await readConfigFile(instanceId, entry.relativePath);
      setContent(text);
      setSavedContent(text);
    } catch (err) {
      setFileError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoadingFile(false);
    }
  }

  const dirty = content !== savedContent;

  function guarded(action: () => void) {
    if (dirty) {
      setPendingAction(() => action);
    } else {
      action();
    }
  }

  async function handleSave() {
    if (!selected) return;
    setSaving(true);
    setFileError(null);
    try {
      await writeConfigFile(instanceId, selected.relativePath, content);
      setSavedContent(content);
    } catch (err) {
      setFileError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  useEscapeKey(() => guarded(onClose), !pendingAction);

  const showPicker = state.stage === "ready" && state.configs.length > 1;

  return (
    <div className={styles.backdrop} role="presentation" onClick={() => guarded(onClose)}>
      <div
        className={styles.dialog}
        role="dialog"
        aria-modal="true"
        aria-label={`Edit config for ${modLabel}`}
        onClick={(e) => e.stopPropagation()}
      >
        <header className={styles.header}>
          <div>
            <h2 className={styles.title}>{modLabel}</h2>
            {selected && <p className={styles.subtitle}>{selected.displayName}</p>}
          </div>
          <button
            type="button"
            className={styles.closeBtn}
            aria-label="Close"
            onClick={() => guarded(onClose)}
          >
            ✕
          </button>
        </header>

        <div className={styles.body}>
          {showPicker && state.stage === "ready" && (
            <ul className={styles.fileList} aria-label="Config files">
              {state.configs.map((entry) => (
                <li key={entry.relativePath}>
                  <button
                    type="button"
                    className={`${styles.fileItem} ${
                      selected?.relativePath === entry.relativePath ? styles.fileItemActive : ""
                    }`}
                    onClick={() => guarded(() => void openFile(entry))}
                  >
                    {entry.displayName}
                  </button>
                </li>
              ))}
            </ul>
          )}

          <div className={styles.editorPane}>
            {state.stage === "loading-list" && <p className={styles.hint}>Loading…</p>}
            {state.stage === "no-configs" && (
              <p className={styles.hint}>No config files found for this mod.</p>
            )}
            {state.stage === "list-error" && <p className={styles.error}>{state.message}</p>}
            {state.stage === "ready" && !selected && !loadingFile && (
              <p className={styles.hint}>Pick a file to edit.</p>
            )}
            {loadingFile && <p className={styles.hint}>Loading…</p>}
            {fileError && <p className={styles.error}>{fileError}</p>}
            {selected && !loadingFile && !fileError && (
              <>
                <textarea
                  className={styles.textarea}
                  value={content}
                  onChange={(e) => setContent(e.target.value)}
                  spellCheck={false}
                  aria-label={`Editing ${selected.displayName}`}
                />
                <div className={styles.footer}>
                  <span className={styles.dirtyHint}>{dirty ? "Unsaved changes" : "Saved"}</span>
                  <button
                    type="button"
                    className={styles.saveBtn}
                    disabled={!dirty || saving}
                    onClick={() => void handleSave()}
                  >
                    {saving ? "Saving…" : "Save"}
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      </div>

      {pendingAction && (
        <ConfirmDialog
          title="Discard unsaved changes?"
          message="This file has changes you haven't saved yet. Discard them?"
          confirmLabel="Discard"
          danger
          onConfirm={() => {
            const action = pendingAction;
            setPendingAction(null);
            action();
          }}
          onCancel={() => setPendingAction(null)}
        />
      )}
    </div>
  );
}
