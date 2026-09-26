import { useEffect, useRef, useState } from "react";
import { FilterBar } from "./components/FilterBar";
import { ModRow } from "./components/ModRow";
import { ProjectDetailPage } from "./components/ProjectDetailPage";
import { InstallTargetDialog } from "./components/InstallTargetDialog";
import { SearchBar } from "./components/SearchBar";
import { fetchModDetails } from "./api";
import type { ModDetail } from "./detailTypes";
import { installVerb } from "./detailTypes";
import type { InstanceInstallTarget } from "../navigation/types";
import type { ModSummary } from "./types";
import { useTimedMessage } from "../../hooks/useTimedMessage";
import { useInstallStore } from "../install/installStore";
import { useBrowseStore } from "./store/browseStore";
import styles from "./BrowsePage.module.css";

const LOADER_LABEL: Record<string, string> = {
  fabric: "Fabric",
  forge: "Forge",
  neoforge: "NeoForge",
  quilt: "Quilt",
  vanilla: "Vanilla",
};

interface BrowsePageProps {
  installTarget?: InstanceInstallTarget | null;
  initialMod?: ModSummary | null;
  onInitialModConsumed?: () => void;
  onReturnToInstance?: () => void;
  onOpenSettings?: () => void;
}

export function BrowsePage({
  installTarget = null,
  initialMod = null,
  onInitialModConsumed,
  onReturnToInstance,
  onOpenSettings,
}: BrowsePageProps) {
  const results = useBrowseStore((s) => s.results);
  const totalHits = useBrowseStore((s) => s.totalHits);
  const query = useBrowseStore((s) => s.query);
  const loading = useBrowseStore((s) => s.loading);
  const error = useBrowseStore((s) => s.error);
  const warnings = useBrowseStore((s) => s.warnings);
  const setLoader = useBrowseStore((s) => s.setLoader);
  const setContentType = useBrowseStore((s) => s.setContentType);
  const offset = useBrowseStore((s) => s.offset);
  const limit = useBrowseStore((s) => s.limit);
  const nextPage = useBrowseStore((s) => s.nextPage);
  const prevPage = useBrowseStore((s) => s.prevPage);
  const [selected, setSelected] = useState<ModSummary | null>(initialMod);
  const listRef = useRef<HTMLDivElement>(null);
  // Row-level Install shortcut: fetches the row's detail on demand and
  // opens the same InstallTargetDialog the detail page uses, so no page
  // open is needed. Non-modpack rows with an install target skip the
  // dialog and install straight into that instance (same as the page).
  const [quickDetail, setQuickDetail] = useState<ModDetail | null>(null);
  const [quickPendingUid, setQuickPendingUid] = useState<string | null>(null);
  const [quickError, setQuickError] = useState<string | null>(null);
  const startInstall = useInstallStore((s) => s.startInstall);
  const { message: toast, showMessage: showToast } = useTimedMessage();

  function handleQuickInstall(mod: ModSummary) {
    if (quickPendingUid) return;
    setQuickError(null);
    if (installTarget && mod.projectType !== "modpack") {
      startInstall(mod.name, {
        modSummary: mod,
        source: null,
        instanceId: installTarget.instanceId,
        origin: "user",
      });
      showToast(`Installing ${mod.name}.`);
      return;
    }
    setQuickPendingUid(mod.uid);
    void fetchModDetails(mod)
      .then((detail) => setQuickDetail(detail))
      .catch((err) =>
        setQuickError(err instanceof Error ? err.message : String(err)),
      )
      .finally(() => setQuickPendingUid(null));
  }

  // Without this, Next/Prev swapped in a new page while the list stayed
  // scrolled wherever it was — landing the user mid-list on the new page
  // instead of its top, and (since the rows use native `loading="lazy"`)
  // leaving images that never crossed into view via an actual scroll
  // permanently undetected by the browser's lazy-load heuristic.
  useEffect(() => {
    listRef.current?.scrollTo({ top: 0 });
  }, [offset]);

  const pageStart = totalHits === 0 ? 0 : offset + 1;
  // Cross-source dedup can merge two full pages' worth of hits into fewer
  // unique results, so the page-end count must reflect what's actually
  // rendered, not the raw per-source page size.
  const pageEnd = Math.min(offset + results.length, totalHits);
  const hasPrev = offset > 0;
  const hasNext = offset + limit < totalHits;

  useEffect(() => {
    if (!initialMod) return;
    setSelected(initialMod);
    onInitialModConsumed?.();
  }, [initialMod]);

  useEffect(() => {
    if (installTarget) {
      setLoader(installTarget.loader);
      setContentType("mod");
    }
    void useBrowseStore.getState().search(true);
  }, [installTarget, setLoader, setContentType]);

  if (selected) {
    return (
      <ProjectDetailPage
        summary={selected}
        installTarget={installTarget}
        onBack={() => setSelected(null)}
        onReturnToInstance={onReturnToInstance}
      />
    );
  }

  return (
    <div className={styles.page}>
      {installTarget && (
        <div className={styles.instanceBanner} role="status">
          <div className={styles.instanceBannerBody}>
            <span className={styles.instanceBannerLabel}>Adding to</span>
            <strong className={styles.instanceBannerName}>
              {installTarget.instanceName}
            </strong>
            <span className={styles.instanceBannerMeta}>
              MC {installTarget.minecraftVersion} ·{" "}
              {LOADER_LABEL[installTarget.loader] ?? installTarget.loader}
            </span>
          </div>
          {onReturnToInstance && (
            <button
              type="button"
              className={styles.instanceBannerBack}
              onClick={onReturnToInstance}
            >
              Back to instance
            </button>
          )}
        </div>
      )}

      <header className={styles.toolbar}>
        <SearchBar />
        <FilterBar />
      </header>

      <div className={styles.statusRow}>
        <div className={styles.statusLeft}>
          {loading && <span className={styles.status}>Loading…</span>}
          {!loading && !error && (
            <span className={styles.status}>
              {totalHits > 0
                ? `${pageStart.toLocaleString()}–${pageEnd.toLocaleString()} of ${totalHits.toLocaleString()}`
                : "0 results"}
              {!query.trim() && " · Modrinth"}
            </span>
          )}
          {error && <span className={styles.error}>{error}</span>}
        </div>
      </div>

      {warnings.length > 0 && (
        <div className={styles.warnings}>
          {warnings.map((warning) => (
            <span key={warning} className={styles.warning}>
              {warning}
              {onOpenSettings && warning.includes("API key") && (
                <>
                  {" "}
                  <button
                    type="button"
                    className={styles.warningLink}
                    onClick={onOpenSettings}
                  >
                    Add one in Settings
                  </button>
                </>
              )}
            </span>
          ))}
        </div>
      )}

      {quickError && <p className={styles.error} role="alert">{quickError}</p>}
      {toast && <p className={styles.status} role="status">{toast}</p>}

      <div className={styles.list} role="list" ref={listRef}>
        {!loading && results.length === 0 && !error && (
          <div className={styles.empty}>
            <p className={styles.emptyTitle}>No results</p>
            <p className={styles.emptyHint}>
              Try a different search term, or clear the version/loader filters
              above.
            </p>
          </div>
        )}
        {results.map((mod) => (
          <ModRow
            key={mod.uid}
            mod={mod}
            onOpen={setSelected}
            onInstall={handleQuickInstall}
            installLabel={
              installTarget && mod.projectType !== "modpack"
                ? `Add to ${installTarget.instanceName}`
                : installVerb(mod.projectType)
            }
            installPending={quickPendingUid === mod.uid}
          />
        ))}
      </div>

      {totalHits > limit && (
        <nav className={styles.pagination} aria-label="Pagination">
          <button
            type="button"
            className={styles.pageBtn}
            disabled={!hasPrev || loading}
            onClick={() => void prevPage()}
          >
            ← Previous
          </button>
          <span className={styles.pageInfo}>
            {pageStart.toLocaleString()}–{pageEnd.toLocaleString()} of{" "}
            {totalHits.toLocaleString()}
          </span>
          <button
            type="button"
            className={styles.pageBtn}
            disabled={!hasNext || loading}
            onClick={() => void nextPage()}
          >
            Next →
          </button>
        </nav>
      )}

      {quickDetail && (
        <InstallTargetDialog
          detail={quickDetail}
          fixedInstanceId={installTarget?.instanceId}
          onClose={() => setQuickDetail(null)}
          onSuccess={(message) => {
            showToast(message);
            setQuickDetail(null);
            if (installTarget && onReturnToInstance) {
              onReturnToInstance();
            }
          }}
        />
      )}
    </div>
  );
}
