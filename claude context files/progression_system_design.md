# Anvil — Progression System Design (master)

Status: design of record. Synthesises 8 research streams (3 modpack-genre studies, the
detection/grounding spec, the content+recipe provisioning spec, the node taxonomy, the
recipe-generator design, the engine architecture). Deep technical detail lives in the
companion **`progression_detection_spec.md`** (vanilla trigger taxonomy, predicate shapes,
registry-dump methods, Heracles codec shapes, 1.20.1↔1.21 deltas) — this document is the
blueprint that ties it together and is the contract implementation follows.

Target: **MC 1.20.1** (Fabric/Forge), Modrinth-only. 1.21 deltas flagged where load-bearing.

---

## 0. Thesis (why this exists)

The original failure the user caught: the curator wrote a "defeat Mewtwo in the Spire"
quest where neither the Spire nor a `cobblemon:mewtwo` entity exists — namespace-only
validation let a fabricated id through, and nothing in the game backed the lore.

Two corrections drive everything below:

1. **A quest is real only if its completion is detectable by the running game via
   something that provably exists.** Lore is free text; the *task and its trigger* must be
   grounded and detectable, or the node is an honest manual checkmark.
2. **Custom recipes are not a side feature — they are the progression enforcement layer.**
   Genre research is unambiguous: "expert feel" is ~80% custom recipe-bridging, ~20% quest
   text (Create: Above & Beyond, Enigmatica 6 Expert, GTNH, Nomifactory). Quests *narrate*
   the gate; **recipes *enforce* it.** A progression engine that only writes quests is half
   a system — it produces a guidebook over an unchanged game (this is exactly FTB Academy's
   deliberate "no recipe layer ⇒ no expert feel"). So recipes must be a first-class
   progression element wired into the same graph as quests, not a parallel silo.

End state: the curator converses, designs against the pack's **real registry**, and emits
one progression graph that compiles to **Heracles quests + an Open Loader recipe datapack +
an Anvil vanilla-primitive content datapack** — for *any* genre, with *no per-mod code* —
and Anvil can **prove** before ship that every id is real-or-Anvil-authored, every
objective is detectable-or-honestly-manual, the difficulty curve rises, and the pack
installs clean.

---

## 1. The unified Progression model

One graph (`ProgressionGraph`) — the existing `QuestGraph` chapter/DAG/stable-hex/`groups.txt`
machinery is kept (it works and is well-tested) and generalised. The node becomes a tagged
union of **facets**; quests and recipes share one DAG so a recipe's prerequisites and the
quest that yields its catalyst sit on the same spine.

```
ProgressionGraph { title, chapters: [Chapter] }
Chapter           { id, title, archetype, nodes: [Node] }
Node {
  id, title, x, y, deps:[id],
  lore:   String,                 // narrative — NEVER grounded, never a failure source
  weight: 0..3,                   // progression-aid weight (§5)
  kind:   quest | recipe_unlock | gate | content | narrative | milestone,
  facet:  Quest{tasks,rewards}
        | RecipeUnlock{recipes:[RecipeDef], surface:TaskRef}
        | Gate{spec} | Content{spec} | (narrative/milestone reuse Quest)
}
```

Backward-compatible: every new field is `#[serde(default)]`; a node with no `kind`
decodes as `quest`. `quest.rs` stays the Quest-facet emitter; `recipe.rs` stays the
recipe IR + serializer + validator (reused, not duplicated); a thin orchestrator
dispatches facets and runs the validation contract.

**Why one graph, not four:** gating, pacing, convergence and the difficulty spine are
*graph* properties. The construct that makes a kitchen-sink pack cohere is exactly
"quest reward in mod A → unlocks a recipe bridging A→B → recipe result is the input task
of mod B's chapter root." That cross-facet edge is impossible if recipes live in a
separate list. Tradeoff accepted: wider node enum, one emitter per facet.

### 1.1 Recipes as a quest-graph facet (the integration the user asked for)

A `recipe_unlock` node **is a quest** and **is a recipe**:

- It lives in the DAG: its `deps` gate *when the bridged craft becomes available*; its
  result is wired as the input task of a downstream node (gating-via-reward).
