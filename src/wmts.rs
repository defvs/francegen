use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use geo_types::Coord;
use image::{self, RgbaImage};
use owo_colors::OwoColorize;
use proj::Proj;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use roxmltree::{Document, Node};
use urlencoding::encode;

use crate::chunk::{ChunkHeights, ColumnOverlay};
use crate::config::{WmtsColorRule, WmtsConfig};
use crate::constants::SECTION_SIDE;
use crate::geo_utils::{CoordinateTransformer, WorldBoundingBox};
use crate::progress::progress_bar;
use crate::world::WorldStats;

const WMTS_HTTP_TIMEOUT_SECONDS: u64 = 30;
const WMTS_FETCH_RETRIES: usize = 2;
const REQUEST_VERSION: &str = "1.0.0";

pub struct WmtsCacheDir {
    root: PathBuf,
    auto_cleanup: bool,
}

impl WmtsCacheDir {
    pub fn prepare(explicit: Option<PathBuf>) -> Result<Self> {
        match explicit {
            Some(path) => {
                if path.exists() && !path.is_dir() {
                    bail!(
                        "WMTS cache path {} exists and is not a directory",
                        path.display()
                    );
                }
                fs::create_dir_all(&path).with_context(|| {
                    format!("Failed to create WMTS cache dir {}", path.display())
                })?;
                Ok(Self {
                    root: path,
                    auto_cleanup: false,
                })
            }
            None => {
                let mut base = std::env::temp_dir();
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let pid = std::process::id();
                base.push(format!("francegen-wmts-{}-{}", pid, timestamp));
                fs::create_dir_all(&base).with_context(|| {
                    format!(
                        "Failed to create temporary WMTS cache dir {}",
                        base.display()
                    )
                })?;
                Ok(Self {
                    root: base,
                    auto_cleanup: true,
                })
            }
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn auto_cleanup(&self) -> bool {
        self.auto_cleanup
    }

    pub fn tile_path(
        &self,
        layer: &str,
        tile_matrix: &str,
        row: u32,
        col: u32,
        extension: &str,
    ) -> PathBuf {
        let mut name = sanitize_for_filename(layer);
        name.push('_');
        name.push_str(&sanitize_for_filename(tile_matrix));
        name.push('_');
        name.push_str(&row.to_string());
        name.push('_');
        name.push_str(&col.to_string());
        name.push('.');
        name.push_str(extension);
        self.root.join(name)
    }

    pub fn cleanup(&self) -> Result<()> {
        if self.auto_cleanup && self.root.exists() {
            fs::remove_dir_all(&self.root).with_context(|| {
                format!("Failed to remove WMTS cache dir {}", self.root.display())
            })?;
        }
        Ok(())
    }
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

pub fn apply_wmts_overlays(
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
    config: &WmtsConfig,
    stats: &WorldStats,
    origin: Coord,
    cache: &WmtsCacheDir,
    order_offset: u32,
) -> Result<()> {
    if chunks.is_empty() || !config.enabled() {
        return Ok(());
    }

    let transformer = CoordinateTransformer::new()?;
    let bbox = WorldBoundingBox::from_stats(stats, origin, config.bbox_margin_m());
    let latlon = bbox.to_latlon(&transformer)?;
    println!(
        "{} WMTS bbox (Lambert93): X:[{:.3}..{:.3}] Z:[{:.3}..{:.3}]",
        "ℹ".blue().bold(),
        bbox.min_x,
        bbox.max_x,
        bbox.min_z,
        bbox.max_z
    );
    println!(
        "  {} WMTS bbox (lat/lon): south {:.6}, west {:.6}, north {:.6}, east {:.6}",
        "◎".blue(),
        latlon.south,
        latlon.west,
        latlon.north,
        latlon.east
    );
    println!(
        "  {} WMTS cache directory: {}{}",
        "◎".blue(),
        cache.root().display(),
        if cache.auto_cleanup() {
            " (temporary)"
        } else {
            ""
        }
    );

    let capabilities_xml = fetch_capabilities_document(config.capabilities_url())?;
    let capabilities = parse_capabilities(&capabilities_xml, config)?;
    let style_id = config
        .style_id()
        .map(|s| s.to_string())
        .or_else(|| capabilities.layer.default_style.clone())
        .ok_or_else(|| {
            anyhow!(
                "Layer '{}' does not provide a default style; set wmts.style_id",
                config.layer()
            )
        })?;
    if let Some(requested) = config.style_id() {
        if !capabilities.layer.styles.iter().any(|s| s == requested) {
            bail!(
                "Layer '{}' does not expose style '{}'",
                config.layer(),
                requested
            );
        }
    }
    if !capabilities
        .layer
        .formats
        .iter()
        .any(|fmt| fmt.eq_ignore_ascii_case(config.format()))
    {
        bail!(
            "Layer '{}' does not list format '{}'",
            config.layer(),
            config.format()
        );
    }

    let matrix = capabilities
        .matrix_set
        .matrices
        .get(config.tile_matrix())
        .ok_or_else(|| {
            anyhow!(
                "Tile matrix '{}' not found in set '{}'",
                config.tile_matrix(),
                config.tile_matrix_set()
            )
        })?;
    let limits = capabilities.layer.limits.get(config.tile_matrix()).copied();

    let normalized_crs = capabilities.matrix_set.supported_crs.clone();
    let lambert_to_tile = Proj::new_known_crs("EPSG:2154", &normalized_crs, None)
        .with_context(|| format!("Failed to build EPSG:2154 → {} transform", normalized_crs))?;

    let coverage = compute_tile_coverage(&bbox, &lambert_to_tile, matrix, limits)?;
    if coverage.tiles.is_empty() {
        println!(
            "  {} No WMTS tiles overlap the requested area",
            "⚠".yellow().bold()
        );
        return Ok(());
    }
    if coverage.tiles.len() as u32 > config.max_tiles() {
        bail!(
            "WMTS would require {} tiles at matrix {}, exceeding wmts.max_tiles ({})",
            coverage.tiles.len(),
            config.tile_matrix(),
            config.max_tiles()
        );
    }

    println!(
        "  {} Prefetching {} WMTS tiles (matrix {})",
        "ℹ".blue().bold(),
        coverage.tiles.len(),
        config.tile_matrix()
    );

    let extension = extension_for_format(config.format())?;
    prefetch_tiles(
        &coverage.tiles,
        &capabilities.get_tile_url,
        config,
        &style_id,
        extension,
        cache,
    )?;

    let tile_images = load_tiles(&coverage.tiles, cache, config, extension)?;
    let prepared_rules = prepare_rules(config.colors(), order_offset);
    let tile_lookup = TileLookup {
        coverage,
        matrix,
        lambert_to_tile,
    };

    let painted =
        apply_tiles_to_chunks(chunks, origin, &tile_lookup, &tile_images, &prepared_rules)?;

    println!(
        "  {} Applied WMTS overlays to {} column{}",
        "✔".green().bold(),
        painted,
        if painted == 1 { "" } else { "s" }
    );

    Ok(())
}

struct PreparedRule<'a> {
    rule: &'a WmtsColorRule,
    overlay: ColumnOverlay,
}

fn prepare_rules(rules: &[WmtsColorRule], order_offset: u32) -> Vec<PreparedRule<'_>> {
    rules
        .iter()
        .map(|rule| {
            let layer_index = rule.layer_index().unwrap_or(0);
            let order = order_offset.saturating_add(rule.original_order());
            PreparedRule {
                rule,
                overlay: ColumnOverlay::new(
                    layer_index,
                    order,
                    rule.style().biome().map(|value| Arc::clone(value)),
                    rule.style().surface_block().map(|value| Arc::clone(value)),
                    rule.style()
                        .subsurface_block()
                        .map(|value| Arc::clone(value)),
                    rule.style().top_thickness(),
                ),
            }
        })
        .collect()
}

fn apply_tiles_to_chunks(
    chunks: &mut HashMap<(i32, i32), ChunkHeights>,
    origin: Coord,
    lookup: &TileLookup,
    tiles: &HashMap<(u32, u32), Arc<RgbaImage>>,
    rules: &[PreparedRule<'_>],
) -> Result<usize> {
    let mut painted = 0usize;
    for (&(chunk_x, chunk_z), chunk) in chunks.iter_mut() {
        for local_z in 0..SECTION_SIDE {
            for local_x in 0..SECTION_SIDE {
                if chunk.column(local_x, local_z).is_none() {
                    continue;
                }
                let world_x = chunk_x * SECTION_SIDE as i32 + local_x as i32;
                let world_z = chunk_z * SECTION_SIDE as i32 + local_z as i32;
                if let Some(sample) = lookup.sample_column(origin, world_x, world_z) {
                    let key = (sample.row, sample.col);
                    let Some(image) = tiles.get(&key) else {
                        continue;
                    };
                    if sample.pixel_x >= image.width() as usize
                        || sample.pixel_y >= image.height() as usize
                    {
                        continue;
                    }
                    let rgba = image
                        .get_pixel(sample.pixel_x as u32, sample.pixel_y as u32)
                        .0;
                    for prepared in rules {
                        if prepared.rule.matches(rgba) {
                            chunk.apply_overlay(local_x, local_z, prepared.overlay.clone());
                            painted += 1;
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(painted)
}

struct TileLookup<'a> {
    coverage: TileCoverage,
    matrix: &'a TileMatrix,
    lambert_to_tile: Proj,
}

impl<'a> TileLookup<'a> {
    fn sample_column(&self, origin: Coord, world_x: i32, world_z: i32) -> Option<ColumnSample> {
        let lambert_x = origin.x + world_x as f64;
        let lambert_z = origin.y - world_z as f64;
        let Ok((tile_x, tile_y)) = self.lambert_to_tile.convert((lambert_x, lambert_z)) else {
            return None;
        };
        locate_tile_pixel(tile_x, tile_y, self.matrix)
            .filter(|sample| self.coverage.contains(sample.col, sample.row))
    }
}

struct ColumnSample {
    col: u32,
    row: u32,
    pixel_x: usize,
    pixel_y: usize,
}

fn locate_tile_pixel(tile_x: f64, tile_y: f64, matrix: &TileMatrix) -> Option<ColumnSample> {
    let resolution = matrix.resolution();
    let pixel_x = (tile_x - matrix.top_left_x) / resolution;
    let pixel_y = (matrix.top_left_y - tile_y) / resolution;
    if pixel_x.is_nan() || pixel_y.is_nan() {
        return None;
    }
    if pixel_x < 0.0 || pixel_y < 0.0 {
        return None;
    }
    let tile_width = matrix.tile_width as f64;
    let tile_height = matrix.tile_height as f64;
    let col = (pixel_x / tile_width).floor();
    let row = (pixel_y / tile_height).floor();
    if col < 0.0 || row < 0.0 {
        return None;
    }
    if col >= matrix.matrix_width as f64 || row >= matrix.matrix_height as f64 {
        return None;
    }
    let mut px = (pixel_x - col * tile_width).floor() as i64;
    let mut py = (pixel_y - row * tile_height).floor() as i64;
    if px < 0 {
        px = 0;
    }
    if py < 0 {
        py = 0;
    }
    let mut px = px as usize;
    let mut py = py as usize;
    if px >= matrix.tile_width as usize {
        px = matrix.tile_width as usize - 1;
    }
    if py >= matrix.tile_height as usize {
        py = matrix.tile_height as usize - 1;
    }
    Some(ColumnSample {
        col: col as u32,
        row: row as u32,
        pixel_x: px,
        pixel_y: py,
    })
}

fn load_tiles(
    tiles: &[TileCoordinate],
    cache: &WmtsCacheDir,
    config: &WmtsConfig,
    extension: &str,
) -> Result<HashMap<(u32, u32), Arc<RgbaImage>>> {
    let mut images = HashMap::new();
    for tile in tiles {
        let path = cache.tile_path(
            config.layer(),
            config.tile_matrix(),
            tile.row,
            tile.col,
            extension,
        );
        let image = image::open(&path)
            .with_context(|| format!("Failed to decode WMTS tile {}", path.display()))?
            .into_rgba8();
        images.insert((tile.row, tile.col), Arc::new(image));
    }
    Ok(images)
}

fn prefetch_tiles(
    tiles: &[TileCoordinate],
    base_url: &str,
    config: &WmtsConfig,
    style_id: &str,
    extension: &str,
    cache: &WmtsCacheDir,
) -> Result<()> {
    let mut client = build_wmts_tile_client()?;
    let pb = progress_bar(tiles.len() as u64, "Prefetching WMTS tiles");

    for tile in tiles {
        let path = cache.tile_path(
            config.layer(),
            config.tile_matrix(),
            tile.row,
            tile.col,
            extension,
        );
        if path.exists() {
            pb.inc(1);
            continue;
        }
        let url = build_tile_url(
            base_url,
            config.layer(),
            style_id,
            config.tile_matrix_set(),
            config.tile_matrix(),
            tile.row,
            tile.col,
            config.format(),
        );
        let mut attempt = 0;
        let bytes = loop {
            attempt += 1;
            let response = match client.get(&url).send() {
                Ok(response) => response,
                Err(err) => {
                    if attempt <= WMTS_FETCH_RETRIES {
                        println!(
                            "  {} Retrying WMTS tile ({}, {}) after network error: {}",
                            "↻".yellow(),
                            tile.row,
                            tile.col,
                            err
                        );
                        client = build_wmts_tile_client()?;
                        continue;
                    } else {
                        return Err(err).with_context(|| {
                            format!(
                                "Failed to fetch WMTS tile row {} col {}",
                                tile.row, tile.col
                            )
                        });
                    }
                }
            };
            if response.status() == StatusCode::OK {
                let body = response.bytes().context("Failed to read WMTS tile body")?;
                break body;
            }
            if attempt <= WMTS_FETCH_RETRIES {
                println!(
                    "  {} Retrying WMTS tile ({}, {}) after status {}",
                    "↻".yellow(),
                    tile.row,
                    tile.col,
                    response.status()
                );
                continue;
            }
            bail!(
                "WMTS tile request ({}, {}) failed with status {}",
                tile.row,
                tile.col,
                response.status()
            );
        };
        fs::write(&path, &bytes)
            .with_context(|| format!("Failed to write WMTS tile {}", path.display()))?;
        pb.inc(1);
    }
    pb.finish_with_message("WMTS tiles ready");
    Ok(())
}

fn build_wmts_tile_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(WMTS_HTTP_TIMEOUT_SECONDS))
        .user_agent("francegen/0.1")
        .build()
        .context("Failed to build WMTS HTTP client")
}

fn build_tile_url(
    base: &str,
    layer: &str,
    style: &str,
    matrix_set: &str,
    matrix: &str,
    row: u32,
    col: u32,
    format: &str,
) -> String {
    let mut url = base.to_string();
    if !url.contains('?') {
        url.push('?');
    } else if !url.ends_with('&') && !url.ends_with('?') {
        url.push('&');
    }
    let params = [
        ("SERVICE", "WMTS"),
        ("REQUEST", "GetTile"),
        ("VERSION", REQUEST_VERSION),
        ("LAYER", layer),
        ("STYLE", style),
        ("FORMAT", format),
        ("TileMatrixSet", matrix_set),
        ("TileMatrix", matrix),
        ("TileRow", &row.to_string()),
        ("TileCol", &col.to_string()),
    ];
    for (idx, (key, value)) in params.iter().enumerate() {
        if idx > 0 || url.ends_with('&') || url.ends_with('?') {
            if !url.ends_with('&') && !url.ends_with('?') {
                url.push('&');
            }
        }
        url.push_str(key);
        url.push('=');
        url.push_str(&encode(value));
        if !url.ends_with('&') {
            url.push('&');
        }
    }
    if url.ends_with('&') {
        url.pop();
    }
    url
}

fn extension_for_format(format: &str) -> Result<&'static str> {
    let fmt = format.to_ascii_lowercase();
    match fmt.as_str() {
        "image/png" => Ok("png"),
        "image/jpeg" | "image/jpg" => Ok("jpg"),
        other => bail!("Unsupported WMTS image format '{other}' (png and jpeg are supported)"),
    }
}

fn fetch_capabilities_document(url: &str) -> Result<String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(WMTS_HTTP_TIMEOUT_SECONDS))
        .user_agent("francegen/0.1")
        .build()
        .context("Failed to build WMTS capabilities HTTP client")?;
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("Failed to download WMTS capabilities from {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("WMTS capabilities request returned status {status}");
    }
    response
        .text()
        .context("Failed to read WMTS capabilities body")
}

struct WmtsCapabilities {
    get_tile_url: String,
    layer: LayerCapabilities,
    matrix_set: TileMatrixSet,
}

#[derive(Clone)]
struct LayerCapabilities {
    formats: Vec<String>,
    styles: Vec<String>,
    default_style: Option<String>,
    limits: HashMap<String, TileMatrixLimits>,
}

#[derive(Clone)]
struct TileMatrixSet {
    supported_crs: String,
    matrices: HashMap<String, TileMatrix>,
}

#[derive(Clone, Copy)]
struct TileMatrixLimits {
    min_row: u32,
    max_row: u32,
    min_col: u32,
    max_col: u32,
}

#[derive(Clone)]
struct TileMatrix {
    top_left_x: f64,
    top_left_y: f64,
    scale_denominator: f64,
    tile_width: u32,
    tile_height: u32,
    matrix_width: u32,
    matrix_height: u32,
}

impl TileMatrix {
    fn resolution(&self) -> f64 {
        self.scale_denominator * 0.00028
    }
}

fn parse_capabilities(xml: &str, config: &WmtsConfig) -> Result<WmtsCapabilities> {
    let doc = Document::parse(xml).context("Failed to parse WMTS capabilities XML")?;
    let root = doc.root_element();
    let get_tile_url = find_get_tile_url(root)?;
    let contents = root
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "Contents")
        .ok_or_else(|| anyhow!("WMTS capabilities missing Contents element"))?;
    let layer_node = contents
        .children()
        .find(|node| {
            node.is_element()
                && node.tag_name().name() == "Layer"
                && node.children().any(|child| {
                    child.tag_name().name() == "Identifier"
                        && child.text().map(|t| t.trim()) == Some(config.layer())
                })
        })
        .ok_or_else(|| anyhow!("Layer '{}' not found in WMTS capabilities", config.layer()))?;

    let matrix_set_node = contents
        .children()
        .find(|node| {
            node.is_element()
                && node.tag_name().name() == "TileMatrixSet"
                && node.children().any(|child| {
                    child.tag_name().name() == "Identifier"
                        && child.text().map(|t| t.trim()) == Some(config.tile_matrix_set())
                })
        })
        .ok_or_else(|| {
            anyhow!(
                "TileMatrixSet '{}' not found in WMTS capabilities",
                config.tile_matrix_set()
            )
        })?;

    let layer = parse_layer_capabilities(layer_node, config)?;
    let matrix_set = parse_tile_matrix_set(matrix_set_node)?;

    Ok(WmtsCapabilities {
        get_tile_url,
        layer,
        matrix_set,
    })
}

fn find_get_tile_url(root: Node<'_, '_>) -> Result<String> {
    root.descendants()
        .find(|node| {
            node.is_element()
                && node.tag_name().name() == "Operation"
                && node.attribute("name") == Some("GetTile")
        })
        .and_then(|operation| {
            operation
                .descendants()
                .find(|child| child.is_element() && child.tag_name().name() == "Get")
        })
        .and_then(|node| node.attribute(("http://www.w3.org/1999/xlink", "href")))
        .map(|value| value.to_string())
        .ok_or_else(|| anyhow!("WMTS capabilities missing OperationsMetadata/GetTile URL"))
}

fn parse_layer_capabilities(node: Node<'_, '_>, config: &WmtsConfig) -> Result<LayerCapabilities> {
    let mut formats = Vec::new();
    let mut styles = Vec::new();
    let mut default_style = None;
    for child in node.children().filter(|child| child.is_element()) {
        match child.tag_name().name() {
            "Format" => {
                if let Some(text) = child.text() {
                    formats.push(text.trim().to_string());
                }
            }
            "Style" => {
                let id = child
                    .children()
                    .find(|c| c.is_element() && c.tag_name().name() == "Identifier")
                    .and_then(|n| n.text())
                    .map(|t| t.trim().to_string());
                if let Some(id) = id {
                    if child.attribute("isDefault") == Some("true") {
                        default_style = Some(id.clone());
                    }
                    styles.push(id);
                }
            }
            _ => {}
        }
    }

    let mut limits_map: HashMap<String, TileMatrixLimits> = HashMap::new();
    let link = node
        .children()
        .find(|child| {
            child.is_element()
                && child.tag_name().name() == "TileMatrixSetLink"
                && child.children().any(|c| {
                    c.tag_name().name() == "TileMatrixSet"
                        && c.text().map(|t| t.trim()) == Some(config.tile_matrix_set())
                })
        })
        .ok_or_else(|| {
            anyhow!(
                "Layer '{}' is not linked to matrix set '{}'",
                config.layer(),
                config.tile_matrix_set()
            )
        })?;

    if let Some(limits_node) = link
        .children()
        .find(|c| c.tag_name().name() == "TileMatrixSetLimits")
    {
        for entry in limits_node
            .children()
            .filter(|c| c.tag_name().name() == "TileMatrixLimits")
        {
            let id = entry
                .children()
                .find(|c| c.tag_name().name() == "TileMatrix")
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string());
            let Some(id) = id else {
                continue;
            };
            let min_row = child_text(entry, "MinTileRow").and_then(|v| v.parse().ok());
            let max_row = child_text(entry, "MaxTileRow").and_then(|v| v.parse().ok());
            let min_col = child_text(entry, "MinTileCol").and_then(|v| v.parse().ok());
            let max_col = child_text(entry, "MaxTileCol").and_then(|v| v.parse().ok());
            if let (Some(min_row), Some(max_row), Some(min_col), Some(max_col)) =
                (min_row, max_row, min_col, max_col)
            {
                limits_map.insert(
                    id,
                    TileMatrixLimits {
                        min_row,
                        max_row,
                        min_col,
                        max_col,
                    },
                );
            }
        }
    }

