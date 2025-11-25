#!/usr/bin/env python3
"""
Download 5 km x 5 km (25 km²) macro-tiles from the IGN WMS and invoke francegen
sequentially for each macro-tile, merging into a single world directory.

The WMS request mirrors utils/wms_dl.py (same base URL, layer, pixel size, and
tile dimensions). Each macro-tile is a 5x5 grid of 1 km tiles.
"""
import argparse
import datetime
import itertools
import os
import shlex
import subprocess
import sys
import time
from pathlib import Path

import requests
from tqdm import tqdm

BASE_URL = "https://data.geopf.fr/wms-r"
LAYER = "IGNF_LIDAR-HD_MNT_ELEVATION.ELEVATIONGRIDCOVERAGE.LAMB93"
PIXEL_SIZE = 0.5  # meters per pixel
TILE_WIDTH_PX = 2000
TILE_HEIGHT_PX = 2000
TILE_SIZE_M = TILE_WIDTH_PX * PIXEL_SIZE  # 1000m tiles

MACRO_TILE_SIDE_M = 5_000  # 5 km -> 25 km²
MACRO_TILE_GRID = 5  # 5 x 5 tiles per macro-tile
REQUEST_DELAY_S = 0.1  # polite delay between WMS calls
DONE_MARKER = ".francegen_done"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Download 25 km² macro-tiles and run francegen sequentially."
    )
    parser.add_argument(
        "--tiles-root",
        required=True,
        help="Directory to store downloaded GeoTIFFs (macro-tile subfolders will be created).",
    )
    parser.add_argument(
        "--world",
        required=True,
        help="World output directory to pass as <output-world> to francegen.",
    )
    parser.add_argument(
        "--center-x",
        type=float,
        required=True,
        help="Center X coordinate (EPSG:2154 / LAMB93).",
    )
    parser.add_argument(
        "--center-y",
        type=float,
        required=True,
        help="Center Y coordinate (EPSG:2154 / LAMB93).",
    )
    parser.add_argument(
        "--macro-radius",
        type=int,
        default=0,
        help=(
            "Number of 25 km² macro-tiles to include outward from the center in each axis. "
            "0 = just the center tile, 1 = 3x3 grid, etc."
        ),
    )
    parser.add_argument(
        "--francegen-bin",
        default="francegen",
        help="Path to the francegen binary (default: francegen on PATH).",
    )
    parser.add_argument(
        "--francegen-args",
        default="",
        help="Additional arguments to pass to francegen (e.g. '--config cfg.json').",
    )
    parser.add_argument(
        "--skip-existing",
        action="store_true",
        help="Skip downloading tiles that already exist on disk.",
    )
    parser.add_argument(
        "--resume",
        action="store_true",
        help=(
            "Resume from the last completed macro-tile (based on marker files in the tiles root). "
            "Useful after an interrupted run."
        ),
    )
    return parser.parse_args()


def macro_tile_centers(center_x: float, center_y: float, radius: int):
    """Yield (macro_x_idx, macro_y_idx, center_x, center_y) for each macro-tile."""
    for dy in range(-radius, radius + 1):
        for dx in range(-radius, radius + 1):
            yield (
                dx,
                dy,
                center_x + dx * MACRO_TILE_SIDE_M,
                center_y + dy * MACRO_TILE_SIDE_M,
            )


