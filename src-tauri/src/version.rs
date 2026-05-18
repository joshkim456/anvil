//! Fabric Loader dependency-predicate version matcher.
//!
//! This is a **standalone, pure** module. It parses and evaluates the version
//! grammar that appears in `fabric.mod.json` `depends`/`breaks` values and Forge
//! `mods.toml` `versionRange`. It is intentionally NOT SemVer / Cargo / npm:
//! Fabric build metadata after `+` is *semantically significant* (it orders the
//! `fabric-language-kotlin` `+kotlin.X.Y.Z` tags), and the predicate syntax is a
//! superset (bare exact, `.x` wildcards, Maven intervals, `||`/space AND).
//!
//! The exact-pin string helpers it supersedes were deleted in Step 5
//! (`pack.rs::strip_build_meta`/`is_exact_pin`/`exact_pin_violation` →
//! `VersionReq::is_exact` + `satisfies`).
//!
//! **`pack.rs::kotlin_major` is NOT superseded and STAYS** (premise
//! corrected in Step 4). It looks like this module should subsume it, but
//! it cannot: IPN/libIPN declare an OPEN-ENDED `>=1.9.2+kotlin.1.8.10` with
//! no upper bound, and correct general semver SATISFIES that with Kotlin-2.x
//! (core `1.13 > 1.9`). The Kotlin-major break "has no metadata expressing
//! it" (resolver memory #2) — the FLK floor is structural inference on top
//! of what the dependent wrote, not something a matcher can recover from the
//! constraint text. This module only proves a kotlin band is expressible
//! WHEN the bound is explicit; the real dependents never write that bound.
//!
//! # Version comparison order (THE canonical rule — later steps lean on this)
//!
//! A version is `core` (the dotted part before `+`) plus `build` (after `+`).
//! [`Version`] implements [`Ord`] as a strict two-stage compare:
//!
//! 1. **Core first.** Compare core segments left-to-right. A segment that is
//!    all-ASCII-digits compares *numerically*; otherwise it compares
//!    *lexically*. A missing segment is treated as `0` (so `1.2` == `1.2.0`).
//! 2. **Build second, only when the core is exactly equal.** Split the build
//!    on `.` and compare segment-wise with the same numeric-aware rule. A
//!    missing build segment is treated as `0`.
//!
//! Consequences (all covered by tests below):
//! * Core dominates: `1.9.2+kotlin.1.8.10` < `1.13.11+kotlin.2.3.21` purely by
//!   core (`9` < `13` at segment 2 — numeric, not lexical).
//! * Build only breaks ties: `1.10.20+kotlin.1.9.24` <
//!   `1.10.20+kotlin.2.0.0` because the cores tie and `kotlin` == `kotlin`
//!   then `1` < `2` numerically. This makes a kotlin band expressible *as a
//!   range* — but only when an EXPLICIT upper bound is written; the real
//!   IPN/libIPN constraint is unbounded, so `pack.rs::kotlin_major` is NOT
//!   superseded (see the premise note above).
//! * No-build vs build: `1.0.0` < `1.0.0+1` (missing build segment is `0`,
//!   `0 < 1` numeric). Documented & tested so step 5 is not surprised.
//!
//! # `satisfies` vs raw `Ord` — a deliberate ASYMMETRY (load-bearing)
//!
//! [`Ord`] is a total order over `Version` and DOES include build (so
//! `0.5.13+mc1.20.1` > `0.5.13`, and `…+kotlin.1.x` < `…+kotlin.2.x`). It is
//! used for "pick the highest available version" style choices.
//!
//! [`satisfies`] does NOT mirror `Ord` for the relational operators. A
//! relational bound (`>= > <= <`) compares with build **only when the BOUND
//! string itself specifies build metadata**:
//! * `>=1.9.2+kotlin.1.8.10` — bound HAS `+kotlin…` ⇒ build participates.
//!   (A *bounded* kotlin band is thus expressible as a range — but IPN's
//!   real constraint is this UNBOUNDED form, which Kotlin-2.x satisfies, so
//!   `pack.rs::kotlin_major` stays the floor; see the premise note above.)
//! * `>0.5.13` — bound has NO build ⇒ **core-only**: the real jar
//!   `0.5.13+mc1.20.1` is NOT `> 0.5.13` (its `+mc` is packaging metadata, not
//!   an author-expressed ordering). Without this, Immersive Portals'
//!   `breaks sodium "<0.5.13 || >0.5.13"` false-fires on the *only* Sodium it
//!   actually allows. (`rel_cmp` implements this; the asymmetry is asserted by
//!   `satisfies_relational_bound_without_build_is_core_only`.)
//!
//! An **exact pin** — bare `1.2.3`, `=1.2.3`, Maven `[1.2.3]` — is likewise
//! **core-only** on both sides (real candidates carry `+mc…`, authors pin the
//! bare release). This mirrors today's `strip_build_meta`/`is_exact_pin`, so
//! deleting them in step 5 changes nothing. The single load-bearing invariant:
//! `Ord` includes build; predicates include build IFF the bound opted in.

