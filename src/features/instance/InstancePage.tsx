import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { openPath } from "@tauri-apps/plugin-opener";

import {
  fetchContentMeta,
  fetchInstanceContent,
  fetchModSummaryForContent,
  removeContentFile,
  setContentEnabled,
  updateModInInstance,
  type ContentCategory,
  type ContentEntry,
  type InstanceContent,
} from "../instances/api";
import type { InstanceSummary } from "../instances/types";
import type { ModSummary } from "../browse/types";
import { fileToIconDataUrl } from "../home/imageIcon";
import { PlayButton } from "../play/PlayButton";
import { usePlayStore } from "../play/store";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { LaunchOverrides } from "./LaunchOverrides";
import styles from "./InstancePage.module.css";

export type Tab = "overview" | "content" | "logs" | "settings";

// Stable reference so the store selector doesn't return a new array each render
// (which would loop useSyncExternalStore and blank the screen).
const EMPTY_LOGS: string[] = [];

interface InstancePageProps {
  instance: InstanceSummary;
  busy: boolean;
  loaderLabel: string;
  onBack: () => void;
  onDelete: () => void;
  onDuplicate: () => void;
  onAddMods: () => void;
  onChangeImage: (icon: string | null) => void;
  onRename: (name: string) => Promise<void>;
  /** Opens a mod's project page in Browse — omitted, the Content tab's rows
   * just aren't clickable. */
  onOpenMod?: (summary: ModSummary) => void;
  /** Which tab is showing, lifted to the app shell so it survives navigating
   * away (e.g. to a mod's page in Browse) and back — InstancePage itself
   * remounts on every return, so anything kept in its own state would reset
   * to "overview" every time. */
  tab: Tab;
  onTabChange: (tab: Tab) => void;
}

const LAUNCHABLE = new Set(["vanilla", "fabric", "forge", "neoforge"]);

