use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use copc_rs::{Bounds, BoundsSelection, CopcReader, LodSelection, Vector};
use geo_types::Coord;
use owo_colors::OwoColorize;

use crate::chunk::{ChunkHeights, ColumnOverlay};
use crate::constants::SECTION_SIDE;
use crate::world::{WorldStats, dem_to_minecraft};

const COPC_LAYER_INDEX: i32 = -20;
const COPC_OVERLAY_ORDER: u32 = u32::MAX;
const DEFAULT_BUILDING_BLOCK: &str = "minecraft:spruce_planks";
const DEFAULT_BUILDING_SUBSURFACE: &str = "minecraft:stone";

pub fn apply_copc_buildings(
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
    stats: &WorldStats,
    origin: Coord,
    dir: &Path,
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

    println!(
        "{} Applying COPC building overlay from {} file{} ({})",
        "ℹ".blue().bold(),
        paths.len(),
        if paths.len() == 1 { "" } else { "s" },
        dir.display()
    );

    let bounds = copc_bounds(stats, origin);
    let mut max_extra_height: HashMap<(i32, i32), u32> = HashMap::new();
    let mut points_seen: usize = 0;
    let mut building_points: usize = 0;

    for path in paths {
        let mut reader = CopcReader::from_path(&path)
            .with_context(|| format!("Failed to open COPC file {}", path.display()))?;
        let selection = BoundsSelection::Within(bounds);
        let iter = reader
            .points(LodSelection::All, selection)
            .with_context(|| format!("Failed to iterate COPC points from {}", path.display()))?;
        for point in iter {
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
            let extra = (top_height - surface) as u32;
            max_extra_height
                .entry((world_x, world_z))
                .and_modify(|value| {
                    if extra > *value {
                        *value = extra;
                    }
                })
                .or_insert(extra);
        }
    }

    let building_block: Arc<str> = Arc::from(DEFAULT_BUILDING_BLOCK);
    let subsurface_block: Arc<str> = Arc::from(DEFAULT_BUILDING_SUBSURFACE);
    let mut painted = 0usize;
    for ((world_x, world_z), extra) in max_extra_height {
        let chunk_x = world_x.div_euclid(SECTION_SIDE as i32);
        let chunk_z = world_z.div_euclid(SECTION_SIDE as i32);
        let Some(chunk) = chunks.get_mut(&(chunk_x, chunk_z)) else {
            continue;
        };
        let local_x = world_x.rem_euclid(SECTION_SIDE as i32) as usize;
        let local_z = world_z.rem_euclid(SECTION_SIDE as i32) as usize;
        let overlay = ColumnOverlay::new(
            COPC_LAYER_INDEX,
            COPC_OVERLAY_ORDER,
            None,
            Some(Arc::clone(&building_block)),
            Some(Arc::clone(&subsurface_block)),
            Some(1),
            Some(Arc::clone(&building_block)),
            Some(extra),
        );
        chunk.apply_overlay(local_x, local_z, overlay);
        painted += 1;
    }

    println!(
        "  {} Read {} point{} ({} building-classified), applied {} column{}",
        "✔".green().bold(),
        points_seen,
        if points_seen == 1 { "" } else { "s" },
        building_points,
        painted,
        if painted == 1 { "" } else { "s" }
    );

    Ok(painted)
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
