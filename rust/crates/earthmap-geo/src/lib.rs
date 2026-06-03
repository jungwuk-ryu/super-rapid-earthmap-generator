#![forbid(unsafe_code)]

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::{cell::RefCell, collections::BTreeMap};

pub const MODULE_STATUS: &str = "phase4-earth-scale-mapping-bootstrap";

pub const WGS84_EQUATORIAL_RADIUS_METERS: f64 = 6_378_137.0;

const TAG_IMAGE_WIDTH: u16 = 256;
const TAG_IMAGE_LENGTH: u16 = 257;
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_SAMPLES_PER_PIXEL: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_SAMPLE_FORMAT: u16 = 339;
const TAG_MODEL_PIXEL_SCALE: u16 = 33550;
const TAG_MODEL_TIEPOINT: u16 = 33922;
const TAG_GEO_KEY_DIRECTORY: u16 = 34735;
const TAG_GDAL_NODATA: u16 = 42113;

const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_DOUBLE: u16 = 12;
const TYPE_LONG8: u16 = 16;

pub type Result<T> = std::result::Result<T, GeoError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeoError {
    message: String,
}

impl GeoError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for GeoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for GeoError {}

#[derive(Clone, Debug, PartialEq)]
pub struct EarthScaleMapping {
    pub denominator: i32,
    pub min_latitude: f64,
    pub max_latitude: f64,
    pub width_blocks: i32,
    pub height_blocks: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GeoTiffMetadata {
    pub path: PathBuf,
    pub width: i32,
    pub height: i32,
    pub bits_per_sample: i32,
    pub sample_format: i32,
    pub compression: i32,
    pub rows_per_strip: i32,
    pub samples_per_pixel: i32,
    pub top_left_longitude: f64,
    pub top_left_latitude: f64,
    pub pixel_width_degrees: f64,
    pub pixel_height_degrees: f64,
    pub epsg_code: Option<i32>,
    pub no_data_value: Option<f64>,
}

impl GeoTiffMetadata {
    pub fn sample_type_name(&self) -> String {
        if self.bits_per_sample == 16 && self.sample_format == 2 {
            return "Int16".to_string();
        }
        if self.bits_per_sample == 32 && self.sample_format == 3 {
            return "Float32".to_string();
        }
        format!(
            "bits={}, sampleFormat={}",
            self.bits_per_sample, self.sample_format
        )
    }
}

#[derive(Debug)]
pub struct GeoTiffHeightmapReader {
    file: Mutex<File>,
    metadata: GeoTiffMetadata,
    strip_offsets: Vec<u64>,
    strip_byte_counts: Vec<usize>,
    row_byte_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GeoTiffRowCacheStats {
    pub max_rows: usize,
    pub resident_rows: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub prefetch_rows: usize,
    pub prefetch_requests: u64,
    pub prefetch_loads: u64,
}

#[derive(Debug)]
pub struct GeoTiffRowCache<'a> {
    reader: &'a GeoTiffHeightmapReader,
    max_rows: usize,
    prefetch_rows: usize,
    state: Mutex<RowCacheState>,
}

#[derive(Debug, Default)]
struct RowCacheState {
    rows: BTreeMap<i32, CachedRow>,
    access_clock: u64,
    read_ahead_target: i32,
    hits: u64,
    misses: u64,
    evictions: u64,
    prefetch_requests: u64,
    prefetch_loads: u64,
}

#[derive(Clone, Debug)]
struct CachedRow {
    values: Vec<i16>,
    access_stamp: u64,
}

impl GeoTiffHeightmapReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut file = File::open(path).map_err(|error| GeoError::invalid(error.to_string()))?;
        parse_bigtiff_heightmap(path, &mut file)
    }

    pub fn metadata(&self) -> &GeoTiffMetadata {
        &self.metadata
    }

    pub fn sample_at_pixel(&self, x: i32, y: i32) -> Result<i16> {
        self.require_pixel(x, y)?;
        let offset = self.strip_offsets[y as usize]
            .checked_add(u64::try_from(x).expect("validated x is non-negative") * 2)
            .ok_or_else(|| GeoError::invalid("sample offset overflow"))?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| GeoError::invalid("heightmap file lock poisoned"))?;
        let bytes = read_exact_at(&mut file, offset, 2)?;
        read_i16_le(&bytes, 0)
    }

    pub fn sample_at_longitude_latitude(&self, longitude: f64, latitude: f64) -> Result<i16> {
        let x = self.pixel_x(longitude)?;
        let y = self.pixel_y(latitude)?;
        self.sample_at_pixel(x, y)
    }

    pub fn read_row(&self, y: i32, destination: &mut [i16]) -> Result<()> {
        if y < 0 || y >= self.metadata.height {
            return Err(GeoError::invalid(format!("row outside raster: {y}")));
        }
        if destination.len() < self.metadata.width as usize {
            return Err(GeoError::invalid(
                "destination is smaller than raster width",
            ));
        }
        let byte_count = self.strip_byte_counts[y as usize];
        if byte_count < self.row_byte_count {
            return Err(GeoError::invalid(format!(
                "strip byte count is too small for row {y}: {byte_count}"
            )));
        }
        let mut file = self
            .file
            .lock()
            .map_err(|_| GeoError::invalid("heightmap file lock poisoned"))?;
        let row = read_exact_at(
            &mut file,
            self.strip_offsets[y as usize],
            self.row_byte_count,
        )?;
        for (index, sample) in destination
            .iter_mut()
            .take(self.metadata.width as usize)
            .enumerate()
        {
            *sample = i16::from_le_bytes([row[index * 2], row[(index * 2) + 1]]);
        }
        Ok(())
    }

    pub fn pixel_x(&self, longitude: f64) -> Result<i32> {
        let x = ((longitude - self.metadata.top_left_longitude) / self.metadata.pixel_width_degrees)
            .floor() as i32;
        if x < 0 || x >= self.metadata.width {
            return Err(GeoError::invalid(format!(
                "longitude outside raster: {longitude}"
            )));
        }
        Ok(x)
    }

    pub fn pixel_y(&self, latitude: f64) -> Result<i32> {
        let y = ((self.metadata.top_left_latitude - latitude) / self.metadata.pixel_height_degrees)
            .floor() as i32;
        if y < 0 || y >= self.metadata.height {
            return Err(GeoError::invalid(format!(
                "latitude outside raster: {latitude}"
            )));
        }
        Ok(y)
    }

    fn require_pixel(&self, x: i32, y: i32) -> Result<()> {
        if x < 0 || x >= self.metadata.width {
            return Err(GeoError::invalid(format!("x outside raster: {x}")));
        }
        if y < 0 || y >= self.metadata.height {
            return Err(GeoError::invalid(format!("y outside raster: {y}")));
        }
        Ok(())
    }
}

