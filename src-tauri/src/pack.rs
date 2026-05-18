//! Pack engine: dependency-closure resolution, `validate_pack`, and standard
//! `.mrpack` emission. Pure logic with no network or filesystem coupling in the
//! hot path (the resolver takes a dependency-lookup closure so it is unit
//! testable offline) — this is the module that must be `cargo test` green.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;

/// A mod selected for a pack, with the metadata needed to validate it and to
/// write a `.mrpack` entry. Built from Modrinth project + version data.
#[derive(Debug, Clone, PartialEq)]
pub struct ModEntry {
    pub project_id: String,
    pub version_id: String,
    /// Final path inside the instance, e.g. `mods/sodium-fabric.jar`.
    pub path: String,
    pub sha1: String,
    pub sha512: String,
    pub downloads: Vec<String>,
    pub file_size: u64,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    /// "required" | "optional" | "unsupported"
    pub client_side: String,
    pub server_side: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ValidationIssue {
    IncompatibleGameVersion { project_id: String, want: String },
    IncompatibleLoader { project_id: String, want: String },
    /// Cannot run on either side — the mod is dead weight / a packaging error.
    UnsupportedOnBothSides { project_id: String },
    DuplicateProject { project_id: String },
    /// Whitelisted-domain rule of the .mrpack spec: downloads must be HTTPS.
    InsecureDownloadUrl { project_id: String, url: String },
    /// A `required` dependency could not be resolved to a version compatible
    /// with the pack's Minecraft version + loader (no matching version, the
    /// project 404s, or Modrinth was unreachable). `reason` carries the cause.
    UnresolvedRequiredDependency {
        needed_by: String,
        missing_project_id: String,
        reason: String,
    },
    /// A mod in the pack declares another project in the pack as
    /// `incompatible`. Both cannot ship together.
    IncompatibleDependencyPresent {
        holder: String,
        conflicts_with: String,
    },
    /// Exact-pin audit: `addon` declares an EXACT-version dependency that the
    /// present provider does not satisfy (e.g. createairfabric needs Create
    /// `0.5.1-j-build.1631` but the pack has `6.0.8.1+build.1744`). The addon
    /// is a leaf (nothing depends on it) so it was auto-dropped from the pack
    /// rather than shipping a guaranteed launch crash. INFORMATIONAL — this
    /// does NOT block assembly (the conflicting mod is already removed).
    IncompatibleAddonDropped {
        addon: String,
        requires: String,
        present: String,
    },
    /// HARD, BLOCKING. A mod (`requirer`, e.g. InventoryProfilesNext) declares
    /// `fabric-language-kotlin >=…+kotlin.<needs_major>` but the FLK build the
    /// resolver settled on embeds Kotlin major `present > needs_major`, AND —
    /// after the floor-lookahead pool expansion fetched FLK's FULL version
    /// list — NO compatible FLK build with Kotlin major ≤ `needs_major`
    /// exists. This is genuinely unsatisfiable (not merely "pool not yet
    /// searched"): shipping it is a guaranteed `KotlinReflectionInternalError`
    /// client crash at world-join, so assembly must block.
    KotlinMajorUnsatisfiable {
        requirer: String,
        needs_major: u32,
        present: u32,
    },
    /// HARD, BLOCKING. General version-constraint violation across the
    /// resolved set, evaluated with the real Fabric matcher (`crate::version`)
    /// over the real jar manifests (incl. JIJ-bundled sub-module versions):
    /// either a `depends` range a present provider does not satisfy
    /// (`kind: Depends`, e.g. Indium needs `sodium >=0.5.11 <0.6`, present
    /// 0.5.8), or a `breaks`/`conflicts` range a present version DOES fall in
    /// (`kind: Breaks`, e.g. Immersive Portals breaks sodium ≠ 0.5.13). This
    /// is the general subsumption of the FLK-specific Kotlin gate — it is the
    /// class that produced "Incompatible mods found" at launch.
    VersionConstraintUnsatisfied {
        holder: String,
        modid: String,
        want: String,
        have: String,
        kind: ConstraintKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ConstraintKind {
    /// A `depends` range the present provider version does not satisfy.
    Depends,
    /// A `breaks`/`conflicts` range the present version falls inside.
    Breaks,
}

/// Gate that must pass before a pack is ever presented as "assembled".
/// Returns every issue found (not just the first) so callers can repair the
/// whole set at once. An empty vec means the pack is coherent.
pub fn validate_pack(mods: &[ModEntry], mc_version: &str, loader: &str) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();

    for m in mods {
        *seen.entry(m.project_id.as_str()).or_insert(0) += 1;

        if !m.game_versions.iter().any(|v| v == mc_version) {
            issues.push(ValidationIssue::IncompatibleGameVersion {
                project_id: m.project_id.clone(),
                want: mc_version.to_string(),
            });
        }
        // Use the same Quilt-accepts-Fabric rule the resolver uses, so a
        // Quilt pack is not falsely flagged for an auto-pulled Fabric-only
        // library (Quilt is wire-compatible with Fabric — see `launch.rs`).
        if !loader_satisfies(&m.loaders, loader) {
            issues.push(ValidationIssue::IncompatibleLoader {
                project_id: m.project_id.clone(),
                want: loader.to_string(),
            });
        }
        if m.client_side == "unsupported" && m.server_side == "unsupported" {
            issues.push(ValidationIssue::UnsupportedOnBothSides {
                project_id: m.project_id.clone(),
            });
        }
        for url in &m.downloads {
            if !url.starts_with("https://") {
                issues.push(ValidationIssue::InsecureDownloadUrl {
                    project_id: m.project_id.clone(),
                    url: url.clone(),
                });
            }
        }
    }

    for (pid, count) in seen {
        if count > 1 {
            issues.push(ValidationIssue::DuplicateProject {
                project_id: pid.to_string(),
            });
        }
    }

    issues
}

// ---- Transitive dependency resolution -------------------------------------
//
// The resolver is intentionally pure and synchronous: it takes pre-fetched
// Modrinth version data (a `project_id -> versions` map the async driver in
// `curator.rs` fills in, with caching and negative-caching) and walks the
// required-dependency graph. No network or filesystem here, so the whole thing
// is exhaustively unit-testable offline. The driver re-runs it after fetching
// any newly discovered projects until the closure stops growing.

/// One dependency edge of a resolved version, mirroring Modrinth's
/// `version.dependencies[]`. `dependency_type` ∈
/// `required` | `optional` | `incompatible` | `embedded`.
#[derive(Debug, Clone)]
pub struct DepEdge {
    pub project_id: Option<String>,
    pub version_id: Option<String>,
    pub dependency_type: String,
}

/// A concrete, downloadable Modrinth version plus everything `write_mrpack`
/// needs to pin it. The driver builds these from `modrinth::Version` +
/// `modrinth::Project`; the resolver only ever consumes this view so it has no
/// dependency on the live API types and stays offline-testable.
#[derive(Debug, Clone)]
pub struct ResolvedVersion {
    pub project_id: String,
    pub version_id: String,
    pub path: String,
    pub sha1: String,
    pub sha512: String,
    pub downloads: Vec<String>,
    pub file_size: u64,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub client_side: String,
    pub server_side: String,
    pub dependencies: Vec<DepEdge>,
    /// Modrinth's `version_number` (the real semantic version string, e.g.
    /// `0.5.13+mc1.20.1`). Carried so general constraint satisfaction compares
    /// the TRUE version, not a fragile filename parse.
    pub version_number: String,
    /// "release" | "beta" | "alpha". Used to prefer stable dependency versions.
    pub version_type: String,
    /// RFC3339 publish timestamp. Used to pick the newest compatible version
    /// independently of the order candidates arrive in.
    pub date_published: String,
}

impl ResolvedVersion {
    fn to_entry(&self) -> ModEntry {
        ModEntry {
            project_id: self.project_id.clone(),
            version_id: self.version_id.clone(),
            path: self.path.clone(),
            sha1: self.sha1.clone(),
            sha512: self.sha512.clone(),
            downloads: self.downloads.clone(),
            file_size: self.file_size,
            game_versions: self.game_versions.clone(),
            loaders: self.loaders.clone(),
            client_side: self.client_side.clone(),
            server_side: self.server_side.clone(),
        }
    }
}

/// Why the resolver could not pin a required dependency. Carried through to a
/// `ValidationIssue::UnresolvedRequiredDependency` so the curator/UI can show a
/// precise reason rather than a generic "missing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepFailure {
    /// Project has no version matching the pack's mc_version + loader.
    NoCompatibleVersion,
    /// `version_id` was pinned by the dependency but that version is unknown.
    PinnedVersionMissing(String),
    /// Project could not be fetched at all (404 / network error).
    Lookup(String),
}

impl DepFailure {
    fn reason(&self) -> String {
        match self {
            DepFailure::NoCompatibleVersion => {
                "no version matches the pack's Minecraft version and loader".to_string()
            }
            DepFailure::PinnedVersionMissing(v) => {
                format!("pinned version {v} not found for the project")
            }
            DepFailure::Lookup(e) => format!("could not fetch from Modrinth: {e}"),
        }
    }
}

/// The set of projects (and, if a dep pinned an exact version, that
/// `version_id`) the resolver needs version data for but does not yet have.
/// The async driver fetches these, feeds them back via `versions`/`failed`,
/// and re-runs the resolver until this comes back empty.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Needed {
    /// project_id -> the exact version_id required (if a dep pinned one).
    pub projects: BTreeMap<String, Option<String>>,
}

impl Needed {
    pub fn is_empty(&self) -> bool {
        self.projects.is_empty()
    }
}

/// Does a candidate version's `loaders` list satisfy the pack `loader`?
///
/// Quilt is wire-compatible with Fabric (see `launch.rs`: Quilt loads the
/// Fabric `profile/json` and Fabric-targeted mods), so a Quilt pack accepts a
/// version that targets `fabric` OR `quilt`. The relationship is NOT symmetric:
/// the Fabric loader cannot load a quilt-only mod, so a Fabric pack only
/// accepts `fabric`.
fn loader_satisfies(version_loaders: &[String], pack_loader: &str) -> bool {
    if version_loaders.iter().any(|l| l == pack_loader) {
        return true;
    }
    pack_loader == "quilt" && version_loaders.iter().any(|l| l == "fabric")
}

fn version_compatible(v: &ResolvedVersion, mc_version: &str, loader: &str) -> bool {
    v.game_versions.iter().any(|g| g == mc_version) && loader_satisfies(&v.loaders, loader)
}

/// Stability rank: release beats beta beats alpha beats anything unknown.
fn channel_rank(version_type: &str) -> u8 {
    match version_type {
        "release" => 0,
        "beta" => 1,
        "alpha" => 2,
        _ => 3,
    }
}

/// Pick the best compatible version deterministically, independent of the
/// order candidates were fetched in: most stable channel first, then the
/// newest publish date (RFC3339 sorts lexically), then version_id as a final
/// stable tiebreak so the choice never flickers between equal candidates.
fn pick_best<'a>(
    candidates: &'a [ResolvedVersion],
    mc_version: &str,
    loader: &str,
) -> Option<&'a ResolvedVersion> {
    candidates
        .iter()
        .filter(|c| version_compatible(c, mc_version, loader))
        .min_by(|a, b| {
            channel_rank(&a.version_type)
                .cmp(&channel_rank(&b.version_type))
                .then(b.date_published.cmp(&a.date_published))
                .then(a.version_id.cmp(&b.version_id))
        })
}

