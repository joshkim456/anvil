//! AI-authored vanilla-primitive CONTENT datapack ENGINE (Slice 3).
//!
//! CONTRACT: like `recipe.rs`, this is a reusable, persistence-FREE engine. It
//! owns the content IR (`ContentSpec` and its sub-shapes), the deterministic
//! Open Loader datapack serializer (`to_openloader_files`), and the
//! grounding/atomicity validator (`validate_content`). It does NOT own a
//! source-of-truth file: the quest graph (`crate::quest`,
//! `<instance>/anvil-quests.json`) is the single source of truth and a content
//! facet is a first-class quest-node field (`QuestNode.content:
//! Option<ContentSpec>`). `crate::quest::write_quests` aggregates every node's
//! content facet and writes this datapack; there is no `anvil-content.json`.
//!
//! WHY A SIBLING DATAPACK: the files live under
//! `config/openloader/data/anvil-content/` — a SIBLING of Slice 2's
//! `config/openloader/data/anvil-recipes/`, NOT the same root. Each slice owns
//! its own Open Loader datapack root + `pack.mcmeta`, so the two never clobber
//! each other's contents and "anvil-recipes" stays honestly recipe-only. Open
//! Loader's convention is `config/openloader/data/<pack>/` with a mandatory
//! `pack.mcmeta`; a sibling pack satisfies that exactly (verified against the
//! same Open Loader behaviour `recipe.rs` documents — pack_format 15, 1.20.1).
//!
//! THE LOADER-AGNOSTIC TOKEN MECHANISM (improves on the design doc's
//! loot-modifier hedge — see progression_system_design.md §3B / §6 #12): the
//! boss's quest token is granted by the KILL-DETECTION ADVANCEMENT's
//! `rewards.function`, NOT a loot-table modifier. Loot modifiers are
//! Forge/NeoForge-specific (Global Loot Modifiers) and differ on Fabric — an
//! advancement -> function -> `give` is fully loader-agnostic, datapack-only,
//! and makes "token atomicity" trivially enforceable (a valid `ContentSpec`
//! deterministically emits ALL of {summon fn, tick fn, kill-advancement,
//! onkill fn that gives the token, trigger, the GatherItem-on-token task} or
//! none). The token is a vanilla item carrying custom NBT
//! `{display:{Name:...},anvil_token:"<hex>"}` so it is forgery-proof and needs
//! no custom item/mod.
//!
//! 1.20.1 DATAPACK SHAPES (NOT the current minecraft.wiki, which is 1.21):
//! - PLURAL folders: `functions/`, `advancements/`, plus `recipes/` (the totem
//!   trigger reuses the recipe folder convention, verified in `recipe.rs`).
//! - `minecraft:player_killed_entity` advancement: in 1.20.1 the `entity`
//!   condition is an INLINE entity-predicate object
//!   (`"entity": { "type": ..., "nbt": ... }`). 1.20.2+ switched this to a
//!   list of predicate conditions; we pin 1.20.1 and emit the inline form.
//! - Item/entity NBT matching is the `nbt` SNBT-string field (1.20.1 uses NBT;
//!   1.20.5+ uses data components — out of scope, we pin 1.20.1).
//!
//! KNOWN-FRAGILE (v1 hedge, like the existing quest.rs/recipe.rs hedges): the
//! Heracles `GatherItemTask.nbt` is documented as a partial `NbtPredicate`
//! compound match in the 1.20.x codec, so the token task's `{anvil_token:"X"}`
//! predicate should match the give-stack's superset compound — but in-game
//! partial-vs-strict behaviour is the next user-verifiable check, not proven
//! offline. The grounding gate remains the correctness seam regardless.

use serde::{Deserialize, Serialize};

use crate::quest::{stable_hex, AllowedIndex, RecipeGrounding};

/// `pack_format` for a Minecraft 1.20 / 1.20.1 datapack (same constant
/// `recipe.rs` uses; Open Loader skips a folder with a wrong/missing format).
const PACK_FORMAT_1_20: i64 = 15;

/// The content-datapack root: a SIBLING of the Slice-2 recipe datapack.
const ROOT: &str = "config/openloader/data/anvil-content";

// ---------------------------------------------------------------------------
// IR
// ---------------------------------------------------------------------------

/// A provisioned-content facet on a quest node. v1 supports a `boss` kind
/// (a real summoned, named, bossbar boss built from a registered entity that
/// drops a unique quest token). The tagged-union shape leaves room for
/// `site`/`gate` variants without a wire break.
///
/// `#[serde(tag = "kind", rename_all = "snake_case")]` so a content facet is
/// `{"kind":"boss", ...}`; future kinds add cleanly. The whole facet is
/// `#[serde(default)] Option<ContentSpec>` on the node so every pre-existing
/// graph / `anvil-quests.json` / test decodes byte-unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentSpec {
    /// A summoned, attribute-buffed, named, bossbar boss built from a
    /// registered entity, whose tag-filtered kill grants a unique NBT token.
    Boss {
        /// The base REGISTERED entity to summon (grounded via Slice-1, e.g.
        /// `minecraft:wither_skeleton`). A fabricated id is a hard reject.
        entity: String,
        /// The boss's in-game name (CustomName + bossbar title).
        display_name: String,
        /// Optional attribute buffs; sane boss defaults when omitted.
        #[serde(default)]
        attributes: BossAttributes,
        /// Optional per-slot equipment (item ids; grounded via Slice-1).
        #[serde(default, skip_serializing_if = "Equipment::is_empty")]
        equipment: Equipment,
        /// Bossbar color (`red`/`blue`/`green`/`yellow`/`pink`/`purple`/
        /// `white`); default `red`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bossbar_color: Option<String>,
        /// The vanilla item used as the token carrier; default
        /// `minecraft:nether_star`. Grounded via Slice-1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_item: Option<String>,
        /// How the boss is summoned. v1 default `totem` = an ALTAR: drop a
        /// nether star + this boss's deterministic offering block together;
        /// `command` = a `/trigger`; `region` reserved (omitted v1).
        #[serde(default)]
        trigger: Trigger,
        /// The token's in-game display name (defaults to
        /// `"<display_name> Token"` when omitted).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_name: Option<String>,
    },
}

/// Optional attribute buffs. `None` = the vanilla base; a value overrides via a
/// summon-NBT `Attributes` entry. Sane BOSS DEFAULTS are applied by
/// `effective_attributes` (a content boss with no attributes is still a boss,
/// not a vanilla mob).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BossAttributes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_health: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack_damage: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knockback_resistance: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub movement_speed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_range: Option<f64>,
}

impl BossAttributes {
    /// Boss-defaulted attribute values: every unset attribute gets a sane
    /// "this is a boss, not a mob" default so a bare `{"kind":"boss"}` still
    /// produces a real fight. Returned as a stable-ordered list of
    /// `(attribute_id, base)` pairs for the summon NBT.
    fn effective(&self) -> Vec<(&'static str, f64)> {
        vec![
            (
                "minecraft:generic.max_health",
                self.max_health.unwrap_or(200.0),
            ),
            (
                "minecraft:generic.attack_damage",
                self.attack_damage.unwrap_or(12.0),
            ),
            ("minecraft:generic.armor", self.armor.unwrap_or(10.0)),
            (
                "minecraft:generic.knockback_resistance",
                self.knockback_resistance.unwrap_or(0.6),
            ),
            (
                "minecraft:generic.movement_speed",
                self.movement_speed.unwrap_or(0.28),
            ),
            (
                "minecraft:generic.follow_range",
                self.follow_range.unwrap_or(40.0),
            ),
        ]
    }

    /// The boss's effective max health (drives the bossbar `max`).
    fn effective_health(&self) -> f64 {
        self.max_health.unwrap_or(200.0)
    }
}

/// Per-slot equipment item ids. Each is a plain registered item id (no NBT /
/// count / enchantments in v1 — kept minimal and groundable).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Equipment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mainhand: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helmet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chestplate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leggings: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boots: Option<String>,
}

