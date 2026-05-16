//! Instance data model + persistence.
//!
//! Instances are version-pinned snapshots: the mod set is frozen at creation
//! and only changes via an explicit, diffed "check for updates" action — never
//! silently (silent in-place mod updates corrupt existing worlds; spec L1).

use crate::settings;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedMod {
    pub project_id: String,
    pub version_id: String,
    pub name: String,
    /// Path under the instance, e.g. `mods/sodium.jar`.
    pub path: String,
    pub sha1: String,
    pub sha512: String,
    pub download_url: String,
    pub file_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub mc_version: String,
    /// "vanilla" | "fabric" | "forge" | "neoforge" | "quilt"
    pub loader: String,
    pub loader_version: String,
    pub created: String,
    pub last_played: Option<String>,
    /// Frozen snapshot. Mutated only through a reviewed update diff.
    pub mods: Vec<PinnedMod>,
}

pub fn instance_dir(id: &str) -> PathBuf {
    settings::instances_dir().join(id)
}

pub fn save_instance(inst: &Instance) -> std::io::Result<()> {
    let dir = settings::instances_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(format!("{}.json", inst.id)),
        serde_json::to_string_pretty(inst).unwrap_or_default(),
    )
}

pub fn load_instances() -> Vec<Instance> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(settings::instances_dir()) {
        for e in entries.flatten() {
            if e.path().extension().and_then(|s| s.to_str()) == Some("json") {
                if let Ok(txt) = std::fs::read_to_string(e.path()) {
                    if let Ok(i) = serde_json::from_str::<Instance>(&txt) {
                        out.push(i);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}