/// The Kotlin MAJOR embedded in a fabric-language-kotlin version string or a
/// dependent's `+kotlin.X.Y.Z` constraint suffix: `">=1.9.2+kotlin.1.8.10"` ->
/// 1, `"1.13.11+kotlin.2.3.21"` -> 2. `None` when no dotted `kotlin.<n>` tag is
/// present (the caller then leaves FLK to the date heuristic — no regression).
/// Matches the dotted `kotlin.` form only, so the project's own
/// `language-kotlin-1.x` name (hyphen, no dot) is never misread.
fn kotlin_major(s: &str) -> Option<u32> {
    let start = s.find("kotlin.")? + "kotlin.".len();
    let digits: String = s[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}


/// Tier 2: open-ended-range version floor.
///
/// The resolver picks each dependency's newest compatible version. When a mod
/// declares an OPEN-ENDED range (`create >=0.5.1-f`, `fabric-language-kotlin
/// >=1.9.2`) that is structurally satisfied by a far-newer, API-incompatible
/// major (Create 6 deleted the class create_dd called; Kotlin 2.x broke IPN's
/// reflection), nothing in any metadata expresses the break. Heuristic, but
/// grounded in observable data: a dependency published well *after* the mod
/// that needs it is the risky one. For each such constraint, re-pin the
/// dependency to the newest compatible version published on/before the
/// requester's publish date + `grace_days`.
///
/// `chosen` is the converged closure; `manifests` are the real jar manifests
/// (project_id -> {provided, requires}); `pool` is every fetched candidate.
/// Returns `(repins, issues)`: `repins` are `(dep_project_id, version_id)` to
/// fold back as roots; `issues` carries a HARD, BLOCKING
/// `KotlinMajorUnsatisfiable` when the FLK constraint is genuinely
/// unsatisfiable. `pool_complete_for` is the set of projects whose FULL
/// version list the async driver has fetched into `pool` (the
/// floor-lookahead pre-pass). It is the discriminator that lets the hard gate
/// distinguish "no compatible FLK build EXISTS" (raise) from "FLK's pool was
/// merely never expanded" (stay silent) — the gate only fires for a project
/// in this set. Pure + deterministic (no I/O) so it is unit-tested directly.
#[allow(clippy::too_many_arguments)]
pub fn version_floor_repins(
    chosen: &[ModEntry],
    manifests: &HashMap<String, crate::registry::JarManifest>,
    pool: &HashMap<String, Vec<ResolvedVersion>>,
    mc_version: &str,
    loader: &str,
    grace_days: i64,
    already: &HashSet<String>,
    pool_complete_for: &HashSet<String>,
) -> (Vec<(String, String)>, Vec<ValidationIssue>) {
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();

    // modid -> the project that provides it (from the real jar manifests).
    let mut owner: HashMap<&str, &str> = HashMap::new();
    for (pid, man) in manifests {
        for (modid, _ver) in &man.provided {
            owner.insert(modid.as_str(), pid.as_str());
        }
    }
    // The closure carries ModEntry (no dates); resolve each to the candidate
    // in `pool` that carries publish date + channel.
    let chosen_by: HashMap<&str, &ResolvedVersion> = chosen
        .iter()
        .filter_map(|e| {
            pool.get(&e.project_id)
                .and_then(|vs| vs.iter().find(|v| v.version_id == e.version_id))
                .map(|v| (e.project_id.as_str(), v))
        })
        .collect();

    // Per dependency project: the EARLIEST (most restrictive) ceiling across
    // every open-ended requester of it. The lib must work for the oldest
    // dependent, so the oldest requester wins.
    let mut ceiling: HashMap<String, chrono::DateTime<chrono::FixedOffset>> = HashMap::new();
    for (req_pid, man) in manifests {
        let Some(req_v) = chosen_by.get(req_pid.as_str()) else {
            continue;
        };
        let Some(req_date) = parse(&req_v.date_published) else {
            continue;
        };
        let cap = req_date + chrono::Duration::days(grace_days);
        for (modid, range) in &man.requires {
            if !crate::registry::is_open_ended_range(range) {
                continue;
            }
            let Some(dep_pid) = owner.get(modid.as_str()) else {
                continue;
            };
            let e = ceiling
                .entry((*dep_pid).to_string())
                .or_insert(cap);
            if cap < *e {
                *e = cap;
            }
        }
    }

    let mut repins: Vec<(String, String)> = Vec::new();
    let mut issues: Vec<ValidationIssue> = Vec::new();

    // --- General version-constraint re-pin (Step 4) -------------------------
    // The EXPRESSED-constraint layer: for a modid whose currently-chosen
    // top-level provider violates a `depends` range (or sits inside a
    // `breaks` range), re-pin that provider to the newest pooled version
    // whose REAL version (threaded `version_number`, not a filename parse)
    // satisfies the conjunction of EVERY depends range ∧ no breaks range on
    // that modid. This is what fixes the Indium/Immersive-Portals/Sodium
    // class (Sodium 0.5.13 ∈ [0.5.11,0.6) ∧ ≠ the IP break) automatically.
    //
    // It does NOT push issues: if it cannot satisfy the conjunction it simply
    // does not re-pin, and `check_version_constraints` (run on the final
    // closure) emits the precise hard `VersionConstraintUnsatisfied` — one
    // source of truth, no double-report.
    //
    // TOP-LEVEL owners only. A JIJ-bundled modid (its version lives inside
    // another project's jar, e.g. fabric-lifecycle-events-v1 in fabric-api)
    // is left to that hard block — auto-fixing it needs an async
    // materialize-each-candidate walk (Step 4.5, deferred). The FLK + date
    // blocks below stay: they recover the open-ended-major break that NO
    // metadata expresses (`>=1.9.2+kotlin.1.8.10` is satisfied by Kotlin-2.x
    // under correct semver — the general layer cannot infer that floor).
    {
        use crate::version::{satisfies, Version, VersionReq};
        let mut dep_ranges: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut brk_ranges: HashMap<&str, Vec<&str>> = HashMap::new();
        for man in manifests.values() {
            for (modid, range) in &man.requires {
                dep_ranges
                    .entry(modid.as_str())
                    .or_default()
                    .push(range.as_str());
            }
            for (modid, range) in &man.breaks {
                brk_ranges
                    .entry(modid.as_str())
                    .or_default()
                    .push(range.as_str());
            }
        }
        let mut targets: Vec<&str> =
            dep_ranges.keys().copied().collect();
        targets.sort();
        for modid in targets {
            let Some(&owner_pid) = owner.get(modid) else {
                continue;
            };
            if already.contains(owner_pid)
                || repins.iter().any(|(p, _)| p == owner_pid)
            {
                continue;
            }
            // Only when the owner's OWN modid is M (top-level). If M is only
            // provided as a JIJ sub-module, owner.provided.first() != M ⇒
            // skip (Step 3 blocks it; Step 4.5 will auto-fix).
            let owner_is_self = manifests
                .get(owner_pid)
                .and_then(|m| m.provided.first())
                .map(|(id, _)| id == modid)
                .unwrap_or(false);
            if !owner_is_self {
                continue;
            }
            let Some(cur) = chosen_by.get(owner_pid) else {
                continue;
            };
            let Some(cur_v) = Version::parse(&cur.version_number) else {
                continue;
            };
            let deps: Vec<VersionReq> = dep_ranges
                .get(modid)
                .into_iter()
                .flatten()
                .filter_map(|r| VersionReq::parse(r))
                .collect();
            let brks: Vec<VersionReq> = brk_ranges
                .get(modid)
                .into_iter()
                .flatten()
                .filter_map(|r| VersionReq::parse(r))
                .collect();
            let ok = |v: &Version| {
                deps.iter().all(|r| satisfies(v, r))
                    && !brks.iter().any(|r| satisfies(v, r))
            };
            if ok(&cur_v) {
                continue; // current pin already satisfies — nothing to do
            }
            let Some(cands) = pool.get(owner_pid) else {
                continue;
            };
            let pick = cands
                .iter()
                .filter(|c| version_compatible(c, mc_version, loader))
                .filter(|c| {
                    Version::parse(&c.version_number)
                        .map(|v| ok(&v))
                        .unwrap_or(false)
                })
                .min_by(|a, b| {
                    channel_rank(&a.version_type)
                        .cmp(&channel_rank(&b.version_type))
                        .then(b.date_published.cmp(&a.date_published))
                        .then(a.version_id.cmp(&b.version_id))
                });
            if let Some(f) = pick {
                if f.version_id != cur.version_id {
                    repins.push((
                        owner_pid.to_string(),
                        f.version_id.clone(),
                    ));
                }
            }
        }
    }

    // --- fabric-language-kotlin / Kotlin-major floor ------------------------
    // FLK is the biggest recurring open-range offender and, uniquely, carries
    // a SECOND deterministic signal the date heuristic cannot see: the Kotlin
    // major is embedded in FLK's own version ("1.13.11+kotlin.2.3.21") AND in
    // dependents' constraints (IPN/libIPN: ">=1.9.2+kotlin.1.8.10"). A mod
    // built for Kotlin 1.x reflection-crashes on FLK Kotlin 2.x at world-join,
    // but both ship continuously so their dates fall within grace. Floor FLK
    // to the newest pooled build whose Kotlin major <= the lowest major any
    // requester encodes. Runs before, and independent of, the date logic.
    if let Some(&flk_pid) = owner.get("fabric-language-kotlin") {
        // Track the constraint AND the project that declared the most
        // restrictive one, so a hard-gate issue can name the real requirer.
        let mut req_major: Option<u32> = None;
        let mut req_owner: Option<&str> = None;
        for (pid, m) in manifests {
            for (modid, range) in &m.requires {
                if modid.as_str() == "fabric-language-kotlin" {
                    if let Some(k) = kotlin_major(range) {
                        if req_major.map_or(true, |c| k < c) {
                            req_owner = Some(pid.as_str());
                        }
                        req_major = Some(req_major.map_or(k, |c| c.min(k)));
                    }
                }
            }
        }
        if let (Some(req_k), Some(cur)) = (req_major, chosen_by.get(flk_pid)) {
            let too_new =
                kotlin_major(&cur.path).map(|c| c > req_k).unwrap_or(false);
            // The `!repins.contains(flk_pid)` guard is defense-in-depth: today
            // it is unreachable (when the FLK block runs, the general block
            // left FLK alone because an open-ended `>=…+kotlin.N` is satisfied
            // by the current pin). It protects a future mod that declares BOTH
            // an open-ended-kotlin constraint AND a bounded depends/breaks on
            // FLK — the general (more authoritative, expressed) repin wins.
            if too_new
                && !already.contains(flk_pid)
                && !repins.iter().any(|(p, _)| p == flk_pid)
            {
                let floor = pool.get(flk_pid).and_then(|cands| {
                    cands
                        .iter()
                        .filter(|c| version_compatible(c, mc_version, loader))
                        .filter(|c| {
                            kotlin_major(&c.path)
                                .map(|k| k <= req_k)
                                .unwrap_or(false)
                        })
                        .min_by(|a, b| {
                            channel_rank(&a.version_type)
                                .cmp(&channel_rank(&b.version_type))
                                .then(b.date_published.cmp(&a.date_published))
                                .then(a.version_id.cmp(&b.version_id))
                        })
                });
                match floor {
                    Some(f) if f.version_id != cur.version_id => {
                        repins.push((flk_pid.to_string(), f.version_id.clone()));
                    }
                    Some(_) => {}
                    // No FLK build with Kotlin major <= req_k. Only a HARD
                    // gate if the pool was actually expanded for FLK (the
                    // floor-lookahead pre-pass fetched its FULL list) — then
                    // this is genuinely unsatisfiable, not "not yet searched".
                    None if pool_complete_for.contains(flk_pid) => {
                        issues.push(ValidationIssue::KotlinMajorUnsatisfiable {
                            requirer: req_owner
                                .unwrap_or(flk_pid)
                                .to_string(),
                            needs_major: req_k,
                            present: kotlin_major(&cur.path).unwrap_or(req_k + 1),
                        });
                    }
                    None => {}
                }
            }
        }
    }

    for (dep_pid, cap) in ceiling {
        if already.contains(&dep_pid) {
            continue;
        }
        // A deterministic pass (FLK Kotlin-major) already floored this dep —
        // do not let the looser date heuristic override it.
        if repins.iter().any(|(p, _)| *p == dep_pid) {
            continue;
        }
        let Some(cur) = chosen_by.get(dep_pid.as_str()) else {
            continue;
        };
        // Only act if the chosen version is actually newer than the ceiling
        // (i.e. the resolver reached past the requester's era).
        match parse(&cur.date_published) {
            Some(d) if d <= cap => continue,
            None => continue,
            _ => {}
        }
        let Some(cands) = pool.get(&dep_pid) else {
            continue;
        };
        let floor = cands
            .iter()
            .filter(|c| version_compatible(c, mc_version, loader))
            .filter(|c| parse(&c.date_published).map(|d| d <= cap).unwrap_or(false))
            .min_by(|a, b| {
                channel_rank(&a.version_type)
                    .cmp(&channel_rank(&b.version_type))
                    .then(b.date_published.cmp(&a.date_published))
                    .then(a.version_id.cmp(&b.version_id))
            });
        if let Some(f) = floor {
            if f.version_id != cur.version_id {
                repins.push((dep_pid.clone(), f.version_id.clone()));
            }
        }
    }
    repins.sort();
    issues.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    (repins, issues)
}

/// Strip SemVer/Fabric build metadata for an exact-version compare: a leading
/// `=`, then everything from the first `+` (build metadata, e.g. `+mc1.20.1`,
/// `+build.1744`, `+kotlin.2.3.21`).
fn strip_build_meta(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix('=').unwrap_or(s);
    match s.find('+') {
        Some(i) => &s[..i],
        None => s,
    }
}

/// Is `constraint` an EXACT version pin (a single concrete version), not a
/// range/wildcard/list? Deliberately strict: ANY range operator, wildcard,
/// separator or bracket ⇒ NOT an exact pin ⇒ the caller does not flag it.
/// Parsing real Fabric/Maven ranges is a false-positive tar pit; this Phase-1
/// audit only acts where non-satisfaction is unambiguous.
fn is_exact_pin(constraint: &str) -> bool {
    let c = strip_build_meta(constraint);
    !c.is_empty()
        && !c.chars().any(|ch| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '>' | '<' | '~' | '^' | '*' | 'x' | 'X' | ',' | '|' | '[' | ']' | '(' | ')'
                )
        })
}

