//! AI-authored custom-recipe datapack ENGINE (Open Loader).
//!
//! CONTRACT: this module is a reusable, persistence-FREE engine. It owns the
//! recipe IR (`RecipeDef`/`RecipeKind`/`Ingredient`/`ItemStack`), the
//! deterministic Open Loader serializer (`to_openloader_files`), and the
//! grounding/structural/quality validator (`validate_recipes`). It does NOT
//! own a source-of-truth file: the quest graph
//! (`crate::quest`, `<instance>/anvil-quests.json`) is the single source of
//! truth and a recipe is a first-class quest-node FACET (Slice 2): every
//! `QuestNode` carries `recipes: Vec<RecipeDef>`. `RecipeSet` is a transient
//! in-memory aggregate `crate::quest::write_quests` builds (one entry per
//! node-recipe, its `id` stamped to the DERIVED `anvil:<hex>` value) and hands
//! to this engine; there is no `anvil-recipes.json` read/write — ever. The
//! datapack is written under `<instance>/config/openloader/data/anvil-recipes/`
//! (pack.mcmeta + one file per recipe; the filename is the derived hex) by
//! `crate::quest::write_quests`, not this module.
//!
//! DERIVED ID (Slice 2): a node-embedded recipe is curator-authored WITHOUT an
//! id (the curator schema has no `id` field). `crate::quest` assigns
//! `id = anvil:<stable_hex("{chapter}:{node}:recipe:{i}")>` in place before
//! both validation and serialization, so the datapack id is Anvil-authored,
//! collision-free, and grounded by construction (it is in the Anvil-authored
//! allowlist). This module stays id-agnostic: it serializes/validates whatever
//! `RecipeDef.id` holds; the quest engine owns the derivation.
//!
//! OPEN LOADER SHAPE (verified against Darkhax-Minecraft/Open-Loader, 1.20.1):
//! the datapack ROOT is `config/openloader/data/anvil-recipes/`. The `config/`
//! prefix is MANDATORY on 1.17+ (pre-1.17 used a bare `openloader/data`; we
//! never emit that). Inside the root: a MANDATORY `pack.mcmeta`
//! (`pack_format` 15 for MC 1.20/1.20.1 — Open Loader SILENTLY DROPS a folder
//! that lacks it) and `data/<namespace>/recipes/<name>.json`. 1.20.1 uses the
//! PLURAL `recipes/` directory; 1.21 renamed it to singular `recipe/` — we pin
//! 1.20.1 and emit the plural (see `recipe_dir`).
//!
//! 1.20.1 RECIPE JSON (NOT the current minecraft.wiki, which documents 1.21):
//! - crafting_shaped: `pattern` (<=3 rows, equal length <=3 cols), `key`
//!   mapping each char to an ingredient object, `result` is an OBJECT
//!   `{"item","count"}`.
//! - crafting_shapeless: `ingredients` (1..=9 ingredient objects), `result`
//!   object.
//! - smelting: `ingredient` object, `result` is a PLAIN STRING in 1.20.1
//!   (the object form is only 1.20.5+), plus `experience` (f64) and
//!   `cookingtime` (i64). Ingredients are always objects (`{"item":..}` or
//!   `{"tag":..}`), never bare strings.
//!
//! GROUNDING LIMITATION (identical to quest.rs, verbatim in spirit): a full
//! per-jar registry dump (every concrete item/tag id a modpack actually
//! registers) requires the *running* game — the Forge/Fabric registries only
//! populate at load time. So grounding is at the *namespace* level only
//! (reusing `crate::quest::build_index` / `crate::quest::namespace_of`). A
//! well-formed recipe that references a nonexistent EXACT item id within an
//! allowed namespace (e.g. `create:does_not_exist`) PASSES the gate and then
//! silently fails to load in-game; only the running game can vet full ids.
//! This is the accepted offline correctness seam.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

// `build_index` is reused by callers (curator.rs) and the tests; this module's
// own logic only needs `namespace_of` + the `AllowedIndex` type.
use crate::quest::{namespace_of, AllowedIndex, RecipeGrounding};

