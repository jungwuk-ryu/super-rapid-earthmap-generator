#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use rayon::prelude::*;
use xxhash_rust::xxh64::xxh64;

pub const MODULE_STATUS: &str = "phase3-region-writer-bootstrap";

const REGION_CHUNK_WIDTH: u8 = 32;
const MCA_SECTOR_BYTES: usize = 4096;
const MCA_HEADER_BYTES: usize = MCA_SECTOR_BYTES * 2;
const MCA_HEADER_SECTORS: usize = 2;
const MCA_COMPRESSION_ZLIB: u8 = 2;
const MCA_DEFAULT_COMPRESSION_LEVEL: u32 = 6;

const LINEAR_SUPERBLOCK: u64 = 0xc3ff_1318_3cca_9d9a;
const LINEAR_VERSION: u8 = 3;
const LINEAR_GRID_SIZE: u8 = 8;
const LINEAR_CHUNKS_PER_REGION: usize = 32 * 32;
const LINEAR_BUCKET_COUNT: usize = 64;
const LINEAR_DEFAULT_COMPRESSION_LEVEL: i32 = 4;

pub const REGION_CHUNKS_PER_REGION: usize = LINEAR_CHUNKS_PER_REGION;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub type Result<T> = std::result::Result<T, RegionError>;

#[derive(Debug)]
pub enum RegionError {
    Io(std::io::Error),
    Invalid(String),
}