/// True only when `constraint` is an exact pin AND `present_version` (build
/// metadata stripped from both) differs — i.e. a definite, unambiguous
/// version conflict. Never returns true for a range/wildcard/unparseable
/// constraint (no false positives).
fn exact_pin_violation(present_version: &str, constraint: &str) -> bool {
    is_exact_pin(constraint)
        && strip_build_meta(present_version) != strip_build_meta(constraint)
}

/// Post-resolution exact-pin dependency audit. The resolver satisfies a
/// dependency by modid PRESENCE, not version — so a mod that exact-pins an old
/// major of a present provider (createairfabric needs Create `0.5.1-j-…` but
/// the pack has Create `6.0.8.1`) sails through and Fabric rejects it at
/// launch. For every entry whose jar exact-pins a present provider it does not
/// satisfy: if the requester is a LEAF (nothing in the closure depends on
/// anything it provides) it is auto-DROPPED (the pack assembles working,
/// `IncompatibleAddonDropped` is reported informationally — NOT blocking);
/// otherwise a blocking `IncompatibleDependencyPresent` is raised (dropping a
/// depended-on mod would cascade — the curator/gate must handle it). Pure +
/// deterministic, unit-tested. Returns the (possibly filtered) closure.
pub fn audit_version_satisfaction(
    entries: &[ModEntry],
    manifests: &HashMap<String, crate::registry::JarManifest>,
) -> (Vec<ModEntry>, Vec<ValidationIssue>) {
    // modid -> (provider project_id, provider's own declared version)
    let mut provider: HashMap<&str, (&str, &str)> = HashMap::new();
    for (pid, m) in manifests {
        for (modid, _ver) in &m.provided {
            provider
                .entry(modid.as_str())
                .or_insert((pid.as_str(), m.version.as_str()));
        }
    }
    let present: HashSet<&str> = entries.iter().map(|e| e.project_id.as_str()).collect();

    let mut drop_pids: HashSet<String> = HashSet::new();
    let mut issues: Vec<ValidationIssue> = Vec::new();

    for e in entries {
        let Some(m) = manifests.get(&e.project_id) else {
            continue;
        };
        for (modid, range) in &m.requires {
            if !is_exact_pin(range) {
                continue;
            }
            let Some(&(prov_pid, prov_ver)) = provider.get(modid.as_str()) else {
                continue; // provider absent entirely -> a different issue class
            };
            if !present.contains(prov_pid) || prov_ver.is_empty() {
                continue; // provider not in closure, or version unknown (Forge): skip
            }
            if !exact_pin_violation(prov_ver, range) {
                continue;
            }
            // Requester is a leaf iff no OTHER entry requires any modid it provides.
            let req_provides: HashSet<&str> =
                m.provided.iter().map(|(modid, _)| modid.as_str()).collect();
            let is_leaf = !entries.iter().any(|o| {
                o.project_id != e.project_id
                    && manifests.get(&o.project_id).is_some_and(|om| {
                        om.requires
                            .iter()
                            .any(|(rm, _)| req_provides.contains(rm.as_str()))
                    })
            });
            let addon = m
                .provided
                .first()
                .map(|(modid, _)| modid.clone())
                .unwrap_or_else(|| e.project_id.clone());
            if is_leaf {
                drop_pids.insert(e.project_id.clone());
                issues.push(ValidationIssue::IncompatibleAddonDropped {
                    addon,
                    requires: format!("{modid} {range}"),
                    present: prov_ver.to_string(),
                });
            } else {
                issues.push(ValidationIssue::IncompatibleDependencyPresent {
                    holder: addon,
                    conflicts_with: format!(
                        "{modid} (needs {range}, pack has {prov_ver})"
                    ),
                });
            }
        }
    }
    let kept = entries
        .iter()
        .filter(|e| !drop_pids.contains(&e.project_id))
        .cloned()
        .collect();
    (kept, issues)
}

/// Report-only general version-constraint check (Step 3 of the resolver
/// restructure). Evaluates EVERY resolved mod's `depends` and `breaks`/
/// `conflicts` ranges against the actual version present in the resolved set
/// — including JIJ-bundled sub-module versions — using the real Fabric
/// matcher (`crate::version`). Pure; does NOT change selection (that is
/// Step 4). This is what turns the silent "Incompatible mods found" launch
/// crash into a precise pre-assemble block: the three crash failures
/// (Indium→sodium floor, Immersive Portals→sodium `breaks`, LambDynamicLights
/// →fabric-lifecycle-events-v1 JIJ sub-module floor) are exactly the
/// `Depends`/`Breaks` cases below.
///
/// Deliberately silent on a *missing* modid (that is the existing
/// `UnresolvedRequiredDependency` class — never double-report) and on an
/// unparseable range/version (unknown ⇒ skip, never a false positive).
pub fn check_version_constraints(
    entries: &[ModEntry],
    manifests: &HashMap<String, crate::registry::JarManifest>,
) -> Vec<ValidationIssue> {
    use crate::version::{satisfies, Version, VersionReq};

    let present: HashSet<&str> =
        entries.iter().map(|e| e.project_id.as_str()).collect();

    // modid -> the highest provided version across the resolved set (a modid
    // can be provided by several mods and/or JIJ-bundled; the dependent sees
    // whichever Fabric loads, so check against the best available).
    let mut have: HashMap<&str, &str> = HashMap::new();
    for (pid, m) in manifests {
        if !present.contains(pid.as_str()) {
            continue;
        }
        for (modid, ver) in &m.provided {
            match have.get(modid.as_str()) {
                None => {
                    have.insert(modid.as_str(), ver.as_str());
                }
                Some(cur) => {
                    if let (Some(a), Some(b)) =
                        (Version::parse(ver), Version::parse(cur))
                    {
                        if a > b {
                            have.insert(modid.as_str(), ver.as_str());
                        }
                    }
                }
            }
        }
    }

    let mut out: Vec<ValidationIssue> = Vec::new();
    for e in entries {
        let Some(m) = manifests.get(&e.project_id) else {
            continue;
        };
        let holder = m
            .provided
            .first()
            .map(|(modid, _)| modid.clone())
            .unwrap_or_else(|| e.project_id.clone());

        for (modid, range) in &m.requires {
            let Some(&have_ver) = have.get(modid.as_str()) else {
                continue; // missing entirely -> UnresolvedRequiredDependency's job
            };
            if have_ver.is_empty() {
                continue; // version unknown (e.g. Forge JIJ) -> cannot judge
            }
            let Some(req) = VersionReq::parse(range) else {
                // Skip (no false positive) but make the silence observable:
                // a genuinely buggy author range otherwise vanishes.
                tracing::warn!(
                    holder = %holder, modid = %modid, range = %range,
                    "unparseable `depends` version range — constraint left \
                     unevaluated"
                );
                continue;
            };
            let Some(hv) = Version::parse(have_ver) else {
                continue; // our-side version unparseable: cannot judge
            };
            if !satisfies(&hv, &req) {
                out.push(ValidationIssue::VersionConstraintUnsatisfied {
                    holder: holder.clone(),
                    modid: modid.clone(),
                    want: range.clone(),
                    have: have_ver.to_string(),
                    kind: ConstraintKind::Depends,
                });
            }
        }

        for (modid, range) in &m.breaks {
            let Some(&have_ver) = have.get(modid.as_str()) else {
                continue; // the broken mod is not present -> fine
            };
            if have_ver.is_empty() {
                continue;
            }
            let Some(req) = VersionReq::parse(range) else {
                tracing::warn!(
                    holder = %holder, modid = %modid, range = %range,
                    "unparseable `breaks` version range — constraint left \
                     unevaluated"
                );
                continue;
            };
            let Some(hv) = Version::parse(have_ver) else {
                continue;
            };
            if satisfies(&hv, &req) {
                out.push(ValidationIssue::VersionConstraintUnsatisfied {
                    holder: holder.clone(),
                    modid: modid.clone(),
                    want: range.clone(),
                    have: have_ver.to_string(),
                    kind: ConstraintKind::Breaks,
                });
            }
        }
    }

    // Deterministic + de-duplicated (a modid can be reached twice).
    out.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    out.dedup();
    out
}

