# Origins / Apoli 2.9.2 (Fabric 1.20.1) — Authoritative Datapack Schema Reference

Target stack: **Origins 1.10.2 + Apoli 2.9.2 + Calio** (`Origins-1.10.2+mc.1.20.x.jar`, JIJ `apoli-2.9.2+mc.1.20.x.jar`).

Source legend used in citations:
- **[DOC]** = https://origins.readthedocs.io/en/latest/ (page path given)
- **[JAR-A]** = decompiled Apoli 2.9.2 at `/tmp/apoli` (javap bytecode)
- **[JAR-O]** = decompiled Origins 1.10.2 at `/tmp/origins_jar` (javap bytecode + stock `data/origins/...` jsons)
- **[LOG]** = runtime evidence, `~/.anvil/instances/18b09b45cf64ee883408/logs/latest.log`

> **Ground-truth rule:** where [DOC] conflicts with [JAR-*]/[LOG], the jar/log wins for this exact version. The docs track a newer Origins; several field *types* differ.

---

## 0. The single most important fact (read first)

**`name` and `description` are plain JSON STRINGS in this version — never text-component objects.**

- Origin: `Origin.fromJson` registers `name` and `description` as `io.github.apace100.calio.data.SerializableDataTypes.STRING` (verified in `Origin.class` constant pool / `DATA` static init). [JAR-O]
- Power: `PowerTypes.readPower` reads `name`/`description` via `net.minecraft.class_3518.method_15253` = vanilla `JsonHelper.getString(JsonObject, key, default)`, which throws if the value is a JSON object. [JAR-A]
- Runtime proof: `There was a problem reading power file anvil:* (skipping): Expected name to be a string, was an object ({"te...})` and `There was a problem reading Origin file anvil:* (skipping): Error reading data field at name: JsonObject`. [LOG lines 405–417]
- [DOC] (`/json/origin/`, `/json/power/`) calls these "Text Component" — **this is wrong for 1.10.2 / 2.9.2.** Emit `"name": "Tank"`, not `"name": {"text":"Tank"}`. Use lang-file translation keys or literal strings only.

---

## A. Origin JSON schema — `data/<namespace>/origins/<id>.json`

Source of field types: `Origin.fromJson` / `Origin.DATA` static initializer [JAR-O], cross-checked with [DOC] `/json/origin/` and stock `data/origins/origins/*.json` [JAR-O].

| Field | JSON type | Calio type [JAR-O] | Req? | Default | Notes |
|---|---|---|---|---|---|
| `powers` | array of identifier strings | `IDENTIFIERS` | optional | `[]` | Power IDs this origin grants. Each must resolve to a loaded power file `data/<ns>/powers/<id>.json`. |
| `icon` | item stack object | ItemStack | optional | empty | Shown in selection screen. Minimal: `{"item":"minecraft:cod"}`. |
| `unchoosable` | boolean | `BOOLEAN` | optional | `false` | If `true`, hidden from selection; settable via command/upgrade. |
| `order` | integer | `INT` | optional | `0` | Sort position among origins of equal `impact` in a layer. |
| `impact` | integer | `INT` | optional | `0` | 0=none … 3=high. Out-of-range still parses but render is clamped. |
| `name` | **string** | **`STRING`** | optional | translation key `origin.<ns>.<path>.name` | PLAIN STRING. Literal text or a lang key. |
| `description` | **string** | **`STRING`** | optional | translation key `origin.<ns>.<path>.description` | PLAIN STRING. |
| `loading_priority` | integer | `INT` | optional | `0` | Higher = loaded later = overrides lower priority same-id origins across datapacks. |
| `upgrades` | array of upgrade objects | upgrade list | optional | none | Each: `{ "condition": "<advancement id>", "origin": "<ns:id>", "announcement": "<string>" }`. Advancement-gated origin transformation. |

Field-name LDC order observed in `Origin.fromJson`: `icon, impact, order, loading_priority, unchoosable, powers, name, description, upgrades`. [JAR-O]

