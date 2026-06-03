#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use earthmap_core::build_info;
use earthmap_geo::{
    EarthScaleMapping, GeoError, GeoTiffHeightmapReader, GeoTiffMetadata, GeoTiffRowCache,
    GeoTiffRowCacheStats, HeightmapScalarSampler, RgbColor, VrtRgbMosaicReader,
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

    const CLASSIC_TIFF_MAGIC: u16 = 42;
    const CLASSIC_IFD_ENTRY_BYTES: usize = 12;
    const TYPE_SHORT: u16 = 3;
    const TYPE_LONG: u16 = 4;
    const TAG_IMAGE_WIDTH: u16 = 256;
    const TAG_IMAGE_LENGTH: u16 = 257;
    const TAG_BITS_PER_SAMPLE: u16 = 258;
    const TAG_COMPRESSION: u16 = 259;
    const TAG_SAMPLES_PER_PIXEL: u16 = 277;
    const TAG_PLANAR_CONFIGURATION: u16 = 284;
    const TAG_TILE_WIDTH: u16 = 322;
    const TAG_TILE_LENGTH: u16 = 323;
    const TAG_TILE_OFFSETS: u16 = 324;
    const TAG_TILE_BYTE_COUNTS: u16 = 325;
}