impl fmt::Display for RegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegionError::Io(error) => write!(f, "{error}"),
            RegionError::Invalid(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for RegionError {}

impl From<std::io::Error> for RegionError {
    fn from(error: std::io::Error) -> Self {
        RegionError::Io(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionFormat {
    Mca,
    Linear,
}

impl RegionFormat {
    pub fn as_manifest_value(self) -> &'static str {
        match self {
            RegionFormat::Mca => "mca",
            RegionFormat::Linear => "linear",
        }
    }

    pub fn extension(self) -> &'static str {
        self.as_manifest_value()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkLocalPos {
    pub x: u8,
    pub z: u8,
}

impl ChunkLocalPos {
    pub fn new(x: u8, z: u8) -> Result<Self> {
        if x >= REGION_CHUNK_WIDTH || z >= REGION_CHUNK_WIDTH {
            return Err(RegionError::Invalid(format!(
                "chunk local position out of range: {x},{z}"
            )));
        }
        Ok(Self { x, z })
    }

    pub fn header_index(self) -> usize {
        usize::from(self.x) + (usize::from(self.z) * usize::from(REGION_CHUNK_WIDTH))
    }
}

impl Ord for ChunkLocalPos {
    fn cmp(&self, other: &Self) -> Ordering {
        self.header_index().cmp(&other.header_index())
    }
}

impl PartialOrd for ChunkLocalPos {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug)]
pub struct RegionPayloads {
    pub format: RegionFormat,
    pub region_x: i32,
    pub region_z: i32,
    pub chunks: BTreeMap<ChunkLocalPos, Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionResumeValidation {
    pub format: RegionFormat,
    pub region_x: i32,
    pub region_z: i32,
    pub chunk_count: usize,
    pub file_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McaRegionValidation {
    pub chunk_count: usize,
    pub total_sectors: usize,
    pub file_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinearRegionValidation {
    pub file_bytes: u64,
    pub region_x: i32,
    pub region_z: i32,
    pub grid_size: u8,
    pub chunk_count: usize,
    pub bitmap_chunk_count: usize,
    pub bitmap_missing_payload_count: usize,
    pub bitmap_extra_chunk_count: usize,
}

impl LinearRegionValidation {
    pub fn bitmap_consistent(self) -> bool {
        self.bitmap_missing_payload_count == 0 && self.bitmap_extra_chunk_count == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionPayloadComparison {
    pub compared_chunks: usize,
    pub matching_chunks: usize,
    pub mismatched_chunks: usize,
    pub missing_in_mca: usize,
    pub missing_in_linear: usize,
    pub first_mismatch: Option<ChunkLocalPos>,
}

impl RegionPayloadComparison {
    pub fn matches(self) -> bool {
        self.mismatched_chunks == 0 && self.missing_in_mca == 0 && self.missing_in_linear == 0
    }
}

pub fn validate_mca_region_file(path: impl AsRef<Path>) -> Result<McaRegionValidation> {
    let path = path.as_ref();
    let payloads = read_mca_region_payloads(path)?;
    let validation = validate_region_file_for_resume(
        path,
        RegionFormat::Mca,
        payloads.region_x,
        payloads.region_z,
        payloads.chunks.len(),
    )?;
    Ok(McaRegionValidation {
        chunk_count: validation.chunk_count,
        total_sectors: usize::try_from(validation.file_bytes).expect("file size fits usize")
            / MCA_SECTOR_BYTES,
        file_bytes: validation.file_bytes,
    })
}

pub fn validate_linear_region_file(path: impl AsRef<Path>) -> Result<LinearRegionValidation> {
    let path = path.as_ref();
    let payloads = read_linear_region_payloads(path)?;
    let validation = validate_region_file_for_resume(
        path,
        RegionFormat::Linear,
        payloads.region_x,
        payloads.region_z,
        payloads.chunks.len(),
    )?;
    Ok(LinearRegionValidation {
        file_bytes: validation.file_bytes,
        region_x: validation.region_x,
        region_z: validation.region_z,
        grid_size: LINEAR_GRID_SIZE,
        chunk_count: validation.chunk_count,
        bitmap_chunk_count: validation.chunk_count,
        bitmap_missing_payload_count: 0,
        bitmap_extra_chunk_count: 0,
    })
}

pub fn compare_mca_linear_region_payloads(
    mca_region: impl AsRef<Path>,
    linear_region: impl AsRef<Path>,
) -> Result<RegionPayloadComparison> {
    let mca = read_mca_region_payloads(mca_region.as_ref())?;
    let linear = read_linear_region_payloads(linear_region.as_ref())?;

    let mut compared_chunks = 0usize;
    let mut matching_chunks = 0usize;
    let mut mismatched_chunks = 0usize;
    let mut missing_in_mca = 0usize;
    let mut missing_in_linear = 0usize;
    let mut first_mismatch = None;

    for z in 0..REGION_CHUNK_WIDTH {
        for x in 0..REGION_CHUNK_WIDTH {
            let pos = ChunkLocalPos::new(x, z)?;
            let mca_payload = mca.chunks.get(&pos);
            let linear_payload = linear.chunks.get(&pos);
            if mca_payload.is_none() && linear_payload.is_none() {
                continue;
            }
            compared_chunks += 1;
            match (mca_payload, linear_payload) {
                (Some(left), Some(right)) if left == right => {
                    matching_chunks += 1;
                }
                (Some(_), Some(_)) => {
                    mismatched_chunks += 1;
                    first_mismatch.get_or_insert(pos);
                }
                (None, Some(_)) => {
                    missing_in_mca += 1;
                    first_mismatch.get_or_insert(pos);
                }
                (Some(_), None) => {
                    missing_in_linear += 1;
                    first_mismatch.get_or_insert(pos);
                }
                (None, None) => unreachable!("empty positions are skipped"),
            }
        }
    }

    Ok(RegionPayloadComparison {
        compared_chunks,
        matching_chunks,
        mismatched_chunks,
        missing_in_mca,
        missing_in_linear,
        first_mismatch,
    })
}

pub fn write_mca_region(
    path: impl AsRef<Path>,
    chunk_payloads: &BTreeMap<ChunkLocalPos, Vec<u8>>,
    timestamp: i32,
) -> Result<()> {
    write_mca_region_with_compression(
        path,
        chunk_payloads,
        timestamp,
        MCA_DEFAULT_COMPRESSION_LEVEL,
    )
}

pub fn write_mca_region_with_compression(
    path: impl AsRef<Path>,
    chunk_payloads: &BTreeMap<ChunkLocalPos, Vec<u8>>,
    timestamp: i32,
    compression_level: u32,
) -> Result<()> {
    if compression_level > 9 {
        return Err(RegionError::Invalid(
            "mcaCompression must be in the zlib range 0..9".to_string(),
        ));
    }
    let path = path.as_ref();
    let mut entries = Vec::new();
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(compression_level));
    for (pos, payload) in chunk_payloads {
        validate_payload(*pos, payload)?;
        encoder.write_all(payload)?;
        // Finish an independent stream and reset its dictionary/checksum.
        // Reuse zlib's working memory across the region's 1024 chunks.
        let compressed = encoder.reset(Vec::new())?;
        let chunk_length = compressed
            .len()
            .checked_add(1)
            .ok_or_else(|| RegionError::Invalid(format!("chunk length overflow for {pos:?}")))?;
        let sectors = sectors_for(4 + chunk_length)?;
        if sectors > 255 {
            return Err(RegionError::Invalid(format!(
                "chunk exceeds MCA sector count byte for {pos:?}"
            )));
        }
        entries.push(McaChunkEntry {
            pos: *pos,
            compressed_payload: compressed,
            chunk_length,
            sectors,
            offset_sector: 0,
        });
    }
    entries.sort_by_key(|entry| entry.pos.header_index());

    let mut next_sector = MCA_HEADER_SECTORS;
    for entry in &mut entries {
        entry.offset_sector = next_sector;
        next_sector += entry.sectors;
        if entry.offset_sector > 0xFF_FFFF {
            return Err(RegionError::Invalid(
                "MCA offset exceeds 24-bit sector address".to_string(),
            ));
        }
    }

    let mut region = vec![0u8; next_sector * MCA_SECTOR_BYTES];
    for entry in &entries {
        let location = ((entry.offset_sector as u32) << 8) | (entry.sectors as u32);
        write_u32_be_at(&mut region, entry.pos.header_index() * 4, location)?;
        write_i32_be_at(
            &mut region,
            MCA_SECTOR_BYTES + (entry.pos.header_index() * 4),
            timestamp,
        )?;

        let chunk_start = entry.offset_sector * MCA_SECTOR_BYTES;
        let chunk_length = u32::try_from(entry.chunk_length).map_err(|_| {
            RegionError::Invalid(format!("MCA chunk length exceeds u32 for {:?}", entry.pos))
        })?;
        write_u32_be_at(&mut region, chunk_start, chunk_length)?;
        region[chunk_start + 4] = MCA_COMPRESSION_ZLIB;
        let payload_start = chunk_start + 5;
        region[payload_start..payload_start + entry.compressed_payload.len()]
            .copy_from_slice(&entry.compressed_payload);
    }

    write_file(path, &region)
}

pub fn write_linear_v2_region(
    path: impl AsRef<Path>,
    chunk_payloads: &BTreeMap<ChunkLocalPos, Vec<u8>>,
    timestamp: i64,
) -> Result<()> {
    write_linear_v2_region_with_compression(
        path,
        chunk_payloads,
        timestamp,
        LINEAR_DEFAULT_COMPRESSION_LEVEL,
    )
}

pub fn write_linear_v2_region_with_compression(
    path: impl AsRef<Path>,
    chunk_payloads: &BTreeMap<ChunkLocalPos, Vec<u8>>,
    timestamp: i64,
    compression_level: i32,
) -> Result<()> {
    if !(1..=22).contains(&compression_level) {
        return Err(RegionError::Invalid(
            "compressionLevel must be in the DivineMC-safe range 1..22".to_string(),
        ));
    }
    let path = path.as_ref();
    let (region_x, region_z) = parse_region_coordinates(path, "linear")?;
    let buckets = build_linear_buckets(chunk_payloads, timestamp, compression_level)?;
    let existence = chunk_existence(chunk_payloads)?;

    let mut data = Vec::new();
    data.extend_from_slice(&LINEAR_SUPERBLOCK.to_be_bytes());
    data.push(LINEAR_VERSION);
    data.extend_from_slice(&timestamp.to_be_bytes());
    data.push(LINEAR_GRID_SIZE);
    data.extend_from_slice(&region_x.to_be_bytes());
    data.extend_from_slice(&region_z.to_be_bytes());
    write_linear_existence_bitmap(&mut data, &existence);
    data.push(0);

    for bucket in &buckets {
        let bucket_size = bucket.as_ref().map_or(0, Vec::len);
        let bucket_size = i32::try_from(bucket_size)
            .map_err(|_| RegionError::Invalid("Linear bucket exceeds i32 size".to_string()))?;
        data.extend_from_slice(&bucket_size.to_be_bytes());
        data.push(
            u8::try_from(compression_level)
                .expect("validated compression level fits in unsigned byte"),
        );
        let bucket_hash = bucket.as_ref().map_or(0, |bytes| xxh64(bytes, 0) as i64);
        data.extend_from_slice(&bucket_hash.to_be_bytes());
    }
    for bytes in buckets.iter().flatten() {
        data.extend_from_slice(bytes);
    }
    data.extend_from_slice(&LINEAR_SUPERBLOCK.to_be_bytes());

    write_file(path, &data)
}

pub fn read_region_payloads(path: impl AsRef<Path>) -> Result<RegionPayloads> {
    let path = path.as_ref();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            RegionError::Invalid(format!("region path has no file name: {}", path.display()))
        })?;
    if file_name.ends_with(".mca") {
        read_mca_region_payloads(path)
    } else if file_name.ends_with(".linear") {
        read_linear_region_payloads(path)
    } else {
        Err(RegionError::Invalid(format!(
            "unsupported region extension: {}",
            path.display()
        )))
    }
}

pub fn validate_region_file_for_resume(
    path: impl AsRef<Path>,
    expected_format: RegionFormat,
    expected_region_x: i32,
    expected_region_z: i32,
    expected_chunks: usize,
) -> Result<RegionResumeValidation> {
    let path = path.as_ref();
    let (region_x, region_z) = parse_region_coordinates(path, expected_format.extension())?;
    if region_x != expected_region_x || region_z != expected_region_z {
        return Err(RegionError::Invalid(format!(
            "region coordinates differ from expected: file={region_x},{region_z} expected={expected_region_x},{expected_region_z}"
        )));
    }
    let validation = match expected_format {
        RegionFormat::Mca => {
            validate_mca_region_for_resume(path, region_x, region_z, expected_chunks)
        }
        RegionFormat::Linear => {
            validate_linear_region_for_resume(path, region_x, region_z, expected_chunks)
        }
    }?;
    validate_region_payloads_for_resume(
        path,
        expected_format,
        expected_region_x,
        expected_region_z,
        expected_chunks,
    )?;
    Ok(validation)
}

fn validate_region_payloads_for_resume(
    path: &Path,
    expected_format: RegionFormat,
    expected_region_x: i32,
    expected_region_z: i32,
    expected_chunks: usize,
) -> Result<()> {
    let payloads = match expected_format {
        RegionFormat::Mca => read_mca_region_payloads(path),
        RegionFormat::Linear => read_linear_region_payloads(path),
    }?;
    if payloads.format != expected_format {
        return Err(RegionError::Invalid(format!(
            "region payload format differs from expected: file={:?} expected={:?}",
            payloads.format, expected_format
        )));
    }
    if payloads.region_x != expected_region_x || payloads.region_z != expected_region_z {
        return Err(RegionError::Invalid(format!(
            "region payload coordinates differ from expected: file={},{} expected={},{}",
            payloads.region_x, payloads.region_z, expected_region_x, expected_region_z
        )));
    }
    validate_expected_chunk_count(payloads.chunks.len(), expected_chunks)?;
    for (pos, payload) in &payloads.chunks {
        validate_payload(*pos, payload)?;
    }
    Ok(())
}

pub fn sectors_for(byte_count: usize) -> Result<usize> {
    if byte_count == 0 {
        return Err(RegionError::Invalid(
            "byteCount must be positive".to_string(),
        ));
    }
    Ok(byte_count.div_ceil(MCA_SECTOR_BYTES))
}

#[cfg(test)]
fn zlib(payload: &[u8], compression_level: u32) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(compression_level));
    encoder.write_all(payload)?;
    Ok(encoder.finish()?)
}

fn validate_payload(pos: ChunkLocalPos, payload: &[u8]) -> Result<()> {
    if payload.is_empty() {
        return Err(RegionError::Invalid(format!(
            "chunk payload must not be empty for {pos:?}"
        )));
    }
    Ok(())
}

fn chunk_existence(chunk_payloads: &BTreeMap<ChunkLocalPos, Vec<u8>>) -> Result<Vec<bool>> {
    let mut existence = vec![false; LINEAR_CHUNKS_PER_REGION];
    for (pos, payload) in chunk_payloads {
        validate_payload(*pos, payload)?;
        existence[pos.header_index()] = true;
    }
    Ok(existence)
}

fn build_linear_buckets(
    chunk_payloads: &BTreeMap<ChunkLocalPos, Vec<u8>>,
    timestamp: i64,
    compression_level: i32,
) -> Result<Vec<Option<Vec<u8>>>> {
    let cell_count = usize::from(REGION_CHUNK_WIDTH) / usize::from(LINEAR_GRID_SIZE);
    // Bucket indices retain the Java X-major order. Collect results first so
    // parallel completion cannot change which invalid payload is reported.
    (0..LINEAR_BUCKET_COUNT)
        .into_par_iter()
        .map(|index| {
            build_linear_bucket(
                chunk_payloads,
                timestamp,
                compression_level,
                cell_count,
                index / usize::from(LINEAR_GRID_SIZE),
                index % usize::from(LINEAR_GRID_SIZE),
            )
        })
        .collect::<Vec<_>>()
        .into_iter()
        .collect()
}

fn build_linear_bucket(
    chunk_payloads: &BTreeMap<ChunkLocalPos, Vec<u8>>,
    timestamp: i64,
    compression_level: i32,
    cell_count: usize,
    bucket_x: usize,
    bucket_z: usize,
) -> Result<Option<Vec<u8>>> {
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), compression_level)?;
    let mut has_data = false;
    for cell_x in 0..cell_count {
        for cell_z in 0..cell_count {
            let chunk_x = bucket_x * cell_count + cell_x;
            let chunk_z = bucket_z * cell_count + cell_z;
            let pos = ChunkLocalPos::new(
                u8::try_from(chunk_x).expect("chunk x fits u8"),
                u8::try_from(chunk_z).expect("chunk z fits u8"),
            )?;
            if let Some(payload) = chunk_payloads.get(&pos) {
                validate_payload(pos, payload)?;
                let data_size = payload.len().checked_add(8).ok_or_else(|| {
                    RegionError::Invalid("Linear payload size overflow".to_string())
                })?;
                let data_size = i32::try_from(data_size).map_err(|_| {
                    RegionError::Invalid(format!("Linear payload exceeds i32 size for {pos:?}"))
                })?;
                has_data = true;
                encoder.write_all(&data_size.to_be_bytes())?;
                encoder.write_all(&timestamp.to_be_bytes())?;
                encoder.write_all(payload)?;
            } else {
                encoder.write_all(&0i32.to_be_bytes())?;
                encoder.write_all(&0i64.to_be_bytes())?;
            }
        }
    }
    encoder.flush()?;
    let compressed = encoder.finish()?;
    Ok(has_data.then_some(compressed))
}

fn write_linear_existence_bitmap(output: &mut Vec<u8>, existence: &[bool]) {
    for byte_index in 0..128 {
        let mut value = 0u8;
        for bit in 0..8 {
            if existence[byte_index * 8 + bit] {
                value |= 1 << (7 - bit);
            }
        }
        output.push(value);
    }
}

fn write_u32_be_at(output: &mut [u8], offset: usize, value: u32) -> Result<()> {
    let bytes = output.get_mut(offset..offset + 4).ok_or_else(|| {
        RegionError::Invalid(format!("truncated big-endian u32 write at {offset}"))
    })?;
    bytes.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn write_i32_be_at(output: &mut [u8], offset: usize, value: i32) -> Result<()> {
    let bytes = output.get_mut(offset..offset + 4).ok_or_else(|| {
        RegionError::Invalid(format!("truncated big-endian i32 write at {offset}"))
    })?;
    bytes.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = unique_temp_file_path(path)?;
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // The temp file contents are durable before the final-name swap. Rust std
        // does not expose portable parent-directory fsync, so after a crash the
        // old or missing final file is acceptable; corrupt temp bytes under the
        // final region name are not.
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(&temp_path, path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    write_result?;
    Ok(())
}

fn unique_temp_file_path(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        RegionError::Invalid(format!("region path has no parent: {}", path.display()))
    })?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            RegionError::Invalid(format!("region path has no file name: {}", path.display()))
        })?;
    let counter = TEMP_FILE_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(parent.join(format!(
        ".{file_name}.{}.{}.{}.tmp",
        std::process::id(),
        counter,
        nanos
    )))
}