**Minimal valid origin (correct for this version):**
```json
{
  "icon": { "item": "minecraft:netherite_chestplate" },
  "order": 0,
  "impact": 2,
  "name": "Tank",
  "description": "Built like a wall.",
  "powers": [
    "anvil:tank_max_health",
    "anvil:tank_armor"
  ]
}
```
(`anvil:tank` failed in [LOG 415] **only** because `name`/`description` were objects — the structure above is otherwise correct.)

---

## B. Origin Layer JSON schema — `data/<namespace>/origin_layers/<id>.json`

To append to the stock layer, the file path **must be** `data/origins/origin_layers/origin.json` (namespace `origins`, id `origin`). Field types from `OriginLayer` / `OriginLayer.createFromData` [JAR-O], cross-checked [DOC] `/json/origin_layer/` and stock `data/origins/origin_layers/origin.json` [JAR-O].

| Field | JSON type | Req? | Default | Notes |
|---|---|---|---|---|
| `origins` | array (string \| object) | **required** | — | Mixed: identifier strings and/or `{origin, condition}` objects (see below). |
| `replace` | boolean | optional | `false` | `false` = **append** this file's `origins` to other datapacks' versions of the same layer (this is how you add to vanilla without wiping it). `true` = this file's `origins` *replace* all others. |
| `order` | integer | optional | `0` | Layer display order; smaller first. |
| `enabled` | boolean | optional | `true` | If `false`, layer is inert. |
| `name` | **string** | optional | translation key | Layer display name. **PLAIN STRING** — `OriginLayer` reads it via `JsonHelper.getString` (`class_3518.method_15253`), bytecode-verified [JAR-O]; an object value is rejected exactly like Origin/Power `name`. |
| `gui_title` | object | optional | none | `{ "choose_origin": "<string>", "view_origin": "<string>" }` — strings read via `JsonHelper.getString` [JAR-O], so each is a PLAIN STRING. |
| `missing_name` | **string** | optional | key | Shown when no origin chosen. **PLAIN STRING** (`JsonHelper.getString`, bytecode-verified [JAR-O]). |
| `missing_description` | **string** | optional | key | **PLAIN STRING** (`JsonHelper.getString`, bytecode-verified [JAR-O]). |
| `allow_random` | boolean | optional | `false` | Show the "Random" option. |
| `allow_random_unchoosable` | boolean | optional | `false` | Random may pick `unchoosable` origins. |
| `exclude_random` | array of identifiers | optional | `[]` | Origins excluded from random. |
| `replace_exclude_random` | boolean | optional | `false` | If true, replace (not merge) inherited `exclude_random`. [JAR-O OriginLayer field] |
| `default_origin` | identifier | optional | none | Auto-assigned origin for new players (skips selection for this layer). |
| `auto_choose` | boolean | optional | `false` | Auto-pick when only one valid origin. |
| `hidden` | boolean | optional | `false` | Hide layer from "View Origin" screen. |
| `loading_priority` | integer | optional | `0` | Higher loads later (override). |

`origins[]` entry forms [DOC `/json/origin_layer/`, stock files]:
- **String form:** `"anvil:tank"`
- **Object (conditioned) form:** `{ "origin": "anvil:tank", "condition": { /* entity condition */ } }` — origin only selectable if condition passes.
- Forms may be mixed in one array.

**Merging across datapacks:** layers with the same id from different datapacks are merged when `replace:false`. Each datapack's `origins`/`exclude_random` are concatenated; scalar fields (`order`, `name`, …) resolved by `loading_priority` (highest wins). Stock `origins:origin` layer is `replace:false` with the 10 vanilla origins [JAR-O `data/origins/origin_layers/origin.json`].

**Minimal file that appends custom origins WITHOUT wiping vanilla** — path `data/origins/origin_layers/origin.json`:
```json
{
  "replace": false,
  "origins": [
    "anvil:tank",
    "anvil:mobility",
    "anvil:survivalist"
  ]
}
```
This is the exact pattern proven safe in [LOG 420–421] (`Trying to read layer file: origins:origin`) and matches stock structure.