impl<'a> GeoTiffRowCache<'a> {
    pub fn new(reader: &'a GeoTiffHeightmapReader, max_rows: usize) -> Result<Self> {
        Self::with_prefetch(reader, max_rows, 0)
    }

    pub fn with_prefetch(
        reader: &'a GeoTiffHeightmapReader,
        max_rows: usize,
        prefetch_rows: usize,
    ) -> Result<Self> {
        if max_rows == 0 {
            return Err(GeoError::invalid("maxRows must be positive"));
        }
        Ok(Self {
            reader,
            max_rows,
            prefetch_rows,
            state: Mutex::new(RowCacheState {
                read_ahead_target: -1,
                ..RowCacheState::default()
            }),
        })
    }

    pub fn sample_at_pixel(&self, x: i32, y: i32) -> Result<i16> {
        let row = self.row(y)?;
        let x =
            usize::try_from(x).map_err(|_| GeoError::invalid(format!("x outside raster: {x}")))?;
        row.get(x)
            .copied()
            .ok_or_else(|| GeoError::invalid(format!("x outside raster: {x}")))
    }

    pub fn sample_at_longitude_latitude(&self, longitude: f64, latitude: f64) -> Result<i16> {
        self.sample_at_pixel(
            self.reader.pixel_x(longitude)?,
            self.reader.pixel_y(latitude)?,
        )
    }

    pub fn row(&self, y: i32) -> Result<Vec<i16>> {
        if y < 0 || y >= self.reader.metadata().height {
            return Err(GeoError::invalid(format!("row outside raster: {y}")));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| GeoError::invalid("row cache lock poisoned"))?;
        if state.rows.contains_key(&y) {
            state.hits += 1;
            let stamp = touch_state(&mut state);
            let row = state.rows.get_mut(&y).expect("row was just checked");
            row.access_stamp = stamp;
            let values = row.values.clone();
            self.schedule_read_ahead_locked(&mut state, y)?;
            return Ok(values);
        }

        state.misses += 1;
        let loaded = self.load_row(y)?;
        let stamp = touch_state(&mut state);
        state.rows.insert(
            y,
            CachedRow {
                values: loaded.clone(),
                access_stamp: stamp,
            },
        );
        trim_cache_locked(&mut state, self.max_rows);
        self.schedule_read_ahead_locked(&mut state, y)?;
        Ok(loaded)
    }

    pub fn stats(&self) -> GeoTiffRowCacheStats {
        let state = self.state.lock().expect("row cache lock not poisoned");
        GeoTiffRowCacheStats {
            max_rows: self.max_rows,
            resident_rows: state.rows.len(),
            hits: state.hits,
            misses: state.misses,
            evictions: state.evictions,
            prefetch_rows: self.prefetch_rows,
            prefetch_requests: state.prefetch_requests,
            prefetch_loads: state.prefetch_loads,
        }
    }

    fn load_row(&self, y: i32) -> Result<Vec<i16>> {
        let mut row = vec![0i16; self.reader.metadata().width as usize];
        self.reader.read_row(y, &mut row)?;
        Ok(row)
    }

