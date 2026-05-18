//! Headless software rasterizer: a Minecraft Java 1.20.1 block/item MODEL
//! (`elements` + `textures` + `display.gui`) -> a small inventory-style PNG
//! icon, the way the game draws a 3D item in a slot. Pure and deterministic
//! (no IO, no GPU): geometry from the cuboid elements, the exact `display.gui`
//! compose chain, an orthographic GUI camera, and the fixed block face shade.
//!
//! Spec sourced from misode/deepslate (BlockModel/ItemRenderer/Renderer) +
//! minecraft.wiki Model. `builtin/generated` (flat) and `builtin/entity`
//! (hardcoded, no data model) are NOT this module's job — the caller handles
//! those. Only models that carry `elements` reach here.

use std::collections::HashMap;

use serde_json::Value;

// ---------------------------------------------------------------------------
// mat4 (column-major, gl-matrix conventions: ops post-multiply, M = M * op)
// ---------------------------------------------------------------------------

type M4 = [f64; 16];

fn ident() -> M4 {
    let mut m = [0.0; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m
}

fn mul(a: &M4, b: &M4) -> M4 {
    let mut o = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + r] * b[c * 4 + k];
            }
            o[c * 4 + r] = s;
        }
    }
    o
}

fn translate(t: [f64; 3]) -> M4 {
    let mut m = ident();
    m[12] = t[0];
    m[13] = t[1];
    m[14] = t[2];
    m
}

fn scale(s: [f64; 3]) -> M4 {
    let mut m = ident();
    m[0] = s[0];
    m[5] = s[1];
    m[10] = s[2];
    m
}

fn rot_x(a: f64) -> M4 {
    let (s, c) = a.sin_cos();
    let mut m = ident();
    m[5] = c;
    m[6] = s;
    m[9] = -s;
    m[10] = c;
    m
}

fn rot_y(a: f64) -> M4 {
    let (s, c) = a.sin_cos();
    let mut m = ident();
    m[0] = c;
    m[2] = -s;
    m[8] = s;
    m[10] = c;
    m
}

fn rot_z(a: f64) -> M4 {
    let (s, c) = a.sin_cos();
    let mut m = ident();
    m[0] = c;
    m[1] = s;
    m[4] = -s;
    m[5] = c;
    m
}

/// Transform a point (w=1) by `m`.
fn xf(m: &M4, p: [f64; 3]) -> [f64; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

fn deg(d: f64) -> f64 {
    d * std::f64::consts::PI / 180.0
}

// ---------------------------------------------------------------------------
// Texture decode (png crate; handles palette/gray/rgb/rgba, any bit depth)
// ---------------------------------------------------------------------------

struct Tex {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}

fn decode_rgba(bytes: &[u8]) -> Option<Tex> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    // EXPAND: palette -> RGB, low-bit grayscale -> 8-bit. STRIP_16: 16 -> 8.
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let src = &buf[..info.buffer_size()];
    let mut rgba = vec![0u8; w * h * 4];
    let px = w * h;
    match info.color_type {
        png::ColorType::Rgba => rgba[..px * 4].copy_from_slice(&src[..px * 4]),
        png::ColorType::Rgb => {
            for i in 0..px {
                rgba[i * 4] = src[i * 3];
                rgba[i * 4 + 1] = src[i * 3 + 1];
                rgba[i * 4 + 2] = src[i * 3 + 2];
                rgba[i * 4 + 3] = 255;
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for i in 0..px {
                let g = src[i * 2];
                rgba[i * 4] = g;
                rgba[i * 4 + 1] = g;
                rgba[i * 4 + 2] = g;
                rgba[i * 4 + 3] = src[i * 2 + 1];
            }
        }
        png::ColorType::Grayscale => {
            for i in 0..px {
                let g = src[i];
                rgba[i * 4] = g;
                rgba[i * 4 + 1] = g;
                rgba[i * 4 + 2] = g;
                rgba[i * 4 + 3] = 255;
            }
        }
        png::ColorType::Indexed => return None, // EXPAND should prevent this
    }
    Some(Tex { w, h, rgba })
}

fn encode_rgba(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(rgba).ok()?;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

fn arr3(v: &Value) -> Option<[f64; 3]> {
    let a = v.as_array()?;
    if a.len() != 3 {
        return None;
    }
    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
}

/// Resolve a `#ref` (possibly chained `#a`->`#b`) against the model textures
/// map to its final value (a `ns:path` or, if unresolved, the raw string).
fn resolve_tex_key<'a>(
    mut t: &'a str,
    textures: &'a serde_json::Map<String, Value>,
) -> &'a str {
    for _ in 0..8 {
        if let Some(name) = t.strip_prefix('#') {
            match textures.get(name).and_then(Value::as_str) {
                Some(next) => t = next,
                None => return t,
            }
        } else {
            return t;
        }
    }
    t
}