fn validate_mca_region_for_resume(
    path: &Path,
    region_x: i32,
    region_z: i32,
    expected_chunks: usize,
) -> Result<RegionResumeValidation> {
    let data = fs::read(path)?;
    if data.len() < MCA_HEADER_BYTES {
        return Err(RegionError::Invalid(format!(
            "MCA file smaller than header: {}",
            data.len()
        )));
    }
    if data.len() % MCA_SECTOR_BYTES != 0 {
        return Err(RegionError::Invalid(format!(
            "MCA file is not sector aligned: {} bytes",
            data.len()
        )));
    }
    let total_sectors = data.len() / MCA_SECTOR_BYTES;
    let mut occupied_sectors = vec![false; total_sectors];
    for occupied in occupied_sectors
        .iter_mut()
        .take(MCA_HEADER_SECTORS.min(total_sectors))
    {
        *occupied = true;
    }

    let mut chunk_count = 0usize;
    for header_index in 0..LINEAR_CHUNKS_PER_REGION {
        let location = read_u32_be(&data, header_index * 4)?;
        if location == 0 {
            continue;
        }
        chunk_count += 1;
        let offset_sector = usize::try_from(location >> 8).expect("24-bit value fits usize");
        let sector_count = usize::try_from(location & 0xff).expect("8-bit value fits usize");
        if offset_sector < MCA_HEADER_SECTORS {
            return Err(RegionError::Invalid(format!(
                "MCA chunk offset points into header: {offset_sector}"
            )));
        }
        if sector_count == 0 {
            return Err(RegionError::Invalid(
                "MCA chunk has zero sectors".to_string(),
            ));
        }
        let end_sector = offset_sector
            .checked_add(sector_count)
            .ok_or_else(|| RegionError::Invalid("MCA sector range overflow".to_string()))?;
        if end_sector > total_sectors {
            return Err(RegionError::Invalid(
                "MCA chunk points beyond file sectors".to_string(),
            ));
        }
        for (sector, occupied) in occupied_sectors
            .iter_mut()
            .enumerate()
            .take(end_sector)
            .skip(offset_sector)
        {
            if *occupied {
                return Err(RegionError::Invalid(format!(
                    "MCA chunk sector overlap at sector {sector}"
                )));
            }
            *occupied = true;
        }

        let chunk_start = offset_sector * MCA_SECTOR_BYTES;
        let chunk_length =
            usize::try_from(read_u32_be(&data, chunk_start)?).expect("u32 chunk length fits usize");
        let max_chunk_length = (sector_count * MCA_SECTOR_BYTES) - 4;
        if chunk_length <= 1 || chunk_length > max_chunk_length {
            return Err(RegionError::Invalid(format!(
                "invalid MCA chunk length {chunk_length} at sector {offset_sector}"
            )));
        }
        let compression = *data
            .get(chunk_start + 4)
            .ok_or_else(|| RegionError::Invalid("truncated MCA compression byte".to_string()))?;
        if compression != MCA_COMPRESSION_ZLIB {
            return Err(RegionError::Invalid(format!(
                "unsupported MCA compression byte: {compression}"
            )));
        }
        let compressed_end = chunk_start
            .checked_add(4)
            .and_then(|offset| offset.checked_add(chunk_length))
            .ok_or_else(|| RegionError::Invalid("MCA payload range overflow".to_string()))?;
        if compressed_end > data.len() {
            return Err(RegionError::Invalid(format!(
                "MCA chunk payload points beyond file at sector {offset_sector}"
            )));
        }
    }
    validate_expected_chunk_count(chunk_count, expected_chunks)?;
    Ok(RegionResumeValidation {
        format: RegionFormat::Mca,
        region_x,
        region_z,
        chunk_count,
        file_bytes: data.len() as u64,
    })
}

