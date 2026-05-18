import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { api, Instance, Quest, QuestGraph } from "../lib/api";
import { Dropdown, Opt } from "../components/Dropdown";

// Compact cards: readable title (2 lines) + a tiny task count, nothing more.
const CARD_W = 170;
const CARD_H = 74;
// Minimum empty space we want between adjacent cards on each axis.
const GAP_X = 44;
const GAP_Y = 30;
// Fallback unit→px scale when a chapter has too few nodes to derive one.
const FALLBACK_SCALE = 110;

/** Smallest positive gap between distinct sorted values, or null if <2. The
 *  curator is only told to use "about 2.0" spacing, so the real unit step is
 *  whatever it actually emitted: derive px scale from the data, never assume. */
function minDelta(values: number[]): number | null {
  const uniq = [...new Set(values)].sort((a, b) => a - b);
  if (uniq.length < 2) return null;
  let min = Infinity;
  for (let i = 1; i < uniq.length; i++) {
    const d = uniq[i] - uniq[i - 1];
    if (d > 0 && d < min) min = d;
  }
  return Number.isFinite(min) ? min : null;
}

interface Placed {
  q: Quest;
  x: number;
  y: number;
}

/** Per-chapter view: tabs across the top, one chapter's quests on a pannable,
 *  zoomable plane below. One chapter at a time keeps each map small and
 *  readable instead of one giant stacked canvas. */
