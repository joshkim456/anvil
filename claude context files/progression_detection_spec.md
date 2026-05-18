# Anvil — Mod-Agnostic Detection & Grounding Layer (SOURCE-GROUNDED SPEC)

Target: **Minecraft 1.20.1** (Java). 1.21 deltas flagged inline. Scope: auto-generate a
progression layer for ANY assembled pack; **never emit an id absent from the actual pack**.
All claims cited to primary source. Verified 2026-05-16.

Citation keys:
- `[ADV]` https://minecraft.wiki/w/Advancement_definition
- `[PRED]` https://minecraft.wiki/w/Predicate
- `[EPRED]` https://minecraft.wiki/w/Entity_predicate
- `[DG]` https://minecraft.wiki/w/Minecraft_Wiki:Projects/wiki.vg_merge/Data_Generators
  & https://minecraft.wiki/w/Tutorials/Running_the_data_generator
- `[HER]` github.com/terrarium-earth/Heracles @ branch `1.20.x` (codec source, paths inline)
- `[RL]` github.com/Team-Resourceful/ResourcefulLib @ branch `1.20`
- `[KJS]` kubejs.com / wiki.latvian.dev (KubeJS `/kubejs dump`)
- `[RDUMP]` modrinth.com/mod/registry-dump
- `[FTBQ]` docs.feed-the-beast.com/mod-docs/mods/suite/Quests + CurseForge listings

VERSION BOUNDARY (load-bearing): item/entity matching switched from **NBT** to **data
components** in snapshot **24w09a → release 1.20.5** `[PRED]`. **1.20.1 uses NBT
everywhere** (`nbt` string field). 1.21 uses `components`/`predicates`. Both shapes given.

---

## 1. VANILLA ADVANCEMENT TRIGGER TAXONOMY (1.20.1)

Advancement file: `data/<ns>/advancements/<path>.json` (folder renamed
`advancements/`→`advancement/` in 1.21 — see §4 delta table). Criterion shape `[ADV]`:
```json
{ "criteria": { "<name>": { "trigger": "minecraft:<id>", "conditions": { ... } } },
  "requirements": [["<name>"]] }
```

**1.20.1 PREDICATE-SHAPE BOUNDARY (load-bearing, separate from the §2 data-component
boundary):** in **1.20.1** the `player`/`entity`/`child`/etc. condition fields are
**inline entity-predicate objects**:
`"conditions": { "player": { <entity predicate> } }`. In **1.20.2** these became a
**list of predicate conditions**:
`"conditions": { "player": [ { "condition":"minecraft:entity_properties",
"entity":"this", "predicate": { <entity predicate> } } ] }` (verified: the current wiki
trigger examples all use the `entity_properties` list form `[ADV]`). Anvil must emit the
**inline** form for a 1.20.1 pack and the **list** form for ≥1.20.2. The objective→trigger
mappings below show the 1.20.1 inline form.

The trigger *set* below is the post-1.20.5 wiki enumeration `[ADV]` filtered to triggers
present in 1.20.1; each row's "Since" column pins the minimum version (verified rows have
an explicit version; "1.20.1✓" = confirmed present in 1.20.1; rows that could not be
pinned to ≤1.20.1 from primary source are **excluded** rather than asserted — the
objective-archetype map needs only ~12 of these, the long list is reference).

Triggers usable for objective detection (1.20.1; † = post-1.20.1, included only to flag
they are NOT usable on a 1.20.1 pack):