---

## C. Power JSON common envelope — `data/<namespace>/powers/<id>.json`

Envelope fields read by `PowerTypes.readPower` [JAR-A] before the type factory parses the rest:

| Field | JSON type | Reader [JAR-A] | Req? | Default | Notes |
|---|---|---|---|---|---|
| `type` | identifier | resolved via `ApoliRegistries.POWER_FACTORY` + `NamespaceAlias` | **required** | — | Must be a registered factory id (see D). Unknown → `Power type "X" is not defined.` and the whole power is skipped [LOG 406]. |
| `name` | **string** | `JsonHelper.getString` (`class_3518.method_15253`) | optional | `""` | **PLAIN STRING ONLY.** Object → hard skip [LOG]. |
| `description` | **string** | `JsonHelper.getString` | optional | `""` | **PLAIN STRING ONLY.** |
| `hidden` | boolean | `JsonHelper.getBoolean` (`method_15258`) | optional | `false` | Hide from power list. |
| `loading_priority` | integer | `JsonHelper.getInt` (`method_15282`) | optional | `0` | Override across datapacks. |
| `condition` | entity condition object | factory data (`PowerFactory` adds optional `condition`) | optional | none | Power only active while this entity condition holds. Added on every factory via `PowerFactory.allowCondition()`. |
| `badges` | array of badge objects/ids | Origins-side | optional | none | Icons after the power name; [DOC `/json/power/`]. Not part of Apoli envelope. |

**Namespace alias (decisive):** `Origins` calls `io.github.apace100.apoli.util.NamespaceAlias.addAlias("origins", "apoli")` [JAR-O `Origins.class`]. Therefore **`origins:<x>` resolves to `apoli:<x>`** for every power/condition/action type. There are **no separate `origins:*` power factories** — Origins ships only one power class (`OriginsCallbackPower`) and otherwise relies entirely on Apoli factories via this alias. Either namespace is valid; generator should prefer `apoli:` for clarity (both work).

### Multiple / sub-powers — `apoli:multiple`

`apoli:multiple` (alias `origins:multiple`) — `MultiplePowerType` holds a list of sub-power ids [JAR-A]. In JSON, every key **other than the envelope keys** is treated as a nested sub-power object (each with its own `type`). At load Apoli auto-creates child powers with id `<parentid>_<key>`. Stock example (`data/origins/powers/climbing.json`) [JAR-O]:
```json
{
  "type": "origins:multiple",
  "toggle":   { "type": "origins:toggle", "key": { "key": "key.origins.primary_active", "continuous": false } },
  "climbing": { "type": "origins:climbing", "hold_condition": { /* ... */ }, "condition": { /* ... */ } }
}
```
Granting `climbing` grants the parent plus auto-generated `climbing_toggle`, `climbing_climbing`. Sub-powers may be referenced as `*:*_toggle` in conditions (stock pattern). For a generator: prefer flat single-type powers; only use `apoli:multiple` when an effect genuinely needs a paired toggle.

---

## D. POWER TYPE CATALOG — Apoli 2.9.2 (the critical deliverable)

**Method:** every id below was extracted from the decompiled jar — `PowerFactories.register()` LDC constants (17) ∪ each `*Power.class`'s `createFactory()/getFactory()` first `Apoli.identifier(<id>)` argument (88) + `MultiplePowerType`/simple. **104 distinct factory ids.** Each is valid as **both** `apoli:<id>` and `origins:<id>` (alias, §C). Any `type` NOT in this set → `Power type "X" is not defined` and the power is skipped — this is exactly why `apoli:water_breathing` failed [LOG 406]: **there is no `water_breathing` factory in either namespace.**

Complexity tiers:
- **S = simple/self-contained** — no required sub-objects; safest for a generator.
- **A = needs an attribute-modifier sub-object** (see E).
- **C = needs a condition/action/other complex sub-object.**

### D.1 Safe simple/self-contained types (tier S) — recommended generator default set

