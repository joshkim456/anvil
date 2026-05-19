//! Minimal async Modrinth API v2 client.
//!
//! Modrinth asks every third-party client to send a descriptive, contactable
//! `User-Agent`. Missing it gets you silently rate-limited or blocked, so it is
//! baked into the client constructor and is not optional. Read endpoints need
//! no auth. Public rate limit is 300 req/min per IP.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const API_BASE: &str = "https://api.modrinth.com/v2";
/// versions() of a big project (fabric-api has hundreds) is the call that
/// dominates a repair loop — every edit_pack re-resolves the closure. Cache
/// it per session so the loop fetches it ONCE, not per attempt.
const VERSIONS_TTL: Duration = Duration::from_secs(300);
const USER_AGENT: &str = concat!(
    "Anvil/",
    env!("CARGO_PKG_VERSION"),
    " (github.com/joshkim456 / joshkim2028@gmail.com)"
);

#[derive(Debug, thiserror::Error)]
pub enum ModrinthError {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("modrinth returned {status} for {url}")]
    Status { status: u16, url: String },
}

pub type Result<T> = std::result::Result<T, ModrinthError>;

type VersionsCache = Arc<Mutex<HashMap<String, (Instant, Vec<Version>)>>>;

#[derive(Clone)]
pub struct Modrinth {
    http: reqwest::Client,
    /// Shared across every `.clone()` (Arc) so the repair loop's repeated
    /// `versions()` calls for the same project (esp. fabric-api) hit cache.
    vers_cache: VersionsCache,
}

/// A transport-level failure (connect refused, request/body timeout, dropped
/// connection mid-decode) — distinct from an HTTP status. These are what kill
/// `versions(fabric-api)` (huge payload) with `os error 60`; the old client
/// only retried HTTP status, so every edit_pack dead-ended here.
fn is_transient_transport(e: &reqwest::Error) -> bool {
    e.is_timeout()
        || e.is_connect()
        || e.is_request()
        || e.is_body()
        || e.is_decode()
}

/// Pure, unit-tested backoff schedule. Transport errors retry FAST (1,2,4s,
/// max 3 — the server isn't asking us to wait, the pipe broke). HTTP 429/5xx
/// retry SLOWER and more (1,2,4,8,16s, max 5 — honour any `Retry-After`).
fn transport_wait(attempt: u32) -> Option<u64> {
    (attempt < 3).then(|| 1u64 << attempt)
}
fn status_wait(attempt: u32, retry_after: Option<u64>) -> Option<u64> {
    if attempt >= 5 {
        return None;
    }
    Some(match retry_after.filter(|s| *s <= 60) {
        Some(s) => s,
        None => 1u64 << attempt,
    })
}

impl Modrinth {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            // A hung fetch (no upstream timeout) used to sit in OS-level
            // retry for minutes before `os error 60`, dead-ending the repair
            // loop. Bound every request so a slow Modrinth fails FAST and the
            // transport-retry below kicks in instead of hanging.
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client builds with a static user agent");
        Self {
            http,
            vers_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T> {
        // Two independent retry budgets: transport (the broken-pipe / timeout
        // class that dead-ended edit_pack) and HTTP status (429/5xx burst from
        // the repair loop re-resolving the closure). Absorbed here so the
        // curator never sees a "transient resolver issue" to narrate around.
        let mut t_attempt = 0u32;
        let mut s_attempt = 0u32;
        loop {
            let resp = match self.http.get(url).send().await {
                Ok(r) => r,
                Err(e) => {
                    if is_transient_transport(&e) {
                        if let Some(w) = transport_wait(t_attempt) {
                            t_attempt += 1;
                            tokio::time::sleep(Duration::from_secs(w)).await;
                            continue;
                        }
                    }
                    return Err(e.into());
                }
            };
            let status = resp.status();
            if status.is_success() {
                match resp.json::<T>().await {
                    Ok(v) => return Ok(v),
                    Err(e) => {
                        // Body cut off mid-stream (dropped connection) — same
                        // transient class, retry on the transport budget.
                        if is_transient_transport(&e) {
                            if let Some(w) = transport_wait(t_attempt) {
                                t_attempt += 1;
                                tokio::time::sleep(Duration::from_secs(w))
                                    .await;
                                continue;
                            }
                        }
                        return Err(e.into());
                    }
                }
            }
            let code = status.as_u16();
            if !(code == 429 || code == 502 || code == 503 || code == 504) {
                return Err(ModrinthError::Status {
                    status: code,
                    url: url.to_string(),
                });
            }
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok());
            match status_wait(s_attempt, retry_after) {
                Some(w) => {
                    s_attempt += 1;
                    tokio::time::sleep(Duration::from_secs(w)).await;
                }
                None => {
                    return Err(ModrinthError::Status {
                        status: code,
                        url: url.to_string(),
                    })
                }
            }
        }
    }

