//! Export a generated [`World`] to the on-disk **export bundle** other engines consume
//! (see `docs/FORMAT.md` → "Export bundle"). This is I/O — the pure generation logic
//! lives elsewhere; here we only serialize an already-built world.
//!
//! NOT wired into `lib.rs` here on purpose: the main session adds `pub mod export;`. This
//! module is written as if that declaration exists (it references `crate::World` etc.).
//!
//! Determinism contract: for a fixed `World` + `ExportMeta` + `ExportOptions`, every byte
//! written is stable across runs. That means: fixed traversal order (row-major z-major,
//! row 0 = min Z; instances trees→rocks→props in Vec order), pinned PNG compression, and
//! hand-written JSON with a fixed key order (so we don't depend on a serde map's iteration
//! order). Two exports of the same inputs diff to nothing.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::scatter::{
    PROP_BUSH_BIRCH, PROP_BUSH_BROADLEAF, PROP_LOG, PROP_MUSHROOM, PROP_STUMP,
};
use crate::tree::Species;
use crate::World;

/// Bundle-level options.
pub struct ExportOptions {
    /// Also write a 32-bit-float heightmap in metres. See the `heightmap.exr` note below:
    /// we currently emit a raw `heightmap.f32` instead of a real EXR (documented deviation).
    pub exr: bool,
}

/// Metadata the generator knows but the `World` doesn't carry.
pub struct ExportMeta {
    pub seed: u32,
    pub generator_version: String,
    /// World extent in metres per side (redundant with `size * cell`, but authoritative
    /// from the caller so it survives any future decoupling of grid res from world size).
    pub world_size_m: f32,
}

/// What was written, for the CLI to print.
pub struct ExportReport {
    pub files: Vec<PathBuf>,
    pub height_min_m: f32,
    pub height_max_m: f32,
    pub instance_count: usize,
}

const FORMAT_VERSION: u32 = 1;

// ── math helpers (kept local so the pure crate stays free of a math dep) ───────────────

/// Hermite smoothstep — matches the shader / scatter code. Works with `e0 > e1` (inverse
/// ramp) because the ratio is clamped to `[0,1]` before the cubic.
#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Log-compressed, max-normalised flow in `[0,1]`. This mirrors exactly how
/// `maps::moisture_map` conditions the flow field before it reaches the terrain shader
/// (which reads it as UV1.y in `[0,1]`), so our `flow.png` mask and the splat's gully term
/// agree with the runtime look.
fn normalized_flow(world: &World) -> Vec<f32> {
    let mut m: Vec<f32> = world.flow.iter().map(|&f| (1.0 + f).ln()).collect();
    let max = m.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
    m.iter_mut().for_each(|v| *v /= max);
    m
}

// ── PNG writers (pinned compression → deterministic bytes) ─────────────────────────────

fn png_encoder<'a, W: Write>(
    w: W,
    size: usize,
    color: png::ColorType,
    depth: png::BitDepth,
) -> png::Encoder<'a, W> {
    let mut enc = png::Encoder::new(w, size as u32, size as u32);
    enc.set_color(color);
    enc.set_depth(depth);
    // Pin compression so the encoded bytes never drift with the crate's default.
    enc.set_compression(png::Compression::Fast);
    enc
}

fn to_io<E: std::error::Error + Send + Sync + 'static>(e: E) -> io::Error {
    io::Error::other(e)
}

/// RGBA8 image, row-major, row 0 = min Z.
fn write_png_rgba8(path: &Path, size: usize, data: &[u8]) -> io::Result<()> {
    let w = BufWriter::new(File::create(path)?);
    let enc = png_encoder(w, size, png::ColorType::Rgba, png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(to_io)?;
    writer.write_image_data(data).map_err(to_io)?;
    writer.finish().map_err(to_io)
}

/// 16-bit grayscale image. PNG stores 16-bit samples big-endian, so `data` is fed as
/// big-endian byte pairs.
fn write_png_gray16(path: &Path, size: usize, data: &[u16]) -> io::Result<()> {
    let mut bytes = Vec::with_capacity(data.len() * 2);
    for &v in data {
        bytes.extend_from_slice(&v.to_be_bytes());
    }
    let w = BufWriter::new(File::create(path)?);
    let enc = png_encoder(w, size, png::ColorType::Grayscale, png::BitDepth::Sixteen);
    let mut writer = enc.write_header().map_err(to_io)?;
    writer.write_image_data(&bytes).map_err(to_io)?;
    writer.finish().map_err(to_io)
}

// ── mesh_id grammar (FORMAT.md) ────────────────────────────────────────────────────────

fn species_name(s: Species) -> &'static str {
    match s {
        Species::Pine => "pine",
        Species::Spruce => "spruce",
        Species::Broadleaf => "broadleaf",
        Species::Birch => "birch",
    }
}

