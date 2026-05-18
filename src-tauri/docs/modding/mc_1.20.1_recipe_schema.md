# Minecraft Java Edition 1.20.1 — Datapack Recipe JSON: Authoritative Schema Reference

Scope: the exact JSON a 1.20.1 client/server accepts under `data/<ns>/recipes/<name>.json`
(Anvil ships these via Open Loader at
`config/openloader/data/anvil-recipes/data/<ns>/recipes/<name>.json`, ids `anvil:<hex>`).

PIN WARNING: the LIVE minecraft.wiki documents 1.21+ and is WRONG for 1.20.1. It shows
`result.id`, `components`, bare-string/array-string ingredients, `crafting_transmute`,
`crafting_dye`. NONE of those exist in 1.20.1. This reference is grounded on the PRIMARY
source: the actual vanilla recipe JSON files Mojang ships for 1.20.1, mirrored verbatim at
`misode/mcmeta` branch `1.20.1-data`, path `data/minecraft/recipes/*.json`. Every shape
below was read from a real 1.20.1 vanilla file (cited per type).

---

## A. EVERY RECIPE `type` VALID IN 1.20.1

There are exactly these recipe `type`s in 1.20.1:

| type | data-bearing? |
|---|---|
| `minecraft:crafting_shaped` | yes |
| `minecraft:crafting_shapeless` | yes |
| `minecraft:smelting` | yes |
| `minecraft:blasting` | yes |
| `minecraft:smoking` | yes |
| `minecraft:campfire_cooking` | yes |
| `minecraft:stonecutting` | yes |
| `minecraft:smithing_transform` | yes |
| `minecraft:smithing_trim` | yes |
| `minecraft:crafting_special_armordye` | NO (type[+category] only) |
| `minecraft:crafting_special_bannerduplicate` | NO |
| `minecraft:crafting_special_bookcloning` | NO |
| `minecraft:crafting_special_firework_rocket` | NO |
| `minecraft:crafting_special_firework_star` | NO |
| `minecraft:crafting_special_firework_star_fade` | NO |
| `minecraft:crafting_special_mapcloning` | NO |
| `minecraft:crafting_special_mapextending` | NO |
| `minecraft:crafting_special_repairitem` | NO |
| `minecraft:crafting_special_shielddecoration` | NO |
| `minecraft:crafting_special_shulkerboxcoloring` | NO |
| `minecraft:crafting_special_suspiciousstew` | NO |
| `minecraft:crafting_special_tippedarrow` | NO |
| `minecraft:crafting_decorated_pot` | NO (type[+category] only) |

The 12 `crafting_special_*` ids above are the COMPLETE 1.20.1 set, verified against the
authoritative 1.20.1 `recipe_serializer` REGISTRY (misode/mcmeta `1.20.1-registries`,
`recipe_serializer/data.json`). That registry's full contents are exactly these 23
serializers: `blasting`, `campfire_cooking`, `crafting_decorated_pot`,
`crafting_shaped`, `crafting_shapeless`, `crafting_special_armordye`,
`crafting_special_bannerduplicate`, `crafting_special_bookcloning`,
`crafting_special_firework_rocket`, `crafting_special_firework_star`,
`crafting_special_firework_star_fade`, `crafting_special_mapcloning`,
`crafting_special_mapextending`, `crafting_special_repairitem`,
`crafting_special_shielddecoration`, `crafting_special_shulkerboxcoloring`,
`crafting_special_suspiciousstew`, `crafting_special_tippedarrow`, `smelting`,
`smithing_transform`, `smithing_trim`, `smoking`, `stonecutting`. Any `type` NOT in
this list is invalid in 1.20.1. (`crafting_special_suspiciousstew` IS registered even
though vanilla makes suspicious stew via ordinary shapeless recipes — the handler
exists and a datapack may use it.)

NOTE — `crafting_transmute` and `crafting_dye` DO NOT EXIST in 1.20.1 (added 1.21.2 /
1.21.5). The live wiki listing them is the 1.21+ contamination. Do not emit them.

The `type` value should be the full namespaced id `minecraft:<x>`. A bare `<x>` (no
namespace) is accepted by the game (defaults to `minecraft`) but vanilla always writes
the explicit `minecraft:` prefix; emit it explicitly.

---

### A.1 `minecraft:crafting_shaped`
Source: vanilla `recipes/iron_pickaxe.json`, `stick.json`, `diamond_block.json`, `tnt.json`.