| `type` | Purpose | Required | Key optional |
|---|---|---|---|
| `apoli:simple` | No-op marker power (used as a flag other code keys off, e.g. water breathing). | none | `hidden` |
| `apoli:multiple` | Container for nested sub-powers. | ≥1 sub-power object | — |
| `apoli:fire_immunity` | Immune to fire/lava damage + burning. | none | — |
| `apoli:swimming` | Acts as if swimming (always-swim posture). | none | — |
| `apoli:climbing` | Spider-style wall climb (use inside a `multiple` with a toggle for stock parity, or standalone). | none | `allow_holding`(bool), `hold_condition`(entity cond) |
| `apoli:night_vision` | Permanent/conditional night vision. | none | `strength`(float 0–1, def 1.0), `condition` |
| `apoli:toggle_night_vision` | Night vision toggled by a key. | none | `strength`(float), `active_by_default`(bool), `key`(key obj), `condition` |
| `apoli:invisibility` | Player is invisible. | none | `render_armor`(bool) |
| `apoli:creative_flight` | Creative-style flight. | none | — |
| `apoli:elytra_flight` | Elytra flight without an elytra item. | none | `render_elytra`(bool, def false), `texture_location`(identifier) |
| `apoli:phasing` | Walk through blocks. | none | `render`(bool), `view_distance`(float), `phase_down_condition`, `blocks`(block cond) |
| `apoli:entity_group` | Reassign the entity's group (e.g. `aquatic`, `arthropod`, `undead`) for AI/potion logic. | `group`(string) | `hidden` |
| `apoli:keep_inventory` | Keep inventory on death. | none | `inventory_type`, `drop_on_death` |
| `apoli:prevent_sleep` | Cannot sleep. | none | `affect_spawn`(bool), `message`(string) |
| `apoli:prevent_sprinting` | Cannot sprint. | none | — |
| `apoli:prevent_death` | Cancel death (with conditions/actions if desired). | none | `condition`, `entity_action` |
| `apoli:disable_regen` | Disable natural health regen. | none | — |
| `apoli:freeze` | Apply powder-snow freeze effect. | none | — |
| `apoli:shaking` | Visual shake (cold) effect. | none | — |
| `apoli:grounded` | Treated as on-ground for movement logic. | none | — |
| `apoli:ignore_water` | Movement ignores water (no buoyancy/drag). | none | — |
| `apoli:walk_on_fluid` | Walk on a fluid tag (e.g. water/lava). | `fluid`(fluid tag id) | — |
| `apoli:invulnerability` | Immune to damage matching a damage condition (this is how stock `fire_immunity` is built). | `damage_condition`(damage cond) | — |
| `apoli:effect_immunity` | Immune to listed status effects. | `effect`/`effects` (status effect id(s)) | `condition` |
| `apoli:self_glow` / `apoli:entity_glow` | Glowing outline (self / other entities). | none | `color`, `entities`(bientity cond) |
| `apoli:model_color` | Tint the player model RGBA. | none | `red`,`green`,`blue`,`alpha`(floats) |
| `apoli:tooltip` | Adds tooltip text (cosmetic). | none | — |
| `apoli:status_bar_texture` / `apoli:overlay` / `apoli:shader` | HUD/screen cosmetics. | varies | mostly identifiers |

> Note: `apoli:water_breathing` is intentionally absent — it is **not** a valid type in 2.9.2. See F#2 and "Water breathing — the correct way" below.

### D.2 Attribute-driven types (tier A) — need an attribute-modifier sub-object (see E)

| `type` | Purpose | Required |
|---|---|---|
| `apoli:attribute` | Apply one/more permanent attribute modifiers. **This is the type for: extra health, armor/toughness, movement speed, attack damage, knockback resistance, etc.** | `modifier` OR `modifiers` (single obj or array) |
| `apoli:modify_attribute` | Like `attribute` but expresses an attribute *modification* (same modifier shape). | `modifier`/`modifiers` |
| `apoli:conditioned_attribute` | Attribute modifier(s) active only while a condition holds. | `modifier`/`modifiers`; opt `condition`, `tick_rate` |