| Trigger | Since | Detects | Key condition fields | Predicate support |
|---|---|---|---|---|
| `minecraft:inventory_changed` | 1.20.1✓ | Player holds/obtains item(s) anywhere in inv | `items` (list of item predicates), `slots.occupied/full/empty` | full item predicate |
| `minecraft:player_killed_entity` | 1.20.1✓ | Player lands killing blow on entity | `entity` (inline 1.20.1 / cond-list 1.20.2+), `killing_blow` (damage-source predicate) | full entity predicate |
| `minecraft:entity_killed_player` | 1.20.1✓ | Entity kills the player | `entity`, `killing_blow` | full entity predicate |
| `minecraft:player_hurt_entity` | 1.20.1✓ | Player damages entity (no kill needed) | `entity`, `damage` (damage predicate) | full entity predicate |
| `minecraft:kill_mob_near_sculk_catalyst` | 1.19✓ | Kill near sculk catalyst | `entity`, `killing_blow` | full entity predicate |
| `minecraft:killed_by_arrow` | 1.20.1✓ | Kill via arrow/projectile | `victims` (list of entity preds), `unique_entity_types`; `fired_from_weapon` is †1.21 only | full entity predicate |
| `minecraft:changed_dimension` | 1.20.1✓ | Cross dimension | `from`, `to` (dimension ids, e.g. `minecraft:the_nether`) | id match only |
| `minecraft:location` | 1.20.1✓ | Polled every 20 ticks while at a place | `player` → entity predicate w/ `location` (biome/structure/dimension/position) | full location predicate |
| `minecraft:tick` | 1.20.1✓ | Every tick — present in 1.20.1; **removed in 1.20.5+, use `location` for 1.21** | `player` | full entity/location predicate |
| `minecraft:slept_in_bed` | 1.20.1✓ | Enter a bed | `player` (location) | location via player |
| `minecraft:recipe_unlocked` | 1.20.1✓ | Recipe unlocked (recipe id known to player) | `recipe` (recipe id) | id match only |
| `minecraft:recipe_crafted` | **1.20**✓ | Craft a specific recipe (table/stonecutter/smithing) — usable on 1.20.1 | `recipe_id`, `ingredients` (item preds) | recipe id + item preds |
| `minecraft:crafter_recipe_crafted` | †1.21 | Auto-crafter crafts recipe — NOT on 1.20.1 | `recipe_id`, `ingredients` | recipe id |
| `minecraft:consume_item` | 1.20.1✓ | Eat/drink an item | `item` (item predicate) | full item predicate |
| `minecraft:using_item` | 1.20.1✓ | Currently using (holding-use) an item | `item` | full item predicate |
| `minecraft:filled_bucket` | 1.20.1✓ | Bucket filled | `item` (resulting filled bucket) | full item predicate |
| `minecraft:fishing_rod_hooked` | 1.20.1✓ | Fished entity/item | `item`, `entity`, `rod` | item + entity predicate |
| `minecraft:shot_crossbow` | 1.20.1✓ | Fired crossbow | `item` | full item predicate |
| `minecraft:enchanted_item` | 1.20.1✓ | Enchanted at table | `item`, `levels` | item predicate |
| `minecraft:item_durability_changed` | 1.20.1✓ | Item took/repaired durability | `item`, `delta`, `durability` | item predicate |
| `minecraft:placed_block` | 1.20.1✓ | Place a block (1.20.1 form; **renamed → `item_used_on_block` in 1.20.5**) | `block`, `state`, `location`, `item` | block id + state + item predicate |
| `minecraft:enter_block` | 1.20.1✓ | Player hitbox inside a block | `block`, `state` | block id + state |
| `minecraft:bred_animals` | 1.20.1✓ | Breed two animals | `child`, `parent`, `partner` (entity preds) | full entity predicate |
| `minecraft:tame_animal` | 1.20.1✓ | Tame an animal | `entity` | full entity predicate |
| `minecraft:player_interacted_with_entity` | 1.20.1✓ | Right-click entity with item | `item`, `entity` | item + entity predicate |
| `minecraft:summoned_entity` | 1.20.1✓ | Entity summoned (golem/wither/dragon) | `entity` | full entity predicate |
| `minecraft:cured_zombie_villager` | 1.20.1✓ | Cure zombie villager | `villager`, `zombie` | entity predicate |
| `minecraft:villager_trade` | 1.20.1✓ | Trade with villager | `villager`, `item` | item + entity predicate |
| `minecraft:brewed_potion` | 1.20.1✓ | Brew a potion | `potion` (potion id) | id match |
| `minecraft:effects_changed` | 1.20.1✓ (`source` field 1.19+) | Status effect applied/removed | `effects` (effect→amplifier/duration), `source` (entity pred) | id + amplifier match |
| `minecraft:levitation` | 1.20.1✓ | Levitation effect active over distance | `distance`, `duration` | distance predicate |
| `minecraft:nether_travel` | 1.20.1✓ | Overworld↔Nether scaled travel | `start_position`, `distance` | location + distance |
| `minecraft:player_generates_container_loot` | 1.20.1✓ | Open/generate a loot table | `loot_table` (loot table id) | id match |
| `minecraft:target_hit` | 1.20.1✓ | Hit a target block | `signal_strength`, `projectile` | int + entity predicate |
| `minecraft:construct_beacon` | 1.20.1✓ | Build/upgrade beacon | `level` | int range |
| `minecraft:used_ender_eye` | 1.20.1✓ | Throw ender eye | `distance` | distance |
| `minecraft:hero_of_the_village` | 1.20.1✓ | Win a raid | `player` (location) | location |
| `minecraft:voluntary_exile` | 1.20.1✓ | Trigger raid via banner | `player` (location) | location |
| `minecraft:impossible` | 1.20.1✓ | Never (manual-only, granted by command) | — | — |

