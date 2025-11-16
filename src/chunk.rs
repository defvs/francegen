use std::cmp::max;
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use fastanvil::Region;
use fastnbt::{self, LongArray};
use rayon::prelude::*;
use serde::Serialize;

use crate::config::{CliffSettings, TerrainConfig};
use crate::constants::{
    BEDROCK_Y, BLOCKS_PER_SECTION, DATA_VERSION, MAX_WORLD_Y, POST_PROCESSING_SECTION_COUNT,
    SECTION_SIDE,
};
use crate::progress::progress_bar;

const BIOME_SIDE: usize = SECTION_SIDE / 4;
const BIOME_SCALE: usize = SECTION_SIDE / BIOME_SIDE;
const BIOME_ENTRIES_PER_SECTION: usize = BIOME_SIDE * BIOME_SIDE * BIOME_SIDE;
const _: [(); SECTION_SIDE % 4] = [];

#[derive(Clone)]
pub struct ColumnOverlay {
    layer_index: i32,
    order: u32,
    biome: Option<Arc<str>>,
    surface_block: Option<Arc<str>>,
    subsurface_block: Option<Arc<str>>,
    top_thickness: Option<u32>,
}

impl ColumnOverlay {
    pub fn new(
        layer_index: i32,
        order: u32,
        biome: Option<Arc<str>>,
        surface_block: Option<Arc<str>>,
        subsurface_block: Option<Arc<str>>,
        top_thickness: Option<u32>,
    ) -> Self {
        Self {
            layer_index,
            order,
            biome,
            surface_block,
            subsurface_block,
            top_thickness,
        }
    }

    fn outranks(&self, other: &ColumnOverlay) -> bool {
        if self.layer_index != other.layer_index {
            self.layer_index < other.layer_index
        } else {
            self.order > other.order
        }
    }

    pub fn biome_override(&self) -> Option<Arc<str>> {
        self.biome.as_ref().map(Arc::clone)
    }

    pub fn surface_block_override(&self) -> Option<Arc<str>> {
        self.surface_block.as_ref().map(Arc::clone)
    }

    pub fn subsurface_block_override(&self) -> Option<Arc<str>> {
        self.subsurface_block.as_ref().map(Arc::clone)
    }

    pub fn top_thickness_override(&self) -> Option<u32> {
        self.top_thickness
    }
}

#[derive(Clone)]
pub struct SlopeProfile {
    stats: Vec<SlopeStats>,
}

impl SlopeProfile {
    pub fn empty(levels: usize) -> Self {
        if levels == 0 {
            return Self { stats: Vec::new() };
        }
        Self {
            stats: vec![SlopeStats::default(); levels],
        }
    }

    pub fn from_stats(stats: Vec<SlopeStats>) -> Self {
        Self { stats }
    }

    pub fn evaluate(&self, settings: &CliffSettings) -> f32 {
        if self.stats.is_empty() {
            return 0.0;
        }
        let radius = settings.smoothing_radius.max(1) as usize;
        let idx = radius.saturating_sub(1).min(self.stats.len() - 1);
        let entry = &self.stats[idx];
        let factor = settings.smoothing_factor.clamp(0.0, 1.0) as f32;
        entry.max_angle + (entry.weighted_average - entry.max_angle) * factor
    }
}

#[derive(Clone, Default)]
pub struct SlopeStats {
    pub max_angle: f32,
    pub weighted_average: f32,
}

pub struct ChunkHeights {
    heights: [Option<i32>; SECTION_SIDE * SECTION_SIDE],
    slopes: Vec<SlopeProfile>,
    overlays: Vec<Option<ColumnOverlay>>,
}

impl ChunkHeights {
    pub fn new(max_smoothing_radius: usize) -> Self {
        let mut slopes = Vec::with_capacity(SECTION_SIDE * SECTION_SIDE);
        for _ in 0..SECTION_SIDE * SECTION_SIDE {
            slopes.push(SlopeProfile::empty(max_smoothing_radius));
        }
        Self {
            heights: [None; SECTION_SIDE * SECTION_SIDE],
            slopes,
            overlays: vec![None; SECTION_SIDE * SECTION_SIDE],
        }
    }

    pub fn set(&mut self, x: usize, z: usize, height: i32, slope_profile: SlopeProfile) {
        let idx = z * SECTION_SIDE + x;
        self.heights[idx] = Some(height);
        self.slopes[idx] = slope_profile;
    }

