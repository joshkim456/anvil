//! Custom Minecraft launch core (no Theseus).
//!
//! CONTRACT — preserve these public signatures so `lib.rs` compiles.
//!
//! Responsibilities: resolve Mojang piston-meta version manifest; download
//! client.jar, libraries (respecting OS/`rules`, extracting natives), the
//! asset index + objects, into the SHARED dir (`settings::shared_mc_dir()`)
//! so instances dedupe. Install the loader (Fabric fully; vanilla fully;
//! forge/neoforge bail early — see SCOPE note below). Materialise the
//! instance's pinned mods into `instance_dir/mods`. Build classpath + JVM +
//! game args, inject the signed-in account, spawn `java`, stream
//! stdout/stderr as log lines.
//!
//! Java: if the caller passes an explicit `java_path` we use it as-is (only
//! verifying it runs). If `java_path` is None, Anvil auto-provisions the
//! correct Adoptium Temurin JRE for the required major version (derived from
//! the version JSON's `javaVersion.majorVersion`, or a MC-version heuristic),
//! caching it under `shared_mc_dir()/runtimes/<major>`. See `provision_jre`.
//! macOS / Linux are the priority targets (tar.gz extracted via the system
//! `tar`); Windows uses the bundled `zip` crate on the Temurin .zip.
//!
//! SCOPE: vanilla + Fabric (+ Quilt, which is wire-compatible with Fabric's
//! `profile/json` endpoint) are fully implemented. `forge` / `neoforge`
//! bail early with a clear message — their installers run a transforming
//! processor pipeline (patched jars, ATs) that is a substantial separate
//! milestone. See `loader_unsupported_bail()`.
//!
//! Hash verification caveat: Mojang publishes SHA-1 digests but the only
//! hashing crate available here is `sha2` (SHA-256/512), which cannot do
//! SHA-1, and we may not add crates. So Mojang client/library/asset files
//! are verified by "exists + non-zero size" only. Modrinth-pinned mods are
//! verified with their SHA-512 (`PinnedMod.sha512`) via `sha2::Sha512`.
//! See `// TODO: SHA-1` markers.

use crate::auth::MinecraftAccount;
use crate::instance::{instance_dir, Instance};
use crate::settings::shared_mc_dir;
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, serde::Serialize)]
// Adjacently tagged: serde cannot serialize internally-tagged newtype variants
// wrapping a primitive (Status/Log/Exited/Error) so they silently failed to
// emit. `content = "data"` makes every variant serialize correctly.
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum LaunchEvent {
    Status(String),
    Progress { done: u64, total: u64, what: String },
    Log(String),
    Exited(i32),
    Error(String),
}

const VERSION_MANIFEST: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const RESOURCES_BASE: &str = "https://resources.download.minecraft.net";
const FABRIC_META: &str = "https://meta.fabricmc.net/v2/versions/loader";
const QUILT_META: &str = "https://meta.quiltmc.org/v3/versions/loader";

/// Concurrency for the (potentially thousands of) asset object downloads.
const ASSET_PARALLELISM: usize = 16;
/// Emit a Progress event only every N completed objects (don't spam one/file).
const PROGRESS_BATCH: u64 = 64;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Download/verify everything this instance needs (idempotent; safe to re-run).
/// Does NOT spawn the game.
pub async fn prepare(instance: &Instance, tx: UnboundedSender<LaunchEvent>) -> Result<()> {
    loader_unsupported_bail(&instance.loader)?;
    prepare_inner(instance, &tx).await?;
    let _ = tx.send(LaunchEvent::Status("Ready to launch".into()));
    Ok(())
}

/// Prepare (if needed) then launch the game. Streams `LaunchEvent`s and
/// resolves with the process exit code.
pub async fn launch(
    instance: &Instance,
    account: &MinecraftAccount,
    java_path: Option<String>,
    tx: UnboundedSender<LaunchEvent>,
) -> Result<i32> {
    loader_unsupported_bail(&instance.loader)?;

    // Fail fast: if the caller supplied an explicit Java, sanity-check it
    // BEFORE any downloads (preserves the original "don't make a user with a
    // broken JAVA_HOME sit through asset downloads to see the error").
    if let Some(p) = &java_path {
        check_java(p).await?;
    }

    // Preparation resolves the version JSON, the source of the required Java
    // major version for auto-provisioning — so it must run before we can
    // provision a JRE.
    let prepared = prepare_inner(instance, &tx).await?;

    // Resolve Java. An explicit `java_path` is used verbatim (already sanity-
    // checked above). With no explicit path we provision the correct Adoptium
    // Temurin JRE ourselves — never silently fall back to a PATH `java`.
    let java = match java_path {
        Some(p) => p,
        None => provision_jre(prepared.java_major, &tx)
            .await
            .context("auto-provisioning a JRE")?,
    };

    let _ = tx.send(LaunchEvent::Status("Building launch command".into()));
    let args = build_command_args(instance, account, &prepared)?;

    let inst_dir = instance_dir(&instance.id);
    tokio::fs::create_dir_all(&inst_dir)
        .await
        .with_context(|| format!("creating instance dir {}", inst_dir.display()))?;

    let _ = tx.send(LaunchEvent::Status("Starting Minecraft".into()));
    tracing::info!(java = %java, args = ?args, "spawning minecraft");

    let mut child = tokio::process::Command::new(&java)
        .args(&args)
        .current_dir(&inst_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn java ({java})"))?;

    // Stream BOTH stdout and stderr concurrently — if we only drain one, the
    // other pipe buffer fills and the child blocks (classic deadlock).
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx.send(LaunchEvent::Log(line));
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx.send(LaunchEvent::Log(line));
            }
        });
    }

    let status = child.wait().await.context("waiting for minecraft process")?;
    let code = status.code().unwrap_or(-1);
    let _ = tx.send(LaunchEvent::Exited(code));
    Ok(code)
}

// ---------------------------------------------------------------------------
// Tier 3: bounded headless smoke test
//
// All three failure classes we have seen surface EARLY — before the main
// menu: Fabric "Incompatible mods found" (dependency reject), an entrypoint
// `NoClassDefFoundError` (API break), a startup crash report. This boots the
// real pack exactly like `launch`, watches the log for a failure signature
// vs a "mods initialized" milestone, then kills the process. ONE boot, hard
// timeout, report-only — never an auto-retry/auto-mutate loop.
// ---------------------------------------------------------------------------

/// Per-line classification of the game log. Pure + deterministic so it is
/// unit-tested directly against the exact lines we have observed.
#[derive(Debug, Clone, PartialEq)]
pub enum SmokeSignal {
    /// Nothing decisive on this line.
    None,
    /// The pack failed to initialize. `mod_name` is the culprit when the line
    /// names it; `reason` is the human-facing cause.
    Failure {
        mod_name: Option<String>,
        reason: String,
    },
    /// Mods initialized successfully (reached the menu / auth / audio).
    Success,
}

/// Text between the first `open` and the next `close` after it, if any.
fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let i = s.find(open)? + open.len();
    let rest = &s[i..];
    let j = rest.find(close)?;
    Some(&rest[..j])
}

pub fn classify_smoke_line(line: &str) -> SmokeSignal {
    // ---- failures (earliest/most specific first) ----
    if line.contains("Incompatible mods found") {
        return SmokeSignal::Failure {
            mod_name: None,
            reason: "Fabric rejected the pack: incompatible / missing mods"
                .into(),
        };
    }
    // `Mod 'Spectrum' (spectrum) 1.8.13 requires ... revelationary, which is missing!`
    if line.contains("requires") && line.contains("which is missing") {
        let m = between(line, "Mod '", "'").map(str::to_string);
        return SmokeSignal::Failure {
            mod_name: m,
            reason: line.trim().to_string(),
        };
    }
    // `Could not execute entrypoint stage 'main' ... provided by 'create_dd'`
    if line.contains("Could not execute entrypoint") {
        let m = between(line, "provided by '", "'").map(str::to_string);
        return SmokeSignal::Failure {
            mod_name: m,
            reason: "A mod's initializer threw during startup".into(),
        };
    }
    // A class-load failure is only fatal as an actually-thrown exception
    // (`Caused by:` / `Exception in thread` / a bare `java.lang.*` top line).
    // Fabric logs MANY benign `[.../WARN]: Error loading class: X
    // (java.lang.ClassNotFoundException: X)` and `@Mixin target ... was not
    // found` probes for optional cross-mod compat — those are NOT crashes.
    if line.contains("NoClassDefFoundError")
        || line.contains("ClassNotFoundException")
    {
        let t = line.trim_start();
        let fatal = t.starts_with("Caused by:")
            || t.starts_with("Exception in thread")
            || t.starts_with("java.lang.NoClassDefFoundError")
            || t.starts_with("java.lang.ClassNotFoundException");
        let benign = line.contains("WARN")
            || line.contains("Error loading class:")
            || line.contains("@Mixin target");
        if fatal && !benign {
            return SmokeSignal::Failure {
                mod_name: None,
                reason: line.trim().to_string(),
            };
        }
    }
    if line.contains("---- Minecraft Crash Report ----")
        || line.contains("Crash report saved to")
    {
        return SmokeSignal::Failure {
            mod_name: None,
            reason: "The game crashed during startup".into(),
        };
    }
    // ---- success: a genuinely POST-entrypoint milestone ONLY.
    // `Setting user:` is logged inside class_310.<init> BEFORE Fabric invokes
    // the `main` mod entrypoints (Hooks.startClient), so treating it as success
    // false-passed every mod that throws in onInitialize — e.g. sprout, whose
    // crash report timestamps ~55s AFTER `Setting user:` (smoke_test returned
    // Ok and killed the JVM long before the crash ever printed). Sound-engine
    // init runs later in the same constructor, after all main/client
    // entrypoints, so it is a safe "the pack actually initialized" signal.
    if line.contains("Sound engine started")
        || line.contains("OpenAL initialized")
    {
        return SmokeSignal::Success;
    }
    SmokeSignal::None
}

// ---------------------------------------------------------------------------
// Tier 4: client-side world-join probe
//
// `smoke_test` boots the client to the main menu. The success milestone
// (`Sound engine started` / `OpenAL initialized`) fires BEFORE the player
// creates a world — so client-side mods that crash on `onJoinWorld`
// (Inventory Profiles Next 1.10.x kotlin reflection bug; broken mixins
// that target world-load hooks; data-pack rule loaders) sail through the
// existing verifier and crash the real user's world creation.
//
// `world_join_probe` closes that gap. It boots the same client, adds
// `--quickPlaySingleplayer <name>` so vanilla auto-creates + joins a
// throwaway world, and watches for either a post-world-join success line
// or a client-only crash signature. The crash-reports/ dir is also
// pre-snapshotted so a brand-new file there at timeout = failed pass.
// ---------------------------------------------------------------------------

/// Per-line classification specific to the world-join phase. Recognises
/// crash patterns that ONLY appear during/after the integrated server
/// hands the world to the client AND the success line that confirms the
/// client actually rendered into the world.
#[derive(Debug, Clone, PartialEq)]
pub enum WorldJoinSignal {
    None,
    /// Crash detected. `mod_name` when the line names it.
    Failure { mod_name: Option<String>, reason: String },
    /// World loaded — the client reached steady-state inside the world.
    Success,
}

pub fn classify_world_join_line(line: &str) -> WorldJoinSignal {
    // ---- failures (most specific first) ----
    //
    // IPN 1.10.x kotlin reflection error — the exact signature from the
    // UCL: Bentham Ultimatum crash report 2026-05-21_00.05.20-client.txt.
    // Marker the kotlin-stdlib runtime cannot resolve a property that IPN
    // declares via `@ByPropertyName` — surfaces on every world join.
    if line.contains("KotlinReflectionInternalError")
        && line.contains("not resolved in file class")
    {
        return WorldJoinSignal::Failure {
            mod_name: between(line, "file class ", "Kt").map(|s| s.split('.').last().unwrap_or(s).to_string()),
            reason:
                "Kotlin reflection mismatch during world-join (a mod's @ByPropertyName delegate failed); \
                 commonly Inventory Profiles Next 1.10.x with the bundled fabric-language-kotlin."
                .into(),
        };
    }
    // Any mod's `onJoinWorld` hook throwing — the most general
    // client-side world-join failure pattern.
    if line.contains(".onJoinWorld")
        && (line.contains("Exception") || line.contains("Error"))
    {
        let m = between(line, "knot//", ".onJoinWorld")
            .or_else(|| between(line, "at ", ".onJoinWorld"))
            .map(|s| s.split('.').next().unwrap_or(s).to_string());
        return WorldJoinSignal::Failure {
            mod_name: m,
            reason: "A client mod threw during onJoinWorld".into(),
        };
    }
    // Mixin/injector crash inside a world-load class — class_310 (Minecraft)
    // is the client; class_634 (ClientPlayNetworkHandler) is world-join.
    if (line.contains("at knot//net.minecraft.class_634.method_11120")
        || line.contains("class_310.method_1481"))
        && line.contains("Caused by:")
    {
        return WorldJoinSignal::Failure {
            mod_name: None,
            reason: "Mixin failed inside the client world-join path".into(),
        };
    }
    // The vanilla crash-report write — the same one smoke_test catches,
    // but at this phase its presence MUST come from world creation or
    // world-join code (smoke_test already filtered early-boot crashes).
    if line.contains("---- Minecraft Crash Report ----")
        || line.contains("Crash report saved to")
    {
        return WorldJoinSignal::Failure {
            mod_name: None,
            reason: "Crashed during world creation / world-join".into(),
        };
    }
    // ---- probe-implementation gap (NOT a pack failure) ----
    //
    // `--quickPlaySingleplayer <name>` only JOINS an existing save; if
    // the named save doesn't exist (or its level.dat is unreadable) the
    // client opens to a "Failed to Quick Play / Could not find world
    // with the provided identifier" error dialog and never enters the
    // world. This is NOT a mod crash — it's the probe being unable to
    // exercise the world-join code path at all. Treat it as a distinct
    // category so the probe driver can fail-fast with a clear reason
    // rather than wasting the full 300s timeout watching the dialog sit.
    if line.contains("Failed to Quick Play")
        || line.contains("Could not find world with the provided identifier")
    {
        return WorldJoinSignal::Failure {
            mod_name: None,
            reason: "Probe wiring gap: `--quickPlaySingleplayer <name>` cannot create a world — \
                     the named save must already exist. Bundle a minimal level.dat fixture and \
                     extract it to `<instance>/saves/anvil_world_probe/` BEFORE calling \
                     world_join_probe (see verify_gate's Stage 1b note for the three options)."
                .into(),
        };
    }
    // ---- success: the genuine post-world-loaded milestone ----
    //
    // 1.20.1 vanilla writes "Preparing spawn area: " repeatedly during
    // world chunk-prep; "Loaded the worlds" is what the integrated
    // server logs once the player has fully entered. Both are safe to
    // treat as "the client survived world join and is in-world".
    if line.contains("Loaded the worlds")
        || line.contains("Time elapsed:")  // chunk-prep timing line, post-spawn
        || (line.contains("Preparing spawn area") && line.contains("100%"))
    {
        return WorldJoinSignal::Success;
    }
    WorldJoinSignal::None
}

/// True for the Fabric mod-RESOLUTION reject class (the loader refuses to
/// start because deps can't be satisfied) as opposed to a runtime / world-gen
/// crash. The resolution reject is followed by a structured remediation block
/// we want to parse, so the dump driver defers its return for this class only.
pub fn is_resolution_reject(reason: &str) -> bool {
    reason.contains("incompatible / missing mods")
        || (reason.contains("requires") && reason.contains("which is missing"))
}

/// One independent root-cause fix distilled from the Fabric remediation block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricAction {
    pub dep_id: String,
    pub dep_name: String,
    /// Version floor as a constraint string, e.g. `>=0.92.2+1.20.1`.
    pub want: String,
    /// The dep is absent entirely (add it) vs present-but-wrong (repin it).
    pub add_missing: bool,
    /// Consumer mod ids blocked by this one dependency.
    pub holders: Vec<String>,
}

/// Root-caused remediation parsed from the Fabric loader's own
/// `Incompatible mods found! / A potential solution / More details` block.
///
/// Fabric prints a PER-CONSUMER `Replace mod X` suggestion list that, followed
/// literally, makes the curator thrash (downgrade three consumers one by one)
/// and can land a needlessly degraded pack. The truth is in the
/// `… requires version >=V of mod 'Dep' …` detail lines: aggregating those by
/// dependency collapses N consumer-replacements into the ONE root-cause repin
/// (e.g. "bump fabric-api", which dissolves antique_atlas + surveyor + yacl).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricRemediation {
    /// Dependency behind the most blocked consumers — for
    /// `DumpOutcome::Crashed.mod_name`.
    pub primary_dep_id: Option<String>,
    pub actions: Vec<FabricAction>,
    /// The full curator-facing message.
    pub summary: String,
}

/// Pick the higher of two version-floor strings using the real Fabric engine;
/// fall back to "keep `a`" if either is unparseable (never lose a constraint).
fn higher_floor(a: &str, b: &str) -> String {
    match (
        crate::version::Version::parse(a),
        crate::version::Version::parse(b),
    ) {
        (Some(va), Some(vb)) => {
            if vb > va {
                b.to_string()
            } else {
                a.to_string()
            }
        }
        _ => a.to_string(),
    }
}

