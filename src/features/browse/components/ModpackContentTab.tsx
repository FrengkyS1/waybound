import { useEffect, useMemo, useState } from "react";
import { fetchModpackContent } from "../api";
import type {
  ModDetail,
  ModpackContentItem,
  ModpackContentKind,
} from "../detailTypes";
import styles from "./ModpackContentTab.module.css";

type ContentFilter = ModpackContentKind | "all";

const FILTER_LABELS: Record<ContentFilter, string> = {
  all: "All",
  mod: "Mods",
  datapack: "Data Packs",
  resourcepack: "Resource Packs",
  shader: "Shaders",
  world: "Worlds",
  other: "Other",
};

interface ModpackContentTabProps {
  detail: ModDetail;
  selectedVersionId?: string;
}

export function ModpackContentTab({
  detail,
  selectedVersionId,
}: ModpackContentTabProps) {
  const [content, setContent] = useState<Awaited<
    ReturnType<typeof fetchModpackContent>
  > | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<ContentFilter>("all");
  const [search, setSearch] = useState("");

  const versionId = selectedVersionId ?? detail.versions[0]?.id;

  useEffect(() => {
    if (!versionId) {
      setLoading(false);
      return;
    }
    setLoading(true);
    setError(null);
    void fetchModpackContent(detail.summary, versionId)
      .then(setContent)
      .catch((err) =>
        setError(err instanceof Error ? err.message : String(err)),
      )
      .finally(() => setLoading(false));
  }, [detail.summary, versionId]);

  const filters = useMemo((): ContentFilter[] => {
    if (!content) return ["mod"];
    const available: ContentFilter[] = ["all"];
    if (content.counts.mods > 0) available.push("mod");
    if (content.counts.datapacks > 0) available.push("datapack");
    if (content.counts.resourcepacks > 0) available.push("resourcepack");
    if (content.counts.shaders > 0) available.push("shader");
    if (content.counts.worlds > 0) available.push("world");
    if (content.counts.other > 0) available.push("other");
    return available.length > 1 ? available : ["mod"];
  }, [content]);

  const filtered = useMemo(() => {
    if (!content) return [];
    const term = search.trim().toLowerCase();
    return content.items.filter((item) => {
      if (filter !== "all" && item.kind !== filter) return false;
      if (!term) return true;
      return (
        item.name.toLowerCase().includes(term) ||
        item.fileName.toLowerCase().includes(term) ||
        item.author?.toLowerCase().includes(term)
      );
    });
  }, [content, filter, search]);

  if (!versionId) {
    return <p className={styles.empty}>No modpack version available.</p>;
  }

  return (
    <div className={styles.wrap}>
      <div className={styles.toolbar}>
        <div className={styles.filters}>
          {filters.map((kind) => (
            <button
              key={kind}
              type="button"
              className={`${styles.filterBtn} ${filter === kind ? styles.filterBtnActive : ""}`}
              onClick={() => setFilter(kind)}
            >
              {FILTER_LABELS[kind]}
              {content && kind !== "all" && (
                <span className={styles.count}>
                  {countForKind(content.counts, kind)}
                </span>
              )}
              {content && kind === "all" && (
                <span className={styles.count}>{content.items.length}</span>
              )}
            </button>
          ))}
        </div>
        <input
          type="search"
          className={styles.search}
          placeholder="Search content…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
      </div>

      {content && (
        <p className={styles.versionHint}>
          Everything below is installed automatically with the modpack — you
          don't add anything by hand. “Optional” mods can be turned off later in
          the instance.
        </p>
      )}

      {loading && <p className={styles.status}>Loading modpack content…</p>}
      {error && <p className={styles.error}>{error}</p>}

      {!loading && !error && filtered.length === 0 && (
        <p className={styles.empty}>No items match this filter.</p>
      )}

      {!loading && filtered.length > 0 && (
        <div className={styles.tableWrap}>
          <table className={styles.table}>
            <thead>
              <tr>
                <th>Name</th>
                <th>Author</th>
                <th>Env</th>
                <th>Required</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map((item) => (
                <ContentRow key={item.id} item={item} />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function ContentRow({ item }: { item: ModpackContentItem }) {
  return (
    <tr>
      <td>
        <span className={styles.itemName}>{item.name}</span>
        {item.fileName && (
          <span className={styles.fileName}>{item.fileName}</span>
        )}
      </td>
      <td className={styles.author}>{item.author ?? "—"}</td>
      <td className={styles.env}>
        {item.envClient || item.envServer ? (
          <span className={styles.envTags}>
            {item.envClient && <span title="Client">C: {item.envClient}</span>}
            {item.envServer && <span title="Server">S: {item.envServer}</span>}
          </span>
        ) : (
          "—"
        )}
      </td>
      <td>{item.required ? "Yes" : "Optional"}</td>
    </tr>
  );
}

function countForKind(
  counts: {
    mods: number;
    datapacks: number;
    resourcepacks: number;
    shaders: number;
    worlds: number;
    other: number;
  },
  kind: ContentFilter,
): number {
  switch (kind) {
    case "mod":
      return counts.mods;
    case "datapack":
      return counts.datapacks;
    case "resourcepack":
      return counts.resourcepacks;
    case "shader":
      return counts.shaders;
    case "world":
      return counts.worlds;
    case "other":
      return counts.other;
    default:
      return 0;
  }
}