Excluded (could not be pinned to ≤1.20.1 from primary source; do NOT emit for a 1.20.1
pack without re-verifying): `any_block_use`, `default_block_use`, `item_used_on_block`,
`fall_after_explosion`, `ride_entity_in_lava`, `player_sheared_equipment`,
`allay_drop_item_on_block`, `crafter_recipe_crafted`, `avoid_vibration`,
`thrown_item_picked_up_by_*` — all appear only on the post-1.20.5 wiki enumeration.

Objective archetype → exact (trigger + predicate):
- **Obtain/have item X** → `inventory_changed`, `conditions.items:[{ "items":["X"] }]` (1.21:
  `items` is the predicate field; 1.20.1 the field is `items` with `item`/`tag` legacy).
- **Defeat specific entity X** → `player_killed_entity`, `conditions.entity:{ "type":"X" }`.
- **Defeat ANY of a group** → `player_killed_entity`, `entity:{ "type":"#namespace:tag" }`
  (entity type tag) — only if a tag exists; else N criteria OR-ed via
  `requirements:[["a"],["b"]]`.
- **Reach biome B** → `location`, `conditions.player:[{ "condition":"location_check",
  "predicate":{ "biomes":"B" } }]` (1.20.1 location predicate `biome` is single id;
  `biomes` accepts tag in 1.21).
- **Reach structure S** → same, location predicate `structure:"S"`.
- **Enter dimension D** → `changed_dimension`, `conditions.to:"D"`.
- **Craft via recipe R** → `recipe_crafted` `recipe_id:"R"` (added **1.20**, usable on
  1.20.1); alt `recipe_unlocked` `recipe:"R"` (fires on unlock not craft — weaker) OR
  `inventory_changed` on the output item.
- **Visit / be-at** → `location` with location predicate `position`/`biome`.
- **Custom milestone** → `impossible` trigger, granted out-of-band by `/advancement grant`
  from a function or quest-mod hook (the only zero-coupling "manual" objective).

---

## 2. ENTITY/ITEM PREDICATE CAPABILITY & THE VARIANT PROBLEM

### Predicate condition types `[PRED]`
Root = object or array (array = AND). Conditions: `minecraft:all_of`/`any_of`
(`terms`), `inverted` (`term`), `entity_properties` (`entity`:"this"|"killer"|...,
`predicate`:<entity predicate>), `location_check` (`predicate`:<location predicate>,
`offsetX/Y/Z`), `match_tool` (`predicate`:<item predicate>), `damage_source_properties`,
`block_state_property` (`block`,`properties`), `entity_scores` (`entity`,`scores`:{obj:
{min,max}}), `random_chance`, `time_check`, `weather_check`, `reference`
(`name`:another predicate id).

### Entity predicate `[EPRED]` (1.20.1 fields)
`type` (entity id or `#tag`), `nbt` (SNBT string, **outer braces included**), `location`,
`stepping_on`, `distance`, `flags` (`is_on_fire`/`is_baby`/`is_sneaking`/...),
`equipment` (per-slot item predicates), `effects`, `team`, `passenger`, `vehicle`,
`targeted_entity`, `type_specific` (subtypes: `fishing_hook`, `lightning`, `player`,
`raider`, `sheep`, `slime`).

### The variant-detection ladder (the hard problem: "a SPECIFIC variant of a generic mod entity")
Most mod bosses/variants are **one `EntityType`** differentiated only by spawn NBT
(`{Variant:..}`, `{boss:1b}`, custom tags) or a custom `Name`. Detection ladder, best→worst:

1. **Distinct EntityType** → `type:"mod:boss_x"`. Auto-generatable: **HIGH**. (Verify the
   id exists in the registry dump §4.)
2. **Entity type tag** (`#mod:bosses`) → covers "any boss" objectives if the pack ships
   the tag (check `data/<ns>/tags/entity_type/`). **HIGH** when tag present.
