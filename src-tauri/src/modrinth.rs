//! Minimal async Modrinth API v2 client.
//!
//! Modrinth asks every third-party client to send a descriptive, contactable
//! `User-Agent`. Missing it gets you silently rate-limited or blocked, so it is
//! baked into the client constructor and is not optional. Read endpoints need
//! no auth. Public rate limit is 300 req/min per IP.

use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.modrinth.com/v2";
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

#[derive(Clone)]
pub struct Modrinth {
    http: reqwest::Client,
}

impl Modrinth {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client builds with a static user agent");
        Self { http }
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T> {
        let resp = self.http.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ModrinthError::Status {
                status: status.as_u16(),
                url: url.to_string(),
            });
        }
        Ok(resp.json::<T>().await?)
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
        self.get_json(&format!("{API_BASE}/project/{project_id}/version"))
            .await
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
