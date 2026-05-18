//! Custom-Origins datapack ENGINE (v1).
//!
//! CONTRACT: like `recipe.rs`/`content.rs`, this is a reusable, persistence-FREE
//! engine. It owns a small typed model (`OriginsSet` = origins + powers), a
//! curated v1 starter set (`build_v1_set`), an in-code referential-integrity
//! validator (`validate`), a deterministic datapack serializer
//! (`build_origins_datapack`), and a thin instance writer
//! (`write_origins_datapack`). It does NOT own a source-of-truth file and is NOT
//! yet wired into the curator/quest pipeline — conversational/curator wiring is
//! a DELIBERATE out-of-v1 follow-up (not done here).
//!
//! WHY THIS DATAPACK ROOT: the files live under
//! `config/openloader/data/anvil-origins/` — a SIBLING of Slice 2's
//! `config/openloader/data/anvil-recipes/` and Slice 3's
//! `config/openloader/data/anvil-content/`, each with its own `pack.mcmeta`.
//! Open Loader is a generic datapack injector (it loads any
//! `config/openloader/data/<pack>/` whose `pack.mcmeta` has the right
//! `pack_format`). Origins/Apoli/Calio are NORMAL Fabric resource-reload
//! listeners that read whatever the loaded-datapack registry surfaces, so an
//! Origins datapack injected this way is picked up exactly like the recipe and
//! content datapacks already are. Mirroring this convention (vs. inventing a
//! new on-disk location) is the source-verified placement for the exact
//! MC 1.20.1 Fabric stack (Origins 1.10.2 / Apoli 2.9.2 / Calio 1.11.2).
//!
//! ORIGINS DATAPACK LAYOUT (decompiled-source-verified, NOT current wiki):
//! - `<root>/pack.mcmeta` -> `{"pack":{"pack_format":15,"description":...}}`
//!   (15 = MC 1.20.1; a wrong format SILENTLY rejects the whole pack).
//! - `<root>/data/<ns>/powers/<power_id>.json`
//! - `<root>/data/<ns>/origins/<origin_id>.json`
//! - `<root>/data/origins/origin_layers/origin.json` — the layer file is
//!   NAMESPACE-INDEPENDENT (the `origins` path segment is the Origins mod's own
//!   namespace / the layer identity `origins:origin`, NOT our pack `<ns>`).
//!   Content `{"origins":[...]}` with NO `replace` key (the loader
//!   MERGES/appends layer entries across packs; `replace:true` would wipe the
//!   10 stock origins).
//! - `<root>/assets/<ns>/lang/en_us.json` — REQUIRED. `name`/`description` on
//!   BOTH origins and powers are TRANSLATION KEYS, not literal text; without a
//!   paired lang file the raw keys show in-game.
//!
//! KNOWN in-game-only check (cannot be proven by offline parsing tests, flagged
//! per the trust-the-spec instruction): whether Open Loader surfaces
//! `assets/<ns>/lang/en_us.json` to the CLIENT (where translation keys actually
//! resolve). The spec is trusted; the file is emitted. Every other property is
//! asserted by the codec-shape tests below.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::json;

use anyhow::Context;

/// `pack_format` for a Minecraft 1.20 / 1.20.1 datapack (same constant
/// `recipe.rs`/`content.rs` use; a wrong/missing format makes the loader skip
/// the whole pack).
const PACK_FORMAT_1_20: i64 = 15;

/// The origins-datapack root: a SIBLING of the recipe/content datapacks. Each
/// engine owns its own Open Loader datapack root + `pack.mcmeta`.
const ROOT: &str = "config/openloader/data/anvil-origins";

/// Modrinth project id of Origins **core** (slug `origins`). The datapack's
/// powers are Apoli powers that core registers; Origins-Classes (`FiDptjtR`)
/// is an ADDON and must NOT by itself trigger the datapack (it would load
/// against a power engine that may not be present and surface broken origins).
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

/// The layer file path is fixed and namespace-INDEPENDENT: the `origins`
/// segment is the Origins mod's own namespace (layer identity `origins:origin`,
/// the layer that surfaces custom origins on the normal character screen).
const LAYER_PATH_SUFFIX: &str = "data/origins/origin_layers/origin.json";

// ---------------------------------------------------------------------------
// Allowlists (source-verified v1 SAFE catalog)
// ---------------------------------------------------------------------------

/// Apoli power-type ids that have a real bundled example in the exact stack, so
/// the emitted shape is known-correct. NEVER includes `origins:simple`/
/// `apoli:simple` (a hardcoded no-op sentinel).
const ALLOWED_POWER_TYPES: &[&str] = &[
    "apoli:attribute",
    "apoli:night_vision",
    "apoli:water_breathing",
    "apoli:modify_falling",
    "apoli:climbing",
    "apoli:modify_jump",
    "apoli:modify_damage_taken",
];

/// Vanilla attribute ids the `apoli:attribute` modifier may target in v1.
const ALLOWED_ATTRIBUTES: &[&str] = &[
    "minecraft:generic.max_health",
    "minecraft:generic.armor",
    "minecraft:generic.movement_speed",
    "minecraft:generic.attack_damage",
];

