//! The NatureScene project format (`.nsproj`) — the non-destructive layer stack over a
//! procedural base. See `docs/FORMAT.md` for the normative contract; this module is the
//! in-memory model + RON/sidecar IO. It carries NO Bevy and NO rendering — a project is a
//! recipe (`params` + `layers`), and `crate::build_project` turns it into a `World`.
//!
//! On disk a project is a RON file plus a `<file>.d/` sidecar directory holding the binary
//! rasters (PNG16 grayscale for height deltas, PNG8 grayscale for masks). The RON stores
//! only the *reference* (a sidecar file name inside the layer kind); the pixel data lives
//! in `Project::rasters`, keyed by layer id, and is loaded/saved alongside the RON.

use crate::WorldParams;
use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};

/// A whole project: the procedural params + the ordered non-destructive layer stack.
///
/// `rasters` is the decoded sidecar payload (kept out of the RON — it lives in the `.d/`
/// dir as PNGs). It is `#[serde(skip)]`, so RON round-trips only the recipe; `load` refills
/// it from disk. Container-level `serde(default)` makes every top-level field optional so a
/// file written by an older editor (missing a newer field) still parses.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Project {
    /// Bumped ONLY on breaking shape changes (additive fields ride serde defaults).
    pub format_version: u32,
    pub name: String,
    pub params: WorldParams,
    /// Ordered bottom-to-top; applied after the procedural base regenerates.
    pub layers: Vec<Layer>,
    /// Decoded sidecar rasters, keyed by layer id. Never serialized into the RON.
    #[serde(skip)]
    pub rasters: HashMap<u64, LayerRaster>,
}

/// The current on-disk format version.
pub const FORMAT_VERSION: u32 = 1;

impl Default for Project {
    fn default() -> Self {
        Project {
            format_version: FORMAT_VERSION,
            name: "untitled".to_string(),
            params: WorldParams::default(),
            layers: Vec::new(),
            rasters: HashMap::new(),
        }
    }
}

impl Project {
    /// A fresh, empty project (no layers) over the given params. Building this reproduces
    /// exactly what `generate(params)` produces.
    pub fn new(name: impl Into<String>, params: WorldParams) -> Self {
        Project { name: name.into(), params, ..Default::default() }
    }

    /// The largest layer id in use (0 if none) — helper for allocating the next unique id.
    pub fn max_layer_id(&self) -> u64 {
        self.layers.iter().map(|l| l.id).max().unwrap_or(0)
    }
}

/// One entry in the non-destructive stack.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Layer {
    /// Unique within the project, never reused (keys `rasters` + sidecar naming).
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    /// Multiplies the layer's effect (0..1 typical). See per-kind semantics.
    pub opacity: f32,
    pub kind: LayerKind,
}

impl Default for Layer {
    fn default() -> Self {
        // enabled + full opacity: the sane default for a freshly-added layer, and what a
        // forward-compat parse fills in when an older file omitted these fields.
        Layer {
            id: 0,
            name: String::new(),
            enabled: true,
            opacity: 1.0,
            kind: LayerKind::default(),
        }
    }
}

/// What a layer does. The sidecar-backed kinds name a PNG file inside the `.d/` dir.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LayerKind {
    /// Signed height offset in metres, sampled from a PNG16 sidecar. The raster stores the
    /// *normalised* signed value in -1..1; the applied delta is `raster · scale · opacity`.
    HeightDelta { sidecar: String, scale: f32 },
    /// A painted scalar field (PNG8) interpreted per `channel`.
    Mask { channel: MaskChannel, sidecar: String },
    /// Inline per-instance edits, applied after scatter.
    Instances {
        #[serde(default)]
        added: Vec<InstanceAdd>,
        #[serde(default)]
        removed: Vec<InstanceRemove>,
    },
}

impl Default for LayerKind {
    fn default() -> Self {
        // Only used to satisfy `Layer`'s container default during a forward-compat parse
        // where `kind` is present (the real kind always overrides this).
        LayerKind::Instances { added: Vec::new(), removed: Vec::new() }
    }
}

/// The four painted mask channels. See `docs/FORMAT.md` for the exact effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MaskChannel {
    /// Multiplies tree-spawn probability (neutral 0.5).
    ForestDensity,
    /// `w>0.5` suppresses trees, rocks and props.
    Clearing,
    /// Max-combined into the trail-wear field (painted paths).
    PathWear,
    /// Exported for renderers; does NOT affect worldgen scatter.
    GrassDensity,
}