use std::cmp::Ordering;

/// A parsed version: numeric/lexical dotted `core`, plus optional `build`
/// metadata (everything after the first `+`). Both are stored as the raw
/// segment strings so comparison can be numeric-aware per segment.
#[derive(Debug, Clone)]
pub struct Version {
    /// Core segments (split on `.`), e.g. `["1", "20", "1"]`.
    core: Vec<String>,
    /// Build segments after `+`, split on `.`, e.g. `["kotlin", "2", "3",
    /// "21"]`. Empty when there is no `+build`.
    build: Vec<String>,
}

// Equality is SEMANTIC (via `Ord`), not structural: `1.2` == `1.2.0` because a
// missing segment is `0`. Structural derives would make them unequal and bite
// later steps that compare parsed versions.
impl PartialEq for Version {
    fn eq(&self, other: &Version) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Version {}

impl Version {
    /// Parse a single concrete version string. Tolerates a leading `=`,
    /// surrounding whitespace, and an optional `v` prefix. Returns `None` only
    /// for an empty / coreless string.
    ///
    /// ```
    /// # use anvil_lib::version::Version;
    /// // Core dominates regardless of build (numeric segment compare):
    /// assert!(Version::parse("1.9.2+kotlin.1.8.10").unwrap()
    ///       < Version::parse("1.13.11+kotlin.2.3.21").unwrap());
    /// // Build only breaks a core tie:
    /// assert!(Version::parse("1.10.20+kotlin.1.9.24").unwrap()
    ///       < Version::parse("1.10.20+kotlin.2.0.0").unwrap());
    /// // Missing core segment treated as 0:
    /// assert_eq!(Version::parse("1.2").unwrap(),
    ///            Version::parse("1.2.0").unwrap());
    /// // No-build < has-build (missing build segment is 0):
    /// assert!(Version::parse("1.0.0").unwrap()
    ///       < Version::parse("1.0.0+1").unwrap());
    /// ```
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('=').unwrap_or(s).trim();
        // Tolerate a leading `v` only when followed by a digit (so we never
        // eat a real lexical segment that happens to start with v).
        let s = match s.strip_prefix('v') {
            Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
            _ => s,
        };
        if s.is_empty() {
            return None;
        }
        let (core_str, build_str) = match s.split_once('+') {
            Some((c, b)) => (c, b),
            None => (s, ""),
        };
        let core_str = core_str.trim();
        if core_str.is_empty() {
            return None;
        }
        // Pre-release `-tag` is kept as part of the trailing core segment so it
        // orders lexically after the bare number on a core tie (Fabric does not
        // give pre-releases lower precedence the way SemVer does; observed packs
        // rely on `0.5.1-f` ordering near `0.5.1`).
        let core: Vec<String> = core_str.split('.').map(|x| x.trim().to_string()).collect();
        // A real Fabric/Maven version always has at least one purely-numeric
        // core segment. Requiring it rejects junk like `not` / `version!!`
        // (the caller then treats the whole predicate as unparseable) instead
        // of silently accepting it as a lexical "version".
        if !core
            .iter()
            .any(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        {
            return None;
        }
        let build: Vec<String> = if build_str.trim().is_empty() {
            Vec::new()
        } else {
            build_str.split('.').map(|x| x.trim().to_string()).collect()
        };
        Some(Version { core, build })
    }

