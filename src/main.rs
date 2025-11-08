mod chunk;
mod cli;
mod constants;
mod generate;
mod georaster;
mod locate;
mod metadata;
mod progress;
mod world;

use anyhow::{Result, anyhow};
use cli::{Command, parse_args};
use generate::run_generate;
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
    }
}
