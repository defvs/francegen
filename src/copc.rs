use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use copc_rs::{Bounds, BoundsSelection, CopcReader, LodSelection, Vector};
use geo_types::Coord;
use owo_colors::OwoColorize;

use crate::chunk::{ChunkHeights, ColumnOverlay};
use crate::config::CopcConfig;
use crate::constants::SECTION_SIDE;
use crate::progress::progress_bar;
use crate::world::{WorldStats, dem_to_minecraft};

const COPC_LAYER_INDEX: i32 = -20;
const COPC_OVERLAY_ORDER: u32 = u32::MAX;
const DEFAULT_BUILDING_BLOCK: &str = "minecraft:spruce_planks";

#[derive(Clone, Copy)]
struct VoxelPoint {
    x: i32,
    y: i32,
    z: i32,
}

#[derive(Clone, Copy)]
struct ChunkBounds {
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
}

impl ChunkBounds {
    fn for_chunk(chunk_x: i32, chunk_z: i32) -> Self {
        let min_x = chunk_x.saturating_mul(SECTION_SIDE as i32);
        let min_z = chunk_z.saturating_mul(SECTION_SIDE as i32);
        let max_x = min_x.saturating_add(SECTION_SIDE as i32 - 1);
        let max_z = min_z.saturating_add(SECTION_SIDE as i32 - 1);
        Self {
            min_x,
            max_x,
            min_z,
            max_z,
        }
    }

    fn expanded(&self, radius: i32) -> Self {
        if radius <= 0 {
            return *self;
        }
        Self {
            min_x: self.min_x.saturating_sub(radius),
            max_x: self.max_x.saturating_add(radius),
            min_z: self.min_z.saturating_sub(radius),
            max_z: self.max_z.saturating_add(radius),
        }
    }

    fn contains(&self, x: i32, z: i32) -> bool {
        x >= self.min_x && x <= self.max_x && z >= self.min_z && z <= self.max_z
    }
}

#[derive(Clone, Copy)]
struct InterpolationParams {
    r_xy: i32,
    h_gap: i32,
    t_wall: i32,
    bands: usize,
    tau_persist: f32,
    min_support: usize,
    always_pillar: bool,
}

impl InterpolationParams {
    fn halo_radius(&self) -> i32 {
        (self.r_xy + 1).max(2)
    }

    fn from_config(config: Option<&CopcConfig>) -> Self {
        match config {
            Some(cfg) => Self {
                r_xy: cfg.r_xy(),
                h_gap: cfg.h_gap(),
                t_wall: cfg.t_wall(),
                bands: cfg.bands(),
                tau_persist: cfg.tau_persist(),
                min_support: cfg.min_support(),
                always_pillar: cfg.always_pillar(),
            },
            None => Self::default(),
        }
    }
}

impl Default for InterpolationParams {
    fn default() -> Self {
        Self {
            r_xy: 1,
            h_gap: 3,
            t_wall: 1,
            bands: 4,
            tau_persist: 0.4,
            min_support: 2,
            always_pillar: true,
        }
    }
}

