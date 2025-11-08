# francegen

Generate Minecraft Java Edition worlds from GeoTIFF heightmaps – one block per metre – and keep origin metadata so you can line real‑world coordinates back up later.

## Requirements

- Rust toolchain (for `cargo run` / `cargo build`)
- Input GeoTIFFs that share the same projected coordinate system (e.g., LAMB93)

## Commands

### Generate terrain

```
francegen [--threads <N>] [--meta-only] [--bounds <min_x,min_z,max_x,max_z>] <tif-folder> <output-world>
```

| Flag | Description |
|------|-------------|
| `--threads <N>` | Override Rayon’s worker count. Defaults to the number of logical CPUs. |
| `--meta-only` | Read the tiles and emit only metadata (no region files). Useful to grab the origin before committing to a full build. |
| `--config <file>` | Load a JSON terrain configuration file to control block layers and the base biome. |
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

You can also `cargo run -- --threads 8 ...` while developing locally.

### Terrain configuration file

Pass `--config <file>` to override the default surface blocks and biome. The file is JSON and supports these keys:

```json
{
  "top_layer_block": "minecraft:grass_block",
  "top_layer_thickness": 1,
  "bottom_layer_block": "minecraft:stone",
  "base_biome": "minecraft:plains",
  "cliff_generation": {
    "enabled": true,
    "angle_threshold_degrees": 60.0,
    "block": "minecraft:stone",
    "smoothing_radius": 2,
    "smoothing_factor": 0.5
  },
  "biome_layers": [
    {
      "range": { "min": "0m", "max": "300m" },
      "biome": "minecraft:plains",
      "cliff_block": "minecraft:cobblestone",
      "cliff_smoothing_radius": 3,
      "cliff_smoothing_factor": 0.8
    },
    {
      "range": { "min": "300m", "max": "1200m" },
      "biome": "minecraft:forest"
    }
  ],
  "top_block_layers": [
    {
      "range": { "min": "0m", "max": "2500m" },
      "block": "minecraft:grass_block"
    },
    {
      "range": { "min": "2500m", "max": "4000m" },
      "block": "minecraft:stone"
    }
  ]
}
```

All fields are optional; missing values fall back to the defaults shown above. `top_layer_thickness` must be at least 1 and defines how many blocks (starting at the surface) use the selected top block. Everything below that (down to bedrock) uses `bottom_layer_block`.

`biome_layers` and `top_block_layers` let you vary the biome and surface block based on a column’s ground elevation. Each entry requires a `range` with an optional `min` and `max` bound, plus the `biome` or `block` to apply when the surface height falls inside that range. Bounds accept either metres (`"300m"`) or raw Minecraft block heights (`"1200b"`). When multiple layers overlap, the first one in the list wins. Columns that do not match any layer continue to use `base_biome` and `top_layer_block`.

Set `cliff_generation.enabled` to `true` to automatically replace the entire top layer on steep slopes. The generator scans neighbours within `smoothing_radius` metres (minimum 1), computes their slope angles, and blends the raw maximum with a weighted average using `smoothing_factor` (0 = original behaviour, 1 = fully smoothed). If the blended angle exceeds `angle_threshold_degrees` the whole top-layer thickness swaps to `cliff_generation.block`. Individual `biome_layers` entries can override any of these values via `cliff_angle_threshold_degrees`, `cliff_block`, `cliff_smoothing_radius`, and `cliff_smoothing_factor`, letting you keep snowy cliffs icy while lower elevations stay rocky.

See [`examples/french_alps.json`](examples/french_alps.json) for a full configuration inspired by alpine terrain.