- It compiles to **(a)** a Heracles quest — **always** an `item` task on the recipe's
  `result` (the curator schema for a recipe-facet node does NOT accept `tasks`; only
  `lore`/`deps`/`rewards`/`recipes`). Never a `recipe` task (Open-Loader-injected-recipe
  detection is unverified). Enforced in code, not left to the model — and **(b)** an
  Open Loader datapack recipe file.
- Its datapack recipe `id` is **derived deterministically from the node**
  (`anvil:<stable_hex(chapter:node:recipe:i)>`), so it is Anvil-authored,
  collision-free, and grounded by construction — the curator supplies type/pattern/
  ingredients/result, never an id.
- Quality rule (the cohesion guarantee): a recipe must touch a pinned-mod namespace
  (non-`minecraft`) on at least one side; the graph as a whole (final gate) must produce
  ≥1 modded output. Pure vanilla→vanilla is rejected as `OrphanRecipe`. Legitimate
  vanilla-input→mod-output bridges are kept (the strict "both sides modded" reading
  would ban the most common cross-mod integration recipe — rejected).

This is "recipes complement progression": the questline *narrates* a tier gate; the
embedded recipe *makes the shortcut un-craftable* so the player must walk the spine.

### 1.2 Objective → detector compiler (the hard rule)

Every objective compiles to exactly one in-game detector or to a manual checkmark — a
**total function**; an undetectable objective is not silently dropped, it is lowered to
`Checkmark` and the node tagged `manual:true` so the UI/validator report it honestly.
Mapping table + predicate shapes: see `progression_detection_spec.md §3`. Summary:
have/submit→`heracles:item`; kill→`heracles:kill_entity`(+entity predicate);
advancement/biome/structure/dimension→native Heracles tasks; craft-bridge→`item` on the
Anvil recipe result; Anvil boss/site/gate→advancement/structure/kill on the
Anvil-authored id; anything with no primitive→`heracles:check` (honest manual).

NOTE — Heracles task-shape bugs found in current `quest.rs` (fix during impl, from the
detection spec): `kill_entity` needs a `RestrictedEntityPredicate` object (not a bare
string — an IR-level change), `dimension`→`to`, `biome`→`biomes`,
`structure`→`structures`, `recipe`→`recipes`, item-reward
`{"item":{"item":...}}`→ItemStackCodec `{"id":...}`, and `heracles:check` is an
`NbtPredicate` task (NOT click-to-complete) — emit a real predicate, drop it, or
confirm the client self-complete path. These currently won't parse in-game.

---

## 2. Grounding contract (the actual fix for fabricated ids)

Move from **post-hoc namespace lint** to **push-driven concrete-id grounding**. Tiers:

1. **Static jar scan (the spine, Slice 1).** After `assemble_pack`, before progression
   design, scan each pinned jar (a zip): `data/<ns>/recipe[s]/`, `tags/`,
   `advancement[s]/`, `worldgen/structure[s]/`, `assets/<ns>/lang/en_us.json`
   (`item.*`/`block.*`/`entity.*`/`advancement.*` keys → also gives human labels),
   `fabric.mod.json`/`mods.toml` (id, name, categories). Produces a concrete
   `RegistryVocab{items,entities,blocks,advancements,structures,biomes,tags,recipe_ids,
   mod_meta}`. Offline, deterministic. Misses runtime/datagen-registered ids → tier 3.
2. **Anvil-authored allowlist.** Every id Anvil's companion datapack registers (recipes,
   bosses, sites, gate advancements) is first-class in the vocab — "verified vs real
   registry **OR** Anvil-authored." This is what makes provisioned content groundable
   without a mod.
3. **First-launch registry probe (Slice 1.5, refinement).** A companion-datapack load
   `function` dumps the live registry to a log Anvil tails on first launch (or the
   Modrinth `registry-dump` mod, Fabric/Forge/NeoForge, 1.20.1 ✓). Reconciles and flags
   any id the static scan hallucinated. Refines, does not gate (chicken-and-egg).