pub fn apply_copc_buildings(
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
    stats: &WorldStats,
    origin: Coord,
    dir: &Path,
    config: Option<&CopcConfig>,
) -> Result<usize> {
    if chunks.is_empty() {
        return Ok(0);
    }

    let paths = collect_copc_files(dir)?;
    if paths.is_empty() {
        anyhow::bail!(
            "No COPC (.copc.laz/.laz/.copc) files found in {}",
            dir.display()
        );
    }

    let file_count = paths.len();
    println!(
        "{} Applying COPC building overlay from {} file{} ({})",
        "ℹ".blue().bold(),
        file_count,
        if file_count == 1 { "" } else { "s" },
        dir.display()
    );

    let bounds = copc_bounds(stats, origin);
    let mut points_by_chunk: HashMap<(i32, i32), Vec<VoxelPoint>> = HashMap::new();
    let mut points_seen: usize = 0;
    let mut building_points: usize = 0;
    let mut usable_points: usize = 0;
    let params = InterpolationParams::from_config(config);

    for (idx, path) in paths.into_iter().enumerate() {
        let mut reader = CopcReader::from_path(&path)
            .with_context(|| format!("Failed to open COPC file {}", path.display()))?;
        let selection = BoundsSelection::Within(bounds);
        let mut iter = reader
            .points(LodSelection::All, selection)
            .with_context(|| format!("Failed to iterate COPC points from {}", path.display()))?;

        let mut remaining = iter.size_hint().0;
        let pb_label = format!("COPC {}/{}", idx + 1, file_count);
        let pb = progress_bar(remaining as u64, &pb_label);
        let msg = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("COPC file")
            .to_string();
        pb.set_message(msg);

        loop {
            let before = remaining;
            let point = iter.next();
            remaining = iter.size_hint().0;
            let processed = before.saturating_sub(remaining);
            if processed > 0 {
                pb.inc(processed as u64);
            }

            let Some(point) = point else {
                break;
            };

            points_seen += 1;
            if u8::from(point.classification) != 6 {
                continue;
            }
            building_points += 1;
            let world_x = (point.x - origin.x).round() as i32;
            let world_z = (origin.y - point.y).round() as i32;
            let chunk_x = world_x.div_euclid(SECTION_SIDE as i32);
            let chunk_z = world_z.div_euclid(SECTION_SIDE as i32);
            let Some(chunk) = chunks.get(&(chunk_x, chunk_z)) else {
                continue;
            };
            let local_x = world_x.rem_euclid(SECTION_SIDE as i32) as usize;
            let local_z = world_z.rem_euclid(SECTION_SIDE as i32) as usize;
            let Some(surface) = chunk.column(local_x, local_z) else {
                continue;
            };
            let top_height = dem_to_minecraft(point.z);
            if top_height <= surface {
                continue;
            }
            points_by_chunk
                .entry((chunk_x, chunk_z))
                .or_default()
                .push(VoxelPoint {
                    x: world_x,
                    y: top_height,
                    z: world_z,
                });
            usable_points += 1;
        }

        pb.finish_and_clear();
    }

    let building_block: Arc<str> = Arc::from(DEFAULT_BUILDING_BLOCK);
    let mut painted = 0usize;
    let mut block_count = 0usize;

    let total_chunks = points_by_chunk.len();
    if total_chunks == 0 {
        println!(
            "  {} Read {} point{} ({} building-classified, {} usable), applied 0 columns",
            "✔".green().bold(),
            points_seen,
            if points_seen == 1 { "" } else { "s" },
            building_points,
            usable_points
        );
        return Ok(0);
    }

    let pb = progress_bar(total_chunks as u64, "Interpolating COPC buildings");
    for ((chunk_x, chunk_z), _) in points_by_chunk.iter() {
        let chunk_bounds = ChunkBounds::for_chunk(*chunk_x, *chunk_z);
        let Some(chunk) = chunks.get_mut(&(*chunk_x, *chunk_z)) else {
            pb.inc(1);
            continue;
        };
        if params.always_pillar {
            let mut max_y_per_column: HashMap<(i32, i32), i32> = HashMap::new();
            if let Some(points) = points_by_chunk.get(&(*chunk_x, *chunk_z)) {
                for p in points {
                    if chunk_bounds.contains(p.x, p.z) {
                        let entry = max_y_per_column.entry((p.x, p.z)).or_insert(p.y);
                        if p.y > *entry {
                            *entry = p.y;
                        }
                    }
                }
            }
            for ((world_x, world_z), max_y) in max_y_per_column {
                let local_x = (world_x - chunk_bounds.min_x) as usize;
                let local_z = (world_z - chunk_bounds.min_z) as usize;
                let Some(surface) = chunk.column(local_x, local_z) else {
                    continue;
                };
                if max_y <= surface {
                    continue;
                }
                let height = (max_y - surface) as u32;
                let overlay = ColumnOverlay::new(
                    COPC_LAYER_INDEX,
                    COPC_OVERLAY_ORDER,
                    None,
                    None,
                    None,
                    None,
                    Some(Arc::clone(&building_block)),
                    Some(height),
                    None,
                );
                chunk.apply_overlay(local_x, local_z, overlay);
                painted += 1;
                block_count += height as usize;
            }
            pb.inc(1);
            continue;
        }

        let halo_bounds = chunk_bounds.expanded(params.halo_radius());
        let points = gather_points_within(&points_by_chunk, &halo_bounds);
        if points.is_empty() {
            pb.inc(1);
            continue;
        }
        let levels = build_building_levels_for_chunk(&points, &params);
        for ((world_x, world_z), mut ys) in levels {
            if !chunk_bounds.contains(world_x, world_z) {
                continue;
            }
            let local_x = (world_x - chunk_bounds.min_x) as usize;
            let local_z = (world_z - chunk_bounds.min_z) as usize;
            if chunk.column(local_x, local_z).is_none() {
                continue;
            }
            ys.sort_unstable();
            ys.dedup();
            if ys.is_empty() {
                continue;
            }
            let level_count = ys.len();
            let overlay = ColumnOverlay::new(
                COPC_LAYER_INDEX,
                COPC_OVERLAY_ORDER,
                None,
                None,
                None,
                None,
                Some(Arc::clone(&building_block)),
                None,
                Some(ys),
            );
            chunk.apply_overlay(local_x, local_z, overlay);
            painted += 1;
            block_count += level_count;
        }
        pb.inc(1);
    }
    pb.finish_and_clear();

    println!(
        "  {} Read {} point{} ({} building-classified, {} usable), applied {} column{} with {} block{}",
        "✔".green().bold(),
        points_seen,
        if points_seen == 1 { "" } else { "s" },
        building_points,
        usable_points,
        painted,
        if painted == 1 { "" } else { "s" },
        block_count,
        if block_count == 1 { "" } else { "s" }
    );

    Ok(painted)
}