impl Equipment {
    fn is_empty(&self) -> bool {
        self.mainhand.is_none()
            && self.helmet.is_none()
            && self.chestplate.is_none()
            && self.leggings.is_none()
            && self.boots.is_none()
    }

    /// Every set equipment item id (for grounding), in stable slot order.
    fn ids(&self) -> Vec<&str> {
        [
            self.mainhand.as_deref(),
            self.helmet.as_deref(),
            self.chestplate.as_deref(),
            self.leggings.as_deref(),
            self.boots.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// How the boss is summoned. `totem` (default) = an ALTAR: a tick scanner
/// that fires the summon when a nether star and this boss's deterministic
/// offering block are dropped together (pure vanilla, no recipe NBT — works
/// on Fabric AND Forge); `command` = a `/trigger` objective + a tick-driven
/// dispatch; `region` is reserved (not emitted in v1).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    #[default]
    Totem,
    Command,
    Region,
}

// ---------------------------------------------------------------------------
// Derived ids
// ---------------------------------------------------------------------------

/// The stable hex for the content facet on `node_id` in `chapter_id`. This is
/// the single keying point: every emitted id, the boss `Tag`, the bossbar id,
/// and the token's `anvil_token` value all derive from it, so they agree
/// byte-for-byte across validate / emit / the authored allowlist.
pub fn content_hex(chapter_id: &str, node_id: &str) -> String {
    stable_hex(&format!("{chapter_id}:{node_id}:content"))
}

/// A derived `anvil:<hex>_<suffix>` id (function / advancement).
/// Anvil-authored (the `anvil` namespace is the tier-2 allowlist root) and
/// collision-free by the content-stable hashing the quest ids use.
fn derived(hex: &str, suffix: &str) -> String {
    format!("anvil:{hex}_{suffix}")
}

/// Vanilla blocks used as the per-boss "altar offering" partner to a nether
/// star. All are valuable/distinctive enough that a player will not casually
/// drop one next to a nether star by accident, and all exist in vanilla
/// 1.20.1 on every loader. Picked deterministically by the content hex so the
/// same boss always wants the same offering (and different bosses in a pack
/// get different ones — the disambiguator, since vanilla recipe results carry
/// no NBT on Fabric 1.20.1, only Forge).
const ALTAR_BLOCKS: &[&str] = &[
    "minecraft:gold_block",
    "minecraft:diamond_block",
    "minecraft:emerald_block",
    "minecraft:netherite_block",
    "minecraft:iron_block",
    "minecraft:lapis_block",
    "minecraft:redstone_block",
    "minecraft:amethyst_block",
    "minecraft:copper_block",
    "minecraft:beacon",
    "minecraft:crying_obsidian",
    "minecraft:gilded_blackstone",
    "minecraft:honeycomb_block",
    "minecraft:bone_block",
    "minecraft:dragon_egg",
    "minecraft:sea_lantern",
];

/// The deterministic per-boss altar offering block id. Stable from the hex so
/// validate / emit / docs all agree.
fn altar_block(hex: &str) -> &'static str {
    // First 8 hex chars -> u32 -> index. Hex is ascii lowercase from
    // `stable_hex`, so this is fully deterministic.
    let n = u32::from_str_radix(&hex[..hex.len().min(8)], 16).unwrap_or(0);
    ALTAR_BLOCKS[(n as usize) % ALTAR_BLOCKS.len()]
}

/// Every derived `anvil:<hex>_*` id this engine emits for one content facet,
/// in a stable order. Mirrors `quest::authored_recipe_ids`: the
/// Anvil-authored-allowlist seam so the token GatherItem task and every
/// internal cross-reference ground cleanly even before the datapack exists.
pub fn facet_authored_ids(chapter_id: &str, node_id: &str, spec: &ContentSpec) -> Vec<String> {
    let hex = content_hex(chapter_id, node_id);
    let ContentSpec::Boss { trigger, .. } = spec;
    let mut ids = vec![
        derived(&hex, "summon"),
        derived(&hex, "tick"),
        derived(&hex, "onkill"),
        derived(&hex, "killed"),
    ];
    match trigger {
        // Altar: a tick scanner + the function it fires. No recipe/advancement
        // (vanilla Fabric 1.20.1 recipe results carry no NBT).
        Trigger::Totem => {
            ids.push(derived(&hex, "altar"));
            ids.push(derived(&hex, "altar_fire"));
        }
        Trigger::Command => {
            ids.push(derived(&hex, "trigger"));
            ids.push(derived(&hex, "give_trigger"));
        }
        Trigger::Region => {}
    }
    ids
}

/// The Heracles task a content facet ALWAYS surfaces: a `GatherItem` on the
/// token (the real, auto-detected objective), matched by the token's
/// `anvil_token` NBT — NEVER a `Checkmark`, never a `kill_entity` on a
/// fabricated id. Synthesized at emit time, mirroring the recipe facet's
/// auto item-on-result task. `None` for a kind that has no token (none in v1).
/// The effective token item id a boss drops (the single source of the
/// `minecraft:nether_star` default). Used by the surfaced task AND by the
/// Heracles quest icon so they never disagree.
pub fn token_item_id(spec: &ContentSpec) -> String {
    let ContentSpec::Boss { token_item, .. } = spec;
    token_item
        .clone()
        .unwrap_or_else(|| "minecraft:nether_star".to_string())
}

/// The token's in-game display name (single source of the
/// `"<display_name> Token"` default).
pub fn token_display_name(spec: &ContentSpec) -> String {
    let ContentSpec::Boss {
        display_name,
        token_name,
        ..
    } = spec;
    token_name
        .clone()
        .unwrap_or_else(|| format!("{display_name} Token"))
}

/// `minecraft:gold_block` -> `Gold Block`. Deterministic; for player prose.
fn pretty_id(id: &str) -> String {
    let bare = id.rsplit(':').next().unwrap_or(id);
    bare.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().chain(ch).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The exact, deterministic in-game summon ritual, built from the SAME
/// offering-block / trigger source `to_openloader_files` emits (`altar_block`,
/// `content_hex`, the `anvil_summon_<hex>` objective), so the quest text and
/// the datapack can never disagree. Prepended to the content node's Heracles
/// description; it then flows through the shared em-dash sanitizer with the
/// rest of the description. Boss summoning is a hidden ritual the player
/// cannot otherwise discover, so unlike a craftable recipe it MUST be stated
/// in-game.
pub fn summon_instructions(chapter_id: &str, node_id: &str, spec: &ContentSpec) -> String {
    let hex = content_hex(chapter_id, node_id);
    let ContentSpec::Boss {
        display_name,
        trigger,
        ..
    } = spec;
    let tname = token_display_name(spec);
    let how = match trigger {
        Trigger::Totem => format!(
            "To summon {display_name}: drop a Nether Star and a {} within 3 \
             blocks of each other.",
            pretty_id(altar_block(&hex))
        ),
        Trigger::Command => format!(
            "To summon {display_name}: run the command \
             /trigger anvil_summon_{hex}."
        ),
        // Region is reserved (never emitted in v1); stay defensive.
        Trigger::Region => format!("Seek out and defeat {display_name}."),
    };
    format!("{how} Defeat {display_name} to claim the {tname}.")
}

pub fn surfaced_task(chapter_id: &str, node_id: &str, spec: &ContentSpec) -> crate::quest::QuestTask {
    let hex = content_hex(chapter_id, node_id);
    let item = token_item_id(spec);
    crate::quest::QuestTask::GatherItem {
        item,
        // Partial-compound predicate: matches the give-stack's superset
        // `{display:{...},anvil_token:"<hex>"}` (1.20.1 NBT, Heracles
        // GatherItemTask.nbt = NbtPredicate partial match).
        nbt: Some(format!("{{anvil_token:\"{hex}\"}}")),
        count: 1,
    }
}

// ---------------------------------------------------------------------------
// SNBT helpers
// ---------------------------------------------------------------------------

/// A minimal text-component **SNBT string literal**, e.g.
/// `{"text":"Eternax"}` -> the single-quoted SNBT form
/// `'{"text":"Eternax"}'`. The inner JSON uses double quotes; escaping `name`
/// keeps a quote in the boss name from breaking the compound. Use ONLY inside
/// an SNBT compound (item `display:{Name:..}`, summon `CustomName:..`) where a
/// single-quoted string is the correct SNBT value form.
fn text_component(name: &str) -> String {
    format!("'{}'", text_component_arg(name))
}

/// The SAME minimal text component as **bare JSON** (no surrounding single
/// quotes): `{"text":"Eternax"}`. Use in a command's `ComponentArgument`
/// position (`bossbar add <id> <component>`). 1.20.1's `ComponentArgument`
/// reads the rest of the line with a GSON-lenient JSON reader: a leading `'`
/// is consumed as a single quoted-string token, so `'{"text":"X"}'` decodes
/// to a literal-text component whose displayed text is the RAW string
/// `{"text":"X"}` — the boss name would never show. Bare JSON parses to the
/// real component (matches the wiki's own `bossbar add` example).
fn text_component_arg(name: &str) -> String {
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    format!("{{\"text\":\"{escaped}\"}}")
}

/// Format an f64 for SNBT as a `d`-suffixed double (e.g. `200.0d`), trimming a
/// trailing `.0` is intentionally NOT done — Minecraft accepts `200.0d`.
fn snbt_double(v: f64) -> String {
    format!("{v}d")
}

// ---------------------------------------------------------------------------
// to_openloader_files
// ---------------------------------------------------------------------------

/// Deterministic content-datapack files for a graph. Returns (relative path,
/// contents) pairs under `config/openloader/data/anvil-content/`. ALWAYS emits
/// the mandatory `pack.mcmeta` first (Open Loader silently drops a folder
/// lacking it), then, for each content facet, ALL of: the summon fn, the tick
/// fn, the onkill fn, the kill-advancement, the trigger (recipe+advancement for
/// `totem`; objective bootstrap + dispatch for `command`), and the merged
/// `minecraft:tick` / `minecraft:load` function tags.
///
/// EMPTY when the graph has no content facets, so a pure quest/recipe pack
/// never gets a stray `anvil-content` datapack.
///
/// Determinism: serde_json (no `preserve_order`) sorts object keys; the .mcfunction
/// bodies are built from stable-ordered iteration and content-stable hex; every
/// file ends with a trailing newline. Two runs on the same graph are
/// byte-identical (the property the determinism test relies on).
pub fn to_openloader_files(
    g: &crate::quest::QuestGraph,
    _mc_version: &str,
) -> Vec<(String, String)> {
    use serde_json::json;

    // Collect (chapter, node, spec) in graph order so every derived id and
    // tag-list entry is stable.
    let facets: Vec<(&str, &str, &ContentSpec)> = g
        .chapters
        .iter()
        .flat_map(|ch| {
            ch.quests.iter().filter_map(move |q| {
                q.content
                    .as_ref()
                    .map(|c| (ch.id.as_str(), q.id.as_str(), c))
            })
        })
        .collect();

    if facets.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<(String, String)> = Vec::new();

    // pack.mcmeta — MANDATORY. Its own root (sibling of anvil-recipes).
    let mcmeta = json!({
        "pack": {
            "pack_format": PACK_FORMAT_1_20,
            "description": "Anvil provisioned content",
        }
    });
    let mut mcmeta_s =
        serde_json::to_string_pretty(&mcmeta).unwrap_or_else(|_| "{}".to_string());
    mcmeta_s.push('\n');
    out.push((format!("{ROOT}/pack.mcmeta"), mcmeta_s));

    // Tick/load function-tag lists (one tag file aggregating every facet).
    let mut tick_fns: Vec<String> = Vec::new();
    let mut load_fns: Vec<String> = Vec::new();

    for (ch_id, node_id, spec) in &facets {
        let hex = content_hex(ch_id, node_id);
        let ContentSpec::Boss {
            entity,
            display_name,
            attributes,
            equipment,
            bossbar_color,
            token_item,
            trigger,
            token_name,
        } = spec;

        let tag = format!("anvil_boss_{hex}");
        let bossbar = format!("anvil:{hex}");
        let color = bossbar_color.as_deref().unwrap_or("red");
        let token = token_item
            .clone()
            .unwrap_or_else(|| "minecraft:nether_star".to_string());
        let tname = token_name
            .clone()
            .unwrap_or_else(|| format!("{display_name} Token"));
        let max_hp = attributes.effective_health();

        // ---- summon fn: data/anvil/functions/<hex>_summon.mcfunction ----
        //
        // Summons the base entity at the executor with CustomName, the unique
        // scoreboard Tag, PersistenceRequired, the buffed Attributes, optional
        // equipment, then creates + arms the bossbar.
        let attrs: Vec<String> = attributes
            .effective()
            .into_iter()
            .map(|(id, base)| {
                format!("{{Name:\"{id}\",Base:{}}}", snbt_double(base))
            })
            .collect();
        let mut summon_nbt = format!(
            "{{CustomName:{cn},CustomNameVisible:1b,PersistenceRequired:1b,\
             Tags:[\"{tag}\"],Health:{hp},Attributes:[{attrs}]",
            cn = text_component(display_name),
            hp = format!("{}f", max_hp),
            attrs = attrs.join(",")
        );
        // HandItems = [mainhand, offhand]; ArmorItems = [feet, legs, chest,
        // head] (vanilla slot order).
        if !equipment.is_empty() {
            let it = |id: &Option<String>| match id {
                Some(i) => format!("{{id:\"{i}\",Count:1b}}"),
                None => "{}".to_string(),
            };
            summon_nbt.push_str(&format!(
                ",HandItems:[{mh},{{}}],ArmorItems:[{ft},{lg},{ct},{hd}]",
                mh = it(&equipment.mainhand),
                ft = it(&equipment.boots),
                lg = it(&equipment.leggings),
                ct = it(&equipment.chestplate),
                hd = it(&equipment.helmet),
            ));
        }
        summon_nbt.push('}');

        let summon_body = format!(
            "# Anvil boss summon: {display_name}\n\
             summon {entity} ~ ~ ~ {summon_nbt}\n\
             bossbar add {bossbar} {title}\n\
             bossbar set {bossbar} color {color}\n\
             bossbar set {bossbar} max {max}\n\
             bossbar set {bossbar} value {max}\n\
             bossbar set {bossbar} players @a\n",
            // `bossbar add <id> <component>` is a ComponentArgument position:
            // bare JSON, NOT the single-quoted SNBT form (1.20.1 lenient
            // reader would render the literal JSON otherwise).
            title = text_component_arg(display_name),
            max = max_hp.round() as i64,
        );
        out.push((
            format!("{ROOT}/data/anvil/functions/{hex}_summon.mcfunction"),
            summon_body,
        ));

        // ---- tick fn: drive the bossbar from the tagged entity's Health,
        // remove the bossbar when the entity is gone. Registered in
        // minecraft:tick.
        let tick_body = format!(
            "# Anvil boss bossbar driver: {display_name}\n\
             execute store result bossbar {bossbar} value run \
             data get entity @e[tag={tag},limit=1] Health 1\n\
             execute unless entity @e[tag={tag},limit=1] run \
             bossbar remove {bossbar}\n"
        );
        out.push((
            format!("{ROOT}/data/anvil/functions/{hex}_tick.mcfunction"),
            tick_body,
        ));
        tick_fns.push(derived(&hex, "tick"));

        // ---- onkill fn: give the unique NBT token, set the gate stage,
        // remove the bossbar. Run by the kill-advancement's rewards.function.
        let stage_n = stage_index(g, ch_id, node_id);
        // `scoreboard objectives add` is idempotent (vanilla treats a re-add
        // of an existing objective as a no-op), so creating `anvil_stage` here
        // guarantees the stage-set lands even for a totem-only pack (the
        // command trigger also creates it at load; without this line a
        // totem-only pack's stage-set would silently no-op). The token `give`
        // is the actual quest-completion path; the stage is the downstream
        // gate hook.
        let onkill_body = format!(
            "# Anvil boss reward: {display_name}\n\
             scoreboard objectives add anvil_stage dummy\n\
             give @s {token}{{display:{{Name:{tn}}},anvil_token:\"{hex}\"}} 1\n\
             scoreboard players set @s anvil_stage {stage_n}\n\
             bossbar remove {bossbar}\n",
            tn = text_component(&tname),
        );
        out.push((
            format!("{ROOT}/data/anvil/functions/{hex}_onkill.mcfunction"),
            onkill_body,
        ));

        // ---- kill-advancement: data/anvil/advancements/<hex>_killed.json ----
        //
        // minecraft:player_killed_entity with the 1.20.1 INLINE entity
        // predicate (NOT the 1.20.2+ entity_properties condition LIST). The
        // `nbt` is an SNBT string matching the boss's unique Tag. Its
        // rewards.function grants the token.
        let killed_adv = json!({
            "criteria": {
                "kill": {
                    "trigger": "minecraft:player_killed_entity",
                    "conditions": {
                        // 1.20.1 inline form. 1.20.2+ would be:
                        //   "entity": [ { "condition":
                        //     "minecraft:entity_properties",
                        //     "entity": "this", "predicate": { ... } } ]
                        "entity": {
                            "type": entity,
                            "nbt": format!("{{Tags:[\"{tag}\"]}}"),
                        }
                    }
                }
            },
            "requirements": [["kill"]],
            "rewards": { "function": derived(&hex, "onkill") }
        });
        let mut adv_s = serde_json::to_string_pretty(&killed_adv)
            .unwrap_or_else(|_| "{}".to_string());
        adv_s.push('\n');
        out.push((
            format!("{ROOT}/data/anvil/advancements/{hex}_killed.json"),
            adv_s,
        ));

        // ---- trigger ----
        match trigger {
            Trigger::Totem => {
                // ALTAR summon. Vanilla Fabric 1.20.1 crafting-recipe results
                // carry NO `nbt` (only Forge patches that in), so the old
                // nbt-tagged craftable was a silent no-op on Fabric — the
                // boss could never be summoned. Instead: drop a nether star
                // next to this boss's deterministic offering block; a per-
                // boss tick scanner fires the summon at that spot and consumes
                // both items. The per-boss offering block is the multi-boss
                // disambiguator (recipes can't be, with no result NBT). Pure
                // vanilla, identical on Fabric and Forge.
                let offer = altar_block(&hex);
                let altar = format!(
                    "# Anvil summon altar: {display_name}\n\
                     # Drop a minecraft:nether_star and a {offer} within 3\n\
                     # blocks of each other to summon this boss.\n\
                     execute as @e[type=item,nbt={{Item:{{id:\"minecraft:nether_star\"}}}},limit=1] \
                     at @s if entity @e[type=item,nbt={{Item:{{id:\"{offer}\"}}}},distance=..3,limit=1] \
                     run function anvil:{hex}_altar_fire\n"
                );
                out.push((
                    format!("{ROOT}/data/anvil/functions/{hex}_altar.mcfunction"),
                    altar,
                ));
                tick_fns.push(derived(&hex, "altar"));

                // Fired AT the nether star's position: summon there, then
                // consume one of each offering item. `_summon` uses ~ ~ ~
                // (the altar spot) and @a/@e[tag] only, so it is correct even
                // after the executor item is removed.
                let fire = format!(
                    "# Anvil altar fired: {display_name}\n\
                     function anvil:{hex}_summon\n\
                     kill @e[type=item,nbt={{Item:{{id:\"minecraft:nether_star\"}}}},distance=..3,limit=1]\n\
                     kill @e[type=item,nbt={{Item:{{id:\"{offer}\"}}}},distance=..3,limit=1]\n"
                );
                out.push((
                    format!("{ROOT}/data/anvil/functions/{hex}_altar_fire.mcfunction"),
                    fire,
                ));
            }
            Trigger::Command => {
                // /trigger anvil_summon_<hex>: a load-registered objective +
                // a tick-driven dispatch that runs the summon for any player
                // whose trigger fired, then resets it.
                let obj = format!("anvil_summon_{hex}");
                load_fns.push(derived(&hex, "give_trigger"));
                let give_trigger = format!(
                    "# Anvil summon trigger objective ({display_name})\n\
                     scoreboard objectives add {obj} trigger\n\
                     scoreboard objectives add anvil_stage dummy\n"
                );
                out.push((
                    format!(
                        "{ROOT}/data/anvil/functions/{hex}_give_trigger.mcfunction"
                    ),
                    give_trigger,
                ));
                let dispatch = format!(
                    "# Anvil summon dispatch ({display_name})\n\
                     execute as @a[scores={{{obj}=1..}}] at @s run \
                     function anvil:{hex}_summon\n\
                     scoreboard players set @a[scores={{{obj}=1..}}] {obj} 0\n\
                     scoreboard players enable @a {obj}\n"
                );
                out.push((
                    format!("{ROOT}/data/anvil/functions/{hex}_trigger.mcfunction"),
                    dispatch,
                ));
                tick_fns.push(derived(&hex, "trigger"));
            }
            Trigger::Region => {
                // Reserved; not emitted in v1 (validator rejects it so this
                // arm is unreachable for a written graph).
            }
        }
    }

    // Merged minecraft:tick / minecraft:load function tags. These COEXIST with
    // any other datapack's tags (separate namespace path, Minecraft merges tag
    // files across datapacks). `replace:false` so we never clobber.
    if !tick_fns.is_empty() {
        let v = json!({ "replace": false, "values": tick_fns });
        let mut s =
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string());
        s.push('\n');
        out.push((
            format!("{ROOT}/data/minecraft/tags/functions/tick.json"),
            s,
        ));
    }
    if !load_fns.is_empty() {
        let v = json!({ "replace": false, "values": load_fns });
        let mut s =
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string());
        s.push('\n');
        out.push((
            format!("{ROOT}/data/minecraft/tags/functions/load.json"),
            s,
        ));
    }