export function InstancePage({
  instance,
  busy,
  loaderLabel,
  onBack,
  onDelete,
  onDuplicate,
  onAddMods,
  onChangeImage,
  onRename,
  onOpenMod,
  tab,
  onTabChange: setTab,
}: InstancePageProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [nameDraft, setNameDraft] = useState<string | null>(null);
  const [renameError, setRenameError] = useState<string | null>(null);
  const [deleteConfirmOpen, setDeleteConfirmOpen] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const logs = usePlayStore((s) => s.logsByInstance[instance.id]) ?? EMPTY_LOGS;

  async function commitRename() {
    if (nameDraft === null) return;
    const next = nameDraft.trim();
    if (next.length < 2 || next === instance.name) {
      setNameDraft(null);
      setRenameError(null);
      return;
    }
    try {
      await onRename(next);
      setNameDraft(null);
      setRenameError(null);
    } catch (err) {
      setRenameError(err instanceof Error ? err.message : String(err));
    }
  }

  // Close the kebab menu on outside click or Escape.
  useEffect(() => {
    if (!menuOpen) return;
    function onPointer(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node))
        setMenuOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setMenuOpen(false);
    }
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  async function handlePickImage(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    try {
      onChangeImage(await fileToIconDataUrl(file));
    } catch {
      /* ignore unreadable images */
    }
  }

  const initial = instance.name.trim().charAt(0).toUpperCase() || "?";
  const launchable = LAUNCHABLE.has(instance.loader);

  function confirmDelete() {
    setDeleteConfirmOpen(true);
  }

  return (
    <div className={styles.page}>
      <button type="button" className={styles.back} onClick={onBack}>
        <span aria-hidden>←</span> My Instances
      </button>

      <header className={styles.hero}>
        {instance.icon && (
          <div className={styles.heroBack} aria-hidden>
            <img className={styles.heroBackImg} src={instance.icon} alt="" />
            <div className={styles.heroBackShade} />
          </div>
        )}
        <button
          type="button"
          className={styles.iconBtn}
          onClick={() => fileRef.current?.click()}
          disabled={busy}
          aria-label="Change instance image"
        >
          {instance.icon ? (
            <img className={styles.iconImg} src={instance.icon} alt="" />
          ) : (
            <span className={styles.iconInitial} aria-hidden>
              {initial}
            </span>
          )}
          <span className={styles.iconOverlay}>Edit</span>
        </button>
        <input
          ref={fileRef}
          type="file"
          accept="image/*"
          hidden
          onChange={(e) => void handlePickImage(e)}
        />

        <div className={styles.heroBody}>
          <div className={styles.heroTop}>
            {nameDraft !== null ? (
              <input
                className={styles.nameInput}
                value={nameDraft}
                autoFocus
                aria-label="Instance name"
                onChange={(e) => setNameDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void commitRename();
                  if (e.key === "Escape") {
                    setNameDraft(null);
                    setRenameError(null);
                  }
                }}
                onBlur={() => {
                  setNameDraft(null);
                  setRenameError(null);
                }}
              />
            ) : (
              <button
                type="button"
                className={styles.name}
                title="Rename"
                onClick={() => setNameDraft(instance.name)}
              >
                {instance.name}
              </button>
            )}
            <div className={styles.badges}>
              <span className={styles.badge}>{loaderLabel}</span>
              <span className={styles.badge}>{instance.minecraftVersion}</span>
              {instance.loaderVersion && (
                <span className={styles.badgeMuted}>
                  {instance.loaderVersion}
                </span>
              )}
            </div>
          </div>

          {renameError && <p className={styles.renameError}>{renameError}</p>}

          <div className={styles.stats}>
            <Stat
              label="Last played"
              value={formatLastPlayed(instance.lastPlayed)}
            />
            <Stat
              label="Play time"
              value={formatDuration(instance.totalPlaySeconds)}
            />
            <Stat label="Created" value={formatDate(instance.createdAt)} />
            <Stat label="Mods" value={String(instance.modCount)} />
          </div>
        </div>

        <div className={styles.heroActions}>
          {launchable ? (
            <PlayButton instanceId={instance.id} instanceName={instance.name} />
          ) : (
            <PlayButton
              instanceId={instance.id}
              instanceName={instance.name}
              disabled
            />
          )}
          <div className={styles.menuWrap} ref={menuRef}>
            <button
              type="button"
              className={styles.kebab}
              aria-haspopup="menu"
              aria-expanded={menuOpen}
              aria-label="More actions"
              onClick={() => setMenuOpen((v) => !v)}
            >
              ⋯
            </button>
            {menuOpen && (
              <div className={styles.menu} role="menu">
                <button
                  type="button"
                  className={styles.menuItem}
                  role="menuitem"
                  onClick={() => {
                    setMenuOpen(false);
                    setNameDraft(instance.name);
                  }}
                >
                  Rename
                </button>
                <button
                  type="button"
                  className={styles.menuItem}
                  role="menuitem"
                  onClick={() => {
                    setMenuOpen(false);
                    onDuplicate();
                  }}
                >
                  Duplicate
                </button>
                <button
                  type="button"
                  className={styles.menuItem}
                  role="menuitem"
                  onClick={() => {
                    setMenuOpen(false);
                    fileRef.current?.click();
                  }}
                >
                  Change image
                </button>
                <button
                  type="button"
                  className={styles.menuItem}
                  role="menuitem"
                  onClick={() => {
                    setMenuOpen(false);
                    void openPath(instance.rootPath);
                  }}
                >
                  Open folder
                </button>
                <button
                  type="button"
                  className={styles.menuItem}
                  role="menuitem"
                  onClick={() => {
                    setMenuOpen(false);
                    onBack();
                  }}
                >
                  Back to instances
                </button>
                <button
                  type="button"
                  className={`${styles.menuItem} ${styles.menuDanger}`}
                  role="menuitem"
                  onClick={() => {
                    setMenuOpen(false);
                    confirmDelete();
                  }}
                >
                  Delete instance
                </button>
              </div>
            )}
          </div>
        </div>
      </header>

      <nav className={styles.tabs} aria-label="Instance sections">
        <TabButton id="overview" active={tab} onClick={setTab}>
          Overview
        </TabButton>
        <TabButton id="content" active={tab} onClick={setTab}>
          Content
        </TabButton>
        <TabButton id="logs" active={tab} onClick={setTab}>
          Logs
        </TabButton>
        <TabButton id="settings" active={tab} onClick={setTab}>
          Settings
        </TabButton>
        <button
          type="button"
          className={styles.addContent}
          onClick={onAddMods}
          disabled={busy}
        >
          <span aria-hidden>+</span> Add content
        </button>
      </nav>

      <div className={styles.content}>
        {tab === "overview" && (
          <OverviewTab
            instance={instance}
            loaderLabel={loaderLabel}
            launchable={launchable}
          />
        )}
        {tab === "content" && (
          <ContentTab instance={instance} busy={busy} onAddMods={onAddMods} onOpenMod={onOpenMod} />
        )}
        {tab === "logs" && (
          <LogsTab
            instanceId={instance.id}
            instanceName={instance.name}
            logs={logs}
          />
        )}
        {tab === "settings" && (
          <SettingsTab
            instance={instance}
            busy={busy}
            onDelete={confirmDelete}
          />
        )}
      </div>

      {deleteConfirmOpen && (
        <ConfirmDialog
          title="Delete instance?"
          message={`Delete "${instance.name}"? This removes the instance and its mod files. This can't be undone.`}
          confirmLabel="Delete"
          danger
          onConfirm={() => {
            setDeleteConfirmOpen(false);
            onDelete();
          }}
          onCancel={() => setDeleteConfirmOpen(false)}
        />
      )}
    </div>
  );
}

