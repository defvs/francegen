use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use geo_types::Coord;
use rayon::prelude::*;

use crate::chunk::{ChunkHeights, SlopeProfile, SlopeStats};
use crate::constants::{BEDROCK_Y, MAX_WORLD_Y, SECTION_SIDE};
use crate::georaster::GeoRaster;
use crate::progress::progress_bar;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelBounds {
    pub min_x: f64,
    pub max_x: f64,
    pub min_z: f64,
    pub max_z: f64,
}

impl ModelBounds {
    pub fn contains(&self, coord: &Coord) -> bool {
        coord.x >= self.min_x
            && coord.x <= self.max_x
            && coord.y >= self.min_z
            && coord.y <= self.max_z
    }
}

#[derive(Clone)]
pub struct WorldStats {
    pub width: usize,
    pub depth: usize,
    pub min_height: f64,
    pub max_height: f64,
    pub min_x: i32,
    pub max_x: i32,
    pub min_z: i32,
    pub max_z: i32,
    pub center_x: f64,
    pub center_z: f64,
}

pub struct WorldBuilder {
    bounds: Option<ModelBounds>,
    origin: Option<Coord>,
    columns: HashMap<(i32, i32), i32>,
    samples: usize,
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
    min_height: f64,
    max_height: f64,
}

impl WorldBuilder {
    pub fn new(bounds: Option<ModelBounds>) -> Self {
        Self {
            bounds,
            origin: None,
            columns: HashMap::new(),
            samples: 0,
            min_x: i32::MAX,
            max_x: i32::MIN,
            min_z: i32::MAX,
            max_z: i32::MIN,
            min_height: f64::INFINITY,
            max_height: f64::NEG_INFINITY,
        }
    }

    pub fn set_origin(&mut self, origin: Coord) {
        self.origin = Some(origin);
    }