def download_macro_tile(dest_dir: Path, center_x: float, center_y: float, skip_existing: bool):
    dest_dir.mkdir(parents=True, exist_ok=True)
    start_x = center_x - (MACRO_TILE_SIDE_M / 2)
    start_y = center_y - (MACRO_TILE_SIDE_M / 2)

    tile_indices = list(itertools.product(range(MACRO_TILE_GRID), range(MACRO_TILE_GRID)))
    for col, row in tqdm(tile_indices, unit="tile", desc=f"Downloading {dest_dir.name}"):
        min_x = start_x + (col * TILE_SIZE_M)
        min_y = start_y + (row * TILE_SIZE_M)
        max_x = min_x + TILE_SIZE_M
        max_y = min_y + TILE_SIZE_M

        bbox_str = f"{min_x},{min_y},{max_x},{max_y}"
        filename = dest_dir / f"elevation_{col}_{row}.tif"

        if skip_existing and filename.exists():
            tqdm.write(f"[Skip] {filename} already exists")
            continue

        params = {
            "SERVICE": "WMS",
            "VERSION": "1.3.0",
            "REQUEST": "GetMap",
            "LAYERS": LAYER,
            "STYLES": "",
            "CRS": "EPSG:2154",
            "BBOX": bbox_str,
            "WIDTH": str(TILE_WIDTH_PX),
            "HEIGHT": str(TILE_HEIGHT_PX),
            "FORMAT": "image/geotiff",
            "EXCEPTIONS": "text/xml",
        }

        try:
            response = requests.get(BASE_URL, params=params, stream=True, timeout=60)
            if response.status_code == 200 and "image" in response.headers.get("content-type", "").lower():
                with open(filename, "wb") as f:
                    for chunk in response.iter_content(1024):
                        f.write(chunk)
            else:
                tqdm.write(f"[Error] {filename.name} -> status {response.status_code} / content-type {response.headers.get('content-type')}")
        except Exception as exc:  # pylint: disable=broad-except
            tqdm.write(f"[Exception] {filename.name}: {exc}")

        time.sleep(REQUEST_DELAY_S)


def run_francegen(bin_path: str, extra_args: str, tif_dir: Path, world_dir: Path):
    cmd = [bin_path]
    if extra_args.strip():
        cmd.extend(shlex.split(extra_args))
    cmd.extend([str(tif_dir), str(world_dir)])
    print(f"Running francegen: {' '.join(cmd)}")
    subprocess.run(cmd, check=True)


def completion_marker(macro_dir: Path) -> Path:
    return macro_dir / DONE_MARKER


def mark_completed(macro_dir: Path, cmd: list[str]):
    marker = completion_marker(macro_dir)
    payload = {
        "completed_at": datetime.datetime.utcnow().isoformat() + "Z",
        "command": cmd,
    }
    marker.write_text(repr(payload), encoding="utf-8")


def find_resume_index(macro_tiles, tiles_root: Path) -> int:
    """Return index of the first macro-tile without a completion marker."""
    for idx, (mx, my, _, _) in enumerate(macro_tiles):
        macro_dir = tiles_root / f"macro_x{mx:+d}_y{my:+d}"
        if not completion_marker(macro_dir).exists():
            return idx
    return len(macro_tiles)


def main():
    args = parse_args()
    tiles_root = Path(args.tiles_root)
    world_dir = Path(args.world)

    if not tiles_root.exists():
        tiles_root.mkdir(parents=True, exist_ok=True)

    macro_tiles = list(macro_tile_centers(args.center_x, args.center_y, args.macro_radius))
    print(f"Preparing {len(macro_tiles)} macro-tile(s) of 25 km² each")

    start_idx = find_resume_index(macro_tiles, tiles_root) if args.resume else 0
    if start_idx >= len(macro_tiles):
        print("All macro-tiles already completed; nothing to do.")
        return

    if args.resume and start_idx > 0:
        print(f"Resuming after index {start_idx - 1}; skipping {start_idx} completed macro-tile(s)")

    for loop_idx, (mx, my, cx, cy) in enumerate(macro_tiles[start_idx:], start=start_idx + 1):
        macro_dir = tiles_root / f"macro_x{mx:+d}_y{my:+d}"
        print(f"[{loop_idx}/{len(macro_tiles)}] Macro tile offset ({mx}, {my}) at center ({cx:.2f}, {cy:.2f})")
        download_macro_tile(macro_dir, cx, cy, args.skip_existing)
        cmd = [args.francegen_bin]
        if args.francegen_args.strip():
            cmd.extend(shlex.split(args.francegen_args))
        cmd.extend([str(macro_dir), str(world_dir)])
        run_francegen(args.francegen_bin, args.francegen_args, macro_dir, world_dir)
        mark_completed(macro_dir, cmd)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as exc:
        print(f"francegen failed with exit code {exc.returncode}", file=sys.stderr)
        sys.exit(exc.returncode)
