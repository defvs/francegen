use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use geo_types::Coord;
use serde::{Deserialize, Serialize};

use crate::constants::META_FILE;
use crate::world::WorldStats;

#[derive(Serialize, Deserialize)]
pub struct WorldMetadata {
    pub origin_model_x: f64,
    pub origin_model_z: f64,
    pub min_x: i32,
    pub max_x: i32,
    pub min_z: i32,
    pub max_z: i32,
    pub min_height: f64,
    pub max_height: f64,
}

impl WorldMetadata {
    pub fn from_stats(origin: Coord, stats: &WorldStats) -> Self {
        Self {
            origin_model_x: origin.x,
            origin_model_z: origin.y,
            min_x: stats.min_x,
            max_x: stats.max_x,
            min_z: stats.min_z,
            max_z: stats.max_z,
            min_height: stats.min_height,
            max_height: stats.max_height,
        }
    }

    pub fn to_stats(&self) -> WorldStats {
        let width = (self.max_x - self.min_x + 1).max(0) as usize;
        let depth = (self.max_z - self.min_z + 1).max(0) as usize;
        let center_x = (self.min_x + self.max_x) as f64 / 2.0;
        let center_z = (self.min_z + self.max_z) as f64 / 2.0;
        WorldStats {
            width,
            depth,
            min_height: self.min_height,
            max_height: self.max_height,
            min_x: self.min_x,
            max_x: self.max_x,
            min_z: self.min_z,
            max_z: self.max_z,
            center_x,
            center_z,
        }
    }
}

pub fn write_metadata(output: &Path, origin: Coord, stats: &WorldStats) -> Result<PathBuf> {
    let metadata = WorldMetadata::from_stats(origin, stats);
    let path = metadata_path(output);
    let json = serde_json::to_string_pretty(&metadata)?;
    fs::write(&path, json)
        .with_context(|| format!("Failed to write metadata {}", path.display()))?;
    Ok(path)
}

pub fn load_metadata(world: &Path) -> Result<WorldMetadata> {
    let meta_path = metadata_path(world);
    let data = fs::read_to_string(&meta_path)
        .with_context(|| format!("Failed to read metadata {}", meta_path.display()))?;
    let metadata = serde_json::from_str(&data)
        .with_context(|| format!("Failed to parse metadata {}", meta_path.display()))?;
    Ok(metadata)
}

pub fn metadata_path(base: &Path) -> PathBuf {
    if base.is_dir() {
        base.join(META_FILE)
    } else {
        base.to_path_buf()
    }
}