/// An instance placed by hand. `y` is derived from the terrain at build time — never stored.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InstanceAdd {
    /// Mesh id, `<family>/<species-or-kind>/<variant>` (e.g. `tree/pine/2`).
    pub mesh: String,
    pub x: f32,
    pub z: f32,
    pub yaw: f32,
    pub scale: f32,
}

impl Default for InstanceAdd {
    fn default() -> Self {
        InstanceAdd { mesh: String::new(), x: 0.0, z: 0.0, yaw: 0.0, scale: 1.0 }
    }
}

/// A positional removal: anything of `kind` within `radius` metres of (x,z) is culled.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InstanceRemove {
    pub kind: RemoveKind,
    pub x: f32,
    pub z: f32,
    pub radius: f32,
}

impl Default for InstanceRemove {
    fn default() -> Self {
        InstanceRemove { kind: RemoveKind::Tree, x: 0.0, z: 0.0, radius: 1.0 }
    }
}

/// Which scattered family a removal targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RemoveKind {
    Tree,
    Rock,
    Prop,
}

impl RemoveKind {
    /// The mesh-id family prefix this kind matches (`tree`/`rock`/`prop`).
    pub fn family(self) -> &'static str {
        match self {
            RemoveKind::Tree => "tree",
            RemoveKind::Rock => "rock",
            RemoveKind::Prop => "prop",
        }
    }
}

// --- Rasters ------------------------------------------------------------------------------

/// An in-memory sidecar raster. Height deltas are `F32` (the normalised signed value in
/// -1..1, decoded from PNG16); masks are `U8` (0..255, PNG8).
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRaster {
    pub w: usize,
    pub h: usize,
    pub data: LayerData,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayerData {
    /// Height-delta payload: normalised signed value in -1..1.
    F32(Vec<f32>),
    /// Mask payload: 0..255.
    U8(Vec<u8>),
}

impl LayerRaster {
    pub fn new_f32(w: usize, h: usize, data: Vec<f32>) -> Self {
        assert_eq!(w * h, data.len(), "raster f32 data length mismatch");
        LayerRaster { w, h, data: LayerData::F32(data) }
    }

    pub fn new_u8(w: usize, h: usize, data: Vec<u8>) -> Self {
        assert_eq!(w * h, data.len(), "raster u8 data length mismatch");
        LayerRaster { w, h, data: LayerData::U8(data) }
    }

    /// The raw scalar at integer pixel (px, py), clamped. F32 returns the stored value;
    /// U8 returns `byte/255` so both live in a common numeric range for sampling.
    #[inline]
    fn texel(&self, px: usize, py: usize) -> f32 {
        let px = px.min(self.w - 1);
        let py = py.min(self.h - 1);
        let i = py * self.w + px;
        match &self.data {
            LayerData::F32(v) => v[i],
            LayerData::U8(v) => v[i] as f32 / 255.0,
        }
    }

    /// Bilinear sample at a normalised position (u,v) in 0..1 (clamped). This makes a raster
    /// resolution-independent from the build grid — the same paint drives any map size.
    pub fn sample_norm(&self, u: f32, v: f32) -> f32 {
        if self.w == 0 || self.h == 0 {
            return 0.0;
        }
        // Map 0..1 onto the last-pixel range so u=1 lands exactly on the far edge.
        let fx = (u.clamp(0.0, 1.0)) * (self.w - 1) as f32;
        let fy = (v.clamp(0.0, 1.0)) * (self.h - 1) as f32;
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let a = self.texel(x0, y0);
        let b = self.texel(x0 + 1, y0);
        let c = self.texel(x0, y0 + 1);
        let d = self.texel(x0 + 1, y0 + 1);
        let top = a + (b - a) * tx;
        let bot = c + (d - c) * tx;
        top + (bot - top) * ty
    }
}

// PNG16 encode/decode helpers. PNG stores 16-bit samples big-endian; the height-delta
// raster is normalised -1..1, mapped to the full 0..65535 range so 0.0 sits mid-scale.

#[inline]
fn encode16(n: f32) -> u16 {
    (((n.clamp(-1.0, 1.0) * 0.5 + 0.5) * 65535.0).round()) as u16
}

#[inline]
fn decode16(v: u16) -> f32 {
    // FORMAT.md's `v/65535 - 0.5) * 2`, i.e. the normalised signed value (pre scale/opacity).
    (v as f32 / 65535.0 - 0.5) * 2.0
}

fn write_png16(path: &Path, r: &LayerRaster, data: &[f32]) -> io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(BufWriter::new(file), r.w as u32, r.h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Sixteen);
    let mut writer = enc.write_header().map_err(png_err)?;
    let mut bytes = Vec::with_capacity(data.len() * 2);
    for &n in data {
        bytes.extend_from_slice(&encode16(n).to_be_bytes());
    }
    writer.write_image_data(&bytes).map_err(png_err)
}

