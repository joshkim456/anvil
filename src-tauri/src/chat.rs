//! Persistent curator chat threads. Each thread is one conversation; once the
//! curator assembles a pack the thread is bound to that instance id so it can
//! be reopened from the Instances surface. Stored one JSON file per thread at
//! `~/.anvil/chats/<id>.json`, mirroring `instance.rs`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::curator::ChatMsg;
use crate::settings;

/// Where a thread is in the build pipeline. The curator's system prompt and
/// visible tools are scoped to the phase (smaller prompt, fewer tools, less
/// error surface — the "MCP/CLI feel" without an orchestrator LLM). A Rust
/// state machine advances it off tool events; the frontend persists it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Gathering intent + building/refining the modpack.
    Curating,
    /// A pack has been assembled; refine it or move to quests.
    Assembled,
    /// Designing the questline for the assembled instance. Serializes as
    /// "progression" to match the curator's Phase events, the phase-scoped
    /// prompt/tool selection, and the frontend Phase type. `alias` keeps any
    /// legacy "questing"-tagged thread file readable.
    #[serde(alias = "questing")]
    Progression,
    /// Questline finalized.
    Complete,
}

impl Default for Phase {
    fn default() -> Self {
        Phase::Curating
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatThread {
    pub id: String,
    /// Set once the curator's assemble_pack runs; None for an unbound draft.
    #[serde(default)]
    pub instance_id: Option<String>,
    pub title: String,
    pub created: String,
    pub updated: String,
    #[serde(default)]
    pub phase: Phase,
    #[serde(default)]
    pub messages: Vec<ChatMsg>,
}

/// One mod in a proposed-but-not-yet-assembled candidate pack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateMod {
    pub project_id: String,
    pub version_id: String,
    #[serde(default)]
    pub title: String,
}

/// The last pack `propose_pack` resolved for a thread. Persisted in a
/// backend-only sidecar (NOT in `ChatThread`) so the curator can recover the
/// expensive resolved mod list across the turn boundary / tool-round limit —
/// the model's own context is wiped every turn. Kept out of `ChatThread`
/// deliberately: the frontend owns thread persistence and would clobber a
/// field it does not know about, so a separate file avoids that race.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidatePack {
    pub mc_version: String,
    pub loader: String,
    pub mods: Vec<CandidateMod>,
}

fn chats_dir() -> PathBuf {
    settings::data_dir().join("chats")
}

fn thread_path(id: &str) -> PathBuf {
    chats_dir().join(format!("{id}.json"))
}

fn candidate_path(id: &str) -> PathBuf {
    chats_dir().join(format!("{id}.candidate.json"))
}

/// Persist the last proposed candidate pack for a thread (overwrites any prior
/// one — a fresh proposal replaces the saved set). Best-effort: a write
/// failure must not break the live turn, so the error is swallowed.
pub fn save_candidate(thread_id: &str, c: &CandidatePack) {
    let dir = chats_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(s) = serde_json::to_string_pretty(c) {
        let _ = std::fs::write(candidate_path(thread_id), s);
    }
}

/// The saved candidate pack for a thread, if `propose_pack` ran in it and the
/// set has not yet been consumed by a successful assemble.
pub fn load_candidate(thread_id: &str) -> Option<CandidatePack> {
    std::fs::read_to_string(candidate_path(thread_id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Drop the saved candidate (called once it has been assembled, so a later
/// "make another pack" in the same thread re-proposes fresh).
pub fn clear_candidate(thread_id: &str) {
    let p = candidate_path(thread_id);
    if p.exists() {
        let _ = std::fs::remove_file(p);
    }
}

fn proposed_path(id: &str) -> PathBuf {
    chats_dir().join(format!("{id}.proposed"))
}

/// Mark that `propose_pack` has been ATTEMPTED in this thread (written at the
/// start of propose_pack, before it can fail). The funnel gate uses this so
/// manual search_mods/get_mod is blocked until the curator has gone through
/// propose_pack at least once — which is what guarantees a recoverable
/// candidate exists (killing the "type anything → re-scout from scratch"
/// loop). A legitimate propose_pack miss still flips this, so manual search
/// remains available as the honest fallback.
pub fn mark_proposed(thread_id: &str) {
    let dir = chats_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(proposed_path(thread_id), b"1");
}

/// Whether propose_pack has been attempted in this thread (see `mark_proposed`).
pub fn proposed(thread_id: &str) -> bool {
    proposed_path(thread_id).exists()
}

fn origins_authored_path(id: &str) -> PathBuf {
    chats_dir().join(format!("{id}.origins_authored"))
}

/// Mark that the model successfully authored + validated a custom origin set
/// in this thread (via generate_origins). The curator then SKIPS the
/// deterministic rescue origins emit so the authored set is not clobbered.
///
/// INTENTIONALLY STICKY (set on first success, never cleared): a later
/// generate_origins call that FAILS validation writes nothing, so the last
/// good authored set stays on disk — exactly what we want. Do not "fix" this
/// to clear on failure; that would let the rescue set clobber a valid pack.
pub fn mark_origins_authored(thread_id: &str) {
    let dir = chats_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(origins_authored_path(thread_id), b"1");
}

/// Whether the model authored a valid origin set in this thread.
pub fn origins_authored(thread_id: &str) -> bool {
    origins_authored_path(thread_id).exists()
}

/// All threads, newest activity first.
pub fn load_threads() -> Vec<ChatThread> {
    let dir = chats_dir();
    let mut out: Vec<ChatThread> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|s| serde_json::from_str::<ChatThread>(&s).ok())
            .collect(),
        Err(_) => Vec::new(),
    };
    // Newest `updated` first; lexicographic works for RFC3339 timestamps.
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    out
}

pub fn load_thread(id: &str) -> Option<ChatThread> {
    std::fs::read_to_string(thread_path(id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn save_thread(t: &ChatThread) -> std::io::Result<()> {
    let dir = chats_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        thread_path(&t.id),
        serde_json::to_string_pretty(t).unwrap_or_default(),
    )
}

pub fn delete_thread(id: &str) -> std::io::Result<()> {
    clear_candidate(id);
    let _ = std::fs::remove_file(proposed_path(id));
    let p = thread_path(id);
    if p.exists() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}

/// The most recently active thread bound to `instance_id`, if any.
pub fn thread_for_instance(instance_id: &str) -> Option<ChatThread> {
    load_threads()
        .into_iter()
        .find(|t| t.instance_id.as_deref() == Some(instance_id))
}

/// Delete every thread bound to `instance_id` (and its candidate sidecar).
/// Called when the instance itself is deleted so a chat does not outlive the
/// pack it built. Returns how many threads were removed.
pub fn delete_threads_for_instance(instance_id: &str) -> usize {
    load_threads()
        .into_iter()
        .filter(|t| t.instance_id.as_deref() == Some(instance_id))
        .filter(|t| delete_thread(&t.id).is_ok())
        .count()
}