fn gather_points_within(
    points_by_chunk: &HashMap<(i32, i32), Vec<VoxelPoint>>,
    bounds: &ChunkBounds,
) -> Vec<VoxelPoint> {
    let min_chunk_x = bounds.min_x.div_euclid(SECTION_SIDE as i32);
    let max_chunk_x = bounds.max_x.div_euclid(SECTION_SIDE as i32);
    let min_chunk_z = bounds.min_z.div_euclid(SECTION_SIDE as i32);
    let max_chunk_z = bounds.max_z.div_euclid(SECTION_SIDE as i32);
    let mut out = Vec::new();
    for chunk_x in min_chunk_x..=max_chunk_x {
        for chunk_z in min_chunk_z..=max_chunk_z {
            if let Some(points) = points_by_chunk.get(&(chunk_x, chunk_z)) {
                for &point in points {
                    if bounds.contains(point.x, point.z) {
                        out.push(point);
                    }
                }
            }
        }
    }
    out
}

fn build_building_levels_for_chunk(
    points: &[VoxelPoint],
    params: &InterpolationParams,
) -> HashMap<(i32, i32), Vec<i32>> {
    let mut occ: HashMap<(i32, i32), BTreeSet<i32>> = HashMap::new();
    for point in points {
        occ.entry((point.x, point.z)).or_default().insert(point.y);
    }
    if occ.is_empty() {
        return HashMap::new();
    }

    // Stage 1: XY closing per Y slice
    let mut closed: HashMap<(i32, i32), BTreeSet<i32>> = HashMap::new();
    let ys_all: BTreeSet<i32> = occ.values().flat_map(|s| s.iter().copied()).collect();
    for &y in ys_all.iter() {
        let mut layer: HashSet<(i32, i32)> = occ
            .iter()
            .filter(|(_, set)| set.contains(&y))
            .map(|(&coord, _)| coord)
            .collect();
        if params.r_xy > 0 {
            layer = dilate_xy(&layer, params.r_xy);
            layer = erode_xy(&layer, params.r_xy);
        }
        for coord in layer {
            closed.entry(coord).or_default().insert(y);
        }
    }

    if closed.is_empty() {
        return HashMap::new();
    }

    // Stage 2 & 3: persistent footprint per band -> perimeter -> vertical fills
    let ys_vec: Vec<i32> = ys_all.iter().copied().collect();
    let bands = split_into_bands(&ys_vec, params.bands);
    let mut levels: HashMap<(i32, i32), BTreeSet<i32>> = HashMap::new();
    for (band_lo, band_hi) in bands {
        let height_len = (band_hi - band_lo + 1).max(1) as usize;
        let mut persistent: HashSet<(i32, i32)> = HashSet::new();
        for (&coord, ys) in closed.iter() {
            let count = ys.range(band_lo..=band_hi).count();
            if count == 0 {
                continue;
            }
            let ratio = count as f32 / height_len as f32;
            if ratio >= params.tau_persist {
                persistent.insert(coord);
            }
        }
        if persistent.is_empty() {
            continue;
        }

        let mut edge = perimeter_xy(&persistent);
        if params.t_wall > 1 {
            edge = dilate_xy(&edge, (params.t_wall - 1) / 2);
        }

        for (x, z) in edge {
            let Some(y_lo) = anchor_low(x, z, band_lo, band_hi, &closed) else {
                continue;
            };
            let Some(y_hi) = anchor_high(x, z, band_lo, band_hi, &closed) else {
                continue;
            };
            if y_hi <= y_lo {
                continue;
            }
            let entry = levels.entry((x, z)).or_default();
            for y in y_lo..=y_hi {
                entry.insert(y);
            }
        }
    }

    // Stage 4: bridge short vertical gaps only when supported on both ends
    for (&(x, z), ys) in closed.iter() {
        let mut sorted: Vec<i32> = ys.iter().copied().collect();
        sorted.sort_unstable();
        for window in sorted.windows(2) {
            let (a, b) = (window[0], window[1]);
            let gap = b - a;
            if gap <= 1 || gap > params.h_gap {
                continue;
            }
            if has_lateral_support(x, z, a, &closed, params.min_support)
                && has_lateral_support(x, z, b, &closed, params.min_support)
            {
                let entry = levels.entry((x, z)).or_default();
                for y in (a + 1)..b {
                    entry.insert(y);
                }
            }
        }
    }

    // Stage 5: final occupancy union
    let mut out: HashMap<(i32, i32), Vec<i32>> = HashMap::new();
    for (coord, mut ys) in closed.into_iter() {
        if let Some(extra) = levels.remove(&coord) {
            ys.extend(extra);
        }
        let values: Vec<i32> = ys.into_iter().collect();
        if !values.is_empty() {
            out.insert(coord, values);
        }
    }
    for (coord, ys) in levels.into_iter() {
        let values: Vec<i32> = ys.into_iter().collect();
        if !values.is_empty() {
            out.insert(coord, values);
        }
    }
    out
}

