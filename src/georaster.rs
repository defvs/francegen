use std::fs::File;
use std::io::{Read, Seek};

use anyhow::{Context, Result, anyhow, bail};
use geo_types::Coord;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RasterType {
    PixelIsArea,
    PixelIsPoint,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterExtent {
    pub min_x: f64,
    pub max_x: f64,
    pub min_z: f64,
    pub max_z: f64,
}

pub struct GeoRaster {
    width: usize,
    height: usize,
    values: Vec<f64>,
    transform: Transform,
    nodata: Option<f64>,
    raster_offset: f64,
}

impl GeoRaster {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("Failed to open GeoTIFF {}", path.display()))?;
        Self::from_reader(file)
            .with_context(|| format!("Failed to decode GeoTIFF {}", path.display()))
    }

    fn from_reader<R: Read + Seek>(reader: R) -> Result<Self> {
        let mut decoder = Decoder::new(reader)?;
        let (width, height) = decoder.dimensions()?;
        let samples_per_pixel = decoder
            .find_tag_unsigned(Tag::SamplesPerPixel)?
            .unwrap_or(1) as usize;
        if samples_per_pixel == 0 {
            bail!("Samples per pixel tag was zero");
        }

        let raster_type = read_raster_type(&mut decoder)?;
        let transform = Transform::from_decoder(&mut decoder)?;
        let nodata = read_nodata(&mut decoder)?;

        let data = decoder.read_image()?;
        let values = convert_to_f64(data, samples_per_pixel)?;
        let pixel_count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| anyhow!("Raster dimensions are too large"))?;
        if values.len() != pixel_count {
            bail!(
                "Raster data length {} does not match dimensions {}x{}",
                values.len(),
                width,
                height
            );
        }

        Ok(Self {
            width: width as usize,
            height: height as usize,
            values,
            transform,
            nodata,
            raster_offset: match raster_type {
                Some(RasterType::PixelIsPoint) => -0.5,
                _ => 0.0,
            },
        })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn origin(&self) -> Coord {
        self.coord_for(0, 0)
    }

    pub fn coord_for(&self, x: usize, y: usize) -> Coord {
        let raster = Coord {
            x: x as f64 + self.raster_offset,
            y: y as f64 + self.raster_offset,
        };
        self.transform.to_model(&raster)
    }

    pub fn sample(&self, x: usize, y: usize) -> Option<f64> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = y * self.width + x;
        let value = self.values[index];
        if let Some(nodata) = self.nodata {
            if approx_equals(value, nodata) {
                return None;
            }
        }
        if value.is_nan() { None } else { Some(value) }
    }

    pub fn extent(&self) -> RasterExtent {
        let max_col = self.width.saturating_sub(1);
        let max_row = self.height.saturating_sub(1);
        let corners = [
            self.coord_for(0, 0),
            self.coord_for(max_col, 0),
            self.coord_for(0, max_row),
            self.coord_for(max_col, max_row),
        ];
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_z = f64::INFINITY;
        let mut max_z = f64::NEG_INFINITY;
        for coord in &corners {
            min_x = min_x.min(coord.x);
            max_x = max_x.max(coord.x);
            min_z = min_z.min(coord.y);
            max_z = max_z.max(coord.y);
        }
        RasterExtent {
            min_x,
            max_x,
            min_z,
            max_z,
        }
    }
}

#[derive(Clone, Copy)]
struct Transform {
    raster_point: Coord,
    model_point: Coord,
    pixel_scale: Coord,
}

impl Transform {
    fn from_decoder<R: Read + Seek>(decoder: &mut Decoder<R>) -> Result<Self> {
        let tie_points = decoder
            .find_tag(Tag::ModelTiepointTag)?
            .ok_or_else(|| anyhow!("GeoTIFF is missing ModelTiepointTag"))?
            .into_f64_vec()?;
        if tie_points.len() < 6 {
            bail!(
                "ModelTiepointTag must contain at least 6 values, found {}",
                tie_points.len()
            );
        }
        let pixel_scale = decoder
            .find_tag(Tag::ModelPixelScaleTag)?
            .ok_or_else(|| anyhow!("GeoTIFF is missing ModelPixelScaleTag"))?
            .into_f64_vec()?;
        if pixel_scale.len() < 2 {
            bail!(
                "ModelPixelScaleTag must contain at least 2 values, found {}",
                pixel_scale.len()
            );
        }

        Ok(Self {
            raster_point: Coord {
                x: tie_points[0],
                y: tie_points[1],
            },
            model_point: Coord {
                x: tie_points[3],
                y: tie_points[4],
            },
            pixel_scale: Coord {
                x: pixel_scale[0],
                y: pixel_scale[1],
            },
        })
    }