function TabButton({
  id,
  active,
  onClick,
  children,
}: {
  id: Tab;
  active: Tab;
  onClick: (t: Tab) => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      className={`${styles.tab} ${active === id ? styles.tabActive : ""}`}
      aria-current={active === id ? "page" : undefined}
      onClick={() => onClick(id)}
    >
      {children}
    </button>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className={styles.stat}>
      <span className={styles.statLabel}>{label}</span>
      <span className={styles.statValue}>{value}</span>
    </div>
  );
}

function OverviewTab({
  instance,
  loaderLabel,
  launchable,
}: {
  instance: InstanceSummary;
  loaderLabel: string;
  launchable: boolean;
}) {
  return (
    <div className={styles.overview}>
      <div className={styles.infoCard}>
        <h2 className={styles.cardTitle}>About this instance</h2>
        <dl className={styles.infoGrid}>
          <div>
            <dt>Minecraft</dt>
            <dd>{instance.minecraftVersion}</dd>
          </div>
          <div>
            <dt>Loader</dt>
            <dd>
              {loaderLabel}
              {instance.loaderVersion ? ` ${instance.loaderVersion}` : ""}
            </dd>
          </div>
          <div>
            <dt>Installed mods</dt>
            <dd>{instance.modCount}</dd>
          </div>
          <div>
            <dt>Folder</dt>
            <dd className={styles.mono}>{instance.rootPath}</dd>
          </div>
        </dl>
        {!launchable && (
          <p className={styles.note}>
            Launching {loaderLabel} isn't supported yet — Vanilla, Fabric,
            Forge, and NeoForge can play.
          </p>
        )}
      </div>
    </div>
  );
}

type ContentFilter = "all" | "mod" | "resourcepack" | "shaderpack";

const CATEGORY_LABEL: Record<ContentCategory, string> = {
  mod: "Mods",
  resourcepack: "Resource packs",
  shaderpack: "Shaders",
};