/// `pack_format` for a Minecraft 1.20 / 1.20.1 datapack. Open Loader reads
/// this from `pack.mcmeta`; a wrong/missing format makes it skip the folder.
const PACK_FORMAT_1_20: i64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeSet {
    #[serde(default)]
    pub recipes: Vec<RecipeDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeDef {
    /// Namespaced datapack id, e.g. `anvil:1A2B...`. `<ns>` selects the
    /// datapack data folder, `<name>` is the on-disk filename. When a recipe
    /// is a quest-node facet the curator does NOT supply this — it is DERIVED
    /// deterministically from the owning node by `crate::quest` and overwritten
    /// before serialization, so it is Anvil-authored and collision-free by
    /// construction. `#[serde(default)]` so a node-embedded recipe (and any
    /// existing fixture) decodes without an `id`; a bare/empty id falls back to
    /// the `minecraft` namespace exactly like every other id here.
    #[serde(default)]
    pub id: String,
    #[serde(flatten)]
    pub kind: RecipeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecipeKind {
    /// `minecraft:crafting_shaped`.
    Shaped {
        /// Up to 3 rows, each up to 3 cols, all rows equal length.
        pattern: Vec<String>,
        /// Pattern char -> ingredient.
        key: BTreeMap<String, Ingredient>,
        result: ItemStack,
    },
    /// `minecraft:crafting_shapeless`.
    Shapeless {
        /// 1..=9 ingredients.
        ingredients: Vec<Ingredient>,
        result: ItemStack,
    },
    /// `minecraft:smelting` (furnace).
    Smelting {
        ingredient: Ingredient,
        /// 1.20.1: a PLAIN namespaced item string (object form is 1.20.5+).
        result: String,
        #[serde(default)]
        experience: f64,
        #[serde(default = "default_cookingtime")]
        cookingtime: i64,
    },
}

fn default_cookingtime() -> i64 {
    200
}

/// A recipe ingredient: either an exact `item` or an `item tag`. Serializes to
/// `{"item":"ns:id"}` or `{"tag":"ns:id"}` exactly as 1.20.1 expects.
// `untagged` so each variant serializes its body DIRECTLY — `Item{item}` ->
// `{"item":"ns:id"}`, `Tag{tag}` -> `{"tag":"ns:id"}` — exactly the 1.20.1
// ingredient shape, NOT an externally-tagged `{"item":{"item":..}}` wrapper.
// Deserialization is unambiguous because the two variants have disjoint keys.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Ingredient {
    Item { item: String },
    Tag { tag: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemStack {
    pub item: String,
    #[serde(default = "default_count")]
    pub count: i64,
}

fn default_count() -> i64 {
    1
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecipeIssue {
    /// An item/tag/result id that is fabricated: its namespace IS
    /// pinned-and-scanned but the concrete id is absent from the pack's real
    /// registry (the `cobblemon:mewtwo`-class miss, now applied to recipes via
    /// the Slice-1 `AllowedIndex`), or its namespace is not pinned at all.
    /// Hard check, every call.
    UnknownItem { recipe: String, id: String },
    /// An id accepted but UNVERIFIED — its mod jar was not on disk at scan
    /// time (`reason:"unscanned"`) or the index is in namespace-only fallback
    /// (`reason:"namespace-only"`). NOT a write-blocking hard fail; surfaced
    /// so the model/user knows it was not proven, exactly like the quest
    /// engine's `LowConfidenceId`. Filtered out of the blocking set.
    LowConfidenceId {
        recipe: String,
        id: String,
        reason: String,
    },
    /// Two recipes share the same `id`. Hard check, every call.
    DuplicateRecipeId { recipe: String },
    /// Shaped pattern has 0 or >3 rows, unequal row lengths, or >3 columns.
    BadPattern { recipe: String },
    /// A `key` entry whose char never appears in the pattern.
    KeyNotInPattern { recipe: String, key: String },
    /// A non-space pattern char with no `key` binding.
    PatternCharUnbound { recipe: String, ch: String },
    /// Shapeless with 0 or >9 ingredients.
    EmptyIngredients { recipe: String },
    /// QUALITY (final-only): the recipe touches NO pinned-mod (non-minecraft)
    /// namespace on EITHER side — pure vanilla shuffling, pointless in a
    /// modpack. See the rationale on `quality_issues`.
    OrphanRecipe { recipe: String },
    /// QUALITY (final-only): the whole set produces nothing whose RESULT
    /// namespace is a pinned mod, so the datapack adds no modded output at all.
    SetHasNoModOutput,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Split a (possibly) namespaced recipe id into `(namespace, name)`. A bare id
/// defaults to the `minecraft` namespace, matching `quest::namespace_of`.
fn split_id(id: &str) -> (&str, &str) {
    match id.split_once(':') {
        Some((ns, name)) => (ns, name),
        None => ("minecraft", id),
    }
}

/// The namespaced id an ingredient grounds against (the item or the tag).
fn ingredient_id(ing: &Ingredient) -> &str {
    match ing {
        Ingredient::Item { item } => item,
        Ingredient::Tag { tag } => tag,
    }
}

/// 1.20.1 recipe directory is PLURAL `recipes`. NOTE: Minecraft 1.21 renamed
/// this to the SINGULAR `recipe`; Anvil pins 1.20.1 so we always emit the
/// plural here. If/when a 1.21 target is added this must branch on the version.
fn recipe_dir(_mc_version: &str) -> &'static str {
    "recipes"
}

/// True iff `id`'s namespace is a pinned mod (anything other than the vanilla
/// `minecraft` namespace). This is the "touches a modded thing" predicate the
/// quality gate is built on.
fn is_modded(id: &str) -> bool {
    namespace_of(id) != "minecraft"
}

// ---------------------------------------------------------------------------
// to_openloader_files
// ---------------------------------------------------------------------------

/// Deterministic Open Loader datapack files. Returns (relative path, contents)
/// pairs. ALWAYS emits `config/openloader/data/anvil-recipes/pack.mcmeta`
/// (Open Loader silently drops a datapack folder lacking it), then one
/// `config/openloader/data/anvil-recipes/data/<ns>/recipes/<name>.json` per
/// recipe where `(ns, name)` split the namespaced `id`.
///
/// Determinism: serde_json (no `preserve_order` feature) writes object keys in
/// sorted order, and our struct/enum shapes serialize stably, so two runs on
/// the same `RecipeSet` are byte-identical — the property the determinism test
/// relies on (same as quest.rs::to_heracles_json). Every file ends with a
/// trailing newline.
pub fn to_openloader_files(set: &RecipeSet, mc_version: &str) -> Vec<(String, String)> {
    use serde_json::{json, Map, Value};

    const ROOT: &str = "config/openloader/data/anvil-recipes";
    let mut out: Vec<(String, String)> = Vec::new();

    // pack.mcmeta — MANDATORY. pack_format is a NUMBER (15 for 1.20/1.20.1).
    let mcmeta = json!({
        "pack": {
            "pack_format": PACK_FORMAT_1_20,
            "description": "Anvil custom recipes",
        }
    });
    let mut mcmeta_s =
        serde_json::to_string_pretty(&mcmeta).unwrap_or_else(|_| "{}".to_string());
    mcmeta_s.push('\n');
    out.push((format!("{ROOT}/pack.mcmeta"), mcmeta_s));

    let rdir = recipe_dir(mc_version);

    for r in &set.recipes {
        let (ns, name) = split_id(&r.id);

        let v: Value = match &r.kind {
            RecipeKind::Shaped {
                pattern,
                key,
                result,
            } => {
                // key is a BTreeMap so the JSON object is already sorted;
                // build the ingredient objects through serde so the
                // `{"item"}`/`{"tag"}` shape is the single source of truth.
                let mut key_map = Map::new();
                for (k, ing) in key {
                    key_map.insert(
                        k.clone(),
                        serde_json::to_value(ing).unwrap_or(Value::Null),
                    );
                }
                json!({
                    "type": "minecraft:crafting_shaped",
                    "pattern": pattern,
                    "key": Value::Object(key_map),
                    "result": {
                        "item": result.item,
                        "count": result.count,
                    },
                })
            }
            RecipeKind::Shapeless {
                ingredients,
                result,
            } => {
                let ings: Vec<Value> = ingredients
                    .iter()
                    .map(|i| serde_json::to_value(i).unwrap_or(Value::Null))
                    .collect();
                json!({
                    "type": "minecraft:crafting_shapeless",
                    "ingredients": ings,
                    "result": {
                        "item": result.item,
                        "count": result.count,
                    },
                })
            }
            RecipeKind::Smelting {
                ingredient,
                result,
                experience,
                cookingtime,
            } => {
                // 1.20.1: `result` is a BARE STRING, not an object.
                json!({
                    "type": "minecraft:smelting",
                    "ingredient": serde_json::to_value(ingredient)
                        .unwrap_or(Value::Null),
                    "result": result,
                    "experience": experience,
                    "cookingtime": cookingtime,
                })
            }
        };

        let mut s =
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string());
        s.push('\n');
        out.push((format!("{ROOT}/data/{ns}/{rdir}/{name}.json"), s));
    }

    out
}

// ---------------------------------------------------------------------------
// validate_recipes
// ---------------------------------------------------------------------------

/// Validate the set against the allowed-namespace index. Empty = ok.
///
/// Issue order is stable: recipes in input order, and within a recipe a fixed
/// sub-order (grounding -> structural -> duplicate), then the global
/// quality pass at the end.
///
/// Hard checks (grounding, structural, duplicate) run on EVERY call. The
/// quality checks (`OrphanRecipe`, `SetHasNoModOutput`) are appended only when
/// `is_final` is true AND the set has >= 3 recipes (size-scaled leniency,
/// mirroring quest.rs which only flags orphans once the graph is big enough).
///
/// GROUNDING (Slice 2): every ingredient item, ingredient tag and result is
/// classified through the SAME Slice-1 `AllowedIndex` the quest tasks use
/// (`ground_recipe_id`), so a fabricated exact id whose namespace is
/// pinned-and-scanned is a HARD `UnknownItem` (the `cobblemon:mewtwo` fix
/// applied to recipes), a jar-absent / namespace-only id degrades to a
/// non-blocking `LowConfidenceId`, and the derived `anvil:<hex>` recipe id
/// (Anvil-authored) always grounds.
pub fn validate_recipes(
    set: &RecipeSet,
    idx: &AllowedIndex,
    is_final: bool,
) -> Vec<RecipeIssue> {
    let mut issues = Vec::new();

    // Classify ONE recipe ref via the shared Slice-1 grounding ladder and push
    // the matching issue: Unknown -> hard `UnknownItem`, LowConfidence ->
    // non-blocking `LowConfidenceId`, Ok -> nothing. `is_tag` selects
    // vocab.tags vs vocab.items (a recipe tag ingredient is a bare `ns:path`).
    fn ground(
        idx: &AllowedIndex,
        recipe: &str,
        id: &str,
        is_tag: bool,
        issues: &mut Vec<RecipeIssue>,
    ) {
        match idx.ground_recipe_id(id, is_tag) {
            RecipeGrounding::Ok => {}
            RecipeGrounding::LowConfidence(reason) => {
                issues.push(RecipeIssue::LowConfidenceId {
                    recipe: recipe.to_string(),
                    id: id.to_string(),
                    reason: reason.to_string(),
                });
            }
            RecipeGrounding::Unknown => {
                issues.push(RecipeIssue::UnknownItem {
                    recipe: recipe.to_string(),
                    id: id.to_string(),
                });
            }
        }
    }
    // An ingredient is item-or-tag; results are always items.
    let ground_ing =
        |idx: &AllowedIndex, recipe: &str, ing: &Ingredient, issues: &mut Vec<RecipeIssue>| {
            let is_tag = matches!(ing, Ingredient::Tag { .. });
            ground(idx, recipe, ingredient_id(ing), is_tag, issues);
        };

    let mut seen_ids: BTreeSet<&str> = BTreeSet::new();

    for r in &set.recipes {
        // --- grounding: every ingredient item, tag, and result ---
        match &r.kind {
            RecipeKind::Shaped { key, result, .. } => {
                for ing in key.values() {
                    ground_ing(idx, &r.id, ing, &mut issues);
                }
                ground(idx, &r.id, &result.item, false, &mut issues);
            }
            RecipeKind::Shapeless {
                ingredients,
                result,
            } => {
                for ing in ingredients {
                    ground_ing(idx, &r.id, ing, &mut issues);
                }
                ground(idx, &r.id, &result.item, false, &mut issues);
            }
            RecipeKind::Smelting {
                ingredient, result, ..
            } => {
                ground_ing(idx, &r.id, ingredient, &mut issues);
                ground(idx, &r.id, result, false, &mut issues);
            }
        }

        // --- structural ---
        match &r.kind {
            RecipeKind::Shaped { pattern, key, .. } => {
                let rows = pattern.len();
                let bad_rows = rows == 0 || rows > 3;
                let width = pattern.first().map(|r| r.chars().count()).unwrap_or(0);
                let unequal = pattern.iter().any(|p| p.chars().count() != width);
                let bad_cols = width > 3;
                if bad_rows || unequal || bad_cols {
                    issues.push(RecipeIssue::BadPattern {
                        recipe: r.id.clone(),
                    });
                }
                // Collect the non-space chars used in the pattern.
                let used: BTreeSet<char> = pattern
                    .iter()
                    .flat_map(|p| p.chars())
                    .filter(|c| *c != ' ')
                    .collect();
                // A key entry whose char never appears in the pattern.
                for k in key.keys() {
                    let appears = k.chars().all(|c| used.contains(&c));
                    if !appears || k.is_empty() {
                        issues.push(RecipeIssue::KeyNotInPattern {
                            recipe: r.id.clone(),
                            key: k.clone(),
                        });
                    }
                }
                // A pattern char with no key binding (single-char keys).
                let bound: BTreeSet<char> =
                    key.keys().flat_map(|k| k.chars()).collect();
                for c in &used {
                    if !bound.contains(c) {
                        issues.push(RecipeIssue::PatternCharUnbound {
                            recipe: r.id.clone(),
                            ch: c.to_string(),
                        });
                    }
                }
            }
            RecipeKind::Shapeless { ingredients, .. } => {
                if ingredients.is_empty() || ingredients.len() > 9 {
                    issues.push(RecipeIssue::EmptyIngredients {
                        recipe: r.id.clone(),
                    });
                }
            }
            RecipeKind::Smelting { .. } => {}
        }

        // --- duplicate id (after this recipe's own checks) ---
        if !seen_ids.insert(r.id.as_str()) {
            issues.push(RecipeIssue::DuplicateRecipeId {
                recipe: r.id.clone(),
            });
        }
    }

    if is_final {
        issues.extend(quality_issues(set));
    }

    issues
}

/// Quality gate (final-only, set must have >= 3 recipes).
///
/// DECISION + RATIONALE: the user's intent is "no orphan / pointless
/// recipes". A recipe is `OrphanRecipe` iff it touches NO pinned-mod
/// (non-minecraft) namespace on EITHER side — i.e. it only shuffles vanilla
/// items, which a modpack datapack has no reason to add. Crucially this does
/// NOT ban legitimate bridges: a recipe with vanilla INPUTS and a modded
/// OUTPUT (e.g. `minecraft:iron_ingot` -> `create:cogwheel`), or modded inputs
/// producing a vanilla output, both "touch" a mod namespace and are kept.
/// Only pure `minecraft:* -> minecraft:*` recipes fire `OrphanRecipe`.
///
/// Additionally, on final, if the WHOLE set produces nothing whose RESULT
/// namespace is a pinned mod, one `SetHasNoModOutput` is emitted: a recipe
/// pack that adds zero modded outputs is pointless as a whole even if
/// individual recipes consume modded items.
///
/// Skipped entirely when `recipes.len() < 3` (size-scaled leniency: a tiny
/// focused set isn't punished for being small — mirrors quest.rs's `n >= 6`
/// orphan threshold philosophy).
fn quality_issues(set: &RecipeSet) -> Vec<RecipeIssue> {
    let mut out = Vec::new();
    if set.recipes.len() < 3 {
        return out;
    }

    for r in &set.recipes {
        if !recipe_touches_mod(r) {
            out.push(RecipeIssue::OrphanRecipe {
                recipe: r.id.clone(),
            });
        }
    }

    let any_mod_output = set.recipes.iter().any(|r| match &r.kind {
        RecipeKind::Shaped { result, .. } => is_modded(&result.item),
        RecipeKind::Shapeless { result, .. } => is_modded(&result.item),
        RecipeKind::Smelting { result, .. } => is_modded(result),
    });
    if !any_mod_output {
        out.push(RecipeIssue::SetHasNoModOutput);
    }

    out
}

/// Does this recipe touch a pinned-mod namespace ANYWHERE (any ingredient
/// item, any ingredient tag, or the result)? Stops at the first modded hit.
fn recipe_touches_mod(r: &RecipeDef) -> bool {
    match &r.kind {
        RecipeKind::Shaped { key, result, .. } => {
            key.values().any(|i| is_modded(ingredient_id(i)))
                || is_modded(&result.item)
        }
        RecipeKind::Shapeless {
            ingredients,
            result,
        } => {
            ingredients.iter().any(|i| is_modded(ingredient_id(i)))
                || is_modded(&result.item)
        }
        RecipeKind::Smelting {
            ingredient, result, ..
        } => is_modded(ingredient_id(ingredient)) || is_modded(result),
    }
}

// NOTE: persistence (`write_recipes` / `load_recipes` / `anvil-recipes.json`)
// is GONE for good (Slice 2). This engine never owned a source-of-truth file:
// recipes live as quest-node facets in `<instance>/anvil-quests.json`, and
// `crate::quest::write_quests` is what aggregates every node's `recipes` into a
// transient `RecipeSet` (after stamping each with its DERIVED `anvil:<hex>`
// id), calls `to_openloader_files`, and writes the datapack under
// `<instance>/config/openloader/data/anvil-recipes/**`. There is no
// `anvil-recipes.json` anywhere in Anvil.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quest::build_index;
    use serde_json::Value;
    use std::collections::BTreeMap;

    fn item(id: &str) -> Ingredient {
        Ingredient::Item {
            item: id.to_string(),
        }
    }

    fn stack(id: &str, count: i64) -> ItemStack {
        ItemStack {
            item: id.to_string(),
            count,
        }
    }

    fn shaped(id: &str, result: &str) -> RecipeDef {
        let mut key = BTreeMap::new();
        key.insert("#".to_string(), item("minecraft:stick"));
        RecipeDef {
            id: id.to_string(),
            kind: RecipeKind::Shaped {
                pattern: vec!["#".to_string()],
                key,
                result: stack(result, 1),
            },
        }
    }

    // (1) Deterministic + well-formed: shaped + shapeless + smelting.
    #[test]
    fn openloader_files_deterministic_and_well_formed() {
        let mut key = BTreeMap::new();
        key.insert("I".to_string(), item("minecraft:iron_ingot"));
        key.insert("S".to_string(), Ingredient::Tag {
            tag: "minecraft:planks".to_string(),
        });
        let set = RecipeSet {
            recipes: vec![
                RecipeDef {
                    id: "create:fancy_block".to_string(),
                    kind: RecipeKind::Shaped {
                        pattern: vec![
                            "III".to_string(),
                            "ISI".to_string(),
                            "III".to_string(),
                        ],
                        key,
                        result: stack("create:fancy_block", 2),
                    },
                },
                RecipeDef {
                    id: "anvil_dust".to_string(), // bare id => minecraft ns
                    kind: RecipeKind::Shapeless {
                        ingredients: vec![
                            item("create:andesite_alloy"),
                            item("minecraft:gunpowder"),
                        ],
                        result: stack("minecraft:redstone", 4),
                    },
                },
                RecipeDef {
                    id: "create:smelt_ingot".to_string(),
                    kind: RecipeKind::Smelting {
                        ingredient: item("create:raw_zinc"),
                        result: "create:zinc_ingot".to_string(),
                        experience: 0.7,
                        cookingtime: 200,
                    },
                },
            ],
        };

        let a = to_openloader_files(&set, "1.20.1");
        let b = to_openloader_files(&set, "1.20.1");
        assert_eq!(a, b, "serialization must be byte-identical across runs");

        // pack.mcmeta present, pack_format == 15 (a number).
        let (_, mcmeta) = a
            .iter()
            .find(|(p, _)| p == "config/openloader/data/anvil-recipes/pack.mcmeta")
            .expect("pack.mcmeta emitted");
        let mv: Value = serde_json::from_str(mcmeta).expect("pack.mcmeta parses");
        assert_eq!(mv["pack"]["pack_format"], 15);
        assert!(mv["pack"]["pack_format"].is_i64());
        assert!(mcmeta.ends_with('\n'));

        // Shaped path is .../data/create/recipes/fancy_block.json (PLURAL).
        let (shaped_path, shaped_body) = a
            .iter()
            .find(|(p, _)| p.contains("/data/create/recipes/fancy_block.json"))
            .expect("shaped recipe file at namespaced path");
        assert!(shaped_path.ends_with("/recipes/fancy_block.json"));
        assert!(shaped_path.starts_with(
            "config/openloader/data/anvil-recipes/data/create/recipes/"
        ));
        let sv: Value = serde_json::from_str(shaped_body).expect("shaped parses");
        assert_eq!(sv["type"], "minecraft:crafting_shaped");
        // Shaped result is an OBJECT {item,count} in 1.20.1.
        assert!(sv["result"].is_object());
        assert_eq!(sv["result"]["item"], "create:fancy_block");
        assert_eq!(sv["result"]["count"], 2);
        assert_eq!(sv["key"]["I"]["item"], "minecraft:iron_ingot");
        assert_eq!(sv["key"]["S"]["tag"], "minecraft:planks");

        // Bare id lands in the minecraft namespace folder.
        assert!(a.iter().any(|(p, _)| p
            == "config/openloader/data/anvil-recipes/data/minecraft/recipes/anvil_dust.json"));

        // Smelting result is a BARE JSON STRING in 1.20.1.
        let (_, smelt_body) = a
            .iter()
            .find(|(p, _)| p.contains("/data/create/recipes/smelt_ingot.json"))
            .expect("smelting recipe file");
        let smv: Value = serde_json::from_str(smelt_body).expect("smelting parses");
        assert_eq!(smv["type"], "minecraft:smelting");
        assert!(
            smv["result"].is_string(),
            "1.20.1 smelting result MUST be a plain string, got {}",
            smv["result"]
        );
        assert_eq!(smv["result"], "create:zinc_ingot");
        assert!(smv["ingredient"].is_object());
        assert_eq!(smv["experience"], 0.7);
        assert_eq!(smv["cookingtime"], 200);
        assert!(smelt_body.ends_with('\n'));
    }

    // (2) Grounding: bad namespace rejected; pinned + minecraft accepted.
    #[test]
    fn grounding_rejects_bad_namespace_accepts_pinned() {
        let set = RecipeSet {
            recipes: vec![
                shaped("create:ok", "create:thing"),
                shaped("minecraft:vanilla_ok", "minecraft:diamond"),
                RecipeDef {
                    id: "bogusmod:bad".to_string(),
                    kind: RecipeKind::Smelting {
                        ingredient: item("bogusmod:ore"),
                        result: "bogusmod:ingot".to_string(),
                        experience: 0.1,
                        cookingtime: 200,
                    },
                },
            ],
        };
        let idx = build_index(&["create".to_string()]);
        let issues = validate_recipes(&set, &idx, false);

        // The hallucinated namespace is flagged (ingredient + result).
        assert!(issues.contains(&RecipeIssue::UnknownItem {
            recipe: "bogusmod:bad".to_string(),
            id: "bogusmod:ore".to_string(),
        }));
        assert!(issues.contains(&RecipeIssue::UnknownItem {
            recipe: "bogusmod:bad".to_string(),
            id: "bogusmod:ingot".to_string(),
        }));
        // create + minecraft are not flagged.
        assert!(!issues.iter().any(|i| matches!(
            i,
            RecipeIssue::UnknownItem { id, .. } if id.starts_with("create:") || id.starts_with("minecraft:")
        )));
    }

    // (3) Structural checks.
    #[test]
    fn structural_issues_flagged() {
        // Bad pattern: 4 rows.
        let mut k1 = BTreeMap::new();
        k1.insert("X".to_string(), item("minecraft:stone"));
        let bad_pattern = RecipeDef {
            id: "minecraft:bad_pat".to_string(),
            kind: RecipeKind::Shaped {
                pattern: vec![
                    "X".to_string(),
                    "X".to_string(),
                    "X".to_string(),
                    "X".to_string(),
                ],
                key: k1,
                result: stack("minecraft:stick", 1),
            },
        };

        // Key 'Z' not in pattern.
        let mut k2 = BTreeMap::new();
        k2.insert("X".to_string(), item("minecraft:stone"));
        k2.insert("Z".to_string(), item("minecraft:dirt"));
        let key_not_in = RecipeDef {
            id: "minecraft:key_not_in".to_string(),
            kind: RecipeKind::Shaped {
                pattern: vec!["X".to_string()],
                key: k2,
                result: stack("minecraft:stick", 1),
            },
        };

        // Pattern char 'Y' unbound (no key for it).
        let mut k3 = BTreeMap::new();
        k3.insert("X".to_string(), item("minecraft:stone"));
        let unbound = RecipeDef {
            id: "minecraft:unbound".to_string(),
            kind: RecipeKind::Shaped {
                pattern: vec!["XY".to_string()],
                key: k3,
                result: stack("minecraft:stick", 1),
            },
        };

        // Shapeless with 0 ingredients and with >9.
        let empty_less = RecipeDef {
            id: "minecraft:empty_less".to_string(),
            kind: RecipeKind::Shapeless {
                ingredients: vec![],
                result: stack("minecraft:stick", 1),
            },
        };
        let too_many = RecipeDef {
            id: "minecraft:too_many".to_string(),
            kind: RecipeKind::Shapeless {
                ingredients: (0..10).map(|_| item("minecraft:stone")).collect(),
                result: stack("minecraft:stick", 1),
            },
        };

        let set = RecipeSet {
            recipes: vec![bad_pattern, key_not_in, unbound, empty_less, too_many],
        };
        let idx = build_index(&[]);
        let issues = validate_recipes(&set, &idx, false);

        assert!(issues.contains(&RecipeIssue::BadPattern {
            recipe: "minecraft:bad_pat".to_string()
        }));
        assert!(issues.contains(&RecipeIssue::KeyNotInPattern {
            recipe: "minecraft:key_not_in".to_string(),
            key: "Z".to_string()
        }));
        assert!(issues.contains(&RecipeIssue::PatternCharUnbound {
            recipe: "minecraft:unbound".to_string(),
            ch: "Y".to_string()
        }));
        assert!(issues.contains(&RecipeIssue::EmptyIngredients {
            recipe: "minecraft:empty_less".to_string()
        }));
        assert!(issues.contains(&RecipeIssue::EmptyIngredients {
            recipe: "minecraft:too_many".to_string()
        }));
    }

    #[test]
    fn duplicate_recipe_id_flagged() {
        let set = RecipeSet {
            recipes: vec![
                shaped("create:dup", "create:a"),
                shaped("create:dup", "create:b"),
            ],
        };
        let idx = build_index(&["create".to_string()]);
        let issues = validate_recipes(&set, &idx, false);
        assert!(issues.contains(&RecipeIssue::DuplicateRecipeId {
            recipe: "create:dup".to_string()
        }));
    }

    // (4) Quality gate.
    #[test]
    fn quality_gate_orphan_and_bridge() {
        // 3 recipes so the gate is active. One pure-vanilla orphan, one
        // vanilla-in -> mod-out bridge (must NOT be orphan), one mod recipe.
        let orphan = RecipeDef {
            id: "minecraft:vanilla_shuffle".to_string(),
            kind: RecipeKind::Shapeless {
                ingredients: vec![item("minecraft:dirt")],
                result: stack("minecraft:coarse_dirt", 1),
            },
        };
        let bridge = RecipeDef {
            id: "minecraft:bridge".to_string(), // bare id, vanilla-named
            kind: RecipeKind::Shapeless {
                ingredients: vec![item("minecraft:iron_ingot")],
                result: stack("create:cogwheel", 1), // modded OUTPUT
            },
        };
        let modded = shaped("create:gear", "create:gear");
        let set = RecipeSet {
            recipes: vec![orphan, bridge, modded],
        };
        let idx = build_index(&["create".to_string()]);

        // Non-final: quality issues suppressed.
        let non_final = validate_recipes(&set, &idx, false);
        assert!(
            !non_final
                .iter()
                .any(|i| matches!(i, RecipeIssue::OrphanRecipe { .. })),
            "quality gate must not run on non-final"
        );

        // Final: only the pure-vanilla recipe is an orphan; the bridge is OK.
        let final_issues = validate_recipes(&set, &idx, true);
        assert!(final_issues.contains(&RecipeIssue::OrphanRecipe {
            recipe: "minecraft:vanilla_shuffle".to_string()
        }));
        assert!(
            !final_issues.iter().any(|i| matches!(
                i,
                RecipeIssue::OrphanRecipe { recipe } if recipe == "minecraft:bridge"
            )),
            "vanilla-in -> mod-out bridge must NOT be flagged orphan"
        );
        // The set has a modded output (create:cogwheel, create:gear).
        assert!(!final_issues.contains(&RecipeIssue::SetHasNoModOutput));
    }

    #[test]
    fn quality_gate_skipped_under_three() {
        // 2 pure-vanilla recipes: would be orphans, but the set is too small.
        let set = RecipeSet {
            recipes: vec![
                RecipeDef {
                    id: "minecraft:a".to_string(),
                    kind: RecipeKind::Shapeless {
                        ingredients: vec![item("minecraft:dirt")],
                        result: stack("minecraft:coarse_dirt", 1),
                    },
                },
                RecipeDef {
                    id: "minecraft:b".to_string(),
                    kind: RecipeKind::Shapeless {
                        ingredients: vec![item("minecraft:sand")],
                        result: stack("minecraft:glass", 1),
                    },
                },
            ],
        };
        let idx = build_index(&["create".to_string()]);
        let issues = validate_recipes(&set, &idx, true);
        assert!(
            !issues.iter().any(|i| matches!(
                i,
                RecipeIssue::OrphanRecipe { .. } | RecipeIssue::SetHasNoModOutput
            )),
            "quality gate must be skipped for <3 recipes, got {issues:?}"
        );
    }

    #[test]
    fn set_has_no_mod_output_flagged() {
        // 3 recipes, all consume modded items but ALL outputs are vanilla.
        let set = RecipeSet {
            recipes: vec![
                RecipeDef {
                    id: "create:a".to_string(),
                    kind: RecipeKind::Shapeless {
                        ingredients: vec![item("create:andesite_alloy")],
                        result: stack("minecraft:iron_ingot", 1),
                    },
                },
                RecipeDef {
                    id: "create:b".to_string(),
                    kind: RecipeKind::Shapeless {
                        ingredients: vec![item("create:brass_ingot")],
                        result: stack("minecraft:gold_ingot", 1),
                    },
                },
                RecipeDef {
                    id: "create:c".to_string(),
                    kind: RecipeKind::Smelting {
                        ingredient: item("create:raw_zinc"),
                        result: "minecraft:iron_nugget".to_string(),
                        experience: 0.1,
                        cookingtime: 200,
                    },
                },
            ],
        };
        let idx = build_index(&["create".to_string()]);
        let issues = validate_recipes(&set, &idx, true);
        assert!(issues.contains(&RecipeIssue::SetHasNoModOutput));
        // None are OrphanRecipe (each touches a mod via the modded input).
        assert!(!issues
            .iter()
            .any(|i| matches!(i, RecipeIssue::OrphanRecipe { .. })));
    }

}
