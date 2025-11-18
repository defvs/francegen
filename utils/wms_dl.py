import os
import requests
import time
import itertools
import argparse
import sys
from tqdm import tqdm  # Progress bar library

# --- Argument Parsing ---
parser = argparse.ArgumentParser(description="Download WMS Tiles for a 10km x 10km grid.")
parser.add_argument("output_dir", help="Directory to save the downloaded tiles")
args = parser.parse_args()

OUTPUT_DIR = args.output_dir

# --- Configuration ---
BASE_URL = "https://data.geopf.fr/wms-r"
LAYER = "IGNF_LIDAR-HD_MNT_ELEVATION.ELEVATIONGRIDCOVERAGE.LAMB93"
PIXEL_SIZE = 0.5  # meters per pixel
TILE_WIDTH_PX = 2000
TILE_HEIGHT_PX = 2000
GRID_SIDE_LENGTH = 10  # 10x10 grid = 100 tiles

# --- Coordinate Calculation ---

# 1. The specific center point you requested
center_x = 697312.50
center_y = 6518866.50

# 2. Calculate Real World Tile Size (1000m x 1000m)
TILE_SIZE_M = TILE_WIDTH_PX * PIXEL_SIZE 

# 3. Calculate Start Point (Bottom-Left of the 10km square)
total_width_m = GRID_SIDE_LENGTH * TILE_SIZE_M
start_x = center_x - (total_width_m / 2)
start_y = center_y - (total_width_m / 2)

# --- Execution ---
if not os.path.exists(OUTPUT_DIR):
    try:
        os.makedirs(OUTPUT_DIR)
    except OSError as e:
        print(f"Error creating directory {OUTPUT_DIR}: {e}")
        sys.exit(1)

print(f"--- WMS Downloader ---")
print(f"Target Directory: {OUTPUT_DIR}")
print(f"Center Point:     {center_x}, {center_y}")
print(f"Area Coverage:    {GRID_SIDE_LENGTH}km x {GRID_SIDE_LENGTH}km")
print("-" * 30)

# Create a list of all coordinate pairs (0,0) to (9,9)
tile_indices = list(itertools.product(range(GRID_SIDE_LENGTH), range(GRID_SIDE_LENGTH)))

# Iterate with Progress Bar
for col, row in tqdm(tile_indices, unit="tile", desc="Downloading"):
    
    # Calculate BBOX
    min_x = start_x + (col * TILE_SIZE_M)
    min_y = start_y + (row * TILE_SIZE_M)
    max_x = min_x + TILE_SIZE_M
    max_y = min_y + TILE_SIZE_M
    
    bbox_str = f"{min_x},{min_y},{max_x},{max_y}"
    filename = os.path.join(OUTPUT_DIR, f"elevation_{col}_{row}.tif")
    
    params = {
        "SERVICE": "WMS", "VERSION": "1.3.0", "REQUEST": "GetMap",
        "LAYERS": LAYER, "STYLES": "", "CRS": "EPSG:2154",
        "BBOX": bbox_str, "WIDTH": str(TILE_WIDTH_PX), "HEIGHT": str(TILE_HEIGHT_PX),
        "FORMAT": "image/geotiff", "EXCEPTIONS": "text/xml"
    }
    
    try:
        response = requests.get(BASE_URL, params=params, stream=True)
        
        if response.status_code == 200:
            # Check if response is an image (ignoring charset)
            content_type = response.headers.get('content-type', '').lower()
            if 'image' in content_type:
                with open(filename, 'wb') as f:
                    for chunk in response.iter_content(1024):
                        f.write(chunk)
            else:
                # Use tqdm.write to print without breaking the progress bar
                tqdm.write(f"[Error] XML returned for {filename}: {response.text[:100]}...")
        else:
            tqdm.write(f"[Error] Status {response.status_code} for {filename}")
            
    except Exception as e:
        tqdm.write(f"[Exception] {e}")

    # Polite delay
    time.sleep(0.1)

print("\nAll downloads complete.")
