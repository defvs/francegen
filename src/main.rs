mod georaster;

use std::cmp::max;
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use fastanvil::Region;
use fastnbt::{self, LongArray};
use geo_types::Coord;
use georaster::GeoRaster;
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use serde::Serialize;

const BEDROCK_Y: i32 = -2048;
const MAX_WORLD_Y: i32 = 2031;
const SECTION_SIDE: usize = 16;
const BLOCKS_PER_SECTION: usize = SECTION_SIDE * SECTION_SIDE * SECTION_SIDE;
const DATA_VERSION: i32 = 3120; // Minecraft 1.20.4

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        eprintln!("Usage: francegen <tif-folder> <output-world>");
        std::process::exit(1);
    }
    let output = PathBuf::from(args.pop().unwrap());
    let input = PathBuf::from(args.pop().unwrap());
    run(&input, &output)
}

fn run(input: &Path, output: &Path) -> Result<()> {
    let mut tif_paths = collect_tifs(input)?;
    if tif_paths.is_empty() {
        bail!("No .tif files found in {}", input.display());
    }
    tif_paths.sort();

    let ingest_pb = progress_bar(tif_paths.len() as u64, "Ingesting tiles");
    let mut builder = WorldBuilder::new();
    for path in &tif_paths {
        let msg = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("tile")
            .to_string();
        ingest_pb.set_message(msg);
        builder.ingest(path)?;
        ingest_pb.inc(1);
    }
    ingest_pb.finish_with_message("GeoTIFFs loaded");

    let sample_count = builder.sample_count();
    let column_count = builder.column_count();
    let chunks = builder.into_chunks();
    let chunk_count = chunks.len();

    let write_stats = write_regions(output, &chunks)?;
    print_summary(Summary {
        input_dir: input,
        output_dir: output,
        tif_files: tif_paths.len(),
        samples: sample_count,
        columns: column_count,
        chunks: chunk_count,
        region_files: write_stats.region_files,
        chunks_written: write_stats.chunks_written,
    });

    Ok(())
}

fn collect_tifs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("Failed to read input directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .map(|ext| ext.eq_ignore_ascii_case("tif") || ext.eq_ignore_ascii_case("tiff"))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
    Ok(out)
}

fn progress_bar(total: u64, label: &str) -> ProgressBar {
    if total == 0 {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template(
                "{prefix:.bold} {spinner} {elapsed_precise} (eta {eta}) {msg}",
            )
            .expect("valid spinner template"),
        );
        pb.set_prefix(label.to_string());
        pb.enable_steady_tick(Duration::from_millis(100));
        pb
    } else {
        let pb = ProgressBar::new(total);
        pb.set_style(
            ProgressStyle::with_template(
                "{prefix:.bold} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} eta {eta_precise} {msg}",
            )
            .expect("valid bar template")
            .progress_chars("##-"),
        );
        pb.set_prefix(label.to_string());
        pb
    }
}

struct Summary<'a> {
    input_dir: &'a Path,
    output_dir: &'a Path,
    tif_files: usize,
    samples: usize,
    columns: usize,
    chunks: usize,
    region_files: usize,
    chunks_written: usize,
}

fn print_summary(summary: Summary<'_>) {
    println!();
    println!(
        "{} {}",
        "✔".green().bold(),
        "World generation complete".green().bold()
    );
    println!(
        "  {} {}",
        "Input directory:".bright_black(),
        summary.input_dir.display()
    );
    println!(
        "  {} {}",
        "Output directory:".bright_black(),
        summary.output_dir.display()
    );
    println!(
        "  {} {:>8}    {} {:>10}",
        "Tiles".cyan().bold(),
        summary.tif_files,
        "Samples".cyan().bold(),
        summary.samples
    );
    println!(
        "  {} {:>8}    {} {:>10}",
        "Columns".purple().bold(),
        summary.columns,
        "Chunks queued".purple().bold(),
        summary.chunks
    );
    println!(
        "  {} {:>8}    {} {:>10}",
        "Region files".yellow().bold(),
        summary.region_files,
        "Chunks written".yellow().bold(),
        summary.chunks_written
    );
}

struct WorldBuilder {
    origin: Option<Coord>,
    columns: HashMap<(i32, i32), i32>,
    samples: usize,
}

impl WorldBuilder {
    fn new() -> Self {
        Self {
            origin: None,
            columns: HashMap::new(),
            samples: 0,
        }
    }

    fn sample_count(&self) -> usize {
        self.samples
    }

    fn column_count(&self) -> usize {
        self.columns.len()
    }

