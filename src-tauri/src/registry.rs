//! Slice 1 grounding: static jar-scan → concrete `RegistryVocab`.
//!
//! DESIGN (declared up front so the implementation does not drift):
//!
//! The fabricated-id problem (`cobblemon:mewtwo`, which is not a real entity)
//! is fixed by moving from NAMESPACE-only grounding to CONCRETE-id grounding
//! against the pack's REAL registry, scanned offline from the resolved mod
//! jars. This module owns ONLY the scan + the vocab type; the grounding
//! decision lives in `crate::quest` (`AllowedIndex`/`check_task`).
//!
//! Three tiers (design-doc §2):
//!  - Tier 1 (here): static jar scan → concrete ids + human labels. Offline,
//!    deterministic, authoritative immediately for datapack-JSON content
//!    (recipes/advancements/structures/biomes/tags), provisional for
//!    code-registered items/entities/blocks (lang-key heuristic).
//!  - Tier 2 (here, seam): an Anvil-authored allowlist passed in by the
//!    caller — ids Anvil's own datapacks will emit (recipe `anvil:<hex>`,
//!    future boss/site/gate ids). Slice 2/3 populate it.
//!  - Tier 3 (scaffold only): first-launch registry-dump reconciliation.
//!    See `FIRST_LAUNCH_PROBE_TODO` — NOT built in Slice 1.
//!
//! JAR AVAILABILITY (a real constraint): the launcher downloads mod jars to
//! `<instance>/mods/` only at launch (`launch.rs::ensure_mod`). At curator /
//! assemble time the jars may NOT be on disk yet. Scanning therefore NEVER
//! blocks and NEVER errors: a pinned mod whose jar is absent contributes NO
//! concrete ids and its namespace is recorded in `unscanned`; grounding then
//! degrades that namespace to a low-confidence (NOT hard-fail) check.
//!
//! 1.20.1 vs 1.21 datapack folder delta (detection-spec §4): folders were
//! renamed singular in 1.21 (`recipes/`→`recipe/`, `advancements/`→
//! `advancement/`, `structures/`→`structure/`). The two name sets are
//! disjoint, so we scan BOTH and union — no need to branch on `pack_format`.
//!
//! Determinism: every id collection is a `BTreeSet`; `mod_meta` is sorted by
//! id. Two scans of the same jar set are byte-identical (the property the
//! determinism test asserts).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::{Cursor, Read};
use std::path::Path;

use crate::instance::Instance;

/// Per-mod metadata harvested from `fabric.mod.json` / `META-INF/mods.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModMeta {
    pub id: String,
    pub name: String,
    pub categories: Vec<String>,
}

/// The concrete grounding vocabulary scanned from the pack's resolved jars.
/// Every set is namespaced-id form (`ns:path`, slashes preserved for recipe
/// ids like `create:crushing/andesite`). Deterministic by construction.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryVocab {
    pub items: BTreeSet<String>,
    pub entities: BTreeSet<String>,
    pub blocks: BTreeSet<String>,
    pub advancements: BTreeSet<String>,
    pub structures: BTreeSet<String>,
    pub biomes: BTreeSet<String>,
    pub tags: BTreeSet<String>,
    pub recipe_ids: BTreeSet<String>,
    /// Human labels harvested from lang keys, keyed by concrete id. Used by
    /// `query_registry` so the model sees readable names, not just ids.
    pub labels: std::collections::BTreeMap<String, String>,
    pub mod_meta: Vec<ModMeta>,
}

impl RegistryVocab {
    /// True iff nothing at all was scanned (no jar on disk). Grounding uses
    /// this to fall back to namespace-only mode so the no-jars case (every
    /// pre-existing test) stays exactly as before.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
            && self.entities.is_empty()
            && self.blocks.is_empty()
            && self.advancements.is_empty()
            && self.structures.is_empty()
            && self.biomes.is_empty()
            && self.tags.is_empty()
            && self.recipe_ids.is_empty()
            && self.mod_meta.is_empty()
    }
}

/// Provenance of a `ScanResult`. Distinguishes a pure static jar scan from a
/// scan that has been reconciled against a real first-launch registry dump.
///
/// `#[default] Static` + `#[serde(default)]` on the field is the serde
/// back-compat mechanism: an `anvil-registry.json` written before Slice 1.5
/// (no `source` key) still deserializes — it loads as `Static`, which is
/// exactly its true provenance, so the existing cache keeps working untouched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanSource {
    /// Static jar scan only (Slice 1 behavior).
    #[default]
    Static,
    /// The static vocab was unioned with a live `/dump registry` dump from a
    /// headless first-launch dedicated server (Slice 1.5). `minecraft:*` and
    /// code-registered ids are now authoritative, so the
    /// `minecraft`-is-unscanned-by-construction fallback is dropped.
    DumpReconciled,
}

/// Result of scanning an instance's resolved jars.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanResult {
    pub vocab: RegistryVocab,
    /// Pinned-mod namespaces whose jar was NOT on disk at scan time (so they
    /// contributed no concrete ids). Grounding treats an id in one of these
    /// namespaces as low-confidence/unverified, never a hard fail.
    pub unscanned: BTreeSet<String>,
    /// Cache key: a hash of the pinned mod set. A cached scan is reused only
    /// when this matches the instance's current pinned set.
    pub mod_set_key: String,
    /// Provenance. Absent in pre-Slice-1.5 caches → deserializes as `Static`.
    #[serde(default)]
    pub source: ScanSource,
}

// ---------------------------------------------------------------------------
// Tier 3 seam (scaffold only — NOT built in Slice 1)
// ---------------------------------------------------------------------------

/// SLICE 1.5 SEAM — first-launch registry-dump reconciliation.
///
/// The static scan misses code-registered / KubeJS-scripted ids. The design
/// (detection-spec §4 method C, design-doc Slice 1.5) closes that with a
/// one-off headless dedicated-server `/dump registry` pass driven by the
/// launcher on first run, reconciling the static `RegistryVocab` against the
/// live registry and downgrading any objective that no longer grounds.
///
/// Slice 1.5: union the parsed `<dump>/dump/<registry-path>/<ns>.json` arrays
/// into the static `RegistryVocab`. `None` → returned unchanged (every failure
/// path in the launcher degrades to here, so this MUST be a pure identity in
/// that case). Tolerant by construction: a missing / unreadable / non-array
/// file is skipped; a sibling that parses still lands. Deterministic
/// (everything is a `BTreeSet`, so re-running on the same dump is idempotent
/// and byte-identical).
pub fn reconcile_with_launch_dump(
    mut static_vocab: RegistryVocab,
    dump_dir: Option<&Path>,
) -> RegistryVocab {
    let Some(root) = dump_dir else {
        // No dump (server never ran / failed / timed out). The static scan is
        // authoritative-as-far-as-it-goes; the `unscanned` low-confidence path
        // covers what it misses. NEVER a false reject.
        return static_vocab;
    };
    let dump = root.join("dump");

    // (registry-relative path under `dump/`, vocab set to union into).
    // `recipe`/`advancement` etc. are the singular dedicated-server registry
    // ids (the dump tool uses the real registry keys, not datapack folder
    // names) — and the worldgen sub-registries are nested. Unknown dirs are
    // ignored: we only consume the registries grounding actually checks.
    type Pick = fn(&mut RegistryVocab) -> &mut BTreeSet<String>;
    const MAP: &[(&str, Pick)] = &[
        ("item", (|v| &mut v.items) as Pick),
        ("entity_type", (|v| &mut v.entities) as Pick),
        ("block", (|v| &mut v.blocks) as Pick),
        ("worldgen/biome", (|v| &mut v.biomes) as Pick),
        ("structure", (|v| &mut v.structures) as Pick),
        ("recipe", (|v| &mut v.recipe_ids) as Pick),
        ("tags", (|v| &mut v.tags) as Pick),
    ];

    for (rel, pick) in MAP {
        let reg_dir = dump.join(rel);
        let Ok(entries) = std::fs::read_dir(&reg_dir) else {
            continue; // registry not dumped — skip, never panic.
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(arr) = serde_json::from_str::<Vec<String>>(&body) else {
                // Non-array / malformed → skip this file, keep siblings.
                continue;
            };
            let set = pick(&mut static_vocab);
            for id in arr {
                if !id.is_empty() {
                    set.insert(id);
                }
            }
        }
    }
    static_vocab
}

/// Reconcile a whole `ScanResult` against a first-launch dump: union the
/// vocab, then DROP every namespace that the dump actually covered from
/// `unscanned` (including `minecraft` — the dedicated server's registry IS the
/// authoritative vanilla registry), and stamp `source = DumpReconciled` so the
/// `minecraft`-is-unscanned-by-construction fallback in `quest.rs` is skipped.
/// `None` → vocab/`unscanned` unchanged but still re-keyed/stamped as `Static`
/// (callers never persist a no-op reconcile; this stays a safe identity).
pub fn reconcile_scan(static_scan: ScanResult, dump_dir: Option<&Path>) -> ScanResult {
    let ScanResult {
        vocab,
        mut unscanned,
        mod_set_key,
        ..
    } = static_scan;

    let Some(root) = dump_dir else {
        return ScanResult {
            vocab,
            unscanned,
            mod_set_key,
            source: ScanSource::Static,
        };
    };

    let vocab = reconcile_with_launch_dump(vocab, Some(root));

    // Every namespace the dump covered is now authoritative — it is no longer
    // "jar absent at scan time", so it must leave `unscanned` or grounding
    // would still degrade real ids to low-confidence. `minecraft` included:
    // the server's registry dump IS the vanilla registry.
    let mut covered: BTreeSet<String> = BTreeSet::new();
    for set in [
        &vocab.items,
        &vocab.entities,
        &vocab.blocks,
        &vocab.biomes,
        &vocab.structures,
        &vocab.recipe_ids,
        &vocab.tags,
    ] {
        for id in set {
            if let Some((ns, _)) = id.split_once(':') {
                if !ns.is_empty() {
                    covered.insert(ns.to_string());
                }
            }
        }
    }
    unscanned.retain(|ns| !covered.contains(ns));

    ScanResult {
        vocab,
        unscanned,
        mod_set_key,
        source: ScanSource::DumpReconciled,
    }
}

