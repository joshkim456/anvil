import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import {
  api,
  Instance,
  OriginEntry,
  OriginsView as OriginsData,
  Quest,
  QuestGraph,
} from "../lib/api";
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

    // Primary root of this chapter — the quest the Heracles bbox-centroid
    // camera (and our reset view) should open on. Mirrors layout_chapter()
    // in quest.rs exactly: intra-chapter in-degree 0, then largest forward
    // closure, then lexicographically smallest id. Deriving it the same way
    // keeps the in-app viewer pointed at the same quest the game opens on,
    // for every chapter of every instance — no special-casing.
    const ids = new Set(quests.map((q) => q.id));
    const succ = new Map<string, string[]>();
    const indeg = new Map<string, number>();
    for (const q of quests) indeg.set(q.id, 0);
    for (const q of quests)
      for (const d of q.deps)
        if (ids.has(d)) {
          if (!succ.has(d)) succ.set(d, []);
          succ.get(d)!.push(q.id);
          indeg.set(q.id, (indeg.get(q.id) ?? 0) + 1);
        }
    let roots = [...ids].filter((id) => (indeg.get(id) ?? 0) === 0).sort();
    if (roots.length === 0) roots = [...ids].sort().slice(0, 1); // cyclic
    const closure = (start: string): number => {
      const seen = new Set([start]);
      const stack = [start];
      while (stack.length) {
        const u = stack.pop()!;
        for (const c of succ.get(u) ?? [])
          if (!seen.has(c)) {
            seen.add(c);
            stack.push(c);
          }
      }
      return seen.size;
    };
    // roots is sorted ascending; only replacing on a *strictly* larger
    // closure makes the lex-smallest of the max-closure roots win the tie.
    let rootId: string | null = null;
    let best = -1;
    for (const r of roots) {
      const c = closure(r);
      if (c > best) {
        best = c;
        rootId = r;
      }
    }

    return {
      pos,
      rootId,
      contentW,
      contentH,
      width: Math.max(contentW + 80, 320),
      height: Math.max(contentH + 80, 200),
    };
  }, [chapter]);

  const viewportRef = useRef<HTMLDivElement | null>(null);
  const [view, setView] = useState({ x: 28, y: 20, z: 1 });

  // Open each chapter centered on its root quest — the same quest the
  // Heracles in-game camera lands on — instead of on the content's
  // bounding box. The old bbox math clamped to a 24px min offset, which
  // for any chapter wider than the viewport pinned the whole graph to the
  // left edge and made the folded tail (e.g. "Iron Shell") read as first.
  // The canvas transform is `translate(x,y) scale(z)` with origin (0,0),
  // so a canvas-local point p maps to screen `view + p*z`; solve for the
  // offset that puts the root card's center at the viewport center.
  function centerView() {
    const vp = viewportRef.current;
    const vw = vp?.clientWidth ?? 0;
    const vh = vp?.clientHeight ?? 0;
    const z = 1;
    const root = layout.rootId ? layout.pos.get(layout.rootId) : undefined;
    if (root) {
      const cx = root.x + CARD_W / 2;
      const cy = root.y + CARD_H / 2;
      setView({
        x: Math.round(vw / 2 - cx * z),
        y: Math.round(vh / 2 - cy * z),
        z,
      });
      return;
    }
    // Empty/degenerate chapter: fall back to centering the bounding box.
    setView({
      x: Math.max(24, Math.round((vw - layout.contentW) / 2)),
      y: Math.max(20, Math.round((vh - layout.contentH) / 2)),
      z,
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
            transform: `translate3d(${view.x}px, ${view.y}px, 0) scale(${view.z})`,
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

/** The raw (un-prettified) result item id of a recipe, for icon lookup. */
function rawResultId(r: Record<string, unknown>): string {
  const res = r.result;
  if (typeof res === "string") return res;
  if (res && typeof res === "object") {
    const item = (res as Record<string, unknown>).item;
    if (typeof item === "string") return item;
  }
  return "";
}

type Ing = { item?: string; tag?: string };

// Module-level cache so the same item id is fetched once across every slot,
// recipe, and drawer open (the Rust side also disk-caches; this avoids even
// the round-trip). Value: data URL, or null = unresolvable (labeled slot).
const iconCache = new Map<string, string | null>();

/** `undefined` = loading, `null` = show a labeled slot, string = data URL. */
function useItemIcon(
  instanceId: string,
  itemId: string | null,
): string | null | undefined {
  const key = `${instanceId}|${itemId}`;
  const [val, setVal] = useState<string | null | undefined>(
    itemId == null
      ? null
      : iconCache.has(key)
        ? iconCache.get(key)
        : undefined,
  );
  useEffect(() => {
    if (itemId == null) {
      setVal(null);
      return;
    }
    if (iconCache.has(key)) {
      setVal(iconCache.get(key));
      return;
    }
    let alive = true;
    api
      .getItemIcon(instanceId, itemId)
      .then((u) => {
        iconCache.set(key, u ?? null);
        if (alive) setVal(u ?? null);
      })
      .catch(() => {
        iconCache.set(key, null);
        if (alive) setVal(null);
      });
    return () => {
      alive = false;
    };
  }, [key, instanceId, itemId]);
  return val;
}

/** Renders a resolved icon. Minecraft animated textures are a tall vertical
 *  film-strip (square frames stacked); we measure the data URL and, if it is
 *  a strip, play it as a stepped sprite animation instead of squishing every
 *  frame into one slot. Static textures render as a plain fitted image. */
function SlotImg({ url, alt }: { url: string; alt: string }) {
  const [frames, setFrames] = useState(1);
  useEffect(() => {
    let alive = true;
    const im = new Image();
    im.onload = () => {
      if (!alive) return;
      const w = im.naturalWidth;
      const h = im.naturalHeight;
      // A vertical strip of >=2 square frames => animated.
      setFrames(w > 0 && h > w && h % w === 0 ? h / w : 1);
    };
    im.src = url;
    return () => {
      alive = false;
    };
  }, [url]);

  if (frames > 1) {
    return (
      <span
        className="recipe-slot-img anim"
        role="img"
        aria-label={alt}
        style={{
          backgroundImage: `url("${url}")`,
          animationDuration: `${Math.max(0.6, frames * 0.12)}s`,
          animationTimingFunction: `steps(${frames})`,
        }}
      />
    );
  }
  return <img className="recipe-slot-img" src={url} alt={alt} />;
}

/** One crafting slot: a real item icon, or a labeled fallback (a tag, a
 *  vanilla item without downloaded assets, or a geometry-only/builtin 3D
 *  model with no flat sprite). Reuses `prettyId` for the label — no third
 *  prettifier. */
function RecipeSlot({
  instanceId,
  ing,
}: {
  instanceId: string;
  ing: Ing | null;
}) {
  const isTag = !!ing?.tag;
  const itemId = ing?.item ?? null;
  const icon = useItemIcon(instanceId, isTag ? null : itemId);
  if (!ing) return <div className="recipe-slot empty" />;
  const full = ing.tag ? `Tag: ${ing.tag}` : prettyId(itemId ?? "");
  const short = ing.tag
    ? `#${ing.tag.split(/[:/]/).pop() ?? ing.tag}`
    : (itemId ?? "").split(":").pop()?.replace(/_/g, " ") ?? "";
  return (
    <div className="recipe-slot" title={full}>
      {icon ? (
        <SlotImg url={icon} alt={full} />
      ) : (
        <span className="recipe-slot-label">{short}</span>
      )}
    </div>
  );
}

/** Deterministic 3x3 crafting grid, rendered from the real RecipeDef (never
 *  the model's prose). Shaped uses pattern+key; shapeless fills in order;
 *  smelting is input then result. */
function CraftingGrid({
  instanceId,
  recipe,
}: {
  instanceId: string;
  recipe: Record<string, unknown>;
}) {
  const type = String(recipe.type ?? "recipe");
  const cells: (Ing | null)[] = Array(9).fill(null);
  if (type === "shaped") {
    const pattern = (recipe.pattern as string[] | undefined) ?? [];
    const key = (recipe.key as Record<string, Ing> | undefined) ?? {};
    for (let r = 0; r < Math.min(3, pattern.length); r++) {
      const rowStr = pattern[r] ?? "";
      for (let c = 0; c < Math.min(3, rowStr.length); c++) {
        const ch = rowStr[c];
        if (ch && ch !== " ") cells[r * 3 + c] = key[ch] ?? null;
      }
    }
  } else if (type === "shapeless") {
    const ings = (recipe.ingredients as Ing[] | undefined) ?? [];
    ings.slice(0, 9).forEach((g, i) => (cells[i] = g));
  } else if (type === "smelting") {
    cells[0] = (recipe.ingredient as Ing | undefined) ?? null;
  }
  const result = rawResultId(recipe);
  return (
    <div className="recipe-craft">
      <div className="recipe-grid" aria-label={`${type} recipe`}>
        {cells.map((g, i) => (
          <RecipeSlot key={i} instanceId={instanceId} ing={g} />
        ))}
      </div>
      <span className="recipe-arrow" aria-hidden>
        →
      </span>
      <RecipeSlot
        instanceId={instanceId}
        ing={result ? { item: result } : null}
      />
    </div>
  );
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
  instanceId,
  onClose,
}: {
  open: boolean;
  quest: Quest | null;
  chapter: string;
  questTitles: Record<string, string>;
  instanceId: string;
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
                    <div className="recipe-block" key={i}>
                      <div className="quest-drawer-row">
                        <span className="quest-drawer-row-type">
                          {titleCase(kind)}
                        </span>
                        <span className="quest-drawer-row-detail">
                          Bridges to {recipeResult(row)}
                        </span>
                      </div>
                      <CraftingGrid instanceId={instanceId} recipe={row} />
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

/** The origin's emblem: the real item icon (reuses the shared `useItemIcon`
 *  fetcher + `SlotImg` strip-aware renderer), or a short labeled fallback
 *  while loading / when the id can't be resolved. Mirrors RecipeSlot's
 *  fallback (last `:` segment, underscores out) — no new prettifier. Sized
 *  by the caller so the same component serves the rail (small) and the
 *  detail header (large). */
function OriginEmblem({
  instanceId,
  itemId,
  size,
}: {
  instanceId: string;
  itemId: string;
  size: number;
}) {
  const icon = useItemIcon(instanceId, itemId);
  const full = prettyId(itemId);
  const short =
    itemId.split(":").pop()?.replace(/_/g, " ") ?? itemId;
  return (
    <div
      className="origins-emblem"
      style={{ width: size, height: size }}
      title={full}
    >
      {icon ? (
        <SlotImg url={icon} alt={full} />
      ) : (
        <span className="recipe-slot-label">{short}</span>
      )}
    </div>
  );
}

/** A 3-pip impact indicator. `impact` is clamped to [0,3] so an out-of-range
 *  value from the backend never overflows the row of pips. */
function ImpactPips({ impact }: { impact: number }) {
  const n = Math.max(0, Math.min(3, Math.round(impact)));
  return (
    <span className="origins-impact" aria-label={`Impact ${n} of 3`}>
      {[0, 1, 2].map((i) => (
        <span
          key={i}
          className={"origins-pip" + (i < n ? " on" : "")}
          aria-hidden
        />
      ))}
    </span>
  );
}

const IMPACT_WORD = ["Minimal", "Low", "Moderate", "High"] as const;

/** The Origins viewer (Pattern A): a scrollable rail of origins on the left,
 *  a read-only detail panel on the right. Faithful to the in-game Origins
 *  screen — emblem, title, impact, description, then a Powers list with
 *  shipped powers visually distinguished. No Select button (read-only). */
function OriginsViewer({
  instanceId,
  origins,
}: {
  instanceId: string;
  origins: OriginEntry[];
}) {
  const [idx, setIdx] = useState(0);
  // The instance (and so the origins array) can change under us — clamp the
  // selection back into range instead of rendering an undefined origin.
  useEffect(() => setIdx(0), [origins]);
  const sel = origins[Math.min(idx, origins.length - 1)] ?? origins[0];
  const impact = Math.max(0, Math.min(3, Math.round(sel.impact)));

  return (
    <div className="origins-view">
      <div className="origins-rail" role="tablist" aria-label="Origins">
        {origins.map((o, i) => {
          const active = o === sel;
          return (
            <button
              key={o.id}
              type="button"
              role="tab"
              aria-selected={active}
              className={"origins-row" + (active ? " active" : "")}
              onClick={() => setIdx(i)}
            >
              <OriginEmblem
                instanceId={instanceId}
                itemId={o.icon}
                size={32}
              />
              <span className="origins-row-name">{o.name}</span>
              <ImpactPips impact={o.impact} />
            </button>
          );
        })}
      </div>

      <div className="origins-detail">
        <div className="origins-detail-head">
          <OriginEmblem
            instanceId={instanceId}
            itemId={sel.icon}
            size={80}
          />
          <div className="origins-detail-heading">
            <h3 className="origins-detail-title">{sel.name}</h3>
            <span className="origins-detail-impact">
              Impact: <ImpactPips impact={sel.impact} />{" "}
              {IMPACT_WORD[impact]}
            </span>
          </div>
        </div>

        {sel.description && (
          <p className="origins-detail-desc">{sel.description}</p>
        )}

        <div className="origins-powers">
          <span className="quest-drawer-label">
            Powers · {sel.powers.length}
          </span>
          {sel.powers.length === 0 ? (
            <div className="card-hint">No powers.</div>
          ) : (
            sel.powers.map((p, i) => (
              <div
                className={
                  "origins-power" + (p.shipped ? " shipped" : "")
                }
                key={i}
              >
                <div className="origins-power-head">
                  <span className="origins-power-name">{p.name}</span>
                  {p.shipped && (
                    <span className="origins-power-badge">shipped</span>
                  )}
                </div>
                {p.description && (
                  <p className="origins-power-desc">{p.description}</p>
                )}
              </div>
            ))
          )}
        </div>
      </div>
    </div>
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
  // Origins live alongside the quest graph on the same instance pick. Their
  // own fetch state mirrors the graph's; `origins === null` (or absent) means
  // this instance has no Anvil origins, so the Origins tab must not appear.
  const [origins, setOrigins] = useState<OriginsData | null>(null);
  const [tab, setTab] = useState<"quests" | "origins">("quests");

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

  // Origins fetch — independent of the graph fetch, same pick trigger.
  // On every pick change we drop the Origins tab back to Quests; if the new
  // instance has origins the user can switch to it, and if it doesn't the
  // forced reset means we never strand the user on a now-empty Origins tab.
  useEffect(() => {
    if (!pick) return;
    let alive = true;
    setTab("quests");
    setOrigins(null);
    api
      .getOrigins(pick)
      .then((o) => {
        if (!alive) return;
        setOrigins(o);
      })
      .catch(() => {
        if (alive) setOrigins(null);
      });
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

          {origins && origins.origins.length > 0 && (
            <div className="quest-tabs" role="tablist">
              <button
                role="tab"
                aria-selected={tab === "quests"}
                className={"quest-tab" + (tab === "quests" ? " active" : "")}
                onClick={() => setTab("quests")}
              >
                Quests
              </button>
              <button
                role="tab"
                aria-selected={tab === "origins"}
                className={"quest-tab" + (tab === "origins" ? " active" : "")}
                onClick={() => setTab("origins")}
              >
                Origins
                <span className="quest-tab-count">
                  {origins.origins.length}
                </span>
              </button>
            </div>
          )}

          {tab === "quests" && (
            <>
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
                    onSelect={(quest, chapter) =>
                      setSel({ quest, chapter })
                    }
                  />
                </div>
              )}
            </>
          )}

          {tab === "origins" && origins && origins.origins.length > 0 && (
            <div className="quest-graph">
              <OriginsViewer
                instanceId={pick}
                origins={origins.origins}
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
        instanceId={pick}
        onClose={() => setSel(null)}
      />
    </>
  );
}
