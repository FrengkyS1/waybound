import { useEffect, useRef } from "react";
import styles from "./SearchBar.module.css";
import { useBrowseStore } from "../store/browseStore";

export function SearchBar() {
  const query = useBrowseStore((s) => s.query);
  const loading = useBrowseStore((s) => s.loading);
  const setQuery = useBrowseStore((s) => s.setQuery);
  const search = useBrowseStore((s) => s.search);

  // Live search-as-you-type; skip the mount run (BrowsePage already searches
  // on mount) and keep the submit button as a manual "search now".
  const mounted = useRef(false);
  useEffect(() => {
    if (!mounted.current) {
      mounted.current = true;
      return;
    }
    const timer = setTimeout(() => void search(true), 300);
    return () => clearTimeout(timer);
  }, [query, search]);

  return (
    <form
      className={styles.bar}
      onSubmit={(event) => {
        event.preventDefault();
        void search(true);
      }}
    >
      <input
        type="search"
        className={styles.input}
        placeholder="Search mods…"
        value={query}
        onChange={(event) => setQuery(event.currentTarget.value)}
        aria-label="Search mods"
      />
      <button type="submit" className={styles.button} disabled={loading}>
        {loading ? "Searching…" : "Search"}
      </button>
    </form>
  );
}