// ---------------------------------------------------------------------------
// Cache-key helper
// ---------------------------------------------------------------------------

/// Stable key over the pinned mod set (sorted `(project_id, version_id,
/// sha1)`). A cached scan is valid only while this is unchanged.
pub fn mod_set_key(inst: &Instance) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut rows: Vec<(String, String, String)> = inst
        .mods
        .iter()
        .map(|m| (m.project_id.clone(), m.version_id.clone(), m.sha1.clone()))
        .collect();
    rows.sort();
    let mut h = DefaultHasher::new();
    rows.hash(&mut h);
    format!("{:016X}", h.finish())
}

// ---------------------------------------------------------------------------
// Namespace derivation (kept byte-identical to curator.rs's existing rule so
// `unscanned` lines up with the namespace the validator extracts from an id)
// ---------------------------------------------------------------------------

/// Filename-derived namespace guess: basename, lowercased, substring before
/// the first of `-`, `_`, `+`, `.`. This is the EXACT rule curator.rs and
/// lib.rs already use for `inst.mods`; keeping it identical means a jar that
/// fails to scan is recorded under the same namespace string the grounding
/// path will extract from a quest id.
pub fn filename_namespace(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let cut = base
        .find(|c: char| matches!(c, '-' | '_' | '+' | '.'))
        .unwrap_or(base.len());
    base[..cut].trim().to_string()
}

// ---------------------------------------------------------------------------
// Class -> mod attribution (deterministic crash culprit, no LLM)
// ---------------------------------------------------------------------------

/// Owner of a Java class, resolved by scanning the instance's installed mod
/// jars for the class's package. Lets crash diagnosis turn a bare
/// `NoClassDefFoundError <class>` (or a runtime stack-frame class) into the MOD
/// that ships — or should ship — it, with no LLM guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassOwner {
    /// Fabric mod id of the jar that owns the class's package.
    pub mod_id: String,
    /// How many leading package segments matched (deeper = more certain).
    pub depth: usize,
}

/// Find which INSTALLED mod jar owns a Java class's package, by matching the
/// class path against the `.class` entries inside each jar. Returns the jar
/// with the DEEPEST package-prefix match (the true owner wins over a shallow
/// shaded/relocated match), or `None` when no installed jar ships that package.
///
/// Crash-diagnosis semantics:
///  - `Some` for a MISSING class (`NoClassDefFoundError`) ⇒ the providing mod
///    IS installed but at an incompatible version (the class moved/renamed
///    between versions) ⇒ repin that mod (version skew), do NOT remove others.
///  - `Some` for a class named in a runtime/mixin stack frame ⇒ that mod is the
///    one whose code is on the stack ⇒ the culprit.
///  - `None` ⇒ the library is absent entirely ⇒ add it (or remove the consumer).
///
/// Accepts either dotted (`a.b.C`) or slashed (`a/b/C`) class paths. Requires a
/// ≥2-segment package match so a bare 2-token name can never own everything.
/// Runs only on the (rare) crash path; per-jar central-directory enumeration is
/// cheap. Tolerant: an unreadable / non-zip jar is skipped, never fatal.
pub fn attribute_class_to_mod(
    inst: &Instance,
    instance_dir: &Path,
    class: &str,
) -> Option<ClassOwner> {
    let slashed = class.trim().replace('.', "/");
    let segs: Vec<&str> =
        slashed.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 {
        return None; // need at least `a/b/Class` — a 2-segment name has no pkg
    }
    let pkg_depth = segs.len() - 1; // exclude the final class-name segment

    let mut best: Option<ClassOwner> = None;
    for m in &inst.mods {
        let jar = instance_dir.join(&m.path);
        let Ok(f) = std::fs::File::open(&jar) else {
            continue;
        };
        let Ok(mut z) = zip::ZipArchive::new(f) else {
            continue;
        };
        // Entry names first (immutable borrow), then the mod id (mutable).
        let names: Vec<String> =
            z.file_names().map(str::to_string).collect();
        // Deepest package prefix (≥2 segments) some `.class` entry shares.
        let mut hit: Option<usize> = None;
        for plen in (2..=pkg_depth).rev() {
            let prefix = format!("{}/", segs[..plen].join("/"));
            if names
                .iter()
                .any(|n| n.ends_with(".class") && n.starts_with(&prefix))
            {
                hit = Some(plen);
                break;
            }
        }
        let Some(depth) = hit else { continue };
        if best.as_ref().map(|b| depth > b.depth).unwrap_or(true) {
            let mod_id = jar_fabric_id(&mut z)
                .unwrap_or_else(|| filename_namespace(&m.path));
            best = Some(ClassOwner { mod_id, depth });
        }
    }
    best
}

/// The mod's own `id` from its `fabric.mod.json`, if readable.
fn jar_fabric_id<R: Read + std::io::Seek>(
    z: &mut zip::ZipArchive<R>,
) -> Option<String> {
    let mut s = String::new();
    z.by_name("fabric.mod.json")
        .ok()?
        .read_to_string(&mut s)
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("id")?.as_str().map(str::to_string)
}

// ---------------------------------------------------------------------------
// Jar scan
// ---------------------------------------------------------------------------

/// Scan every resolved jar of an instance into a `ScanResult`. `mods_root` is
/// the dir the launcher downloads jars into (`<instance>/mods` via
/// `m.path`). A pinned mod whose jar is absent is degraded into `unscanned`.
/// Never blocks, never errors.
pub fn scan_instance(inst: &Instance, instance_dir: &Path) -> ScanResult {
    let mut vocab = RegistryVocab::default();
    let mut unscanned: BTreeSet<String> = BTreeSet::new();

    for m in &inst.mods {
        let ns_guess = filename_namespace(&m.path);
        let jar = instance_dir.join(&m.path);
        if !jar.is_file() {
            if !ns_guess.is_empty() {
                unscanned.insert(ns_guess);
            }
            continue;
        }
        if !scan_one_jar(&jar, &mut vocab) {
            // Jar present but unreadable / not a zip: degrade like absent.
            if !ns_guess.is_empty() {
                unscanned.insert(ns_guess);
            }
        }
    }

    vocab.mod_meta.sort_by(|a, b| a.id.cmp(&b.id));
    vocab.mod_meta.dedup_by(|a, b| a.id == b.id);

    ScanResult {
        vocab,
        unscanned,
        mod_set_key: mod_set_key(inst),
        source: ScanSource::Static,
    }
}

/// Scan a single jar (a zip). Returns false if it could not be opened as a
/// zip (caller degrades that mod to `unscanned`). All extracted ids are added
/// to `vocab`. Tolerant: a malformed entry is skipped, never fatal.
pub fn scan_one_jar(jar: &Path, vocab: &mut RegistryVocab) -> bool {
    let Ok(f) = std::fs::File::open(jar) else {
        return false;
    };
    let Ok(mut archive) = zip::ZipArchive::new(f) else {
        return false;
    };

    // First pass: collect entry names (borrow of `archive` must end before we
    // re-borrow individual entries to read their bytes).
    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
        .collect();

    for name in &names {
        classify_path(name, vocab);
    }

    // Second pass: read the files we actually need to parse (lang + metadata).
    for name in &names {
        let want_lang = is_lang_path(name);
        let want_fabric = name == "fabric.mod.json";
        let want_forge =
            name == "META-INF/mods.toml" || name == "META-INF/neoforge.mods.toml";
        if !(want_lang || want_fabric || want_forge) {
            continue;
        }
        let mut buf = String::new();
        {
            let Ok(mut entry) = archive.by_name(name) else {
                continue;
            };
            if entry.read_to_string(&mut buf).is_err() {
                continue;
            }
        }
        if want_lang {
            if let Some(ns) = lang_namespace(name) {
                harvest_lang(&ns, &buf, vocab);
            }
        } else if want_fabric {
            harvest_fabric_meta(&buf, vocab);
        } else if want_forge {
            harvest_forge_meta(&buf, vocab);
        }
    }

    true
}

