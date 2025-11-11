use anyhow::{Context, Result, anyhow};
use geo_types::Coord;
use proj::Proj;

use crate::world::WorldStats;

/// Bidirectional transformer between Lambert93 (EPSG:2154) and WGS84 lat/lon (EPSG:4326).
pub struct CoordinateTransformer {
    to_latlon: Proj,
    to_lambert: Proj,
}

impl CoordinateTransformer {
    pub fn new() -> Result<Self> {
        let to_latlon = Proj::new_known_crs("EPSG:2154", "EPSG:4326", None)
            .context("Failed to build EPSG:2154 → EPSG:4326 transform")?;
        let to_lambert = Proj::new_known_crs("EPSG:4326", "EPSG:2154", None)
            .context("Failed to build EPSG:4326 → EPSG:2154 transform")?;
        Ok(Self {
            to_latlon,
            to_lambert,
        })
    }

    pub fn lambert_to_latlon(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let coord = self
            .to_latlon
            .convert((x, y))
            .map_err(|err| anyhow!("Lambert93 → WGS84 transform failed: {err}"))?;
        Ok((coord.1, coord.0))
    }

    pub fn latlon_to_lambert(&self, lat: f64, lon: f64) -> Result<(f64, f64)> {
        let coord = self
            .to_lambert
            .convert((lon, lat))
            .map_err(|err| anyhow!("WGS84 → Lambert93 transform failed: {err}"))?;
        Ok((coord.0, coord.1))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WorldBoundingBox {
    pub min_x: f64,
    pub max_x: f64,
    pub min_z: f64,
    pub max_z: f64,
}

impl WorldBoundingBox {
    pub fn from_stats(stats: &WorldStats, origin: Coord, margin: f64) -> Self {
        let margin = margin.max(0.0);
        let min_x = origin.x + stats.min_x as f64 - margin;
        let max_x = origin.x + stats.max_x as f64 + margin;
        let min_z = origin.y - stats.max_z as f64 - margin;
        let max_z = origin.y - stats.min_z as f64 + margin;
        Self {
            min_x,
            max_x,
            min_z,
            max_z,
        }
    }

    pub fn lambert_corners(&self) -> [(f64, f64); 4] {
        [
            (self.min_x, self.min_z),
            (self.min_x, self.max_z),
            (self.max_x, self.min_z),
            (self.max_x, self.max_z),
        ]
    }

    pub fn to_latlon(&self, transform: &CoordinateTransformer) -> Result<LatLonBounds> {
        let mut south = f64::INFINITY;
        let mut north = f64::NEG_INFINITY;
        let mut west = f64::INFINITY;
        let mut east = f64::NEG_INFINITY;
        for (x, z) in self.lambert_corners() {
            let (lat, lon) = transform.lambert_to_latlon(x, z)?;
            south = south.min(lat);
            north = north.max(lat);
            west = west.min(lon);
            east = east.max(lon);
        }
        Ok(LatLonBounds {
            south,
            north,
            west,
            east,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LatLonBounds {
    pub south: f64,
    pub north: f64,
    pub west: f64,
    pub east: f64,
}

impl LatLonBounds {
    pub fn to_overpass_bbox(&self) -> String {
        format!(
            "{:.7},{:.7},{:.7},{:.7}",
            self.south, self.west, self.north, self.east
        )
    }
}
