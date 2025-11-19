use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Deserializer};

use crate::world::dem_to_minecraft;

#[derive(Debug, Clone)]
pub struct TerrainConfig {
    top_layer_block: Arc<str>,
    bottom_layer_block: Arc<str>,
    top_layer_thickness: u32,
    base_biome: Arc<str>,
    biome_layers: Vec<BiomeLayer>,
    top_block_layers: Vec<TopBlockLayer>,
    cliffs: CliffConfig,
    osm: Option<OsmConfig>,
    wmts: Option<WmtsConfig>,
    generate_features: bool,
    empty_chunk_radius: u32,
}

impl TerrainConfig {
    pub fn load_from_path(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read terrain config {}", path.display()))?;
        let file: TerrainConfigFile = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse terrain config {}", path.display()))?;
        Self::from_file(file)
    }

    pub fn top_layer_block(&self) -> Arc<str> {
        Arc::clone(&self.top_layer_block)
    }

    pub fn bottom_layer_block(&self) -> Arc<str> {
        Arc::clone(&self.bottom_layer_block)
    }

    pub fn top_layer_thickness(&self) -> u32 {
        self.top_layer_thickness
    }

    pub fn base_biome(&self) -> Arc<str> {
        Arc::clone(&self.base_biome)
    }

    pub fn biome_and_cliff_for_height(
        &self,
        surface_height: i32,
    ) -> (Arc<str>, Option<CliffSettings>) {
        let layer = self
            .biome_layers
            .iter()
            .find(|layer| layer.contains(surface_height));
        let biome = match layer {
            Some(layer) => Arc::clone(&layer.biome),
            None => Arc::clone(&self.base_biome),
        };
        let cliff = self
            .cliffs
            .resolve(layer.and_then(|layer| layer.cliff_override.as_ref()));
        (biome, cliff)
    }

    pub fn max_smoothing_radius(&self) -> u32 {
        if !self.cliffs.enabled() {
            return 0;
        }
        let mut max_radius = self.cliffs.default_radius();
        for layer in &self.biome_layers {
            if let Some(override_cfg) = &layer.cliff_override {
                if let Some(radius) = override_cfg.smoothing_radius {
                    max_radius = max_radius.max(radius);
                }
            }
        }
        max_radius
    }

    pub fn top_block_for_height(&self, surface_height: i32) -> Arc<str> {
        match self
            .top_block_layers
            .iter()
            .find(|layer| layer.contains(surface_height))
        {
            Some(layer) => Arc::clone(&layer.block),
            None => Arc::clone(&self.top_layer_block),
        }
    }

    pub fn osm(&self) -> Option<&OsmConfig> {
        self.osm.as_ref()
    }

    pub fn generate_features(&self) -> bool {
        self.generate_features
    }

    pub fn wmts(&self) -> Option<&WmtsConfig> {
        self.wmts.as_ref()
    }

    pub fn empty_chunk_radius(&self) -> u32 {
        self.empty_chunk_radius
    }

    fn from_file(file: TerrainConfigFile) -> Result<Self> {
        if file.top_layer_thickness == 0 {
            bail!("top_layer_thickness must be greater than 0");
        }

        let cliffs = CliffConfig::from_file(file.cliff_generation)?;
        let biome_layers = file
            .biome_layers
            .into_iter()
            .map(parse_biome_layer)
            .collect::<Result<Vec<_>>>()?;
        let top_block_layers = file
            .top_block_layers
            .into_iter()
            .map(parse_top_block_layer)
            .collect::<Result<Vec<_>>>()?;
        let osm = match file.osm {
            Some(config) => Some(OsmConfig::from_file(config)?),
            None => None,
        };
        let wmts = match file.wmts {
            Some(config) => Some(WmtsConfig::from_file(config)?),
            None => None,
        };
        Ok(Self {
            top_layer_block: Arc::<str>::from(file.top_layer_block),
            bottom_layer_block: Arc::<str>::from(file.bottom_layer_block),
            top_layer_thickness: file.top_layer_thickness,
            base_biome: Arc::<str>::from(file.base_biome),
            biome_layers,
            top_block_layers,
            cliffs,
            osm,
            wmts,
            generate_features: file.generate_features,
            empty_chunk_radius: file.empty_chunk_radius,
        })
    }
}

