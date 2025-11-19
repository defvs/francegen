# User Configuration Reference

`francegen` accepts an optional JSON configuration file (`--config <file>`) that controls surface blocks, biome selection, overlays, and caching.

## Terrain configuration file

```json
{
  "top_layer_block": "minecraft:grass_block",
  "top_layer_thickness": 1,
  "bottom_layer_block": "minecraft:stone",
  "base_biome": "minecraft:plains",
  "generate_features": true,
  "empty_chunk_radius": 32,
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

`empty_chunk_radius` (default `32`) pads the exported world with that many pre-generated, all-air chunks on each side of the generated area so Minecraft doesn’t suddenly fall back to its vanilla terrain at the border.

`biome_layers` and `top_block_layers` let you vary the biome and surface block based on a column’s ground elevation. Each entry requires a `range` with an optional `min` and `max` bound, plus the `biome` or `block` to apply when the surface height falls inside that range. Bounds accept either metres (`"300m"`) or raw Minecraft block heights (`"1200b"`). When multiple layers overlap, the first one in the list wins. Columns that do not match any layer continue to use `base_biome` and `top_layer_block`.

Set `cliff_generation.enabled` to `true` to automatically replace the entire top layer on steep slopes. The generator scans neighbours within `smoothing_radius` metres (minimum 1), computes their slope angles, and blends the raw maximum with a weighted average using `smoothing_factor` (0 = original behaviour, 1 = fully smoothed). If the blended angle exceeds `angle_threshold_degrees` the whole top-layer thickness swaps to `cliff_generation.block`. Individual `biome_layers` entries can override any of these values via `cliff_angle_threshold_degrees`, `cliff_block`, `cliff_smoothing_radius`, and `cliff_smoothing_factor`, letting you keep snowy cliffs icy while lower elevations stay rocky.

See [`examples/french_alps.json`](../../examples/french_alps.json) for a full configuration inspired by alpine terrain.

## OpenStreetMap overlays

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
    - `width_m` accepts either a fixed number (legacy behavior) or an object with `default`, optional `min`/`max`, and a `sources` array such as `[{ "key": "width" }, { "key": "lanes", "multiplier": 3.5 }]`. When present, the first matching tag value (converted to meters) overrides the default so highways, rivers, or ski pistes can inherit their real-world widths straight from OSM.
  - `query`: raw OverpassQL inserted between the global `[out:json]…;…;out geom;` wrapper. Use `{{bbox}}` as a placeholder for the lat/lon bounding box.
  - `style`: at least one of `surface_block`, `subsurface_block`, `top_thickness`, `biome`, or an `extrusion` block. `surface_block` replaces the exposed material, `subsurface_block` controls what fills beneath it, `top_thickness` overrides how many blocks receive the surface material, and `biome` swaps the biome palette. `extrusion` lets polygon layers (like buildings) specify a vertical height via the same dynamic source syntax as `width_m`, plus an optional `block` override; francegen extrudes that many blocks above the surface using either the extrusion block or the surface block.
  - `layer_index` (optional): controls when the layer is applied relative to other OSM/WMTS overlays. Higher values paint earlier, while lower values paint later (and therefore win). When omitted the layer behaves as if `layer_index = 0`, and ties fall back to the order in the JSON array.

```json
{
  "width_m": {
    "default": 5.0,
    "min": 3.0,
    "max": 18.0,
    "sources": [
      { "key": "width" },
      { "key": "lanes", "multiplier": 3.5 }
    ]
  },
  "style": {
    "surface_block": "minecraft:spruce_planks",
    "extrusion": {
      "height_m": {
        "default": 8.0,
        "sources": [
          { "key": "height" },
          { "key": "building:levels", "multiplier": 3.0 }
        ]
      }
    }
  }
}
```

The generator sends HTTPS requests to the configured Overpass endpoint whenever an `osm` block is present, so make sure outbound network access is available or point `overpass_url` to a local mirror. Pass `--cache-dir <path>` to persist the JSON responses inside `<path>/overpass/` and reuse them between runs instead of re-querying the API.

## Layer ordering

Painting happens in three passes: `biome_layers`, `top_block_layers`, and finally all OSM/WMTS overlays. During the overlay pass both the OSM layers and WMTS color rules are gathered into a single list and sorted by `layer_index` (highest first). When two entries share the same index—or leave it unset—they keep their JSON order, with every OSM layer evaluated before the WMTS color rules. Because the lowest index is applied last, give the overlays you want on top the smallest numbers (even negatives) and leave background fillers at larger values.

## WMTS overlays

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
