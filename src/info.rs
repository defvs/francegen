use anyhow::Result;
use owo_colors::OwoColorize;

use crate::chunky::print_chunky_reminder;
use crate::cli::InfoConfig;
use crate::metadata::{load_metadata, metadata_path};

pub fn run_info(config: &InfoConfig) -> Result<()> {
    let metadata = load_metadata(&config.world)?;
    let meta_path = metadata_path(&config.world);
    let center_x = (metadata.min_x + metadata.max_x) as f64 / 2.0;
    let center_z = (metadata.min_z + metadata.max_z) as f64 / 2.0;

    println!(
        "{} World metadata: {}",
        "ℹ".blue().bold(),
        meta_path.display()
    );
    println!(
        "  {} Heights: min {:.2} m, max {:.2} m",
        "↕".blue(),
        metadata.min_height,
        metadata.max_height
    );
    println!(
        "  {} World bounds X:[{}..{}], Z:[{}..{}]",
        "⬚".blue(),
        metadata.min_x,
        metadata.max_x,
        metadata.min_z,
        metadata.max_z
    );
    println!(
        "  {} Origin (model): ({:.3}, {:.3}) → MC (0, 0)",
        "◎".blue(),
        metadata.origin_model_x,
        metadata.origin_model_z
    );
    println!(
        "  {} Center: ({:.1}, {:.1})",
        "◎".blue(),
        center_x,
        center_z
    );

    print_chunky_reminder(
        metadata.min_x,
        metadata.min_z,
        metadata.max_x,
        metadata.max_z,
    );

    Ok(())
}
