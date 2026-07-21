import type { ContentType, ModLoader, SortIndex } from "../types";
import { SegmentGroup } from "../../../components/SegmentGroup";
import { useBrowseStore } from "../store/browseStore";
import styles from "./FilterBar.module.css";

const CONTENT_TYPES: { value: ContentType; label: string }[] = [
  { value: "mod", label: "Mods" },
  { value: "modpack", label: "Modpacks" },
  { value: "resourcepack", label: "Packs" },
  { value: "shader", label: "Shaders" },
];

const LOADERS: { value: ModLoader | "any"; label: string }[] = [
  { value: "any", label: "Any" },
  { value: "fabric", label: "Fabric" },
  { value: "forge", label: "Forge" },
  { value: "neoforge", label: "NeoForge" },
  { value: "quilt", label: "Quilt" },
];

const SORTS: { value: SortIndex; label: string }[] = [
  { value: "downloads", label: "Popular" },
  { value: "updated", label: "Updated" },
  { value: "relevance", label: "Relevance" },
  { value: "new", label: "Newest" },
];

export function FilterBar() {
  const contentType = useBrowseStore((s) => s.contentType);
  const loader = useBrowseStore((s) => s.loader);
  const sort = useBrowseStore((s) => s.sort);
  const setContentType = useBrowseStore((s) => s.setContentType);
  const setLoader = useBrowseStore((s) => s.setLoader);
  const setSort = useBrowseStore((s) => s.setSort);
  const search = useBrowseStore((s) => s.search);

  const apply = () => {
    void search(true);
  };

  return (
    <div className={styles.bar}>
      <SegmentGroup
        label="Type"
        value={contentType ?? "mod"}
        options={CONTENT_TYPES}
        onChange={(value) => {
          setContentType(value);
          apply();
        }}
        compact
      />

      <SegmentGroup
        label="Loader"
        value={loader ?? "any"}
        options={LOADERS}
        onChange={(value) => {
          setLoader(value === "any" ? undefined : value);
          apply();
        }}
        compact
      />

      <SegmentGroup
        label="Sort"
        value={sort}
        options={SORTS}
        onChange={(value) => {
          setSort(value);
          apply();
        }}
        compact
      />
    </div>
  );
}