/// Undergrowth prop kind → grammar name. Props carry no variant field in worldgen, so the
/// variant is always `0` (the grammar reserves higher variants for future prop meshes).
fn prop_name(kind: u8) -> &'static str {
    match kind {
        PROP_BUSH_BROADLEAF => "bush_broadleaf",
        PROP_BUSH_BIRCH => "bush_birch",
        PROP_LOG => "log",
        PROP_STUMP => "stump",
        PROP_MUSHROOM => "mushroom",
        _ => "unknown",
    }
}

/// One flattened instance, ready for JSON/CSV. `y` is taken from the instance (all worldgen
/// instances carry a baked terrain-height `y`); FORMAT allows deriving it from terrain when
/// an instance lacks one, but ours never do.
struct Inst {
    mesh: String,
    x: f32,
    y: f32,
    z: f32,
    yaw: f32,
    scale: f32,
}

/// Trees → rocks → props, each in its own `Vec` order (stable ordering guarantee).
fn collect_instances(world: &World) -> Vec<Inst> {
    let mut out = Vec::with_capacity(world.trees.len() + world.rocks.len() + world.props.len());
    for t in &world.trees {
        out.push(Inst {
            mesh: format!("tree/{}/{}", species_name(t.species), t.variant),
            x: t.x,
            y: t.y,
            z: t.z,
            yaw: t.yaw,
            scale: t.scale,
        });
    }
    for r in &world.rocks {
        out.push(Inst {
            // `% 4` is defensive — `kind` is already `next_u32() % 4`, but the grammar
            // fixes boulder variants at 0..3.
            mesh: format!("rock/boulder/{}", r.kind % 4),
            x: r.x,
            y: r.y,
            z: r.z,
            yaw: r.yaw,
            scale: r.scale,
        });
    }
    for p in &world.props {
        out.push(Inst {
            mesh: format!("prop/{}/0", prop_name(p.kind)),
            x: p.x,
            y: p.y,
            z: p.z,
            yaw: p.yaw,
            scale: p.scale,
        });
    }
    // Hand-placed instances (the project's `Instances` layers) come last — their mesh id
    // is stored verbatim in the layer, so it passes through untouched.
    for a in &world.added {
        out.push(Inst {
            mesh: a.mesh.clone(),
            x: a.x,
            y: a.y,
            z: a.z,
            yaw: a.yaw,
            scale: a.scale,
        });
    }
    out
}

// ── tiny hand-rolled JSON (fixed key order, no serde dep in the pure crate) ─────────────

/// Finite floats only — non-finite would produce invalid JSON. Heights/coords are always
/// finite here, but we clamp defensively rather than emit `inf`/`NaN`. Rust's default `f32`
/// Display is shortest-round-trip and deterministic.
fn jf(v: f32) -> String {
    if v.is_finite() { format!("{v}") } else { "0".to_string() }
}

fn jstr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── the export ─────────────────────────────────────────────────────────────────────────