/// Parse one `Mod 'Holder' (hid) hv requires version V or later of mod
/// 'Dep' (depid), <tail>` detail line into
/// `(holder_id, dep_id, dep_name, want, add_missing)`.
fn parse_requires_line(
    line: &str,
) -> Option<(String, String, String, String, bool)> {
    let r = line.find(" requires version ")?;
    let head = &line[..r];
    if !head.contains("Mod '") {
        return None;
    }
    // holder id = last "(...)" group before "requires"
    let ho = head.rfind('(')?;
    let hc = head[ho..].find(')')? + ho;
    let holder_id = head[ho + 1..hc].trim().to_string();
    if holder_id.is_empty() {
        return None;
    }
    let after = &line[r + " requires version ".len()..];
    let want_raw = between(after, "", " or later")
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let depseg = after.split(" of mod '").nth(1)?;
    let (dep_name, rest) = depseg.split_once('\'')?;
    let do_ = rest.find('(')?;
    let dc = rest[do_..].find(')')? + do_;
    let dep_id = rest[do_ + 1..dc].trim().to_string();
    if dep_id.is_empty() {
        return None;
    }
    let add_missing = line.contains("which is missing");
    Some((
        holder_id,
        dep_id,
        dep_name.trim().to_string(),
        format!(">={}", want_raw),
        add_missing,
    ))
}

pub fn parse_fabric_remediation(log: &str) -> Option<FabricRemediation> {
    use std::collections::BTreeMap;
    // dep_id -> (dep_name, want_floor, add_missing, holders)
    let mut by_dep: BTreeMap<String, (String, String, bool, Vec<String>)> =
        BTreeMap::new();
    for line in log.lines() {
        if !line.contains(" requires version ") || !line.contains(" of mod '")
        {
            continue;
        }
        let Some((holder, dep_id, dep_name, want, missing)) =
            parse_requires_line(line)
        else {
            continue;
        };
        let e = by_dep.entry(dep_id).or_insert_with(|| {
            (dep_name.clone(), want.clone(), missing, Vec::new())
        });
        e.1 = {
            // higher_floor works on bare versions; strip the ">=" we added.
            let cur = e.1.trim_start_matches(">=");
            let new = want.trim_start_matches(">=");
            format!(">={}", higher_floor(cur, new))
        };
        e.2 = e.2 || missing;
        if !e.3.contains(&holder) {
            e.3.push(holder);
        }
    }
    if by_dep.is_empty() {
        return None;
    }
    let mut actions: Vec<FabricAction> = by_dep
        .into_iter()
        .map(|(dep_id, (dep_name, want, add_missing, mut holders))| {
            holders.sort();
            FabricAction {
                dep_id,
                dep_name,
                want,
                add_missing,
                holders,
            }
        })
        .collect();
    // Most-blocked dependency first — that's the root-cause repin.
    actions.sort_by(|a, b| {
        b.holders
            .len()
            .cmp(&a.holders.len())
            .then(a.dep_id.cmp(&b.dep_id))
    });
    let primary_dep_id = actions.first().map(|a| a.dep_id.clone());

    let mut summary = String::from(
        "Mod resolution failed — this pack does NOT boot. Fabric's own \
         per-mod \"Replace\" hints are misleading (they tell you to downgrade \
         the dependent mods); the real fix is the dependency repin(s) below. \
         Apply ALL of these in place with edit_pack on this instance, do NOT \
         downgrade the dependent mods, and treat removing a mod as a LAST \
         resort only if a repin/add cannot resolve it:\n",
    );
    for (i, a) in actions.iter().enumerate() {
        let verb = if a.add_missing {
            format!("add dependency '{}'", a.dep_id)
        } else {
            format!("repin '{}'", a.dep_id)
        };
        summary.push_str(&format!(
            "  {}. {} to {}  ({} — required by: {})\n",
            i + 1,
            verb,
            a.want,
            a.dep_name,
            a.holders.join(", "),
        ));
    }
    Some(FabricRemediation {
        primary_dep_id,
        actions,
        summary,
    })
}

/// The verdict surfaced to the UI/curator.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum SmokeVerdict {
    /// Reached the mods-initialized milestone — the early failure classes are
    /// ruled out (a later world-gen crash is out of scope for a bounded boot).
    Ok,
    /// A concrete failure was detected.
    Failed {
        mod_name: Option<String>,
        reason: String,
    },
    /// No verdict within the timeout / could not run — not a pass, not a
    /// confirmed failure.
    Inconclusive { reason: String },
}

/// The verdict of the headless dedicated-server registry-dump boot. Unlike
/// the title-screen client `SmokeVerdict`, this boot reaches spawn-region
/// generation (a Fabric server only prints the ready line after
/// `Preparing spawn area`), so it exercises WORLD CREATION — the failure
/// class that `smoke_test` is structurally blind to.
#[derive(Debug, Clone)]
pub enum DumpOutcome {
    /// Reached the ready line and `/dump registry` produced `dump/`. The pack
    /// generated a world cleanly AND the real runtime registry is captured.
    Dumped(std::path::PathBuf),
    /// A crash was classified during the boot (e.g. a `NoClassDefFoundError`
    /// on entity-load during spawn prep — the world-creation crash class).
    Crashed {
        mod_name: Option<String>,
        reason: String,
    },
    /// Could not even start the pass: loader not Fabric/Quilt, offline /
    /// helper-jar or server-jar or JRE unavailable, JVM contention with
    /// `wait_for_jvm == false`, or spawn failed. We could NOT try — proceeding
    /// on the static scan WITH a surfaced caveat is acceptable.
    EnvUnavailable(String),
    /// Booted but produced no proof: timed out, or the stream ended with no
    /// ready line and no classified crash. We tried and got nothing — NOT
    /// safe to assume the pack works.
    Failed(String),
}

/// Boot the pack once and watch for an early failure vs the mods-initialized
/// milestone. Reuses the exact `launch` preparation path. Kills the process
/// as soon as there is a verdict (or on timeout).
/// Open a fresh `<instance>/.verify-logs/boot-NNN.log` for the audit tee that
/// captures a verify boot's full stdout (the complete, untruncated crash
/// source). Prunes to the most recent ~12 boots. Best-effort: returns `None`
/// (capture skipped, never fatal) if the dir/file can't be created.
fn open_boot_log(inst_dir: &Path) -> Option<std::fs::File> {
    let dir = inst_dir.join(".verify-logs");
    std::fs::create_dir_all(&dir).ok()?;
    let mut existing: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?.to_string();
            let n = name.strip_prefix("boot-")?.strip_suffix(".log")?;
            n.parse::<u64>().ok().map(|num| (num, p))
        })
        .collect();
    existing.sort_by_key(|(n, _)| *n);
    let next = existing.last().map(|(n, _)| n + 1).unwrap_or(1);
    while existing.len() > 11 {
        let (_, p) = existing.remove(0);
        let _ = std::fs::remove_file(p);
    }
    std::fs::File::create(dir.join(format!("boot-{next:03}.log"))).ok()
}

pub async fn smoke_test(
    instance: &Instance,
    account: &MinecraftAccount,
    java_path: Option<String>,
    tx: UnboundedSender<LaunchEvent>,
) -> Result<SmokeVerdict> {
    loader_unsupported_bail(&instance.loader)?;
    if let Some(p) = &java_path {
        check_java(p).await?;
    }
    let prepared = prepare_inner(instance, &tx).await?;
    let java = match java_path {
        Some(p) => p,
        None => provision_jre(prepared.java_major, &tx)
            .await
            .context("auto-provisioning a JRE")?,
    };
    let _ = tx.send(LaunchEvent::Status("Smoke test: booting pack".into()));
    let mut args = build_command_args(instance, account, &prepared)?;
    // SMOKE-ONLY: force AWT headless. On a mod-RESOLUTION failure ("Incompatible
    // mods found") Fabric otherwise opens a BLOCKING Swing dialog and waits for
    // the user to click Exit — a verify boot must never wait on a modal window
    // (it writes no crash report, so the drain loop would hang on it until the
    // 150s timeout). Headless makes Fabric print the remediation to the console
    // and exit on its own, so we capture the full block and the JVM closes. It
    // also avoids the macOS AWT/GLFW main-thread conflict; GLFW/OpenGL/OpenAL are
    // unaffected, so a normal boot still reaches the menu. JVM arg → before the
    // main class.
    if let Some(pos) = args.iter().position(|a| a == &prepared.main_class) {
        args.insert(pos, "-Djava.awt.headless=true".into());
    }
    let inst_dir = instance_dir(&instance.id);
    tokio::fs::create_dir_all(&inst_dir).await.ok();

    // Two-JVM guard: never co-boot with a registry-dump server (each is a
    // full modded JVM; two at once OOMs an 8–16 GB machine). The smoke test
    // is the user-facing path, so it WAITS for the lock (a queued boot is
    // fine); `registry_dump_pass` is best-effort and skips instead. Held for
    // the whole spawn→wait window via `_jvm_guard`.
    let _jvm_guard = jvm_lock().lock().await;

    let mut child = tokio::process::Command::new(&java)
        .args(&args)
        .current_dir(&inst_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn java ({java})"))?;

    // Merge stdout+stderr into one line channel (draining both avoids the
    // pipe-buffer deadlock the real launcher also guards against).
    let (ltx, mut lrx) = tokio::sync::mpsc::unbounded_channel::<String>();
    if let Some(out) = child.stdout.take() {
        let ltx = ltx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if ltx.send(l).is_err() {
                    break;
                }
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let ltx = ltx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if ltx.send(l).is_err() {
                    break;
                }
            }
        });
    }
    drop(ltx);

    // Audit + Fix 1: tee the FULL boot stdout to a per-boot log AND do not bail
    // on the first failure line. Minecraft prints the crash-report HEADER (which
    // names no mod) BEFORE the `Could not execute entrypoint … provided by
    // '<mod>'` culprit line; the old "return on first Failure + kill" lost that
    // name and truncated both `latest.log` and the crash `.txt` mid-write. Now
    // we keep draining until the report finishes (`Crash report saved to`) or
    // the stream ends, upgrading to the NAMED culprit when it appears.
    let mut boot_log = open_boot_log(&inst_dir);
    let verdict = {
        use std::io::Write;
        let scan = async {
            let mut crash: Option<(Option<String>, String)> = None;
            while let Some(line) = lrx.recv().await {
                if let Some(f) = boot_log.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
                let sig = classify_smoke_line(&line);
                let saved = line.contains("Crash report saved to");
                let _ = tx.send(LaunchEvent::Log(line));
                match sig {
                    SmokeSignal::Failure { mod_name, reason } => {
                        // Prefer a NAMED culprit; otherwise keep the first
                        // failure (e.g. the headerless crash-report line).
                        let upgrade = match &crash {
                            Some((Some(_), _)) => false, // already have a name
                            _ => mod_name.is_some() || crash.is_none(),
                        };
                        if upgrade {
                            crash = Some((mod_name, reason));
                        }
                    }
                    // Only a crash-free boot may pass.
                    SmokeSignal::Success if crash.is_none() => {
                        return SmokeVerdict::Ok
                    }
                    _ => {}
                }
                // The crash report has fully flushed — safe to conclude with the
                // best culprit we captured (the .txt is now complete too).
                if saved {
                    if let Some((mod_name, reason)) = crash {
                        return SmokeVerdict::Failed { mod_name, reason };
                    }
                }
            }
            // Stream ended (JVM exited). A captured crash with no trailing
            // "saved to" line (e.g. a resolution reject) still fails here.
            match crash {
                Some((mod_name, reason)) => {
                    SmokeVerdict::Failed { mod_name, reason }
                }
                None => SmokeVerdict::Inconclusive {
                    reason: "the game exited before mods finished initializing"
                        .into(),
                },
            }
        };
        // Generous: JVM + 200-mod init on first run. Preparation/downloads
        // already happened above, so this only covers boot.
        match tokio::time::timeout(std::time::Duration::from_secs(150), scan)
            .await
        {
            Ok(v) => v,
            Err(_) => SmokeVerdict::Inconclusive {
                reason: "no pass/fail signal within 150s (slow machine?)"
                    .into(),
            },
        }
    };

    // Always reap the child — a smoke test must never leave a real game open.
    let _ = child.start_kill();
    let _ = child.wait().await;
    Ok(verdict)
}

/// The verdict from `world_join_probe`. Mirrors `SmokeVerdict` so the
/// verify-gate caller can treat both the same way (proceed on Ok, surface
/// repair payload on Failed, caveat on Inconclusive).
#[derive(Debug, Clone, PartialEq)]
pub enum WorldJoinVerdict {
    Ok,
    Failed { mod_name: Option<String>, reason: String },
    Inconclusive { reason: String },
}

/// Boot the client with `--quickPlaySingleplayer` and watch for
/// world-join failure vs the in-world success milestone. Closes the
/// IPN-class crash gap that `smoke_test` cannot detect (smoke_test
/// returns Ok at the main menu, BEFORE world creation runs).
pub async fn world_join_probe(
    instance: &Instance,
    account: &MinecraftAccount,
    java_path: Option<String>,
    tx: UnboundedSender<LaunchEvent>,
) -> Result<WorldJoinVerdict> {
    loader_unsupported_bail(&instance.loader)?;
    if let Some(p) = &java_path {
        check_java(p).await?;
    }
    let prepared = prepare_inner(instance, &tx).await?;
    let java = match java_path {
        Some(p) => p,
        None => provision_jre(prepared.java_major, &tx)
            .await
            .context("auto-provisioning a JRE for world-join probe")?,
    };
    let _ = tx.send(LaunchEvent::Status(
        "World-join probe: booting + auto-creating throwaway world".into(),
    ));
    let mut args = build_command_args(instance, account, &prepared)?;
    // vanilla 1.20.1 quick-play flags: --quickPlaySingleplayer <save name>
    // auto-creates the world if missing and joins it immediately. We name
    // it predictably so the probe can clean up later if it ever wants to.
    args.push("--quickPlaySingleplayer".to_string());
    args.push("anvil_world_probe".to_string());
    let inst_dir = instance_dir(&instance.id);
    tokio::fs::create_dir_all(&inst_dir).await.ok();

    // Pre-snapshot crash-reports/ so a NEW file at exit = a real crash
    // missed by stdout scanning (some crashes write the file but the
    // logger flushes after our process kill).
    let crash_dir = inst_dir.join("crash-reports");
    let prior_crashes: std::collections::BTreeSet<std::ffi::OsString> = if crash_dir.is_dir() {
        std::fs::read_dir(&crash_dir)
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.file_name())).collect())
            .unwrap_or_default()
    } else { std::collections::BTreeSet::new() };

    let _jvm_guard = jvm_lock().lock().await;
    let mut child = tokio::process::Command::new(&java)
        .args(&args)
        .current_dir(&inst_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn java for world-join probe ({java})"))?;

    let (ltx, mut lrx) = tokio::sync::mpsc::unbounded_channel::<String>();
    if let Some(out) = child.stdout.take() {
        let ltx = ltx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if ltx.send(l).is_err() { break; }
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let ltx = ltx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if ltx.send(l).is_err() { break; }
            }
        });
    }
    drop(ltx);

    let verdict = {
        let scan = async {
            while let Some(line) = lrx.recv().await {
                let sig = classify_world_join_line(&line);
                let _ = tx.send(LaunchEvent::Log(line));
                match sig {
                    WorldJoinSignal::Failure { mod_name, reason } => {
                        return WorldJoinVerdict::Failed { mod_name, reason };
                    }
                    WorldJoinSignal::Success => return WorldJoinVerdict::Ok,
                    WorldJoinSignal::None => {}
                }
            }
            WorldJoinVerdict::Inconclusive {
                reason: "process exited before world-join verdict line".into(),
            }
        };
        // Allow time for: JVM warm-up, mods init, world generation, spawn-prep.
        // Generously sized for a 50+ mod pack on a typical machine.
        match tokio::time::timeout(std::time::Duration::from_secs(300), scan).await {
            Ok(v) => v,
            Err(_) => WorldJoinVerdict::Inconclusive {
                reason: "no world-loaded signal within 300s".into(),
            },
        }
    };

    let _ = child.start_kill();
    let _ = child.wait().await;

    // If stdout scanning didn't catch a crash but a new crash-reports/
    // file appeared, that IS the crash — upgrade Inconclusive → Failed.
    if matches!(verdict, WorldJoinVerdict::Inconclusive { .. } | WorldJoinVerdict::Ok) {
        if let Ok(entries) = std::fs::read_dir(&crash_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if !prior_crashes.contains(&name) {
                    let path = entry.path();
                    let snippet = std::fs::read_to_string(&path).ok()
                        .map(|s| s.lines().take(20).collect::<Vec<_>>().join("\n"))
                        .unwrap_or_else(|| format!("crash report at {}", path.display()));
                    return Ok(WorldJoinVerdict::Failed {
                        mod_name: None,
                        reason: format!(
                            "Crashed during world-join (crash-reports/{}). First lines:\n{snippet}",
                            name.to_string_lossy(),
                        ),
                    });
                }
            }
        }
    }
    Ok(verdict)
}

// ---------------------------------------------------------------------------
// Slice 1.5 — first-launch registry-dump pass
//
// Boots a HEADLESS Fabric dedicated server (NOT the client) in a throwaway
// dir, running the `registry-dump` helper mod, pipes `/dump registry` to its
// stdin, then `stop`, and hands back the dir whose `dump/**` JSON arrays
// `registry::reconcile_with_launch_dump` parses. EVERY failure path returns
// `Ok(None)` — the static scan stays authoritative; this NEVER blocks assemble
// or launch (it runs detached from `tool_assemble_pack`).
// ---------------------------------------------------------------------------