impl Default for TerrainConfig {
    fn default() -> Self {
        Self {
            top_layer_block: Arc::<str>::from("minecraft:grass_block"),
            bottom_layer_block: Arc::<str>::from("minecraft:stone"),
            top_layer_thickness: 1,
            base_biome: Arc::<str>::from("minecraft:plains"),
            biome_layers: Vec::new(),
            top_block_layers: Vec::new(),
            cliffs: CliffConfig::default(),
            osm: None,
            wmts: None,
            generate_features: false,
            empty_chunk_radius: default_empty_chunk_radius(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TerrainConfigFile {
    #[serde(default = "default_bottom_layer")]
    bottom_layer_block: String,
    #[serde(default = "default_top_layer")]
    top_layer_block: String,
    #[serde(default = "default_top_thickness")]
    top_layer_thickness: u32,
    #[serde(default = "default_base_biome")]
    base_biome: String,
    #[serde(default)]
    biome_layers: Vec<BiomeLayerFile>,
    #[serde(default)]
    top_block_layers: Vec<TopBlockLayerFile>,
    #[serde(default)]
    cliff_generation: CliffGenerationFile,
    #[serde(default)]
    osm: Option<OsmConfigFile>,
    #[serde(default)]
    wmts: Option<WmtsConfigFile>,
    #[serde(default = "default_generate_features")]
    generate_features: bool,
    #[serde(default = "default_empty_chunk_radius")]
    empty_chunk_radius: u32,
}

fn default_bottom_layer() -> String {
    "minecraft:stone".to_string()
}

fn default_top_layer() -> String {
    "minecraft:grass_block".to_string()
}

fn default_top_thickness() -> u32 {
    1
}

fn default_base_biome() -> String {
    "minecraft:plains".to_string()
}

fn default_generate_features() -> bool {
    false
}

fn default_empty_chunk_radius() -> u32 {
    32
}

fn default_smoothing_radius() -> u32 {
    1
}

fn default_smoothing_factor() -> f64 {
    0.0
}

fn default_cliff_angle() -> f64 {
    60.0
}

fn default_cliff_block() -> String {
    "minecraft:stone".to_string()
}

#[derive(Debug, Deserialize)]
struct BiomeLayerFile {
    #[serde(default)]
    range: RangeFile,
    biome: String,
    #[serde(default)]
    cliff_angle_threshold_degrees: Option<f64>,
    #[serde(default)]
    cliff_block: Option<String>,
    #[serde(default)]
    cliff_smoothing_radius: Option<u32>,
    #[serde(default)]
    cliff_smoothing_factor: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct TopBlockLayerFile {
    #[serde(default)]
    range: RangeFile,
    block: String,
}

#[derive(Debug, Deserialize, Clone)]
struct CliffGenerationFile {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_cliff_angle")]
    angle_threshold_degrees: f64,
    #[serde(default = "default_cliff_block")]
    block: String,
    #[serde(default = "default_smoothing_radius")]
    smoothing_radius: u32,
    #[serde(default = "default_smoothing_factor")]
    smoothing_factor: f64,
}

impl Default for CliffGenerationFile {
    fn default() -> Self {
        Self {
            enabled: false,
            angle_threshold_degrees: default_cliff_angle(),
            block: default_cliff_block(),
            smoothing_radius: default_smoothing_radius(),
            smoothing_factor: default_smoothing_factor(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct RangeFile {
    #[serde(default)]
    min: Option<String>,
    #[serde(default)]
    max: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CliffSettings {
    pub angle_threshold_degrees: f64,
    pub block: Arc<str>,
    pub smoothing_radius: u32,
    pub smoothing_factor: f64,
}

#[derive(Debug, Clone)]
struct CliffOverride {
    angle_threshold_degrees: Option<f64>,
    block: Option<Arc<str>>,
    smoothing_radius: Option<u32>,
    smoothing_factor: Option<f64>,
}

#[derive(Debug, Clone)]
struct CliffConfig {
    enabled: bool,
    default_settings: CliffSettings,
}

impl CliffConfig {
    fn from_file(file: CliffGenerationFile) -> Result<Self> {
        if file.angle_threshold_degrees <= 0.0 {
            bail!("cliff_generation.angle_threshold_degrees must be greater than 0");
        }
        if file.block.trim().is_empty() {
            bail!("cliff_generation.block must not be empty");
        }
        if file.smoothing_radius == 0 {
            bail!("cliff_generation.smoothing_radius must be at least 1");
        }
        if !(0.0..=1.0).contains(&file.smoothing_factor) {
            bail!("cliff_generation.smoothing_factor must be between 0 and 1");
        }
        Ok(Self {
            enabled: file.enabled,
            default_settings: CliffSettings {
                angle_threshold_degrees: file.angle_threshold_degrees,
                block: Arc::<str>::from(file.block),
                smoothing_radius: file.smoothing_radius,
                smoothing_factor: file.smoothing_factor,
            },
        })
    }

    fn resolve(&self, override_settings: Option<&CliffOverride>) -> Option<CliffSettings> {
        if !self.enabled {
            return None;
        }
        let mut resolved = self.default_settings.clone();
        if let Some(overrides) = override_settings {
            if let Some(angle) = overrides.angle_threshold_degrees {
                resolved.angle_threshold_degrees = angle;
            }
            if let Some(block) = &overrides.block {
                resolved.block = Arc::clone(block);
            }
            if let Some(radius) = overrides.smoothing_radius {
                resolved.smoothing_radius = radius;
            }
            if let Some(factor) = overrides.smoothing_factor {
                resolved.smoothing_factor = factor;
            }
        }
        Some(resolved)
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn default_radius(&self) -> u32 {
        self.default_settings.smoothing_radius
    }
}

impl Default for CliffConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_settings: CliffSettings {
                angle_threshold_degrees: default_cliff_angle(),
                block: Arc::<str>::from(default_cliff_block()),
                smoothing_radius: default_smoothing_radius(),
                smoothing_factor: default_smoothing_factor(),
            },
        }
    }
}

#[derive(Debug, Clone)]
struct BiomeLayer {
    min: i32,
    max: i32,
    biome: Arc<str>,
    cliff_override: Option<CliffOverride>,
}

impl BiomeLayer {
    fn contains(&self, height: i32) -> bool {
        height >= self.min && height <= self.max
    }
}

#[derive(Debug, Clone)]
struct TopBlockLayer {
    min: i32,
    max: i32,
    block: Arc<str>,
}

impl TopBlockLayer {
    fn contains(&self, height: i32) -> bool {
        height >= self.min && height <= self.max
    }
}

fn parse_biome_layer(file: BiomeLayerFile) -> Result<BiomeLayer> {
    if file.biome.trim().is_empty() {
        bail!("Biome layer value must not be empty");
    }
    let range = parse_range(file.range)?;
    let cliff_override = parse_cliff_override(
        file.cliff_angle_threshold_degrees,
        file.cliff_block,
        file.cliff_smoothing_radius,
        file.cliff_smoothing_factor,
    )?;
    Ok(BiomeLayer {
        min: range.0,
        max: range.1,
        biome: Arc::<str>::from(file.biome),
        cliff_override,
    })
}

fn parse_top_block_layer(file: TopBlockLayerFile) -> Result<TopBlockLayer> {
    if file.block.trim().is_empty() {
        bail!("Top block layer value must not be empty");
    }
    let range = parse_range(file.range)?;
    Ok(TopBlockLayer {
        min: range.0,
        max: range.1,
        block: Arc::<str>::from(file.block),
    })
}

fn parse_cliff_override(
    angle: Option<f64>,
    block: Option<String>,
    smoothing_radius: Option<u32>,
    smoothing_factor: Option<f64>,
) -> Result<Option<CliffOverride>> {
    if angle.is_none()
        && block.is_none()
        && smoothing_radius.is_none()
        && smoothing_factor.is_none()
    {
        return Ok(None);
    }
    if let Some(value) = angle {
        if value <= 0.0 {
            bail!("cliff_angle_threshold_degrees must be greater than 0");
        }
    }
    let block = match block {
        Some(name) => {
            if name.trim().is_empty() {
                bail!("cliff_block must not be empty when provided");
            }
            Some(Arc::<str>::from(name))
        }
        None => None,
    };
    if let Some(radius) = smoothing_radius {
        if radius == 0 {
            bail!("cliff_smoothing_radius must be at least 1 when provided");
        }
    }
    if let Some(factor) = smoothing_factor {
        if !(0.0..=1.0).contains(&factor) {
            bail!("cliff_smoothing_factor must be between 0 and 1 when provided");
        }
    }
    Ok(Some(CliffOverride {
        angle_threshold_degrees: angle,
        block,
        smoothing_radius,
        smoothing_factor,
    }))
}

fn parse_range(range: RangeFile) -> Result<(i32, i32)> {
    let min = match range.min {
        Some(raw) => parse_height(&raw)?,
        None => i32::MIN,
    };
    let max = match range.max {
        Some(raw) => parse_height(&raw)?,
        None => i32::MAX,
    };
    if min > max {
        bail!("Layer range min must be less than or equal to max");
    }
    Ok((min, max))
}

fn parse_height(raw: &str) -> Result<i32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("Height value must not be empty");
    }
    let (value_part, unit) = match trimmed.chars().last() {
        Some('m') | Some('M') => (&trimmed[..trimmed.len() - 1], 'm'),
        Some('b') | Some('B') => (&trimmed[..trimmed.len() - 1], 'b'),
        _ => (trimmed, 'm'),
    };
    let value_str = value_part.trim();
    if value_str.is_empty() {
        bail!("Height number is missing before unit");
    }
    let number: f64 = value_str
        .parse()
        .with_context(|| format!("Failed to parse height value '{raw}'"))?;
    let value = match unit {
        'm' => dem_to_minecraft(number),
        'b' => number.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32,
        _ => unreachable!(),
    };
    Ok(value)
}

fn default_osm_enabled() -> bool {
    true
}

fn default_overpass_url() -> String {
    "https://overpass-api.de/api/interpreter".to_string()
}

fn default_bbox_margin_m() -> f64 {
    300.0
}

fn default_line_width_m() -> f64 {
    3.0
}

fn default_line_width_source() -> AttributeSourceFile {
    AttributeSourceFile::Fixed(default_line_width_m())
}

fn default_wmts_enabled() -> bool {
    false
}

fn default_wmts_format() -> String {
    "image/png".to_string()
}

fn default_wmts_bbox_margin_m() -> f64 {
    0.0
}

fn default_wmts_max_tiles() -> u32 {
    2048
}

#[derive(Debug, Deserialize)]
struct OsmConfigFile {
    #[serde(default = "default_osm_enabled")]
    enabled: bool,
    #[serde(default = "default_overpass_url")]
    overpass_url: String,
    #[serde(default = "default_bbox_margin_m")]
    bbox_margin_m: f64,
    #[serde(default)]
    layers: Vec<OsmLayerFile>,
}

#[derive(Debug, Deserialize)]
struct OsmLayerFile {
    name: String,
    #[serde(default)]
    geometry: OsmGeometryFile,
    query: String,
    #[serde(default = "default_line_width_source")]
    width_m: AttributeSourceFile,
    #[serde(default)]
    priority: Option<u32>,
    #[serde(default)]
    layer_index: Option<i32>,
    #[serde(default)]
    style: OverlayStyleFile,
}

#[derive(Debug, Deserialize, Clone, Copy)]
enum OsmGeometryFile {
    #[serde(rename = "line")]
    Line,
    #[serde(rename = "polygon")]
    Polygon,
}

impl Default for OsmGeometryFile {
    fn default() -> Self {
        Self::Polygon
    }
}

#[derive(Debug, Deserialize, Default)]
struct OverlayStyleFile {
    biome: Option<String>,
    surface_block: Option<String>,
    subsurface_block: Option<String>,
    top_thickness: Option<u32>,
    #[serde(default)]
    extrusion: Option<ExtrusionStyleFile>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AttributeSourceFile {
    Fixed(f64),
    Dynamic(AttributeSourceObjectFile),
}

#[derive(Debug, Deserialize, Default)]
struct AttributeSourceObjectFile {
    default: Option<f64>,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    sources: Vec<AttributeKeySourceFile>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    multiplier: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct AttributeKeySourceFile {
    key: String,
    #[serde(default = "default_attribute_multiplier")]
    multiplier: f64,
}

fn default_attribute_multiplier() -> f64 {
    1.0
}

#[derive(Debug, Clone)]
pub struct AttributeSource {
    default_value: f64,
    min: Option<f64>,
    max: Option<f64>,
    sources: Vec<AttributeKeySource>,
}

#[derive(Debug, Clone)]
pub struct AttributeKeySource {
    key: Arc<str>,
    multiplier: f64,
}

impl AttributeSource {
    fn from_file(file: AttributeSourceFile, context: &str, absolute_min: f64) -> Result<Self> {
        match file {
            AttributeSourceFile::Fixed(value) => {
                validate_number(value, context)?;
                if value < absolute_min {
                    bail!("{context} must be at least {absolute_min}");
                }
                Ok(Self {
                    default_value: value,
                    min: None,
                    max: None,
                    sources: Vec::new(),
                })
            }
            AttributeSourceFile::Dynamic(config) => {
                let default_value = config.default.ok_or_else(|| {
                    anyhow!("{context}.default must be provided when using an object")
                })?;
                validate_number(default_value, &format!("{context}.default"))?;
                if default_value < absolute_min {
                    bail!("{context}.default must be at least {absolute_min}");
                }
                let min = match config.min {
                    Some(value) => {
                        validate_number(value, &format!("{context}.min"))?;
                        if value < absolute_min {
                            bail!("{context}.min must be at least {absolute_min}");
                        }
                        Some(value)
                    }
                    None => None,
                };
                let max = match config.max {
                    Some(value) => {
                        validate_number(value, &format!("{context}.max"))?;
                        if value < absolute_min {
                            bail!("{context}.max must be at least {absolute_min}");
                        }
                        Some(value)
                    }
                    None => None,
                };
                if let (Some(min_value), Some(max_value)) = (min, max) {
                    if min_value > max_value {
                        bail!("{context}.min must be less than or equal to {context}.max");
                    }
                }
                let mut entries = Vec::with_capacity(config.sources.len() + 1);
                if let Some(key) = config.key {
                    let multiplier = config
                        .multiplier
                        .unwrap_or_else(default_attribute_multiplier);
                    entries.push(AttributeKeySourceFile { key, multiplier });
                }
                entries.extend(config.sources);
                let sources = entries
                    .into_iter()
                    .enumerate()
                    .map(|(idx, entry)| {
                        AttributeKeySource::from_file(entry, &format!("{context}.sources[{idx}]"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(Self {
                    default_value,
                    min,
                    max,
                    sources,
                })
            }
        }
    }

    pub fn default_value(&self) -> f64 {
        self.default_value
    }

    pub fn sources(&self) -> &[AttributeKeySource] {
        &self.sources
    }

    pub fn clamp(&self, value: f64) -> f64 {
        let mut result = value;
        if let Some(min) = self.min {
            result = result.max(min);
        }
        if let Some(max) = self.max {
            result = result.min(max);
        }
        result
    }
}

impl AttributeKeySource {
    fn from_file(file: AttributeKeySourceFile, context: &str) -> Result<Self> {
        if file.key.trim().is_empty() {
            bail!("{context}.key must not be empty");
        }
        validate_number(file.multiplier, &format!("{context}.multiplier"))?;
        Ok(Self {
            key: Arc::<str>::from(file.key),
            multiplier: file.multiplier,
        })
    }

    pub fn key(&self) -> &Arc<str> {
        &self.key
    }

    pub fn multiplier(&self) -> f64 {
        self.multiplier
    }
}

#[derive(Debug, Deserialize)]
struct ExtrusionStyleFile {
    #[serde(rename = "height_m")]
    height: AttributeSourceFile,
    #[serde(default)]
    block: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExtrusionStyle {
    height: AttributeSource,
    block: Option<Arc<str>>,
}

impl ExtrusionStyle {
    fn from_file(file: ExtrusionStyleFile, context: &str) -> Result<Self> {
        let height = AttributeSource::from_file(file.height, &format!("{context}.height_m"), 0.0)?;
        let block = normalize_style_name(file.block, &format!("{context}.block"))?;
        Ok(Self { height, block })
    }

    pub fn height(&self) -> &AttributeSource {
        &self.height
    }

    pub fn block(&self) -> Option<&Arc<str>> {
        self.block.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct OsmConfig {
    enabled: bool,
    overpass_url: Arc<str>,
    bbox_margin_m: f64,
    layers: Vec<OsmLayer>,
}

impl OsmConfig {
    fn from_file(file: OsmConfigFile) -> Result<Self> {
        if file.layers.is_empty() && file.enabled {
            bail!("osm.layers must contain at least one entry when osm.enabled is true");
        }
        let mut layers = Vec::with_capacity(file.layers.len());
        for (idx, layer) in file.layers.into_iter().enumerate() {
            layers.push(OsmLayer::from_file(layer, idx as u32)?);
        }
        reorder_osm_layers(&mut layers);
        Ok(Self {
            enabled: file.enabled,
            overpass_url: Arc::<str>::from(file.overpass_url),
            bbox_margin_m: file.bbox_margin_m.max(0.0),
            layers,
        })
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn overpass_url(&self) -> &str {
        &self.overpass_url
    }

    pub fn bbox_margin_m(&self) -> f64 {
        self.bbox_margin_m
    }

    pub fn layers(&self) -> &[OsmLayer] {
        &self.layers
    }
}

#[derive(Debug, Clone)]
pub struct OsmLayer {
    name: Arc<str>,
    geometry: OsmGeometry,
    query: Arc<str>,
    width: AttributeSource,
    style: OverlayStyle,
    layer_index: Option<i32>,
    original_order: u32,
}

impl OsmLayer {
    fn from_file(file: OsmLayerFile, original_order: u32) -> Result<Self> {
        if file.name.trim().is_empty() {
            bail!("osm layer name must not be empty");
        }
        if file.query.trim().is_empty() {
            bail!("osm layer query must not be empty");
        }
        let geometry = match file.geometry {
            OsmGeometryFile::Line => OsmGeometry::Line,
            OsmGeometryFile::Polygon => OsmGeometry::Polygon,
        };
        let width = AttributeSource::from_file(file.width_m, "osm.layers[].width_m", 0.5)?;
        let style = OverlayStyle::from_file(file.style, "osm.layers[].style")?;
        let layer_index = file.layer_index.or(file.priority.map(|value| value as i32));
        Ok(Self {
            name: Arc::<str>::from(file.name),
            geometry,
            query: Arc::<str>::from(file.query),
            width,
            style,
            layer_index,
            original_order,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn geometry(&self) -> OsmGeometry {
        self.geometry
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn width(&self) -> &AttributeSource {
        &self.width
    }

    pub fn style(&self) -> &OverlayStyle {
        &self.style
    }

    pub fn layer_index(&self) -> Option<i32> {
        self.layer_index
    }

    pub fn original_order(&self) -> u32 {
        self.original_order
    }
}

#[derive(Debug, Clone, Copy)]
pub enum OsmGeometry {
    Line,
    Polygon,
}

#[derive(Debug, Clone)]
pub struct OverlayStyle {
    biome: Option<Arc<str>>,
    surface_block: Option<Arc<str>>,
    subsurface_block: Option<Arc<str>>,
    top_thickness: Option<u32>,
    extrusion: Option<ExtrusionStyle>,
}

impl OverlayStyle {
    fn from_file(file: OverlayStyleFile, context: &str) -> Result<Self> {
        if file.biome.is_none()
            && file.surface_block.is_none()
            && file.subsurface_block.is_none()
            && file.top_thickness.is_none()
            && file.extrusion.is_none()
        {
            bail!(
                "{context} must set at least one of biome, surface_block, subsurface_block, top_thickness, or extrusion"
            );
        }
        if let Some(thickness) = file.top_thickness {
            if thickness == 0 {
                bail!("{context}.top_thickness must be greater than 0 when provided");
            }
        }
        let extrusion = match file.extrusion {
            Some(extrusion) => Some(ExtrusionStyle::from_file(
                extrusion,
                &format!("{context}.extrusion"),
            )?),
            None => None,
        };
        Ok(Self {
            biome: normalize_style_name(file.biome, &format!("{context}.biome"))?,
            surface_block: normalize_style_name(
                file.surface_block,
                &format!("{context}.surface_block"),
            )?,
            subsurface_block: normalize_style_name(
                file.subsurface_block,
                &format!("{context}.subsurface_block"),
            )?,
            top_thickness: file.top_thickness,
            extrusion,
        })
    }

    pub fn biome(&self) -> Option<&Arc<str>> {
        self.biome.as_ref()
    }

    pub fn surface_block(&self) -> Option<&Arc<str>> {
        self.surface_block.as_ref()
    }

    pub fn subsurface_block(&self) -> Option<&Arc<str>> {
        self.subsurface_block.as_ref()
    }

    pub fn top_thickness(&self) -> Option<u32> {
        self.top_thickness
    }

    pub fn extrusion(&self) -> Option<&ExtrusionStyle> {
        self.extrusion.as_ref()
    }
}

fn normalize_style_name(value: Option<String>, field: &str) -> Result<Option<Arc<str>>> {
    match value {
        Some(name) => {
            if name.trim().is_empty() {
                bail!("{field} must not be empty when provided");
            }
            Ok(Some(Arc::<str>::from(name)))
        }
        None => Ok(None),
    }
}

fn validate_number(value: f64, field: &str) -> Result<()> {
    if !value.is_finite() {
        bail!("{field} must be a finite number");
    }
    Ok(())
}

fn reorder_osm_layers(layers: &mut Vec<OsmLayer>) {
    layers.sort_by(|a, b| {
        compare_layer_order(
            a.layer_index(),
            b.layer_index(),
            a.original_order(),
            b.original_order(),
        )
    });
}

fn reorder_wmts_rules(rules: &mut Vec<WmtsColorRule>) {
    rules.sort_by(|a, b| {
        compare_layer_order(
            a.layer_index(),
            b.layer_index(),
            a.original_order(),
            b.original_order(),
        )
    });
}

fn compare_layer_order(
    a_index: Option<i32>,
    b_index: Option<i32>,
    a_order: u32,
    b_order: u32,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let a_idx = a_index.unwrap_or(0);
    let b_idx = b_index.unwrap_or(0);
    match b_idx.cmp(&a_idx) {
        Ordering::Equal => a_order.cmp(&b_order),
        other => other,
    }
}

#[derive(Debug, Deserialize)]
struct WmtsConfigFile {
    #[serde(default = "default_wmts_enabled")]
    enabled: bool,
    capabilities_url: Option<String>,
    layer: Option<String>,
    #[serde(default)]
    style_id: Option<String>,
    tile_matrix_set: Option<String>,
    tile_matrix: Option<TileMatrixId>,
    #[serde(default = "default_wmts_format")]
    format: String,
    #[serde(default = "default_wmts_bbox_margin_m")]
    bbox_margin_m: f64,
    #[serde(default = "default_wmts_max_tiles")]
    max_tiles: u32,
    #[serde(default)]
    colors: Vec<WmtsColorRuleFile>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct WmtsColorRuleFile {
    #[serde(default)]
    name: Option<String>,
    color: String,
    #[serde(default)]
    tolerance: Option<u8>,
    #[serde(default)]
    alpha_threshold: Option<u8>,
    #[serde(default)]
    priority: Option<u32>,
    #[serde(default)]
    layer_index: Option<i32>,
    #[serde(default)]
    style: OverlayStyleFile,
}

#[derive(Debug, Clone)]
pub struct WmtsConfig {
    enabled: bool,
    capabilities_url: Arc<str>,
    layer: Arc<str>,
    style_id: Option<Arc<str>>,
    tile_matrix_set: Arc<str>,
    tile_matrix: Arc<str>,
    format: Arc<str>,
    bbox_margin_m: f64,
    max_tiles: u32,
    colors: Vec<WmtsColorRule>,
}

impl WmtsConfig {
    fn from_file(file: WmtsConfigFile) -> Result<Self> {
        if !file.enabled {
            return Ok(Self {
                enabled: false,
                capabilities_url: Arc::<str>::from(""),
                layer: Arc::<str>::from(""),
                style_id: None,
                tile_matrix_set: Arc::<str>::from(""),
                tile_matrix: Arc::<str>::from(""),
                format: Arc::<str>::from(file.format),
                bbox_margin_m: 0.0,
                max_tiles: file.max_tiles.max(1),
                colors: Vec::new(),
            });
        }

        let capabilities_url = file
            .capabilities_url
            .ok_or_else(|| anyhow!("wmts.capabilities_url is required when wmts.enabled = true"))?;
        if capabilities_url.trim().is_empty() {
            bail!("wmts.capabilities_url must not be empty");
        }

        let layer = file
            .layer
            .ok_or_else(|| anyhow!("wmts.layer is required when wmts.enabled = true"))?;
        if layer.trim().is_empty() {
            bail!("wmts.layer must not be empty");
        }

        let tile_matrix_set = file
            .tile_matrix_set
            .ok_or_else(|| anyhow!("wmts.tile_matrix_set is required when wmts.enabled = true"))?;
        if tile_matrix_set.trim().is_empty() {
            bail!("wmts.tile_matrix_set must not be empty");
        }

        let tile_matrix = file
            .tile_matrix
            .ok_or_else(|| anyhow!("wmts.tile_matrix is required when wmts.enabled = true"))?;

        if file.colors.is_empty() {
            bail!("wmts.colors must contain at least one rule when wmts.enabled = true");
        }

        let mut colors = Vec::with_capacity(file.colors.len());
        for (idx, rule) in file.colors.into_iter().enumerate() {
            colors.push(WmtsColorRule::from_file(rule, idx as u32)?);
        }
        reorder_wmts_rules(&mut colors);

        Ok(Self {
            enabled: true,
            capabilities_url: Arc::<str>::from(capabilities_url),
            layer: Arc::<str>::from(layer),
            style_id: match file.style_id {
                Some(id) if id.trim().is_empty() => {
                    bail!("wmts.style_id must not be empty when provided");
                }
                Some(id) => Some(Arc::<str>::from(id)),
                None => None,
            },
            tile_matrix_set: Arc::<str>::from(tile_matrix_set),
            tile_matrix: Arc::<str>::from(tile_matrix.into_inner()),
            format: Arc::<str>::from(file.format),
            bbox_margin_m: file.bbox_margin_m.max(0.0),
            max_tiles: file.max_tiles.max(1),
            colors,
        })
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn capabilities_url(&self) -> &str {
        &self.capabilities_url
    }

    pub fn layer(&self) -> &str {
        &self.layer
    }

    pub fn style_id(&self) -> Option<&str> {
        self.style_id.as_deref()
    }

    pub fn tile_matrix_set(&self) -> &str {
        &self.tile_matrix_set
    }

    pub fn tile_matrix(&self) -> &str {
        &self.tile_matrix
    }

    pub fn format(&self) -> &str {
        &self.format
    }

    pub fn bbox_margin_m(&self) -> f64 {
        self.bbox_margin_m
    }

    pub fn max_tiles(&self) -> u32 {
        self.max_tiles
    }

    pub fn colors(&self) -> &[WmtsColorRule] {
        &self.colors
    }
}

#[derive(Debug, Clone)]
pub struct WmtsColorRule {
    color: RgbaColor,
    tolerance: u8,
    alpha_threshold: u8,
    style: OverlayStyle,
    layer_index: Option<i32>,
    original_order: u32,
}

impl WmtsColorRule {
    fn from_file(file: WmtsColorRuleFile, position: u32) -> Result<Self> {
        let color = RgbaColor::parse(&file.color)
            .with_context(|| format!("Invalid wmts.colors[{position}].color value"))?;
        let tolerance = file.tolerance.unwrap_or(0);
        let alpha_threshold = file.alpha_threshold.unwrap_or(1);
        let style = OverlayStyle::from_file(file.style, &format!("wmts.colors[{position}].style"))?;
        let layer_index = file.layer_index.or(file.priority.map(|value| value as i32));
        Ok(Self {
            color,
            tolerance,
            alpha_threshold,
            style,
            layer_index,
            original_order: position,
        })
    }

    pub fn style(&self) -> &OverlayStyle {
        &self.style
    }

    pub fn layer_index(&self) -> Option<i32> {
        self.layer_index
    }

    pub fn original_order(&self) -> u32 {
        self.original_order
    }

    pub fn matches(&self, rgba: [u8; 4]) -> bool {
        if rgba[3] < self.alpha_threshold {
            return false;
        }
        let alpha_matches = if self.color.a < 255 {
            rgba[3].abs_diff(self.color.a) <= self.tolerance
        } else {
            true
        };
        alpha_matches
            && rgba[0].abs_diff(self.color.r) <= self.tolerance
            && rgba[1].abs_diff(self.color.g) <= self.tolerance
            && rgba[2].abs_diff(self.color.b) <= self.tolerance
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RgbaColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl RgbaColor {
    fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
        let bytes = match hex.len() {
            6 => hex_to_bytes(hex)?,
            8 => hex_to_bytes(hex)?,
            _ => {
                bail!("expected 6 or 8 hex digits, found '{}'", value);
            }
        };
        if bytes.len() == 3 {
            Ok(Self {
                r: bytes[0],
                g: bytes[1],
                b: bytes[2],
                a: 255,
            })
        } else {
            Ok(Self {
                r: bytes[0],
                g: bytes[1],
                b: bytes[2],
                a: bytes[3],
            })
        }
    }
}

fn hex_to_bytes(raw: &str) -> Result<Vec<u8>> {
    if raw.len() % 2 != 0 {
        bail!("hex string must have even length");
    }
    let mut bytes = Vec::with_capacity(raw.len() / 2);
    let chars: Vec<char> = raw.chars().collect();
    for chunk in chars.chunks(2) {
        let pair: String = chunk.iter().collect();
        let value =
            u8::from_str_radix(&pair, 16).with_context(|| format!("Invalid hex pair '{pair}'"))?;
        bytes.push(value);
    }
    Ok(bytes)
}

#[derive(Debug)]
struct TileMatrixId(String);

impl TileMatrixId {
    fn into_inner(self) -> String {
        self.0
    }
}

impl<'de> Deserialize<'de> for TileMatrixId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = TileMatrixId;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or integer tile matrix identifier")
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(TileMatrixId(value.to_string()))
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(TileMatrixId(value.to_string()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(TileMatrixId(value))
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}