    fn ingest(&mut self, path: &Path) -> Result<()> {
        let raster = GeoRaster::open(path)?;
        if self.origin.is_none() {
            let origin = raster.origin();
            self.origin = Some(origin);
            println!(
                "Using GeoTIFF origin ({:.3}, {:.3}) as world (0,0)",
                origin.x, origin.y
            );
        }
        self.ingest_raster(&raster);
        Ok(())
    }

    fn ingest_raster(&mut self, raster: &GeoRaster) {
        let origin = self.origin.expect("origin initialized");
        for row in 0..raster.height() {
            for col in 0..raster.width() {
                let Some(height_value) = raster.sample(col, row) else {
                    continue;
                };
                self.samples += 1;
                let coord = raster.coord_for(col, row);
                let (world_x, world_z) = model_to_world(&origin, &coord);
                let mc_height = dem_to_minecraft(height_value);
                self.columns.insert((world_x, world_z), mc_height);
            }
        }
    }

    fn into_chunks(self) -> HashMap<(i32, i32), ChunkHeights> {
        let mut chunks: HashMap<(i32, i32), ChunkHeights> = HashMap::new();
        for ((x, z), height) in self.columns {
            let chunk_x = x.div_euclid(SECTION_SIDE as i32);
            let chunk_z = z.div_euclid(SECTION_SIDE as i32);
            let local_x = x.rem_euclid(SECTION_SIDE as i32) as usize;
            let local_z = z.rem_euclid(SECTION_SIDE as i32) as usize;
            let entry = chunks
                .entry((chunk_x, chunk_z))
                .or_insert_with(ChunkHeights::new);
            entry.set(local_x, local_z, height);
        }
        chunks
    }
}

fn model_to_world(origin: &Coord, coord: &Coord) -> (i32, i32) {
    let dx = coord.x - origin.x;
    let dz = coord.y - origin.y;
    (dx.round() as i32, dz.round() as i32)
}

fn dem_to_minecraft(value: f64) -> i32 {
    let height = BEDROCK_Y as f64 + value;
    height.round().clamp(BEDROCK_Y as f64, MAX_WORLD_Y as f64) as i32
}

struct ChunkHeights {
    heights: [Option<i32>; SECTION_SIDE * SECTION_SIDE],
}

impl ChunkHeights {
    fn new() -> Self {
        Self {
            heights: [None; SECTION_SIDE * SECTION_SIDE],
        }
    }

    fn set(&mut self, x: usize, z: usize, height: i32) {
        let idx = z * SECTION_SIDE + x;
        self.heights[idx] = Some(height);
    }

    fn column(&self, x: usize, z: usize) -> Option<i32> {
        self.heights[z * SECTION_SIDE + x]
    }

    fn max_height(&self) -> Option<i32> {
        self.heights.iter().copied().flatten().max()
    }
}

struct WriteStats {
    region_files: usize,
    chunks_written: usize,
}

