#![forbid(unsafe_code)]

use earthmap_geo::EarthScaleMapping;
use flate2::read::ZlibDecoder;
use quick_xml::events::Event;
use quick_xml::Reader;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const MODULE_STATUS: &str = "phase1-rust-osm-tools";
pub const REGION_SIZE_BLOCKS: i32 = 512;
const MAX_BLOB_HEADER_BYTES: i32 = 64 * 1024;
const MAX_BOUNDED_NODES: usize = 5_000_000;
const MAX_BOUNDED_WAYS: usize = 1_000_000;
const MAX_TARGET_NODE_REFS: usize = 10_000_000;
const REGION_NODE_PADDING_BLOCKS: i32 = 64;
const DEFAULT_GRANULARITY: i64 = 100;
const NANO_DEGREES: f64 = 1.0e-9;

pub type Result<T> = std::result::Result<T, OsmError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OsmError {
    message: String,
    blob_index: Option<i32>,
    byte_offset: Option<u64>,
}

impl OsmError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            blob_index: None,
            byte_offset: None,
        }
    }

    pub fn scan(message: impl Into<String>, blob_index: i32, byte_offset: u64) -> Self {
        Self {
            message: message.into(),
            blob_index: Some(blob_index),
            byte_offset: Some(byte_offset),
        }
    }

    pub fn blob_index(&self) -> Option<i32> {
        self.blob_index
    }

    pub fn byte_offset(&self) -> Option<u64> {
        self.byte_offset
    }
}