    out
}

/// The gate stage number a content node's onkill sets. Deterministic: the
/// 1-based index of this content node among ALL content nodes in graph order,
/// so each boss advances a monotonically-rising `anvil_stage`.
fn stage_index(g: &crate::quest::QuestGraph, ch_id: &str, node_id: &str) -> i64 {
    let mut n = 0i64;
    for ch in &g.chapters {
        for q in &ch.quests {
            if q.content.is_some() {
                n += 1;
                if ch.id == ch_id && q.id == node_id {
                    return n;
                }
            }
        }
    }
    n.max(1)
}

// ---------------------------------------------------------------------------
// validate_content
// ---------------------------------------------------------------------------

/// A content-facet defect, surfaced through the quest issue channel (mapped by
/// `crate::quest`). All hard (write-blocking) — content provisioning has no
/// "quality, final-only" tier; a partial/ungrounded boss is never acceptable
/// (design §6 #12 token atomicity).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentIssue {
    /// A `content.entity`/`equipment`/`token_item` id that is fabricated: its
    /// namespace IS pinned-and-scanned but the concrete id is absent from the
    /// pack's real registry (the `cobblemon:mewtwo` class), or its namespace
    /// is not pinned at all. Hard, every call.
    UnknownItem { node: String, id: String },
    /// An id accepted but UNVERIFIED (jar absent / namespace-only fallback).
    /// NOT write-blocking; surfaced like the quest engine's `LowConfidenceId`.
    LowConfidenceId {
        node: String,
        id: String,
        reason: String,
    },
    /// TOKEN ATOMICITY (design §6 #12): the content facet cannot deterministically
    /// emit the FULL atomic set {summon, tick, onkill+token, kill-advancement,
    /// trigger, GatherItem-on-token}. Caused by an empty/blank required field
    /// or a reserved (`region`) trigger. Hard, every call, write-blocked.
    ContentIncomplete { node: String, detail: String },
}

