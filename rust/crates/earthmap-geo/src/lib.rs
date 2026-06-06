#![forbid(unsafe_code)]

use quick_xml::events::{BytesCData, BytesRef, BytesStart, BytesText, Event};
use quick_xml::Reader;
use quick_xml::XmlVersion;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
};
use weezl::{decode::Decoder as LzwDecoder, BitOrder};

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
const TAG_PREDICTOR: u16 = 317;
const TAG_SAMPLE_FORMAT: u16 = 339;
const TAG_MODEL_PIXEL_SCALE: u16 = 33550;
const TAG_MODEL_TIEPOINT: u16 = 33922;
const TAG_GEO_KEY_DIRECTORY: u16 = 34735;
const TAG_GDAL_NODATA: u16 = 42113;

const TYPE_BYTE: u16 = 1;
const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_RATIONAL: u16 = 5;
const TYPE_DOUBLE: u16 = 12;
const TYPE_LONG8: u16 = 16;

const CLASSIC_TIFF_MAGIC: u16 = 42;
const BIG_TIFF_MAGIC: u16 = 43;
const CLASSIC_IFD_ENTRY_BYTES: usize = 12;
const BIG_IFD_ENTRY_BYTES: usize = 20;
const SINGLE_BAND_BLOCK_WIDTH: i32 = 128;
const SINGLE_BAND_BLOCK_HEIGHT: i32 = 128;
pub const DEFAULT_RGB_TILE_CACHE_ENTRIES: usize = 256;
const TIFF_COMPRESSION_NONE: i32 = 1;
const TIFF_COMPRESSION_LZW: i32 = 5;
const TIFF_COMPRESSION_PACKBITS: i32 = 32773;
const TAG_PLANAR_CONFIGURATION: u16 = 284;
const TAG_TILE_WIDTH: u16 = 322;
const TAG_TILE_LENGTH: u16 = 323;
const TAG_TILE_OFFSETS: u16 = 324;
const TAG_TILE_BYTE_COUNTS: u16 = 325;

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

#[derive(Debug)]
pub struct GeoTiffFloat32Reader {
    file: Mutex<File>,
    metadata: GeoTiffMetadata,
    strip_offsets: Vec<u64>,
    strip_byte_counts: Vec<usize>,
}

