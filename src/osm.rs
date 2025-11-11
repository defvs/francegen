use core::time;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use geo::algorithm::bounding_rect::BoundingRect;
use geo::prelude::Contains;
use geo::{LineString, Point, Polygon};
use geo_types::Coord;
use owo_colors::OwoColorize;
use proj::Proj;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::chunk::{ChunkHeights, ColumnOverlay};
use crate::config::{OsmConfig, OsmGeometry, OsmLayer};
use crate::constants::SECTION_SIDE;
use crate::world::WorldStats;

const OVERPASS_TIMEOUT_SECONDS: u32 = 90;
const OVERPASS_HTTP_TIMEOUT_SECONDS: u64 = 30;
const OVERPASS_MAX_RETRIES: usize = 3;

pub fn apply_osm_overlays(
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
    osm: &OsmConfig,
    stats: &WorldStats,
    origin: Coord,
) -> Result<()> {
    if chunks.is_empty() || !osm.enabled() {
        return Ok(());
    }

    let transform = CoordinateTransformer::new()?;
    let bbox = WorldBoundingBox::from_stats(stats, origin, osm.bbox_margin_m());
    let latlon_bounds = bbox.to_latlon(&transform)?;
    let bbox_param = latlon_bounds.to_overpass_bbox();
    println!(
        "{} OSM bbox (Lambert93): X:[{:.3}..{:.3}] Z:[{:.3}..{:.3}]",
        "ℹ".blue().bold(),
        bbox.min_x,
        bbox.max_x,
        bbox.min_z,
        bbox.max_z
    );
    println!(
        "  {} OSM bbox (lat/lon): south {:.6}, west {:.6}, north {:.6}, east {:.6}",
        "◎".blue(),
        latlon_bounds.south,
        latlon_bounds.west,
        latlon_bounds.north,
        latlon_bounds.east
    );

    let client = Client::builder()
        .user_agent("francegen/0.1")
        .timeout(Duration::from_secs(OVERPASS_HTTP_TIMEOUT_SECONDS))
        .build()
        .context("Failed to build HTTP client for Overpass")?;

    for layer in osm.layers() {
        let query = build_query(layer, &bbox_param);
        println!(
            "{} Fetching layer '{}' via Overpass",
            "ℹ".blue().bold(),
            layer.name()
        );
        let mut attempt = 0;
        let body = loop {
            attempt += 1;
            let response = client
                .post(osm.overpass_url())
                .form(&[("data", query.clone())])
                .send()
                .with_context(|| {
                    format!("Failed to query Overpass for layer '{}'", layer.name())
                })?;
            let status = response.status();
            let body = response
                .text()
                .with_context(|| format!("Failed to read Overpass body for '{}'", layer.name()))?;
            if status.is_success() {
                break body;
            }
            if status == StatusCode::GATEWAY_TIMEOUT && attempt < OVERPASS_MAX_RETRIES {
                println!(
                    "  {} Overpass timed out for '{}', retrying ({}/{})",
                    "↻".yellow(),
                    layer.name(),
                    attempt,
                    OVERPASS_MAX_RETRIES
                );
                thread::sleep(time::Duration::from_secs(5));
                continue;
            }
            anyhow::bail!(
                "Overpass request for '{}' returned {}. Body: {}",
                layer.name(),
                status,
                trim_preview(&body)
            );
        };
        let parsed: OverpassResponse = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse Overpass JSON for '{}'", layer.name()))?;
        let painted = rasterize_layer(layer, &parsed.elements, &transform, origin, chunks)?;
        println!(
            "  {} Applied {} overlay column{}",
            "✔".green().bold(),
            painted,
            if painted == 1 { "" } else { "s" }
        );
    }

    Ok(())
}

fn build_query(layer: &OsmLayer, bbox_param: &str) -> String {
    let mut body = if layer.query().contains("{{bbox}}") {
        layer.query().replace("{{bbox}}", bbox_param)
    } else {
        layer.query().to_string()
    };
    body = body.trim().to_string();
    if !body.ends_with(';') {
        body.push(';');
    }
    format!(
        "[out:json][timeout:{OVERPASS_TIMEOUT_SECONDS}];{body}out geom;",
        body = body
    )
}

fn rasterize_layer(
    layer: &OsmLayer,
    elements: &[OverpassElement],
    transform: &CoordinateTransformer,
    origin: Coord,
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
) -> Result<usize> {
    let overlay = ColumnOverlay::new(
        layer.priority(),
        layer.style().biome().map(|value| Arc::clone(value)),
        layer.style().surface_block().map(|value| Arc::clone(value)),
        layer
            .style()
            .subsurface_block()
            .map(|value| Arc::clone(value)),
        layer.style().top_thickness(),
    );

    let mut painted = 0usize;
    for element in elements {
        let geometry = match &element.geometry {
            Some(geom) if geom.len() >= 2 => geom,
            _ => continue,
        };
        let mut path: Vec<(i32, i32)> = Vec::with_capacity(geometry.len());
        for point in geometry {
            let (x, y) = transform.latlon_to_lambert(point.lat, point.lon)?;
            let world_x = (x - origin.x).round() as i32;
            let world_z = (origin.y - y).round() as i32;
            path.push((world_x, world_z));
        }

        match layer.geometry() {
            OsmGeometry::Line => {
                painted += rasterize_line(&path, layer.width_m(), &overlay, chunks);
            }
            OsmGeometry::Polygon => {
                painted += rasterize_polygon(&path, &overlay, chunks);
            }
        }
    }

    Ok(painted)
}