/// Validate every content facet in the graph against the Slice-1 index +
/// enforce TOKEN ATOMICITY. Empty = ok.
///
/// Issue order is stable: chapters -> quests -> (atomicity first, then
/// grounding: entity, then equipment in slot order, then token_item).
///
/// ATOMICITY: the emitter is deterministic and emits ALL of the atomic set for
/// any VALID spec, so a "partial" content node can only arise from an invalid
/// spec — an empty `entity`/`display_name`, or the reserved `region` trigger.
/// Those become a hard `ContentIncomplete` (write blocked, every call). There
/// is no path that emits a token task without its kill-advancement+onkill: a
/// valid spec emits the whole set or `validate_content` blocks the write.
pub fn validate_content(
    g: &crate::quest::QuestGraph,
    idx: &AllowedIndex,
) -> Vec<ContentIssue> {
    let mut issues = Vec::new();

    for ch in &g.chapters {
        for q in &ch.quests {
            let Some(spec) = q.content.as_ref() else {
                continue;
            };
            let ContentSpec::Boss {
                entity,
                display_name,
                equipment,
                token_item,
                trigger,
                ..
            } = spec;

            // --- atomicity: a blank required field or reserved trigger means
            // the full atomic set cannot be emitted -> hard incomplete.
            if entity.trim().is_empty() {
                issues.push(ContentIssue::ContentIncomplete {
                    node: q.id.clone(),
                    detail: "boss entity is empty".to_string(),
                });
            }
            if display_name.trim().is_empty() {
                issues.push(ContentIssue::ContentIncomplete {
                    node: q.id.clone(),
                    detail: "boss display_name is empty".to_string(),
                });
            }
            if *trigger == Trigger::Region {
                issues.push(ContentIssue::ContentIncomplete {
                    node: q.id.clone(),
                    detail: "trigger 'region' is not supported in v1 (use \
                             totem or command)"
                        .to_string(),
                });
            }

            // --- grounding: entity, equipment (slot order), token_item.
            ground(idx, &q.id, entity, GroundKind::Entity, &mut issues);
            for eq in equipment.ids() {
                ground(idx, &q.id, eq, GroundKind::Item, &mut issues);
            }
            if let Some(ti) = token_item {
                ground(idx, &q.id, ti, GroundKind::Item, &mut issues);
            }
        }
    }

    issues
}