    /// Compare only the dotted core (build metadata ignored). This is the
    /// exact-pin / wildcard / Maven-interval comparison surface.
    fn cmp_core(&self, other: &Version) -> Ordering {
        cmp_segments(&self.core, &other.core)
    }
}

impl Ord for Version {
    /// Two-stage: core first, then build only on a core tie. See module docs.
    fn cmp(&self, other: &Version) -> Ordering {
        match cmp_segments(&self.core, &other.core) {
            Ordering::Equal => cmp_segments(&self.build, &other.build),
            ord => ord,
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compare two segment lists element-wise. Each segment compares numerically
/// when *both* sides are all-ASCII-digits, else lexically. Missing segments are
/// treated as `"0"` so `1.2` == `1.2.0` and `1.0.0` < `1.0.0+1`.
fn cmp_segments(a: &[String], b: &[String]) -> Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).map(String::as_str).unwrap_or("0");
        let y = b.get(i).map(String::as_str).unwrap_or("0");
        let ord = cmp_segment(x, y);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

fn cmp_segment(x: &str, y: &str) -> Ordering {
    let x_num = !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit());
    let y_num = !y.is_empty() && y.bytes().all(|c| c.is_ascii_digit());
    if x_num && y_num {
        // Parse may overflow on absurd inputs; fall back to length-then-lex
        // which is correct for non-negative decimal integers.
        match (x.parse::<u128>(), y.parse::<u128>()) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            _ => x
                .trim_start_matches('0')
                .len()
                .cmp(&y.trim_start_matches('0').len())
                .then_with(|| x.cmp(y)),
        }
    } else {
        x.cmp(y)
    }
}

/// Comparison for a relational predicate (`>= > <= <`): include build
/// metadata IFF the bound carries it. A bound with build (`…+kotlin.1.8.10`)
/// opts into build-aware ordering (FLK); a plain bound (`0.5.13`) is core-only
/// so a packaging-only suffix on the real jar (`0.5.13+mc1.20.1`) does not
/// shift it across the bound.
fn rel_cmp(v: &Version, bound: &Version) -> Ordering {
    if bound.build.is_empty() {
        v.cmp_core(bound)
    } else {
        v.cmp(bound)
    }
}

/// One relational comparator predicate over the core (`>= > <= < =`) or a
/// half-open `[lo, hi)` interval (used by wildcards and Maven ranges).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Predicate {
    /// Matches anything (`*`, empty, `(,)`).
    Any,
    /// Exact pin: core-only equality (build ignored on both sides).
    Exact(Version),
    /// `>= bound` — full Ord (core+build).
    Gte(Version),
    /// `> bound` — full Ord.
    Gt(Version),
    /// `<= bound` — full Ord.
    Lte(Version),
    /// `< bound` — full Ord.
    Lt(Version),
    /// Half-open / closed interval. `lo`/`hi` carry inclusivity. `None` bound
    /// means open on that side. Bounds compare on the **core** (wildcards and
    /// Maven ranges are core-level constructs).
    Interval {
        lo: Option<(Version, bool)>, // (bound, inclusive)
        hi: Option<(Version, bool)>,
    },
}

/// A parsed predicate string: top-level OR of clauses, each an AND of
/// [`Predicate`]s. `*`/empty parse to a single `Any`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReq {
    /// OR clauses; a clause is satisfied iff *all* its predicates hold.
    or: Vec<Vec<Predicate>>,
}

impl VersionReq {
    /// True iff this requirement is a single EXACT pin — one concrete
    /// version, no range/wildcard/interval/OR/AND. This is the semantic
    /// successor of `pack.rs::is_exact_pin`: the post-resolution exact-pin
    /// audit acts only where non-satisfaction is unambiguous, and an `Exact`
    /// predicate compares core-only on both sides (build metadata ignored —
    /// see the module-level asymmetry note), which is exactly what the old
    /// `strip_build_meta`/`exact_pin_violation` pair did.
    pub fn is_exact(&self) -> bool {
        matches!(self.or.as_slice(), [clause]
            if matches!(clause.as_slice(), [Predicate::Exact(_)]))
    }

    /// Parse a predicate string exactly as it appears in `fabric.mod.json`
    /// `depends`/`breaks` or Forge `mods.toml` `versionRange`.
    ///
    /// Returns `None` on a string we genuinely cannot parse. The caller treats
    /// `None` as "unknown ⇒ skip this edge" (policy is decided by later steps;
    /// this module never panics and never silently match-alls a malformed
    /// string).
    ///
    /// ```
    /// # use anvil_lib::version::{Version, VersionReq, satisfies};
    /// let r = VersionReq::parse(">=0.5.11 <0.6").unwrap();
    /// assert!(!satisfies(&Version::parse("0.5.8").unwrap(), &r));
    /// assert!( satisfies(&Version::parse("0.5.13").unwrap(), &r));
    /// assert!(VersionReq::parse("not a version!!").is_none());
    /// ```
    pub fn parse(input: &str) -> Option<VersionReq> {
        let input = input.trim();
        if input.is_empty() || input == "*" {
            return Some(VersionReq {
                or: vec![vec![Predicate::Any]],
            });
        }

        let mut or: Vec<Vec<Predicate>> = Vec::new();
        for clause in split_top_level(input, "||") {
            let clause = clause.trim();
            if clause.is_empty() {
                continue;
            }
            let mut preds: Vec<Predicate> = Vec::new();
            for tok in tokenize_and(clause) {
                let tok = tok.trim();
                if tok.is_empty() {
                    continue;
                }
                preds.push(parse_predicate(tok)?);
            }
            if preds.is_empty() {
                return None;
            }
            or.push(preds);
        }
        if or.is_empty() {
            return None;
        }
        Some(VersionReq { or })
    }
}

/// Split `s` on `sep` but never inside a `[...]`/`(...)` Maven interval.
fn split_top_level(s: &str, sep: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let sep_b = sep.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && bytes[i..].starts_with(sep_b) {
            out.push(s[start..i].to_string());
            i += sep_b.len();
            start = i;
            continue;
        }
        i += 1;
    }
    out.push(s[start..].to_string());
    out
}

