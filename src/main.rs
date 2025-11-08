mod georaster;

use std::cmp::max;
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use fastanvil::Region;
use fastnbt::{self, LongArray};
use geo_types::Coord;
use georaster::GeoRaster;
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use rayon::{ThreadPoolBuilder, prelude::*};
use serde::{Deserialize, Serialize};

const BEDROCK_Y: i32 = -2048;
const MAX_WORLD_Y: i32 = 2031;
const SECTION_SIDE: usize = 16;
const BLOCKS_PER_SECTION: usize = SECTION_SIDE * SECTION_SIDE * SECTION_SIDE;
const DATA_VERSION: i32 = 3120; // Minecraft 1.20.4
const META_FILE: &str = "francegen_meta.json";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args)? {
        Command::Generate(config) => {
            if let Some(threads) = config.threads {
                ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build_global()
                    .map_err(|err| anyhow!("Failed to configure thread pool: {err}"))?;
            }
            run_generate(&config)
        }
        Command::Locate(config) => run_locate(&config),
    }
}

fn run_generate(config: &GenerateConfig) -> Result<()> {
    let input = &config.input;
    let output = &config.output;

    fs::create_dir_all(output)
        .with_context(|| format!("Failed to create output directory {}", output.display()))?;

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

    let stats = builder.stats();
    if let Some(summary) = &stats {
        print_ingest_stats(summary);
    }
    let origin = builder.origin_coord();

    if config.meta_only {
        let origin =
            origin.ok_or_else(|| anyhow!("Origin not available; unable to write metadata"))?;
        let stats =
            stats.ok_or_else(|| anyhow!("No samples were ingested; metadata unavailable"))?;
        let path = write_metadata(output, origin, &stats)?;
        println!(
            "{} Saved metadata only: {}",
            "ℹ".blue().bold(),
            path.display()
        );
        println!("  Skipped region generation (--meta-only).");
        return Ok(());
    }

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

    if let (Some(stats), Some(origin)) = (stats, origin) {
        let path = write_metadata(output, origin, &stats)?;
        println!("{} Saved metadata: {}", "ℹ".blue().bold(), path.display());
    }

    Ok(())
}