    pub fn column(&self, x: usize, z: usize) -> Option<i32> {
        self.heights[z * SECTION_SIDE + x]
    }

    pub fn slope(&self, x: usize, z: usize, settings: &CliffSettings) -> f32 {
        let idx = z * SECTION_SIDE + x;
        self.slopes[idx].evaluate(settings)
    }

    pub fn max_height(&self) -> Option<i32> {
        self.heights.iter().copied().flatten().max()
    }

    pub fn apply_overlay(&mut self, x: usize, z: usize, overlay: ColumnOverlay) {
        let idx = z * SECTION_SIDE + x;
        let replace = match &self.overlays[idx] {
            Some(current) if current.outranks(&overlay) => false,
            _ => true,
        };
        if replace {
            self.overlays[idx] = Some(overlay);
        }
    }

    pub fn overlay(&self, x: usize, z: usize) -> Option<&ColumnOverlay> {
        self.overlays[z * SECTION_SIDE + x].as_ref()
    }
}

pub struct WriteStats {
    pub region_files: usize,
    pub chunks_written: usize,
}

#[derive(Clone, Copy)]
struct ChunkJob {
    chunk_x: i32,
    chunk_z: i32,
    is_empty: bool,
}

impl ChunkJob {
    fn filled(chunk_x: i32, chunk_z: i32) -> Self {
        Self {
            chunk_x,
            chunk_z,
            is_empty: false,
        }
    }

    fn empty(chunk_x: i32, chunk_z: i32) -> Self {
        Self {
            chunk_x,
            chunk_z,
            is_empty: true,
        }
    }
}