/// Classify a datapack/asset path into the right vocab set. Handles BOTH the
/// 1.20.1 plural and 1.21 singular folder names (detection-spec §4 delta).
fn classify_path(path: &str, vocab: &mut RegistryVocab) {
    // data/<ns>/<kind>/<rest...>.json
    let Some(rest) = path.strip_prefix("data/") else {
        return;
    };
    let mut it = rest.splitn(2, '/');
    let Some(ns) = it.next() else { return };
    let Some(after_ns) = it.next() else { return };
    if ns.is_empty() {
        return;
    }

    // tags/**: id is the tag path under tags/<reg>/, e.g.
    // data/c/tags/items/ingots.json -> tag id "c:ingots" (the registry
    // folder is dropped; tag refs are `#ns:path`). We keep `ns:path` form.
    if let Some(tag_rest) = after_ns.strip_prefix("tags/") {
        // tag_rest = "<registry-or-deeper>/<path>.json"; the registry folder
        // (items/blocks/entity_types/...) is not part of the tag id.
        if let Some((_, p)) = tag_rest.split_once('/') {
            if let Some(stem) = p.strip_suffix(".json") {
                vocab.tags.insert(format!("{ns}:{stem}"));
            }
        }
        return;
    }

    // worldgen structures + biomes (folder names unchanged across 1.20.1/1.21).
    if let Some(p) = after_ns.strip_prefix("worldgen/structure/") {
        if let Some(stem) = p.strip_suffix(".json") {
            vocab.structures.insert(format!("{ns}:{stem}"));
        }
        return;
    }
    if let Some(p) = after_ns.strip_prefix("worldgen/biome/") {
        if let Some(stem) = p.strip_suffix(".json") {
            vocab.biomes.insert(format!("{ns}:{stem}"));
        }
        return;
    }

    // recipes: 1.20.1 `recipes/`, 1.21 `recipe/`.
    for pfx in ["recipes/", "recipe/"] {
        if let Some(p) = after_ns.strip_prefix(pfx) {
            if let Some(stem) = p.strip_suffix(".json") {
                vocab.recipe_ids.insert(format!("{ns}:{stem}"));
            }
            return;
        }
    }
    // advancements: 1.20.1 `advancements/`, 1.21 `advancement/`.
    for pfx in ["advancements/", "advancement/"] {
        if let Some(p) = after_ns.strip_prefix(pfx) {
            if let Some(stem) = p.strip_suffix(".json") {
                vocab.advancements.insert(format!("{ns}:{stem}"));
            }
            return;
        }
    }
}

fn is_lang_path(path: &str) -> bool {
    path.starts_with("assets/")
        && path.ends_with("/lang/en_us.json")
}

/// `assets/<ns>/lang/en_us.json` -> `<ns>`.
fn lang_namespace(path: &str) -> Option<String> {
    let rest = path.strip_prefix("assets/")?;
    let ns = rest.split('/').next()?;
    if ns.is_empty() {
        None
    } else {
        Some(ns.to_string())
    }
}

/// Harvest concrete item/block/entity/advancement ids + human labels from a
/// lang file's keys. Keys look like `item.<ns>.<path>` (path may contain `.`
/// for sub-paths) -> id `<ns>:<path-with-/>`. Heuristic per detection-spec §4
/// (lang keys ≈ ids, not 1:1) — this is the provisional items/entities surface
/// the design accepts; the `unscanned` path covers what it misses.
fn harvest_lang(ns: &str, body: &str, vocab: &mut RegistryVocab) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };
    let Some(obj) = v.as_object() else {
        return;
    };
    for (k, val) in obj {
        let label = val.as_str().unwrap_or("").to_string();
        // key = <domain>.<ns>.<rest...>
        let mut parts = k.splitn(3, '.');
        let domain = parts.next().unwrap_or("");
        let key_ns = parts.next().unwrap_or("");
        let Some(rest) = parts.next() else { continue };
        if key_ns != ns || rest.is_empty() {
            continue;
        }
        // A concrete registered id's translation key is
        // `<domain>.<ns>.<flatpath>` — `rest` is a SINGLE segment for the
        // overwhelming majority of mods/vanilla. A `.` inside `rest` means a
        // NESTED lang sub-key (`sleeping_bag.auto_use.tooltip`, `<id>.state`,
        // `<id>.desc`, …), NOT a registry id — harvesting it into an id
        // bucket is exactly the pollution that let
        // `comforts:sleeping_bag.auto_use.tooltip` masquerade as an item and
        // ship a quest that crashed world creation. Nested keys are dropped
        // from BOTH id buckets and labels (the real flat id's own key still
        // provides its label). Accepted tradeoff: a rare mod using a
        // `/`-subdir item path (`item.ns.tools.wrench`) is missed by the
        // STATIC scan — the authoritative `/dump registry` (Slice 1.5) is
        // the real id source; this is only the hardened offline fallback.
        if rest.contains('.') {
            continue;
        }
        let id = format!("{ns}:{rest}");
        let target = match domain {
            "item" => Some(&mut vocab.items),
            "block" => Some(&mut vocab.blocks),
            "entity" => Some(&mut vocab.entities),
            "advancement" => Some(&mut vocab.advancements),
            _ => None,
        };
        if let Some(set) = target {
            set.insert(id.clone());
            if !label.is_empty() {
                vocab.labels.entry(id).or_insert(label);
            }
        }
    }
}

/// Parse `fabric.mod.json` for id/name + categories-ish hints. Tolerant: a
/// missing field just yields an empty string / no categories.
fn harvest_fabric_meta(body: &str, vocab: &mut RegistryVocab) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
    if id.is_empty() {
        return;
    }
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or(id)
        .to_string();
    // Fabric has no formal categories; surface keywords if present.
    let categories = v
        .get("custom")
        .and_then(|c| c.get("modmenu"))
        .and_then(|m| m.get("badges"))
        .and_then(|b| b.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    vocab.mod_meta.push(ModMeta {
        id: id.to_string(),
        name,
        categories,
    });
}

/// Parse a Forge/NeoForge `mods.toml` for the first `[[mods]]` id + display
/// name. A tiny hand-rolled scan (no toml crate dependency) — only the two
/// fields we need, tolerant of formatting.
fn harvest_forge_meta(body: &str, vocab: &mut RegistryVocab) {
    let mut id = String::new();
    let mut name = String::new();
    let mut in_mods = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("[[mods]]") {
            if in_mods {
                break; // only the first mod block
            }
            in_mods = true;
            continue;
        }
        if !in_mods {
            continue;
        }
        if let Some(rest) = t.strip_prefix("modId") {
            id = toml_value(rest);
        } else if let Some(rest) = t.strip_prefix("displayName") {
            name = toml_value(rest);
        }
    }
    if !id.is_empty() {
        if name.is_empty() {
            name = id.clone();
        }
        vocab.mod_meta.push(ModMeta {
            id,
            name,
            categories: Vec::new(),
        });
    }
}

/// `= "value"` (or `='value'`) -> `value`. Best-effort.
fn toml_value(after_key: &str) -> String {
    let s = after_key.trim();
    let s = s.strip_prefix('=').unwrap_or(s).trim();
    s.trim_matches(|c| c == '"' || c == '\'').to_string()
}

// ---------------------------------------------------------------------------
// Jar dependency manifest (Tier 1 resolver fix)
//
// Modrinth's `version.dependencies[]` is author-curated and systematically
// incomplete (Spectrum 1.8.13 declares ZERO deps on Modrinth, yet its jar's
// fabric.mod.json requires revelationary, modonomicon, trinkets, ...). The jar
// itself is the source of truth Fabric Loader actually enforces. This reads it.
// ---------------------------------------------------------------------------

/// Modids supplied by the loader/runtime itself: never a Modrinth project, so
/// never a "missing dependency". `fabric`/`fabric-api` are the Fabric API
/// umbrella ids; fabric-api is effectively always a pack root, so excluding it
/// here just avoids a false miss rather than dropping a real requirement.
pub const BUILTIN_MODIDS: &[&str] = &[
    "minecraft",
    "java",
    "fabricloader",
    "fabric",
    "fabric-api",
    "forge",
    "neoforge",
    "mixinextras",
    "mixinsquared",
];

/// What a mod jar declares about itself: the modids it PROVIDES (its own `id`
/// plus any `provides`) and its HARD dependencies as `(modid, declared_range)`.
/// Built-ins are filtered out of `requires`. Soft `recommends`/`suggests` are
/// intentionally ignored (Fabric does not enforce them).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JarManifest {
    /// `(modid, version)` for every modid this jar makes available: its own
    /// `id` + each `provides[]` alias (all paired with the jar's OWN
    /// `version`), AND every JIJ-bundled nested module paired with that
    /// nested `fabric.mod.json`'s OWN `version`. Version is `""` when absent
    /// or unparseable (presence is still recorded). A resolver can now
    /// evaluate a dependent's range against a *bundled* provider's version
    /// (e.g. LambDynamicLights needs `fabric-lifecycle-events-v1 >=2.2.22`
    /// while fabric-api bundles it at `2.2.21+...`).
    pub provided: Vec<(String, String)>,
    pub requires: Vec<(String, String)>,
    /// Negative constraints from `fabric.mod.json` `breaks` (and `conflicts`):
    /// `(modid, declared_range)`, parsed identically to `depends`/`requires`
    /// (string or array joined with `" || "`). The presence of `modid` at a
    /// version inside this range means the two cannot coexist. Empty for
    /// Forge (`mods.toml` has no analogous block on the 1.20.1 Fabric-first
    /// target — see `parse_forge_manifest`).
    pub breaks: Vec<(String, String)>,
    /// The mod's own declared version (fabric.mod.json `version`). Empty for
    /// Forge (its `mods.toml` usually has a `${file.jarVersion}` placeholder,
    /// not the real version) and when absent. Used by the exact-pin
    /// dependency audit to compare a present provider against a requester's
    /// exact-version constraint.
    pub version: String,
}

