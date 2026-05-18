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
//! - The layer file appends to the stock chooser with
//!   `{"replace": false, "origins": [...]}`.

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
    "origins:slow_falling",
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
/// strings; `impact` a number; the layer appends with `replace:false`; only
/// `Local` powers get a file (a `Shipped` ref is just listed in the origin).
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

    // Layer: append to the stock chooser (replace:false).
    let layer_origins: Vec<String> =
        origins.iter().map(|o| format!("{ns}:{}", o.id)).collect();
    out.push((
        format!("{ROOT}/{LAYER_PATH_SUFFIX}"),
        to_file(&json!({ "replace": false, "origins": layer_origins })),
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

/// Write a MODEL-AUTHORED, already-`Validated` origin set (the
/// `tool_generate_origins` path). Same prune-then-write semantics.
pub fn write_validated_origins(
    instance_dir: &Path,
    namespace: &str,
    v: &Validated,
) -> anyhow::Result<()> {
    write_files(instance_dir, emit(v, namespace))
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
        "slow_falling" => (
            "Slow Falling",
            "You fall slowly and take no fall damage.",
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
    fn no_emitted_power_uses_an_invalid_type_and_layer_has_replace_false() {
        let files = index(&build_origins_datapack(NS));
        let layer = parse(&files, &format!("{ROOT}/{LAYER_PATH_SUFFIX}"));
        assert_eq!(layer["replace"], json!(false), "layer must be replace:false");
        assert!(layer["origins"].as_array().is_some_and(|a| !a.is_empty()));
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