    fn schedule_read_ahead_locked(&self, state: &mut RowCacheState, y: i32) -> Result<()> {
        if self.prefetch_rows == 0 {
            return Ok(());
        }
        let height = self.reader.metadata().height;
        if y < 0 || y >= height - 1 {
            return Ok(());
        }
        let prefetch_rows = i32::try_from(self.prefetch_rows).unwrap_or(i32::MAX);
        let target = y.saturating_add(prefetch_rows).min(height - 1);
        if target <= state.read_ahead_target {
            return Ok(());
        }
        let start = y
            .saturating_add(1)
            .max(state.read_ahead_target.saturating_add(1));
        state.read_ahead_target = target;
        for prefetch_y in start..=target {
            if state.rows.contains_key(&prefetch_y) {
                continue;
            }
            state.prefetch_requests += 1;
            let loaded = self.load_row(prefetch_y)?;
            state.prefetch_loads += 1;
            let stamp = touch_state(state);
            state.rows.insert(
                prefetch_y,
                CachedRow {
                    values: loaded,
                    access_stamp: stamp,
                },
            );
            trim_cache_locked(state, self.max_rows);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct HeightmapScalarSampler<'a> {
    reader: &'a GeoTiffHeightmapReader,
    row_cache: Option<&'a GeoTiffRowCache<'a>>,
    cached_rows: RefCell<CachedRows>,
    width: i32,
    height: i32,
    top_left_longitude: f64,
    top_left_latitude: f64,
    pixel_width_degrees: f64,
    pixel_height_degrees: f64,
}

#[derive(Debug, Default)]
struct CachedRows {
    first_y: Option<i32>,
    second_y: Option<i32>,
    first_row: Vec<i16>,
    second_row: Vec<i16>,
}

impl<'a> HeightmapScalarSampler<'a> {
    pub fn new(reader: &'a GeoTiffHeightmapReader) -> Self {
        Self::new_with_cache(reader, None)
    }

    pub fn with_row_cache(
        reader: &'a GeoTiffHeightmapReader,
        row_cache: &'a GeoTiffRowCache<'a>,
    ) -> Self {
        Self::new_with_cache(reader, Some(row_cache))
    }

    fn new_with_cache(
        reader: &'a GeoTiffHeightmapReader,
        row_cache: Option<&'a GeoTiffRowCache<'a>>,
    ) -> Self {
        let metadata = reader.metadata();
        Self {
            reader,
            row_cache,
            cached_rows: RefCell::new(CachedRows::default()),
            width: metadata.width,
            height: metadata.height,
            top_left_longitude: metadata.top_left_longitude,
            top_left_latitude: metadata.top_left_latitude,
            pixel_width_degrees: metadata.pixel_width_degrees,
            pixel_height_degrees: metadata.pixel_height_degrees,
        }
    }

    pub fn nearest_meters(&self, longitude: f64, latitude: f64) -> Result<f64> {
        let x = self.reader.pixel_x(longitude)?;
        let y = self.reader.pixel_y(latitude)?;
        Ok(f64::from(self.sample_at_pixel(x, y)?))
    }

    pub fn bilinear_meters(&self, longitude: f64, latitude: f64) -> Result<f64> {
        let pixel_x = ((longitude - self.top_left_longitude) / self.pixel_width_degrees) - 0.5;
        let pixel_y = ((self.top_left_latitude - latitude) / self.pixel_height_degrees) - 0.5;
        let floor_x = pixel_x.floor();
        let floor_y = pixel_y.floor();
        let x0 = clamp(floor_x as i32, self.width);
        let y0 = clamp(floor_y as i32, self.height);
        let x1 = clamp(x0 + 1, self.width);
        let y1 = clamp(y0 + 1, self.height);
        let tx = clamp_unit(pixel_x - floor_x);
        let ty = clamp_unit(pixel_y - floor_y);

        let (v00, v10, v01, v11) = if self.row_cache.is_some() {
            let row0 = self.cached_row(y0)?;
            let row1 = if y1 == y0 {
                row0.clone()
            } else {
                self.cached_row(y1)?
            };
            (
                f64::from(row0[x0 as usize]),
                f64::from(row0[x1 as usize]),
                f64::from(row1[x0 as usize]),
                f64::from(row1[x1 as usize]),
            )
        } else {
            (
                f64::from(self.reader.sample_at_pixel(x0, y0)?),
                f64::from(self.reader.sample_at_pixel(x1, y0)?),
                f64::from(self.reader.sample_at_pixel(x0, y1)?),
                f64::from(self.reader.sample_at_pixel(x1, y1)?),
            )
        };
        let top = lerp(v00, v10, tx);
        let bottom = lerp(v01, v11, tx);
        Ok(lerp(top, bottom, ty))
    }

    fn sample_at_pixel(&self, x: i32, y: i32) -> Result<i16> {
        if let Some(cache) = self.row_cache {
            cache.sample_at_pixel(x, y)
        } else {
            self.reader.sample_at_pixel(x, y)
        }
    }

    fn cached_row(&self, y: i32) -> Result<Vec<i16>> {
        let mut cached = self.cached_rows.borrow_mut();
        if cached.first_y == Some(y) {
            return Ok(cached.first_row.clone());
        }
        if cached.second_y == Some(y) {
            return Ok(cached.second_row.clone());
        }
        let row = self
            .row_cache
            .expect("cached_row is used only when row_cache is set")
            .row(y)?;
        cached.second_y = cached.first_y;
        cached.second_row = std::mem::take(&mut cached.first_row);
        cached.first_y = Some(y);
        cached.first_row = row.clone();
        Ok(row)
    }
}

impl EarthScaleMapping {
    pub fn for_denominator(denominator: i32, min_latitude: f64, max_latitude: f64) -> Result<Self> {
        if denominator <= 0 {
            return Err(GeoError::invalid("scale denominator must be positive"));
        }
        if min_latitude.partial_cmp(&max_latitude) != Some(std::cmp::Ordering::Less) {
            return Err(GeoError::invalid("minLatitude must be below maxLatitude"));
        }
        let circumference = std::f64::consts::PI * 2.0 * WGS84_EQUATORIAL_RADIUS_METERS;
        let width_blocks = round_to_i32_exact(circumference / f64::from(denominator))?;
        let height_blocks =
            round_to_i32_exact(f64::from(width_blocks) * ((max_latitude - min_latitude) / 360.0))?;
        Ok(Self {
            denominator,
            min_latitude,
            max_latitude,
            width_blocks,
            height_blocks,
        })
    }

    pub fn longitude_for_block_x(&self, block_x: i32) -> Result<f64> {
        require_range(block_x, self.width_blocks, "blockX")?;
        Ok(-180.0 + (((f64::from(block_x) + 0.5) / f64::from(self.width_blocks)) * 360.0))
    }

    pub fn latitude_for_block_z(&self, block_z: i32) -> Result<f64> {
        require_range(block_z, self.height_blocks, "blockZ")?;
        Ok(self.max_latitude
            - (((f64::from(block_z) + 0.5) / f64::from(self.height_blocks))
                * (self.max_latitude - self.min_latitude)))
    }

    pub fn block_x_for_longitude(&self, longitude: f64) -> Result<i32> {
        if longitude < -180.0 || longitude >= 180.0 {
            return Err(GeoError::invalid(format!(
                "longitude outside [-180, 180): {longitude}"
            )));
        }
        let value = (((longitude + 180.0) / 360.0) * f64::from(self.width_blocks)).floor() as i32;
        Ok(clamp(value, self.width_blocks))
    }

    pub fn block_z_for_latitude(&self, latitude: f64) -> Result<i32> {
        if latitude < self.min_latitude || latitude > self.max_latitude {
            return Err(GeoError::invalid(format!(
                "latitude outside mapped extent: {latitude}"
            )));
        }
        let value = (((self.max_latitude - latitude) / (self.max_latitude - self.min_latitude))
            * f64::from(self.height_blocks))
        .floor() as i32;
        Ok(clamp(value, self.height_blocks))
    }
}

fn round_to_i32_exact(value: f64) -> Result<i32> {
    let rounded = value.round();
    if !rounded.is_finite() || rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        return Err(GeoError::invalid(format!(
            "rounded mapping dimension outside i32 range: {value}"
        )));
    }
    Ok(rounded as i32)
}

fn clamp(value: i32, exclusive_max: i32) -> i32 {
    if value < 0 {
        0
    } else if value >= exclusive_max {
        exclusive_max - 1
    } else {
        value
    }
}

fn require_range(value: i32, exclusive_max: i32, name: &str) -> Result<()> {
    if value < 0 || value >= exclusive_max {
        return Err(GeoError::invalid(format!(
            "{name} outside mapped grid: {value}"
        )));
    }
    Ok(())
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + ((b - a) * t)
}

fn clamp_unit(value: f64) -> f64 {
    if value < 0.0 {
        0.0
    } else if value > 1.0 {
        1.0
    } else {
        value
    }
}

fn touch_state(state: &mut RowCacheState) -> u64 {
    state.access_clock += 1;
    state.access_clock
}

fn trim_cache_locked(state: &mut RowCacheState, max_rows: usize) {
    while state.rows.len() > max_rows {
        let Some((&oldest_y, _)) = state.rows.iter().min_by_key(|(_y, row)| row.access_stamp)
        else {
            return;
        };
        if state.rows.remove(&oldest_y).is_some() {
            state.evictions += 1;
        }
    }
}

fn parse_bigtiff_heightmap(path: &Path, file: &mut File) -> Result<GeoTiffHeightmapReader> {
    let header = read_exact_at(file, 0, 16)?;
    if header[0] != b'I' || header[1] != b'I' {
        return Err(GeoError::invalid(
            "only little-endian BigTIFF is supported for heightmap input",
        ));
    }
    let magic = read_u16_le(&header, 2)?;
    if magic != 43 {
        return Err(GeoError::invalid(format!(
            "expected BigTIFF magic 43, found {magic}"
        )));
    }
    let offset_size = read_u16_le(&header, 4)?;
    let reserved = read_u16_le(&header, 6)?;
    if offset_size != 8 || reserved != 0 {
        return Err(GeoError::invalid(format!(
            "unsupported BigTIFF header offsetSize={offset_size} reserved={reserved}"
        )));
    }
    let ifd_offset = read_u64_le(&header, 8)?;
    let entries = read_ifd(file, ifd_offset)?;

    let width = i32_from_u64(required_unsigned(&entries, TAG_IMAGE_WIDTH)?, "width")?;
    let height = i32_from_u64(required_unsigned(&entries, TAG_IMAGE_LENGTH)?, "height")?;
    let bits_per_sample = i32_from_u64(
        required_unsigned(&entries, TAG_BITS_PER_SAMPLE)?,
        "bits per sample",
    )?;
    let compression = i32_from_u64(required_unsigned(&entries, TAG_COMPRESSION)?, "compression")?;
    let samples_per_pixel = i32_from_u64(
        required_unsigned(&entries, TAG_SAMPLES_PER_PIXEL)?,
        "samples per pixel",
    )?;
    let rows_per_strip = i32_from_u64(
        required_unsigned(&entries, TAG_ROWS_PER_STRIP)?,
        "rows per strip",
    )?;
    let sample_format = i32_from_u64(
        required_unsigned(&entries, TAG_SAMPLE_FORMAT)?,
        "sample format",
    )?;

    if bits_per_sample != 16
        || sample_format != 2
        || compression != 1
        || samples_per_pixel != 1
        || rows_per_strip != 1
    {
        return Err(GeoError::invalid(format!(
            "unsupported heightmap TIFF layout: bits={bits_per_sample}, sampleFormat={sample_format}, compression={compression}, samplesPerPixel={samples_per_pixel}, rowsPerStrip={rows_per_strip}"
        )));
    }

    let strip_offsets = read_unsigned_array(file, required(&entries, TAG_STRIP_OFFSETS)?)?;
    let byte_counts = read_unsigned_array(file, required(&entries, TAG_STRIP_BYTE_COUNTS)?)?;
    if strip_offsets.len() != height as usize || byte_counts.len() != height as usize {
        return Err(GeoError::invalid(format!(
            "one row strip layout expected: height={height} offsets={} byteCounts={}",
            strip_offsets.len(),
            byte_counts.len()
        )));
    }
    let strip_byte_counts = byte_counts
        .into_iter()
        .map(|value| usize_from_u64(value, "strip byte count"))
        .collect::<Result<Vec<_>>>()?;

    let pixel_scale = read_double_array(file, required(&entries, TAG_MODEL_PIXEL_SCALE)?)?;
    let tiepoint = read_double_array(file, required(&entries, TAG_MODEL_TIEPOINT)?)?;
    if pixel_scale.len() < 2 || tiepoint.len() < 6 {
        return Err(GeoError::invalid("GeoTIFF transform tags are incomplete"));
    }
    let top_left_longitude = tiepoint[3] - (tiepoint[0] * pixel_scale[0]);
    let top_left_latitude = tiepoint[4] + (tiepoint[1] * pixel_scale[1]);

    let epsg_code = parse_epsg(file, entries.get(&TAG_GEO_KEY_DIRECTORY))?;
    let no_data_value = parse_no_data(file, entries.get(&TAG_GDAL_NODATA))?;
    let row_byte_count = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(2))
        .ok_or_else(|| GeoError::invalid("row byte count overflow"))?;
    if row_byte_count > i32::MAX as usize {
        return Err(GeoError::invalid("row byte count overflow"));
    }
    let metadata = GeoTiffMetadata {
        path: path.to_path_buf(),
        width,
        height,
        bits_per_sample,
        sample_format,
        compression,
        rows_per_strip,
        samples_per_pixel,
        top_left_longitude,
        top_left_latitude,
        pixel_width_degrees: pixel_scale[0],
        pixel_height_degrees: pixel_scale[1],
        epsg_code,
        no_data_value,
    };
    Ok(GeoTiffHeightmapReader {
        file: Mutex::new(
            file.try_clone()
                .map_err(|error| GeoError::invalid(error.to_string()))?,
        ),
        metadata,
        strip_offsets,
        strip_byte_counts,
        row_byte_count,
    })
}