function QuestCanvas({
  graph,
  selectedId,
  onSelect,
}: {
  graph: QuestGraph;
  selectedId: string | null;
  onSelect: (q: Quest, chapter: string) => void;
}) {
  const [chapterIdx, setChapterIdx] = useState(0);
  // Reset to the first chapter when the questline changes.
  useEffect(() => setChapterIdx(0), [graph]);
  const idx = Math.min(chapterIdx, Math.max(0, graph.chapters.length - 1));
  const chapter = graph.chapters[idx];

  // Lay out just this chapter on its own plane (no band offsets).
  const layout = useMemo(() => {
    const quests = chapter?.quests ?? [];
    const dx = minDelta(quests.map((q) => q.x));
    const dy = minDelta(quests.map((q) => q.y));
    const xScale = dx ? (CARD_W + GAP_X) / dx : FALLBACK_SCALE;
    const yScale = dy ? (CARD_H + GAP_Y) / dy : FALLBACK_SCALE;

    // The model doesn't start every chapter at x=0/y=0, so normalise each
    // chapter to its own origin — otherwise chapters whose coords are offset
    // render far off-screen and look empty.
    const minX = quests.length ? Math.min(...quests.map((q) => q.x)) : 0;
    const minY = quests.length ? Math.min(...quests.map((q) => q.y)) : 0;

    const pos = new Map<string, Placed>();
    let contentW = 0;
    let contentH = 0;
    for (const q of quests) {
      const x = (q.x - minX) * xScale;
      const y = (q.y - minY) * yScale;
      pos.set(q.id, { q, x, y });
      contentW = Math.max(contentW, x + CARD_W);
      contentH = Math.max(contentH, y + CARD_H);
    }
    return {
      pos,
      contentW,
      contentH,
      width: Math.max(contentW + 80, 320),
      height: Math.max(contentH + 80, 200),
    };
  }, [chapter]);

  const viewportRef = useRef<HTMLDivElement | null>(null);
  const [view, setView] = useState({ x: 28, y: 20, z: 1 });

  // Center this chapter's content in the viewport whenever the chapter
  // changes (and on first mount once the viewport has a size).
  function centerView() {
    const vp = viewportRef.current;
    const vw = vp?.clientWidth ?? 0;
    const vh = vp?.clientHeight ?? 0;
    setView({
      x: Math.max(24, Math.round((vw - layout.contentW) / 2)),
      y: Math.max(20, Math.round((vh - layout.contentH) / 2)),
      z: 1,
    });
  }
  useEffect(() => {
    centerView();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chapter, layout]);

  const drag = useRef<{
    px: number;
    py: number;
    vx: number;
    vy: number;
  } | null>(null);
  // True once a press has moved past the click threshold, so the node's
  // onClick can tell a pan apart from a real selection.
  const moved = useRef(false);

  function onPointerDown(e: ReactPointerEvent) {
    // Always clear the pan flag on a fresh press — otherwise a stale `true`
    // from a previous pan suppresses the very next node click and the
    // detail drawer never opens.
    moved.current = false;
    if ((e.target as HTMLElement).closest(".quest-node")) return;
    drag.current = { px: e.clientX, py: e.clientY, vx: view.x, vy: view.y };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function onPointerMove(e: ReactPointerEvent) {
    const d = drag.current;
    if (!d) return;
    const dx = e.clientX - d.px;
    const dy = e.clientY - d.py;
    if (Math.abs(dx) + Math.abs(dy) > 4) moved.current = true;
    setView((v) => ({ ...v, x: d.vx + dx, y: d.vy + dy }));
  }
  function endPan(e: ReactPointerEvent) {
    drag.current = null;
    const el = e.currentTarget as HTMLElement;
    if (el.hasPointerCapture?.(e.pointerId))
      el.releasePointerCapture(e.pointerId);
  }
  function onWheel(e: ReactWheelEvent) {
    e.preventDefault();
    setView((v) => {
      const z = Math.min(1.6, Math.max(0.4, v.z * (e.deltaY < 0 ? 1.1 : 0.9)));
      return { ...v, z };
    });
  }

  const chapterTitle = chapter?.title ?? "";

  return (
    <div className="quest-stage">
      <div className="quest-tabs" role="tablist">
        {graph.chapters.map((c, i) => (
          <button
            key={c.id}
            role="tab"
            aria-selected={i === idx}
            className={"quest-tab" + (i === idx ? " active" : "")}
            onClick={() => setChapterIdx(i)}
          >
            {c.title}
            <span className="quest-tab-count">{c.quests.length}</span>
          </button>
        ))}
      </div>
      <div className="quest-toolbar">
        <button className="btn ghost" onClick={centerView}>
          Reset view
        </button>
        <span className="quest-hint">Drag to pan · scroll to zoom</span>
      </div>
      <div
        className="quest-viewport"
        ref={viewportRef}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endPan}
        onPointerCancel={endPan}
        onWheel={onWheel}
      >
        <div
          className="quest-canvas"
          style={{
            width: layout.width,
            height: layout.height,
            transform: `translate(${view.x}px, ${view.y}px) scale(${view.z})`,
          }}
        >
          <svg
            className="quest-links"
            width={layout.width}
            height={layout.height}
            aria-hidden
          >
            {(chapter?.quests ?? []).flatMap((q) => {
              const to = layout.pos.get(q.id);
              if (!to) return [];
              // Only draw edges inside this chapter; cross-chapter
              // prerequisites are listed in the quest drawer instead.
              return q.deps.flatMap((d) => {
                const from = layout.pos.get(d);
                if (!from) return [];
                const x1 = from.x + CARD_W;
                const y1 = from.y + CARD_H / 2;
                const x2 = to.x;
                const y2 = to.y + CARD_H / 2;
                const mx = (x1 + x2) / 2;
                return [
                  <path
                    key={`${d}->${q.id}`}
                    d={`M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`}
                    fill="none"
                    stroke="var(--accent)"
                    strokeWidth={1.5}
                    opacity={0.7}
                  />,
                ];
              });
            })}
          </svg>

          {[...layout.pos.values()].map(({ q, x, y }) => (
            <button
              key={q.id}
              type="button"
              className={
                "quest-node" + (q.id === selectedId ? " selected" : "")
              }
              style={{ left: x, top: y, width: CARD_W, height: CARD_H }}
              onClick={() => {
                if (moved.current) return; // was a pan, not a click
                onSelect(q, chapterTitle);
              }}
            >
              <span className="quest-node-title">{q.title}</span>
              <span className="quest-node-sub">
                {q.tasks.length} {q.tasks.length === 1 ? "task" : "tasks"}
                {q.content && (
                  <span className="quest-node-badge">Boss</span>
                )}
                {(q.recipes?.length ?? 0) > 0 && (
                  <span className="quest-node-badge">Recipe</span>
                )}
              </span>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

/** "minecraft:bone_meal" -> "Bone Meal"; "create:cogwheel" -> "Cogwheel
 *  (Create)". Strips the vanilla namespace, words-out the path, Title Cases. */
function titleCase(s: string): string {
  return s
    .replace(/[_./]+/g, " ")
    .trim()
    .split(/\s+/)
    .map((w) => (w ? w[0].toUpperCase() + w.slice(1) : w))
    .join(" ");
}

function prettyId(raw: string): string {
  const [ns, path] = raw.includes(":")
    ? raw.split(":", 2)
    : ["minecraft", raw];
  const name = titleCase(path);
  return ns && ns !== "minecraft" ? `${name} (${titleCase(ns)})` : name;
}

/** The bridged result item of a recipe-facet entry (Slice 2). Shaped/shapeless
 *  results are an object {item,count}; smelting result is a plain string. We
 *  only need the human label here, so be defensive about the loose shape. */
function recipeResult(r: Record<string, unknown>): string {
  const res = r.result;
  if (typeof res === "string") return prettyId(res);
  if (res && typeof res === "object") {
    const item = (res as Record<string, unknown>).item;
    if (typeof item === "string") return prettyId(item);
  }
  return "?";
}

/** Tasks/rewards are loosely-typed bags from the quest IR. Turn each known
 *  variant into a plain-English chip + line; unknown shapes fall back to a
 *  readable key/value dump so future variants never render as nothing. */
function describeEntry(
  row: Record<string, unknown>,
  isReward: boolean,
): { tag: string; detail: string } {
  const type = String(row.type ?? (isReward ? "reward" : "task"));
  const withQty = (name: string, n: unknown) => {
    const c = Number(n);
    return Number.isFinite(c) && c > 1 ? `${name} ×${c}` : name;
  };
  switch (type) {
    case "item":
      return {
        tag: isReward ? "Item" : "Collect",
        detail: withQty(prettyId(String(row.id ?? "")), row.count),
      };
    case "gather_item":
      return {
        tag: "Collect",
        detail: withQty(prettyId(String(row.item ?? "")), row.count),
      };
    case "kill":
      return {
        tag: "Defeat",
        detail: withQty(prettyId(String(row.entity_type ?? "")), row.count),
      };
    case "advancement":
      return {
        tag: "Advancement",
        detail: prettyId(String(row.id ?? "")),
      };
    case "biome":
      return { tag: "Explore", detail: prettyId(String(row.biome ?? "")) };
    case "dimension":
      return { tag: "Travel", detail: prettyId(String(row.dimension ?? "")) };
    case "structure":
      return { tag: "Find", detail: prettyId(String(row.structure ?? "")) };
    case "recipe":
      return { tag: "Craft", detail: prettyId(String(row.recipe ?? "")) };
    case "composite": {
      const n = Array.isArray(row.tasks) ? row.tasks.length : 0;
      return { tag: "All of", detail: `${n} sub-task${n === 1 ? "" : "s"}` };
    }
    case "stat":
      return {
        tag: "Stat",
        detail: `${prettyId(String(row.stat ?? ""))} → ${row.target ?? 0}`,
      };
    case "location": {
      const where = [row.dimension, row.biome, row.structure]
        .filter((v) => v != null && v !== "")
        .map((v) => prettyId(String(v)))
        .join(" · ");
      return { tag: "Be at", detail: where };
    }
    case "checkmark":
      return { tag: "Check", detail: "Manual / narrative beat" };
    case "xp":
      return { tag: "XP", detail: `${row.amount ?? 0} XP` };
    case "command":
      return { tag: "Command", detail: String(row.command ?? "") };
    default: {
      const detail = Object.entries(row)
        .filter(([k, v]) => k !== "type" && v != null && typeof v !== "object")
        .map(([k, v]) => `${k} ${v}`)
        .join(" · ");
      return { tag: titleCase(type), detail };
    }
  }
}

function QuestDrawer({
  open,
  quest,
  chapter,
  questTitles,
  onClose,
}: {
  open: boolean;
  quest: Quest | null;
  chapter: string;
  questTitles: Record<string, string>;
  onClose: () => void;
}) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  return (
    <>
      <div
        className={"quest-scrim" + (open ? " open" : "")}
        onClick={onClose}
        aria-hidden
      />
      <aside
        className={"quest-drawer" + (open ? " open" : "")}
        aria-hidden={!open}
      >
        {quest && (
          <>
            <div className="quest-drawer-head">
              <span className="quest-drawer-chapter">{chapter}</span>
              <button
                className="quest-drawer-close"
                onClick={onClose}
                aria-label="Close"
              >
                ✕
              </button>
            </div>
            <h3 className="quest-drawer-title">{quest.title}</h3>
            {quest.description && (
              <p className="quest-drawer-desc">{quest.description}</p>
            )}

            <div className="quest-drawer-section">
              <span className="quest-drawer-label">
                Tasks · {quest.tasks.length}
              </span>
              {quest.tasks.length === 0 ? (
                <div className="card-hint">No tasks.</div>
              ) : (
                quest.tasks.map((t, i) => {
                  const { tag, detail } = describeEntry(
                    t as Record<string, unknown>,
                    false,
                  );
                  return (
                    <div className="quest-drawer-row" key={i}>
                      <span className="quest-drawer-row-type">{tag}</span>
                      {detail && (
                        <span className="quest-drawer-row-detail">
                          {detail}
                        </span>
                      )}
                    </div>
                  );
                })
              )}
            </div>

            {quest.content && (
              <div className="quest-drawer-section">
                <span className="quest-drawer-label">Encounter</span>
                <div className="quest-drawer-row">
                  <span className="quest-drawer-row-type">Boss</span>
                  <span className="quest-drawer-row-detail">
                    {String(
                      quest.content.display_name ??
                        prettyId(String(quest.content.entity ?? "?")),
                    )}
                    {quest.content.entity
                      ? ` (${prettyId(String(quest.content.entity))})`
                      : ""}
                  </span>
                </div>
                <div className="quest-drawer-row">
                  <span className="quest-drawer-row-type">Token</span>
                  <span className="quest-drawer-row-detail">
                    {String(
                      quest.content.token_name ??
                        prettyId(
                          String(
                            quest.content.token_item ??
                              "minecraft:nether_star",
                          ),
                        ),
                    )}
                  </span>
                </div>
              </div>
            )}

            {(quest.recipes?.length ?? 0) > 0 && (
              <div className="quest-drawer-section">
                <span className="quest-drawer-label">
                  Recipes · {quest.recipes?.length ?? 0}
                </span>
                {(quest.recipes ?? []).map((r, i) => {
                  const row = r as Record<string, unknown>;
                  const kind = String(row.type ?? "recipe");
                  return (
                    <div className="quest-drawer-row" key={i}>
                      <span className="quest-drawer-row-type">
                        {titleCase(kind)}
                      </span>
                      <span className="quest-drawer-row-detail">
                        Bridges to {recipeResult(row)}
                      </span>
                    </div>
                  );
                })}
              </div>
            )}

            {quest.rewards.length > 0 && (
              <div className="quest-drawer-section">
                <span className="quest-drawer-label">
                  Rewards · {quest.rewards.length}
                </span>
                {quest.rewards.map((r, i) => {
                  const { tag, detail } = describeEntry(
                    r as Record<string, unknown>,
                    true,
                  );
                  return (
                    <div className="quest-drawer-row" key={i}>
                      <span className="quest-drawer-row-type">{tag}</span>
                      {detail && (
                        <span className="quest-drawer-row-detail">
                          {detail}
                        </span>
                      )}
                    </div>
                  );
                })}
              </div>
            )}

            {quest.deps.length > 0 && (
              <div className="quest-drawer-section">
                <span className="quest-drawer-label">
                  Requires · {quest.deps.length}
                </span>
                {quest.deps.map((d) => (
                  <div className="quest-drawer-row" key={d}>
                    <span className="quest-drawer-row-type">Quest</span>
                    <span className="quest-drawer-row-detail">
                      {questTitles[d] ?? titleCase(d.replace(/^q_/, ""))}
                    </span>
                  </div>
                ))}
              </div>
            )}
          </>
        )}
      </aside>
    </>
  );
}

export default function Quests() {
  const [instances, setInstances] = useState<Instance[]>([]);
  const [pick, setPick] = useState("");
  const [graph, setGraph] = useState<QuestGraph | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sel, setSel] = useState<{ quest: Quest; chapter: string } | null>(
    null,
  );

  useEffect(() => {
    api
      .listInstances()
      .then((list) => {
        setInstances(list);
        if (list.length) setPick(list[0].id);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!pick) return;
    let alive = true;
    setLoading(true);
    setLoaded(false);
    setError(null);
    setSel(null);
    api
      .getQuestGraph(pick)
      .then((g) => {
        if (!alive) return;
        setGraph(g);
        setLoaded(true);
      })
      .catch((e) => {
        if (!alive) return;
        setError(String(e));
      })
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [pick]);

  const instOpts: Opt[] = instances.map((i) => ({
    value: i.id,
    label: i.name,
  }));

  // Resolve dependency node-ids to their quest titles (graph-wide).
  const questTitles = useMemo(() => {
    const m: Record<string, string> = {};
    for (const c of graph?.chapters ?? [])
      for (const q of c.quests) m[q.id] = q.title;
    return m;
  }, [graph]);

  return (
    <>
      <div className="page-head">
        <h2>Quests</h2>
      </div>

      {instances.length === 0 ? (
        <div className="placeholder">
          <strong>No instances yet</strong>
          Create or import an instance, then ask the Curator for a storyline.
        </div>
      ) : (
        <>
          <div className="searchbar">
            <Dropdown value={pick} options={instOpts} onChange={setPick} />
          </div>

          {error && <div className="error">{error}</div>}

          {loading && <div className="spinner">Loading quests.</div>}

          {!loading && loaded && !graph && (
            <div className="placeholder">
              <strong>No quests yet</strong>
              Ask the Curator to add a storyline to this pack.
            </div>
          )}

          {!loading && graph && (
            <div className="quest-graph">
              <h2 className="quest-graph-title">{graph.title}</h2>
              <QuestCanvas
                graph={graph}
                selectedId={sel?.quest.id ?? null}
                onSelect={(quest, chapter) => setSel({ quest, chapter })}
              />
            </div>
          )}
        </>
      )}

      <QuestDrawer
        open={!!sel}
        quest={sel?.quest ?? null}
        chapter={sel?.chapter ?? ""}
        questTitles={questTitles}
        onClose={() => setSel(null)}
      />
    </>
  );
}
