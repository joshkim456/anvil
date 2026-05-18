//! AI-authored quest + difficulty engine.
//!
//! CONTRACT: subagent must preserve every public signature + type shape so
//! `lib.rs` / `curator.rs` integration compiles. Implementation overwrites the
//! bodies.
//!
//! The model emits a structured `QuestGraph` (the editable node graph); a
//! DETERMINISTIC serializer turns it into Heracles (a.k.a. "Odyssey Quests" on
//! Modrinth — `terrarium-earth/Heracles`) quest JSON with stable 16-char
//! uppercase-hex IDs. The LLM never writes the game format directly. Graphs are
//! persisted as JSON (source of truth) at `<instance>/anvil-quests.json`; the
//! Heracles files are written to `<instance>/config/heracles/quests/<id>.json`
//! (one file per quest; the filename is the quest id, dependencies reference
//! those ids).
//!
//! HERACLES SHAPE: VERIFIED against the Heracles 1.20.x + ResourcefulLib
//! source (2026-05-17). Confirmed correct against the actual codecs:
//! `heracles:biome`/`biomes` and `heracles:structure`/`structures` (both a
//! bare resource-location string or `#tag` via RegistryValue — single, NOT a
//! list), `heracles:changed_dimension`/`to`, `heracles:stat`/`stat`+`target`,
//! `heracles:kill_entity`/`entity`(RestrictedEntityPredicate: only `type`
//! required)+`amount`, item reward `item`:{id,count} (ItemStackCodec), and
//! `heracles:check` (nbt-less = NbtPredicate.ANY = auto-completes instantly;
//! see `Checkmark`). v1 emits the well-supported subset and relies on codec
//! defaults for the rest (icon/title/settings omitted). The grounding gate
//! (build_index/validate_graph) remains the id-correctness seam.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestGraph {
    pub title: String,
    pub chapters: Vec<QuestChapter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestChapter {
    pub id: String,
    pub title: String,
    pub quests: Vec<QuestNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestNode {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<QuestTask>,
    #[serde(default)]
    pub rewards: Vec<QuestReward>,
    /// Recipe facet (Slice 2 — recipes are a quest-graph node facet, not a
    /// silo). A node may carry 0+ custom recipes (usually 1). When non-empty
    /// the node ALWAYS surfaces as a Heracles quest with an auto `item` task
    /// on the PRIMARY (first) recipe's `result` (synthesized at emit time, not
    /// stored here — the curator schema forbids author-supplied `tasks` on a
    /// recipe node), and every node's recipes are aggregated into one Open
    /// Loader datapack. Each recipe's datapack id is DERIVED deterministically
    /// from the owning node (`anvil:<stable_hex(chapter:node:recipe:i)>`) and
    /// stamped in place before validate/emit — the curator never supplies one.
    /// `#[serde(default)]` so every pre-existing graph / `anvil-quests.json` /
    /// test decodes byte-unchanged (a node with no `recipes` is a plain quest).
    #[serde(default)]
    pub recipes: Vec<crate::recipe::RecipeDef>,
    /// Content facet (Slice 3 — provisioned boss/site/gate are a quest-graph
    /// node facet, mirroring the recipe facet). When `Some`, this node is a
    /// provisioned-content node: it ALWAYS surfaces as a Heracles quest with
    /// an auto `GatherItem` task on the unique NBT token its boss drops
    /// (synthesized at emit time, never stored — the curator schema forbids
    /// author-supplied `tasks` on a content node), and the whole facet is
    /// emitted into the Anvil content datapack
    /// (`config/openloader/data/anvil-content/`, a sibling of the Slice-2
    /// recipe datapack). All derived ids are keyed off the owning node
    /// (`anvil:<content_hex>_*`) so they are Anvil-authored and collision-free
    /// by construction. `#[serde(default)]` so every pre-existing graph /
    /// `anvil-quests.json` / test decodes byte-unchanged (a node with no
    /// `content` is a plain quest/recipe node).
    #[serde(default)]
    pub content: Option<crate::content::ContentSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuestTask {
    /// Have/obtain an item anywhere in inventory. Maps to `heracles:item`
    /// (`GatherItemTask`). Kept as-is for backward compat with graphs saved by
    /// the old code; `GatherItem` is the nbt-capable superset.
    Item { id: String, count: i64 },
    /// Collect an item, optionally NBT-discriminated. Maps to `heracles:item`
    /// (`GatherItemTask`, the event-collect form). The codec field is `item`
    /// (id string or `#tag`), `nbt` (SNBT compound, optional), `amount`.
    GatherItem {
        item: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nbt: Option<String>,
        count: i64,
    },
    /// Defeat an entity. Maps to `heracles:kill_entity` whose codec field
    /// `entity` is a `RestrictedEntityPredicate` OBJECT (`{type, nbt?, ...}`),
    /// NOT a bare string. `entity_type` carries the §2 variant ladder's HIGH
    /// rung (distinct type or `#tag`); `nbt` is the LOW rung discriminator
    /// (only emitted when the model supplies one — Anvil never invents it).
    /// `#[serde(alias = "entity")]` keeps graphs saved by the old
    /// `Kill { entity, count }` shape decoding unchanged.
    Kill {
        #[serde(alias = "entity")]
        entity_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nbt: Option<String>,
        count: i64,
    },
    Advancement { id: String },
    /// Enter a biome (e.g. "minecraft:soul_sand_valley").
    Biome { biome: String },
    /// Enter a dimension (e.g. "minecraft:the_nether").
    Dimension { dimension: String },
    /// Locate/enter a structure (e.g. "minecraft:fortress").
    Structure { structure: String },
    /// Craft via a recipe id (e.g. "create:crushing/andesite").
    Recipe { recipe: String },
    /// All nested tasks must complete. Maps to `heracles:composite`
    /// (`CompositeTask`, nested `tasks` array).
    Composite {
        #[serde(default)]
        tasks: Vec<QuestTask>,
    },
    /// Reach a statistic threshold. Maps to `heracles:stat` (`StatTask`).
    Stat { stat: String, target: i64 },
    /// Be-in a dimension/biome/structure. VERIFIED against Heracles 1.20.x
    /// source (2026-05-17): there is NO combined location task —
    /// `heracles:location`'s codec is a single Minecraft `LocationPredicate`,
    /// not `{dimension,biome,structure}`. So this decomposes at emit time into
    /// the source-verified separate tasks (`heracles:changed_dimension`/`to`,
    /// `heracles:biome`/`biomes`, `heracles:structure`/`structures`),
    /// composited via `heracles:composite` when more than one is set.
    Location {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dimension: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        biome: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structure: Option<String>,
    },
    /// Narrative/flavor node. Maps to `heracles:check` (`CheckTask`), emitted
    /// with NO `nbt`. VERIFIED against Heracles 1.20.x source (2026-05-17):
    /// the `nbt` field defaults to `NbtPredicate.ANY`, which ALWAYS matches —
    /// so an nbt-less check **auto-completes the instant it is evaluated**. It
    /// is NOT a manual click-to-complete and it CANNOT gate progression (it
    /// never blocks). Correct use is a free pass-through lore/flavor beat that
    /// immediately unlocks its dependents; anything that must actually gate or
    /// require a player action must be a real task or a content-boss node.
    Checkmark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuestReward {
    Item { id: String, count: i64 },
    Xp { amount: i64 },
    Command { command: String },
}

/// Allowed IDs for grounding.
///
/// ADDITIVE evolution (Slice 1): the three legacy `BTreeSet`s keep their
/// original NAMESPACE-marker semantics and still drive the
/// namespace-fallback path, so every pre-existing test and `recipe.rs`'s
/// namespace grounding keep working unchanged. The new fields carry CONCRETE
/// grounding from the pack's real registry (`crate::registry`):
///
/// - `vocab` — concrete ids scanned from the resolved jars (tier 1).
/// - `unscanned` — pinned-mod namespaces whose jar was not on disk at scan
///   time; an id in one of these is accepted as LOW-CONFIDENCE (never a hard
///   fail) because the scan could not see it (jar-absence degrade, design §2).
/// - `authored` — Anvil-authored allowlist (tier 2): ids Anvil's own
///   datapacks emit (recipe `anvil:<hex>`, future boss/site/gate ids).
///   Slice 2/3 populate it via `with_authored`.
/// - `has_vocab` — true iff ANY jar was scanned. When false the index is in
///   pure namespace mode (the historical behaviour); the no-jars case (all
///   existing tests) hits this and is byte-for-byte unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AllowedIndex {
    pub items: BTreeSet<String>,
    pub entities: BTreeSet<String>,
    pub advancements: BTreeSet<String>,
    #[serde(default)]
    pub vocab: crate::registry::RegistryVocab,
    #[serde(default)]
    pub unscanned: BTreeSet<String>,
    #[serde(default)]
    pub authored: BTreeSet<String>,
    #[serde(default)]
    pub has_vocab: bool,
}

impl AllowedIndex {
    /// Register Anvil-authored ids that grounding always accepts (tier 2).
    /// The `anvil` namespace is always authored; Slice 2/3 inject concrete
    /// ids (recipe `anvil:<hex>`, boss/site/gate ids) through here.
    pub fn with_authored(mut self, ids: impl IntoIterator<Item = String>) -> Self {
        self.authored.extend(ids);
        self
    }

    /// Is `id` Anvil-authored (tier 2)? The `anvil` namespace is always
    /// authored (everything Anvil's companion datapack registers lives
    /// there); plus any explicitly injected id.
    fn is_authored(&self, id: &str) -> bool {
        namespace_of(id) == "anvil" || self.authored.contains(id)
    }

    /// True if `id`'s namespace was pinned but its jar was not scanned (so
    /// the scan could not possibly contain it — accept low-confidence).
    fn is_unscanned_ns(&self, id: &str) -> bool {
        self.unscanned.contains(namespace_of(id))
    }
}

/// How an id grounded, decided by `ground_id`. The caller maps a hard miss to
/// the right `Unknown*` issue and a soft hit to `LowConfidenceId`.
enum Grounded {
    /// Concrete hit in the real registry, or Anvil-authored: fully verified.
    Ok,
    /// Accepted but unverified (jar absent / Anvil-authored-by-namespace /
    /// namespace-only fallback mode). NOT a hard fail; surfaced as
    /// `LowConfidenceId` so the model/user knows it wasn't proven.
    LowConfidence(&'static str),
    /// Namespace pinned-and-scanned but the concrete id is absent from the
    /// real registry: a fabricated id (the `cobblemon:mewtwo` case). Hard.
    Unknown,
}

impl AllowedIndex {
    /// Ground a `#ns:path` TAG ref. Identical decision order to `ground_id`
    /// but the concrete check is against `vocab.tags` (a tag id never lives
    /// in the typed item/entity/biome set). The caller passes the already-
    /// stripped (`#`-less) id.
    fn ground_tag(
        &self,
        id: &str,
        ns_markers: &BTreeSet<String>,
    ) -> Grounded {
        if !self.has_vocab {
            return if ns_markers.contains(namespace_of(id)) {
                Grounded::LowConfidence("namespace-only")
            } else {
                Grounded::Unknown
            };
        }
        if self.is_authored(id) {
            return Grounded::Ok;
        }
        if self.vocab.tags.contains(id) {
            return Grounded::Ok;
        }
        if self.is_unscanned_ns(id) {
            return Grounded::LowConfidence("unscanned");
        }
        Grounded::Unknown
    }

    /// Ground one concrete id against the matching `RegistryVocab` set
    /// (`pick`), with the namespace set as the legacy fallback marker.
    ///
    /// Decision order (design-doc §2):
    ///  1. namespace-only mode (no jars scanned) → legacy namespace check;
    ///     pass = LowConfidence("namespace-only"), fail = Unknown.
    ///  2. Anvil-authored (tier 2) → Ok.
    ///  3. concrete hit in the real registry set → Ok.
    ///  4. namespace unscanned (jar absent) → LowConfidence("unscanned").
    ///  5. namespace pinned-and-scanned but id absent → Unknown (fabricated).
    ///  6. namespace not pinned at all → Unknown (typo'd namespace).
    fn ground_id(
        &self,
        id: &str,
        ns_markers: &BTreeSet<String>,
        pick: fn(&crate::registry::RegistryVocab) -> &BTreeSet<String>,
    ) -> Grounded {
        if !self.has_vocab {
            // Historical behaviour: namespace marker check only. A pass here
            // is genuinely unverified (we never saw a real registry), so it
            // is reported as low-confidence rather than silently "Ok" — but
            // it is NOT a hard fail (backward compatible).
            return if ns_markers.contains(namespace_of(id)) {
                Grounded::LowConfidence("namespace-only")
            } else {
                Grounded::Unknown
            };
        }
        if self.is_authored(id) {
            return Grounded::Ok;
        }
        if pick(&self.vocab).contains(id) {
            return Grounded::Ok;
        }
        if self.is_unscanned_ns(id) {
            return Grounded::LowConfidence("unscanned");
        }
        Grounded::Unknown
    }

    /// Ground a quest-task ref that may be a plain id OR a `#ns:path` tag.
    /// A `#`-prefixed ref grounds against `vocab.tags`; otherwise the typed
    /// `pick` set. `ns_markers` is the legacy fallback set for both modes.
    /// This is the single grounding entry point `check_task` uses so the
    /// tag-vs-typed branch lives in exactly one place.
    fn ground_ref(
        &self,
        raw: &str,
        ns_markers: &BTreeSet<String>,
        pick: fn(&crate::registry::RegistryVocab) -> &BTreeSet<String>,
    ) -> Grounded {
        let id = strip_tag(raw);
        if raw.starts_with('#') {
            self.ground_tag(id, ns_markers)
        } else {
            self.ground_id(id, ns_markers, pick)
        }
    }

    /// Slice-2 recipe-grounding seam: classify ONE recipe item/tag/result id
    /// against this Slice-1 index so `crate::recipe::validate_recipes` gets the
    /// SAME concrete grounding as quest tasks (the `cobblemon:mewtwo`-class fix
    /// applies to recipe ingredients/results too) without duplicating the
    /// tier ladder. `is_tag` selects `vocab.tags` vs `vocab.items` (recipe
    /// `Ingredient::Tag` is a bare `ns:path`, never `#`-prefixed). Returns:
    ///  - `Ok`            fully grounded (concrete hit or Anvil-authored — the
    ///                    derived `anvil:<hex>` recipe id self-grounds here);
    ///  - `LowConfidence` accepted but unverified (jar absent / namespace-only
    ///                    fallback) — NOT a hard fail, surfaced not gated;
    ///  - `Unknown`       fabricated (namespace pinned-and-scanned but absent,
    ///                    or namespace not pinned at all) — hard fail.
    pub(crate) fn ground_recipe_id(
        &self,
        id: &str,
        is_tag: bool,
    ) -> RecipeGrounding {
        let g = if is_tag {
            self.ground_tag(id, &self.items)
        } else {
            self.ground_id(id, &self.items, |v| &v.items)
        };
        match g {
            Grounded::Ok => RecipeGrounding::Ok,
            Grounded::LowConfidence(r) => RecipeGrounding::LowConfidence(r),
            Grounded::Unknown => RecipeGrounding::Unknown,
        }
    }

    /// Slice-3 content-grounding seam: classify ONE content `entity` ref
    /// (the boss base entity, a plain `ns:path` or `#ns:path` tag) against
    /// this Slice-1 index so `crate::content::validate_content` gets the SAME
    /// concrete grounding ladder as quest `Kill` tasks (a fabricated
    /// `cobblemon:mewtwo`-class boss entity is a hard reject) without
    /// duplicating the tier logic. `is_tag` selects `vocab.tags` vs
    /// `vocab.entities`. Maps 1:1 to `RecipeGrounding` (the public mirror) so
    /// the internal `Grounded` enum stays private.
    pub(crate) fn ground_content_entity(
        &self,
        id: &str,
        is_tag: bool,
    ) -> RecipeGrounding {
        let g = if is_tag {
            self.ground_tag(id, &self.entities)
        } else {
            self.ground_id(id, &self.entities, |v| &v.entities)
        };
        match g {
            Grounded::Ok => RecipeGrounding::Ok,
            Grounded::LowConfidence(r) => RecipeGrounding::LowConfidence(r),
            Grounded::Unknown => RecipeGrounding::Unknown,
        }
    }
}

/// Public mirror of the private `Grounded` ladder, so `crate::recipe` can
/// classify recipe ids through `AllowedIndex::ground_recipe_id` without the
/// internal enum leaking. Maps 1:1 to `Grounded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecipeGrounding {
    Ok,
    LowConfidence(&'static str),
    Unknown,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuestIssue {
    UnknownItem { quest: String, id: String },
    UnknownEntity { quest: String, id: String },
    /// Slice 1: an id that grounded but could NOT be verified against the
    /// pack's real registry — either the mod's jar was not on disk at scan
    /// time (`reason:"unscanned"`), the pack is in namespace-only fallback
    /// mode (`reason:"namespace-only"`, no jars scanned at all). Accepted
    /// (NOT a write-blocking hard fail) but surfaced so the model/user knows
    /// it was not proven. Filtered out of the blocking set exactly like the
    /// quality issues.
    LowConfidenceId {
        quest: String,
        id: String,
        reason: String,
    },
    MissingDependency { quest: String, dep: String },
    CyclicDependency { quest: String },
    /// A quest has no prerequisites and nothing depends on it (a dead island).
    /// Only flagged once the graph is big enough to expect a connected web.
    OrphanQuest { quest: String },
    /// A chapter (past the first) shares no dependency edge with any other
    /// chapter, so it floats disconnected from the progression spine.
    DisconnectedChapter { chapter: String },
    /// The graph is too sparse to be a real questline: too few prerequisite
    /// edges for its size (near-linear / mostly unconnected).
    TooSparse { quests: usize, deps: usize },
    /// Slice 2: a HARD recipe-facet defect surfaced through the quest channel
    /// — a structural problem (bad pattern, unbound char, key not in pattern,
    /// empty/over-full ingredients) or a duplicate DERIVED recipe id. Checked
    /// on EVERY call (never filtered), so the model fixes the offending node's
    /// `recipes` before anything is written. `recipe` is the derived
    /// `anvil:<hex>` id; `detail` is the snake_case `RecipeIssue` kind.
    RecipeStructural { recipe: String, detail: String },
    /// Slice 2: a recipe QUALITY defect (`orphan_recipe` = touches no modded
    /// namespace on either side; `set_has_no_mod_output` = the whole datapack
    /// adds zero modded outputs). FINAL-ONLY and only when the recipe count is
    /// >= 3 (mirrors the quest quality gate's final-only + size leniency);
    /// filtered out of the blocking set on non-final calls exactly like
    /// `OrphanQuest`/`TooSparse`. `recipe` is empty for the set-wide variant.
    RecipeQuality { recipe: String, detail: String },
    /// Slice 3: TOKEN ATOMICITY (design §6 #12) — a content boss node that
    /// cannot deterministically emit its FULL atomic set {summon fn, tick fn,
    /// onkill fn that gives the token, kill-advancement, trigger,
    /// GatherItem-on-token task}: a blank required field or the reserved
    /// `region` trigger. HARD, checked on EVERY call (never filtered), so the
    /// model fixes the offending content node before anything is written.
    /// `node` is the owning quest node id; `detail` is the human reason.
    ContentIncomplete { node: String, detail: String },
    /// Phase 1B difficulty gate: a quest in chapter `chapter_index` (0-based)
    /// carries a task whose difficulty `task_tier` exceeds that chapter's
    /// ceiling `max_allowed` (chapter N caps at tier min(N+1,5); the final
    /// chapter may use 5). The classic offender: `adventuring_time` (visit all
    /// biomes, T5) used as a Chapter-I task/dependency. HARD, checked on EVERY
    /// call (NOT in the final-only filter) so the model must replace the task
    /// with a chapter-appropriate one before anything is written.
    OverdifficultForChapter {
        quest: String,
        chapter_index: usize,
        task: String,
        task_tier: u8,
        max_allowed: u8,
    },
}

/// Difficulty tier of a vanilla advancement: T1 (first ~10 min) .. T5
/// (completionist / post-credits). Tiering follows the well-known vanilla
/// advancement tree (minecraft.wiki "Advancement"): a constant's tier is its
/// position on the story/nether/end/adventure/husbandry spine. Only
/// load-bearing, uncontroversial ids are pinned (e.g. `adventuring_time` =
/// all biomes = T5; `netherite_armor` = T4; `kill_all_mobs` = T5). ANY id not
/// listed — including every modded advancement — defaults to T3 (mid,
/// known-but-unproven): never auto-reject an unrecognised id, only the
/// clearly-too-hard vanilla ones.
fn advancement_tier(id: &str) -> u8 {
    let Some(a) = id.strip_prefix("minecraft:") else {
        return 3; // modded advancement: conservative mid
    };
    match a {
        "story/root" | "story/mine_stone" | "story/upgrade_tools"
        | "story/smelt_iron" | "story/obtain_armor" | "story/lava_bucket"
        | "story/iron_tools" | "story/sleep_in_bed" | "husbandry/root"
        | "husbandry/plant_seed" | "husbandry/breed_an_animal"
        | "husbandry/tame_an_animal" | "adventure/root"
        | "adventure/kill_a_mob" => 1,
        "story/mine_diamond" | "story/deflect_arrow" | "story/form_obsidian"
        | "story/enter_the_nether" | "nether/root"
        | "husbandry/fishy_business" | "husbandry/tactical_fishing"
        | "adventure/sleep_in_bed" | "adventure/trade"
        | "adventure/shoot_arrow" | "adventure/throw_trident"
        | "adventure/honey_block_slide" => 2,
        "story/shiny_gear" | "story/enchant_item" | "nether/get_wither_skull"
        | "nether/obtain_blaze_rod" | "nether/find_fortress"
        | "nether/fast_travel" | "nether/distract_piglin"
        | "nether/ride_strider" | "adventure/totem_of_undying"
        | "adventure/trade_at_world_height" | "husbandry/balanced_diet"
        | "husbandry/obtain_netherite_hoe" | "husbandry/wax_on" => 3,
        "nether/obtain_ancient_debris" | "nether/netherite_armor"
        | "nether/summon_wither" | "nether/create_beacon"
        | "nether/all_potions" | "nether/uneasy_alliance"
        | "nether/explore_nether" | "end/root" | "end/enter_end_gateway"
        | "end/dragon_egg" | "end/levitate" | "adventure/two_birds_one_arrow"
        | "adventure/whos_the_pillager_now"
        | "adventure/very_very_frightening"
        | "adventure/hero_of_the_village" => 4,
        "adventure/adventuring_time" | "adventure/kill_all_mobs"
        | "adventure/bullseye" | "husbandry/bred_all_animals"
        | "husbandry/complete_catalogue" | "husbandry/froglights"
        | "nether/create_full_beacon" | "nether/all_effects"
        | "end/respawn_dragon" | "end/dragon_breath" | "end/find_end_city"
        | "end/elytra" | "adventure/play_jukebox_in_meadows"
        | "adventure/walk_on_powder_snow_with_leather_boots" => 5,
        _ => 3, // unpinned vanilla advancement: conservative mid
    }
}

fn item_tier(id: &str) -> u8 {
    if !id.starts_with("minecraft:") {
        3 // modded item: roughly a tech/progression item, mid
    } else if id.contains("netherite")
        || id.contains("diamond")
        || id.contains("elytra")
        || id.contains("nether_star")
        || id.contains("dragon")
        || id.contains("beacon")
        || id.contains("totem")
    {
        3
    } else {
        1
    }
}

/// Difficulty tier of a single task. Pure; unit-tested. Tiers per the
/// Phase-1B taxonomy (advancement table above; task-type base weights from
/// the analysis subagent's report).
fn task_tier(t: &QuestTask) -> u8 {
    match t {
        QuestTask::Checkmark => 1,
        QuestTask::Stat { target, .. } => {
            if *target >= 72_000 {
                2
            } else {
                1
            }
        }
        QuestTask::Item { id, .. } => item_tier(id),
        QuestTask::GatherItem { item, .. } => item_tier(item),
        QuestTask::Kill {
            entity_type, count, ..
        } => {
            if *count >= 10 || !entity_type.starts_with("minecraft:") {
                3
            } else {
                2
            }
        }
        QuestTask::Biome { .. } => 2,
        QuestTask::Dimension { dimension } => {
            if dimension.contains("the_end") {
                4
            } else {
                2
            }
        }
        QuestTask::Structure { .. } => 3,
        QuestTask::Recipe { .. } => 3,
        QuestTask::Advancement { id } => advancement_tier(id),
        QuestTask::Location {
            dimension,
            biome,
            structure,
        } => {
            let mut m = 1u8;
            if let Some(d) = dimension {
                m = m.max(if d.contains("the_end") { 4 } else { 2 });
            }
            if biome.is_some() {
                m = m.max(2);
            }
            if structure.is_some() {
                m = m.max(3);
            }
            m
        }
        QuestTask::Composite { tasks } => {
            tasks.iter().map(task_tier).max().unwrap_or(1)
        }
    }
}

/// Short label for a task, for the `OverdifficultForChapter` message.
fn task_label(t: &QuestTask) -> String {
    match t {
        QuestTask::Advancement { id } => format!("advancement:{id}"),
        QuestTask::Item { id, .. } => format!("item:{id}"),
        QuestTask::GatherItem { item, .. } => format!("item:{item}"),
        QuestTask::Kill { entity_type, .. } => format!("kill:{entity_type}"),
        QuestTask::Biome { biome } => format!("biome:{biome}"),
        QuestTask::Dimension { dimension } => format!("dimension:{dimension}"),
        QuestTask::Structure { structure } => format!("structure:{structure}"),
        QuestTask::Recipe { recipe } => format!("recipe:{recipe}"),
        QuestTask::Stat { stat, target } => format!("stat:{stat}>={target}"),
        QuestTask::Location { .. } => "location".to_string(),
        QuestTask::Composite { .. } => "composite".to_string(),
        QuestTask::Checkmark => "checkmark".to_string(),
    }
}

/// Chapter difficulty ceiling: chapter index `idx` (0-based) of `n` caps at
/// tier `min(idx+1, 5)`; the FINAL chapter is always allowed 5 (it is the
/// designed climax). So Chapter I (idx 0) = T1 only, Chapter II = T2, ...
fn chapter_max_tier(idx: usize, n: usize) -> u8 {
    if n > 0 && idx == n - 1 {
        return 5;
    }
    ((idx as u8).saturating_add(1)).min(5)
}

/// Phase-1B difficulty gate: every task whose tier exceeds its chapter's
/// ceiling. Pure + deterministic; called from `validate_graph` so it runs on
/// EVERY `generate_quests` call and is write-blocking (it is intentionally
/// absent from curator.rs's final-only `retain` filter).
fn check_difficulty(g: &QuestGraph) -> Vec<QuestIssue> {
    let n = g.chapters.len();
    let mut out = Vec::new();
    for (ci, ch) in g.chapters.iter().enumerate() {
        let cap = chapter_max_tier(ci, n);
        for q in &ch.quests {
            for t in &q.tasks {
                let tier = task_tier(t);
                if tier > cap {
                    out.push(QuestIssue::OverdifficultForChapter {
                        quest: q.id.clone(),
                        chapter_index: ci,
                        task: task_label(t),
                        task_tier: tier,
                        max_allowed: cap,
                    });
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Stable 16-char uppercase-hex id from arbitrary key material.
///
/// `DefaultHasher` is SipHash-1-3 with a *fixed* initial state (unlike
/// `HashMap`'s `RandomState`), so this is fully deterministic across processes
/// and CI runs on the same toolchain — exactly what FTB Quests' stable id
/// requirement and our determinism test need.
pub(crate) fn stable_hex(key: &str) -> String {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    format!("{:016X}", h.finish())
}

/// The DERIVED Open Loader datapack id for the `i`-th recipe on the node
/// `node_id` in chapter `chapter_id`: `anvil:<stable_hex(chapter:node:recipe:i)>`.
///
/// Anvil-authored (the `anvil` namespace is the tier-2 allowlist root, so the
/// quest's item-on-result task + any self-reference ground cleanly) and
/// collision-free by the same content-stable hashing the quest ids use. The
/// curator never supplies a recipe id — this is the single source of it.
pub(crate) fn derived_recipe_id(
    chapter_id: &str,
    node_id: &str,
    i: usize,
) -> String {
    format!(
        "anvil:{}",
        stable_hex(&format!("{chapter_id}:{node_id}:recipe:{i}"))
    )
}

/// Aggregate every node's `recipes` into a transient `RecipeSet`, stamping
/// each `RecipeDef.id` with its DERIVED `anvil:<hex>` value (chapter+node+index
/// keyed). This is the single place the derivation happens; callers
/// (`write_quests`, `validate_graph`, `tool_generate_quests`) all go through
/// it so validation messages, the datapack filenames, and the authored
/// allowlist agree byte-for-byte. Nodes with no recipes contribute nothing,
/// so an empty result means "no recipe facets in this graph".
pub(crate) fn collect_recipe_set(g: &QuestGraph) -> crate::recipe::RecipeSet {
    let mut recipes = Vec::new();
    for ch in &g.chapters {
        for q in &ch.quests {
            for (i, r) in q.recipes.iter().enumerate() {
                let mut r = r.clone();
                r.id = derived_recipe_id(&ch.id, &q.id, i);
                recipes.push(r);
            }
        }
    }
    crate::recipe::RecipeSet { recipes }
}

/// Every derived `anvil:<hex>` recipe id in the graph — the Anvil-authored
/// allowlist seam (`AllowedIndex::with_authored`) so a recipe-node's
/// item-on-result task and any self-reference ground cleanly even before the
/// datapack is written.
pub(crate) fn authored_recipe_ids(g: &QuestGraph) -> Vec<String> {
    let mut ids = Vec::new();
    for ch in &g.chapters {
        for q in &ch.quests {
            for i in 0..q.recipes.len() {
                ids.push(derived_recipe_id(&ch.id, &q.id, i));
            }
        }
    }
    ids
}

/// Every derived `anvil:<hex>_*` content id in the graph (functions,
/// advancements, the totem recipe) — the Slice-3 analogue of
/// `authored_recipe_ids`. Seeded into the Anvil-authored allowlist
/// (`AllowedIndex::with_authored`) so the token GatherItem task and every
/// internal cross-reference ground cleanly even before the datapack is written.
pub(crate) fn authored_content_ids(g: &QuestGraph) -> Vec<String> {
    let mut ids = Vec::new();
    for ch in &g.chapters {
        for q in &ch.quests {
            if let Some(spec) = q.content.as_ref() {
                ids.extend(crate::content::facet_authored_ids(
                    &ch.id, &q.id, spec,
                ));
            }
        }
    }
    ids
}

/// True iff ANY node in the graph carries a content facet (the presence-gate
/// extension: content also needs the Open Loader datapack, exactly like
/// recipes).
pub(crate) fn any_content(g: &QuestGraph) -> bool {
    g.chapters
        .iter()
        .any(|c| c.quests.iter().any(|q| q.content.is_some()))
}

/// The PRIMARY recipe result item for a recipe-bearing node: the first
/// recipe's `result` (shaped/shapeless `result.item`, smelting `result`
/// string). This is the id the auto `item` task is synthesized on. `None`
/// when the node carries no recipes.
fn primary_recipe_result(q: &QuestNode) -> Option<String> {
    use crate::recipe::RecipeKind;
    q.recipes.first().map(|r| match &r.kind {
        RecipeKind::Shaped { result, .. } => result.item.clone(),
        RecipeKind::Shapeless { result, .. } => result.item.clone(),
        RecipeKind::Smelting { result, .. } => result.clone(),
    })
}

/// Extract the namespace of a (possibly) namespaced id. Minecraft ids default
/// to the `minecraft` namespace when no `:` is present.
///
/// `pub(crate)` so the recipe datapack generator (`crate::recipe`) reuses the
/// exact same namespace-grounding rule rather than duplicating it.
pub(crate) fn namespace_of(id: &str) -> &str {
    match id.split_once(':') {
        Some((ns, _)) => ns,
        None => "minecraft",
    }
}

/// The Heracles "group" key for a chapter. This single string is used in THREE
/// places that must never drift apart: (a) the key in each quest's
/// `display.groups` map, (b) the lines of `groups.txt` (which Heracles loads
/// IN ORDER to drive the Groups sidebar — `QuestHandler.loadGroups` /
/// `ClientQuests.groups()` on the 1.20.x branch), and (c) the cross-group
/// prerequisite-visibility lookup. A quest only renders in a group when its own
/// `display.groups` contains that exact key
/// (`QuestsScreen.java:57` filters `display().groups().containsKey(group)`),
/// so byte-equality of this string across (a)/(b)/(c) is load-bearing.
///
/// NOTE: the non-empty title is used verbatim (NOT trimmed) — Heracles reads
/// groups.txt line-by-line via commons-io `readLines`, and Anvil already emits
/// untrimmed titles as the map key; trimming here would orphan groups.
fn group_key(ch: &QuestChapter) -> String {
    if ch.title.trim().is_empty() {
        ch.id.clone()
    } else {
        ch.title.clone()
    }
}

// ---------------------------------------------------------------------------
// build_index
// ---------------------------------------------------------------------------

/// Build a NAMESPACE-MODE index from the namespaces present in the pack.
///
/// SIGNATURE PRESERVED (Slice 1): unchanged callers (`recipe.rs`,
/// `lib.rs::validate_quest_graph`, every existing test) keep getting exactly
/// the historical behaviour — namespace markers in the three legacy sets,
/// `has_vocab == false`. In this mode `validate_graph` checks an id's
/// namespace prefix only (catching typo'd / hallucinated mod namespaces while
/// staying offline) and reports a pass as `LowConfidenceId` ("namespace-only")
/// instead of silently accepting it: it is still NOT a write-blocking failure,
/// so behaviour for callers that ignore low-confidence issues is unchanged.
///
/// For CONCRETE registry grounding (the fabricated-id fix) use
/// `build_index_for_instance`, which scans the resolved jars.
pub fn build_index(mod_namespaces: &[String]) -> AllowedIndex {
    let mut idx = AllowedIndex::default();
    // Sentinel vanilla namespace is always allowed.
    let mut namespaces: Vec<String> = vec!["minecraft".to_string()];
    for ns in mod_namespaces {
        namespaces.push(ns.to_lowercase());
    }
    for ns in namespaces {
        idx.items.insert(ns.clone());
        idx.entities.insert(ns.clone());
        idx.advancements.insert(ns);
    }
    // has_vocab stays false (Default) -> namespace-fallback mode.
    idx
}

/// Build a CONCRETE-id index for an instance by scanning its resolved jars
/// (`crate::registry`), with the namespace set retained as the fallback
/// marker for any namespace whose jar was absent at scan time.
///
/// This is the Slice 1 fix for fabricated ids: an id whose namespace is
/// pinned-and-scanned but is NOT in the real registry is a hard
/// `UnknownItem`/`UnknownEntity`; an id whose mod jar was not on disk yet is
/// accepted low-confidence (jar-absence degrade, never blocks). Anvil-authored
/// ids (tier 2: the `anvil` namespace + injected `authored`) always pass.
///
/// Caching: the scan is persisted at `<instance_dir>/anvil-registry.json`
/// keyed by the pinned mod set; a matching cache is reused so repeated
/// `query_registry`/validation calls are cheap. A scan never blocks or errors
/// — a degraded (jar-absent) scan is still a valid, usable index.
pub fn build_index_for_instance(
    inst: &crate::instance::Instance,
    instance_dir: &Path,
    authored: impl IntoIterator<Item = String>,
) -> AllowedIndex {
    let scan = scan_or_cached(inst, instance_dir);

    // Legacy namespace markers: vanilla + every pinned mod's filename ns AND
    // every scanned mod-meta id (so a jar whose internal id differs from its
    // filename still grounds its own concrete ids). Keeps the fallback path
    // honest and identical in spirit to `build_index`.
    let mut idx = AllowedIndex::default();
    let mut ns_markers: BTreeSet<String> = BTreeSet::new();
    ns_markers.insert("minecraft".to_string());
    ns_markers.insert("anvil".to_string());
    for m in &inst.mods {
        let ns = crate::registry::filename_namespace(&m.path);
        if !ns.is_empty() {
            ns_markers.insert(ns);
        }
    }
    for mm in &scan.vocab.mod_meta {
        if !mm.id.is_empty() {
            ns_markers.insert(mm.id.to_lowercase());
        }
    }
    for ns in &ns_markers {
        idx.items.insert(ns.clone());
        idx.entities.insert(ns.clone());
        idx.advancements.insert(ns.clone());
    }

    // `ScanSource` is `Copy`; read it before `scan` is partially moved below.
    let dump_reconciled =
        scan.source == crate::registry::ScanSource::DumpReconciled;
    idx.has_vocab = !scan.vocab.is_empty();
    idx.vocab = scan.vocab;
    idx.unscanned = scan.unscanned;
    // Vanilla content (`minecraft:*`) lives in the bundled client/server jar,
    // which the launcher never pins into `inst.mods` and the static scan
    // therefore never sees. Without this, EVERY vanilla id (the bulk of any
    // real questline — `minecraft:diamond`, `minecraft:fortress`,
    // `minecraft:the_nether`) would hard-fail as Unknown once `has_vocab` is
    // true. Treat `minecraft` as unscanned-by-construction so vanilla refs
    // degrade to non-blocking LowConfidence, never a false hard reject.
    //
    // Slice 1.5: once a first-launch `/dump registry` has reconciled this
    // scan (`source == DumpReconciled`), the dedicated server's registry IS
    // the authoritative vanilla registry — `minecraft:*` is now concretely
    // grounded, so this construction-fallback would WRONGLY re-degrade real
    // vanilla ids back to low-confidence. Skip it in that case (and trust
    // `reconcile_scan`, which already trimmed `minecraft` from `unscanned`).
    if !dump_reconciled {
        idx.unscanned.insert("minecraft".to_string());
    }
    idx.with_authored(authored)
}

/// Load the cached scan if its mod-set key still matches, else scan the jars
/// and persist the result. Cache I/O failure is non-fatal (just re-scan).
fn scan_or_cached(
    inst: &crate::instance::Instance,
    instance_dir: &Path,
) -> crate::registry::ScanResult {
    let cache_path = instance_dir.join("anvil-registry.json");
    let key = crate::registry::mod_set_key(inst);
    if let Ok(txt) = std::fs::read_to_string(&cache_path) {
        if let Ok(cached) =
            serde_json::from_str::<crate::registry::ScanResult>(&txt)
        {
            if cached.mod_set_key == key {
                return cached;
            }
        }
    }
    let scan = crate::registry::scan_instance(inst, instance_dir);
    if let Ok(txt) = serde_json::to_string(&scan) {
        let _ = std::fs::create_dir_all(instance_dir);
        let _ = std::fs::write(&cache_path, txt);
    }
    scan
}

// ---------------------------------------------------------------------------
// validate_graph
// ---------------------------------------------------------------------------

/// A tag ref (`#ns:path`) grounds against the bare id (`ns:path`); a plain id
/// grounds as itself. Recipe ingredient refs use the bare form, quest task
/// refs use `#` — strip it once here so vocab lookups are uniform.
fn strip_tag(id: &str) -> &str {
    id.strip_prefix('#').unwrap_or(id)
}

/// Push the right issue for a `Grounded` outcome. `unknown` builds the
/// hard-fail issue (`UnknownItem`/`UnknownEntity`); a `LowConfidence` becomes
/// the non-blocking `LowConfidenceId`; `Ok` pushes nothing.
fn record(
    g: Grounded,
    quest_id: &str,
    id: &str,
    issues: &mut Vec<QuestIssue>,
    unknown: impl FnOnce(String, String) -> QuestIssue,
) {
    match g {
        Grounded::Ok => {}
        Grounded::LowConfidence(reason) => issues.push(QuestIssue::LowConfidenceId {
            quest: quest_id.to_string(),
            id: id.to_string(),
            reason: reason.to_string(),
        }),
        Grounded::Unknown => {
            issues.push(unknown(quest_id.to_string(), id.to_string()))
        }
    }
}

/// Ground one task's ids against the index, recursing into `Composite`.
///
/// Slice 1: when a real registry is available (`has_vocab`) every id is
/// checked for CONCRETE membership in its matching vocab set — items against
/// `vocab.items`, entities against `vocab.entities`, advancements/biomes/
/// structures/recipes against their sets (tags strip the leading `#`). A
/// fabricated id whose namespace IS pinned-and-scanned (e.g.
/// `cobblemon:mewtwo`) is a hard `UnknownItem`/`UnknownEntity`; a jar that was
/// not on disk degrades to non-blocking `LowConfidenceId`. With no jars
/// scanned the whole index is in namespace-fallback mode and every grounded
/// id is reported low-confidence ("namespace-only") — never a hard fail —
/// keeping the historical no-jars behaviour (existing tests) green.
///
/// `Dimension`/`Location`/`Stat`/`Checkmark` reference registries the static
/// scan does not enumerate (dimensions/stats are code-side); they stay
/// lenient (no issue) exactly as before, so this is additive only.
fn check_task(
    task: &QuestTask,
    quest_id: &str,
    idx: &AllowedIndex,
    issues: &mut Vec<QuestIssue>,
) {
    use crate::registry::RegistryVocab as V;
    match task {
        QuestTask::Item { id, .. } => {
            let g = idx.ground_ref(id, &idx.items, |v: &V| &v.items);
            record(g, quest_id, id, issues, |quest, id| {
                QuestIssue::UnknownItem { quest, id }
            });
        }
        QuestTask::GatherItem { item, .. } => {
            let g = idx.ground_ref(item, &idx.items, |v: &V| &v.items);
            record(g, quest_id, item, issues, |quest, id| {
                QuestIssue::UnknownItem { quest, id }
            });
        }
        QuestTask::Kill { entity_type, .. } => {
            let g =
                idx.ground_ref(entity_type, &idx.entities, |v: &V| &v.entities);
            record(g, quest_id, entity_type, issues, |quest, id| {
                QuestIssue::UnknownEntity { quest, id }
            });
        }
        QuestTask::Advancement { id } => {
            let g = idx.ground_ref(
                id,
                &idx.advancements,
                |v: &V| &v.advancements,
            );
            record(g, quest_id, id, issues, |quest, id| {
                QuestIssue::UnknownItem { quest, id }
            });
        }
        QuestTask::Biome { biome } => {
            let g = idx.ground_ref(biome, &idx.items, |v: &V| &v.biomes);
            record(g, quest_id, biome, issues, |quest, id| {
                QuestIssue::UnknownItem { quest, id }
            });
        }
        QuestTask::Structure { structure } => {
            let g =
                idx.ground_ref(structure, &idx.items, |v: &V| &v.structures);
            record(g, quest_id, structure, issues, |quest, id| {
                QuestIssue::UnknownItem { quest, id }
            });
        }
        QuestTask::Recipe { recipe } => {
            let g =
                idx.ground_ref(recipe, &idx.items, |v: &V| &v.recipe_ids);
            record(g, quest_id, recipe, issues, |quest, id| {
                QuestIssue::UnknownItem { quest, id }
            });
        }
        QuestTask::Composite { tasks } => {
            for t in tasks {
                check_task(t, quest_id, idx, issues);
            }
        }
        // Dimensions/stats are code-registered and NOT enumerable by the
        // static scan; checkmark/location have no grounded id. Lenient,
        // exactly as before (additive change only).
        QuestTask::Dimension { .. }
        | QuestTask::Stat { .. }
        | QuestTask::Location { .. }
        | QuestTask::Checkmark => {}
    }
}

/// Map a recipe-engine `RecipeIssue` onto the quest issue channel (Slice 2).
/// Grounding misses reuse the existing `UnknownItem`/`LowConfidenceId`
/// variants (the `quest` field becomes the derived `anvil:<hex>` recipe id —
/// accurate and avoids variant explosion); structural/duplicate become the
/// HARD `RecipeStructural`; the two quality variants become the FINAL-ONLY
/// `RecipeQuality`. `detail` is the snake_case `RecipeIssue` kind so the
/// curator can pattern-match and the message stays human-readable.
fn recipe_issue_to_quest(ri: crate::recipe::RecipeIssue) -> QuestIssue {
    use crate::recipe::RecipeIssue as R;
    match ri {
        R::UnknownItem { recipe, id } => {
            QuestIssue::UnknownItem { quest: recipe, id }
        }
        R::LowConfidenceId {
            recipe,
            id,
            reason,
        } => QuestIssue::LowConfidenceId {
            quest: recipe,
            id,
            reason,
        },
        R::DuplicateRecipeId { recipe } => QuestIssue::RecipeStructural {
            recipe,
            detail: "duplicate_recipe_id".to_string(),
        },
        R::BadPattern { recipe } => QuestIssue::RecipeStructural {
            recipe,
            detail: "bad_pattern".to_string(),
        },
        R::KeyNotInPattern { recipe, key } => QuestIssue::RecipeStructural {
            recipe,
            detail: format!("key_not_in_pattern:{key}"),
        },
        R::PatternCharUnbound { recipe, ch } => QuestIssue::RecipeStructural {
            recipe,
            detail: format!("pattern_char_unbound:{ch}"),
        },
        R::EmptyIngredients { recipe } => QuestIssue::RecipeStructural {
            recipe,
            detail: "empty_ingredients".to_string(),
        },
        R::OrphanRecipe { recipe } => QuestIssue::RecipeQuality {
            recipe,
            detail: "orphan_recipe".to_string(),
        },
        R::SetHasNoModOutput => QuestIssue::RecipeQuality {
            recipe: String::new(),
            detail: "set_has_no_mod_output".to_string(),
        },
    }
}

/// Map a content-engine `ContentIssue` onto the quest issue channel (Slice 3),
/// exactly as `recipe_issue_to_quest` does for recipes. Grounding misses reuse
/// the existing `UnknownItem`/`LowConfidenceId` variants (the `quest` field
/// becomes the content node id — accurate, no variant explosion); the
/// atomicity miss becomes the HARD `ContentIncomplete`. ALL content issues are
/// hard (no quality/final-only tier — a partial boss is never acceptable per
/// design §6 #12), so none are filtered on non-final calls.
fn content_issue_to_quest(ci: crate::content::ContentIssue) -> QuestIssue {
    use crate::content::ContentIssue as C;
    match ci {
        C::UnknownItem { node, id } => {
            QuestIssue::UnknownItem { quest: node, id }
        }
        C::LowConfidenceId { node, id, reason } => QuestIssue::LowConfidenceId {
            quest: node,
            id,
            reason,
        },
        C::ContentIncomplete { node, detail } => {
            QuestIssue::ContentIncomplete { node, detail }
        }
    }
}

/// Validate the graph against the index: DAG (no cycles), all deps resolve,
/// task/reward IDs are within the allowed index. Empty = ok.
///
/// Issue order is stable: chapters -> quests -> (missing deps, then task/reward
/// id checks), followed by the global cycle pass, then the recipe-facet pass.
pub fn validate_graph(g: &QuestGraph, idx: &AllowedIndex) -> Vec<QuestIssue> {
    let mut issues = Vec::new();

    // All quest ids across all chapters.
    let mut all_ids: BTreeSet<&str> = BTreeSet::new();
    for ch in &g.chapters {
        for q in &ch.quests {
            all_ids.insert(q.id.as_str());
        }
    }

    // Per-quest checks: missing dependencies + unknown task/reward namespaces.
    for ch in &g.chapters {
        for q in &ch.quests {
            for dep in &q.deps {
                if !all_ids.contains(dep.as_str()) {
                    issues.push(QuestIssue::MissingDependency {
                        quest: q.id.clone(),
                        dep: dep.clone(),
                    });
                }
            }

            for task in &q.tasks {
                check_task(task, &q.id, idx, &mut issues);
            }

            for reward in &q.rewards {
                if let QuestReward::Item { id, .. } = reward {
                    let g = idx.ground_ref(
                        id,
                        &idx.items,
                        |v: &crate::registry::RegistryVocab| &v.items,
                    );
                    record(g, &q.id, id, &mut issues, |quest, id| {
                        QuestIssue::UnknownItem { quest, id }
                    });
                }
            }
        }
    }

    // Global cycle detection over the prerequisite (dep) graph.
    issues.extend(detect_cycles(g));

    // Interconnection / quality gate: structurally forces the model to hand
    // back a real questline web, not a sparse pile of disconnected nodes.
    issues.extend(interconnection_issues(g));

    // Slice 2: recipe-facet validation, folded into the SAME issue channel.
    // Build the transient `RecipeSet` (derived `anvil:<hex>` ids stamped) and
    // run the recipe engine's validator against the SAME Slice-1 index, so
    // recipe ingredient/result ids get concrete grounding too. We always pass
    // `is_final = true` so the quality variants come back; the curator filters
    // `RecipeQuality` (like `OrphanQuest`/`TooSparse`) on non-final calls. The
    // `< 3` size leniency lives inside `validate_recipes::quality_issues`.
    // `validate_recipes` already emits in a deterministic order (recipes in
    // input order = chapter+node+index order, fixed sub-order within each).
    let set = collect_recipe_set(g);
    if !set.recipes.is_empty() {
        for ri in crate::recipe::validate_recipes(&set, idx, true) {
            issues.push(recipe_issue_to_quest(ri));
        }
    }

    // Slice 3: content-facet validation, folded into the SAME issue channel.
    // Concrete grounding (entity/equipment/token_item) reuses the Slice-1
    // index; TOKEN ATOMICITY (§6 #12) is a HARD `ContentIncomplete` on every
    // call (never filtered — a partial boss is the worst failure class). The
    // content engine already emits in deterministic graph order.
    if any_content(g) {
        for ci in crate::content::validate_content(g, idx) {
            issues.push(content_issue_to_quest(ci));
        }
    }

    // Phase 1B: difficulty gate. HARD, every call (not in the final-only
    // filter) — an over-hard early quest (e.g. Adventuring Time in Chapter I)
    // must be replaced before anything is written.
    issues.extend(check_difficulty(g));

    issues
}

/// Quality gate: orphan islands, chapters with no link to the rest, and
/// graphs too sparse to be a real questline. Thresholds scale with size so a
/// small focused pack isn't punished for being small.
fn interconnection_issues(g: &QuestGraph) -> Vec<QuestIssue> {
    let mut out = Vec::new();

    // id -> chapter id, and the set of all quest ids.
    let mut chapter_of: HashMap<&str, &str> = HashMap::new();
    for ch in &g.chapters {
        for q in &ch.quests {
            chapter_of.insert(q.id.as_str(), ch.id.as_str());
        }
    }
    let n = chapter_of.len();
    if n == 0 {
        return out;
    }

    // Resolving prerequisite edges only (unknown deps are MissingDependency).
    let mut depended_on: BTreeSet<&str> = BTreeSet::new();
    let mut total_edges = 0usize;
    // chapters that touch any cross-chapter edge (either direction).
    let mut linked_chapters: BTreeSet<&str> = BTreeSet::new();
    for ch in &g.chapters {
        for q in &ch.quests {
            for d in &q.deps {
                let d = d.as_str();
                if let Some(&dep_ch) = chapter_of.get(d) {
                    total_edges += 1;
                    depended_on.insert(d);
                    if dep_ch != ch.id.as_str() {
                        linked_chapters.insert(ch.id.as_str());
                        linked_chapters.insert(dep_ch);
                    }
                }
            }
        }
    }

    // Orphan: no prerequisites AND nothing depends on it. Only meaningful once
    // the graph is big enough to expect a connected web.
    if n >= 6 {
        for ch in &g.chapters {
            for q in &ch.quests {
                let has_dep = q
                    .deps
                    .iter()
                    .any(|d| chapter_of.contains_key(d.as_str()));
                if !has_dep && !depended_on.contains(q.id.as_str()) {
                    out.push(QuestIssue::OrphanQuest {
                        quest: q.id.clone(),
                    });
                }
            }
        }
    }

    // Every chapter past the first must hook into the progression spine via at
    // least one cross-chapter dependency edge.
    if g.chapters.len() > 1 {
        for ch in g.chapters.iter().skip(1) {
            if !linked_chapters.contains(ch.id.as_str()) {
                out.push(QuestIssue::DisconnectedChapter {
                    chapter: ch.id.clone(),
                });
            }
        }
    }

    // Sparser than a single spanning tree (n-1 edges), or — for large graphs —
    // below ~1.2 average prerequisite degree, is not a real questline.
    let too_sparse = (n >= 8 && total_edges + 1 < n)
        || (n >= 20 && (total_edges as f64) < 1.2 * n as f64);
    if too_sparse {
        out.push(QuestIssue::TooSparse {
            quests: n,
            deps: total_edges,
        });
    }

    out
}

/// Three-color DFS over the dep graph. `deps` are prerequisite quest ids; an
/// edge q -> dep means q depends on dep. Any quest that participates in a cycle
/// (including a self-loop) is reported exactly once. Roots are visited in
/// input chapter+quest order for stable issue ordering.
fn detect_cycles(g: &QuestGraph) -> Vec<QuestIssue> {
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    // Adjacency restricted to known quest ids (unknown deps are reported
    // separately as MissingDependency and must not crash the walk).
    let order: Vec<&str> = g
        .chapters
        .iter()
        .flat_map(|c| c.quests.iter().map(|q| q.id.as_str()))
        .collect();
    let known: BTreeSet<&str> = order.iter().copied().collect();

    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for ch in &g.chapters {
        for q in &ch.quests {
            let e = adj.entry(q.id.as_str()).or_default();
            for d in &q.deps {
                if known.contains(d.as_str()) {
                    e.push(d.as_str());
                }
            }
        }
    }

    let empty: Vec<&str> = Vec::new();
    let mut color: HashMap<&str, Color> = order.iter().map(|&id| (id, Color::White)).collect();
    let mut cyclic: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = Vec::new();

    // Iterative DFS so deep dep chains can't blow the call stack. Each work
    // item is (node, next-child-index). State is read/written by index so we
    // never hold a borrow on `work` across a push/pop.
    for &root in &order {
        if color[root] != Color::White {
            continue;
        }
        let mut work: Vec<(&str, usize)> = vec![(root, 0)];
        color.insert(root, Color::Gray);
        stack.push(root);

        while !work.is_empty() {
            let top = work.len() - 1;
            let (node, ci) = work[top];
            let children = adj.get(node).unwrap_or(&empty);
            if ci < children.len() {
                let next = children[ci];
                work[top].1 = ci + 1;
                match color.get(next).copied().unwrap_or(Color::White) {
                    Color::White => {
                        color.insert(next, Color::Gray);
                        stack.push(next);
                        work.push((next, 0));
                    }
                    Color::Gray => {
                        // Back edge: everything on `stack` from `next` to the
                        // current top is part of a cycle.
                        if let Some(pos) = stack.iter().position(|&s| s == next) {
                            for &n in &stack[pos..] {
                                cyclic.insert(n);
                            }
                        }
                    }
                    Color::Black => {}
                }
            } else {
                color.insert(node, Color::Black);
                stack.pop();
                work.pop();
            }
        }
    }

    // Report in input order so issue ordering is stable.
    order
        .iter()
        .filter(|id| cyclic.contains(*id))
        .map(|id| QuestIssue::CyclicDependency {
            quest: id.to_string(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// to_heracles_json
// ---------------------------------------------------------------------------

/// Serialize one `QuestTask` to its verified Heracles `1.20.x` codec shape.
/// Recursive for `Composite`. `serde_json` (no `preserve_order`) sorts object
/// keys, so output stays byte-deterministic.
fn task_to_json(task: &QuestTask) -> serde_json::Value {
    use serde_json::{json, Value};
    match task {
        // `GatherItemTask`: codec key is `item` (id or `#tag`), `amount`.
        QuestTask::Item { id, count } => json!({
            "type": "heracles:item", "item": id, "amount": count
        }),
        // Same task type; `nbt` (SNBT compound) emitted only when supplied.
        QuestTask::GatherItem { item, nbt, count } => {
            let mut o = json!({
                "type": "heracles:item", "item": item, "amount": count
            });
            if let Some(nbt) = nbt {
                o["nbt"] = Value::String(nbt.clone());
            }
            o
        }
        // `KillEntityQuestTask`: `entity` is a RestrictedEntityPredicate
        // OBJECT `{ "type": <id> (required), "nbt"? }` — NOT a bare string.
        // `amount` (int, default 1).
        QuestTask::Kill {
            entity_type,
            nbt,
            count,
        } => {
            let mut entity = json!({ "type": entity_type });
            if let Some(nbt) = nbt {
                entity["nbt"] = Value::String(nbt.clone());
            }
            json!({
                "type": "heracles:kill_entity",
                "entity": entity,
                "amount": count
            })
        }
        QuestTask::Advancement { id } => json!({
            "type": "heracles:advancement", "advancements": [id]
        }),
        // Codec field is `biomes` (id or `#tag`) — NOT `biome`.
        QuestTask::Biome { biome } => json!({
            "type": "heracles:biome", "biomes": biome
        }),
        // Codec field is `to` (and/or `from`) — NOT `dimension`.
        QuestTask::Dimension { dimension } => json!({
            "type": "heracles:changed_dimension", "to": dimension
        }),
        // Codec field is `structures` — NOT `structure`.
        QuestTask::Structure { structure } => json!({
            "type": "heracles:structure", "structures": structure
        }),
        // Codec field is `recipes` (JSON array of ids) — NOT scalar `recipe`.
        QuestTask::Recipe { recipe } => json!({
            "type": "heracles:recipe", "recipes": [recipe]
        }),
        // `CompositeTask`: nested `tasks` array (recursive).
        QuestTask::Composite { tasks } => {
            let inner: Vec<Value> = tasks.iter().map(task_to_json).collect();
            json!({ "type": "heracles:composite", "tasks": inner })
        }
        // `StatTask`: stat id + target.
        QuestTask::Stat { stat, target } => json!({
            "type": "heracles:stat", "stat": stat, "target": target
        }),
        // Heracles has NO combined location task: `heracles:location` takes a
        // single Minecraft LocationPredicate, not {dimension,biome,structure}.
        // Decompose into the source-verified separate tasks; combine with
        // `heracles:composite` when more than one criterion is set.
        QuestTask::Location {
            dimension,
            biome,
            structure,
        } => {
            let mut subs: Vec<Value> = Vec::new();
            if let Some(d) = dimension {
                subs.push(json!({ "type": "heracles:changed_dimension", "to": d }));
            }
            if let Some(b) = biome {
                subs.push(json!({ "type": "heracles:biome", "biomes": b }));
            }
            if let Some(s) = structure {
                subs.push(json!({ "type": "heracles:structure", "structures": s }));
            }
            match subs.len() {
                0 => json!({ "type": "heracles:check" }),
                1 => subs.pop().unwrap(),
                _ => json!({ "type": "heracles:composite", "tasks": subs }),
            }
        }
        // `CheckTask`: codec is `{ "nbt"? }`. Emit with NO nbt (Anvil never
        // invents a predicate); see the IR doc note on unverified semantics.
        QuestTask::Checkmark => json!({ "type": "heracles:check" }),
    }
}

/// Deterministically recompute every chapter's quest (x,y) so the chapter's
/// PRIMARY ROOT lands on the Heracles "open group" camera target.
///
/// WHY (primary-source verified against `Heracles-fabric-1.20.1-1.1.13`):
/// `QuestsWidget.update()` sets the initial camera to
/// `centreOffset = (-(maxX+minX)/2, -(maxY+minY)/2)` where min/max are the
/// quest grid extents padded by a SYMMETRIC ±100px — i.e. exactly the
/// bounding-box centroid of the group's quests. There is no per-quest /
/// per-group "focus" or "is start" field anywhere in the Heracles API
/// (`GroupDisplay` = {id, position}; `QuestDisplay` = per-group x/y only).
/// So the ONLY lever is the coordinates. A model-authored left-to-right
/// layout puts the dependency-free root at the far-left edge (x=0); the
/// centroid then lands deep among the dependent quests and the start quest
/// opens off-screen — the reported "quest not rooted at the start" bug.
///
/// FIX: geometry is not the model's job. Ignore the model's x/y entirely and
/// rebuild a ROOT-CENTERED, BIDIRECTIONAL layered layout from `deps` alone:
/// the primary root sits at the origin; its spanning subtree is split into
/// two depth-balanced halves flowing into -x and +x; every column is
/// vertically centered and the primary root is forced to its column's
/// midpoint. Result: `((minX+maxX)/2,(minY+maxY)/2) ≈ root` for any DAG, so
/// Heracles opens centered on the start quest. Consequence (accepted, and
/// surfaced to the user): some dependency chains read right-to-left — the
/// arrows stay correct (Heracles draws them either direction); the mirrored
/// reading is the price of the start quest being the on-open focus.
///
/// Pure, deterministic (every tie breaks on the stable quest id), infallible
/// (a dependency cycle is reported by `validate_graph`; here it merely falls
/// back to a lex-min root so layout always terminates). Only INTRA-chapter
/// edges shape a chapter — cross-chapter prerequisites are positioned by the
/// `to_heracles_json` incoming-gutter pass and are intentionally ignored here.
pub fn layout_graph(g: &mut QuestGraph) {
    for ch in &mut g.chapters {
        layout_chapter(&mut ch.quests);
    }
}

/// Lay out one chapter's quests in place (see [`layout_graph`]).
fn layout_chapter(quests: &mut [QuestNode]) {
    let n = quests.len();
    if n == 0 {
        return;
    }

    // Stable id universe + intra-chapter dependency edges only.
    let ids: HashSet<&str> = quests.iter().map(|q| q.id.as_str()).collect();
    let mut succ: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut indeg: HashMap<&str, usize> = HashMap::new();
    for q in quests.iter() {
        indeg.entry(q.id.as_str()).or_insert(0);
    }
    for q in quests.iter() {
        for d in &q.deps {
            if ids.contains(d.as_str()) {
                succ.entry(d.as_str()).or_default().push(q.id.as_str());
                *indeg.entry(q.id.as_str()).or_insert(0) += 1;
            }
        }
    }
    for v in succ.values_mut() {
        v.sort_unstable();
    }

    // Roots = intra-indegree-0 quests (a quest whose only deps are
    // cross-chapter is a root HERE). Cycle / pathological input: lex-min id.
    let mut roots: Vec<&str> = quests
        .iter()
        .map(|q| q.id.as_str())
        .filter(|id| indeg.get(id).copied().unwrap_or(0) == 0)
        .collect();
    roots.sort_unstable();
    if roots.is_empty() {
        let mut all: Vec<&str> = ids.iter().copied().collect();
        all.sort_unstable();
        roots.push(all[0]);
    }

    // Forward-reachable closure size over intra edges (BFS).
    let closure = |start: &str| -> usize {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack = vec![start];
        seen.insert(start);
        while let Some(u) = stack.pop() {
            if let Some(cs) = succ.get(u) {
                for &c in cs {
                    if seen.insert(c) {
                        stack.push(c);
                    }
                }
            }
        }
        seen.len()
    };

    // Primary root = largest forward closure, tie-break lex-min id.
    let primary: &str = *roots
        .iter()
        .max_by(|a, b| {
            closure(a)
                .cmp(&closure(b))
                .then_with(|| b.cmp(a)) // larger closure first; lex-min id wins ties
        })
        .unwrap();

    // Longest-path rank (Kahn topo); cycle leftovers get a deterministic
    // trailing rank so layout still terminates.
    let mut rank: HashMap<&str, i64> = HashMap::new();
    {
        let mut deg = indeg.clone();
        let mut queue: Vec<&str> =
            deg.iter().filter(|(_, &d)| d == 0).map(|(&k, _)| k).collect();
        queue.sort_unstable();
        for &r in &queue {
            rank.insert(r, 0);
        }
        let mut i = 0;
        while i < queue.len() {
            let u = queue[i];
            i += 1;
            let ru = rank[u];
            if let Some(cs) = succ.get(u) {
                for &c in cs {
                    let nr = ru + 1;
                    let e = rank.entry(c).or_insert(nr);
                    if nr > *e {
                        *e = nr;
                    }
                    let d = deg.get_mut(c).unwrap();
                    *d -= 1;
                    if *d == 0 {
                        queue.push(c);
                    }
                }
            }
        }
        // Any node still unranked sits on a cycle: park it past the max rank,
        // in lex order, so the function is total and deterministic.
        let maxr = rank.values().copied().max().unwrap_or(0);
        let mut leftover: Vec<&str> =
            ids.iter().copied().filter(|id| !rank.contains_key(id)).collect();
        leftover.sort_unstable();
        for (k, id) in leftover.into_iter().enumerate() {
            rank.insert(id, maxr + 1 + k as i64);
        }
    }

    // Spanning tree from the primary root (BFS, children lex-sorted).
    let mut tree_kids: HashMap<&str, Vec<&str>> = HashMap::new();
    {
        let mut seen: HashSet<&str> = HashSet::new();
        seen.insert(primary);
        let mut q = std::collections::VecDeque::new();
        q.push_back(primary);
        while let Some(u) = q.pop_front() {
            if let Some(cs) = succ.get(u) {
                for &c in cs {
                    if seen.insert(c) {
                        tree_kids.entry(u).or_default().push(c);
                        q.push_back(c);
                    }
                }
            }
        }
    }
    let subtree_size = |root: &str| -> usize { closure(root) };

    // Descend the forced spine (single spanning-child links, no other roots)
    // to the first BRANCH node; that is where we split left/right.
    let other_roots: Vec<&str> =
        roots.iter().copied().filter(|&r| r != primary).collect();
    let mut spine: Vec<&str> = vec![primary];
    let mut pivot = primary;
    if other_roots.is_empty() {
        loop {
            let kids = tree_kids.get(pivot).map(|v| v.as_slice()).unwrap_or(&[]);
            if kids.len() == 1 {
                let only = kids[0];
                spine.push(only);
                pivot = only;
            } else {
                break;
            }
        }
    }

    let mut x: HashMap<&str, f64> = HashMap::new();
    let mut y: HashMap<&str, f64> = HashMap::new();

    // Distribution set: the pivot's spanning children + every other root.
    let mut dist: Vec<&str> = tree_kids
        .get(pivot)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .chain(other_roots.iter().copied())
        .collect();
    dist.sort_unstable();

    if dist.is_empty() {
        // PURE CHAIN: no branch anywhere. Lay the whole path on the x=0 axis
        // and route half DOWN / half UP from the root so the root is the
        // y-centroid (one reentrant arrow at the U-turn — accepted).
        let path = spine; // primary .. pivot, in order
        let k = path.len() as i64 - 1; // last index
        let h = (k + 1) / 2; // down-arm length
        for (i, &id) in path.iter().enumerate() {
            let i = i as i64;
            x.insert(id, 0.0);
            let yy = if i == 0 {
                0
            } else if i <= h {
                -i
            } else {
                i - h
            };
            y.insert(id, yy as f64);
        }
    } else {
        // BIDIRECTIONAL SPLIT. Balance the two arms by DEPTH (Heracles'
        // centroid uses only min/max, so equal max-extent — not equal node
        // count — is what centers the root). Deepest subtree to the
        // currently-shallower side; ties lex by subtree root id.
        let max_rank_in = |root: &str| -> i64 {
            let mut best = rank.get(root).copied().unwrap_or(0);
            let mut stack = vec![root];
            let mut seen: HashSet<&str> = HashSet::new();
            seen.insert(root);
            while let Some(u) = stack.pop() {
                best = best.max(rank.get(u).copied().unwrap_or(0));
                if let Some(cs) = tree_kids.get(u) {
                    for &c in cs {
                        if seen.insert(c) {
                            stack.push(c);
                        }
                    }
                }
            }
            best
        };
        let mut order = dist.clone();
        order.sort_by(|a, b| {
            max_rank_in(b)
                .cmp(&max_rank_in(a))
                .then_with(|| subtree_size(b).cmp(&subtree_size(a)))
                .then_with(|| a.cmp(b))
        });
        let (mut load_l, mut load_r) = (0i64, 0i64);
        // side: -1 => -x arm, +1 => +x arm.
        let mut side: HashMap<&str, i64> = HashMap::new();
        for &sub in &order {
            let depth = max_rank_in(sub);
            let s = if load_l <= load_r { -1 } else { 1 };
            if s < 0 {
                load_l = load_l.max(depth);
            } else {
                load_r = load_r.max(depth);
            }
            // Whole spanning subtree inherits the arm.
            let mut stack = vec![sub];
            let mut seen: HashSet<&str> = HashSet::new();
            seen.insert(sub);
            while let Some(u) = stack.pop() {
                side.insert(u, s);
                if let Some(cs) = tree_kids.get(u) {
                    for &c in cs {
                        if seen.insert(c) {
                            stack.push(c);
                        }
                    }
                }
            }
        }
        // Spine + pivot live on the x=0 axis.
        for &s in &spine {
            x.insert(s, 0.0);
        }
        x.insert(pivot, 0.0);
        // Anything still unsided (DAG cross-edges, cycle nodes): inherit a
        // sided intra-parent if any (lex-first), else the lighter global arm.
        let mut rest: Vec<&str> = ids
            .iter()
            .copied()
            .filter(|id| !x.contains_key(id) && !side.contains_key(id))
            .collect();
        rest.sort_unstable();
        for id in rest {
            let parent_side = quests
                .iter()
                .find(|q| q.id == id)
                .map(|q| {
                    let mut ps: Vec<i64> = q
                        .deps
                        .iter()
                        .filter_map(|d| side.get(d.as_str()).copied())
                        .collect();
                    ps.sort_unstable();
                    ps.first().copied()
                })
                .flatten();
            let s = parent_side.unwrap_or(if load_l <= load_r { -1 } else { 1 });
            side.insert(id, s);
        }
        // x = arm-sign * rank for sided nodes (axis nodes already x=0).
        for &id in &ids {
            if x.contains_key(id) {
                continue;
            }
            let s = side.get(id).copied().unwrap_or(-1);
            x.insert(id, (s * rank.get(id).copied().unwrap_or(0)) as f64);
        }

        // Vertical pass: center every column on 0; force the primary root to
        // its (x=0) column's MIDPOINT so primary.y ≈ centroidY regardless of
        // spine length.
        let mut cols: std::collections::BTreeMap<i64, Vec<&str>> =
            std::collections::BTreeMap::new();
        for &id in &ids {
            let xv = *x.get(id).unwrap() as i64;
            cols.entry(xv).or_default().push(id);
        }
        for (xv, col) in cols.iter_mut() {
            col.sort_by(|a, b| {
                rank.get(a)
                    .copied()
                    .unwrap_or(0)
                    .cmp(&rank.get(b).copied().unwrap_or(0))
                    .then_with(|| a.cmp(b))
            });
            if *xv == 0 {
                // Re-seat the primary root at the column's vertical middle.
                col.retain(|&c| c != primary);
                let mid = col.len() / 2;
                col.insert(mid, primary);
            }
            let len = col.len() as f64;
            for (i, &id) in col.iter().enumerate() {
                y.insert(id, i as f64 - (len - 1.0) / 2.0);
            }
        }
    }

    // Materialize owned-key positions so every borrow of `quests` ends before
    // the mutable commit loop (anything unplaced parks at origin — defensive;
    // the passes above cover every id).
    let final_pos: HashMap<String, (f64, f64)> = ids
        .iter()
        .map(|&id| {
            (
                id.to_string(),
                (
                    x.get(id).copied().unwrap_or(0.0),
                    y.get(id).copied().unwrap_or(0.0),
                ),
            )
        })
        .collect();
    for q in quests.iter_mut() {
        if let Some(&(qx, qy)) = final_pos.get(q.id.as_str()) {
            q.x = qx;
            q.y = qy;
        }
    }
}

/// Deterministic Heracles quest JSON. Returns (relative path, contents) pairs,
/// e.g. ("config/heracles/quests/<hex>.json", "<json>"). One file per quest;
/// the filename (sans `.json`) IS the quest id Heracles uses, and a quest's
/// `dependencies` reference those same ids. A quest's id is the stable hex of
/// `<chapter>:<quest>` so cross-chapter deps resolve to the same value.
///
/// Determinism: serde_json (no `preserve_order` feature) writes object keys in
/// sorted order, and the hex ids are content-stable, so two runs on the same
/// graph are byte-identical — the property the determinism test relies on.
pub fn to_heracles_json(g: &QuestGraph) -> Vec<(String, String)> {
    use serde_json::{json, Map, Value};

    let mut quest_hex: HashMap<&str, String> = HashMap::new();
    // Quest id -> its chapter's group key (its "home" group).
    let mut home_group: HashMap<&str, String> = HashMap::new();
    for ch in &g.chapters {
        let gk = group_key(ch);
        for q in &ch.quests {
            quest_hex.insert(
                q.id.as_str(),
                stable_hex(&format!("{}:{}", ch.id, q.id)),
            );
            home_group.insert(q.id.as_str(), gk.clone());
        }
    }

    // Cross-group prerequisite visibility.
    //
    // Heracles only draws a quest in a group when that quest's own
    // `display.groups` map contains the group key
    // (`QuestsScreen.java:57`: `.filter(... display().groups().containsKey(group))`),
    // and dependency lines are skipped for any child not in the current group
    // (`QuestsWidget.java:242`). There is NO "show cross-group deps" flag. So a
    // prerequisite that lives in a different chapter than the quest depending
    // on it is invisible (the "depends on: X but X isn't shown" bug).
    //
    // Fix: for every dependency edge (dependent D, prereq P) where D and P have
    // DIFFERENT home groups, add D's home group to P's `display.groups`. Only
    // DIRECT prerequisites are co-grouped (no transitive flooding). Intra-
    // chapter edges are skipped (same home group), so single-chapter packs are
    // unaffected. `BTreeSet` keeps the added-group order deterministic.
    let mut extra_groups: HashMap<&str, BTreeSet<String>> = HashMap::new();
    for ch in &g.chapters {
        for q in &ch.quests {
            let Some(d_group) = home_group.get(q.id.as_str()) else {
                continue;
            };
            for dep in &q.deps {
                let dep = dep.as_str();
                // Unknown deps are surfaced by validate_graph; ignore here.
                let Some(p_group) = home_group.get(dep) else {
                    continue;
                };
                if p_group != d_group {
                    extra_groups
                        .entry(dep)
                        .or_default()
                        .insert(d_group.clone());
                }
            }
        }
    }

    let mut out: Vec<(String, String)> = Vec::new();

    // groups.txt drives the Groups sidebar order. Heracles fills its `GROUPS`
    // list from this file line-by-line IN ORDER (`QuestHandler.loadGroups`),
    // then `ClientQuests.groups()` renders the sidebar in that order. Without
    // it the order is whatever quest-load happens to append => the scrambled
    // sidebar bug. Emit the chapter group keys in QuestGraph.chapters order
    // (curator progression order), de-duplicated preserving first occurrence,
    // using the EXACT key each quest uses in `display.groups`.
    {
        let mut seen: Vec<String> = Vec::new();
        for ch in &g.chapters {
            let gk = group_key(ch);
            if !seen.contains(&gk) {
                seen.push(gk);
            }
        }
        // An empty graph would otherwise emit "\n", which commons-io
        // `readLines` parses as one empty-string group entry. Skip the file
        // entirely in that (non-curator) case.
        if !seen.is_empty() {
            let mut contents = seen.join("\n");
            contents.push('\n');
            out.push(("config/heracles/groups.txt".to_string(), contents));
        }
    }

    // Foreign-prerequisite placement (the "wrong first quest" fix).
    //
    // A cross-chapter prerequisite injected into a dependent's group (the
    // `extra_groups` pass) USED to reuse the prereq's own (x,y). Every
    // chapter's intended entry is placed by the model at the chapter origin
    // (~0,0), so a Chapter-I root pulled into Chapter II landed exactly on
    // Chapter II's native entry — two quests at (0,0) — and Heracles anchored
    // the group on the FOREIGN quest, showing it as the chapter's "first".
    // Instead, place each foreign prereq in a dedicated "incoming gutter" two
    // units LEFT of its destination chapter's leftmost native node, staggered
    // vertically, never on a native coordinate. The dependency arrow stays
    // drawn; the chapter's real entry stays the anchor. Deterministic:
    // native-min-x is a pure function of the graph; the y stagger follows the
    // fixed chapter -> quest -> BTreeSet(extra) iteration order.
    let mut min_native_x: HashMap<String, i64> = HashMap::new();
    for ch in &g.chapters {
        let gk = group_key(ch);
        for q in &ch.quests {
            let x = q.x.round() as i64;
            min_native_x
                .entry(gk.clone())
                .and_modify(|m| {
                    if x < *m {
                        *m = x;
                    }
                })
                .or_insert(x);
        }
    }
    let mut gutter_slot: HashMap<String, i64> = HashMap::new();

    for ch in &g.chapters {
        // A chapter maps to a Heracles "group". Key it by the readable title so
        // the in-game group label is sensible (Heracles defaults a group's
        // title to its key when no groups file overrides it).
        let group = group_key(ch);

        for q in &ch.quests {
            let qhex = quest_hex[q.id.as_str()].clone();

            // Deps to nonexistent quests are surfaced by validate_graph; skip
            // them silently here rather than unwrap.
            let deps: Vec<Value> = q
                .deps
                .iter()
                .filter_map(|d| quest_hex.get(d.as_str()).cloned())
                .map(Value::String)
                .collect();

            // tasks/rewards are id-keyed maps; the key is the element id (the
            // Heracles codec injects the map key as the record id, so the
            // value object does not repeat it).
            let mut tasks = Map::new();
            // Slice 2: a recipe-facet node ALWAYS surfaces a Heracles quest
            // with an auto `item` task on the PRIMARY recipe's result (NOT a
            // `recipe` task — Open-Loader-injected-recipe detection is
            // unverified; spec §1.1). Synthesized at emit time only — NEVER
            // stored in `node.tasks` (keeps `anvil-quests.json` round-trip
            // exact and the editor honest). The curator schema forbids
            // author-supplied `tasks` on a recipe node, but be defensive: if
            // any are present we still force-add this one (its `recipe_result`
            // key can never collide with an author `task:i` key).
            if let Some(result_id) = primary_recipe_result(q) {
                let thex =
                    stable_hex(&format!("{}:{}:recipe_result", ch.id, q.id));
                tasks.insert(
                    thex,
                    task_to_json(&QuestTask::Item {
                        id: result_id,
                        count: 1,
                    }),
                );
            }
            // Slice 3: a content node ALWAYS surfaces a Heracles quest with an
            // auto `GatherItem` on the boss's unique NBT token (the real,
            // auto-detected objective — NEVER a checkmark, never a kill on a
            // fabricated id). Synthesized at emit time only — never stored in
            // `node.content` (keeps `anvil-quests.json` round-trip exact). The
            // curator schema forbids author `tasks` on a content node; be
            // defensive anyway — the `content_token` key never collides with
            // an author `task:i` or the `recipe_result` key.
            if let Some(spec) = q.content.as_ref() {
                let thex =
                    stable_hex(&format!("{}:{}:content_token", ch.id, q.id));
                tasks.insert(
                    thex,
                    task_to_json(&crate::content::surfaced_task(
                        &ch.id, &q.id, spec,
                    )),
                );
            }
            for (ti, task) in q.tasks.iter().enumerate() {
                let thex = stable_hex(&format!("{}:{}:task:{}", ch.id, q.id, ti));
                tasks.insert(thex, task_to_json(task));
            }

            let mut rewards = Map::new();
            for (ri, reward) in q.rewards.iter().enumerate() {
                let rhex =
                    stable_hex(&format!("{}:{}:reward:{}", ch.id, q.id, ri));
                let v = match reward {
                    // `ItemReward.item` is an ItemStackCodec: object key is
                    // `id` (NOT `item`), with `count`. (A bare id string also
                    // decodes, but the explicit object preserves `count`.)
                    QuestReward::Item { id, count } => json!({
                        "type": "heracles:item",
                        "item": { "id": id, "count": count }
                    }),
                    QuestReward::Xp { amount } => json!({
                        "type": "heracles:xp", "amount": amount
                    }),
                    QuestReward::Command { command } => json!({
                        "type": "heracles:command", "command": command
                    }),
                };
                rewards.insert(rhex, v);
            }

            // Multi-line descriptions become a string list (one per line);
            // empty stays an empty list so no blank line renders in-game.
            let description: Vec<Value> = if q.description.trim().is_empty() {
                Vec::new()
            } else {
                q.description
                    .split('\n')
                    .map(|l| Value::String(l.to_string()))
                    .collect()
            };

            let mut groups = Map::new();
            let pos = json!({
                "x": q.x.round() as i64,
                "y": q.y.round() as i64,
            });
            groups.insert(
                group.clone(),
                json!({ "id": group, "position": pos }),
            );
            // Also place this quest in the home group of any quest that
            // directly depends on it, so the prerequisite is visible to the
            // dependent. We reuse the quest's own (x,y) for the added group
            // (acceptable per spec; may overlap other nodes there). serde_json
            // sorts object keys (no `preserve_order`), and `extra_groups`
            // values are a BTreeSet, so output stays byte-deterministic.
            if let Some(extra) = extra_groups.get(q.id.as_str()) {
                for eg in extra {
                    // Invariant: a quest's own home group is never inserted
                    // into its `extra_groups` (the `p_group != d_group` guard
                    // above guarantees it). Place this foreign prereq in the
                    // destination chapter's left "incoming gutter" — never on a
                    // native coordinate — so the chapter's real entry stays the
                    // anchor (fixes the wrong-first-quest collision).
                    debug_assert_ne!(eg, &group);
                    let gx = min_native_x.get(eg).copied().unwrap_or(0) - 2;
                    let slot = {
                        let s = gutter_slot.entry(eg.clone()).or_insert(0);
                        let v = *s;
                        *s += 1;
                        v
                    };
                    let gpos = json!({ "x": gx, "y": slot * 2 });
                    groups
                        .entry(eg.clone())
                        .or_insert_with(|| json!({ "id": eg, "position": gpos }));
                }
            }

            // VERIFIED against Heracles 1.20.x QuestSettings source: the
            // `hidden` field is a QuestDisplayStatus that DEFAULTS to LOCKED,
            // which hides every quest until its dependencies are met — so a
            // fresh world shows only each chapter's single root quest (the
            // reported "one quest per chapter"). `dependencies_visible` (the
            // serialized name is the full "quest.heracles.<lower>" string)
            // reveals the whole chapter tree with locked quests greyed —
            // the All-the-Mods-style web the design intends.
            let quest = json!({
                "dependencies": deps,
                "tasks": Value::Object(tasks),
                "rewards": Value::Object(rewards),
                "settings": {
                    "hidden": "quest.heracles.dependencies_visible"
                },
                "display": {
                    "title": q.title,
                    "description": description,
                    "groups": Value::Object(groups),
                }
            });

            let mut s = serde_json::to_string_pretty(&quest)
                .unwrap_or_else(|_| "{}".to_string());
            s.push('\n');
            out.push((format!("config/heracles/quests/{}.json", qhex), s));
        }
    }

    out
}

// ---------------------------------------------------------------------------
// write_quests / load_graph
// ---------------------------------------------------------------------------

/// Write the graph as JSON (source of truth), the Heracles quest JSON, AND —
/// when ANY node carries a `recipes` facet — the Open Loader recipe datapack,
/// all into the instance directory. ONE graph compiles to BOTH game artifacts;
/// `<instance>/anvil-quests.json` (now carrying recipe facets) is the only
/// source of truth — there is no `anvil-recipes.json`.
///
/// Anvil pins MC 1.20.1 (the recipe dir is the plural `recipes/`; see
/// `recipe::to_openloader_files`), so the version is the fixed `"1.20.1"`.
/// If the graph has ZERO recipe-bearing nodes the `config/openloader/` tree is
/// NOT created at all (no stray empty datapack).
pub fn write_quests(g: &QuestGraph, instance_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(instance_dir)
        .with_context(|| format!("creating instance dir {}", instance_dir.display()))?;

    // Recompute a deterministic root-centered layout BEFORE anything is
    // written, so the source-of-truth JSON, the Heracles quest files, and the
    // in-app viewer (which reads anvil-quests.json x/y directly) all agree and
    // Heracles' bbox-centroid camera opens on each chapter's start quest.
    let mut laid = g.clone();
    layout_graph(&mut laid);
    let g = &laid;

    // JSON source of truth.
    let json = serde_json::to_string_pretty(g).context("serializing quest graph to JSON")?;
    let json_path = instance_dir.join("anvil-quests.json");
    std::fs::write(&json_path, json)
        .with_context(|| format!("writing {}", json_path.display()))?;

    // Generated Heracles quest JSON.
    for (rel, contents) in to_heracles_json(g) {
        let path = instance_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating dir {}", parent.display()))?;
        }
        std::fs::write(&path, contents)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    // Slice 2: aggregate every node's recipes (derived `anvil:<hex>` ids
    // stamped) and write the Open Loader datapack. Skip the whole emit — and
    // the `config/openloader/` dir — when there are no recipe facets, so a
    // pure-quest pack is byte-for-byte unchanged from pre-Slice-2.
    let set = collect_recipe_set(g);
    if !set.recipes.is_empty() {
        for (rel, contents) in
            crate::recipe::to_openloader_files(&set, "1.20.1")
        {
            let path = instance_dir.join(&rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("creating dir {}", parent.display())
                })?;
            }
            std::fs::write(&path, contents)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }

    // Slice 3: emit the Anvil content datapack (a SIBLING of the recipe
    // datapack, its own pack.mcmeta + root). Skipped entirely — and the
    // `config/openloader/data/anvil-content/` dir never created — when there
    // are no content facets, so a pure quest/recipe pack is byte-for-byte
    // unchanged from pre-Slice-3.
    if any_content(g) {
        for (rel, contents) in
            crate::content::to_openloader_files(g, "1.20.1")
        {
            let path = instance_dir.join(&rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("creating dir {}", parent.display())
                })?;
            }
            std::fs::write(&path, contents)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }

    Ok(())
}

/// Load the persisted graph JSON for an instance, if any.
/// Accumulate submitted chapters into the persisted graph (incremental quest
/// build). A submitted chapter REPLACES an existing one matched by EITHER the
/// same id OR the same non-empty normalized title; otherwise it is appended.
/// Title-identity (not just the model-supplied id) is load-bearing: during a
/// retry storm the model re-emits the same logical chapter under a fresh id
/// (e.g. `ch2_combat` then `ch2_warriors`, both "Chapter II: ..."). Keying
/// replacement on id alone appended a duplicate chapter the player then saw
/// twice in Heracles — and the Heracles group key IS the title, so two
/// same-title chapters are the same group regardless of id. Empty-title
/// chapters fall back to id-only matching (no false collapse). Pure +
/// deterministic; unit-tested.
pub fn merge_chapters(into: &mut Vec<QuestChapter>, submitted: Vec<QuestChapter>) {
    fn norm(s: &str) -> String {
        s.trim().to_lowercase()
    }
    for ch in submitted {
        let m = into.iter_mut().find(|c| {
            c.id == ch.id
                || (!ch.title.trim().is_empty()
                    && norm(&c.title) == norm(&ch.title))
        });
        match m {
            Some(existing) => *existing = ch,
            None => into.push(ch),
        }
    }
}

pub fn load_graph(instance_dir: &Path) -> Option<QuestGraph> {
    let path = instance_dir.join("anvil-quests.json");
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn node(id: &str, deps: &[&str]) -> QuestNode {
        QuestNode {
            id: id.to_string(),
            title: format!("Quest {}", id),
            description: String::new(),
            x: 0.0,
            y: 0.0,
            deps: deps.iter().map(|s| s.to_string()).collect(),
            tasks: Vec::new(),
            rewards: Vec::new(),
            recipes: Vec::new(),
            content: None,
        }
    }

    fn graph_with(quests: Vec<QuestNode>) -> QuestGraph {
        QuestGraph {
            title: "Test Pack".to_string(),
            chapters: vec![QuestChapter {
                id: "intro".to_string(),
                title: "Intro".to_string(),
                quests,
            }],
        }
    }

    // ---- Slice 1: concrete-id grounding ----

    /// A concrete (has_vocab) index where `cobblemon` is pinned-and-SCANNED
    /// (its real registry contains exactly `cobblemon:poke_ball`), so a
    /// fabricated `cobblemon:mewtwo` must HARD-fail; plus a `notpinned` mod
    /// recorded as `unscanned` (jar absent) so its ids degrade to
    /// low-confidence, never a hard fail.
    fn concrete_idx() -> AllowedIndex {
        let mut v = crate::registry::RegistryVocab::default();
        v.entities.insert("cobblemon:poke_ball_entity".to_string());
        v.items.insert("cobblemon:poke_ball".to_string());
        v.items.insert("minecraft:diamond".to_string());
        v.tags.insert("cobblemon:pokeballs".to_string());
        v.structures.insert("cobblemon:pokecenter".to_string());
        // Recipe ids keep their sub-path slashes (scan does `ns:stem`).
        v.recipe_ids.insert("cobblemon:crafting/poke_ball".to_string());
        // mod_meta makes is_empty() false even if a set were empty.
        v.mod_meta.push(crate::registry::ModMeta {
            id: "cobblemon".to_string(),
            name: "Cobblemon".to_string(),
            categories: vec![],
        });
        let mut idx = AllowedIndex {
            vocab: v,
            has_vocab: true,
            ..Default::default()
        };
        for ns in ["minecraft", "cobblemon", "anvil"] {
            idx.items.insert(ns.to_string());
            idx.entities.insert(ns.to_string());
            idx.advancements.insert(ns.to_string());
        }
        idx.unscanned.insert("notpinned".to_string());
        // Mirror build_index_for_instance: vanilla content lives in the
        // bundled (never-scanned) client jar, so `minecraft` is
        // unscanned-by-construction -> vanilla refs degrade, never hard-fail.
        idx.unscanned.insert("minecraft".to_string());
        idx
    }

    #[test]
    fn concrete_grounding_rejects_fabricated_id_accepts_real() {
        let mut q = node("a", &[]);
        q.tasks = vec![
            // Real id present in the scanned registry -> OK.
            QuestTask::Item {
                id: "cobblemon:poke_ball".to_string(),
                count: 1,
            },
            // The user's exact bug: well-formed, namespace IS pinned AND
            // scanned, but the entity does NOT exist -> HARD UnknownEntity.
            QuestTask::Kill {
                entity_type: "cobblemon:mewtwo".to_string(),
                nbt: None,
                count: 1,
            },
            // Vanilla real id -> OK.
            QuestTask::Item {
                id: "minecraft:diamond".to_string(),
                count: 1,
            },
        ];
        let issues = validate_graph(&graph_with(vec![q]), &concrete_idx());

        assert!(
            issues.contains(&QuestIssue::UnknownEntity {
                quest: "a".to_string(),
                id: "cobblemon:mewtwo".to_string(),
            }),
            "fabricated cobblemon:mewtwo must hard-fail; got {issues:?}"
        );
        // The real ids are neither Unknown nor LowConfidence.
        assert!(!issues.iter().any(|i| matches!(
            i,
            QuestIssue::UnknownItem { id, .. }
                | QuestIssue::LowConfidenceId { id, .. }
            if id == "cobblemon:poke_ball" || id == "minecraft:diamond"
        )));
    }

    #[test]
    fn unscanned_mod_id_is_low_confidence_not_hard_fail() {
        let mut q = node("a", &[]);
        // `notpinned` is in `unscanned` (its jar was not on disk). A bogus-
        // looking id there must be accepted LOW-CONFIDENCE, never Unknown.
        q.tasks = vec![QuestTask::Item {
            id: "notpinned:mystery_gizmo".to_string(),
            count: 1,
        }];
        let issues = validate_graph(&graph_with(vec![q]), &concrete_idx());
        assert!(
            issues.contains(&QuestIssue::LowConfidenceId {
                quest: "a".to_string(),
                id: "notpinned:mystery_gizmo".to_string(),
                reason: "unscanned".to_string(),
            }),
            "unscanned-mod id must be low-confidence; got {issues:?}"
        );
        assert!(
            !issues.iter().any(|i| matches!(
                i,
                QuestIssue::UnknownItem { .. } | QuestIssue::UnknownEntity { .. }
            )),
            "unscanned id must NOT be a hard fail"
        );
    }

    #[test]
    fn anvil_authored_id_always_grounds() {
        let mut q = node("a", &[]);
        q.tasks = vec![QuestTask::Item {
            id: "anvil:1A2B3C".to_string(),
            count: 1,
        }];
        q.rewards = vec![QuestReward::Item {
            id: "explicit:token".to_string(),
            count: 1,
        }];
        let idx = concrete_idx()
            .with_authored(["explicit:token".to_string()]);
        let issues = validate_graph(&graph_with(vec![q]), &idx);
        assert!(
            !issues.iter().any(|i| matches!(
                i,
                QuestIssue::UnknownItem { .. }
                    | QuestIssue::LowConfidenceId { .. }
            )),
            "anvil-namespace + injected authored ids must fully ground; got {issues:?}"
        );
    }

    #[test]
    fn namespace_fallback_mode_is_low_confidence_never_blocks() {
        // build_index -> has_vocab=false: a known-namespace id is accepted
        // (low-confidence, "namespace-only"), an unknown namespace hard-fails
        // — exactly the pre-Slice-1 leniency, just labelled.
        let mut q = node("a", &[]);
        q.tasks = vec![
            QuestTask::Item {
                id: "createmod:gear".to_string(),
                count: 1,
            },
            QuestTask::Item {
                id: "bogusmod:nope".to_string(),
                count: 1,
            },
        ];
        let idx = build_index(&["createmod".to_string()]);
        assert!(!idx.has_vocab);
        let issues = validate_graph(&graph_with(vec![q]), &idx);
        assert!(issues.contains(&QuestIssue::LowConfidenceId {
            quest: "a".to_string(),
            id: "createmod:gear".to_string(),
            reason: "namespace-only".to_string(),
        }));
        assert!(issues.contains(&QuestIssue::UnknownItem {
            quest: "a".to_string(),
            id: "bogusmod:nope".to_string(),
        }));
    }

    #[test]
    fn build_index_for_instance_degrades_when_jars_absent() {
        use crate::instance::{Instance, PinnedMod};
        let dir = tempdir().unwrap();
        let inst = Instance {
            id: "i".to_string(),
            name: "I".to_string(),
            mc_version: "1.20.1".to_string(),
            loader: "fabric".to_string(),
            loader_version: "x".to_string(),
            created: "now".to_string(),
            last_played: None,
            mods: vec![PinnedMod {
                project_id: "p".to_string(),
                version_id: "v".to_string(),
                name: "Cobblemon".to_string(),
                path: "mods/cobblemon-1.0.jar".to_string(),
                sha1: "s1".to_string(),
                sha512: "s5".to_string(),
                download_url: "u".to_string(),
                file_size: 1,
            }],
        };
        // No jar on disk -> degrade: namespace-fallback mode, `cobblemon`
        // recorded as unscanned, NEVER an error.
        let idx = build_index_for_instance(&inst, dir.path(), Vec::<String>::new());
        assert!(!idx.has_vocab, "no jars -> namespace-fallback");
        assert!(idx.unscanned.contains("cobblemon"));
        // A cobblemon id is accepted low-confidence (unscanned), not Unknown.
        let mut q = node("a", &[]);
        q.tasks = vec![QuestTask::Kill {
            entity_type: "cobblemon:mewtwo".to_string(),
            nbt: None,
            count: 1,
        }];
        let issues = validate_graph(&graph_with(vec![q]), &idx);
        assert!(
            issues.iter().any(|i| matches!(
                i,
                QuestIssue::LowConfidenceId { id, .. } if id == "cobblemon:mewtwo"
            )),
            "jar-absent cobblemon id must be low-confidence; got {issues:?}"
        );
        assert!(!issues
            .iter()
            .any(|i| matches!(i, QuestIssue::UnknownEntity { .. })));
    }

    #[test]
    fn vanilla_ids_never_hard_fail_in_concrete_mode() {
        // minecraft.jar is bundled, never pinned/scanned -> vanilla ids are
        // NOT in the vocab. They must degrade to non-blocking LowConfidence
        // ("unscanned"), NEVER a hard Unknown, even when has_vocab is true.
        let mut idx = concrete_idx();
        idx.vocab.biomes.clear(); // ensure no vanilla biome is in the vocab
        let mut q = node("a", &[]);
        q.tasks = vec![
            QuestTask::Biome {
                biome: "minecraft:soul_sand_valley".to_string(),
            },
            QuestTask::Structure {
                structure: "minecraft:fortress".to_string(),
            },
            QuestTask::Item {
                id: "minecraft:nether_star".to_string(),
            count: 1,
            },
        ];
        let issues = validate_graph(&graph_with(vec![q]), &idx);
        assert!(
            !issues.iter().any(|i| matches!(
                i,
                QuestIssue::UnknownItem { .. } | QuestIssue::UnknownEntity { .. }
            )),
            "vanilla ids must NOT hard-fail; got {issues:?}"
        );
        // They are reported low-confidence ("unscanned"), so they are honest.
        assert!(issues.iter().any(|i| matches!(
            i,
            QuestIssue::LowConfidenceId { id, reason, .. }
                if id == "minecraft:fortress" && reason == "unscanned"
        )));
    }

    #[test]
    fn tag_ref_grounds_against_vocab_tags() {
        let idx = concrete_idx();
        let mut q = node("a", &[]);
        q.tasks = vec![
            // Real tag present in vocab.tags -> OK (no issue).
            QuestTask::Kill {
                entity_type: "#cobblemon:pokeballs".to_string(),
                nbt: None,
                count: 1,
            },
            // Bogus tag, namespace pinned-and-scanned, NOT in vocab.tags
            // -> hard UnknownEntity (the fabricated-tag case).
            QuestTask::Kill {
                entity_type: "#cobblemon:legendaries".to_string(),
                nbt: None,
                count: 1,
            },
        ];
        let issues = validate_graph(&graph_with(vec![q]), &idx);
        assert!(
            !issues.iter().any(|i| matches!(
                i,
                QuestIssue::UnknownEntity { id, .. } if id == "#cobblemon:pokeballs"
            )),
            "real tag must ground OK; got {issues:?}"
        );
        assert!(
            issues.contains(&QuestIssue::UnknownEntity {
                quest: "a".to_string(),
                id: "#cobblemon:legendaries".to_string(),
            }),
            "fabricated tag must hard-fail; got {issues:?}"
        );
    }

    #[test]
    fn recipe_id_with_slashes_grounds_concretely() {
        let idx = concrete_idx();
        let mut q = node("a", &[]);
        q.tasks = vec![
            // Exact scanned recipe id (sub-path with slash) -> OK.
            QuestTask::Recipe {
                recipe: "cobblemon:crafting/poke_ball".to_string(),
            },
            // Same namespace, fabricated recipe path -> hard fail.
            QuestTask::Recipe {
                recipe: "cobblemon:crafting/master_ball".to_string(),
            },
        ];
        let issues = validate_graph(&graph_with(vec![q]), &idx);
        assert!(
            !issues.iter().any(|i| matches!(
                i,
                QuestIssue::UnknownItem { id, .. }
                    if id == "cobblemon:crafting/poke_ball"
            )),
            "real slash-path recipe id must ground OK; got {issues:?}"
        );
        assert!(
            issues.contains(&QuestIssue::UnknownItem {
                quest: "a".to_string(),
                id: "cobblemon:crafting/master_ball".to_string(),
            }),
            "fabricated recipe id must hard-fail; got {issues:?}"
        );
    }

    #[test]
    fn missing_dependency_detected() {
        let g = graph_with(vec![node("a", &["ghost"])]);
        let idx = build_index(&[]);
        let issues = validate_graph(&g, &idx);
        assert!(issues.contains(&QuestIssue::MissingDependency {
            quest: "a".to_string(),
            dep: "ghost".to_string(),
        }));
    }

    #[test]
    fn cycle_detected() {
        // a -> b -> a
        let g = graph_with(vec![node("a", &["b"]), node("b", &["a"])]);
        let idx = build_index(&[]);
        let issues = validate_graph(&g, &idx);
        assert!(issues.contains(&QuestIssue::CyclicDependency {
            quest: "a".to_string()
        }));
        assert!(issues.contains(&QuestIssue::CyclicDependency {
            quest: "b".to_string()
        }));
        // No false missing-dependency since both ids exist.
        assert!(!issues
            .iter()
            .any(|i| matches!(i, QuestIssue::MissingDependency { .. })));
    }

    #[test]
    fn self_loop_is_cyclic() {
        let g = graph_with(vec![node("a", &["a"])]);
        let idx = build_index(&[]);
        let issues = validate_graph(&g, &idx);
        assert!(issues.contains(&QuestIssue::CyclicDependency {
            quest: "a".to_string()
        }));
    }

    #[test]
    fn unknown_namespace_flagged_allowed_not() {
        let mut q = node("a", &[]);
        q.tasks = vec![
            QuestTask::Item {
                id: "minecraft:diamond".to_string(),
                count: 1,
            },
            QuestTask::Item {
                id: "bogusmod:unobtainium".to_string(),
                count: 1,
            },
        ];
        q.rewards = vec![QuestReward::Item {
            id: "createmod:andesite_alloy".to_string(),
            count: 4,
        }];
        let g = graph_with(vec![q]);
        // Only `createmod` is an allowed mod namespace (plus vanilla).
        let idx = build_index(&["createmod".to_string()]);
        let issues = validate_graph(&g, &idx);

        // The hallucinated namespace is flagged...
        assert!(issues.contains(&QuestIssue::UnknownItem {
            quest: "a".to_string(),
            id: "bogusmod:unobtainium".to_string(),
        }));
        // ...vanilla and the allowed mod namespace are not.
        assert!(!issues.iter().any(|i| matches!(
            i,
            QuestIssue::UnknownItem { id, .. } if id == "minecraft:diamond"
        )));
        assert!(!issues.iter().any(|i| matches!(
            i,
            QuestIssue::UnknownItem { id, .. } if id == "createmod:andesite_alloy"
        )));
    }

    #[test]
    fn heracles_json_is_deterministic_and_well_formed() {
        let mut q = node("start", &[]);
        q.title = "He said \"hi\"".to_string(); // exercise JSON escaping
        q.description = "line1\nline2".to_string();
        q.tasks = vec![
            QuestTask::Item {
                id: "minecraft:stone".to_string(),
                count: 64,
            },
            QuestTask::Kill {
                entity_type: "minecraft:zombie".to_string(),
                nbt: None,
                count: 10,
            },
            QuestTask::Advancement {
                id: "minecraft:story/mine_stone".to_string(),
            },
            QuestTask::Biome {
                biome: "minecraft:soul_sand_valley".to_string(),
            },
            QuestTask::Dimension {
                dimension: "minecraft:the_nether".to_string(),
            },
            QuestTask::Structure {
                structure: "minecraft:fortress".to_string(),
            },
            QuestTask::Recipe {
                recipe: "create:crushing/andesite".to_string(),
            },
            QuestTask::Checkmark,
        ];
        q.rewards = vec![
            QuestReward::Xp { amount: 100 },
            QuestReward::Command {
                command: "/give @p minecraft:cake".to_string(),
            },
            QuestReward::Item {
                id: "minecraft:diamond".to_string(),
                count: 3,
            },
        ];
        let g = graph_with(vec![q]);

        let a = to_heracles_json(&g);
        let b = to_heracles_json(&g);
        // Byte-identical across two calls on the same graph.
        assert_eq!(a, b);

        // One file per quest, named by the stable hex of "<chapter>:<quest>".
        let qhex = stable_hex("intro:start");
        assert_eq!(qhex.len(), 16);
        let file = a
            .iter()
            .find(|(p, _)| p == &format!("config/heracles/quests/{}.json", qhex))
            .expect("quest file emitted at its hex path");

        // The contents are valid JSON matching the Heracles quest shape.
        let v: serde_json::Value =
            serde_json::from_str(&file.1).expect("quest json parses");
        assert_eq!(v["display"]["title"], "He said \"hi\"");
        // Multi-line description becomes a string list.
        assert_eq!(v["display"]["description"][0], "line1");
        assert_eq!(v["display"]["description"][1], "line2");
        // Source-verified: Heracles QuestSettings.hidden defaults to LOCKED
        // (only roots show until deps met). We emit dependencies_visible so
        // the whole chapter tree is visible (locked quests greyed). The
        // serialized enum value is the full "quest.heracles.<lower>" string.
        assert_eq!(
            v["settings"]["hidden"], "quest.heracles.dependencies_visible",
            "every quest must reveal the full tree, not hide until unlocked"
        );
        // Tasks are an id-keyed map carrying the heracles:* type tags.
        let tasks = v["tasks"].as_object().expect("tasks object");
        assert_eq!(tasks.len(), 8);
        let types: Vec<&str> = tasks
            .values()
            .filter_map(|t| t["type"].as_str())
            .collect();
        for t in [
            "heracles:item",
            "heracles:kill_entity",
            "heracles:advancement",
            "heracles:biome",
            "heracles:changed_dimension",
            "heracles:structure",
            "heracles:recipe",
            "heracles:check",
        ] {
            assert!(types.contains(&t), "missing task type {t}");
        }

        // ---- Codec-shape regression: the 6 fixed field bugs ----
        let by_type = |ty: &str| -> serde_json::Value {
            tasks
                .values()
                .find(|t| t["type"] == ty)
                .cloned()
                .unwrap_or_else(|| panic!("no task {ty}"))
        };
        // 1. kill_entity: `entity` is a RestrictedEntityPredicate object with
        //    a required `type`, NOT a bare string.
        let kill = by_type("heracles:kill_entity");
        assert!(kill["entity"].is_object(), "kill entity must be an object");
        assert_eq!(kill["entity"]["type"], "minecraft:zombie");
        assert!(kill["entity"].get("nbt").is_none(), "no nbt when None");
        assert_eq!(kill["amount"], 10);
        // 2. changed_dimension: `to`, not `dimension`.
        let dim = by_type("heracles:changed_dimension");
        assert_eq!(dim["to"], "minecraft:the_nether");
        assert!(dim.get("dimension").is_none());
        // 3. biome: `biomes`, not `biome`.
        let biome = by_type("heracles:biome");
        assert_eq!(biome["biomes"], "minecraft:soul_sand_valley");
        assert!(biome.get("biome").is_none());
        // 4. structure: `structures`, not `structure`.
        let st = by_type("heracles:structure");
        assert_eq!(st["structures"], "minecraft:fortress");
        assert!(st.get("structure").is_none());
        // 5. recipe: `recipes` array, not scalar `recipe`.
        let rec = by_type("heracles:recipe");
        assert_eq!(
            rec["recipes"],
            serde_json::json!(["create:crushing/andesite"])
        );
        assert!(rec.get("recipe").is_none());
        // 6. reward heracles:item: ItemStackCodec key is `id`, not `item`.
        let rewards = v["rewards"].as_object().expect("rewards object");
        let item_reward = rewards
            .values()
            .find(|r| r["type"] == "heracles:item")
            .expect("item reward present");
        assert_eq!(item_reward["item"]["id"], "minecraft:diamond");
        assert_eq!(item_reward["item"]["count"], 3);
        assert!(
            item_reward["item"].get("item").is_none(),
            "old wrong inner `item` key must be gone"
        );

        // The quest is placed in its chapter's group with a position.
        let groups = v["display"]["groups"].as_object().expect("groups");
        assert!(groups.contains_key("Intro"));
    }

    /// Backward compat: a quest graph saved by the OLD code (kill task with a
    /// bare `entity` string) must still decode and serialize. `#[serde(alias =
    /// "entity")]` on `entity_type` is what makes this work.
    #[test]
    fn old_kill_shape_decodes_and_emits_predicate_object() {
        let t: QuestTask = serde_json::from_str(
            r#"{"type":"kill","entity":"minecraft:zombie","count":10}"#,
        )
        .expect("old kill JSON decodes via alias");
        match &t {
            QuestTask::Kill {
                entity_type,
                nbt,
                count,
            } => {
                assert_eq!(entity_type, "minecraft:zombie");
                assert_eq!(*nbt, None);
                assert_eq!(*count, 10);
            }
            other => panic!("expected Kill, got {other:?}"),
        }
        // And it serializes to the corrected predicate-object shape.
        let v = task_to_json(&t);
        assert_eq!(v["type"], "heracles:kill_entity");
        assert_eq!(v["entity"]["type"], "minecraft:zombie");
        assert_eq!(v["amount"], 10);

        // A whole old-shape graph round-trips through write/load unchanged.
        let mut q = node("k", &[]);
        q.tasks = vec![t];
        let g = graph_with(vec![q]);
        let dir = tempdir().expect("tempdir");
        write_quests(&g, dir.path()).expect("write_quests");
        let loaded = load_graph(dir.path()).expect("load_graph");
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::to_value(&loaded).unwrap()
        );
    }

    /// The new task variants serialize to their verified `heracles:*` shapes.
    #[test]
    fn new_task_variants_emit_correct_shapes() {
        // GatherItem with nbt -> heracles:item + nbt + amount.
        let gi = task_to_json(&QuestTask::GatherItem {
            item: "minecraft:diamond_sword".to_string(),
            nbt: Some("{display:{Name:'Excalibur'}}".to_string()),
            count: 1,
        });
        assert_eq!(gi["type"], "heracles:item");
        assert_eq!(gi["item"], "minecraft:diamond_sword");
        assert_eq!(gi["nbt"], "{display:{Name:'Excalibur'}}");
        assert_eq!(gi["amount"], 1);

        // GatherItem without nbt omits the key entirely.
        let gi2 = task_to_json(&QuestTask::GatherItem {
            item: "minecraft:apple".to_string(),
            nbt: None,
            count: 5,
        });
        assert!(gi2.get("nbt").is_none());

        // Kill with an nbt discriminator -> predicate object carries nbt.
        let k = task_to_json(&QuestTask::Kill {
            entity_type: "minecraft:zombie".to_string(),
            nbt: Some("{IsBaby:1b}".to_string()),
            count: 3,
        });
        assert_eq!(k["entity"]["type"], "minecraft:zombie");
        assert_eq!(k["entity"]["nbt"], "{IsBaby:1b}");

        // Composite nests serialized child tasks.
        let c = task_to_json(&QuestTask::Composite {
            tasks: vec![
                QuestTask::Item {
                    id: "minecraft:stone".to_string(),
                    count: 1,
                },
                QuestTask::Checkmark,
            ],
        });
        assert_eq!(c["type"], "heracles:composite");
        let inner = c["tasks"].as_array().expect("composite tasks array");
        assert_eq!(inner.len(), 2);
        assert_eq!(inner[0]["type"], "heracles:item");
        assert_eq!(inner[1]["type"], "heracles:check");

        // Stat.
        let s = task_to_json(&QuestTask::Stat {
            stat: "minecraft:jump".to_string(),
            target: 100,
        });
        assert_eq!(s["type"], "heracles:stat");
        assert_eq!(s["stat"], "minecraft:jump");
        assert_eq!(s["target"], 100);

        // Location: Heracles has no combined task, so a single criterion
        // emits the source-verified task directly...
        let loc = task_to_json(&QuestTask::Location {
            dimension: Some("minecraft:the_end".to_string()),
            biome: None,
            structure: None,
        });
        assert_eq!(loc["type"], "heracles:changed_dimension");
        assert_eq!(loc["to"], "minecraft:the_end");
        // ...and multiple criteria compose the verified sub-tasks.
        let loc2 = task_to_json(&QuestTask::Location {
            dimension: None,
            biome: Some("minecraft:plains".to_string()),
            structure: Some("minecraft:village".to_string()),
        });
        assert_eq!(loc2["type"], "heracles:composite");
        let subs = loc2["tasks"].as_array().expect("composite tasks");
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0]["type"], "heracles:biome");
        assert_eq!(subs[0]["biomes"], "minecraft:plains");
        assert_eq!(subs[1]["type"], "heracles:structure");
        assert_eq!(subs[1]["structures"], "minecraft:village");

        // Checkmark: heracles:check with NO nbt (Anvil never invents one).
        let chk = task_to_json(&QuestTask::Checkmark);
        assert_eq!(chk["type"], "heracles:check");
        assert!(chk.get("nbt").is_none());
    }

    #[test]
    fn sparse_graph_rejected_connected_ok() {
        let idx = build_index(&[]);

        // 8 quests, zero dependencies: orphan islands + too sparse.
        let sparse =
            graph_with((0..8).map(|i| node(&format!("q{i}"), &[])).collect());
        let issues = validate_graph(&sparse, &idx);
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, QuestIssue::TooSparse { .. })),
            "expected TooSparse, got {issues:?}"
        );
        assert!(
            issues
                .iter()
                .filter(|i| matches!(i, QuestIssue::OrphanQuest { .. }))
                .count()
                >= 6
        );

        // Same size but a connected chain q0<-q1<-...<-q7: no quality issues.
        let mut quests: Vec<QuestNode> =
            (0..8).map(|i| node(&format!("q{i}"), &[])).collect();
        for i in 1..8 {
            quests[i].deps = vec![format!("q{}", i - 1)];
        }
        let chain = graph_with(quests);
        let bad: Vec<_> = validate_graph(&chain, &idx)
            .into_iter()
            .filter(|i| {
                matches!(
                    i,
                    QuestIssue::TooSparse { .. }
                        | QuestIssue::OrphanQuest { .. }
                        | QuestIssue::DisconnectedChapter { .. }
                )
            })
            .collect();
        assert!(bad.is_empty(), "connected chain flagged: {bad:?}");
    }

    #[test]
    fn chapter_with_no_cross_edge_is_flagged() {
        let g = QuestGraph {
            title: "X".to_string(),
            chapters: vec![
                QuestChapter {
                    id: "a".to_string(),
                    title: "A".to_string(),
                    quests: vec![node("a1", &[]), node("a2", &["a1"])],
                },
                QuestChapter {
                    id: "b".to_string(),
                    title: "B".to_string(),
                    quests: vec![node("b1", &[]), node("b2", &["b1"])],
                },
            ],
        };
        let issues = validate_graph(&g, &build_index(&[]));
        assert!(issues.contains(&QuestIssue::DisconnectedChapter {
            chapter: "b".to_string()
        }));
    }

    #[test]
    fn cross_chapter_dep_hex_resolves() {
        // q2 lives in ch2 but depends on q1 which lives in ch1; the emitted
        // dependency must be the hex of "ch1:q1", not "ch2:q1".
        let g = QuestGraph {
            title: "X".to_string(),
            chapters: vec![
                QuestChapter {
                    id: "ch1".to_string(),
                    title: "One".to_string(),
                    quests: vec![node("q1", &[])],
                },
                QuestChapter {
                    id: "ch2".to_string(),
                    title: "Two".to_string(),
                    quests: vec![node("q2", &["q1"])],
                },
            ],
        };
        let files = to_heracles_json(&g);
        let q2_hex = stable_hex("ch2:q2");
        let q2 = files
            .iter()
            .find(|(p, _)| p == &format!("config/heracles/quests/{}.json", q2_hex))
            .expect("q2 file");
        let v: serde_json::Value =
            serde_json::from_str(&q2.1).expect("q2 json parses");
        let expected = stable_hex("ch1:q1");
        assert_eq!(
            v["dependencies"],
            serde_json::json!([expected]),
            "q2 should depend on q1's cross-chapter hex; got:\n{}",
            q2.1
        );
    }

    /// Real-artifact regression (layer 2 + 3): loads the ACTUAL on-disk
    /// `anvil-quests.json` from the "Stellar Archetypes" pack the user hit, runs
    /// the real `to_heracles_json`, parses the emitted Heracles quest files, and
    /// asserts NO two distinct quests share an (x,y) within the same group.
    /// Pre-fix this fails: a Chapter-I root injected into Chapter II via
    /// `extra_groups` reuses its own (0,0), colliding with Chapter II's native
    /// (0,0) entry — so Heracles shows the foreign quest as the chapter's first.
    #[test]
    fn real_stellar_archetypes_no_intragroup_position_collisions() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/real/stellar_archetypes.anvil-quests.json"
        );
        let raw = std::fs::read_to_string(path)
            .expect("real Stellar Archetypes fixture present");
        let g: QuestGraph = serde_json::from_str(&raw)
            .expect("real anvil-quests.json deserializes via the same serde path");
        let files = to_heracles_json(&g);

        // group key -> ((x,y) -> distinct quest hexes placed there)
        let mut by_group: HashMap<String, HashMap<(i64, i64), BTreeSet<String>>> =
            HashMap::new();
        for (p, content) in &files {
            let Some(hex) = p
                .strip_prefix("config/heracles/quests/")
                .and_then(|s| s.strip_suffix(".json"))
            else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(content) else {
                continue;
            };
            let Some(groups) = v
                .get("display")
                .and_then(|d| d.get("groups"))
                .and_then(|x| x.as_object())
            else {
                continue;
            };
            for (gk, gv) in groups {
                let pos = gv.get("position").cloned().unwrap_or_default();
                let x = pos.get("x").and_then(|n| n.as_i64()).unwrap_or(0);
                let y = pos.get("y").and_then(|n| n.as_i64()).unwrap_or(0);
                by_group
                    .entry(gk.clone())
                    .or_default()
                    .entry((x, y))
                    .or_default()
                    .insert(hex.to_string());
            }
        }

        let mut collisions = Vec::new();
        for (gk, posmap) in &by_group {
            for ((x, y), hexes) in posmap {
                if hexes.len() > 1 {
                    collisions.push(format!(
                        "group {gk:?}: {} quests at ({x},{y}) -> {hexes:?}",
                        hexes.len()
                    ));
                }
            }
        }
        collisions.sort();
        assert!(
            collisions.is_empty(),
            "intra-group position collisions (the wrong-first-quest bug) in the \
             REAL Stellar Archetypes pack:\n{}",
            collisions.join("\n")
        );
    }

    /// Real-artifact regression (Phase 1B): the actual Stellar Archetypes
    /// graph the user hit has `q1_dungeons` using `adventuring_time` (T5) and
    /// `q1_netherite` using `netherite_armor` (T4) in Chapter I (cap T1). The
    /// difficulty gate MUST flag both.
    #[test]
    fn real_stellar_archetypes_flags_overhard_chapter1() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/real/stellar_archetypes.anvil-quests.json"
        );
        let g: QuestGraph =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap())
                .expect("real fixture parses");
        let issues = check_difficulty(&g);
        assert!(
            issues.iter().any(|i| matches!(
                i,
                QuestIssue::OverdifficultForChapter { quest, chapter_index, task, task_tier, max_allowed }
                if quest == "q1_dungeons" && *chapter_index == 0
                    && task.contains("adventuring_time")
                    && *task_tier == 5 && *max_allowed == 1
            )),
            "must flag q1_dungeons adventuring_time T5 in Chapter I; got {issues:#?}"
        );
        assert!(
            issues.iter().any(|i| matches!(
                i,
                QuestIssue::OverdifficultForChapter { quest, chapter_index, .. }
                if quest == "q1_netherite" && *chapter_index == 0
            )),
            "must flag q1_netherite (netherite_armor T4) in Chapter I; got {issues:#?}"
        );
    }

    #[test]
    fn difficulty_tiers_and_chapter_ceiling() {
        assert_eq!(advancement_tier("minecraft:story/sleep_in_bed"), 1);
        assert_eq!(advancement_tier("minecraft:adventure/adventuring_time"), 5);
        assert_eq!(advancement_tier("minecraft:nether/netherite_armor"), 4);
        assert_eq!(advancement_tier("create:andesite_alloy"), 3); // modded -> mid
        assert_eq!(advancement_tier("minecraft:adventure/some_future_adv"), 3);
        assert_eq!(task_tier(&QuestTask::Checkmark), 1);
        assert_eq!(
            task_tier(&QuestTask::Advancement {
                id: "minecraft:adventure/adventuring_time".into()
            }),
            5
        );
        // chapter ceiling: ch0=T1, ch1=T2, last forced T5
        assert_eq!(chapter_max_tier(0, 6), 1);
        assert_eq!(chapter_max_tier(1, 6), 2);
        assert_eq!(chapter_max_tier(3, 6), 4); // non-last: min(idx+1,5)
        assert_eq!(chapter_max_tier(5, 6), 5); // last forced 5
        assert_eq!(chapter_max_tier(3, 4), 5); // idx 3 of 4 = last -> forced 5
    }

    /// Gap #11: a retry re-emitting the same logical chapter under a NEW id
    /// must REPLACE it (same title), not append a visible duplicate.
    #[test]
    fn merge_chapters_dedups_by_title_not_just_model_id() {
        fn ch(id: &str, title: &str) -> QuestChapter {
            QuestChapter {
                id: id.into(),
                title: title.into(),
                quests: vec![],
            }
        }
        let mut into = vec![ch("ch2_combat", "Chapter II: Warriors of the Surface")];
        // The exact #11 shape: same chapter, fresh id during a retry.
        merge_chapters(
            &mut into,
            vec![ch("ch2_warriors", "Chapter II: Warriors of the Surface")],
        );
        assert_eq!(into.len(), 1, "same-title chapter under a new id must replace");
        assert_eq!(into[0].id, "ch2_warriors", "replaced in place");
        // id-match still replaces (rename path)
        merge_chapters(&mut into, vec![ch("ch2_warriors", "Renamed II")]);
        assert_eq!(into.len(), 1);
        assert_eq!(into[0].title, "Renamed II");
        // genuinely new chapter appends
        merge_chapters(&mut into, vec![ch("ch3", "Chapter III")]);
        assert_eq!(into.len(), 2);
        // empty titles must NOT collapse together (id-only fallback)
        let mut e = vec![ch("a", "")];
        merge_chapters(&mut e, vec![ch("b", "")]);
        assert_eq!(e.len(), 2, "empty-title chapters match by id only");
    }

    fn quest_json<'a>(
        files: &'a [(String, String)],
        chapter: &str,
        quest: &str,
    ) -> serde_json::Value {
        let qhex = stable_hex(&format!("{chapter}:{quest}"));
        let f = files
            .iter()
            .find(|(p, _)| p == &format!("config/heracles/quests/{}.json", qhex))
            .unwrap_or_else(|| panic!("quest file for {chapter}:{quest}"));
        serde_json::from_str(&f.1).expect("quest json parses")
    }

    #[test]
    fn groups_txt_emitted_in_progression_order_and_deduped() {
        // Chapter titles in curator progression order, with a duplicate title
        // ("Gyms") that must de-dupe preserving first occurrence.
        let g = QuestGraph {
            title: "Pack".to_string(),
            chapters: vec![
                QuestChapter {
                    id: "intro".to_string(),
                    title: "A New Journey Begins".to_string(),
                    quests: vec![node("q1", &[])],
                },
                QuestChapter {
                    id: "gym1".to_string(),
                    title: "Gyms".to_string(),
                    quests: vec![node("q2", &["q1"])],
                },
                QuestChapter {
                    id: "gym2".to_string(),
                    title: "Gyms".to_string(),
                    quests: vec![node("q3", &["q2"])],
                },
                QuestChapter {
                    id: "elite".to_string(),
                    title: "The Elite Four".to_string(),
                    quests: vec![node("q4", &["q3"])],
                },
            ],
        };
        let files = to_heracles_json(&g);
        let (_, contents) = files
            .iter()
            .find(|(p, _)| p == "config/heracles/groups.txt")
            .expect("groups.txt emitted");
        // Sidebar order = exact line order = chapter (progression) order,
        // duplicate "Gyms" collapsed to its first occurrence.
        assert_eq!(
            contents,
            "A New Journey Begins\nGyms\nThe Elite Four\n"
        );
        // Determinism: identical across runs.
        assert_eq!(to_heracles_json(&g), files);
    }

    #[test]
    fn cross_chapter_dep_co_groups_prereq() {
        // ch1:q1  <-- ch2:q2  (q2 depends on q1, different chapters/groups).
        let g = QuestGraph {
            title: "X".to_string(),
            chapters: vec![
                QuestChapter {
                    id: "ch1".to_string(),
                    title: "One".to_string(),
                    quests: vec![node("q1", &[])],
                },
                QuestChapter {
                    id: "ch2".to_string(),
                    title: "Two".to_string(),
                    quests: vec![node("q2", &["q1"])],
                },
            ],
        };
        let files = to_heracles_json(&g);

        // The prerequisite q1 must now appear in BOTH its home group "One"
        // (so it still renders in its own chapter) AND "Two" (so it's visible
        // to q2, which is the bug-2 fix).
        let q1 = quest_json(&files, "ch1", "q1");
        let q1_groups = q1["display"]["groups"].as_object().expect("q1 groups");
        assert!(q1_groups.contains_key("One"), "q1 keeps its home group");
        assert!(
            q1_groups.contains_key("Two"),
            "q1 co-grouped into dependent's group; got {q1_groups:?}"
        );
        // The added group reuses q1's own position record.
        assert_eq!(q1_groups["Two"]["id"], "Two");
        assert!(q1_groups["Two"]["position"]["x"].is_i64());

        // The dependent q2 is only in its own group (it depends on nothing
        // cross-group, so nothing co-groups INTO it here).
        let q2 = quest_json(&files, "ch2", "q2");
        let q2_groups = q2["display"]["groups"].as_object().expect("q2 groups");
        assert_eq!(q2_groups.len(), 1);
        assert!(q2_groups.contains_key("Two"));
    }

    #[test]
    fn intra_chapter_dep_does_not_add_groups() {
        // Single chapter: q2 depends on q1. Same home group => no co-grouping,
        // each quest stays in exactly its one group.
        let g = graph_with(vec![node("q1", &[]), node("q2", &["q1"])]);
        let files = to_heracles_json(&g);

        let q1 = quest_json(&files, "intro", "q1");
        let q1_groups = q1["display"]["groups"].as_object().expect("q1 groups");
        assert_eq!(q1_groups.len(), 1, "intra-chapter dep adds no extra group");
        assert!(q1_groups.contains_key("Intro"));

        let q2 = quest_json(&files, "intro", "q2");
        assert_eq!(
            q2["display"]["groups"].as_object().unwrap().len(),
            1
        );

        // Determinism holds with the co-grouping pass present.
        assert_eq!(to_heracles_json(&g), files);
    }

    #[test]
    fn write_then_load_roundtrip() {
        let mut q = node("q1", &[]);
        q.tasks = vec![QuestTask::Item {
            id: "minecraft:apple".to_string(),
            count: 3,
        }];
        let g = QuestGraph {
            title: "Roundtrip".to_string(),
            chapters: vec![
                QuestChapter {
                    id: "ch1".to_string(),
                    title: "Chapter One".to_string(),
                    quests: vec![q],
                },
                QuestChapter {
                    id: "ch2".to_string(),
                    title: "Chapter Two".to_string(),
                    quests: vec![node("q2", &["q1"])],
                },
            ],
        };

        let dir = tempdir().expect("tempdir");
        write_quests(&g, dir.path()).expect("write_quests");

        // The Heracles quest JSON was written to disk too.
        assert!(dir
            .path()
            .join(format!(
                "config/heracles/quests/{}.json",
                stable_hex("ch1:q1")
            ))
            .exists());

        let loaded = load_graph(dir.path()).expect("load_graph");
        // Compare via JSON (types intentionally don't derive PartialEq).
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::to_value(&loaded).unwrap()
        );
    }

    #[test]
    fn load_graph_missing_is_none() {
        let dir = tempdir().expect("tempdir");
        assert!(load_graph(dir.path()).is_none());
    }

    // ---- Slice 2: recipes as a quest-graph node facet ----

    use crate::recipe::{Ingredient, ItemStack, RecipeDef, RecipeKind};

    /// A node carrying a shaped recipe (Create alloy + vanilla diamond ->
    /// Thermal frame, the worked curator example), no author-supplied tasks.
    fn recipe_node() -> QuestNode {
        let mut key = std::collections::BTreeMap::new();
        key.insert(
            "A".to_string(),
            Ingredient::Item {
                item: "create:andesite_alloy".to_string(),
            },
        );
        key.insert(
            "D".to_string(),
            Ingredient::Item {
                item: "minecraft:diamond".to_string(),
            },
        );
        let mut q = node("thermal_gate", &[]);
        q.recipes = vec![RecipeDef {
            id: String::new(), // curator never supplies one — derived at emit
            kind: RecipeKind::Shaped {
                pattern: vec![
                    "AAA".to_string(),
                    "ADA".to_string(),
                    "AAA".to_string(),
                ],
                key,
                result: ItemStack {
                    item: "thermal:machine_frame".to_string(),
                    count: 1,
                },
            },
        }];
        q
    }

    #[test]
    fn recipe_facet_round_trips_and_emits_both_artifacts() {
        let g = graph_with(vec![recipe_node()]);
        let dir = tempdir().expect("tempdir");
        write_quests(&g, dir.path()).expect("write_quests");

        // (a) anvil-quests.json round-trips byte-for-byte through serde —
        // the recipe facet decodes back identically (no author-supplied id).
        let loaded = load_graph(dir.path()).expect("load_graph");
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::to_value(&loaded).unwrap()
        );
        // No anvil-recipes.json is EVER written (the silo is gone).
        assert!(!dir.path().join("anvil-recipes.json").exists());

        // (b) the Heracles quest file has an AUTO item task on the recipe
        // result (NOT a recipe task) — synthesized at emit, never stored.
        let qhex = stable_hex("intro:thermal_gate");
        let qpath =
            dir.path().join(format!("config/heracles/quests/{qhex}.json"));
        assert!(qpath.exists(), "heracles quest file emitted");
        let qv: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&qpath).unwrap(),
        )
        .unwrap();
        let tasks = qv["tasks"].as_object().expect("tasks object");
        assert_eq!(tasks.len(), 1, "exactly the auto item-on-result task");
        let t = tasks.values().next().unwrap();
        assert_eq!(t["type"], "heracles:item");
        assert_eq!(t["item"], "thermal:machine_frame");
        assert_eq!(t["amount"], 1);
        // The synthesized task is NOT persisted in the graph JSON.
        assert!(loaded.chapters[0].quests[0].tasks.is_empty());

        // (c) the Open Loader datapack: pack.mcmeta + the derived-hex recipe
        // file at data/<ns>/recipes/<hex>.json, shaped result {item,count}.
        let mcmeta = dir
            .path()
            .join("config/openloader/data/anvil-recipes/pack.mcmeta");
        assert!(mcmeta.exists(), "pack.mcmeta emitted");
        let mv: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&mcmeta).unwrap(),
        )
        .unwrap();
        assert_eq!(mv["pack"]["pack_format"], 15);

        let derived = derived_recipe_id("intro", "thermal_gate", 0);
        let (_, hex) = derived.split_once(':').unwrap();
        let rpath = dir.path().join(format!(
            "config/openloader/data/anvil-recipes/data/anvil/recipes/{hex}.json"
        ));
        assert!(rpath.exists(), "recipe file at derived-hex path: {derived}");
        let rv: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&rpath).unwrap(),
        )
        .unwrap();
        assert_eq!(rv["type"], "minecraft:crafting_shaped");
        assert!(rv["result"].is_object(), "shaped result is {{item,count}}");
        assert_eq!(rv["result"]["item"], "thermal:machine_frame");
        assert_eq!(rv["result"]["count"], 1);
    }

    #[test]
    fn smelting_recipe_facet_result_is_bare_string() {
        let mut q = node("smelt_gate", &[]);
        q.recipes = vec![RecipeDef {
            id: String::new(),
            kind: RecipeKind::Smelting {
                ingredient: Ingredient::Item {
                    item: "create:raw_zinc".to_string(),
                },
                result: "create:zinc_ingot".to_string(),
                experience: 0.7,
                cookingtime: 200,
            },
        }];
        let g = graph_with(vec![q]);
        let dir = tempdir().expect("tempdir");
        write_quests(&g, dir.path()).expect("write_quests");

        let derived = derived_recipe_id("intro", "smelt_gate", 0);
        let (_, hex) = derived.split_once(':').unwrap();
        let rpath = dir.path().join(format!(
            "config/openloader/data/anvil-recipes/data/anvil/recipes/{hex}.json"
        ));
        let rv: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&rpath).unwrap(),
        )
        .unwrap();
        assert_eq!(rv["type"], "minecraft:smelting");
        assert!(
            rv["result"].is_string(),
            "1.20.1 smelting result MUST be a bare string"
        );
        assert_eq!(rv["result"], "create:zinc_ingot");
        // The auto item-on-result task uses the smelting result string.
        let qhex = stable_hex("intro:smelt_gate");
        let qv: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                dir.path().join(format!("config/heracles/quests/{qhex}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        let t = qv["tasks"].as_object().unwrap().values().next().unwrap();
        assert_eq!(t["item"], "create:zinc_ingot");
    }

    #[test]
    fn no_recipe_nodes_writes_no_openloader_dir() {
        let g = graph_with(vec![node("plain", &[])]);
        let dir = tempdir().expect("tempdir");
        write_quests(&g, dir.path()).expect("write_quests");
        // The whole config/openloader tree must NOT exist.
        assert!(
            !dir.path().join("config/openloader").exists(),
            "no recipe facets => no openloader datapack at all"
        );
        // But the Heracles quests path still exists (unchanged behaviour).
        assert!(dir.path().join("config/heracles").exists());
    }

    #[test]
    fn write_quests_is_byte_deterministic_including_datapack() {
        // Two recipe nodes across two chapters so the datapack has >1 file.
        let g = QuestGraph {
            title: "Det".to_string(),
            chapters: vec![
                QuestChapter {
                    id: "c1".to_string(),
                    title: "C1".to_string(),
                    quests: vec![recipe_node()],
                },
                QuestChapter {
                    id: "c2".to_string(),
                    title: "C2".to_string(),
                    quests: vec![{
                        let mut q = node("q2", &["thermal_gate"]);
                        q.recipes = vec![RecipeDef {
                            id: String::new(),
                            kind: RecipeKind::Shapeless {
                                ingredients: vec![Ingredient::Item {
                                    item: "create:brass_ingot".to_string(),
                                }],
                                result: ItemStack {
                                    item: "thermal:redstone_servo".to_string(),
                                    count: 2,
                                },
                            },
                        }];
                        q
                    }],
                },
            ],
        };
        let d1 = tempdir().expect("tempdir");
        let d2 = tempdir().expect("tempdir");
        write_quests(&g, d1.path()).expect("write 1");
        write_quests(&g, d2.path()).expect("write 2");

        // Walk both trees and assert every relative file is byte-identical,
        // INCLUDING config/openloader/** (the datapack), per spec.
        fn walk(base: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
            fn go(
                base: &Path,
                cur: &Path,
                out: &mut std::collections::BTreeMap<String, Vec<u8>>,
            ) {
                if let Ok(rd) = std::fs::read_dir(cur) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            go(base, &p, out);
                        } else {
                            let rel = p
                                .strip_prefix(base)
                                .unwrap()
                                .to_string_lossy()
                                .replace('\\', "/");
                            out.insert(
                                rel,
                                std::fs::read(&p).unwrap(),
                            );
                        }
                    }
                }
            }
            let mut out = std::collections::BTreeMap::new();
            go(base, base, &mut out);
            out
        }
        let a = walk(d1.path());
        let b = walk(d2.path());
        assert_eq!(a, b, "two write_quests runs must be byte-identical");
        // Sanity: the datapack actually participated.
        assert!(a
            .keys()
            .any(|k| k.starts_with("config/openloader/data/anvil-recipes/")));
    }

    /// `concrete_idx` (above) with `thermal` ADDED as a scanned mod so a
    /// fabricated `thermal:*` id hard-fails, plus the derived recipe ids
    /// injected into the authored allowlist (mirrors `tool_generate_quests`).
    #[test]
    fn validate_graph_grounds_recipe_ids_and_flags_orphans_on_final_only() {
        // A recipe whose ingredient namespace IS pinned-and-scanned
        // (cobblemon) but the exact id does NOT exist -> hard UnknownItem,
        // surfaced through the quest channel (the cobblemon:mewtwo class).
        let mut key = std::collections::BTreeMap::new();
        key.insert(
            "X".to_string(),
            Ingredient::Item {
                item: "cobblemon:does_not_exist".to_string(),
            },
        );
        let mut bad = node("bad", &[]);
        bad.recipes = vec![RecipeDef {
            id: String::new(),
            kind: RecipeKind::Shaped {
                pattern: vec!["X".to_string()],
                key,
                result: ItemStack {
                    item: "cobblemon:poke_ball".to_string(),
                    count: 1,
                },
            },
        }];
        let g = graph_with(vec![bad]);
        // Authored allowlist seeded with the derived ids exactly as the
        // curator does, so the item-on-result task self-grounds.
        let idx = concrete_idx()
            .with_authored(authored_recipe_ids(&g));
        let issues = validate_graph(&g, &idx);
        let derived = derived_recipe_id("intro", "bad", 0);
        assert!(
            issues.contains(&QuestIssue::UnknownItem {
                quest: derived.clone(),
                id: "cobblemon:does_not_exist".to_string(),
            }),
            "fabricated recipe ingredient must hard-fail through the quest \
             channel; got {issues:?}"
        );
        // The auto item-on-result task is on cobblemon:poke_ball, a REAL
        // scanned id -> it must NOT be flagged (and the derived recipe id,
        // being anvil-authored, never appears as Unknown).
        assert!(
            !issues.iter().any(|i| matches!(
                i,
                QuestIssue::UnknownItem { id, .. }
                    if id == "cobblemon:poke_ball" || id == &derived
            )),
            "real result + anvil-authored derived id must ground; got {issues:?}"
        );

        // Orphan-recipe quality gate: 3 pure-vanilla recipes (no modded ns
        // either side). FINAL-only + size>=3 leniency lives in the recipe
        // engine; validate_graph always passes is_final=true so the curator
        // can filter — here we assert the RecipeQuality variant appears.
        let mk = |id: &str, res: &str| {
            let mut q = node(id, &[]);
            q.recipes = vec![RecipeDef {
                id: String::new(),
                kind: RecipeKind::Shapeless {
                    ingredients: vec![Ingredient::Item {
                        item: "minecraft:dirt".to_string(),
                    }],
                    result: ItemStack {
                        item: res.to_string(),
                        count: 1,
                    },
                },
            }];
            q
        };
        let orphans = graph_with(vec![
            mk("o1", "minecraft:coarse_dirt"),
            mk("o2", "minecraft:mud"),
            mk("o3", "minecraft:clay"),
        ]);
        let idx2 = concrete_idx()
            .with_authored(authored_recipe_ids(&orphans));
        let qi = validate_graph(&orphans, &idx2);
        let orphan_count = qi
            .iter()
            .filter(|i| matches!(
                i,
                QuestIssue::RecipeQuality { detail, .. }
                    if detail == "orphan_recipe"
            ))
            .count();
        assert_eq!(orphan_count, 3, "all 3 pure-vanilla are orphans; got {qi:?}");
        // And the set-wide no-modded-output quality issue fires too.
        assert!(qi.iter().any(|i| matches!(
            i,
            QuestIssue::RecipeQuality { detail, .. }
                if detail == "set_has_no_mod_output"
        )));

        // Size leniency: under 3 recipes, NO orphan quality issue.
        let small = graph_with(vec![mk("s1", "minecraft:clay")]);
        let idx3 = concrete_idx()
            .with_authored(authored_recipe_ids(&small));
        assert!(
            !validate_graph(&small, &idx3).iter().any(|i| matches!(
                i,
                QuestIssue::RecipeQuality { .. }
            )),
            "quality gate skipped under 3 recipes"
        );
    }

    // ---- Slice 3: content provisioning (loot-token boss) ----

    use crate::content::{BossAttributes, ContentSpec, Equipment, Trigger};

    /// A node carrying a content boss facet (the worked curator example:
    /// Chapter-8 climax, Eternax / Void Heart, totem trigger).
    fn boss_node(id: &str) -> QuestNode {
        let mut q = node(id, &[]);
        q.content = Some(ContentSpec::Boss {
            entity: "minecraft:wither_skeleton".to_string(),
            display_name: "Eternax, the Void Sovereign".to_string(),
            attributes: BossAttributes {
                max_health: Some(400.0),
                ..Default::default()
            },
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

    #[test]
    fn content_boss_round_trips_and_emits_datapack_and_token_task() {
        let g = graph_with(vec![boss_node("climax")]);
        let dir = tempdir().expect("tempdir");
        write_quests(&g, dir.path()).expect("write_quests");

        // (a) anvil-quests.json round-trips byte-for-byte through serde —
        // the content facet decodes back identically.
        let loaded = load_graph(dir.path()).expect("load_graph");
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::to_value(&loaded).unwrap()
        );
        // The synthesized token task is NOT persisted in the graph JSON.
        assert!(loaded.chapters[0].quests[0].tasks.is_empty());

        // (b) the Heracles quest's ONLY task is a GatherItem on the token,
        // NBT-matched (heracles:item + item + nbt) — NOT a checkmark, NOT a
        // kill_entity on a fabricated id.
        let qhex = stable_hex("intro:climax");
        let qpath =
            dir.path().join(format!("config/heracles/quests/{qhex}.json"));
        let qv: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&qpath).unwrap(),
        )
        .unwrap();
        let tasks = qv["tasks"].as_object().expect("tasks object");
        assert_eq!(tasks.len(), 1, "exactly the auto token-obtain task");
        let t = tasks.values().next().unwrap();
        assert_eq!(t["type"], "heracles:item");
        assert_eq!(t["item"], "minecraft:nether_star");
        let chex = crate::content::content_hex("intro", "climax");
        assert_eq!(t["nbt"], format!("{{anvil_token:\"{chex}\"}}"));
        assert!(t.get("type").is_some_and(|v| v != "heracles:check"));

        // (c) the content datapack: pack.mcmeta + summon/tick/onkill fns +
        // kill-advancement + totem recipe, at the derived hex paths, in the
        // SIBLING anvil-content root (not anvil-recipes).
        let root = "config/openloader/data/anvil-content";
        let mcmeta = dir.path().join(format!("{root}/pack.mcmeta"));
        assert!(mcmeta.exists(), "content pack.mcmeta emitted");
        let mv: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&mcmeta).unwrap(),
        )
        .unwrap();
        assert_eq!(mv["pack"]["pack_format"], 15);
        for rel in [
            format!("{root}/data/anvil/functions/{chex}_summon.mcfunction"),
            format!("{root}/data/anvil/functions/{chex}_tick.mcfunction"),
            format!("{root}/data/anvil/functions/{chex}_onkill.mcfunction"),
            format!("{root}/data/anvil/advancements/{chex}_killed.json"),
            // Altar trigger (no recipe — vanilla Fabric 1.20.1 result has no
            // nbt): a tick scanner + the function it fires.
            format!("{root}/data/anvil/functions/{chex}_altar.mcfunction"),
            format!("{root}/data/anvil/functions/{chex}_altar_fire.mcfunction"),
            format!("{root}/data/minecraft/tags/functions/tick.json"),
        ] {
            assert!(
                dir.path().join(&rel).exists(),
                "content datapack file missing: {rel}"
            );
        }
        // The kill-advancement is the loader-agnostic adv->fn->give path.
        let adv: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(format!(
                "{root}/data/anvil/advancements/{chex}_killed.json"
            )))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            adv["criteria"]["kill"]["trigger"],
            "minecraft:player_killed_entity"
        );
        assert!(
            adv["criteria"]["kill"]["conditions"]["entity"].is_object(),
            "1.20.1 inline entity predicate (object, not 1.20.2 list)"
        );
        assert_eq!(
            adv["rewards"]["function"],
            format!("anvil:{chex}_onkill")
        );
    }

    #[test]
    fn no_content_node_writes_no_content_datapack() {
        let g = graph_with(vec![node("plain", &[])]);
        let dir = tempdir().expect("tempdir");
        write_quests(&g, dir.path()).expect("write_quests");
        assert!(
            !dir.path()
                .join("config/openloader/data/anvil-content")
                .exists(),
            "no content facet => no anvil-content datapack at all"
        );
    }

    #[test]
    fn validate_graph_hard_rejects_fabricated_content_entity() {
        let mut q = boss_node("bad_boss");
        if let Some(ContentSpec::Boss { entity, .. }) = q.content.as_mut() {
            // Namespace IS pinned-and-scanned (cobblemon) but the entity does
            // NOT exist -> hard UnknownItem through the quest channel.
            *entity = "cobblemon:mewtwo".to_string();
        }
        let g = graph_with(vec![q]);
        let idx = concrete_idx().with_authored(authored_content_ids(&g));
        let issues = validate_graph(&g, &idx);
        assert!(
            issues.contains(&QuestIssue::UnknownItem {
                quest: "bad_boss".to_string(),
                id: "cobblemon:mewtwo".to_string(),
            }),
            "fabricated content entity must hard-fail; got {issues:?}"
        );
    }

    #[test]
    fn validate_graph_hard_rejects_partial_content_node_atomicity() {
        // A content node whose required entity is blank: the full atomic set
        // (summon/tick/onkill+token/kill-adv/trigger) cannot be emitted, so
        // it is a HARD ContentIncomplete (token atomicity, design §6 #12) —
        // checked on EVERY call, never filtered.
        let mut q = boss_node("hollow");
        if let Some(ContentSpec::Boss { entity, .. }) = q.content.as_mut() {
            *entity = "  ".to_string();
        }
        let g = graph_with(vec![q]);
        let idx = concrete_idx().with_authored(authored_content_ids(&g));
        let issues = validate_graph(&g, &idx);
        assert!(
            issues
                .iter()
                .any(|i| matches!(
                    i,
                    QuestIssue::ContentIncomplete { node, .. }
                        if node == "hollow"
                )),
            "partial content node must hard-fail ContentIncomplete; got \
             {issues:?}"
        );
        // The reserved `region` trigger is also a hard atomicity failure.
        let mut q2 = boss_node("region_boss");
        if let Some(ContentSpec::Boss { trigger, .. }) = q2.content.as_mut() {
            *trigger = Trigger::Region;
        }
        let g2 = graph_with(vec![q2]);
        let idx2 = concrete_idx().with_authored(authored_content_ids(&g2));
        assert!(
            validate_graph(&g2, &idx2).iter().any(|i| matches!(
                i,
                QuestIssue::ContentIncomplete { node, .. }
                    if node == "region_boss"
            )),
            "reserved region trigger must hard-fail ContentIncomplete"
        );
    }

    #[test]
    fn derived_content_ids_ground_via_authored_allowlist() {
        let g = graph_with(vec![boss_node("climax")]);
        // WITHOUT the authored ids the derived anvil:<hex>_* refs would still
        // ground (anvil ns is authored-by-construction); with them the EXACT
        // ids are explicit. Either way: no Unknown/LowConfidence for them.
        let idx = concrete_idx().with_authored(authored_content_ids(&g));
        let ids = authored_content_ids(&g);
        assert!(
            ids.iter().any(|i| i.ends_with("_summon"))
                && ids.iter().any(|i| i.ends_with("_onkill"))
                && ids.iter().any(|i| i.ends_with("_killed"))
                && ids.iter().any(|i| i.ends_with("_altar")),
            "authored ids must enumerate the full atomic set; got {ids:?}"
        );
        let issues = validate_graph(&g, &idx);
        // The derived anvil:<hex>_* refs never appear as Unknown (they are
        // Anvil-authored) and the boss is structurally complete (no
        // ContentIncomplete). Vanilla base ids (minecraft:wither_skeleton …)
        // are non-blocking LowConfidence "unscanned" by the SAME documented
        // Slice-1 rule as `vanilla_ids_never_hard_fail_in_concrete_mode`
        // (minecraft.jar is bundled, never scanned) — that is honest, not a
        // failure, and never write-blocking.
        assert!(
            !issues.iter().any(|i| matches!(
                i,
                QuestIssue::UnknownItem { .. }
                    | QuestIssue::ContentIncomplete { .. }
            )),
            "a fully-grounded content boss must produce no HARD issues; got \
             {issues:?}"
        );
        // No derived anvil-authored id is ever flagged.
        assert!(
            !issues.iter().any(|i| matches!(
                i,
                QuestIssue::UnknownItem { id, .. }
                    | QuestIssue::LowConfidenceId { id, .. }
                    if id.starts_with("anvil:")
            )),
            "derived anvil:<hex>_* ids must ground via the authored \
             allowlist; got {issues:?}"
        );
    }

    #[test]
    fn write_quests_byte_deterministic_including_content_datapack() {
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
                        q.deps = vec!["b1".to_string()];
                        q
                    }],
                },
            ],
        };
        let d1 = tempdir().expect("tempdir");
        let d2 = tempdir().expect("tempdir");
        write_quests(&g, d1.path()).expect("write 1");
        write_quests(&g, d2.path()).expect("write 2");

        fn walk(base: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
            fn go(
                base: &Path,
                cur: &Path,
                out: &mut std::collections::BTreeMap<String, Vec<u8>>,
            ) {
                if let Ok(rd) = std::fs::read_dir(cur) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            go(base, &p, out);
                        } else {
                            let rel = p
                                .strip_prefix(base)
                                .unwrap()
                                .to_string_lossy()
                                .replace('\\', "/");
                            out.insert(rel, std::fs::read(&p).unwrap());
                        }
                    }
                }
            }
            let mut out = std::collections::BTreeMap::new();
            go(base, base, &mut out);
            out
        }
        let a = walk(d1.path());
        let b = walk(d2.path());
        assert_eq!(a, b, "two write_quests runs must be byte-identical");
        assert!(
            a.keys()
                .any(|k| k.starts_with(
                    "config/openloader/data/anvil-content/"
                )),
            "the content datapack must have participated"
        );
    }
}