3. **NBT discriminator** → `entity:{ "type":"mod:generic", "nbt":"{Variant:3}" }`.
   `nbt` does a **partial** compound match `[EPRED]`. Auto-generatable: **LOW** — the
   discriminator key/value is mod-internal and not in any registry dump; would need a
   spawn-egg/structure inspection or hardcoded per-mod knowledge. Anvil must NOT invent
   NBT keys.
4. **CustomName match** → `nbt:"{CustomName:'{\"text\":\"Boss\"}'}"` — only if the pack
   names the entity; brittle. **LOW**.
5. **Fallback: do not disambiguate** — generate the objective at EntityType granularity
   ("kill a <generic>") and surface a transparency note. **This is the default** when 1–2
   fail. Never silently emit an unverifiable NBT predicate.

### Item predicate — 1.20.1 vs 1.21 (load-bearing) `[PRED]`
**1.20.1 (NBT):**
```json
{ "items": ["mod:thing"], "nbt": "{display:{Name:'...'}}", "count": {"min":1},
  "enchantments": [{"enchantment":"minecraft:sharpness","levels":{"min":1}}] }
```
**1.21 (data components):** field renames — `tag`→removed, `nbt`→`custom_data`,
component-specific predicates moved under a `predicates` map:
```json
{ "items":"mod:thing", "components":{"minecraft:custom_data":"{k:1}"},
  "predicates":{"minecraft:enchantments":[{"enchantments":["minecraft:sharpness"],
  "levels":{"min":1}}]} }
```
Boundary = 1.20.5/24w09a `[PRED]`. Anvil emits the **NBT shape for 1.20.1** and the
**component shape for ≥1.20.5** based on the pack's MC version. Same variant ladder
applies (custom_data ↔ nbt).

---

## 3. OBJECTIVE → DETECTOR COMPILER TABLE

Auto-gen rating: **HIGH** = one vanilla trigger+predicate fully specifies it from a
registry id alone; **MED** = trigger exists but disambiguation needs a tag/NBT that may
be absent (emit coarse + transparency note); **LOW** = needs custom datapack
function/scoreboard or mod-specific knowledge — Anvil should route to Heracles' own
task type instead of a synthetic advancement.

| Objective | Vanilla detector (1.20.1) | Heracles task (preferred path, §5) | Auto-gen |
|---|---|---|---|
| Have / obtain item X | adv `inventory_changed` items=[X] | `heracles:item` `{item:X}` | HIGH |
| Obtain N of item X | adv `inventory_changed` items=[X] + count, OR Heracles amount | `heracles:item` `{item:X,amount:N}` | HIGH |
| Defeat specific entity X (distinct type) | adv `player_killed_entity` entity.type=X | `heracles:kill_entity` entity.type=X | HIGH |
| Defeat ANY entity in group | adv with `#tag` if shipped, else N OR-criteria | `heracles:kill_entity` w/ type tag | MED |
| Defeat mod boss = NBT variant | adv `player_killed_entity` + nbt (unverifiable) | `heracles:kill_entity` coarse type + note | LOW |
| Reach biome B | adv `location` + location_check biome=B | `heracles:biome` biomes=B | HIGH |
| Reach structure S | adv `location` + location_check structure=S | `heracles:structure` structures=S | HIGH |
| Enter dimension D | adv `changed_dimension` to=D | `heracles:changed_dimension` to=D | HIGH |
| Craft via recipe R | adv `recipe_crafted` recipe_id=R (added 1.20, OK on 1.20.1) | `heracles:recipe` recipes=[R] | HIGH |
| Visit a place / be-at | adv `location` + position/biome | `heracles:biome` or `heracles:check` | HIGH/MED |
| Interact with block/entity | adv `item_used_on_block`/`player_interacted_with_entity` | `heracles:block_interact` / `heracles:entity_interact` | MED |
| Gain XP level | scoreboard / Heracles only | `heracles:xp` | MED (Heracles) |
| Reach a stat threshold | scoreboard `minecraft.custom` | `heracles:stat` | MED |
| Custom narrative milestone | adv `impossible` + external `/advancement grant` | `heracles:check` (manual checkbox) | HIGH (via Heracles check) |

Rule: prefer the **Heracles task type** (it observes the live game, no datapack
authoring, no advancement-file id risk) and use synthetic advancements only for
objectives Heracles cannot express. The advancement taxonomy (§1) is the *fallback
expressiveness reference*, not the primary emit path.

---

## 4. REGISTRY-DUMP GROUNDING (the correctness seam)