/// Resolve the transitive `required`-dependency closure for `roots`.
///
/// `roots` are the user/curator-pinned versions (their exact pinned versions
/// are authoritative and are never replaced or duplicated). `pool` maps a
/// project_id to every published version the driver has fetched so far;
/// `failed` records projects whose lookup hard-failed (404 / network) so a
/// transient error degrades to a reported issue instead of an infinite
/// re-fetch loop.
///
/// Returns the deduped closure entries (roots first, then resolved deps), the
/// validation issues discovered while resolving, and the set of still-unknown
/// projects the driver must fetch before calling again. When `needed` is empty
/// the closure is complete.
pub fn resolve_dependencies(
    roots: &[ResolvedVersion],
    pool: &HashMap<String, Vec<ResolvedVersion>>,
    failed: &HashMap<String, String>,
    mc_version: &str,
    loader: &str,
) -> (Vec<ModEntry>, Vec<ValidationIssue>, Needed) {
    let mut entries: Vec<ModEntry> = Vec::with_capacity(roots.len());
    let mut issues: Vec<ValidationIssue> = Vec::new();
    let mut needed = Needed::default();

    // project_id -> chosen ResolvedVersion. Roots are inserted first so a
    // user-pinned project always wins over any transitively-discovered
    // candidate for the same project (dedupe is keyed purely by project_id;
    // a pack never carries two versions of one project).
    let mut chosen: HashMap<String, ResolvedVersion> = HashMap::new();
    // Worklist of versions whose dependency edges still need walking.
    let mut work: Vec<ResolvedVersion> = Vec::new();
    // project_ids we've already decided on (cycle + dedupe guard).
    let mut visited: HashSet<String> = HashSet::new();
    // conflict project_id -> the mod that declared it `incompatible`. Recorded
    // while walking; a conflict is only an issue if that project actually ends
    // up in the closure, which we only know once the walk completes.
    let mut incompat_holders: HashMap<String, String> = HashMap::new();

    for r in roots {
        if visited.insert(r.project_id.clone()) {
            chosen.insert(r.project_id.clone(), r.clone());
            work.push(r.clone());
        }
        // A duplicate root project is reported by `validate_pack`'s existing
        // DuplicateProject check; here we just keep the first.
    }

    while let Some(cur) = work.pop() {
        for dep in &cur.dependencies {
            match dep.dependency_type.as_str() {
                "required" => {}
                "incompatible" => {
                    if let Some(pid) = &dep.project_id {
                        incompat_holders
                            .entry(pid.clone())
                            .or_insert_with(|| cur.project_id.clone());
                    }
                    continue;
                }
                // `embedded` ships inside the parent jar (double-load if
                // added); `optional` is the user's call. Neither is pulled.
                _ => continue,
            }

            let Some(dep_pid) = dep.project_id.clone() else {
                // A file_name-only dependency carries no project to resolve;
                // nothing actionable, skip it.
                continue;
            };

            if visited.contains(&dep_pid) {
                continue; // already chosen (cycle-safe) or pending decision
            }

            // Hard lookup failure recorded by the driver -> a reported issue,
            // not a panic and not an infinite fetch loop.
            if let Some(err) = failed.get(&dep_pid) {
                visited.insert(dep_pid.clone());
                issues.push(ValidationIssue::UnresolvedRequiredDependency {
                    needed_by: cur.project_id.clone(),
                    missing_project_id: dep_pid.clone(),
                    reason: DepFailure::Lookup(err.clone()).reason(),
                });
                continue;
            }

            let Some(candidates) = pool.get(&dep_pid) else {
                // Not fetched yet: ask the driver for it (remembering an exact
                // pinned version_id if the dep specified one).
                needed
                    .projects
                    .entry(dep_pid.clone())
                    .or_insert_with(|| dep.version_id.clone());
                continue;
            };

            // We have data for this project; commit to a version now.
            visited.insert(dep_pid.clone());
            let picked = match &dep.version_id {
                // Exact version pinned by the dependency: honor it verbatim,
                // no version-shopping. If it does not match mc/loader the
                // standard validate_pack checks will flag it downstream.
                Some(vid) => match candidates.iter().find(|c| &c.version_id == vid) {
                    Some(v) => Some(v.clone()),
                    None => {
                        issues.push(ValidationIssue::UnresolvedRequiredDependency {
                            needed_by: cur.project_id.clone(),
                            missing_project_id: dep_pid.clone(),
                            reason: DepFailure::PinnedVersionMissing(vid.clone()).reason(),
                        });
                        None
                    }
                },
                // No pin: best compatible version, chosen explicitly rather
                // than trusting candidate order — prefer a stable release, then
                // the newest publish date (see `pick_best`).
                None => {
                    match pick_best(candidates, mc_version, loader) {
                        Some(v) => Some(v.clone()),
                        None => {
                            issues.push(ValidationIssue::UnresolvedRequiredDependency {
                                needed_by: cur.project_id.clone(),
                                missing_project_id: dep_pid.clone(),
                                reason: DepFailure::NoCompatibleVersion.reason(),
                            });
                            None
                        }
                    }
                }
            };

            if let Some(v) = picked {
                chosen.insert(dep_pid.clone(), v.clone());
                work.push(v);
            }
        }
    }

    // Roots first (stable order the caller passed), then resolved deps sorted
    // by project_id for a deterministic .mrpack.
    let mut out_pids: HashSet<String> = HashSet::new();
    for r in roots {
        if let Some(v) = chosen.get(&r.project_id) {
            if out_pids.insert(v.project_id.clone()) {
                entries.push(v.to_entry());
            }
        }
    }
    let mut extra: Vec<&ResolvedVersion> = chosen
        .values()
        .filter(|v| !out_pids.contains(&v.project_id))
        .collect();
    extra.sort_by(|a, b| a.project_id.cmp(&b.project_id));
    for v in extra {
        if out_pids.insert(v.project_id.clone()) {
            entries.push(v.to_entry());
        }
    }

    // Now that the full closure is known, flag any `incompatible` edge whose
    // target project actually ended up in the pack.
    for (conflict_pid, holder) in &incompat_holders {
        if out_pids.contains(conflict_pid) {
            issues.push(ValidationIssue::IncompatibleDependencyPresent {
                holder: holder.clone(),
                conflicts_with: conflict_pid.clone(),
            });
        }
    }

    (entries, issues, needed)
}

// ---- .mrpack emission (standard Modrinth modpack format) ----

#[derive(Debug, Serialize, Deserialize)]
pub struct PackMeta {
    pub name: String,
    pub version_id: String,
    pub summary: String,
    pub mc_version: String,
    /// Modrinth loader dependency key, e.g. "fabric-loader", "neoforge".
    pub loader_key: String,
    pub loader_version: String,
}

#[derive(Serialize)]
struct IndexFileEnv {
    client: String,
    server: String,
}

#[derive(Serialize)]
struct IndexFile {
    path: String,
    hashes: BTreeMap<String, String>,
    env: IndexFileEnv,
    downloads: Vec<String>,
    #[serde(rename = "fileSize")]
    file_size: u64,
}

#[derive(Serialize)]
struct MrpackIndex {
    #[serde(rename = "formatVersion")]
    format_version: u32,
    game: String,
    #[serde(rename = "versionId")]
    version_id: String,
    name: String,
    summary: String,
    files: Vec<IndexFile>,
    dependencies: BTreeMap<String, String>,
}

fn build_index(meta: &PackMeta, mods: &[ModEntry]) -> MrpackIndex {
    let files = mods
        .iter()
        .map(|m| {
            let mut hashes = BTreeMap::new();
            hashes.insert("sha1".to_string(), m.sha1.clone());
            hashes.insert("sha512".to_string(), m.sha512.clone());
            IndexFile {
                path: m.path.clone(),
                hashes,
                env: IndexFileEnv {
                    client: m.client_side.clone(),
                    server: m.server_side.clone(),
                },
                downloads: m.downloads.clone(),
                file_size: m.file_size,
            }
        })
        .collect();

    let mut dependencies = BTreeMap::new();
    dependencies.insert("minecraft".to_string(), meta.mc_version.clone());
    dependencies.insert(meta.loader_key.clone(), meta.loader_version.clone());

    MrpackIndex {
        format_version: 1,
        game: "minecraft".to_string(),
        version_id: meta.version_id.clone(),
        name: meta.name.clone(),
        summary: meta.summary.clone(),
        files,
        dependencies,
    }
}

/// Serialize the `modrinth.index.json` exactly as it would be written into the
/// `.mrpack` zip. Kept separate so tests can assert on it without disk I/O.
pub fn index_json(meta: &PackMeta, mods: &[ModEntry]) -> String {
    serde_json::to_string_pretty(&build_index(meta, mods))
        .expect("index is composed of plain serializable types")
}

