import { useEffect, useMemo, useState } from "react";
import {
  fetchActivityLogs,
  fetchModDetails,
  fetchVersionChangelog,
} from "../api";
import type { ModSummary } from "../types";
import type { ModLoader } from "../../instances/types";
import { useTimedMessage } from "../../../hooks/useTimedMessage";
import { useInstallStore } from "../../install/installStore";
import type { InstanceInstallTarget } from "../../navigation/types";
import { isInstallableType, installVerb } from "../detailTypes";
import type { VersionPrefill } from "../../settings/types";
import { InstallTargetDialog } from "./InstallTargetDialog";
import { MarkdownBody } from "./MarkdownBody";
import { OverviewBody } from "./OverviewBody";
import { ModpackContentTab } from "./ModpackContentTab";
import { SourceBadges } from "./SourceBadges";
import styles from "./ProjectDetailPage.module.css";

type BaseTab = "overview" | "versions";
type ModpackTab =
  BaseTab | "content" | "changelog" | "gallery" | "comments" | "logs";
type Tab = BaseTab | ModpackTab;

function formatDownloads(count: number): string {
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}K`;
  return count.toString();
}

function formatDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

function pickLoader(loaders: ModLoader[], fallback: ModLoader): ModLoader {
  return loaders[0] ?? fallback;
}

interface ProjectDetailPageProps {
  summary: ModSummary;
  installTarget?: InstanceInstallTarget | null;
  onBack: () => void;
  onReturnToInstance?: () => void;
}

export function ProjectDetailPage({
  summary,
  installTarget = null,
  onBack,
  onReturnToInstance,
}: ProjectDetailPageProps) {
  const [detail, setDetail] = useState<Awaited<
    ReturnType<typeof fetchModDetails>
  > | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("overview");
  const [installOpen, setInstallOpen] = useState(false);
  const startInstall = useInstallStore((s) => s.startInstall);
  const [versionPrefill, setVersionPrefill] = useState<
    VersionPrefill | undefined
  >();
  const { message: toast, showMessage: showToast } = useTimedMessage();
  const [contentVersionId, setContentVersionId] = useState<
    string | undefined
  >();
  const [changelogVersionId, setChangelogVersionId] = useState<
    string | undefined
  >();
  const [changelogBody, setChangelogBody] = useState<string | null>(null);
  const [changelogLoading, setChangelogLoading] = useState(false);
  const [activityLogs, setActivityLogs] = useState<
    Awaited<ReturnType<typeof fetchActivityLogs>>
  >([]);

  const isModpack = summary.projectType === "modpack";

  useEffect(() => {
    void (async () => {
      try {
        const loaded = await fetchModDetails(summary);
        setDetail(loaded);
        setContentVersionId(loaded.versions[0]?.id);
        setChangelogVersionId(loaded.versions[0]?.id);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, [summary]);

  useEffect(() => {
    if (tab !== "changelog" || !detail || !changelogVersionId) return;
    setChangelogLoading(true);
    void fetchVersionChangelog(detail.summary, changelogVersionId)
      .then((value) => setChangelogBody(value))
      .catch(() => setChangelogBody(null))
      .finally(() => setChangelogLoading(false));
  }, [tab, detail, changelogVersionId]);

  useEffect(() => {
    if (tab !== "logs") return;
    void fetchActivityLogs(100).then(setActivityLogs);
  }, [tab]);

  const data = detail?.summary ?? summary;
  const canInstall = isInstallableType(data.projectType);

  const projectLogs = useMemo(
    () => activityLogs.filter((entry) => entry.projectUid === data.uid),
    [activityLogs, data.uid],
  );

  const tabs = useMemo((): { id: Tab; label: string }[] => {
    const base: { id: Tab; label: string }[] = [
      { id: "overview", label: "Overview" },
    ];
    if (isModpack) {
      base.push({ id: "content", label: "Content" });
    }
    base.push({ id: "changelog", label: "Changelog" });
    if ((detail?.gallery.length ?? 0) > 0) {
      base.push({ id: "gallery", label: "Gallery" });
    }
    base.push({ id: "versions", label: "Versions" });
    if (detail?.commentsUrl) {
      base.push({ id: "comments", label: "Comments" });
    }
    base.push({ id: "logs", label: "Logs" });
    return base;
  }, [detail, isModpack]);

  const directInstall =
    Boolean(installTarget) && data.projectType !== "modpack";

  function openInstall(version?: VersionPrefill) {
    if (directInstall && installTarget) {
      void handleDirectInstall(version);
      return;
    }
    setVersionPrefill(version);
    setInstallOpen(true);
  }

  function handleDirectInstall(version?: VersionPrefill) {
    if (!installTarget) return;
    const name = detail?.summary?.name ?? summary.name;
    startInstall(name, {
      modSummary: detail?.summary ?? summary,
      source: null,
      instanceId: installTarget.instanceId,
      versionId: version?.versionId,
    });
    showToast(`Installing ${name}…`);
  }

  return (
    <div className={styles.page}>
      <div className={styles.pageInner}>
        <div className={styles.backRow}>
          <button type="button" className={styles.backBtn} onClick={onBack}>
            ← Back to browse
          </button>
          {onReturnToInstance && (
            <button type="button" className={styles.backBtn} onClick={onReturnToInstance}>
              ← Back to instance
            </button>
          )}
        </div>

        {loading && <p className={styles.status}>Loading project…</p>}
        {error && <p className={styles.error}>{error}</p>}
        {toast && <p className={styles.toast}>{toast}</p>}

        {!loading && (
          <>
            <header className={styles.hero}>
              {data.iconUrl && (
                <div className={styles.heroBack} aria-hidden>
                  <img className={styles.heroBackImg} src={data.iconUrl} alt="" />
                  <div className={styles.heroBackShade} />
                </div>
              )}
              <div className={styles.iconWrap}>
                {data.iconUrl ? (
                  <img src={data.iconUrl} alt="" className={styles.icon} />
                ) : (
                  <div className={styles.iconFallback} aria-hidden />
                )}
              </div>

              <div className={styles.heroBody}>
                <div className={styles.titleRow}>
                  <div>
                    <h1 className={styles.title}>{data.name}</h1>
                    <p className={styles.author}>by {data.author}</p>
                  </div>
                  {canInstall && (
                    <button
                      type="button"
                      className={styles.installBtn}
                      onClick={() => openInstall()}
                    >
                      {directInstall
                        ? `Add to ${installTarget?.instanceName ?? "instance"}`
                        : installVerb(data.projectType)}
                    </button>
                  )}
                </div>

                <p className={styles.tagline}>{data.description}</p>

                <div className={styles.metaRow}>
                  <SourceBadges sources={data.sources} />
                  <span className={styles.metaItem}>
                    {formatDownloads(data.downloads)} downloads
                  </span>
                  {detail?.gameVersions[0] && (
                    <span className={styles.metaItem}>
                      MC {detail.gameVersions[0]}
                    </span>
                  )}
                  {detail?.loaders[0] && (
                    <span className={styles.metaItem}>{detail.loaders[0]}</span>
                  )}
                  <span className={styles.metaItem}>
                    Updated {formatDate(data.updatedAt)}
                  </span>
                </div>

                {detail && detail.categories.length > 0 && (
                  <div className={styles.tags}>
                    {detail.categories.slice(0, 8).map((tag) => (
                      <span key={tag} className={styles.tag}>
                        {tag}
                      </span>
                    ))}
                  </div>
                )}

                {detail?.externalUrl && (
                  <a
                    className={styles.externalLink}
                    href={detail.externalUrl}
                    target="_blank"
                    rel="noreferrer"
                  >
                    Open on source site
                  </a>
                )}
              </div>
            </header>

            <nav className={styles.tabs} aria-label="Project sections">
              {tabs.map((item) => (
                <button
                  key={item.id}
                  type="button"
                  className={`${styles.tab} ${tab === item.id ? styles.tabActive : ""}`}
                  onClick={() => setTab(item.id)}
                >
                  {item.label}
                </button>
              ))}
            </nav>

            <div className={styles.panel}>
              <div
                className={`${styles.panelInner} ${
                  tab === "content" ? styles.panelInnerWide : ""
                }`}
              >
                {tab === "overview" && (
                  <div className={styles.overview}>
                    {detail?.body ? (
                      <OverviewBody
                        content={detail.body}
                        bodyFormat={detail.bodyFormat}
                      />
                    ) : (
                      <p className={styles.emptyBody}>
                        {data.description || "No description available."}
                      </p>
                    )}
                  </div>
                )}

                {tab === "content" && detail && isModpack && (
                  <div className={styles.contentTab}>
                    {detail.versions.length > 1 && (
                      <label className={styles.versionPicker}>
                        <span>Version</span>
                        <select
                          value={contentVersionId ?? ""}
                          onChange={(e) => setContentVersionId(e.target.value)}
                        >
                          {detail.versions.map((version) => (
                            <option key={version.id} value={version.id}>
                              {version.name}
                            </option>
                          ))}
                        </select>
                      </label>
                    )}
                    <ModpackContentTab
                      detail={detail}
                      selectedVersionId={contentVersionId}
                    />
                  </div>
                )}

                {tab === "changelog" && detail && (
                  <div className={styles.changelogTab}>
                    <label className={styles.versionPicker}>
                      <span>Version</span>
                      <select
                        value={changelogVersionId ?? ""}
                        onChange={(e) => setChangelogVersionId(e.target.value)}
                      >
                        {detail.versions.map((version) => (
                          <option key={version.id} value={version.id}>
                            {version.name}
                          </option>
                        ))}
                      </select>
                    </label>
                    {changelogLoading && (
                      <p className={styles.emptyBody}>Loading changelog…</p>
                    )}
                    {!changelogLoading && changelogBody && (
                      <MarkdownBody content={changelogBody} />
                    )}
                    {!changelogLoading && !changelogBody && (
                      <p className={styles.emptyBody}>
                        No changelog for this version.
                      </p>
                    )}
                  </div>
                )}

                {tab === "gallery" &&
                  detail &&
                  (detail.gallery ?? []).length > 0 && (
                    <div className={styles.galleryGrid}>
                      {(detail.gallery ?? []).map((item, index) => (
                        <figure
                          key={`${item.url}-${index}`}
                          className={styles.galleryItem}
                        >
                          <a href={item.url} target="_blank" rel="noreferrer">
                            <img
                              src={item.thumbnailUrl ?? item.url}
                              alt={item.title ?? "Screenshot"}
                              loading="lazy"
                            />
                          </a>
                          {(item.title || item.description) && (
                            <figcaption>
                              {item.title && <strong>{item.title}</strong>}
                              {item.description && <p>{item.description}</p>}
                            </figcaption>
                          )}
                        </figure>
                      ))}
                    </div>
                  )}

                {tab === "versions" && (
                  <div className={styles.versionList}>
                    {!detail?.versions.length && (
                      <p className={styles.emptyBody}>
                        No version list available.
                      </p>
                    )}
                    {detail?.versions.map((version) => {
                      const mc =
                        version.gameVersions[0] ??
                        detail.suggestedInstance.minecraftVersion;
                      const loader = pickLoader(
                        version.loaders,
                        detail.suggestedInstance.loader,
                      );
                      return (
                        <article key={version.id} className={styles.versionRow}>
                          <div className={styles.versionMain}>
                            <h3 className={styles.versionName}>
                              {version.name}
                            </h3>
                            <p className={styles.versionMeta}>
                              {version.gameVersions.join(", ") || "—"}
                              {version.loaders.length > 0 &&
                                ` · ${version.loaders.join(", ")}`}
                            </p>
                            <p className={styles.versionStatsInline}>
                              {formatDownloads(version.downloads)} downloads ·{" "}
                              {formatDate(version.publishedAt)}
                            </p>
                          </div>
                          {canInstall && (
                            <button
                              type="button"
                              className={styles.versionInstallBtn}
                              onClick={() =>
                                openInstall({
                                  versionId: version.id,
                                  minecraftVersion: mc,
                                  loader,
                                })
                              }
                            >
                              {directInstall ? "Add" : "Install"}
                            </button>
                          )}
                        </article>
                      );
                    })}
                  </div>
                )}

                {tab === "comments" && detail?.commentsUrl && (
                  <div className={styles.commentsTab}>
                    <p className={styles.emptyBody}>
                      Comments are hosted on the source site. Open the page
                      below to read and post comments.
                    </p>
                    <a
                      className={styles.externalLink}
                      href={detail.commentsUrl}
                      target="_blank"
                      rel="noreferrer"
                    >
                      View comments on source site
                    </a>
                  </div>
                )}

                {tab === "logs" && (
                  <div className={styles.logsTab}>
                    {projectLogs.length === 0 && (
                      <p className={styles.emptyBody}>
                        No Waybound activity logged for this project yet.
                        Install events will appear here.
                      </p>
                    )}
                    <ul className={styles.logList}>
                      {projectLogs.map((entry) => (
                        <li
                          key={`${entry.timestamp}-${entry.message}`}
                          className={styles.logRow}
                        >
                          <span className={styles.logTime}>
                            {new Date(entry.timestamp * 1000).toLocaleString()}
                          </span>
                          <span className={styles.logLevel}>{entry.level}</span>
                          <span>{entry.message}</span>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </div>
            </div>
          </>
        )}
      </div>

      {installOpen && detail && (
        <InstallTargetDialog
          detail={detail}
          versionPrefill={versionPrefill}
          fixedInstanceId={installTarget?.instanceId}
          onClose={() => {
            setInstallOpen(false);
            setVersionPrefill(undefined);
          }}
          onSuccess={(message) => {
            showToast(message);
            if (installTarget && onReturnToInstance) {
              onReturnToInstance();
            }
          }}
        />
      )}
    </div>
  );
}
