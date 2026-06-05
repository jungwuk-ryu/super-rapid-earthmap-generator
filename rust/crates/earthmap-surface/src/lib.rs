#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::LocalKey;
use std::time::Instant;

use earthmap_core::build_info;
use earthmap_geo::{
    EarthScaleMapping, GeoError, GeoTiffFloat32Reader, GeoTiffHeightmapReader, GeoTiffMetadata,
    GeoTiffRgbReader, GeoTiffRowCache, GeoTiffRowCacheStats, GeoTiffSingleBandReader,
    HeightmapScalarSampler, RgbColor, VrtRgbMosaicReader, DEFAULT_RGB_TILE_CACHE_ENTRIES,
};
use earthmap_minecraft::block_state_ids;
use earthmap_minecraft::chunk_generation_status::ChunkGenerationStatus;
use earthmap_minecraft::chunk_model::{ChunkModel, BIOME_CELL_WIDTH, CHUNK_WIDTH};
use earthmap_minecraft::{chunk_nbt_encoder, level_dat_template, MinecraftError};
use earthmap_region::{ChunkLocalPos, RegionError};
use quick_xml::events::Event;
use quick_xml::Reader;
use rayon::prelude::*;

pub const MODULE_STATUS: &str = "phase5-surface-bootstrap";

pub const SEA_LEVEL_Y: i32 = 63;
pub const ELEVATION_METERS_PER_BLOCK: f64 = 35.0;
pub const REGION_CHUNKS: i32 = 32;
pub const REGION_SIZE_BLOCKS: i32 = REGION_CHUNKS * CHUNK_WIDTH as i32;
pub const DEFAULT_HEIGHT_ONLY_CACHE_ROWS: usize = 64;
pub const DEFAULT_SURFACE_TILE_CACHE_ENTRIES: usize = DEFAULT_RGB_TILE_CACHE_ENTRIES;
pub const SURVIVAL_MANIFEST_FILE_NAME: &str = "earthmap-survival.properties";

static PHOTO_SURFACE_TRACE_ENABLED: OnceLock<bool> = OnceLock::new();

fn photo_surface_trace_enabled() -> bool {
    cfg!(test)
        || *PHOTO_SURFACE_TRACE_ENABLED.get_or_init(|| {
            std::env::var("EARTHMAP_PHOTO_DECISION_TRACE")
                .map(|value| {
                    matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                })
                .unwrap_or(false)
        })
}

fn photo_surface_trace(args: fmt::Arguments<'_>) -> String {
    if photo_surface_trace_enabled() {
        args.to_string()
    } else {
        String::new()
    }
}

pub type Result<T> = std::result::Result<T, SurfaceError>;

#[derive(Debug)]
pub enum SurfaceError {
    Io(std::io::Error),
    Geo(GeoError),
    Minecraft(MinecraftError),
    Region(RegionError),
    Invalid(String),
}

impl SurfaceError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SurfaceError::Io(error) => write!(f, "{error}"),
            SurfaceError::Geo(error) => write!(f, "{error}"),
            SurfaceError::Minecraft(error) => write!(f, "{error}"),
            SurfaceError::Region(error) => write!(f, "{error}"),
            SurfaceError::Invalid(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for SurfaceError {}

impl From<std::io::Error> for SurfaceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<GeoError> for SurfaceError {
    fn from(error: GeoError) -> Self {
        Self::Geo(error)
    }
}

impl From<MinecraftError> for SurfaceError {
    fn from(error: MinecraftError) -> Self {
        Self::Minecraft(error)
    }
}

impl From<RegionError> for SurfaceError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Mca,
    LinearV2,
}

impl OutputFormat {
    pub fn java_name(self) -> &'static str {
        match self {
            Self::Mca => "MCA",
            Self::LinearV2 => "LINEAR_V2",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text.to_ascii_lowercase().as_str() {
            "mca" => Ok(Self::Mca),
            "linear" | "linear_v2" => Ok(Self::LinearV2),
            _ => Err(SurfaceError::invalid("format must be mca or linear")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceTextureMode {
    Classified,
    Photo,
}

impl SurfaceTextureMode {
    pub const DEFAULT: Self = Self::Photo;

    pub fn id(self) -> &'static str {
        match self {
            Self::Classified => "classified",
            Self::Photo => "photo",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        if text.trim().is_empty() {
            return Err(SurfaceError::invalid("textureMode must not be blank"));
        }
        let normalized = text.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "classified" | "classify" | "semantic" | "terrain" => Ok(Self::Classified),
            "photo" | "photographic" | "satellite" | "satellite-photo" | "true-marble"
            | "truemarble" => Ok(Self::Photo),
            _ => Err(SurfaceError::invalid(format!(
                "textureMode must be classified or photo: {text}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeightOnlySettings {
    pub heightmap_path: PathBuf,
    pub world_dir: PathBuf,
    pub level_name: String,
    pub seed: i64,
    pub scale_denominator: i32,
    pub region_x: i32,
    pub region_z: i32,
    pub output_format: OutputFormat,
    pub cache_rows: usize,
}

impl HeightOnlySettings {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        heightmap_path: impl Into<PathBuf>,
        world_dir: impl Into<PathBuf>,
        level_name: impl Into<String>,
        seed: i64,
        scale_denominator: i32,
        region_x: i32,
        region_z: i32,
        output_format: OutputFormat,
        cache_rows: usize,
    ) -> Result<Self> {
        let level_name = level_name.into();
        if level_name.trim().is_empty() {
            return Err(SurfaceError::invalid("levelName must not be blank"));
        }
        if scale_denominator <= 0 {
            return Err(SurfaceError::invalid("scaleDenominator must be positive"));
        }
        if cache_rows == 0 {
            return Err(SurfaceError::invalid("cacheRows must be positive"));
        }
        Ok(Self {
            heightmap_path: heightmap_path.into(),
            world_dir: world_dir.into(),
            level_name,
            seed,
            scale_denominator,
            region_x,
            region_z,
            output_format,
            cache_rows,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HeightOnlyRegionReport {
    pub region_x: i32,
    pub region_z: i32,
    pub output_format: OutputFormat,
    pub scale_denominator: i32,
    pub chunk_count: usize,
    pub min_surface_y: i32,
    pub max_surface_y: i32,
    pub region_file: PathBuf,
    pub cache_stats: GeoTiffRowCacheStats,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceRegionSettings {
    pub heightmap_path: PathBuf,
    pub world_dir: PathBuf,
    pub level_name: String,
    pub seed: i64,
    pub scale_denominator: i32,
    pub region_x: i32,
    pub region_z: i32,
    pub output_format: OutputFormat,
    pub cache_rows: usize,
    pub write_world_metadata: bool,
    pub chunk_status: ChunkGenerationStatus,
    pub vertical_scale: f64,
    pub texture_mode: SurfaceTextureMode,
    pub surface_material_path: Option<PathBuf>,
    pub surface_tile_cache_entries: usize,
    pub parallel_column_sampling: bool,
    pub mca_compression_level: Option<u32>,
    pub linear_compression_level: Option<i32>,
}

impl SurfaceRegionSettings {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        heightmap_path: impl Into<PathBuf>,
        world_dir: impl Into<PathBuf>,
        level_name: impl Into<String>,
        seed: i64,
        scale_denominator: i32,
        region_x: i32,
        region_z: i32,
        output_format: OutputFormat,
        cache_rows: usize,
    ) -> Result<Self> {
        Self::new_with_options(
            heightmap_path,
            world_dir,
            level_name,
            seed,
            scale_denominator,
            region_x,
            region_z,
            output_format,
            cache_rows,
            true,
            ChunkGenerationStatus::Full,
            DEFAULT_VERTICAL_SCALE,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_options(
        heightmap_path: impl Into<PathBuf>,
        world_dir: impl Into<PathBuf>,
        level_name: impl Into<String>,
        seed: i64,
        scale_denominator: i32,
        region_x: i32,
        region_z: i32,
        output_format: OutputFormat,
        cache_rows: usize,
        write_world_metadata: bool,
        chunk_status: ChunkGenerationStatus,
        vertical_scale: f64,
    ) -> Result<Self> {
        Self::new_with_texture_options(
            heightmap_path,
            world_dir,
            level_name,
            seed,
            scale_denominator,
            region_x,
            region_z,
            output_format,
            cache_rows,
            write_world_metadata,
            chunk_status,
            vertical_scale,
            SurfaceTextureMode::DEFAULT,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_texture_options(
        heightmap_path: impl Into<PathBuf>,
        world_dir: impl Into<PathBuf>,
        level_name: impl Into<String>,
        seed: i64,
        scale_denominator: i32,
        region_x: i32,
        region_z: i32,
        output_format: OutputFormat,
        cache_rows: usize,
        write_world_metadata: bool,
        chunk_status: ChunkGenerationStatus,
        vertical_scale: f64,
        texture_mode: SurfaceTextureMode,
    ) -> Result<Self> {
        let level_name = level_name.into();
        if level_name.trim().is_empty() {
            return Err(SurfaceError::invalid("levelName must not be blank"));
        }
        if scale_denominator <= 0 {
            return Err(SurfaceError::invalid("scaleDenominator must be positive"));
        }
        if cache_rows == 0 {
            return Err(SurfaceError::invalid("cacheRows must be positive"));
        }
        let vertical_scale = require_valid_vertical_scale(vertical_scale)?;
        Ok(Self {
            heightmap_path: heightmap_path.into(),
            world_dir: world_dir.into(),
            level_name,
            seed,
            scale_denominator,
            region_x,
            region_z,
            output_format,
            cache_rows,
            write_world_metadata,
            chunk_status,
            vertical_scale,
            texture_mode,
            surface_material_path: None,
            surface_tile_cache_entries: DEFAULT_SURFACE_TILE_CACHE_ENTRIES,
            parallel_column_sampling: true,
            mca_compression_level: None,
            linear_compression_level: None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceRegionReport {
    pub region_x: i32,
    pub region_z: i32,
    pub output_format: OutputFormat,
    pub scale_denominator: i32,
    pub chunk_count: usize,
    pub land_columns: i32,
    pub water_columns: i32,
    pub min_ground_y: i32,
    pub max_ground_y: i32,
    pub region_file: PathBuf,
    pub preview_tile_file: Option<PathBuf>,
    pub cache_stats: GeoTiffRowCacheStats,
    pub surface_material_raster_stats: SurfaceMaterialRasterStats,
    pub surface_sample_nanos: u128,
    pub chunk_build_nanos: u128,
    pub nbt_encode_nanos: u128,
    pub region_write_nanos: u128,
    pub preview_nanos: u128,
    pub metadata_nanos: u128,
    pub total_nanos: u128,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceRegionColumnTrace {
    pub local_x: usize,
    pub local_z: usize,
    pub global_block_x: i32,
    pub global_block_z: i32,
    pub map_x: i32,
    pub map_z: i32,
    pub longitude: f64,
    pub latitude: f64,
    pub raw_elevation_meters: f64,
    pub smoothed_elevation_meters: f64,
    pub local_relief_meters: f64,
    pub initial_water: bool,
    pub valid: bool,
    pub coast_factor: f64,
    pub material_sample: Option<SurfaceMaterialSample>,
    pub base_column: EarthSurfaceColumn,
    pub semantic_column: Option<EarthSurfaceColumn>,
    pub photo_column: Option<EarthSurfaceColumn>,
    pub post_cell_column: Option<EarthSurfaceColumn>,
    pub post_stabilized_column: Option<EarthSurfaceColumn>,
    pub post_first_component_trace: Option<SurfaceBiomeComponentTrace>,
    pub post_smoothed_column: Option<EarthSurfaceColumn>,
    pub post_component_column: Option<EarthSurfaceColumn>,
    pub post_component_trace: Option<SurfaceBiomeComponentTrace>,
    pub final_column: EarthSurfaceColumn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceBiomeComponentTrace {
    pub family: String,
    pub size: usize,
    pub min_local_x: usize,
    pub min_local_z: usize,
    pub max_local_x: usize,
    pub max_local_z: usize,
    pub neighbor_majority_biome: Option<String>,
    pub neighbor_counts: Vec<(String, i32)>,
    pub action: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceChunkBuild {
    pub chunk: ChunkModel,
    pub land_columns: i32,
    pub water_columns: i32,
    pub min_ground_y: i32,
    pub max_ground_y: i32,
    pub biome: String,
    pub ground_surface_y_by_local_column: Vec<i32>,
    pub water_by_local_column: Vec<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceChunkSample {
    columns: Vec<EarthSurfaceColumn>,
}

impl SurfaceChunkSample {
    pub fn new(columns: Vec<EarthSurfaceColumn>) -> Result<Self> {
        if columns.len() != CHUNK_WIDTH * CHUNK_WIDTH {
            return Err(SurfaceError::invalid(
                "columns must contain one entry per chunk column",
            ));
        }
        Ok(Self { columns })
    }

    pub fn columns(&self) -> &[EarthSurfaceColumn] {
        &self.columns
    }

    pub fn column(&self, local_x: i32, local_z: i32) -> Result<&EarthSurfaceColumn> {
        if local_x < 0
            || local_x >= CHUNK_WIDTH as i32
            || local_z < 0
            || local_z >= CHUNK_WIDTH as i32
        {
            return Err(SurfaceError::invalid(format!(
                "local column outside chunk: {local_x},{local_z}"
            )));
        }
        Ok(&self.columns[(local_z as usize * CHUNK_WIDTH) + local_x as usize])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceRegionSample {
    columns: Vec<EarthSurfaceColumn>,
}

impl SurfaceRegionSample {
    pub fn new(columns: Vec<EarthSurfaceColumn>) -> Result<Self> {
        if columns.len() != SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH {
            return Err(SurfaceError::invalid(
                "columns must contain one entry per region column",
            ));
        }
        Ok(Self { columns })
    }

    pub fn width(&self) -> usize {
        SURFACE_REGION_WIDTH
    }

    pub fn columns(&self) -> &[EarthSurfaceColumn] {
        &self.columns
    }

    pub fn column(&self, local_x: i32, local_z: i32) -> Result<&EarthSurfaceColumn> {
        if local_x < 0
            || local_x >= SURFACE_REGION_WIDTH as i32
            || local_z < 0
            || local_z >= SURFACE_REGION_WIDTH as i32
        {
            return Err(SurfaceError::invalid(format!(
                "local column outside region: {local_x},{local_z}"
            )));
        }
        Ok(&self.columns
            [surface_class_index(local_x as usize, local_z as usize, SURFACE_REGION_WIDTH)])
    }

    pub fn chunk_sample(
        &self,
        local_chunk_x: i32,
        local_chunk_z: i32,
    ) -> Result<SurfaceChunkSample> {
        if local_chunk_x < 0
            || local_chunk_x >= REGION_CHUNKS
            || local_chunk_z < 0
            || local_chunk_z >= REGION_CHUNKS
        {
            return Err(SurfaceError::invalid(format!(
                "local chunk outside region: {local_chunk_x},{local_chunk_z}"
            )));
        }
        let mut chunk_columns = Vec::with_capacity(CHUNK_WIDTH * CHUNK_WIDTH);
        let block_x = local_chunk_x as usize * CHUNK_WIDTH;
        let block_z = local_chunk_z as usize * CHUNK_WIDTH;
        for local_z in 0..CHUNK_WIDTH {
            for local_x in 0..CHUNK_WIDTH {
                let index =
                    surface_class_index(block_x + local_x, block_z + local_z, SURFACE_REGION_WIDTH);
                chunk_columns.push(self.columns[index].clone());
            }
        }
        SurfaceChunkSample::new(chunk_columns)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerrainTokenSource {
    None,
    Export,
    JavaStandardPalette,
}

pub const OSM_OVERLAY_ROAD: i32 = 1;
pub const OSM_OVERLAY_WATERWAY: i32 = 1 << 1;
pub const OSM_OVERLAY_LANDUSE: i32 = 1 << 2;
pub const OSM_OVERLAY_BUILDING: i32 = 1 << 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OsmFeatureKind {
    Road,
    Waterway,
    Landuse,
    Building,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OsmRegionFeatureMask {
    flags: Vec<i32>,
}

impl OsmRegionFeatureMask {
    pub fn new() -> Self {
        Self {
            flags: vec![0; (REGION_SIZE_BLOCKS * REGION_SIZE_BLOCKS) as usize],
        }
    }

    pub fn mark(&mut self, kind: OsmFeatureKind, local_x: i32, local_z: i32) {
        let Some(index) = osm_region_mask_index(local_x, local_z) else {
            return;
        };
        self.flags[index] |= osm_feature_flag(kind);
    }

    pub fn mark_line(&mut self, kind: OsmFeatureKind, x0: i32, z0: i32, x1: i32, z1: i32) {
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

    pub fn road_at(&self, local_x: i32, local_z: i32) -> Result<bool> {
        Ok((self.flags_at(local_x, local_z)? & OSM_OVERLAY_ROAD) != 0)
    }

    pub fn waterway_at(&self, local_x: i32, local_z: i32) -> Result<bool> {
        Ok((self.flags_at(local_x, local_z)? & OSM_OVERLAY_WATERWAY) != 0)
    }

    pub fn landuse_at(&self, local_x: i32, local_z: i32) -> Result<bool> {
        Ok((self.flags_at(local_x, local_z)? & OSM_OVERLAY_LANDUSE) != 0)
    }

    pub fn building_at(&self, local_x: i32, local_z: i32) -> Result<bool> {
        Ok((self.flags_at(local_x, local_z)? & OSM_OVERLAY_BUILDING) != 0)
    }

    pub fn flags_at(&self, local_x: i32, local_z: i32) -> Result<i32> {
        let Some(index) = osm_region_mask_index(local_x, local_z) else {
            return Err(SurfaceError::invalid(format!(
                "local coordinate outside region: {local_x},{local_z}"
            )));
        };
        Ok(self.flags[index])
    }

    pub fn road_count(&self) -> usize {
        self.flags
            .iter()
            .filter(|&&flags| (flags & OSM_OVERLAY_ROAD) != 0)
            .count()
    }

    pub fn waterway_count(&self) -> usize {
        self.flags
            .iter()
            .filter(|&&flags| (flags & OSM_OVERLAY_WATERWAY) != 0)
            .count()
    }

    pub fn landuse_count(&self) -> usize {
        self.flags
            .iter()
            .filter(|&&flags| (flags & OSM_OVERLAY_LANDUSE) != 0)
            .count()
    }

    pub fn building_count(&self) -> usize {
        self.flags
            .iter()
            .filter(|&&flags| (flags & OSM_OVERLAY_BUILDING) != 0)
            .count()
    }
}

impl Default for OsmRegionFeatureMask {
    fn default() -> Self {
        Self::new()
    }
}

fn osm_feature_flag(kind: OsmFeatureKind) -> i32 {
    match kind {
        OsmFeatureKind::Road => OSM_OVERLAY_ROAD,
        OsmFeatureKind::Waterway => OSM_OVERLAY_WATERWAY,
        OsmFeatureKind::Landuse => OSM_OVERLAY_LANDUSE,
        OsmFeatureKind::Building => OSM_OVERLAY_BUILDING,
    }
}

fn osm_region_mask_index(local_x: i32, local_z: i32) -> Option<usize> {
    if !(0..REGION_SIZE_BLOCKS).contains(&local_x) || !(0..REGION_SIZE_BLOCKS).contains(&local_z) {
        return None;
    }
    Some(((local_z * REGION_SIZE_BLOCKS) + local_x) as usize)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EarthSurfaceColumn {
    pub water: bool,
    pub ground_surface_y: i32,
    pub water_surface_y: i32,
    pub top_block_state_id: i32,
    pub filler_block_state_id: i32,
    pub biome_id: String,
    pub decision_source: String,
    pub terrain_token_source: TerrainTokenSource,
    pub data_evidence_flags: i32,
}

impl EarthSurfaceColumn {
    pub fn new(
        water: bool,
        ground_surface_y: i32,
        water_surface_y: i32,
        top_block_state_id: i32,
        filler_block_state_id: i32,
        biome_id: impl Into<String>,
        decision_source: impl Into<String>,
    ) -> Self {
        let biome_id = normalize_text_default(biome_id.into(), "minecraft:plains");
        let decision_source = normalize_text_default(decision_source.into(), "unknown");
        Self {
            water,
            ground_surface_y,
            water_surface_y,
            top_block_state_id,
            filler_block_state_id,
            biome_id,
            decision_source,
            terrain_token_source: TerrainTokenSource::None,
            data_evidence_flags: 0,
        }
    }

    pub fn with_decision_source(&self, source: impl Into<String>) -> Self {
        let mut column = self.clone();
        column.decision_source = normalize_text_default(source.into(), "unknown");
        column
    }

    pub fn with_biome_id(&self, biome: impl Into<String>) -> Self {
        let mut column = self.clone();
        column.biome_id = normalize_text_default(biome.into(), "minecraft:plains");
        column
    }

    pub fn with_terrain_token_source(&self, source: TerrainTokenSource) -> Self {
        let mut column = self.clone();
        column.terrain_token_source = source;
        column
    }

    pub fn with_terrain_token_available(&self, available: bool) -> Self {
        self.with_terrain_token_source(if available {
            TerrainTokenSource::Export
        } else {
            TerrainTokenSource::None
        })
    }

    pub fn with_data_evidence_flags(&self, flags: i32) -> Self {
        let mut column = self.clone();
        column.data_evidence_flags = flags;
        column
    }

    pub fn has_data_evidence(&self, mask: i32) -> bool {
        (self.data_evidence_flags & mask) != 0
    }

    pub fn terrain_token_available(&self) -> bool {
        self.terrain_token_source != TerrainTokenSource::None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceMaterialSample {
    pub color: RgbColor,
    pub terrain_token_color: RgbColor,
    pub terrain_token_source: TerrainTokenSource,
    pub climate_class: i32,
    pub evergreen_broadleaf_trees: i32,
    pub deciduous_broadleaf_trees: i32,
    pub needleleaf_trees: i32,
    pub mixed_trees: i32,
    pub herbaceous_vegetation: i32,
    pub shrubs: i32,
    pub snow_cover: i32,
    pub swamp_cover: i32,
    pub ocean_temperature: i32,
    pub bathymetry_meters: i32,
    pub slope_permille: i32,
    pub ecoregion_name: String,
    pub ecoregion_biome_id: String,
    pub ecoregion_confidence: f64,
}

impl SurfaceMaterialSample {
    pub const UNKNOWN: i32 = -1;

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        color: RgbColor,
        terrain_token_color: RgbColor,
        terrain_token_source: TerrainTokenSource,
        climate_class: i32,
        evergreen_broadleaf_trees: i32,
        deciduous_broadleaf_trees: i32,
        needleleaf_trees: i32,
        mixed_trees: i32,
        herbaceous_vegetation: i32,
        shrubs: i32,
        snow_cover: i32,
        swamp_cover: i32,
        ocean_temperature: i32,
        bathymetry_meters: i32,
        slope_permille: i32,
        ecoregion_name: impl Into<String>,
        ecoregion_biome_id: impl Into<String>,
        ecoregion_confidence: f64,
    ) -> Self {
        let terrain_token_source = if terrain_token_color.available {
            terrain_token_source
        } else {
            TerrainTokenSource::None
        };
        Self {
            color,
            terrain_token_color,
            terrain_token_source,
            climate_class,
            evergreen_broadleaf_trees,
            deciduous_broadleaf_trees,
            needleleaf_trees,
            mixed_trees,
            herbaceous_vegetation,
            shrubs,
            snow_cover,
            swamp_cover,
            ocean_temperature,
            bathymetry_meters,
            slope_permille,
            ecoregion_name: ecoregion_name.into().trim().to_string(),
            ecoregion_biome_id: ecoregion_biome_id.into().trim().to_string(),
            ecoregion_confidence: if ecoregion_confidence.is_finite() {
                ecoregion_confidence.clamp(0.0, 1.0)
            } else {
                0.0
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_export_token(
        color: RgbColor,
        terrain_token_color: RgbColor,
        climate_class: i32,
        evergreen_broadleaf_trees: i32,
        deciduous_broadleaf_trees: i32,
        needleleaf_trees: i32,
        mixed_trees: i32,
        herbaceous_vegetation: i32,
        shrubs: i32,
        snow_cover: i32,
        swamp_cover: i32,
        ocean_temperature: i32,
        bathymetry_meters: i32,
        slope_permille: i32,
        ecoregion_name: impl Into<String>,
        ecoregion_biome_id: impl Into<String>,
        ecoregion_confidence: f64,
    ) -> Self {
        Self::new(
            color,
            terrain_token_color,
            TerrainTokenSource::Export,
            climate_class,
            evergreen_broadleaf_trees,
            deciduous_broadleaf_trees,
            needleleaf_trees,
            mixed_trees,
            herbaceous_vegetation,
            shrubs,
            snow_cover,
            swamp_cover,
            ocean_temperature,
            bathymetry_meters,
            slope_permille,
            ecoregion_name,
            ecoregion_biome_id,
            ecoregion_confidence,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn land(
        color: RgbColor,
        climate_class: i32,
        evergreen_broadleaf_trees: i32,
        deciduous_broadleaf_trees: i32,
        needleleaf_trees: i32,
        mixed_trees: i32,
        herbaceous_vegetation: i32,
        shrubs: i32,
        snow_cover: i32,
        swamp_cover: i32,
        ocean_temperature: i32,
        bathymetry_meters: i32,
        slope_permille: i32,
        ecoregion_name: impl Into<String>,
        ecoregion_biome_id: impl Into<String>,
        ecoregion_confidence: f64,
    ) -> Self {
        Self::new(
            color,
            RgbColor::unavailable(),
            TerrainTokenSource::None,
            climate_class,
            evergreen_broadleaf_trees,
            deciduous_broadleaf_trees,
            needleleaf_trees,
            mixed_trees,
            herbaceous_vegetation,
            shrubs,
            snow_cover,
            swamp_cover,
            ocean_temperature,
            bathymetry_meters,
            slope_permille,
            ecoregion_name,
            ecoregion_biome_id,
            ecoregion_confidence,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn land_with_inferred_ecoregion_confidence(
        color: RgbColor,
        climate_class: i32,
        evergreen_broadleaf_trees: i32,
        deciduous_broadleaf_trees: i32,
        needleleaf_trees: i32,
        mixed_trees: i32,
        herbaceous_vegetation: i32,
        shrubs: i32,
        snow_cover: i32,
        swamp_cover: i32,
        ocean_temperature: i32,
        bathymetry_meters: i32,
        slope_permille: i32,
        ecoregion_name: impl Into<String>,
        ecoregion_biome_id: impl Into<String>,
    ) -> Self {
        let ecoregion_biome_id = ecoregion_biome_id.into();
        let ecoregion_confidence = if ecoregion_biome_id.trim().is_empty() {
            0.0
        } else {
            1.0
        };
        Self::land(
            color,
            climate_class,
            evergreen_broadleaf_trees,
            deciduous_broadleaf_trees,
            needleleaf_trees,
            mixed_trees,
            herbaceous_vegetation,
            shrubs,
            snow_cover,
            swamp_cover,
            ocean_temperature,
            bathymetry_meters,
            slope_permille,
            ecoregion_name,
            ecoregion_biome_id,
            ecoregion_confidence,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn climate_only(
        color: RgbColor,
        climate_class: i32,
        evergreen_broadleaf_trees: i32,
        deciduous_broadleaf_trees: i32,
        needleleaf_trees: i32,
        mixed_trees: i32,
        herbaceous_vegetation: i32,
        shrubs: i32,
        snow_cover: i32,
        swamp_cover: i32,
        ocean_temperature: i32,
    ) -> Self {
        Self::land(
            color,
            climate_class,
            evergreen_broadleaf_trees,
            deciduous_broadleaf_trees,
            needleleaf_trees,
            mixed_trees,
            herbaceous_vegetation,
            shrubs,
            snow_cover,
            swamp_cover,
            ocean_temperature,
            Self::UNKNOWN,
            Self::UNKNOWN,
            "",
            "",
            0.0,
        )
    }

    pub fn color_only(color: RgbColor) -> Self {
        Self::land(
            color,
            Self::UNKNOWN,
            Self::UNKNOWN,
            Self::UNKNOWN,
            Self::UNKNOWN,
            Self::UNKNOWN,
            Self::UNKNOWN,
            Self::UNKNOWN,
            Self::UNKNOWN,
            Self::UNKNOWN,
            Self::UNKNOWN,
            Self::UNKNOWN,
            Self::UNKNOWN,
            "",
            "",
            0.0,
        )
    }

    pub fn with_color(&self, new_color: RgbColor) -> Self {
        Self::new(
            new_color,
            self.terrain_token_color,
            self.terrain_token_source,
            self.climate_class,
            self.evergreen_broadleaf_trees,
            self.deciduous_broadleaf_trees,
            self.needleleaf_trees,
            self.mixed_trees,
            self.herbaceous_vegetation,
            self.shrubs,
            self.snow_cover,
            self.swamp_cover,
            self.ocean_temperature,
            self.bathymetry_meters,
            self.slope_permille,
            self.ecoregion_name.clone(),
            self.ecoregion_biome_id.clone(),
            self.ecoregion_confidence,
        )
    }

    pub fn rounded(value: Option<f64>) -> i32 {
        value.map_or(Self::UNKNOWN, java_math_round_double_to_narrowed_i32)
    }

    pub fn has_climate_class(&self) -> bool {
        self.climate_class > 0
    }

    pub fn tree_cover(&self) -> f64 {
        coverage(self.evergreen_broadleaf_trees)
            .max(coverage(self.deciduous_broadleaf_trees))
            .max(coverage(self.needleleaf_trees))
            .max(coverage(self.mixed_trees))
    }

    pub fn canopy_cover(&self) -> f64 {
        (coverage(self.evergreen_broadleaf_trees)
            + coverage(self.deciduous_broadleaf_trees)
            + coverage(self.needleleaf_trees)
            + coverage(self.mixed_trees))
        .min(1.0)
    }

    pub fn herbaceous_cover(&self) -> f64 {
        coverage(self.herbaceous_vegetation)
    }

    pub fn shrub_cover(&self) -> f64 {
        coverage(self.shrubs)
    }

    pub fn vegetation_cover(&self) -> f64 {
        self.tree_cover()
            .max(self.herbaceous_cover())
            .max(self.shrub_cover())
    }

    pub fn has_vegetation_presence(&self) -> bool {
        self.vegetation_cover() > 0.0
    }

    pub fn snow_cover_ratio(&self) -> f64 {
        coverage(self.snow_cover)
    }

    pub fn swamp_cover_ratio(&self) -> f64 {
        coverage(self.swamp_cover)
    }

    pub fn slope_ratio(&self) -> f64 {
        if self.slope_permille == Self::UNKNOWN || self.slope_permille <= 0 {
            return 0.0;
        }
        if self.slope_permille >= 1000 {
            return 1.0;
        }
        f64::from(self.slope_permille) / 1000.0
    }

    pub fn has_ecoregion(&self) -> bool {
        !self.ecoregion_name.is_empty()
    }

    pub fn has_ecoregion_biome(&self) -> bool {
        !self.ecoregion_biome_id.is_empty()
    }

    pub fn has_bathymetry(&self) -> bool {
        self.bathymetry_meters != Self::UNKNOWN
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SurfaceColorMetrics {
    red: f64,
    green: f64,
    blue: f64,
    hue: f64,
    saturation: f64,
    value: f64,
}

impl SurfaceColorMetrics {
    fn from(color: RgbColor) -> Self {
        let red = f64::from(color.red) / 255.0;
        let green = f64::from(color.green) / 255.0;
        let blue = f64::from(color.blue) / 255.0;
        let max = red.max(green).max(blue);
        let min = red.min(green).min(blue);
        let delta = max - min;
        let hue = if delta == 0.0 {
            0.0
        } else if max == red {
            (60.0 * ((green - blue) / delta).rem_euclid(6.0)).rem_euclid(360.0)
        } else if max == green {
            60.0 * (((blue - red) / delta) + 2.0)
        } else {
            60.0 * (((red - green) / delta) + 4.0)
        };
        let saturation = if max == 0.0 { 0.0 } else { delta / max };
        Self {
            red,
            green,
            blue,
            hue,
            saturation,
            value: max,
        }
    }

    fn color(self) -> RgbColor {
        RgbColor::of(
            clamp_surface_material_color(java_math_round_double_to_narrowed_i32(self.red * 255.0)),
            clamp_surface_material_color(java_math_round_double_to_narrowed_i32(
                self.green * 255.0,
            )),
            clamp_surface_material_color(java_math_round_double_to_narrowed_i32(self.blue * 255.0)),
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub fn apply_surface_material(
    base: &EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    coast_factor: f64,
    local_relief_meters: f64,
    vertical_scale: f64,
) -> Result<EarthSurfaceColumn> {
    let vertical_scale = require_valid_vertical_scale(vertical_scale)?;
    let coast_factor = clamp_unit(coast_factor);
    let local_relief_meters = local_relief_meters.max(0.0);
    let color = sample.color;
    let usable_color = color.available && (!color.is_near_black() || base.water);
    let metrics = usable_color.then(|| SurfaceColorMetrics::from(color));
    let classified = if base.water {
        if let Some(metrics) = metrics {
            if should_source_land_override_water(
                base,
                sample,
                metrics,
                elevation_meters,
                longitude,
                latitude,
                coast_factor,
            ) {
                let ground_y = SEA_LEVEL_Y.max(shaped_ground_surface_y(
                    elevation_meters.max(0.0),
                    longitude,
                    latitude,
                    false,
                    coast_factor,
                    vertical_scale,
                ));
                let biome = biome_id(
                    elevation_meters.max(0.0),
                    longitude,
                    latitude,
                    false,
                    ground_y,
                    coast_factor,
                );
                if is_pale_dry_land(metrics) {
                    EarthSurfaceColumn::new(
                        false,
                        ground_y,
                        i32::MIN,
                        block_state_ids::CALCITE,
                        block_state_ids::CALCITE,
                        "minecraft:desert",
                        "source-land-override",
                    )
                } else if is_desert_sand_like(metrics) || is_dry_land_neutral(metrics) {
                    EarthSurfaceColumn::new(
                        false,
                        ground_y,
                        i32::MIN,
                        block_state_ids::SAND,
                        block_state_ids::SAND,
                        "minecraft:desert",
                        "source-land-override",
                    )
                } else {
                    let land_base = EarthSurfaceColumn::new(
                        false,
                        ground_y,
                        i32::MIN,
                        block_state_ids::SAND,
                        block_state_ids::SAND,
                        biome,
                        "source-land-override",
                    );
                    classify_surface_material_land(
                        &land_base,
                        metrics,
                        elevation_meters.max(0.0),
                        longitude,
                        latitude,
                        coast_factor,
                        sample,
                        local_relief_meters,
                    )
                }
            } else {
                classify_surface_material_water(
                    base,
                    sample,
                    Some(metrics),
                    latitude,
                    coast_factor,
                    vertical_scale,
                )
            }
        } else {
            classify_surface_material_water(
                base,
                sample,
                None,
                latitude,
                coast_factor,
                vertical_scale,
            )
        }
    } else if should_bathymetry_override_land(base, sample, elevation_meters, coast_factor) {
        let ocean_base = EarthSurfaceColumn::new(
            true,
            base.ground_surface_y,
            SEA_LEVEL_Y,
            block_state_ids::GRAVEL,
            block_state_ids::GRAVEL,
            surface_material_ocean_biome_id(&base.biome_id, sample, latitude, 1),
            "bathymetry-land-override",
        );
        classify_surface_material_water(
            &ocean_base,
            sample,
            metrics,
            latitude,
            coast_factor,
            vertical_scale,
        )
    } else if let Some(metrics) = metrics {
        classify_surface_material_land(
            base,
            metrics,
            elevation_meters,
            longitude,
            latitude,
            coast_factor,
            sample,
            local_relief_meters,
        )
    } else if let Some(metrics) =
        surface_material_semantic_fallback_metrics(sample, longitude, latitude)
    {
        classify_surface_material_land(
            base,
            metrics,
            elevation_meters,
            longitude,
            latitude,
            coast_factor,
            sample,
            local_relief_meters,
        )
    } else {
        base.with_decision_source("base")
    };
    Ok(with_surface_material_metadata(classified, sample))
}

fn classify_surface_material_water(
    base: &EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
    metrics: Option<SurfaceColorMetrics>,
    latitude: f64,
    coast_factor: f64,
    vertical_scale: f64,
) -> EarthSurfaceColumn {
    let ground_y =
        surface_material_bathymetry_ground_surface_y(base, sample, coast_factor, vertical_scale);
    let depth = 0.max(base.water_surface_y - ground_y);
    let top = surface_material_ocean_floor_block(metrics, depth, coast_factor);
    EarthSurfaceColumn::new(
        true,
        ground_y,
        base.water_surface_y,
        top,
        top,
        surface_material_ocean_biome_id(&base.biome_id, sample, latitude, depth),
        "water",
    )
}

fn surface_material_ocean_floor_block(
    metrics: Option<SurfaceColorMetrics>,
    depth: i32,
    coast_factor: f64,
) -> i32 {
    let Some(metrics) = metrics else {
        return block_state_ids::GRAVEL;
    };
    let blue_water = (165.0..=255.0).contains(&metrics.hue)
        && metrics.blue >= metrics.red * 1.10
        && metrics.green >= metrics.red * 0.80;
    let dark_open_water = is_dark_open_water(metrics);
    let open_water = coast_factor < 0.92 || depth >= 8;
    if open_water && (blue_water || dark_open_water) {
        if dark_open_water {
            return block_state_ids::DEEPSLATE;
        }
        if depth >= 30 && metrics.value <= 0.13 {
            return block_state_ids::DEEPSLATE;
        }
        if metrics.value <= 0.13 {
            return block_state_ids::BLACK_TERRACOTTA;
        }
        if depth >= 26 && metrics.value <= 0.24 {
            return block_state_ids::BLACK_TERRACOTTA;
        }
        if depth >= 16 && metrics.value <= 0.38 {
            return block_state_ids::DEEPSLATE;
        }
    }
    if depth <= 7 && blue_water && metrics.value >= 0.34 && metrics.saturation >= 0.16 {
        return block_state_ids::CLAY;
    }
    block_state_ids::GRAVEL
}

#[allow(clippy::too_many_arguments)]
fn classify_surface_material_land(
    base: &EarthSurfaceColumn,
    metrics: SurfaceColorMetrics,
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    coast_factor: f64,
    sample: &SurfaceMaterialSample,
    local_relief_meters: f64,
) -> EarthSurfaceColumn {
    let abs_lat = latitude.abs();
    let green_like = is_green_like(metrics);
    let dark_vegetation = green_like && metrics.value < 0.48;
    let lush_vegetation = green_like && metrics.saturation >= 0.20;
    let olive_dry_grass = is_olive_dry_grass(metrics);
    let desert_sand_like = is_desert_sand_like(metrics);
    let orange_rock_like = is_orange_rock_like(metrics);
    let sahara_score = surface_material_sahara_score(longitude, latitude);
    let sahel_score = surface_material_sahel_score(longitude, latitude);
    let rainforest_score = surface_material_rainforest_score(longitude, latitude);
    let dry_savanna_score = surface_material_dry_savanna_score(longitude, latitude);
    let mediterranean_score = surface_material_mediterranean_score(longitude, latitude);
    let patch_noise = surface_material_ecology_noise(longitude, latitude, 2.4, 0x4165d9e7a1f31c0b);
    let fine_noise =
        surface_material_ecology_noise(longitude, latitude, 7.5, 0x9d6c63b5a8e33f21_u64 as i64);
    let immediate_beach = is_immediate_beach(base, coast_factor);
    let beach_sand_allowed = should_use_beach_sand(metrics, longitude, latitude);
    let terrain = surface_material_met_terrain(sample, metrics);
    let semantic_terrain =
        sample.terrain_token_source == TerrainTokenSource::Export && terrain.confident();
    let warm_bright_dry_land = abs_lat <= 42.0
        && metrics.value >= 0.58
        && (28.0..=72.0).contains(&metrics.hue)
        && metrics.red >= metrics.blue * 1.01
        && metrics.green >= metrics.blue * 1.01;

    if immediate_beach && beach_sand_allowed {
        return mark_surface_material(
            surface_material_with_surface(
                base,
                block_state_ids::SAND,
                block_state_ids::SAND,
                "minecraft:beach",
            ),
            "shoreline",
        );
    }

    let snow_climate =
        abs_lat >= 58.0 || base.ground_surface_y >= 165 || elevation_meters >= 2_800.0;
    let snow_like = snow_climate
        && metrics.value >= 0.70
        && metrics.saturation <= 0.24
        && metrics.blue >= metrics.red * 0.90
        && !warm_bright_dry_land;
    if snow_like || abs_lat >= 68.0 || (base.ground_surface_y >= 170 && abs_lat >= 25.0) {
        return mark_surface_material(
            surface_material_with_surface(
                base,
                block_state_ids::SNOW_BLOCK,
                block_state_ids::DIRT,
                "minecraft:snowy_plains",
            ),
            "snow",
        );
    }

    if let Some(intent_column) = classify_surface_material_by_semantic_intent(
        base,
        sample,
        metrics,
        elevation_meters,
        longitude,
        latitude,
        sahara_score,
        sahel_score,
        rainforest_score,
        dry_savanna_score,
        mediterranean_score,
        patch_noise,
        fine_noise,
        local_relief_meters,
        terrain,
        semantic_terrain,
    ) {
        let vegetation_evidence = has_vegetation_evidence(sample, metrics);
        let decision_source =
            if is_dry_core_ecoregion_evidence(sample, metrics, vegetation_evidence) {
                "intent-ecoregion"
            } else {
                "intent"
            };
        return mark_surface_material(intent_column, decision_source);
    }

    let java_standard_terrain = sample.terrain_token_source
        == TerrainTokenSource::JavaStandardPalette
        && terrain.confident();
    let token_vegetated = is_trusted_vegetated_token(
        sample,
        metrics,
        terrain,
        semantic_terrain,
        java_standard_terrain,
    );
    let sparse_dry_open_tropical = is_sparse_dry_open_tropical(
        sample,
        metrics,
        sahel_score,
        dry_savanna_score,
        token_vegetated,
    );
    let sparse_dry_open_has_dry_intent =
        is_sahel_latitude(latitude) || sahel_score >= 0.22 || dry_savanna_score >= 0.35;
    if is_tropical_rain_climate(sample.climate_class)
        && sparse_dry_open_tropical
        && !sparse_dry_open_has_dry_intent
        && !is_named_forest_savanna_mosaic(sample)
    {
        let biome = if sample.tree_cover() >= 0.15 || rainforest_score >= 0.45 || dark_vegetation {
            rainforest_biome_with_sample(
                sample,
                rainforest_score.max(0.65),
                dark_vegetation,
                patch_noise,
                fine_noise,
            )
        } else {
            "minecraft:sparse_jungle".to_string()
        };
        return mark_surface_material(
            surface_material_with_surface(
                base,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                biome,
            ),
            "environment",
        );
    }

    if !sample.has_climate_class()
        && (sample.herbaceous_cover() >= 0.30 || sample.shrub_cover() >= 0.30)
        && orange_rock_like
        && elevation_meters < 1_500.0
    {
        let biome = dry_grass_biome(
            dry_savanna_score.max(sahel_score).max(0.45),
            elevation_meters,
            patch_noise,
            fine_noise,
        );
        let top = dry_grass_surface(metrics, patch_noise, fine_noise, 0.44);
        return mark_surface_material(
            surface_material_with_surface(base, top, block_state_ids::DIRT, biome),
            "environment",
        );
    }

    if !is_sahel_latitude(latitude)
        && terrain.confident()
        && terrain.kind == MetTerrainKind::Vegetated
        && desert_sand_like
        && warm_bright_dry_land
        && (sahara_score >= 0.18 || desert_score(longitude, latitude) >= 0.50)
    {
        return surface_material_with_surface(
            base,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:desert",
        );
    }

    if let Some(ecoregion_column) = classify_surface_material_by_ecoregion(
        base,
        sample,
        metrics,
        elevation_meters,
        longitude,
        latitude,
        sahara_score,
        sahel_score,
        rainforest_score,
        dry_savanna_score,
        patch_noise,
        fine_noise,
        local_relief_meters,
        sample.slope_ratio(),
        coast_factor,
        terrain,
        semantic_terrain,
    ) {
        return mark_surface_material(ecoregion_column, "ecoregion");
    }

    if is_sahel_latitude(latitude)
        && elevation_meters < 1_600.0
        && metrics.value < 0.74
        && (green_like
            || olive_dry_grass
            || orange_rock_like
            || (desert_sand_like && metrics.value < 0.66))
    {
        let biome = dry_grass_biome(
            sahel_score.max(0.70),
            elevation_meters,
            patch_noise,
            fine_noise,
        );
        let top = dry_grass_surface(metrics, patch_noise, fine_noise, 0.48);
        return surface_material_with_surface(base, top, block_state_ids::DIRT, biome);
    }

    if orange_rock_like
        && is_exposed_dry_rock(
            metrics,
            elevation_meters,
            longitude,
            latitude,
            sahara_score,
            dry_savanna_score,
            local_relief_meters,
            sample.slope_ratio(),
        )
    {
        let top = if metrics.hue <= 35.0 || metrics.red > metrics.green * 1.18 {
            block_state_ids::ORANGE_TERRACOTTA
        } else {
            block_state_ids::TERRACOTTA
        };
        let biome = if elevation_meters >= 1_200.0 || fine_noise >= 0.68 {
            "minecraft:wooded_badlands"
        } else {
            "minecraft:badlands"
        };
        return surface_material_with_surface(base, top, top, biome);
    }

    if rainforest_score >= 0.35 && (green_like || metrics.value < 0.44) {
        let biome = rainforest_biome(rainforest_score, dark_vegetation, patch_noise, fine_noise);
        return surface_material_with_surface(
            base,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            biome,
        );
    }

    if sahel_score >= 0.24 && (green_like || olive_dry_grass || metrics.value < 0.75) {
        let biome = dry_grass_biome(sahel_score, elevation_meters, patch_noise, fine_noise);
        let top = dry_grass_surface(
            metrics,
            patch_noise,
            fine_noise,
            0.42 + (sahel_score * 0.18),
        );
        return surface_material_with_surface(base, top, block_state_ids::DIRT, biome);
    }

    if is_sahel_latitude(latitude)
        && metrics.value < 0.72
        && (olive_dry_grass || desert_sand_like || orange_rock_like)
    {
        let biome = dry_grass_biome(
            sahel_score.max(0.70),
            elevation_meters,
            patch_noise,
            fine_noise,
        );
        let top = dry_grass_surface(metrics, patch_noise, fine_noise, 0.48);
        return surface_material_with_surface(base, top, block_state_ids::DIRT, biome);
    }

    if sahara_score >= 0.45 && (desert_sand_like || !green_like || warm_bright_dry_land) {
        if orange_rock_like
            && metrics.value < 0.42
            && elevation_meters >= 450.0
            && local_relief_meters >= 160.0
            && fine_noise >= 0.60
        {
            let top = if metrics.hue <= 35.0 {
                block_state_ids::ORANGE_TERRACOTTA
            } else {
                block_state_ids::TERRACOTTA
            };
            return surface_material_with_surface(base, top, top, "minecraft:badlands");
        }
        let top = desert_surface(
            metrics,
            patch_noise,
            fine_noise,
            sahara_score,
            terrain,
            semantic_terrain,
        );
        return surface_material_with_surface(
            base,
            top,
            desert_filler(top),
            desert_biome(elevation_meters, patch_noise, fine_noise),
        );
    }

    if dry_savanna_score >= 0.35
        && (green_like || olive_dry_grass || desert_sand_like || orange_rock_like)
    {
        let biome = dry_grass_biome(dry_savanna_score, elevation_meters, patch_noise, fine_noise);
        let top = dry_grass_surface(
            metrics,
            patch_noise,
            fine_noise,
            0.38 + (dry_savanna_score * 0.18),
        );
        return surface_material_with_surface(base, top, block_state_ids::DIRT, biome);
    }

    if mediterranean_score >= 0.40
        && (green_like || olive_dry_grass || orange_rock_like || desert_sand_like)
    {
        let biome = if patch_noise >= 0.62 || dark_vegetation {
            "minecraft:forest"
        } else {
            "minecraft:plains"
        };
        let top = if fine_noise >= 0.76 && !green_like {
            block_state_ids::COARSE_DIRT
        } else {
            block_state_ids::GRASS_BLOCK
        };
        return surface_material_with_surface(base, top, block_state_ids::DIRT, biome);
    }

    if lush_vegetation {
        return surface_material_with_surface(
            base,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            lush_biome(abs_lat, dark_vegetation, patch_noise),
        );
    }

    if !immediate_beach && desert_sand_like {
        if abs_lat <= 28.0
            && metrics.value < 0.66
            && (olive_dry_grass || dry_savanna_score >= 0.20 || sahel_score >= 0.18)
        {
            let biome = dry_grass_biome(
                dry_savanna_score.max(sahel_score).max(0.45),
                elevation_meters,
                patch_noise,
                fine_noise,
            );
            let top = dry_grass_surface(metrics, patch_noise, fine_noise, 0.48);
            return surface_material_with_surface(base, top, block_state_ids::DIRT, biome);
        }
        let sand = if metrics.red > metrics.green * 1.18 && metrics.hue <= 45.0 {
            block_state_ids::RED_SAND
        } else {
            block_state_ids::SAND
        };
        return surface_material_with_surface(base, sand, sand, "minecraft:desert");
    }

    if base.biome_id == "minecraft:desert"
        && abs_lat <= 42.0
        && metrics.value >= 0.50
        && metrics.saturation <= 0.20
    {
        return surface_material_with_surface(
            base,
            block_state_ids::SAND,
            block_state_ids::SAND,
            "minecraft:desert",
        );
    }

    if orange_rock_like {
        if dry_savanna_score >= 0.20 || (abs_lat <= 35.0 && metrics.value >= 0.45) {
            let biome = dry_grass_biome(
                dry_savanna_score.max(0.40),
                elevation_meters,
                patch_noise,
                fine_noise,
            );
            let top = dry_grass_surface(metrics, patch_noise, fine_noise, 0.46);
            return surface_material_with_surface(base, top, block_state_ids::DIRT, biome);
        }
        let top = if metrics.value < 0.36 {
            block_state_ids::BROWN_TERRACOTTA
        } else {
            block_state_ids::ORANGE_TERRACOTTA
        };
        return surface_material_with_surface(base, top, top, "minecraft:badlands");
    }

    let rock_like =
        metrics.saturation <= 0.16 || base.ground_surface_y >= 150 || elevation_meters >= 3_000.0;
    if rock_like {
        let top = if metrics.value < 0.35 {
            block_state_ids::GRAVEL
        } else {
            block_state_ids::STONE
        };
        let biome = if base.ground_surface_y >= 150 {
            "minecraft:snowy_plains".to_string()
        } else {
            non_beach_biome(base, latitude)
        };
        return surface_material_with_surface(base, top, block_state_ids::STONE, biome);
    }

    if olive_dry_grass || ((45.0..=95.0).contains(&metrics.hue) && metrics.saturation < 0.28) {
        let biome = dry_grass_biome(
            dry_savanna_score.max(0.45),
            elevation_meters,
            patch_noise,
            fine_noise,
        );
        let top = dry_grass_surface(metrics, patch_noise, fine_noise, 0.42);
        return surface_material_with_surface(base, top, block_state_ids::DIRT, biome);
    }

    surface_material_with_surface(
        base,
        block_state_ids::GRASS_BLOCK,
        block_state_ids::DIRT,
        fallback_biome(base, latitude, patch_noise),
    )
}

#[allow(clippy::too_many_arguments)]
fn classify_surface_material_by_semantic_intent(
    base: &EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    sahara_score: f64,
    sahel_score: f64,
    rainforest_score: f64,
    dry_savanna_score: f64,
    mediterranean_score: f64,
    patch_noise: f64,
    fine_noise: f64,
    local_relief_meters: f64,
    terrain: MetTerrainMatch,
    semantic_terrain: bool,
) -> Option<EarthSurfaceColumn> {
    let climate = sample.climate_class;
    let has_climate = sample.has_climate_class();
    let tree_cover = sample.tree_cover();
    let herb_cover = sample.herbaceous_cover();
    let shrub_cover = sample.shrub_cover();
    let green_like = is_green_like(metrics);
    let dark_vegetation = green_like && metrics.value < 0.48;
    let olive_dry_grass = is_olive_dry_grass(metrics);
    let desert_sand_like = is_desert_sand_like(metrics);
    let orange_rock_like = is_orange_rock_like(metrics);
    let forest_like = is_forest_like_ecoregion(sample);
    let savanna_like = is_savanna_like_ecoregion(sample);
    let vegetation_evidence = has_vegetation_evidence(sample, metrics);
    let ecoregion_dry_core = is_dry_core_ecoregion_evidence(sample, metrics, vegetation_evidence);
    let dry_tropical_woodland =
        latitude.abs() <= 25.0 && dry_savanna_score >= 0.20 && !is_tropical_rain_climate(climate);
    let java_standard_terrain = sample.terrain_token_source
        == TerrainTokenSource::JavaStandardPalette
        && terrain.confident();
    let token_vegetated = is_trusted_vegetated_token(
        sample,
        metrics,
        terrain,
        semantic_terrain,
        java_standard_terrain,
    );
    let token_dry_surface = terrain.confident()
        && matches!(
            terrain.kind,
            MetTerrainKind::Sand
                | MetTerrainKind::RedSand
                | MetTerrainKind::CoarseDirt
                | MetTerrainKind::Gravel
                | MetTerrainKind::Rock
        );

    if sample.snow_cover_ratio() >= 0.35 || climate == 29 || climate == 30 {
        return Some(surface_material_with_surface(
            base,
            block_state_ids::SNOW_BLOCK,
            block_state_ids::DIRT,
            if latitude.abs() >= 58.0 {
                "minecraft:snowy_taiga"
            } else {
                "minecraft:snowy_plains"
            },
        ));
    }

    if sample.swamp_cover_ratio() >= 0.35 && latitude.abs() <= 45.0 {
        let top = if fine_noise >= 0.82 {
            block_state_ids::MUD
        } else {
            block_state_ids::GRASS_BLOCK
        };
        return Some(surface_material_with_surface(
            base,
            top,
            if top == block_state_ids::MUD {
                block_state_ids::MUD
            } else {
                block_state_ids::DIRT
            },
            "minecraft:swamp",
        ));
    }

    if is_forest_savanna_mosaic_intent(
        sample,
        metrics,
        longitude,
        latitude,
        sahara_score,
        rainforest_score,
        dry_savanna_score,
        token_vegetated,
        patch_noise,
        fine_noise,
    ) {
        let mut canopy_weight = clamp_unit(
            0.10 + (sample.canopy_cover() * 0.48)
                + (sample.vegetation_cover() * 0.12)
                + (rainforest_score * 0.22)
                - (sahel_score * 0.38)
                - (dry_savanna_score * 0.34),
        );
        if is_humid_forest_core(sample, rainforest_score, sahel_score, dry_savanna_score) {
            canopy_weight = canopy_weight.max(0.72);
        } else if is_named_forest_savanna_mosaic(sample) {
            canopy_weight = canopy_weight.max(clamp_unit(
                0.52 + (rainforest_score * 0.08)
                    - (sahel_score * 0.12)
                    - (dry_savanna_score * 0.12),
            ));
        }
        let local = (patch_noise * 0.68) + (fine_noise * 0.32);
        let lush_patch = local < canopy_weight
            || (sample.canopy_cover() >= 0.62 && sample.tree_cover() >= 0.28 && fine_noise < 0.80);
        if lush_patch {
            let humid_score = rainforest_score.max(0.48);
            return Some(surface_material_with_surface(
                base,
                lush_vegetation_surface(
                    sample,
                    metrics,
                    patch_noise,
                    fine_noise,
                    humid_score,
                    terrain,
                ),
                block_state_ids::DIRT,
                rainforest_biome_with_sample(
                    sample,
                    humid_score,
                    metrics.value < 0.48,
                    patch_noise,
                    fine_noise,
                ),
            ));
        }
        let dry_score = sahel_score.max(dry_savanna_score).max(0.50);
        let top = dry_grass_surface_conservative(
            metrics,
            sample,
            patch_noise,
            fine_noise,
            dry_score,
            terrain,
            semantic_terrain,
            local_relief_meters,
        );
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            dry_grass_biome(dry_score, elevation_meters, patch_noise, fine_noise),
        ));
    }

    let sparse_dry_open_tropical = is_sparse_dry_open_tropical(
        sample,
        metrics,
        sahel_score,
        dry_savanna_score,
        token_vegetated,
    );
    let rainforest_intent =
        (has_climate && is_tropical_rain_climate(climate) && !sparse_dry_open_tropical)
            || (rainforest_score >= 0.35 && tree_cover >= 0.12)
            || (rainforest_score >= 0.45
                && (sample.vegetation_cover() >= 0.18
                    || tree_cover >= 0.08
                    || green_like
                    || metrics.value < 0.44));
    if rainforest_intent {
        return Some(surface_material_with_surface(
            base,
            lush_vegetation_surface(
                sample,
                metrics,
                patch_noise,
                fine_noise,
                rainforest_score,
                terrain,
            ),
            block_state_ids::DIRT,
            rainforest_biome_with_sample(
                sample,
                rainforest_score.max(0.60),
                metrics.value < 0.48,
                patch_noise,
                fine_noise,
            ),
        ));
    }

    if (is_sahel_latitude(latitude) || sahel_score >= 0.22)
        && (vegetation_evidence
            || olive_dry_grass
            || dry_savanna_score >= 0.25
            || token_vegetated
            || (desert_sand_like && metrics.value < 0.72)
            || (orange_rock_like && metrics.value < 0.74))
    {
        let desert_edge = is_desert_climate(climate) || sahara_score >= 0.35;
        let dry_score =
            sahel_score
                .max(dry_savanna_score)
                .max(if desert_edge { 0.58 } else { 0.45 });
        let top = dry_grass_surface_conservative(
            metrics,
            sample,
            patch_noise,
            fine_noise,
            dry_score,
            terrain,
            semantic_terrain,
            local_relief_meters,
        );
        let biome = if desert_edge && dry_score >= 0.58 {
            "minecraft:savanna".to_string()
        } else {
            dry_grass_biome(dry_score, elevation_meters, patch_noise, fine_noise)
        };
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            biome,
        ));
    }

    let strong_dry_core = is_dry_core_ecoregion_evidence(sample, metrics, vegetation_evidence)
        || (is_desert_climate(climate) && !vegetation_evidence && sahara_score >= 0.35);
    if mediterranean_score >= 0.40 && !strong_dry_core {
        return Some(surface_material_with_surface(
            base,
            mediterranean_surface(sample, metrics, fine_noise),
            block_state_ids::DIRT,
            mediterranean_biome(sample, metrics, patch_noise),
        ));
    }

    if sparse_dry_open_tropical
        && (is_sahel_latitude(latitude) || sahel_score >= 0.22 || dry_savanna_score >= 0.35)
    {
        let dry_score = sahel_score.max(dry_savanna_score).max(0.45);
        let biome = dry_grass_biome(dry_score, elevation_meters, patch_noise, fine_noise);
        let top = dry_grass_surface_conservative(
            metrics,
            sample,
            patch_noise,
            fine_noise,
            dry_score,
            terrain,
            semantic_terrain,
            local_relief_meters,
        );
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            biome,
        ));
    }

    if is_tropical_savanna_climate(climate) {
        let dry_score = sahel_score.max(dry_savanna_score).max(0.45);
        let biome = dry_grass_biome(dry_score, elevation_meters, patch_noise, fine_noise);
        let top = dry_grass_surface_conservative(
            metrics,
            sample,
            patch_noise,
            fine_noise,
            dry_score,
            terrain,
            semantic_terrain,
            local_relief_meters,
        );
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            biome,
        ));
    }

    if is_steppe_climate(climate) {
        if dry_savanna_score >= 0.35
            || sahel_score >= 0.18
            || (latitude.abs() <= 32.0 && dry_savanna_score >= 0.18)
        {
            let dry_score = sahel_score.max(dry_savanna_score).max(0.45);
            let biome = dry_grass_biome(dry_score, elevation_meters, patch_noise, fine_noise);
            let top = dry_grass_surface_conservative(
                metrics,
                sample,
                patch_noise,
                fine_noise,
                dry_score,
                terrain,
                semantic_terrain,
                local_relief_meters,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                biome,
            ));
        }
        let biome = temperate_grassland_biome(latitude, sample, metrics, patch_noise, fine_noise);
        return Some(surface_material_with_surface(
            base,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            biome,
        ));
    }

    if dry_savanna_score >= 0.35
        && !is_dry_core_ecoregion_evidence(sample, metrics, vegetation_evidence)
    {
        let dry_score = sahel_score.max(dry_savanna_score).max(0.45);
        let top = dry_grass_surface_conservative(
            metrics,
            sample,
            patch_noise,
            fine_noise,
            dry_score,
            terrain,
            semantic_terrain,
            local_relief_meters,
        );
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            dry_grass_biome(dry_score, elevation_meters, patch_noise, fine_noise),
        ));
    }

    if savanna_like
        && sample.ecoregion_confidence >= 0.78
        && (is_sahel_latitude(latitude) || sahel_score >= 0.22)
        && is_desert_climate(climate)
        && !is_dry_core_ecoregion_evidence(sample, metrics, vegetation_evidence)
        && !is_tropical_rain_climate(climate)
    {
        if desert_sand_like && !vegetation_evidence && !token_vegetated && metrics.value >= 0.78 {
            let sand_patch_noise =
                surface_material_ecology_noise(longitude, latitude, 0.46, 0x519bc3a22e8f4d31);
            let sand_patch_threshold = clamp_unit(
                0.04 + (sahara_score * 0.10) + ((metrics.value - 0.78) * 0.28)
                    - (sahel_score * 0.18)
                    - (dry_savanna_score * 0.10),
            );
            if sand_patch_noise < sand_patch_threshold {
                return Some(hot_desert_surface_column(
                    base,
                    sample,
                    metrics,
                    elevation_meters,
                    longitude,
                    latitude,
                    sahara_score,
                    dry_savanna_score,
                    patch_noise,
                    fine_noise,
                    local_relief_meters,
                    terrain,
                    semantic_terrain,
                ));
            }
        }
        let dry_score = sahel_score.max(dry_savanna_score).max(0.58);
        let top = dry_grass_surface_conservative(
            metrics,
            sample,
            patch_noise,
            fine_noise,
            dry_score,
            terrain,
            semantic_terrain,
            local_relief_meters,
        );
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            "minecraft:savanna",
        ));
    }

    if savanna_like
        && sample.ecoregion_confidence >= 0.55
        && !is_dry_core_ecoregion_evidence(sample, metrics, vegetation_evidence)
        && !is_tropical_rain_climate(climate)
    {
        if (is_desert_climate(climate) || is_steppe_climate(climate))
            && desert_sand_like
            && !vegetation_evidence
            && !token_vegetated
            && metrics.value >= 0.70
        {
            let sand_patch_noise =
                surface_material_ecology_noise(longitude, latitude, 0.42, 0x1b8d4a732e66c1d7);
            let sand_patch_threshold = clamp_unit(
                0.12 + (sahara_score * 0.24) + ((metrics.value - 0.70) * 0.46)
                    - (sahel_score * 0.10)
                    - (dry_savanna_score * 0.08),
            );
            if sand_patch_noise < sand_patch_threshold {
                return Some(hot_desert_surface_column(
                    base,
                    sample,
                    metrics,
                    elevation_meters,
                    longitude,
                    latitude,
                    sahara_score,
                    dry_savanna_score,
                    patch_noise,
                    fine_noise,
                    local_relief_meters,
                    terrain,
                    semantic_terrain,
                ));
            }
        }
        if dry_savanna_score >= 0.25
            || sahel_score >= 0.18
            || is_tropical_savanna_climate(climate)
            || latitude.abs() <= 32.0
        {
            let dry_score = sahel_score.max(dry_savanna_score).max(0.45);
            let top = dry_grass_surface_conservative(
                metrics,
                sample,
                patch_noise,
                fine_noise,
                dry_score,
                terrain,
                semantic_terrain,
                local_relief_meters,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                dry_grass_biome(dry_score, elevation_meters, patch_noise, fine_noise),
            ));
        }
        return Some(surface_material_with_surface(
            base,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            temperate_grassland_biome(latitude, sample, metrics, patch_noise, fine_noise),
        ));
    }

    if java_standard_terrain
        && token_vegetated
        && !green_like
        && !is_tropical_rain_climate(climate)
        && latitude.abs() <= 35.0
        && (sahel_score >= 0.18 || dry_savanna_score >= 0.18 || mediterranean_score >= 0.32)
        && metrics.value < 0.76
    {
        let desert_edge = sahara_score >= 0.35 || is_desert_climate(climate);
        let dry_score =
            sahel_score
                .max(dry_savanna_score)
                .max(if desert_edge { 0.58 } else { 0.45 });
        let top = dry_grass_surface_conservative(
            metrics,
            sample,
            patch_noise,
            fine_noise,
            dry_score,
            terrain,
            semantic_terrain,
            local_relief_meters,
        );
        let biome = if desert_edge && dry_score >= 0.58 {
            "minecraft:savanna".to_string()
        } else {
            dry_grass_biome(dry_score, elevation_meters, patch_noise, fine_noise)
        };
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            biome,
        ));
    }

    if orange_rock_like
        && is_exposed_dry_rock(
            metrics,
            elevation_meters,
            longitude,
            latitude,
            sahara_score,
            dry_savanna_score,
            local_relief_meters,
            sample.slope_ratio(),
        )
        && (elevation_meters >= 700.0 || local_relief_meters >= 240.0)
    {
        return Some(highland_rock_surface(
            base,
            metrics,
            elevation_meters,
            patch_noise,
            fine_noise,
        ));
    }

    if token_dry_surface
        && java_standard_terrain
        && !vegetation_evidence
        && !token_vegetated
        && (sahara_score >= 0.35
            || is_dry_core_ecoregion_evidence(sample, metrics, vegetation_evidence))
        && matches!(terrain.kind, MetTerrainKind::Gravel | MetTerrainKind::Rock)
        && (elevation_meters >= 700.0 || local_relief_meters >= 200.0)
    {
        return Some(highland_rock_surface(
            base,
            metrics,
            elevation_meters,
            patch_noise,
            fine_noise,
        ));
    }

    if ecoregion_dry_core
        && dry_savanna_score >= 0.32
        && sample.ecoregion_confidence < 0.92
        && latitude.abs() <= 42.0
        && metrics.value < 0.78
    {
        let transition_noise =
            surface_material_ecology_noise(longitude, latitude, 1.25, 0x4bd1a7240f78c8d3);
        let confidence_blend = (0.92 - sample.ecoregion_confidence) / 0.92;
        let dry_savanna_blend =
            clamp_unit(0.22 + (dry_savanna_score * 0.28) + (confidence_blend * 0.35));
        if transition_noise < dry_savanna_blend {
            let dry_score = sahel_score.max(dry_savanna_score).max(0.58);
            let top = dry_grass_surface_conservative(
                metrics,
                sample,
                patch_noise,
                fine_noise,
                dry_score,
                terrain,
                semantic_terrain,
                local_relief_meters,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ));
        }
    }

    if ecoregion_dry_core
        || (has_climate && is_desert_climate(climate))
        || sahara_score >= 0.45
        || (desert_sand_like && latitude.abs() <= 38.0 && !green_like && sahara_score >= 0.25)
    {
        if (vegetation_evidence || (java_standard_terrain && token_vegetated))
            && (sahel_score >= 0.18 || dry_savanna_score >= 0.18 || !desert_sand_like)
        {
            let dry_score = sahel_score.max(dry_savanna_score).max(0.58);
            let top = dry_grass_surface_conservative(
                metrics,
                sample,
                patch_noise,
                fine_noise,
                dry_score,
                terrain,
                semantic_terrain,
                local_relief_meters,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ));
        }
        return Some(hot_desert_surface_column(
            base,
            sample,
            metrics,
            elevation_meters,
            longitude,
            latitude,
            sahara_score,
            dry_savanna_score,
            patch_noise,
            fine_noise,
            local_relief_meters,
            terrain,
            semantic_terrain,
        ));
    }

    if forest_like
        && sample.ecoregion_confidence >= 0.45
        && !is_desert_climate(climate)
        && !ecoregion_dry_core
    {
        return Some(surface_material_with_surface(
            base,
            temperate_forest_surface(sample, metrics, patch_noise, fine_noise),
            block_state_ids::DIRT,
            temperate_biome(latitude, sample, metrics, patch_noise, fine_noise),
        ));
    }

    if (has_climate && (is_temperate_climate(climate) || is_cold_climate(climate)))
        || tree_cover >= 0.12
        || green_like
    {
        return Some(surface_material_with_surface(
            base,
            temperate_forest_surface(sample, metrics, patch_noise, fine_noise),
            block_state_ids::DIRT,
            temperate_biome(latitude, sample, metrics, patch_noise, fine_noise),
        ));
    }

    if is_desert_climate(climate) {
        if (tree_cover >= 0.12 || herb_cover >= 0.35 || shrub_cover >= 0.35 || green_like)
            && (savanna_like || is_sahel_latitude(latitude) || sahel_score >= 0.18)
        {
            let dry_score = sahel_score.max(dry_savanna_score).max(0.58);
            let top = dry_grass_surface_conservative(
                metrics,
                sample,
                patch_noise,
                fine_noise,
                dry_score,
                terrain,
                semantic_terrain,
                local_relief_meters,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ));
        }
        if orange_rock_like
            && is_exposed_dry_rock(
                metrics,
                elevation_meters,
                longitude,
                latitude,
                sahara_score,
                dry_savanna_score,
                local_relief_meters,
                sample.slope_ratio(),
            )
            && fine_noise >= 0.70
        {
            let top = if metrics.hue <= 35.0 {
                block_state_ids::ORANGE_TERRACOTTA
            } else {
                block_state_ids::TERRACOTTA
            };
            return Some(surface_material_with_surface(
                base,
                top,
                top,
                "minecraft:badlands",
            ));
        }
        let top = desert_surface(
            metrics,
            patch_noise,
            fine_noise,
            sahara_score.max(0.65),
            terrain,
            semantic_terrain,
        );
        return Some(surface_material_with_surface(
            base,
            top,
            desert_filler(top),
            desert_biome(elevation_meters, patch_noise, fine_noise),
        ));
    }

    if is_temperate_climate(climate) {
        if dry_tropical_woodland {
            let dry_score = dry_savanna_score.max(0.45);
            let biome = dry_grass_biome(dry_score, elevation_meters, patch_noise, fine_noise);
            let top = dry_grass_surface_conservative(
                metrics,
                sample,
                patch_noise,
                fine_noise,
                dry_score,
                terrain,
                semantic_terrain,
                local_relief_meters,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                biome,
            ));
        }
        if tree_cover >= 0.06 || dark_vegetation || forest_like {
            return Some(surface_material_with_surface(
                base,
                temperate_forest_surface(sample, metrics, patch_noise, fine_noise),
                block_state_ids::DIRT,
                forest_biome_for_environment(
                    latitude,
                    climate,
                    tree_cover,
                    dark_vegetation,
                    patch_noise,
                ),
            ));
        }
        if herb_cover >= 0.18 || shrub_cover >= 0.18 || green_like || metrics.value >= 0.42 {
            let top = if fine_noise >= 0.86 && shrub_cover > herb_cover {
                block_state_ids::COARSE_DIRT
            } else {
                block_state_ids::GRASS_BLOCK
            };
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                "minecraft:plains",
            ));
        }
    }

    if is_cold_climate(climate) {
        if elevation_meters >= 2_400.0 || base.ground_surface_y >= 165 {
            return Some(surface_material_with_surface(
                base,
                block_state_ids::SNOW_BLOCK,
                block_state_ids::DIRT,
                "minecraft:snowy_plains",
            ));
        }
        let biome = if tree_cover >= 0.12 || patch_noise >= 0.48 {
            "minecraft:taiga"
        } else {
            "minecraft:plains"
        };
        return Some(surface_material_with_surface(
            base,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            biome,
        ));
    }

    if !has_climate {
        if forest_like && (sample.vegetation_cover() >= 0.02 || tree_cover >= 0.06) {
            return Some(surface_material_with_surface(
                base,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                forest_biome_for_environment(
                    latitude,
                    climate,
                    tree_cover,
                    dark_vegetation,
                    patch_noise,
                ),
            ));
        }
        if savanna_like && sample.vegetation_cover() > 0.0 {
            let dry_score = sahel_score.max(dry_savanna_score).max(0.45);
            let biome = dry_grass_biome(dry_score, elevation_meters, patch_noise, fine_noise);
            let top = dry_grass_surface_conservative(
                metrics,
                sample,
                patch_noise,
                fine_noise,
                dry_score,
                terrain,
                semantic_terrain,
                local_relief_meters,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                biome,
            ));
        }
    }

    None
}

#[allow(clippy::too_many_arguments)]
fn classify_surface_material_by_ecoregion(
    base: &EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    sahara_score: f64,
    sahel_score: f64,
    rainforest_score: f64,
    dry_savanna_score: f64,
    patch_noise: f64,
    fine_noise: f64,
    local_relief_meters: f64,
    slope: f64,
    coast_factor: f64,
    terrain: MetTerrainMatch,
    semantic_terrain: bool,
) -> Option<EarthSurfaceColumn> {
    if !sample.has_ecoregion_biome() {
        return None;
    }

    let biome = sample.ecoregion_biome_id.as_str();
    let key_source = biome.strip_prefix("minecraft:").unwrap_or(biome);
    let key = key_source.to_ascii_lowercase();
    let green_like = is_green_like(metrics);
    let olive_dry_grass = is_olive_dry_grass(metrics);
    let desert_sand_like = is_desert_sand_like(metrics);
    let vegetation_evidence = has_vegetation_evidence(sample, metrics);
    let vegetation_strength = sample.vegetation_cover();
    let eco_name = sample.ecoregion_name.to_ascii_lowercase();

    if should_defer_ecoregion_at_transition(
        sample,
        &key,
        &eco_name,
        metrics,
        elevation_meters,
        latitude,
        sahara_score,
        rainforest_score,
        dry_savanna_score,
        patch_noise,
        fine_noise,
        vegetation_evidence,
    ) {
        return None;
    }

    if key.contains("beach") {
        if is_immediate_beach(base, coast_factor)
            && should_use_beach_sand(metrics, longitude, latitude)
        {
            return Some(surface_material_with_surface(
                base,
                block_state_ids::SAND,
                block_state_ids::SAND,
                biome,
            ));
        }
        return None;
    }
    if key.contains("snow") || key.contains("frozen") || key.contains("grove") {
        let top = if base.ground_surface_y >= 145 || sample.snow_cover_ratio() >= 0.18 {
            block_state_ids::SNOW_BLOCK
        } else {
            block_state_ids::GRASS_BLOCK
        };
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            biome,
        ));
    }
    if key.contains("peak")
        || key.contains("stony")
        || key.contains("jagged")
        || key.contains("gravelly")
    {
        let top = if base.ground_surface_y >= 150 || local_relief_meters >= 180.0 {
            block_state_ids::STONE
        } else {
            block_state_ids::GRASS_BLOCK
        };
        let filler = if top == block_state_ids::STONE {
            block_state_ids::STONE
        } else {
            block_state_ids::DIRT
        };
        return Some(surface_material_with_surface(base, top, filler, biome));
    }
    if key.contains("swamp") || sample.swamp_cover_ratio() >= 0.35 {
        if sample.swamp_cover_ratio() < 0.25
            && !is_immediate_beach(base, coast_factor)
            && base.ground_surface_y > SEA_LEVEL_Y + 3
        {
            return None;
        }
        let top = if fine_noise >= 0.72 || sample.swamp_cover_ratio() >= 0.55 {
            block_state_ids::MUD
        } else {
            block_state_ids::GRASS_BLOCK
        };
        let filler = if top == block_state_ids::MUD {
            block_state_ids::MUD
        } else {
            block_state_ids::DIRT
        };
        return Some(surface_material_with_surface(
            base,
            top,
            filler,
            if key.contains("mangrove") {
                "minecraft:mangrove_swamp"
            } else {
                "minecraft:swamp"
            },
        ));
    }
    if key.contains("jungle") {
        if !vegetation_evidence
            && rainforest_score < 0.25
            && !is_tropical_rain_climate(sample.climate_class)
        {
            return None;
        }
        let mosaic = eco_name.contains("mosaic") || eco_name.contains("savanna");
        if mosaic || rainforest_score < 0.42 || (vegetation_strength < 0.12 && !green_like) {
            if (olive_dry_grass || dry_savanna_score >= 0.25) && patch_noise < 0.46 {
                let top = dry_grass_surface_conservative(
                    metrics,
                    sample,
                    patch_noise,
                    fine_noise,
                    dry_savanna_score.max(0.55),
                    terrain,
                    semantic_terrain,
                    local_relief_meters,
                );
                return Some(surface_material_with_surface(
                    base,
                    top,
                    block_state_ids::DIRT,
                    "minecraft:savanna",
                ));
            }
            let top = lush_vegetation_surface(
                sample,
                metrics,
                patch_noise,
                fine_noise,
                rainforest_score.max(0.45),
                terrain,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                if patch_noise >= 0.58 {
                    "minecraft:jungle"
                } else {
                    "minecraft:sparse_jungle"
                },
            ));
        }
        if rainforest_score < 0.58 || vegetation_strength < 0.16 || !green_like {
            if olive_dry_grass && patch_noise < 0.34 {
                let top = dry_grass_surface_conservative(
                    metrics,
                    sample,
                    patch_noise,
                    fine_noise,
                    dry_savanna_score.max(0.48),
                    terrain,
                    semantic_terrain,
                    local_relief_meters,
                );
                return Some(surface_material_with_surface(
                    base,
                    top,
                    block_state_ids::DIRT,
                    "minecraft:savanna",
                ));
            }
            let top = lush_vegetation_surface(
                sample,
                metrics,
                patch_noise,
                fine_noise,
                rainforest_score.max(0.45),
                terrain,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                if patch_noise >= 0.54 {
                    "minecraft:jungle"
                } else {
                    "minecraft:sparse_jungle"
                },
            ));
        }
        let jungle_biome = if key.contains("sparse") {
            "minecraft:sparse_jungle"
        } else if sample.tree_cover() >= 0.55 && patch_noise >= 0.64 {
            "minecraft:bamboo_jungle"
        } else if patch_noise < 0.28 && fine_noise < 0.62 {
            "minecraft:sparse_jungle"
        } else {
            "minecraft:jungle"
        };
        let top = lush_vegetation_surface(
            sample,
            metrics,
            patch_noise,
            fine_noise,
            rainforest_score.max(0.58),
            terrain,
        );
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            jungle_biome,
        ));
    }
    if key.contains("savanna") {
        if !vegetation_evidence
            && desert_sand_like
            && sahara_score >= 0.35
            && dry_savanna_score < 0.25
        {
            return None;
        }
        if desert_sand_like
            && vegetation_strength < 0.10
            && !green_like
            && metrics.value >= 0.58
            && (sahara_score >= 0.24 || sahel_score >= 0.20)
            && patch_noise < 0.58
        {
            return None;
        }
        let dry_score = sahel_score.max(dry_savanna_score).max(0.62);
        let top = dry_grass_surface_conservative(
            metrics,
            sample,
            patch_noise,
            fine_noise,
            dry_score,
            terrain,
            semantic_terrain,
            local_relief_meters,
        );
        let savanna_biome = if key.contains("windswept") || elevation_meters >= 900.0 {
            "minecraft:windswept_savanna"
        } else {
            "minecraft:savanna"
        };
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            savanna_biome,
        ));
    }
    if key.contains("desert") {
        if vegetation_evidence
            && (sahel_score >= 0.18 || dry_savanna_score >= 0.18)
            && (!eco_name.contains("sahara desert")
                || sahel_score >= 0.28
                || dry_savanna_score >= 0.28
                || is_sahel_latitude(latitude))
        {
            let top = dry_grass_surface_conservative(
                metrics,
                sample,
                patch_noise,
                fine_noise,
                0.70,
                terrain,
                semantic_terrain,
                local_relief_meters,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ));
        }
        if eco_name.contains("sahara desert")
            && (olive_dry_grass || green_like)
            && (sahel_score >= 0.18 || latitude <= 20.0)
            && patch_noise >= 0.42
        {
            let top = dry_grass_surface_conservative(
                metrics,
                sample,
                patch_noise,
                fine_noise,
                0.62,
                terrain,
                semantic_terrain,
                local_relief_meters,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ));
        }
        let top = hot_desert_surface(
            metrics,
            patch_noise,
            fine_noise,
            local_relief_meters,
            sahara_score,
            terrain,
            semantic_terrain,
        );
        return Some(surface_material_with_surface(
            base,
            top,
            desert_filler(top),
            "minecraft:desert",
        ));
    }
    if key.contains("badlands") {
        let exposed = is_exposed_dry_rock(
            metrics,
            elevation_meters,
            longitude,
            latitude,
            sahara_score,
            dry_savanna_score,
            local_relief_meters,
            slope,
        );
        if exposed && (local_relief_meters >= 160.0 || elevation_meters >= 900.0) {
            return Some(highland_rock_surface(
                base,
                metrics,
                elevation_meters,
                patch_noise,
                fine_noise,
            ));
        }
        if eco_name.contains("desert") && !has_vegetation_evidence(sample, metrics) {
            let mut top = if metrics.red > metrics.green * 1.18 && metrics.hue <= 45.0 {
                block_state_ids::RED_SAND
            } else {
                block_state_ids::SAND
            };
            if metrics.value < 0.42 && fine_noise >= 0.82 {
                top = block_state_ids::COARSE_DIRT;
            }
            return Some(surface_material_with_surface(
                base,
                top,
                desert_filler(top),
                biome,
            ));
        }
        if vegetation_evidence || dry_savanna_score >= 0.20 || olive_dry_grass {
            return None;
        }
        let top = if fine_noise >= 0.88 && !green_like {
            block_state_ids::COARSE_DIRT
        } else {
            block_state_ids::GRASS_BLOCK
        };
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            biome,
        ));
    }
    if key.contains("forest") || key.contains("taiga") {
        if !vegetation_evidence
            && !is_temperate_climate(sample.climate_class)
            && !is_cold_climate(sample.climate_class)
        {
            return None;
        }
        if eco_name.contains("mosaic")
            && vegetation_strength < 0.16
            && (olive_dry_grass || dry_savanna_score >= 0.25)
            && patch_noise < 0.50
        {
            let top = dry_grass_surface_conservative(
                metrics,
                sample,
                patch_noise,
                fine_noise,
                dry_savanna_score.max(0.52),
                terrain,
                semantic_terrain,
                local_relief_meters,
            );
            return Some(surface_material_with_surface(
                base,
                top,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ));
        }
        let forest_biome = if key.contains("forest")
            && sample.tree_cover() < 0.08
            && !green_like
            && olive_dry_grass
        {
            "minecraft:plains"
        } else {
            biome
        };
        let top = temperate_forest_surface(sample, metrics, patch_noise, fine_noise);
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            forest_biome,
        ));
    }
    if key.contains("meadow") || key.contains("plains") {
        let top = if fine_noise >= 0.92 && !green_like && !olive_dry_grass {
            block_state_ids::COARSE_DIRT
        } else {
            block_state_ids::GRASS_BLOCK
        };
        return Some(surface_material_with_surface(
            base,
            top,
            block_state_ids::DIRT,
            biome,
        ));
    }

    None
}

#[allow(clippy::too_many_arguments)]
fn should_defer_ecoregion_at_transition(
    sample: &SurfaceMaterialSample,
    key: &str,
    eco_name: &str,
    metrics: SurfaceColorMetrics,
    elevation_meters: f64,
    latitude: f64,
    sahara_score: f64,
    rainforest_score: f64,
    dry_savanna_score: f64,
    patch_noise: f64,
    fine_noise: f64,
    vegetation_evidence: bool,
) -> bool {
    if sample.ecoregion_confidence >= 0.84 {
        return false;
    }
    if !(key.contains("desert")
        || key.contains("savanna")
        || key.contains("jungle")
        || key.contains("forest")
        || key.contains("badlands")
        || key.contains("plains"))
    {
        return false;
    }
    let green_like = is_green_like(metrics);
    let desert_sand_like = is_desert_sand_like(metrics);
    if eco_name.contains("sahara desert")
        && latitude >= 18.0
        && !vegetation_evidence
        && (desert_sand_like || sahara_score >= 0.55)
    {
        return false;
    }
    if key.contains("jungle")
        && (sample.tree_cover() >= 0.22
            || rainforest_score >= 0.62
            || (is_tropical_rain_climate(sample.climate_class) && green_like))
        && sample.ecoregion_confidence >= 0.56
    {
        return false;
    }
    if key.contains("savanna")
        && (dry_savanna_score >= 0.55 || is_tropical_savanna_climate(sample.climate_class))
        && sample.ecoregion_confidence >= 0.62
    {
        return false;
    }
    if (key.contains("badlands") || key.contains("desert"))
        && elevation_meters >= 700.0
        && sample.slope_ratio() >= 0.20
        && !vegetation_evidence
    {
        return false;
    }
    let uncertainty = (0.84 - sample.ecoregion_confidence).max(0.0) / 0.84;
    let defer_threshold = 0.58_f64.min(0.22 + (uncertainty * 0.48));
    let transition_noise = (patch_noise * 0.65) + (fine_noise * 0.35);
    transition_noise < defer_threshold
}

fn surface_material_semantic_fallback_metrics(
    sample: &SurfaceMaterialSample,
    longitude: f64,
    latitude: f64,
) -> Option<SurfaceColorMetrics> {
    surface_material_semantic_fallback_color(sample, longitude, latitude)
        .map(SurfaceColorMetrics::from)
}

fn surface_material_semantic_fallback_color(
    sample: &SurfaceMaterialSample,
    longitude: f64,
    latitude: f64,
) -> Option<RgbColor> {
    let climate = sample.climate_class;
    let vegetation = sample.vegetation_cover();
    let forest_like = is_forest_like_ecoregion(sample);
    let savanna_like = is_savanna_like_ecoregion(sample);
    let dry_ecoregion = is_dry_ecoregion_biome(sample);

    if sample.snow_cover_ratio() >= 0.18 || climate == 29 || climate == 30 {
        return Some(RgbColor::of(230, 235, 235));
    }
    if sample.swamp_cover_ratio() >= 0.25 {
        return Some(RgbColor::of(45, 85, 55));
    }
    if is_tropical_rain_climate(climate)
        || (forest_like && vegetation >= 0.02)
        || sample.tree_cover() >= 0.06
    {
        return Some(RgbColor::of(45, 96, 42));
    }
    if is_tropical_savanna_climate(climate) || is_steppe_climate(climate) || savanna_like {
        return Some(RgbColor::of(126, 123, 70));
    }
    if is_desert_climate(climate) || dry_ecoregion {
        if vegetation >= 0.04
            && (savanna_like || surface_material_sahel_score(longitude, latitude) >= 0.18)
        {
            return Some(RgbColor::of(126, 123, 70));
        }
        return Some(RgbColor::of(230, 205, 160));
    }
    if is_temperate_climate(climate) || is_cold_climate(climate) || forest_like {
        return Some(RgbColor::of(78, 118, 64));
    }
    if vegetation > 0.0 {
        return Some(RgbColor::of(126, 123, 70));
    }
    if sample.has_ecoregion() || sample.has_ecoregion_biome() {
        return Some(RgbColor::of(104, 128, 68));
    }
    None
}

fn surface_material_with_surface(
    base: &EarthSurfaceColumn,
    top: i32,
    filler: i32,
    biome_id: impl Into<String>,
) -> EarthSurfaceColumn {
    let mut column = EarthSurfaceColumn::new(
        base.water,
        base.ground_surface_y,
        base.water_surface_y,
        top,
        filler,
        biome_id,
        "material-rule",
    );
    column.terrain_token_source = base.terrain_token_source;
    column.data_evidence_flags = base.data_evidence_flags;
    column
}

fn mark_surface_material(column: EarthSurfaceColumn, decision_source: &str) -> EarthSurfaceColumn {
    column.with_decision_source(decision_source)
}

fn with_surface_material_metadata(
    column: EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
) -> EarthSurfaceColumn {
    let mut with_evidence =
        column.with_data_evidence_flags(surface_data_evidence::from_sample(sample));
    if sample.terrain_token_color.available {
        with_evidence = with_evidence.with_terrain_token_source(sample.terrain_token_source);
    }
    with_evidence
}

fn surface_material_bathymetry_ground_surface_y(
    base: &EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
    coast_factor: f64,
    vertical_scale: f64,
) -> i32 {
    if !sample.has_bathymetry() || sample.bathymetry_meters >= -1 {
        return base.ground_surface_y;
    }
    let mut depth_blocks = clamp_i32(
        java_math_round_double_to_narrowed_i32(
            (f64::from(-sample.bathymetry_meters) * vertical_scale) / BATHYMETRY_METERS_PER_BLOCK,
        ),
        2,
        123,
    );
    depth_blocks =
        coastal_bathymetry_shelf_adjusted_depth_blocks(depth_blocks, coast_factor, vertical_scale);
    clamp_i32(SEA_LEVEL_Y - depth_blocks, MIN_SURFACE_Y, SEA_LEVEL_Y - 1)
}

fn surface_material_ocean_biome_id(
    fallback_biome: &str,
    sample: &SurfaceMaterialSample,
    latitude: f64,
    depth: i32,
) -> String {
    let abs_lat = latitude.abs();
    let deep = depth >= 28;
    if abs_lat >= 70.0 {
        return if deep {
            "minecraft:deep_frozen_ocean"
        } else {
            "minecraft:frozen_ocean"
        }
        .to_string();
    }
    if abs_lat >= 56.0 {
        return if deep {
            "minecraft:deep_cold_ocean"
        } else {
            "minecraft:cold_ocean"
        }
        .to_string();
    }
    if abs_lat <= 23.5 {
        return if deep {
            "minecraft:deep_lukewarm_ocean"
        } else {
            "minecraft:warm_ocean"
        }
        .to_string();
    }
    if sample.ocean_temperature != SurfaceMaterialSample::UNKNOWN
        && sample.ocean_temperature >= 30000
    {
        return if deep {
            "minecraft:deep_lukewarm_ocean"
        } else {
            "minecraft:lukewarm_ocean"
        }
        .to_string();
    }
    if fallback_biome.contains("ocean") {
        if deep && !fallback_biome.contains("deep") {
            if fallback_biome.contains("lukewarm") || fallback_biome.contains("warm") {
                return "minecraft:deep_lukewarm_ocean".to_string();
            }
            if fallback_biome.contains("cold") {
                return "minecraft:deep_cold_ocean".to_string();
            }
            return "minecraft:deep_ocean".to_string();
        }
        return fallback_biome.to_string();
    }
    if deep {
        "minecraft:deep_ocean"
    } else {
        "minecraft:ocean"
    }
    .to_string()
}

#[allow(clippy::too_many_arguments)]
fn should_source_land_override_water(
    base: &EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    coast_factor: f64,
) -> bool {
    if !sample.has_bathymetry() || sample.bathymetry_meters < -1 {
        return false;
    }
    if is_open_water_color(sample.color) {
        return false;
    }
    let land_color = is_desert_sand_like(metrics)
        || is_green_like(metrics)
        || is_orange_rock_like(metrics)
        || is_dry_land_neutral(metrics)
        || is_pale_dry_land(metrics)
        || is_snow_like_land(metrics, latitude);
    if !land_color {
        return false;
    }
    if elevation_meters < -6.0 && coast_factor < 0.80 {
        return false;
    }
    base.ground_surface_y >= SEA_LEVEL_Y - 4
        || coast_factor >= 0.65
        || sample.bathymetry_meters >= 0
        || surface_material_sahara_score(longitude, latitude) >= 0.18
}

fn should_bathymetry_override_land(
    base: &EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
    elevation_meters: f64,
    coast_factor: f64,
) -> bool {
    let open_ocean_false_island = sample.has_bathymetry()
        && sample.bathymetry_meters <= -24
        && is_open_water_color(sample.color)
        && elevation_meters <= 20.0
        && base.ground_surface_y <= SEA_LEVEL_Y + 2;
    open_ocean_false_island
        || (sample.has_bathymetry()
            && sample.bathymetry_meters <= -24
            && coast_factor < 0.35
            && elevation_meters <= 10.0
            && base.ground_surface_y <= SEA_LEVEL_Y + 1)
}

fn is_dark_open_water(metrics: SurfaceColorMetrics) -> bool {
    metrics.value <= 0.16
        && metrics.blue >= metrics.red.max(metrics.green) * 1.08
        && metrics.saturation >= 0.24
}

fn is_open_water_color(color: RgbColor) -> bool {
    if !color.available || color.is_near_black() {
        return false;
    }
    let metrics = SurfaceColorMetrics::from(color);
    (170.0..=245.0).contains(&metrics.hue)
        && metrics.saturation >= 0.18
        && metrics.blue >= metrics.red * 1.12
        && metrics.green >= metrics.red * 1.02
}

fn is_dry_land_neutral(metrics: SurfaceColorMetrics) -> bool {
    metrics.value >= 0.34
        && metrics.saturation >= 0.05
        && (18.0..=88.0).contains(&metrics.hue)
        && metrics.red >= metrics.blue * 1.04
        && metrics.green >= metrics.blue * 0.86
}

fn is_pale_dry_land(metrics: SurfaceColorMetrics) -> bool {
    metrics.value >= 0.72
        && metrics.saturation <= 0.16
        && metrics.red >= metrics.blue * 0.92
        && metrics.green >= metrics.blue * 0.92
}

fn is_snow_like_land(metrics: SurfaceColorMetrics, latitude: f64) -> bool {
    latitude.abs() >= 45.0
        && metrics.value >= 0.72
        && metrics.saturation <= 0.22
        && metrics.blue >= metrics.red * 0.86
}

fn is_immediate_beach(base: &EarthSurfaceColumn, coast_factor: f64) -> bool {
    coast_factor >= BEACH_COAST_FACTOR && base.ground_surface_y <= SEA_LEVEL_Y + 2
}

fn is_sandy_beach_color(metrics: SurfaceColorMetrics) -> bool {
    (30.0..=64.0).contains(&metrics.hue)
        && metrics.value >= 0.50
        && metrics.saturation >= 0.08
        && metrics.saturation <= 0.55
        && metrics.red >= metrics.blue * 1.08
        && metrics.green >= metrics.blue * 1.03
        && !is_green_like(metrics)
}

fn should_use_beach_sand(metrics: SurfaceColorMetrics, longitude: f64, latitude: f64) -> bool {
    if !is_sandy_beach_color(metrics) {
        return false;
    }
    let abs_lat = latitude.abs();
    if surface_material_sahara_score(longitude, latitude) >= 0.35 || abs_lat >= 25.0 {
        return surface_material_sahara_score(longitude, latitude) >= 0.35;
    }
    metrics.value >= 0.72 && metrics.saturation <= 0.25
}

fn is_green_like(metrics: SurfaceColorMetrics) -> bool {
    (70.0..=170.0).contains(&metrics.hue)
        && metrics.green >= metrics.red * 0.92
        && metrics.green >= metrics.blue * 1.03
}

fn is_olive_dry_grass(metrics: SurfaceColorMetrics) -> bool {
    (48.0..=92.0).contains(&metrics.hue)
        && metrics.saturation >= 0.12
        && metrics.saturation <= 0.48
        && metrics.value >= 0.24
        && metrics.value <= 0.62
}

fn is_desert_sand_like(metrics: SurfaceColorMetrics) -> bool {
    (25.0..=65.0).contains(&metrics.hue)
        && metrics.value >= 0.42
        && metrics.green >= metrics.blue * 1.03
        && metrics.saturation >= 0.10
}

fn is_orange_rock_like(metrics: SurfaceColorMetrics) -> bool {
    (12.0..=44.0).contains(&metrics.hue)
        && metrics.saturation >= 0.24
        && metrics.value >= 0.26
        && metrics.red >= metrics.green * 1.08
}

fn is_sahel_latitude(latitude: f64) -> bool {
    (6.0..=19.0).contains(&latitude)
}

fn dry_grass_biome(
    dry_score: f64,
    elevation_meters: f64,
    patch_noise: f64,
    fine_noise: f64,
) -> String {
    if elevation_meters >= 900.0 && fine_noise >= 0.52 {
        return "minecraft:windswept_savanna".to_string();
    }
    if patch_noise <= 0.22 && dry_score < 0.62 {
        return if fine_noise <= 0.36 {
            "minecraft:sunflower_plains"
        } else {
            "minecraft:plains"
        }
        .to_string();
    }
    if dry_score >= 0.68 || patch_noise >= 0.72 {
        return "minecraft:savanna".to_string();
    }
    if fine_noise >= 0.58 {
        "minecraft:savanna_plateau"
    } else {
        "minecraft:plains"
    }
    .to_string()
}

fn dry_grass_surface(
    metrics: SurfaceColorMetrics,
    patch_noise: f64,
    fine_noise: f64,
    dry_threshold: f64,
) -> i32 {
    let dryness = (metrics.saturation * 0.38)
        + ((1.0 - metrics.value) * 0.28)
        + (patch_noise * 0.22)
        + (fine_noise * 0.12);
    if !is_green_like(metrics)
        && dryness >= dry_threshold + 0.28
        && metrics.value < 0.48
        && fine_noise >= 0.68
    {
        return block_state_ids::COARSE_DIRT;
    }
    if !is_green_like(metrics) && fine_noise >= 0.92 && patch_noise >= 0.56 && metrics.value < 0.56
    {
        return block_state_ids::COARSE_DIRT;
    }
    block_state_ids::GRASS_BLOCK
}

#[allow(clippy::too_many_arguments)]
fn dry_grass_surface_conservative(
    metrics: SurfaceColorMetrics,
    sample: &SurfaceMaterialSample,
    patch_noise: f64,
    fine_noise: f64,
    dry_score: f64,
    terrain: MetTerrainMatch,
    semantic_terrain: bool,
    local_relief_meters: f64,
) -> i32 {
    let vegetation = sample
        .tree_cover()
        .max(sample.herbaceous_cover())
        .max(sample.shrub_cover());
    let green_like = is_green_like(metrics);
    let sparse_vegetation = vegetation < 0.12 && !green_like;
    let very_sparse_vegetation = vegetation < 0.06 && !green_like;
    let very_dry_dark = metrics.value < 0.44 && metrics.saturation >= 0.28;
    let muted_dry_grass = is_olive_dry_grass(metrics)
        || ((38.0..=86.0).contains(&metrics.hue)
            && metrics.saturation >= 0.14
            && metrics.saturation <= 0.42
            && metrics.value >= 0.34
            && metrics.value <= 0.64);
    let dryness = (metrics.saturation * 0.34)
        + ((1.0 - metrics.value) * 0.28)
        + (patch_noise * 0.24)
        + (fine_noise * 0.14)
        + (dry_score * 0.22);

    if terrain.confident()
        && terrain.kind == MetTerrainKind::CoarseDirt
        && sparse_vegetation
        && dry_score >= 0.52
        && (semantic_terrain || fine_noise >= 0.42 || patch_noise >= 0.54)
    {
        return block_state_ids::COARSE_DIRT;
    }
    if terrain.confident()
        && matches!(terrain.kind, MetTerrainKind::Gravel | MetTerrainKind::Rock)
        && very_sparse_vegetation
        && local_relief_meters >= 90.0
        && dry_score >= 0.58
        && fine_noise >= 0.68
    {
        return block_state_ids::COARSE_DIRT;
    }
    if terrain.confident()
        && matches!(terrain.kind, MetTerrainKind::Sand | MetTerrainKind::RedSand)
        && very_sparse_vegetation
        && dry_score >= 0.72
        && dryness >= 0.72
        && patch_noise >= 0.58
    {
        return block_state_ids::COARSE_DIRT;
    }
    if sparse_vegetation
        && dry_score >= 0.62
        && dryness >= 0.70
        && (fine_noise >= 0.58 || patch_noise >= 0.66)
    {
        return block_state_ids::COARSE_DIRT;
    }
    if sparse_vegetation
        && is_savanna_like_ecoregion(sample)
        && dry_score >= 0.55
        && !green_like
        && (muted_dry_grass || metrics.value < 0.76)
        && dryness >= 0.54
        && (fine_noise >= 0.76 || (patch_noise >= 0.62 && fine_noise >= 0.34))
    {
        return block_state_ids::COARSE_DIRT;
    }
    if sparse_vegetation
        && muted_dry_grass
        && dry_score >= 0.54
        && dryness >= 0.61
        && (fine_noise >= 0.84 || (patch_noise >= 0.72 && fine_noise >= 0.48))
    {
        return block_state_ids::COARSE_DIRT;
    }
    if !green_like
        && muted_dry_grass
        && vegetation < 0.22
        && dry_score >= 0.52
        && dryness >= 0.56
        && fine_noise >= 0.70
        && patch_noise >= 0.34
    {
        return block_state_ids::COARSE_DIRT;
    }
    if sparse_vegetation
        && very_dry_dark
        && dry_score >= 0.70
        && fine_noise >= 0.88
        && patch_noise >= 0.52
    {
        return block_state_ids::COARSE_DIRT;
    }
    block_state_ids::GRASS_BLOCK
}

fn lush_vegetation_surface(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    patch_noise: f64,
    fine_noise: f64,
    rainforest_score: f64,
    terrain: MetTerrainMatch,
) -> i32 {
    let dark_green = is_green_like(metrics) && metrics.value < 0.48;
    let tree_cover = sample.tree_cover();
    let vegetation = sample.vegetation_cover();
    let strong_humid_evidence = rainforest_score >= 0.58
        || is_tropical_rain_climate(sample.climate_class)
        || tree_cover >= 0.18
        || vegetation >= 0.28;
    if sample.terrain_token_source == TerrainTokenSource::Export
        && terrain.confident()
        && terrain.distance_squared == 0
        && matches!(
            terrain.top_block_state_id,
            block_state_ids::MOSS_BLOCK | block_state_ids::PODZOL
        )
        && strong_humid_evidence
    {
        return terrain.top_block_state_id;
    }
    if strong_humid_evidence && dark_green && tree_cover >= 0.28 {
        let forest_floor_texture = (patch_noise * 0.60) + (fine_noise * 0.40);
        if forest_floor_texture >= 0.42 {
            return block_state_ids::MOSS_BLOCK;
        }
        if !is_tropical_rain_climate(sample.climate_class)
            && !is_tropical_savanna_climate(sample.climate_class)
            && fine_noise >= 0.58
        {
            return block_state_ids::PODZOL;
        }
        return block_state_ids::GRASS_BLOCK;
    }
    if strong_humid_evidence
        && dark_green
        && (patch_noise >= 0.54 || tree_cover >= 0.28)
        && fine_noise >= 0.24
    {
        return block_state_ids::MOSS_BLOCK;
    }
    if strong_humid_evidence && tree_cover >= 0.22 && patch_noise >= 0.72 && fine_noise >= 0.46 {
        return block_state_ids::MOSS_BLOCK;
    }
    if tree_cover >= 0.16
        && !is_tropical_rain_climate(sample.climate_class)
        && !is_tropical_savanna_climate(sample.climate_class)
        && patch_noise <= 0.18
        && fine_noise >= 0.62
    {
        return block_state_ids::PODZOL;
    }
    block_state_ids::GRASS_BLOCK
}

fn desert_surface(
    metrics: SurfaceColorMetrics,
    patch_noise: f64,
    fine_noise: f64,
    sahara_score: f64,
    terrain: MetTerrainMatch,
    semantic_terrain: bool,
) -> i32 {
    if terrain.confident() {
        if terrain.kind == MetTerrainKind::Vegetated
            && sahara_score < 0.86
            && (semantic_terrain || metrics.value < 0.70)
        {
            return block_state_ids::GRASS_BLOCK;
        }
        if terrain.kind == MetTerrainKind::CoarseDirt && sahara_score < 0.70 && fine_noise >= 0.68 {
            return block_state_ids::COARSE_DIRT;
        }
        if terrain.kind == MetTerrainKind::Gravel && sahara_score < 0.70 && fine_noise >= 0.76 {
            return block_state_ids::GRAVEL;
        }
    }
    if metrics.red > metrics.green * 1.42
        && metrics.hue <= 32.0
        && fine_noise >= 0.94
        && patch_noise >= 0.58
    {
        return block_state_ids::RED_SAND;
    }
    if metrics.value < 0.36 && metrics.saturation >= 0.34 && patch_noise >= 0.78 {
        return if metrics.hue <= 35.0 {
            block_state_ids::ORANGE_TERRACOTTA
        } else {
            block_state_ids::TERRACOTTA
        };
    }
    if sahara_score >= 0.55 && fine_noise >= 0.80 && patch_noise >= 0.50 {
        return block_state_ids::GRAVEL;
    }
    if sahara_score < 0.58 && metrics.value < 0.50 && fine_noise >= 0.84 {
        return block_state_ids::COARSE_DIRT;
    }
    block_state_ids::SAND
}

fn desert_filler(top: i32) -> i32 {
    match top {
        block_state_ids::SAND
        | block_state_ids::RED_SAND
        | block_state_ids::TERRACOTTA
        | block_state_ids::ORANGE_TERRACOTTA
        | block_state_ids::BROWN_TERRACOTTA => top,
        _ => block_state_ids::DIRT,
    }
}

fn desert_biome(elevation_meters: f64, patch_noise: f64, fine_noise: f64) -> String {
    if elevation_meters >= 900.0 && fine_noise >= 0.60 {
        return "minecraft:eroded_badlands".to_string();
    }
    if patch_noise >= 0.82 {
        return "minecraft:badlands".to_string();
    }
    "minecraft:desert".to_string()
}

fn rainforest_biome(
    rainforest_score: f64,
    dark_vegetation: bool,
    patch_noise: f64,
    fine_noise: f64,
) -> String {
    if !dark_vegetation && rainforest_score < 0.55 {
        return if patch_noise >= 0.62 {
            "minecraft:sparse_jungle"
        } else {
            "minecraft:savanna"
        }
        .to_string();
    }
    if rainforest_score >= 0.68 && patch_noise >= 0.64 && fine_noise >= 0.44 {
        return "minecraft:bamboo_jungle".to_string();
    }
    if dark_vegetation && fine_noise >= 0.34 {
        return "minecraft:jungle".to_string();
    }
    if rainforest_score >= 0.58 && patch_noise >= 0.48 {
        return "minecraft:sparse_jungle".to_string();
    }
    if patch_noise >= 0.58 {
        "minecraft:forest"
    } else {
        "minecraft:savanna"
    }
    .to_string()
}

fn rainforest_biome_with_sample(
    sample: &SurfaceMaterialSample,
    rainforest_score: f64,
    dark_vegetation: bool,
    patch_noise: f64,
    fine_noise: f64,
) -> String {
    let tree_cover = sample.tree_cover();
    let vegetation_cover = sample.vegetation_cover();
    if tree_cover >= 0.35 && patch_noise >= 0.64 && fine_noise >= 0.44 {
        return "minecraft:bamboo_jungle".to_string();
    }
    if dark_vegetation || tree_cover >= 0.16 {
        return if fine_noise >= 0.30 {
            "minecraft:jungle"
        } else {
            "minecraft:sparse_jungle"
        }
        .to_string();
    }
    if tree_cover >= 0.08 || vegetation_cover >= 0.24 || rainforest_score >= 0.58 {
        return if patch_noise >= 0.38 {
            "minecraft:sparse_jungle"
        } else {
            "minecraft:forest"
        }
        .to_string();
    }
    rainforest_biome(rainforest_score, dark_vegetation, patch_noise, fine_noise)
}

fn lush_biome(abs_lat: f64, dark_vegetation: bool, patch_noise: f64) -> String {
    if abs_lat <= 14.0 && dark_vegetation {
        return if patch_noise >= 0.68 {
            "minecraft:bamboo_jungle"
        } else {
            "minecraft:jungle"
        }
        .to_string();
    }
    if abs_lat <= 20.0 {
        return if patch_noise >= 0.58 {
            "minecraft:jungle"
        } else {
            "minecraft:sparse_jungle"
        }
        .to_string();
    }
    if abs_lat >= 48.0 {
        return if dark_vegetation || patch_noise >= 0.55 {
            "minecraft:taiga"
        } else {
            "minecraft:forest"
        }
        .to_string();
    }
    if dark_vegetation || patch_noise >= 0.64 {
        return "minecraft:dark_forest".to_string();
    }
    if abs_lat >= 24.0 {
        return "minecraft:forest".to_string();
    }
    "minecraft:savanna".to_string()
}

fn forest_biome_for_environment(
    latitude: f64,
    climate: i32,
    tree_cover: f64,
    dark_vegetation: bool,
    patch_noise: f64,
) -> String {
    let abs_lat = latitude.abs();
    if climate == 1 || climate == 2 || abs_lat <= 12.0 {
        if tree_cover >= 0.65 && patch_noise >= 0.58 {
            return "minecraft:bamboo_jungle".to_string();
        }
        return if dark_vegetation || tree_cover >= 0.20 {
            "minecraft:jungle"
        } else {
            "minecraft:sparse_jungle"
        }
        .to_string();
    }
    if abs_lat >= 56.0 || is_cold_climate(climate) {
        return "minecraft:taiga".to_string();
    }
    if dark_vegetation && tree_cover >= 0.24 {
        return "minecraft:dark_forest".to_string();
    }
    "minecraft:forest".to_string()
}

fn temperate_biome(
    latitude: f64,
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    patch_noise: f64,
    fine_noise: f64,
) -> String {
    let eco_name = sample.ecoregion_name.to_ascii_lowercase();
    let broadleaf = eco_name.contains("broadleaf")
        || eco_name.contains("mixed")
        || eco_name.contains("temperate");
    let boreal =
        eco_name.contains("boreal") || eco_name.contains("conifer") || eco_name.contains("taiga");
    if is_forest_like_ecoregion(sample) && sample.ecoregion_confidence >= 0.55 {
        if boreal || (latitude.abs() >= 55.0 && !broadleaf) {
            return if patch_noise >= 0.42 {
                "minecraft:taiga"
            } else {
                "minecraft:forest"
            }
            .to_string();
        }
        if patch_noise >= 0.72 && metrics.value < 0.46 {
            return "minecraft:dark_forest".to_string();
        }
        if sample.tree_cover() >= 0.14 && fine_noise <= 0.18 && patch_noise >= 0.46 {
            return "minecraft:flower_forest".to_string();
        }
        if fine_noise < 0.22
            && sample.tree_cover() < 0.08
            && metrics.value >= 0.38
            && !sample.has_vegetation_presence()
        {
            return if patch_noise >= 0.42 {
                "minecraft:sunflower_plains"
            } else {
                "minecraft:plains"
            }
            .to_string();
        }
        return "minecraft:forest".to_string();
    }
    if latitude.abs() >= 50.0
        && (sample.tree_cover() >= 0.06 || (is_green_like(metrics) && metrics.value < 0.46))
    {
        if sample.tree_cover() >= 0.14 && fine_noise <= 0.16 {
            return "minecraft:flower_forest".to_string();
        }
        return "minecraft:forest".to_string();
    }
    if latitude.abs() >= 56.0 {
        return if sample.tree_cover() >= 0.10 || patch_noise >= 0.46 {
            "minecraft:taiga"
        } else {
            "minecraft:plains"
        }
        .to_string();
    }
    if sample.tree_cover() >= 0.12 || metrics.value < 0.42 || patch_noise >= 0.56 {
        if sample.tree_cover() >= 0.14 && fine_noise <= 0.16 {
            return "minecraft:flower_forest".to_string();
        }
        return forest_biome_for_environment(
            latitude,
            sample.climate_class,
            sample.tree_cover(),
            is_green_like(metrics) && metrics.value < 0.48,
            patch_noise,
        );
    }
    if patch_noise >= 0.70 {
        "minecraft:sunflower_plains"
    } else {
        "minecraft:plains"
    }
    .to_string()
}

fn temperate_grassland_biome(
    latitude: f64,
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    patch_noise: f64,
    fine_noise: f64,
) -> String {
    let abs_lat = latitude.abs();
    let eco_name = sample.ecoregion_name.to_ascii_lowercase();
    let forest_edge = is_forest_like_ecoregion(sample)
        || eco_name.contains("woodland")
        || eco_name.contains("mosaic");
    let tree_patch =
        sample.tree_cover() >= 0.10 || (sample.tree_cover() >= 0.06 && patch_noise >= 0.64);
    if abs_lat >= 56.0 && (tree_patch || patch_noise >= 0.76) {
        return "minecraft:taiga".to_string();
    }
    if elevation_friendly_meadow(sample, metrics, patch_noise, fine_noise) {
        return "minecraft:meadow".to_string();
    }
    if forest_edge && tree_patch && patch_noise >= 0.58 {
        return forest_biome_for_environment(
            latitude,
            sample.climate_class,
            sample.tree_cover(),
            is_green_like(metrics) && metrics.value < 0.48,
            patch_noise,
        );
    }
    if fine_noise >= 0.92
        && sample.shrub_cover() > sample.tree_cover().max(sample.herbaceous_cover())
    {
        return if abs_lat >= 45.0 {
            "minecraft:taiga"
        } else {
            "minecraft:forest"
        }
        .to_string();
    }
    if patch_noise >= 0.58 && sample.herbaceous_cover() >= sample.shrub_cover() {
        return "minecraft:sunflower_plains".to_string();
    }
    "minecraft:plains".to_string()
}

fn elevation_friendly_meadow(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    patch_noise: f64,
    fine_noise: f64,
) -> bool {
    if sample.tree_cover() >= 0.10 || sample.shrub_cover() > sample.herbaceous_cover() {
        return false;
    }
    sample.herbaceous_cover() >= 0.12
        && metrics.value >= 0.40
        && patch_noise >= 0.68
        && fine_noise <= 0.34
}

fn highland_rock_surface(
    base: &EarthSurfaceColumn,
    metrics: SurfaceColorMetrics,
    elevation_meters: f64,
    patch_noise: f64,
    fine_noise: f64,
) -> EarthSurfaceColumn {
    if elevation_meters >= 2_400.0 || base.ground_surface_y >= 165 {
        return surface_material_with_surface(
            base,
            block_state_ids::STONE,
            block_state_ids::STONE,
            "minecraft:windswept_hills",
        );
    }
    let top = if metrics.hue <= 35.0 || metrics.red > metrics.green * 1.18 {
        block_state_ids::ORANGE_TERRACOTTA
    } else {
        block_state_ids::TERRACOTTA
    };
    let biome = if elevation_meters >= 1_200.0 || fine_noise >= 0.68 || patch_noise >= 0.68 {
        "minecraft:wooded_badlands"
    } else {
        "minecraft:badlands"
    };
    surface_material_with_surface(base, top, top, biome)
}

#[allow(clippy::too_many_arguments)]
fn hot_desert_surface_column(
    base: &EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    sahara_score: f64,
    dry_savanna_score: f64,
    patch_noise: f64,
    fine_noise: f64,
    local_relief_meters: f64,
    terrain: MetTerrainMatch,
    semantic_terrain: bool,
) -> EarthSurfaceColumn {
    if is_exposed_dry_rock(
        metrics,
        elevation_meters,
        longitude,
        latitude,
        sahara_score,
        dry_savanna_score,
        local_relief_meters,
        sample.slope_ratio(),
    ) && (metrics.saturation >= 0.24 || fine_noise >= 0.76)
    {
        return highland_rock_surface(base, metrics, elevation_meters, patch_noise, fine_noise);
    }
    let top = hot_desert_surface(
        metrics,
        patch_noise,
        fine_noise,
        local_relief_meters,
        sahara_score,
        terrain,
        semantic_terrain,
    );
    surface_material_with_surface(
        base,
        top,
        desert_filler(top),
        hot_desert_biome(
            elevation_meters,
            local_relief_meters,
            patch_noise,
            fine_noise,
        ),
    )
}

fn hot_desert_surface(
    metrics: SurfaceColorMetrics,
    patch_noise: f64,
    fine_noise: f64,
    local_relief_meters: f64,
    sahara_score: f64,
    terrain: MetTerrainMatch,
    semantic_terrain: bool,
) -> i32 {
    if terrain.confident() {
        if terrain.kind == MetTerrainKind::Vegetated
            && (semantic_terrain || (metrics.value < 0.70 && metrics.saturation < 0.42))
        {
            return block_state_ids::GRASS_BLOCK;
        }
        if terrain.kind == MetTerrainKind::CoarseDirt
            && fine_noise >= 0.68
            && (sahara_score < 0.78 || local_relief_meters >= 120.0)
        {
            return block_state_ids::COARSE_DIRT;
        }
        if terrain.kind == MetTerrainKind::Gravel
            && local_relief_meters >= 120.0
            && fine_noise >= 0.62
        {
            return block_state_ids::GRAVEL;
        }
        if terrain.kind == MetTerrainKind::RedSand
            && local_relief_meters >= 120.0
            && fine_noise >= 0.78
            && patch_noise >= 0.50
        {
            return block_state_ids::RED_SAND;
        }
    }
    if metrics.red > metrics.green * 1.42
        && metrics.hue <= 32.0
        && local_relief_meters >= 180.0
        && fine_noise >= 0.96
        && patch_noise >= 0.62
    {
        return block_state_ids::RED_SAND;
    }
    if metrics.saturation >= 0.20
        && metrics.value < 0.54
        && local_relief_meters >= 120.0
        && fine_noise >= 0.86
        && patch_noise >= 0.58
    {
        return if metrics.hue <= 36.0 {
            block_state_ids::ORANGE_TERRACOTTA
        } else {
            block_state_ids::TERRACOTTA
        };
    }
    let desert_texture = (patch_noise * 0.47) + (fine_noise * 0.53);
    let muted_sand_texture =
        (26.0..=68.0).contains(&metrics.hue) && metrics.saturation >= 0.10 && metrics.value < 0.78;
    if sahara_score >= 0.42 && muted_sand_texture {
        if local_relief_meters >= 80.0 && desert_texture >= 0.76 && metrics.value < 0.70 {
            return block_state_ids::GRAVEL;
        }
        if metrics.value < 0.58 && desert_texture >= 0.72 && fine_noise >= 0.60 {
            return block_state_ids::COARSE_DIRT;
        }
        if local_relief_meters >= 65.0
            && metrics.red > metrics.green * 1.10
            && metrics.hue <= 44.0
            && desert_texture >= 0.78
        {
            return block_state_ids::RED_SAND;
        }
    }
    if sahara_score >= 0.55 && fine_noise >= 0.80 && patch_noise >= 0.50 {
        return block_state_ids::GRAVEL;
    }
    if sahara_score < 0.45 && metrics.value < 0.48 && fine_noise >= 0.90 && patch_noise >= 0.60 {
        return block_state_ids::COARSE_DIRT;
    }
    block_state_ids::SAND
}

fn hot_desert_biome(
    elevation_meters: f64,
    local_relief_meters: f64,
    patch_noise: f64,
    fine_noise: f64,
) -> &'static str {
    if elevation_meters >= 1_100.0 && local_relief_meters >= 220.0 && fine_noise >= 0.62 {
        return "minecraft:eroded_badlands";
    }
    if local_relief_meters >= 260.0 && patch_noise >= 0.84 {
        return "minecraft:badlands";
    }
    "minecraft:desert"
}

fn mediterranean_surface(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    fine_noise: f64,
) -> i32 {
    let low_vegetation = sample
        .tree_cover()
        .max(sample.herbaceous_cover())
        .max(sample.shrub_cover())
        < 0.10;
    if low_vegetation && !is_green_like(metrics) && metrics.value < 0.50 && fine_noise >= 0.92 {
        return block_state_ids::COARSE_DIRT;
    }
    block_state_ids::GRASS_BLOCK
}

fn mediterranean_biome(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    patch_noise: f64,
) -> String {
    if sample.has_ecoregion()
        && sample
            .ecoregion_name
            .to_ascii_lowercase()
            .contains("mediterranean")
        && patch_noise < 0.34
        && !is_green_like(metrics)
    {
        return if patch_noise < 0.16 {
            "minecraft:sunflower_plains"
        } else {
            "minecraft:savanna"
        }
        .to_string();
    }
    if sample.tree_cover() >= 0.12 || metrics.value < 0.42 || patch_noise >= 0.58 {
        return if patch_noise >= 0.82 && sample.tree_cover() >= 0.18 {
            "minecraft:dark_forest"
        } else {
            "minecraft:forest"
        }
        .to_string();
    }
    if patch_noise >= 0.42 {
        "minecraft:sunflower_plains"
    } else {
        "minecraft:plains"
    }
    .to_string()
}

fn temperate_forest_surface(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    patch_noise: f64,
    fine_noise: f64,
) -> i32 {
    let dark_green = is_green_like(metrics) && metrics.value < 0.46;
    if (sample.tree_cover() >= 0.20 || dark_green) && patch_noise >= 0.74 && fine_noise >= 0.34 {
        return block_state_ids::MOSS_BLOCK;
    }
    if sample.tree_cover() >= 0.10 && patch_noise <= 0.24 && fine_noise >= 0.58 {
        return block_state_ids::PODZOL;
    }
    block_state_ids::GRASS_BLOCK
}

fn fallback_biome(base: &EarthSurfaceColumn, latitude: f64, patch_noise: f64) -> String {
    let biome = non_beach_biome(base, latitude);
    if biome != "minecraft:plains" {
        return biome;
    }
    let abs_lat = latitude.abs();
    if abs_lat >= 50.0 && patch_noise >= 0.48 {
        return "minecraft:taiga".to_string();
    }
    if abs_lat >= 32.0 && patch_noise >= 0.56 {
        return "minecraft:forest".to_string();
    }
    biome
}

fn non_beach_biome(base: &EarthSurfaceColumn, latitude: f64) -> String {
    if base.biome_id != "minecraft:beach" {
        return base.biome_id.clone();
    }
    if latitude.abs() <= 23.5 {
        "minecraft:savanna"
    } else {
        "minecraft:plains"
    }
    .to_string()
}

fn is_forest_like_ecoregion(sample: &SurfaceMaterialSample) -> bool {
    if !sample.has_ecoregion() {
        return false;
    }
    let name = sample.ecoregion_name.to_ascii_lowercase();
    let biome = sample.ecoregion_biome_id.to_ascii_lowercase();
    biome.contains("forest")
        || biome.contains("jungle")
        || biome.contains("taiga")
        || name.contains("forest")
        || name.contains("woodland")
        || name.contains("broadleaf")
        || name.contains("conifer")
}

fn is_savanna_like_ecoregion(sample: &SurfaceMaterialSample) -> bool {
    if !sample.has_ecoregion() {
        return false;
    }
    let name = sample.ecoregion_name.to_ascii_lowercase();
    let biome = sample.ecoregion_biome_id.to_ascii_lowercase();
    biome.contains("savanna")
        || biome.contains("plains")
        || name.contains("savanna")
        || name.contains("grassland")
        || name.contains("steppe")
        || name.contains("xeric")
}

fn is_named_forest_savanna_mosaic(sample: &SurfaceMaterialSample) -> bool {
    if !sample.has_ecoregion() {
        return false;
    }
    let name = sample.ecoregion_name.to_ascii_lowercase();
    name.contains("forest-savanna")
        || name.contains("forest savanna")
        || (name.contains("mosaic")
            && (name.contains("forest") || name.contains("woodland") || name.contains("savanna")))
}

#[allow(clippy::too_many_arguments)]
fn is_forest_savanna_mosaic_intent(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    longitude: f64,
    latitude: f64,
    sahara_score: f64,
    rainforest_score: f64,
    dry_savanna_score: f64,
    token_vegetated: bool,
    patch_noise: f64,
    fine_noise: f64,
) -> bool {
    let biome_id = sample.ecoregion_biome_id.to_ascii_lowercase();
    let sparse_jungle_savanna_edge = biome_id.contains("sparse_jungle")
        && (is_tropical_savanna_climate(sample.climate_class) || dry_savanna_score >= 0.18);
    let climate_ecotone = is_humid_dry_tropical_ecotone(
        sample,
        metrics,
        longitude,
        latitude,
        rainforest_score,
        dry_savanna_score,
        patch_noise,
        fine_noise,
    );
    if !is_named_forest_savanna_mosaic(sample) && !sparse_jungle_savanna_edge && !climate_ecotone {
        return false;
    }
    let vegetation_evidence = has_vegetation_evidence(sample, metrics);
    if is_dry_core_ecoregion_evidence(sample, metrics, vegetation_evidence)
        || is_desert_climate(sample.climate_class)
        || sahara_score >= 0.35
    {
        return false;
    }
    let ecological_edge = is_tropical_savanna_climate(sample.climate_class)
        || is_tropical_rain_climate(sample.climate_class)
        || rainforest_score >= 0.15
        || dry_savanna_score >= 0.15;
    let evidence = sample.tree_cover() >= 0.10
        || sample.vegetation_cover() >= 0.18
        || is_green_like(metrics)
        || is_olive_dry_grass(metrics)
        || token_vegetated;
    let strong_rainforest_core = is_tropical_rain_climate(sample.climate_class)
        && sample.tree_cover() >= 0.60
        && rainforest_score >= 0.65;
    ecological_edge && evidence && !strong_rainforest_core
}

fn is_humid_forest_core(
    sample: &SurfaceMaterialSample,
    rainforest_score: f64,
    sahel_score: f64,
    dry_savanna_score: f64,
) -> bool {
    let biome = sample.ecoregion_biome_id.to_ascii_lowercase();
    let name = sample.ecoregion_name.to_ascii_lowercase();
    let forest_named = biome.contains("jungle")
        || biome.contains("forest")
        || name.contains("rainforest")
        || name.contains("lowland forest");
    let humid_climate = sample.climate_class == 1 || sample.climate_class == 2;
    forest_named
        && humid_climate
        && sample.canopy_cover() >= 0.24
        && rainforest_score >= 0.18
        && sahel_score < 0.18
        && dry_savanna_score < 0.30
}

#[allow(clippy::too_many_arguments)]
fn is_humid_dry_tropical_ecotone(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    longitude: f64,
    latitude: f64,
    rainforest_score: f64,
    dry_savanna_score: f64,
    patch_noise: f64,
    fine_noise: f64,
) -> bool {
    let sahel_score = surface_material_sahel_score(longitude, latitude);
    if latitude >= 3.0 && (-22.0..=45.0).contains(&longitude) && sahel_score >= 0.08 {
        return false;
    }
    let humid_side = is_tropical_rain_climate(sample.climate_class)
        || rainforest_score >= 0.22
        || sample.tree_cover() >= 0.12
        || is_forest_like_ecoregion(sample);
    let dry_side = is_tropical_savanna_climate(sample.climate_class)
        || sahel_score >= 0.10
        || dry_savanna_score >= 0.10
        || is_savanna_like_ecoregion(sample)
        || is_olive_dry_grass(metrics);
    if !humid_side || !dry_side {
        return false;
    }
    if sample.tree_cover() >= 0.60 && rainforest_score >= 0.62 && patch_noise < 0.78 {
        return false;
    }
    let transition_latitude = latitude.abs() <= 18.0 || dry_savanna_score >= 0.20;
    let transition_texture =
        patch_noise >= 0.18 || fine_noise >= 0.34 || sample.tree_cover() < 0.42;
    transition_latitude
        && transition_texture
        && sample.vegetation_cover() >= 0.08
        && !is_desert_sand_like(metrics)
}

fn is_dry_ecoregion_biome(sample: &SurfaceMaterialSample) -> bool {
    if !sample.has_ecoregion_biome() {
        return false;
    }
    let mut key = sample.ecoregion_biome_id.as_str();
    if let Some(stripped) = key.strip_prefix("minecraft:") {
        key = stripped;
    }
    let key = key.to_ascii_lowercase();
    key.contains("desert") || key.contains("badlands")
}

fn has_vegetation_evidence(sample: &SurfaceMaterialSample, metrics: SurfaceColorMetrics) -> bool {
    if sample.tree_cover() >= 0.08
        || sample.herbaceous_cover() >= 0.16
        || sample.shrub_cover() >= 0.16
        || is_green_like(metrics)
        || is_olive_dry_grass(metrics)
    {
        return true;
    }
    let vegetation = sample.vegetation_cover();
    if vegetation <= 0.0 {
        return false;
    }
    let climate_supports_vegetation = is_tropical_rain_climate(sample.climate_class)
        || is_tropical_savanna_climate(sample.climate_class)
        || is_steppe_climate(sample.climate_class)
        || is_temperate_climate(sample.climate_class)
        || is_cold_climate(sample.climate_class);
    let ecoregion_supports_vegetation =
        is_forest_like_ecoregion(sample) || is_savanna_like_ecoregion(sample);
    if (climate_supports_vegetation || ecoregion_supports_vegetation) && vegetation >= 0.035 {
        return true;
    }
    !is_desert_climate(sample.climate_class)
        && !is_dry_ecoregion_biome(sample)
        && vegetation >= 0.08
}

fn is_dry_core_ecoregion_evidence(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    vegetation_evidence: bool,
) -> bool {
    if !is_dry_ecoregion_biome(sample) {
        return false;
    }
    let desert_like =
        is_desert_sand_like(metrics) || is_orange_rock_like(metrics) || metrics.value >= 0.48;
    if vegetation_evidence || is_green_like(metrics) || !desert_like {
        return false;
    }
    if sample.ecoregion_confidence >= 0.70 {
        return true;
    }
    sample.ecoregion_confidence >= 0.45
        && sample.slope_ratio() >= 0.16
        && (is_desert_sand_like(metrics) || is_orange_rock_like(metrics))
}

fn is_trusted_vegetated_token(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    terrain: MetTerrainMatch,
    semantic_terrain: bool,
    java_standard_terrain: bool,
) -> bool {
    if !terrain.confident() || terrain.kind != MetTerrainKind::Vegetated {
        return false;
    }
    if semantic_terrain {
        return true;
    }
    if !java_standard_terrain {
        return false;
    }
    is_green_like(metrics)
        || is_olive_dry_grass(metrics)
        || sample.tree_cover() >= 0.08
        || sample.herbaceous_cover() >= 0.16
        || sample.shrub_cover() >= 0.16
        || is_forest_like_ecoregion(sample)
        || is_savanna_like_ecoregion(sample)
        || is_temperate_climate(sample.climate_class)
        || is_tropical_savanna_climate(sample.climate_class)
}

fn is_sparse_dry_open_tropical(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
    sahel_score: f64,
    dry_savanna_score: f64,
    token_vegetated: bool,
) -> bool {
    if !is_tropical_rain_climate(sample.climate_class) {
        return false;
    }
    let dry_open = is_savanna_like_ecoregion(sample)
        || dry_savanna_score >= 0.15
        || sahel_score >= 0.12
        || is_tropical_savanna_climate(sample.climate_class);
    if !dry_open {
        return false;
    }
    sample.canopy_cover() < 0.22
        && sample.tree_cover() < 0.14
        && sample.vegetation_cover() < 0.42
        && (is_olive_dry_grass(metrics)
            || token_vegetated
            || !is_green_like(metrics)
            || metrics.value >= 0.40)
}

fn is_tropical_rain_climate(climate: i32) -> bool {
    climate == 1 || climate == 2
}

fn is_tropical_savanna_climate(climate: i32) -> bool {
    climate == 3
}

fn is_desert_climate(climate: i32) -> bool {
    climate == 4 || climate == 5
}

fn is_steppe_climate(climate: i32) -> bool {
    climate == 6 || climate == 7
}

fn is_temperate_climate(climate: i32) -> bool {
    (8..=16).contains(&climate)
}

fn is_cold_climate(climate: i32) -> bool {
    (17..=28).contains(&climate)
}

#[allow(clippy::too_many_arguments)]
fn is_exposed_dry_rock(
    metrics: SurfaceColorMetrics,
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    sahara_score: f64,
    dry_savanna_score: f64,
    local_relief_meters: f64,
    slope_ratio: f64,
) -> bool {
    let strong_slope_evidence = slope_ratio >= 0.20;
    if !strong_slope_evidence
        && dry_savanna_score >= 0.35
        && latitude <= -10.0
        && elevation_meters < 1_800.0
        && metrics.value >= 0.40
    {
        return false;
    }
    let ruggedness =
        surface_material_ecology_noise(longitude, latitude, 1.8, 0x2c1f8d54c73a92bd_u64 as i64);
    let high_rock = (elevation_meters >= 1_100.0 && local_relief_meters >= 140.0)
        || (elevation_meters >= 700.0 && local_relief_meters >= 220.0 && ruggedness >= 0.50)
        || (elevation_meters >= 550.0 && slope_ratio >= 0.28);
    let dark_rock = metrics.value <= 0.48 && metrics.saturation >= 0.30;
    let red_rock = metrics.red >= metrics.green * 1.16 && metrics.saturation >= 0.36;
    let saharan_massif = sahara_score >= 0.55
        && elevation_meters >= 550.0
        && (local_relief_meters >= 180.0 || slope_ratio >= 0.24)
        && (dark_rock || ruggedness >= 0.70);
    (high_rock && (dark_rock || red_rock || ruggedness >= 0.60 || slope_ratio >= 0.34))
        || saharan_massif
}

fn surface_material_met_terrain(
    sample: &SurfaceMaterialSample,
    metrics: SurfaceColorMetrics,
) -> MetTerrainMatch {
    if sample.terrain_token_color.available
        && sample.terrain_token_source == TerrainTokenSource::Export
    {
        let exact = MetTerrainVocabulary::exact(sample.terrain_token_color);
        if exact.confident() {
            return exact;
        }
    }
    MetTerrainVocabulary::nearest(metrics.color())
}

fn surface_material_sahara_score(longitude: f64, latitude: f64) -> f64 {
    let mut score = surface_material_textured_smooth_box(
        longitude,
        latitude,
        -20.0,
        38.0,
        15.0,
        34.0,
        5.5,
        0x3340b42e9c8f1231,
    );
    score = score.max(surface_material_textured_smooth_box(
        longitude,
        latitude,
        35.0,
        60.0,
        12.0,
        32.0,
        4.0,
        0x5b18c62a7f921935,
    ));
    score
}

fn surface_material_sahel_score(longitude: f64, latitude: f64) -> f64 {
    surface_material_textured_smooth_box(
        longitude,
        latitude,
        -20.0,
        45.0,
        7.0,
        17.5,
        4.5,
        0x71a9e2d5065c17a1,
    )
}

fn surface_material_rainforest_score(longitude: f64, latitude: f64) -> f64 {
    let mut score = surface_material_textured_smooth_box(
        longitude,
        latitude,
        -16.0,
        10.0,
        3.0,
        10.0,
        2.5,
        0x119b6617db734561,
    );
    score = score.max(surface_material_textured_smooth_box(
        longitude,
        latitude,
        8.0,
        33.0,
        -9.0,
        7.0,
        4.0,
        0x3a86d32fd8e71855,
    ));
    score = score.max(surface_material_textured_smooth_box(
        longitude,
        latitude,
        95.0,
        145.0,
        -11.0,
        20.0,
        4.5,
        0x6e8ac59a6f19d72b,
    ));
    score = score.max(surface_material_textured_smooth_box(
        longitude,
        latitude,
        -77.0,
        -45.0,
        -16.0,
        7.0,
        4.5,
        0x243f6a8885a308d3,
    ));
    score
}

fn surface_material_dry_savanna_score(longitude: f64, latitude: f64) -> f64 {
    let mut score = surface_material_textured_smooth_box(
        longitude,
        latitude,
        -18.0,
        42.0,
        -35.0,
        -10.0,
        5.0,
        0x5225f1ab3df447c9,
    );
    score = score.max(surface_material_textured_smooth_box(
        longitude,
        latitude,
        24.0,
        45.0,
        -8.0,
        12.0,
        4.0,
        0x21cf64acb1a77e15,
    ));
    score = score.max(surface_material_textured_smooth_box(
        longitude,
        latitude,
        -80.0,
        -36.0,
        -34.0,
        -8.0,
        4.5,
        0x789f2bc3d49b7011,
    ));
    score = score.max(surface_material_textured_smooth_box(
        longitude,
        latitude,
        110.0,
        155.0,
        -38.0,
        -12.0,
        5.0,
        0x14f9e6b7556303f1,
    ));
    let texture = surface_material_ecology_noise(longitude, latitude, 0.35, 0x1f5b28a9c472d733);
    clamp_unit(score + ((texture - 0.5) * 0.18))
}

fn surface_material_mediterranean_score(longitude: f64, latitude: f64) -> f64 {
    let mut score = surface_material_textured_smooth_box(
        longitude,
        latitude,
        -11.0,
        43.0,
        31.0,
        46.0,
        4.0,
        0x68405c2d09d2ec45,
    );
    score = score.max(surface_material_textured_smooth_box(
        longitude,
        latitude,
        -125.0,
        -112.0,
        30.0,
        42.0,
        3.0,
        0x63cf8b99e48aa305,
    ));
    score = score.max(surface_material_textured_smooth_box(
        longitude,
        latitude,
        115.0,
        147.0,
        -39.0,
        -28.0,
        3.5,
        0x0f73e21989b4c351,
    ));
    let texture = surface_material_ecology_noise(longitude, latitude, 0.45, 0x7b3e2f64a91c0d11);
    clamp_unit(score + ((texture - 0.5) * 0.16))
}

#[allow(clippy::too_many_arguments)]
fn surface_material_textured_smooth_box(
    longitude: f64,
    latitude: f64,
    min_longitude: f64,
    max_longitude: f64,
    min_latitude: f64,
    max_latitude: f64,
    edge_degrees: f64,
    seed: i64,
) -> f64 {
    let longitude_jitter = (surface_material_ecology_noise(longitude, latitude, 0.38, seed) - 0.5)
        * edge_degrees
        * 2.8;
    let latitude_jitter = (surface_material_ecology_noise(
        longitude,
        latitude,
        0.38,
        seed ^ (0x9e3779b97f4a7c15_u64 as i64),
    ) - 0.5)
        * edge_degrees
        * 2.8;
    let score = smooth_box(
        longitude + longitude_jitter,
        latitude + latitude_jitter,
        min_longitude,
        max_longitude,
        min_latitude,
        max_latitude,
        edge_degrees,
    );
    let edge_texture = surface_material_ecology_noise(
        longitude,
        latitude,
        1.15,
        seed ^ (0xbf58476d1ce4e5b9_u64 as i64),
    );
    clamp_unit(score + ((edge_texture - 0.5) * 0.20))
}

fn surface_material_ecology_noise(longitude: f64, latitude: f64, frequency: f64, seed: i64) -> f64 {
    fractal_value_noise(longitude * frequency, latitude * frequency, 3, seed)
}

fn clamp_surface_material_color(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[derive(Clone, Debug, PartialEq)]
pub struct PhotoSurfaceInput {
    pub semantic_column: EarthSurfaceColumn,
    pub sample: SurfaceMaterialSample,
    pub elevation_meters: f64,
    pub longitude: f64,
    pub latitude: f64,
    pub coast_factor: f64,
    pub local_relief_meters: f64,
    pub global_block_x: i32,
    pub global_block_z: i32,
    pub token_luma_profile: Option<Arc<PhotoSurfaceTokenLumaProfile>>,
}

impl PhotoSurfaceInput {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        semantic_column: EarthSurfaceColumn,
        sample: SurfaceMaterialSample,
        elevation_meters: f64,
        longitude: f64,
        latitude: f64,
        coast_factor: f64,
        local_relief_meters: f64,
        global_block_x: i32,
        global_block_z: i32,
    ) -> Self {
        Self {
            semantic_column,
            sample,
            elevation_meters,
            longitude,
            latitude,
            coast_factor,
            local_relief_meters,
            global_block_x,
            global_block_z,
            token_luma_profile: None,
        }
    }

    pub fn with_token_luma_profile(mut self, profile: Arc<PhotoSurfaceTokenLumaProfile>) -> Self {
        self.token_luma_profile = Some(profile);
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhotoSurfaceTokenLumaProfile {
    ranges: HashMap<i32, PhotoSurfaceTokenLumaRange>,
}

impl PhotoSurfaceTokenLumaProfile {
    const MIN_PROFILE_SAMPLES: usize = 32;

    pub fn add(&mut self, sample: &SurfaceMaterialSample) {
        if sample.terrain_token_source != TerrainTokenSource::JavaStandardPalette
            || !sample.color.available
            || sample.color.is_near_black()
            || !sample.terrain_token_color.available
        {
            return;
        }
        let token = MetTerrainVocabulary::exact(sample.terrain_token_color);
        if !source_ranked_cross_crop_token(token) {
            return;
        }
        self.ranges
            .entry(met_terrain_match_rgb(token))
            .or_default()
            .add(sample.color);
    }

    pub fn normalized_luma(&self, token: MetTerrainMatch, source: RgbColor) -> Option<f64> {
        let range = self.ranges.get(&met_terrain_match_rgb(token))?;
        if range.count < Self::MIN_PROFILE_SAMPLES || range.max <= range.min + 1.0e-9 {
            return None;
        }
        Some(((photo_luma(source) - range.min) / (range.max - range.min)).clamp(0.0, 1.0))
    }

    pub fn mean_source(&self, token: MetTerrainMatch) -> Option<RgbColor> {
        let range = self.ranges.get(&met_terrain_match_rgb(token))?;
        if range.count < Self::MIN_PROFILE_SAMPLES {
            return None;
        }
        Some(range.mean_rgb())
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PhotoSurfaceTokenLumaRange {
    count: usize,
    min: f64,
    max: f64,
    red_sum: u64,
    green_sum: u64,
    blue_sum: u64,
}

impl Default for PhotoSurfaceTokenLumaRange {
    fn default() -> Self {
        Self {
            count: 0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            red_sum: 0,
            green_sum: 0,
            blue_sum: 0,
        }
    }
}

impl PhotoSurfaceTokenLumaRange {
    fn add(&mut self, color: RgbColor) {
        self.count += 1;
        self.red_sum += u64::from(color.red);
        self.green_sum += u64::from(color.green);
        self.blue_sum += u64::from(color.blue);
        let luma = photo_luma(color);
        self.min = self.min.min(luma);
        self.max = self.max.max(luma);
    }

    fn mean_rgb(&self) -> RgbColor {
        RgbColor::of(
            ((self.red_sum as f64) / (self.count as f64)).round() as u8,
            ((self.green_sum as f64) / (self.count as f64)).round() as u8,
            ((self.blue_sum as f64) / (self.count as f64)).round() as u8,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhotoSurfaceDecision {
    pub top_block_state_id: i32,
    pub filler_block_state_id: i32,
    pub biome_id: String,
    pub rendered_rgb: i32,
    pub recipe_id: String,
    pub stage_id: String,
    pub trace: String,
}

impl PhotoSurfaceDecision {
    pub fn new(
        top_block_state_id: i32,
        filler_block_state_id: i32,
        biome_id: impl Into<String>,
        rendered_rgb: i32,
        recipe_id: impl Into<String>,
        stage_id: impl Into<String>,
        trace: impl Into<String>,
    ) -> Self {
        Self {
            top_block_state_id,
            filler_block_state_id,
            biome_id: normalize_text_default(biome_id.into(), "minecraft:plains"),
            rendered_rgb,
            recipe_id: normalize_text_default(recipe_id.into(), "unknown"),
            stage_id: normalize_text_default(stage_id.into(), "unknown"),
            trace: if photo_surface_trace_enabled() {
                trace.into()
            } else {
                String::new()
            },
        }
    }

    pub fn from_column(column: &EarthSurfaceColumn, stage_id: &str, trace: &str) -> Self {
        Self::new(
            column.top_block_state_id,
            column.filler_block_state_id,
            column.biome_id.clone(),
            render_surface_color(column.top_block_state_id, Some(&column.biome_id)),
            column.decision_source.clone(),
            stage_id.to_string(),
            trace.to_string(),
        )
    }

    pub fn to_column(&self, semantic_column: &EarthSurfaceColumn) -> EarthSurfaceColumn {
        if self.top_block_state_id == semantic_column.top_block_state_id
            && self.filler_block_state_id == semantic_column.filler_block_state_id
        {
            return semantic_column
                .with_biome_id(self.biome_id.clone())
                .with_decision_source(self.recipe_id.clone());
        }
        let mut column = EarthSurfaceColumn::new(
            false,
            semantic_column.ground_surface_y,
            semantic_column.water_surface_y,
            self.top_block_state_id,
            self.filler_block_state_id,
            self.biome_id.clone(),
            self.recipe_id.clone(),
        );
        column.terrain_token_source = semantic_column.terrain_token_source;
        column.data_evidence_flags = semantic_column.data_evidence_flags;
        column
    }
}

pub fn solve_photo_surface(input: &PhotoSurfaceInput) -> Result<PhotoSurfaceDecision> {
    if input.semantic_column.water {
        return Ok(PhotoSurfaceDecision::from_column(
            &input.semantic_column,
            "water",
            "semantic water column preserved",
        ));
    }
    let source = input.sample.color;
    if !source.available || source.is_near_black() {
        return Ok(PhotoSurfaceDecision::from_column(
            &input.semantic_column,
            "no-photo-source",
            "photo source unavailable or near black",
        ));
    }
    if let Some(decision) = solve_authoritative_arid_token_surface(input, source) {
        return Ok(decision);
    }
    if let Some(decision) = solve_java_standard_vegetation_token_surface(input, source) {
        return Ok(decision);
    }
    if let Some(decision) = solve_java_standard_static_token_surface(input, source) {
        return Ok(decision);
    }
    let entry = nearest_photo_palette_entry(source);
    if photo_solver_snow_evidence(input, source) {
        let block_state_id = if entry.block_state_id == block_state_ids::CALCITE {
            block_state_ids::CALCITE
        } else {
            block_state_ids::SNOW_BLOCK
        };
        let biome = if block_state_id == block_state_ids::SNOW_BLOCK {
            "minecraft:snowy_plains".to_string()
        } else {
            photo_solver_biome_for_top(input, block_state_id, source)
        };
        let filler = if block_state_id == input.semantic_column.top_block_state_id {
            input.semantic_column.filler_block_state_id
        } else {
            smoother_filler_for(block_state_id)
        };
        return Ok(PhotoSurfaceDecision::new(
            block_state_id,
            filler,
            biome.clone(),
            render_surface_color(block_state_id, Some(&biome)),
            if block_state_id == entry.block_state_id {
                "photo-texture"
            } else {
                "photo-ecology"
            },
            "source-render-solver",
            photo_surface_trace(format_args!(
                "snow evidence photo palette block {} for rgb #{:02X}{:02X}{:02X}",
                block_state_id, source.red, source.green, source.blue
            )),
        ));
    }
    let block_state_id = java_standard_tan_carrier_top(input, source, entry.block_state_id);
    let biome = photo_solver_biome_for_top(input, block_state_id, source);
    let filler = if block_state_id == input.semantic_column.top_block_state_id {
        input.semantic_column.filler_block_state_id
    } else {
        smoother_filler_for(block_state_id)
    };
    Ok(PhotoSurfaceDecision::new(
        block_state_id,
        filler,
        biome.clone(),
        render_surface_color(block_state_id, Some(&biome)),
        "photo-texture",
        "palette-nearest",
        photo_surface_trace(format_args!(
            "nearest photo palette block {} for rgb #{:02X}{:02X}{:02X}",
            block_state_id, source.red, source.green, source.blue
        )),
    ))
}

pub fn apply_photo_surface_material(input: &PhotoSurfaceInput) -> Result<EarthSurfaceColumn> {
    Ok(solve_photo_surface(input)?.to_column(&input.semantic_column))
}

fn photo_solver_biome(
    semantic_column: &EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
) -> String {
    if !sample.ecoregion_biome_id.trim().is_empty() && sample.ecoregion_confidence >= 0.50 {
        return sample.ecoregion_biome_id.clone();
    }
    semantic_column.biome_id.clone()
}

fn photo_solver_biome_for_top(input: &PhotoSurfaceInput, top: i32, source: RgbColor) -> String {
    if is_tinted_vegetation_block(top) {
        return photo_solver_tinted_vegetation_biome(input, top, source);
    }
    compatible_biome_for_palette_non_grass(&input.semantic_column.biome_id, top)
}

fn photo_solver_tinted_vegetation_biome(
    input: &PhotoSurfaceInput,
    top: i32,
    source: RgbColor,
) -> String {
    let fallback = if input.semantic_column.biome_id.trim().is_empty() {
        "minecraft:plains"
    } else {
        input.semantic_column.biome_id.as_str()
    };
    photo_solver_grass_biomes(
        fallback,
        &input.sample,
        input.elevation_meters,
        input.latitude,
        input.local_relief_meters,
    )
    .into_iter()
    .filter(|biome| !biome.trim().is_empty())
    .min_by(|left, right| {
        photo_weighted_render_distance(source, top, left)
            .partial_cmp(&photo_weighted_render_distance(source, top, right))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
    .unwrap_or_else(|| fallback.to_string())
}

fn photo_solver_grass_biomes(
    fallback: &str,
    sample: &SurfaceMaterialSample,
    elevation_meters: f64,
    latitude: f64,
    local_relief_meters: f64,
) -> Vec<String> {
    let base = if fallback.trim().is_empty() {
        "minecraft:plains"
    } else {
        fallback
    };
    let evidence = if sample.ecoregion_biome_id.trim().is_empty() {
        base
    } else {
        sample.ecoregion_biome_id.as_str()
    };
    let preferred = if is_wet_surface_biome(base)
        || is_wet_surface_biome(evidence)
        || sample.swamp_cover_ratio() >= 0.12
    {
        "minecraft:swamp"
    } else if is_photo_solver_forest_biome(base)
        || is_photo_solver_forest_biome(evidence)
        || is_tropical_rain_climate(sample.climate_class)
    {
        if photo_solver_tropical(latitude, sample) {
            "minecraft:jungle"
        } else {
            "minecraft:forest"
        }
    } else if is_dry_vegetation_biome(base)
        || is_dry_vegetation_biome(evidence)
        || is_tropical_savanna_climate(sample.climate_class)
        || is_steppe_climate(sample.climate_class)
    {
        if local_relief_meters >= 100.0 || elevation_meters >= 750.0 {
            "minecraft:windswept_savanna"
        } else {
            "minecraft:savanna"
        }
    } else if latitude.abs() >= 50.0 || elevation_meters >= 1_500.0 {
        "minecraft:taiga"
    } else {
        base
    };
    let mut candidates = Vec::with_capacity(PHOTO_GRASS_RENDER_BIOMES.len() + 2);
    candidates.push(preferred.to_string());
    candidates.push(base.to_string());
    candidates.extend(
        PHOTO_GRASS_RENDER_BIOMES
            .iter()
            .map(|biome| biome.to_string()),
    );
    candidates
}

fn is_photo_solver_forest_biome(biome: &str) -> bool {
    is_forest_surface_biome(biome) || is_jungle_surface_biome(biome)
}

fn photo_solver_tropical(latitude: f64, sample: &SurfaceMaterialSample) -> bool {
    latitude.abs() <= 24.0
        || is_tropical_rain_climate(sample.climate_class)
        || is_tropical_savanna_climate(sample.climate_class)
}

fn solve_authoritative_arid_token_surface(
    input: &PhotoSurfaceInput,
    source: RgbColor,
) -> Option<PhotoSurfaceDecision> {
    if input.sample.terrain_token_source != TerrainTokenSource::JavaStandardPalette {
        return None;
    }
    let token = MetTerrainVocabulary::exact(input.sample.terrain_token_color);
    if !token.confident() {
        return None;
    }
    if !matches!(
        token.kind,
        MetTerrainKind::Sand | MetTerrainKind::RedSand | MetTerrainKind::Snow
    ) {
        return None;
    }
    let snow_evidence = photo_solver_snow_evidence(input, source);
    if token.kind == MetTerrainKind::Snow && snow_evidence {
        return Some(photo_surface_decision_for_top(
            input,
            token.top_block_state_id,
            "photo-palette",
            "palette-token-solver",
            source,
            "authoritative snow terrain token",
        ));
    }
    if input.sample.terrain_token_source == TerrainTokenSource::JavaStandardPalette
        && photo_solver_clear_green_source(source)
    {
        return None;
    }
    if token.kind == MetTerrainKind::Snow && !photo_solver_dry_context(input, source) {
        return None;
    }
    let top = ordered_arid_token_top(input, source, token);
    Some(photo_surface_decision_for_top(
        input,
        top,
        "photo-palette",
        "palette-token-solver",
        source,
        "authoritative arid terrain token",
    ))
}

fn solve_java_standard_vegetation_token_surface(
    input: &PhotoSurfaceInput,
    source: RgbColor,
) -> Option<PhotoSurfaceDecision> {
    if input.sample.terrain_token_source != TerrainTokenSource::JavaStandardPalette {
        return None;
    }
    let token = MetTerrainVocabulary::exact(input.sample.terrain_token_color);
    if !token.confident() || !matches!(token.kind, MetTerrainKind::Vegetated | MetTerrainKind::Wet)
    {
        return None;
    }
    if photo_solver_dark_standard_shadow(input) {
        let top = nearest_photo_static_carrier(
            source,
            &[
                block_state_ids::BLACK_TERRACOTTA,
                block_state_ids::GRAY_TERRACOTTA,
                block_state_ids::DEEPSLATE,
            ],
        )
        .unwrap_or(block_state_ids::BLACK_TERRACOTTA);
        return Some(photo_surface_decision_for_top(
            input,
            top,
            "photo-palette",
            "palette-token-solver",
            source,
            "java standard dark vegetation token",
        ));
    }
    if token.kind == MetTerrainKind::Vegetated
        && token.top_block_state_id == block_state_ids::GRASS_BLOCK
        && photo_solver_gray_olive_standard_target(source)
    {
        let baseline = nearest_photo_palette_entry(source);
        if !is_tinted_vegetation_block(baseline.block_state_id) {
            return None;
        }
        let biome = photo_solver_biome_for_top(input, baseline.block_state_id, source);
        let tinted_distance =
            photo_weighted_render_distance(source, baseline.block_state_id, &biome);
        let top = nearest_photo_static_carrier(source, STATIC_CARRIER_BLOCKS)
            .unwrap_or(block_state_ids::ANDESITE);
        if is_tinted_vegetation_block(top) {
            return None;
        }
        let carrier_distance = photo_weighted_render_distance(source, top, &biome);
        if carrier_distance
            + photo_solver_gray_olive_carrier_margin(
                source,
                input.global_block_x,
                input.global_block_z,
            )
            >= tinted_distance
        {
            return None;
        }
        return Some(photo_surface_decision_for_top(
            input,
            top,
            "photo-palette",
            "palette-token-solver",
            source,
            "java standard gray olive static carrier",
        ));
    }
    let solve = java_standard_vegetation_palette_solve(input, source, token)?;
    Some(photo_surface_decision_from_solve(
        input,
        solve,
        "photo-palette",
        "palette-token-solver",
        source,
        "authoritative java standard vegetation token",
    ))
}

fn solve_java_standard_static_token_surface(
    input: &PhotoSurfaceInput,
    source: RgbColor,
) -> Option<PhotoSurfaceDecision> {
    if input.sample.terrain_token_source != TerrainTokenSource::JavaStandardPalette {
        return None;
    }
    let token = MetTerrainVocabulary::exact(input.sample.terrain_token_color);
    if !token.confident()
        || !matches!(
            token.kind,
            MetTerrainKind::CoarseDirt | MetTerrainKind::Gravel | MetTerrainKind::Rock
        )
    {
        return None;
    }
    let source_metrics = SurfaceColorMetrics::from(source);
    if photo_solver_snow_evidence(input, source)
        || photo_solver_source_looks_clearly_green(source_metrics)
    {
        return None;
    }
    let target = photo_blend(
        source,
        input.sample.terrain_token_color,
        java_standard_render_anchor_weight(source, input.sample.terrain_token_color, token),
    );
    let blocks = match token.kind {
        MetTerrainKind::CoarseDirt => &PALETTE_COARSE_DIRT_CANDIDATES[..],
        MetTerrainKind::Gravel | MetTerrainKind::Rock => &PALETTE_ROCK_CANDIDATES[..],
        _ => return None,
    };
    let mut best: Option<PhotoSurfaceSolve> = None;
    for &top in blocks {
        if top == block_state_ids::SNOW_BLOCK {
            continue;
        }
        let biome = compatible_biome_for_palette_non_grass(&input.semantic_column.biome_id, top);
        let candidate = java_standard_palette_candidate(input, source, target, token, top, biome);
        if best
            .as_ref()
            .is_none_or(|current| candidate.score < current.score)
        {
            best = Some(candidate);
        }
    }
    Some(photo_surface_decision_from_solve(
        input,
        best?,
        "photo-palette",
        "palette-token-solver",
        source,
        "authoritative java standard static token",
    ))
}

#[derive(Clone, Debug)]
struct PhotoSurfaceSolve {
    top_block_state_id: i32,
    biome_id: String,
    score: f64,
}

#[derive(Clone, Copy, Debug)]
struct PhotoRenderMetrics {
    lab: [f64; 3],
    luma: f64,
}

impl PhotoRenderMetrics {
    fn new(render_rgb: i32) -> Self {
        Self {
            lab: photo_rgb_i32_to_lab(render_rgb),
            luma: photo_luma_rgb(render_rgb),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct JavaStandardPaletteCandidateContext {
    target_lab: [f64; 3],
    source_lab: [f64; 3],
    source_luma: f64,
    snow_context: bool,
    dark_standard_shadow: bool,
}

impl JavaStandardPaletteCandidateContext {
    fn new(input: &PhotoSurfaceInput, source: RgbColor, target: RgbColor) -> Self {
        Self {
            target_lab: photo_rgb_color_to_lab(target),
            source_lab: photo_rgb_color_to_lab(source),
            source_luma: photo_luma(source),
            snow_context: photo_solver_snow_evidence(input, source),
            dark_standard_shadow: photo_solver_dark_standard_shadow(input),
        }
    }
}

fn java_standard_vegetation_palette_solve(
    input: &PhotoSurfaceInput,
    source: RgbColor,
    token: MetTerrainMatch,
) -> Option<PhotoSurfaceSolve> {
    let token_color = input.sample.terrain_token_color;
    if !token_color.available || !source.available || source.is_near_black() {
        return None;
    }
    let target = photo_blend(
        source,
        token_color,
        java_standard_render_anchor_weight(source, token_color, token),
    );
    let candidate_context = JavaStandardPaletteCandidateContext::new(input, source, target);
    let grass_biomes = photo_solver_grass_biomes(
        &input.semantic_column.biome_id,
        &input.sample,
        input.elevation_meters,
        input.latitude,
        input.local_relief_meters,
    );
    let mut best: Option<PhotoSurfaceSolve> = None;
    for &top in PHOTO_SOLVER_CANDIDATE_BLOCKS {
        if is_tinted_vegetation_block(top) {
            for biome in &grass_biomes {
                if biome.trim().is_empty() {
                    continue;
                }
                let candidate = java_standard_palette_candidate_with_context(
                    input,
                    candidate_context,
                    token,
                    top,
                    biome.clone(),
                );
                if best
                    .as_ref()
                    .is_none_or(|current| candidate.score < current.score)
                {
                    best = Some(candidate);
                }
            }
        } else {
            let biome =
                compatible_biome_for_palette_non_grass(&input.semantic_column.biome_id, top);
            let candidate = java_standard_palette_candidate_with_context(
                input,
                candidate_context,
                token,
                top,
                biome,
            );
            if best
                .as_ref()
                .is_none_or(|current| candidate.score < current.score)
            {
                best = Some(candidate);
            }
        }
    }
    let baseline = best?;
    direct_java_standard_vegetation_recipe_solve(input, source, token, &baseline)
        .or_else(|| preferred_java_standard_carrier_solve(input, source, token, &baseline))
        .or(Some(baseline))
}

fn java_standard_palette_candidate(
    input: &PhotoSurfaceInput,
    source: RgbColor,
    target: RgbColor,
    token: MetTerrainMatch,
    top: i32,
    biome: String,
) -> PhotoSurfaceSolve {
    let context = JavaStandardPaletteCandidateContext::new(input, source, target);
    java_standard_palette_candidate_with_context(input, context, token, top, biome)
}

fn java_standard_palette_candidate_with_context(
    input: &PhotoSurfaceInput,
    context: JavaStandardPaletteCandidateContext,
    token: MetTerrainMatch,
    top: i32,
    biome: String,
) -> PhotoSurfaceSolve {
    let render_metrics = photo_render_metrics_for_surface(top, &biome);
    let base_score = java_standard_palette_candidate_base_score(context, render_metrics);
    let score =
        base_score + java_standard_palette_candidate_bias_with_context(input, context, token, top);
    PhotoSurfaceSolve {
        top_block_state_id: top,
        biome_id: biome,
        score,
    }
}

fn java_standard_palette_candidate_base_score(
    context: JavaStandardPaletteCandidateContext,
    render_metrics: PhotoRenderMetrics,
) -> f64 {
    photo_ciede2000(context.target_lab, render_metrics.lab)
        + java_standard_source_render_preservation_score_with_context(context, render_metrics)
}

fn photo_surface_decision_from_solve(
    input: &PhotoSurfaceInput,
    solve: PhotoSurfaceSolve,
    recipe_id: &str,
    stage_id: &str,
    source: RgbColor,
    reason: &str,
) -> PhotoSurfaceDecision {
    let filler = if solve.top_block_state_id == input.semantic_column.top_block_state_id {
        input.semantic_column.filler_block_state_id
    } else {
        smoother_filler_for(solve.top_block_state_id)
    };
    PhotoSurfaceDecision::new(
        solve.top_block_state_id,
        filler,
        solve.biome_id.clone(),
        render_surface_color(solve.top_block_state_id, Some(&solve.biome_id)),
        recipe_id,
        stage_id,
        photo_surface_trace(format_args!(
            "sourceRgb={},{},{};{};top={};biome={}",
            source.red, source.green, source.blue, reason, solve.top_block_state_id, solve.biome_id
        )),
    )
}

fn photo_surface_decision_for_top(
    input: &PhotoSurfaceInput,
    top: i32,
    recipe_id: &str,
    stage_id: &str,
    source: RgbColor,
    reason: &str,
) -> PhotoSurfaceDecision {
    let biome = photo_solver_biome_for_top(input, top, source);
    let filler = if top == input.semantic_column.top_block_state_id {
        input.semantic_column.filler_block_state_id
    } else {
        smoother_filler_for(top)
    };
    PhotoSurfaceDecision::new(
        top,
        filler,
        biome.clone(),
        render_surface_color(top, Some(&biome)),
        recipe_id,
        stage_id,
        photo_surface_trace(format_args!(
            "sourceRgb={},{},{};{};top={};biome={}",
            source.red, source.green, source.blue, reason, top, biome
        )),
    )
}

fn ordered_arid_token_top(
    input: &PhotoSurfaceInput,
    source: RgbColor,
    token: MetTerrainMatch,
) -> i32 {
    let candidates = if input.sample.terrain_token_source == TerrainTokenSource::JavaStandardPalette
        && matches!(token.kind, MetTerrainKind::Sand | MetTerrainKind::RedSand)
        && photo_solver_gray_rock_source_for_sand_token(input, source)
    {
        &PALETTE_ROCK_CANDIDATES[..]
    } else if token.kind == MetTerrainKind::RedSand {
        &PALETTE_RED_SAND_CANDIDATES[..]
    } else {
        &PALETTE_SAND_CANDIDATES[..]
    };
    let biome = photo_solver_biome(&input.semantic_column, &input.sample);
    let mut scored = candidates
        .iter()
        .copied()
        .filter(|&top| top != block_state_ids::SNOW_BLOCK)
        .filter(|&top| !is_capped_bright_arid_carrier(top) || photo_color_value(source) >= 225)
        .map(|top| {
            (
                top,
                photo_weighted_render_distance(source, top, &biome)
                    + arid_token_candidate_bias(source, top, input),
            )
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    let first = scored
        .first()
        .map(|&(top, _)| top)
        .unwrap_or(block_state_ids::SANDSTONE);
    let second = scored
        .iter()
        .map(|&(top, _)| top)
        .find(|&top| top != first && same_surface_family(top, first))
        .or_else(|| scored.get(1).map(|&(top, _)| top))
        .unwrap_or(first);
    if second == first {
        return first;
    }
    let right_slots = cross_crop_arid_right_slots(input, source, token, first, second, &biome);
    if photo_dither_threshold(input.global_block_x, input.global_block_z, first, second)
        < right_slots
    {
        second
    } else {
        first
    }
}

fn cross_crop_arid_right_slots(
    input: &PhotoSurfaceInput,
    source: RgbColor,
    token: MetTerrainMatch,
    first: i32,
    second: i32,
    biome: &str,
) -> i32 {
    const ORDERED_DITHER_SLOTS: i32 = 16;
    const DEFAULT_RIGHT_SLOTS: i32 = 5;
    if input.sample.terrain_token_source != TerrainTokenSource::JavaStandardPalette
        || !source_ranked_cross_crop_token(token)
    {
        return DEFAULT_RIGHT_SLOTS;
    }
    let source_rank = learned_cross_crop_source_luma_rank(input, source, token);
    let mut slot_nudge = ((source_rank - 0.5) * 10.0).round() as i32;
    let left_luma = photo_luma_rgb(render_surface_color(first, Some(biome)));
    let right_luma = photo_luma_rgb(render_surface_color(second, Some(biome)));
    if right_luma < left_luma {
        slot_nudge = -slot_nudge;
    }
    (DEFAULT_RIGHT_SLOTS + slot_nudge).clamp(1, ORDERED_DITHER_SLOTS - 1)
}

fn source_ranked_cross_crop_token(token: MetTerrainMatch) -> bool {
    matches!(
        met_terrain_match_rgb(token),
        0x003200 | 0x323C1E | 0x958667 | 0x9BA06E | 0xE6CDA0 | 0xFFC840 | 0xFFC880 | 0xFFFFBE
    )
}

fn learned_cross_crop_source_luma_rank(
    input: &PhotoSurfaceInput,
    source: RgbColor,
    token: MetTerrainMatch,
) -> f64 {
    if let Some(profile) = input.token_luma_profile.as_deref() {
        if let Some(rank) = profile.normalized_luma(token, source) {
            return rank;
        }
    }
    if let Some((min_luma, max_luma)) = learned_cross_crop_source_luma_range(token) {
        return ((photo_luma(source) - min_luma) / (max_luma - min_luma)).clamp(0.0, 1.0);
    }
    token_source_luma_rank(source, token.color())
}

fn learned_cross_crop_source_luma_range(token: MetTerrainMatch) -> Option<(f64, f64)> {
    match met_terrain_match_rgb(token) {
        0x003200 => Some((25.30, 54.46)),
        0x323C1E => Some((35.35, 84.47)),
        0x958667 => Some((123.38, 144.88)),
        0x9BA06E => Some((147.24, 184.72)),
        0xE6CDA0 => Some((181.89, 234.50)),
        0xFFC840 => Some((195.02, 208.90)),
        0xFFC880 => Some((173.70, 221.04)),
        0xFFFFBE => Some((220.64, 243.17)),
        _ => None,
    }
}

fn token_source_luma_rank(source: RgbColor, standard: RgbColor) -> f64 {
    let standard_luma = photo_luma(standard);
    let source_luma = photo_luma(source);
    let (lower, upper) = if standard_luma >= 190.0 {
        ((standard_luma - 86.0).max(0.0), 255.0)
    } else if standard.green >= standard.red && standard.green >= standard.blue {
        (
            (standard_luma - 70.0).max(0.0),
            (standard_luma + 92.0).min(255.0),
        )
    } else {
        (
            (standard_luma - 62.0).max(0.0),
            (standard_luma + 74.0).min(255.0),
        )
    };
    if upper <= lower + 1.0 {
        0.5
    } else {
        ((source_luma - lower) / (upper - lower)).clamp(0.0, 1.0)
    }
}

fn met_terrain_match_rgb(token: MetTerrainMatch) -> i32 {
    (i32::from(token.red) << 16) | (i32::from(token.green) << 8) | i32::from(token.blue)
}

fn java_standard_tan_carrier_top(
    input: &PhotoSurfaceInput,
    source: RgbColor,
    palette_top: i32,
) -> i32 {
    if input.sample.terrain_token_source != TerrainTokenSource::JavaStandardPalette {
        return palette_top;
    }
    if palette_top != block_state_ids::SAND || !photo_solver_dry_context(input, source) {
        return palette_top;
    }
    if input.coast_factor < 0.70 || photo_color_value(source) < 190 {
        return palette_top;
    }
    let candidates = [
        block_state_ids::SANDSTONE,
        block_state_ids::SMOOTH_SANDSTONE,
        block_state_ids::CUT_SANDSTONE,
        block_state_ids::END_STONE,
        block_state_ids::BONE_BLOCK,
        block_state_ids::CALCITE,
        block_state_ids::WHITE_TERRACOTTA,
    ];
    let biome = photo_solver_biome(&input.semantic_column, &input.sample);
    candidates
        .iter()
        .copied()
        .min_by(|&left, &right| {
            photo_weighted_render_distance(source, left, &biome)
                .partial_cmp(&photo_weighted_render_distance(source, right, &biome))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.cmp(&right))
        })
        .unwrap_or(block_state_ids::SANDSTONE)
}

fn photo_solver_dry_context(input: &PhotoSurfaceInput, source: RgbColor) -> bool {
    let biome = input.semantic_column.biome_id.to_ascii_lowercase();
    let eco_biome = input.sample.ecoregion_biome_id.to_ascii_lowercase();
    let eco_name = input.sample.ecoregion_name.to_ascii_lowercase();
    biome.contains("desert")
        || biome.contains("savanna")
        || biome.contains("badlands")
        || eco_biome.contains("desert")
        || eco_biome.contains("savanna")
        || eco_name.contains("desert")
        || eco_name.contains("savanna")
        || eco_name.contains("sahel")
        || photo_color_saturation(source) >= 0.16
            && (28.0..=66.0).contains(&photo_color_hue_degrees(source))
}

fn photo_solver_clear_green_source(source: RgbColor) -> bool {
    let value = f64::from(photo_color_value(source)) / 255.0;
    let saturation = photo_color_saturation(source);
    value >= 0.18
        && saturation >= 0.22
        && (photo_solver_green_like(source) || photo_solver_olive_vegetation_like(source))
}

fn photo_solver_green_like(source: RgbColor) -> bool {
    let red = f64::from(source.red) / 255.0;
    let green = f64::from(source.green) / 255.0;
    let blue = f64::from(source.blue) / 255.0;
    let hue = photo_color_hue_degrees(source);
    (58.0..=170.0).contains(&hue) && green >= red * 0.88 && green >= blue * 1.02
}

fn photo_solver_olive_vegetation_like(source: RgbColor) -> bool {
    let red = f64::from(source.red) / 255.0;
    let green = f64::from(source.green) / 255.0;
    let blue = f64::from(source.blue) / 255.0;
    let hue = photo_color_hue_degrees(source);
    let value = f64::from(photo_color_value(source)) / 255.0;
    (42.0..=105.0).contains(&hue)
        && photo_color_saturation(source) >= 0.12
        && (0.24..=0.64).contains(&value)
        && green >= red * 0.72
        && green >= blue * 0.82
}

fn photo_solver_gray_rock_source_for_sand_token(
    input: &PhotoSurfaceInput,
    source: RgbColor,
) -> bool {
    let value = f64::from(photo_color_value(source)) / 255.0;
    photo_color_saturation(source) <= 0.16
        && (0.42..=0.72).contains(&value)
        && (input.local_relief_meters >= 80.0 || input.elevation_meters >= 700.0)
}

fn photo_solver_dark_standard_shadow(input: &PhotoSurfaceInput) -> bool {
    if input.sample.terrain_token_source != TerrainTokenSource::JavaStandardPalette
        || !input.sample.terrain_token_color.available
        || !input.sample.color.available
    {
        return false;
    }
    let token = input.sample.terrain_token_color;
    let source = input.sample.color;
    token.red <= 24
        && token.green <= 24
        && token.blue <= 24
        && photo_luma(source) <= 46.0
        && photo_luma(token) <= 24.0
}

fn photo_solver_gray_olive_standard_target(color: RgbColor) -> bool {
    color.red >= 120
        && color.red <= 148
        && color.green >= 128
        && color.green <= 156
        && color.blue >= 65
        && color.blue <= 114
        && (i32::from(color.red) - i32::from(color.green)).abs() <= 18
        && color.green >= color.blue.saturating_add(28)
}

fn nearest_photo_static_carrier(source: RgbColor, candidates: &[i32]) -> Option<i32> {
    let biome = "minecraft:plains";
    candidates.iter().copied().min_by(|&left, &right| {
        photo_weighted_render_distance(source, left, biome)
            .partial_cmp(&photo_weighted_render_distance(source, right, biome))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.cmp(&right))
    })
}

fn photo_solver_gray_olive_carrier_margin(
    source: RgbColor,
    global_block_x: i32,
    global_block_z: i32,
) -> f64 {
    let luma_gap = (photo_luma(source) - 135.0).abs();
    let noise = photo_solver_texture_cell_noise(
        global_block_x,
        global_block_z,
        32,
        block_state_ids::GRASS_BLOCK,
        block_state_ids::ANDESITE,
    );
    0.35 + 1.10_f64.min(luma_gap * 0.025) + (noise * 0.25)
}

fn photo_solver_texture_cell_noise(
    global_block_x: i32,
    global_block_z: i32,
    cell_width: i32,
    salt_a: i32,
    salt_b: i32,
) -> f64 {
    let cell_x = global_block_x.div_euclid(cell_width);
    let cell_z = global_block_z.div_euclid(cell_width);
    let tx = f64::from(global_block_x.rem_euclid(cell_width)) / f64::from(cell_width);
    let tz = f64::from(global_block_z.rem_euclid(cell_width)) / f64::from(cell_width);
    let sx = smooth_step(tx);
    let sz = smooth_step(tz);
    let h00 = photo_solver_texture_cell_hash(cell_x, cell_z, salt_a, salt_b);
    let h10 = photo_solver_texture_cell_hash(cell_x.wrapping_add(1), cell_z, salt_a, salt_b);
    let h01 = photo_solver_texture_cell_hash(cell_x, cell_z.wrapping_add(1), salt_a, salt_b);
    let h11 = photo_solver_texture_cell_hash(
        cell_x.wrapping_add(1),
        cell_z.wrapping_add(1),
        salt_a,
        salt_b,
    );
    lerp(lerp(h00, h10, sx), lerp(h01, h11, sx), sz)
}

fn photo_solver_texture_cell_hash(cell_x: i32, cell_z: i32, salt_a: i32, salt_b: i32) -> f64 {
    fine_photo_solver_hash(
        cell_x ^ salt_a.wrapping_mul(31),
        cell_z ^ salt_b.wrapping_mul(17),
    )
}

fn java_standard_render_anchor_weight(
    source_color: RgbColor,
    token_color: RgbColor,
    token: MetTerrainMatch,
) -> f64 {
    if !source_color.available || !token_color.available {
        return 0.0;
    }
    if photo_rgb_distance_squared(source_color, token_color) > 14_400 {
        return 0.05;
    }
    if is_cross_crop_provisional_token(token) {
        return 0.35;
    }
    0.75
}

fn java_standard_source_render_preservation_score_with_context(
    context: JavaStandardPaletteCandidateContext,
    render_metrics: PhotoRenderMetrics,
) -> f64 {
    let ciede = photo_ciede2000(context.source_lab, render_metrics.lab) * 0.16;
    let luma_gap = (context.source_luma - render_metrics.luma).abs();
    ciede + (luma_gap - 8.0).max(0.0) * 0.045
}

fn java_standard_palette_candidate_bias_with_context(
    input: &PhotoSurfaceInput,
    context: JavaStandardPaletteCandidateContext,
    token: MetTerrainMatch,
    top: i32,
) -> f64 {
    let mut bias = 0.0;
    let snow_context = context.snow_context;
    if !snow_context && top == block_state_ids::SNOW_BLOCK {
        bias += 120.0;
    }
    if token.kind == MetTerrainKind::Snow && !snow_context {
        let token_rgb = rgb(
            i32::from(token.red),
            i32::from(token.green),
            i32::from(token.blue),
        );
        if top == block_state_ids::SNOW_BLOCK {
            bias += 90.0;
        }
        if top == block_state_ids::QUARTZ_BLOCK {
            bias += if token_rgb == 0xFAFFFA { -5.0 } else { 18.0 };
        }
        if matches!(
            top,
            block_state_ids::END_STONE
                | block_state_ids::CALCITE
                | block_state_ids::BONE_BLOCK
                | block_state_ids::SMOOTH_SANDSTONE
                | block_state_ids::CUT_SANDSTONE
                | block_state_ids::CHISELED_SANDSTONE
        ) {
            bias -= 8.0;
        }
    }
    if context.dark_standard_shadow {
        if matches!(
            top,
            block_state_ids::BLACK_TERRACOTTA
                | block_state_ids::DEEPSLATE
                | block_state_ids::GRAY_TERRACOTTA
        ) {
            bias -= 7.5;
        }
        if is_tinted_vegetation_block(top) {
            bias += 18.0;
        }
        if matches!(
            top,
            block_state_ids::MUD | block_state_ids::MOSS_BLOCK | block_state_ids::PODZOL
        ) {
            bias += 5.0;
        }
    }
    if is_photo_solver_coastal_sand_halo_candidate(top, input) {
        bias += 5.5;
    }
    bias
}

fn preferred_java_standard_carrier_solve(
    input: &PhotoSurfaceInput,
    source: RgbColor,
    token: MetTerrainMatch,
    baseline: &PhotoSurfaceSolve,
) -> Option<PhotoSurfaceSolve> {
    if input.sample.terrain_token_source != TerrainTokenSource::JavaStandardPalette
        || !source.available
        || !input.sample.terrain_token_color.available
    {
        return None;
    }
    let (top, biome_override) = preferred_java_standard_carrier(token)?;
    let token_color = token.color();
    let source_metrics = (!source.is_near_black()).then(|| SurfaceColorMetrics::from(source));
    let source_green = source_metrics
        .map(|metrics| {
            photo_solver_green_like_metrics(metrics)
                || photo_solver_olive_vegetation_like_metrics(metrics)
        })
        .unwrap_or(false);
    if photo_rgb_distance_squared(source, token_color) > 14_400
        && !photo_solver_dark_standard_shadow(input)
    {
        return None;
    }
    if source_green && token.kind != MetTerrainKind::Vegetated && !is_tinted_vegetation_block(top) {
        return None;
    }
    let biome = biome_override.map(str::to_string).unwrap_or_else(|| {
        compatible_biome_for_palette_non_grass(&input.semantic_column.biome_id, top)
    });
    let target = photo_blend(
        source,
        input.sample.terrain_token_color,
        java_standard_render_anchor_weight(source, input.sample.terrain_token_color, token),
    );
    let mut preferred = java_standard_palette_candidate(input, source, target, token, top, biome);
    if token_rgb(token) == 0x323C1E
        && preferred.top_block_state_id == block_state_ids::OAK_LEAVES
        && preferred.biome_id == "minecraft:swamp"
        && photo_dither_threshold(
            input.global_block_x,
            input.global_block_z,
            token.top_block_state_id,
            preferred.top_block_state_id,
        ) < 5
    {
        preferred.biome_id = "minecraft:dark_forest".to_string();
        preferred.score = java_standard_palette_candidate(
            input,
            source,
            target,
            token,
            block_state_ids::OAK_LEAVES,
            preferred.biome_id.clone(),
        )
        .score;
    }
    if matches!(token_rgb(token), 0x323C1E | 0x003200) {
        return Some(preferred);
    }
    (preferred.score <= baseline.score + 4.0).then_some(preferred)
}

fn preferred_java_standard_carrier(token: MetTerrainMatch) -> Option<(i32, Option<&'static str>)> {
    match token_rgb(token) {
        0x323C1E => Some((block_state_ids::OAK_LEAVES, Some("minecraft:swamp"))),
        0x003200 => Some((block_state_ids::OAK_LEAVES, Some("minecraft:dark_forest"))),
        0xA4875B => Some((block_state_ids::YELLOW_TERRACOTTA, None)),
        0x9B7F67 => Some((block_state_ids::PACKED_MUD, None)),
        0x141414 | 0x000000 => Some((block_state_ids::BLACK_TERRACOTTA, None)),
        0xBE9678 => Some((block_state_ids::WHITE_TERRACOTTA, None)),
        0xE6CDA0 => Some((block_state_ids::SMOOTH_SANDSTONE, None)),
        0xFFFFBE => Some((block_state_ids::END_STONE, None)),
        0xFAFFFA => Some((block_state_ids::QUARTZ_BLOCK, None)),
        0xFFC880 => Some((block_state_ids::CHISELED_SANDSTONE, None)),
        0x5F644B => Some((block_state_ids::OAK_LEAVES, Some("minecraft:savanna"))),
        0x64553C | 0x645032 => Some((block_state_ids::COARSE_DIRT, None)),
        0xA79267 => Some((block_state_ids::GRASS_BLOCK, Some("minecraft:savanna"))),
        0x4B553C => Some((block_state_ids::GREEN_TERRACOTTA, None)),
        0xAA693C => Some((block_state_ids::SMOOTH_RED_SANDSTONE, None)),
        0x374632 => Some((block_state_ids::SPRUCE_LEAVES, Some("minecraft:taiga"))),
        0x645A4B => Some((block_state_ids::MYCELIUM, None)),
        0x8C5032 => Some((block_state_ids::CUT_RED_SANDSTONE, None)),
        0xBE8250 => Some((block_state_ids::SMOOTH_RED_SANDSTONE, None)),
        _ => None,
    }
}

fn direct_java_standard_vegetation_recipe_solve(
    input: &PhotoSurfaceInput,
    source: RgbColor,
    token: MetTerrainMatch,
    baseline: &PhotoSurfaceSolve,
) -> Option<PhotoSurfaceSolve> {
    if !matches!(token.kind, MetTerrainKind::Vegetated | MetTerrainKind::Wet)
        || !source.available
        || source.is_near_black()
        || !input.sample.terrain_token_color.available
        || photo_solver_snow_evidence(input, source)
        || photo_solver_gray_olive_standard_target(source)
    {
        return None;
    }
    let source_metrics = SurfaceColorMetrics::from(source);
    if !photo_solver_source_looks_clearly_green(source_metrics)
        && !photo_solver_dark_standard_shadow(input)
    {
        return None;
    }
    let target =
        if source_metrics.saturation <= 0.16 && (0.35..=0.72).contains(&source_metrics.value) {
            source
        } else {
            photo_blend(source, input.sample.terrain_token_color, 0.30)
        };
    if token_rgb(token) == 0xA79267
        && photo_luma(source) + 12.0 < photo_luma(input.sample.terrain_token_color)
    {
        return java_standard_dry_grass_texture_candidate(
            input,
            source,
            input.sample.terrain_token_color,
        );
    }
    let blocks = if token.kind == MetTerrainKind::Wet {
        PALETTE_WET_CANDIDATES
    } else {
        PALETTE_VEGETATED_CANDIDATES
    };
    let target_lab = photo_rgb_color_to_lab(target);
    let grass_biomes = photo_solver_grass_biomes(
        &input.semantic_column.biome_id,
        &input.sample,
        input.elevation_meters,
        input.latitude,
        input.local_relief_meters,
    );
    let mut best: Option<PhotoSurfaceSolve> = None;
    for &top in blocks {
        if top == block_state_ids::SNOW_BLOCK {
            continue;
        }
        if is_tinted_vegetation_block(top) {
            for biome in &grass_biomes {
                if biome.trim().is_empty() {
                    continue;
                }
                let candidate =
                    token_recipe_candidate_with_target_lab(target_lab, top, biome.clone());
                if best
                    .as_ref()
                    .is_none_or(|current| candidate.score < current.score)
                {
                    best = Some(candidate);
                }
            }
        } else {
            let biome =
                compatible_biome_for_palette_non_grass(&input.semantic_column.biome_id, top);
            let candidate = token_recipe_candidate_with_target_lab(target_lab, top, biome);
            if best
                .as_ref()
                .is_none_or(|current| candidate.score < current.score)
            {
                best = Some(candidate);
            }
        }
    }
    let candidate = best?;
    accept_direct_java_standard_remap(
        baseline,
        &candidate,
        source,
        input.sample.terrain_token_color,
        token,
    )
    .then_some(candidate)
}

fn java_standard_dry_grass_texture_candidate(
    input: &PhotoSurfaceInput,
    source: RgbColor,
    standard: RgbColor,
) -> Option<PhotoSurfaceSolve> {
    DRY_GRASS_TEXTURE_CANDIDATES
        .iter()
        .copied()
        .map(|top| {
            let biome =
                compatible_biome_for_palette_non_grass(&input.semantic_column.biome_id, top);
            let render_rgb = render_surface_color(top, Some(&biome));
            PhotoSurfaceSolve {
                top_block_state_id: top,
                biome_id: biome,
                score: source_primary_score(
                    render_rgb,
                    rgb(
                        i32::from(source.red),
                        i32::from(source.green),
                        i32::from(source.blue),
                    ),
                    rgb(
                        i32::from(standard.red),
                        i32::from(standard.green),
                        i32::from(standard.blue),
                    ),
                ),
            }
        })
        .min_by(|left, right| {
            left.score
                .partial_cmp(&right.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.top_block_state_id.cmp(&right.top_block_state_id))
        })
}

fn token_recipe_candidate_with_target_lab(
    target_lab: [f64; 3],
    top: i32,
    biome: String,
) -> PhotoSurfaceSolve {
    let render_metrics = photo_render_metrics_for_surface(top, &biome);
    PhotoSurfaceSolve {
        top_block_state_id: top,
        biome_id: biome,
        score: photo_ciede2000(target_lab, render_metrics.lab),
    }
}

fn photo_render_metrics_for_surface(top: i32, biome: &str) -> PhotoRenderMetrics {
    let render_rgb = render_surface_color(top, Some(biome));
    photo_render_metrics_for_rgb(render_rgb)
}

fn photo_render_metrics_for_rgb(render_rgb: i32) -> PhotoRenderMetrics {
    precomputed_photo_render_metrics_by_rgb()
        .get(&render_rgb)
        .copied()
        .unwrap_or_else(|| PhotoRenderMetrics::new(render_rgb))
}

fn precomputed_photo_render_metrics_by_rgb() -> &'static HashMap<i32, PhotoRenderMetrics> {
    static METRICS: OnceLock<HashMap<i32, PhotoRenderMetrics>> = OnceLock::new();
    METRICS.get_or_init(|| {
        let mut metrics = HashMap::new();
        for candidates in [
            PHOTO_SOLVER_CANDIDATE_BLOCKS,
            PALETTE_VEGETATED_CANDIDATES,
            PALETTE_WET_CANDIDATES,
            &PALETTE_COARSE_DIRT_CANDIDATES,
            &PALETTE_ROCK_CANDIDATES,
            &PALETTE_SAND_CANDIDATES,
            &PALETTE_RED_SAND_CANDIDATES,
            DRY_GRASS_TEXTURE_CANDIDATES,
            STATIC_CARRIER_BLOCKS,
        ] {
            for &top in candidates {
                let render_rgb = render_surface_color(top, None);
                metrics
                    .entry(render_rgb)
                    .or_insert_with(|| PhotoRenderMetrics::new(render_rgb));
                if is_tinted_vegetation_block(top) {
                    for biome in PHOTO_GRASS_RENDER_BIOMES {
                        let render_rgb = render_surface_color(top, Some(biome));
                        metrics
                            .entry(render_rgb)
                            .or_insert_with(|| PhotoRenderMetrics::new(render_rgb));
                    }
                }
            }
        }
        metrics
    })
}

fn accept_direct_java_standard_remap(
    baseline: &PhotoSurfaceSolve,
    candidate: &PhotoSurfaceSolve,
    source: RgbColor,
    standard: RgbColor,
    token: MetTerrainMatch,
) -> bool {
    if same_photo_surface_solve(baseline, candidate) {
        return false;
    }
    let source_rgb = rgb(
        i32::from(source.red),
        i32::from(source.green),
        i32::from(source.blue),
    );
    let standard_rgb = rgb(
        i32::from(standard.red),
        i32::from(standard.green),
        i32::from(standard.blue),
    );
    let baseline_rgb = render_surface_color(baseline.top_block_state_id, Some(&baseline.biome_id));
    let candidate_rgb =
        render_surface_color(candidate.top_block_state_id, Some(&candidate.biome_id));
    let baseline_score = source_primary_score(baseline_rgb, source_rgb, standard_rgb);
    let candidate_score = source_primary_score(candidate_rgb, source_rgb, standard_rgb);
    let primary_gain = baseline_score - candidate_score;
    let standard_regression = photo_ciede2000_rgb_rgb(candidate_rgb, standard_rgb)
        - photo_ciede2000_rgb_rgb(baseline_rgb, standard_rgb);
    if standard_regression <= 1.15 && primary_gain >= 0.12 {
        return true;
    }
    if matches!(token.kind, MetTerrainKind::Sand | MetTerrainKind::RedSand)
        && standard_regression <= 2.35
        && primary_gain >= 0.95
    {
        return true;
    }
    standard_regression <= 3.50 && primary_gain >= 1.80
}

fn same_photo_surface_solve(left: &PhotoSurfaceSolve, right: &PhotoSurfaceSolve) -> bool {
    left.top_block_state_id == right.top_block_state_id && left.biome_id == right.biome_id
}

fn source_primary_score(actual_rgb: i32, source_rgb: i32, standard_rgb: i32) -> f64 {
    let source_distance = photo_ciede2000_rgb_rgb(actual_rgb, source_rgb);
    let standard_distance = photo_ciede2000_rgb_rgb(actual_rgb, standard_rgb);
    let luma_penalty = (photo_luma_rgb(actual_rgb) - photo_luma_rgb(source_rgb)).abs() * 0.01;
    (source_distance * 0.70) + (standard_distance * 0.30) + luma_penalty
}

fn photo_solver_source_looks_clearly_green(metrics: SurfaceColorMetrics) -> bool {
    (photo_solver_green_like_metrics(metrics)
        || photo_solver_olive_vegetation_like_metrics(metrics))
        && metrics.value >= 0.18
        && metrics.saturation >= 0.22
}

fn photo_solver_green_like_metrics(metrics: SurfaceColorMetrics) -> bool {
    (58.0..=170.0).contains(&metrics.hue)
        && metrics.green >= metrics.red * 0.88
        && metrics.green >= metrics.blue * 1.02
}

fn photo_solver_olive_vegetation_like_metrics(metrics: SurfaceColorMetrics) -> bool {
    (42.0..=105.0).contains(&metrics.hue)
        && metrics.saturation >= 0.12
        && (0.24..=0.64).contains(&metrics.value)
        && metrics.green >= metrics.red * 0.72
        && metrics.green >= metrics.blue * 0.82
}

fn compatible_biome_for_palette_non_grass(biome: &str, top: i32) -> String {
    let fallback = if biome.trim().is_empty() {
        "minecraft:plains"
    } else {
        biome
    };
    if top == block_state_ids::SNOW_BLOCK {
        return "minecraft:snowy_plains".to_string();
    }
    if top == block_state_ids::SAND || is_photo_solver_pale_sand_carrier(top) {
        return if fallback.contains("beach") {
            "minecraft:beach".to_string()
        } else {
            fallback.to_string()
        };
    }
    if top == block_state_ids::RED_SAND || is_photo_solver_red_sandstone_carrier(top) {
        return if fallback.contains("badlands") {
            fallback.to_string()
        } else {
            "minecraft:badlands".to_string()
        };
    }
    fallback.to_string()
}

fn is_photo_solver_coastal_sand_halo_candidate(top: i32, input: &PhotoSurfaceInput) -> bool {
    is_photo_solver_sand_like_carrier(top)
        && input.coast_factor >= 0.70
        && !is_photo_solver_naturally_sandy_biome(&input.semantic_column.biome_id)
}

fn is_photo_solver_naturally_sandy_biome(biome: &str) -> bool {
    let lower = biome.to_ascii_lowercase();
    lower.contains("desert") || lower.contains("badlands")
}

fn is_photo_solver_sand_like_carrier(top: i32) -> bool {
    matches!(
        top,
        block_state_ids::SAND
            | block_state_ids::RED_SAND
            | block_state_ids::SANDSTONE
            | block_state_ids::SMOOTH_SANDSTONE
            | block_state_ids::CUT_SANDSTONE
            | block_state_ids::CHISELED_SANDSTONE
            | block_state_ids::SMOOTH_RED_SANDSTONE
            | block_state_ids::CUT_RED_SANDSTONE
            | block_state_ids::CHISELED_RED_SANDSTONE
    )
}

fn is_photo_solver_pale_sand_carrier(top: i32) -> bool {
    matches!(
        top,
        block_state_ids::SANDSTONE
            | block_state_ids::END_STONE
            | block_state_ids::END_STONE_BRICKS
            | block_state_ids::SMOOTH_SANDSTONE
            | block_state_ids::CUT_SANDSTONE
            | block_state_ids::CHISELED_SANDSTONE
    )
}

fn is_photo_solver_red_sandstone_carrier(top: i32) -> bool {
    matches!(
        top,
        block_state_ids::SMOOTH_RED_SANDSTONE
            | block_state_ids::CUT_RED_SANDSTONE
            | block_state_ids::CHISELED_RED_SANDSTONE
    )
}

fn is_cross_crop_provisional_token(token: MetTerrainMatch) -> bool {
    if !token.confident() {
        return false;
    }
    matches!(
        token_rgb(token),
        0x329632
            | 0x648C6E
            | 0xFFC880
            | 0x9BA06E
            | 0xFFFFBE
            | 0x003200
            | 0x958667
            | 0xFFC840
            | 0xE6CDA0
            | 0x323C1E
    )
}

fn token_rgb(token: MetTerrainMatch) -> i32 {
    rgb(
        i32::from(token.red),
        i32::from(token.green),
        i32::from(token.blue),
    )
}

fn photo_blend(source: RgbColor, anchor: RgbColor, anchor_weight: f64) -> RgbColor {
    let t = anchor_weight.clamp(0.0, 1.0);
    RgbColor::of(
        clamp_surface_material_color(java_math_round_double_to_narrowed_i32(
            f64::from(source.red) * (1.0 - t) + f64::from(anchor.red) * t,
        )),
        clamp_surface_material_color(java_math_round_double_to_narrowed_i32(
            f64::from(source.green) * (1.0 - t) + f64::from(anchor.green) * t,
        )),
        clamp_surface_material_color(java_math_round_double_to_narrowed_i32(
            f64::from(source.blue) * (1.0 - t) + f64::from(anchor.blue) * t,
        )),
    )
}

fn photo_rgb_distance_squared(left: RgbColor, right: RgbColor) -> i32 {
    let red = i32::from(left.red) - i32::from(right.red);
    let green = i32::from(left.green) - i32::from(right.green);
    let blue = i32::from(left.blue) - i32::from(right.blue);
    (red * red) + (green * green) + (blue * blue)
}

fn photo_ciede2000_rgb_rgb(left_rgb: i32, right_rgb: i32) -> f64 {
    photo_ciede2000(
        photo_rgb_i32_to_lab(left_rgb),
        photo_rgb_i32_to_lab(right_rgb),
    )
}

fn photo_rgb_color_to_lab(color: RgbColor) -> [f64; 3] {
    photo_rgb_to_lab(
        i32::from(color.red),
        i32::from(color.green),
        i32::from(color.blue),
    )
}

fn photo_rgb_i32_to_lab(color: i32) -> [f64; 3] {
    photo_rgb_to_lab((color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff)
}

fn photo_ciede2000(left: [f64; 3], right: [f64; 3]) -> f64 {
    let left_c = left[1].hypot(left[2]);
    let right_c = right[1].hypot(right[2]);
    let average_c = (left_c + right_c) * 0.5;
    let average_c7 = average_c.powf(7.0);
    let g = 0.5 * (1.0 - (average_c7 / (average_c7 + 25.0_f64.powf(7.0))).sqrt());
    let left_a_prime = (1.0 + g) * left[1];
    let right_a_prime = (1.0 + g) * right[1];
    let left_c_prime = left_a_prime.hypot(left[2]);
    let right_c_prime = right_a_prime.hypot(right[2]);
    let left_h_prime = photo_hue_degrees(left_a_prime, left[2]);
    let right_h_prime = photo_hue_degrees(right_a_prime, right[2]);

    let delta_l_prime = right[0] - left[0];
    let delta_c_prime = right_c_prime - left_c_prime;
    let delta_h_prime =
        photo_delta_hue_prime(left_c_prime, right_c_prime, left_h_prime, right_h_prime);
    let delta_h =
        2.0 * (left_c_prime * right_c_prime).sqrt() * (delta_h_prime * 0.5).to_radians().sin();

    let average_l_prime = (left[0] + right[0]) * 0.5;
    let average_c_prime = (left_c_prime + right_c_prime) * 0.5;
    let average_h_prime =
        photo_average_hue_prime(left_c_prime, right_c_prime, left_h_prime, right_h_prime);
    let t = 1.0 - 0.17 * (average_h_prime - 30.0).to_radians().cos()
        + 0.24 * (2.0 * average_h_prime).to_radians().cos()
        + 0.32 * (3.0 * average_h_prime + 6.0).to_radians().cos()
        - 0.20 * (4.0 * average_h_prime - 63.0).to_radians().cos();
    let delta_theta = 30.0 * (-((average_h_prime - 275.0) / 25.0).powi(2)).exp();
    let average_c_prime7 = average_c_prime.powf(7.0);
    let r_c = 2.0 * (average_c_prime7 / (average_c_prime7 + 25.0_f64.powf(7.0))).sqrt();
    let l_offset = average_l_prime - 50.0;
    let s_l = 1.0 + (0.015 * l_offset * l_offset) / (20.0 + l_offset * l_offset).sqrt();
    let s_c = 1.0 + 0.045 * average_c_prime;
    let s_h = 1.0 + 0.015 * average_c_prime * t;
    let r_t = -(2.0 * delta_theta).to_radians().sin() * r_c;
    let l_term = delta_l_prime / s_l;
    let c_term = delta_c_prime / s_c;
    let h_term = delta_h / s_h;
    (l_term * l_term + c_term * c_term + h_term * h_term + r_t * c_term * h_term).sqrt()
}

fn photo_hue_degrees(a: f64, b: f64) -> f64 {
    if a == 0.0 && b == 0.0 {
        return 0.0;
    }
    let hue = b.atan2(a).to_degrees();
    if hue >= 0.0 {
        hue
    } else {
        hue + 360.0
    }
}

fn photo_delta_hue_prime(
    left_c_prime: f64,
    right_c_prime: f64,
    left_h_prime: f64,
    right_h_prime: f64,
) -> f64 {
    if left_c_prime * right_c_prime == 0.0 {
        return 0.0;
    }
    let delta = right_h_prime - left_h_prime;
    if delta.abs() <= 180.0 {
        return delta;
    }
    if delta > 180.0 {
        delta - 360.0
    } else {
        delta + 360.0
    }
}

fn photo_average_hue_prime(
    left_c_prime: f64,
    right_c_prime: f64,
    left_h_prime: f64,
    right_h_prime: f64,
) -> f64 {
    if left_c_prime * right_c_prime == 0.0 {
        return left_h_prime + right_h_prime;
    }
    let difference = (left_h_prime - right_h_prime).abs();
    if difference <= 180.0 {
        return (left_h_prime + right_h_prime) * 0.5;
    }
    if left_h_prime + right_h_prime < 360.0 {
        (left_h_prime + right_h_prime + 360.0) * 0.5
    } else {
        (left_h_prime + right_h_prime - 360.0) * 0.5
    }
}

fn photo_rgb_to_lab(red: i32, green: i32, blue: i32) -> [f64; 3] {
    let r = photo_pivot_rgb(f64::from(red) / 255.0);
    let g = photo_pivot_rgb(f64::from(green) / 255.0);
    let b = photo_pivot_rgb(f64::from(blue) / 255.0);
    let x = ((r * 0.4124) + (g * 0.3576) + (b * 0.1805)) / 0.95047;
    let y = ((r * 0.2126) + (g * 0.7152) + (b * 0.0722)) / 1.00000;
    let z = ((r * 0.0193) + (g * 0.1192) + (b * 0.9505)) / 1.08883;
    let fx = photo_pivot_xyz(x);
    let fy = photo_pivot_xyz(y);
    let fz = photo_pivot_xyz(z);
    [(116.0 * fy) - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn photo_pivot_rgb(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn photo_pivot_xyz(value: f64) -> f64 {
    if value > 0.008856 {
        value.cbrt()
    } else {
        (7.787 * value) + (16.0 / 116.0)
    }
}

fn fine_photo_solver_hash(x: i32, z: i32) -> f64 {
    let mut hash = 0x9e3779b97f4a7c15_u64;
    hash ^= (x as i64 as u64).wrapping_mul(0xbf58476d1ce4e5b9);
    hash ^= (z as i64 as u64).wrapping_mul(0x94d049bb133111eb);
    hash ^= hash >> 30;
    hash = hash.wrapping_mul(0xbf58476d1ce4e5b9);
    hash ^= hash >> 27;
    hash = hash.wrapping_mul(0x94d049bb133111eb);
    hash ^= hash >> 31;
    ((hash >> 11) as f64) * (1.0 / ((1_u64 << 53) as f64))
}

fn photo_solver_snow_evidence(input: &PhotoSurfaceInput, source: RgbColor) -> bool {
    if input.sample.snow_cover_ratio() >= 0.10 {
        return true;
    }
    let biome = input.semantic_column.biome_id.to_ascii_lowercase();
    if biome.contains("snow") || biome.contains("frozen") {
        return true;
    }
    let value = f64::from(photo_color_value(source)) / 255.0;
    value >= 0.78
        && photo_color_saturation(source) <= 0.22
        && (input.latitude.abs() >= 45.0 || input.elevation_meters >= 2_000.0)
}

fn photo_weighted_render_distance(source: RgbColor, top: i32, biome: &str) -> f64 {
    let color = render_surface_color(top, Some(biome));
    let red = f64::from(source.red) - f64::from((color >> 16) & 0xff);
    let green = f64::from(source.green) - f64::from((color >> 8) & 0xff);
    let blue = f64::from(source.blue) - f64::from(color & 0xff);
    ((red * red * 0.30) + (green * green * 0.45) + (blue * blue * 0.25)).sqrt()
}

fn photo_luma(color: RgbColor) -> f64 {
    (f64::from(color.red) * 0.2126)
        + (f64::from(color.green) * 0.7152)
        + (f64::from(color.blue) * 0.0722)
}

fn photo_luma_rgb(rgb: i32) -> f64 {
    (f64::from((rgb >> 16) & 0xff) * 0.2126)
        + (f64::from((rgb >> 8) & 0xff) * 0.7152)
        + (f64::from(rgb & 0xff) * 0.0722)
}

fn arid_token_candidate_bias(source: RgbColor, top: i32, input: &PhotoSurfaceInput) -> f64 {
    let mut bias = 0.0;
    if top == block_state_ids::SAND && input.coast_factor >= 0.70 {
        bias += 9.0;
    }
    if top == block_state_ids::SAND && input.coast_factor >= 0.985 {
        bias += 12.0;
    }
    if matches!(
        top,
        block_state_ids::BONE_BLOCK
            | block_state_ids::CALCITE
            | block_state_ids::QUARTZ_BLOCK
            | block_state_ids::WHITE_TERRACOTTA
    ) && photo_color_value(source) < 215
    {
        bias += 7.0;
    }
    if matches!(
        top,
        block_state_ids::GRAVEL
            | block_state_ids::TERRACOTTA
            | block_state_ids::PACKED_MUD
            | block_state_ids::YELLOW_TERRACOTTA
    ) && photo_color_value(source) >= 225
    {
        bias += 6.0;
    }
    bias
}

fn is_capped_bright_arid_carrier(top: i32) -> bool {
    matches!(
        top,
        block_state_ids::QUARTZ_BLOCK
            | block_state_ids::CALCITE
            | block_state_ids::BONE_BLOCK
            | block_state_ids::WHITE_TERRACOTTA
    )
}

fn same_surface_family(left: i32, right: i32) -> bool {
    photo_texture_family(left) == photo_texture_family(right)
}

fn photo_dither_threshold(global_block_x: i32, global_block_z: i32, left: i32, right: i32) -> i32 {
    let salt_a = left.wrapping_mul(37).wrapping_add(right.wrapping_mul(17));
    let salt_b = right.wrapping_mul(41).wrapping_add(left.wrapping_mul(13));
    let patch = photo_solver_texture_cell_noise(global_block_x, global_block_z, 5, salt_a, salt_b);
    let macro_patch =
        photo_solver_texture_cell_noise(global_block_x, global_block_z, 17, salt_b, salt_a);
    let grain = fine_photo_solver_hash(
        global_block_x.div_euclid(3) ^ salt_a,
        global_block_z.div_euclid(3) ^ salt_b,
    );
    let jitter = fine_photo_solver_hash(global_block_x ^ salt_a, global_block_z ^ salt_b);
    let value = ((patch * 0.42) + (macro_patch * 0.16) + (grain * 0.24) + (jitter * 0.18) - 0.08)
        .clamp(0.0, 0.999_999);
    (value * 16.0) as i32
}

fn photo_color_value(color: RgbColor) -> u8 {
    color.red.max(color.green).max(color.blue)
}

fn photo_color_saturation(color: RgbColor) -> f64 {
    let max = f64::from(photo_color_value(color)) / 255.0;
    if max == 0.0 {
        return 0.0;
    }
    let min = f64::from(color.red.min(color.green).min(color.blue)) / 255.0;
    (max - min) / max
}

fn photo_color_hue_degrees(color: RgbColor) -> f64 {
    let red = f64::from(color.red) / 255.0;
    let green = f64::from(color.green) / 255.0;
    let blue = f64::from(color.blue) / 255.0;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let delta = max - min;
    if delta == 0.0 {
        return 0.0;
    }
    let hue = if max == red {
        60.0 * ((green - blue) / delta).rem_euclid(6.0)
    } else if max == green {
        60.0 * (((blue - red) / delta) + 2.0)
    } else {
        60.0 * (((red - green) / delta) + 4.0)
    };
    if hue < 0.0 {
        hue + 360.0
    } else {
        hue
    }
}

const PALETTE_SAND_CANDIDATES: [i32; 16] = [
    block_state_ids::SAND,
    block_state_ids::SANDSTONE,
    block_state_ids::YELLOW_TERRACOTTA,
    block_state_ids::END_STONE,
    block_state_ids::END_STONE_BRICKS,
    block_state_ids::SMOOTH_SANDSTONE,
    block_state_ids::CUT_SANDSTONE,
    block_state_ids::CHISELED_SANDSTONE,
    block_state_ids::WHITE_TERRACOTTA,
    block_state_ids::LIGHT_GRAY_TERRACOTTA,
    block_state_ids::BONE_BLOCK,
    block_state_ids::CALCITE,
    block_state_ids::QUARTZ_BLOCK,
    block_state_ids::GRAVEL,
    block_state_ids::TERRACOTTA,
    block_state_ids::PACKED_MUD,
];

const PALETTE_RED_SAND_CANDIDATES: [i32; 13] = [
    block_state_ids::RED_SAND,
    block_state_ids::ORANGE_TERRACOTTA,
    block_state_ids::TERRACOTTA,
    block_state_ids::SMOOTH_RED_SANDSTONE,
    block_state_ids::CUT_RED_SANDSTONE,
    block_state_ids::CHISELED_RED_SANDSTONE,
    block_state_ids::MUD_BRICKS,
    block_state_ids::RED_TERRACOTTA,
    block_state_ids::BROWN_TERRACOTTA,
    block_state_ids::GRANITE,
    block_state_ids::PACKED_MUD,
    block_state_ids::WHITE_TERRACOTTA,
    block_state_ids::BONE_BLOCK,
];

const PALETTE_COARSE_DIRT_CANDIDATES: [i32; 11] = [
    block_state_ids::COARSE_DIRT,
    block_state_ids::DIRT,
    block_state_ids::ROOTED_DIRT,
    block_state_ids::PACKED_MUD,
    block_state_ids::PODZOL,
    block_state_ids::TERRACOTTA,
    block_state_ids::BROWN_TERRACOTTA,
    block_state_ids::GREEN_TERRACOTTA,
    block_state_ids::MYCELIUM,
    block_state_ids::MUD_BRICKS,
    block_state_ids::DRIPSTONE_BLOCK,
];

const PALETTE_ROCK_CANDIDATES: [i32; 18] = [
    block_state_ids::STONE,
    block_state_ids::TUFF,
    block_state_ids::GRAVEL,
    block_state_ids::DEEPSLATE,
    block_state_ids::ANDESITE,
    block_state_ids::DIORITE,
    block_state_ids::CYAN_TERRACOTTA,
    block_state_ids::GRAY_TERRACOTTA,
    block_state_ids::BLACK_TERRACOTTA,
    block_state_ids::CALCITE,
    block_state_ids::TERRACOTTA,
    block_state_ids::QUARTZ_BLOCK,
    block_state_ids::BONE_BLOCK,
    block_state_ids::END_STONE,
    block_state_ids::END_STONE_BRICKS,
    block_state_ids::SMOOTH_RED_SANDSTONE,
    block_state_ids::CUT_RED_SANDSTONE,
    block_state_ids::CHISELED_RED_SANDSTONE,
];

const PHOTO_GRASS_RENDER_BIOMES: [&str; 16] = [
    "minecraft:dark_forest",
    "minecraft:jungle",
    "minecraft:bamboo_jungle",
    "minecraft:sparse_jungle",
    "minecraft:forest",
    "minecraft:flower_forest",
    "minecraft:taiga",
    "minecraft:swamp",
    "minecraft:plains",
    "minecraft:sunflower_plains",
    "minecraft:meadow",
    "minecraft:savanna",
    "minecraft:savanna_plateau",
    "minecraft:windswept_savanna",
    "minecraft:snowy_plains",
    "minecraft:beach",
];

const PHOTO_SOLVER_CANDIDATE_BLOCKS: &[i32] = &[
    block_state_ids::GRASS_BLOCK,
    block_state_ids::OAK_LEAVES,
    block_state_ids::JUNGLE_LEAVES,
    block_state_ids::DARK_OAK_LEAVES,
    block_state_ids::SPRUCE_LEAVES,
    block_state_ids::MOSS_BLOCK,
    block_state_ids::PODZOL,
    block_state_ids::COARSE_DIRT,
    block_state_ids::DIRT,
    block_state_ids::ROOTED_DIRT,
    block_state_ids::MYCELIUM,
    block_state_ids::MUD,
    block_state_ids::PACKED_MUD,
    block_state_ids::GREEN_TERRACOTTA,
    block_state_ids::LIME_TERRACOTTA,
    block_state_ids::GRAY_TERRACOTTA,
    block_state_ids::BLACK_TERRACOTTA,
    block_state_ids::SAND,
    block_state_ids::SANDSTONE,
    block_state_ids::YELLOW_TERRACOTTA,
    block_state_ids::WHITE_TERRACOTTA,
    block_state_ids::LIGHT_GRAY_TERRACOTTA,
    block_state_ids::BONE_BLOCK,
    block_state_ids::CALCITE,
    block_state_ids::QUARTZ_BLOCK,
    block_state_ids::END_STONE,
    block_state_ids::END_STONE_BRICKS,
    block_state_ids::SMOOTH_SANDSTONE,
    block_state_ids::CUT_SANDSTONE,
    block_state_ids::CHISELED_SANDSTONE,
    block_state_ids::GRAVEL,
    block_state_ids::TERRACOTTA,
    block_state_ids::RED_SAND,
    block_state_ids::ORANGE_TERRACOTTA,
    block_state_ids::RED_TERRACOTTA,
    block_state_ids::BROWN_TERRACOTTA,
    block_state_ids::GRANITE,
    block_state_ids::SMOOTH_RED_SANDSTONE,
    block_state_ids::CUT_RED_SANDSTONE,
    block_state_ids::CHISELED_RED_SANDSTONE,
    block_state_ids::MUD_BRICKS,
    block_state_ids::DRIPSTONE_BLOCK,
    block_state_ids::STONE,
    block_state_ids::TUFF,
    block_state_ids::DEEPSLATE,
    block_state_ids::ANDESITE,
    block_state_ids::DIORITE,
    block_state_ids::CYAN_TERRACOTTA,
    block_state_ids::SNOW_BLOCK,
    block_state_ids::CLAY,
];

const PALETTE_VEGETATED_CANDIDATES: &[i32] = &[
    block_state_ids::GRASS_BLOCK,
    block_state_ids::OAK_LEAVES,
    block_state_ids::JUNGLE_LEAVES,
    block_state_ids::DARK_OAK_LEAVES,
    block_state_ids::SPRUCE_LEAVES,
    block_state_ids::MOSS_BLOCK,
    block_state_ids::PODZOL,
    block_state_ids::COARSE_DIRT,
    block_state_ids::ROOTED_DIRT,
    block_state_ids::MYCELIUM,
    block_state_ids::MUD,
    block_state_ids::PACKED_MUD,
    block_state_ids::GREEN_TERRACOTTA,
    block_state_ids::LIME_TERRACOTTA,
    block_state_ids::GRAY_TERRACOTTA,
    block_state_ids::BLACK_TERRACOTTA,
];

const PALETTE_WET_CANDIDATES: &[i32] = &[
    block_state_ids::MUD,
    block_state_ids::CLAY,
    block_state_ids::MOSS_BLOCK,
    block_state_ids::GRASS_BLOCK,
    block_state_ids::OAK_LEAVES,
    block_state_ids::JUNGLE_LEAVES,
    block_state_ids::DARK_OAK_LEAVES,
    block_state_ids::SPRUCE_LEAVES,
    block_state_ids::PACKED_MUD,
    block_state_ids::BLACK_TERRACOTTA,
    block_state_ids::DEEPSLATE,
];

const DRY_GRASS_TEXTURE_CANDIDATES: &[i32] = &[
    block_state_ids::COARSE_DIRT,
    block_state_ids::ROOTED_DIRT,
    block_state_ids::PACKED_MUD,
    block_state_ids::PODZOL,
    block_state_ids::MOSS_BLOCK,
    block_state_ids::MYCELIUM,
    block_state_ids::GREEN_TERRACOTTA,
    block_state_ids::LIME_TERRACOTTA,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EcoregionSample {
    pub name: String,
    pub biome_id: String,
}

impl EcoregionSample {
    pub fn new(name: impl Into<String>, biome_id: impl Into<String>) -> Self {
        let name = name.into();
        let biome_id = biome_id.into();
        Self {
            name: java_string_trim(&name).to_string(),
            biome_id: java_string_trim(&biome_id).to_string(),
        }
    }

    pub fn unknown() -> Self {
        Self::new("", "")
    }

    pub fn available(&self) -> bool {
        !self.name.is_empty()
    }

    pub fn has_biome(&self) -> bool {
        !self.biome_id.is_empty()
    }
}

pub mod surface_data_evidence {
    use super::SurfaceMaterialSample;

    pub const CLIMATE: i32 = 1 << 0;
    pub const TREE: i32 = 1 << 1;
    pub const HERBACEOUS: i32 = 1 << 2;
    pub const SHRUB: i32 = 1 << 3;
    pub const SNOW: i32 = 1 << 4;
    pub const SWAMP: i32 = 1 << 5;
    pub const OCEAN_TEMPERATURE: i32 = 1 << 6;
    pub const BATHYMETRY: i32 = 1 << 7;
    pub const SLOPE: i32 = 1 << 8;
    pub const ECOREGION: i32 = 1 << 9;
    pub const TREE_PRESENT: i32 = 1 << 10;
    pub const HERBACEOUS_PRESENT: i32 = 1 << 11;
    pub const SHRUB_PRESENT: i32 = 1 << 12;
    pub const SNOW_PRESENT: i32 = 1 << 13;
    pub const SWAMP_PRESENT: i32 = 1 << 14;
    pub const STEEP_SLOPE: i32 = 1 << 15;

    pub fn from_sample(sample: &SurfaceMaterialSample) -> i32 {
        let mut flags = 0;
        if sample.has_climate_class() {
            flags |= CLIMATE;
        }
        if known(sample.evergreen_broadleaf_trees)
            || known(sample.deciduous_broadleaf_trees)
            || known(sample.needleleaf_trees)
            || known(sample.mixed_trees)
        {
            flags |= TREE;
        }
        if positive(sample.evergreen_broadleaf_trees)
            || positive(sample.deciduous_broadleaf_trees)
            || positive(sample.needleleaf_trees)
            || positive(sample.mixed_trees)
        {
            flags |= TREE_PRESENT;
        }
        if known(sample.herbaceous_vegetation) {
            flags |= HERBACEOUS;
        }
        if positive(sample.herbaceous_vegetation) {
            flags |= HERBACEOUS_PRESENT;
        }
        if known(sample.shrubs) {
            flags |= SHRUB;
        }
        if positive(sample.shrubs) {
            flags |= SHRUB_PRESENT;
        }
        if known(sample.snow_cover) {
            flags |= SNOW;
        }
        if positive(sample.snow_cover) {
            flags |= SNOW_PRESENT;
        }
        if known(sample.swamp_cover) {
            flags |= SWAMP;
        }
        if positive(sample.swamp_cover) {
            flags |= SWAMP_PRESENT;
        }
        if known(sample.ocean_temperature) {
            flags |= OCEAN_TEMPERATURE;
        }
        if sample.has_bathymetry() {
            flags |= BATHYMETRY;
        }
        if known(sample.slope_permille) {
            flags |= SLOPE;
        }
        if sample.slope_ratio() >= 0.20 {
            flags |= STEEP_SLOPE;
        }
        if sample.has_ecoregion() || sample.has_ecoregion_biome() {
            flags |= ECOREGION;
        }
        flags
    }

    pub fn has(flags: i32, mask: i32) -> bool {
        (flags & mask) != 0
    }

    pub fn has_any_vegetation(flags: i32) -> bool {
        has(flags, TREE) || has(flags, HERBACEOUS) || has(flags, SHRUB)
    }

    pub fn has_any_vegetation_present(flags: i32) -> bool {
        has(flags, TREE_PRESENT) || has(flags, HERBACEOUS_PRESENT) || has(flags, SHRUB_PRESENT)
    }

    fn known(value: i32) -> bool {
        value != SurfaceMaterialSample::UNKNOWN && value != 255
    }

    fn positive(value: i32) -> bool {
        known(value) && value > 0
    }
}

pub trait SurfaceMaterialSampler: Send + Sync {
    fn sample(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample>;

    fn sample_photo(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        self.sample(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )
    }

    fn samples_open_water(&self) -> bool {
        false
    }

    fn sample_water(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        self.sample(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )
    }
}

#[derive(Debug)]
pub struct TrueMarbleSurfaceMaterialSampler {
    reader: VrtRgbMosaicReader,
}

impl TrueMarbleSurfaceMaterialSampler {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            reader: VrtRgbMosaicReader::open(path)?,
        })
    }

    pub fn reader(&self) -> &VrtRgbMosaicReader {
        &self.reader
    }
}

impl SurfaceMaterialSampler for TrueMarbleSurfaceMaterialSampler {
    fn sample(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        Ok(SurfaceMaterialSample::color_only(
            self.reader.sample_averaged(
                longitude,
                latitude,
                longitude_span_degrees,
                latitude_span_degrees,
            )?,
        ))
    }
}

#[derive(Debug)]
pub struct EarthDataSurfaceMaterialSampler {
    true_marble: VrtRgbMosaicReader,
    climate: Option<GeoTiffSingleBandReader>,
    evergreen_broadleaf_trees: Option<GeoTiffSingleBandReader>,
    deciduous_broadleaf_trees: Option<GeoTiffSingleBandReader>,
    needleleaf_trees: Option<GeoTiffSingleBandReader>,
    mixed_trees: Option<GeoTiffSingleBandReader>,
    herbaceous_vegetation: Option<GeoTiffSingleBandReader>,
    shrubs: Option<GeoTiffSingleBandReader>,
    snow: Option<GeoTiffSingleBandReader>,
    swamp: Option<GeoTiffSingleBandReader>,
    ocean_temperature: Option<GeoTiffSingleBandReader>,
    bathymetry: Option<GeoTiffFloat32Reader>,
    slope: Option<GeoTiffFloat32Reader>,
    ecoregions: Option<Box<dyn EcoregionSampler>>,
    terrain_tokens: Option<MetImageExportTerrainSampler>,
    land_shallow_topo: Option<LandShallowTopoPhotoSampler>,
    material_cache: ShardedAccessCache<SurfaceMaterialSample>,
    photo_color_cache: ShardedAccessCache<RgbColor>,
    photo_evidence_cache: ShardedAccessCache<SurfaceMaterialSample>,
    ocean_cache: ShardedAccessCache<SurfaceMaterialSample>,
    ecoregion_cache: ShardedAccessCache<EcoregionEvidence>,
}

#[derive(Clone, Debug)]
struct SurfaceSamplerThreadCacheEntry<T> {
    sampler_id: usize,
    key: i64,
    value: T,
}

thread_local! {
    static SURFACE_MATERIAL_SAMPLE_L1: RefCell<Option<SurfaceSamplerThreadCacheEntry<SurfaceMaterialSample>>> = RefCell::new(None);
    static SURFACE_PHOTO_COLOR_L1: RefCell<Option<SurfaceSamplerThreadCacheEntry<RgbColor>>> = RefCell::new(None);
    static SURFACE_PHOTO_EVIDENCE_L1: RefCell<Option<SurfaceSamplerThreadCacheEntry<SurfaceMaterialSample>>> = RefCell::new(None);
    static SURFACE_OCEAN_SAMPLE_L1: RefCell<Option<SurfaceSamplerThreadCacheEntry<SurfaceMaterialSample>>> = RefCell::new(None);
    static SURFACE_ECOREGION_L1: RefCell<Option<SurfaceSamplerThreadCacheEntry<EcoregionEvidence>>> = RefCell::new(None);
}

fn surface_sampler_l1_get<T: Clone + 'static>(
    cache: &'static LocalKey<RefCell<Option<SurfaceSamplerThreadCacheEntry<T>>>>,
    sampler_id: usize,
    key: i64,
) -> Option<T> {
    cache.with(|cache| {
        cache
            .borrow()
            .as_ref()
            .filter(|entry| entry.sampler_id == sampler_id && entry.key == key)
            .map(|entry| entry.value.clone())
    })
}

fn surface_sampler_l1_put<T: Clone + 'static>(
    cache: &'static LocalKey<RefCell<Option<SurfaceSamplerThreadCacheEntry<T>>>>,
    sampler_id: usize,
    key: i64,
    value: T,
) {
    cache.with(|cache| {
        *cache.borrow_mut() = Some(SurfaceSamplerThreadCacheEntry {
            sampler_id,
            key,
            value,
        });
    });
}

impl EarthDataSurfaceMaterialSampler {
    const DEFAULT_CACHE_BLOCKS: usize = 256;
    const MATERIAL_CACHE_ENTRIES: usize = 262_144;
    const OCEAN_CACHE_ENTRIES: usize = 262_144;
    const ECOREGION_CACHE_ENTRIES: usize = 262_144;
    const PHOTO_EVIDENCE_CACHE_ENTRIES: usize = Self::MATERIAL_CACHE_ENTRIES / 2;
    const CACHE_SHARDS: usize = 64;

    fn thread_cache_id(&self) -> usize {
        self as *const Self as usize
    }

    pub fn open(true_marble_path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_tile_cache_entries(true_marble_path, DEFAULT_SURFACE_TILE_CACHE_ENTRIES)
    }

    pub fn open_with_tile_cache_entries(
        true_marble_path: impl AsRef<Path>,
        tile_cache_entries: usize,
    ) -> Result<Self> {
        if tile_cache_entries == 0 {
            return Err(SurfaceError::invalid(
                "surfaceTileCacheEntries must be positive",
            ));
        }
        let true_marble_path = true_marble_path.as_ref();
        let terrain_dir = true_marble_path
            .canonicalize()
            .unwrap_or_else(|_| true_marble_path.to_path_buf())
            .parent()
            .map(Path::to_path_buf);
        let tif_root = terrain_dir
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf);
        let vegetation = tif_root.as_ref().map(|root| root.join("vegetation"));
        Ok(Self {
            true_marble: VrtRgbMosaicReader::open_with_tile_cache_entries(
                true_marble_path,
                tile_cache_entries,
            )?,
            climate: open_surface_raster(tif_root.as_deref(), "climate.tif", tile_cache_entries)?,
            evergreen_broadleaf_trees: open_surface_raster(
                vegetation.as_deref(),
                "EvergreenBroadleafTrees.tif",
                tile_cache_entries,
            )?,
            deciduous_broadleaf_trees: open_surface_raster(
                vegetation.as_deref(),
                "DeciduousBroadleafTrees.tif",
                tile_cache_entries,
            )?,
            needleleaf_trees: open_surface_raster(
                vegetation.as_deref(),
                "EvergreenDeciduousNeedleleafTrees.tif",
                tile_cache_entries,
            )?,
            mixed_trees: open_surface_raster(
                vegetation.as_deref(),
                "mixed.tif",
                tile_cache_entries,
            )?,
            herbaceous_vegetation: open_surface_raster(
                vegetation.as_deref(),
                "HerbaceousVegetation.tif",
                tile_cache_entries,
            )?,
            shrubs: open_surface_raster(vegetation.as_deref(), "Shrubs.tif", tile_cache_entries)?,
            snow: open_surface_raster(vegetation.as_deref(), "Snow.tif", tile_cache_entries)?,
            swamp: open_surface_raster(vegetation.as_deref(), "Swamp.tif", tile_cache_entries)?,
            ocean_temperature: open_surface_raster(
                tif_root.as_deref(),
                "ocean_temp_infill.tif",
                tile_cache_entries,
            )?,
            bathymetry: open_surface_float_raster(tif_root.as_deref(), "bathymetry.tif")?,
            slope: open_surface_float_raster(tif_root.as_deref(), "slope.tif")?,
            ecoregions: WwfEcoregionSampler::open_auto_cache(true_marble_path)?
                .map(|sampler| Box::new(sampler) as Box<dyn EcoregionSampler>),
            terrain_tokens: MetImageExportTerrainSampler::open_auto(true_marble_path)?,
            land_shallow_topo: LandShallowTopoPhotoSampler::open_near_with_tile_cache_entries(
                true_marble_path,
                tile_cache_entries,
            )?,
            material_cache: ShardedAccessCache::new(
                Self::MATERIAL_CACHE_ENTRIES,
                Self::CACHE_SHARDS,
            ),
            photo_color_cache: ShardedAccessCache::new(
                Self::MATERIAL_CACHE_ENTRIES,
                Self::CACHE_SHARDS,
            ),
            photo_evidence_cache: ShardedAccessCache::new(
                Self::PHOTO_EVIDENCE_CACHE_ENTRIES,
                Self::CACHE_SHARDS,
            ),
            ocean_cache: ShardedAccessCache::new(Self::OCEAN_CACHE_ENTRIES, Self::CACHE_SHARDS),
            ecoregion_cache: ShardedAccessCache::new(
                Self::ECOREGION_CACHE_ENTRIES,
                Self::CACHE_SHARDS,
            ),
        })
    }

    pub fn raster_stats(&self) -> SurfaceMaterialRasterStats {
        let stats = self.true_marble.stats();
        SurfaceMaterialRasterStats {
            source_count: stats.source_count,
            open_readers: stats.open_readers,
            resident_tiles: stats.resident_tiles,
            tile_hits: stats.tile_hits,
            tile_misses: stats.tile_misses,
            tile_evictions: stats.tile_evictions,
            sample_nearest_requests: stats.sample_nearest_requests,
            sample_averaged_requests: stats.sample_averaged_requests,
        }
    }

    fn sample_land(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        let cell_degrees = material_cell_degrees(longitude_span_degrees, latitude_span_degrees);
        let cell = quantized_cell(longitude, latitude, cell_degrees);
        let sampler_id = self.thread_cache_id();
        if let Some(cached) =
            surface_sampler_l1_get(&SURFACE_MATERIAL_SAMPLE_L1, sampler_id, cell.key)
        {
            return Ok(self.with_terrain_token(&cached, longitude, latitude));
        }
        if let Some(cached) = self
            .material_cache
            .get(cell.key, "surface material cache lock poisoned")?
        {
            surface_sampler_l1_put(
                &SURFACE_MATERIAL_SAMPLE_L1,
                sampler_id,
                cell.key,
                cached.clone(),
            );
            return Ok(self.with_terrain_token(&cached, longitude, latitude));
        }
        let sampled = self.sample_land_uncached(
            cell.center_longitude,
            cell.center_latitude,
            cell_degrees,
            cell_degrees,
        )?;
        let sample = self.material_cache.insert_or_get(
            cell.key,
            sampled,
            "surface material cache lock poisoned",
        )?;
        surface_sampler_l1_put(
            &SURFACE_MATERIAL_SAMPLE_L1,
            sampler_id,
            cell.key,
            sample.clone(),
        );
        Ok(self.with_terrain_token(&sample, longitude, latitude))
    }

    fn sample_photo_with_coarse_evidence(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        let color = self.sample_photo_color(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )?;
        let evidence = self.sample_photo_evidence(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )?;
        Ok(self.with_terrain_token(&evidence.with_color(color), longitude, latitude))
    }

    fn sample_photo_color(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<RgbColor> {
        let cell_degrees = photo_cell_degrees(longitude_span_degrees, latitude_span_degrees);
        let cell = quantized_cell(longitude, latitude, cell_degrees);
        let sampler_id = self.thread_cache_id();
        if let Some(cached) = surface_sampler_l1_get(&SURFACE_PHOTO_COLOR_L1, sampler_id, cell.key)
        {
            return Ok(cached);
        }
        if let Some(cached) = self
            .photo_color_cache
            .get(cell.key, "surface photo color cache lock poisoned")?
        {
            surface_sampler_l1_put(&SURFACE_PHOTO_COLOR_L1, sampler_id, cell.key, cached);
            return Ok(cached);
        }
        let sampled = self.sample_primary_photo_color(
            cell.center_longitude,
            cell.center_latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )?;
        let color = self.photo_color_cache.insert_or_get(
            cell.key,
            sampled,
            "surface photo color cache lock poisoned",
        )?;
        surface_sampler_l1_put(&SURFACE_PHOTO_COLOR_L1, sampler_id, cell.key, color);
        Ok(color)
    }

    fn sample_primary_photo_color(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<RgbColor> {
        let prefer_topographic = prefers_topographic_photo_source();
        if prefer_topographic {
            let topographic = self.sample_topographic_photo_color(
                longitude,
                latitude,
                longitude_span_degrees,
                latitude_span_degrees,
            )?;
            if usable_photo_color(topographic) {
                return Ok(topographic);
            }
        }
        let satellite = self.true_marble.sample_averaged(
            longitude,
            latitude,
            photo_average_span_degrees(longitude_span_degrees),
            photo_average_span_degrees(latitude_span_degrees),
        )?;
        if usable_photo_color(satellite) {
            return Ok(satellite);
        }
        if !prefer_topographic {
            let topographic = self.sample_topographic_photo_color(
                longitude,
                latitude,
                longitude_span_degrees,
                latitude_span_degrees,
            )?;
            if usable_photo_color(topographic) {
                return Ok(topographic);
            }
        }
        self.true_marble
            .sample_nearest(longitude, latitude)
            .map_err(Into::into)
    }

    fn sample_topographic_photo_color(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<RgbColor> {
        let Some(land_shallow_topo) = self.land_shallow_topo.as_ref() else {
            return Ok(RgbColor::unavailable());
        };
        land_shallow_topo.sample_averaged(
            longitude,
            latitude,
            photo_average_span_degrees(longitude_span_degrees),
            photo_average_span_degrees(latitude_span_degrees),
        )
    }

    fn sample_photo_evidence(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        let cell_degrees =
            photo_evidence_cell_degrees(longitude_span_degrees, latitude_span_degrees);
        let cell = quantized_cell(longitude, latitude, cell_degrees);
        let sampler_id = self.thread_cache_id();
        if let Some(cached) =
            surface_sampler_l1_get(&SURFACE_PHOTO_EVIDENCE_L1, sampler_id, cell.key)
        {
            return Ok(cached);
        }
        if let Some(cached) = self
            .photo_evidence_cache
            .get(cell.key, "surface photo evidence cache lock poisoned")?
        {
            surface_sampler_l1_put(
                &SURFACE_PHOTO_EVIDENCE_L1,
                sampler_id,
                cell.key,
                cached.clone(),
            );
            return Ok(cached);
        }
        let sampled = self.sample_photo_evidence_uncached(
            cell.center_longitude,
            cell.center_latitude,
            cell_degrees,
            cell_degrees,
        )?;
        let evidence = self.photo_evidence_cache.insert_or_get(
            cell.key,
            sampled,
            "surface photo evidence cache lock poisoned",
        )?;
        surface_sampler_l1_put(
            &SURFACE_PHOTO_EVIDENCE_L1,
            sampler_id,
            cell.key,
            evidence.clone(),
        );
        Ok(evidence)
    }

    fn sample_photo_evidence_uncached(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        let ecoregion = self.sample_ecoregion(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )?;
        Ok(SurfaceMaterialSample::new(
            RgbColor::unavailable(),
            RgbColor::unavailable(),
            TerrainTokenSource::None,
            sample_rounded(self.climate.as_ref(), longitude, latitude),
            sample_rounded(self.evergreen_broadleaf_trees.as_ref(), longitude, latitude),
            sample_rounded(self.deciduous_broadleaf_trees.as_ref(), longitude, latitude),
            sample_rounded(self.needleleaf_trees.as_ref(), longitude, latitude),
            sample_rounded(self.mixed_trees.as_ref(), longitude, latitude),
            sample_rounded(self.herbaceous_vegetation.as_ref(), longitude, latitude),
            sample_rounded(self.shrubs.as_ref(), longitude, latitude),
            sample_rounded(self.snow.as_ref(), longitude, latitude),
            sample_rounded(self.swamp.as_ref(), longitude, latitude),
            sample_rounded(self.ocean_temperature.as_ref(), longitude, latitude),
            sample_bathymetry_meters(self.bathymetry.as_ref(), longitude, latitude),
            sample_slope_permille(self.slope.as_ref(), longitude, latitude),
            ecoregion.sample.name,
            ecoregion.sample.biome_id,
            ecoregion.confidence,
        ))
    }

    fn sample_land_uncached(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        let color = self.true_marble.sample_averaged(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )?;
        let ecoregion = self.sample_ecoregion(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )?;
        Ok(SurfaceMaterialSample::land(
            color,
            sample_rounded(self.climate.as_ref(), longitude, latitude),
            sample_rounded(self.evergreen_broadleaf_trees.as_ref(), longitude, latitude),
            sample_rounded(self.deciduous_broadleaf_trees.as_ref(), longitude, latitude),
            sample_rounded(self.needleleaf_trees.as_ref(), longitude, latitude),
            sample_rounded(self.mixed_trees.as_ref(), longitude, latitude),
            sample_rounded(self.herbaceous_vegetation.as_ref(), longitude, latitude),
            sample_rounded(self.shrubs.as_ref(), longitude, latitude),
            sample_rounded(self.snow.as_ref(), longitude, latitude),
            sample_rounded(self.swamp.as_ref(), longitude, latitude),
            sample_rounded(self.ocean_temperature.as_ref(), longitude, latitude),
            sample_bathymetry_meters(self.bathymetry.as_ref(), longitude, latitude),
            sample_slope_permille(self.slope.as_ref(), longitude, latitude),
            ecoregion.sample.name,
            ecoregion.sample.biome_id,
            ecoregion.confidence,
        ))
    }

    fn sample_ecoregion(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<EcoregionEvidence> {
        if self.ecoregions.is_none() {
            return Ok(EcoregionEvidence::unknown());
        }
        let cell_degrees = ecoregion_cell_degrees(longitude_span_degrees, latitude_span_degrees);
        let cell = quantized_cell(longitude, latitude, cell_degrees);
        let sampler_id = self.thread_cache_id();
        if let Some(cached) = surface_sampler_l1_get(&SURFACE_ECOREGION_L1, sampler_id, cell.key) {
            return Ok(cached);
        }
        if let Some(cached) = self
            .ecoregion_cache
            .get(cell.key, "surface ecoregion cache lock poisoned")?
        {
            surface_sampler_l1_put(&SURFACE_ECOREGION_L1, sampler_id, cell.key, cached.clone());
            return Ok(cached);
        }
        let sampled = self.sample_ecoregion_uncached(
            cell.center_longitude,
            cell.center_latitude,
            cell_degrees,
        );
        let evidence = self.ecoregion_cache.insert_or_get(
            cell.key,
            sampled,
            "surface ecoregion cache lock poisoned",
        )?;
        surface_sampler_l1_put(
            &SURFACE_ECOREGION_L1,
            sampler_id,
            cell.key,
            evidence.clone(),
        );
        Ok(evidence)
    }

    fn sample_ecoregion_uncached(
        &self,
        longitude: f64,
        latitude: f64,
        cell_degrees: f64,
    ) -> EcoregionEvidence {
        let Some(ecoregions) = self.ecoregions.as_deref() else {
            return EcoregionEvidence::unknown();
        };
        sample_ecoregion_evidence(ecoregions, longitude, latitude, cell_degrees)
    }

    fn sample_water_cached(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        let cell_degrees = material_cell_degrees(longitude_span_degrees, latitude_span_degrees);
        let cell = quantized_cell(longitude, latitude, cell_degrees);
        let sampler_id = self.thread_cache_id();
        if let Some(cached) = surface_sampler_l1_get(&SURFACE_OCEAN_SAMPLE_L1, sampler_id, cell.key)
        {
            return Ok(self.with_terrain_token(&cached, longitude, latitude));
        }
        if let Some(cached) = self
            .ocean_cache
            .get(cell.key, "surface ocean cache lock poisoned")?
        {
            surface_sampler_l1_put(
                &SURFACE_OCEAN_SAMPLE_L1,
                sampler_id,
                cell.key,
                cached.clone(),
            );
            return Ok(self.with_terrain_token(&cached, longitude, latitude));
        }
        let sampled = self.sample_ocean_uncached(
            cell.center_longitude,
            cell.center_latitude,
            cell_degrees,
            cell_degrees,
        );
        let sample = self.ocean_cache.insert_or_get(
            cell.key,
            sampled,
            "surface ocean cache lock poisoned",
        )?;
        surface_sampler_l1_put(
            &SURFACE_OCEAN_SAMPLE_L1,
            sampler_id,
            cell.key,
            sample.clone(),
        );
        Ok(self.with_terrain_token(&sample, longitude, latitude))
    }

    fn sample_ocean_uncached(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> SurfaceMaterialSample {
        let color = self
            .true_marble
            .sample_averaged(
                longitude,
                latitude,
                longitude_span_degrees,
                latitude_span_degrees,
            )
            .unwrap_or_else(|_| RgbColor::unavailable());
        SurfaceMaterialSample::new(
            color,
            RgbColor::unavailable(),
            TerrainTokenSource::None,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            sample_rounded(self.ocean_temperature.as_ref(), longitude, latitude),
            sample_bathymetry_meters(self.bathymetry.as_ref(), longitude, latitude),
            SurfaceMaterialSample::UNKNOWN,
            "",
            "",
            0.0,
        )
    }

    fn sample_terrain_token_color(&self, longitude: f64, latitude: f64) -> RgbColor {
        let Some(terrain_tokens) = self.terrain_tokens.as_ref() else {
            return RgbColor::unavailable();
        };
        terrain_tokens
            .sample(longitude, latitude)
            .unwrap_or_else(|_| RgbColor::unavailable())
    }

    fn with_terrain_token(
        &self,
        sample: &SurfaceMaterialSample,
        longitude: f64,
        latitude: f64,
    ) -> SurfaceMaterialSample {
        with_surface_terrain_token(sample, self.sample_terrain_token_color(longitude, latitude))
    }
}

impl SurfaceMaterialSampler for EarthDataSurfaceMaterialSampler {
    fn sample(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        self.sample_land(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )
    }

    fn sample_photo(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        self.sample_photo_with_coarse_evidence(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )
    }

    fn samples_open_water(&self) -> bool {
        self.bathymetry.is_some() || self.ocean_temperature.is_some()
    }

    fn sample_water(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<SurfaceMaterialSample> {
        self.sample_water_cached(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )
    }
}

#[derive(Debug)]
struct BoundedAccessCache<T> {
    entries: HashMap<i64, BoundedCacheEntry<T>>,
    access_order: BTreeSet<(u64, i64)>,
    max_entries: usize,
    access_clock: u64,
}

impl<T: Clone> BoundedAccessCache<T> {
    fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(max_entries.min(4096)),
            access_order: BTreeSet::new(),
            max_entries,
            access_clock: 0,
        }
    }

    fn get(&mut self, key: i64) -> Option<T> {
        let stamp = self.next_access_stamp();
        let entry = self.entries.get_mut(&key)?;
        let old_stamp = entry.access_stamp;
        entry.access_stamp = stamp;
        let value = entry.value.clone();
        self.access_order.remove(&(old_stamp, key));
        self.access_order.insert((stamp, key));
        Some(value)
    }

    fn insert_or_get(&mut self, key: i64, value: T) -> T {
        let stamp = self.next_access_stamp();
        if let Some(entry) = self.entries.get_mut(&key) {
            let old_stamp = entry.access_stamp;
            entry.access_stamp = stamp;
            let existing = entry.value.clone();
            self.access_order.remove(&(old_stamp, key));
            self.access_order.insert((stamp, key));
            return existing;
        }
        if self.max_entries == 0 {
            return value;
        }
        self.entries.insert(
            key,
            BoundedCacheEntry {
                value: value.clone(),
                access_stamp: stamp,
            },
        );
        self.access_order.insert((stamp, key));
        self.trim_to_capacity();
        value
    }

    fn next_access_stamp(&mut self) -> u64 {
        self.access_clock = self.access_clock.saturating_add(1);
        self.access_clock
    }

    fn trim_to_capacity(&mut self) {
        while self.entries.len() > self.max_entries {
            let Some((_oldest_stamp, oldest_key)) = self.access_order.pop_first() else {
                return;
            };
            self.entries.remove(&oldest_key);
        }
    }
}

#[derive(Debug)]
struct BoundedCacheEntry<T> {
    value: T,
    access_stamp: u64,
}

#[derive(Debug)]
struct ShardedAccessCache<T> {
    shards: Vec<Mutex<BoundedAccessCache<T>>>,
}

impl<T: Clone> ShardedAccessCache<T> {
    fn new(max_entries: usize, shard_count: usize) -> Self {
        let shard_count = shard_count.max(1);
        let entries_per_shard = max_entries.saturating_add(shard_count - 1) / shard_count;
        let shards = (0..shard_count)
            .map(|_| Mutex::new(BoundedAccessCache::new(entries_per_shard)))
            .collect();
        Self { shards }
    }

    fn get(&self, key: i64, poison_message: &'static str) -> Result<Option<T>> {
        let mut shard = self
            .shard(key)
            .lock()
            .map_err(|_| SurfaceError::invalid(poison_message))?;
        Ok(shard.get(key))
    }

    fn insert_or_get(&self, key: i64, value: T, poison_message: &'static str) -> Result<T> {
        let mut shard = self
            .shard(key)
            .lock()
            .map_err(|_| SurfaceError::invalid(poison_message))?;
        Ok(shard.insert_or_get(key, value))
    }

    fn shard(&self, key: i64) -> &Mutex<BoundedAccessCache<T>> {
        let mixed = (key as u64)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .rotate_left(17);
        &self.shards[(mixed as usize) % self.shards.len()]
    }
}

#[derive(Debug)]
pub struct LandShallowTopoPhotoSampler {
    west_path: PathBuf,
    east_path: PathBuf,
    tile_cache_entries: usize,
    readers: Mutex<HashMap<LandShallowReaderKey, Arc<GeoTiffRgbReader>>>,
}

impl LandShallowTopoPhotoSampler {
    const WEST_FILE: &'static str = "land_shallow_topo_west.tif";
    const EAST_FILE: &'static str = "land_shallow_topo_east.tif";
    const HALF_WORLD_DEGREES: f64 = 180.0;
    const DEFAULT_PIXEL_DEGREES: f64 = 1.0 / 120.0;
    const ROW_CACHE_ENTRIES: usize = 1024;

    pub fn open_near(true_marble_path: impl AsRef<Path>) -> Result<Option<Self>> {
        Self::open_near_with_tile_cache_entries(true_marble_path, Self::ROW_CACHE_ENTRIES)
    }

    pub fn open_near_with_tile_cache_entries(
        true_marble_path: impl AsRef<Path>,
        tile_cache_entries: usize,
    ) -> Result<Option<Self>> {
        let normalized = absolute_normalized_path(true_marble_path.as_ref())?;
        let Some(tif_root) = normalized.parent().and_then(Path::parent) else {
            return Ok(None);
        };
        let west_path = absolute_normalized_path(&tif_root.join(Self::WEST_FILE))?;
        let east_path = absolute_normalized_path(&tif_root.join(Self::EAST_FILE))?;
        if !west_path.is_file() || !east_path.is_file() {
            return Ok(None);
        }
        Ok(Some(Self {
            west_path,
            east_path,
            tile_cache_entries: tile_cache_entries.max(1),
            readers: Mutex::new(HashMap::new()),
        }))
    }

    pub fn sample_nearest(&self, longitude: f64, latitude: f64) -> Result<RgbColor> {
        let lon = normalize_longitude(longitude);
        let lat = java_max(-90.0, java_min(90.0, latitude));
        let (path, min_longitude) = if lon < 0.0 {
            (&self.west_path, -Self::HALF_WORLD_DEGREES)
        } else {
            (&self.east_path, 0.0)
        };
        let reader = self.reader(path)?;
        let x = (((lon - min_longitude) / Self::HALF_WORLD_DEGREES) * f64::from(reader.width()))
            .floor() as i32;
        let y =
            (((90.0 - lat) / Self::HALF_WORLD_DEGREES) * f64::from(reader.height())).floor() as i32;
        reader
            .sample_pixel(
                clamp_i32(x, 0, reader.width() - 1),
                clamp_i32(y, 0, reader.height() - 1),
            )
            .map_err(Into::into)
    }

    pub fn sample_averaged(
        &self,
        longitude: f64,
        latitude: f64,
        longitude_span_degrees: f64,
        latitude_span_degrees: f64,
    ) -> Result<RgbColor> {
        let lon_offset = java_max(longitude_span_degrees.abs(), Self::DEFAULT_PIXEL_DEGREES) / 2.0;
        let lat_offset = java_max(latitude_span_degrees.abs(), Self::DEFAULT_PIXEL_DEGREES) / 2.0;
        let mut red = 0u64;
        let mut green = 0u64;
        let mut blue = 0u64;
        let mut samples = 0u64;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let color = self.sample_nearest(
                    longitude + (f64::from(dx) * lon_offset),
                    latitude + (f64::from(dz) * lat_offset),
                )?;
                if !color.available {
                    continue;
                }
                red += u64::from(color.red);
                green += u64::from(color.green);
                blue += u64::from(color.blue);
                samples += 1;
            }
        }
        if samples == 0 {
            return Ok(RgbColor::unavailable());
        }
        Ok(RgbColor::of(
            java_math_round_double_to_narrowed_i32(red as f64 / samples as f64) as u8,
            java_math_round_double_to_narrowed_i32(green as f64 / samples as f64) as u8,
            java_math_round_double_to_narrowed_i32(blue as f64 / samples as f64) as u8,
        ))
    }

    fn reader(&self, path: &Path) -> Result<Arc<GeoTiffRgbReader>> {
        let key = LandShallowReaderKey {
            path: path.to_path_buf(),
        };
        {
            let readers = self.readers.lock().map_err(|_| {
                SurfaceError::invalid("land-shallow topo reader cache lock poisoned")
            })?;
            if let Some(reader) = readers.get(&key) {
                return Ok(reader.clone());
            }
        }
        let loaded = Arc::new(GeoTiffRgbReader::open_with_tile_cache_entries(
            path,
            self.tile_cache_entries,
        )?);
        let mut readers = self
            .readers
            .lock()
            .map_err(|_| SurfaceError::invalid("land-shallow topo reader cache lock poisoned"))?;
        if let Some(existing) = readers.get(&key) {
            return Ok(existing.clone());
        }
        readers.insert(key, loaded.clone());
        Ok(loaded)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LandShallowReaderKey {
    path: PathBuf,
}

#[derive(Debug)]
pub struct MetImageExportTerrainSampler {
    tiles: Vec<MetTerrainTile>,
    tile_index: HashMap<i64, Vec<usize>>,
    images: Mutex<BoundedAccessCache<Arc<MetTerrainImage>>>,
}

impl MetImageExportTerrainSampler {
    const IMAGE_CACHE_ENTRIES: usize = 64;

    pub fn open_auto(true_marble_path: impl AsRef<Path>) -> Result<Option<Self>> {
        let mut tiles = Vec::new();
        for root in met_candidate_roots(true_marble_path.as_ref())? {
            add_met_image_export_root(&root.join("image_exports"), &mut tiles)?;
            add_met_image_export_root(&root.join("met_work").join("image_exports"), &mut tiles)?;
            add_nested_met_image_export_roots(&root.join("met_shards"), &mut tiles)?;
            add_nested_met_image_export_roots(&root.join("met_turbo_temp"), &mut tiles)?;
        }
        if tiles.is_empty() {
            return Ok(None);
        }
        tiles.sort_by(|left, right| {
            left.area_degrees()
                .partial_cmp(&right.area_degrees())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    left.path
                        .to_string_lossy()
                        .cmp(&right.path.to_string_lossy())
                })
        });
        let tile_index = build_met_tile_index(&tiles);
        Ok(Some(Self {
            tiles,
            tile_index,
            images: Mutex::new(BoundedAccessCache::new(Self::IMAGE_CACHE_ENTRIES)),
        }))
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    pub fn sample(&self, longitude: f64, latitude: f64) -> Result<RgbColor> {
        let Some(tile_index) = self.tile_for(longitude, latitude) else {
            return Ok(RgbColor::unavailable());
        };
        let tile = &self.tiles[tile_index];
        let image = self.image(tile_index)?;
        let pixel_x = clamp_i32(
            ((normalize_longitude(longitude) - tile.origin_longitude) / tile.pixel_longitude)
                .floor() as i32,
            0,
            image.width.saturating_sub(1) as i32,
        ) as usize;
        let pixel_y = clamp_i32(
            ((latitude - tile.origin_latitude) / tile.pixel_latitude).floor() as i32,
            0,
            image.height.saturating_sub(1) as i32,
        ) as usize;
        Ok(image.pixel(pixel_x, pixel_y))
    }

    fn tile_for(&self, longitude: f64, latitude: f64) -> Option<usize> {
        let lon = normalize_longitude(longitude);
        let candidates = self
            .tile_index
            .get(&met_index_key(lon.floor() as i32, latitude.floor() as i32))?;
        candidates
            .iter()
            .copied()
            .find(|index| self.tiles[*index].contains(lon, latitude))
    }

    fn image(&self, tile_index: usize) -> Result<Arc<MetTerrainImage>> {
        let key = tile_index as i64;
        {
            let mut cache = self
                .images
                .lock()
                .map_err(|_| SurfaceError::invalid("MET terrain image cache lock poisoned"))?;
            if let Some(cached) = cache.get(key) {
                return Ok(cached);
            }
        }
        let loaded = Arc::new(load_met_png_rgb(&self.tiles[tile_index].path)?);
        let mut cache = self
            .images
            .lock()
            .map_err(|_| SurfaceError::invalid("MET terrain image cache lock poisoned"))?;
        Ok(cache.insert_or_get(key, loaded))
    }
}

#[derive(Clone, Debug)]
struct MetTerrainTile {
    path: PathBuf,
    origin_longitude: f64,
    pixel_longitude: f64,
    origin_latitude: f64,
    pixel_latitude: f64,
    min_longitude: f64,
    max_longitude: f64,
    min_latitude: f64,
    max_latitude: f64,
}

impl MetTerrainTile {
    fn from_geo_transform(path: PathBuf, size: MetImageSize, transform: MetGeoTransform) -> Self {
        let other_longitude =
            transform.origin_longitude + (transform.pixel_longitude * size.width as f64);
        let other_latitude =
            transform.origin_latitude + (transform.pixel_latitude * size.height as f64);
        Self {
            path,
            origin_longitude: transform.origin_longitude,
            pixel_longitude: transform.pixel_longitude,
            origin_latitude: transform.origin_latitude,
            pixel_latitude: transform.pixel_latitude,
            min_longitude: transform.origin_longitude.min(other_longitude),
            max_longitude: transform.origin_longitude.max(other_longitude),
            min_latitude: transform.origin_latitude.min(other_latitude),
            max_latitude: transform.origin_latitude.max(other_latitude),
        }
    }

    fn area_degrees(&self) -> f64 {
        ((self.max_longitude - self.min_longitude) * (self.max_latitude - self.min_latitude)).abs()
    }

    fn contains(&self, longitude: f64, latitude: f64) -> bool {
        longitude >= self.min_longitude
            && longitude < self.max_longitude
            && latitude >= self.min_latitude
            && latitude < self.max_latitude
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MetImageSize {
    width: usize,
    height: usize,
}

#[derive(Clone, Copy, Debug)]
struct MetGeoTransform {
    origin_longitude: f64,
    pixel_longitude: f64,
    origin_latitude: f64,
    pixel_latitude: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MetTileOrigin {
    latitude_direction: u8,
    latitude: i32,
    longitude_direction: u8,
    longitude: i32,
}

#[derive(Debug)]
struct MetTerrainImage {
    width: usize,
    height: usize,
    pixels: Vec<RgbColor>,
}

impl MetTerrainImage {
    fn pixel(&self, x: usize, y: usize) -> RgbColor {
        self.pixels[y * self.width + x]
    }
}

fn add_nested_met_image_export_roots(parent: &Path, tiles: &mut Vec<MetTerrainTile>) -> Result<()> {
    if !parent.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(parent)? {
        let child = entry?.path();
        if child.is_dir() {
            add_met_image_export_root(&child.join("image_exports"), tiles)?;
        }
    }
    Ok(())
}

fn add_met_image_export_root(image_exports: &Path, tiles: &mut Vec<MetTerrainTile>) -> Result<()> {
    if !image_exports.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(image_exports)? {
        let tile_dir = entry?.path();
        if tile_dir.is_dir() {
            add_met_tile(&tile_dir, tiles)?;
        }
    }
    Ok(())
}

fn add_met_tile(tile_dir: &Path, tiles: &mut Vec<MetTerrainTile>) -> Result<()> {
    let Some(tile_name) = tile_dir.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let Some(origin) = parse_met_tile_name(tile_name) else {
        return Ok(());
    };
    let Some(image_path) = met_terrain_image_path(tile_dir, tile_name) else {
        return Ok(());
    };
    let Some(terrain_size) = met_png_image_size(&image_path)? else {
        return Ok(());
    };
    let exported_image_path = tile_dir
        .join("heightmap")
        .join(format!("{tile_name}_exported.png"));
    let aux_path = tile_dir
        .join("heightmap")
        .join(format!("{tile_name}_exported.png.aux.xml"));
    if aux_path.is_file() {
        if let Some(transform) = read_met_geo_transform(&aux_path)? {
            let exported_size = met_png_image_size(&exported_image_path)?;
            if exported_size.is_some_and(|size| size != terrain_size) {
                return Ok(());
            }
            tiles.push(MetTerrainTile::from_geo_transform(
                absolute_normalized_path(&image_path)?,
                terrain_size,
                transform,
            ));
            return Ok(());
        }
    }
    if let Some(tile) = infer_fallback_met_tile(&image_path, origin, terrain_size)? {
        tiles.push(tile);
    }
    Ok(())
}

fn met_terrain_image_path(tile_dir: &Path, tile_name: &str) -> Option<PathBuf> {
    [
        tile_dir.join(format!("{tile_name}_terrain_reduced_colors.png")),
        tile_dir.join("terrain_reduced_colors.png"),
        tile_dir.join(format!("{tile_name}_terrain.png")),
        tile_dir.join("terrain.png"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

fn infer_fallback_met_tile(
    image_path: &Path,
    origin: MetTileOrigin,
    terrain_size: MetImageSize,
) -> Result<Option<MetTerrainTile>> {
    if terrain_size.width != 512 || terrain_size.height != 512 {
        return Ok(None);
    }
    let west = if origin.longitude_direction == b'E' {
        f64::from(origin.longitude)
    } else {
        -f64::from(origin.longitude) - 1.0
    };
    let north = if origin.latitude_direction == b'N' {
        f64::from(origin.latitude) + 1.0
    } else {
        -f64::from(origin.latitude)
    };
    Ok(Some(MetTerrainTile::from_geo_transform(
        absolute_normalized_path(image_path)?,
        terrain_size,
        MetGeoTransform {
            origin_longitude: west,
            pixel_longitude: 1.0 / terrain_size.width as f64,
            origin_latitude: north,
            pixel_latitude: -1.0 / terrain_size.height as f64,
        },
    )))
}

fn met_png_image_size(image_path: &Path) -> Result<Option<MetImageSize>> {
    if !image_path.is_file() {
        return Ok(None);
    }
    let decoder = png::Decoder::new(File::open(image_path)?);
    let reader = decoder.read_info().map_err(|error| {
        SurfaceError::invalid(format!(
            "failed to read terrain token PNG metadata {}: {error}",
            image_path.display()
        ))
    })?;
    let info = reader.info();
    Ok(Some(MetImageSize {
        width: info.width as usize,
        height: info.height as usize,
    }))
}

fn load_met_png_rgb(image_path: &Path) -> Result<MetTerrainImage> {
    let mut decoder = png::Decoder::new(File::open(image_path)?);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|error| {
        SurfaceError::invalid(format!(
            "failed to decode terrain token PNG {}: {error}",
            image_path.display()
        ))
    })?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).map_err(|error| {
        SurfaceError::invalid(format!(
            "failed to read terrain token PNG {}: {error}",
            image_path.display()
        ))
    })?;
    let bytes = &buffer[..info.buffer_size()];
    let width = info.width as usize;
    let height = info.height as usize;
    let mut pixels = Vec::with_capacity(width.saturating_mul(height));
    match info.color_type {
        png::ColorType::Rgb => {
            for chunk in bytes.chunks_exact(3) {
                pixels.push(RgbColor::of(chunk[0], chunk[1], chunk[2]));
            }
        }
        png::ColorType::Rgba => {
            for chunk in bytes.chunks_exact(4) {
                pixels.push(RgbColor::of(chunk[0], chunk[1], chunk[2]));
            }
        }
        png::ColorType::Grayscale => {
            for gray in bytes {
                pixels.push(RgbColor::of(*gray, *gray, *gray));
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for chunk in bytes.chunks_exact(2) {
                pixels.push(RgbColor::of(chunk[0], chunk[0], chunk[0]));
            }
        }
        png::ColorType::Indexed => {
            return Err(SurfaceError::invalid(format!(
                "unsupported indexed terrain token PNG after expansion: {}",
                image_path.display()
            )));
        }
    }
    if pixels.len() != width.saturating_mul(height) {
        return Err(SurfaceError::invalid(format!(
            "unexpected terrain token PNG buffer size: {}",
            image_path.display()
        )));
    }
    Ok(MetTerrainImage {
        width,
        height,
        pixels,
    })
}

fn read_met_geo_transform(aux_path: &Path) -> Result<Option<MetGeoTransform>> {
    let xml = fs::read_to_string(aux_path)?;
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut capture = false;
    let mut capture_text = String::new();
    loop {
        match reader.read_event().map_err(|error| {
            SurfaceError::invalid(format!(
                "failed to parse terrain aux XML {}: {error}",
                aux_path.display()
            ))
        })? {
            Event::Start(element) if element.name().as_ref() == b"GeoTransform" => {
                capture = true;
                capture_text.clear();
            }
            Event::Text(text) if capture => {
                capture_text.push_str(&text.decode().map_err(|error| {
                    SurfaceError::invalid(format!(
                        "failed to decode terrain aux XML {}: {error}",
                        aux_path.display()
                    ))
                })?);
            }
            Event::CData(text) if capture => {
                capture_text.push_str(&text.decode().map_err(|error| {
                    SurfaceError::invalid(format!(
                        "failed to decode terrain aux XML {}: {error}",
                        aux_path.display()
                    ))
                })?);
            }
            Event::GeneralRef(reference) if capture => {
                capture_text.push_str(&decode_met_xml_general_ref(&reference, aux_path)?);
            }
            Event::End(end) if end.name().as_ref() == b"GeoTransform" && capture => {
                return parse_met_geo_transform(&capture_text, aux_path);
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

fn decode_met_xml_general_ref(
    reference: &quick_xml::events::BytesRef<'_>,
    aux_path: &Path,
) -> Result<String> {
    let name = reference.decode().map_err(|error| {
        SurfaceError::invalid(format!(
            "failed to decode terrain aux XML {}: {error}",
            aux_path.display()
        ))
    })?;
    match name.as_ref() {
        "amp" => Ok("&".to_string()),
        "lt" => Ok("<".to_string()),
        "gt" => Ok(">".to_string()),
        "quot" => Ok("\"".to_string()),
        "apos" => Ok("'".to_string()),
        text if text.starts_with("#x") || text.starts_with("#X") => {
            let codepoint = u32::from_str_radix(&text[2..], 16).map_err(|_| {
                SurfaceError::invalid(format!(
                    "unrecognized terrain aux XML entity {} in {}",
                    text,
                    aux_path.display()
                ))
            })?;
            char::from_u32(codepoint)
                .map(|ch| ch.to_string())
                .ok_or_else(|| {
                    SurfaceError::invalid(format!(
                        "unrecognized terrain aux XML entity {} in {}",
                        text,
                        aux_path.display()
                    ))
                })
        }
        text if text.starts_with('#') => {
            let codepoint = text[1..].parse::<u32>().map_err(|_| {
                SurfaceError::invalid(format!(
                    "unrecognized terrain aux XML entity {} in {}",
                    text,
                    aux_path.display()
                ))
            })?;
            char::from_u32(codepoint)
                .map(|ch| ch.to_string())
                .ok_or_else(|| {
                    SurfaceError::invalid(format!(
                        "unrecognized terrain aux XML entity {} in {}",
                        text,
                        aux_path.display()
                    ))
                })
        }
        text => Err(SurfaceError::invalid(format!(
            "unrecognized terrain aux XML entity {} in {}",
            text,
            aux_path.display()
        ))),
    }
}

fn parse_met_geo_transform(text: &str, aux_path: &Path) -> Result<Option<MetGeoTransform>> {
    let parts = text.trim().split(',').collect::<Vec<_>>();
    if parts.len() != 6 {
        return Ok(None);
    }
    let parse_part = |index: usize| {
        parts[index].trim().parse::<f64>().map_err(|error| {
            SurfaceError::invalid(format!(
                "failed to parse terrain aux GeoTransform {}: {error}",
                aux_path.display()
            ))
        })
    };
    let origin_longitude = parse_part(0)?;
    let pixel_longitude = parse_part(1)?;
    let rotation_x = parse_part(2)?;
    let origin_latitude = parse_part(3)?;
    let rotation_y = parse_part(4)?;
    let pixel_latitude = parse_part(5)?;
    if rotation_x != 0.0 || rotation_y != 0.0 || pixel_longitude == 0.0 || pixel_latitude == 0.0 {
        return Ok(None);
    }
    Ok(Some(MetGeoTransform {
        origin_longitude,
        pixel_longitude,
        origin_latitude,
        pixel_latitude,
    }))
}

fn parse_met_tile_name(tile_name: &str) -> Option<MetTileOrigin> {
    let bytes = tile_name.as_bytes();
    if bytes.len() != 7 {
        return None;
    }
    let latitude_direction = bytes[0];
    let longitude_direction = bytes[3];
    if !matches!(latitude_direction, b'N' | b'S') || !matches!(longitude_direction, b'E' | b'W') {
        return None;
    }
    if !bytes[1..3].iter().all(u8::is_ascii_digit) || !bytes[4..7].iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(MetTileOrigin {
        latitude_direction,
        latitude: tile_name[1..3].parse().ok()?,
        longitude_direction,
        longitude: tile_name[4..7].parse().ok()?,
    })
}

fn met_candidate_roots(true_marble_path: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    let normalized = absolute_normalized_path(true_marble_path)?;
    if let Some(earth_root) = normalized
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
    {
        push_distinct_path(&mut roots, earth_root.to_path_buf());
    }
    for root in ["E:/earthmap", "D:/earthmap", "F:/earthmap"] {
        push_distinct_path(&mut roots, absolute_normalized_path(Path::new(root))?);
    }
    Ok(roots)
}

fn build_met_tile_index(tiles: &[MetTerrainTile]) -> HashMap<i64, Vec<usize>> {
    let mut index = HashMap::<i64, Vec<usize>>::new();
    for (tile_index, tile) in tiles.iter().enumerate() {
        let min_lon = tile.min_longitude.floor() as i32;
        let max_lon = next_down_f64(tile.max_longitude).floor() as i32;
        let min_lat = tile.min_latitude.floor() as i32;
        let max_lat = next_down_f64(tile.max_latitude).floor() as i32;
        for lat in min_lat..=max_lat {
            for lon in min_lon..=max_lon {
                index
                    .entry(met_index_key(lon, lat))
                    .or_default()
                    .push(tile_index);
            }
        }
    }
    index
}

fn met_index_key(longitude_cell: i32, latitude_cell: i32) -> i64 {
    (i64::from(longitude_cell) << 32) ^ (i64::from(latitude_cell) & 0xffff_ffff)
}

fn next_down_f64(value: f64) -> f64 {
    if value.is_nan() || value == f64::NEG_INFINITY {
        return value;
    }
    if value == 0.0 {
        return -f64::from_bits(1);
    }
    let bits = value.to_bits();
    if value > 0.0 {
        f64::from_bits(bits - 1)
    } else {
        f64::from_bits(bits + 1)
    }
}

fn absolute_normalized_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(normalize_path_components(&absolute))
}

fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[derive(Clone, Debug, PartialEq)]
struct EcoregionEvidence {
    sample: EcoregionSample,
    confidence: f64,
}

impl EcoregionEvidence {
    fn new(sample: EcoregionSample, confidence: f64) -> Self {
        Self { sample, confidence }
    }

    fn unknown() -> Self {
        Self::new(EcoregionSample::unknown(), 0.0)
    }
}

trait EcoregionSampler: fmt::Debug + Send + Sync {
    fn sample(&self, longitude: f64, latitude: f64) -> EcoregionSample;
}

#[derive(Debug)]
pub struct WwfEcoregionSampler {
    polygons: Vec<WwfEcoregionPolygon>,
    grid: Vec<Vec<usize>>,
    last_hit: Mutex<Option<usize>>,
}

impl WwfEcoregionSampler {
    const CACHE_MAGIC: i32 = 0x4552_4731;
    const CACHE_VERSION: i32 = 2;
    const MIN_LONGITUDE: f64 = -180.0;
    const MIN_LATITUDE: f64 = -90.0;
    const CELL_DEGREES: f64 = 0.5;
    const GRID_WIDTH: usize = 720;
    const GRID_HEIGHT: usize = 360;

    pub fn open_cache(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        let mut input = BufReader::new(file);
        Self::read_cache(&mut input)
    }

    pub fn open(shape_path: impl AsRef<Path>, mapping_csv: impl AsRef<Path>) -> Result<Self> {
        let shape_path = shape_path.as_ref();
        let mapping_csv = mapping_csv.as_ref();
        let dbf_path = shape_path.with_extension("dbf");
        if !shape_path.is_file() || !dbf_path.is_file() || !mapping_csv.is_file() {
            return Err(SurfaceError::invalid(
                "WWF ecoregion shapefile or mapping CSV is missing",
            ));
        }
        let biome_by_ecoregion = read_wwf_biome_mapping(mapping_csv)?;
        let names = read_wwf_ecoregion_names(&dbf_path)?;
        let polygons = read_wwf_polygons(shape_path, &names, &biome_by_ecoregion)?;
        let grid = Self::build_grid(&polygons);
        Ok(Self {
            polygons,
            grid,
            last_hit: Mutex::new(None),
        })
    }

    fn open_auto_cache(true_marble_path: &Path) -> Result<Option<Self>> {
        for root in wwf_candidate_roots(true_marble_path) {
            let shape = root
                .join("ShapeFiles")
                .join("ecoregionsOrig")
                .join("wwf_terr_ecos.shp");
            let dbf = shape.with_extension("dbf");
            let mapping = root.join("ecoregions.csv");
            let cache = root
                .join(".earthmap-cache")
                .join(format!("wwf-ecoregions-v{}.bin", Self::CACHE_VERSION));
            if !shape.is_file() || !dbf.is_file() || !mapping.is_file() {
                continue;
            }
            if let Ok(sampler) = Self::open_cached(&shape, &mapping, &cache) {
                return Ok(Some(sampler));
            }
        }
        Ok(None)
    }

    fn open_cached(shape_path: &Path, mapping_csv: &Path, cache_path: &Path) -> Result<Self> {
        let dbf_path = shape_path.with_extension("dbf");
        if is_fresh_wwf_cache(
            cache_path,
            &[
                shape_path.to_path_buf(),
                dbf_path,
                mapping_csv.to_path_buf(),
            ],
        ) {
            if let Ok(sampler) = Self::open_cache(cache_path) {
                return Ok(sampler);
            }
        }
        let sampler = Self::open(shape_path, mapping_csv)?;
        let _ = sampler.write_cache(cache_path);
        Ok(sampler)
    }

    pub fn sample(&self, longitude: f64, latitude: f64) -> EcoregionSample {
        self.sample_internal(longitude, latitude)
    }

    fn read_cache(input: &mut impl Read) -> Result<Self> {
        if read_i32_be(input)? != Self::CACHE_MAGIC || read_i32_be(input)? != Self::CACHE_VERSION {
            return Err(SurfaceError::invalid("unsupported ecoregion cache version"));
        }
        let polygon_count = read_nonnegative_usize(input, "ecoregion polygon count")?;
        let mut polygons = Vec::with_capacity(polygon_count);
        for _ in 0..polygon_count {
            let min_x = read_f64_be(input)?;
            let min_y = read_f64_be(input)?;
            let max_x = read_f64_be(input)?;
            let max_y = read_f64_be(input)?;
            let parts = read_usize_array_be(input, "ecoregion polygon parts")?;
            let xs = read_f64_array_be(input, "ecoregion polygon x coordinates")?;
            let ys = read_f64_array_be(input, "ecoregion polygon y coordinates")?;
            if parts.len() < 2 || xs.len() != ys.len() {
                return Err(SurfaceError::invalid("malformed ecoregion polygon"));
            }
            let point_count = xs.len();
            if parts
                .windows(2)
                .any(|pair| pair[0] > pair[1] || pair[1] > point_count)
            {
                return Err(SurfaceError::invalid(
                    "ecoregion polygon part index outside point array",
                ));
            }
            let name = read_java_utf(input)?;
            let biome = read_java_utf(input)?;
            polygons.push(WwfEcoregionPolygon {
                min_x,
                min_y,
                max_x,
                max_y,
                parts,
                xs,
                ys,
                sample: EcoregionSample::new(name, biome),
            });
        }
        let grid_length = read_i32_be(input)?;
        if grid_length != (Self::GRID_WIDTH * Self::GRID_HEIGHT) as i32 {
            return Err(SurfaceError::invalid("ecoregion cache grid size mismatch"));
        }
        let mut grid = Vec::with_capacity(Self::GRID_WIDTH * Self::GRID_HEIGHT);
        for _ in 0..(Self::GRID_WIDTH * Self::GRID_HEIGHT) {
            let cell = read_usize_array_be(input, "ecoregion grid cell")?;
            if cell.iter().any(|&candidate| candidate >= polygons.len()) {
                return Err(SurfaceError::invalid(
                    "ecoregion grid candidate outside polygon array",
                ));
            }
            grid.push(cell);
        }
        Ok(Self {
            polygons,
            grid,
            last_hit: Mutex::new(None),
        })
    }

    fn write_cache(&self, cache_path: &Path) -> Result<()> {
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = BufWriter::new(File::create(cache_path)?);
        write_i32_be(&mut output, Self::CACHE_MAGIC)?;
        write_i32_be(&mut output, Self::CACHE_VERSION)?;
        write_i32_be(
            &mut output,
            usize_to_i32(self.polygons.len(), "ecoregion polygon count")?,
        )?;
        for polygon in &self.polygons {
            write_f64_be(&mut output, polygon.min_x)?;
            write_f64_be(&mut output, polygon.min_y)?;
            write_f64_be(&mut output, polygon.max_x)?;
            write_f64_be(&mut output, polygon.max_y)?;
            write_usize_array_be(&mut output, &polygon.parts)?;
            write_f64_array_be(&mut output, &polygon.xs)?;
            write_f64_array_be(&mut output, &polygon.ys)?;
            write_java_utf(&mut output, &polygon.sample.name)?;
            write_java_utf(&mut output, &polygon.sample.biome_id)?;
        }
        write_i32_be(
            &mut output,
            usize_to_i32(self.grid.len(), "ecoregion grid length")?,
        )?;
        for cell in &self.grid {
            write_usize_array_be(&mut output, cell)?;
        }
        Ok(())
    }

    fn sample_internal(&self, longitude: f64, latitude: f64) -> EcoregionSample {
        if longitude.is_nan() || latitude.is_nan() || !(-90.0..=90.0).contains(&latitude) {
            return EcoregionSample::unknown();
        }
        let lon = normalize_longitude(longitude);
        if let Ok(mut last_hit) = self.last_hit.try_lock() {
            if let Some(previous) = *last_hit {
                if self
                    .polygons
                    .get(previous)
                    .is_some_and(|polygon| polygon.contains(lon, latitude))
                {
                    return self.polygons[previous].sample.clone();
                }
            }
            let cell = self.cell_index(lon, latitude);
            let Some(candidates) = self.grid.get(cell) else {
                *last_hit = None;
                return EcoregionSample::unknown();
            };
            for &candidate in candidates {
                let polygon = &self.polygons[candidate];
                if polygon.contains(lon, latitude) {
                    *last_hit = Some(candidate);
                    return polygon.sample.clone();
                }
            }
            *last_hit = None;
            return EcoregionSample::unknown();
        }
        self.sample_grid_candidates(lon, latitude)
    }

    fn sample_grid_candidates(&self, longitude: f64, latitude: f64) -> EcoregionSample {
        let cell = self.cell_index(longitude, latitude);
        let Some(candidates) = self.grid.get(cell) else {
            return EcoregionSample::unknown();
        };
        for &candidate in candidates {
            let polygon = &self.polygons[candidate];
            if polygon.contains(longitude, latitude) {
                return polygon.sample.clone();
            }
        }
        EcoregionSample::unknown()
    }

    fn cell_index(&self, longitude: f64, latitude: f64) -> usize {
        (Self::grid_y(latitude) * Self::GRID_WIDTH) + Self::grid_x(longitude)
    }

    fn grid_x(longitude: f64) -> usize {
        let raw = ((normalize_longitude(longitude) - Self::MIN_LONGITUDE) / Self::CELL_DEGREES)
            .floor() as i32;
        clamp_i32(raw, 0, (Self::GRID_WIDTH - 1) as i32) as usize
    }

    fn grid_y(latitude: f64) -> usize {
        let raw = ((latitude - Self::MIN_LATITUDE) / Self::CELL_DEGREES).floor() as i32;
        clamp_i32(raw, 0, (Self::GRID_HEIGHT - 1) as i32) as usize
    }

    fn build_grid(polygons: &[WwfEcoregionPolygon]) -> Vec<Vec<usize>> {
        let mut cells = vec![Vec::<usize>::new(); Self::GRID_WIDTH * Self::GRID_HEIGHT];
        for (index, polygon) in polygons.iter().enumerate() {
            let min_x = Self::grid_x(polygon.min_x);
            let max_x = Self::grid_x(polygon.max_x);
            let min_y = Self::grid_y(polygon.min_y);
            let max_y = Self::grid_y(polygon.max_y);
            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    cells[(y * Self::GRID_WIDTH) + x].push(index);
                }
            }
        }
        cells
    }
}

impl EcoregionSampler for WwfEcoregionSampler {
    fn sample(&self, longitude: f64, latitude: f64) -> EcoregionSample {
        self.sample_internal(longitude, latitude)
    }
}

#[derive(Debug)]
struct WwfEcoregionPolygon {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    parts: Vec<usize>,
    xs: Vec<f64>,
    ys: Vec<f64>,
    sample: EcoregionSample,
}

impl WwfEcoregionPolygon {
    fn contains(&self, x: f64, y: f64) -> bool {
        if x < self.min_x || x > self.max_x || y < self.min_y || y > self.max_y {
            return false;
        }
        let mut inside = false;
        for part in 0..self.parts.len() - 1 {
            let start = self.parts[part];
            let end = self.parts[part + 1];
            if start >= end {
                continue;
            }
            let mut j = end - 1;
            for i in start..end {
                let yi = self.ys[i];
                let yj = self.ys[j];
                if (yi > y) != (yj > y) {
                    let xi = self.xs[i];
                    let xj = self.xs[j];
                    let intersection_x = ((xj - xi) * (y - yi) / (yj - yi)) + xi;
                    if x < intersection_x {
                        inside = !inside;
                    }
                }
                j = i;
            }
        }
        inside
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WwfDbfField {
    name: String,
    offset: usize,
    length: usize,
}

fn read_wwf_biome_mapping(mapping_csv: &Path) -> Result<HashMap<String, String>> {
    let text = fs::read_to_string(mapping_csv)?;
    let mut mappings = HashMap::new();
    for line in text.lines() {
        if java_string_is_blank(line) {
            continue;
        }
        let columns = parse_wwf_csv_line(line);
        if columns.len() < 2 {
            continue;
        }
        let name = java_string_trim(&columns[0]).to_string();
        let biome = wwf_minecraft_biome_id(java_string_trim(&columns[1]));
        if !name.is_empty() && !biome.is_empty() {
            mappings.insert(name, biome);
        }
    }
    Ok(mappings)
}

fn read_wwf_ecoregion_names(dbf_path: &Path) -> Result<Vec<String>> {
    let mut file = BufReader::new(File::open(dbf_path)?);
    let mut header = [0u8; 32];
    file.read_exact(&mut header)?;
    let record_count = little_i32(&header, 4);
    if record_count < 0 {
        return Err(SurfaceError::invalid("negative DBF record count"));
    }
    let header_length = usize::from(little_u16(&header, 8));
    let record_length = usize::from(little_u16(&header, 10));
    if header_length < 33 || record_length == 0 {
        return Err(SurfaceError::invalid("malformed DBF header"));
    }
    let mut fields = Vec::new();
    let mut offset = 1usize;
    while file.stream_position()? < header_length.saturating_sub(1) as u64 {
        let mut descriptor = [0u8; 32];
        file.read_exact(&mut descriptor)?;
        if descriptor[0] == 0x0d {
            break;
        }
        let name = decode_wwf_dbf_text(&descriptor, 0, 11)?;
        let length = usize::from(descriptor[16]);
        fields.push(WwfDbfField {
            name,
            offset,
            length,
        });
        offset = offset
            .checked_add(length)
            .ok_or_else(|| SurfaceError::invalid("DBF field offset overflow"))?;
    }
    let eco_name = fields
        .iter()
        .find(|field| field.name.eq_ignore_ascii_case("ECO_NAME"))
        .ok_or_else(|| {
            SurfaceError::invalid(format!("ECO_NAME field missing in {}", dbf_path.display()))
        })?;
    file.seek(SeekFrom::Start(header_length as u64))?;
    let mut names = Vec::with_capacity(record_count as usize);
    let mut record = vec![0u8; record_length];
    for _ in 0..record_count {
        file.read_exact(&mut record)?;
        if record[0] == b'*' {
            names.push(String::new());
        } else {
            names.push(decode_wwf_dbf_text(
                &record,
                eco_name.offset,
                eco_name.length,
            )?);
        }
    }
    Ok(names)
}

fn read_wwf_polygons(
    shape_path: &Path,
    names: &[String],
    biome_by_ecoregion: &HashMap<String, String>,
) -> Result<Vec<WwfEcoregionPolygon>> {
    let file = File::open(shape_path)?;
    let file_length = file.metadata()?.len();
    let mut file = BufReader::new(file);
    file.seek(SeekFrom::Start(100))?;
    let mut record_index = 0usize;
    let mut polygons = Vec::with_capacity(names.len());
    while file.stream_position()? < file_length {
        let remaining = file_length.saturating_sub(file.stream_position()?);
        if remaining < 8 {
            break;
        }
        let _record_number = read_i32_be(&mut file)?;
        let content_words = read_i32_be(&mut file)?;
        if content_words < 0 {
            return Err(SurfaceError::invalid("negative shapefile record length"));
        }
        let content_bytes = i64::from(content_words)
            .checked_mul(2)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| SurfaceError::invalid("shapefile record length overflow"))?;
        let record_end = file
            .stream_position()?
            .checked_add(content_bytes)
            .ok_or_else(|| SurfaceError::invalid("shapefile record end overflow"))?;
        let shape_type = read_i32_le(&mut file)?;
        if shape_type == 0 {
            file.seek(SeekFrom::Start(record_end))?;
            record_index += 1;
            continue;
        }
        if shape_type != 5 {
            return Err(SurfaceError::invalid(format!(
                "Unsupported ecoregion shape type {shape_type}"
            )));
        }
        let min_x = read_f64_le(&mut file)?;
        let min_y = read_f64_le(&mut file)?;
        let max_x = read_f64_le(&mut file)?;
        let max_y = read_f64_le(&mut file)?;
        let part_count = read_i32_le(&mut file)?;
        let point_count = read_i32_le(&mut file)?;
        if part_count < 0 || point_count < 0 {
            return Err(SurfaceError::invalid("negative shapefile polygon counts"));
        }
        let part_count = usize::try_from(part_count)
            .map_err(|_| SurfaceError::invalid("shapefile part count overflow"))?;
        let point_count = usize::try_from(point_count)
            .map_err(|_| SurfaceError::invalid("shapefile point count overflow"))?;
        let mut parts = Vec::with_capacity(part_count + 1);
        for _ in 0..part_count {
            let part = read_i32_le(&mut file)?;
            if part < 0 {
                return Err(SurfaceError::invalid("negative shapefile part index"));
            }
            parts.push(
                usize::try_from(part)
                    .map_err(|_| SurfaceError::invalid("shapefile part index overflow"))?,
            );
        }
        parts.push(point_count);
        if parts.len() < 2
            || parts
                .windows(2)
                .any(|pair| pair[0] > pair[1] || pair[1] > point_count)
        {
            return Err(SurfaceError::invalid("malformed shapefile polygon parts"));
        }
        let mut xs = Vec::with_capacity(point_count);
        let mut ys = Vec::with_capacity(point_count);
        for _ in 0..point_count {
            xs.push(read_f64_le(&mut file)?);
            ys.push(read_f64_le(&mut file)?);
        }
        let name = names.get(record_index).cloned().unwrap_or_default();
        let biome = biome_by_ecoregion.get(&name).cloned().unwrap_or_default();
        polygons.push(WwfEcoregionPolygon {
            min_x,
            min_y,
            max_x,
            max_y,
            parts,
            xs,
            ys,
            sample: EcoregionSample::new(name, biome),
        });
        file.seek(SeekFrom::Start(record_end))?;
        record_index += 1;
    }
    Ok(polygons)
}

fn parse_wwf_csv_line(line: &str) -> Vec<String> {
    let mut columns = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                current.push(ch);
            }
        } else if ch == '"' {
            quoted = true;
        } else if ch == ',' {
            columns.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    columns.push(current);
    columns
}

fn wwf_minecraft_biome_id(csv_biome: &str) -> String {
    if java_string_is_blank(csv_biome) {
        return String::new();
    }
    let key = match java_string_trim(csv_biome).to_ascii_uppercase().as_str() {
        "DESERT_LAKES" => "DESERT".to_string(),
        "COLD_BEACH" => "SNOWY_BEACH".to_string(),
        other => other.to_string(),
    };
    format!("minecraft:{}", key.to_ascii_lowercase())
}

fn decode_wwf_dbf_text(data: &[u8], offset: usize, length: usize) -> Result<String> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| SurfaceError::invalid("DBF text range overflow"))?;
    let slice = data
        .get(offset..end)
        .ok_or_else(|| SurfaceError::invalid("DBF text range outside record"))?;
    let text = slice
        .iter()
        .map(|&byte| {
            if byte == 0 {
                ' '
            } else {
                windows_1252_char(byte)
            }
        })
        .collect::<String>();
    Ok(java_string_trim(&text).to_string())
}

fn java_string_trim(value: &str) -> &str {
    let Some((start, _)) = value.char_indices().find(|&(_, ch)| ch as u32 > 0x20) else {
        return "";
    };
    let end = value
        .char_indices()
        .rev()
        .find(|&(_, ch)| ch as u32 > 0x20)
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(start);
    &value[start..end]
}

fn java_string_is_blank(value: &str) -> bool {
    value.chars().all(java_character_is_whitespace)
}

fn java_character_is_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'..='\u{000d}'
            | '\u{001c}'..='\u{0020}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

fn system_time_to_java_millis(time: std::time::SystemTime) -> Option<i128> {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => {
            Some((i128::from(duration.as_secs()) * 1_000) + i128::from(duration.subsec_millis()))
        }
        Err(error) => {
            let duration = error.duration();
            let millis = (i128::from(duration.as_secs()) * 1_000)
                + i128::from((duration.subsec_nanos() + 999_999) / 1_000_000);
            Some(-millis)
        }
    }
}

fn file_modified_java_millis(path: &Path) -> Option<i128> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    system_time_to_java_millis(metadata.modified().ok()?)
}

fn windows_1252_char(byte: u8) -> char {
    match byte {
        0x80 => '\u{20ac}',
        0x81 | 0x8d | 0x8f | 0x90 | 0x9d => '\u{fffd}',
        0x82 => '\u{201a}',
        0x83 => '\u{0192}',
        0x84 => '\u{201e}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02c6}',
        0x89 => '\u{2030}',
        0x8a => '\u{0160}',
        0x8b => '\u{2039}',
        0x8c => '\u{0152}',
        0x8e => '\u{017d}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201c}',
        0x94 => '\u{201d}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02dc}',
        0x99 => '\u{2122}',
        0x9a => '\u{0161}',
        0x9b => '\u{203a}',
        0x9c => '\u{0153}',
        0x9e => '\u{017e}',
        0x9f => '\u{0178}',
        _ => char::from(byte),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceMaterialRasterStats {
    pub source_count: usize,
    pub open_readers: usize,
    pub resident_tiles: u64,
    pub tile_hits: u64,
    pub tile_misses: u64,
    pub tile_evictions: u64,
    pub sample_nearest_requests: u64,
    pub sample_averaged_requests: u64,
}

impl SurfaceMaterialRasterStats {
    pub const EMPTY: Self = Self {
        source_count: 0,
        open_readers: 0,
        resident_tiles: 0,
        tile_hits: 0,
        tile_misses: 0,
        tile_evictions: 0,
        sample_nearest_requests: 0,
        sample_averaged_requests: 0,
    };

    pub fn minus(self, previous: Self) -> Self {
        Self {
            source_count: self.source_count,
            open_readers: self.open_readers,
            resident_tiles: self.resident_tiles.saturating_sub(previous.resident_tiles),
            tile_hits: self.tile_hits.saturating_sub(previous.tile_hits),
            tile_misses: self.tile_misses.saturating_sub(previous.tile_misses),
            tile_evictions: self.tile_evictions.saturating_sub(previous.tile_evictions),
            sample_nearest_requests: self
                .sample_nearest_requests
                .saturating_sub(previous.sample_nearest_requests),
            sample_averaged_requests: self
                .sample_averaged_requests
                .saturating_sub(previous.sample_averaged_requests),
        }
    }
}

pub struct MetTerrainVocabulary;

impl MetTerrainVocabulary {
    pub fn nearest(color: RgbColor) -> MetTerrainMatch {
        if !color.available {
            return MetTerrainMatch::unavailable();
        }
        let Some(entry_index) = nearest_image_magick_remap(color) else {
            return MetTerrainMatch::unavailable();
        };
        let entry = MET_TERRAIN_ENTRIES[entry_index];
        MetTerrainMatch::from_entry(entry, met_terrain_distance_squared(color, entry))
    }

    pub fn exact(color: RgbColor) -> MetTerrainMatch {
        if !color.available {
            return MetTerrainMatch::unavailable();
        }
        for entry in MET_TERRAIN_ENTRIES {
            if color.red == entry.red && color.green == entry.green && color.blue == entry.blue {
                return MetTerrainMatch::from_entry(entry, 0);
            }
        }
        MetTerrainMatch::unavailable()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetTerrainKind {
    Vegetated,
    Sand,
    RedSand,
    CoarseDirt,
    Gravel,
    Rock,
    Snow,
    Wet,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetTerrainMatch {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub top_block_state_id: i32,
    pub kind: MetTerrainKind,
    pub distance_squared: i32,
}

impl MetTerrainMatch {
    pub fn unavailable() -> Self {
        Self {
            red: 0,
            green: 0,
            blue: 0,
            top_block_state_id: block_state_ids::GRASS_BLOCK,
            kind: MetTerrainKind::Unknown,
            distance_squared: i32::MAX,
        }
    }

    pub fn confident(self) -> bool {
        self.distance_squared <= MET_TERRAIN_CONFIDENT_DISTANCE_SQUARED
    }

    pub fn color(self) -> RgbColor {
        if self.kind == MetTerrainKind::Unknown {
            RgbColor::unavailable()
        } else {
            RgbColor::of(self.red, self.green, self.blue)
        }
    }

    fn from_entry(entry: MetTerrainEntry, distance_squared: i32) -> Self {
        Self {
            red: entry.red,
            green: entry.green,
            blue: entry.blue,
            top_block_state_id: entry.top_block_state_id,
            kind: entry.kind,
            distance_squared,
        }
    }
}

pub fn with_surface_terrain_token(
    sample: &SurfaceMaterialSample,
    exported_terrain_token_color: RgbColor,
) -> SurfaceMaterialSample {
    let mut terrain_token_color = exported_terrain_token_color;
    let mut terrain_token_source = if exported_terrain_token_color.available {
        TerrainTokenSource::Export
    } else {
        TerrainTokenSource::None
    };
    if !terrain_token_color.available && sample.color.available {
        let synthetic = MetTerrainVocabulary::nearest(sample.color);
        if synthetic.kind != MetTerrainKind::Unknown {
            terrain_token_color = synthetic.color();
            terrain_token_source = TerrainTokenSource::JavaStandardPalette;
        }
    }
    if !terrain_token_color.available && !sample.terrain_token_color.available {
        return sample.clone();
    }
    SurfaceMaterialSample::new(
        sample.color,
        terrain_token_color,
        terrain_token_source,
        sample.climate_class,
        sample.evergreen_broadleaf_trees,
        sample.deciduous_broadleaf_trees,
        sample.needleleaf_trees,
        sample.mixed_trees,
        sample.herbaceous_vegetation,
        sample.shrubs,
        sample.snow_cover,
        sample.swamp_cover,
        sample.ocean_temperature,
        sample.bathymetry_meters,
        sample.slope_permille,
        sample.ecoregion_name.clone(),
        sample.ecoregion_biome_id.clone(),
        sample.ecoregion_confidence,
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceQuantizedCell {
    pub key: i64,
    pub center_longitude: f64,
    pub center_latitude: f64,
}

pub fn normalized_slope_permille(raw_value: f64) -> i32 {
    if !raw_value.is_finite() || raw_value < 0.0 {
        return SurfaceMaterialSample::UNKNOWN;
    }
    let ratio = if raw_value <= 1.0 {
        raw_value
    } else if raw_value <= 90.0 {
        raw_value / 90.0
    } else {
        let percent_slope = raw_value / 100.0;
        let rise_over_run = percent_slope / 100.0;
        rise_over_run.atan() / (std::f64::consts::PI / 2.0)
    };
    java_math_round_double_to_narrowed_i32(java_clamp_unit(ratio) * 1000.0)
}

pub fn normalize_longitude(longitude: f64) -> f64 {
    if !longitude.is_finite() {
        return longitude;
    }
    let mut lon = longitude;
    while lon < -180.0 {
        lon += 360.0;
    }
    while lon >= 180.0 {
        lon -= 360.0;
    }
    lon
}

pub fn material_cell_degrees(longitude_span_degrees: f64, latitude_span_degrees: f64) -> f64 {
    java_max(
        0.012,
        java_min(
            0.060,
            java_max(longitude_span_degrees.abs(), latitude_span_degrees.abs()) * 2.0,
        ),
    )
}

pub fn photo_cell_degrees(longitude_span_degrees: f64, latitude_span_degrees: f64) -> f64 {
    java_max(
        0.0030,
        java_min(
            0.050,
            java_max(longitude_span_degrees.abs(), latitude_span_degrees.abs()) * 2.75,
        ),
    )
}

pub fn photo_average_span_degrees(span_degrees: f64) -> f64 {
    java_max(0.0030, java_min(0.060, span_degrees.abs() * 2.75))
}

pub fn prefers_topographic_photo_source() -> bool {
    let property = std::env::var("earthmap.photoSource").unwrap_or_default();
    let environment = std::env::var("EARTHMAP_PHOTO_SOURCE").unwrap_or_default();
    photo_source_prefers_topographic(&property, &environment)
}

pub fn photo_source_prefers_topographic(property: &str, environment: &str) -> bool {
    let value = if property.trim().is_empty() {
        environment
    } else {
        property
    };
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "topo" | "topographic" | "land-shallow-topo" | "land_shallow_topo"
    )
}

fn usable_photo_color(color: RgbColor) -> bool {
    color.available && !color.is_near_black()
}

pub fn photo_evidence_cell_degrees(longitude_span_degrees: f64, latitude_span_degrees: f64) -> f64 {
    java_max(
        0.030,
        java_min(
            0.090,
            java_max(longitude_span_degrees.abs(), latitude_span_degrees.abs()) * 5.0,
        ),
    )
}

pub fn ecoregion_cell_degrees(longitude_span_degrees: f64, latitude_span_degrees: f64) -> f64 {
    java_max(
        0.012,
        java_min(
            0.045,
            java_max(longitude_span_degrees.abs(), latitude_span_degrees.abs()) * 1.6,
        ),
    )
}

pub fn quantized_cell(longitude: f64, latitude: f64, cell_degrees: f64) -> SurfaceQuantizedCell {
    let lon = normalize_longitude(longitude);
    let lat = java_max(-90.0, java_min(90.0, latitude));
    let lon_cell = ((lon + 180.0) / cell_degrees).floor() as i32;
    let lat_cell = ((lat + 90.0) / cell_degrees).floor() as i32;
    let cell_code = java_math_round_double_to_narrowed_i32(cell_degrees * 10_000.0);
    let key = (i64::from(cell_code) << 48)
        ^ ((i64::from(lon_cell) & 0x00ff_ffff) << 24)
        ^ (i64::from(lat_cell) & 0x00ff_ffff);
    let center_longitude = -180.0 + ((f64::from(lon_cell) + 0.5) * cell_degrees);
    let center_latitude = -90.0 + ((f64::from(lat_cell) + 0.5) * cell_degrees);
    SurfaceQuantizedCell {
        key,
        center_longitude,
        center_latitude,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PhotoPaletteEntry {
    block_state_id: i32,
    red: u8,
    green: u8,
    blue: u8,
}

impl PhotoPaletteEntry {
    const fn new(block_state_id: i32, red: u8, green: u8, blue: u8) -> Self {
        Self {
            block_state_id,
            red,
            green,
            blue,
        }
    }
}

const PHOTO_PALETTE_ENTRIES: [PhotoPaletteEntry; 50] = [
    PhotoPaletteEntry::new(block_state_ids::GRASS_BLOCK, 92, 133, 62),
    PhotoPaletteEntry::new(block_state_ids::OAK_LEAVES, 48, 89, 43),
    PhotoPaletteEntry::new(block_state_ids::JUNGLE_LEAVES, 36, 97, 36),
    PhotoPaletteEntry::new(block_state_ids::DARK_OAK_LEAVES, 26, 58, 28),
    PhotoPaletteEntry::new(block_state_ids::SPRUCE_LEAVES, 55, 82, 49),
    PhotoPaletteEntry::new(block_state_ids::MOSS_BLOCK, 75, 112, 41),
    PhotoPaletteEntry::new(block_state_ids::PODZOL, 94, 64, 36),
    PhotoPaletteEntry::new(block_state_ids::COARSE_DIRT, 112, 92, 58),
    PhotoPaletteEntry::new(block_state_ids::DIRT, 115, 82, 48),
    PhotoPaletteEntry::new(block_state_ids::ROOTED_DIRT, 123, 91, 57),
    PhotoPaletteEntry::new(block_state_ids::MUD, 72, 64, 54),
    PhotoPaletteEntry::new(block_state_ids::PACKED_MUD, 142, 106, 79),
    PhotoPaletteEntry::new(block_state_ids::MYCELIUM, 111, 99, 85),
    PhotoPaletteEntry::new(block_state_ids::SAND, 218, 202, 142),
    PhotoPaletteEntry::new(block_state_ids::SANDSTONE, 213, 191, 121),
    PhotoPaletteEntry::new(block_state_ids::END_STONE, 221, 214, 164),
    PhotoPaletteEntry::new(block_state_ids::END_STONE_BRICKS, 216, 207, 163),
    PhotoPaletteEntry::new(block_state_ids::SMOOTH_SANDSTONE, 216, 195, 137),
    PhotoPaletteEntry::new(block_state_ids::CUT_SANDSTONE, 214, 190, 121),
    PhotoPaletteEntry::new(block_state_ids::CHISELED_SANDSTONE, 215, 192, 129),
    PhotoPaletteEntry::new(block_state_ids::RED_SAND, 181, 97, 45),
    PhotoPaletteEntry::new(block_state_ids::SMOOTH_RED_SANDSTONE, 181, 101, 57),
    PhotoPaletteEntry::new(block_state_ids::CUT_RED_SANDSTONE, 166, 91, 50),
    PhotoPaletteEntry::new(block_state_ids::CHISELED_RED_SANDSTONE, 179, 98, 54),
    PhotoPaletteEntry::new(block_state_ids::MUD_BRICKS, 137, 107, 78),
    PhotoPaletteEntry::new(block_state_ids::DRIPSTONE_BLOCK, 138, 106, 89),
    PhotoPaletteEntry::new(block_state_ids::YELLOW_TERRACOTTA, 186, 133, 36),
    PhotoPaletteEntry::new(block_state_ids::ORANGE_TERRACOTTA, 184, 92, 42),
    PhotoPaletteEntry::new(block_state_ids::TERRACOTTA, 154, 102, 76),
    PhotoPaletteEntry::new(block_state_ids::BROWN_TERRACOTTA, 104, 66, 48),
    PhotoPaletteEntry::new(block_state_ids::RED_TERRACOTTA, 143, 61, 47),
    PhotoPaletteEntry::new(block_state_ids::WHITE_TERRACOTTA, 210, 178, 161),
    PhotoPaletteEntry::new(block_state_ids::LIGHT_GRAY_TERRACOTTA, 135, 107, 98),
    PhotoPaletteEntry::new(block_state_ids::GREEN_TERRACOTTA, 76, 83, 42),
    PhotoPaletteEntry::new(block_state_ids::LIME_TERRACOTTA, 104, 117, 53),
    PhotoPaletteEntry::new(block_state_ids::CYAN_TERRACOTTA, 86, 91, 91),
    PhotoPaletteEntry::new(block_state_ids::STONE, 118, 122, 118),
    PhotoPaletteEntry::new(block_state_ids::ANDESITE, 136, 136, 136),
    PhotoPaletteEntry::new(block_state_ids::GRANITE, 149, 103, 85),
    PhotoPaletteEntry::new(block_state_ids::DIORITE, 188, 188, 182),
    PhotoPaletteEntry::new(block_state_ids::TUFF, 108, 109, 103),
    PhotoPaletteEntry::new(block_state_ids::GRAVEL, 112, 112, 106),
    PhotoPaletteEntry::new(block_state_ids::CLAY, 145, 158, 160),
    PhotoPaletteEntry::new(block_state_ids::DEEPSLATE, 79, 79, 82),
    PhotoPaletteEntry::new(block_state_ids::GRAY_TERRACOTTA, 57, 41, 35),
    PhotoPaletteEntry::new(block_state_ids::BLACK_TERRACOTTA, 37, 23, 16),
    PhotoPaletteEntry::new(block_state_ids::BONE_BLOCK, 229, 224, 195),
    PhotoPaletteEntry::new(block_state_ids::QUARTZ_BLOCK, 236, 229, 220),
    PhotoPaletteEntry::new(block_state_ids::CALCITE, 224, 220, 204),
    PhotoPaletteEntry::new(block_state_ids::SNOW_BLOCK, 232, 238, 236),
];

const MET_TERRAIN_CONFIDENT_DISTANCE_SQUARED: i32 = 2_200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MetTerrainEntry {
    red: u8,
    green: u8,
    blue: u8,
    top_block_state_id: i32,
    kind: MetTerrainKind,
}

impl MetTerrainEntry {
    const fn new(
        red: u8,
        green: u8,
        blue: u8,
        top_block_state_id: i32,
        kind: MetTerrainKind,
    ) -> Self {
        Self {
            red,
            green,
            blue,
            top_block_state_id,
            kind,
        }
    }
}

const MET_TERRAIN_ENTRIES: [MetTerrainEntry; 42] = [
    MetTerrainEntry::new(0, 0, 0, block_state_ids::PODZOL, MetTerrainKind::Vegetated),
    MetTerrainEntry::new(20, 20, 20, block_state_ids::MUD, MetTerrainKind::Wet),
    MetTerrainEntry::new(
        50,
        60,
        30,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        0,
        50,
        0,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        75,
        85,
        60,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        55,
        70,
        50,
        block_state_ids::MOSS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        50,
        150,
        50,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        50,
        200,
        50,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        95,
        100,
        75,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        100,
        140,
        110,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        140,
        150,
        110,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        155,
        160,
        110,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(64, 64, 64, block_state_ids::STONE, MetTerrainKind::Rock),
    MetTerrainEntry::new(
        163,
        142,
        232,
        block_state_ids::MYCELIUM,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        192,
        192,
        192,
        block_state_ids::SNOW_BLOCK,
        MetTerrainKind::Snow,
    ),
    MetTerrainEntry::new(
        100,
        80,
        50,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        100,
        85,
        60,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        100,
        90,
        75,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(
        255,
        255,
        220,
        block_state_ids::SNOW_BLOCK,
        MetTerrainKind::Snow,
    ),
    MetTerrainEntry::new(255, 200, 128, block_state_ids::SAND, MetTerrainKind::Sand),
    MetTerrainEntry::new(255, 200, 64, block_state_ids::SAND, MetTerrainKind::Sand),
    MetTerrainEntry::new(255, 255, 190, block_state_ids::SAND, MetTerrainKind::Sand),
    MetTerrainEntry::new(230, 205, 160, block_state_ids::SAND, MetTerrainKind::Sand),
    MetTerrainEntry::new(
        167,
        146,
        103,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(166, 152, 126, block_state_ids::SAND, MetTerrainKind::Sand),
    MetTerrainEntry::new(173, 143, 115, block_state_ids::SAND, MetTerrainKind::Sand),
    MetTerrainEntry::new(155, 127, 103, block_state_ids::SAND, MetTerrainKind::Sand),
    MetTerrainEntry::new(164, 135, 91, block_state_ids::SAND, MetTerrainKind::Sand),
    MetTerrainEntry::new(
        158,
        144,
        117,
        block_state_ids::GRAVEL,
        MetTerrainKind::Gravel,
    ),
    MetTerrainEntry::new(
        149,
        134,
        103,
        block_state_ids::GRASS_BLOCK,
        MetTerrainKind::Vegetated,
    ),
    MetTerrainEntry::new(190, 150, 120, block_state_ids::SAND, MetTerrainKind::Sand),
    MetTerrainEntry::new(
        190,
        130,
        80,
        block_state_ids::RED_SAND,
        MetTerrainKind::RedSand,
    ),
    MetTerrainEntry::new(
        170,
        105,
        60,
        block_state_ids::RED_SAND,
        MetTerrainKind::RedSand,
    ),
    MetTerrainEntry::new(
        255,
        0,
        0,
        block_state_ids::RED_SAND,
        MetTerrainKind::RedSand,
    ),
    MetTerrainEntry::new(
        128,
        50,
        0,
        block_state_ids::RED_SAND,
        MetTerrainKind::RedSand,
    ),
    MetTerrainEntry::new(
        140,
        80,
        50,
        block_state_ids::COARSE_DIRT,
        MetTerrainKind::CoarseDirt,
    ),
    MetTerrainEntry::new(
        255,
        255,
        255,
        block_state_ids::SNOW_BLOCK,
        MetTerrainKind::Snow,
    ),
    MetTerrainEntry::new(
        110,
        150,
        170,
        block_state_ids::SNOW_BLOCK,
        MetTerrainKind::Snow,
    ),
    MetTerrainEntry::new(
        25,
        50,
        110,
        block_state_ids::SNOW_BLOCK,
        MetTerrainKind::Snow,
    ),
    MetTerrainEntry::new(
        230,
        255,
        230,
        block_state_ids::SNOW_BLOCK,
        MetTerrainKind::Snow,
    ),
    MetTerrainEntry::new(
        240,
        255,
        240,
        block_state_ids::SNOW_BLOCK,
        MetTerrainKind::Snow,
    ),
    MetTerrainEntry::new(
        250,
        255,
        250,
        block_state_ids::SNOW_BLOCK,
        MetTerrainKind::Snow,
    ),
];

#[derive(Clone, Debug)]
struct MetOctreeNode {
    parent: Option<usize>,
    children: [Option<usize>; 8],
    entry: Option<usize>,
}

impl MetOctreeNode {
    fn new(parent: Option<usize>) -> Self {
        Self {
            parent,
            children: [None; 8],
            entry: None,
        }
    }
}

fn nearest_image_magick_remap(color: RgbColor) -> Option<usize> {
    let nodes = build_met_remap_tree();
    let rgb = met_rgb(color);
    let mut node = 0usize;
    for bit in (1..=7).rev() {
        let child = nodes[node].children[met_node_id(rgb, bit)];
        let Some(child) = child else {
            break;
        };
        node = child;
    }
    let search_root = nodes[node].parent.unwrap_or(node);
    let mut closest = MetClosestMatch {
        entry: None,
        distance_squared: i32::MAX,
    };
    find_met_closest(&nodes, search_root, color, &mut closest);
    closest.entry
}

fn nearest_photo_palette_entry(color: RgbColor) -> PhotoPaletteEntry {
    let mut best = PHOTO_PALETTE_ENTRIES[0];
    let mut best_distance = photo_palette_distance_squared(color, best);
    for &entry in PHOTO_PALETTE_ENTRIES.iter().skip(1) {
        let distance = photo_palette_distance_squared(color, entry);
        if distance < best_distance {
            best = entry;
            best_distance = distance;
        }
    }
    best
}

fn photo_palette_distance_squared(color: RgbColor, entry: PhotoPaletteEntry) -> i32 {
    let dr = i32::from(color.red) - i32::from(entry.red);
    let dg = i32::from(color.green) - i32::from(entry.green);
    let db = i32::from(color.blue) - i32::from(entry.blue);
    (dr * dr) + (dg * dg) + (db * db)
}

fn build_met_remap_tree() -> Vec<MetOctreeNode> {
    let mut nodes = vec![MetOctreeNode::new(None)];
    for (entry_index, entry) in MET_TERRAIN_ENTRIES.iter().enumerate() {
        let rgb = met_entry_rgb(*entry);
        let mut node = 0usize;
        for bit in (0..=7).rev() {
            let id = met_node_id(rgb, bit);
            let child = if let Some(child) = nodes[node].children[id] {
                child
            } else {
                let child = nodes.len();
                nodes.push(MetOctreeNode::new(Some(node)));
                nodes[node].children[id] = Some(child);
                child
            };
            node = child;
        }
        nodes[node].entry = Some(entry_index);
    }
    nodes
}

fn find_met_closest(
    nodes: &[MetOctreeNode],
    node_index: usize,
    color: RgbColor,
    closest: &mut MetClosestMatch,
) {
    for child in nodes[node_index].children.into_iter().flatten() {
        find_met_closest(nodes, child, color, closest);
    }
    let Some(entry_index) = nodes[node_index].entry else {
        return;
    };
    let distance_squared = met_terrain_distance_squared(color, MET_TERRAIN_ENTRIES[entry_index]);
    if distance_squared <= closest.distance_squared {
        closest.entry = Some(entry_index);
        closest.distance_squared = distance_squared;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MetClosestMatch {
    entry: Option<usize>,
    distance_squared: i32,
}

fn met_node_id(rgb: i32, bit: i32) -> usize {
    let red = (rgb >> (16 + bit)) & 1;
    let green = ((rgb >> (8 + bit)) & 1) << 1;
    let blue = ((rgb >> bit) & 1) << 2;
    (red | green | blue) as usize
}

fn met_rgb(color: RgbColor) -> i32 {
    (i32::from(color.red) << 16) | (i32::from(color.green) << 8) | i32::from(color.blue)
}

fn met_entry_rgb(entry: MetTerrainEntry) -> i32 {
    (i32::from(entry.red) << 16) | (i32::from(entry.green) << 8) | i32::from(entry.blue)
}

fn met_terrain_distance_squared(color: RgbColor, entry: MetTerrainEntry) -> i32 {
    let red = i32::from(color.red) - i32::from(entry.red);
    let green = i32::from(color.green) - i32::from(entry.green);
    let blue = i32::from(color.blue) - i32::from(entry.blue);
    (red * red) + (green * green) + (blue * blue)
}

fn sample_ecoregion_evidence(
    ecoregions: &dyn EcoregionSampler,
    longitude: f64,
    latitude: f64,
    cell_degrees: f64,
) -> EcoregionEvidence {
    let center = ecoregions.sample(longitude, latitude);
    if !center.has_biome() {
        return EcoregionEvidence::unknown();
    }
    let offset = 0.45_f64.max(cell_degrees * 10.0);
    let mut total = 0;
    let mut same_family = 0;
    let center_family = biome_family(&center.biome_id);
    for dz in -1..=1 {
        let sample_latitude = latitude + (f64::from(dz) * offset);
        if sample_latitude < -90.0 || sample_latitude > 90.0 {
            continue;
        }
        for dx in -1..=1 {
            let neighbor = ecoregions.sample(longitude + (f64::from(dx) * offset), sample_latitude);
            if !neighbor.has_biome() {
                continue;
            }
            total += 1;
            if center_family == biome_family(&neighbor.biome_id) {
                same_family += 1;
            }
        }
    }
    let confidence = if total == 0 {
        1.0
    } else {
        f64::from(same_family) / f64::from(total)
    };
    EcoregionEvidence::new(center, confidence)
}

fn biome_family(biome: &str) -> &str {
    if biome.contains("desert") {
        "desert"
    } else if biome.contains("badlands") {
        "badlands"
    } else if biome.contains("savanna") {
        "savanna"
    } else if biome.contains("jungle") {
        "jungle"
    } else if biome.contains("forest") {
        "forest"
    } else if biome.contains("swamp") {
        "swamp"
    } else if biome.contains("taiga") {
        "taiga"
    } else if biome.contains("snow") || biome.contains("frozen") {
        "snow"
    } else if biome.contains("beach") {
        "beach"
    } else if biome.contains("plains") || biome.contains("meadow") {
        "grassland"
    } else {
        biome
    }
}

fn wwf_candidate_roots(true_marble_path: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::with_capacity(4);
    let terrain = true_marble_path
        .canonicalize()
        .unwrap_or_else(|_| true_marble_path.to_path_buf())
        .parent()
        .map(Path::to_path_buf);
    let tif_root = terrain.as_ref().and_then(|path| path.parent());
    if let Some(root) = tif_root.and_then(|path| path.parent()) {
        push_distinct_path(&mut roots, root.to_path_buf());
    }
    push_distinct_path(&mut roots, PathBuf::from("E:/earthmap"));
    push_distinct_path(&mut roots, PathBuf::from("D:/earthmap"));
    push_distinct_path(&mut roots, PathBuf::from("F:/earthmap"));
    roots
}

fn push_distinct_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn is_fresh_wwf_cache(cache: &Path, sources: &[PathBuf]) -> bool {
    let Some(cache_modified) = file_modified_java_millis(cache) else {
        return false;
    };
    for source in sources {
        let Some(source_modified) = file_modified_java_millis(source) else {
            return false;
        };
        if source_modified > cache_modified {
            return false;
        }
    }
    true
}

fn read_nonnegative_usize(input: &mut impl Read, label: &str) -> Result<usize> {
    let value = read_i32_be(input)?;
    if value < 0 {
        return Err(SurfaceError::invalid(format!("negative {label}")));
    }
    Ok(value as usize)
}

fn read_usize_array_be(input: &mut impl Read, label: &str) -> Result<Vec<usize>> {
    let length = read_nonnegative_usize(input, label)?;
    let mut values = Vec::with_capacity(length);
    for _ in 0..length {
        values.push(read_nonnegative_usize(input, label)?);
    }
    Ok(values)
}

fn read_f64_array_be(input: &mut impl Read, label: &str) -> Result<Vec<f64>> {
    let length = read_nonnegative_usize(input, label)?;
    let mut values = Vec::with_capacity(length);
    for _ in 0..length {
        values.push(read_f64_be(input)?);
    }
    Ok(values)
}

fn read_java_utf(input: &mut impl Read) -> Result<String> {
    let length = usize::from(read_u16_be(input)?);
    let mut bytes = vec![0u8; length];
    input.read_exact(&mut bytes)?;
    decode_modified_utf8(&bytes)
}

fn decode_modified_utf8(bytes: &[u8]) -> Result<String> {
    let mut code_units = Vec::<u16>::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte >> 7 == 0 {
            code_units.push(u16::from(byte));
            index += 1;
        } else if byte >> 5 == 0b110 {
            if index + 1 >= bytes.len() {
                return Err(SurfaceError::invalid("truncated modified UTF-8 sequence"));
            }
            let byte2 = bytes[index + 1];
            if byte2 >> 6 != 0b10 {
                return Err(SurfaceError::invalid("invalid modified UTF-8 continuation"));
            }
            code_units.push((u16::from(byte & 0x1f) << 6) | u16::from(byte2 & 0x3f));
            index += 2;
        } else if byte >> 4 == 0b1110 {
            if index + 2 >= bytes.len() {
                return Err(SurfaceError::invalid("truncated modified UTF-8 sequence"));
            }
            let byte2 = bytes[index + 1];
            let byte3 = bytes[index + 2];
            if byte2 >> 6 != 0b10 || byte3 >> 6 != 0b10 {
                return Err(SurfaceError::invalid("invalid modified UTF-8 continuation"));
            }
            code_units.push(
                (u16::from(byte & 0x0f) << 12)
                    | (u16::from(byte2 & 0x3f) << 6)
                    | u16::from(byte3 & 0x3f),
            );
            index += 3;
        } else {
            return Err(SurfaceError::invalid("invalid modified UTF-8 leading byte"));
        }
    }
    String::from_utf16(&code_units)
        .map_err(|error| SurfaceError::invalid(format!("invalid modified UTF-8 string: {error}")))
}

fn read_i32_be(input: &mut impl Read) -> Result<i32> {
    let mut bytes = [0u8; 4];
    input.read_exact(&mut bytes)?;
    Ok(i32::from_be_bytes(bytes))
}

fn read_u16_be(input: &mut impl Read) -> Result<u16> {
    let mut bytes = [0u8; 2];
    input.read_exact(&mut bytes)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_f64_be(input: &mut impl Read) -> Result<f64> {
    let mut bytes = [0u8; 8];
    input.read_exact(&mut bytes)?;
    Ok(f64::from_be_bytes(bytes))
}

fn read_i32_le(input: &mut impl Read) -> Result<i32> {
    let mut bytes = [0u8; 4];
    input.read_exact(&mut bytes)?;
    Ok(i32::from_le_bytes(bytes))
}

fn read_f64_le(input: &mut impl Read) -> Result<f64> {
    let mut bytes = [0u8; 8];
    input.read_exact(&mut bytes)?;
    Ok(f64::from_le_bytes(bytes))
}

fn write_i32_be(output: &mut impl Write, value: i32) -> Result<()> {
    output.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn write_f64_be(output: &mut impl Write, value: f64) -> Result<()> {
    output.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn write_usize_array_be(output: &mut impl Write, values: &[usize]) -> Result<()> {
    write_i32_be(output, usize_to_i32(values.len(), "array length")?)?;
    for &value in values {
        write_i32_be(output, usize_to_i32(value, "array value")?)?;
    }
    Ok(())
}

fn write_f64_array_be(output: &mut impl Write, values: &[f64]) -> Result<()> {
    write_i32_be(output, usize_to_i32(values.len(), "double array length")?)?;
    for &value in values {
        write_f64_be(output, value)?;
    }
    Ok(())
}

fn write_java_utf(output: &mut impl Write, value: &str) -> Result<()> {
    let bytes = modified_utf8_bytes(value);
    if bytes.len() > usize::from(u16::MAX) {
        return Err(SurfaceError::invalid("modified UTF-8 string is too long"));
    }
    output.write_all(&(bytes.len() as u16).to_be_bytes())?;
    output.write_all(&bytes)?;
    Ok(())
}

fn modified_utf8_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len());
    for code_unit in value.encode_utf16() {
        match code_unit {
            0x0001..=0x007f => bytes.push(code_unit as u8),
            0x0000..=0x07ff => {
                bytes.push((0xc0 | ((code_unit >> 6) & 0x1f)) as u8);
                bytes.push((0x80 | (code_unit & 0x3f)) as u8);
            }
            _ => {
                bytes.push((0xe0 | ((code_unit >> 12) & 0x0f)) as u8);
                bytes.push((0x80 | ((code_unit >> 6) & 0x3f)) as u8);
                bytes.push((0x80 | (code_unit & 0x3f)) as u8);
            }
        }
    }
    bytes
}

fn usize_to_i32(value: usize, label: &str) -> Result<i32> {
    i32::try_from(value).map_err(|_| SurfaceError::invalid(format!("{label} exceeds i32 range")))
}

fn little_i32(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn little_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn open_surface_raster(
    directory: Option<&Path>,
    name: &str,
    tile_cache_entries: usize,
) -> Result<Option<GeoTiffSingleBandReader>> {
    for candidate_directory in surface_candidate_directories(directory) {
        let candidate = candidate_directory.join(name);
        match GeoTiffSingleBandReader::open_if_present(
            &candidate,
            tile_cache_entries.max(EarthDataSurfaceMaterialSampler::DEFAULT_CACHE_BLOCKS),
        ) {
            Ok(Some(reader)) => return Ok(Some(reader)),
            Ok(None) | Err(_) => {}
        }
    }
    Ok(None)
}

fn open_surface_float_raster(
    directory: Option<&Path>,
    name: &str,
) -> Result<Option<GeoTiffFloat32Reader>> {
    for candidate_directory in surface_candidate_directories(directory) {
        let candidate = candidate_directory.join(name);
        match GeoTiffFloat32Reader::open_if_present(&candidate) {
            Ok(Some(reader)) => return Ok(Some(reader)),
            Ok(None) | Err(_) => {}
        }
    }
    Ok(None)
}

fn surface_candidate_directories(directory: Option<&Path>) -> Vec<PathBuf> {
    let vegetation = directory
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("vegetation"));
    let relative = if vegetation {
        PathBuf::from("TifFiles").join("vegetation")
    } else {
        PathBuf::from("TifFiles")
    };
    let mut candidates = Vec::with_capacity(4);
    if let Some(directory) = directory {
        candidates.push(directory.to_path_buf());
    }
    candidates.push(PathBuf::from("E:/earthmap").join(&relative));
    candidates.push(PathBuf::from("D:/earthmap").join(&relative));
    candidates.push(PathBuf::from("F:/earthmap").join(relative));
    candidates
}

fn sample_rounded(reader: Option<&GeoTiffSingleBandReader>, longitude: f64, latitude: f64) -> i32 {
    let Some(reader) = reader else {
        return SurfaceMaterialSample::UNKNOWN;
    };
    match reader.sample_nearest(longitude, latitude) {
        Ok(value) => SurfaceMaterialSample::rounded(value),
        Err(_) => SurfaceMaterialSample::UNKNOWN,
    }
}

fn sample_slope_permille(
    reader: Option<&GeoTiffFloat32Reader>,
    longitude: f64,
    latitude: f64,
) -> i32 {
    let Some(reader) = reader else {
        return SurfaceMaterialSample::UNKNOWN;
    };
    match reader.sample_nearest(longitude, latitude) {
        Ok(Some(value)) => normalized_slope_permille(value),
        Ok(None) | Err(_) => SurfaceMaterialSample::UNKNOWN,
    }
}

fn sample_bathymetry_meters(
    reader: Option<&GeoTiffFloat32Reader>,
    longitude: f64,
    latitude: f64,
) -> i32 {
    let Some(reader) = reader else {
        return SurfaceMaterialSample::UNKNOWN;
    };
    match reader.sample_nearest(longitude, latitude) {
        Ok(Some(value)) if value.is_finite() => SurfaceMaterialSample::rounded(Some(value)),
        Ok(Some(_)) | Ok(None) | Err(_) => SurfaceMaterialSample::UNKNOWN,
    }
}

pub fn generate_height_only_region(
    settings: &HeightOnlySettings,
) -> Result<HeightOnlyRegionReport> {
    std::fs::create_dir_all(&settings.world_dir)?;
    std::fs::create_dir_all(settings.world_dir.join("region"))?;

    let reader = GeoTiffHeightmapReader::open(&settings.heightmap_path)?;
    let mapping = mapping_for(reader.metadata(), settings.scale_denominator)?;
    let cache = GeoTiffRowCache::new(&reader, settings.cache_rows)?;
    let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);

    let spawn_x = settings
        .region_x
        .wrapping_mul(REGION_SIZE_BLOCKS)
        .wrapping_add(REGION_SIZE_BLOCKS / 2);
    let spawn_z = settings
        .region_z
        .wrapping_mul(REGION_SIZE_BLOCKS)
        .wrapping_add(REGION_SIZE_BLOCKS / 2);
    let level_settings = level_dat_template::Settings::new(
        settings.level_name.clone(),
        settings.seed,
        spawn_x,
        SEA_LEVEL_Y + 10,
        spawn_z,
    )?;
    level_dat_template::write(settings.world_dir.join("level.dat"), &level_settings)?;

    let mut chunks = BTreeMap::new();
    let mut min_surface_y = i32::MAX;
    let mut max_surface_y = i32::MIN;
    for local_chunk_z in 0..REGION_CHUNKS {
        for local_chunk_x in 0..REGION_CHUNKS {
            let chunk_x = settings
                .region_x
                .wrapping_mul(REGION_CHUNKS)
                .wrapping_add(local_chunk_x);
            let chunk_z = settings
                .region_z
                .wrapping_mul(REGION_CHUNKS)
                .wrapping_add(local_chunk_z);
            let chunk_build = height_only_chunk(chunk_x, chunk_z, &mapping, &sampler)?;
            min_surface_y = min_surface_y.min(chunk_build.min_surface_y);
            max_surface_y = max_surface_y.max(chunk_build.max_surface_y);
            chunks.insert(
                ChunkLocalPos::new(local_chunk_x as u8, local_chunk_z as u8)?,
                chunk_nbt_encoder::encode_to_bytes(&chunk_build.chunk, 0)?,
            );
        }
    }

    let region_file = region_file(
        &settings.world_dir,
        settings.output_format,
        settings.region_x,
        settings.region_z,
    );
    write_region_file(settings.output_format, &region_file, &chunks)?;
    write_exploration_only_manifest(settings)?;

    Ok(HeightOnlyRegionReport {
        region_x: settings.region_x,
        region_z: settings.region_z,
        output_format: settings.output_format,
        scale_denominator: settings.scale_denominator,
        chunk_count: chunks.len(),
        min_surface_y,
        max_surface_y,
        region_file,
        cache_stats: cache.stats(),
    })
}

pub fn generate_surface_region(settings: &SurfaceRegionSettings) -> Result<SurfaceRegionReport> {
    let surface_material_sampler = if settings.texture_mode == SurfaceTextureMode::Photo {
        settings
            .surface_material_path
            .as_ref()
            .map(|path| {
                EarthDataSurfaceMaterialSampler::open_with_tile_cache_entries(
                    path,
                    settings.surface_tile_cache_entries,
                )
            })
            .transpose()?
    } else {
        None
    };
    generate_surface_region_with_open_material_sampler(settings, surface_material_sampler.as_ref())
}

pub fn generate_surface_region_with_open_material_sampler(
    settings: &SurfaceRegionSettings,
    surface_material_sampler: Option<&EarthDataSurfaceMaterialSampler>,
) -> Result<SurfaceRegionReport> {
    fs::create_dir_all(&settings.world_dir)?;
    fs::create_dir_all(settings.world_dir.join("region"))?;

    let reader = GeoTiffHeightmapReader::open(&settings.heightmap_path)?;
    let mapping = mapping_for(reader.metadata(), settings.scale_denominator)?;
    let cache = GeoTiffRowCache::new(&reader, settings.cache_rows)?;
    let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);
    let total_start = Instant::now();

    let mut metadata_nanos = 0;
    if settings.write_world_metadata {
        let phase_start = Instant::now();
        let spawn_x = settings
            .region_x
            .wrapping_mul(REGION_SIZE_BLOCKS)
            .wrapping_add(REGION_SIZE_BLOCKS / 2);
        let spawn_z = settings
            .region_z
            .wrapping_mul(REGION_SIZE_BLOCKS)
            .wrapping_add(REGION_SIZE_BLOCKS / 2);
        let level_settings = level_dat_template::Settings::new(
            settings.level_name.clone(),
            settings.seed,
            spawn_x,
            SEA_LEVEL_Y + 10,
            spawn_z,
        )?;
        level_dat_template::write(settings.world_dir.join("level.dat"), &level_settings)?;
        metadata_nanos += phase_start.elapsed().as_nanos();
    }

    let phase_start = Instant::now();
    let region_surface = sample_surface_region_scaled_with_material_sampler(
        settings.region_x,
        settings.region_z,
        &mapping,
        &sampler,
        settings.vertical_scale,
        settings.texture_mode,
        surface_material_sampler.map(|sampler| sampler as &dyn SurfaceMaterialSampler),
        settings.parallel_column_sampling,
    )?;
    let surface_sample_nanos = phase_start.elapsed().as_nanos();
    let surface_material_raster_stats = surface_material_sampler
        .map(EarthDataSurfaceMaterialSampler::raster_stats)
        .unwrap_or(SurfaceMaterialRasterStats::EMPTY);

    struct SurfaceChunkPayloadBuild {
        local_chunk_x: i32,
        local_chunk_z: i32,
        land_columns: i32,
        water_columns: i32,
        min_ground_y: i32,
        max_ground_y: i32,
        chunk_build_nanos: u128,
        nbt_encode_nanos: u128,
        payload: Vec<u8>,
    }

    let mut chunk_payloads = (0..(REGION_CHUNKS * REGION_CHUNKS))
        .into_par_iter()
        .map(|chunk_index| -> Result<SurfaceChunkPayloadBuild> {
            let local_chunk_x = chunk_index % REGION_CHUNKS;
            let local_chunk_z = chunk_index / REGION_CHUNKS;
            let chunk_x = settings
                .region_x
                .wrapping_mul(REGION_CHUNKS)
                .wrapping_add(local_chunk_x);
            let chunk_z = settings
                .region_z
                .wrapping_mul(REGION_CHUNKS)
                .wrapping_add(local_chunk_z);
            let sample = region_surface.chunk_sample(local_chunk_x, local_chunk_z)?;

            let phase_start = Instant::now();
            let build = build_surface_chunk(chunk_x, chunk_z, sample.columns())?;
            let chunk_build_nanos = phase_start.elapsed().as_nanos();

            let phase_start = Instant::now();
            let status = surface_chunk_status_for(&build, settings.chunk_status);
            let payload = chunk_nbt_encoder::encode_to_bytes_with_status(&build.chunk, 0, status)?;
            let nbt_encode_nanos = phase_start.elapsed().as_nanos();

            Ok(SurfaceChunkPayloadBuild {
                local_chunk_x,
                local_chunk_z,
                land_columns: build.land_columns,
                water_columns: build.water_columns,
                min_ground_y: build.min_ground_y,
                max_ground_y: build.max_ground_y,
                chunk_build_nanos,
                nbt_encode_nanos,
                payload,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    chunk_payloads.sort_by_key(|chunk| (chunk.local_chunk_z, chunk.local_chunk_x));

    let mut chunks = BTreeMap::new();
    let mut land_columns = 0;
    let mut water_columns = 0;
    let mut min_ground_y = i32::MAX;
    let mut max_ground_y = i32::MIN;
    let mut chunk_build_nanos = 0;
    let mut nbt_encode_nanos = 0;
    for chunk in chunk_payloads {
        land_columns += chunk.land_columns;
        water_columns += chunk.water_columns;
        min_ground_y = min_ground_y.min(chunk.min_ground_y);
        max_ground_y = max_ground_y.max(chunk.max_ground_y);
        chunk_build_nanos += chunk.chunk_build_nanos;
        nbt_encode_nanos += chunk.nbt_encode_nanos;

        chunks.insert(
            ChunkLocalPos::new(chunk.local_chunk_x as u8, chunk.local_chunk_z as u8)?,
            chunk.payload,
        );
    }

    let region_file = region_file(
        &settings.world_dir,
        settings.output_format,
        settings.region_x,
        settings.region_z,
    );
    let phase_start = Instant::now();
    write_region_with_settings(settings, &region_file, &chunks)?;
    let region_write_nanos = phase_start.elapsed().as_nanos();

    if settings.write_world_metadata {
        let phase_start = Instant::now();
        write_surface_region_manifest(settings)?;
        metadata_nanos += phase_start.elapsed().as_nanos();
    }

    Ok(SurfaceRegionReport {
        region_x: settings.region_x,
        region_z: settings.region_z,
        output_format: settings.output_format,
        scale_denominator: settings.scale_denominator,
        chunk_count: chunks.len(),
        land_columns,
        water_columns,
        min_ground_y,
        max_ground_y,
        region_file,
        preview_tile_file: None,
        cache_stats: cache.stats(),
        surface_material_raster_stats,
        surface_sample_nanos,
        chunk_build_nanos,
        nbt_encode_nanos,
        region_write_nanos,
        preview_nanos: 0,
        metadata_nanos,
        total_nanos: total_start.elapsed().as_nanos(),
    })
}

pub fn trace_surface_region_columns(
    settings: &SurfaceRegionSettings,
    local_columns: &[(usize, usize)],
) -> Result<Vec<SurfaceRegionColumnTrace>> {
    if local_columns.is_empty() {
        return Err(SurfaceError::invalid(
            "at least one local region column is required",
        ));
    }
    let mut selected = HashMap::with_capacity(local_columns.len());
    for (request_index, &(local_x, local_z)) in local_columns.iter().enumerate() {
        if local_x >= SURFACE_REGION_WIDTH || local_z >= SURFACE_REGION_WIDTH {
            return Err(SurfaceError::invalid(format!(
                "local region column outside region: {local_x},{local_z}"
            )));
        }
        let column_index = (local_z * SURFACE_REGION_WIDTH) + local_x;
        if selected.insert(column_index, request_index).is_some() {
            return Err(SurfaceError::invalid(format!(
                "duplicate local region column: {local_x},{local_z}"
            )));
        }
    }

    let reader = GeoTiffHeightmapReader::open(&settings.heightmap_path)?;
    let mapping = mapping_for(reader.metadata(), settings.scale_denominator)?;
    let cache = GeoTiffRowCache::new(&reader, settings.cache_rows)?;
    let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);
    let surface_material_sampler = settings
        .surface_material_path
        .as_ref()
        .map(|path| {
            EarthDataSurfaceMaterialSampler::open_with_tile_cache_entries(
                path,
                settings.surface_tile_cache_entries,
            )
        })
        .transpose()?;
    let material_sampler = surface_material_sampler
        .as_ref()
        .map(|sampler| sampler as &dyn SurfaceMaterialSampler);

    let region_block_x = settings.region_x.wrapping_mul(REGION_SIZE_BLOCKS);
    let region_block_z = settings.region_z.wrapping_mul(REGION_SIZE_BLOCKS);
    let mut elevations = vec![0.0; SURFACE_REGION_EXTENT * SURFACE_REGION_EXTENT];
    let mut valid = vec![false; SURFACE_REGION_EXTENT * SURFACE_REGION_EXTENT];
    for z in 0..SURFACE_REGION_EXTENT {
        for x in 0..SURFACE_REGION_EXTENT {
            let global_block_x = region_block_x
                .wrapping_add(x as i32)
                .wrapping_sub(SURFACE_REGION_COAST_RADIUS as i32);
            let global_block_z = region_block_z
                .wrapping_add(z as i32)
                .wrapping_sub(SURFACE_REGION_COAST_RADIUS as i32);
            let map_x = global_block_x.wrapping_add(mapping.width_blocks / 2);
            let map_z = global_block_z.wrapping_add(mapping.height_blocks / 2);
            let index = surface_region_extent_index(x, z);
            if !surface_chunk_map_position_valid(&mapping, map_x, map_z) {
                continue;
            }
            let longitude = mapping.longitude_for_block_x(map_x)?;
            let latitude = mapping.latitude_for_block_z(map_z)?;
            elevations[index] = sampler.bilinear_meters(longitude, latitude)?;
            valid[index] = true;
        }
    }

    let water_mask = surface_region_water_decision_mask(&elevations, &valid);
    let coast_factor_extent = surface_region_coast_factors(&valid, &water_mask);
    let mut columns = Vec::with_capacity(SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH);
    let mut coast_factors = Vec::with_capacity(SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH);
    let collect_photo_materials =
        settings.texture_mode == SurfaceTextureMode::Photo && material_sampler.is_some();
    let mut photo_material_columns = if collect_photo_materials {
        Some(vec![None; SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH])
    } else {
        None
    };
    let mut photo_smoothed_elevations = if collect_photo_materials {
        Some(vec![0.0; SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH])
    } else {
        None
    };
    let mut photo_longitudes = if collect_photo_materials {
        Some(vec![0.0; SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH])
    } else {
        None
    };
    let mut photo_latitudes = if collect_photo_materials {
        Some(vec![0.0; SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH])
    } else {
        None
    };
    let mut photo_local_relief_meters = if collect_photo_materials {
        Some(vec![0.0; SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH])
    } else {
        None
    };
    let mut photo_global_block_x = if collect_photo_materials {
        Some(vec![0; SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH])
    } else {
        None
    };
    let mut photo_global_block_z = if collect_photo_materials {
        Some(vec![0; SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH])
    } else {
        None
    };
    let mut photo_token_luma_profile = if collect_photo_materials {
        Some(PhotoSurfaceTokenLumaProfile::default())
    } else {
        None
    };
    let mut traces = vec![None; local_columns.len()];

    for local_z in 0..SURFACE_REGION_WIDTH {
        for local_x in 0..SURFACE_REGION_WIDTH {
            let column_index = (local_z * SURFACE_REGION_WIDTH) + local_x;
            let center_x = local_x + SURFACE_REGION_COAST_RADIUS;
            let center_z = local_z + SURFACE_REGION_COAST_RADIUS;
            let global_block_x = region_block_x.wrapping_add(local_x as i32);
            let global_block_z = region_block_z.wrapping_add(local_z as i32);
            let map_x = global_block_x.wrapping_add(mapping.width_blocks / 2);
            let map_z = global_block_z.wrapping_add(mapping.height_blocks / 2);
            let longitude = if map_x < 0 || map_x >= mapping.width_blocks {
                0.0
            } else {
                mapping.longitude_for_block_x(map_x)?
            };
            let latitude = if map_z < 0 || map_z >= mapping.height_blocks {
                0.0
            } else {
                mapping.latitude_for_block_z(map_z)?
            };
            let sample_index = surface_region_extent_index(center_x, center_z);
            let raw_elevation = elevations[sample_index];
            let smoothed_elevation =
                surface_region_smoothed_elevation(&elevations, &valid, center_x, center_z);
            let local_relief_meters =
                surface_region_local_relief_meters(&elevations, &valid, center_x, center_z);
            let water = water_mask[sample_index];
            let coast_factor = coast_factor_extent[sample_index];
            let base_column = classify_shaped_surface_scaled(
                smoothed_elevation,
                longitude,
                latitude,
                water,
                coast_factor,
                settings.vertical_scale,
            )?;
            let mut column = base_column.clone();
            let mut material_sample = None;
            let mut semantic_column = None;
            if let Some(material_sampler) = material_sampler {
                if should_sample_surface_material(
                    material_sampler,
                    valid[sample_index],
                    water,
                    coast_factor,
                ) {
                    let longitude_span = 360.0 / f64::from(mapping.width_blocks);
                    let latitude_span = (mapping.max_latitude - mapping.min_latitude)
                        / f64::from(mapping.height_blocks);
                    let material = sample_surface_region_material(
                        material_sampler,
                        settings.texture_mode,
                        water,
                        longitude,
                        latitude,
                        longitude_span,
                        latitude_span,
                    )?;
                    column = apply_surface_region_semantic_material_sample(
                        column,
                        &material,
                        smoothed_elevation,
                        longitude,
                        latitude,
                        coast_factor,
                        local_relief_meters,
                        settings.vertical_scale,
                    )?;
                    semantic_column = Some(column.clone());
                    if let Some(token_luma_profile) = photo_token_luma_profile.as_mut() {
                        if !column.water {
                            token_luma_profile.add(&material);
                        }
                    }
                    if let Some(photo_material_columns) = photo_material_columns.as_mut() {
                        photo_material_columns[column_index] = Some(material.clone());
                    }
                    if let Some(photo_smoothed_elevations) = photo_smoothed_elevations.as_mut() {
                        photo_smoothed_elevations[column_index] = smoothed_elevation;
                    }
                    if let Some(photo_longitudes) = photo_longitudes.as_mut() {
                        photo_longitudes[column_index] = longitude;
                    }
                    if let Some(photo_latitudes) = photo_latitudes.as_mut() {
                        photo_latitudes[column_index] = latitude;
                    }
                    if let Some(photo_local_relief_meters) = photo_local_relief_meters.as_mut() {
                        photo_local_relief_meters[column_index] = local_relief_meters;
                    }
                    if let Some(photo_global_block_x) = photo_global_block_x.as_mut() {
                        photo_global_block_x[column_index] = global_block_x;
                    }
                    if let Some(photo_global_block_z) = photo_global_block_z.as_mut() {
                        photo_global_block_z[column_index] = global_block_z;
                    }
                    material_sample = Some(material);
                }
            }
            if let Some(&request_index) = selected.get(&column_index) {
                traces[request_index] = Some(SurfaceRegionColumnTrace {
                    local_x,
                    local_z,
                    global_block_x,
                    global_block_z,
                    map_x,
                    map_z,
                    longitude,
                    latitude,
                    raw_elevation_meters: raw_elevation,
                    smoothed_elevation_meters: smoothed_elevation,
                    local_relief_meters,
                    initial_water: water,
                    valid: valid[sample_index],
                    coast_factor,
                    material_sample,
                    base_column,
                    semantic_column,
                    photo_column: None,
                    post_cell_column: None,
                    post_stabilized_column: None,
                    post_first_component_trace: None,
                    post_smoothed_column: None,
                    post_component_column: None,
                    post_component_trace: None,
                    final_column: column.clone(),
                });
            }
            columns.push(column);
            coast_factors.push(coast_factor);
        }
    }

    if let Some(photo_material_columns) = photo_material_columns {
        let photo_token_luma_profile = photo_token_luma_profile.map(Arc::new);
        let photo_smoothed_elevations = photo_smoothed_elevations.expect("photo elevations");
        let photo_longitudes = photo_longitudes.expect("photo longitudes");
        let photo_latitudes = photo_latitudes.expect("photo latitudes");
        let photo_local_relief_meters = photo_local_relief_meters.expect("photo local relief");
        let photo_global_block_x = photo_global_block_x.expect("photo global block x");
        let photo_global_block_z = photo_global_block_z.expect("photo global block z");
        for (column_index, material) in photo_material_columns.into_iter().enumerate() {
            if let Some(material) = material {
                columns[column_index] = apply_surface_region_photo_material_sample(
                    columns[column_index].clone(),
                    material,
                    photo_smoothed_elevations[column_index],
                    photo_longitudes[column_index],
                    photo_latitudes[column_index],
                    coast_factors[column_index],
                    photo_local_relief_meters[column_index],
                    photo_global_block_x[column_index],
                    photo_global_block_z[column_index],
                    settings.vertical_scale,
                    photo_token_luma_profile.as_ref(),
                )?;
                if let Some(&request_index) = selected.get(&column_index) {
                    if let Some(trace) = traces[request_index].as_mut() {
                        trace.photo_column = Some(columns[column_index].clone());
                        trace.final_column = columns[column_index].clone();
                    }
                }
            }
        }
    }

    let (post_cell, stabilized, smoothed, post_smoothed, preserve_surface) = match settings
        .texture_mode
    {
        SurfaceTextureMode::Photo => {
            let post_cell = stabilize_surface_biome_cells(&columns, SURFACE_REGION_WIDTH, true);
            let stabilized = stabilize_small_surface_biome_family_components(
                &post_cell,
                SURFACE_REGION_WIDTH,
                true,
            );
            let smoothed = smooth_photo_textures(&stabilized, SURFACE_REGION_WIDTH)?;
            let post_smoothed = stabilize_small_surface_biome_family_components(
                &smoothed,
                SURFACE_REGION_WIDTH,
                true,
            );
            (post_cell, stabilized, smoothed, post_smoothed, true)
        }
        SurfaceTextureMode::Classified => {
            let post_cell = stabilize_surface_biome_cells(&columns, SURFACE_REGION_WIDTH, false);
            let stabilized = stabilize_small_surface_biome_family_components(
                &post_cell,
                SURFACE_REGION_WIDTH,
                false,
            );
            let smoothed = smooth_surface_classes(&stabilized, SURFACE_REGION_WIDTH)?;
            let post_smoothed = stabilize_small_surface_biome_family_components(
                &smoothed,
                SURFACE_REGION_WIDTH,
                false,
            );
            (post_cell, stabilized, smoothed, post_smoothed, false)
        }
    };
    let cleaned =
        clean_coastal_surface_columns(&post_smoothed, &coast_factors, SURFACE_REGION_WIDTH)?;
    for (&column_index, &request_index) in &selected {
        if let Some(trace) = traces[request_index].as_mut() {
            trace.post_cell_column = Some(post_cell[column_index].clone());
            trace.post_stabilized_column = Some(stabilized[column_index].clone());
            trace.post_first_component_trace = Some(surface_biome_component_trace(
                &post_cell,
                SURFACE_REGION_WIDTH,
                column_index,
                preserve_surface,
            ));
            trace.post_smoothed_column = Some(smoothed[column_index].clone());
            trace.post_component_column = Some(post_smoothed[column_index].clone());
            trace.post_component_trace = Some(surface_biome_component_trace(
                &smoothed,
                SURFACE_REGION_WIDTH,
                column_index,
                preserve_surface,
            ));
            trace.final_column = cleaned[column_index].clone();
        }
    }

    traces
        .into_iter()
        .map(|trace| trace.ok_or_else(|| SurfaceError::invalid("missing requested column trace")))
        .collect()
}

fn surface_chunk_status_for(
    build: &SurfaceChunkBuild,
    requested_status: ChunkGenerationStatus,
) -> ChunkGenerationStatus {
    if requested_status == ChunkGenerationStatus::Surface && build.water_columns > 0 {
        ChunkGenerationStatus::Carvers
    } else {
        requested_status
    }
}

pub fn surface_y_for_elevation_meters(elevation_meters: f64) -> i32 {
    let rounded =
        java_math_round_double_to_narrowed_i32(elevation_meters / ELEVATION_METERS_PER_BLOCK);
    let y = SEA_LEVEL_Y.wrapping_add(rounded);
    y.clamp(-60, 319)
}

pub const DEFAULT_VERTICAL_SCALE: f64 = 1.0;

const BEACH_COAST_FACTOR: f64 = 0.985;
const SURFACE_REGION_WATER_MATERIAL_COAST_FACTOR: f64 = 0.985;
const SHAPED_ELEVATION_METERS_PER_BLOCK: f64 = 45.0;
const BATHYMETRY_METERS_PER_BLOCK: f64 = SHAPED_ELEVATION_METERS_PER_BLOCK;
const MIN_VERTICAL_SCALE: f64 = 0.25;
const MAX_VERTICAL_SCALE: f64 = 4.0;
const MIN_SURFACE_Y: i32 = -60;
const MAX_SURFACE_Y: i32 = 319;
const SURFACE_REGION_RELIEF_RADIUS: i32 = 4;

pub fn classify_surface(
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
) -> EarthSurfaceColumn {
    let water = elevation_meters <= 0.0;
    let ground_y = ground_surface_y_at(elevation_meters, longitude, latitude, water);
    let biome = biome_id(elevation_meters, longitude, latitude, water, ground_y, 0.0);
    let top = top_block_state_id(
        elevation_meters,
        longitude,
        latitude,
        water,
        ground_y,
        &biome,
        0.0,
    );
    let filler = filler_block_state_id(top, water);
    let water_surface_y = if water { SEA_LEVEL_Y } else { i32::MIN };
    EarthSurfaceColumn::new(
        water,
        ground_y,
        water_surface_y,
        top,
        filler,
        biome,
        "height-rule",
    )
}

pub fn classify_shaped_surface(
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    water: bool,
    coast_factor: f64,
) -> Result<EarthSurfaceColumn> {
    classify_shaped_surface_scaled(
        elevation_meters,
        longitude,
        latitude,
        water,
        coast_factor,
        DEFAULT_VERTICAL_SCALE,
    )
}

pub fn classify_shaped_surface_scaled(
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    water: bool,
    coast_factor: f64,
    vertical_scale: f64,
) -> Result<EarthSurfaceColumn> {
    let coast_factor = clamp_unit(coast_factor);
    let vertical_scale = require_valid_vertical_scale(vertical_scale)?;
    let ground_y = shaped_ground_surface_y(
        elevation_meters,
        longitude,
        latitude,
        water,
        coast_factor,
        vertical_scale,
    );
    let biome = biome_id(
        elevation_meters,
        longitude,
        latitude,
        water,
        ground_y,
        coast_factor,
    );
    let top = top_block_state_id(
        elevation_meters,
        longitude,
        latitude,
        water,
        ground_y,
        &biome,
        coast_factor,
    );
    let filler = filler_block_state_id(top, water);
    let water_surface_y = if water { SEA_LEVEL_Y } else { i32::MIN };
    Ok(EarthSurfaceColumn::new(
        water,
        ground_y,
        water_surface_y,
        top,
        filler,
        biome,
        "height-rule",
    ))
}

pub fn normalize_surface_column_for_chunk(column: &EarthSurfaceColumn) -> EarthSurfaceColumn {
    if !column.water {
        return column.clone();
    }
    let water_surface_y = if column.water_surface_y == i32::MIN {
        SEA_LEVEL_Y
    } else {
        clamp_surface_y(column.water_surface_y)
    };
    let mut ground_y = clamp_surface_y(column.ground_surface_y.min(water_surface_y - 1));
    if ground_y >= water_surface_y {
        ground_y = MIN_SURFACE_Y.max(water_surface_y - 1);
    }
    if ground_y == column.ground_surface_y && water_surface_y == column.water_surface_y {
        return column.clone();
    }
    let mut normalized = EarthSurfaceColumn::new(
        true,
        ground_y,
        water_surface_y,
        column.top_block_state_id,
        column.filler_block_state_id,
        column.biome_id.clone(),
        format!("{}+water-volume", column.decision_source),
    );
    normalized.terrain_token_source = column.terrain_token_source;
    normalized.data_evidence_flags = column.data_evidence_flags;
    normalized
}

pub fn sanitize_surface_column_for_production(column: &EarthSurfaceColumn) -> EarthSurfaceColumn {
    if !column.water && is_photo_preserved_natural_surface_column(column) {
        let top = column.top_block_state_id;
        let filler = smoother_filler_for(top);
        if filler == column.filler_block_state_id {
            return column.clone();
        }
        return replace_surface_blocks(column, top, filler, "photo-natural-surface");
    }
    let mut top =
        production_surface_top_for_water(column.top_block_state_id, &column.biome_id, column.water);
    let mut filler = production_surface_filler(
        column.filler_block_state_id,
        top,
        &column.biome_id,
        column.water,
    );
    if column.water && (top == block_state_ids::GRAVEL || is_sand_like_surface(top)) {
        top = stable_water_floor_replacement(column);
        filler = top;
    }
    if !column.water
        && is_sand_like_surface(top)
        && (column.ground_surface_y <= SEA_LEVEL_Y + 6
            || !is_true_sandy_land_biome(&column.biome_id))
    {
        top = if is_wet_surface_biome(&column.biome_id) {
            block_state_ids::MUD
        } else {
            block_state_ids::GRASS_BLOCK
        };
        filler = filler_for_production_top(top);
    }
    if !column.water && should_soften_temperate_rock(column, top) {
        top = temperate_rock_replacement(&column.biome_id);
        filler = filler_for_production_top(top);
    }
    if !column.water && should_regreen_temperate_earth(column, top) {
        top = temperate_earth_replacement(&column.biome_id);
        filler = filler_for_production_top(top);
    }
    if top == column.top_block_state_id && filler == column.filler_block_state_id {
        return column.clone();
    }
    replace_surface_blocks(column, top, filler, "natural-surface")
}

pub fn build_surface_chunk(
    chunk_x: i32,
    chunk_z: i32,
    columns: &[EarthSurfaceColumn],
) -> Result<SurfaceChunkBuild> {
    let expected_columns = CHUNK_WIDTH * CHUNK_WIDTH;
    if columns.len() != expected_columns {
        return Err(SurfaceError::invalid(
            "surface chunk sample must contain one entry per chunk column",
        ));
    }
    let mut chunk = ChunkModel::overworld(chunk_x, chunk_z);
    let mut land_columns = 0;
    let mut water_columns = 0;
    let mut min_ground_y = i32::MAX;
    let mut max_ground_y = i32::MIN;
    let mut biome_by_local_column = vec![String::new(); expected_columns];
    let mut biome_min_y_by_local_column = vec![0; expected_columns];
    let mut biome_max_y_by_local_column = vec![0; expected_columns];
    let mut ground_surface_y_by_local_column = vec![0; expected_columns];
    let mut water_by_local_column = vec![false; expected_columns];
    let mut biome_counts = BTreeMap::<String, i32>::new();
    for local_z in 0..CHUNK_WIDTH {
        for local_x in 0..CHUNK_WIDTH {
            let column_index = (local_z * CHUNK_WIDTH) + local_x;
            let column = normalize_surface_column_for_chunk(&columns[column_index]);
            if column.water {
                water_columns += 1;
            } else {
                land_columns += 1;
            }
            min_ground_y = min_ground_y.min(column.ground_surface_y);
            max_ground_y = max_ground_y.max(column.ground_surface_y);
            biome_by_local_column[column_index] = column.biome_id.clone();
            biome_min_y_by_local_column[column_index] = column.ground_surface_y;
            biome_max_y_by_local_column[column_index] = if column.water {
                column.water_surface_y
            } else {
                column.ground_surface_y
            };
            ground_surface_y_by_local_column[column_index] = column.ground_surface_y;
            water_by_local_column[column_index] = column.water;
            *biome_counts.entry(column.biome_id.clone()).or_insert(0) += 1;
            fill_surface_column(&mut chunk, local_x as i32, local_z as i32, &column)?;
        }
    }
    let biome = dominant_surface_biome(&biome_counts);
    chunk.set_biome_id(biome.clone())?;
    apply_surface_biome_cells(
        &mut chunk,
        &biome_by_local_column,
        &biome_min_y_by_local_column,
        &biome_max_y_by_local_column,
        &biome,
    )?;
    Ok(SurfaceChunkBuild {
        chunk,
        land_columns,
        water_columns,
        min_ground_y,
        max_ground_y,
        biome,
        ground_surface_y_by_local_column,
        water_by_local_column,
    })
}

pub fn sample_surface_chunk(
    chunk_x: i32,
    chunk_z: i32,
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
) -> Result<SurfaceChunkSample> {
    sample_surface_chunk_scaled(chunk_x, chunk_z, mapping, sampler, DEFAULT_VERTICAL_SCALE)
}

pub fn sample_surface_chunk_scaled(
    chunk_x: i32,
    chunk_z: i32,
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
    vertical_scale: f64,
) -> Result<SurfaceChunkSample> {
    sample_surface_chunk_with_elevation_fn(
        chunk_x,
        chunk_z,
        mapping,
        vertical_scale,
        |longitude, latitude| {
            sampler
                .bilinear_meters(longitude, latitude)
                .map_err(Into::into)
        },
    )
}

pub fn sample_surface_region_scaled(
    region_x: i32,
    region_z: i32,
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
    vertical_scale: f64,
) -> Result<SurfaceRegionSample> {
    sample_surface_region_scaled_with_texture_mode(
        region_x,
        region_z,
        mapping,
        sampler,
        vertical_scale,
        SurfaceTextureMode::DEFAULT,
    )
}

pub fn sample_surface_region_scaled_with_texture_mode(
    region_x: i32,
    region_z: i32,
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
    vertical_scale: f64,
    texture_mode: SurfaceTextureMode,
) -> Result<SurfaceRegionSample> {
    sample_surface_region_scaled_with_material_sampler(
        region_x,
        region_z,
        mapping,
        sampler,
        vertical_scale,
        texture_mode,
        None,
        true,
    )
}

pub fn sample_surface_region_scaled_with_material_sampler(
    region_x: i32,
    region_z: i32,
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
    vertical_scale: f64,
    texture_mode: SurfaceTextureMode,
    material_sampler: Option<&dyn SurfaceMaterialSampler>,
    parallel_column_sampling: bool,
) -> Result<SurfaceRegionSample> {
    sample_surface_region_with_elevation_fn(
        region_x,
        region_z,
        mapping,
        vertical_scale,
        |longitude, latitude| {
            sampler
                .bilinear_meters(longitude, latitude)
                .map_err(Into::into)
        },
        texture_mode,
        material_sampler,
        parallel_column_sampling,
    )
}

fn sample_surface_region_with_elevation_fn<F>(
    region_x: i32,
    region_z: i32,
    mapping: &EarthScaleMapping,
    vertical_scale: f64,
    mut elevation_fn: F,
    texture_mode: SurfaceTextureMode,
    material_sampler: Option<&dyn SurfaceMaterialSampler>,
    parallel_column_sampling: bool,
) -> Result<SurfaceRegionSample>
where
    F: FnMut(f64, f64) -> Result<f64>,
{
    let vertical_scale = require_valid_vertical_scale(vertical_scale)?;
    let mut elevations = vec![0.0; SURFACE_REGION_EXTENT * SURFACE_REGION_EXTENT];
    let mut valid = vec![false; SURFACE_REGION_EXTENT * SURFACE_REGION_EXTENT];
    let region_block_x = region_x.wrapping_mul(REGION_SIZE_BLOCKS);
    let region_block_z = region_z.wrapping_mul(REGION_SIZE_BLOCKS);
    for z in 0..SURFACE_REGION_EXTENT {
        for x in 0..SURFACE_REGION_EXTENT {
            let global_block_x = region_block_x
                .wrapping_add(x as i32)
                .wrapping_sub(SURFACE_REGION_COAST_RADIUS as i32);
            let global_block_z = region_block_z
                .wrapping_add(z as i32)
                .wrapping_sub(SURFACE_REGION_COAST_RADIUS as i32);
            let map_x = global_block_x.wrapping_add(mapping.width_blocks / 2);
            let map_z = global_block_z.wrapping_add(mapping.height_blocks / 2);
            let index = surface_region_extent_index(x, z);
            if !surface_chunk_map_position_valid(mapping, map_x, map_z) {
                elevations[index] = 0.0;
                valid[index] = false;
                continue;
            }
            let longitude = mapping.longitude_for_block_x(map_x)?;
            let latitude = mapping.latitude_for_block_z(map_z)?;
            elevations[index] = elevation_fn(longitude, latitude)?;
            valid[index] = true;
        }
    }

    let water_mask = surface_region_water_decision_mask(&elevations, &valid);
    let coast_factor_extent = surface_region_coast_factors(&valid, &water_mask);
    let collect_photo_materials =
        texture_mode == SurfaceTextureMode::Photo && material_sampler.is_some();

    struct SurfaceColumnSampleBuild {
        column: EarthSurfaceColumn,
        coast_factor: f64,
        material: Option<SurfaceMaterialSample>,
        smoothed_elevation: f64,
        longitude: f64,
        latitude: f64,
        local_relief_meters: f64,
        global_block_x: i32,
        global_block_z: i32,
    }

    let build_column = |column_index| -> Result<SurfaceColumnSampleBuild> {
        let local_z = column_index / SURFACE_REGION_WIDTH;
        let local_x = column_index % SURFACE_REGION_WIDTH;
        let center_x = local_x + SURFACE_REGION_COAST_RADIUS;
        let center_z = local_z + SURFACE_REGION_COAST_RADIUS;
        let global_block_x = region_block_x.wrapping_add(local_x as i32);
        let global_block_z = region_block_z.wrapping_add(local_z as i32);
        let map_x = global_block_x.wrapping_add(mapping.width_blocks / 2);
        let map_z = global_block_z.wrapping_add(mapping.height_blocks / 2);
        let longitude = if map_x < 0 || map_x >= mapping.width_blocks {
            0.0
        } else {
            mapping.longitude_for_block_x(map_x)?
        };
        let latitude = if map_z < 0 || map_z >= mapping.height_blocks {
            0.0
        } else {
            mapping.latitude_for_block_z(map_z)?
        };
        let sample_index = surface_region_extent_index(center_x, center_z);
        let smoothed_elevation =
            surface_region_smoothed_elevation(&elevations, &valid, center_x, center_z);
        let water = water_mask[sample_index];
        let coast_factor = coast_factor_extent[sample_index];
        let mut column = classify_shaped_surface_scaled(
            smoothed_elevation,
            longitude,
            latitude,
            water,
            coast_factor,
            vertical_scale,
        )?;
        let mut material = None;
        let mut local_relief_meters = 0.0;
        if let Some(material_sampler) = material_sampler {
            if should_sample_surface_material(
                material_sampler,
                valid[sample_index],
                water,
                coast_factor,
            ) {
                let longitude_span = 360.0 / f64::from(mapping.width_blocks);
                let latitude_span = (mapping.max_latitude - mapping.min_latitude)
                    / f64::from(mapping.height_blocks);
                let sampled_material = sample_surface_region_material(
                    material_sampler,
                    texture_mode,
                    water,
                    longitude,
                    latitude,
                    longitude_span,
                    latitude_span,
                )?;
                local_relief_meters =
                    surface_region_local_relief_meters(&elevations, &valid, center_x, center_z);
                column = apply_surface_region_semantic_material_sample(
                    column,
                    &sampled_material,
                    smoothed_elevation,
                    longitude,
                    latitude,
                    coast_factor,
                    local_relief_meters,
                    vertical_scale,
                )?;
                material = Some(sampled_material);
            }
        }

        Ok(SurfaceColumnSampleBuild {
            column,
            coast_factor,
            material,
            smoothed_elevation,
            longitude,
            latitude,
            local_relief_meters,
            global_block_x,
            global_block_z,
        })
    };
    let column_builds = if parallel_column_sampling {
        (0..(SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH))
            .into_par_iter()
            .map(build_column)
            .collect::<Result<Vec<_>>>()?
    } else {
        let mut column_builds = Vec::with_capacity(SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH);
        for column_index in 0..(SURFACE_REGION_WIDTH * SURFACE_REGION_WIDTH) {
            column_builds.push(build_column(column_index)?);
        }
        column_builds
    };
    let coast_factors = column_builds
        .iter()
        .map(|build| build.coast_factor)
        .collect::<Vec<_>>();

    let columns = if collect_photo_materials {
        let mut photo_token_luma_profile = PhotoSurfaceTokenLumaProfile::default();
        for build in &column_builds {
            if let Some(material) = build.material.as_ref() {
                if !build.column.water {
                    photo_token_luma_profile.add(material);
                }
            }
        }
        let photo_token_luma_profile = Arc::new(photo_token_luma_profile);
        column_builds
            .into_par_iter()
            .map(|build| -> Result<EarthSurfaceColumn> {
                if let Some(material) = build.material {
                    if build.column.water {
                        return Ok(build.column);
                    }
                    return apply_surface_region_photo_material_sample(
                        build.column,
                        material,
                        build.smoothed_elevation,
                        build.longitude,
                        build.latitude,
                        build.coast_factor,
                        build.local_relief_meters,
                        build.global_block_x,
                        build.global_block_z,
                        vertical_scale,
                        Some(&photo_token_luma_profile),
                    );
                }
                Ok(build.column)
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        column_builds
            .into_iter()
            .map(|build| build.column)
            .collect::<Vec<_>>()
    };

    let cleaned = post_process_surface_region_columns(
        &columns,
        &coast_factors,
        SURFACE_REGION_WIDTH,
        texture_mode,
    )?;
    SurfaceRegionSample::new(cleaned)
}

fn post_process_surface_region_columns(
    columns: &[EarthSurfaceColumn],
    coast_factors: &[f64],
    width: usize,
    texture_mode: SurfaceTextureMode,
) -> Result<Vec<EarthSurfaceColumn>> {
    let post_smoothed = match texture_mode {
        SurfaceTextureMode::Photo => {
            let stabilized = stabilize_surface_biome_families_preserving_surfaces(columns, width)?;
            let smoothed = smooth_photo_textures(&stabilized, width)?;
            stabilize_small_surface_biome_family_components(&smoothed, width, true)
        }
        SurfaceTextureMode::Classified => {
            let stabilized = stabilize_surface_biome_families(columns, width)?;
            let smoothed = smooth_surface_classes(&stabilized, width)?;
            stabilize_small_surface_biome_family_components(&smoothed, width, false)
        }
    };
    clean_coastal_surface_columns(&post_smoothed, coast_factors, width)
}
fn should_sample_surface_material(
    material_sampler: &dyn SurfaceMaterialSampler,
    valid: bool,
    water: bool,
    coast_factor: f64,
) -> bool {
    valid
        && (!water
            || material_sampler.samples_open_water()
            || coast_factor >= SURFACE_REGION_WATER_MATERIAL_COAST_FACTOR)
}

fn sample_surface_region_material(
    material_sampler: &dyn SurfaceMaterialSampler,
    texture_mode: SurfaceTextureMode,
    water: bool,
    longitude: f64,
    latitude: f64,
    longitude_span_degrees: f64,
    latitude_span_degrees: f64,
) -> Result<SurfaceMaterialSample> {
    if water && material_sampler.samples_open_water() {
        return material_sampler.sample_water(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        );
    }
    match texture_mode {
        SurfaceTextureMode::Photo => material_sampler.sample_photo(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        ),
        SurfaceTextureMode::Classified => material_sampler.sample(
            longitude,
            latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_surface_region_semantic_material_sample(
    semantic_column: EarthSurfaceColumn,
    sample: &SurfaceMaterialSample,
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    coast_factor: f64,
    local_relief_meters: f64,
    vertical_scale: f64,
) -> Result<EarthSurfaceColumn> {
    apply_surface_material(
        &semantic_column,
        sample,
        elevation_meters,
        longitude,
        latitude,
        coast_factor,
        local_relief_meters * vertical_scale,
        vertical_scale,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_surface_region_photo_material_sample(
    semantic_column: EarthSurfaceColumn,
    sample: SurfaceMaterialSample,
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    coast_factor: f64,
    local_relief_meters: f64,
    global_block_x: i32,
    global_block_z: i32,
    vertical_scale: f64,
    token_luma_profile: Option<&Arc<PhotoSurfaceTokenLumaProfile>>,
) -> Result<EarthSurfaceColumn> {
    let mut input = PhotoSurfaceInput::new(
        semantic_column,
        sample,
        elevation_meters,
        longitude,
        latitude,
        coast_factor,
        local_relief_meters * vertical_scale,
        global_block_x,
        global_block_z,
    );
    if let Some(profile) = token_luma_profile {
        input = input.with_token_luma_profile(Arc::clone(profile));
    }
    apply_photo_surface_material(&input)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn apply_surface_region_material_sample(
    semantic_column: EarthSurfaceColumn,
    sample: SurfaceMaterialSample,
    texture_mode: SurfaceTextureMode,
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    coast_factor: f64,
    local_relief_meters: f64,
    global_block_x: i32,
    global_block_z: i32,
    vertical_scale: f64,
) -> Result<EarthSurfaceColumn> {
    let semantic_column = apply_surface_region_semantic_material_sample(
        semantic_column,
        &sample,
        elevation_meters,
        longitude,
        latitude,
        coast_factor,
        local_relief_meters,
        vertical_scale,
    )?;
    match texture_mode {
        SurfaceTextureMode::Photo => apply_surface_region_photo_material_sample(
            semantic_column,
            sample,
            elevation_meters,
            longitude,
            latitude,
            coast_factor,
            local_relief_meters,
            global_block_x,
            global_block_z,
            vertical_scale,
            None,
        ),
        SurfaceTextureMode::Classified => Ok(semantic_column),
    }
}

fn surface_region_local_relief_meters(
    elevations: &[f64],
    valid: &[bool],
    center_x: usize,
    center_z: usize,
) -> f64 {
    let center_index = surface_region_extent_index(center_x, center_z);
    if !valid[center_index] {
        return 0.0;
    }
    let mut min_elevation = elevations[center_index];
    let mut max_elevation = elevations[center_index];
    for dz in -SURFACE_REGION_RELIEF_RADIUS..=SURFACE_REGION_RELIEF_RADIUS {
        for dx in -SURFACE_REGION_RELIEF_RADIUS..=SURFACE_REGION_RELIEF_RADIUS {
            let x = center_x as i32 + dx;
            let z = center_z as i32 + dz;
            if x < 0
                || x >= SURFACE_REGION_EXTENT as i32
                || z < 0
                || z >= SURFACE_REGION_EXTENT as i32
            {
                continue;
            }
            let x = x as usize;
            let z = z as usize;
            let index = surface_region_extent_index(x, z);
            if !valid[index] {
                continue;
            }
            let elevation = elevations[index];
            min_elevation = min_elevation.min(elevation);
            max_elevation = max_elevation.max(elevation);
        }
    }
    max_elevation - min_elevation
}

pub fn smooth_surface_classes(
    columns: &[EarthSurfaceColumn],
    width: usize,
) -> Result<Vec<EarthSurfaceColumn>> {
    if width == 0 || columns.len() % width != 0 {
        return Err(SurfaceError::invalid("width must divide columns length"));
    }
    let height = columns.len() / width;
    let mut result = columns.to_vec();
    for z in 0..height {
        for x in 0..width {
            let index = surface_class_index(x, z, width);
            let column = &columns[index];
            if column.water {
                continue;
            }
            let stats = surface_neighborhood_stats(columns, width, height, x, z);
            if stats.land_count < SURFACE_SMOOTHER_MAJORITY_COUNT_MIN {
                continue;
            }
            let top = column.top_block_state_id;
            let biome = &column.biome_id;
            let mut smoothed_top = top;
            let mut smoothed_biome = biome.clone();
            if stats.count_top(top) <= SURFACE_SMOOTHER_ISOLATED_COUNT_MAX
                && stats.majority_top_count >= SURFACE_SMOOTHER_MAJORITY_COUNT_MIN
                && !is_protected_surface_for_smoothing(top, biome)
            {
                smoothed_top = stats.majority_top;
            }
            if stats.count_biome(biome) <= SURFACE_SMOOTHER_ISOLATED_COUNT_MAX
                && stats.majority_biome_count >= SURFACE_SMOOTHER_MAJORITY_COUNT_MIN
                && biome != "minecraft:beach"
            {
                smoothed_biome = stats.majority_biome.clone();
            }
            smoothed_top = compatible_top_for_smoother_biome(smoothed_top, &smoothed_biome);
            if smoothed_top != top || smoothed_biome != *biome {
                result[index] = smoother_replacement(column, smoothed_top, &smoothed_biome);
            }
        }
    }
    Ok(result)
}

pub fn smooth_photo_textures(
    columns: &[EarthSurfaceColumn],
    width: usize,
) -> Result<Vec<EarthSurfaceColumn>> {
    require_surface_grid_width(columns, width)?;
    let local_pass = smooth_photo_texture_local(columns, width);
    Ok(smooth_photo_macro_vegetation(&local_pass, width))
}

pub fn stabilize_surface_biome_families(
    columns: &[EarthSurfaceColumn],
    width: usize,
) -> Result<Vec<EarthSurfaceColumn>> {
    require_surface_grid_width(columns, width)?;
    let cell_pass = stabilize_surface_biome_cells(columns, width, false);
    Ok(stabilize_small_surface_biome_family_components(
        &cell_pass, width, false,
    ))
}

pub fn stabilize_surface_biome_families_preserving_surfaces(
    columns: &[EarthSurfaceColumn],
    width: usize,
) -> Result<Vec<EarthSurfaceColumn>> {
    require_surface_grid_width(columns, width)?;
    let cell_pass = stabilize_surface_biome_cells(columns, width, true);
    Ok(stabilize_small_surface_biome_family_components(
        &cell_pass, width, true,
    ))
}

pub fn stabilize_small_surface_biome_components(
    columns: &[EarthSurfaceColumn],
    width: usize,
) -> Result<Vec<EarthSurfaceColumn>> {
    require_surface_grid_width(columns, width)?;
    Ok(stabilize_small_surface_biome_family_components(
        columns, width, false,
    ))
}

pub fn stabilize_small_surface_biome_components_preserving_surfaces(
    columns: &[EarthSurfaceColumn],
    width: usize,
) -> Result<Vec<EarthSurfaceColumn>> {
    require_surface_grid_width(columns, width)?;
    Ok(stabilize_small_surface_biome_family_components(
        columns, width, true,
    ))
}

pub fn is_allowed_production_top(block: i32, _biome: &str) -> bool {
    is_allowed_natural_surface_top(block)
}

pub fn production_surface_top(block: i32, biome: &str) -> i32 {
    production_surface_top_for_water(block, biome, false)
}

fn production_surface_top_for_water(block: i32, biome: &str, water: bool) -> i32 {
    if is_allowed_natural_surface_top(block) {
        if (water || is_coast_surface_biome(biome)) && !is_coastal_native_surface_top(block) {
            return coastal_replacement(block, biome);
        }
        return block;
    }
    if water || is_coast_surface_biome(biome) {
        return coastal_replacement(block, biome);
    }
    match block {
        block_state_ids::OAK_LEAVES
        | block_state_ids::JUNGLE_LEAVES
        | block_state_ids::DARK_OAK_LEAVES
        | block_state_ids::SPRUCE_LEAVES
        | block_state_ids::GREEN_TERRACOTTA
        | block_state_ids::LIME_TERRACOTTA => vegetated_replacement(biome),
        block_state_ids::YELLOW_TERRACOTTA
        | block_state_ids::WHITE_TERRACOTTA
        | block_state_ids::LIGHT_GRAY_TERRACOTTA
        | block_state_ids::QUARTZ_BLOCK
        | block_state_ids::BONE_BLOCK
        | block_state_ids::END_STONE
        | block_state_ids::END_STONE_BRICKS
        | block_state_ids::SMOOTH_SANDSTONE
        | block_state_ids::CUT_SANDSTONE
        | block_state_ids::CHISELED_SANDSTONE => pale_dry_replacement(biome),
        block_state_ids::TERRACOTTA
        | block_state_ids::ORANGE_TERRACOTTA
        | block_state_ids::BROWN_TERRACOTTA
        | block_state_ids::RED_TERRACOTTA
        | block_state_ids::SMOOTH_RED_SANDSTONE
        | block_state_ids::CUT_RED_SANDSTONE
        | block_state_ids::CHISELED_RED_SANDSTONE
        | block_state_ids::PACKED_MUD
        | block_state_ids::MUD_BRICKS
        | block_state_ids::DRIPSTONE_BLOCK => warm_earth_replacement(biome),
        block_state_ids::GRAY_TERRACOTTA
        | block_state_ids::BLACK_TERRACOTTA
        | block_state_ids::CYAN_TERRACOTTA
        | block_state_ids::BLACK_CONCRETE => dark_rock_replacement(biome),
        block_state_ids::OAK_LOG | block_state_ids::JUNGLE_LOG => block_state_ids::DIRT,
        _ => fallback_for_surface_biome(biome),
    }
}

pub fn production_surface_filler(
    filler: i32,
    production_top: i32,
    biome: &str,
    water: bool,
) -> i32 {
    if water {
        return production_top;
    }
    if is_allowed_production_top(filler, biome) {
        return filler;
    }
    filler_for_production_top(production_top)
}

fn filler_for_production_top(top: i32) -> i32 {
    match top {
        block_state_ids::SAND
        | block_state_ids::SANDSTONE
        | block_state_ids::SMOOTH_SANDSTONE
        | block_state_ids::CUT_SANDSTONE
        | block_state_ids::CHISELED_SANDSTONE
        | block_state_ids::RED_SAND
        | block_state_ids::SMOOTH_RED_SANDSTONE
        | block_state_ids::CUT_RED_SANDSTONE
        | block_state_ids::CHISELED_RED_SANDSTONE
        | block_state_ids::ROOTED_DIRT
        | block_state_ids::MYCELIUM
        | block_state_ids::PACKED_MUD
        | block_state_ids::TERRACOTTA
        | block_state_ids::ORANGE_TERRACOTTA
        | block_state_ids::BROWN_TERRACOTTA
        | block_state_ids::RED_TERRACOTTA
        | block_state_ids::YELLOW_TERRACOTTA
        | block_state_ids::WHITE_TERRACOTTA
        | block_state_ids::LIGHT_GRAY_TERRACOTTA
        | block_state_ids::GRAY_TERRACOTTA
        | block_state_ids::DRIPSTONE_BLOCK => top,
        block_state_ids::STONE
        | block_state_ids::DEEPSLATE
        | block_state_ids::GRAVEL
        | block_state_ids::CLAY
        | block_state_ids::CALCITE
        | block_state_ids::TUFF
        | block_state_ids::ANDESITE
        | block_state_ids::GRANITE
        | block_state_ids::DIORITE => block_state_ids::STONE,
        block_state_ids::MUD => block_state_ids::MUD,
        block_state_ids::SNOW_BLOCK => block_state_ids::SNOW_BLOCK,
        _ => block_state_ids::DIRT,
    }
}

fn is_allowed_natural_surface_top(block: i32) -> bool {
    matches!(
        block,
        block_state_ids::GRASS_BLOCK
            | block_state_ids::DIRT
            | block_state_ids::COARSE_DIRT
            | block_state_ids::ROOTED_DIRT
            | block_state_ids::PODZOL
            | block_state_ids::MYCELIUM
            | block_state_ids::MOSS_BLOCK
            | block_state_ids::MUD
            | block_state_ids::SAND
            | block_state_ids::RED_SAND
            | block_state_ids::SANDSTONE
            | block_state_ids::GRAVEL
            | block_state_ids::CLAY
            | block_state_ids::STONE
            | block_state_ids::DEEPSLATE
            | block_state_ids::TUFF
            | block_state_ids::ANDESITE
            | block_state_ids::GRANITE
            | block_state_ids::DIORITE
            | block_state_ids::CALCITE
            | block_state_ids::SNOW_BLOCK
            | block_state_ids::ICE
    ) || is_natural_photo_palette_extension_top(block)
}

fn is_natural_photo_palette_extension_top(block: i32) -> bool {
    matches!(
        block,
        block_state_ids::PACKED_MUD
            | block_state_ids::TERRACOTTA
            | block_state_ids::ORANGE_TERRACOTTA
            | block_state_ids::BROWN_TERRACOTTA
            | block_state_ids::RED_TERRACOTTA
            | block_state_ids::YELLOW_TERRACOTTA
            | block_state_ids::WHITE_TERRACOTTA
            | block_state_ids::LIGHT_GRAY_TERRACOTTA
            | block_state_ids::GRAY_TERRACOTTA
            | block_state_ids::SMOOTH_SANDSTONE
            | block_state_ids::CUT_SANDSTONE
            | block_state_ids::CHISELED_SANDSTONE
            | block_state_ids::SMOOTH_RED_SANDSTONE
            | block_state_ids::CUT_RED_SANDSTONE
            | block_state_ids::CHISELED_RED_SANDSTONE
            | block_state_ids::DRIPSTONE_BLOCK
    )
}

fn is_photo_preserved_natural_surface_column(column: &EarthSurfaceColumn) -> bool {
    is_photo_material_driven_column(column)
        && is_photo_preserved_natural_surface_top(column.top_block_state_id)
}

fn is_photo_preserved_natural_surface_top(block: i32) -> bool {
    if block == block_state_ids::GRASS_BLOCK {
        return false;
    }
    is_allowed_natural_surface_top(block)
}

fn is_sand_like_surface(block: i32) -> bool {
    matches!(
        block,
        block_state_ids::SAND
            | block_state_ids::RED_SAND
            | block_state_ids::SANDSTONE
            | block_state_ids::SMOOTH_SANDSTONE
            | block_state_ids::CUT_SANDSTONE
            | block_state_ids::CHISELED_SANDSTONE
            | block_state_ids::SMOOTH_RED_SANDSTONE
            | block_state_ids::CUT_RED_SANDSTONE
            | block_state_ids::CHISELED_RED_SANDSTONE
    )
}

fn is_coastal_native_surface_top(block: i32) -> bool {
    matches!(
        block,
        block_state_ids::SAND
            | block_state_ids::RED_SAND
            | block_state_ids::SANDSTONE
            | block_state_ids::GRAVEL
            | block_state_ids::CLAY
            | block_state_ids::STONE
            | block_state_ids::MUD
    )
}

fn coastal_replacement(block: i32, biome: &str) -> i32 {
    match block {
        block_state_ids::OAK_LEAVES
        | block_state_ids::JUNGLE_LEAVES
        | block_state_ids::DARK_OAK_LEAVES
        | block_state_ids::SPRUCE_LEAVES
        | block_state_ids::GREEN_TERRACOTTA
        | block_state_ids::LIME_TERRACOTTA => {
            if is_wet_surface_biome(biome) {
                block_state_ids::MUD
            } else {
                block_state_ids::GRASS_BLOCK
            }
        }
        block_state_ids::YELLOW_TERRACOTTA
        | block_state_ids::WHITE_TERRACOTTA
        | block_state_ids::LIGHT_GRAY_TERRACOTTA
        | block_state_ids::QUARTZ_BLOCK
        | block_state_ids::BONE_BLOCK
        | block_state_ids::END_STONE
        | block_state_ids::END_STONE_BRICKS
        | block_state_ids::SMOOTH_SANDSTONE
        | block_state_ids::CUT_SANDSTONE
        | block_state_ids::CHISELED_SANDSTONE => block_state_ids::SAND,
        block_state_ids::TERRACOTTA
        | block_state_ids::ORANGE_TERRACOTTA
        | block_state_ids::BROWN_TERRACOTTA
        | block_state_ids::RED_TERRACOTTA
        | block_state_ids::SMOOTH_RED_SANDSTONE
        | block_state_ids::CUT_RED_SANDSTONE
        | block_state_ids::CHISELED_RED_SANDSTONE
        | block_state_ids::PACKED_MUD
        | block_state_ids::MUD_BRICKS
        | block_state_ids::DRIPSTONE_BLOCK => {
            if is_dry_surface_biome(biome) {
                block_state_ids::RED_SAND
            } else {
                block_state_ids::SAND
            }
        }
        block_state_ids::GRAY_TERRACOTTA
        | block_state_ids::BLACK_TERRACOTTA
        | block_state_ids::CYAN_TERRACOTTA
        | block_state_ids::BLACK_CONCRETE => {
            if is_wet_surface_biome(biome) {
                block_state_ids::CLAY
            } else {
                block_state_ids::SAND
            }
        }
        _ => block_state_ids::SAND,
    }
}

fn vegetated_replacement(biome: &str) -> i32 {
    if is_wet_surface_biome(biome) {
        return block_state_ids::MUD;
    }
    if is_forest_surface_biome(biome) || is_jungle_surface_biome(biome) {
        return block_state_ids::GRASS_BLOCK;
    }
    if is_dry_surface_biome(biome) {
        return block_state_ids::COARSE_DIRT;
    }
    block_state_ids::GRASS_BLOCK
}

fn pale_dry_replacement(biome: &str) -> i32 {
    if is_snowy_surface_biome(biome) {
        return block_state_ids::SNOW_BLOCK;
    }
    if is_rocky_surface_biome(biome) {
        return block_state_ids::CALCITE;
    }
    block_state_ids::SAND
}

fn warm_earth_replacement(biome: &str) -> i32 {
    if is_dry_surface_biome(biome) {
        return block_state_ids::RED_SAND;
    }
    if is_rocky_surface_biome(biome) {
        return block_state_ids::GRANITE;
    }
    if is_wet_surface_biome(biome) {
        return block_state_ids::MUD;
    }
    block_state_ids::COARSE_DIRT
}

fn dark_rock_replacement(biome: &str) -> i32 {
    if is_coast_surface_biome(biome) {
        return block_state_ids::STONE;
    }
    if is_wet_surface_biome(biome) {
        return block_state_ids::CLAY;
    }
    if is_snowy_surface_biome(biome) {
        return block_state_ids::STONE;
    }
    if is_rocky_surface_biome(biome) {
        block_state_ids::DEEPSLATE
    } else {
        block_state_ids::STONE
    }
}

fn fallback_for_surface_biome(biome: &str) -> i32 {
    if is_coast_surface_biome(biome) {
        return block_state_ids::SAND;
    }
    if is_snowy_surface_biome(biome) {
        return block_state_ids::SNOW_BLOCK;
    }
    if is_dry_surface_biome(biome) {
        return block_state_ids::SAND;
    }
    if is_rocky_surface_biome(biome) {
        return block_state_ids::STONE;
    }
    if is_wet_surface_biome(biome) {
        return block_state_ids::MUD;
    }
    block_state_ids::GRASS_BLOCK
}

fn should_soften_temperate_rock(column: &EarthSurfaceColumn, top: i32) -> bool {
    if !is_drab_temperate_surface(top)
        || is_snowy_surface_biome(&column.biome_id)
        || is_arid_bare_surface_biome(&column.biome_id)
        || is_coast_surface_biome(&column.biome_id)
    {
        return false;
    }
    let y = column.ground_surface_y;
    if is_hard_alpine_surface_biome(&column.biome_id) {
        return y <= SEA_LEVEL_Y + 82;
    }
    if is_temperate_vegetated_surface_biome(&column.biome_id) {
        return y <= SEA_LEVEL_Y + 90;
    }
    if is_rocky_surface_biome(&column.biome_id) {
        return y <= SEA_LEVEL_Y + 76;
    }
    y <= SEA_LEVEL_Y + 40
}

fn should_regreen_temperate_earth(column: &EarthSurfaceColumn, top: i32) -> bool {
    if !is_drab_temperate_earth(top)
        || is_snowy_surface_biome(&column.biome_id)
        || is_arid_bare_surface_biome(&column.biome_id)
        || is_coast_surface_biome(&column.biome_id)
    {
        return false;
    }
    let y = column.ground_surface_y;
    if is_temperate_vegetated_surface_biome(&column.biome_id)
        || is_wet_surface_biome(&column.biome_id)
    {
        return y <= SEA_LEVEL_Y + 96;
    }
    if is_dry_surface_biome(&column.biome_id) {
        return y <= SEA_LEVEL_Y + 64;
    }
    y <= SEA_LEVEL_Y + 48
}

fn stable_water_floor_replacement(column: &EarthSurfaceColumn) -> i32 {
    let depth = 1.max(column.water_surface_y - column.ground_surface_y);
    if depth <= 6 || is_wet_surface_biome(&column.biome_id) {
        block_state_ids::CLAY
    } else if depth <= 18 {
        block_state_ids::GRAVEL
    } else {
        block_state_ids::STONE
    }
}

fn temperate_rock_replacement(biome: &str) -> i32 {
    if is_wet_surface_biome(biome) {
        return block_state_ids::MOSS_BLOCK;
    }
    let lower = lower_surface_biome(biome);
    if lower.contains("taiga") || lower.contains("old_growth") {
        return block_state_ids::PODZOL;
    }
    block_state_ids::GRASS_BLOCK
}

fn temperate_earth_replacement(biome: &str) -> i32 {
    if is_wet_surface_biome(biome) {
        block_state_ids::MOSS_BLOCK
    } else {
        block_state_ids::GRASS_BLOCK
    }
}

fn is_bare_rock_like_surface(block: i32) -> bool {
    matches!(
        block,
        block_state_ids::GRAVEL
            | block_state_ids::STONE
            | block_state_ids::DEEPSLATE
            | block_state_ids::TUFF
            | block_state_ids::ANDESITE
            | block_state_ids::GRANITE
            | block_state_ids::DIORITE
    )
}

fn is_drab_temperate_surface(block: i32) -> bool {
    is_bare_rock_like_surface(block)
        || matches!(block, block_state_ids::CLAY | block_state_ids::CALCITE)
}

fn is_drab_temperate_earth(block: i32) -> bool {
    matches!(
        block,
        block_state_ids::MUD
            | block_state_ids::COARSE_DIRT
            | block_state_ids::PODZOL
            | block_state_ids::MYCELIUM
            | block_state_ids::DIRT
    )
}

fn is_temperate_vegetated_surface_biome(biome: &str) -> bool {
    let lower = lower_surface_biome(biome);
    lower.contains("forest")
        || lower.contains("taiga")
        || lower.contains("jungle")
        || lower.contains("plains")
        || lower.contains("meadow")
        || lower.contains("grove")
        || lower.contains("windswept")
        || lower.contains("mountain")
        || lower.contains("hill")
}

fn is_hard_alpine_surface_biome(biome: &str) -> bool {
    let lower = lower_surface_biome(biome);
    lower.contains("stony") || lower.contains("peak") || lower.contains("jagged")
}

fn is_coast_surface_biome(biome: &str) -> bool {
    let lower = lower_surface_biome(biome);
    lower.contains("beach")
        || lower.contains("ocean")
        || lower.contains("river")
        || lower.contains("shore")
}

fn is_dry_surface_biome(biome: &str) -> bool {
    let lower = lower_surface_biome(biome);
    lower.contains("desert")
        || lower.contains("badlands")
        || lower.contains("savanna")
        || lower.contains("steppe")
        || lower.contains("grassland")
}

fn is_arid_bare_surface_biome(biome: &str) -> bool {
    let lower = lower_surface_biome(biome);
    lower.contains("desert") || lower.contains("badlands") || lower.contains("steppe")
}

fn is_true_sandy_land_biome(biome: &str) -> bool {
    let lower = lower_surface_biome(biome);
    lower.contains("desert") || lower.contains("badlands")
}

fn is_wet_surface_biome(biome: &str) -> bool {
    let lower = lower_surface_biome(biome);
    lower.contains("swamp") || lower.contains("mangrove") || lower.contains("wetland")
}

fn is_snowy_surface_biome(biome: &str) -> bool {
    let lower = lower_surface_biome(biome);
    lower.contains("snow") || lower.contains("frozen") || lower.contains("ice")
}

fn is_rocky_surface_biome(biome: &str) -> bool {
    let lower = lower_surface_biome(biome);
    lower.contains("mountain")
        || lower.contains("peak")
        || lower.contains("stony")
        || lower.contains("windswept")
}

fn is_forest_surface_biome(biome: &str) -> bool {
    let lower = lower_surface_biome(biome);
    lower.contains("forest") || lower.contains("taiga")
}

fn is_jungle_surface_biome(biome: &str) -> bool {
    lower_surface_biome(biome).contains("jungle")
}

const IMMEDIATE_COAST_FACTOR: f64 = 0.985;
const SHALLOW_WATER_COAST_FACTOR: f64 = 0.86;
const NEAR_COAST_FACTOR: f64 = 0.70;

pub fn clean_coastal_surface_columns(
    columns: &[EarthSurfaceColumn],
    coast_factors: &[f64],
    width: usize,
) -> Result<Vec<EarthSurfaceColumn>> {
    if width == 0 || !columns.len().is_multiple_of(width) || coast_factors.len() != columns.len() {
        return Err(SurfaceError::invalid(
            "width must divide columns and coastFactors must match columns",
        ));
    }
    Ok(columns
        .iter()
        .zip(coast_factors.iter())
        .map(|(column, &coast_factor)| clean_coastal_surface_column(column, coast_factor))
        .collect())
}

pub fn clean_coastal_surface_column(
    column: &EarthSurfaceColumn,
    coast_factor: f64,
) -> EarthSurfaceColumn {
    let cleaned = if column.water {
        clean_water_surface_column(column, coast_factor)
    } else {
        clean_land_surface_column(column, coast_factor)
    };
    sanitize_surface_column_for_production(&cleaned)
}

fn clean_land_surface_column(column: &EarthSurfaceColumn, coast_factor: f64) -> EarthSurfaceColumn {
    if coast_factor < NEAR_COAST_FACTOR
        || column.ground_surface_y > SEA_LEVEL_Y + 4
        || (!is_rock_surface_top(column.top_block_state_id)
            && !is_sand_surface_top(column.top_block_state_id))
    {
        return column.clone();
    }
    let top = land_shore_top(&column.biome_id);
    replace_surface_blocks(column, top, land_shore_filler(top), "coastal-cleanup")
}

fn clean_water_surface_column(
    column: &EarthSurfaceColumn,
    coast_factor: f64,
) -> EarthSurfaceColumn {
    if coast_factor < SHALLOW_WATER_COAST_FACTOR
        || (!is_rock_surface_top(column.top_block_state_id)
            && !is_sand_surface_top(column.top_block_state_id))
    {
        return column.clone();
    }
    let depth = 0.max(column.water_surface_y - column.ground_surface_y);
    if depth > 9 && coast_factor < IMMEDIATE_COAST_FACTOR {
        return column.clone();
    }
    let top = water_floor_top(&column.biome_id, depth);
    replace_surface_blocks(column, top, top, "coastal-water-cleanup")
}

fn land_shore_top(biome: &str) -> i32 {
    let lower = lower_surface_biome(biome);
    if lower.contains("swamp") || lower.contains("mangrove") || lower.contains("wetland") {
        return block_state_ids::MUD;
    }
    if lower.contains("badlands") || lower.contains("savanna") {
        return block_state_ids::COARSE_DIRT;
    }
    if lower.contains("desert") {
        return block_state_ids::GRASS_BLOCK;
    }
    block_state_ids::GRASS_BLOCK
}

fn land_shore_filler(top: i32) -> i32 {
    match top {
        block_state_ids::SAND | block_state_ids::RED_SAND | block_state_ids::MUD => top,
        block_state_ids::STONE
        | block_state_ids::DEEPSLATE
        | block_state_ids::GRAVEL
        | block_state_ids::CLAY
        | block_state_ids::CALCITE
        | block_state_ids::TUFF
        | block_state_ids::ANDESITE
        | block_state_ids::GRANITE
        | block_state_ids::DIORITE => block_state_ids::STONE,
        _ => block_state_ids::DIRT,
    }
}

fn water_floor_top(biome: &str, depth: i32) -> i32 {
    let lower = lower_surface_biome(biome);
    if lower.contains("swamp") || lower.contains("mangrove") || lower.contains("wetland") {
        return block_state_ids::CLAY;
    }
    if depth <= 6 {
        return block_state_ids::CLAY;
    }
    if depth <= 18 {
        return block_state_ids::GRAVEL;
    }
    block_state_ids::STONE
}

fn is_rock_surface_top(block: i32) -> bool {
    matches!(
        block,
        block_state_ids::GRAVEL
            | block_state_ids::STONE
            | block_state_ids::DEEPSLATE
            | block_state_ids::TUFF
            | block_state_ids::ANDESITE
            | block_state_ids::GRANITE
            | block_state_ids::DIORITE
            | block_state_ids::CALCITE
    )
}

fn is_sand_surface_top(block: i32) -> bool {
    matches!(
        block,
        block_state_ids::SAND | block_state_ids::RED_SAND | block_state_ids::SANDSTONE
    )
}

fn replace_surface_blocks(
    column: &EarthSurfaceColumn,
    top: i32,
    filler: i32,
    source_suffix: &str,
) -> EarthSurfaceColumn {
    if top == column.top_block_state_id && filler == column.filler_block_state_id {
        return column.clone();
    }
    let mut replaced = EarthSurfaceColumn::new(
        column.water,
        column.ground_surface_y,
        column.water_surface_y,
        top,
        filler,
        column.biome_id.clone(),
        format!("{}+{}", column.decision_source, source_suffix),
    );
    replaced.terrain_token_source = column.terrain_token_source;
    replaced.data_evidence_flags = column.data_evidence_flags;
    replaced
}

fn lower_surface_biome(biome: &str) -> String {
    biome.to_ascii_lowercase()
}

const SURFACE_BIOME_BELOW_PADDING: i32 = 4;
const SURFACE_BIOME_ABOVE_PADDING: i32 = 12;
const STATIC_CARRIER_MIN_IMPROVEMENT: i64 = 900;
const STATIC_CARRIER_MIN_Y: i32 = SEA_LEVEL_Y + 5;

#[derive(Clone, Debug, Eq, PartialEq)]
struct SurfaceBiomeCell {
    biome: String,
    min_y: i32,
    max_y: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StaticCarrier {
    block: i32,
    error: i64,
}

pub fn apply_surface_biome_cells(
    chunk: &mut ChunkModel,
    biome_by_local_column: &[String],
    min_y_by_local_column: &[i32],
    max_y_by_local_column: &[i32],
    chunk_default_biome: &str,
) -> Result<()> {
    let expected_columns = CHUNK_WIDTH * CHUNK_WIDTH;
    if biome_by_local_column.len() != expected_columns
        || min_y_by_local_column.len() != expected_columns
        || max_y_by_local_column.len() != expected_columns
    {
        return Err(SurfaceError::invalid(
            "biome arrays must have one entry per chunk column",
        ));
    }
    for cell_z in 0..BIOME_CELL_WIDTH {
        for cell_x in 0..BIOME_CELL_WIDTH {
            let cell = render_aware_surface_biome_cell(
                chunk,
                biome_by_local_column,
                min_y_by_local_column,
                max_y_by_local_column,
                cell_x,
                cell_z,
            )?;
            apply_static_carrier_fallbacks(
                chunk,
                biome_by_local_column,
                min_y_by_local_column,
                cell_x,
                cell_z,
                &cell.biome,
            )?;
            if cell.biome == chunk_default_biome {
                continue;
            }
            let cell_origin_x = (cell_x * BIOME_CELL_WIDTH) as i32;
            let cell_origin_z = (cell_z * BIOME_CELL_WIDTH) as i32;
            let min_y = align_down_surface_biome_y(clamp_chunk_y(
                chunk,
                cell.min_y - SURFACE_BIOME_BELOW_PADDING,
            ));
            let max_y = align_down_surface_biome_y(clamp_chunk_y(
                chunk,
                cell.max_y + SURFACE_BIOME_ABOVE_PADDING,
            ));
            let mut y = min_y;
            while y <= max_y {
                chunk.set_biome_id_at(cell_origin_x, y, cell_origin_z, cell.biome.clone())?;
                y += BIOME_CELL_WIDTH as i32;
            }
        }
    }
    Ok(())
}

fn render_aware_surface_biome_cell(
    chunk: &ChunkModel,
    biome_by_local_column: &[String],
    min_y_by_local_column: &[i32],
    max_y_by_local_column: &[i32],
    cell_x: usize,
    cell_z: usize,
) -> Result<SurfaceBiomeCell> {
    let mut counts = BTreeMap::<String, i32>::new();
    let mut min_y = i32::MAX;
    let mut max_y = i32::MIN;
    for dz in 0..BIOME_CELL_WIDTH {
        for dx in 0..BIOME_CELL_WIDTH {
            let local_x = (cell_x * BIOME_CELL_WIDTH) + dx;
            let local_z = (cell_z * BIOME_CELL_WIDTH) + dz;
            let column_index = (local_z * CHUNK_WIDTH) + local_x;
            *counts
                .entry(biome_by_local_column[column_index].clone())
                .or_insert(0) += 1;
            min_y = min_y.min(min_y_by_local_column[column_index]);
            max_y = max_y.max(max_y_by_local_column[column_index]);
        }
    }
    Ok(SurfaceBiomeCell {
        biome: best_render_surface_biome(
            chunk,
            biome_by_local_column,
            min_y_by_local_column,
            &counts,
            cell_x,
            cell_z,
        )?,
        min_y,
        max_y,
    })
}

fn best_render_surface_biome(
    chunk: &ChunkModel,
    biome_by_local_column: &[String],
    min_y_by_local_column: &[i32],
    counts: &BTreeMap<String, i32>,
    cell_x: usize,
    cell_z: usize,
) -> Result<String> {
    let dominant = dominant_surface_biome(counts);
    let mut best = dominant.clone();
    let mut best_error = i64::MAX;
    let mut best_count = -1;
    for (candidate, &count) in counts {
        let error = render_error_for_surface_biome_cell(
            chunk,
            biome_by_local_column,
            min_y_by_local_column,
            cell_x,
            cell_z,
            candidate,
        )?;
        if error < best_error
            || (error == best_error && count > best_count)
            || (error == best_error && count == best_count && candidate == &dominant)
        {
            best = candidate.clone();
            best_error = error;
            best_count = count;
        }
    }
    Ok(best)
}

fn render_error_for_surface_biome_cell(
    chunk: &ChunkModel,
    biome_by_local_column: &[String],
    min_y_by_local_column: &[i32],
    cell_x: usize,
    cell_z: usize,
    candidate_biome: &str,
) -> Result<i64> {
    let mut error = 0_i64;
    for dz in 0..BIOME_CELL_WIDTH {
        for dx in 0..BIOME_CELL_WIDTH {
            let local_x = (cell_x * BIOME_CELL_WIDTH) + dx;
            let local_z = (cell_z * BIOME_CELL_WIDTH) + dz;
            let column_index = (local_z * CHUNK_WIDTH) + local_x;
            let top = chunk.get_block_state_id(
                local_x as i32,
                min_y_by_local_column[column_index],
                local_z as i32,
            )?;
            let intended = render_surface_color(top, Some(&biome_by_local_column[column_index]));
            let actual = render_surface_color(top, Some(candidate_biome));
            error += squared_rgb_distance(actual, intended);
        }
    }
    Ok(error)
}

fn apply_static_carrier_fallbacks(
    chunk: &mut ChunkModel,
    biome_by_local_column: &[String],
    min_y_by_local_column: &[i32],
    cell_x: usize,
    cell_z: usize,
    cell_biome: &str,
) -> Result<()> {
    for dz in 0..BIOME_CELL_WIDTH {
        for dx in 0..BIOME_CELL_WIDTH {
            let local_x = (cell_x * BIOME_CELL_WIDTH) + dx;
            let local_z = (cell_z * BIOME_CELL_WIDTH) + dz;
            let column_index = (local_z * CHUNK_WIDTH) + local_x;
            let y = min_y_by_local_column[column_index];
            let mut top = chunk.get_block_state_id(local_x as i32, y, local_z as i32)?;
            let column_biome = &biome_by_local_column[column_index];
            if !is_allowed_production_top(top, column_biome) {
                top = production_surface_top(top, column_biome);
                chunk.set_block_state_id(local_x as i32, y, local_z as i32, top)?;
            }
            if y < STATIC_CARRIER_MIN_Y || !is_tinted_vegetation_block(top) {
                continue;
            }
            let intended_color = render_surface_color(top, Some(column_biome));
            let tinted_error =
                replacement_score(render_surface_color(top, Some(cell_biome)), intended_color);
            let carrier = nearest_static_carrier(intended_color, cell_biome);
            if is_sand_like_surface(carrier.block) && !is_naturally_sandy_biome(column_biome) {
                continue;
            }
            if carrier.block >= 0 && carrier.error + STATIC_CARRIER_MIN_IMPROVEMENT < tinted_error {
                chunk.set_block_state_id(local_x as i32, y, local_z as i32, carrier.block)?;
            }
        }
    }
    Ok(())
}

fn nearest_static_carrier(target_color: i32, biome: &str) -> StaticCarrier {
    let mut best = StaticCarrier {
        block: -1,
        error: i64::MAX,
    };
    for &block in STATIC_CARRIER_BLOCKS {
        if !is_allowed_production_top(block, biome) {
            continue;
        }
        let error = replacement_score(render_surface_color(block, None), target_color);
        if error < best.error {
            best = StaticCarrier { block, error };
        }
    }
    best
}

fn dominant_surface_biome(counts: &BTreeMap<String, i32>) -> String {
    let mut dominant = String::new();
    let mut max_count = -1;
    for (biome, &count) in counts {
        if count > max_count {
            dominant = biome.clone();
            max_count = count;
        }
    }
    dominant
}

fn clamp_chunk_y(chunk: &ChunkModel, y: i32) -> i32 {
    y.clamp(
        chunk.dimension().min_y(),
        chunk.dimension().max_y_inclusive(),
    )
}

fn align_down_surface_biome_y(y: i32) -> i32 {
    y.div_euclid(BIOME_CELL_WIDTH as i32) * BIOME_CELL_WIDTH as i32
}

const STATIC_CARRIER_BLOCKS: &[i32] = &[
    block_state_ids::GREEN_TERRACOTTA,
    block_state_ids::LIME_TERRACOTTA,
    block_state_ids::COARSE_DIRT,
    block_state_ids::ROOTED_DIRT,
    block_state_ids::PACKED_MUD,
    block_state_ids::MUD,
    block_state_ids::MYCELIUM,
    block_state_ids::PODZOL,
    block_state_ids::BLACK_TERRACOTTA,
    block_state_ids::GRAY_TERRACOTTA,
    block_state_ids::DEEPSLATE,
    block_state_ids::TUFF,
    block_state_ids::STONE,
    block_state_ids::ANDESITE,
    block_state_ids::GRAVEL,
    block_state_ids::CYAN_TERRACOTTA,
    block_state_ids::TERRACOTTA,
    block_state_ids::BROWN_TERRACOTTA,
    block_state_ids::YELLOW_TERRACOTTA,
    block_state_ids::ORANGE_TERRACOTTA,
    block_state_ids::RED_TERRACOTTA,
    block_state_ids::SAND,
    block_state_ids::SANDSTONE,
    block_state_ids::RED_SAND,
    block_state_ids::WHITE_TERRACOTTA,
    block_state_ids::LIGHT_GRAY_TERRACOTTA,
    block_state_ids::CALCITE,
    block_state_ids::BONE_BLOCK,
    block_state_ids::QUARTZ_BLOCK,
    block_state_ids::END_STONE,
    block_state_ids::END_STONE_BRICKS,
    block_state_ids::SMOOTH_SANDSTONE,
    block_state_ids::CUT_SANDSTONE,
    block_state_ids::CHISELED_SANDSTONE,
    block_state_ids::SMOOTH_RED_SANDSTONE,
    block_state_ids::CUT_RED_SANDSTONE,
    block_state_ids::CHISELED_RED_SANDSTONE,
    block_state_ids::MUD_BRICKS,
    block_state_ids::DRIPSTONE_BLOCK,
    block_state_ids::SNOW_BLOCK,
];

fn render_surface_color(block: i32, biome: Option<&str>) -> i32 {
    rendered_surface_color_for_block(block, biome, 0)
}

pub fn render_surface_rgb(block: i32, biome: Option<&str>) -> (u8, u8, u8) {
    let color = render_surface_color(block, biome);
    (
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
    )
}

fn rendered_surface_color_for_block(block: i32, biome: Option<&str>, fallback_rgb: i32) -> i32 {
    if block == block_state_ids::GRASS_BLOCK {
        return grass_render_color(biome);
    }
    if is_leaf_block(block) {
        return leaf_render_color(block, biome);
    }
    match block {
        block_state_ids::DIRT => rgb(115, 82, 48),
        block_state_ids::COARSE_DIRT => rgb(112, 92, 58),
        block_state_ids::ROOTED_DIRT => rgb(123, 91, 57),
        block_state_ids::PODZOL => rgb(94, 64, 36),
        block_state_ids::MOSS_BLOCK => rgb(75, 112, 41),
        block_state_ids::MYCELIUM => rgb(111, 99, 85),
        block_state_ids::MUD => rgb(72, 64, 54),
        block_state_ids::PACKED_MUD => rgb(142, 106, 79),
        block_state_ids::SAND => rgb(218, 202, 142),
        block_state_ids::RED_SAND => rgb(181, 97, 45),
        block_state_ids::SANDSTONE => rgb(213, 191, 121),
        block_state_ids::END_STONE => rgb(221, 214, 164),
        block_state_ids::END_STONE_BRICKS => rgb(216, 207, 163),
        block_state_ids::SMOOTH_SANDSTONE => rgb(216, 195, 137),
        block_state_ids::CUT_SANDSTONE => rgb(214, 190, 121),
        block_state_ids::CHISELED_SANDSTONE => rgb(215, 192, 129),
        block_state_ids::SMOOTH_RED_SANDSTONE => rgb(181, 101, 57),
        block_state_ids::CUT_RED_SANDSTONE => rgb(166, 91, 50),
        block_state_ids::CHISELED_RED_SANDSTONE => rgb(179, 98, 54),
        block_state_ids::MUD_BRICKS => rgb(137, 107, 78),
        block_state_ids::DRIPSTONE_BLOCK => rgb(138, 106, 89),
        block_state_ids::GRAVEL => rgb(112, 112, 106),
        block_state_ids::CLAY => rgb(145, 158, 160),
        block_state_ids::STONE => rgb(118, 122, 118),
        block_state_ids::DEEPSLATE => rgb(79, 79, 82),
        block_state_ids::TUFF => rgb(108, 109, 103),
        block_state_ids::ANDESITE => rgb(136, 136, 136),
        block_state_ids::GRANITE => rgb(149, 103, 85),
        block_state_ids::DIORITE => rgb(188, 188, 182),
        block_state_ids::TERRACOTTA => rgb(154, 102, 76),
        block_state_ids::ORANGE_TERRACOTTA => rgb(184, 92, 42),
        block_state_ids::RED_TERRACOTTA => rgb(143, 61, 47),
        block_state_ids::BROWN_TERRACOTTA => rgb(104, 66, 48),
        block_state_ids::YELLOW_TERRACOTTA => rgb(186, 133, 36),
        block_state_ids::WHITE_TERRACOTTA => rgb(210, 178, 161),
        block_state_ids::LIGHT_GRAY_TERRACOTTA => rgb(135, 107, 98),
        block_state_ids::GRAY_TERRACOTTA => rgb(57, 41, 35),
        block_state_ids::BLACK_TERRACOTTA => rgb(37, 23, 16),
        block_state_ids::GREEN_TERRACOTTA => rgb(76, 83, 42),
        block_state_ids::LIME_TERRACOTTA => rgb(104, 117, 53),
        block_state_ids::CYAN_TERRACOTTA => rgb(86, 91, 91),
        block_state_ids::BLACK_CONCRETE => rgb(8, 10, 15),
        block_state_ids::SNOW_BLOCK => rgb(232, 238, 236),
        block_state_ids::QUARTZ_BLOCK => rgb(236, 229, 220),
        block_state_ids::BONE_BLOCK => rgb(229, 224, 195),
        block_state_ids::CALCITE => rgb(224, 220, 204),
        _ => fallback_rgb,
    }
}

fn is_tinted_vegetation_block(block: i32) -> bool {
    block == block_state_ids::GRASS_BLOCK || is_leaf_block(block)
}

fn is_leaf_block(block: i32) -> bool {
    matches!(
        block,
        block_state_ids::OAK_LEAVES
            | block_state_ids::JUNGLE_LEAVES
            | block_state_ids::DARK_OAK_LEAVES
            | block_state_ids::SPRUCE_LEAVES
    )
}

fn grass_render_color(biome: Option<&str>) -> i32 {
    let Some(biome) = biome else {
        return rgb(99, 139, 63);
    };
    if biome.contains("sparse_jungle") {
        return rgb(72, 130, 54);
    }
    if biome.contains("bamboo_jungle") {
        return rgb(54, 136, 48);
    }
    if biome.contains("jungle") {
        return rgb(45, 118, 45);
    }
    if biome.contains("meadow") {
        return rgb(119, 151, 82);
    }
    if biome.contains("windswept_savanna") {
        return rgb(135, 141, 75);
    }
    if biome.contains("savanna") {
        return rgb(151, 153, 77);
    }
    if biome.contains("dark_forest") {
        return rgb(42, 82, 45);
    }
    if biome.contains("flower_forest") {
        return rgb(76, 135, 62);
    }
    if biome.contains("forest") {
        return rgb(64, 124, 54);
    }
    if biome.contains("sunflower_plains") {
        return rgb(117, 153, 68);
    }
    if biome.contains("taiga") {
        return rgb(88, 120, 92);
    }
    if biome.contains("swamp") {
        return rgb(73, 101, 56);
    }
    if biome.contains("snowy") {
        return rgb(157, 179, 145);
    }
    rgb(100, 146, 67)
}

fn leaf_render_color(block: i32, biome: Option<&str>) -> i32 {
    let Some(biome) = biome else {
        if block == block_state_ids::DARK_OAK_LEAVES {
            return rgb(26, 58, 28);
        }
        if block == block_state_ids::SPRUCE_LEAVES {
            return rgb(55, 82, 49);
        }
        return if block == block_state_ids::JUNGLE_LEAVES {
            rgb(35, 98, 37)
        } else {
            rgb(48, 89, 43)
        };
    };
    if block == block_state_ids::DARK_OAK_LEAVES {
        if biome.contains("savanna") {
            return rgb(72, 80, 38);
        }
        return rgb(25, 58, 28);
    }
    if block == block_state_ids::SPRUCE_LEAVES {
        if biome.contains("taiga") || biome.contains("snow") {
            return rgb(50, 76, 53);
        }
        return rgb(55, 82, 49);
    }
    if biome.contains("jungle") {
        return if block == block_state_ids::JUNGLE_LEAVES {
            rgb(30, 92, 34)
        } else {
            rgb(38, 91, 39)
        };
    }
    if biome.contains("dark_forest") {
        return rgb(25, 58, 28);
    }
    if biome.contains("forest") {
        return rgb(38, 86, 38);
    }
    if biome.contains("taiga") {
        return rgb(58, 86, 62);
    }
    if biome.contains("swamp") {
        return rgb(44, 70, 32);
    }
    if biome.contains("savanna") {
        return rgb(92, 96, 45);
    }
    if block == block_state_ids::JUNGLE_LEAVES {
        rgb(35, 98, 37)
    } else {
        rgb(48, 89, 43)
    }
}

fn squared_rgb_distance(left: i32, right: i32) -> i64 {
    let red = ((left >> 16) & 0xff) - ((right >> 16) & 0xff);
    let green = ((left >> 8) & 0xff) - ((right >> 8) & 0xff);
    let blue = (left & 0xff) - (right & 0xff);
    i64::from((red * red) + (green * green) + (blue * blue))
}

fn replacement_score(actual_color: i32, target_color: i32) -> i64 {
    let error = squared_rgb_distance(actual_color, target_color);
    if is_gray_olive_tint_target(target_color) {
        let luma_delta = luma10000(actual_color) - luma10000(target_color);
        return error + (i64::from(luma_delta) * i64::from(luma_delta) * 12 / 100_000_000);
    }
    if is_dark_green_tint_target(target_color) {
        let luma_delta = luma10000(actual_color) - luma10000(target_color);
        return error
            + (i64::from(luma_delta) * i64::from(luma_delta) * 8 / 100_000_000)
            + dark_green_hue_penalty(actual_color);
    }
    error
}

fn is_naturally_sandy_biome(biome: &str) -> bool {
    if biome.trim().is_empty() {
        return false;
    }
    let lower = biome.to_ascii_lowercase();
    lower.contains("desert") || lower.contains("badlands")
}

fn is_gray_olive_tint_target(color: i32) -> bool {
    let red = (color >> 16) & 0xff;
    let green = (color >> 8) & 0xff;
    let blue = color & 0xff;
    (120..=142).contains(&red)
        && (128..=146).contains(&green)
        && (65..=95).contains(&blue)
        && (red - green).abs() <= 16
}

fn is_dark_green_tint_target(color: i32) -> bool {
    let red = (color >> 16) & 0xff;
    let green = (color >> 8) & 0xff;
    let blue = color & 0xff;
    (18..=58).contains(&red)
        && (54..=88).contains(&green)
        && (18..=44).contains(&blue)
        && green >= red + 12
        && green >= blue + 20
}

fn dark_green_hue_penalty(color: i32) -> i64 {
    let red = (color >> 16) & 0xff;
    let green = (color >> 8) & 0xff;
    let blue = color & 0xff;
    let mut penalty = 0_i64;
    if green < red {
        let miss = red - green;
        penalty += 4_800 + i64::from(miss * miss * 18);
    }
    if green < blue + 8 {
        let miss = (blue + 8) - green;
        penalty += 1_800 + i64::from(miss * miss * 12);
    }
    penalty
}

fn luma10000(color: i32) -> i32 {
    let red = (color >> 16) & 0xff;
    let green = (color >> 8) & 0xff;
    let blue = color & 0xff;
    (red * 2126) + (green * 7152) + (blue * 722)
}

const fn rgb(red: i32, green: i32, blue: i32) -> i32 {
    ((red & 0xff) << 16) | ((green & 0xff) << 8) | (blue & 0xff)
}

pub fn require_valid_vertical_scale(vertical_scale: f64) -> Result<f64> {
    if !vertical_scale.is_finite()
        || !(MIN_VERTICAL_SCALE..=MAX_VERTICAL_SCALE).contains(&vertical_scale)
    {
        return Err(SurfaceError::invalid(format!(
            "verticalScale must be between {MIN_VERTICAL_SCALE} and {MAX_VERTICAL_SCALE}"
        )));
    }
    Ok(vertical_scale)
}

pub fn ground_surface_y(elevation_meters: f64, water: bool) -> i32 {
    ground_surface_y_at(elevation_meters, 0.0, 0.0, water)
}

pub fn ground_surface_y_at(
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    water: bool,
) -> i32 {
    if water {
        let depth_blocks = ocean_depth_blocks(
            elevation_meters,
            longitude,
            latitude,
            DEFAULT_VERTICAL_SCALE,
        );
        return clamp_surface_y(SEA_LEVEL_Y - depth_blocks);
    }
    let y = SEA_LEVEL_Y.wrapping_add(java_math_round_double_to_narrowed_i32(
        elevation_meters / ELEVATION_METERS_PER_BLOCK,
    ));
    clamp_surface_y(y)
}

pub fn biome_id(
    _elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    water: bool,
    surface_y: i32,
    coast_factor: f64,
) -> String {
    let abs_lat = latitude.abs();
    if water {
        if abs_lat >= 70.0 {
            return "minecraft:frozen_ocean".to_string();
        }
        let depth = 1.max(SEA_LEVEL_Y - surface_y);
        if depth >= 28 {
            if abs_lat <= 34.0 {
                return "minecraft:deep_lukewarm_ocean".to_string();
            }
            if abs_lat >= 56.0 {
                return "minecraft:deep_cold_ocean".to_string();
            }
            return "minecraft:deep_ocean".to_string();
        }
        if abs_lat <= 23.5 {
            return "minecraft:warm_ocean".to_string();
        }
        if abs_lat <= 38.0 {
            return "minecraft:lukewarm_ocean".to_string();
        }
        if abs_lat >= 55.0 {
            return "minecraft:cold_ocean".to_string();
        }
        return "minecraft:ocean".to_string();
    }
    if is_beach_band(coast_factor, surface_y, longitude, latitude) {
        return "minecraft:beach".to_string();
    }
    if desert_score(longitude, latitude) >= 0.58 {
        return "minecraft:desert".to_string();
    }
    if abs_lat >= 66.0 || surface_y >= 210 || (surface_y >= 165 && abs_lat >= 28.0) {
        return "minecraft:snowy_plains".to_string();
    }
    if jungle_score(longitude, latitude) >= 0.55 {
        return "minecraft:jungle".to_string();
    }
    if (25.0..=55.0).contains(&abs_lat) && forest_score(longitude, latitude) >= 0.35 {
        return "minecraft:forest".to_string();
    }
    if abs_lat <= 23.5 {
        return "minecraft:savanna".to_string();
    }
    "minecraft:plains".to_string()
}

fn height_only_chunk(
    chunk_x: i32,
    chunk_z: i32,
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
) -> Result<ChunkBuild> {
    let mut chunk = ChunkModel::overworld(chunk_x, chunk_z);
    let mut min_surface_y = i32::MAX;
    let mut max_surface_y = i32::MIN;
    for local_z in 0..CHUNK_WIDTH as i32 {
        for local_x in 0..CHUNK_WIDTH as i32 {
            let global_block_x = chunk_x
                .wrapping_mul(CHUNK_WIDTH as i32)
                .wrapping_add(local_x);
            let global_block_z = chunk_z
                .wrapping_mul(CHUNK_WIDTH as i32)
                .wrapping_add(local_z);
            let map_x = global_block_x.wrapping_add(mapping.width_blocks / 2);
            let map_z = global_block_z.wrapping_add(mapping.height_blocks / 2);
            let elevation = if map_x >= 0
                && map_x < mapping.width_blocks
                && map_z >= 0
                && map_z < mapping.height_blocks
            {
                sampler.bilinear_meters(
                    mapping.longitude_for_block_x(map_x)?,
                    mapping.latitude_for_block_z(map_z)?,
                )?
            } else {
                0.0
            };
            let surface_y = surface_y_for_elevation_meters(elevation);
            min_surface_y = min_surface_y.min(surface_y);
            max_surface_y = max_surface_y.max(surface_y);
            fill_height_only_column(&mut chunk, local_x, local_z, surface_y)?;
        }
    }
    Ok(ChunkBuild {
        chunk,
        min_surface_y,
        max_surface_y,
    })
}

fn fill_height_only_column(
    chunk: &mut ChunkModel,
    local_x: i32,
    local_z: i32,
    surface_y: i32,
) -> Result<()> {
    chunk.set_block_state_id(local_x, -64, local_z, block_state_ids::BEDROCK)?;
    if surface_y <= -63 {
        chunk.set_block_state_id(local_x, surface_y, local_z, block_state_ids::STONE)?;
        return Ok(());
    }
    chunk.fill_column(
        local_x,
        local_z,
        -63,
        (-63).max(surface_y - 4),
        block_state_ids::STONE,
    )?;
    chunk.fill_column(
        local_x,
        local_z,
        (-63).max(surface_y - 3),
        surface_y - 1,
        block_state_ids::DIRT,
    )?;
    chunk.set_block_state_id(local_x, surface_y, local_z, block_state_ids::GRASS_BLOCK)?;
    Ok(())
}

pub fn apply_osm_surface_overlay(
    chunk: &mut ChunkModel,
    local_x: i32,
    local_z: i32,
    column: &EarthSurfaceColumn,
    mask: Option<&OsmRegionFeatureMask>,
    region_local_x: i32,
    region_local_z: i32,
) -> Result<i32> {
    let Some(mask) = mask else {
        return Ok(0);
    };
    let waterway = mask.waterway_at(region_local_x, region_local_z)?;
    let building = mask.building_at(region_local_x, region_local_z)?;
    let road = mask.road_at(region_local_x, region_local_z)?;
    let landuse = mask.landuse_at(region_local_x, region_local_z)?;

    if waterway {
        apply_osm_waterway(chunk, local_x, local_z, column)?;
        return Ok(OSM_OVERLAY_WATERWAY);
    }
    if !column.water && building {
        apply_osm_building(chunk, local_x, local_z, column)?;
        return Ok(OSM_OVERLAY_BUILDING);
    }
    if !column.water && road {
        apply_osm_road(chunk, local_x, local_z, column)?;
        return Ok(OSM_OVERLAY_ROAD);
    }
    if !column.water && landuse {
        apply_osm_landuse(chunk, local_x, local_z, column)?;
        return Ok(OSM_OVERLAY_LANDUSE);
    }
    Ok(0)
}

pub fn osm_overlay_has_waterway(flags: i32) -> bool {
    (flags & OSM_OVERLAY_WATERWAY) != 0
}

pub fn osm_overlay_has_road(flags: i32) -> bool {
    (flags & OSM_OVERLAY_ROAD) != 0
}

pub fn osm_overlay_has_landuse(flags: i32) -> bool {
    (flags & OSM_OVERLAY_LANDUSE) != 0
}

pub fn osm_overlay_has_building(flags: i32) -> bool {
    (flags & OSM_OVERLAY_BUILDING) != 0
}

fn apply_osm_road(
    chunk: &mut ChunkModel,
    local_x: i32,
    local_z: i32,
    column: &EarthSurfaceColumn,
) -> Result<()> {
    let surface_y = column.ground_surface_y;
    chunk.set_block_state_id(local_x, surface_y, local_z, block_state_ids::STONE)?;
    if surface_y > chunk.dimension().min_y() {
        chunk.set_block_state_id(local_x, surface_y - 1, local_z, block_state_ids::STONE)?;
    }
    Ok(())
}

fn apply_osm_building(
    chunk: &mut ChunkModel,
    local_x: i32,
    local_z: i32,
    column: &EarthSurfaceColumn,
) -> Result<()> {
    let surface_y = column.ground_surface_y;
    chunk.set_block_state_id(local_x, surface_y, local_z, block_state_ids::STONE_BRICKS)?;
    if surface_y < chunk.dimension().max_y_inclusive() {
        chunk.set_block_state_id(
            local_x,
            surface_y + 1,
            local_z,
            block_state_ids::STONE_BRICKS,
        )?;
    }
    Ok(())
}

fn apply_osm_landuse(
    chunk: &mut ChunkModel,
    local_x: i32,
    local_z: i32,
    column: &EarthSurfaceColumn,
) -> Result<()> {
    chunk.set_block_state_id(
        local_x,
        column.ground_surface_y,
        local_z,
        block_state_ids::GRASS_BLOCK,
    )?;
    Ok(())
}

fn apply_osm_waterway(
    chunk: &mut ChunkModel,
    local_x: i32,
    local_z: i32,
    column: &EarthSurfaceColumn,
) -> Result<()> {
    let surface_y = column.ground_surface_y;
    if surface_y > chunk.dimension().min_y() {
        chunk.set_block_state_id(local_x, surface_y - 1, local_z, block_state_ids::SAND)?;
    }
    chunk.set_block_state_id(local_x, surface_y, local_z, block_state_ids::WATER)?;
    let bank_top_y = surface_y.saturating_add(1).min(column.water_surface_y);
    if bank_top_y > surface_y {
        chunk.fill_column(
            local_x,
            local_z,
            surface_y + 1,
            bank_top_y,
            block_state_ids::WATER,
        )?;
    }
    Ok(())
}

fn fill_surface_column(
    chunk: &mut ChunkModel,
    local_x: i32,
    local_z: i32,
    column: &EarthSurfaceColumn,
) -> Result<()> {
    let column = normalize_surface_column_for_chunk(column);
    let column = sanitize_surface_column_for_production(&column);
    chunk.set_block_state_id(local_x, -64, local_z, block_state_ids::BEDROCK)?;
    chunk.fill_column(
        local_x,
        local_z,
        -63,
        (-63).max(column.ground_surface_y - 4),
        block_state_ids::STONE,
    )?;
    chunk.fill_column(
        local_x,
        local_z,
        (-63).max(column.ground_surface_y - 3),
        column.ground_surface_y - 1,
        column.filler_block_state_id,
    )?;
    chunk.set_block_state_id(
        local_x,
        column.ground_surface_y,
        local_z,
        column.top_block_state_id,
    )?;
    if column.water {
        chunk.fill_column(
            local_x,
            local_z,
            column.ground_surface_y + 1,
            column.water_surface_y,
            block_state_ids::WATER,
        )?;
    }
    Ok(())
}

const SURFACE_CHUNK_COAST_RADIUS: usize = 64;
const SURFACE_CHUNK_SMOOTH_RADIUS: i32 = 5;
const SURFACE_CHUNK_EXTENT: usize = CHUNK_WIDTH + (SURFACE_CHUNK_COAST_RADIUS * 2);
const SURFACE_REGION_WIDTH: usize = REGION_SIZE_BLOCKS as usize;
const SURFACE_REGION_COAST_RADIUS: usize = 64;
const SURFACE_REGION_EXTENT: usize = SURFACE_REGION_WIDTH + (SURFACE_REGION_COAST_RADIUS * 2);

fn sample_surface_chunk_with_elevation_fn<F>(
    chunk_x: i32,
    chunk_z: i32,
    mapping: &EarthScaleMapping,
    vertical_scale: f64,
    mut elevation_fn: F,
) -> Result<SurfaceChunkSample>
where
    F: FnMut(f64, f64) -> Result<f64>,
{
    let vertical_scale = require_valid_vertical_scale(vertical_scale)?;
    let mut elevations = vec![0.0; SURFACE_CHUNK_EXTENT * SURFACE_CHUNK_EXTENT];
    let mut valid = vec![false; SURFACE_CHUNK_EXTENT * SURFACE_CHUNK_EXTENT];
    for z in 0..SURFACE_CHUNK_EXTENT {
        for x in 0..SURFACE_CHUNK_EXTENT {
            let global_block_x = chunk_x
                .wrapping_mul(CHUNK_WIDTH as i32)
                .wrapping_add(x as i32)
                .wrapping_sub(SURFACE_CHUNK_COAST_RADIUS as i32);
            let global_block_z = chunk_z
                .wrapping_mul(CHUNK_WIDTH as i32)
                .wrapping_add(z as i32)
                .wrapping_sub(SURFACE_CHUNK_COAST_RADIUS as i32);
            let map_x = global_block_x.wrapping_add(mapping.width_blocks / 2);
            let map_z = global_block_z.wrapping_add(mapping.height_blocks / 2);
            let index = surface_chunk_extent_index(x, z);
            if !surface_chunk_map_position_valid(mapping, map_x, map_z) {
                elevations[index] = 0.0;
                valid[index] = false;
                continue;
            }
            let longitude = mapping.longitude_for_block_x(map_x)?;
            let latitude = mapping.latitude_for_block_z(map_z)?;
            elevations[index] = elevation_fn(longitude, latitude)?;
            valid[index] = true;
        }
    }

    let water_mask = surface_chunk_water_decision_mask(&elevations, &valid);
    let mut columns = Vec::with_capacity(CHUNK_WIDTH * CHUNK_WIDTH);
    for local_z in 0..CHUNK_WIDTH {
        for local_x in 0..CHUNK_WIDTH {
            let center_x = local_x + SURFACE_CHUNK_COAST_RADIUS;
            let center_z = local_z + SURFACE_CHUNK_COAST_RADIUS;
            let global_block_x = chunk_x
                .wrapping_mul(CHUNK_WIDTH as i32)
                .wrapping_add(local_x as i32);
            let global_block_z = chunk_z
                .wrapping_mul(CHUNK_WIDTH as i32)
                .wrapping_add(local_z as i32);
            let map_x = global_block_x.wrapping_add(mapping.width_blocks / 2);
            let map_z = global_block_z.wrapping_add(mapping.height_blocks / 2);
            let longitude = if map_x < 0 || map_x >= mapping.width_blocks {
                0.0
            } else {
                mapping.longitude_for_block_x(map_x)?
            };
            let latitude = if map_z < 0 || map_z >= mapping.height_blocks {
                0.0
            } else {
                mapping.latitude_for_block_z(map_z)?
            };
            let sample_index = surface_chunk_extent_index(center_x, center_z);
            let smoothed_elevation =
                surface_chunk_smoothed_elevation(&elevations, &valid, center_x, center_z);
            let water = water_mask[sample_index];
            let coast_factor =
                surface_chunk_coast_factor(&valid, &water_mask, center_x, center_z, water);
            let column = classify_shaped_surface_scaled(
                smoothed_elevation,
                longitude,
                latitude,
                water,
                coast_factor,
                vertical_scale,
            )?;
            columns.push(clean_coastal_surface_column(&column, coast_factor));
        }
    }
    SurfaceChunkSample::new(columns)
}

fn surface_chunk_smoothed_elevation(
    elevations: &[f64],
    valid: &[bool],
    center_x: usize,
    center_z: usize,
) -> f64 {
    let mut weighted = 0.0;
    let mut weights = 0.0;
    for dz in -SURFACE_CHUNK_SMOOTH_RADIUS..=SURFACE_CHUNK_SMOOTH_RADIUS {
        for dx in -SURFACE_CHUNK_SMOOTH_RADIUS..=SURFACE_CHUNK_SMOOTH_RADIUS {
            let x = center_x as i32 + dx;
            let z = center_z as i32 + dz;
            if x < 0
                || x >= SURFACE_CHUNK_EXTENT as i32
                || z < 0
                || z >= SURFACE_CHUNK_EXTENT as i32
            {
                continue;
            }
            let index = surface_chunk_extent_index(x as usize, z as usize);
            if !valid[index] {
                continue;
            }
            let distance_squared = f64::from((dx * dx) + (dz * dz));
            let weight = 1.0 / (1.0 + distance_squared);
            weighted += elevations[index] * weight;
            weights += weight;
        }
    }
    if weights == 0.0 {
        0.0
    } else {
        weighted / weights
    }
}

fn surface_chunk_water_decision(raw_elevation: f64, smoothed_elevation: f64) -> bool {
    if raw_elevation <= -6.0 {
        return true;
    }
    if raw_elevation >= 8.0 {
        return false;
    }
    smoothed_elevation <= 0.0
}

fn surface_chunk_water_decision_mask(elevations: &[f64], valid: &[bool]) -> Vec<bool> {
    let mut water_mask = vec![false; SURFACE_CHUNK_EXTENT * SURFACE_CHUNK_EXTENT];
    for z in 0..SURFACE_CHUNK_EXTENT {
        for x in 0..SURFACE_CHUNK_EXTENT {
            let index = surface_chunk_extent_index(x, z);
            if !valid[index] {
                continue;
            }
            let raw_elevation = elevations[index];
            let smoothed_elevation =
                if surface_chunk_requires_smoothed_water_decision(raw_elevation) {
                    surface_chunk_smoothed_elevation(elevations, valid, x, z)
                } else {
                    0.0
                };
            water_mask[index] = surface_chunk_water_decision(raw_elevation, smoothed_elevation);
        }
    }
    water_mask
}

fn surface_chunk_requires_smoothed_water_decision(raw_elevation: f64) -> bool {
    raw_elevation > -6.0 && raw_elevation < 8.0
}

fn surface_chunk_coast_factor(
    valid: &[bool],
    water_mask: &[bool],
    center_x: usize,
    center_z: usize,
    water: bool,
) -> f64 {
    let mut nearest_squared = i32::MAX;
    let radius = SURFACE_CHUNK_COAST_RADIUS as i32;
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            let x = center_x as i32 + dx;
            let z = center_z as i32 + dz;
            let index = surface_chunk_extent_index(x as usize, z as usize);
            if !valid[index] {
                continue;
            }
            if water_mask[index] == water {
                continue;
            }
            let distance_squared = (dx * dx) + (dz * dz);
            nearest_squared = nearest_squared.min(distance_squared);
        }
    }
    if nearest_squared == i32::MAX || nearest_squared > radius * radius {
        return 0.0;
    }
    let nearest = f64::from(nearest_squared).sqrt();
    clamp_unit((f64::from(radius) + 1.0 - nearest) / f64::from(radius))
}

fn surface_chunk_extent_index(x: usize, z: usize) -> usize {
    (z * SURFACE_CHUNK_EXTENT) + x
}

fn surface_chunk_map_position_valid(mapping: &EarthScaleMapping, map_x: i32, map_z: i32) -> bool {
    map_x >= 0 && map_x < mapping.width_blocks && map_z >= 0 && map_z < mapping.height_blocks
}

fn surface_region_smoothed_elevation(
    elevations: &[f64],
    valid: &[bool],
    center_x: usize,
    center_z: usize,
) -> f64 {
    let mut weighted = 0.0;
    let mut weights = 0.0;
    for dz in -SURFACE_CHUNK_SMOOTH_RADIUS..=SURFACE_CHUNK_SMOOTH_RADIUS {
        for dx in -SURFACE_CHUNK_SMOOTH_RADIUS..=SURFACE_CHUNK_SMOOTH_RADIUS {
            let x = center_x as i32 + dx;
            let z = center_z as i32 + dz;
            if x < 0
                || x >= SURFACE_REGION_EXTENT as i32
                || z < 0
                || z >= SURFACE_REGION_EXTENT as i32
            {
                continue;
            }
            let index = surface_region_extent_index(x as usize, z as usize);
            if !valid[index] {
                continue;
            }
            let distance_squared = f64::from((dx * dx) + (dz * dz));
            let weight = 1.0 / (1.0 + distance_squared);
            weighted += elevations[index] * weight;
            weights += weight;
        }
    }
    if weights == 0.0 {
        0.0
    } else {
        weighted / weights
    }
}

fn surface_region_water_decision_mask(elevations: &[f64], valid: &[bool]) -> Vec<bool> {
    let mut water_mask = vec![false; SURFACE_REGION_EXTENT * SURFACE_REGION_EXTENT];
    for z in 0..SURFACE_REGION_EXTENT {
        for x in 0..SURFACE_REGION_EXTENT {
            let index = surface_region_extent_index(x, z);
            if !valid[index] {
                continue;
            }
            let raw_elevation = elevations[index];
            let smoothed_elevation =
                if surface_chunk_requires_smoothed_water_decision(raw_elevation) {
                    surface_region_smoothed_elevation(elevations, valid, x, z)
                } else {
                    0.0
                };
            water_mask[index] = surface_chunk_water_decision(raw_elevation, smoothed_elevation);
        }
    }
    water_mask
}

const SURFACE_REGION_DISTANCE_INFINITY: i32 = 1_000_000_000;

fn surface_region_coast_factors(valid: &[bool], water_mask: &[bool]) -> Vec<f64> {
    let distance_to_water = surface_region_squared_distance_to_mask(valid, water_mask, true);
    let distance_to_land = surface_region_squared_distance_to_mask(valid, water_mask, false);
    let mut factors = vec![0.0; SURFACE_REGION_EXTENT * SURFACE_REGION_EXTENT];
    for z in 0..SURFACE_REGION_EXTENT {
        for x in 0..SURFACE_REGION_EXTENT {
            let index = surface_region_extent_index(x, z);
            let nearest_squared = if water_mask[index] {
                distance_to_land[index]
            } else {
                distance_to_water[index]
            };
            factors[index] = surface_region_coast_factor_from_distance_squared(nearest_squared);
        }
    }
    factors
}

fn surface_region_coast_factor_from_distance_squared(nearest_squared: i32) -> f64 {
    let radius = SURFACE_REGION_COAST_RADIUS as i32;
    if nearest_squared == i32::MAX
        || nearest_squared >= SURFACE_REGION_DISTANCE_INFINITY
        || nearest_squared > radius * radius
    {
        return 0.0;
    }
    let nearest = f64::from(nearest_squared).sqrt();
    clamp_unit((f64::from(radius) + 1.0 - nearest) / f64::from(radius))
}

fn surface_region_squared_distance_to_mask(
    valid: &[bool],
    water_mask: &[bool],
    target_water: bool,
) -> Vec<i32> {
    let mut row_distances = vec![0; SURFACE_REGION_EXTENT * SURFACE_REGION_EXTENT];
    let mut distances = vec![0; SURFACE_REGION_EXTENT * SURFACE_REGION_EXTENT];
    let mut input = vec![0; SURFACE_REGION_EXTENT];
    let mut output = vec![0; SURFACE_REGION_EXTENT];
    let mut parabolas = vec![0; SURFACE_REGION_EXTENT];
    let mut boundaries = vec![0.0; SURFACE_REGION_EXTENT + 1];

    for z in 0..SURFACE_REGION_EXTENT {
        for x in 0..SURFACE_REGION_EXTENT {
            let index = surface_region_extent_index(x, z);
            input[x] = if valid[index] && water_mask[index] == target_water {
                0
            } else {
                SURFACE_REGION_DISTANCE_INFINITY
            };
        }
        surface_region_distance_transform_1d(&input, &mut output, &mut parabolas, &mut boundaries);
        for x in 0..SURFACE_REGION_EXTENT {
            row_distances[surface_region_extent_index(x, z)] = output[x];
        }
    }

    for x in 0..SURFACE_REGION_EXTENT {
        for z in 0..SURFACE_REGION_EXTENT {
            input[z] = row_distances[surface_region_extent_index(x, z)];
        }
        surface_region_distance_transform_1d(&input, &mut output, &mut parabolas, &mut boundaries);
        for z in 0..SURFACE_REGION_EXTENT {
            distances[surface_region_extent_index(x, z)] = output[z];
        }
    }
    distances
}

fn surface_region_distance_transform_1d(
    input: &[i32],
    output: &mut [i32],
    parabolas: &mut [usize],
    boundaries: &mut [f64],
) {
    let mut envelope_index: isize = -1;
    for q in 0..SURFACE_REGION_EXTENT {
        if input[q] >= SURFACE_REGION_DISTANCE_INFINITY {
            continue;
        }
        if envelope_index < 0 {
            envelope_index = 0;
            parabolas[0] = q;
            boundaries[0] = f64::NEG_INFINITY;
            boundaries[1] = f64::INFINITY;
            continue;
        }

        let mut intersection;
        loop {
            let previous = parabolas[envelope_index as usize];
            intersection = surface_region_parabola_intersection(input, q, previous);
            if intersection > boundaries[envelope_index as usize] {
                break;
            }
            envelope_index -= 1;
            if envelope_index < 0 {
                break;
            }
        }
        envelope_index += 1;
        let active = envelope_index as usize;
        parabolas[active] = q;
        boundaries[active] = if active == 0 {
            f64::NEG_INFINITY
        } else {
            intersection
        };
        boundaries[active + 1] = f64::INFINITY;
    }

    if envelope_index < 0 {
        output.fill(SURFACE_REGION_DISTANCE_INFINITY);
        return;
    }

    let mut active = 0usize;
    for (q, out) in output.iter_mut().enumerate().take(SURFACE_REGION_EXTENT) {
        while boundaries[active + 1] < q as f64 {
            active += 1;
        }
        let nearest = parabolas[active];
        let delta = q as i64 - nearest as i64;
        let distance = (delta * delta) + i64::from(input[nearest]);
        *out = if distance >= i64::from(SURFACE_REGION_DISTANCE_INFINITY) {
            SURFACE_REGION_DISTANCE_INFINITY
        } else {
            distance as i32
        };
    }
}

fn surface_region_parabola_intersection(input: &[i32], q: usize, previous: usize) -> f64 {
    ((f64::from(input[q]) + ((q * q) as f64))
        - (f64::from(input[previous]) + ((previous * previous) as f64)))
        / (2.0 * (q as f64 - previous as f64))
}

fn surface_region_extent_index(x: usize, z: usize) -> usize {
    (z * SURFACE_REGION_EXTENT) + x
}

const SURFACE_SMOOTHER_NEIGHBOR_RADIUS: i32 = 1;
const SURFACE_SMOOTHER_ISOLATED_COUNT_MAX: i32 = 2;
const SURFACE_SMOOTHER_MAJORITY_COUNT_MIN: i32 = 5;
const PHOTO_TEXTURE_NEIGHBOR_RADIUS: i32 = 2;
const PHOTO_EXACT_ISOLATED_MAX: i32 = 4;
const PHOTO_FAMILY_ISOLATED_MAX: i32 = 7;
const PHOTO_EXACT_MAJORITY_MIN: i32 = 10;
const PHOTO_FAMILY_MAJORITY_MIN: i32 = 15;
const PHOTO_MACRO_NEIGHBOR_RADIUS: i32 = 4;
const PHOTO_MACRO_LAND_MIN: i32 = 52;
const PHOTO_LUSH_VEGETATION_RATIO_MIN: f64 = 0.58;
const PHOTO_DRY_VEGETATION_RATIO_MIN: f64 = 0.82;

#[derive(Clone, Debug, Eq, PartialEq)]
struct SurfaceNeighborhoodStats {
    land_count: i32,
    top_counts: BTreeMap<i32, i32>,
    biome_counts: BTreeMap<String, i32>,
    majority_top: i32,
    majority_top_count: i32,
    majority_biome: String,
    majority_biome_count: i32,
}

impl SurfaceNeighborhoodStats {
    fn count_top(&self, top: i32) -> i32 {
        self.top_counts.get(&top).copied().unwrap_or(0)
    }

    fn count_biome(&self, biome: &str) -> i32 {
        self.biome_counts.get(biome).copied().unwrap_or(0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PhotoTextureStats {
    land_count: i32,
    top_counts: Vec<JavaHashI32Count>,
    family_counts: Vec<JavaHashI32Count>,
    majority_top: i32,
    majority_top_count: i32,
    majority_family: i32,
    majority_family_count: i32,
}

impl PhotoTextureStats {
    fn count_top(&self, top: i32) -> i32 {
        self.top_counts
            .iter()
            .find(|count| count.key == top)
            .map(|count| count.count)
            .unwrap_or(0)
    }

    fn count_family(&self, family: i32) -> i32 {
        self.family_counts
            .iter()
            .find(|count| count.key == family)
            .map(|count| count.count)
            .unwrap_or(0)
    }

    fn majority_top_for_family(&self, family: i32) -> i32 {
        java_hashmap_i32_majority_with_count_by(&self.top_counts, |top| {
            photo_texture_family(top) == family
        })
        .map(|(top, _)| top)
        .unwrap_or(0)
    }
}

fn smooth_photo_texture_local(
    columns: &[EarthSurfaceColumn],
    width: usize,
) -> Vec<EarthSurfaceColumn> {
    let height = columns.len() / width;
    let mut result = columns.to_vec();
    for z in 0..height {
        for x in 0..width {
            let index = surface_class_index(x, z, width);
            let column = &columns[index];
            if column.water
                || is_token_driven_photo_surface(column)
                || is_protected_photo_surface(column.top_block_state_id, &column.biome_id)
            {
                continue;
            }
            let stats =
                photo_texture_stats(columns, width, height, x, z, PHOTO_TEXTURE_NEIGHBOR_RADIUS);
            if stats.land_count < PHOTO_FAMILY_MAJORITY_MIN {
                continue;
            }
            let top = column.top_block_state_id;
            let family = photo_texture_family(top);
            let mut smoothed_top = top;
            if stats.count_top(top) <= PHOTO_EXACT_ISOLATED_MAX
                && stats.majority_top_count >= PHOTO_EXACT_MAJORITY_MIN
                && can_photo_replace(top, stats.majority_top, &column.biome_id)
            {
                smoothed_top = stats.majority_top;
            } else if can_absorb_dry_vegetation_sand_patch(top, &stats, &column.biome_id) {
                smoothed_top = stats.majority_top_for_family(1);
            } else if stats.count_family(family) <= PHOTO_FAMILY_ISOLATED_MAX
                && stats.majority_family_count >= PHOTO_FAMILY_MAJORITY_MIN
            {
                let replacement_top = stats.majority_top_for_family(stats.majority_family);
                if can_photo_replace(top, replacement_top, &column.biome_id) {
                    smoothed_top = replacement_top;
                }
            }
            if smoothed_top != top {
                result[index] = photo_smoother_replacement(column, smoothed_top, "smoother-photo");
            }
        }
    }
    result
}

fn smooth_photo_macro_vegetation(
    columns: &[EarthSurfaceColumn],
    width: usize,
) -> Vec<EarthSurfaceColumn> {
    let height = columns.len() / width;
    let mut result = columns.to_vec();
    for z in 0..height {
        for x in 0..width {
            let index = surface_class_index(x, z, width);
            let column = &columns[index];
            let biome = &column.biome_id;
            if column.water
                || is_token_driven_photo_surface(column)
                || is_protected_photo_surface(column.top_block_state_id, biome)
                || !is_photo_vegetation_biome(biome)
            {
                continue;
            }
            let top = column.top_block_state_id;
            let family = photo_texture_family(top);
            if family == 1 || family == 5 || family == 6 {
                continue;
            }
            let stats =
                photo_texture_stats(columns, width, height, x, z, PHOTO_MACRO_NEIGHBOR_RADIUS);
            if stats.land_count < PHOTO_MACRO_LAND_MIN {
                continue;
            }
            let replacement_top = stats.majority_top_for_family(1);
            if replacement_top == 0 || !can_photo_replace(top, replacement_top, biome) {
                continue;
            }
            let vegetation_ratio = f64::from(stats.count_family(1)) / f64::from(stats.land_count);
            let required_ratio = if is_lush_vegetation_biome(biome) {
                PHOTO_LUSH_VEGETATION_RATIO_MIN
            } else {
                PHOTO_DRY_VEGETATION_RATIO_MIN
            };
            if vegetation_ratio < required_ratio {
                continue;
            }
            let family_count = stats.count_family(family);
            let compact_accent = if is_lush_vegetation_biome(biome) {
                family_count <= 30
            } else {
                family_count <= 4
            };
            if !compact_accent {
                continue;
            }
            result[index] =
                photo_smoother_replacement(column, replacement_top, "smoother-photo-macro");
        }
    }
    result
}

fn surface_neighborhood_stats(
    columns: &[EarthSurfaceColumn],
    width: usize,
    height: usize,
    center_x: usize,
    center_z: usize,
) -> SurfaceNeighborhoodStats {
    let mut top_counts = BTreeMap::<i32, i32>::new();
    let mut biome_counts = BTreeMap::<String, i32>::new();
    let mut land_count = 0;
    for dz in -SURFACE_SMOOTHER_NEIGHBOR_RADIUS..=SURFACE_SMOOTHER_NEIGHBOR_RADIUS {
        for dx in -SURFACE_SMOOTHER_NEIGHBOR_RADIUS..=SURFACE_SMOOTHER_NEIGHBOR_RADIUS {
            let x = center_x as i32 + dx;
            let z = center_z as i32 + dz;
            if x < 0 || x >= width as i32 || z < 0 || z >= height as i32 {
                continue;
            }
            let column = &columns[surface_class_index(x as usize, z as usize, width)];
            if column.water {
                continue;
            }
            land_count += 1;
            *top_counts.entry(column.top_block_state_id).or_insert(0) += 1;
            *biome_counts.entry(column.biome_id.clone()).or_insert(0) += 1;
        }
    }
    let (majority_top, majority_top_count) = surface_majority_i32(&top_counts);
    let (majority_biome, majority_biome_count) = surface_majority_string(&biome_counts);
    SurfaceNeighborhoodStats {
        land_count,
        top_counts,
        biome_counts,
        majority_top,
        majority_top_count,
        majority_biome,
        majority_biome_count,
    }
}

fn photo_texture_stats(
    columns: &[EarthSurfaceColumn],
    width: usize,
    height: usize,
    center_x: usize,
    center_z: usize,
    radius: i32,
) -> PhotoTextureStats {
    let mut top_counts = Vec::<JavaHashI32Count>::new();
    let mut family_counts = Vec::<JavaHashI32Count>::new();
    let mut land_count = 0;
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            let x = center_x as i32 + dx;
            let z = center_z as i32 + dz;
            if x < 0 || x >= width as i32 || z < 0 || z >= height as i32 {
                continue;
            }
            let column = &columns[surface_class_index(x as usize, z as usize, width)];
            if column.water {
                continue;
            }
            let top = column.top_block_state_id;
            land_count += 1;
            increment_java_hash_i32_count(&mut top_counts, top);
            increment_java_hash_i32_count(&mut family_counts, photo_texture_family(top));
        }
    }
    let (majority_top, majority_top_count) =
        java_hashmap_i32_majority_with_count(&top_counts).unwrap_or((0, 0));
    let (majority_family, majority_family_count) =
        java_hashmap_i32_majority_with_count(&family_counts).unwrap_or((0, 0));
    PhotoTextureStats {
        land_count,
        top_counts,
        family_counts,
        majority_top,
        majority_top_count,
        majority_family,
        majority_family_count,
    }
}

fn is_protected_surface_for_smoothing(top: i32, biome: &str) -> bool {
    top == block_state_ids::SNOW_BLOCK || top == block_state_ids::MUD || biome == "minecraft:beach"
}

fn is_token_driven_photo_surface(column: &EarthSurfaceColumn) -> bool {
    is_photo_material_driven_column(column)
}

fn is_photo_material_driven_column(column: &EarthSurfaceColumn) -> bool {
    matches!(
        column.terrain_token_source,
        TerrainTokenSource::Export | TerrainTokenSource::JavaStandardPalette
    ) || column.decision_source.starts_with("photo-")
        || column.decision_source.starts_with("smoother-photo")
}

fn is_protected_photo_surface(top: i32, biome: &str) -> bool {
    if is_protected_surface_for_smoothing(top, biome) {
        return true;
    }
    is_desert_like_biome(biome) && matches!(photo_texture_family(top), 2 | 3)
}

fn can_photo_replace(current_top: i32, replacement_top: i32, biome: &str) -> bool {
    if current_top == replacement_top || replacement_top == 0 {
        return false;
    }
    if is_protected_photo_surface(current_top, biome)
        || is_protected_photo_surface(replacement_top, biome)
    {
        return false;
    }
    let current_family = photo_texture_family(current_top);
    let replacement_family = photo_texture_family(replacement_top);
    if current_family == replacement_family {
        return true;
    }
    if current_family == 1 && matches!(replacement_family, 2 | 3) {
        return false;
    }
    replacement_family != 6
}

fn can_absorb_dry_vegetation_sand_patch(
    current_top: i32,
    stats: &PhotoTextureStats,
    biome: &str,
) -> bool {
    if !is_dry_vegetation_biome(biome)
        || is_desert_like_biome(biome)
        || is_protected_photo_surface(current_top, biome)
    {
        return false;
    }
    let current_family = photo_texture_family(current_top);
    if !matches!(current_family, 2 | 3) {
        return false;
    }
    let vegetation_family_count = stats.count_family(1);
    let current_family_count = stats.count_family(current_family);
    let replacement_top = stats.majority_top_for_family(1);
    replacement_top != 0
        && vegetation_family_count >= 18
        && current_family_count <= 4
        && can_photo_replace(current_top, replacement_top, biome)
}

fn is_dry_vegetation_biome(biome: &str) -> bool {
    let lower = biome.to_ascii_lowercase();
    lower.contains("savanna")
        || lower.contains("plains")
        || lower.contains("meadow")
        || lower.contains("grassland")
        || lower.contains("steppe")
}

fn is_photo_vegetation_biome(biome: &str) -> bool {
    is_dry_vegetation_biome(biome) || is_lush_vegetation_biome(biome)
}

fn is_lush_vegetation_biome(biome: &str) -> bool {
    let lower = biome.to_ascii_lowercase();
    lower.contains("jungle") || lower.contains("forest") || lower.contains("taiga")
}

fn is_desert_like_biome(biome: &str) -> bool {
    let lower = biome.to_ascii_lowercase();
    lower.contains("desert") || lower.contains("badlands")
}

fn photo_texture_family(top: i32) -> i32 {
    match top {
        block_state_ids::GRASS_BLOCK
        | block_state_ids::MOSS_BLOCK
        | block_state_ids::PODZOL
        | block_state_ids::COARSE_DIRT
        | block_state_ids::DIRT
        | block_state_ids::ROOTED_DIRT
        | block_state_ids::PACKED_MUD
        | block_state_ids::MYCELIUM
        | block_state_ids::GREEN_TERRACOTTA
        | block_state_ids::LIME_TERRACOTTA
        | block_state_ids::OAK_LEAVES
        | block_state_ids::JUNGLE_LEAVES
        | block_state_ids::DARK_OAK_LEAVES
        | block_state_ids::SPRUCE_LEAVES => 1,
        block_state_ids::SAND
        | block_state_ids::SANDSTONE
        | block_state_ids::RED_SAND
        | block_state_ids::YELLOW_TERRACOTTA
        | block_state_ids::END_STONE
        | block_state_ids::END_STONE_BRICKS
        | block_state_ids::SMOOTH_SANDSTONE
        | block_state_ids::CUT_SANDSTONE
        | block_state_ids::CHISELED_SANDSTONE
        | block_state_ids::SMOOTH_RED_SANDSTONE
        | block_state_ids::CUT_RED_SANDSTONE
        | block_state_ids::CHISELED_RED_SANDSTONE => 2,
        block_state_ids::TERRACOTTA
        | block_state_ids::ORANGE_TERRACOTTA
        | block_state_ids::BROWN_TERRACOTTA
        | block_state_ids::RED_TERRACOTTA
        | block_state_ids::WHITE_TERRACOTTA
        | block_state_ids::LIGHT_GRAY_TERRACOTTA
        | block_state_ids::GRAY_TERRACOTTA
        | block_state_ids::BLACK_TERRACOTTA
        | block_state_ids::CYAN_TERRACOTTA
        | block_state_ids::GRANITE
        | block_state_ids::MUD_BRICKS
        | block_state_ids::DRIPSTONE_BLOCK => 3,
        block_state_ids::STONE
        | block_state_ids::TUFF
        | block_state_ids::GRAVEL
        | block_state_ids::DEEPSLATE
        | block_state_ids::CALCITE
        | block_state_ids::ANDESITE
        | block_state_ids::DIORITE
        | block_state_ids::CLAY => 4,
        block_state_ids::MUD => 5,
        block_state_ids::SNOW_BLOCK => 6,
        _ => 0,
    }
}

fn compatible_top_for_smoother_biome(current_top: i32, biome: &str) -> i32 {
    if !biome.contains("snow") && !biome.contains("frozen") {
        return current_top;
    }
    if matches!(
        current_top,
        block_state_ids::SNOW_BLOCK
            | block_state_ids::STONE
            | block_state_ids::GRAVEL
            | block_state_ids::ANDESITE
            | block_state_ids::GRANITE
            | block_state_ids::DIORITE
            | block_state_ids::TUFF
            | block_state_ids::CALCITE
    ) {
        return current_top;
    }
    block_state_ids::SNOW_BLOCK
}

fn smoother_replacement(column: &EarthSurfaceColumn, top: i32, biome: &str) -> EarthSurfaceColumn {
    let mut replacement = EarthSurfaceColumn::new(
        false,
        column.ground_surface_y,
        column.water_surface_y,
        top,
        smoother_filler_for(top),
        biome.to_string(),
        "smoother-isolated",
    );
    replacement.terrain_token_source = column.terrain_token_source;
    replacement.data_evidence_flags = column.data_evidence_flags;
    replacement
}

fn photo_smoother_replacement(
    column: &EarthSurfaceColumn,
    top: i32,
    source: &str,
) -> EarthSurfaceColumn {
    let mut replacement = EarthSurfaceColumn::new(
        false,
        column.ground_surface_y,
        column.water_surface_y,
        top,
        smoother_filler_for(top),
        column.biome_id.clone(),
        source.to_string(),
    );
    replacement.terrain_token_source = column.terrain_token_source;
    replacement.data_evidence_flags = column.data_evidence_flags;
    replacement
}

fn smoother_filler_for(top: i32) -> i32 {
    match top {
        block_state_ids::SAND
        | block_state_ids::SANDSTONE
        | block_state_ids::RED_SAND
        | block_state_ids::END_STONE
        | block_state_ids::END_STONE_BRICKS
        | block_state_ids::SMOOTH_SANDSTONE
        | block_state_ids::CUT_SANDSTONE
        | block_state_ids::CHISELED_SANDSTONE
        | block_state_ids::SMOOTH_RED_SANDSTONE
        | block_state_ids::CUT_RED_SANDSTONE
        | block_state_ids::CHISELED_RED_SANDSTONE
        | block_state_ids::TERRACOTTA
        | block_state_ids::ORANGE_TERRACOTTA
        | block_state_ids::BROWN_TERRACOTTA
        | block_state_ids::WHITE_TERRACOTTA
        | block_state_ids::LIGHT_GRAY_TERRACOTTA
        | block_state_ids::GRAY_TERRACOTTA
        | block_state_ids::BLACK_TERRACOTTA
        | block_state_ids::YELLOW_TERRACOTTA
        | block_state_ids::RED_TERRACOTTA
        | block_state_ids::GREEN_TERRACOTTA
        | block_state_ids::CYAN_TERRACOTTA
        | block_state_ids::LIME_TERRACOTTA
        | block_state_ids::ROOTED_DIRT
        | block_state_ids::MYCELIUM
        | block_state_ids::MUD_BRICKS
        | block_state_ids::DRIPSTONE_BLOCK => top,
        block_state_ids::STONE
        | block_state_ids::GRAVEL
        | block_state_ids::CLAY
        | block_state_ids::DEEPSLATE
        | block_state_ids::ANDESITE
        | block_state_ids::GRANITE
        | block_state_ids::DIORITE
        | block_state_ids::TUFF
        | block_state_ids::CALCITE => block_state_ids::STONE,
        block_state_ids::MUD | block_state_ids::PACKED_MUD => block_state_ids::MUD,
        _ => block_state_ids::DIRT,
    }
}

fn surface_majority_i32(counts: &BTreeMap<i32, i32>) -> (i32, i32) {
    let mut best = 0;
    let mut best_count = 0;
    for (&value, &count) in counts {
        if count > best_count {
            best = value;
            best_count = count;
        }
    }
    (best, best_count)
}

fn surface_majority_string(counts: &BTreeMap<String, i32>) -> (String, i32) {
    let mut best = String::new();
    let mut best_count = 0;
    for (value, &count) in counts {
        if count > best_count {
            best = value.clone();
            best_count = count;
        }
    }
    (best, best_count)
}

fn surface_class_index(x: usize, z: usize, width: usize) -> usize {
    (z * width) + x
}

const BIOME_INTENT_CELL_WIDTH: usize = 4;
const BIOME_INTENT_CELL_DOMINANCE_MIN: i32 = 10;
const SMALL_BIOME_COMPONENT_MAX: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
struct JavaHashStringCount {
    key: String,
    count: i32,
    first_order: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct JavaHashI32Count {
    key: i32,
    count: i32,
    first_order: usize,
}

fn require_surface_grid_width(columns: &[EarthSurfaceColumn], width: usize) -> Result<()> {
    if width == 0 || columns.len() % width != 0 {
        return Err(SurfaceError::invalid("width must divide columns length"));
    }
    Ok(())
}

fn stabilize_surface_biome_cells(
    columns: &[EarthSurfaceColumn],
    width: usize,
    preserve_surface: bool,
) -> Vec<EarthSurfaceColumn> {
    let height = columns.len() / width;
    let mut result = columns.to_vec();
    for cell_z in (0..height).step_by(BIOME_INTENT_CELL_WIDTH) {
        for cell_x in (0..width).step_by(BIOME_INTENT_CELL_WIDTH) {
            let mut counts = Vec::<JavaHashStringCount>::new();
            let mut family_counts = BTreeMap::<String, i32>::new();
            let mut land_count = 0;
            for dz in 0..BIOME_INTENT_CELL_WIDTH {
                if cell_z + dz >= height {
                    break;
                }
                for dx in 0..BIOME_INTENT_CELL_WIDTH {
                    if cell_x + dx >= width {
                        break;
                    }
                    let column = &columns[surface_class_index(cell_x + dx, cell_z + dz, width)];
                    if column.water {
                        continue;
                    }
                    land_count += 1;
                    increment_java_hash_string_count(&mut counts, &column.biome_id);
                    *family_counts
                        .entry(intent_biome_family(&column.biome_id))
                        .or_insert(0) += 1;
                }
            }
            let Some((majority_biome, majority_count)) =
                java_hashmap_string_majority_with_count(&counts)
            else {
                continue;
            };
            if land_count < BIOME_INTENT_CELL_DOMINANCE_MIN
                || majority_count < BIOME_INTENT_CELL_DOMINANCE_MIN
                || is_arid_transition_cell(&family_counts)
            {
                continue;
            }
            for dz in 0..BIOME_INTENT_CELL_WIDTH {
                if cell_z + dz >= height {
                    break;
                }
                for dx in 0..BIOME_INTENT_CELL_WIDTH {
                    if cell_x + dx >= width {
                        break;
                    }
                    let index = surface_class_index(cell_x + dx, cell_z + dz, width);
                    let column = &result[index];
                    if column.water
                        || is_protected_intent_biome(&column.biome_id)
                        || (preserve_surface && is_render_locked_photo_biome(column))
                    {
                        continue;
                    }
                    if column.biome_id != majority_biome {
                        result[index] = if preserve_surface {
                            intent_biome_only_replacement(
                                column,
                                &majority_biome,
                                "intent-stabilized-cell",
                            )
                        } else {
                            intent_surface_compatible_replacement(
                                column,
                                &majority_biome,
                                "intent-stabilized-cell",
                            )
                        };
                    }
                }
            }
        }
    }
    result
}

fn stabilize_small_surface_biome_family_components(
    columns: &[EarthSurfaceColumn],
    width: usize,
    preserve_surface: bool,
) -> Vec<EarthSurfaceColumn> {
    let height = columns.len() / width;
    let mut result = columns.to_vec();
    let mut visited = vec![false; columns.len()];
    let mut in_component = vec![false; columns.len()];
    let mut component = Vec::<usize>::with_capacity(columns.len());
    let mut queue = VecDeque::<usize>::new();
    for index in 0..columns.len() {
        if visited[index] {
            continue;
        }
        let seed = &columns[index];
        if seed.water
            || is_protected_intent_biome(&seed.biome_id)
            || (preserve_surface && is_render_locked_photo_biome(seed))
        {
            visited[index] = true;
            continue;
        }
        let family = intent_biome_family(&seed.biome_id);
        component.clear();
        queue.clear();
        visited[index] = true;
        in_component[index] = true;
        queue.push_back(index);
        while let Some(current) = queue.pop_front() {
            component.push(current);
            let x = current % width;
            let z = current / width;
            enqueue_same_intent_family(
                columns,
                &mut visited,
                &mut in_component,
                &mut queue,
                current.wrapping_sub(width),
                z > 0,
                &family,
                preserve_surface,
            );
            enqueue_same_intent_family(
                columns,
                &mut visited,
                &mut in_component,
                &mut queue,
                current + width,
                z < height - 1,
                &family,
                preserve_surface,
            );
            enqueue_same_intent_family(
                columns,
                &mut visited,
                &mut in_component,
                &mut queue,
                current.wrapping_sub(1),
                x > 0,
                &family,
                preserve_surface,
            );
            enqueue_same_intent_family(
                columns,
                &mut visited,
                &mut in_component,
                &mut queue,
                current + 1,
                x < width - 1,
                &family,
                preserve_surface,
            );
        }
        if component.len() <= SMALL_BIOME_COMPONENT_MAX {
            if let Some(replacement_biome) =
                neighboring_intent_majority_biome(columns, width, height, &component, &in_component)
            {
                if replacement_biome != seed.biome_id
                    && !(component.len() > 16
                        && is_arid_transition_pair(
                            &family,
                            &intent_biome_family(&replacement_biome),
                        ))
                {
                    for &component_index in &component {
                        if preserve_surface
                            && is_render_locked_photo_biome(&result[component_index])
                        {
                            continue;
                        }
                        result[component_index] = intent_component_replacement(
                            &result[component_index],
                            &replacement_biome,
                            preserve_surface,
                        );
                    }
                }
            }
        }
        for &component_index in &component {
            in_component[component_index] = false;
        }
    }
    result
}

fn surface_biome_component_trace(
    columns: &[EarthSurfaceColumn],
    width: usize,
    seed_index: usize,
    preserve_surface: bool,
) -> SurfaceBiomeComponentTrace {
    let height = columns.len() / width;
    let seed = &columns[seed_index];
    if seed.water {
        return SurfaceBiomeComponentTrace {
            family: String::new(),
            size: 0,
            min_local_x: 0,
            min_local_z: 0,
            max_local_x: 0,
            max_local_z: 0,
            neighbor_majority_biome: None,
            neighbor_counts: Vec::new(),
            action: "skip-water".to_string(),
        };
    }
    if is_protected_intent_biome(&seed.biome_id) {
        return SurfaceBiomeComponentTrace {
            family: intent_biome_family(&seed.biome_id),
            size: 0,
            min_local_x: 0,
            min_local_z: 0,
            max_local_x: 0,
            max_local_z: 0,
            neighbor_majority_biome: None,
            neighbor_counts: Vec::new(),
            action: "skip-protected-biome".to_string(),
        };
    }
    if preserve_surface && is_render_locked_photo_biome(seed) {
        return SurfaceBiomeComponentTrace {
            family: intent_biome_family(&seed.biome_id),
            size: 0,
            min_local_x: 0,
            min_local_z: 0,
            max_local_x: 0,
            max_local_z: 0,
            neighbor_majority_biome: None,
            neighbor_counts: Vec::new(),
            action: "skip-render-locked".to_string(),
        };
    }
    let family = intent_biome_family(&seed.biome_id);
    let mut visited = vec![false; columns.len()];
    let mut in_component = vec![false; columns.len()];
    let mut component = Vec::<usize>::new();
    let mut queue = VecDeque::<usize>::new();
    visited[seed_index] = true;
    in_component[seed_index] = true;
    queue.push_back(seed_index);
    while let Some(current) = queue.pop_front() {
        component.push(current);
        let x = current % width;
        let z = current / width;
        enqueue_same_intent_family(
            columns,
            &mut visited,
            &mut in_component,
            &mut queue,
            current.wrapping_sub(width),
            z > 0,
            &family,
            preserve_surface,
        );
        enqueue_same_intent_family(
            columns,
            &mut visited,
            &mut in_component,
            &mut queue,
            current + width,
            z < height - 1,
            &family,
            preserve_surface,
        );
        enqueue_same_intent_family(
            columns,
            &mut visited,
            &mut in_component,
            &mut queue,
            current.wrapping_sub(1),
            x > 0,
            &family,
            preserve_surface,
        );
        enqueue_same_intent_family(
            columns,
            &mut visited,
            &mut in_component,
            &mut queue,
            current + 1,
            x < width - 1,
            &family,
            preserve_surface,
        );
    }
    let neighbor_counts =
        neighboring_intent_biome_counts(columns, width, height, &component, &in_component);
    let neighbor_majority_biome = java_hashmap_string_majority(&neighbor_counts);
    let mut min_local_x = width;
    let mut min_local_z = height;
    let mut max_local_x = 0usize;
    let mut max_local_z = 0usize;
    for &component_index in &component {
        let x = component_index % width;
        let z = component_index / width;
        min_local_x = min_local_x.min(x);
        min_local_z = min_local_z.min(z);
        max_local_x = max_local_x.max(x);
        max_local_z = max_local_z.max(z);
    }
    let production_seed = component
        .iter()
        .copied()
        .min()
        .map(|index| &columns[index])
        .unwrap_or(seed);
    let production_family = intent_biome_family(&production_seed.biome_id);
    let action = if component.len() > SMALL_BIOME_COMPONENT_MAX {
        "skip-large-component"
    } else if neighbor_majority_biome.is_none() {
        "skip-no-neighbor-majority"
    } else {
        let replacement_biome = neighbor_majority_biome.as_deref().unwrap_or_default();
        if replacement_biome == production_seed.biome_id {
            "skip-already-majority"
        } else if component.len() > 16
            && is_arid_transition_pair(&production_family, &intent_biome_family(replacement_biome))
        {
            "skip-arid-transition-pair"
        } else {
            "replace"
        }
    };
    SurfaceBiomeComponentTrace {
        family,
        size: component.len(),
        min_local_x,
        min_local_z,
        max_local_x,
        max_local_z,
        neighbor_majority_biome,
        neighbor_counts: neighbor_counts
            .into_iter()
            .map(|count| (count.key, count.count))
            .collect(),
        action: action.to_string(),
    }
}

fn enqueue_same_intent_family(
    columns: &[EarthSurfaceColumn],
    visited: &mut [bool],
    in_component: &mut [bool],
    queue: &mut VecDeque<usize>,
    index: usize,
    in_bounds: bool,
    family: &str,
    preserve_surface: bool,
) {
    if !in_bounds || visited[index] {
        return;
    }
    let column = &columns[index];
    if column.water
        || is_protected_intent_biome(&column.biome_id)
        || (preserve_surface && is_render_locked_photo_biome(column))
    {
        return;
    }
    if intent_biome_family(&column.biome_id) != family {
        return;
    }
    visited[index] = true;
    in_component[index] = true;
    queue.push_back(index);
}

fn neighboring_intent_majority_biome(
    columns: &[EarthSurfaceColumn],
    width: usize,
    height: usize,
    component: &[usize],
    in_component: &[bool],
) -> Option<String> {
    let counts = neighboring_intent_biome_counts(columns, width, height, component, in_component);
    java_hashmap_string_majority(&counts)
}

fn neighboring_intent_biome_counts(
    columns: &[EarthSurfaceColumn],
    width: usize,
    height: usize,
    component: &[usize],
    in_component: &[bool],
) -> Vec<JavaHashStringCount> {
    let mut counts = Vec::<JavaHashStringCount>::new();
    for &index in component {
        let x = index % width;
        let z = index / width;
        count_neighbor_intent_biome(
            columns,
            in_component,
            &mut counts,
            index.wrapping_sub(width),
            z > 0,
        );
        count_neighbor_intent_biome(
            columns,
            in_component,
            &mut counts,
            index + width,
            z < height - 1,
        );
        count_neighbor_intent_biome(
            columns,
            in_component,
            &mut counts,
            index.wrapping_sub(1),
            x > 0,
        );
        count_neighbor_intent_biome(columns, in_component, &mut counts, index + 1, x < width - 1);
    }
    counts
}

fn count_neighbor_intent_biome(
    columns: &[EarthSurfaceColumn],
    in_component: &[bool],
    counts: &mut Vec<JavaHashStringCount>,
    index: usize,
    in_bounds: bool,
) {
    if !in_bounds || in_component[index] {
        return;
    }
    let column = &columns[index];
    if column.water || is_protected_intent_biome(&column.biome_id) {
        return;
    }
    increment_java_hash_string_count(counts, &column.biome_id);
}

fn increment_java_hash_string_count(counts: &mut Vec<JavaHashStringCount>, key: &str) {
    if let Some(count) = counts.iter_mut().find(|count| count.key == key) {
        count.count += 1;
        return;
    }
    counts.push(JavaHashStringCount {
        key: key.to_string(),
        count: 1,
        first_order: counts.len(),
    });
}

fn increment_java_hash_i32_count(counts: &mut Vec<JavaHashI32Count>, key: i32) {
    if let Some(count) = counts.iter_mut().find(|count| count.key == key) {
        count.count += 1;
        return;
    }
    counts.push(JavaHashI32Count {
        key,
        count: 1,
        first_order: counts.len(),
    });
}

fn java_hashmap_i32_majority_with_count(counts: &[JavaHashI32Count]) -> Option<(i32, i32)> {
    java_hashmap_i32_majority_with_count_by(counts, |_| true)
}

fn java_hashmap_i32_majority_with_count_by(
    counts: &[JavaHashI32Count],
    include: impl Fn(i32) -> bool,
) -> Option<(i32, i32)> {
    let capacity = java_hashmap_capacity_for_size(counts.len());
    counts
        .iter()
        .filter(|count| include(count.key))
        .max_by(|left, right| {
            left.count
                .cmp(&right.count)
                .then_with(|| {
                    java_hashmap_i32_bucket(right.key, capacity)
                        .cmp(&java_hashmap_i32_bucket(left.key, capacity))
                })
                .then_with(|| right.first_order.cmp(&left.first_order))
        })
        .filter(|count| count.count > 0)
        .map(|count| (count.key, count.count))
}

fn java_hashmap_string_majority(counts: &[JavaHashStringCount]) -> Option<String> {
    java_hashmap_string_majority_with_count(counts).map(|(key, _)| key)
}

fn java_hashmap_string_majority_with_count(
    counts: &[JavaHashStringCount],
) -> Option<(String, i32)> {
    let capacity = java_hashmap_capacity_for_size(counts.len());
    counts
        .iter()
        .max_by(|left, right| {
            left.count
                .cmp(&right.count)
                .then_with(|| {
                    java_hashmap_string_bucket(&right.key, capacity)
                        .cmp(&java_hashmap_string_bucket(&left.key, capacity))
                })
                .then_with(|| right.first_order.cmp(&left.first_order))
        })
        .filter(|count| count.count > 0)
        .map(|count| (count.key.clone(), count.count))
}

fn java_hashmap_capacity_for_size(size: usize) -> usize {
    let mut capacity = 16;
    let mut threshold = (capacity * 3) / 4;
    while size > threshold {
        capacity *= 2;
        threshold = (capacity * 3) / 4;
    }
    capacity
}

fn java_hashmap_string_bucket(key: &str, capacity: usize) -> usize {
    let hash = java_string_hash_code(key) as u32;
    let spread = hash ^ (hash >> 16);
    (spread as usize) & (capacity - 1)
}

fn java_hashmap_i32_bucket(key: i32, capacity: usize) -> usize {
    let hash = key as u32;
    let spread = hash ^ (hash >> 16);
    (spread as usize) & (capacity - 1)
}

fn java_string_hash_code(text: &str) -> i32 {
    let mut hash = 0_i32;
    for unit in text.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    hash
}

fn intent_component_replacement(
    column: &EarthSurfaceColumn,
    biome: &str,
    preserve_surface: bool,
) -> EarthSurfaceColumn {
    if preserve_surface {
        return intent_biome_only_replacement(column, biome, "intent-stabilized-component");
    }
    intent_surface_compatible_replacement(column, biome, "intent-stabilized-component")
}

fn intent_surface_compatible_replacement(
    column: &EarthSurfaceColumn,
    biome: &str,
    source: &str,
) -> EarthSurfaceColumn {
    let top = intent_compatible_top_for_biome(column.top_block_state_id, biome);
    let mut replacement = EarthSurfaceColumn::new(
        false,
        column.ground_surface_y,
        column.water_surface_y,
        top,
        intent_filler_for(top),
        biome.to_string(),
        source.to_string(),
    );
    replacement.terrain_token_source = column.terrain_token_source;
    replacement.data_evidence_flags = column.data_evidence_flags;
    replacement
}

fn intent_biome_only_replacement(
    column: &EarthSurfaceColumn,
    biome: &str,
    source: &str,
) -> EarthSurfaceColumn {
    let mut replacement = EarthSurfaceColumn::new(
        false,
        column.ground_surface_y,
        column.water_surface_y,
        column.top_block_state_id,
        column.filler_block_state_id,
        biome.to_string(),
        source.to_string(),
    );
    replacement.terrain_token_source = column.terrain_token_source;
    replacement.data_evidence_flags = column.data_evidence_flags;
    replacement
}

fn intent_compatible_top_for_biome(current_top: i32, biome: &str) -> i32 {
    if is_snow_intent_biome(biome) {
        if matches!(
            current_top,
            block_state_ids::SNOW_BLOCK | block_state_ids::STONE | block_state_ids::GRAVEL
        ) {
            return current_top;
        }
        return block_state_ids::SNOW_BLOCK;
    }
    if is_vegetated_intent_biome(biome)
        && current_top != block_state_ids::MUD
        && current_top != block_state_ids::SNOW_BLOCK
    {
        if current_top == block_state_ids::COARSE_DIRT && is_dry_vegetated_intent_biome(biome) {
            return current_top;
        }
        if (current_top == block_state_ids::MOSS_BLOCK || current_top == block_state_ids::PODZOL)
            && is_lush_vegetated_intent_biome(biome)
        {
            return current_top;
        }
        return block_state_ids::GRASS_BLOCK;
    }
    if biome.contains("desert") {
        return block_state_ids::SAND;
    }
    if biome.contains("badlands") {
        if matches!(
            current_top,
            block_state_ids::RED_SAND
                | block_state_ids::ORANGE_TERRACOTTA
                | block_state_ids::BROWN_TERRACOTTA
                | block_state_ids::TERRACOTTA
        ) {
            return current_top;
        }
        return block_state_ids::TERRACOTTA;
    }
    current_top
}

fn is_protected_intent_biome(biome: &str) -> bool {
    biome == "minecraft:beach"
        || biome.contains("snow")
        || biome.contains("swamp")
        || biome.contains("mangrove")
}

fn is_render_locked_photo_biome(column: &EarthSurfaceColumn) -> bool {
    is_photo_material_driven_column(column)
        && (is_tinted_intent_render_surface(column.top_block_state_id)
            || is_photo_preserved_natural_surface_top(column.top_block_state_id))
}

fn is_tinted_intent_render_surface(top: i32) -> bool {
    matches!(
        top,
        block_state_ids::GRASS_BLOCK
            | block_state_ids::OAK_LEAVES
            | block_state_ids::JUNGLE_LEAVES
            | block_state_ids::DARK_OAK_LEAVES
            | block_state_ids::SPRUCE_LEAVES
    )
}

fn is_snow_intent_biome(biome: &str) -> bool {
    biome.contains("snow") || biome.contains("frozen")
}

fn is_vegetated_intent_biome(biome: &str) -> bool {
    biome.contains("savanna")
        || biome.contains("jungle")
        || biome.contains("forest")
        || biome.contains("plains")
        || biome.contains("meadow")
        || biome.contains("taiga")
}

fn is_dry_vegetated_intent_biome(biome: &str) -> bool {
    biome.contains("savanna") || biome.contains("plains")
}

fn is_lush_vegetated_intent_biome(biome: &str) -> bool {
    biome.contains("jungle") || biome.contains("forest") || biome.contains("taiga")
}

fn is_arid_transition_cell(family_counts: &BTreeMap<String, i32>) -> bool {
    if family_counts.len() <= 1 {
        return false;
    }
    let mut has_arid = false;
    for family in family_counts.keys() {
        if is_arid_transition_family(family) {
            has_arid = true;
            continue;
        }
        return false;
    }
    has_arid
}

fn is_arid_transition_pair(first_family: &str, second_family: &str) -> bool {
    first_family != second_family
        && is_arid_transition_family(first_family)
        && is_arid_transition_family(second_family)
}

fn is_arid_transition_family(family: &str) -> bool {
    matches!(family, "desert" | "badlands" | "savanna" | "grassland")
}

fn intent_biome_family(biome: &str) -> String {
    if biome.contains("desert") {
        return "desert".to_string();
    }
    if biome.contains("badlands") {
        return "badlands".to_string();
    }
    if biome.contains("savanna") {
        return "savanna".to_string();
    }
    if biome.contains("jungle") {
        return "jungle".to_string();
    }
    if biome.contains("forest") {
        return "forest".to_string();
    }
    if biome.contains("swamp") {
        return "swamp".to_string();
    }
    if biome.contains("taiga") {
        return "taiga".to_string();
    }
    if biome.contains("plains") || biome.contains("meadow") {
        return "grassland".to_string();
    }
    biome.to_string()
}

fn intent_filler_for(top: i32) -> i32 {
    match top {
        block_state_ids::SAND
        | block_state_ids::RED_SAND
        | block_state_ids::TERRACOTTA
        | block_state_ids::SANDSTONE
        | block_state_ids::END_STONE
        | block_state_ids::END_STONE_BRICKS
        | block_state_ids::SMOOTH_SANDSTONE
        | block_state_ids::CUT_SANDSTONE
        | block_state_ids::CHISELED_SANDSTONE
        | block_state_ids::SMOOTH_RED_SANDSTONE
        | block_state_ids::CUT_RED_SANDSTONE
        | block_state_ids::CHISELED_RED_SANDSTONE
        | block_state_ids::ORANGE_TERRACOTTA
        | block_state_ids::BROWN_TERRACOTTA
        | block_state_ids::WHITE_TERRACOTTA
        | block_state_ids::LIGHT_GRAY_TERRACOTTA
        | block_state_ids::YELLOW_TERRACOTTA
        | block_state_ids::RED_TERRACOTTA
        | block_state_ids::MUD_BRICKS
        | block_state_ids::DRIPSTONE_BLOCK => top,
        block_state_ids::STONE | block_state_ids::GRAVEL | block_state_ids::CLAY => {
            block_state_ids::STONE
        }
        _ => block_state_ids::DIRT,
    }
}

fn top_block_state_id(
    _elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    water: bool,
    surface_y: i32,
    biome_id: &str,
    coast_factor: f64,
) -> i32 {
    if water {
        return block_state_ids::GRAVEL;
    }
    if biome_id == "minecraft:desert"
        || (biome_id == "minecraft:beach" && default_beach_sand_likely(longitude, latitude))
        || is_beach_band(coast_factor, surface_y, longitude, latitude)
    {
        return block_state_ids::SAND;
    }
    if biome_id == "minecraft:snowy_plains" {
        return block_state_ids::SNOW_BLOCK;
    }
    if surface_y >= 140 {
        return block_state_ids::STONE;
    }
    block_state_ids::GRASS_BLOCK
}

fn shaped_ground_surface_y(
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    water: bool,
    coast_factor: f64,
    vertical_scale: f64,
) -> i32 {
    if water {
        let mut depth_blocks =
            ocean_depth_blocks(elevation_meters, longitude, latitude, vertical_scale);
        depth_blocks =
            coastal_shelf_adjusted_depth_blocks(depth_blocks, coast_factor, vertical_scale);
        return clamp_surface_y(SEA_LEVEL_Y - clamp_i32(depth_blocks, 1, 123));
    }
    let positive_elevation = elevation_meters.max(0.0);
    let mut extra_blocks = java_math_round_double_to_narrowed_i32(
        (positive_elevation * vertical_scale) / SHAPED_ELEVATION_METERS_PER_BLOCK,
    );
    if coast_factor > 0.55 {
        let near_shore = clamp_unit((coast_factor - 0.55) / 0.45);
        let inland_distance = 1.0 - near_shore;
        let coast_limit = java_math_round_double_to_narrowed_i32(
            2.0 + (inland_distance.powf(1.15) * 28.0 * vertical_scale.sqrt()),
        );
        if extra_blocks > coast_limit {
            let coast_pull = smooth_step(near_shore);
            extra_blocks = java_math_round_double_to_narrowed_i32(lerp(
                f64::from(extra_blocks),
                f64::from(coast_limit),
                coast_pull,
            ));
        }
    }
    clamp_surface_y(SEA_LEVEL_Y.wrapping_add(extra_blocks))
}

fn desert_score(longitude: f64, latitude: f64) -> f64 {
    let mut score: f64 = 0.0;
    score = score.max(smooth_box(
        longitude, latitude, -17.0, 60.0, 12.0, 35.0, 5.5,
    ));
    score = score.max(smooth_box(
        longitude, latitude, 112.0, 155.0, -34.0, -16.0, 4.5,
    ));
    score = score.max(smooth_box(
        longitude, latitude, 66.0, 105.0, 35.0, 47.0, 4.0,
    ));
    score = score.max(smooth_box(
        longitude, latitude, -75.0, -66.0, -28.0, -16.0, 2.5,
    ));
    let texture = value_noise(
        (longitude * 0.23) + 15.0,
        (latitude * 0.23) - 41.0,
        0x52dce729,
    );
    clamp_unit(score + ((texture - 0.5) * 0.22))
}

fn is_beach_band(coast_factor: f64, surface_y: i32, longitude: f64, latitude: f64) -> bool {
    coast_factor >= BEACH_COAST_FACTOR
        && surface_y <= SEA_LEVEL_Y + 2
        && default_beach_sand_likely(longitude, latitude)
}

fn default_beach_sand_likely(longitude: f64, latitude: f64) -> bool {
    let abs_lat = latitude.abs();
    let desert = desert_score(longitude, latitude);
    if desert >= 0.50 {
        return true;
    }
    if abs_lat >= 25.0 {
        return false;
    }
    if abs_lat >= 58.0 {
        return false;
    }
    let shore_texture = value_noise(
        (longitude * 0.83) + 17.0,
        (latitude * 0.83) - 29.0,
        0x4f1bbcdc6e3c2a19_u64 as i64,
    );
    if desert >= 0.30 {
        return shore_texture >= 0.46;
    }
    if abs_lat <= 28.0 {
        return shore_texture >= 0.82;
    }
    shore_texture >= 0.90
}

fn filler_block_state_id(top: i32, water: bool) -> i32 {
    if water {
        return top;
    }
    if top == block_state_ids::SAND {
        block_state_ids::SAND
    } else {
        block_state_ids::DIRT
    }
}

fn jungle_score(longitude: f64, latitude: f64) -> f64 {
    let mut score: f64 = 0.0;
    score = score.max(smooth_box(
        longitude, latitude, -77.0, -45.0, -16.0, 7.0, 4.5,
    ));
    score = score.max(smooth_box(longitude, latitude, 10.0, 32.0, -9.0, 7.0, 4.0));
    score = score.max(smooth_box(
        longitude, latitude, 95.0, 145.0, -11.0, 20.0, 4.5,
    ));
    let texture = value_noise(
        (longitude * 0.19) - 31.0,
        (latitude * 0.19) + 8.0,
        0x7f4a7c15,
    );
    clamp_unit(score + ((texture - 0.5) * 0.18))
}

fn forest_score(longitude: f64, latitude: f64) -> f64 {
    let abs_lat = latitude.abs();
    let mid_latitude = 1.0 - clamp_unit((abs_lat - 40.0).abs() / 25.0);
    let moisture = value_noise(
        (longitude * 0.12) + 73.0,
        (latitude * 0.12) - 11.0,
        0x94d049bb,
    );
    clamp_unit((mid_latitude * 0.65) + (moisture * 0.45))
}

fn smooth_box(
    longitude: f64,
    latitude: f64,
    min_longitude: f64,
    max_longitude: f64,
    min_latitude: f64,
    max_latitude: f64,
    edge_degrees: f64,
) -> f64 {
    let west = smooth_step(clamp_unit((longitude - min_longitude) / edge_degrees));
    let east = smooth_step(clamp_unit((max_longitude - longitude) / edge_degrees));
    let south = smooth_step(clamp_unit((latitude - min_latitude) / edge_degrees));
    let north = smooth_step(clamp_unit((max_latitude - latitude) / edge_degrees));
    west * east * south * north
}

fn ocean_depth_blocks(
    elevation_meters: f64,
    longitude: f64,
    latitude: f64,
    vertical_scale: f64,
) -> i32 {
    let abs_lat = latitude.abs();
    let basin = fractal_value_noise(longitude * 0.08, latitude * 0.08, 4, 0x6d2b79f5);
    let detail = fractal_value_noise(
        (longitude * 0.55) + 19.0,
        (latitude * 0.55) - 37.0,
        3,
        0x9e3779b97f4a7c15_u64 as i64,
    );
    let trench = fractal_value_noise(
        (longitude * 0.18) - 71.0,
        (latitude * 0.18) + 43.0,
        2,
        0xbf58476d1ce4e5b9_u64 as i64,
    )
    .powf(3.0);
    let polar_shelf = if abs_lat > 68.0 {
        (abs_lat - 68.0) * 0.35
    } else {
        0.0
    };
    let mut synthetic_depth = java_math_round_double_to_narrowed_i32(
        (9.0 + (basin * 29.0) + (detail * 8.0) + (trench * 12.0) - polar_shelf)
            * vertical_scale.sqrt(),
    );
    synthetic_depth = clamp_i32(synthetic_depth, 6, 56);
    if elevation_meters < 0.0 {
        let source_depth = 1.max(java_math_round_double_to_narrowed_i32(
            (-elevation_meters * vertical_scale) / ELEVATION_METERS_PER_BLOCK,
        ));
        let roughness = java_math_round_double_to_narrowed_i32((detail - 0.5) * 5.0);
        return clamp_i32(source_depth.max(synthetic_depth / 2) + roughness, 2, 123);
    }
    synthetic_depth
}

fn coastal_shelf_adjusted_depth_blocks(
    depth_blocks: i32,
    coast_factor: f64,
    vertical_scale: f64,
) -> i32 {
    let near_shore = clamp_unit((coast_factor - 0.88) / 0.12);
    if near_shore <= 0.0 {
        return depth_blocks;
    }
    let shelf_scale = vertical_scale.sqrt();
    let minimum_depth = if depth_blocks >= 12 { 3 } else { 2 };
    let shelf_depth = java_math_round_double_to_narrowed_i32(
        (f64::from(minimum_depth) + ((1.0 - near_shore).powf(1.20) * 6.0)) * shelf_scale,
    );
    let shelf_pull = near_shore.powf(1.75);
    clamp_i32(
        java_math_round_double_to_narrowed_i32(lerp(
            f64::from(depth_blocks),
            f64::from(shelf_depth),
            shelf_pull,
        )),
        1,
        123,
    )
}

fn coastal_bathymetry_shelf_adjusted_depth_blocks(
    depth_blocks: i32,
    coast_factor: f64,
    vertical_scale: f64,
) -> i32 {
    let near_shore = clamp_unit((coast_factor - 0.35) / 0.65);
    if near_shore <= 0.0 {
        return depth_blocks;
    }
    let shelf_scale = vertical_scale.sqrt();
    let immediate_depth = clamp_i32(
        java_math_round_double_to_narrowed_i32(2.0 * shelf_scale),
        2,
        8,
    );
    let shelf_extra =
        java_math_round_double_to_narrowed_i32((1.0 - near_shore).powf(1.55) * 42.0 * shelf_scale);
    let shelf_depth = clamp_i32(immediate_depth + shelf_extra, immediate_depth, 123);
    depth_blocks.min(shelf_depth)
}

fn fractal_value_noise(x: f64, z: f64, octaves: i32, seed: i64) -> f64 {
    let mut value = 0.0;
    let mut amplitude = 1.0;
    let mut amplitude_sum = 0.0;
    let mut frequency = 1.0;
    for octave in 0..octaves {
        value += value_noise(
            x * frequency,
            z * frequency,
            seed.wrapping_add((octave as i64).wrapping_mul(0x632be59bd9b4e019)),
        ) * amplitude;
        amplitude_sum += amplitude;
        amplitude *= 0.5;
        frequency *= 2.0;
    }
    value / amplitude_sum
}

fn value_noise(x: f64, z: f64, seed: i64) -> f64 {
    let x0 = fast_floor(x);
    let z0 = fast_floor(z);
    let tx = smooth_step(x - f64::from(x0));
    let tz = smooth_step(z - f64::from(z0));
    let a = lattice_value(x0, z0, seed);
    let next_x = x0.wrapping_add(1);
    let next_z = z0.wrapping_add(1);
    let b = lattice_value(next_x, z0, seed);
    let c = lattice_value(x0, next_z, seed);
    let d = lattice_value(next_x, next_z, seed);
    let ab = lerp(a, b, tx);
    let cd = lerp(c, d, tx);
    lerp(ab, cd, tz)
}

fn lattice_value(x: i32, z: i32, seed: i64) -> f64 {
    let mut hash = seed;
    hash ^= (x as i64).wrapping_mul(0x9e3779b97f4a7c15_u64 as i64);
    hash ^= (z as i64).wrapping_mul(0xbf58476d1ce4e5b9_u64 as i64);
    hash ^= ((hash as u64) >> 30) as i64;
    hash = hash.wrapping_mul(0xbf58476d1ce4e5b9_u64 as i64);
    hash ^= ((hash as u64) >> 27) as i64;
    hash = hash.wrapping_mul(0x94d049bb133111eb_u64 as i64);
    hash ^= ((hash as u64) >> 31) as i64;
    (((hash as u64) >> 11) as f64) * (1.0 / ((1u64 << 53) as f64))
}

fn fast_floor(value: f64) -> i32 {
    let integer = value as i32;
    if value < f64::from(integer) {
        integer - 1
    } else {
        integer
    }
}

fn smooth_step(value: f64) -> f64 {
    value * value * (3.0 - (2.0 * value))
}

fn lerp(a: f64, b: f64, amount: f64) -> f64 {
    a + ((b - a) * amount)
}

fn clamp_unit(value: f64) -> f64 {
    if value < 0.0 {
        return 0.0;
    }
    if value > 1.0 {
        return 1.0;
    }
    value
}

fn clamp_surface_y(y: i32) -> i32 {
    clamp_i32(y, MIN_SURFACE_Y, MAX_SURFACE_Y)
}

fn clamp_i32(value: i32, min: i32, max: i32) -> i32 {
    value.max(min).min(max)
}

fn normalize_text_default(value: String, default: &str) -> String {
    if value.trim().is_empty() {
        default.to_string()
    } else {
        value
    }
}

fn coverage(value: i32) -> f64 {
    if value == SurfaceMaterialSample::UNKNOWN || value == 255 {
        return 0.0;
    }
    if value <= 0 {
        return 0.0;
    }
    if value >= 100 {
        return 1.0;
    }
    f64::from(value) / 100.0
}

fn mapping_for(
    metadata: &GeoTiffMetadata,
    scale_denominator: i32,
) -> earthmap_geo::Result<EarthScaleMapping> {
    EarthScaleMapping::for_denominator(
        scale_denominator,
        metadata.top_left_latitude - (f64::from(metadata.height) * metadata.pixel_height_degrees),
        metadata.top_left_latitude,
    )
}

fn region_file(
    world_dir: &Path,
    output_format: OutputFormat,
    region_x: i32,
    region_z: i32,
) -> PathBuf {
    let extension = match output_format {
        OutputFormat::Mca => "mca",
        OutputFormat::LinearV2 => "linear",
    };
    world_dir
        .join("region")
        .join(format!("r.{region_x}.{region_z}.{extension}"))
}

fn write_region_with_settings(
    settings: &SurfaceRegionSettings,
    region_file: &Path,
    chunks: &BTreeMap<ChunkLocalPos, Vec<u8>>,
) -> Result<()> {
    match settings.output_format {
        OutputFormat::Mca => {
            if let Some(level) = settings.mca_compression_level {
                earthmap_region::write_mca_region_with_compression(region_file, chunks, 0, level)?;
            } else {
                earthmap_region::write_mca_region(region_file, chunks, 0)?;
            }
        }
        OutputFormat::LinearV2 => {
            if let Some(level) = settings.linear_compression_level {
                earthmap_region::write_linear_v2_region_with_compression(
                    region_file,
                    chunks,
                    0,
                    level,
                )?;
            } else {
                earthmap_region::write_linear_v2_region(region_file, chunks, 0)?;
            }
        }
    }
    Ok(())
}

fn write_region_file(
    output_format: OutputFormat,
    region_file: &Path,
    chunks: &BTreeMap<ChunkLocalPos, Vec<u8>>,
) -> Result<()> {
    match output_format {
        OutputFormat::Mca => earthmap_region::write_mca_region(region_file, chunks, 0)?,
        OutputFormat::LinearV2 => {
            earthmap_region::write_linear_v2_region(region_file, chunks, 0)?;
        }
    }
    Ok(())
}

fn java_math_round_double_to_narrowed_i32(value: f64) -> i32 {
    if value.is_nan() {
        return 0;
    }
    let rounded = if value >= i64::MAX as f64 {
        i64::MAX
    } else if value <= i64::MIN as f64 {
        i64::MIN
    } else {
        (value + 0.5).floor() as i64
    };
    rounded as i32
}

fn java_min(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left <= right {
        left
    } else {
        right
    }
}

fn java_max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left >= right {
        left
    } else {
        right
    }
}

fn java_clamp_unit(value: f64) -> f64 {
    java_max(0.0, java_min(1.0, value))
}

#[cfg(test)]
fn java_height_only_surface_y_reference(elevation_meters: f64) -> i32 {
    let ratio = elevation_meters / ELEVATION_METERS_PER_BLOCK;
    let rounded = if ratio.is_nan() {
        0
    } else if ratio >= i64::MAX as f64 {
        i64::MAX
    } else if ratio <= i64::MIN as f64 {
        i64::MIN
    } else {
        (ratio + 0.5).floor() as i64
    };
    SEA_LEVEL_Y.wrapping_add(rounded as i32).clamp(-60, 319)
}

fn write_exploration_only_manifest(settings: &HeightOnlySettings) -> Result<PathBuf> {
    let mut values = base_exploration_only_manifest("height-only-region");
    values.insert("features.terrainHeight".to_string(), "true".to_string());
    values.insert(
        "generation.format".to_string(),
        settings.output_format.java_name().to_string(),
    );
    values.insert(
        "generation.scaleDenominator".to_string(),
        settings.scale_denominator.to_string(),
    );
    values.insert(
        "generation.regionX".to_string(),
        settings.region_x.to_string(),
    );
    values.insert(
        "generation.regionZ".to_string(),
        settings.region_z.to_string(),
    );

    std::fs::create_dir_all(&settings.world_dir)?;
    let manifest_path = settings.world_dir.join(SURVIVAL_MANIFEST_FILE_NAME);
    let mut file = std::fs::File::create(&manifest_path)?;
    writeln!(file, "# SR EarthMap survival manifest")?;
    for (key, value) in values {
        writeln!(file, "{key}={}", escape_properties_value(&value))?;
    }
    Ok(manifest_path)
}

fn write_surface_region_manifest(settings: &SurfaceRegionSettings) -> Result<PathBuf> {
    let mut values = base_exploration_only_manifest("surface-region");
    values.insert("features.surfaceRules".to_string(), "true".to_string());
    values.insert("features.waterSurface".to_string(), "true".to_string());
    values.insert("features.biomes".to_string(), "heuristic".to_string());
    values.insert(
        "features.surfaceMaterialRaster".to_string(),
        settings.surface_material_path.is_some().to_string(),
    );
    values.insert(
        "features.serverDelegation".to_string(),
        settings.chunk_status.delegates_to_server().to_string(),
    );
    values.insert(
        "generation.format".to_string(),
        settings.output_format.java_name().to_string(),
    );
    values.insert(
        "generation.scaleDenominator".to_string(),
        settings.scale_denominator.to_string(),
    );
    values.insert(
        "generation.regionX".to_string(),
        settings.region_x.to_string(),
    );
    values.insert(
        "generation.regionZ".to_string(),
        settings.region_z.to_string(),
    );
    values.insert(
        "generation.chunkStatus".to_string(),
        settings.chunk_status.id().to_string(),
    );
    values.insert(
        "generation.verticalScale".to_string(),
        java_double_properties_string(settings.vertical_scale),
    );
    values.insert(
        "generation.textureMode".to_string(),
        settings.texture_mode.id().to_string(),
    );
    values.insert(
        "generation.progressionPlacementPolicy".to_string(),
        "none".to_string(),
    );
    values.insert(
        "generation.progressionStrategy".to_string(),
        "none".to_string(),
    );
    values.insert(
        "generation.progressionStructures".to_string(),
        "0".to_string(),
    );
    values.insert("generation.progressionPortalX".to_string(), "0".to_string());
    values.insert("generation.progressionPortalY".to_string(), "0".to_string());
    values.insert("generation.progressionPortalZ".to_string(), "0".to_string());
    values.insert(
        "generation.directProgressionStructures".to_string(),
        "false".to_string(),
    );
    values.insert("generation.directCaves".to_string(), "false".to_string());
    values.insert("generation.directOres".to_string(), "false".to_string());
    values.insert(
        "generation.directVegetation".to_string(),
        "false".to_string(),
    );
    values.insert(
        "generation.directStructures".to_string(),
        "false".to_string(),
    );

    fs::create_dir_all(&settings.world_dir)?;
    let manifest_path = settings.world_dir.join(SURVIVAL_MANIFEST_FILE_NAME);
    let mut file = File::create(&manifest_path)?;
    writeln!(file, "# SR EarthMap survival manifest")?;
    for (key, value) in values {
        writeln!(file, "{key}={}", escape_properties_value(&value))?;
    }
    Ok(manifest_path)
}

fn base_exploration_only_manifest(generator: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    values.insert("manifest.version".to_string(), "1".to_string());
    values.insert(
        "minecraft.version".to_string(),
        build_info::MINECRAFT_TARGET.to_string(),
    );
    values.insert("gameplay.claim".to_string(), "exploration-only".to_string());
    values.insert("generator.name".to_string(), generator.to_string());
    values.insert(
        "generator.targetGameplayObjective".to_string(),
        build_info::GAMEPLAY_PROFILE.to_string(),
    );
    values.insert(
        "generator.currentGameplayClaim".to_string(),
        "exploration-only".to_string(),
    );
    values.insert("delegated.lighting".to_string(), "true".to_string());
    values.insert(
        "delegated.vanillaDimensions".to_string(),
        "true".to_string(),
    );
    for key in REQUIRED_SURVIVAL_BOOLEAN_KEYS {
        values.insert((*key).to_string(), "false".to_string());
    }
    values
}

fn java_double_properties_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == f64::INFINITY {
        return "Infinity".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    if value.is_finite() && value.fract() == 0.0 {
        return format!("{value:.1}");
    }
    value.to_string()
}

fn escape_properties_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

const REQUIRED_SURVIVAL_BOOLEAN_KEYS: &[&str] = &[
    "evidence.serverBootSaveReboot",
    "evidence.spawnToEnd",
    "features.caves",
    "features.caveConnectivity",
    "features.ores",
    "features.strongholdOrEquivalent",
    "features.endPortal",
    "features.netherProgression",
    "features.lootTables",
    "features.spawners",
    "reports.oreHistogram",
    "reports.caveConnectivity",
    "reports.structureMetadata",
    "reports.resourceFairness",
];

struct ChunkBuild {
    chunk: ChunkModel,
    min_surface_y: i32,
    max_surface_y: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_texture_mode_parse_matches_java_aliases() {
        assert_eq!(
            SurfaceTextureMode::parse("classified").unwrap(),
            SurfaceTextureMode::Classified
        );
        assert_eq!(
            SurfaceTextureMode::parse("classify").unwrap(),
            SurfaceTextureMode::Classified
        );
        assert_eq!(
            SurfaceTextureMode::parse("semantic").unwrap(),
            SurfaceTextureMode::Classified
        );
        assert_eq!(
            SurfaceTextureMode::parse("terrain").unwrap(),
            SurfaceTextureMode::Classified
        );
        assert_eq!(
            SurfaceTextureMode::parse("photo").unwrap(),
            SurfaceTextureMode::Photo
        );
        assert_eq!(
            SurfaceTextureMode::parse("satellite_photo").unwrap(),
            SurfaceTextureMode::Photo
        );
        assert_eq!(
            SurfaceTextureMode::parse("true-marble").unwrap(),
            SurfaceTextureMode::Photo
        );
        assert_eq!(
            SurfaceTextureMode::parse("truemarble").unwrap(),
            SurfaceTextureMode::Photo
        );
        assert_eq!(
            SurfaceTextureMode::parse("").unwrap_err().to_string(),
            "textureMode must not be blank"
        );
        assert_eq!(
            SurfaceTextureMode::parse("painted")
                .unwrap_err()
                .to_string(),
            "textureMode must be classified or photo: painted"
        );
    }

    #[test]
    fn surface_material_classifier_color_only_land_matches_java_bootstrap_cases() {
        let base_land = surface_column(
            false,
            SEA_LEVEL_Y + 10,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );

        let desert = apply_test_surface_material(
            &base_land,
            RgbColor::of(240, 213, 126),
            250.0,
            13.0,
            24.0,
            0.0,
            0.0,
        );
        assert_eq!(desert.top_block_state_id, block_state_ids::SAND);
        assert_eq!(desert.biome_id, "minecraft:desert");

        let met_dry_grass = apply_test_surface_material(
            &base_land,
            RgbColor::of(167, 146, 103),
            250.0,
            13.0,
            24.0,
            0.0,
            0.0,
        );
        assert_eq!(
            met_dry_grass.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(met_dry_grass.biome_id, "minecraft:desert");

        let sahel = apply_test_surface_material(
            &base_land,
            RgbColor::of(126, 123, 70),
            220.0,
            12.0,
            12.0,
            0.0,
            0.0,
        );
        assert_eq!(sahel.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert!(matches!(
            sahel.biome_id.as_str(),
            "minecraft:savanna" | "minecraft:savanna_plateau"
        ));

        let dry_sahel = apply_test_surface_material(
            &base_land,
            RgbColor::of(174, 151, 86),
            260.0,
            8.0,
            13.0,
            0.0,
            0.0,
        );
        assert!(matches!(
            dry_sahel.top_block_state_id,
            block_state_ids::GRASS_BLOCK | block_state_ids::COARSE_DIRT
        ));
        assert!(matches!(
            dry_sahel.biome_id.as_str(),
            "minecraft:savanna" | "minecraft:savanna_plateau"
        ));

        let congo = apply_test_surface_material(
            &base_land,
            RgbColor::of(31, 93, 37),
            350.0,
            20.0,
            -2.0,
            0.0,
            0.0,
        );
        assert_eq!(congo.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert!(matches!(
            congo.biome_id.as_str(),
            "minecraft:jungle" | "minecraft:sparse_jungle"
        ));

        let atlas_rock = apply_test_surface_material(
            &base_land,
            RgbColor::of(181, 96, 46),
            1200.0,
            -6.0,
            30.0,
            0.0,
            260.0,
        );
        assert!(matches!(
            atlas_rock.top_block_state_id,
            block_state_ids::ORANGE_TERRACOTTA | block_state_ids::TERRACOTTA
        ));
        assert!(matches!(
            atlas_rock.biome_id.as_str(),
            "minecraft:badlands" | "minecraft:wooded_badlands"
        ));
    }

    #[test]
    fn surface_material_classifier_semantic_fallback_matches_java_bootstrap_cases() {
        let base_land = surface_column(
            false,
            SEA_LEVEL_Y + 10,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );

        let missing_land_raster = apply_surface_material(
            &base_land,
            &SurfaceMaterialSample::color_only(RgbColor::unavailable()),
            250.0,
            13.0,
            24.0,
            0.0,
            0.0,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();
        assert_eq!(missing_land_raster.decision_source, "base");

        let no_color_savanna = SurfaceMaterialSample::land(
            RgbColor::unavailable(),
            3,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            25,
            12,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            40,
            SurfaceMaterialSample::UNKNOWN,
            "West Sudanian savanna",
            "minecraft:savanna",
            1.0,
        );
        let semantic_savanna = apply_test_surface_material_sample(
            &base_land,
            no_color_savanna,
            260.0,
            8.0,
            13.0,
            0.0,
            0.0,
        );
        assert!(matches!(
            semantic_savanna.biome_id.as_str(),
            "minecraft:savanna"
                | "minecraft:savanna_plateau"
                | "minecraft:windswept_savanna"
                | "minecraft:plains"
                | "minecraft:sunflower_plains"
        ));
        assert_eq!(semantic_savanna.decision_source, "intent");

        let no_color_desert = SurfaceMaterialSample::land(
            RgbColor::unavailable(),
            4,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            20,
            SurfaceMaterialSample::UNKNOWN,
            "Sahara desert",
            "minecraft:desert",
            1.0,
        );
        let semantic_desert = apply_test_surface_material_sample(
            &base_land,
            no_color_desert,
            250.0,
            13.0,
            24.0,
            0.0,
            0.0,
        );
        assert_eq!(semantic_desert.top_block_state_id, block_state_ids::SAND);
        assert_eq!(semantic_desert.biome_id, "minecraft:desert");
        assert_eq!(semantic_desert.decision_source, "intent-ecoregion");

        let no_color_arabian_desert = SurfaceMaterialSample::land(
            RgbColor::unavailable(),
            4,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            4,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            25,
            SurfaceMaterialSample::UNKNOWN,
            "Arabian desert",
            "minecraft:desert",
            1.0,
        );
        let semantic_arabian_desert = apply_test_surface_material_sample(
            &base_land,
            no_color_arabian_desert,
            250.0,
            55.0,
            15.0,
            0.0,
            0.0,
        );
        assert_eq!(
            semantic_arabian_desert.top_block_state_id,
            block_state_ids::SAND
        );
        assert_eq!(semantic_arabian_desert.biome_id, "minecraft:desert");
        assert_eq!(semantic_arabian_desert.decision_source, "intent-ecoregion");

        let no_color_forest = SurfaceMaterialSample::land(
            RgbColor::unavailable(),
            15,
            6,
            2,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            12,
            4,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            80,
            SurfaceMaterialSample::UNKNOWN,
            "Western European broadleaf forests",
            "minecraft:forest",
            1.0,
        );
        let semantic_forest = apply_test_surface_material_sample(
            &base_land,
            no_color_forest,
            180.0,
            10.0,
            50.0,
            0.0,
            0.0,
        );
        assert_eq!(
            semantic_forest.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert!(matches!(
            semantic_forest.biome_id.as_str(),
            "minecraft:forest" | "minecraft:dark_forest"
        ));
    }

    #[test]
    fn surface_material_classifier_climate_intent_matches_java_bootstrap_cases() {
        let base_land = surface_column(
            false,
            SEA_LEVEL_Y + 10,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );

        let bright_congo_forest = SurfaceMaterialSample::land(
            RgbColor::of(103, 126, 68),
            2,
            18,
            10,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            22,
            14,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            35,
            SurfaceMaterialSample::UNKNOWN,
            "Central Congolian lowland forests",
            "minecraft:jungle",
            1.0,
        );
        let semantic_congo = apply_test_surface_material_sample(
            &base_land,
            bright_congo_forest,
            330.0,
            20.0,
            -2.0,
            0.0,
            0.0,
        );
        assert_eq!(
            semantic_congo.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert!(
            semantic_congo.biome_id.contains("jungle")
                || semantic_congo.biome_id.contains("forest")
        );

        let moss_congo_forest = SurfaceMaterialSample::with_export_token(
            RgbColor::of(0, 50, 0),
            RgbColor::of(0, 50, 0),
            2,
            35,
            10,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            22,
            14,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            35,
            "Central Congolian lowland forests",
            "minecraft:jungle",
            1.0,
        );
        let moss_congo = apply_test_surface_material_sample(
            &base_land,
            moss_congo_forest,
            330.0,
            20.0,
            -2.0,
            0.0,
            0.0,
        );
        assert_eq!(moss_congo.top_block_state_id, block_state_ids::MOSS_BLOCK);
        assert!(moss_congo.biome_id.contains("jungle") || moss_congo.biome_id.contains("forest"));
        assert_eq!(moss_congo.decision_source, "intent");

        let equatorial_snow = SurfaceMaterialSample::climate_only(
            RgbColor::of(232, 238, 236),
            29,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            45,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
        );
        let snow = apply_test_surface_material_sample(
            &base_land,
            equatorial_snow,
            50.0,
            20.0,
            0.0,
            0.0,
            0.0,
        );
        assert_eq!(snow.top_block_state_id, block_state_ids::SNOW_BLOCK);
        assert_eq!(snow.biome_id, "minecraft:snowy_plains");
        assert_eq!(snow.decision_source, "intent");

        let muddy_wetland = SurfaceMaterialSample::land(
            RgbColor::of(45, 75, 48),
            12,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            20,
            10,
            SurfaceMaterialSample::UNKNOWN,
            80,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Coastal swamp mosaic",
            "minecraft:swamp",
            0.90,
        );
        let (swamp_longitude, swamp_latitude) = (-180..=180)
            .flat_map(|longitude| (-45..=45).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                surface_material_ecology_noise(
                    f64::from(longitude),
                    f64::from(latitude),
                    7.5,
                    0x9d6c63b5a8e33f21_u64 as i64,
                ) >= 0.82
            })
            .expect("test fixture should find a muddy wetland fine-noise coordinate");
        let swamp = apply_test_surface_material_sample(
            &base_land,
            muddy_wetland,
            20.0,
            f64::from(swamp_longitude),
            f64::from(swamp_latitude),
            0.0,
            0.0,
        );
        assert_eq!(swamp.top_block_state_id, block_state_ids::MUD);
        assert_eq!(swamp.filler_block_state_id, block_state_ids::MUD);
        assert_eq!(swamp.biome_id, "minecraft:swamp");
        assert_eq!(swamp.decision_source, "intent");

        let tropical_savanna = SurfaceMaterialSample::climate_only(
            RgbColor::of(181, 96, 46),
            3,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            35,
            20,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
        );
        let climate_sahel = apply_test_surface_material_sample(
            &base_land,
            tropical_savanna,
            800.0,
            8.0,
            13.0,
            0.0,
            0.0,
        );
        assert!(matches!(
            climate_sahel.top_block_state_id,
            block_state_ids::GRASS_BLOCK | block_state_ids::COARSE_DIRT
        ));
        assert!(matches!(
            climate_sahel.biome_id.as_str(),
            "minecraft:savanna"
                | "minecraft:savanna_plateau"
                | "minecraft:windswept_savanna"
                | "minecraft:plains"
                | "minecraft:sunflower_plains"
        ));
        assert_eq!(climate_sahel.decision_source, "intent");

        let sahel_band_olive = SurfaceMaterialSample::color_only(RgbColor::of(126, 123, 70));
        let olive_sahel = apply_test_surface_material_sample(
            &base_land,
            sahel_band_olive,
            220.0,
            12.0,
            12.0,
            0.0,
            0.0,
        );
        assert_eq!(olive_sahel.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert_eq!(olive_sahel.filler_block_state_id, block_state_ids::DIRT);
        assert!(matches!(
            olive_sahel.biome_id.as_str(),
            "minecraft:savanna"
                | "minecraft:savanna_plateau"
                | "minecraft:windswept_savanna"
                | "minecraft:plains"
                | "minecraft:sunflower_plains"
        ));
        assert_eq!(olive_sahel.decision_source, "intent");

        let mediterranean_open_scrub = SurfaceMaterialSample::color_only(RgbColor::of(118, 98, 58));
        let mediterranean_metrics = SurfaceColorMetrics::from(mediterranean_open_scrub.color);
        assert!(!is_green_like(mediterranean_metrics));
        let (mediterranean_longitude, mediterranean_latitude) = (-11..=43)
            .flat_map(|longitude| (31..=46).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let patch_noise =
                    surface_material_ecology_noise(longitude, latitude, 2.4, 0x4165d9e7a1f31c0b);
                let fine_noise = surface_material_ecology_noise(
                    longitude,
                    latitude,
                    7.5,
                    0x9d6c63b5a8e33f21_u64 as i64,
                );
                surface_material_mediterranean_score(longitude, latitude) >= 0.40
                    && (0.42..0.58).contains(&patch_noise)
                    && fine_noise < 0.92
            })
            .expect("test fixture should find an open Mediterranean scrub coordinate");
        let mediterranean_scrub = apply_test_surface_material_sample(
            &base_land,
            mediterranean_open_scrub,
            260.0,
            f64::from(mediterranean_longitude),
            f64::from(mediterranean_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            mediterranean_scrub.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            mediterranean_scrub.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert_eq!(mediterranean_scrub.biome_id, "minecraft:sunflower_plains");
        assert_eq!(mediterranean_scrub.decision_source, "intent");

        let vegetated_dry_ecoregion_mediterranean = SurfaceMaterialSample::land(
            RgbColor::of(86, 112, 68),
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            0,
            0,
            22,
            8,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Mediterranean desert steppe",
            "minecraft:desert",
            0.82,
        );
        let vegetated_mediterranean = apply_test_surface_material_sample(
            &base_land,
            vegetated_dry_ecoregion_mediterranean,
            260.0,
            f64::from(mediterranean_longitude),
            f64::from(mediterranean_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            vegetated_mediterranean.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            vegetated_mediterranean.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert_eq!(vegetated_mediterranean.decision_source, "intent");

        let coarse_token_savanna = SurfaceMaterialSample::new(
            RgbColor::of(140, 80, 50),
            RgbColor::of(140, 80, 50),
            TerrainTokenSource::JavaStandardPalette,
            3,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            4,
            4,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "West Sudanian savanna",
            "minecraft:savanna",
            1.0,
        );
        let coarse_metrics = SurfaceColorMetrics::from(coarse_token_savanna.color);
        assert_eq!(
            surface_material_met_terrain(&coarse_token_savanna, coarse_metrics).kind,
            MetTerrainKind::CoarseDirt
        );
        let (coarse_longitude, coarse_latitude) = (-30..=40)
            .flat_map(|longitude| (5..=20).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let patch_noise = surface_material_ecology_noise(
                    f64::from(longitude),
                    f64::from(latitude),
                    2.4,
                    0x4165d9e7a1f31c0b,
                );
                let fine_noise = surface_material_ecology_noise(
                    f64::from(longitude),
                    f64::from(latitude),
                    7.5,
                    0x9d6c63b5a8e33f21_u64 as i64,
                );
                let dry_score =
                    surface_material_sahel_score(f64::from(longitude), f64::from(latitude))
                        .max(surface_material_dry_savanna_score(
                            f64::from(longitude),
                            f64::from(latitude),
                        ))
                        .max(0.45);
                dry_score >= 0.52
                    && fine_noise >= 0.42
                    && !(fine_noise >= 0.92 && patch_noise >= 0.56)
            })
            .expect("test fixture should find a conservative dry terrain-token coordinate");
        let coarse_savanna = apply_test_surface_material_sample(
            &base_land,
            coarse_token_savanna,
            260.0,
            f64::from(coarse_longitude),
            f64::from(coarse_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            coarse_savanna.top_block_state_id,
            block_state_ids::COARSE_DIRT
        );
        assert_eq!(coarse_savanna.decision_source, "intent");

        let sparse_dry_open_tropical = SurfaceMaterialSample::land(
            RgbColor::of(126, 123, 70),
            2,
            4,
            3,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            8,
            6,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Guinean forest-savanna mosaic",
            "minecraft:savanna",
            0.82,
        );
        let tropical_edge = apply_test_surface_material_sample(
            &base_land,
            sparse_dry_open_tropical,
            280.0,
            8.0,
            8.0,
            0.0,
            0.0,
        );
        assert_eq!(
            tropical_edge.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert!(
            !tropical_edge.biome_id.contains("jungle")
                && !tropical_edge.biome_id.contains("forest")
        );
        assert_eq!(tropical_edge.decision_source, "intent");

        let non_sahel_sparse_tropical_mosaic = SurfaceMaterialSample::land(
            RgbColor::of(126, 123, 70),
            2,
            4,
            3,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            8,
            6,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Tropical savanna edge",
            "minecraft:savanna",
            0.82,
        );
        let non_sahel_tropical_edge = apply_test_surface_material_sample(
            &base_land,
            non_sahel_sparse_tropical_mosaic,
            280.0,
            80.0,
            3.0,
            0.0,
            0.0,
        );
        assert_eq!(
            non_sahel_tropical_edge.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert!(
            non_sahel_tropical_edge.biome_id.contains("jungle")
                || non_sahel_tropical_edge.biome_id.contains("forest")
        );
        assert_eq!(non_sahel_tropical_edge.decision_source, "environment");

        let named_savanna_mosaic = SurfaceMaterialSample::land(
            RgbColor::of(126, 123, 70),
            2,
            4,
            3,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            8,
            6,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Tropical savanna mosaic",
            "minecraft:savanna",
            0.82,
        );
        let savanna_mosaic = apply_test_surface_material_sample(
            &base_land,
            named_savanna_mosaic,
            280.0,
            80.0,
            3.0,
            0.0,
            0.0,
        );
        assert_eq!(
            savanna_mosaic.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(savanna_mosaic.decision_source, "intent");

        let bare_named_mosaic = SurfaceMaterialSample::land(
            RgbColor::of(166, 152, 126),
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Tropical savanna mosaic",
            "minecraft:savanna",
            0.82,
        );
        let bare_metrics = SurfaceColorMetrics::from(bare_named_mosaic.color);
        assert!(!is_forest_savanna_mosaic_intent(
            &bare_named_mosaic,
            bare_metrics,
            80.0,
            3.0,
            surface_material_sahara_score(80.0, 3.0),
            surface_material_rainforest_score(80.0, 3.0),
            surface_material_dry_savanna_score(80.0, 3.0),
            false,
            0.0,
            0.0,
        ));
        let bare_mosaic = apply_test_surface_material_sample(
            &base_land,
            bare_named_mosaic,
            280.0,
            80.0,
            3.0,
            0.0,
            0.0,
        );
        assert!(matches!(
            bare_mosaic.top_block_state_id,
            block_state_ids::GRASS_BLOCK | block_state_ids::COARSE_DIRT
        ));
        assert_eq!(bare_mosaic.filler_block_state_id, block_state_ids::DIRT);
        assert_eq!(bare_mosaic.decision_source, "intent");

        let sahara_named_mosaic = SurfaceMaterialSample::land(
            RgbColor::of(126, 123, 70),
            SurfaceMaterialSample::UNKNOWN,
            4,
            3,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            8,
            6,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Tropical savanna mosaic",
            "minecraft:savanna",
            0.82,
        );
        let sahara_metrics = SurfaceColorMetrics::from(sahara_named_mosaic.color);
        assert!(surface_material_sahara_score(20.0, 25.0) >= 0.35);
        assert!(!is_forest_savanna_mosaic_intent(
            &sahara_named_mosaic,
            sahara_metrics,
            20.0,
            25.0,
            surface_material_sahara_score(20.0, 25.0),
            surface_material_rainforest_score(20.0, 25.0),
            surface_material_dry_savanna_score(20.0, 25.0),
            false,
            0.0,
            0.0,
        ));

        let humid_named_mosaic = SurfaceMaterialSample::with_export_token(
            RgbColor::of(35, 86, 34),
            RgbColor::of(55, 70, 50),
            2,
            36,
            30,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            24,
            8,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Guinean forest-savanna mosaic",
            "minecraft:sparse_jungle",
            0.88,
        );
        let humid_mosaic = apply_test_surface_material_sample(
            &base_land,
            humid_named_mosaic,
            280.0,
            -8.0,
            0.0,
            0.0,
            0.0,
        );
        assert_eq!(humid_mosaic.top_block_state_id, block_state_ids::MOSS_BLOCK);
        assert_eq!(humid_mosaic.decision_source, "intent");

        let green_sparse_tropical_mosaic = SurfaceMaterialSample::land(
            RgbColor::of(42, 95, 38),
            2,
            4,
            3,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            8,
            6,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Guinean forest-savanna mosaic",
            "minecraft:savanna",
            0.82,
        );
        let green_tropical_edge = apply_test_surface_material_sample(
            &base_land,
            green_sparse_tropical_mosaic,
            280.0,
            8.0,
            8.0,
            0.0,
            0.0,
        );
        assert_eq!(
            green_tropical_edge.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert!(
            green_tropical_edge.biome_id.contains("jungle")
                || green_tropical_edge.biome_id.contains("forest")
        );
        assert_eq!(green_tropical_edge.decision_source, "intent");

        let no_color_sparse_tropical_mosaic = SurfaceMaterialSample::land(
            RgbColor::unavailable(),
            2,
            4,
            3,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            8,
            6,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Guinean forest-savanna mosaic",
            "minecraft:savanna",
            0.82,
        );
        let no_color_tropical_edge = apply_test_surface_material_sample(
            &base_land,
            no_color_sparse_tropical_mosaic,
            280.0,
            8.0,
            8.0,
            0.0,
            0.0,
        );
        assert_eq!(
            no_color_tropical_edge.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert!(
            no_color_tropical_edge.biome_id.contains("jungle")
                || no_color_tropical_edge.biome_id.contains("forest")
        );
        assert_eq!(no_color_tropical_edge.decision_source, "intent");

        let low_latitude_steppe = SurfaceMaterialSample::land(
            RgbColor::of(134, 126, 82),
            6,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            18,
            4,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            45,
            SurfaceMaterialSample::UNKNOWN,
            "Low latitude steppe",
            "minecraft:plains",
            0.45,
        );
        let (low_steppe_longitude, low_steppe_latitude) = (-180..=180)
            .flat_map(|longitude| (-32..=32).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let patch_noise =
                    surface_material_ecology_noise(longitude, latitude, 2.4, 0x4165d9e7a1f31c0b);
                let fine_noise = surface_material_ecology_noise(
                    longitude,
                    latitude,
                    7.5,
                    0x9d6c63b5a8e33f21_u64 as i64,
                );
                !is_sahel_latitude(latitude)
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.18
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
                    && patch_noise >= 0.72
                    && (0.34..0.92).contains(&fine_noise)
            })
            .expect("test fixture should find a low-latitude non-dry steppe coordinate");
        let low_steppe = apply_test_surface_material_sample(
            &base_land,
            low_latitude_steppe,
            320.0,
            f64::from(low_steppe_longitude),
            f64::from(low_steppe_latitude),
            0.0,
            0.0,
        );
        assert_eq!(low_steppe.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert_eq!(low_steppe.filler_block_state_id, block_state_ids::DIRT);
        assert_eq!(low_steppe.biome_id, "minecraft:sunflower_plains");
        assert_eq!(low_steppe.decision_source, "intent");

        let dry_savanna_score_only = SurfaceMaterialSample::color_only(RgbColor::of(126, 123, 70));
        let (dry_score_longitude, dry_score_latitude) = (-180..=180)
            .flat_map(|longitude| (-35..=35).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                !is_sahel_latitude(latitude)
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) >= 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
            })
            .expect("test fixture should find a standalone dry-savanna-score coordinate");
        let dry_score_savanna = apply_test_surface_material_sample(
            &base_land,
            dry_savanna_score_only,
            340.0,
            f64::from(dry_score_longitude),
            f64::from(dry_score_latitude),
            0.0,
            0.0,
        );
        assert!(matches!(
            dry_score_savanna.top_block_state_id,
            block_state_ids::GRASS_BLOCK | block_state_ids::COARSE_DIRT
        ));
        assert_eq!(
            dry_score_savanna.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert!(matches!(
            dry_score_savanna.biome_id.as_str(),
            "minecraft:savanna"
                | "minecraft:savanna_plateau"
                | "minecraft:windswept_savanna"
                | "minecraft:plains"
                | "minecraft:sunflower_plains"
        ));
        assert_eq!(dry_score_savanna.decision_source, "intent");

        let java_standard_vegetated_token = SurfaceMaterialSample::new(
            RgbColor::of(167, 146, 103),
            RgbColor::of(167, 146, 103),
            TerrainTokenSource::JavaStandardPalette,
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            0,
            0,
            20,
            6,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Open shrubland",
            "minecraft:plains",
            0.40,
        );
        let java_standard_metrics = SurfaceColorMetrics::from(java_standard_vegetated_token.color);
        let java_standard_terrain =
            surface_material_met_terrain(&java_standard_vegetated_token, java_standard_metrics);
        assert_eq!(java_standard_terrain.kind, MetTerrainKind::Vegetated);
        assert!(java_standard_terrain.confident());
        assert!(!is_green_like(java_standard_metrics));
        let (java_standard_longitude, java_standard_latitude) = (-125..=147)
            .flat_map(|longitude| (-39..=42).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let mediterranean = surface_material_mediterranean_score(longitude, latitude);
                latitude.abs() <= 35.0
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.35
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
                    && (0.32..0.40).contains(&mediterranean)
            })
            .expect("test fixture should find a JavaStandard vegetated-token intent coordinate");
        let java_standard_token_dry = apply_test_surface_material_sample(
            &base_land,
            java_standard_vegetated_token,
            280.0,
            f64::from(java_standard_longitude),
            f64::from(java_standard_latitude),
            0.0,
            0.0,
        );
        assert!(matches!(
            java_standard_token_dry.top_block_state_id,
            block_state_ids::GRASS_BLOCK | block_state_ids::COARSE_DIRT
        ));
        assert_eq!(
            java_standard_token_dry.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert!(matches!(
            java_standard_token_dry.biome_id.as_str(),
            "minecraft:savanna"
                | "minecraft:savanna_plateau"
                | "minecraft:windswept_savanna"
                | "minecraft:plains"
                | "minecraft:sunflower_plains"
        ));
        assert_eq!(java_standard_token_dry.decision_source, "intent");

        let bright_savanna_desert_edge = SurfaceMaterialSample::land(
            RgbColor::of(230, 205, 160),
            4,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "West Sudanian savanna",
            "minecraft:savanna",
            0.82,
        );
        let bright_savanna_metrics = SurfaceColorMetrics::from(bright_savanna_desert_edge.color);
        assert!(is_desert_sand_like(bright_savanna_metrics));
        let (savanna_edge_longitude, savanna_edge_latitude) = (-20..=35)
            .flat_map(|longitude| (6..=19).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let sahel = surface_material_sahel_score(longitude, latitude);
                let dry_savanna = surface_material_dry_savanna_score(longitude, latitude);
                let sand_patch_noise =
                    surface_material_ecology_noise(longitude, latitude, 0.46, 0x519bc3a22e8f4d31);
                let sand_patch_threshold = clamp_unit(
                    0.04 + (surface_material_sahara_score(longitude, latitude) * 0.10)
                        + ((bright_savanna_metrics.value - 0.78) * 0.28)
                        - (sahel * 0.18)
                        - (dry_savanna * 0.10),
                );
                is_sahel_latitude(latitude)
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && dry_savanna < 0.25
                    && sand_patch_noise >= sand_patch_threshold
            })
            .expect("test fixture should find a savanna-like desert-edge coordinate");
        let savanna_ecoregion_edge = apply_test_surface_material_sample(
            &base_land,
            bright_savanna_desert_edge,
            280.0,
            f64::from(savanna_edge_longitude),
            f64::from(savanna_edge_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            savanna_ecoregion_edge.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert_eq!(savanna_ecoregion_edge.biome_id, "minecraft:savanna");
        assert_eq!(savanna_ecoregion_edge.decision_source, "intent");

        let bright_savanna_hot_desert = SurfaceMaterialSample::land(
            RgbColor::of(230, 205, 160),
            4,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Xeric grassland",
            "minecraft:plains",
            0.62,
        );
        let (hot_savanna_longitude, hot_savanna_latitude) = (-180..=180)
            .flat_map(|longitude| (-38..=38).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let sahel = surface_material_sahel_score(longitude, latitude);
                let dry_savanna = surface_material_dry_savanna_score(longitude, latitude);
                let sand_patch_noise =
                    surface_material_ecology_noise(longitude, latitude, 0.42, 0x1b8d4a732e66c1d7);
                let sand_patch_threshold = clamp_unit(
                    0.12 + (surface_material_sahara_score(longitude, latitude) * 0.24)
                        + ((bright_savanna_metrics.value - 0.70) * 0.46)
                        - (sahel * 0.10)
                        - (dry_savanna * 0.08),
                );
                surface_material_rainforest_score(longitude, latitude) < 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && dry_savanna < 0.35
                    && sand_patch_noise < sand_patch_threshold
            })
            .expect("test fixture should find a savanna-like hot-desert coordinate");
        let savanna_hot_desert = apply_test_surface_material_sample(
            &base_land,
            bright_savanna_hot_desert,
            280.0,
            f64::from(hot_savanna_longitude),
            f64::from(hot_savanna_latitude),
            0.0,
            0.0,
        );
        assert!(matches!(
            savanna_hot_desert.top_block_state_id,
            block_state_ids::SAND
                | block_state_ids::GRAVEL
                | block_state_ids::COARSE_DIRT
                | block_state_ids::RED_SAND
        ));
        assert_eq!(savanna_hot_desert.biome_id, "minecraft:desert");
        assert_eq!(savanna_hot_desert.decision_source, "intent");

        let exposed_highland_rock = SurfaceMaterialSample::color_only(RgbColor::of(181, 96, 46));
        let highland_metrics = SurfaceColorMetrics::from(exposed_highland_rock.color);
        assert!(is_orange_rock_like(highland_metrics));
        let (highland_longitude, highland_latitude) = (-180..=180)
            .flat_map(|longitude| (-45..=45).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let sahara = surface_material_sahara_score(longitude, latitude);
                let dry_savanna = surface_material_dry_savanna_score(longitude, latitude);
                surface_material_sahel_score(longitude, latitude) < 0.18
                    && dry_savanna < 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
                    && is_exposed_dry_rock(
                        highland_metrics,
                        1_200.0,
                        longitude,
                        latitude,
                        sahara,
                        dry_savanna,
                        260.0,
                        0.0,
                    )
            })
            .expect("test fixture should find an exposed highland rock coordinate");
        let highland_rock = apply_test_surface_material_sample(
            &base_land,
            exposed_highland_rock,
            1_200.0,
            f64::from(highland_longitude),
            f64::from(highland_latitude),
            0.0,
            260.0,
        );
        assert_eq!(
            highland_rock.top_block_state_id,
            block_state_ids::ORANGE_TERRACOTTA
        );
        assert_eq!(
            highland_rock.filler_block_state_id,
            block_state_ids::ORANGE_TERRACOTTA
        );
        assert_eq!(highland_rock.biome_id, "minecraft:wooded_badlands");
        assert_eq!(highland_rock.decision_source, "intent");

        let java_standard_rock_token = SurfaceMaterialSample::new(
            RgbColor::of(64, 64, 64),
            RgbColor::of(64, 64, 64),
            TerrainTokenSource::JavaStandardPalette,
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "",
            "",
            0.0,
        );
        let java_standard_rock_metrics = SurfaceColorMetrics::from(java_standard_rock_token.color);
        let java_standard_rock_terrain =
            surface_material_met_terrain(&java_standard_rock_token, java_standard_rock_metrics);
        assert_eq!(java_standard_rock_terrain.kind, MetTerrainKind::Rock);
        assert!(java_standard_rock_terrain.confident());
        let (standard_rock_longitude, standard_rock_latitude) = (-20..=60)
            .flat_map(|longitude| (18..=34).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                surface_material_sahara_score(longitude, latitude) >= 0.35
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
            })
            .expect("test fixture should find a JavaStandard rock highland coordinate");
        let standard_rock_highland = apply_test_surface_material_sample(
            &base_land,
            java_standard_rock_token,
            2_400.0,
            f64::from(standard_rock_longitude),
            f64::from(standard_rock_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            standard_rock_highland.top_block_state_id,
            block_state_ids::STONE
        );
        assert_eq!(
            standard_rock_highland.filler_block_state_id,
            block_state_ids::STONE
        );
        assert_eq!(standard_rock_highland.biome_id, "minecraft:windswept_hills");
        assert_eq!(standard_rock_highland.decision_source, "intent");

        let dry_core_transition_candidate = SurfaceMaterialSample::land(
            RgbColor::of(166, 152, 126),
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Dry desert transition",
            "minecraft:desert",
            0.80,
        );
        let dry_core_transition_metrics =
            SurfaceColorMetrics::from(dry_core_transition_candidate.color);
        assert!(is_dry_core_ecoregion_evidence(
            &dry_core_transition_candidate,
            dry_core_transition_metrics,
            false
        ));
        let (dry_core_transition_longitude, dry_core_transition_latitude) = (-180..=180)
            .flat_map(|longitude| (-42..=42).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let dry_savanna = surface_material_dry_savanna_score(longitude, latitude);
                let transition_noise =
                    surface_material_ecology_noise(longitude, latitude, 1.25, 0x4bd1a7240f78c8d3);
                let confidence_blend =
                    (0.92 - dry_core_transition_candidate.ecoregion_confidence) / 0.92;
                let dry_savanna_blend =
                    clamp_unit(0.22 + (dry_savanna * 0.28) + (confidence_blend * 0.35));
                !is_sahel_latitude(latitude)
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && dry_savanna >= 0.32
                    && dry_savanna < 0.35
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
                    && transition_noise < dry_savanna_blend
            })
            .expect("test fixture should find a dry-core ecoregion transition coordinate");
        let dry_core_transition = apply_test_surface_material_sample(
            &base_land,
            dry_core_transition_candidate,
            420.0,
            f64::from(dry_core_transition_longitude),
            f64::from(dry_core_transition_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            dry_core_transition.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert_eq!(dry_core_transition.biome_id, "minecraft:savanna");
        assert_eq!(dry_core_transition.decision_source, "intent-ecoregion");

        let dry_core_hot_desert_candidate = SurfaceMaterialSample::land(
            RgbColor::of(230, 205, 160),
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Sahara desert",
            "minecraft:desert",
            0.80,
        );
        let dry_core_hot_metrics = SurfaceColorMetrics::from(dry_core_hot_desert_candidate.color);
        assert!(is_dry_core_ecoregion_evidence(
            &dry_core_hot_desert_candidate,
            dry_core_hot_metrics,
            false
        ));
        let (dry_core_hot_longitude, dry_core_hot_latitude) = (-180..=180)
            .flat_map(|longitude| (-42..=42).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                !is_sahel_latitude(latitude)
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.32
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
            })
            .expect("test fixture should find a dry-core hot-desert coordinate");
        let dry_core_hot_desert = apply_test_surface_material_sample(
            &base_land,
            dry_core_hot_desert_candidate,
            300.0,
            f64::from(dry_core_hot_longitude),
            f64::from(dry_core_hot_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            dry_core_hot_desert.top_block_state_id,
            block_state_ids::SAND
        );
        assert_eq!(
            dry_core_hot_desert.filler_block_state_id,
            block_state_ids::SAND
        );
        assert_eq!(dry_core_hot_desert.biome_id, "minecraft:desert");
        assert_eq!(dry_core_hot_desert.decision_source, "intent-ecoregion");

        let desert_climate_vegetation_edge = SurfaceMaterialSample::land(
            RgbColor::of(84, 132, 76),
            4,
            12,
            0,
            18,
            0,
            22,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "",
            "",
            0.0,
        );
        let (desert_vegetation_longitude, desert_vegetation_latitude) = (-180..=180)
            .flat_map(|longitude| (-42..=42).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                !is_sahel_latitude(latitude)
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.18
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
            })
            .expect("test fixture should find a non-Sahel desert-climate vegetation coordinate");
        let desert_vegetation_edge = apply_test_surface_material_sample(
            &base_land,
            desert_climate_vegetation_edge,
            260.0,
            f64::from(desert_vegetation_longitude),
            f64::from(desert_vegetation_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            desert_vegetation_edge.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert_eq!(desert_vegetation_edge.biome_id, "minecraft:savanna");
        assert_eq!(desert_vegetation_edge.decision_source, "intent");

        let forest_like_ecoregion = SurfaceMaterialSample::land(
            RgbColor::of(92, 126, 72),
            SurfaceMaterialSample::UNKNOWN,
            10,
            0,
            4,
            0,
            10,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Temperate broadleaf forest",
            "minecraft:forest",
            0.60,
        );
        let (forest_like_longitude, forest_like_latitude) = (-180..=180)
            .flat_map(|longitude| (-52..=52).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                surface_material_sahara_score(longitude, latitude) < 0.45
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
            })
            .expect("test fixture should find a forest-like ecoregion coordinate");
        let forest_like_intent = apply_test_surface_material_sample(
            &base_land,
            forest_like_ecoregion,
            420.0,
            f64::from(forest_like_longitude),
            f64::from(forest_like_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            forest_like_intent.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            forest_like_intent.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert_eq!(forest_like_intent.biome_id, "minecraft:forest");
        assert_eq!(forest_like_intent.decision_source, "intent");

        let low_tree_temperate_climate = SurfaceMaterialSample::land(
            RgbColor::of(134, 126, 82),
            8,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "",
            "",
            0.0,
        );
        let (temperate_forest_longitude, temperate_forest_latitude) = (-180..=180)
            .flat_map(|longitude| (-49..=49).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let patch_noise =
                    surface_material_ecology_noise(longitude, latitude, 2.4, 0x4165d9e7a1f31c0b);
                surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
                    && (0.56..0.70).contains(&patch_noise)
            })
            .expect("test fixture should find a low-tree temperate forest coordinate");
        let temperate_climate_forest = apply_test_surface_material_sample(
            &base_land,
            low_tree_temperate_climate,
            380.0,
            f64::from(temperate_forest_longitude),
            f64::from(temperate_forest_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            temperate_climate_forest.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            temperate_climate_forest.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert_eq!(temperate_climate_forest.biome_id, "minecraft:forest");
        assert_eq!(temperate_climate_forest.decision_source, "intent");

        let moss_temperate_forest = SurfaceMaterialSample::land(
            RgbColor::of(54, 96, 48),
            8,
            22,
            0,
            0,
            0,
            22,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Temperate mixed forest",
            "minecraft:forest",
            0.60,
        );
        let (moss_longitude, moss_latitude) = (-180..=180)
            .flat_map(|longitude| (-49..=49).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let patch_noise =
                    surface_material_ecology_noise(longitude, latitude, 2.4, 0x4165d9e7a1f31c0b);
                let fine_noise = surface_material_ecology_noise(
                    longitude,
                    latitude,
                    7.5,
                    0x9d6c63b5a8e33f21_u64 as i64,
                );
                surface_material_sahara_score(longitude, latitude) < 0.45
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
                    && patch_noise >= 0.74
                    && fine_noise >= 0.34
            })
            .expect("test fixture should find a moss temperate forest coordinate");
        let moss_forest = apply_test_surface_material_sample(
            &base_land,
            moss_temperate_forest,
            420.0,
            f64::from(moss_longitude),
            f64::from(moss_latitude),
            0.0,
            0.0,
        );
        assert_eq!(moss_forest.top_block_state_id, block_state_ids::MOSS_BLOCK);
        assert_eq!(moss_forest.filler_block_state_id, block_state_ids::DIRT);
        assert_eq!(moss_forest.decision_source, "intent");

        let podzol_temperate_forest = SurfaceMaterialSample::land(
            RgbColor::of(92, 126, 72),
            8,
            10,
            0,
            0,
            0,
            10,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Temperate mixed forest",
            "minecraft:forest",
            0.60,
        );
        let (podzol_longitude, podzol_latitude) = (-180..=180)
            .flat_map(|longitude| (-49..=49).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let patch_noise =
                    surface_material_ecology_noise(longitude, latitude, 2.4, 0x4165d9e7a1f31c0b);
                let fine_noise = surface_material_ecology_noise(
                    longitude,
                    latitude,
                    7.5,
                    0x9d6c63b5a8e33f21_u64 as i64,
                );
                surface_material_sahara_score(longitude, latitude) < 0.45
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
                    && patch_noise <= 0.24
                    && fine_noise >= 0.58
            })
            .expect("test fixture should find a podzol temperate forest coordinate");
        let podzol_forest = apply_test_surface_material_sample(
            &base_land,
            podzol_temperate_forest,
            420.0,
            f64::from(podzol_longitude),
            f64::from(podzol_latitude),
            0.0,
            0.0,
        );
        assert_eq!(podzol_forest.top_block_state_id, block_state_ids::PODZOL);
        assert_eq!(podzol_forest.filler_block_state_id, block_state_ids::DIRT);
        assert_eq!(podzol_forest.decision_source, "intent");

        let savanna_like_highland_candidate = SurfaceMaterialSample::land(
            RgbColor::of(181, 96, 46),
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            0,
            0,
            20,
            6,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Tropical savanna",
            "minecraft:savanna",
            0.82,
        );
        let savanna_like_highland = apply_test_surface_material_sample(
            &base_land,
            savanna_like_highland_candidate,
            1_200.0,
            f64::from(highland_longitude),
            f64::from(highland_latitude),
            0.0,
            260.0,
        );
        assert_ne!(
            savanna_like_highland.top_block_state_id,
            block_state_ids::ORANGE_TERRACOTTA
        );
        assert!(!savanna_like_highland.biome_id.contains("badlands"));
        assert_eq!(savanna_like_highland.decision_source, "intent");

        let high_latitude_savanna_like = SurfaceMaterialSample::land(
            RgbColor::of(134, 126, 82),
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            10,
            0,
            10,
            8,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Eurasian steppe grassland",
            "minecraft:plains",
            0.62,
        );
        let (temperate_savanna_longitude, temperate_savanna_latitude) = (-180..=180)
            .flat_map(|longitude| (-55..=-33).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.25
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
            })
            .expect("test fixture should find a temperate savanna-like coordinate");
        let temperate_savanna_like = apply_test_surface_material_sample(
            &base_land,
            high_latitude_savanna_like,
            360.0,
            f64::from(temperate_savanna_longitude),
            f64::from(temperate_savanna_latitude),
            0.0,
            0.0,
        );
        assert_eq!(
            temperate_savanna_like.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            temperate_savanna_like.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert!(!temperate_savanna_like.biome_id.contains("savanna"));
        assert_eq!(temperate_savanna_like.decision_source, "intent");

        let temperate_steppe = SurfaceMaterialSample::land(
            RgbColor::of(134, 126, 82),
            6,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            22,
            10,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            55,
            SurfaceMaterialSample::UNKNOWN,
            "Eurasian steppe",
            "minecraft:savanna",
            1.0,
        );
        let climate_steppe = apply_test_surface_material_sample(
            &base_land,
            temperate_steppe,
            300.0,
            30.0,
            48.0,
            0.0,
            0.0,
        );
        assert_eq!(
            climate_steppe.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert!(!climate_steppe.biome_id.contains("savanna"));
        assert!(matches!(
            climate_steppe.biome_id.as_str(),
            "minecraft:plains"
                | "minecraft:forest"
                | "minecraft:taiga"
                | "minecraft:sunflower_plains"
                | "minecraft:meadow"
        ));
        assert_eq!(climate_steppe.decision_source, "intent");

        let desert_climate = SurfaceMaterialSample::climate_only(
            RgbColor::of(240, 213, 126),
            4,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
        );
        let climate_desert = apply_test_surface_material_sample(
            &base_land,
            desert_climate,
            250.0,
            13.0,
            24.0,
            0.0,
            0.0,
        );
        assert_eq!(climate_desert.top_block_state_id, block_state_ids::SAND);
        assert_eq!(climate_desert.biome_id, "minecraft:desert");

        let desert_edge_vegetation = SurfaceMaterialSample::land(
            RgbColor::of(126, 123, 70),
            4,
            12,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            18,
            4,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Sahara desert",
            "minecraft:desert",
            0.82,
        );
        let (edge_longitude, edge_latitude) = (-20..=35)
            .flat_map(|longitude| (6..=19).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let patch_noise =
                    surface_material_ecology_noise(longitude, latitude, 2.4, 0x4165d9e7a1f31c0b);
                let fine_noise = surface_material_ecology_noise(
                    longitude,
                    latitude,
                    7.5,
                    0x9d6c63b5a8e33f21_u64 as i64,
                );
                let dry_score = surface_material_sahel_score(longitude, latitude)
                    .max(surface_material_dry_savanna_score(longitude, latitude))
                    .max(0.58);
                dry_score < 0.62
                    && matches!(
                        dry_grass_biome(dry_score, 250.0, patch_noise, fine_noise).as_str(),
                        "minecraft:plains" | "minecraft:sunflower_plains"
                    )
            })
            .expect("test fixture should find a low-patch desert-edge coordinate");
        let desert_edge = apply_test_surface_material_sample(
            &base_land,
            desert_edge_vegetation,
            250.0,
            f64::from(edge_longitude),
            f64::from(edge_latitude),
            0.0,
            0.0,
        );
        assert_eq!(desert_edge.biome_id, "minecraft:savanna");
        assert_eq!(desert_edge.decision_source, "intent");
    }

    #[test]
    fn surface_material_classifier_environment_matches_java_bootstrap_cases() {
        let base_land = surface_column(
            false,
            SEA_LEVEL_Y + 10,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );

        let herbaceous_orange_patch = SurfaceMaterialSample::land(
            RgbColor::of(150, 70, 45),
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            0,
            0,
            35,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "",
            "",
            0.0,
        );
        let (orange_longitude, orange_latitude) = (-180..=180)
            .flat_map(|longitude| {
                (36..=55)
                    .chain(-55..=-36)
                    .map(move |latitude| (longitude, latitude))
            })
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                surface_material_sahara_score(longitude, latitude) < 0.25
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.20
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
            })
            .expect("test fixture should find a low-intent herbaceous orange coordinate");
        let herbaceous_environment = apply_test_surface_material_sample(
            &base_land,
            herbaceous_orange_patch,
            420.0,
            f64::from(orange_longitude),
            f64::from(orange_latitude),
            0.0,
            0.0,
        );
        assert!(matches!(
            herbaceous_environment.top_block_state_id,
            block_state_ids::GRASS_BLOCK | block_state_ids::COARSE_DIRT
        ));
        assert_eq!(
            herbaceous_environment.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert!(!herbaceous_environment.biome_id.contains("badlands"));
        assert_eq!(herbaceous_environment.decision_source, "environment");
    }

    #[test]
    fn surface_material_classifier_ecoregion_fallback_matches_java_bootstrap_cases() {
        let base_highland = surface_column(
            false,
            160,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let jagged_peaks = SurfaceMaterialSample::land(
            RgbColor::of(96, 96, 92),
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Alpine peaks",
            "minecraft:jagged_peaks",
            0.90,
        );
        let peaks = apply_test_surface_material_sample(
            &base_highland,
            jagged_peaks,
            1_200.0,
            78.0,
            4.0,
            0.0,
            0.0,
        );
        assert_eq!(peaks.top_block_state_id, block_state_ids::STONE);
        assert_eq!(peaks.filler_block_state_id, block_state_ids::STONE);
        assert_eq!(peaks.biome_id, "minecraft:jagged_peaks");
        assert_eq!(peaks.decision_source, "ecoregion");

        let coastal_lowland = surface_column(
            false,
            SEA_LEVEL_Y + 2,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let weak_mangrove_swamp = SurfaceMaterialSample::land(
            RgbColor::of(96, 82, 62),
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            5,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Coastal mangrove swamp",
            "minecraft:mangrove_swamp",
            0.88,
        );
        let (swamp_longitude, swamp_latitude) = (-180..=180)
            .flat_map(|longitude| (-45..=45).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                surface_material_sahara_score(longitude, latitude) < 0.45
                    && surface_material_sahel_score(longitude, latitude) < 0.18
                    && surface_material_dry_savanna_score(longitude, latitude) < 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
            })
            .expect("test fixture should find a low-intent swamp ecoregion coordinate");
        let swamp = apply_test_surface_material_sample(
            &coastal_lowland,
            weak_mangrove_swamp,
            20.0,
            f64::from(swamp_longitude),
            f64::from(swamp_latitude),
            0.0,
            0.0,
        );
        assert!(matches!(
            swamp.top_block_state_id,
            block_state_ids::GRASS_BLOCK | block_state_ids::MUD
        ));
        assert_eq!(swamp.biome_id, "minecraft:mangrove_swamp");
        assert_eq!(swamp.decision_source, "ecoregion");
    }

    #[test]
    fn surface_material_classifier_color_only_water_matches_java_bootstrap_cases() {
        let deep_ocean = surface_column(
            true,
            SEA_LEVEL_Y - 35,
            SEA_LEVEL_Y,
            block_state_ids::GRAVEL,
            block_state_ids::GRAVEL,
            "minecraft:ocean",
        );
        let dark = apply_test_surface_material(
            &deep_ocean,
            RgbColor::of(20, 55, 85),
            -120.0,
            -42.0,
            35.0,
            0.0,
            0.0,
        );
        assert!(matches!(
            dark.top_block_state_id,
            block_state_ids::DEEPSLATE | block_state_ids::BLACK_TERRACOTTA
        ));
        assert_eq!(dark.filler_block_state_id, dark.top_block_state_id);

        let shelf_ocean = surface_column(
            true,
            SEA_LEVEL_Y - 5,
            SEA_LEVEL_Y,
            block_state_ids::GRAVEL,
            block_state_ids::GRAVEL,
            "minecraft:ocean",
        );
        let shelf = apply_test_surface_material(
            &shelf_ocean,
            RgbColor::of(50, 95, 120),
            -18.0,
            -60.0,
            4.0,
            0.75,
            0.0,
        );
        assert!(matches!(
            shelf.top_block_state_id,
            block_state_ids::GRAVEL | block_state_ids::CLAY
        ));
        assert_ne!(shelf.top_block_state_id, block_state_ids::SAND);

        let bright_deep = apply_test_surface_material(
            &deep_ocean,
            RgbColor::of(65, 118, 155),
            -120.0,
            -42.0,
            35.0,
            0.0,
            0.0,
        );
        assert_eq!(bright_deep.top_block_state_id, block_state_ids::GRAVEL);

        let missing = apply_test_surface_material(
            &deep_ocean,
            RgbColor::unavailable(),
            -120.0,
            -42.0,
            35.0,
            0.0,
            0.0,
        );
        assert_eq!(missing.top_block_state_id, block_state_ids::GRAVEL);
        assert_eq!(missing.decision_source, "water");
    }

    #[test]
    fn bathymetry_shelf_adjustment_uses_surface_scale_and_coastal_ramp() {
        assert_eq!(
            coastal_bathymetry_shelf_adjusted_depth_blocks(123, 0.0, DEFAULT_VERTICAL_SCALE),
            123
        );
        let mid_coast_depth =
            coastal_bathymetry_shelf_adjusted_depth_blocks(123, 0.75, DEFAULT_VERTICAL_SCALE);
        let near_coast_depth =
            coastal_bathymetry_shelf_adjusted_depth_blocks(123, 0.890625, DEFAULT_VERTICAL_SCALE);
        let immediate_depth =
            coastal_bathymetry_shelf_adjusted_depth_blocks(123, 1.0, DEFAULT_VERTICAL_SCALE);
        assert!(mid_coast_depth > near_coast_depth);
        assert!(near_coast_depth > immediate_depth);
        assert!(immediate_depth <= 3);
        assert_eq!(
            coastal_shelf_adjusted_depth_blocks(123, 0.890625, DEFAULT_VERTICAL_SCALE),
            121
        );

        let base = surface_column(
            true,
            29,
            SEA_LEVEL_Y,
            block_state_ids::GRAVEL,
            block_state_ids::GRAVEL,
            "minecraft:deep_lukewarm_ocean",
        );
        let sample = SurfaceMaterialSample::new(
            RgbColor::of(1, 1, 21),
            RgbColor::of(0, 0, 0),
            TerrainTokenSource::JavaStandardPalette,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            54_790,
            -3_018,
            SurfaceMaterialSample::UNKNOWN,
            "",
            "",
            0.0,
        );
        let applied = apply_test_surface_material_sample(
            &base,
            sample,
            0.0,
            5.883967560823464,
            -0.02239432817833631,
            0.890625,
            0.0,
        );

        assert!(
            (SEA_LEVEL_Y - 8..SEA_LEVEL_Y).contains(&applied.ground_surface_y),
            "coastal bathymetry should ramp instead of cutting to the world floor, got {}",
            applied.ground_surface_y
        );
        assert_eq!(applied.decision_source, "water");
    }

    #[test]
    fn source_land_override_uses_coastal_shape_to_avoid_cliff_edges() {
        let base = surface_column(
            true,
            SEA_LEVEL_Y - 1,
            SEA_LEVEL_Y,
            block_state_ids::GRAVEL,
            block_state_ids::GRAVEL,
            "minecraft:warm_ocean",
        );
        let dry_land_sample = SurfaceMaterialSample::new(
            RgbColor::of(210, 188, 132),
            RgbColor::unavailable(),
            TerrainTokenSource::None,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            46_380,
            0,
            SurfaceMaterialSample::UNKNOWN,
            "Great Barrier Reef coastal land",
            "minecraft:beach",
            0.45,
        );

        let restored = apply_surface_material(
            &base,
            &dry_land_sample,
            900.0,
            146.0,
            -18.5,
            1.0,
            0.0,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();

        assert!(!restored.water);
        assert_eq!(restored.decision_source, "source-land-override");
        assert!(
            restored.ground_surface_y <= SEA_LEVEL_Y + 2,
            "source-land restored coast should stay beach-height, got {}",
            restored.ground_surface_y
        );
    }

    #[test]
    fn bathymetry_near_shore_follows_surface_scale_without_abyss_cut() {
        let shelf_water = surface_column(
            true,
            SEA_LEVEL_Y - 5,
            SEA_LEVEL_Y,
            block_state_ids::GRAVEL,
            block_state_ids::GRAVEL,
            "minecraft:deep_lukewarm_ocean",
        );
        let transition_bathymetry = SurfaceMaterialSample::new(
            RgbColor::of(0, 0, 18),
            RgbColor::unavailable(),
            TerrainTokenSource::None,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            39_000,
            -1271,
            0,
            "",
            "",
            0.0,
        );

        let open_water = apply_surface_material(
            &shelf_water,
            &transition_bathymetry,
            0.0,
            127.0,
            34.0,
            0.0,
            0.0,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();
        let mid_shelf = apply_surface_material(
            &shelf_water,
            &transition_bathymetry,
            0.0,
            127.0,
            34.0,
            0.75,
            0.0,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();
        let near_shore_shelf = apply_surface_material(
            &shelf_water,
            &transition_bathymetry,
            0.0,
            127.0,
            34.0,
            0.984375,
            0.0,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();
        let immediate_shore_shelf = apply_surface_material(
            &shelf_water,
            &transition_bathymetry,
            0.0,
            127.0,
            34.0,
            1.0,
            0.0,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();

        assert!(open_water.ground_surface_y > MIN_SURFACE_Y);
        assert!(open_water.ground_surface_y <= SEA_LEVEL_Y - 20);
        assert!(mid_shelf.ground_surface_y > open_water.ground_surface_y);
        assert!(mid_shelf.ground_surface_y <= SEA_LEVEL_Y - 8);
        assert!(
            near_shore_shelf.ground_surface_y >= SEA_LEVEL_Y - 4,
            "near-shore bathymetry should stay in the coastal ramp, got {}",
            near_shore_shelf.ground_surface_y
        );
        assert!(immediate_shore_shelf.ground_surface_y >= near_shore_shelf.ground_surface_y);

        let cleaned_immediate = clean_single_coastal_column(immediate_shore_shelf, 1.0);
        assert_eq!(cleaned_immediate.top_block_state_id, block_state_ids::CLAY);
        assert_eq!(
            cleaned_immediate.filler_block_state_id,
            block_state_ids::CLAY
        );

        let conflicting_coastal_land = surface_column(
            false,
            SEA_LEVEL_Y,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:savanna",
        );
        let restored_water = apply_surface_material(
            &conflicting_coastal_land,
            &transition_bathymetry,
            1.0,
            121.39221556886224,
            -10.915433956397123,
            0.984375,
            149.25274820796724,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();
        assert!(restored_water.water);
        assert!(
            restored_water.ground_surface_y >= SEA_LEVEL_Y - 4,
            "bathymetry-land override at the coast should not excavate to {}, got {}",
            MIN_SURFACE_Y,
            restored_water.ground_surface_y
        );
    }

    #[test]
    fn surface_region_settings_default_to_java_photo_texture_mode() {
        let settings = SurfaceRegionSettings::new(
            "height.tif",
            "world",
            "SR EarthMap Surface",
            0,
            5000,
            0,
            0,
            OutputFormat::LinearV2,
            DEFAULT_HEIGHT_ONLY_CACHE_ROWS,
        )
        .unwrap();

        assert_eq!(settings.texture_mode, SurfaceTextureMode::Photo);
        assert!(settings.surface_material_path.is_none());
    }

    #[test]
    fn surface_region_settings_can_preserve_classified_texture_mode_for_manifest() {
        let world_dir = std::env::temp_dir().join(format!(
            "earthmap-surface-manifest-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&world_dir);
        let settings = SurfaceRegionSettings::new_with_texture_options(
            "height.tif",
            &world_dir,
            "SR EarthMap Surface",
            0,
            5000,
            -1,
            2,
            OutputFormat::Mca,
            DEFAULT_HEIGHT_ONLY_CACHE_ROWS,
            true,
            ChunkGenerationStatus::Surface,
            DEFAULT_VERTICAL_SCALE,
            SurfaceTextureMode::Classified,
        )
        .unwrap();

        let manifest_path = write_surface_region_manifest(&settings).unwrap();
        let manifest = std::fs::read_to_string(manifest_path).unwrap();

        assert!(manifest.contains("generation.textureMode=classified\n"));
        assert!(manifest.contains("features.surfaceMaterialRaster=false\n"));
        assert!(manifest.contains("generation.chunkStatus=minecraft:surface\n"));

        fs::remove_dir_all(world_dir).unwrap();
    }

    #[test]
    fn surface_region_manifest_records_configured_material_raster() {
        let world_dir = std::env::temp_dir().join(format!(
            "earthmap-surface-material-manifest-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&world_dir);
        let mut settings = SurfaceRegionSettings::new(
            "height.tif",
            &world_dir,
            "SR EarthMap Surface",
            0,
            5000,
            0,
            0,
            OutputFormat::LinearV2,
            DEFAULT_HEIGHT_ONLY_CACHE_ROWS,
        )
        .unwrap();
        settings.surface_material_path = Some(PathBuf::from("TifFiles/terrain/TrueMarble.vrt"));

        let manifest_path = write_surface_region_manifest(&settings).unwrap();
        let manifest = std::fs::read_to_string(manifest_path).unwrap();

        assert!(manifest.contains("features.surfaceMaterialRaster=true\n"));
        assert!(manifest.contains("generation.textureMode=photo\n"));

        fs::remove_dir_all(world_dir).unwrap();
    }

    #[test]
    fn surface_region_photo_material_sample_applies_photo_solver_metadata() {
        let semantic = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let material = SurfaceMaterialSample::with_export_token(
            RgbColor::of(218, 184, 92),
            RgbColor::of(255, 200, 128),
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "",
            "",
            0.0,
        );

        let applied = apply_surface_region_material_sample(
            semantic,
            material,
            SurfaceTextureMode::Photo,
            240.0,
            13.0,
            24.0,
            0.0,
            15.0,
            101,
            100,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();

        assert_eq!(applied.terrain_token_source, TerrainTokenSource::Export);
        assert!(applied.has_data_evidence(surface_data_evidence::CLIMATE));
        assert!(matches!(
            applied.top_block_state_id,
            block_state_ids::SAND
                | block_state_ids::SANDSTONE
                | block_state_ids::END_STONE
                | block_state_ids::SMOOTH_SANDSTONE
                | block_state_ids::CUT_SANDSTONE
                | block_state_ids::CHISELED_SANDSTONE
                | block_state_ids::YELLOW_TERRACOTTA
        ));
    }

    #[test]
    fn surface_region_photo_material_helpers_split_java_two_pass_flow() {
        let semantic = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let material = SurfaceMaterialSample::with_export_token(
            RgbColor::of(218, 184, 92),
            RgbColor::of(255, 200, 128),
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "",
            "",
            0.0,
        );

        let semantic_only = apply_surface_region_semantic_material_sample(
            semantic.clone(),
            &material,
            240.0,
            13.0,
            24.0,
            0.0,
            15.0,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();
        let photo_pass = apply_surface_region_photo_material_sample(
            semantic_only.clone(),
            material.clone(),
            240.0,
            13.0,
            24.0,
            0.0,
            15.0,
            101,
            100,
            DEFAULT_VERTICAL_SCALE,
            None,
        )
        .unwrap();
        let combined = apply_surface_region_material_sample(
            semantic,
            material,
            SurfaceTextureMode::Photo,
            240.0,
            13.0,
            24.0,
            0.0,
            15.0,
            101,
            100,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();

        assert_eq!(photo_pass, combined);
        assert_ne!(semantic_only, combined);
    }

    #[test]
    fn surface_region_photo_material_uses_smoothed_elevation_input() {
        let semantic = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let material = SurfaceMaterialSample::new(
            RgbColor::of(156, 149, 137),
            RgbColor::of(230, 205, 160),
            TerrainTokenSource::JavaStandardPalette,
            4,
            0,
            0,
            0,
            0,
            0,
            0,
            -1,
            -1,
            28,
            -1,
            150,
            "rocky desert plateau",
            "minecraft:desert",
            0.95,
        );

        let low_elevation = apply_surface_region_material_sample(
            semantic.clone(),
            material.clone(),
            SurfaceTextureMode::Photo,
            650.0,
            34.0,
            22.0,
            0.0,
            0.0,
            512,
            512,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();
        let smoothed_elevation = apply_surface_region_material_sample(
            semantic,
            material,
            SurfaceTextureMode::Photo,
            900.0,
            34.0,
            22.0,
            0.0,
            0.0,
            512,
            512,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();

        assert_ne!(
            low_elevation.top_block_state_id,
            smoothed_elevation.top_block_state_id
        );
        assert_ne!(smoothed_elevation.top_block_state_id, block_state_ids::SAND);
        assert_ne!(
            smoothed_elevation.top_block_state_id,
            block_state_ids::SANDSTONE
        );
    }

    #[test]
    fn classified_surface_material_sampling_applies_semantic_material() {
        let semantic = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let classified = apply_surface_region_material_sample(
            semantic,
            SurfaceMaterialSample::color_only(RgbColor::of(230, 205, 160)),
            SurfaceTextureMode::Classified,
            120.0,
            20.0,
            25.0,
            0.0,
            0.0,
            0,
            0,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap();

        assert_eq!(classified.top_block_state_id, block_state_ids::SAND);
        assert_eq!(classified.filler_block_state_id, block_state_ids::SAND);
        assert_eq!(classified.biome_id, "minecraft:desert");
        assert_eq!(classified.decision_source, "intent");
    }

    #[test]
    fn classified_surface_material_scales_local_relief_like_java() {
        let semantic = surface_column(
            false,
            SEA_LEVEL_Y + 72,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let material = SurfaceMaterialSample::color_only(RgbColor::of(181, 96, 46));
        let metrics = SurfaceColorMetrics::from(material.color);
        let (longitude, latitude) = (-180..=180)
            .flat_map(|longitude| (-45..=45).map(move |latitude| (longitude, latitude)))
            .find(|&(longitude, latitude)| {
                let longitude = f64::from(longitude);
                let latitude = f64::from(latitude);
                let dry_savanna = surface_material_dry_savanna_score(longitude, latitude);
                surface_material_sahel_score(longitude, latitude) < 0.18
                    && dry_savanna < 0.35
                    && surface_material_mediterranean_score(longitude, latitude) < 0.40
                    && surface_material_rainforest_score(longitude, latitude) < 0.35
                    && is_exposed_dry_rock(
                        metrics,
                        650.0,
                        longitude,
                        latitude,
                        surface_material_sahara_score(longitude, latitude),
                        dry_savanna,
                        240.0,
                        0.0,
                    )
                    && !is_exposed_dry_rock(
                        metrics,
                        650.0,
                        longitude,
                        latitude,
                        surface_material_sahara_score(longitude, latitude),
                        dry_savanna,
                        120.0,
                        0.0,
                    )
            })
            .expect("test fixture should find a vertical-scale relief highland coordinate");
        let classified = apply_surface_region_material_sample(
            semantic,
            material,
            SurfaceTextureMode::Classified,
            650.0,
            f64::from(longitude),
            f64::from(latitude),
            0.0,
            120.0,
            0,
            0,
            2.0,
        )
        .unwrap();

        assert_eq!(
            classified.top_block_state_id,
            block_state_ids::ORANGE_TERRACOTTA
        );
        assert_eq!(
            classified.filler_block_state_id,
            block_state_ids::ORANGE_TERRACOTTA
        );
        assert!(matches!(
            classified.biome_id.as_str(),
            "minecraft:badlands" | "minecraft:wooded_badlands"
        ));
        assert_eq!(classified.decision_source, "intent");
    }

    #[test]
    fn generate_surface_region_no_longer_rejects_classified_material_before_opening_inputs() {
        let temp = std::env::temp_dir().join(format!(
            "earthmap-classified-material-error-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut settings = SurfaceRegionSettings::new_with_texture_options(
            temp.join("missing-height.tif"),
            temp.join("world"),
            "SR EarthMap Surface",
            0,
            5000,
            0,
            0,
            OutputFormat::LinearV2,
            DEFAULT_HEIGHT_ONLY_CACHE_ROWS,
            true,
            ChunkGenerationStatus::Full,
            DEFAULT_VERTICAL_SCALE,
            SurfaceTextureMode::Classified,
        )
        .unwrap();
        settings.surface_material_path = Some(temp.join("missing-TrueMarble.vrt"));

        let error = generate_surface_region(&settings).unwrap_err();

        assert!(!error
            .to_string()
            .contains("classified surface material sampling is not ported yet"));
        assert!(temp.join("world").exists());
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn surface_region_post_processing_follows_texture_mode() {
        let mut patch = filled_surface_columns(
            5,
            5,
            land_surface_column(
                block_state_ids::GRASS_BLOCK,
                "minecraft:forest",
                SEA_LEVEL_Y + 4,
            ),
        );
        patch[surface_class_index(2, 2, 5)] = land_surface_column(
            block_state_ids::RED_SAND,
            "minecraft:desert",
            SEA_LEVEL_Y + 4,
        );
        let coast_factors = vec![0.0; patch.len()];

        let classified = post_process_surface_region_columns(
            &patch,
            &coast_factors,
            5,
            SurfaceTextureMode::Classified,
        )
        .unwrap();
        let photo = post_process_surface_region_columns(
            &patch,
            &coast_factors,
            5,
            SurfaceTextureMode::Photo,
        )
        .unwrap();

        let center = surface_class_index(2, 2, 5);
        assert_eq!(
            classified[center].top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(classified[center].decision_source, "intent-stabilized-cell");
        assert_eq!(
            photo[center].top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_ne!(
            classified[center].decision_source,
            photo[center].decision_source
        );
    }

    #[test]
    fn earth_surface_rules_match_java_contract_cases() {
        let ocean = classify_surface(0.0, -150.0, 0.0);
        assert!(ocean.water);
        assert!(ocean.ground_surface_y < SEA_LEVEL_Y);
        assert_eq!(ocean.water_surface_y, SEA_LEVEL_Y);
        assert_eq!(ocean.top_block_state_id, block_state_ids::GRAVEL);
        assert!(ocean.biome_id.ends_with("ocean"));

        let second_ocean = classify_surface(0.0, 20.0, -35.0);
        assert!(second_ocean.ground_surface_y < SEA_LEVEL_Y);
        assert_ne!(ocean.ground_surface_y, second_ocean.ground_surface_y);
        assert_eq!(
            ocean.ground_surface_y,
            classify_surface(0.0, -150.0, 0.0).ground_surface_y
        );
        let shallow_sea = classify_surface(-35.0, 10.0, 45.0);
        assert!(shallow_sea.ground_surface_y < SEA_LEVEL_Y);

        let sahara = classify_surface(350.0, 13.0, 23.0);
        assert!(!sahara.water);
        assert_eq!(sahara.top_block_state_id, block_state_ids::SAND);
        assert_eq!(sahara.biome_id, "minecraft:desert");

        let everest = classify_surface(8765.0, 86.925, 27.9881);
        assert!(!everest.water);
        assert_eq!(everest.ground_surface_y, 313);
        assert!(
            everest.top_block_state_id == block_state_ids::SNOW_BLOCK
                || everest.top_block_state_id == block_state_ids::STONE
        );

        let amazon = classify_surface(120.0, -60.0, -3.0);
        assert_eq!(amazon.biome_id, "minecraft:jungle");

        let coastal_land = classify_shaped_surface(900.0, 0.0, 40.0, false, 1.0).unwrap();
        assert!(coastal_land.ground_surface_y <= SEA_LEVEL_Y + 2);
        assert_ne!(coastal_land.top_block_state_id, block_state_ids::SAND);

        let desert_coast = classify_shaped_surface(120.0, 13.0, 23.0, false, 1.0).unwrap();
        assert_eq!(desert_coast.top_block_state_id, block_state_ids::SAND);

        let coastal_slope = classify_shaped_surface(1800.0, 0.0, 40.0, false, 0.75).unwrap();
        let inland_mountain = classify_shaped_surface(1800.0, 0.0, 40.0, false, 0.0).unwrap();
        let taller_inland_mountain =
            classify_shaped_surface_scaled(1800.0, 0.0, 40.0, false, 0.0, 1.35).unwrap();
        assert!(coastal_slope.ground_surface_y > SEA_LEVEL_Y + 20);
        assert!(inland_mountain.ground_surface_y >= coastal_slope.ground_surface_y);
        assert!(taller_inland_mountain.ground_surface_y > inland_mountain.ground_surface_y);
        assert_ne!(coastal_slope.top_block_state_id, block_state_ids::SAND);
        let mid_coast_slope = classify_shaped_surface(1800.0, 0.0, 40.0, false, 0.50).unwrap();
        assert!(mid_coast_slope.ground_surface_y >= coastal_slope.ground_surface_y);

        let coastal_water = classify_shaped_surface(-2000.0, 0.0, 0.0, true, 1.0).unwrap();
        assert!(coastal_water.ground_surface_y <= SEA_LEVEL_Y - 2);
        assert_eq!(coastal_water.biome_id, "minecraft:warm_ocean");

        let shelf_water = classify_shaped_surface(-4500.0, 0.0, 0.0, true, 0.75).unwrap();
        assert!(shelf_water.ground_surface_y <= SEA_LEVEL_Y - 16);

        let deep_water = classify_shaped_surface(-4500.0, 0.0, 5.0, true, 0.0).unwrap();
        let mid_depth_water = classify_shaped_surface(-1200.0, 0.0, 5.0, true, 0.0).unwrap();
        let deeper_scaled_water =
            classify_shaped_surface_scaled(-1200.0, 0.0, 5.0, true, 0.0, 1.35).unwrap();
        assert_eq!(deep_water.biome_id, "minecraft:deep_lukewarm_ocean");
        assert!(deeper_scaled_water.ground_surface_y < mid_depth_water.ground_surface_y);

        let impossible_water = EarthSurfaceColumn::new(
            true,
            SEA_LEVEL_Y + 5,
            SEA_LEVEL_Y,
            block_state_ids::GRAVEL,
            block_state_ids::GRAVEL,
            "minecraft:ocean",
            "test",
        );
        let normalized_water = normalize_surface_column_for_chunk(&impossible_water);
        assert!(normalized_water.water);
        assert_eq!(normalized_water.ground_surface_y, SEA_LEVEL_Y - 1);
        assert_eq!(normalized_water.water_surface_y, SEA_LEVEL_Y);

        let korea = classify_shaped_surface(320.0, 126.9, 36.0, false, 0.0).unwrap();
        assert_eq!(korea.biome_id, "minecraft:forest");
    }

    #[test]
    fn natural_surface_block_policy_preserves_photo_palette_carriers() {
        for block in [
            block_state_ids::OAK_LEAVES,
            block_state_ids::JUNGLE_LEAVES,
            block_state_ids::DARK_OAK_LEAVES,
            block_state_ids::SPRUCE_LEAVES,
            block_state_ids::BLACK_TERRACOTTA,
            block_state_ids::GREEN_TERRACOTTA,
            block_state_ids::CYAN_TERRACOTTA,
            block_state_ids::LIME_TERRACOTTA,
            block_state_ids::BLACK_CONCRETE,
            block_state_ids::QUARTZ_BLOCK,
            block_state_ids::BONE_BLOCK,
            block_state_ids::END_STONE,
            block_state_ids::END_STONE_BRICKS,
            block_state_ids::MUD_BRICKS,
        ] {
            assert!(!is_allowed_production_top(block, "minecraft:plains"));
            let replacement = production_surface_top(block, "minecraft:plains");
            assert!(
                is_allowed_production_top(replacement, "minecraft:plains"),
                "palette block replacement is natural: {block} -> {replacement}"
            );
        }

        for block in [
            block_state_ids::TERRACOTTA,
            block_state_ids::ORANGE_TERRACOTTA,
            block_state_ids::BROWN_TERRACOTTA,
            block_state_ids::RED_TERRACOTTA,
            block_state_ids::YELLOW_TERRACOTTA,
            block_state_ids::WHITE_TERRACOTTA,
            block_state_ids::LIGHT_GRAY_TERRACOTTA,
            block_state_ids::GRAY_TERRACOTTA,
            block_state_ids::SMOOTH_SANDSTONE,
            block_state_ids::CUT_SANDSTONE,
            block_state_ids::CHISELED_SANDSTONE,
            block_state_ids::SMOOTH_RED_SANDSTONE,
            block_state_ids::CUT_RED_SANDSTONE,
            block_state_ids::CHISELED_RED_SANDSTONE,
            block_state_ids::PACKED_MUD,
            block_state_ids::DRIPSTONE_BLOCK,
        ] {
            assert!(is_allowed_production_top(block, "minecraft:plains"));
            assert_eq!(
                production_surface_top(block, "minecraft:plains"),
                block,
                "natural photo palette block is preserved: {block}"
            );
        }

        assert_eq!(
            production_surface_top(block_state_ids::BLACK_TERRACOTTA, "minecraft:beach"),
            block_state_ids::SAND
        );
        assert_eq!(
            production_surface_top(block_state_ids::BROWN_TERRACOTTA, "minecraft:beach"),
            block_state_ids::SAND
        );
        assert_eq!(
            production_surface_top(block_state_ids::DARK_OAK_LEAVES, "minecraft:dark_forest"),
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            production_surface_top(block_state_ids::OAK_LEAVES, "minecraft:savanna"),
            block_state_ids::COARSE_DIRT
        );

        let rocky_korea = surface_column(
            false,
            SEA_LEVEL_Y + 34,
            i32::MIN,
            block_state_ids::ANDESITE,
            block_state_ids::STONE,
            "minecraft:windswept_hills",
        );
        let cleaned = sanitize_surface_column_for_production(&rocky_korea);
        assert_eq!(cleaned.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::DIRT);

        let alpine = surface_column(
            false,
            SEA_LEVEL_Y + 90,
            i32::MIN,
            block_state_ids::STONE,
            block_state_ids::STONE,
            "minecraft:stony_peaks",
        );
        let cleaned = sanitize_surface_column_for_production(&alpine);
        assert_eq!(cleaned.top_block_state_id, block_state_ids::STONE);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::STONE);

        let dark_forest_podzol = surface_column(
            false,
            SEA_LEVEL_Y + 18,
            i32::MIN,
            block_state_ids::PODZOL,
            block_state_ids::PODZOL,
            "minecraft:dark_forest",
        );
        let cleaned = sanitize_surface_column_for_production(&dark_forest_podzol);
        assert_eq!(cleaned.top_block_state_id, block_state_ids::GRASS_BLOCK);

        let wet_mud = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::MUD,
            block_state_ids::MUD,
            "minecraft:swamp",
        );
        let cleaned = sanitize_surface_column_for_production(&wet_mud);
        assert_eq!(cleaned.top_block_state_id, block_state_ids::MOSS_BLOCK);

        let shallow_ocean = surface_column(
            true,
            SEA_LEVEL_Y - 4,
            SEA_LEVEL_Y,
            block_state_ids::GRAVEL,
            block_state_ids::GRAVEL,
            "minecraft:ocean",
        );
        let cleaned = sanitize_surface_column_for_production(&shallow_ocean);
        assert_eq!(cleaned.top_block_state_id, block_state_ids::CLAY);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::CLAY);
    }

    #[test]
    fn photo_natural_surface_carriers_survive_production_cleanup() {
        let inland_granite = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::GRANITE,
            block_state_ids::STONE,
            "minecraft:savanna",
        )
        .with_decision_source("photo-palette")
        .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette);
        let cleaned = sanitize_surface_column_for_production(&inland_granite);
        assert_eq!(cleaned.top_block_state_id, block_state_ids::GRANITE);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::STONE);

        let salt_flat = surface_column(
            false,
            SEA_LEVEL_Y + 1,
            i32::MIN,
            block_state_ids::SMOOTH_SANDSTONE,
            block_state_ids::SMOOTH_SANDSTONE,
            "minecraft:desert",
        )
        .with_decision_source("photo-palette")
        .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette);
        let cleaned = sanitize_surface_column_for_production(&salt_flat);
        assert_eq!(
            cleaned.top_block_state_id,
            block_state_ids::SMOOTH_SANDSTONE
        );
        assert_eq!(
            cleaned.filler_block_state_id,
            block_state_ids::SMOOTH_SANDSTONE
        );

        let red_interior = surface_column(
            false,
            SEA_LEVEL_Y + 18,
            i32::MIN,
            block_state_ids::ORANGE_TERRACOTTA,
            block_state_ids::ORANGE_TERRACOTTA,
            "minecraft:badlands",
        )
        .with_decision_source("photo-palette")
        .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette);
        let cleaned = sanitize_surface_column_for_production(&red_interior);
        assert_eq!(
            cleaned.top_block_state_id,
            block_state_ids::ORANGE_TERRACOTTA
        );
        assert_eq!(
            cleaned.filler_block_state_id,
            block_state_ids::ORANGE_TERRACOTTA
        );

        let non_photo_temperate = surface_column(
            false,
            SEA_LEVEL_Y + 34,
            i32::MIN,
            block_state_ids::ANDESITE,
            block_state_ids::STONE,
            "minecraft:windswept_hills",
        );
        let cleaned = sanitize_surface_column_for_production(&non_photo_temperate);
        assert_eq!(cleaned.top_block_state_id, block_state_ids::GRASS_BLOCK);
    }

    #[test]
    fn coastal_surface_cleaner_matches_java_contract_cases() {
        let cleaned = clean_single_coastal_column(
            land_surface_column(block_state_ids::STONE, "minecraft:beach", SEA_LEVEL_Y + 1),
            0.99,
        );
        assert_eq!(cleaned.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::DIRT);

        let cleaned = clean_single_coastal_column(
            land_surface_column(
                block_state_ids::ANDESITE,
                "minecraft:plains",
                SEA_LEVEL_Y + 2,
            ),
            0.91,
        );
        assert_eq!(cleaned.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::DIRT);

        let cleaned = clean_single_coastal_column(
            land_surface_column(block_state_ids::GRAVEL, "minecraft:plains", SEA_LEVEL_Y + 1),
            0.95,
        );
        assert_eq!(cleaned.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::DIRT);

        let cleaned = clean_single_coastal_column(
            surface_column(
                true,
                SEA_LEVEL_Y - 2,
                SEA_LEVEL_Y,
                block_state_ids::STONE,
                block_state_ids::STONE,
                "minecraft:warm_ocean",
            ),
            0.99,
        );
        assert_eq!(cleaned.top_block_state_id, block_state_ids::CLAY);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::CLAY);

        let cleaned = clean_single_coastal_column(
            surface_column(
                true,
                SEA_LEVEL_Y - 16,
                SEA_LEVEL_Y,
                block_state_ids::STONE,
                block_state_ids::STONE,
                "minecraft:warm_ocean",
            ),
            1.0,
        );
        assert_eq!(cleaned.top_block_state_id, block_state_ids::GRAVEL);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::GRAVEL);

        let cleaned = clean_single_coastal_column(
            surface_column(
                true,
                SEA_LEVEL_Y - 5,
                SEA_LEVEL_Y,
                block_state_ids::GRAVEL,
                block_state_ids::GRAVEL,
                "minecraft:ocean",
            ),
            0.90,
        );
        assert_eq!(cleaned.top_block_state_id, block_state_ids::CLAY);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::CLAY);

        let inland = land_surface_column(
            block_state_ids::STONE,
            "minecraft:stony_peaks",
            SEA_LEVEL_Y + 90,
        );
        assert_eq!(clean_single_coastal_column(inland.clone(), 0.0), inland);

        let cleaned = clean_single_coastal_column(
            land_surface_column(
                block_state_ids::GREEN_TERRACOTTA,
                "minecraft:dark_forest",
                SEA_LEVEL_Y + 20,
            ),
            0.0,
        );
        assert_eq!(cleaned.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert_eq!(cleaned.filler_block_state_id, block_state_ids::DIRT);

        assert!(clean_coastal_surface_columns(&[inland.clone()], &[0.0], 0).is_err());
        assert!(clean_coastal_surface_columns(&[inland], &[], 1).is_err());
    }

    #[test]
    fn osm_region_feature_mask_marks_java_line_fixture_cases() {
        let mut mask = OsmRegionFeatureMask::new();
        mask.mark_line(OsmFeatureKind::Road, 10, 10, 20, 10);
        mask.mark_line(OsmFeatureKind::Waterway, 30, 30, 30, 40);
        mask.mark_line(OsmFeatureKind::Building, 50, 50, 52, 52);
        mask.mark_line(OsmFeatureKind::Landuse, -5, 60, 5, 60);

        assert!(mask.road_at(10, 10).unwrap());
        assert!(mask.road_at(20, 10).unwrap());
        assert!(mask.waterway_at(30, 35).unwrap());
        assert!(mask.building_at(51, 51).unwrap());
        assert!(mask.landuse_at(0, 60).unwrap());
        assert_eq!(mask.road_count(), 11);
        assert_eq!(mask.waterway_count(), 11);
        assert_eq!(mask.building_count(), 3);
        assert_eq!(mask.landuse_count(), 6);
        assert!(mask.flags_at(-1, 0).is_err());
    }

    #[test]
    fn osm_surface_overlay_matches_java_fixture_cases() {
        let mut mask = OsmRegionFeatureMask::new();
        mask.mark(OsmFeatureKind::Road, 10, 10);
        mask.mark(OsmFeatureKind::Building, 20, 20);
        mask.mark(OsmFeatureKind::Waterway, 30, 30);
        mask.mark(OsmFeatureKind::Landuse, 40, 40);

        let mut chunk = ChunkModel::overworld(0, 0);
        let land = surface_column(
            false,
            70,
            SEA_LEVEL_Y,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let desert = surface_column(
            false,
            70,
            SEA_LEVEL_Y,
            block_state_ids::SAND,
            block_state_ids::SAND,
            "minecraft:desert",
        );

        fill_surface_column(&mut chunk, 10, 10, &land).unwrap();
        let road_flags =
            apply_osm_surface_overlay(&mut chunk, 10, 10, &land, Some(&mask), 10, 10).unwrap();
        assert!(osm_overlay_has_road(road_flags));
        assert_eq!(
            chunk.get_block_state_id(10, 70, 10).unwrap(),
            block_state_ids::STONE
        );
        assert_eq!(
            chunk.get_block_state_id(10, 69, 10).unwrap(),
            block_state_ids::STONE
        );

        fill_surface_column(&mut chunk, 4, 4, &land).unwrap();
        let no_flags =
            apply_osm_surface_overlay(&mut chunk, 4, 4, &land, Some(&mask), 4, 4).unwrap();
        assert_eq!(no_flags, 0);
        assert_eq!(
            apply_osm_surface_overlay(&mut chunk, 4, 4, &land, None, 4, 4).unwrap(),
            0
        );
        assert_eq!(
            chunk.get_block_state_id(4, 70, 4).unwrap(),
            block_state_ids::GRASS_BLOCK
        );

        fill_surface_column(&mut chunk, 12, 4, &land).unwrap();
        let building_flags =
            apply_osm_surface_overlay(&mut chunk, 12, 4, &land, Some(&mask), 20, 20).unwrap();
        assert!(osm_overlay_has_building(building_flags));
        assert_eq!(
            chunk.get_block_state_id(12, 70, 4).unwrap(),
            block_state_ids::STONE_BRICKS
        );
        assert_eq!(
            chunk.get_block_state_id(12, 71, 4).unwrap(),
            block_state_ids::STONE_BRICKS
        );

        fill_surface_column(&mut chunk, 14, 14, &land).unwrap();
        let waterway_flags =
            apply_osm_surface_overlay(&mut chunk, 14, 14, &land, Some(&mask), 30, 30).unwrap();
        assert!(osm_overlay_has_waterway(waterway_flags));
        assert_eq!(
            chunk.get_block_state_id(14, 69, 14).unwrap(),
            block_state_ids::SAND
        );
        assert_eq!(
            chunk.get_block_state_id(14, 70, 14).unwrap(),
            block_state_ids::WATER
        );

        fill_surface_column(&mut chunk, 8, 8, &desert).unwrap();
        let landuse_flags =
            apply_osm_surface_overlay(&mut chunk, 8, 8, &desert, Some(&mask), 40, 40).unwrap();
        assert!(osm_overlay_has_landuse(landuse_flags));
        assert_eq!(
            chunk.get_block_state_id(8, 70, 8).unwrap(),
            block_state_ids::GRASS_BLOCK
        );
    }

    #[test]
    fn surface_biome_cell_writer_covers_java_surface_band_fixture() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_biome_id("minecraft:plains").unwrap();
        let (mut biomes, mut min_y, mut max_y) = surface_biome_arrays("minecraft:plains", 64);
        for z in 0..BIOME_CELL_WIDTH {
            for x in 0..BIOME_CELL_WIDTH {
                let index = local_column_index(x, z);
                biomes[index] = "minecraft:jungle".to_string();
                min_y[index] = 60 + x as i32 + z as i32;
                max_y[index] = 88 - x as i32 + z as i32;
            }
        }

        apply_surface_biome_cells(&mut chunk, &biomes, &min_y, &max_y, "minecraft:plains").unwrap();

        assert_eq!(chunk.get_biome_id_at(0, 60, 0).unwrap(), "minecraft:jungle");
        assert_eq!(chunk.get_biome_id_at(0, 88, 0).unwrap(), "minecraft:jungle");
        assert_eq!(chunk.get_biome_id_at(0, 96, 0).unwrap(), "minecraft:jungle");
        assert_eq!(chunk.get_biome_id_at(8, 64, 0).unwrap(), "minecraft:plains");
    }

    #[test]
    fn surface_biome_cell_writer_static_carriers_match_java_fixtures() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_biome_id("minecraft:savanna").unwrap();
        let (mut biomes, min_y, max_y) = surface_biome_arrays("minecraft:savanna", 64);
        fill_surface_top(&mut chunk, 64, block_state_ids::GRASS_BLOCK);
        biomes[local_column_index(0, 0)] = "minecraft:swamp".to_string();
        biomes[local_column_index(1, 0)] = "minecraft:swamp".to_string();

        apply_surface_biome_cells(&mut chunk, &biomes, &min_y, &max_y, "minecraft:savanna")
            .unwrap();

        require_natural_ground(
            chunk.get_block_state_id(0, 64, 0).unwrap(),
            "minecraft:swamp",
        );
        require_natural_ground(
            chunk.get_block_state_id(1, 64, 0).unwrap(),
            "minecraft:swamp",
        );
        assert_eq!(
            chunk.get_block_state_id(2, 64, 0).unwrap(),
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            chunk.get_biome_id_at(0, 64, 0).unwrap(),
            "minecraft:savanna"
        );

        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_biome_id("minecraft:dark_forest").unwrap();
        let y = SEA_LEVEL_Y + 20;
        let (mut biomes, min_y, max_y) = surface_biome_arrays("minecraft:dark_forest", y);
        fill_surface_top(&mut chunk, y, block_state_ids::GRASS_BLOCK);
        biomes[local_column_index(0, 0)] = "minecraft:windswept_savanna".to_string();
        biomes[local_column_index(1, 0)] = "minecraft:windswept_savanna".to_string();

        apply_surface_biome_cells(&mut chunk, &biomes, &min_y, &max_y, "minecraft:dark_forest")
            .unwrap();

        assert_eq!(
            chunk.get_block_state_id(0, y, 0).unwrap(),
            block_state_ids::ANDESITE
        );
        assert_eq!(
            chunk.get_block_state_id(1, y, 0).unwrap(),
            block_state_ids::ANDESITE
        );
        assert_eq!(
            chunk.get_block_state_id(2, y, 0).unwrap(),
            block_state_ids::GRASS_BLOCK
        );

        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_biome_id("minecraft:dark_forest").unwrap();
        let y = SEA_LEVEL_Y + 1;
        let (mut biomes, min_y, max_y) = surface_biome_arrays("minecraft:dark_forest", y);
        fill_surface_top(&mut chunk, y, block_state_ids::GRASS_BLOCK);
        biomes[local_column_index(0, 0)] = "minecraft:windswept_savanna".to_string();
        biomes[local_column_index(1, 0)] = "minecraft:windswept_savanna".to_string();

        apply_surface_biome_cells(&mut chunk, &biomes, &min_y, &max_y, "minecraft:dark_forest")
            .unwrap();

        assert_eq!(
            chunk.get_block_state_id(0, y, 0).unwrap(),
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            chunk.get_block_state_id(1, y, 0).unwrap(),
            block_state_ids::GRASS_BLOCK
        );

        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_biome_id("minecraft:savanna").unwrap();
        let y = SEA_LEVEL_Y + 12;
        let (biomes, min_y, max_y) = surface_biome_arrays("minecraft:savanna", y);
        fill_surface_top(&mut chunk, y, block_state_ids::SMOOTH_SANDSTONE);

        apply_surface_biome_cells(&mut chunk, &biomes, &min_y, &max_y, "minecraft:savanna")
            .unwrap();

        assert_eq!(
            chunk.get_block_state_id(0, y, 0).unwrap(),
            block_state_ids::SMOOTH_SANDSTONE
        );
    }

    #[test]
    fn surface_biome_cell_writer_dark_green_and_leaf_fixtures_match_java() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_biome_id("minecraft:savanna").unwrap();
        let (mut biomes, min_y, max_y) = surface_biome_arrays("minecraft:savanna", 64);
        fill_surface_top(&mut chunk, 64, block_state_ids::GRASS_BLOCK);
        biomes[local_column_index(0, 0)] = "minecraft:dark_forest".to_string();
        biomes[local_column_index(1, 0)] = "minecraft:swamp".to_string();

        apply_surface_biome_cells(&mut chunk, &biomes, &min_y, &max_y, "minecraft:savanna")
            .unwrap();

        require_natural_ground(
            chunk.get_block_state_id(0, 64, 0).unwrap(),
            "minecraft:dark_forest",
        );
        require_natural_ground(
            chunk.get_block_state_id(1, 64, 0).unwrap(),
            "minecraft:swamp",
        );
        assert_eq!(
            chunk.get_block_state_id(2, 64, 0).unwrap(),
            block_state_ids::GRASS_BLOCK
        );

        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_biome_id("minecraft:savanna").unwrap();
        let (mut biomes, min_y, max_y) = surface_biome_arrays("minecraft:savanna", 64);
        fill_surface_top(&mut chunk, 64, block_state_ids::OAK_LEAVES);
        biomes[local_column_index(0, 0)] = "minecraft:dark_forest".to_string();
        biomes[local_column_index(1, 0)] = "minecraft:dark_forest".to_string();

        apply_surface_biome_cells(&mut chunk, &biomes, &min_y, &max_y, "minecraft:savanna")
            .unwrap();

        require_natural_ground(
            chunk.get_block_state_id(0, 64, 0).unwrap(),
            "minecraft:dark_forest",
        );
        require_natural_ground(
            chunk.get_block_state_id(1, 64, 0).unwrap(),
            "minecraft:dark_forest",
        );
        let majority = chunk.get_block_state_id(2, 64, 0).unwrap();
        require_natural_ground(majority, "minecraft:savanna");
        assert_ne!(majority, block_state_ids::OAK_LEAVES);
    }

    #[test]
    fn surface_biome_cell_writer_uses_java_zero_render_fallback_for_unknown_blocks() {
        assert_eq!(
            render_surface_color(block_state_ids::AIR, Some("minecraft:plains")),
            0
        );
        assert_eq!(render_surface_color(i32::MAX, None), 0);
    }

    #[test]
    fn surface_chunk_builder_wires_biome_cells_and_static_carriers_like_java_surface_chunk() {
        let y = SEA_LEVEL_Y + 20;
        let default_column = surface_column(
            false,
            y,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:dark_forest",
        );
        let mut columns = vec![default_column; CHUNK_WIDTH * CHUNK_WIDTH];
        columns[local_column_index(0, 0)] = surface_column(
            false,
            y,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:windswept_savanna",
        );
        columns[local_column_index(1, 0)] = surface_column(
            false,
            y,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:windswept_savanna",
        );

        let build = build_surface_chunk(3, -2, &columns).unwrap();

        assert_eq!(build.land_columns, 256);
        assert_eq!(build.water_columns, 0);
        assert_eq!(build.min_ground_y, y);
        assert_eq!(build.max_ground_y, y);
        assert_eq!(build.biome, "minecraft:dark_forest");
        assert_eq!(build.chunk.chunk_x(), 3);
        assert_eq!(build.chunk.chunk_z(), -2);
        assert_eq!(build.ground_surface_y_by_local_column[0], y);
        assert!(!build.water_by_local_column[0]);
        assert_eq!(
            build.chunk.get_block_state_id(0, y, 0).unwrap(),
            block_state_ids::ANDESITE
        );
        assert_eq!(
            build.chunk.get_block_state_id(1, y, 0).unwrap(),
            block_state_ids::ANDESITE
        );
        assert_eq!(
            build.chunk.get_block_state_id(2, y, 0).unwrap(),
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            build.chunk.get_biome_id_at(0, y, 0).unwrap(),
            "minecraft:dark_forest"
        );
    }

    #[test]
    fn surface_chunk_builder_validates_one_column_per_chunk_block() {
        let columns = vec![
            land_surface_column(
                block_state_ids::GRASS_BLOCK,
                "minecraft:plains",
                SEA_LEVEL_Y
            );
            (CHUNK_WIDTH * CHUNK_WIDTH) - 1
        ];

        assert!(build_surface_chunk(0, 0, &columns).is_err());
    }

    #[test]
    fn surface_chunk_sampler_water_and_coast_helpers_match_java_contract() {
        assert!(surface_chunk_water_decision(-6.0, 100.0));
        assert!(!surface_chunk_water_decision(8.0, -100.0));
        assert!(surface_chunk_water_decision(0.0, 0.0));
        assert!(!surface_chunk_water_decision(0.0, 0.1));
        assert!(surface_chunk_requires_smoothed_water_decision(0.0));
        assert!(!surface_chunk_requires_smoothed_water_decision(-6.0));
        assert!(!surface_chunk_requires_smoothed_water_decision(8.0));

        let mut valid = vec![true; SURFACE_CHUNK_EXTENT * SURFACE_CHUNK_EXTENT];
        let mut water_mask = vec![false; SURFACE_CHUNK_EXTENT * SURFACE_CHUNK_EXTENT];
        assert_eq!(
            surface_chunk_coast_factor(&valid, &water_mask, 64, 64, false),
            0.0
        );
        water_mask[surface_chunk_extent_index(65, 64)] = true;
        assert_eq!(
            surface_chunk_coast_factor(&valid, &water_mask, 64, 64, false),
            1.0
        );
        valid[surface_chunk_extent_index(65, 64)] = false;
        assert_eq!(
            surface_chunk_coast_factor(&valid, &water_mask, 64, 64, false),
            0.0
        );
    }

    #[test]
    fn surface_chunk_sampler_generates_java_shaped_columns_from_heightmap_closure() {
        let mapping = EarthScaleMapping::for_denominator(1_000_000, -90.0, 90.0).unwrap();
        let sample =
            sample_surface_chunk_with_elevation_fn(0, 0, &mapping, 1.0, |longitude, _latitude| {
                Ok(if longitude < 90.0 { -20.0 } else { 100.0 })
            })
            .unwrap();

        assert_eq!(sample.columns().len(), CHUNK_WIDTH * CHUNK_WIDTH);
        let water = sample.column(0, 0).unwrap();
        assert!(water.water);
        assert_eq!(water.water_surface_y, SEA_LEVEL_Y);
        assert!(water.ground_surface_y < SEA_LEVEL_Y);
        let land = sample.column(15, 0).unwrap();
        assert!(!land.water);
        assert_eq!(land.water_surface_y, i32::MIN);
        assert!(land.ground_surface_y > SEA_LEVEL_Y);
        assert!(sample.column(-1, 0).is_err());
        assert!(sample_surface_chunk_with_elevation_fn(
            0,
            0,
            &mapping,
            0.0,
            |_longitude, _latitude| { Ok(0.0) }
        )
        .is_err());
    }

    #[test]
    fn surface_class_smoother_matches_java_non_photo_fixture_cases() {
        let mut patch = filled_surface_columns(
            5,
            5,
            land_surface_column(
                block_state_ids::GRASS_BLOCK,
                "minecraft:forest",
                SEA_LEVEL_Y + 4,
            ),
        );
        patch[surface_class_index(2, 2, 5)] = land_surface_column(
            block_state_ids::RED_SAND,
            "minecraft:desert",
            SEA_LEVEL_Y + 4,
        );

        let smoothed = smooth_surface_classes(&patch, 5).unwrap();
        let center = &smoothed[surface_class_index(2, 2, 5)];
        assert_eq!(center.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert_eq!(center.filler_block_state_id, block_state_ids::DIRT);
        assert_eq!(center.biome_id, "minecraft:forest");
        assert_eq!(center.decision_source, "smoother-isolated");

        let mut coast = filled_surface_columns(
            5,
            5,
            land_surface_column(
                block_state_ids::GRASS_BLOCK,
                "minecraft:forest",
                SEA_LEVEL_Y + 4,
            ),
        );
        coast[surface_class_index(2, 2, 5)] = water_surface_column();
        let coast_smoothed = smooth_surface_classes(&coast, 5).unwrap();
        let water = &coast_smoothed[surface_class_index(2, 2, 5)];
        assert!(water.water);
        assert_eq!(water.top_block_state_id, block_state_ids::CLAY);

        let mut snow_neighborhood = filled_surface_columns(
            5,
            5,
            land_surface_column(
                block_state_ids::SNOW_BLOCK,
                "minecraft:snowy_plains",
                SEA_LEVEL_Y + 4,
            ),
        );
        snow_neighborhood[surface_class_index(2, 2, 5)] = land_surface_column(
            block_state_ids::GRASS_BLOCK,
            "minecraft:plains",
            SEA_LEVEL_Y + 4,
        );
        let snow_smoothed = smooth_surface_classes(&snow_neighborhood, 5).unwrap();
        let center = &snow_smoothed[surface_class_index(2, 2, 5)];
        assert_eq!(center.biome_id, "minecraft:snowy_plains");
        assert_eq!(center.top_block_state_id, block_state_ids::SNOW_BLOCK);
        assert_eq!(center.filler_block_state_id, block_state_ids::DIRT);

        let mut beach = filled_surface_columns(
            5,
            5,
            land_surface_column(
                block_state_ids::GRASS_BLOCK,
                "minecraft:forest",
                SEA_LEVEL_Y + 4,
            ),
        );
        beach[surface_class_index(2, 2, 5)] =
            land_surface_column(block_state_ids::SAND, "minecraft:beach", SEA_LEVEL_Y + 4);
        let beach_smoothed = smooth_surface_classes(&beach, 5).unwrap();
        let center = &beach_smoothed[surface_class_index(2, 2, 5)];
        assert_eq!(center.top_block_state_id, block_state_ids::SAND);
        assert_eq!(center.biome_id, "minecraft:beach");

        let mut wetland = filled_surface_columns(
            5,
            5,
            land_surface_column(
                block_state_ids::GRASS_BLOCK,
                "minecraft:savanna",
                SEA_LEVEL_Y + 4,
            ),
        );
        let mut swamp =
            land_surface_column(block_state_ids::MUD, "minecraft:swamp", SEA_LEVEL_Y + 4);
        swamp.terrain_token_source = TerrainTokenSource::JavaStandardPalette;
        swamp.data_evidence_flags = surface_data_evidence::SWAMP;
        wetland[surface_class_index(2, 2, 5)] = swamp;
        let wetland_smoothed = smooth_surface_classes(&wetland, 5).unwrap();
        let center = &wetland_smoothed[surface_class_index(2, 2, 5)];
        assert_eq!(center.top_block_state_id, block_state_ids::MUD);
        assert_eq!(center.biome_id, "minecraft:savanna");
        assert_eq!(
            center.terrain_token_source,
            TerrainTokenSource::JavaStandardPalette
        );
        assert_eq!(center.data_evidence_flags, surface_data_evidence::SWAMP);

        assert!(smooth_surface_classes(&patch, 0).is_err());
        assert!(smooth_surface_classes(&patch, 4).is_err());
    }

    #[test]
    fn surface_class_photo_texture_smoother_matches_java_fixture_cases() {
        let mut photo_speckles = filled_surface_columns(
            5,
            5,
            photo_texture_surface_column(
                block_state_ids::MOSS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:jungle",
            ),
        );
        photo_speckles[surface_class_index(2, 2, 5)] = photo_texture_surface_column(
            block_state_ids::ORANGE_TERRACOTTA,
            block_state_ids::ORANGE_TERRACOTTA,
            "minecraft:jungle",
        );
        photo_speckles[surface_class_index(2, 1, 5)] = photo_texture_surface_column(
            block_state_ids::TERRACOTTA,
            block_state_ids::TERRACOTTA,
            "minecraft:jungle",
        );
        photo_speckles[surface_class_index(1, 2, 5)] = photo_texture_surface_column(
            block_state_ids::GRANITE,
            block_state_ids::STONE,
            "minecraft:jungle",
        );
        let photo_smoothed = smooth_photo_textures(&photo_speckles, 5).unwrap();
        assert_eq!(
            photo_smoothed[surface_class_index(2, 2, 5)].top_block_state_id,
            block_state_ids::MOSS_BLOCK
        );
        assert_eq!(
            photo_smoothed[surface_class_index(2, 2, 5)].decision_source,
            "smoother-photo"
        );

        let mut dry_savanna_sand_speckle = filled_surface_columns(
            7,
            7,
            photo_texture_surface_column(
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ),
        );
        dry_savanna_sand_speckle[surface_class_index(3, 3, 7)] = photo_texture_surface_column(
            block_state_ids::SANDSTONE,
            block_state_ids::SANDSTONE,
            "minecraft:savanna",
        );
        let dry_savanna_sand_speckle_smoothed =
            smooth_photo_textures(&dry_savanna_sand_speckle, 7).unwrap();
        assert_eq!(
            dry_savanna_sand_speckle_smoothed[surface_class_index(3, 3, 7)].top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );

        let mut dry_savanna_sand_island = filled_surface_columns(
            9,
            9,
            photo_texture_surface_column(
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ),
        );
        for z in 3..=5 {
            for x in 3..=5 {
                dry_savanna_sand_island[surface_class_index(x, z, 9)] =
                    photo_texture_surface_column(
                        block_state_ids::SANDSTONE,
                        block_state_ids::SANDSTONE,
                        "minecraft:savanna",
                    );
            }
        }
        let dry_savanna_sand_smoothed = smooth_photo_textures(&dry_savanna_sand_island, 9).unwrap();
        assert_eq!(
            dry_savanna_sand_smoothed[surface_class_index(4, 4, 9)].top_block_state_id,
            block_state_ids::SANDSTONE
        );

        let mut desert_sand_patch = filled_surface_columns(
            9,
            9,
            photo_texture_surface_column(
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ),
        );
        for z in 3..=5 {
            for x in 3..=5 {
                desert_sand_patch[surface_class_index(x, z, 9)] = photo_texture_surface_column(
                    block_state_ids::SAND,
                    block_state_ids::SAND,
                    "minecraft:desert",
                );
            }
        }
        let desert_sand_smoothed = smooth_photo_textures(&desert_sand_patch, 9).unwrap();
        assert_eq!(
            desert_sand_smoothed[surface_class_index(4, 4, 9)].top_block_state_id,
            block_state_ids::SAND
        );

        let mut jungle_hot_rock_noise = filled_surface_columns(
            11,
            11,
            photo_texture_surface_column(
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:jungle",
            ),
        );
        for z in 4..=6 {
            for x in 4..=6 {
                jungle_hot_rock_noise[surface_class_index(x, z, 11)] = photo_texture_surface_column(
                    block_state_ids::ORANGE_TERRACOTTA,
                    block_state_ids::ORANGE_TERRACOTTA,
                    "minecraft:jungle",
                );
            }
        }
        let jungle_hot_rock_smoothed = smooth_photo_textures(&jungle_hot_rock_noise, 11).unwrap();
        assert_eq!(
            jungle_hot_rock_smoothed[surface_class_index(5, 5, 11)].top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            jungle_hot_rock_smoothed[surface_class_index(5, 5, 11)].decision_source,
            "smoother-photo-macro"
        );

        assert!(smooth_photo_textures(&photo_speckles, 0).is_err());
        assert!(smooth_photo_textures(&photo_speckles, 4).is_err());
    }

    #[test]
    fn surface_biome_family_intent_grid_matches_java_non_photo_fixture_cases() {
        let mut cell = filled_surface_columns(
            4,
            4,
            surface_column(
                false,
                SEA_LEVEL_Y + 4,
                i32::MIN,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:forest",
            ),
        );
        cell[surface_class_index(3, 3, 4)] = surface_column(
            false,
            SEA_LEVEL_Y + 4,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let cell_smoothed = stabilize_surface_biome_families(&cell, 4).unwrap();
        assert_eq!(
            cell_smoothed[surface_class_index(3, 3, 4)].biome_id,
            "minecraft:forest"
        );
        assert_eq!(
            cell_smoothed[surface_class_index(3, 3, 4)].decision_source,
            "intent-stabilized-cell"
        );

        let mut photo_cell = filled_surface_columns(
            4,
            4,
            surface_column(
                false,
                SEA_LEVEL_Y + 4,
                i32::MIN,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:forest",
            ),
        );
        photo_cell[surface_class_index(3, 3, 4)] = photo_palette_surface_column(
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:savanna",
        );
        let photo_cell_smoothed =
            stabilize_surface_biome_families_preserving_surfaces(&photo_cell, 4).unwrap();
        assert_eq!(
            photo_cell_smoothed[surface_class_index(3, 3, 4)].biome_id,
            "minecraft:savanna"
        );
        assert_eq!(
            photo_cell_smoothed[surface_class_index(3, 3, 4)].decision_source,
            "photo-palette"
        );

        let mut preserve_cell = filled_surface_columns(
            4,
            4,
            surface_column(
                false,
                SEA_LEVEL_Y + 4,
                i32::MIN,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:forest",
            ),
        );
        preserve_cell[surface_class_index(3, 3, 4)] = surface_column(
            false,
            SEA_LEVEL_Y + 4,
            i32::MIN,
            block_state_ids::RED_SAND,
            block_state_ids::RED_SAND,
            "minecraft:desert",
        );
        let preserve_smoothed =
            stabilize_surface_biome_families_preserving_surfaces(&preserve_cell, 4).unwrap();
        let preserved_surface = &preserve_smoothed[surface_class_index(3, 3, 4)];
        assert_eq!(preserved_surface.biome_id, "minecraft:forest");
        assert_eq!(
            preserved_surface.top_block_state_id,
            block_state_ids::RED_SAND
        );
        assert_eq!(
            preserved_surface.filler_block_state_id,
            block_state_ids::RED_SAND
        );
        assert_eq!(preserved_surface.decision_source, "intent-stabilized-cell");

        let mut snow_cell = filled_surface_columns(
            4,
            4,
            surface_column(
                false,
                SEA_LEVEL_Y + 4,
                i32::MIN,
                block_state_ids::SNOW_BLOCK,
                block_state_ids::DIRT,
                "minecraft:snowy_plains",
            ),
        );
        snow_cell[surface_class_index(3, 3, 4)] = surface_column(
            false,
            SEA_LEVEL_Y + 4,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let snow_smoothed = stabilize_surface_biome_families(&snow_cell, 4).unwrap();
        let snowy = &snow_smoothed[surface_class_index(3, 3, 4)];
        assert_eq!(snowy.biome_id, "minecraft:snowy_plains");
        assert_eq!(snowy.top_block_state_id, block_state_ids::SNOW_BLOCK);

        let mut arid_transition = filled_surface_columns(
            8,
            8,
            surface_column(
                false,
                SEA_LEVEL_Y + 4,
                i32::MIN,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ),
        );
        for z in 0..4 {
            for x in 0..8 {
                arid_transition[surface_class_index(x, z, 8)] = surface_column(
                    false,
                    SEA_LEVEL_Y + 4,
                    i32::MIN,
                    block_state_ids::SAND,
                    block_state_ids::SAND,
                    "minecraft:desert",
                );
            }
        }
        let arid_smoothed = stabilize_surface_biome_families(&arid_transition, 8).unwrap();
        assert_eq!(
            arid_smoothed[surface_class_index(0, 0, 8)].biome_id,
            "minecraft:desert"
        );
        assert_eq!(
            arid_smoothed[surface_class_index(0, 7, 8)].biome_id,
            "minecraft:savanna"
        );

        let mut island = filled_surface_columns(
            8,
            8,
            surface_column(
                false,
                SEA_LEVEL_Y + 4,
                i32::MIN,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ),
        );
        for z in 3..=4 {
            for x in 3..=4 {
                island[surface_class_index(x, z, 8)] = surface_column(
                    false,
                    SEA_LEVEL_Y + 4,
                    i32::MIN,
                    block_state_ids::GRASS_BLOCK,
                    block_state_ids::DIRT,
                    "minecraft:jungle",
                );
            }
        }
        let island_smoothed = stabilize_surface_biome_families(&island, 8).unwrap();
        assert_eq!(
            island_smoothed[surface_class_index(3, 3, 8)].biome_id,
            "minecraft:savanna"
        );
        assert!(island_smoothed[surface_class_index(3, 3, 8)]
            .decision_source
            .starts_with("intent-stabilized"));

        let mut photo_island = filled_surface_columns(
            8,
            8,
            surface_column(
                false,
                SEA_LEVEL_Y + 4,
                i32::MIN,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ),
        );
        for z in 3..=4 {
            for x in 3..=4 {
                photo_island[surface_class_index(x, z, 8)] = photo_palette_surface_column(
                    block_state_ids::GRASS_BLOCK,
                    block_state_ids::DIRT,
                    "minecraft:jungle",
                );
            }
        }
        let photo_island_smoothed =
            stabilize_surface_biome_families_preserving_surfaces(&photo_island, 8).unwrap();
        assert_eq!(
            photo_island_smoothed[surface_class_index(3, 3, 8)].biome_id,
            "minecraft:jungle"
        );
        assert_eq!(
            photo_island_smoothed[surface_class_index(3, 3, 8)].decision_source,
            "photo-palette"
        );

        let mut tied_neighbors = filled_surface_columns(5, 5, water_surface_column());
        tied_neighbors[surface_class_index(2, 2, 5)] = surface_column(
            false,
            SEA_LEVEL_Y + 4,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:jungle",
        );
        tied_neighbors[surface_class_index(2, 1, 5)] = surface_column(
            false,
            SEA_LEVEL_Y + 4,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:dark_forest",
        );
        tied_neighbors[surface_class_index(2, 3, 5)] = surface_column(
            false,
            SEA_LEVEL_Y + 4,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:dark_forest",
        );
        tied_neighbors[surface_class_index(1, 2, 5)] = surface_column(
            false,
            SEA_LEVEL_Y + 4,
            i32::MIN,
            block_state_ids::SAND,
            block_state_ids::SAND,
            "minecraft:desert",
        );
        tied_neighbors[surface_class_index(3, 2, 5)] = surface_column(
            false,
            SEA_LEVEL_Y + 4,
            i32::MIN,
            block_state_ids::SAND,
            block_state_ids::SAND,
            "minecraft:desert",
        );
        let tied_smoothed = stabilize_small_surface_biome_components(&tied_neighbors, 5).unwrap();
        let tied_center = &tied_smoothed[surface_class_index(2, 2, 5)];
        assert_eq!(tied_center.biome_id, "minecraft:desert");
        assert_eq!(tied_center.top_block_state_id, block_state_ids::SAND);
        assert_eq!(tied_center.decision_source, "intent-stabilized-component");

        let mut mixed_family_component = filled_surface_columns(3, 3, water_surface_column());
        mixed_family_component[surface_class_index(1, 0, 3)] = photo_palette_surface_column(
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:forest",
        );
        mixed_family_component[surface_class_index(2, 0, 3)] = photo_palette_surface_column(
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:forest",
        );
        mixed_family_component[surface_class_index(1, 1, 3)] = surface_column(
            false,
            SEA_LEVEL_Y + 4,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:forest",
        );
        mixed_family_component[surface_class_index(2, 1, 3)] = surface_column(
            false,
            SEA_LEVEL_Y + 4,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:dark_forest",
        );
        mixed_family_component[surface_class_index(1, 2, 3)] = photo_palette_surface_column(
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:forest",
        );
        mixed_family_component[surface_class_index(2, 2, 3)] = photo_palette_surface_column(
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:forest",
        );
        let mixed_trace = surface_biome_component_trace(
            &mixed_family_component,
            3,
            surface_class_index(2, 1, 3),
            true,
        );
        assert_eq!(mixed_trace.family, "forest");
        assert_eq!(mixed_trace.size, 2);
        assert_eq!(
            mixed_trace.neighbor_majority_biome.as_deref(),
            Some("minecraft:forest")
        );
        assert_eq!(mixed_trace.action, "skip-already-majority");
        let mixed_smoothed = stabilize_small_surface_biome_components_preserving_surfaces(
            &mixed_family_component,
            3,
        )
        .unwrap();
        assert_eq!(
            mixed_smoothed[surface_class_index(2, 1, 3)].biome_id,
            "minecraft:dark_forest"
        );

        let mut swamp = filled_surface_columns(
            8,
            8,
            surface_column(
                false,
                SEA_LEVEL_Y + 4,
                i32::MIN,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ),
        );
        for z in 3..=4 {
            for x in 3..=4 {
                swamp[surface_class_index(x, z, 8)] = surface_column(
                    false,
                    SEA_LEVEL_Y + 4,
                    i32::MIN,
                    block_state_ids::MUD,
                    block_state_ids::MUD,
                    "minecraft:swamp",
                );
            }
        }
        let swamp_smoothed = stabilize_surface_biome_families(&swamp, 8).unwrap();
        assert_eq!(
            swamp_smoothed[surface_class_index(3, 3, 8)].biome_id,
            "minecraft:swamp"
        );
        assert_eq!(
            swamp_smoothed[surface_class_index(3, 3, 8)].top_block_state_id,
            block_state_ids::MUD
        );

        assert!(stabilize_surface_biome_families(&cell, 0).is_err());
        assert!(stabilize_small_surface_biome_components(&cell, 3).is_err());
    }

    #[test]
    fn value_noise_neighbor_coordinates_wrap_like_java_int_addition() {
        let edge = f64::from(i32::MAX);
        assert!(value_noise(edge, edge, 0x52dce729).is_finite());
        assert!(value_noise(edge + 0.25, edge + 0.25, 0x52dce729).is_finite());
    }

    #[test]
    fn surface_material_sample_matches_java_cover_contract() {
        let mixed_canopy = SurfaceMaterialSample::climate_only(
            RgbColor::of(90, 120, 70),
            2,
            20,
            15,
            10,
            8,
            30,
            5,
            -1,
            -1,
            -1,
        );
        assert!((mixed_canopy.tree_cover() - 0.20).abs() < 0.0001);
        assert!((mixed_canopy.canopy_cover() - 0.53).abs() < 0.0001);
        assert_eq!(mixed_canopy.herbaceous_cover(), 0.30);
        assert_eq!(mixed_canopy.shrub_cover(), 0.05);
        assert_eq!(mixed_canopy.vegetation_cover(), 0.30);
        assert!(mixed_canopy.has_vegetation_presence());

        let capped_canopy = SurfaceMaterialSample::climate_only(
            RgbColor::of(40, 85, 35),
            2,
            80,
            50,
            40,
            30,
            70,
            10,
            -1,
            -1,
            -1,
        );
        assert!((capped_canopy.canopy_cover() - 1.0).abs() < 0.0001);
    }

    #[test]
    fn surface_material_sample_normalizes_like_java_record_constructor() {
        let sample = SurfaceMaterialSample::new(
            RgbColor::of(1, 2, 3),
            RgbColor::unavailable(),
            TerrainTokenSource::JavaStandardPalette,
            0,
            255,
            100,
            101,
            SurfaceMaterialSample::UNKNOWN,
            0,
            -5,
            255,
            100,
            12,
            0,
            1250,
            "  Ecoregion  ",
            " minecraft:forest ",
            f64::INFINITY,
        );
        assert_eq!(sample.terrain_token_source, TerrainTokenSource::None);
        assert_eq!(sample.ecoregion_name, "Ecoregion");
        assert_eq!(sample.ecoregion_biome_id, "minecraft:forest");
        assert_eq!(sample.ecoregion_confidence, 0.0);
        assert!(!sample.has_climate_class());
        assert_eq!(sample.tree_cover(), 1.0);
        assert_eq!(sample.snow_cover_ratio(), 0.0);
        assert_eq!(sample.swamp_cover_ratio(), 1.0);
        assert_eq!(sample.slope_ratio(), 1.0);
        assert!(sample.has_ecoregion());
        assert!(sample.has_ecoregion_biome());
        assert!(sample.has_bathymetry());

        let token_sample = SurfaceMaterialSample::with_export_token(
            RgbColor::of(1, 2, 3),
            RgbColor::of(4, 5, 6),
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "",
            "",
            2.0,
        );
        assert_eq!(
            token_sample.terrain_token_source,
            TerrainTokenSource::Export
        );
        assert_eq!(token_sample.ecoregion_confidence, 1.0);
        assert!(token_sample.has_climate_class());
        assert!(!token_sample.has_bathymetry());
        assert_eq!(
            token_sample.with_color(RgbColor::of(9, 8, 7)).color,
            RgbColor::of(9, 8, 7)
        );

        assert_eq!(
            SurfaceMaterialSample::rounded(None),
            SurfaceMaterialSample::UNKNOWN
        );
        assert_eq!(SurfaceMaterialSample::rounded(Some(12.5)), 13);
        assert_eq!(SurfaceMaterialSample::rounded(Some(-12.5)), -12);
        assert_eq!(SurfaceMaterialSample::rounded(Some(f64::NAN)), 0);
    }

    #[test]
    fn photo_surface_solver_bootstrap_matches_java_contract_fixture_cases() {
        let semantic = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let input = PhotoSurfaceInput::new(
            semantic.clone(),
            SurfaceMaterialSample::color_only(RgbColor::of(218, 184, 92)),
            240.0,
            13.0,
            24.0,
            0.0,
            15.0,
            101,
            100,
        );
        let decision = solve_photo_surface(&input).unwrap();
        let solved = decision.to_column(&semantic);
        let applied = apply_photo_surface_material(&input).unwrap();
        assert_eq!(solved, applied);
        assert_ne!(decision.rendered_rgb, 0);
        assert!(!decision.stage_id.is_empty());
        assert!(!decision.trace.is_empty());
        assert!(matches!(
            applied.top_block_state_id,
            block_state_ids::SAND
                | block_state_ids::SANDSTONE
                | block_state_ids::END_STONE
                | block_state_ids::SMOOTH_SANDSTONE
                | block_state_ids::CUT_SANDSTONE
                | block_state_ids::CHISELED_SANDSTONE
                | block_state_ids::YELLOW_TERRACOTTA
        ));

        let lush_input = PhotoSurfaceInput::new(
            semantic.clone(),
            SurfaceMaterialSample::color_only(RgbColor::of(42, 95, 38)),
            180.0,
            21.0,
            -3.0,
            0.0,
            35.0,
            100,
            100,
        );
        let lush = apply_photo_surface_material(&lush_input).unwrap();
        assert!(matches!(
            lush.top_block_state_id,
            block_state_ids::GRASS_BLOCK
                | block_state_ids::OAK_LEAVES
                | block_state_ids::JUNGLE_LEAVES
                | block_state_ids::DARK_OAK_LEAVES
                | block_state_ids::SPRUCE_LEAVES
                | block_state_ids::MOSS_BLOCK
                | block_state_ids::PODZOL
        ));

        let same_top_semantic = surface_column(
            false,
            SEA_LEVEL_Y + 9,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::CLAY,
            "minecraft:plains",
        );
        let same_top_input = PhotoSurfaceInput::new(
            same_top_semantic.clone(),
            SurfaceMaterialSample::color_only(RgbColor::of(92, 133, 62)),
            150.0,
            12.0,
            35.0,
            0.0,
            5.0,
            8,
            9,
        );
        let same_top = apply_photo_surface_material(&same_top_input).unwrap();
        assert_eq!(same_top.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert_eq!(same_top.filler_block_state_id, block_state_ids::CLAY);

        let water = surface_column(
            true,
            SEA_LEVEL_Y - 4,
            SEA_LEVEL_Y,
            block_state_ids::CLAY,
            block_state_ids::CLAY,
            "minecraft:warm_ocean",
        );
        let water_input = PhotoSurfaceInput::new(
            water.clone(),
            SurfaceMaterialSample::color_only(RgbColor::of(42, 95, 38)),
            -10.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0,
            0,
        );
        assert_eq!(
            solve_photo_surface(&water_input).unwrap().to_column(&water),
            water
        );

        let no_photo_input = PhotoSurfaceInput::new(
            semantic.clone(),
            SurfaceMaterialSample::color_only(RgbColor::unavailable()),
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0,
            0,
        );
        assert_eq!(
            solve_photo_surface(&no_photo_input)
                .unwrap()
                .to_column(&semantic),
            semantic
        );

        let dark_standard_semantic = surface_column(
            false,
            SEA_LEVEL_Y + 1,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:forest",
        )
        .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette);
        let dark_standard_input = PhotoSurfaceInput::new(
            dark_standard_semantic.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(0, 8, 18),
                RgbColor::of(0, 0, 0),
                TerrainTokenSource::JavaStandardPalette,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                SurfaceMaterialSample::UNKNOWN,
                55_333,
                -3,
                0,
                "Atlantic Equatorial coastal forests",
                "minecraft:jungle",
                1.0,
            ),
            22.929_269_880_390_567,
            9.297_567_061_759_196,
            -0.112_233_830_266_731_62,
            0.9375,
            184.842_606_607_382_07,
            207,
            2,
        );
        let dark_standard = apply_photo_surface_material(&dark_standard_input).unwrap();
        assert_eq!(
            dark_standard.top_block_state_id,
            block_state_ids::BLACK_TERRACOTTA
        );
        assert_eq!(
            dark_standard.filler_block_state_id,
            block_state_ids::BLACK_TERRACOTTA
        );
        assert_eq!(dark_standard.biome_id, "minecraft:forest");

        let gray_rock_semantic = surface_column(
            false,
            SEA_LEVEL_Y,
            i32::MIN,
            block_state_ids::GRAVEL,
            block_state_ids::STONE,
            "minecraft:savanna",
        )
        .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette);
        let gray_rock_input = PhotoSurfaceInput::new(
            gray_rock_semantic.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(62, 65, 73),
                RgbColor::of(64, 64, 64),
                TerrainTokenSource::JavaStandardPalette,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                SurfaceMaterialSample::UNKNOWN,
                54_845,
                -187,
                0,
                "",
                "",
                0.0,
            ),
            63.0,
            8.758_577_666_874_755,
            -0.426_473_924_902_382_1,
            0.98,
            0.0,
            195,
            9,
        );
        let gray_rock = apply_photo_surface_material(&gray_rock_input).unwrap();
        assert_eq!(gray_rock.top_block_state_id, block_state_ids::DEEPSLATE);
        assert_eq!(gray_rock.filler_block_state_id, block_state_ids::STONE);
        assert_eq!(gray_rock.biome_id, "minecraft:savanna");
        assert_eq!(gray_rock.decision_source, "photo-palette");

        let swamp_carrier_input = PhotoSurfaceInput::new(
            surface_column(
                false,
                SEA_LEVEL_Y,
                i32::MIN,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:savanna",
            )
            .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette),
            SurfaceMaterialSample::new(
                RgbColor::of(36, 42, 53),
                RgbColor::of(50, 60, 30),
                TerrainTokenSource::JavaStandardPalette,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                SurfaceMaterialSample::UNKNOWN,
                55_102,
                -18,
                0,
                "",
                "",
                0.0,
            ),
            63.0,
            8.938_240_798_502_97,
            -0.381_555_747_143_153_35,
            0.98,
            0.0,
            199,
            8,
        );
        let swamp_carrier = apply_photo_surface_material(&swamp_carrier_input).unwrap();
        assert_eq!(
            swamp_carrier.top_block_state_id,
            block_state_ids::OAK_LEAVES
        );
        assert_eq!(swamp_carrier.filler_block_state_id, block_state_ids::DIRT);
        assert_eq!(swamp_carrier.biome_id, "minecraft:swamp");
        assert_eq!(swamp_carrier.decision_source, "photo-palette");

        let dark_forest_carrier_input = PhotoSurfaceInput::new(
            surface_column(
                false,
                SEA_LEVEL_Y,
                i32::MIN,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:savanna",
            )
            .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette),
            SurfaceMaterialSample::new(
                RgbColor::of(33, 39, 50),
                RgbColor::of(50, 60, 30),
                TerrainTokenSource::JavaStandardPalette,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                SurfaceMaterialSample::UNKNOWN,
                54_976,
                -17,
                0,
                "",
                "",
                0.0,
            ),
            63.0,
            8.938_594_783_144_245,
            -0.359_507_345_111_242,
            0.98,
            28.022_039_515_908_155,
            200,
            10,
        );
        let dark_forest_carrier = apply_photo_surface_material(&dark_forest_carrier_input).unwrap();
        assert_eq!(
            dark_forest_carrier.top_block_state_id,
            block_state_ids::OAK_LEAVES
        );
        assert_eq!(
            dark_forest_carrier.filler_block_state_id,
            block_state_ids::DIRT
        );
        assert_eq!(dark_forest_carrier.biome_id, "minecraft:dark_forest");
        assert_eq!(dark_forest_carrier.decision_source, "photo-palette");
    }

    #[test]
    fn photo_surface_solver_handles_color_only_edge_cases() {
        let plains = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );

        let vegetated_coast = apply_photo_surface_material(&PhotoSurfaceInput::new(
            surface_column(
                false,
                SEA_LEVEL_Y + 1,
                i32::MIN,
                block_state_ids::SAND,
                block_state_ids::SAND,
                "minecraft:beach",
            ),
            SurfaceMaterialSample::land(
                RgbColor::of(62, 122, 58),
                8,
                8,
                12,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                26,
                10,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                16,
                SurfaceMaterialSample::UNKNOWN,
                18,
                "temperate coastal grassland",
                "minecraft:plains",
                1.0,
            ),
            8.0,
            -5.0,
            43.0,
            1.0,
            12.0,
            96,
            64,
        ))
        .unwrap();
        assert!(!matches!(
            vegetated_coast.top_block_state_id,
            block_state_ids::SAND | block_state_ids::SANDSTONE | block_state_ids::RED_SAND
        ));

        let dark_rock = apply_photo_surface_material(&PhotoSurfaceInput::new(
            plains.clone(),
            SurfaceMaterialSample::color_only(RgbColor::of(62, 58, 55)),
            1_600.0,
            -70.0,
            -20.0,
            0.0,
            260.0,
            102,
            100,
        ))
        .unwrap();
        assert!(matches!(
            dark_rock.top_block_state_id,
            block_state_ids::DEEPSLATE
                | block_state_ids::STONE
                | block_state_ids::TUFF
                | block_state_ids::ANDESITE
                | block_state_ids::GRANITE
                | block_state_ids::DIORITE
                | block_state_ids::GRAY_TERRACOTTA
                | block_state_ids::BLACK_TERRACOTTA
                | block_state_ids::CYAN_TERRACOTTA
                | block_state_ids::MUD
                | block_state_ids::ROOTED_DIRT
                | block_state_ids::MYCELIUM
        ));

        let snow_input = PhotoSurfaceInput::new(
            plains.clone(),
            SurfaceMaterialSample::climate_only(
                RgbColor::of(230, 232, 224),
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                40,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
            ),
            2_300.0,
            86.9,
            28.0,
            0.0,
            80.0,
            103,
            100,
        );
        let snow_decision = solve_photo_surface(&snow_input).unwrap();
        let snow = snow_decision.to_column(&snow_input.semantic_column);
        assert_eq!(block_state_ids::SNOW_BLOCK, snow.top_block_state_id);
        assert_eq!("minecraft:snowy_plains", snow.biome_id);
        assert_eq!("photo-ecology", snow.decision_source);
        assert_eq!("photo-ecology", snow_decision.recipe_id);
        assert_eq!("source-render-solver", snow_decision.stage_id);

        let calcite_input = PhotoSurfaceInput::new(
            plains,
            SurfaceMaterialSample::climate_only(
                RgbColor::of(224, 220, 204),
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                40,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
            ),
            2_300.0,
            86.9,
            28.0,
            0.0,
            80.0,
            104,
            100,
        );
        let calcite_decision = solve_photo_surface(&calcite_input).unwrap();
        let calcite = calcite_decision.to_column(&calcite_input.semantic_column);
        assert_eq!(block_state_ids::CALCITE, calcite.top_block_state_id);
        assert_eq!("photo-texture", calcite.decision_source);
        assert_eq!("photo-texture", calcite_decision.recipe_id);
        assert_eq!("source-render-solver", calcite_decision.stage_id);
    }

    #[test]
    fn photo_surface_solver_handles_java_standard_arid_token_cases() {
        let plains = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let savanna = surface_column(
            false,
            SEA_LEVEL_Y + 10,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:savanna",
        );

        let dry_coastal_palette = apply_photo_surface_material(&PhotoSurfaceInput::new(
            savanna.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(218, 202, 142),
                RgbColor::of(218, 202, 142),
                TerrainTokenSource::JavaStandardPalette,
                13,
                0,
                0,
                0,
                0,
                0,
                0,
                -1,
                -1,
                28,
                -1,
                15,
                "dry coastal scrub",
                "minecraft:savanna",
                1.0,
            ),
            8.0,
            58.0,
            19.0,
            0.80,
            8.0,
            128,
            96,
        ))
        .unwrap();
        assert_ne!(
            dry_coastal_palette.top_block_state_id,
            block_state_ids::SAND
        );

        let export_sand_token_over_green = apply_photo_surface_material(&PhotoSurfaceInput::new(
            savanna.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(42, 95, 38),
                RgbColor::of(230, 205, 160),
                TerrainTokenSource::Export,
                4,
                0,
                0,
                0,
                0,
                0,
                0,
                -1,
                -1,
                -1,
                -1,
                12,
                "desert",
                "minecraft:desert",
                0.80,
            ),
            220.0,
            50.0,
            15.0,
            0.0,
            15.0,
            280,
            216,
        ))
        .unwrap();
        assert!(matches!(
            export_sand_token_over_green.top_block_state_id,
            block_state_ids::GRASS_BLOCK
                | block_state_ids::OAK_LEAVES
                | block_state_ids::JUNGLE_LEAVES
                | block_state_ids::DARK_OAK_LEAVES
                | block_state_ids::SPRUCE_LEAVES
                | block_state_ids::MOSS_BLOCK
                | block_state_ids::GREEN_TERRACOTTA
                | block_state_ids::LIME_TERRACOTTA
        ));

        let standard_sand_token_over_green = apply_photo_surface_material(&PhotoSurfaceInput::new(
            savanna.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(54, 118, 46),
                RgbColor::of(230, 205, 160),
                TerrainTokenSource::JavaStandardPalette,
                3,
                0,
                0,
                0,
                0,
                30,
                12,
                -1,
                -1,
                28,
                -1,
                45,
                "West Sudanian savanna",
                "minecraft:savanna",
                0.95,
            ),
            260.0,
            16.0,
            10.0,
            0.0,
            30.0,
            316,
            236,
        ))
        .unwrap();
        assert!(matches!(
            standard_sand_token_over_green.top_block_state_id,
            block_state_ids::GRASS_BLOCK
                | block_state_ids::OAK_LEAVES
                | block_state_ids::JUNGLE_LEAVES
                | block_state_ids::DARK_OAK_LEAVES
                | block_state_ids::SPRUCE_LEAVES
                | block_state_ids::MOSS_BLOCK
                | block_state_ids::GREEN_TERRACOTTA
                | block_state_ids::LIME_TERRACOTTA
        ));

        let olive_standard_sand_token = apply_photo_surface_material(&PhotoSurfaceInput::new(
            savanna.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(126, 123, 70),
                RgbColor::of(230, 205, 160),
                TerrainTokenSource::JavaStandardPalette,
                3,
                0,
                0,
                0,
                0,
                24,
                18,
                -1,
                -1,
                28,
                -1,
                36,
                "West Sudanian savanna",
                "minecraft:savanna",
                0.95,
            ),
            260.0,
            16.0,
            10.0,
            0.0,
            30.0,
            317,
            236,
        ))
        .unwrap();
        assert_ne!(olive_standard_sand_token.decision_source, "photo-palette");

        let false_snow_desert = apply_photo_surface_material(&PhotoSurfaceInput::new(
            plains.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(238, 226, 190),
                RgbColor::of(250, 255, 250),
                TerrainTokenSource::JavaStandardPalette,
                13,
                0,
                0,
                0,
                0,
                4,
                2,
                0,
                0,
                30,
                -1,
                12,
                "Saharan desert",
                "minecraft:desert",
                1.0,
            ),
            350.0,
            13.0,
            24.0,
            0.0,
            20.0,
            140,
            112,
        ))
        .unwrap();
        assert_ne!(
            false_snow_desert.top_block_state_id,
            block_state_ids::SNOW_BLOCK
        );

        let snow_cover_boundary = apply_photo_surface_material(&PhotoSurfaceInput::new(
            plains.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(138, 122, 86),
                RgbColor::of(250, 255, 250),
                TerrainTokenSource::JavaStandardPalette,
                13,
                0,
                0,
                0,
                0,
                0,
                0,
                10,
                0,
                5,
                -1,
                30,
                "snowy upland scrub",
                "minecraft:plains",
                0.80,
            ),
            450.0,
            38.0,
            6.0,
            0.0,
            15.0,
            164,
            164,
        ))
        .unwrap();
        assert_eq!(
            snow_cover_boundary.top_block_state_id,
            block_state_ids::SNOW_BLOCK
        );

        let high_elevation_dry_false_snow = apply_photo_surface_material(&PhotoSurfaceInput::new(
            plains.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(143, 128, 82),
                RgbColor::of(250, 255, 250),
                TerrainTokenSource::JavaStandardPalette,
                13,
                0,
                0,
                0,
                0,
                4,
                2,
                0,
                0,
                12,
                -1,
                120,
                "high desert plateau",
                "minecraft:desert",
                0.90,
            ),
            2_500.0,
            34.0,
            14.0,
            0.0,
            110.0,
            188,
            188,
        ))
        .unwrap();
        assert_ne!(
            high_elevation_dry_false_snow.top_block_state_id,
            block_state_ids::SNOW_BLOCK
        );

        let gray_rock_standard_token = apply_photo_surface_material(&PhotoSurfaceInput::new(
            plains.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(156, 149, 137),
                RgbColor::of(230, 205, 160),
                TerrainTokenSource::JavaStandardPalette,
                4,
                0,
                0,
                0,
                0,
                0,
                0,
                -1,
                -1,
                28,
                -1,
                150,
                "rocky desert plateau",
                "minecraft:desert",
                0.95,
            ),
            900.0,
            34.0,
            22.0,
            0.0,
            130.0,
            512,
            512,
        ))
        .unwrap();
        assert_ne!(
            gray_rock_standard_token.top_block_state_id,
            block_state_ids::SAND
        );
        assert_ne!(
            gray_rock_standard_token.top_block_state_id,
            block_state_ids::SANDSTONE
        );
        let gray_source = RgbColor::of(156, 149, 137);
        let gray_biome = "minecraft:desert";
        assert!(
            photo_weighted_render_distance(
                gray_source,
                gray_rock_standard_token.top_block_state_id,
                gray_biome
            ) < photo_weighted_render_distance(gray_source, block_state_ids::SAND, gray_biome).min(
                photo_weighted_render_distance(gray_source, block_state_ids::SANDSTONE, gray_biome)
            )
        );

        let mut first_dither_top = -1;
        let mut saw_second_dither_top = false;
        for z in 0..4 {
            for x in 0..4 {
                let dithered = apply_photo_surface_material(&PhotoSurfaceInput::new(
                    plains.clone(),
                    SurfaceMaterialSample::new(
                        RgbColor::of(242, 216, 160),
                        RgbColor::of(230, 205, 160),
                        TerrainTokenSource::JavaStandardPalette,
                        13,
                        0,
                        0,
                        0,
                        0,
                        4,
                        2,
                        0,
                        0,
                        30,
                        -1,
                        12,
                        "Saharan desert",
                        "minecraft:desert",
                        1.0,
                    ),
                    350.0,
                    13.0,
                    24.0,
                    0.0,
                    20.0,
                    x,
                    z,
                ))
                .unwrap();
                assert!(matches!(
                    dithered.top_block_state_id,
                    block_state_ids::SAND
                        | block_state_ids::SANDSTONE
                        | block_state_ids::SMOOTH_SANDSTONE
                        | block_state_ids::CUT_SANDSTONE
                        | block_state_ids::CHISELED_SANDSTONE
                        | block_state_ids::END_STONE
                        | block_state_ids::END_STONE_BRICKS
                        | block_state_ids::WHITE_TERRACOTTA
                        | block_state_ids::LIGHT_GRAY_TERRACOTTA
                        | block_state_ids::CALCITE
                        | block_state_ids::BONE_BLOCK
                ));
                assert_ne!(dithered.top_block_state_id, block_state_ids::SNOW_BLOCK);
                if first_dither_top < 0 {
                    first_dither_top = dithered.top_block_state_id;
                } else if dithered.top_block_state_id != first_dither_top {
                    saw_second_dither_top = true;
                }
            }
        }
        assert!(saw_second_dither_top);
    }

    #[test]
    fn photo_arid_dither_avoids_single_block_bayer_checkerboard() {
        let mut threshold_grid = [[0_i32; 4]; 4];
        for z in 0..4 {
            for x in 0..4 {
                threshold_grid[z][x] = photo_dither_threshold(
                    1388 + x as i32,
                    272 + z as i32,
                    block_state_ids::TERRACOTTA,
                    block_state_ids::GRANITE,
                );
            }
        }
        let choice_grid = threshold_grid.map(|row| row.map(|threshold| threshold < 8));
        assert!(
            !is_boolean_parity_checkerboard(&choice_grid),
            "photo dithering should form organic patches, not a 1-block parity checkerboard"
        );

        let savanna = surface_column(
            false,
            SEA_LEVEL_Y + 10,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:savanna",
        )
        .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette);
        let sample = SurfaceMaterialSample::new(
            RgbColor::of(157, 109, 75),
            RgbColor::of(170, 105, 60),
            TerrainTokenSource::JavaStandardPalette,
            4,
            0,
            0,
            0,
            0,
            0,
            72,
            0,
            SurfaceMaterialSample::UNKNOWN,
            46_103,
            440,
            0,
            "Gibson desert",
            "minecraft:badlands",
            1.0,
        );
        let mut top_grid = [[0_i32; 4]; 4];
        for z in 0..4 {
            for x in 0..4 {
                let column = apply_photo_surface_material(&PhotoSurfaceInput::new(
                    savanna.clone(),
                    sample.clone(),
                    433.9769227262579,
                    124.80538922155688,
                    -24.57103827383429,
                    0.0,
                    88.65127942295612,
                    1388 + x as i32,
                    272 + z as i32,
                ))
                .unwrap();
                top_grid[z][x] = column.top_block_state_id;
            }
        }
        assert!(
            !is_i32_parity_checkerboard(&top_grid),
            "near 1389,273 arid photo palette should not alternate by one-block parity"
        );
    }

    #[test]
    fn photo_region_token_luma_profile_changes_java_standard_arid_dither() {
        let plains = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:plains",
        );
        let token_color = RgbColor::of(230, 205, 160);
        let token = MetTerrainVocabulary::exact(token_color);
        assert!(source_ranked_cross_crop_token(token));
        let mut profile = PhotoSurfaceTokenLumaProfile::default();
        for offset in 0..32 {
            profile.add(&SurfaceMaterialSample::new(
                RgbColor::of(95 + offset, 82 + offset, 54 + offset),
                token_color,
                TerrainTokenSource::JavaStandardPalette,
                13,
                0,
                0,
                0,
                0,
                4,
                2,
                0,
                0,
                30,
                -1,
                12,
                "Saharan desert profile fixture",
                "minecraft:desert",
                1.0,
            ));
        }
        let source = RgbColor::of(242, 216, 160);
        assert_eq!(profile.normalized_luma(token, source), Some(1.0));
        assert!(profile.mean_source(token).is_some());
        let profile = Arc::new(profile);
        let material = SurfaceMaterialSample::new(
            source,
            token_color,
            TerrainTokenSource::JavaStandardPalette,
            13,
            0,
            0,
            0,
            0,
            4,
            2,
            0,
            0,
            30,
            -1,
            12,
            "Saharan desert",
            "minecraft:desert",
            1.0,
        );

        let mut saw_profile_dependent_top = false;
        for z in 0..4 {
            for x in 0..4 {
                let baseline = apply_photo_surface_material(&PhotoSurfaceInput::new(
                    plains.clone(),
                    material.clone(),
                    350.0,
                    13.0,
                    24.0,
                    0.0,
                    20.0,
                    x,
                    z,
                ))
                .unwrap();
                let profiled = apply_photo_surface_material(
                    &PhotoSurfaceInput::new(
                        plains.clone(),
                        material.clone(),
                        350.0,
                        13.0,
                        24.0,
                        0.0,
                        20.0,
                        x,
                        z,
                    )
                    .with_token_luma_profile(Arc::clone(&profile)),
                )
                .unwrap();
                assert_eq!(baseline.decision_source, "photo-palette");
                assert_eq!(profiled.decision_source, "photo-palette");
                saw_profile_dependent_top |=
                    baseline.top_block_state_id != profiled.top_block_state_id;
            }
        }
        assert!(saw_profile_dependent_top);
    }

    #[derive(Debug)]
    struct RegionTokenProfileMaterialSampler {
        varied_profile: bool,
    }

    impl RegionTokenProfileMaterialSampler {
        fn material_for(&self, longitude: f64) -> SurfaceMaterialSample {
            let source = if self.varied_profile && longitude < 5.0 {
                let offset = ((longitude.max(0.0) / 5.0) * 31.0).round() as u8;
                RgbColor::of(
                    86 + offset.min(31),
                    82 + offset.min(31),
                    78 + offset.min(31),
                )
            } else {
                RgbColor::of(156, 149, 137)
            };
            SurfaceMaterialSample::new(
                source,
                RgbColor::of(230, 205, 160),
                TerrainTokenSource::JavaStandardPalette,
                13,
                0,
                0,
                0,
                0,
                4,
                2,
                0,
                0,
                30,
                -1,
                150,
                "rocky desert plateau",
                "minecraft:desert",
                1.0,
            )
        }
    }

    impl SurfaceMaterialSampler for RegionTokenProfileMaterialSampler {
        fn sample(
            &self,
            longitude: f64,
            _latitude: f64,
            _longitude_span_degrees: f64,
            _latitude_span_degrees: f64,
        ) -> Result<SurfaceMaterialSample> {
            Ok(self.material_for(longitude))
        }

        fn sample_photo(
            &self,
            longitude: f64,
            _latitude: f64,
            _longitude_span_degrees: f64,
            _latitude_span_degrees: f64,
        ) -> Result<SurfaceMaterialSample> {
            Ok(self.material_for(longitude))
        }
    }

    #[test]
    #[ignore = "slow full-region profile wiring fixture"]
    fn surface_region_photo_pass_uses_collected_token_luma_profile() {
        let mapping = EarthScaleMapping::for_denominator(15_000, -90.0, 90.0).unwrap();
        let uniform = sample_surface_region_with_elevation_fn(
            0,
            0,
            &mapping,
            DEFAULT_VERTICAL_SCALE,
            |_longitude, _latitude| Ok(900.0),
            SurfaceTextureMode::Photo,
            Some(&RegionTokenProfileMaterialSampler {
                varied_profile: false,
            }),
            true,
        )
        .unwrap();
        let profiled = sample_surface_region_with_elevation_fn(
            0,
            0,
            &mapping,
            DEFAULT_VERTICAL_SCALE,
            |_longitude, _latitude| Ok(900.0),
            SurfaceTextureMode::Photo,
            Some(&RegionTokenProfileMaterialSampler {
                varied_profile: true,
            }),
            true,
        )
        .unwrap();

        let comparison_min_longitude = mapping
            .longitude_for_block_x(mapping.width_blocks / 2 + 128)
            .unwrap();
        assert!(comparison_min_longitude >= 5.0);
        let mut saw_profile_dependent_region_top = false;
        for z in 0..16 {
            for x in 128..256 {
                let uniform_column = uniform.column(x, z).unwrap();
                let profiled_column = profiled.column(x, z).unwrap();
                assert!(uniform_column.decision_source.starts_with("photo-palette"));
                assert!(profiled_column.decision_source.starts_with("photo-palette"));
                if uniform_column.top_block_state_id != profiled_column.top_block_state_id {
                    saw_profile_dependent_region_top = true;
                }
            }
        }
        assert!(saw_profile_dependent_region_top);
    }

    #[test]
    fn photo_surface_solver_handles_java_standard_vegetation_token_cases() {
        let savanna = surface_column(
            false,
            SEA_LEVEL_Y + 10,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:savanna",
        );
        let jungle = surface_column(
            false,
            SEA_LEVEL_Y + 12,
            i32::MIN,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:jungle",
        );

        let high_relief_windswept_grass = photo_surface_decision_for_top(
            &PhotoSurfaceInput::new(
                savanna.clone(),
                SurfaceMaterialSample::land(
                    RgbColor::of(135, 141, 75),
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    SurfaceMaterialSample::UNKNOWN,
                    "",
                    "",
                    0.0,
                ),
                12.0,
                6.4,
                -0.02,
                0.95,
                180.0,
                143,
                0,
            ),
            block_state_ids::GRASS_BLOCK,
            "photo-texture",
            "source-render-solver",
            RgbColor::of(135, 141, 75),
            "high relief dry grass render biome",
        );
        assert_eq!(
            high_relief_windswept_grass.biome_id,
            "minecraft:windswept_savanna"
        );

        let java_standard_gray_photo_seed = apply_photo_surface_material(&PhotoSurfaceInput::new(
            surface_column(
                false,
                SEA_LEVEL_Y,
                i32::MIN,
                block_state_ids::STONE,
                block_state_ids::STONE,
                "minecraft:savanna",
            )
            .with_decision_source("material-rule")
            .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette)
            .with_data_evidence_flags(192),
            SurfaceMaterialSample::new(
                RgbColor::of(136, 137, 137),
                RgbColor::of(140, 150, 110),
                TerrainTokenSource::JavaStandardPalette,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                SurfaceMaterialSample::UNKNOWN,
                54_487,
                -2_437,
                SurfaceMaterialSample::UNKNOWN,
                "",
                "",
                0.0,
            ),
            2.0927068140875886,
            6.557704304429194,
            -0.11223383026673162,
            0.96875,
            37.23935019238658,
            146,
            2,
        ))
        .unwrap();
        assert_eq!(
            java_standard_gray_photo_seed.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            java_standard_gray_photo_seed.biome_id,
            "minecraft:windswept_savanna"
        );
        assert_eq!(
            java_standard_gray_photo_seed.decision_source,
            "photo-palette"
        );

        let black_standard_shadow = apply_photo_surface_material(&PhotoSurfaceInput::new(
            jungle.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(0, 4, 14),
                RgbColor::of(0, 0, 0),
                TerrainTokenSource::JavaStandardPalette,
                2,
                18,
                8,
                0,
                0,
                24,
                12,
                -1,
                -1,
                28,
                -1,
                80,
                "dark montane forest",
                "minecraft:jungle",
                0.90,
            ),
            900.0,
            15.0,
            -2.0,
            0.0,
            120.0,
            300,
            232,
        ))
        .unwrap();
        assert!(matches!(
            black_standard_shadow.top_block_state_id,
            block_state_ids::BLACK_TERRACOTTA
                | block_state_ids::DEEPSLATE
                | block_state_ids::GRAY_TERRACOTTA
        ));

        let dry_savanna_dark_shadow = apply_photo_surface_material(&PhotoSurfaceInput::new(
            savanna.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(25, 35, 15),
                RgbColor::of(20, 20, 20),
                TerrainTokenSource::JavaStandardPalette,
                3,
                0,
                0,
                0,
                0,
                9,
                6,
                -1,
                -1,
                28,
                -1,
                55,
                "Guinean forest-savanna mosaic",
                "minecraft:savanna",
                0.88,
            ),
            240.0,
            12.0,
            8.0,
            0.0,
            28.0,
            1006,
            804,
        ))
        .unwrap();
        assert!(matches!(
            dry_savanna_dark_shadow.top_block_state_id,
            block_state_ids::BLACK_TERRACOTTA
                | block_state_ids::GRAY_TERRACOTTA
                | block_state_ids::DEEPSLATE
        ));

        let standard_grass_token_dry_open = apply_photo_surface_material(&PhotoSurfaceInput::new(
            savanna.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(143, 128, 82),
                RgbColor::of(167, 146, 103),
                TerrainTokenSource::JavaStandardPalette,
                3,
                0,
                0,
                0,
                0,
                24,
                18,
                -1,
                -1,
                28,
                -1,
                36,
                "West Sudanian savanna",
                "minecraft:savanna",
                0.95,
            ),
            260.0,
            16.0,
            10.0,
            0.0,
            30.0,
            318,
            236,
        ))
        .unwrap();
        assert!(matches!(
            standard_grass_token_dry_open.top_block_state_id,
            block_state_ids::GRASS_BLOCK
                | block_state_ids::MOSS_BLOCK
                | block_state_ids::PODZOL
                | block_state_ids::COARSE_DIRT
                | block_state_ids::ROOTED_DIRT
                | block_state_ids::MYCELIUM
                | block_state_ids::MUD
                | block_state_ids::PACKED_MUD
                | block_state_ids::MUD_BRICKS
                | block_state_ids::DRIPSTONE_BLOCK
                | block_state_ids::GREEN_TERRACOTTA
                | block_state_ids::LIME_TERRACOTTA
        ));

        let standard_grass_token_dark_dry = apply_photo_surface_material(&PhotoSurfaceInput::new(
            savanna.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(112, 100, 63),
                RgbColor::of(167, 146, 103),
                TerrainTokenSource::JavaStandardPalette,
                3,
                0,
                0,
                0,
                0,
                14,
                8,
                -1,
                -1,
                28,
                -1,
                50,
                "West Sudanian savanna",
                "minecraft:savanna",
                0.95,
            ),
            260.0,
            16.0,
            10.0,
            0.0,
            30.0,
            500,
            500,
        ))
        .unwrap();
        assert_ne!(
            standard_grass_token_dark_dry.top_block_state_id,
            block_state_ids::GRASS_BLOCK
        );
        assert!(
            photo_weighted_render_distance(
                RgbColor::of(112, 100, 63),
                standard_grass_token_dark_dry.top_block_state_id,
                &standard_grass_token_dark_dry.biome_id
            ) + 8.0
                < photo_weighted_render_distance(
                    RgbColor::of(112, 100, 63),
                    block_state_ids::GRASS_BLOCK,
                    "minecraft:savanna"
                )
        );

        let dark_standard_vegetation = apply_photo_surface_material(&PhotoSurfaceInput::new(
            jungle.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(35, 82, 32),
                RgbColor::of(50, 60, 30),
                TerrainTokenSource::JavaStandardPalette,
                2,
                32,
                10,
                0,
                0,
                30,
                12,
                -1,
                -1,
                28,
                -1,
                30,
                "Central Congolian lowland forests",
                "minecraft:jungle",
                0.95,
            ),
            300.0,
            20.0,
            -2.0,
            0.0,
            30.0,
            320,
            240,
        ))
        .unwrap();
        assert!(matches!(
            dark_standard_vegetation.top_block_state_id,
            block_state_ids::GRASS_BLOCK
                | block_state_ids::OAK_LEAVES
                | block_state_ids::JUNGLE_LEAVES
        ));

        let dark_olive_standard_vegetation = apply_photo_surface_material(&PhotoSurfaceInput::new(
            savanna.clone(),
            SurfaceMaterialSample::new(
                RgbColor::of(69, 99, 23),
                RgbColor::of(75, 85, 60),
                TerrainTokenSource::JavaStandardPalette,
                3,
                2,
                0,
                0,
                0,
                14,
                8,
                -1,
                -1,
                28,
                -1,
                42,
                "Guinean forest-savanna mosaic",
                "minecraft:savanna",
                0.90,
            ),
            240.0,
            12.0,
            8.0,
            0.0,
            28.0,
            824,
            534,
        ))
        .unwrap();
        assert!(
            dark_olive_standard_vegetation.top_block_state_id != block_state_ids::GRASS_BLOCK
                || !dark_olive_standard_vegetation.biome_id.contains("savanna")
        );

        let dark_standard_natural_shadow = apply_photo_surface_material(&PhotoSurfaceInput::new(
            jungle,
            SurfaceMaterialSample::new(
                RgbColor::of(22, 34, 13),
                RgbColor::of(20, 20, 20),
                TerrainTokenSource::JavaStandardPalette,
                2,
                24,
                12,
                0,
                0,
                30,
                12,
                -1,
                -1,
                28,
                -1,
                64,
                "dark montane forest",
                "minecraft:forest",
                0.90,
            ),
            720.0,
            55.0,
            9.0,
            0.0,
            90.0,
            220,
            -1160,
        ))
        .unwrap();
        assert_eq!(
            dark_standard_natural_shadow.top_block_state_id,
            block_state_ids::BLACK_TERRACOTTA
        );

        let gray_olive_source = RgbColor::of(140, 150, 110);
        let gray_olive_standard_carrier = apply_photo_surface_material(&PhotoSurfaceInput::new(
            savanna,
            SurfaceMaterialSample::new(
                gray_olive_source,
                RgbColor::of(140, 150, 110),
                TerrainTokenSource::JavaStandardPalette,
                3,
                0,
                0,
                0,
                0,
                14,
                8,
                -1,
                -1,
                28,
                -1,
                36,
                "dry open woodland",
                "minecraft:savanna",
                0.95,
            ),
            260.0,
            16.0,
            10.0,
            0.0,
            30.0,
            314,
            236,
        ))
        .unwrap();
        assert!(!is_tinted_vegetation_block(
            gray_olive_standard_carrier.top_block_state_id
        ));
        assert!(
            photo_weighted_render_distance(
                gray_olive_source,
                gray_olive_standard_carrier.top_block_state_id,
                &gray_olive_standard_carrier.biome_id
            ) < photo_weighted_render_distance(
                gray_olive_source,
                block_state_ids::GRASS_BLOCK,
                "minecraft:savanna"
            )
        );

        let marginal_gray_olive = apply_photo_surface_material(&PhotoSurfaceInput::new(
            surface_column(
                false,
                SEA_LEVEL_Y + 10,
                i32::MIN,
                block_state_ids::GRASS_BLOCK,
                block_state_ids::DIRT,
                "minecraft:savanna",
            ),
            SurfaceMaterialSample::new(
                RgbColor::of(120, 130, 77),
                RgbColor::of(140, 150, 110),
                TerrainTokenSource::JavaStandardPalette,
                3,
                0,
                0,
                0,
                0,
                14,
                8,
                -1,
                -1,
                28,
                -1,
                36,
                "dry open woodland",
                "minecraft:savanna",
                0.95,
            ),
            260.0,
            16.0,
            10.0,
            0.0,
            30.0,
            314,
            236,
        ))
        .unwrap();
        assert_ne!(marginal_gray_olive.decision_source, "photo-palette");
    }

    #[test]
    fn surface_material_sample_infers_ecoregion_confidence_like_java_overload() {
        let with_biome = SurfaceMaterialSample::land_with_inferred_ecoregion_confidence(
            RgbColor::of(42, 95, 38),
            2,
            20,
            15,
            10,
            8,
            30,
            5,
            -1,
            -1,
            -1,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Central Congolian lowland forests",
            " minecraft:jungle ",
        );
        assert_eq!(with_biome.ecoregion_confidence, 1.0);
        assert_eq!(with_biome.ecoregion_biome_id, "minecraft:jungle");

        let without_biome = SurfaceMaterialSample::land_with_inferred_ecoregion_confidence(
            RgbColor::of(42, 95, 38),
            2,
            20,
            15,
            10,
            8,
            30,
            5,
            -1,
            -1,
            -1,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "Central Congolian lowland forests",
            "   ",
        );
        assert_eq!(without_biome.ecoregion_confidence, 0.0);
        assert_eq!(without_biome.ecoregion_biome_id, "");
    }

    #[test]
    fn surface_data_evidence_flags_match_java_rules() {
        let sample = SurfaceMaterialSample::land(
            RgbColor::of(1, 2, 3),
            2,
            SurfaceMaterialSample::UNKNOWN,
            0,
            255,
            12,
            0,
            7,
            255,
            5,
            13,
            42,
            250,
            " ecoregion ",
            "",
            0.75,
        );

        let flags = surface_data_evidence::from_sample(&sample);
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::CLIMATE
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::TREE
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::TREE_PRESENT
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::HERBACEOUS
        ));
        assert!(!surface_data_evidence::has(
            flags,
            surface_data_evidence::HERBACEOUS_PRESENT
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::SHRUB
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::SHRUB_PRESENT
        ));
        assert!(!surface_data_evidence::has(
            flags,
            surface_data_evidence::SNOW
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::SWAMP
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::SWAMP_PRESENT
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::OCEAN_TEMPERATURE
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::BATHYMETRY
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::SLOPE
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::STEEP_SLOPE
        ));
        assert!(surface_data_evidence::has(
            flags,
            surface_data_evidence::ECOREGION
        ));
        assert!(surface_data_evidence::has_any_vegetation(flags));
        assert!(surface_data_evidence::has_any_vegetation_present(flags));
    }

    #[test]
    fn true_marble_surface_material_sampler_wraps_vrt_average_as_color_only_sample() {
        let root = std::env::temp_dir().join(format!(
            "earthmap-surface-true-marble-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let tiff = root.join("tiny.tif");
        std::fs::write(&tiff, synthetic_classic_rgb_tiff()).unwrap();
        let vrt = root.join("tiny.vrt");
        std::fs::write(
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

        let sampler = TrueMarbleSurfaceMaterialSampler::open(&vrt).unwrap();
        assert!(!sampler.samples_open_water());

        let sample = sampler.sample(1.25, 0.75, 0.0, 0.0).unwrap();
        assert_eq!(sample.color, RgbColor::of(70, 80, 90));
        assert_eq!(sample.terrain_token_color, RgbColor::unavailable());
        assert_eq!(sample.terrain_token_source, TerrainTokenSource::None);
        assert_eq!(sample.climate_class, SurfaceMaterialSample::UNKNOWN);

        assert_eq!(
            sampler.sample_photo(1.25, 0.75, 0.0, 0.0).unwrap().color,
            RgbColor::of(70, 80, 90)
        );
        assert_eq!(
            sampler.sample_water(1.25, 0.75, 0.0, 0.0).unwrap().color,
            RgbColor::of(70, 80, 90)
        );
        assert_eq!(sampler.reader().stats().sample_averaged_requests, 3);

        drop(sampler);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn photo_source_preference_matches_java_property_and_environment_rules() {
        assert!(!photo_source_prefers_topographic("", ""));
        assert!(!photo_source_prefers_topographic("   ", "satellite"));
        assert!(photo_source_prefers_topographic("topo", ""));
        assert!(photo_source_prefers_topographic(" topographic ", ""));
        assert!(photo_source_prefers_topographic("", "land-shallow-topo"));
        assert!(photo_source_prefers_topographic("", "land_shallow_topo"));
        assert!(photo_source_prefers_topographic("topo", "satellite"));
        assert!(!photo_source_prefers_topographic("satellite", "topo"));
    }

    #[test]
    fn land_shallow_topo_photo_sampler_matches_java_half_world_mapping() {
        let root = std::env::temp_dir().join(format!(
            "earthmap-surface-land-shallow-topo-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let tif_root = root.join("TifFiles");
        let terrain = tif_root.join("terrain");
        std::fs::create_dir_all(&terrain).unwrap();
        let true_marble = terrain.join("TrueMarble.vrt");
        std::fs::write(&true_marble, b"placeholder").unwrap();
        std::fs::write(
            tif_root.join("land_shallow_topo_west.tif"),
            synthetic_classic_rgb_tiff(),
        )
        .unwrap();
        std::fs::write(
            tif_root.join("land_shallow_topo_east.tif"),
            synthetic_classic_rgb_tiff(),
        )
        .unwrap();

        let sampler = LandShallowTopoPhotoSampler::open_near(&true_marble)
            .unwrap()
            .expect("both land-shallow topo halves should be discovered");
        assert_eq!(
            sampler.sample_nearest(45.0, 45.0).unwrap(),
            RgbColor::of(10, 20, 30)
        );
        assert_eq!(
            sampler.sample_nearest(-45.0, -45.0).unwrap(),
            RgbColor::of(100, 110, 120)
        );
        assert_eq!(
            sampler.sample_averaged(45.0, 45.0, 0.0, 0.0).unwrap(),
            RgbColor::of(10, 20, 30)
        );

        drop(sampler);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn met_image_export_terrain_sampler_matches_java_tile_discovery_and_sampling() {
        let root = std::env::temp_dir().join(format!(
            "earthmap-surface-met-terrain-sampler-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let terrain = root.join("TifFiles").join("terrain");
        std::fs::create_dir_all(&terrain).unwrap();
        let true_marble = terrain.join("TrueMarble.vrt");
        std::fs::write(
            &true_marble,
            r#"<VRTDataset rasterXSize="1" rasterYSize="1" />"#,
        )
        .unwrap();

        let tile = root.join("image_exports").join("N10E020");
        std::fs::create_dir_all(tile.join("heightmap")).unwrap();
        write_synthetic_met_png(&tile.join("N10E020_terrain_reduced_colors.png"), 4, 4);
        write_synthetic_met_png(&tile.join("heightmap").join("N10E020_exported.png"), 4, 4);
        std::fs::write(
            tile.join("heightmap").join("N10E020_exported.png.aux.xml"),
            r#"
<PAMDataset>
  <GeoTransform> 20.0, 0.25, 0.0, 11.0, 0.0, -0.25</GeoTransform>
</PAMDataset>
"#,
        )
        .unwrap();

        let unsafe_tile = root.join("image_exports").join("N12E022");
        std::fs::create_dir_all(&unsafe_tile).unwrap();
        write_synthetic_met_png(
            &unsafe_tile.join("N12E022_terrain_reduced_colors.png"),
            1024,
            1024,
        );

        let terrain_only_tile = root.join("image_exports").join("N13E023");
        std::fs::create_dir_all(&terrain_only_tile).unwrap();
        write_synthetic_met_png(&terrain_only_tile.join("N13E023_terrain.png"), 512, 512);

        let sampler = MetImageExportTerrainSampler::open_auto(&true_marble)
            .unwrap()
            .expect("sampler should discover aux-backed MET terrain tile");
        assert!(sampler.tile_count() >= 2);
        assert_eq!(
            sampler.sample(20.10, 10.90).unwrap(),
            RgbColor::of(167, 146, 103)
        );
        assert_eq!(
            sampler.sample(20.90, 10.10).unwrap(),
            RgbColor::of(230, 205, 160)
        );
        assert_eq!(
            sampler.sample(23.10, 13.90).unwrap(),
            RgbColor::of(167, 146, 103)
        );
        assert!(!sampler.sample(22.10, 12.90).unwrap().available);

        drop(sampler);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn met_aux_geo_transform_decodes_xml_entities_like_java_dom() {
        let root = std::env::temp_dir().join(format!(
            "earthmap-surface-met-aux-entities-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let aux = root.join("tile.png.aux.xml");
        std::fs::write(
            &aux,
            r#"
<PAMDataset>
  <GeoTransform> 20&#46;0, 0&#x2e;25, 0, 11&#46;0, 0, -0&#46;25</GeoTransform>
</PAMDataset>
"#,
        )
        .unwrap();

        let transform = read_met_geo_transform(&aux).unwrap().unwrap();
        assert_eq!(transform.origin_longitude, 20.0);
        assert_eq!(transform.pixel_longitude, 0.25);
        assert_eq!(transform.origin_latitude, 11.0);
        assert_eq!(transform.pixel_latitude, -0.25);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn earth_data_surface_material_sampler_samples_optional_rasters_and_caches() {
        let root = std::env::temp_dir().join(format!(
            "earthmap-surface-earth-data-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let tif_root = root.join("TifFiles");
        let terrain = tif_root.join("terrain");
        let vegetation = tif_root.join("vegetation");
        let shape_dir = root.join("ShapeFiles").join("ecoregionsOrig");
        std::fs::create_dir_all(&terrain).unwrap();
        std::fs::create_dir_all(&vegetation).unwrap();
        std::fs::create_dir_all(&shape_dir).unwrap();
        std::fs::create_dir_all(root.join(".earthmap-cache")).unwrap();
        let met_tile = root.join("image_exports").join("N00E001");
        std::fs::create_dir_all(met_tile.join("heightmap")).unwrap();
        write_synthetic_met_png(&met_tile.join("N00E001_terrain_reduced_colors.png"), 4, 4);
        write_synthetic_met_png(
            &met_tile.join("heightmap").join("N00E001_exported.png"),
            4,
            4,
        );
        std::fs::write(
            met_tile
                .join("heightmap")
                .join("N00E001_exported.png.aux.xml"),
            r#"
<PAMDataset>
  <GeoTransform> 1.0, 0.25, 0.0, 1.0, 0.0, -0.25</GeoTransform>
</PAMDataset>
"#,
        )
        .unwrap();
        std::fs::write(terrain.join("tiny.tif"), synthetic_classic_rgb_tiff()).unwrap();
        std::fs::write(shape_dir.join("wwf_terr_ecos.shp"), b"shape placeholder").unwrap();
        std::fs::write(shape_dir.join("wwf_terr_ecos.dbf"), b"dbf placeholder").unwrap();
        std::fs::write(root.join("ecoregions.csv"), b"Cache Jungle,JUNGLE\n").unwrap();
        std::fs::write(
            root.join(".earthmap-cache").join("wwf-ecoregions-v2.bin"),
            synthetic_wwf_ecoregion_cache(),
        )
        .unwrap();
        std::fs::write(
            terrain.join("TrueMarble.vrt"),
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
        for (path, value) in [
            (tif_root.join("climate.tif"), 2),
            (vegetation.join("EvergreenBroadleafTrees.tif"), 10),
            (vegetation.join("DeciduousBroadleafTrees.tif"), 20),
            (vegetation.join("EvergreenDeciduousNeedleleafTrees.tif"), 30),
            (vegetation.join("mixed.tif"), 40),
            (vegetation.join("HerbaceousVegetation.tif"), 50),
            (vegetation.join("Shrubs.tif"), 60),
            (vegetation.join("Snow.tif"), 70),
            (vegetation.join("Swamp.tif"), 80),
            (tif_root.join("ocean_temp_infill.tif"), 12),
        ] {
            std::fs::write(path, synthetic_classic_single_band_tiff(value)).unwrap();
        }
        std::fs::write(
            tif_root.join("slope.tif"),
            synthetic_bigtiff_float32_tiff(45.0),
        )
        .unwrap();
        std::fs::write(
            tif_root.join("bathymetry.tif"),
            synthetic_bigtiff_float32_tiff(-123.4),
        )
        .unwrap();

        let sampler =
            EarthDataSurfaceMaterialSampler::open(terrain.join("TrueMarble.vrt")).unwrap();
        assert!(sampler.samples_open_water());

        let sample = sampler.sample(1.25, 0.75, 0.0, 0.0).unwrap();
        assert_eq!(sample.color, RgbColor::of(70, 80, 90));
        assert_eq!(sample.terrain_token_color, RgbColor::of(167, 146, 103));
        assert_eq!(sample.terrain_token_source, TerrainTokenSource::Export);
        assert_eq!(sample.climate_class, 2);
        assert_eq!(sample.evergreen_broadleaf_trees, 10);
        assert_eq!(sample.deciduous_broadleaf_trees, 20);
        assert_eq!(sample.needleleaf_trees, 30);
        assert_eq!(sample.mixed_trees, 40);
        assert_eq!(sample.herbaceous_vegetation, 50);
        assert_eq!(sample.shrubs, 60);
        assert_eq!(sample.snow_cover, 70);
        assert_eq!(sample.swamp_cover, 80);
        assert_eq!(sample.ocean_temperature, 12);
        assert_eq!(sample.bathymetry_meters, -123);
        assert_eq!(sample.slope_permille, 500);
        assert_eq!(sample.ecoregion_name, "Cache Jungle");
        assert_eq!(sample.ecoregion_biome_id, "minecraft:jungle");
        assert_eq!(sample.ecoregion_confidence, 1.0);
        assert_eq!(sampler.raster_stats().sample_averaged_requests, 1);

        assert_eq!(sampler.sample(1.25, 0.75, 0.0, 0.0).unwrap(), sample);
        assert_eq!(sampler.raster_stats().sample_averaged_requests, 1);

        let water = sampler.sample_water(1.25, 0.75, 0.0, 0.0).unwrap();
        assert_eq!(water.color, RgbColor::of(70, 80, 90));
        assert_eq!(water.terrain_token_color, RgbColor::of(167, 146, 103));
        assert_eq!(water.terrain_token_source, TerrainTokenSource::Export);
        assert_eq!(water.climate_class, SurfaceMaterialSample::UNKNOWN);
        assert_eq!(water.ocean_temperature, 12);
        assert_eq!(water.bathymetry_meters, -123);
        assert_eq!(water.slope_permille, SurfaceMaterialSample::UNKNOWN);
        assert_eq!(sampler.raster_stats().sample_averaged_requests, 2);

        assert_eq!(sampler.sample_water(1.25, 0.75, 0.0, 0.0).unwrap(), water);
        assert_eq!(sampler.raster_stats().sample_averaged_requests, 2);

        let photo = sampler.sample_photo(1.25, 0.75, 0.0, 0.0).unwrap();
        assert_eq!(photo.color, RgbColor::of(70, 80, 90));
        assert_eq!(photo.terrain_token_color, RgbColor::of(167, 146, 103));
        assert_eq!(photo.terrain_token_source, TerrainTokenSource::Export);
        assert_eq!(photo.climate_class, 2);
        assert_eq!(photo.ecoregion_name, "Cache Jungle");
        assert_eq!(photo.ecoregion_biome_id, "minecraft:jungle");
        assert_eq!(photo.ecoregion_confidence, 1.0);
        assert_eq!(sampler.raster_stats().sample_averaged_requests, 3);

        assert_eq!(sampler.sample_photo(1.25, 0.75, 0.0, 0.0).unwrap(), photo);
        assert_eq!(sampler.raster_stats().sample_averaged_requests, 3);

        drop(sampler);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn optional_surface_raster_open_failures_are_tolerated_like_java() {
        let root = std::env::temp_dir().join(format!(
            "earthmap-surface-optional-raster-failure-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let name = format!("malformed-optional-{}.tif", std::process::id());
        std::fs::write(root.join(&name), b"not a tiff").unwrap();

        assert!(
            open_surface_raster(Some(&root), &name, DEFAULT_SURFACE_TILE_CACHE_ENTRIES)
                .unwrap()
                .is_none()
        );
        assert!(open_surface_float_raster(Some(&root), &name)
            .unwrap()
            .is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn bounded_surface_material_cache_evicts_by_access_order_like_java_linked_hash_map() {
        let mut cache = BoundedAccessCache::new(2);
        let first = SurfaceMaterialSample::color_only(RgbColor::of(1, 1, 1));
        let second = SurfaceMaterialSample::color_only(RgbColor::of(2, 2, 2));
        let third = SurfaceMaterialSample::color_only(RgbColor::of(3, 3, 3));

        assert_eq!(cache.insert_or_get(1, first.clone()), first);
        assert_eq!(cache.insert_or_get(2, second.clone()), second);
        assert_eq!(cache.get(1), Some(first.clone()));
        assert_eq!(cache.insert_or_get(3, third.clone()), third);

        assert_eq!(cache.get(2), None);
        assert_eq!(cache.get(1), Some(first));
        assert_eq!(cache.get(3), Some(third));
    }

    #[test]
    fn ecoregion_sample_and_evidence_match_java_family_confidence_rules() {
        let trimmed = EcoregionSample::new("  Name  ", " minecraft:forest ");
        assert_eq!(trimmed.name, "Name");
        assert_eq!(trimmed.biome_id, "minecraft:forest");
        assert!(trimmed.available());
        assert!(trimmed.has_biome());
        assert!(!EcoregionSample::unknown().available());
        assert!(!EcoregionSample::new("Name", "").has_biome());

        assert_eq!(biome_family("minecraft:snowy_plains"), "snow");
        assert_eq!(biome_family("minecraft:frozen_river"), "snow");
        assert_eq!(biome_family("minecraft:meadow"), "grassland");
        assert_eq!(
            biome_family("minecraft:mushroom_fields"),
            "minecraft:mushroom_fields"
        );
        assert_eq!(ecoregion_cell_degrees(0.0, 0.0), 0.012);
        assert_eq!(ecoregion_cell_degrees(0.02, 0.0), 0.032);
        assert_eq!(ecoregion_cell_degrees(1.0, 0.0), 0.045);

        let evidence = sample_ecoregion_evidence(&FixtureEcoregionSampler, 0.0, 0.0, 0.012);
        assert_eq!(evidence.sample.name, "Center Jungle");
        assert_eq!(evidence.sample.biome_id, "minecraft:jungle");
        assert!((evidence.confidence - (2.0 / 3.0)).abs() < 0.0001);

        let nan_latitude_evidence =
            sample_ecoregion_evidence(&FixtureEcoregionSampler, 0.0, f64::NAN, 0.012);
        assert!((nan_latitude_evidence.confidence - (2.0 / 3.0)).abs() < 0.0001);

        let unknown = sample_ecoregion_evidence(&UnknownEcoregionSampler, 0.0, 0.0, 0.012);
        assert_eq!(unknown, EcoregionEvidence::unknown());
    }

    #[test]
    fn wwf_ecoregion_sampler_reads_java_cache_fixture() {
        let root =
            std::env::temp_dir().join(format!("earthmap-surface-wwf-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cache = root.join("wwf-ecoregions-v2.bin");
        std::fs::write(&cache, synthetic_wwf_ecoregion_cache()).unwrap();

        let sampler = WwfEcoregionSampler::open_cache(&cache).unwrap();
        assert_eq!(
            sampler.sample(1.0, 1.0),
            EcoregionSample::new("Cache Jungle", "minecraft:jungle")
        );
        assert_eq!(
            sampler.sample(361.0, 1.0),
            EcoregionSample::new("Cache Jungle", "minecraft:jungle")
        );
        assert_eq!(sampler.sample(3.0, 1.0), EcoregionSample::unknown());
        assert_eq!(sampler.sample(f64::NAN, 1.0), EcoregionSample::unknown());
        assert_eq!(sampler.sample(1.0, 91.0), EcoregionSample::unknown());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn wwf_ecoregion_sampler_regenerates_java_cache_from_source_fixture() {
        let root = std::env::temp_dir().join(format!(
            "earthmap-surface-wwf-source-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let terrain = root.join("TifFiles").join("terrain");
        let shape_dir = root.join("ShapeFiles").join("ecoregionsOrig");
        std::fs::create_dir_all(&terrain).unwrap();
        std::fs::create_dir_all(&shape_dir).unwrap();
        let true_marble = terrain.join("TrueMarble.vrt");
        std::fs::write(&true_marble, b"placeholder").unwrap();
        let shape = shape_dir.join("wwf_terr_ecos.shp");
        std::fs::write(&shape, synthetic_wwf_polygon_shapefile()).unwrap();
        std::fs::write(shape_dir.join("wwf_terr_ecos.dbf"), synthetic_wwf_dbf()).unwrap();
        std::fs::write(root.join("ecoregions.csv"), b"Cache Jungle,JUNGLE\n").unwrap();
        let cache = root.join(".earthmap-cache").join("wwf-ecoregions-v2.bin");
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&cache, b"corrupt").unwrap();

        let sampler = WwfEcoregionSampler::open_auto_cache(&true_marble)
            .unwrap()
            .expect("WWF source fixture should be discovered");
        assert_eq!(
            sampler.sample(1.0, 1.0),
            EcoregionSample::new("Cache Jungle", "minecraft:jungle")
        );
        assert_eq!(sampler.sample(3.0, 1.0), EcoregionSample::unknown());
        assert_eq!(
            sampler.sample(5.0, 1.0),
            EcoregionSample::new("Cache Jungle", "minecraft:jungle")
        );

        assert!(cache.is_file());
        assert_ne!(std::fs::read(&cache).unwrap(), b"corrupt");
        let cached = WwfEcoregionSampler::open_cache(&cache).unwrap();
        assert_eq!(
            cached.sample(1.0, 1.0),
            EcoregionSample::new("Cache Jungle", "minecraft:jungle")
        );
        assert_eq!(
            cached.sample(5.0, 1.0),
            EcoregionSample::new("Cache Jungle", "minecraft:jungle")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn wwf_text_decoding_matches_java_trim_and_windows_1252_edges() {
        assert_eq!(java_string_trim(" \tCache Jungle\r\n"), "Cache Jungle");
        assert_eq!(
            java_string_trim("\u{00a0}Cache Jungle\u{00a0}"),
            "\u{00a0}Cache Jungle\u{00a0}"
        );
        assert!(java_string_is_blank(" \t\r\n"));
        assert!(!java_string_is_blank("\u{00a0}"));

        let decoded = decode_wwf_dbf_text(&[b' ', 0xa0, b'N', 0x81, b' '], 0, 5).unwrap();
        assert_eq!(decoded, "\u{00a0}N\u{fffd}");
        assert_eq!(
            EcoregionSample::new("\u{00a0}Cache Jungle\u{00a0}", " minecraft:jungle ").name,
            "\u{00a0}Cache Jungle\u{00a0}"
        );

        let root =
            std::env::temp_dir().join(format!("earthmap-surface-wwf-text-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mapping = root.join("ecoregions.csv");
        std::fs::write(&mapping, "\u{00a0}Cache Jungle\u{00a0}, COLD_BEACH\n").unwrap();
        let mappings = read_wwf_biome_mapping(&mapping).unwrap();
        assert_eq!(
            mappings.get("\u{00a0}Cache Jungle\u{00a0}"),
            Some(&"minecraft:snowy_beach".to_string())
        );
        assert!(!mappings.contains_key("Cache Jungle"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn wwf_cache_freshness_uses_java_millisecond_precision() {
        let same_java_millis = std::time::UNIX_EPOCH + std::time::Duration::new(123, 999_999);
        let next_java_millis = std::time::UNIX_EPOCH + std::time::Duration::new(123, 1_000_000);

        assert_eq!(system_time_to_java_millis(same_java_millis), Some(123_000));
        assert_eq!(system_time_to_java_millis(next_java_millis), Some(123_001));
    }

    #[test]
    fn earth_data_sampler_helper_math_matches_java_contract() {
        assert_eq!(
            normalized_slope_permille(f64::NAN),
            SurfaceMaterialSample::UNKNOWN
        );
        assert_eq!(
            normalized_slope_permille(-0.1),
            SurfaceMaterialSample::UNKNOWN
        );
        assert_eq!(normalized_slope_permille(0.0), 0);
        assert_eq!(normalized_slope_permille(0.5), 500);
        assert_eq!(normalized_slope_permille(45.0), 500);
        assert_eq!(normalized_slope_permille(90.0), 1000);
        assert_eq!(normalized_slope_permille(10_000.0), 500);

        assert_eq!(normalize_longitude(181.0), -179.0);
        assert_eq!(normalize_longitude(-181.0), 179.0);
        assert!(normalize_longitude(f64::NAN).is_nan());

        assert_eq!(material_cell_degrees(0.0, 0.0), 0.012);
        assert_eq!(material_cell_degrees(0.02, 0.01), 0.04);
        assert_eq!(material_cell_degrees(1.0, 0.0), 0.060);
        assert_eq!(photo_cell_degrees(0.001, 0.0), 0.0030);
        assert_eq!(photo_cell_degrees(0.010, 0.0), 0.0275);
        assert_eq!(photo_average_span_degrees(1.0), 0.060);
        assert_eq!(photo_evidence_cell_degrees(0.001, 0.0), 0.030);
        assert_eq!(photo_evidence_cell_degrees(0.010, 0.0), 0.050);
        assert_eq!(photo_evidence_cell_degrees(1.0, 0.0), 0.090);

        let cell = quantized_cell(181.0, 91.0, 0.5);
        assert_eq!(cell.center_longitude, -178.75);
        assert_eq!(cell.center_latitude, 90.25);
        assert_eq!(cell.key, (5_000_i64 << 48) ^ (2_i64 << 24) ^ 360_i64);

        let nan_cell = quantized_cell(f64::NAN, f64::NAN, 0.5);
        assert_eq!(nan_cell.key, 5_000_i64 << 48);
        assert_eq!(nan_cell.center_longitude, -179.75);
        assert_eq!(nan_cell.center_latitude, -89.75);
    }

    #[test]
    fn surface_material_raster_stats_minus_matches_java_saturating_delta() {
        assert_eq!(
            SurfaceMaterialRasterStats::EMPTY,
            SurfaceMaterialRasterStats {
                source_count: 0,
                open_readers: 0,
                resident_tiles: 0,
                tile_hits: 0,
                tile_misses: 0,
                tile_evictions: 0,
                sample_nearest_requests: 0,
                sample_averaged_requests: 0,
            }
        );

        let current = SurfaceMaterialRasterStats {
            source_count: 3,
            open_readers: 2,
            resident_tiles: 10,
            tile_hits: 12,
            tile_misses: 4,
            tile_evictions: 1,
            sample_nearest_requests: 2,
            sample_averaged_requests: 20,
        };
        let previous = SurfaceMaterialRasterStats {
            source_count: 99,
            open_readers: 88,
            resident_tiles: 12,
            tile_hits: 2,
            tile_misses: 5,
            tile_evictions: 1,
            sample_nearest_requests: 7,
            sample_averaged_requests: 8,
        };

        assert_eq!(
            current.minus(previous),
            SurfaceMaterialRasterStats {
                source_count: 3,
                open_readers: 2,
                resident_tiles: 0,
                tile_hits: 10,
                tile_misses: 0,
                tile_evictions: 0,
                sample_nearest_requests: 0,
                sample_averaged_requests: 12,
            }
        );
    }

    #[test]
    fn met_terrain_vocabulary_matches_java_exact_and_remap_contracts() {
        assert_eq!(
            MetTerrainVocabulary::exact(RgbColor::unavailable()),
            MetTerrainMatch::unavailable()
        );
        assert_eq!(
            MetTerrainVocabulary::nearest(RgbColor::unavailable()),
            MetTerrainMatch::unavailable()
        );

        let exact_sand = MetTerrainVocabulary::exact(RgbColor::of(255, 200, 64));
        assert_eq!(exact_sand.kind, MetTerrainKind::Sand);
        assert_eq!(exact_sand.top_block_state_id, block_state_ids::SAND);
        assert_eq!(exact_sand.distance_squared, 0);
        assert!(exact_sand.confident());
        assert_eq!(exact_sand.color(), RgbColor::of(255, 200, 64));

        let near_sand = MetTerrainVocabulary::nearest(RgbColor::of(254, 201, 63));
        assert_eq!(near_sand.kind, MetTerrainKind::Sand);
        assert_eq!(near_sand.top_block_state_id, block_state_ids::SAND);
        assert_eq!(near_sand.distance_squared, 3);
        assert_eq!(near_sand.color(), RgbColor::of(255, 200, 64));
        assert!(near_sand.confident());

        let dry_grass = MetTerrainVocabulary::nearest(RgbColor::of(167, 146, 103));
        assert_eq!(dry_grass.kind, MetTerrainKind::Vegetated);
        assert_eq!(dry_grass.top_block_state_id, block_state_ids::GRASS_BLOCK);
        assert_eq!(dry_grass.distance_squared, 0);
        assert!(dry_grass.confident());

        let near_red_sand = MetTerrainVocabulary::nearest(RgbColor::of(180, 120, 70));
        assert_eq!(near_red_sand.kind, MetTerrainKind::RedSand);
        assert_eq!(near_red_sand.top_block_state_id, block_state_ids::RED_SAND);

        let exact_unknown = MetTerrainVocabulary::exact(RgbColor::of(1, 2, 3));
        assert_eq!(exact_unknown.kind, MetTerrainKind::Unknown);
        assert!(!exact_unknown.confident());
        assert_eq!(exact_unknown.color(), RgbColor::unavailable());
    }

    #[test]
    fn surface_terrain_token_synthesis_matches_java_with_terrain_token() {
        let sample = SurfaceMaterialSample::land(
            RgbColor::of(254, 201, 63),
            2,
            10,
            20,
            30,
            40,
            50,
            60,
            70,
            80,
            90,
            100,
            110,
            "ecoregion",
            "minecraft:desert",
            0.75,
        );

        let exported = with_surface_terrain_token(&sample, RgbColor::of(1, 2, 3));
        assert_eq!(exported.terrain_token_color, RgbColor::of(1, 2, 3));
        assert_eq!(exported.terrain_token_source, TerrainTokenSource::Export);
        assert_eq!(exported.color, sample.color);
        assert_eq!(exported.ecoregion_name, "ecoregion");

        let synthetic = with_surface_terrain_token(&sample, RgbColor::unavailable());
        assert_eq!(synthetic.terrain_token_color, RgbColor::of(255, 200, 64));
        assert_eq!(
            synthetic.terrain_token_source,
            TerrainTokenSource::JavaStandardPalette
        );
        assert_eq!(synthetic.climate_class, 2);
        assert_eq!(synthetic.ecoregion_biome_id, "minecraft:desert");
        assert_eq!(synthetic.ecoregion_confidence, 0.75);

        let unavailable = SurfaceMaterialSample::color_only(RgbColor::unavailable());
        assert_eq!(
            with_surface_terrain_token(&unavailable, RgbColor::unavailable()),
            unavailable
        );

        let existing_token = SurfaceMaterialSample::with_export_token(
            RgbColor::unavailable(),
            RgbColor::of(9, 9, 9),
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            SurfaceMaterialSample::UNKNOWN,
            "",
            "",
            0.0,
        );
        let rebuilt = with_surface_terrain_token(&existing_token, RgbColor::unavailable());
        assert_eq!(rebuilt.terrain_token_color, RgbColor::unavailable());
        assert_eq!(rebuilt.terrain_token_source, TerrainTokenSource::None);
        assert_eq!(rebuilt.color, RgbColor::unavailable());
    }

    #[test]
    fn surface_y_matches_java_height_only_rounding_and_clamps() {
        assert_eq!(surface_y_for_elevation_meters(0.0), 63);
        assert_eq!(surface_y_for_elevation_meters(17.5), 64);
        assert_eq!(surface_y_for_elevation_meters(-17.5), 63);
        assert_eq!(surface_y_for_elevation_meters(-18.0), 62);
        assert_eq!(surface_y_for_elevation_meters(-10_000.0), -60);
        assert_eq!(surface_y_for_elevation_meters(20_000.0), 319);
        for elevation in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.0e30, -1.0e30] {
            assert_eq!(
                surface_y_for_elevation_meters(elevation),
                java_height_only_surface_y_reference(elevation)
            );
        }
        assert_eq!(surface_y_for_elevation_meters(f64::NAN), 63);
        assert_eq!(surface_y_for_elevation_meters(f64::INFINITY), 62);
        assert_eq!(surface_y_for_elevation_meters(f64::NEG_INFINITY), 63);
    }

    #[test]
    fn fill_height_only_column_matches_java_layers() {
        let mut chunk = ChunkModel::overworld(0, 0);

        fill_height_only_column(&mut chunk, 1, 2, 63).unwrap();

        assert_eq!(
            chunk.get_block_state_id(1, -64, 2).unwrap(),
            block_state_ids::BEDROCK
        );
        assert_eq!(
            chunk.get_block_state_id(1, 59, 2).unwrap(),
            block_state_ids::STONE
        );
        assert_eq!(
            chunk.get_block_state_id(1, 60, 2).unwrap(),
            block_state_ids::DIRT
        );
        assert_eq!(
            chunk.get_block_state_id(1, 62, 2).unwrap(),
            block_state_ids::DIRT
        );
        assert_eq!(
            chunk.get_block_state_id(1, 63, 2).unwrap(),
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            chunk.get_block_state_id(1, 64, 2).unwrap(),
            block_state_ids::AIR
        );
    }

    #[test]
    fn exploration_manifest_values_match_java_height_only_overrides() {
        let settings = HeightOnlySettings::new(
            "height.tif",
            "world",
            "SR EarthMap Height Only",
            0,
            5000,
            -1,
            2,
            OutputFormat::LinearV2,
            DEFAULT_HEIGHT_ONLY_CACHE_ROWS,
        )
        .unwrap();

        let mut values = base_exploration_only_manifest("height-only-region");
        values.insert("features.terrainHeight".to_string(), "true".to_string());
        values.insert(
            "generation.format".to_string(),
            settings.output_format.java_name().to_string(),
        );
        values.insert(
            "generation.scaleDenominator".to_string(),
            settings.scale_denominator.to_string(),
        );
        values.insert(
            "generation.regionX".to_string(),
            settings.region_x.to_string(),
        );
        values.insert(
            "generation.regionZ".to_string(),
            settings.region_z.to_string(),
        );

        assert_eq!(values["gameplay.claim"], "exploration-only");
        assert_eq!(values["features.terrainHeight"], "true");
        assert_eq!(values["features.ores"], "false");
        assert_eq!(values["features.strongholdOrEquivalent"], "false");
        assert_eq!(values["generation.format"], "LINEAR_V2");
    }

    #[derive(Debug)]
    struct FixtureEcoregionSampler;

    impl EcoregionSampler for FixtureEcoregionSampler {
        fn sample(&self, longitude: f64, _latitude: f64) -> EcoregionSample {
            if longitude > 0.30 && longitude < 1.0 {
                EcoregionSample::new("Neighbor Forest", "minecraft:forest")
            } else {
                EcoregionSample::new("Center Jungle", "minecraft:jungle")
            }
        }
    }

    #[derive(Debug)]
    struct UnknownEcoregionSampler;

    impl EcoregionSampler for UnknownEcoregionSampler {
        fn sample(&self, _longitude: f64, _latitude: f64) -> EcoregionSample {
            EcoregionSample::unknown()
        }
    }

    fn surface_column(
        water: bool,
        ground_surface_y: i32,
        water_surface_y: i32,
        top: i32,
        filler: i32,
        biome: &str,
    ) -> EarthSurfaceColumn {
        EarthSurfaceColumn::new(
            water,
            ground_surface_y,
            water_surface_y,
            top,
            filler,
            biome,
            "test",
        )
    }

    fn land_surface_column(top: i32, biome: &str, y: i32) -> EarthSurfaceColumn {
        surface_column(false, y, i32::MIN, top, top, biome)
    }

    fn photo_texture_surface_column(top: i32, filler: i32, biome: &str) -> EarthSurfaceColumn {
        surface_column(false, 72, i32::MIN, top, filler, biome)
    }

    fn photo_palette_surface_column(top: i32, filler: i32, biome: &str) -> EarthSurfaceColumn {
        surface_column(false, SEA_LEVEL_Y + 4, i32::MIN, top, filler, biome)
            .with_decision_source("photo-palette")
            .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette)
    }

    fn water_surface_column() -> EarthSurfaceColumn {
        surface_column(
            true,
            SEA_LEVEL_Y - 4,
            SEA_LEVEL_Y,
            block_state_ids::CLAY,
            block_state_ids::CLAY,
            "minecraft:ocean",
        )
    }

    fn is_boolean_parity_checkerboard(grid: &[[bool; 4]; 4]) -> bool {
        let first = grid[0][0];
        for (z, row) in grid.iter().enumerate() {
            for (x, &value) in row.iter().enumerate() {
                let expected = if ((x + z) & 1) == 0 { first } else { !first };
                if value != expected {
                    return false;
                }
            }
        }
        true
    }

    fn is_i32_parity_checkerboard(grid: &[[i32; 4]; 4]) -> bool {
        let first = grid[0][0];
        let second = grid[0][1];
        if first == second {
            return false;
        }
        for (z, row) in grid.iter().enumerate() {
            for (x, &value) in row.iter().enumerate() {
                let expected = if ((x + z) & 1) == 0 { first } else { second };
                if value != expected {
                    return false;
                }
            }
        }
        true
    }

    fn apply_test_surface_material(
        base: &EarthSurfaceColumn,
        color: RgbColor,
        elevation_meters: f64,
        longitude: f64,
        latitude: f64,
        coast_factor: f64,
        local_relief_meters: f64,
    ) -> EarthSurfaceColumn {
        apply_surface_material(
            base,
            &SurfaceMaterialSample::color_only(color),
            elevation_meters,
            longitude,
            latitude,
            coast_factor,
            local_relief_meters,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap()
    }

    fn apply_test_surface_material_sample(
        base: &EarthSurfaceColumn,
        sample: SurfaceMaterialSample,
        elevation_meters: f64,
        longitude: f64,
        latitude: f64,
        coast_factor: f64,
        local_relief_meters: f64,
    ) -> EarthSurfaceColumn {
        apply_surface_material(
            base,
            &sample,
            elevation_meters,
            longitude,
            latitude,
            coast_factor,
            local_relief_meters,
            DEFAULT_VERTICAL_SCALE,
        )
        .unwrap()
    }

    fn filled_surface_columns(
        width: usize,
        height: usize,
        column: EarthSurfaceColumn,
    ) -> Vec<EarthSurfaceColumn> {
        vec![column; width * height]
    }

    fn clean_single_coastal_column(
        column: EarthSurfaceColumn,
        coast_factor: f64,
    ) -> EarthSurfaceColumn {
        clean_coastal_surface_columns(&[column], &[coast_factor], 1)
            .unwrap()
            .remove(0)
    }

    fn surface_biome_arrays(default_biome: &str, y: i32) -> (Vec<String>, Vec<i32>, Vec<i32>) {
        let columns = CHUNK_WIDTH * CHUNK_WIDTH;
        (
            vec![default_biome.to_string(); columns],
            vec![y; columns],
            vec![y; columns],
        )
    }

    fn local_column_index(x: usize, z: usize) -> usize {
        (z * CHUNK_WIDTH) + x
    }

    fn fill_surface_top(chunk: &mut ChunkModel, y: i32, block: i32) {
        for z in 0..CHUNK_WIDTH {
            for x in 0..CHUNK_WIDTH {
                chunk
                    .set_block_state_id(x as i32, y, z as i32, block)
                    .unwrap();
            }
        }
    }

    fn require_natural_ground(block: i32, biome: &str) {
        assert!(
            is_allowed_production_top(block, biome),
            "block should be production-safe natural ground: {block}"
        );
        assert!(
            !matches!(
                block,
                block_state_ids::OAK_LEAVES
                    | block_state_ids::JUNGLE_LEAVES
                    | block_state_ids::DARK_OAK_LEAVES
                    | block_state_ids::SPRUCE_LEAVES
                    | block_state_ids::TERRACOTTA
                    | block_state_ids::ORANGE_TERRACOTTA
                    | block_state_ids::BROWN_TERRACOTTA
                    | block_state_ids::RED_TERRACOTTA
                    | block_state_ids::YELLOW_TERRACOTTA
                    | block_state_ids::WHITE_TERRACOTTA
                    | block_state_ids::LIGHT_GRAY_TERRACOTTA
                    | block_state_ids::GRAY_TERRACOTTA
                    | block_state_ids::BLACK_TERRACOTTA
                    | block_state_ids::GREEN_TERRACOTTA
                    | block_state_ids::CYAN_TERRACOTTA
                    | block_state_ids::LIME_TERRACOTTA
                    | block_state_ids::BLACK_CONCRETE
            ),
            "block should not be an artificial palette carrier: {block}"
        );
    }

    fn synthetic_wwf_ecoregion_cache() -> Vec<u8> {
        let mut out = Vec::new();
        push_i32_be(&mut out, WwfEcoregionSampler::CACHE_MAGIC);
        push_i32_be(&mut out, WwfEcoregionSampler::CACHE_VERSION);
        push_i32_be(&mut out, 1);
        for value in [0.0, 0.0, 2.0, 2.0] {
            push_f64_be(&mut out, value);
        }
        push_i32_array_be(&mut out, &[0, 5]);
        push_f64_array_be(&mut out, &[0.0, 2.0, 2.0, 0.0, 0.0]);
        push_f64_array_be(&mut out, &[0.0, 0.0, 2.0, 2.0, 0.0]);
        push_java_utf(&mut out, "Cache Jungle");
        push_java_utf(&mut out, "minecraft:jungle");

        let grid_len = WwfEcoregionSampler::GRID_WIDTH * WwfEcoregionSampler::GRID_HEIGHT;
        push_i32_be(&mut out, grid_len as i32);
        for cell in 0..grid_len {
            let x = cell % WwfEcoregionSampler::GRID_WIDTH;
            let y = cell / WwfEcoregionSampler::GRID_WIDTH;
            if (360..=364).contains(&x) && (180..=184).contains(&y) {
                push_i32_array_be(&mut out, &[0]);
            } else {
                push_i32_be(&mut out, 0);
            }
        }
        out
    }

    fn synthetic_wwf_polygon_shapefile() -> Vec<u8> {
        let parts = [0, 5];
        let points = [
            (0.0, 0.0),
            (2.0, 0.0),
            (2.0, 2.0),
            (0.0, 2.0),
            (0.0, 0.0),
            (4.0, 0.0),
            (6.0, 0.0),
            (6.0, 2.0),
            (4.0, 2.0),
            (4.0, 0.0),
        ];
        let content_bytes = 4 + 32 + 4 + 4 + (parts.len() * 4) + (points.len() * 16);
        let record_bytes = 8 + content_bytes;
        let file_bytes = 100 + record_bytes;
        let mut out = vec![0u8; 100];
        put_i32_be(&mut out, 0, 9994);
        put_i32_be(&mut out, 24, (file_bytes / 2) as i32);
        put_i32_le(&mut out, 28, 1000);
        put_i32_le(&mut out, 32, 5);
        put_f64_le(&mut out, 36, 0.0);
        put_f64_le(&mut out, 44, 0.0);
        put_f64_le(&mut out, 52, 6.0);
        put_f64_le(&mut out, 60, 2.0);

        push_i32_be(&mut out, 1);
        push_i32_be(&mut out, (content_bytes / 2) as i32);
        push_i32_le(&mut out, 5);
        for value in [0.0, 0.0, 6.0, 2.0] {
            push_f64_le(&mut out, value);
        }
        push_i32_le(&mut out, parts.len() as i32);
        push_i32_le(&mut out, points.len() as i32);
        for part in parts {
            push_i32_le(&mut out, part);
        }
        for (x, y) in points {
            push_f64_le(&mut out, x);
            push_f64_le(&mut out, y);
        }
        out
    }

    fn synthetic_wwf_dbf() -> Vec<u8> {
        let record_count = 1;
        let field_length = 20usize;
        let header_length = 32 + 32 + 1;
        let record_length = 1 + field_length;
        let mut out = vec![0u8; header_length + record_length];
        out[0] = 0x03;
        put_i32_le(&mut out, 4, record_count);
        put_u16(&mut out, 8, header_length as u16);
        put_u16(&mut out, 10, record_length as u16);
        let descriptor = 32;
        out[descriptor..descriptor + 8].copy_from_slice(b"ECO_NAME");
        out[descriptor + 11] = b'C';
        out[descriptor + 16] = field_length as u8;
        out[header_length - 1] = 0x0d;
        out[header_length] = b' ';
        let name = b"Cache Jungle";
        out[header_length + 1..header_length + 1 + name.len()].copy_from_slice(name);
        for byte in &mut out[header_length + 1 + name.len()..header_length + record_length] {
            *byte = b' ';
        }
        out
    }

    fn push_i32_array_be(out: &mut Vec<u8>, values: &[i32]) {
        push_i32_be(out, values.len() as i32);
        for &value in values {
            push_i32_be(out, value);
        }
    }

    fn push_f64_array_be(out: &mut Vec<u8>, values: &[f64]) {
        push_i32_be(out, values.len() as i32);
        for &value in values {
            push_f64_be(out, value);
        }
    }

    fn push_java_utf(out: &mut Vec<u8>, value: &str) {
        let bytes = modified_utf8_bytes(value);
        assert!(bytes.len() <= u16::MAX as usize);
        out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
        out.extend_from_slice(&bytes);
    }

    fn modified_utf8_bytes(value: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for code_unit in value.encode_utf16() {
            match code_unit {
                0x0001..=0x007f => bytes.push(code_unit as u8),
                0x0000..=0x07ff => {
                    bytes.push((0xc0 | ((code_unit >> 6) & 0x1f)) as u8);
                    bytes.push((0x80 | (code_unit & 0x3f)) as u8);
                }
                _ => {
                    bytes.push((0xe0 | ((code_unit >> 12) & 0x0f)) as u8);
                    bytes.push((0x80 | ((code_unit >> 6) & 0x3f)) as u8);
                    bytes.push((0x80 | (code_unit & 0x3f)) as u8);
                }
            }
        }
        bytes
    }

    fn push_i32_be(out: &mut Vec<u8>, value: i32) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn push_i32_le(out: &mut Vec<u8>, value: i32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_f64_be(out: &mut Vec<u8>, value: f64) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn push_f64_le(out: &mut Vec<u8>, value: f64) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn write_synthetic_met_png(path: &Path, width: u32, height: u32) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let file = File::create(path).unwrap();
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
        for _y in 0..height {
            for x in 0..width {
                let color = if x < width / 2 {
                    RgbColor::of(167, 146, 103)
                } else {
                    RgbColor::of(230, 205, 160)
                };
                pixels.extend_from_slice(&[color.red, color.green, color.blue]);
            }
        }
        writer.write_image_data(&pixels).unwrap();
    }

    fn synthetic_classic_rgb_tiff() -> Vec<u8> {
        let pixel_bytes = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
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

    fn synthetic_classic_single_band_tiff(value: u8) -> Vec<u8> {
        let width = 2usize;
        let height = 2usize;
        let samples = [value; 4];
        let entry_count = 12usize;
        let ifd_offset = 8usize;
        let ifd_bytes = 2 + (entry_count * CLASSIC_IFD_ENTRY_BYTES) + 4;
        let data_start = ifd_offset + ifd_bytes;
        let strip_offsets_offset = data_start;
        let strip_byte_counts_offset = strip_offsets_offset + (height * 4);
        let pixel_scale_offset = strip_byte_counts_offset + (height * 4);
        let tiepoint_offset = pixel_scale_offset + (3 * 8);
        let sample_offset = tiepoint_offset + (6 * 8);
        let row_byte_count = width;
        let file_size = sample_offset + samples.len();
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
            (TAG_SAMPLE_FORMAT, TYPE_SHORT, 1, 1),
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
        put_f64(&mut out, cursor, 1.0);
        put_f64(&mut out, cursor + 8, 1.0);
        put_f64(&mut out, cursor + 16, 0.0);

        cursor = tiepoint_offset;
        put_f64(&mut out, cursor, 0.0);
        put_f64(&mut out, cursor + 8, 0.0);
        put_f64(&mut out, cursor + 16, 0.0);
        put_f64(&mut out, cursor + 24, 0.0);
        put_f64(&mut out, cursor + 32, 2.0);
        put_f64(&mut out, cursor + 40, 0.0);

        out[sample_offset..sample_offset + samples.len()].copy_from_slice(&samples);
        out
    }

    fn synthetic_bigtiff_float32_tiff(value: f32) -> Vec<u8> {
        let width = 2usize;
        let height = 2usize;
        let pixel_bytes = width * height * 4;
        let ifd_offset = 16 + pixel_bytes;
        let entry_count = 14usize;
        let ifd_bytes = 8 + (entry_count * BIG_IFD_ENTRY_BYTES) + 8;
        let data_start = ifd_offset + ifd_bytes;
        let strip_offsets_offset = data_start;
        let strip_byte_counts_offset = strip_offsets_offset + (height * 8);
        let pixel_scale_offset = strip_byte_counts_offset + (height * 4);
        let tiepoint_offset = pixel_scale_offset + (3 * 8);
        let file_size = tiepoint_offset + (6 * 8);
        let mut out = vec![0u8; file_size];

        out[0] = b'I';
        out[1] = b'I';
        put_u16(&mut out, 2, BIG_TIFF_MAGIC);
        put_u16(&mut out, 4, 8);
        put_u16(&mut out, 6, 0);
        put_u64(&mut out, 8, ifd_offset as u64);

        let mut cursor = 16;
        for _ in 0..(width * height) {
            put_f32(&mut out, cursor, value);
            cursor += 4;
        }

        cursor = ifd_offset;
        put_u64(&mut out, cursor, entry_count as u64);
        cursor += 8;
        let no_data_ascii = b"-9999\0";
        let no_data_inline = inline_ascii_u64(no_data_ascii);
        let row_byte_count = (width * 4) as u32;
        let strip_byte_counts_inline =
            u64::from(row_byte_count) | (u64::from(row_byte_count) << 32);
        for (tag, field_type, count, value_or_offset) in [
            (TAG_IMAGE_WIDTH, TYPE_LONG, 1, width as u64),
            (TAG_IMAGE_LENGTH, TYPE_LONG, 1, height as u64),
            (TAG_BITS_PER_SAMPLE, TYPE_SHORT, 1, 32),
            (TAG_COMPRESSION, TYPE_SHORT, 1, 1),
            (TAG_PHOTOMETRIC_INTERPRETATION, TYPE_SHORT, 1, 1),
            (
                TAG_STRIP_OFFSETS,
                TYPE_LONG8,
                height as u64,
                strip_offsets_offset as u64,
            ),
            (TAG_SAMPLES_PER_PIXEL, TYPE_SHORT, 1, 1),
            (TAG_ROWS_PER_STRIP, TYPE_SHORT, 1, 1),
            (
                TAG_STRIP_BYTE_COUNTS,
                TYPE_LONG,
                height as u64,
                strip_byte_counts_inline,
            ),
            (TAG_PLANAR_CONFIGURATION, TYPE_SHORT, 1, 1),
            (TAG_SAMPLE_FORMAT, TYPE_SHORT, 1, 3),
            (
                TAG_MODEL_PIXEL_SCALE,
                TYPE_DOUBLE,
                3,
                pixel_scale_offset as u64,
            ),
            (TAG_MODEL_TIEPOINT, TYPE_DOUBLE, 6, tiepoint_offset as u64),
            (
                TAG_GDAL_NODATA,
                TYPE_ASCII,
                no_data_ascii.len() as u64,
                no_data_inline,
            ),
        ] {
            put_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += BIG_IFD_ENTRY_BYTES;
        }
        put_u64(&mut out, cursor, 0);

        cursor = strip_offsets_offset;
        for y in 0..height {
            put_u64(&mut out, cursor, 16 + ((y * width * 4) as u64));
            cursor += 8;
        }

        cursor = strip_byte_counts_offset;
        for _ in 0..height {
            put_u32(&mut out, cursor, row_byte_count);
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
        put_f64(&mut out, cursor + 32, 2.0);
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

    fn put_u16(out: &mut [u8], offset: usize, value: u16) {
        out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_i32_le(out: &mut [u8], offset: usize, value: i32) {
        out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_i32_be(out: &mut [u8], offset: usize, value: i32) {
        out[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_f64_le(out: &mut [u8], offset: usize, value: f64) {
        out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
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

    fn inline_ascii_u64(bytes: &[u8]) -> u64 {
        assert!(bytes.len() <= 8);
        let mut inline = [0u8; 8];
        inline[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(inline)
    }

    const CLASSIC_TIFF_MAGIC: u16 = 42;
    const BIG_TIFF_MAGIC: u16 = 43;
    const CLASSIC_IFD_ENTRY_BYTES: usize = 12;
    const BIG_IFD_ENTRY_BYTES: usize = 20;
    const TYPE_ASCII: u16 = 2;
    const TYPE_SHORT: u16 = 3;
    const TYPE_LONG: u16 = 4;
    const TYPE_DOUBLE: u16 = 12;
    const TYPE_LONG8: u16 = 16;
    const TAG_IMAGE_WIDTH: u16 = 256;
    const TAG_IMAGE_LENGTH: u16 = 257;
    const TAG_BITS_PER_SAMPLE: u16 = 258;
    const TAG_COMPRESSION: u16 = 259;
    const TAG_PHOTOMETRIC_INTERPRETATION: u16 = 262;
    const TAG_STRIP_OFFSETS: u16 = 273;
    const TAG_SAMPLES_PER_PIXEL: u16 = 277;
    const TAG_ROWS_PER_STRIP: u16 = 278;
    const TAG_STRIP_BYTE_COUNTS: u16 = 279;
    const TAG_PLANAR_CONFIGURATION: u16 = 284;
    const TAG_SAMPLE_FORMAT: u16 = 339;
    const TAG_TILE_WIDTH: u16 = 322;
    const TAG_TILE_LENGTH: u16 = 323;
    const TAG_TILE_OFFSETS: u16 = 324;
    const TAG_TILE_BYTE_COUNTS: u16 = 325;
    const TAG_MODEL_PIXEL_SCALE: u16 = 33550;
    const TAG_MODEL_TIEPOINT: u16 = 33922;
    const TAG_GDAL_NODATA: u16 = 42113;
}