/// Write a standard `.mrpack` (zip containing `modrinth.index.json`). Mod jars
/// are NOT bundled — only manifest references — which is both the legal
/// requirement and the universal launcher pattern.
pub fn write_mrpack(
    meta: &PackMeta,
    mods: &[ModEntry],
    out_path: &std::path::Path,
) -> anyhow::Result<()> {
    let file = std::fs::File::create(out_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("modrinth.index.json", opts)?;
    zip.write_all(index_json(meta, mods).as_bytes())?;
    zip.finish()?;
    Ok(())
}

// ---- .mrpack import (read side) ----

/// Loader-neutral view of a `.mrpack`, ready to become an `Instance`.
#[derive(Debug, Clone)]
pub struct ImportedPack {
    pub name: String,
    pub mc_version: String,
    /// "vanilla" | "fabric" | "forge" | "neoforge" | "quilt"
    pub loader: String,
    pub loader_version: String,
    pub mods: Vec<ImportedMod>,
}

#[derive(Debug, Clone)]
pub struct ImportedMod {
    pub name: String,
    pub path: String,
    pub sha1: String,
    pub sha512: String,
    pub download_url: String,
    pub file_size: u64,
}

#[derive(Deserialize)]
struct InHashes {
    #[serde(default)]
    sha1: String,
    #[serde(default)]
    sha512: String,
}
#[derive(Deserialize)]
struct InFile {
    path: String,
    hashes: InHashes,
    downloads: Vec<String>,
    #[serde(rename = "fileSize", default)]
    file_size: u64,
}
#[derive(Deserialize)]
struct InIndex {
    #[serde(default)]
    name: String,
    files: Vec<InFile>,
    dependencies: BTreeMap<String, String>,
}

/// Parse a standard `.mrpack` (zip containing `modrinth.index.json`) into an
/// `ImportedPack`. Mod jars are not bundled; the manifest carries CDN URLs.
pub fn read_mrpack(path: &std::path::Path) -> anyhow::Result<ImportedPack> {
    let f = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(f)?;
    let mut s = String::new();
    {
        let mut entry = archive
            .by_name("modrinth.index.json")
            .map_err(|_| anyhow::anyhow!("not a valid .mrpack (no modrinth.index.json)"))?;
        std::io::Read::read_to_string(&mut entry, &mut s)?;
    }
    let idx: InIndex = serde_json::from_str(&s)?;

    let mc_version = idx
        .dependencies
        .get("minecraft")
        .cloned()
        .unwrap_or_default();
    let (loader, loader_version) = [
        ("fabric-loader", "fabric"),
        ("quilt-loader", "quilt"),
        ("neoforge", "neoforge"),
        ("forge", "forge"),
    ]
    .iter()
    .find_map(|(key, loader)| {
        idx.dependencies
            .get(*key)
            .map(|v| (loader.to_string(), v.clone()))
    })
    .unwrap_or_else(|| ("vanilla".to_string(), String::new()));

    let mods = idx
        .files
        .into_iter()
        .map(|file| ImportedMod {
            name: file
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&file.path)
                .to_string(),
            download_url: file.downloads.into_iter().next().unwrap_or_default(),
            path: file.path,
            sha1: file.hashes.sha1,
            sha512: file.hashes.sha512,
            file_size: file.file_size,
        })
        .collect();

    Ok(ImportedPack {
        name: if idx.name.is_empty() {
            "Imported pack".to_string()
        } else {
            idx.name
        },
        mc_version,
        loader,
        loader_version,
        mods,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mod_entry(id: &str, mc: &[&str], loaders: &[&str]) -> ModEntry {
        ModEntry {
            project_id: id.to_string(),
            version_id: format!("{id}-v1"),
            path: format!("mods/{id}.jar"),
            sha1: "a".repeat(40),
            sha512: "b".repeat(128),
            downloads: vec![format!("https://cdn.modrinth.com/data/{id}/x.jar")],
            file_size: 1024,
            game_versions: mc.iter().map(|s| s.to_string()).collect(),
            loaders: loaders.iter().map(|s| s.to_string()).collect(),
            client_side: "required".to_string(),
            server_side: "optional".to_string(),
        }
    }

    #[test]
    fn clean_pack_has_no_issues() {
        let mods = vec![
            mod_entry("sodium", &["1.21.1"], &["fabric"]),
            mod_entry("lithium", &["1.21.1"], &["fabric"]),
        ];
        assert!(validate_pack(&mods, "1.21.1", "fabric").is_empty());
    }

    #[test]
    fn detects_incompatible_loader_and_game_version() {
        let mods = vec![mod_entry("create", &["1.20.1"], &["forge"])];
        let issues = validate_pack(&mods, "1.21.1", "fabric");
        assert!(issues.contains(&ValidationIssue::IncompatibleGameVersion {
            project_id: "create".into(),
            want: "1.21.1".into()
        }));
        assert!(issues.contains(&ValidationIssue::IncompatibleLoader {
            project_id: "create".into(),
            want: "fabric".into()
        }));
    }

    #[test]
    fn detects_unsupported_on_both_sides() {
        let mut m = mod_entry("ghost", &["1.21.1"], &["fabric"]);
        m.client_side = "unsupported".into();
        m.server_side = "unsupported".into();
        let issues = validate_pack(&[m], "1.21.1", "fabric");
        assert!(issues.contains(&ValidationIssue::UnsupportedOnBothSides {
            project_id: "ghost".into()
        }));
    }

    #[test]
    fn detects_duplicate_project() {
        let mods = vec![
            mod_entry("sodium", &["1.21.1"], &["fabric"]),
            mod_entry("sodium", &["1.21.1"], &["fabric"]),
        ];
        let issues = validate_pack(&mods, "1.21.1", "fabric");
        assert!(issues.contains(&ValidationIssue::DuplicateProject {
            project_id: "sodium".into()
        }));
    }

    // ---- transitive dependency resolver ----

    fn dep(pid: &str, kind: &str) -> DepEdge {
        DepEdge {
            project_id: Some(pid.to_string()),
            version_id: None,
            dependency_type: kind.to_string(),
        }
    }

    /// A compatible (mc=1.20.1, loader=fabric) version with the given deps.
    fn rv(pid: &str, deps: Vec<DepEdge>) -> ResolvedVersion {
        rv_for(pid, &["1.20.1"], &["fabric"], deps)
    }

    fn rv_for(
        pid: &str,
        mc: &[&str],
        loaders: &[&str],
        deps: Vec<DepEdge>,
    ) -> ResolvedVersion {
        ResolvedVersion {
            project_id: pid.to_string(),
            version_id: format!("{pid}-v1"),
            path: format!("mods/{pid}.jar"),
            sha1: "a".repeat(40),
            sha512: "b".repeat(128),
            downloads: vec![format!("https://cdn.modrinth.com/data/{pid}/x.jar")],
            file_size: 1024,
            game_versions: mc.iter().map(|s| s.to_string()).collect(),
            loaders: loaders.iter().map(|s| s.to_string()).collect(),
            client_side: "required".to_string(),
            server_side: "optional".to_string(),
            dependencies: deps,
            version_number: "1.0.0".to_string(),
            version_type: "release".to_string(),
            date_published: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    fn pool_of(versions: &[ResolvedVersion]) -> HashMap<String, Vec<ResolvedVersion>> {
        let mut m: HashMap<String, Vec<ResolvedVersion>> = HashMap::new();
        for v in versions {
            m.entry(v.project_id.clone()).or_default().push(v.clone());
        }
        m
    }

    /// Drive the pure resolver to a fixpoint against an in-memory pool,
    /// mimicking what the async curator driver does (no network).
    fn resolve_to_fixpoint(
        roots: &[ResolvedVersion],
        universe: &[ResolvedVersion],
        mc: &str,
        loader: &str,
    ) -> (Vec<ModEntry>, Vec<ValidationIssue>) {
        let full = pool_of(universe);
        let failed: HashMap<String, String> = HashMap::new();
        let mut pool: HashMap<String, Vec<ResolvedVersion>> = HashMap::new();
        for r in roots {
            pool.entry(r.project_id.clone())
                .or_default()
                .push(r.clone());
        }
        loop {
            let (entries, issues, needed) =
                resolve_dependencies(roots, &pool, &failed, mc, loader);
            if needed.is_empty() {
                return (entries, issues);
            }
            for pid in needed.projects.keys() {
                if let Some(vs) = full.get(pid) {
                    pool.insert(pid.clone(), vs.clone());
                } else {
                    // Project genuinely unknown: stop, resolver will flag it.
                    pool.entry(pid.clone()).or_default();
                }
            }
        }
    }

    #[test]
    fn resolves_required_transitively_skips_optional_and_embedded() {
        // create -> flywheel(required), jei(optional), embeddedlib(embedded)
        // flywheel -> none
        let create = rv(
            "create",
            vec![
                dep("flywheel", "required"),
                dep("jei", "optional"),
                dep("embeddedlib", "embedded"),
            ],
        );
        let flywheel = rv("flywheel", vec![]);
        let jei = rv("jei", vec![]);
        let embeddedlib = rv("embeddedlib", vec![]);
        let (entries, issues) = resolve_to_fixpoint(
            &[create.clone()],
            &[create, flywheel, jei, embeddedlib],
            "1.20.1",
            "fabric",
        );
        let ids: Vec<&str> = entries.iter().map(|e| e.project_id.as_str()).collect();
        assert!(ids.contains(&"create"));
        assert!(ids.contains(&"flywheel"), "required dep pulled");
        assert!(!ids.contains(&"jei"), "optional dep NOT pulled");
        assert!(!ids.contains(&"embeddedlib"), "embedded dep NOT pulled");
        assert!(issues.is_empty(), "no issues: {issues:?}");
    }

    #[test]
    fn real_world_bug_pulls_libraries() {
        // The reported bug: shipped mods without their required libraries.
        let ench = rv("enchantmentdescriptions", vec![dep("bookshelf", "required")]);
        let tectonic = rv("tectonic", vec![dep("lithostitched", "required")]);
        let villages = rv("villagespillages", vec![dep("yungsapi", "required")]);
        let bookshelf = rv("bookshelf", vec![]);
        let lithostitched = rv("lithostitched", vec![]);
        let yungsapi = rv("yungsapi", vec![]);
        let (entries, issues) = resolve_to_fixpoint(
            &[ench.clone(), tectonic.clone(), villages.clone()],
            &[ench, tectonic, villages, bookshelf, lithostitched, yungsapi],
            "1.20.1",
            "fabric",
        );
        let ids: Vec<&str> = entries.iter().map(|e| e.project_id.as_str()).collect();
        assert!(ids.contains(&"bookshelf"));
        assert!(ids.contains(&"lithostitched"));
        assert!(ids.contains(&"yungsapi"));
        assert!(issues.is_empty(), "no issues: {issues:?}");
    }

    #[test]
    fn resolver_is_cycle_safe() {
        // a -> b (required), b -> a (required): must terminate.
        let a = rv("a", vec![dep("b", "required")]);
        let b = rv("b", vec![dep("a", "required")]);
        let (entries, issues) =
            resolve_to_fixpoint(&[a.clone()], &[a, b], "1.20.1", "fabric");
        let mut ids: Vec<&str> = entries.iter().map(|e| e.project_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["a", "b"]);
        assert!(issues.is_empty());
    }

    #[test]
    fn shared_dependency_is_deduped() {
        // two roots both require fabric-api; it appears exactly once.
        let m1 = rv("m1", vec![dep("fabric-api", "required")]);
        let m2 = rv("m2", vec![dep("fabric-api", "required")]);
        let fapi = rv("fabric-api", vec![]);
        let (entries, _issues) = resolve_to_fixpoint(
            &[m1.clone(), m2.clone()],
            &[m1, m2, fapi],
            "1.20.1",
            "fabric",
        );
        let count = entries
            .iter()
            .filter(|e| e.project_id == "fabric-api")
            .count();
        assert_eq!(count, 1, "shared dep added exactly once");
    }

    #[test]
    fn user_pinned_version_wins_over_transitive() {
        // user pinned fabric-api at a SPECIFIC older version; a root requires
        // fabric-api too. The user's pin must survive (no replace, no dup).
        let mut user_fapi = rv("fabric-api", vec![]);
        user_fapi.version_id = "user-pinned-old".to_string();
        let consumer = rv("consumer", vec![dep("fabric-api", "required")]);
        // Universe also has a "newer" fabric-api, but it must NOT be chosen.
        let mut newer_fapi = rv("fabric-api", vec![]);
        newer_fapi.version_id = "newer-auto".to_string();

        let roots = vec![consumer.clone(), user_fapi.clone()];
        let (entries, issues) = resolve_to_fixpoint(
            &roots,
            &[consumer, newer_fapi],
            "1.20.1",
            "fabric",
        );
        let fapi: Vec<&ModEntry> = entries
            .iter()
            .filter(|e| e.project_id == "fabric-api")
            .collect();
        assert_eq!(fapi.len(), 1, "exactly one fabric-api");
        assert_eq!(fapi[0].version_id, "user-pinned-old", "user pin wins");
        assert!(issues.is_empty());
    }

    #[test]
    fn unsatisfiable_required_dep_becomes_issue() {
        // tectonic requires lithostitched, which has no compatible version.
        let tectonic = rv("tectonic", vec![dep("lithostitched", "required")]);
        let litho_wrong = rv_for("lithostitched", &["1.21.1"], &["fabric"], vec![]);
        let (entries, issues) = resolve_to_fixpoint(
            &[tectonic.clone()],
            &[tectonic, litho_wrong],
            "1.20.1",
            "fabric",
        );
        assert!(!entries.iter().any(|e| e.project_id == "lithostitched"));
        assert!(issues.iter().any(|i| matches!(
            i,
            ValidationIssue::UnresolvedRequiredDependency { missing_project_id, needed_by, .. }
            if missing_project_id == "lithostitched" && needed_by == "tectonic"
        )));
    }

    #[test]
    fn missing_project_lookup_failure_becomes_issue_not_panic() {
        // The driver could not fetch the dep project at all (404 / network).
        let m = rv("m", vec![dep("ghostlib", "required")]);
        let mut pool: HashMap<String, Vec<ResolvedVersion>> = HashMap::new();
        pool.insert("m".to_string(), vec![m.clone()]);
        let mut failed = HashMap::new();
        failed.insert("ghostlib".to_string(), "modrinth returned 404".to_string());
        let (_e, issues, needed) =
            resolve_dependencies(&[m], &pool, &failed, "1.20.1", "fabric");
        assert!(needed.is_empty(), "failed dep is not re-requested");
        assert!(issues.iter().any(|i| matches!(
            i,
            ValidationIssue::UnresolvedRequiredDependency { missing_project_id, reason, .. }
            if missing_project_id == "ghostlib" && reason.contains("404")
        )));
    }

    #[test]
    fn incompatible_dependency_present_is_flagged() {
        // sodium declares "rubidium" incompatible; rubidium is also pinned.
        let sodium = ResolvedVersion {
            dependencies: vec![DepEdge {
                project_id: Some("rubidium".to_string()),
                version_id: None,
                dependency_type: "incompatible".to_string(),
            }],
            ..rv("sodium", vec![])
        };
        let rubidium = rv("rubidium", vec![]);
        let (_entries, issues) = resolve_to_fixpoint(
            &[sodium.clone(), rubidium.clone()],
            &[sodium, rubidium],
            "1.20.1",
            "fabric",
        );
        assert!(issues.iter().any(|i| matches!(
            i,
            ValidationIssue::IncompatibleDependencyPresent { holder, conflicts_with }
            if holder == "sodium" && conflicts_with == "rubidium"
        )));
    }

    #[test]
    fn incompatible_dependency_absent_is_not_flagged() {
        let sodium = ResolvedVersion {
            dependencies: vec![DepEdge {
                project_id: Some("rubidium".to_string()),
                version_id: None,
                dependency_type: "incompatible".to_string(),
            }],
            ..rv("sodium", vec![])
        };
        let (_e, issues) =
            resolve_to_fixpoint(&[sodium.clone()], &[sodium], "1.20.1", "fabric");
        assert!(!issues
            .iter()
            .any(|i| matches!(i, ValidationIssue::IncompatibleDependencyPresent { .. })));
    }

    #[test]
    fn quilt_pack_accepts_fabric_only_dependency() {
        // Quilt is wire-compatible with Fabric: a fabric-only lib resolves.
        let m = rv_for("m", &["1.20.1"], &["quilt"], vec![dep("flib", "required")]);
        let flib = rv_for("flib", &["1.20.1"], &["fabric"], vec![]);
        let (entries, issues) =
            resolve_to_fixpoint(&[m.clone()], &[m, flib], "1.20.1", "quilt");
        assert!(entries.iter().any(|e| e.project_id == "flib"));
        assert!(issues.is_empty(), "quilt accepts fabric dep: {issues:?}");
    }

    #[test]
    fn quilt_pack_fabric_dep_passes_the_validate_gate() {
        // Integration of the FULL assemble gate: resolver pulls a Fabric-only
        // lib into a Quilt pack, then validate_pack runs over the closure.
        // validate_pack must NOT flag the Fabric dep as IncompatibleLoader,
        // otherwise tool_assemble_pack would wrongly refuse a complete pack.
        let m = rv_for("m", &["1.20.1"], &["quilt"], vec![dep("flib", "required")]);
        let flib = rv_for("flib", &["1.20.1"], &["fabric"], vec![]);
        let (entries, dep_issues) =
            resolve_to_fixpoint(&[m.clone()], &[m, flib], "1.20.1", "quilt");
        let mut all = dep_issues;
        all.extend(validate_pack(&entries, "1.20.1", "quilt"));
        assert!(
            all.is_empty(),
            "the full Quilt assemble gate must accept a Fabric dep: {all:?}"
        );
    }

    #[test]
    fn fabric_pack_rejects_quilt_only_dependency() {
        // The relationship is NOT symmetric: fabric loader can't load a
        // quilt-only mod, so it must surface as unresolved.
        let m = rv_for("m", &["1.20.1"], &["fabric"], vec![dep("qlib", "required")]);
        let qlib = rv_for("qlib", &["1.20.1"], &["quilt"], vec![]);
        let (entries, issues) =
            resolve_to_fixpoint(&[m.clone()], &[m, qlib], "1.20.1", "fabric");
        assert!(!entries.iter().any(|e| e.project_id == "qlib"));
        assert!(issues.iter().any(|i| matches!(
            i,
            ValidationIssue::UnresolvedRequiredDependency { missing_project_id, .. }
            if missing_project_id == "qlib"
        )));
    }

    #[test]
    fn exact_pinned_dependency_version_is_honored() {
        // A dep that pins version_id must take THAT version, not newest.
        let consumer = ResolvedVersion {
            dependencies: vec![DepEdge {
                project_id: Some("lib".to_string()),
                version_id: Some("lib-exact".to_string()),
                dependency_type: "required".to_string(),
            }],
            ..rv("consumer", vec![])
        };
        let lib_newest = ResolvedVersion {
            version_id: "lib-newest".to_string(),
            ..rv("lib", vec![])
        };
        let lib_exact = ResolvedVersion {
            version_id: "lib-exact".to_string(),
            ..rv("lib", vec![])
        };
        let full = {
            let mut m: HashMap<String, Vec<ResolvedVersion>> = HashMap::new();
            // newest first, as Modrinth returns it
            m.insert(
                "lib".to_string(),
                vec![lib_newest.clone(), lib_exact.clone()],
            );
            m.insert("consumer".to_string(), vec![consumer.clone()]);
            m
        };
        let failed = HashMap::new();
        let (entries, issues, needed) = resolve_dependencies(
            &[consumer],
            &full,
            &failed,
            "1.20.1",
            "fabric",
        );
        assert!(needed.is_empty());
        assert!(issues.is_empty());
        let lib = entries.iter().find(|e| e.project_id == "lib").unwrap();
        assert_eq!(lib.version_id, "lib-exact", "exact pin honored, not newest");
    }

    #[test]
    fn unpinned_dependency_prefers_release_then_newest_order_independent() {
        let consumer = ResolvedVersion {
            dependencies: vec![DepEdge {
                project_id: Some("lib".to_string()),
                version_id: None,
                dependency_type: "required".to_string(),
            }],
            ..rv("consumer", vec![])
        };
        let mk = |vid: &str, vt: &str, date: &str| ResolvedVersion {
            version_id: vid.to_string(),
            version_type: vt.to_string(),
            date_published: date.to_string(),
            ..rv("lib", vec![])
        };
        let beta_new = mk("lib-beta", "beta", "2024-06-01T00:00:00Z");
        let rel_old = mk("lib-rel", "release", "2024-03-01T00:00:00Z");
        let rel_new = mk("lib-rel2", "release", "2024-05-01T00:00:00Z");
        let alpha_newest = mk("lib-alpha", "alpha", "2024-09-01T00:00:00Z");

        // The newest *release* must win over a newer beta/alpha, and the pick
        // must not depend on the order the driver fetched candidates in.
        for order in [
            vec![
                beta_new.clone(),
                rel_old.clone(),
                rel_new.clone(),
                alpha_newest.clone(),
            ],
            vec![
                alpha_newest.clone(),
                rel_new.clone(),
                beta_new.clone(),
                rel_old.clone(),
            ],
            vec![
                rel_old.clone(),
                alpha_newest.clone(),
                beta_new.clone(),
                rel_new.clone(),
            ],
        ] {
            let mut full: HashMap<String, Vec<ResolvedVersion>> = HashMap::new();
            full.insert("lib".to_string(), order);
            full.insert("consumer".to_string(), vec![consumer.clone()]);
            let (entries, issues, needed) = resolve_dependencies(
                &[consumer.clone()],
                &full,
                &HashMap::new(),
                "1.20.1",
                "fabric",
            );
            assert!(needed.is_empty() && issues.is_empty());
            let lib = entries.iter().find(|e| e.project_id == "lib").unwrap();
            assert_eq!(
                lib.version_id, "lib-rel2",
                "newest release wins over newer beta/alpha, regardless of order"
            );
        }
    }

    #[test]
    fn mrpack_index_is_valid_and_references_files() {
        let meta = PackMeta {
            name: "Test Pack".into(),
            version_id: "0.1.0".into(),
            summary: "a test".into(),
            mc_version: "1.21.1".into(),
            loader_key: "fabric-loader".into(),
            loader_version: "0.16.0".into(),
        };
        let mods = vec![mod_entry("sodium", &["1.21.1"], &["fabric"])];
        let json = index_json(&meta, &mods);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["formatVersion"], 1);
        assert_eq!(parsed["game"], "minecraft");
        assert_eq!(parsed["files"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["files"][0]["path"], "mods/sodium.jar");
        assert_eq!(parsed["dependencies"]["minecraft"], "1.21.1");
        assert_eq!(parsed["dependencies"]["fabric-loader"], "0.16.0");
    }

    #[test]
    fn write_mrpack_produces_readable_zip() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("test.mrpack");
        let meta = PackMeta {
            name: "Z".into(),
            version_id: "1".into(),
            summary: "s".into(),
            mc_version: "1.21.1".into(),
            loader_key: "neoforge".into(),
            loader_version: "21.1.0".into(),
        };
        let mods = vec![mod_entry("jei", &["1.21.1"], &["neoforge"])];
        write_mrpack(&meta, &mods, &out).unwrap();

        let f = std::fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(f).unwrap();
        let mut entry = archive.by_name("modrinth.index.json").unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut entry, &mut s).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["files"][0]["path"], "mods/jei.jar");
        assert_eq!(parsed["dependencies"]["neoforge"], "21.1.0");
    }

    #[test]
    fn mrpack_write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("rt.mrpack");
        let meta = PackMeta {
            name: "Round Trip".into(),
            version_id: "1.0.0".into(),
            summary: "rt".into(),
            mc_version: "1.21.1".into(),
            loader_key: "fabric-loader".into(),
            loader_version: "0.16.0".into(),
        };
        let mods = vec![mod_entry("sodium", &["1.21.1"], &["fabric"])];
        write_mrpack(&meta, &mods, &out).unwrap();

        let imported = read_mrpack(&out).unwrap();
        assert_eq!(imported.name, "Round Trip");
        assert_eq!(imported.mc_version, "1.21.1");
        // exercises the loader-key reverse mapping (fabric-loader -> fabric)
        assert_eq!(imported.loader, "fabric");
        assert_eq!(imported.loader_version, "0.16.0");
        assert_eq!(imported.mods.len(), 1);
        let m = &imported.mods[0];
        assert_eq!(m.path, "mods/sodium.jar");
        assert_eq!(m.name, "sodium.jar");
        assert_eq!(m.sha1, "a".repeat(40));
        assert_eq!(m.sha512, "b".repeat(128));
        assert_eq!(m.file_size, 1024);
        assert!(m.download_url.starts_with("https://cdn.modrinth.com/"));
    }
}