Fields:
| field | req? | type | default | notes |
|---|---|---|---|---|
| `type` | required | string | — | `"minecraft:crafting_shaped"` |
| `pattern` | required | array of 1–3 strings | — | each string ≤3 chars; all rows EQUAL length; a space `" "` = empty slot |
| `key` | required | object | — | maps each non-space pattern char (1 char) → Ingredient |
| `result` | required | ItemStack object | — | `{"item": "...", "count": n}`; `count` optional, default 1 |
| `category` | optional | string | none/"misc" in book | one of `building`,`equipment`,`misc`,`redstone` |
| `group` | optional | string | — | recipe-book grouping key |
| `show_notification` | optional | boolean | `true` | toast on unlock; vanilla writes it explicitly |

Minimal correct example (vanilla `diamond_block.json`):
```json
{
  "type": "minecraft:crafting_shaped",
  "pattern": ["###", "###", "###"],
  "key": { "#": { "item": "minecraft:diamond" } },
  "result": { "item": "minecraft:diamond_block" }
}
```
Full example with count + tag key + alternatives (vanilla `tnt.json`, `stick.json`):
```json
{
  "type": "minecraft:crafting_shaped",
  "category": "redstone",
  "group": "sticks",
  "pattern": ["X#X", "#X#", "X#X"],
  "key": {
    "#": [ { "item": "minecraft:sand" }, { "item": "minecraft:red_sand" } ],
    "X": { "item": "minecraft:gunpowder" }
  },
  "result": { "item": "minecraft:tnt", "count": 4 },
  "show_notification": true
}
```

### A.2 `minecraft:crafting_shapeless`
Source: vanilla `recipes/book.json`, `fire_charge.json`, `firework_rocket_simple.json`.

Fields:
| field | req? | type | default | notes |
|---|---|---|---|---|
| `type` | required | string | — | `"minecraft:crafting_shapeless"` |
| `ingredients` | required | array of 1–9 Ingredients | — | one entry per input item; MAX 9 |
| `result` | required | ItemStack object | — | `{"item","count"}`, `count` default 1 |
| `category` | optional | string | — | `building`,`equipment`,`misc`,`redstone` |
| `group` | optional | string | — | |
| `show_notification` | optional | boolean | `true` | |

Minimal correct example (vanilla `book.json`):
```json
{
  "type": "minecraft:crafting_shapeless",
  "ingredients": [
    { "item": "minecraft:paper" },
    { "item": "minecraft:paper" },
    { "item": "minecraft:paper" },
    { "item": "minecraft:leather" }
  ],
  "result": { "item": "minecraft:book" }
}
```
(`fire_charge.json` shows an array-of-alternatives entry INSIDE `ingredients`:
`[ {"item":"minecraft:coal"}, {"item":"minecraft:charcoal"} ]` as one of the entries,
plus `"result": { "count": 3, "item": "minecraft:fire_charge" }`.)

### A.3 `minecraft:smelting` (furnace)
Source: vanilla `recipes/iron_ingot_from_smelting_iron_ore.json`.

Fields:
| field | req? | type | default | notes |
|---|---|---|---|---|
| `type` | required | string | — | `"minecraft:smelting"` |
| `ingredient` | required | Ingredient (object or array) | — | NOT a bare string in 1.20.1 |
| `result` | required | **bare item-id STRING** | — | `"minecraft:iron_ingot"` — NOT an object, NO count |
| `experience` | optional | float | `0.0` | XP on collect |
| `cookingtime` | optional | int (ticks) | `200` | |
| `category` | optional | string | — | `food`,`blocks`,`misc` |
| `group` | optional | string | — | |

Correct example (verbatim vanilla):
```json
{
  "type": "minecraft:smelting",
  "category": "misc",
  "cookingtime": 200,
  "experience": 0.7,
  "group": "iron_ingot",
  "ingredient": { "item": "minecraft:iron_ore" },
  "result": "minecraft:iron_ingot"
}
```