pub fn write_regions(
    output: &Path,
    chunks: &HashMap<(i32, i32), ChunkHeights>,
    terrain: &TerrainConfig,
) -> Result<WriteStats> {
    if chunks.is_empty() {
        return Ok(WriteStats {
            region_files: 0,
            chunks_written: 0,
        });
    }

    let region_dir = output.join("region");
    fs::create_dir_all(&region_dir)
        .with_context(|| format!("Failed to create region directory {}", region_dir.display()))?;

    let mut per_region: HashMap<(i32, i32), Vec<ChunkJob>> = HashMap::new();
    let mut min_chunk_x = i32::MAX;
    let mut max_chunk_x = i32::MIN;
    let mut min_chunk_z = i32::MAX;
    let mut max_chunk_z = i32::MIN;
    for (&(chunk_x, chunk_z), _) in chunks.iter() {
        let region_x = chunk_x.div_euclid(32);
        let region_z = chunk_z.div_euclid(32);
        per_region
            .entry((region_x, region_z))
            .or_default()
            .push(ChunkJob::filled(chunk_x, chunk_z));
        min_chunk_x = min_chunk_x.min(chunk_x);
        max_chunk_x = max_chunk_x.max(chunk_x);
        min_chunk_z = min_chunk_z.min(chunk_z);
        max_chunk_z = max_chunk_z.max(chunk_z);
    }

    let padding = terrain.empty_chunk_radius() as i32;
    if padding > 0 && min_chunk_x <= max_chunk_x && min_chunk_z <= max_chunk_z {
        let padded_min_x = min_chunk_x.saturating_sub(padding);
        let padded_max_x = max_chunk_x.saturating_add(padding);
        let padded_min_z = min_chunk_z.saturating_sub(padding);
        let padded_max_z = max_chunk_z.saturating_add(padding);
        for chunk_x in padded_min_x..=padded_max_x {
            for chunk_z in padded_min_z..=padded_max_z {
                if chunks.contains_key(&(chunk_x, chunk_z)) {
                    continue;
                }
                if chunk_x >= min_chunk_x
                    && chunk_x <= max_chunk_x
                    && chunk_z >= min_chunk_z
                    && chunk_z <= max_chunk_z
                {
                    continue;
                }
                let region_x = chunk_x.div_euclid(32);
                let region_z = chunk_z.div_euclid(32);
                per_region
                    .entry((region_x, region_z))
                    .or_default()
                    .push(ChunkJob::empty(chunk_x, chunk_z));
            }
        }
    }

    let total_chunks: usize = per_region.values().map(|v| v.len()).sum();
    if total_chunks == 0 {
        return Ok(WriteStats {
            region_files: 0,
            chunks_written: 0,
        });
    }

    let pb = Arc::new(progress_bar(total_chunks as u64, "Writing chunks"));
    let region_dir = Arc::new(region_dir);
    let region_file_count = per_region.len();

    let chunks_written = per_region
        .into_par_iter()
        .map(|((region_x, region_z), coords)| -> Result<usize> {
            let pb = pb.clone();
            let mut region = create_region(region_dir.as_ref(), region_x, region_z)?;
            let mut written = 0usize;
            for job in coords {
                let chunk_x = job.chunk_x;
                let chunk_z = job.chunk_z;
                let data = if job.is_empty {
                    Some(build_empty_chunk_bytes(chunk_x, chunk_z, terrain)?)
                } else if let Some(columns) = chunks.get(&(chunk_x, chunk_z)) {
                    build_chunk_bytes(chunk_x, chunk_z, columns, terrain)?
                } else {
                    None
                };
                if let Some(bytes) = data {
                    let local_x = chunk_x.rem_euclid(32) as usize;
                    let local_z = chunk_z.rem_euclid(32) as usize;
                    region
                        .write_chunk(local_x, local_z, &bytes)
                        .with_context(|| {
                            format!("Failed to write chunk at ({chunk_x}, {chunk_z})")
                        })?;
                    written += 1;
                }
                pb.inc(1);
            }
            Ok(written)
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .sum();

    pb.finish_with_message("Chunks finalized");

    Ok(WriteStats {
        region_files: region_file_count,
        chunks_written,
    })
}

fn create_region(dir: &Path, rx: i32, rz: i32) -> Result<Region<File>> {
    let file_path = dir.join(format!("r.{rx}.{rz}.mca"));
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&file_path)
        .with_context(|| format!("Failed to open region file {}", file_path.display()))?;
    Region::new(file).with_context(|| format!("Failed to initialize {}", file_path.display()))
}

fn build_chunk_bytes(
    chunk_x: i32,
    chunk_z: i32,
    columns: &ChunkHeights,
    terrain: &TerrainConfig,
) -> Result<Option<Vec<u8>>> {
    let max_height = match columns.max_height() {
        Some(value) => value,
        None => return Ok(None),
    };

    let sections = build_sections(columns, max_height, terrain);
    if sections.is_empty() {
        return Ok(None);
    }

    let heightmaps = build_heightmaps(columns);

    let status = if terrain.generate_features() {
        "minecraft:liquid_carvers"
    } else {
        "minecraft:full"
    };

    let post_processing = if terrain.generate_features() {
        Some(empty_post_processing_lists())
    } else {
        None
    };

    let chunk = ChunkNbt {
        data_version: DATA_VERSION,
        last_update: 0,
        inhabited_time: 0,
        x_pos: chunk_x,
        z_pos: chunk_z,
        y_pos: (BEDROCK_Y.div_euclid(SECTION_SIDE as i32)) as i32,
        status: status.to_string(),
        sections,
        heightmaps,
        structures: StructuresNbt::default(),
        post_processing,
    };

    let bytes = fastnbt::to_bytes(&chunk).context("Failed to serialize chunk NBT")?;
    Ok(Some(bytes))
}

fn build_empty_chunk_bytes(chunk_x: i32, chunk_z: i32, terrain: &TerrainConfig) -> Result<Vec<u8>> {
    let status = if terrain.generate_features() {
        "minecraft:liquid_carvers"
    } else {
        "minecraft:full"
    };

    let post_processing = if terrain.generate_features() {
        Some(empty_post_processing_lists())
    } else {
        None
    };

    let chunk = ChunkNbt {
        data_version: DATA_VERSION,
        last_update: 0,
        inhabited_time: 0,
        x_pos: chunk_x,
        z_pos: chunk_z,
        y_pos: (BEDROCK_Y.div_euclid(SECTION_SIDE as i32)) as i32,
        status: status.to_string(),
        sections: Vec::new(),
        heightmaps: empty_heightmaps(),
        structures: StructuresNbt::default(),
        post_processing,
    };

    fastnbt::to_bytes(&chunk).context("Failed to serialize empty chunk NBT")
}

fn build_sections(
    columns: &ChunkHeights,
    max_height: i32,
    terrain: &TerrainConfig,
) -> Vec<SectionNbt> {
    let mut sections = Vec::new();
    let min_section = BEDROCK_Y.div_euclid(SECTION_SIDE as i32);
    let max_section = max(min_section, max_height.div_euclid(SECTION_SIDE as i32));

    let default_biome = terrain.base_biome();
    let default_top_block = terrain.top_layer_block();
    let bottom_block = terrain.bottom_layer_block();
    let top_thickness = terrain.top_layer_thickness();

    let mut column_settings = Vec::with_capacity(SECTION_SIDE * SECTION_SIDE);
    for local_z in 0..SECTION_SIDE {
        for local_x in 0..SECTION_SIDE {
            let height = columns.column(local_x, local_z);
            let overlay = columns.overlay(local_x, local_z).cloned();
            let (biome, top_block, cliff) = match height {
                Some(surface) => {
                    let (biome, cliff) = terrain.biome_and_cliff_for_height(surface);
                    let top_block = terrain.top_block_for_height(surface);
                    (biome, top_block, cliff)
                }
                None => (
                    Arc::clone(&default_biome),
                    Arc::clone(&default_top_block),
                    None,
                ),
            };
            let biome = overlay
                .as_ref()
                .and_then(|o| o.biome_override())
                .unwrap_or(biome);
            let top_block = overlay
                .as_ref()
                .and_then(|o| o.surface_block_override())
                .unwrap_or(top_block);
            let slope_degrees = match (&height, &cliff) {
                (Some(_), Some(settings)) => columns.slope(local_x, local_z, settings),
                _ => 0.0,
            };
            let column_top_thickness = overlay
                .as_ref()
                .and_then(|o| o.top_thickness_override())
                .unwrap_or(top_thickness)
                .max(1);
            let biome_min_y = height.map(|surface| {
                let min_y = surface as i64 - column_top_thickness as i64 + 1;
                min_y.clamp(i32::MIN as i64, i32::MAX as i64) as i32
            });
            column_settings.push(ColumnSettings {
                height,
                biome,
                top_block,
                slope_degrees,
                cliff,
                top_thickness: column_top_thickness,
                bottom_block_override: overlay.as_ref().and_then(|o| o.subsurface_block_override()),
                biome_min_y,
            });
        }
    }

    for section_y in min_section..=max_section {
        let mut builder = SectionBuilder::new(section_y as i8);
        for local_y in 0..SECTION_SIDE {
            let world_y = section_y * SECTION_SIDE as i32 + local_y as i32;
            for local_z in 0..SECTION_SIDE {
                for local_x in 0..SECTION_SIDE {
                    let column = &column_settings[local_z * SECTION_SIDE + local_x];
                    let block = block_for(world_y, column, &bottom_block);
                    let biome = column.biome_for_y(world_y, &default_biome);
                    builder.set(local_x, local_y, local_z, block, biome);
                }
            }
        }

        if let Some(section) = builder.finish() {
            sections.push(section);
        }
    }

    sections
}

fn build_heightmaps(columns: &ChunkHeights) -> HeightmapsNbt {
    let mut heights = Vec::with_capacity(SECTION_SIDE * SECTION_SIDE);
    for z in 0..SECTION_SIDE {
        for x in 0..SECTION_SIDE {
            let column_height = columns.column(x, z).unwrap_or(BEDROCK_Y);
            let relative = (column_height - BEDROCK_Y + 1).max(0) as u64;
            heights.push(relative);
        }
    }

    heightmaps_from_values(&heights)
}

fn empty_heightmaps() -> HeightmapsNbt {
    let height = (BEDROCK_Y - BEDROCK_Y + 1).max(0) as u64;
    let heights = vec![height; SECTION_SIDE * SECTION_SIDE];
    heightmaps_from_values(&heights)
}

fn heightmaps_from_values(values: &[u64]) -> HeightmapsNbt {
    let max_range = (MAX_WORLD_Y - BEDROCK_Y + 2) as usize;
    let bits = bits_for_range(max_range);
    let data = pack_unsigned(values, bits);
    HeightmapsNbt {
        motion_blocking: LongArray::new(data),
    }
}

fn empty_post_processing_lists() -> Vec<Vec<i16>> {
    let mut lists = Vec::with_capacity(POST_PROCESSING_SECTION_COUNT);
    for _ in 0..POST_PROCESSING_SECTION_COUNT {
        lists.push(Vec::new());
    }
    lists
}

fn bits_for_range(size: usize) -> usize {
    if size <= 1 {
        1
    } else {
        (usize::BITS - (size - 1).leading_zeros()) as usize
    }
}

fn pack_unsigned(values: &[u64], bits: usize) -> Vec<i64> {
    assert!(bits > 0 && bits <= 64);
    let values_per_long = 64 / bits;
    let mut longs = vec![0i64; (values.len() + values_per_long - 1) / values_per_long];
    let mask_u64 = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let mask_i64 = mask_u64 as i64;
    for (i, &value) in values.iter().enumerate() {
        let idx = i / values_per_long;
        let offset = (i % values_per_long) * bits;
        let clamped = (value & mask_u64) as i64;
        longs[idx] |= (clamped & mask_i64) << offset;
    }
    longs
}

struct ColumnSettings {
    height: Option<i32>,
    biome: Arc<str>,
    top_block: Arc<str>,
    slope_degrees: f32,
    cliff: Option<CliffSettings>,
    top_thickness: u32,
    bottom_block_override: Option<Arc<str>>,
    biome_min_y: Option<i32>,
}

impl ColumnSettings {
    fn cliff_block_override(&self) -> Option<Arc<str>> {
        let settings = self.cliff.as_ref()?;
        if self.slope_degrees as f64 >= settings.angle_threshold_degrees {
            Some(Arc::clone(&settings.block))
        } else {
            None
        }
    }

    fn biome_for_y<'a>(&'a self, world_y: i32, base_biome: &'a Arc<str>) -> &'a Arc<str> {
        match self.biome_min_y {
            Some(min_y) if world_y >= min_y => &self.biome,
            _ => base_biome,
        }
    }
}

fn block_for(world_y: i32, column: &ColumnSettings, default_bottom_block: &Arc<str>) -> BlockId {
    if world_y <= BEDROCK_Y {
        return BlockId::Bedrock;
    }
    let Some(surface) = column.height else {
        return BlockId::Air;
    };
    if world_y > surface {
        return BlockId::Air;
    }
    let top_thickness_i32 = column.top_thickness.max(1).min(i32::MAX as u32) as i32;
    let depth = surface - world_y;
    if depth < top_thickness_i32 {
        if let Some(block) = column.cliff_block_override() {
            return BlockId::Named(block);
        }
        BlockId::Named(Arc::clone(&column.top_block))
    } else {
        let bottom = column
            .bottom_block_override
            .as_ref()
            .unwrap_or(default_bottom_block);
        BlockId::Named(Arc::clone(bottom))
    }
}

fn biome_index(x: usize, y: usize, z: usize) -> usize {
    let bx = x / BIOME_SCALE;
    let by = y / BIOME_SCALE;
    let bz = z / BIOME_SCALE;
    by * BIOME_SIDE * BIOME_SIDE + bz * BIOME_SIDE + bx
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum BlockId {
    Air,
    Bedrock,
    Named(Arc<str>),
}

impl BlockId {
    fn name(&self) -> &str {
        match self {
            BlockId::Air => "minecraft:air",
            BlockId::Bedrock => "minecraft:bedrock",
            BlockId::Named(name) => name.as_ref(),
        }
    }
}

struct SectionBuilder {
    y: i8,
    palette: PaletteBuilder,
    indices: Vec<u16>,
    has_blocks: bool,
    biome_palette: BiomePaletteBuilder,
    biome_indices: Vec<u16>,
}

impl SectionBuilder {
    fn new(y: i8) -> Self {
        Self {
            y,
            palette: PaletteBuilder::new(),
            indices: vec![0; BLOCKS_PER_SECTION],
            has_blocks: false,
            biome_palette: BiomePaletteBuilder::new(),
            biome_indices: vec![0; BIOME_ENTRIES_PER_SECTION],
        }
    }

    fn set(&mut self, x: usize, y: usize, z: usize, block: BlockId, biome: &Arc<str>) {
        let palette_index = self.palette.index(&block);
        let idx = y * SECTION_SIDE * SECTION_SIDE + z * SECTION_SIDE + x;
        self.indices[idx] = palette_index;
        if block != BlockId::Air {
            self.has_blocks = true;
        }
        let biome_idx = biome_index(x, y, z);
        let biome_palette_index = self.biome_palette.index(biome);
        self.biome_indices[biome_idx] = biome_palette_index;
    }

    fn finish(self) -> Option<SectionNbt> {
        if !self.has_blocks {
            return None;
        }
        Some(SectionNbt {
            y: self.y,
            block_states: BlockStatesNbt::from_palette(self.palette, &self.indices),
            biomes: BiomesNbt::from_palette(self.biome_palette, &self.biome_indices),
        })
    }
}

struct PaletteBuilder {
    entries: Vec<BlockId>,
    lookup: HashMap<BlockId, u16>,
}

impl PaletteBuilder {
    fn new() -> Self {
        Self {
            entries: vec![BlockId::Air],
            lookup: HashMap::from([(BlockId::Air, 0)]),
        }
    }

    fn index(&mut self, block: &BlockId) -> u16 {
        if let Some(idx) = self.lookup.get(block) {
            *idx
        } else {
            let idx = self.entries.len() as u16;
            self.entries.push(block.clone());
            self.lookup.insert(block.clone(), idx);
            idx
        }
    }
}

struct BiomePaletteBuilder {
    entries: Vec<Arc<str>>,
    lookup: HashMap<Arc<str>, u16>,
}

impl BiomePaletteBuilder {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            lookup: HashMap::new(),
        }
    }

    fn index(&mut self, biome: &Arc<str>) -> u16 {
        if let Some(idx) = self.lookup.get(biome) {
            *idx
        } else {
            let idx = self.entries.len() as u16;
            self.entries.push(Arc::clone(biome));
            self.lookup.insert(Arc::clone(biome), idx);
            idx
        }
    }
}