#[cfg(test)]
mod tier2_tests {
    use super::*;
    use crate::registry::{is_open_ended_range, JarManifest};

    fn rvd(pid: &str, vid: &str, date: &str) -> ResolvedVersion {
        ResolvedVersion {
            project_id: pid.into(),
            version_id: vid.into(),
            path: format!("mods/{pid}.jar"),
            sha1: "a".repeat(40),
            sha512: "b".repeat(128),
            downloads: vec![format!("https://cdn.modrinth.com/{pid}.jar")],
            file_size: 1,
            game_versions: vec!["1.20.1".into()],
            loaders: vec!["fabric".into()],
            client_side: "required".into(),
            server_side: "optional".into(),
            dependencies: vec![],
            version_number: vid.into(),
            version_type: "release".into(),
            date_published: date.into(),
        }
    }
    fn entry(pid: &str, vid: &str) -> ModEntry {
        ModEntry {
            project_id: pid.into(),
            version_id: vid.into(),
            path: format!("mods/{pid}.jar"),
            sha1: "a".repeat(40),
            sha512: "b".repeat(128),
            downloads: vec![format!("https://cdn.modrinth.com/{pid}.jar")],
            file_size: 1,
            game_versions: vec!["1.20.1".into()],
            loaders: vec!["fabric".into()],
            client_side: "required".into(),
            server_side: "optional".into(),
        }
    }
    fn man(provided: &[&str], requires: &[(&str, &str)]) -> JarManifest {
        JarManifest {
            // Presence-only test fixtures: pair each modid with an empty
            // version (these tests never assert provided versions).
            provided: provided
                .iter()
                .map(|s| (s.to_string(), String::new()))
                .collect(),
            requires: requires
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            breaks: Vec::new(),
            version: String::new(),
        }
    }
    /// `man` with the provider's own declared version (for the exact-pin audit).
    fn man_v(provided: &[&str], requires: &[(&str, &str)], version: &str) -> JarManifest {
        let mut m = man(provided, requires);
        m.version = version.to_string();
        m
    }