function ContentTab({
  instance,
  busy,
  onAddMods,
  onOpenMod,
}: {
  instance: InstanceSummary;
  busy: boolean;
  onAddMods: () => void;
  onOpenMod?: (summary: ModSummary) => void;
}) {
  const [content, setContent] = useState<InstanceContent | null>(null);
  const [filter, setFilter] = useState<ContentFilter>("all");
  const [search, setSearch] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function load() {
    try {
      setContent(await fetchInstanceContent(instance.id));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instance.id]);

  // The manual-download watcher writes straight into this instance's
  // mods/resourcepacks folder, bypassing the normal install flow — without
  // this, the list would sit stale until the user left and came back.
  useEffect(() => {
    const unlisten = listen<{ instanceId: string }>("missing-mods://placed", (event) => {
      if (event.payload.instanceId === instance.id) void load();
    });
    return () => void unlisten.then((fn) => fn());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instance.id]);

  // Name + icon are resolved lazily, one file at a time, only for rows that
  // actually scroll into view — opening every jar up front is what made a
  // 300+ mod instance take seconds to show its content list.
  const observerRef = useRef<IntersectionObserver | null>(null);
  const fetchedRef = useRef<Set<string>>(new Set());
  const targetsRef = useRef<Map<Element, { category: ContentCategory; fileName: string }>>(
    new Map(),
  );

  useEffect(() => {
    fetchedRef.current = new Set();
    targetsRef.current = new Map();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (!e.isIntersecting) continue;
          const info = targetsRef.current.get(e.target);
          if (!info) continue;
          const key = `${info.category}:${info.fileName}`;
          if (fetchedRef.current.has(key)) continue;
          fetchedRef.current.add(key);
          observer.unobserve(e.target);
          targetsRef.current.delete(e.target);
          void fetchContentMeta(instance.id, info.category, info.fileName).then(
            (meta) => {
              if (meta.name || meta.icon) {
                patchEntry(info.category, info.fileName, meta);
              }
            },
          );
        }
      },
      { rootMargin: "300px" },
    );
    observerRef.current = observer;
    return () => observer.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instance.id]);

  // `metaResolved` means the backend's on-disk cache already had this exact
  // file's name/icon (or confirmed it has neither) — skip observing it
  // entirely so a previously-seen instance's Content tab does zero fetches,
  // not just fast ones.
  function observeRow(category: ContentCategory, fileName: string, metaResolved: boolean) {
    return (el: HTMLLIElement | null) => {
      const key = `${category}:${fileName}`;
      if (!el || metaResolved || fetchedRef.current.has(key)) return;
      targetsRef.current.set(el, { category, fileName });
      observerRef.current?.observe(el);
    };
  }

  const groups: { category: ContentCategory; entries: ContentEntry[] }[] =
    content
      ? [
          { category: "mod", entries: content.mods },
          { category: "resourcepack", entries: content.resourcePacks },
          { category: "shaderpack", entries: content.shaderPacks },
        ]
      : [];

  const counts = {
    mod: content?.mods.length ?? 0,
    resourcepack: content?.resourcePacks.length ?? 0,
    shaderpack: content?.shaderPacks.length ?? 0,
  };
  const total = counts.mod + counts.resourcepack + counts.shaderpack;

  const categoryKey: Record<ContentCategory, "mods" | "resourcePacks" | "shaderPacks"> = {
    mod: "mods",
    resourcepack: "resourcePacks",
    shaderpack: "shaderPacks",
  };

  function patchEntry(
    category: ContentCategory,
    fileName: string,
    patch: Partial<ContentEntry>,
  ) {
    setContent((prev) => {
      if (!prev) return prev;
      const key = categoryKey[category];
      return {
        ...prev,
        [key]: prev[key].map((e) =>
          e.fileName === fileName ? { ...e, ...patch } : e,
        ),
      };
    });
  }

  function dropEntry(category: ContentCategory, fileName: string) {
    setContent((prev) => {
      if (!prev) return prev;
      const key = categoryKey[category];
      return { ...prev, [key]: prev[key].filter((e) => e.fileName !== fileName) };
    });
  }

  // Apply the change to local state immediately, fire the mutation in the
  // background, and only re-fetch (rolling the optimistic edit back) if it
  // fails — avoids a full content re-scan after every single toggle/remove.
  async function act(optimistic: () => void, fn: () => Promise<void>) {
    setError(null);
    optimistic();
    try {
      await fn();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      await load();
    }
  }

  // Not optimistic like `act` above — an update can resolve to a different
  // filename entirely (a version bump), so there's nothing sensible to patch
  // in ahead of the result. Tracked per-file rather than via the shared
  // `busy` prop so updating one mod doesn't freeze every other row's buttons.
  const [updatingFiles, setUpdatingFiles] = useState<Set<string>>(new Set());
  const [message, setMessage] = useState<string | null>(null);

  async function handleUpdate(fileName: string) {
    setError(null);
    setMessage(null);
    setUpdatingFiles((prev) => new Set(prev).add(fileName));
    const installId =
      typeof crypto !== "undefined" && crypto.randomUUID
        ? crypto.randomUUID()
        : `${Date.now()}-${Math.random()}`;
    try {
      const result = await updateModInInstance(instance.id, fileName, installId);
      setMessage(result.message);
      setTimeout(() => setMessage(null), 6000);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setUpdatingFiles((prev) => {
        const next = new Set(prev);
        next.delete(fileName);
        return next;
      });
      await load();
    }
  }

  const [openingFile, setOpeningFile] = useState<string | null>(null);

  async function handleOpenMod(entry: ContentEntry) {
    if (!onOpenMod) return;
    setError(null);
    setOpeningFile(entry.fileName);
    try {
      const summary = await fetchModSummaryForContent(instance.id, entry.fileName);
      // The DB's stored name is filename-derived (whatever `sync_mods_folder`
      // saw at import time), but the Content row has already resolved the
      // jar's own real display name — prefer that over the backend's guess.
      onOpenMod(entry.name ? { ...summary, name: entry.name } : summary);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setOpeningFile(null);
    }
  }

  const term = search.trim().toLowerCase();
  const visible = groups
    .filter((g) => filter === "all" || g.category === filter)
    .map((g) => ({
      ...g,
      entries: term
        ? g.entries.filter((e) => e.fileName.toLowerCase().includes(term))
        : g.entries,
    }));

  if (content && total === 0) {
    return (
      <div className={styles.emptyState}>
        <p className={styles.emptyTitle}>No content yet</p>
        <p className={styles.emptyHint}>
          Add mods, resource packs, or a modpack. Waybound picks a compatible
          file for {instance.minecraftVersion}.
        </p>
        <button
          type="button"
          className={styles.primaryBtn}
          onClick={onAddMods}
          disabled={busy}
        >
          Add content
        </button>
      </div>
    );
  }

  return (
    <div>
      <div className={styles.contentToolbar}>
        <div
          className={styles.contentFilters}
          role="group"
          aria-label="Filter content by type"
        >
          {(
            ["all", "mod", "resourcepack", "shaderpack"] as ContentFilter[]
          ).map((f) => (
            <button
              key={f}
              type="button"
              className={`${styles.chip} ${filter === f ? styles.chipActive : ""}`}
              aria-pressed={filter === f}
              onClick={() => setFilter(f)}
            >
              {f === "all"
                ? `All (${total})`
                : `${CATEGORY_LABEL[f]} (${counts[f]})`}
            </button>
          ))}
        </div>
        <input
          type="search"
          className={styles.contentSearch}
          placeholder="Search content…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          aria-label="Search installed content"
        />
      </div>

      {error && <p className={styles.error}>{error}</p>}
      {message && <p className={styles.message}>{message}</p>}

      {visible.map((group) =>
        group.entries.length === 0 ? null : (
          <section key={group.category} className={styles.contentGroup}>
            {filter === "all" && (
              <h3 className={styles.contentGroupTitle}>
                {CATEGORY_LABEL[group.category]}
              </h3>
            )}
            <ul
              className={styles.modList}
              aria-label={CATEGORY_LABEL[group.category]}
            >
              {group.entries.map((entry) => {
                const iconAndInfo = (
                  <>
                    <div className={styles.modIconWrap}>
                      {entry.icon ? (
                        <img
                          src={entry.icon}
                          alt=""
                          className={styles.modIcon}
                          loading="lazy"
                        />
                      ) : (
                        <div className={styles.modIconFallback} aria-hidden />
                      )}
                    </div>
                    <div className={styles.modInfo}>
                      <span className={styles.modName}>
                        {entry.name ?? humanizeFileName(entry.fileName)}
                      </span>
                      <span
                        className={styles.modMeta}
                        title={formatSize(entry.sizeBytes)}
                      >
                        {entry.fileName}
                      </span>
                    </div>
                  </>
                );
                // Only "mod" rows can be resolved back to a tracked project
                // right now — a resourcepack/shaderpack has no equivalent
                // lookup, so its row stays non-interactive.
                const canOpen = Boolean(onOpenMod) && group.category === "mod";
                return (
                <li
                  key={entry.fileName}
                  ref={observeRow(group.category, entry.fileName, entry.metaResolved)}
                  className={`${styles.modRow} ${entry.enabled ? "" : styles.modRowDisabled}`}
                >
                  {canOpen ? (
                    <button
                      type="button"
                      className={styles.modOpen}
                      disabled={openingFile === entry.fileName}
                      onClick={() => void handleOpenMod(entry)}
                    >
                      {iconAndInfo}
                    </button>
                  ) : (
                    iconAndInfo
                  )}
                  <div className={styles.modActions}>
                    {group.category === "mod" && (
                      <button
                        type="button"
                        className={styles.updateBtn}
                        disabled={busy || updatingFiles.has(entry.fileName)}
                        onClick={() => void handleUpdate(entry.fileName)}
                      >
                        {updatingFiles.has(entry.fileName) ? "Updating…" : "Update"}
                      </button>
                    )}
                    <button
                      type="button"
                      className={`${styles.toggleBtn} ${
                        entry.enabled ? styles.toggleBtnOn : styles.toggleBtnOff
                      }`}
                      disabled={busy}
                      onClick={() =>
                        void act(
                          () =>
                            patchEntry(group.category, entry.fileName, {
                              enabled: !entry.enabled,
                            }),
                          () =>
                            setContentEnabled(
                              instance.id,
                              group.category,
                              entry.fileName,
                              !entry.enabled,
                            ),
                        )
                      }
                    >
                      {entry.enabled ? "Enabled" : "Disabled"}
                    </button>
                    <button
                      type="button"
                      className={styles.removeBtn}
                      disabled={busy}
                      aria-label={`Remove ${entry.fileName}`}
                      onClick={() =>
                        void act(
                          () => dropEntry(group.category, entry.fileName),
                          () =>
                            removeContentFile(
                              instance.id,
                              group.category,
                              entry.fileName,
                            ),
                        )
                      }
                    >
                      Remove
                    </button>
                  </div>
                </li>
                );
              })}
            </ul>
          </section>
        ),
      )}
    </div>
  );
}

