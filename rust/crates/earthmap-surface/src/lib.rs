#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use earthmap_core::build_info;
use earthmap_geo::{
    EarthScaleMapping, GeoError, GeoTiffFloat32Reader, GeoTiffHeightmapReader, GeoTiffMetadata,
    GeoTiffRgbReader, GeoTiffRowCache, GeoTiffRowCacheStats, GeoTiffSingleBandReader,
    HeightmapScalarSampler, RgbColor, VrtRgbMosaicReader,
};
use earthmap_minecraft::block_state_ids;
use earthmap_minecraft::chunk_model::{ChunkModel, BIOME_CELL_WIDTH, CHUNK_WIDTH};
use earthmap_minecraft::{chunk_nbt_encoder, level_dat_template, MinecraftError};
use earthmap_region::{ChunkLocalPos, RegionError};
use quick_xml::events::Event;
use quick_xml::Reader;

pub const MODULE_STATUS: &str = "phase5-surface-bootstrap";

pub const SEA_LEVEL_Y: i32 = 63;
pub const ELEVATION_METERS_PER_BLOCK: f64 = 35.0;
pub const REGION_CHUNKS: i32 = 32;
pub const REGION_SIZE_BLOCKS: i32 = REGION_CHUNKS * CHUNK_WIDTH as i32;
pub const DEFAULT_HEIGHT_ONLY_CACHE_ROWS: usize = 64;
pub const SURVIVAL_MANIFEST_FILE_NAME: &str = "earthmap-survival.properties";

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

pub trait SurfaceMaterialSampler {
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
    slope: Option<GeoTiffSingleBandReader>,
    ecoregions: Option<Box<dyn EcoregionSampler>>,
    terrain_tokens: Option<MetImageExportTerrainSampler>,
    land_shallow_topo: Option<LandShallowTopoPhotoSampler>,
    material_cache: Mutex<BoundedAccessCache<SurfaceMaterialSample>>,
    photo_color_cache: Mutex<BoundedAccessCache<RgbColor>>,
    photo_evidence_cache: Mutex<BoundedAccessCache<SurfaceMaterialSample>>,
    ocean_cache: Mutex<BoundedAccessCache<SurfaceMaterialSample>>,
    ecoregion_cache: Mutex<BoundedAccessCache<EcoregionEvidence>>,
}

impl EarthDataSurfaceMaterialSampler {
    const DEFAULT_CACHE_BLOCKS: usize = 256;
    const MATERIAL_CACHE_ENTRIES: usize = 262_144;
    const OCEAN_CACHE_ENTRIES: usize = 262_144;
    const ECOREGION_CACHE_ENTRIES: usize = 262_144;
    const PHOTO_EVIDENCE_CACHE_ENTRIES: usize = Self::MATERIAL_CACHE_ENTRIES / 2;