### A.4 `minecraft:blasting`
Source: vanilla `iron_ingot_from_blasting_iron_ore.json`. Identical schema to smelting;
only `type` differs and the vanilla default `cookingtime` is `100`. `category` allowed
values for blasting are `blocks`, `misc` ONLY — there is no `food` blasting category
(Mojang's cooking-category enum has no blast-food member). Do NOT emit
`"category":"food"` on a blasting recipe.
```json
{
  "type": "minecraft:blasting",
  "category": "misc",
  "cookingtime": 100,
  "experience": 0.7,
  "group": "iron_ingot",
  "ingredient": { "item": "minecraft:iron_ore" },
  "result": "minecraft:iron_ingot"
}
```

### A.5 `minecraft:smoking`
Source: vanilla `cooked_beef_from_smoking.json`. Same schema as smelting; `result` is a
bare string; vanilla default `cookingtime` `100`; `category` always `food` in vanilla.
```json
{
  "type": "minecraft:smoking",
  "category": "food",
  "cookingtime": 100,
  "experience": 0.35,
  "ingredient": { "item": "minecraft:beef" },
  "result": "minecraft:cooked_beef"
}
```

### A.6 `minecraft:campfire_cooking`
Source: vanilla `cooked_beef_from_campfire_cooking.json`. Same schema as smelting;
`result` bare string; vanilla `cookingtime` `600`; `category` `food` in vanilla.
```json
{
  "type": "minecraft:campfire_cooking",
  "category": "food",
  "cookingtime": 600,
  "experience": 0.35,
  "ingredient": { "item": "minecraft:beef" },
  "result": "minecraft:cooked_beef"
}
```

### A.7 `minecraft:stonecutting`
Source: vanilla `stone_stairs_from_stone_stonecutting.json`.

Fields — DISTINCT SHAPE, easy to get wrong:
| field | req? | type | default | notes |
|---|---|---|---|---|
| `type` | required | string | — | `"minecraft:stonecutting"` |
| `ingredient` | required | Ingredient (object or array) | — | |
| `result` | required | **bare item-id STRING** | — | NOT an object |
| `count` | required | int | — | **TOP-LEVEL** `count`, NOT inside result |
| `group` | optional | string | — | |

NO `category`, NO `experience`, NO `show_notification` for stonecutting.
Correct example (verbatim vanilla):
```json
{
  "type": "minecraft:stonecutting",
  "ingredient": { "item": "minecraft:stone" },
  "result": "minecraft:stone_stairs",
  "count": 1
}
```

### A.8 `minecraft:smithing_transform`
Source: vanilla `netherite_pickaxe_smithing.json`, `netherite_chestplate_smithing.json`.

Fields:
| field | req? | type | default | notes |
|---|---|---|---|---|
| `type` | required | string | — | `"minecraft:smithing_transform"` |
| `template` | required | Ingredient | — | required in 1.20.1 (smithing template, added 1.20) |
| `base` | required | Ingredient | — | the item being upgraded |
| `addition` | required | Ingredient | — | the material |
| `result` | required | ItemStack object | — | `{"item":"..."}`; vanilla writes NO `count` (always 1) |

NO `category`, NO `group`, NO `show_notification`, NO top-level `count`.
Correct example (verbatim vanilla):
```json
{
  "type": "minecraft:smithing_transform",
  "template": { "item": "minecraft:netherite_upgrade_smithing_template" },
  "base": { "item": "minecraft:diamond_pickaxe" },
  "addition": { "item": "minecraft:netherite_ingot" },
  "result": { "item": "minecraft:netherite_pickaxe" }
}
```

### A.9 `minecraft:smithing_trim`
Source: vanilla `coast_armor_trim_smithing_template_smithing_trim.json`.

Fields:
| field | req? | type | default | notes |
|---|---|---|---|---|
| `type` | required | string | — | `"minecraft:smithing_trim"` |
| `template` | required | Ingredient | — | the trim template item |
| `base` | required | Ingredient | — | trimmable armor (vanilla uses a tag) |
| `addition` | required | Ingredient | — | trim material (vanilla uses a tag) |

NO `result` field at all (the game synthesizes the trimmed item from `base`). NO
`category`/`group`/`show_notification`. Correct example (verbatim vanilla):
```json
{
  "type": "minecraft:smithing_trim",
  "template": { "item": "minecraft:coast_armor_trim_smithing_template" },
  "base": { "tag": "minecraft:trimmable_armor" },
  "addition": { "tag": "minecraft:trim_materials" }
}
```

### A.10 `minecraft:crafting_special_*` and `minecraft:crafting_decorated_pot`
Source: vanilla `armor_dye.json`, `map_extending.json`, `tipped_arrow.json`,
`repair_item.json`, `book_cloning.json`, `banner_duplicate.json`, `firework_star.json`,
`firework_star_fade.json`, `decorated_pot.json`.

These are hard-coded recipe handlers. The JSON carries `type` and OPTIONALLY `category`.
NOTHING ELSE. No `pattern`/`key`/`ingredients`/`result`/`group`/`show_notification`.
Adding any other field is non-conformant.
Verbatim vanilla:
```json
{ "type": "minecraft:crafting_special_armordye", "category": "misc" }
```
```json
{ "type": "minecraft:crafting_decorated_pot", "category": "misc" }
```
Full data-bearing `crafting_special_*` id set is the table in section A.

---

## B. INGREDIENT FORMAT (1.20.1)

An Ingredient is ALWAYS one of:

1. Single item — object `{ "item": "<namespaced_id>" }`
   e.g. `{ "item": "minecraft:iron_ingot" }`
2. Item tag — object `{ "tag": "<namespaced_tag>" }` (NO leading `#` in datapack JSON;
   the `#` form is only for the 1.21+ string syntax which 1.20.1 does NOT support)
   e.g. `{ "tag": "minecraft:planks" }`
3. Array of alternatives — a JSON array whose elements are form (1) or (2):
   `[ { "item": "minecraft:coal" }, { "item": "minecraft:charcoal" } ]`
   Matches any one of the listed entries.

A bare string `"minecraft:iron_ingot"` as an ingredient is INVALID in 1.20.1 (that is
1.21+ syntax). Every vanilla 1.20.1 ingredient is an object or an array of objects.

Where each form is allowed:
- `crafting_shaped.key.<char>`: forms 1, 2, 3 (`tnt.json` uses an array).
- `crafting_shapeless.ingredients[i]`: forms 1, 2, 3 (`fire_charge.json` uses an array
  as one element).
- `smelting/blasting/smoking/campfire_cooking.ingredient`: forms 1, 2, 3.
- `stonecutting.ingredient`: forms 1, 2, 3.
- `smithing_transform.template/base/addition`: forms 1, 2, 3 (single object in vanilla).
- `smithing_trim.template/base/addition`: forms 1, 2, 3 (vanilla uses tags for
  `base`/`addition`).

`item` / `tag` values must be valid resource locations: `[a-z0-9_.-]` path,
`[a-z0-9_.-]` namespace, single `:`. A missing item silently drops the recipe at load.

---

## C. RESULT FORMAT PER TYPE (1.20.1) — SUMMARY

| recipe type | result shape |
|---|---|
| `crafting_shaped` | OBJECT `{ "item": "<id>", "count": <int default 1> }` |
| `crafting_shapeless` | OBJECT `{ "item": "<id>", "count": <int default 1> }` |
| `smelting` | BARE STRING `"<id>"` + sibling `experience` (float) + `cookingtime` (int) |
| `blasting` | BARE STRING `"<id>"` + `experience` + `cookingtime` |
| `smoking` | BARE STRING `"<id>"` + `experience` + `cookingtime` |
| `campfire_cooking` | BARE STRING `"<id>"` + `experience` + `cookingtime` |
| `stonecutting` | BARE STRING `"<id>"` `result` + a SEPARATE TOP-LEVEL `count` (int) |
| `smithing_transform` | OBJECT `{ "item": "<id>" }` (vanilla writes no `count`) |
| `smithing_trim` | NO result field (trimmed item synthesized from `base`) |
| `crafting_special_*` / `crafting_decorated_pot` | NO result field |

1.20.1-SPECIFIC (vs other versions — pin to 1.20.1):
- Crafting result key is **`item`**, NOT `id`. `id` + `components` is 1.20.5+.
- Cooking (`smelting`/etc.) result is a **bare string**. The object form
  `{"id":..,"count":..}` for cooking results is 1.20.5+ (1.20.1 has neither object form
  nor a cooking `count`).
- Stonecutting uses a **bare-string `result` + top-level `count`**. The
  `result:{id,count}` object is 1.20.5+.
- `smithing_transform` result is an object `{"item"}`; no `count` written by vanilla.
- No `components` field exists anywhere in 1.20.1 result objects.

---

## D. COMMON PITFALLS A 1.20.1 GENERATOR MUST RESPECT

1. Result shape is NOT uniform across types. Crafting = `{"item","count"}` object;
   cooking = bare string + sibling `experience`/`cookingtime`; stonecutting = bare
   string `result` + sibling top-level `count`; smithing_transform = `{"item"}` object;
   smithing_trim / special = no result. Emitting a crafting-style object as a smelting
   `result` is rejected (silent load failure).
2. NEVER use `result.id` or `result.components` for 1.20.1 — those are 1.20.5+. Use
   `result.item`.
3. Ingredients are objects (`{"item"}`/`{"tag"}`) or arrays of such objects — NEVER bare
   strings, NEVER `"#tag"` strings (1.21+ only). Tags use `{"tag":"ns:path"}` with NO
   leading `#`.
4. Shaped constraints: `pattern` is 1–3 rows; every row same length; each row ≤3 chars;
   space `" "` is the only empty slot; every non-space char in `pattern` MUST have a
   `key` entry; every `key` char MUST appear in `pattern`; key chars are single
   characters (1 grapheme).
5. Shapeless: `ingredients` length 1–9 (a 3×3 grid maximum). >9 is invalid.
6. `group`: optional free string; only groups recipe-book entries; not validated as a
   resource location; legal on shaped/shapeless/cooking; vanilla does NOT put it on
   stonecutting/smithing/special (harmless but pointless there).
7. `category`: optional. Allowed values are TYPE-DEPENDENT and confirmed by sampling
   vanilla 1.20.1 data:
   - crafting (shaped/shapeless/special/decorated_pot): `building`, `equipment`,
     `misc`, `redstone`
   - smelting: `food`, `blocks`, `misc`
   - blasting: `blocks`, `misc` (NO `food` — invalid for blasting)
   - smoking: `food`
   - campfire_cooking: `food`
   - stonecutting, smithing_transform, smithing_trim: NO `category` field (vanilla never
     emits one; do not add it)
   An unrecognized category value is rejected at datapack load.
8. `show_notification`: optional boolean, default `true`, legal ONLY on the two crafting
   types (shaped/shapeless). Mojang's codec accepts it on both; vanilla writes it
   explicitly on `crafting_shaped` (observed in the data scan) and it is also valid on
   `crafting_shapeless`. Not present/valid on cooking/stonecutting/smithing/special; do
   not add it there.
9. `crafting_special_*` and `crafting_decorated_pot` MUST contain ONLY `type` (and
   optionally `category`). Any `pattern`/`key`/`ingredients`/`result`/`group`/
   `show_notification` makes them non-conformant.
10. `crafting_transmute` / `crafting_dye` do not exist in 1.20.1 — never emit them.
11. The recipe id is the FILE PATH (`data/<ns>/recipes/<name>.json`), not a JSON field.
    There is NO `id`/`name` field inside recipe JSON. `<ns>` and `<name>` must be valid
    resource-location segments (`[a-z0-9_.-]`, `/` allowed in `<name>` for subfolders).
12. `experience` is a float (`0.0` legal, often omitted → defaults `0.0`);
    `cookingtime` is an int in ticks (defaults `200`; vanilla uses 100 for blast/smoke,
    600 for campfire). `count` is an int ≥1.
13. 1.20.1 recipes directory is PLURAL `recipes/`. (1.21 renamed it to singular
    `recipe/`. Anvil pins 1.20.1 → plural.)

---

## E. GAP ANALYSIS vs `/Users/joshuakim/Software Projects/anvil/src-tauri/src/recipe.rs`

### E.1 What Anvil supports today
`RecipeKind` (recipe.rs:89-116) has exactly THREE variants:
- `Shaped` → `minecraft:crafting_shaped` (recipe.rs:92-99, serialized recipe.rs:258-282)
- `Shapeless` → `minecraft:crafting_shapeless` (recipe.rs:100-105, serialized 283-299)
- `Smelting` → `minecraft:smelting` (recipe.rs:106-115, serialized 300-315)

### E.2 Conformance of what IS emitted (verified CORRECT for 1.20.1)
- Shaped `result` emitted as object `{"item","count"}` (recipe.rs:277-280) — CORRECT
  for 1.20.1 (matches vanilla `iron_pickaxe.json`).
- Shapeless `result` object `{"item","count"}` (recipe.rs:294-297) — CORRECT.
- Smelting `result` emitted as a BARE STRING (recipe.rs:307-314, `"result": result`
  where `result: String`) — CORRECT for 1.20.1 (the recipe.rs:41-44 / :109 / :306
  comment is right; verified against vanilla `iron_ingot_from_smelting_iron_ore.json`).
- Ingredient enum serializes to `{"item":..}` / `{"tag":..}` via `#[serde(untagged)]`
  (recipe.rs:128-133) — CORRECT single-item / tag forms for 1.20.1.
- Smelting `experience` (f64) + `cookingtime` (i64, default 200) (recipe.rs:111-120,
  emitted 312-313) — CORRECT field names/types/default.
- `pack.mcmeta` `pack_format` 15 for 1.20.1 (recipe.rs:65, 241-250) — CORRECT.
- Plural `recipes/` directory (recipe.rs:205-210, 321) — CORRECT for 1.20.1.
- Structural validation matches the 1.20.1 constraints in §D: pattern ≤3×3 / equal rows
  (recipe.rs:421-431), key-not-in-pattern (439-447), unbound pattern char (449-458),
  shapeless 1..=9 (460-466). All CORRECT.

### E.3 Missing recipe TYPES (Anvil cannot emit these — section A items absent)
- `minecraft:blasting` — NOT supported
- `minecraft:smoking` — NOT supported
- `minecraft:campfire_cooking` — NOT supported
- `minecraft:stonecutting` — NOT supported
- `minecraft:smithing_transform` — NOT supported
- `minecraft:smithing_trim` — NOT supported
- ALL `minecraft:crafting_special_*` (12 ids) — NOT supported
- `minecraft:crafting_decorated_pot` — NOT supported

Note: blasting/smoking/campfire are schema-identical to the existing `Smelting` variant
(only `type` + vanilla cookingtime default differ), so they are the cheapest to add —
the engine already has the exact ingredient/bare-string-result/experience/cookingtime
shape they need.

### E.4 Missing FIELDS on the types Anvil DOES support
On `Shaped`/`Shapeless` (recipe.rs:92-105) Anvil omits these OPTIONAL 1.20.1 fields:
- `category` — not modeled. Legal-but-absent; recipe still loads (category only affects
  recipe-book placement). Non-blocking but a fidelity gap.
- `group` — not modeled. Same: legal-but-absent.
- `show_notification` — not modeled; the game defaults it to `true`, so the emitted
  recipe is conformant (vanilla writes it explicitly but it is optional). Fidelity gap
  only.
On `Smelting` (recipe.rs:106-115): `category` and `group` not modeled — legal-but-absent,
non-blocking.
None of these omissions make emitted JSON INVALID — they are all optional in 1.20.1.

### E.5 Non-conformance / correctness risks in emitted JSON
- `ItemStack` always serializes `count` (recipe.rs:135-144 + explicit
  `"count": result.count` at recipe.rs:279, 296). Vanilla often omits `count` when 1,
  but an explicit `"count": 1` IS valid 1.20.1 (e.g. effectively what the schema allows;
  `stonecutting`/others aside). NOT a bug — just differs cosmetically from vanilla
  minimalism. No action required.
- `Ingredient` (recipe.rs:128-133) models ONLY single `{"item"}` / `{"tag"}`. It CANNOT
  represent the array-of-alternatives ingredient form
  (`[{"item":..},{"item":..}]`) that 1.20.1 fully supports (vanilla `tnt.json` key,
  `fire_charge.json` ingredient). This is an EXPRESSIVENESS gap, not an invalidity:
  every recipe Anvil can model is still valid; it just can't author "A or B" inputs.
- Grounding is namespace-level only (recipe.rs:46-54): a syntactically valid recipe
  referencing a nonexistent exact id in an allowed namespace passes validation and then
  silently fails to load in-game. Documented accepted seam, not a schema defect.
- No validation that `result`/`item`/`tag` strings are well-formed resource locations
  (only namespace grounding). A value like `"Minecraft:Iron Ingot"` would pass
  `validate_recipes` yet be rejected by the game. Minor robustness gap.

### E.6 Bottom line
Everything `recipe.rs` emits today is SCHEMA-CONFORMANT for 1.20.1 (the load-bearing
"smelting result is a bare string in 1.20.1" claim in the file's header comment is
CORRECT — verified against Mojang's shipped vanilla data). The gaps are (a) 6 unmodeled
data-bearing types + special/decorated_pot, (b) the array-of-alternatives ingredient
form, and (c) optional cosmetic fields (`category`/`group`/`show_notification`). None of
(c) and none of the present code produce invalid JSON; (a) and (b) are
coverage/expressiveness limits, not correctness bugs.

---

SOURCES
- PRIMARY: Mojang vanilla 1.20.1 recipe JSON, verbatim mirror `misode/mcmeta` branch
  `1.20.1-data`, `data/minecraft/recipes/*.json` (every per-type example above quotes a
  named file; category value space confirmed by sampling 250 vanilla files).
- AUDIT TARGET: `/Users/joshuakim/Software Projects/anvil/src-tauri/src/recipe.rs`
  (line numbers cited inline in section E).
- The live minecraft.wiki Recipe page documents 1.21+ and was NOT used as a 1.20.1
  authority (it disagrees with the primary source on result/ingredient shapes).