/// Attribute-modifier operations valid in 1.20.1 Apoli.
const ALLOWED_OPERATIONS: &[&str] = &["addition", "multiply_base", "multiply_total"];

/// Origin `impact` enum (the exact 1.20.1 Origins values).
const ALLOWED_IMPACTS: &[&str] = &["none", "low", "medium", "high"];

/// Real vanilla item ids used as origin `icon`s in v1 (must be a REAL vanilla
/// item id or the icon silently fails to render).
const ALLOWED_ICONS: &[&str] = &[
    "minecraft:netherite_chestplate",
    "minecraft:feather",
    "minecraft:torch",
];

// ---------------------------------------------------------------------------
// Typed model
// ---------------------------------------------------------------------------

/// One Apoli power. `body` is the source-verified power-type JSON shape minus
/// the translation keys (those are derived deterministically from `id`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Power {
    /// Bare power id (no namespace); becomes `data/<ns>/powers/<id>.json` and
    /// is referenced as `<ns>:<id>` from origins.
    pub id: String,
    /// The Apoli power-type id (must be in `ALLOWED_POWER_TYPES`).
    pub power_type: String,
    /// Type-specific fields (everything except `type`/`name`/`description`).
    /// Built only via the typed constructors below so the shape is the single
    /// source of truth.
    pub body: serde_json::Map<String, serde_json::Value>,
}

/// One Origin composing 1+ emitted powers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    /// Bare origin id; becomes `data/<ns>/origins/<id>.json`.
    pub id: String,
    /// Bare power ids this origin grants (each MUST be an emitted `Power.id`).
    pub powers: Vec<String>,
    /// Real vanilla item id (must be in `ALLOWED_ICONS`).
    pub icon: String,
    /// One of `ALLOWED_IMPACTS`.
    pub impact: String,
    /// Display order on the character screen (distinct per origin).
    pub order: i64,
}

/// The full typed set: every power referenced by any origin must be present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OriginsSet {
    pub origins: Vec<Origin>,
    pub powers: Vec<Power>,
}

/// Referential-integrity failure (the negative-test seam).
#[derive(Debug, Clone, PartialEq)]
pub enum IntegrityError {
    /// `id` is not lowercase `[a-z0-9_.-]+`.
    BadId(String),
    /// Two powers / two origins share an id.
    DuplicateId(String),
    /// Origin references a power id with no emitted power file.
    DanglingPowerRef { origin: String, power: String },
    /// Power type not in the SAFE catalog.
    BadPowerType { power: String, ty: String },
    /// Origin icon is not a real allowlisted vanilla item.
    BadIcon { origin: String, icon: String },
    /// Origin impact not in {none,low,medium,high}.
    BadImpact { origin: String, impact: String },
    /// `apoli:attribute` body targets a non-allowlisted attribute.
    BadAttribute { power: String, attribute: String },
    /// An attribute-modifier operation is not valid.
    BadOperation { power: String, operation: String },
    /// No origins => the layer would be empty.
    EmptySet,
}

impl std::fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IntegrityError::BadId(s) => write!(f, "invalid id `{s}` (must be lowercase [a-z0-9_.-]+)"),
            IntegrityError::DuplicateId(s) => write!(f, "duplicate id `{s}`"),
            IntegrityError::DanglingPowerRef { origin, power } => {
                write!(f, "origin `{origin}` references unemitted power `{power}`")
            }
            IntegrityError::BadPowerType { power, ty } => {
                write!(f, "power `{power}` has non-catalog type `{ty}`")
            }
            IntegrityError::BadIcon { origin, icon } => {
                write!(f, "origin `{origin}` has non-vanilla icon `{icon}`")
            }
            IntegrityError::BadImpact { origin, impact } => {
                write!(f, "origin `{origin}` has bad impact `{impact}`")
            }
            IntegrityError::BadAttribute { power, attribute } => {
                write!(f, "power `{power}` targets bad attribute `{attribute}`")
            }
            IntegrityError::BadOperation { power, operation } => {
                write!(f, "power `{power}` has bad operation `{operation}`")
            }
            IntegrityError::EmptySet => write!(f, "origins set is empty (layer would be empty)"),
        }
    }
}

impl std::error::Error for IntegrityError {}

// ---------------------------------------------------------------------------
// Typed power constructors (the single source of truth for each shape)
// ---------------------------------------------------------------------------