    Ok(LayerCapabilities {
        formats,
        styles,
        default_style,
        limits: limits_map,
    })
}

fn parse_tile_matrix_set(node: Node<'_, '_>) -> Result<TileMatrixSet> {
    let supported_crs_raw = node
        .children()
        .find(|c| c.is_element() && c.tag_name().name() == "SupportedCRS")
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .ok_or_else(|| anyhow!("TileMatrixSet missing SupportedCRS"))?;
    let supported_crs = normalize_crs_identifier(&supported_crs_raw);

    let mut matrices = HashMap::new();
    for matrix_node in node
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "TileMatrix")
    {
        let id = child_text(matrix_node, "Identifier")
            .ok_or_else(|| anyhow!("TileMatrix missing Identifier"))?
            .to_string();
        let scale_denominator: f64 = child_text(matrix_node, "ScaleDenominator")
            .ok_or_else(|| anyhow!("TileMatrix missing ScaleDenominator"))?
            .parse()
            .with_context(|| format!("Invalid ScaleDenominator for TileMatrix {id}"))?;
        let top_left = child_text(matrix_node, "TopLeftCorner")
            .ok_or_else(|| anyhow!("TileMatrix missing TopLeftCorner"))?;
        let (top_left_x, top_left_y) = parse_corner(&top_left)
            .with_context(|| format!("Invalid TopLeftCorner for TileMatrix {id}"))?;
        let tile_width: u32 = child_text(matrix_node, "TileWidth")
            .ok_or_else(|| anyhow!("TileMatrix missing TileWidth"))?
            .parse()
            .with_context(|| format!("Invalid TileWidth for TileMatrix {id}"))?;
        let tile_height: u32 = child_text(matrix_node, "TileHeight")
            .ok_or_else(|| anyhow!("TileMatrix missing TileHeight"))?
            .parse()
            .with_context(|| format!("Invalid TileHeight for TileMatrix {id}"))?;
        let matrix_width: u32 = child_text(matrix_node, "MatrixWidth")
            .ok_or_else(|| anyhow!("TileMatrix missing MatrixWidth"))?
            .parse()
            .with_context(|| format!("Invalid MatrixWidth for TileMatrix {id}"))?;
        let matrix_height: u32 = child_text(matrix_node, "MatrixHeight")
            .ok_or_else(|| anyhow!("TileMatrix missing MatrixHeight"))?
            .parse()
            .with_context(|| format!("Invalid MatrixHeight for TileMatrix {id}"))?;
        matrices.insert(
            id,
            TileMatrix {
                top_left_x,
                top_left_y,
                scale_denominator,
                tile_width,
                tile_height,
                matrix_width,
                matrix_height,
            },
        );
    }

    Ok(TileMatrixSet {
        supported_crs,
        matrices,
    })
}

