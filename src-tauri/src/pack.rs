//! Pack engine: dependency-closure resolution, `validate_pack`, and standard
//! `.mrpack` emission. Pure logic with no network or filesystem coupling in the
//! hot path (the resolver takes a dependency-lookup closure so it is unit
//! testable offline) — this is the module that must be `cargo test` green.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
        if !m.loaders.iter().any(|l| l == loader) {
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

/// Resolve the transitive required-dependency closure. Generic over a lookup
/// closure `fetch` (`project_id -> [(dep_project_id, dependency_type)]`) so it
/// is testable without hitting the network. Only `required` deps are pulled;
/// `optional`/`embedded`/`incompatible` are surfaced to the caller elsewhere.
pub fn resolve_closure<F>(roots: &[String], mut fetch: F) -> Vec<String>
where
    F: FnMut(&str) -> Vec<(String, String)>,
{
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<String> = roots.to_vec();
    while let Some(pid) = stack.pop() {
        if out.contains(&pid) {
            continue;
        }
        out.push(pid.clone());
        for (dep, kind) in fetch(&pid) {
            if kind == "required" && !out.contains(&dep) {
                stack.push(dep);
            }
        }
    }
    out.sort();
    out
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

    #[test]
    fn resolve_closure_pulls_required_transitively_and_dedups() {
        // create -> flywheel(required), jei(optional); flywheel -> (none)
        let closure = resolve_closure(&["create".to_string()], |pid| match pid {
            "create" => vec![
                ("flywheel".to_string(), "required".to_string()),
                ("jei".to_string(), "optional".to_string()),
            ],
            _ => vec![],
        });
        assert!(closure.contains(&"create".to_string()));
        assert!(closure.contains(&"flywheel".to_string()));
        assert!(!closure.contains(&"jei".to_string()));
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