#[derive(Serialize)]
struct ChunkNbt {
    #[serde(rename = "DataVersion")]
    data_version: i32,
    #[serde(rename = "LastUpdate")]
    last_update: i64,
    #[serde(rename = "InhabitedTime")]
    inhabited_time: i64,
    #[serde(rename = "xPos")]
    x_pos: i32,
    #[serde(rename = "zPos")]
    z_pos: i32,
    #[serde(rename = "yPos")]
    y_pos: i32,
    #[serde(rename = "Status")]
    status: String,
    sections: Vec<SectionNbt>,
    #[serde(rename = "Heightmaps")]
    heightmaps: HeightmapsNbt,
    #[serde(rename = "structures")]
    structures: StructuresNbt,
    #[serde(rename = "PostProcessing", skip_serializing_if = "Option::is_none")]
    post_processing: Option<Vec<Vec<i16>>>,
}

#[derive(Serialize)]
struct SectionNbt {
    #[serde(rename = "Y")]
    y: i8,
    #[serde(rename = "block_states")]
    block_states: BlockStatesNbt,
    biomes: BiomesNbt,
}

#[derive(Serialize)]
struct BlockStatesNbt {
    palette: Vec<PaletteBlock>,
    #[serde(rename = "data", skip_serializing_if = "Option::is_none")]
    data: Option<Vec<i64>>,
}