fn read_ifd(file: &mut File, offset: u64) -> Result<std::collections::BTreeMap<u16, TiffEntry>> {
    let entry_count_bytes = read_exact_at(file, offset, 8)?;
    let entry_count = read_u64_le(&entry_count_bytes, 0)?;
    if entry_count == 0 || entry_count > 10_000 {
        return Err(GeoError::invalid(format!(
            "unreasonable BigTIFF IFD entry count: {entry_count}"
        )));
    }
    let entry_bytes_len = usize_from_u64(
        entry_count
            .checked_mul(20)
            .ok_or_else(|| GeoError::invalid("IFD byte count overflow"))?,
        "IFD byte count",
    )?;
    let entry_bytes = read_exact_at(file, offset + 8, entry_bytes_len)?;
    let mut entries = std::collections::BTreeMap::new();
    let mut cursor = 0usize;
    for _ in 0..entry_count {
        let tag = read_u16_le(&entry_bytes, cursor)?;
        let field_type = read_u16_le(&entry_bytes, cursor + 2)?;
        let count = read_u64_le(&entry_bytes, cursor + 4)?;
        let inline = checked_slice(&entry_bytes, cursor + 12, 8)?;
        let mut inline_bytes = [0u8; 8];
        inline_bytes.copy_from_slice(inline);
        let value_or_offset = u64::from_le_bytes(inline_bytes);
        entries.insert(
            tag,
            TiffEntry {
                tag,
                field_type,
                count,
                value_or_offset,
                inline_bytes,
            },
        );
        cursor = cursor
            .checked_add(20)
            .ok_or_else(|| GeoError::invalid("IFD entry cursor overflow"))?;
    }
    Ok(entries)
}

fn required(entries: &std::collections::BTreeMap<u16, TiffEntry>, tag: u16) -> Result<&TiffEntry> {
    entries
        .get(&tag)
        .ok_or_else(|| GeoError::invalid(format!("missing required TIFF tag {tag}")))
}

fn required_unsigned(
    entries: &std::collections::BTreeMap<u16, TiffEntry>,
    tag: u16,
) -> Result<u64> {
    let values = read_unsigned_array_inline(required(entries, tag)?)?;
    if values.len() != 1 {
        return Err(GeoError::invalid(format!("expected scalar TIFF tag {tag}")));
    }
    Ok(values[0])
}

fn read_unsigned_array_inline(entry: &TiffEntry) -> Result<Vec<u64>> {
    let bytes = entry_value_bytes_inline(entry)?;
    read_unsigned_array_from_bytes(&bytes, entry)
}

fn read_unsigned_array(file: &mut File, entry: &TiffEntry) -> Result<Vec<u64>> {
    let bytes = entry_value_bytes(file, entry)?;
    read_unsigned_array_from_bytes(&bytes, entry)
}