Goal: from an *assembled, not-yet-played* pack, obtain the **real** registries:
items, blocks, entity_type, advancements, structures, biomes, dimensions, recipes, tags.
Grounding ≠ detection: grounding produces the allowed-ID universe **once per pack
assembly**; detection (§3/§5) consumes it.

### Datapack folder names — 1.20.1 vs 1.21 delta (load-bearing for the static scrape)

The static scrape's globs differ by MC version. Singular-folder rename landed in **1.21**:

| Content | 1.20.1 path | 1.21 path |
|---|---|---|
| Advancements | `data/<ns>/advancements/` | `data/<ns>/advancement/` |
| Recipes | `data/<ns>/recipes/` | `data/<ns>/recipe/` |
| Loot tables | `data/<ns>/loot_tables/` | `data/<ns>/loot_table/` |
| Predicates | `data/<ns>/predicates/` | `data/<ns>/predicate/` |
| Item modifiers | `data/<ns>/item_modifiers/` | `data/<ns>/item_modifier/` |
| Functions | `data/<ns>/functions/` | `data/<ns>/function/` |
| Structures | `data/<ns>/structures/` | `data/<ns>/structure/` |
| Entity-type tags | `data/<ns>/tags/entity_types/` | `data/<ns>/tags/entity_type/` |
| Block/item/fluid tags | `data/<ns>/tags/{blocks,items,fluids}/` | `tags/{block,item,fluid}/` |
| (worldgen) | `data/<ns>/worldgen/{biome,structure}/` | unchanged |

The scrape must select the glob set from the pack's `pack_format` / declared MC version.

### Methods, by reliability

**A. Static jar/datapack scrape (offline, no game launch).** Read, from every resolved
mod jar + the pack's datapacks (folder names per the delta table above):
- `data/<ns>/advancements|advancement/**.json` → advancement ids `<ns>:<path>`
- `data/<ns>/recipes|recipe/**.json` → recipe ids
- `data/<ns>/loot_tables|loot_table/**`, `tags/**` → loot/tag ids
- `data/<ns>/worldgen/biome/**`, `structure/**`, `dimension/**` → datapack-defined ids
- `assets/<ns>/lang/en_us.json` keys `item.<ns>.*` / `block.*` / `entity.<ns>.*` →
  *surface* of item/block/entity ids (heuristic; lang keys ≈ ids but not 1:1)
Captures: all **static datapack/mod-asset content**. **Misses**: code-registered items/
entities/blocks not in lang, KubeJS-scripted registrations, runtime-built recipes.
Reliability: **MEDIUM** (good for advancements/recipes/biomes/structures/tags which are
*always* datapack JSON; weak for code-registered items/entities). Zero launch cost.

**B. Vanilla `--reports` data generator** `[DG]`:
`java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports` →
`generated/reports/registries.json` (shape:
`{"minecraft:item":{"protocol_id":N,"entries":{"minecraft:stone":{"protocol_id":M}}}}`),
plus `blocks.json`, `commands.json`. **VANILLA-ONLY**: a vanilla server jar registers no
mods, so this is a *baseline* (the minecraft: namespace), **not** a pack solution.
Reliability for a modpack: **LOW** (incomplete by construction).

**C. Headless first-launch + registry-dump mod (canonical).** Inject a small Modrinth
mod into the assembled instance, launch the server/client once headless, dump, remove.
Modrinth-published option satisfying the Modrinth-only constraint:
**Registry Dump** `[RDUMP]` (modrinth.com/mod/registry-dump) — Fabric **+ Forge +
NeoForge**, MC **1.19.2 / 1.20.1 / 1.21.1**, server-side, command-triggered
`/dump registry`, output `dump/<registry key>/<namespace>.json` (JSON array of every key
in that registry/namespace). Because it reads the **populated** registries it captures
*everything* — code-registered, datapack, and (if KubeJS is in the pack) KubeJS-scripted
ids. Cost: one ~30–60s headless launch on first pack build.
Reliability: **HIGH** (observes the real loaded registry — the only method that does).
Alternatives (same idea, also Modrinth): TellMe, Dumpster, Data Dumper, Registry Dumper
3000. KubeJS-specific: `/kubejs dump <registry>` + `/kubejs list-tag <registry>` if the
pack already ships KubeJS `[KJS]` (no extra mod). All require the command be run, so a
launch is unavoidable for completeness.

