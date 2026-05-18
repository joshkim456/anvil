//! Item-icon extraction for the in-app recipe-grid preview.
//!
//! Resolves a Minecraft item id to a PNG by walking the model -> texture
//! graph inside the pinned mod jars Anvil already has on disk. Pragmatic, not
//! a full Minecraft model engine: it handles the overwhelmingly common cases
//! (`item/generated` / `item/handheld` layered items, and simple block-item
//! models) with a depth-3 parent cap. Anything genuinely 3D / `builtin/*` /
//! cross-mod / vanilla-only resolves to `None`, and the UI falls back to a
//! labeled slot — never a broken image.
//!
//! Vanilla (`minecraft:`) resolves ONLY from an already-downloaded shared
//! assets dir (a prior launch). We never bundle or fetch Mojang textures
//! (their EULA forbids redistribution); absent assets -> `None`.
//!
//! Determinism: the parent walk is child-wins, the block-texture choice has a
//! fixed preference order, and the on-disk cache key is stable, so the same
//! item id always yields the same PNG.

use std::collections::HashMap;
use std::io::{Read, Seek};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::{Map, Value};
use zip::ZipArchive;

use crate::instance::{instance_dir, load_instances};
use crate::settings;

// 6 (was 3): a 3D item's `elements` often live in a block model two or more
// `parent` hops up; the deeper cap reaches it. Self-referential chains still
// terminate at the cap.
const MAX_PARENT_DEPTH: usize = 6;

/// `(namespace, path)`; an unqualified id is `minecraft:` (Minecraft model
/// JSON routinely omits the namespace on parents and texture refs).
fn split_ns(id: &str) -> (String, String) {
    match id.split_once(':') {
        Some((ns, rest)) => (ns.to_string(), rest.to_string()),
        None => ("minecraft".to_string(), id.to_string()),
    }
}

/// Builtin model parents terminate the chain — there is no file to open and
/// the textures come from the most-specific child that defined them.
fn is_builtin_model(ns: &str, path: &str) -> bool {
    ns == "minecraft"
        && (path == "item/generated"
            || path == "item/handheld"
            || path.starts_with("builtin/"))
}