#[derive(Debug)]
pub struct GeoTiffSingleBandReader {
    file: Mutex<File>,
    metadata: GeoTiffMetadata,
    byte_order: TiffByteOrder,
    bytes_per_sample: usize,
    tile_width: i32,
    tile_length: i32,
    tiles_across: i32,
    tile_offsets: Vec<u64>,
    tile_byte_counts: Vec<usize>,
    tile_cache_entries: usize,
    tile_state: Mutex<SingleBandTileState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RgbColor {
    pub available: bool,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl RgbColor {
    pub const fn of(red: u8, green: u8, blue: u8) -> Self {
        Self {
            available: true,
            red,
            green,
            blue,
        }
    }

    pub const fn unavailable() -> Self {
        Self {
            available: false,
            red: 0,
            green: 0,
            blue: 0,
        }
    }

    pub fn is_near_black(self) -> bool {
        self.available && self.red <= 4 && self.green <= 4 && self.blue <= 4
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GeoTiffRgbReaderStats {
    pub max_tile_cache_entries: usize,
    pub resident_tiles: usize,
    pub tile_hits: u64,
    pub tile_misses: u64,
    pub tile_evictions: u64,
}

#[derive(Debug)]
pub struct GeoTiffRgbReader {
    file: Mutex<File>,
    width: i32,
    height: i32,
    samples_per_pixel: usize,
    tile_width: i32,
    tile_length: i32,
    tiles_across: i32,
    tile_offsets: Vec<u64>,
    tile_byte_counts: Vec<usize>,
    tile_cache_entries: usize,
    tile_state: Mutex<RgbTileState>,
}

#[derive(Debug, Default)]
struct RgbTileState {
    tiles: BTreeMap<usize, CachedTile>,
    access_clock: u64,
    tile_hits: u64,
    tile_misses: u64,
    tile_evictions: u64,
}

#[derive(Debug, Default)]
struct SingleBandTileState {
    tiles: BTreeMap<usize, CachedTile>,
    access_clock: u64,
    tile_hits: u64,
    tile_misses: u64,
    tile_evictions: u64,
}

#[derive(Clone, Debug)]
struct CachedTile {
    bytes: Arc<[u8]>,
    access_stamp: u64,
}

#[derive(Debug)]
pub struct VrtRgbMosaicReader {
    vrt_path: PathBuf,
    raster_width: i32,
    raster_height: i32,
    origin_x: f64,
    pixel_width: f64,
    origin_y: f64,
    pixel_height: f64,
    sources: Vec<VrtSource>,
    source_lookup: VrtSourceLookup,
    tile_cache_entries: usize,
    readers: Mutex<HashMap<VrtReaderKey, Arc<GeoTiffRgbReader>>>,
    sample_nearest_requests: AtomicU64,
    sample_averaged_requests: AtomicU64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VrtRgbMosaicStats {
    pub source_count: usize,
    pub open_readers: usize,
    pub resident_tiles: u64,
    pub tile_hits: u64,
    pub tile_misses: u64,
    pub tile_evictions: u64,
    pub sample_nearest_requests: u64,
    pub sample_averaged_requests: u64,
    pub indexed_source_lookup: bool,
    pub source_lookup_cells: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VrtSource {
    path: PathBuf,
    src: VrtRect,
    dst: VrtRect,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct VrtReaderKey {
    path: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VrtRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Clone, Debug)]
enum VrtSourceLookup {
    Linear {
        sources: Vec<VrtSource>,
    },
    Grid {
        cell_width: i32,
        cell_height: i32,
        columns: i32,
        grid: Vec<Option<usize>>,
        sources: Vec<VrtSource>,
    },
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
    values: Arc<[i16]>,
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

impl GeoTiffFloat32Reader {
    pub fn open_if_present(path: impl AsRef<Path>) -> Result<Option<Self>> {
        let path = path.as_ref();
        if !path.is_file() {
            return Ok(None);
        }
        Self::open(path).map(Some)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut file = File::open(path).map_err(|error| GeoError::invalid(error.to_string()))?;
        parse_bigtiff_float32(path, &mut file)
    }

    pub fn metadata(&self) -> &GeoTiffMetadata {
        &self.metadata
    }

    pub fn sample_nearest(&self, longitude: f64, latitude: f64) -> Result<Option<f64>> {
        let x = self.pixel_x(longitude);
        let y = self.pixel_y(latitude);
        if x < 0 || x >= self.metadata.width || y < 0 || y >= self.metadata.height {
            return Ok(None);
        }
        let value = f64::from(self.sample_at_pixel(x, y)?);
        if self
            .metadata
            .no_data_value
            .is_some_and(|no_data| java_double_compare_equal(value, no_data))
        {
            return Ok(None);
        }
        Ok(Some(value))
    }

    pub fn sample_bilinear(&self, longitude: f64, latitude: f64) -> Result<Option<f64>> {
        let pixel_x = ((longitude - self.metadata.top_left_longitude)
            / self.metadata.pixel_width_degrees)
            - 0.5;
        let pixel_y = ((self.metadata.top_left_latitude - latitude)
            / self.metadata.pixel_height_degrees)
            - 0.5;
        let floor_x = pixel_x.floor();
        let floor_y = pixel_y.floor();
        let x0 = clamp(floor_x as i32, self.metadata.width);
        let y0 = clamp(floor_y as i32, self.metadata.height);
        let x1 = clamp(x0 + 1, self.metadata.width);
        let y1 = clamp(y0 + 1, self.metadata.height);
        let tx = clamp_unit(pixel_x - floor_x);
        let ty = clamp_unit(pixel_y - floor_y);

        let samples = [
            (
                self.sample_optional_at_pixel(x0, y0)?,
                (1.0 - tx) * (1.0 - ty),
            ),
            (self.sample_optional_at_pixel(x1, y0)?, tx * (1.0 - ty)),
            (self.sample_optional_at_pixel(x0, y1)?, (1.0 - tx) * ty),
            (self.sample_optional_at_pixel(x1, y1)?, tx * ty),
        ];
        let mut weighted = 0.0;
        let mut weight_sum = 0.0;
        for (sample, weight) in samples {
            if let Some(value) = sample {
                weighted += value * weight;
                weight_sum += weight;
            }
        }
        if weight_sum == 0.0 {
            Ok(None)
        } else {
            Ok(Some(weighted / weight_sum))
        }
    }

    pub fn sample_at_pixel(&self, x: i32, y: i32) -> Result<f32> {
        self.require_pixel(x, y)?;
        let expected_bytes = usize::try_from(self.metadata.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| GeoError::invalid("row byte count overflow"))?;
        let byte_count = self.strip_byte_counts[y as usize];
        if byte_count < expected_bytes {
            return Err(GeoError::invalid(format!(
                "strip byte count is too small for row {y}: {byte_count}"
            )));
        }
        let offset = self.strip_offsets[y as usize]
            .checked_add(u64::try_from(x).expect("validated x is non-negative") * 4)
            .ok_or_else(|| GeoError::invalid("sample offset overflow"))?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| GeoError::invalid("Float32 GeoTIFF file lock poisoned"))?;
        let bytes = read_exact_at(&mut file, offset, 4)?;
        Ok(f32::from_le_bytes(
            bytes.try_into().expect("sample read length is 4"),
        ))
    }

    pub fn pixel_x(&self, longitude: f64) -> i32 {
        ((longitude - self.metadata.top_left_longitude) / self.metadata.pixel_width_degrees).floor()
            as i32
    }

    pub fn pixel_y(&self, latitude: f64) -> i32 {
        ((self.metadata.top_left_latitude - latitude) / self.metadata.pixel_height_degrees).floor()
            as i32
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

    fn sample_optional_at_pixel(&self, x: i32, y: i32) -> Result<Option<f64>> {
        let value = f64::from(self.sample_at_pixel(x, y)?);
        if !value.is_finite()
            || self
                .metadata
                .no_data_value
                .is_some_and(|no_data| java_double_compare_equal(value, no_data))
        {
            return Ok(None);
        }
        Ok(Some(value))
    }
}

impl GeoTiffSingleBandReader {
    pub fn open_if_present(
        path: impl AsRef<Path>,
        tile_cache_entries: usize,
    ) -> Result<Option<Self>> {
        let path = path.as_ref();
        if !path.is_file() {
            return Ok(None);
        }
        Self::open_with_tile_cache_entries(path, tile_cache_entries).map(Some)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_tile_cache_entries(path, 256)
    }

    pub fn open_with_tile_cache_entries(
        path: impl AsRef<Path>,
        tile_cache_entries: usize,
    ) -> Result<Self> {
        let path = path.as_ref();
        let mut file = File::open(path).map_err(|error| GeoError::invalid(error.to_string()))?;
        parse_single_band_tiff(path, &mut file, tile_cache_entries.max(4))
    }

    pub fn metadata(&self) -> &GeoTiffMetadata {
        &self.metadata
    }

    pub fn sample_nearest(&self, longitude: f64, latitude: f64) -> Result<Option<f64>> {
        let x = self.pixel_x(longitude);
        let y = self.pixel_y(latitude);
        if x < 0 || x >= self.metadata.width || y < 0 || y >= self.metadata.height {
            return Ok(None);
        }
        let value = self.sample_at_pixel(x, y)?;
        if self
            .metadata
            .no_data_value
            .is_some_and(|no_data| java_double_compare_equal(value, no_data))
        {
            return Ok(None);
        }
        Ok(Some(value))
    }

    pub fn sample_at_pixel(&self, x: i32, y: i32) -> Result<f64> {
        if x < 0 || x >= self.metadata.width {
            return Err(GeoError::invalid(format!("x outside raster: {x}")));
        }
        if y < 0 || y >= self.metadata.height {
            return Err(GeoError::invalid(format!("y outside raster: {y}")));
        }
        let block_x = x / SINGLE_BAND_BLOCK_WIDTH;
        let block_y = y / SINGLE_BAND_BLOCK_HEIGHT;
        let block_origin_x = block_x * SINGLE_BAND_BLOCK_WIDTH;
        let block_origin_y = block_y * SINGLE_BAND_BLOCK_HEIGHT;
        let block_width = SINGLE_BAND_BLOCK_WIDTH.min(self.metadata.width - block_origin_x);
        let block_height = SINGLE_BAND_BLOCK_HEIGHT.min(self.metadata.height - block_origin_y);
        let block = self.block(block_x, block_y, block_width, block_height)?;
        let local_x = usize::try_from(x - block_origin_x)
            .map_err(|_| GeoError::invalid("single-band local x outside block"))?;
        let local_y = usize::try_from(y - block_origin_y)
            .map_err(|_| GeoError::invalid("single-band local y outside block"))?;
        let offset = local_y
            .checked_mul(usize::try_from(block_width).expect("positive block width fits usize"))
            .and_then(|value| value.checked_add(local_x))
            .and_then(|value| value.checked_mul(self.bytes_per_sample))
            .ok_or_else(|| GeoError::invalid("single-band block sample offset overflow"))?;
        match self.bytes_per_sample {
            1 => Ok(f64::from(
                *checked_slice(&block, offset, 1)?.first().expect("one byte"),
            )),
            2 => Ok(f64::from(read_u16_order(&block, offset, self.byte_order)?)),
            _ => Err(GeoError::invalid(format!(
                "unsupported single-band sample byte width: {}",
                self.bytes_per_sample
            ))),
        }
    }

    pub fn pixel_x(&self, longitude: f64) -> i32 {
        ((longitude - self.metadata.top_left_longitude) / self.metadata.pixel_width_degrees).floor()
            as i32
    }

    pub fn pixel_y(&self, latitude: f64) -> i32 {
        ((self.metadata.top_left_latitude - latitude) / self.metadata.pixel_height_degrees).floor()
            as i32
    }

    fn block(
        &self,
        block_x: i32,
        block_y: i32,
        block_width: i32,
        block_height: i32,
    ) -> Result<Arc<[u8]>> {
        let blocks_across = ceil_div_i32(self.metadata.width, SINGLE_BAND_BLOCK_WIDTH)?;
        let block_index = block_y
            .checked_mul(blocks_across)
            .and_then(|value| value.checked_add(block_x))
            .ok_or_else(|| GeoError::invalid("single-band block index overflow"))?;
        let block_index = usize::try_from(block_index)
            .map_err(|_| GeoError::invalid("single-band block index outside raster"))?;
        let mut state = self
            .tile_state
            .lock()
            .map_err(|_| GeoError::invalid("single-band tile cache lock poisoned"))?;
        if state.tiles.contains_key(&block_index) {
            state.tile_hits += 1;
            let stamp = touch_single_band_tile_state(&mut state);
            let tile = state
                .tiles
                .get_mut(&block_index)
                .expect("block was just checked");
            tile.access_stamp = stamp;
            return Ok(Arc::clone(&tile.bytes));
        }
        state.tile_misses += 1;
        let bytes = Arc::<[u8]>::from(
            self.read_sample_block(
                block_x * SINGLE_BAND_BLOCK_WIDTH,
                block_y * SINGLE_BAND_BLOCK_HEIGHT,
                block_width,
                block_height,
            )?
            .into_boxed_slice(),
        );
        let stamp = touch_single_band_tile_state(&mut state);
        state.tiles.insert(
            block_index,
            CachedTile {
                bytes: Arc::clone(&bytes),
                access_stamp: stamp,
            },
        );
        trim_single_band_tile_cache_locked(&mut state, self.tile_cache_entries);
        Ok(bytes)
    }

    fn read_sample_block(
        &self,
        origin_x: i32,
        origin_y: i32,
        width: i32,
        height: i32,
    ) -> Result<Vec<u8>> {
        let width_usize = usize::try_from(width)
            .map_err(|_| GeoError::invalid("single-band block width outside usize"))?;
        let height_usize = usize::try_from(height)
            .map_err(|_| GeoError::invalid("single-band block height outside usize"))?;
        let block_len = width_usize
            .checked_mul(height_usize)
            .and_then(|samples| samples.checked_mul(self.bytes_per_sample))
            .ok_or_else(|| GeoError::invalid("single-band block byte count overflow"))?;
        let mut block = vec![0u8; block_len];
        let mut file = self
            .file
            .lock()
            .map_err(|_| GeoError::invalid("single-band TIFF file lock poisoned"))?;
        let mut decoded_tiles: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        for local_row in 0..height {
            let pixel_y = origin_y + local_row;
            let mut pixel_x = origin_x;
            while pixel_x < origin_x + width {
                let tile_x = pixel_x / self.tile_width;
                let tile_y = pixel_y / self.tile_length;
                let tile_index = tile_y
                    .checked_mul(self.tiles_across)
                    .and_then(|value| value.checked_add(tile_x))
                    .ok_or_else(|| GeoError::invalid("single-band tile index overflow"))?;
                let tile_index = usize::try_from(tile_index)
                    .map_err(|_| GeoError::invalid("single-band tile index outside raster"))?;
                if tile_index >= self.tile_offsets.len() {
                    return Err(GeoError::invalid(format!(
                        "single-band tile outside raster: {tile_index}"
                    )));
                }
                let tile_origin_x = tile_x * self.tile_width;
                let tile_origin_y = tile_y * self.tile_length;
                let actual_tile_width = self.tile_width.min(self.metadata.width - tile_origin_x);
                let actual_tile_height = self.tile_length.min(self.metadata.height - tile_origin_y);
                let local_tile_x = pixel_x - tile_origin_x;
                let local_tile_y = pixel_y - tile_origin_y;
                let segment_width =
                    (origin_x + width - pixel_x).min(actual_tile_width - local_tile_x);
                let segment_bytes = usize::try_from(segment_width)
                    .ok()
                    .and_then(|segment_width| segment_width.checked_mul(self.bytes_per_sample))
                    .ok_or_else(|| GeoError::invalid("single-band segment byte count overflow"))?;
                let destination_offset = usize::try_from(local_row)
                    .expect("validated local row fits usize")
                    .checked_mul(width_usize)
                    .and_then(|value| {
                        value.checked_add(
                            usize::try_from(pixel_x - origin_x)
                                .expect("validated local block x fits usize"),
                        )
                    })
                    .and_then(|value| value.checked_mul(self.bytes_per_sample))
                    .ok_or_else(|| {
                        GeoError::invalid("single-band block destination offset overflow")
                    })?;
                if self.metadata.compression != TIFF_COMPRESSION_NONE {
                    if !decoded_tiles.contains_key(&tile_index) {
                        let decoded = self.read_decoded_single_band_tile(
                            &mut file,
                            tile_index,
                            actual_tile_width,
                            actual_tile_height,
                        )?;
                        decoded_tiles.insert(tile_index, decoded);
                    }
                    let tile = decoded_tiles
                        .get(&tile_index)
                        .expect("decoded tile was inserted");
                    let row_stride = single_band_decoded_tile_row_stride(
                        tile.len(),
                        self.tile_width,
                        self.tile_length,
                        actual_tile_width,
                        actual_tile_height,
                        self.bytes_per_sample,
                    )?;
                    let source_offset = usize::try_from(local_tile_y)
                        .expect("validated local tile y is non-negative")
                        .checked_mul(row_stride)
                        .and_then(|value| {
                            value.checked_add(
                                usize::try_from(local_tile_x)
                                    .expect("validated local tile x is non-negative")
                                    .checked_mul(self.bytes_per_sample)?,
                            )
                        })
                        .ok_or_else(|| {
                            GeoError::invalid("single-band decoded tile offset overflow")
                        })?;
                    let bytes = checked_slice(tile, source_offset, segment_bytes)?;
                    block[destination_offset..destination_offset + segment_bytes]
                        .copy_from_slice(bytes);
                } else {
                    let row_stride = self.single_band_tile_row_stride(
                        tile_index,
                        actual_tile_width,
                        actual_tile_height,
                    )?;
                    let source_offset = self.tile_offsets[tile_index]
                        .checked_add(
                            u64::try_from(local_tile_y)
                                .expect("validated local tile y is non-negative")
                                .checked_mul(
                                    u64::try_from(row_stride).expect("row stride fits u64"),
                                )
                                .and_then(|value| {
                                    value.checked_add(
                                        u64::try_from(local_tile_x)
                                            .expect("validated local tile x is non-negative")
                                            .checked_mul(
                                                u64::try_from(self.bytes_per_sample)
                                                    .expect("sample size fits u64"),
                                            )?,
                                    )
                                })
                                .ok_or_else(|| {
                                    GeoError::invalid("single-band tile read offset overflow")
                                })?,
                        )
                        .ok_or_else(|| {
                            GeoError::invalid("single-band tile file offset overflow")
                        })?;
                    let bytes = read_exact_at(&mut file, source_offset, segment_bytes)?;
                    block[destination_offset..destination_offset + segment_bytes]
                        .copy_from_slice(&bytes);
                }
                pixel_x += segment_width;
            }
        }
        Ok(block)
    }

    fn read_decoded_single_band_tile(
        &self,
        file: &mut File,
        tile_index: usize,
        actual_tile_width: i32,
        actual_tile_height: i32,
    ) -> Result<Vec<u8>> {
        let raw = read_exact_at(
            file,
            self.tile_offsets[tile_index],
            self.tile_byte_counts[tile_index],
        )?;
        let expected_size =
            single_band_full_tile_size(self.tile_width, self.tile_length, self.bytes_per_sample)?;
        let decoded = match self.metadata.compression {
            TIFF_COMPRESSION_LZW => decode_lzw_single_band_tile(&raw, tile_index, expected_size)?,
            TIFF_COMPRESSION_PACKBITS => {
                decode_packbits_single_band_tile(&raw, tile_index, expected_size)?
            }
            compression => {
                return Err(GeoError::invalid(format!(
                    "unsupported compressed single-band TIFF tile compression: {compression}"
                )))
            }
        };
        single_band_decoded_tile_row_stride(
            decoded.len(),
            self.tile_width,
            self.tile_length,
            actual_tile_width,
            actual_tile_height,
            self.bytes_per_sample,
        )?;
        Ok(decoded)
    }

    fn single_band_tile_row_stride(
        &self,
        tile_index: usize,
        actual_tile_width: i32,
        actual_tile_height: i32,
    ) -> Result<usize> {
        let full_stride = usize::try_from(self.tile_width)
            .ok()
            .and_then(|width| width.checked_mul(self.bytes_per_sample))
            .ok_or_else(|| GeoError::invalid("single-band full stride overflow"))?;
        let cropped_stride = usize::try_from(actual_tile_width)
            .ok()
            .and_then(|width| width.checked_mul(self.bytes_per_sample))
            .ok_or_else(|| GeoError::invalid("single-band cropped stride overflow"))?;
        let cropped_size = cropped_stride
            .checked_mul(
                usize::try_from(actual_tile_height)
                    .map_err(|_| GeoError::invalid("single-band cropped height outside usize"))?,
            )
            .ok_or_else(|| GeoError::invalid("single-band cropped tile size overflow"))?;
        if self.tile_byte_counts[tile_index] == cropped_size {
            Ok(cropped_stride)
        } else {
            Ok(full_stride)
        }
    }
}

impl GeoTiffRgbReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_tile_cache_entries(path, 256)
    }

    pub fn open_with_tile_cache_entries(
        path: impl AsRef<Path>,
        tile_cache_entries: usize,
    ) -> Result<Self> {
        let path = path.as_ref();
        let mut file = File::open(path).map_err(|error| GeoError::invalid(error.to_string()))?;
        parse_rgb_tiff(&mut file, tile_cache_entries.max(1))
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn sample_pixel(&self, x: i32, y: i32) -> Result<RgbColor> {
        let packed = self.sample_pixel_packed(x, y)?;
        if packed < 0 {
            return Ok(RgbColor::unavailable());
        }
        Ok(RgbColor::of(
            ((packed >> 16) & 0xff) as u8,
            ((packed >> 8) & 0xff) as u8,
            (packed & 0xff) as u8,
        ))
    }

    pub fn sample_pixel_packed(&self, x: i32, y: i32) -> Result<i32> {
        if x < 0 || x >= self.width || y < 0 || y >= self.height {
            return Ok(-1);
        }
        let tile_x = x / self.tile_width;
        let tile_y = y / self.tile_length;
        let tile_index = tile_y
            .checked_mul(self.tiles_across)
            .and_then(|value| value.checked_add(tile_x))
            .ok_or_else(|| GeoError::invalid("RGB tile index overflow"))?;
        if tile_index < 0 || tile_index as usize >= self.tile_offsets.len() {
            return Ok(-1);
        }
        let tile = self.tile(tile_index as usize)?;
        let actual_width = self.tile_width.min(self.width - (tile_x * self.tile_width));
        let actual_height = self
            .tile_length
            .min(self.height - (tile_y * self.tile_length));
        let full_stride = usize::try_from(self.tile_width)
            .ok()
            .and_then(|width| width.checked_mul(self.samples_per_pixel))
            .ok_or_else(|| GeoError::invalid("RGB full stride overflow"))?;
        let cropped_stride = usize::try_from(actual_width)
            .ok()
            .and_then(|width| width.checked_mul(self.samples_per_pixel))
            .ok_or_else(|| GeoError::invalid("RGB cropped stride overflow"))?;
        let cropped_size = cropped_stride
            .checked_mul(usize::try_from(actual_height).unwrap_or(0))
            .ok_or_else(|| GeoError::invalid("RGB cropped tile size overflow"))?;
        let row_stride = if tile.len() == cropped_size {
            cropped_stride
        } else {
            full_stride
        };
        let local_x = usize::try_from(x - (tile_x * self.tile_width))
            .map_err(|_| GeoError::invalid("RGB local x outside tile"))?;
        let local_y = usize::try_from(y - (tile_y * self.tile_length))
            .map_err(|_| GeoError::invalid("RGB local y outside tile"))?;
        let offset = local_y
            .checked_mul(row_stride)
            .and_then(|value| value.checked_add(local_x.checked_mul(self.samples_per_pixel)?))
            .ok_or_else(|| GeoError::invalid("RGB tile sample offset overflow"))?;
        if offset.checked_add(2).is_none_or(|end| end >= tile.len()) {
            return Ok(-1);
        }
        Ok(((i32::from(tile[offset])) << 16)
            | ((i32::from(tile[offset + 1])) << 8)
            | i32::from(tile[offset + 2]))
    }

    pub fn stats(&self) -> GeoTiffRgbReaderStats {
        let state = self
            .tile_state
            .lock()
            .expect("RGB tile cache lock not poisoned");
        GeoTiffRgbReaderStats {
            max_tile_cache_entries: self.tile_cache_entries,
            resident_tiles: state.tiles.len(),
            tile_hits: state.tile_hits,
            tile_misses: state.tile_misses,
            tile_evictions: state.tile_evictions,
        }
    }

    fn tile(&self, tile_index: usize) -> Result<Arc<[u8]>> {
        let mut state = self
            .tile_state
            .lock()
            .map_err(|_| GeoError::invalid("RGB tile cache lock poisoned"))?;
        if state.tiles.contains_key(&tile_index) {
            state.tile_hits += 1;
            let stamp = touch_rgb_tile_state(&mut state);
            let tile = state
                .tiles
                .get_mut(&tile_index)
                .expect("tile was just checked");
            tile.access_stamp = stamp;
            return Ok(Arc::clone(&tile.bytes));
        }
        state.tile_misses += 1;
        let offset = self.tile_offsets[tile_index];
        let byte_count = self.tile_byte_counts[tile_index];
        let mut file = self
            .file
            .lock()
            .map_err(|_| GeoError::invalid("RGB TIFF file lock poisoned"))?;
        let bytes =
            Arc::<[u8]>::from(read_exact_at(&mut file, offset, byte_count)?.into_boxed_slice());
        let stamp = touch_rgb_tile_state(&mut state);
        state.tiles.insert(
            tile_index,
            CachedTile {
                bytes: Arc::clone(&bytes),
                access_stamp: stamp,
            },
        );
        trim_rgb_tile_cache_locked(&mut state, self.tile_cache_entries);
        Ok(bytes)
    }
}

impl VrtRgbMosaicReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_tile_cache_entries(path, DEFAULT_RGB_TILE_CACHE_ENTRIES)
    }

    pub fn open_with_tile_cache_entries(
        path: impl AsRef<Path>,
        tile_cache_entries: usize,
    ) -> Result<Self> {
        if tile_cache_entries == 0 {
            return Err(GeoError::invalid("tileCacheEntries must be positive"));
        }
        let vrt_path = absolute_existing_path(path.as_ref())?;
        let parsed = parse_vrt_rgb_mosaic(&vrt_path)?;
        if parsed.sources.is_empty() {
            return Err(GeoError::invalid(format!(
                "VRT has no RGB sources: {}",
                vrt_path.display()
            )));
        }
        let source_lookup = VrtSourceLookup::create(
            parsed.sources.clone(),
            parsed.raster_width,
            parsed.raster_height,
        );
        Ok(Self {
            vrt_path,
            raster_width: parsed.raster_width,
            raster_height: parsed.raster_height,
            origin_x: parsed.origin_x,
            pixel_width: parsed.pixel_width,
            origin_y: parsed.origin_y,
            pixel_height: parsed.pixel_height,
            sources: parsed.sources,
            source_lookup,
            tile_cache_entries,
            readers: Mutex::new(HashMap::new()),
            sample_nearest_requests: AtomicU64::new(0),
            sample_averaged_requests: AtomicU64::new(0),
        })
    }

    pub fn sample_nearest(&self, longitude: f64, latitude: f64) -> Result<RgbColor> {
        self.sample_nearest_requests.fetch_add(1, Ordering::Relaxed);
        self.sample_nearest_internal(longitude, latitude)
    }

    pub fn sample_averaged(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<RgbColor> {
        self.sample_averaged_requests
            .fetch_add(1, Ordering::Relaxed);
        let lon_offset = longitude_span_degrees.abs().max(self.pixel_width.abs()) / 3.0;
        let lat_offset = latitude_span_degrees.abs().max(self.pixel_height.abs()) / 3.0;
        let mut red = 0u64;
        let mut green = 0u64;
        let mut blue = 0u64;
        let mut samples = 0u64;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let sample_longitude = longitude + (f64::from(dx) * lon_offset);
                let sample_latitude = latitude + (f64::from(dz) * lat_offset);
                if !(-180.0..180.0).contains(&sample_longitude)
                    || !(-90.0..=90.0).contains(&sample_latitude)
                {
                    continue;
                }
                let packed =
                    self.sample_nearest_packed_internal(sample_longitude, sample_latitude)?;
                if packed < 0 {
                    continue;
                }
                red += ((packed >> 16) & 0xff) as u64;
                green += ((packed >> 8) & 0xff) as u64;
                blue += (packed & 0xff) as u64;
                samples += 1;
            }
        }
        if samples == 0 {
            return Ok(RgbColor::unavailable());
        }
        Ok(RgbColor::of(
            java_round(red as f64 / samples as f64) as u8,
            java_round(green as f64 / samples as f64) as u8,
            java_round(blue as f64 / samples as f64) as u8,
        ))
    }

    pub fn stats(&self) -> VrtRgbMosaicStats {
        let readers = self
            .readers
            .lock()
            .expect("VRT reader cache lock not poisoned");
        let mut resident_tiles = 0u64;
        let mut tile_hits = 0u64;
        let mut tile_misses = 0u64;
        let mut tile_evictions = 0u64;
        for reader in readers.values() {
            let stats = reader.stats();
            resident_tiles += stats.resident_tiles as u64;
            tile_hits += stats.tile_hits;
            tile_misses += stats.tile_misses;
            tile_evictions += stats.tile_evictions;
        }
        VrtRgbMosaicStats {
            source_count: self.sources.len(),
            open_readers: readers.len(),
            resident_tiles,
            tile_hits,
            tile_misses,
            tile_evictions,
            sample_nearest_requests: self.sample_nearest_requests.load(Ordering::Relaxed),
            sample_averaged_requests: self.sample_averaged_requests.load(Ordering::Relaxed),
            indexed_source_lookup: self.source_lookup.indexed(),
            source_lookup_cells: self.source_lookup.cells(),
        }
    }

    pub fn raster_width(&self) -> i32 {
        self.raster_width
    }

    pub fn raster_height(&self) -> i32 {
        self.raster_height
    }

    pub fn path(&self) -> &Path {
        &self.vrt_path
    }

    fn sample_nearest_internal(&self, longitude: f64, latitude: f64) -> Result<RgbColor> {
        let pixel_x = ((longitude - self.origin_x) / self.pixel_width).floor() as i32;
        let pixel_y = ((latitude - self.origin_y) / self.pixel_height).floor() as i32;
        self.sample_pixel(pixel_x, pixel_y)
    }

    fn sample_nearest_packed_internal(&self, longitude: f64, latitude: f64) -> Result<i32> {
        let pixel_x = ((longitude - self.origin_x) / self.pixel_width).floor() as i32;
        let pixel_y = ((latitude - self.origin_y) / self.pixel_height).floor() as i32;
        self.sample_pixel_packed(pixel_x, pixel_y)
    }

    fn sample_pixel(&self, pixel_x: i32, pixel_y: i32) -> Result<RgbColor> {
        let packed = self.sample_pixel_packed(pixel_x, pixel_y)?;
        if packed < 0 {
            return Ok(RgbColor::unavailable());
        }
        Ok(RgbColor::of(
            ((packed >> 16) & 0xff) as u8,
            ((packed >> 8) & 0xff) as u8,
            (packed & 0xff) as u8,
        ))
    }

    fn sample_pixel_packed(&self, pixel_x: i32, pixel_y: i32) -> Result<i32> {
        if pixel_x < 0
            || pixel_x >= self.raster_width
            || pixel_y < 0
            || pixel_y >= self.raster_height
        {
            return Ok(-1);
        }
        let Some(source_index) = self.source_lookup.source_at(pixel_x, pixel_y) else {
            return Ok(-1);
        };
        let source = &self.sources[source_index];
        let source_x = source.source_x(pixel_x);
        let source_y = source.source_y(pixel_y);
        let reader = {
            let reader_key = VrtReaderKey {
                path: source.path.clone(),
            };
            let mut readers = self
                .readers
                .lock()
                .map_err(|_| GeoError::invalid("VRT reader cache lock poisoned"))?;
            if !readers.contains_key(&reader_key) {
                let tile_cache_entries =
                    vrt_reader_tile_cache_entries(self.tile_cache_entries, self.sources.len());
                readers.insert(
                    reader_key.clone(),
                    Arc::new(GeoTiffRgbReader::open_with_tile_cache_entries(
                        &source.path,
                        tile_cache_entries,
                    )?),
                );
            }
            Arc::clone(
                readers
                    .get(&reader_key)
                    .expect("reader was inserted or already present"),
            )
        };
        reader.sample_pixel_packed(source_x, source_y)
    }
}

fn vrt_reader_tile_cache_entries(total_tile_cache_entries: usize, source_count: usize) -> usize {
    total_tile_cache_entries
        .max(1)
        .div_ceil(source_count.max(1))
        .max(1)
}

impl VrtSource {
    fn contains(&self, pixel_x: i32, pixel_y: i32) -> bool {
        pixel_x >= self.dst.x
            && pixel_x < self.dst.x.saturating_add(self.dst.width)
            && pixel_y >= self.dst.y
            && pixel_y < self.dst.y.saturating_add(self.dst.height)
    }

    fn source_x(&self, pixel_x: i32) -> i32 {
        let fraction = f64::from(pixel_x - self.dst.x) / f64::from(self.dst.width);
        self.src.x + (self.src.width - 1).min((fraction * f64::from(self.src.width)).floor() as i32)
    }

    fn source_y(&self, pixel_y: i32) -> i32 {
        let fraction = f64::from(pixel_y - self.dst.y) / f64::from(self.dst.height);
        self.src.y
            + (self.src.height - 1).min((fraction * f64::from(self.src.height)).floor() as i32)
    }
}

impl VrtSourceLookup {
    fn create(sources: Vec<VrtSource>, raster_width: i32, raster_height: i32) -> Self {
        Self::try_create_grid(sources.clone(), raster_width, raster_height)
            .unwrap_or(Self::Linear { sources })
    }

    fn source_at(&self, pixel_x: i32, pixel_y: i32) -> Option<usize> {
        match self {
            Self::Linear { sources } => sources
                .iter()
                .enumerate()
                .find_map(|(index, source)| source.contains(pixel_x, pixel_y).then_some(index)),
            Self::Grid {
                cell_width,
                cell_height,
                columns,
                grid,
                sources,
            } => {
                let column = pixel_x / *cell_width;
                let row = pixel_y / *cell_height;
                let index = row
                    .checked_mul(*columns)
                    .and_then(|value| value.checked_add(column))?;
                let source_index = *grid.get(usize::try_from(index).ok()?)?;
                let source = &sources[source_index?];
                source.contains(pixel_x, pixel_y).then_some(source_index?)
            }
        }
    }

    fn indexed(&self) -> bool {
        matches!(self, Self::Grid { .. })
    }

    fn cells(&self) -> usize {
        match self {
            Self::Linear { sources } => sources.len(),
            Self::Grid { grid, .. } => grid.len(),
        }
    }

    fn try_create_grid(
        sources: Vec<VrtSource>,
        raster_width: i32,
        raster_height: i32,
    ) -> Option<Self> {
        let first = sources.first()?;
        let cell_width = first.dst.width;
        let cell_height = first.dst.height;
        if cell_width <= 0
            || cell_height <= 0
            || raster_width % cell_width != 0
            || raster_height % cell_height != 0
        {
            return None;
        }
        let columns = raster_width / cell_width;
        let rows = raster_height / cell_height;
        let cell_count = usize::try_from(columns.checked_mul(rows)?).ok()?;
        let mut grid = vec![None; cell_count];
        for (source_index, source) in sources.iter().enumerate() {
            let dst = source.dst;
            if dst.width != cell_width
                || dst.height != cell_height
                || dst.x < 0
                || dst.y < 0
                || dst.x + dst.width > raster_width
                || dst.y + dst.height > raster_height
                || dst.x % cell_width != 0
                || dst.y % cell_height != 0
            {
                return None;
            }
            let column = dst.x / cell_width;
            let row = dst.y / cell_height;
            let index = usize::try_from((row * columns) + column).ok()?;
            if grid[index].is_some() {
                return None;
            }
            grid[index] = Some(source_index);
        }
        Some(Self::Grid {
            cell_width,
            cell_height,
            columns,
            grid,
            sources,
        })
    }
}

#[derive(Debug)]
struct ParsedVrtRgbMosaic {
    raster_width: i32,
    raster_height: i32,
    origin_x: f64,
    pixel_width: f64,
    origin_y: f64,
    pixel_height: f64,
    sources: Vec<VrtSource>,
}

#[derive(Debug, Default)]
struct ParsedSimpleSource {
    filename: Option<ParsedSourceFilename>,
    src: Option<VrtRect>,
    dst: Option<VrtRect>,
}

#[derive(Debug)]
struct ParsedSourceFilename {
    text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VrtTextCapture {
    GeoTransform,
    SourceFilename,
}

fn parse_vrt_rgb_mosaic(vrt_path: &Path) -> Result<ParsedVrtRgbMosaic> {
    let xml = fs::read_to_string(vrt_path).map_err(|error| GeoError::invalid(error.to_string()))?;
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut stack = Vec::<Vec<u8>>::new();
    let mut root_seen = false;
    let mut raster_width = None;
    let mut raster_height = None;
    let mut geo_transform = None;
    let mut saw_band_1 = false;
    let mut red_band_depth = None::<usize>;
    let mut current_simple_source = None::<ParsedSimpleSource>;
    let mut capture = None::<VrtTextCapture>;
    let mut capture_text = String::new();
    let mut sources = Vec::new();
    let base_dir = vrt_path.parent().unwrap_or_else(|| Path::new("."));

    loop {
        match reader
            .read_event()
            .map_err(|error| GeoError::invalid(format!("failed to parse VRT: {error}")))?
        {
            Event::Start(element) => {
                let name = element.name().as_ref().to_vec();
                let depth = stack.len();
                if !root_seen {
                    if name.as_slice() != b"VRTDataset" {
                        return Err(GeoError::invalid(format!(
                            "not a VRTDataset: {}",
                            vrt_path.display()
                        )));
                    }
                    raster_width =
                        Some(parse_vrt_i32_attr(&element, b"rasterXSize", "rasterXSize")?);
                    raster_height =
                        Some(parse_vrt_i32_attr(&element, b"rasterYSize", "rasterYSize")?);
                    root_seen = true;
                }

                if name.as_slice() == b"VRTRasterBand"
                    && attr_string(&element, b"band")?.as_deref() == Some("1")
                {
                    saw_band_1 = true;
                    red_band_depth = Some(depth);
                }
                if name.as_slice() == b"GeoTransform"
                    && stack.last().is_some_and(|n| n == b"VRTDataset")
                {
                    capture = Some(VrtTextCapture::GeoTransform);
                    capture_text.clear();
                }
                if red_band_depth.is_some() && name.as_slice() == b"SimpleSource" {
                    current_simple_source = Some(ParsedSimpleSource::default());
                }
                if let Some(simple_source) = current_simple_source.as_mut() {
                    match name.as_slice() {
                        b"SourceFilename" => {
                            capture = Some(VrtTextCapture::SourceFilename);
                            capture_text.clear();
                        }
                        b"SrcRect" => simple_source.src = Some(parse_vrt_rect(&element)?),
                        b"DstRect" => simple_source.dst = Some(parse_vrt_rect(&element)?),
                        _ => {}
                    }
                }
                stack.push(name);
            }
            Event::Empty(element) => {
                let name = element.name().as_ref().to_vec();
                if let Some(simple_source) = current_simple_source.as_mut() {
                    match name.as_slice() {
                        b"SrcRect" => simple_source.src = Some(parse_vrt_rect(&element)?),
                        b"DstRect" => simple_source.dst = Some(parse_vrt_rect(&element)?),
                        b"SourceFilename" => {
                            simple_source.filename = Some(ParsedSourceFilename {
                                text: String::new(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            Event::Text(text) => {
                if capture.is_some() {
                    capture_text.push_str(&decoded_xml_text(&text)?);
                }
            }
            Event::CData(text) => {
                if capture.is_some() {
                    capture_text.push_str(&decoded_cdata_text(&text)?);
                }
            }
            Event::GeneralRef(reference) => {
                if capture.is_some() {
                    capture_text.push_str(&decoded_general_ref(&reference)?);
                }
            }
            Event::End(end) => {
                let name = end.name().as_ref().to_vec();
                if name.as_slice() == b"GeoTransform"
                    && capture == Some(VrtTextCapture::GeoTransform)
                {
                    geo_transform = Some(capture_text.trim().to_string());
                    capture = None;
                    capture_text.clear();
                } else if name.as_slice() == b"SourceFilename" {
                    if capture == Some(VrtTextCapture::SourceFilename) {
                        if let Some(simple_source) = current_simple_source.as_mut() {
                            simple_source.filename = Some(ParsedSourceFilename {
                                text: capture_text.trim().to_string(),
                            });
                        }
                        capture = None;
                        capture_text.clear();
                    }
                } else if name.as_slice() == b"SimpleSource" {
                    if let Some(simple_source) = current_simple_source.take() {
                        if let Some(source) = finish_vrt_simple_source(base_dir, simple_source)? {
                            sources.push(source);
                        }
                    }
                }

                let depth = stack.len().saturating_sub(1);
                if red_band_depth == Some(depth) && name.as_slice() == b"VRTRasterBand" {
                    red_band_depth = None;
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if !root_seen {
        return Err(GeoError::invalid(format!(
            "not a VRTDataset: {}",
            vrt_path.display()
        )));
    }
    let raster_width =
        raster_width.ok_or_else(|| GeoError::invalid("missing integer attribute: rasterXSize"))?;
    let raster_height =
        raster_height.ok_or_else(|| GeoError::invalid("missing integer attribute: rasterYSize"))?;
    let transform = parse_vrt_geo_transform(geo_transform.as_deref(), vrt_path)?;
    if !saw_band_1 {
        return Err(GeoError::invalid(format!(
            "VRT is missing band 1: {}",
            vrt_path.display()
        )));
    }
    add_true_marble_grid_fallback_sources(&mut sources, base_dir, raster_width, raster_height);
    Ok(ParsedVrtRgbMosaic {
        raster_width,
        raster_height,
        origin_x: transform[0],
        pixel_width: transform[1],
        origin_y: transform[3],
        pixel_height: transform[5],
        sources,
    })
}

fn finish_vrt_simple_source(
    base_dir: &Path,
    simple_source: ParsedSimpleSource,
) -> Result<Option<VrtSource>> {
    let (Some(filename), Some(src), Some(dst)) =
        (simple_source.filename, simple_source.src, simple_source.dst)
    else {
        return Ok(None);
    };
    let source_path = vrt_source_path(base_dir, &filename)?;
    if !source_path.is_file() {
        return Err(GeoError::invalid(format!(
            "VRT source is missing: {}",
            source_path.display()
        )));
    }
    Ok(Some(VrtSource {
        path: absolute_existing_path(&source_path)?,
        src,
        dst,
    }))
}

fn vrt_source_path(base_dir: &Path, filename: &ParsedSourceFilename) -> Result<PathBuf> {
    let raw = PathBuf::from(filename.text.trim());
    let path = if raw.is_absolute() {
        raw
    } else {
        base_dir.join(raw)
    };
    Ok(path)
}

fn add_true_marble_grid_fallback_sources(
    sources: &mut Vec<VrtSource>,
    base_dir: &Path,
    raster_width: i32,
    raster_height: i32,
) {
    if raster_width % 8 != 0 || raster_height % 4 != 0 {
        return;
    }
    let tile_width = raster_width / 8;
    let tile_height = raster_height / 4;
    for column in 0..8 {
        for row in 0..4 {
            let dst = VrtRect {
                x: column * tile_width,
                y: row * tile_height,
                width: tile_width,
                height: tile_height,
            };
            if has_destination(sources, dst) {
                continue;
            }
            let tile_name = format!(
                "TrueMarble.250m.21600x21600.{}{}.tif",
                (b'A' + column as u8) as char,
                row + 1
            );
            if let Some(source_path) = first_valid_true_marble_tile(base_dir, &tile_name) {
                sources.push(VrtSource {
                    path: source_path,
                    src: VrtRect {
                        x: 0,
                        y: 0,
                        width: tile_width,
                        height: tile_height,
                    },
                    dst,
                });
            }
        }
    }
}

fn has_destination(sources: &[VrtSource], dst: VrtRect) -> bool {
    sources.iter().any(|source| source.dst == dst)
}

fn first_valid_true_marble_tile(base_dir: &Path, tile_name: &str) -> Option<PathBuf> {
    [base_dir.join(tile_name)]
        .into_iter()
        .filter_map(|candidate| absolute_existing_path(&candidate).ok())
        .find(|candidate| {
            candidate.is_file()
                && GeoTiffRgbReader::open_with_tile_cache_entries(candidate, 1).is_ok()
        })
}

fn parse_vrt_geo_transform(text: Option<&str>, path: &Path) -> Result<[f64; 6]> {
    let Some(text) = text else {
        return Err(GeoError::invalid(format!(
            "VRT is missing GeoTransform: {}",
            path.display()
        )));
    };
    if text.trim().is_empty() {
        return Err(GeoError::invalid(format!(
            "VRT is missing GeoTransform: {}",
            path.display()
        )));
    }
    let parts = text.split(',').collect::<Vec<_>>();
    if parts.len() != 6 {
        return Err(GeoError::invalid(format!(
            "VRT GeoTransform must have 6 values: {}",
            path.display()
        )));
    }
    let mut values = [0.0f64; 6];
    for (index, part) in parts.iter().enumerate() {
        values[index] = parse_java_double(part.trim()).map_err(GeoError::invalid)?;
    }
    if values[2] != 0.0 || values[4] != 0.0 || values[1] == 0.0 || values[5] == 0.0 {
        return Err(GeoError::invalid(format!(
            "rotated or degenerate VRT GeoTransform is not supported: {}",
            path.display()
        )));
    }
    Ok(values)
}

fn parse_vrt_rect(element: &BytesStart<'_>) -> Result<VrtRect> {
    Ok(VrtRect {
        x: parse_vrt_i32_attr(element, b"xOff", "xOff")?,
        y: parse_vrt_i32_attr(element, b"yOff", "yOff")?,
        width: parse_vrt_i32_attr(element, b"xSize", "xSize")?,
        height: parse_vrt_i32_attr(element, b"ySize", "ySize")?,
    })
}

fn parse_vrt_i32_attr(element: &BytesStart<'_>, key: &[u8], name: &str) -> Result<i32> {
    let Some(value) = attr_string(element, key)? else {
        return Err(GeoError::invalid(format!(
            "missing integer attribute: {name}"
        )));
    };
    if value.trim().is_empty() {
        return Err(GeoError::invalid(format!(
            "missing integer attribute: {name}"
        )));
    }
    value
        .trim()
        .parse::<i32>()
        .map_err(|error| GeoError::invalid(error.to_string()))
}

fn decoded_xml_text(text: &BytesText<'_>) -> Result<String> {
    Ok(text
        .decode()
        .map_err(|error| GeoError::invalid(error.to_string()))?
        .into_owned())
}

fn decoded_cdata_text(text: &BytesCData<'_>) -> Result<String> {
    Ok(text
        .decode()
        .map_err(|error| GeoError::invalid(error.to_string()))?
        .into_owned())
}

fn decoded_general_ref(reference: &BytesRef<'_>) -> Result<String> {
    let name = reference
        .decode()
        .map_err(|error| GeoError::invalid(error.to_string()))?;
    let value = match name.as_ref() {
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "quot" => "\"".to_string(),
        "apos" => "'".to_string(),
        text if text.starts_with("#x") || text.starts_with("#X") => {
            let codepoint = u32::from_str_radix(&text[2..], 16)
                .map_err(|_| GeoError::invalid(format!("unrecognized XML entity: {text}")))?;
            char::from_u32(codepoint)
                .ok_or_else(|| GeoError::invalid(format!("unrecognized XML entity: {text}")))?
                .to_string()
        }
        text if text.starts_with('#') => {
            let codepoint = text[1..]
                .parse::<u32>()
                .map_err(|_| GeoError::invalid(format!("unrecognized XML entity: {text}")))?;
            char::from_u32(codepoint)
                .ok_or_else(|| GeoError::invalid(format!("unrecognized XML entity: {text}")))?
                .to_string()
        }
        text => {
            return Err(GeoError::invalid(format!(
                "unrecognized XML entity: {text}"
            )))
        }
    };
    Ok(value)
}

fn attr_string(element: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| GeoError::invalid(error.to_string()))?;
        if attribute.key.as_ref() == key {
            return Ok(Some(
                attribute
                    .normalized_value(XmlVersion::Implicit1_0)
                    .map_err(|error| GeoError::invalid(error.to_string()))?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}

fn absolute_existing_path(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).map_err(|error| GeoError::invalid(error.to_string()))
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
        Ok(self.row_arc(y)?.to_vec())
    }

    pub fn row_arc(&self, y: i32) -> Result<Arc<[i16]>> {
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
            let values = Arc::clone(&row.values);
            self.schedule_read_ahead_locked(&mut state, y)?;
            return Ok(values);
        }

        state.misses += 1;
        let loaded = self.load_row(y)?;
        let stamp = touch_state(&mut state);
        state.rows.insert(
            y,
            CachedRow {
                values: Arc::clone(&loaded),
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

    fn load_row(&self, y: i32) -> Result<Arc<[i16]>> {
        let mut row = vec![0i16; self.reader.metadata().width as usize];
        self.reader.read_row(y, &mut row)?;
        Ok(Arc::from(row.into_boxed_slice()))
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
    first_row: Option<Arc<[i16]>>,
    second_row: Option<Arc<[i16]>>,
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

    fn cached_row(&self, y: i32) -> Result<Arc<[i16]>> {
        let mut cached = self.cached_rows.borrow_mut();
        if cached.first_y == Some(y) {
            return Ok(Arc::clone(
                cached
                    .first_row
                    .as_ref()
                    .expect("first row is present when first_y is set"),
            ));
        }
        if cached.second_y == Some(y) {
            return Ok(Arc::clone(
                cached
                    .second_row
                    .as_ref()
                    .expect("second row is present when second_y is set"),
            ));
        }
        let row = self
            .row_cache
            .expect("cached_row is used only when row_cache is set")
            .row_arc(y)?;
        cached.second_y = cached.first_y;
        cached.second_row = cached.first_row.take();
        cached.first_y = Some(y);
        cached.first_row = Some(Arc::clone(&row));
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

fn java_round(value: f64) -> i64 {
    if value.is_nan() {
        0
    } else {
        (value + 0.5).floor() as i64
    }
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

fn touch_rgb_tile_state(state: &mut RgbTileState) -> u64 {
    state.access_clock += 1;
    state.access_clock
}

fn trim_rgb_tile_cache_locked(state: &mut RgbTileState, max_tiles: usize) {
    while state.tiles.len() > max_tiles {
        let Some((&oldest_tile, _)) = state
            .tiles
            .iter()
            .min_by_key(|(_tile_index, tile)| tile.access_stamp)
        else {
            return;
        };
        if state.tiles.remove(&oldest_tile).is_some() {
            state.tile_evictions += 1;
        }
    }
}

fn touch_single_band_tile_state(state: &mut SingleBandTileState) -> u64 {
    state.access_clock += 1;
    state.access_clock
}

fn trim_single_band_tile_cache_locked(state: &mut SingleBandTileState, max_tiles: usize) {
    while state.tiles.len() > max_tiles {
        let Some((&oldest_tile, _)) = state
            .tiles
            .iter()
            .min_by_key(|(_tile_index, tile)| tile.access_stamp)
        else {
            return;
        };
        if state.tiles.remove(&oldest_tile).is_some() {
            state.tile_evictions += 1;
        }
    }
}

fn parse_single_band_tiff(
    path: &Path,
    file: &mut File,
    tile_cache_entries: usize,
) -> Result<GeoTiffSingleBandReader> {
    let header = read_exact_at(file, 0, 16)?;
    let order = tiff_byte_order(&header)?;
    let magic = read_u16_order(&header, 2, order)?;
    let entries = if magic == CLASSIC_TIFF_MAGIC {
        let ifd_offset = u64::from(read_u32_order(&header, 4, order)?);
        read_classic_rgb_ifd(file, order, ifd_offset)?
    } else if magic == BIG_TIFF_MAGIC {
        let offset_size = read_u16_order(&header, 4, order)?;
        let reserved = read_u16_order(&header, 6, order)?;
        if offset_size != 8 || reserved != 0 {
            return Err(GeoError::invalid("unsupported BigTIFF header"));
        }
        let ifd_offset = read_u64_order(&header, 8, order)?;
        read_big_rgb_ifd(file, order, ifd_offset)?
    } else {
        return Err(GeoError::invalid(format!(
            "not a TIFF file or unsupported TIFF magic: {magic}"
        )));
    };

    let width = i32_from_u64(
        required_rgb_first_unsigned(file, &entries, TAG_IMAGE_WIDTH, "ImageWidth")?,
        "width",
    )?;
    let height = i32_from_u64(
        required_rgb_first_unsigned(file, &entries, TAG_IMAGE_LENGTH, "ImageLength")?,
        "height",
    )?;
    let bits = required_rgb_unsigned_array(file, &entries, TAG_BITS_PER_SAMPLE, "BitsPerSample")?;
    let bits_per_sample = i32_from_u64(*bits.first().unwrap_or(&0), "bits per sample")?;
    let compression = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_COMPRESSION, 1)?,
        "compression",
    )?;
    let samples_per_pixel = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_SAMPLES_PER_PIXEL, 1)?,
        "samples per pixel",
    )?;
    let sample_format = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_SAMPLE_FORMAT, 1)?,
        "sample format",
    )?;
    let planar_configuration = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_PLANAR_CONFIGURATION, 1)?,
        "planar configuration",
    )?;
    let predictor = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_PREDICTOR, 1)?,
        "predictor",
    )?;
    let tiled = entries.contains_key(&TAG_TILE_WIDTH)
        && entries.contains_key(&TAG_TILE_LENGTH)
        && entries.contains_key(&TAG_TILE_OFFSETS)
        && entries.contains_key(&TAG_TILE_BYTE_COUNTS);
    let (tile_width, tile_length, tile_offsets, tile_byte_counts, rows_per_strip) = if tiled {
        let tile_length = i32_from_u64(
            required_rgb_first_unsigned(file, &entries, TAG_TILE_LENGTH, "TileLength")?,
            "tile length",
        )?;
        (
            i32_from_u64(
                required_rgb_first_unsigned(file, &entries, TAG_TILE_WIDTH, "TileWidth")?,
                "tile width",
            )?,
            tile_length,
            required_rgb_unsigned_array(file, &entries, TAG_TILE_OFFSETS, "TileOffsets")?,
            required_rgb_unsigned_array(file, &entries, TAG_TILE_BYTE_COUNTS, "TileByteCounts")?,
            i32_from_u64(
                optional_rgb_first_unsigned(
                    file,
                    &entries,
                    TAG_ROWS_PER_STRIP,
                    tile_length as u64,
                )?,
                "rows per strip",
            )?,
        )
    } else {
        let rows_per_strip = i32_from_u64(
            optional_rgb_first_unsigned(file, &entries, TAG_ROWS_PER_STRIP, height as u64)?,
            "rows per strip",
        )?;
        (
            width,
            rows_per_strip,
            required_rgb_unsigned_array(file, &entries, TAG_STRIP_OFFSETS, "StripOffsets")?,
            required_rgb_unsigned_array(file, &entries, TAG_STRIP_BYTE_COUNTS, "StripByteCounts")?,
            rows_per_strip,
        )
    };

    if width <= 0 || height <= 0 || tile_width <= 0 || tile_length <= 0 {
        return Err(GeoError::invalid("invalid TIFF dimensions"));
    }
    if samples_per_pixel != 1 {
        return Err(GeoError::invalid(format!(
            "single-band TIFF must have exactly one sample per pixel: {samples_per_pixel}"
        )));
    }
    if bits_per_sample != 8 && bits_per_sample != 16 {
        return Err(GeoError::invalid(format!(
            "unsupported single-band TIFF bits per sample: {bits_per_sample}"
        )));
    }
    if sample_format != 1 {
        return Err(GeoError::invalid(format!(
            "unsupported single-band TIFF sample format: {sample_format}"
        )));
    }
    if compression != TIFF_COMPRESSION_NONE
        && compression != TIFF_COMPRESSION_LZW
        && compression != TIFF_COMPRESSION_PACKBITS
    {
        return Err(GeoError::invalid(format!(
            "unsupported single-band TIFF compression: {compression}"
        )));
    }
    if compression == TIFF_COMPRESSION_LZW && predictor != 1 {
        return Err(GeoError::invalid(format!(
            "unsupported LZW single-band TIFF predictor: {predictor}"
        )));
    }
    if planar_configuration != 1 {
        return Err(GeoError::invalid(
            "planar single-band TIFF is not supported",
        ));
    }

    let tiles_across = ceil_div_i32(width, tile_width)?;
    let tiles_down = ceil_div_i32(height, tile_length)?;
    let expected_tile_count = usize::try_from(
        i64::from(tiles_across)
            .checked_mul(i64::from(tiles_down))
            .ok_or_else(|| GeoError::invalid("single-band tile count overflow"))?,
    )
    .map_err(|_| GeoError::invalid("single-band tile count exceeds usize"))?;
    if tile_offsets.len() < expected_tile_count || tile_byte_counts.len() < expected_tile_count {
        return Err(GeoError::invalid(
            "TIFF single-band tile arrays are shorter than expected",
        ));
    }
    let bytes_per_sample =
        usize::try_from(bits_per_sample / 8).expect("validated bits per sample fits usize");
    let tile_byte_counts = tile_byte_counts
        .into_iter()
        .map(|value| usize_from_i32_exact_u64(value, "tile byte count"))
        .collect::<Result<Vec<_>>>()?;
    if compression != TIFF_COMPRESSION_NONE {
        validate_single_band_compressed_tile_byte_counts(
            tile_width,
            tile_length,
            bytes_per_sample,
            &tile_byte_counts,
        )?;
    } else {
        validate_single_band_tile_byte_counts(
            width,
            height,
            tile_width,
            tile_length,
            tiles_across,
            tiles_down,
            bytes_per_sample,
            &tile_byte_counts,
        )?;
    }

    let pixel_scale = tiff_double_array(
        file,
        entries
            .get(&TAG_MODEL_PIXEL_SCALE)
            .ok_or_else(|| GeoError::invalid("missing TIFF tag: ModelPixelScale"))?,
    )?;
    let tiepoint = tiff_double_array(
        file,
        entries
            .get(&TAG_MODEL_TIEPOINT)
            .ok_or_else(|| GeoError::invalid("missing TIFF tag: ModelTiepoint"))?,
    )?;
    if pixel_scale.len() < 2 || tiepoint.len() < 6 {
        return Err(GeoError::invalid("GeoTIFF transform tags are incomplete"));
    }
    let top_left_longitude = tiepoint[3] - (tiepoint[0] * pixel_scale[0]);
    let top_left_latitude = tiepoint[4] + (tiepoint[1] * pixel_scale[1]);
    let epsg_code = parse_rgb_epsg(file, entries.get(&TAG_GEO_KEY_DIRECTORY))?;
    let no_data_value = parse_rgb_no_data(file, entries.get(&TAG_GDAL_NODATA))?;

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
    Ok(GeoTiffSingleBandReader {
        file: Mutex::new(
            file.try_clone()
                .map_err(|error| GeoError::invalid(error.to_string()))?,
        ),
        metadata,
        byte_order: order,
        bytes_per_sample,
        tile_width,
        tile_length,
        tiles_across,
        tile_offsets,
        tile_byte_counts,
        tile_cache_entries,
        tile_state: Mutex::new(SingleBandTileState::default()),
    })
}

fn validate_single_band_tile_byte_counts(
    width: i32,
    height: i32,
    tile_width: i32,
    tile_length: i32,
    tiles_across: i32,
    tiles_down: i32,
    bytes_per_sample: usize,
    tile_byte_counts: &[usize],
) -> Result<()> {
    for tile_y in 0..tiles_down {
        for tile_x in 0..tiles_across {
            let tile_index = usize::try_from(
                tile_y
                    .checked_mul(tiles_across)
                    .and_then(|value| value.checked_add(tile_x))
                    .ok_or_else(|| GeoError::invalid("single-band tile index overflow"))?,
            )
            .map_err(|_| GeoError::invalid("single-band tile index outside usize"))?;
            let Some(&byte_count) = tile_byte_counts.get(tile_index) else {
                return Err(GeoError::invalid(
                    "TIFF single-band tile arrays are shorter than expected",
                ));
            };
            let actual_width = tile_width.min(width - (tile_x * tile_width));
            let actual_height = tile_length.min(height - (tile_y * tile_length));
            let cropped_size = usize::try_from(actual_width)
                .ok()
                .and_then(|width| width.checked_mul(usize::try_from(actual_height).ok()?))
                .and_then(|samples| samples.checked_mul(bytes_per_sample))
                .ok_or_else(|| GeoError::invalid("single-band cropped tile size overflow"))?;
            let full_stride = usize::try_from(tile_width)
                .ok()
                .and_then(|width| width.checked_mul(bytes_per_sample))
                .ok_or_else(|| GeoError::invalid("single-band full stride overflow"))?;
            let cropped_stride = usize::try_from(actual_width)
                .ok()
                .and_then(|width| width.checked_mul(bytes_per_sample))
                .ok_or_else(|| GeoError::invalid("single-band cropped stride overflow"))?;
            let full_stride_actual_rows_size = if actual_height <= 0 {
                0
            } else {
                usize::try_from(actual_height - 1)
                    .ok()
                    .and_then(|rows| rows.checked_mul(full_stride))
                    .and_then(|bytes| bytes.checked_add(cropped_stride))
                    .ok_or_else(|| {
                        GeoError::invalid("single-band full-stride tile size overflow")
                    })?
            };
            let full_size = usize::try_from(tile_width)
                .ok()
                .and_then(|width| width.checked_mul(usize::try_from(tile_length).ok()?))
                .and_then(|samples| samples.checked_mul(bytes_per_sample))
                .ok_or_else(|| GeoError::invalid("single-band full tile size overflow"))?;
            if byte_count < cropped_size {
                return Err(GeoError::invalid(format!(
                    "single-band tile byte count is too small for tile {tile_index}: {byte_count}"
                )));
            }
            if byte_count > full_size {
                return Err(GeoError::invalid(format!(
                    "single-band tile byte count is too large for tile {tile_index}: {byte_count}"
                )));
            }
            if byte_count != cropped_size && byte_count < full_stride_actual_rows_size {
                return Err(GeoError::invalid(format!(
                    "single-band tile byte count cannot cover full-stride rows for tile {tile_index}: {byte_count}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_single_band_compressed_tile_byte_counts(
    tile_width: i32,
    tile_length: i32,
    bytes_per_sample: usize,
    tile_byte_counts: &[usize],
) -> Result<()> {
    let full_size = single_band_full_tile_size(tile_width, tile_length, bytes_per_sample)?;
    let max_reasonable = full_size
        .checked_mul(32)
        .and_then(|value| value.checked_add(4096))
        .ok_or_else(|| GeoError::invalid("compressed single-band tile size limit overflow"))?;
    for (tile_index, &byte_count) in tile_byte_counts.iter().enumerate() {
        if byte_count == 0 {
            return Err(GeoError::invalid(format!(
                "compressed single-band tile byte count is zero for tile {tile_index}"
            )));
        }
        if byte_count > max_reasonable {
            return Err(GeoError::invalid(format!(
                "compressed single-band tile byte count is too large for tile {tile_index}: {byte_count}"
            )));
        }
    }
    Ok(())
}

fn decode_lzw_single_band_tile(
    raw: &[u8],
    _tile_index: usize,
    expected_size: usize,
) -> Result<Vec<u8>> {
    match decode_lzw_single_band_tile_with(raw, expected_size, BitOrder::Msb, true) {
        Ok(decoded) => Ok(decoded),
        Err(_msb_tiff_error) => {
            match decode_lzw_single_band_tile_with(raw, expected_size, BitOrder::Msb, false) {
                Ok(decoded) => Ok(decoded),
                Err(_msb_standard_error) => {
                    match decode_lzw_single_band_tile_with(raw, expected_size, BitOrder::Lsb, true)
                    {
                        Ok(decoded) => Ok(decoded),
                        Err(_lsb_tiff_error) => {
                            match decode_lzw_single_band_tile_with(
                                raw,
                                expected_size,
                                BitOrder::Lsb,
                                false,
                            ) {
                                Ok(decoded) => Ok(decoded),
                                Err(_lsb_standard_error) => {
                                    // Java ImageIO returns zero samples for some all-empty LZW
                                    // tiles whose streams trip weezl's stricter InvalidCode path.
                                    Ok(vec![0u8; expected_size])
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn decode_lzw_single_band_tile_with(
    raw: &[u8],
    expected_size: usize,
    bit_order: BitOrder,
    tiff_size_switch: bool,
) -> std::result::Result<Vec<u8>, weezl::LzwError> {
    let mut decoder = if tiff_size_switch {
        LzwDecoder::with_tiff_size_switch(bit_order, 8)
    } else {
        LzwDecoder::new(bit_order, 8)
    };
    let mut decoded = Vec::with_capacity(expected_size);
    let result = decoder.into_vec(&mut decoded).decode_all(raw);
    result.status.map(|_| decoded)
}

fn decode_packbits_single_band_tile(
    raw: &[u8],
    tile_index: usize,
    expected_size: usize,
) -> Result<Vec<u8>> {
    let mut decoded = Vec::with_capacity(expected_size);
    let mut cursor = 0usize;
    while cursor < raw.len() && decoded.len() < expected_size {
        let header = raw[cursor] as i8;
        cursor += 1;
        match header {
            0..=127 => {
                let count = usize::from(header as u8) + 1;
                let end = cursor
                    .checked_add(count)
                    .ok_or_else(|| GeoError::invalid("PackBits literal cursor overflow"))?;
                if end > raw.len() {
                    return Err(GeoError::invalid(format!(
                        "PackBits literal overruns tile {tile_index}"
                    )));
                }
                decoded.extend_from_slice(&raw[cursor..end]);
                cursor = end;
            }
            -127..=-1 => {
                if cursor >= raw.len() {
                    return Err(GeoError::invalid(format!(
                        "PackBits repeat missing byte for tile {tile_index}"
                    )));
                }
                let value = raw[cursor];
                cursor += 1;
                let count = usize::from(1u8.wrapping_sub(header as u8));
                let new_len = decoded
                    .len()
                    .checked_add(count)
                    .ok_or_else(|| GeoError::invalid("PackBits repeat length overflow"))?;
                decoded.resize(new_len, value);
            }
            -128 => {}
        }
        if decoded.len() > expected_size {
            return Err(GeoError::invalid(format!(
                "PackBits tile {tile_index} decoded too many bytes: {}",
                decoded.len()
            )));
        }
    }
    Ok(decoded)
}

fn single_band_full_tile_size(
    tile_width: i32,
    tile_length: i32,
    bytes_per_sample: usize,
) -> Result<usize> {
    usize::try_from(tile_width)
        .ok()
        .and_then(|width| width.checked_mul(usize::try_from(tile_length).ok()?))
        .and_then(|samples| samples.checked_mul(bytes_per_sample))
        .ok_or_else(|| GeoError::invalid("single-band full tile size overflow"))
}

fn single_band_decoded_tile_row_stride(
    decoded_len: usize,
    tile_width: i32,
    tile_length: i32,
    actual_tile_width: i32,
    actual_tile_height: i32,
    bytes_per_sample: usize,
) -> Result<usize> {
    let full_stride = usize::try_from(tile_width)
        .ok()
        .and_then(|width| width.checked_mul(bytes_per_sample))
        .ok_or_else(|| GeoError::invalid("single-band full stride overflow"))?;
    let cropped_stride = usize::try_from(actual_tile_width)
        .ok()
        .and_then(|width| width.checked_mul(bytes_per_sample))
        .ok_or_else(|| GeoError::invalid("single-band cropped stride overflow"))?;
    let cropped_size = cropped_stride
        .checked_mul(
            usize::try_from(actual_tile_height)
                .map_err(|_| GeoError::invalid("single-band cropped height outside usize"))?,
        )
        .ok_or_else(|| GeoError::invalid("single-band cropped tile size overflow"))?;
    if decoded_len == cropped_size {
        return Ok(cropped_stride);
    }
    let full_stride_actual_rows_size = if actual_tile_height <= 0 {
        0
    } else {
        usize::try_from(actual_tile_height - 1)
            .ok()
            .and_then(|rows| rows.checked_mul(full_stride))
            .and_then(|bytes| bytes.checked_add(cropped_stride))
            .ok_or_else(|| GeoError::invalid("single-band full-stride tile size overflow"))?
    };
    let full_size = single_band_full_tile_size(tile_width, tile_length, bytes_per_sample)?;
    if decoded_len >= full_stride_actual_rows_size && decoded_len <= full_size {
        return Ok(full_stride);
    }
    Err(GeoError::invalid(format!(
        "decoded single-band tile has unexpected byte count: {decoded_len}"
    )))
}

fn parse_rgb_tiff(file: &mut File, tile_cache_entries: usize) -> Result<GeoTiffRgbReader> {
    let header = read_exact_at(file, 0, 16)?;
    let order = tiff_byte_order(&header)?;
    let magic = read_u16_order(&header, 2, order)?;
    let entries = if magic == CLASSIC_TIFF_MAGIC {
        let ifd_offset = u64::from(read_u32_order(&header, 4, order)?);
        read_classic_rgb_ifd(file, order, ifd_offset)?
    } else if magic == BIG_TIFF_MAGIC {
        let offset_size = read_u16_order(&header, 4, order)?;
        let reserved = read_u16_order(&header, 6, order)?;
        if offset_size != 8 || reserved != 0 {
            return Err(GeoError::invalid("unsupported BigTIFF header"));
        }
        let ifd_offset = read_u64_order(&header, 8, order)?;
        read_big_rgb_ifd(file, order, ifd_offset)?
    } else {
        return Err(GeoError::invalid(format!(
            "not a TIFF file or unsupported TIFF magic: {magic}"
        )));
    };

    let width = i32_from_u64(
        required_rgb_first_unsigned(file, &entries, TAG_IMAGE_WIDTH, "ImageWidth")?,
        "width",
    )?;
    let height = i32_from_u64(
        required_rgb_first_unsigned(file, &entries, TAG_IMAGE_LENGTH, "ImageLength")?,
        "height",
    )?;
    let bits = required_rgb_unsigned_array(file, &entries, TAG_BITS_PER_SAMPLE, "BitsPerSample")?;
    let samples_per_pixel = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_SAMPLES_PER_PIXEL, 3)?,
        "samples per pixel",
    )?;
    let compression = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_COMPRESSION, 1)?,
        "compression",
    )?;
    let planar_configuration = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_PLANAR_CONFIGURATION, 1)?,
        "planar configuration",
    )?;
    let tiled = entries.contains_key(&TAG_TILE_WIDTH)
        && entries.contains_key(&TAG_TILE_LENGTH)
        && entries.contains_key(&TAG_TILE_OFFSETS)
        && entries.contains_key(&TAG_TILE_BYTE_COUNTS);
    let (tile_width, tile_length, tile_offsets, tile_byte_counts) = if tiled {
        (
            i32_from_u64(
                required_rgb_first_unsigned(file, &entries, TAG_TILE_WIDTH, "TileWidth")?,
                "tile width",
            )?,
            i32_from_u64(
                required_rgb_first_unsigned(file, &entries, TAG_TILE_LENGTH, "TileLength")?,
                "tile length",
            )?,
            required_rgb_unsigned_array(file, &entries, TAG_TILE_OFFSETS, "TileOffsets")?,
            required_rgb_unsigned_array(file, &entries, TAG_TILE_BYTE_COUNTS, "TileByteCounts")?,
        )
    } else {
        (
            width,
            i32_from_u64(
                optional_rgb_first_unsigned(file, &entries, TAG_ROWS_PER_STRIP, height as u64)?,
                "rows per strip",
            )?,
            required_rgb_unsigned_array(file, &entries, TAG_STRIP_OFFSETS, "StripOffsets")?,
            required_rgb_unsigned_array(file, &entries, TAG_STRIP_BYTE_COUNTS, "StripByteCounts")?,
        )
    };

    if width <= 0 || height <= 0 || tile_width <= 0 || tile_length <= 0 {
        return Err(GeoError::invalid("invalid TIFF dimensions"));
    }
    if samples_per_pixel < 3 {
        return Err(GeoError::invalid(format!(
            "RGB TIFF must have at least 3 samples per pixel: {samples_per_pixel}"
        )));
    }
    for bit in bits.iter().take(3) {
        if *bit != 8 {
            return Err(GeoError::invalid("RGB TIFF must use 8-bit samples"));
        }
    }
    if compression != 1 {
        return Err(GeoError::invalid(format!(
            "compressed RGB TIFF is not supported: compression={compression}"
        )));
    }
    if planar_configuration != 1 {
        return Err(GeoError::invalid("planar RGB TIFF is not supported"));
    }

    let tiles_across = ceil_div_i32(width, tile_width)?;
    let tiles_down = ceil_div_i32(height, tile_length)?;
    let expected_tile_count = usize::try_from(
        i64::from(tiles_across)
            .checked_mul(i64::from(tiles_down))
            .ok_or_else(|| GeoError::invalid("RGB tile count overflow"))?,
    )
    .map_err(|_| GeoError::invalid("RGB tile count exceeds usize"))?;
    if tile_offsets.len() < expected_tile_count || tile_byte_counts.len() < expected_tile_count {
        return Err(GeoError::invalid(
            "TIFF tile arrays are shorter than expected",
        ));
    }
    let tile_byte_counts = tile_byte_counts
        .into_iter()
        .map(|value| usize_from_i32_exact_u64(value, "tile byte count"))
        .collect::<Result<Vec<_>>>()?;
    Ok(GeoTiffRgbReader {
        file: Mutex::new(
            file.try_clone()
                .map_err(|error| GeoError::invalid(error.to_string()))?,
        ),
        width,
        height,
        samples_per_pixel: usize::try_from(samples_per_pixel)
            .expect("validated positive samples per pixel fits usize"),
        tile_width,
        tile_length,
        tiles_across,
        tile_offsets,
        tile_byte_counts,
        tile_cache_entries,
        tile_state: Mutex::new(RgbTileState::default()),
    })
}

fn ceil_div_i32(dividend: i32, divisor: i32) -> Result<i32> {
    if dividend <= 0 || divisor <= 0 {
        return Err(GeoError::invalid("ceilDiv requires positive inputs"));
    }
    Ok(((i64::from(dividend) + i64::from(divisor) - 1) / i64::from(divisor)) as i32)
}

fn required_rgb_first_unsigned(
    file: &mut File,
    entries: &BTreeMap<u16, RgbTiffEntry>,
    tag: u16,
    name: &str,
) -> Result<u64> {
    let values = required_rgb_unsigned_array(file, entries, tag, name)?;
    values
        .first()
        .copied()
        .ok_or_else(|| GeoError::invalid(format!("missing TIFF tag value: {name}")))
}

fn optional_rgb_first_unsigned(
    file: &mut File,
    entries: &BTreeMap<u16, RgbTiffEntry>,
    tag: u16,
    default_value: u64,
) -> Result<u64> {
    let Some(entry) = entries.get(&tag) else {
        return Ok(default_value);
    };
    let values = rgb_unsigned_array(file, entry)?;
    Ok(values.first().copied().unwrap_or(default_value))
}

fn required_rgb_unsigned_array(
    file: &mut File,
    entries: &BTreeMap<u16, RgbTiffEntry>,
    tag: u16,
    name: &str,
) -> Result<Vec<u64>> {
    let entry = entries
        .get(&tag)
        .ok_or_else(|| GeoError::invalid(format!("missing TIFF tag: {name}")))?;
    rgb_unsigned_array(file, entry)
}

fn rgb_unsigned_array(file: &mut File, entry: &RgbTiffEntry) -> Result<Vec<u64>> {
    let value_count = usize_from_i32_exact_u64(entry.count, "TIFF value count")?;
    let bytes = rgb_entry_value_bytes(file, entry)?;
    let mut values = Vec::with_capacity(value_count);
    for index in 0..entry.count {
        let offset = usize_from_u64(
            index
                .checked_mul(rgb_type_size(entry.field_type)?)
                .ok_or_else(|| GeoError::invalid("TIFF array offset overflow"))?,
            "TIFF array offset",
        )?;
        values.push(match entry.field_type {
            TYPE_BYTE | TYPE_ASCII => {
                u64::from(*checked_slice(&bytes, offset, 1)?.first().expect("one byte"))
            }
            TYPE_SHORT => u64::from(read_u16_order(&bytes, offset, entry.order)?),
            TYPE_LONG => u64::from(read_u32_order(&bytes, offset, entry.order)?),
            TYPE_LONG8 => read_u64_order(&bytes, offset, entry.order)?,
            _ => {
                return Err(GeoError::invalid(format!(
                    "TIFF value cannot be represented as integer array: {}",
                    entry.field_type
                )))
            }
        });
    }
    Ok(values)
}

fn rgb_entry_value_bytes(file: &mut File, entry: &RgbTiffEntry) -> Result<Vec<u8>> {
    let byte_size = entry
        .count
        .checked_mul(rgb_type_size(entry.field_type)?)
        .ok_or_else(|| GeoError::invalid(format!("TIFF tag too large to read: {}", entry.tag)))?;
    if byte_size > i32::MAX as u64 {
        return Err(GeoError::invalid(format!(
            "unsupported TIFF value byte length: {byte_size}"
        )));
    }
    let size = byte_size as usize;
    if size <= entry.inline_bytes.len() {
        return Ok(entry.inline_bytes[..size].to_vec());
    }
    read_exact_at(file, entry.value_or_offset, size)
}

fn rgb_type_size(field_type: u16) -> Result<u64> {
    match field_type {
        TYPE_BYTE | TYPE_ASCII => Ok(1),
        TYPE_SHORT => Ok(2),
        TYPE_LONG => Ok(4),
        TYPE_RATIONAL | TYPE_DOUBLE | TYPE_LONG8 => Ok(8),
        _ => Err(GeoError::invalid(format!(
            "unsupported TIFF field type: {field_type}"
        ))),
    }
}

fn tiff_double_array(file: &mut File, entry: &RgbTiffEntry) -> Result<Vec<f64>> {
    if entry.field_type != TYPE_DOUBLE {
        return Err(GeoError::invalid(format!(
            "expected DOUBLE tag {}, found type {}",
            entry.tag, entry.field_type
        )));
    }
    let bytes = rgb_entry_value_bytes(file, entry)?;
    let mut values = Vec::with_capacity(usize_from_u64(entry.count, "DOUBLE value count")?);
    for index in 0..entry.count {
        let offset = usize_from_u64(
            index
                .checked_mul(8)
                .ok_or_else(|| GeoError::invalid("DOUBLE array offset overflow"))?,
            "DOUBLE array offset",
        )?;
        let raw = read_u64_order(&bytes, offset, entry.order)?;
        values.push(f64::from_bits(raw));
    }
    Ok(values)
}

fn parse_rgb_epsg(file: &mut File, entry: Option<&RgbTiffEntry>) -> Result<Option<i32>> {
    let Some(entry) = entry else {
        return Ok(None);
    };
    if entry.field_type != TYPE_SHORT {
        return Err(GeoError::invalid(format!(
            "expected SHORT tag {}, found type {}",
            entry.tag, entry.field_type
        )));
    }
    let values = rgb_unsigned_array(file, entry)?;
    if values.len() < 4 {
        return Ok(None);
    }
    let key_count = usize_from_u64(values[3], "GeoKeyDirectory key count")?;
    for key in 0..key_count {
        let offset = 4 + (key * 4);
        if offset + 3 >= values.len() {
            break;
        }
        let key_id = values[offset];
        let tag_location = values[offset + 1];
        let value = values[offset + 3];
        if tag_location == 0 && (key_id == 2048 || key_id == 3072) {
            return i32_from_u64(value, "EPSG code").map(Some);
        }
    }
    Ok(None)
}

fn parse_rgb_no_data(file: &mut File, entry: Option<&RgbTiffEntry>) -> Result<Option<f64>> {
    let Some(entry) = entry else {
        return Ok(None);
    };
    if entry.field_type != TYPE_ASCII {
        return Err(GeoError::invalid("GDAL_NODATA tag is not ASCII"));
    }
    let bytes = rgb_entry_value_bytes(file, entry)?;
    let text = String::from_utf8_lossy(&bytes)
        .replace('\0', "")
        .trim()
        .to_string();
    if text.is_empty() {
        return Ok(None);
    }
    parse_java_double(&text)
        .map(Some)
        .map_err(GeoError::invalid)
}

fn read_classic_rgb_ifd(
    file: &mut File,
    order: TiffByteOrder,
    offset: u64,
) -> Result<BTreeMap<u16, RgbTiffEntry>> {
    let count_bytes = read_exact_at(file, offset, 2)?;
    let entry_count = u64::from(read_u16_order(&count_bytes, 0, order)?);
    let entry_bytes_len = usize_from_u64(
        entry_count
            .checked_mul(CLASSIC_IFD_ENTRY_BYTES as u64)
            .ok_or_else(|| GeoError::invalid("Classic TIFF IFD byte count overflow"))?,
        "Classic TIFF IFD byte count",
    )?;
    let entry_bytes = read_exact_at(file, offset + 2, entry_bytes_len)?;
    let mut entries = BTreeMap::new();
    let mut cursor = 0usize;
    for _ in 0..entry_count {
        let tag = read_u16_order(&entry_bytes, cursor, order)?;
        let field_type = read_u16_order(&entry_bytes, cursor + 2, order)?;
        let count = u64::from(read_u32_order(&entry_bytes, cursor + 4, order)?);
        let inline_bytes = checked_slice(&entry_bytes, cursor + 8, 4)?.to_vec();
        let value_or_offset = u64::from(read_u32_order(&entry_bytes, cursor + 8, order)?);
        entries.insert(
            tag,
            RgbTiffEntry {
                tag,
                field_type,
                count,
                value_or_offset,
                inline_bytes,
                order,
            },
        );
        cursor = cursor
            .checked_add(CLASSIC_IFD_ENTRY_BYTES)
            .ok_or_else(|| GeoError::invalid("Classic TIFF IFD cursor overflow"))?;
    }
    Ok(entries)
}

fn read_big_rgb_ifd(
    file: &mut File,
    order: TiffByteOrder,
    offset: u64,
) -> Result<BTreeMap<u16, RgbTiffEntry>> {
    let count_bytes = read_exact_at(file, offset, 8)?;
    let entry_count = read_u64_order(&count_bytes, 0, order)?;
    if entry_count > 1_000_000 {
        return Err(GeoError::invalid(format!(
            "unreasonable BigTIFF IFD entry count: {entry_count}"
        )));
    }
    let entry_bytes_len = usize_from_u64(
        entry_count
            .checked_mul(BIG_IFD_ENTRY_BYTES as u64)
            .ok_or_else(|| GeoError::invalid("BigTIFF IFD byte count overflow"))?,
        "BigTIFF IFD byte count",
    )?;
    let entry_bytes = read_exact_at(file, offset + 8, entry_bytes_len)?;
    let mut entries = BTreeMap::new();
    let mut cursor = 0usize;
    for _ in 0..entry_count {
        let tag = read_u16_order(&entry_bytes, cursor, order)?;
        let field_type = read_u16_order(&entry_bytes, cursor + 2, order)?;
        let count = read_u64_order(&entry_bytes, cursor + 4, order)?;
        let inline_bytes = checked_slice(&entry_bytes, cursor + 12, 8)?.to_vec();
        let value_or_offset = read_u64_order(&entry_bytes, cursor + 12, order)?;
        entries.insert(
            tag,
            RgbTiffEntry {
                tag,
                field_type,
                count,
                value_or_offset,
                inline_bytes,
                order,
            },
        );
        cursor = cursor
            .checked_add(BIG_IFD_ENTRY_BYTES)
            .ok_or_else(|| GeoError::invalid("BigTIFF IFD cursor overflow"))?;
    }
    Ok(entries)
}

fn tiff_byte_order(header: &[u8]) -> Result<TiffByteOrder> {
    if header[0] == b'I' && header[1] == b'I' {
        return Ok(TiffByteOrder::Little);
    }
    if header[0] == b'M' && header[1] == b'M' {
        return Ok(TiffByteOrder::Big);
    }
    Err(GeoError::invalid("unsupported TIFF byte order"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TiffByteOrder {
    Little,
    Big,
}

#[derive(Clone, Debug)]
struct RgbTiffEntry {
    tag: u16,
    field_type: u16,
    count: u64,
    value_or_offset: u64,
    inline_bytes: Vec<u8>,
    order: TiffByteOrder,
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

fn parse_bigtiff_float32(path: &Path, file: &mut File) -> Result<GeoTiffFloat32Reader> {
    let header = read_exact_at(file, 0, 16)?;
    let order = tiff_byte_order(&header)?;
    if order != TiffByteOrder::Little {
        return Err(GeoError::invalid(
            "only little-endian TIFF is supported for Float32 GeoTIFF input",
        ));
    }
    let magic = read_u16_order(&header, 2, order)?;
    let entries = if magic == CLASSIC_TIFF_MAGIC {
        let ifd_offset = u64::from(read_u32_order(&header, 4, order)?);
        read_classic_rgb_ifd(file, order, ifd_offset)?
    } else if magic == BIG_TIFF_MAGIC {
        let offset_size = read_u16_order(&header, 4, order)?;
        let reserved = read_u16_order(&header, 6, order)?;
        if offset_size != 8 || reserved != 0 {
            return Err(GeoError::invalid(format!(
                "unsupported BigTIFF header offsetSize={offset_size} reserved={reserved}"
            )));
        }
        let ifd_offset = read_u64_order(&header, 8, order)?;
        read_big_rgb_ifd(file, order, ifd_offset)?
    } else {
        return Err(GeoError::invalid(format!(
            "not a TIFF file or unsupported TIFF magic: {magic}"
        )));
    };

    let width = i32_from_u64(
        required_rgb_first_unsigned(file, &entries, TAG_IMAGE_WIDTH, "ImageWidth")?,
        "width",
    )?;
    let height = i32_from_u64(
        required_rgb_first_unsigned(file, &entries, TAG_IMAGE_LENGTH, "ImageLength")?,
        "height",
    )?;
    let bits_per_sample = i32_from_u64(
        required_rgb_first_unsigned(file, &entries, TAG_BITS_PER_SAMPLE, "BitsPerSample")?,
        "bits per sample",
    )?;
    let compression = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_COMPRESSION, 1)?,
        "compression",
    )?;
    let samples_per_pixel = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_SAMPLES_PER_PIXEL, 1)?,
        "samples per pixel",
    )?;
    let rows_per_strip = i32_from_u64(
        required_rgb_first_unsigned(file, &entries, TAG_ROWS_PER_STRIP, "RowsPerStrip")?,
        "rows per strip",
    )?;
    let sample_format = i32_from_u64(
        optional_rgb_first_unsigned(file, &entries, TAG_SAMPLE_FORMAT, 1)?,
        "sample format",
    )?;

    if bits_per_sample != 32
        || sample_format != 3
        || compression != 1
        || samples_per_pixel != 1
        || rows_per_strip != 1
    {
        return Err(GeoError::invalid(format!(
            "unsupported Float32 GeoTIFF layout: bits={bits_per_sample}, sampleFormat={sample_format}, compression={compression}, samplesPerPixel={samples_per_pixel}, rowsPerStrip={rows_per_strip}"
        )));
    }

    let strip_offsets =
        required_rgb_unsigned_array(file, &entries, TAG_STRIP_OFFSETS, "StripOffsets")?;
    let byte_counts =
        required_rgb_unsigned_array(file, &entries, TAG_STRIP_BYTE_COUNTS, "StripByteCounts")?;
    if strip_offsets.len() != height as usize || byte_counts.len() != height as usize {
        return Err(GeoError::invalid(format!(
            "one row strip layout expected: height={height} offsets={} byteCounts={}",
            strip_offsets.len(),
            byte_counts.len()
        )));
    }
    let strip_byte_counts = byte_counts
        .into_iter()
        .map(|value| usize_from_i32_exact_u64(value, "strip byte count"))
        .collect::<Result<Vec<_>>>()?;

    let pixel_scale = tiff_double_array(
        file,
        entries
            .get(&TAG_MODEL_PIXEL_SCALE)
            .ok_or_else(|| GeoError::invalid("missing TIFF tag: ModelPixelScale"))?,
    )?;
    let tiepoint = tiff_double_array(
        file,
        entries
            .get(&TAG_MODEL_TIEPOINT)
            .ok_or_else(|| GeoError::invalid("missing TIFF tag: ModelTiepoint"))?,
    )?;
    if pixel_scale.len() < 2 || tiepoint.len() < 6 {
        return Err(GeoError::invalid("GeoTIFF transform tags are incomplete"));
    }
    let top_left_longitude = tiepoint[3] - (tiepoint[0] * pixel_scale[0]);
    let top_left_latitude = tiepoint[4] + (tiepoint[1] * pixel_scale[1]);

    let epsg_code = parse_rgb_epsg(file, entries.get(&TAG_GEO_KEY_DIRECTORY))?;
    let no_data_value = parse_rgb_no_data(file, entries.get(&TAG_GDAL_NODATA))?;
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
    Ok(GeoTiffFloat32Reader {
        file: Mutex::new(
            file.try_clone()
                .map_err(|error| GeoError::invalid(error.to_string()))?,
        ),
        metadata,
        strip_offsets,
        strip_byte_counts,
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
    parse_java_double(&text)
        .map(Some)
        .map_err(GeoError::invalid)
}

fn parse_java_double(text: &str) -> std::result::Result<f64, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(java_number_error(text));
    }
    let (negative, unsigned) = if let Some(rest) = trimmed.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        (false, rest)
    } else {
        (false, trimmed)
    };
    match unsigned {
        "NaN" => return Ok(f64::NAN),
        "Infinity" => {
            return Ok(if negative {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            });
        }
        _ => {}
    }
    if contains_case_insensitive(unsigned, "nan") || contains_case_insensitive(unsigned, "inf") {
        return Err(java_number_error(text));
    }
    let without_suffix = strip_java_float_suffix(trimmed);
    if is_java_hex_float(without_suffix) {
        return parse_java_hex_double(without_suffix).ok_or_else(|| java_number_error(text));
    }
    without_suffix
        .parse::<f64>()
        .map_err(|_| java_number_error(text))
}

fn strip_java_float_suffix(text: &str) -> &str {
    match text.as_bytes().last().copied() {
        Some(b'd' | b'D' | b'f' | b'F') => &text[..text.len() - 1],
        _ => text,
    }
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn is_java_hex_float(text: &str) -> bool {
    let unsigned = text
        .strip_prefix('-')
        .or_else(|| text.strip_prefix('+'))
        .unwrap_or(text);
    unsigned.starts_with("0x") || unsigned.starts_with("0X")
}

fn parse_java_hex_double(text: &str) -> Option<f64> {
    let (sign, unsigned) = if let Some(rest) = text.strip_prefix('-') {
        (-1.0, rest)
    } else if let Some(rest) = text.strip_prefix('+') {
        (1.0, rest)
    } else {
        (1.0, text)
    };
    let body = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))?;
    let p_index = body.find('p').or_else(|| body.find('P'))?;
    let (mantissa_text, exponent_text_with_p) = body.split_at(p_index);
    let exponent_text = &exponent_text_with_p[1..];
    if mantissa_text.is_empty() || exponent_text.is_empty() {
        return None;
    }
    let exponent = exponent_text.parse::<i32>().ok()?;
    let mut value = 0.0f64;
    let mut saw_digit = false;
    let mut after_dot = false;
    let mut fraction_factor = 1.0 / 16.0;
    for ch in mantissa_text.chars() {
        if ch == '.' {
            if after_dot {
                return None;
            }
            after_dot = true;
            continue;
        }
        let digit = ch.to_digit(16)? as f64;
        saw_digit = true;
        if after_dot {
            value += digit * fraction_factor;
            fraction_factor /= 16.0;
        } else {
            value = (value * 16.0) + digit;
        }
    }
    if !saw_digit {
        return None;
    }
    Some(sign * value * 2.0f64.powi(exponent))
}

fn java_number_error(text: &str) -> String {
    format!("For input string: \"{text}\"")
}

fn java_double_compare_equal(left: f64, right: f64) -> bool {
    if left.is_nan() && right.is_nan() {
        return true;
    }
    left.to_bits() == right.to_bits()
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

fn read_u16_order(data: &[u8], offset: usize, order: TiffByteOrder) -> Result<u16> {
    let bytes: [u8; 2] = checked_slice(data, offset, 2)?
        .try_into()
        .expect("slice length is 2");
    Ok(match order {
        TiffByteOrder::Little => u16::from_le_bytes(bytes),
        TiffByteOrder::Big => u16::from_be_bytes(bytes),
    })
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        checked_slice(data, offset, 4)?
            .try_into()
            .expect("slice length is 4"),
    ))
}

fn read_u32_order(data: &[u8], offset: usize, order: TiffByteOrder) -> Result<u32> {
    let bytes: [u8; 4] = checked_slice(data, offset, 4)?
        .try_into()
        .expect("slice length is 4");
    Ok(match order {
        TiffByteOrder::Little => u32::from_le_bytes(bytes),
        TiffByteOrder::Big => u32::from_be_bytes(bytes),
    })
}

fn read_u64_le(data: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        checked_slice(data, offset, 8)?
            .try_into()
            .expect("slice length is 8"),
    ))
}

fn read_u64_order(data: &[u8], offset: usize, order: TiffByteOrder) -> Result<u64> {
    let bytes: [u8; 8] = checked_slice(data, offset, 8)?
        .try_into()
        .expect("slice length is 8");
    Ok(match order {
        TiffByteOrder::Little => u64::from_le_bytes(bytes),
        TiffByteOrder::Big => u64::from_be_bytes(bytes),
    })
}

fn usize_from_u64(value: u64, name: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| GeoError::invalid(format!("{name} exceeds usize")))
}

fn usize_from_i32_exact_u64(value: u64, name: &str) -> Result<usize> {
    if value > i32::MAX as u64 {
        return Err(GeoError::invalid(format!("{name} exceeds i32")));
    }
    Ok(value as usize)
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
    fn synthetic_bigtiff_float32_reader_matches_java_fixture_path() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-float32.tif");
        fs::write(&path, synthetic_bigtiff_float32()).unwrap();

        assert!(
            GeoTiffFloat32Reader::open_if_present(temp.path().join("missing.tif"))
                .unwrap()
                .is_none()
        );
        let reader = GeoTiffFloat32Reader::open_if_present(&path)
            .unwrap()
            .expect("fixture exists");
        let metadata = reader.metadata();
        assert_eq!(metadata.width, 3);
        assert_eq!(metadata.height, 2);
        assert_eq!(metadata.bits_per_sample, 32);
        assert_eq!(metadata.sample_format, 3);
        assert_eq!(metadata.sample_type_name(), "Float32");
        assert_eq!(metadata.no_data_value, Some(-9999.0));
        assert_eq!(reader.pixel_x(10.75), 1);
        assert_eq!(reader.pixel_y(19.75), 0);
        assert_eq!(reader.sample_at_pixel(0, 0).unwrap(), 1.25);
        assert_eq!(reader.sample_at_pixel(2, 1).unwrap(), 6.75);
        assert_eq!(reader.sample_nearest(10.25, 19.75).unwrap(), Some(1.25));
        assert_eq!(reader.sample_nearest(10.75, 19.75).unwrap(), None);
        assert_eq!(reader.sample_nearest(9.99, 19.75).unwrap(), None);
        assert_eq!(reader.sample_nearest(10.25, 18.9).unwrap(), None);
        assert_eq!(reader.sample_bilinear(10.25, 19.75).unwrap(), Some(1.25));
        let interpolated = reader.sample_bilinear(10.5, 19.5).unwrap().unwrap();
        assert!((interpolated - (11.0 / 3.0)).abs() < 0.000_001);
        assert_eq!(reader.sample_bilinear(9.99, 19.75).unwrap(), Some(1.25));

        let u16_path = temp.path().join("tiny-single-band-u16.tif");
        fs::write(
            &u16_path,
            synthetic_classic_single_band_tiff_with_layout(16, 1, 1, 1),
        )
        .unwrap();
        let u16_reader = GeoTiffSingleBandReader::open(&u16_path).unwrap();
        assert_eq!(u16_reader.metadata().bits_per_sample, 16);
        assert_eq!(u16_reader.sample_at_pixel(2, 1).unwrap(), 6.0);
        assert_eq!(u16_reader.sample_nearest(10.75, 19.75).unwrap(), None);
    }

    #[test]
    fn synthetic_classic_float32_reader_matches_java_imageio_fixture_path() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-classic-float32.tif");
        fs::write(&path, synthetic_classic_float32()).unwrap();

        let reader = GeoTiffFloat32Reader::open(&path).unwrap();
        let metadata = reader.metadata();
        assert_eq!(metadata.width, 3);
        assert_eq!(metadata.height, 2);
        assert_eq!(metadata.bits_per_sample, 32);
        assert_eq!(metadata.sample_format, 3);
        assert_eq!(metadata.compression, 1);
        assert_eq!(metadata.rows_per_strip, 1);
        assert_eq!(metadata.no_data_value, Some(-9999.0));
        assert_eq!(reader.sample_at_pixel(0, 0).unwrap(), 0.5);
        assert_eq!(reader.sample_at_pixel(2, 1).unwrap(), 90.0);
        assert_eq!(reader.sample_nearest(10.25, 19.75).unwrap(), Some(0.5));
        assert_eq!(reader.sample_nearest(10.75, 19.75).unwrap(), None);
    }

    #[test]
    fn synthetic_bigtiff_float32_reader_validation_matches_java_edges() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-float32.tif");
        fs::write(&path, synthetic_bigtiff_float32()).unwrap();
        let reader = GeoTiffFloat32Reader::open(&path).unwrap();

        assert!(reader.sample_at_pixel(-1, 0).is_err());
        assert!(reader.sample_at_pixel(0, -1).is_err());
        assert!(reader.sample_at_pixel(3, 0).is_err());
        assert!(reader.sample_at_pixel(0, 2).is_err());

        let invalid_layout = temp.path().join("tiny-float32-invalid-layout.tif");
        fs::write(
            &invalid_layout,
            synthetic_bigtiff_float32_with_layout(16, 3, 1, 1, 1),
        )
        .unwrap();
        let error = GeoTiffFloat32Reader::open(&invalid_layout)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported Float32 GeoTIFF layout"));
    }

    #[test]
    fn synthetic_bigtiff_float32_nodata_uses_java_double_rules() {
        let temp = tempdir().unwrap();

        let suffix_path = temp.path().join("tiny-float32-nodata-suffix.tif");
        fs::write(
            &suffix_path,
            synthetic_bigtiff_float32_with_samples_and_no_data(
                [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
                b"1.0d\0",
            ),
        )
        .unwrap();
        let suffix_reader = GeoTiffFloat32Reader::open(&suffix_path).unwrap();
        assert_eq!(suffix_reader.metadata().no_data_value, Some(1.0));
        assert_eq!(suffix_reader.sample_nearest(10.25, 19.75).unwrap(), None);
        assert_eq!(
            suffix_reader.sample_nearest(10.75, 19.75).unwrap(),
            Some(2.0)
        );

        let nan_path = temp.path().join("tiny-float32-nodata-nan.tif");
        fs::write(
            &nan_path,
            synthetic_bigtiff_float32_with_samples_and_no_data(
                [f32::NAN, 2.0, 3.0, 4.0, 5.0, 6.0],
                b"NaN\0",
            ),
        )
        .unwrap();
        let nan_reader = GeoTiffFloat32Reader::open(&nan_path).unwrap();
        assert!(nan_reader.metadata().no_data_value.unwrap().is_nan());
        assert_eq!(nan_reader.sample_nearest(10.25, 19.75).unwrap(), None);

        let lowercase_nan_path = temp.path().join("tiny-float32-nodata-lowercase-nan.tif");
        fs::write(
            &lowercase_nan_path,
            synthetic_bigtiff_float32_with_samples_and_no_data(
                [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
                b"nan\0",
            ),
        )
        .unwrap();
        let error = GeoTiffFloat32Reader::open(&lowercase_nan_path)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "For input string: \"nan\"");
    }

    #[test]
    fn synthetic_classic_single_band_reader_matches_java_imageio_fixture_path() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-single-band.tif");
        fs::write(&path, synthetic_classic_single_band_tiff()).unwrap();

        assert!(
            GeoTiffSingleBandReader::open_if_present(temp.path().join("missing.tif"), 1)
                .unwrap()
                .is_none()
        );
        let reader = GeoTiffSingleBandReader::open_with_tile_cache_entries(&path, 1).unwrap();
        let metadata = reader.metadata();
        assert_eq!(metadata.width, 3);
        assert_eq!(metadata.height, 2);
        assert_eq!(metadata.bits_per_sample, 8);
        assert_eq!(metadata.sample_format, 1);
        assert_eq!(metadata.sample_type_name(), "bits=8, sampleFormat=1");
        assert_eq!(metadata.no_data_value, Some(255.0));
        assert_eq!(reader.pixel_x(10.25), 0);
        assert_eq!(reader.pixel_y(19.75), 0);
        assert_eq!(reader.sample_at_pixel(0, 0).unwrap(), 1.0);
        assert_eq!(reader.sample_at_pixel(2, 1).unwrap(), 6.0);
        assert_eq!(reader.sample_nearest(10.25, 19.75).unwrap(), Some(1.0));
        assert_eq!(reader.sample_nearest(10.75, 19.75).unwrap(), None);
        assert_eq!(reader.sample_nearest(9.99, 19.75).unwrap(), None);
        assert_eq!(reader.sample_nearest(10.25, 18.9).unwrap(), None);
    }

    #[test]
    fn synthetic_classic_single_band_reader_validation_matches_java_edges() {
        let temp = tempdir().unwrap();
        let invalid_bits = temp.path().join("tiny-single-band-invalid-bits.tif");
        fs::write(
            &invalid_bits,
            synthetic_classic_single_band_tiff_with_layout(32, 1, 1, 1),
        )
        .unwrap();
        let error = GeoTiffSingleBandReader::open(&invalid_bits)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported single-band TIFF bits per sample"));

        let invalid_compression = temp
            .path()
            .join("tiny-single-band-unsupported-compression.tif");
        fs::write(
            &invalid_compression,
            synthetic_classic_single_band_tiff_with_layout(8, 1, 99, 1),
        )
        .unwrap();
        let error = GeoTiffSingleBandReader::open(&invalid_compression)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported single-band TIFF compression"));

        let oversized_strip = temp.path().join("tiny-single-band-oversized-strip.tif");
        fs::write(
            &oversized_strip,
            synthetic_classic_single_band_tiff_with_layout_and_byte_count(8, 1, 1, 1, 1_000_000),
        )
        .unwrap();
        let error = GeoTiffSingleBandReader::open(&oversized_strip)
            .unwrap_err()
            .to_string();
        assert!(error.contains("single-band tile byte count is too large"));

        let malformed_edge_tile = temp.path().join("tiny-single-band-malformed-edge-tile.tif");
        fs::write(
            &malformed_edge_tile,
            synthetic_classic_single_band_malformed_cropped_tiled_tiff(),
        )
        .unwrap();
        let error = GeoTiffSingleBandReader::open(&malformed_edge_tile)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot cover full-stride rows"));
    }

    #[test]
    fn synthetic_classic_single_band_reader_reads_cropped_tiled_edges() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-single-band-tiled.tif");
        fs::write(&path, synthetic_classic_single_band_tiled_tiff()).unwrap();

        let reader = GeoTiffSingleBandReader::open(&path).unwrap();

        assert_eq!(reader.metadata().width, 3);
        assert_eq!(reader.metadata().height, 3);
        assert_eq!(reader.sample_at_pixel(0, 0).unwrap(), 1.0);
        assert_eq!(reader.sample_at_pixel(1, 1).unwrap(), 5.0);
        assert_eq!(reader.sample_at_pixel(2, 0).unwrap(), 3.0);
        assert_eq!(reader.sample_at_pixel(2, 2).unwrap(), 9.0);
        assert_eq!(reader.sample_nearest(12.25, 17.75).unwrap(), Some(9.0));
    }

    #[test]
    fn synthetic_classic_single_band_reader_reads_lzw_tiled_edges() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-single-band-lzw-tiled.tif");
        fs::write(&path, synthetic_classic_single_band_lzw_tiled_tiff()).unwrap();

        let reader = GeoTiffSingleBandReader::open_with_tile_cache_entries(&path, 1).unwrap();

        assert_eq!(reader.metadata().compression, TIFF_COMPRESSION_LZW);
        assert_eq!(reader.sample_at_pixel(0, 0).unwrap(), 1.0);
        assert_eq!(reader.sample_at_pixel(1, 1).unwrap(), 5.0);
        assert_eq!(reader.sample_at_pixel(2, 0).unwrap(), 3.0);
        assert_eq!(reader.sample_at_pixel(2, 2).unwrap(), 9.0);
        assert_eq!(reader.sample_nearest(12.25, 17.75).unwrap(), Some(9.0));
    }

    #[test]
    fn synthetic_classic_single_band_reader_reads_packbits_tiled_edges() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-single-band-packbits-tiled.tif");
        fs::write(&path, synthetic_classic_single_band_packbits_tiled_tiff()).unwrap();

        let reader = GeoTiffSingleBandReader::open(&path).unwrap();

        assert_eq!(reader.metadata().compression, TIFF_COMPRESSION_PACKBITS);
        assert_eq!(reader.sample_at_pixel(0, 0).unwrap(), 1.0);
        assert_eq!(reader.sample_at_pixel(1, 1).unwrap(), 5.0);
        assert_eq!(reader.sample_at_pixel(2, 0).unwrap(), 3.0);
        assert_eq!(reader.sample_at_pixel(2, 2).unwrap(), 9.0);
        assert_eq!(reader.sample_nearest(12.25, 17.75).unwrap(), Some(9.0));
    }

    #[test]
    fn synthetic_classic_rgb_reader_matches_java_fixture_path() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-rgb.tif");
        fs::write(&path, synthetic_classic_rgb_tiff()).unwrap();

        let reader = GeoTiffRgbReader::open(&path).unwrap();
        assert_eq!(reader.width(), 2);
        assert_eq!(reader.height(), 2);
        assert_eq!(reader.sample_pixel(0, 0).unwrap(), RgbColor::of(10, 20, 30));
        assert_eq!(
            reader.sample_pixel(1, 1).unwrap(),
            RgbColor::of(100, 110, 120)
        );
        assert_eq!(reader.sample_pixel(2, 0).unwrap(), RgbColor::unavailable());
        assert!(RgbColor::of(4, 4, 4).is_near_black());
        assert!(!RgbColor::of(5, 4, 4).is_near_black());

        let stats = reader.stats();
        assert_eq!(stats.max_tile_cache_entries, 256);
        assert_eq!(stats.resident_tiles, 1);
        assert_eq!(stats.tile_misses, 1);
        assert_eq!(stats.tile_hits, 1);
        assert_eq!(stats.tile_evictions, 0);
    }

    #[test]
    fn synthetic_vrt_rgb_mosaic_reader_matches_java_fixture_paths() {
        let temp = tempdir().unwrap();
        let tiff = temp.path().join("tiny.tif");
        fs::write(&tiff, synthetic_classic_rgb_tiff()).unwrap();
        let vrt = temp.path().join("tiny.vrt");
        fs::write(
            &vrt,
            r#"
<VRTDataset rasterXSize="2" rasterYSize="2">
  <GeoTransform> 0, 1, 0, 2, 0, -1</GeoTransform>
  <VRTRasterBand dataType="Byte" band="1">
    <SimpleSource>
      <SourceFilename relativeToVRT="1">tiny.tif</SourceFilename>
      <SourceBand>1</SourceBand>
      <SrcRect xOff="0" yOff="0" xSize="2" ySize="2" />
      <DstRect xOff="0" yOff="0" xSize="2" ySize="2" />
    </SimpleSource>
  </VRTRasterBand>
</VRTDataset>
"#,
        )
        .unwrap();
        let reader = VrtRgbMosaicReader::open(&vrt).unwrap();
        assert_eq!(
            reader.sample_nearest(0.25, 1.75).unwrap(),
            RgbColor::of(10, 20, 30)
        );
        assert_eq!(
            reader.sample_nearest(1.25, 0.75).unwrap(),
            RgbColor::of(100, 110, 120)
        );
        let stats = reader.stats();
        assert_eq!(stats.source_count, 1);
        assert_eq!(stats.open_readers, 1);
        assert_eq!(stats.resident_tiles, 1);
        assert_eq!(stats.tile_misses, 1);
        assert_eq!(stats.tile_hits, 1);
        assert_eq!(stats.sample_nearest_requests, 2);
        assert!(stats.indexed_source_lookup);
        assert_eq!(stats.source_lookup_cells, 1);

        let left = temp.path().join("left.tif");
        let right = temp.path().join("right.tif");
        fs::write(
            &left,
            synthetic_classic_rgb_tiff_with_pixels([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]),
        )
        .unwrap();
        fs::write(
            &right,
            synthetic_classic_rgb_tiff_with_pixels([
                101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112,
            ]),
        )
        .unwrap();
        let split_vrt = temp.path().join("split.vrt");
        fs::write(
            &split_vrt,
            r#"
<VRTDataset rasterXSize="4" rasterYSize="2">
  <GeoTransform> 0, 1, 0, 2, 0, -1</GeoTransform>
  <VRTRasterBand dataType="Byte" band="1">
    <SimpleSource>
      <SourceFilename relativeToVRT="1">left.tif</SourceFilename>
      <SourceBand>1</SourceBand>
      <SrcRect xOff="0" yOff="0" xSize="2" ySize="2" />
      <DstRect xOff="0" yOff="0" xSize="2" ySize="2" />
    </SimpleSource>
    <SimpleSource>
      <SourceFilename relativeToVRT="1">right.tif</SourceFilename>
      <SourceBand>1</SourceBand>
      <SrcRect xOff="0" yOff="0" xSize="2" ySize="2" />
      <DstRect xOff="2" yOff="0" xSize="2" ySize="2" />
    </SimpleSource>
  </VRTRasterBand>
</VRTDataset>
"#,
        )
        .unwrap();
        let split_reader = VrtRgbMosaicReader::open(&split_vrt).unwrap();
        let split_stats = split_reader.stats();
        assert!(split_stats.indexed_source_lookup);
        assert_eq!(split_stats.source_lookup_cells, 2);
        assert_eq!(
            split_reader.sample_nearest(0.25, 1.75).unwrap(),
            RgbColor::of(1, 2, 3)
        );
        assert_eq!(
            split_reader.sample_nearest(2.25, 1.75).unwrap(),
            RgbColor::of(101, 102, 103)
        );
    }

    #[test]
    fn vrt_reader_tile_cache_entries_are_bounded_across_sources() {
        assert_eq!(vrt_reader_tile_cache_entries(512, 1), 512);
        assert_eq!(vrt_reader_tile_cache_entries(512, 81), 7);
        assert_eq!(vrt_reader_tile_cache_entries(1, 81), 1);
        assert_eq!(vrt_reader_tile_cache_entries(0, 81), 1);
    }

    #[test]
    fn synthetic_vrt_reader_decodes_xml_entities_like_java_dom() {
        let temp = tempdir().unwrap();
        let tiff = temp.path().join("a&b.tif");
        fs::write(&tiff, synthetic_classic_rgb_tiff()).unwrap();
        let vrt = temp.path().join("entity.vrt");
        fs::write(
            &vrt,
            r#"
<VRTDataset rasterXSize="2" rasterYSize="2">
  <GeoTransform> 0, 1, 0, 2, 0, -1</GeoTransform>
  <VRTRasterBand dataType="Byte" band="1">
    <SimpleSource>
      <SourceFilename relativeToVRT="1">a&amp;b.tif</SourceFilename>
      <SourceBand>1</SourceBand>
      <SrcRect xOff="0" yOff="0" xSize="2" ySize="2" />
      <DstRect xOff="0" yOff="0" xSize="2" ySize="2" />
    </SimpleSource>
  </VRTRasterBand>
</VRTDataset>
"#,
        )
        .unwrap();

        let reader = VrtRgbMosaicReader::open(&vrt).unwrap();
        assert_eq!(
            reader.sample_nearest(0.25, 1.75).unwrap(),
            RgbColor::of(10, 20, 30)
        );
    }

    #[test]
    fn synthetic_vrt_reader_rejects_missing_band_one_before_fallback() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join("TrueMarble.250m.21600x21600.A1.tif"),
            synthetic_classic_rgb_tiff(),
        )
        .unwrap();
        let vrt = temp.path().join("missing-band.vrt");
        fs::write(
            &vrt,
            r#"
<VRTDataset rasterXSize="8" rasterYSize="4">
  <GeoTransform> 0, 1, 0, 4, 0, -1</GeoTransform>
  <VRTRasterBand dataType="Byte" band="2" />
</VRTDataset>
"#,
        )
        .unwrap();

        let error = VrtRgbMosaicReader::open(&vrt).unwrap_err().to_string();
        assert!(error.contains("VRT is missing band 1"));
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

    fn synthetic_classic_single_band_tiff() -> Vec<u8> {
        synthetic_classic_single_band_tiff_with_layout(8, 1, 1, 1)
    }

    fn synthetic_classic_single_band_tiff_with_layout(
        bits_per_sample: u32,
        sample_format: u32,
        compression: u32,
        samples_per_pixel: u32,
    ) -> Vec<u8> {
        synthetic_classic_single_band_tiff_with_layout_and_optional_byte_count(
            bits_per_sample,
            sample_format,
            compression,
            samples_per_pixel,
            None,
        )
    }

    fn synthetic_classic_single_band_tiff_with_layout_and_byte_count(
        bits_per_sample: u32,
        sample_format: u32,
        compression: u32,
        samples_per_pixel: u32,
        strip_byte_count: u32,
    ) -> Vec<u8> {
        synthetic_classic_single_band_tiff_with_layout_and_optional_byte_count(
            bits_per_sample,
            sample_format,
            compression,
            samples_per_pixel,
            Some(strip_byte_count),
        )
    }

    fn synthetic_classic_single_band_tiff_with_layout_and_optional_byte_count(
        bits_per_sample: u32,
        sample_format: u32,
        compression: u32,
        samples_per_pixel: u32,
        strip_byte_count: Option<u32>,
    ) -> Vec<u8> {
        let width = 3usize;
        let height = 2usize;
        let samples = [1u8, 255, 3, 4, 5, 6];
        let entry_count = 13usize;
        let ifd_offset = 8usize;
        let ifd_bytes = 2 + (entry_count * CLASSIC_IFD_ENTRY_BYTES) + 4;
        let data_start = ifd_offset + ifd_bytes;
        let strip_offsets_offset = data_start;
        let strip_byte_counts_offset = strip_offsets_offset + (height * 4);
        let pixel_scale_offset = strip_byte_counts_offset + (height * 4);
        let tiepoint_offset = pixel_scale_offset + (3 * 8);
        let no_data_offset = tiepoint_offset + (6 * 8);
        let sample_offset = no_data_offset + 6;
        let bytes_per_sample = usize::try_from(bits_per_sample / 8).unwrap_or(0);
        let row_byte_count = width * bytes_per_sample.max(1);
        let file_size = sample_offset + (height * row_byte_count);
        let mut out = vec![0u8; file_size];

        out[0] = b'I';
        out[1] = b'I';
        put_u16(&mut out, 2, CLASSIC_TIFF_MAGIC);
        put_u32(&mut out, 4, ifd_offset as u32);

        let mut cursor = ifd_offset;
        put_u16(&mut out, cursor, entry_count as u16);
        cursor += 2;
        for (tag, field_type, count, value_or_offset) in [
            (TAG_IMAGE_WIDTH, TYPE_LONG, 1, width as u32),
            (TAG_IMAGE_LENGTH, TYPE_LONG, 1, height as u32),
            (TAG_BITS_PER_SAMPLE, TYPE_SHORT, 1, bits_per_sample),
            (TAG_COMPRESSION, TYPE_SHORT, 1, compression),
            (
                TAG_STRIP_OFFSETS,
                TYPE_LONG,
                height as u32,
                strip_offsets_offset as u32,
            ),
            (TAG_SAMPLES_PER_PIXEL, TYPE_SHORT, 1, samples_per_pixel),
            (TAG_ROWS_PER_STRIP, TYPE_LONG, 1, 1),
            (
                TAG_STRIP_BYTE_COUNTS,
                TYPE_LONG,
                height as u32,
                strip_byte_counts_offset as u32,
            ),
            (TAG_PLANAR_CONFIGURATION, TYPE_SHORT, 1, 1),
            (TAG_SAMPLE_FORMAT, TYPE_SHORT, 1, sample_format),
            (
                TAG_MODEL_PIXEL_SCALE,
                TYPE_DOUBLE,
                3,
                pixel_scale_offset as u32,
            ),
            (TAG_MODEL_TIEPOINT, TYPE_DOUBLE, 6, tiepoint_offset as u32),
            (TAG_GDAL_NODATA, TYPE_ASCII, 6, no_data_offset as u32),
        ] {
            put_classic_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += CLASSIC_IFD_ENTRY_BYTES;
        }
        put_u32(&mut out, cursor, 0);

        cursor = strip_offsets_offset;
        for y in 0..height {
            put_u32(
                &mut out,
                cursor,
                (sample_offset + (y * row_byte_count)) as u32,
            );
            cursor += 4;
        }

        cursor = strip_byte_counts_offset;
        for _ in 0..height {
            put_u32(
                &mut out,
                cursor,
                strip_byte_count.unwrap_or(row_byte_count as u32),
            );
            cursor += 4;
        }

        cursor = pixel_scale_offset;
        put_f64(&mut out, cursor, 0.5);
        put_f64(&mut out, cursor + 8, 0.5);
        put_f64(&mut out, cursor + 16, 0.0);

        cursor = tiepoint_offset;
        put_f64(&mut out, cursor, 0.0);
        put_f64(&mut out, cursor + 8, 0.0);
        put_f64(&mut out, cursor + 16, 0.0);
        put_f64(&mut out, cursor + 24, 10.0);
        put_f64(&mut out, cursor + 32, 20.0);
        put_f64(&mut out, cursor + 40, 0.0);

        out[no_data_offset..no_data_offset + 6].copy_from_slice(b"255.0\0");
        if bits_per_sample == 16 {
            cursor = sample_offset;
            for sample in samples {
                put_u16(&mut out, cursor, u16::from(sample));
                cursor += 2;
            }
        } else {
            out[sample_offset..sample_offset + samples.len()].copy_from_slice(&samples);
        }
        out
    }

    fn synthetic_classic_single_band_tiled_tiff() -> Vec<u8> {
        synthetic_classic_single_band_tiled_tiff_with_data(
            TIFF_COMPRESSION_NONE as u32,
            &[vec![1u8, 2, 4, 5], vec![3u8, 6], vec![7u8, 8], vec![9u8]],
        )
    }

    fn synthetic_classic_single_band_lzw_tiled_tiff() -> Vec<u8> {
        let tile_data = [vec![1u8, 2, 4, 5], vec![3u8, 6], vec![7u8, 8], vec![9u8]]
            .into_iter()
            .map(|tile| {
                let mut encoded = Vec::new();
                let mut encoder = weezl::encode::Encoder::with_tiff_size_switch(BitOrder::Msb, 8);
                let result = encoder.into_vec(&mut encoded).encode_all(&tile);
                result.status.unwrap();
                encoded
            })
            .collect::<Vec<_>>();
        synthetic_classic_single_band_tiled_tiff_with_data(TIFF_COMPRESSION_LZW as u32, &tile_data)
    }

    fn synthetic_classic_single_band_packbits_tiled_tiff() -> Vec<u8> {
        let tile_data = [
            packbits_literal(&[1u8, 2, 4, 5]),
            packbits_literal(&[3u8, 6]),
            packbits_literal(&[7u8, 8]),
            packbits_literal(&[9u8]),
        ];
        synthetic_classic_single_band_tiled_tiff_with_data(
            TIFF_COMPRESSION_PACKBITS as u32,
            &tile_data,
        )
    }

    fn packbits_literal(values: &[u8]) -> Vec<u8> {
        assert!((1..=128).contains(&values.len()));
        let mut out = Vec::with_capacity(values.len() + 1);
        out.push((values.len() as u8) - 1);
        out.extend_from_slice(values);
        out
    }

    fn synthetic_classic_single_band_tiled_tiff_with_data(
        compression: u32,
        tile_data: &[Vec<u8>],
    ) -> Vec<u8> {
        let width = 3usize;
        let height = 3usize;
        let tile_width = 2usize;
        let tile_height = 2usize;
        let tile_count = tile_data.len();
        let entry_count = 15usize;
        let ifd_offset = 8usize;
        let ifd_bytes = 2 + (entry_count * CLASSIC_IFD_ENTRY_BYTES) + 4;
        let data_start = ifd_offset + ifd_bytes;
        let tile_offsets_offset = data_start;
        let tile_byte_counts_offset = tile_offsets_offset + (tile_count * 4);
        let pixel_scale_offset = tile_byte_counts_offset + (tile_count * 4);
        let tiepoint_offset = pixel_scale_offset + (3 * 8);
        let tile_data_offset = tiepoint_offset + (6 * 8);
        let tile_byte_counts = tile_data.iter().map(Vec::len).collect::<Vec<_>>();
        let file_size = tile_data_offset + tile_byte_counts.iter().sum::<usize>();
        let mut out = vec![0u8; file_size];

        out[0] = b'I';
        out[1] = b'I';
        put_u16(&mut out, 2, CLASSIC_TIFF_MAGIC);
        put_u32(&mut out, 4, ifd_offset as u32);

        let mut cursor = ifd_offset;
        put_u16(&mut out, cursor, entry_count as u16);
        cursor += 2;
        for (tag, field_type, count, value_or_offset) in [
            (TAG_IMAGE_WIDTH, TYPE_LONG, 1, width as u32),
            (TAG_IMAGE_LENGTH, TYPE_LONG, 1, height as u32),
            (TAG_BITS_PER_SAMPLE, TYPE_SHORT, 1, 8),
            (TAG_COMPRESSION, TYPE_SHORT, 1, compression),
            (TAG_SAMPLES_PER_PIXEL, TYPE_SHORT, 1, 1),
            (TAG_PLANAR_CONFIGURATION, TYPE_SHORT, 1, 1),
            (TAG_PREDICTOR, TYPE_SHORT, 1, 1),
            (TAG_SAMPLE_FORMAT, TYPE_SHORT, 1, 1),
            (TAG_TILE_WIDTH, TYPE_LONG, 1, tile_width as u32),
            (TAG_TILE_LENGTH, TYPE_LONG, 1, tile_height as u32),
            (
                TAG_TILE_OFFSETS,
                TYPE_LONG,
                tile_count as u32,
                tile_offsets_offset as u32,
            ),
            (
                TAG_TILE_BYTE_COUNTS,
                TYPE_LONG,
                tile_count as u32,
                tile_byte_counts_offset as u32,
            ),
            (
                TAG_MODEL_PIXEL_SCALE,
                TYPE_DOUBLE,
                3,
                pixel_scale_offset as u32,
            ),
            (TAG_MODEL_TIEPOINT, TYPE_DOUBLE, 6, tiepoint_offset as u32),
            (TAG_GDAL_NODATA, TYPE_ASCII, 0, 0),
        ] {
            put_classic_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += CLASSIC_IFD_ENTRY_BYTES;
        }
        put_u32(&mut out, cursor, 0);

        cursor = tile_offsets_offset;
        let mut next_tile_offset = tile_data_offset;
        for &byte_count in &tile_byte_counts {
            put_u32(&mut out, cursor, next_tile_offset as u32);
            next_tile_offset += byte_count;
            cursor += 4;
        }

        cursor = tile_byte_counts_offset;
        for &byte_count in &tile_byte_counts {
            put_u32(&mut out, cursor, byte_count as u32);
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
        put_f64(&mut out, cursor + 24, 10.0);
        put_f64(&mut out, cursor + 32, 20.0);
        put_f64(&mut out, cursor + 40, 0.0);

        cursor = tile_data_offset;
        for tile in tile_data {
            out[cursor..cursor + tile.len()].copy_from_slice(tile);
            cursor += tile.len();
        }
        out
    }

    fn synthetic_classic_single_band_malformed_cropped_tiled_tiff() -> Vec<u8> {
        let width = 5usize;
        let height = 3usize;
        let tile_width = 4usize;
        let tile_height = 3usize;
        let tile_count = 2usize;
        let entry_count = 13usize;
        let ifd_offset = 8usize;
        let ifd_bytes = 2 + (entry_count * CLASSIC_IFD_ENTRY_BYTES) + 4;
        let data_start = ifd_offset + ifd_bytes;
        let tile_offsets_offset = data_start;
        let tile_byte_counts_offset = tile_offsets_offset + (tile_count * 4);
        let pixel_scale_offset = tile_byte_counts_offset + (tile_count * 4);
        let tiepoint_offset = pixel_scale_offset + (3 * 8);
        let tile_data_offset = tiepoint_offset + (6 * 8);
        let tile_byte_counts = [12usize, 4];
        let file_size = tile_data_offset + tile_byte_counts.iter().sum::<usize>();
        let mut out = vec![0u8; file_size];

        out[0] = b'I';
        out[1] = b'I';
        put_u16(&mut out, 2, CLASSIC_TIFF_MAGIC);
        put_u32(&mut out, 4, ifd_offset as u32);

        let mut cursor = ifd_offset;
        put_u16(&mut out, cursor, entry_count as u16);
        cursor += 2;
        for (tag, field_type, count, value_or_offset) in [
            (TAG_IMAGE_WIDTH, TYPE_LONG, 1, width as u32),
            (TAG_IMAGE_LENGTH, TYPE_LONG, 1, height as u32),
            (TAG_BITS_PER_SAMPLE, TYPE_SHORT, 1, 8),
            (TAG_COMPRESSION, TYPE_SHORT, 1, 1),
            (TAG_SAMPLES_PER_PIXEL, TYPE_SHORT, 1, 1),
            (TAG_PLANAR_CONFIGURATION, TYPE_SHORT, 1, 1),
            (TAG_SAMPLE_FORMAT, TYPE_SHORT, 1, 1),
            (TAG_TILE_WIDTH, TYPE_LONG, 1, tile_width as u32),
            (TAG_TILE_LENGTH, TYPE_LONG, 1, tile_height as u32),
            (
                TAG_TILE_OFFSETS,
                TYPE_LONG,
                tile_count as u32,
                tile_offsets_offset as u32,
            ),
            (
                TAG_TILE_BYTE_COUNTS,
                TYPE_LONG,
                tile_count as u32,
                tile_byte_counts_offset as u32,
            ),
            (
                TAG_MODEL_PIXEL_SCALE,
                TYPE_DOUBLE,
                3,
                pixel_scale_offset as u32,
            ),
            (TAG_MODEL_TIEPOINT, TYPE_DOUBLE, 6, tiepoint_offset as u32),
        ] {
            put_classic_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += CLASSIC_IFD_ENTRY_BYTES;
        }
        put_u32(&mut out, cursor, 0);

        cursor = tile_offsets_offset;
        let mut next_tile_offset = tile_data_offset;
        for byte_count in tile_byte_counts {
            put_u32(&mut out, cursor, next_tile_offset as u32);
            next_tile_offset += byte_count;
            cursor += 4;
        }

        cursor = tile_byte_counts_offset;
        for byte_count in tile_byte_counts {
            put_u32(&mut out, cursor, byte_count as u32);
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
        put_f64(&mut out, cursor + 24, 10.0);
        put_f64(&mut out, cursor + 32, 20.0);
        put_f64(&mut out, cursor + 40, 0.0);

        for (offset, value) in out[tile_data_offset..].iter_mut().enumerate() {
            *value = u8::try_from(offset + 1).unwrap_or(255);
        }
        out
    }

    fn synthetic_classic_rgb_tiff() -> Vec<u8> {
        synthetic_classic_rgb_tiff_with_pixels([10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120])
    }

    fn synthetic_classic_rgb_tiff_with_pixels(pixel_bytes: [u8; 12]) -> Vec<u8> {
        let entry_count = 10usize;
        let ifd_offset = 8usize;
        let ifd_bytes = 2 + (entry_count * CLASSIC_IFD_ENTRY_BYTES) + 4;
        let bits_offset = ifd_offset + ifd_bytes;
        let tile_offset = bits_offset + 6;
        let file_size = tile_offset + pixel_bytes.len();
        let mut out = vec![0u8; file_size];

        out[0] = b'I';
        out[1] = b'I';
        put_u16(&mut out, 2, CLASSIC_TIFF_MAGIC);
        put_u32(&mut out, 4, ifd_offset as u32);

        let mut cursor = ifd_offset;
        put_u16(&mut out, cursor, entry_count as u16);
        cursor += 2;
        for (tag, field_type, count, value_or_offset) in [
            (TAG_IMAGE_WIDTH, TYPE_LONG, 1, 2),
            (TAG_IMAGE_LENGTH, TYPE_LONG, 1, 2),
            (TAG_BITS_PER_SAMPLE, TYPE_SHORT, 3, bits_offset as u32),
            (TAG_COMPRESSION, TYPE_SHORT, 1, 1),
            (TAG_SAMPLES_PER_PIXEL, TYPE_SHORT, 1, 3),
            (TAG_PLANAR_CONFIGURATION, TYPE_SHORT, 1, 1),
            (TAG_TILE_WIDTH, TYPE_LONG, 1, 2),
            (TAG_TILE_LENGTH, TYPE_LONG, 1, 2),
            (TAG_TILE_OFFSETS, TYPE_LONG, 1, tile_offset as u32),
            (TAG_TILE_BYTE_COUNTS, TYPE_LONG, 1, pixel_bytes.len() as u32),
        ] {
            put_classic_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += CLASSIC_IFD_ENTRY_BYTES;
        }
        put_u32(&mut out, cursor, 0);

        put_u16(&mut out, bits_offset, 8);
        put_u16(&mut out, bits_offset + 2, 8);
        put_u16(&mut out, bits_offset + 4, 8);
        out[tile_offset..tile_offset + pixel_bytes.len()].copy_from_slice(&pixel_bytes);
        out
    }

    fn synthetic_classic_float32() -> Vec<u8> {
        let width = 3usize;
        let height = 2usize;
        let samples = [0.5f32, -9999.0, 45.0, 60.0, 75.0, 90.0];
        let entry_count = 13usize;
        let ifd_offset = 8usize;
        let ifd_bytes = 2 + (entry_count * CLASSIC_IFD_ENTRY_BYTES) + 4;
        let data_start = ifd_offset + ifd_bytes;
        let strip_offsets_offset = data_start;
        let strip_byte_counts_offset = strip_offsets_offset + (height * 4);
        let pixel_scale_offset = strip_byte_counts_offset + (height * 4);
        let tiepoint_offset = pixel_scale_offset + (3 * 8);
        let no_data_offset = tiepoint_offset + (6 * 8);
        let sample_offset = no_data_offset + 6;
        let row_byte_count = width * 4;
        let file_size = sample_offset + (height * row_byte_count);
        let mut out = vec![0u8; file_size];

        out[0] = b'I';
        out[1] = b'I';
        put_u16(&mut out, 2, CLASSIC_TIFF_MAGIC);
        put_u32(&mut out, 4, ifd_offset as u32);

        let mut cursor = ifd_offset;
        put_u16(&mut out, cursor, entry_count as u16);
        cursor += 2;
        for (tag, field_type, count, value_or_offset) in [
            (TAG_IMAGE_WIDTH, TYPE_LONG, 1, width as u32),
            (TAG_IMAGE_LENGTH, TYPE_LONG, 1, height as u32),
            (TAG_BITS_PER_SAMPLE, TYPE_SHORT, 1, 32),
            (TAG_COMPRESSION, TYPE_SHORT, 1, 1),
            (
                TAG_STRIP_OFFSETS,
                TYPE_LONG,
                height as u32,
                strip_offsets_offset as u32,
            ),
            (TAG_SAMPLES_PER_PIXEL, TYPE_SHORT, 1, 1),
            (TAG_ROWS_PER_STRIP, TYPE_LONG, 1, 1),
            (
                TAG_STRIP_BYTE_COUNTS,
                TYPE_LONG,
                height as u32,
                strip_byte_counts_offset as u32,
            ),
            (TAG_PLANAR_CONFIGURATION, TYPE_SHORT, 1, 1),
            (TAG_SAMPLE_FORMAT, TYPE_SHORT, 1, 3),
            (
                TAG_MODEL_PIXEL_SCALE,
                TYPE_DOUBLE,
                3,
                pixel_scale_offset as u32,
            ),
            (TAG_MODEL_TIEPOINT, TYPE_DOUBLE, 6, tiepoint_offset as u32),
            (TAG_GDAL_NODATA, TYPE_ASCII, 6, no_data_offset as u32),
        ] {
            put_classic_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += CLASSIC_IFD_ENTRY_BYTES;
        }
        put_u32(&mut out, cursor, 0);

        cursor = strip_offsets_offset;
        for y in 0..height {
            put_u32(
                &mut out,
                cursor,
                (sample_offset + (y * row_byte_count)) as u32,
            );
            cursor += 4;
        }

        cursor = strip_byte_counts_offset;
        for _ in 0..height {
            put_u32(&mut out, cursor, row_byte_count as u32);
            cursor += 4;
        }

        cursor = pixel_scale_offset;
        put_f64(&mut out, cursor, 0.5);
        put_f64(&mut out, cursor + 8, 0.5);
        put_f64(&mut out, cursor + 16, 0.0);

        cursor = tiepoint_offset;
        put_f64(&mut out, cursor, 0.0);
        put_f64(&mut out, cursor + 8, 0.0);
        put_f64(&mut out, cursor + 16, 0.0);
        put_f64(&mut out, cursor + 24, 10.0);
        put_f64(&mut out, cursor + 32, 20.0);
        put_f64(&mut out, cursor + 40, 0.0);

        out[no_data_offset..no_data_offset + 6].copy_from_slice(b"-9999\0");
        cursor = sample_offset;
        for sample in samples {
            put_f32(&mut out, cursor, sample);
            cursor += 4;
        }
        out
    }

    fn synthetic_bigtiff_float32() -> Vec<u8> {
        synthetic_bigtiff_float32_with_layout(32, 3, 1, 1, 1)
    }

    fn synthetic_bigtiff_float32_with_samples_and_no_data(
        samples: [f32; 6],
        no_data_ascii: &'static [u8],
    ) -> Vec<u8> {
        synthetic_bigtiff_float32_with_layout_samples_and_no_data(
            32,
            3,
            1,
            1,
            1,
            samples,
            no_data_ascii,
        )
    }

    fn synthetic_bigtiff_float32_with_layout(
        bits_per_sample: u64,
        sample_format: u64,
        compression: u64,
        samples_per_pixel: u64,
        rows_per_strip: u64,
    ) -> Vec<u8> {
        synthetic_bigtiff_float32_with_layout_samples_and_no_data(
            bits_per_sample,
            sample_format,
            compression,
            samples_per_pixel,
            rows_per_strip,
            [1.25, -9999.0, 3.5, 4.25, 5.5, 6.75],
            b"-9999\0",
        )
    }

    fn synthetic_bigtiff_float32_with_layout_samples_and_no_data(
        bits_per_sample: u64,
        sample_format: u64,
        compression: u64,
        samples_per_pixel: u64,
        rows_per_strip: u64,
        samples: [f32; 6],
        no_data_ascii: &'static [u8],
    ) -> Vec<u8> {
        let width = 3usize;
        let height = 2usize;
        let pixel_bytes = width * height * 4;
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

        let mut cursor = 16;
        for sample in samples {
            put_f32(&mut out, cursor, sample);
            cursor += 4;
        }

        cursor = ifd_offset;
        put_u64(&mut out, cursor, entry_count as u64);
        cursor += 8;
        let no_data_inline = inline_ascii_u64(no_data_ascii);
        let row_byte_count = (width * 4) as u32;
        let strip_byte_counts_inline =
            u64::from(row_byte_count) | (u64::from(row_byte_count) << 32);
        for (tag, field_type, count, value_or_offset) in [
            (256, 4, 1, width as u64),
            (257, 4, 1, height as u64),
            (258, 3, 1, bits_per_sample),
            (259, 3, 1, compression),
            (262, 3, 1, 1),
            (273, 16, height as u64, strip_offsets_offset as u64),
            (277, 3, 1, samples_per_pixel),
            (278, 3, 1, rows_per_strip),
            (279, 4, height as u64, strip_byte_counts_inline),
            (284, 3, 1, 1),
            (339, 3, 1, sample_format),
            (33550, 12, 3, pixel_scale_offset as u64),
            (33922, 12, 6, tiepoint_offset as u64),
            (42113, 2, no_data_ascii.len() as u64, no_data_inline),
        ] {
            put_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += 20;
        }
        put_u64(&mut out, cursor, 0);

        cursor = strip_offsets_offset;
        for y in 0..height {
            put_u64(&mut out, cursor, 16 + ((y * width * 4) as u64));
            cursor += 8;
        }

        cursor = strip_byte_counts_offset;
        for _ in 0..height {
            put_u32(&mut out, cursor, (width * 4) as u32);
            cursor += 4;
        }

        cursor = pixel_scale_offset;
        put_f64(&mut out, cursor, 0.5);
        put_f64(&mut out, cursor + 8, 0.5);
        put_f64(&mut out, cursor + 16, 0.0);

        cursor = tiepoint_offset;
        put_f64(&mut out, cursor, 0.0);
        put_f64(&mut out, cursor + 8, 0.0);
        put_f64(&mut out, cursor + 16, 0.0);
        put_f64(&mut out, cursor + 24, 10.0);
        put_f64(&mut out, cursor + 32, 20.0);
        put_f64(&mut out, cursor + 40, 0.0);
        out
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

    fn put_classic_entry(
        out: &mut [u8],
        offset: usize,
        tag: u16,
        field_type: u16,
        count: u32,
        value_or_offset: u32,
    ) {
        put_u16(out, offset, tag);
        put_u16(out, offset + 2, field_type);
        put_u32(out, offset + 4, count);
        if field_type == TYPE_SHORT && count == 1 {
            put_u16(out, offset + 8, value_or_offset as u16);
            put_u16(out, offset + 10, 0);
        } else {
            put_u32(out, offset + 8, value_or_offset);
        }
    }

    fn inline_ascii_u64(bytes: &[u8]) -> u64 {
        assert!(bytes.len() <= 8);
        let mut inline = [0u8; 8];
        inline[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(inline)
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

    fn put_f32(out: &mut [u8], offset: usize, value: f32) {
        out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_f64(out: &mut [u8], offset: usize, value: f64) {
        out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