fn rasterize_line(
    path: &[(i32, i32)],
    width_m: f64,
    overlay: &ColumnOverlay,
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
) -> usize {
    if path.len() < 2 {
        return 0;
    }
    let radius = (width_m / 2.0).ceil().max(1.0) as i32;
    let mut painted = 0usize;
    for segment in path.windows(2) {
        let (x0, z0) = segment[0];
        let (x1, z1) = segment[1];
        let steps = (x1 - x0).abs().max((z1 - z0).abs()).max(1);
        for step in 0..=steps {
            let t = step as f64 / steps as f64;
            let x = x0 as f64 + (x1 - x0) as f64 * t;
            let z = z0 as f64 + (z1 - z0) as f64 * t;
            painted += paint_disk(x.round() as i32, z.round() as i32, radius, overlay, chunks);
        }
    }
    painted
}

fn paint_disk(
    center_x: i32,
    center_z: i32,
    radius: i32,
    overlay: &ColumnOverlay,
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
) -> usize {
    let mut painted = 0usize;
    let r_sq = (radius * radius) as i32;
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dz * dz > r_sq {
                continue;
            }
            if apply_overlay_column(center_x + dx, center_z + dz, overlay, chunks) {
                painted += 1;
            }
        }
    }
    painted
}

fn rasterize_polygon(
    path: &[(i32, i32)],
    overlay: &ColumnOverlay,
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
) -> usize {
    if path.len() < 3 {
        return 0;
    }
    let mut coords: Vec<(f64, f64)> = path.iter().map(|(x, z)| (*x as f64, *z as f64)).collect();
    if coords.first() != coords.last() {
        if let Some(first) = coords.first().copied() {
            coords.push(first);
        }
    }
    let polygon: Polygon = Polygon::new(LineString::from(coords), vec![]);
    let Some(rect) = polygon.bounding_rect() else {
        return 0;
    };
    let min_x = rect.min().x.floor() as i32;
    let max_x = rect.max().x.ceil() as i32;
    let min_z = rect.min().y.floor() as i32;
    let max_z = rect.max().y.ceil() as i32;
    let mut painted = 0usize;
    for z in min_z..=max_z {
        for x in min_x..=max_x {
            let point = Point::new(x as f64 + 0.5, z as f64 + 0.5);
            if polygon.contains(&point) {
                if apply_overlay_column(x, z, overlay, chunks) {
                    painted += 1;
                }
            }
        }
    }
    painted
}

fn apply_overlay_column(
    x: i32,
    z: i32,
    overlay: &ColumnOverlay,
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
) -> bool {
    let chunk_x = x.div_euclid(SECTION_SIDE as i32);
    let chunk_z = z.div_euclid(SECTION_SIDE as i32);
    if let Some(chunk) = chunks.get_mut(&(chunk_x, chunk_z)) {
        let local_x = x.rem_euclid(SECTION_SIDE as i32) as usize;
        let local_z = z.rem_euclid(SECTION_SIDE as i32) as usize;
        chunk.apply_overlay(local_x, local_z, overlay.clone());
        return true;
    }
    false
}

struct CoordinateTransformer {
    to_latlon: Proj,
    to_lambert: Proj,
}

impl CoordinateTransformer {
    fn new() -> Result<Self> {
        let to_latlon = Proj::new_known_crs("EPSG:2154", "EPSG:4326", None)
            .context("Failed to build EPSG:2154 → EPSG:4326 transform")?;
        let to_lambert = Proj::new_known_crs("EPSG:4326", "EPSG:2154", None)
            .context("Failed to build EPSG:4326 → EPSG:2154 transform")?;
        Ok(Self {
            to_latlon,
            to_lambert,
        })
    }

    fn lambert_to_latlon(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let coord = self
            .to_latlon
            .convert((x, y))
            .map_err(|err| anyhow!("Lambert93 → WGS84 transform failed: {err}"))?;
        Ok((coord.1, coord.0))
    }

    fn latlon_to_lambert(&self, lat: f64, lon: f64) -> Result<(f64, f64)> {
        let coord = self
            .to_lambert
            .convert((lon, lat))
            .map_err(|err| anyhow!("WGS84 → Lambert93 transform failed: {err}"))?;
        Ok((coord.0, coord.1))
    }
}

struct WorldBoundingBox {
    min_x: f64,
    max_x: f64,
    min_z: f64,
    max_z: f64,
}

impl WorldBoundingBox {
    fn from_stats(stats: &WorldStats, origin: Coord, margin: f64) -> Self {
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

    fn to_latlon(&self, transform: &CoordinateTransformer) -> Result<LatLonBounds> {
        let corners = [
            (self.min_x, self.min_z),
            (self.min_x, self.max_z),
            (self.max_x, self.min_z),
            (self.max_x, self.max_z),
        ];
        let mut south = f64::INFINITY;
        let mut north = f64::NEG_INFINITY;
        let mut west = f64::INFINITY;
        let mut east = f64::NEG_INFINITY;
        for (x, z) in corners {
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

struct LatLonBounds {
    south: f64,
    north: f64,
    west: f64,
    east: f64,
}

impl LatLonBounds {
    fn to_overpass_bbox(&self) -> String {
        format!(
            "{:.7},{:.7},{:.7},{:.7}",
            self.south, self.west, self.north, self.east
        )
    }
}

#[derive(Debug, Deserialize)]
struct OverpassResponse {
    #[serde(default)]
    elements: Vec<OverpassElement>,
}

#[derive(Debug, Deserialize)]
struct OverpassElement {
    #[serde(default)]
    geometry: Option<Vec<OverpassPoint>>,
}

#[derive(Debug, Deserialize)]
struct OverpassPoint {
    lat: f64,
    lon: f64,
}

fn trim_preview(body: &str) -> String {
    const LIMIT: usize = 600;
    if body.len() <= LIMIT {
        body.trim().to_string()
    } else {
        format!("{}…", body[..LIMIT].trim())
    }
}