enum GroundKind {
    Entity,
    Item,
}

/// Classify one content ref through the SAME Slice-1 grounding ladder the
/// quest tasks / recipes use, pushing the matching `ContentIssue`.
fn ground(
    idx: &AllowedIndex,
    node: &str,
    id: &str,
    kind: GroundKind,
    issues: &mut Vec<ContentIssue>,
) {
    let is_tag = id.starts_with('#');
    let bare = id.strip_prefix('#').unwrap_or(id);
    let g = match kind {
        GroundKind::Entity => idx.ground_content_entity(bare, is_tag),
        GroundKind::Item => idx.ground_recipe_id(bare, is_tag),
    };
    match g {
        RecipeGrounding::Ok => {}
        RecipeGrounding::LowConfidence(reason) => {
            issues.push(ContentIssue::LowConfidenceId {
                node: node.to_string(),
                id: id.to_string(),
                reason: reason.to_string(),
            });
        }
        RecipeGrounding::Unknown => {
            issues.push(ContentIssue::UnknownItem {
                node: node.to_string(),
                id: id.to_string(),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quest::{QuestChapter, QuestGraph, QuestNode};

    fn boss_node(id: &str) -> QuestNode {
        let mut q = QuestNode {
            id: id.to_string(),
            title: format!("Quest {id}"),
            description: String::new(),
            x: 0.0,
            y: 0.0,
            deps: Vec::new(),
            tasks: Vec::new(),
            rewards: Vec::new(),
            recipes: Vec::new(),
            content: None,
        };
        q.content = Some(ContentSpec::Boss {
            entity: "minecraft:wither_skeleton".to_string(),
            display_name: "Eternax, the Void Sovereign".to_string(),
            attributes: BossAttributes::default(),
            equipment: Equipment {
                mainhand: Some("minecraft:netherite_sword".to_string()),
                ..Default::default()
            },
            bossbar_color: Some("purple".to_string()),
            token_item: Some("minecraft:nether_star".to_string()),
            trigger: Trigger::Totem,
            token_name: Some("Void Heart".to_string()),
        });
        q
    }

    fn graph_with(q: QuestNode) -> QuestGraph {
        QuestGraph {
            title: "T".to_string(),
            chapters: vec![QuestChapter {
                id: "ch8".to_string(),
                title: "Climax".to_string(),
                quests: vec![q],
            }],
        }
    }

    #[test]
    fn summon_instructions_match_the_datapack_offering_and_trigger() {
        // Totem: the prose names the SAME deterministic offering block the
        // altar scanner keys on (single source: altar_block(content_hex)).
        let q = boss_node("climax");
        let spec = q.content.as_ref().unwrap();
        let hex = content_hex("ch8", "climax");
        let si = summon_instructions("ch8", "climax", spec);
        assert!(
            si.contains(&pretty_id(altar_block(&hex))),
            "summon prose must name the offering block: {si}"
        );
        assert!(si.contains("Nether Star"));
        assert!(si.contains("within 3 blocks"));
        assert!(si.contains(
            "Defeat Eternax, the Void Sovereign to claim the Void Heart."
        ));
        assert!(!si.contains('\u{2014}'), "no em dash in synthesized text");

        // Cross-artifact: the block in the prose is exactly the one
        // to_openloader_files writes into the altar scanner function.
        let files = to_openloader_files(&graph_with(boss_node("climax")), "1.20.1");
        let altar = files
            .iter()
            .find(|(p, _)| {
                p == &format!("{ROOT}/data/anvil/functions/{hex}_altar.mcfunction")
            })
            .expect("altar scanner emitted");
        assert!(
            altar.1.contains(altar_block(&hex)),
            "datapack altar must use the same offering block as the prose"
        );

        // Command trigger: the prose names the exact /trigger objective.
        let mut q2 = boss_node("c2");
        if let Some(ContentSpec::Boss { trigger, .. }) = q2.content.as_mut() {
            *trigger = Trigger::Command;
        }
        let spec2 = q2.content.as_ref().unwrap();
        let hex2 = content_hex("ch8", "c2");
        let si2 = summon_instructions("ch8", "c2", spec2);
        assert!(
            si2.contains(&format!("/trigger anvil_summon_{hex2}")),
            "command prose must name the trigger objective: {si2}"
        );
    }

    #[test]
    fn boss_emits_full_atomic_datapack() {
        let g = graph_with(boss_node("climax"));
        let files = to_openloader_files(&g, "1.20.1");
        let hex = content_hex("ch8", "climax");

        let has = |p: &str| files.iter().any(|(f, _)| f == p);
        assert!(has(&format!("{ROOT}/pack.mcmeta")));
        assert!(has(&format!(
            "{ROOT}/data/anvil/functions/{hex}_summon.mcfunction"
        )));
        assert!(has(&format!(
            "{ROOT}/data/anvil/functions/{hex}_tick.mcfunction"
        )));
        assert!(has(&format!(
            "{ROOT}/data/anvil/functions/{hex}_onkill.mcfunction"
        )));
        assert!(has(&format!(
            "{ROOT}/data/anvil/advancements/{hex}_killed.json"
        )));
        // Altar trigger (no recipe — vanilla Fabric 1.20.1 result has no nbt):
        // a per-boss tick scanner + the function it fires.
        assert!(has(&format!(
            "{ROOT}/data/anvil/functions/{hex}_altar.mcfunction"
        )));
        assert!(has(&format!(
            "{ROOT}/data/anvil/functions/{hex}_altar_fire.mcfunction"
        )));
        assert!(
            !has(&format!("{ROOT}/data/anvil/recipes/{hex}_totem.json")),
            "the broken nbt-recipe totem must no longer be emitted"
        );
        // The altar scanner must be wired into minecraft:tick.
        let (_, tick_tag) = files
            .iter()
            .find(|(p, _)| {
                p == &format!("{ROOT}/data/minecraft/tags/functions/tick.json")
            })
            .expect("tick tag emitted");
        assert!(
            tick_tag.contains(&format!("anvil:{hex}_altar")),
            "altar scanner registered in minecraft:tick"
        );

        // pack_format 15 (1.20.1), trailing newline.
        let (_, mcmeta) = files
            .iter()
            .find(|(p, _)| p == &format!("{ROOT}/pack.mcmeta"))
            .unwrap();
        let mv: serde_json::Value = serde_json::from_str(mcmeta).unwrap();
        assert_eq!(mv["pack"]["pack_format"], 15);
        assert!(mcmeta.ends_with('\n'));

        // The kill-advancement: 1.20.1 INLINE entity predicate (object, NOT a
        // list), nbt matches the boss Tag, rewards.function -> onkill.
        let (_, adv) = files
            .iter()
            .find(|(p, _)| {
                p == &format!("{ROOT}/data/anvil/advancements/{hex}_killed.json")
            })
            .unwrap();
        let av: serde_json::Value = serde_json::from_str(adv).unwrap();
        let ent = &av["criteria"]["kill"]["conditions"]["entity"];
        assert!(ent.is_object(), "1.20.1 inline entity predicate is an object");
        assert!(!ent.is_array(), "must NOT be the 1.20.2 condition list");
        assert_eq!(ent["type"], "minecraft:wither_skeleton");
        assert_eq!(
            ent["nbt"],
            format!("{{Tags:[\"anvil_boss_{hex}\"]}}")
        );
        assert_eq!(
            av["rewards"]["function"],
            format!("anvil:{hex}_onkill")
        );

        // onkill gives the token with the anvil_token NBT + sets the stage.
        let (_, onkill) = files
            .iter()
            .find(|(p, _)| {
                p == &format!(
                    "{ROOT}/data/anvil/functions/{hex}_onkill.mcfunction"
                )
            })
            .unwrap();
        assert!(onkill.contains(&format!("anvil_token:\"{hex}\"")));
        assert!(onkill.contains("minecraft:nether_star"));
        assert!(onkill.contains("scoreboard players set @s anvil_stage 1"));

        // Determinism: byte-identical across runs.
        assert_eq!(to_openloader_files(&g, "1.20.1"), files);
    }

    #[test]
    fn no_content_emits_no_files() {
        let q = QuestNode {
            id: "plain".to_string(),
            title: "Plain".to_string(),
            description: String::new(),
            x: 0.0,
            y: 0.0,
            deps: Vec::new(),
            tasks: Vec::new(),
            rewards: Vec::new(),
            recipes: Vec::new(),
            content: None,
        };
        assert!(to_openloader_files(&graph_with(q), "1.20.1").is_empty());
    }

    #[test]
    fn surfaced_task_is_gather_item_on_token_nbt() {
        let g = graph_with(boss_node("climax"));
        let spec = g.chapters[0].quests[0].content.as_ref().unwrap();
        let t = surfaced_task("ch8", "climax", spec);
        let hex = content_hex("ch8", "climax");
        match t {
            crate::quest::QuestTask::GatherItem { item, nbt, count } => {
                assert_eq!(item, "minecraft:nether_star");
                assert_eq!(nbt, Some(format!("{{anvil_token:\"{hex}\"}}")));
                assert_eq!(count, 1);
            }
            other => panic!("expected GatherItem, got {other:?}"),
        }
    }

    #[test]
    fn command_trigger_emits_objective_and_dispatch() {
        let mut q = boss_node("cmd");
        if let Some(ContentSpec::Boss { trigger, .. }) = q.content.as_mut() {
            *trigger = Trigger::Command;
        }
        let g = graph_with(q);
        let files = to_openloader_files(&g, "1.20.1");
        let hex = content_hex("ch8", "cmd");
        assert!(files.iter().any(|(p, _)| p
            == &format!(
                "{ROOT}/data/anvil/functions/{hex}_give_trigger.mcfunction"
            )));
        assert!(files.iter().any(|(p, _)| p
            == &format!("{ROOT}/data/minecraft/tags/functions/load.json")));
        // No totem recipe for the command trigger.
        assert!(!files.iter().any(|(p, _)| p
            == &format!("{ROOT}/data/anvil/recipes/{hex}_totem.json")));
    }

    // -----------------------------------------------------------------------
    // REAL end-to-end loadability tests (MC 1.20.1): parse every emitted file
    // the way the game would and assert game-loadability, then cross-check the
    // forgery-proof token linkage and the atomic-set closure.
    // -----------------------------------------------------------------------

    /// Every command word `to_openloader_files` could legitimately emit in a
    /// 1.20.1 datapack. A first token outside this set in a non-comment line is
    /// an obviously-malformed command (the bar the user asked for).
    const KNOWN_COMMANDS: &[&str] = &[
        "summon", "execute", "give", "kill", "data", "bossbar", "tag",
        "tellraw", "playsound", "particle", "function", "scoreboard", "title",
        "advancement", "effect", "gamerule", "weather", "time", "setblock",
        "fill", "teleport", "tp", "say", "schedule", "clear", "clone",
        "damage", "ride", "attribute", "item", "loot", "spreadplayers",
    ];

    /// True iff every `{}`/`[]` pair in `s` is balanced AND nested correctly
    /// (ignoring brace/bracket chars that sit inside a `"`- or `'`-quoted
    /// run, since SNBT/JSON string values legitimately contain them). This is
    /// the "balanced braces in any NBT/JSON arg" check.
    fn brackets_balanced(s: &str) -> bool {
        let mut stack: Vec<char> = Vec::new();
        let mut in_str: Option<char> = None;
        let mut esc = false;
        for c in s.chars() {
            if let Some(q) = in_str {
                if esc {
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == q {
                    in_str = None;
                }
                continue;
            }
            match c {
                '"' | '\'' => in_str = Some(c),
                '{' | '[' => stack.push(c),
                '}' => {
                    if stack.pop() != Some('{') {
                        return false;
                    }
                }
                ']' => {
                    if stack.pop() != Some('[') {
                        return false;
                    }
                }
                _ => {}
            }
        }
        in_str.is_none() && stack.is_empty()
    }

    /// Split a command line into top-level whitespace tokens, treating any
    /// `{...}`/`[...]`/quoted run as a single opaque token so an NBT/JSON arg
    /// with internal spaces is not mis-split. Used to find `run <subcommand>`
    /// tails for the no-malformed-`run`-chain check.
    fn first_word(line: &str) -> &str {
        line.split_whitespace().next().unwrap_or("")
    }

    /// Assert one emitted `.mcfunction` body is game-loadable for 1.20.1:
    /// every non-blank, non-`#` line is a single well-formed command — known
    /// first word, balanced braces/brackets, and every `run <cmd>` tail itself
    /// starts with a known command word (no broken `execute ... run` chain).
    /// Component-argument positions (`bossbar add <id> <component>`) must be
    /// bare JSON, never an SNBT single-quoted string (1.20.1 `ComponentArgument`
    /// reads GSON-lenient: a leading `'` is parsed as a literal-text string,
    /// NOT the formatted component — a real game defect).
    fn assert_mcfunction_loadable(path: &str, body: &str) {
        assert!(
            body.ends_with('\n'),
            "{path}: every emitted file ends with a trailing newline"
        );
        for raw in body.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let w = first_word(line);
            assert!(
                KNOWN_COMMANDS.contains(&w),
                "{path}: unknown/malformed command word {w:?} in line: {line}"
            );
            assert!(
                brackets_balanced(line),
                "{path}: unbalanced braces/brackets in line: {line}"
            );
            // No broken `run` chain: the token after a top-level ` run ` must
            // itself be a known command word.
            if let Some(idx) = line.find(" run ") {
                let tail = line[idx + 5..].trim_start();
                let rw = first_word(tail);
                assert!(
                    KNOWN_COMMANDS.contains(&rw),
                    "{path}: `run` chains into non-command {rw:?}: {line}"
                );
            }
            // 1.20.1 ComponentArgument: `bossbar add <id> <component>` — the
            // component arg must be bare JSON ({ or [), NEVER a single-quoted
            // SNBT string (that would render the literal raw JSON in-game).
            if let Some(rest) = line.strip_prefix("bossbar add ") {
                let comp = rest.splitn(2, ' ').nth(1).unwrap_or("").trim_start();
                let c0 = comp.chars().next().unwrap_or(' ');
                assert!(
                    c0 == '{' || c0 == '[',
                    "{path}: bossbar-add component arg must be bare JSON for \
                     1.20.1 (got {comp:?}); a single-quoted SNBT string \
                     renders the literal JSON, not the boss name"
                );
                // And it must parse as a JSON text component.
                let v: serde_json::Value = serde_json::from_str(comp)
                    .unwrap_or_else(|e| panic!(
                        "{path}: bossbar component not valid JSON ({e}): {comp}"
                    ));
                assert!(
                    v.is_object() || v.is_array(),
                    "{path}: bossbar component must be an object/array: {comp}"
                );
            }
        }
    }

    /// Extract the `anvil_token:"<hex>"` value from any SNBT/text body, or
    /// `None`. The single keying point the forgery-proof linkage rests on.
    fn extract_anvil_token(s: &str) -> Option<String> {
        let i = s.find("anvil_token:\"")? + "anvil_token:\"".len();
        let rest = &s[i..];
        let j = rest.find('"')?;
        Some(rest[..j].to_string())
    }

    /// Find an emitted file by exact relative path.
    fn file<'a>(
        files: &'a [(String, String)],
        path: &str,
    ) -> &'a str {
        files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, c)| c.as_str())
            .unwrap_or_else(|| panic!("expected emitted file: {path}"))
    }

    #[test]
    fn every_emitted_mcfunction_is_a_valid_120_1_command_stream() {
        // Cover BOTH triggers so every function-emitting code path is parsed.
        for trig in [Trigger::Totem, Trigger::Command] {
            let mut q = boss_node("climax");
            if let Some(ContentSpec::Boss { trigger, .. }) = q.content.as_mut() {
                *trigger = trig.clone();
            }
            let g = graph_with(q);
            let files = to_openloader_files(&g, "1.20.1");
            let hex = content_hex("ch8", "climax");

            let mut saw_summon = false;
            for (p, body) in &files {
                if p.ends_with(".mcfunction") {
                    assert_mcfunction_loadable(p, body);
                }
                if p == &format!(
                    "{ROOT}/data/anvil/functions/{hex}_summon.mcfunction"
                ) {
                    saw_summon = true;
                    // The summon fn actually `summon`s the spec's base entity.
                    assert!(
                        body.lines().any(|l| {
                            let l = l.trim();
                            l.starts_with("summon minecraft:wither_skeleton ")
                        }),
                        "summon fn must summon the spec base entity: {body}"
                    );
                }
            }
            assert!(saw_summon, "summon fn must be emitted ({trig:?})");

            // The reward fn `give`s the token with the anvil_token NBT.
            let onkill = file(
                &files,
                &format!(
                    "{ROOT}/data/anvil/functions/{hex}_onkill.mcfunction"
                ),
            );
            assert!(
                onkill.lines().any(|l| {
                    let l = l.trim();
                    l.starts_with("give @s ")
                        && l.contains(&format!("anvil_token:\"{hex}\""))
                }),
                "reward fn must `give` the forgery-proof token: {onkill}"
            );
        }
    }

    #[test]
    fn kill_advancement_has_vanilla_120_1_required_shape() {
        let g = graph_with(boss_node("climax"));
        let files = to_openloader_files(&g, "1.20.1");
        let hex = content_hex("ch8", "climax");

        let adv_path =
            format!("{ROOT}/data/anvil/advancements/{hex}_killed.json");
        let adv_s = file(&files, &adv_path);
        // Parses as JSON.
        let av: serde_json::Value = serde_json::from_str(adv_s)
            .expect("kill-advancement must be valid JSON");

        // criteria object with >=1 trigger whose `trigger` is a real id.
        let crit = av["criteria"].as_object().expect("criteria object");
        assert!(!crit.is_empty(), "criteria has >=1 trigger");
        let kill = &av["criteria"]["kill"];
        assert_eq!(
            kill["trigger"], "minecraft:player_killed_entity",
            "trigger must be the real vanilla id"
        );
        // 1.20.1 INLINE entity predicate: an OBJECT (NOT the 1.20.2+ list).
        let ent = &kill["conditions"]["entity"];
        assert!(ent.is_object(), "1.20.1 inline entity predicate is an object");
        assert!(!ent.is_array(), "must NOT be the 1.20.2+ condition list");
        assert!(ent["type"].is_string(), "entity.type present");
        assert!(ent["nbt"].is_string(), "entity.nbt present (Tag match)");

        // rewards.function points at the reward function we ALSO emitted.
        let rf = av["rewards"]["function"]
            .as_str()
            .expect("rewards.function present");
        assert_eq!(rf, format!("anvil:{hex}_onkill"));
        assert!(
            files.iter().any(|(p, _)| p
                == &format!(
                    "{ROOT}/data/anvil/functions/{hex}_onkill.mcfunction"
                )),
            "rewards.function must reference an emitted function file"
        );
    }

    #[test]
    fn other_json_files_parse_and_have_vanilla_120_1_fields() {
        let g = graph_with(boss_node("climax"));
        let files = to_openloader_files(&g, "1.20.1");

        // pack.mcmeta: parses, pack_format 15 (1.20.1).
        let mcmeta = file(&files, &format!("{ROOT}/pack.mcmeta"));
        let mv: serde_json::Value =
            serde_json::from_str(mcmeta).expect("pack.mcmeta valid JSON");
        assert_eq!(mv["pack"]["pack_format"], 15);
        assert!(mv["pack"]["description"].is_string());

        // function tags: parse, `replace:false`, `values` array of fn ids.
        let tick_tag = file(
            &files,
            &format!("{ROOT}/data/minecraft/tags/functions/tick.json"),
        );
        let tv: serde_json::Value =
            serde_json::from_str(tick_tag).expect("tick tag valid JSON");
        assert_eq!(tv["replace"], false);
        assert!(
            tv["values"].as_array().is_some_and(|a| !a.is_empty()),
            "tick tag has a non-empty values array"
        );
        // Every value is an emitted-or-anvil function id.
        for v in tv["values"].as_array().unwrap() {
            let id = v.as_str().expect("tag value is a string id");
            assert!(
                id.starts_with("anvil:"),
                "tag value must be an anvil-authored fn id: {id}"
            );
        }
    }

    #[test]
    fn token_atomicity_full_set_or_none_and_hex_linkage_is_forgery_proof() {
        // VALID content node: the FULL atomic set must emit (totem trigger).
        let g = graph_with(boss_node("climax"));
        let files = to_openloader_files(&g, "1.20.1");
        let hex = content_hex("ch8", "climax");

        // The exact closure for one valid totem boss: 8 files, no more/less
        // related to this facet.
        let expected: Vec<String> = vec![
            format!("{ROOT}/pack.mcmeta"),
            format!("{ROOT}/data/anvil/functions/{hex}_summon.mcfunction"),
            format!("{ROOT}/data/anvil/functions/{hex}_tick.mcfunction"),
            format!("{ROOT}/data/anvil/functions/{hex}_onkill.mcfunction"),
            format!("{ROOT}/data/anvil/advancements/{hex}_killed.json"),
            format!("{ROOT}/data/anvil/functions/{hex}_altar.mcfunction"),
            format!("{ROOT}/data/anvil/functions/{hex}_altar_fire.mcfunction"),
            format!("{ROOT}/data/minecraft/tags/functions/tick.json"),
        ];
        let mut got: Vec<String> =
            files.iter().map(|(p, _)| p.clone()).collect();
        got.sort();
        let mut want = expected.clone();
        want.sort();
        assert_eq!(
            got, want,
            "a valid totem boss emits EXACTLY the atomic set (no partial)"
        );

        // The Heracles quest emitted for this graph carries the
        // GatherItem-on-token task (the real auto objective): the rest of the
        // atomic set on the QUEST side.
        let hj = crate::quest::to_heracles_json(&g);
        let qhex = crate::quest::stable_hex("ch8:climax");
        let qpath = format!("config/heracles/quests/{qhex}.json");
        let qbody = hj
            .iter()
            .find(|(p, _)| p == &qpath)
            .map(|(_, c)| c.as_str())
            .expect("the content node's Heracles quest must be emitted");
        let qv: serde_json::Value =
            serde_json::from_str(qbody).expect("quest JSON parses");
        let tasks = qv["tasks"].as_object().expect("tasks object");
        let token_task = tasks
            .values()
            .find(|t| t["type"] == "heracles:item" && t.get("nbt").is_some())
            .expect("the GatherItem-on-token task must be present");
        assert_eq!(token_task["item"], "minecraft:nether_star");

        // FORGERY-PROOF LINKAGE: the anvil_token hex must be byte-identical
        // across (a) the give command, (b) the quest task's expected NBT, and
        // (c) the kill-advancement's entity Tag — independently extracted.
        let onkill = file(
            &files,
            &format!("{ROOT}/data/anvil/functions/{hex}_onkill.mcfunction"),
        );
        let give_tok = extract_anvil_token(onkill)
            .expect("onkill give carries an anvil_token");
        let task_nbt = token_task["nbt"].as_str().expect("task nbt string");
        let task_tok = extract_anvil_token(task_nbt)
            .expect("token task carries an anvil_token");
        let adv = file(
            &files,
            &format!("{ROOT}/data/anvil/advancements/{hex}_killed.json"),
        );
        let av: serde_json::Value = serde_json::from_str(adv).unwrap();
        let adv_nbt = av["criteria"]["kill"]["conditions"]["entity"]["nbt"]
            .as_str()
            .expect("advancement entity nbt");
        // The advancement matches the boss Tag `anvil_boss_<hex>`.
        assert_eq!(
            adv_nbt,
            format!("{{Tags:[\"anvil_boss_{hex}\"]}}"),
            "kill-advancement Tag must derive from the same content hex"
        );
        assert_eq!(
            give_tok, task_tok,
            "give-token hex must equal the quest task's expected token hex"
        );
        assert_eq!(
            give_tok, hex,
            "the linkage hex must be the content-stable hex (forgery-proof)"
        );

        // NONE side: an INVALID spec (blank entity) must hard-fail validation
        // BEFORE any write — i.e. the atomic set is all-or-nothing, enforced
        // by `validate_content`, not a partial emit.
        let mut bad = boss_node("hollow");
        if let Some(ContentSpec::Boss { entity, .. }) = bad.content.as_mut() {
            *entity = "   ".to_string();
        }
        let bg = graph_with(bad);
        let idx = crate::quest::AllowedIndex::default();
        let issues = validate_content(&bg, &idx);
        assert!(
            issues.iter().any(|i| matches!(
                i,
                ContentIssue::ContentIncomplete { .. }
            )),
            "a blank-required-field boss must hard-fail ContentIncomplete \
             (atomic set is all-or-nothing): {issues:?}"
        );
    }

    #[test]
    fn boss_entity_is_grounded_wrong_id_is_rejected() {
        // Build a CONCRETE index where `cobblemon` is pinned-and-scanned and
        // its real registry has exactly `cobblemon:poke_ball_entity` — so a
        // fabricated `cobblemon:mewtwo` boss entity is a HARD reject, and a
        // fabricated token_item likewise. Mirrors quest.rs::concrete_idx.
        fn concrete_idx() -> crate::quest::AllowedIndex {
            let mut v = crate::registry::RegistryVocab::default();
            v.entities.insert("cobblemon:poke_ball_entity".to_string());
            v.items.insert("cobblemon:poke_ball".to_string());
            v.mod_meta.push(crate::registry::ModMeta {
                id: "cobblemon".to_string(),
                name: "Cobblemon".to_string(),
                categories: vec![],
            });
            let mut idx = crate::quest::AllowedIndex {
                vocab: v,
                has_vocab: true,
                ..Default::default()
            };
            for ns in ["minecraft", "cobblemon", "anvil"] {
                idx.items.insert(ns.to_string());
                idx.entities.insert(ns.to_string());
            }
            // minecraft is bundled/never-scanned -> vanilla degrades, never
            // hard-fails (same documented Slice-1 rule the quest tests use).
            idx.unscanned.insert("minecraft".to_string());
            idx
        }

        // Sanity: the good fixture (vanilla entity) produces NO hard issue.
        let good = graph_with(boss_node("climax"));
        let ok_issues = validate_content(&good, &concrete_idx());
        assert!(
            !ok_issues.iter().any(|i| matches!(
                i,
                ContentIssue::UnknownItem { .. }
                    | ContentIssue::ContentIncomplete { .. }
            )),
            "a fully-grounded vanilla boss must have no HARD issue: \
             {ok_issues:?}"
        );

        // WRONG boss entity: pinned-and-scanned namespace, absent id -> HARD.
        let mut q = boss_node("bad_boss");
        if let Some(ContentSpec::Boss { entity, .. }) = q.content.as_mut() {
            *entity = "cobblemon:mewtwo".to_string();
        }
        let bg = graph_with(q);
        let issues = validate_content(&bg, &concrete_idx());
        assert!(
            issues.iter().any(|i| matches!(
                i,
                ContentIssue::UnknownItem { id, .. }
                    if id == "cobblemon:mewtwo"
            )),
            "a fabricated boss entity id must hard-fail UnknownItem: \
             {issues:?}"
        );

        // WRONG token_item: same grounding ladder, same hard reject.
        let mut q2 = boss_node("bad_token");
        if let Some(ContentSpec::Boss { token_item, .. }) =
            q2.content.as_mut()
        {
            *token_item = Some("cobblemon:notreal".to_string());
        }
        let bg2 = graph_with(q2);
        let issues2 = validate_content(&bg2, &concrete_idx());
        assert!(
            issues2.iter().any(|i| matches!(
                i,
                ContentIssue::UnknownItem { id, .. }
                    if id == "cobblemon:notreal"
            )),
            "a fabricated token_item id must hard-fail UnknownItem: \
             {issues2:?}"
        );
    }

    #[test]
    fn content_datapack_emit_is_byte_deterministic() {
        // Two bosses across two chapters (stage indices differ), both triggers
        // exercised, so every stable-ordering seam is covered.
        let g = QuestGraph {
            title: "Det".to_string(),
            chapters: vec![
                QuestChapter {
                    id: "c1".to_string(),
                    title: "C1".to_string(),
                    quests: vec![boss_node("b1")],
                },
                QuestChapter {
                    id: "c2".to_string(),
                    title: "C2".to_string(),
                    quests: vec![{
                        let mut q = boss_node("b2");
                        if let Some(ContentSpec::Boss { trigger, .. }) =
                            q.content.as_mut()
                        {
                            *trigger = Trigger::Command;
                        }
                        q
                    }],
                },
            ],
        };
        let a = to_openloader_files(&g, "1.20.1");
        let b = to_openloader_files(&g, "1.20.1");
        assert_eq!(a, b, "two emit runs must be byte-identical");
        assert!(
            a.iter().any(|(p, _)| p.starts_with(ROOT)),
            "the content datapack must have participated"
        );
    }
}
