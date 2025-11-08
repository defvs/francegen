use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

use crate::world::ModelBounds;

const USAGE: &str = "Usage: francegen [--threads <N>] [--config <file>] [--bounds <min_x,min_z,max_x,max_z>] <tif-folder> <output-world>\n       francegen locate <world-dir> <real-x> <real-z> [<real-height>]\n       francegen bounds <tif-folder>";

pub enum Command {
    Generate(GenerateConfig),
    Locate(LocateConfig),
    Bounds(BoundsConfig),
}

pub struct GenerateConfig {
    pub input: PathBuf,
    pub output: PathBuf,
    pub threads: Option<usize>,
    pub meta_only: bool,
    pub terrain_config: Option<PathBuf>,
    pub bounds: Option<ModelBounds>,
}

pub struct LocateConfig {
    pub world: PathBuf,
    pub real_x: f64,
    pub real_z: f64,
    pub real_height: Option<f64>,
}

pub struct BoundsConfig {
    pub input: PathBuf,
}

pub fn parse_args(args: &[String]) -> Result<Command> {
    if args.is_empty() {
        bail!("No arguments supplied.\n{USAGE}");
    }

    if args[0] == "locate" {
        return parse_locate(&args[1..]).map(Command::Locate);
    }

    if args[0] == "bounds" {
        return parse_bounds_command(&args[1..]).map(Command::Bounds);
    }

    parse_generate(args).map(Command::Generate)
}

fn parse_generate(args: &[String]) -> Result<GenerateConfig> {
    let mut input = None;
    let mut output = None;
    let mut threads = None;
    let mut meta_only = false;
    let mut terrain_config = None;
    let mut bounds = None;

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
        } else if arg == "--config" {
            i += 1;
            if i >= args.len() {
                bail!("Missing value for --config\n{USAGE}");
            }
            terrain_config = Some(PathBuf::from(&args[i]));
        } else if let Some(value) = arg.strip_prefix("--config=") {
            terrain_config = Some(PathBuf::from(value));
        } else if arg == "--bounds" {
            i += 1;
            if i >= args.len() {
                bail!("Missing value for --bounds\n{USAGE}");
            }
            bounds = Some(parse_bounds(&args[i])?);
        } else if let Some(value) = arg.strip_prefix("--bounds=") {
            bounds = Some(parse_bounds(value)?);
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
        terrain_config,
        bounds,
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

fn parse_bounds_command(args: &[String]) -> Result<BoundsConfig> {
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        println!("{USAGE}");
        std::process::exit(0);
    }

    if args.len() != 1 {
        bail!("bounds requires exactly one argument: <tif-folder>\n{USAGE}");
    }

    Ok(BoundsConfig {
        input: PathBuf::from(&args[0]),
    })
}

fn parse_bounds(value: &str) -> Result<ModelBounds> {
    let parts: Vec<&str> = value.split(',').collect();
    if parts.len() != 4 {
        bail!("--bounds expects 4 comma-separated numbers: min_x,min_z,max_x,max_z");
    }
    let min_x = parts[0]
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("Invalid min_x '{}'", parts[0].trim()))?;
    let min_z = parts[1]
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("Invalid min_z '{}'", parts[1].trim()))?;
    let max_x = parts[2]
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("Invalid max_x '{}'", parts[2].trim()))?;
    let max_z = parts[3]
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("Invalid max_z '{}'", parts[3].trim()))?;

    if min_x > max_x {
        bail!("--bounds min_x must be <= max_x");
    }
    if min_z > max_z {
        bail!("--bounds min_z must be <= max_z");
    }

    Ok(ModelBounds {
        min_x,
        max_x,
        min_z,
        max_z,
    })
}