/// Deterministic block-model texture choice. A block model's `textures` is a
/// JSON object; iterating it by hash order would return a different texture
/// across runs and thrash the cache. Fixed preference, then the lowest-sorted
/// non-`particle` key.
fn pick_block_texture(textures: &Map<String, Value>) -> Option<String> {
    for k in ["all", "side", "front", "up", "north", "texture"] {
        if let Some(s) = textures.get(k).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    let mut keys: Vec<&String> =
        textures.keys().filter(|k| k.as_str() != "particle").collect();
    keys.sort();
    keys.first()
        .and_then(|k| textures[k.as_str()].as_str())
        .map(str::to_string)
}

/// Resolve a `#variable` texture ref against the merged texture map (a few
/// hops; Minecraft never nests these deeply).
fn deref_var(mut r: String, merged: &Map<String, Value>) -> Option<String> {
    for _ in 0..4 {
        if let Some(name) = r.strip_prefix('#') {
            r = merged.get(name).and_then(Value::as_str)?.to_string();
        } else {
            return Some(r);
        }
    }
    None
}

fn read_text<R: Read + Seek>(z: &mut ZipArchive<R>, path: &str) -> Option<String> {
    let mut e = z.by_name(path).ok()?;
    let mut s = String::new();
    e.read_to_string(&mut s).ok()?;
    Some(s)
}

fn read_bytes<R: Read + Seek>(z: &mut ZipArchive<R>, path: &str) -> Option<Vec<u8>> {
    let mut e = z.by_name(path).ok()?;
    let mut v = Vec::new();
    e.read_to_end(&mut v).ok()?;
    Some(v)
}

/// PURE inner resolver: from one opened jar whose mod namespace is `jar_ns`,
/// resolve item `name` to PNG bytes. Only assets that physically live in THIS
/// jar resolve; a texture that points at another namespace (vanilla / another
/// mod) returns `None`. Deterministic and unit-testable in isolation.
pub(crate) fn resolve_in_jar<R: Read + Seek>(
    z: &mut ZipArchive<R>,
    jar_ns: &str,
    name: &str,
) -> Option<Vec<u8>> {
    let mut merged: Map<String, Value> = Map::new();
    let mut elements: Vec<Value> = Vec::new();
    let mut display_gui: Option<Value> = None;
    let mut cur_ns = jar_ns.to_string();
    let mut cur_path = format!("item/{name}");
    let mut layered = false;
    let mut builtin_entity = false;

    // Single flatten pass over item -> ... -> block parent chain (same jar).
    // textures: child-wins. elements: the first model that defines them wins
    // (a model's own elements override its parent's; usually inherited from a
    // block parent). display.gui: the first model that defines it wins, used
    // WHOLE (no deep-merge with the block default — that would ship
    // slightly-too-big icons for partial custom item displays).
    for _ in 0..=MAX_PARENT_DEPTH {
        if cur_ns != jar_ns {
            break; // a parent in another jar — not openable here.
        }
        let model = read_text(z, &format!("assets/{cur_ns}/models/{cur_path}.json"))?;
        let j: Value = serde_json::from_str(&model).ok()?;
        if let Some(t) = j.get("textures").and_then(Value::as_object) {
            for (k, v) in t {
                merged.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        if elements.is_empty() {
            if let Some(e) = j.get("elements").and_then(Value::as_array) {
                if !e.is_empty() {
                    elements = e.clone();
                }
            }
        }
        if display_gui.is_none() {
            if let Some(g) = j.get("display").and_then(|d| d.get("gui")) {
                display_gui = Some(g.clone());
            }
        }
        match j.get("parent").and_then(Value::as_str) {
            None => break,
            Some(p) => {
                let (pns, ppath) = split_ns(p);
                if is_builtin_model(&pns, &ppath) {
                    builtin_entity = ppath == "builtin/entity";
                    layered = ppath == "item/generated"
                        || ppath == "item/handheld"
                        || ppath == "builtin/generated";
                    break;
                }
                // Any other parent (incl. a block model): keep walking it so
                // its `elements`/`textures`/`display` are inherited.
                cur_ns = pns;
                cur_path = ppath;
            }
        }
    }

    // (1) A 3D model is present -> render it. Collect the UNIQUE textures it
    // could reference (each `textures` key, deref'd, that lives in this jar),
    // capped at 32 (guards a pathological item from spiking memory), then
    // hand off to the software rasterizer. ANY failure (no textures, parse,
    // nothing drawn) falls through to the flat path below, so the 3D branch
    // never makes a currently-working icon worse.
    if !elements.is_empty() {
        let mut tex_png: HashMap<String, Vec<u8>> = HashMap::new();
        let mut keys: Vec<&String> = merged.keys().collect();
        keys.sort();
        for k in keys.into_iter().take(32) {
            let Some(v) = merged.get(k).and_then(Value::as_str) else {
                continue;
            };
            let Some(r) = deref_var(v.to_string(), &merged) else {
                continue;
            };
            let (tns, tpath) = split_ns(&r);
            if tns == jar_ns {
                if let Some(b) = read_bytes(
                    z,
                    &format!("assets/{tns}/textures/{tpath}.png"),
                ) {
                    tex_png.insert(k.clone(), b);
                }
            }
        }
        if !tex_png.is_empty() {
            if let Some(png) = crate::model3d::render_item_icon(
                &elements,
                &merged,
                display_gui.as_ref(),
                &tex_png,
                64,
            ) {
                return Some(png);
            }
        }
        // else: fall through to the flat / representative-texture path.
    }

    // (2) builtin/entity has no data-driven model and no flat sprite ->
    // labeled slot (by design; we do not reimplement entity renderers).
    if builtin_entity {
        return None;
    }

    // (3) Flat sprite (`layer0`), or for a textured-but-unrenderable model
    // the representative texture. Behaviour unchanged from before.
    let _ = layered;
    let tex_ref = merged
        .get("layer0")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| pick_block_texture(&merged))?;

    let tex_ref = deref_var(tex_ref, &merged)?;
    let (tns, tpath) = split_ns(&tex_ref);
    if tns != jar_ns {
        return None; // texture lives elsewhere (vanilla / another mod).
    }
    read_bytes(z, &format!("assets/{tns}/textures/{tpath}.png"))
}

/// Vanilla item/block icon from THIS instance's pinned version client jar
/// (`versions/<v>/<v>.jar` -> `assets/minecraft/...`). Mojang ships item and
/// block textures *inside* that jar; the hashed `assets/objects` store holds
/// only sounds/lang/etc (it has no `minecraft/textures/item/*` entries at
/// all), so the icon must be pulled from the jar — through the very same
/// model->texture walker the modded path uses, with the `minecraft`
/// namespace (vanilla model refs are unqualified, which `split_ns` already
/// maps to `minecraft`). `None` if the jar is absent (game/version never
/// downloaded) or the item has no flat sprite / drawable model. We still
/// never fetch or bundle Mojang assets — the launcher already put the jar
/// there.
fn vanilla_icon(mc_version: &str, name: &str) -> Option<Vec<u8>> {
    let jar = settings::shared_mc_dir()
        .join("versions")
        .join(mc_version)
        .join(format!("{mc_version}.jar"));
    let f = std::fs::File::open(&jar).ok()?;
    let mut z = ZipArchive::new(f).ok()?;
    resolve_in_jar(&mut z, "minecraft", name)
}

/// Outer resolver: `item_id` -> a `data:image/png;base64,...` URL, or `None`
/// (the UI then renders a labeled slot). Disk-cached so the (heavier) jar
/// walk + 3D render runs once per distinct item, not on every drawer open.
/// Vanilla resolves from this instance's pinned version client jar and is
/// cached shared per MC version (`icon-cache/_vanilla/<mc_version>/`, since
/// `minecraft:*` art is identical across every instance on that version);
/// modded scans the instance's pinned jars (filename != asset namespace for
/// some mods, so we try each jar rather than guess), stops at the first hit,
/// and is cached per instance (`icon-cache/<instance>/`).
pub fn item_icon_data_url(instance_id: &str, item_id: &str) -> Option<String> {
    let (ns, name) = split_ns(item_id);
    let inst = load_instances()
        .into_iter()
        .find(|i| i.id == instance_id)?;

    let cache = if ns == "minecraft" {
        settings::data_dir()
            .join("icon-cache")
            .join("_vanilla")
            .join(&inst.mc_version)
    } else {
        settings::data_dir().join("icon-cache").join(instance_id)
    };
    let cache_file =
        cache.join(format!("{ns}__{}.png", name.replace('/', "_")));

    let bytes = if let Ok(b) = std::fs::read(&cache_file) {
        b
    } else {
        let resolved = if ns == "minecraft" {
            vanilla_icon(&inst.mc_version, &name)
        } else {
            let dir = instance_dir(instance_id);
            let mut found: Option<Vec<u8>> = None;
            for m in &inst.mods {
                let Ok(f) = std::fs::File::open(dir.join(&m.path)) else {
                    continue;
                };
                let Ok(mut z) = ZipArchive::new(f) else { continue };
                if let Some(b) = resolve_in_jar(&mut z, &ns, &name) {
                    found = Some(b);
                    break;
                }
            }
            found
        }?;
        let _ = std::fs::create_dir_all(&cache);
        let _ = std::fs::write(&cache_file, &resolved);
        resolved
    };
    Some(format!("data:image/png;base64,{}", B64.encode(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    /// Build an in-memory jar from (path, bytes) entries (Stored, no
    /// compression feature needed), for branch-coverage unit tests.
    fn jar(entries: &[(&str, &[u8])]) -> ZipArchive<Cursor<Vec<u8>>> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = ZipWriter::new(&mut buf);
            let opt = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (p, b) in entries {
                w.start_file(*p, opt).unwrap();
                w.write_all(b).unwrap();
            }
            w.finish().unwrap();
        }
        ZipArchive::new(buf).unwrap()
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-fake-but-signed";

    #[test]
    fn item_generated_layer0_resolves() {
        let mut z = jar(&[
            (
                "assets/foo/models/item/widget.json",
                br#"{"parent":"item/generated","textures":{"layer0":"foo:item/widget"}}"#,
            ),
            ("assets/foo/textures/item/widget.png", PNG),
        ]);
        assert_eq!(resolve_in_jar(&mut z, "foo", "widget").as_deref(), Some(PNG));
    }

    #[test]
    fn implicit_minecraft_namespace_on_parent_and_handheld() {
        // parent unqualified (= minecraft:item/handheld), layer0 in child.
        let mut z = jar(&[
            (
                "assets/foo/models/item/sword.json",
                br#"{"parent":"item/handheld","textures":{"layer0":"foo:item/sword"}}"#,
            ),
            ("assets/foo/textures/item/sword.png", PNG),
        ]);
        assert_eq!(resolve_in_jar(&mut z, "foo", "sword").as_deref(), Some(PNG));
    }

    #[test]
    fn block_item_uses_deterministic_texture_pick() {
        // item -> block model; block model has all+side+particle; `all` wins.
        let mut z = jar(&[
            (
                "assets/foo/models/item/slab.json",
                br#"{"parent":"foo:block/slab"}"#,
            ),
            (
                "assets/foo/models/block/slab.json",
                br#"{"textures":{"particle":"foo:block/p","side":"foo:block/s","all":"foo:block/a"}}"#,
            ),
            ("assets/foo/textures/block/a.png", PNG),
        ]);
        assert_eq!(resolve_in_jar(&mut z, "foo", "slab").as_deref(), Some(PNG));
    }

    #[test]
    fn custom_3d_item_model_falls_back_to_primary_texture() {
        // A modeled item with NO layer0 (Blockbench-style numeric keys). We
        // can't render 3D geometry, but rather than show nothing we surface
        // the model's primary texture (particle excluded, deterministic).
        let mut z = jar(&[
            (
                "assets/foo/models/item/rocket.json",
                br#"{"textures":{"0":"foo:item/rocket_body","particle":"foo:item/spark"}}"#,
            ),
            ("assets/foo/textures/item/rocket_body.png", PNG),
        ]);
        assert_eq!(
            resolve_in_jar(&mut z, "foo", "rocket").as_deref(),
            Some(PNG),
            "no-layer0 modeled item resolves to its primary texture"
        );

        // Item -> block model with no layer0: the block's `all` is used.
        let mut z2 = jar(&[
            (
                "assets/foo/models/item/casing.json",
                br#"{"parent":"foo:block/casing"}"#,
            ),
            (
                "assets/foo/models/block/casing.json",
                br#"{"textures":{"all":"foo:block/casing","particle":"foo:block/p"}}"#,
            ),
            ("assets/foo/textures/block/casing.png", PNG),
        ]);
        assert_eq!(
            resolve_in_jar(&mut z2, "foo", "casing").as_deref(),
            Some(PNG)
        );
    }

    #[test]
    fn cross_namespace_texture_is_none() {
        // layer0 points at vanilla — not in this jar -> None (labeled slot).
        let mut z = jar(&[(
            "assets/foo/models/item/gem.json",
            br#"{"parent":"item/generated","textures":{"layer0":"minecraft:item/diamond"}}"#,
        )]);
        assert_eq!(resolve_in_jar(&mut z, "foo", "gem"), None);
    }

    #[test]
    fn missing_or_builtin_entity_is_none() {
        let mut z = jar(&[(
            "assets/foo/models/item/chest.json",
            br#"{"parent":"builtin/entity"}"#,
        )]);
        assert_eq!(resolve_in_jar(&mut z, "foo", "chest"), None);
        assert_eq!(resolve_in_jar(&mut z, "foo", "does_not_exist"), None);
    }

    #[test]
    fn parent_depth_cap_is_respected() {
        // 5-deep self-referential parent chain must not hang / must bail None.
        let mut z = jar(&[(
            "assets/foo/models/item/loop.json",
            br#"{"parent":"foo:item/loop"}"#,
        )]);
        assert_eq!(resolve_in_jar(&mut z, "foo", "loop"), None);
    }

    /// Real-jar integration: the origins fixture has exactly one item,
    /// `origins:orb_of_origin` (`item/generated` + `layer0`), with a real
    /// PNG. This is the advisor's "not done until it passes on the real jar".
    #[test]
    fn real_origins_jar_orb_of_origin_resolves_to_png() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/real/origins-1.10.2.jar"
        );
        let f = std::fs::File::open(path).expect("origins fixture jar");
        let mut z = ZipArchive::new(f).expect("jar opens");
        let png = resolve_in_jar(&mut z, "origins", "orb_of_origin")
            .expect("orb_of_origin resolves from the real jar");
        assert!(
            png.starts_with(b"\x89PNG"),
            "resolved bytes are a real PNG ({} bytes)",
            png.len()
        );
        assert_eq!(resolve_in_jar(&mut z, "origins", "no_such_item"), None);
    }

    /// A real RGBA PNG (the synthetic `PNG` const is not a decodable image —
    /// the 3D renderer must decode the source texture).
    fn real_png(c: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, 4, 4);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            let mut w = e.write_header().unwrap();
            let mut buf = vec![0u8; 4 * 4 * 4];
            for p in buf.chunks_exact_mut(4) {
                p[0] = c[0];
                p[1] = c[1];
                p[2] = c[2];
                p[3] = 255;
            }
            w.write_image_data(&buf).unwrap();
        }
        out
    }

    #[test]
    fn block_model_item_renders_a_3d_icon() {
        let tex = real_png([180, 90, 40]);
        let mut z = jar(&[
            (
                "assets/foo/models/item/widget.json",
                br#"{"parent":"foo:block/widget"}"#,
            ),
            (
                "assets/foo/models/block/widget.json",
                br##"{"textures":{"all":"foo:block/widget"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"},"down":{"texture":"#all"},"north":{"texture":"#all"},"south":{"texture":"#all"},"east":{"texture":"#all"},"west":{"texture":"#all"}}}]}"##,
            ),
            ("assets/foo/textures/block/widget.png", &tex),
        ]);
        let out = resolve_in_jar(&mut z, "foo", "widget")
            .expect("3D block-model item renders");
        assert!(out.starts_with(b"\x89PNG"), "rendered output is a PNG");
        // It is the 64x64 render, not the 4x4 source texture passed through.
        let mut d = png::Decoder::new(std::io::Cursor::new(&out))
            .read_info()
            .unwrap();
        let info = d.next_frame(&mut vec![0; d.output_buffer_size().unwrap()]).unwrap();
        assert_eq!((info.width, info.height), (64, 64));
    }

    #[test]
    fn fallback_regression_gate_3d_failure_keeps_flat_icon() {
        // Has `elements`, but every face references a texture absent from the
        // jar -> the 3D render produces nothing. It MUST fall through to the
        // flat layer0 sprite, never None and never an error.
        let flat = real_png([20, 200, 90]);
        let mut z = jar(&[
            (
                "assets/foo/models/item/gizmo.json",
                br##"{"parent":"item/generated","textures":{"layer0":"foo:item/gizmo"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#missing"}}}]}"##,
            ),
            ("assets/foo/textures/item/gizmo.png", &flat),
        ]);
        let out = resolve_in_jar(&mut z, "foo", "gizmo")
            .expect("must NOT be None — flat fallback");
        assert_eq!(
            out, flat,
            "3D failure falls back to the exact flat layer0 sprite bytes"
        );
    }
}