/// Parse a mod jar's loader manifest from its bytes. Fabric `fabric.mod.json`
/// is tried first (the dominant loader for the 1.20.1 target); then
/// Forge/NeoForge `META-INF/mods.toml`. Tolerant: a non-zip blob, a missing
/// manifest, or unparseable fields yield `None`/empty rather than an error.
pub fn jar_manifest(bytes: &[u8]) -> Option<JarManifest> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;

    // Outer manifest (fabric first, then forge/neoforge).
    let mut manifest: Option<JarManifest> = None;
    {
        if let Ok(mut e) = archive.by_name("fabric.mod.json") {
            let mut s = String::new();
            if e.read_to_string(&mut s).is_ok() {
                manifest = parse_fabric_manifest(&s);
            }
        }
    }
    if manifest.is_none() {
        for toml in ["META-INF/mods.toml", "META-INF/neoforge.mods.toml"] {
            if let Ok(mut e) = archive.by_name(toml) {
                let mut s = String::new();
                if e.read_to_string(&mut s).is_ok() {
                    manifest = Some(parse_forge_manifest(&s));
                    break;
                }
            }
        }
    }
    let mut m = manifest?;

    // Jar-in-jar: a mod jar bundles its libraries under META-INF/jars/. Those
    // nested jars PROVIDE modids (fabric-api ships 52: fabric-biome-api-v1,
    // fabric-api-base, ...; cardinal-components its sub-modules; Xaero's
    // xaerolib; Create its porting_lib_*). Other mods `depend` on those
    // nested ids — they are NOT separate Modrinth projects. Without harvesting
    // them the resolver reports them "missing" and (correctly-but-wrongly)
    // blocks every dependent. `jar_provided_only` recurses, so nesting
    // deeper than one level (Origins -> apoli -> calio) is harvested too.
    let nested: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
        .filter(|n| {
            n.starts_with("META-INF/jars/") && n.ends_with(".jar")
        })
        .collect();
    for name in nested {
        let mut buf = Vec::new();
        {
            let Ok(mut e) = archive.by_name(&name) else {
                continue;
            };
            if e.read_to_end(&mut buf).is_err() {
                continue;
            }
        }
        if let Some(inner) = jar_provided_only(&buf) {
            for p in inner {
                // Modid-level dedup (unchanged behaviour): a modid already
                // present — including from the outer jar itself — is not
                // re-added. We intentionally do NOT key on `(modid, version)`
                // here; a nested module appearing once is enough, and the
                // first-seen version (depth-first) wins, matching the old
                // first-seen-modid semantics.
                if !m.provided.iter().any(|(id, _)| id == &p.0) {
                    m.provided.push(p);
                }
            }
        }
    }
    Some(m)
}

/// The modids a (nested) jar PROVIDES — its `id` + `provides`, AND the same
/// for every jar IT bundles, recursively. Fabric jar-in-jar nests arbitrarily:
/// Origins bundles `apoli`, and `apoli` itself bundles `calio`, so a
/// single-level harvest misses `calio` and the resolver falsely reports it as
/// a missing required dependency. Depth is bounded as a guard against a
/// pathological / cyclic archive; a content-hash visited set skips
/// byte-identical bundled libraries (fabric-api and its ~52 sub-modules,
/// cardinal-components, cloth-config recur across many branches) so each blob
/// is decompressed once, and also gives natural termination independent of the
/// depth bound. A bundled library's own `depends` are still deliberately NOT
/// collected (they ship bundled alongside it).
fn jar_provided_only(bytes: &[u8]) -> Option<Vec<(String, String)>> {
    /// Real Fabric JIJ is 2-3 deep (Origins->apoli->calio); 6 is generous
    /// headroom while still terminating on a malformed/cyclic jar.
    const MAX_JIJ_DEPTH: u8 = 6;

    fn collect(
        bytes: &[u8],
        depth: u8,
        seen: &mut std::collections::HashSet<[u8; 32]>,
        out: &mut Vec<(String, String)>,
    ) {
        use sha2::{Digest, Sha256};
        if depth == 0 {
            return;
        }
        // Content-addressed memoisation: a byte-identical blob already
        // harvested has its provides + whole subtree in `out`; re-walking it
        // is wasted decompression. sha2 is already a dependency (no new
        // crate); a SHA-256 collision against another valid jar is not a
        // practical concern. First encounter does the full work; later ones
        // (a shared lib bundled in multiple branches) short-circuit.
        let key: [u8; 32] = Sha256::digest(bytes).into();
        if !seen.insert(key) {
            return;
        }
        let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(bytes)) else {
            return;
        };
        // This jar's own provides (fabric first, then forge/neoforge).
        let mut got = false;
        if let Ok(mut e) = archive.by_name("fabric.mod.json") {
            let mut s = String::new();
            if e.read_to_string(&mut s).is_ok() {
                if let Some(m) = parse_fabric_manifest(&s) {
                    out.extend(m.provided);
                    got = true;
                }
            }
        }
        if !got {
            for toml in ["META-INF/mods.toml", "META-INF/neoforge.mods.toml"] {
                if let Ok(mut e) = archive.by_name(toml) {
                    let mut s = String::new();
                    if e.read_to_string(&mut s).is_ok() {
                        out.extend(parse_forge_manifest(&s).provided);
                        break;
                    }
                }
            }
        }
        // Recurse into ITS bundled jars (the Origins -> apoli -> calio chain).
        let nested: Vec<String> = (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
            .filter(|n| n.starts_with("META-INF/jars/") && n.ends_with(".jar"))
            .collect();
        for name in nested {
            let mut buf = Vec::new();
            {
                let Ok(mut e) = archive.by_name(&name) else {
                    continue;
                };
                if e.read_to_end(&mut buf).is_err() {
                    continue;
                }
            }
            collect(&buf, depth - 1, seen, out);
        }
    }

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    collect(bytes, MAX_JIJ_DEPTH, &mut seen, &mut out);
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn is_builtin(modid: &str) -> bool {
    BUILTIN_MODIDS.contains(&modid)
}

/// Does a declared dependency range have NO upper bound? Tier 2 only acts on
/// open-ended ranges — the case where a mod said `>=X` (or `*`) and the
/// resolver is free to grab a far-newer, API-incompatible major (Create 6 vs
/// `create >=0.5.1-f`; Kotlin 2.x vs `fabric-language-kotlin >=1.9.2`).
///
/// Conservative by construction: anything that clearly carries an upper bound
/// (`<`, or a closed `[a,b)` / `(a,b]` interval with a right operand) or that
/// cannot be confidently classified returns `false`, so Tier 2 never downgrades
/// a dependency whose author actually pinned a ceiling.
pub fn is_open_ended_range(range: &str) -> bool {
    let s = range.trim();
    if s.is_empty() || s == "*" {
        return true;
    }
    if s.contains('<') {
        return false; // explicit upper bound
    }
    // Maven/forge interval form: open-ended iff the right operand is empty,
    // e.g. `[47,)` (>=47, no ceiling) is open; `[0.5,0.6)` is bounded.
    if let Some(first) = s.chars().next() {
        if first == '[' || first == '(' {
            if let Some(comma) = s.find(',') {
                let right = s[comma + 1..]
                    .trim_end_matches(|c| c == ')' || c == ']')
                    .trim();
                return right.is_empty();
            }
            return false; // single-value interval like `[1.0]` = pinned
        }
    }
    // Bare predicate forms: `>=x` / `>x` (no other comparator) are open;
    // an exact `=x` / plain `x` is a pin, not open.
    if s.starts_with(">=") || s.starts_with('>') {
        return true;
    }
    false
}

/// `fabric.mod.json`: `id` + `provides[]` are provided; `depends` is the hard
/// requirement map (value is a version-range string OR an array of them — OR
/// semantics; we keep the raw text for display + the conservative range check).
fn parse_fabric_manifest(body: &str) -> Option<JarManifest> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let id = v.get("id").and_then(|x| x.as_str())?.to_string();
    if id.is_empty() {
        return None;
    }
    let version = v
        .get("version")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    // The mod's `id` and every `provides[]` alias are made available by THIS
    // jar, so they ship at THIS jar's own declared `version`.
    let mut provided: Vec<(String, String)> = vec![(id, version.clone())];
    if let Some(arr) = v.get("provides").and_then(|x| x.as_array()) {
        provided.extend(
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| (s.to_string(), version.clone()))),
        );
    }

    // Parse a constraint map (`depends` / `breaks` / `conflicts`) the same
    // way: value is a range string OR an array of them (OR semantics; we keep
    // the raw text joined with " || " for display + the conservative range
    // check). `skip_builtins` is true only for `depends` (built-ins are always
    // present so a hard require on them is noise); a `breaks` against a
    // built-in is still meaningful and kept.
    let parse_constraints = |key: &str, skip_builtins: bool| -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(obj) = v.get(key).and_then(|x| x.as_object()) {
            for (modid, range) in obj {
                if skip_builtins && is_builtin(modid) {
                    continue;
                }
                let r = match range {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(a) => a
                        .iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" || "),
                    _ => "*".to_string(),
                };
                out.push((modid.clone(), r));
            }
        }
        out
    };

    let mut requires = parse_constraints("depends", true);
    requires.sort();

    // Negative constraints: Fabric `breaks` (hard incompat) plus the rarer
    // `conflicts` (soft, but Fabric still surfaces it) — merged, since both
    // mean "must not coexist with `modid` in this range".
    let mut breaks = parse_constraints("breaks", false);
    breaks.extend(parse_constraints("conflicts", false));
    breaks.sort();

    Some(JarManifest {
        provided,
        requires,
        breaks,
        version,
    })
}

