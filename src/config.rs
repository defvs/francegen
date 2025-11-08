use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct TerrainConfig {
    top_layer_block: Arc<str>,
    bottom_layer_block: Arc<str>,
    top_layer_thickness: u32,
    base_biome: Arc<str>,
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

    fn from_file(file: TerrainConfigFile) -> Result<Self> {
        if file.top_layer_thickness == 0 {
            bail!("top_layer_thickness must be greater than 0");
        }

        Ok(Self {
            top_layer_block: Arc::<str>::from(file.top_layer_block),
            bottom_layer_block: Arc::<str>::from(file.bottom_layer_block),
            top_layer_thickness: file.top_layer_thickness,
            base_biome: Arc::<str>::from(file.base_biome),
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
