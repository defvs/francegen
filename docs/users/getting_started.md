# User Getting Started

This guide walks through installing francegen, running the CLI, and understanding the metadata it produces.

## Requirements

- Rust toolchain (for `cargo run`, `cargo build`, or `cargo install`)
- Input GeoTIFFs that share the same projected coordinate system (e.g., LAMB93)
- Optional: LiDAR COPC/LAZ tiles for higher-fidelity buildings. IGN publishes them at https://cartes.gouv.fr/telechargement/IGNF_NUAGES-DE-POINTS-LIDAR-HD (same CRS as their DEMs).

## Core commands

### Generate terrain

```
francegen [--threads <N>] [--meta-only] [--bounds <min_x,min_z,max_x,max_z>] [--config <file>] [--cache-dir <path>] <tif-folder> <output-world>
```

| Flag | Description |
|------|-------------|
| `--threads <N>` | Override Rayon’s worker count. Defaults to the number of logical CPUs. |
| `--meta-only` | Read the tiles and emit only metadata (no region files). Useful to grab the origin before committing to a full build. |
| `--config <file>` | Load a JSON terrain configuration file to control block layers and the base biome. |
| `--cache-dir <path>` | Directory for cached remote downloads (Overpass responses and WMTS tiles). Defaults to a temporary folder that is deleted after the run. |
| `--bounds <min_x,min_z,max_x,max_z>` | Clip generation to a rectangle in real/model coordinates (metres in the GeoTIFF CRS, matching `francegen locate`). |

During ingestion the tool prints world-size estimates, DEM min/max, and the origin in model space. When generation finishes, it writes the usual `region/` directory plus a `francegen_meta.json` file inside `<output-world>` containing the GeoTIFF origin and bounds.

### Locate a coordinate

```
francegen locate <world-dir> <real-x> <real-z> [<real-height>]
```

Reads `francegen_meta.json`, subtracts the recorded origin, and prints the Minecraft block/chunk coordinates. Provide a third number (height in metres) to also get the Minecraft Y value (`DEM + (-2048)`; clamped to [-2048, 2031]).

### Inspect GeoTIFF bounds

```
francegen bounds <tif-folder>
```

Scans the folder for `.tif`/`.tiff` files, prints their combined real-world bounding rectangle, and echoes a ready-to-copy `--bounds` flag.

## Metadata format

`francegen_meta.json` stores:

```json
{
  "origin_model_x": 871234.0,
  "origin_model_z": 6423456.0,
  "min_x": 0,
  "max_x": 24000,
  "min_z": -1000,
  "max_z": 7000,
  "min_height": 756.8,
  "max_height": 3981.97
}
```

`origin_model_*` is the real-world coordinate that became Minecraft `(0,0)`. X increases east, while **Z increases south (north is `-Z`)** just like standard Minecraft coordinates. The generator flips the GeoTIFF Y axis internally so geographic north (increasing model-space Y) aligns with negative Minecraft Z.

## Examples

```bash
# Build a world using 8 threads
francegen --threads 8 data/tiffs ./worlds/alps

# Only capture metadata to inspect alignment
francegen --meta-only data/tiffs ./worlds/alps-metadata

# Inspect tile bounds, then build a clipped world inside that rectangle
francegen bounds data/tiffs
francegen --bounds 873000.0,6427000.0,874000.0,6428000.0 data/tiffs ./worlds/alps-clipped

# Convert a surveyed coordinate into Minecraft space
francegen locate ./worlds/alps 873210.4 6428123.6 1523.0
```

You can also run `cargo run -- --threads 8 ...` while developing locally.
