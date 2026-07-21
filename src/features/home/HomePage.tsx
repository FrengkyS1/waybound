import { useEffect, useMemo, useRef, useState } from "react";
import { openPath } from "@tauri-apps/plugin-opener";
import {
  createInstance,
  deleteInstance,
  duplicateInstance,
  fetchInstances,
  fetchMinecraftVersions,
  renameInstance,
  setInstanceIcon,
} from "../instances/api";
import { fileToIconDataUrl } from "./imageIcon";
import { fetchCurseForgeStatus } from "../settings/api";
import { SignInDialog } from "../play/SignInDialog";
import type { ModSummary } from "../browse/types";
import type { InstanceSummary, ModLoader } from "../instances/types";
import { useTimedMessage } from "../../hooks/useTimedMessage";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { ContextMenu, type ContextMenuItem } from "../../components/ContextMenu";
import { CreateInstanceDialog } from "./CreateInstanceDialog";
import { InstanceCard } from "./InstanceCard";
import { InstancePage } from "../instance/InstancePage";
import { usePlayStore } from "../play/store";
import { useInstallStore } from "../install/installStore";
import styles from "./HomePage.module.css";

const LOADER_LABEL: Record<ModLoader, string> = {
  fabric: "Fabric",
  forge: "Forge",
  neoforge: "NeoForge",
  quilt: "Quilt",
  vanilla: "Vanilla",
};

interface HomePageProps {
  onAddMods: (instance: InstanceSummary) => void;
  onOpenMod: (summary: ModSummary) => void;
  onOpenSettings: () => void;
  reopenInstanceId?: string | null;
  onReopenConsumed?: () => void;
  selectedId: string | null;
  onSelectId: (id: string | null) => void;
}

