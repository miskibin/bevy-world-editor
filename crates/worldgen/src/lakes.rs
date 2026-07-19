//! Lake detection: priority-flood depression filling. Erosion leaves real basins in the
//! heightfield; flooding them (instead of one global water plane) puts lakes in mountain
//! valleys at their own levels.
//!
//! Classic algorithm (Barnes et al.): seed a min-heap with the border cells at terrain
//! height; repeatedly pop the lowest water level and relax neighbours to
//! `max(their height, popped level)`. Wherever the filled surface ends up above the
//! terrain, that cell is under a lake whose surface is the filled level.

use crate::heightfield::HeightField;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// Per-cell lake surface height; `f32::NEG_INFINITY` where there is no water.
pub struct WaterSurface {
    pub surface: Vec<f32>,
    pub lake_count: usize,
}

struct Cell {
    level: f32,
    idx: usize,
}

impl PartialEq for Cell {
    fn eq(&self, other: &Self) -> bool {
        self.level == other.level
    }
}
impl Eq for Cell {}
impl PartialOrd for Cell {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Cell {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse: BinaryHeap is a max-heap, we need the LOWEST level first.
        other.level.partial_cmp(&self.level).unwrap_or(Ordering::Equal)
    }
}

/// `min_depth`/`min_cells` filter out puddle noise; `global_level` floods the lowland
/// (the "sea" of the map) regardless of basin topology.
pub fn detect_lakes(
    hf: &HeightField,
    global_level: f32,
    min_depth: f32,
    min_cells: usize,
) -> WaterSurface {
    let size = hf.size;
    let n = size * size;
    let mut filled = vec![f32::INFINITY; n];
    let mut heap = BinaryHeap::new();

    // Border cells can always drain off-map: their filled level is their own height
    // (but never below the global waterline, which floods in from the edges).
    for i in 0..size {
        for idx in [i, (size - 1) * size + i, i * size, i * size + size - 1] {
            let level = hf.h[idx].max(global_level);
            if filled[idx].is_infinite() {
                filled[idx] = level;
                heap.push(Cell { level, idx });
            }
        }
    }

    while let Some(Cell { level, idx }) = heap.pop() {
        if level > filled[idx] {
            continue; // stale entry
        }
        let x = idx % size;
        let z = idx / size;
        for (nx, nz) in [
            (x.wrapping_sub(1), z),
            (x + 1, z),
            (x, z.wrapping_sub(1)),
            (x, z + 1),
        ] {
            if nx >= size || nz >= size {
                continue;
            }
            let ni = nz * size + nx;
            let cand = hf.h[ni].max(level);
            if cand < filled[ni] {
                filled[ni] = cand;
                heap.push(Cell { level: cand, idx: ni });
            }
        }
    }

    // Lake mask: filled meaningfully above terrain.
    let mut surface: Vec<f32> = (0..n)
        .map(|i| {
            if filled[i] - hf.h[i] > 0.05 {
                filled[i]
            } else {
                f32::NEG_INFINITY
            }
        })
        .collect();

    // Connected components; drop small/shallow ones (puddle noise from erosion pits).
    let mut comp = vec![u32::MAX; n];
    let mut lake_count = 0usize;
    let mut stack = Vec::new();
    for start in 0..n {
        if surface[start].is_finite() && comp[start] == u32::MAX {
            let id = lake_count as u32;
            let mut cells = Vec::new();
            let mut max_depth = 0.0f32;
            stack.push(start);
            comp[start] = id;
            while let Some(i) = stack.pop() {
                cells.push(i);
                max_depth = max_depth.max(surface[i] - hf.h[i]);
                let x = i % size;
                let z = i / size;
                for (nx, nz) in [
                    (x.wrapping_sub(1), z),
                    (x + 1, z),
                    (x, z.wrapping_sub(1)),
                    (x, z + 1),
                ] {
                    if nx >= size || nz >= size {
                        continue;
                    }
                    let ni = nz * size + nx;
                    if surface[ni].is_finite() && comp[ni] == u32::MAX {
                        comp[ni] = id;
                        stack.push(ni);
                    }
                }
            }
            if cells.len() < min_cells || max_depth < min_depth {
                for i in cells {
                    surface[i] = f32::NEG_INFINITY;
                }
            } else {
                lake_count += 1;
            }
        }
    }

    WaterSurface { surface, lake_count }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bowl_becomes_lake() {
        // A 32x32 field with a 6-deep bowl in the middle.
        let mut hf = HeightField::new(32, 1.0);
        for z in 0..32 {
            for x in 0..32 {
                let dx = x as f32 - 16.0;
                let dz = z as f32 - 16.0;
                let d = (dx * dx + dz * dz).sqrt();
                hf.set(x, z, 20.0 - (6.0 - d * 0.8).max(0.0));
            }
        }
        let w = detect_lakes(&hf, -100.0, 0.4, 4);
        assert_eq!(w.lake_count, 1);
        let centre = w.surface[16 * 32 + 16];
        assert!(centre.is_finite());
        // Surface at the bowl's rim height (spill level), well above the bowl floor.
        assert!(centre > 14.0 && centre <= 20.01, "surface={centre}");
        // Rim itself dry.
        assert!(w.surface[2 * 32 + 2].is_infinite());
    }

    #[test]
    fn slope_has_no_lakes() {
        let mut hf = HeightField::new(32, 1.0);
        for z in 0..32 {
            for x in 0..32 {
                hf.set(x, z, x as f32 * 0.5 + 10.0);
            }
        }
        let w = detect_lakes(&hf, -100.0, 0.4, 4);
        assert_eq!(w.lake_count, 0);
        assert!(w.surface.iter().all(|s| s.is_infinite()));
    }

    #[test]
    fn global_level_floods_lowland() {
        let mut hf = HeightField::new(32, 1.0);
        for z in 0..32 {
            for x in 0..32 {
                hf.set(x, z, x as f32 * 0.5); // 0..15.5 ramp
            }
        }
        let w = detect_lakes(&hf, 5.0, 0.4, 4);
        assert!(w.lake_count >= 1);
        // Low end under water at the global level, high end dry.
        assert!((w.surface[16 * 32] - 5.0).abs() < 1e-3);
        assert!(w.surface[16 * 32 + 31].is_infinite());
    }
}