/// Process-wide single-JVM guard. A modded MC JVM is multi-GB; the smoke test
/// and the registry-dump server must never co-boot. `smoke_test` `.lock()`s
/// (waits); `registry_dump_pass` `try_lock()`s and skips on contention (the
/// next assemble re-triggers — losing one best-effort dump is fine, an OOM is
/// not). A `OnceLock<Mutex<()>>` keeps this in `launch.rs` so the scope stays
/// launch/registry/curator/quest (no `lib.rs` managed-state field needed).
fn jvm_lock() -> &'static tokio::sync::Mutex<()> {
    static JVM: std::sync::OnceLock<tokio::sync::Mutex<()>> =
        std::sync::OnceLock::new();
    JVM.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// The helper mod that prints the live registry. Confirmed: Modrinth slug
/// `registry-dump`, project `UAzbcRGX`, jar `registry_dump-fabric-2.1.0.jar`,
/// version `YApQLBuM`. It HARD-depends on Fabric API (project `P7dR8mSH`,
/// version `0.92.9+1.20.1`) so BOTH go into the throwaway `mods/`.
const REGISTRY_DUMP_PROJECT: &str = "UAzbcRGX";
const REGISTRY_DUMP_VERSION: &str = "YApQLBuM";
const FABRIC_API_PROJECT: &str = "P7dR8mSH";
const FABRIC_API_VERSION: &str = "0.92.9+1.20.1";
/// Fabric server-launcher pin for 1.20.1 (loader 0.19.2 / installer 1.1.1).
const FABRIC_SERVER_LOADER: &str = "0.19.2";
const FABRIC_SERVER_INSTALLER: &str = "1.1.1";

/// The headless dedicated server's `server.properties`. A flat world with
/// zero sim/render/players boots in seconds and never generates terrain — we
/// only need the registry, not a world. Hand-formatted (no toml/props crate).
/// Pure + test-locked so `level-type=flat` can never silently regress.
fn server_properties_bytes() -> Vec<u8> {
    let s = "\
level-type=flat
online-mode=false
max-players=0
view-distance=2
simulation-distance=2
";
    s.as_bytes().to_vec()
}

/// `eula.txt` accepting the Mojang EULA (required before the first server
/// boot). Test-locked literal.
fn eula_bytes() -> Vec<u8> {
    b"eula=true\n".to_vec()
}

/// True iff a server stdout line means "ready for stdin commands": it must
/// contain BOTH the `Done (` marker AND the help hint (the trailing seconds
/// vary, so we never anchor them).
fn is_server_ready_line(line: &str) -> bool {
    line.contains("Done (") && line.contains(r#"! For help, type "help""#)
}

/// Resolve a single Modrinth project version's primary jar via the existing
/// `Modrinth` client + the launcher's verified-cache `ensure_mod` pattern.
/// Reuses `PinnedMod` so the SHA-512 verify + delete-on-mismatch is identical
/// to every other jar the launcher fetches. `version_id` pins the exact file.
async fn resolve_helper_jar(
    mr: &crate::modrinth::Modrinth,
    client: &reqwest::Client,
    project_id: &str,
    version_id: &str,
    dest: &Path,
) -> Result<()> {
    let versions = mr
        .versions(project_id)
        .await
        .map_err(|e| anyhow!("Modrinth versions({project_id}) failed: {e}"))?;
    let v = versions
        .iter()
        .find(|v| v.id == version_id)
        .or_else(|| versions.first())
        .ok_or_else(|| anyhow!("project {project_id} has no published versions"))?;
    let f = v
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| v.files.first())
        .ok_or_else(|| anyhow!("version {} has no downloadable file", v.id))?;
    // Reuse the launcher's verified fetch (cache-hit, SHA-512, delete-on-bad).
    let pinned = crate::instance::PinnedMod {
        project_id: project_id.to_string(),
        version_id: v.id.clone(),
        name: f.filename.clone(),
        path: format!("mods/{}", f.filename),
        sha1: f.hashes.sha1.clone(),
        sha512: f.hashes.sha512.clone(),
        download_url: f.url.clone(),
        file_size: f.size,
        client_side: "required".to_string(),
        server_side: "required".to_string(),
    };
    ensure_mod(client, &pinned, dest).await
}

/// Resolve the Mojang dedicated-server jar URL for `mc_version` from
/// piston-meta (mirrors `prepare_inner`'s vanilla-jar resolution, but the
/// `downloads.server` entry instead of `downloads.client`).
async fn resolve_server_jar_url(
    client: &reqwest::Client,
    mc_version: &str,
) -> Result<String> {
    let vj = fetch_version_json(client, &shared_mc_dir(), mc_version).await?;
    vj.get("downloads")
        .and_then(|d| d.get("server"))
        .and_then(|s| s.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("version JSON for {mc_version} has no downloads.server.url"))
}

/// Inner, INJECTABLE driver: given a fully-prepared `<dump>` dir and the path
/// to a `java` binary, spawn `java -jar <server_launcher> nogui` with
/// `current_dir(<dump>)`, drain BOTH pipes (mirrors `smoke_test`'s
/// stdout+stderr+reap pattern — draining one only would deadlock), and on the
/// ready line write `/dump registry` then (after a short grace) `stop` to
/// stdin. The java/launcher path is a parameter so tests point it at a stub
/// script and never spawn a real JVM or hit the network.
///
/// Returns `Ok(Some(<dump>))` on a clean stop (dir then contains `dump/`),
/// `Ok(None)` on timeout / a `classify_smoke_line` failure class / the JVM
/// guard being contended — i.e. every degrade path. Holds `jvm_lock` for the
/// whole spawn→wait window (acquired via `try_lock`: skip, never queue).
async fn drive_dump_server(
    dump_dir: &Path,
    java: &str,
    server_launcher: &Path,
    grace: std::time::Duration,
    timeout: std::time::Duration,
    wait_for_jvm: bool,
    // macOS: a jna-5.14.0 jar to put on `-Xbootclasspath/a:` so it shadows
    // the Mojang bundler's self-extracted JNA 5.12.1 (which hard-aborts the
    // JVM here). `None` off macOS / when the one-time fetch failed.
    jna_boot: Option<std::path::PathBuf>,
    tx: &UnboundedSender<LaunchEvent>,
) -> Result<DumpOutcome> {
    // Single-JVM guard (the OOM defense — two modded JVMs co-booting OOMs an
    // 8-16 GB machine). The detached background pass `try_lock`s and treats
    // contention as `EnvUnavailable` (the next assemble re-triggers). A GATED
    // caller (verify_pack / the pre-quest gate) passes `wait_for_jvm` so a
    // queued boot waits instead of silently skipping the only verification.
    let _jvm_guard = if wait_for_jvm {
        jvm_lock().lock().await
    } else {
        match jvm_lock().try_lock() {
            Ok(g) => g,
            Err(_) => {
                let _ = tx.send(LaunchEvent::Status(
                    "Registry dump skipped (another JVM is running)".into(),
                ));
                return Ok(DumpOutcome::EnvUnavailable(
                    "another JVM is already running".into(),
                ));
            }
        }
    };

    let _ = tx.send(LaunchEvent::Status("Booting headless registry server".into()));
    let mut cmd = tokio::process::Command::new(java);
    // macOS: the dump server is `java -jar fabric-server-launcher.jar`; the
    // Mojang bundler self-extracts its OWN pinned JNA 5.12.1 (sha256
    // re-verified, so the client path's classpath swap / a file replace can
    // never reach it) and that old JNA HARD-ABORTS the JVM on modern macOS
    // (see `ensure_modern_jna_macos`). Boot-classpath classes are resolved by
    // the bootstrap loader ahead of the bundler's URLClassLoader, so putting
    // jna-5.14.0 here makes `com.sun.jna.*` load from 5.14.0 and 5.12.1 is
    // never the one used — without fighting the bundler.
    if let Some(j) = jna_boot.as_deref() {
        cmd.arg(format!("-Xbootclasspath/a:{}", j.display()));
        let _ = tx.send(LaunchEvent::Log(format!(
            "[anvil] dump server: JNA 5.14.0 on boot classpath \
             (older JNA aborts the JVM on this macOS)"
        )));
    }
    let mut child = match cmd
        .arg("-jar")
        .arg(server_launcher)
        .arg("nogui")
        .current_dir(dump_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            // Could not even spawn → could-not-try, never propagate.
            let _ = tx.send(LaunchEvent::Status(format!(
                "Registry dump skipped (spawn failed: {e})"
            )));
            return Ok(DumpOutcome::EnvUnavailable(format!(
                "could not spawn the server JVM: {e}"
            )));
        }
    };

    let mut stdin = child.stdin.take();

    // Merge stdout+stderr into one line channel — the SAME pattern smoke_test
    // uses; draining only one fills the other pipe buffer and deadlocks.
    let (ltx, mut lrx) = tokio::sync::mpsc::unbounded_channel::<String>();
    if let Some(out) = child.stdout.take() {
        let ltx = ltx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if ltx.send(l).is_err() {
                    break;
                }
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let ltx = ltx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if ltx.send(l).is_err() {
                    break;
                }
            }
        });
    }
    drop(ltx);

    let dump_dir_buf = dump_dir.to_path_buf();
    let outcome = {
        let drive = async {
            let mut requested_stop = false;
            // Buffer the boot log so the Fabric mod-resolution remediation
            // block (`A potential solution / More details`, which prints AFTER
            // the first failure line, then the JVM exits) can be parsed whole.
            // Bounded: a resolution reject is tiny; a big successful/world-gen
            // log just stops accumulating (we never parse those).
            let mut log_buf = String::new();
            let mut resolution_fail = false;
            while let Some(line) = lrx.recv().await {
                if log_buf.len() < 512_000 {
                    log_buf.push_str(&line);
                    log_buf.push('\n');
                }
                // Once we know it's a resolution reject, just keep draining
                // (the JVM dies in a beat) so we can parse the full block on
                // stream end — do not re-classify per-line.
                if resolution_fail {
                    let _ = tx.send(LaunchEvent::Log(line));
                    continue;
                }
                // A failure class we already classify (incompatible mods,
                // entrypoint crash, crash report, a fatal NoClassDefFound on
                // entity-load during spawn prep — the world-creation crash
                // class). This loop keeps scanning every line, so a crash
                // AFTER the ready line / during the post-`stop` grace is also
                // caught here, not missed.
                if let SmokeSignal::Failure { mod_name, reason } =
                    classify_smoke_line(&line)
                {
                    // The mod-resolution reject prints its structured fix
                    // block next — defer; every other fatal class (world-gen
                    // crash, crash report) returns now (analyze_crash owns it).
                    if is_resolution_reject(&reason) {
                        resolution_fail = true;
                        let _ = tx.send(LaunchEvent::Status(
                            "Pack failed mod resolution — reading Fabric's \
                             remediation"
                                .into(),
                        ));
                        let _ = tx.send(LaunchEvent::Log(line));
                        continue;
                    }
                    let _ = tx.send(LaunchEvent::Status(format!(
                        "Registry dump aborted: {reason}"
                    )));
                    let _ = tx.send(LaunchEvent::Log(line));
                    return DumpOutcome::Crashed { mod_name, reason };
                }
                let ready = is_server_ready_line(&line);
                let _ = tx.send(LaunchEvent::Log(line));
                if ready && !requested_stop {
                    requested_stop = true;
                    if let Some(si) = stdin.as_mut() {
                        let _ = si.write_all(b"/dump registry\n").await;
                        let _ = si.flush().await;
                        // `/dump` is synchronous server-side file IO but tokio
                        // gets no completion signal — a short fixed grace lets
                        // it finish writing every `dump/**/*.json` before we
                        // ask the server to stop.
                        tokio::time::sleep(grace).await;
                        let _ = si.write_all(b"stop\n").await;
                        let _ = si.flush().await;
                    }
                    // Drop stdin so the server sees EOF too (belt and braces).
                    stdin = None;
                }
            }
            // Stream ended. A deferred mod-resolution reject: parse the whole
            // remediation block into the root-cause repin set the curator can
            // act on directly (instead of the generic "incompatible mods").
            if resolution_fail {
                return match parse_fabric_remediation(&log_buf) {
                    Some(rem) => DumpOutcome::Crashed {
                        mod_name: rem.primary_dep_id.clone(),
                        reason: rem.summary,
                    },
                    None => DumpOutcome::Crashed {
                        mod_name: None,
                        reason: "Pack failed mod resolution (Fabric rejected \
                                 incompatible / missing mods); the remediation \
                                 block could not be parsed — inspect the boot \
                                 log and repair with edit_pack."
                            .into(),
                    },
                };
            }
            // A clean `stop` after a dump leaves `dump/` behind;
            // its presence is the success signal (a crash never writes it).
            if requested_stop && dump_dir_buf.join("dump").is_dir() {
                DumpOutcome::Dumped(dump_dir_buf.clone())
            } else {
                // Booted but no ready line / no usable dump and no classified
                // crash: we tried and got no proof. NOT "unavailable".
                DumpOutcome::Failed(
                    "the server exited before producing a usable registry \
                     dump (no ready line, no recognized crash)"
                        .into(),
                )
            }
        };
        match tokio::time::timeout(timeout, drive).await {
            Ok(v) => v,
            Err(_) => {
                let _ = tx.send(LaunchEvent::Status(
                    "Registry dump timed out".into(),
                ));
                DumpOutcome::Failed(
                    "the verification boot did not finish within the time \
                     budget"
                        .into(),
                )
            }
        }
    };

    // Always reap — a dump pass must never leave a server running.
    let _ = child.start_kill();
    let _ = child.wait().await;

    match &outcome {
        DumpOutcome::Dumped(_) => {
            let _ = tx.send(LaunchEvent::Status("Registry dump captured".into()));
        }
        DumpOutcome::Crashed { .. } => {
            let _ = tx.send(LaunchEvent::Status(
                "Pack crashed during the verification boot".into(),
            ));
        }
        DumpOutcome::EnvUnavailable(_) | DumpOutcome::Failed(_) => {
            let _ = tx.send(LaunchEvent::Status(
                "Registry dump unavailable — using static registry".into(),
            ));
        }
    }
    Ok(outcome)
}