Attribute → intent mapping (all via `apoli:attribute`, E for shape):
- **Extra health** → `minecraft:generic.max_health`
- **Armor** → `minecraft:generic.armor`; **toughness** → `minecraft:generic.armor_toughness`
- **Movement speed** → `minecraft:generic.movement_speed`
- **Attack damage** → `minecraft:generic.attack_damage`; **knockback resist** → `minecraft:generic.knockback_resistance`
- **Jump** → there is **no vanilla jump attribute** in 1.20.1; use `apoli:modify_jump` (D.3), not `apoli:attribute`.
- **Slow falling** → use `apoli:modify_falling` (D.3), not an attribute.

### D.3 Modify-* and behaviour types (tier S/C — most are simple scalar fields)

`apoli:modify_falling` (S — **the correct slow-fall type**; fields: `velocity`(double), `take_fall_damage`(bool), opt `condition`),
`apoli:modify_jump` (`apoli:modify_jump` — jump boost; scalar/modifier),
`apoli:modify_swim_speed`, `apoli:modify_air_speed`, `apoli:modify_lava_speed`, `apoli:modify_slipperiness`,
`apoli:modify_velocity`, `apoli:modify_break_speed`, `apoli:modify_harvest`, `apoli:modify_exhaustion`,
`apoli:modify_healing`, `apoli:modify_food`, `apoli:modify_xp_gain`, `apoli:modify_insomnia_ticks`,
`apoli:modify_damage_dealt`, `apoli:modify_damage_taken`, `apoli:modify_projectile_damage`,
`apoli:modify_status_effect_amplifier`, `apoli:modify_status_effect_duration`,
`apoli:modify_camera_submersion`, `apoli:modify_block_render`, `apoli:modify_fluid_render`,
`apoli:modify_crafting`, `apoli:modify_grindstone`, `apoli:modify_player_spawn`.
Most `modify_*` take either a scalar (`velocity`/`amount`) or a value-modifier list; `modify_falling` is the simplest and is the stock slow-fall mechanism (`data/origins/powers/slow_falling.json` uses `apoli:modify_falling` with `velocity:0.01`, `take_fall_damage:false`) [JAR-O].

### D.4 Action / condition-driven types (tier C — require sub-objects)

`apoli:action_on_being_used`, `apoli:action_on_block_break`, `apoli:action_on_block_use`, `apoli:action_on_callback`, `apoli:action_on_entity_use`, `apoli:action_on_hit`, `apoli:action_on_item_use`, `apoli:action_on_land`, `apoli:action_on_wake_up`, `apoli:action_over_time`, `apoli:action_when_damage_taken`, `apoli:action_when_hit`, `apoli:active_self`, `apoli:attacker_action_when_hit`, `apoli:self_action_on_hit`, `apoli:self_action_on_kill`, `apoli:self_action_when_hit`, `apoli:target_action_on_hit`, `apoli:fire_projectile`, `apoli:item_on_item`, `apoli:starting_equipment`, `apoli:inventory`, `apoli:cooldown`, `apoli:resource`, `apoli:toggle`, `apoli:recipe`, `apoli:stacking_status_effect`, `apoli:burn`, `apoli:damage_over_time`, `apoli:exhaust`, `apoli:attribute_modify_transfer`, `apoli:conditioned_restrict_armor`, `apoli:restrict_armor`, `apoli:prevent_being_used`, `apoli:prevent_block_selection`, `apoli:prevent_block_use`, `apoli:prevent_elytra_flight`, `apoli:prevent_entity_collision`, `apoli:prevent_entity_render`, `apoli:prevent_entity_use`, `apoli:prevent_feature_render`, `apoli:prevent_game_event`, `apoli:prevent_item_use`, `apoli:particle`, `apoli:lava_vision`.

> A generator that wants zero-risk packs should restrict itself to D.1 + D.2(`apoli:attribute`) + `apoli:modify_falling` + `apoli:modify_jump`. Everything else needs valid nested condition/action objects.

