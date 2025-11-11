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
| `--cache-dir <path>` | Directory for cached remote downloads (WMTS tiles today, future features later). Defaults to a temporary folder that is deleted after the run. |
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
  "generate_features": true,
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

`generate_features` toggles whether vanilla Minecraft should keep running the late-worldgen feature pass (trees, ores, etc.) when it loads your chunks. Leave it `false` (the default) to save chunks with `minecraft:full` status, or set it to `true` to emit proto-chunks marked as `minecraft:liquid_carvers` so the game populates them later.

`biome_layers` and `top_block_layers` let you vary the biome and surface block based on a column’s ground elevation. Each entry requires a `range` with an optional `min` and `max` bound, plus the `biome` or `block` to apply when the surface height falls inside that range. Bounds accept either metres (`"300m"`) or raw Minecraft block heights (`"1200b"`). When multiple layers overlap, the first one in the list wins. Columns that do not match any layer continue to use `base_biome` and `top_layer_block`.

Set `cliff_generation.enabled` to `true` to automatically replace the entire top layer on steep slopes. The generator scans neighbours within `smoothing_radius` metres (minimum 1), computes their slope angles, and blends the raw maximum with a weighted average using `smoothing_factor` (0 = original behaviour, 1 = fully smoothed). If the blended angle exceeds `angle_threshold_degrees` the whole top-layer thickness swaps to `cliff_generation.block`. Individual `biome_layers` entries can override any of these values via `cliff_angle_threshold_degrees`, `cliff_block`, `cliff_smoothing_radius`, and `cliff_smoothing_factor`, letting you keep snowy cliffs icy while lower elevations stay rocky.

See [`examples/french_alps.json`](examples/french_alps.json) for a full configuration inspired by alpine terrain.

### OpenStreetMap overlays

Add an `osm` block to the JSON config to paint additional materials and biomes using live OSM data fetched via the Overpass API. `francegen` automatically requests the data using the DEM bounds (plus an optional margin) and rasterizes the features onto the existing heightmap before writing region files.

```
"osm": {
  "enabled": true,
  "overpass_url": "https://overpass-api.de/api/interpreter",
  "bbox_margin_m": 400.0,
  "layers": [
    {
      "name": "paved_roads",
      "geometry": "line",
      "width_m": 5.0,
      "query": "(way[\"highway\"~\"^(primary|secondary|tertiary|residential)$\"]({{bbox}}););",
      "style": {
        "surface_block": "minecraft:stone_bricks",
        "top_thickness": 1
      }
    },
    {
      "name": "forest_polygons",
      "geometry": "polygon",
      "query": "(way[\"landuse\"=\"forest\"]({{bbox}});relation[\"landuse\"=\"forest\"]({{bbox}}););",
      "style": {
        "biome": "minecraft:forest",
        "surface_block": "minecraft:grass_block"
      }
    }
  ]
}
```

Key fields:

- `enabled` toggles the overlay stage without editing layers.
- `bbox_margin_m` expands the DEM-derived bounding box before querying Overpass. Increase this when you want roads that slightly overshoot the clipped DEM area.
- `layers` describes how to query and render each feature class. Layers later in the list override earlier ones when they overlap.
  - `geometry`: `"line"` (buffered with `width_m`, measured in meters/blocks) or `"polygon"` (filled area).
  - `query`: raw OverpassQL inserted between the global `[out:json]…;…;out geom;` wrapper. Use `{{bbox}}` as a placeholder for the lat/lon bounding box.
  - `style`: at least one of `surface_block`, `subsurface_block`, `top_thickness`, or `biome`. `surface_block` replaces the exposed material, `subsurface_block` controls what fills beneath it, `top_thickness` overrides how many blocks receive the surface material, and `biome` swaps the biome palette.
  - `layer_index` (optional): controls when the layer is applied relative to other OSM/WMTS overlays. Higher values paint earlier, while lower values paint later (and therefore win). When omitted the layer behaves as if `layer_index = 0`, and ties fall back to the order in the JSON array.

The generator sends HTTPS requests to the configured Overpass endpoint whenever an `osm` block is present, so make sure outbound network access is available or point `overpass_url` to a local mirror.

### Layer ordering

Painting happens in three passes: `biome_layers`, `top_block_layers`, and finally all OSM/WMTS overlays. The overlay pass sorts every OSM layer and WMTS color rule by `layer_index` (highest first). When two entries share the same index—or leave it unset—they keep the order from the configuration array. Because the lowest index is applied last, give the overlays you want on top the smallest numbers (even negatives) and leave background fillers at larger values.

### WMTS overlays

You can also paint landuse from a tiled WMTS layer. Add a `wmts` block to the config to download raster tiles from the advertised GetCapabilities document, map specific colors to biomes/blocks, and stamp them directly onto the terrain columns:

```json
"wmts": {
  "enabled": true,
  "capabilities_url": "https://data.geopf.fr/wmts?REQUEST=GetCapabilities&service=WMTS",
  "layer": "IGNF_COSIA_2024",
  "style_id": "normal",
  "tile_matrix_set": "PM_6_18",
  "tile_matrix": 15,
  "format": "image/png",
  "bbox_margin_m": 200.0,
  "max_tiles": 4096,
  "colors": [
    {
      "color": "#2b8cbe",
      "tolerance": 12,
      "layer_index": 20,
      "style": { "surface_block": "minecraft:water", "subsurface_block": "minecraft:clay" }
    },
    {
      "color": "#5ca636",
      "tolerance": 18,
      "layer_index": 2,
      "style": { "biome": "minecraft:forest", "surface_block": "minecraft:grass_block" }
    }
  ]
}
```

`capabilities_url`, `layer`, `tile_matrix_set`, and `tile_matrix` identify which WMTS layer/zoom to sample. Each `colors` entry describes a target pixel (hex `#RRGGBB` or `#RRGGBBAA`) plus an optional per-channel `tolerance` and the overlay style to apply when that color is seen. `alpha_threshold` filters out transparent pixels, while `layer_index` mirrors the OSM behavior for ordering.

Tiles are prefetched before rasterization. By default they are downloaded once per run into a unique temporary directory that is deleted at the end; pass `--cache-dir <path>` to reuse or inspect the files between runs. Increase `max_tiles` if you deliberately need a wide bbox/zoom combination.