    #[test]
    fn open_ended_range_classification() {
        // The real failing cases must read as open-ended.
        assert!(is_open_ended_range(">=0.5.1-f"));
        assert!(is_open_ended_range(">=1.9.2+kotlin.1.8.10"));
        assert!(is_open_ended_range("*"));
        assert!(is_open_ended_range("[47,)")); // >=47, no ceiling
        // Bounded ones must NOT be touched by Tier 2.
        assert!(!is_open_ended_range(">=0.5.1 <0.6"));
        assert!(!is_open_ended_range("[0.5,0.6)"));
        assert!(!is_open_ended_range("1.0.0"));
        assert!(!is_open_ended_range("=1.0.0"));
        assert!(!is_open_ended_range("[1.0]"));
    }

    /// The create_dd case: create_dd (2024) declares `create >=0.5.1-f`; the
    /// resolver picked Create 6 (2026). Tier 2 must floor `create` back to the
    /// newest version published within grace of create_dd.
    #[test]
    fn floors_open_ended_major_bump() {
        let chosen = vec![entry("createdd", "dd-1"), entry("create", "c6")];
        let mut manifests = std::collections::HashMap::new();
        manifests.insert(
            "createdd".to_string(),
            man(&["create_dd"], &[("create", ">=0.5.1-f")]),
        );
        manifests.insert("create".to_string(), man(&["create"], &[]));
        let mut pool = std::collections::HashMap::new();
        pool.insert(
            "createdd".to_string(),
            vec![rvd("createdd", "dd-1", "2024-01-15T00:00:00Z")],
        );
        pool.insert(
            "create".to_string(),
            vec![
                rvd("create", "c05", "2024-02-01T00:00:00Z"),
                rvd("create", "c6", "2026-02-01T00:00:00Z"),
            ],
        );
        let (repins, _) = version_floor_repins(
            &chosen,
            &manifests,
            &pool,
            "1.20.1",
            "fabric",
            90,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(repins, vec![("create".to_string(), "c05".to_string())]);
    }

    /// Step 4 general re-pin — the real Indium/Immersive-Portals/Sodium
    /// crash. Indium needs `sodium >=0.5.11 <0.6`; IP `breaks` sodium
    /// outside 0.5.13; the resolver pinned 0.5.8. The general block must
    /// re-pin sodium to 0.5.13 — the ONLY version satisfying the whole
    /// expressed conjunction — with NO FLK/date heuristic involved.
    #[test]
    fn general_repin_resolves_expressed_constraint_conjunction() {
        let chosen = vec![
            entry("sodium", "0.5.8+mc1.20.1"),
            entry("indium", "i-1"),
            entry("immersive_portals", "ip-1"),
        ];
        let mut manifests = std::collections::HashMap::new();
        manifests.insert("sodium".to_string(), man(&["sodium"], &[]));
        manifests.insert(
            "indium".to_string(),
            man(&["indium"], &[("sodium", ">=0.5.11 <0.6")]),
        );
        let mut ip = man(&["immersive_portals"], &[]);
        ip.breaks = vec![(
            "sodium".to_string(),
            "<0.5.13 || >0.5.13".to_string(),
        )];
        manifests.insert("immersive_portals".to_string(), ip);
        let mut pool = std::collections::HashMap::new();
        pool.insert(
            "sodium".to_string(),
            vec![
                rvd("sodium", "0.5.8+mc1.20.1", "2024-02-01T00:00:00Z"),
                rvd("sodium", "0.5.11+mc1.20.1", "2024-06-01T00:00:00Z"),
                rvd("sodium", "0.5.13+mc1.20.1", "2024-09-01T00:00:00Z"),
            ],
        );
        let (repins, issues) = version_floor_repins(
            &chosen,
            &manifests,
            &pool,
            "1.20.1",
            "fabric",
            90,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(
            repins,
            vec![(
                "sodium".to_string(),
                "0.5.13+mc1.20.1".to_string()
            )],
            "0.5.13 is the only version in [0.5.11,0.6) AND outside IP's break"
        );
        assert!(issues.is_empty(), "solvable ⇒ no hard issue; got {issues:?}");
    }

    /// Genuinely unsatisfiable expressed constraint ⇒ the general block
    /// makes NO pick (the hard `VersionConstraintUnsatisfied` then comes
    /// from `check_version_constraints` on the unfixed closure) — never a
    /// wrong pick. IP demands ==0.5.13 while Indium demands >=0.5.14.
    #[test]
    fn general_repin_makes_no_pick_when_unsatisfiable() {
        let chosen = vec![
            entry("sodium", "0.5.8+mc1.20.1"),
            entry("indium", "i-1"),
            entry("immersive_portals", "ip-1"),
        ];
        let mut manifests = std::collections::HashMap::new();
        manifests.insert("sodium".to_string(), man(&["sodium"], &[]));
        manifests.insert(
            "indium".to_string(),
            man(&["indium"], &[("sodium", ">=0.5.14")]),
        );
        let mut ip = man(&["immersive_portals"], &[]);
        ip.breaks = vec![(
            "sodium".to_string(),
            "<0.5.13 || >0.5.13".to_string(),
        )];
        manifests.insert("immersive_portals".to_string(), ip);
        let mut pool = std::collections::HashMap::new();
        pool.insert(
            "sodium".to_string(),
            vec![
                rvd("sodium", "0.5.8+mc1.20.1", "2024-02-01T00:00:00Z"),
                rvd("sodium", "0.5.13+mc1.20.1", "2024-09-01T00:00:00Z"),
                rvd("sodium", "0.5.20+mc1.20.1", "2025-01-01T00:00:00Z"),
            ],
        );
        let (repins, _) = version_floor_repins(
            &chosen,
            &manifests,
            &pool,
            "1.20.1",
            "fabric",
            90,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        );
        assert!(
            repins.is_empty(),
            "no version satisfies >=0.5.14 AND ==0.5.13 — must not wrong-pick, got {repins:?}"
        );
    }

    /// The real IPN/libIPN case: both declare `>=1.9.2+kotlin.1.8.10`
    /// (Kotlin 1.x) but the resolver pulled FLK `1.13.11+kotlin.2.3.21`
    /// (Kotlin 2.x), whose reflection ABI crashed IPN at world-join. Both FLK
    /// builds here are recent (dates within grace), so ONLY the embedded
    /// `+kotlin.<major>` tag can distinguish them — the date heuristic can't.
    #[test]
    fn floors_flk_to_matching_kotlin_major() {
        let chosen = vec![entry("ipn", "ipn1"), entry("flk", "k2")];
        let mut manifests = std::collections::HashMap::new();
        manifests.insert(
            "ipn".to_string(),
            man(
                &["inventoryprofilesnext"],
                &[("fabric-language-kotlin", ">=1.9.2+kotlin.1.8.10")],
            ),
        );
        manifests.insert(
            "flk".to_string(),
            man(&["fabric-language-kotlin"], &[]),
        );
        let mut pool = std::collections::HashMap::new();
        pool.insert(
            "ipn".to_string(),
            vec![rvd("ipn", "ipn1", "2026-03-01T00:00:00Z")],
        );
        let mut k1 = rvd("flk", "k1", "2026-02-01T00:00:00Z");
        k1.path = "mods/fabric-language-kotlin-1.12.3+kotlin.1.9.24.jar".into();
        let mut k2 = rvd("flk", "k2", "2026-04-24T00:00:00Z");
        k2.path = "mods/fabric-language-kotlin-1.13.11+kotlin.2.3.21.jar".into();
        pool.insert("flk".to_string(), vec![k1, k2]);
        let (repins, _) = version_floor_repins(
            &chosen,
            &manifests,
            &pool,
            "1.20.1",
            "fabric",
            90,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(repins, vec![("flk".to_string(), "k1".to_string())]);
    }

    /// Chosen FLK is already Kotlin 1.x for a Kotlin-1.x requester -> no-op
    /// (must not floor a pack that is already correct).
    #[test]
    fn flk_floor_noop_when_kotlin_major_already_ok() {
        let chosen = vec![entry("ipn", "ipn1"), entry("flk", "k1")];
        let mut manifests = std::collections::HashMap::new();
        manifests.insert(
            "ipn".to_string(),
            man(
                &["inventoryprofilesnext"],
                &[("fabric-language-kotlin", ">=1.9.2+kotlin.1.8.10")],
            ),
        );
        manifests.insert(
            "flk".to_string(),
            man(&["fabric-language-kotlin"], &[]),
        );
        let mut pool = std::collections::HashMap::new();
        pool.insert(
            "ipn".to_string(),
            vec![rvd("ipn", "ipn1", "2026-03-01T00:00:00Z")],
        );
        let mut k1 = rvd("flk", "k1", "2026-02-01T00:00:00Z");
        k1.path = "mods/fabric-language-kotlin-1.12.3+kotlin.1.9.24.jar".into();
        pool.insert("flk".to_string(), vec![k1]);
        let (repins, _) = version_floor_repins(
            &chosen,
            &manifests,
            &pool,
            "1.20.1",
            "fabric",
            90,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        );
        assert!(repins.is_empty());
    }

    #[test]
    fn exact_pin_classification_and_violation() {
        assert!(is_exact_pin("0.5.1-j-build.1631+mc1.20.1"));
        assert!(is_exact_pin("1.20.1"));
        assert!(is_exact_pin("=1.0.0"));
        assert!(!is_exact_pin("*"));
        assert!(!is_exact_pin(">=6.0.7"));
        assert!(!is_exact_pin(">=1.0 <2.0"));
        assert!(!is_exact_pin("1.2.x"));
        assert!(!is_exact_pin("[1.0,2.0)"));
        assert!(!is_exact_pin("1.0 || 2.0"));
        assert!(exact_pin_violation(
            "6.0.8.1+build.1744-mc1.20.1",
            "0.5.1-j-build.1631+mc1.20.1"
        ));
        // same version, only build metadata differs -> NOT a violation
        assert!(!exact_pin_violation(
            "0.5.1-j-build.1631+other",
            "0.5.1-j-build.1631+mc1.20.1"
        ));
        // a range constraint is never a violation (never false-positive)
        assert!(!exact_pin_violation("6.0.8.1", ">=6.0.7"));
    }

    #[test]
    fn leaf_exact_pin_addon_dropped_real_createair_shape() {
        let mut manifests = std::collections::HashMap::new();
        manifests.insert(
            "caf".to_string(),
            man_v(
                &["createairfabric"],
                &[("create", "0.5.1-j-build.1631+mc1.20.1")],
                "1.0+1.20.1-26",
            ),
        );
        manifests.insert(
            "cr".to_string(),
            man_v(&["create"], &[], "6.0.8.1+build.1744-mc1.20.1"),
        );
        let entries = vec![entry("caf", "x"), entry("cr", "x")];
        let (kept, issues) = audit_version_satisfaction(&entries, &manifests);
        let pids: Vec<&str> = kept.iter().map(|e| e.project_id.as_str()).collect();
        assert_eq!(pids, vec!["cr"], "incompatible leaf must be dropped");
        assert!(matches!(
            issues.as_slice(),
            [ValidationIssue::IncompatibleAddonDropped { addon, .. }] if addon == "createairfabric"
        ));
    }

    #[test]
    fn non_leaf_exact_pin_conflict_blocks_not_drops() {
        let mut manifests = std::collections::HashMap::new();
        manifests.insert(
            "dep".to_string(),
            man_v(
                &["depmod"],
                &[("create", "0.5.1-j-build.1631+mc1.20.1")],
                "1.0",
            ),
        );
        manifests.insert(
            "cr".to_string(),
            man_v(&["create"], &[], "6.0.8.1+build.1744"),
        );
        manifests.insert(
            "oth".to_string(),
            man_v(&["othermod"], &[("depmod", "*")], "1.0"),
        );
        let entries = vec![entry("dep", "x"), entry("cr", "x"), entry("oth", "x")];
        let (kept, issues) = audit_version_satisfaction(&entries, &manifests);
        assert_eq!(kept.len(), 3, "non-leaf conflict must NOT drop (cascade)");
        assert!(matches!(
            issues.as_slice(),
            [ValidationIssue::IncompatibleDependencyPresent { .. }]
        ));
    }

    #[test]
    fn matching_pin_or_range_never_flagged() {
        let mut manifests = std::collections::HashMap::new();
        manifests.insert(
            "a".to_string(),
            man_v(&["amod"], &[("create", "6.0.8.1+mc1.20.1")], "1"),
        );
        manifests.insert(
            "b".to_string(),
            man_v(&["bmod"], &[("create", ">=6.0.7")], "1"),
        );
        manifests.insert(
            "cr".to_string(),
            man_v(&["create"], &[], "6.0.8.1+build.1744-mc1.20.1"),
        );
        let entries = vec![entry("a", "x"), entry("b", "x"), entry("cr", "x")];
        let (kept, issues) = audit_version_satisfaction(&entries, &manifests);
        assert_eq!(kept.len(), 3);
        assert!(issues.is_empty(), "no false positive, got {issues:?}");
    }

    #[test]
    fn bounded_range_is_left_alone() {
        let chosen = vec![entry("m", "m1"), entry("lib", "libnew")];
        let mut manifests = std::collections::HashMap::new();
        manifests.insert("m".to_string(), man(&["m"], &[("lib", ">=1.0 <2.0")]));
        manifests.insert("lib".to_string(), man(&["lib"], &[]));
        let mut pool = std::collections::HashMap::new();
        pool.insert(
            "m".to_string(),
            vec![rvd("m", "m1", "2024-01-01T00:00:00Z")],
        );
        pool.insert(
            "lib".to_string(),
            vec![
                rvd("lib", "libold", "2024-01-01T00:00:00Z"),
                rvd("lib", "libnew", "2026-01-01T00:00:00Z"),
            ],
        );
        let (repins, _) = version_floor_repins(
            &chosen,
            &manifests,
            &pool,
            "1.20.1",
            "fabric",
            90,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        );
        assert!(repins.is_empty(), "bounded range must not be floored");
    }

    #[test]
    fn within_grace_and_already_pinned_are_skipped() {
        let chosen = vec![entry("m", "m1"), entry("lib", "libnew")];
        let mut manifests = std::collections::HashMap::new();
        manifests.insert("m".to_string(), man(&["m"], &[("lib", ">=1.0")]));
        manifests.insert("lib".to_string(), man(&["lib"], &[]));
        let mut pool = std::collections::HashMap::new();
        pool.insert(
            "m".to_string(),
            vec![rvd("m", "m1", "2024-01-01T00:00:00Z")],
        );
        pool.insert(
            "lib".to_string(),
            vec![
                rvd("lib", "libold", "2023-12-01T00:00:00Z"),
                // 30 days after requester -> within 90d grace
                rvd("lib", "libnew", "2024-01-31T00:00:00Z"),
            ],
        );
        let none = std::collections::HashSet::new();
        assert!(
            version_floor_repins(
                &chosen,
                &manifests,
                &pool,
                "1.20.1",
                "fabric",
                90,
                &none,
                &std::collections::HashSet::new(),
            )
            .0
            .is_empty(),
            "within grace -> no floor"
        );

        // Same data but dep far newer, yet already Tier-2 pinned -> skip.
        pool.get_mut("lib").unwrap()[1].date_published =
            "2027-01-01T00:00:00Z".into();
        let mut already = std::collections::HashSet::new();
        already.insert("lib".to_string());
        assert!(
            version_floor_repins(
                &chosen,
                &manifests,
                &pool,
                "1.20.1",
                "fabric",
                90,
                &already,
                &std::collections::HashSet::new(),
            )
            .0
            .is_empty(),
            "already pinned -> skip"
        );
    }
}

#[cfg(test)]
mod constraint_check_tests {
    //! Step 3 report-only pass driven by the EXACT strings from the real
    //! "Incompatible mods found" launch crash (Indium / Immersive Portals /
    //! Sodium / LambDynamicLights / fabric-api). Not a synthetic graph — the
    //! ranges below are transcribed from the crash log.
    use super::*;
    use crate::registry::JarManifest;
    use std::collections::HashMap;

    fn entry(pid: &str) -> ModEntry {
        ModEntry {
            project_id: pid.into(),
            version_id: format!("{pid}-v"),
            path: format!("mods/{pid}.jar"),
            sha1: "a".repeat(40),
            sha512: "b".repeat(128),
            downloads: vec![format!("https://cdn.modrinth.com/{pid}.jar")],
            file_size: 1,
            game_versions: vec!["1.20.1".into()],
            loaders: vec!["fabric".into()],
            client_side: "required".into(),
            server_side: "optional".into(),
        }
    }
    fn jm(
        provided: &[(&str, &str)],
        requires: &[(&str, &str)],
        breaks: &[(&str, &str)],
    ) -> JarManifest {
        JarManifest {
            provided: provided
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            requires: requires
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            breaks: breaks
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            version: String::new(),
        }
    }

    /// Build the exact crash set; `sodium_ver` / `fle_ver` let a test flip
    /// between the broken pins and the satisfying ones.
    fn crash_set(
        sodium_ver: &str,
        fle_ver: &str,
    ) -> (Vec<ModEntry>, HashMap<String, JarManifest>) {
        let entries = ["sodium", "indium", "immersive_portals", "fabric-api", "lambdynlights"]
            .iter()
            .map(|p| entry(p))
            .collect();
        let mut m = HashMap::new();
        m.insert(
            "sodium".to_string(),
            jm(&[("sodium", sodium_ver)], &[], &[]),
        );
        // Indium 1.0.36 requires Sodium >=0.5.11 (incl) and <0.6 (excl).
        m.insert(
            "indium".to_string(),
            jm(&[("indium", "1.0.36+mc1.20.1")], &[("sodium", ">=0.5.11 <0.6")], &[]),
        );
        // Immersive Portals 5.2.0 is incompatible with any sodium before AND
        // after 0.5.13 -> the only allowed value is exactly 0.5.13.
        m.insert(
            "immersive_portals".to_string(),
            jm(
                &[("immersive_portals", "5.2.0")],
                &[],
                &[("sodium", "<0.5.13 || >0.5.13")],
            ),
        );
        // fabric-api JIJ-bundles the lifecycle-events sub-module at fle_ver.
        m.insert(
            "fabric-api".to_string(),
            jm(
                &[
                    ("fabric-api", "0.92.2+1.20.1"),
                    ("fabric-lifecycle-events-v1", fle_ver),
                ],
                &[],
                &[],
            ),
        );
        // LambDynamicLights 4.4.0 needs lifecycle-events >=2.2.22, AND
        // declares a require on a mod that is entirely ABSENT (must NOT be
        // reported here — that is UnresolvedRequiredDependency's job).
        m.insert(
            "lambdynlights".to_string(),
            jm(
                &[("lambdynlights", "4.4.0+1.20.1")],
                &[
                    ("fabric-lifecycle-events-v1", ">=2.2.22"),
                    ("a-mod-not-in-this-pack", ">=1.0"),
                ],
                &[],
            ),
        );
        (entries, m)
    }

    #[test]
    fn flags_exactly_the_three_real_crash_failures() {
        let (entries, manifests) =
            crash_set("0.5.8+mc1.20.1", "2.2.21+1.20.1");
        let issues = check_version_constraints(&entries, &manifests);

        // Exactly three, and exactly these three.
        assert_eq!(issues.len(), 3, "got: {issues:#?}");
        let has = |holder: &str, modid: &str, k: ConstraintKind| {
            issues.iter().any(|i| matches!(i,
                ValidationIssue::VersionConstraintUnsatisfied { holder: h, modid: md, kind, .. }
                if h == holder && md == modid && *kind == k))
        };
        assert!(has("indium", "sodium", ConstraintKind::Depends),
            "Indium needs sodium >=0.5.11 <0.6, present 0.5.8");
        assert!(has("immersive_portals", "sodium", ConstraintKind::Breaks),
            "Immersive Portals breaks sodium != 0.5.13, present 0.5.8");
        assert!(has("lambdynlights", "fabric-lifecycle-events-v1", ConstraintKind::Depends),
            "LDL needs lifecycle-events >=2.2.22, JIJ-bundled 2.2.21");
        // The absent dep is NOT double-reported here.
        assert!(!issues.iter().any(|i| matches!(i,
            ValidationIssue::VersionConstraintUnsatisfied { modid, .. }
            if modid == "a-mod-not-in-this-pack")),
            "missing dep must be left to UnresolvedRequiredDependency");
    }

    #[test]
    fn zero_issues_when_pins_satisfy_all_constraints() {
        // Sodium 0.5.13 satisfies BOTH Indium [0.5.11,0.6) AND Immersive
        // Portals (== 0.5.13, so NOT in "<0.5.13 || >0.5.13"); fabric-api
        // bundling lifecycle-events 2.2.22 satisfies LDL. The exact reachable
        // fix Step 4 must produce.
        let (entries, manifests) =
            crash_set("0.5.13+mc1.20.1", "2.2.22+1.20.1");
        let issues = check_version_constraints(&entries, &manifests);
        assert!(issues.is_empty(), "no false positives; got: {issues:#?}");
    }

    #[test]
    fn unparseable_range_is_skipped_not_false_flagged() {
        let entries = vec![entry("sodium"), entry("weird")];
        let mut m = HashMap::new();
        m.insert("sodium".into(), jm(&[("sodium", "0.5.8")], &[], &[]));
        m.insert(
            "weird".into(),
            jm(&[("weird", "1.0")], &[("sodium", "@@not-a-range@@")], &[]),
        );
        assert!(
            check_version_constraints(&entries, &m).is_empty(),
            "an unparseable constraint must never be a false positive"
        );
    }
}
