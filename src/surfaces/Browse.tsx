import { useCallback, useEffect, useRef, useState } from "react";
import { api, SearchHit } from "../lib/api";
import ModDetail from "../components/ModDetail";
import { Dropdown, MultiSelect, Opt } from "../components/Dropdown";

const cap = (s: string) => s[0].toUpperCase() + s.slice(1);

const SORT_OPTS: Opt[] = [
  { value: "downloads", label: "Popular" },
  { value: "relevance", label: "Relevant" },
  { value: "follows", label: "Followed" },
  { value: "updated", label: "Updated" },
  { value: "newest", label: "Newest" },
];
const MC_OPTS: Opt[] = [
  { value: "", label: "Any version" },
  ...["1.21.1", "1.21", "1.20.1", "1.19.2", "1.18.2"].map((v) => ({
    value: v,
    label: v,
  })),
];
const LOADER_OPTS: Opt[] = [
  { value: "", label: "Any loader" },
  ...["fabric", "forge", "neoforge", "quilt"].map((v) => ({
    value: v,
    label: cap(v),
  })),
];
const GENRE_OPTS: Opt[] = [
  "adventure",
  "technology",
  "magic",
  "utility",
  "optimization",
  "decoration",
  "food",
  "library",
  "storage",
  "mobs",
  "worldgen",
  "equipment",
].map((c) => ({ value: c, label: cap(c) }));

export default function Browse() {
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [mc, setMc] = useState("");
  const [loader, setLoader] = useState("");
  const [sort, setSort] = useState("downloads");
  const [cats, setCats] = useState<string[]>([]);
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [total, setTotal] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const t = setTimeout(() => setDebounced(query), 350);
    return () => clearTimeout(t);
  }, [query]);

  // Latest filters + paging state for the (long-lived) observer callback.
  const filters = {
    query: debounced,
    mcVersion: mc,
    loader,
    index: sort,
    categories: cats,
  };
  const filtersRef = useRef(filters);
  filtersRef.current = filters;
  const pagingRef = useRef({ count: 0, total: 0, busy: true });
  pagingRef.current = {
    count: hits.length,
    total: total ?? 0,
    busy: loading || loadingMore,
  };

  // Filter change -> reset to the first page.
  useEffect(() => {
    let alive = true;
    setLoading(true);
    setError(null);
    api
      .searchMods({ ...filtersRef.current, offset: 0 })
      .then((res) => {
        if (!alive) return;
        setHits(res.hits);
        setTotal(res.total_hits);
      })
      .catch((err) => {
        if (!alive) return;
        setError(String(err));
        setHits([]);
        setTotal(0);
      })
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [debounced, mc, loader, sort, cats]);

  const loadMore = useCallback(async () => {
    const p = pagingRef.current;
    if (p.busy || (p.total > 0 && p.count >= p.total)) return;
    setLoadingMore(true);
    try {
      const res = await api.searchMods({
        ...filtersRef.current,
        offset: pagingRef.current.count,
      });
      setHits((prev) => {
        const seen = new Set(prev.map((h) => h.project_id));
        return [...prev, ...res.hits.filter((h) => !seen.has(h.project_id))];
      });
      setTotal(res.total_hits);
    } catch {
      // keep what we have; a transient page error shouldn't wipe the list
    } finally {
      setLoadingMore(false);
    }
  }, []);

  // Prefetch the next page as the sentinel nears the scroll viewport.
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el) return;
    const root = el.closest(".main");
    const io = new IntersectionObserver(
      (entries) => {
        if (entries[0].isIntersecting) loadMore();
      },
      { root: root ?? null, rootMargin: "500px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [loadMore]);

  const hasMore = total !== null && hits.length < total;

  return (
    <>
      <div className="page-head">
        <h2>Browse mods</h2>
      </div>

      <div className="searchbar">
        <input
          placeholder="Search mods"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <Dropdown value={sort} options={SORT_OPTS} onChange={setSort} />
        <Dropdown value={mc} options={MC_OPTS} onChange={setMc} />
        <Dropdown value={loader} options={LOADER_OPTS} onChange={setLoader} />
        <MultiSelect
          selected={cats}
          options={GENRE_OPTS}
          onChange={setCats}
          placeholder="Any genre"
        />
      </div>

      {error && <div className="error">Modrinth request failed: {error}</div>}

      {!error && total !== null && (
        <p style={{ color: "var(--muted)", margin: "4px 0 14px" }}>
          {loading
            ? "Loading"
            : `Showing ${hits.length} of ${total.toLocaleString()}`}
        </p>
      )}

      <div className="mod-grid">
        {hits.map((h) => (
          <button
            key={h.project_id}
            className="mod-card"
            onClick={() => setOpen(h.slug || h.project_id)}
          >
            {h.icon_url ? (
              <img src={h.icon_url} alt="" />
            ) : (
              <span className="icon-fallback">
                {h.title.slice(0, 1).toUpperCase()}
              </span>
            )}
            <div style={{ minWidth: 0 }}>
              <h3>{h.title}</h3>
              <div className="by">by {h.author}</div>
              <div className="desc">{h.description}</div>
              <div className="meta">
                <span>↓ {h.downloads.toLocaleString()}</span>
                <span>
                  {h.client_side}/{h.server_side}
                </span>
              </div>
            </div>
          </button>
        ))}
      </div>

      <div ref={sentinelRef} style={{ height: 1 }} />
      {!loading && (loadingMore || hasMore) && (
        <p
          style={{
            color: "var(--muted)",
            textAlign: "center",
            padding: "18px 0 8px",
          }}
        >
          {loadingMore ? "Loading more" : ""}
        </p>
      )}

      {open && <ModDetail idOrSlug={open} onClose={() => setOpen(null)} />}
    </>
  );
}