### Full 104-id list (alphabetical, all valid in 2.9.2; prefix with `apoli:` or `origins:`)
```
action_on_being_used, action_on_block_break, action_on_block_use, action_on_callback,
action_on_entity_use, action_on_hit, action_on_item_use, action_on_land, action_on_wake_up,
action_over_time, action_when_damage_taken, action_when_hit, active_self,
attacker_action_when_hit, attribute, attribute_modify_transfer, burn, climbing,
conditioned_attribute, conditioned_restrict_armor, cooldown, creative_flight,
damage_over_time, disable_regen, effect_immunity, elytra_flight, entity_glow, entity_group,
exhaust, fire_immunity, fire_projectile, freeze, grounded, ignore_water, inventory,
invisibility, invulnerability, item_on_item, keep_inventory, lava_vision, model_color,
modify_air_speed, modify_attribute, modify_block_render, modify_break_speed,
modify_camera_submersion, modify_crafting, modify_damage_dealt, modify_damage_taken,
modify_exhaustion, modify_falling, modify_fluid_render, modify_food, modify_grindstone,
modify_harvest, modify_healing, modify_insomnia_ticks, modify_jump, modify_lava_speed,
modify_player_spawn, modify_projectile_damage, modify_slipperiness,
modify_status_effect_amplifier, modify_status_effect_duration, modify_swim_speed,
modify_velocity, modify_xp_gain, multiple, night_vision, overlay, particle, phasing,
prevent_being_used, prevent_block_selection, prevent_block_use, prevent_death,
prevent_elytra_flight, prevent_entity_collision, prevent_entity_render, prevent_entity_use,
prevent_feature_render, prevent_game_event, prevent_item_use, prevent_sleep,
prevent_sprinting, recipe, resource, restrict_armor, self_action_on_hit,
self_action_on_kill, self_action_when_hit, self_glow, shader, shaking, simple,
stacking_status_effect, starting_equipment, status_bar_texture, target_action_on_hit,
toggle, toggle_night_vision, tooltip, walk_on_fluid
```

### Water breathing — the correct way (resolved from source)

- `apoli:water_breathing` / `origins:water_breathing` is **NOT a power factory** (not in the 104). Emitting it as a `type` causes `Power type "...water_breathing" is not defined` and the power is skipped [LOG 406].
- Origins implements underwater breathing as **hardcoded behaviour keyed on the existence of the power with id `origins:water_breathing`**. `OriginsPowerTypes.WATER_BREATHING = new PowerTypeReference(Origins.identifier("water_breathing"))` [JAR-O], and the stock power file `data/origins/powers/water_breathing.json` is literally `{ "type": "origins:simple" }` [JAR-O] — a no-op marker; Origins' own mixins grant the breathing when a player has that power.
- **Generator rule:** to give an origin water breathing, **add the string `"origins:water_breathing"` to the origin's `powers` array** (reference the built-in power that Origins already ships). **Do not define your own water-breathing power.** Stock `merling.json` does exactly this: `"powers": ["origins:water_breathing", "origins:water_vision", ...]` [JAR-O].
- Other useful built-in `origins:*` powers safe to reference the same way (shipped in the Origins jar, no need to redefine): `origins:water_vision`, `origins:aqua_affinity`, `origins:swim_speed`, `origins:like_water`, `origins:slow_falling`, `origins:climbing`, `origins:fire_immunity`, `origins:water_protection`, `origins:fall_immunity`, `origins:scare_creepers`, `origins:phantomize`, `origins:elytra`, `origins:cat_vision` (full list = filenames in `data/origins/powers/` of the Origins jar).

---

## E. Sub-schemas referenced by the safe power types

### E.1 Item Stack (used by Origin `icon`)
Minimal: `{ "item": "minecraft:cod" }`. Optional: `"count"`(int), `"nbt"`(string/SNBT), `"tag"`(SNBT). Stock origins use only `{"item":"..."}` [JAR-O].

### E.2 Attribute modifier object (the `modifier` field of `apoli:attribute` etc.)

