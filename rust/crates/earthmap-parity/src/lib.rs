#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write;
use std::path::Path;

use earthmap_region::{read_region_payloads, ChunkLocalPos, RegionPayloads};
use sha2::{Digest, Sha256};

pub const MODULE_STATUS: &str = "phase0-parity-manifests";
pub const CHUNK_PAYLOAD_MANIFEST_HEADER: &str =
    "format,regionX,regionZ,localChunkX,localChunkZ,payloadBytes,payloadSha256";

pub type Result<T> = std::result::Result<T, ParityError>;

#[derive(Debug)]
pub enum ParityError {
    Io(std::io::Error),
    Region(earthmap_region::RegionError),
    Invalid(String),
}

impl fmt::Display for ParityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParityError::Io(error) => write!(f, "{error}"),
            ParityError::Region(error) => write!(f, "{error}"),
            ParityError::Invalid(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ParityError {}

impl From<std::io::Error> for ParityError {
    fn from(error: std::io::Error) -> Self {
        ParityError::Io(error)
    }
}

impl From<earthmap_region::RegionError> for ParityError {
    fn from(error: earthmap_region::RegionError) -> Self {
        ParityError::Region(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionManifestReport {
    pub format: String,
    pub region_x: i32,
    pub region_z: i32,
    pub compared_chunks: usize,
    pub matching_chunks: usize,
    pub missing_chunks: usize,
    pub extra_chunks: usize,
    pub mismatched_chunks: usize,
    pub first_mismatch: Option<ChunkLocalPos>,
}

impl RegionManifestReport {
    pub fn matches(&self) -> bool {
        self.missing_chunks == 0 && self.extra_chunks == 0 && self.mismatched_chunks == 0
    }
}

pub fn write_sha256_manifest(root: impl AsRef<Path>, output: impl AsRef<Path>) -> Result<()> {
    let root = root.as_ref();
    let output = output.as_ref();
    let output_abs = output
        .canonicalize()
        .unwrap_or_else(|_| output.to_path_buf());
    let mut rows = Vec::new();

    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.map_err(|error| ParityError::Invalid(error.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let path_abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if path_abs == output_abs {
            continue;
        }
        let relative = normalize_relative_path(root, path)?;
        rows.push((relative, sha256_file_hex(path)?));
    }

    rows.sort_by(|left, right| left.0.cmp(&right.0));
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(output)?;
    for (relative, hash) in rows {
        writeln!(file, "{hash}  {relative}")?;
    }
    Ok(())
}

pub fn write_region_payload_manifest(
    region: impl AsRef<Path>,
    output: impl AsRef<Path>,
) -> Result<()> {
    let payloads = read_region_payloads(region)?;
    if let Some(parent) = output.as_ref().parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(output)?;
    write_region_payload_manifest_to(&payloads, &mut file, true)
}

pub fn append_region_payload_manifest(
    region: impl AsRef<Path>,
    output: impl AsRef<Path>,
) -> Result<()> {
    let payloads = read_region_payloads(region)?;
    let output = output.as_ref();
    let include_header = !output.exists() || output.metadata()?.len() == 0;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(output)?;
    write_region_payload_manifest_to(&payloads, &mut file, include_header)
}

pub fn compare_region_payload_manifest(
    manifest: impl AsRef<Path>,
    region: impl AsRef<Path>,
) -> Result<RegionManifestReport> {
    let region = read_region_payloads(region)?;
    let expected = read_manifest_rows(manifest, &region)?;
    let mut matching_chunks = 0;
    let mut missing_chunks = 0;
    let mut extra_chunks = 0;
    let mut mismatched_chunks = 0;
    let mut first_mismatch = None;

    let expected_positions = expected.keys().copied().collect::<BTreeSet<_>>();
    let actual_positions = region.chunks.keys().copied().collect::<BTreeSet<_>>();
    for pos in expected_positions.union(&actual_positions) {
        match (expected.get(pos), region.chunks.get(pos)) {
            (Some(expected), Some(actual)) => {
                let actual_hash = sha256_bytes_hex(actual);
                if expected.payload_bytes == actual.len() && expected.payload_sha256 == actual_hash
                {
                    matching_chunks += 1;
                } else {
                    mismatched_chunks += 1;
                    first_mismatch = first_mismatch.or(Some(*pos));
                }
            }
            (Some(_), None) => {
                missing_chunks += 1;
                first_mismatch = first_mismatch.or(Some(*pos));
            }
            (None, Some(_)) => {
                extra_chunks += 1;
                first_mismatch = first_mismatch.or(Some(*pos));
            }
            (None, None) => unreachable!("union never yields positions missing from both sets"),
        }
    }

    Ok(RegionManifestReport {
        format: region.format.as_manifest_value().to_string(),
        region_x: region.region_x,
        region_z: region.region_z,
        compared_chunks: expected_positions.union(&actual_positions).count(),
        matching_chunks,
        missing_chunks,
        extra_chunks,
        mismatched_chunks,
        first_mismatch,
    })
}

fn write_region_payload_manifest_to(
    payloads: &RegionPayloads,
    writer: &mut impl Write,
    include_header: bool,
) -> Result<()> {
    if include_header {
        writeln!(writer, "{CHUNK_PAYLOAD_MANIFEST_HEADER}")?;
    }
    for (pos, payload) in &payloads.chunks {
        writeln!(
            writer,
            "{},{},{},{},{},{},{}",
            payloads.format.as_manifest_value(),
            payloads.region_x,
            payloads.region_z,
            pos.x,
            pos.z,
            payload.len(),
            sha256_bytes_hex(payload)
        )?;
    }
    Ok(())
}

fn read_manifest_rows(
    manifest: impl AsRef<Path>,
    region: &RegionPayloads,
) -> Result<BTreeMap<ChunkLocalPos, ManifestRow>> {
    let text = std::fs::read_to_string(manifest)?;
    let mut rows = BTreeMap::new();
    for (line_index, line) in text.lines().enumerate() {
        if line_index == 0 {
            if line != CHUNK_PAYLOAD_MANIFEST_HEADER {
                return Err(ParityError::Invalid(format!(
                    "unexpected chunk payload manifest header: {line}"
                )));
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let columns = line.split(',').collect::<Vec<_>>();
        if columns.len() != 7 {
            return Err(ParityError::Invalid(format!(
                "invalid chunk payload manifest row {}: expected 7 columns",
                line_index + 1
            )));
        }
        if columns[0] != region.format.as_manifest_value()
            || parse_i32(columns[1], "regionX")? != region.region_x
            || parse_i32(columns[2], "regionZ")? != region.region_z
        {
            continue;
        }
        let pos = ChunkLocalPos::new(
            parse_u8(columns[3], "localChunkX")?,
            parse_u8(columns[4], "localChunkZ")?,
        )
        .map_err(ParityError::Region)?;
        rows.insert(
            pos,
            ManifestRow {
                payload_bytes: parse_usize(columns[5], "payloadBytes")?,
                payload_sha256: columns[6].to_string(),
            },
        );
    }
    Ok(rows)
}

pub fn sha256_file_hex(path: impl AsRef<Path>) -> Result<String> {
    Ok(sha256_bytes_hex(&std::fs::read(path)?))
}

pub fn sha256_bytes_hex(bytes: &[u8]) -> String {
    hex_lower(Sha256::digest(bytes).as_slice())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

fn normalize_relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|error| ParityError::Invalid(error.to_string()))?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn parse_i32(text: &str, name: &str) -> Result<i32> {
    text.parse::<i32>()
        .map_err(|error| ParityError::Invalid(format!("invalid {name}: {error}")))
}

fn parse_u8(text: &str, name: &str) -> Result<u8> {
    text.parse::<u8>()
        .map_err(|error| ParityError::Invalid(format!("invalid {name}: {error}")))
}

fn parse_usize(text: &str, name: &str) -> Result<usize> {
    text.parse::<usize>()
        .map_err(|error| ParityError::Invalid(format!("invalid {name}: {error}")))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestRow {
    payload_bytes: usize,
    payload_sha256: String,
}
