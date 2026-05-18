//! Tiny dependency-free on-disk cache (JSON files under `~/.anvil/cache`).
//! Used to make Browse fast and resilient when Modrinth is slow/offline.
//! Deliberately not SQLite: avoids a new crate; the access pattern is a simple
//! keyed blob with a TTL.

use crate::settings;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_dir() -> std::path::PathBuf {
    settings::data_dir().join("cache")
}

fn path_for(key: &str) -> std::path::PathBuf {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    cache_dir().join(format!("{:016x}.json", h.finish()))
}

/// Return the cached value if present and younger than `ttl_secs`.
pub fn get(key: &str, ttl_secs: u64) -> Option<String> {
    let txt = std::fs::read_to_string(path_for(key)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    let ts = v.get("ts")?.as_u64()?;
    if now().saturating_sub(ts) > ttl_secs {
        return None;
    }
    Some(v.get("value")?.as_str()?.to_string())
}

/// Store a value under `key` (best-effort; cache failures are non-fatal).
pub fn put(key: &str, value: &str) {
    let dir = cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let blob = serde_json::json!({ "ts": now(), "value": value });
    let _ = std::fs::write(path_for(key), blob.to_string());
}

/// Last-resort stale read (any age) for offline/degraded mode.
pub fn get_stale(key: &str) -> Option<String> {
    get(key, u64::MAX)
}