`build_index`/`validate_graph` are rewritten to check **concrete membership** (tier
1+2), namespace-fallback only where the scan is known-incomplete (tagged at lower
confidence in the issue). New curator tool `query_registry(kind, filter)` lets the model
*query* real ids during design instead of recalling from training and being linted
after. Methods/comparison: `progression_detection_spec.md §4`.

Current limitation if Slice 1 not yet shipped: namespace-only grounding (both quests and
recipes) — a well-formed but nonexistent exact id passes and silently fails in-game.
This is the documented status quo; Slice 1 is the highest-leverage fix.

---

## 3. Companion-datapack layer (making progression real)

Two write targets inside the instance dir, both verified:

**A. Custom recipes → Open Loader.** Path **`<instance>/config/openloader/data/
anvil-recipes/`** (the `config/` prefix is mandatory; pre-1.17 `openloader/data` is
wrong — verified from Open Loader source). Mandatory `pack.mcmeta`
(`pack_format:15` for 1.20.1) or the pack is silently dropped. 1.20.1 dir is `recipes/`
(plural; 1.21→`recipe/`). 1.20.1 shapes: shaped/shapeless `result:{"item","count"}`,
**smelting `result` is a plain string**, ingredients are `{"item"}`/`{"tag"}` objects.
Open Loader 1.20.1 = Fabric+Forge only (NeoForge from 1.21.1 → NeoForge-1.20.1 falls
back to per-world datapack, flagged). When any node has recipes the curator MUST pin
`open-loader` (enforced like quests require odyssey-quests+resourceful-lib).

**B. Bosses / sites / gates → Anvil vanilla-primitive datapack** at
`<instance>/datapacks/anvil_progression/` (or via Open Loader). No mods, no code:
- **Gate** = advancement whose `rewards.function` sets a scoreboard/grants a flag;
  downstream detector = `advancement` on that gate.