fn run_locate(config: &LocateConfig) -> Result<()> {
    let metadata = load_metadata(&config.world)?;
    let mc_x = (config.real_x - metadata.origin_model_x).round() as i32;
    let mc_z = (metadata.origin_model_z - config.real_z).round() as i32;
    let chunk_x = mc_x.div_euclid(SECTION_SIDE as i32);
    let chunk_z = mc_z.div_euclid(SECTION_SIDE as i32);
    let block_x = mc_x.rem_euclid(SECTION_SIDE as i32);
    let block_z = mc_z.rem_euclid(SECTION_SIDE as i32);

    println!(
        "{} Located point ({:.3}, {:.3}) using metadata from {}",
        "ℹ".blue().bold(),
        config.real_x,
        config.real_z,
        metadata_path(&config.world).display()
    );
    println!("  Minecraft block: X={}, Z={}", mc_x, mc_z);
    println!(
        "  Chunk: ({}, {})  block-in-chunk: ({}, {})",
        chunk_x, chunk_z, block_x, block_z
    );

    if let Some(real_height) = config.real_height {
        let mc_y = dem_to_minecraft(real_height);
        println!(
            "  Height: real {:.2} m -> Minecraft Y {}",
            real_height, mc_y
        );
    } else {
        println!(
            "  Provide a real-world elevation to also convert Y (e.g. append the height value)."
        );
    }

    println!(
        "  World bounds: X [{}..{}], Z [{}..{}]",
        metadata.min_x, metadata.max_x, metadata.min_z, metadata.max_z
    );

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

const USAGE: &str = "Usage: francegen [--threads <N>] <tif-folder> <output-world>\n       francegen locate <world-dir> <real-x> <real-z> [<real-height>]";

enum Command {
    Generate(GenerateConfig),
    Locate(LocateConfig),
}

struct GenerateConfig {
    input: PathBuf,
    output: PathBuf,
    threads: Option<usize>,
    meta_only: bool,
}

struct LocateConfig {
    world: PathBuf,
    real_x: f64,
    real_z: f64,
    real_height: Option<f64>,
}

fn parse_args(args: &[String]) -> Result<Command> {
    if args.is_empty() {
        bail!("No arguments supplied.\n{USAGE}");
    }

    if args[0] == "locate" {
        return parse_locate(&args[1..]).map(Command::Locate);
    }

    parse_generate(args).map(Command::Generate)
}

fn parse_generate(args: &[String]) -> Result<GenerateConfig> {
    let mut input = None;
    let mut output = None;
    let mut threads = None;
    let mut meta_only = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--help" || arg == "-h" {
            println!("{USAGE}");
            std::process::exit(0);
        } else if arg == "--threads" {
            i += 1;
            if i >= args.len() {
                bail!("Missing value for --threads\n{USAGE}");
            }
            threads = Some(parse_threads(&args[i])?);
        } else if let Some(value) = arg.strip_prefix("--threads=") {
            threads = Some(parse_threads(value)?);
        } else if arg == "--meta-only" {
            meta_only = true;
        } else if let Some(value) = arg.strip_prefix("--meta-only=") {
            meta_only = value
                .parse::<bool>()
                .map_err(|_| anyhow!("Invalid value for --meta-only (expected true/false)"))?;
        } else if input.is_none() {
            input = Some(PathBuf::from(arg));
        } else if output.is_none() {
            output = Some(PathBuf::from(arg));
        } else {
            bail!("Unexpected argument: {arg}\n{USAGE}");
        }
        i += 1;
    }

    let input = input.ok_or_else(|| anyhow!("Missing input directory argument.\n{USAGE}"))?;
    let output = output.ok_or_else(|| anyhow!("Missing output directory argument.\n{USAGE}"))?;

    Ok(GenerateConfig {
        input,
        output,
        threads,
        meta_only,
    })
}

fn parse_locate(args: &[String]) -> Result<LocateConfig> {
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        println!("{USAGE}");
        std::process::exit(0);
    }

    if args.len() < 3 {
        bail!("locate requires <world-dir> <real-x> <real-z> [<real-height>]\n{USAGE}");
    }

    let world = PathBuf::from(&args[0]);
    let real_x = args[1]
        .parse::<f64>()
        .map_err(|_| anyhow!("Invalid real-x '{}'", args[1]))?;
    let real_z = args[2]
        .parse::<f64>()
        .map_err(|_| anyhow!("Invalid real-z '{}'", args[2]))?;
    let real_height = if args.len() > 3 {
        Some(
            args[3]
                .parse::<f64>()
                .map_err(|_| anyhow!("Invalid real-height '{}'", args[3]))?,
        )
    } else {
        None
    };

    Ok(LocateConfig {
        world,
        real_x,
        real_z,
        real_height,
    })
}

fn parse_threads(value: &str) -> Result<usize> {
    let threads: usize = value
        .parse()
        .map_err(|_| anyhow!("Invalid thread count '{value}'"))?;
    if threads == 0 {
        bail!("Thread count must be > 0");
    }
    Ok(threads)
}

#[derive(Clone)]
struct WorldStats {
    width: usize,
    depth: usize,
    min_height: f64,
    max_height: f64,
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
    center_x: f64,
    center_z: f64,
}

#[derive(Serialize, Deserialize)]
struct WorldMetadata {
    origin_model_x: f64,
    origin_model_z: f64,
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
    min_height: f64,
    max_height: f64,
}