**D. Function/command-only, no mod** — vanilla has **no** command that enumerates a full
registry to a file (`/tag`, `/datapack list` don't; predicates can't introspect).
Confirmed not viable `[ADV]`/function page. Not an option.

### Recommended automatable strategy (zero human)

Two-tier, cache-keyed by the resolved pack's content hash:

1. **Tier 1 (instant, on assemble):** Method **A** static scrape → provisional
   `AllowedIndex`. Authoritative *immediately* for advancements/recipes/biomes/
   structures/dimensions/tags (always datapack JSON). Provisional for items/entities/
   blocks.
2. **Tier 2 (first launch, automatic):** On the instance's first run, Anvil's launcher
   (it already controls launch — `launch.rs`) runs a one-off **headless dedicated-server
   dump pass** (no display needed; client is harder and pointless here). This is more
   involved than "run server, read dump" — exact procedure:

   **First-launch dump procedure (zero human):**
   a. Materialise a throwaway server dir: copy the resolved `mods/` set + the pack's
      `config/`/`datapacks/`; add the **Registry Dump** `[RDUMP]` jar
      (loader-matched: Fabric→Fabric build, (Neo)Forge→matching build).
   b. Write `eula.txt` = `eula=true` and a minimal `server.properties`
      (`level-type=minecraft\:flat`, `spawn-protection=0`, `online-mode=false`,
      `max-players=1`, a fixed `level-name`) — superflat minimises worldgen time.
   c. Launch the loader's dedicated-server entrypoint with `nogui`; capture stdout.
   d. Wait for the server-ready log line ("Done (Ns)! For help, type help"). Budget
      **60–180s** (mod init + flat worldgen; kitchen-sink packs trend higher), with a
      hard timeout + a crash-detector on stderr — some mods refuse dedicated-server
      load or crash; on crash, fall back to Tier-1 only and flag the pack as
      static-grounded (degrade, never block).
   e. Pipe to stdin: `/dump registry` then (after the dump-complete log line) `stop`.
   f. Output path is loader/working-dir dependent — Registry Dump writes
      `dump/<registry key>/<namespace>.json` under the server run dir; resolve it
      relative to the working dir Anvil set, not a guessed default.
   g. Parse all `dump/**/*.json` (each = JSON array of keys) into the **complete**
      AllowedIndex; delete the throwaway server dir (the live instance is untouched —
      the helper mod never enters the player's `mods/`).
   h. Re-validate every already-generated quest against the now-authoritative index,
      downgrading any objective that no longer grounds (per §2 ladder / §3 ratings).
3. Persist the resolved index next to `anvil-quests.json`; only re-dump when the pack's
   mod set hash changes.

This replaces the current `quest.rs` **namespace-only** grounding (`build_index`,
which it self-documents as a limitation: it only checks the namespace prefix) with
**full-id** grounding. The static scrape removes most of the gap offline; the
first-launch dump closes it entirely.

---

## 5. HERACLES SCHEMA (verified from 1.20.x codec source) vs FTB QUESTS

### File layout `[HER]`
One file per quest: `config/heracles/quests/<id>.json`. Groups: `config/heracles/
groups.txt`. Quest id = filename stem; dependencies reference those ids.

### Quest top-level — `Quest.CODEC`
(`common/.../api/quests/Quest.java`):
```json
{ "display": { ... }, "settings": { ... },
  "dependencies": ["<questId>", ...],
  "tasks":   { "<taskKeyId>":   { "type":"heracles:<t>", ... } },
  "rewards": { "<rewardKeyId>": { "type":"heracles:<r>", ... } } }
```
`tasks`/`rewards` are **maps** keyed by an arbitrary stable id; the discriminator field
is exactly **`"type"`** (`QuestTasks.java` `KeyDispatchCodec` typeKey = `"type"`); the
type id is a ResourceLocation → `heracles:item` etc. All of `display`/`settings`/
`dependencies`/`tasks`/`rewards` are optional (codec `.orElse`).

`display` — `QuestDisplay.CODEC`:
```json
{ "icon": {...}, "icon_background": "<rl>", "title": <text-component>,
  "subtitle": <text-component>, "description": <string | [string,...]>,
  "groups": { "<groupName>": { "position": { "x": <int>, "y": <int> } } } }
```
NOTE: `title`/`subtitle` are **text Components** (`ExtraCodecs.COMPONENT`) — a plain
JSON string is a valid component. `groups` value = `GroupDisplay` (codec injects the map
key as id; only field is `position`, a `Vector2i` = `{"x":int,"y":int}`). `description`
accepts a single string OR a list of strings.

### Task types (exact codec fields, branch `1.20.x`, `api/tasks/defaults/`)

| `type` | Fields (exact JSON keys) | Source class |
|---|---|---|
| `heracles:item` | `item` (RegistryValue<Item>: id string **or** `#tag`), `nbt` (SNBT compound, opt), `amount` (int, def 1), `collection` (`AUTOMATIC`/`MANUAL`/..., opt) | `GatherItemTask` |
| `heracles:kill_entity` | `entity` (RestrictedEntityPredicate, **required**), `amount` (int, def 1) | `KillEntityQuestTask` |
| `heracles:advancement` | `advancements` (Set<ResourceLocation> → JSON array of ids) | `AdvancementTask` |
| `heracles:biome` | `biomes` (RegistryValue<Biome>: id **or** `#tag`) | `BiomeTask` |
| `heracles:changed_dimension` | `from` (dim id, opt), `to` (dim id, opt) | `ChangedDimensionTask` |
| `heracles:structure` | `structures` (RegistryValue<Structure>: id **or** `#tag`) | `StructureTask` |
| `heracles:recipe` | `recipes` (Set<ResourceLocation> → JSON array) | `RecipeTask` |
| `heracles:check` | `nbt` (NbtPredicate, opt) — manual checkmark | `CheckTask` |
| `heracles:dimension` | (LocationTask: be-in dimension) | `LocationTask` |
| `heracles:stat` | stat id + target | `StatTask` |
| `heracles:xp` | levels/points target | `XpTask` |
| `heracles:item_interact` / `heracles:block_interact` / `heracles:entity_interact` | predicate + target | `*InteractTask` |
| `heracles:composite` | nested tasks | `CompositeTask` |
| `heracles:dummy` | always-incomplete placeholder | `DummyTask` |

All task records also accept optional `title` (string) and `icon`.

`RestrictedEntityPredicate.CODEC` `[RL]`
(`resourcefullib .../codecs/predicates/RestrictedEntityPredicate.java`):
```json
{ "type": "<entity id>",            // required (BuiltInRegistries.ENTITY_TYPE byName)
  "location": { <LocationPredicate> },   // optional
  "effects":  { <MobEffectsPredicate> }, // optional
  "nbt": "{ ...SNBT... }",               // optional (NbtPredicate = CompoundTag)
  "flags":  { <EntityFlagsPredicate> },  // optional
  "target": { <vanilla EntityPredicate> }// optional
}
```
→ This is the verified mechanism for the §2 variant ladder inside Heracles: distinct
`type` (HIGH) or `nbt` discriminator (LOW, only with mod knowledge).

### Reward types (`api/rewards/defaults/`)
| `type` | Fields | Source |
|---|---|---|
| `heracles:item` | `item` = **ItemStackCodec**: bare id string **OR** `{"id":"<item>","count":<int>,"nbt":<compound>}` | `ItemReward` |
| `heracles:xp` | `xptype` (`LEVEL`/`POINTS`, def LEVEL), `amount` (int) | `XpQuestReward` |
| `heracles:command` | `command` (string) | `CommandReward` |
| `heracles:loottable` | loot table ref | `LootTableReward` |
| `heracles:selectable` | choice list | `SelectableReward` |

### ⚠ Bugs in current `quest.rs` emitter (caught by codec source)
The existing serializer mis-shapes several fields vs. the real `1.20.x` codecs:
- `kill_entity`: emits `"entity": "<string>"` — codec needs
  `"entity": { "type": "<id>", ... }` (RestrictedEntityPredicate). **WILL FAIL TO PARSE.**
- `changed_dimension`: emits `"dimension": <id>` — codec field is `to` (and/or `from`).
- `biome`: emits `"biome"` — codec field is `biomes`.
- `structure`: emits `"structure"` — codec field is `structures`.
- `recipe`: emits `"recipe": <id>` — codec field is `recipes` (array).
- reward `heracles:item`: emits `{"item":{"item":id,"count":n}}` — ItemStackCodec object
  key is `id` not `item` (or use a bare id string). Current shape won't decode.
- `advancement`/`item`/`check` shapes are correct.
- `display.title` as a bare string is OK (valid text component); `groups.<g>.position`
  `{x,y}` is correct.

**IR-LEVEL (not just serializer):** the fix for `kill_entity` is bigger than a field
rename. `heracles:kill_entity` requires a `RestrictedEntityPredicate` **object**
(`{type, location?, effects?, nbt?, flags?, target?}`), but the current IR is
`QuestTask::Kill { entity: String, count }`. A `String` cannot express the §2 variant
ladder (type vs `#tag` vs `nbt` discriminator). The IR must grow to
`Kill { entity: EntityPredicateIR, count }` where `EntityPredicateIR` carries at minimum
`type: String` + optional `nbt: Option<String>` (and ideally `effects`/`flags` for
future use). Without the IR change the §2/§3 LOW→HIGH ladder is unimplementable even
after the JSON shape is corrected. Same applies if item/check objectives ever need NBT
discrimination (`GatherItemTask.nbt`, `CheckTask.nbt` exist in the codec). These are
implementation work, separate from this research deliverable.

### FTB Quests vs Heracles — recommendation
FTB Quests format `[FTBQ]`: SNBT under `config/ftbquests/quests/chapters/*.snbt`,
chapter groups, `x`/`y` per quest, hot-reload — equally machine-writable. **But FTB
Quests is CurseForge-only** (verified: all FTB Quests Fabric/NeoForge/Forge builds are
CurseForge listings; no Modrinth project). Anvil is **Modrinth-only** (locked). Heracles
is on Modrinth as "Odyssey Quests" (`terrarium-earth/Heracles`) and is JSON
(serde-trivial, no SNBT serializer needed) with a verifiable codec.
**Recommendation: Heracles. Confirmed: FTB Quests SNBT would be the marginally cleaner
node-graph target if sources were unconstrained, but the Modrinth-only constraint is
binding and decisive — Heracles it is.** No Heracles task gap blocks §3 (every archetype
maps; the only LOW rows are intrinsic mod-variant limits, not Heracles limits).

---

## DEFINITIVE RECOMMENDED ARCHITECTURE

Two decoupled pipelines. **Grounding** runs per pack-assembly; **Detection** runs per
quest-graph generation and consumes grounding output.

```
GROUNDING (once per resolved pack; cache-key = mod-set hash)
  resolved pack jars + datapacks
    └─ Tier 1  static scrape ───────────────► provisional AllowedIndex
         (data/<ns>/advancements,recipes,loot_tables,tags,worldgen/{biome,structure,
          dimension}; assets/<ns>/lang for item/block/entity surface)
         AUTHORITATIVE now for: advancements, recipes, biomes, structures,
         dimensions, tags.  PROVISIONAL for: items, entities, blocks.
    └─ Tier 2  first headless dump pass (launcher-driven, automatic, no human):
         throwaway server dir = resolved mods + Modrinth `registry-dump` +
         eula.txt + superflat server.properties → dedicated server nogui →
         wait ready (60–180s, crash→fall back to Tier-1) → stdin `/dump registry`;
         `stop` → parse dump/**/*.json → COMPLETE AllowedIndex (items/blocks/
         entities/biomes/structures/recipes/tags incl. code- & KubeJS-registered)
         → discard throwaway dir (live instance never sees the helper mod)
         → re-validate already-generated quests against the final index
    └─ persist resolved AllowedIndex beside anvil-quests.json

DETECTION (per quest-graph; never emits an ungrounded id)
  QuestGraph IR  ──validate every task/reward id ∈ AllowedIndex (FULL id, not
                   namespace) ; downgrade per §2 ladder when an id is absent──►
  deterministic Heracles serializer (stable 16-hex ids)
    objective→detector map (§3): prefer native heracles:<task>; advancement
    fallback only for objectives Heracles can't express, emitting a synthetic
    datapack advancement whose ids are ALL drawn from the grounded index, and a
    heracles:advancement task pointing at it.
  → config/heracles/quests/<id>.json (+ groups.txt)
```

Hard invariants:
1. The serializer NEVER writes an id not present in the resolved (Tier-2) AllowedIndex;
   pre-Tier-2 it may only write ids present in Tier-1's authoritative classes.
2. Variant disambiguation never invents NBT; absent a distinct type/tag it emits the
   coarse objective + a transparency note (consistent with the existing
   substitution-transparency principle).
3. Heracles is the emit target (Modrinth-only constraint). Field shapes per the
   codec-verified §5 table — current `quest.rs` has 6 field-name bugs to fix.
4. Grounding's first-launch dump is the canonical source; static scrape is the offline
   fallback that makes the system usable before first launch.