function formatSize(bytes: number): string {
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

function humanizeFileName(fileName: string): string {
  return fileName
    .replace(/\.(jar|zip)$/i, "")
    // Minecraft formatting codes (§ + one char) — some shader packs bake
    // these into the zip name itself so it renders colorfully in the
    // in-game shader list; outside that context they're just noise.
    .replace(/§./g, "")
    .replace(/[-_]+/g, " ");
}

const LOG_ERROR_RE = /\b(ERROR|FATAL|Exception|Caused by)\b/;

function LogsTab({
  instanceId,
  instanceName,
  logs,
}: {
  instanceId: string;
  instanceName: string;
  logs: string[];
}) {
  const ref = useRef<HTMLPreElement>(null);
  const firstErrorRef = useRef<HTMLSpanElement>(null);
  const clearLogs = usePlayStore((s) => s.clearLogs);
  const [copied, setCopied] = useState(false);
  const firstErrorIdx = logs.findIndex((line) => LOG_ERROR_RE.test(line));

  useEffect(() => {
    if (ref.current) ref.current.scrollTop = ref.current.scrollHeight;
  }, [logs]);

  async function copy() {
    try {
      await navigator.clipboard.writeText(logs.join("\n"));
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard unavailable */
    }
  }

  function download() {
    const blob = new Blob([logs.join("\n")], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${instanceName.replace(/[^a-z0-9-_]+/gi, "_")}-latest.log`;
    a.click();
    URL.revokeObjectURL(url);
  }

  const empty = logs.length === 0;

  return (
    <div className={styles.logsTab}>
      <div className={styles.logsToolbar}>
        <span className={styles.logsCount}>
          {empty ? "No output" : `${logs.length} lines`}
        </span>
        <div className={styles.logsActions}>
          {firstErrorIdx !== -1 && (
            <button
              type="button"
              className={styles.ghostBtn}
              onClick={() =>
                firstErrorRef.current?.scrollIntoView({ block: "center" })
              }
            >
              Jump to error
            </button>
          )}
          <button
            type="button"
            className={styles.ghostBtn}
            disabled={empty}
            onClick={() => void copy()}
          >
            {copied ? "Copied" : "Copy"}
          </button>
          <button
            type="button"
            className={styles.ghostBtn}
            disabled={empty}
            onClick={download}
          >
            Download
          </button>
          <button
            type="button"
            className={styles.ghostBtn}
            disabled={empty}
            onClick={() => clearLogs(instanceId)}
          >
            Clear
          </button>
        </div>
      </div>
      {empty ? (
        <div className={styles.emptyState}>
          <p className={styles.emptyTitle}>No logs yet</p>
          <p className={styles.emptyHint}>
            Launch the instance to see the game's console output here.
          </p>
        </div>
      ) : (
        <pre className={styles.logView} ref={ref}>
          {logs.map((line, i) => (
            <span
              key={i}
              ref={i === firstErrorIdx ? firstErrorRef : undefined}
              className={LOG_ERROR_RE.test(line) ? styles.logErrorLine : undefined}
            >
              {line}
              {"\n"}
            </span>
          ))}
        </pre>
      )}
    </div>
  );
}

function SettingsTab({
  instance,
  busy,
  onDelete,
}: {
  instance: InstanceSummary;
  busy: boolean;
  onDelete: () => void;
}) {
  return (
    <div className={styles.settings}>
      <LaunchOverrides instanceId={instance.id} />

      <section className={styles.dangerCard}>
        <div>
          <h2 className={styles.cardTitle}>Delete instance</h2>
          <p className={styles.note}>
            Removes this instance and its mod files. This can't be undone.
          </p>
        </div>
        <button
          type="button"
          className={styles.deleteBtn}
          disabled={busy}
          onClick={onDelete}
        >
          Delete
        </button>
      </section>
    </div>
  );
}

function formatDuration(seconds: number): string {
  if (!seconds) return "—";
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m`;
  return `${seconds}s`;
}

function formatLastPlayed(unix?: number | null): string {
  if (!unix) return "Never";
  return formatDate(unix);
}

function formatDate(unix: number): string {
  return new Date(unix * 1000).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}