impl WorldMetadata {
    fn from_stats(origin: Coord, stats: &WorldStats) -> Self {
        Self {
            origin_model_x: origin.x,
            origin_model_z: origin.y,
            min_x: stats.min_x,
            max_x: stats.max_x,
            min_z: stats.min_z,
            max_z: stats.max_z,
            min_height: stats.min_height,
            max_height: stats.max_height,
        }
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

fn print_ingest_stats(stats: &WorldStats) {
    println!();
    println!(
        "{} Expected world size: {} x {} blocks ({:.1} x {:.1} chunks)",
        "ℹ".blue().bold(),
        stats.width,
        stats.depth,
        stats.width as f64 / 16.0,
        stats.depth as f64 / 16.0
    );
    println!(
        "  {} Heights: min {:.2} m, max {:.2} m",
        "↕".blue(),
        stats.min_height,
        stats.max_height
    );
    println!(
        "  {} World bounds X:[{}..{}], Z:[{}..{}]",
        "⬚".blue(),
        stats.min_x,
        stats.max_x,
        stats.min_z,
        stats.max_z
    );
    println!(
        "  {} Center: ({:.1}, {:.1})",
        "◎".blue(),
        stats.center_x,
        stats.center_z
    );
    println!();
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

fn write_metadata(output: &Path, origin: Coord, stats: &WorldStats) -> Result<PathBuf> {
    let metadata = WorldMetadata::from_stats(origin, stats);
    let path = metadata_path(output);
    let json = serde_json::to_string_pretty(&metadata)?;
    fs::write(&path, json)
        .with_context(|| format!("Failed to write metadata {}", path.display()))?;
    Ok(path)
}

fn load_metadata(world: &Path) -> Result<WorldMetadata> {
    let meta_path = metadata_path(world);
    let data = fs::read_to_string(&meta_path)
        .with_context(|| format!("Failed to read metadata {}", meta_path.display()))?;
    let metadata = serde_json::from_str(&data)
        .with_context(|| format!("Failed to parse metadata {}", meta_path.display()))?;
    Ok(metadata)
}

fn metadata_path(base: &Path) -> PathBuf {
    if base.is_dir() {
        base.join(META_FILE)
    } else {
        base.to_path_buf()
    }
}

struct WorldBuilder {
    origin: Option<Coord>,
    columns: HashMap<(i32, i32), i32>,
    samples: usize,
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
    min_height: f64,
    max_height: f64,
}

impl WorldBuilder {
    fn new() -> Self {
        Self {
            origin: None,
            columns: HashMap::new(),
            samples: 0,
            min_x: i32::MAX,
            max_x: i32::MIN,
            min_z: i32::MAX,
            max_z: i32::MIN,
            min_height: f64::INFINITY,
            max_height: f64::NEG_INFINITY,
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
                self.update_stats(world_x, world_z, height_value);
            }
        }
    }

    fn update_stats(&mut self, x: i32, z: i32, height_value: f64) {
        self.min_x = self.min_x.min(x);
        self.max_x = self.max_x.max(x);
        self.min_z = self.min_z.min(z);
        self.max_z = self.max_z.max(z);
        self.min_height = self.min_height.min(height_value);
        self.max_height = self.max_height.max(height_value);
    }

    fn stats(&self) -> Option<WorldStats> {
        if self.columns.is_empty() {
            return None;
        }
        let width = (self.max_x - self.min_x + 1).max(0) as usize;
        let depth = (self.max_z - self.min_z + 1).max(0) as usize;
        let center_x = (self.min_x + self.max_x) as f64 / 2.0;
        let center_z = (self.min_z + self.max_z) as f64 / 2.0;
        Some(WorldStats {
            width,
            depth,
            min_height: self.min_height,
            max_height: self.max_height,
            min_x: self.min_x,
            max_x: self.max_x,
            min_z: self.min_z,
            max_z: self.max_z,
            center_x,
            center_z,
        })
    }

    fn origin_coord(&self) -> Option<Coord> {
        self.origin
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
    let dz = origin.y - coord.y; // flip so geographic north (increasing model Y) maps to Minecraft -Z
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
                    if let Some(data) = build_chunk_bytes(chunk_x, chunk_z, columns)? {
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