fn dilate_xy(layer: &HashSet<(i32, i32)>, radius: i32) -> HashSet<(i32, i32)> {
    if radius <= 0 || layer.is_empty() {
        return layer.clone();
    }
    let offsets = disk_offsets(radius);
    let mut out = HashSet::new();
    for &(x, z) in layer.iter() {
        for (dx, dz) in offsets.iter() {
            out.insert((x + dx, z + dz));
        }
    }
    out
}

fn erode_xy(layer: &HashSet<(i32, i32)>, radius: i32) -> HashSet<(i32, i32)> {
    if radius <= 0 || layer.is_empty() {
        return layer.clone();
    }
    let offsets = disk_offsets(radius);
    let mut out = HashSet::new();
    'outer: for &(x, z) in layer.iter() {
        for (dx, dz) in offsets.iter() {
            if !layer.contains(&(x + dx, z + dz)) {
                continue 'outer;
            }
        }
        out.insert((x, z));
    }
    out
}

fn perimeter_xy(mask: &HashSet<(i32, i32)>) -> HashSet<(i32, i32)> {
    if mask.is_empty() {
        return HashSet::new();
    }
    let dilated = dilate_xy(mask, 1);
    let eroded = erode_xy(mask, 1);
    dilated.difference(&eroded).copied().collect()
}