fn write_regions(output: &Path, chunks: &HashMap<(i32, i32), ChunkHeights>) -> Result<WriteStats> {
    if chunks.is_empty() {
        return Ok(WriteStats {
            region_files: 0,
            chunks_written: 0,
        });
    }

    let region_dir = output.join("region");
    fs::create_dir_all(&region_dir)
        .with_context(|| format!("Failed to create region directory {}", region_dir.display()))?;

    let mut region_cache: HashMap<(i32, i32), Region<File>> = HashMap::new();
    let mut region_creations = 0usize;
    let mut written_chunks = 0usize;

    let mut keys: Vec<(i32, i32)> = chunks.keys().copied().collect();
    keys.sort();

    let pb = progress_bar(keys.len() as u64, "Writing chunks");
    for (chunk_x, chunk_z) in keys {
        pb.set_message(format!("Chunk {chunk_x},{chunk_z}"));
        let columns = &chunks[&(chunk_x, chunk_z)];
        let Some(chunk_bytes) = build_chunk_bytes(chunk_x, chunk_z, columns)? else {
            pb.inc(1);
            continue;
        };

        let region_x = chunk_x.div_euclid(32);
        let region_z = chunk_z.div_euclid(32);
        let local_x = chunk_x.rem_euclid(32) as usize;
        let local_z = chunk_z.rem_euclid(32) as usize;

        use std::collections::hash_map::Entry;
        let region = match region_cache.entry((region_x, region_z)) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                region_creations += 1;
                let region = create_region(&region_dir, region_x, region_z)?;
                entry.insert(region)
            }
        };

        region
            .write_chunk(local_x, local_z, &chunk_bytes)
            .with_context(|| format!("Failed to write chunk at ({chunk_x}, {chunk_z})"))?;
        written_chunks += 1;
        pb.inc(1);
    }

    pb.finish_with_message("Chunks finalized");
    drop(region_cache);
    Ok(WriteStats {
        region_files: region_creations,
        chunks_written: written_chunks,
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
) -> Result<Option<Vec<u8>>> {
    let max_height = match columns.max_height() {
        Some(value) => value,
        None => return Ok(None),
    };

    let sections = build_sections(columns, max_height);
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

fn build_sections(columns: &ChunkHeights, max_height: i32) -> Vec<SectionNbt> {
    let mut sections = Vec::new();
    let min_section = BEDROCK_Y.div_euclid(SECTION_SIDE as i32);
    let max_section = max(min_section, max_height.div_euclid(SECTION_SIDE as i32));

    for section_y in min_section..=max_section {
        let mut builder = SectionBuilder::new(section_y as i8);
        for local_y in 0..SECTION_SIDE {
            let world_y = section_y * SECTION_SIDE as i32 + local_y as i32;
            for local_z in 0..SECTION_SIDE {
                for local_x in 0..SECTION_SIDE {
                    let column_height = columns.column(local_x, local_z);
                    let block = block_for(world_y, column_height);
                    builder.set(local_x, local_y, local_z, block);
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

fn block_for(world_y: i32, height: Option<i32>) -> BlockId {
    if world_y <= BEDROCK_Y {
        return BlockId::Bedrock;
    }
    let Some(surface) = height else {
        return BlockId::Air;
    };
    if world_y > surface {
        return BlockId::Air;
    }
    let depth = surface - world_y;
    match depth {
        0 => BlockId::Grass,
        1..=3 => BlockId::Dirt,
        _ => BlockId::Stone,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BlockId {
    Air,
    Bedrock,
    Stone,
    Dirt,
    Grass,
}

impl BlockId {
    fn name(self) -> &'static str {
        match self {
            BlockId::Air => "minecraft:air",
            BlockId::Bedrock => "minecraft:bedrock",
            BlockId::Stone => "minecraft:stone",
            BlockId::Dirt => "minecraft:dirt",
            BlockId::Grass => "minecraft:grass_block",
        }
    }
}

struct SectionBuilder {
    y: i8,
    palette: PaletteBuilder,
    indices: Vec<u16>,
    has_blocks: bool,
}

impl SectionBuilder {
    fn new(y: i8) -> Self {
        Self {
            y,
            palette: PaletteBuilder::new(),
            indices: vec![0; BLOCKS_PER_SECTION],
            has_blocks: false,
        }
    }

    fn set(&mut self, x: usize, y: usize, z: usize, block: BlockId) {
        let palette_index = self.palette.index(block);
        let idx = y * SECTION_SIDE * SECTION_SIDE + z * SECTION_SIDE + x;
        self.indices[idx] = palette_index;
        if block != BlockId::Air {
            self.has_blocks = true;
        }
    }

    fn finish(self) -> Option<SectionNbt> {
        if !self.has_blocks {
            return None;
        }
        Some(SectionNbt {
            y: self.y,
            block_states: BlockStatesNbt::from_palette(self.palette, &self.indices),
            biomes: BiomesNbt::uniform("minecraft:plains"),
        })
    }
}

struct PaletteBuilder {
    entries: Vec<BlockId>,
    lookup: HashMap<BlockId, u16>,
}

impl PaletteBuilder {
    fn new() -> Self {
        let mut lookup = HashMap::new();
        lookup.insert(BlockId::Air, 0);
        Self {
            entries: vec![BlockId::Air],
            lookup,
        }
    }

    fn index(&mut self, block: BlockId) -> u16 {
        if let Some(idx) = self.lookup.get(&block) {
            *idx
        } else {
            let idx = self.entries.len() as u16;
            self.entries.push(block);
            self.lookup.insert(block, idx);
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
        let data = pack_palette_indices(indices, palette_entries.len());
        Self {
            palette: palette_entries,
            data,
        }
    }
}

fn pack_palette_indices(indices: &[u16], palette_len: usize) -> Option<Vec<i64>> {
    if palette_len <= 1 {
        return None;
    }
    let bits_per_block = max(4, bits_for_range(palette_len));
    let values_per_long = 64 / bits_per_block;
    let mut longs = vec![0i64; (indices.len() + values_per_long - 1) / values_per_long];
    for (i, &value) in indices.iter().enumerate() {
        let idx = i / values_per_long;
        let offset = (i % values_per_long) * bits_per_block;
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
    palette: Vec<BiomeEntry>,
    #[serde(rename = "data", skip_serializing_if = "Option::is_none")]
    data: Option<Vec<i64>>,
}

impl BiomesNbt {
    fn uniform(name: &str) -> Self {
        Self {
            palette: vec![BiomeEntry {
                name: name.to_string(),
            }],
            data: None,
        }
    }
}

#[derive(Serialize)]
struct BiomeEntry {
    #[serde(rename = "Name")]
    name: String,
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
