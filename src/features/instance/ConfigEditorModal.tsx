import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import {
  listInstanceConfigs,
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
  title: string;
  emptyHint: string;
  onClose: () => void;
} & (
  | { scope: "mod"; fileName: string; worldFolder?: never }
  | { scope: "world"; worldFolder: string; fileName?: never }
  | { scope: "instance"; fileName?: never; worldFolder?: never }
);

type LoadState =
  | { stage: "loading-list" }
  | { stage: "no-configs" }
  | { stage: "list-error"; message: string }
  | { stage: "ready"; configs: ConfigFileEntry[] };

const errorMessage = (error: unknown) => error instanceof Error ? error.message : String(error);

export function ConfigEditorModal({ instanceId, title, emptyHint, onClose, ...scope }: ConfigEditorModalProps) {
  const [state, setState] = useState<LoadState>({ stage: "loading-list" });
  const [selected, setSelected] = useState<ConfigFileEntry | null>(null);
  const [content, setContent] = useState("");
  const [savedContent, setSavedContent] = useState("");
  const [loadingFile, setLoadingFile] = useState(false);
  const [loadedFile, setLoadedFile] = useState(false);
  const [recoveryDraft, setRecoveryDraft] = useState<string | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [didSave, setDidSave] = useState(false);
  const [fileFilter, setFileFilter] = useState("");
  const [findOpen, setFindOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [matchCase, setMatchCase] = useState(false);
  const [matchIndex, setMatchIndex] = useState(0);
  const [cursor, setCursor] = useState(0);
  const [pendingAction, setPendingAction] = useState<(() => void) | null>(null);
  const [pendingReload, setPendingReload] = useState(false);
  const [tabIndents, setTabIndents] = useState(true);
  const request = useRef(0);
  const saveInFlight = useRef(false);
  const resetFindSelection = useRef(false);
  const editorRef = useRef<HTMLTextAreaElement>(null);
  const findRef = useRef<HTMLInputElement>(null);
  const modalRef = useModalFocus();
  const dirty = content !== savedContent;
  const scopeKey = scope.scope === "mod" ? `mod:${scope.fileName}` : scope.scope === "world" ? `world:${scope.worldFolder}` : "instance";

  useEffect(() => {
    if (dirty) setDidSave(false);
  }, [dirty]);

  useEffect(() => {
    let cancelled = false;
    setState({ stage: "loading-list" });
    setSelected(null);
    setContent("");
    setSavedContent("");
    setLoadedFile(false);
    setRecoveryDraft(null);
    setLoadingFile(false);
    setSaving(false);
    setReadError(null);
    setSaveError(null);
    setDidSave(false);
    setFileFilter("");
    request.current += 1;
    void (scope.scope === "mod" ? listModConfigs(instanceId, scope.fileName)
      : scope.scope === "world" ? listWorldFiles(instanceId, scope.worldFolder)
        : listInstanceConfigs(instanceId))
      .then((configs) => {
        if (cancelled) return;
        setState(configs.length ? { stage: "ready", configs } : { stage: "no-configs" });
        if (configs.length === 1) void openFile(configs[0]);
      })
      .catch((error) => {
        if (!cancelled) setState({ stage: "list-error", message: errorMessage(error) });
      });
    return () => { cancelled = true; request.current += 1; };
    // Scope primitives intentionally identify each file-list request.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instanceId, scopeKey]);

  useEffect(() => {
    if (findOpen) { findRef.current?.focus(); findRef.current?.select(); }
  }, [findOpen]);

  const matches = useMemo(() => {
    if (!query) return [];
    const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    return Array.from(content.matchAll(new RegExp(escaped, matchCase ? "g" : "gi")), (match) => match.index);
  }, [content, query, matchCase]);
  const activeMatch = matches.length ? Math.min(matchIndex, matches.length - 1) : 0;
  const beforeCursor = content.slice(0, cursor);
  const line = beforeCursor.split("\n").length;
  const column = cursor - beforeCursor.lastIndexOf("\n");

  async function openFile(entry: ConfigFileEntry, reload = false) {
    if (saveInFlight.current) return;
    const generation = ++request.current;
    const previousDraft = reload && dirty ? content : null;
    if (!reload) {
      setContent("");
      setSavedContent("");
      setLoadedFile(false);
      setRecoveryDraft(null);
      setSaveError(null);
      setDidSave(false);
      setSelected(entry);
      setCursor(0);
      setMatchIndex(0);
    }
    setReadError(null);
    setLoadingFile(true);
    try {
      const text = scope.scope === "world"
        ? await readWorldFile(instanceId, scope.worldFolder, entry.relativePath)
        : await readConfigFile(instanceId, entry.relativePath);
      if (generation !== request.current) return;
      setContent(text);
      setSavedContent(text);
      setLoadedFile(true);
      setSaveError(null);
      setDidSave(false);
      setCursor(0);
      setMatchIndex(0);
      if (previousDraft !== null) setRecoveryDraft(previousDraft);
    } catch (error) {
      if (generation === request.current) setReadError(errorMessage(error));
    } finally {
      if (generation === request.current) setLoadingFile(false);
    }
  }

  function guarded(action: () => void, reload = false) {
    if (saveInFlight.current) return;
    if (dirty) {
      setPendingReload(reload);
      setPendingAction(() => action);
    } else action();
  }

  async function handleSave() {
    if (!selected || !dirty || loadingFile || !loadedFile || saveInFlight.current) return;
    saveInFlight.current = true;
    const generation = request.current;
    const draft = content;
    const expected = savedContent;
    setSaving(true);
    setSaveError(null);
    setDidSave(false);
    try {
      if (scope.scope === "world") {
        await writeWorldFile(instanceId, scope.worldFolder, selected.relativePath, draft, expected);
      } else {
        await writeConfigFile(instanceId, selected.relativePath, draft, expected);
      }
      if (generation === request.current) { setSavedContent(draft); setDidSave(true); }
    } catch (error) {
      if (generation === request.current) {
        setDidSave(false);
        setSaveError(errorMessage(error));
      }
    } finally {
      saveInFlight.current = false;
      if (generation === request.current) setSaving(false);
    }
  }

  function showFind() {
    setFindOpen(true);
    findRef.current?.focus();
    findRef.current?.select();
  }

  function selectMatch(index: number, focusEditor = true) {
    if (!matches.length) return;
    const next = (index + matches.length) % matches.length;
    setMatchIndex(next);
    const start = matches[next];
    const editor = editorRef.current;
    if (!editor) return;
    if (focusEditor) editor.focus();
    editor.setSelectionRange(start, start + query.length);
    const lineHeight = Number.parseFloat(getComputedStyle(editor).lineHeight) || 20;
    const matchLine = content.slice(0, start).split("\n").length - 1;
    editor.scrollTop = Math.max(0, matchLine * lineHeight - editor.clientHeight / 2);
    setCursor(start);
  }

  useEffect(() => {
    if (!resetFindSelection.current) return;
    resetFindSelection.current = false;
    selectMatch(0, false);
    // Only explicit query/case changes request selection; editing must keep its caret.
  }, [matches, query, matchCase]);

  function handleKeys(event: KeyboardEvent<HTMLDivElement>) {
    if (pendingAction) return;
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      void handleSave();
    } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "f" && selected && loadedFile) {
      event.preventDefault();
      showFind();
    } else if (event.key === "F3" && findOpen) {
      event.preventDefault();
      selectMatch(activeMatch + (event.shiftKey ? -1 : 1));
    }
  }

  function indent(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (saving || loadingFile) return;
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "m") {
      event.preventDefault();
      event.stopPropagation();
      setTabIndents(!tabIndents);
      return;
    }
    if (!tabIndents || event.key !== "Tab" || event.ctrlKey || event.metaKey || event.altKey) return;
    event.preventDefault();
    event.stopPropagation();
    const editor = event.currentTarget;
    const start = editor.selectionStart;
    const end = editor.selectionEnd;
    let next: string;
    let nextStart: number;
    let nextEnd: number;
    if (start === end && !event.shiftKey) {
      next = content.slice(0, start) + "  " + content.slice(end);
      nextStart = nextEnd = start + 2;
    } else {
      const blockStart = start === 0 ? 0 : content.lastIndexOf("\n", start - 1) + 1;
      const lastPosition = end > start && content[end - 1] === "\n" ? end - 1 : end;
      const nextNewline = content.indexOf("\n", lastPosition);
      const blockEnd = nextNewline === -1 ? content.length : nextNewline;
      const block = content.slice(blockStart, blockEnd);
      let removed = 0;
      let firstDelta = 0;
      const lines = block.split("\n");
      const indented = lines.map((text, index) => {
        const delta = event.shiftKey ? -(text.match(/^(?:\t| {1,2})/)?.[0].length ?? 0) : 2;
        if (index === 0) firstDelta = delta;
        removed += delta;
        return event.shiftKey ? text.slice(-delta) : "  " + text;
      }).join("\n");
      next = content.slice(0, blockStart) + indented + content.slice(blockEnd);
      nextStart = Math.max(blockStart, start + firstDelta);
      nextEnd = Math.max(nextStart, end + removed);
    }
    setContent(next);
    setCursor(nextStart);
    requestAnimationFrame(() => editor.setSelectionRange(nextStart, nextEnd));
  }

  useEscapeKey(() => {
    if (findOpen) { setFindOpen(false); editorRef.current?.focus(); }
    else guarded(onClose);
  }, !pendingAction && !saving);

  const showPicker = state.stage === "ready" && (state.configs.length > 1 || scope.scope === "instance");
  const visibleFiles = state.stage === "ready"
    ? state.configs.filter((file) => `${file.displayName} ${file.relativePath}`.toLowerCase().includes(fileFilter.toLowerCase())) : [];

  return (
    <div className={styles.backdrop} role="presentation" onClick={() => guarded(onClose)}>
      <div className={styles.dialog} ref={modalRef} tabIndex={-1} role="dialog" aria-modal="true"
        aria-label={`Edit files for ${title}`} onClick={(event) => event.stopPropagation()} onKeyDown={handleKeys}>
        <header className={styles.header}>
          <div><h2 className={styles.title}>{title}</h2>{selected && <p className={styles.subtitle}>{selected.displayName}</p>}</div>
          <button type="button" className={styles.closeBtn} disabled={saving} aria-label="Close" onClick={() => guarded(onClose)}>✕</button>
        </header>
        <div className={styles.body}>
          {showPicker && <aside className={styles.filePicker}>
            <input className={styles.searchInput} aria-label="Filter files" placeholder="Filter files…" value={fileFilter} onChange={(event) => setFileFilter(event.target.value)} />
            <ul className={styles.fileList} aria-label="Config files">
              {visibleFiles.map((entry) => <li key={entry.relativePath}>
                <button type="button" disabled={saving} aria-pressed={selected?.relativePath === entry.relativePath}
                  className={`${styles.fileItem} ${selected?.relativePath === entry.relativePath ? styles.fileItemActive : ""}`}
                  onClick={() => { if (selected?.relativePath !== entry.relativePath) guarded(() => void openFile(entry)); }}>
                  {entry.displayName}
                </button>
              </li>)}
            </ul>
            {!visibleFiles.length && <p className={styles.hint}>No matching files.</p>}
          </aside>}
          <div className={styles.editorPane}>
            {state.stage === "loading-list" && <p className={styles.hint}>Loading files…</p>}
            {state.stage === "no-configs" && <p className={styles.hint}>{emptyHint}</p>}
            {state.stage === "list-error" && <p className={styles.error} role="alert">{state.message}</p>}
            {state.stage === "ready" && !selected && <p className={styles.hint}>Pick a file to edit. Filter by name or path.</p>}
            {loadingFile && <p className={styles.hint}>Loading file…</p>}
            {readError && <><p className={styles.error} role="alert">{readError}{loadedFile && " Your draft is still here."}</p><button className={styles.toolBtn} type="button" onClick={() => selected && guarded(() => void openFile(selected, loadedFile), loadedFile)}>Retry read</button></>}
            {selected && loadedFile && <>
              <div className={styles.toolbar}>
                <button type="button" className={styles.toolBtn} onClick={showFind}>Find <kbd>Ctrl+F</kbd></button>
                <span className={styles.hint}>{tabIndents ? "Tab / Shift+Tab to indent" : "Tab moves focus"} · Ctrl+M to toggle</span>
                <button type="button" className={styles.toolBtn} disabled={saving || loadingFile} onClick={() => guarded(() => void openFile(selected, true), true)}>Reload from disk</button>
              </div>
              {findOpen && <div className={styles.findBar} role="search" aria-label="Find in file">
                <input ref={findRef} className={styles.searchInput} aria-label="Find in file" placeholder="Find in file…" value={query}
                  onChange={(event) => {
                    resetFindSelection.current = event.target.value !== query;
                    setQuery(event.target.value);
                    setMatchIndex(0);
                  }}
                  onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); selectMatch(activeMatch + (event.shiftKey ? -1 : 1)); } }} />
                <button type="button" className={styles.toolBtn} aria-pressed={matchCase} onClick={() => { resetFindSelection.current = true; setMatchCase(!matchCase); setMatchIndex(0); }}>Match case</button>
                <span className={styles.matchCount} aria-live="polite">{query ? `${matches.length ? activeMatch + 1 : 0} of ${matches.length}` : "0 matches"}</span>
                <button type="button" className={styles.toolBtn} aria-label="Previous match" disabled={!matches.length} onClick={() => selectMatch(activeMatch - 1)}>↑</button>
                <button type="button" className={styles.toolBtn} aria-label="Next match" disabled={!matches.length} onClick={() => selectMatch(activeMatch + 1)}>↓</button>
                <button type="button" className={styles.toolBtn} aria-label="Close find" onClick={() => { setFindOpen(false); editorRef.current?.focus(); }}>✕</button>
              </div>}
              <textarea ref={editorRef} className={styles.textarea} value={content} disabled={loadingFile} readOnly={saving}
                onChange={(event) => { setContent(event.target.value); setCursor(event.target.selectionStart); }}
                onSelect={(event) => setCursor(event.currentTarget.selectionStart)} onKeyDown={indent}
                spellCheck={false} aria-label={`Editing ${selected.displayName}`} />
              {saveError && <p className={styles.error} role="alert">Save failed: {saveError} Your draft is still here. Retry Save, or copy it before reloading.</p>}
              {didSave && !dirty && scope.scope !== "world" && <p className={styles.hint} role="status">Saved to disk; some mods require restart and may overwrite live changes</p>}
              {(!didSave || dirty) && <p className={styles.hint}>{scope.scope === "world"
                ? "World files cannot be saved while the game is running."
                : "Saves update disk only. Running mods may need a restart and may overwrite live changes."}</p>}
              {recoveryDraft !== null && <div className={styles.toolbar}>
                <span className={styles.hint}>Your draft from before reload is recoverable until you switch files or close.</span>
                <button type="button" className={styles.toolBtn} disabled={saving || loadingFile} onClick={() => guarded(() => {
                  setContent(recoveryDraft);
                  setRecoveryDraft(null);
                  setDidSave(false);
                  requestAnimationFrame(() => editorRef.current?.focus());
                })}>Restore previous draft</button>
              </div>}
              <div className={styles.footer}>
                <span className={styles.dirtyHint} role="status">{saving ? "Saving…" : dirty ? "Unsaved changes" : didSave ? "Saved" : "No changes"}</span>
                <span className={styles.position}>Ln {line}, Col {column}</span>
                <button type="button" className={styles.saveBtn} disabled={!dirty || saving || loadingFile} onClick={() => void handleSave()}>{saving ? "Saving…" : saveError ? "Retry Save" : "Save"}</button>
              </div>
            </>}
          </div>
        </div>
      </div>
      {pendingAction && <ConfirmDialog title={pendingReload ? "Reload and keep draft?" : "Discard unsaved changes?"}
        message={pendingReload ? "Reload the file from disk? Your current draft will stay recoverable with Restore previous draft. A failed reload keeps it in the editor." : "This file has changes you haven't saved yet. Discard them? Copy your draft first if you need to keep it."}
        confirmLabel={pendingReload ? "Reload" : "Discard"} danger={!pendingReload} onConfirm={() => { const action = pendingAction; setPendingAction(null); action(); }} onCancel={() => setPendingAction(null)} />}
    </div>
  );
}