fn child_text<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
        .and_then(|child| child.text())
}

fn parse_corner(raw: &str) -> Result<(f64, f64)> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() != 2 {
        bail!("TopLeftCorner must contain two numbers");
    }
    let x: f64 = parts[0].parse().with_context(|| {
        format!(
            "Invalid coordinate in TopLeftCorner '{}': '{}'",
            raw, parts[0]
        )
    })?;
    let y: f64 = parts[1].parse().with_context(|| {
        format!(
            "Invalid coordinate in TopLeftCorner '{}': '{}'",
            raw, parts[1]
        )
    })?;
    Ok((x, y))
}

fn normalize_crs_identifier(raw: &str) -> String {
    if raw.to_uppercase().contains("EPSG") {
        if let Some(code) = raw.rsplit(':').next() {
            if !code.is_empty() {
                return format!("EPSG:{code}");
            }
        }
    }
    raw.to_string()
}

struct TileCoverage {
    tiles: Vec<TileCoordinate>,
    col_start: u32,
    col_end: u32,
    row_start: u32,
    row_end: u32,
}

impl TileCoverage {
    fn contains(&self, col: u32, row: u32) -> bool {
        col >= self.col_start && col <= self.col_end && row >= self.row_start && row <= self.row_end
    }
}

#[derive(Clone, Copy)]
struct TileCoordinate {
    row: u32,
    col: u32,
}

