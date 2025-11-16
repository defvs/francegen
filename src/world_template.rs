use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use fastnbt::{self, IntArray, Value};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

const TEMPLATE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/world_template");

pub struct SpawnSettings<'a> {
    pub spawn_x: i32,
    pub spawn_y: i32,
    pub spawn_z: i32,
    pub level_name: &'a str,
}

pub fn apply_world_template(output: &Path, spawn: &SpawnSettings<'_>) -> Result<()> {
    let template_dir = Path::new(TEMPLATE_ROOT);
    if !template_dir.exists() {
        bail!(
            "World template directory {} is missing",
            template_dir.display()
        );
    }
    copy_level_dat(template_dir, output, spawn)?;
    copy_datapacks(template_dir, output)?;
    Ok(())
}

fn copy_level_dat(template_dir: &Path, output: &Path, spawn: &SpawnSettings<'_>) -> Result<()> {
    let src = template_dir.join("level.dat");
    if !src.exists() {
        bail!("Template level.dat not found at {}", src.display());
    }
    let dest = output.join("level.dat");
    fs::copy(&src, &dest).with_context(|| {
        format!(
            "Failed to copy level.dat template from {} to {}",
            src.display(),
            dest.display()
        )
    })?;
    customize_level_dat(&dest, spawn)
}

fn copy_datapacks(template_dir: &Path, output: &Path) -> Result<()> {
    let src = template_dir.join("datapacks");
    if !src.exists() {
        return Ok(());
    }
    let dest = output.join("datapacks");
    if dest.exists() {
        fs::remove_dir_all(&dest).with_context(|| {
            format!("Failed to remove existing datapacks at {}", dest.display())
        })?;
    }
    copy_dir_recursive(&src, &dest)
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)
        .with_context(|| format!("Failed to create directory {}", dest.display()))?;
    for entry in fs::read_dir(src)
        .with_context(|| format!("Failed to read directory {}", src.display()))?
    {
        let entry = entry?;
        let entry_path = entry.path();
        let target = dest.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&entry_path, &target)?;
        } else {
            fs::copy(&entry_path, &target).with_context(|| {
                format!(
                    "Failed to copy {} to {}",
                    entry_path.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

fn customize_level_dat(path: &Path, spawn: &SpawnSettings<'_>) -> Result<()> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open {} for reading", path.display()))?;
    let mut decoder = GzDecoder::new(file);
    let mut data = Vec::new();
    decoder
        .read_to_end(&mut data)
        .with_context(|| format!("Failed to decompress {}", path.display()))?;
    let mut level: LevelDat = fastnbt::from_bytes(&data).context("Failed to parse level.dat")?;

    level.data.level_name = spawn.level_name.to_string();
    level.data.set_spawn_position(spawn);

    let file = File::create(path)
        .with_context(|| format!("Failed to open {} for writing", path.display()))?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    fastnbt::to_writer(&mut encoder, &level).context("Failed to serialize level.dat")?;
    encoder
        .finish()
        .context("Failed to finalize compressed level.dat")?;
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct LevelDat {
    #[serde(rename = "Data")]
    data: LevelData,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct LevelData {
    #[serde(rename = "LevelName")]
    level_name: String,
    #[serde(rename = "spawn")]
    #[serde(default)]
    spawn: Option<SpawnData>,
    #[serde(flatten)]
    #[serde(default)]
    other: HashMap<String, Value>,
}

impl LevelData {
    fn set_spawn_position(&mut self, spawn: &SpawnSettings<'_>) {
        let new_pos = IntArray::new(vec![spawn.spawn_x, spawn.spawn_y, spawn.spawn_z]);
        let entry = self.spawn.get_or_insert_with(SpawnData::default);
        entry.pos = Some(new_pos);

        for (key, value) in [
            ("SpawnX", spawn.spawn_x),
            ("SpawnY", spawn.spawn_y),
            ("SpawnZ", spawn.spawn_z),
        ] {
            if let Some(existing) = self.other.get_mut(key) {
                *existing = Value::Int(value);
            }
        }
    }
}

#[derive(Deserialize, Serialize, Default)]
struct SpawnData {
    #[serde(rename = "pos")]
    #[serde(default)]
    pos: Option<IntArray>,
    #[serde(flatten)]
    #[serde(default)]
    other: HashMap<String, Value>,
}