pub fn export_bundle(
    world: &World,
    seed_meta: &ExportMeta,
    out_dir: &Path,
    opts: &ExportOptions,
) -> io::Result<ExportReport> {
    let size = world.height.size;
    let cell = world.height.cell;
    let h = &world.height.h;
    let n = size * size;

    fs::create_dir_all(out_dir)?;
    let masks_dir = out_dir.join("masks");
    fs::create_dir_all(&masks_dir)?;

    let mut files: Vec<PathBuf> = Vec::new();

    // Height range over the whole field (drives the r16 normalisation + meta).
    let mut hmin = f32::INFINITY;
    let mut hmax = f32::NEG_INFINITY;
    for &v in h {
        hmin = hmin.min(v);
        hmax = hmax.max(v);
    }
    if !hmin.is_finite() {
        hmin = 0.0;
        hmax = 0.0;
    }
    let range = (hmax - hmin).max(1e-6);

    // ── heightmap.r16 — 16-bit LE, row-major, row 0 = min Z. The global-min cell maps to
    // 0 and the global-max cell to 65535, so the recorded [min,max] in meta.json is the
    // exact reconstruction key: h = min + (v/65535)*(max-min).
    {
        let path = out_dir.join("heightmap.r16");
        let mut bytes = Vec::with_capacity(n * 2);
        for &v in h {
            let t = ((v - hmin) / range).clamp(0.0, 1.0);
            let q = (t * 65535.0).round() as u16;
            bytes.extend_from_slice(&q.to_le_bytes());
        }
        fs::write(&path, &bytes)?;
        files.push(path);
    }

    // ── heightmap.f32 (optional). DEVIATION from FORMAT.md: it names an OpenEXR
    // `heightmap.exr`; to keep this pure crate free of the heavy `exr` dependency we emit a
    // raw little-endian f32 raster of metres instead — explicitly sanctioned fallback. The
    // filename + format are recorded in meta.json (`heightmap_float_file`) so consumers can
    // find it. Same orientation as the r16 (row 0 = min Z).
    if opts.exr {
        let path = out_dir.join("heightmap.f32");
        let mut bytes = Vec::with_capacity(n * 4);
        for &v in h {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        fs::write(&path, &bytes)?;
        files.push(path);
    }

    // ── splatmap.png — RGBA8 material weights (R grass, G forest-floor, B rock, A dirt).
    // CPU approximation of `terrain.wgsl`'s weight logic. DIVERGENCES from the shader,
    // deliberate (the shader's per-pixel character noise has no CPU counterpart at grid
    // res): (1) the organic `patch_noise` boundary wobble on every threshold is dropped —
    // we use the shader's *central* thresholds; (2) the dirt "dry patch" term's noise mask
    // is replaced by its ~mean (×0.5); (3) `slope` here is the geometric normal from the
    // slope map, without the shader's micro-relief bump. Weights are normalised to sum 1
    // then quantised to bytes summing ~255.
    {
        let flow_n = normalized_flow(world);
        let mut px = Vec::with_capacity(n * 4);
        for i in 0..n {
            let s = world.slope[i]; // rise/run (tan of slope angle)
            let gn_y = 1.0 / (1.0 + s * s).sqrt(); // = cos(angle) = geometric normal.y
            let shader_slope = 1.0 - gn_y; // 0 flat … 1 vertical (matches shader `slope`)
            let m = world.moisture[i];
            let f = flow_n[i];
            let t = world.trails[i];

            let rock = smoothstep(0.16, 0.30, shader_slope);
            let dirt_flow = smoothstep(0.35, 0.75, f);
            let dirt_dry = smoothstep(0.25, 0.05, m) * 0.5; // shader ×noise → ×mean
            let dirt = (dirt_flow + dirt_dry).clamp(0.0, 1.0) * (1.0 - rock);
            let ff = smoothstep(0.28, 0.58, m) * (1.0 - rock) * (1.0 - dirt);
            let grass = (1.0 - rock - dirt - ff).max(0.0);

            // Trail wear punches worn earth (dirt) through every layer, like the shader's
            // trail lane: pull all weights toward pure dirt by the lane factor.
            let lane = smoothstep(0.18, 0.62, t);
            let inv = 1.0 - lane;
            let (grass, ff, rock, dirt) =
                (grass * inv, ff * inv, rock * inv, dirt * inv + lane);

            let sum = grass + ff + rock + dirt;
            let (g, fl, rk, dt) = if sum > 1e-6 {
                (grass / sum, ff / sum, rock / sum, dirt / sum)
            } else {
                (1.0, 0.0, 0.0, 0.0) // fully-dry degenerate cell → grass
            };
            px.push((g * 255.0).round() as u8);
            px.push((fl * 255.0).round() as u8);
            px.push((rk * 255.0).round() as u8);
            px.push((dt * 255.0).round() as u8);
        }
        let path = out_dir.join("splatmap.png");
        write_png_rgba8(&path, size, &px)?;
        files.push(path);
    }

    // ── masks/*.png — PNG16 grayscale ──────────────────────────────────────────────────
    {
        let to_u16 = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;

        // moisture 0..1
        let moisture: Vec<u16> = world.moisture.iter().map(|&v| to_u16(v)).collect();
        let path = masks_dir.join("moisture.png");
        write_png_gray16(&path, size, &moisture)?;
        files.push(path);

        // flow: same log/max normalisation the maps use, so 0..1
        let flow_n = normalized_flow(world);
        let flow: Vec<u16> = flow_n.iter().map(|&v| to_u16(v)).collect();
        let path = masks_dir.join("flow.png");
        write_png_gray16(&path, size, &flow)?;
        files.push(path);

        // trail wear 0..1
        let trail: Vec<u16> = world.trails.iter().map(|&v| to_u16(v)).collect();
        let path = masks_dir.join("trail_wear.png");
        write_png_gray16(&path, size, &trail)?;
        files.push(path);

        // water depth in millimetres (surface − terrain where wet, else 0=dry), clamped to
        // the u16 range (max ≈ 65.5 m of depth).
        let depth: Vec<u16> = (0..n)
            .map(|i| {
                let w = world.water[i];
                let d_m = if w.is_finite() { (w - h[i]).max(0.0) } else { 0.0 };
                (d_m * 1000.0).round().clamp(0.0, 65535.0) as u16
            })
            .collect();
        let path = masks_dir.join("water_depth.png");
        write_png_gray16(&path, size, &depth)?;
        files.push(path);
    }

    // ── instances.json / instances.csv ─────────────────────────────────────────────────
    let insts = collect_instances(world);
    {
        // JSON array, one object per line for a clean diff.
        let mut s = String::from("[\n");
        for (i, it) in insts.iter().enumerate() {
            s.push_str(&format!(
                "  {{\"mesh\":{},\"x\":{},\"y\":{},\"z\":{},\"yaw\":{},\"scale\":{}}}",
                jstr(&it.mesh),
                jf(it.x),
                jf(it.y),
                jf(it.z),
                jf(it.yaw),
                jf(it.scale)
            ));
            if i + 1 < insts.len() {
                s.push(',');
            }
            s.push('\n');
        }
        s.push_str("]\n");
        let path = out_dir.join("instances.json");
        fs::write(&path, s.as_bytes())?;
        files.push(path);

        // CSV: header + one row per instance. Line count = instance_count + 1.
        let mut c = String::from("mesh,x,y,z,yaw,scale\n");
        for it in &insts {
            c.push_str(&format!(
                "{},{},{},{},{},{}\n",
                it.mesh,
                jf(it.x),
                jf(it.y),
                jf(it.z),
                jf(it.yaw),
                jf(it.scale)
            ));
        }
        let path = out_dir.join("instances.csv");
        fs::write(&path, c.as_bytes())?;
        files.push(path);
    }

    // ── manifest.json — per-mesh_id counts, ordered by first appearance (stable). ───────
    {
        let mut ids: Vec<(String, usize)> = Vec::new();
        for it in &insts {
            match ids.iter_mut().find(|(id, _)| *id == it.mesh) {
                Some((_, c)) => *c += 1,
                None => ids.push((it.mesh.clone(), 1)),
            }
        }
        let mut s = format!("{{\n  \"format_version\": {FORMAT_VERSION},\n  \"mesh_ids\": [\n");
        for (i, (id, count)) in ids.iter().enumerate() {
            s.push_str(&format!("    {{\"id\": {}, \"count\": {}}}", jstr(id), count));
            if i + 1 < ids.len() {
                s.push(',');
            }
            s.push('\n');
        }
        s.push_str("  ]\n}\n");
        let path = out_dir.join("manifest.json");
        fs::write(&path, s.as_bytes())?;
        files.push(path);
    }

    // ── meta.json — the reconstruction key + provenance. Fixed key order. ───────────────
    {
        let mut s = String::from("{\n");
        s.push_str(&format!("  \"world_size_m\": {},\n", jf(seed_meta.world_size_m)));
        s.push_str(&format!("  \"grid_resolution\": {size},\n"));
        s.push_str(&format!("  \"cell_size_m\": {},\n", jf(cell)));
        s.push_str(&format!("  \"height_min_m\": {},\n", jf(hmin)));
        s.push_str(&format!("  \"height_max_m\": {},\n", jf(hmax)));
        s.push_str(&format!("  \"seed\": {},\n", seed_meta.seed));
        s.push_str(&format!(
            "  \"generator_version\": {},\n",
            jstr(&seed_meta.generator_version)
        ));
        // Additive fields (readers ignore unknowns per the FORMAT stability promise).
        s.push_str("  \"heightmap_r16\": \"u16 LE, row-major, row 0 = min Z; h = height_min_m + (v/65535)*(height_max_m-height_min_m)\"");
        if opts.exr {
            s.push_str(",\n  \"heightmap_float_file\": \"heightmap.f32\",\n");
            s.push_str("  \"heightmap_float_format\": \"raw little-endian f32 metres (EXR fallback)\"\n");
        } else {
            s.push('\n');
        }
        s.push_str("}\n");
        let path = out_dir.join("meta.json");
        fs::write(&path, s.as_bytes())?;
        files.push(path);
    }

    Ok(ExportReport {
        files,
        height_min_m: hmin,
        height_max_m: hmax,
        instance_count: insts.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::erosion::ErosionParams;
    use crate::heightfield::TerrainParams;
    use crate::scatter::ForestParams;
    use crate::{generate, WorldParams};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn tiny_world() -> World {
        let p = WorldParams {
            terrain: TerrainParams { size: 96, ..Default::default() },
            erosion: ErosionParams { droplets: 4000, ..Default::default() },
            forest: ForestParams::default(),
        };
        generate(&p, |_, _| {})
    }

    fn unique_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let c = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "wg_export_{tag}_{}_{nanos}_{c}",
            std::process::id()
        ))
    }

    fn read_bytes(p: &Path) -> Vec<u8> {
        fs::read(p).unwrap()
    }

    #[test]
    fn exports_all_files_and_shapes() {
        let world = tiny_world();
        let size = world.height.size;
        let dir = unique_dir("shapes");
        let meta = ExportMeta {
            seed: 20260719,
            generator_version: "test-0".to_string(),
            world_size_m: world.height.extent(),
        };
        let report =
            export_bundle(&world, &meta, &dir, &ExportOptions { exr: true }).unwrap();

        // Every advertised file exists.
        for name in [
            "heightmap.r16",
            "heightmap.f32",
            "splatmap.png",
            "instances.json",
            "instances.csv",
            "manifest.json",
            "meta.json",
            "masks/moisture.png",
            "masks/flow.png",
            "masks/trail_wear.png",
            "masks/water_depth.png",
        ] {
            assert!(dir.join(name).exists(), "missing {name}");
        }

        // r16: length and that the normalisation touches both rails.
        let r16 = read_bytes(&dir.join("heightmap.r16"));
        assert_eq!(r16.len(), size * size * 2, "r16 wrong length");
        let vals: Vec<u16> = r16
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(*vals.iter().min().unwrap(), 0, "r16 min not 0");
        assert_eq!(*vals.iter().max().unwrap(), 65535, "r16 max not 65535");

        // splatmap dims == grid. png 0.18's Decoder wants BufRead + Seek.
        let dec = png::Decoder::new(io::BufReader::new(
            File::open(dir.join("splatmap.png")).unwrap(),
        ));
        let reader = dec.read_info().unwrap();
        let info = reader.info();
        assert_eq!(info.width as usize, size);
        assert_eq!(info.height as usize, size);

        // instances.csv line count == manifest total + 1 (header).
        let csv = fs::read_to_string(dir.join("instances.csv")).unwrap();
        let csv_lines = csv.lines().filter(|l| !l.is_empty()).count();
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        let manifest_total: u64 = manifest["mesh_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["count"].as_u64().unwrap())
            .sum();
        assert_eq!(csv_lines as u64, manifest_total + 1, "csv rows vs manifest");
        assert_eq!(manifest_total as usize, report.instance_count);

        // meta.json parses and round-trips the height range.
        let meta_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap())
                .unwrap();
        assert_eq!(meta_json["grid_resolution"].as_u64().unwrap() as usize, size);
        let mmin = meta_json["height_min_m"].as_f64().unwrap() as f32;
        let mmax = meta_json["height_max_m"].as_f64().unwrap() as f32;
        assert!((mmin - report.height_min_m).abs() < 1e-3);
        assert!((mmax - report.height_max_m).abs() < 1e-3);
        assert_eq!(meta_json["heightmap_float_file"], "heightmap.f32");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deterministic_byte_identical() {
        let world = tiny_world();
        let meta = ExportMeta {
            seed: 7,
            generator_version: "det".to_string(),
            world_size_m: world.height.extent(),
        };
        let a = unique_dir("det_a");
        let b = unique_dir("det_b");
        let ra = export_bundle(&world, &meta, &a, &ExportOptions { exr: true }).unwrap();
        let _rb = export_bundle(&world, &meta, &b, &ExportOptions { exr: true }).unwrap();

        for f in &ra.files {
            let rel = f.strip_prefix(&a).unwrap();
            let other = b.join(rel);
            assert_eq!(
                read_bytes(f),
                read_bytes(&other),
                "byte mismatch for {rel:?}"
            );
        }

        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }
}
