# francegen

Generate Minecraft Java Edition worlds from GeoTIFF heightmaps – one block per metre – and keep origin metadata so you can line real-world coordinates back up later.

## Quickstart (users)

1. Install the Rust toolchain and collect GeoTIFFs that share the same projected coordinate system (e.g., LAMB93).
2. Run `francegen [options] <tif-folder> <output-world>` to build terrain, or use `francegen locate <world> <real-x> <real-z>` to translate coordinates.
3. Explore `francegen_meta.json` inside the generated world for origin/bounds information.
4. Dive into [User Getting Started](docs/users/getting_started.md) and the [User Configuration Reference](docs/users/configuration.md) for detailed CLI flags, metadata, and overlay configuration tips.

## Quickstart (developers)

1. Install the latest stable Rust via `rustup`.
2. Use `cargo fmt --all`, and `cargo build` before sending changes.
3. Iterate with `cargo run -- --config examples/french_alps.json <tif-folder> <output-world>` to validate code against sample data.

## Documentation

All extended documentation now lives in [`docs/`](docs/README.md):

- [User Getting Started](docs/users/getting_started.md)
- [User Configuration Reference](docs/users/configuration.md)
