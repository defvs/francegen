# francegen

Generate Minecraft Java Edition worlds from GeoTIFF heightmaps – one block per metre – and keep origin metadata so you can line real‑world coordinates back up later.

## Requirements

- Rust toolchain (for `cargo run` / `cargo build`)
- Input GeoTIFFs that share the same projected coordinate system (e.g., LAMB93)

## Commands

### Generate terrain

```
francegen [--threads <N>] [--meta-only] <tif-folder> <output-world>
```

| Flag | Description |
|------|-------------|
| `--threads <N>` | Override Rayon’s worker count. Defaults to the number of logical CPUs. |
| `--meta-only` | Read the tiles and emit only metadata (no region files). Useful to grab the origin before committing to a full build. |

During ingestion the tool prints world-size estimates, DEM min/max, and the origin in model space. When generation finishes, it writes the usual `region/` directory plus a `francegen_meta.json` file inside `<output-world>` containing the GeoTIFF origin and bounds.

### Locate a coordinate

```
francegen locate <world-dir> <real-x> <real-z> [<real-height>]
```

Reads `francegen_meta.json`, subtracts the recorded origin, and prints the Minecraft block/chunk coordinates. Provide a third number (height in metres) to also get the Minecraft Y value (`DEM + (-2048)`; clamped to [-2048, 2031]).

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

`origin_model_*` is the real-world coordinate that became Minecraft `(0,0)`. X increases east, Z increases south (north is `-Z`, matching Minecraft).

## Examples

```bash
# Build a world using 8 threads
francegen --threads 8 data/tiffs ./worlds/alps

# Only capture metadata to inspect alignment
francegen --meta-only data/tiffs ./worlds/alps-metadata

# Convert a surveyed coordinate into Minecraft space
francegen locate ./worlds/alps 873210.4 6428123.6 1523.0
```

You can also `cargo run -- --threads 8 ...` while developing locally.