- **Boss** = the headline **loot-token pattern** (DawnCraft's "Eye", generalised):
  summon any registered entity via a `function` with attribute modifiers + custom name +
  unique scoreboard `Tag` + bossbar; detect death via a tag-filtered
  `player_killed_entity` advancement; inject a unique Anvil token into its loot via a
  loot-table modifier; the quest is `item` on that token. Token+modifier emission is
  **atomic** (a token quest with no modifier is hollow — worse than a checkmark).
  Fallback for AoE/non-player kills documented in the provisioning notes.
- **Site** = ship a small pre-built `.nbt` theme library (ruin/altar/arena/vault) +
  generated `structure_set`/`structure`/`template_pool` placement JSON; quest detects
  via `structure`/`location`. Programmatic NBT geometry is out of scope (high-risk).
- **Custom dimensions**: deferred (must exist at world creation; silent worldgen breakage).

Everything Anvil authors is added to the Anvil-authored allowlist at emit time, so it is
groundable by construction. Determinism: sorted keys, stable hex, trailing newline,
extending the existing test discipline. Risk matrix: provisioning spec.

---

## 4. Node taxonomy, edges, requirements

Node `kind` (6): `quest` (default), `recipe_unlock`, `gate` (pure prereq junction, no
task/reward), `content` (provisioned boss/site), `narrative` (manual flavor beat),
`milestone` (convergence capstone). Quest sub-typing is carried by its task list (the
existing `QuestTask` enum + additions), not new kinds.

Requirement model (wire-compatible: bare `["a","b"]` still = implicit AND/hard):
`requirement{ mode: all|any|n_of, n, of:[ {node,gate:hard|soft} | {item,count} |
{stage} | {recipe} | {advancement} ] }`. Heracles deps are flat-AND only (verified in
`Quest.java`) — `any`/`n_of` lower to synthesised hidden `gate` nodes + companion-datapack
flags; `soft` gates omit the Heracles edge (kept in IR for layout/weight). Item/stage/
recipe/advancement preconditions that are not nodes compile to extra tasks or datapack
stage predicates, never phantom dep ids.

Reward model: `item|xp|command|recipe|stage|choice/loot_bag|cosmetic|none`. Heracles
adds `loot` + `selectable` (choose-one) — add these; flat item dumps are the anti-pattern,
choice/currency economy is how every successful pack controls the power curve.
**Gating-via-reward is a checked invariant**: if B requires item/recipe/stage X, some
ancestor must reward X (`UnfedRequirement` issue otherwise). A `recipe_unlock` MUST emit
a `recipe`/result reward so successors can use the bridge.

Edge roles (derived, drive validator + layout): chain, branch/fork, convergence
(in-deg≥2), spine_link (cross-chapter), side_spur, terminal. Healthy-DAG rules (5
already in `validate_graph`, ~5 new): ≤1 root/chapter, exactly one global root, ≥1
convergence per pack, every `recipe_unlock` bridges two distinct non-vanilla namespaces,
no pointless node, spine touches a node per task-bearing chapter.

---

## 5. Difficulty, narrative, economy, multiplayer

- **Narrative ⟂ mechanics**: `lore` is free text, never grounded, never a failure
  source; an undetectable beat → `Checkmark` + rich lore + honest manual flag.
- **Progression weight** 0..3 = effort × topo-depth × convergence bonus; curator sets
  coarse, Anvil derives precise and reconciles. **Spine** = longest weighted path
  root→terminal-milestone; validator asserts cumulative weight **non-decreasing along
  the spine** (`CurveRegression`). Balance band: weight≥2 nodes 40–70% of task nodes
  (warn outside 30–85%); side/flavor 15–40%.
- **Anti-grind** (data-driven, warn-not-fail): keep mandatory spine short, push bonus to
  optional leaves, fan into 3–4 parallel branches mid-game, OR/k-of-N milestones so
  players route around disliked grinds, scale rewards with depth, no two consecutive
  same-task nodes without facet variety.
- **Multiplayer**: Heracles progress is per-player by default (team via config — emit
  per single/multi intake); Anvil datapack reward functions operate on `@s` not `@a`
  unless explicitly co-op; Open Loader recipes are world-global (correct).

---

## 6. Validation guarantees (what Anvil can PROVE pre-ship)

Green/red report before the pack is declared done: (1) DAG no cycles; (2) every dep
resolves; (3) no unreachable islands; (4) every chapter wired to spine; (5) **every
emitted concrete id ∈ grounded vocab OR Anvil-authored allowlist**; (6) every
`recipe_unlock` ingredient+result grounded & well-formed for its type; (7) no
Anvil recipe id collides with a scanned existing recipe id (save-safe); (8) every
objective compiled to a real detector OR explicit checkmark (manual count reported);
(9) curve non-decreasing along spine; (10) byte-deterministic emit incl. datapack;
(11) if any recipe/content facet, `open-loader` (+Heracles+resourceful-lib) pinned.
(12) **token atomicity** — a `content` boss node with a generated loot-token but no
paired loot-modifier emission path is a **hard `validate_progression` failure** (write
blocked), never a guideline; a hollow token (looks real, never completable, e.g. a
NeoForge-1.20.1 pack where the loot-modifier loader differs) is the worst failure class.

5–8, 11 and 12 are new and only possible because of grounding — the difference between
"looks designed" and "provably installs and is detectable."

---

## 7. Per-genre adaptation (no per-mod code)

= static chapter-archetype library × the registry vocab × curator reasoning. Archetypes
(~8 reusable shapes: Onboarding, ResourceTier, MachineMastery, BossArc, ExplorationArc,
RecipeBridge, CollectionLadder, ConvergenceMilestone). Genre presets map to an *ordered
archetype sequence + gating style + recipe-bridge intensity + pacing + reward economy*,
driven by mod **category/tag/download** signals (never a hardcoded mod list):

| Genre | Spine | Gating | Recipe-bridge | Pacing |
|---|---|---|---|---|
| Tech | Onboard→ResourceTier×2→MachineMastery×N→Convergence | recipe-gated | high | steep |
| Magic | Onboard→CollectionLadder→BossArc→Convergence | gate-advancement | medium | ritual |
| Adventure | Onboard→ExplorationArc×N→BossArc→milestone | structure/biome | low | wide |
| Skyblock | Onboard→RecipeBridge×N→ResourceTier→milestone | **recipe (almost all)** | max | tight |
| Kitchen-sink | Onboard→(tier/mastery per major mod)→multi-convergence | mixed | high | broad 6–10 ch |
| Creature | Onboard→CollectionLadder×N→gym BossArc→Elite | collection | low–med | ladder |

Adding a new mod needs zero Anvil changes: its ids enter the vocab via scan, its
category routes it to an archetype, the curator threads it.

---

## 8. Curator & tool contract

One `progression` phase (already renamed from `questing`). Scoped prompt + tools:
- `query_registry(kind,filter)` — **new spine tool**: grounded id/label search so design
  is push-grounded not post-linted. **Locked signature:** `query_registry(kind:
  "item"|"entity"|"advancement"|"structure"|"biome"|"tag"|"recipe", filter:{namespace?,
  contains?, mod?})` → `[{id, label, source_mod}]`, paginated (cap N, offset). Read-only
  over the Slice-1 `RegistryVocab` + Anvil-authored allowlist.
- `generate_quests(graph, final)` — the orchestrator tool; nodes may carry a `recipes`
  facet (shaped/shapeless/smelting; no id — derived). Emits Heracles + (if any recipe
  nodes) the Open Loader datapack. **The separate `generate_recipes` tool is removed —
  recipes are quest nodes.** Batched/accumulating exactly as today (1–3 chapters/call,
  same-id replace, hard checks every call, quality gate only on `final`, nothing written
  on failure).
- `validate_progression(instance_id)` — the §6 contract; final hard gate.

System prompt: design a *progression* — quests, recipe-bridge nodes that stitch mods,
gated/provisioned content; `query_registry` before referencing any id; vary facets;
honest checkmarks; one final validate. Keep the kitchen-sink/All-the-Mods quality bar.
Editor (existing node-graph viewer): color/icon by facet, manual nodes badged, pin/lock
a node from regeneration, inline lore edit (never re-grounded), re-validate button.

---

## 9. Rename / refactor plan (minimal risk)

| Today | Becomes | Control |
|---|---|---|
| `quest.rs` | stays = Quest-facet emitter; public fns keep working | existing tests stay green; `build_index` gains concrete ids |
| `recipe.rs` | stays = recipe IR + serializer + validator, **reused by the graph** (no standalone `anvil-recipes.json`/`write_recipes` silo) | tests adapted |
| — | thin orchestrator: dispatch facets, run §6 contract | wraps the above |
| `anvil-quests.json` | carries recipe facets; migrate-on-load legacy | no data loss |
| `config/heracles/quests/*` | unchanged | no in-game path churn |
| tool `generate_recipes` | **removed**; folded into `generate_quests` node schema | no curator-contract break (one fewer tool) |
| UI "Quests" tab | "Progression" tab, facet-aware | label + render only |

Principle: quest is a *facet* of progression, not a synonym; the umbrella is new, the
quest path is load-bearing and preserved/wrapped, new capability is additive.

---

## 10. Phased roadmap — SEQUENCED, with verification gates

**Scope of "implement the whole doc": Slices 0→1→2, each code-verified then
user-verified in-game before the next.** Slices 3–5 are explicitly **gated on a
verified 0–2** — mass-building content provisioning / genre adaptation on top of an
unverified core is the exact failure mode this session has repeatedly hit. Do NOT
parallel-dispatch all slices.

**Slice exit-criteria (every slice, non-negotiable):**
1. `cargo test --lib` all green, `cargo check` + `npx tsc --noEmit` clean (only the
   pre-existing `launch.rs::prepare` warning), zero new warnings.
2. Slice ends with a concrete **user-verifiable in-game check** stated explicitly;
   the next slice does not start until the user reports that signal.
3. If the user reports a failure, **read the deterministic artifact first**
   (`hs_err*`, the Fabric/MC log, `~/Library/Logs/DiagnosticReports/*.ips`, the actual
   `validate_progression` output) *before* proposing any fix. Theorizing before
   reading the artifact is the prohibited pattern.

- **Slice 0 — Heracles emission correctness + task IR (HARD PREREQUISITE).** Fix all 6
  codec shape bugs (detection-spec §"Bugs in current quest.rs emitter", lines ~398–416):
  `kill_entity` → `RestrictedEntityPredicate` object (**IR-level**: `QuestTask::Kill`
  grows from `entity:String` to `{type, location?, nbt?, ...}`), `dimension`→`to`,
  `biome`→`biomes`, `structure`→`structures`, `recipe`→`recipes`, item-reward →
  ItemStackCodec `{"id":...}`; fix/qualify `heracles:check` (NbtPredicate, not
  click-complete). **This slice also owns the `QuestTask` enum expansion** toward the
  verified 17-codec set (priority: `GatherItem`+nbt, `Composite`, `Stat`, `Location`;
  interaction tasks optional). Rationale: if these shapes are wrong, the in-game
  questbook has been silently malformed and nothing the user tested was a fair test —
  so this is the foundation, not a footnote. **User-verifiable check:** regenerate a
  questline, launch, Fabric log shows quests parse with no Heracles codec errors and
  tasks render.
- **Slice 1 — Grounding (MVC).** Static jar-scan → concrete `RegistryVocab`; rewrite
  `build_index`/`validate_graph` to concrete ids; add `query_registry` (locked sig §8).
  No new emitters. Kills fabricated ids for quests *and* recipes. **User-verifiable
  check:** generate a new pack; `validate_progression` reports zero false "unknown id"
  for ids the scan should catch, and rejects a deliberately bad id.
- **Slice 1.5 — First-launch registry probe.** Reconcile scan misses (refinement).
- **Slice 2 — Recipes as quest-graph facet.** Node `recipes` facet reusing `recipe.rs`;
  `write_quests` also emits the Open Loader datapack; `validate_graph` folds in recipe
  grounding/quality; remove the standalone `generate_recipes`/silo; recipe-facet nodes
  always surface `item`-on-result (curator schema forbids `tasks` on them); add ≥1
  fully-worked recipe-bridge example to the curator prompt (e.g. Create andesite_alloy
  + vanilla diamond → Thermal machine_frame, 3×3 shaped) or the model writes only
  quest nodes. **User-verifiable check:** generate a recipe-bridge pack, restart, open
  the recipe in-game — it crafts; the gating quest tracks the result item.
- **Slice 3 — Content provisioning** (loot-token boss/site/gate). *Gated on verified 2.*
- **Slice 4 — Genre adaptation** (archetype library + presets). *Gated on verified 3.*
- **Slice 5 — Polish** (facet editor, curve viz, multiplayer cfg, server layout).

**First action: Slice 0 only.** Ship it, code-verify, then have the user launch their
existing pack. If quests now parse cleanly the foundation is real and 1→2 are unblocked;
if not, we bisect with one slice's diff before the bigger refactor.

---

## 11. Verified facts & open decisions (carry into implementation)

- Open Loader path `config/openloader/data/anvil-recipes/`; `pack.mcmeta` mandatory
  (`pack_format` 15 / 1.20.1); `recipes/` plural; smelting `result` = bare string;
  Open Loader 1.20.1 = Fabric+Forge only.
- Heracles is the emission target (FTB Quests is CurseForge-only → Modrinth constraint
  decisive; Heracles per-quest JSON + `groups.txt` is the only cleanly launcher-writable
  format). Anvil currently models 8/17 Heracles tasks; add `GatherItem`(+NBT),
  `Composite`, `Stat`, `Location`, interaction tasks. Fix the 6 shape bugs (§1.2).
- Quality gate decision (locked): `OrphanRecipe` = touches no modded ns either side;
  `SetHasNoModOutput` on final; skip <3 recipes; vanilla→mod bridges allowed.
- Loot-token boss gate is the headline mod-agnostic content pattern; token+loot-modifier
  emission must be atomic; AoE/non-player-kill fragility documented.
- Worldgen NBT geometry & custom dimensions: out of v1 scope (fragile).
- Deep technical detail (triggers, predicate shapes, registry-dump, codec JSON,
  1.20.1↔1.21 deltas) authoritative in `progression_detection_spec.md`.
