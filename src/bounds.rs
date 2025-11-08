use anyhow::{Context, Result, bail};
use owo_colors::OwoColorize;

use crate::cli::BoundsConfig;
use crate::generate::collect_tifs;
use crate::georaster::GeoRaster;

pub fn run_bounds(config: &BoundsConfig) -> Result<()> {
    let mut tif_paths = collect_tifs(&config.input)?;
    if tif_paths.is_empty() {
        bail!("No .tif files found in {}", config.input.display());
    }
    tif_paths.sort();

    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_z = f64::INFINITY;
    let mut max_z = f64::NEG_INFINITY;

    for path in &tif_paths {
        let raster = GeoRaster::open(path)
            .with_context(|| format!("Failed to open GeoTIFF {}", path.display()))?;
        let extent = raster.extent();
        min_x = min_x.min(extent.min_x);
        max_x = max_x.max(extent.max_x);
        min_z = min_z.min(extent.min_z);
        max_z = max_z.max(extent.max_z);
    }

    println!(
        "{} Found {} GeoTIFF(s) in {}",
        "ℹ".blue().bold(),
        tif_paths.len(),
        config.input.display()
    );
    println!(
        "  X bounds: [{:.3} .. {:.3}] (width {:.3} m)",
        min_x,
        max_x,
        max_x - min_x
    );
    println!(
        "  Z bounds: [{:.3} .. {:.3}] (depth {:.3} m)",
        min_z,
        max_z,
        max_z - min_z
    );
    println!(
        "  Suggested flag: --bounds {:.3},{:.3},{:.3},{:.3}",
        min_x, min_z, max_x, max_z
    );

    Ok(())
}
