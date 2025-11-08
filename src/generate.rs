use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use geo_types::Coord;
use owo_colors::OwoColorize;

use crate::chunk::write_regions;
use crate::cli::GenerateConfig;
use crate::config::TerrainConfig;
use crate::constants::{BEDROCK_Y, MAX_WORLD_Y};
use crate::metadata::write_metadata;
use crate::progress::progress_bar;
use crate::world::{WorldBuilder, WorldStats};

pub fn run_generate(config: &GenerateConfig) -> Result<()> {
    let input = &config.input;
    let output = &config.output;

    fs::create_dir_all(output)
        .with_context(|| format!("Failed to create output directory {}", output.display()))?;

    let mut tif_paths = collect_tifs(input)?;
    if tif_paths.is_empty() {
        bail!("No .tif files found in {}", input.display());
    }
    tif_paths.sort();

    if let Some(bounds) = config.bounds {
        println!(
            "{} Limiting to model bounds X:[{:.3}..{:.3}] Z:[{:.3}..{:.3}]",
            "ℹ".blue().bold(),
            bounds.min_x,
            bounds.max_x,
            bounds.min_z,
            bounds.max_z
        );
    }

    let ingest_pb = progress_bar(tif_paths.len() as u64, "Ingesting tiles");
    let mut builder = WorldBuilder::new(config.bounds);
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
    ingest_pb.finish_and_clear();
    println!(
        "{} Ingested {} GeoTIFF(s)",
        "✔".green().bold(),
        tif_paths.len()
    );

    let stats = builder.stats();
    let origin = builder.origin_coord();
    if let Some(summary) = &stats {
        print_ingest_stats(summary, origin);
    }

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

    let terrain_config = match &config.terrain_config {
        Some(path) => {
            let config = TerrainConfig::load_from_path(path)?;
            println!(
                "{} Loaded terrain config: {}",
                "ℹ".blue().bold(),
                path.display()
            );
            config
        }
        None => TerrainConfig::default(),
    };

    let sample_count = builder.sample_count();
    let column_count = builder.column_count();
    let max_radius = terrain_config.max_smoothing_radius();
    let chunks = builder.into_chunks(max_radius);
    let chunk_count = chunks.len();

    let write_stats = write_regions(output, &chunks, &terrain_config)?;
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

pub fn collect_tifs(dir: &Path) -> Result<Vec<PathBuf>> {
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

fn print_ingest_stats(stats: &WorldStats, origin: Option<Coord>) {
    println!();
    println!(
        "{} Expected world size: {} x {} blocks ({:.1} x {:.1} chunks)",
        "ℹ".blue().bold(),
        stats.width,
        stats.depth,
        stats.width as f64 / 16.0,
        stats.depth as f64 / 16.0
    );
    let max_allowed = (MAX_WORLD_Y - BEDROCK_Y) as f64;
    let min_clip = stats.min_height < 0.0;
    let max_clip = stats.max_height > max_allowed;
    let clip_note = if min_clip || max_clip {
        let mut parts: Vec<String> = vec![];
        if min_clip {
            parts.push("below 0 m".to_string());
        }
        if max_clip {
            parts.push(format!("above {:.0} m", max_allowed));
        }
        format!(
            " {}",
            format!("⚠ clipped {}", parts.join(" & ")).yellow().bold()
        )
    } else {
        String::new()
    };
    println!(
        "  {} Heights: min {:.2} m, max {:.2} m{}",
        "↕".blue(),
        stats.min_height,
        stats.max_height,
        clip_note
    );
    println!(
        "  {} World bounds X:[{}..{}], Z:[{}..{}]",
        "⬚".blue(),
        stats.min_x,
        stats.max_x,
        stats.min_z,
        stats.max_z
    );
    if let Some(origin) = origin {
        println!(
            "  {} Origin (model): ({:.3}, {:.3}) → MC (0, 0)",
            "◎".blue(),
            origin.x,
            origin.y
        );
    }
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