/// Forge/NeoForge `mods.toml`: first `[[mods]] modId` is provided; each
/// `[[dependencies.<owner>]]` block with `mandatory = true` is a hard require
/// (its `modId` + `versionRange`). Hand-rolled (no toml crate), tolerant.
fn parse_forge_manifest(body: &str) -> JarManifest {
    // Forge `mods.toml` has no analogous incompatibility block on the 1.20.1
    // Fabric-first target (`type = "incompatible"` only exists 1.20.4+), so
    // `breaks` is intentionally left empty for Forge — see the `JarManifest`
    // doc. The Forge `version` is a `${file.jarVersion}` build placeholder so
    // every provided modid pairs with `""` (presence still recorded).
    let mut provided: Vec<String> = Vec::new();
    let mut requires: Vec<(String, String)> = Vec::new();
    let mut in_mods = false;
    let mut got_mod_id = false;

    // Current dependency block accumulator.
    let mut d_active = false;
    let mut d_modid = String::new();
    let mut d_range = String::new();
    let mut d_mandatory = true; // forge default is true

    let flush = |modid: &str, range: &str, mandatory: bool, out: &mut Vec<(String, String)>| {
        if mandatory && !modid.is_empty() && !is_builtin(modid) {
            out.push((modid.to_string(), if range.is_empty() { "*".into() } else { range.to_string() }));
        }
    };

    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("[[mods]]") {
            if d_active {
                flush(&d_modid, &d_range, d_mandatory, &mut requires);
                d_active = false;
            }
            in_mods = true;
            got_mod_id = false;
            continue;
        }
        if t.starts_with("[[dependencies.") || t.starts_with("[dependencies.") {
            if d_active {
                flush(&d_modid, &d_range, d_mandatory, &mut requires);
            }
            d_active = true;
            in_mods = false;
            d_modid = String::new();
            d_range = String::new();
            d_mandatory = true;
            continue;
        }
        if t.starts_with('[') {
            // Some other table: close any open dependency block.
            if d_active {
                flush(&d_modid, &d_range, d_mandatory, &mut requires);
                d_active = false;
            }
            in_mods = false;
            continue;
        }
        if in_mods && !got_mod_id {
            if let Some(rest) = t.strip_prefix("modId") {
                let id = toml_value(rest);
                if !id.is_empty() {
                    provided.push(id);
                    got_mod_id = true;
                }
            }
        } else if d_active {
            if let Some(rest) = t.strip_prefix("modId") {
                d_modid = toml_value(rest);
            } else if let Some(rest) = t.strip_prefix("mandatory") {
                d_mandatory = toml_value(rest) != "false";
            } else if let Some(rest) = t.strip_prefix("versionRange") {
                d_range = toml_value(rest);
            }
        }
    }
    if d_active {
        flush(&d_modid, &d_range, d_mandatory, &mut requires);
    }
    requires.sort();
    requires.dedup();
    // Forge `mods.toml` version is almost always a `${file.jarVersion}`
    // build-time placeholder, not a real version — leave empty (the exact-pin
    // audit then simply never fires for a Forge provider, which is fine: v1 is
    // Fabric-only anyway). Each provided modid therefore pairs with `""`.
    JarManifest {
        provided: provided.into_iter().map(|id| (id, String::new())).collect(),
        requires,
        breaks: Vec::new(),
        version: String::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build an in-memory jar (zip) with the given (name, bytes) entries,
    /// write it to `path`.
    fn make_jar(path: &Path, entries: &[(&str, &str)]) {
        let f = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (n, body) in entries {
            zw.start_file(*n, opts).unwrap();
            zw.write_all(body.as_bytes()).unwrap();
        }
        zw.finish().unwrap();
    }

    #[test]
    fn scan_jar_extracts_concrete_ids() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("testmod.jar");
        make_jar(
            &jar,
            &[
                (
                    "fabric.mod.json",
                    r#"{"id":"testmod","name":"Test Mod"}"#,
                ),
                ("data/testmod/recipes/cool/foo.json", "{}"),
                ("data/testmod/advancements/root.json", "{}"),
                ("data/testmod/worldgen/structure/bar.json", "{}"),
                ("data/testmod/worldgen/biome/glade.json", "{}"),
                ("data/testmod/tags/items/gems.json", "{}"),
                (
                    "assets/testmod/lang/en_us.json",
                    r#"{"item.testmod.ruby":"Ruby","block.testmod.ruby_ore":"Ruby Ore","entity.testmod.golem":"Stone Golem"}"#,
                ),
            ],
        );
        let mut v = RegistryVocab::default();
        assert!(scan_one_jar(&jar, &mut v));

        // recipe id keeps its sub-path with slashes.
        assert!(v.recipe_ids.contains("testmod:cool/foo"));
        assert!(v.advancements.contains("testmod:root"));
        assert!(v.structures.contains("testmod:bar"));
        assert!(v.biomes.contains("testmod:glade"));
        assert!(v.tags.contains("testmod:gems"));
        assert!(v.items.contains("testmod:ruby"));
        assert!(v.blocks.contains("testmod:ruby_ore"));
        assert!(v.entities.contains("testmod:golem"));
        assert_eq!(v.labels.get("testmod:ruby").map(String::as_str), Some("Ruby"));
        assert_eq!(v.mod_meta.len(), 1);
        assert_eq!(v.mod_meta[0].id, "testmod");
        assert_eq!(v.mod_meta[0].name, "Test Mod");
    }

    // ---- class -> mod attribution (Fix 4) -------------------------------
    fn pinned(path: &str) -> crate::instance::PinnedMod {
        crate::instance::PinnedMod {
            project_id: String::new(),
            version_id: String::new(),
            name: path.to_string(),
            path: path.to_string(),
            sha1: String::new(),
            sha512: String::new(),
            download_url: String::new(),
            file_size: 0,
            client_side: "required".into(),
            server_side: "required".into(),
        }
    }
    fn inst_with(mods: Vec<crate::instance::PinnedMod>) -> Instance {
        Instance {
            id: "t".into(),
            name: "T".into(),
            mc_version: "1.20.1".into(),
            loader: "fabric".into(),
            loader_version: "0".into(),
            created: String::new(),
            last_played: None,
            mods,
            roots: vec![],
        }
    }

    /// Real vinery shape: doapi IS installed (ships `de/cristelknight/doapi/…`)
    /// but lacks the specific missing class → attributed to doapi (version skew,
    /// repin), NOT a removal of unrelated mods.
    #[test]
    fn attribution_present_jar_owns_missing_class() {
        let dir = tempfile::tempdir().unwrap();
        make_jar(
            &dir.path().join("doapi.jar"),
            &[
                ("fabric.mod.json", r#"{"id":"doapi"}"#),
                ("de/cristelknight/doapi/DoApi.class", "x"),
                ("de/cristelknight/doapi/registry/Boats.class", "x"),
            ],
        );
        let inst = inst_with(vec![pinned("doapi.jar")]);
        let owner = attribute_class_to_mod(
            &inst,
            dir.path(),
            "de/cristelknight/doapi/DoApiExpectPlatform", // the MISSING class
        )
        .expect("doapi owns the package");
        assert_eq!(owner.mod_id, "doapi");
        assert!(owner.depth >= 3);
    }

    /// Real another_furniture / create_dd shape: a present `create` jar owns
    /// `com/simibubi/create/…`; the missing inner API class attributes to it.
    #[test]
    fn attribution_create_api_class() {
        let dir = tempfile::tempdir().unwrap();
        make_jar(
            &dir.path().join("create.jar"),
            &[
                ("fabric.mod.json", r#"{"id":"create"}"#),
                ("com/simibubi/create/Create.class", "x"),
                ("com/simibubi/create/content/Stuff.class", "x"),
            ],
        );
        let inst = inst_with(vec![pinned("create.jar")]);
        let owner = attribute_class_to_mod(
            &inst,
            dir.path(),
            "com.simibubi.create.api.behaviour.interaction.MovingInteractionBehaviour",
        )
        .expect("create owns com.simibubi.create");
        assert_eq!(owner.mod_id, "create");
    }

    /// Real villagernames shape: the consumer expects `dev.mrsterner.guard…`
    /// but the installed Guard Villagers ships the renamed `dev.sterner.guard…`
    /// package → NO present jar owns it → `None` (absent ⇒ add/swap, not repin).
    #[test]
    fn attribution_absent_when_package_renamed() {
        let dir = tempfile::tempdir().unwrap();
        make_jar(
            &dir.path().join("guardvillagers.jar"),
            &[
                ("fabric.mod.json", r#"{"id":"guardvillagers"}"#),
                ("dev/sterner/guardvillagers/GuardEntity.class", "x"),
            ],
        );
        let inst = inst_with(vec![pinned("guardvillagers.jar")]);
        let owner = attribute_class_to_mod(
            &inst,
            dir.path(),
            "dev/mrsterner/guardvillagers/common/entity/GuardEntity",
        );
        assert!(owner.is_none(), "renamed package must not match: {owner:?}");
    }

    /// The DEEPEST package match wins over a shallow shaded/relocated one.
    #[test]
    fn attribution_deepest_match_wins() {
        let dir = tempfile::tempdir().unwrap();
        make_jar(
            &dir.path().join("shallow.jar"),
            &[
                ("fabric.mod.json", r#"{"id":"shallow"}"#),
                ("com/foo/Unrelated.class", "x"), // matches only com/foo
            ],
        );
        make_jar(
            &dir.path().join("deep.jar"),
            &[
                ("fabric.mod.json", r#"{"id":"deep"}"#),
                ("com/foo/bar/baz/Thing.class", "x"),
            ],
        );
        let inst = inst_with(vec![pinned("shallow.jar"), pinned("deep.jar")]);
        let owner =
            attribute_class_to_mod(&inst, dir.path(), "com.foo.bar.baz.Missing")
                .expect("deep jar owns the deeper package");
        assert_eq!(owner.mod_id, "deep");
    }

    /// A bare two-segment class name has no package to own → `None` (never lets
    /// a shallow match claim ownership of everything).
    #[test]
    fn attribution_two_segment_name_is_none() {
        let dir = tempfile::tempdir().unwrap();
        make_jar(
            &dir.path().join("m.jar"),
            &[("fabric.mod.json", r#"{"id":"m"}"#), ("Foo/Bar.class", "x")],
        );
        let inst = inst_with(vec![pinned("m.jar")]);
        assert!(attribute_class_to_mod(&inst, dir.path(), "Foo.Bar").is_none());
    }

    /// Falls back to the filename namespace when a jar has no fabric.mod.json.
    #[test]
    fn attribution_falls_back_to_filename_namespace() {
        let dir = tempfile::tempdir().unwrap();
        make_jar(
            &dir.path().join("adorn-1.2.3.jar"),
            &[("juuxel/adorn/block/Variant.class", "x")],
        );
        let inst = inst_with(vec![pinned("adorn-1.2.3.jar")]);
        let owner = attribute_class_to_mod(
            &inst,
            dir.path(),
            "juuxel/adorn/block/variant/BlockVariantSets",
        )
        .expect("matches by package even without fabric.mod.json");
        assert_eq!(owner.mod_id, "adorn"); // filename_namespace("adorn-1.2.3.jar")
    }

    #[test]
    fn harvest_lang_never_pollutes_id_buckets_with_nested_keys() {
        // The exact real-world shape that shipped the Stardew Hollow crash:
        // `comforts` has color-variant items + a nested tooltip lang key.
        let body = r#"{
            "item.comforts.white_sleeping_bag": "White Sleeping Bag",
            "item.comforts.sleeping_bag.auto_use.tooltip": "Sneak to skip",
            "block.comforts.rope": "Rope",
            "block.comforts.rope.placement.tooltip": "Place on a block",
            "entity.comforts.cat.subtitle": "Cat purrs"
        }"#;
        let mut v = RegistryVocab::default();
        harvest_lang("comforts", body, &mut v);

        // Flat ids are harvested as before.
        assert!(v.items.contains("comforts:white_sleeping_bag"));
        assert!(v.blocks.contains("comforts:rope"));
        // Nested lang sub-keys NEVER enter an id bucket (the bug).
        assert!(
            !v.items.contains("comforts:sleeping_bag.auto_use.tooltip"),
            "tooltip lang key must not masquerade as an item id"
        );
        assert!(!v.blocks.contains("comforts:rope.placement.tooltip"));
        assert!(!v.entities.contains("comforts:cat.subtitle"));
        // No item id called `comforts:sleeping_bag` exists at all (only the
        // color variants do) — the polluted entry must be gone entirely.
        assert!(!v
            .items
            .iter()
            .any(|i| i.starts_with("comforts:sleeping_bag.")));
        // Labels for nested keys are dropped too (the flat id keeps its own).
        assert_eq!(
            v.labels.get("comforts:white_sleeping_bag").map(String::as_str),
            Some("White Sleeping Bag")
        );
        assert!(!v
            .labels
            .contains_key("comforts:sleeping_bag.auto_use.tooltip"));
    }

    #[test]
    fn scan_handles_1_21_singular_folders() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("m.jar");
        make_jar(
            &jar,
            &[
                ("data/m/recipe/x.json", "{}"),       // 1.21 singular
                ("data/m/advancement/y.json", "{}"),  // 1.21 singular
            ],
        );
        let mut v = RegistryVocab::default();
        assert!(scan_one_jar(&jar, &mut v));
        assert!(v.recipe_ids.contains("m:x"));
        assert!(v.advancements.contains("m:y"));
    }

    #[test]
    fn forge_mods_toml_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("f.jar");
        make_jar(
            &jar,
            &[(
                "META-INF/mods.toml",
                "[[mods]]\nmodId=\"forgey\"\ndisplayName=\"Forgey Mod\"\n",
            )],
        );
        let mut v = RegistryVocab::default();
        assert!(scan_one_jar(&jar, &mut v));
        assert_eq!(v.mod_meta.len(), 1);
        assert_eq!(v.mod_meta[0].id, "forgey");
        assert_eq!(v.mod_meta[0].name, "Forgey Mod");
    }

    #[test]
    fn unreadable_jar_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("not.jar");
        std::fs::write(&bogus, b"not a zip").unwrap();
        let mut v = RegistryVocab::default();
        assert!(!scan_one_jar(&bogus, &mut v));
        assert!(v.is_empty());
    }

    #[test]
    fn vocab_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("d.jar");
        make_jar(
            &jar,
            &[
                ("data/d/recipes/a.json", "{}"),
                ("data/d/recipes/b.json", "{}"),
                (
                    "assets/d/lang/en_us.json",
                    r#"{"item.d.z":"Z","item.d.a":"A"}"#,
                ),
            ],
        );
        let mut v1 = RegistryVocab::default();
        scan_one_jar(&jar, &mut v1);
        let mut v2 = RegistryVocab::default();
        scan_one_jar(&jar, &mut v2);
        assert_eq!(
            serde_json::to_string(&v1).unwrap(),
            serde_json::to_string(&v2).unwrap()
        );
    }

    #[test]
    fn filename_namespace_matches_curator_rule() {
        assert_eq!(filename_namespace("mods/sodium-fabric-0.5.jar"), "sodium");
        assert_eq!(filename_namespace("mods/create_1.20.1.jar"), "create");
        assert_eq!(filename_namespace("cobblemon.jar"), "cobblemon");
    }

    /// Build a jar (zip) in memory and return its bytes — exercises the exact
    /// `&[u8]` path `jar_manifest` consumes (no filesystem).
    fn jar_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::SimpleFileOptions =
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
            for (n, body) in entries {
                zw.start_file(*n, opts).unwrap();
                zw.write_all(body.as_bytes()).unwrap();
            }
            zw.finish().unwrap();
        }
        buf
    }

    #[test]
    fn fabric_manifest_extracts_real_spectrum_shape() {
        // The exact shape that broke the resolver: Modrinth listed ZERO deps
        // but the jar declares them. Built-ins (fabric-api/fabricloader/
        // minecraft/java) must be filtered out of `requires`.
        let bytes = jar_bytes(&[(
            "fabric.mod.json",
            r#"{"id":"spectrum","provides":["spectrum_api"],
                "depends":{"fabricloader":">=0.15.6","fabric-api":">=0.92.2",
                "minecraft":">=1.20.1","java":">=17","revelationary":"*",
                "cloth-config":"*","modonomicon":">=1.77.3","trinkets":"*"}}"#,
        )]);
        let m = jar_manifest(&bytes).expect("fabric manifest");
        // `id` + each `provides[]` alias, both paired with the host's own
        // version (absent here -> ""). Presence still readable via `.0`.
        assert_eq!(
            m.provided,
            vec![
                ("spectrum".to_string(), String::new()),
                ("spectrum_api".to_string(), String::new()),
            ]
        );
        // sorted, builtins removed
        assert_eq!(
            m.requires,
            vec![
                ("cloth-config".into(), "*".into()),
                ("modonomicon".into(), ">=1.77.3".into()),
                ("revelationary".into(), "*".into()),
                ("trinkets".into(), "*".into()),
            ]
        );
    }

    #[test]
    fn fabric_manifest_array_range_joined() {
        let bytes = jar_bytes(&[(
            "fabric.mod.json",
            r#"{"id":"m","depends":{"lib":[">=1.0",">=2.0"]}}"#,
        )]);
        let m = jar_manifest(&bytes).unwrap();
        assert_eq!(m.requires, vec![("lib".into(), ">=1.0 || >=2.0".into())]);
    }

    #[test]
    fn forge_mods_toml_mandatory_only() {
        let bytes = jar_bytes(&[(
            "META-INF/mods.toml",
            r#"
[[mods]]
modId="coolmod"
[[dependencies.coolmod]]
modId="forge"
mandatory=true
versionRange="[47,)"
[[dependencies.coolmod]]
modId="jei"
mandatory=false
versionRange="*"
[[dependencies.coolmod]]
modId="createbig"
mandatory=true
versionRange="[0.5,0.6)"
"#,
        )]);
        let m = jar_manifest(&bytes).unwrap();
        // Forge provided modids pair with "" (jarVersion placeholder).
        assert!(m
            .provided
            .iter()
            .any(|(id, v)| id == "coolmod" && v.is_empty()));
        // Forge has no incompat block on the 1.20.1 target -> breaks empty.
        assert!(m.breaks.is_empty());
        // `forge` is built-in -> filtered; optional `jei` -> skipped.
        assert_eq!(m.requires, vec![("createbig".into(), "[0.5,0.6)".into())]);
    }

    #[test]
    fn non_jar_or_no_manifest_is_none() {
        assert!(jar_manifest(b"not a zip").is_none());
        let empty = jar_bytes(&[("README.txt", "hi")]);
        assert!(jar_manifest(&empty).is_none());
    }

    /// Build a jar that bundles `inner` bytes at `META-INF/jars/<name>`.
    fn jar_with_nested(outer_fmj: &str, name: &str, inner: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let o: zip::write::SimpleFileOptions =
                zip::write::SimpleFileOptions::default();
            zw.start_file("fabric.mod.json", o).unwrap();
            zw.write_all(outer_fmj.as_bytes()).unwrap();
            zw.start_file(format!("META-INF/jars/{name}"), o).unwrap();
            zw.write_all(inner).unwrap();
            zw.finish().unwrap();
        }
        buf
    }

    /// The reported false-positive: fabric-api exposes only `fabric-api` +
    /// `fabric` at the top level but JIJ-bundles ~50 `fabric-*-v1` modules.
    /// Those nested modids MUST be harvested or every dependent (Create,
    /// Sodium, …) is wrongly reported as needing a missing dependency.
    /// Convenience: does `provided` contain this modid at any version?
    /// Round-trip check that presence is still readable via `.0`.
    fn has_id(provided: &[(String, String)], modid: &str) -> bool {
        provided.iter().any(|(id, _)| id == modid)
    }
    /// Find the captured version for a modid (None if absent).
    fn ver_of<'a>(provided: &'a [(String, String)], modid: &str) -> Option<&'a str> {
        provided
            .iter()
            .find(|(id, _)| id == modid)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn nested_jij_provided_modids_are_harvested() {
        // Mimic fabric-api bundling fabric-lifecycle-events-v1 at a CONCRETE
        // version (the real root-cause shape: a dependent ranges on this
        // bundled module's version, which was previously dropped).
        let inner = jar_bytes(&[(
            "fabric.mod.json",
            r#"{"id":"fabric-lifecycle-events-v1","version":"2.2.21+afsd2024",
                "provides":["fabric-lifecycle-events"]}"#,
        )]);
        let outer = jar_with_nested(
            r#"{"id":"fabric-api","version":"0.92.2+1.20.1","provides":["fabric"]}"#,
            "fabric-lifecycle-events-v1-2.2.21+afsd2024.jar",
            &inner,
        );
        let m = jar_manifest(&outer).expect("manifest");
        // Round-trip: presence still holds via `.0`.
        assert!(has_id(&m.provided, "fabric-api"));
        assert!(has_id(&m.provided, "fabric"));
        // Outer jar's own id + alias carry the OUTER jar's own version.
        assert_eq!(ver_of(&m.provided, "fabric-api"), Some("0.92.2+1.20.1"));
        assert_eq!(ver_of(&m.provided, "fabric"), Some("0.92.2+1.20.1"));
        // The fix: the JIJ-bundled module id is now satisfied AND its OWN
        // nested version is captured (not just the modid).
        assert!(
            m.provided.contains(&(
                "fabric-lifecycle-events-v1".to_string(),
                "2.2.21+afsd2024".to_string()
            )),
            "nested JIJ modid+version must be captured, got {:?}",
            m.provided
        );
        // The nested module's `provides[]` alias carries the NESTED version.
        assert_eq!(
            ver_of(&m.provided, "fabric-lifecycle-events"),
            Some("2.2.21+afsd2024")
        );
    }

    /// The real Origins case: Origins `depends` on `calio`, does NOT bundle it
    /// at level 1, but bundles `apoli`, and `apoli` bundles `calio`. A
    /// single-level harvest misses `calio` -> false missing-dependency ->
    /// assembler blocks. The recursive harvest must surface `calio`.
    #[test]
    fn deep_jij_origins_apoli_calio_is_harvested() {
        // Each nested module declares its OWN version; the harvest must pair
        // each modid with the version from ITS fabric.mod.json (not the root).
        let calio = jar_bytes(&[(
            "fabric.mod.json",
            r#"{"id":"calio","version":"1.11.2+mc.1.20.x"}"#,
        )]);
        let apoli_with_calio = jar_with_nested(
            r#"{"id":"apoli","version":"2.9.2+mc.1.20.x","depends":{"calio":">=1.11.0"}}"#,
            "calio-1.11.2+mc.1.20.x.jar",
            &calio,
        );
        let origins = jar_with_nested(
            r#"{"id":"origins","version":"1.10.2+mc.1.20.x","depends":{"apoli":">=2.9.0","calio":">=1.11.0"}}"#,
            "apoli-2.9.2+mc.1.20.x.jar",
            &apoli_with_calio,
        );
        let m = jar_manifest(&origins).expect("manifest");
        // Round-trip presence still holds via `.0`.
        assert!(has_id(&m.provided, "origins"));
        assert!(has_id(&m.provided, "apoli"));
        assert!(
            has_id(&m.provided, "calio"),
            "level-2 JIJ modid `calio` must be harvested, got {:?}",
            m.provided
        );
        // Each modid pairs with the version from ITS OWN manifest.
        assert_eq!(ver_of(&m.provided, "origins"), Some("1.10.2+mc.1.20.x"));
        assert_eq!(ver_of(&m.provided, "apoli"), Some("2.9.2+mc.1.20.x"));
        assert_eq!(
            ver_of(&m.provided, "calio"),
            Some("1.11.2+mc.1.20.x"),
            "level-2 nested version must be the nested jar's own, got {:?}",
            m.provided
        );
    }

    /// The dedup must not DROP a provide: a library bundled in two branches
    /// (byte-identical, different filenames) is harvested once but must still
    /// contribute its modid. Guards the "skip == lose a provide" risk.
    #[test]
    fn duplicate_bundled_lib_still_provided_once() {
        let lib = jar_bytes(&[(
            "fabric.mod.json",
            r#"{"id":"sharedlib","version":"3.1.0"}"#,
        )]);
        let outer = {
            let mut buf = Vec::new();
            {
                let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
                let o: zip::write::SimpleFileOptions =
                    zip::write::SimpleFileOptions::default();
                zw.start_file("fabric.mod.json", o).unwrap();
                zw.write_all(br#"{"id":"outermod","depends":{"sharedlib":"*"}}"#)
                    .unwrap();
                zw.start_file("META-INF/jars/sharedlib-a.jar", o).unwrap();
                zw.write_all(&lib).unwrap();
                zw.start_file("META-INF/jars/sharedlib-b.jar", o).unwrap();
                zw.write_all(&lib).unwrap();
                zw.finish().unwrap();
            }
            buf
        };
        let m = jar_manifest(&outer).expect("manifest");
        assert!(has_id(&m.provided, "outermod"));
        assert!(
            has_id(&m.provided, "sharedlib"),
            "dedup must keep the provide, got {:?}",
            m.provided
        );
        // SHA-256 byte-identical dedup: contributed exactly ONCE (modid-level
        // dedup at the push site), and with its own nested version.
        assert_eq!(
            m.provided.iter().filter(|(id, _)| id == "sharedlib").count(),
            1,
            "byte-identical bundled lib must contribute once, got {:?}",
            m.provided
        );
        assert_eq!(ver_of(&m.provided, "sharedlib"), Some("3.1.0"));
    }

    // -----------------------------------------------------------------------
    // Step 2 — negative constraints (`breaks` / `conflicts`)
    // -----------------------------------------------------------------------

    /// Realistic Immersive-Portals-style `breaks` (string range) PLUS a
    /// `conflicts` block: both must land in `JarManifest.breaks` parsed the
    /// same way `depends` is (raw range text preserved). A `breaks` against a
    /// built-in is kept (unlike `depends`, where built-ins are noise).
    #[test]
    fn fabric_breaks_and_conflicts_captured() {
        let bytes = jar_bytes(&[(
            "fabric.mod.json",
            r#"{"id":"immersive_portals","version":"3.0",
                "breaks":{"sodium":"<0.5.13 || >0.5.13","fabric":">=999"},
                "conflicts":{"optifabric":"*"}}"#,
        )]);
        let m = jar_manifest(&bytes).expect("fabric manifest");
        // `breaks` + `conflicts` merged, sorted, range text verbatim.
        assert!(
            m.breaks.contains(&(
                "sodium".to_string(),
                "<0.5.13 || >0.5.13".to_string()
            )),
            "breaks must capture the raw range, got {:?}",
            m.breaks
        );
        // built-in `fabric` is NOT filtered out of `breaks` (it is from
        // `depends`); a hard incompat with a builtin is meaningful.
        assert!(m.breaks.iter().any(|(id, _)| id == "fabric"));
        // `conflicts` folded into the same `breaks` vec.
        assert!(m
            .breaks
            .contains(&("optifabric".to_string(), "*".to_string())));
        // depends untouched; this jar has none -> requires empty.
        assert!(m.requires.is_empty());
    }

    /// `breaks` array form must join with `" || "` exactly like `depends`.
    #[test]
    fn fabric_breaks_array_range_joined() {
        let bytes = jar_bytes(&[(
            "fabric.mod.json",
            r#"{"id":"m","breaks":{"badlib":["<1.0",">2.0"]}}"#,
        )]);
        let m = jar_manifest(&bytes).unwrap();
        assert_eq!(
            m.breaks,
            vec![("badlib".to_string(), "<1.0 || >2.0".to_string())]
        );
    }

    /// No negative constraints -> `breaks` is empty (does not spuriously
    /// capture `depends`).
    #[test]
    fn fabric_no_breaks_is_empty() {
        let bytes = jar_bytes(&[(
            "fabric.mod.json",
            r#"{"id":"m","depends":{"lib":">=1.0"}}"#,
        )]);
        let m = jar_manifest(&bytes).unwrap();
        assert!(m.breaks.is_empty());
        assert_eq!(m.requires, vec![("lib".into(), ">=1.0".into())]);
    }

    // -----------------------------------------------------------------------
    // Step 2 — real committed fixture jars (apoli/origins have real JIJ)
    // -----------------------------------------------------------------------

    /// `tests/fixtures/real/apoli-2.9.2.jar` JIJ-bundles `calio` and
    /// `playerabilitylib` at concrete real versions — assert the harvest
    /// captures the modid AND its real nested version, on a real jar.
    #[test]
    fn real_apoli_jar_captures_nested_module_versions() {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/real/apoli-2.9.2.jar"),
        )
        .expect("committed apoli fixture present");
        let m = jar_manifest(&bytes).expect("apoli manifest");
        // Outer jar's own id + its real version.
        assert_eq!(ver_of(&m.provided, "apoli"), Some("2.9.2+mc.1.20.x"));
        // Level-1 JIJ: calio at its real bundled version.
        assert_eq!(
            ver_of(&m.provided, "calio"),
            Some("1.11.2+mc.1.20.x"),
            "real nested calio version must be captured, got {:?}",
            m.provided
        );
        // PlayerAbilityLib's modid is `playerabilitylib`, bundled at 1.8.0.
        assert_eq!(ver_of(&m.provided, "playerabilitylib"), Some("1.8.0"));
    }

    /// `tests/fixtures/real/origins-1.10.2.jar` bundles `apoli`, and `apoli`
    /// itself bundles `calio` — a real-data DEEP (level-2) JIJ version capture.
    #[test]
    fn real_origins_jar_captures_deep_nested_version() {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/real/origins-1.10.2.jar"),
        )
        .expect("committed origins fixture present");
        let m = jar_manifest(&bytes).expect("origins manifest");
        assert_eq!(ver_of(&m.provided, "origins"), Some("1.10.2+mc.1.20.x"));
        // Level-1: apoli (bundled in origins).
        assert_eq!(ver_of(&m.provided, "apoli"), Some("2.9.2+mc.1.20.x"));
        // Level-2: calio (origins -> apoli -> calio) — the deep harvest must
        // surface its real version, not just its presence.
        assert_eq!(
            ver_of(&m.provided, "calio"),
            Some("1.11.2+mc.1.20.x"),
            "deep (level-2) real nested calio version must be captured, got {:?}",
            m.provided
        );
    }

    // -----------------------------------------------------------------------
    // Slice 1.5 — reconcile_with_launch_dump / reconcile_scan
    // -----------------------------------------------------------------------

    /// Write `<root>/dump/<rel>/<file>.json` = `body`.
    fn write_dump(root: &Path, rel: &str, file: &str, body: &str) {
        let dir = root.join("dump").join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{file}.json")), body).unwrap();
    }

    #[test]
    fn reconcile_unions_dumped_ids_into_right_sets() {
        let d = tempfile::tempdir().unwrap();
        write_dump(d.path(), "item", "minecraft", r#"["minecraft:stone","minecraft:diamond"]"#);
        write_dump(d.path(), "entity_type", "minecraft", r#"["minecraft:creeper"]"#);
        write_dump(d.path(), "block", "minecraft", r#"["minecraft:stone"]"#);
        write_dump(d.path(), "worldgen/biome", "minecraft", r#"["minecraft:plains"]"#);
        write_dump(d.path(), "structure", "minecraft", r#"["minecraft:fortress"]"#);
        write_dump(d.path(), "recipe", "minecraft", r#"["minecraft:stick"]"#);
        write_dump(d.path(), "tags", "minecraft", r#"["minecraft:logs"]"#);
        // An unknown registry dir is ignored, not an error.
        write_dump(d.path(), "fluid", "minecraft", r#"["minecraft:water"]"#);

        // Pre-seed the vocab: union, do not replace.
        let mut seed = RegistryVocab::default();
        seed.items.insert("cobblemon:poke_ball".to_string());

        let out = reconcile_with_launch_dump(seed, Some(d.path()));
        assert!(out.items.contains("minecraft:stone"));
        assert!(out.items.contains("minecraft:diamond"));
        assert!(out.items.contains("cobblemon:poke_ball")); // pre-seed preserved
        assert!(out.entities.contains("minecraft:creeper"));
        assert!(out.blocks.contains("minecraft:stone"));
        assert!(out.biomes.contains("minecraft:plains"));
        assert!(out.structures.contains("minecraft:fortress"));
        assert!(out.recipe_ids.contains("minecraft:stick"));
        assert!(out.tags.contains("minecraft:logs"));
        // Unknown registry dir contributed nothing anywhere.
        assert!(!out.items.contains("minecraft:water"));
    }

    #[test]
    fn reconcile_none_is_identity() {
        let mut seed = RegistryVocab::default();
        seed.items.insert("minecraft:diamond".to_string());
        let out = reconcile_with_launch_dump(seed.clone(), None);
        assert_eq!(out.items, seed.items);
        assert!(out.is_empty() == seed.is_empty());
    }

    #[test]
    fn reconcile_is_deterministic_on_rerun() {
        let d = tempfile::tempdir().unwrap();
        write_dump(d.path(), "item", "minecraft", r#"["minecraft:stone","minecraft:diamond"]"#);
        let a = reconcile_with_launch_dump(RegistryVocab::default(), Some(d.path()));
        let b = reconcile_with_launch_dump(RegistryVocab::default(), Some(d.path()));
        assert_eq!(a.items, b.items);
        // Re-running on top of an already-reconciled vocab is idempotent.
        let c = reconcile_with_launch_dump(a.clone(), Some(d.path()));
        assert_eq!(a.items, c.items);
    }

    #[test]
    fn reconcile_skips_malformed_file_but_parses_siblings() {
        let d = tempfile::tempdir().unwrap();
        write_dump(d.path(), "item", "minecraft", r#"{ this is not an array "#);
        write_dump(d.path(), "item", "create", r#"["create:cogwheel"]"#);
        let out = reconcile_with_launch_dump(RegistryVocab::default(), Some(d.path()));
        // Malformed minecraft.json skipped; the create.json sibling still lands.
        assert!(out.items.contains("create:cogwheel"));
        assert!(!out.items.iter().any(|i| i.starts_with("minecraft:")));
    }

    #[test]
    fn reconcile_missing_dump_dir_is_noop() {
        let d = tempfile::tempdir().unwrap();
        // No `dump/` subdir at all.
        let out = reconcile_with_launch_dump(RegistryVocab::default(), Some(d.path()));
        assert!(out.is_empty());
    }

    #[test]
    fn reconcile_scan_trims_unscanned_and_stamps_source() {
        let d = tempfile::tempdir().unwrap();
        write_dump(d.path(), "item", "minecraft", r#"["minecraft:stone"]"#);
        write_dump(d.path(), "item", "create", r#"["create:cogwheel"]"#);

        let mut scan = ScanResult::default();
        scan.unscanned.insert("minecraft".to_string());
        scan.unscanned.insert("create".to_string());
        scan.unscanned.insert("offlinemod".to_string()); // not dumped → stays
        scan.mod_set_key = "KEY".to_string();

        let out = reconcile_scan(scan, Some(d.path()));
        assert_eq!(out.source, ScanSource::DumpReconciled);
        assert!(out.vocab.items.contains("minecraft:stone"));
        assert!(out.vocab.items.contains("create:cogwheel"));
        // minecraft + create were dumped → removed from unscanned.
        assert!(!out.unscanned.contains("minecraft"));
        assert!(!out.unscanned.contains("create"));
        // A namespace the dump did not cover is left as-is.
        assert!(out.unscanned.contains("offlinemod"));
        // mod_set_key is preserved (cache still keyed to the same pin set).
        assert_eq!(out.mod_set_key, "KEY");
    }

    #[test]
    fn reconcile_scan_none_stays_static_identity() {
        let mut scan = ScanResult::default();
        scan.unscanned.insert("minecraft".to_string());
        scan.source = ScanSource::DumpReconciled; // even a stale value
        let out = reconcile_scan(scan, None);
        assert_eq!(out.source, ScanSource::Static);
        assert!(out.unscanned.contains("minecraft"));
    }

    /// Serde back-compat: a pre-Slice-1.5 `anvil-registry.json` has NO `source`
    /// key. It must still deserialize, defaulting to `ScanSource::Static`.
    #[test]
    fn old_scan_json_without_source_loads_as_static() {
        let old = r#"{
            "vocab": {
                "items": ["minecraft:diamond"],
                "entities": [], "blocks": [], "advancements": [],
                "structures": [], "biomes": [], "tags": [], "recipe_ids": [],
                "labels": {}, "mod_meta": []
            },
            "unscanned": ["minecraft"],
            "mod_set_key": "ABC123"
        }"#;
        let r: ScanResult = serde_json::from_str(old).expect("old cache deserializes");
        assert_eq!(r.source, ScanSource::Static);
        assert_eq!(r.mod_set_key, "ABC123");
        assert!(r.vocab.items.contains("minecraft:diamond"));
    }
}