    fn to_model(&self, coord: &Coord) -> Coord {
        Coord {
            x: (coord.x - self.raster_point.x) * self.pixel_scale.x + self.model_point.x,
            y: (coord.y - self.raster_point.y) * -self.pixel_scale.y + self.model_point.y,
        }
    }
}

fn read_raster_type<R: Read + Seek>(decoder: &mut Decoder<R>) -> Result<Option<RasterType>> {
    let Some(raw) = decoder.find_tag(Tag::GeoKeyDirectoryTag)? else {
        return Ok(None);
    };
    let dir = raw.into_u16_vec()?;
    if dir.len() < 4 {
        return Ok(None);
    }
    let declared = dir[3] as usize;
    let available = (dir.len().saturating_sub(4)) / 4;
    let keys = declared.min(available);
    for i in 0..keys {
        let base = 4 + i * 4;
        if base + 3 >= dir.len() {
            break;
        }
        let key_id = dir[base];
        let tiff_location = dir[base + 1];
        let value_offset = dir[base + 3];
        if key_id == 1025 && tiff_location == 0 {
            return Ok(match value_offset {
                1 => Some(RasterType::PixelIsPoint),
                _ => Some(RasterType::PixelIsArea),
            });
        }
    }
    Ok(None)
}

fn read_nodata<R: Read + Seek>(decoder: &mut Decoder<R>) -> Result<Option<f64>> {
    let Some(raw) = decoder.find_tag(Tag::GdalNodata)? else {
        return Ok(None);
    };
    let text = raw.into_string()?;
    let trimmed = text.trim_matches(char::from(0)).trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.eq_ignore_ascii_case("nan") {
        return Ok(Some(f64::NAN));
    }
    Ok(trimmed.parse().ok())
}

fn convert_to_f64(data: DecodingResult, samples: usize) -> Result<Vec<f64>> {
    match data {
        DecodingResult::U8(buf) => map_samples(buf, samples, |v| v as f64),
        DecodingResult::U16(buf) => map_samples(buf, samples, |v| v as f64),
        DecodingResult::U32(buf) => map_samples(buf, samples, |v| v as f64),
        DecodingResult::U64(buf) => map_samples(buf, samples, |v| v as f64),
        DecodingResult::I8(buf) => map_samples(buf, samples, |v| v as f64),
        DecodingResult::I16(buf) => map_samples(buf, samples, |v| v as f64),
        DecodingResult::I32(buf) => map_samples(buf, samples, |v| v as f64),
        DecodingResult::I64(buf) => map_samples(buf, samples, |v| v as f64),
        DecodingResult::F32(buf) => map_samples(buf, samples, |v| v as f64),
        DecodingResult::F64(buf) => map_samples(buf, samples, |v| v as f64),
    }
}

fn map_samples<T: Copy, F: Fn(T) -> f64>(data: Vec<T>, samples: usize, map: F) -> Result<Vec<f64>> {
    if samples == 0 {
        bail!("Samples per pixel cannot be zero");
    }
    if data.len() % samples != 0 {
        bail!(
            "Raster sample count {} is not divisible by samples per pixel {}",
            data.len(),
            samples
        );
    }
    let mut out = Vec::with_capacity(data.len() / samples);
    for chunk in data.chunks(samples) {
        out.push(map(chunk[0]));
    }
    Ok(out)
}

fn approx_equals(a: f64, b: f64) -> bool {
    if a == b {
        true
    } else {
        let diff = (a - b).abs();
        let scale = a.abs().max(b.abs()).max(1.0);
        diff <= scale * 1e-9
    }
}
