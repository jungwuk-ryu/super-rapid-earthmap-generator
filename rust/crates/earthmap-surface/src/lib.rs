#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use earthmap_core::build_info;
use earthmap_geo::{
    EarthScaleMapping, GeoError, GeoTiffHeightmapReader, GeoTiffMetadata, GeoTiffRowCache,
    GeoTiffRowCacheStats, HeightmapScalarSampler,
};
use earthmap_minecraft::block_state_ids;
use earthmap_minecraft::chunk_model::{ChunkModel, CHUNK_WIDTH};
use earthmap_minecraft::{chunk_nbt_encoder, level_dat_template, MinecraftError};
use earthmap_region::{ChunkLocalPos, RegionError};

pub const MODULE_STATUS: &str = "phase4-height-only-region-bootstrap";

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
}