/// Slice 1.5 entry point. Builds the throwaway server dir, fetches the helper
/// jars + server launcher, provisions the JRE, then drives the dump. The
/// throwaway dir is `<instance>/.anvil-dump` (removed + recreated each run).
/// EVERY error degrades to `Ok(None)` — this is best-effort grounding, never a
/// gate on assemble or launch.
pub async fn registry_dump_pass(
    instance: &Instance,
    mr: &crate::modrinth::Modrinth,
    wait_for_jvm: bool,
    tx: UnboundedSender<LaunchEvent>,
) -> Result<DumpOutcome> {
    // Only Fabric/Quilt expose a server launcher we can drive this way.
    if !matches!(instance.loader.as_str(), "fabric" | "quilt") {
        return Ok(DumpOutcome::EnvUnavailable(format!(
            "loader '{}' has no headless server boot (Anvil v1 is Fabric)",
            instance.loader
        )));
    }

    // One up-front chip so the user knows a background network/CPU job started
    // (this pass downloads the pinned pack + helper jars and boots a JVM).
    // Reuses the existing `Status` variant — no new UI. NOTE: the detached
    // curator caller drops this channel's receiver, so it is only user-visible
    // for direct/foreground callers; that silence is a property of the
    // pre-existing detached design, not introduced here.
    let _ = tx.send(LaunchEvent::Status(
        "Grounding registry (background)…".into(),
    ));

    let dump = instance_dir(&instance.id).join(".anvil-dump");
    // Fresh every run: a stale `dump/` from a previous pin set would be parsed
    // as truth. Failure to clean is non-fatal (we recreate over it).
    let _ = tokio::fs::remove_dir_all(&dump).await;
    let mods_dir = dump.join("mods");
    if let Err(e) = tokio::fs::create_dir_all(&mods_dir).await {
        let _ = tx.send(LaunchEvent::Status(format!(
            "Registry dump skipped (mkdir failed: {e})"
        )));
        return Ok(DumpOutcome::EnvUnavailable(format!(
            "could not create the dump working dir: {e}"
        )));
    }

    let client = match http_client() {
        Ok(c) => c,
        Err(e) => {
            return Ok(DumpOutcome::EnvUnavailable(format!(
                "no HTTP client for helper-jar fetch: {e}"
            )))
        }
    };

    // Populate the throwaway mods/ with the FULL pinned modpack.
    //
    // Why iterate `instance.mods` and NOT hard-link `<instance>/mods/*.jar`:
    // this pass runs at ASSEMBLE time (detached, right after the pack is
    // written), but jars only materialize into `<instance>/mods/` at LAUNCH
    // (`prepare_inner`/`ensure_mod`). At assemble time that dir is empty, so
    // the old link-from-instance-mods step linked nothing and the dump server
    // booted with only the 2 helper jars — capturing VANILLA registries only,
    // defeating Slice 1.5 (whose whole point is grounding MODDED ids). The
    // pinned `PinnedMod` set, by contrast, IS already known here (it is what
    // `tool_assemble_pack` just persisted).
    //
    // Why hard-link from the shared sha-keyed cache (not `ensure_mod` direct):
    // `ensure_jar_cached` puts each jar in `~/.anvil/cache/jars/<sha1>.jar`,
    // the SAME cache the launcher's own fetch path warms — so a re-assemble or
    // a later real launch reuses these bytes instead of re-downloading 50
    // jars. We hard-link the cached file into `<dump>/mods/` (instant, no
    // extra disk); `fs::copy` is the fallback on a cross-device / link error.
    //
    // Degrade, never block: a pinned mod we cannot resolve/fetch is SKIPPED
    // (the dump just misses that one mod's ids — strictly better than the old
    // vanilla-only behavior); we never error the pass on it.
    for m in &instance.mods {
        let Some(cached) =
            crate::curator::ensure_jar_cached(&client, &m.download_url, &m.sha1).await
        else {
            continue; // unresolvable/failed → skip this mod, keep going.
        };
        // `PinnedMod.path` is instance-relative like `mods/sodium.jar`.
        let fname = std::path::Path::new(&m.path)
            .file_name()
            .map(|f| f.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from(format!("{}.jar", m.version_id)));
        let link = mods_dir.join(&fname);
        if tokio::fs::hard_link(&cached, &link).await.is_err() {
            let _ = tokio::fs::copy(&cached, &link).await;
        }
    }

    // The helper mod + its hard Fabric API dependency. Either failing → degrade.
    if let Err(e) = resolve_helper_jar(
        mr,
        &client,
        REGISTRY_DUMP_PROJECT,
        REGISTRY_DUMP_VERSION,
        &mods_dir.join("registry_dump-fabric-2.1.0.jar"),
    )
    .await
    {
        let _ = tx.send(LaunchEvent::Status(format!(
            "Registry dump skipped (helper mod unavailable: {e})"
        )));
        return Ok(DumpOutcome::EnvUnavailable(format!(
            "registry-dump helper mod unavailable: {e}"
        )));
    }
    if let Err(e) = resolve_helper_jar(
        mr,
        &client,
        FABRIC_API_PROJECT,
        FABRIC_API_VERSION,
        &mods_dir.join("fabric-api.jar"),
    )
    .await
    {
        let _ = tx.send(LaunchEvent::Status(format!(
            "Registry dump skipped (Fabric API unavailable: {e})"
        )));
        return Ok(DumpOutcome::EnvUnavailable(format!(
            "Fabric API (dump helper dependency) unavailable: {e}"
        )));
    }

    // EULA + properties BEFORE first boot (a server refuses to start without
    // an accepted EULA and would regenerate properties we want pinned).
    if tokio::fs::write(dump.join("eula.txt"), eula_bytes())
        .await
        .is_err()
        || tokio::fs::write(
            dump.join("server.properties"),
            server_properties_bytes(),
        )
        .await
        .is_err()
    {
        return Ok(DumpOutcome::EnvUnavailable(
            "could not write eula.txt / server.properties".into(),
        ));
    }

    // Fabric's PRE-BUILT server launcher jar (loader + installer baked in) —
    // simplest server entry: no installer pipeline to run.
    let launcher_url = format!(
        "{FABRIC_META}/{}/{}/{}/server/jar",
        instance.mc_version, FABRIC_SERVER_LOADER, FABRIC_SERVER_INSTALLER
    );
    let server_launcher = dump.join("fabric-server-launcher.jar");
    if let Err(e) = stream_download(&client, &launcher_url, &server_launcher).await {
        let _ = tx.send(LaunchEvent::Status(format!(
            "Registry dump skipped (server launcher unavailable: {e})"
        )));
        return Ok(DumpOutcome::EnvUnavailable(format!(
            "Fabric server launcher unavailable: {e}"
        )));
    }
    // The launcher needs the Mojang server jar present too; piston-meta's
    // `downloads.server`. (The Fabric launcher fetches it itself when online,
    // but pre-resolving the URL fails loud here instead of mid-boot.)
    if let Err(e) = resolve_server_jar_url(&client, &instance.mc_version).await {
        let _ = tx.send(LaunchEvent::Status(format!(
            "Registry dump skipped (server jar unresolved: {e})"
        )));
        return Ok(DumpOutcome::EnvUnavailable(format!(
            "Mojang server jar unresolved: {e}"
        )));
    }

    // Java 17 for 1.20.1 — reuse the launcher's own provisioner/cache.
    let java = match provision_jre(17, &tx).await {
        Ok(j) => j,
        Err(e) => {
            let _ = tx.send(LaunchEvent::Status(format!(
                "Registry dump skipped (JRE unavailable: {e})"
            )));
            return Ok(DumpOutcome::EnvUnavailable(format!(
                "no JRE for the verification boot: {e}"
            )));
        }
    };

    // 300s (was 180s): the throwaway mods/ now holds the FULL pinned pack, so
    // a ~50-mod Fabric load is materially slower to reach the ready line than
    // the helper-jars-only boot this budget was first sized for. Still finite,
    // still degrades to `Ok(None)` on overshoot (best-effort, never a gate).
    // The test-injected shortened timeouts go straight to `drive_dump_server`
    // and are untouched.
    // macOS-only: the boot-classpath JNA shim (no-op / None elsewhere). One
    // cached download; never blocks the pass if it fails.
    let jna_boot = macos_jna_boot_jar(&client).await;

    drive_dump_server(
        &dump,
        &java,
        &server_launcher,
        std::time::Duration::from_secs(8),
        std::time::Duration::from_secs(300),
        wait_for_jvm,
        jna_boot,
        &tx,
    )
    .await
}

// ---------------------------------------------------------------------------
// Loader gate
// ---------------------------------------------------------------------------

/// Forge/NeoForge are deliberately still gated here. Unlike Fabric/Quilt —
/// which expose a ready-made launcher profile (libraries + mainClass) over a
/// simple `profile/json` endpoint — modern Forge and NeoForge ship an
/// *installer jar* whose `install_profile.json` declares a chain of
/// `processors`. Those processors must be executed (each is a small Java
/// program: `binarypatcher`, `jarsplitter`, `installertools`, the MCP/Mojang
/// mappings merger, access-transformer applier, …) to *generate* the patched
/// client jar and the remapped/SRG libraries that the actual classpath then
/// points at. There is no clean shortcut: you cannot assemble a working
/// classpath without running that pipeline first.
///
/// Implementing it is a self-contained but substantial milestone and was
/// intentionally deferred so it cannot regress the working vanilla/Fabric/
/// Quilt paths (their classpath ordering in particular).
///
/// TODO (Forge/NeoForge — processor pipeline):
///   1. Download the loader's *installer* jar (Forge maven /
///      NeoForge `maven.neoforged.net`).
///   2. Extract `install_profile.json` + `version.json` from inside it.
///   3. Resolve & download every library named by the install profile
///      (`libraries`, plus `processors[].classpath`).
///   4. For each entry in `processors` (respecting its `sides`), spawn the
///      provisioned JRE with `-cp <processor classpath>` and the processor's
///      main class, substituting `{...}` data placeholders
///      (`{MINECRAFT_JAR}`, `{SIDE}`, mappings paths, …) and the
///      `[maven:coord]` artifact refs.
///   5. The pipeline's outputs (patched client jar, SRG-named libs) become
///      the real classpath; merge `version.json`'s `arguments`/`mainClass`
///      the same way Fabric's profile is folded in today.
/// The provisioned JRE from `provision_jre` is exactly the runtime those
/// processors should be executed with, so step 4 is now unblocked.
fn loader_unsupported_bail(loader: &str) -> Result<()> {
    match loader {
        "vanilla" | "fabric" | "quilt" => Ok(()),
        "forge" | "neoforge" => bail!(
            "{loader} launch is not implemented yet (vanilla + Fabric supported)"
        ),
        other => bail!(
            "unknown loader '{other}' (vanilla + Fabric supported; forge/neoforge planned)"
        ),
    }
}

// ---------------------------------------------------------------------------
// Shared preparation
// ---------------------------------------------------------------------------

/// Everything `launch` needs once preparation is done.
struct PreparedLaunch {
    /// All classpath entries (libraries + client jar), in load order.
    classpath: Vec<PathBuf>,
    main_class: String,
    /// `assetIndex.id` (used for `--assetIndex` and the legacy assets dir).
    asset_index_id: String,
    /// Merged version JSON (vanilla; Fabric mainClass/libs already folded in
    /// elsewhere, but vanilla args/asset metadata live here).
    version_json: Value,
    /// Fabric/Quilt profile JSON if a loader is in use (carries `arguments`).
    loader_profile: Option<Value>,
    natives_dir: PathBuf,
    assets_dir: PathBuf,
    /// Required Java major version (from `javaVersion.majorVersion` in the
    /// version JSON, else a MC-version heuristic). Threaded out here so
    /// `launch` can auto-provision the matching JRE without re-reading.
    java_major: u32,
}

/// Steps 1–7: manifest, client jar, libraries+natives, assets, loader, mods.
async fn prepare_inner(instance: &Instance, tx: &UnboundedSender<LaunchEvent>) -> Result<PreparedLaunch> {
    let client = http_client()?;
    let shared = shared_mc_dir();
    let inst_dir = instance_dir(&instance.id);
    let natives_dir = inst_dir.join("natives");

    // --- Step 2: version manifest + version JSON --------------------------
    let _ = tx.send(LaunchEvent::Status(format!(
        "Resolving Minecraft {}",
        instance.mc_version
    )));
    let version_json = fetch_version_json(&client, &shared, &instance.mc_version).await?;
    let version_id = version_json
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(&instance.mc_version)
        .to_string();
    let java_major = required_java_major(&version_json, &instance.mc_version);

    // --- Step 3: client jar ----------------------------------------------
    let _ = tx.send(LaunchEvent::Status("Downloading client jar".into()));
    let client_jar = shared
        .join("versions")
        .join(&version_id)
        .join(format!("{version_id}.jar"));
    if let Some(dl) = version_json
        .get("downloads")
        .and_then(|d| d.get("client"))
    {
        let url = dl
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("version JSON missing downloads.client.url"))?;
        download_if_missing(&client, url, &client_jar).await?;
    } else {
        bail!("version JSON has no downloads.client (unsupported/old version?)");
    }

    // --- Step 4: libraries + natives -------------------------------------
    let _ = tx.send(LaunchEvent::Status("Downloading libraries".into()));
    let mut classpath: Vec<PathBuf> = Vec::new();

    // Fabric/Quilt libraries go FIRST on the classpath so their intermediary
    // / ASM / loader classes win over anything vanilla ships.
    let loader_profile = match instance.loader.as_str() {
        "fabric" => {
            let lv = resolve_loader_version(
                &client,
                FABRIC_META,
                &instance.mc_version,
                &instance.loader_version,
            )
            .await
            .context("resolving Fabric loader version")?;
            let _ = tx.send(LaunchEvent::Status(format!(
                "Using Fabric Loader {lv}"
            )));
            Some(
                fetch_fabric_profile(&client, FABRIC_META, &instance.mc_version, &lv)
                    .await
                    .context("fetching Fabric loader profile")?,
            )
        }
        "quilt" => {
            let lv = resolve_loader_version(
                &client,
                QUILT_META,
                &instance.mc_version,
                &instance.loader_version,
            )
            .await
            .context("resolving Quilt loader version")?;
            let _ = tx.send(LaunchEvent::Status(format!(
                "Using Quilt Loader {lv}"
            )));
            Some(
                fetch_fabric_profile(&client, QUILT_META, &instance.mc_version, &lv)
                    .await
                    .context("fetching Quilt loader profile")?,
            )
        }
        _ => None,
    };

    if let Some(profile) = &loader_profile {
        let _ = tx.send(LaunchEvent::Status("Downloading loader libraries".into()));
        if let Some(libs) = profile.get("libraries").and_then(Value::as_array) {
            for lib in libs {
                if let Some(p) = download_maven_library(&client, lib, &shared).await? {
                    classpath.push(p);
                }
            }
        }
    }

    let lib_libs = version_json
        .get("libraries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Which modules have a native jar built for THIS arch (and pass OS rules)?
    // Used so we only fall back to an x64 native (Rosetta) when no host-arch
    // one exists, instead of stranding a module with no native at all.
    let host_arch = host_native_arch();
    let host_native_modules: std::collections::HashSet<String> = lib_libs
        .iter()
        .filter(|lib| rules_allow(lib.get("rules")))
        .filter_map(|lib| lib.get("name").and_then(Value::as_str))
        .filter(|n| native_classifier_arch(n) == Some(host_arch))
        .map(maven_module_base)
        .collect();
    let mut skipped_native_arch = 0usize;

    for lib in &lib_libs {
        if !rules_allow(lib.get("rules")) {
            continue;
        }
        // Skip wrong-arch LWJGL natives (both x64 and arm64 jars pass the
        // os-only rule). Keep an x64 jar only as a Rosetta fallback when the
        // module ships no host-arch native.
        if let Some(name) = lib.get("name").and_then(Value::as_str) {
            if let Some(arch) = native_classifier_arch(name) {
                if arch != host_arch {
                    let has_host =
                        host_native_modules.contains(&maven_module_base(name));
                    if has_host || arch != "x64" {
                        skipped_native_arch += 1;
                        continue;
                    }
                }
            }
        }
        // Regular artifact -> classpath.
        if let Some(art) = lib
            .get("downloads")
            .and_then(|d| d.get("artifact"))
        {
            if let Some(p) = download_artifact(&client, art, &shared).await? {
                classpath.push(p);
            }
        }
        // Native classifier (1.18-) -> extract into instance natives dir.
        if let Some(natives) = lib.get("natives").and_then(Value::as_object) {
            let os_key = mojang_os();
            if let Some(class_tmpl) = natives.get(os_key).and_then(Value::as_str) {
                // Substitute ${arch} (e.g. natives-windows-${arch}).
                let classifier = class_tmpl.replace("${arch}", arch_bits());
                if let Some(cls) = lib
                    .get("downloads")
                    .and_then(|d| d.get("classifiers"))
                    .and_then(|c| c.get(&classifier))
                {
                    if let Some(jar) = download_artifact(&client, cls, &shared).await? {
                        let exclude = lib
                            .get("extract")
                            .and_then(|e| e.get("exclude"))
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        extract_natives(&jar, &natives_dir, &exclude)
                            .with_context(|| format!("extracting natives from {}", jar.display()))?;
                    }
                }
            }
        }
    }

    // Self-confirming line, inline in the launch log (not a transient status
    // chip) so it is visible in pasted logs: proves the arch filter ran and
    // which LWJGL native arch is actually on the classpath.
    let _ = tx.send(LaunchEvent::Log(format!(
        "[anvil] LWJGL natives: {host_arch} ({skipped_native_arch} wrong-arch jar(s) excluded)"
    )));

    // Client jar last on the classpath (vanilla). For Fabric, vanilla classes
    // still come from here; Fabric's loader (already earlier) wraps them.
    classpath.push(client_jar.clone());

    // Old JNA bundled by mods hard-aborts the JVM on modern macOS; upgrade it.
    ensure_modern_jna_macos(&mut classpath, &client, &shared, tx).await?;

    // --- Step 5: assets ---------------------------------------------------
    let (asset_index_id, assets_dir) =
        download_assets(&client, &version_json, &shared, tx).await?;

    // --- Step 6: loader mainClass (Fabric/Quilt) -------------------------
    let main_class = if let Some(profile) = &loader_profile {
        profile
            .get("mainClass")
            .and_then(|m| {
                // Fabric: string. Quilt: sometimes { "client": "...", ... }.
                m.as_str()
                    .map(str::to_string)
                    .or_else(|| {
                        m.get("client")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
            })
            .ok_or_else(|| anyhow!("loader profile missing mainClass"))?
    } else {
        version_json
            .get("mainClass")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("version JSON missing mainClass"))?
            .to_string()
    };

    // --- Step 7: instance mods -------------------------------------------
    if !instance.mods.is_empty() {
        let _ = tx.send(LaunchEvent::Status(format!(
            "Downloading {} mod(s)",
            instance.mods.len()
        )));
        let total = instance.mods.len() as u64;
        for (i, m) in instance.mods.iter().enumerate() {
            let dest = inst_dir.join(&m.path);
            ensure_mod(&client, m, &dest).await.with_context(|| {
                format!("downloading mod {} -> {}", m.name, dest.display())
            })?;
            let _ = tx.send(LaunchEvent::Progress {
                done: (i + 1) as u64,
                total,
                what: "mods".into(),
            });
        }
    }

    Ok(PreparedLaunch {
        classpath,
        main_class,
        asset_index_id,
        version_json,
        loader_profile,
        natives_dir,
        assets_dir,
        java_major,
    })
}

// ---------------------------------------------------------------------------
// Java
// ---------------------------------------------------------------------------

/// Determine the Java major version this Minecraft version needs.
///
/// Prefer the authoritative `javaVersion.majorVersion` field Mojang ships in
/// the version JSON (1.18+). Fall back to a coarse MC-version heuristic for
/// older JSONs that omit it: 1.20.5+ -> 21, 1.17..1.20.4 -> 17, else 8.
fn required_java_major(version_json: &Value, mc_version: &str) -> u32 {
    if let Some(m) = version_json
        .get("javaVersion")
        .and_then(|j| j.get("majorVersion"))
        .and_then(Value::as_u64)
    {
        return m as u32;
    }
    heuristic_java_major(mc_version)
}

/// Heuristic for legacy version JSONs lacking `javaVersion`. Parses the
/// leading `major.minor.patch` of a release id (e.g. "1.20.4"); snapshots /
/// unparseable ids fall through to the modern default (21).
fn heuristic_java_major(mc_version: &str) -> u32 {
    // Split off any snapshot/pre suffix; we only need the numeric release.
    let nums: Vec<u32> = mc_version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    let minor = nums.get(1).copied().unwrap_or(0);
    let patch = nums.get(2).copied().unwrap_or(0);
    if nums.first().copied().unwrap_or(1) != 1 {
        // Non-1.x (shouldn't happen for real MC ids) — assume modern.
        return 21;
    }
    if minor > 20 || (minor == 20 && patch >= 5) {
        21
    } else if minor >= 17 {
        17
    } else {
        8
    }
}

/// Run `java -version` (prints to stderr). We don't parse the version string —
/// distros format it differently and the user explicitly chose this path;
/// gating wrongly is worse than not gating. We only confirm it executes.
/// (Only used for an explicitly-provided `java_path`; the auto-provisioned
/// JRE is trusted by construction.)
async fn check_java(java: &str) -> Result<()> {
    let out = tokio::process::Command::new(java)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    match out {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!(
            "`{java} -version` exited with {} — install a JDK 17+/21 (or set a valid Java path)",
            s.code().unwrap_or(-1)
        ),
        Err(e) => bail!(
            "Java not found on PATH — install a JDK 17+/21 (tried `{java}`: {e})"
        ),
    }
}

// ---------------------------------------------------------------------------
// JRE auto-provisioning (Adoptium Temurin)
// ---------------------------------------------------------------------------

const ADOPTIUM_API: &str = "https://api.adoptium.net/v3";

/// Adoptium's `os` token for the current target. NOTE: this is *not*
/// `mojang_os()` — Adoptium uses `mac`, Mojang uses `osx`.
fn adoptium_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

/// Adoptium's `architecture` token. `std::env::consts::ARCH` is `aarch64` on
/// Apple Silicon / ARM Linux and `x86_64` on Intel/AMD; Adoptium wants
/// `aarch64` / `x64` respectively.
fn adoptium_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        _ => "x64",
    }
}