fn write_png8(path: &Path, r: &LayerRaster, data: &[u8]) -> io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(BufWriter::new(file), r.w as u32, r.h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(png_err)?;
    writer.write_image_data(data).map_err(png_err)
}

/// Read a sidecar PNG. 16-bit grayscale ⇒ `F32` (decoded), 8-bit grayscale ⇒ `U8`.
fn read_png(path: &Path) -> io::Result<LayerRaster> {
    let file = std::fs::File::open(path)?;
    let decoder = png::Decoder::new(BufReader::new(file));
    let mut reader = decoder.read_info().map_err(png_err)?;
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf).map_err(png_err)?;
    let (w, h) = (info.width as usize, info.height as usize);
    match info.bit_depth {
        png::BitDepth::Sixteen => {
            // Big-endian u16 samples → normalised signed f32.
            let mut data = Vec::with_capacity(w * h);
            for i in 0..(w * h) {
                let v = u16::from_be_bytes([buf[2 * i], buf[2 * i + 1]]);
                data.push(decode16(v));
            }
            Ok(LayerRaster::new_f32(w, h, data))
        }
        png::BitDepth::Eight => Ok(LayerRaster::new_u8(w, h, buf[..w * h].to_vec())),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported sidecar bit depth {other:?} in {}", path.display()),
        )),
    }
}

fn png_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("png: {e}"))
}

// --- Save / load --------------------------------------------------------------------------

/// The `<file>.d/` sidecar directory for a given project path.
fn sidecar_dir(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".d");
    PathBuf::from(s)
}

