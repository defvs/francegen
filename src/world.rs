use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use geo_types::Coord;

use crate::chunk::ChunkHeights;
use crate::constants::{BEDROCK_Y, MAX_WORLD_Y, SECTION_SIDE};
use crate::georaster::GeoRaster;

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
    pub fn new() -> Self {
        Self {
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

    pub fn into_chunks(self) -> HashMap<(i32, i32), ChunkHeights> {
        let mut chunks: HashMap<(i32, i32), ChunkHeights> = HashMap::new();
        for ((x, z), height) in self.columns {
            let chunk_x = x.div_euclid(SECTION_SIDE as i32);
            let chunk_z = z.div_euclid(SECTION_SIDE as i32);
            let local_x = x.rem_euclid(SECTION_SIDE as i32) as usize;
            let local_z = z.rem_euclid(SECTION_SIDE as i32) as usize;
            let entry = chunks
                .entry((chunk_x, chunk_z))
                .or_insert_with(ChunkHeights::new);
            entry.set(local_x, local_z, height);
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
                self.samples += 1;
                let coord = raster.coord_for(col, row);
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

fn model_to_world(origin: &Coord, coord: &Coord) -> (i32, i32) {
    let dx = coord.x - origin.x;
    let dz = origin.y - coord.y;
    (dx.round() as i32, dz.round() as i32)
}

pub fn dem_to_minecraft(value: f64) -> i32 {
    let height = BEDROCK_Y as f64 + value;
    height.round().clamp(BEDROCK_Y as f64, MAX_WORLD_Y as f64) as i32
}