/// Provision (or reuse a cached) Adoptium Temurin JRE of the given major
/// version and return an absolute path to its `java` executable.
///
/// Cache layout: `shared_mc_dir()/runtimes/<major>/...` (the extracted
/// Temurin tree, top-level dir named e.g. `jdk-21.0.5+11-jre`). A second
/// launch with the same major finds the binary already on disk and skips all
/// network/extraction work.
///
/// macOS/Linux are the priority targets: Adoptium serves a `.tar.gz` which we
/// extract by shelling out to the system `tar` (universally present on those
/// platforms; we have no `tar` crate). Windows gets a `.zip`, extracted with
/// the bundled `zip` crate.
async fn provision_jre(major: u32, tx: &UnboundedSender<LaunchEvent>) -> Result<String> {
    let runtime_dir = shared_mc_dir()
        .join("runtimes")
        .join(major.to_string());

    // Fast path: a usable java is already cached for this major.
    if let Some(found) = find_java_binary(&runtime_dir).await {
        let _ = tx.send(LaunchEvent::Status(format!("Using cached Java {major}")));
        return Ok(found.to_string_lossy().into_owned());
    }

    tokio::fs::create_dir_all(&runtime_dir)
        .await
        .with_context(|| format!("creating runtime dir {}", runtime_dir.display()))?;

    let client = http_client()?;
    let api_url = format!(
        "{ADOPTIUM_API}/assets/latest/{major}/hotspot?architecture={arch}&image_type=jre&os={os}&vendor=eclipse",
        arch = adoptium_arch(),
        os = adoptium_os(),
    );
    let _ = tx.send(LaunchEvent::Status(format!("Downloading Java {major}")));

    let assets: Value = client
        .get(&api_url)
        .send()
        .await
        .with_context(|| format!("GET {api_url}"))?
        .error_for_status()
        .with_context(|| format!("Adoptium API {api_url} error status"))?
        .json()
        .await
        .context("parsing Adoptium assets JSON")?;

    // Response is an array of release assets; take the first binary's package.
    let pkg = assets
        .as_array()
        .and_then(|a| a.first())
        .and_then(|asset| asset.get("binary"))
        .and_then(|b| b.get("package"))
        .ok_or_else(|| {
            anyhow!(
                "Adoptium returned no JRE for Java {major} ({}/{}) — \
                 no binary.package in response",
                adoptium_os(),
                adoptium_arch()
            )
        })?;
    let link = pkg
        .get("link")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Adoptium package missing download link for Java {major}"))?
        .to_string();

    // Archive extension follows the platform (tar.gz on unix, zip on Windows).
    let archive_name = if cfg!(target_os = "windows") {
        "temurin.zip"
    } else {
        "temurin.tar.gz"
    };
    let archive = runtime_dir.join(archive_name);
    stream_download(&client, &link, &archive)
        .await
        .with_context(|| format!("downloading Temurin JRE {major} from {link}"))?;

    let _ = tx.send(LaunchEvent::Status(format!("Extracting Java {major}")));
    extract_jre_archive(&archive, &runtime_dir)
        .await
        .with_context(|| format!("extracting {}", archive.display()))?;
    // The archive itself is no longer needed; failing to delete is harmless.
    let _ = tokio::fs::remove_file(&archive).await;

    let java = find_java_binary(&runtime_dir).await.ok_or_else(|| {
        anyhow!(
            "extracted Temurin JRE {major} but no java binary found under {}",
            runtime_dir.display()
        )
    })?;

    // The system `tar` preserves the executable bit; the `zip` crate does not
    // restore unix perms, and a fresh extract may land without +x. Ensure it.
    make_executable(&java)
        .with_context(|| format!("making {} executable", java.display()))?;

    let _ = tx.send(LaunchEvent::Status(format!("Java {major} ready")));
    Ok(java.to_string_lossy().into_owned())
}

/// Extract the downloaded JRE archive into `dest`.
///
/// macOS/Linux: shell out to the system `tar` (`tar -xzf <archive> -C <dest>`)
/// — there is no `tar` crate in our allowed set, and `tar` ships with every
/// macOS/Linux install. Windows: Adoptium serves a `.zip`, extracted with the
/// `zip` crate (the same one used for natives), preserving the directory tree.
async fn extract_jre_archive(archive: &Path, dest: &Path) -> Result<()> {
    if cfg!(target_os = "windows") {
        let archive = archive.to_path_buf();
        let dest = dest.to_path_buf();
        // zip crate is sync + CPU/IO bound; don't block the async runtime.
        tokio::task::spawn_blocking(move || extract_zip_tree(&archive, &dest))
            .await
            .context("join zip-extract task")??;
        Ok(())
    } else {
        let status = tokio::process::Command::new("tar")
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(dest)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .context("spawning system `tar` to extract the JRE")?;
        if !status.success() {
            bail!(
                "`tar -xzf` failed (exit {}) extracting {}",
                status.code().unwrap_or(-1),
                archive.display()
            );
        }
        Ok(())
    }
}

/// Extract a full zip tree into `dest` (Windows JRE). Unlike `extract_natives`
/// this preserves the archive's directory structure (refusing `..` traversal).
fn extract_zip_tree(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("opening {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("reading zip {}", archive.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // `enclosed_name` rejects absolute paths and `..` components.
        let rel = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        let out_path = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .with_context(|| format!("creating {}", out_path.display()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut out = std::fs::File::create(&out_path)
            .with_context(|| format!("creating {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("extracting {}", rel.display()))?;
    }
    Ok(())
}

/// Walk the runtime dir (bounded depth) for the platform's `java` executable.
/// Temurin layouts:
///   macOS  : `<dir>/jdk-*-jre/Contents/Home/bin/java`
///   Linux  : `<dir>/jdk-*-jre/bin/java`
///   Windows: `<dir>/jdk-*-jre/bin/java.exe`
/// Returns the first match. `None` => not provisioned yet.
async fn find_java_binary(root: &Path) -> Option<PathBuf> {
    let exe = if cfg!(target_os = "windows") {
        "java.exe"
    } else {
        "java"
    };
    // Depth is bounded (~5) — enough for `<root>/<top>/Contents/Home/bin/java`
    // and the Linux/Windows shallower trees — without an unbounded recursion.
    find_named(root, exe, 5).await
}

/// Bounded breadth-first search for a regular file named `target` under
/// `root`. Pure tokio fs; missing/permission-denied dirs are skipped, not
/// fatal (a not-yet-extracted runtime simply yields `None`).
async fn find_named(root: &Path, target: &str, max_depth: usize) -> Option<PathBuf> {
    let mut queue: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = queue.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let ft = match entry.file_type().await {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_file() {
                if entry.file_name().to_string_lossy() == target {
                    return Some(path);
                }
            } else if ft.is_dir() && depth < max_depth {
                queue.push((path, depth + 1));
            }
        }
    }
    None
}

/// Ensure `path` has the owner-execute bit on unix (the `zip` crate drops
/// perms; a fresh `tar` extract usually keeps them but we don't rely on it).
/// No-op on Windows (executability is by extension there).
fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path)
            .with_context(|| format!("stat {}", path.display()))?;
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("chmod +x {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP / download helpers
// ---------------------------------------------------------------------------

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("anvil/0.1.0 (custom-launcher)")
        .build()
        .context("building reqwest client")
}

/// Stream a URL to `dest` (creating parent dirs). Used for big jars/objects so
/// we never buffer a whole file in memory.
async fn stream_download(client: &reqwest::Client, url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating dir {}", parent.display()))?;
    }
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url} returned an error status"))?;

    // Download to a temp file then rename, so a crash mid-download never
    // leaves a truncated file that later "exists" and gets skipped.
    let tmp = dest.with_extension("anvil-part");
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("creating {}", tmp.display()))?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("reading body of {url}"))?;
        file.write_all(&chunk)
            .await
            .with_context(|| format!("writing {}", tmp.display()))?;
    }
    file.flush().await.ok();
    drop(file);
    tokio::fs::rename(&tmp, dest)
        .await
        .with_context(|| format!("renaming {} -> {}", tmp.display(), dest.display()))?;
    Ok(())
}

/// Download `url` to `dest` unless a non-empty file is already there.
///
/// TODO: SHA-1 verification needs a sha1 crate (not in deps). Mojang publishes
/// SHA-1 only; `sha2` cannot compute it. So we accept any existing non-empty
/// file as valid (size>0 + presence). Mods (SHA-512) are verified properly.
async fn download_if_missing(client: &reqwest::Client, url: &str, dest: &Path) -> Result<()> {
    if let Ok(meta) = tokio::fs::metadata(dest).await {
        if meta.is_file() && meta.len() > 0 {
            return Ok(());
        }
    }
    stream_download(client, url, dest).await
}

// ---------------------------------------------------------------------------
// Version manifest
// ---------------------------------------------------------------------------

/// Step 2: locate `mc_version` in the manifest, fetch its version JSON, cache
/// under `shared/versions/<id>/<id>.json`.
async fn fetch_version_json(
    client: &reqwest::Client,
    shared: &Path,
    mc_version: &str,
) -> Result<Value> {
    let cache = shared
        .join("versions")
        .join(mc_version)
        .join(format!("{mc_version}.json"));
    if let Ok(txt) = tokio::fs::read_to_string(&cache).await {
        if let Ok(v) = serde_json::from_str::<Value>(&txt) {
            if v.get("downloads").is_some() || v.get("mainClass").is_some() {
                return Ok(v);
            }
        }
    }

    let manifest: Value = client
        .get(VERSION_MANIFEST)
        .send()
        .await
        .context("GET version_manifest_v2")?
        .error_for_status()
        .context("version_manifest_v2 error status")?
        .json()
        .await
        .context("parsing version_manifest_v2 JSON")?;

    let entry = manifest
        .get("versions")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .find(|v| v.get("id").and_then(Value::as_str) == Some(mc_version))
        })
        .ok_or_else(|| anyhow!("Minecraft version '{mc_version}' not found in manifest"))?;

    let url = entry
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest entry for '{mc_version}' missing url"))?;

    let version_json: Value = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET version JSON for {mc_version}"))?
        .error_for_status()
        .context("version JSON error status")?
        .json()
        .await
        .context("parsing version JSON")?;

    if let Some(parent) = cache.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tokio::fs::write(&cache, serde_json::to_vec_pretty(&version_json)?)
        .await
        .with_context(|| format!("caching version JSON to {}", cache.display()))?;

    Ok(version_json)
}

// ---------------------------------------------------------------------------
// Library rules (OS gating)
// ---------------------------------------------------------------------------

/// Mojang's OS name for the current target (`osx`, not `macos`).
fn mojang_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "osx"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

/// Mojang's arch token used for `${arch}` substitution in native classifiers.
fn arch_bits() -> &'static str {
    if cfg!(target_pointer_width = "32") {
        "32"
    } else {
        "64"
    }
}

// 1.19+ ships per-arch LWJGL natives as separate classpath jars
// (`org.lwjgl:lwjgl:3.3.1:natives-macos` AND `:natives-macos-arm64`) gated
// ONLY by `os.name` -- both pass `rules_allow` on macOS. Mojang's launcher
// picks by host arch, not rules; without the same filter the wrong-arch jar
// lands on an Apple Silicon classpath and crashes the GL bootstrap. The two
// helpers below restore that arch selection.

/// Host CPU as the LWJGL/Mojang native-classifier arch token.
fn host_native_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "arm" => "arm32",
        "x86" => "x86",
        _ => "x64", // x86_64
    }
}

/// The arch a `natives-<os>[-<arch>]` maven classifier targets, or `None` if
/// `name` is not an arch-specific native artifact (so it is unaffected).
fn native_classifier_arch(name: &str) -> Option<&'static str> {
    let cls = name.split(':').nth(3)?.split('@').next()?;
    if !cls.starts_with("natives-") {
        return None;
    }
    Some(if cls.ends_with("-arm64") {
        "arm64"
    } else if cls.ends_with("-arm32") || cls.ends_with("-arm") {
        "arm32"
    } else if cls.ends_with("-x86") {
        "x86"
    } else {
        "x64" // plain `natives-<os>` is the 64-bit build
    })
}

/// `group:artifact:version` of a maven coord (classifier stripped), used to
/// tell whether a module has a host-arch native sibling.
fn maven_module_base(name: &str) -> String {
    name.split(':').take(3).collect::<Vec<_>>().join(":")
}

/// Evaluate a Mojang `rules` array for the current OS. Default policy when a
/// `rules` array is present is DENY; each matching rule flips the state to its
/// `action`. No `rules` (or non-array) => allowed.
fn rules_allow(rules: Option<&Value>) -> bool {
    let arr = match rules.and_then(Value::as_array) {
        Some(a) if !a.is_empty() => a,
        _ => return true, // no rules => always allowed
    };
    let mut allowed = false;
    for rule in arr {
        if rule_matches(rule) {
            allowed = rule
                .get("action")
                .and_then(Value::as_str)
                .map(|a| a == "allow")
                .unwrap_or(false);
        }
    }
    allowed
}