impl Project {
    /// Write the RON recipe to `path` and every sidecar raster into `<path>.d/`.
    ///
    /// A sidecar is written for each layer whose `rasters` entry exists and whose kind names
    /// a sidecar (HeightDelta ⇒ PNG16, Mask ⇒ PNG8). Iteration is sorted by layer id so the
    /// on-disk write order is deterministic. Instances layers carry no sidecar.
    pub fn save(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        // RON recipe (rasters are #[serde(skip)] — only the reference lands here).
        let ron = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::new())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("ron: {e}")))?;
        std::fs::write(path, ron)?;

        // Sidecars — only created when there is raster payload to write.
        let dir = sidecar_dir(path);
        let mut needs_dir = false;
        let mut sorted: Vec<&Layer> = self.layers.iter().collect();
        sorted.sort_by_key(|l| l.id); // deterministic write order
        for layer in &sorted {
            let raster = match self.rasters.get(&layer.id) {
                Some(r) => r,
                None => continue,
            };
            let sidecar = match &layer.kind {
                LayerKind::HeightDelta { sidecar, .. } | LayerKind::Mask { sidecar, .. } => sidecar,
                LayerKind::Instances { .. } => continue,
            };
            if !needs_dir {
                std::fs::create_dir_all(&dir)?;
                needs_dir = true;
            }
            let out = dir.join(sidecar);
            match &raster.data {
                LayerData::F32(d) => write_png16(&out, raster, d)?,
                LayerData::U8(d) => write_png8(&out, raster, d)?,
            }
        }
        Ok(())
    }

    /// Read the RON recipe from `path` and every referenced sidecar out of `<path>.d/`.
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        let mut project: Project = ron::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("ron: {e}")))?;
        let dir = sidecar_dir(path);
        for layer in &project.layers {
            let sidecar = match &layer.kind {
                LayerKind::HeightDelta { sidecar, .. } | LayerKind::Mask { sidecar, .. } => sidecar,
                LayerKind::Instances { .. } => continue,
            };
            let file = dir.join(sidecar);
            // A referenced-but-missing sidecar is a hard error — the layer would silently
            // do nothing otherwise, which is worse than a loud failure.
            let raster = read_png(&file)?;
            project.rasters.insert(layer.id, raster);
        }
        Ok(project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A fresh, unique temp directory for a test's on-disk artifacts.
    fn temp_dir(tag: &str) -> PathBuf {
        static C: AtomicU64 = AtomicU64::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir()
            .join(format!("worldgen_test_{tag}_{}_{nanos}_{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A project carrying one of each layer kind, with small sidecar rasters. The height
    /// delta's f32 values are put on the PNG16 quantisation grid so they round-trip exactly.
    fn sample_project() -> Project {
        let mut p = Project::new("Round Trip", WorldParams::default());
        let (w, h) = (16usize, 16usize);

        // HeightDelta — F32 raster of on-grid normalised values (decode16 output).
        let hd: Vec<f32> = (0..w * h).map(|i| decode16(((i * 137) % 65536) as u16)).collect();
        p.layers.push(Layer {
            id: 1,
            name: "raise".into(),
            enabled: true,
            opacity: 1.0,
            kind: LayerKind::HeightDelta { sidecar: "layer-1-heightdelta.png".into(), scale: 40.0 },
        });
        p.rasters.insert(1, LayerRaster::new_f32(w, h, hd));

        // Mask — U8 raster.
        let mk: Vec<u8> = (0..w * h).map(|i| (i % 256) as u8).collect();
        p.layers.push(Layer {
            id: 2,
            name: "forest".into(),
            enabled: true,
            opacity: 0.8,
            kind: LayerKind::Mask {
                channel: MaskChannel::ForestDensity,
                sidecar: "layer-2-forest.png".into(),
            },
        });
        p.rasters.insert(2, LayerRaster::new_u8(w, h, mk));

        // Instances — inline, no sidecar.
        p.layers.push(Layer {
            id: 3,
            name: "edits".into(),
            enabled: true,
            opacity: 1.0,
            kind: LayerKind::Instances {
                added: vec![InstanceAdd {
                    mesh: "tree/pine/2".into(),
                    x: 10.5,
                    z: 20.0,
                    yaw: 1.2,
                    scale: 1.1,
                }],
                removed: vec![InstanceRemove {
                    kind: RemoveKind::Tree,
                    x: 5.0,
                    z: 5.0,
                    radius: 3.0,
                }],
            },
        });
        p
    }

    #[test]
    fn round_trip_all_layer_kinds() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("proj.nsproj");
        let proj = sample_project();
        proj.save(&path).unwrap();

        // Sidecar dir + the two raster PNGs exist; the instances layer wrote none.
        let d = sidecar_dir(&path);
        assert!(d.join("layer-1-heightdelta.png").exists());
        assert!(d.join("layer-2-forest.png").exists());

        let loaded = Project::load(&path).unwrap();
        assert_eq!(loaded.params, proj.params, "params must round-trip");
        assert_eq!(loaded.layers, proj.layers, "layer recipe must round-trip");
        // Raster bytes (decoded payloads) must round-trip exactly.
        assert_eq!(loaded.rasters, proj.rasters, "sidecar rasters must round-trip");
        assert_eq!(loaded, proj);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forward_compat_missing_optional_field() {
        // Simulate a file written by an OLDER editor that predates the `opacity` field: take
        // a valid serialization and strip the opacity lines. Container serde(default) must
        // refill it from `Layer::default()` (1.0) rather than failing to parse.
        let proj = sample_project();
        let ron = ron::ser::to_string_pretty(&proj, ron::ser::PrettyConfig::new()).unwrap();
        let stripped: String = ron
            .lines()
            .filter(|l| !l.trim_start().starts_with("opacity:"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!stripped.contains("opacity"), "opacity lines removed");
        let parsed: Project = ron::from_str(&stripped).unwrap();
        for layer in &parsed.layers {
            assert_eq!(layer.opacity, 1.0, "missing opacity must default to 1.0");
        }
    }

    #[test]
    fn png16_normalised_round_trips_on_grid() {
        // The height-delta decode/encode is lossless on the quantisation grid.
        for v in [0u16, 1, 12345, 32767, 32768, 65534, 65535] {
            assert_eq!(encode16(decode16(v)), v, "v={v}");
        }
        // Mid-scale decodes to ~0 (neutral height delta).
        assert!(decode16(32768).abs() < 1e-4);
    }
}