Two related shapes exist; **for `apoli:attribute`/`apoli:modify_attribute` use the *attributed* form** (includes the `attribute` key). Ground-truth shape from stock `data/origins/powers/swim_speed.json` and `nine_lives.json` [JAR-O]:

```json
{
  "attribute": "minecraft:generic.max_health",
  "operation": "addition",
  "value": 6.0,
  "name": "Tank max health"
}
```

| Field | Type | Req? | Notes |
|---|---|---|---|
| `attribute` | identifier | **required** | Vanilla/modded attribute id, e.g. `minecraft:generic.max_health`, `minecraft:generic.armor`, `minecraft:generic.armor_toughness`, `minecraft:generic.movement_speed`, `minecraft:generic.attack_damage`, `minecraft:generic.knockback_resistance`, `minecraft:generic.luck`. |
| `operation` | operation enum | **required** | See E.3. |
| `value` | float | **required** | Can be negative (stock `nine_lives` uses `-2.0`). |
| `name` | string | optional | Descriptive label; PLAIN STRING (matches the global string rule). |

`apoli:attribute` accepts either `"modifier": { ...one... }` or `"modifiers": [ {...}, {...} ]` (both keys exist on `AttributePower.DATA` [JAR-A]). Optional `"update_health": true` to immediately re-clamp current health when max_health changes.

**Working example — extra health power (`anvil:tank_max_health`), correct for this version:**
```json
{
  "type": "apoli:attribute",
  "name": "Sturdy",
  "description": "You have extra hearts.",
  "modifier": {
    "attribute": "minecraft:generic.max_health",
    "operation": "addition",
    "value": 6.0,
    "name": "Tank max health"
  }
}
```
**Armor + toughness example (multiple modifiers):**
```json
{
  "type": "apoli:attribute",
  "name": "Iron Skin",
  "modifiers": [
    { "attribute": "minecraft:generic.armor",           "operation": "addition", "value": 4.0, "name": "Tank armor" },
    { "attribute": "minecraft:generic.armor_toughness",  "operation": "addition", "value": 2.0, "name": "Tank toughness" }
  ]
}
```

### E.3 Attribute modifier operation enum (exact JSON values)

Two accepted vocabularies in Apoli 2.9.2:

- **Legacy (vanilla) — always safe, proven by stock files:** `addition`, `multiply_base`, `multiply_total`.
  (Stock `swim_speed.json` uses `multiply_base`; `nine_lives.json` uses `addition` [JAR-O].)
- **Apoli extended set** [DOC `/types/data_types/attribute_modifier_operation/`], matching the `ModifierOperations` enum constants in the jar [JAR-A] (`ADD_BASE_EARLY`, `MULTIPLY_BASE_ADDITIVE`, … lowercased): `add_base_early`, `multiply_base_additive`, `multiply_base_multiplicative`, `add_base_late`, `min_base`, `max_base`, `set_base`, `multiply_total_additive`, `multiply_total_multiplicative`, `min_total`, `max_total`, `set_total`. Applied in that priority order.

> **Generator recommendation:** emit only `addition` / `multiply_base` / `multiply_total` — proven valid in this exact build, no version risk.

### E.4 Entity condition object (the `condition` envelope field and `apoli:invulnerability.damage_condition`)
Shape: `{ "type": "<apoli/origins condition id>", ...fields..., "inverted": <bool> }`. Logical wrappers: `{ "type": "origins:and", "conditions": [ ... ] }`, `origins:or`. Common leaf conditions seen in stock files [JAR-O]: `origins:submerged_in` (`{"fluid":"minecraft:water"}`), `origins:fall_flying`, `origins:sneaking`, `origins:collided_horizontally`, `origins:power_active` (`{"power":"*:*_toggle"}`), `origins:block_collision`. Damage conditions (for `apoli:invulnerability`): `{ "type": "origins:fire" }` is how stock `fire_immunity.json` blocks fire damage [JAR-O]. Full condition-type lists: [DOC] `/types/entity_condition_types/`, `/types/damage_condition_types/`. Conditions are tier-C; a safe generator avoids them unless copying a stock pattern verbatim.

