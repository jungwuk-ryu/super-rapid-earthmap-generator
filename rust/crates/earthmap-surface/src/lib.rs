#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use earthmap_core::build_info;
use earthmap_geo::{
    EarthScaleMapping, GeoError, GeoTiffFloat32Reader, GeoTiffHeightmapReader, GeoTiffMetadata,
    GeoTiffRowCache, GeoTiffRowCacheStats, GeoTiffSingleBandReader, HeightmapScalarSampler,
    RgbColor, VrtRgbMosaicReader,
};
use earthmap_minecraft::block_state_ids;
use earthmap_minecraft::chunk_model::{ChunkModel, CHUNK_WIDTH};
use earthmap_minecraft::{chunk_nbt_encoder, level_dat_template, MinecraftError};
use earthmap_region::{ChunkLocalPos, RegionError};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerrainTokenSource {
    None,
    Export,
    JavaStandardPalette,
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
        Self {
            name: name.into().trim().to_string(),
            biome_id: biome_id.into().trim().to_string(),
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
    material_cache: Mutex<BoundedAccessCache<SurfaceMaterialSample>>,
    ocean_cache: Mutex<BoundedAccessCache<SurfaceMaterialSample>>,
    ecoregion_cache: Mutex<BoundedAccessCache<EcoregionEvidence>>,
}

impl EarthDataSurfaceMaterialSampler {
    const DEFAULT_CACHE_BLOCKS: usize = 256;
    const MATERIAL_CACHE_ENTRIES: usize = 262_144;
    const OCEAN_CACHE_ENTRIES: usize = 262_144;
    const ECOREGION_CACHE_ENTRIES: usize = 262_144;

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
            material_cache: Mutex::new(BoundedAccessCache::new(Self::MATERIAL_CACHE_ENTRIES)),
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
                return Ok(with_surface_terrain_token(&cached, RgbColor::unavailable()));
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
        Ok(with_surface_terrain_token(&sample, RgbColor::unavailable()))
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
                return Ok(with_surface_terrain_token(&cached, RgbColor::unavailable()));
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
        Ok(with_surface_terrain_token(&sample, RgbColor::unavailable()))
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
            if !shape.is_file()
                || !dbf.is_file()
                || !mapping.is_file()
                || !is_fresh_wwf_cache(&cache, &[shape, dbf, mapping])
            {
                continue;
            }
            if let Ok(sampler) = Self::open_cache(cache) {
                return Ok(Some(sampler));
            }
        }
        Ok(None)
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
        (self.grid_y(latitude) * Self::GRID_WIDTH) + self.grid_x(longitude)
    }

    fn grid_x(&self, longitude: f64) -> usize {
        let raw = ((normalize_longitude(longitude) - Self::MIN_LONGITUDE) / Self::CELL_DEGREES)
            .floor() as i32;
        clamp_i32(raw, 0, (Self::GRID_WIDTH - 1) as i32) as usize
    }

    fn grid_y(&self, latitude: f64) -> usize {
        let raw = ((latitude - Self::MIN_LATITUDE) / Self::CELL_DEGREES).floor() as i32;
        clamp_i32(raw, 0, (Self::GRID_HEIGHT - 1) as i32) as usize
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
    let Ok(cache_modified) = std::fs::metadata(cache).and_then(|metadata| metadata.modified())
    else {
        return false;
    };
    for source in sources {
        let Ok(source_modified) =
            std::fs::metadata(source).and_then(|metadata| metadata.modified())
        else {
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
        assert_eq!(
            sample.terrain_token_color,
            MetTerrainVocabulary::nearest(sample.color).color()
        );
        assert_eq!(
            sample.terrain_token_source,
            TerrainTokenSource::JavaStandardPalette
        );
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
        assert_eq!(water.climate_class, SurfaceMaterialSample::UNKNOWN);
        assert_eq!(water.ocean_temperature, 12);
        assert_eq!(water.bathymetry_meters, -123);
        assert_eq!(water.slope_permille, SurfaceMaterialSample::UNKNOWN);
        assert_eq!(sampler.raster_stats().sample_averaged_requests, 2);

        assert_eq!(sampler.sample_water(1.25, 0.75, 0.0, 0.0).unwrap(), water);
        assert_eq!(sampler.raster_stats().sample_averaged_requests, 2);

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

    fn push_f64_be(out: &mut Vec<u8>, value: f64) {
        out.extend_from_slice(&value.to_be_bytes());
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