fn anchor_low(
    x: i32,
    z: i32,
    y_min: i32,
    y_max: i32,
    closed: &HashMap<(i32, i32), BTreeSet<i32>>,
) -> Option<i32> {
    let mut best: Option<i32> = None;
    for dz in -1..=1 {
        for dx in -1..=1 {
            if let Some(ys) = closed.get(&(x + dx, z + dz)) {
                if let Some(&y) = ys.range(y_min..=y_max).next() {
                    best = Some(match best {
                        Some(current) => current.min(y),
                        None => y,
                    });
                }
            }
        }
    }
    best
}

fn anchor_high(
    x: i32,
    z: i32,
    y_min: i32,
    y_max: i32,
    closed: &HashMap<(i32, i32), BTreeSet<i32>>,
) -> Option<i32> {
    let mut best: Option<i32> = None;
    for dz in -1..=1 {
        for dx in -1..=1 {
            if let Some(ys) = closed.get(&(x + dx, z + dz)) {
                if let Some(&y) = ys.range(y_min..=y_max).next_back() {
                    best = Some(match best {
                        Some(current) => current.max(y),
                        None => y,
                    });
                }
            }
        }
    }
    best
}

fn has_lateral_support(
    x: i32,
    z: i32,
    y: i32,
    closed: &HashMap<(i32, i32), BTreeSet<i32>>,
    min_support: usize,
) -> bool {
    let mut count = 0usize;
    for dz in -1..=1 {
        for dx in -1..=1 {
            if let Some(ys) = closed.get(&(x + dx, z + dz)) {
                if ys.contains(&y) {
                    count += 1;
                    if count >= min_support {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn split_into_bands(ys: &[i32], band_count: usize) -> Vec<(i32, i32)> {
    if ys.is_empty() || band_count == 0 {
        return Vec::new();
    }
    let mut values = ys.to_vec();
    values.sort_unstable();
    values.dedup();
    if let (Some(min_y), Some(max_y)) = (values.first(), values.last()) {
        if min_y == max_y {
            return vec![(*min_y, *max_y)];
        }
        let span = max_y - min_y + 1;
        let band_height = ((span as f32) / (band_count as f32)).ceil().max(1.0) as i32;
        let mut bands = Vec::new();
        let mut start = *min_y;
        while start <= *max_y {
            let end = (start + band_height - 1).min(*max_y);
            bands.push((start, end));
            start = end.saturating_add(1);
        }
        bands
    } else {
        Vec::new()
    }
}

fn disk_offsets(radius: i32) -> Vec<(i32, i32)> {
    if radius <= 0 {
        return vec![(0, 0)];
    }
    let r2 = radius * radius;
    let mut offsets = Vec::new();
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dz * dz <= r2 {
                offsets.push((dx, dz));
            }
        }
    }
    offsets
}

fn copc_bounds(stats: &WorldStats, origin: Coord) -> Bounds {
    let min_x = origin.x + stats.min_x as f64;
    let max_x = origin.x + stats.max_x as f64;
    let min_y = origin.y - stats.max_z as f64;
    let max_y = origin.y - stats.min_z as f64;
    let min_z = stats.min_height.min(0.0) - 50.0;
    let max_z = stats.max_height + 500.0;
    Bounds {
        min: Vector {
            x: min_x,
            y: min_y,
            z: min_z,
        },
        max: Vector {
            x: max_x,
            y: max_y,
            z: max_z,
        },
    }
}

fn collect_copc_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("Failed to read COPC directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(value) => value.to_ascii_lowercase(),
            None => continue,
        };
        if name.ends_with(".copc.laz") || name.ends_with(".copc") || name.ends_with(".laz") {
            out.push(path);
        }
    }
    Ok(out)
}