export function HomePage({
  onAddMods,
  onOpenSettings,
  reopenInstanceId = null,
  onReopenConsumed,
  selectedId,
  onSelectId: setSelectedId,
}: HomePageProps) {
  const [instances, setInstances] = useState<InstanceSummary[]>([]);
  const [versions, setVersions] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState("");
  const [createOpen, setCreateOpen] = useState(false);
  // Separate from per-instance ops below — creating a new instance isn't
  // "about" any existing one, so it shouldn't disable/be disabled by them.
  const [creating, setCreating] = useState(false);
  // Keyed by instance id (not a single shared flag) so duplicating/deleting
  // one instance doesn't wrongly disable another instance's buttons, or the
  // "Create" dialog, while that operation is in flight.
  const [busyInstanceId, setBusyInstanceId] = useState<string | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [deleteTargetId, setDeleteTargetId] = useState<string | null>(null);
  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    instanceId: string;
  } | null>(null);
  const iconTargetId = useRef<string | null>(null);
  const iconFileRef = useRef<HTMLInputElement>(null);
  const { message, showMessage } = useTimedMessage();
  const [error, setError] = useState<string | null>(null);
  const refreshTick = usePlayStore((s) => s.refreshTick);
  const installTick = useInstallStore((s) => s.refreshTick);
  const account = usePlayStore((s) => s.account);
  const [cfConfigured, setCfConfigured] = useState(false);
  const [signInOpen, setSignInOpen] = useState(false);
  const [checklistDismissed, setChecklistDismissed] = useState(
    () => localStorage.getItem("waybound.gettingStartedDismissed") === "1",
  );

  const selected = instances.find((i) => i.id === selectedId) ?? null;

  const filtered = useMemo(() => {
    const term = search.trim().toLowerCase();
    if (!term) return instances;
    return instances.filter(
      (instance) =>
        instance.name.toLowerCase().includes(term) ||
        instance.minecraftVersion.includes(term) ||
        instance.loader.includes(term),
    );
  }, [instances, search]);

  async function refresh(selectId?: string | null) {
    const list = await fetchInstances();
    setInstances(list);
    const nextId = selectId === null ? null : (selectId ?? selectedId ?? null);
    if (nextId && list.some((i) => i.id === nextId)) {
      setSelectedId(nextId);
    } else {
      setSelectedId(null);
    }
  }

  useEffect(() => {
    void (async () => {
      try {
        const [list, gameVersions] = await Promise.all([
          fetchInstances(),
          fetchMinecraftVersions(),
        ]);
        setInstances(list);
        setVersions(gameVersions.map((v) => v.version));
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
    fetchCurseForgeStatus()
      .then((status) => setCfConfigured(status.configured))
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!reopenInstanceId) return;
    void refresh(reopenInstanceId).then(() => onReopenConsumed?.());
  }, [reopenInstanceId]);

  // Refresh instance stats (play time, last played) after a session ends.
  useEffect(() => {
    if (refreshTick === 0) return;
    void refresh(selectedId);
  }, [refreshTick]);

  // Refresh the instance list when a background install finishes.
  useEffect(() => {
    if (installTick === 0) return;
    void refresh(selectedId);
  }, [installTick]);

  async function handleCreate(input: {
    name: string;
    minecraftVersion: string;
    loader: ModLoader;
    icon: string | null;
  }) {
    setCreating(true);
    setError(null);
    try {
      const created = await createInstance({
        name: input.name,
        minecraftVersion: input.minecraftVersion,
        loader: input.loader,
      });
      if (input.icon) {
        await setInstanceIcon(created.id, input.icon);
      }
      setCreateOpen(false);
      showMessage(`Created "${created.name}".`);
      await refresh(created.id);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setCreating(false);
    }
  }

  async function handleChangeIcon(instanceId: string, icon: string | null) {
    setBusyInstanceId(instanceId);
    setError(null);
    try {
      await setInstanceIcon(instanceId, icon);
      await refresh(instanceId);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyInstanceId(null);
    }
  }

  async function handleRename(instanceId: string, name: string) {
    setBusyInstanceId(instanceId);
    setError(null);
    try {
      await renameInstance(instanceId, name);
      await refresh(instanceId);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      throw err;
    } finally {
      setBusyInstanceId(null);
    }
  }

  function handleOpenDetail(id: string) {
    setSelectedId(id);
  }

  async function handleDuplicate(id: string) {
    setBusyInstanceId(id);
    setError(null);
    try {
      const copy = await duplicateInstance(id);
      showMessage(`Duplicated to "${copy.name}".`);
      await refresh(selectedId);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyInstanceId(null);
    }
  }

  async function handleDelete(id: string) {
    setBusyInstanceId(id);
    setError(null);
    try {
      await deleteInstance(id);
      showMessage("Instance deleted.");
      await refresh(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyInstanceId(null);
    }
  }

  function pickIcon(instanceId: string) {
    iconTargetId.current = instanceId;
    iconFileRef.current?.click();
  }

  async function handleIconFileChange(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    e.target.value = "";
    const instanceId = iconTargetId.current;
    if (!file || !instanceId) return;
    try {
      await handleChangeIcon(instanceId, await fileToIconDataUrl(file));
    } catch {
      /* ignore unreadable images */
    }
  }

  function contextMenuItems(instance: InstanceSummary): ContextMenuItem[] {
    return [
      { label: "Open", onClick: () => handleOpenDetail(instance.id) },
      { label: "Rename", onClick: () => setRenamingId(instance.id) },
      { label: "Duplicate", onClick: () => void handleDuplicate(instance.id) },
      { label: "Change image", onClick: () => pickIcon(instance.id) },
      { label: "Open folder", onClick: () => void openPath(instance.rootPath) },
      { label: "Add mods", onClick: () => onAddMods(instance) },
      {
        label: "Delete instance",
        danger: true,
        onClick: () => setDeleteTargetId(instance.id),
      },
    ];
  }

  const deleteTarget = instances.find((i) => i.id === deleteTargetId) ?? null;
  const contextMenuInstance = contextMenu
    ? instances.find((i) => i.id === contextMenu.instanceId) ?? null
    : null;

  // Full-page instance view takes over when an instance is open.
  if (selected) {
    return (
      <InstancePage
        instance={selected}
        busy={busyInstanceId === selected.id}
        loaderLabel={LOADER_LABEL[selected.loader] ?? selected.loader}
        onBack={() => setSelectedId(null)}
        onDelete={() => void handleDelete(selected.id)}
        onDuplicate={() => void handleDuplicate(selected.id)}
        onAddMods={() => onAddMods(selected)}
        onChangeImage={(icon) => void handleChangeIcon(selected.id, icon)}
        onRename={(name) => handleRename(selected.id, name)}
      />
    );
  }

  return (
    <div className={styles.page}>
      <header className={styles.toolbar}>
        <div className={styles.toolbarLeft}>
          <button
            type="button"
            className={styles.createBtn}
            onClick={() => setCreateOpen(true)}
          >
            <span aria-hidden>+</span> Create
          </button>
        </div>
        <div className={styles.toolbarRight}>
          <input
            type="search"
            className={styles.search}
            placeholder="Search instances…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            aria-label="Search instances"
          />
        </div>
      </header>

      {message && <p className={styles.message}>{message}</p>}
      {error && <p className={styles.error}>{error}</p>}

      {loading && <p className={styles.status}>Loading instances…</p>}

      {!loading && instances.length === 0 && (
        <div className={styles.empty}>
          <p className={styles.emptyTitle}>No instances yet</p>
          <p className={styles.emptyHint}>
            Create a Minecraft profile, then add mods from Browse or directly
            from the instance.
          </p>
          <button
            type="button"
            className={styles.createBtn}
            onClick={() => setCreateOpen(true)}
          >
            <span aria-hidden>+</span> Create instance
          </button>

          {!checklistDismissed && (
            <section className={styles.checklist} aria-label="Getting started">
              <div className={styles.checklistHead}>
                <h2 className={styles.checklistTitle}>Getting started</h2>
                <button
                  type="button"
                  className={styles.checklistDismiss}
                  onClick={() => {
                    localStorage.setItem("waybound.gettingStartedDismissed", "1");
                    setChecklistDismissed(true);
                  }}
                >
                  Hide
                </button>
              </div>
              <ol className={styles.checklistItems}>
                <li className={styles.checklistItem}>
                  <span aria-hidden className={account ? styles.checkDone : styles.checkTodo}>
                    {account ? "✓" : "1"}
                  </span>
                  {account ? (
                    <span>Signed in as {account.username}</span>
                  ) : (
                    <button
                      type="button"
                      className={styles.checklistAction}
                      onClick={() => setSignInOpen(true)}
                    >
                      Sign in with Microsoft
                    </button>
                  )}
                  <span className={styles.checklistWhy}>needed to launch the game</span>
                </li>
                <li className={styles.checklistItem}>
                  <span aria-hidden className={cfConfigured ? styles.checkDone : styles.checkTodo}>
                    {cfConfigured ? "✓" : "2"}
                  </span>
                  {cfConfigured ? (
                    <span>CurseForge API key configured</span>
                  ) : (
                    <button
                      type="button"
                      className={styles.checklistAction}
                      onClick={onOpenSettings}
                    >
                      Add a CurseForge API key
                    </button>
                  )}
                  <span className={styles.checklistWhy}>
                    optional — search works with Modrinth without it
                  </span>
                </li>
                <li className={styles.checklistItem}>
                  <span aria-hidden className={styles.checkTodo}>3</span>
                  <button
                    type="button"
                    className={styles.checklistAction}
                    onClick={() => setCreateOpen(true)}
                  >
                    Create your first instance
                  </button>
                  <span className={styles.checklistWhy}>
                    Java is detected or downloaded automatically
                  </span>
                </li>
              </ol>
            </section>
          )}
        </div>
      )}

      {!loading && instances.length > 0 && filtered.length === 0 && (
        <div className={styles.empty}>
          <p className={styles.emptyTitle}>No matches</p>
          <p className={styles.emptyHint}>
            No instance matches “{search.trim()}”.
          </p>
        </div>
      )}

      {!loading && filtered.length > 0 && (
        <div className={styles.grid}>
          {filtered.map((instance) => (
            <InstanceCard
              key={instance.id}
              instance={instance}
              onOpen={() => void handleOpenDetail(instance.id)}
              onContextMenu={(e) => {
                e.preventDefault();
                setContextMenu({ x: e.clientX, y: e.clientY, instanceId: instance.id });
              }}
              renaming={renamingId === instance.id}
              onRenameCommit={(name) => {
                setRenamingId(null);
                void handleRename(instance.id, name);
              }}
              onRenameCancel={() => setRenamingId(null)}
            />
          ))}
        </div>
      )}

      {contextMenu && contextMenuInstance && (
        <ContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          items={contextMenuItems(contextMenuInstance)}
          onClose={() => setContextMenu(null)}
        />
      )}

      <input
        ref={iconFileRef}
        type="file"
        accept="image/*"
        hidden
        onChange={(e) => void handleIconFileChange(e)}
      />

      {deleteTarget && (
        <ConfirmDialog
          title="Delete instance?"
          message={`Delete "${deleteTarget.name}"? This removes the instance and its mod files. This can't be undone.`}
          confirmLabel="Delete"
          danger
          onConfirm={() => {
            setDeleteTargetId(null);
            void handleDelete(deleteTarget.id);
          }}
          onCancel={() => setDeleteTargetId(null)}
        />
      )}

      {signInOpen && <SignInDialog onClose={() => setSignInOpen(false)} />}

      {createOpen && (
        <CreateInstanceDialog
          versions={versions}
          busy={creating}
          onClose={() => setCreateOpen(false)}
          onCreate={(input) => void handleCreate(input)}
        />
      )}
    </div>
  );
}