fn validate_linear_region_for_resume(
    path: &Path,
    name_region_x: i32,
    name_region_z: i32,
    expected_chunks: usize,
) -> Result<RegionResumeValidation> {
    let data = fs::read(path)?;
    let mut cursor = Cursor::new(&data);

    let header_superblock = cursor.read_u64()?;
    if header_superblock != LINEAR_SUPERBLOCK {
        return Err(RegionError::Invalid(
            "invalid Linear superblock".to_string(),
        ));
    }
    let version = cursor.read_u8()?;
    if version != LINEAR_VERSION {
        return Err(RegionError::Invalid(format!(
            "unsupported Linear version: {version}"
        )));
    }
    let _timestamp = cursor.read_i64()?;
    let grid_size = cursor.read_u8()?;
    if grid_size != LINEAR_GRID_SIZE {
        return Err(RegionError::Invalid(format!(
            "unsupported Linear grid size: {grid_size}"
        )));
    }
    let region_x = cursor.read_i32()?;
    let region_z = cursor.read_i32()?;
    if region_x != name_region_x || region_z != name_region_z {
        return Err(RegionError::Invalid(format!(
            "Linear region coordinates differ from file name: header={region_x},{region_z} file={name_region_x},{name_region_z}"
        )));
    }

    let existence = read_linear_existence_bitmap(&mut cursor)?;
    let chunk_count = existence.iter().filter(|exists| **exists).count();
    validate_expected_chunk_count(chunk_count, expected_chunks)?;
    skip_linear_features(&mut cursor)?;

    let bucket_count = usize::from(grid_size) * usize::from(grid_size);
    let mut bucket_sizes = Vec::with_capacity(bucket_count);
    let mut bucket_hashes = Vec::with_capacity(bucket_count);
    for index in 0..bucket_count {
        let size = cursor.read_i32()?;
        if size < 0 {
            return Err(RegionError::Invalid(format!(
                "negative Linear bucket size at {index}"
            )));
        }
        let size = usize::try_from(size).expect("non-negative i32 fits usize");
        bucket_sizes.push(size);
        let compression_level = cursor.read_u8()?;
        if size > 0 && !(1..=22).contains(&compression_level) {
            return Err(RegionError::Invalid(format!(
                "invalid Linear bucket compression level {compression_level} at {index}"
            )));
        }
        let bucket_hash = cursor.read_i64()?;
        if size == 0 && bucket_hash != 0 {
            return Err(RegionError::Invalid(format!(
                "Linear empty bucket has nonzero hash at {index}"
            )));
        }
        bucket_hashes.push(bucket_hash);
    }

    for bucket_index in 0..bucket_count {
        let size = bucket_sizes[bucket_index];
        let bucket_has_bitmap_chunks =
            linear_bucket_has_existing_chunk(bucket_index, grid_size, &existence);
        if size == 0 {
            if bucket_has_bitmap_chunks {
                return Err(RegionError::Invalid(format!(
                    "Linear bitmap is set but bucket is empty at {bucket_index}"
                )));
            }
            continue;
        }
        if !bucket_has_bitmap_chunks {
            return Err(RegionError::Invalid(format!(
                "Linear bucket has bytes but no bitmap chunks at {bucket_index}"
            )));
        }
        let bucket = cursor.read_bytes(size)?;
        let actual_hash = xxh64(bucket, 0) as i64;
        if actual_hash != bucket_hashes[bucket_index] {
            return Err(RegionError::Invalid(format!(
                "Linear bucket hash mismatch at {bucket_index}"
            )));
        }
    }

    let footer_superblock = cursor.read_u64()?;
    if footer_superblock != LINEAR_SUPERBLOCK {
        return Err(RegionError::Invalid(
            "invalid Linear footer superblock".to_string(),
        ));
    }
    if cursor.remaining() != 0 {
        return Err(RegionError::Invalid(format!(
            "unexpected trailing Linear bytes: {}",
            cursor.remaining()
        )));
    }

    Ok(RegionResumeValidation {
        format: RegionFormat::Linear,
        region_x,
        region_z,
        chunk_count,
        file_bytes: data.len() as u64,
    })
}

fn validate_expected_chunk_count(actual: usize, expected: usize) -> Result<()> {
    if actual != expected {
        return Err(RegionError::Invalid(format!(
            "region has {actual} chunks, expected {expected}"
        )));
    }
    Ok(())
}