fn compute_tile_coverage(
    bbox: &WorldBoundingBox,
    lambert_to_tile: &Proj,
    matrix: &TileMatrix,
    limits: Option<TileMatrixLimits>,
) -> Result<TileCoverage> {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, z) in bbox.lambert_corners() {
        let (tx, ty) = lambert_to_tile
            .convert((x, z))
            .map_err(|err| anyhow!("Lambert93 → tile CRS transform failed: {err}"))?;
        min_x = min_x.min(tx);
        max_x = max_x.max(tx);
        min_y = min_y.min(ty);
        max_y = max_y.max(ty);
    }

    if !min_x.is_finite() || !max_x.is_finite() || !min_y.is_finite() || !max_y.is_finite() {
        bail!("Failed to project WMTS bounds to target CRS");
    }

    let resolution = matrix.resolution();
    let tile_width = matrix.tile_width as f64;
    let tile_height = matrix.tile_height as f64;

    let mut col_start = ((min_x - matrix.top_left_x) / (resolution * tile_width)).floor() as i64;
    let mut col_end = ((max_x - matrix.top_left_x) / (resolution * tile_width)).ceil() as i64;
    if col_start > col_end {
        std::mem::swap(&mut col_start, &mut col_end);
    }
    let mut row_start = ((matrix.top_left_y - max_y) / (resolution * tile_height)).floor() as i64;
    let mut row_end = ((matrix.top_left_y - min_y) / (resolution * tile_height)).ceil() as i64;
    if row_start > row_end {
        std::mem::swap(&mut row_start, &mut row_end);
    }

    col_start = col_start.clamp(0, matrix.matrix_width as i64 - 1);
    col_end = col_end.clamp(0, matrix.matrix_width as i64 - 1);
    row_start = row_start.clamp(0, matrix.matrix_height as i64 - 1);
    row_end = row_end.clamp(0, matrix.matrix_height as i64 - 1);

    if let Some(limit) = limits {
        col_start = col_start.max(limit.min_col as i64);
        col_end = col_end.min(limit.max_col as i64);
        row_start = row_start.max(limit.min_row as i64);
        row_end = row_end.min(limit.max_row as i64);
    }

    if col_start > col_end || row_start > row_end {
        return Ok(TileCoverage {
            tiles: Vec::new(),
            col_start: 0,
            col_end: 0,
            row_start: 0,
            row_end: 0,
        });
    }

    let mut tiles = Vec::new();
    for row in row_start..=row_end {
        for col in col_start..=col_end {
            tiles.push(TileCoordinate {
                row: row as u32,
                col: col as u32,
            });
        }
    }

    Ok(TileCoverage {
        tiles,
        col_start: col_start as u32,
        col_end: col_end as u32,
        row_start: row_start as u32,
        row_end: row_end as u32,
    })
}