fn read_unsigned_array_from_bytes(bytes: &[u8], entry: &TiffEntry) -> Result<Vec<u64>> {
    let mut values = Vec::with_capacity(usize_from_u64(entry.count, "TIFF value count")?);
    for index in 0..entry.count {
        let offset = usize_from_u64(
            index
                .checked_mul(type_size(entry.field_type)?)
                .ok_or_else(|| GeoError::invalid("TIFF array offset overflow"))?,
            "TIFF array offset",
        )?;
        values.push(match entry.field_type {
            TYPE_SHORT => u64::from(read_u16_le(bytes, offset)?),
            TYPE_LONG => u64::from(read_u32_le(bytes, offset)?),
            TYPE_LONG8 => read_u64_le(bytes, offset)?,
            _ => {
                return Err(GeoError::invalid(format!(
                    "unsupported unsigned TIFF type {} for tag {}",
                    entry.field_type, entry.tag
                )))
            }
        });
    }
    Ok(values)
}

fn read_double_array(file: &mut File, entry: &TiffEntry) -> Result<Vec<f64>> {
    if entry.field_type != TYPE_DOUBLE {
        return Err(GeoError::invalid(format!(
            "expected DOUBLE tag {}, found type {}",
            entry.tag, entry.field_type
        )));
    }
    let bytes = entry_value_bytes(file, entry)?;
    let mut values = Vec::with_capacity(usize_from_u64(entry.count, "DOUBLE value count")?);
    for index in 0..entry.count {
        let offset = usize_from_u64(
            index
                .checked_mul(8)
                .ok_or_else(|| GeoError::invalid("DOUBLE array offset overflow"))?,
            "DOUBLE array offset",
        )?;
        values.push(f64::from_le_bytes(
            checked_slice(&bytes, offset, 8)?
                .try_into()
                .expect("slice length is 8"),
        ));
    }
    Ok(values)
}

fn read_unsigned_short_array(file: &mut File, entry: &TiffEntry) -> Result<Vec<u16>> {
    if entry.field_type != TYPE_SHORT {
        return Err(GeoError::invalid(format!(
            "expected SHORT tag {}, found type {}",
            entry.tag, entry.field_type
        )));
    }
    let bytes = entry_value_bytes(file, entry)?;
    let mut values = Vec::with_capacity(usize_from_u64(entry.count, "SHORT value count")?);
    for index in 0..entry.count {
        let offset = usize_from_u64(
            index
                .checked_mul(2)
                .ok_or_else(|| GeoError::invalid("SHORT array offset overflow"))?,
            "SHORT array offset",
        )?;
        values.push(read_u16_le(&bytes, offset)?);
    }
    Ok(values)
}

fn parse_epsg(file: &mut File, entry: Option<&TiffEntry>) -> Result<Option<i32>> {
    let Some(entry) = entry else {
        return Ok(None);
    };
    let directory = read_unsigned_short_array(file, entry)?;
    if directory.len() < 4 {
        return Ok(None);
    }
    let key_count = usize::from(directory[3]);
    for key in 0..key_count {
        let offset = 4 + (key * 4);
        if offset + 3 >= directory.len() {
            break;
        }
        let key_id = directory[offset];
        let tag_location = directory[offset + 1];
        let value = directory[offset + 3];
        if tag_location == 0 && (key_id == 2048 || key_id == 3072) {
            return Ok(Some(i32::from(value)));
        }
    }
    Ok(None)
}

fn parse_no_data(file: &mut File, entry: Option<&TiffEntry>) -> Result<Option<f64>> {
    let Some(entry) = entry else {
        return Ok(None);
    };
    if entry.field_type != TYPE_ASCII {
        return Err(GeoError::invalid("GDAL_NODATA tag is not ASCII"));
    }
    let bytes = entry_value_bytes(file, entry)?;
    let text = String::from_utf8_lossy(&bytes)
        .replace('\0', "")
        .trim()
        .to_string();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<f64>()
        .map(Some)
        .map_err(|error| GeoError::invalid(error.to_string()))
}

fn entry_value_bytes_inline(entry: &TiffEntry) -> Result<Vec<u8>> {
    let byte_size = entry
        .count
        .checked_mul(type_size(entry.field_type)?)
        .ok_or_else(|| GeoError::invalid(format!("TIFF tag too large to read: {}", entry.tag)))?;
    if byte_size > i32::MAX as u64 {
        return Err(GeoError::invalid(format!(
            "TIFF tag too large to read into memory: {}",
            entry.tag
        )));
    }
    let size = byte_size as usize;
    if size <= 8 {
        return Ok(entry.inline_bytes[..size].to_vec());
    }
    Err(GeoError::invalid(format!(
        "TIFF tag {} requires external value storage",
        entry.tag
    )))
}

fn entry_value_bytes(file: &mut File, entry: &TiffEntry) -> Result<Vec<u8>> {
    let byte_size = entry
        .count
        .checked_mul(type_size(entry.field_type)?)
        .ok_or_else(|| GeoError::invalid(format!("TIFF tag too large to read: {}", entry.tag)))?;
    if byte_size > i32::MAX as u64 {
        return Err(GeoError::invalid(format!(
            "TIFF tag too large to read into memory: {}",
            entry.tag
        )));
    }
    let size = byte_size as usize;
    if size <= 8 {
        return Ok(entry.inline_bytes[..size].to_vec());
    }
    read_exact_at(file, entry.value_or_offset, size)
}

fn type_size(field_type: u16) -> Result<u64> {
    match field_type {
        TYPE_ASCII => Ok(1),
        TYPE_SHORT => Ok(2),
        TYPE_LONG => Ok(4),
        TYPE_DOUBLE | TYPE_LONG8 => Ok(8),
        _ => Err(GeoError::invalid(format!(
            "unsupported TIFF field type {field_type}"
        ))),
    }
}

#[derive(Clone, Debug)]
struct TiffEntry {
    tag: u16,
    field_type: u16,
    count: u64,
    value_or_offset: u64,
    inline_bytes: [u8; 8],
}

fn checked_slice(data: &[u8], offset: usize, size: usize) -> Result<&[u8]> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| GeoError::invalid(format!("unexpected end of file at {offset}")))?;
    data.get(offset..end)
        .ok_or_else(|| GeoError::invalid(format!("unexpected end of file at {offset}")))
}

fn read_exact_at(file: &mut File, position: u64, size: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; size];
    file.seek(SeekFrom::Start(position))
        .map_err(|error| GeoError::invalid(error.to_string()))?;
    file.read_exact(&mut bytes)
        .map_err(|_| GeoError::invalid(format!("unexpected end of file at {position}")))?;
    Ok(bytes)
}

fn read_i16_le(data: &[u8], offset: usize) -> Result<i16> {
    Ok(i16::from_le_bytes(
        checked_slice(data, offset, 2)?
            .try_into()
            .expect("slice length is 2"),
    ))
}

fn read_u16_le(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        checked_slice(data, offset, 2)?
            .try_into()
            .expect("slice length is 2"),
    ))
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        checked_slice(data, offset, 4)?
            .try_into()
            .expect("slice length is 4"),
    ))
}