impl std::fmt::Display for OsmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for OsmError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OsmPbfScanReport {
    pub blobs_scanned: i32,
    pub osm_header_blobs: i32,
    pub osm_data_blobs: i32,
    pub compressed_bytes: i64,
    pub decoded_bytes: i64,
    pub primitive_groups: i64,
    pub dense_nodes: i64,
    pub ways: i64,
    pub highway_ways: i64,
    pub elapsed_millis: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OsmPbfRegionExtractReport {
    pub blobs_scanned: i32,
    pub osm_data_blobs: i32,
    pub primitive_groups: i64,
    pub decoded_nodes: i64,
    pub decoded_ways: i64,
    pub index_considered_ways: i32,
    pub index_skipped_missing_node_ways: i32,
    pub indexed_features: i32,
    pub road_features: i32,
    pub waterway_features: i32,
    pub landuse_features: i32,
    pub building_features: i32,
    pub road_mask_pixels: i32,
    pub waterway_mask_pixels: i32,
    pub landuse_mask_pixels: i32,
    pub building_mask_pixels: i32,
    pub elapsed_millis: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OsmSourceIdentity {
    pub kind: String,
    pub source_path: PathBuf,
    pub file_count: i32,
    pub total_bytes: i64,
    pub sha256: String,
}

pub fn scan_pbf(path: &Path, max_blobs: i32) -> Result<OsmPbfScanReport> {
    scan_pbf_range(path, 0, max_blobs)
}

pub fn scan_pbf_range(path: &Path, skip_blobs: i32, max_blobs: i32) -> Result<OsmPbfScanReport> {
    scan_pbf_internal(path, max_blobs, skip_blobs, true, None)
}

pub fn scan_pbf_data_blocks<F>(
    path: &Path,
    max_blobs: i32,
    skip_blobs: i32,
    parse_stats: bool,
    mut consumer: F,
) -> Result<OsmPbfScanReport>
where
    F: FnMut(i32, u64, &[u8]) -> Result<()>,
{
    scan_pbf_internal(
        path,
        max_blobs,
        skip_blobs,
        parse_stats,
        Some(&mut consumer),
    )
}

fn scan_pbf_internal(
    path: &Path,
    max_blobs: i32,
    skip_blobs: i32,
    parse_primitive_stats: bool,
    mut data_block_consumer: Option<&mut dyn FnMut(i32, u64, &[u8]) -> Result<()>>,
) -> Result<OsmPbfScanReport> {
    if max_blobs <= 0 {
        return Err(OsmError::invalid("maxBlobs must be positive"));
    }
    if skip_blobs < 0 {
        return Err(OsmError::invalid("skipBlobs must be non-negative"));
    }
    let start = Instant::now();
    let mut file = File::open(path).map_err(|error| OsmError::invalid(error.to_string()))?;
    let file_size = file
        .metadata()
        .map_err(|error| OsmError::invalid(error.to_string()))?
        .len();
    let mut position = 0u64;
    let mut blobs_scanned = 0i32;
    let mut blob_index = 0i32;
    let mut osm_header_blobs = 0i32;
    let mut osm_data_blobs = 0i32;
    let mut compressed_bytes = 0i64;
    let mut decoded_bytes = 0i64;
    let mut primitive_groups = 0i64;
    let mut dense_nodes = 0i64;
    let mut ways = 0i64;
    let mut highway_ways = 0i64;

    while blobs_scanned < max_blobs && position < file_size {
        let current_blob_index = blob_index;
        let header_offset = position;
        let header_size = read_i32_be(&mut file, position).map_err(|error| {
            OsmError::scan(
                format!("truncated PBF blob header size: {error}"),
                current_blob_index,
                header_offset,
            )
        })?;
        if header_size <= 0 || header_size > MAX_BLOB_HEADER_BYTES {
            return Err(OsmError::scan(
                format!("invalid PBF BlobHeader size {header_size}"),
                current_blob_index,
                header_offset,
            ));
        }
        position += 4;
        let result = (|| -> Result<()> {
            let header_bytes = read_exact_at(&mut file, position, header_size as usize)?;
            position += header_size as u64;
            let header = parse_blob_header(&header_bytes)?;
            if blob_index < skip_blobs {
                blob_index += 1;
                position += header.data_size as u64;
                return Ok(());
            }
            blob_index += 1;
            let blob_bytes = read_exact_at(&mut file, position, header.data_size as usize)?;
            position += header.data_size as u64;
            compressed_bytes += blob_bytes.len() as i64;
            let decoded = decode_blob(&blob_bytes)?;
            decoded_bytes += decoded.len() as i64;
            match header.blob_type.as_str() {
                "OSMHeader" => osm_header_blobs += 1,
                "OSMData" => {
                    osm_data_blobs += 1;
                    if let Some(consumer) = &mut data_block_consumer {
                        consumer(current_blob_index, header_offset, &decoded)?;
                    }
                    if parse_primitive_stats {
                        let stats = parse_primitive_stats_block(&decoded)?;
                        primitive_groups += stats.primitive_groups;
                        dense_nodes += stats.dense_nodes;
                        ways += stats.ways;
                        highway_ways += stats.highway_ways;
                    }
                }
                _ => {}
            }
            Ok(())
        })();
        if let Err(error) = result {
            if error.blob_index.is_some() {
                return Err(error);
            }
            return Err(OsmError::scan(
                error.to_string(),
                current_blob_index,
                header_offset,
            ));
        }
        if current_blob_index >= skip_blobs {
            blobs_scanned += 1;
        }
    }
    Ok(OsmPbfScanReport {
        blobs_scanned,
        osm_header_blobs,
        osm_data_blobs,
        compressed_bytes,
        decoded_bytes,
        primitive_groups,
        dense_nodes,
        ways,
        highway_ways,
        elapsed_millis: elapsed_millis(start),
    })
}

#[derive(Clone, Debug)]
struct BlobHeader {
    blob_type: String,
    data_size: i32,
}

fn parse_blob_header(bytes: &[u8]) -> Result<BlobHeader> {
    let mut reader = ProtoReader::new(bytes);
    let mut blob_type = None;
    let mut data_size = None;
    while !reader.exhausted() {
        let field = reader.next_field()?;
        match field {
            1 => blob_type = Some(reader.read_string()?),
            3 => data_size = Some(reader.read_varint_i64()? as i32),
            _ => reader.skip_current_field()?,
        }
    }
    match (blob_type, data_size) {
        (Some(blob_type), Some(data_size)) if data_size >= 0 => Ok(BlobHeader {
            blob_type,
            data_size,
        }),
        _ => Err(OsmError::invalid("PBF BlobHeader missing type or datasize")),
    }
}

fn decode_blob(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut reader = ProtoReader::new(bytes);
    let mut raw = None;
    let mut zlib = None;
    while !reader.exhausted() {
        let field = reader.next_field()?;
        match field {
            1 => raw = Some(reader.read_bytes()?.to_vec()),
            3 => zlib = Some(reader.read_bytes()?.to_vec()),
            _ => reader.skip_current_field()?,
        }
    }
    if let Some(raw) = raw {
        return Ok(raw);
    }
    if let Some(zlib) = zlib {
        let mut decoder = ZlibDecoder::new(&zlib[..]);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|error| OsmError::invalid(error.to_string()))?;
        return Ok(out);
    }
    Err(OsmError::invalid("PBF Blob has neither raw nor zlib_data"))
}

#[derive(Default)]
struct PrimitiveStats {
    primitive_groups: i64,
    dense_nodes: i64,
    ways: i64,
    highway_ways: i64,
}

fn parse_primitive_stats_block(bytes: &[u8]) -> Result<PrimitiveStats> {
    let mut strings = Vec::new();
    let mut stats = PrimitiveStats::default();
    let mut reader = ProtoReader::new(bytes);
    while !reader.exhausted() {
        let field = reader.next_field()?;
        match field {
            1 => strings = parse_string_table(reader.read_bytes()?)?,
            2 => {
                stats.primitive_groups += 1;
                let group_stats = parse_primitive_group_stats(reader.read_bytes()?, &strings)?;
                stats.dense_nodes += group_stats.dense_nodes;
                stats.ways += group_stats.ways;
                stats.highway_ways += group_stats.highway_ways;
            }
            _ => reader.skip_current_field()?,
        }
    }
    Ok(stats)
}

fn parse_primitive_group_stats(bytes: &[u8], strings: &[String]) -> Result<PrimitiveStats> {
    let mut stats = PrimitiveStats::default();
    let mut reader = ProtoReader::new(bytes);
    while !reader.exhausted() {
        let field = reader.next_field()?;
        match field {
            2 => stats.dense_nodes += parse_dense_node_count(reader.read_bytes()?)?,
            3 => {
                stats.ways += 1;
                if way_has_tag(reader.read_bytes()?, strings, "highway")? {
                    stats.highway_ways += 1;
                }
            }
            _ => reader.skip_current_field()?,
        }
    }
    Ok(stats)
}

fn parse_dense_node_count(bytes: &[u8]) -> Result<i64> {
    let mut reader = ProtoReader::new(bytes);
    while !reader.exhausted() {
        let field = reader.next_field()?;
        if field == 1 {
            return count_packed_varints(reader.read_bytes()?);
        }
        reader.skip_current_field()?;
    }
    Ok(0)
}

fn way_has_tag(bytes: &[u8], strings: &[String], key: &str) -> Result<bool> {
    let mut reader = ProtoReader::new(bytes);
    let mut keys = Vec::new();
    while !reader.exhausted() {
        let field = reader.next_field()?;
        if field == 2 {
            keys = read_packed_i32(reader.read_bytes()?)?;
        } else {
            reader.skip_current_field()?;
        }
    }
    Ok(keys.into_iter().any(|index| {
        index >= 0 && (index as usize) < strings.len() && strings[index as usize] == key
    }))
}

fn count_packed_varints(bytes: &[u8]) -> Result<i64> {
    let mut reader = ProtoReader::new(bytes);
    let mut count = 0;
    while !reader.exhausted() {
        reader.read_varint_i64()?;
        count += 1;
    }
    Ok(count)
}

pub fn decode_primitive_block(bytes: &[u8]) -> Result<OsmPbfDecodedBlock> {
    let mut strings = Vec::new();
    let mut primitive_group_bytes = Vec::new();
    let mut granularity = DEFAULT_GRANULARITY;
    let mut lat_offset = 0i64;
    let mut lon_offset = 0i64;
    let mut reader = ProtoReader::new(bytes);
    while !reader.exhausted() {
        let field = reader.next_field()?;
        match field {
            1 => strings = parse_string_table(reader.read_bytes()?)?,
            2 => primitive_group_bytes.push(reader.read_bytes()?.to_vec()),
            17 => granularity = reader.read_varint_i64()?,
            18 => lat_offset = reader.read_sint64()?,
            19 => lon_offset = reader.read_sint64()?,
            _ => reader.skip_current_field()?,
        }
    }
    let mut nodes = Vec::new();
    let mut ways = Vec::new();
    for group in &primitive_group_bytes {
        decode_primitive_group(
            group,
            &strings,
            granularity,
            lat_offset,
            lon_offset,
            &mut nodes,
            &mut ways,
        )?;
    }
    Ok(OsmPbfDecodedBlock {
        nodes,
        ways,
        primitive_groups: primitive_group_bytes.len() as i64,
    })
}

fn decode_primitive_group(
    bytes: &[u8],
    strings: &[String],
    granularity: i64,
    lat_offset: i64,
    lon_offset: i64,
    nodes: &mut Vec<OsmNode>,
    ways: &mut Vec<OsmWay>,
) -> Result<()> {
    let mut reader = ProtoReader::new(bytes);
    while !reader.exhausted() {
        let field = reader.next_field()?;
        match field {
            1 => nodes.push(parse_node(
                reader.read_bytes()?,
                granularity,
                lat_offset,
                lon_offset,
            )?),
            2 => nodes.extend(parse_dense_nodes(
                reader.read_bytes()?,
                granularity,
                lat_offset,
                lon_offset,
            )?),
            3 => ways.push(parse_way(reader.read_bytes()?, strings)?),
            _ => reader.skip_current_field()?,
        }
    }
    Ok(())
}

fn parse_node(bytes: &[u8], granularity: i64, lat_offset: i64, lon_offset: i64) -> Result<OsmNode> {
    let mut reader = ProtoReader::new(bytes);
    let mut id = 0i64;
    let mut lat = 0i64;
    let mut lon = 0i64;
    while !reader.exhausted() {
        let field = reader.next_field()?;
        match field {
            1 => id = reader.read_sint64()?,
            8 => lat = reader.read_sint64()?,
            9 => lon = reader.read_sint64()?,
            _ => reader.skip_current_field()?,
        }
    }
    Ok(OsmNode {
        id,
        longitude: to_degrees(lon, granularity, lon_offset),
        latitude: to_degrees(lat, granularity, lat_offset),
    })
}

fn parse_dense_nodes(
    bytes: &[u8],
    granularity: i64,
    lat_offset: i64,
    lon_offset: i64,
) -> Result<Vec<OsmNode>> {
    let mut reader = ProtoReader::new(bytes);
    let mut id_bytes = None;
    let mut lat_bytes = None;
    let mut lon_bytes = None;
    while !reader.exhausted() {
        let field = reader.next_field()?;
        match field {
            1 => id_bytes = Some(reader.read_bytes()?.to_vec()),
            8 => lat_bytes = Some(reader.read_bytes()?.to_vec()),
            9 => lon_bytes = Some(reader.read_bytes()?.to_vec()),
            _ => reader.skip_current_field()?,
        }
    }
    let (Some(id_bytes), Some(lat_bytes), Some(lon_bytes)) = (id_bytes, lat_bytes, lon_bytes)
    else {
        return Ok(Vec::new());
    };
    let mut ids = ProtoReader::new(&id_bytes);
    let mut lats = ProtoReader::new(&lat_bytes);
    let mut lons = ProtoReader::new(&lon_bytes);
    let mut id = 0i64;
    let mut lat = 0i64;
    let mut lon = 0i64;
    let mut nodes = Vec::new();
    while !ids.exhausted() && !lats.exhausted() && !lons.exhausted() {
        id += ids.read_sint64()?;
        lat += lats.read_sint64()?;
        lon += lons.read_sint64()?;
        nodes.push(OsmNode {
            id,
            longitude: to_degrees(lon, granularity, lon_offset),
            latitude: to_degrees(lat, granularity, lat_offset),
        });
    }
    Ok(nodes)
}

fn parse_way(bytes: &[u8], strings: &[String]) -> Result<OsmWay> {
    let mut reader = ProtoReader::new(bytes);
    let mut id = 0i64;
    let mut keys = Vec::new();
    let mut values = Vec::new();
    let mut ref_deltas = Vec::new();
    while !reader.exhausted() {
        let field = reader.next_field()?;
        match field {
            1 => id = reader.read_varint_i64()?,
            2 => keys = read_packed_i32(reader.read_bytes()?)?,
            3 => values = read_packed_i32(reader.read_bytes()?)?,
            8 => ref_deltas = read_packed_sint64(reader.read_bytes()?)?,
            _ => reader.skip_current_field()?,
        }
    }
    let mut refs = Vec::with_capacity(ref_deltas.len());
    let mut node_ref = 0i64;
    for delta in ref_deltas {
        node_ref += delta;
        refs.push(node_ref);
    }
    Ok(OsmWay {
        id,
        node_refs: refs,
        tags: tags_for(&keys, &values, strings),
    })
}

fn parse_string_table(bytes: &[u8]) -> Result<Vec<String>> {
    let mut reader = ProtoReader::new(bytes);
    let mut strings = Vec::new();
    while !reader.exhausted() {
        let field = reader.next_field()?;
        if field == 1 {
            strings.push(reader.read_string()?);
        } else {
            reader.skip_current_field()?;
        }
    }
    Ok(strings)
}

fn tags_for(keys: &[i32], values: &[i32], strings: &[String]) -> BTreeMap<String, String> {
    let mut tags = BTreeMap::new();
    for index in 0..keys.len().min(values.len()) {
        let key = keys[index];
        let value = values[index];
        if key >= 0
            && value >= 0
            && (key as usize) < strings.len()
            && (value as usize) < strings.len()
        {
            tags.insert(
                strings[key as usize].clone(),
                strings[value as usize].clone(),
            );
        }
    }
    tags
}

fn read_packed_i32(bytes: &[u8]) -> Result<Vec<i32>> {
    let mut reader = ProtoReader::new(bytes);
    let mut values = Vec::new();
    while !reader.exhausted() {
        values.push(reader.read_varint_i64()? as i32);
    }
    Ok(values)
}

fn read_packed_sint64(bytes: &[u8]) -> Result<Vec<i64>> {
    let mut reader = ProtoReader::new(bytes);
    let mut values = Vec::new();
    while !reader.exhausted() {
        values.push(reader.read_sint64()?);
    }
    Ok(values)
}

fn to_degrees(value: i64, granularity: i64, offset: i64) -> f64 {
    (offset + (granularity * value)) as f64 * NANO_DEGREES
}

#[derive(Clone, Debug, PartialEq)]
pub struct OsmPbfDecodedBlock {
    pub nodes: Vec<OsmNode>,
    pub ways: Vec<OsmWay>,
    pub primitive_groups: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OsmNode {
    pub id: i64,
    pub longitude: f64,
    pub latitude: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OsmWay {
    pub id: i64,
    pub node_refs: Vec<i64>,
    pub tags: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OsmBlockPoint {
    pub block_x: i32,
    pub block_z: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum OsmFeatureKind {
    Road,
    Waterway,
    Landuse,
    Building,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OsmIndexedFeature {
    pub way_id: i64,
    pub kind: OsmFeatureKind,
    pub points: Vec<OsmBlockPoint>,
    pub min_block_x: i32,
    pub min_block_z: i32,
    pub max_block_x: i32,
    pub max_block_z: i32,
    pub tags: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OsmRegionFeatureIndex {
    pub region_x: i32,
    pub region_z: i32,
    pub features: Vec<OsmIndexedFeature>,
    pub considered_ways: i32,
    pub skipped_unscoped_ways: i32,
    pub skipped_missing_node_ways: i32,
}

impl OsmRegionFeatureIndex {
    pub fn build(
        mapping: &EarthScaleMapping,
        region_x: i32,
        region_z: i32,
        nodes: &HashMap<i64, OsmNode>,
        ways: &[OsmWay],
    ) -> Result<Self> {
        Self::build_internal(mapping, region_x, region_z, nodes, ways, false)
    }

    pub fn build_allow_partial(
        mapping: &EarthScaleMapping,
        region_x: i32,
        region_z: i32,
        nodes: &HashMap<i64, OsmNode>,
        ways: &[OsmWay],
    ) -> Result<Self> {
        Self::build_internal(mapping, region_x, region_z, nodes, ways, true)
    }

    fn build_internal(
        mapping: &EarthScaleMapping,
        region_x: i32,
        region_z: i32,
        nodes: &HashMap<i64, OsmNode>,
        ways: &[OsmWay],
        allow_partial_segments: bool,
    ) -> Result<Self> {
        let min_region_block_x = region_x * REGION_SIZE_BLOCKS;
        let min_region_block_z = region_z * REGION_SIZE_BLOCKS;
        let max_region_block_x = min_region_block_x + REGION_SIZE_BLOCKS - 1;
        let max_region_block_z = min_region_block_z + REGION_SIZE_BLOCKS - 1;
        let mut skipped_unscoped_ways = 0;
        let mut skipped_missing_node_ways = 0;
        let mut features = Vec::new();
        for way in ways {
            let Some(kind) = classify_tags(&way.tags) else {
                skipped_unscoped_ways += 1;
                continue;
            };
            let candidates = if allow_partial_segments {
                project_partial_way_segments(mapping, nodes, way)?
            } else {
                project_way(mapping, nodes, way)?.into_iter().collect()
            };
            if candidates.is_empty() {
                skipped_missing_node_ways += 1;
                continue;
            }
            for candidate in candidates {
                if !intersects(
                    candidate.min_block_x,
                    candidate.min_block_z,
                    candidate.max_block_x,
                    candidate.max_block_z,
                    min_region_block_x,
                    min_region_block_z,
                    max_region_block_x,
                    max_region_block_z,
                ) {
                    continue;
                }
                features.push(OsmIndexedFeature {
                    way_id: way.id,
                    kind,
                    points: candidate.points,
                    min_block_x: candidate.min_block_x,
                    min_block_z: candidate.min_block_z,
                    max_block_x: candidate.max_block_x,
                    max_block_z: candidate.max_block_z,
                    tags: way.tags.clone(),
                });
            }
        }
        Ok(Self {
            region_x,
            region_z,
            features,
            considered_ways: ways.len() as i32,
            skipped_unscoped_ways,
            skipped_missing_node_ways,
        })
    }

    pub fn count_by_kind(&self, kind: OsmFeatureKind) -> i32 {
        self.features
            .iter()
            .filter(|feature| feature.kind == kind)
            .count() as i32
    }
}

pub fn classify_tags(tags: &BTreeMap<String, String>) -> Option<OsmFeatureKind> {
    if tags.contains_key("highway") {
        return Some(OsmFeatureKind::Road);
    }
    if tags.contains_key("waterway") {
        return Some(OsmFeatureKind::Waterway);
    }
    if tags.contains_key("landuse") || tags.contains_key("natural") || tags.contains_key("leisure")
    {
        return Some(OsmFeatureKind::Landuse);
    }
    if tags.contains_key("building") {
        return Some(OsmFeatureKind::Building);
    }
    None
}

#[derive(Clone, Debug)]
struct IndexedCandidate {
    points: Vec<OsmBlockPoint>,
    min_block_x: i32,
    min_block_z: i32,
    max_block_x: i32,
    max_block_z: i32,
}

fn project_way(
    mapping: &EarthScaleMapping,
    nodes: &HashMap<i64, OsmNode>,
    way: &OsmWay,
) -> Result<Option<IndexedCandidate>> {
    if way.node_refs.len() < 2 {
        return Ok(None);
    }
    let mut points = Vec::with_capacity(way.node_refs.len());
    let mut min_block_x = i32::MAX;
    let mut min_block_z = i32::MAX;
    let mut max_block_x = i32::MIN;
    let mut max_block_z = i32::MIN;
    for node_ref in &way.node_refs {
        let Some(node) = nodes.get(node_ref) else {
            return Ok(None);
        };
        let point = node_to_block_point(mapping, *node)?;
        points.push(point);
        min_block_x = min_block_x.min(point.block_x);
        min_block_z = min_block_z.min(point.block_z);
        max_block_x = max_block_x.max(point.block_x);
        max_block_z = max_block_z.max(point.block_z);
    }
    Ok(Some(IndexedCandidate {
        points,
        min_block_x,
        min_block_z,
        max_block_x,
        max_block_z,
    }))
}

fn project_partial_way_segments(
    mapping: &EarthScaleMapping,
    nodes: &HashMap<i64, OsmNode>,
    way: &OsmWay,
) -> Result<Vec<IndexedCandidate>> {
    if way.node_refs.len() == 1 {
        let Some(node) = nodes.get(&way.node_refs[0]) else {
            return Ok(Vec::new());
        };
        let point = node_to_block_point(mapping, *node)?;
        return Ok(vec![IndexedCandidate {
            points: vec![point],
            min_block_x: point.block_x,
            min_block_z: point.block_z,
            max_block_x: point.block_x,
            max_block_z: point.block_z,
        }]);
    }
    if way.node_refs.len() < 2 {
        return Ok(Vec::new());
    }
    let mut candidates = Vec::new();
    let mut current = Vec::new();
    let mut min_block_x = i32::MAX;
    let mut min_block_z = i32::MAX;
    let mut max_block_x = i32::MIN;
    let mut max_block_z = i32::MIN;
    for node_ref in &way.node_refs {
        if let Some(node) = nodes.get(node_ref) {
            let point = node_to_block_point(mapping, *node)?;
            current.push(point);
            min_block_x = min_block_x.min(point.block_x);
            min_block_z = min_block_z.min(point.block_z);
            max_block_x = max_block_x.max(point.block_x);
            max_block_z = max_block_z.max(point.block_z);
        } else {
            if current.len() >= 2 {
                candidates.push(IndexedCandidate {
                    points: current,
                    min_block_x,
                    min_block_z,
                    max_block_x,
                    max_block_z,
                });
            }
            current = Vec::new();
            min_block_x = i32::MAX;
            min_block_z = i32::MAX;
            max_block_x = i32::MIN;
            max_block_z = i32::MIN;
        }
    }
    if current.len() >= 2 {
        candidates.push(IndexedCandidate {
            points: current,
            min_block_x,
            min_block_z,
            max_block_x,
            max_block_z,
        });
    }
    Ok(candidates)
}

fn node_to_block_point(mapping: &EarthScaleMapping, node: OsmNode) -> Result<OsmBlockPoint> {
    let map_x = mapping
        .block_x_for_longitude(node.longitude)
        .map_err(|error| OsmError::invalid(error.to_string()))?;
    let map_z = mapping
        .block_z_for_latitude(node.latitude)
        .map_err(|error| OsmError::invalid(error.to_string()))?;
    Ok(OsmBlockPoint {
        block_x: map_x - (mapping.width_blocks / 2),
        block_z: map_z - (mapping.height_blocks / 2),
    })
}

fn intersects(
    min_x: i32,
    min_z: i32,
    max_x: i32,
    max_z: i32,
    region_min_x: i32,
    region_min_z: i32,
    region_max_x: i32,
    region_max_z: i32,
) -> bool {
    max_x >= region_min_x && min_x <= region_max_x && max_z >= region_min_z && min_z <= region_max_z
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OsmRegionFeatureMask {
    roads: Vec<bool>,
    waterways: Vec<bool>,
    landuse: Vec<bool>,
    buildings: Vec<bool>,
}

impl OsmRegionFeatureMask {
    pub fn rasterize(index: &OsmRegionFeatureIndex) -> Self {
        let mut mask = Self {
            roads: vec![false; (REGION_SIZE_BLOCKS * REGION_SIZE_BLOCKS) as usize],
            waterways: vec![false; (REGION_SIZE_BLOCKS * REGION_SIZE_BLOCKS) as usize],
            landuse: vec![false; (REGION_SIZE_BLOCKS * REGION_SIZE_BLOCKS) as usize],
            buildings: vec![false; (REGION_SIZE_BLOCKS * REGION_SIZE_BLOCKS) as usize],
        };
        let region_origin_x = index.region_x * REGION_SIZE_BLOCKS;
        let region_origin_z = index.region_z * REGION_SIZE_BLOCKS;
        for feature in &index.features {
            if feature.points.len() == 1 {
                let point = feature.points[0];
                mask.mark(
                    feature.kind,
                    point.block_x - region_origin_x,
                    point.block_z - region_origin_z,
                );
                continue;
            }
            for point_index in 1..feature.points.len() {
                let previous = feature.points[point_index - 1];
                let current = feature.points[point_index];
                mask.mark_line(
                    feature.kind,
                    previous.block_x - region_origin_x,
                    previous.block_z - region_origin_z,
                    current.block_x - region_origin_x,
                    current.block_z - region_origin_z,
                );
            }
        }
        mask
    }

    pub fn road_count(&self) -> i32 {
        count_true(&self.roads)
    }

    pub fn waterway_count(&self) -> i32 {
        count_true(&self.waterways)
    }

    pub fn landuse_count(&self) -> i32 {
        count_true(&self.landuse)
    }

    pub fn building_count(&self) -> i32 {
        count_true(&self.buildings)
    }

    pub fn road_at(&self, local_x: i32, local_z: i32) -> Result<bool> {
        feature_at(&self.roads, local_x, local_z)
    }

    pub fn waterway_at(&self, local_x: i32, local_z: i32) -> Result<bool> {
        feature_at(&self.waterways, local_x, local_z)
    }

    pub fn landuse_at(&self, local_x: i32, local_z: i32) -> Result<bool> {
        feature_at(&self.landuse, local_x, local_z)
    }

    pub fn building_at(&self, local_x: i32, local_z: i32) -> Result<bool> {
        feature_at(&self.buildings, local_x, local_z)
    }

    fn mark_line(&mut self, kind: OsmFeatureKind, x0: i32, z0: i32, x1: i32, z1: i32) {
        let dx = (x1 - x0).abs();
        let dz = (z1 - z0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sz = if z0 < z1 { 1 } else { -1 };
        let mut error = dx - dz;
        let mut x = x0;
        let mut z = z0;
        loop {
            self.mark(kind, x, z);
            if x == x1 && z == z1 {
                return;
            }
            let double_error = error * 2;
            if double_error > -dz {
                error -= dz;
                x += sx;
            }
            if double_error < dx {
                error += dx;
                z += sz;
            }
        }
    }

    fn mark(&mut self, kind: OsmFeatureKind, local_x: i32, local_z: i32) {
        if !(0..REGION_SIZE_BLOCKS).contains(&local_x)
            || !(0..REGION_SIZE_BLOCKS).contains(&local_z)
        {
            return;
        }
        let offset = (local_z * REGION_SIZE_BLOCKS + local_x) as usize;
        match kind {
            OsmFeatureKind::Road => self.roads[offset] = true,
            OsmFeatureKind::Waterway => self.waterways[offset] = true,
            OsmFeatureKind::Landuse => self.landuse[offset] = true,
            OsmFeatureKind::Building => self.buildings[offset] = true,
        }
    }
}

fn count_true(values: &[bool]) -> i32 {
    values.iter().filter(|&&value| value).count() as i32
}

fn feature_at(values: &[bool], local_x: i32, local_z: i32) -> Result<bool> {
    if !(0..REGION_SIZE_BLOCKS).contains(&local_x) || !(0..REGION_SIZE_BLOCKS).contains(&local_z) {
        return Err(OsmError::invalid(format!(
            "local coordinate outside region: {local_x},{local_z}"
        )));
    }
    Ok(values[(local_z * REGION_SIZE_BLOCKS + local_x) as usize])
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OsmPbfRegionExtractResult {
    pub mask: OsmRegionFeatureMask,
    pub report: OsmPbfRegionExtractReport,
}

pub fn extract_mask(
    pbf: &Path,
    max_blobs: i32,
    mapping: &EarthScaleMapping,
    region_x: i32,
    region_z: i32,
) -> Result<OsmPbfRegionExtractResult> {
    extract_window(pbf, max_blobs, 0, 0, mapping, region_x, region_z)
}

pub fn extract_window(
    pbf: &Path,
    node_max_blobs: i32,
    way_skip_blobs: i32,
    way_max_blobs: i32,
    mapping: &EarthScaleMapping,
    region_x: i32,
    region_z: i32,
) -> Result<OsmPbfRegionExtractResult> {
    if node_max_blobs <= 0 {
        return Err(OsmError::invalid("nodeMaxBlobs must be positive"));
    }
    if way_skip_blobs < 0 || way_max_blobs < 0 {
        return Err(OsmError::invalid(
            "waySkipBlobs and wayMaxBlobs must be non-negative",
        ));
    }
    let start = Instant::now();
    let bounds =
        RegionGeoBounds::for_region(mapping, region_x, region_z, REGION_NODE_PADDING_BLOCKS);
    let mut nodes = HashMap::new();
    let mut ways = Vec::new();
    let mut primitive_groups = 0i64;
    let node_scan = scan_pbf_data_blocks(
        pbf,
        node_max_blobs,
        0,
        false,
        |blob_index, byte_offset, bytes| {
            let decoded = decode_primitive_block(bytes)?;
            retain_decoded(
                &bounds,
                &mut nodes,
                &mut ways,
                &mut primitive_groups,
                decoded,
                blob_index,
                byte_offset,
                true,
                true,
            )
        },
    )?;
    let way_scan = if way_max_blobs > 0 {
        Some(scan_pbf_data_blocks(
            pbf,
            way_max_blobs,
            way_skip_blobs,
            false,
            |blob_index, byte_offset, bytes| {
                let decoded = decode_primitive_block(bytes)?;
                retain_decoded(
                    &bounds,
                    &mut nodes,
                    &mut ways,
                    &mut primitive_groups,
                    decoded,
                    blob_index,
                    byte_offset,
                    true,
                    true,
                )
            },
        )?)
    } else {
        None
    };
    let combined = combine_scan_reports(node_scan, way_scan);
    build_extract_result(
        combined,
        primitive_groups,
        nodes.len() as i64,
        ways.len() as i64,
        mapping,
        region_x,
        region_z,
        &nodes,
        &ways,
        false,
        elapsed_millis(start),
    )
}

pub fn extract_way_ref_window(
    pbf: &Path,
    node_max_blobs: i32,
    way_skip_blobs: i32,
    way_max_blobs: i32,
    mapping: &EarthScaleMapping,
    region_x: i32,
    region_z: i32,
) -> Result<OsmPbfRegionExtractResult> {
    if node_max_blobs <= 0 || way_max_blobs <= 0 {
        return Err(OsmError::invalid(
            "nodeMaxBlobs and wayMaxBlobs must be positive",
        ));
    }
    if way_skip_blobs < 0 {
        return Err(OsmError::invalid("waySkipBlobs must be non-negative"));
    }
    let start = Instant::now();
    let mut nodes = HashMap::new();
    let mut ways = Vec::new();
    let mut target_node_refs = HashSet::new();
    let mut primitive_groups = 0i64;
    let way_scan = scan_pbf_data_blocks(
        pbf,
        way_max_blobs,
        way_skip_blobs,
        false,
        |blob_index, byte_offset, bytes| {
            let decoded = decode_primitive_block(bytes)?;
            retain_classified_ways_and_refs(
                &mut ways,
                &mut target_node_refs,
                &mut primitive_groups,
                decoded,
                blob_index,
                byte_offset,
            )
        },
    )?;
    let node_scan = scan_pbf_data_blocks(
        pbf,
        node_max_blobs,
        0,
        false,
        |blob_index, byte_offset, bytes| {
            let decoded = decode_primitive_block(bytes)?;
            retain_target_nodes(
                &mut nodes,
                &target_node_refs,
                &mut primitive_groups,
                decoded,
                blob_index,
                byte_offset,
            )
        },
    )?;
    let combined = combine_scan_reports(node_scan, Some(way_scan));
    build_extract_result(
        combined,
        primitive_groups,
        nodes.len() as i64,
        ways.len() as i64,
        mapping,
        region_x,
        region_z,
        &nodes,
        &ways,
        true,
        elapsed_millis(start),
    )
}

pub fn extract_full_scan<F>(
    pbf: &Path,
    max_blobs: i32,
    mapping: &EarthScaleMapping,
    region_x: i32,
    region_z: i32,
    progress_every: i32,
    mut progress_consumer: F,
) -> Result<OsmPbfRegionExtractResult>
where
    F: FnMut(FullScanProgress) -> Result<()>,
{
    if max_blobs <= 0 {
        return Err(OsmError::invalid("maxBlobs must be positive"));
    }
    if progress_every < 0 {
        return Err(OsmError::invalid("progressEveryBlobs must be non-negative"));
    }
    let start = Instant::now();
    let bounds =
        RegionGeoBounds::for_region(mapping, region_x, region_z, REGION_NODE_PADDING_BLOCKS);
    let mut nodes = HashMap::new();
    let mut ways = Vec::new();
    let mut primitive_groups = 0i64;
    let scan = scan_pbf_data_blocks(
        pbf,
        max_blobs,
        0,
        false,
        |blob_index, byte_offset, bytes| {
            let decoded = decode_primitive_block(bytes)?;
            retain_decoded(
                &bounds,
                &mut nodes,
                &mut ways,
                &mut primitive_groups,
                decoded,
                blob_index,
                byte_offset,
                true,
                true,
            )?;
            if progress_every > 0 && blob_index % progress_every == 0 {
                progress_consumer(FullScanProgress {
                    blob_index,
                    byte_offset,
                    elapsed_millis: elapsed_millis(start),
                    retained_nodes: nodes.len() as i32,
                    considered_ways: ways.len() as i32,
                    indexed_features: 0,
                    primitive_groups,
                })?;
            }
            Ok(())
        },
    )?;
    build_extract_result(
        scan,
        primitive_groups,
        nodes.len() as i64,
        ways.len() as i64,
        mapping,
        region_x,
        region_z,
        &nodes,
        &ways,
        true,
        elapsed_millis(start),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FullScanProgress {
    pub blob_index: i32,
    pub byte_offset: u64,
    pub elapsed_millis: i64,
    pub retained_nodes: i32,
    pub considered_ways: i32,
    pub indexed_features: i32,
    pub primitive_groups: i64,
}

fn retain_decoded(
    bounds: &RegionGeoBounds,
    nodes: &mut HashMap<i64, OsmNode>,
    ways: &mut Vec<OsmWay>,
    primitive_groups: &mut i64,
    decoded: OsmPbfDecodedBlock,
    blob_index: i32,
    byte_offset: u64,
    retain_nodes: bool,
    retain_ways: bool,
) -> Result<()> {
    if retain_nodes {
        for node in decoded.nodes {
            if bounds.contains(node.longitude, node.latitude) {
                if nodes.len() >= MAX_BOUNDED_NODES {
                    return Err(OsmError::scan(
                        format!("bounded OSM extract node limit exceeded at blob {blob_index} offset {byte_offset}: {MAX_BOUNDED_NODES}"),
                        blob_index,
                        byte_offset,
                    ));
                }
                nodes.insert(node.id, node);
            }
        }
    }
    if retain_ways {
        for way in decoded.ways {
            if classify_tags(&way.tags).is_none() {
                continue;
            }
            if ways.len() >= MAX_BOUNDED_WAYS {
                return Err(OsmError::scan(
                    format!("bounded OSM extract way limit exceeded at blob {blob_index} offset {byte_offset}: {MAX_BOUNDED_WAYS}"),
                    blob_index,
                    byte_offset,
                ));
            }
            ways.push(way);
        }
    }
    *primitive_groups += decoded.primitive_groups;
    Ok(())
}

fn retain_classified_ways_and_refs(
    ways: &mut Vec<OsmWay>,
    target_node_refs: &mut HashSet<i64>,
    primitive_groups: &mut i64,
    decoded: OsmPbfDecodedBlock,
    blob_index: i32,
    byte_offset: u64,
) -> Result<()> {
    for way in decoded.ways {
        if classify_tags(&way.tags).is_none() {
            continue;
        }
        if ways.len() >= MAX_BOUNDED_WAYS {
            return Err(OsmError::scan(
                format!("bounded OSM extract way limit exceeded at blob {blob_index} offset {byte_offset}: {MAX_BOUNDED_WAYS}"),
                blob_index,
                byte_offset,
            ));
        }
        for node_ref in &way.node_refs {
            if !target_node_refs.contains(node_ref) {
                if target_node_refs.len() >= MAX_TARGET_NODE_REFS {
                    return Err(OsmError::scan(
                        format!("bounded OSM target node ref limit exceeded at blob {blob_index} offset {byte_offset}: {MAX_TARGET_NODE_REFS}"),
                        blob_index,
                        byte_offset,
                    ));
                }
                target_node_refs.insert(*node_ref);
            }
        }
        ways.push(way);
    }
    *primitive_groups += decoded.primitive_groups;
    Ok(())
}

fn retain_target_nodes(
    nodes: &mut HashMap<i64, OsmNode>,
    target_node_refs: &HashSet<i64>,
    primitive_groups: &mut i64,
    decoded: OsmPbfDecodedBlock,
    blob_index: i32,
    byte_offset: u64,
) -> Result<()> {
    for node in decoded.nodes {
        if !target_node_refs.contains(&node.id) {
            continue;
        }
        if nodes.len() >= MAX_BOUNDED_NODES {
            return Err(OsmError::scan(
                format!("bounded OSM extract node limit exceeded at blob {blob_index} offset {byte_offset}: {MAX_BOUNDED_NODES}"),
                blob_index,
                byte_offset,
            ));
        }
        nodes.insert(node.id, node);
    }
    *primitive_groups += decoded.primitive_groups;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_extract_result(
    scan: OsmPbfScanReport,
    primitive_groups: i64,
    decoded_nodes: i64,
    decoded_ways: i64,
    mapping: &EarthScaleMapping,
    region_x: i32,
    region_z: i32,
    nodes: &HashMap<i64, OsmNode>,
    ways: &[OsmWay],
    allow_partial: bool,
    elapsed_millis: i64,
) -> Result<OsmPbfRegionExtractResult> {
    let index = if allow_partial {
        OsmRegionFeatureIndex::build_allow_partial(mapping, region_x, region_z, nodes, ways)?
    } else {
        OsmRegionFeatureIndex::build(mapping, region_x, region_z, nodes, ways)?
    };
    let mask = OsmRegionFeatureMask::rasterize(&index);
    let report = OsmPbfRegionExtractReport {
        blobs_scanned: scan.blobs_scanned,
        osm_data_blobs: scan.osm_data_blobs,
        primitive_groups,
        decoded_nodes,
        decoded_ways,
        index_considered_ways: index.considered_ways,
        index_skipped_missing_node_ways: index.skipped_missing_node_ways,
        indexed_features: index.features.len() as i32,
        road_features: index.count_by_kind(OsmFeatureKind::Road),
        waterway_features: index.count_by_kind(OsmFeatureKind::Waterway),
        landuse_features: index.count_by_kind(OsmFeatureKind::Landuse),
        building_features: index.count_by_kind(OsmFeatureKind::Building),
        road_mask_pixels: mask.road_count(),
        waterway_mask_pixels: mask.waterway_count(),
        landuse_mask_pixels: mask.landuse_count(),
        building_mask_pixels: mask.building_count(),
        elapsed_millis,
    };
    Ok(OsmPbfRegionExtractResult { mask, report })
}

fn combine_scan_reports(
    first: OsmPbfScanReport,
    second: Option<OsmPbfScanReport>,
) -> OsmPbfScanReport {
    let Some(second) = second else {
        return first;
    };
    OsmPbfScanReport {
        blobs_scanned: first.blobs_scanned + second.blobs_scanned,
        osm_header_blobs: first.osm_header_blobs + second.osm_header_blobs,
        osm_data_blobs: first.osm_data_blobs + second.osm_data_blobs,
        compressed_bytes: first.compressed_bytes + second.compressed_bytes,
        decoded_bytes: first.decoded_bytes + second.decoded_bytes,
        primitive_groups: first.primitive_groups + second.primitive_groups,
        dense_nodes: first.dense_nodes + second.dense_nodes,
        ways: first.ways + second.ways,
        highway_ways: first.highway_ways + second.highway_ways,
        elapsed_millis: first.elapsed_millis + second.elapsed_millis,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RegionGeoBounds {
    min_longitude: f64,
    max_longitude: f64,
    min_latitude: f64,
    max_latitude: f64,
}

impl RegionGeoBounds {
    fn for_region(
        mapping: &EarthScaleMapping,
        region_x: i32,
        region_z: i32,
        padding_blocks: i32,
    ) -> Self {
        let min_global_x = (region_x * REGION_SIZE_BLOCKS) - padding_blocks;
        let max_global_x = ((region_x + 1) * REGION_SIZE_BLOCKS) - 1 + padding_blocks;
        let min_global_z = (region_z * REGION_SIZE_BLOCKS) - padding_blocks;
        let max_global_z = ((region_z + 1) * REGION_SIZE_BLOCKS) - 1 + padding_blocks;
        let min_map_x =
            (min_global_x + (mapping.width_blocks / 2)).clamp(0, mapping.width_blocks - 1);
        let max_map_x =
            (max_global_x + (mapping.width_blocks / 2)).clamp(0, mapping.width_blocks - 1);
        let min_map_z =
            (min_global_z + (mapping.height_blocks / 2)).clamp(0, mapping.height_blocks - 1);
        let max_map_z =
            (max_global_z + (mapping.height_blocks / 2)).clamp(0, mapping.height_blocks - 1);
        let min_longitude = mapping.longitude_for_block_x(min_map_x).unwrap_or(-180.0);
        let max_longitude = mapping.longitude_for_block_x(max_map_x).unwrap_or(180.0);
        let max_latitude = mapping
            .latitude_for_block_z(min_map_z)
            .unwrap_or(mapping.max_latitude);
        let min_latitude = mapping
            .latitude_for_block_z(max_map_z)
            .unwrap_or(mapping.min_latitude);
        Self {
            min_longitude,
            max_longitude,
            min_latitude,
            max_latitude,
        }
    }

    fn contains(self, longitude: f64, latitude: f64) -> bool {
        longitude >= self.min_longitude
            && longitude <= self.max_longitude
            && latitude >= self.min_latitude
            && latitude <= self.max_latitude
    }
}

pub fn benchmark_osm_index(
    scale: i32,
    region_x: i32,
    region_z: i32,
    way_count: i32,
) -> Result<OsmIndexBenchmarkReport> {
    if way_count <= 0 {
        return Err(OsmError::invalid("wayCount must be positive"));
    }
    let mapping = EarthScaleMapping::for_denominator(scale, -90.0, 90.0)
        .map_err(|error| OsmError::invalid(error.to_string()))?;
    let setup_start = Instant::now();
    let mut nodes = HashMap::with_capacity(way_count as usize * 2);
    let mut ways = Vec::with_capacity(way_count as usize);
    for index in 0..way_count {
        let first_node_id = (index * 2 + 1) as i64;
        let second_node_id = first_node_id + 1;
        let local_x = index % REGION_SIZE_BLOCKS;
        let local_z = (index / REGION_SIZE_BLOCKS) % REGION_SIZE_BLOCKS;
        let first_block_x = (region_x * REGION_SIZE_BLOCKS) + local_x;
        let first_block_z = (region_z * REGION_SIZE_BLOCKS) + local_z;
        nodes.insert(
            first_node_id,
            node_for_block(&mapping, first_node_id, first_block_x, first_block_z)?,
        );
        nodes.insert(
            second_node_id,
            node_for_block(&mapping, second_node_id, first_block_x + 1, first_block_z)?,
        );
        ways.push(OsmWay {
            id: index as i64 + 1,
            node_refs: vec![first_node_id, second_node_id],
            tags: synthetic_way_tags(index),
        });
    }
    let setup_millis = elapsed_millis(setup_start);
    let index_start = Instant::now();
    let index = OsmRegionFeatureIndex::build(&mapping, region_x, region_z, &nodes, &ways)?;
    let index_millis = elapsed_millis(index_start);
    let ways_per_second = if index_millis == 0 {
        way_count as f64 * 1000.0
    } else {
        (way_count as f64 * 1000.0) / index_millis as f64
    };
    Ok(OsmIndexBenchmarkReport {
        scale,
        region_x,
        region_z,
        ways: way_count,
        nodes: nodes.len() as i32,
        setup_millis,
        index_millis,
        ways_per_second,
        features: index.features.len() as i32,
        skipped_unscoped_ways: index.skipped_unscoped_ways,
        skipped_missing_node_ways: index.skipped_missing_node_ways,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OsmIndexBenchmarkReport {
    pub scale: i32,
    pub region_x: i32,
    pub region_z: i32,
    pub ways: i32,
    pub nodes: i32,
    pub setup_millis: i64,
    pub index_millis: i64,
    pub ways_per_second: f64,
    pub features: i32,
    pub skipped_unscoped_ways: i32,
    pub skipped_missing_node_ways: i32,
}

fn node_for_block(
    mapping: &EarthScaleMapping,
    node_id: i64,
    global_block_x: i32,
    global_block_z: i32,
) -> Result<OsmNode> {
    let map_x = global_block_x + (mapping.width_blocks / 2);
    let map_z = global_block_z + (mapping.height_blocks / 2);
    Ok(OsmNode {
        id: node_id,
        longitude: mapping
            .longitude_for_block_x(map_x)
            .map_err(|error| OsmError::invalid(error.to_string()))?,
        latitude: mapping
            .latitude_for_block_z(map_z)
            .map_err(|error| OsmError::invalid(error.to_string()))?,
    })
}

fn synthetic_way_tags(index: i32) -> BTreeMap<String, String> {
    let mut tags = BTreeMap::new();
    match index % 4 {
        0 => {
            tags.insert("highway".to_string(), "primary".to_string());
        }
        1 => {
            tags.insert("waterway".to_string(), "river".to_string());
        }
        2 => {
            tags.insert("landuse".to_string(), "residential".to_string());
        }
        _ => {
            tags.insert("building".to_string(), "yes".to_string());
        }
    }
    tags
}

pub fn extract_xml_region_mask(
    directory: &Path,
    mapping: &EarthScaleMapping,
    region_x: i32,
    region_z: i32,
) -> Result<OsmPbfRegionExtractResult> {
    if !directory.is_dir() {
        return Err(OsmError::invalid(format!(
            "OSM XML cache path is not a directory: {}",
            directory.display()
        )));
    }
    let start = Instant::now();
    let bounds =
        RegionGeoBounds::for_region(mapping, region_x, region_z, REGION_NODE_PADDING_BLOCKS);
    let mut nodes = HashMap::new();
    let mut ways = Vec::new();
    let mut files = osm_xml_files(directory)?;
    files.sort_by_key(|path| path.file_name().map(|name| name.to_os_string()));
    for file in &files {
        parse_osm_xml_file(file, bounds, &mut nodes, &mut ways)?;
    }
    let scan = OsmPbfScanReport {
        blobs_scanned: files.len() as i32,
        osm_header_blobs: 0,
        osm_data_blobs: files.len() as i32,
        compressed_bytes: 0,
        decoded_bytes: 0,
        primitive_groups: 0,
        dense_nodes: 0,
        ways: ways.len() as i64,
        highway_ways: 0,
        elapsed_millis: 0,
    };
    build_extract_result(
        scan,
        0,
        nodes.len() as i64,
        ways.len() as i64,
        mapping,
        region_x,
        region_z,
        &nodes,
        &ways,
        true,
        elapsed_millis(start),
    )
}

fn parse_osm_xml_file(
    path: &Path,
    bounds: RegionGeoBounds,
    nodes: &mut HashMap<i64, OsmNode>,
    ways: &mut Vec<OsmWay>,
) -> Result<()> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        OsmError::invalid(format!(
            "failed to read OSM XML cache file {}: {error}",
            path.display()
        ))
    })?;
    let mut reader = Reader::from_str(&text);
    reader.config_mut().trim_text(true);
    let mut current_way: Option<(i64, Vec<i64>, BTreeMap<String, String>)> = None;
    let mut current_node: Option<(i64, f64, f64, BTreeMap<String, String>)> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                b"node" => {
                    let id = required_xml_i64(&event, b"id")?;
                    let latitude = required_xml_f64(&event, b"lat")?;
                    let longitude = required_xml_f64(&event, b"lon")?;
                    current_node = Some((id, longitude, latitude, BTreeMap::new()));
                }
                b"way" => {
                    current_way = Some((
                        required_xml_i64(&event, b"id")?,
                        Vec::new(),
                        BTreeMap::new(),
                    ));
                }
                b"nd" => {
                    if let Some((_id, refs, _tags)) = &mut current_way {
                        refs.push(required_xml_i64(&event, b"ref")?);
                    }
                }
                b"tag" => {
                    let key = required_xml_string(&event, b"k")?;
                    let value = required_xml_string(&event, b"v")?;
                    if let Some((_id, _lon, _lat, tags)) = &mut current_node {
                        tags.insert(key, value);
                    } else if let Some((_id, _refs, tags)) = &mut current_way {
                        tags.insert(key, value);
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(event)) => match event.local_name().as_ref() {
                b"node" => {
                    let id = required_xml_i64(&event, b"id")?;
                    let latitude = required_xml_f64(&event, b"lat")?;
                    let longitude = required_xml_f64(&event, b"lon")?;
                    if bounds.contains(longitude, latitude) {
                        nodes.insert(
                            id,
                            OsmNode {
                                id,
                                longitude,
                                latitude,
                            },
                        );
                    }
                }
                b"nd" => {
                    if let Some((_id, refs, _tags)) = &mut current_way {
                        refs.push(required_xml_i64(&event, b"ref")?);
                    }
                }
                b"tag" => {
                    let key = required_xml_string(&event, b"k")?;
                    let value = required_xml_string(&event, b"v")?;
                    if let Some((_id, _lon, _lat, tags)) = &mut current_node {
                        tags.insert(key, value);
                    } else if let Some((_id, _refs, tags)) = &mut current_way {
                        tags.insert(key, value);
                    }
                }
                _ => {}
            },
            Ok(Event::End(event)) => match event.local_name().as_ref() {
                b"node" => {
                    if let Some((id, longitude, latitude, tags)) = current_node.take() {
                        if bounds.contains(longitude, latitude) {
                            nodes.insert(
                                id,
                                OsmNode {
                                    id,
                                    longitude,
                                    latitude,
                                },
                            );
                            if classify_tags(&tags).is_some() {
                                ways.push(OsmWay {
                                    id,
                                    node_refs: vec![id],
                                    tags,
                                });
                            }
                        }
                    }
                }
                b"way" => {
                    if let Some((id, node_refs, tags)) = current_way.take() {
                        if classify_tags(&tags).is_some() {
                            ways.push(OsmWay {
                                id,
                                node_refs,
                                tags,
                            });
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(OsmError::invalid(format!(
                    "failed to parse OSM XML cache file {}: {error}",
                    path.display()
                )))
            }
            _ => {}
        }
    }
    Ok(())
}

fn required_xml_string(event: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Result<String> {
    for attribute in event.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| OsmError::invalid(error.to_string()))?;
        if attribute.key.as_ref() == name {
            return String::from_utf8(attribute.value.into_owned())
                .map_err(|error| OsmError::invalid(error.to_string()));
        }
    }
    Err(OsmError::invalid(format!(
        "missing OSM XML attribute {}",
        String::from_utf8_lossy(name)
    )))
}

fn required_xml_i64(event: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Result<i64> {
    required_xml_string(event, name)?
        .parse::<i64>()
        .map_err(|error| OsmError::invalid(error.to_string()))
}

fn required_xml_f64(event: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Result<f64> {
    required_xml_string(event, name)?
        .parse::<f64>()
        .map_err(|error| OsmError::invalid(error.to_string()))
}

pub fn identify_xml_cache(directory: &Path) -> Result<OsmSourceIdentity> {
    if !directory.is_dir() {
        return Err(OsmError::invalid(format!(
            "OSM XML cache path is not a directory: {}",
            directory.display()
        )));
    }
    let mut files = osm_xml_files(directory)?;
    files.sort_by_key(|path| path.file_name().map(|name| name.to_os_string()));
    let mut overall = Sha256::new();
    let mut total_bytes = 0i64;
    for file in &files {
        let bytes = std::fs::read(file).map_err(|error| OsmError::invalid(error.to_string()))?;
        let file_hash = Sha256::digest(&bytes);
        let file_name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        total_bytes += bytes.len() as i64;
        overall.update(file_name.as_bytes());
        overall.update([0]);
        overall.update(bytes.len().to_string().as_bytes());
        overall.update([0]);
        overall.update(file_hash);
    }
    Ok(OsmSourceIdentity {
        kind: "xml-directory".to_string(),
        source_path: directory
            .canonicalize()
            .unwrap_or_else(|_| directory.to_path_buf()),
        file_count: files.len() as i32,
        total_bytes,
        sha256: hex_lower(&overall.finalize()),
    })
}

fn osm_xml_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in
        std::fs::read_dir(directory).map_err(|error| OsmError::invalid(error.to_string()))?
    {
        let path = entry
            .map_err(|error| OsmError::invalid(error.to_string()))?
            .path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".osm"))
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[derive(Clone, Copy, Debug)]
struct ProtoReader<'a> {
    bytes: &'a [u8],
    position: usize,
    current_wire_type: u8,
}

impl<'a> ProtoReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            current_wire_type: 0,
        }
    }

    fn exhausted(&self) -> bool {
        self.position >= self.bytes.len()
    }

    fn next_field(&mut self) -> Result<i32> {
        let key = self.read_varint_i64()?;
        self.current_wire_type = (key & 7) as u8;
        Ok((key >> 3) as i32)
    }

    fn read_varint_i64(&mut self) -> Result<i64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        while shift < 64 {
            if self.position >= self.bytes.len() {
                return Err(OsmError::invalid("truncated protobuf varint"));
            }
            let byte = self.bytes[self.position];
            self.position += 1;
            value |= u64::from(byte & 0x7F) << shift;
            if (byte & 0x80) == 0 {
                return Ok(value as i64);
            }
            shift += 7;
        }
        Err(OsmError::invalid("protobuf varint too long"))
    }

    fn read_sint64(&mut self) -> Result<i64> {
        let raw = self.read_varint_i64()? as u64;
        Ok(((raw >> 1) as i64) ^ -((raw & 1) as i64))
    }

    fn read_string(&mut self) -> Result<String> {
        String::from_utf8(self.read_bytes()?.to_vec())
            .map_err(|error| OsmError::invalid(error.to_string()))
    }

    fn read_bytes(&mut self) -> Result<&'a [u8]> {
        let length = self.read_varint_i64()?;
        if length < 0 {
            return Err(OsmError::invalid(format!(
                "invalid protobuf byte length: {length}"
            )));
        }
        let length = length as usize;
        if self.position + length > self.bytes.len() {
            return Err(OsmError::invalid(format!(
                "invalid protobuf byte length: {length}"
            )));
        }
        let start = self.position;
        self.position += length;
        Ok(&self.bytes[start..self.position])
    }

    fn skip_current_field(&mut self) -> Result<()> {
        match self.current_wire_type {
            0 => {
                self.read_varint_i64()?;
            }
            1 => self.skip(8)?,
            2 => {
                let count = self.read_varint_i64()?;
                if count < 0 {
                    return Err(OsmError::invalid("negative protobuf byte length"));
                }
                self.skip(count as usize)?;
            }
            5 => self.skip(4)?,
            other => {
                return Err(OsmError::invalid(format!(
                    "unsupported protobuf wire type {other}"
                )))
            }
        }
        Ok(())
    }

    fn skip(&mut self, count: usize) -> Result<()> {
        if self.position + count > self.bytes.len() {
            return Err(OsmError::invalid("truncated protobuf field"));
        }
        self.position += count;
        Ok(())
    }
}

fn read_i32_be(file: &mut File, position: u64) -> std::io::Result<i32> {
    let bytes = read_exact_at_io(file, position, 4)?;
    Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_exact_at(file: &mut File, position: u64, size: usize) -> Result<Vec<u8>> {
    read_exact_at_io(file, position, size).map_err(|error| OsmError::invalid(error.to_string()))
}

fn read_exact_at_io(file: &mut File, position: u64, size: usize) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(position))?;
    let mut out = vec![0u8; size];
    file.read_exact(&mut out)?;
    Ok(out)
}

fn elapsed_millis(start: Instant) -> i64 {
    start.elapsed().as_millis().try_into().unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn pbf_scanner_counts_synthetic_data_blob() {
        let temp = tempdir().unwrap();
        let pbf = temp.path().join("tiny.osm.pbf");
        std::fs::write(&pbf, synthetic_pbf()).unwrap();

        let report = scan_pbf(&pbf, 2).unwrap();

        assert_eq!(report.blobs_scanned, 2);
        assert_eq!(report.osm_header_blobs, 1);
        assert_eq!(report.osm_data_blobs, 1);
        assert_eq!(report.primitive_groups, 1);
        assert_eq!(report.dense_nodes, 2);
        assert_eq!(report.ways, 1);
        assert_eq!(report.highway_ways, 1);
    }

    #[test]
    fn pbf_range_scanner_counts_blobs_after_skip() {
        let temp = tempdir().unwrap();
        let pbf = temp.path().join("tiny.osm.pbf");
        std::fs::write(&pbf, synthetic_pbf()).unwrap();

        let report = scan_pbf_range(&pbf, 1, 1).unwrap();

        assert_eq!(report.blobs_scanned, 1);
        assert_eq!(report.osm_header_blobs, 0);
        assert_eq!(report.osm_data_blobs, 1);
        assert_eq!(report.dense_nodes, 2);
        assert_eq!(report.highway_ways, 1);
    }

    #[test]
    fn pbf_extract_rasterizes_synthetic_road() {
        let temp = tempdir().unwrap();
        let pbf = temp.path().join("tiny.osm.pbf");
        std::fs::write(&pbf, synthetic_pbf()).unwrap();
        let mapping = EarthScaleMapping::for_denominator(1000, -90.0, 90.0).unwrap();

        let result = extract_mask(&pbf, 2, &mapping, 0, 0).unwrap();

        assert_eq!(result.report.indexed_features, 1);
        assert_eq!(result.report.road_features, 1);
        assert!(result.report.road_mask_pixels > 0);
    }

    #[test]
    fn xml_identity_and_extract_use_sorted_osm_files() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join("xml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.osm"),
            r#"<osm>
<node id="1" lon="0.0" lat="0.0" />
<node id="2" lon="0.01" lat="0.0" />
<way id="7"><nd ref="1"/><nd ref="2"/><tag k="highway" v="primary"/></way>
</osm>"#,
        )
        .unwrap();
        std::fs::write(dir.join("ignore.txt"), "x").unwrap();
        let mapping = EarthScaleMapping::for_denominator(1000, -90.0, 90.0).unwrap();

        let identity = identify_xml_cache(&dir).unwrap();
        assert_eq!(identity.kind, "xml-directory");
        assert_eq!(identity.file_count, 1);
        assert!(identity.total_bytes > 0);
        assert_eq!(identity.sha256.len(), 64);

        let result = extract_xml_region_mask(&dir, &mapping, 0, 0).unwrap();
        assert_eq!(result.report.indexed_features, 1);
        assert_eq!(result.report.road_features, 1);
        assert!(result.report.road_mask_pixels > 0);
    }

    fn synthetic_pbf() -> Vec<u8> {
        let header_blob = blob_message(1, b"header");
        let data_block = primitive_block();
        let data_blob = blob_message(1, &data_block);
        let mut out = Vec::new();
        append_blob(&mut out, "OSMHeader", &header_blob);
        append_blob(&mut out, "OSMData", &data_blob);
        out
    }

    fn append_blob(out: &mut Vec<u8>, blob_type: &str, blob: &[u8]) {
        let mut header = Vec::new();
        field_bytes(&mut header, 1, blob_type.as_bytes());
        field_varint(&mut header, 3, blob.len() as u64);
        out.extend_from_slice(&(header.len() as u32).to_be_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(blob);
    }

    fn primitive_block() -> Vec<u8> {
        let strings = ["", "highway", "primary"];
        let mut string_table = Vec::new();
        for value in strings {
            field_bytes(&mut string_table, 1, value.as_bytes());
        }
        let mut dense = Vec::new();
        field_bytes(&mut dense, 1, &packed_sint64(&[1, 1]));
        field_bytes(&mut dense, 8, &packed_sint64(&[0, 0]));
        field_bytes(&mut dense, 9, &packed_sint64(&[0, 100_000]));
        let mut way = Vec::new();
        field_varint(&mut way, 1, 5);
        field_bytes(&mut way, 2, &packed_varints(&[1]));
        field_bytes(&mut way, 3, &packed_varints(&[2]));
        field_bytes(&mut way, 8, &packed_sint64(&[1, 1]));
        let mut group = Vec::new();
        field_bytes(&mut group, 2, &dense);
        field_bytes(&mut group, 3, &way);
        let mut block = Vec::new();
        field_bytes(&mut block, 1, &string_table);
        field_bytes(&mut block, 2, &group);
        block
    }

    fn blob_message(field: u32, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        field_bytes(&mut out, field, bytes);
        out
    }

    fn field_varint(out: &mut Vec<u8>, field: u32, value: u64) {
        write_varint(out, u64::from(field << 3));
        write_varint(out, value);
    }

    fn field_bytes(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
        write_varint(out, u64::from((field << 3) | 2));
        write_varint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
    }

    fn packed_varints(values: &[u64]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            write_varint(&mut out, *value);
        }
        out
    }

    fn packed_sint64(values: &[i64]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            write_varint(&mut out, zigzag(*value));
        }
        out
    }

    fn zigzag(value: i64) -> u64 {
        ((value << 1) ^ (value >> 63)) as u64
    }

    fn write_varint(out: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            out.push((value as u8) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }
}