### E.5 Key object (active/toggle powers)
`{ "key": "key.origins.primary_active", "continuous": <bool> }` (also `key.origins.secondary_active`). [JAR-O stock files]

---

## F. TOP correctness pitfalls a generator MUST respect

1. **`name`/`description` are plain strings, NEVER component objects** — applies to Origin, Origin Layer, and Power JSON. An object value = the whole file is skipped at load. (Docs say "Text Component" — wrong for 1.10.2/2.9.2.) [LOG 405–417, JAR-A, JAR-O]
2. **`type` must be one of the 104 registered factory ids** (§D list), prefixed `apoli:` or `origins:` (alias-equivalent). Unknown type → power skipped (`Power type "X" is not defined`). Specifically **`apoli:water_breathing` does not exist** — never emit it. [LOG 406, JAR-A]
3. **Water breathing (and similar built-ins) = reference the existing `origins:water_breathing` power id in the origin's `powers` array; do not define a water-breathing power.** Same for `origins:climbing`, `origins:fire_immunity`, etc. — reuse Origins' shipped powers. [JAR-O]
4. **Slow fall = `apoli:modify_falling`** (`velocity`, `take_fall_damage`), not an attribute and not a nonexistent type. **Jump boost = `apoli:modify_jump`** (no vanilla jump attribute exists in 1.20.1). [JAR-O slow_falling.json]
5. **Attribute operation enum:** only `addition` / `multiply_base` / `multiply_total` (legacy, guaranteed) or the documented extended snake_case set. Never use Java enum constant names (`ADDITION`, `MULTIPLY_BASE`) or vanilla NBT ints (`0/1/2`). [JAR-A, DOC]
6. **Resource location format:** `namespace:path`, lowercase, `[a-z0-9_.-]` in path, `[a-z0-9_.-]` (no colon) in namespace; path may contain `/`. Origin/power file id = its path under `data/<ns>/origins|powers/...` (e.g. file `data/anvil/powers/tank_armor.json` → power id `anvil:tank_armor`). Layer that appends to vanilla **must** be `data/origins/origin_layers/origin.json`.
7. **Append, don't replace, the stock layer:** `{"replace": false, "origins": [...]}`; `replace:true` (or omitting and reusing the path with replace semantics) wipes the 10 vanilla origins. [JAR-O stock layer]
8. **`powers` entries must each resolve to a real power file**; a power that fails to load (e.g. bad `name`) makes any origin referencing it unusable, and a bad origin makes the layer entry dead — failures cascade. Validate every referenced id exists and parses. [LOG 405–417]
9. **`apoli:multiple`:** non-envelope keys are sub-powers and each needs its own valid `type`; child ids become `<parent>_<key>`. Don't put arbitrary scalar fields at the top level of a `multiple` power. [JAR-A, JAR-O climbing.json]
10. **`impact` 0–3, integers for `order`/`impact`/`loading_priority`, booleans not strings** for `unchoosable`/`hidden`/`enabled`/`replace`. Calio type-checks each field (`INT`, `BOOLEAN`, `STRING`, `IDENTIFIERS`) and skips the file on mismatch (`Error reading data field at <field>: <wrongType>`). [JAR-O, LOG 415–417]

---

### Appendix — provenance of the 104-id set
`PowerFactories.register()` directly registers 17 (incl. `simple`, `creative_flight`, `fire_immunity`, `freeze`, `grounded`, `ignore_water`, `swimming`, `disable_regen`, `prevent_sprinting`, `shaking`, the `modify_air_speed/exhaustion/healing/xp_gain/insomnia_ticks`, `action_when_damage_taken`, `self_action_when_hit`). The remaining ~87 are each registered by their own `XxxPower.createFactory()` via `new PowerFactory<>(Apoli.identifier("<id>"), …)` (pattern verified in `AttributePower`, `NightVisionPower`, etc.). Union, deduped = 104, plus `multiple`/`simple` confirmed in `PowerTypes` static init. This is the exact valid type set for Apoli 2.9.2; treat it as the generator's whitelist. [JAR-A]