fn read_u64_le(data: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        checked_slice(data, offset, 8)?
            .try_into()
            .expect("slice length is 8"),
    ))
}

fn usize_from_u64(value: u64, name: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| GeoError::invalid(format!("{name} exceeds usize")))
}

fn i32_from_u64(value: u64, name: &str) -> Result<i32> {
    i32::try_from(value).map_err(|_| GeoError::invalid(format!("{name} exceeds i32")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn scale_500_matches_java_earth_scale_mapping_test() {
        let mapping = EarthScaleMapping::for_denominator(500, -84.0, 84.0).unwrap();

        assert_eq!(mapping.width_blocks, 80_150);
        assert_eq!(mapping.height_blocks, 37_403);
        assert_eq!(mapping.block_x_for_longitude(0.0).unwrap(), 40_075);
        assert_eq!(mapping.block_z_for_latitude(0.0).unwrap(), 18_701);
        assert!(mapping.longitude_for_block_x(40_075).unwrap().abs() < 0.01);
        assert!(mapping.latitude_for_block_z(18_701).unwrap().abs() < 0.01);
    }

    #[test]
    fn validation_matches_java_earth_scale_mapping_test() {
        assert!(EarthScaleMapping::for_denominator(0, -84.0, 84.0).is_err());
        assert!(EarthScaleMapping::for_denominator(500, 84.0, -84.0).is_err());

        let mapping = EarthScaleMapping::for_denominator(500, -84.0, 84.0).unwrap();
        assert!(mapping.block_x_for_longitude(180.0).is_err());
        assert!(mapping.block_x_for_longitude(-180.0).is_ok());
        assert_eq!(mapping.block_x_for_longitude(f64::NAN).unwrap(), 0);
        assert!(mapping.block_z_for_latitude(84.0).is_ok());
        assert!(mapping.block_z_for_latitude(-84.0).is_ok());
        assert_eq!(mapping.block_z_for_latitude(f64::NAN).unwrap(), 0);
        assert!(mapping.longitude_for_block_x(-1).is_err());
        assert!(mapping.longitude_for_block_x(mapping.width_blocks).is_err());
        assert!(mapping.latitude_for_block_z(-1).is_err());
        assert!(mapping.latitude_for_block_z(mapping.height_blocks).is_err());
    }

    #[test]
    fn global_scale_matches_java_rounding_contract() {
        let mapping = EarthScaleMapping::for_denominator(1000, -90.0, 90.0).unwrap();

        assert_eq!(mapping.width_blocks, 40_075);
        assert_eq!(mapping.height_blocks, 20_038);
        assert_eq!(mapping.block_x_for_longitude(-179.999).unwrap(), 0);
        assert_eq!(
            mapping.block_x_for_longitude(179.999).unwrap(),
            mapping.width_blocks - 1
        );
        assert_eq!(mapping.block_z_for_latitude(90.0).unwrap(), 0);
        assert_eq!(
            mapping.block_z_for_latitude(-90.0).unwrap(),
            mapping.height_blocks - 1
        );
    }

    #[test]
    fn geo_tiff_metadata_sample_type_names_match_java() {
        let mut metadata = metadata_with_sample_layout(16, 2);
        assert_eq!(metadata.sample_type_name(), "Int16");

        metadata = metadata_with_sample_layout(32, 3);
        assert_eq!(metadata.sample_type_name(), "Float32");

        metadata = metadata_with_sample_layout(8, 1);
        assert_eq!(metadata.sample_type_name(), "bits=8, sampleFormat=1");
    }

    #[test]
    fn synthetic_bigtiff_heightmap_reader_matches_java_test_fixture() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-bigtiff.tif");
        fs::write(&path, synthetic_bigtiff_heightmap()).unwrap();

        let reader = GeoTiffHeightmapReader::open(&path).unwrap();
        let metadata = reader.metadata();
        assert_eq!(metadata.width, 3);
        assert_eq!(metadata.height, 3);
        assert_eq!(metadata.bits_per_sample, 16);
        assert_eq!(metadata.sample_format, 2);
        assert_eq!(metadata.compression, 1);
        assert_eq!(metadata.rows_per_strip, 1);
        assert_eq!(metadata.no_data_value, None);
        assert_eq!(reader.pixel_x(102.2).unwrap(), 2);
        assert_eq!(reader.pixel_y(48.1).unwrap(), 1);
        assert_eq!(reader.sample_at_pixel(0, 0).unwrap(), 10);
        assert_eq!(reader.sample_at_pixel(2, 0).unwrap(), 30);
        assert_eq!(reader.sample_at_pixel(0, 1).unwrap(), -5);
        assert_eq!(
            reader.sample_at_longitude_latitude(102.2, 48.1).unwrap(),
            1000
        );
        let mut row = [0i16; 3];
        reader.read_row(1, &mut row).unwrap();
        assert_eq!(row, [-5, 0, 1000]);
        reader.read_row(2, &mut row).unwrap();
        assert_eq!(row, [7, 8, 9]);
    }

    #[test]
    fn synthetic_bigtiff_reader_validation_matches_java_edges() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-bigtiff.tif");
        fs::write(&path, synthetic_bigtiff_heightmap()).unwrap();
        let reader = GeoTiffHeightmapReader::open(&path).unwrap();

        assert!(reader.sample_at_pixel(-1, 0).is_err());
        assert!(reader.sample_at_pixel(0, -1).is_err());
        assert!(reader.sample_at_pixel(3, 0).is_err());
        assert!(reader.sample_at_pixel(0, 3).is_err());
        assert!(reader.pixel_x(99.9).is_err());
        assert!(reader.pixel_y(46.9).is_err());
        let mut short_row = [0i16; 2];
        assert!(reader.read_row(0, &mut short_row).is_err());
        let mut row = [0i16; 3];
        assert!(reader.read_row(-1, &mut row).is_err());
        assert!(reader.read_row(3, &mut row).is_err());
    }

    #[test]
    fn heightmap_scalar_sampler_uncached_matches_java_fixture() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-bigtiff-sampler.tif");
        fs::write(&path, synthetic_bigtiff_scalar_sampler()).unwrap();

        let reader = GeoTiffHeightmapReader::open(&path).unwrap();
        let sampler = HeightmapScalarSampler::new(&reader);

        assert_eq!(sampler.nearest_meters(0.5, 2.5).unwrap(), 0.0);
        assert_eq!(sampler.bilinear_meters(0.5, 2.5).unwrap(), 0.0);
        assert_eq!(sampler.bilinear_meters(1.0, 2.5).unwrap(), 5.0);
        assert_eq!(sampler.bilinear_meters(0.5, 2.0).unwrap(), 15.0);
        assert_eq!(sampler.bilinear_meters(1.0, 2.0).unwrap(), 20.0);
        assert_eq!(sampler.nearest_meters(1.5, 1.5).unwrap(), 40.0);
        assert_eq!(sampler.bilinear_meters(1.0, 2.0).unwrap(), 20.0);
    }

    #[test]
    fn geo_tiff_row_cache_stats_match_java_sequential_fixture() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-bigtiff-row-cache.tif");
        fs::write(&path, synthetic_bigtiff_row_cache()).unwrap();

        let reader = GeoTiffHeightmapReader::open(&path).unwrap();
        let cache = GeoTiffRowCache::new(&reader, 2).unwrap();

        assert_eq!(cache.sample_at_pixel(0, 0).unwrap(), 1);
        assert_eq!(cache.sample_at_pixel(1, 0).unwrap(), 2);
        assert_eq!(cache.sample_at_pixel(0, 1).unwrap(), 4);
        assert_eq!(cache.sample_at_pixel(0, 2).unwrap(), 7);
        let stats = cache.stats();
        assert_eq!(stats.max_rows, 2);
        assert_eq!(stats.resident_rows, 2);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 3);
        assert_eq!(stats.evictions, 1);
    }

    #[test]
    fn geo_tiff_row_cache_prefetch_records_next_row_load() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-bigtiff-prefetch.tif");
        fs::write(&path, synthetic_bigtiff_row_cache()).unwrap();

        let reader = GeoTiffHeightmapReader::open(&path).unwrap();
        let cache = GeoTiffRowCache::with_prefetch(&reader, 3, 1).unwrap();

        assert_eq!(cache.sample_at_pixel(0, 0).unwrap(), 1);
        assert_eq!(cache.sample_at_pixel(0, 1).unwrap(), 4);
        let stats = cache.stats();
        assert_eq!(stats.prefetch_rows, 1);
        assert!(stats.prefetch_requests >= 1);
        assert!(stats.prefetch_loads >= 1);
    }

    #[test]
    fn geo_tiff_row_cache_large_prefetch_saturates_like_bounded_int() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-bigtiff-huge-prefetch.tif");
        fs::write(&path, synthetic_bigtiff_row_cache()).unwrap();

        let reader = GeoTiffHeightmapReader::open(&path).unwrap();
        let cache = GeoTiffRowCache::with_prefetch(&reader, 3, usize::MAX).unwrap();

        assert_eq!(cache.sample_at_pixel(0, 0).unwrap(), 1);
        assert_eq!(cache.sample_at_pixel(0, 1).unwrap(), 4);
        let stats = cache.stats();
        assert_eq!(stats.prefetch_rows, usize::MAX);
        assert_eq!(stats.prefetch_requests, 2);
        assert_eq!(stats.prefetch_loads, 2);
    }

    #[test]
    fn cached_heightmap_scalar_sampler_reuses_two_local_rows_like_java() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-bigtiff-row-memo.tif");
        fs::write(&path, synthetic_bigtiff_scalar_sampler()).unwrap();

        let reader = GeoTiffHeightmapReader::open(&path).unwrap();
        let cache = GeoTiffRowCache::new(&reader, 2).unwrap();
        let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);

        for _ in 0..32 {
            assert_eq!(sampler.bilinear_meters(1.0, 2.0).unwrap(), 20.0);
        }
        let stats = cache.stats();
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.hits, 0);
    }

    fn metadata_with_sample_layout(bits_per_sample: i32, sample_format: i32) -> GeoTiffMetadata {
        GeoTiffMetadata {
            path: PathBuf::from("synthetic.tif"),
            width: 2,
            height: 2,
            bits_per_sample,
            sample_format,
            compression: 1,
            rows_per_strip: 1,
            samples_per_pixel: 1,
            top_left_longitude: -180.0,
            top_left_latitude: 90.0,
            pixel_width_degrees: 0.1,
            pixel_height_degrees: 0.1,
            epsg_code: Some(4326),
            no_data_value: None,
        }
    }

    fn synthetic_bigtiff_heightmap() -> Vec<u8> {
        let width = 3usize;
        let height = 3usize;
        let pixel_bytes = width * height * 2;
        let ifd_offset = 16 + pixel_bytes;
        let entry_count = 14usize;
        let ifd_bytes = 8 + (entry_count * 20) + 8;
        let data_start = ifd_offset + ifd_bytes;
        let strip_offsets_offset = data_start;
        let strip_byte_counts_offset = strip_offsets_offset + (height * 8);
        let pixel_scale_offset = strip_byte_counts_offset + (height * 4);
        let tiepoint_offset = pixel_scale_offset + (3 * 8);
        let file_size = tiepoint_offset + (6 * 8);
        let mut out = vec![0u8; file_size];

        out[0] = b'I';
        out[1] = b'I';
        put_u16(&mut out, 2, 43);
        put_u16(&mut out, 4, 8);
        put_u16(&mut out, 6, 0);
        put_u64(&mut out, 8, ifd_offset as u64);

        let samples = [10i16, 20, 30, -5, 0, 1000, 7, 8, 9];
        let mut cursor = 16;
        for sample in samples {
            put_i16(&mut out, cursor, sample);
            cursor += 2;
        }

        cursor = ifd_offset;
        put_u64(&mut out, cursor, entry_count as u64);
        cursor += 8;
        for (tag, field_type, count, value_or_offset) in [
            (256, 4, 1, width as u64),
            (257, 4, 1, height as u64),
            (258, 3, 1, 16),
            (259, 3, 1, 1),
            (262, 3, 1, 1),
            (273, 16, height as u64, strip_offsets_offset as u64),
            (277, 3, 1, 1),
            (278, 3, 1, 1),
            (279, 4, height as u64, strip_byte_counts_offset as u64),
            (284, 3, 1, 1),
            (339, 3, 1, 2),
            (33550, 12, 3, pixel_scale_offset as u64),
            (33922, 12, 6, tiepoint_offset as u64),
            (34735, 3, 4, 1),
        ] {
            put_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += 20;
        }
        put_u64(&mut out, cursor, 0);

        cursor = strip_offsets_offset;
        for y in 0..height {
            put_u64(&mut out, cursor, 16 + ((y * width * 2) as u64));
            cursor += 8;
        }

        cursor = strip_byte_counts_offset;
        for _ in 0..height {
            put_u32(&mut out, cursor, (width * 2) as u32);
            cursor += 4;
        }

        cursor = pixel_scale_offset;
        put_f64(&mut out, cursor, 1.0);
        put_f64(&mut out, cursor + 8, 1.0);
        put_f64(&mut out, cursor + 16, 0.0);

        cursor = tiepoint_offset;
        put_f64(&mut out, cursor, 0.0);
        put_f64(&mut out, cursor + 8, 0.0);
        put_f64(&mut out, cursor + 16, 0.0);
        put_f64(&mut out, cursor + 24, 100.0);
        put_f64(&mut out, cursor + 32, 50.0);
        put_f64(&mut out, cursor + 40, 0.0);
        out
    }

    fn synthetic_bigtiff_row_cache() -> Vec<u8> {
        let width = 3usize;
        let height = 3usize;
        let pixel_bytes = width * height * 2;
        let ifd_offset = 16 + pixel_bytes;
        let entry_count = 13usize;
        let ifd_bytes = 8 + (entry_count * 20) + 8;
        let data_start = ifd_offset + ifd_bytes;
        let strip_offsets_offset = data_start;
        let strip_byte_counts_offset = strip_offsets_offset + (height * 8);
        let pixel_scale_offset = strip_byte_counts_offset + (height * 4);
        let tiepoint_offset = pixel_scale_offset + (3 * 8);
        let file_size = tiepoint_offset + (6 * 8);
        let mut out = vec![0u8; file_size];

        out[0] = b'I';
        out[1] = b'I';
        put_u16(&mut out, 2, 43);
        put_u16(&mut out, 4, 8);
        put_u16(&mut out, 6, 0);
        put_u64(&mut out, 8, ifd_offset as u64);

        let mut cursor = 16;
        for sample in 1i16..=9 {
            put_i16(&mut out, cursor, sample);
            cursor += 2;
        }

        cursor = ifd_offset;
        put_u64(&mut out, cursor, entry_count as u64);
        cursor += 8;
        for (tag, field_type, count, value_or_offset) in [
            (256, 4, 1, width as u64),
            (257, 4, 1, height as u64),
            (258, 3, 1, 16),
            (259, 3, 1, 1),
            (262, 3, 1, 1),
            (273, 16, height as u64, strip_offsets_offset as u64),
            (277, 3, 1, 1),
            (278, 3, 1, 1),
            (279, 4, height as u64, strip_byte_counts_offset as u64),
            (284, 3, 1, 1),
            (339, 3, 1, 2),
            (33550, 12, 3, pixel_scale_offset as u64),
            (33922, 12, 6, tiepoint_offset as u64),
        ] {
            put_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += 20;
        }
        put_u64(&mut out, cursor, 0);

        cursor = strip_offsets_offset;
        for y in 0..height {
            put_u64(&mut out, cursor, 16 + ((y * width * 2) as u64));
            cursor += 8;
        }

        cursor = strip_byte_counts_offset;
        for _ in 0..height {
            put_u32(&mut out, cursor, (width * 2) as u32);
            cursor += 4;
        }

        cursor = pixel_scale_offset;
        put_f64(&mut out, cursor, 1.0);
        put_f64(&mut out, cursor + 8, 1.0);
        put_f64(&mut out, cursor + 16, 0.0);

        cursor = tiepoint_offset;
        put_f64(&mut out, cursor, 0.0);
        put_f64(&mut out, cursor + 8, 0.0);
        put_f64(&mut out, cursor + 16, 0.0);
        put_f64(&mut out, cursor + 24, 0.0);
        put_f64(&mut out, cursor + 32, 3.0);
        put_f64(&mut out, cursor + 40, 0.0);
        out
    }

    fn synthetic_bigtiff_scalar_sampler() -> Vec<u8> {
        let width = 2usize;
        let height = 3usize;
        let pixel_bytes = width * height * 2;
        let ifd_offset = 16 + pixel_bytes;
        let entry_count = 13usize;
        let ifd_bytes = 8 + (entry_count * 20) + 8;
        let data_start = ifd_offset + ifd_bytes;
        let strip_offsets_offset = data_start;
        let strip_byte_counts_offset = strip_offsets_offset + (height * 8);
        let pixel_scale_offset = strip_byte_counts_offset + (height * 4);
        let tiepoint_offset = pixel_scale_offset + (3 * 8);
        let file_size = tiepoint_offset + (6 * 8);
        let mut out = vec![0u8; file_size];

        out[0] = b'I';
        out[1] = b'I';
        put_u16(&mut out, 2, 43);
        put_u16(&mut out, 4, 8);
        put_u16(&mut out, 6, 0);
        put_u64(&mut out, 8, ifd_offset as u64);

        let samples = [0i16, 10, 30, 40, 50, 60];
        let mut cursor = 16;
        for sample in samples {
            put_i16(&mut out, cursor, sample);
            cursor += 2;
        }

        cursor = ifd_offset;
        put_u64(&mut out, cursor, entry_count as u64);
        cursor += 8;
        for (tag, field_type, count, value_or_offset) in [
            (256, 4, 1, width as u64),
            (257, 4, 1, height as u64),
            (258, 3, 1, 16),
            (259, 3, 1, 1),
            (262, 3, 1, 1),
            (273, 16, height as u64, strip_offsets_offset as u64),
            (277, 3, 1, 1),
            (278, 3, 1, 1),
            (279, 4, height as u64, strip_byte_counts_offset as u64),
            (284, 3, 1, 1),
            (339, 3, 1, 2),
            (33550, 12, 3, pixel_scale_offset as u64),
            (33922, 12, 6, tiepoint_offset as u64),
        ] {
            put_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += 20;
        }
        put_u64(&mut out, cursor, 0);

        cursor = strip_offsets_offset;
        for y in 0..height {
            put_u64(&mut out, cursor, 16 + ((y * width * 2) as u64));
            cursor += 8;
        }

        cursor = strip_byte_counts_offset;
        for _ in 0..height {
            put_u32(&mut out, cursor, (width * 2) as u32);
            cursor += 4;
        }

        cursor = pixel_scale_offset;
        put_f64(&mut out, cursor, 1.0);
        put_f64(&mut out, cursor + 8, 1.0);
        put_f64(&mut out, cursor + 16, 0.0);

        cursor = tiepoint_offset;
        put_f64(&mut out, cursor, 0.0);
        put_f64(&mut out, cursor + 8, 0.0);
        put_f64(&mut out, cursor + 16, 0.0);
        put_f64(&mut out, cursor + 24, 0.0);
        put_f64(&mut out, cursor + 32, 3.0);
        put_f64(&mut out, cursor + 40, 0.0);
        out
    }

    fn put_entry(
        out: &mut [u8],
        offset: usize,
        tag: u16,
        field_type: u16,
        count: u64,
        value_or_offset: u64,
    ) {
        put_u16(out, offset, tag);
        put_u16(out, offset + 2, field_type);
        put_u64(out, offset + 4, count);
        put_u64(out, offset + 12, value_or_offset);
    }

    fn put_i16(out: &mut [u8], offset: usize, value: i16) {
        out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u16(out: &mut [u8], offset: usize, value: u16) {
        out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(out: &mut [u8], offset: usize, value: u32) {
        out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(out: &mut [u8], offset: usize, value: u64) {
        out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn put_f64(out: &mut [u8], offset: usize, value: f64) {
        out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
