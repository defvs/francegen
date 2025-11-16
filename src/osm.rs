use core::time;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use geo::algorithm::bounding_rect::BoundingRect;
use geo::prelude::Contains;
use geo::{LineString, Point, Polygon};
use geo_types::Coord;
use owo_colors::OwoColorize;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::chunk::{ChunkHeights, ColumnOverlay};
use crate::config::{OsmConfig, OsmGeometry, OsmLayer};
use crate::constants::SECTION_SIDE;
use crate::geo_utils::{CoordinateTransformer, WorldBoundingBox};
use crate::world::WorldStats;

const OVERPASS_TIMEOUT_SECONDS: u32 = 90;
const OVERPASS_HTTP_TIMEOUT_SECONDS: u64 = 30;
const OVERPASS_MAX_RETRIES: usize = 100;
const OVERPASS_RETRY_WAIT_DURATION: Duration = time::Duration::from_secs(5);

pub fn apply_osm_overlays(
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
    osm: &OsmConfig,
    stats: &WorldStats,
    origin: Coord,
    cache_root: Option<&Path>,
) -> Result<()> {
    if chunks.is_empty() || !osm.enabled() {
        return Ok(());
    }

    let cache = match cache_root {
        Some(root) => Some(OverpassCache::prepare(root)?),
        None => None,
    };

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

    let mut client = build_overpass_client()?;

    for layer in osm.layers() {
        let query = build_query(layer, &bbox_param);
        let cached_body = match cache.as_ref() {
            Some(cache) => cache.load(layer.name(), &query)?,
            None => None,
        };
        let body = if let Some(entry) = cached_body {
            println!(
                "{} Using cached Overpass response for '{}' ({}).",
                "◎".blue(),
                layer.name(),
                entry.path.display()
            );
            entry.body
        } else {
            println!(
                "{} Fetching layer '{}' via Overpass",
                "ℹ".blue().bold(),
                layer.name()
            );
            let mut attempt = 0;
            let body = loop {
                attempt += 1;
                let response = match client
                    .post(osm.overpass_url())
                    .form(&[("data", query.clone())])
                    .send()
                {
                    Ok(response) => response,
                    Err(err) => {
                        if attempt < OVERPASS_MAX_RETRIES {
                            println!(
                                "  {} Overpass request failed for '{}', retrying ({}/{})",
                                "↻".yellow(),
                                layer.name(),
                                attempt,
                                OVERPASS_MAX_RETRIES
                            );
                            client = build_overpass_client()?;
                            thread::sleep(OVERPASS_RETRY_WAIT_DURATION);
                            continue;
                        } else {
                            return Err(err).with_context(|| {
                                format!("Failed to query Overpass for layer '{}'", layer.name())
                            });
                        }
                    }
                };
                let status = response.status();
                let body = response.text().with_context(|| {
                    format!("Failed to read Overpass body for '{}'", layer.name())
                })?;
                if status.is_success() {
                    break body;
                }
                if status != StatusCode::OK && attempt < OVERPASS_MAX_RETRIES {
                    println!(
                        "  {} Overpass timed out for '{}', retrying ({}/{})",
                        "↻".yellow(),
                        layer.name(),
                        attempt,
                        OVERPASS_MAX_RETRIES
                    );
                    client = build_overpass_client()?;
                    thread::sleep(OVERPASS_RETRY_WAIT_DURATION);
                    continue;
                }
                anyhow::bail!(
                    "Overpass request for '{}' returned {}. Body: {}",
                    layer.name(),
                    status,
                    trim_preview(&body)
                );
            };
            if let Some(cache) = cache.as_ref() {
                let path = cache.store(layer.name(), &query, &body)?;
                println!(
                    "  {} Cached Overpass response for '{}' at {}",
                    "◎".blue(),
                    layer.name(),
                    path.display()
                );
            }
            body
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

fn build_overpass_client() -> Result<Client> {
    Client::builder()
        .user_agent("francegen/0.1")
        .timeout(Duration::from_secs(OVERPASS_HTTP_TIMEOUT_SECONDS))
        .build()
        .context("Failed to build HTTP client for Overpass")
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

struct OverpassCache {
    dir: PathBuf,
}

impl OverpassCache {
    fn prepare(root: &Path) -> Result<Self> {
        let cache_root = root.join("overpass");
        fs::create_dir_all(&cache_root).with_context(|| {
            format!(
                "Failed to create Overpass cache directory {}",
                cache_root.display()
            )
        })?;
        Ok(Self { dir: cache_root })
    }

    fn load(&self, layer: &str, query: &str) -> Result<Option<CachedResponse>> {
        let path = self.entry_path(layer, query);
        if !path.exists() {
            return Ok(None);
        }
        let body = fs::read_to_string(&path).with_context(|| {
            format!("Failed to read cached Overpass response {}", path.display())
        })?;
        Ok(Some(CachedResponse { body, path }))
    }

    fn store(&self, layer: &str, query: &str, body: &str) -> Result<PathBuf> {
        let path = self.entry_path(layer, query);
        fs::write(&path, body).with_context(|| {
            format!(
                "Failed to write cached Overpass response {}",
                path.display()
            )
        })?;
        Ok(path)
    }

    fn entry_path(&self, layer: &str, query: &str) -> PathBuf {
        let mut name = sanitize_for_filename(layer);
        name.push('_');
        name.push_str(&hash_query(layer, query));
        name.push_str(".json");
        self.dir.join(name)
    }
}

struct CachedResponse {
    body: String,
    path: PathBuf,
}

fn hash_query(layer: &str, query: &str) -> String {
    let mut hasher = DefaultHasher::new();
    layer.hash(&mut hasher);
    query.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn sanitize_for_filename(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}
