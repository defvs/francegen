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
    biome_layers: Vec<VerticalLayer>,
    top_block_layers: Vec<VerticalLayer>,
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

    pub fn biome_for_height(&self, surface_height: i32) -> Arc<str> {
        match self
            .biome_layers
            .iter()
            .find(|layer| layer.contains(surface_height))
        {
            Some(layer) => Arc::clone(&layer.value),
            None => Arc::clone(&self.base_biome),
        }
    }

    pub fn top_block_for_height(&self, surface_height: i32) -> Arc<str> {
        match self
            .top_block_layers
            .iter()
            .find(|layer| layer.contains(surface_height))
        {
            Some(layer) => Arc::clone(&layer.value),
            None => Arc::clone(&self.top_layer_block),
        }
    }

    fn from_file(file: TerrainConfigFile) -> Result<Self> {
        if file.top_layer_thickness == 0 {
            bail!("top_layer_thickness must be greater than 0");
        }

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

#[derive(Debug, Deserialize)]
struct BiomeLayerFile {
    #[serde(default)]
    range: RangeFile,
    biome: String,
}

#[derive(Debug, Deserialize)]
struct TopBlockLayerFile {
    #[serde(default)]
    range: RangeFile,
    block: String,
}

#[derive(Debug, Deserialize, Default)]
struct RangeFile {
    #[serde(default)]
    min: Option<String>,
    #[serde(default)]
    max: Option<String>,
}

#[derive(Debug, Clone)]
struct VerticalLayer {
    min: i32,
    max: i32,
    value: Arc<str>,
}

impl VerticalLayer {
    fn contains(&self, height: i32) -> bool {
        height >= self.min && height <= self.max
    }
}

fn parse_biome_layer(file: BiomeLayerFile) -> Result<VerticalLayer> {
    if file.biome.trim().is_empty() {
        bail!("Biome layer value must not be empty");
    }
    let range = parse_range(file.range)?;
    Ok(VerticalLayer {
        min: range.0,
        max: range.1,
        value: Arc::<str>::from(file.biome),
    })
}

fn parse_top_block_layer(file: TopBlockLayerFile) -> Result<VerticalLayer> {
    if file.block.trim().is_empty() {
        bail!("Top block layer value must not be empty");
    }
    let range = parse_range(file.range)?;
    Ok(VerticalLayer {
        min: range.0,
        max: range.1,
        value: Arc::<str>::from(file.block),
    })
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