fn body(pairs: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match pairs {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

/// `apoli:attribute` with a single modifier.
fn power_attribute(
    id: &str,
    attribute: &str,
    operation: &str,
    value: f64,
    modifier_name: &str,
) -> Power {
    Power {
        id: id.to_string(),
        power_type: "apoli:attribute".to_string(),
        body: body(json!({
            "modifier": {
                "attribute": attribute,
                "operation": operation,
                "value": value,
                "name": modifier_name,
            }
        })),
    }
}

/// `apoli:night_vision` with a strength.
fn power_night_vision(id: &str, strength: f64) -> Power {
    Power {
        id: id.to_string(),
        power_type: "apoli:night_vision".to_string(),
        body: body(json!({ "strength": strength })),
    }
}

/// `apoli:water_breathing` (no fields).
fn power_water_breathing(id: &str) -> Power {
    Power {
        id: id.to_string(),
        power_type: "apoli:water_breathing".to_string(),
        body: serde_json::Map::new(),
    }
}

/// `apoli:modify_falling` (slow-fall) — velocity + no fall damage.
fn power_modify_falling(id: &str, velocity: f64, take_fall_damage: bool) -> Power {
    Power {
        id: id.to_string(),
        power_type: "apoli:modify_falling".to_string(),
        body: body(json!({
            "velocity": velocity,
            "take_fall_damage": take_fall_damage,
        })),
    }
}

/// `apoli:climbing` (no fields).
fn power_climbing(id: &str) -> Power {
    Power {
        id: id.to_string(),
        power_type: "apoli:climbing".to_string(),
        body: serde_json::Map::new(),
    }
}

/// `apoli:modify_jump` with a single modifier.
fn power_modify_jump(id: &str, operation: &str, value: f64, modifier_name: &str) -> Power {
    Power {
        id: id.to_string(),
        power_type: "apoli:modify_jump".to_string(),
        body: body(json!({
            "modifier": {
                "operation": operation,
                "value": value,
                "name": modifier_name,
            }
        })),
    }
}

/// `apoli:modify_damage_taken` with a single modifier.
fn power_modify_damage_taken(
    id: &str,
    operation: &str,
    value: f64,
    modifier_name: &str,
) -> Power {
    Power {
        id: id.to_string(),
        power_type: "apoli:modify_damage_taken".to_string(),
        body: body(json!({
            "modifier": {
                "operation": operation,
                "value": value,
                "name": modifier_name,
            }
        })),
    }
}

// ---------------------------------------------------------------------------
// Curated v1 set: 3 starter origins spanning 7 catalog power types
// ---------------------------------------------------------------------------

/// The curated v1 starter set.
///
/// - Tank — `apoli:attribute` max_health + `apoli:attribute` armor +
///   `apoli:modify_damage_taken` (icon netherite_chestplate, high impact).
/// - Mobility — `apoli:modify_jump` + `apoli:modify_falling` (slow-fall) +
///   `apoli:climbing` (icon feather, medium impact).
/// - Survivalist — `apoli:night_vision` + `apoli:water_breathing` +
///   `apoli:attribute` movement_speed (icon torch, low impact).
///
/// Distinct power ids (one file per power, 9 total); union of types =
/// {attribute, modify_damage_taken, modify_jump, modify_falling, climbing,
/// night_vision, water_breathing} = 7 of the 7-entry catalog. Deterministic.
pub fn build_v1_set() -> OriginsSet {
    let powers = vec![
        // Tank
        power_attribute(
            "tank_max_health",
            "minecraft:generic.max_health",
            "addition",
            6.0,
            "Anvil Tank Max Health",
        ),
        power_attribute(
            "tank_armor",
            "minecraft:generic.armor",
            "addition",
            4.0,
            "Anvil Tank Armor",
        ),
        power_modify_damage_taken(
            "tank_resilience",
            "multiply_base",
            -0.25,
            "Anvil Tank Resilience",
        ),
        // Mobility
        power_modify_jump("mobility_high_jump", "multiply_base", 0.5, "Anvil High Jump"),
        power_modify_falling("mobility_slow_fall", 0.04, false),
        power_climbing("mobility_climbing"),
        // Survivalist
        power_night_vision("survivalist_night_vision", 1.0),
        power_water_breathing("survivalist_water_breathing"),
        power_attribute(
            "survivalist_swift",
            "minecraft:generic.movement_speed",
            "multiply_base",
            0.15,
            "Anvil Survivalist Swiftness",
        ),
    ];

    let origins = vec![
        Origin {
            id: "tank".to_string(),
            powers: vec![
                "tank_max_health".to_string(),
                "tank_armor".to_string(),
                "tank_resilience".to_string(),
            ],
            icon: "minecraft:netherite_chestplate".to_string(),
            impact: "high".to_string(),
            order: 0,
        },
        Origin {
            id: "mobility".to_string(),
            powers: vec![
                "mobility_high_jump".to_string(),
                "mobility_slow_fall".to_string(),
                "mobility_climbing".to_string(),
            ],
            icon: "minecraft:feather".to_string(),
            impact: "medium".to_string(),
            order: 1,
        },
        Origin {
            id: "survivalist".to_string(),
            powers: vec![
                "survivalist_night_vision".to_string(),
                "survivalist_water_breathing".to_string(),
                "survivalist_swift".to_string(),
            ],
            icon: "minecraft:torch".to_string(),
            impact: "low".to_string(),
            order: 2,
        },
    ];

    OriginsSet { origins, powers }
}

// ---------------------------------------------------------------------------
// Referential-integrity validation (the negative-test seam)
// ---------------------------------------------------------------------------

/// True iff `id` is a non-empty lowercase `[a-z0-9_.-]+` token (equal to its
/// filename when emitted).
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
}

/// Validate the set's referential integrity BEFORE any emission. Stable issue
/// order: powers first (id, type, attribute/operation), then origins (id,
/// dangling refs, icon, impact), then the emptiness check.
pub fn validate(set: &OriginsSet) -> Result<(), IntegrityError> {
    use std::collections::BTreeSet;

    // Powers: id shape, duplicates, catalog type, attribute/operation bodies.
    let mut seen_powers: BTreeSet<&str> = BTreeSet::new();
    for p in &set.powers {
        if !is_valid_id(&p.id) {
            return Err(IntegrityError::BadId(p.id.clone()));
        }
        if !seen_powers.insert(p.id.as_str()) {
            return Err(IntegrityError::DuplicateId(p.id.clone()));
        }
        if !ALLOWED_POWER_TYPES.contains(&p.power_type.as_str()) {
            return Err(IntegrityError::BadPowerType {
                power: p.id.clone(),
                ty: p.power_type.clone(),
            });
        }
        // Every modifier (single `modifier` or `modifiers[]`) must carry a
        // valid operation; `apoli:attribute` modifiers also a valid attribute.
        let is_attr = p.power_type == "apoli:attribute";
        let mut mods: Vec<&serde_json::Value> = Vec::new();
        if let Some(m) = p.body.get("modifier") {
            mods.push(m);
        }
        if let Some(serde_json::Value::Array(a)) = p.body.get("modifiers") {
            for m in a {
                mods.push(m);
            }
        }
        for m in mods {
            if let Some(op) = m.get("operation").and_then(|v| v.as_str()) {
                if !ALLOWED_OPERATIONS.contains(&op) {
                    return Err(IntegrityError::BadOperation {
                        power: p.id.clone(),
                        operation: op.to_string(),
                    });
                }
            }
            if is_attr {
                if let Some(attr) = m.get("attribute").and_then(|v| v.as_str()) {
                    if !ALLOWED_ATTRIBUTES.contains(&attr) {
                        return Err(IntegrityError::BadAttribute {
                            power: p.id.clone(),
                            attribute: attr.to_string(),
                        });
                    }
                }
            }
        }
    }

    // Origins: id shape, duplicates, dangling power refs, icon, impact.
    let mut seen_origins: BTreeSet<&str> = BTreeSet::new();
    for o in &set.origins {
        if !is_valid_id(&o.id) {
            return Err(IntegrityError::BadId(o.id.clone()));
        }
        if !seen_origins.insert(o.id.as_str()) {
            return Err(IntegrityError::DuplicateId(o.id.clone()));
        }
        for pid in &o.powers {
            if !seen_powers.contains(pid.as_str()) {
                return Err(IntegrityError::DanglingPowerRef {
                    origin: o.id.clone(),
                    power: pid.clone(),
                });
            }
        }
        if !ALLOWED_ICONS.contains(&o.icon.as_str()) {
            return Err(IntegrityError::BadIcon {
                origin: o.id.clone(),
                icon: o.icon.clone(),
            });
        }
        if !ALLOWED_IMPACTS.contains(&o.impact.as_str()) {
            return Err(IntegrityError::BadImpact {
                origin: o.id.clone(),
                impact: o.impact.clone(),
            });
        }
    }

    if set.origins.is_empty() {
        return Err(IntegrityError::EmptySet);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Deterministic datapack serializer
// ---------------------------------------------------------------------------

fn lang_name_key(ns: &str, kind: &str, id: &str) -> String {
    // origins use `origin.<ns>.<id>.name`; powers use `power.<ns>.<id>.name`.
    format!("{kind}.{ns}.{id}.name")
}

fn lang_desc_key(ns: &str, kind: &str, id: &str) -> String {
    format!("{kind}.{ns}.{id}.description")
}

/// Humanize a bare id (`tank_max_health` -> `Tank Max Health`) for readable
/// fallback lang text.
fn humanize(id: &str) -> String {
    id.split(|c| c == '_' || c == '.' || c == '-')
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// Single source of truth for each in-game-readable string. Each helper is
// called ONCE per origin/power in `emit`; its return value feeds BOTH the
// literal `{"text": ...}` Component (what renders in-game with no resource
// pack) AND the `assets/<ns>/lang/en_us.json` value (kept for future
// localization), so the two can never drift.

/// Readable display name for a power (same string as its `en_us.json` value).
fn power_name_text(id: &str) -> String {
    humanize(id)
}

/// Readable description for a power (same string as its `en_us.json` value).
fn power_desc_text(id: &str) -> String {
    format!("{} power.", humanize(id))
}

/// Readable display name for an origin (same string as its `en_us.json` value).
fn origin_name_text(id: &str) -> String {
    humanize(id)
}

/// Readable description for an origin (same string as its `en_us.json` value).
fn origin_desc_text(id: &str, impact: &str) -> String {
    format!("The {} origin. Impact: {}.", humanize(id), impact)
}

/// Serialize a JSON value with the existing engine idiom: pretty + trailing
/// newline (matches `recipe.rs`/`content.rs`; serde_json without
/// `preserve_order` sorts keys, so output is deterministic).
fn to_file(v: &serde_json::Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}

/// Build the full deterministic Origins datapack: `Vec<(relative path, file
/// content)>`. Emission order is stable: `pack.mcmeta`, then powers sorted by
/// id, then origins sorted by id, then the layer file, then `en_us.json`.
///
/// The curated v1 set is invariantly valid, so `validate` here is an honest
/// assertion (a panic would mean the v1 set itself is broken — a build bug,
/// caught by the integrity test).
pub fn build_origins_datapack(namespace: &str) -> Vec<(String, String)> {
    let set = build_v1_set();
    validate(&set).expect("curated v1 origins set is invariantly valid");
    emit(&set, namespace)
}

/// Pure emission of a PRE-VALIDATED set (split out so determinism is testable
/// without rebuilding v1). Callers other than tests should use
/// `build_origins_datapack`.
fn emit(set: &OriginsSet, ns: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut lang = serde_json::Map::new();

    // 1. pack.mcmeta — MANDATORY, pack_format is a NUMBER (15 for 1.20.1).
    let mcmeta = json!({
        "pack": {
            "pack_format": PACK_FORMAT_1_20,
            "description": "Anvil custom origins",
        }
    });
    out.push((format!("{ROOT}/pack.mcmeta"), to_file(&mcmeta)));

    // 2. Powers, sorted by id for stable order.
    let mut powers: Vec<&Power> = set.powers.iter().collect();
    powers.sort_by(|a, b| a.id.cmp(&b.id));
    for p in powers {
        let name_key = lang_name_key(ns, "power", &p.id);
        let desc_key = lang_desc_key(ns, "power", &p.id);
        // Single-sourced readable strings: the SAME values feed the literal
        // `{"text": ...}` Component below AND the en_us.json entries.
        let name_text = power_name_text(&p.id);
        let desc_text = power_desc_text(&p.id);

        // type + literal name/description Components + the type-specific body.
        // `name`/`description` are emitted as Minecraft text Component OBJECTS
        // (`{"text": ...}`), not bare translation-key strings: Origins' Calio
        // codec accepts a Component, so these render in-game with NO client
        // resource pack. Build through a Map so the shape is the single source
        // of truth and serde sorts keys deterministically.
        let mut obj = serde_json::Map::new();
        obj.insert("type".to_string(), json!(p.power_type));
        obj.insert("name".to_string(), json!({ "text": name_text }));
        obj.insert("description".to_string(), json!({ "text": desc_text }));
        for (k, v) in &p.body {
            obj.insert(k.clone(), v.clone());
        }
        out.push((
            format!("{ROOT}/data/{ns}/powers/{}.json", p.id),
            to_file(&serde_json::Value::Object(obj)),
        ));

        // Lang file kept (harmless; enables future localization) — same
        // single-sourced strings, so the two can never drift.
        lang.insert(name_key, json!(name_text));
        lang.insert(desc_key, json!(desc_text));
    }

    // 3. Origins, sorted by id for stable order.
    let mut origins: Vec<&Origin> = set.origins.iter().collect();
    origins.sort_by(|a, b| a.id.cmp(&b.id));
    for o in &origins {
        let name_key = lang_name_key(ns, "origin", &o.id);
        let desc_key = lang_desc_key(ns, "origin", &o.id);
        // Single-sourced readable strings: the SAME values feed the literal
        // `{"text": ...}` Component below AND the en_us.json entries.
        let name_text = origin_name_text(&o.id);
        let desc_text = origin_desc_text(&o.id, &o.impact);

        let power_refs: Vec<String> =
            o.powers.iter().map(|p| format!("{ns}:{p}")).collect();

        // `name`/`description` are emitted as Minecraft text Component OBJECTS
        // (`{"text": ...}`), not bare translation-key strings, so they render
        // in-game with NO client resource pack (Calio's codec accepts a
        // Component).
        let v = json!({
            "powers": power_refs,
            "icon": { "item": o.icon },
            "impact": o.impact,
            "order": o.order,
            "name": { "text": name_text },
            "description": { "text": desc_text },
        });
        out.push((
            format!("{ROOT}/data/{ns}/origins/{}.json", o.id),
            to_file(&v),
        ));

        // Lang file kept (harmless; enables future localization) — same
        // single-sourced strings, so the two can never drift.
        lang.insert(name_key, json!(name_text));
        lang.insert(desc_key, json!(desc_text));
    }

    // 4. The layer file — fixed namespace-INDEPENDENT path, NO `replace` key
    //    (the loader merges/appends across packs; `replace:true` would wipe
    //    the 10 stock origins). Lists every emitted origin in sorted order.
    let layer_origins: Vec<String> =
        origins.iter().map(|o| format!("{ns}:{}", o.id)).collect();
    let layer = json!({ "origins": layer_origins });
    out.push((
        format!("{ROOT}/{LAYER_PATH_SUFFIX}"),
        to_file(&layer),
    ));

    // 5. en_us.json — REQUIRED; maps every emitted name/description key to
    //    readable text (serde sorts keys, so this is deterministic).
    out.push((
        format!("{ROOT}/assets/{ns}/lang/en_us.json"),
        to_file(&serde_json::Value::Object(lang)),
    ));

    out
}

// ---------------------------------------------------------------------------
// Instance writer
// ---------------------------------------------------------------------------

/// Write the v1 Origins datapack under `instance_dir` (mirrors
/// `quest::write_quests`: `create_dir_all` the instance dir, then per
/// (rel, contents) `create_dir_all` the parent and `fs::write`). The datapack
/// lands at `<instance>/config/openloader/data/anvil-origins/**`, a sibling of
/// the recipe/content datapacks.
pub fn write_origins_datapack(instance_dir: &Path, namespace: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(instance_dir)
        .with_context(|| format!("creating instance dir {}", instance_dir.display()))?;

    for (rel, contents) in build_origins_datapack(namespace) {
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

// ---------------------------------------------------------------------------
// Tests — REAL codec-shape parsing ("would the game load this")
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeMap;

    const NS: &str = "anvil";

    /// Index emitted files by relative path for lookup.
    fn index(files: &[(String, String)]) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for (p, c) in files {
            assert!(
                m.insert(p.clone(), c.clone()).is_none(),
                "duplicate emitted path {p}"
            );
        }
        m
    }

    fn parse(files: &BTreeMap<String, String>, path: &str) -> Value {
        let raw = files
            .get(path)
            .unwrap_or_else(|| panic!("expected emitted file {path}"));
        assert!(
            raw.ends_with('\n'),
            "file {path} must end with a trailing newline (engine idiom)"
        );
        serde_json::from_str(raw).unwrap_or_else(|e| panic!("file {path} must parse: {e}"))
    }

    // (1) pack.mcmeta parses and pack_format == 15 (a NUMBER).
    #[test]
    fn origins_pack_mcmeta_format_15() {
        let files = index(&build_origins_datapack(NS));
        let mc = parse(&files, &format!("{ROOT}/pack.mcmeta"));
        assert_eq!(mc["pack"]["pack_format"], 15);
        assert!(
            mc["pack"]["pack_format"].is_i64(),
            "pack_format must be a number, not a string"
        );
        assert!(mc["pack"]["description"].is_string());
    }

    // (2) Layer file is at exactly data/origins/origin_layers/origin.json,
    //     parses, references every emitted origin, and has NO `replace` key.
    #[test]
    fn origins_layer_file_exact_path_no_replace() {
        let files = index(&build_origins_datapack(NS));
        let layer_path = format!("{ROOT}/data/origins/origin_layers/origin.json");
        let layer = parse(&files, &layer_path);

        // NO `replace` key (would wipe the 10 stock origins).
        assert!(
            layer.get("replace").is_none(),
            "layer file must NOT carry a `replace` key (loader merges/appends)"
        );

        // Collect every emitted origin id from the actual origin files.
        let emitted_origins: Vec<String> = files
            .keys()
            .filter_map(|p| {
                p.strip_prefix(&format!("{ROOT}/data/{NS}/origins/"))
                    .and_then(|s| s.strip_suffix(".json"))
                    .map(|s| format!("{NS}:{s}"))
            })
            .collect();
        assert!(!emitted_origins.is_empty(), "expected emitted origins");

        let listed: Vec<String> = layer["origins"]
            .as_array()
            .expect("layer `origins` must be an array")
            .iter()
            .map(|v| v.as_str().expect("origin ref must be a string").to_string())
            .collect();

        for o in &emitted_origins {
            assert!(
                listed.contains(o),
                "layer must reference emitted origin {o}; layer lists {listed:?}"
            );
        }
        assert_eq!(
            listed.len(),
            emitted_origins.len(),
            "layer must list exactly the emitted origins (no extras/missing)"
        );
    }

    // (3) Every emitted origin file parses; impact is valid; icon resolves to
    //     a non-empty item id; every powers[] entry has a matching emitted
    //     power file.
    #[test]
    fn origins_files_valid_and_powers_resolve() {
        let files = index(&build_origins_datapack(NS));

        let origin_paths: Vec<String> = files
            .keys()
            .filter(|p| {
                p.starts_with(&format!("{ROOT}/data/{NS}/origins/")) && p.ends_with(".json")
            })
            .cloned()
            .collect();
        assert!(!origin_paths.is_empty(), "expected >= 1 origin file");

        for op in &origin_paths {
            let o = parse(&files, op);

            // impact ∈ {none,low,medium,high}
            let impact = o["impact"].as_str().expect("origin impact must be a string");
            assert!(
                ALLOWED_IMPACTS.contains(&impact),
                "origin {op} impact `{impact}` not in {ALLOWED_IMPACTS:?}"
            );

            // icon resolves to a non-empty item id (object {item:..} form).
            let icon = o["icon"]["item"]
                .as_str()
                .expect("origin icon must be {\"item\":\"<id>\"}");
            assert!(!icon.is_empty(), "origin {op} icon item is empty");
            assert!(
                icon.starts_with("minecraft:"),
                "origin {op} icon `{icon}` must be a real vanilla item id"
            );

            // every powers[] entry -> matching emitted power file.
            let powers = o["powers"].as_array().expect("origin powers must be array");
            assert!(!powers.is_empty(), "origin {op} must grant >= 1 power");
            for pr in powers {
                let pref = pr.as_str().expect("power ref must be a string");
                let bare = pref
                    .strip_prefix(&format!("{NS}:"))
                    .unwrap_or_else(|| panic!("power ref {pref} must be {NS}-namespaced"));
                let expected = format!("{ROOT}/data/{NS}/powers/{bare}.json");
                assert!(
                    files.contains_key(&expected),
                    "origin {op} references power `{pref}` but {expected} was not emitted"
                );
            }
        }
    }

    // (4) Every emitted power file parses; type is a catalog id.
    #[test]
    fn origins_power_files_have_catalog_type() {
        let files = index(&build_origins_datapack(NS));
        let power_paths: Vec<String> = files
            .keys()
            .filter(|p| {
                p.starts_with(&format!("{ROOT}/data/{NS}/powers/")) && p.ends_with(".json")
            })
            .cloned()
            .collect();
        assert!(power_paths.len() >= 7, "expected >= 7 power files (v1 catalog span)");

        let mut seen_types = std::collections::BTreeSet::new();
        for pp in &power_paths {
            let p = parse(&files, pp);
            let ty = p["type"].as_str().expect("power type must be a string");
            assert!(
                ALLOWED_POWER_TYPES.contains(&ty),
                "power {pp} type `{ty}` not in catalog {ALLOWED_POWER_TYPES:?}"
            );
            // filename must equal the bare id (id == filename invariant).
            let bare = pp
                .strip_prefix(&format!("{ROOT}/data/{NS}/powers/"))
                .and_then(|s| s.strip_suffix(".json"))
                .unwrap();
            assert!(is_valid_id(bare), "power filename `{bare}` is not a valid id");
            seen_types.insert(ty.to_string());
        }
        // v1 spans 5-7 distinct catalog types.
        assert!(
            (5..=7).contains(&seen_types.len()),
            "v1 must span 5-7 distinct catalog power types, spans {}",
            seen_types.len()
        );
    }

    // (5) Every emitted origin/power still has BOTH its name + description
    //     keys defined (non-empty) in assets/<ns>/lang/en_us.json. The lang
    //     file is still emitted (for future localization) even though the JSON
    //     now carries literal Components, so the key is derived from the file
    //     path (origin/power id), not from the now-object `name` field.
    #[test]
    fn origins_every_lang_key_is_defined() {
        let files = index(&build_origins_datapack(NS));
        let lang = parse(&files, &format!("{ROOT}/assets/{NS}/lang/en_us.json"));
        let lang_obj = lang.as_object().expect("en_us.json must be an object");

        let mut referenced: Vec<String> = Vec::new();
        for path in files.keys() {
            if !path.ends_with(".json") {
                continue;
            }
            let kind_id = if let Some(s) =
                path.strip_prefix(&format!("{ROOT}/data/{NS}/origins/"))
            {
                s.strip_suffix(".json").map(|id| ("origin", id))
            } else if let Some(s) =
                path.strip_prefix(&format!("{ROOT}/data/{NS}/powers/"))
            {
                s.strip_suffix(".json").map(|id| ("power", id))
            } else {
                None
            };
            if let Some((kind, id)) = kind_id {
                referenced.push(lang_name_key(NS, kind, id));
                referenced.push(lang_desc_key(NS, kind, id));
            }
        }
        assert!(!referenced.is_empty(), "expected referenced lang keys");

        for key in &referenced {
            assert!(
                lang_obj.contains_key(key),
                "lang key `{key}` referenced but missing from en_us.json"
            );
            assert!(
                lang_obj[key].as_str().map(|s| !s.is_empty()).unwrap_or(false),
                "lang key `{key}` maps to empty/non-string text"
            );
        }
    }

    // (5b) The in-game-readable invariant: every emitted origin/power file
    //      emits `name` AND `description` as a literal Minecraft text
    //      Component OBJECT (`{"text": "<non-empty>"}`), NOT a bare
    //      translation-key string — so they render with NO client resource
    //      pack. (The lang file is still emitted, asserted by test (5).)
    #[test]
    fn origins_name_and_description_are_literal_components() {
        let files = index(&build_origins_datapack(NS));

        let json_paths: Vec<String> = files
            .keys()
            .filter(|p| {
                (p.starts_with(&format!("{ROOT}/data/{NS}/origins/"))
                    || p.starts_with(&format!("{ROOT}/data/{NS}/powers/")))
                    && p.ends_with(".json")
            })
            .cloned()
            .collect();
        assert!(
            !json_paths.is_empty(),
            "expected >= 1 origin/power file to assert against"
        );

        for path in &json_paths {
            let v = parse(&files, path);
            for field in ["name", "description"] {
                let f = &v[field];
                // Explicitly NOT a bare JSON string.
                assert!(
                    !f.is_string(),
                    "{path} `{field}` must NOT be a bare string (would need a \
                     resource pack to resolve); got {f}"
                );
                // It is a JSON object with a non-empty `text` member.
                assert!(
                    f.is_object(),
                    "{path} `{field}` must be a literal Component object, got {f}"
                );
                let text = f["text"].as_str();
                assert!(
                    text.is_some(),
                    "{path} `{field}`.text must be a string, got {f}"
                );
                assert!(
                    !text.unwrap().is_empty(),
                    "{path} `{field}`.text must be non-empty, got {f}"
                );
            }
        }
    }

    // (6) Determinism: building twice is byte-identical.
    #[test]
    fn origins_build_is_deterministic() {
        let a = build_origins_datapack(NS);
        let b = build_origins_datapack(NS);
        assert_eq!(a, b, "two builds must be byte-identical");
        // Also stable across a different (still valid) namespace shape.
        let c = build_origins_datapack("anvil");
        assert_eq!(a, c);
    }

    // (7) Negative/integrity test: an origin referencing a non-emitted power
    //     is rejected by the integrity check (proves the guard works).
    #[test]
    fn origins_dangling_power_ref_is_rejected() {
        let mut set = build_v1_set();
        // Sanity: the curated set is valid.
        assert!(validate(&set).is_ok(), "v1 set must validate");

        // Point an origin at a power id that has no emitted power file.
        set.origins[0]
            .powers
            .push("ghost_power_that_does_not_exist".to_string());

        match validate(&set) {
            Err(IntegrityError::DanglingPowerRef { origin, power }) => {
                assert_eq!(origin, "tank");
                assert_eq!(power, "ghost_power_that_does_not_exist");
            }
            other => panic!("expected DanglingPowerRef, got {other:?}"),
        }
    }

    // (8) Extra integrity guards: bad type / bad icon / bad impact / bad
    //     attribute / dup id / empty set are all rejected.
    #[test]
    fn origins_integrity_guards_catch_each_class() {
        // Bad power type (incl. the forbidden no-op sentinel).
        let mut s = build_v1_set();
        s.powers[0].power_type = "apoli:simple".to_string();
        assert!(matches!(
            validate(&s),
            Err(IntegrityError::BadPowerType { .. })
        ));

        // Bad icon (not a real allowlisted vanilla item).
        let mut s = build_v1_set();
        s.origins[0].icon = "minecraft:not_a_real_item".to_string();
        assert!(matches!(validate(&s), Err(IntegrityError::BadIcon { .. })));

        // Bad impact.
        let mut s = build_v1_set();
        s.origins[0].impact = "catastrophic".to_string();
        assert!(matches!(validate(&s), Err(IntegrityError::BadImpact { .. })));

        // Bad attribute target.
        let mut s = build_v1_set();
        s.powers[0] = power_attribute(
            "tank_max_health",
            "minecraft:generic.luck",
            "addition",
            1.0,
            "X",
        );
        assert!(matches!(
            validate(&s),
            Err(IntegrityError::BadAttribute { .. })
        ));

        // Bad operation.
        let mut s = build_v1_set();
        s.powers[0] = power_attribute(
            "tank_max_health",
            "minecraft:generic.max_health",
            "set",
            1.0,
            "X",
        );
        assert!(matches!(
            validate(&s),
            Err(IntegrityError::BadOperation { .. })
        ));

        // Duplicate power id.
        let mut s = build_v1_set();
        let dup = s.powers[0].clone();
        s.powers.push(dup);
        assert!(matches!(
            validate(&s),
            Err(IntegrityError::DuplicateId(_))
        ));

        // Bad id shape.
        let mut s = build_v1_set();
        s.origins[0].id = "Tank Origin!".to_string();
        assert!(matches!(validate(&s), Err(IntegrityError::BadId(_))));

        // Empty set.
        let s = OriginsSet {
            origins: vec![],
            powers: vec![],
        };
        assert!(matches!(validate(&s), Err(IntegrityError::EmptySet)));
    }

    // (9) write_origins_datapack lands files under the sibling root in a temp
    //     dir and they parse off disk (mirrors the quest writer pattern).
    #[test]
    fn origins_write_to_instance_roundtrips() {
        let tmp = std::env::temp_dir().join(format!(
            "anvil_origins_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        write_origins_datapack(&tmp, NS).expect("write must succeed");

        let mcmeta_disk = tmp
            .join(ROOT)
            .join("pack.mcmeta");
        let raw = std::fs::read_to_string(&mcmeta_disk).expect("pack.mcmeta on disk");
        let v: Value = serde_json::from_str(&raw).expect("disk pack.mcmeta parses");
        assert_eq!(v["pack"]["pack_format"], 15);

        let layer_disk = tmp
            .join(ROOT)
            .join("data/origins/origin_layers/origin.json");
        assert!(layer_disk.exists(), "layer file must exist on disk at exact path");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
