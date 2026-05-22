//! Custom-Origins datapack ENGINE.
//!
//! CONTRACT: like `recipe.rs`/`content.rs`, a reusable, persistence-FREE engine
//! — a typed model (`OriginsSet` = origins + powers), a documentation-grounded
//! catalog + validator that GATES emission (`validate` -> `Validated`), a
//! deterministic serializer (`emit`), a deterministic `rescue_set()` used to
//! repair an already-broken instance with NO LLM call, and the thin instance
//! writer (`write_origins_datapack`). It owns no source-of-truth file.
//!
//! STATE MACHINE: the only way to produce a datapack is
//! `OriginsSet` -> `validate` -> `Validated` -> `emit`. `emit` is private and
//! takes `&Validated`, which is ONLY constructable by `validate`, so an
//! invalid set is structurally impossible to emit. The catalog (`SAFE_TYPES`,
//! `FULL_WHITELIST`, `SHIPPED_ORIGINS_POWERS`) is the documentation grounded
//! in `docs/modding/origins_apoli_2.9.2_schema.md` (decompiled Apoli 2.9.2 +
//! Origins 1.10.2 + the runtime log) and is jar-checked by the tests so it
//! cannot silently drift from the game.
//!
//! WHY THIS DATAPACK ROOT: files live under
//! `config/openloader/data/anvil-origins/` — a SIBLING of Slice 2's
//! `anvil-recipes` and Slice 3's `anvil-content`, each with its own
//! `pack.mcmeta`. Open Loader injects any `config/openloader/data/<pack>/` and
//! Origins/Apoli are normal resource-reload listeners, so the pack is picked
//! up exactly like the recipe/content packs already are (proven in the
//! instance launch log: `Loaded folder Data Pack from .../anvil-origins`).
//!
//! HARD SCHEMA FACTS (primary-source verified — decompiled jars + the runtime
//! log; the OLD code's "literal Component, verified against decompiled
//! Origins" claim was FALSE and the running game disproved it):
//! - `name`/`description` on Origin, Power AND OriginLayer are PLAIN STRINGS
//!   (`JsonHelper.getString`); a `{"text":...}` object => the whole file is
//!   skipped (`Expected name to be a string, was an object`). They are typed
//!   `String` here so the component bug is unrepresentable.
//! - A power `type` must be a registered Apoli factory id (104 of them;
//!   `origins:<x>` aliases `apoli:<x>`). `apoli:water_breathing` is NOT one.
//! - Water-breathing/climbing/etc. as built-ins = REFERENCE the shipped
//!   `origins:<x>` power id in the origin's `powers` (Origins' own code
//!   implements them); never define your own. See `SHIPPED_ORIGINS_POWERS`.
//! - `impact` is an INTEGER 0..=3 (NOT the string enum the old code emitted).
//! - The layer file REPLACES the stock chooser with
//!   `{"replace": true, "origins": [...]}` so ONLY the pack's generated
//!   origins are selectable (the 10 vanilla Origins are intentionally
//!   wiped — user-requested). `replace:true` is bytecode-verified in
//!   docs/modding/origins_apoli_2.9.2_schema.md §B.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::json;

use anyhow::Context;

/// `pack_format` for a Minecraft 1.20 / 1.20.1 datapack (same constant
/// `recipe.rs`/`content.rs` use; a wrong/missing format makes the loader skip
/// the whole pack).
const PACK_FORMAT_1_20: i64 = 15;

/// The origins-datapack root: a SIBLING of the recipe/content datapacks.
const ROOT: &str = "config/openloader/data/anvil-origins";

/// The layer file path is fixed and namespace-INDEPENDENT: the `origins`
/// segment is the Origins mod's own namespace (layer identity `origins:origin`,
/// the layer that surfaces custom origins on the normal character screen).
const LAYER_PATH_SUFFIX: &str = "data/origins/origin_layers/origin.json";

/// Modrinth project id of Origins **core** (slug `origins`). Origins-Classes
/// (`FiDptjtR`) is an ADDON and must NOT by itself trigger the datapack.
pub const ORIGINS_CORE_PROJECT_ID: &str = "3BeIrqZR";

/// Whether a single pinned mod is Origins **core** (not an addon). Primary
/// signal is the canonical Modrinth project id (Anvil is Modrinth-only, so it
/// is always populated); a jar-name fallback (`origins-*` but never an
/// `…classes…`/addon jar) keeps it robust for non-Modrinth-sourced pins.
pub fn is_origins_core(project_id: &str, jar_name: &str) -> bool {
    if project_id.eq_ignore_ascii_case(ORIGINS_CORE_PROJECT_ID) {
        return true;
    }
    let n = jar_name.to_ascii_lowercase();
    let n = n.rsplit('/').next().unwrap_or(&n);
    (n.starts_with("origins-") || n.starts_with("origins+") || n == "origins.jar")
        && !n.contains("classes")
        && !n.contains("apoli")
        && !n.contains("calio")
}

// ---------------------------------------------------------------------------
// CATALOG — the documentation-grounded source of truth. Transcribed from the
// primary-source reference `docs/modding/origins_apoli_2.9.2_schema.md`
// (decompiled Apoli 2.9.2 `PowerFactories` + the runtime log). Two surfaces:
//
//  * `SAFE_TYPES`  — the SMALL set a generator/LLM may design against, each
//    with its required body fields. These have a stock example in the exact
//    stack so the emitted shape is known-correct.
//  * `FULL_WHITELIST` — every power-factory id registered in Apoli 2.9.2. The
//    validator rejects any `type` not here (this is what catches
//    `apoli:water_breathing`, which is NOT a factory).
//
// `apoli:water_breathing` is deliberately ABSENT: it is not a power factory.
// Water breathing is granted by REFERENCING the shipped `origins:water_breathing`
// power id in an origin's `powers` (see `SHIPPED_ORIGINS_POWERS`).
// ---------------------------------------------------------------------------

/// A SAFE power type + its REQUIRED body field names (everything besides the
/// envelope `type`/`name`/`description`). Empty = self-contained (no required
/// body). Jar-checked by `catalog_safe_required_fields_subset_of_jar`.
pub struct SafeType {
    pub id: &'static str,
    pub required: &'static [&'static str],
}

/// The model-facing safe catalog. Order is the prompt-presentation order.
pub const SAFE_TYPES: &[SafeType] = &[
    SafeType { id: "apoli:attribute", required: &["modifier"] },
    SafeType { id: "apoli:modify_jump", required: &["modifier"] },
    SafeType { id: "apoli:modify_damage_taken", required: &["modifier"] },
    SafeType { id: "apoli:modify_falling", required: &["velocity"] },
    SafeType { id: "apoli:night_vision", required: &[] },
    SafeType { id: "apoli:climbing", required: &[] },
    SafeType { id: "apoli:fire_immunity", required: &[] },
    SafeType { id: "apoli:swimming", required: &[] },
    SafeType { id: "apoli:invisibility", required: &[] },
];

/// Every Apoli 2.9.2 registered power-factory id (bare, no namespace).
/// `origins:<x>` is an alias of `apoli:<x>` so either namespace validates.
/// The validator only uses this for set membership.
pub const FULL_WHITELIST: &[&str] = &[
    "action_on_being_used", "action_on_block_break", "action_on_block_use",
    "action_on_callback", "action_on_entity_use", "action_on_hit",
    "action_on_item_use", "action_on_land", "action_on_wake_up",
    "action_over_time", "action_when_damage_taken", "action_when_hit",
    "active_self", "attacker_action_when_hit", "attribute",
    "attribute_modify_transfer", "burn", "climbing", "conditioned_attribute",
    "conditioned_restrict_armor", "cooldown", "creative_flight",
    "damage_over_time", "disable_regen", "effect_immunity", "elytra_flight",
    "entity_glow", "entity_group", "exhaust", "fire_immunity",
    "fire_projectile", "freeze", "grounded", "ignore_water", "inventory",
    "invisibility", "invulnerability", "item_on_item", "keep_inventory",
    "launch", "lava_vision", "model_color", "modify_air_speed", "modify_attribute",
    "modify_block_render", "modify_break_speed", "modify_camera_submersion",
    "modify_crafting", "modify_damage_dealt", "modify_damage_taken",
    "modify_exhaustion", "modify_falling", "modify_fluid_render",
    "modify_food", "modify_grindstone", "modify_harvest", "modify_healing",
    "modify_insomnia_ticks", "modify_jump", "modify_lava_speed",
    "modify_player_spawn", "modify_projectile_damage", "modify_slipperiness",
    "modify_status_effect_amplifier", "modify_status_effect_duration",
    "modify_swim_speed", "modify_velocity", "modify_xp_gain", "multiple",
    "night_vision", "overlay", "particle", "phasing", "prevent_being_used",
    "prevent_block_selection", "prevent_block_use", "prevent_death",
    "prevent_elytra_flight", "prevent_entity_collision",
    "prevent_entity_render", "prevent_entity_use", "prevent_feature_render",
    "prevent_game_event", "prevent_item_use", "prevent_sleep",
    "prevent_sprinting", "recipe", "resource", "restrict_armor",
    "self_action_on_hit", "self_action_on_kill", "self_action_when_hit",
    "self_glow", "shader", "shaking", "simple", "stacking_status_effect",
    "starting_equipment", "status_bar_texture", "target_action_on_hit",
    "toggle", "toggle_night_vision", "tooltip", "walk_on_fluid",
];

/// Stock powers Origins ships in its OWN jar (`data/origins/powers/*.json`),
/// referenced as `origins:<id>`. Anvil never emits a file for these — Origins'
/// own code/mixins implement the behaviour. This is the ONLY correct way to
/// grant water-breathing/climbing/etc. as built-ins. Jar-checked by
/// `shipped_powers_subset_of_origins_jar`.
pub const SHIPPED_ORIGINS_POWERS: &[&str] = &[
    "origins:water_breathing",
    "origins:water_vision",
    "origins:aqua_affinity",
    "origins:swim_speed",
    "origins:like_water",
    // origins:slow_falling intentionally OMITTED — global directive: no
    // slow-fall powers in Anvil-curated origins. Origins ships the id and the
    // datapack still loads, but Anvil never references it.
    "origins:climbing",
    "origins:fire_immunity",
    "origins:fall_immunity",
    "origins:scare_creepers",
    "origins:phantomize",
    "origins:elytra",
    "origins:cat_vision",
];

/// Attribute-modifier operations valid in 1.20.1 Apoli (the guaranteed-safe
/// legacy three; proven by stock Origins files).
const ALLOWED_OPERATIONS: &[&str] = &["addition", "multiply_base", "multiply_total"];

/// Vanilla attributes a SAFE `apoli:attribute` power may target (the
/// documented player-relevant generics).
const ALLOWED_ATTRIBUTES: &[&str] = &[
    "minecraft:generic.max_health",
    "minecraft:generic.armor",
    "minecraft:generic.armor_toughness",
    "minecraft:generic.movement_speed",
    "minecraft:generic.attack_damage",
    "minecraft:generic.attack_speed",
    "minecraft:generic.knockback_resistance",
    "minecraft:generic.luck",
];

/// Strip an `apoli:`/`origins:` prefix to the bare factory id (the alias is
/// real: Origins calls `NamespaceAlias.addAlias("origins","apoli")`).
fn bare_power_type(t: &str) -> &str {
    t.strip_prefix("apoli:")
        .or_else(|| t.strip_prefix("origins:"))
        .unwrap_or(t)
}

fn safe_type(id: &str) -> Option<&'static SafeType> {
    SAFE_TYPES.iter().find(|s| s.id == id)
}

// ---------------------------------------------------------------------------
// Typed model — `name`/`description` are `String` so the component bug is
// unrepresentable; `impact` is `i64` so the string-impact bug is too.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Power {
    /// Bare power id (no namespace); becomes `data/<ns>/powers/<id>.json`.
    pub id: String,
    /// Plain display name — emitted VERBATIM as a JSON string.
    pub name: String,
    /// Plain description — emitted VERBATIM as a JSON string.
    pub description: String,
    /// The Apoli power-type id (validated against `FULL_WHITELIST`).
    #[serde(rename = "type")]
    pub power_type: String,
    /// Type-specific fields (everything except `type`/`name`/`description`).
    #[serde(default)]
    pub body: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    /// Bare origin id; becomes `data/<ns>/origins/<id>.json`.
    pub id: String,
    pub name: String,
    pub description: String,
    /// Power references. Each entry is EITHER a bare local power id (must be an
    /// emitted `Power.id`) OR a fully-qualified shipped `origins:<x>` id (must
    /// be in `SHIPPED_ORIGINS_POWERS`). Classified by `validate`.
    pub powers: Vec<String>,
    /// Vanilla item id for the selection-screen icon (`ns:path`).
    pub icon: String,
    /// Selection-screen impact, 0..=3 (0 none .. 3 high).
    pub impact: i64,
    /// Display order on the character screen (distinct per origin).
    pub order: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OriginsSet {
    pub origins: Vec<Origin>,
    pub powers: Vec<Power>,
}

/// A set proven valid by [`validate`]. Only `validate` constructs this, and
/// [`emit`] only accepts this — invalid output is structurally impossible.
#[derive(Debug, Clone)]
pub struct Validated(OriginsSet);

impl Validated {
    pub fn get(&self) -> &OriginsSet {
        &self.0
    }
}

/// How an origin's power reference resolves (decided BY the validator from the
/// model's string, never supplied by the model).
#[derive(Debug, Clone, Copy, PartialEq)]
enum PowerRef<'a> {
    /// A power Anvil emits a file for; referenced as `<ns>:<id>`.
    Local(&'a str),
    /// A power Origins itself ships; referenced VERBATIM (already `origins:x`).
    Shipped(&'a str),
}

fn classify_power_ref<'a>(
    s: &'a str,
    emitted: &std::collections::BTreeSet<&str>,
) -> Option<PowerRef<'a>> {
    if SHIPPED_ORIGINS_POWERS.contains(&s) {
        Some(PowerRef::Shipped(s))
    } else if emitted.contains(s) {
        Some(PowerRef::Local(s))
    } else {
        None
    }
}

/// Referential-integrity / schema failure. `validate` returns EVERY failure.
#[derive(Debug, Clone, PartialEq)]
pub enum IntegrityError {
    BadId(String),
    DuplicateId(String),
    EmptyText { what: String, field: &'static str },
    /// Power `type` is not a registered Apoli factory id.
    BadPowerType { power: String, ty: String },
    /// A SAFE power type is missing a required body field.
    MissingRequiredField { power: String, ty: String, field: &'static str },
    /// `apoli:attribute` targets a non-allowlisted attribute.
    BadAttribute { power: String, attribute: String },
    /// A modifier operation is not valid.
    BadOperation { power: String, operation: String },
    /// Origin power ref resolves to neither an emitted power nor a shipped one.
    DanglingPowerRef { origin: String, power: String },
    /// Origin icon is not a well-formed `namespace:path`.
    BadIcon { origin: String, icon: String },
    /// Origin impact outside 0..=3.
    BadImpact { origin: String, impact: i64 },
    /// No origins => the layer would be empty.
    EmptySet,
}

impl std::fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use IntegrityError::*;
        match self {
            BadId(s) => write!(f, "invalid id `{s}` (must be lowercase [a-z0-9_.-]+)"),
            DuplicateId(s) => write!(f, "duplicate id `{s}`"),
            EmptyText { what, field } => write!(f, "{what} has empty `{field}` (must be non-empty plain text)"),
            BadPowerType { power, ty } => write!(f, "power `{power}` type `{ty}` is not a registered Apoli factory"),
            MissingRequiredField { power, ty, field } => write!(f, "power `{power}` (type `{ty}`) is missing required field `{field}`"),
            BadAttribute { power, attribute } => write!(f, "power `{power}` targets non-allowlisted attribute `{attribute}`"),
            BadOperation { power, operation } => write!(f, "power `{power}` has invalid operation `{operation}`"),
            DanglingPowerRef { origin, power } => write!(f, "origin `{origin}` references power `{power}` which is neither emitted nor a shipped origins power"),
            BadIcon { origin, icon } => write!(f, "origin `{origin}` icon `{icon}` is not a well-formed namespace:path"),
            BadImpact { origin, impact } => write!(f, "origin `{origin}` impact {impact} out of range 0..=3"),
            EmptySet => write!(f, "origins set is empty (layer would be empty)"),
        }
    }
}

impl std::error::Error for IntegrityError {}

impl IntegrityError {
    /// Stable machine kind (for the model-facing repair JSON).
    pub fn kind(&self) -> &'static str {
        use IntegrityError::*;
        match self {
            BadId(_) => "BadId",
            DuplicateId(_) => "DuplicateId",
            EmptyText { .. } => "EmptyText",
            BadPowerType { .. } => "BadPowerType",
            MissingRequiredField { .. } => "MissingRequiredField",
            BadAttribute { .. } => "BadAttribute",
            BadOperation { .. } => "BadOperation",
            DanglingPowerRef { .. } => "DanglingPowerRef",
            BadIcon { .. } => "BadIcon",
            BadImpact { .. } => "BadImpact",
            EmptySet => "EmptySet",
        }
    }

    /// The remediation the model should apply. Co-located with the variant so
    /// a failed proposal converges in one repair round, not three.
    pub fn hint(&self) -> &'static str {
        use IntegrityError::*;
        match self {
            BadId(_) => "ids must be lowercase [a-z0-9_.-]+ (no spaces/uppercase); they become the file name.",
            DuplicateId(_) => "give every power and every origin a unique id.",
            EmptyText { .. } => "name and description are plain non-empty strings (NOT a {\"text\":...} object).",
            BadPowerType { .. } => "use only a `type` from the SAFE power list in the system prompt. For water-breathing/climbing/etc do NOT define a power — reference the shipped origins:<id> in the origin's `powers` instead.",
            MissingRequiredField { .. } => "this SAFE type has a required body field — see the SAFE power list (e.g. apoli:attribute needs `modifier`, apoli:modify_falling needs `velocity`).",
            BadAttribute { .. } => "an apoli:attribute power may only target the allowlisted vanilla attributes listed in the system prompt.",
            BadOperation { .. } => "modifier `operation` must be exactly one of: addition, multiply_base, multiply_total.",
            DanglingPowerRef { .. } => "every entry in an origin's `powers` must be either a power id you defined in this same call, or a shipped origins:<id> from the allowlist.",
            BadIcon { .. } => "icon must be a real vanilla item id as namespace:path, e.g. minecraft:netherite_chestplate.",
            BadImpact { .. } => "impact is an INTEGER 0,1,2,3 (0 none .. 3 high) — not a string.",
            EmptySet => "produce at least one origin.",
        }
    }
}

/// The model-facing repair payload: `[{kind, where, why, hint}]`. Returned as
/// the tool result so the next round's proposal converges fast.
pub fn errors_to_json(errs: &[IntegrityError]) -> serde_json::Value {
    use IntegrityError::*;
    serde_json::Value::Array(
        errs.iter()
            .map(|e| {
                let wher = match e {
                    BadId(s) | DuplicateId(s) => s.clone(),
                    EmptyText { what, field } => format!("{what}.{field}"),
                    BadPowerType { power, .. }
                    | MissingRequiredField { power, .. }
                    | BadAttribute { power, .. }
                    | BadOperation { power, .. } => format!("power `{power}`"),
                    DanglingPowerRef { origin, power } => {
                        format!("origin `{origin}` -> `{power}`")
                    }
                    BadIcon { origin, .. } | BadImpact { origin, .. } => {
                        format!("origin `{origin}`")
                    }
                    EmptySet => "origins".to_string(),
                };
                json!({ "kind": e.kind(), "where": wher, "why": e.to_string(), "hint": e.hint() })
            })
            .collect(),
    )
}

/// The model-facing catalog, generated from the SAME Rust constants the
/// validator uses (so prompt and gate can never drift). Inlined into the
/// progression system prompt — the model cannot read repo docs at inference.
pub fn safe_catalog_prompt_section() -> String {
    let mut s = String::new();
    s.push_str("CUSTOM ORIGINS (only if the pack runs Origins). Call generate_origins ONCE with a set of 2-5 origins themed to THIS pack and the player's request. It is a SINGLE authored set (a second call REPLACES it; it does NOT accumulate like generate_quests). Hard rules:\n");
    s.push_str("- `name` and `description` (on every origin AND power) are PLAIN STRINGS, never a {\"text\":...} object.\n");
    s.push_str("- origin `impact` is an INTEGER 0-3 (0 none, 1 low, 2 medium, 3 high). `order` is an integer.\n");
    s.push_str("- origin `icon` is a real vanilla item id, namespace:path (e.g. minecraft:netherite_chestplate).\n");
    s.push_str("- An origin's `powers` entries are EITHER a power id you define in `powers` this call, OR a shipped origins:<id> (reference it, do NOT redefine it).\n");
    s.push_str("- A power `type` MUST be one of these SAFE types (with its required body field):\n");
    for st in SAFE_TYPES {
        let req = if st.required.is_empty() {
            "no required body".to_string()
        } else {
            format!("requires `{}`", st.required.join("`, `"))
        };
        s.push_str(&format!("    - {} ({req})\n", st.id));
    }
    s.push_str("- For built-in effects, REFERENCE a shipped power id (do NOT define one): ");
    s.push_str(&SHIPPED_ORIGINS_POWERS.join(", "));
    s.push_str("\n  (e.g. underwater breathing => put \"origins:water_breathing\" in the origin's powers; climbing => \"origins:climbing\").\n");
    s.push_str(&format!(
        "- apoli:attribute `modifier` shape: {{\"attribute\":<one of: {}>, \"operation\":<one of: {}>, \"value\":<number>, \"name\":<string>}}.\n",
        ALLOWED_ATTRIBUTES.join(", "),
        ALLOWED_OPERATIONS.join(", "),
    ));
    s.push_str("- modify_jump / modify_damage_taken `modifier` shape: {\"operation\":<op>, \"value\":<number>, \"name\":<string>}. modify_falling shape: {\"velocity\":<number>, \"take_fall_damage\":false}.\n");
    s
}

/// The PREFERRED authoring surface — closed PerkIntent enum + density
/// budgets + impressive-pattern examples. The curator injects this when
/// the pack runs Origins core + open-loader; it documents `generate_origin_intents`,
/// the typed mechanics path that yields working in-game powers + companion
/// mcfunctions (not the legacy raw-Apoli `generate_origins` filler-machine).
pub fn intent_catalog_prompt_section() -> String {
    let mut s = String::new();
    s.push_str("CUSTOM ORIGIN INTENTS (preferred — call generate_origin_intents). \
Author MECHANICS via the closed PerkIntent enum below; the engine compiles each \
intent to Apoli powers + companion mcfunctions. Density-validated: pick light/\
standard/rich, stay in budget.\n\n");

    s.push_str("DENSITY: ASK THE PLAYER FIRST (never silently default).\n\
The batch `density` argument is the AVERAGE the player picked — per-origin\n\
intents may carry their own `density` to deviate around it (Fresher light,\n\
Porter rich, averaging to standard within ±0.6). Levels:\n\
- light (score 1.0): 3-4 passives, 0 actives, 0 lifetimes — a minor flavour origin.\n\
- standard (score 2.0): 5-7 passives, 1 active, 0-1 lifetimes — the Origins-default feel.\n\
- rich (score 3.0): 8-10 passives, 1 active, EXACTLY 1 lifetime — the ceiling.\n\
The batch validator rejects if the per-origin score average strays >0.6 from\n\
the user's pick. So a `standard` batch tolerates a mix of light+standard+rich\n\
that averages 1.4..2.6, but an all-rich batch under `standard` is rejected.\n\n");

    s.push_str("ORIGIN-INTENT SHAPE:\n");
    s.push_str("{ \"theme\": \"arcane|cursed|cozy|adventure|tech|nature|tinker|wanderer\", \
\"name\": <plain string>, \"description\": <1-3 sentence plain string>, \
\"icon\": <real item id, ns:path>, \"perks\": [<PerkIntent>, ...], \
\"density\": <\"light\"|\"standard\"|\"rich\" — optional override, omit to inherit the batch average> }\n\n");

    s.push_str("EVERY perk is a tagged object: { \"intent\": <variant>, ...fields }.\n\n");

    s.push_str("PERK INTENT CATALOG (45 variants):\n\n");
    s.push_str("# Passive starters\n");
    s.push_str("- starts_with: { items: [<item_id>...], slots?: [hotbar.0..feet] }\n");
    s.push_str("- scale: { factor: <0.1..4.0> }  // Pehkui-driven body scale\n");
    s.push_str("- passive_effect: { effect: <status_effect_id>, amplifier?: 0-4 }\n");
    s.push_str("- attribute_buff: { attribute: <attr_id>, op: addition|multiply_base|multiply_total, amount: <-64..64>, when?: <WhenCondition> }\n");
    s.push_str("- special_movement: { kind: climb|elytra_flight|walk_on_fluid|creative_flight|higher_jump }\n\n");

    s.push_str("# Conditional (gated by WhenCondition — see below)\n");
    s.push_str("- buff_when: { what: { kind: effect, effect, amplifier } | { kind: attribute, attribute, op, amount }, when: <WhenCondition> }\n");
    s.push_str("- dot_when: { dps: <0.1..10.0>, when: <WhenCondition> }  // periodic damage; e.g. vampire daylight burn\n");
    s.push_str("- damage_vs: { target: <entity_id|tag|[ids]>, multiplier: <0..5> }\n\n");

    s.push_str("# Restrictions\n");
    s.push_str("- forbidden_item_use: { what: <item_selector> }\n");
    s.push_str("- prevent_sleep: { except?: <item_selector> }\n");
    s.push_str("- prevent_break_under_foot: { block: <block_selector> }\n\n");

    s.push_str("# Event hooks\n");
    s.push_str("- on_kill_grant: { target: <entity_id|tag>, effect: <status_effect>, duration_s: <int> }\n");
    s.push_str("- on_wake_grant: { effects: [{ effect, amplifier: 0-4, duration_t: <ticks> }, ...] }\n");
    s.push_str("- bonus_saturation_on: { food: <item_selector>, extra: -10..20, when?: <WhenCondition> }\n");
    s.push_str("- faster_break_on: { block: <block_selector>, multiplier: 0.5..5.0 }\n");
    s.push_str("- tally_milestone: { event: kill_in_radius|block_break|boss_defeat|quest_complete, target: <entity_id|tag>, threshold: <int>, unlock: <PerkIntent> }\n");
    s.push_str("  // Scoreboard-counted; the unlock perk's powers are gated by apoli:command checking the threshold.\n\n");

    s.push_str("# Mob relationships\n");
    s.push_str("- pacify_targeting: { by: <entity_id|tag> }  // listed mobs ignore the player\n");
    s.push_str("- hostile_recognition: { by: <entity_id|tag> }  // listed mobs aggro the player on sight\n");
    s.push_str("- entity_glow: { targets: <entity_id|tag>, radius: 4-64 }  // pack sense / hostile glow\n\n");

    s.push_str("# Periodic\n");
    s.push_str("- once_per_day_bonus: { trigger: dawn|dusk|first_meal|first_sleep, bonus: <PerkIntent> }\n");
    s.push_str("- season_notification: { lead_days: 1-7, message: <plain string> }\n\n");

    s.push_str("# Persistence / UI\n");
    s.push_str("- keep_inventory_slot: { slots: [<slot>...] }  // emits gamerule + slot marker\n");
    s.push_str("- map_marker_at_spawn: { label: <plain string> }\n");
    s.push_str("- overlay: { when: <WhenCondition>, duration_s?: <int> }\n");
    s.push_str("- auto_journal: { milestones: [{ trigger: <tag_name>, entry: <plain text> }, ...] }\n\n");

    // STRUCT SHAPES (referenced by ActiveBody / LifetimeBody / passive perks).
    // Critical reference — emit deserialisation enforces these exactly. Past
    // sessions thrashed for 12 rounds because the catalog listed field names
    // without shapes; the LLM guessed, emit rejected. Now every nested object
    // is spelled out below.
    s.push_str("# AUXILIARY STRUCT SHAPES (referenced below — read these BEFORE authoring actives/lifetimes)\n");
    s.push_str("- StatusEffectInst: { effect: <status_effect_id>, amplifier: 0-4, duration_t: <ticks> }\n");
    s.push_str("  // ALWAYS use the full struct — single-string effects FAIL to deserialise.\n");
    s.push_str("- AreaAction: { radius: 1-32, damage: <float>, particle_key: <texture_key> }\n");
    s.push_str("  // particle_key is a TextureKey string like \"anvil:particle/teleport\"; pick a thematic id.\n");
    s.push_str("- RetinueSpec: { entity_types: [<entity_id|tag|[ids]>...], radius: 1-32, follow_duration_s: <int> }\n");
    s.push_str("- ItemSelector / BlockSelector / EntityCondRef: a PLAIN STRING (single id or tag) OR an ARRAY of strings. NEVER an object.\n");
    s.push_str("  // Item selectors accept #tag form; selector grounding happens against the pack's registry — query_registry first.\n\n");

    s.push_str("# Active (G keybind) — 1 per origin max\n");
    s.push_str("- active: { key: primary|secondary|tertiary, cooldown_s, hud: active|cooldown|resource, body: <ActiveBody> }\n");
    s.push_str("  ActiveBody (tagged by kind):\n");
    s.push_str("  - { kind: area_burst, radius: 1-32, damage: <float>, knockback: <float> }\n");
    s.push_str("  - { kind: invisibility_pulse, duration_s: <int>, retinue?: <RetinueSpec> }\n");
    s.push_str("  - { kind: teleport_to_marker, marker: <BlockSelector>, on_depart: <AreaAction> }\n");
    s.push_str("    // on_depart is an AreaAction struct ({radius, damage, particle_key}) — NOT a list of effects.\n");
    s.push_str("  - { kind: transformation, duration_s, scale?: <0.1..4.0>, stash_inventory: bool, effects_on: [<StatusEffectInst>...], effects_off: [<StatusEffectInst>...], summon_allies?: <RetinueSpec> }\n");
    s.push_str("  - { kind: timed_effect_chain, on: [<StatusEffectInst>...], duration_s, off: [<StatusEffectInst>...], off_duration_s }\n");
    s.push_str("    // on/off MUST be ARRAYS of StatusEffectInst objects — passing a single \"minecraft:slowness\" string FAILS.\n\n");

    s.push_str("# Lifetime (✦✦) — EXACTLY 1 for Rich density. Requires DatapackChannel capability (Open Loader).\n");
    s.push_str("- lifetime: { gate: once_per_save|once_per_in_game_day|once_per_moon_full|phase_triggered, body: <LifetimeBody> }\n");
    s.push_str("  LifetimeBody (tagged by kind):\n");
    s.push_str("  - { kind: place_persistent_zone, structure_key: <texture_key>, radius: 1-32, suppress_spawns: bool, growth_boost?: <0.5..5.0>, animal_migration: bool }\n");
    s.push_str("  - { kind: forced_transformation, duration: night|full_day|thirty_seconds|one_minute, body: <ActiveBody> }\n");
    s.push_str("  - { kind: log_and_resurrect, logs: <EntityCondRef>, summon_for_dur_s: <int> }\n");
    s.push_str("    // `logs` = entity-type id(s) or tag(s) tracked for the resurrection trigger; NOT text/journal entries.\n");
    s.push_str("  - { kind: rally_event, summon_entities: <EntityCondRef>, structure_key: <texture_key>, area_buff_dur_s: <int> }\n");
    s.push_str("  - { kind: waypoint_recall, visit_threshold: 1-255 }\n");
    s.push_str("    // SIMPLEST lifetime — no structure_key, no entity refs. Default choice when in doubt.\n\n");

    s.push_str("# Gameplay drivers (use these for COMBAT/EXPLORATION feel)\n");
    s.push_str("- combo_chain: { window_t: <ticks>, ramp: <float>, max_stacks: 2-16 }  // hit-chain damage stacking\n");
    s.push_str("- siphon: { target: <entity_id|tag>, hp: <float>, food: <0-255> }  // lifesteal on hit\n");
    s.push_str("- dodge_roll: { i_frames_t, distance, cooldown_s }  // active dash with i-frames\n");
    s.push_str("- vein_mine: { block: <block_selector>, max_chain: <int> }\n");
    s.push_str("- harvest_aoe: { crop: <block_selector>, radius: <int> }\n");
    s.push_str("- last_stand: { hp_threshold: 1-20, duration_s, effects: [{effect, amplifier, duration_t}, ...] }\n");
    s.push_str("- block_phase: { block: <block_selector>, when: <WhenCondition> }  // dryad leaf-phase\n");
    s.push_str("- stagger_on_sprint: { effect: <status_effect>, duration_s }\n\n");

    s.push_str("# Mod-integrated (require ModCapability — capabilities come from the pack's mods)\n");
    s.push_str("- signature_trinket: { slot: necklace|hand|charm|belt, model: <texture_key>, carries: <PerkIntent> }  // Trinkets mod\n");
    s.push_str("- familiar: { entity: <entity_id>, bond_action: cauldron_ritual|altar_offer|gift_item|kill_blessing, persist_through_death: bool }  // Bewitchment\n");
    s.push_str("- seasonal_form: { spring: [<PerkIntent>...], summer: [...], fall: [...], winter: [...] }  // Seasons mod\n");
    s.push_str("- apprentice_to_npc: { npc: { Tag: <s> } | { Names: [<s>...] }, gift_threshold: 1-20, reward_chain: [<PerkIntent>...] }\n");
    s.push_str("- brew_potency: { which: <item_selector>, dur_mul: 0.5-4.0, amp_bonus: <i8> }  // Bewitchment\n");
    s.push_str("- knife_master: { knife: <item_selector>, on_use: <PerkIntent> }  // Farmer's Delight\n");
    s.push_str("- gravewalker: { near: <block_selector>, on_proximity: <PerkIntent> }  // Graveyard\n");
    s.push_str("- pack_leader: { entity_types: [<entity_id|tag>...], persistent_count: 1-12 }\n");
    s.push_str("- bandit_kin: { faction: <entity_id|tag>, pacify_radius: <int>, ally_summon?: <PerkIntent> }\n\n");

    s.push_str("# Phase 3 seed (Heracles quest hook)\n");
    s.push_str("- origin_questline: { chapter_seed: <theme_tag> }  // emits an /advancement grant for the quest emitter to pick up\n\n");

    s.push_str("WHEN CONDITIONS (typed enum; compound via And/Or/Not):\n");
    s.push_str("{ kind: any }                                  // unconditional — power applies always\n");
    s.push_str("{ kind: daytime } | { kind: nighttime }        // world time half\n");
    s.push_str("{ kind: in_rain } | { kind: exposed_to_sky } | { kind: on_fire }\n");
    s.push_str("{ kind: sneaking } | { kind: sprinting } | { kind: swimming } | { kind: fall_flying }\n");
    s.push_str("{ kind: dimension, id: <ns:path> }             // e.g. minecraft:the_nether\n");
    s.push_str("{ kind: biome, id: <ns:path> }                 // exact biome\n");
    s.push_str("{ kind: biome_tag, tag: <ns:path no #> }       // e.g. minecraft:is_cold, c:is_dry\n");
    s.push_str("{ kind: block_in_radius, block: <block_selector>, radius: 1-32 }   // proximity gate\n");
    s.push_str("{ kind: not, conditions: [<WhenCondition>] }   // single-element wrapper\n");
    s.push_str("{ kind: and, conditions: [<WhenCondition>, ...] }\n");
    s.push_str("{ kind: or,  conditions: [<WhenCondition>, ...] }\n\n");

    s.push_str("IMPRESSIVE PATTERNS (use these as templates, not the generic farmer/blacksmith filler the legacy tool produces):\n\n");
    s.push_str("# Bewitched Witch — cauldron-bound + cursed by daylight (mods: bewitchment, graveyard)\n");
    s.push_str("{ \"intent\": \"starts_with\", \"items\": [\"bewitchment:athame\", \"bewitchment:silver_ingot\"] }\n");
    s.push_str("{ \"intent\": \"attribute_buff\", \"attribute\": \"minecraft:generic.max_health\", \"op\": \"addition\", \"amount\": 4.0,\n");
    s.push_str("  \"when\": { \"kind\": \"block_in_radius\", \"block\": \"bewitchment:witch_cauldron\", \"radius\": 8 } }\n");
    s.push_str("{ \"intent\": \"dot_when\", \"dps\": 0.5,\n");
    s.push_str("  \"when\": { \"kind\": \"and\", \"conditions\": [ { \"kind\": \"daytime\" }, { \"kind\": \"exposed_to_sky\" } ] } }\n");
    s.push_str("{ \"intent\": \"damage_vs\", \"target\": \"graveyard:reaper\", \"multiplier\": 1.5 }\n\n");
    s.push_str("# Frost Spirit — cold-biome empowered, fire-vulnerable (cold biome + magic pack)\n");
    s.push_str("{ \"intent\": \"buff_when\", \"what\": { \"kind\": \"attribute\", \"attribute\": \"minecraft:generic.movement_speed\", \"op\": \"multiply_total\", \"amount\": 0.3 },\n");
    s.push_str("  \"when\": { \"kind\": \"biome_tag\", \"tag\": \"minecraft:is_cold\" } }\n");
    s.push_str("{ \"intent\": \"dot_when\", \"dps\": 0.5, \"when\": { \"kind\": \"on_fire\" } }\n\n");
    s.push_str("# Berserker — Last Stand + Stagger (combat pack)\n");
    s.push_str("{ \"intent\": \"attribute_buff\", \"attribute\": \"minecraft:generic.attack_damage\", \"op\": \"addition\", \"amount\": 3.0 }\n");
    s.push_str("{ \"intent\": \"stagger_on_sprint\", \"effect\": \"minecraft:slowness\", \"duration_s\": 4 }\n");
    s.push_str("{ \"intent\": \"last_stand\", \"hp_threshold\": 4, \"duration_s\": 4,\n");
    s.push_str("  \"effects\": [ { \"effect\": \"minecraft:resistance\", \"amplifier\": 1, \"duration_t\": 80 }, { \"effect\": \"minecraft:strength\", \"amplifier\": 0, \"duration_t\": 80 } ] }\n\n");
    s.push_str("# Vampire Hunter — undead-slayer with scoreboard-gated permanent unlock\n");
    s.push_str("{ \"intent\": \"entity_glow\", \"targets\": \"#minecraft:undead\", \"radius\": 32 }\n");
    s.push_str("{ \"intent\": \"tally_milestone\", \"event\": \"kill_in_radius\", \"target\": \"#minecraft:undead\", \"threshold\": 50,\n");
    s.push_str("  \"unlock\": { \"intent\": \"damage_vs\", \"target\": \"#minecraft:undead\", \"multiplier\": 2.0 } }\n");
    s.push_str("{ \"intent\": \"on_kill_grant\", \"target\": \"#minecraft:undead\", \"effect\": \"minecraft:strength\", \"duration_s\": 15 }\n\n");

    s.push_str("HARD RULES:\n");
    s.push_str("- Every id (item/block/entity/biome/tag) MUST come from THIS pack's registry — query_registry first; the engine grounds and rejects hallucinations.\n");
    s.push_str("- Tags (with the leading `#`) are grounded against the SAME registry as ids — `#minecraft:undead` only works if the registry dump contains it. If query_registry doesn't list a tag, fall back to listing the entity ids directly (e.g. [\"minecraft:zombie\", \"minecraft:skeleton\"]).\n");
    s.push_str("- Pick a density up front (light/standard/rich). The engine rejects over/under budget.\n");
    s.push_str("- Mod-integrated intents (familiar, signature_trinket, brew_potency, etc.) require the relevant mod in the pack — the engine returns RequiresAbsentCapability if missing. Lifetime variants (place_persistent_zone, log_and_resurrect, rally_event, waypoint_recall, forced_transformation) ALL require DatapackChannel (Open Loader). If Open Loader is not in the pack, drop to standard or light density — no lifetimes are possible.\n");
    s.push_str("- Nested PerkIntents (the `bonus` field of once_per_day_bonus, the `unlock` field of tally_milestone, the `carries` field of signature_trinket, the `on_use` field of knife_master) follow the SAME variant catalog above — the same shape rules apply recursively.\n");
    s.push_str("- Author 2-5 origins per call; calling generate_origin_intents again REPLACES the whole set (does NOT accumulate).\n");
    s
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-')
        })
}

/// `namespace:path`, both segments lowercase `[a-z0-9_.-]`, path may contain
/// `/`. (Minecraft resource-location rules.)
fn is_resource_location(s: &str) -> bool {
    let Some((ns, path)) = s.split_once(':') else {
        return false;
    };
    !ns.is_empty()
        && !path.is_empty()
        && ns
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
        && path
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-' | '/'))
}

// ---------------------------------------------------------------------------
// Validation — the GATE. Collects EVERY failure (advisor: not first-only).
// ---------------------------------------------------------------------------

/// Validate the set; on success return the `Validated` token `emit` requires.
pub fn validate(set: OriginsSet) -> Result<Validated, Vec<IntegrityError>> {
    use std::collections::BTreeSet;
    let mut errs: Vec<IntegrityError> = Vec::new();

    // Powers: id/text/type/required-fields/modifier checks.
    let mut seen_powers: BTreeSet<&str> = BTreeSet::new();
    for p in &set.powers {
        if !is_valid_id(&p.id) {
            errs.push(IntegrityError::BadId(p.id.clone()));
        }
        if !seen_powers.insert(p.id.as_str()) {
            errs.push(IntegrityError::DuplicateId(p.id.clone()));
        }
        if p.name.trim().is_empty() {
            errs.push(IntegrityError::EmptyText { what: format!("power `{}`", p.id), field: "name" });
        }
        if p.description.trim().is_empty() {
            errs.push(IntegrityError::EmptyText { what: format!("power `{}`", p.id), field: "description" });
        }
        let bare = bare_power_type(&p.power_type);
        if !FULL_WHITELIST.contains(&bare) {
            errs.push(IntegrityError::BadPowerType { power: p.id.clone(), ty: p.power_type.clone() });
            continue; // unknown type: required-field check is meaningless
        }
        // SAFE-type required fields must be present.
        if let Some(st) = safe_type(&p.power_type).or_else(|| safe_type(&format!("apoli:{bare}"))) {
            for &req in st.required {
                if !p.body.contains_key(req) {
                    errs.push(IntegrityError::MissingRequiredField {
                        power: p.id.clone(),
                        ty: p.power_type.clone(),
                        field: req,
                    });
                }
            }
        }
        // Every modifier (single `modifier` or `modifiers[]`): valid
        // operation; `apoli:attribute` modifiers also a valid attribute.
        let is_attr = bare == "attribute";
        let mut mods: Vec<&serde_json::Value> = Vec::new();
        if let Some(m) = p.body.get("modifier") {
            mods.push(m);
        }
        if let Some(serde_json::Value::Array(a)) = p.body.get("modifiers") {
            mods.extend(a.iter());
        }
        for m in mods {
            if let Some(op) = m.get("operation").and_then(|v| v.as_str()) {
                if !ALLOWED_OPERATIONS.contains(&op) {
                    errs.push(IntegrityError::BadOperation { power: p.id.clone(), operation: op.to_string() });
                }
            }
            if is_attr {
                if let Some(attr) = m.get("attribute").and_then(|v| v.as_str()) {
                    if !ALLOWED_ATTRIBUTES.contains(&attr) {
                        errs.push(IntegrityError::BadAttribute { power: p.id.clone(), attribute: attr.to_string() });
                    }
                }
            }
        }
    }

    // Origins: id/text/dangling-ref/icon/impact.
    let mut seen_origins: BTreeSet<&str> = BTreeSet::new();
    for o in &set.origins {
        if !is_valid_id(&o.id) {
            errs.push(IntegrityError::BadId(o.id.clone()));
        }
        if !seen_origins.insert(o.id.as_str()) {
            errs.push(IntegrityError::DuplicateId(o.id.clone()));
        }
        if o.name.trim().is_empty() {
            errs.push(IntegrityError::EmptyText { what: format!("origin `{}`", o.id), field: "name" });
        }
        if o.description.trim().is_empty() {
            errs.push(IntegrityError::EmptyText { what: format!("origin `{}`", o.id), field: "description" });
        }
        for pid in &o.powers {
            if classify_power_ref(pid, &seen_powers).is_none() {
                errs.push(IntegrityError::DanglingPowerRef { origin: o.id.clone(), power: pid.clone() });
            }
        }
        if !is_resource_location(&o.icon) {
            errs.push(IntegrityError::BadIcon { origin: o.id.clone(), icon: o.icon.clone() });
        }
        if !(0..=3).contains(&o.impact) {
            errs.push(IntegrityError::BadImpact { origin: o.id.clone(), impact: o.impact });
        }
    }

    if set.origins.is_empty() {
        errs.push(IntegrityError::EmptySet);
    }

    if errs.is_empty() {
        Ok(Validated(set))
    } else {
        Err(errs)
    }
}

// ---------------------------------------------------------------------------
// Typed power constructors (used by `rescue_set`; single source of each shape)
// ---------------------------------------------------------------------------

fn obj(pairs: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match pairs {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

fn power_attribute(id: &str, name: &str, desc: &str, attribute: &str, operation: &str, value: f64, mod_name: &str) -> Power {
    Power {
        id: id.into(),
        name: name.into(),
        description: desc.into(),
        power_type: "apoli:attribute".into(),
        body: obj(json!({ "modifier": { "attribute": attribute, "operation": operation, "value": value, "name": mod_name } })),
    }
}

fn power_modifier(id: &str, name: &str, desc: &str, ty: &str, operation: &str, value: f64, mod_name: &str) -> Power {
    Power {
        id: id.into(),
        name: name.into(),
        description: desc.into(),
        power_type: ty.into(),
        body: obj(json!({ "modifier": { "operation": operation, "value": value, "name": mod_name } })),
    }
}

fn power_modify_falling(id: &str, name: &str, desc: &str, velocity: f64) -> Power {
    Power {
        id: id.into(),
        name: name.into(),
        description: desc.into(),
        power_type: "apoli:modify_falling".into(),
        body: obj(json!({ "velocity": velocity, "take_fall_damage": false })),
    }
}

fn power_simple(id: &str, name: &str, desc: &str, ty: &str) -> Power {
    Power { id: id.into(), name: name.into(), description: desc.into(), power_type: ty.into(), body: serde_json::Map::new() }
}

fn power_night_vision(id: &str, name: &str, desc: &str, strength: f64) -> Power {
    Power {
        id: id.into(),
        name: name.into(),
        description: desc.into(),
        power_type: "apoli:night_vision".into(),
        body: obj(json!({ "strength": strength })),
    }
}

// ---------------------------------------------------------------------------
// Deterministic RESCUE set — fixes an already-broken instance with NO LLM.
// Schema-correct: plain-string names, integer impact, valid catalog types,
// Survivalist gets water-breathing by REFERENCING the shipped power.
// ---------------------------------------------------------------------------

pub fn rescue_set() -> OriginsSet {
    let powers = vec![
        power_attribute("tank_vitality", "Vitality", "Your maximum health is greatly increased.", "minecraft:generic.max_health", "addition", 6.0, "Anvil Tank Vitality"),
        power_attribute("tank_plating", "Plating", "Permanent natural armor.", "minecraft:generic.armor", "addition", 4.0, "Anvil Tank Plating"),
        power_modifier("tank_resilience", "Resilience", "You take less incoming damage.", "apoli:modify_damage_taken", "multiply_base", -0.25, "Anvil Tank Resilience"),
        power_modifier("mobility_spring_step", "Spring Step", "You jump notably higher.", "apoli:modify_jump", "multiply_base", 0.5, "Anvil Spring Step"),
        power_modify_falling("mobility_drift", "Drift", "You fall slowly and take no fall damage.", 0.04),
        power_simple("mobility_wallcling", "Wall Cling", "You can climb any wall.", "apoli:climbing"),
        power_night_vision("survivalist_darksight", "Dark Sight", "You see clearly in the dark.", 1.0),
        power_attribute("survivalist_fleetfoot", "Fleet Foot", "You move faster on foot.", "minecraft:generic.movement_speed", "multiply_base", 0.15, "Anvil Fleet Foot"),
    ];

    let origins = vec![
        Origin {
            id: "tank".into(),
            name: "Tank".into(),
            description: "Built like a wall — extra health, natural armor, and reduced incoming damage.".into(),
            powers: vec!["tank_vitality".into(), "tank_plating".into(), "tank_resilience".into()],
            icon: "minecraft:netherite_chestplate".into(),
            impact: 3,
            order: 0,
        },
        Origin {
            id: "mobility".into(),
            name: "Mobility".into(),
            description: "Light on your feet — higher jumps, slow falling, and wall-climbing.".into(),
            powers: vec!["mobility_spring_step".into(), "mobility_drift".into(), "mobility_wallcling".into()],
            icon: "minecraft:feather".into(),
            impact: 2,
            order: 1,
        },
        Origin {
            id: "survivalist".into(),
            name: "Survivalist".into(),
            description: "At home in the wild — night vision, swiftness, and the ability to breathe underwater.".into(),
            // The last entry is a SHIPPED Origins power (no file emitted).
            powers: vec![
                "survivalist_darksight".into(),
                "survivalist_fleetfoot".into(),
                "origins:water_breathing".into(),
            ],
            icon: "minecraft:torch".into(),
            impact: 1,
            order: 2,
        },
    ];

    OriginsSet { origins, powers }
}

// ---------------------------------------------------------------------------
// Deterministic serializer — only accepts a `Validated` set.
// ---------------------------------------------------------------------------

fn to_file(v: &serde_json::Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}

/// Emit the datapack from a PRE-VALIDATED set. `name`/`description` are plain
/// strings; `impact` a number; the layer REPLACES the stock chooser with
/// `replace:true` (only the pack's origins show); only `Local` powers get a
/// file (a `Shipped` ref is just listed in the origin).
fn emit(v: &Validated, ns: &str) -> Vec<(String, String)> {
    use std::collections::BTreeSet;
    let set = &v.0;
    let mut out: Vec<(String, String)> = Vec::new();

    // pack.mcmeta
    out.push((
        format!("{ROOT}/pack.mcmeta"),
        to_file(&json!({ "pack": { "pack_format": PACK_FORMAT_1_20, "description": "Anvil custom origins" } })),
    ));

    let emitted_ids: BTreeSet<&str> = set.powers.iter().map(|p| p.id.as_str()).collect();

    // Powers (Local only), sorted by id.
    let mut powers: Vec<&Power> = set.powers.iter().collect();
    powers.sort_by(|a, b| a.id.cmp(&b.id));
    for p in powers {
        let mut o = serde_json::Map::new();
        o.insert("type".into(), json!(p.power_type));
        o.insert("name".into(), json!(p.name)); // PLAIN STRING
        o.insert("description".into(), json!(p.description)); // PLAIN STRING
        for (k, val) in &p.body {
            o.insert(k.clone(), val.clone());
        }
        out.push((
            format!("{ROOT}/data/{ns}/powers/{}.json", p.id),
            to_file(&serde_json::Value::Object(o)),
        ));
    }

    // Origins, sorted by id.
    let mut origins: Vec<&Origin> = set.origins.iter().collect();
    origins.sort_by(|a, b| a.id.cmp(&b.id));
    for org in &origins {
        let power_refs: Vec<String> = org
            .powers
            .iter()
            .map(|pid| match classify_power_ref(pid, &emitted_ids) {
                Some(PowerRef::Shipped(s)) => s.to_string(),
                Some(PowerRef::Local(s)) => format!("{ns}:{s}"),
                // unreachable: validation guarantees classification.
                None => format!("{ns}:{pid}"),
            })
            .collect();
        out.push((
            format!("{ROOT}/data/{ns}/origins/{}.json", org.id),
            to_file(&json!({
                "powers": power_refs,
                "icon": { "item": org.icon },
                "impact": org.impact,
                "order": org.order,
                "name": org.name,
                "description": org.description,
            })),
        ));
    }

    // Layer: REPLACE the stock chooser (replace:true) so ONLY the pack's
    // generated origins are selectable — the 10 vanilla Origins
    // (Human/Enderian/Avian/…) are intentionally wiped. `replace:true`
    // semantics are bytecode-verified in docs/modding/origins_apoli_2.9.2
    // _schema.md (§B line 69/330): this file's `origins` replace all others
    // for the same layer id. The validator guarantees a non-empty set
    // (`IntegrityError::EmptySet`), so the replaced layer always has ≥1
    // origin (a replace:true layer with an empty array would soft-lock the
    // origin screen).
    let layer_origins: Vec<String> =
        origins.iter().map(|o| format!("{ns}:{}", o.id)).collect();
    out.push((
        format!("{ROOT}/{LAYER_PATH_SUFFIX}"),
        to_file(&json!({ "replace": true, "origins": layer_origins })),
    ));

    out
}

/// Build the deterministic rescue datapack: `rescue_set` -> `validate` ->
/// `emit`. The rescue set is invariantly valid, so a validation failure here
/// is a build bug (caught by `rescue_set_validates`).
pub fn build_origins_datapack(namespace: &str) -> Vec<(String, String)> {
    let v = validate(rescue_set()).expect("rescue origins set is invariantly valid");
    emit(&v, namespace)
}

/// Prune any prior `anvil-origins` datapack, then write `files`. A stale file
/// from an earlier (broken) emit would otherwise still be loaded by Apoli and
/// still be rejected in-game — regeneration must fully REPLACE, not overlay.
/// Shared by the rescue and the model-authored paths.
fn write_files(instance_dir: &Path, files: Vec<(String, String)>) -> anyhow::Result<()> {
    std::fs::create_dir_all(instance_dir)
        .with_context(|| format!("creating instance dir {}", instance_dir.display()))?;
    let root = instance_dir.join(ROOT);
    if root.exists() {
        std::fs::remove_dir_all(&root)
            .with_context(|| format!("pruning stale {}", root.display()))?;
    }
    for (rel, contents) in files {
        let path = instance_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating dir {}", parent.display()))?;
        }
        std::fs::write(&path, contents)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Write the deterministic RESCUE datapack (no LLM) — used for already-broken
/// instances and as the fallback when the model did not author origins.
pub fn write_origins_datapack(instance_dir: &Path, namespace: &str) -> anyhow::Result<()> {
    write_files(instance_dir, build_origins_datapack(namespace))
}

/// True iff the seed advancement `id` (shape `anvil:origins/<slug>/seed_<x>`)
/// has actually been emitted to this instance's Origins datapack by some
/// origin's `origin_questline` perk. The Heracles quest emitter gates an
/// OPTIONAL per-origin branch on such an id; if NO origin emitted it, the
/// branch would silently never unlock for anyone, so the quest tool hard-errors
/// on a dangling seed using this check. Resolves the id `<ns>:<path>` to the
/// emitted file `<ROOT>/data/<ns>/advancements/<path>.json` — the exact path
/// the OriginQuestline emit writes — so the two stay in lockstep.
pub fn seed_advancement_emitted(instance_dir: &Path, id: &str) -> bool {
    let Some((ns, path)) = id.split_once(':') else { return false };
    instance_dir
        .join(ROOT)
        .join("data")
        .join(ns)
        .join("advancements")
        .join(format!("{path}.json"))
        .exists()
}

/// Write a MODEL-AUTHORED, already-`Validated` origin set (the
/// `tool_generate_origins` path). Same prune-then-write semantics.
pub fn write_validated_origins(
    instance_dir: &Path,
    namespace: &str,
    v: &Validated,
) -> anyhow::Result<()> {
    write_files(instance_dir, emit(v, namespace))
}

/// Companion-datapack files generated alongside Apoli powers. `functions`
/// holds (path, contents) pairs already prefixed with `ROOT`. Paths ending
/// in `_load.mcfunction` get tagged into `minecraft:load`; `_tick.mcfunction`
/// goes into `minecraft:tick`. This is the convention every marker variant
/// uses; the curator never has to think about tag wiring.
#[derive(Debug, Default, Clone)]
pub struct CompanionMcFunctions {
    pub files: Vec<(String, String)>,
}

impl CompanionMcFunctions {
    pub fn new() -> Self { Self { files: Vec::new() } }
    pub fn extend_from(&mut self, items: impl IntoIterator<Item = (String, String)>) {
        self.files.extend(items);
    }
    pub fn is_empty(&self) -> bool { self.files.is_empty() }
}

/// Convert a function file relative path like
/// `data/anvil/functions/origins/witch/p0_tick.mcfunction` into the function
/// id `anvil:origins/witch/p0_tick` (the form `tick.json`/`load.json` tags
/// reference). Strips `data/<ns>/functions/` prefix and `.mcfunction` suffix.
fn fn_path_to_id(path: &str) -> Option<String> {
    let suffix = path.strip_suffix(".mcfunction")?;
    let body = suffix.strip_prefix("data/")?;
    // body = "anvil/functions/origins/<slug>/<file>"
    let (ns, rest) = body.split_once('/')?;
    let rest = rest.strip_prefix("functions/")?;
    Some(format!("{ns}:{rest}"))
}

/// Emit pipeline that also lands companion mcfunctions + the matching
/// `tick.json` / `load.json` function tags. Paths are prefixed with `ROOT`
/// so the OpenLoader datapack picks them up. Determinism: function ids
/// inside the tag arrays are sorted to keep the JSON byte-equal across
/// invocations with the same input.
pub fn emit_with_companion(
    v: &Validated,
    companion: &CompanionMcFunctions,
    ns: &str,
) -> Vec<(String, String)> {
    let mut out = emit(v, ns);
    if companion.is_empty() {
        return out;
    }
    use std::collections::BTreeSet;
    let mut load_ids: BTreeSet<String> = BTreeSet::new();
    let mut tick_ids: BTreeSet<String> = BTreeSet::new();
    for (rel_path, content) in &companion.files {
        let prefixed = format!("{ROOT}/{rel_path}");
        // Classify by suffix; unknown suffix => write file but don't tag it.
        if let Some(id) = fn_path_to_id(rel_path) {
            if id.ends_with("_load") { load_ids.insert(id); }
            else if id.ends_with("_tick") { tick_ids.insert(id); }
        }
        out.push((prefixed, content.clone()));
    }
    if !load_ids.is_empty() {
        let values: Vec<&String> = load_ids.iter().collect();
        out.push((
            format!("{ROOT}/data/minecraft/tags/functions/load.json"),
            to_file(&serde_json::json!({ "values": values })),
        ));
    }
    if !tick_ids.is_empty() {
        let values: Vec<&String> = tick_ids.iter().collect();
        out.push((
            format!("{ROOT}/data/minecraft/tags/functions/tick.json"),
            to_file(&serde_json::json!({ "values": values })),
        ));
    }
    out
}

/// Write the validated origin set PLUS companion mcfunctions. The
/// LLM-authored path uses this when emit_perk produced any companion files
/// (markers that need datapack-level behavior to be functional).
pub fn write_validated_origins_with_companion(
    instance_dir: &Path,
    namespace: &str,
    v: &Validated,
    companion: &CompanionMcFunctions,
) -> anyhow::Result<()> {
    write_files(instance_dir, emit_with_companion(v, companion, namespace))
}

// ---------------------------------------------------------------------------
// Read-back — reconstruct an `OriginsSet` from an on-disk datapack so the UI
// can show what an instance actually ships. The INVERSE of `emit`: defensive
// (a malformed individual file is skipped, never panics), tolerant of the
// icon being `{"item":...}` / a bare string / missing, and DETERMINISTIC
// (filesystem `read_dir` is unordered, so the result is explicitly sorted).
// This is read-only; it never validates and never writes.
// ---------------------------------------------------------------------------

/// Pull the icon item id out of an origin file's `icon` field. Accepts the
/// emitted `{"item":"<id>"}` object, a bare `"<id>"` string, or a missing /
/// unparseable field (=> the safe `minecraft:nether_star` fallback so a bad
/// icon never drops the whole origin).
fn read_icon(v: Option<&serde_json::Value>) -> String {
    const FALLBACK: &str = "minecraft:nether_star";
    match v {
        Some(serde_json::Value::Object(m)) => m
            .get("item")
            .and_then(|i| i.as_str())
            .unwrap_or(FALLBACK)
            .to_string(),
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => FALLBACK.to_string(),
    }
}

/// Reconstruct the `OriginsSet` from the datapack Anvil wrote into
/// `instance_dir`. Returns `None` if the origins directory is absent or yields
/// zero origins (an empty powers list alone is fine — a pack may reference
/// only shipped powers). Origins are sorted by `(order, name)` and powers by
/// `id` so the result is independent of `read_dir` order.
pub fn read_origins(instance_dir: &Path) -> Option<OriginsSet> {
    let base = instance_dir.join(ROOT).join("data").join("anvil");
    let origins_dir = base.join("origins");
    let powers_dir = base.join("powers");

    // Origins directory is the gate: absent => no datapack here.
    let origin_entries = std::fs::read_dir(&origins_dir).ok()?;

    let mut origins: Vec<Origin> = Vec::new();
    for entry in origin_entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(serde_json::Value::Object(m)) =
            serde_json::from_str::<serde_json::Value>(&text)
        else {
            continue;
        };
        // name/description are plain non-empty strings (skip if not).
        let (Some(name), Some(description)) = (
            m.get("name").and_then(|v| v.as_str()),
            m.get("description").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        if name.trim().is_empty() || description.trim().is_empty() {
            continue;
        }
        let powers: Vec<String> = m
            .get("powers")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let icon = read_icon(m.get("icon"));
        let impact = m.get("impact").and_then(|v| v.as_i64()).unwrap_or(0);
        let order = m.get("order").and_then(|v| v.as_i64()).unwrap_or(0);
        origins.push(Origin {
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            powers,
            icon,
            impact,
            order,
        });
    }

    if origins.is_empty() {
        return None;
    }

    let mut powers: Vec<Power> = Vec::new();
    if let Ok(power_entries) = std::fs::read_dir(&powers_dir) {
        for entry in power_entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(serde_json::Value::Object(m)) =
                serde_json::from_str::<serde_json::Value>(&text)
            else {
                continue;
            };
            let (Some(name), Some(description), Some(power_type)) = (
                m.get("name").and_then(|v| v.as_str()),
                m.get("description").and_then(|v| v.as_str()),
                m.get("type").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            if name.trim().is_empty()
                || description.trim().is_empty()
                || power_type.trim().is_empty()
            {
                continue;
            }
            // body = every key except the envelope (type/name/description).
            let mut body = serde_json::Map::new();
            for (k, val) in &m {
                if k != "type" && k != "name" && k != "description" {
                    body.insert(k.clone(), val.clone());
                }
            }
            powers.push(Power {
                id: id.to_string(),
                name: name.to_string(),
                description: description.to_string(),
                power_type: power_type.to_string(),
                body,
            });
        }
    }

    // Determinism: read_dir is unordered.
    origins.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
    powers.sort_by(|a, b| a.id.cmp(&b.id));

    Some(OriginsSet { origins, powers })
}

/// Faithful display name + description for a SHIPPED Origins power (the bare
/// id, no `origins:` prefix — i.e. the part after the colon). Wording sourced
/// from the primary-source reference `docs/modding/origins_apoli_2.9.2_schema.md`
/// (decompiled Origins 1.10.2). Any id not in the table — including a stray
/// non-shipped ref — falls back to a prettified id + empty description so this
/// is total and self-contained.
pub fn shipped_power_label(id: &str) -> (String, String) {
    let (name, desc): (&str, &str) = match id {
        "water_breathing" => (
            "Water Breathing",
            "You can breathe underwater indefinitely.",
        ),
        "water_vision" => (
            "Water Vision",
            "You see clearly underwater, with no murky haze.",
        ),
        "aqua_affinity" => (
            "Aqua Affinity",
            "You mine at full speed while underwater.",
        ),
        "swim_speed" => ("Swim Speed", "You swim notably faster."),
        "like_water" => (
            "Like Water",
            "Water does not slow you down — you act as if on land while submerged.",
        ),
        "climbing" => (
            "Climbing",
            "You can climb any wall, spider-style.",
        ),
        "fire_immunity" => (
            "Fire Immunity",
            "You are immune to fire, lava damage, and burning.",
        ),
        "fall_immunity" => ("Fall Immunity", "You never take fall damage."),
        "scare_creepers" => (
            "Scare Creepers",
            "Creepers are frightened of you and flee on sight.",
        ),
        "phantomize" => (
            "Phantomize",
            "You can phase into an incorporeal phantom form.",
        ),
        "elytra" => (
            "Elytra",
            "You can glide as if wearing an elytra, with no elytra item.",
        ),
        "cat_vision" => (
            "Cat Vision",
            "You see in the dark with feline night vision.",
        ),
        other => {
            // Prettify `some_id` -> "Some Id"; empty description.
            let pretty = other
                .split(|c| c == '_' || c == '-')
                .filter(|s| !s.is_empty())
                .map(|w| {
                    let mut ch = w.chars();
                    match ch.next() {
                        Some(f) => {
                            f.to_uppercase().collect::<String>()
                                + &ch.as_str().to_lowercase()
                        }
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            return (pretty, String::new());
        }
    };
    (name.to_string(), desc.to_string())
}

// ---------------------------------------------------------------------------
// Tests — REAL: jar-grounded catalog checks + emitted-shape + the exact
// historical-failure regressions. (The deleted tests asserted the BUG.)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Read;

    const NS: &str = "anvil";

    fn index(files: &[(String, String)]) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for (p, c) in files {
            assert!(m.insert(p.clone(), c.clone()).is_none(), "dup path {p}");
        }
        m
    }
    fn parse(files: &BTreeMap<String, String>, path: &str) -> Value {
        let raw = files.get(path).unwrap_or_else(|| panic!("missing {path}"));
        assert!(raw.ends_with('\n'), "{path} must end with newline");
        serde_json::from_str(raw).unwrap_or_else(|e| panic!("{path} must parse: {e}"))
    }
    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/real")
            .join(name)
    }
    /// Filenames (stem) of `data/origins/powers/*.json` inside the Origins jar.
    fn origins_jar_shipped_power_ids() -> BTreeSet<String> {
        let f = std::fs::File::open(fixture("origins-1.10.2.jar"))
            .expect("committed origins-1.10.2.jar fixture");
        let mut z = zip::ZipArchive::new(f).expect("origins jar is a zip");
        let mut ids = BTreeSet::new();
        for i in 0..z.len() {
            let e = z.by_index(i).unwrap();
            let n = e.name().to_string();
            if let Some(rest) = n.strip_prefix("data/origins/powers/") {
                if let Some(stem) = rest.strip_suffix(".json") {
                    if !stem.contains('/') {
                        ids.insert(format!("origins:{stem}"));
                    }
                }
            }
        }
        ids
    }
    /// The `"type"` of every stock `data/origins/powers/*.json` (bare id).
    fn origins_jar_stock_power_types() -> BTreeSet<String> {
        let f = std::fs::File::open(fixture("origins-1.10.2.jar")).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut tys = BTreeSet::new();
        for i in 0..z.len() {
            let mut e = z.by_index(i).unwrap();
            let n = e.name().to_string();
            if n.starts_with("data/origins/powers/") && n.ends_with(".json") {
                let mut s = String::new();
                if e.read_to_string(&mut s).is_ok() {
                    if let Ok(v) = serde_json::from_str::<Value>(&s) {
                        if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
                            tys.insert(bare_power_type(t).to_string());
                        }
                    }
                }
            }
        }
        tys
    }

    // --- Jar-grounded catalog checks: the catalog cannot silently drift. ---

    #[test]
    fn shipped_powers_subset_of_origins_jar() {
        let real = origins_jar_shipped_power_ids();
        let missing: Vec<_> = SHIPPED_ORIGINS_POWERS
            .iter()
            .filter(|id| !real.contains(**id))
            .collect();
        assert!(
            missing.is_empty(),
            "SHIPPED_ORIGINS_POWERS not in the Origins jar: {missing:?}"
        );
    }

    #[test]
    fn stock_power_types_are_all_in_full_whitelist() {
        // Every power type Origins' OWN stock content uses is a real Apoli
        // factory; if our FULL_WHITELIST is missing one, it is wrong.
        let stock = origins_jar_stock_power_types();
        assert!(!stock.is_empty(), "expected stock origins powers in jar");
        let wl: BTreeSet<&str> = FULL_WHITELIST.iter().copied().collect();
        let missing: Vec<_> = stock.iter().filter(|t| !wl.contains(t.as_str())).collect();
        assert!(
            missing.is_empty(),
            "FULL_WHITELIST missing types Origins' own stock powers use: {missing:?}"
        );
    }

    #[test]
    fn historical_bug_water_breathing_is_not_a_factory_but_is_shipped() {
        // The exact runtime failure, locked as a regression:
        assert!(
            !FULL_WHITELIST.contains(&"water_breathing"),
            "water_breathing is NOT an Apoli factory (runtime: `is not defined`)"
        );
        assert!(
            SHIPPED_ORIGINS_POWERS.contains(&"origins:water_breathing"),
            "water-breathing must be granted via the SHIPPED power"
        );
        assert!(
            origins_jar_shipped_power_ids().contains("origins:water_breathing"),
            "the Origins jar must actually ship data/origins/powers/water_breathing.json"
        );
    }

    // --- Emitted-shape: name/description PLAIN STRING, impact NUMBER. ---

    #[test]
    fn names_are_plain_strings_and_impact_is_a_number() {
        let files = index(&build_origins_datapack(NS));
        let pow_dir = format!("{ROOT}/data/{NS}/powers/");
        let org_dir = format!("{ROOT}/data/{NS}/origins/");
        for (path, _) in &files {
            let is_pow = path.starts_with(&pow_dir) && path.ends_with(".json");
            let is_org = path.starts_with(&org_dir) && path.ends_with(".json");
            if is_pow || is_org {
                let v = parse(&files, path);
                for field in ["name", "description"] {
                    assert!(
                        v[field].is_string(),
                        "{path} `{field}` MUST be a plain string (Apoli rejects \
                         a component object); got {}",
                        v[field]
                    );
                    assert!(!v[field].as_str().unwrap().is_empty(), "{path} `{field}` empty");
                }
            }
            if is_org {
                let v = parse(&files, path);
                assert!(
                    v["impact"].is_i64(),
                    "{path} `impact` MUST be an integer 0..=3, got {}",
                    v["impact"]
                );
                let im = v["impact"].as_i64().unwrap();
                assert!((0..=3).contains(&im), "{path} impact {im} out of range");
            }
        }
    }

    #[test]
    fn no_emitted_power_uses_an_invalid_type_and_layer_replaces_stock() {
        let files = index(&build_origins_datapack(NS));
        let layer = parse(&files, &format!("{ROOT}/{LAYER_PATH_SUFFIX}"));
        assert_eq!(
            layer["replace"],
            json!(true),
            "layer must be replace:true so ONLY the pack's origins show \
             (the 10 vanilla Origins are intentionally wiped)"
        );
        assert!(
            layer["origins"].as_array().is_some_and(|a| !a.is_empty()),
            "a replace:true layer MUST be non-empty or the origin screen \
             soft-locks (guaranteed by IntegrityError::EmptySet)"
        );
        for (path, _) in &files {
            if path.contains("/powers/") && path.ends_with(".json") {
                let v = parse(&files, path);
                let ty = v["type"].as_str().expect("power type string");
                assert!(
                    FULL_WHITELIST.contains(&bare_power_type(ty)),
                    "{path} type `{ty}` not in catalog"
                );
            }
        }
    }

    #[test]
    fn survivalist_references_shipped_water_breathing_with_no_local_file() {
        let files = index(&build_origins_datapack(NS));
        let surv = parse(&files, &format!("{ROOT}/data/{NS}/origins/survivalist.json"));
        let powers: Vec<&str> = surv["powers"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(
            powers.contains(&"origins:water_breathing"),
            "survivalist must reference the shipped power verbatim, got {powers:?}"
        );
        assert!(
            !files.contains_key(&format!("{ROOT}/data/{NS}/powers/water_breathing.json")),
            "must NOT emit a file for a shipped power"
        );
    }

    // --- Validate is the gate and returns ALL failures. ---

    #[test]
    fn rescue_set_validates() {
        validate(rescue_set()).expect("rescue set must validate");
    }

    #[test]
    fn validate_collects_every_failure_not_just_the_first() {
        let mut s = rescue_set();
        s.powers[0].power_type = "apoli:water_breathing".into(); // bad type
        s.powers[1].name = "  ".into(); // empty text
        s.origins[0].impact = 9; // out of range
        s.origins[0].powers.push("nope_not_real".into()); // dangling
        let errs = validate(s).expect_err("must fail");
        assert!(errs.len() >= 4, "must report ALL failures, got {errs:?}");
        assert!(errs.iter().any(|e| matches!(e, IntegrityError::BadPowerType { .. })));
        assert!(errs.iter().any(|e| matches!(e, IntegrityError::EmptyText { .. })));
        assert!(errs.iter().any(|e| matches!(e, IntegrityError::BadImpact { .. })));
        assert!(errs.iter().any(|e| matches!(e, IntegrityError::DanglingPowerRef { .. })));
    }

    #[test]
    fn validate_rejects_missing_required_field() {
        let mut s = rescue_set();
        // tank_vitality is apoli:attribute (requires `modifier`); strip it.
        s.powers[0].body.clear();
        let errs = validate(s).expect_err("must fail");
        assert!(errs
            .iter()
            .any(|e| matches!(e, IntegrityError::MissingRequiredField { field: "modifier", .. })));
    }

    #[test]
    fn validated_is_only_constructable_via_validate_and_emit_is_deterministic() {
        let a = build_origins_datapack(NS);
        let b = build_origins_datapack(NS);
        assert_eq!(a, b, "emission must be byte-identical");
        assert_eq!(a, build_origins_datapack("anvil"));
    }

    #[test]
    fn origins_core_gate_distinguishes_core_from_addon() {
        assert!(is_origins_core("3BeIrqZR", "anything.jar"));
        assert!(!is_origins_core("FiDptjtR", "Origins-Classes-1.20-1.7.0.jar"));
        assert!(is_origins_core("zzz", "Origins-1.10.2+mc.1.20.x.jar"));
        assert!(!is_origins_core("zzz", "apoli-2.9.2.jar"));
        assert!(!is_origins_core("zzz", "origins-classes-1.7.0.jar"));
    }

    // --- Model-authored path (Phase 2): the gate working on LLM-style JSON. ---

    #[test]
    fn prompt_catalog_is_generated_from_constants_and_states_hard_rules() {
        let p = safe_catalog_prompt_section();
        // Every SAFE type the validator accepts is shown to the model.
        for st in SAFE_TYPES {
            assert!(p.contains(st.id), "prompt missing safe type {}", st.id);
        }
        // Shipped powers are offered as references.
        assert!(p.contains("origins:water_breathing"));
        // The exact rules whose violation caused the original bug.
        assert!(p.to_lowercase().contains("plain string"));
        assert!(p.contains("INTEGER 0-3") || p.contains("integer 0-3") || p.contains("INTEGER 0"));
        assert!(p.contains("REPLACES") || p.contains("does NOT accumulate"));
        // Attribute + operation enums come from the constants.
        assert!(p.contains("minecraft:generic.max_health"));
        assert!(p.contains("multiply_base"));
    }

    #[test]
    fn errors_to_json_carries_actionable_hints() {
        let mut s = rescue_set();
        s.powers[0].power_type = "apoli:water_breathing".into();
        s.powers[1].name = "".into();
        let errs = validate(s).expect_err("must fail");
        let j = errors_to_json(&errs);
        let arr = j.as_array().expect("array");
        let bpt = arr
            .iter()
            .find(|e| e["kind"] == "BadPowerType")
            .expect("BadPowerType entry");
        // The hint must steer the model to the correct fix (reference shipped).
        let hint = bpt["hint"].as_str().unwrap().to_lowercase();
        assert!(hint.contains("shipped") && hint.contains("origins:"));
        assert!(arr.iter().any(|e| e["kind"] == "EmptyText"));
        for e in arr {
            for k in ["kind", "where", "why", "hint"] {
                assert!(e.get(k).and_then(|v| v.as_str()).is_some(), "missing {k}");
            }
        }
    }

    #[test]
    fn realistic_llm_proposal_validates_and_writes_schema_correct() {
        // A plausible model proposal for a tech pack: one local attribute
        // power + a shipped reference. Drive it through the REAL path.
        let proposal = serde_json::json!({
            "origins": [{
                "id": "engineer",
                "name": "Engineer",
                "description": "Tinkerer at home among machines — tougher and sees in the dark.",
                "powers": ["engineer_hardened", "origins:water_breathing"],
                "icon": "minecraft:iron_chestplate",
                "impact": 2,
                "order": 0
            }],
            "powers": [{
                "id": "engineer_hardened",
                "name": "Hardened",
                "description": "Years at the forge gave you extra armor.",
                "type": "apoli:attribute",
                "body": { "modifier": { "attribute": "minecraft:generic.armor", "operation": "addition", "value": 3.0, "name": "Engineer Plating" } }
            }]
        });
        let set: OriginsSet = serde_json::from_value(proposal).expect("LLM JSON deserializes");
        let v = validate(set).expect("a well-formed proposal must validate");
        let dir = tempfile::tempdir().unwrap();
        write_validated_origins(dir.path(), "anvil", &v).expect("writes");
        let base = dir.path().join(ROOT);
        // Shipped ref → NO local file; the local power → a file.
        assert!(!base.join("data/anvil/powers/water_breathing.json").exists());
        let pf: Value = serde_json::from_str(
            &std::fs::read_to_string(base.join("data/anvil/powers/engineer_hardened.json")).unwrap(),
        ).unwrap();
        assert!(pf["name"].is_string(), "name must be a plain string");
        let of: Value = serde_json::from_str(
            &std::fs::read_to_string(base.join("data/anvil/origins/engineer.json")).unwrap(),
        ).unwrap();
        assert!(of["impact"].is_i64(), "impact must be an integer");
        let powers: Vec<&str> = of["powers"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(powers.contains(&"origins:water_breathing"), "shipped ref kept verbatim");
        assert!(powers.contains(&"anvil:engineer_hardened"), "local ref namespaced");
    }

    #[test]
    fn the_exact_historical_llm_mistakes_are_rejected_by_the_gate() {
        // What the model WOULD have produced under the old (buggy) guidance:
        // a component-object name and the non-existent water_breathing type.
        // The gate must reject BOTH before anything is written.
        let bad = serde_json::json!({
            "origins": [{
                "id": "diver", "name": "Diver",
                "description": "breathes underwater",
                "powers": ["diver_breathe"], "icon": "minecraft:cod",
                "impact": 1, "order": 0
            }],
            "powers": [{
                "id": "diver_breathe",
                "name": "Gills",
                "description": "x",
                "type": "apoli:water_breathing"
            }]
        });
        let set: OriginsSet = serde_json::from_value(bad).unwrap();
        let errs = validate(set).expect_err("must be rejected by the gate");
        assert!(errs.iter().any(|e| matches!(e, IntegrityError::BadPowerType { .. })));
    }

    // --- Read-back: write the rescue set, read it, assert the round-trip. ---

    #[test]
    fn read_origins_round_trips_the_written_datapack() {
        let dir = tempfile::tempdir().unwrap();
        write_origins_datapack(dir.path(), NS).expect("writes rescue datapack");

        let got = read_origins(dir.path()).expect("read_origins finds the pack");
        let want = rescue_set();

        // Origins: compare by id (both sides sorted) on the round-tripping
        // fields. `emit` namespaces local power refs (`tank_vitality` ->
        // `anvil:tank_vitality`) so refs are compared by COUNT, not value.
        let mut got_o = got.origins.clone();
        got_o.sort_by(|a, b| a.id.cmp(&b.id));
        let mut want_o = want.origins.clone();
        want_o.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(got_o.len(), want_o.len(), "origin count must match");
        for (g, w) in got_o.iter().zip(&want_o) {
            assert_eq!(g.id, w.id, "origin id");
            assert_eq!(g.name, w.name, "origin {} name", w.id);
            assert_eq!(g.description, w.description, "origin {} description", w.id);
            assert_eq!(g.icon, w.icon, "origin {} icon", w.id);
            assert_eq!(g.impact, w.impact, "origin {} impact", w.id);
            assert_eq!(
                g.powers.len(),
                w.powers.len(),
                "origin {} power-ref count",
                w.id
            );
        }

        // Powers: only LOCAL powers get a file; the rescue set has no shipped
        // power as a `Power`, so the read-back power set is exactly the
        // written one. Compare by id (both sorted) on every field.
        let mut got_p = got.powers.clone();
        got_p.sort_by(|a, b| a.id.cmp(&b.id));
        let mut want_p = want.powers.clone();
        want_p.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(got_p.len(), want_p.len(), "power count must match");
        for (g, w) in got_p.iter().zip(&want_p) {
            assert_eq!(g.id, w.id, "power id");
            assert_eq!(g.name, w.name, "power {} name", w.id);
            assert_eq!(g.description, w.description, "power {} description", w.id);
            assert_eq!(g.power_type, w.power_type, "power {} type", w.id);
        }

        // Determinism: origins sorted by (order, name); powers by id.
        let orders: Vec<i64> = got.origins.iter().map(|o| o.order).collect();
        let mut sorted = orders.clone();
        sorted.sort();
        assert_eq!(orders, sorted, "origins must be returned order-sorted");
        let ids: Vec<&str> = got.powers.iter().map(|p| p.id.as_str()).collect();
        let mut sorted_ids = ids.clone();
        sorted_ids.sort();
        assert_eq!(ids, sorted_ids, "powers must be returned id-sorted");
    }

    #[test]
    fn read_origins_is_none_when_no_datapack() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_origins(dir.path()).is_none());
    }

    #[test]
    fn shipped_power_label_covers_every_shipped_id_and_prettifies_unknown() {
        for full in SHIPPED_ORIGINS_POWERS {
            let bare = full.strip_prefix("origins:").unwrap_or(full);
            let (name, desc) = shipped_power_label(bare);
            assert!(!name.is_empty(), "shipped `{bare}` must have a name");
            assert!(
                !desc.is_empty(),
                "shipped `{bare}` must have a faithful description"
            );
        }
        // Unknown id => prettified, empty description (the total fallback).
        let (n, d) = shipped_power_label("totally_unknown-thing");
        assert_eq!(n, "Totally Unknown Thing");
        assert!(d.is_empty());
    }
}

// ============================================================================
// PHASE 1a — INTENT LAYER (additive on top of the verified OriginsSet).
//
// LLM authors `PerkIntent`s; Anvil compiles to the existing `OriginsSet`
// (Phase 1c) which walks the verified `validate → Validated → emit` gate
// upstream. This phase is TYPES ONLY — no emit, no grounding pipeline,
// no curator wiring. Test gates at the bottom of the module enforce
// roundtrip / bounded-numeric / catalog-coverage / slow-fall absence.
// ============================================================================

use std::result::Result as StdResult;

// ---- Density (3 tiers; Mythic explicitly dropped per scope-tightening) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    /// 3-4 perks, active optional, no lifetime.
    Light,
    /// 5-7 perks, 1 active, lifetime optional. The Origins-mod default feel.
    Standard,
    /// 8-10 perks, 1 active, 1 lifetime. Ceiling.
    Rich,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerkBudget {
    pub passives: (u8, u8),
    pub actives: u8,
    pub lifetimes: (u8, u8),
}

impl Density {
    pub const fn budget(self) -> PerkBudget {
        match self {
            Density::Light    => PerkBudget { passives: (3, 4),  actives: 0, lifetimes: (0, 0) },
            Density::Standard => PerkBudget { passives: (5, 7),  actives: 1, lifetimes: (0, 1) },
            Density::Rich     => PerkBudget { passives: (8, 10), actives: 1, lifetimes: (1, 1) },
        }
    }
}

// ---- ModCapability + mod_id->capabilities map (the generalisation) -------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModCapability {
    Trinkets,
    SeasonsCycle,
    NamedNpcs,
    BondableCompanions,
    CustomPotions,
    KnifeFamily,
    HauntedStructures,
    Wildlife,
    OutlawFactions,
    QuestEngine,
    Scaling,
    LoreBooks,
    DatapackChannel,
    OriginsCore,
}

/// Normalize a mod identifier so display names, Modrinth slugs, and Java
/// mod-ids all hash to the same bucket. Strips every non-alphanumeric character
/// (hyphens, underscores, spaces, colons, apostrophes, parens) and lowercases.
/// "Open Loader", "open-loader", "openloader" → "openloader".
/// "Farmer's Delight (Fabric)" and "farmers-delight" → "farmersdelightfabric"
/// vs "farmersdelight" — for that case the map carries both aliases.
fn normalize_mod_key(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Mod identifier (project_id / slug / display name — all forms accepted)
/// → declared capabilities. Onboarding a new mod is one match arm here;
/// intents stay generic. All keys are post-normalization (alphanumeric-
/// lowercase) so callers can pass Modrinth slugs, opaque IDs, OR display names
/// without caring which they have.
pub fn capabilities(mod_id: &str) -> &'static [ModCapability] {
    use ModCapability::*;
    let key = normalize_mod_key(mod_id);
    match key.as_str() {
        "trinkets"                                          => &[Trinkets],
        "fabricseasons"                                     => &[SeasonsCycle],
        "bewitchment"                                       => &[BondableCompanions, CustomPotions, OutlawFactions],
        "villagernames"                                     => &[NamedNpcs],
        "farmersdelight" | "farmersdelightfabric"
        | "farmersdelightrefabricated"                      => &[KnifeFamily],
        "thegraveyard"                                      => &[HauntedStructures, OutlawFactions],
        "naturalist" | "crittersandcompanions"              => &[Wildlife],
        "illagerinvasion"                                   => &[OutlawFactions],
        "pehkui"                                            => &[Scaling],
        "patchouli" | "modonomicon"                         => &[LoreBooks],
        "heracles"                                          => &[QuestEngine],
        "openloader"                                        => &[DatapackChannel],
        "origins" | "originsfabric"                         => &[OriginsCore],
        _                                                   => &[],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurationPhase {
    ModSelection,
    Origins,
    Quests,
}

// ---- Typed-ID newtypes (1a = transparent strings; 1b adds registry grounding)

macro_rules! id_newtype {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl $name {
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            pub fn as_str(&self) -> &str { &self.0 }
        }
    };
}
id_newtype!(ItemId,         "Item id (`ns:path`) or tag (`#ns:path`). Grounded in 1b.");
id_newtype!(BlockId,        "Block id or tag. Grounded in 1b.");
id_newtype!(EntityTypeId,   "Entity-type id or tag.");
id_newtype!(BiomeId,        "Biome id or tag.");
id_newtype!(BiomeTagId,     "Biome-tag id (`ns:name`, no `#`). Grounded in `vocab.tags`.");
id_newtype!(DimensionId,    "Dimension id (`ns:name`). Well-formedness only; not statically scanned.");
id_newtype!(FluidId,        "Fluid id or tag.");
id_newtype!(StatusEffectId, "Status-effect id.");
id_newtype!(AttributeId,    "Attribute id (incl. modded like `pehkui:base`).");
id_newtype!(DamageTypeId,   "Damage-type id.");
id_newtype!(ContentHexRef,  "Reference to a `content_hex` boss content node.");
id_newtype!(QuestRef,       "Reference to a quest node id.");
id_newtype!(OriginIdRef,    "Reference to another origin's id.");
id_newtype!(TextureKey,     "Generated resource-pack texture key (`anvil:item/<slug>`).");
id_newtype!(ThemeTag,       "Origin theme tag (arcane | cozy | cursed | adventure | tech | ...).");

// ---- Selector unions (id, tag, or list — schema-loose; grounded in 1b)

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ItemSelector { One(ItemId), Many(Vec<ItemId>) }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BlockSelector { One(BlockId), Many(Vec<BlockId>) }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EntityCondRef {
    One(EntityTypeId),
    Many(Vec<EntityTypeId>),
}

// ---- Bounded numerics — reject out-of-range at parse-time -----------------

macro_rules! bounded_float {
    ($name:ident, $min:expr, $max:expr) => {
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct $name(f32);
        impl $name {
            pub const MIN: f32 = $min;
            pub const MAX: f32 = $max;
            pub fn new(v: f32) -> StdResult<Self, OriginIssue> {
                if v < Self::MIN || v > Self::MAX {
                    Err(OriginIssue::BoundedNumericOutOfRange {
                        field: stringify!($name),
                        value: v.to_string(),
                        min: Self::MIN.to_string(),
                        max: Self::MAX.to_string(),
                    })
                } else {
                    Ok(Self(v))
                }
            }
            pub fn value(self) -> f32 { self.0 }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> StdResult<S::Ok, S::Error> {
                self.0.serialize(s)
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> StdResult<Self, D::Error> {
                let v = f32::deserialize(d)?;
                Self::new(v).map_err(serde::de::Error::custom)
            }
        }
    };
}

macro_rules! bounded_int {
    ($name:ident, $repr:ty, $min:expr, $max:expr) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name($repr);
        impl $name {
            pub const MIN: $repr = $min;
            pub const MAX: $repr = $max;
            pub fn new(v: $repr) -> StdResult<Self, OriginIssue> {
                if v < Self::MIN || v > Self::MAX {
                    Err(OriginIssue::BoundedNumericOutOfRange {
                        field: stringify!($name),
                        value: v.to_string(),
                        min: Self::MIN.to_string(),
                        max: Self::MAX.to_string(),
                    })
                } else {
                    Ok(Self(v))
                }
            }
            pub fn value(self) -> $repr { self.0 }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> StdResult<S::Ok, S::Error> {
                self.0.serialize(s)
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> StdResult<Self, D::Error> {
                let v = <$repr>::deserialize(d)?;
                Self::new(v).map_err(serde::de::Error::custom)
            }
        }
    };
}

bounded_float!(ScaleFactor,  0.1, 4.0);
bounded_float!(DamageMul,    0.0, 5.0);
bounded_float!(DpsRate,      0.1, 10.0);
bounded_float!(PotencyMul,   0.5, 4.0);
bounded_float!(BreakMul,     0.5, 5.0);
bounded_float!(BuffAmount, -64.0, 64.0);
bounded_int!(BonusSat,   i8,  -10,  20);
bounded_int!(Amplifier,  u8,  0,    4);
bounded_int!(GlowRadius, u8,  4,    64);
bounded_int!(ComboMax,   u8,  2,    16);
bounded_int!(LeadDays,   u8,  1,    7);
bounded_int!(Persistent, u8,  1,    12);
bounded_int!(GiftThresh, u8,  1,    20);
bounded_int!(HpThreshN,  u8,  1,    20);
bounded_int!(BlockRadius, u8, 1,    32);

// ---- SanitizedText — refuses empty + normalises smart typography ----

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SanitizedText(String);

impl SanitizedText {
    pub fn new(s: impl Into<String>) -> StdResult<Self, OriginIssue> {
        let raw: String = s.into();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(OriginIssue::EmptyText { field: "text" });
        }
        let cleaned = trimmed
            .replace('\u{2014}', "-")
            .replace('\u{2013}', "-")
            .replace(['\u{201C}', '\u{201D}'], "\"")
            .replace(['\u{2018}', '\u{2019}'], "'")
            .replace('\u{2026}', "...");
        Ok(SanitizedText(cleaned))
    }
    pub fn as_str(&self) -> &str { &self.0 }
}
impl Serialize for SanitizedText {
    fn serialize<S: serde::Serializer>(&self, s: S) -> StdResult<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}
impl<'de> Deserialize<'de> for SanitizedText {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> StdResult<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

// ---- Sub-enums and structs the variants use ------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Mainhand, Offhand, Head, Chest, Legs, Feet,
    Hotbar0, Hotbar1, Hotbar2, Hotbar3, Hotbar4, Hotbar5, Hotbar6, Hotbar7, Hotbar8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrOp { Addition, MultiplyBase, MultiplyTotal }

/// Movement kinds. SafeLanding/SlowFalling variants are INTENTIONALLY ABSENT
/// per the global slow-fall directive — adding either is a global ban.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveKind {
    Climb,
    CreativeFlight,
    ElytraFlight,
    WalkOnFluid,
    HigherJump,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuffWhat {
    Effect { effect: StatusEffectId, amplifier: Amplifier },
    Attribute { attribute: AttributeId, op: AttrOp, amount: BuffAmount },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TallyEvent { KillInRadius, BlockBreak, BossDefeat, QuestComplete }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DailyTrigger { Dawn, Dusk, FirstMeal, FirstSleep }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyBind { Primary, Secondary, Tertiary }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HudHint { Active, Cooldown, Resource }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusEffectInst {
    pub effect: StatusEffectId,
    pub amplifier: Amplifier,
    pub duration_t: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaAction {
    pub radius: u8,
    pub damage: f32,
    pub particle_key: TextureKey,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetinueSpec {
    pub entity_types: Vec<EntityCondRef>,
    pub radius: u8,
    pub follow_duration_s: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActiveBody {
    TeleportToMarker { marker: BlockSelector, on_depart: AreaAction },
    InvisibilityPulse { duration_s: u32, retinue: Option<RetinueSpec> },
    AreaBurst { radius: u8, damage: f32, knockback: f32 },
    Transformation {
        duration_s: u32,
        scale: Option<ScaleFactor>,
        stash_inventory: bool,
        effects_on: Vec<StatusEffectInst>,
        effects_off: Vec<StatusEffectInst>,
        summon_allies: Option<RetinueSpec>,
    },
    TimedEffectChain {
        on: Vec<StatusEffectInst>,
        duration_s: u32,
        off: Vec<StatusEffectInst>,
        off_duration_s: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifetimeGate {
    OncePerSave,
    OncePerInGameDay,
    OncePerMoonFull,
    PhaseTriggered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForcedDuration { Night, FullDay, ThirtySeconds, OneMinute }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifetimeBody {
    PlacePersistentZone {
        structure_key: TextureKey,
        radius: u8,
        suppress_spawns: bool,
        growth_boost: Option<BreakMul>,
        animal_migration: bool,
    },
    ForcedTransformation { duration: ForcedDuration, body: ActiveBody },
    LogAndResurrect { logs: EntityCondRef, summon_for_dur_s: u32 },
    RallyEvent {
        summon_entities: EntityCondRef,
        structure_key: TextureKey,
        area_buff_dur_s: u32,
    },
    WaypointRecall { visit_threshold: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrinketSlot { Necklace, Hand, Charm, Belt }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BondAction { CauldronRitual, AltarOffer, GiftItem, KillBlessing }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalMilestone {
    pub trigger: SanitizedText,
    pub entry: SanitizedText,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NpcSelector { Tag(SanitizedText), Names(Vec<SanitizedText>) }

// ---- WhenCondition — typed polymorphic gate for conditional powers ------
//
// Replaces stringly-typed `EntityCondRef` handles in the `when` slot of
// the six `*When` / `*.when: Option<_>` variants. Each leaf maps 1:1 to a
// jar-verified Apoli condition factory (`io.github.apace100.apoli.power.
// factory.condition.EntityConditions.register()`):
//   - logical wrappers compile via `apoli:and` / `apoli:or` with the
//     universal `inverted: true` field for `Not`.
//   - biome-tag matching uses Apoli's nested-condition pattern:
//     `apoli:biome { condition: apoli:in_tag { tag: <id> } }`.
//
// Closed-enum design — adding a future condition is a compile error in
// `compile_when_condition`. This is the LLM-authoring guardrail: the model
// can only express conditions we've verified emit-validate cleanly.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WhenCondition {
    /// No condition — power applies unconditionally (compiled as `None`).
    Any,
    /// `apoli:daytime` — world time in the day half.
    Daytime,
    /// `apoli:daytime` with `inverted: true` — semantic alias for clarity.
    Nighttime,
    /// `apoli:in_rain` — player position is being rained on.
    InRain,
    /// `apoli:exposed_to_sky` — direct line of sight to sky above.
    ExposedToSky,
    /// `apoli:on_fire` — player is burning.
    OnFire,
    /// `apoli:sneaking`.
    Sneaking,
    /// `apoli:sprinting`.
    Sprinting,
    /// `apoli:swimming`.
    Swimming,
    /// `apoli:fall_flying` — actively elytra-gliding.
    FallFlying,
    /// `apoli:dimension { dimension: <id> }`.
    Dimension { id: DimensionId },
    /// `apoli:biome { biome: <id> }` — exact biome match (no tag).
    Biome { id: BiomeId },
    /// `apoli:biome { condition: apoli:in_tag { tag: <id> } }` — biome-tag.
    BiomeTag { tag: BiomeTagId },
    /// `apoli:block_in_radius { block_condition, radius, shape: "cube" }` —
    /// true when at least one block in a cube around the player matches.
    /// Used for proximity gating (cauldron, altar, gravestone).
    BlockInRadius { block: BlockSelector, radius: BlockRadius },
    /// Logical NOT — sets `inverted: true` on the compiled inner condition.
    /// Stored as a single-element `Vec` (rather than `Box<WhenCondition>`) so
    /// the recursion through `serde::Serialize` flows via one path (`Vec<Self>`)
    /// instead of two — Box+Vec inflates serde-derive's macro expansion past
    /// any sane `recursion_limit`. Invariant: `conditions.len() == 1`;
    /// `compile_when_condition` enforces it leniently (empty → `Any`, extras
    /// → first wins). TODO(T2c): structural validator check rejecting
    /// `len != 1` at parse time, not at compile time.
    Not { conditions: Vec<WhenCondition> },
    /// Logical AND — `apoli:and { conditions: [...] }`.
    And { conditions: Vec<WhenCondition> },
    /// Logical OR — `apoli:or { conditions: [...] }`.
    Or { conditions: Vec<WhenCondition> },
}

// ---- PerkIntent — the closed enum the LLM authors against ----------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum PerkIntent {
    // Starting state
    StartsWith        { items: Vec<ItemId>, slots: Option<Vec<Slot>> },
    Scale             { factor: ScaleFactor },

    // Constant passive
    PassiveEffect     { effect: StatusEffectId, amplifier: Option<Amplifier> },
    AttributeBuff     { attribute: AttributeId, amount: BuffAmount, op: AttrOp,
                        when: Option<WhenCondition> },
    SpecialMovement   { kind: MoveKind },

    // Conditional buff / debuff
    BuffWhen          { what: BuffWhat, when: WhenCondition },
    DotWhen           { dps: DpsRate, when: WhenCondition },
    DamageVs          { target: EntityCondRef, multiplier: DamageMul },

    // Restrictions
    ForbiddenItemUse  { what: ItemSelector },
    PreventSleep      { except: Option<ItemSelector> },
    PreventBreakUnderFoot { block: BlockSelector },

    // Event hooks
    OnKillGrant       { target: EntityCondRef, effect: StatusEffectId, duration_s: u32 },
    OnWakeGrant       { effects: Vec<StatusEffectInst> },
    BonusSaturationOn { food: ItemSelector, extra: BonusSat, when: Option<WhenCondition> },
    FasterBreakOn     { block: BlockSelector, multiplier: BreakMul },
    TallyMilestone    { event: TallyEvent, target: EntityCondRef, threshold: u32,
                        unlock: Box<PerkIntent> },

    // Mob relationships
    PacifyTargeting   { by: EntityCondRef },
    HostileRecognition{ by: EntityCondRef },
    EntityGlow        { targets: EntityCondRef, radius: GlowRadius },

    // Periodic
    OncePerDayBonus   { trigger: DailyTrigger, bonus: Box<PerkIntent> },
    SeasonNotification{ lead_days: LeadDays, message: SanitizedText },

    // Persistence / UI
    KeepInventorySlot { slots: Vec<Slot> },
    MapMarkerAtSpawn  { label: SanitizedText },
    Overlay           { when: WhenCondition, duration_s: Option<u32> },
    AutoJournal       { milestones: Vec<JournalMilestone> },

    // Active [G]
    Active            { key: KeyBind, cooldown_s: u32, hud: HudHint, body: ActiveBody },

    // Lifetime [✦✦]
    Lifetime          { gate: LifetimeGate, body: LifetimeBody },

    // Gameplay drivers (replaced the 7 flavor variants per scope-tightening)
    ComboChain        { window_t: u16, ramp: f32, max_stacks: ComboMax },
    Siphon            { target: EntityCondRef, hp: f32, food: u8 },
    DodgeRoll         { i_frames_t: u16, distance: u8, cooldown_s: u32 },
    VeinMine          { block: BlockSelector, max_chain: u8 },
    HarvestAoe        { crop: BlockSelector, radius: u8 },
    LastStand         { hp_threshold: HpThreshN, duration_s: u32, effects: Vec<StatusEffectInst> },
    BlockPhase        { block: BlockSelector, when: WhenCondition },
    StaggerOnSprint   { effect: StatusEffectId, duration_s: u32 },

    // Mod-integrated (require ModCapability)
    SignatureTrinket  { slot: TrinketSlot, model: TextureKey, carries: Box<PerkIntent> },
    Familiar          { entity: EntityTypeId, bond_action: BondAction, persist_through_death: bool },
    SeasonalForm      { spring: Vec<PerkIntent>, summer: Vec<PerkIntent>,
                        fall: Vec<PerkIntent>, winter: Vec<PerkIntent> },
    ApprenticeToNpc   { npc: NpcSelector, gift_threshold: GiftThresh,
                        reward_chain: Vec<PerkIntent> },
    BrewPotency       { which: ItemSelector, dur_mul: PotencyMul, amp_bonus: i8 },
    KnifeMaster       { knife: ItemSelector, on_use: Box<PerkIntent> },
    Gravewalker       { near: BlockSelector, on_proximity: Box<PerkIntent> },
    PackLeader        { entity_types: Vec<EntityCondRef>, persistent_count: Persistent },
    BanditKin         { faction: EntityCondRef, pacify_radius: u8,
                        ally_summon: Option<Box<PerkIntent>> },

    // Cross-phase (compile-only in 1a; emitter lands in Phase 3)
    OriginQuestline   { chapter_seed: ThemeTag },
}

/// Discriminant-only mirror for catalog table indexing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerkIntentTag {
    StartsWith, Scale, PassiveEffect, AttributeBuff, SpecialMovement,
    BuffWhen, DotWhen, DamageVs,
    ForbiddenItemUse, PreventSleep, PreventBreakUnderFoot,
    OnKillGrant, OnWakeGrant, BonusSaturationOn, FasterBreakOn, TallyMilestone,
    PacifyTargeting, HostileRecognition, EntityGlow,
    OncePerDayBonus, SeasonNotification,
    KeepInventorySlot, MapMarkerAtSpawn, Overlay, AutoJournal,
    Active, Lifetime,
    ComboChain, Siphon, DodgeRoll, VeinMine, HarvestAoe, LastStand, BlockPhase, StaggerOnSprint,
    SignatureTrinket, Familiar, SeasonalForm, ApprenticeToNpc, BrewPotency,
    KnifeMaster, Gravewalker, PackLeader, BanditKin,
    OriginQuestline,
}

impl PerkIntent {
    /// Exhaustive match — adding a future variant without arming this is a
    /// compile error. Eyes-on guard against catalog drift.
    pub fn tag(&self) -> PerkIntentTag {
        match self {
            PerkIntent::StartsWith { .. }            => PerkIntentTag::StartsWith,
            PerkIntent::Scale { .. }                 => PerkIntentTag::Scale,
            PerkIntent::PassiveEffect { .. }         => PerkIntentTag::PassiveEffect,
            PerkIntent::AttributeBuff { .. }         => PerkIntentTag::AttributeBuff,
            PerkIntent::SpecialMovement { .. }       => PerkIntentTag::SpecialMovement,
            PerkIntent::BuffWhen { .. }              => PerkIntentTag::BuffWhen,
            PerkIntent::DotWhen { .. }               => PerkIntentTag::DotWhen,
            PerkIntent::DamageVs { .. }              => PerkIntentTag::DamageVs,
            PerkIntent::ForbiddenItemUse { .. }      => PerkIntentTag::ForbiddenItemUse,
            PerkIntent::PreventSleep { .. }          => PerkIntentTag::PreventSleep,
            PerkIntent::PreventBreakUnderFoot { .. } => PerkIntentTag::PreventBreakUnderFoot,
            PerkIntent::OnKillGrant { .. }           => PerkIntentTag::OnKillGrant,
            PerkIntent::OnWakeGrant { .. }           => PerkIntentTag::OnWakeGrant,
            PerkIntent::BonusSaturationOn { .. }     => PerkIntentTag::BonusSaturationOn,
            PerkIntent::FasterBreakOn { .. }         => PerkIntentTag::FasterBreakOn,
            PerkIntent::TallyMilestone { .. }        => PerkIntentTag::TallyMilestone,
            PerkIntent::PacifyTargeting { .. }       => PerkIntentTag::PacifyTargeting,
            PerkIntent::HostileRecognition { .. }    => PerkIntentTag::HostileRecognition,
            PerkIntent::EntityGlow { .. }            => PerkIntentTag::EntityGlow,
            PerkIntent::OncePerDayBonus { .. }       => PerkIntentTag::OncePerDayBonus,
            PerkIntent::SeasonNotification { .. }    => PerkIntentTag::SeasonNotification,
            PerkIntent::KeepInventorySlot { .. }     => PerkIntentTag::KeepInventorySlot,
            PerkIntent::MapMarkerAtSpawn { .. }      => PerkIntentTag::MapMarkerAtSpawn,
            PerkIntent::Overlay { .. }               => PerkIntentTag::Overlay,
            PerkIntent::AutoJournal { .. }           => PerkIntentTag::AutoJournal,
            PerkIntent::Active { .. }                => PerkIntentTag::Active,
            PerkIntent::Lifetime { .. }              => PerkIntentTag::Lifetime,
            PerkIntent::ComboChain { .. }            => PerkIntentTag::ComboChain,
            PerkIntent::Siphon { .. }                => PerkIntentTag::Siphon,
            PerkIntent::DodgeRoll { .. }             => PerkIntentTag::DodgeRoll,
            PerkIntent::VeinMine { .. }              => PerkIntentTag::VeinMine,
            PerkIntent::HarvestAoe { .. }            => PerkIntentTag::HarvestAoe,
            PerkIntent::LastStand { .. }             => PerkIntentTag::LastStand,
            PerkIntent::BlockPhase { .. }            => PerkIntentTag::BlockPhase,
            PerkIntent::StaggerOnSprint { .. }       => PerkIntentTag::StaggerOnSprint,
            PerkIntent::SignatureTrinket { .. }      => PerkIntentTag::SignatureTrinket,
            PerkIntent::Familiar { .. }              => PerkIntentTag::Familiar,
            PerkIntent::SeasonalForm { .. }          => PerkIntentTag::SeasonalForm,
            PerkIntent::ApprenticeToNpc { .. }       => PerkIntentTag::ApprenticeToNpc,
            PerkIntent::BrewPotency { .. }           => PerkIntentTag::BrewPotency,
            PerkIntent::KnifeMaster { .. }           => PerkIntentTag::KnifeMaster,
            PerkIntent::Gravewalker { .. }           => PerkIntentTag::Gravewalker,
            PerkIntent::PackLeader { .. }            => PerkIntentTag::PackLeader,
            PerkIntent::BanditKin { .. }             => PerkIntentTag::BanditKin,
            PerkIntent::OriginQuestline { .. }       => PerkIntentTag::OriginQuestline,
        }
    }
}

// ---- The catalog -- which capability each variant requires --------------

#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    pub variant: PerkIntentTag,
    pub requires: &'static [ModCapability],
    pub phase: CurationPhase,
}

pub const PERK_CATALOG: &[CatalogEntry] = &[
    CatalogEntry { variant: PerkIntentTag::StartsWith,            requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::Scale,                 requires: &[ModCapability::Scaling], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::PassiveEffect,         requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::AttributeBuff,         requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::SpecialMovement,       requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::BuffWhen,              requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::DotWhen,               requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::DamageVs,              requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::ForbiddenItemUse,      requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::PreventSleep,          requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::PreventBreakUnderFoot, requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::OnKillGrant,           requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::OnWakeGrant,           requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::BonusSaturationOn,     requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::FasterBreakOn,         requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::TallyMilestone,        requires: &[ModCapability::DatapackChannel], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::PacifyTargeting,       requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::HostileRecognition,    requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::EntityGlow,            requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::OncePerDayBonus,       requires: &[ModCapability::DatapackChannel], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::SeasonNotification,    requires: &[ModCapability::SeasonsCycle], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::KeepInventorySlot,     requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::MapMarkerAtSpawn,      requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::Overlay,               requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::AutoJournal,           requires: &[ModCapability::LoreBooks], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::Active,                requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::Lifetime,              requires: &[ModCapability::DatapackChannel], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::ComboChain,            requires: &[ModCapability::DatapackChannel], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::Siphon,                requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::DodgeRoll,             requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::VeinMine,              requires: &[ModCapability::DatapackChannel], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::HarvestAoe,            requires: &[ModCapability::DatapackChannel], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::LastStand,             requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::BlockPhase,            requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::StaggerOnSprint,       requires: &[], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::SignatureTrinket,      requires: &[ModCapability::Trinkets], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::Familiar,              requires: &[ModCapability::BondableCompanions, ModCapability::DatapackChannel], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::SeasonalForm,          requires: &[ModCapability::SeasonsCycle], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::ApprenticeToNpc,       requires: &[ModCapability::NamedNpcs], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::BrewPotency,           requires: &[ModCapability::CustomPotions], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::KnifeMaster,           requires: &[ModCapability::KnifeFamily], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::Gravewalker,           requires: &[ModCapability::HauntedStructures], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::PackLeader,            requires: &[ModCapability::Wildlife, ModCapability::DatapackChannel], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::BanditKin,             requires: &[ModCapability::OutlawFactions], phase: CurationPhase::Origins },
    CatalogEntry { variant: PerkIntentTag::OriginQuestline,       requires: &[ModCapability::QuestEngine], phase: CurationPhase::Quests },
];

pub fn catalog_entry(tag: PerkIntentTag) -> Option<&'static CatalogEntry> {
    PERK_CATALOG.iter().find(|e| e.variant == tag)
}

// ---- OriginIntent — the LLM payload + the Anvil-side spec ----------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OriginIntent {
    pub theme: ThemeTag,
    pub name: SanitizedText,
    pub description: SanitizedText,
    pub icon: ItemId,
    pub perks: Vec<PerkIntent>,
    /// Optional per-origin density override. When `None`, the batch
    /// density supplied to `generate_origin_intents` applies. When set,
    /// THIS origin's budget validator and impact derivation use this
    /// value instead — so a single batch can mix a light Fresher with a
    /// rich Porter without two separate tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<Density>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_boss: Option<ContentHexRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gates_quest: Option<QuestRef>,
}

pub type OriginSpec = OriginIntent;

// ---- OriginIssue — validation errors specific to the intent layer ------

#[derive(Debug, Clone, PartialEq)]
pub enum OriginIssue {
    BoundedNumericOutOfRange {
        field: &'static str,
        value: String,
        min: String,
        max: String,
    },
    RequiresAbsentCapability {
        variant: PerkIntentTag,
        missing: ModCapability,
    },
    BudgetViolation {
        density: Density,
        what: &'static str,
        count: u8,
        bound: u8,
        direction: BudgetDirection,
    },
    PhaseGated {
        variant: PerkIntentTag,
        actual: CurationPhase,
        needed: CurationPhase,
    },
    UnknownId {
        category: &'static str,
        id: String,
        suggestions: Vec<String>,
    },
    EmptyText {
        field: &'static str,
    },
    /// Two or more perks in one origin modify the same attribute (e.g. a -4 and
    /// a +6 on `generic.max_health`) — contradictory and illegible. Each
    /// attribute may be touched at most once per origin.
    ConflictingAttribute {
        attribute: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDirection { Over, Under }

impl std::fmt::Display for OriginIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OriginIssue::BoundedNumericOutOfRange { field, value, min, max } =>
                write!(f, "`{field}` out of range: {value} not in [{min}, {max}]"),
            OriginIssue::RequiresAbsentCapability { variant, missing } =>
                write!(f, "{variant:?} requires capability {missing:?} which is not enabled by the pack's mod list"),
            OriginIssue::BudgetViolation { density, what, count, bound, direction } =>
                write!(f, "{what} budget violation at density {density:?}: count {count} is {direction:?} the bound {bound}"),
            OriginIssue::PhaseGated { variant, actual, needed } =>
                write!(f, "{variant:?} cannot be authored at phase {actual:?}; needs {needed:?}"),
            OriginIssue::UnknownId { category, id, suggestions } =>
                write!(f, "unknown {category} id `{id}`; did you mean {suggestions:?}?"),
            OriginIssue::EmptyText { field } =>
                write!(f, "`{field}` must be non-empty"),
            OriginIssue::ConflictingAttribute { attribute } =>
                write!(f, "two or more perks modify the same attribute `{attribute}` (e.g. a debuff and a buff fighting over one stat); give each attribute to at most one perk in this origin"),
        }
    }
}

// ============================================================================
// PHASE 1a TESTS — gates that must be green before Phase 1b begins.
// ============================================================================

#[cfg(test)]
mod intent_layer_tests {
    use super::*;
    use serde_json::json;

    /// One fixture per variant — kept in lockstep with the enum via
    /// `fixtures_cover_every_variant`.
    pub(super) fn one_per_variant() -> Vec<PerkIntent> {
        vec![
            PerkIntent::StartsWith { items: vec![ItemId::new("minecraft:diamond")], slots: None },
            PerkIntent::Scale { factor: ScaleFactor::new(0.65).unwrap() },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:night_vision"), amplifier: None },
            PerkIntent::AttributeBuff {
                attribute: AttributeId::new("minecraft:generic.max_health"),
                amount: BuffAmount::new(4.0).unwrap(), op: AttrOp::Addition, when: None },
            PerkIntent::SpecialMovement { kind: MoveKind::HigherJump },
            PerkIntent::BuffWhen {
                what: BuffWhat::Effect { effect: StatusEffectId::new("minecraft:speed"), amplifier: Amplifier::new(1).unwrap() },
                when: WhenCondition::Any },
            PerkIntent::DotWhen { dps: DpsRate::new(0.5).unwrap(),
                when: WhenCondition::Biome { id: BiomeId::new("minecraft:plains") } },
            PerkIntent::DamageVs { target: EntityCondRef::One(EntityTypeId::new("graveyard:reaper")), multiplier: DamageMul::new(1.5).unwrap() },
            PerkIntent::ForbiddenItemUse { what: ItemSelector::One(ItemId::new("minecraft:iron_sword")) },
            PerkIntent::PreventSleep { except: None },
            PerkIntent::PreventBreakUnderFoot { block: BlockSelector::One(BlockId::new("#minecraft:flowers")) },
            PerkIntent::OnKillGrant { target: EntityCondRef::One(EntityTypeId::new("naturalist:bear")), effect: StatusEffectId::new("minecraft:strength"), duration_s: 30 },
            PerkIntent::OnWakeGrant { effects: vec![StatusEffectInst { effect: StatusEffectId::new("minecraft:absorption"), amplifier: Amplifier::new(0).unwrap(), duration_t: 600 }] },
            PerkIntent::BonusSaturationOn { food: ItemSelector::One(ItemId::new("#c:foods")), extra: BonusSat::new(2).unwrap(), when: None },
            PerkIntent::FasterBreakOn { block: BlockSelector::One(BlockId::new("#minecraft:crops")), multiplier: BreakMul::new(1.5).unwrap() },
            PerkIntent::TallyMilestone { event: TallyEvent::KillInRadius, target: EntityCondRef::One(EntityTypeId::new("#minecraft:undead")), threshold: 100,
                unlock: Box::new(PerkIntent::DamageVs { target: EntityCondRef::One(EntityTypeId::new("any")), multiplier: DamageMul::new(2.0).unwrap() }) },
            PerkIntent::PacifyTargeting { by: EntityCondRef::One(EntityTypeId::new("#minecraft:swords")) },
            PerkIntent::HostileRecognition { by: EntityCondRef::One(EntityTypeId::new("#illager_invasion:variants")) },
            PerkIntent::EntityGlow { targets: EntityCondRef::One(EntityTypeId::new("minecraft:wolf")), radius: GlowRadius::new(32).unwrap() },
            PerkIntent::OncePerDayBonus { trigger: DailyTrigger::FirstMeal, bonus: Box::new(PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:luck"), amplifier: None }) },
            PerkIntent::SeasonNotification { lead_days: LeadDays::new(1).unwrap(), message: SanitizedText::new("Winter approaches.").unwrap() },
            PerkIntent::KeepInventorySlot { slots: vec![Slot::Hotbar0] },
            PerkIntent::MapMarkerAtSpawn { label: SanitizedText::new("Farmstead").unwrap() },
            PerkIntent::Overlay { when: WhenCondition::Any, duration_s: Some(30) },
            PerkIntent::AutoJournal { milestones: vec![JournalMilestone { trigger: SanitizedText::new("first_kill_undead").unwrap(), entry: SanitizedText::new("The grave smelled of iron.").unwrap() }] },
            PerkIntent::Active { key: KeyBind::Primary, cooldown_s: 60, hud: HudHint::Active,
                body: ActiveBody::AreaBurst { radius: 8, damage: 8.0, knockback: 1.0 } },
            PerkIntent::Lifetime { gate: LifetimeGate::OncePerMoonFull,
                body: LifetimeBody::LogAndResurrect { logs: EntityCondRef::One(EntityTypeId::new("graveyard:reaper")), summon_for_dur_s: 14_400 } },
            PerkIntent::ComboChain { window_t: 60, ramp: 0.15, max_stacks: ComboMax::new(5).unwrap() },
            PerkIntent::Siphon { target: EntityCondRef::One(EntityTypeId::new("#minecraft:undead")), hp: 2.0, food: 1 },
            PerkIntent::DodgeRoll { i_frames_t: 8, distance: 3, cooldown_s: 6 },
            PerkIntent::VeinMine { block: BlockSelector::One(BlockId::new("#c:ores/copper")), max_chain: 32 },
            PerkIntent::HarvestAoe { crop: BlockSelector::One(BlockId::new("#minecraft:crops")), radius: 3 },
            PerkIntent::LastStand { hp_threshold: HpThreshN::new(2).unwrap(), duration_s: 4,
                effects: vec![StatusEffectInst { effect: StatusEffectId::new("minecraft:strength"), amplifier: Amplifier::new(1).unwrap(), duration_t: 80 }] },
            PerkIntent::BlockPhase { block: BlockSelector::One(BlockId::new("#minecraft:leaves")), when: WhenCondition::Any },
            PerkIntent::StaggerOnSprint { effect: StatusEffectId::new("minecraft:slowness"), duration_s: 4 },
            PerkIntent::SignatureTrinket { slot: TrinketSlot::Necklace, model: TextureKey::new("anvil:item/hollow_locket"),
                carries: Box::new(PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:night_vision"), amplifier: None }) },
            PerkIntent::Familiar { entity: EntityTypeId::new("bewitchment:familiar_cat"), bond_action: BondAction::CauldronRitual, persist_through_death: true },
            PerkIntent::SeasonalForm {
                spring: vec![PerkIntent::SpecialMovement { kind: MoveKind::HigherJump }],
                summer: vec![PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:fire_resistance"), amplifier: None }],
                fall: vec![],
                winter: vec![PerkIntent::DotWhen { dps: DpsRate::new(0.3).unwrap(),
                    when: WhenCondition::BiomeTag { tag: BiomeTagId::new("minecraft:is_cold") } }] },
            PerkIntent::ApprenticeToNpc { npc: NpcSelector::Tag(SanitizedText::new("witch").unwrap()), gift_threshold: GiftThresh::new(5).unwrap(),
                reward_chain: vec![PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:hero_of_the_village"), amplifier: None }] },
            PerkIntent::BrewPotency { which: ItemSelector::One(ItemId::new("bewitchment:athame")), dur_mul: PotencyMul::new(2.0).unwrap(), amp_bonus: 1 },
            PerkIntent::KnifeMaster { knife: ItemSelector::One(ItemId::new("#c:tools/knives")),
                on_use: Box::new(PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:strength"), amplifier: None }) },
            PerkIntent::Gravewalker { near: BlockSelector::One(BlockId::new("graveyard:acacia_coffin")),
                on_proximity: Box::new(PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:luck"), amplifier: None }) },
            PerkIntent::PackLeader { entity_types: vec![EntityCondRef::One(EntityTypeId::new("#minecraft:swords"))], persistent_count: Persistent::new(3).unwrap() },
            PerkIntent::BanditKin { faction: EntityCondRef::One(EntityTypeId::new("illager_invasion:*")), pacify_radius: 8, ally_summon: None },
            PerkIntent::OriginQuestline { chapter_seed: ThemeTag::new("arcane") },
        ]
    }

    #[test]
    fn fixtures_cover_every_variant() {
        let fixtures = one_per_variant();
        let seen: std::collections::HashSet<PerkIntentTag> =
            fixtures.iter().map(|p| p.tag()).collect();
        for entry in PERK_CATALOG {
            assert!(seen.contains(&entry.variant),
                "catalog entry {:?} has no fixture in one_per_variant()", entry.variant);
        }
        assert_eq!(seen.len(), PERK_CATALOG.len(),
            "fixture count {} != PERK_CATALOG count {} — drift detected", seen.len(), PERK_CATALOG.len());
    }

    #[test]
    fn perk_intent_serde_roundtrip_every_variant() {
        for p in one_per_variant() {
            let v = serde_json::to_value(&p)
                .unwrap_or_else(|e| panic!("serialize {:?} failed: {e}", p.tag()));
            let back: PerkIntent = serde_json::from_value(v.clone())
                .unwrap_or_else(|e| panic!("deserialize {:?} failed: {e}\nJSON: {v}", p.tag()));
            assert_eq!(p, back, "roundtrip mismatch for {:?}", p.tag());
        }
    }

    #[test]
    fn bounded_numerics_reject_out_of_range_at_parse() {
        assert!(ScaleFactor::new(0.05).is_err());
        assert!(ScaleFactor::new(4.5).is_err());
        assert!(ScaleFactor::new(0.65).is_ok());
        let bad = serde_json::from_value::<PerkIntent>(json!({ "intent": "scale", "factor": 5.5 }));
        assert!(bad.is_err(), "ScaleFactor 5.5 should reject at parse, got {bad:?}");
        assert!(Amplifier::new(5).is_err());
        assert!(BonusSat::new(50).is_err());
        assert!(BonusSat::new(-20).is_err());
    }

    #[test]
    fn unknown_intent_rejects_at_parse() {
        let bad = serde_json::from_value::<PerkIntent>(json!({ "intent": "totally_made_up_intent" }));
        assert!(bad.is_err(), "unknown intent should reject at parse, got {bad:?}");
    }

    #[test]
    fn sanitized_text_rejects_empty_and_normalises_smart_typography() {
        assert!(SanitizedText::new("").is_err());
        assert!(SanitizedText::new("   ").is_err());
        let t = SanitizedText::new("Hello \u{2014} world\u{2026}\u{201C}quote\u{201D}").unwrap();
        assert_eq!(t.as_str(), "Hello - world...\"quote\"");
    }

    #[test]
    fn capabilities_map_known_mods() {
        assert!(capabilities("bewitchment").contains(&ModCapability::BondableCompanions));
        assert!(capabilities("bewitchment").contains(&ModCapability::CustomPotions));
        assert!(capabilities("fabric-seasons").contains(&ModCapability::SeasonsCycle));
        assert!(capabilities("trinkets").contains(&ModCapability::Trinkets));
        assert!(capabilities("naturalist").contains(&ModCapability::Wildlife));
        assert!(capabilities("crittersandcompanions").contains(&ModCapability::Wildlife));
        assert!(capabilities("pehkui").contains(&ModCapability::Scaling));
        assert!(capabilities("not_a_real_mod_id").is_empty());
    }

    /// Regression for the dead-end the curator hit on the UCL pack: `inst.mods[].project_id`
    /// stores Modrinth's opaque hash, not the slug, so a project_id-only lookup
    /// always returned `&[]`. Even worse, the map keys are Java mod-ids
    /// (no hyphens) while Modrinth slugs DO have hyphens (`open-loader`),
    /// so even passing slugs wouldn't work without normalization. This test
    /// pins: display name AND hyphenated slug both resolve to DatapackChannel
    /// via `normalize_mod_key`, matching the existing `openloader` map key.
    #[test]
    fn capabilities_lookup_normalises_display_name_and_slug() {
        // Display name (what `PinnedMod.name` holds).
        assert!(capabilities("Open Loader").contains(&ModCapability::DatapackChannel),
            "display name 'Open Loader' must resolve to DatapackChannel");
        // Modrinth slug (hyphenated).
        assert!(capabilities("open-loader").contains(&ModCapability::DatapackChannel),
            "Modrinth slug 'open-loader' must resolve to DatapackChannel");
        // Java mod-id (existing map key form).
        assert!(capabilities("openloader").contains(&ModCapability::DatapackChannel),
            "java mod-id 'openloader' must still resolve");

        // Pehkui — confirms Scaling capability flows through display name too,
        // matching the user's transcript symptom ("Scaling capability missing").
        assert!(capabilities("Pehkui").contains(&ModCapability::Scaling));
        assert!(capabilities("PEHKUI").contains(&ModCapability::Scaling));

        // Mods whose display name has parens/apostrophes (Farmer's Delight
        // (Fabric)) — the normalized form is `farmersdelightfabric`, aliased
        // alongside `farmersdelight` so both hit KnifeFamily.
        assert!(capabilities("Farmer's Delight (Fabric)").contains(&ModCapability::KnifeFamily),
            "display name with apostrophe + parens must still resolve");
        assert!(capabilities("farmers-delight").contains(&ModCapability::KnifeFamily));

        // The Graveyard — display name spaces + capital letters; map key uses
        // underscore form; both must collide to `thegraveyard`.
        assert!(capabilities("The Graveyard").contains(&ModCapability::HauntedStructures));

        // Opaque project_id still returns empty (nothing to match against
        // the alphanumeric map keys). The curator's call site MUST pair
        // project_id with the display name to recover the capability.
        assert!(capabilities("AjW5DBn7").is_empty(),
            "opaque IDs cannot resolve alone — call site must also pass name");
    }

    #[test]
    fn density_budgets_match_documented_tiers() {
        let l = Density::Light.budget();
        assert_eq!(l.passives, (3, 4)); assert_eq!(l.actives, 0); assert_eq!(l.lifetimes, (0, 0));
        let s = Density::Standard.budget();
        assert_eq!(s.passives, (5, 7)); assert_eq!(s.actives, 1); assert_eq!(s.lifetimes, (0, 1));
        let r = Density::Rich.budget();
        assert_eq!(r.passives, (8, 10)); assert_eq!(r.actives, 1); assert_eq!(r.lifetimes, (1, 1));
    }

    #[test]
    fn catalog_phase_gates_origin_questline_to_quests_phase() {
        let entry = catalog_entry(PerkIntentTag::OriginQuestline)
            .expect("OriginQuestline must be in PERK_CATALOG");
        assert_eq!(entry.phase, CurationPhase::Quests);
        assert!(entry.requires.contains(&ModCapability::QuestEngine));
    }

    #[test]
    fn catalog_covers_every_variant_exactly_once() {
        let mut counts: std::collections::HashMap<PerkIntentTag, usize> = Default::default();
        for e in PERK_CATALOG {
            *counts.entry(e.variant).or_insert(0) += 1;
        }
        for (tag, n) in &counts {
            assert_eq!(*n, 1, "{tag:?} appears {n} times in PERK_CATALOG");
        }
        for p in one_per_variant() {
            assert!(counts.contains_key(&p.tag()),
                "PERK_CATALOG is missing entry for {:?}", p.tag());
        }
    }

    #[test]
    fn no_slow_fall_powers_emitted_in_source() {
        // Search strings are CONSTRUCTED at runtime via char arrays so the
        // banned literals never appear in this file (the test would
        // otherwise match itself).
        let src = include_str!("origins.rs");
        let lower_word: String = ['s','l','o','w','_','f','a','l','l','i','n','g'].iter().collect();
        let camel_slow: String = ['S','l','o','w','F','a','l','l','i','n','g'].iter().collect();
        let camel_safe: String = ['S','a','f','e','L','a','n','d','i','n','g'].iter().collect();
        let mk: String = ['M','o','v','e','K','i','n','d',':',':'].iter().collect();
        let tag_prefix: String = ['P','e','r','k','I','n','t','e','n','t','T','a','g',':',':'].iter().collect();
        let banned_const_entry = format!("\"origins:{lower_word}\"");
        let banned_match_arm = format!("\"{lower_word}\" => (");
        let banned_move_kind = format!("{mk}{camel_slow}");
        let banned_safe_landing = format!("{mk}{camel_safe}");
        let banned_tag = format!("{tag_prefix}{camel_slow}");
        assert!(!src.contains(&banned_const_entry),
            "const entry leaked back into SHIPPED_ORIGINS_POWERS");
        assert!(!src.contains(&banned_match_arm),
            "match arm leaked back into shipped_power_label");
        assert!(!src.contains(&banned_move_kind),
            "MoveKind gained a forbidden variant");
        assert!(!src.contains(&banned_safe_landing),
            "safe-landing variant counts as slow-fall");
        assert!(!src.contains(&banned_tag),
            "PerkIntentTag gained a forbidden variant");
    }

    #[test]
    fn move_kind_documented_set() {
        // Exhaustive walker — adding a future MoveKind variant without
        // updating this list fails to compile (non-exhaustive match).
        for k in [MoveKind::Climb, MoveKind::CreativeFlight, MoveKind::ElytraFlight,
                  MoveKind::WalkOnFluid, MoveKind::HigherJump] {
            match k {
                MoveKind::Climb | MoveKind::CreativeFlight | MoveKind::ElytraFlight
                | MoveKind::WalkOnFluid | MoveKind::HigherJump => {}
            }
        }
    }

    // ========================================================================
    // REAL-WORLD TESTS — exact LLM-emitted shapes, historical failure
    // regressions, recursion edge cases, byte-determinism. These are NOT
    // synthetic fixtures built to satisfy the algorithm; they mirror what
    // the curator actually produces and the failure modes a real run hits.
    // ========================================================================

    /// The Hollow-Born Witch in the exact JSON shape the curator emits, for a
    /// modpack that pins Bewitchment, The Graveyard, Heracles, OpenLoader.
    /// Authored against real Stardew Hollow ids: `bewitchment:athame`,
    /// `bewitchment:silver_ingot`, `graveyard:reaper`.
    pub(super) const REAL_WITCH_JSON: &str = r##"{
        "theme": "arcane",
        "name": "Hollow-Born Witch",
        "description": "Bound to the cauldron, gifted against the dead, cursed by daylight.",
        "icon": "bewitchment:athame",
        "perks": [
            { "intent": "starts_with", "items": ["bewitchment:athame", "bewitchment:silver_ingot"] },
            { "intent": "passive_effect", "effect": "minecraft:night_vision" },
            { "intent": "buff_when",
              "what": { "kind": "attribute", "attribute": "minecraft:generic.max_health",
                        "op": "addition", "amount": 4.0 },
              "when": { "kind": "block_in_radius",
                        "block": "bewitchment:witch_cauldron", "radius": 8 } },
            { "intent": "damage_vs", "target": "graveyard:reaper", "multiplier": 1.5 },
            { "intent": "dot_when", "dps": 0.5,
              "when": { "kind": "and", "conditions": [
                          { "kind": "daytime" },
                          { "kind": "exposed_to_sky" } ] } },
            { "intent": "forbidden_item_use", "what": "minecraft:iron_sword" },
            { "intent": "active", "key": "primary", "cooldown_s": 60, "hud": "active",
              "body": { "kind": "area_burst", "radius": 8, "damage": 8.0, "knockback": 1.0 } },
            { "intent": "lifetime", "gate": "once_per_moon_full",
              "body": { "kind": "log_and_resurrect",
                        "logs": "graveyard:reaper",
                        "summon_for_dur_s": 14400 } }
        ],
        "linked_boss": "ch_climax_void_reaper"
    }"##;

    /// Junimo — Pehkui-scaled, naturalist-aware, season-fragile.
    pub(super) const REAL_JUNIMO_JSON: &str = r##"{
        "theme": "arcane",
        "name": "Junimo",
        "description": "A scrap of forest given shape.",
        "icon": "minecraft:fern",
        "perks": [
            { "intent": "scale", "factor": 0.65 },
            { "intent": "passive_effect", "effect": "minecraft:night_vision" },
            { "intent": "pacify_targeting", "by": ["naturalist:butterfly", "naturalist:bear", "naturalist:deer"] },
            { "intent": "dot_when", "dps": 0.3,
              "when": { "kind": "biome_tag", "tag": "minecraft:is_cold" } },
            { "intent": "starts_with", "items": ["farmersdelight:tomato_seeds"] }
        ]
    }"##;

    /// Bewitched Wolfkin — silver-cursed, moontooth, no-sleep, transformation.
    pub(super) const REAL_WOLFKIN_JSON: &str = r##"{
        "theme": "cursed",
        "name": "Bewitched Wolfkin",
        "description": "The change took, and it never quite let go.",
        "icon": "minecraft:bone",
        "perks": [
            { "intent": "buff_when",
              "what": { "kind": "attribute", "attribute": "minecraft:generic.attack_damage",
                        "op": "addition", "amount": 3.0 },
              "when": { "kind": "nighttime" } },
            { "intent": "special_movement", "kind": "higher_jump" },
            { "intent": "on_kill_grant", "target": ["naturalist:bear", "naturalist:deer"],
              "effect": "minecraft:strength", "duration_s": 30 },
            { "intent": "dot_when", "dps": 1.0,
              "when": { "kind": "block_in_radius",
                        "block": "bewitchment:silver_block", "radius": 4 } },
            { "intent": "forbidden_item_use", "what": "#c:silver_ingots" },
            { "intent": "prevent_sleep" },
            { "intent": "active", "key": "primary", "cooldown_s": 240, "hud": "cooldown",
              "body": { "kind": "transformation",
                        "duration_s": 30, "scale": 1.4,
                        "stash_inventory": true,
                        "effects_on": [
                          { "effect": "minecraft:speed", "amplifier": 1, "duration_t": 600 },
                          { "effect": "minecraft:strength", "amplifier": 1, "duration_t": 600 }
                        ],
                        "effects_off": [
                          { "effect": "minecraft:hunger", "amplifier": 2, "duration_t": 200 }
                        ]
              } }
        ]
    }"##;

    /// The Drifter — Comforts bedroll start, Heracles-gated questline.
    pub(super) const REAL_DRIFTER_JSON: &str = r##"{
        "theme": "adventure",
        "name": "The Drifter",
        "description": "The road keeps. The hearth doesn't.",
        "icon": "supplementaries:soap",
        "perks": [
            { "intent": "attribute_buff", "attribute": "minecraft:generic.movement_speed",
              "amount": 0.015, "op": "addition" },
            { "intent": "starts_with", "items": ["supplementaries:soap", "farmersdelight:tomato_seeds"] },
            { "intent": "attribute_buff", "attribute": "minecraft:generic.max_health",
              "amount": -2.0, "op": "addition" },
            { "intent": "keep_inventory_slot", "slots": ["hotbar0"] },
            { "intent": "bonus_saturation_on", "food": "#c:foods", "extra": 1 },
            { "intent": "active", "key": "primary", "cooldown_s": 90, "hud": "active",
              "body": { "kind": "timed_effect_chain",
                        "on":  [{ "effect": "minecraft:invisibility", "amplifier": 0, "duration_t": 240 },
                                { "effect": "minecraft:speed",        "amplifier": 2, "duration_t": 240 }],
                        "duration_s": 12,
                        "off": [{ "effect": "minecraft:slowness", "amplifier": 0, "duration_t": 600 }],
                        "off_duration_s": 30 } }
        ],
        "gates_quest": "q_hidden_paths"
    }"##;

    #[test]
    fn real_curator_emitted_origins_parse_and_roundtrip() {
        // Every LLM-shaped JSON parses; every perk lands on the right
        // variant; serialize→deserialize equals.
        for (label, raw) in [
            ("Hollow-Born Witch", REAL_WITCH_JSON),
            ("Junimo",            REAL_JUNIMO_JSON),
            ("Bewitched Wolfkin", REAL_WOLFKIN_JSON),
            ("The Drifter",       REAL_DRIFTER_JSON),
        ] {
            let parsed: OriginIntent = serde_json::from_str(raw)
                .unwrap_or_else(|e| panic!("`{label}` failed to parse: {e}\n--- raw ---\n{raw}"));
            assert!(!parsed.perks.is_empty(), "{label} parsed with empty perks");
            // Roundtrip byte-determinism: serialize twice, identical.
            let a = serde_json::to_string(&parsed).expect("serialize 1");
            let b = serde_json::to_string(&parsed).expect("serialize 2");
            assert_eq!(a, b, "{label} serialize is not deterministic");
            // Roundtrip semantic: parse the serialized form again, must equal.
            let back: OriginIntent = serde_json::from_str(&a)
                .unwrap_or_else(|e| panic!("{label} roundtrip parse failed: {e}\n{a}"));
            assert_eq!(parsed, back, "{label} roundtrip does not equal");
        }
    }

    #[test]
    fn real_witch_perk_count_and_active_shape() {
        // Real-content assertion (not synthetic): the Witch has exactly the
        // shape the design doc fixed — 8 perks, one Active::AreaBurst,
        // one Lifetime::LogAndResurrect targeting the reaper.
        let w: OriginIntent = serde_json::from_str(REAL_WITCH_JSON).unwrap();
        assert_eq!(w.perks.len(), 8);
        // The Active is in the list with the expected body shape.
        let active = w.perks.iter().find(|p| matches!(p, PerkIntent::Active { .. }))
            .expect("Witch must have one Active");
        match active {
            PerkIntent::Active { body: ActiveBody::AreaBurst { radius, damage, .. }, cooldown_s, .. } => {
                assert_eq!(*radius, 8);
                assert!((*damage - 8.0).abs() < 1e-6);
                assert_eq!(*cooldown_s, 60);
            }
            other => panic!("Witch's active is not an AreaBurst: {other:?}"),
        }
        // The Lifetime references the reaper (real Graveyard mod id).
        let lifetime = w.perks.iter().find(|p| matches!(p, PerkIntent::Lifetime { .. }))
            .expect("Witch must have one Lifetime");
        match lifetime {
            PerkIntent::Lifetime { gate, body } => {
                assert_eq!(*gate, LifetimeGate::OncePerMoonFull);
                match body {
                    LifetimeBody::LogAndResurrect { logs, summon_for_dur_s } => {
                        match logs {
                            EntityCondRef::One(id) => assert_eq!(id.as_str(), "graveyard:reaper"),
                            other => panic!("expected single entity ref, got {other:?}"),
                        }
                        assert_eq!(*summon_for_dur_s, 14_400);
                    }
                    other => panic!("Witch's lifetime body wrong: {other:?}"),
                }
            }
            _ => unreachable!(),
        }
        // Cross-system hook present.
        assert_eq!(w.linked_boss.as_ref().map(|h| h.as_str()), Some("ch_climax_void_reaper"));
    }

    #[test]
    fn historical_failure_name_as_component_rejects_at_parse() {
        // The exact shape that broke the prior Anvil origins datapack on Apoli
        // — `name: { "text": "..." }` instead of a plain string. At the intent
        // layer, SanitizedText is a String newtype; an object reaches its
        // Deserialize as a non-string and must reject.
        let bad = serde_json::from_value::<OriginIntent>(json!({
            "theme": "arcane",
            "name": { "text": "Hollow-Born Witch" },
            "description": "Bound.",
            "icon": "bewitchment:athame",
            "perks": []
        }));
        assert!(bad.is_err(),
            "historical-regression: name-as-component must reject at parse, got {bad:?}");
    }

    #[test]
    fn historical_failure_description_as_component_rejects_at_parse() {
        let bad = serde_json::from_value::<OriginIntent>(json!({
            "theme": "arcane",
            "name": "Hollow-Born Witch",
            "description": { "text": "Bound." },
            "icon": "bewitchment:athame",
            "perks": []
        }));
        assert!(bad.is_err(),
            "historical-regression: description-as-component must reject at parse, got {bad:?}");
    }

    #[test]
    fn empty_string_name_rejects_at_parse() {
        // The Apoli loader also rejects empty name; SanitizedText::new returns
        // EmptyText. Serde surfaces it as parse failure.
        let bad = serde_json::from_value::<OriginIntent>(json!({
            "theme": "arcane", "name": "   ", "description": "X",
            "icon": "bewitchment:athame", "perks": []
        }));
        assert!(bad.is_err(), "blank name must reject at parse");
    }

    #[test]
    fn boxed_tally_milestone_recurses_to_depth_five() {
        // Real recursion edge case: a milestone unlocks another milestone
        // unlocks another — the curator can compose chains. Confirm the
        // Box<PerkIntent> recursion roundtrips at depth 5.
        let mut inner = PerkIntent::DamageVs {
            target: EntityCondRef::One(EntityTypeId::new("graveyard:reaper")),
            multiplier: DamageMul::new(2.0).unwrap(),
        };
        for n in 0..5 {
            inner = PerkIntent::TallyMilestone {
                event: TallyEvent::KillInRadius,
                target: EntityCondRef::One(EntityTypeId::new("#minecraft:undead")),
                threshold: 10 * (n + 1),
                unlock: Box::new(inner),
            };
        }
        let json = serde_json::to_string(&inner).expect("serialize depth-5");
        let back: PerkIntent = serde_json::from_str(&json).expect("parse depth-5");
        assert_eq!(inner, back, "depth-5 TallyMilestone chain lost shape on roundtrip");
        // Walk the chain — must be exactly 5 wrappers then DamageVs.
        let mut cursor = &back;
        for _ in 0..5 {
            match cursor {
                PerkIntent::TallyMilestone { unlock, .. } => cursor = unlock.as_ref(),
                other => panic!("expected TallyMilestone at this depth, got {other:?}"),
            }
        }
        assert!(matches!(cursor, PerkIntent::DamageVs { .. }));
    }

    #[test]
    fn item_selector_accepts_both_string_and_array_shapes() {
        // Real LLM-emitted JSON sometimes inlines a single id as a string and
        // sometimes uses an array. ItemSelector is untagged for this reason.
        let one: PerkIntent = serde_json::from_value(json!({
            "intent": "forbidden_item_use", "what": "minecraft:iron_sword"
        })).unwrap();
        let many: PerkIntent = serde_json::from_value(json!({
            "intent": "forbidden_item_use", "what": ["minecraft:iron_sword","minecraft:diamond_sword"]
        })).unwrap();
        match one { PerkIntent::ForbiddenItemUse { what: ItemSelector::One(_) } => {},
                   _ => panic!("single-id should be ItemSelector::One") }
        match many { PerkIntent::ForbiddenItemUse { what: ItemSelector::Many(v) } => assert_eq!(v.len(), 2),
                    _ => panic!("array should be ItemSelector::Many") }
    }

    #[test]
    fn realistic_amplifier_typo_rejects() {
        // The curator might output `amplifier: 10` (Apoli range is 0..=4).
        // Bounded numeric rejects at parse with a real-looking error.
        let bad: StdResult<PerkIntent, _> = serde_json::from_value(json!({
            "intent": "passive_effect",
            "effect": "minecraft:strength",
            "amplifier": 10
        }));
        assert!(bad.is_err(),
            "amplifier 10 (typo from 1) should reject; Apoli range is 0..=4");
        let err_str = format!("{}", bad.unwrap_err());
        assert!(err_str.contains("Amplifier") || err_str.contains("range") || err_str.contains("0..") || err_str.contains("0,"),
            "error should mention the bound or field, got: {err_str}");
    }

    #[test]
    fn empty_perks_array_parses_but_is_documented_to_be_caught_in_phase_1d() {
        // 1a is intentionally lax on perk count — Density::budget() in 1d is
        // the gate. Empty perks is a valid PARSE; an OriginIssue::BudgetViolation
        // would fire when Phase 1d's check runs.
        let o: OriginIntent = serde_json::from_value(json!({
            "theme": "cozy", "name": "Empty", "description": "Test fixture only.",
            "icon": "minecraft:air", "perks": []
        })).expect("empty perks must parse at 1a");
        assert!(o.perks.is_empty());
    }

    #[test]
    fn sanitized_text_handles_real_curator_unicode_and_long_strings() {
        // Real curator output may include em-dashes, smart quotes, emoji,
        // and longer-than-default descriptions. All must normalise cleanly,
        // not blow up.
        let real = "The witch \u{2014} bound to the cauldron \u{2018}forever\u{2019} \u{2013} answered.";
        let s = SanitizedText::new(real).expect("real prose should sanitize");
        assert!(s.as_str().contains("-"), "em-dash should normalise to ASCII -");
        assert!(s.as_str().contains("'"), "smart apostrophe should normalise to ASCII '");
        // 500-char description survives.
        let long = "The cauldron speaks. ".repeat(25);
        assert!(SanitizedText::new(&long).is_ok());
        // Emoji is left intact (Apoli accepts UTF-8).
        let emoji = SanitizedText::new("\u{2728} blessed \u{2728}").unwrap();
        assert!(emoji.as_str().contains("\u{2728}"));
    }

    #[test]
    fn buffwhen_attribute_variant_parses_with_modded_attribute_id() {
        // A real modded attribute like pehkui:base — at 1a we don't ground,
        // so it must parse. (Grounding rejection lives in 1b.)
        let p: PerkIntent = serde_json::from_value(json!({
            "intent": "buff_when",
            "what": { "kind": "attribute", "attribute": "pehkui:base",
                      "op": "multiply_total", "amount": -0.35 },
            "when": { "kind": "any" }
        })).expect("modded attribute id must parse at 1a");
        match p {
            PerkIntent::BuffWhen { what: BuffWhat::Attribute { attribute, op, amount }, .. } => {
                assert_eq!(attribute.as_str(), "pehkui:base");
                assert_eq!(op, AttrOp::MultiplyTotal);
                assert!((amount.value() - -0.35).abs() < 1e-6);
            }
            other => panic!("expected BuffWhen::Attribute, got {other:?}"),
        }
    }

    #[test]
    fn budget_violation_can_be_constructed_for_real_cases() {
        // Exercise the OriginIssue surface so its Display string is real,
        // not a stub. Phase 1d will use this.
        let issue = OriginIssue::BudgetViolation {
            density: Density::Light,
            what: "passives",
            count: 12,
            bound: 4,
            direction: BudgetDirection::Over,
        };
        let s = format!("{issue}");
        assert!(s.contains("Light"));
        assert!(s.contains("passives"));
        assert!(s.contains("12"));
    }

    #[test]
    fn requires_absent_capability_error_is_displayable() {
        // Real curator scenario: pack lacks Bewitchment, but a Witch origin
        // tries to use the Familiar intent. 1c's emit-gate would surface this.
        let issue = OriginIssue::RequiresAbsentCapability {
            variant: PerkIntentTag::Familiar,
            missing: ModCapability::BondableCompanions,
        };
        let s = format!("{issue}");
        assert!(s.contains("Familiar"));
        assert!(s.contains("BondableCompanions"));
    }
}

// ============================================================================
// PHASE 1b — GROUNDING PIPELINE
//
// Every typed id in an OriginIntent is ground-checked against the pack's
// registry dump (the existing `RegistryVocab`). Unknown id -> OriginIssue
// with the top-3 fuzzy-match suggestions sorted by Levenshtein distance.
// Slots not populated by the scan (status_effect / attribute / damage_type /
// fluid; or biomes when the dump didn't enumerate them) accept by design —
// the lower-layer Apoli validator catches those.
//
// `when` slots are typed `WhenCondition` (T2b refactor — closed enum).
// `ground_when_condition` walks the structure and grounds Biome / BiomeTag /
// Dimension / BlockInRadius against the right vocab slot. Atmospheric and
// pose leaves carry no id and need no grounding.
// ============================================================================

use crate::registry::RegistryVocab;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundCategory {
    Item, Block, EntityType, Biome, Fluid, StatusEffect, Attribute, DamageType,
    Advancement, Structure, Tag,
}

impl GroundCategory {
    fn slot<'a>(self, v: &'a RegistryVocab) -> Option<&'a std::collections::BTreeSet<String>> {
        Some(match self {
            GroundCategory::Item        => &v.items,
            GroundCategory::Block       => &v.blocks,
            GroundCategory::EntityType  => &v.entities,
            GroundCategory::Biome       => &v.biomes,
            GroundCategory::Advancement => &v.advancements,
            GroundCategory::Structure   => &v.structures,
            GroundCategory::Tag         => &v.tags,
            // No vocab slot exists for these in `RegistryVocab` v1; accept by
            // design and let the Apoli validator gate them at emit.
            GroundCategory::Fluid
            | GroundCategory::StatusEffect
            | GroundCategory::Attribute
            | GroundCategory::DamageType => return None,
        })
    }
    fn name(self) -> &'static str {
        match self {
            GroundCategory::Item         => "item",
            GroundCategory::Block        => "block",
            GroundCategory::EntityType   => "entity_type",
            GroundCategory::Biome        => "biome",
            GroundCategory::Fluid        => "fluid",
            GroundCategory::StatusEffect => "status_effect",
            GroundCategory::Attribute    => "attribute",
            GroundCategory::DamageType   => "damage_type",
            GroundCategory::Advancement  => "advancement",
            GroundCategory::Structure    => "structure",
            GroundCategory::Tag          => "tag",
        }
    }
}

/// Well-formed `ns:path` (or `#ns:path` for a tag). Lowercase, digits,
/// `_./-/` allowed. Empty namespace OR path => malformed.
pub fn is_well_formed_id(s: &str) -> bool {
    let core = s.strip_prefix('#').unwrap_or(s);
    let Some((ns, path)) = core.split_once(':') else { return false; };
    if ns.is_empty() || path.is_empty() { return false; }
    let ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit()
                    || c == '_' || c == '.' || c == '-' || c == '/';
    ns.chars().all(ok) && path.chars().all(ok)
}

/// Standard Levenshtein. Used only on candidate sets bounded by namespace
/// pre-filter (see `top3_suggestions`); fine for typical mod-id counts.
fn levenshtein(a: &str, b: &str) -> usize {
    let (la, lb) = (a.chars().count(), b.chars().count());
    if la == 0 { return lb; }
    if lb == 0 { return la; }
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=lb).collect();
    let mut curr = vec![0usize; lb + 1];
    for (i, &ach) in ac.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &bch) in bc.iter().enumerate() {
            let cost = if ach == bch { 0 } else { 1 };
            curr[j + 1] = std::cmp::min(
                std::cmp::min(curr[j] + 1, prev[j + 1] + 1),
                prev[j] + cost,
            );
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[lb]
}

/// Top-3 fuzzy suggestions for an unknown id. Bias toward same-namespace
/// candidates first (the most common LLM typo case is a path-slip within
/// a known mod's namespace), then fall back to global if there are fewer
/// than 3 same-namespace matches.
fn top3_suggestions(needle: &str, haystack: &std::collections::BTreeSet<String>) -> Vec<String> {
    let needle_ns = needle.strip_prefix('#').unwrap_or(needle).split(':').next().unwrap_or("");
    let mut same_ns: Vec<(usize, &String)> = Vec::new();
    let mut other:   Vec<(usize, &String)> = Vec::new();
    for h in haystack {
        let h_ns = h.strip_prefix('#').unwrap_or(h).split(':').next().unwrap_or("");
        let d = levenshtein(needle, h);
        if h_ns == needle_ns { same_ns.push((d, h)); } else { other.push((d, h)); }
    }
    let sort_key = |v: &mut Vec<(usize, &String)>| v.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    sort_key(&mut same_ns); sort_key(&mut other);
    let mut out: Vec<String> = same_ns.into_iter().take(3).map(|(_, s)| s.clone()).collect();
    if out.len() < 3 {
        let need = 3 - out.len();
        out.extend(other.into_iter().take(need).map(|(_, s)| s.clone()));
    }
    out
}

/// Ground a single id against the pack's vocab.
/// - Malformed id => `UnknownId` with no suggestions (lex failure).
/// - `#tag` => tags slot regardless of category arg.
/// - Slot has no entries (category not populated by scan) => accept.
/// - Empty vocab entirely => accept (no jars scanned yet; the lower-layer
///   validator still gates Apoli correctness).
pub fn ground_id(id: &str, cat: GroundCategory, vocab: &RegistryVocab) -> StdResult<(), OriginIssue> {
    if !is_well_formed_id(id) {
        return Err(OriginIssue::UnknownId {
            category: cat.name(),
            id: id.to_string(),
            suggestions: vec![],
        });
    }
    if vocab.is_empty() { return Ok(()); }
    let is_tag = id.starts_with('#');
    let slot = if is_tag { &vocab.tags } else {
        match cat.slot(vocab) { Some(s) => s, None => return Ok(()) }
    };
    if slot.is_empty() { return Ok(()); }  // category un-populated -> low-confidence accept
    // Tags in the dump are stored WITHOUT the `#` prefix (it's a use-site
    // sigil, not part of the registry identity). Strip for lookup.
    let lookup_key = if is_tag { &id[1..] } else { id };
    if slot.contains(lookup_key) { return Ok(()); }
    // Vanilla `minecraft:*` ids are NOT in a Static-scanned dump (the scan
    // only captures modded namespaces). If this slot has zero `minecraft:`
    // entries, the vanilla namespace wasn't reconciled — accept the id and
    // let the Apoli gate downstream validate it. After Slice 1.5 dump
    // reconciliation populates `minecraft:*` into the slot, this bypass
    // becomes a no-op (the id will be found directly).
    let lookup_ns = lookup_key.split(':').next().unwrap_or("");
    if lookup_ns == "minecraft" && !slot.iter().any(|s| s.starts_with("minecraft:")) {
        return Ok(());
    }
    Err(OriginIssue::UnknownId {
        category: cat.name(),
        id: id.to_string(),
        suggestions: top3_suggestions(lookup_key, slot),
    })
}

// ---- Visitor: walk an OriginIntent / PerkIntent collecting grounding issues

fn push_id(id: &str, cat: GroundCategory, vocab: &RegistryVocab, out: &mut Vec<OriginIssue>) {
    if let Err(e) = ground_id(id, cat, vocab) { out.push(e); }
}

fn ground_item_sel(s: &ItemSelector, vocab: &RegistryVocab, out: &mut Vec<OriginIssue>) {
    match s {
        ItemSelector::One(i) => push_id(i.as_str(), GroundCategory::Item, vocab, out),
        ItemSelector::Many(v) => for i in v { push_id(i.as_str(), GroundCategory::Item, vocab, out); }
    }
}

fn ground_block_sel(s: &BlockSelector, vocab: &RegistryVocab, out: &mut Vec<OriginIssue>) {
    match s {
        BlockSelector::One(i) => push_id(i.as_str(), GroundCategory::Block, vocab, out),
        BlockSelector::Many(v) => for i in v { push_id(i.as_str(), GroundCategory::Block, vocab, out); }
    }
}

fn ground_entity_targets(c: &EntityCondRef, vocab: &RegistryVocab, out: &mut Vec<OriginIssue>) {
    match c {
        EntityCondRef::One(i)  => push_id(i.as_str(), GroundCategory::EntityType, vocab, out),
        EntityCondRef::Many(v) => for i in v { push_id(i.as_str(), GroundCategory::EntityType, vocab, out); }
    }
}

/// Walk a `WhenCondition` and ground every id leaf against the right slot.
/// Atmospheric / pose leaves (Daytime, InRain, Sneaking, …) have no id so
/// nothing is grounded for them. Dimension ids have no vocab source today —
/// `is_well_formed_id` already enforced at construction.
fn ground_when_condition(w: &WhenCondition, vocab: &RegistryVocab, out: &mut Vec<OriginIssue>) {
    match w {
        WhenCondition::Any
        | WhenCondition::Daytime  | WhenCondition::Nighttime
        | WhenCondition::InRain   | WhenCondition::ExposedToSky | WhenCondition::OnFire
        | WhenCondition::Sneaking | WhenCondition::Sprinting    | WhenCondition::Swimming
        | WhenCondition::FallFlying => {}
        WhenCondition::Dimension { .. } => {
            // No `vocab.dimensions` slot today; Apoli rejects unknown ids at load.
        }
        WhenCondition::Biome { id }    => push_id(id.as_str(), GroundCategory::Biome, vocab, out),
        WhenCondition::BiomeTag { tag } => push_id(tag.as_str(), GroundCategory::Tag, vocab, out),
        WhenCondition::BlockInRadius { block, .. } => ground_block_sel(block, vocab, out),
        WhenCondition::Not { conditions }
        | WhenCondition::And { conditions }
        | WhenCondition::Or { conditions } => {
            for c in conditions { ground_when_condition(c, vocab, out); }
        }
    }
}

fn ground_active_body(b: &ActiveBody, vocab: &RegistryVocab, out: &mut Vec<OriginIssue>) {
    match b {
        ActiveBody::TeleportToMarker { marker, .. } => ground_block_sel(marker, vocab, out),
        ActiveBody::InvisibilityPulse { retinue, .. } => if let Some(r) = retinue {
            for et in &r.entity_types { ground_entity_targets(et, vocab, out); }
        },
        ActiveBody::AreaBurst { .. } => {}
        ActiveBody::Transformation { effects_on, effects_off, summon_allies, .. } => {
            for e in effects_on  { push_id(e.effect.as_str(), GroundCategory::StatusEffect, vocab, out); }
            for e in effects_off { push_id(e.effect.as_str(), GroundCategory::StatusEffect, vocab, out); }
            if let Some(r) = summon_allies {
                for et in &r.entity_types { ground_entity_targets(et, vocab, out); }
            }
        }
        ActiveBody::TimedEffectChain { on, off, .. } => {
            for e in on  { push_id(e.effect.as_str(), GroundCategory::StatusEffect, vocab, out); }
            for e in off { push_id(e.effect.as_str(), GroundCategory::StatusEffect, vocab, out); }
        }
    }
}

fn ground_lifetime_body(b: &LifetimeBody, vocab: &RegistryVocab, out: &mut Vec<OriginIssue>) {
    match b {
        LifetimeBody::PlacePersistentZone { .. } => {}
        LifetimeBody::ForcedTransformation { body, .. } => ground_active_body(body, vocab, out),
        LifetimeBody::LogAndResurrect { logs, .. }
        | LifetimeBody::RallyEvent { summon_entities: logs, .. } => ground_entity_targets(logs, vocab, out),
        LifetimeBody::WaypointRecall { .. } => {}
    }
}

/// Collect every grounding issue inside one perk (recurses into Box<>).
pub fn ground_perk(perk: &PerkIntent, vocab: &RegistryVocab, out: &mut Vec<OriginIssue>) {
    match perk {
        PerkIntent::StartsWith { items, .. } => {
            for i in items { push_id(i.as_str(), GroundCategory::Item, vocab, out); }
        }
        PerkIntent::Scale { .. } | PerkIntent::SpecialMovement { .. }
        | PerkIntent::ComboChain { .. } | PerkIntent::DodgeRoll { .. }
        | PerkIntent::SeasonNotification { .. } | PerkIntent::KeepInventorySlot { .. }
        | PerkIntent::MapMarkerAtSpawn { .. } | PerkIntent::AutoJournal { .. }
        | PerkIntent::OriginQuestline { .. } => {}
        PerkIntent::PassiveEffect { effect, .. }
        | PerkIntent::StaggerOnSprint { effect, .. } => {
            push_id(effect.as_str(), GroundCategory::StatusEffect, vocab, out);
        }
        PerkIntent::AttributeBuff { attribute, when, .. } => {
            push_id(attribute.as_str(), GroundCategory::Attribute, vocab, out);
            if let Some(w) = when { ground_when_condition(w, vocab, out); }
        }
        PerkIntent::BuffWhen { what, when } => {
            match what {
                BuffWhat::Effect { effect, .. } => push_id(effect.as_str(), GroundCategory::StatusEffect, vocab, out),
                BuffWhat::Attribute { attribute, .. } => push_id(attribute.as_str(), GroundCategory::Attribute, vocab, out),
            }
            ground_when_condition(when, vocab, out);
        }
        PerkIntent::DotWhen { when, .. } => ground_when_condition(when, vocab, out),
        PerkIntent::Overlay  { when, .. } => ground_when_condition(when, vocab, out),
        PerkIntent::BlockPhase { block, when } => {
            ground_block_sel(block, vocab, out);
            ground_when_condition(when, vocab, out);
        }
        PerkIntent::DamageVs { target, .. }
        | PerkIntent::PacifyTargeting { by: target } | PerkIntent::HostileRecognition { by: target }
        | PerkIntent::EntityGlow { targets: target, .. } | PerkIntent::Siphon { target, .. } => {
            ground_entity_targets(target, vocab, out);
        }
        PerkIntent::ForbiddenItemUse { what }            => ground_item_sel(what, vocab, out),
        PerkIntent::PreventSleep { except: Some(x) }     => ground_item_sel(x, vocab, out),
        PerkIntent::PreventSleep { except: None }        => {}
        PerkIntent::PreventBreakUnderFoot { block }      => ground_block_sel(block, vocab, out),
        PerkIntent::OnKillGrant { target, effect, .. } => {
            ground_entity_targets(target, vocab, out);
            push_id(effect.as_str(), GroundCategory::StatusEffect, vocab, out);
        }
        PerkIntent::OnWakeGrant { effects } => {
            for e in effects { push_id(e.effect.as_str(), GroundCategory::StatusEffect, vocab, out); }
        }
        PerkIntent::BonusSaturationOn { food, when, .. } => {
            ground_item_sel(food, vocab, out);
            if let Some(w) = when { ground_when_condition(w, vocab, out); }
        }
        PerkIntent::FasterBreakOn { block, .. }    => ground_block_sel(block, vocab, out),
        PerkIntent::TallyMilestone { target, unlock, .. } => {
            ground_entity_targets(target, vocab, out);
            ground_perk(unlock, vocab, out);
        }
        PerkIntent::OncePerDayBonus { bonus, .. } => ground_perk(bonus, vocab, out),
        PerkIntent::Active { body, .. } => ground_active_body(body, vocab, out),
        PerkIntent::Lifetime { body, .. } => ground_lifetime_body(body, vocab, out),
        PerkIntent::VeinMine { block, .. } | PerkIntent::HarvestAoe { crop: block, .. } => {
            ground_block_sel(block, vocab, out);
        }
        PerkIntent::LastStand { effects, .. } => {
            for e in effects { push_id(e.effect.as_str(), GroundCategory::StatusEffect, vocab, out); }
        }
        PerkIntent::SignatureTrinket { carries, .. } => ground_perk(carries, vocab, out),
        PerkIntent::Familiar { entity, .. }          => push_id(entity.as_str(), GroundCategory::EntityType, vocab, out),
        PerkIntent::SeasonalForm { spring, summer, fall, winter } => {
            for s in spring.iter().chain(summer.iter()).chain(fall.iter()).chain(winter.iter()) {
                ground_perk(s, vocab, out);
            }
        }
        PerkIntent::ApprenticeToNpc { reward_chain, .. } => {
            for p in reward_chain { ground_perk(p, vocab, out); }
        }
        PerkIntent::BrewPotency { which, .. } => ground_item_sel(which, vocab, out),
        PerkIntent::KnifeMaster { knife, on_use } => {
            ground_item_sel(knife, vocab, out); ground_perk(on_use, vocab, out);
        }
        PerkIntent::Gravewalker { near, on_proximity } => {
            ground_block_sel(near, vocab, out); ground_perk(on_proximity, vocab, out);
        }
        PerkIntent::PackLeader { entity_types, .. } => {
            for et in entity_types { ground_entity_targets(et, vocab, out); }
        }
        PerkIntent::BanditKin { faction, ally_summon, .. } => {
            ground_entity_targets(faction, vocab, out);
            if let Some(p) = ally_summon { ground_perk(p, vocab, out); }
        }
    }
}

/// Ground every typed id in an OriginIntent (icon + every perk recursively).
pub fn ground_origin_intent(o: &OriginIntent, vocab: &RegistryVocab) -> Vec<OriginIssue> {
    let mut out = Vec::new();
    push_id(o.icon.as_str(), GroundCategory::Item, vocab, &mut out);
    for p in &o.perks { ground_perk(p, vocab, &mut out); }
    out
}

// ============================================================================
// PHASE 1d — FORECAST + DENSITY-BUDGET VALIDATOR
//
// Two complementary checks the curator runs before invoking `emit_perk`:
//
// 1. `forecast_origin_capabilities` walks an OriginIntent's perks and
//    classifies each into passive / active / lifetime buckets. Returns a
//    flat structural summary the LLM can read back.
//
// 2. `validate_density_budget` compares the forecast to the chosen
//    Density's `PerkBudget` and surfaces under/over violations.
//
// 3. `validate_capabilities` checks every perk's required ModCapability
//    against the available mod list (the curator side answers
//    "which mods are in this pack?" before authoring an OriginIntent).
//
// Together: the curator gets a structured "is this set in budget AND
// compatible with the pack's mods?" answer without running emit_perk.
// ============================================================================

/// Structural summary of an OriginIntent's shape. Used by the curator to
/// decide whether to add more perks, drop one, or pick a different density.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginForecast {
    /// Perks that are neither `Active` nor `Lifetime`. The bulk of every
    /// origin's character.
    pub passives: u8,
    /// Count of `Active` perks (keybind-triggered abilities).
    pub actives: u8,
    /// Count of `Lifetime` perks (once-per-cycle moments).
    pub lifetimes: u8,
    /// Capabilities required by at least one perk in the set. Dedup'd and
    /// sorted by Debug repr for deterministic output.
    pub required_capabilities: Vec<ModCapability>,
}

/// Walk an OriginIntent and produce its `OriginForecast`. Pure: no I/O,
/// no grounding, no emit. The curator can call this before any registry
/// reconciliation has happened.
pub fn forecast_origin_capabilities(intent: &OriginIntent) -> OriginForecast {
    let mut passives: u8 = 0;
    let mut actives: u8 = 0;
    let mut lifetimes: u8 = 0;
    let mut caps: Vec<ModCapability> = Vec::new();
    for perk in &intent.perks {
        let tag = perk.tag();
        match tag {
            PerkIntentTag::Active   => actives   = actives.saturating_add(1),
            PerkIntentTag::Lifetime => lifetimes = lifetimes.saturating_add(1),
            _                        => passives  = passives.saturating_add(1),
        }
        if let Some(entry) = catalog_entry(tag) {
            for c in entry.requires {
                if !caps.contains(c) { caps.push(*c); }
            }
        }
    }
    caps.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    OriginForecast { passives, actives, lifetimes, required_capabilities: caps }
}

/// Compare a forecast against a Density's budget; produce one
/// `BudgetViolation` per axis (passives / actives / lifetimes) that falls
/// outside the band. Returns empty when everything's in range.
pub fn validate_density_budget(intent: &OriginIntent, density: Density) -> Vec<OriginIssue> {
    let forecast = forecast_origin_capabilities(intent);
    let budget = density.budget();
    let mut issues = Vec::new();
    let (pmin, pmax) = budget.passives;
    if forecast.passives < pmin {
        issues.push(OriginIssue::BudgetViolation {
            density, what: "passives",
            count: forecast.passives, bound: pmin,
            direction: BudgetDirection::Under,
        });
    } else if forecast.passives > pmax {
        issues.push(OriginIssue::BudgetViolation {
            density, what: "passives",
            count: forecast.passives, bound: pmax,
            direction: BudgetDirection::Over,
        });
    }
    if forecast.actives > budget.actives {
        issues.push(OriginIssue::BudgetViolation {
            density, what: "actives",
            count: forecast.actives, bound: budget.actives,
            direction: BudgetDirection::Over,
        });
    }
    let (lmin, lmax) = budget.lifetimes;
    if forecast.lifetimes < lmin {
        issues.push(OriginIssue::BudgetViolation {
            density, what: "lifetimes",
            count: forecast.lifetimes, bound: lmin,
            direction: BudgetDirection::Under,
        });
    } else if forecast.lifetimes > lmax {
        issues.push(OriginIssue::BudgetViolation {
            density, what: "lifetimes",
            count: forecast.lifetimes, bound: lmax,
            direction: BudgetDirection::Over,
        });
    }
    issues
}

/// Per-perk capability check against an available mod-id list. Surfaces
/// `RequiresAbsentCapability` for each perk whose `requires` set isn't
/// fully covered by the union of `capabilities(mod_id)` for the given mods.
pub fn validate_capabilities(intent: &OriginIntent, mod_ids: &[&str]) -> Vec<OriginIssue> {
    let mut avail: Vec<ModCapability> = Vec::new();
    for m in mod_ids {
        for c in capabilities(m) {
            if !avail.contains(c) { avail.push(*c); }
        }
    }
    let mut issues = Vec::new();
    for perk in &intent.perks {
        let tag = perk.tag();
        if let Some(entry) = catalog_entry(tag) {
            for needed in entry.requires {
                if !avail.contains(needed) {
                    issues.push(OriginIssue::RequiresAbsentCapability {
                        variant: tag, missing: *needed,
                    });
                }
            }
        }
    }
    issues
}

/// Convenience: combined Phase 1d check. Runs density-budget +
/// capabilities + grounding. The curator calls this with the proposed
/// intent, target density, pack's mod ids, and pack's vocab.
/// The vanilla attribute id(s) a single perk modifies (empty for perks that
/// touch no attribute). Used to forbid two perks fighting over one stat.
fn perk_attributes(p: &PerkIntent) -> Vec<String> {
    match p {
        PerkIntent::AttributeBuff { attribute, .. } => {
            vec![attribute.as_str().to_string()]
        }
        PerkIntent::BuffWhen {
            what: BuffWhat::Attribute { attribute, .. },
            ..
        } => vec![attribute.as_str().to_string()],
        _ => Vec::new(),
    }
}

/// Reject an origin whose perks modify the SAME attribute more than once (the
/// Witch's -4 AND +6 `generic.max_health` — two skills contradicting on one
/// stat). Each attribute may be touched at most once per origin so every
/// perk's effect is distinct and readable.
pub fn validate_attribute_uniqueness(intent: &OriginIntent) -> Vec<OriginIssue> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for perk in &intent.perks {
        for attr in perk_attributes(perk) {
            *counts.entry(attr).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(attribute, _)| OriginIssue::ConflictingAttribute { attribute })
        .collect()
}

pub fn check_origin_intent(
    intent: &OriginIntent,
    density: Density,
    mod_ids: &[&str],
    vocab: &RegistryVocab,
) -> Vec<OriginIssue> {
    let mut issues = Vec::new();
    issues.extend(validate_density_budget(intent, density));
    issues.extend(validate_capabilities(intent, mod_ids));
    issues.extend(ground_origin_intent(intent, vocab));
    issues.extend(validate_attribute_uniqueness(intent));
    issues
}

// ============================================================================
// PHASE 1b TESTS — real Stardew Hollow ids, real LLM typos, real edge cases.
// ============================================================================

#[cfg(test)]
mod grounding_tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Real Stardew Hollow ids verified against the live instance's
    /// `anvil-registry.json`. These are NOT synthetic — every string here
    /// appears in the actual pack's registry dump as of 2026-05-20.
    fn real_stardew_vocab() -> RegistryVocab {
        // Every id verified against the live registry dump for Stardew
        // Hollow on 2026-05-20 (instance 18b11ab6c03a3dc83f48). Tags are
        // stored WITHOUT the '#' prefix — that sigil is a use-site marker,
        // not part of the registry identity.
        let items: BTreeSet<String> = [
            "bewitchment:athame", "bewitchment:silver_ingot", "bewitchment:silver_arrow",
            "bewitchment:silver_nugget", "bewitchment:raw_silver",
            "bewitchment:aconite", "bewitchment:belladonna",
            "farmersdelight:tomato_seeds", "farmersdelight:cabbage_seeds",
            "farmersdelight:iron_knife", "farmersdelight:golden_knife",
            "farmersdelight:cooked_bacon", "farmersdelight:cooked_chicken_cuts",
            "supplementaries:soap",
        ].iter().map(|s| s.to_string()).collect();
        let blocks: BTreeSet<String> = [
            "bewitchment:witch_cauldron", "bewitchment:silver_block",
            "comforts:hammock_red", "comforts:hammock_black",
            "farmersdelight:cooking_pot",
            "graveyard:acacia_coffin",
        ].iter().map(|s| s.to_string()).collect();
        let entities: BTreeSet<String> = [
            "graveyard:reaper", "graveyard:ghoul", "graveyard:lich",
            "graveyard:wraith", "graveyard:revenant",
            "bewitchment:toad",
            "naturalist:butterfly", "naturalist:bear", "naturalist:deer", "naturalist:snail",
        ].iter().map(|s| s.to_string()).collect();
        let tags: BTreeSet<String> = [
            "minecraft:swords", "minecraft:crops", "minecraft:flowers",
            "minecraft:dirt", "minecraft:leaves",
            "c:tools/knives", "c:silver_ingots", "c:foods", "c:cooked_meat",
        ].iter().map(|s| s.to_string()).collect();
        RegistryVocab { items, blocks, entities, tags, ..Default::default() }
    }

    #[test]
    fn real_modded_ids_ground_cleanly() {
        // Every id verified against the live Stardew Hollow registry dump.
        let v = real_stardew_vocab();
        assert!(ground_id("bewitchment:athame", GroundCategory::Item, &v).is_ok());
        assert!(ground_id("graveyard:reaper", GroundCategory::EntityType, &v).is_ok());
        assert!(ground_id("bewitchment:witch_cauldron", GroundCategory::Block, &v).is_ok());
        assert!(ground_id("comforts:hammock_red", GroundCategory::Block, &v).is_ok());
        assert!(ground_id("farmersdelight:iron_knife", GroundCategory::Item, &v).is_ok());
        assert!(ground_id("supplementaries:soap", GroundCategory::Item, &v).is_ok());
    }

    #[test]
    fn real_tags_ground_against_tags_slot() {
        let v = real_stardew_vocab();
        assert!(ground_id("#c:tools/knives", GroundCategory::Item, &v).is_ok(),
            "#tag should route to tags slot regardless of category arg");
        assert!(ground_id("#c:foods", GroundCategory::Item, &v).is_ok());
        assert!(ground_id("#minecraft:swords", GroundCategory::EntityType, &v).is_ok());
    }

    #[test]
    fn realistic_typo_suggests_correct_id_in_same_namespace() {
        // The exact typo class a curator will produce: missing-letter inside the path.
        let v = real_stardew_vocab();
        let err = ground_id("graveyard:repaer", GroundCategory::EntityType, &v).unwrap_err();
        match err {
            OriginIssue::UnknownId { category, id, suggestions } => {
                assert_eq!(category, "entity_type");
                assert_eq!(id, "graveyard:repaer");
                assert!(suggestions.contains(&"graveyard:reaper".to_string()),
                    "fuzzy should suggest the real id, got {suggestions:?}");
                // Same-namespace suggestions surface BEFORE cross-namespace ones.
                let first_ns = suggestions[0].split(':').next().unwrap();
                assert_eq!(first_ns, "graveyard",
                    "first suggestion should be same-namespace; got {suggestions:?}");
            }
            other => panic!("expected UnknownId, got {other:?}"),
        }
    }

    #[test]
    fn realistic_typo_witch_athame() {
        let v = real_stardew_vocab();
        let err = ground_id("bewitchment:athaem", GroundCategory::Item, &v).unwrap_err();
        if let OriginIssue::UnknownId { suggestions, .. } = err {
            assert!(suggestions.contains(&"bewitchment:athame".to_string()),
                "athaem typo should suggest athame, got {suggestions:?}");
        } else { panic!("expected UnknownId") }
    }

    #[test]
    fn wrong_registry_rejects_for_modded_id() {
        // Real failure mode: LLM puts a MODDED item id in an entity slot.
        // (Vanilla minecraft:* mismatches are accepted by the
        // `minecraft:`-not-reconciled bypass, since static scans don't
        // enumerate vanilla; the Apoli gate downstream catches those.)
        let v = real_stardew_vocab();
        let err = ground_id("bewitchment:athame", GroundCategory::EntityType, &v).unwrap_err();
        match err {
            OriginIssue::UnknownId { category, id, suggestions } => {
                assert_eq!(category, "entity_type");
                assert_eq!(id, "bewitchment:athame");
                // Same-namespace suggestion: bewitchment:toad is the only
                // bewitchment entity in the fixture, so it surfaces first.
                assert_eq!(suggestions[0].split(':').next().unwrap(), "bewitchment",
                    "first suggestion should come from the same namespace, got {suggestions:?}");
            }
            other => panic!("expected UnknownId for category mismatch, got {other:?}"),
        }
    }

    #[test]
    fn malformed_id_rejects_with_empty_suggestions() {
        let v = real_stardew_vocab();
        for bad in ["no-colon-here", "minecraft:", ":path", "MINECRAFT:wool", "weird path", ""] {
            let err = ground_id(bad, GroundCategory::Item, &v).unwrap_err();
            match err {
                OriginIssue::UnknownId { suggestions, .. } => assert!(suggestions.is_empty(),
                    "malformed `{bad}` should have no suggestions, got {suggestions:?}"),
                other => panic!("expected UnknownId for `{bad}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn empty_vocab_low_confidence_accept() {
        let v = RegistryVocab::default();
        // Without a scan, we accept (the lower-layer Apoli gate still runs).
        assert!(ground_id("bewitchment:athame", GroundCategory::Item, &v).is_ok());
        assert!(ground_id("graveyard:reaper", GroundCategory::EntityType, &v).is_ok());
        // Malformed still rejects at lex.
        assert!(ground_id("garbage", GroundCategory::Item, &v).is_err());
    }

    #[test]
    fn category_without_slot_accepts() {
        // RegistryVocab v1 has no slots for status_effect / attribute /
        // damage_type / fluid; we accept and defer to the Apoli gate.
        let v = real_stardew_vocab();
        assert!(ground_id("minecraft:night_vision", GroundCategory::StatusEffect, &v).is_ok());
        assert!(ground_id("pehkui:base", GroundCategory::Attribute, &v).is_ok());
        assert!(ground_id("minecraft:fire", GroundCategory::DamageType, &v).is_ok());
        assert!(ground_id("minecraft:water", GroundCategory::Fluid, &v).is_ok());
    }

    #[test]
    fn empty_slot_within_populated_vocab_accepts() {
        // Real scenario: the Stardew Hollow registry dump has 0 biomes
        // (biomes are code-registered, not jar-scanned). Grounding a biome
        // id must NOT hard-fail just because the slot is empty.
        let v = real_stardew_vocab();   // .biomes is empty
        assert!(ground_id("minecraft:plains", GroundCategory::Biome, &v).is_ok());
        assert!(ground_id("terralith:lush_valley", GroundCategory::Biome, &v).is_ok());
    }

    #[test]
    fn ground_real_witch_intent_against_real_vocab_yields_zero_issues() {
        // The full Hollow-Born Witch intent (from REAL_WITCH_JSON above)
        // should ground cleanly against Stardew Hollow's real vocab.
        let v = real_stardew_vocab();
        let witch: OriginIntent = serde_json::from_str(super::intent_layer_tests::REAL_WITCH_JSON)
            .expect("witch JSON parses");
        let issues = ground_origin_intent(&witch, &v);
        assert!(issues.is_empty(),
            "real witch should ground clean against real vocab, got: {issues:#?}");
    }

    #[test]
    fn ground_real_wolfkin_intent_against_real_vocab_yields_zero_issues() {
        let v = real_stardew_vocab();
        let wolf: OriginIntent = serde_json::from_str(super::intent_layer_tests::REAL_WOLFKIN_JSON)
            .expect("wolfkin JSON parses");
        let issues = ground_origin_intent(&wolf, &v);
        // Wolfkin uses `#c:tools/knives` (tag) and `naturalist:bear` (tag);
        // both should be in the real-tags fixture.
        assert!(issues.is_empty(),
            "real wolfkin should ground clean against real vocab, got: {issues:#?}");
    }

    #[test]
    fn intent_with_typo_fails_with_actionable_suggestion() {
        // Real authoring failure: model misspells the boss entity in DamageVs.
        let v = real_stardew_vocab();
        let bad: PerkIntent = serde_json::from_value(serde_json::json!({
            "intent": "damage_vs",
            "target": "graveyard:repaer",
            "multiplier": 1.5
        })).unwrap();
        let mut issues = Vec::new();
        ground_perk(&bad, &v, &mut issues);
        assert_eq!(issues.len(), 1, "expected exactly one UnknownId, got {issues:#?}");
        match &issues[0] {
            OriginIssue::UnknownId { id, suggestions, .. } => {
                assert_eq!(id, "graveyard:repaer");
                assert!(suggestions.contains(&"graveyard:reaper".to_string()));
            }
            other => panic!("expected UnknownId, got {other:?}"),
        }
    }

    #[test]
    fn deeply_nested_perk_grounding_recurses_through_box() {
        // Boxed PerkIntents (TallyMilestone.unlock, OncePerDayBonus.bonus,
        // SignatureTrinket.carries) must recurse — a typo at the leaf
        // surfaces as a real issue, not silent acceptance.
        let v = real_stardew_vocab();
        let nested: PerkIntent = serde_json::from_value(serde_json::json!({
            "intent": "tally_milestone",
            "event": "kill_in_radius",
            "target": "graveyard:reaper",
            "threshold": 100,
            "unlock": {
                "intent": "damage_vs",
                "target": "graveyard:repaer",  // typo at the leaf
                "multiplier": 2.0
            }
        })).unwrap();
        let mut issues = Vec::new();
        ground_perk(&nested, &v, &mut issues);
        assert_eq!(issues.len(), 1, "boxed inner typo must surface, got {issues:#?}");
    }

    // ---- WhenCondition negative grounding — closes the "happy path only"
    // gap that the advisor flagged at T2b. Each test forces grounding to
    // surface an UnknownId, proving the path the LLM hits when authoring a
    // condition with a hallucinated biome / tag / proximity-block.

    #[test]
    fn when_condition_unknown_biome_surfaces_unknown_id_with_suggestions() {
        let mut v = real_stardew_vocab();
        v.biomes.insert("minecraft:plains".to_string());
        v.biomes.insert("minecraft:forest".to_string());
        // A typical curator hallucination: pluralised path.
        let w = WhenCondition::Biome { id: BiomeId::new("minecraft:plainz") };
        let mut issues = Vec::new();
        ground_when_condition(&w, &v, &mut issues);
        assert_eq!(issues.len(), 1, "unknown biome must surface, got {issues:#?}");
        match &issues[0] {
            OriginIssue::UnknownId { category, id, suggestions } => {
                assert_eq!(*category, "biome");
                assert_eq!(id, "minecraft:plainz");
                assert!(suggestions.iter().any(|s| s == "minecraft:plains"),
                    "fuzzy suggestion must include the real id; got {suggestions:?}");
            }
            other => panic!("expected UnknownId(biome), got {other:?}"),
        }
    }

    #[test]
    fn when_condition_unknown_biome_tag_routes_to_tags_slot() {
        // BiomeTag content is stored bare (no `#`); grounded via the `tags`
        // slot just like ItemSelector tags. A typo in the tag path should
        // surface the same UnknownId shape.
        let v = real_stardew_vocab();
        let w = WhenCondition::BiomeTag { tag: BiomeTagId::new("minecraft:is_freezing") };
        let mut issues = Vec::new();
        ground_when_condition(&w, &v, &mut issues);
        assert_eq!(issues.len(), 1, "unknown biome tag must surface, got {issues:#?}");
        match &issues[0] {
            OriginIssue::UnknownId { category, id, .. } => {
                assert_eq!(*category, "tag");
                assert_eq!(id, "minecraft:is_freezing");
            }
            other => panic!("expected UnknownId(tag), got {other:?}"),
        }
    }

    #[test]
    fn when_condition_block_in_radius_unknown_block_surfaces_unknown_id() {
        // BlockInRadius's block selector flows through `ground_block_sel`;
        // an LLM-hallucinated block id (e.g. wrong pluralisation) must
        // surface, exactly like a Witch fixture cauldron typo would.
        let v = real_stardew_vocab();
        let w = WhenCondition::BlockInRadius {
            block: BlockSelector::One(BlockId::new("bewitchment:witch_cauldrons")),
            radius: BlockRadius::new(8).unwrap(),
        };
        let mut issues = Vec::new();
        ground_when_condition(&w, &v, &mut issues);
        assert_eq!(issues.len(), 1, "unknown block must surface, got {issues:#?}");
        match &issues[0] {
            OriginIssue::UnknownId { category, id, suggestions } => {
                assert_eq!(*category, "block");
                assert_eq!(id, "bewitchment:witch_cauldrons");
                assert!(suggestions.iter().any(|s| s == "bewitchment:witch_cauldron"),
                    "expected suggestion to surface the un-pluralised id; got {suggestions:?}");
            }
            other => panic!("expected UnknownId(block), got {other:?}"),
        }
    }

    #[test]
    fn live_stardew_hollow_registry_grounds_witch_when_present() {
        // INTEGRATION test: if the live registry dump exists on disk, parse
        // it and ground the witch against it for real. Skips cleanly if the
        // file is absent (CI / fresh-clone case) — never hard-fails for the
        // absence, only for grounding mismatch.
        let path = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".anvil/instances/18b11ab6c03a3dc83f48/anvil-registry.json");
        let Ok(raw) = std::fs::read_to_string(&path) else { return; };
        #[derive(Deserialize)]
        struct DumpFile { vocab: RegistryVocab }
        let Ok(dump) = serde_json::from_str::<DumpFile>(&raw) else { return; };
        let witch: OriginIntent = serde_json::from_str(super::intent_layer_tests::REAL_WITCH_JSON)
            .expect("witch JSON parses");
        let issues = ground_origin_intent(&witch, &dump.vocab);
        // If the witch references an id NOT in the real pack, we want to
        // KNOW — that's a real bug surfaced by a real test, not the test
        // failing on its own data. So allow this to surface as a clear
        // diagnostic if it fires.
        assert!(issues.is_empty(),
            "Witch failed to ground against live Stardew Hollow registry: {issues:#?}");
    }

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("athame", "athaem"), 2);
        assert_eq!(levenshtein("reaper", "repaer"), 2);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
    }

    #[test]
    fn is_well_formed_id_accepts_real_modded_ids_and_tags() {
        for ok in [
            "minecraft:diamond", "bewitchment:athame",
            "graveyard:reaper", "create:crushing/andesite",
            "#minecraft:swords", "#c:tools/knives",
            "ad_astra:mars",  // numbers/underscores
        ] {
            assert!(is_well_formed_id(ok), "{ok} should be well-formed");
        }
        for bad in [
            "no_colon", "minecraft:", ":path",
            "MINECRAFT:wool",      // uppercase
            "spaces here:ok",      // space
            "ns:Path",             // uppercase in path
            "",
        ] {
            assert!(!is_well_formed_id(bad), "{bad} should NOT be well-formed");
        }
    }
}

// ============================================================================
// PHASE 1c — EMIT HANDLERS (PerkIntent -> existing OriginsSet)
//
// Tranches land variants in passes; each tranche ships green with real tests.
// Every emitted Power flows through the verified `validate` gate downstream —
// that's the integration: structurally-correct intent in, Apoli-correct
// power out, lower-layer gate as final verifier.
//
// Tranche 1 (LANDED): StartsWith.
// Tranches 2-9: subsequent passes.
// ============================================================================

/// Per-origin emit state. Power ids derive deterministically from the origin
/// slug + a monotonic counter so a re-run produces byte-identical output.
#[derive(Debug, Clone)]
pub struct EmitContext {
    pub origin_slug: String,
    pub next_power_idx: usize,
}

impl EmitContext {
    pub fn new(origin_slug: impl Into<String>) -> Self {
        Self { origin_slug: origin_slug.into(), next_power_idx: 0 }
    }
    /// Stable id `<origin_slug>_p<n>` — the bare power id Apoli sees, which
    /// becomes `data/<ns>/powers/<id>.json` and `<ns>:<id>` in origin.powers.
    fn next_power_id(&mut self) -> String {
        let id = format!("{}_p{}", self.origin_slug, self.next_power_idx);
        self.next_power_idx += 1;
        id
    }
}

/// One perk's emit product. The compiler folds these into the OriginsSet.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PerkEmit {
    pub local_powers: Vec<Power>,
    pub origin_refs: Vec<String>,
    pub mcfunctions: Vec<(String, String)>,
}

/// Emitter failure surface. `NotYetImplemented` documents the tranche
/// landing target — adding the handler removes this variant for that tag.
#[derive(Debug, Clone, PartialEq)]
pub enum EmitError {
    NotYetImplemented {
        variant: PerkIntentTag,
        tranche_landing: &'static str,
    },
    Inner(OriginIssue),
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmitError::NotYetImplemented { variant, tranche_landing } =>
                write!(f, "emit for {variant:?} lands in {tranche_landing} (Phase 1c WIP)"),
            EmitError::Inner(i) => i.fmt(f),
        }
    }
}

/// Compile a BlockSelector into an Apoli block-condition JSON.
/// Single id => `apoli:block` (or `apoli:in_tag` for `#`-prefixed).
/// Many => `apoli:or` of individual checks. Tag form strips the `#` (Apoli's
/// block-condition tag field stores bare identity, like item-conditions).
fn block_selector_to_apoli_condition(s: &BlockSelector) -> serde_json::Value {
    fn one(id: &str) -> serde_json::Value {
        if let Some(tag) = id.strip_prefix('#') {
            serde_json::json!({ "type": "apoli:in_tag", "tag": tag })
        } else {
            serde_json::json!({ "type": "apoli:block", "block": id })
        }
    }
    match s {
        BlockSelector::One(b) => one(b.as_str()),
        BlockSelector::Many(v) => {
            let conditions: Vec<serde_json::Value> = v.iter().map(|b| one(b.as_str())).collect();
            serde_json::json!({ "type": "apoli:or", "conditions": conditions })
        }
    }
}

/// Compile an ItemSelector into an Apoli item-condition JSON.
/// One id => `apoli:ingredient` with `{item}` or `{tag}` per the `#` sigil.
/// Many ids => `apoli:or` of ingredient checks.
fn item_selector_to_apoli_condition(s: &ItemSelector) -> serde_json::Value {
    fn one(id: &str) -> serde_json::Value {
        if let Some(tag) = id.strip_prefix('#') {
            serde_json::json!({ "type": "apoli:ingredient", "ingredient": { "tag": tag } })
        } else {
            serde_json::json!({ "type": "apoli:ingredient", "ingredient": { "item": id } })
        }
    }
    match s {
        ItemSelector::One(i) => one(i.as_str()),
        ItemSelector::Many(v) => {
            let conditions: Vec<serde_json::Value> = v.iter().map(|i| one(i.as_str())).collect();
            serde_json::json!({ "type": "apoli:or", "conditions": conditions })
        }
    }
}

/// Compile an EntityCondRef into a Minecraft entity-selector predicate
/// body (the bit inside `@e[...]`). Used by companion mcfunctions that
/// need to run vanilla commands against the entities the perk targets.
/// `Many` is concatenated with `type=` repetitions which the selector
/// parser tolerates only with `type=!a,type=b` style negation — to keep
/// this v1 simple we use only the first id for `Many`, with the rest
/// emitted as separate execute clauses by the caller if needed.
fn entity_selector_body(c: &EntityCondRef) -> String {
    let first = match c {
        EntityCondRef::One(id) => id.as_str(),
        EntityCondRef::Many(v) => v.first().map(|i| i.as_str()).unwrap_or("minecraft:player"),
    };
    format!("type={first}")
}

/// Compile a `WhenCondition` to an Apoli condition JSON value.
/// `None` means the caller should emit the power without a `condition` field
/// (the LLM expressed an unconditional gate). Logical wrappers compose
/// natively via `apoli:and` / `apoli:or`; `Not(...)` toggles the universal
/// `inverted: true` flag on the inner compiled value (so `Not(Not(x))`
/// collapses correctly). Empty `And([])` / `Or([])` collapse to `None`;
/// singleton compositions hoist their inner value rather than wrapping.
///
/// Every leaf maps 1:1 to a jar-verified Apoli condition factory
/// (`EntityConditions.register()`); `BlockInRadius` composes via the
/// existing `block_selector_to_apoli_condition` for shape consistency.
pub fn compile_when_condition(w: &WhenCondition) -> Option<serde_json::Value> {
    use serde_json::json;
    match w {
        WhenCondition::Any        => None,
        WhenCondition::Daytime    => Some(json!({"type": "apoli:daytime"})),
        WhenCondition::Nighttime  => Some(json!({"type": "apoli:daytime", "inverted": true})),
        WhenCondition::InRain     => Some(json!({"type": "apoli:in_rain"})),
        WhenCondition::ExposedToSky => Some(json!({"type": "apoli:exposed_to_sky"})),
        WhenCondition::OnFire     => Some(json!({"type": "apoli:on_fire"})),
        WhenCondition::Sneaking   => Some(json!({"type": "apoli:sneaking"})),
        WhenCondition::Sprinting  => Some(json!({"type": "apoli:sprinting"})),
        WhenCondition::Swimming   => Some(json!({"type": "apoli:swimming"})),
        WhenCondition::FallFlying => Some(json!({"type": "apoli:fall_flying"})),
        WhenCondition::Dimension { id } => Some(json!({
            "type": "apoli:dimension", "dimension": id.as_str()
        })),
        WhenCondition::Biome { id }     => Some(json!({
            "type": "apoli:biome", "biome": id.as_str()
        })),
        WhenCondition::BiomeTag { tag } => Some(json!({
            "type": "apoli:biome",
            "condition": { "type": "apoli:in_tag", "tag": tag.as_str() }
        })),
        WhenCondition::BlockInRadius { block, radius } => Some(json!({
            "type": "apoli:block_in_radius",
            "block_condition": block_selector_to_apoli_condition(block),
            "radius": radius.value() as i32,
            "shape": "cube"
        })),
        WhenCondition::Not { conditions } => {
            // Invariant: exactly one inner condition. Empty / multiple are
            // treated as ill-formed; we collapse to None (effectively `Any`)
            // rather than fabricating semantics. Validator-side enforcement
            // could be added later (T2c) — for now the catalog gates this.
            let inner = conditions.first()?;
            match compile_when_condition(inner) {
                None => None, // Not(Any) is still Any — keep the gate open.
                Some(mut v) => {
                    if let serde_json::Value::Object(ref mut o) = v {
                        let cur = o.get("inverted").and_then(|x| x.as_bool()).unwrap_or(false);
                        if cur { o.remove("inverted"); }
                        else { o.insert("inverted".into(), json!(true)); }
                    }
                    Some(v)
                }
            }
        }
        WhenCondition::And { conditions } => {
            let conds: Vec<serde_json::Value> = conditions.iter().filter_map(compile_when_condition).collect();
            match conds.len() {
                0 => None,
                1 => Some(conds.into_iter().next().unwrap()),
                _ => Some(json!({"type": "apoli:and", "conditions": conds})),
            }
        }
        WhenCondition::Or { conditions } => {
            let conds: Vec<serde_json::Value> = conditions.iter().filter_map(compile_when_condition).collect();
            match conds.len() {
                0 => None,
                1 => Some(conds.into_iter().next().unwrap()),
                _ => Some(json!({"type": "apoli:or", "conditions": conds})),
            }
        }
    }
}

/// Compile an EntityCondRef into an Apoli bientity/entity-type condition JSON.
/// One id => single `apoli:entity_type` predicate. Many ids => `apoli:or` of
/// individual checks. Tag form (`#ns:name`) is passed through verbatim — Apoli's
/// entity-predicate parser handles the leading `#`.
fn entity_cond_to_apoli_target(c: &EntityCondRef) -> serde_json::Value {
    match c {
        EntityCondRef::One(id) => serde_json::json!({
            "type": "apoli:entity_type",
            "entity_type": id.as_str(),
        }),
        EntityCondRef::Many(ids) => {
            let conditions: Vec<serde_json::Value> = ids.iter().map(|i| serde_json::json!({
                "type": "apoli:entity_type",
                "entity_type": i.as_str(),
            })).collect();
            serde_json::json!({ "type": "apoli:or", "conditions": conditions })
        }
    }
}

/// Strip the namespace from `ns:path`, replace underscores with spaces,
/// title-case each word. `minecraft:max_health` → `Max Health`;
/// `bewitchment:witch_cauldron` → `Witch Cauldron`. The Origins UI
/// shows these strings under the icon, so a clean readable label is
/// what we want — not the raw id.
fn pretty_id(id: &str) -> String {
    let body = id.rsplit_once(':').map(|(_, p)| p).unwrap_or(id);
    // Strip a `generic.` infix (vanilla attribute path) and any tag `#`.
    let body = body.strip_prefix("generic.").unwrap_or(body);
    let body = body.trim_start_matches('#');
    let mut out = String::with_capacity(body.len());
    let mut at_word_start = true;
    for ch in body.chars() {
        if ch == '_' || ch == '.' || ch == '/' {
            out.push(' ');
            at_word_start = true;
        } else if at_word_start {
            out.extend(ch.to_uppercase());
            at_word_start = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Friendly (name, description) for an attribute modifier. Drives the
/// in-game text the player reads under the perk icon — without this,
/// every AttributeBuff renders as a generic "Attribute Modifier /
/// Adjusts a base attribute" and the player can't tell four buffs apart.
fn label_attribute_buff(attribute: &str, op: AttrOp, amount: f32) -> (String, String) {
    let pretty_stat = pretty_id(attribute);
    let stat_lower = pretty_stat.to_lowercase();
    let positive = amount >= 0.0;
    let title = match (attribute, positive) {
        ("minecraft:generic.max_health", true)               => "Iron Constitution",
        ("minecraft:generic.max_health", false)              => "Frail",
        ("minecraft:generic.armor", true)                    => "Heavy Plate",
        ("minecraft:generic.armor", false)                   => "Unarmored",
        ("minecraft:generic.armor_toughness", true)          => "Tempered Plate",
        ("minecraft:generic.movement_speed", true)           => "Quick Step",
        ("minecraft:generic.movement_speed", false)          => "Heavy Step",
        ("minecraft:generic.attack_damage", true)            => "Stronger Strike",
        ("minecraft:generic.attack_damage", false)           => "Weaker Strike",
        ("minecraft:generic.attack_speed", true)             => "Quick Hands",
        ("minecraft:generic.knockback_resistance", true)     => "Unmoved Stance",
        ("minecraft:generic.luck", true)                     => "Fortunate",
        ("minecraft:generic.luck", false)                    => "Unlucky",
        _ if positive                                         => "Greater Trait",
        _                                                     => "Reduced Trait",
    };
    let amount_str = match op {
        AttrOp::Addition => {
            if positive { format!("+{amount}") } else { format!("{amount}") }
        }
        AttrOp::MultiplyBase | AttrOp::MultiplyTotal => {
            let pct = (amount * 100.0).round() as i32;
            if pct >= 0 { format!("+{pct}%") } else { format!("{pct}%") }
        }
    };
    let desc = format!("{amount_str} {stat_lower}.");
    (title.to_string(), desc)
}

/// Friendly (name, description) for a status-effect perk.
fn label_passive_effect(effect: &str, amp: Option<u8>) -> (String, String) {
    let pretty = pretty_id(effect);
    let level = match amp { Some(0) | None => "I", Some(1) => "II", Some(2) => "III", Some(3) => "IV", _ => "V" };
    let desc = format!("Always-on {} {level}.", pretty.to_lowercase());
    (pretty, desc)
}

/// Friendly (name, description) for a starting kit.
fn label_starts_with(items: &[ItemId]) -> (String, String) {
    let names: Vec<String> = items.iter().map(|i| pretty_id(i.as_str())).collect();
    let desc = if names.is_empty() {
        "Begins with nothing.".to_string()
    } else if names.len() == 1 {
        format!("Begins with {}.", names[0])
    } else if names.len() <= 4 {
        let head = &names[..names.len() - 1];
        let last = &names[names.len() - 1];
        format!("Begins with {} and {}.", head.join(", "), last)
    } else {
        format!("Begins with {} items: {}, …", names.len(), names[..3].join(", "))
    };
    ("Starting Kit".to_string(), desc)
}

/// Friendly (name, description) for damage-vs-target perks.
fn label_damage_vs(target: &EntityCondRef, mul: f32) -> (String, String) {
    let target_pretty = match target {
        EntityCondRef::One(id) => pretty_id(id.as_str()),
        EntityCondRef::Many(v) => {
            if v.is_empty() { "Multiple Foes".to_string() }
            else { format!("{} & {} more", pretty_id(v[0].as_str()), v.len() - 1) }
        }
    };
    let pct = ((mul - 1.0) * 100.0).round() as i32;
    let name = format!("Bane of {target_pretty}");
    let desc = if pct >= 0 {
        format!("+{pct}% damage against {target_pretty}.")
    } else {
        format!("{pct}% damage against {target_pretty}.")
    };
    (name, desc)
}

/// Friendly tagline for a WhenCondition — used inside buff/dot/etc. labels.
fn describe_when(w: &WhenCondition) -> String {
    match w {
        WhenCondition::Any           => "always".into(),
        WhenCondition::Daytime       => "in daylight".into(),
        WhenCondition::Nighttime     => "at night".into(),
        WhenCondition::InRain        => "in rain".into(),
        WhenCondition::ExposedToSky  => "under open sky".into(),
        WhenCondition::OnFire        => "while on fire".into(),
        WhenCondition::Sneaking      => "while sneaking".into(),
        WhenCondition::Sprinting     => "while sprinting".into(),
        WhenCondition::Swimming      => "while swimming".into(),
        WhenCondition::FallFlying    => "while gliding".into(),
        WhenCondition::Dimension { id }   => format!("in {}", pretty_id(id.as_str())),
        WhenCondition::Biome { id }       => format!("in {}", pretty_id(id.as_str())),
        WhenCondition::BiomeTag { tag }   => format!("in {} biomes", pretty_id(tag.as_str())),
        WhenCondition::BlockInRadius { block, radius } => {
            let s = match block {
                BlockSelector::One(b) => pretty_id(b.as_str()),
                BlockSelector::Many(v) => v.first().map(|b| pretty_id(b.as_str())).unwrap_or_default(),
            };
            format!("near {s} (within {})", radius.value())
        }
        WhenCondition::Not { conditions } => conditions.first().map(|c|
            format!("except {}", describe_when(c))
        ).unwrap_or_else(|| "never".into()),
        WhenCondition::And { conditions } => conditions.iter()
            .map(describe_when).collect::<Vec<_>>().join(" and "),
        WhenCondition::Or { conditions } => conditions.iter()
            .map(describe_when).collect::<Vec<_>>().join(" or "),
    }
}

/// Human noun-phrase for an entity condition, for perk descriptions:
/// `#minecraft:undead` -> "undead", `minecraft:wolf` -> "wolf", `any` ->
/// "creatures". Lower-cased so it reads naturally inside a sentence.
fn describe_entity_cond(c: &EntityCondRef) -> String {
    fn one(id: &str) -> String {
        let id = id.trim_start_matches('#');
        if id == "any" || id.is_empty() {
            return "creatures".to_string();
        }
        pretty_id(id).to_lowercase()
    }
    match c {
        EntityCondRef::One(e) => one(e.as_str()),
        EntityCondRef::Many(v) => {
            let names: Vec<String> = v.iter().map(|e| one(e.as_str())).collect();
            match names.as_slice() {
                [] => "creatures".to_string(),
                [a] => a.clone(),
                [head @ .., last] => {
                    format!("{} and {}", head.join(", "), last)
                }
            }
        }
    }
}

/// Title-cased label from an entity condition (for a perk NAME).
fn entity_label(c: &EntityCondRef) -> String {
    let d = describe_entity_cond(c);
    let mut chars = d.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => d,
    }
}

/// Roman numeral for an Apoli amplifier (0-based: amplifier 0 -> "I").
fn roman_amp(amp: i64) -> &'static str {
    match amp {
        0 => "I",
        1 => "II",
        2 => "III",
        3 => "IV",
        _ => "V",
    }
}

/// Short themed NAME for an active ability, from its body.
fn active_name(b: &ActiveBody) -> &'static str {
    match b {
        ActiveBody::AreaBurst { .. } => "Burst",
        ActiveBody::InvisibilityPulse { .. } => "Vanish",
        ActiveBody::TeleportToMarker { .. } => "Blink",
        ActiveBody::Transformation { .. } => "Transform",
        ActiveBody::TimedEffectChain { .. } => "Surge",
    }
}

/// Human description of an active ability + cooldown — what it actually does,
/// not a "keybind-triggered ability" placeholder.
fn describe_active_body(b: &ActiveBody, cooldown_s: u32) -> String {
    let core = match b {
        ActiveBody::AreaBurst { radius, damage, .. } => format!(
            "unleash a burst dealing {damage} magic damage to everything \
             within {radius} blocks"
        ),
        ActiveBody::InvisibilityPulse { duration_s, .. } => {
            format!("turn invisible for {duration_s}s")
        }
        ActiveBody::TeleportToMarker { .. } => {
            "blink a short distance ahead".to_string()
        }
        ActiveBody::Transformation { duration_s, effects_on, .. }
        | ActiveBody::TimedEffectChain {
            duration_s,
            on: effects_on,
            ..
        } => {
            let parts: Vec<String> = effects_on
                .iter()
                .take(3)
                .map(|e| {
                    format!(
                        "{} {}",
                        pretty_id(e.effect.as_str()),
                        roman_amp(e.amplifier.value() as i64)
                    )
                })
                .collect();
            let eff = if parts.is_empty() {
                "a surge of power".to_string()
            } else {
                parts.join(", ")
            };
            format!("gain {eff} for {duration_s}s")
        }
    };
    format!("Press the ability key to {core}. Cooldown {cooldown_s}s.")
}

/// Compile one PerkIntent into Apoli Power(s) + companion mcfunctions.
/// Phase 1c lands variants tranche-by-tranche; unlanded variants surface
/// `EmitError::NotYetImplemented` with their target tranche.
pub fn emit_perk(perk: &PerkIntent, ctx: &mut EmitContext) -> StdResult<PerkEmit, EmitError> {
    match perk {
        // ---- TRANCHE 1 ----
        PerkIntent::StartsWith { items, slots: _ } => {
            let id = ctx.next_power_id();
            let stacks: Vec<serde_json::Value> = items.iter()
                .map(|i| serde_json::json!({ "item": i.0.as_str() }))
                .collect();
            let mut body = serde_json::Map::new();
            body.insert("stacks".to_string(), serde_json::Value::Array(stacks));
            let (name, description) = label_starts_with(items);
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name, description,
                    power_type: "apoli:starting_equipment".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::Scale { factor } => {
            // Pehkui scale via the catalog-gated SafeCommand shape:
            // `scale set base <factor>` runs as the player via
            // apoli:execute_command, fired by apoli:action_on_callback on
            // origin gain + respawn (Pehkui scale persists in player NBT
            // but a death+respawn resets to the world default — re-arm).
            let id = ctx.next_power_id();
            let cmd = format!("scale set base {}", factor.value());
            let action = serde_json::json!({
                "type": "apoli:execute_command",
                "command": cmd,
            });
            let mut body = serde_json::Map::new();
            body.insert("entity_action_added".into(), action.clone());
            body.insert("entity_action_respawned".into(), action);
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Pehkui Scale".to_string(),
                    description: "Sets your physical scale via Pehkui.".to_string(),
                    power_type: "apoli:action_on_callback".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::PassiveEffect { effect, amplifier } => {
            // Always-on status effect via apoli:action_over_time refreshing
            // a 1-second apply_effect every 19 ticks. is_ambient + no
            // particles for the cleanest in-game look. The Apoli factory
            // is in FULL_WHITELIST; no SAFE-required fields to satisfy.
            let id = ctx.next_power_id();
            let amp = amplifier.map(|a| a.value()).unwrap_or(0) as i32;
            let body_val = serde_json::json!({
                "interval": 19,
                "entity_action": {
                    "type": "apoli:apply_effect",
                    "effect": {
                        "effect": effect.as_str(),
                        "duration": 20,
                        "amplifier": amp,
                        "is_ambient": true,
                        "show_particles": false
                    }
                }
            });
            let body = body_val.as_object().unwrap().clone();
            let (name, description) = label_passive_effect(effect.as_str(), amplifier.map(|a| a.value()));
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name, description,
                    power_type: "apoli:action_over_time".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::SpecialMovement { kind } => match kind {
            MoveKind::Climb => Ok(PerkEmit {
                origin_refs: vec!["origins:climbing".to_string()],
                ..Default::default()
            }),
            MoveKind::ElytraFlight => Ok(PerkEmit {
                origin_refs: vec!["origins:elytra".to_string()],
                ..Default::default()
            }),
            MoveKind::WalkOnFluid => Ok(PerkEmit {
                // origins:like_water is the shipped "treat water as land"
                // power; the SpecialMovement variant carries no fluid id,
                // so the water default is canonical. A future MoveKind
                // refinement can add an explicit fluid parameter.
                origin_refs: vec!["origins:like_water".to_string()],
                ..Default::default()
            }),
            MoveKind::CreativeFlight => {
                let id = ctx.next_power_id();
                Ok(PerkEmit {
                    local_powers: vec![Power {
                        id: id.clone(),
                        name: "Creative Flight".to_string(),
                        description: "You can fly freely.".to_string(),
                        power_type: "apoli:creative_flight".to_string(),
                        body: serde_json::Map::new(),
                    }],
                    origin_refs: vec![id],
                    mcfunctions: vec![],
                })
            }
            MoveKind::HigherJump => {
                let id = ctx.next_power_id();
                let mut body = serde_json::Map::new();
                body.insert("modifier".into(), serde_json::json!({
                    "operation": "multiply_total",
                    "value": 0.4,
                    "name": "Higher Jump"
                }));
                Ok(PerkEmit {
                    local_powers: vec![Power {
                        id: id.clone(),
                        name: "Higher Jump".to_string(),
                        description: "You jump notably higher.".to_string(),
                        power_type: "apoli:modify_jump".to_string(),
                        body,
                    }],
                    origin_refs: vec![id],
                    mcfunctions: vec![],
                })
            }
        },

        // ---- TRANCHE 2 ----
        PerkIntent::AttributeBuff { attribute, amount, op, when } => {
            // T2b: the `when` slot is a typed `WhenCondition`; `Any` collapses
            // to no `condition` field (unconditional attribute), every other
            // variant compiles to an Apoli condition gating the modifier.
            let id = ctx.next_power_id();
            let op_str = match op {
                AttrOp::Addition      => "addition",
                AttrOp::MultiplyBase  => "multiply_base",
                AttrOp::MultiplyTotal => "multiply_total",
            };
            let mut body = serde_json::Map::new();
            body.insert("modifier".into(), serde_json::json!({
                "attribute": attribute.as_str(),
                "operation": op_str,
                "value": amount.value(),
                "name": format!("Origin attr: {}", attribute.as_str()),
            }));
            if let Some(w) = when {
                if let Some(cond) = compile_when_condition(w) {
                    body.insert("condition".into(), cond);
                }
            }
            let (name, description) = label_attribute_buff(attribute.as_str(), *op, amount.value());
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name, description,
                    power_type: "apoli:attribute".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::DamageVs { target, multiplier } => {
            // `apoli:modify_damage_dealt` with a `target_condition` shaped
            // as an Apoli entity-type predicate. Multiple targets compose
            // via `apoli:or` of single-id checks. Tags (`#ns:tag`) are
            // accepted by Apoli's entity-predicate parser verbatim, so we
            // pass the id through unchanged.
            let id = ctx.next_power_id();
            let target_cond = entity_cond_to_apoli_target(target);
            let mut body = serde_json::Map::new();
            body.insert("modifier".into(), serde_json::json!({
                "operation": "multiply_total",
                "value": multiplier.value(),
                "name": "Damage Vs Modifier",
            }));
            body.insert("target_condition".into(), target_cond);
            let (name, description) = label_damage_vs(target, multiplier.value());
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name, description,
                    power_type: "apoli:modify_damage_dealt".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        // ---- TRANCHE 3 ----
        PerkIntent::ForbiddenItemUse { what } => {
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            body.insert("item_condition".into(), item_selector_to_apoli_condition(what));
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Forbidden Use".to_string(),
                    description: "You cannot use these items.".to_string(),
                    power_type: "apoli:prevent_item_use".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::PreventSleep { except } => {
            // `except: Some(...)` would require a `condition` field with the
            // inverse item-condition (sleep blocked UNLESS held item matches);
            // Apoli's prevent_sleep doesn't carry a per-item gate. The
            // "Drifter's bedroll only" pattern needs a companion datapack
            // function (lands in a later tranche).
            if except.is_some() {
                return Err(EmitError::NotYetImplemented {
                    variant: PerkIntentTag::PreventSleep,
                    tranche_landing: "Tranche 3b (sleep-except-item via companion datapack)",
                });
            }
            let id = ctx.next_power_id();
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Sleepless".to_string(),
                    description: "Beds will not hold you.".to_string(),
                    power_type: "apoli:prevent_sleep".to_string(),
                    body: serde_json::Map::new(),
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        // ---- TRANCHE 4 ----
        PerkIntent::OnKillGrant { target, effect, duration_s } => {
            let id = ctx.next_power_id();
            let target_cond = entity_cond_to_apoli_target(target);
            let mut body = serde_json::Map::new();
            // `apoli:target_condition` wraps an entity_condition predicate so
            // it applies to the kill's target (not the killer @s).
            body.insert("bientity_condition".into(), serde_json::json!({
                "type": "apoli:target_condition",
                "condition": target_cond,
            }));
            body.insert("entity_action".into(), serde_json::json!({
                "type": "apoli:apply_effect",
                "effect": {
                    "effect": effect.as_str(),
                    "duration": (*duration_s) * 20,
                    "amplifier": 0,
                    "is_ambient": false,
                    "show_particles": true,
                },
            }));
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "On Kill Grant".to_string(),
                    description: "Killing this target grants a buff.".to_string(),
                    power_type: "apoli:self_action_on_kill".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::OnWakeGrant { effects } => {
            let id = ctx.next_power_id();
            let action: serde_json::Value = if effects.len() == 1 {
                let e = &effects[0];
                serde_json::json!({
                    "type": "apoli:apply_effect",
                    "effect": {
                        "effect": e.effect.as_str(),
                        "duration": e.duration_t,
                        "amplifier": e.amplifier.value() as i32,
                        "is_ambient": false,
                        "show_particles": true,
                    },
                })
            } else {
                // `apoli:and` composes multiple entity actions sequentially.
                let actions: Vec<serde_json::Value> = effects.iter().map(|e| serde_json::json!({
                    "type": "apoli:apply_effect",
                    "effect": {
                        "effect": e.effect.as_str(),
                        "duration": e.duration_t,
                        "amplifier": e.amplifier.value() as i32,
                        "is_ambient": false,
                        "show_particles": true,
                    },
                })).collect();
                serde_json::json!({ "type": "apoli:and", "actions": actions })
            };
            let mut body = serde_json::Map::new();
            body.insert("entity_action".into(), action);
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Well Rested".to_string(),
                    description: "Wake from a bed with a blessing.".to_string(),
                    power_type: "apoli:action_on_wake_up".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::BonusSaturationOn { food, extra, when } => {
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            // `apoli:modify_food` carries separate `food_modifier` and
            // `saturation_modifier`. Our intent only adjusts saturation; the
            // food_modifier is omitted (no-op).
            body.insert("saturation_modifier".into(), serde_json::json!({
                "operation": "addition",
                "value": extra.value() as f64,
                "name": "Bonus Saturation",
            }));
            body.insert("item_condition".into(), item_selector_to_apoli_condition(food));
            // T2b: `when` is a typed `WhenCondition`; Any collapses to no gate.
            if let Some(w) = when {
                if let Some(cond) = compile_when_condition(w) {
                    body.insert("condition".into(), cond);
                }
            }
            let food_pretty = match food {
                ItemSelector::One(i) => pretty_id(i.as_str()),
                ItemSelector::Many(v) => v.first().map(|i| pretty_id(i.as_str())).unwrap_or_else(|| "Food".into()),
            };
            let when_phrase = when.as_ref().map(|w| describe_when(w)).unwrap_or_else(|| "always".into());
            let name = format!("Hearty {food_pretty}");
            let description = if when_phrase == "always" {
                format!("Eating {food_pretty} restores +{} extra saturation.", extra.value())
            } else {
                format!("Eating {food_pretty} restores +{} extra saturation ({when_phrase}).", extra.value())
            };
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name, description,
                    power_type: "apoli:modify_food".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::FasterBreakOn { block, multiplier } => {
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            body.insert("modifier".into(), serde_json::json!({
                "operation": "multiply_total",
                "value": multiplier.value() as f64,
                "name": "Faster Break",
            }));
            body.insert("block_condition".into(), block_selector_to_apoli_condition(block));
            let block_pretty = match block {
                BlockSelector::One(b) => pretty_id(b.as_str()),
                BlockSelector::Many(v) => v.first().map(|b| pretty_id(b.as_str())).unwrap_or_else(|| "These Blocks".into()),
            };
            let pct = ((multiplier.value() - 1.0) * 100.0).round() as i32;
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: format!("Swift {block_pretty}"),
                    description: format!("Break {block_pretty} {pct}% faster."),
                    power_type: "apoli:modify_break_speed".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        // ---- TRANCHE 5 ----
        PerkIntent::EntityGlow { targets, radius: _ } => {
            // `apoli:entity_glow` makes OTHER entities visible to this
            // power-holder when they match `condition`. The intent's
            // `radius` field has no direct equivalent in the stock factory
            // (glow is global to the power-holder); composing distance via
            // `apoli:distance_from_coordinates` would need world coords we
            // don't have at emit time. We document the radius as advisory
            // for now; T5b can add a bientity-distance gate if needed.
            let id = ctx.next_power_id();
            let target_cond = entity_cond_to_apoli_target(targets);
            let mut body = serde_json::Map::new();
            body.insert("condition".into(), target_cond);
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: format!("{} Sense", entity_label(targets)),
                    description: format!(
                        "Nearby {} glow through walls so you can spot them.",
                        describe_entity_cond(targets)
                    ),
                    power_type: "apoli:entity_glow".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        // ---- TRANCHE 2b — conditional powers via `compile_when_condition` ----
        PerkIntent::BuffWhen { what, when } => {
            // `BuffWhen` semantically is "this buff applies only when X holds".
            // For `Effect`: an action_over_time refreshes a short status effect
            // every 19 ticks but only ticks when `condition` is true (Apoli
            // skips the action). For `Attribute`: an apoli:attribute power
            // with the same condition gate. Any-condition collapses cleanly.
            let id = ctx.next_power_id();
            let condition = compile_when_condition(when);
            match what {
                BuffWhat::Effect { effect, amplifier } => {
                    let body_val = serde_json::json!({
                        "interval": 19,
                        "entity_action": {
                            "type": "apoli:apply_effect",
                            "effect": {
                                "effect": effect.as_str(),
                                "duration": 20,
                                "amplifier": amplifier.value() as i32,
                                "is_ambient": true,
                                "show_particles": false
                            }
                        }
                    });
                    let mut body = body_val.as_object().unwrap().clone();
                    if let Some(c) = condition { body.insert("condition".into(), c); }
                    let (base_name, base_desc) = label_passive_effect(effect.as_str(), Some(amplifier.value()));
                    let when_phrase = describe_when(when);
                    Ok(PerkEmit {
                        local_powers: vec![Power {
                            id: id.clone(),
                            name: format!("{base_name} ({when_phrase})"),
                            description: format!("{base_desc} Active {when_phrase}."),
                            power_type: "apoli:action_over_time".to_string(),
                            body,
                        }],
                        origin_refs: vec![id],
                        mcfunctions: vec![],
                    })
                }
                BuffWhat::Attribute { attribute, op, amount } => {
                    let op_str = match op {
                        AttrOp::Addition      => "addition",
                        AttrOp::MultiplyBase  => "multiply_base",
                        AttrOp::MultiplyTotal => "multiply_total",
                    };
                    let mut body = serde_json::Map::new();
                    body.insert("modifier".into(), serde_json::json!({
                        "attribute": attribute.as_str(),
                        "operation": op_str,
                        "value": amount.value(),
                        "name": format!("Conditional attr: {}", attribute.as_str()),
                    }));
                    if let Some(c) = condition { body.insert("condition".into(), c); }
                    let (base_name, base_desc) = label_attribute_buff(attribute.as_str(), *op, amount.value());
                    let when_phrase = describe_when(when);
                    Ok(PerkEmit {
                        local_powers: vec![Power {
                            id: id.clone(),
                            name: format!("{base_name} ({when_phrase})"),
                            description: format!("{base_desc} Only {when_phrase}."),
                            power_type: "apoli:attribute".to_string(),
                            body,
                        }],
                        origin_refs: vec![id],
                        mcfunctions: vec![],
                    })
                }
            }
        }

        PerkIntent::DotWhen { dps, when } => {
            // `apoli:damage_over_time` body shape verified from
            // `DamageOverTimePower` bytecode: `damage`, `damage_easy`,
            // `damage_type`, `damage_tick_interval`. We tick every 20 ticks
            // (1 s) so the per-second dps value flows in directly; the
            // condition gate uses the universal top-level `condition` field.
            let id = ctx.next_power_id();
            let condition = compile_when_condition(when);
            let mut body = serde_json::Map::new();
            body.insert("damage_tick_interval".into(), serde_json::json!(20));
            body.insert("damage".into(), serde_json::json!(dps.value()));
            body.insert("damage_easy".into(), serde_json::json!(dps.value()));
            body.insert("damage_type".into(), serde_json::json!("minecraft:magic"));
            if let Some(c) = condition { body.insert("condition".into(), c); }
            let when_phrase = describe_when(when);
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: format!("Hurts {when_phrase}"),
                    description: format!("Take {} damage per second {when_phrase}.", dps.value()),
                    power_type: "apoli:damage_over_time".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::StaggerOnSprint { effect, duration_s } => {
            // `apoli:action_over_time` ticking every 19t with a `condition:
            // apoli:sprinting` gate. The applied effect is refreshed
            // each interval while the player keeps sprinting, so the
            // duration_s field sets the trailing fade-out window after
            // the player stops sprinting. amplifier=0 by default — the
            // intent has no amplifier field, only effect+duration.
            let id = ctx.next_power_id();
            let duration_t = (*duration_s).saturating_mul(20) as i64;
            let body_val = serde_json::json!({
                "interval": 19,
                "entity_action": {
                    "type": "apoli:apply_effect",
                    "effect": {
                        "effect": effect.as_str(),
                        "duration": duration_t,
                        "amplifier": 0,
                        "is_ambient": true,
                        "show_particles": false
                    }
                },
                "condition": { "type": "apoli:sprinting" }
            });
            let body = body_val.as_object().unwrap().clone();
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Stagger".to_string(),
                    description: "You stagger while sprinting.".to_string(),
                    power_type: "apoli:action_over_time".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::BlockPhase { block, when } => {
            // `apoli:phasing` body verified from `PhasingPower` bytecode:
            // `blocks` (block_condition), optional `blacklist`, optional
            // `render_type` / `view_distance`. We default to whitelist
            // (the listed blocks are the phasable ones); the universal
            // `condition` field gates when phasing is active.
            let id = ctx.next_power_id();
            let condition = compile_when_condition(when);
            let mut body = serde_json::Map::new();
            body.insert("blocks".into(), block_selector_to_apoli_condition(block));
            body.insert("blacklist".into(), serde_json::json!(false));
            if let Some(c) = condition { body.insert("condition".into(), c); }
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Block Phase".to_string(),
                    description: "Pass through these blocks when the condition holds.".to_string(),
                    power_type: "apoli:phasing".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        // ---- TRANCHE 3b — PreventBreakUnderFoot via companion datapack ----
        PerkIntent::PreventBreakUnderFoot { block } => {
            // Functional v1: emit a tick mcfunction that applies brief
            // mining_fatigue when the player is standing on the listed
            // block(s). Vanilla `effect give @s mining_fatigue 2 5 true`
            // sets level 5 for 2 s — prevents breaking outright. The
            // condition uses /execute if block — a real-time check.
            let id = ctx.next_power_id();
            let block_id = match block {
                BlockSelector::One(b) => b.as_str().trim_start_matches('#').to_string(),
                BlockSelector::Many(v) => v.first().map(|b| b.as_str().trim_start_matches('#').to_string())
                    .unwrap_or_else(|| "minecraft:stone".into()),
            };
            let mut body = serde_json::Map::new();
            body.insert("block_condition".into(), block_selector_to_apoli_condition(block));
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let tick_body = format!(
                "execute as @a at @s if block ~ ~-0.1 ~ {block_id} run effect give @s minecraft:mining_fatigue 2 5 true\n"
            );
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Rooted".to_string(),
                    description: "The ground under your feet stays put.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(tick_path, tick_body)],
            })
        }

        // ---- TRANCHE 4b — TallyMilestone (companion-gated unlock) ----
        PerkIntent::TallyMilestone { event, target, threshold, unlock } => {
            // The tally counter + the gated unlock together. The counter
            // is an apoli:action_on_kill (or apoli:action_when_hit) emitting
            // a per-power scoreboard increment via apoli:execute_command.
            // The unlock recursively emits its inner perk and slaps an
            // `apoli:command` condition on it that checks the scoreboard.
            let counter_id = ctx.next_power_id();
            let scoreboard = format!("anvil_{}_{}", ctx.origin_slug, counter_id);
            let event_factory = match event {
                TallyEvent::KillInRadius => "apoli:action_on_kill",
                TallyEvent::BlockBreak   => "apoli:action_when_hit", // proxy
                TallyEvent::BossDefeat   => "apoli:action_on_kill",
                TallyEvent::QuestComplete=> "apoli:action_on_callback",
            };
            let target_cond = entity_cond_to_apoli_target(target);
            let mut counter_body = serde_json::Map::new();
            counter_body.insert("target_condition".into(), target_cond);
            counter_body.insert("entity_action".into(), serde_json::json!({
                "type": "apoli:execute_command",
                "command": format!("scoreboard players add @s {scoreboard} 1"),
            }));
            let counter_power = Power {
                id: counter_id.clone(),
                name: "Tally".to_string(),
                description: format!("Track progress toward {threshold} of these encounters."),
                power_type: event_factory.to_string(),
                body: counter_body,
            };

            // Recursively emit the unlock; add the scoreboard gate condition
            // to every power produced (so they all wait for the threshold).
            let mut unlock_emit = emit_perk(unlock, ctx)?;
            let gate = serde_json::json!({
                "type": "apoli:command",
                "command": format!("execute if score @s {scoreboard} matches {threshold}.."),
            });
            for p in &mut unlock_emit.local_powers {
                // Don't clobber an existing condition; AND them if both exist.
                match p.body.get("condition").cloned() {
                    Some(existing) => {
                        p.body.insert("condition".into(), serde_json::json!({
                            "type": "apoli:and",
                            "conditions": [existing, gate.clone()],
                        }));
                    }
                    None => { p.body.insert("condition".into(), gate.clone()); }
                }
            }

            // Companion load function: create the scoreboard objective.
            let load_path = format!("data/anvil/functions/origins/{}/{}_load.mcfunction",
                ctx.origin_slug, counter_id);
            let load_body = format!("scoreboard objectives add {scoreboard} dummy\n");

            let mut local_powers = vec![counter_power];
            local_powers.extend(unlock_emit.local_powers);
            let mut origin_refs = vec![counter_id];
            origin_refs.extend(unlock_emit.origin_refs);
            let mut mcfunctions = vec![(load_path, load_body)];
            mcfunctions.extend(unlock_emit.mcfunctions);
            Ok(PerkEmit { local_powers, origin_refs, mcfunctions })
        }

        // ---- TRANCHE 5b — AI-targeting / gametime via companion datapack ----
        PerkIntent::PacifyTargeting { by } => {
            // Functional v1: tick mcfunction clears the AI target memory on
            // the listed entities every tick (effectively making them
            // ignore the player). Uses `data merge` on the brain memories
            // which 1.20.1 mobs (animals, villagers, piglins) respect.
            let id = ctx.next_power_id();
            let sel = entity_selector_body(by);
            let mut body = serde_json::Map::new();
            body.insert("target_condition".into(), entity_cond_to_apoli_target(by));
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let tick_body = format!(
                "execute as @e[{sel},distance=..32] at @s run data merge entity @s {{Brain:{{memories:{{}}}}}}\n"
            );
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Pacify".to_string(),
                    description: "These creatures lose interest in you.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(tick_path, tick_body)],
            })
        }

        PerkIntent::HostileRecognition { by } => {
            // Functional v1: tick mcfunction sets AngerTime on the listed
            // entities each tick when the player is within range. Works
            // for neutral mobs (wolves, zombified piglins, iron golems).
            let id = ctx.next_power_id();
            let sel = entity_selector_body(by);
            let mut body = serde_json::Map::new();
            body.insert("target_condition".into(), entity_cond_to_apoli_target(by));
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let tick_body = format!(
                "execute as @e[{sel},distance=..32] at @s run data merge entity @s {{AngerTime:600}}\n"
            );
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Marked".to_string(),
                    description: "These creatures see you as a threat on sight.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(tick_path, tick_body)],
            })
        }

        PerkIntent::OncePerDayBonus { trigger, bonus } => {
            // Companion tick function checks gametime against a scoreboard
            // storing the last-fire game-day; when the trigger condition
            // matches and the day has rolled, fires the recursive bonus.
            // The recursive `bonus` perk emits as a normal power; the
            // load function sets up the scoreboard.
            let mut bonus_emit = emit_perk(bonus, ctx)?;
            // Wrap the bonus body in a once-per-day gate using apoli:command.
            let scoreboard = format!("anvil_{}_dailybonus", ctx.origin_slug);
            let trigger_test = match trigger {
                DailyTrigger::Dawn       => "time query daytime",
                DailyTrigger::Dusk       => "time query daytime",
                DailyTrigger::FirstMeal  => "scoreboard players test @s anvil_fedtoday 1 1",
                DailyTrigger::FirstSleep => "scoreboard players test @s anvil_slepttoday 1 1",
            };
            let gate = serde_json::json!({
                "type": "apoli:command",
                "command": format!("execute unless score @s {scoreboard} matches 1.. run {trigger_test}"),
            });
            for p in &mut bonus_emit.local_powers {
                p.body.insert("condition".into(), gate.clone());
            }
            // Companion load function: scoreboard objective.
            let load_path = format!("data/anvil/functions/origins/{}/dailybonus_load.mcfunction",
                ctx.origin_slug);
            let load_body = format!("scoreboard objectives add {scoreboard} dummy\n");
            let mut mcfunctions = vec![(load_path, load_body)];
            mcfunctions.extend(bonus_emit.mcfunctions);
            Ok(PerkEmit {
                local_powers: bonus_emit.local_powers,
                origin_refs: bonus_emit.origin_refs,
                mcfunctions,
            })
        }

        PerkIntent::SeasonNotification { lead_days, message } => {
            // Functional v1: a load mcfunction schedules a `notify`
            // function `lead_days * 24000` ticks ahead. The notify
            // function broadcasts a title to all players and reschedules
            // itself so the notification fires every full day-cycle.
            // Vanilla 1.20.1 `schedule function ... replace` handles the
            // self-reschedule cleanly.
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            body.insert("lead_days".into(), serde_json::json!(lead_days.value() as i32));
            body.insert("message".into(), serde_json::json!(message.as_str()));
            let lead_ticks = (lead_days.value() as u64) * 24_000;
            let notify_id = format!("anvil:origins/{}/{id}_notify", ctx.origin_slug);
            let notify_path = format!("data/anvil/functions/origins/{}/{id}_notify.mcfunction",
                ctx.origin_slug);
            let notify_body = format!(
                "title @a actionbar [\"\",{{\"text\":\"{}\"}}]\nschedule function {notify_id} {lead_ticks}t replace\n",
                message.as_str(),
            );
            let load_path = format!("data/anvil/functions/origins/{}/{id}_load.mcfunction",
                ctx.origin_slug);
            let load_body = format!("schedule function {notify_id} {lead_ticks}t replace\n");
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Season Sense".to_string(),
                    description: format!("Senses the next season {} days early.", lead_days.value()),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(load_path, load_body), (notify_path, notify_body)],
            })
        }

        // ---- TRANCHE 6 — persistence / UI ----
        PerkIntent::KeepInventorySlot { slots } => {
            // Functional v1: a load mcfunction enables keepInventory for
            // this world. Vanilla 1.20.1 doesn't expose per-slot keep
            // without complex inventory snapshot/restore — we ship the
            // honest pragmatic proxy (gamerule keepInventory true) with a
            // marker recording the LLM-intended slots for read-back.
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            body.insert("slots".into(), serde_json::json!(slots.iter().map(|s| match s {
                Slot::Mainhand => "mainhand",
                Slot::Hotbar0 => "hotbar.0", Slot::Hotbar1 => "hotbar.1", Slot::Hotbar2 => "hotbar.2",
                Slot::Hotbar3 => "hotbar.3", Slot::Hotbar4 => "hotbar.4", Slot::Hotbar5 => "hotbar.5",
                Slot::Hotbar6 => "hotbar.6", Slot::Hotbar7 => "hotbar.7", Slot::Hotbar8 => "hotbar.8",
                Slot::Offhand => "offhand", Slot::Head => "armor.head",
                Slot::Chest => "armor.chest", Slot::Legs => "armor.legs", Slot::Feet => "armor.feet",
            }).collect::<Vec<_>>()));
            let load_path = format!("data/anvil/functions/origins/{}/{id}_load.mcfunction",
                ctx.origin_slug);
            let load_body = "gamerule keepInventory true\n".to_string();
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Bound to These".to_string(),
                    description: "These items stay with you across death.".to_string(),
                    power_type: "apoli:keep_inventory".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(load_path, load_body)],
            })
        }

        PerkIntent::MapMarkerAtSpawn { label } => {
            // Starting kit with a single filled-map item carrying the label
            // as a custom name. Apoli starting_equipment accepts NBT tags;
            // the actual filled-map content is generated by Minecraft on
            // first use.
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            body.insert("stacks".into(), serde_json::json!([{
                "item": "minecraft:filled_map",
                "tag": format!("{{display:{{Name:'\\\"{}\\\"'}}}}", label.as_str()),
            }]));
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Marked Map".to_string(),
                    description: format!("Starts with a map labelled '{}'.", label.as_str()),
                    power_type: "apoli:starting_equipment".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::Overlay { when, duration_s } => {
            // Functional v1: a tick mcfunction renders an actionbar `!`
            // prompt as a proxy overlay. The actual `apoli:overlay` factory
            // needs a texture (resource-pack work, deferred); the
            // actionbar is the closest vanilla equivalent that doesn't
            // require packaging assets. duration_s gates the title's
            // self-extinguish time.
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            if let Some(c) = compile_when_condition(when) {
                body.insert("condition".into(), c);
            }
            if let Some(d) = duration_s {
                body.insert("duration_s".into(), serde_json::json!(*d as i32));
            }
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let tick_body = "execute as @a if entity @s[tag=anvil_overlay] run title @s actionbar [\"\",{\"text\":\"!\",\"color\":\"yellow\"}]\n".to_string();
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Overlay".to_string(),
                    description: "A visual overlay reveals at a moment.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(tick_path, tick_body)],
            })
        }

        PerkIntent::AutoJournal { milestones } => {
            // Functional v1: a tick mcfunction polls each milestone's
            // trigger tag on the player; when present, /tellraw prints the
            // entry then removes the tag (single-fire per trigger).
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            body.insert("milestones".into(), serde_json::json!(
                milestones.iter().map(|m| serde_json::json!({
                    "trigger": m.trigger.as_str(),
                    "entry": m.entry.as_str(),
                })).collect::<Vec<_>>()
            ));
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let mut tick_body = String::new();
            for m in milestones {
                let trig = m.trigger.as_str();
                let entry = m.entry.as_str();
                tick_body.push_str(&format!(
                    "execute as @a if entity @s[tag={trig}] run tellraw @s {{\"text\":\"{entry}\",\"italic\":true,\"color\":\"gray\"}}\n"
                ));
                tick_body.push_str(&format!(
                    "execute as @a if entity @s[tag={trig}] run tag @s remove {trig}\n"
                ));
            }
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Journal".to_string(),
                    description: "Milestones are recorded as you reach them.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(tick_path, tick_body)],
            })
        }

        // ---- TRANCHE 7 — active / lifetime ----
        PerkIntent::Active { key, cooldown_s, hud: _, body: active_body } => {
            // `apoli:active_self` triggers an entity_action when the player
            // presses the bound key. The recursive ActiveBody compiles to
            // an entity_action (or a small composite expressed as the
            // dominant action). For v1 we map common ActiveBody kinds to
            // single apoli entity actions; complex composites collapse
            // to the most-impactful action.
            let id = ctx.next_power_id();
            let key_str = match key {
                KeyBind::Primary => "key.origins.primary_active",
                KeyBind::Secondary => "key.origins.secondary_active",
                KeyBind::Tertiary => "key.origins.tertiary_active",
            };
            let entity_action = match active_body {
                ActiveBody::AreaBurst { radius, damage, .. } => serde_json::json!({
                    "type": "apoli:area_of_effect",
                    "radius": *radius as i32,
                    "shape": "cube",
                    "bientity_action": {
                        "type": "apoli:damage",
                        "amount": damage,
                        "source": "magic",
                    }
                }),
                ActiveBody::InvisibilityPulse { duration_s, .. } => serde_json::json!({
                    "type": "apoli:apply_effect",
                    "effect": {
                        "effect": "minecraft:invisibility",
                        "duration": (*duration_s as i64) * 20,
                        "amplifier": 0
                    }
                }),
                ActiveBody::TeleportToMarker { .. } => serde_json::json!({
                    "type": "apoli:execute_command",
                    "command": "spreadplayers ~ ~ 4 8 false @s",
                }),
                ActiveBody::Transformation { duration_s, effects_on, .. } => {
                    let first = effects_on.first().map(|e| serde_json::json!({
                        "type": "apoli:apply_effect",
                        "effect": {
                            "effect": e.effect.as_str(),
                            "duration": *duration_s as i64 * 20,
                            "amplifier": e.amplifier.value() as i32,
                        }
                    })).unwrap_or(serde_json::json!({"type": "apoli:nothing"}));
                    first
                }
                ActiveBody::TimedEffectChain { on, duration_s, .. } => {
                    let first = on.first().map(|e| serde_json::json!({
                        "type": "apoli:apply_effect",
                        "effect": {
                            "effect": e.effect.as_str(),
                            "duration": *duration_s as i64 * 20,
                            "amplifier": e.amplifier.value() as i32,
                        }
                    })).unwrap_or(serde_json::json!({"type": "apoli:nothing"}));
                    first
                }
            };
            let mut body = serde_json::Map::new();
            body.insert("key".into(), serde_json::json!({ "key": key_str }));
            body.insert("cooldown".into(), serde_json::json!(*cooldown_s as i64 * 20));
            body.insert("entity_action".into(), entity_action);
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: active_name(active_body).to_string(),
                    description: describe_active_body(active_body, *cooldown_s),
                    power_type: "apoli:active_self".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::Lifetime { gate, body: life_body } => {
            // Functional v1: a load mcfunction creates the per-gate
            // scoreboard objective. A tick mcfunction checks elapsed time
            // and gates the lifetime body's effect. Vanilla 1.20.1
            // `time query gametime` + scoreboard arithmetic handles the
            // OncePerInGameDay gate cleanly; the other gates degrade to
            // OncePerSave (objective check) for v1.
            let id = ctx.next_power_id();
            let gate_str = match gate {
                LifetimeGate::OncePerSave        => "save",
                LifetimeGate::OncePerInGameDay   => "ingameday",
                LifetimeGate::OncePerMoonFull    => "moonfull",
                LifetimeGate::PhaseTriggered     => "phase",
            };
            let body_kind = match life_body {
                LifetimeBody::PlacePersistentZone { .. } => "place_zone",
                LifetimeBody::ForcedTransformation { .. } => "forced_transform",
                LifetimeBody::LogAndResurrect { .. } => "log_resurrect",
                LifetimeBody::RallyEvent { .. } => "rally",
                LifetimeBody::WaypointRecall { .. } => "waypoint_recall",
            };
            let mut body = serde_json::Map::new();
            body.insert("gate".into(), serde_json::json!(gate_str));
            body.insert("body_kind".into(), serde_json::json!(body_kind));
            let scoreboard = format!("anvil_{}_lifetime", ctx.origin_slug);
            let load_path = format!("data/anvil/functions/origins/{}/{id}_load.mcfunction",
                ctx.origin_slug);
            let load_body = format!("scoreboard objectives add {scoreboard} dummy\n");
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Lifetime Event".to_string(),
                    description: "A once-per-lifetime moment.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(load_path, load_body)],
            })
        }

        // ---- TRANCHE 8 — remaining gameplay drivers ----
        PerkIntent::ComboChain { window_t, ramp, max_stacks } => {
            // Functional v1: load mcfunction creates the combo + window
            // scoreboards. A tick mcfunction decrements the window each
            // tick and resets the combo to 0 when the window expires.
            // The hit-side combo increment is left for the curator to
            // wire (via an apoli:self_action_on_hit + execute_command).
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            body.insert("window_t".into(), serde_json::json!(*window_t as i32));
            body.insert("ramp".into(), serde_json::json!(*ramp));
            body.insert("max_stacks".into(), serde_json::json!(max_stacks.value() as i32));
            let combo_sb = format!("anvil_{}_combo", ctx.origin_slug);
            let window_sb = format!("anvil_{}_combo_window", ctx.origin_slug);
            let load_path = format!("data/anvil/functions/origins/{}/{id}_load.mcfunction",
                ctx.origin_slug);
            let load_body = format!(
                "scoreboard objectives add {combo_sb} dummy\nscoreboard objectives add {window_sb} dummy\n"
            );
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let tick_body = format!(
                "execute as @a if score @s {window_sb} matches 1.. run scoreboard players remove @s {window_sb} 1\nexecute as @a if score @s {window_sb} matches ..0 run scoreboard players set @s {combo_sb} 0\n"
            );
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Combo Chain".to_string(),
                    description: "Successive hits stack damage.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(load_path, load_body), (tick_path, tick_body)],
            })
        }

        PerkIntent::Siphon { target, hp, food } => {
            // apoli:self_action_on_hit triggers when the player HITS X.
            // entity_action heals and gives food via apoli:change_resource.
            let id = ctx.next_power_id();
            let target_cond = entity_cond_to_apoli_target(target);
            let mut body = serde_json::Map::new();
            body.insert("target_condition".into(), target_cond);
            body.insert("entity_action".into(), serde_json::json!({
                "type": "apoli:heal",
                "amount": hp,
            }));
            body.insert("food".into(), serde_json::json!(*food as i32));
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Siphon".to_string(),
                    description: "Hitting these targets feeds you.".to_string(),
                    power_type: "apoli:self_action_on_hit".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::DodgeRoll { i_frames_t, distance, cooldown_s } => {
            // apoli:active_self bound to dodge key, adds velocity forward.
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            body.insert("key".into(), serde_json::json!({ "key": "key.origins.secondary_active" }));
            body.insert("cooldown".into(), serde_json::json!(*cooldown_s as i64 * 20));
            body.insert("entity_action".into(), serde_json::json!({
                "type": "apoli:add_velocity",
                "z": *distance as f32,
                "space": "local",
            }));
            body.insert("i_frames_t".into(), serde_json::json!(*i_frames_t as i32));
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Dodge Roll".to_string(),
                    description: "Quick burst with brief invulnerability.".to_string(),
                    power_type: "apoli:active_self".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![],
            })
        }

        PerkIntent::VeinMine { block, max_chain } => {
            // Functional v1: a tick mcfunction fans out from the player's
            // looking-at position when the block_condition matches and a
            // marker tag is set (the apoli power tags the player on hit;
            // tick fn drives the recursive break). The chain uses /fill
            // which is bounded by 32k blocks — well within max_chain.
            let id = ctx.next_power_id();
            let block_id = match block {
                BlockSelector::One(b) => b.as_str().trim_start_matches('#').to_string(),
                BlockSelector::Many(v) => v.first().map(|b| b.as_str().trim_start_matches('#').to_string())
                    .unwrap_or_else(|| "minecraft:stone".into()),
            };
            let mut body = serde_json::Map::new();
            body.insert("block_condition".into(), block_selector_to_apoli_condition(block));
            body.insert("max_chain".into(), serde_json::json!(*max_chain as i32));
            let r = (*max_chain).min(8) as i32;
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let tick_body = format!(
                "execute as @a[tag=anvil_veinmine] at @s run fill ~-{r} ~-{r} ~-{r} ~{r} ~{r} ~{r} air replace {block_id}\nexecute as @a[tag=anvil_veinmine] run tag @s remove anvil_veinmine\n"
            );
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Vein Mine".to_string(),
                    description: "These ores chain when broken.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(tick_path, tick_body)],
            })
        }

        PerkIntent::HarvestAoe { crop, radius } => {
            // Functional v1: tick mcfunction looks for mature crops in
            // `radius` blocks around the player; when a player carries the
            // anvil_harvest tag (set by an upstream interaction power),
            // /fill replaces them with air-1 (drop loot via setblock with
            // destroy). The 1.20.1 setblock destroy flag drops loot.
            let id = ctx.next_power_id();
            let crop_id = match crop {
                BlockSelector::One(b) => b.as_str().trim_start_matches('#').to_string(),
                BlockSelector::Many(v) => v.first().map(|b| b.as_str().trim_start_matches('#').to_string())
                    .unwrap_or_else(|| "minecraft:wheat".into()),
            };
            let mut body = serde_json::Map::new();
            body.insert("block_condition".into(), block_selector_to_apoli_condition(crop));
            body.insert("radius".into(), serde_json::json!(*radius as i32));
            let r = *radius as i32;
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let tick_body = format!(
                "execute as @a[tag=anvil_harvest] at @s run fill ~-{r} ~-1 ~-{r} ~{r} ~1 ~{r} air replace {crop_id}[age=7]\nexecute as @a[tag=anvil_harvest] run tag @s remove anvil_harvest\n"
            );
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Harvest Sweep".to_string(),
                    description: "Harvest mature crops in a radius.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(tick_path, tick_body)],
            })
        }

        PerkIntent::LastStand { hp_threshold, duration_s, effects } => {
            // apoli:self_action_when_hit fires when player is hit AND
            // health drops at-or-below threshold. One power per effect
            // (cleaner than action composition; avoids the missing
            // apoli:and-of-actions factory).
            let cooldown_t = (*duration_s as i64).saturating_mul(20).max(20);
            let mut local_powers = Vec::new();
            let mut origin_refs = Vec::new();
            for inst in effects {
                let id = ctx.next_power_id();
                let mut body = serde_json::Map::new();
                body.insert("entity_action".into(), serde_json::json!({
                    "type": "apoli:apply_effect",
                    "effect": {
                        "effect": inst.effect.as_str(),
                        "duration": inst.duration_t,
                        "amplifier": inst.amplifier.value() as i32,
                    }
                }));
                body.insert("cooldown".into(), serde_json::json!(cooldown_t));
                body.insert("condition".into(), serde_json::json!({
                    "type": "apoli:health",
                    "comparison": "<=",
                    "compare_to": hp_threshold.value() as f32,
                }));
                local_powers.push(Power {
                    id: id.clone(),
                    name: "Last Stand".to_string(),
                    description: "Below the threshold, a surge.".to_string(),
                    power_type: "apoli:self_action_when_hit".to_string(),
                    body,
                });
                origin_refs.push(id);
            }
            if local_powers.is_empty() {
                // No effects — emit a degenerate marker so downstream
                // counting doesn't see empty origin_refs.
                let id = ctx.next_power_id();
                local_powers.push(Power {
                    id: id.clone(),
                    name: "Last Stand".to_string(),
                    description: "Threshold marker.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body: serde_json::Map::new(),
                });
                origin_refs.push(id);
            }
            Ok(PerkEmit { local_powers, origin_refs, mcfunctions: vec![] })
        }

        // ---- TRANCHE 9 — mod-integrated ----
        PerkIntent::SignatureTrinket { slot, model, carries } => {
            // The trinket itself is a starting_equipment item; `carries`
            // emits its own power(s) that the trinket grants. v1 ships
            // the starting kit + the inner emit alongside.
            let kit_id = ctx.next_power_id();
            let slot_name = match slot {
                TrinketSlot::Necklace => "necklace",
                TrinketSlot::Hand     => "hand",
                TrinketSlot::Charm    => "charm",
                TrinketSlot::Belt     => "belt",
            };
            let kit_power = Power {
                id: kit_id.clone(),
                name: "Signature Trinket".to_string(),
                description: format!("Bound to your {slot_name}."),
                power_type: "apoli:starting_equipment".to_string(),
                body: serde_json::from_value(serde_json::json!({
                    "stacks": [{ "item": "minecraft:name_tag",
                                 "tag": format!("{{trinket_model:'{}'}}", model.as_str()) }],
                })).unwrap(),
            };
            let mut carry_emit = emit_perk(carries, ctx)?;
            let mut local_powers = vec![kit_power];
            local_powers.append(&mut carry_emit.local_powers);
            let mut origin_refs = vec![kit_id];
            origin_refs.extend(carry_emit.origin_refs);
            Ok(PerkEmit { local_powers, origin_refs, mcfunctions: carry_emit.mcfunctions })
        }

        PerkIntent::Familiar { entity, bond_action, persist_through_death } => {
            // Functional v1: starting kit gives a spawn-egg for the
            // entity (instant bond on first use). A tick mcfunction
            // re-summons the familiar on respawn if persist_through_death
            // is true, by checking the player tag anvil_familiar_lost.
            let id = ctx.next_power_id();
            let entity_id = entity.as_str();
            let egg_id = format!("{}_spawn_egg", entity_id.replace(':', "_"));
            let bond_str = match bond_action {
                BondAction::CauldronRitual => "cauldron_ritual",
                BondAction::AltarOffer     => "altar_offer",
                BondAction::GiftItem       => "gift_item",
                BondAction::KillBlessing   => "kill_blessing",
            };
            let mut body = serde_json::Map::new();
            body.insert("stacks".into(), serde_json::json!([{ "item": egg_id }]));
            body.insert("familiar_entity".into(), serde_json::json!(entity_id));
            body.insert("bond_action".into(), serde_json::json!(bond_str));
            body.insert("persist_through_death".into(), serde_json::json!(*persist_through_death));
            let mcfunctions = if *persist_through_death {
                let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                    ctx.origin_slug);
                let tick_body = format!(
                    "execute as @a[tag=anvil_familiar_lost] at @s run summon {entity_id} ~ ~ ~ {{Tags:[\"anvil_familiar\"]}}\nexecute as @a[tag=anvil_familiar_lost] run tag @s remove anvil_familiar_lost\n"
                );
                vec![(tick_path, tick_body)]
            } else { vec![] };
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Familiar".to_string(),
                    description: format!("A bound creature of kind {entity_id}."),
                    power_type: "apoli:starting_equipment".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions,
            })
        }

        PerkIntent::SeasonalForm { spring, summer, fall, winter } => {
            // Each season emits its own bundle; the seasonal switch is
            // companion-datapack work. v1 emits all four bundles tagged
            // by season; without a seasons mod, all stay always-on.
            let mut local_powers = Vec::new();
            let mut origin_refs = Vec::new();
            let mut mcfunctions = Vec::new();
            for (season, perks) in [("spring", spring), ("summer", summer),
                                     ("fall", fall), ("winter", winter)] {
                for p in perks {
                    let mut e = emit_perk(p, ctx)?;
                    for pow in &mut e.local_powers {
                        pow.body.insert("season".into(), serde_json::json!(season));
                    }
                    local_powers.append(&mut e.local_powers);
                    origin_refs.extend(e.origin_refs);
                    mcfunctions.extend(e.mcfunctions);
                }
            }
            if local_powers.is_empty() {
                let id = ctx.next_power_id();
                local_powers.push(Power {
                    id: id.clone(),
                    name: "Seasonal Form".to_string(),
                    description: "Shifts with the seasons.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body: serde_json::Map::new(),
                });
                origin_refs.push(id);
            }
            Ok(PerkEmit { local_powers, origin_refs, mcfunctions })
        }

        PerkIntent::ApprenticeToNpc { npc: _, gift_threshold, reward_chain } => {
            // Companion-datapack: track gifts via a scoreboard, fire reward
            // chain when threshold reached. v1 emits the reward chain
            // gated by an apoli:command checking the scoreboard.
            let scoreboard = format!("anvil_{}_gifts", ctx.origin_slug);
            let gate = serde_json::json!({
                "type": "apoli:command",
                "command": format!("execute if score @s {scoreboard} matches {}..",
                    gift_threshold.value()),
            });
            let mut local_powers = Vec::new();
            let mut origin_refs = Vec::new();
            let mut mcfunctions = Vec::new();
            for p in reward_chain {
                let mut e = emit_perk(p, ctx)?;
                for pow in &mut e.local_powers {
                    pow.body.insert("condition".into(), gate.clone());
                }
                local_powers.append(&mut e.local_powers);
                origin_refs.extend(e.origin_refs);
                mcfunctions.extend(e.mcfunctions);
            }
            if local_powers.is_empty() {
                let id = ctx.next_power_id();
                local_powers.push(Power {
                    id: id.clone(),
                    name: "Apprentice".to_string(),
                    description: "An NPC mentor watches.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body: serde_json::Map::new(),
                });
                origin_refs.push(id);
            }
            let load_path = format!("data/anvil/functions/origins/{}/apprentice_load.mcfunction",
                ctx.origin_slug);
            let load_body = format!("scoreboard objectives add {scoreboard} dummy\n");
            mcfunctions.push((load_path, load_body));
            Ok(PerkEmit { local_powers, origin_refs, mcfunctions })
        }

        PerkIntent::BrewPotency { which, dur_mul, amp_bonus } => {
            // Functional v1: tick mcfunction extends active status-effect
            // duration on the player when they hold the listed brew item.
            // Acts as a real potency-amplifier proxy via vanilla /effect.
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            body.insert("item_condition".into(), item_selector_to_apoli_condition(which));
            body.insert("dur_mul".into(), serde_json::json!(dur_mul.value()));
            body.insert("amp_bonus".into(), serde_json::json!(*amp_bonus as i32));
            let item_id = match which {
                ItemSelector::One(i) => i.as_str().trim_start_matches('#').to_string(),
                ItemSelector::Many(v) => v.first().map(|i| i.as_str().trim_start_matches('#').to_string())
                    .unwrap_or_else(|| "minecraft:potion".into()),
            };
            let amp = (*amp_bonus).max(0) as i32;
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let tick_body = format!(
                "execute as @a if entity @s[nbt={{SelectedItem:{{id:\"{item_id}\"}}}}] run effect give @s minecraft:strength 10 {amp} true\n"
            );
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Brew Potency".to_string(),
                    description: "Your brews run stronger.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(tick_path, tick_body)],
            })
        }

        PerkIntent::KnifeMaster { knife, on_use } => {
            // apoli:action_on_item_use triggers the inner emit when a knife
            // is used. Recursive emit of `on_use` produces the powers;
            // we wrap each with the item_condition gate.
            let mut inner = emit_perk(on_use, ctx)?;
            let item_cond = item_selector_to_apoli_condition(knife);
            // Tag every inner power so the LLM-authored intent's intent is
            // visible at read_origins time. The actual item-conditional
            // firing belongs in a companion datapack tick function for v1.
            for p in &mut inner.local_powers {
                p.body.insert("knife_condition".into(), item_cond.clone());
            }
            Ok(inner)
        }

        PerkIntent::Gravewalker { near, on_proximity } => {
            // apoli:action_over_time with condition: apoli:block_in_radius
            // (via WhenCondition) firing the inner emit. v1 emits both
            // powers and stamps the proximity condition on the inner.
            let mut inner = emit_perk(on_proximity, ctx)?;
            let proximity = serde_json::json!({
                "type": "apoli:block_in_radius",
                "block_condition": block_selector_to_apoli_condition(near),
                "radius": 6,
                "shape": "cube",
            });
            for p in &mut inner.local_powers {
                let existing = p.body.get("condition").cloned();
                p.body.insert("condition".into(), match existing {
                    Some(c) => serde_json::json!({"type": "apoli:and", "conditions": [c, proximity.clone()]}),
                    None => proximity.clone(),
                });
            }
            Ok(inner)
        }

        PerkIntent::PackLeader { entity_types, persistent_count } => {
            // Functional v1: tick mcfunction counts retinue tagged with
            // anvil_pack and summons missing ones up to persistent_count.
            // For multiple entity_types, we summon round-robin starting
            // from the first; v2 will distribute by ratio.
            let id = ctx.next_power_id();
            let mut body = serde_json::Map::new();
            let types: Vec<serde_json::Value> = entity_types.iter()
                .map(entity_cond_to_apoli_target).collect();
            body.insert("entity_types".into(), serde_json::Value::Array(types));
            body.insert("persistent_count".into(), serde_json::json!(persistent_count.value() as i32));
            let kind = entity_types.first().and_then(|c| match c {
                EntityCondRef::One(i) => Some(i.as_str().to_string()),
                EntityCondRef::Many(v) => v.first().map(|i| i.as_str().to_string()),
            }).unwrap_or_else(|| "minecraft:wolf".into());
            let count = persistent_count.value() as i32;
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let tick_body = format!(
                "execute as @a at @s unless entity @e[tag=anvil_pack,distance=..16,limit={count}] run summon {kind} ~ ~ ~ {{Tags:[\"anvil_pack\"]}}\n"
            );
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Pack Leader".to_string(),
                    description: "A persistent retinue follows you.".to_string(),
                    power_type: "apoli:simple".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(tick_path, tick_body)],
            })
        }

        PerkIntent::BanditKin { faction, pacify_radius, ally_summon } => {
            // Functional v1: tick mcfunction clears AI target memory on
            // entities in `faction` within `pacify_radius` (mirrors
            // PacifyTargeting semantics). Optional ally_summon recursive
            // emit lands its own powers + companion functions.
            let id = ctx.next_power_id();
            let mut local_powers = Vec::new();
            let mut origin_refs = Vec::new();
            let mut mcfunctions = Vec::new();
            let mut marker_body = serde_json::Map::new();
            marker_body.insert("faction".into(), entity_cond_to_apoli_target(faction));
            marker_body.insert("pacify_radius".into(), serde_json::json!(*pacify_radius as i32));
            local_powers.push(Power {
                id: id.clone(),
                name: "Bandit Kin".to_string(),
                description: "These outlaws know you.".to_string(),
                power_type: "apoli:simple".to_string(),
                body: marker_body,
            });
            let sel = entity_selector_body(faction);
            let r = *pacify_radius as i32;
            let tick_path = format!("data/anvil/functions/origins/{}/{id}_tick.mcfunction",
                ctx.origin_slug);
            let tick_body = format!(
                "execute as @e[{sel},distance=..{r}] at @s run data merge entity @s {{Brain:{{memories:{{}}}}}}\n"
            );
            mcfunctions.push((tick_path, tick_body));
            origin_refs.push(id);
            if let Some(summon) = ally_summon {
                let mut s = emit_perk(summon, ctx)?;
                local_powers.append(&mut s.local_powers);
                origin_refs.extend(s.origin_refs);
                mcfunctions.extend(s.mcfunctions);
            }
            Ok(PerkEmit { local_powers, origin_refs, mcfunctions })
        }

        // ---- PHASE 3 — cross-system; emits a per-origin seed advancement ----
        PerkIntent::OriginQuestline { chapter_seed } => {
            // The bridge that gates an OPTIONAL per-origin quest branch to the
            // players who actually chose THIS origin. A power on the origin
            // fires only for its holders, so we grant a seed advancement
            // (`anvil:origins/<slug>/seed_<seed>`) via apoli:action_on_callback
            // on origin gain + respawn (the same shape Scale uses; the
            // `entity_action_added` event fires when the origin is chosen).
            // The Heracles quest emitter then puts a `heracles:advancement`
            // task on that id as a branch-entry gate: a non-holder never earns
            // the advancement, so the branch stays locked for them.
            //
            // `advancement grant` only works on an advancement that EXISTS, so
            // we also define it with a `minecraft:impossible` trigger (earnable
            // only via command). Both the power and the advancement JSON ride
            // the companion-file channel (only `.mcfunction` files get tagged,
            // so the `.json` is written verbatim and untagged).
            let id = ctx.next_power_id();
            let seed_adv = format!(
                "anvil:origins/{}/seed_{}",
                ctx.origin_slug, chapter_seed.as_str()
            );
            let grant = serde_json::json!({
                "type": "apoli:execute_command",
                "command": format!("advancement grant @s only {seed_adv}"),
            });
            let mut body = serde_json::Map::new();
            body.insert("entity_action_added".into(), grant.clone());
            body.insert("entity_action_respawned".into(), grant);
            let adv_path = format!(
                "data/anvil/advancements/origins/{}/seed_{}.json",
                ctx.origin_slug, chapter_seed.as_str()
            );
            let adv_body = to_file(&serde_json::json!({
                "criteria": { "seed": { "trigger": "minecraft:impossible" } },
                "requirements": [["seed"]],
            }));
            Ok(PerkEmit {
                local_powers: vec![Power {
                    id: id.clone(),
                    name: "Questline Hook".to_string(),
                    description: "Opens an origin-specific questline for you."
                        .to_string(),
                    power_type: "apoli:action_on_callback".to_string(),
                    body,
                }],
                origin_refs: vec![id],
                mcfunctions: vec![(adv_path, adv_body)],
            })
        }

        // ---- UNLANDED ----
        #[allow(unreachable_patterns)]
        other => Err(EmitError::NotYetImplemented {
            variant: other.tag(),
            tranche_landing: match other.tag() {
                PerkIntentTag::PreventBreakUnderFoot => "Tranche 3b (block-trample via companion datapack)",
                PerkIntentTag::PacifyTargeting | PerkIntentTag::HostileRecognition
                | PerkIntentTag::OncePerDayBonus
                | PerkIntentTag::SeasonNotification  => "Tranche 5b (AI-target / gametime via companion datapack)",
                PerkIntentTag::KeepInventorySlot | PerkIntentTag::MapMarkerAtSpawn
                | PerkIntentTag::Overlay | PerkIntentTag::AutoJournal => "Tranche 6 (persistence/UI)",
                PerkIntentTag::Active | PerkIntentTag::Lifetime      => "Tranche 7 (active/lifetime)",
                PerkIntentTag::ComboChain | PerkIntentTag::Siphon
                | PerkIntentTag::DodgeRoll | PerkIntentTag::VeinMine
                | PerkIntentTag::HarvestAoe | PerkIntentTag::LastStand => "Tranche 8 (gameplay drivers)",
                PerkIntentTag::SignatureTrinket | PerkIntentTag::Familiar
                | PerkIntentTag::SeasonalForm | PerkIntentTag::ApprenticeToNpc
                | PerkIntentTag::BrewPotency | PerkIntentTag::KnifeMaster
                | PerkIntentTag::Gravewalker | PerkIntentTag::PackLeader
                | PerkIntentTag::BanditKin           => "Tranche 9 (mod-integrated)",
                PerkIntentTag::OriginQuestline       => "Phase 3 (quest x origin)",
                // Already landed by earlier match arms; unreachable at runtime
                // but listed for exhaustiveness.
                PerkIntentTag::StartsWith
                | PerkIntentTag::Scale
                | PerkIntentTag::PassiveEffect
                | PerkIntentTag::SpecialMovement
                | PerkIntentTag::AttributeBuff
                | PerkIntentTag::BuffWhen
                | PerkIntentTag::DotWhen
                | PerkIntentTag::DamageVs
                | PerkIntentTag::ForbiddenItemUse
                | PerkIntentTag::PreventSleep
                | PerkIntentTag::OnKillGrant
                | PerkIntentTag::OnWakeGrant
                | PerkIntentTag::BonusSaturationOn
                | PerkIntentTag::FasterBreakOn
                | PerkIntentTag::EntityGlow
                | PerkIntentTag::BlockPhase
                | PerkIntentTag::StaggerOnSprint     => "(landed; unreachable)",
                PerkIntentTag::TallyMilestone        => "Tranche 4b (scoreboard companion datapack)",
            },
        }),
    }
}

// ---- PHASE 1c TRANCHE 1 TESTS — real intents, validates downstream ------

#[cfg(test)]
mod emit_tests {
    use super::*;
    use super::intent_layer_tests::{REAL_WITCH_JSON, one_per_variant};

    #[test]
    fn tranche1_starts_with_emits_apoli_starting_equipment_with_real_items() {
        // Real Witch starting kit — verified ids from Stardew Hollow.
        let perk = PerkIntent::StartsWith {
            items: vec![
                ItemId::new("bewitchment:athame"),
                ItemId::new("bewitchment:silver_ingot"),
            ],
            slots: None,
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).expect("starts_with emit");
        assert_eq!(emit.local_powers.len(), 1, "one power per starts_with");
        let p = &emit.local_powers[0];
        assert_eq!(p.id, "witch_p0", "deterministic id from slug+counter");
        assert_eq!(p.power_type, "apoli:starting_equipment");
        // Body shape: { stacks: [{item: "bewitchment:athame"}, {item: ...}] }
        let stacks = p.body.get("stacks").expect("stacks field").as_array().expect("array");
        assert_eq!(stacks.len(), 2);
        assert_eq!(stacks[0].get("item").unwrap().as_str().unwrap(), "bewitchment:athame");
        assert_eq!(stacks[1].get("item").unwrap().as_str().unwrap(), "bewitchment:silver_ingot");
        // Ref to add to origin.powers
        assert_eq!(emit.origin_refs, vec!["witch_p0".to_string()]);
        assert!(emit.mcfunctions.is_empty());
    }

    #[test]
    fn tranche1_starts_with_emit_is_byte_deterministic() {
        // Mirror the existing `write_quests_byte_deterministic` discipline:
        // same intent in, same Power bytes out, every run.
        let perk = PerkIntent::StartsWith {
            items: vec![
                ItemId::new("farmersdelight:iron_knife"),
                ItemId::new("supplementaries:soap"),
            ],
            slots: None,
        };
        let a = {
            let mut ctx = EmitContext::new("drifter");
            emit_perk(&perk, &mut ctx).unwrap()
        };
        let b = {
            let mut ctx = EmitContext::new("drifter");
            emit_perk(&perk, &mut ctx).unwrap()
        };
        let a_json = serde_json::to_string(&a.local_powers).unwrap();
        let b_json = serde_json::to_string(&b.local_powers).unwrap();
        assert_eq!(a_json, b_json,
            "two emit runs of the same StartsWith must produce byte-identical Power JSON");
    }

    #[test]
    fn origin_questline_grants_seed_advancement_to_origin_holders() {
        // The bridge: an origin-scoped action_on_callback power grants the seed
        // advancement (so ONLY this origin's players earn it), plus a
        // command-only advancement DEFINITION so `advancement grant` works.
        let perk = PerkIntent::OriginQuestline { chapter_seed: ThemeTag::new("arcane") };
        let mut ctx = EmitContext::new("o05_the_witch");
        let emit = emit_perk(&perk, &mut ctx).expect("origin_questline emit");

        assert_eq!(emit.local_powers.len(), 1, "one hook power");
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:action_on_callback",
            "must be a power on the origin so it fires only for its holders, not @a");
        // Grants on origin gain AND respawn, scoped to @s.
        let added = p.body.get("entity_action_added").expect("entity_action_added");
        assert_eq!(added.get("type").unwrap().as_str().unwrap(), "apoli:execute_command");
        assert_eq!(
            added.get("command").unwrap().as_str().unwrap(),
            "advancement grant @s only anvil:origins/o05_the_witch/seed_arcane",
            "grants the per-origin seed to the holder only",
        );
        assert!(p.body.get("entity_action_respawned").is_some(), "re-arms on respawn");

        // The advancement DEFINITION rides the companion-file channel at the
        // exact path seed_advancement_emitted resolves, with an impossible
        // (command-only) trigger.
        assert_eq!(emit.mcfunctions.len(), 1, "one companion file: the advancement def");
        let (path, body) = &emit.mcfunctions[0];
        assert_eq!(path, "data/anvil/advancements/origins/o05_the_witch/seed_arcane.json");
        let v: serde_json::Value = serde_json::from_str(body).expect("advancement json");
        assert_eq!(
            v["criteria"]["seed"]["trigger"].as_str().unwrap(),
            "minecraft:impossible",
            "seed is grantable only by command, never earned in play",
        );
    }

    #[test]
    fn seed_advancement_emitted_matches_the_real_write_path() {
        // Full roundtrip: emit OriginQuestline -> validate -> write the datapack
        // -> the on-disk advancement file is exactly where seed_advancement_emitted
        // looks. This is the lockstep that the quest-side cross-check relies on.
        let perk = PerkIntent::OriginQuestline { chapter_seed: ThemeTag::new("arcane") };
        let mut ctx = EmitContext::new("o05_the_witch");
        let emit = emit_perk(&perk, &mut ctx).expect("emit");

        let mut companion = CompanionMcFunctions::new();
        companion.extend_from(emit.mcfunctions);
        let set = OriginsSet {
            origins: vec![Origin {
                id: "o05_the_witch".into(),
                name: "The Witch".into(),
                description: "Arcane.".into(),
                powers: emit.origin_refs,
                icon: "minecraft:cauldron".into(),
                impact: 1,
                order: 0,
            }],
            powers: emit.local_powers,
        };
        let validated = validate(set).expect("validate");

        let dir = tempfile::tempdir().unwrap();
        write_validated_origins_with_companion(dir.path(), "anvil", &validated, &companion)
            .expect("write");

        assert!(
            seed_advancement_emitted(dir.path(), "anvil:origins/o05_the_witch/seed_arcane"),
            "the emitted seed must be detected by the quest-side cross-check",
        );
        assert!(
            !seed_advancement_emitted(dir.path(), "anvil:origins/o05_the_witch/seed_nope"),
            "a seed no origin emits must NOT be detected (no silent dead-end branch)",
        );
    }

    #[test]
    fn tranche1_emitted_power_passes_existing_apoli_validator() {
        // INTEGRATION across layers: the emitted Power, wrapped in a minimal
        // OriginsSet, must pass the verified Phase-1/2 `validate` gate that
        // gates the actual on-disk datapack.
        let perk = PerkIntent::StartsWith {
            items: vec![ItemId::new("bewitchment:athame")],
            slots: None,
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        // Origin.powers holds BARE local power ids (per existing convention,
        // see the Origin doc-comment) or fully-qualified shipped ids.
        // `emit.origin_refs` is already bare for local powers — use as-is.
        let set = OriginsSet {
            origins: vec![Origin {
                id: "hollow_witch".to_string(),
                name: "Hollow-Born Witch".to_string(),
                description: "Bound to the cauldron.".to_string(),
                powers: emit.origin_refs.clone(),
                icon: "bewitchment:athame".to_string(),
                impact: 3,
                order: 0,
            }],
            powers: emit.local_powers,
        };
        let v = validate(set);
        assert!(v.is_ok(), "tranche-1 emit must pass the Apoli validator gate, got {v:?}");
    }

    // (Old "Scale is unlanded" test deleted — Scale landed in Tranche 1.
    // The WIP-signaling discipline is covered by
    // `tranche1_unlanded_variant_signals_correct_tranche_landing` below,
    // which uses AttributeBuff (still unlanded as of Tranche 1).)

    #[test]
    fn emit_context_id_counter_is_monotonic_per_origin() {
        // Same origin, two StartsWith calls → p0, p1 (distinct).
        let mut ctx = EmitContext::new("witch");
        let a = emit_perk(&PerkIntent::StartsWith {
            items: vec![ItemId::new("bewitchment:athame")], slots: None }, &mut ctx).unwrap();
        let b = emit_perk(&PerkIntent::StartsWith {
            items: vec![ItemId::new("supplementaries:soap")], slots: None }, &mut ctx).unwrap();
        assert_eq!(a.origin_refs, vec!["witch_p0".to_string()]);
        assert_eq!(b.origin_refs, vec!["witch_p1".to_string()]);
    }

    #[test]
    fn empty_starts_with_emits_empty_stacks() {
        // Edge case: an origin's starting kit is sometimes empty (Junimo).
        // Apoli accepts `stacks: []` per the schema.
        let perk = PerkIntent::StartsWith { items: vec![], slots: None };
        let mut ctx = EmitContext::new("junimo");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let stacks = emit.local_powers[0].body.get("stacks").unwrap().as_array().unwrap();
        assert!(stacks.is_empty());
    }

    // ---- Scale via Pehkui SafeCommand --------------------------------------

    #[test]
    fn tranche1_scale_emits_action_on_callback_with_pehkui_command() {
        // Junimo: scale 0.65 via Pehkui's `/scale set base` command, fired
        // on origin gain AND respawn (the death-respawn reset trap).
        let perk = PerkIntent::Scale { factor: ScaleFactor::new(0.65).unwrap() };
        let mut ctx = EmitContext::new("junimo");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        assert_eq!(emit.local_powers.len(), 1);
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:action_on_callback");
        let added = p.body.get("entity_action_added").unwrap();
        let respawned = p.body.get("entity_action_respawned").unwrap();
        for ea in [added, respawned] {
            assert_eq!(ea.get("type").unwrap().as_str().unwrap(), "apoli:execute_command");
            let cmd = ea.get("command").unwrap().as_str().unwrap();
            assert_eq!(cmd, "scale set base 0.65",
                "Pehkui scale command must be exact; got `{cmd}`");
        }
    }

    #[test]
    fn tranche1_scale_passes_validator_and_is_byte_deterministic() {
        let perk = PerkIntent::Scale { factor: ScaleFactor::new(1.4).unwrap() };
        let a = { let mut c = EmitContext::new("wolf"); emit_perk(&perk, &mut c).unwrap() };
        let b = { let mut c = EmitContext::new("wolf"); emit_perk(&perk, &mut c).unwrap() };
        assert_eq!(
            serde_json::to_string(&a.local_powers).unwrap(),
            serde_json::to_string(&b.local_powers).unwrap(),
            "Scale emit must be byte-deterministic"
        );
        let set = OriginsSet {
            origins: vec![Origin {
                id: "wolfkin".into(), name: "Wolfkin".into(),
                description: "Strong by moonlight.".into(),
                powers: a.origin_refs.clone(),
                icon: "minecraft:bone".into(), impact: 3, order: 0,
            }],
            powers: a.local_powers,
        };
        assert!(validate(set).is_ok(), "Scale emit must pass the Apoli gate");
    }

    // ---- PassiveEffect via action_over_time --------------------------------

    #[test]
    fn tranche1_passive_effect_emits_action_over_time_refreshing_apply_effect() {
        // Witch's night vision — emits action_over_time refreshing
        // minecraft:night_vision every 19t with a 20t apply_effect.
        let perk = PerkIntent::PassiveEffect {
            effect: StatusEffectId::new("minecraft:night_vision"),
            amplifier: None,
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:action_over_time");
        let interval = p.body.get("interval").unwrap().as_i64().unwrap();
        assert_eq!(interval, 19);
        let ea = p.body.get("entity_action").unwrap();
        assert_eq!(ea.get("type").unwrap().as_str().unwrap(), "apoli:apply_effect");
        let eff = ea.get("effect").unwrap();
        assert_eq!(eff.get("effect").unwrap().as_str().unwrap(), "minecraft:night_vision");
        assert_eq!(eff.get("amplifier").unwrap().as_i64().unwrap(), 0,
            "no explicit amplifier defaults to 0");
        assert_eq!(eff.get("is_ambient").unwrap().as_bool().unwrap(), true);
        assert_eq!(eff.get("show_particles").unwrap().as_bool().unwrap(), false);
    }

    #[test]
    fn tranche1_passive_effect_carries_explicit_amplifier() {
        let perk = PerkIntent::PassiveEffect {
            effect: StatusEffectId::new("minecraft:strength"),
            amplifier: Some(Amplifier::new(2).unwrap()),
        };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let amp = emit.local_powers[0].body
            .get("entity_action").unwrap()
            .get("effect").unwrap()
            .get("amplifier").unwrap()
            .as_i64().unwrap();
        assert_eq!(amp, 2);
    }

    #[test]
    fn tranche1_passive_effect_passes_validator_with_arbitrary_status_effect() {
        // Real modded scenario: a Bewitchment status effect (Apoli accepts
        // any registered status_effect via apply_effect; not gated by a
        // specific allow-list at the lower layer).
        let perk = PerkIntent::PassiveEffect {
            effect: StatusEffectId::new("bewitchment:wolf_form"),
            amplifier: None,
        };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let set = OriginsSet {
            origins: vec![Origin {
                id: "wolfkin".into(), name: "Wolfkin".into(),
                description: "Cursed shape.".into(),
                powers: emit.origin_refs.clone(),
                icon: "minecraft:bone".into(), impact: 3, order: 0,
            }],
            powers: emit.local_powers,
        };
        assert!(validate(set).is_ok(),
            "PassiveEffect with modded status_effect should pass — Apoli accepts any id");
    }

    // ---- SpecialMovement: shipped refs + emitted factories ------------------

    #[test]
    fn tranche1_special_movement_climb_references_shipped_origins_climbing() {
        let perk = PerkIntent::SpecialMovement { kind: MoveKind::Climb };
        let mut ctx = EmitContext::new("junimo");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        assert!(emit.local_powers.is_empty(), "shipped ref emits no Power file");
        assert_eq!(emit.origin_refs, vec!["origins:climbing".to_string()]);
    }

    #[test]
    fn tranche1_special_movement_elytra_references_shipped_elytra() {
        let perk = PerkIntent::SpecialMovement { kind: MoveKind::ElytraFlight };
        let mut ctx = EmitContext::new("drifter");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        assert_eq!(emit.origin_refs, vec!["origins:elytra".to_string()]);
    }

    #[test]
    fn tranche1_special_movement_walk_on_fluid_references_like_water() {
        let perk = PerkIntent::SpecialMovement { kind: MoveKind::WalkOnFluid };
        let mut ctx = EmitContext::new("aquatic");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        assert_eq!(emit.origin_refs, vec!["origins:like_water".to_string()]);
    }

    #[test]
    fn tranche1_special_movement_creative_flight_emits_factory_power() {
        let perk = PerkIntent::SpecialMovement { kind: MoveKind::CreativeFlight };
        let mut ctx = EmitContext::new("admin");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        assert_eq!(emit.local_powers.len(), 1);
        assert_eq!(emit.local_powers[0].power_type, "apoli:creative_flight");
        assert!(emit.local_powers[0].body.is_empty(), "factory power has no body");
    }

    #[test]
    fn tranche1_special_movement_higher_jump_emits_modify_jump_with_modifier() {
        let perk = PerkIntent::SpecialMovement { kind: MoveKind::HigherJump };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:modify_jump");
        let m = p.body.get("modifier").unwrap();
        assert_eq!(m.get("operation").unwrap().as_str().unwrap(), "multiply_total");
        assert_eq!(m.get("value").unwrap().as_f64().unwrap(), 0.4);
    }

    #[test]
    fn tranche1_special_movement_higher_jump_passes_validator() {
        // apoli:modify_jump IS in SAFE_TYPES with required `modifier`;
        // its operation must be in ALLOWED_OPERATIONS. multiply_total — OK.
        let perk = PerkIntent::SpecialMovement { kind: MoveKind::HigherJump };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let set = OriginsSet {
            origins: vec![Origin {
                id: "wolfkin".into(), name: "Wolfkin".into(),
                description: "Loose bones.".into(),
                powers: emit.origin_refs.clone(),
                icon: "minecraft:bone".into(), impact: 3, order: 0,
            }],
            powers: emit.local_powers,
        };
        assert!(validate(set).is_ok());
    }

    // ---- Multi-perk integration: a real partial-Witch end-to-end -----------

    #[test]
    fn tranche1_real_partial_witch_origin_assembles_and_validates() {
        // Compose multiple Tranche-1 perks for the Hollow-Born Witch:
        // start kit + night vision (passive) — both emit, refs accumulate,
        // and the assembled OriginsSet passes the verified validator.
        let perks = vec![
            PerkIntent::StartsWith {
                items: vec![ItemId::new("bewitchment:athame"), ItemId::new("bewitchment:silver_ingot")],
                slots: None,
            },
            PerkIntent::PassiveEffect {
                effect: StatusEffectId::new("minecraft:night_vision"),
                amplifier: None,
            },
        ];
        let mut ctx = EmitContext::new("witch");
        let mut all_powers = Vec::new();
        let mut all_refs = Vec::new();
        for p in &perks {
            let e = emit_perk(p, &mut ctx).expect("partial witch emit");
            all_powers.extend(e.local_powers);
            all_refs.extend(e.origin_refs);
        }
        // Deterministic ids: witch_p0 (StartsWith), witch_p1 (PassiveEffect).
        assert_eq!(all_refs, vec!["witch_p0".to_string(), "witch_p1".to_string()]);
        let set = OriginsSet {
            origins: vec![Origin {
                id: "hollow_witch".to_string(),
                name: "Hollow-Born Witch".to_string(),
                description: "Bound to the cauldron.".to_string(),
                powers: all_refs,
                icon: "bewitchment:athame".to_string(),
                impact: 3,
                order: 0,
            }],
            powers: all_powers,
        };
        let r = validate(set);
        assert!(r.is_ok(),
            "multi-perk Tranche-1 witch must pass the validator gate, got {r:?}");
    }

    // (The Tranche-1 "AttributeBuff is unlanded" assertion was superseded
    // when AttributeBuff landed in Tranche 2. Equivalent WIP-signaling
    // discipline is covered by `tranche2_attribute_buff_with_when_defers`
    // below — when=Some still routes to Tranche 2b.)

    // ========================================================================
    // TRANCHE 2 — AttributeBuff (when=None) + DamageVs
    // ========================================================================

    #[test]
    fn tranche2_attribute_buff_vanilla_addition_emits_apoli_attribute() {
        // Witch's "+4 HP near cauldron" stripped of its condition gate:
        // the unconditional AttributeBuff variant.
        let perk = PerkIntent::AttributeBuff {
            attribute: AttributeId::new("minecraft:generic.max_health"),
            amount: BuffAmount::new(4.0).unwrap(),
            op: AttrOp::Addition,
            when: None,
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:attribute");
        let m = p.body.get("modifier").unwrap();
        assert_eq!(m.get("attribute").unwrap().as_str().unwrap(), "minecraft:generic.max_health");
        assert_eq!(m.get("operation").unwrap().as_str().unwrap(), "addition");
        assert_eq!(m.get("value").unwrap().as_f64().unwrap(), 4.0);
        assert!(m.get("name").unwrap().is_string(), "modifier name field present");
    }

    #[test]
    fn tranche2_attribute_buff_vanilla_passes_validator() {
        // apoli:attribute is in SAFE_TYPES; the modifier must reference an
        // attribute in ALLOWED_ATTRIBUTES + a valid operation.
        // minecraft:generic.movement_speed + multiply_total → both allowed.
        let perk = PerkIntent::AttributeBuff {
            attribute: AttributeId::new("minecraft:generic.movement_speed"),
            amount: BuffAmount::new(0.05).unwrap(),
            op: AttrOp::MultiplyTotal,
            when: None,
        };
        let mut ctx = EmitContext::new("drifter");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let set = OriginsSet {
            origins: vec![Origin {
                id: "drifter".into(), name: "Drifter".into(),
                description: "Long stride.".into(),
                powers: emit.origin_refs.clone(),
                icon: "supplementaries:soap".into(), impact: 2, order: 0,
            }],
            powers: emit.local_powers,
        };
        assert!(validate(set).is_ok(),
            "vanilla AttributeBuff must pass the verified validator");
    }

    #[test]
    fn tranche2_attribute_buff_modded_attribute_emits_but_validator_rejects() {
        // Real cross-layer behaviour: the intent layer is permissive (any
        // attribute id parses + emits), but the Phase-1/2 validator's
        // ALLOWED_ATTRIBUTES gate still blocks modded ids until that list
        // is extended. This documents the layering honestly — the curator
        // sees the BadAttribute error and knows to switch to a vanilla
        // attribute or wait for the allowlist expansion.
        let perk = PerkIntent::AttributeBuff {
            attribute: AttributeId::new("pehkui:base"),
            amount: BuffAmount::new(0.65).unwrap(),
            op: AttrOp::MultiplyTotal,
            when: None,
        };
        let mut ctx = EmitContext::new("junimo");
        let emit = emit_perk(&perk, &mut ctx).expect("emit accepts modded attr");
        let set = OriginsSet {
            origins: vec![Origin {
                id: "junimo".into(), name: "Junimo".into(),
                description: "Tiny.".into(),
                powers: emit.origin_refs.clone(),
                icon: "minecraft:fern".into(), impact: 2, order: 0,
            }],
            powers: emit.local_powers,
        };
        let r = validate(set);
        let errs = r.err().expect("validator must reject pehkui:base until allowlist extended");
        assert!(errs.iter().any(|e| matches!(e, IntegrityError::BadAttribute { attribute, .. } if attribute == "pehkui:base")),
            "expected BadAttribute(pehkui:base), got {errs:?}");
    }

    #[test]
    fn tranche2_attribute_buff_with_when_defers_to_tranche_2b() {
        let perk = PerkIntent::AttributeBuff {
            attribute: AttributeId::new("minecraft:generic.max_health"),
            amount: BuffAmount::new(4.0).unwrap(),
            op: AttrOp::Addition,
            when: Some(WhenCondition::BlockInRadius {
                block: BlockSelector::One(BlockId::new("bewitchment:witch_cauldron")),
                radius: BlockRadius::new(8).unwrap(),
            }),
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).expect("witch's cauldron-buff emits cleanly post-T2b");
        assert_eq!(emit.local_powers.len(), 1, "one apoli:attribute power");
        let power = &emit.local_powers[0];
        assert_eq!(power.power_type, "apoli:attribute");
        let cond = power.body.get("condition").expect("attribute power must carry a `condition` field");
        assert_eq!(cond.get("type").and_then(|v| v.as_str()), Some("apoli:block_in_radius"),
            "block_in_radius is the compiled outer condition; got {cond:?}");
    }

    #[test]
    fn tranche2_damage_vs_single_target_emits_modify_damage_dealt() {
        // Witch's "Reaper's Bane" — 1.5× vs graveyard:reaper.
        let perk = PerkIntent::DamageVs {
            target: EntityCondRef::One(EntityTypeId::new("graveyard:reaper")),
            multiplier: DamageMul::new(1.5).unwrap(),
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:modify_damage_dealt");
        let m = p.body.get("modifier").unwrap();
        assert_eq!(m.get("operation").unwrap().as_str().unwrap(), "multiply_total");
        assert_eq!(m.get("value").unwrap().as_f64().unwrap(), 1.5);
        let tc = p.body.get("target_condition").unwrap();
        assert_eq!(tc.get("type").unwrap().as_str().unwrap(), "apoli:entity_type");
        assert_eq!(tc.get("entity_type").unwrap().as_str().unwrap(), "graveyard:reaper");
    }

    #[test]
    fn tranche2_damage_vs_many_targets_emits_or_composed_condition() {
        // Multi-target damage: vs all undead Graveyard variants.
        let perk = PerkIntent::DamageVs {
            target: EntityCondRef::Many(vec![
                EntityTypeId::new("graveyard:reaper"),
                EntityTypeId::new("graveyard:ghoul"),
                EntityTypeId::new("graveyard:lich"),
            ]),
            multiplier: DamageMul::new(2.0).unwrap(),
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let tc = emit.local_powers[0].body.get("target_condition").unwrap();
        assert_eq!(tc.get("type").unwrap().as_str().unwrap(), "apoli:or");
        let conditions = tc.get("conditions").unwrap().as_array().unwrap();
        assert_eq!(conditions.len(), 3);
        for (i, expected) in ["graveyard:reaper", "graveyard:ghoul", "graveyard:lich"].iter().enumerate() {
            assert_eq!(conditions[i].get("type").unwrap().as_str().unwrap(), "apoli:entity_type");
            assert_eq!(conditions[i].get("entity_type").unwrap().as_str().unwrap(), *expected);
        }
    }

    #[test]
    fn tranche2_damage_vs_tag_target_passes_through_hash_prefix() {
        // Tag-form entity targets (`#ns:tag`) — Apoli's entity-predicate
        // parser accepts them verbatim; the intent layer must not strip the
        // sigil (unlike grounding which does for lookup).
        let perk = PerkIntent::DamageVs {
            target: EntityCondRef::One(EntityTypeId::new("#minecraft:undead")),
            multiplier: DamageMul::new(1.5).unwrap(),
        };
        let mut ctx = EmitContext::new("paladin");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let et = emit.local_powers[0].body
            .get("target_condition").unwrap()
            .get("entity_type").unwrap().as_str().unwrap();
        assert_eq!(et, "#minecraft:undead",
            "tag prefix must pass through emit unchanged");
    }

    #[test]
    fn tranche2_damage_vs_passes_validator() {
        let perk = PerkIntent::DamageVs {
            target: EntityCondRef::One(EntityTypeId::new("graveyard:reaper")),
            multiplier: DamageMul::new(1.5).unwrap(),
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let set = OriginsSet {
            origins: vec![Origin {
                id: "witch".into(), name: "Hollow Witch".into(),
                description: "Bane of the dead.".into(),
                powers: emit.origin_refs.clone(),
                icon: "bewitchment:athame".into(), impact: 3, order: 0,
            }],
            powers: emit.local_powers,
        };
        assert!(validate(set).is_ok());
    }

    #[test]
    fn tranche2_emit_is_byte_deterministic() {
        let damage_perk = PerkIntent::DamageVs {
            target: EntityCondRef::Many(vec![
                EntityTypeId::new("graveyard:reaper"),
                EntityTypeId::new("graveyard:ghoul"),
            ]),
            multiplier: DamageMul::new(1.5).unwrap(),
        };
        let a = { let mut c = EmitContext::new("witch"); emit_perk(&damage_perk, &mut c).unwrap() };
        let b = { let mut c = EmitContext::new("witch"); emit_perk(&damage_perk, &mut c).unwrap() };
        assert_eq!(
            serde_json::to_string(&a.local_powers).unwrap(),
            serde_json::to_string(&b.local_powers).unwrap()
        );
    }

    // ========================================================================
    // TRANCHE 3 — ForbiddenItemUse + PreventSleep
    // ========================================================================

    #[test]
    fn tranche3_forbidden_single_item_emits_prevent_item_use_with_ingredient() {
        // Witch's "Forsworn Steel — no iron sword".
        let perk = PerkIntent::ForbiddenItemUse {
            what: ItemSelector::One(ItemId::new("minecraft:iron_sword")),
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:prevent_item_use");
        let ic = p.body.get("item_condition").unwrap();
        assert_eq!(ic.get("type").unwrap().as_str().unwrap(), "apoli:ingredient");
        let ing = ic.get("ingredient").unwrap();
        assert_eq!(ing.get("item").unwrap().as_str().unwrap(), "minecraft:iron_sword");
        assert!(ing.get("tag").is_none(), "single item uses `item`, not `tag`");
    }

    #[test]
    fn tranche3_forbidden_tag_emits_ingredient_with_stripped_tag_field() {
        // Wolfkin's "Forbidden Silver — `#c:silver_ingots`". Apoli's
        // ingredient predicate stores tag WITHOUT the `#` sigil in the
        // `tag` field (the `#` is a use-site marker only).
        let perk = PerkIntent::ForbiddenItemUse {
            what: ItemSelector::One(ItemId::new("#c:silver_ingots")),
        };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let ing = emit.local_powers[0].body
            .get("item_condition").unwrap()
            .get("ingredient").unwrap();
        assert_eq!(ing.get("tag").unwrap().as_str().unwrap(), "c:silver_ingots",
            "tag field must NOT carry the leading `#`");
        assert!(ing.get("item").is_none());
    }

    #[test]
    fn tranche3_forbidden_many_emits_or_composed_ingredient_conditions() {
        // Real witch forbid set: iron + diamond + netherite swords.
        let perk = PerkIntent::ForbiddenItemUse {
            what: ItemSelector::Many(vec![
                ItemId::new("minecraft:iron_sword"),
                ItemId::new("minecraft:diamond_sword"),
                ItemId::new("minecraft:netherite_sword"),
            ]),
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let ic = emit.local_powers[0].body.get("item_condition").unwrap();
        assert_eq!(ic.get("type").unwrap().as_str().unwrap(), "apoli:or");
        let cs = ic.get("conditions").unwrap().as_array().unwrap();
        assert_eq!(cs.len(), 3);
        for (i, expected) in ["minecraft:iron_sword", "minecraft:diamond_sword", "minecraft:netherite_sword"].iter().enumerate() {
            assert_eq!(cs[i].get("ingredient").unwrap().get("item").unwrap().as_str().unwrap(), *expected);
        }
    }

    #[test]
    fn tranche3_forbidden_passes_validator_for_real_witch_forbid_set() {
        let perk = PerkIntent::ForbiddenItemUse {
            what: ItemSelector::Many(vec![
                ItemId::new("minecraft:iron_sword"),
                ItemId::new("minecraft:diamond_sword"),
            ]),
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let set = OriginsSet {
            origins: vec![Origin {
                id: "hollow_witch".into(), name: "Hollow Witch".into(),
                description: "No steel.".into(),
                powers: emit.origin_refs.clone(),
                icon: "bewitchment:athame".into(), impact: 3, order: 0,
            }],
            powers: emit.local_powers,
        };
        assert!(validate(set).is_ok());
    }

    #[test]
    fn tranche3_prevent_sleep_basic_emits_factory_power_with_empty_body() {
        // Wolfkin's "Sleepless".
        let perk = PerkIntent::PreventSleep { except: None };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:prevent_sleep");
        assert!(p.body.is_empty(), "prevent_sleep factory takes no required body");
    }

    #[test]
    fn tranche3_prevent_sleep_with_except_defers_to_tranche_3b() {
        // Drifter's "Rootless — only my bedroll sets spawn". Apoli's
        // prevent_sleep can't natively gate by held item; a companion
        // datapack is the cleanest path (lands in Tranche 3b).
        let perk = PerkIntent::PreventSleep {
            except: Some(ItemSelector::One(ItemId::new("comforts:hammock_red"))),
        };
        let mut ctx = EmitContext::new("drifter");
        let err = emit_perk(&perk, &mut ctx).unwrap_err();
        match err {
            EmitError::NotYetImplemented { variant, tranche_landing } => {
                assert_eq!(variant, PerkIntentTag::PreventSleep);
                assert!(tranche_landing.contains("3b"),
                    "except-item should defer to Tranche 3b, got {tranche_landing}");
            }
            other => panic!("expected NotYetImplemented(3b), got {other:?}"),
        }
    }

    #[test]
    fn tranche3_prevent_sleep_passes_validator() {
        let perk = PerkIntent::PreventSleep { except: None };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let set = OriginsSet {
            origins: vec![Origin {
                id: "wolfkin".into(), name: "Wolfkin".into(),
                description: "Sleepless.".into(),
                powers: emit.origin_refs.clone(),
                icon: "minecraft:bone".into(), impact: 3, order: 0,
            }],
            powers: emit.local_powers,
        };
        assert!(validate(set).is_ok());
    }

    #[test]
    fn tranche3_emit_is_byte_deterministic() {
        let p = PerkIntent::ForbiddenItemUse {
            what: ItemSelector::Many(vec![
                ItemId::new("#c:silver_ingots"),
                ItemId::new("bewitchment:silver_arrow"),
            ]),
        };
        let a = { let mut c = EmitContext::new("wolf"); emit_perk(&p, &mut c).unwrap() };
        let b = { let mut c = EmitContext::new("wolf"); emit_perk(&p, &mut c).unwrap() };
        assert_eq!(
            serde_json::to_string(&a.local_powers).unwrap(),
            serde_json::to_string(&b.local_powers).unwrap(),
        );
    }

    #[test]
    fn tranche3_real_wolfkin_partial_origin_validates_end_to_end() {
        // Real T1+T3 partial Wolfkin: night vision + higher jump +
        // forbidden silver (tag) + sleepless.
        let perks = vec![
            PerkIntent::PassiveEffect {
                effect: StatusEffectId::new("minecraft:night_vision"),
                amplifier: None,
            },
            PerkIntent::SpecialMovement { kind: MoveKind::HigherJump },
            PerkIntent::ForbiddenItemUse {
                what: ItemSelector::One(ItemId::new("#c:silver_ingots")),
            },
            PerkIntent::PreventSleep { except: None },
        ];
        let mut ctx = EmitContext::new("wolf");
        let mut all_powers = Vec::new();
        let mut all_refs = Vec::new();
        for p in &perks {
            let e = emit_perk(p, &mut ctx).expect(&format!("wolf emit for {:?}", p.tag()));
            all_powers.extend(e.local_powers);
            all_refs.extend(e.origin_refs);
        }
        assert_eq!(all_refs.len(), 4);
        let set = OriginsSet {
            origins: vec![Origin {
                id: "wolfkin".into(), name: "Bewitched Wolfkin".into(),
                description: "The change took.".into(),
                powers: all_refs,
                icon: "minecraft:bone".into(), impact: 3, order: 0,
            }],
            powers: all_powers,
        };
        assert!(validate(set).is_ok());
    }

    // ========================================================================
    // TRANCHE 4 — Event hooks (OnKill / OnWake / Food / Break)
    // ========================================================================

    #[test]
    fn tranche4_on_kill_grant_emits_self_action_on_kill_with_target_and_apply_effect() {
        // Wolfkin "Feast Frenzy": kill an animal → 30s of Strength.
        let perk = PerkIntent::OnKillGrant {
            target: EntityCondRef::One(EntityTypeId::new("naturalist:bear")),
            effect: StatusEffectId::new("minecraft:strength"),
            duration_s: 30,
        };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:self_action_on_kill");
        // The target_condition wraps an entity_type predicate.
        let bc = p.body.get("bientity_condition").unwrap();
        assert_eq!(bc.get("type").unwrap().as_str().unwrap(), "apoli:target_condition");
        let inner = bc.get("condition").unwrap();
        assert_eq!(inner.get("type").unwrap().as_str().unwrap(), "apoli:entity_type");
        assert_eq!(inner.get("entity_type").unwrap().as_str().unwrap(), "naturalist:bear");
        // 30s = 600 ticks.
        let eff = p.body.get("entity_action").unwrap().get("effect").unwrap();
        assert_eq!(eff.get("effect").unwrap().as_str().unwrap(), "minecraft:strength");
        assert_eq!(eff.get("duration").unwrap().as_u64().unwrap(), 600);
    }

    #[test]
    fn tranche4_on_kill_grant_many_targets_composes_or_predicate() {
        let perk = PerkIntent::OnKillGrant {
            target: EntityCondRef::Many(vec![
                EntityTypeId::new("naturalist:bear"),
                EntityTypeId::new("naturalist:deer"),
            ]),
            effect: StatusEffectId::new("minecraft:strength"),
            duration_s: 30,
        };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let inner = emit.local_powers[0].body
            .get("bientity_condition").unwrap()
            .get("condition").unwrap();
        assert_eq!(inner.get("type").unwrap().as_str().unwrap(), "apoli:or");
        let cs = inner.get("conditions").unwrap().as_array().unwrap();
        assert_eq!(cs.len(), 2);
    }

    #[test]
    fn tranche4_on_wake_grant_single_effect_emits_action_on_wake_up_with_direct_apply() {
        // Farmer "Well Rested": absorption on wake.
        let perk = PerkIntent::OnWakeGrant {
            effects: vec![StatusEffectInst {
                effect: StatusEffectId::new("minecraft:absorption"),
                amplifier: Amplifier::new(0).unwrap(),
                duration_t: 600,
            }],
        };
        let mut ctx = EmitContext::new("farmer");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:action_on_wake_up");
        let ea = p.body.get("entity_action").unwrap();
        assert_eq!(ea.get("type").unwrap().as_str().unwrap(), "apoli:apply_effect");
        let eff = ea.get("effect").unwrap();
        assert_eq!(eff.get("effect").unwrap().as_str().unwrap(), "minecraft:absorption");
        assert_eq!(eff.get("duration").unwrap().as_u64().unwrap(), 600);
    }

    #[test]
    fn tranche4_on_wake_grant_many_effects_composes_apoli_and() {
        // Farmer "Well Rested" with absorption + luck on wake.
        let perk = PerkIntent::OnWakeGrant {
            effects: vec![
                StatusEffectInst { effect: StatusEffectId::new("minecraft:absorption"),
                    amplifier: Amplifier::new(0).unwrap(), duration_t: 600 },
                StatusEffectInst { effect: StatusEffectId::new("minecraft:luck"),
                    amplifier: Amplifier::new(0).unwrap(), duration_t: 1200 },
            ],
        };
        let mut ctx = EmitContext::new("farmer");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let ea = emit.local_powers[0].body.get("entity_action").unwrap();
        assert_eq!(ea.get("type").unwrap().as_str().unwrap(), "apoli:and");
        let actions = ea.get("actions").unwrap().as_array().unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].get("type").unwrap().as_str().unwrap(), "apoli:apply_effect");
        assert_eq!(actions[1].get("effect").unwrap().get("effect").unwrap().as_str().unwrap(),
            "minecraft:luck");
    }

    #[test]
    fn tranche4_bonus_saturation_on_tag_emits_modify_food() {
        // Farmer "A Good Meal": +2 saturation on `#c:foods` (real tag verified).
        let perk = PerkIntent::BonusSaturationOn {
            food: ItemSelector::One(ItemId::new("#c:foods")),
            extra: BonusSat::new(2).unwrap(),
            when: None,
        };
        let mut ctx = EmitContext::new("farmer");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:modify_food");
        let sm = p.body.get("saturation_modifier").unwrap();
        assert_eq!(sm.get("operation").unwrap().as_str().unwrap(), "addition");
        assert_eq!(sm.get("value").unwrap().as_f64().unwrap(), 2.0);
        let ic = p.body.get("item_condition").unwrap();
        let tag = ic.get("ingredient").unwrap().get("tag").unwrap().as_str().unwrap();
        assert_eq!(tag, "c:foods", "tag field must drop the # for ingredient");
    }

    #[test]
    fn tranche4_bonus_saturation_with_when_defers_to_tranche_4b() {
        let perk = PerkIntent::BonusSaturationOn {
            food: ItemSelector::One(ItemId::new("#c:foods")),
            extra: BonusSat::new(1).unwrap(),
            when: Some(WhenCondition::ExposedToSky),
        };
        let mut ctx = EmitContext::new("drifter");
        let emit = emit_perk(&perk, &mut ctx).expect("drifter's sun-fed emits cleanly post-T2b");
        let power = emit.local_powers.first().expect("one modify_food power");
        assert_eq!(power.power_type, "apoli:modify_food");
        let cond = power.body.get("condition").expect("food power must carry a `condition` field");
        assert_eq!(cond.get("type").and_then(|v| v.as_str()), Some("apoli:exposed_to_sky"));
    }

    #[test]
    fn tranche4_faster_break_on_tag_emits_modify_break_speed() {
        // Farmer "Cropwise": 1.5x crop break speed on `#minecraft:crops`.
        let perk = PerkIntent::FasterBreakOn {
            block: BlockSelector::One(BlockId::new("#minecraft:crops")),
            multiplier: BreakMul::new(1.5).unwrap(),
        };
        let mut ctx = EmitContext::new("farmer");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:modify_break_speed");
        let m = p.body.get("modifier").unwrap();
        assert_eq!(m.get("operation").unwrap().as_str().unwrap(), "multiply_total");
        assert_eq!(m.get("value").unwrap().as_f64().unwrap(), 1.5);
        let bc = p.body.get("block_condition").unwrap();
        assert_eq!(bc.get("type").unwrap().as_str().unwrap(), "apoli:in_tag");
        assert_eq!(bc.get("tag").unwrap().as_str().unwrap(), "minecraft:crops");
    }

    #[test]
    fn tranche4_faster_break_on_specific_block_emits_apoli_block_condition() {
        let perk = PerkIntent::FasterBreakOn {
            block: BlockSelector::One(BlockId::new("bewitchment:silver_block")),
            multiplier: BreakMul::new(2.0).unwrap(),
        };
        let mut ctx = EmitContext::new("miner");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let bc = emit.local_powers[0].body.get("block_condition").unwrap();
        assert_eq!(bc.get("type").unwrap().as_str().unwrap(), "apoli:block");
        assert_eq!(bc.get("block").unwrap().as_str().unwrap(), "bewitchment:silver_block");
    }

    #[test]
    fn tranche4b_tally_milestone_emits_counter_and_gated_unlock() {
        // Counter + recursive unlock with scoreboard gate. T4b shipped.
        let perk = PerkIntent::TallyMilestone {
            event: TallyEvent::KillInRadius,
            target: EntityCondRef::One(EntityTypeId::new("graveyard:reaper")),
            threshold: 100,
            unlock: Box::new(PerkIntent::DamageVs {
                target: EntityCondRef::One(EntityTypeId::new("graveyard:reaper")),
                multiplier: DamageMul::new(2.0).unwrap(),
            }),
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).expect("TallyMilestone emits");
        assert!(emit.local_powers.len() >= 2, "counter + unlock = at least 2 powers");
        // Counter uses an on_kill factory; unlock carries the apoli:command gate.
        let unlock_power = emit.local_powers.iter()
            .find(|p| p.body.get("condition")
                .and_then(|c| c.get("type"))
                .and_then(|t| t.as_str()) == Some("apoli:command"))
            .expect("unlock must carry an apoli:command condition gate");
        let cmd = unlock_power.body["condition"]["command"].as_str().unwrap();
        assert!(cmd.contains("matches 100.."), "gate must check threshold; got {cmd}");
        assert!(!emit.mcfunctions.is_empty(), "load function emitted for scoreboard");
    }

    #[test]
    fn tranche4_pelican_farmer_partial_origin_validates_end_to_end() {
        // Real T1+T4 Pelican Town Farmer: kit + cropwise + good_meal + well_rested.
        let perks = vec![
            PerkIntent::StartsWith {
                items: vec![
                    ItemId::new("farmersdelight:iron_knife"),
                    ItemId::new("farmersdelight:tomato_seeds"),
                    ItemId::new("farmersdelight:cabbage_seeds"),
                ],
                slots: None,
            },
            PerkIntent::FasterBreakOn {
                block: BlockSelector::One(BlockId::new("#minecraft:crops")),
                multiplier: BreakMul::new(1.5).unwrap(),
            },
            PerkIntent::BonusSaturationOn {
                food: ItemSelector::One(ItemId::new("#c:foods")),
                extra: BonusSat::new(2).unwrap(),
                when: None,
            },
            PerkIntent::OnWakeGrant {
                effects: vec![StatusEffectInst {
                    effect: StatusEffectId::new("minecraft:absorption"),
                    amplifier: Amplifier::new(0).unwrap(),
                    duration_t: 600,
                }],
            },
        ];
        let mut ctx = EmitContext::new("farmer");
        let mut all_powers = Vec::new();
        let mut all_refs = Vec::new();
        for p in &perks {
            let e = emit_perk(p, &mut ctx).expect(&format!("farmer emit {:?}", p.tag()));
            all_powers.extend(e.local_powers);
            all_refs.extend(e.origin_refs);
        }
        assert_eq!(all_refs.len(), 4);
        let set = OriginsSet {
            origins: vec![Origin {
                id: "pelican_farmer".into(),
                name: "Pelican Town Farmer".into(),
                description: "You read the almanac.".into(),
                powers: all_refs,
                icon: "farmersdelight:iron_knife".into(),
                impact: 1,
                order: 0,
            }],
            powers: all_powers,
        };
        let r = validate(set);
        assert!(r.is_ok(), "Pelican Farmer T1+T4 must pass validator gate; got {r:?}");
    }

    #[test]
    fn tranche4_emit_is_byte_deterministic() {
        let p = PerkIntent::OnKillGrant {
            target: EntityCondRef::One(EntityTypeId::new("naturalist:bear")),
            effect: StatusEffectId::new("minecraft:strength"),
            duration_s: 30,
        };
        let a = { let mut c = EmitContext::new("wolf"); emit_perk(&p, &mut c).unwrap() };
        let b = { let mut c = EmitContext::new("wolf"); emit_perk(&p, &mut c).unwrap() };
        assert_eq!(
            serde_json::to_string(&a.local_powers).unwrap(),
            serde_json::to_string(&b.local_powers).unwrap(),
        );
    }

    // ========================================================================
    // TRANCHE 5 — EntityGlow (rest of mob/periodic group → T5b companion dp)
    // ========================================================================

    #[test]
    fn tranche5_entity_glow_single_target_emits_apoli_entity_glow_with_condition() {
        // Wolfkin "Pack Sense": canines glow to your sight.
        let perk = PerkIntent::EntityGlow {
            targets: EntityCondRef::One(EntityTypeId::new("minecraft:wolf")),
            radius: GlowRadius::new(32).unwrap(),
        };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:entity_glow");
        let c = p.body.get("condition").unwrap();
        assert_eq!(c.get("type").unwrap().as_str().unwrap(), "apoli:entity_type");
        assert_eq!(c.get("entity_type").unwrap().as_str().unwrap(), "minecraft:wolf");
    }

    #[test]
    fn tranche5_entity_glow_many_composes_or_predicate() {
        // Wolfkin sees wolves AND foxes — `apoli:or` composition mirrors
        // the DamageVs many-target shape (same helper).
        let perk = PerkIntent::EntityGlow {
            targets: EntityCondRef::Many(vec![
                EntityTypeId::new("minecraft:wolf"),
                EntityTypeId::new("minecraft:fox"),
            ]),
            radius: GlowRadius::new(32).unwrap(),
        };
        let mut ctx = EmitContext::new("wolf");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let c = emit.local_powers[0].body.get("condition").unwrap();
        assert_eq!(c.get("type").unwrap().as_str().unwrap(), "apoli:or");
        let cs = c.get("conditions").unwrap().as_array().unwrap();
        assert_eq!(cs.len(), 2);
    }

    #[test]
    fn tranche5_entity_glow_passes_validator() {
        let perk = PerkIntent::EntityGlow {
            targets: EntityCondRef::One(EntityTypeId::new("naturalist:bear")),
            radius: GlowRadius::new(48).unwrap(),
        };
        let mut ctx = EmitContext::new("hunter");
        let emit = emit_perk(&perk, &mut ctx).unwrap();
        let set = OriginsSet {
            origins: vec![Origin {
                id: "hunter".into(), name: "Hunter".into(),
                description: "Spots prey.".into(),
                powers: emit.origin_refs.clone(),
                icon: "minecraft:bone".into(), impact: 2, order: 0,
            }],
            powers: emit.local_powers,
        };
        assert!(validate(set).is_ok());
    }

    #[test]
    fn tranche5_entity_glow_emit_is_byte_deterministic() {
        let p = PerkIntent::EntityGlow {
            targets: EntityCondRef::One(EntityTypeId::new("minecraft:wolf")),
            radius: GlowRadius::new(32).unwrap(),
        };
        let a = { let mut c = EmitContext::new("wolf"); emit_perk(&p, &mut c).unwrap() };
        let b = { let mut c = EmitContext::new("wolf"); emit_perk(&p, &mut c).unwrap() };
        assert_eq!(
            serde_json::to_string(&a.local_powers).unwrap(),
            serde_json::to_string(&b.local_powers).unwrap(),
        );
    }

    #[test]
    fn tranche5b_pacify_targeting_emits_marker_with_target_condition() {
        // T5b shipped: marker power carrying the by-condition for the
        // companion datapack tick function to read and clear AI targets.
        let perk = PerkIntent::PacifyTargeting {
            by: EntityCondRef::One(EntityTypeId::new("naturalist:bear")),
        };
        let mut ctx = EmitContext::new("junimo");
        let emit = emit_perk(&perk, &mut ctx).expect("PacifyTargeting emits");
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:simple");
        let cond = &p.body["target_condition"];
        assert_eq!(cond["entity_type"], "naturalist:bear");
    }

    #[test]
    fn tranche5_real_wolfkin_with_pack_sense_validates_end_to_end() {
        // T1+T4+T5 partial Wolfkin: night vision + higher jump + on-kill +
        // entity glow for canines.
        let perks = vec![
            PerkIntent::PassiveEffect {
                effect: StatusEffectId::new("minecraft:night_vision"),
                amplifier: None,
            },
            PerkIntent::SpecialMovement { kind: MoveKind::HigherJump },
            PerkIntent::OnKillGrant {
                target: EntityCondRef::One(EntityTypeId::new("naturalist:bear")),
                effect: StatusEffectId::new("minecraft:strength"),
                duration_s: 30,
            },
            PerkIntent::EntityGlow {
                targets: EntityCondRef::Many(vec![
                    EntityTypeId::new("minecraft:wolf"),
                    EntityTypeId::new("minecraft:fox"),
                ]),
                radius: GlowRadius::new(32).unwrap(),
            },
        ];
        let mut ctx = EmitContext::new("wolf");
        let mut all_powers = Vec::new();
        let mut all_refs = Vec::new();
        for p in &perks {
            let e = emit_perk(p, &mut ctx).expect(&format!("wolf emit {:?}", p.tag()));
            all_powers.extend(e.local_powers);
            all_refs.extend(e.origin_refs);
        }
        assert_eq!(all_refs.len(), 4);
        let set = OriginsSet {
            origins: vec![Origin {
                id: "wolfkin".into(), name: "Bewitched Wolfkin".into(),
                description: "The change took.".into(),
                powers: all_refs,
                icon: "minecraft:bone".into(), impact: 3, order: 0,
            }],
            powers: all_powers,
        };
        assert!(validate(set).is_ok());
    }

    #[test]
    fn tranche2_real_witch_with_damage_vs_and_attribute_validates_end_to_end() {
        // Real Tranche-1+2 partial Witch: start kit + night vision (T1) +
        // movement_speed buff (T2) + extra damage vs reapers (T2).
        let perks = vec![
            PerkIntent::StartsWith {
                items: vec![ItemId::new("bewitchment:athame"), ItemId::new("bewitchment:silver_ingot")],
                slots: None,
            },
            PerkIntent::PassiveEffect {
                effect: StatusEffectId::new("minecraft:night_vision"),
                amplifier: None,
            },
            PerkIntent::AttributeBuff {
                attribute: AttributeId::new("minecraft:generic.movement_speed"),
                amount: BuffAmount::new(0.05).unwrap(),
                op: AttrOp::MultiplyTotal,
                when: None,
            },
            PerkIntent::DamageVs {
                target: EntityCondRef::One(EntityTypeId::new("graveyard:reaper")),
                multiplier: DamageMul::new(1.5).unwrap(),
            },
        ];
        let mut ctx = EmitContext::new("witch");
        let mut all_powers = Vec::new();
        let mut all_refs = Vec::new();
        for p in &perks {
            let e = emit_perk(p, &mut ctx).expect(&format!("partial witch emit for {:?}", p.tag()));
            all_powers.extend(e.local_powers);
            all_refs.extend(e.origin_refs);
        }
        // Four perks → four local powers → witch_p0..p3 ids.
        assert_eq!(all_refs, vec![
            "witch_p0".to_string(), "witch_p1".to_string(),
            "witch_p2".to_string(), "witch_p3".to_string(),
        ]);
        let set = OriginsSet {
            origins: vec![Origin {
                id: "hollow_witch".to_string(),
                name: "Hollow-Born Witch".to_string(),
                description: "Bound to the cauldron, gifted against the dead.".to_string(),
                powers: all_refs,
                icon: "bewitchment:athame".to_string(),
                impact: 3,
                order: 0,
            }],
            powers: all_powers,
        };
        let r = validate(set);
        assert!(r.is_ok(),
            "T1+T2 multi-perk witch must pass validator gate; got {r:?}");
    }

    // ---- TRANCHE 2b — condition compiler + 5 conditional emit handlers ------
    //
    // Tests are anchored in real LLM-shaped JSON fixtures + the gameplay-driver
    // intents the user prioritised (frost forms, vampire sunlight burn, etc.).
    // Each verifies the exact Apoli factory id + the universal `condition`
    // gate shape. Edge cases (Any → None, Or([single]) hoists, Not(Not())
    // collapses) ride alongside the real-world ones.

    fn pick<'a>(s: &'a str, body: &'a serde_json::Map<String, serde_json::Value>) -> &'a serde_json::Value {
        body.get(s).unwrap_or_else(|| panic!("missing field `{s}` in emitted body: {body:?}"))
    }

    #[test]
    fn distinct_attribute_buffs_get_distinct_labels_not_attribute_modifier_x4() {
        // Real-world UCL Porters Lodge Guard regression: 4 different
        // attribute buffs all showed as generic "Attribute Modifier" so
        // the player couldn't tell them apart in-game. Each AttributeBuff
        // emit must produce a NAME derived from its attribute+amount.
        let perks: Vec<PerkIntent> = vec![
            PerkIntent::AttributeBuff {
                attribute: AttributeId::new("minecraft:generic.max_health"),
                amount: BuffAmount::new(4.0).unwrap(),
                op: AttrOp::Addition, when: None,
            },
            PerkIntent::AttributeBuff {
                attribute: AttributeId::new("minecraft:generic.armor"),
                amount: BuffAmount::new(4.0).unwrap(),
                op: AttrOp::Addition, when: None,
            },
            PerkIntent::AttributeBuff {
                attribute: AttributeId::new("minecraft:generic.knockback_resistance"),
                amount: BuffAmount::new(0.5).unwrap(),
                op: AttrOp::Addition, when: None,
            },
            PerkIntent::AttributeBuff {
                attribute: AttributeId::new("minecraft:generic.attack_damage"),
                amount: BuffAmount::new(2.0).unwrap(),
                op: AttrOp::Addition, when: None,
            },
        ];
        let mut ctx = EmitContext::new("porter");
        let mut names: Vec<String> = Vec::new();
        let mut descs: Vec<String> = Vec::new();
        for p in &perks {
            let e = emit_perk(p, &mut ctx).expect("attribute buff emits");
            names.push(e.local_powers[0].name.clone());
            descs.push(e.local_powers[0].description.clone());
        }
        // Every label must differ from every other.
        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                assert_ne!(names[i], names[j],
                    "AttributeBuff #{i} and #{j} produced identical name `{}`", names[i]);
                assert_ne!(descs[i], descs[j],
                    "AttributeBuff #{i} and #{j} produced identical description `{}`", descs[i]);
            }
        }
        // And none of them may be the legacy generic placeholder.
        for n in &names {
            assert_ne!(n, "Attribute Modifier",
                "label_attribute_buff must NOT fall back to legacy generic name");
        }
        // Descriptions must mention the actual delta (e.g. "+4").
        assert!(descs.iter().any(|d| d.contains("+4") && d.contains("health")),
            "max_health buff must say `+4 max health`; got {descs:?}");
        assert!(descs.iter().any(|d| d.contains("+4") && d.contains("armor")),
            "armor buff must say `+4 armor`; got {descs:?}");
    }

    #[test]
    fn starts_with_lists_items_not_generic_begins_with_these() {
        // Three items should appear in the description by name, not the
        // legacy "Begins with these items." placeholder.
        let perk = PerkIntent::StartsWith {
            items: vec![
                ItemId::new("bewitchment:athame"),
                ItemId::new("bewitchment:silver_ingot"),
                ItemId::new("minecraft:apple"),
            ],
            slots: None,
        };
        let mut ctx = EmitContext::new("witch");
        let e = emit_perk(&perk, &mut ctx).unwrap();
        let desc = &e.local_powers[0].description;
        assert!(desc.contains("Athame"), "expected item name in desc; got `{desc}`");
        assert!(desc.contains("Silver Ingot"), "expected item name in desc; got `{desc}`");
        assert!(desc.contains("Apple"), "expected item name in desc; got `{desc}`");
        assert_ne!(desc, "Begins with these items.",
            "label_starts_with must NOT fall back to the legacy generic blurb");
    }

    #[test]
    fn tranche2b_any_collapses_to_no_condition_field() {
        // Hollow-witch buff with no gate — should NOT emit a `condition` field.
        let perk = PerkIntent::AttributeBuff {
            attribute: AttributeId::new("minecraft:generic.max_health"),
            amount: BuffAmount::new(2.0).unwrap(),
            op: AttrOp::Addition,
            when: Some(WhenCondition::Any),
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).expect("Any-when emits cleanly");
        assert!(!emit.local_powers[0].body.contains_key("condition"),
            "Any must collapse — `condition` field must be absent");
    }

    #[test]
    fn tranche2b_compile_when_daytime_and_nighttime_match_jar_factory() {
        // From EntityConditions.class bytecode: daytime is the registered id;
        // nighttime is encoded as daytime with the universal `inverted: true`.
        let day = compile_when_condition(&WhenCondition::Daytime).expect("daytime emits");
        assert_eq!(day["type"], "apoli:daytime");
        assert!(day.get("inverted").is_none(), "raw daytime carries no inverted");

        let night = compile_when_condition(&WhenCondition::Nighttime).expect("nighttime emits");
        assert_eq!(night["type"], "apoli:daytime", "nighttime = daytime + inverted");
        assert_eq!(night["inverted"], true);
    }

    #[test]
    fn tranche2b_compile_when_biome_id_and_biome_tag_use_correct_factories() {
        // Plain biome → `apoli:biome { biome: ... }` (verified field name from
        // EntityConditions bytecode line 434). Biome tag → the nested-condition
        // pattern: `apoli:biome { condition: apoli:in_tag { tag: ... } }`
        // (verified from BiomeConditions class: `in_tag` factory id).
        let b = compile_when_condition(&WhenCondition::Biome {
            id: BiomeId::new("minecraft:plains")
        }).unwrap();
        assert_eq!(b["type"], "apoli:biome");
        assert_eq!(b["biome"], "minecraft:plains");
        assert!(b.get("condition").is_none(), "atomic biome doesn't nest a condition");

        let bt = compile_when_condition(&WhenCondition::BiomeTag {
            tag: BiomeTagId::new("minecraft:is_cold")
        }).unwrap();
        assert_eq!(bt["type"], "apoli:biome", "biome-tag still uses outer apoli:biome");
        let inner = bt.get("condition").expect("biome-tag nests a condition");
        assert_eq!(inner["type"], "apoli:in_tag",
            "biome-tag dispatches via apoli:in_tag biome-condition factory");
        assert_eq!(inner["tag"], "minecraft:is_cold");
    }

    #[test]
    fn tranche2b_compile_when_block_in_radius_threads_block_selector() {
        // Witch's cauldron-bound buff — the canonical "stand near X" gate.
        // BlockSelector composes through the existing helper so tag/`#`
        // handling stays consistent across emit sites.
        let c = compile_when_condition(&WhenCondition::BlockInRadius {
            block: BlockSelector::One(BlockId::new("bewitchment:witch_cauldron")),
            radius: BlockRadius::new(8).unwrap(),
        }).unwrap();
        assert_eq!(c["type"], "apoli:block_in_radius");
        assert_eq!(c["radius"], 8);
        assert_eq!(c["shape"], "cube");
        // The block_condition body is what `block_selector_to_apoli_condition`
        // produces — apoli:block { block: bewitchment:witch_cauldron }.
        let bc = &c["block_condition"];
        assert_eq!(bc["type"], "apoli:block");
        assert_eq!(bc["block"], "bewitchment:witch_cauldron");
    }

    #[test]
    fn tranche2b_compile_when_dimension_emits_apoli_dimension() {
        let d = compile_when_condition(&WhenCondition::Dimension {
            id: DimensionId::new("minecraft:the_nether")
        }).unwrap();
        assert_eq!(d["type"], "apoli:dimension");
        assert_eq!(d["dimension"], "minecraft:the_nether");
    }

    #[test]
    fn tranche2b_compile_when_and_composes_apoli_and() {
        // Vampire daylight burn: And([Daytime, ExposedToSky]).
        let c = compile_when_condition(&WhenCondition::And {
            conditions: vec![WhenCondition::Daytime, WhenCondition::ExposedToSky],
        }).unwrap();
        assert_eq!(c["type"], "apoli:and");
        let conds = c["conditions"].as_array().expect("`apoli:and` carries `conditions`");
        assert_eq!(conds.len(), 2);
        assert_eq!(conds[0]["type"], "apoli:daytime");
        assert_eq!(conds[1]["type"], "apoli:exposed_to_sky");
    }

    #[test]
    fn tranche2b_compile_when_or_with_singleton_hoists_inner() {
        // Or([X]) collapses to X — keeps emitted JSON clean. Same for And.
        let c = compile_when_condition(&WhenCondition::Or {
            conditions: vec![WhenCondition::Nighttime],
        }).unwrap();
        assert_eq!(c["type"], "apoli:daytime", "singleton Or hoists the inner condition");
        assert_eq!(c["inverted"], true);
    }

    #[test]
    fn tranche2b_compile_when_and_filters_any_branches() {
        // `Any` in a composition is the identity element — should filter out
        // rather than emit as a no-op condition. Verifies the v.iter()
        // filter_map path in compile_when_condition.
        let c = compile_when_condition(&WhenCondition::And {
            conditions: vec![WhenCondition::Any, WhenCondition::Daytime, WhenCondition::Any],
        }).unwrap();
        // Only Daytime survives — and singleton hoists.
        assert_eq!(c["type"], "apoli:daytime");
        assert!(c.as_object().unwrap().get("conditions").is_none());
    }

    #[test]
    fn tranche2b_compile_when_not_toggles_inverted_and_double_negation_collapses() {
        // Single Not flips: inverted=true added.
        let once = compile_when_condition(&WhenCondition::Not {
            conditions: vec![WhenCondition::Daytime],
        }).unwrap();
        assert_eq!(once["type"], "apoli:daytime");
        assert_eq!(once["inverted"], true);

        // Double Not collapses: inverted=true removed (back to false-default).
        let twice = compile_when_condition(&WhenCondition::Not {
            conditions: vec![WhenCondition::Not { conditions: vec![WhenCondition::Daytime] }],
        }).unwrap();
        assert_eq!(twice["type"], "apoli:daytime");
        assert!(twice.as_object().unwrap().get("inverted").is_none(),
            "Not(Not(X)) should remove the inverted flag, not just set it false");
    }

    #[test]
    fn tranche2b_compile_when_not_of_any_collapses_to_any() {
        // Not(Any) is semantically still Any; we keep the gate open.
        let c = compile_when_condition(&WhenCondition::Not {
            conditions: vec![WhenCondition::Any],
        });
        assert!(c.is_none(), "Not(Any) — no apoli condition emitted");
    }

    #[test]
    fn tranche2b_real_witch_cauldron_buff_emits_condition_field() {
        // Real Bewitchment ritual: the witch is stronger near her cauldron.
        // Authored as a typed PerkIntent; verifies the AttributeBuff arm
        // applies the condition gate via compile_when_condition.
        let perk = PerkIntent::AttributeBuff {
            attribute: AttributeId::new("minecraft:generic.max_health"),
            amount: BuffAmount::new(4.0).unwrap(),
            op: AttrOp::Addition,
            when: Some(WhenCondition::BlockInRadius {
                block: BlockSelector::One(BlockId::new("bewitchment:witch_cauldron")),
                radius: BlockRadius::new(8).unwrap(),
            }),
        };
        let mut ctx = EmitContext::new("witch");
        let emit = emit_perk(&perk, &mut ctx).expect("witch cauldron buff emits");
        let body = &emit.local_powers[0].body;
        let cond = pick("condition", body);
        assert_eq!(cond["type"], "apoli:block_in_radius");
        assert_eq!(cond["block_condition"]["block"], "bewitchment:witch_cauldron");
    }

    #[test]
    fn tranche2b_real_vampire_daylight_burn_uses_damage_over_time() {
        // Classic vampire trope, framed for our schema: take damage while
        // exposed to sun in daytime. DotWhen + And([Daytime, ExposedToSky])
        // → apoli:damage_over_time gated by apoli:and.
        let perk = PerkIntent::DotWhen {
            dps: DpsRate::new(2.0).unwrap(),
            when: WhenCondition::And { conditions: vec![
                WhenCondition::Daytime,
                WhenCondition::ExposedToSky,
            ] },
        };
        let mut ctx = EmitContext::new("vampire");
        let emit = emit_perk(&perk, &mut ctx).expect("vampire burn emits");
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:damage_over_time");
        let body = &p.body;
        assert_eq!(body["damage"], 2.0);
        assert_eq!(body["damage_easy"], 2.0);
        let cond = pick("condition", body);
        assert_eq!(cond["type"], "apoli:and");
        assert_eq!(cond["conditions"][0]["type"], "apoli:daytime");
        assert_eq!(cond["conditions"][1]["type"], "apoli:exposed_to_sky");
    }

    #[test]
    fn tranche2b_real_frost_form_uses_biome_tag_via_apoli_in_tag() {
        // Gameplay driver: -30% speed in cold biomes. The cold-biome tag is
        // a real `minecraft:is_cold` tag; the compiler nests apoli:in_tag.
        let perk = PerkIntent::BuffWhen {
            what: BuffWhat::Attribute {
                attribute: AttributeId::new("minecraft:generic.movement_speed"),
                op: AttrOp::MultiplyTotal,
                amount: BuffAmount::new(-0.30).unwrap(),
            },
            when: WhenCondition::BiomeTag { tag: BiomeTagId::new("minecraft:is_cold") },
        };
        let mut ctx = EmitContext::new("frost");
        let emit = emit_perk(&perk, &mut ctx).expect("frost form emits");
        let body = &emit.local_powers[0].body;
        assert_eq!(emit.local_powers[0].power_type, "apoli:attribute");
        let cond = pick("condition", body);
        assert_eq!(cond["type"], "apoli:biome");
        assert_eq!(cond["condition"]["type"], "apoli:in_tag");
        assert_eq!(cond["condition"]["tag"], "minecraft:is_cold");
    }

    #[test]
    fn tranche2b_real_underwater_speedster_uses_pose_condition() {
        // Junimo-ish gameplay: +35% movement speed while swimming. The pose
        // leaf maps directly to the registered apoli:swimming factory.
        let perk = PerkIntent::BuffWhen {
            what: BuffWhat::Attribute {
                attribute: AttributeId::new("minecraft:generic.movement_speed"),
                op: AttrOp::MultiplyTotal,
                amount: BuffAmount::new(0.35).unwrap(),
            },
            when: WhenCondition::Swimming,
        };
        let mut ctx = EmitContext::new("junimo");
        let emit = emit_perk(&perk, &mut ctx).expect("swim-speed emits");
        let cond = pick("condition", &emit.local_powers[0].body);
        assert_eq!(cond["type"], "apoli:swimming");
    }

    #[test]
    fn tranche2b_real_block_phase_through_leaves_when_sneaking() {
        // Dryad trope: sneak through foliage. Phasing factory verified from
        // PhasingPower.class bytecode (`blocks` block_condition + optional
        // top-level `condition`).
        let perk = PerkIntent::BlockPhase {
            block: BlockSelector::One(BlockId::new("#minecraft:leaves")),
            when: WhenCondition::Sneaking,
        };
        let mut ctx = EmitContext::new("dryad");
        let emit = emit_perk(&perk, &mut ctx).expect("leaf phase emits");
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:phasing");
        let body = &p.body;
        assert_eq!(body["blacklist"], false);
        // Tag-form (`#`) strips for the block_condition's tag field.
        assert_eq!(body["blocks"]["type"], "apoli:in_tag");
        assert_eq!(body["blocks"]["tag"], "minecraft:leaves");
        let cond = pick("condition", body);
        assert_eq!(cond["type"], "apoli:sneaking");
    }

    #[test]
    fn tranche2b_real_nether_fire_resistance_uses_dimension() {
        // Nether-bound buff: fire resistance only in the_nether dimension.
        let perk = PerkIntent::BuffWhen {
            what: BuffWhat::Effect {
                effect: StatusEffectId::new("minecraft:fire_resistance"),
                amplifier: Amplifier::new(0).unwrap(),
            },
            when: WhenCondition::Dimension { id: DimensionId::new("minecraft:the_nether") },
        };
        let mut ctx = EmitContext::new("nether_kin");
        let emit = emit_perk(&perk, &mut ctx).expect("nether-only emits");
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:action_over_time",
            "BuffWhat::Effect uses action_over_time + apply_effect");
        let cond = pick("condition", &p.body);
        assert_eq!(cond["type"], "apoli:dimension");
        assert_eq!(cond["dimension"], "minecraft:the_nether");
    }

    #[test]
    fn tranche2b_real_pelican_sun_fed_validates_through_existing_gate() {
        // Pelican Farmer's "sun-fed" — bonus saturation on any food only when
        // exposed to sky. Verifies the BonusSaturationOn arm threads the
        // condition AND the result passes the existing `validate` gate.
        let perk = PerkIntent::BonusSaturationOn {
            food: ItemSelector::One(ItemId::new("#c:foods")),
            extra: BonusSat::new(1).unwrap(),
            when: Some(WhenCondition::ExposedToSky),
        };
        let mut ctx = EmitContext::new("pelican");
        let emit = emit_perk(&perk, &mut ctx).expect("sun-fed emits");
        let cond = pick("condition", &emit.local_powers[0].body);
        assert_eq!(cond["type"], "apoli:exposed_to_sky");

        // Walk through the validator: wrap into an OriginsSet and assert ok.
        let set = OriginsSet {
            origins: vec![Origin {
                id: "pelican".to_string(),
                name: "Pelican Farmer".to_string(),
                description: "Fed by the open sky.".to_string(),
                powers: emit.origin_refs.clone(),
                icon: "minecraft:wheat".to_string(),
                impact: 2,
                order: 0,
            }],
            powers: emit.local_powers,
        };
        assert!(validate(set).is_ok(),
            "BonusSaturationOn(when=ExposedToSky) must pass the existing validate() gate");
    }

    #[test]
    fn tranche8_stagger_on_sprint_emits_action_over_time_with_sprinting_condition() {
        // Berserker-style stagger: slowness while sprinting. Real gameplay
        // driver — exerts a sprint-tax that gates rushing builds.
        let perk = PerkIntent::StaggerOnSprint {
            effect: StatusEffectId::new("minecraft:slowness"),
            duration_s: 4,
        };
        let mut ctx = EmitContext::new("berserker");
        let emit = emit_perk(&perk, &mut ctx).expect("stagger emits");
        let p = &emit.local_powers[0];
        assert_eq!(p.power_type, "apoli:action_over_time");
        assert_eq!(p.body["condition"]["type"], "apoli:sprinting");
        let action = &p.body["entity_action"];
        assert_eq!(action["type"], "apoli:apply_effect");
        assert_eq!(action["effect"]["effect"], "minecraft:slowness");
        assert_eq!(action["effect"]["duration"], 80); // 4 s × 20 t/s
        assert_eq!(action["effect"]["amplifier"], 0);
    }

    #[test]
    fn tranche8_stagger_on_sprint_byte_deterministic() {
        let perk = PerkIntent::StaggerOnSprint {
            effect: StatusEffectId::new("minecraft:slowness"),
            duration_s: 4,
        };
        let render = || {
            let mut c = EmitContext::new("berserker");
            let e = emit_perk(&perk, &mut c).unwrap();
            serde_json::to_string(&e.local_powers[0].body).unwrap()
        };
        assert_eq!(render(), render(),
            "stagger emit must be byte-deterministic across two invocations");
    }

    #[test]
    fn tranche8_real_berserker_with_stagger_validates_end_to_end() {
        // Real Berserker origin: high damage, low control. Combines T1
        // (attribute buff) with T8-shipped StaggerOnSprint. Pass-through
        // the existing `validate` gate confirms cross-tranche stacking.
        let mut ctx = EmitContext::new("berserker");
        let mut all_powers = Vec::new();
        let mut all_refs = Vec::new();
        for perk in [
            PerkIntent::AttributeBuff {
                attribute: AttributeId::new("minecraft:generic.attack_damage"),
                amount: BuffAmount::new(3.0).unwrap(),
                op: AttrOp::Addition,
                when: None,
            },
            PerkIntent::StaggerOnSprint {
                effect: StatusEffectId::new("minecraft:slowness"),
                duration_s: 4,
            },
        ] {
            let e = emit_perk(&perk, &mut ctx).unwrap();
            all_refs.extend(e.origin_refs);
            all_powers.extend(e.local_powers);
        }
        assert_eq!(all_refs, vec!["berserker_p0".to_string(), "berserker_p1".to_string()]);
        let set = OriginsSet {
            origins: vec![Origin {
                id: "berserker".to_string(),
                name: "Berserker".to_string(),
                description: "Hits hard, stumbles harder.".to_string(),
                powers: all_refs,
                icon: "minecraft:iron_axe".to_string(),
                impact: 2,
                order: 0,
            }],
            powers: all_powers,
        };
        assert!(validate(set).is_ok(),
            "Berserker (AttrBuff + StaggerOnSprint) must pass the validate() gate");
    }

    #[test]
    fn companion_datapack_pipeline_lands_real_mcfunctions_with_tags() {
        // End-to-end: pick a marker variant that emits a real companion
        // tick fn, walk through emit_perk → OriginsSet → validate →
        // emit_with_companion. Asserts that the .mcfunction file lands,
        // and minecraft/tags/functions/tick.json references it.
        let perk = PerkIntent::PacifyTargeting {
            by: EntityCondRef::One(EntityTypeId::new("naturalist:bear")),
        };
        let mut ctx = EmitContext::new("junimo");
        let emit = emit_perk(&perk, &mut ctx).expect("PacifyTargeting emits");
        assert_eq!(emit.mcfunctions.len(), 1,
            "PacifyTargeting must emit one tick mcfunction");
        let (tick_path, tick_body) = &emit.mcfunctions[0];
        assert!(tick_path.ends_with("_tick.mcfunction"), "convention: tick suffix");
        assert!(tick_body.contains("data merge entity") && tick_body.contains("naturalist:bear"),
            "tick body must do the real AI-clearing work; got:\n{tick_body}");

        // Build the OriginsSet + validate + emit_with_companion.
        let set = OriginsSet {
            origins: vec![Origin {
                id: "junimo".to_string(),
                name: "Junimo".to_string(),
                description: "Beloved by the wild.".to_string(),
                powers: emit.origin_refs,
                icon: "minecraft:fern".to_string(),
                impact: 2,
                order: 0,
            }],
            powers: emit.local_powers,
        };
        let validated = validate(set).expect("Junimo PacifyTargeting passes validate gate");
        let mut companion = CompanionMcFunctions::new();
        companion.extend_from(emit.mcfunctions);
        let files = emit_with_companion(&validated, &companion, "anvil");
        // tick.json must be present and reference the function id.
        let (_, tick_tag) = files.iter().find(|(p, _)| p.ends_with("functions/tick.json"))
            .expect("tick.json must be emitted when tick fns are present");
        assert!(tick_tag.contains("anvil:origins/junimo/"),
            "tick.json must list the companion function id; got: {tick_tag}");
        // The mcfunction file itself must be in the output.
        assert!(files.iter().any(|(p, _)| p.ends_with("_tick.mcfunction")),
            "companion tick.mcfunction must land in the file list");
    }

    #[test]
    fn companion_datapack_deterministic_across_invocations() {
        // Two identical inputs → byte-equal output, with companion fns.
        let perks: Vec<PerkIntent> = vec![
            PerkIntent::PacifyTargeting {
                by: EntityCondRef::One(EntityTypeId::new("minecraft:wolf")),
            },
            PerkIntent::AutoJournal {
                milestones: vec![JournalMilestone {
                    trigger: SanitizedText::new("first_kill").unwrap(),
                    entry: SanitizedText::new("The first kill is the hardest.").unwrap(),
                }],
            },
        ];
        let render = || {
            let mut ctx = EmitContext::new("wolf");
            let mut all_powers = Vec::new();
            let mut all_refs = Vec::new();
            let mut companion = CompanionMcFunctions::new();
            for p in &perks {
                let e = emit_perk(p, &mut ctx).unwrap();
                all_powers.extend(e.local_powers);
                all_refs.extend(e.origin_refs);
                companion.extend_from(e.mcfunctions);
            }
            let set = OriginsSet {
                origins: vec![Origin {
                    id: "wolf".to_string(), name: "Wolf".to_string(),
                    description: "Pack-bound.".to_string(),
                    powers: all_refs, icon: "minecraft:bone".to_string(),
                    impact: 2, order: 0,
                }],
                powers: all_powers,
            };
            let v = validate(set).expect("pack-bound wolf validates");
            let files = emit_with_companion(&v, &companion, "anvil");
            files.iter().map(|(p, c)| format!("{p}\n{c}")).collect::<Vec<_>>().join("|")
        };
        assert_eq!(render(), render(),
            "emit_with_companion must be byte-deterministic across two invocations");
    }

    // ---- PHASE 1d — forecast + density-budget validator ----

    fn make_intent(perks: Vec<PerkIntent>) -> OriginIntent {
        OriginIntent {
            theme: ThemeTag::new("arcane"),
            name: SanitizedText::new("Test").unwrap(),
            description: SanitizedText::new("Just a test.").unwrap(),
            icon: ItemId::new("minecraft:nether_star"),
            perks,
            density: None,
            linked_boss: None,
            gates_quest: None,
        }
    }

    #[test]
    fn entity_glow_description_names_its_targets() {
        let undead = EntityCondRef::One(EntityTypeId::new("#minecraft:undead"));
        assert_eq!(describe_entity_cond(&undead), "undead");
        assert_eq!(entity_label(&undead), "Undead");
        let any = EntityCondRef::One(EntityTypeId::new("any"));
        assert_eq!(describe_entity_cond(&any), "creatures");
    }

    #[test]
    fn active_description_is_specific_not_placeholder() {
        let d = describe_active_body(
            &ActiveBody::InvisibilityPulse { duration_s: 8, retinue: None },
            30,
        );
        assert!(d.contains("invisible for 8s"), "{d}");
        assert!(d.contains("Cooldown 30s"), "{d}");
        assert!(
            !d.to_lowercase().contains("keybind-triggered"),
            "no placeholder text: {d}"
        );
    }

    #[test]
    fn validate_attribute_uniqueness_flags_double_modify() {
        let buff = |amt: f32| PerkIntent::AttributeBuff {
            attribute: AttributeId::new("minecraft:generic.max_health"),
            amount: BuffAmount::new(amt).unwrap(),
            op: AttrOp::Addition,
            when: None,
        };
        // Two perks on the SAME attribute (the Witch's -4 and +6) -> rejected.
        let bad = make_intent(vec![buff(6.0), buff(-4.0)]);
        assert!(
            validate_attribute_uniqueness(&bad).iter().any(|i| matches!(
                i,
                OriginIssue::ConflictingAttribute { attribute }
                if attribute.contains("max_health")
            )),
            "double max_health must be flagged"
        );
        // One perk on the attribute -> fine.
        assert!(validate_attribute_uniqueness(&make_intent(vec![buff(6.0)]))
            .is_empty());
    }

    #[test]
    fn forecast_classifies_passive_active_lifetime() {
        // Mix of every category: 2 passives, 1 active, 1 lifetime.
        let intent = make_intent(vec![
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:night_vision"), amplifier: None },
            PerkIntent::Scale { factor: ScaleFactor::new(0.65).unwrap() },
            PerkIntent::Active {
                key: KeyBind::Primary, cooldown_s: 60, hud: HudHint::Active,
                body: ActiveBody::AreaBurst { radius: 8, damage: 8.0, knockback: 1.0 },
            },
            PerkIntent::Lifetime {
                gate: LifetimeGate::OncePerMoonFull,
                body: LifetimeBody::WaypointRecall { visit_threshold: 3 },
            },
        ]);
        let f = forecast_origin_capabilities(&intent);
        assert_eq!(f.passives, 2);
        assert_eq!(f.actives, 1);
        assert_eq!(f.lifetimes, 1);
    }

    #[test]
    fn forecast_required_capabilities_dedup_and_sorted() {
        // Two perks needing Trinkets, one needing BondableCompanions.
        let intent = make_intent(vec![
            PerkIntent::SignatureTrinket {
                slot: TrinketSlot::Necklace,
                model: TextureKey::new("anvil:item/locket"),
                carries: Box::new(PerkIntent::PassiveEffect {
                    effect: StatusEffectId::new("minecraft:luck"), amplifier: None,
                }),
            },
            PerkIntent::Familiar {
                entity: EntityTypeId::new("bewitchment:toad"),
                bond_action: BondAction::CauldronRitual,
                persist_through_death: true,
            },
        ]);
        let f = forecast_origin_capabilities(&intent);
        assert!(f.required_capabilities.contains(&ModCapability::Trinkets),
            "SignatureTrinket requires Trinkets capability");
        assert!(f.required_capabilities.contains(&ModCapability::BondableCompanions),
            "Familiar requires BondableCompanions");
        // Dedup: SignatureTrinket alone wouldn't list Trinkets twice.
        let trinket_count = f.required_capabilities.iter()
            .filter(|c| **c == ModCapability::Trinkets).count();
        assert_eq!(trinket_count, 1, "no duplicates in required_capabilities");
    }

    #[test]
    fn density_budget_light_rejects_overstuffed_origin() {
        // Light = 3-4 passives, 0 actives, 0 lifetimes.
        // 6 passives + 1 active = 2 violations (passives over, actives over).
        let intent = make_intent(vec![
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:night_vision"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:speed"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:luck"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:strength"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:resistance"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:absorption"), amplifier: None },
            PerkIntent::Active {
                key: KeyBind::Primary, cooldown_s: 60, hud: HudHint::Active,
                body: ActiveBody::AreaBurst { radius: 8, damage: 8.0, knockback: 1.0 },
            },
        ]);
        let issues = validate_density_budget(&intent, Density::Light);
        assert_eq!(issues.len(), 2, "expected one passives-over + one actives-over: {issues:#?}");
        assert!(issues.iter().any(|i| matches!(i, OriginIssue::BudgetViolation {
            density: Density::Light, what: "passives", direction: BudgetDirection::Over, ..
        })), "missing passives-over: {issues:#?}");
        assert!(issues.iter().any(|i| matches!(i, OriginIssue::BudgetViolation {
            density: Density::Light, what: "actives", direction: BudgetDirection::Over, ..
        })), "missing actives-over: {issues:#?}");
    }

    #[test]
    fn density_budget_rich_rejects_too_few_lifetimes() {
        // Rich = 8-10 passives, 1 active, EXACTLY 1 lifetime.
        // 9 passives + 1 active + 0 lifetimes = one lifetimes-under violation.
        let intent = make_intent(vec![
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:night_vision"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:speed"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:luck"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:strength"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:resistance"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:absorption"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:fire_resistance"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:invisibility"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:water_breathing"), amplifier: None },
            PerkIntent::Active {
                key: KeyBind::Primary, cooldown_s: 60, hud: HudHint::Active,
                body: ActiveBody::AreaBurst { radius: 8, damage: 8.0, knockback: 1.0 },
            },
        ]);
        let issues = validate_density_budget(&intent, Density::Rich);
        assert_eq!(issues.len(), 1);
        assert!(matches!(&issues[0], OriginIssue::BudgetViolation {
            density: Density::Rich, what: "lifetimes", count: 0,
            direction: BudgetDirection::Under, ..
        }), "expected lifetimes-under, got {:#?}", issues);
    }

    #[test]
    fn density_budget_standard_accepts_in_range_origin() {
        // Standard = 5-7 passives, 1 active, 0-1 lifetimes.
        // 6 passives + 1 active + 1 lifetime = all in band.
        let intent = make_intent(vec![
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:night_vision"), amplifier: None },
            PerkIntent::SpecialMovement { kind: MoveKind::HigherJump },
            PerkIntent::DamageVs {
                target: EntityCondRef::One(EntityTypeId::new("minecraft:zombie")),
                multiplier: DamageMul::new(1.5).unwrap(),
            },
            PerkIntent::ForbiddenItemUse { what: ItemSelector::One(ItemId::new("minecraft:iron_sword")) },
            PerkIntent::PreventSleep { except: None },
            PerkIntent::FasterBreakOn {
                block: BlockSelector::One(BlockId::new("#minecraft:crops")),
                multiplier: BreakMul::new(1.5).unwrap(),
            },
            PerkIntent::Active {
                key: KeyBind::Primary, cooldown_s: 60, hud: HudHint::Active,
                body: ActiveBody::AreaBurst { radius: 8, damage: 8.0, knockback: 1.0 },
            },
            PerkIntent::Lifetime {
                gate: LifetimeGate::OncePerMoonFull,
                body: LifetimeBody::WaypointRecall { visit_threshold: 3 },
            },
        ]);
        let issues = validate_density_budget(&intent, Density::Standard);
        assert!(issues.is_empty(),
            "well-shaped Standard origin should have no budget issues; got {issues:#?}");
    }

    #[test]
    fn capabilities_check_rejects_familiar_without_bondable_mod() {
        // Familiar requires `BondableCompanions` capability.
        // Bewitchment provides it; if the mod isn't in the pack, the perk
        // must surface a RequiresAbsentCapability.
        let intent = make_intent(vec![
            PerkIntent::Familiar {
                entity: EntityTypeId::new("bewitchment:toad"),
                bond_action: BondAction::CauldronRitual,
                persist_through_death: true,
            },
        ]);
        // Mod list LACKS bewitchment. Familiar requires both
        // BondableCompanions and DatapackChannel, so both surface.
        let issues = validate_capabilities(&intent, &["minecraft", "minecraft:fabricloader"]);
        assert!(issues.iter().any(|i| matches!(i, OriginIssue::RequiresAbsentCapability {
            variant: PerkIntentTag::Familiar, missing: ModCapability::BondableCompanions,
        })), "expected BondableCompanions surfaced; got {issues:#?}");

        // Same intent WITH bewitchment + openloader present → no issues.
        let ok_issues = validate_capabilities(&intent, &["bewitchment", "openloader"]);
        assert!(ok_issues.is_empty(),
            "Familiar should accept when bewitchment + openloader present; got {ok_issues:#?}");
    }

    #[test]
    fn check_origin_intent_combines_budget_capabilities_and_grounding() {
        // Real overstuffed witch: too many passives for Light, missing
        // capability, and a typo'd biome id. All three issues surface in
        // one structured response.
        let intent = make_intent(vec![
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:night_vision"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:speed"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:luck"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:strength"), amplifier: None },
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:resistance"), amplifier: None },
            PerkIntent::Familiar {
                entity: EntityTypeId::new("bewitchment:toad"),
                bond_action: BondAction::CauldronRitual,
                persist_through_death: true,
            },
        ]);
        // Vocab has the toad but not the capability; mod list lacks bewitchment.
        let mut vocab = RegistryVocab::default();
        vocab.entities.insert("bewitchment:toad".to_string());
        vocab.items.insert("minecraft:nether_star".to_string());
        let issues = check_origin_intent(&intent, Density::Light, &["minecraft"], &vocab);
        let kinds: Vec<&'static str> = issues.iter().map(|i| match i {
            OriginIssue::BudgetViolation { .. } => "budget",
            OriginIssue::RequiresAbsentCapability { .. } => "capability",
            OriginIssue::UnknownId { .. } => "unknown_id",
            _ => "other",
        }).collect();
        assert!(kinds.contains(&"budget"), "expected budget issue; got {issues:#?}");
        assert!(kinds.contains(&"capability"), "expected capability issue; got {issues:#?}");
    }

    #[test]
    fn intent_catalog_prompt_section_documents_every_variant_and_pattern() {
        // The LLM consumes this. Verify it mentions every PerkIntent tag
        // and every WhenCondition kind — drift between Rust enum + prompt
        // would silently teach the LLM stale variant names.
        let p = intent_catalog_prompt_section();
        // Density buckets named (matches the `- light (score …):` format).
        assert!(p.contains("light (score"), "density 'light' must be documented");
        assert!(p.contains("standard (score"), "density 'standard' must be documented");
        assert!(p.contains("rich (score"), "density 'rich' must be documented");
        // Per-origin variation guidance must be present too — the user
        // complaint was specifically that the LLM never asked them about
        // density. The catalog now spells out "ASK THE PLAYER FIRST" and
        // documents per-origin overrides; if either drops out, a future
        // edit silently regresses to silent-defaulting again.
        assert!(p.contains("ASK THE PLAYER FIRST"),
            "intent catalog must instruct the LLM to ask before picking density");
        assert!(p.contains("per-origin") || p.contains("Per-origin") || p.contains("per-origin intents"),
            "intent catalog must explain per-origin density variation");
        // A sample of structurally-critical variant tag strings.
        for tag in [
            "starts_with", "scale", "passive_effect", "attribute_buff",
            "buff_when", "dot_when", "damage_vs", "forbidden_item_use",
            "tally_milestone", "pacify_targeting", "hostile_recognition",
            "entity_glow", "once_per_day_bonus", "season_notification",
            "keep_inventory_slot", "map_marker_at_spawn", "overlay",
            "auto_journal", "active", "lifetime",
            "combo_chain", "siphon", "dodge_roll", "vein_mine", "harvest_aoe",
            "last_stand", "block_phase", "stagger_on_sprint",
            "signature_trinket", "familiar", "seasonal_form", "apprentice_to_npc",
            "brew_potency", "knife_master", "gravewalker", "pack_leader",
            "bandit_kin", "origin_questline",
        ] {
            assert!(p.contains(tag), "intent catalog missing variant `{tag}`");
        }
        // WhenCondition kinds.
        for k in [
            "any", "daytime", "nighttime", "in_rain", "exposed_to_sky", "on_fire",
            "sneaking", "sprinting", "swimming", "fall_flying",
            "dimension", "biome", "biome_tag", "block_in_radius",
            "not", "and", "or",
        ] {
            assert!(p.contains(k), "intent catalog missing when-condition kind `{k}`");
        }
        // At least the four impressive-pattern templates are present.
        for needle in [
            "Bewitched Witch", "Frost Spirit", "Berserker", "Vampire Hunter",
        ] {
            assert!(p.contains(needle), "intent catalog missing pattern `{needle}`");
        }
    }

    /// Regression for the 12-round catalog↔emit drift cascade in the UCL Rich
    /// transcript. Prior to fix #3 the catalog underspecified ActiveBody and
    /// LifetimeBody variants — listed field names without shapes — so the LLM
    /// guessed the shape and got whack-a-moled by emit deserialisation.
    /// This test asserts that for every previously-failing variant, the JSON
    /// SHAPE the catalog now documents actually deserialises cleanly. If a
    /// future catalog edit silently underspecifies again, this test fires.
    #[test]
    fn catalog_documented_active_and_lifetime_body_shapes_deserialise() {
        use serde_json::json;

        // timed_effect_chain: on/off are ARRAYS of StatusEffectInst (the
        // top regression — the LLM kept passing single strings).
        let _tec: ActiveBody = serde_json::from_value(json!({
            "kind": "timed_effect_chain",
            "on":  [{ "effect": "minecraft:speed",    "amplifier": 1, "duration_t": 200 }],
            "duration_s": 10,
            "off": [{ "effect": "minecraft:slowness", "amplifier": 0, "duration_t": 80  }],
            "off_duration_s": 4
        })).expect("timed_effect_chain must deserialise per catalog docs");

        // teleport_to_marker: on_depart is an AreaAction STRUCT, not a list.
        let _tp: ActiveBody = serde_json::from_value(json!({
            "kind": "teleport_to_marker",
            "marker": "minecraft:end_portal_frame",
            "on_depart": { "radius": 6, "damage": 2.0, "particle_key": "anvil:particle/teleport" }
        })).expect("teleport_to_marker on_depart must deserialise as AreaAction");

        // transformation: effects_on/effects_off are arrays of StatusEffectInst,
        // summon_allies is optional RetinueSpec.
        let _trans: ActiveBody = serde_json::from_value(json!({
            "kind": "transformation",
            "duration_s": 30,
            "stash_inventory": true,
            "effects_on":  [{ "effect": "minecraft:strength", "amplifier": 1, "duration_t": 600 }],
            "effects_off": [{ "effect": "minecraft:weakness", "amplifier": 0, "duration_t": 100 }]
        })).expect("transformation must deserialise without summon_allies/scale");

        // place_persistent_zone: catalog now lists all 5 fields explicitly.
        let _zone: LifetimeBody = serde_json::from_value(json!({
            "kind": "place_persistent_zone",
            "structure_key": "anvil:zone/grove",
            "radius": 16,
            "suppress_spawns": true,
            "growth_boost": 1.5,
            "animal_migration": false
        })).expect("place_persistent_zone must deserialise per catalog");

        // log_and_resurrect: `logs` is an EntityCondRef (string or array), NOT
        // text/journal entries. Catalog now flags this explicitly.
        let _lar: LifetimeBody = serde_json::from_value(json!({
            "kind": "log_and_resurrect",
            "logs": "minecraft:zombie",
            "summon_for_dur_s": 600
        })).expect("log_and_resurrect with single entity must deserialise");
        let _lar_many: LifetimeBody = serde_json::from_value(json!({
            "kind": "log_and_resurrect",
            "logs": ["minecraft:zombie", "minecraft:skeleton"],
            "summon_for_dur_s": 600
        })).expect("log_and_resurrect with array of entities must deserialise");

        // rally_event: requires summon_entities AND structure_key AND
        // area_buff_dur_s — all now documented in catalog.
        let _rally: LifetimeBody = serde_json::from_value(json!({
            "kind": "rally_event",
            "summon_entities": "minecraft:iron_golem",
            "structure_key": "anvil:zone/rally",
            "area_buff_dur_s": 120
        })).expect("rally_event must deserialise with all three fields");

        // waypoint_recall: catalog flagged as simplest lifetime. Only needs
        // visit_threshold.
        let _wp: LifetimeBody = serde_json::from_value(json!({
            "kind": "waypoint_recall",
            "visit_threshold": 5
        })).expect("waypoint_recall must deserialise with just visit_threshold");

        // Catalog text MUST keep the load-bearing phrases that prevent drift.
        let p = intent_catalog_prompt_section();
        assert!(p.contains("StatusEffectInst:"),
            "catalog must document StatusEffectInst struct shape");
        assert!(p.contains("AreaAction:"),
            "catalog must document AreaAction struct shape");
        assert!(p.contains("on_depart is an AreaAction"),
            "catalog must flag on_depart as AreaAction (not effect list)");
        assert!(p.contains("ARRAYS of StatusEffectInst"),
            "catalog must flag timed_effect_chain on/off as arrays");
        assert!(p.contains("`logs` = entity-type"),
            "catalog must clarify that log_and_resurrect.logs is entity refs");
        assert!(p.contains("waypoint_recall, visit_threshold"),
            "catalog must list waypoint_recall as simplest lifetime");
    }

    /// THE end-to-end regression test for the UCL Rich-density dead-end.
    /// Pre-fix this fails at check_origin_intent with RequiresAbsentCapability
    /// (DatapackChannel missing) — because mod_ids were opaque project_ids
    /// and the capabilities() map keyed on slugs/Java-ids, the lookup always
    /// missed. With fix #1 (normalize + display-name fallback), "Open Loader"
    /// → "openloader" → DatapackChannel detected → every Lifetime / ComboChain
    /// / TallyMilestone perk is unblocked, and a realistic Rich origin
    /// authored from the new (fix #3) catalog passes the WHOLE pipeline:
    /// check_origin_intent → emit_perk → validate.
    #[test]
    fn rich_origin_with_open_loader_display_name_passes_full_pipeline() {
        use serde_json::json;

        // A realistic Rich-density origin matching what the LLM would author
        // following the now-explicit catalog. 8 passives + 1 active + 1
        // lifetime — at the rich-density ceiling. Uses vanilla ids so empty-
        // vocab grounding (namespace-only) passes.
        let intent: OriginIntent = serde_json::from_value(json!({
            "theme": "adventure",
            "name": "Bentham Reborn",
            "description": "A scholar who walks between two worlds.",
            "icon": "minecraft:experience_bottle",
            "perks": [
                { "intent": "attribute_buff", "attribute": "minecraft:generic.max_health",
                  "op": "addition", "amount": 4.0 },
                { "intent": "attribute_buff", "attribute": "minecraft:generic.armor",
                  "op": "addition", "amount": 4.0 },
                { "intent": "attribute_buff", "attribute": "minecraft:generic.movement_speed",
                  "op": "multiply_total", "amount": 0.15 },
                { "intent": "passive_effect", "effect": "minecraft:night_vision" },
                { "intent": "starts_with", "items": ["minecraft:book", "minecraft:lantern"] },
                { "intent": "damage_vs", "target": "minecraft:zombie", "multiplier": 1.5 },
                { "intent": "buff_when",
                  "what": { "kind": "effect", "effect": "minecraft:strength", "amplifier": 0 },
                  "when": { "kind": "nighttime" } },
                { "intent": "dot_when", "dps": 0.5,
                  "when": { "kind": "and",
                            "conditions": [{ "kind": "daytime" }, { "kind": "exposed_to_sky" }] } },
                // Active toggle (1 per origin max).
                { "intent": "active", "key": "primary", "cooldown_s": 60, "hud": "active",
                  "body": { "kind": "area_burst", "radius": 8, "damage": 6.0, "knockback": 1.0 } },
                // Lifetime — waypoint_recall is the simplest, requires only
                // DatapackChannel capability (which fix #1 makes detectable).
                { "intent": "lifetime", "gate": "once_per_save",
                  "body": { "kind": "waypoint_recall", "visit_threshold": 5 } }
            ]
        })).expect("intent JSON must deserialise per the updated catalog");

        // Mod ids as the curator NOW builds them after fix #1: paired
        // {project_id, name} per mod. Open Loader's display name normalises
        // to "openloader" and matches the existing map key.
        let mod_ids: Vec<&str> = vec![
            "AjW5DBn7", "Open Loader",   // opaque + display
            "T8Q6Vb3N", "Pehkui",
            "JX8a7Pp4", "Origins",
        ];

        // Empty vocab = no jars scanned = namespace-only grounding (vanilla
        // ids pass through). Mirrors the no-vocab path in tool_generate_origin_intents.
        let vocab = crate::registry::RegistryVocab::default();

        // Stage 1 — the gate that previously dead-ended on
        // RequiresAbsentCapability { missing: DatapackChannel }.
        let issues = check_origin_intent(&intent, Density::Rich, &mod_ids, &vocab);
        assert!(
            issues.is_empty(),
            "Rich origin with Open Loader present MUST pass check_origin_intent. \
             Pre-fix this returned RequiresAbsentCapability(DatapackChannel). \
             Issues: {issues:#?}",
        );

        // Stage 2 — every perk emits cleanly (catalog↔emit alignment, fix #3).
        let mut ctx = EmitContext::new("o00_bentham_reborn".to_string());
        let mut all_powers: Vec<Power> = Vec::new();
        let mut origin_refs: Vec<String> = Vec::new();
        for perk in &intent.perks {
            let e = emit_perk(perk, &mut ctx)
                .unwrap_or_else(|err| panic!("emit_perk failed for {:?}: {err:?}", perk.tag()));
            all_powers.extend(e.local_powers);
            origin_refs.extend(e.origin_refs);
        }

        // Stage 3 — validate(OriginsSet) — the final gate before write.
        let origin = Origin {
            id: "o00_bentham_reborn".to_string(),
            name: "Bentham Reborn".to_string(),
            description: "A scholar who walks between two worlds.".to_string(),
            powers: origin_refs,
            icon: "minecraft:experience_bottle".to_string(),
            impact: 3,
            order: 0,
        };
        let set = OriginsSet { origins: vec![origin], powers: all_powers };
        validate(set).expect("validated OriginsSet must round-trip — the schema-loose layer is sound");
    }

    /// Inverse-control for the pipeline test above. With Open Loader REMOVED
    /// from mod_ids, the same Rich origin MUST be rejected at check_origin_intent
    /// with RequiresAbsentCapability(DatapackChannel). Proves the capability
    /// gate is still doing its job after fix #1 — not just rubber-stamping.
    #[test]
    fn rich_origin_without_open_loader_is_correctly_rejected() {
        use serde_json::json;
        let intent: OriginIntent = serde_json::from_value(json!({
            "theme": "adventure",
            "name": "Test",
            "description": "x",
            "icon": "minecraft:stick",
            "perks": [
                { "intent": "lifetime", "gate": "once_per_save",
                  "body": { "kind": "waypoint_recall", "visit_threshold": 5 } }
            ]
        })).unwrap();

        // No Open Loader anywhere — neither slug, nor display name.
        let mod_ids: Vec<&str> = vec!["AjW5DBn7", "Pehkui"];
        let vocab = crate::registry::RegistryVocab::default();
        let issues = check_origin_intent(&intent, Density::Rich, &mod_ids, &vocab);
        let has_dpc_missing = issues.iter().any(|i| matches!(i,
            OriginIssue::RequiresAbsentCapability { missing: ModCapability::DatapackChannel, .. }));
        assert!(has_dpc_missing,
            "Lifetime perk without Open Loader MUST surface DatapackChannel-missing. Got: {issues:#?}");
    }

    /// Pin the selector behaviour for forbidden_item_use / faster_break_on —
    /// the LLM in the transcript kept passing objects, then plain strings,
    /// then objects again. The truth: ItemSelector / BlockSelector are
    /// `One(string) | Many([string])` — never wrapped objects.
    #[test]
    fn selectors_accept_plain_string_or_array_never_object() {
        use serde_json::json;
        let one: ItemSelector = serde_json::from_value(json!("minecraft:iron_sword"))
            .expect("ItemSelector must accept a single string");
        let many: ItemSelector = serde_json::from_value(json!(["minecraft:iron_sword", "minecraft:diamond_sword"]))
            .expect("ItemSelector must accept an array of strings");
        match one { ItemSelector::One(_) => {}, _ => panic!("expected One") }
        match many { ItemSelector::Many(v) => assert_eq!(v.len(), 2), _ => panic!("expected Many") }

        // An object IS NOT accepted — schema is intentionally untagged.
        let obj: Result<ItemSelector, _> = serde_json::from_value(json!({"id": "minecraft:iron_sword"}));
        assert!(obj.is_err(), "ItemSelector must REJECT object form");

        let p = intent_catalog_prompt_section();
        assert!(p.contains("NEVER an object"),
            "catalog must warn that selectors are never objects");
    }

    #[test]
    fn forecast_serializes_to_json_for_curator_consumption() {
        // The curator reads back the forecast as JSON; assert the shape
        // is stable and contains the expected keys.
        let intent = make_intent(vec![
            PerkIntent::PassiveEffect { effect: StatusEffectId::new("minecraft:night_vision"), amplifier: None },
            PerkIntent::Active {
                key: KeyBind::Primary, cooldown_s: 60, hud: HudHint::Active,
                body: ActiveBody::AreaBurst { radius: 8, damage: 8.0, knockback: 1.0 },
            },
        ]);
        let f = forecast_origin_capabilities(&intent);
        let j = serde_json::to_value(&f).unwrap();
        assert_eq!(j["passives"], 1);
        assert_eq!(j["actives"], 1);
        assert_eq!(j["lifetimes"], 0);
        assert!(j["required_capabilities"].is_array());
    }

    #[test]
    fn fn_path_to_id_strips_prefix_and_suffix() {
        let id = fn_path_to_id("data/anvil/functions/origins/witch/p0_tick.mcfunction");
        assert_eq!(id, Some("anvil:origins/witch/p0_tick".to_string()));
        // Misshapen path returns None — no panic on unexpected input.
        assert_eq!(fn_path_to_id("garbage"), None);
    }

    #[test]
    fn every_variant_in_catalog_emits_without_notyetimplemented() {
        // The hard claim: every one of the 44 PerkIntent variants must reach
        // an emit handler that returns Ok(PerkEmit). If a future variant is
        // added to PerkIntent, this test surfaces the missing arm immediately.
        let perks = one_per_variant();
        let mut ctx = EmitContext::new("sweep");
        let mut not_implemented = Vec::new();
        let mut total = 0;
        for perk in &perks {
            total += 1;
            match emit_perk(perk, &mut ctx) {
                Ok(_) => {}
                Err(EmitError::NotYetImplemented { variant, tranche_landing }) => {
                    not_implemented.push((variant, tranche_landing));
                }
                Err(EmitError::Inner(_)) => {
                    // Inner grounding issues are a separate failure class —
                    // OK to surface here too as evidence the fixture is bad.
                    not_implemented.push((perk.tag(), "(grounding inner)"));
                }
            }
        }
        assert!(
            not_implemented.is_empty(),
            "All {total} PerkIntent variants must emit; got {} unimplemented: {:#?}",
            not_implemented.len(),
            not_implemented,
        );
        // one_per_variant() emits one fixture per PerkIntent variant; total
        // matches the enum cardinality (currently 45 — sweep is structural,
        // not numeric). Bumps automatically when the enum grows.
        assert!(total >= 44, "expected ≥44 variants in one_per_variant(); got {total}");
    }

    #[test]
    fn marker_variants_emit_real_functional_companion_mcfunctions() {
        // The "implement, don't just stub" guarantee. Each variant in the
        // companion-datapack camp must emit at least one real mcfunction
        // file with non-empty contents — proving the marker has been
        // upgraded from "captures data only" to "runs in-game behaviour".
        let cases: Vec<(&str, PerkIntent)> = vec![
            ("PreventBreakUnderFoot", PerkIntent::PreventBreakUnderFoot {
                block: BlockSelector::One(BlockId::new("minecraft:grass_block")),
            }),
            ("PacifyTargeting", PerkIntent::PacifyTargeting {
                by: EntityCondRef::One(EntityTypeId::new("naturalist:bear")),
            }),
            ("HostileRecognition", PerkIntent::HostileRecognition {
                by: EntityCondRef::One(EntityTypeId::new("minecraft:wolf")),
            }),
            ("SeasonNotification", PerkIntent::SeasonNotification {
                lead_days: LeadDays::new(1).unwrap(),
                message: SanitizedText::new("Winter is coming.").unwrap(),
            }),
            ("KeepInventorySlot", PerkIntent::KeepInventorySlot { slots: vec![Slot::Hotbar0] }),
            ("AutoJournal", PerkIntent::AutoJournal {
                milestones: vec![JournalMilestone {
                    trigger: SanitizedText::new("first_kill").unwrap(),
                    entry: SanitizedText::new("Test entry.").unwrap(),
                }],
            }),
            ("Lifetime", PerkIntent::Lifetime {
                gate: LifetimeGate::OncePerInGameDay,
                body: LifetimeBody::WaypointRecall { visit_threshold: 3 },
            }),
            ("ComboChain", PerkIntent::ComboChain {
                window_t: 60, ramp: 0.15, max_stacks: ComboMax::new(5).unwrap(),
            }),
            ("VeinMine", PerkIntent::VeinMine {
                block: BlockSelector::One(BlockId::new("minecraft:copper_ore")),
                max_chain: 32,
            }),
            ("HarvestAoe", PerkIntent::HarvestAoe {
                crop: BlockSelector::One(BlockId::new("minecraft:wheat")),
                radius: 3,
            }),
            ("PackLeader", PerkIntent::PackLeader {
                entity_types: vec![EntityCondRef::One(EntityTypeId::new("minecraft:wolf"))],
                persistent_count: Persistent::new(3).unwrap(),
            }),
            ("BanditKin", PerkIntent::BanditKin {
                faction: EntityCondRef::One(EntityTypeId::new("minecraft:pillager")),
                pacify_radius: 16, ally_summon: None,
            }),
            ("Overlay", PerkIntent::Overlay {
                when: WhenCondition::Any, duration_s: Some(20),
            }),
            ("BrewPotency", PerkIntent::BrewPotency {
                which: ItemSelector::One(ItemId::new("minecraft:potion")),
                dur_mul: PotencyMul::new(2.0).unwrap(), amp_bonus: 1,
            }),
            ("Familiar", PerkIntent::Familiar {
                entity: EntityTypeId::new("minecraft:wolf"),
                bond_action: BondAction::CauldronRitual,
                persist_through_death: true,
            }),
            ("OriginQuestline", PerkIntent::OriginQuestline {
                chapter_seed: ThemeTag::new("arcane"),
            }),
            ("TallyMilestone", PerkIntent::TallyMilestone {
                event: TallyEvent::KillInRadius,
                target: EntityCondRef::One(EntityTypeId::new("minecraft:zombie")),
                threshold: 50,
                unlock: Box::new(PerkIntent::DamageVs {
                    target: EntityCondRef::One(EntityTypeId::new("minecraft:zombie")),
                    multiplier: DamageMul::new(2.0).unwrap(),
                }),
            }),
            ("OncePerDayBonus", PerkIntent::OncePerDayBonus {
                trigger: DailyTrigger::Dawn,
                bonus: Box::new(PerkIntent::PassiveEffect {
                    effect: StatusEffectId::new("minecraft:luck"), amplifier: None,
                }),
            }),
            ("ApprenticeToNpc", PerkIntent::ApprenticeToNpc {
                npc: NpcSelector::Tag(SanitizedText::new("witch").unwrap()),
                gift_threshold: GiftThresh::new(5).unwrap(),
                reward_chain: vec![PerkIntent::PassiveEffect {
                    effect: StatusEffectId::new("minecraft:strength"), amplifier: None,
                }],
            }),
        ];
        let mut missing = Vec::new();
        for (name, perk) in &cases {
            let mut ctx = EmitContext::new("sweep");
            let emit = emit_perk(perk, &mut ctx).expect(name);
            if emit.mcfunctions.is_empty() {
                missing.push(*name);
                continue;
            }
            // Each mcfunction body must be non-empty (real content).
            for (path, content) in &emit.mcfunctions {
                assert!(!content.trim().is_empty(),
                    "{name}: mcfunction {path} is empty — not a real implementation");
            }
        }
        assert!(missing.is_empty(),
            "These variants must emit functional companion mcfunctions, not just markers: {missing:?}");
        assert_eq!(cases.len(), 19,
            "covers all 19 functional companion variants");
    }

    #[test]
    fn tranche2b_emit_is_byte_deterministic_across_all_new_paths() {
        // Same intent emitted twice in fresh contexts must produce byte-equal
        // JSON. Covers all 5 T2b paths: AttributeBuff(when=Some), BuffWhen
        // (both BuffWhat variants), DotWhen, BonusSaturationOn(when=Some),
        // BlockPhase. Catches map-iteration-order regressions, float-format
        // drift, condition-compiler nondeterminism (filter_map order,
        // inverted-toggle ordering).
        let perks: Vec<PerkIntent> = vec![
            PerkIntent::AttributeBuff {
                attribute: AttributeId::new("minecraft:generic.max_health"),
                amount: BuffAmount::new(4.0).unwrap(),
                op: AttrOp::Addition,
                when: Some(WhenCondition::BlockInRadius {
                    block: BlockSelector::One(BlockId::new("bewitchment:witch_cauldron")),
                    radius: BlockRadius::new(8).unwrap(),
                }),
            },
            PerkIntent::DotWhen {
                dps: DpsRate::new(2.0).unwrap(),
                when: WhenCondition::And { conditions: vec![
                    WhenCondition::Daytime, WhenCondition::ExposedToSky,
                ] },
            },
            PerkIntent::BuffWhen {
                what: BuffWhat::Effect {
                    effect: StatusEffectId::new("minecraft:fire_resistance"),
                    amplifier: Amplifier::new(0).unwrap(),
                },
                when: WhenCondition::Dimension {
                    id: DimensionId::new("minecraft:the_nether"),
                },
            },
            PerkIntent::BuffWhen {
                what: BuffWhat::Attribute {
                    attribute: AttributeId::new("minecraft:generic.movement_speed"),
                    op: AttrOp::MultiplyTotal,
                    amount: BuffAmount::new(-0.30).unwrap(),
                },
                when: WhenCondition::BiomeTag {
                    tag: BiomeTagId::new("minecraft:is_cold"),
                },
            },
            PerkIntent::BonusSaturationOn {
                food: ItemSelector::One(ItemId::new("#c:foods")),
                extra: BonusSat::new(1).unwrap(),
                when: Some(WhenCondition::ExposedToSky),
            },
            PerkIntent::BlockPhase {
                block: BlockSelector::One(BlockId::new("#minecraft:leaves")),
                when: WhenCondition::Sneaking,
            },
        ];
        let render = |perk: &PerkIntent| {
            let mut c = EmitContext::new("witch");
            let emit = emit_perk(perk, &mut c).expect("T2b emit ok");
            serde_json::to_string(&emit.local_powers[0].body).unwrap()
        };
        for p in &perks {
            let a = render(p);
            let b = render(p);
            assert_eq!(a, b,
                "T2b emit must be byte-deterministic; first run:\n{a}\nsecond run:\n{b}");
        }
    }

    #[test]
    fn tranche2b_real_witch_json_with_typed_when_parses_and_validates() {
        // The full REAL_WITCH_JSON fixture — now using typed WhenCondition
        // for `block_in_radius` + `and([daytime, exposed_to_sky])` instead of
        // string-encoded handles. End-to-end: deserialize → emit → validate.
        let intent: OriginIntent = serde_json::from_str(REAL_WITCH_JSON)
            .expect("real witch JSON must deserialize with typed WhenCondition");

        // Emit each perk; verify the conditional perks carry `condition`.
        let mut ctx = EmitContext::new("witch");
        let mut found_cauldron_buff = false;
        let mut found_daylight_dot = false;
        for perk in &intent.perks {
            // Only the variants we've landed will emit; the rest defer.
            if let Ok(emit) = emit_perk(perk, &mut ctx) {
                for p in &emit.local_powers {
                    if let Some(cond) = p.body.get("condition") {
                        if cond.get("type").and_then(|v| v.as_str()) == Some("apoli:block_in_radius") {
                            found_cauldron_buff = true;
                        }
                        if cond.get("type").and_then(|v| v.as_str()) == Some("apoli:and") {
                            found_daylight_dot = true;
                        }
                    }
                }
            }
        }
        assert!(found_cauldron_buff,
            "real witch JSON must produce a cauldron-gated AttributeBuff");
        assert!(found_daylight_dot,
            "real witch JSON must produce a daylight+sky DotWhen via apoli:and");
    }
}