/// Split an AND clause into predicate tokens on whitespace, but keep a
/// `[..]`/`(..)` Maven interval (which may contain `,` and spaces) intact.
fn tokenize_and(clause: &str) -> Vec<String> {
    let bytes = clause.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' | b'(' => {
                depth += 1;
                cur.push(bytes[i] as char);
            }
            b']' | b')' => {
                depth -= 1;
                cur.push(bytes[i] as char);
            }
            b' ' | b'\t' if depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(clause[i..].chars().next().unwrap()),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Parse one whitespace-free predicate token (operator form, wildcard, bare
/// exact, or a whole Maven interval).
fn parse_predicate(tok: &str) -> Option<Predicate> {
    let tok = tok.trim();
    if tok.is_empty() || tok == "*" {
        return Some(Predicate::Any);
    }

    // Maven interval: `[1,2)`, `(,2.0)`, `[47,)`, `[1.0]`.
    if tok.starts_with('[') || tok.starts_with('(') {
        return parse_interval(tok);
    }

    // Caret / tilde on the numeric core.
    if let Some(rest) = tok.strip_prefix('^') {
        return caret(rest);
    }
    if let Some(rest) = tok.strip_prefix('~') {
        return tilde(rest);
    }

    // Relational operators (order matters: two-char before one-char).
    for (op, ctor) in [
        (">=", 0u8),
        ("<=", 1),
        (">", 2),
        ("<", 3),
        ("=", 4),
    ] {
        if let Some(rest) = tok.strip_prefix(op) {
            let v = Version::parse(rest)?;
            return Some(match ctor {
                0 => Predicate::Gte(v),
                1 => Predicate::Lte(v),
                2 => Predicate::Gt(v),
                3 => Predicate::Lt(v),
                _ => Predicate::Exact(v),
            });
        }
    }

    // Wildcard: `1.2.x`, `1.x`, `1.2.*` (but NOT a bare `*`, handled above).
    if let Some(p) = wildcard(tok) {
        return Some(p);
    }

    // Bare => exact pin on the core.
    Some(Predicate::Exact(Version::parse(tok)?))
}

/// `1.2.x` / `1.2.*` / `1.x` => half-open core interval `[a, a-with-next-
/// significant-bumped)`. `1.x` => `[1.0.0, 2.0.0)`; `1.2.x` => `[1.2.0,
/// 1.3.0)`. Returns `None` if the token is not a trailing-wildcard form.
fn wildcard(tok: &str) -> Option<Predicate> {
    let parts: Vec<&str> = tok.split('.').collect();
    let last = parts.last()?;
    if *last != "x" && *last != "X" && *last != "*" {
        return None;
    }
    // Every preceding segment must be a plain non-negative integer.
    let prefix = &parts[..parts.len() - 1];
    if prefix.is_empty() {
        return None; // a lone `*` is Any, handled before this is reached
    }
    let mut nums: Vec<u128> = Vec::with_capacity(prefix.len());
    for p in prefix {
        nums.push(p.parse::<u128>().ok()?);
    }
    let lo = Version {
        core: nums.iter().map(|n| n.to_string()).collect(),
        build: Vec::new(),
    };
    // Bump the last fixed segment for the exclusive upper bound.
    let mut hi_nums = nums.clone();
    *hi_nums.last_mut().unwrap() += 1;
    let hi = Version {
        core: hi_nums.iter().map(|n| n.to_string()).collect(),
        build: Vec::new(),
    };
    Some(Predicate::Interval {
        lo: Some((lo, true)),
        hi: Some((hi, false)),
    })
}

/// `^X` — allow changes that do not modify the left-most non-zero core
/// segment (npm/Cargo semantics; documented choice). `^1.2.3` =>
/// `[1.2.3, 2.0.0)`; `^0.2.3` => `[0.2.3, 0.3.0)`; `^0.0.3` => `[0.0.3,
/// 0.0.4)`.
fn caret(rest: &str) -> Option<Predicate> {
    let v = Version::parse(rest)?;
    let nums = numeric_core(&v)?;
    let mut hi = vec![0u128; nums.len().max(3)];
    let lead = nums.iter().position(|&n| n != 0);
    match lead {
        Some(idx) => {
            hi[idx] = nums[idx] + 1;
        }
        None => {
            // all-zero core: ^0.0.0 => only 0.0.0
            return Some(Predicate::Exact(v));
        }
    }
    finish_range(v, hi)
}

/// `~X` — allow patch-level changes (npm/Cargo). `~1.2.3` => `[1.2.3,
/// 1.3.0)`; `~1.2` => `[1.2.0, 1.3.0)`; `~1` => `[1.0.0, 2.0.0)`.
fn tilde(rest: &str) -> Option<Predicate> {
    let v = Version::parse(rest)?;
    let nums = numeric_core(&v)?;
    let mut hi = vec![0u128; nums.len().max(3)];
    if nums.len() >= 2 {
        // bump the minor (index 1), keep major
        hi[0] = nums[0];
        hi[1] = nums[1] + 1;
    } else {
        // `~1` => bump the major
        hi[0] = nums[0] + 1;
    }
    finish_range(v, hi)
}

fn numeric_core(v: &Version) -> Option<Vec<u128>> {
    v.core.iter().map(|s| s.parse::<u128>().ok()).collect()
}

fn finish_range(lo: Version, hi_nums: Vec<u128>) -> Option<Predicate> {
    let hi = Version {
        core: hi_nums.iter().map(|n| n.to_string()).collect(),
        build: Vec::new(),
    };
    Some(Predicate::Interval {
        lo: Some((lo, true)),
        hi: Some((hi, false)),
    })
}

/// Maven/Forge interval: `[1.2.3,2.0.0)`, `[47,)`, `(,2.0)`, `[1.0]`.
/// `[`/`]` inclusive, `(`/`)` exclusive. A single value `[1.0]` is an exact
/// pin (core-only, per module docs).
fn parse_interval(tok: &str) -> Option<Predicate> {
    let bytes = tok.as_bytes();
    let open = *bytes.first()?;
    let close = *bytes.last()?;
    if (open != b'[' && open != b'(') || (close != b']' && close != b')') {
        return None;
    }
    let lo_incl = open == b'[';
    let hi_incl = close == b']';
    let inner = &tok[1..tok.len() - 1];

    if !inner.contains(',') {
        // `[1.0]` exact pin (only valid for inclusive brackets).
        let v = Version::parse(inner.trim())?;
        if lo_incl && hi_incl {
            return Some(Predicate::Exact(v));
        }
        return None;
    }
    let (lo_s, hi_s) = inner.split_once(',')?;
    let lo_s = lo_s.trim();
    let hi_s = hi_s.trim();
    let lo = if lo_s.is_empty() {
        None
    } else {
        Some((Version::parse(lo_s)?, lo_incl))
    };
    let hi = if hi_s.is_empty() {
        None
    } else {
        Some((Version::parse(hi_s)?, hi_incl))
    };
    if lo.is_none() && hi.is_none() {
        return Some(Predicate::Any); // `(,)`
    }
    Some(Predicate::Interval { lo, hi })
}

impl Predicate {
    fn matches(&self, v: &Version) -> bool {
        match self {
            Predicate::Any => true,
            // Exact / interval bounds are CORE comparisons (build ignored).
            Predicate::Exact(b) => v.cmp_core(b) == Ordering::Equal,
            // Relational operators compare with build metadata ONLY when the
            // BOUND itself specifies it (e.g. FLK `>=1.9.2+kotlin.1.8.10`):
            // there the `+kotlin.*` tag is author-significant. A plain bound
            // like `>0.5.13` is a core comparison — otherwise the real Sodium
            // jar `0.5.13+mc1.20.1` reads as `> 0.5.13` and Immersive Portals'
            // `breaks sodium "<0.5.13 || >0.5.13"` false-fires on the only
            // version it actually allows. (`+mc1.20.1` is packaging metadata,
            // not an author-expressed ordering, unless the predicate opts in.)
            Predicate::Gte(b) => rel_cmp(v, b) != Ordering::Less,
            Predicate::Gt(b) => rel_cmp(v, b) == Ordering::Greater,
            Predicate::Lte(b) => rel_cmp(v, b) != Ordering::Greater,
            Predicate::Lt(b) => rel_cmp(v, b) == Ordering::Less,
            Predicate::Interval { lo, hi } => {
                let lo_ok = match lo {
                    None => true,
                    Some((b, incl)) => {
                        let o = v.cmp_core(b);
                        if *incl {
                            o != Ordering::Less
                        } else {
                            o == Ordering::Greater
                        }
                    }
                };
                let hi_ok = match hi {
                    None => true,
                    Some((b, incl)) => {
                        let o = v.cmp_core(b);
                        if *incl {
                            o != Ordering::Greater
                        } else {
                            o == Ordering::Less
                        }
                    }
                };
                lo_ok && hi_ok
            }
        }
    }
}

/// Does version `v` satisfy requirement `req`? `true` iff at least one OR
/// clause has *all* its predicates hold (AND binds tighter than `||`).
pub fn satisfies(v: &Version, req: &VersionReq) -> bool {
    req.or
        .iter()
        .any(|clause| clause.iter().all(|p| p.matches(v)))
}

/// Convenience: does an open-ended (no upper bound) requirement exist? This
/// lets a later step replace `registry.rs::is_open_ended_range` with a real
/// parse instead of string sniffing. Conservative: an unparseable string is
/// NOT treated as open-ended (returns `false`).
pub fn is_open_ended(req: &str) -> bool {
    let Some(r) = VersionReq::parse(req) else {
        return false;
    };
    // Open-ended iff EVERY clause lacks an upper bound (so a far-newer major
    // is structurally allowed). `Any`, bare `>=`/`>`, and `[x,)` qualify.
    r.or.iter().any(|clause| {
        clause.iter().all(|p| match p {
            Predicate::Any => true,
            Predicate::Gte(_) | Predicate::Gt(_) => true,
            Predicate::Interval { hi, .. } => hi.is_none(),
            _ => false,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).expect("version parses")
    }
    /// satisfies(version_str, req_str) — the asserted-crash shorthand.
    fn sat(ver: &str, req: &str) -> bool {
        let r = VersionReq::parse(req).expect("req parses");
        satisfies(&v(ver), &r)
    }

    // ---- comparison order (the canonical rule) -------------------------

    #[test]
    fn core_dominates_build_numeric_segments() {
        // 9 < 13 NUMERICALLY at core segment index 1 (not lexical "13"<"9").
        assert!(v("1.9.2+kotlin.1.8.10") < v("1.13.11+kotlin.2.3.21"));
        assert!(v("0.5.8") < v("0.5.11"));
        assert!(v("0.5.9") < v("0.5.13"));
    }

    #[test]
    fn build_only_breaks_a_core_tie() {
        assert_eq!(v("1.10.20+kotlin.1.9.24").cmp(&v("1.10.20+kotlin.1.9.24")),
                   Ordering::Equal);
        assert!(v("1.10.20+kotlin.1.9.24") < v("1.10.20+kotlin.2.0.0"));
        // Core differs => build is NOT consulted (13 > 10 wins outright even
        // though kotlin.1 < kotlin.2 would say otherwise).
        assert!(v("1.13.11+kotlin.1.0.0") > v("1.10.20+kotlin.2.0.0"));
    }

    #[test]
    fn missing_core_segment_is_zero() {
        // Equality is SEMANTIC: a missing core segment is 0.
        assert_eq!(v("1.2").cmp(&v("1.2.0")), Ordering::Equal);
        assert_eq!(v("1"), v("1.0.0"));
        assert_eq!(v("1.2"), v("1.2.0"));
        assert!(v("1.2") < v("1.2.1"));
    }

    #[test]
    fn no_build_is_less_than_has_build() {
        // Documented discriminator for step 5: missing build segment == 0.
        assert!(v("1.0.0") < v("1.0.0+1"));
        assert!(v("0.5.13") < v("0.5.13+mc1.20.1"));
    }

    // ---- the EXACT crash shapes ---------------------------------------

    #[test]
    fn indium_sodium_range_crash() {
        // Indium needs `sodium >=0.5.11 <0.6`; pack had 0.5.8 (crash).
        assert_eq!(sat("0.5.8", ">=0.5.11 <0.6"), false);
        assert_eq!(sat("0.5.13", ">=0.5.11 <0.6"), true);
        assert_eq!(sat("0.5.11", ">=0.5.11 <0.6"), true); // inclusive lower
        assert_eq!(sat("0.6", ">=0.5.11 <0.6"), false); // exclusive upper
        assert_eq!(sat("0.6.0", ">=0.5.11 <0.6"), false);
    }

    #[test]
    fn immersive_portals_only_0_5_13() {
        // IP effectively pins sodium == 0.5.13 (`[0.5.13]`).
        assert_eq!(sat("0.5.13", "[0.5.13]"), true);
        assert_eq!(sat("0.5.8", "[0.5.13]"), false);
        assert_eq!(sat("0.5.14", "[0.5.13]"), false);
        // Exact pin is CORE-only: a real candidate with a build tag matches.
        assert_eq!(sat("0.5.13+mc1.20.1", "[0.5.13]"), true);
        // bare `=` and bare forms agree with the bracket form.
        assert_eq!(sat("0.5.13", "=0.5.13"), true);
        assert_eq!(sat("0.5.8", "0.5.13"), false);
        assert_eq!(sat("0.5.13", "0.5.13"), true);
    }

    #[test]
    fn lambdynamiclights_lifecycle_events_floor() {
        // LDL needs `fabric-lifecycle-events-v1 >=2.2.22+1.20.1`; pack had
        // 2.2.21 (crash). Real build suffix so the test self-documents.
        assert_eq!(sat("2.2.21+1.20.1", ">=2.2.22+1.20.1"), false);
        assert_eq!(sat("2.2.22+1.20.1", ">=2.2.22+1.20.1"), true);
        assert_eq!(sat("2.2.23+1.20.1", ">=2.2.22+1.20.1"), true);
    }

    // ---- FLK: kotlin_major hack is now unnecessary --------------------

    #[test]
    fn flk_open_floor_satisfied_by_newer_core() {
        // `>=1.9.2+kotlin.1.8.10` satisfied by `1.10.20+kotlin.1.9.24`
        // purely because core 1.10.20 >= core 1.9.2.
        assert_eq!(sat("1.10.20+kotlin.1.9.24", ">=1.9.2+kotlin.1.8.10"), true);
        assert_eq!(sat("1.9.2+kotlin.1.8.10", ">=1.9.2+kotlin.1.8.10"), true);
        assert_eq!(sat("1.8.0+kotlin.1.7.0", ">=1.9.2+kotlin.1.8.10"), false);
    }

    #[test]
    fn flk_bounded_kotlin_band_is_an_ordinary_range() {
        // Proves CAPABILITY only: a kotlin band is expressible as a normal
        // range WHEN an explicit upper bound is written. It does NOT make
        // `pack.rs::kotlin_major` unnecessary — IPN's real constraint is the
        // UNBOUNDED `>=…+kotlin.1.8.10` (no upper bound), which Kotlin-2.x
        // satisfies; recovering that floor from the unbounded text is the
        // FLK heuristic's job (Step 4 keeps it; see module premise note).
        assert!(v("1.10.20+kotlin.1.9.24") < v("1.13.11+kotlin.2.3.21"));
        assert!(v("1.10.20+kotlin.1.9.24") < v("1.10.20+kotlin.2.0.0"));
        let r = ">=1.10.20+kotlin.1.0.0 <1.10.20+kotlin.2.0.0";
        assert_eq!(sat("1.10.20+kotlin.1.9.24", r), true); // kotlin 1.x band
        assert_eq!(sat("1.10.20+kotlin.2.0.0", r), false); // kotlin 2.x excluded
        // Build IS consulted on a core tie (this is what kotlin_major faked):
        assert_eq!(sat("1.10.20+kotlin.1.9.24", ">1.10.20+kotlin.1.8.0"), true);
        assert_eq!(sat("1.10.20+kotlin.1.7.0", ">1.10.20+kotlin.1.8.0"), false);
    }

    // ---- Maven / Forge intervals --------------------------------------

    #[test]
    fn maven_intervals() {
        assert_eq!(sat("52", "[47,)"), true); // open upper
        assert_eq!(sat("47", "[47,)"), true);
        assert_eq!(sat("46", "[47,)"), false);
        assert_eq!(sat("1.9", "(,2.0)"), true); // open lower, excl upper
        assert_eq!(sat("2.0", "(,2.0)"), false);
        assert_eq!(sat("1.5.0", "[1.2.3,2.0.0)"), true);
        assert_eq!(sat("2.0.0", "[1.2.3,2.0.0)"), false);
        assert_eq!(sat("1.2.3", "[1.2.3,2.0.0)"), true);
        assert_eq!(sat("1.0", "[1.0]"), true); // single-value pin
        assert_eq!(sat("1.1", "[1.0]"), false);
        // whitespace inside brackets must NOT split the AND clause.
        let r = VersionReq::parse("[1.0, 2.0)").expect("interval with space");
        assert_eq!(satisfies(&v("1.5"), &r), true);
        assert_eq!(satisfies(&v("2.0"), &r), false);
    }

    // ---- wildcards ----------------------------------------------------

    #[test]
    fn wildcards() {
        assert_eq!(sat("1.2.9", "1.2.x"), true);
        assert_eq!(sat("1.2.0", "1.2.x"), true);
        assert_eq!(sat("1.3.0", "1.2.x"), false);
        assert_eq!(sat("1.1.9", "1.2.x"), false);
        assert_eq!(sat("1.2.9", "1.2.*"), true);
        assert_eq!(sat("1.9.9", "1.x"), true);
        assert_eq!(sat("2.0.0", "1.x"), false);
        assert_eq!(sat("1.0.0", "1.x"), true);
    }

    // ---- caret / tilde ------------------------------------------------

    #[test]
    fn caret_and_tilde() {
        // ^1.2.3 => [1.2.3, 2.0.0)
        assert_eq!(sat("1.5.0", "^1.2.3"), true);
        assert_eq!(sat("2.0.0", "^1.2.3"), false);
        assert_eq!(sat("1.2.2", "^1.2.3"), false);
        // ^0.2.3 => [0.2.3, 0.3.0) (leftmost non-zero)
        assert_eq!(sat("0.2.9", "^0.2.3"), true);
        assert_eq!(sat("0.3.0", "^0.2.3"), false);
        // ~1.2.3 => [1.2.3, 1.3.0)
        assert_eq!(sat("1.2.9", "~1.2.3"), true);
        assert_eq!(sat("1.3.0", "~1.2.3"), false);
        // ~1 => [1.0.0, 2.0.0)
        assert_eq!(sat("1.9.9", "~1"), true);
        assert_eq!(sat("2.0.0", "~1"), false);
    }

    // ---- wildcard / empty / OR / precedence ---------------------------

    #[test]
    fn star_and_empty_match_all() {
        assert_eq!(sat("9.9.9", "*"), true);
        assert_eq!(sat("0.0.1", ""), true);
        assert_eq!(sat("1.2.3", "  *  "), true);
    }

    #[test]
    fn or_clauses() {
        assert_eq!(sat("1.2.9", "1.2.x || 2.0.x"), true);
        assert_eq!(sat("2.0.5", "1.2.x || 2.0.x"), true);
        assert_eq!(sat("3.0.0", "1.2.x || 2.0.x"), false);
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // (>=1 <2) || (>=3 <4)
        let r = ">=1 <2 || >=3 <4";
        assert_eq!(sat("1.5", r), true);
        assert_eq!(sat("3.5", r), true);
        assert_eq!(sat("2.5", r), false);
        assert_eq!(sat("4.5", r), false);
    }

    // ---- tolerance & error sentinel -----------------------------------

    #[test]
    fn tolerates_leading_eq_and_whitespace() {
        assert_eq!(sat("1.2.3", "  =1.2.3  "), true);
        assert_eq!(v(" =1.2.3 "), v("1.2.3"));
        assert_eq!(sat("0.5.13", "  >=0.5.11   <0.6  "), true);
    }

    #[test]
    fn unparseable_returns_none_no_panic() {
        assert!(VersionReq::parse("not a version!!").is_none());
        assert!(VersionReq::parse(">=").is_none()); // operator, no operand
        assert!(VersionReq::parse("[1.2.3").is_none()); // unbalanced
        assert!(VersionReq::parse(">=1.0 <").is_none()); // dangling operand
        assert!(Version::parse("").is_none());
        assert!(Version::parse("=").is_none());
        // A None req simply means "skip this edge" for the caller; never a panic.
    }

    #[test]
    fn is_open_ended_matches_legacy_helper_intent() {
        assert!(is_open_ended("*"));
        assert!(is_open_ended(""));
        assert!(is_open_ended(">=0.5.1-f"));
        assert!(is_open_ended(">=1.9.2+kotlin.1.8.10"));
        assert!(is_open_ended("[47,)"));
        assert!(!is_open_ended(">=0.5.11 <0.6")); // has ceiling
        assert!(!is_open_ended("[0.5,0.6)"));
        assert!(!is_open_ended("[1.0]")); // pinned
        assert!(!is_open_ended("=1.2.3"));
        assert!(!is_open_ended("not parseable!!")); // conservative: false
    }

    /// THE load-bearing asymmetry (locks the Step-3 comparator fix so Steps
    /// 4/5 cannot silently undo it): `Ord` includes build metadata, but a
    /// relational predicate whose bound has NO build is core-only.
    #[test]
    fn satisfies_relational_bound_without_build_is_core_only() {
        let real = Version::parse("0.5.13+mc1.20.1").unwrap();
        let bare = Version::parse("0.5.13").unwrap();
        // Ord DOES order by build:
        assert!(real > bare, "Ord includes build: 0.5.13+mc1.20.1 > 0.5.13");
        // satisfies with a build-less bound does NOT:
        let gt = VersionReq::parse(">0.5.13").unwrap();
        assert!(
            !satisfies(&real, &gt),
            "`>0.5.13` (no build in bound) is core-only: 0.5.13+mc1.20.1 \
             must NOT count as > 0.5.13"
        );
        // The exact Immersive Portals breaks shape on the only allowed jar:
        let ip_breaks = VersionReq::parse("<0.5.13 || >0.5.13").unwrap();
        assert!(
            !satisfies(&real, &ip_breaks),
            "0.5.13+mc1.20.1 is the version IP allows; its breaks range must \
             NOT match it"
        );
        // But a bound that OPTS IN to build still evaluates it (FLK):
        let flk = VersionReq::parse(">=1.9.2+kotlin.1.8.10").unwrap();
        assert!(
            satisfies(
                &Version::parse("1.10.20+kotlin.1.9.24").unwrap(),
                &flk
            ),
            "bound carries +kotlin ⇒ build-aware; core 1.10.20 ≥ 1.9.2"
        );
        let kotlin_band =
            VersionReq::parse(">=1.0.0+kotlin.1.0.0 <1.0.0+kotlin.2.0.0")
                .unwrap();
        assert!(
            satisfies(
                &Version::parse("1.0.0+kotlin.1.9.24").unwrap(),
                &kotlin_band
            ) && !satisfies(
                &Version::parse("1.0.0+kotlin.2.3.21").unwrap(),
                &kotlin_band
            ),
            "a BOUNDED kotlin band works via build-bearing bounds (capability \
             only — IPN's real constraint is unbounded, so pack.rs::\
             kotlin_major stays the floor; see module premise note)"
        );
    }
}
