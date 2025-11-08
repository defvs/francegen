use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

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

        Ok(Self {
            top_layer_block: Arc::<str>::from(file.top_layer_block),
            bottom_layer_block: Arc::<str>::from(file.bottom_layer_block),
            top_layer_thickness: file.top_layer_thickness,
            base_biome: Arc::<str>::from(file.base_biome),
            biome_layers,
            top_block_layers,
            cliffs,
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
