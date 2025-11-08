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

use crate::config::TerrainConfig;
use crate::constants::{BEDROCK_Y, BLOCKS_PER_SECTION, DATA_VERSION, MAX_WORLD_Y, SECTION_SIDE};
use crate::progress::progress_bar;

const BIOME_SIDE: usize = SECTION_SIDE / 4;
const BIOME_SCALE: usize = SECTION_SIDE / BIOME_SIDE;
const BIOME_ENTRIES_PER_SECTION: usize = BIOME_SIDE * BIOME_SIDE * BIOME_SIDE;
const _: [(); SECTION_SIDE % 4] = [];

pub struct ChunkHeights {
    heights: [Option<i32>; SECTION_SIDE * SECTION_SIDE],
}

impl ChunkHeights {
    pub fn new() -> Self {
        Self {
            heights: [None; SECTION_SIDE * SECTION_SIDE],
        }
    }

    pub fn set(&mut self, x: usize, z: usize, height: i32) {
        self.heights[z * SECTION_SIDE + x] = Some(height);
    }

    pub fn column(&self, x: usize, z: usize) -> Option<i32> {
        self.heights[z * SECTION_SIDE + x]
    }

    pub fn max_height(&self) -> Option<i32> {
        self.heights.iter().copied().flatten().max()
    }
}

pub struct WriteStats {
    pub region_files: usize,
    pub chunks_written: usize,
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

    let mut per_region: HashMap<(i32, i32), Vec<(i32, i32)>> = HashMap::new();
    for (&(chunk_x, chunk_z), _) in chunks.iter() {
        let region_x = chunk_x.div_euclid(32);
        let region_z = chunk_z.div_euclid(32);
        per_region
            .entry((region_x, region_z))
            .or_default()
            .push((chunk_x, chunk_z));
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
            for (chunk_x, chunk_z) in coords {
                if let Some(columns) = chunks.get(&(chunk_x, chunk_z)) {
                    if let Some(data) = build_chunk_bytes(chunk_x, chunk_z, columns, terrain)? {
                        let local_x = chunk_x.rem_euclid(32) as usize;
                        let local_z = chunk_z.rem_euclid(32) as usize;
                        region
                            .write_chunk(local_x, local_z, &data)
                            .with_context(|| {
                                format!("Failed to write chunk at ({chunk_x}, {chunk_z})")
                            })?;
                        written += 1;
                    }
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

    let chunk = ChunkNbt {
        data_version: DATA_VERSION,
        last_update: 0,
        inhabited_time: 0,
        x_pos: chunk_x,
        z_pos: chunk_z,
        y_pos: (BEDROCK_Y.div_euclid(SECTION_SIDE as i32)) as i32,
        status: "full".to_string(),
        sections,
        heightmaps,
        structures: StructuresNbt::default(),
    };

    let bytes = fastnbt::to_bytes(&chunk).context("Failed to serialize chunk NBT")?;
    Ok(Some(bytes))
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
            let (biome, top_block) = match height {
                Some(surface) => (
                    terrain.biome_for_height(surface),
                    terrain.top_block_for_height(surface),
                ),
                None => (Arc::clone(&default_biome), Arc::clone(&default_top_block)),
            };
            column_settings.push(ColumnSettings {
                height,
                biome,
                top_block,
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
                    let block = block_for(world_y, column, top_thickness, &bottom_block);
                    builder.set(local_x, local_y, local_z, block, &column.biome);
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

    let max_range = (MAX_WORLD_Y - BEDROCK_Y + 2) as usize;
    let bits = bits_for_range(max_range);
    let data = pack_unsigned(&heights, bits);
    HeightmapsNbt {
        motion_blocking: LongArray::new(data),
    }
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
}

fn block_for(
    world_y: i32,
    column: &ColumnSettings,
    top_thickness: u32,
    bottom_block: &Arc<str>,
) -> BlockId {
    if world_y <= BEDROCK_Y {
        return BlockId::Bedrock;
    }
    let Some(surface) = column.height else {
        return BlockId::Air;
    };
    if world_y > surface {
        return BlockId::Air;
    }
    let depth = surface - world_y;
    if depth < top_thickness as i32 {
        BlockId::Named(Arc::clone(&column.top_block))
    } else {
        BlockId::Named(Arc::clone(bottom_block))
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