impl BlockStatesNbt {
    fn from_palette(palette: PaletteBuilder, indices: &[u16]) -> Self {
        let palette_entries = palette
            .entries
            .iter()
            .map(|id| PaletteBlock {
                name: id.name().to_string(),
            })
            .collect::<Vec<_>>();
        let data = pack_palette_indices(indices, palette_entries.len(), 4);
        Self {
            palette: palette_entries,
            data,
        }
    }
}

fn pack_palette_indices(indices: &[u16], palette_len: usize, min_bits: usize) -> Option<Vec<i64>> {
    if palette_len <= 1 {
        return None;
    }
    let bits_per_value = max(min_bits, bits_for_range(palette_len));
    let values_per_long = 64 / bits_per_value;
    let mut longs = vec![0i64; (indices.len() + values_per_long - 1) / values_per_long];
    for (i, &value) in indices.iter().enumerate() {
        let idx = i / values_per_long;
        let offset = (i % values_per_long) * bits_per_value;
        longs[idx] |= (value as i64) << offset;
    }
    Some(longs)
}

#[derive(Serialize)]
struct PaletteBlock {
    #[serde(rename = "Name")]
    name: String,
}

#[derive(Serialize)]
struct BiomesNbt {
    palette: Vec<String>,
    #[serde(rename = "data", skip_serializing_if = "Option::is_none")]
    data: Option<Vec<i64>>,
}

impl BiomesNbt {
    fn from_palette(palette: BiomePaletteBuilder, indices: &[u16]) -> Self {
        let palette_entries = palette
            .entries
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        let data = pack_palette_indices(indices, palette_entries.len(), 1);
        Self {
            palette: palette_entries,
            data,
        }
    }
}

#[derive(Serialize)]
struct HeightmapsNbt {
    #[serde(rename = "MOTION_BLOCKING")]
    motion_blocking: LongArray,
}

#[derive(Default, Serialize)]
struct StructuresNbt {
    #[serde(rename = "References")]
    references: HashMap<String, Vec<i64>>,
    #[serde(rename = "Starts")]
    starts: HashMap<String, fastnbt::Value>,
}
