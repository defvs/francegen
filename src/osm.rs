use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, PI};
use std::sync::Arc;

use anyhow::{Context, Result};
use geo::algorithm::bounding_rect::BoundingRect;
use geo::prelude::Contains;
use geo::{LineString, Point, Polygon};
use geo_types::Coord;
use owo_colors::OwoColorize;
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::chunk::{ChunkHeights, ColumnOverlay};
use crate::config::{OsmConfig, OsmGeometry, OsmLayer};
use crate::constants::SECTION_SIDE;
use crate::world::WorldStats;

const OVERPASS_TIMEOUT_SECONDS: u32 = 90;

pub fn apply_osm_overlays(
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
    osm: &OsmConfig,
    stats: &WorldStats,
    origin: Coord,
) -> Result<()> {
    if chunks.is_empty() || !osm.enabled() {
        return Ok(());
    }

    let lambert = Lambert93::new();
    let bbox = WorldBoundingBox::from_stats(stats, origin, osm.bbox_margin_m());
    let latlon_bounds = bbox.to_latlon(&lambert);
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
        .build()
        .context("Failed to build HTTP client for Overpass")?;

    for layer in osm.layers() {
        let query = build_query(layer, &bbox_param);
        println!(
            "{} Fetching layer '{}' via Overpass",
            "ℹ".blue().bold(),
            layer.name()
        );
        let response = client
            .post(osm.overpass_url())
            .form(&[("data", query.clone())])
            .send()
            .with_context(|| format!("Failed to query Overpass for layer '{}'", layer.name()))?;
        let status = response.status();
        let body = response
            .text()
            .with_context(|| format!("Failed to read Overpass body for '{}'", layer.name()))?;
        if !status.is_success() {
            anyhow::bail!(
                "Overpass request for '{}' returned {}. Body: {}",
                layer.name(),
                status,
                trim_preview(&body)
            );
        }
        let parsed: OverpassResponse = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse Overpass JSON for '{}'", layer.name()))?;
        let painted = rasterize_layer(layer, &parsed.elements, &lambert, origin, chunks);
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
    lambert: &Lambert93,
    origin: Coord,
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
) -> usize {
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
            let (x, y) = lambert.latlon_to_lambert(point.lat, point.lon);
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

    painted
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

struct Lambert93 {
    a: f64,
    e: f64,
    n: f64,
    c: f64,
    rho0: f64,
    lon0: f64,
    false_easting: f64,
    false_northing: f64,
}

impl Lambert93 {
    fn new() -> Self {
        const LAT1: f64 = 44.0_f64.to_radians();
        const LAT2: f64 = 49.0_f64.to_radians();
        const LAT0: f64 = 46.5_f64.to_radians();
        const A: f64 = 6_378_137.0; // GRS80 semi-major axis
        const F_INV: f64 = 298.257_222_101; // inverse flattening
        let f = 1.0 / F_INV;
        let e2 = 2.0 * f - f * f;
        let e = e2.sqrt();
        let m1 = Self::m(LAT1, e);
        let m2 = Self::m(LAT2, e);
        let t1 = Self::t(LAT1, e);
        let t2 = Self::t(LAT2, e);
        let t0 = Self::t(LAT0, e);
        let n = (m1.ln() - m2.ln()) / (t1.ln() - t2.ln());
        let c = m1 / (n * t1.powf(n));
        let rho0 = A * c * t0.powf(n);
        Self {
            a: A,
            e,
            n,
            c,
            rho0,
            lon0: 3.0_f64.to_radians(),
            false_easting: 700_000.0,
            false_northing: 6_600_000.0,
        }
    }

    fn latlon_to_lambert(&self, lat_deg: f64, lon_deg: f64) -> (f64, f64) {
        let lat = lat_deg.to_radians();
        let lon = lon_deg.to_radians();
        let t = Self::t(lat, self.e);
        let rho = self.a * self.c * t.powf(self.n);
        let theta = self.n * (lon - self.lon0);
        let x = self.false_easting + rho * theta.sin();
        let y = self.false_northing + self.rho0 - rho * theta.cos();
        (x, y)
    }

    fn lambert_to_latlon(&self, x: f64, y: f64) -> (f64, f64) {
        let dx = x - self.false_easting;
        let dy = self.rho0 - (y - self.false_northing);
        let rho = (dx * dx + dy * dy).sqrt();
        let t = (rho / (self.a * self.c)).powf(1.0 / self.n);
        let mut phi = FRAC_PI_2 - 2.0 * t.atan();
        for _ in 0..6 {
            let sin_phi = phi.sin();
            let term = ((1.0 + self.e * sin_phi) / (1.0 - self.e * sin_phi)).powf(self.e / 2.0);
            let next = FRAC_PI_2 - 2.0 * (t * term).atan();
            if (phi - next).abs() < 1e-12 {
                phi = next;
                break;
            }
            phi = next;
        }
        let theta = dx.atan2(dy);
        let lon = self.lon0 + theta / self.n;
        (phi.to_degrees(), lon.to_degrees())
    }

    fn m(lat: f64, e: f64) -> f64 {
        lat.cos() / (1.0 - e * e * lat.sin().powi(2)).sqrt()
    }

    fn t(lat: f64, e: f64) -> f64 {
        let sin_lat = lat.sin();
        let numerator = (1.0 - e * sin_lat) / (1.0 + e * sin_lat);
        ((PI / 4.0 - lat / 2.0).tan()) / numerator.powf(e / 2.0)
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

    fn to_latlon(&self, transform: &Lambert93) -> LatLonBounds {
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
            let (lat, lon) = transform.lambert_to_latlon(x, z);
            south = south.min(lat);
            north = north.max(lat);
            west = west.min(lon);
            east = east.max(lon);
        }
        LatLonBounds {
            south,
            north,
            west,
            east,
        }
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
