use anyhow::Result;
use owo_colors::OwoColorize;

use crate::cli::LocateConfig;
use crate::constants::SECTION_SIDE;
use crate::metadata::{load_metadata, metadata_path};
use crate::world::dem_to_minecraft;

pub fn run_locate(config: &LocateConfig) -> Result<()> {
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
