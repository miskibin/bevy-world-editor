//! `wed` — headless CLI for the NatureScene world editor.
//!
//! ```text
//! wed export <project.nsproj | --seed N> --out <dir> [--exr] [--size M]
//! ```
//!
//! Runs `worldgen::generate` then `worldgen::export::export_bundle`, writing the export
//! bundle documented in `docs/FORMAT.md`. A `.nsproj` supplies `WorldParams`; `--seed`
//! builds them directly. `--size`/`--seed` override whatever the project provided.
//!
//! Project parsing here is deliberately *lenient*: we do NOT deserialize the whole typed
//! project (that's a separate module still in flight). We read the RON as a generic
//! `ron::Value`, pull the `params` sub-tree, and override our defaults from whichever
//! fields are present. Anything missing falls back to `WorldParams::default()`. If the
//! file has no `params` at all, we error clearly and point at `--seed`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use worldgen::export::{export_bundle, ExportMeta, ExportOptions};
use worldgen::{generate, heightfield::TerrainParams, WorldParams};

const GENERATOR_VERSION: &str = concat!("wed ", env!("CARGO_PKG_VERSION"));

fn usage() -> &'static str {
    "usage: wed export <project.nsproj | --seed N> --out <dir> [--exr] [--size M]"
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let mut it = args.iter();
    let sub = it.next().ok_or_else(|| "missing subcommand".to_string())?;
    if sub != "export" {
        return Err(format!("unknown subcommand `{sub}` (only `export` is supported)"));
    }

    let mut project: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut seed: Option<u32> = None;
    let mut size: Option<usize> = None;
    let mut exr = false;

    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => {
                out = Some(PathBuf::from(
                    it.next().ok_or("`--out` needs a directory")?,
                ));
            }
            "--seed" => {
                let v = it.next().ok_or("`--seed` needs a number")?;
                seed = Some(v.parse().map_err(|_| format!("bad --seed `{v}`"))?);
            }
            "--size" => {
                let v = it.next().ok_or("`--size` needs a number (metres)")?;
                size = Some(v.parse().map_err(|_| format!("bad --size `{v}`"))?);
            }
            "--exr" => exr = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}`"));
            }
            // A bare positional argument is the project file.
            other => project = Some(PathBuf::from(other)),
        }
    }

    let out = out.ok_or("`--out <dir>` is required")?;

    // Assemble WorldParams: project (if any) → then --seed/--size overrides.
    let mut wp = WorldParams::default();
    if let Some(ref path) = project {
        load_params_from_project(path, &mut wp)?;
    } else if seed.is_none() {
        return Err("provide a <project.nsproj> or --seed N".to_string());
    }
    if let Some(s) = seed {
        // Seed drives every deterministic sub-system, so set it everywhere it lives.
        wp.terrain.seed = s;
        wp.forest.seed = s;
    }
    if let Some(m) = size {
        // World size is in metres; at cell=1.0 that's the grid resolution. Keep `cell`.
        wp.terrain.size = m.max(16); // guard against a degenerate tiny grid
    }

    eprintln!(
        "generating {}×{} @ {} m/cell (seed {}) …",
        wp.terrain.size, wp.terrain.size, wp.terrain.cell, wp.terrain.seed
    );
    let mut last_stage = String::new();
    let world = generate(&wp, |frac, stage| {
        // Print only on a stage change (worldgen fires this very often).
        if stage != last_stage {
            eprintln!("  [{:3.0}%] {}", frac * 100.0, stage);
            last_stage = stage.to_string();
        }
    });

    let meta = ExportMeta {
        seed: wp.terrain.seed,
        generator_version: GENERATOR_VERSION.to_string(),
        world_size_m: world.height.extent(),
    };
    let report = export_bundle(&world, &meta, &out, &ExportOptions { exr })
        .map_err(|e| format!("export failed: {e}"))?;

    println!("exported bundle → {}", out.display());
    println!("  files:      {}", report.files.len());
    println!(
        "  height:     {:.2} … {:.2} m",
        report.height_min_m, report.height_max_m
    );
    println!("  instances:  {}", report.instance_count);
    if exr {
        println!("  note:       --exr wrote raw heightmap.f32 (LE f32 metres), not OpenEXR");
    }
    Ok(())
}

/// Read a `.nsproj` as a generic RON value and override `wp` from its `params` sub-tree.
fn load_params_from_project(path: &Path, wp: &mut WorldParams) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let root: ron::Value = ron::from_str(&text)
        .map_err(|e| format!("`{}` is not valid RON: {e}", path.display()))?;
    let params = get(&root, "params").ok_or_else(|| {
        format!(
            "`{}` has no `params` field — pass --seed N to build params directly",
            path.display()
        )
    })?;

    if let Some(terr) = get(params, "terrain") {
        apply(&mut wp.terrain, terr);
    }
    if let Some(er) = get(params, "erosion") {
        if let Some(v) = num(er, "droplets") {
            wp.erosion.droplets = v as u32;
        }
        if let Some(v) = num(er, "talus") {
            wp.erosion.talus = v as f32;
        }
        if let Some(v) = num(er, "thermal_passes") {
            wp.erosion.thermal_passes = v as u32;
        }
    }
    if let Some(forest) = get(params, "forest") {
        if let Some(v) = num(forest, "seed") {
            wp.forest.seed = v as u32;
        }
        if let Some(v) = num(forest, "density") {
            wp.forest.density = v as f32;
        }
        if let Some(v) = num(forest, "treeline") {
            wp.forest.treeline = v as f32;
        }
        if let Some(v) = num(forest, "water_level") {
            wp.forest.water_level = v as f32;
        }
        if let Some(v) = num(forest, "spacing") {
            wp.forest.spacing = v as f32;
        }
    }
    Ok(())
}

/// Apply the commonly-tuned `TerrainParams` scalars found in the RON.
fn apply(t: &mut TerrainParams, v: &ron::Value) {
    if let Some(s) = num(v, "seed") {
        t.seed = s as u32;
    }
    if let Some(s) = num(v, "size") {
        t.size = s as usize;
    }
    if let Some(s) = num(v, "cell") {
        t.cell = s as f32;
    }
    if let Some(s) = num(v, "mountainousness") {
        t.mountainousness = s as f32;
    }
    if let Some(s) = num(v, "mountain_height") {
        t.mountain_height = s as f32;
    }
    if let Some(s) = num(v, "base_height") {
        t.base_height = s as f32;
    }
    if let Some(s) = num(v, "warp") {
        t.warp = s as f32;
    }
}

/// Field lookup in a RON struct/map value (RON renders named-struct fields as string keys).
fn get<'a>(v: &'a ron::Value, key: &str) -> Option<&'a ron::Value> {
    match v {
        ron::Value::Map(m) => m.get(&ron::Value::String(key.to_string())),
        _ => None,
    }
}

/// Numeric field as f64 (RON integers and floats both land in `Value::Number`).
fn num(v: &ron::Value, key: &str) -> Option<f64> {
    match get(v, key)? {
        ron::Value::Number(n) => Some(n.clone().into_f64()),
        _ => None,
    }
}