    /// Search projects. `facets` is Modrinth's facet syntax already encoded by
    /// the caller (e.g. `[["project_type:mod"],["versions:1.21.1"]]`).
    pub async fn search(
        &self,
        query: &str,
        facets: Option<&str>,
        index: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResponse> {
        let mut url = format!(
            "{API_BASE}/search?query={}&index={}&limit={}&offset={}",
            urlencoding(query),
            index,
            limit,
            offset
        );
        if let Some(f) = facets {
            url.push_str(&format!("&facets={}", urlencoding(f)));
        }
        self.get_json(&url).await
    }

    pub async fn project(&self, id_or_slug: &str) -> Result<Project> {
        self.get_json(&format!("{API_BASE}/project/{id_or_slug}"))
            .await
    }

    pub async fn versions(&self, project_id: &str) -> Result<Vec<Version>> {
        if let Some((t, v)) = self.vers_cache.lock().await.get(project_id) {
            if t.elapsed() < VERSIONS_TTL {
                return Ok(v.clone());
            }
        }
        let v: Vec<Version> = self
            .get_json(&format!("{API_BASE}/project/{project_id}/version"))
            .await?;
        self.vers_cache
            .lock()
            .await
            .insert(project_id.to_string(), (Instant::now(), v.clone()));
        Ok(v)
    }
}

#[cfg(test)]
mod retry_tests {
    use super::{status_wait, transport_wait};

    #[test]
    fn transport_budget_is_short_and_bounded() {
        // 1,2,4s then give up — a broken pipe won't heal by waiting longer.
        assert_eq!(transport_wait(0), Some(1));
        assert_eq!(transport_wait(1), Some(2));
        assert_eq!(transport_wait(2), Some(4));
        assert_eq!(transport_wait(3), None);
    }

    #[test]
    fn status_budget_honours_retry_after_then_caps() {
        // Exponential when no Retry-After.
        assert_eq!(status_wait(0, None), Some(1));
        assert_eq!(status_wait(4, None), Some(16));
        assert_eq!(status_wait(5, None), None); // bounded at 5
                                                // Server-sent Retry-After wins (<=60s sanity cap).
        assert_eq!(status_wait(0, Some(7)), Some(7));
        assert_eq!(status_wait(0, Some(9999)), Some(1)); // absurd → ignore
    }
}

impl Default for Modrinth {
    fn default() -> Self {
        Self::new()
    }
}

/// Tiny percent-encoder for query values (avoids pulling a URL crate for the
/// handful of characters that actually appear in mod queries/facets).
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---- Response types (only the fields the app actually uses) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub offset: u32,
    pub limit: u32,
    pub total_hits: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub categories: Vec<String>,
    pub client_side: String,
    pub server_side: String,
    pub project_type: String,
    pub downloads: u64,
    #[serde(default)]
    pub follows: u64,
    pub icon_url: Option<String>,
    pub author: String,
    #[serde(default)]
    pub versions: Vec<String>,
    #[serde(default)]
    pub display_categories: Vec<String>,
    pub license: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub body: String,
    #[serde(default)]
    pub categories: Vec<String>,
    pub client_side: String,
    pub server_side: String,
    pub project_type: String,
    pub downloads: u64,
    #[serde(default)]
    pub followers: u64,
    pub icon_url: Option<String>,
    #[serde(default)]
    pub gallery: Vec<GalleryItem>,
    pub license: License,
    pub source_url: Option<String>,
    pub issues_url: Option<String>,
    pub wiki_url: Option<String>,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GalleryItem {
    pub url: String,
    #[serde(default)]
    pub featured: bool,
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct License {
    pub id: String,
    pub name: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    pub version_type: String,
    #[serde(default)]
    pub loaders: Vec<String>,
    pub downloads: u64,
    pub date_published: String,
    #[serde(default)]
    pub files: Vec<VersionFile>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionFile {
    pub hashes: FileHashes,
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHashes {
    pub sha1: String,
    pub sha512: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    pub version_id: Option<String>,
    pub project_id: Option<String>,
    /// "required" | "optional" | "incompatible" | "embedded"
    pub dependency_type: String,
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Hits the real Modrinth API. Run explicitly:
    /// `cargo test --lib live -- --ignored --nocapture`
    /// Proves the client (incl. the User-Agent) + serde types match the live
    /// v2 schema for the exact calls the Browse surface makes.
    #[tokio::test]
    #[ignore]
    async fn live_search_project_and_versions() {
        let mr = Modrinth::new();

        let facets = "[[\"project_type:mod\"],[\"versions:1.21.1\"]]";
        let res = mr
            .search("sodium", Some(facets), "relevance", 5, 0)
            .await
            .expect("search deserializes");
        assert!(res.total_hits > 0, "expected sodium results");
        assert!(!res.hits.is_empty());
        let hit = &res.hits[0];
        println!("top hit: {} ({})", hit.title, hit.slug);

        let project = mr.project(&hit.slug).await.expect("project deserializes");
        assert_eq!(project.project_type, "mod");

        let versions = mr.versions(&project.id).await.expect("versions deserialize");
        assert!(!versions.is_empty(), "expected at least one version");
        assert!(
            versions[0].files.iter().any(|f| !f.hashes.sha512.is_empty()),
            "version files must carry sha512 (needed for .mrpack)"
        );
        println!(
            "{}: {} versions, newest {}",
            project.title,
            versions.len(),
            versions[0].version_number
        );
    }
}
