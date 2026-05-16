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
//! Java: use a `java` on PATH (or an explicit path); we only verify it runs.
//! Full JRE auto-provisioning is a documented follow-up, not in scope here.
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
#[serde(tag = "kind", rename_all = "snake_case")]
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

    // Resolve + sanity-check Java first so we fail fast before any downloads.
    let java = java_path.unwrap_or_else(|| "java".to_string());
    check_java(&java).await?;

    let prepared = prepare_inner(instance, &tx).await?;

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
// Loader gate
// ---------------------------------------------------------------------------

/// Forge/NeoForge require running their installer's transforming processor
/// pipeline (patched client jars, access transformers, library remapping) —
/// a substantial separate milestone. Bail early, before any network work.
///
/// TODO (follow-up): implement Forge/NeoForge installer execution
/// (download installer jar, run its `install_profile.json` processors with
/// the bundled JDK, assemble the patched classpath).
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
        "fabric" => Some(
            fetch_fabric_profile(&client, FABRIC_META, &instance.mc_version, &instance.loader_version)
                .await
                .context("fetching Fabric loader profile")?,
        ),
        "quilt" => Some(
            fetch_fabric_profile(&client, QUILT_META, &instance.mc_version, &instance.loader_version)
                .await
                .context("fetching Quilt loader profile")?,
        ),
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
    for lib in &lib_libs {
        if !rules_allow(lib.get("rules")) {
            continue;
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

    // Client jar last on the classpath (vanilla). For Fabric, vanilla classes
    // still come from here; Fabric's loader (already earlier) wraps them.
    classpath.push(client_jar.clone());

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
    })
}

// ---------------------------------------------------------------------------
// Java
// ---------------------------------------------------------------------------

/// Run `java -version` (prints to stderr). We don't parse the version string —
/// distros format it differently and the user explicitly chose this path;
/// gating wrongly is worse than not gating. We only confirm it executes.
///
/// TODO (follow-up): bundle/auto-provision an Adoptium JRE 17/21 so users
/// don't need a system JDK; for now we require one on PATH.
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

// Note: 1.19+ ships per-arch native classifiers as separate rule-gated
// library entries (e.g. `org.lwjgl:lwjgl:3.3:natives-macos-arm64`) that flow
// through the normal `downloads.artifact` path onto the classpath, so we do
// not need to derive an `arm64`/`x64` classifier token ourselves. The legacy
// `${arch}` substitution (1.18-) is handled by `arch_bits()` above.
//
// TODO (follow-up): if a future version reintroduces `${os.arch}`-style
// classifier templating, add an explicit `arm64`/`x64`/`x86` resolver here.

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
async fn ensure_mod(
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

