mod bounds;
mod chunk;
mod chunky;
mod cli;
mod config;
mod constants;
mod copc;
mod generate;
mod geo_utils;
mod georaster;
mod info;
mod locate;
mod metadata;
mod osm;
mod progress;
mod wmts;
mod world;
mod world_template;

use anyhow::{Result, anyhow};
use bounds::run_bounds;
use cli::{Command, parse_args};
use generate::run_generate;
use info::run_info;
use locate::run_locate;
use rayon::ThreadPoolBuilder;

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
        Command::Bounds(config) => run_bounds(&config),
        Command::Info(config) => run_info(&config),
    }
}