fn linear_bucket_has_existing_chunk(
    bucket_index: usize,
    grid_size: u8,
    existence: &[bool],
) -> bool {
    let cell_count = 32 / usize::from(grid_size);
    let bucket_x = bucket_index / usize::from(grid_size);
    let bucket_z = bucket_index % usize::from(grid_size);
    for cell_x in 0..cell_count {
        for cell_z in 0..cell_count {
            let chunk_x = bucket_x * cell_count + cell_x;
            let chunk_z = bucket_z * cell_count + cell_z;
            let header_index = chunk_x + (chunk_z * usize::from(REGION_CHUNK_WIDTH));
            if existence.get(header_index).copied().unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

struct McaChunkEntry {
    pos: ChunkLocalPos,
    compressed_payload: Vec<u8>,
    chunk_length: usize,
    sectors: usize,
    offset_sector: usize,
}

fn read_mca_region_payloads(path: &Path) -> Result<RegionPayloads> {
    let (region_x, region_z) = parse_region_coordinates(path, "mca")?;
    let data = std::fs::read(path)?;
    if data.len() < MCA_HEADER_BYTES {
        return Err(RegionError::Invalid(format!(
            "MCA file smaller than header: {}",
            data.len()
        )));
    }

    let total_sectors = data.len().div_ceil(MCA_SECTOR_BYTES);
    let mut chunks = BTreeMap::new();
    for header_index in 0..LINEAR_CHUNKS_PER_REGION {
        let location = read_u32_be(&data, header_index * 4)?;
        if location == 0 {
            continue;
        }
        let offset_sector = usize::try_from(location >> 8).expect("24-bit value fits usize");
        let sector_count = usize::try_from(location & 0xff).expect("8-bit value fits usize");
        if offset_sector < MCA_HEADER_SECTORS {
            return Err(RegionError::Invalid(format!(
                "MCA chunk offset points into header: {offset_sector}"
            )));
        }
        if sector_count == 0 {
            return Err(RegionError::Invalid(
                "MCA chunk has zero sectors".to_string(),
            ));
        }
        if offset_sector + sector_count > total_sectors {
            return Err(RegionError::Invalid(
                "MCA chunk points beyond file sectors".to_string(),
            ));
        }

        let chunk_start = offset_sector * MCA_SECTOR_BYTES;
        let chunk_length = usize::try_from(read_u32_be(&data, chunk_start)?)
            .map_err(|_| RegionError::Invalid("negative MCA chunk length".to_string()))?;
        let max_chunk_length = (sector_count * MCA_SECTOR_BYTES) - 4;
        if chunk_length <= 1 || chunk_length > max_chunk_length {
            return Err(RegionError::Invalid(format!(
                "invalid MCA chunk length {chunk_length} at sector {offset_sector}"
            )));
        }
        let compression = *data
            .get(chunk_start + 4)
            .ok_or_else(|| RegionError::Invalid("truncated MCA compression byte".to_string()))?;
        if compression != MCA_COMPRESSION_ZLIB {
            return Err(RegionError::Invalid(format!(
                "unsupported MCA compression byte: {compression}"
            )));
        }
        let compressed_start = chunk_start + 5;
        let compressed_end = chunk_start + 4 + chunk_length;
        if compressed_end > data.len() {
            return Err(RegionError::Invalid(format!(
                "MCA chunk payload points beyond file at sector {offset_sector}"
            )));
        }
        let local_x = u8::try_from(header_index % 32).expect("local x fits u8");
        let local_z = u8::try_from(header_index / 32).expect("local z fits u8");
        let mut decoder = ZlibDecoder::new(&data[compressed_start..compressed_end]);
        let mut payload = Vec::new();
        decoder.read_to_end(&mut payload).map_err(|error| {
            RegionError::Invalid(format!(
                "invalid MCA zlib payload at {local_x},{local_z}: {error}"
            ))
        })?;
        chunks.insert(ChunkLocalPos::new(local_x, local_z)?, payload);
    }

    Ok(RegionPayloads {
        format: RegionFormat::Mca,
        region_x,
        region_z,
        chunks,
    })
}

fn read_linear_region_payloads(path: &Path) -> Result<RegionPayloads> {
    let (name_region_x, name_region_z) = parse_region_coordinates(path, "linear")?;
    let data = std::fs::read(path)?;
    let mut cursor = Cursor::new(&data);

    let header_superblock = cursor.read_u64()?;
    if header_superblock != LINEAR_SUPERBLOCK {
        return Err(RegionError::Invalid(
            "invalid Linear superblock".to_string(),
        ));
    }
    let version = cursor.read_u8()?;
    if version != LINEAR_VERSION {
        return Err(RegionError::Invalid(format!(
            "unsupported Linear version: {version}"
        )));
    }
    let _timestamp = cursor.read_i64()?;
    let grid_size = cursor.read_u8()?;
    if grid_size != LINEAR_GRID_SIZE {
        return Err(RegionError::Invalid(format!(
            "unsupported Linear grid size: {grid_size}"
        )));
    }
    let region_x = cursor.read_i32()?;
    let region_z = cursor.read_i32()?;
    if region_x != name_region_x || region_z != name_region_z {
        return Err(RegionError::Invalid(format!(
            "Linear region coordinates differ from file name: header={region_x},{region_z} file={name_region_x},{name_region_z}"
        )));
    }

    let existence = read_linear_existence_bitmap(&mut cursor)?;
    skip_linear_features(&mut cursor)?;

    let bucket_count = usize::from(grid_size) * usize::from(grid_size);
    let mut bucket_sizes = Vec::with_capacity(bucket_count);
    let mut bucket_hashes = Vec::with_capacity(bucket_count);
    for index in 0..bucket_count {
        let size = cursor.read_i32()?;
        if size < 0 {
            return Err(RegionError::Invalid(format!(
                "negative Linear bucket size at {index}"
            )));
        }
        bucket_sizes.push(usize::try_from(size).expect("non-negative i32 fits usize"));
        let _compression_level = cursor.read_u8()?;
        bucket_hashes.push(cursor.read_i64()?);
    }

    let mut chunks = BTreeMap::new();
    for bucket_index in 0..bucket_count {
        let size = bucket_sizes[bucket_index];
        if size == 0 {
            validate_empty_linear_bucket(bucket_index, grid_size, &existence)?;
            continue;
        }
        let bucket = cursor.read_bytes(size)?.to_vec();
        let actual_hash = xxh64(&bucket, 0) as i64;
        if actual_hash != bucket_hashes[bucket_index] {
            return Err(RegionError::Invalid(format!(
                "Linear bucket hash mismatch at {bucket_index}"
            )));
        }
        read_linear_bucket(&bucket, grid_size, bucket_index, &existence, &mut chunks)?;
    }

    let footer_superblock = cursor.read_u64()?;
    if footer_superblock != LINEAR_SUPERBLOCK {
        return Err(RegionError::Invalid(
            "invalid Linear footer superblock".to_string(),
        ));
    }
    if cursor.remaining() != 0 {
        return Err(RegionError::Invalid(format!(
            "unexpected trailing Linear bytes: {}",
            cursor.remaining()
        )));
    }

    Ok(RegionPayloads {
        format: RegionFormat::Linear,
        region_x,
        region_z,
        chunks,
    })
}

fn read_linear_existence_bitmap(cursor: &mut Cursor<'_>) -> Result<Vec<bool>> {
    let mut existence = vec![false; LINEAR_CHUNKS_PER_REGION];
    for byte_index in 0..128 {
        let value = cursor.read_u8()?;
        for bit in 0..8 {
            existence[byte_index * 8 + bit] = ((value >> (7 - bit)) & 1) == 1;
        }
    }
    Ok(existence)
}

fn skip_linear_features(cursor: &mut Cursor<'_>) -> Result<()> {
    loop {
        let name_length = usize::from(cursor.read_u8()?);
        if name_length == 0 {
            return Ok(());
        }
        cursor.skip(name_length + 4)?;
    }
}

fn read_linear_bucket(
    bucket: &[u8],
    grid_size: u8,
    bucket_index: usize,
    existence: &[bool],
    chunks: &mut BTreeMap<ChunkLocalPos, Vec<u8>>,
) -> Result<()> {
    let mut decoder = zstd::stream::read::Decoder::new(bucket).map_err(|error| {
        RegionError::Invalid(format!(
            "invalid Linear zstd bucket at {bucket_index}: {error}"
        ))
    })?;
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).map_err(|error| {
        RegionError::Invalid(format!(
            "invalid Linear zstd bucket at {bucket_index}: {error}"
        ))
    })?;
    let mut cursor = Cursor::new(&decompressed);
    let cell_count = 32 / usize::from(grid_size);
    let bucket_x = bucket_index / usize::from(grid_size);
    let bucket_z = bucket_index % usize::from(grid_size);

    for cell_x in 0..cell_count {
        for cell_z in 0..cell_count {
            let chunk_x = bucket_x * cell_count + cell_x;
            let chunk_z = bucket_z * cell_count + cell_z;
            let data_size = cursor.read_i32()?;
            let _timestamp = cursor.read_i64()?;
            if data_size < 0 {
                return Err(RegionError::Invalid(format!(
                    "negative Linear chunk data size at {chunk_x},{chunk_z}"
                )));
            }
            let header_index = chunk_x + (chunk_z * usize::from(REGION_CHUNK_WIDTH));
            let bitmap_exists = existence.get(header_index).copied().ok_or_else(|| {
                RegionError::Invalid(format!(
                    "Linear bitmap index out of range at {chunk_x},{chunk_z}"
                ))
            })?;
            if data_size > 0 {
                if !bitmap_exists {
                    return Err(RegionError::Invalid(format!(
                        "Linear payload exists but bitmap is unset at {chunk_x},{chunk_z}"
                    )));
                }
                let payload_size = data_size - 8;
                if payload_size <= 0 {
                    return Err(RegionError::Invalid(format!(
                        "invalid Linear chunk payload size at {chunk_x},{chunk_z}"
                    )));
                }
                let payload = cursor
                    .read_bytes(usize::try_from(payload_size).expect("positive i32 fits usize"))?
                    .to_vec();
                chunks.insert(
                    ChunkLocalPos::new(
                        u8::try_from(chunk_x).expect("chunk x fits u8"),
                        u8::try_from(chunk_z).expect("chunk z fits u8"),
                    )?,
                    payload,
                );
            } else if bitmap_exists {
                return Err(RegionError::Invalid(format!(
                    "Linear bitmap is set but payload is missing at {chunk_x},{chunk_z}"
                )));
            }
        }
    }
    if cursor.remaining() != 0 {
        return Err(RegionError::Invalid(format!(
            "extra decompressed Linear bucket data at {bucket_index}"
        )));
    }
    Ok(())
}

fn validate_empty_linear_bucket(
    bucket_index: usize,
    grid_size: u8,
    existence: &[bool],
) -> Result<()> {
    let cell_count = 32 / usize::from(grid_size);
    let bucket_x = bucket_index / usize::from(grid_size);
    let bucket_z = bucket_index % usize::from(grid_size);
    for cell_x in 0..cell_count {
        for cell_z in 0..cell_count {
            let chunk_x = bucket_x * cell_count + cell_x;
            let chunk_z = bucket_z * cell_count + cell_z;
            let header_index = chunk_x + (chunk_z * usize::from(REGION_CHUNK_WIDTH));
            if existence.get(header_index).copied().unwrap_or(false) {
                return Err(RegionError::Invalid(format!(
                    "Linear bitmap is set but bucket is empty at {chunk_x},{chunk_z}"
                )));
            }
        }
    }
    Ok(())
}

fn parse_region_coordinates(path: &Path, extension: &str) -> Result<(i32, i32)> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            RegionError::Invalid(format!("region path has no file name: {}", path.display()))
        })?;
    let parts = file_name.split('.').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "r" || parts[3] != extension {
        return Err(RegionError::Invalid(format!(
            "region file name must be r.<x>.<z>.{extension}: {file_name}"
        )));
    }
    let x = parts[1].parse::<i32>().map_err(|error| {
        RegionError::Invalid(format!("invalid region X in {file_name}: {error}"))
    })?;
    let z = parts[2].parse::<i32>().map_err(|error| {
        RegionError::Invalid(format!("invalid region Z in {file_name}: {error}"))
    })?;
    Ok((x, z))
}