    pub fn sample_count(&self) -> usize {
        self.samples
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn ingest(&mut self, path: &Path) -> Result<()> {
        let raster = GeoRaster::open(path)?;
        if self.origin.is_none() {
            let origin = raster.origin();
            self.origin = Some(origin);
            println!(
                "Using GeoTIFF origin ({:.3}, {:.3}) as world (0,0)",
                origin.x, origin.y
            );
        }
        self.ingest_raster(&raster);
        Ok(())
    }

    pub fn stats(&self) -> Option<WorldStats> {
        if self.columns.is_empty() {
            return None;
        }
        let width = (self.max_x - self.min_x + 1).max(0) as usize;
        let depth = (self.max_z - self.min_z + 1).max(0) as usize;
        let center_x = (self.min_x + self.max_x) as f64 / 2.0;
        let center_z = (self.min_z + self.max_z) as f64 / 2.0;
        Some(WorldStats {
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
        })
    }

    pub fn origin_coord(&self) -> Option<Coord> {
        self.origin
    }

    pub fn into_chunks(self, max_smoothing_radius: u32) -> HashMap<(i32, i32), ChunkHeights> {
        let columns = Arc::new(self.columns);
        let total = columns.len();
        let chunk_pb = Arc::new(progress_bar(total as u64, "Generating chunk data"));
        let radius = max_smoothing_radius as i32;
        let smoothing_radius = max_smoothing_radius as usize;

        struct ChunkWork {
            chunk_coords: (i32, i32),
            local_x: usize,
            local_z: usize,
            height: i32,
            slope_profile: SlopeProfile,
        }

        let work: Vec<ChunkWork> = columns
            .par_iter()
            .map(|(&(x, z), &height)| {
                let chunk_x = x.div_euclid(SECTION_SIDE as i32);
                let chunk_z = z.div_euclid(SECTION_SIDE as i32);
                let local_x = x.rem_euclid(SECTION_SIDE as i32) as usize;
                let local_z = z.rem_euclid(SECTION_SIDE as i32) as usize;
                let slope_profile = slope_profile_for(x, z, height, columns.as_ref(), radius);
                chunk_pb.inc(1);
                ChunkWork {
                    chunk_coords: (chunk_x, chunk_z),
                    local_x,
                    local_z,
                    height,
                    slope_profile,
                }
            })
            .collect();

        chunk_pb.finish_and_clear();

        let mut chunks: HashMap<(i32, i32), ChunkHeights> = HashMap::new();
        for work_item in work {
            let ChunkWork {
                chunk_coords,
                local_x,
                local_z,
                height,
                slope_profile,
            } = work_item;
            let entry = chunks
                .entry(chunk_coords)
                .or_insert_with(|| ChunkHeights::new(smoothing_radius));
            entry.set(local_x, local_z, height, slope_profile);
        }
        chunks
    }

    fn ingest_raster(&mut self, raster: &GeoRaster) {
        let origin = self.origin.expect("origin initialized");
        for row in 0..raster.height() {
            for col in 0..raster.width() {
                let Some(height_value) = raster.sample(col, row) else {
                    continue;
                };
                let coord = raster.coord_for(col, row);
                if let Some(bounds) = self.bounds {
                    if !bounds.contains(&coord) {
                        continue;
                    }
                }
                self.samples += 1;
                let (world_x, world_z) = model_to_world(&origin, &coord);
                let mc_height = dem_to_minecraft(height_value);
                self.columns.insert((world_x, world_z), mc_height);
                self.update_stats(world_x, world_z, height_value);
            }
        }
    }

    fn update_stats(&mut self, x: i32, z: i32, height_value: f64) {
        self.min_x = self.min_x.min(x);
        self.max_x = self.max_x.max(x);
        self.min_z = self.min_z.min(z);
        self.max_z = self.max_z.max(z);
        self.min_height = self.min_height.min(height_value);
        self.max_height = self.max_height.max(height_value);
    }
}

impl WorldStats {
    pub fn union(&self, other: &WorldStats) -> WorldStats {
        let min_x = self.min_x.min(other.min_x);
        let max_x = self.max_x.max(other.max_x);
        let min_z = self.min_z.min(other.min_z);
        let max_z = self.max_z.max(other.max_z);
        let min_height = self.min_height.min(other.min_height);
        let max_height = self.max_height.max(other.max_height);
        let width = (max_x - min_x + 1).max(0) as usize;
        let depth = (max_z - min_z + 1).max(0) as usize;
        let center_x = (min_x + max_x) as f64 / 2.0;
        let center_z = (min_z + max_z) as f64 / 2.0;
        WorldStats {
            width,
            depth,
            min_height,
            max_height,
            min_x,
            max_x,
            min_z,
            max_z,
            center_x,
            center_z,
        }
    }
}

fn model_to_world(origin: &Coord, coord: &Coord) -> (i32, i32) {
    let dx = coord.x - origin.x;
    let dz = origin.y - coord.y;
    (dx.round() as i32, dz.round() as i32)
}

fn slope_profile_for(
    x: i32,
    z: i32,
    height: i32,
    columns: &HashMap<(i32, i32), i32>,
    max_radius: i32,
) -> SlopeProfile {
    if max_radius <= 0 {
        return SlopeProfile::empty(0);
    }
    let mut stats = Vec::with_capacity(max_radius as usize);
    let mut max_angle = 0.0f32;
    let mut weighted_sum = 0.0f64;
    let mut weight_total = 0.0f64;
    for radius in 1..=max_radius {
        let r = radius as i32;
        for dz in -r..=r {
            for dx in -r..=r {
                if dx == 0 && dz == 0 {
                    continue;
                }
                if dx.abs().max(dz.abs()) != r {
                    continue;
                }
                if let Some(neighbor_height) = columns.get(&(x + dx, z + dz)) {
                    let horizontal = ((dx * dx + dz * dz) as f64).sqrt();
                    if horizontal == 0.0 {
                        continue;
                    }
                    let diff = (height - *neighbor_height).abs() as f64;
                    let angle = (diff / horizontal).atan().to_degrees() as f32;
                    if angle > max_angle {
                        max_angle = angle;
                    }
                    let weight = 1.0 / horizontal;
                    weighted_sum += angle as f64 * weight;
                    weight_total += weight;
                }
            }
        }
        let weighted_average = if weight_total > 0.0 {
            (weighted_sum / weight_total) as f32
        } else {
            0.0
        };
        stats.push(SlopeStats {
            max_angle,
            weighted_average,
        });
    }
    SlopeProfile::from_stats(stats)
}

pub fn dem_to_minecraft(value: f64) -> i32 {
    let height = BEDROCK_Y as f64 + value;
    height.round().clamp(BEDROCK_Y as f64, MAX_WORLD_Y as f64) as i32
}