/// Does this single rule's `os` (and we ignore `features`, treating any
/// feature-gated arg as not-applicable for a plain launch) match us?
fn rule_matches(rule: &Value) -> bool {
    // `features` (e.g. is_demo_user, has_custom_resolution) never apply to a
    // standard launch — a rule that gates on a feature does not match.
    if rule.get("features").is_some() {
        return false;
    }
    match rule.get("os") {
        None => true, // OS-less rule applies to everyone
        Some(os) => {
            if let Some(name) = os.get("name").and_then(Value::as_str) {
                if name != mojang_os() {
                    return false;
                }
            }
            if let Some(arch) = os.get("arch").and_then(Value::as_str) {
                // Mojang's `os.arch` is in practice only ever "x86", meaning
                // "this rule applies only on a 32-bit JVM". On a 64-bit
                // target such a rule must NOT match. Any other value we treat
                // as matching (no other token is used in real version JSONs).
                if arch == "x86" && cfg!(target_pointer_width = "64") {
                    return false;
                }
            }
            // `os.version` is a host-version regex; extremely rare and we
            // can't pull a regex crate, so we conservatively ignore it
            // (treat as matching) — being permissive here only ever adds an
            // already-OS-correct library.
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Library downloads
// ---------------------------------------------------------------------------

/// Download a Mojang `downloads.artifact`/`classifiers[..]` object (has
/// `path`, `url`) into `shared/libraries/<path>`. Returns the local path.
async fn download_artifact(
    client: &reqwest::Client,
    art: &Value,
    shared: &Path,
) -> Result<Option<PathBuf>> {
    let url = match art.get("url").and_then(Value::as_str) {
        Some(u) if !u.is_empty() => u,
        _ => return Ok(None),
    };
    let rel = art
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("library artifact missing path"))?;
    let dest = shared.join("libraries").join(rel);
    download_if_missing(client, url, &dest).await?;
    Ok(Some(dest))
}

/// Fabric/Quilt-style library entry: `name` is a maven coordinate
/// `group:artifact:version[:classifier]`, `url` is a maven repo base. Build
/// the jar path from the coordinate and download into the shared libs dir.
async fn download_maven_library(
    client: &reqwest::Client,
    lib: &Value,
    shared: &Path,
) -> Result<Option<PathBuf>> {
    // Loader libs may still carry rules (rare) — respect them.
    if !rules_allow(lib.get("rules")) {
        return Ok(None);
    }
    let name = match lib.get("name").and_then(Value::as_str) {
        Some(n) => n,
        None => return Ok(None),
    };
    let rel = maven_coord_to_path(name)
        .ok_or_else(|| anyhow!("bad maven coordinate '{name}'"))?;

    // Some entries provide an explicit `url` to the exact jar; most provide a
    // repo base that we append the maven path to.
    let base = lib
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("https://maven.fabricmc.net/");
    let url = if base.ends_with(".jar") {
        base.to_string()
    } else {
        format!("{}{}", ensure_trailing_slash(base), rel)
    };

    let dest = shared.join("libraries").join(&rel);
    download_if_missing(client, &url, &dest).await?;
    Ok(Some(dest))
}

fn ensure_trailing_slash(s: &str) -> String {
    if s.ends_with('/') {
        s.to_string()
    } else {
        format!("{s}/")
    }
}

/// JNA `< 5.13` HARD-ABORTS the JVM on modern macOS: when a library load
/// fails, `dispatch.c`'s `LOAD_ERROR` does `assert(count <= len)` on the
/// `dlerror()` string, which the long macOS message overflows -> the exact
/// `Assertion failed: ... snprintf() output has been truncated ... dispatch.c
/// line 74` crash, killing Minecraft at startup (e.g. OSHI probing IOKit).
/// JNA 5.13+ truncates gracefully and resolves macOS frameworks. Many mods
/// still ship 5.12.x, so on macOS transparently swap the classpath jar for
/// 5.14.0 (JNA is binary-stable across 5.x). No-op off macOS, or when JNA is
/// absent / already >= 5.13.
#[cfg(target_os = "macos")]
async fn ensure_modern_jna_macos(
    classpath: &mut [PathBuf],
    client: &reqwest::Client,
    shared: &Path,
    tx: &tokio::sync::mpsc::UnboundedSender<LaunchEvent>,
) -> Result<()> {
    const TARGET: &str = "5.14.0";

    let Some(idx) = classpath.iter().position(|p| {
        let s = p.to_string_lossy();
        s.contains("/net/java/dev/jna/jna/")
            && p.file_name()
                .and_then(|f| f.to_str())
                .map(|f| f.starts_with("jna-") && f.ends_with(".jar"))
                .unwrap_or(false)
    }) else {
        return Ok(()); // no JNA on the classpath
    };

    // Maven layout: .../net/java/dev/jna/jna/<version>/jna-<version>.jar
    let ver = classpath[idx]
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_string();
    let mut nums = ver.split('.').filter_map(|n| n.parse::<u32>().ok());
    let mm = (nums.next().unwrap_or(0), nums.next().unwrap_or(0));
    if mm >= (5, 13) {
        return Ok(()); // already non-aborting
    }

    let dest = shared
        .join("libraries/net/java/dev/jna/jna")
        .join(TARGET)
        .join(format!("jna-{TARGET}.jar"));
    let url = format!(
        "https://repo1.maven.org/maven2/net/java/dev/jna/jna/{TARGET}/jna-{TARGET}.jar"
    );
    download_if_missing(client, &url, &dest)
        .await
        .with_context(|| format!("downloading JNA {TARGET} (macOS crash fix)"))?;
    let _ = tx.send(LaunchEvent::Log(format!(
        "[anvil] JNA {ver} -> {TARGET} (older JNA aborts the JVM on this macOS)"
    )));
    classpath[idx] = dest;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
async fn ensure_modern_jna_macos(
    _classpath: &mut [PathBuf],
    _client: &reqwest::Client,
    _shared: &Path,
    _tx: &tokio::sync::mpsc::UnboundedSender<LaunchEvent>,
) -> Result<()> {
    Ok(())
}

/// The `-jar` registry-dump server cannot use `ensure_modern_jna_macos`
/// (that mutates a constructed classpath; the Mojang bundler self-manages
/// its own). Return a cached jna-5.14.0 jar for `-Xbootclasspath/a:`
/// instead — the bootstrap loader wins over the bundler's URLClassLoader,
/// so 5.14.0 shadows the aborting 5.12.1. Cached under the shared MC dir
/// (downloaded once, reused by every verify AND the client path). macOS
/// only; `None` elsewhere or if the one-time download fails (the dump then
/// keeps its prior best-effort behaviour — fix 4 keeps that honest, not a
/// fabricated culprit).
#[cfg(target_os = "macos")]
async fn macos_jna_boot_jar(client: &reqwest::Client) -> Option<std::path::PathBuf> {
    const TARGET: &str = "5.14.0";
    let dest = shared_mc_dir()
        .join("libraries/net/java/dev/jna/jna")
        .join(TARGET)
        .join(format!("jna-{TARGET}.jar"));
    let url = format!(
        "https://repo1.maven.org/maven2/net/java/dev/jna/jna/{TARGET}/jna-{TARGET}.jar"
    );
    download_if_missing(client, &url, &dest).await.ok()?;
    Some(dest)
}

#[cfg(not(target_os = "macos"))]
async fn macos_jna_boot_jar(
    _client: &reqwest::Client,
) -> Option<std::path::PathBuf> {
    None
}

/// `group:artifact:version[:classifier]` ->
/// `group/with/slashes/artifact/version/artifact-version[-classifier].jar`.
fn maven_coord_to_path(coord: &str) -> Option<String> {
    let parts: Vec<&str> = coord.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).filter(|c| !c.is_empty());
    let file = match classifier {
        Some(c) => format!("{artifact}-{version}-{c}.jar"),
        None => format!("{artifact}-{version}.jar"),
    };
    Some(format!("{group}/{artifact}/{version}/{file}"))
}

// ---------------------------------------------------------------------------
// Natives extraction
// ---------------------------------------------------------------------------

/// Unzip a native jar into `dest`, skipping `META-INF` and any entry whose
/// path starts with one of `exclude`. Synchronous (zip crate is sync) — the
/// jars are small; fine to block briefly.
fn extract_natives(jar: &Path, dest: &Path, exclude: &[String]) -> Result<()> {
    std::fs::create_dir_all(dest)
        .with_context(|| format!("creating natives dir {}", dest.display()))?;
    let file = std::fs::File::open(jar)
        .with_context(|| format!("opening native jar {}", jar.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("reading native jar {}", jar.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        if name.starts_with("META-INF") || name.ends_with('/') {
            continue;
        }
        if exclude.iter().any(|ex| name.starts_with(ex.as_str())) {
            continue;
        }
        // Flatten safely: take just the file name, refusing path traversal.
        let out_name = Path::new(&name)
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&name));
        let out_path = dest.join(&out_name);
        let mut out = std::fs::File::create(&out_path)
            .with_context(|| format!("creating {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("extracting {name}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Assets
// ---------------------------------------------------------------------------

/// Step 5: fetch the asset index, then download every object (deduped,
/// concurrent). Returns (assetIndex.id, shared assets dir).
async fn download_assets(
    client: &reqwest::Client,
    version_json: &Value,
    shared: &Path,
    tx: &UnboundedSender<LaunchEvent>,
) -> Result<(String, PathBuf)> {
    let assets_dir = shared.join("assets");
    let asset_index = version_json
        .get("assetIndex")
        .ok_or_else(|| anyhow!("version JSON missing assetIndex"))?;
    let index_id = asset_index
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("assetIndex missing id"))?
        .to_string();
    let index_url = asset_index
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("assetIndex missing url"))?;

    let index_path = assets_dir.join("indexes").join(format!("{index_id}.json"));
    let index: Value = if let Ok(txt) = tokio::fs::read_to_string(&index_path).await {
        serde_json::from_str(&txt).context("parsing cached asset index")?
    } else {
        let v: Value = client
            .get(index_url)
            .send()
            .await
            .context("GET asset index")?
            .error_for_status()
            .context("asset index error status")?
            .json()
            .await
            .context("parsing asset index JSON")?;
        if let Some(p) = index_path.parent() {
            tokio::fs::create_dir_all(p).await.ok();
        }
        tokio::fs::write(&index_path, serde_json::to_vec_pretty(&v)?)
            .await
            .context("caching asset index")?;
        v
    };

    let objects = index
        .get("objects")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    // Mojang asset indexes routinely list multiple objects sharing one hash
    // (identical files across locales). Dedupe BEFORE downloading: two
    // concurrent tasks on the same hash would otherwise both create — and
    // race on renaming — the same `.anvil-part` temp file, making a fresh
    // prepare fail with a confusing "no such file" rename error.
    let mut hashes: Vec<String> = objects
        .values()
        .filter_map(|o| {
            o.get("hash")
                .and_then(Value::as_str)
                .map(|h| h.to_string())
        })
        .collect();
    hashes.sort();
    hashes.dedup();
    let total = hashes.len() as u64;
    let _ = tx.send(LaunchEvent::Status(format!(
        "Downloading {total} asset object(s)"
    )));

    // Concurrent with bounded parallelism so a 1.20 pack (~3700 objects)
    // doesn't take minutes serially.
    let objects_dir = assets_dir.join("objects");
    let mut tasks = futures_util::stream::iter(hashes)
    .map(|hash| {
        let client = client.clone();
        let objects_dir = objects_dir.clone();
        async move {
            let sub = &hash[..2.min(hash.len())];
            let dest = objects_dir.join(sub).join(&hash);
            // TODO: SHA-1 verification needs a sha1 crate (not in deps). We
            // accept any existing non-empty object; presence = good enough.
            if let Ok(meta) = tokio::fs::metadata(&dest).await {
                if meta.is_file() && meta.len() > 0 {
                    return Ok::<(), anyhow::Error>(());
                }
            }
            let url = format!("{RESOURCES_BASE}/{sub}/{hash}");
            stream_download(&client, &url, &dest).await
        }
    })
    .buffer_unordered(ASSET_PARALLELISM);

    let mut done: u64 = 0;
    while let Some(res) = tasks.next().await {
        res.context("downloading asset object")?;
        done += 1;
        if done % PROGRESS_BATCH == 0 || done == total {
            let _ = tx.send(LaunchEvent::Progress {
                done,
                total,
                what: "assets".into(),
            });
        }
    }

    Ok((index_id, assets_dir))
}

// ---------------------------------------------------------------------------
// Fabric / Quilt loader profile
// ---------------------------------------------------------------------------

/// Pick the loader version to actually install. Fabric/Quilt loaders are
/// backward-compatible and mods only ever require a *minimum* loader, so we
/// always install the newest *stable* loader for the MC version (this is what
/// every launcher does, and it's why pinning `loader_version` — e.g. an old
/// "0.16.0" — caused "Incompatible mods found" at runtime). A non-empty
/// `pinned` value is used only if the meta listing is unreachable.
async fn resolve_loader_version(
    client: &reqwest::Client,
    meta_base: &str,
    mc_version: &str,
    pinned: &str,
) -> Result<String> {
    let url = format!("{meta_base}/{mc_version}");
    let list: Vec<Value> = match client.get(&url).send().await {
        Ok(resp) => match resp.error_for_status() {
            Ok(ok) => ok.json().await.unwrap_or_default(),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    };

    // Entries are newest-first. Fabric marks stability with `loader.stable`;
    // Quilt has no such flag, so skip pre-release suffixes there.
    let newest_stable = list.iter().find_map(|e| {
        let l = e.get("loader")?;
        let v = l.get("version")?.as_str()?;
        let stable = match l.get("stable").and_then(Value::as_bool) {
            Some(s) => s,
            None => {
                !v.contains("-beta") && !v.contains("-pre") && !v.contains("-rc")
            }
        };
        stable.then(|| v.to_string())
    });
    let newest_any = || {
        list.iter()
            .find_map(|e| Some(e.get("loader")?.get("version")?.as_str()?.to_string()))
    };

    newest_stable
        .or_else(newest_any)
        .or_else(|| {
            let p = pinned.trim();
            (!p.is_empty() && p != "latest").then(|| p.to_string())
        })
        .ok_or_else(|| {
            let kind = if meta_base.contains("quilt") {
                "Quilt"
            } else {
                "Fabric"
            };
            anyhow!("could not resolve a {kind} loader for Minecraft {mc_version}")
        })
}

/// GET `<meta>/<mc>/<loader>/profile/json` — a launcher profile with extra
/// `libraries`, a `mainClass`, and (sometimes) extra `arguments`.
async fn fetch_fabric_profile(
    client: &reqwest::Client,
    meta_base: &str,
    mc_version: &str,
    loader_version: &str,
) -> Result<Value> {
    let url = format!("{meta_base}/{mc_version}/{loader_version}/profile/json");
    let v: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("loader profile {url} error status"))?
        .json()
        .await
        .context("parsing loader profile JSON")?;
    Ok(v)
}

// ---------------------------------------------------------------------------
// Mods
// ---------------------------------------------------------------------------

/// Ensure a pinned mod is present at `dest` and matches its SHA-512.
/// (Modrinth provides SHA-512; `sha2::Sha512` can verify it.)
///
/// `pub(crate)` so the curator can pre-warm these jars at curation/quest time
/// (registry grounding scans them on disk; without a prefetch they only exist
/// post-launch). Behavior is unchanged — same path, same verify, same
/// delete-on-mismatch — so this also warms the launcher's own cache.
pub(crate) async fn ensure_mod(
    client: &reqwest::Client,
    m: &crate::instance::PinnedMod,
    dest: &Path,
) -> Result<()> {
    if let Ok(bytes) = tokio::fs::read(dest).await {
        if !bytes.is_empty() && sha512_hex(&bytes) == m.sha512.to_lowercase() {
            return Ok(());
        }
    }
    stream_download(client, &m.download_url, dest).await?;
    let bytes = tokio::fs::read(dest)
        .await
        .with_context(|| format!("re-reading downloaded mod {}", dest.display()))?;
    let got = sha512_hex(&bytes);
    if !m.sha512.is_empty() && got != m.sha512.to_lowercase() {
        // Remove the bad file so a retry re-downloads it.
        let _ = tokio::fs::remove_file(dest).await;
        bail!(
            "SHA-512 mismatch for mod '{}' (expected {}, got {})",
            m.name,
            m.sha512,
            got
        );
    }
    Ok(())
}

fn sha512_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha512};
    let mut h = Sha512::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

// ---------------------------------------------------------------------------
// Launch command construction
// ---------------------------------------------------------------------------

/// Step 8: build the full `java` argument vector (JVM args, main class, game
/// args) with all placeholders substituted.
fn build_command_args(
    instance: &Instance,
    account: &MinecraftAccount,
    p: &PreparedLaunch,
) -> Result<Vec<String>> {
    let sep = if cfg!(target_os = "windows") { ";" } else { ":" };
    let classpath = p
        .classpath
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(sep);

    let inst_dir = instance_dir(&instance.id);

    // Placeholder table. We include the extras modern JSONs reference
    // (auth_xuid/clientid/classpath_separator) so no `${...}` literal ever
    // reaches the JVM and breaks arg parsing.
    let subst = |s: &str| -> String {
        s.replace("${auth_player_name}", &account.username)
            .replace("${auth_uuid}", &account.uuid)
            .replace("${auth_access_token}", &account.minecraft_token)
            .replace("${auth_xuid}", "")
            .replace("${clientid}", "")
            .replace("${user_type}", "msa")
            .replace("${version_name}", &instance.mc_version)
            .replace("${version_type}", "release")
            .replace("${game_directory}", &inst_dir.to_string_lossy())
            .replace("${assets_root}", &p.assets_dir.to_string_lossy())
            .replace("${assets_index_name}", &p.asset_index_id)
            .replace("${natives_directory}", &p.natives_dir.to_string_lossy())
            .replace("${classpath}", &classpath)
            .replace("${classpath_separator}", sep)
            .replace("${launcher_name}", "anvil")
            .replace("${launcher_version}", "0.1.0")
            .replace("${user_properties}", "{}")
            // Legacy 1.7-ish placeholders that occasionally appear.
            .replace("${auth_session}", &account.minecraft_token)
            .replace("${game_assets}", &p.assets_dir.to_string_lossy())
            .replace("${profile_name}", &instance.name)
    };

    let mut args: Vec<String> = Vec::new();

    let modern_jvm = p
        .version_json
        .get("arguments")
        .and_then(|a| a.get("jvm"))
        .and_then(Value::as_array);

    if let Some(jvm) = modern_jvm {
        // Modern format.
        push_arg_array(&mut args, jvm, &subst);
    } else {
        // Legacy versions have no JVM args block — supply the essentials.
        args.push(format!("-Djava.library.path={}", p.natives_dir.display()));
        args.push("-cp".into());
        args.push(classpath.clone());
    }

    // Loader profiles (Fabric/Quilt) may add JVM args too.
    if let Some(profile) = &p.loader_profile {
        if let Some(jvm) = profile
            .get("arguments")
            .and_then(|a| a.get("jvm"))
            .and_then(Value::as_array)
        {
            push_arg_array(&mut args, jvm, &subst);
        }
    }

    // TEMPORARY DIAGNOSTIC: make JNA log exactly which native library it tries
    // to open and from where, right before it aborts in dispatch.c's
    // LOAD_ERROR. Harmless system properties (no-ops if JNA is absent); remove
    // once the failing JNA target is identified.
    args.push("-Djna.debug_load=true".into());
    args.push("-Djna.debug_load.jna=true".into());

    // Opt-in per-instance log4j2 override: if the user (or a debug session)
    // dropped a `log4j2.xml` at `<instance>/config/log4j2.xml`, point the JVM
    // at it so we can raise specific loggers (e.g. `earth.terrarium`) to
    // DEBUG without rebuilding the bundled MC config. Absent file → no-op.
    let log4j_cfg = instance_dir(&instance.id).join("config").join("log4j2.xml");
    if log4j_cfg.is_file() {
        args.push(format!(
            "-Dlog4j.configurationFile={}",
            log4j_cfg.display()
        ));
    }

    args.push(p.main_class.clone());

    // Game args: modern `arguments.game` or legacy `minecraftArguments`.
    if let Some(game) = p
        .version_json
        .get("arguments")
        .and_then(|a| a.get("game"))
        .and_then(Value::as_array)
    {
        push_arg_array(&mut args, game, &subst);
    } else if let Some(legacy) = p
        .version_json
        .get("minecraftArguments")
        .and_then(Value::as_str)
    {
        for tok in legacy.split_whitespace() {
            args.push(subst(tok));
        }
    }

    // Loader profile game args appended after vanilla's.
    if let Some(profile) = &p.loader_profile {
        if let Some(game) = profile
            .get("arguments")
            .and_then(|a| a.get("game"))
            .and_then(Value::as_array)
        {
            push_arg_array(&mut args, game, &subst);
        }
    }

    Ok(args)
}

/// Push a modern `arguments.{jvm,game}` array: each element is a plain string
/// OR an object `{ rules, value }`. Objects whose rules deny are skipped;
/// `value` may be a string (1 arg) or array (many).
fn push_arg_array(out: &mut Vec<String>, arr: &[Value], subst: &impl Fn(&str) -> String) {
    for el in arr {
        match el {
            Value::String(s) => out.push(subst(s)),
            Value::Object(_) => {
                if !rules_allow(el.get("rules")) {
                    continue;
                }
                match el.get("value") {
                    Some(Value::String(s)) => out.push(subst(s)),
                    Some(Value::Array(vals)) => {
                        for v in vals {
                            if let Some(s) = v.as_str() {
                                out.push(subst(s));
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{maven_module_base, native_classifier_arch};

    #[test]
    fn classifier_arch_parsing_covers_every_platform_token() {
        let c = native_classifier_arch;
        // Apple Silicon vs Intel mac (the bug: both pass the os-only rule).
        assert_eq!(c("org.lwjgl:lwjgl-glfw:3.3.1:natives-macos"), Some("x64"));
        assert_eq!(
            c("org.lwjgl:lwjgl-glfw:3.3.1:natives-macos-arm64"),
            Some("arm64")
        );
        // Windows family.
        assert_eq!(c("org.lwjgl:lwjgl:3.3.1:natives-windows"), Some("x64"));
        assert_eq!(
            c("org.lwjgl:lwjgl:3.3.1:natives-windows-arm64"),
            Some("arm64")
        );
        assert_eq!(c("org.lwjgl:lwjgl:3.3.1:natives-windows-x86"), Some("x86"));
        // Linux incl. ARM boards.
        assert_eq!(c("org.lwjgl:lwjgl:3.3.1:natives-linux"), Some("x64"));
        assert_eq!(
            c("org.lwjgl:lwjgl:3.3.1:natives-linux-arm32"),
            Some("arm32")
        );
        // Non-native libs and odd shapes are left untouched.
        assert_eq!(c("com.google.code.gson:gson:2.10"), None);
        assert_eq!(c("net.fabricmc:fabric-loader:0.19.2"), None);
        assert_eq!(c("org.lwjgl:lwjgl:3.3.1"), None);
        assert_eq!(c("org.lwjgl:lwjgl:3.3.1:natives-macos@zip"), Some("x64"));
    }

    #[test]
    fn module_base_strips_classifier() {
        assert_eq!(
            maven_module_base("org.lwjgl:lwjgl-glfw:3.3.1:natives-macos-arm64"),
            "org.lwjgl:lwjgl-glfw:3.3.1"
        );
        // The x64 and arm64 jars of one module share a base, so the
        // host-arch-exists check pairs them correctly.
        assert_eq!(
            maven_module_base("org.lwjgl:lwjgl-glfw:3.3.1:natives-macos"),
            maven_module_base("org.lwjgl:lwjgl-glfw:3.3.1:natives-macos-arm64")
        );
    }
}


#[cfg(test)]
mod smoke_tests {
    use super::{classify_smoke_line, classify_world_join_line, parse_fabric_remediation, SmokeSignal, WorldJoinSignal};

    #[test]
    fn detects_the_exact_failures_seen_this_session() {
        // Spectrum missing-dep (the resolver-blindness crash).
        let s = classify_smoke_line(
            " - Mod 'Spectrum' (spectrum) 1.8.13 requires version 1.77.3 \
             or later of modonomicon, which is missing!",
        );
        assert!(matches!(
            s,
            SmokeSignal::Failure { ref mod_name, .. } if mod_name.as_deref() == Some("Spectrum")
        ));

        // Fabric's umbrella rejection banner.
        assert!(matches!(
            classify_smoke_line("Incompatible mods found!"),
            SmokeSignal::Failure { .. }
        ));

        // create_dd entrypoint crash (Create 6 API break).
        let s = classify_smoke_line(
            "Could not execute entrypoint stage 'main' due to errors, \
             provided by 'create_dd' at 'uwu.lopyluna.create_dd.DDCreate'!",
        );
        assert_eq!(
            s,
            SmokeSignal::Failure {
                mod_name: Some("create_dd".into()),
                reason: "A mod's initializer threw during startup".into(),
            }
        );

        // The Kotlin/IPN class shows up as NoClassDef/crash report too.
        assert!(matches!(
            classify_smoke_line(
                "Caused by: java.lang.NoClassDefFoundError: \
                 com/simibubi/create/foundation/utility/Components"
            ),
            SmokeSignal::Failure { .. }
        ));
        assert!(matches!(
            classify_smoke_line(
                "#@!@# Game crashed! Crash report saved to: #@!@# /x/crash.txt"
            ),
            SmokeSignal::Failure { .. }
        ));
    }

    /// The world-CREATION crash class the new world-gen gate exists to catch,
    /// using the EXACT lines from the real Stardew Hollow server crash
    /// (`crash-2026-05-18_20.37.58-server.txt` + its preceding WARNs). The
    /// gate's `drive_dump_server` scan maps any `Failure` here to
    /// `DumpOutcome::Crashed`; the benign probes must stay `None` or every
    /// pack with an optional cross-mod integration would false-positive.
    #[test]
    fn villagernames_world_creation_crash_is_detected_not_the_benign_probes() {
        // BENIGN — ubiquitous optional-integration probes Fabric logs at
        // mod-load. These fire for villagernames+guardvillagers too and must
        // NOT abort the verification boot.
        assert_eq!(
            classify_smoke_line(
                "[20:36:45] [main/WARN]: Error loading class: \
                 dev/mrsterner/guardvillagers/common/entity/GuardEntity \
                 (java.lang.ClassNotFoundException: \
                 dev/mrsterner/guardvillagers/common/entity/GuardEntity)"
            ),
            SmokeSignal::None,
            "an `Error loading class` WARN is an optional-compat probe"
        );
        assert_eq!(
            classify_smoke_line(
                "[20:36:45] [main/WARN]: @Mixin target \
                 dev.mrsterner.guardvillagers.common.entity.GuardEntity was \
                 not found villagernames.mixins.json:guard.GuardEntityMixin \
                 from mod villagernames"
            ),
            SmokeSignal::None,
            "an unresolved @Mixin target WARN is not a crash"
        );

        // FATAL — the actual world-tick crash (verbatim from the crash
        // report). Each MUST be a Failure so the gate returns Crashed and
        // BLOCKS quest generation instead of shipping a world-killing pack.
        for fatal in [
            "---- Minecraft Crash Report ----",
            "java.lang.NoClassDefFoundError: \
             dev/mrsterner/guardvillagers/common/entity/GuardEntity",
            "Caused by: java.lang.ClassNotFoundException: \
             dev.mrsterner.guardvillagers.common.entity.GuardEntity",
        ] {
            assert!(
                matches!(
                    classify_smoke_line(fatal),
                    SmokeSignal::Failure { .. }
                ),
                "must classify as Failure: {fatal:?}"
            );
        }
    }

    // ---- world_join_probe classifier — IPN regression + benign probes ----
    //
    // Verbatim from the UCL: Bentham Ultimatum crash report
    // (crash-2026-05-21_00.05.20-client.txt) — the EXACT line shapes the
    // probe must catch. smoke_test sees none of these because they only
    // appear AFTER the main-menu success milestone (sound engine started)
    // when the player attempts world creation.
    #[test]
    fn ipn_kotlin_reflection_world_join_crash_is_a_failure() {
        let real = "kotlin.reflect.jvm.internal.KotlinReflectionInternalError: \
                    Property 'none' (JVM signature: getNone()Lkotlin/jvm/functions/Function0;) \
                    not resolved in file class \
                    org.anti_ad.mc.ipnext.item.rule.natives.DefinedNativeRulesKt";
        match classify_world_join_line(real) {
            WorldJoinSignal::Failure { reason, .. } => {
                assert!(reason.contains("Kotlin reflection"),
                    "reason must surface the actual cause; got `{reason}`");
            }
            other => panic!("IPN kotlin-reflection error must be Failure; got {other:?}"),
        }
    }

    #[test]
    fn on_join_world_throwing_classifies_as_failure() {
        // From the same crash report — the actual stack-frame line that
        // names IPN's broken `onJoinWorld` hook.
        let frame = "at knot//org.anti_ad.mc.ipnext.event.ClientEventHandler.onJoinWorld(Unknown Source)";
        // The frame alone isn't fatal (it appears in the stack); but the
        // dispatcher line WITH `Exception` IS — `Exception/Error` words
        // co-occurring with onJoinWorld is the actual failure marker.
        let with_error = format!("Caused by: java.lang.NullPointerException at {frame}");
        assert!(matches!(
            classify_world_join_line(&with_error),
            WorldJoinSignal::Failure { .. }
        ), "Exception-bearing onJoinWorld line must be Failure: `{with_error}`");
    }

    #[test]
    fn world_loaded_milestone_is_success_not_a_crash() {
        // Vanilla 1.20.1 integrated server prints these AFTER spawn-prep.
        for ok in [
            "[Server thread/INFO]: Preparing spawn area: 100%",
            "[Server thread/INFO]: Time elapsed: 4321 ms",
            "[Server thread/INFO]: Loaded the worlds",
        ] {
            assert_eq!(
                classify_world_join_line(ok),
                WorldJoinSignal::Success,
                "world-load milestone must be Success: `{ok}`"
            );
        }
    }

    #[test]
    fn quick_play_missing_world_is_a_probe_wiring_gap_not_a_mod_crash() {
        // The exact dialog text from screenshot 2026-05-21_00:33 when the
        // probe ran `--quickPlaySingleplayer anvil_world_probe` against an
        // instance without any save by that name. MUST surface as Failure
        // with a probe-wiring-gap reason so the caller can route around
        // it (vs. silently waiting out 300s of nothing).
        for line in [
            "Failed to Quick Play",
            "Could not find world with the provided identifier",
            "[Render thread/ERROR]: Failed to Quick Play: Could not find world with the provided identifier",
        ] {
            match classify_world_join_line(line) {
                WorldJoinSignal::Failure { reason, .. } => {
                    assert!(reason.contains("Probe wiring gap"),
                        "must label this as a probe gap, not a mod crash; got `{reason}`");
                }
                other => panic!("`{line}` must classify as Failure (probe gap); got {other:?}"),
            }
        }
    }

    #[test]
    fn benign_world_join_lines_do_not_classify_as_failure() {
        // The Resource-manager-reload line is the WHOLE 80-mod mention
        // from the user's crash log — must not match anything in the
        // failure section (no `Exception`, no kotlin reflection).
        let benign = "[Render thread/INFO]: Reloading ResourceManager: \
                      vanilla, fabric (apoli, appleskin, azurelib, ...)";
        assert_eq!(classify_world_join_line(benign), WorldJoinSignal::None);
        // The bare frame line (no `Exception` / `Error`) is just stack — None.
        let bare_frame = "at knot//org.anti_ad.mc.ipnext.event.ClientEventHandler.onJoinWorld(Unknown Source)";
        assert_eq!(classify_world_join_line(bare_frame), WorldJoinSignal::None);
    }

    #[test]
    fn mods_initialized_milestone_is_post_entrypoint_only() {
        // `Setting user:` is PRE-entrypoint — it must NOT short-circuit to
        // success. Doing so false-passed sprout's onInitialize crash, which
        // the crash report timestamps ~55s LATER. It is inert now.
        assert_eq!(
            classify_smoke_line("[Render thread/INFO]: Setting user: Kimcheee"),
            SmokeSignal::None
        );
        // The genuine post-entrypoint milestones still pass.
        assert_eq!(
            classify_smoke_line("[Render thread/INFO]: Sound engine started"),
            SmokeSignal::Success
        );
        assert_eq!(
            classify_smoke_line(
                "[Render thread/INFO]: OpenAL initialized on device CoreAudio Default"
            ),
            SmokeSignal::Success
        );
    }

    /// Regression for the EXACT sprout false-pass. Verbatim lines from the
    /// Valley of Plenty `crash-2026-05-19_14.41.11-client.txt` and its
    /// `logs/`. BEFORE the fix, the `Setting user:` line returned `Success`
    /// and `smoke_test` killed the JVM ~55s before this entrypoint crash
    /// ever printed — the verifier reported a passing pack that hard-crashed
    /// in real play.
    #[test]
    fn setting_user_then_entrypoint_crash_is_caught_not_passed() {
        // 1. The pre-entrypoint line must be inert (no early success).
        assert_eq!(
            classify_smoke_line(
                "[14:40:16] [Render thread/INFO]: Setting user: Kimcheee"
            ),
            SmokeSignal::None
        );
        // 2. The real sprout entrypoint failure must be a Failure that names
        //    the culprit mod (so the repair loop can act on it).
        match classify_smoke_line(
            "[14:41:11] [main/ERROR]: Could not execute entrypoint stage 'main' \
             due to errors, provided by 'sprout' at \
             'tech.thatgravyboat.sprout.SproutFabric'!",
        ) {
            SmokeSignal::Failure { mod_name, .. } => {
                assert_eq!(mod_name.as_deref(), Some("sprout"));
            }
            other => {
                panic!("sprout entrypoint crash must be a Failure, got {other:?}")
            }
        }
    }

    #[test]
    fn ordinary_log_lines_are_inert() {
        assert_eq!(
            classify_smoke_line("[main/INFO]: Loading 209 mods:"),
            SmokeSignal::None
        );
        assert_eq!(
            classify_smoke_line("[Render thread/INFO]: Create 6.0.8.1 initializing!"),
            SmokeSignal::None
        );
    }

    /// The real launch log is FULL of these benign optional-compat probes.
    /// They contain `ClassNotFoundException`/`NoClassDefFoundError` substrings
    /// but are NOT crashes — must never trip a Failure.
    #[test]
    fn benign_mixin_class_probes_do_not_false_fail() {
        for l in [
            "[main/WARN]: Error loading class: io/vram/frex/base/renderer/context/render/EntityBlockRenderContext (java.lang.ClassNotFoundException: io/vram/frex/...)",
            "[main/WARN]: @Mixin target dev.ftb.mods.ftbchunks.client.gui.LargeMapScreen was not found create.mixins.json",
            "[main/WARN]: @Mixin target mezz.jei.fabric.platform.RenderHelper was not found appleskin.jei.mixins.json",
        ] {
            assert_eq!(
                classify_smoke_line(l),
                SmokeSignal::None,
                "benign probe must be inert: {l}"
            );
        }
        // ...but the genuine fatal `Caused by:` form still fails.
        assert!(matches!(
            classify_smoke_line(
                "Caused by: java.lang.NoClassDefFoundError: com/simibubi/create/foundation/utility/Components"
            ),
            SmokeSignal::Failure { .. }
        ));
    }

    /// The real Valley of Plenty mod-resolution reject (verbatim from its
    /// `.anvil-dump/logs/latest.log`). The parser MUST collapse Fabric's four
    /// misleading per-consumer "Replace" hints into the TWO root-cause repins
    /// (fabric-api, seasons) — and fabric-api must be primary because it
    /// blocks the most consumers and its floor must be the HIGHER 0.92.2
    /// (antique_atlas/surveyor), not yacl's 0.92.0.
    #[test]
    fn valley_resolution_block_aggregates_to_root_cause_repins() {
        let log = r#"
[22:00:08] [main/ERROR]: Incompatible mods found!
net.fabricmc.loader.impl.FormattedException: Some of your mods are incompatible with the game or each other!
A potential solution has been determined, this may resolve your problem:
	 - Replace mod 'YetAnotherConfigLib' (yet_another_config_lib_v3) 3.6.6+1.20.1-fabric with version 3.6.6+1.20.1- or later that is compatible with:
		 - fabric-api 0.91.0+1.20.1
	 - Replace mod 'Fabric Seasons' (seasons) 2.2.1+1.20 with version 2.4.2-BETA or later.
	 - Replace mod 'Surveyor Map Framework' (surveyor) 1.2.1+1.20 with any version that is compatible with:
		 - fabric-api 0.91.0+1.20.1
	 - Replace mod 'Antique Atlas' (antique_atlas) 3.1.2+1.20 with any version that is compatible with:
		 - fabric-api 0.91.0+1.20.1
		 - surveyor, any version
More details:
	 - Mod 'Antique Atlas' (antique_atlas) 3.1.2+1.20 requires version 0.92.2+1.20.1 or later of mod 'Fabric API' (fabric-api), which can't be loaded due to other constraints!
	 - Mod 'Surveyor Map Framework' (surveyor) 1.2.1+1.20 requires version 0.92.2+1.20.1 or later of mod 'Fabric API' (fabric-api), which can't be loaded due to other constraints!
	 - Mod 'YetAnotherConfigLib' (yet_another_config_lib_v3) 3.6.6+1.20.1-fabric requires version 0.92.0+1.20.1 or later of mod 'Fabric API' (fabric-api), which can't be loaded due to other constraints!
	 - Mod 'Fabric Seasons: Delight Refabricated Compat' (seasonsdelightrefabcompat) 1.0-1.20.1-2.2.0+refabricated requires version 2.4.2-BETA or later of mod 'Fabric Seasons' (seasons), but only the wrong version is present: 2.2.1+1.20!
"#;
        let rem = parse_fabric_remediation(log).expect("parses");
        assert_eq!(rem.primary_dep_id.as_deref(), Some("fabric-api"));
        assert_eq!(rem.actions.len(), 2, "two root causes: fabric-api, seasons");

        let api = &rem.actions[0];
        assert_eq!(api.dep_id, "fabric-api");
        assert!(!api.add_missing);
        assert_eq!(
            api.want, ">=0.92.2+1.20.1",
            "must pick the HIGHER floor, not yacl's 0.92.0"
        );
        assert_eq!(
            api.holders,
            vec![
                "antique_atlas".to_string(),
                "surveyor".to_string(),
                "yet_another_config_lib_v3".to_string()
            ]
        );

        let seasons = &rem.actions[1];
        assert_eq!(seasons.dep_id, "seasons");
        assert_eq!(seasons.want, ">=2.4.2-BETA");
        assert_eq!(seasons.holders, vec!["seasonsdelightrefabcompat"]);

        // The curator-facing message names the repins, not a downgrade.
        assert!(rem.summary.contains("repin 'fabric-api' to >=0.92.2+1.20.1"));
        assert!(rem.summary.contains("repin 'seasons' to >=2.4.2-BETA"));
        assert!(rem.summary.contains("do NOT downgrade"));

        // LOAD-BEARING: the `want` string we emit MUST round-trip through the
        // SAME version engine range-aware edit_pack uses, and must ACCEPT a
        // real newer fabric-api (build-metadata `+1.20.1` asymmetry is a known
        // sharp edge in crate::version). If this fails the loop can never find
        // fabric-api and escalates to DELETING it — catastrophic.
        let api_req = crate::version::VersionReq::parse(&rem.actions[0].want)
            .expect("the emitted want must parse as a VersionReq");
        for v in ["0.92.2+1.20.1", "0.92.5+1.20.1", "0.100.1+1.20.1"] {
            let cand = crate::version::Version::parse(v).unwrap();
            assert!(
                crate::version::satisfies(&cand, &api_req),
                "emitted want {:?} must accept real fabric-api {v}",
                rem.actions[0].want
            );
        }
        // ...and must REJECT the too-old one that caused the crash.
        let old = crate::version::Version::parse("0.91.0+1.20.1").unwrap();
        assert!(
            !crate::version::satisfies(&old, &api_req),
            "emitted want must reject the old fabric-api 0.91.0"
        );
        let seasons_req =
            crate::version::VersionReq::parse(&rem.actions[1].want)
                .expect("seasons want must parse");
        assert!(crate::version::satisfies(
            &crate::version::Version::parse("2.4.2-BETA").unwrap(),
            &seasons_req
        ));
    }

    /// A standalone missing-dep reject (no "More details" umbrella) must still
    /// yield an add-dependency action.
    #[test]
    fn standalone_missing_dep_becomes_add_action() {
        let log = " - Mod 'Spectrum' (spectrum) 1.8.13 requires version \
                    1.77.3 or later of mod 'Modonomicon' (modonomicon), \
                    which is missing!";
        let rem = parse_fabric_remediation(log).expect("parses");
        assert_eq!(rem.actions.len(), 1);
        let a = &rem.actions[0];
        assert_eq!(a.dep_id, "modonomicon");
        assert!(a.add_missing);
        assert_eq!(a.want, ">=1.77.3");
        assert_eq!(a.holders, vec!["spectrum"]);
        assert!(rem.summary.contains("add dependency 'modonomicon'"));
    }

    /// REPRO (Harvest Hollow): the failure surfaced in the CLIENT smoke log,
    /// not a Stage-2 server boot — `stage1_core` now runs THIS parser on the
    /// resolution-reject class instead of the LLM analyst (curator.rs #63).
    /// The line is verbatim from the real `<instance>/logs/latest.log`.
    /// Asserts: (1) "Incompatible mods found" is a resolution reject, (2) the
    /// parser names moonlight + the MC-prefixed floor, (3) the emitted `want`
    /// round-trips through the SAME version engine the repin loop uses —
    /// accepting the real available build `1.20-2.16.32-fabric` and rejecting
    /// the stuck pin `1.20-2.13.82`. If (3) fails the repin can never land.
    #[test]
    fn harvest_hollow_client_smoke_reject_parses_deterministically() {
        assert!(super::is_resolution_reject(
            "Fabric rejected the pack: incompatible / missing mods"
        ));
        let log = "[10:58:06] [main/ERROR]: Incompatible mods found!\n\
            A potential solution has been determined, this may resolve your problem:\n\
            \t - Replace mod 'Moonlight' (moonlight) 1.20-2.13.82 with version 1.20-2.16.26 or later.\n\
            More details:\n\
            \t - Mod 'Supplementaries' (supplementaries) 1.20-3.1.43 requires version 1.20-2.16.26 or later of mod 'Moonlight' (moonlight), but only the wrong version is present: 1.20-2.13.82!\n";
        let rem = parse_fabric_remediation(log).expect("parses");
        assert_eq!(rem.primary_dep_id.as_deref(), Some("moonlight"));
        assert_eq!(rem.actions.len(), 1);
        let a = &rem.actions[0];
        assert_eq!(a.dep_id, "moonlight");
        assert!(!a.add_missing, "moonlight is present-but-wrong, a repin");
        assert_eq!(a.want, ">=1.20-2.16.26");
        assert_eq!(a.holders, vec!["supplementaries"]);
        assert!(rem.summary.contains("repin 'moonlight' to >=1.20-2.16.26"));

        let req = crate::version::VersionReq::parse(&a.want)
            .expect("emitted want must parse as a VersionReq");
        for ok in ["1.20-2.16.26-fabric", "1.20-2.16.32-fabric"] {
            assert!(
                crate::version::satisfies(
                    &crate::version::Version::parse(ok).unwrap(),
                    &req
                ),
                "emitted want {:?} must accept the real build {ok}",
                a.want
            );
        }
        assert!(
            !crate::version::satisfies(
                &crate::version::Version::parse("1.20-2.13.82-fabric")
                    .unwrap(),
                &req
            ),
            "emitted want must reject the stuck pin 1.20-2.13.82"
        );
    }
}

// ---------------------------------------------------------------------------
// Slice 1.5 — registry-dump driver tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod dump_tests {
    use super::{
        drive_dump_server, eula_bytes, is_server_ready_line,
        server_properties_bytes, DumpOutcome, Instance, LaunchEvent,
    };

    /// Regression-lock the EXACT server.properties bytes — `level-type=flat`
    /// in particular: a non-flat world would generate terrain and slow / hang
    /// the headless boot the whole pass depends on.
    #[test]
    fn server_properties_literal_is_locked() {
        let got = String::from_utf8(server_properties_bytes()).unwrap();
        assert_eq!(
            got,
            "level-type=flat\n\
             online-mode=false\n\
             max-players=0\n\
             view-distance=2\n\
             simulation-distance=2\n"
        );
        assert!(got.contains("level-type=flat"));
    }

    #[test]
    fn eula_literal_is_locked() {
        assert_eq!(eula_bytes(), b"eula=true\n");
    }

    #[test]
    fn ready_line_needs_both_markers() {
        assert!(is_server_ready_line(
            r#"[12:00:00] [Server thread/INFO]: Done (1.234s)! For help, type "help""#
        ));
        // Either marker alone is NOT ready.
        assert!(!is_server_ready_line("[INFO]: Done (1.0s) loading something"));
        assert!(!is_server_ready_line(r#"type "help" for commands"#));
        assert!(!is_server_ready_line("[INFO]: Preparing spawn area: 12%"));
    }

    /// These integration tests each call `drive_dump_server`, which guards on
    /// the PRODUCTION-global `jvm_lock()` via `try_lock()` (skip-not-queue:
    /// the OOM defense). cargo runs `#[tokio::test]`s in parallel, so without
    /// test-level serialization whichever test loses that `try_lock` race
    /// degrades to `Ok(None)` ("another JVM is running") and breaks the
    /// happy-path `expect(Some(dir))`. This module-local async mutex
    /// serializes them so only one is ever inside `drive_dump_server` at a
    /// time. Not a production lock — purely a parallel-test gate.
    #[cfg(unix)]
    fn dump_test_serial() -> &'static tokio::sync::Mutex<()> {
        static L: std::sync::OnceLock<tokio::sync::Mutex<()>> =
            std::sync::OnceLock::new();
        L.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// Write a `chmod +x` shell-script stub at `path` that fakes the server.
    #[cfg(unix)]
    fn write_stub(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        let mut p = std::fs::metadata(path).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(path, p).unwrap();
    }

    /// INTEGRATION: a stub "java" that prints the ready line, reads stdin until
    /// it sees `stop`, writes `dump/item/minecraft.json`, exits 0. Asserts the
    /// full happy path AND that the captured dir reconciles into a
    /// `DumpReconciled` ScanResult with `minecraft:stone` in `items`. No
    /// network: `drive_dump_server` is called directly with the stub as java.
    #[cfg(unix)]
    #[tokio::test]
    async fn stub_java_full_dump_roundtrip() {
        let _serial = dump_test_serial().lock().await;
        let d = tempfile::tempdir().unwrap();
        let dump_dir = d.path().join(".anvil-dump");
        std::fs::create_dir_all(&dump_dir).unwrap();

        // The stub records stdin so we can assert command ORDER, writes the
        // dump only AFTER it sees `stop` (proving the stop handshake ran).
        let stub = d.path().join("java-stub.sh");
        write_stub(
            &stub,
            r#"#!/bin/sh
echo '[12:00:00] [Server thread/INFO]: Done (1.0s)! For help, type "help"'
while IFS= read -r line; do
  echo "$line" >> "$PWD/stdin.log"
  case "$line" in
    stop) break ;;
  esac
done
mkdir -p "$PWD/dump/item"
printf '%s' '["minecraft:stone"]' > "$PWD/dump/item/minecraft.json"
exit 0
"#,
        );

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<LaunchEvent>();
        tokio::spawn(async move { while rx.recv().await.is_some() {} });

        let out = drive_dump_server(
            &dump_dir,
            stub.to_str().unwrap(),
            std::path::Path::new("unused-launcher.jar"),
            std::time::Duration::from_millis(50), // grace
            std::time::Duration::from_secs(10),   // test-shortened timeout
            false,                                // try_lock (serialized tests)
            None,
            &tx,
        )
        .await
        .expect("driver never errors");

        let got = match out {
            DumpOutcome::Dumped(d) => d,
            o => panic!("expected Dumped, got {o:?}"),
        };
        assert_eq!(got, dump_dir);

        // stdin order: `/dump registry` strictly before `stop`.
        let log =
            std::fs::read_to_string(dump_dir.join("stdin.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.first(), Some(&"/dump registry"));
        assert!(
            lines.iter().any(|l| *l == "stop"),
            "stop must be written, got {lines:?}"
        );
        let di = lines.iter().position(|l| *l == "/dump registry").unwrap();
        let si = lines.iter().position(|l| *l == "stop").unwrap();
        assert!(di < si, "/dump registry must precede stop");

        // The dump parses + reconciles into a DumpReconciled scan.
        let scan = crate::registry::reconcile_scan(
            crate::registry::ScanResult::default(),
            Some(&got),
        );
        assert_eq!(scan.source, crate::registry::ScanSource::DumpReconciled);
        assert!(scan.vocab.items.contains("minecraft:stone"));
    }

    /// INTEGRATION (modded-grounding regression catch): a stub "java" that
    /// dumps BOTH `dump/item/minecraft.json` (vanilla) AND
    /// `dump/item/foo.json` (a modded namespace). Drives the pass then
    /// reconciles a static scan that still lists `foo` as unscanned. Asserts
    /// the modded id lands in `vocab.items`, `foo` is dropped from `unscanned`
    /// (so grounding stops degrading real ids), and the source is stamped
    /// `DumpReconciled`. This is the test that would have caught the original
    /// bug where the dump server booted with no mods and saw vanilla only.
    #[cfg(unix)]
    #[tokio::test]
    async fn stub_java_modded_dump_reconciles_nonvanilla_ids() {
        let _serial = dump_test_serial().lock().await;
        let d = tempfile::tempdir().unwrap();
        let dump_dir = d.path().join(".anvil-dump");
        std::fs::create_dir_all(&dump_dir).unwrap();

        let stub = d.path().join("java-modded.sh");
        write_stub(
            &stub,
            r#"#!/bin/sh
echo '[12:00:00] [Server thread/INFO]: Done (1.0s)! For help, type "help"'
while IFS= read -r line; do
  case "$line" in
    stop) break ;;
  esac
done
mkdir -p "$PWD/dump/item"
printf '%s' '["minecraft:stone"]' > "$PWD/dump/item/minecraft.json"
printf '%s' '["foo:bar"]' > "$PWD/dump/item/foo.json"
exit 0
"#,
        );

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<LaunchEvent>();
        tokio::spawn(async move { while rx.recv().await.is_some() {} });

        let out = drive_dump_server(
            &dump_dir,
            stub.to_str().unwrap(),
            std::path::Path::new("unused-launcher.jar"),
            std::time::Duration::from_millis(50), // grace
            std::time::Duration::from_secs(10),   // test-shortened timeout
            false,                                // try_lock (serialized tests)
            None,
            &tx,
        )
        .await
        .expect("driver never errors");
        let got = match out {
            DumpOutcome::Dumped(d) => d,
            o => panic!("expected Dumped, got {o:?}"),
        };

        // Seed `foo` into `unscanned` so the "removed from unscanned"
        // assertion actually discriminates (default ScanResult is empty, which
        // would make the check trivially pass).
        let mut seed = crate::registry::ScanResult::default();
        seed.unscanned.insert("foo".to_string());
        let scan = crate::registry::reconcile_scan(seed, Some(&got));

        assert_eq!(scan.source, crate::registry::ScanSource::DumpReconciled);
        // Vanilla AND the modded id both grounded from the live dump.
        assert!(scan.vocab.items.contains("minecraft:stone"));
        assert!(
            scan.vocab.items.contains("foo:bar"),
            "modded id must be unioned from the dump, got {:?}",
            scan.vocab.items
        );
        // `foo` was covered by the dump → no longer low-confidence.
        assert!(
            !scan.unscanned.contains("foo"),
            "a namespace the dump covered must leave `unscanned`, got {:?}",
            scan.unscanned
        );
    }

    /// INTEGRATION: a stub that NEVER prints the ready line. Within the
    /// (test-shortened) timeout `drive_dump_server` must degrade to `Ok(None)`
    /// and leave no `dump/` behind (the original cache stays untouched because
    /// the caller only writes back on `Some`).
    #[cfg(unix)]
    #[tokio::test]
    async fn stub_java_never_ready_degrades_to_none() {
        let _serial = dump_test_serial().lock().await;
        let d = tempfile::tempdir().unwrap();
        let dump_dir = d.path().join(".anvil-dump");
        std::fs::create_dir_all(&dump_dir).unwrap();

        let stub = d.path().join("java-hang.sh");
        // Prints inert lines, never the ready marker, then sleeps past the
        // test timeout. The driver must time out and reap it.
        write_stub(
            &stub,
            "#!/bin/sh\n\
             echo '[INFO]: Preparing spawn area: 22%'\n\
             sleep 30\n",
        );

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<LaunchEvent>();
        tokio::spawn(async move { while rx.recv().await.is_some() {} });

        let out = drive_dump_server(
            &dump_dir,
            stub.to_str().unwrap(),
            std::path::Path::new("unused.jar"),
            std::time::Duration::from_millis(50),
            std::time::Duration::from_millis(800), // short timeout
            false,
            None,
            &tx,
        )
        .await
        .expect("driver never errors");

        assert!(
            matches!(out, DumpOutcome::Failed(_)),
            "no ready line + timeout → Failed (tried, no proof), got {out:?}"
        );
        assert!(
            !dump_dir.join("dump").exists(),
            "a failed pass must not leave a dump/ to be parsed as truth"
        );
    }

    /// SURGICAL, EXPLICIT-ONLY one-shot: prove the world-gen gate catches the
    /// REAL Stardew Hollow world-creation crash (villagernames 4.5.1
    /// hard-refs `dev.mrsterner...GuardEntity`; guardvillagers 2.0.9 ships
    /// `dev.sterner...`). Boots the REAL headless dedicated server with the
    /// REAL pinned mod set and asserts `DumpOutcome::Crashed` — i.e. the
    /// pre-quest gate would BLOCK. `#[ignore]` (a real ~2-3 min network+JVM
    /// op); a hard id guard + a broken-pair guard so it can only ever run
    /// against that one intentionally-broken instance. DO NOT generalize.
    ///
    ///   cargo test --lib \
    ///     launch::dump_tests::live_stardew_hollow_world_gen_gate_blocks \
    ///     -- --ignored --exact --nocapture
    #[cfg(unix)]
    #[tokio::test]
    #[ignore]
    async fn live_stardew_hollow_world_gen_gate_blocks() {
        const ID: &str = "18b0bf8e343aa9f8bc78";
        let home = std::env::var("HOME").expect("HOME set");
        let base = std::path::PathBuf::from(&home).join(".anvil/instances");
        let json = base.join(format!("{ID}.json"));
        assert!(
            json.exists(),
            "guard: {ID} must be the broken Stardew Hollow instance"
        );
        let inst: Instance = serde_json::from_str(
            &std::fs::read_to_string(&json).unwrap(),
        )
        .expect("instance json deserializes");
        // Broken-pair guard: only ever run against the intended repro.
        let names = inst
            .mods
            .iter()
            .map(|m| m.name.to_lowercase())
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            names.contains("villagernames")
                && names.contains("guardvillager"),
            "guard: instance must still carry the villagernames + \
             guardvillagers broken pair (revert it first)"
        );

        let mr = crate::modrinth::Modrinth::new();
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<LaunchEvent>();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if let LaunchEvent::Status(s) = ev {
                    eprintln!("[boot] {s}");
                }
            }
        });

        let outcome = super::registry_dump_pass(&inst, &mr, true, tx)
            .await
            .expect("registry_dump_pass never errors (it degrades)");
        eprintln!("VERDICT: {outcome:?}");
        match outcome {
            DumpOutcome::Crashed { mod_name, reason } => {
                eprintln!(
                    "PASS — the gate BLOCKS quest-gen for this pack. \
                     mod_name={mod_name:?}\nreason={reason}"
                );
            }
            other => panic!(
                "the world-gen gate MUST block this pack, got {other:?}"
            ),
        }
        let _ = std::fs::remove_dir_all(base.join(ID).join(".anvil-dump"));
    }
}