/// The four (u,v) corner picks for a face, by `face.rotation`. Indices select
/// from `[u0,v0,u1,v1]` (deepslate `faceRotations`); each pair is (u_idx,
/// v_idx) for one of the 4 face vertices, matching the vertex order below.
const FACE_ROT: [[usize; 8]; 4] = [
    [0, 3, 2, 3, 2, 1, 0, 1], // 0
    [2, 3, 2, 1, 0, 1, 0, 3], // 90
    [2, 1, 0, 1, 0, 3, 2, 3], // 180
    [0, 1, 0, 3, 2, 3, 2, 1], // 270
];

struct Quad {
    /// world-space verts (post element-rotation + display transform)
    p: [[f64; 3]; 4],
    /// per-vertex (u,v) in 0..1 texture space
    uv: [[f64; 2]; 4],
    tex_key: String,
}

/// Build the 6 (or fewer) face quads of one element, in MODEL space, applying
/// the optional element rotation (rescale ONLY when `rescale == true`).
fn element_quads(el: &Value) -> Vec<Quad> {
    let Some(from) = el.get("from").and_then(arr3) else {
        return Vec::new();
    };
    let Some(to) = el.get("to").and_then(arr3) else {
        return Vec::new();
    };
    let (x0, y0, z0) = (from[0], from[1], from[2]);
    let (x1, y1, z1) = (to[0], to[1], to[2]);

    // Element rotation matrix about its origin (default identity).
    let er = el.get("rotation");
    let emat = if let Some(r) = er {
        let origin = r.get("origin").and_then(arr3).unwrap_or([8.0, 8.0, 8.0]);
        let axis = r.get("axis").and_then(Value::as_str).unwrap_or("y");
        let ang = r.get("angle").and_then(Value::as_f64).unwrap_or(0.0);
        let rescale = r
            .get("rescale")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let rmat = match axis {
            "x" => rot_x(deg(ang)),
            "z" => rot_z(deg(ang)),
            _ => rot_y(deg(ang)),
        };
        let smat = if rescale && ang != 0.0 {
            let f = 1.0 / deg(ang).cos().abs();
            match axis {
                "x" => scale([1.0, f, f]),
                "z" => scale([f, f, 1.0]),
                _ => scale([f, 1.0, f]),
            }
        } else {
            ident()
        };
        // T(o) * R * S * T(-o)
        let m = mul(&translate(origin), &rmat);
        let m = mul(&m, &smat);
        mul(&m, &translate([-origin[0], -origin[1], -origin[2]]))
    } else {
        ident()
    };

    // (name, default-uv, the 4 world verts) per face. Vertex order matches
    // deepslate BlockModel face quads; FACE_ROT corner picks align to it.
    let faces: [(&str, [f64; 4], [[f64; 3]; 4]); 6] = [
        (
            "up",
            [x0, 16.0 - z1, x1, 16.0 - z0],
            [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [x0, y1, z0]],
        ),
        (
            "down",
            [16.0 - z1, 16.0 - x1, 16.0 - z0, 16.0 - x0],
            [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
        ),
        (
            "south",
            [x0, 16.0 - y1, x1, 16.0 - y0],
            [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
        ),
        (
            "north",
            [16.0 - x1, 16.0 - y1, 16.0 - x0, 16.0 - y0],
            [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]],
        ),
        (
            "east",
            [16.0 - z1, 16.0 - y1, 16.0 - z0, 16.0 - y0],
            [[x1, y0, z1], [x1, y0, z0], [x1, y1, z0], [x1, y1, z1]],
        ),
        (
            "west",
            [z0, 16.0 - y1, z1, 16.0 - y0],
            [[x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]],
        ),
    ];

    let fobj = el.get("faces").and_then(Value::as_object);
    let mut out = Vec::new();
    for (name, def_uv, verts) in faces {
        let Some(face) = fobj.and_then(|f| f.get(name)) else {
            continue;
        };
        let tex_key = face
            .get("texture")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let uv4 = face
            .get("uv")
            .and_then(|u| {
                let a = u.as_array()?;
                Some([
                    a.first()?.as_f64()?,
                    a.get(1)?.as_f64()?,
                    a.get(2)?.as_f64()?,
                    a.get(3)?.as_f64()?,
                ])
            })
            .unwrap_or(def_uv);
        let rot = face
            .get("rotation")
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as i64;
        let pick = FACE_ROT[match rot {
            90 => 1,
            180 => 2,
            270 => 3,
            _ => 0,
        }];
        // arr = [u0,v0,u1,v1] in 0..16 -> 0..1 (v top-down stays as-is).
        let arr = [
            uv4[0] / 16.0,
            uv4[1] / 16.0,
            uv4[2] / 16.0,
            uv4[3] / 16.0,
        ];
        let mut uv = [[0.0; 2]; 4];
        for i in 0..4 {
            uv[i] = [arr[pick[i * 2]], arr[pick[i * 2 + 1]]];
        }
        let p = [
            xf(&emat, verts[0]),
            xf(&emat, verts[1]),
            xf(&emat, verts[2]),
            xf(&emat, verts[3]),
        ];
        out.push(Quad { p, uv, tex_key });
    }
    out
}

// ---------------------------------------------------------------------------
// display.gui transform
// ---------------------------------------------------------------------------

/// The model->world matrix for the GUI view. `display_gui` is the resolved
/// `display.gui` object (first model in the chain that defined `display`),
/// used WHOLE — unset sub-fields take their per-field defaults, NEVER pulled
/// from the block default. If `display_gui` is `None`, the
/// `minecraft:block/block` default applies (rotation [30,225,0], scale
/// [0.625]^3) — the blocks-as-items case.
fn gui_matrix(display_gui: Option<&Value>) -> M4 {
    let (tr, rot, sc) = match display_gui {
        Some(g) => (
            g.get("translation").and_then(arr3).unwrap_or([0.0, 0.0, 0.0]),
            g.get("rotation").and_then(arr3).unwrap_or([0.0, 0.0, 0.0]),
            g.get("scale").and_then(arr3).unwrap_or([1.0, 1.0, 1.0]),
        ),
        None => ([0.0, 0.0, 0.0], [30.0, 225.0, 0.0], [0.625, 0.625, 0.625]),
    };
    let tr = [
        tr[0].clamp(-80.0, 80.0),
        tr[1].clamp(-80.0, 80.0),
        tr[2].clamp(-80.0, 80.0),
    ];
    let sc = [sc[0].min(4.0), sc[1].min(4.0), sc[2].min(4.0)];
    // T(8) * T(tr) * Rx * Ry * Rz(-rz) * S * T(-8)  (NOTE: Rz uses -rz).
    let mut m = translate([8.0, 8.0, 8.0]);
    m = mul(&m, &translate(tr));
    m = mul(&m, &rot_x(deg(rot[0])));
    m = mul(&m, &rot_y(deg(rot[1])));
    m = mul(&m, &rot_z(deg(-rot[2])));
    m = mul(&m, &scale(sc));
    mul(&m, &translate([-8.0, -8.0, -8.0]))
}

// ---------------------------------------------------------------------------
// Rasterizer
// ---------------------------------------------------------------------------

fn shade(n: [f64; 3]) -> f64 {
    // deepslate vertex formula: 0.8 + n.y*0.2 + |n.z|*0.1 (top 1.0, bottom
    // 0.6, +/-Z 0.9, +/-X 0.8). Deterministic per face.
    (0.8 + n[1] * 0.2 + n[2].abs() * 0.1).clamp(0.0, 1.0)
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l < 1e-9 {
        [0.0, 0.0, 0.0]
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}

/// Render the model to an `size`x`size` RGBA PNG. `tex_png` maps a textures
/// map KEY (no `#`, e.g. "side"/"0"/"all"/"layer0") to that texture's raw PNG
/// bytes (the caller resolved the `ns:path` -> bytes). Returns `None` if
/// nothing drew (caller falls back to a flat texture / label).
pub(crate) fn render_item_icon(
    elements: &[Value],
    textures: &serde_json::Map<String, Value>,
    display_gui: Option<&Value>,
    tex_png: &HashMap<String, Vec<u8>>,
    size: u32,
) -> Option<Vec<u8>> {
    if elements.is_empty() {
        return None;
    }
    let n = size as usize;
    let mut color = vec![0u8; n * n * 4];
    let mut depth = vec![f64::NEG_INFINITY; n * n]; // view-z; larger = nearer
    let mvp = gui_matrix(display_gui);

    // Decode each referenced texture once.
    let mut cache: HashMap<String, Option<Tex>> = HashMap::new();
    let mut drew = false;

    for el in elements {
        for q in element_quads(el) {
            // Resolve "#side" -> textures map -> "ns:path"; the caller keyed
            // tex_png by the textures-map key, so map back to that key.
            let raw = q.tex_key.trim_start_matches('#');
            // Find which textures key ultimately backs this face.
            let key = if textures.contains_key(raw) {
                raw.to_string()
            } else {
                // face referenced a path directly; try any key resolving to it
                resolve_tex_key(&q.tex_key, textures).to_string()
            };
            let tex = cache
                .entry(key.clone())
                .or_insert_with(|| {
                    tex_png.get(&key).and_then(|b| decode_rgba(b))
                })
                .as_ref();
            let Some(tex) = tex else { continue };

            // World verts + face normal (from transformed edges).
            let w: Vec<[f64; 3]> =
                q.p.iter().map(|&p| xf(&mvp, p)).collect();
            let e1 = [w[1][0] - w[0][0], w[1][1] - w[0][1], w[1][2] - w[0][2]];
            let e2 = [w[2][0] - w[0][0], w[2][1] - w[0][1], w[2][2] - w[0][2]];
            let nrm = normalize([
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ]);
            let sh = shade(nrm);

            // Two triangles: (0,1,2) and (0,2,3). Orthographic: world x/y in
            // 0..16 -> pixels; depth = world z (camera looks down -Z, larger
            // z is nearer the viewer).
            for tri in [[0usize, 1, 2], [0, 2, 3]] {
                raster_tri(
                    [w[tri[0]], w[tri[1]], w[tri[2]]],
                    [q.uv[tri[0]], q.uv[tri[1]], q.uv[tri[2]]],
                    tex,
                    sh,
                    n,
                    &mut color,
                    &mut depth,
                    &mut drew,
                );
            }
        }
    }

    if !drew {
        return None;
    }
    encode_rgba(size, size, &color)
}

#[allow(clippy::too_many_arguments)]
fn raster_tri(
    p: [[f64; 3]; 3],
    uv: [[f64; 2]; 3],
    tex: &Tex,
    sh: f64,
    n: usize,
    color: &mut [u8],
    depth: &mut [f64],
    drew: &mut bool,
) {
    // Model 0..16 -> pixel; flip Y so +Y world is the top of the image.
    let sx = |x: f64| x / 16.0 * n as f64;
    let sy = |y: f64| (1.0 - y / 16.0) * n as f64;
    let sp: [[f64; 2]; 3] = [
        [sx(p[0][0]), sy(p[0][1])],
        [sx(p[1][0]), sy(p[1][1])],
        [sx(p[2][0]), sy(p[2][1])],
    ];
    let minx = sp.iter().map(|v| v[0]).fold(f64::MAX, f64::min).floor().max(0.0) as usize;
    let maxx = sp.iter().map(|v| v[0]).fold(f64::MIN, f64::max).ceil().min(n as f64) as usize;
    let miny = sp.iter().map(|v| v[1]).fold(f64::MAX, f64::min).floor().max(0.0) as usize;
    let maxy = sp.iter().map(|v| v[1]).fold(f64::MIN, f64::max).ceil().min(n as f64) as usize;
    let area = (sp[1][0] - sp[0][0]) * (sp[2][1] - sp[0][1])
        - (sp[2][0] - sp[0][0]) * (sp[1][1] - sp[0][1]);
    if area.abs() < 1e-9 {
        return;
    }
    for py in miny..maxy {
        for px in minx..maxx {
            let fx = px as f64 + 0.5;
            let fy = py as f64 + 0.5;
            let w0 = ((sp[1][0] - fx) * (sp[2][1] - fy)
                - (sp[2][0] - fx) * (sp[1][1] - fy))
                / area;
            let w1 = ((sp[2][0] - fx) * (sp[0][1] - fy)
                - (sp[0][0] - fx) * (sp[2][1] - fy))
                / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let z = w0 * p[0][2] + w1 * p[1][2] + w2 * p[2][2];
            let di = py * n + px;
            if z <= depth[di] {
                continue;
            }
            let u = w0 * uv[0][0] + w1 * uv[1][0] + w2 * uv[2][0];
            let v = w0 * uv[0][1] + w1 * uv[1][1] + w2 * uv[2][1];
            let tx = ((u * tex.w as f64).floor() as i64)
                .rem_euclid(tex.w as i64) as usize;
            let ty = ((v * tex.h as f64).floor() as i64)
                .rem_euclid(tex.h as i64) as usize;
            let ti = (ty * tex.w + tx) * 4;
            let a = tex.rgba[ti + 3];
            if a < 8 {
                continue;
            }
            depth[di] = z;
            let ci = di * 4;
            color[ci] = (tex.rgba[ti] as f64 * sh) as u8;
            color[ci + 1] = (tex.rgba[ti + 1] as f64 * sh) as u8;
            color[ci + 2] = (tex.rgba[ti + 2] as f64 * sh) as u8;
            color[ci + 3] = a;
            *drew = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A solid WxH RGBA PNG of one colour, for deterministic shade checks.
    fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for px in buf.chunks_exact_mut(4) {
            px[0] = rgb[0];
            px[1] = rgb[1];
            px[2] = rgb[2];
            px[3] = 255;
        }
        encode_rgba(w, h, &buf).unwrap()
    }

    fn decode(png: &[u8]) -> Tex {
        decode_rgba(png).unwrap()
    }

    fn unit_cube(faces: Value) -> Vec<Value> {
        vec![json!({ "from": [0,0,0], "to": [16,16,16], "faces": faces })]
    }

    fn all_faces(tex: &str) -> Value {
        let f = json!({ "texture": tex });
        json!({
            "up": f, "down": f, "north": f,
            "south": f, "east": f, "west": f
        })
    }

    #[test]
    fn cube_renders_and_shade_constants_hold() {
        let els = unit_cube(all_faces("#t"));
        let textures = json!({ "t": "x:block/t" })
            .as_object()
            .unwrap()
            .clone();
        let mut tp = HashMap::new();
        tp.insert("t".to_string(), solid_png(16, 16, [200, 200, 200]));
        let out =
            render_item_icon(&els, &textures, None, &tp, 64).expect("renders");
        let img = decode(&out);
        assert_eq!((img.w, img.h), (64, 64));
        // Default block display ([30,225,0]) shows top + two sides. Some
        // pixel must be lit at the TOP face shade (1.0 => ~200) and some at
        // a side shade (0.8 => ~160); nothing brighter than the source.
        let lum: Vec<u8> = img.rgba.chunks_exact(4).map(|p| p[0]).collect();
        let maxp = *lum.iter().max().unwrap();
        assert!(maxp <= 200, "never brighter than source ({maxp})");
        assert!(maxp >= 195, "top face ~= source*1.0 ({maxp})");
        assert!(
            lum.iter().any(|&p| (150..=170).contains(&p)),
            "a side face ~= source*0.8 present"
        );
        // Opaque silhouette exists.
        assert!(img.rgba.chunks_exact(4).any(|p| p[3] == 255));
    }

    #[test]
    fn z_rotation_negation_guard() {
        // The single most likely typo: missing minus on rotateZ. A model
        // rotated +90 about Z must differ from -90 (and from default).
        let els = unit_cube(all_faces("#t"));
        let textures =
            json!({ "t": "x:t" }).as_object().unwrap().clone();
        let mut tp = HashMap::new();
        // Asymmetric texture so orientation actually matters.
        let mut a = vec![0u8; 16 * 16 * 4];
        for y in 0..16 {
            for x in 0..16 {
                let i = (y * 16 + x) * 4;
                a[i] = if x < 4 { 255 } else { 30 };
                a[i + 3] = 255;
            }
        }
        tp.insert("t".to_string(), encode_rgba(16, 16, &a).unwrap());
        let g = |rz: i64| {
            json!({ "rotation": [0, 0, rz], "scale": [0.9, 0.9, 0.9] })
        };
        let pos = render_item_icon(&els, &textures, Some(&g(90)), &tp, 48)
            .unwrap();
        let neg = render_item_icon(&els, &textures, Some(&g(-90)), &tp, 48)
            .unwrap();
        assert_ne!(pos, neg, "+90 and -90 Z must not be identical");
    }

    #[test]
    fn default_uv_and_rotation_render_something() {
        // No face.uv => derived from from/to. Also a uv rotation:90.
        let els = vec![json!({
            "from": [0,0,0], "to": [16,16,16],
            "faces": { "up": { "texture": "#t" },
                       "south": { "texture": "#t", "rotation": 90 } }
        })];
        let textures =
            json!({ "t": "x:t" }).as_object().unwrap().clone();
        let mut tp = HashMap::new();
        tp.insert("t".to_string(), solid_png(16, 16, [120, 180, 90]));
        let out =
            render_item_icon(&els, &textures, None, &tp, 32).unwrap();
        assert!(decode(&out).rgba.chunks_exact(4).any(|p| p[3] == 255));
    }

    #[test]
    fn element_rescale_is_conditional() {
        let textures =
            json!({ "t": "x:t" }).as_object().unwrap().clone();
        let mut tp = HashMap::new();
        tp.insert("t".to_string(), solid_png(16, 16, [180, 60, 60]));
        let mk = |rescale: bool| {
            vec![json!({
                "from": [0,0,0], "to": [16,16,16],
                "rotation": { "origin": [8,8,8], "axis": "y",
                              "angle": 45, "rescale": rescale },
                "faces": all_faces("#t")
            })]
        };
        let off = render_item_icon(&mk(false), &textures, None, &tp, 48)
            .unwrap();
        let on = render_item_icon(&mk(true), &textures, None, &tp, 48)
            .unwrap();
        assert_ne!(off, on, "rescale=true must change the silhouette");
    }

    #[test]
    fn empty_elements_is_none() {
        let textures = serde_json::Map::new();
        assert!(render_item_icon(&[], &textures, None, &HashMap::new(), 32)
            .is_none());
    }

    #[test]
    fn render_is_deterministic() {
        let els = unit_cube(all_faces("#t"));
        let textures =
            json!({ "t": "x:t" }).as_object().unwrap().clone();
        let mut tp = HashMap::new();
        tp.insert("t".to_string(), solid_png(16, 16, [10, 200, 240]));
        let a = render_item_icon(&els, &textures, None, &tp, 40).unwrap();
        let b = render_item_icon(&els, &textures, None, &tp, 40).unwrap();
        assert_eq!(a, b, "same input => byte-identical PNG");
    }
}