    pub fn open(true_marble_path: impl AsRef<Path>) -> Result<Self> {
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
            true_marble: VrtRgbMosaicReader::open(true_marble_path)?,
            climate: open_surface_raster(tif_root.as_deref(), "climate.tif")?,
            evergreen_broadleaf_trees: open_surface_raster(
                vegetation.as_deref(),
                "EvergreenBroadleafTrees.tif",
            )?,
            deciduous_broadleaf_trees: open_surface_raster(
                vegetation.as_deref(),
                "DeciduousBroadleafTrees.tif",
            )?,
            needleleaf_trees: open_surface_raster(
                vegetation.as_deref(),
                "EvergreenDeciduousNeedleleafTrees.tif",
            )?,
            mixed_trees: open_surface_raster(vegetation.as_deref(), "mixed.tif")?,
            herbaceous_vegetation: open_surface_raster(
                vegetation.as_deref(),
                "HerbaceousVegetation.tif",
            )?,
            shrubs: open_surface_raster(vegetation.as_deref(), "Shrubs.tif")?,
            snow: open_surface_raster(vegetation.as_deref(), "Snow.tif")?,
            swamp: open_surface_raster(vegetation.as_deref(), "Swamp.tif")?,
            ocean_temperature: open_surface_raster(tif_root.as_deref(), "ocean_temp_infill.tif")?,
            bathymetry: open_surface_float_raster(tif_root.as_deref(), "bathymetry.tif")?,
            slope: open_surface_raster(tif_root.as_deref(), "slope.tif")?,
            ecoregions: WwfEcoregionSampler::open_auto_cache(true_marble_path)?
                .map(|sampler| Box::new(sampler) as Box<dyn EcoregionSampler>),
            terrain_tokens: MetImageExportTerrainSampler::open_auto(true_marble_path)?,
            land_shallow_topo: LandShallowTopoPhotoSampler::open_near(true_marble_path)?,
            material_cache: Mutex::new(BoundedAccessCache::new(Self::MATERIAL_CACHE_ENTRIES)),
            photo_color_cache: Mutex::new(BoundedAccessCache::new(Self::MATERIAL_CACHE_ENTRIES)),
            photo_evidence_cache: Mutex::new(BoundedAccessCache::new(
                Self::PHOTO_EVIDENCE_CACHE_ENTRIES,
            )),
            ocean_cache: Mutex::new(BoundedAccessCache::new(Self::OCEAN_CACHE_ENTRIES)),
            ecoregion_cache: Mutex::new(BoundedAccessCache::new(Self::ECOREGION_CACHE_ENTRIES)),
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
        {
            let mut cache = self
                .material_cache
                .lock()
                .map_err(|_| SurfaceError::invalid("surface material cache lock poisoned"))?;
            if let Some(cached) = cache.get(cell.key) {
                return Ok(self.with_terrain_token(&cached, longitude, latitude));
            }
        }
        let sampled = self.sample_land_uncached(
            cell.center_longitude,
            cell.center_latitude,
            cell_degrees,
            cell_degrees,
        )?;
        let mut cache = self
            .material_cache
            .lock()
            .map_err(|_| SurfaceError::invalid("surface material cache lock poisoned"))?;
        let sample = cache.insert_or_get(cell.key, sampled);
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
        {
            let mut cache = self
                .photo_color_cache
                .lock()
                .map_err(|_| SurfaceError::invalid("surface photo color cache lock poisoned"))?;
            if let Some(cached) = cache.get(cell.key) {
                return Ok(cached);
            }
        }
        let sampled = self.sample_primary_photo_color(
            cell.center_longitude,
            cell.center_latitude,
            longitude_span_degrees,
            latitude_span_degrees,
        )?;
        let mut cache = self
            .photo_color_cache
            .lock()
            .map_err(|_| SurfaceError::invalid("surface photo color cache lock poisoned"))?;
        Ok(cache.insert_or_get(cell.key, sampled))
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
        {
            let mut cache = self
                .photo_evidence_cache
                .lock()
                .map_err(|_| SurfaceError::invalid("surface photo evidence cache lock poisoned"))?;
            if let Some(cached) = cache.get(cell.key) {
                return Ok(cached);
            }
        }
        let sampled = self.sample_photo_evidence_uncached(
            cell.center_longitude,
            cell.center_latitude,
            cell_degrees,
            cell_degrees,
        )?;
        let mut cache = self
            .photo_evidence_cache
            .lock()
            .map_err(|_| SurfaceError::invalid("surface photo evidence cache lock poisoned"))?;
        Ok(cache.insert_or_get(cell.key, sampled))
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
        {
            let mut cache = self
                .ecoregion_cache
                .lock()
                .map_err(|_| SurfaceError::invalid("surface ecoregion cache lock poisoned"))?;
            if let Some(cached) = cache.get(cell.key) {
                return Ok(cached);
            }
        }
        let sampled = self.sample_ecoregion_uncached(
            cell.center_longitude,
            cell.center_latitude,
            cell_degrees,
        );
        let mut cache = self
            .ecoregion_cache
            .lock()
            .map_err(|_| SurfaceError::invalid("surface ecoregion cache lock poisoned"))?;
        Ok(cache.insert_or_get(cell.key, sampled))
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
        {
            let mut cache = self
                .ocean_cache
                .lock()
                .map_err(|_| SurfaceError::invalid("surface ocean cache lock poisoned"))?;
            if let Some(cached) = cache.get(cell.key) {
                return Ok(self.with_terrain_token(&cached, longitude, latitude));
            }
        }
        let sampled = self.sample_ocean_uncached(
            cell.center_longitude,
            cell.center_latitude,
            cell_degrees,
            cell_degrees,
        );
        let mut cache = self
            .ocean_cache
            .lock()
            .map_err(|_| SurfaceError::invalid("surface ocean cache lock poisoned"))?;
        let sample = cache.insert_or_get(cell.key, sampled);
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
pub struct LandShallowTopoPhotoSampler {
    west_path: PathBuf,
    east_path: PathBuf,
    readers: Mutex<HashMap<LandShallowReaderKey, Arc<GeoTiffRgbReader>>>,
}

impl LandShallowTopoPhotoSampler {
    const WEST_FILE: &'static str = "land_shallow_topo_west.tif";
    const EAST_FILE: &'static str = "land_shallow_topo_east.tif";
    const HALF_WORLD_DEGREES: f64 = 180.0;
    const DEFAULT_PIXEL_DEGREES: f64 = 1.0 / 120.0;
    const ROW_CACHE_ENTRIES: usize = 1024;

    pub fn open_near(true_marble_path: impl AsRef<Path>) -> Result<Option<Self>> {
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
            thread_id: std::thread::current().id(),
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
            Self::ROW_CACHE_ENTRIES,
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
    thread_id: std::thread::ThreadId,
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
        let mut file = File::open(path)?;
        Self::read_cache(&mut file)
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
        let mut output = File::create(cache_path)?;
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
        if let Ok(mut last_hit) = self.last_hit.lock() {
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
    let mut file = File::open(dbf_path)?;
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
    let mut file = File::open(shape_path)?;
    let file_length = file.metadata()?.len();
    file.seek(SeekFrom::Start(100))?;
    let mut record_index = 0usize;
    let mut polygons = Vec::with_capacity(names.len());
    while file.stream_position()? < file_length {
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
) -> Result<Option<GeoTiffSingleBandReader>> {
    for candidate_directory in surface_candidate_directories(directory) {
        let candidate = candidate_directory.join(name);
        match GeoTiffSingleBandReader::open_if_present(
            &candidate,
            EarthDataSurfaceMaterialSampler::DEFAULT_CACHE_BLOCKS,
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
    reader: Option<&GeoTiffSingleBandReader>,
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
    match settings.output_format {
        OutputFormat::Mca => earthmap_region::write_mca_region(&region_file, &chunks, 0)?,
        OutputFormat::LinearV2 => {
            earthmap_region::write_linear_v2_region(&region_file, &chunks, 0)?
        }
    }
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

pub fn surface_y_for_elevation_meters(elevation_meters: f64) -> i32 {
    let rounded =
        java_math_round_double_to_narrowed_i32(elevation_meters / ELEVATION_METERS_PER_BLOCK);
    let y = SEA_LEVEL_Y.wrapping_add(rounded);
    y.clamp(-60, 319)
}

pub const DEFAULT_VERTICAL_SCALE: f64 = 1.0;

const BEACH_COAST_FACTOR: f64 = 0.985;
const SHAPED_ELEVATION_METERS_PER_BLOCK: f64 = 45.0;
const MIN_VERTICAL_SCALE: f64 = 0.25;
const MAX_VERTICAL_SCALE: f64 = 4.0;
const MIN_SURFACE_Y: i32 = -60;
const MAX_SURFACE_Y: i32 = 319;

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
        | block_state_ids::RED_SAND
        | block_state_ids::ROOTED_DIRT
        | block_state_ids::MYCELIUM => top,
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
    )
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
    if depth <= 8 || is_wet_surface_biome(&column.biome_id) {
        block_state_ids::CLAY
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
    top_counts: BTreeMap<i32, i32>,
    family_counts: BTreeMap<i32, i32>,
    majority_top: i32,
    majority_top_count: i32,
    majority_family: i32,
    majority_family_count: i32,
}

impl PhotoTextureStats {
    fn count_top(&self, top: i32) -> i32 {
        self.top_counts.get(&top).copied().unwrap_or(0)
    }

    fn count_family(&self, family: i32) -> i32 {
        self.family_counts.get(&family).copied().unwrap_or(0)
    }

    fn majority_top_for_family(&self, family: i32) -> i32 {
        let mut best_top = 0;
        let mut best_count = 0;
        for (&top, &count) in &self.top_counts {
            if photo_texture_family(top) == family && count > best_count {
                best_top = top;
                best_count = count;
            }
        }
        best_top
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
    let mut top_counts = BTreeMap::<i32, i32>::new();
    let mut family_counts = BTreeMap::<i32, i32>::new();
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
            *top_counts.entry(top).or_insert(0) += 1;
            *family_counts.entry(photo_texture_family(top)).or_insert(0) += 1;
        }
    }
    let (majority_top, majority_top_count) = surface_majority_i32(&top_counts);
    let (majority_family, majority_family_count) = surface_majority_i32(&family_counts);
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
    matches!(
        column.terrain_token_source,
        TerrainTokenSource::Export | TerrainTokenSource::JavaStandardPalette
    )
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
            let mut counts = BTreeMap::<String, i32>::new();
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
                    *counts.entry(column.biome_id.clone()).or_insert(0) += 1;
                    *family_counts
                        .entry(intent_biome_family(&column.biome_id))
                        .or_insert(0) += 1;
                }
            }
            let (majority_biome, majority_count) = surface_majority_string(&counts);
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
    java_hashmap_string_majority(&counts)
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

fn java_hashmap_string_majority(counts: &[JavaHashStringCount]) -> Option<String> {
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
        .map(|count| count.key.clone())
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
    is_tinted_intent_render_surface(column.top_block_state_id)
        && (column.terrain_token_source == TerrainTokenSource::Export
            || column.terrain_token_source == TerrainTokenSource::JavaStandardPalette
            || column.decision_source == "photo-palette")
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
    fn natural_surface_block_policy_matches_java_contract_cases() {
        for block in [
            block_state_ids::OAK_LEAVES,
            block_state_ids::JUNGLE_LEAVES,
            block_state_ids::DARK_OAK_LEAVES,
            block_state_ids::SPRUCE_LEAVES,
            block_state_ids::TERRACOTTA,
            block_state_ids::ORANGE_TERRACOTTA,
            block_state_ids::BROWN_TERRACOTTA,
            block_state_ids::RED_TERRACOTTA,
            block_state_ids::YELLOW_TERRACOTTA,
            block_state_ids::WHITE_TERRACOTTA,
            block_state_ids::LIGHT_GRAY_TERRACOTTA,
            block_state_ids::GRAY_TERRACOTTA,
            block_state_ids::BLACK_TERRACOTTA,
            block_state_ids::GREEN_TERRACOTTA,
            block_state_ids::CYAN_TERRACOTTA,
            block_state_ids::LIME_TERRACOTTA,
            block_state_ids::BLACK_CONCRETE,
            block_state_ids::QUARTZ_BLOCK,
            block_state_ids::BONE_BLOCK,
            block_state_ids::END_STONE,
            block_state_ids::END_STONE_BRICKS,
            block_state_ids::SMOOTH_SANDSTONE,
            block_state_ids::CUT_SANDSTONE,
            block_state_ids::CHISELED_SANDSTONE,
            block_state_ids::SMOOTH_RED_SANDSTONE,
            block_state_ids::CUT_RED_SANDSTONE,
            block_state_ids::CHISELED_RED_SANDSTONE,
            block_state_ids::PACKED_MUD,
            block_state_ids::MUD_BRICKS,
            block_state_ids::DRIPSTONE_BLOCK,
        ] {
            assert!(!is_allowed_production_top(block, "minecraft:plains"));
            let replacement = production_surface_top(block, "minecraft:plains");
            assert!(
                is_allowed_production_top(replacement, "minecraft:plains"),
                "palette block replacement is natural: {block} -> {replacement}"
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
            (tif_root.join("slope.tif"), 45),
        ] {
            std::fs::write(path, synthetic_classic_single_band_tiff(value)).unwrap();
        }
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

        assert!(open_surface_raster(Some(&root), &name).unwrap().is_none());
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