fn read_u32_be(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| RegionError::Invalid(format!("truncated big-endian u32 at {offset}")))?;
    Ok(u32::from_be_bytes(
        bytes.try_into().expect("slice length is 4"),
    ))
}

struct Cursor<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.offset)
    }

    fn read_u8(&mut self) -> Result<u8> {
        let value = *self
            .data
            .get(self.offset)
            .ok_or_else(|| RegionError::Invalid(format!("truncated byte at {}", self.offset)))?;
        self.offset += 1;
        Ok(value)
    }

    fn read_i32(&mut self) -> Result<i32> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_be_bytes(
            bytes.try_into().expect("slice length is 4"),
        ))
    }

    fn read_i64(&mut self) -> Result<i64> {
        let bytes = self.read_bytes(8)?;
        Ok(i64::from_be_bytes(
            bytes.try_into().expect("slice length is 8"),
        ))
    }

    fn read_u64(&mut self) -> Result<u64> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_be_bytes(
            bytes.try_into().expect("slice length is 8"),
        ))
    }

    fn read_bytes(&mut self, count: usize) -> Result<&'a [u8]> {
        let bytes = self
            .data
            .get(self.offset..self.offset + count)
            .ok_or_else(|| RegionError::Invalid(format!("truncated bytes at {}", self.offset)))?;
        self.offset += count;
        Ok(bytes)
    }

    fn skip(&mut self, count: usize) -> Result<()> {
        self.read_bytes(count).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use xxhash_rust::xxh64::xxh64;

    #[test]
    fn chunk_local_pos_orders_like_java_header_index() {
        let mut positions = [
            ChunkLocalPos::new(0, 1).unwrap(),
            ChunkLocalPos::new(31, 0).unwrap(),
            ChunkLocalPos::new(1, 0).unwrap(),
            ChunkLocalPos::new(0, 0).unwrap(),
        ];

        positions.sort();

        assert_eq!(
            positions,
            [
                ChunkLocalPos::new(0, 0).unwrap(),
                ChunkLocalPos::new(1, 0).unwrap(),
                ChunkLocalPos::new(31, 0).unwrap(),
                ChunkLocalPos::new(0, 1).unwrap(),
            ]
        );
    }

    #[test]
    fn sectors_for_matches_java_cases() {
        assert!(sectors_for(0).is_err());
        assert_eq!(sectors_for(1).unwrap(), 1);
        assert_eq!(sectors_for(4096).unwrap(), 1);
        assert_eq!(sectors_for(4097).unwrap(), 2);
    }

    #[test]
    fn region_compression_preserves_bytes_and_error_order_across_thread_counts() {
        let single = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let parallel = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let mut payloads = full_region_payloads();
        payloads.insert(
            ChunkLocalPos::new(31, 31).unwrap(),
            b"mixed stone/dirt/grass palette".repeat(4096),
        );
        payloads.insert(
            ChunkLocalPos::new(0, 1).unwrap(),
            (0..131071).map(|index| (index * 31) as u8).collect(),
        );
        for (format, levels) in [
            (RegionFormat::Mca, vec![0, 1, 6, 9]),
            (RegionFormat::Linear, vec![1, 4, 9]),
        ] {
            let extension = format.extension();
            let first = temp_region_path(&format!("r.3.-4.{extension}"));
            let second = temp_region_path(&format!("r.3.-4.{extension}"));
            for level in levels {
                let write = |path: &Path, payloads: &BTreeMap<ChunkLocalPos, Vec<u8>>| match format
                {
                    RegionFormat::Mca => {
                        write_mca_region_with_compression(path, payloads, 42, level as u32)
                    }
                    RegionFormat::Linear => {
                        write_linear_v2_region_with_compression(path, payloads, 42, level)
                    }
                };
                single.install(|| write(&first, &payloads)).unwrap();
                parallel.install(|| write(&second, &payloads)).unwrap();
                assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
                assert_eq!(read_region_payloads(&second).unwrap().chunks, payloads);
                if format == RegionFormat::Mca {
                    let bytes = fs::read(&second).unwrap();
                    for (pos, payload) in &payloads {
                        let offset = (read_u32_be(&bytes, pos.header_index() * 4).unwrap() >> 8)
                            as usize
                            * MCA_SECTOR_BYTES;
                        let length = read_u32_be(&bytes, offset).unwrap() as usize;
                        // Compare compressed bytes with a fresh encoder, not
                        // just the decompressed content of the reused stream.
                        assert_eq!(
                            &bytes[offset + 5..offset + 4 + length],
                            zlib(payload, level as u32).unwrap()
                        );
                    }
                }
                let mut invalid = payloads.clone();
                invalid.insert(ChunkLocalPos::new(1, 0).unwrap(), Vec::new());
                invalid.insert(ChunkLocalPos::new(0, 1).unwrap(), Vec::new());
                let expected = single
                    .install(|| write(&first, &invalid))
                    .unwrap_err()
                    .to_string();
                let actual = parallel
                    .install(|| write(&second, &invalid))
                    .unwrap_err()
                    .to_string();
                assert_eq!(actual, expected);
            }
            fs::remove_file(first).unwrap();
            fs::remove_file(second).unwrap();
        }
    }

    #[test]
    fn writes_mca_region_round_trip_payloads() {
        let path = temp_region_path("r.0.0.mca");
        let payloads = sample_payloads();

        write_mca_region(&path, &payloads, 42).expect("MCA region should write");
        let region = read_region_payloads(&path).expect("MCA region should read");

        assert_eq!(region.format, RegionFormat::Mca);
        assert_eq!(region.region_x, 0);
        assert_eq!(region.region_z, 0);
        assert_eq!(region.chunks, payloads);

        let bytes = std::fs::read(&path).unwrap();
        let timestamp_offset =
            MCA_SECTOR_BYTES + (ChunkLocalPos::new(31, 0).unwrap().header_index() * 4);
        assert_eq!(read_u32_be(&bytes, timestamp_offset).unwrap(), 42);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn writes_mca_region_with_custom_compression_round_trip_payloads() {
        let path = temp_region_path("r.1.0.mca");
        let payloads = sample_payloads();

        write_mca_region_with_compression(&path, &payloads, 42, 9)
            .expect("MCA region should write with custom compression");
        let region = read_region_payloads(&path).expect("MCA region should read");

        assert_eq!(region.format, RegionFormat::Mca);
        assert_eq!(region.region_x, 1);
        assert_eq!(region.region_z, 0);
        assert_eq!(region.chunks, payloads);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn validates_complete_mca_region_for_resume_with_payload_decode() {
        let path = temp_region_path("r.4.-7.mca");
        let payloads = full_region_payloads();

        write_mca_region_with_compression(&path, &payloads, 42, 1)
            .expect("complete MCA region should write");
        let validation = validate_region_file_for_resume(
            &path,
            RegionFormat::Mca,
            4,
            -7,
            REGION_CHUNKS_PER_REGION,
        )
        .expect("complete MCA region should validate");

        assert_eq!(validation.format, RegionFormat::Mca);
        assert_eq!(validation.region_x, 4);
        assert_eq!(validation.region_z, -7);
        assert_eq!(validation.chunk_count, REGION_CHUNKS_PER_REGION);
        assert!(validation.file_bytes >= MCA_HEADER_BYTES as u64);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_mca_compression_outside_zlib_range() {
        let error = write_mca_region_with_compression(
            temp_region_path("r.0.1.mca"),
            &sample_payloads(),
            42,
            10,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("0..9"));
    }

    #[test]
    fn writes_linear_v2_region_round_trip_payloads() {
        let path = temp_region_path("r.-2.3.linear");
        let payloads = sample_payloads();

        write_linear_v2_region(&path, &payloads, 42).expect("Linear region should write");
        let region = read_region_payloads(&path).expect("Linear region should read");

        assert_eq!(region.format, RegionFormat::Linear);
        assert_eq!(region.region_x, -2);
        assert_eq!(region.region_z, 3);
        assert_eq!(region.chunks, payloads);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn validates_complete_linear_region_for_resume_with_payload_decode() {
        let path = temp_region_path("r.-8.9.linear");
        let payloads = full_region_payloads();

        write_linear_v2_region_with_compression(&path, &payloads, 42, 1)
            .expect("complete Linear region should write");
        let validation = validate_region_file_for_resume(
            &path,
            RegionFormat::Linear,
            -8,
            9,
            REGION_CHUNKS_PER_REGION,
        )
        .expect("complete Linear region should validate");

        assert_eq!(validation.format, RegionFormat::Linear);
        assert_eq!(validation.region_x, -8);
        assert_eq!(validation.region_z, 9);
        assert_eq!(validation.chunk_count, REGION_CHUNKS_PER_REGION);
        assert!(validation.file_bytes > 0);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn atomic_region_write_replaces_existing_final_and_leaves_no_temp_file() {
        let path = temp_region_path("r.5.6.linear");
        let first = sample_payloads();
        let second = full_region_payloads();

        write_linear_v2_region(&path, &first, 42).expect("first Linear region should write");
        write_linear_v2_region_with_compression(&path, &second, 43, 1)
            .expect("second Linear region should replace the first");

        let validation = validate_region_file_for_resume(
            &path,
            RegionFormat::Linear,
            5,
            6,
            REGION_CHUNKS_PER_REGION,
        )
        .expect("replacement final region should validate");
        assert_eq!(validation.chunk_count, REGION_CHUNKS_PER_REGION);

        let temp_count = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"))
            })
            .count();
        assert_eq!(temp_count, 0);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn resume_validation_rejects_incomplete_region() {
        let path = temp_region_path("r.0.0.linear");
        write_linear_v2_region(&path, &sample_payloads(), 42).expect("Linear region should write");

        let error = validate_region_file_for_resume(
            &path,
            RegionFormat::Linear,
            0,
            0,
            REGION_CHUNKS_PER_REGION,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("expected 1024"), "error={error}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn resume_validation_rejects_wrong_coordinates_and_zero_byte_files() {
        let wrong_coord = temp_region_path("r.1.2.linear");
        write_linear_v2_region(&wrong_coord, &full_region_payloads(), 42)
            .expect("Linear region should write");

        let error = validate_region_file_for_resume(
            &wrong_coord,
            RegionFormat::Linear,
            1,
            3,
            REGION_CHUNKS_PER_REGION,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("expected=1,3"), "error={error}");

        let zero = temp_region_path("r.0.0.mca");
        std::fs::create_dir_all(zero.parent().unwrap()).unwrap();
        std::fs::write(&zero, []).unwrap();
        let error = validate_region_file_for_resume(
            &zero,
            RegionFormat::Mca,
            0,
            0,
            REGION_CHUNKS_PER_REGION,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("smaller than header"), "error={error}");

        let _ = std::fs::remove_file(wrong_coord);
        let _ = std::fs::remove_file(zero);
    }

    #[test]
    fn resume_validation_rejects_linear_bucket_hash_mismatch() {
        let path = temp_region_path("r.0.0.linear");
        write_linear_v2_region_with_compression(&path, &full_region_payloads(), 42, 1)
            .expect("complete Linear region should write");
        let mut bytes = std::fs::read(&path).unwrap();

        let first_bucket_offset = 8 + 1 + 8 + 1 + 4 + 4 + 128 + 1 + (LINEAR_BUCKET_COUNT * 13);
        bytes[first_bucket_offset] ^= 0x5a;
        std::fs::write(&path, &bytes).unwrap();

        let error = validate_region_file_for_resume(
            &path,
            RegionFormat::Linear,
            0,
            0,
            REGION_CHUNKS_PER_REGION,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("hash mismatch"), "error={error}");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn resume_validation_rejects_corrupted_mca_zlib_payload() {
        let path = temp_region_path("r.0.0.mca");
        write_mca_region_with_compression(&path, &full_region_payloads(), 42, 1)
            .expect("complete MCA region should write");
        let mut bytes = std::fs::read(&path).unwrap();

        let location = read_u32_be(&bytes, 0).unwrap();
        let offset_sector = usize::try_from(location >> 8).unwrap();
        let chunk_start = offset_sector * MCA_SECTOR_BYTES;
        let compressed_start = chunk_start + 5;
        bytes[compressed_start] ^= 0x5a;
        std::fs::write(&path, &bytes).unwrap();

        let error = validate_region_file_for_resume(
            &path,
            RegionFormat::Mca,
            0,
            0,
            REGION_CHUNKS_PER_REGION,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("invalid MCA zlib payload"), "error={error}");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn resume_validation_rejects_corrupted_linear_zstd_bucket_with_matching_hash() {
        let path = temp_region_path("r.0.0.linear");
        write_linear_v2_region_with_compression(&path, &full_region_payloads(), 42, 1)
            .expect("complete Linear region should write");
        let mut bytes = std::fs::read(&path).unwrap();

        let bucket_table_offset = 8 + 1 + 8 + 1 + 4 + 4 + 128 + 1;
        let bucket_size = i32::from_be_bytes(
            bytes[bucket_table_offset..bucket_table_offset + 4]
                .try_into()
                .unwrap(),
        );
        assert!(bucket_size > 0);
        let bucket_size = usize::try_from(bucket_size).unwrap();
        let first_bucket_offset = bucket_table_offset + (LINEAR_BUCKET_COUNT * 13);
        bytes[first_bucket_offset] ^= 0x5a;
        let bucket_hash = xxh64(
            &bytes[first_bucket_offset..first_bucket_offset + bucket_size],
            0,
        ) as i64;
        let hash_offset = bucket_table_offset + 5;
        bytes[hash_offset..hash_offset + 8].copy_from_slice(&bucket_hash.to_be_bytes());
        std::fs::write(&path, &bytes).unwrap();

        let error = validate_region_file_for_resume(
            &path,
            RegionFormat::Linear,
            0,
            0,
            REGION_CHUNKS_PER_REGION,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("invalid Linear zstd bucket"),
            "error={error}"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn resume_validation_rejects_mca_chunk_pointer_beyond_file() {
        let path = temp_region_path("r.0.0.mca");
        write_mca_region_with_compression(&path, &full_region_payloads(), 42, 1)
            .expect("complete MCA region should write");
        let mut bytes = std::fs::read(&path).unwrap();

        let impossible_location = ((0x00ff_ffffu32) << 8) | 1;
        bytes[0..4].copy_from_slice(&impossible_location.to_be_bytes());
        std::fs::write(&path, &bytes).unwrap();

        let error = validate_region_file_for_resume(
            &path,
            RegionFormat::Mca,
            0,
            0,
            REGION_CHUNKS_PER_REGION,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("beyond file sectors"), "error={error}");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_linear_bitmap_payload_mismatch() {
        let path = temp_region_path("r.0.0.linear");
        write_linear_v2_region(&path, &sample_payloads(), 42).expect("Linear region should write");
        let mut bytes = std::fs::read(&path).unwrap();

        bytes[30] = 0;
        std::fs::write(&path, &bytes).unwrap();

        let error = read_region_payloads(&path).unwrap_err().to_string();
        assert!(
            error.contains("payload exists but bitmap is unset"),
            "error={error}"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_linear_bitmap_set_for_empty_bucket() {
        let path = temp_region_path("r.0.0.linear");
        let payloads = BTreeMap::from([(
            ChunkLocalPos::new(0, 0).unwrap(),
            b"synthetic-region-writer-payload-0-0".to_vec(),
        )]);
        write_linear_v2_region(&path, &payloads, 42).expect("Linear region should write");
        let mut bytes = std::fs::read(&path).unwrap();

        bytes[30 + 4] = 0x80;
        std::fs::write(&path, &bytes).unwrap();

        let error = read_region_payloads(&path).unwrap_err().to_string();
        assert!(
            error.contains("bucket is empty") || error.contains("payload is missing"),
            "error={error}"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_negative_linear_chunk_data_size() {
        let path = temp_region_path("r.0.0.linear");
        write_linear_with_negative_data_size(&path);

        let error = read_region_payloads(&path).unwrap_err().to_string();
        assert!(
            error.contains("negative Linear chunk data size"),
            "error={error}"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reads_minimal_mca_payload() {
        let path = temp_region_path("r.0.0.mca");
        let payload = b"synthetic-mca-payload";
        write_minimal_mca(&path, payload);

        let region = read_region_payloads(&path).expect("MCA payload should read");

        assert_eq!(region.format, RegionFormat::Mca);
        assert_eq!(region.region_x, 0);
        assert_eq!(region.region_z, 0);
        assert_eq!(
            region
                .chunks
                .get(&ChunkLocalPos::new(0, 0).unwrap())
                .unwrap(),
            payload
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reads_minimal_linear_payload() {
        let path = temp_region_path("r.0.0.linear");
        let payload = b"synthetic-linear-payload";
        write_minimal_linear(&path, payload);

        let region = read_region_payloads(&path).expect("Linear payload should read");

        assert_eq!(region.format, RegionFormat::Linear);
        assert_eq!(region.region_x, 0);
        assert_eq!(region.region_z, 0);
        assert_eq!(
            region
                .chunks
                .get(&ChunkLocalPos::new(0, 0).unwrap())
                .unwrap(),
            payload
        );

        let _ = std::fs::remove_file(path);
    }

    fn temp_region_path(file_name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after epoch")
            .as_nanos();
        std::env::temp_dir()
            .join("earthmap-region-tests")
            .join(nonce.to_string())
            .join(file_name)
    }

    fn sample_payloads() -> BTreeMap<ChunkLocalPos, Vec<u8>> {
        BTreeMap::from([
            (
                ChunkLocalPos::new(0, 0).unwrap(),
                b"synthetic-region-writer-payload-0-0".to_vec(),
            ),
            (
                ChunkLocalPos::new(31, 0).unwrap(),
                b"synthetic-region-writer-payload-31-0".to_vec(),
            ),
            (
                ChunkLocalPos::new(0, 1).unwrap(),
                b"synthetic-region-writer-payload-0-1".to_vec(),
            ),
        ])
    }

    fn full_region_payloads() -> BTreeMap<ChunkLocalPos, Vec<u8>> {
        let mut payloads = BTreeMap::new();
        for z in 0..REGION_CHUNK_WIDTH {
            for x in 0..REGION_CHUNK_WIDTH {
                payloads.insert(
                    ChunkLocalPos::new(x, z).unwrap(),
                    format!("synthetic-complete-region-payload-{x}-{z}").into_bytes(),
                );
            }
        }
        payloads
    }

    fn write_minimal_mca(path: &Path, payload: &[u8]) {
        let parent = path.parent().expect("temp path should have a parent");
        std::fs::create_dir_all(parent).expect("temp dir should be creatable");

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(payload)
            .expect("test payload should compress");
        let compressed = encoder.finish().expect("test zlib stream should finish");
        let chunk_length = compressed.len() + 1;
        let sectors = (4 + chunk_length).div_ceil(MCA_SECTOR_BYTES);
        let mut data = vec![0u8; (MCA_HEADER_SECTORS + sectors) * MCA_SECTOR_BYTES];
        let location = ((MCA_HEADER_SECTORS as u32) << 8) | (sectors as u32);
        data[0..4].copy_from_slice(&location.to_be_bytes());
        let chunk_start = MCA_HEADER_BYTES;
        data[chunk_start..chunk_start + 4].copy_from_slice(&(chunk_length as u32).to_be_bytes());
        data[chunk_start + 4] = MCA_COMPRESSION_ZLIB;
        data[chunk_start + 5..chunk_start + 5 + compressed.len()].copy_from_slice(&compressed);
        std::fs::write(path, data).expect("test MCA should be writable");
    }

    fn write_minimal_linear(path: &Path, payload: &[u8]) {
        let parent = path.parent().expect("temp path should have a parent");
        std::fs::create_dir_all(parent).expect("temp dir should be creatable");

        let mut bucket_plain = Vec::new();
        for cell_x in 0..4 {
            for cell_z in 0..4 {
                if cell_x == 0 && cell_z == 0 {
                    bucket_plain.extend_from_slice(&((payload.len() as i32) + 8).to_be_bytes());
                    bucket_plain.extend_from_slice(&0i64.to_be_bytes());
                    bucket_plain.extend_from_slice(payload);
                } else {
                    bucket_plain.extend_from_slice(&0i32.to_be_bytes());
                    bucket_plain.extend_from_slice(&0i64.to_be_bytes());
                }
            }
        }
        let bucket = zstd::bulk::compress(&bucket_plain, 4).expect("test bucket should compress");
        let bucket_hash = xxh64(&bucket, 0) as i64;

        let mut data = Vec::new();
        data.extend_from_slice(&LINEAR_SUPERBLOCK.to_be_bytes());
        data.push(LINEAR_VERSION);
        data.extend_from_slice(&0i64.to_be_bytes());
        data.push(LINEAR_GRID_SIZE);
        data.extend_from_slice(&0i32.to_be_bytes());
        data.extend_from_slice(&0i32.to_be_bytes());
        data.push(0x80);
        data.extend(std::iter::repeat_n(0u8, 127));
        data.push(0);
        for bucket_index in 0..64 {
            if bucket_index == 0 {
                data.extend_from_slice(&(bucket.len() as i32).to_be_bytes());
                data.push(4);
                data.extend_from_slice(&bucket_hash.to_be_bytes());
            } else {
                data.extend_from_slice(&0i32.to_be_bytes());
                data.push(0);
                data.extend_from_slice(&0i64.to_be_bytes());
            }
        }
        data.extend_from_slice(&bucket);
        data.extend_from_slice(&LINEAR_SUPERBLOCK.to_be_bytes());
        std::fs::write(path, data).expect("test Linear region should be writable");
    }

    fn write_linear_with_negative_data_size(path: &Path) {
        let parent = path.parent().expect("temp path should have a parent");
        std::fs::create_dir_all(parent).expect("temp dir should be creatable");

        let mut bucket_plain = Vec::new();
        for cell_x in 0..4 {
            for cell_z in 0..4 {
                if cell_x == 0 && cell_z == 0 {
                    bucket_plain.extend_from_slice(&(-1i32).to_be_bytes());
                } else {
                    bucket_plain.extend_from_slice(&0i32.to_be_bytes());
                }
                bucket_plain.extend_from_slice(&0i64.to_be_bytes());
            }
        }
        let bucket = zstd::bulk::compress(&bucket_plain, 4).expect("test bucket should compress");
        let bucket_hash = xxh64(&bucket, 0) as i64;

        let mut data = Vec::new();
        data.extend_from_slice(&LINEAR_SUPERBLOCK.to_be_bytes());
        data.push(LINEAR_VERSION);
        data.extend_from_slice(&0i64.to_be_bytes());
        data.push(LINEAR_GRID_SIZE);
        data.extend_from_slice(&0i32.to_be_bytes());
        data.extend_from_slice(&0i32.to_be_bytes());
        data.extend(std::iter::repeat_n(0u8, 128));
        data.push(0);
        for bucket_index in 0..64 {
            if bucket_index == 0 {
                data.extend_from_slice(&(bucket.len() as i32).to_be_bytes());
                data.push(4);
                data.extend_from_slice(&bucket_hash.to_be_bytes());
            } else {
                data.extend_from_slice(&0i32.to_be_bytes());
                data.push(0);
                data.extend_from_slice(&0i64.to_be_bytes());
            }
        }
        data.extend_from_slice(&bucket);
        data.extend_from_slice(&LINEAR_SUPERBLOCK.to_be_bytes());
        std::fs::write(path, data).expect("test Linear region should be writable");
    }
}
