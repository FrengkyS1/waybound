import { useEffect, useRef, useState } from "react";
import {
  listModConfigs,
  listWorldFiles,
  readConfigFile,
  readWorldFile,
  writeConfigFile,
  writeWorldFile,
  type ConfigFileEntry,
} from "../instances/api";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import { useModalFocus } from "../../hooks/useModalFocus";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import styles from "./ConfigEditorModal.module.css";

type ConfigEditorModalProps = {
  instanceId: string;
  /** Dialog title (mod display name or world name). */
  title: string;
  /** Shown when the scope lists zero files. */
  emptyHint: string;
  onClose: () => void;
} & (
  /** Per-mod config/ files matched by jar filename. */
  | { scope: "mod"; fileName: string; worldFolder?: never }
  /** Text files inside one world folder. */
  | { scope: "world"; worldFolder: string; fileName?: never }
);

type LoadState =
  | { stage: "loading-list" }
  | { stage: "no-configs" }
  | { stage: "list-error"; message: string }
  | { stage: "ready"; configs: ConfigFileEntry[] };

export function ConfigEditorModal({
  instanceId,
  title,
  emptyHint,
  onClose,
  ...scope
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
  const request = useRef(0);
  const saveInFlight = useRef(false);
  const modalRef = useModalFocus();

  // Stable primitive key for the effect below — the scope object itself
  // would be a fresh identity every render.
  const scopeKey =
    scope.scope === "mod" ? `mod:${scope.fileName}` : `world:${scope.worldFolder}`;
  useEffect(() => {
    let cancelled = false;
    setState({ stage: "loading-list" });
    setSelected(null);
    setContent("");
    setSavedContent("");
    setFileError(null);
    request.current += 1;
    void (scope.scope === "mod"
      ? listModConfigs(instanceId, scope.fileName)
      : listWorldFiles(instanceId, scope.worldFolder)
    )
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
      request.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instanceId, scopeKey]);

  async function openFile(entry: ConfigFileEntry) {
    if (saveInFlight.current) return;
    const generation = ++request.current;
    setContent("");
    setSavedContent("");
    setFileError(null);
    setLoadingFile(true);
    setSelected(entry);
    try {
      const text =
        scope.scope === "mod"
          ? await readConfigFile(instanceId, entry.relativePath)
          : await readWorldFile(instanceId, scope.worldFolder, entry.relativePath);
      if (generation !== request.current) return;
      setContent(text);
      setSavedContent(text);
    } catch (err) {
      if (generation !== request.current) return;
      setFileError(err instanceof Error ? err.message : String(err));
    } finally {
      if (generation === request.current) setLoadingFile(false);
    }
  }

  const dirty = content !== savedContent;

  function guarded(action: () => void) {
    if (saveInFlight.current) return;
    if (dirty) {
      setPendingAction(() => action);
    } else {
      action();
    }
  }

  async function handleSave() {
    if (!selected || loadingFile || saveInFlight.current) return;
    saveInFlight.current = true;
    const generation = request.current;
    const saved = content;
    setSaving(true);
    setFileError(null);
    try {
      if (scope.scope === "mod") {
        await writeConfigFile(instanceId, selected.relativePath, saved);
      } else {
        await writeWorldFile(instanceId, scope.worldFolder, selected.relativePath, saved);
      }
      if (generation === request.current) setSavedContent(saved);
    } catch (err) {
      if (generation === request.current) setFileError(err instanceof Error ? err.message : String(err));
    } finally {
      saveInFlight.current = false;
      if (generation === request.current) setSaving(false);
    }
  }

  useEscapeKey(() => guarded(onClose), !pendingAction && !saving);

  const showPicker = state.stage === "ready" && state.configs.length > 1;

  return (
    <div className={styles.backdrop} role="presentation" onClick={() => guarded(onClose)}>
      <div
        className={styles.dialog}
        ref={modalRef}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-label={`Edit files for ${title}`}
        onClick={(e) => e.stopPropagation()}
      >
        <header className={styles.header}>
          <div>
            <h2 className={styles.title}>{title}</h2>
            {selected && <p className={styles.subtitle}>{selected.displayName}</p>}
          </div>
          <button
            type="button"
            className={styles.closeBtn}
            disabled={saving}
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
                    disabled={saving}
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
              <p className={styles.hint}>{emptyHint}</p>
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
                  disabled={saving}
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
