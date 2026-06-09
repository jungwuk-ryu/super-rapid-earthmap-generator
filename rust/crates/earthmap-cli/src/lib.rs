#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use earthmap_core::build_info;
use earthmap_core::commands::{self, CommandStatus};
use earthmap_core::progress;
use earthmap_geo::{
    EarthScaleMapping, GeoTiffFloat32Reader, GeoTiffHeightmapReader, GeoTiffMetadata,
    GeoTiffRowCache, GeoTiffSingleBandReader, HeightmapScalarSampler, VrtRgbMosaicReader,
};
use earthmap_minecraft::{
    block_state_ids,
    chunk_generation_status::ChunkGenerationStatus,
    chunk_model::{
        ChunkModel, BIOME_CELL_WIDTH, CHUNK_WIDTH, SECTION_BIOME_CELL_COUNT, SECTION_BLOCK_COUNT,
    },
    chunk_nbt_encoder,
    dimension_profile::{OVERWORLD_1_21_11, SECTION_HEIGHT},
    level_dat_template,
    nbt::{self, Tag},
    packed_long_array::PackedLongArray,
    section_palette::bits_per_entry_for_palette_size,
};
use earthmap_quality::{self as quality};
use earthmap_region::{
    compare_mca_linear_region_payloads, read_region_payloads, validate_linear_region_file,
    validate_mca_region_file, validate_region_file_for_resume, ChunkLocalPos, RegionError,
    RegionFormat, REGION_CHUNKS_PER_REGION,
};
use earthmap_surface::{
    auto_vertical_scale_for_denominator, classify_surface,
    generate_surface_region_with_open_material_sampler,
    generate_surface_region_with_prepared_sample,
    prepare_surface_region_sample_with_heightmap_sampler, surface_y_for_elevation_meters,
    EarthDataSurfaceMaterialSampler, EarthSurfaceColumn, HeightOnlySettings,
    LandShallowTopoPhotoSampler, MetImageExportTerrainSampler,
    OsmFeatureKind as SurfaceOsmFeatureKind, OsmRegionFeatureMask as SurfaceOsmRegionFeatureMask,
    OutputFormat, PreparedSurfaceRegionSample, SurfaceMaterialSample, SurfaceRegionColumnTrace,
    SurfaceRegionReport, SurfaceRegionSettings, SurfaceTextureMode, WwfEcoregionSampler,
    DEFAULT_SURFACE_TILE_CACHE_ENTRIES, DEFAULT_VERTICAL_SCALE, REGION_SIZE_BLOCKS, SEA_LEVEL_Y,
    SURVIVAL_MANIFEST_FILE_NAME,
};
use serde_json::{json, Value};

const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;
const HEIGHTMAP_PATH_ENV: &str = "EARTHMAP_HEIGHTMAP";
const DATA_ROOT_ENV: &str = "EARTHMAP_DATA_ROOT";
const TIF_ROOT_ENV: &str = "EARTHMAP_TIF_ROOT";
const SURFACE_RASTER_ENV: &str = "EARTHMAP_SURFACE_RASTER";
const FLAT_TEST_REGION_CHUNKS: u8 = 32;
const FLAT_TEST_SURFACE_Y: i32 = 63;
const QUALITY_PREVIEW_REGION_WIDTH: usize = 512;
const QUALITY_PREVIEW_GRID_MIN: usize = 8;
const QUALITY_PREVIEW_GRID_MAX: usize = 256;
const HEIGHTMAP_CACHE_ROWS_ENV: &str = "EARTHMAP_HEIGHTMAP_CACHE_ROWS";
const SURFACE_TILE_CACHE_ENTRIES_ENV: &str = "EARTHMAP_SURFACE_TILE_CACHE_ENTRIES";
const CACHE_AUTO_VALUE: &str = "auto";
const BYTES_PER_MIB: u64 = 1024 * 1024;
const BYTES_PER_GIB: u64 = 1024 * BYTES_PER_MIB;
const HEIGHTMAP_CACHE_MEMORY_PERCENT: u64 = 8;
const HEIGHTMAP_CACHE_MIN_ROWS: usize = 128;
const HEIGHTMAP_CACHE_MAX_ROWS: usize = 8192;
const SURFACE_TILE_CACHE_MEMORY_PERCENT: u64 = 12;
const SURFACE_TILE_CACHE_ASSUMED_ENTRY_BYTES: u64 = 512 * 1024;
const SURFACE_TILE_CACHE_MIN_ENTRIES: usize = 64;
const SURFACE_TILE_CACHE_MAX_ENTRIES: usize = 32_768;
const PREPARED_SURFACE_REGION_ESTIMATED_BYTES: u64 = 128 * BYTES_PER_MIB;
const PREFETCH_MEMORY_PERCENT: u64 = 20;
const PREFETCH_MAX_MEMORY_BYTES: u64 = 32 * BYTES_PER_GIB;
const PREFETCH_SEND_RETRY_MILLIS: u64 = 10;
const SURFACE_PHOTO_LEGACY_REGION_WORKER_LIMIT: usize = 4;
const SURFACE_PHOTO_AUTOTUNE_ENV: &str = "EARTHMAP_SURFACE_WORKER_AUTOTUNE";
const SURFACE_PHOTO_AUTOTUNE_MIN_REGIONS: usize = 8;
const SURFACE_PHOTO_AUTOTUNE_MAX_SAMPLES: usize = 2;
const SURFACE_PHOTO_AUTOTUNE_MAX_PROBES: usize = 96;
const SURFACE_PHOTO_AUTOTUNE_LAND_TARGET: usize = 1;
const SURFACE_PHOTO_AUTOTUNE_MIXED_TARGET: usize = 1;
const SURFACE_PHOTO_AUTOTUNE_OCEAN_TARGET: usize = 0;
const SURFACE_PHOTO_AUTOTUNE_NOISE_RATIO: f64 = 1.05;
const SURFACE_PHOTO_RAYON_THREAD_MULTIPLIER: usize = 2;
const SURFACE_PHOTO_RAYON_THREAD_LIMIT: usize = 16;
const PROGRESS_EVENT_SCHEMA_VERSION: u32 = 1;
const RESUME_FINGERPRINT_SCHEMA_VERSION: u32 = 2;
const SURFACE_SAMPLING_PROFILE_VERSION: &str = "bilinear-bathymetry-detail-coast-v1";
const VANILLA_DELEGATED_RESUME_JOURNAL_FILE_NAME: &str = "earthmap-vanilla-delegated-resume.ndjson";
const PARALLEL_EVENT_CHANNEL_CAPACITY: usize = 1024;
const TOPDOWN_REGION_SIZE_BLOCKS: usize = 512;
const TOPDOWN_AIR_BLOCK: &str = "minecraft:air";
const TOPDOWN_DEFAULT_BIOME: &str = "minecraft:plains";
const POST_FINAL_REGION_SIZE_BLOCKS: usize = 32 * CHUNK_WIDTH;
const POST_FINAL_COAST_OFFENDER_LIMIT: usize = 32;
const FLAT_TEST_MCA_LEVEL_NAME: &str = "SR EarthMap Flat Test";
const FLAT_TEST_LINEAR_LEVEL_NAME: &str = "SR EarthMap Linear Flat Test";
const FLAT_TEST_MCA_SEED: i64 = 987654321;
const FLAT_TEST_LINEAR_SEED: i64 = 13579;
const PALETTE_STRESS_SURFACE_Y: i32 = 64;
const PALETTE_STRESS_Y: i32 = 65;
const PALETTE_STRESS_SECTION_Y: i32 = 4;
const PALETTE_STRESS_EXPECTED_SECTION_DATA_LONGS: i32 = 342;
const PALETTE_STRESS_LEVEL_NAME: &str = "SR EarthMap Palette Stress";
const PALETTE_STRESS_BLOCK_STATE_IDS: &[i32] = &[
    block_state_ids::STONE,
    block_state_ids::DIRT,
    block_state_ids::GRASS_BLOCK,
    block_state_ids::SAND,
    block_state_ids::SNOW_BLOCK,
    block_state_ids::ICE,
    block_state_ids::DEEPSLATE,
    block_state_ids::COAL_ORE,
    block_state_ids::IRON_ORE,
    block_state_ids::COPPER_ORE,
    block_state_ids::GOLD_ORE,
    block_state_ids::REDSTONE_ORE,
    block_state_ids::LAPIS_ORE,
    block_state_ids::DIAMOND_ORE,
    block_state_ids::EMERALD_ORE,
    block_state_ids::DEEPSLATE_COAL_ORE,
    block_state_ids::DEEPSLATE_IRON_ORE,
    block_state_ids::DEEPSLATE_COPPER_ORE,
    block_state_ids::DEEPSLATE_GOLD_ORE,
    block_state_ids::STONE_BRICKS,
];

pub fn run<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    run_with_writers(args, &mut stdout, &mut stderr)
}

fn configure_surface_photo_rayon_threads(requested_threads: usize) {
    if std::env::var_os("RAYON_NUM_THREADS").is_some() {
        return;
    }
    let rayon_threads = configured_surface_photo_rayon_thread_count(requested_threads);
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(rayon_threads)
        .build_global();
}

fn configured_surface_photo_rayon_thread_count(requested_threads: usize) -> usize {
    if let Some(explicit) = explicit_rayon_num_threads() {
        return explicit.max(1);
    }
    requested_threads
        .saturating_mul(SURFACE_PHOTO_RAYON_THREAD_MULTIPLIER)
        .max(requested_threads.max(1))
        .min(SURFACE_PHOTO_RAYON_THREAD_LIMIT)
}

fn explicit_rayon_num_threads() -> Option<usize> {
    std::env::var("RAYON_NUM_THREADS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfacePhotoWorkerTuneSampleKind {
    Land,
    Ocean,
    Mixed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SurfacePhotoWorkerTuneSample {
    region_x: i32,
    region_z: i32,
    kind: SurfacePhotoWorkerTuneSampleKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SurfacePhotoWorkerCandidateConfig {
    worker_count: usize,
    rayon_threads: usize,
    parallel_column_sampling: bool,
}

impl SurfacePhotoWorkerCandidateConfig {
    fn new(worker_count: usize, rayon_threads: usize, parallel_column_sampling: bool) -> Self {
        Self {
            worker_count: worker_count.max(1),
            rayon_threads: rayon_threads.max(worker_count.max(1)),
            parallel_column_sampling,
        }
    }

    fn resource_rank(self) -> (usize, usize, bool) {
        (
            self.rayon_threads,
            self.worker_count,
            self.parallel_column_sampling,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
struct SurfacePhotoWorkerCandidateResult {
    worker_count: usize,
    rayon_threads: usize,
    parallel_column_sampling: bool,
    elapsed_millis: u128,
    sample_count: usize,
}

impl SurfacePhotoWorkerCandidateResult {
    fn config(&self) -> SurfacePhotoWorkerCandidateConfig {
        SurfacePhotoWorkerCandidateConfig::new(
            self.worker_count,
            self.rayon_threads,
            self.parallel_column_sampling,
        )
    }

    fn millis_per_region(&self) -> f64 {
        if self.sample_count == 0 {
            f64::INFINITY
        } else {
            self.elapsed_millis as f64 / self.sample_count as f64
        }
    }

    fn regions_per_hour(&self) -> f64 {
        if self.elapsed_millis == 0 {
            0.0
        } else {
            self.sample_count as f64 * 3_600_000.0 / self.elapsed_millis as f64
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct SurfacePhotoWorkerTuning {
    mode: &'static str,
    requested_threads: usize,
    selected_worker_count: usize,
    selected_rayon_threads: usize,
    parallel_column_sampling: bool,
    sample_count: usize,
    land_samples: usize,
    ocean_samples: usize,
    mixed_samples: usize,
    candidates: Vec<SurfacePhotoWorkerCandidateResult>,
    confirmation_candidates: Vec<SurfacePhotoWorkerCandidateResult>,
    message: Option<String>,
}

impl SurfacePhotoWorkerTuning {
    fn fallback(
        mode: &'static str,
        requested_threads: usize,
        submitted_regions: usize,
        message: impl Into<String>,
    ) -> Self {
        let selected_worker_count =
            fallback_surface_photo_worker_count(requested_threads, submitted_regions);
        let selected_rayon_threads = configured_surface_photo_rayon_thread_count(requested_threads)
            .max(selected_worker_count);
        Self {
            mode,
            requested_threads,
            selected_worker_count,
            selected_rayon_threads,
            parallel_column_sampling: surface_photo_parallel_column_sampling(
                selected_worker_count,
                selected_rayon_threads,
            ),
            sample_count: 0,
            land_samples: 0,
            ocean_samples: 0,
            mixed_samples: 0,
            candidates: Vec::new(),
            confirmation_candidates: Vec::new(),
            message: Some(message.into()),
        }
    }

    fn to_progress_json(&self) -> Value {
        json!({
            "mode": self.mode,
            "requestedThreads": self.requested_threads,
            "selectedWorkerThreads": self.selected_worker_count,
            "selectedRegionWorkerThreads": self.selected_worker_count,
            "selectedRayonThreads": self.selected_rayon_threads,
            "parallelColumnSampling": self.parallel_column_sampling,
            "sampleCount": self.sample_count,
            "landSamples": self.land_samples,
            "oceanSamples": self.ocean_samples,
            "mixedSamples": self.mixed_samples,
            "noiseRatio": SURFACE_PHOTO_AUTOTUNE_NOISE_RATIO,
            "message": self.message,
            "candidates": self.candidates.iter().map(|candidate| {
                json!({
                    "workerThreads": candidate.worker_count,
                    "regionWorkerThreads": candidate.worker_count,
                    "rayonThreads": candidate.rayon_threads,
                    "parallelColumnSampling": candidate.parallel_column_sampling,
                    "elapsedMillis": u128_to_u64(candidate.elapsed_millis),
                    "sampleCount": candidate.sample_count,
                    "millisPerRegion": candidate.millis_per_region(),
                    "regionsPerHour": candidate.regions_per_hour(),
                })
            }).collect::<Vec<_>>(),
            "confirmationCandidates": self.confirmation_candidates.iter().map(|candidate| {
                json!({
                    "workerThreads": candidate.worker_count,
                    "regionWorkerThreads": candidate.worker_count,
                    "rayonThreads": candidate.rayon_threads,
                    "parallelColumnSampling": candidate.parallel_column_sampling,
                    "elapsedMillis": u128_to_u64(candidate.elapsed_millis),
                    "sampleCount": candidate.sample_count,
                    "millisPerRegion": candidate.millis_per_region(),
                    "regionsPerHour": candidate.regions_per_hour(),
                })
            }).collect::<Vec<_>>(),
        })
    }
}

fn surface_photo_parallel_column_sampling(_worker_count: usize, rayon_threads: usize) -> bool {
    rayon_threads > 1
}

fn fallback_surface_photo_worker_count(
    requested_threads: usize,
    submitted_regions: usize,
) -> usize {
    let available = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(requested_threads.max(1));
    requested_threads
        .min(submitted_regions.max(1))
        .min(available.max(1))
        .min(SURFACE_PHOTO_LEGACY_REGION_WORKER_LIMIT)
        .max(1)
}

fn surface_photo_worker_autotune_enabled() -> bool {
    std::env::var(SURFACE_PHOTO_AUTOTUNE_ENV)
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(true)
}

fn surface_photo_worker_candidates(
    requested_threads: usize,
    submitted_regions: usize,
) -> Vec<usize> {
    let max_workers = requested_threads.min(submitted_regions.max(1)).max(1);
    let legacy = requested_threads
        .min(submitted_regions.max(1))
        .min(SURFACE_PHOTO_LEGACY_REGION_WORKER_LIMIT)
        .max(1);
    if max_workers <= legacy {
        return vec![max_workers];
    }
    vec![legacy]
}

fn surface_photo_worker_candidate_configs(
    requested_threads: usize,
    submitted_regions: usize,
) -> Vec<SurfacePhotoWorkerCandidateConfig> {
    let region_workers = surface_photo_worker_candidates(requested_threads, submitted_regions);
    let default_rayon_threads = configured_surface_photo_rayon_thread_count(requested_threads);
    let mut configs = Vec::new();
    for worker_count in region_workers {
        let mut rayon_candidates = vec![default_rayon_threads.max(worker_count)];
        rayon_candidates.sort_unstable();
        rayon_candidates.dedup();
        for rayon_threads in rayon_candidates {
            configs.push(SurfacePhotoWorkerCandidateConfig::new(
                worker_count,
                rayon_threads,
                surface_photo_parallel_column_sampling(worker_count, rayon_threads),
            ));
        }
    }
    configs.sort_by_key(|config| {
        (
            config.worker_count,
            config.rayon_threads,
            config.parallel_column_sampling,
        )
    });
    configs.dedup();
    configs
}

fn surface_photo_worker_candidate_config_json(config: SurfacePhotoWorkerCandidateConfig) -> Value {
    json!({
        "workerThreads": config.worker_count,
        "regionWorkerThreads": config.worker_count,
        "rayonThreads": config.rayon_threads,
        "parallelColumnSampling": config.parallel_column_sampling,
    })
}

fn select_surface_photo_worker_candidate(
    candidates: &[SurfacePhotoWorkerCandidateResult],
    noise_ratio: f64,
) -> Option<SurfacePhotoWorkerCandidateConfig> {
    let best_score = candidates
        .iter()
        .map(SurfacePhotoWorkerCandidateResult::millis_per_region)
        .filter(|score| score.is_finite())
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))?;
    candidates
        .iter()
        .filter(|candidate| candidate.millis_per_region() <= best_score * noise_ratio)
        .map(SurfacePhotoWorkerCandidateResult::config)
        .min_by_key(|config| config.resource_rank())
}

fn tune_surface_photo_workers_for_grid(
    heightmap: &Path,
    world: &Path,
    format: OutputFormat,
    scale: i32,
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
    requested_threads: usize,
    status: ChunkGenerationStatus,
    vertical_scale: f64,
    surface_material_path: &Path,
    cache_rows: usize,
    surface_tile_cache_entries: usize,
    compression_options: RegionCompressionOptions,
) -> SurfacePhotoWorkerTuning {
    let submitted_regions = usize::try_from(cols.saturating_mul(rows)).unwrap_or(usize::MAX);
    if submitted_regions < SURFACE_PHOTO_AUTOTUNE_MIN_REGIONS || requested_threads <= 1 {
        return SurfacePhotoWorkerTuning::fallback(
            "fallback-small-batch",
            requested_threads,
            submitted_regions,
            "batch is too small for startup worker tuning",
        );
    }
    if !surface_photo_worker_autotune_enabled() {
        return SurfacePhotoWorkerTuning::fallback(
            "fallback-disabled",
            requested_threads,
            submitted_regions,
            format!("{SURFACE_PHOTO_AUTOTUNE_ENV} disabled worker tuning"),
        );
    }
    let positions = surface_photo_grid_probe_regions(start_region_x, start_region_z, cols, rows);
    tune_surface_photo_workers_from_regions(
        heightmap,
        world,
        format,
        scale,
        requested_threads,
        status,
        vertical_scale,
        surface_material_path,
        cache_rows,
        surface_tile_cache_entries,
        compression_options,
        submitted_regions,
        &positions,
    )
}

#[allow(clippy::too_many_arguments)]
fn tune_surface_photo_workers_for_plan(
    heightmap: &Path,
    world: &Path,
    format: OutputFormat,
    scale: i32,
    plan_regions: &[(i32, i32)],
    submitted_regions: usize,
    requested_threads: usize,
    status: ChunkGenerationStatus,
    vertical_scale: f64,
    surface_material_path: &Path,
    cache_rows: usize,
    surface_tile_cache_entries: usize,
    compression_options: RegionCompressionOptions,
) -> SurfacePhotoWorkerTuning {
    if submitted_regions < SURFACE_PHOTO_AUTOTUNE_MIN_REGIONS || requested_threads <= 1 {
        return SurfacePhotoWorkerTuning::fallback(
            "fallback-small-batch",
            requested_threads,
            submitted_regions,
            "batch is too small for startup worker tuning",
        );
    }
    if !surface_photo_worker_autotune_enabled() {
        return SurfacePhotoWorkerTuning::fallback(
            "fallback-disabled",
            requested_threads,
            submitted_regions,
            format!("{SURFACE_PHOTO_AUTOTUNE_ENV} disabled worker tuning"),
        );
    }
    let positions = surface_photo_plan_probe_regions(plan_regions, submitted_regions);
    tune_surface_photo_workers_from_regions(
        heightmap,
        world,
        format,
        scale,
        requested_threads,
        status,
        vertical_scale,
        surface_material_path,
        cache_rows,
        surface_tile_cache_entries,
        compression_options,
        submitted_regions,
        &positions,
    )
}

#[allow(clippy::too_many_arguments)]
fn tune_surface_photo_workers_from_regions(
    heightmap: &Path,
    world: &Path,
    format: OutputFormat,
    scale: i32,
    requested_threads: usize,
    status: ChunkGenerationStatus,
    vertical_scale: f64,
    surface_material_path: &Path,
    cache_rows: usize,
    surface_tile_cache_entries: usize,
    compression_options: RegionCompressionOptions,
    submitted_regions: usize,
    probe_regions: &[(i32, i32)],
) -> SurfacePhotoWorkerTuning {
    let candidates = surface_photo_worker_candidate_configs(requested_threads, submitted_regions);
    if candidates.len() <= 1 {
        return SurfacePhotoWorkerTuning::fallback(
            "fallback-single-candidate",
            requested_threads,
            submitted_regions,
            "only one worker candidate is available",
        );
    }
    let samples =
        match select_surface_photo_tune_samples(heightmap, scale, cache_rows, probe_regions) {
            Ok(samples) if !samples.is_empty() => samples,
            Ok(_) => {
                return SurfacePhotoWorkerTuning::fallback(
                    "fallback-no-samples",
                    requested_threads,
                    submitted_regions,
                    "no valid land/ocean tuning samples were found",
                )
            }
            Err(error) => {
                return SurfacePhotoWorkerTuning::fallback(
                    "fallback-sample-error",
                    requested_threads,
                    submitted_regions,
                    error,
                )
            }
        };
    let sample_count = samples.len();
    let land_samples = samples
        .iter()
        .filter(|sample| sample.kind == SurfacePhotoWorkerTuneSampleKind::Land)
        .count();
    let ocean_samples = samples
        .iter()
        .filter(|sample| sample.kind == SurfacePhotoWorkerTuneSampleKind::Ocean)
        .count();
    let mixed_samples = sample_count
        .saturating_sub(land_samples)
        .saturating_sub(ocean_samples);
    let tune_root = surface_photo_worker_tune_root(world);
    let _ = fs::create_dir_all(&tune_root);

    let mut results = Vec::with_capacity(candidates.len());
    let warmup_worker = candidates
        .iter()
        .copied()
        .find(|candidate| candidate.worker_count >= SURFACE_PHOTO_LEGACY_REGION_WORKER_LIMIT)
        .unwrap_or(candidates[0]);
    if let Some(warmup_sample) = samples.first() {
        let warmup_dir = tune_root.join("warmup");
        let _ = benchmark_surface_photo_worker_candidate(
            heightmap,
            &warmup_dir,
            format,
            scale,
            std::slice::from_ref(warmup_sample),
            warmup_worker,
            status,
            vertical_scale,
            surface_material_path,
            cache_rows,
            surface_tile_cache_entries,
            compression_options,
        );
    }
    for candidate in candidates {
        let candidate_dir = tune_root.join(format!(
            "workers-{}-rayon-{}-columns-{}",
            candidate.worker_count,
            candidate.rayon_threads,
            if candidate.parallel_column_sampling {
                "parallel"
            } else {
                "serial"
            }
        ));
        match benchmark_surface_photo_worker_candidate(
            heightmap,
            &candidate_dir,
            format,
            scale,
            &samples,
            candidate,
            status,
            vertical_scale,
            surface_material_path,
            cache_rows,
            surface_tile_cache_entries,
            compression_options,
        ) {
            Ok(result) => results.push(result),
            Err(error) => {
                let _ = fs::remove_dir_all(&tune_root);
                return SurfacePhotoWorkerTuning::fallback(
                    "fallback-benchmark-error",
                    requested_threads,
                    submitted_regions,
                    error,
                );
            }
        }
    }
    let _ = fs::remove_dir_all(&tune_root);
    let initial_selected =
        select_surface_photo_worker_candidate(&results, SURFACE_PHOTO_AUTOTUNE_NOISE_RATIO)
            .unwrap_or_else(|| {
                SurfacePhotoWorkerCandidateConfig::new(
                    fallback_surface_photo_worker_count(requested_threads, submitted_regions),
                    configured_surface_photo_rayon_thread_count(requested_threads),
                    true,
                )
            });
    let selected_config = initial_selected;
    let confirmation_candidates = Vec::new();
    SurfacePhotoWorkerTuning {
        mode: "autotuned",
        requested_threads,
        selected_worker_count: selected_config.worker_count,
        selected_rayon_threads: selected_config.rayon_threads,
        parallel_column_sampling: selected_config.parallel_column_sampling,
        sample_count,
        land_samples,
        ocean_samples,
        mixed_samples,
        candidates: results,
        confirmation_candidates,
        message: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn benchmark_surface_photo_worker_candidate(
    heightmap: &Path,
    world: &Path,
    format: OutputFormat,
    scale: i32,
    samples: &[SurfacePhotoWorkerTuneSample],
    candidate: SurfacePhotoWorkerCandidateConfig,
    status: ChunkGenerationStatus,
    vertical_scale: f64,
    surface_material_path: &Path,
    cache_rows: usize,
    surface_tile_cache_entries: usize,
    compression_options: RegionCompressionOptions,
) -> std::result::Result<SurfacePhotoWorkerCandidateResult, String> {
    fs::create_dir_all(world.join("region")).map_err(|error| error.to_string())?;
    let region_queue = Mutex::new(VecDeque::from(samples.to_vec()));
    let stop_queueing = AtomicBool::new(false);
    let start = Instant::now();
    let actual_workers = candidate.worker_count.min(samples.len()).max(1);
    let surface_tile_cache_entries_per_worker =
        surface_tile_cache_entries.div_ceil(actual_workers).max(1);
    let worker_error = Mutex::new(None::<String>);
    let pool = build_surface_photo_rayon_pool(candidate.rayon_threads)?;
    pool.scope(|scope| {
        for _ in 0..actual_workers {
            let queue = &region_queue;
            let stop_queueing = &stop_queueing;
            let worker_error = &worker_error;
            scope.spawn(move |_| {
                let surface_material_sampler =
                    match EarthDataSurfaceMaterialSampler::open_with_tile_cache_entries(
                        surface_material_path,
                        surface_tile_cache_entries_per_worker,
                    ) {
                        Ok(sampler) => sampler,
                        Err(error) => {
                            stop_queueing.store(true, Ordering::SeqCst);
                            *worker_error.lock().expect("worker tuning error lock") =
                                Some(error.to_string());
                            return;
                        }
                    };
                loop {
                    if stop_queueing.load(Ordering::SeqCst) {
                        return;
                    }
                    let next = {
                        let mut queue = queue.lock().expect("worker tuning queue lock");
                        queue.pop_front()
                    };
                    let Some(sample) = next else {
                        return;
                    };
                    let result = (|| -> std::result::Result<(), String> {
                        let mut settings = SurfaceRegionSettings::new_with_texture_options(
                            heightmap,
                            world,
                            "SR EarthMap Worker Tune",
                            0,
                            scale,
                            sample.region_x,
                            sample.region_z,
                            format,
                            cache_rows,
                            false,
                            status,
                            vertical_scale,
                            SurfaceTextureMode::Photo,
                        )
                        .map_err(|error| error.to_string())?;
                        settings.surface_material_path = Some(surface_material_path.to_path_buf());
                        settings.surface_tile_cache_entries = surface_tile_cache_entries_per_worker;
                        settings.parallel_column_sampling = candidate.parallel_column_sampling;
                        apply_region_compression_options(&mut settings, compression_options);
                        generate_surface_region_with_open_material_sampler(
                            &settings,
                            Some(&surface_material_sampler),
                        )
                        .map_err(|error| error.to_string())?;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        stop_queueing.store(true, Ordering::SeqCst);
                        *worker_error.lock().expect("worker tuning error lock") = Some(error);
                        return;
                    }
                }
            });
        }
    });
    if let Some(error) = worker_error.into_inner().expect("worker tuning error lock") {
        return Err(error);
    }
    Ok(SurfacePhotoWorkerCandidateResult {
        worker_count: candidate.worker_count,
        rayon_threads: candidate.rayon_threads,
        parallel_column_sampling: candidate.parallel_column_sampling,
        elapsed_millis: start.elapsed().as_millis(),
        sample_count: samples.len(),
    })
}

fn build_surface_photo_rayon_pool(
    rayon_threads: usize,
) -> std::result::Result<rayon::ThreadPool, String> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(rayon_threads.max(1))
        .build()
        .map_err(|error| error.to_string())
}

fn split_prefetch_rayon_threads(
    rayon_threads: usize,
    prefetch_workers: usize,
    consumer_workers: usize,
) -> (usize, usize) {
    let total = rayon_threads.max(1);
    if total == 1 {
        return (1, 1);
    }
    let consumer_threads = if total >= 16 {
        consumer_workers.min(3).max(1)
    } else if total >= 8 {
        consumer_workers.min(2).max(1)
    } else {
        consumer_workers.min((total / 4).max(1)).max(1)
    };
    let sample_threads = total
        .saturating_sub(consumer_threads)
        .max(prefetch_workers.max(1));
    (sample_threads, consumer_threads)
}

fn open_ocean_prefetch_output_rayon_threads(
    rayon_threads: usize,
    base_output_threads: usize,
    consumer_workers: usize,
) -> usize {
    let total = rayon_threads.max(1);
    let base = base_output_threads.max(1);
    if total >= 16 {
        return (consumer_workers.saturating_add(2))
            .min(total.saturating_sub(1).max(1))
            .max(base);
    }
    if total >= 8 {
        return consumer_workers.min(3).max(base);
    }
    base
}

fn surface_photo_worker_tune_root(world: &Path) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "earthmap-worker-tune-{}-{millis}-{}",
        std::process::id(),
        sanitize_filename_component(&world.display().to_string())
    ))
}

fn sanitize_filename_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn surface_photo_grid_probe_regions(
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
) -> Vec<(i32, i32)> {
    let x_offsets = evenly_spaced_i32_offsets(cols, 10);
    let z_offsets = evenly_spaced_i32_offsets(rows, 10);
    let mut regions = Vec::new();
    for z in z_offsets {
        for &x in &x_offsets {
            regions.push((
                start_region_x.wrapping_add(x),
                start_region_z.wrapping_add(z),
            ));
            if regions.len() >= SURFACE_PHOTO_AUTOTUNE_MAX_PROBES {
                return regions;
            }
        }
    }
    regions
}

fn surface_photo_plan_probe_regions(
    plan_regions: &[(i32, i32)],
    submitted_regions: usize,
) -> Vec<(i32, i32)> {
    let count = submitted_regions.min(plan_regions.len());
    if count == 0 {
        return Vec::new();
    }
    let offsets = evenly_spaced_usize_offsets(count, SURFACE_PHOTO_AUTOTUNE_MAX_PROBES);
    offsets
        .into_iter()
        .filter_map(|index| plan_regions.get(index).copied())
        .collect()
}

fn evenly_spaced_i32_offsets(count: i32, target: usize) -> Vec<i32> {
    if count <= 0 || target == 0 {
        return Vec::new();
    }
    let count_usize = usize::try_from(count).unwrap_or(usize::MAX);
    evenly_spaced_usize_offsets(count_usize, target)
        .into_iter()
        .filter_map(|value| i32::try_from(value).ok())
        .collect()
}

fn evenly_spaced_usize_offsets(count: usize, target: usize) -> Vec<usize> {
    if count == 0 || target == 0 {
        return Vec::new();
    }
    if count <= target {
        return (0..count).collect();
    }
    let mut offsets = Vec::with_capacity(target);
    for index in 0..target {
        let numerator = index * (count - 1);
        let offset = (numerator + ((target - 1) / 2)) / (target - 1);
        if offsets.last().copied() != Some(offset) {
            offsets.push(offset);
        }
    }
    offsets
}

fn select_surface_photo_tune_samples(
    heightmap: &Path,
    scale: i32,
    cache_rows: usize,
    probe_regions: &[(i32, i32)],
) -> std::result::Result<Vec<SurfacePhotoWorkerTuneSample>, String> {
    let reader = GeoTiffHeightmapReader::open(heightmap).map_err(|error| error.to_string())?;
    let mapping = mapping_for(reader.metadata(), scale).map_err(|error| error.to_string())?;
    let cache = GeoTiffRowCache::new(&reader, cache_rows).map_err(|error| error.to_string())?;
    let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);
    let mut land = Vec::new();
    let mut ocean = Vec::new();
    let mut mixed = Vec::new();
    for &(region_x, region_z) in probe_regions {
        let kind = classify_surface_photo_tune_region(&mapping, &sampler, region_x, region_z)?;
        let sample = SurfacePhotoWorkerTuneSample {
            region_x,
            region_z,
            kind,
        };
        match kind {
            SurfacePhotoWorkerTuneSampleKind::Land => land.push(sample),
            SurfacePhotoWorkerTuneSampleKind::Ocean => ocean.push(sample),
            SurfacePhotoWorkerTuneSampleKind::Mixed => mixed.push(sample),
        }
        if land.len() >= SURFACE_PHOTO_AUTOTUNE_LAND_TARGET
            && mixed.len() >= SURFACE_PHOTO_AUTOTUNE_MIXED_TARGET
            && ocean.len() >= SURFACE_PHOTO_AUTOTUNE_OCEAN_TARGET
        {
            break;
        }
    }
    let mut samples = Vec::with_capacity(SURFACE_PHOTO_AUTOTUNE_MAX_SAMPLES);
    surface_photo_take_samples(
        &mut samples,
        &mut mixed,
        SURFACE_PHOTO_AUTOTUNE_MIXED_TARGET,
    );
    surface_photo_take_samples(&mut samples, &mut land, SURFACE_PHOTO_AUTOTUNE_LAND_TARGET);
    surface_photo_take_samples(
        &mut samples,
        &mut ocean,
        SURFACE_PHOTO_AUTOTUNE_OCEAN_TARGET,
    );
    while samples.len() < SURFACE_PHOTO_AUTOTUNE_MAX_SAMPLES {
        if let Some(sample) = mixed.pop() {
            samples.push(sample);
            continue;
        }
        if let Some(sample) = ocean.pop() {
            samples.push(sample);
            continue;
        }
        if let Some(sample) = land.pop() {
            samples.push(sample);
            continue;
        }
        break;
    }
    Ok(samples)
}

fn surface_photo_take_samples(
    samples: &mut Vec<SurfacePhotoWorkerTuneSample>,
    source: &mut Vec<SurfacePhotoWorkerTuneSample>,
    count: usize,
) {
    for _ in 0..count {
        let Some(sample) = source.pop() else {
            return;
        };
        samples.push(sample);
    }
}

fn classify_surface_photo_tune_region(
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
    region_x: i32,
    region_z: i32,
) -> std::result::Result<SurfacePhotoWorkerTuneSampleKind, String> {
    const CLASSIFICATION_GRID: i32 = 9;
    let mut land = 0;
    let mut water = 0;
    for grid_z in 0..CLASSIFICATION_GRID {
        let local_z = ((grid_z + 1) * REGION_SIZE_BLOCKS) / (CLASSIFICATION_GRID + 1);
        for grid_x in 0..CLASSIFICATION_GRID {
            let local_x = ((grid_x + 1) * REGION_SIZE_BLOCKS) / (CLASSIFICATION_GRID + 1);
            if let Some(elevation) = sample_tune_region_elevation(
                mapping, sampler, region_x, region_z, local_x, local_z,
            )? {
                if elevation <= 0.0 {
                    water += 1;
                } else {
                    land += 1;
                }
                if land > 0 && water > 0 {
                    return Ok(SurfacePhotoWorkerTuneSampleKind::Mixed);
                }
            }
        }
    }
    Ok(if land > 0 && water > 0 {
        SurfacePhotoWorkerTuneSampleKind::Mixed
    } else if land > 0 {
        SurfacePhotoWorkerTuneSampleKind::Land
    } else if water > 0 {
        SurfacePhotoWorkerTuneSampleKind::Ocean
    } else {
        SurfacePhotoWorkerTuneSampleKind::Mixed
    })
}

fn sample_tune_region_elevation(
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
    region_x: i32,
    region_z: i32,
    local_x: i32,
    local_z: i32,
) -> std::result::Result<Option<f64>, String> {
    let global_block_x = region_x
        .wrapping_mul(REGION_SIZE_BLOCKS)
        .wrapping_add(local_x);
    let global_block_z = region_z
        .wrapping_mul(REGION_SIZE_BLOCKS)
        .wrapping_add(local_z);
    let map_x = global_block_x.wrapping_add(mapping.width_blocks / 2);
    let map_z = global_block_z.wrapping_add(mapping.height_blocks / 2);
    if map_x < 0 || map_x >= mapping.width_blocks || map_z < 0 || map_z >= mapping.height_blocks {
        return Ok(None);
    }
    let longitude = mapping
        .longitude_for_block_x(map_x)
        .map_err(|error| error.to_string())?;
    let latitude = mapping
        .latitude_for_block_z(map_z)
        .map_err(|error| error.to_string())?;
    sampler
        .bilinear_meters(longitude, latitude)
        .map(Some)
        .map_err(|error| error.to_string())
}

pub fn run_with_writers<I, S, W, E>(args: I, stdout: &mut W, stderr: &mut E) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    W: Write,
    E: Write,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    if args.is_empty() || has(&args, "--help") || has(&args, "-h") {
        return write_result(print_help(stdout));
    }
    if has(&args, "--version") {
        return write_result(print_version(stdout));
    }
    if has(&args, "doctor") {
        return write_result(print_doctor(stdout));
    }
    if has(&args, "capabilities") {
        return write_result(print_capabilities(stdout));
    }

    match args[0].as_str() {
        "inspect-heightmap" if args.len() == 1 => {
            match default_heightmap_or_print(stderr, "inspect-heightmap") {
                Some(heightmap) => write_result(inspect_heightmap(stdout, stderr, &heightmap)),
                None => EXIT_USAGE,
            }
        }
        "inspect-heightmap" if args.len() == 2 => {
            write_result(inspect_heightmap(stdout, stderr, &args[1]))
        }
        "locate-heightmap-point" if args.len() == 4 => {
            match default_heightmap_or_print(stderr, "locate-heightmap-point") {
                Some(heightmap) => write_result(locate_heightmap_point(
                    stdout, stderr, &heightmap, &args[1], &args[2], &args[3],
                )),
                None => EXIT_USAGE,
            }
        }
        "locate-heightmap-point" if args.len() == 5 => write_result(locate_heightmap_point(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4],
        )),
        "classify-surface-point" if args.len() == 4 => {
            match default_heightmap_or_print(stderr, "classify-surface-point") {
                Some(heightmap) => write_result(classify_surface_point(
                    stdout, stderr, &heightmap, &args[1], &args[2], &args[3],
                )),
                None => EXIT_USAGE,
            }
        }
        "classify-surface-point" if args.len() == 5 => write_result(classify_surface_point(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4],
        )),
        "raster-smoke" if args.len() == 3 => {
            match default_heightmap_or_print(stderr, "raster-smoke") {
                Some(heightmap) => {
                    write_result(raster_smoke(stdout, stderr, &heightmap, &args[1], &args[2]))
                }
                None => EXIT_USAGE,
            }
        }
        "raster-smoke" if args.len() == 4 => {
            write_result(raster_smoke(stdout, stderr, &args[1], &args[2], &args[3]))
        }
        "sample-vrt-rgb" if args.len() == 4 => {
            write_result(sample_vrt_rgb(stdout, stderr, &args[1], &args[2], &args[3]))
        }
        "photo-parity-crop" if (10..=13).contains(&args.len()) => write_result(photo_parity_crop(
            stdout,
            stderr,
            &args[1],
            &args[2],
            &args[3],
            &args[4],
            &args[5],
            &args[6],
            &args[7],
            &args[8],
            &args[9],
            args.get(10).map(String::as_str),
            args.get(11).map(String::as_str),
            args.get(12).map(String::as_str).unwrap_or("all"),
        )),
        "photo-compare-crop" if (8..=10).contains(&args.len()) => write_result(photo_compare_crop(
            stdout,
            stderr,
            &args[1],
            &args[2],
            &args[3],
            &args[4],
            &args[5],
            &args[6],
            &args[7],
            args.get(8).map(String::as_str),
            args.get(9).map(String::as_str).unwrap_or("all"),
        )),
        "photo-parity-metric-crop" if (9..=11).contains(&args.len()) => {
            write_result(photo_parity_metric_crop(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                &args[7],
                &args[8],
                args.get(9).map(String::as_str),
                args.get(10).map(String::as_str).unwrap_or("all"),
            ))
        }
        "photo-parity-metric-batch" if args.len() == 3 || args.len() == 4 => {
            write_result(photo_parity_metric_batch(
                stdout,
                stderr,
                &args[1],
                &args[2],
                args.get(3).map(String::as_str).unwrap_or("auto"),
            ))
        }
        "quality-production-sample-batch" if args.len() >= 7 => {
            write_result(quality_production_sample_batch(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                &args[7..],
            ))
        }
        "photo-standard-remap-parity-crop" if (8..=10).contains(&args.len()) => {
            write_result(photo_standard_remap_parity_crop(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                &args[7],
                args.get(8).map(String::as_str),
                args.get(9).map(String::as_str).unwrap_or("all"),
            ))
        }
        "photo-standard-remap-parity-batch" if args.len() == 3 || args.len() == 4 => {
            write_result(photo_standard_remap_parity_batch(
                stdout,
                stderr,
                &args[1],
                &args[2],
                args.get(3).map(String::as_str).unwrap_or("auto"),
            ))
        }
        "photo-production-candidate-diff-crop" if (10..=12).contains(&args.len()) => {
            write_result(photo_production_candidate_diff_crop(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                &args[7],
                &args[8],
                &args[9],
                args.get(10).map(String::as_str),
                args.get(11).map(String::as_str).unwrap_or("all"),
            ))
        }
        "photo-carrier-remap-sim-crop" if args.len() == 11 || args.len() == 12 => {
            write_result(photo_carrier_remap_sim_crop(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                &args[7],
                &args[8],
                &args[9],
                &args[10],
                args.get(11).map(String::as_str).unwrap_or("8"),
            ))
        }
        "generate-height-region" if args.len() == 7 => write_result(generate_height_region(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
        )),
        "generate-height-region" if args.len() == 6 => {
            match default_heightmap_or_print(stderr, "generate-height-region") {
                Some(heightmap) => write_result(generate_height_region(
                    stdout, stderr, &heightmap, &args[1], &args[2], &args[3], &args[4], &args[5],
                )),
                None => EXIT_USAGE,
            }
        }
        "generate-surface-region" if args.len() == 7 => write_result(generate_surface_region(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
        )),
        "generate-surface-region" if args.len() == 6 => {
            match default_heightmap_or_print(stderr, "generate-surface-region") {
                Some(heightmap) => write_result(generate_surface_region(
                    stdout, stderr, &heightmap, &args[1], &args[2], &args[3], &args[4], &args[5],
                )),
                None => EXIT_USAGE,
            }
        }
        "generate-survival-region" | "generate-survival-region-osm-synthetic"
            if args.len() == 7 =>
        {
            write_result(generate_survival_region_alias(
                stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
            ))
        }
        "generate-survival-region-osm-pbf" if args.len() == 9 => {
            write_result(generate_survival_region_osm_pbf(
                stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
                &args[7], &args[8],
            ))
        }
        "generate-survival-region-osm-pbf-ref-window" if args.len() == 11 => {
            write_result(generate_survival_region_osm_pbf_ref_window(
                stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
                &args[7], &args[8], &args[9], &args[10],
            ))
        }
        "generate-survival-region-osm-pbf-full-scan" if (9..=11).contains(&args.len()) => {
            write_result(generate_survival_region_osm_pbf_full_scan(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                &args[7],
                &args[8],
                args.get(9).map(String::as_str),
                args.get(10).map(String::as_str),
            ))
        }
        "generate-survival-region-osm-xml-cache" if args.len() == 8 => {
            write_result(generate_survival_region_osm_xml_cache(
                stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
                &args[7],
            ))
        }
        "generate-survival-regions-parallel"
        | "generate-survival-regions-parallel-osm-synthetic"
            if args.len() == 10 || args.len() == 11 =>
        {
            write_result(generate_survival_regions_parallel_alias(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                &args[7],
                &args[8],
                &args[9],
                args.get(10).map(String::as_str),
            ))
        }
        "generate-survival-region-plan-parallel"
        | "generate-survival-region-plan-parallel-osm-synthetic"
            if args.len() == 7 || args.len() == 8 =>
        {
            write_result(generate_survival_region_plan_parallel_alias(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                args.get(7).map(String::as_str),
            ))
        }
        "quality-candidate" if (6..=9).contains(&args.len()) => {
            match quality_candidate_args(&args) {
                Ok(Some(parsed)) => write_result(generate_quality_candidate(
                    stdout,
                    stderr,
                    &parsed.heightmap_path,
                    parsed.world_dir,
                    parsed.scale,
                    parsed.region_x,
                    parsed.region_z,
                    parsed.format,
                    parsed.surface_raster,
                    parsed.sample_grid,
                )),
                Ok(None) => write_result(print_usage_error(
                    stderr,
                    "Invalid quality-candidate arguments. Use --help.",
                )),
                Err(error) => write_result(print_usage_error(stderr, error)),
            }
        }
        "benchmark-height-regions" if args.len() == 9 => write_result(benchmark_height_regions(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6], &args[7],
            &args[8],
        )),
        "benchmark-surface-regions" if args.len() == 9 => write_result(benchmark_surface_regions(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6], &args[7],
            &args[8],
        )),
        "benchmark-survival-regions" if args.len() == 9 => {
            write_result(benchmark_survival_regions(
                stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
                &args[7], &args[8],
            ))
        }
        "benchmark-survival-regions-parallel" if args.len() == 10 => {
            write_result(benchmark_survival_regions_parallel(
                stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
                &args[7], &args[8], &args[9],
            ))
        }
        "benchmark-region-writers" if args.len() == 2 => write_result(benchmark_region_writers(
            stdout,
            stderr,
            &args[1],
            "iterations=3",
        )),
        "benchmark-region-writers" if args.len() == 3 => {
            write_result(benchmark_region_writers(stdout, stderr, &args[1], &args[2]))
        }
        "benchmark-surface-input-open" if args.len() == 2 => {
            write_result(benchmark_surface_input_open(stdout, stderr, &args[1]))
        }
        "playability-smoke" if args.len() == 3 => {
            write_result(playability_smoke(stdout, stderr, &args[1], &args[2]))
        }
        "generate"
            if args.len() >= 10 && vanilla_delegated_regions_parallel_args(&args).is_some() =>
        {
            let parsed = vanilla_delegated_regions_parallel_args(&args)
                .expect("validated generate alias args");
            write_result(generate_vanilla_delegated_regions_parallel(
                stdout,
                stderr,
                parsed.heightmap_path,
                parsed.world_dir,
                parsed.scale,
                parsed.start_region_x,
                parsed.start_region_z,
                parsed.cols,
                parsed.rows,
                parsed.format,
                parsed.threads,
                parsed.status,
                parsed.surface_raster,
                parsed.extra_options,
            ))
        }
        "generate-vanilla-delegated-region" if args.len() >= 6 => {
            match vanilla_delegated_args(&args) {
                Ok(Some(parsed)) => write_result(generate_vanilla_delegated_region(
                    stdout,
                    stderr,
                    &parsed.heightmap_path,
                    parsed.world_dir,
                    parsed.scale,
                    parsed.region_x,
                    parsed.region_z,
                    parsed.format,
                    parsed.status,
                    parsed.surface_raster,
                    parsed.extra_options,
                )),
                Ok(None) => write_result(print_usage_error(
                    stderr,
                    "Invalid generate-vanilla-delegated-region arguments. Use --help.",
                )),
                Err(error) => write_result(print_usage_error(stderr, error)),
            }
        }
        "generate-vanilla-delegated-regions-parallel"
            if args.len() >= 10 && vanilla_delegated_regions_parallel_args(&args).is_some() =>
        {
            let parsed = vanilla_delegated_regions_parallel_args(&args)
                .expect("validated vanilla delegated parallel args");
            write_result(generate_vanilla_delegated_regions_parallel(
                stdout,
                stderr,
                parsed.heightmap_path,
                parsed.world_dir,
                parsed.scale,
                parsed.start_region_x,
                parsed.start_region_z,
                parsed.cols,
                parsed.rows,
                parsed.format,
                parsed.threads,
                parsed.status,
                parsed.surface_raster,
                parsed.extra_options,
            ))
        }
        "generate-vanilla-delegated-plan-parallel"
        | "generate-vanilla-delegated-region-plan-parallel"
            if args.len() >= 7 =>
        {
            write_result(generate_vanilla_delegated_plan_parallel(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                &args[7..],
            ))
        }
        "plan-representative-regions" if args.len() == 5 => write_result(
            plan_representative_regions(stdout, stderr, &args[1], &args[2], &args[3], &args[4]),
        ),
        "describe-earth-grid" if args.len() == 3 => {
            write_result(describe_earth_grid(stdout, stderr, &args[1], &args[2]))
        }
        "validate-surface-spawn" if args.len() == 5 => write_result(validate_surface_spawn(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4],
        )),
        "validate-height-seam" if args.len() == 6 => write_result(validate_height_seam(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5],
        )),
        "write-vanilla-finalization-commands" if (6..=8).contains(&args.len()) => {
            write_result(write_vanilla_finalization_commands(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                args.get(6).map(String::as_str),
                args.get(7).map(String::as_str),
            ))
        }
        "validate-survival-manifest" if args.len() == 2 => {
            write_result(validate_survival_manifest(stdout, stderr, &args[1]))
        }
        "apply-survival-evidence" if args.len() == 7 => write_result(apply_survival_evidence(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
        )),
        "validate-cave-density" if args.len() == 5 => write_result(validate_cave_density(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4],
        )),
        "validate-cave-connectivity" if args.len() == 5 => write_result(
            validate_cave_connectivity(stdout, stderr, &args[1], &args[2], &args[3], &args[4]),
        ),
        "validate-ore-histogram-synthetic" if args.len() == 1 => {
            write_result(validate_ore_histogram_synthetic(stdout, stderr))
        }
        "validate-underground-fluid-synthetic" if args.len() == 1 => {
            write_result(validate_underground_fluid_synthetic(stdout, stderr))
        }
        "validate-global-resource-fairness" if args.len() == 3 => write_result(
            validate_global_resource_fairness(stdout, stderr, &args[1], &args[2]),
        ),
        "validate-loot-economy" if args.len() == 3 => {
            write_result(validate_loot_economy(stdout, stderr, &args[1], &args[2]))
        }
        "generate-nation-war-readiness-report" if (6..=10).contains(&args.len()) => {
            write_result(generate_nation_war_readiness_report(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                args.get(6).map(String::as_str),
                args.get(7).map(String::as_str),
                args.get(8).map(String::as_str),
                args.get(9).map(String::as_str),
            ))
        }
        "scan-osm-pbf" if args.len() == 3 => {
            write_result(scan_osm_pbf(stdout, stderr, &args[1], &args[2]))
        }
        "scan-osm-pbf-range" if args.len() == 4 => write_result(scan_osm_pbf_range(
            stdout, stderr, &args[1], &args[2], &args[3],
        )),
        "validate-osm-pbf" if args.len() == 3 => {
            write_result(validate_osm_pbf(stdout, stderr, &args[1], &args[2]))
        }
        "benchmark-osm-index" if args.len() == 5 => write_result(benchmark_osm_index(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4],
        )),
        "extract-osm-region-mask" if args.len() == 6 => write_result(extract_osm_region_mask(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5],
        )),
        "extract-osm-region-mask-window" if args.len() == 8 => {
            write_result(extract_osm_region_mask_window(
                stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
                &args[7],
            ))
        }
        "extract-osm-region-mask-ref-window" if args.len() == 8 => {
            write_result(extract_osm_region_mask_ref_window(
                stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
                &args[7],
            ))
        }
        "extract-osm-region-mask-full-scan" if (5..=8).contains(&args.len()) => {
            write_result(extract_osm_region_mask_full_scan(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                args.get(5).map(String::as_str),
                args.get(6).map(String::as_str),
                args.get(7).map(String::as_str),
            ))
        }
        "extract-osm-xml-region-mask" if args.len() == 5 => write_result(
            extract_osm_xml_region_mask(stdout, stderr, &args[1], &args[2], &args[3], &args[4]),
        ),
        "identify-osm-xml-cache" if args.len() == 2 => {
            write_result(identify_osm_xml_cache(stdout, stderr, &args[1]))
        }
        "trace-surface-region-column" if (6..=8).contains(&args.len()) => {
            match trace_column_args(&args) {
                Ok(parsed) => write_result(trace_surface_region_column(
                    stdout,
                    stderr,
                    &parsed.heightmap_path,
                    parsed.scale,
                    parsed.region_x,
                    parsed.region_z,
                    parsed.local_x,
                    parsed.local_z,
                    parsed.surface_raster,
                )),
                Err(error) => write_result(print_usage_error(stderr, error)),
            }
        }
        "trace-surface-region-cell" if (8..=10).contains(&args.len()) => {
            match trace_cell_args(&args) {
                Ok(parsed) => write_result(trace_surface_region_cell(
                    stdout,
                    stderr,
                    &parsed.heightmap_path,
                    parsed.scale,
                    parsed.region_x,
                    parsed.region_z,
                    parsed.chunk_local_x,
                    parsed.chunk_local_z,
                    parsed.cell_x,
                    parsed.cell_z,
                    parsed.surface_raster,
                )),
                Err(error) => write_result(print_usage_error(stderr, error)),
            }
        }
        "write-sha256-manifest" if args.len() == 3 => {
            write_result(write_sha256_manifest(stdout, stderr, &args[1], &args[2]))
        }
        "write-region-payload-manifest" if args.len() == 3 => write_result(
            write_region_payload_manifest(stdout, stderr, &args[1], &args[2], false),
        ),
        "append-region-payload-manifest" if args.len() == 3 => write_result(
            write_region_payload_manifest(stdout, stderr, &args[1], &args[2], true),
        ),
        "compare-region-payload-manifest" if args.len() == 3 => write_result(
            compare_region_payload_manifest(stdout, stderr, &args[1], &args[2]),
        ),
        "validate-mca-region" if args.len() == 2 => {
            write_result(validate_mca_region(stdout, stderr, &args[1]))
        }
        "validate-linear-region" if args.len() == 2 => {
            write_result(validate_linear_region(stdout, stderr, &args[1]))
        }
        "compare-mca-linear-region-payloads" if args.len() == 3 => write_result(
            compare_mca_linear_region_payloads_cli(stdout, stderr, &args[1], &args[2]),
        ),
        "mca-topdown-render" if args.len() == 7 || args.len() == 8 => {
            write_result(topdown_render_cli(
                stdout,
                stderr,
                TopdownFormat::Mca,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                args.get(7).map(String::as_str),
            ))
        }
        "linear-topdown-render" if args.len() == 7 || args.len() == 8 => {
            write_result(topdown_render_cli(
                stdout,
                stderr,
                TopdownFormat::Linear,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                &args[5],
                &args[6],
                args.get(7).map(String::as_str),
            ))
        }
        "dynmap-tile-mosaic" if args.len() == 3 || args.len() == 4 => {
            write_result(dynmap_tile_mosaic_cli(
                stdout,
                stderr,
                &args[1],
                &args[2],
                args.get(3).map(String::as_str).unwrap_or("base"),
            ))
        }
        "convert-mca-region-to-linear" if args.len() == 3 => write_result(
            convert_mca_region_to_linear(stdout, stderr, &args[1], &args[2]),
        ),
        "convert-mca-world-to-linear" if args.len() == 3 => write_result(
            convert_mca_world_to_linear(stdout, stderr, &args[1], &args[2]),
        ),
        "inspect-mca-palettes" if args.len() == 2 => write_result(inspect_block_palettes(
            stdout,
            stderr,
            &args[1],
            "MCA palettes scanned",
            "MCA palette inspection failed",
        )),
        "validate-mca-survival-palette" if args.len() == 2 => {
            write_result(validate_survival_palette(
                stdout,
                stderr,
                &args[1],
                "MCA survival palette scanned",
                "MCA survival palette validation failed",
            ))
        }
        "inspect-linear-palettes" if args.len() == 2 => write_result(inspect_block_palettes(
            stdout,
            stderr,
            &args[1],
            "Linear palettes scanned",
            "Linear palette inspection failed",
        )),
        "validate-linear-survival-palette" if args.len() == 2 => {
            write_result(validate_survival_palette(
                stdout,
                stderr,
                &args[1],
                "Linear survival palette scanned",
                "Linear survival palette validation failed",
            ))
        }
        "inspect-mca-biomes" if args.len() == 2 => write_result(inspect_biomes(
            stdout,
            stderr,
            &args[1],
            "MCA biomes scanned",
            "MCA biome inspection failed",
        )),
        "inspect-linear-biomes" if args.len() == 2 => write_result(inspect_biomes(
            stdout,
            stderr,
            &args[1],
            "Linear biomes scanned",
            "Linear biome inspection failed",
        )),
        "inspect-mca-statuses" if args.len() == 2 => write_result(inspect_statuses(
            stdout,
            stderr,
            &args[1],
            "MCA statuses scanned",
            "MCA status inspection failed",
        )),
        "inspect-linear-statuses" if args.len() == 2 => write_result(inspect_statuses(
            stdout,
            stderr,
            &args[1],
            "Linear statuses scanned",
            "Linear status inspection failed",
        )),
        "inspect-mca-post-final-integrity" if args.len() == 2 => {
            write_result(inspect_post_final_integrity(
                stdout,
                stderr,
                &args[1],
                RegionFormat::Mca,
                "MCA post-final integrity scanned",
                "MCA post-final integrity inspection failed",
            ))
        }
        "inspect-linear-post-final-integrity" if args.len() == 2 => {
            write_result(inspect_post_final_integrity(
                stdout,
                stderr,
                &args[1],
                RegionFormat::Linear,
                "Linear post-final integrity scanned",
                "Linear post-final integrity inspection failed",
            ))
        }
        "rewrite-mca-status" if args.len() == 3 => {
            write_result(rewrite_mca_status(stdout, stderr, &args[1], &args[2]))
        }
        "repair-mca-post-final-water" if args.len() == 2 => {
            write_result(repair_mca_post_final_water(stdout, stderr, &args[1]))
        }
        "repair-linear-sandlike-surfaces" if args.len() == 2 => {
            write_result(repair_linear_sandlike_surfaces(stdout, stderr, &args[1]))
        }
        "summarize-region-chunk" if args.len() == 4 => write_result(summarize_region_chunk(
            stdout, stderr, &args[1], &args[2], &args[3],
        )),
        "compare-region-chunk-details" if args.len() == 5 || args.len() == 7 => {
            write_result(compare_region_chunk_details(
                stdout,
                stderr,
                &args[1],
                &args[2],
                &args[3],
                &args[4],
                args.get(5).map(String::as_str),
                args.get(6).map(String::as_str),
            ))
        }
        "generate-flat-test-world" if args.len() == 3 => {
            write_result(generate_flat_test_world(stdout, stderr, &args[1], &args[2]))
        }
        "generate-palette-stress-world" if args.len() == 2 => {
            write_result(generate_palette_stress_world(stdout, stderr, &args[1]))
        }
        "write-nbt-parity-fixtures" if args.len() == 2 => {
            write_result(write_nbt_parity_fixtures(stdout, stderr, &args[1]))
        }
        "write-nbt-gzip-parity-fixtures" if args.len() == 2 => {
            write_result(write_nbt_gzip_parity_fixtures(stdout, stderr, &args[1]))
        }
        "write-region-writer-parity-fixtures" if args.len() == 2 => write_result(
            write_region_writer_parity_fixtures(stdout, stderr, &args[1]),
        ),
        command => {
            if let Some(spec) = commands::find_initial_command(command) {
                if spec.status == CommandStatus::NotPortedYet {
                    return write_result(print_not_implemented(stderr, spec.name));
                }
            }
            let _ = writeln!(stderr, "Unknown command. Use --help.");
            EXIT_USAGE
        }
    }
}

fn has(args: &[String], value: &str) -> bool {
    args.iter().any(|arg| arg == value)
}

fn configured_path_from_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).and_then(|value| {
        let text = value.to_string_lossy().trim().to_string();
        (!text.is_empty()).then(|| PathBuf::from(text))
    })
}

fn configured_default_heightmap_path() -> Option<String> {
    configured_path_from_env(HEIGHTMAP_PATH_ENV).map(|path| path.display().to_string())
}

fn default_heightmap_or_print(err: &mut impl Write, command: &str) -> Option<String> {
    let heightmap = configured_default_heightmap_path();
    if heightmap.is_none() {
        let _ = writeln!(
            err,
            "{command} requires <heightmap> unless {HEIGHTMAP_PATH_ENV} is set."
        );
    }
    heightmap
}

fn missing_default_heightmap_error(command: &str) -> String {
    format!("{command} requires <heightmap> unless {HEIGHTMAP_PATH_ENV} is set")
}

fn print_usage_error(err: &mut impl Write, message: impl AsRef<str>) -> io::Result<i32> {
    writeln!(err, "{}", message.as_ref())?;
    Ok(EXIT_USAGE)
}

fn write_result(result: io::Result<i32>) -> i32 {
    match result {
        Ok(exit_code) => exit_code,
        Err(error) => {
            let _ = writeln!(io::stderr(), "I/O error: {error}");
            1
        }
    }
}

fn photo_compare_crop(
    out: &mut impl Write,
    err: &mut impl Write,
    actual_path: &str,
    expected_path: &str,
    output_directory: &str,
    crop_x: &str,
    crop_y: &str,
    crop_width: &str,
    crop_height: &str,
    mask_path: Option<&str>,
    mask_mode: &str,
) -> io::Result<i32> {
    match parse_photo_crop(crop_x, crop_y, crop_width, crop_height).and_then(
        |(crop_x, crop_y, crop_width, crop_height)| {
            let mask_path = parse_optional_cli_path(mask_path);
            quality::write_compare_report(
                Path::new(actual_path),
                Path::new(expected_path),
                Path::new(output_directory),
                crop_x,
                crop_y,
                crop_width,
                crop_height,
                mask_path.as_deref(),
                mask_mode,
            )
        },
    ) {
        Ok(report) => {
            writeln!(out, "Photo compare crop written")?;
            writeln!(
                out,
                "outputDirectory={}",
                cli_path_display(&report.output_directory)
            )?;
            write!(out, "{}", report.text)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Photo compare crop failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn photo_parity_crop(
    out: &mut impl Write,
    err: &mut impl Write,
    source_path: &str,
    met_target_path: &str,
    current_surface_path: &str,
    standard_palette_path: &str,
    output_directory: &str,
    crop_x: &str,
    crop_y: &str,
    crop_width: &str,
    crop_height: &str,
    image_magick_remap_path: Option<&str>,
    mask_path: Option<&str>,
    mask_mode: &str,
) -> io::Result<i32> {
    match parse_photo_crop(crop_x, crop_y, crop_width, crop_height).and_then(
        |(crop_x, crop_y, crop_width, crop_height)| {
            let met_target_path = parse_required_optional_cli_path(met_target_path);
            let current_surface_path = parse_required_optional_cli_path(current_surface_path);
            let image_magick_remap_path = parse_optional_cli_path(image_magick_remap_path);
            let mask_path = parse_optional_cli_path(mask_path);
            quality::write_parity_crop_report(
                Path::new(source_path),
                met_target_path.as_deref(),
                current_surface_path.as_deref(),
                Path::new(standard_palette_path),
                Path::new(output_directory),
                crop_x,
                crop_y,
                crop_width,
                crop_height,
                image_magick_remap_path.as_deref(),
                mask_path.as_deref(),
                mask_mode,
            )
        },
    ) {
        Ok(report) => {
            writeln!(out, "Photo parity crop written")?;
            writeln!(
                out,
                "outputDirectory={}",
                cli_path_display(&report.output_directory)
            )?;
            write!(out, "{}", report.text)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Photo parity crop failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn photo_parity_metric_crop(
    out: &mut impl Write,
    err: &mut impl Write,
    source_path: &str,
    expected_path: &str,
    current_surface_path: &str,
    output_directory: &str,
    crop_x: &str,
    crop_y: &str,
    crop_width: &str,
    crop_height: &str,
    mask_path: Option<&str>,
    mask_mode: &str,
) -> io::Result<i32> {
    match parse_photo_crop(crop_x, crop_y, crop_width, crop_height).and_then(
        |(crop_x, crop_y, crop_width, crop_height)| {
            let mask_path = parse_optional_cli_path(mask_path);
            quality::write_metric_crop_report(
                Path::new(source_path),
                Path::new(expected_path),
                Path::new(current_surface_path),
                Path::new(output_directory),
                crop_x,
                crop_y,
                crop_width,
                crop_height,
                mask_path.as_deref(),
                mask_mode,
            )
        },
    ) {
        Ok(report) => {
            writeln!(out, "Photo parity metric crop written")?;
            writeln!(
                out,
                "outputDirectory={}",
                cli_path_display(&report.output_directory)
            )?;
            write!(out, "{}", report.text)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Photo parity metric crop failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn photo_parity_metric_batch(
    out: &mut impl Write,
    err: &mut impl Write,
    jobs_csv: &str,
    output_root: &str,
    threads: &str,
) -> io::Result<i32> {
    match quality::run_photo_metric_batch(Path::new(jobs_csv), Path::new(output_root), threads) {
        Ok(report) => {
            writeln!(out, "Photo parity metric batch written")?;
            writeln!(out, "jobsCsv={}", cli_path_display(&report.jobs_csv))?;
            writeln!(out, "outputRoot={}", cli_path_display(&report.output_root))?;
            writeln!(out, "jobs={}", report.jobs)?;
            writeln!(out, "threads={}", report.threads)?;
            writeln!(out, "elapsedMillis={}", report.elapsed_millis)?;
            writeln!(
                out,
                "sample,elapsedMillis,pixels,currentVsExpectedMean,currentVsSourceMean,\
sourceVsExpectedMean,canopyVsExpectedMean,canopyVsSourceMean,\
selectiveCanopyVsExpectedMean,selectiveCanopyVsSourceMean,outputDirectory"
            )?;
            for result in report.results {
                writeln!(
                    out,
                    "{},{},{},{},{},{},{},{},{},{},{}",
                    result.sample,
                    result.elapsed_millis,
                    result.pixels,
                    metric_text(result.current_vs_expected_mean),
                    metric_text(result.current_vs_source_mean),
                    metric_text(result.source_vs_expected_mean),
                    metric_text(result.canopy_vs_expected_mean),
                    metric_text(result.canopy_vs_source_mean),
                    metric_text(result.selective_canopy_vs_expected_mean),
                    metric_text(result.selective_canopy_vs_source_mean),
                    cli_path_display(&result.output_directory)
                )?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Photo parity metric batch failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProductionMetricMode {
    Full,
    CurrentOnly,
}

impl ProductionMetricMode {
    fn parse(text: &str) -> std::result::Result<Self, String> {
        if text.trim().is_empty() {
            return Err("metricMode must be full or current-only".to_string());
        }
        let normalized = text.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "full" | "candidate" | "candidate-full" => Ok(Self::Full),
            "current" | "current-only" | "currentonly" | "production-current" => {
                Ok(Self::CurrentOnly)
            }
            _ => Err(format!("metricMode must be full or current-only: {text}")),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::CurrentOnly => "current-only",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProductionPreviewDebug {
    Off,
    Auto,
    Directory(std::path::PathBuf),
}

impl ProductionPreviewDebug {
    fn parse(text: &str) -> std::result::Result<Self, String> {
        let trimmed = text.trim();
        if trimmed.is_empty()
            || matches_ignore_ascii_case(trimmed, &["none", "off", "false", "null"])
        {
            return Ok(Self::Off);
        }
        if matches_ignore_ascii_case(trimmed, &["auto", "true", "on"]) {
            return Ok(Self::Auto);
        }
        Ok(Self::Directory(std::path::PathBuf::from(trimmed)))
    }

    fn id(&self) -> String {
        match self {
            Self::Off => "off".to_string(),
            Self::Auto => "auto".to_string(),
            Self::Directory(path) => normalized_path_display(path),
        }
    }

    fn directory_for(&self, job: &ProductionSampleJob) -> Option<std::path::PathBuf> {
        match self {
            Self::Off => None,
            Self::Auto => Some(job.output_directory.join("preview-debug")),
            Self::Directory(root) => Some(root.join(&job.sample)),
        }
    }
}

#[derive(Clone, Debug)]
struct ProductionSampleOptions {
    cache_rows: usize,
    prefetch_rows: usize,
    vertical_scale: f64,
    texture_mode: SurfaceTextureMode,
    surface_raster: String,
    chunk_status: ChunkGenerationStatus,
    metric_mode: ProductionMetricMode,
    preview_debug: ProductionPreviewDebug,
}

#[derive(Clone, Debug)]
struct ProductionSampleJob {
    sample: String,
    region_x: i32,
    region_z: i32,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    source: std::path::PathBuf,
    expected_standard: std::path::PathBuf,
    land_mask: Option<std::path::PathBuf>,
    mask_mode: String,
    output_directory: std::path::PathBuf,
}

#[derive(Clone, Debug)]
struct ProductionSampleJobResult {
    sample: String,
    output_directory: std::path::PathBuf,
    elapsed_millis: u128,
    generation_millis: u128,
    render_millis: u128,
    metric_millis: u128,
    region_file: std::path::PathBuf,
    current_render: std::path::PathBuf,
    metric_directory: std::path::PathBuf,
    current_vs_expected_mean: f64,
    current_vs_source_mean: f64,
    preview_debug_directory: Option<std::path::PathBuf>,
    production_source_debug: Option<std::path::PathBuf>,
    production_source_vs_reference_mean: f64,
    production_source_vs_expected_mean: f64,
    missing_regions: usize,
    missing_chunks: usize,
    evidence_json: std::path::PathBuf,
}

#[derive(Clone, Debug)]
struct ProductionSampleBatchReport {
    samples: usize,
    requested_threads: usize,
    elapsed_millis: u128,
    summary_csv: std::path::PathBuf,
    contact_sheet: std::path::PathBuf,
    results: Vec<ProductionSampleJobResult>,
}

#[allow(clippy::too_many_arguments)]
fn quality_production_sample_batch(
    out: &mut impl Write,
    err: &mut impl Write,
    samples_csv: &str,
    heightmap: &str,
    output_root: &str,
    scale: &str,
    format: &str,
    threads: &str,
    optional_args: &[String],
) -> io::Result<i32> {
    match quality_production_sample_batch_impl(
        Path::new(samples_csv),
        Path::new(heightmap),
        Path::new(output_root),
        scale,
        format,
        threads,
        optional_args,
    ) {
        Ok(report) => {
            writeln!(out, "Quality production sample batch complete")?;
            writeln!(out, "samples={}", report.samples)?;
            writeln!(out, "requestedThreads={}", report.requested_threads)?;
            writeln!(out, "execution=single-rust-sequential")?;
            writeln!(out, "elapsedMillis={}", report.elapsed_millis)?;
            writeln!(
                out,
                "summaryCsv={}",
                normalized_path_display(&report.summary_csv)
            )?;
            writeln!(
                out,
                "contactSheet={}",
                normalized_path_display(&report.contact_sheet)
            )?;
            writeln!(
                out,
                "sample,elapsedMillis,generationMillis,renderMillis,metricMillis,currentVsExpectedMean,currentVsSourceMean,outputDirectory"
            )?;
            for result in &report.results {
                writeln!(
                    out,
                    "{},{},{},{},{},{},{},{}",
                    csv_field(&result.sample),
                    result.elapsed_millis,
                    result.generation_millis,
                    result.render_millis,
                    result.metric_millis,
                    production_metric_text(result.current_vs_expected_mean),
                    production_metric_text(result.current_vs_source_mean),
                    csv_field(&normalized_path_display(&result.output_directory))
                )?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Quality production sample batch failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn quality_production_sample_batch_impl(
    samples_csv: &Path,
    heightmap: &Path,
    output_root: &Path,
    scale_text: &str,
    format_text: &str,
    threads_text: &str,
    optional_args: &[String],
) -> std::result::Result<ProductionSampleBatchReport, String> {
    let scale = parse_positive_i32_string("scale", scale_text)?;
    let format = parse_quality_sample_output_format(format_text)?;
    let requested_threads = parse_positive_usize_string("threads", threads_text)?;
    let options = parse_production_sample_options(optional_args, heightmap)?;
    let jobs = read_production_sample_jobs(samples_csv, output_root)?;
    if jobs.is_empty() {
        return Err("samplesCsv contains no jobs".to_string());
    }
    std::fs::create_dir_all(output_root).map_err(|error| error.to_string())?;

    let surface_material_path =
        parse_optional_surface_material_path(&options.surface_raster, heightmap)?;
    if options.texture_mode == SurfaceTextureMode::Photo && surface_material_path.is_none() {
        return Err(
            "textureMode=photo requires surfaceRaster=auto or an explicit path".to_string(),
        );
    }
    let surface_tile_cache_entries =
        configured_surface_tile_cache_entries(surface_material_path.as_deref())?;
    let surface_material_sampler = if options.texture_mode == SurfaceTextureMode::Photo {
        surface_material_path
            .as_ref()
            .map(|path| {
                EarthDataSurfaceMaterialSampler::open_with_tile_cache_entries(
                    path,
                    surface_tile_cache_entries,
                )
                .map_err(|error| error.to_string())
            })
            .transpose()?
    } else {
        None
    };

    let batch_start = Instant::now();
    let mut results = Vec::with_capacity(jobs.len());
    for job in &jobs {
        results.push(run_production_sample_job(
            job,
            heightmap,
            scale,
            format,
            &options,
            surface_material_path.as_deref(),
            surface_tile_cache_entries,
            surface_material_sampler.as_ref(),
        )?);
    }

    let summary_csv = output_root.join("quality-production-sample-summary.csv");
    write_text(&summary_csv, &production_sample_summary_csv(&results))?;
    let contact_sheet = write_production_sample_contact_sheet(
        &results,
        &output_root.join("quality-production-sample-contact-sheet.png"),
    )?;

    Ok(ProductionSampleBatchReport {
        samples: jobs.len(),
        requested_threads,
        elapsed_millis: batch_start.elapsed().as_millis(),
        summary_csv,
        contact_sheet,
        results,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_production_sample_job(
    job: &ProductionSampleJob,
    heightmap: &Path,
    scale: i32,
    format: OutputFormat,
    options: &ProductionSampleOptions,
    surface_material_path: Option<&Path>,
    surface_tile_cache_entries: usize,
    surface_material_sampler: Option<&EarthDataSurfaceMaterialSampler>,
) -> std::result::Result<ProductionSampleJobResult, String> {
    let sample_start = Instant::now();
    std::fs::create_dir_all(&job.output_directory).map_err(|error| error.to_string())?;
    let world_dir = job.output_directory.join("world");
    let photo_parity_dir = job.output_directory.join("photo-parity");
    std::fs::create_dir_all(&photo_parity_dir).map_err(|error| error.to_string())?;
    let preview_debug_directory = options.preview_debug.directory_for(job);
    if let Some(directory) = &preview_debug_directory {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }

    let generation_start = Instant::now();
    let mut settings = SurfaceRegionSettings::new_with_texture_options(
        heightmap,
        &world_dir,
        format!("SR EarthMap Quality {}", job.sample),
        0,
        scale,
        job.region_x,
        job.region_z,
        format,
        options.cache_rows,
        true,
        options.chunk_status,
        options.vertical_scale,
        options.texture_mode,
    )
    .map_err(|error| error.to_string())?;
    settings.surface_material_path = surface_material_path.map(Path::to_path_buf);
    settings.surface_tile_cache_entries = surface_tile_cache_entries;
    let surface_report =
        generate_surface_region_with_open_material_sampler(&settings, surface_material_sampler)
            .map_err(|error| error.to_string())?;
    let generation_millis = generation_start.elapsed().as_millis();

    let render_start = Instant::now();
    let current_render = photo_parity_dir.join("mca-visible-topdown.png");
    let render_report = topdown_render_impl(
        TopdownFormat::Mca,
        &world_dir,
        &current_render,
        &job.region_x.to_string(),
        &job.region_z.to_string(),
        "1",
        "1",
        Some("visible"),
    )?;
    let render_millis = render_start.elapsed().as_millis();

    let metric_start = Instant::now();
    let metric_directory = photo_parity_dir.join("metric-land");
    let metric_report = match options.metric_mode {
        ProductionMetricMode::CurrentOnly => quality::write_current_metric_crop_report(
            &job.source,
            &job.expected_standard,
            &current_render,
            &metric_directory,
            job.crop_x,
            job.crop_y,
            job.crop_width,
            job.crop_height,
            job.land_mask.as_deref(),
            &job.mask_mode,
        ),
        ProductionMetricMode::Full => quality::write_metric_crop_report(
            &job.source,
            &job.expected_standard,
            &current_render,
            &metric_directory,
            job.crop_x,
            job.crop_y,
            job.crop_width,
            job.crop_height,
            job.land_mask.as_deref(),
            &job.mask_mode,
        ),
    }
    .map_err(|error| error.to_string())?;
    let metric_millis = metric_start.elapsed().as_millis();

    let current_vs_expected_mean = quality_metric_mean(&metric_report, "current-vs-expected");
    let current_vs_source_mean = quality_metric_mean(&metric_report, "current-vs-source");
    let evidence_json = job
        .output_directory
        .join("quality-production-sample-evidence.json");

    let result = ProductionSampleJobResult {
        sample: job.sample.clone(),
        output_directory: job.output_directory.clone(),
        elapsed_millis: sample_start.elapsed().as_millis(),
        generation_millis,
        render_millis,
        metric_millis,
        region_file: surface_report.region_file.clone(),
        current_render,
        metric_directory: metric_report.output_directory.clone(),
        current_vs_expected_mean,
        current_vs_source_mean,
        preview_debug_directory,
        production_source_debug: None,
        production_source_vs_reference_mean: f64::NAN,
        production_source_vs_expected_mean: f64::NAN,
        missing_regions: render_report.stats.missing_regions,
        missing_chunks: render_report.stats.missing_chunks,
        evidence_json,
    };

    write_production_sample_properties(job, options, &surface_report, &render_report, &result)?;
    write_production_sample_evidence_json(
        job,
        options,
        &surface_report,
        &render_report,
        &result,
        surface_material_path,
        surface_tile_cache_entries,
    )?;
    Ok(result)
}

fn parse_quality_sample_output_format(text: &str) -> std::result::Result<OutputFormat, String> {
    let format = OutputFormat::parse(text).map_err(|error| error.to_string())?;
    if format != OutputFormat::Mca {
        return Err(
            "quality-production-sample-batch currently requires mca for topdown parity".to_string(),
        );
    }
    Ok(format)
}

fn parse_production_sample_options(
    optional_args: &[String],
    heightmap_path: &Path,
) -> std::result::Result<ProductionSampleOptions, String> {
    let mut cache_rows = 512usize;
    let mut prefetch_rows = 0usize;
    let mut vertical_scale = 1.25f64;
    let mut texture_mode = SurfaceTextureMode::Photo;
    let mut surface_raster = "auto".to_string();
    let mut chunk_status = ChunkGenerationStatus::Surface;
    let mut metric_mode = ProductionMetricMode::Full;
    let mut preview_debug = ProductionPreviewDebug::Off;

    for option in optional_args {
        if option.trim().is_empty() {
            continue;
        }
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| format!("quality sample options must be key=value: {option}"))?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "cacheRows" | "sharedCacheRows" | "heightmapCacheRows" => {
                cache_rows = parse_optional_auto_positive_usize(value, "cacheRows")?
                    .unwrap_or_else(|| {
                        configured_heightmap_cache_rows(heightmap_path).unwrap_or(512)
                    });
            }
            "prefetchRows" | "readAheadRows" => {
                prefetch_rows =
                    parse_optional_auto_nonnegative_usize(value, "prefetchRows")?.unwrap_or(0);
            }
            "verticalScale" | "heightScale" | "yScale" | "reliefScale" => {
                vertical_scale = value.parse::<f64>().map_err(|error| error.to_string())?;
                if !vertical_scale.is_finite() || vertical_scale <= 0.0 {
                    return Err(format!("verticalScale must be positive: {value}"));
                }
            }
            "textureMode" | "texture" | "surfaceMode" | "renderMode" => {
                texture_mode =
                    SurfaceTextureMode::parse(value).map_err(|error| error.to_string())?;
            }
            "surfaceRaster" | "terrainRaster" | "surfaceMaterial" | "trueMarble" => {
                surface_raster = value.to_string();
            }
            "status" | "chunkStatus" => {
                chunk_status =
                    ChunkGenerationStatus::parse(value).map_err(|error| error.to_string())?;
                if chunk_status == ChunkGenerationStatus::Full {
                    return Err("quality sample chunkStatus must be surface or carvers".to_string());
                }
            }
            "metricMode" | "metricsMode" | "metricScope" | "metricsScope" => {
                metric_mode = ProductionMetricMode::parse(value)?;
            }
            "previewDebug" | "previewDebugDir" | "debugPreview" | "debugPreviewDir"
            | "debugSource" => {
                preview_debug = ProductionPreviewDebug::parse(value)?;
            }
            _ => return Err(format!("unknown quality sample option: {key}")),
        }
    }
    if prefetch_rows >= cache_rows {
        return Err(
            "prefetchRows must be lower than cacheRows for quality sample runs".to_string(),
        );
    }
    if texture_mode == SurfaceTextureMode::Photo
        && parse_optional_surface_material_path(&surface_raster, heightmap_path)?.is_none()
    {
        return Err(
            "textureMode=photo requires surfaceRaster=auto or an explicit path".to_string(),
        );
    }

    Ok(ProductionSampleOptions {
        cache_rows,
        prefetch_rows,
        vertical_scale,
        texture_mode,
        surface_raster,
        chunk_status,
        metric_mode,
        preview_debug,
    })
}

fn parse_optional_auto_positive_usize(
    value: &str,
    name: &str,
) -> std::result::Result<Option<usize>, String> {
    if value.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    let parsed = value.parse::<usize>().map_err(|error| error.to_string())?;
    if parsed == 0 {
        return Err(format!("{name} must be positive: {parsed}"));
    }
    Ok(Some(parsed))
}

fn parse_optional_auto_nonnegative_usize(
    value: &str,
    name: &str,
) -> std::result::Result<Option<usize>, String> {
    if value.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    let parsed = value.parse::<usize>().map_err(|error| error.to_string())?;
    if parsed == usize::MAX {
        return Err(format!("{name} is out of range: {value}"));
    }
    Ok(Some(parsed))
}

fn read_production_sample_jobs(
    samples_csv: &Path,
    output_root: &Path,
) -> std::result::Result<Vec<ProductionSampleJob>, String> {
    let text = std::fs::read_to_string(samples_csv).map_err(|error| error.to_string())?;
    let csv_directory = samples_csv
        .canonicalize()
        .unwrap_or_else(|_| samples_csv.to_path_buf())
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let mut header = Vec::<String>::new();
    let mut header_index = BTreeMap::<String, usize>::new();
    let mut jobs = Vec::<ProductionSampleJob>::new();

    for (line_index, line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let raw_line = strip_utf8_bom(line);
        if raw_line.trim().is_empty() || raw_line.trim_start().starts_with('#') {
            continue;
        }
        let columns = split_csv_line(raw_line)?;
        if header.is_empty() {
            header = columns;
            for (index, name) in header.iter().enumerate() {
                header_index.insert(normalize_csv_header(name), index);
            }
            continue;
        }

        let sample = csv_required(&columns, &header_index, line_number, &["sample"])?;
        let region_x = csv_required(&columns, &header_index, line_number, &["regionX"])?
            .parse::<i32>()
            .map_err(|error| error.to_string())?;
        let region_z = csv_required(&columns, &header_index, line_number, &["regionZ"])?
            .parse::<i32>()
            .map_err(|error| error.to_string())?;
        let crop_x = csv_required(&columns, &header_index, line_number, &["cropX", "x"])?
            .parse::<u32>()
            .map_err(|error| error.to_string())?;
        let crop_y = csv_required(&columns, &header_index, line_number, &["cropY", "y"])?
            .parse::<u32>()
            .map_err(|error| error.to_string())?;
        let crop_width = csv_required(
            &columns,
            &header_index,
            line_number,
            &["cropWidth", "width"],
        )?
        .parse::<u32>()
        .map_err(|error| error.to_string())?;
        let crop_height = csv_required(
            &columns,
            &header_index,
            line_number,
            &["cropHeight", "height"],
        )?
        .parse::<u32>()
        .map_err(|error| error.to_string())?;
        if crop_width == 0 || crop_height == 0 {
            return Err(format!(
                "cropWidth and cropHeight must be positive at line {line_number}"
            ));
        }
        let source = csv_required_input_path(
            &columns,
            &header_index,
            line_number,
            &csv_directory,
            &["sourcePng", "source"],
        )?;
        let expected_standard = csv_required_input_path(
            &columns,
            &header_index,
            line_number,
            &csv_directory,
            &[
                "expectedStandardPng",
                "expectedStandard",
                "expected",
                "standardPng",
            ],
        )?;
        let land_mask = csv_optional_input_path(
            &columns,
            &header_index,
            &csv_directory,
            &["landMaskPng", "landMask", "mask", "maskPng"],
        );
        let mask_mode = csv_optional(&columns, &header_index, &["maskMode", "mode"])
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if land_mask.is_some() {
                    "land".to_string()
                } else {
                    "all".to_string()
                }
            });
        let output_directory = csv_optional_output_path(
            &columns,
            &header_index,
            output_root,
            &["output", "outputDir", "outputDirectory"],
        )
        .unwrap_or_else(|| output_root.join(&sample));

        jobs.push(ProductionSampleJob {
            sample,
            region_x,
            region_z,
            crop_x,
            crop_y,
            crop_width,
            crop_height,
            source,
            expected_standard,
            land_mask,
            mask_mode,
            output_directory,
        });
    }

    if header.is_empty() {
        return Err("samplesCsv does not contain a header row".to_string());
    }
    Ok(jobs)
}

fn strip_utf8_bom(value: &str) -> &str {
    value.strip_prefix('\u{feff}').unwrap_or(value)
}

fn normalize_csv_header(value: &str) -> String {
    value.trim().replace(['_', '-'], "").to_ascii_lowercase()
}

fn split_csv_line(line: &str) -> std::result::Result<Vec<String>, String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            if quoted && chars.peek() == Some(&'"') {
                current.push('"');
                let _ = chars.next();
            } else {
                quoted = !quoted;
            }
        } else if ch == ',' && !quoted {
            values.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(ch);
        }
    }
    if quoted {
        return Err(format!("unterminated quoted CSV value: {line}"));
    }
    values.push(current.trim().to_string());
    Ok(values)
}

fn csv_required(
    columns: &[String],
    header_index: &BTreeMap<String, usize>,
    line_number: usize,
    names: &[&str],
) -> std::result::Result<String, String> {
    let value = csv_optional(columns, header_index, names);
    if value.is_none_or(|value| value.trim().is_empty()) {
        return Err(format!(
            "missing required CSV column {} at line {line_number}",
            names[0]
        ));
    }
    Ok(value.unwrap().to_string())
}

fn csv_optional<'a>(
    columns: &'a [String],
    header_index: &BTreeMap<String, usize>,
    names: &[&str],
) -> Option<&'a str> {
    for name in names {
        if let Some(index) = header_index.get(&normalize_csv_header(name)) {
            if let Some(value) = columns.get(*index) {
                return Some(value);
            }
        }
    }
    None
}

fn csv_required_input_path(
    columns: &[String],
    header_index: &BTreeMap<String, usize>,
    line_number: usize,
    base_directory: &Path,
    names: &[&str],
) -> std::result::Result<std::path::PathBuf, String> {
    let value = csv_required(columns, header_index, line_number, names)?;
    Ok(resolve_csv_path(&value, base_directory))
}

fn csv_optional_input_path(
    columns: &[String],
    header_index: &BTreeMap<String, usize>,
    base_directory: &Path,
    names: &[&str],
) -> Option<std::path::PathBuf> {
    csv_optional(columns, header_index, names)
        .and_then(parse_optional_csv_path)
        .map(|value| resolve_csv_path(value, base_directory))
}

fn csv_optional_output_path(
    columns: &[String],
    header_index: &BTreeMap<String, usize>,
    base_directory: &Path,
    names: &[&str],
) -> Option<std::path::PathBuf> {
    csv_optional(columns, header_index, names)
        .and_then(parse_optional_csv_path)
        .map(|value| resolve_csv_path(value, base_directory))
}

fn parse_optional_csv_path(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() || matches_ignore_ascii_case(trimmed, &["none", "off", "false", "null"]) {
        None
    } else {
        Some(trimmed)
    }
}

fn resolve_csv_path(value: &str, base_directory: &Path) -> std::path::PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_directory.join(path).components().collect()
    }
}

fn quality_metric_mean(report: &quality::Report, name: &str) -> f64 {
    report
        .metric(name)
        .map(|metrics| metrics.mean_delta_e2000)
        .unwrap_or(f64::NAN)
}

fn production_metric_text(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.6}")
    } else {
        "nan".to_string()
    }
}

fn production_path_text(path: Option<&Path>) -> String {
    path.map(normalized_path_display)
        .unwrap_or_else(|| "none".to_string())
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn production_sample_summary_csv(results: &[ProductionSampleJobResult]) -> String {
    let mut text = String::from(
        "sample,elapsedMillis,generationMillis,renderMillis,metricMillis,\
currentVsExpectedMean,currentVsSourceMean,missingRegions,missingChunks,\
outputDirectory,currentRender,metricDirectory,previewDebugDirectory,productionSourceDebug,\
productionSourceVsReferenceMean,productionSourceVsExpectedMean,evidenceJson\n",
    );
    for result in results {
        let row = [
            csv_field(&result.sample),
            result.elapsed_millis.to_string(),
            result.generation_millis.to_string(),
            result.render_millis.to_string(),
            result.metric_millis.to_string(),
            production_metric_text(result.current_vs_expected_mean),
            production_metric_text(result.current_vs_source_mean),
            result.missing_regions.to_string(),
            result.missing_chunks.to_string(),
            csv_field(&normalized_path_display(&result.output_directory)),
            csv_field(&normalized_path_display(&result.current_render)),
            csv_field(&normalized_path_display(&result.metric_directory)),
            csv_field(&production_path_text(
                result.preview_debug_directory.as_deref(),
            )),
            csv_field(&production_path_text(
                result.production_source_debug.as_deref(),
            )),
            production_metric_text(result.production_source_vs_reference_mean),
            production_metric_text(result.production_source_vs_expected_mean),
            csv_field(&normalized_path_display(&result.evidence_json)),
        ];
        text.push_str(&row.join(","));
        text.push('\n');
    }
    text
}

fn write_production_sample_properties(
    job: &ProductionSampleJob,
    options: &ProductionSampleOptions,
    _surface_report: &SurfaceRegionReport,
    render_report: &TopdownReport,
    result: &ProductionSampleJobResult,
) -> std::result::Result<(), String> {
    let text = format!(
        "sample={}\nregionX={}\nregionZ={}\ncropX={}\ncropY={}\ncropWidth={}\n\
cropHeight={}\ncacheRows={}\nprefetchRows={}\nverticalScale={}\ntextureMode={}\n\
chunkStatus={}\nmetricMode={}\npreviewDebug={}\nregionFile={}\ncurrentRender={}\n\
metricDirectory={}\npreviewDebugDirectory={}\nproductionSourceDebug={}\n\
productionSourceVsReferenceMean={}\nproductionSourceVsExpectedMean={}\n\
elapsedMillis={}\ngenerationMillis={}\nrenderMillis={}\nmetricMillis={}\n\
currentVsExpectedMean={}\ncurrentVsSourceMean={}\nmissingRegions={}\nmissingChunks={}\n\
evidenceJson={}\n",
        job.sample,
        job.region_x,
        job.region_z,
        job.crop_x,
        job.crop_y,
        job.crop_width,
        job.crop_height,
        options.cache_rows,
        options.prefetch_rows,
        options.vertical_scale,
        options.texture_mode.id(),
        options.chunk_status.id(),
        options.metric_mode.id(),
        options.preview_debug.id(),
        normalized_path_display(&result.region_file),
        normalized_path_display(&result.current_render),
        normalized_path_display(&result.metric_directory),
        production_path_text(result.preview_debug_directory.as_deref()),
        production_path_text(result.production_source_debug.as_deref()),
        production_metric_text(result.production_source_vs_reference_mean),
        production_metric_text(result.production_source_vs_expected_mean),
        result.elapsed_millis,
        result.generation_millis,
        result.render_millis,
        result.metric_millis,
        production_metric_text(result.current_vs_expected_mean),
        production_metric_text(result.current_vs_source_mean),
        render_report.stats.missing_regions,
        render_report.stats.missing_chunks,
        normalized_path_display(&result.evidence_json)
    );
    write_text(
        &job.output_directory
            .join("quality-production-sample.properties"),
        &text,
    )
}

fn write_production_sample_evidence_json(
    job: &ProductionSampleJob,
    options: &ProductionSampleOptions,
    surface_report: &SurfaceRegionReport,
    render_report: &TopdownReport,
    result: &ProductionSampleJobResult,
    surface_material_path: Option<&Path>,
    surface_tile_cache_entries: usize,
) -> std::result::Result<(), String> {
    let document = json!({
        "schemaVersion": 1,
        "sample": job.sample,
        "inputs": {
            "sourcePng": normalized_path_display(&job.source),
            "expectedStandardPng": normalized_path_display(&job.expected_standard),
            "landMaskPng": production_path_text(job.land_mask.as_deref()),
            "maskMode": job.mask_mode,
        },
        "generation": {
            "regionX": job.region_x,
            "regionZ": job.region_z,
            "chunkStatus": options.chunk_status.id(),
            "textureMode": options.texture_mode.id(),
            "surfaceRaster": surface_material_path
                .map(normalized_path_display)
                .unwrap_or_else(|| "none".to_string()),
            "cacheRows": options.cache_rows,
            "prefetchRows": options.prefetch_rows,
            "verticalScale": options.vertical_scale,
            "surfaceTileCacheEntries": surface_tile_cache_entries,
        },
        "artifacts": {
            "worldDir": normalized_path_display(&job.output_directory.join("world")),
            "regionFile": normalized_path_display(&surface_report.region_file),
            "currentRender": normalized_path_display(&result.current_render),
            "metricDirectory": normalized_path_display(&result.metric_directory),
            "previewDebugDirectory": production_path_text(result.preview_debug_directory.as_deref()),
            "productionSourceDebug": production_path_text(result.production_source_debug.as_deref()),
        },
        "timingsMillis": {
            "elapsed": result.elapsed_millis,
            "generation": result.generation_millis,
            "render": result.render_millis,
            "metric": result.metric_millis,
        },
        "metrics": {
            "currentVsExpectedMean": result.current_vs_expected_mean,
            "currentVsSourceMean": result.current_vs_source_mean,
            "productionSourceVsReferenceMean": result.production_source_vs_reference_mean,
            "productionSourceVsExpectedMean": result.production_source_vs_expected_mean,
        },
        "topdown": {
            "missingRegions": render_report.stats.missing_regions,
            "missingChunks": render_report.stats.missing_chunks,
            "columnCount": render_report.stats.column_count,
            "waterTopColumns": render_report.stats.water_top_columns,
            "leafTopColumns": render_report.stats.leaf_top_columns,
        }
    });
    write_json(&result.evidence_json, &document)
}

fn write_production_sample_contact_sheet(
    results: &[ProductionSampleJobResult],
    output_path: &Path,
) -> std::result::Result<std::path::PathBuf, String> {
    const COLUMNS: usize = 4;
    let mut rows = Vec::<[image::RgbImage; COLUMNS]>::new();
    let mut cell_width = 1u32;
    let mut cell_height = 1u32;
    for result in results {
        let paths = [
            result.metric_directory.join("source-crop.png"),
            result.metric_directory.join("expected-crop.png"),
            result.metric_directory.join("current-surface-crop.png"),
            result
                .metric_directory
                .join("current-vs-expected-error.png"),
        ];
        let images = paths
            .map(|path| {
                image::open(&path)
                    .map_err(|error| {
                        format!(
                            "failed to read contact sheet image {}: {error}",
                            path.display()
                        )
                    })
                    .map(|image| image.to_rgb8())
            })
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for image in &images {
            cell_width = cell_width.max(image.width());
            cell_height = cell_height.max(image.height());
        }
        let [source, expected, current, error] = images
            .try_into()
            .map_err(|_| "internal contact sheet image count mismatch".to_string())?;
        rows.push([source, expected, current, error]);
    }
    let width = cell_width
        .checked_mul(COLUMNS as u32)
        .ok_or_else(|| "contact sheet width overflow".to_string())?;
    let height = cell_height
        .checked_mul(rows.len() as u32)
        .ok_or_else(|| "contact sheet height overflow".to_string())?;
    let mut sheet = image::RgbImage::from_pixel(width, height, image::Rgb([24, 24, 24]));
    for (row_index, row) in rows.iter().enumerate() {
        let y_offset = row_index as u32 * cell_height;
        for (column_index, image) in row.iter().enumerate() {
            let x_offset = column_index as u32 * cell_width;
            image::imageops::replace(&mut sheet, image, i64::from(x_offset), i64::from(y_offset));
        }
    }
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    sheet.save(output_path).map_err(|error| error.to_string())?;
    Ok(output_path.to_path_buf())
}

fn photo_standard_remap_parity_crop(
    out: &mut impl Write,
    err: &mut impl Write,
    source_path: &str,
    image_magick_remap_path: &str,
    output_directory: &str,
    crop_x: &str,
    crop_y: &str,
    crop_width: &str,
    crop_height: &str,
    mask_path: Option<&str>,
    mask_mode: &str,
) -> io::Result<i32> {
    match parse_photo_crop(crop_x, crop_y, crop_width, crop_height).and_then(
        |(crop_x, crop_y, crop_width, crop_height)| {
            let mask_path = parse_optional_cli_path(mask_path);
            quality::write_standard_remap_parity_report(
                Path::new(source_path),
                Path::new(image_magick_remap_path),
                Path::new(output_directory),
                crop_x,
                crop_y,
                crop_width,
                crop_height,
                mask_path.as_deref(),
                mask_mode,
            )
        },
    ) {
        Ok(report) => {
            writeln!(out, "Photo Standard remap parity crop written")?;
            writeln!(
                out,
                "outputDirectory={}",
                cli_path_display(&report.output_directory)
            )?;
            write!(out, "{}", report.text)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Photo Standard remap parity crop failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn photo_standard_remap_parity_batch(
    out: &mut impl Write,
    err: &mut impl Write,
    jobs_csv: &str,
    output_root: &str,
    threads: &str,
) -> io::Result<i32> {
    match quality::run_standard_remap_batch(Path::new(jobs_csv), Path::new(output_root), threads) {
        Ok(report) => {
            writeln!(out, "Photo Standard remap parity batch written")?;
            writeln!(out, "jobsCsv={}", cli_path_display(&report.jobs_csv))?;
            writeln!(out, "outputRoot={}", cli_path_display(&report.output_root))?;
            writeln!(out, "jobs={}", report.jobs)?;
            writeln!(out, "threads={}", report.threads)?;
            writeln!(out, "elapsedMillis={}", report.elapsed_millis)?;
            writeln!(
                out,
                "sample,elapsedMillis,pixels,meanDeltaE2000,p95DeltaE2000,exactMatchPercent,outputDirectory"
            )?;
            for result in report.results {
                writeln!(
                    out,
                    "{},{},{},{},{},{},{}",
                    result.sample,
                    result.elapsed_millis,
                    result.pixels,
                    metric_text(result.mean_delta_e2000),
                    metric_text(result.p95_delta_e2000),
                    metric_text(result.exact_match_percent),
                    cli_path_display(&result.output_directory)
                )?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Photo Standard remap parity batch failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn photo_production_candidate_diff_crop(
    out: &mut impl Write,
    err: &mut impl Write,
    source_path: &str,
    expected_path: &str,
    current_surface_path: &str,
    candidate_surface_path: &str,
    output_directory: &str,
    crop_x: &str,
    crop_y: &str,
    crop_width: &str,
    crop_height: &str,
    mask_path: Option<&str>,
    mask_mode: &str,
) -> io::Result<i32> {
    match parse_photo_crop(crop_x, crop_y, crop_width, crop_height).and_then(
        |(crop_x, crop_y, crop_width, crop_height)| {
            let mask_path = parse_optional_cli_path(mask_path);
            quality::write_production_candidate_diff_report(
                Path::new(source_path),
                Path::new(expected_path),
                Path::new(current_surface_path),
                Path::new(candidate_surface_path),
                Path::new(output_directory),
                crop_x,
                crop_y,
                crop_width,
                crop_height,
                mask_path.as_deref(),
                mask_mode,
            )
        },
    ) {
        Ok(report) => {
            writeln!(out, "Photo production-candidate diff crop written")?;
            writeln!(
                out,
                "outputDirectory={}",
                cli_path_display(&report.output_directory)
            )?;
            write!(out, "{}", report.text)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Photo production-candidate diff crop failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn photo_carrier_remap_sim_crop(
    out: &mut impl Write,
    err: &mut impl Write,
    current_surface_path: &str,
    expected_path: &str,
    output_directory: &str,
    crop_x: &str,
    crop_y: &str,
    crop_width: &str,
    crop_height: &str,
    mask_path: &str,
    mask_mode: &str,
    carrier_filter: &str,
    top_buckets: &str,
) -> io::Result<i32> {
    match parse_photo_crop(crop_x, crop_y, crop_width, crop_height).and_then(
        |(crop_x, crop_y, crop_width, crop_height)| {
            let mask_path = parse_required_optional_cli_path(mask_path);
            let top_buckets = top_buckets
                .parse::<usize>()
                .map_err(|error| error.to_string())?;
            quality::write_carrier_remap_simulation_report(
                Path::new(current_surface_path),
                Path::new(expected_path),
                Path::new(output_directory),
                crop_x,
                crop_y,
                crop_width,
                crop_height,
                mask_path.as_deref(),
                mask_mode,
                carrier_filter,
                top_buckets,
            )
        },
    ) {
        Ok(report) => {
            writeln!(out, "Photo carrier remap simulation crop written")?;
            writeln!(
                out,
                "outputDirectory={}",
                cli_path_display(&report.output_directory)
            )?;
            write!(out, "{}", report.text)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Photo carrier remap simulation crop failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn parse_photo_crop(
    crop_x: &str,
    crop_y: &str,
    crop_width: &str,
    crop_height: &str,
) -> Result<(u32, u32, u32, u32), String> {
    let crop_x = crop_x.parse::<u32>().map_err(|error| error.to_string())?;
    let crop_y = crop_y.parse::<u32>().map_err(|error| error.to_string())?;
    let crop_width = crop_width
        .parse::<u32>()
        .map_err(|error| error.to_string())?;
    let crop_height = crop_height
        .parse::<u32>()
        .map_err(|error| error.to_string())?;
    if crop_width == 0 || crop_height == 0 {
        return Err("crop width and height must be positive".to_string());
    }
    Ok((crop_x, crop_y, crop_width, crop_height))
}

fn parse_required_optional_cli_path(path: &str) -> Option<std::path::PathBuf> {
    parse_optional_cli_path(Some(path))
}

fn parse_optional_cli_path(path: Option<&str>) -> Option<std::path::PathBuf> {
    let path = path?.trim();
    if path.is_empty() || path.eq_ignore_ascii_case("none") || path.eq_ignore_ascii_case("null") {
        None
    } else {
        Some(std::path::PathBuf::from(path))
    }
}

fn cli_path_display(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn metric_text(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.6}")
    } else {
        "NaN".to_string()
    }
}

struct VanillaDelegatedArgs<'a> {
    heightmap_path: String,
    world_dir: &'a str,
    scale: &'a str,
    region_x: &'a str,
    region_z: &'a str,
    format: &'a str,
    status: &'a str,
    surface_raster: &'a str,
    extra_options: &'a [String],
}

struct VanillaDelegatedRegionsParallelArgs<'a> {
    heightmap_path: &'a str,
    world_dir: &'a str,
    scale: &'a str,
    start_region_x: &'a str,
    start_region_z: &'a str,
    cols: &'a str,
    rows: &'a str,
    format: &'a str,
    threads: &'a str,
    status: &'a str,
    surface_raster: &'a str,
    extra_options: &'a [String],
}

struct QualityCandidateArgs<'a> {
    heightmap_path: String,
    world_dir: &'a str,
    scale: &'a str,
    region_x: &'a str,
    region_z: &'a str,
    format: &'a str,
    surface_raster: &'a str,
    sample_grid: &'a str,
}

fn quality_candidate_args(
    args: &[String],
) -> std::result::Result<Option<QualityCandidateArgs<'_>>, String> {
    quality_candidate_args_with_default(args, configured_default_heightmap_path())
}

fn quality_candidate_args_with_default(
    args: &[String],
    default_heightmap: Option<String>,
) -> std::result::Result<Option<QualityCandidateArgs<'_>>, String> {
    if args.get(5).is_some_and(|arg| is_output_format_text(arg)) {
        let (surface_raster, sample_grid) = optional_quality_candidate_args(args, 6);
        return Ok(Some(QualityCandidateArgs {
            heightmap_path: default_heightmap
                .ok_or_else(|| missing_default_heightmap_error("quality-candidate"))?,
            world_dir: args[1].as_str(),
            scale: args[2].as_str(),
            region_x: args[3].as_str(),
            region_z: args[4].as_str(),
            format: args[5].as_str(),
            surface_raster,
            sample_grid,
        }));
    }
    if !args.get(6).is_some_and(|arg| is_output_format_text(arg)) {
        return Ok(None);
    }
    let (surface_raster, sample_grid) = optional_quality_candidate_args(args, 7);
    Ok(Some(QualityCandidateArgs {
        heightmap_path: args[1].to_string(),
        world_dir: args[2].as_str(),
        scale: args[3].as_str(),
        region_x: args[4].as_str(),
        region_z: args[5].as_str(),
        format: args[6].as_str(),
        surface_raster,
        sample_grid,
    }))
}

fn optional_quality_candidate_args(args: &[String], first_optional: usize) -> (&str, &str) {
    (
        args.get(first_optional)
            .map(String::as_str)
            .unwrap_or("surfaceRaster=auto"),
        args.get(first_optional + 1)
            .map(String::as_str)
            .unwrap_or("sampleGrid=64"),
    )
}

fn vanilla_delegated_args(
    args: &[String],
) -> std::result::Result<Option<VanillaDelegatedArgs<'_>>, String> {
    vanilla_delegated_args_with_default(args, configured_default_heightmap_path())
}

fn vanilla_delegated_args_with_default(
    args: &[String],
    default_heightmap: Option<String>,
) -> std::result::Result<Option<VanillaDelegatedArgs<'_>>, String> {
    let uses_default_heightmap = args.get(5).is_some_and(|arg| is_output_format_text(arg));
    if uses_default_heightmap {
        let optional = optional_generation_args(args, 6);
        return Ok(Some(VanillaDelegatedArgs {
            heightmap_path: default_heightmap.ok_or_else(|| {
                missing_default_heightmap_error("generate-vanilla-delegated-region")
            })?,
            world_dir: args[1].as_str(),
            scale: args[2].as_str(),
            region_x: args[3].as_str(),
            region_z: args[4].as_str(),
            format: args[5].as_str(),
            status: optional.status,
            surface_raster: optional.surface_raster,
            extra_options: optional.extra_options,
        }));
    }
    if !args.get(6).is_some_and(|arg| is_output_format_text(arg)) {
        return Ok(None);
    }
    let optional = optional_generation_args(args, 7);
    Ok(Some(VanillaDelegatedArgs {
        heightmap_path: args[1].to_string(),
        world_dir: args[2].as_str(),
        scale: args[3].as_str(),
        region_x: args[4].as_str(),
        region_z: args[5].as_str(),
        format: args[6].as_str(),
        status: optional.status,
        surface_raster: optional.surface_raster,
        extra_options: optional.extra_options,
    }))
}

fn vanilla_delegated_regions_parallel_args(
    args: &[String],
) -> Option<VanillaDelegatedRegionsParallelArgs<'_>> {
    if !args.get(8).is_some_and(|arg| is_output_format_text(arg)) {
        return None;
    }
    let optional = optional_generation_args(args, 10);
    Some(VanillaDelegatedRegionsParallelArgs {
        heightmap_path: args[1].as_str(),
        world_dir: args[2].as_str(),
        scale: args[3].as_str(),
        start_region_x: args[4].as_str(),
        start_region_z: args[5].as_str(),
        cols: args[6].as_str(),
        rows: args[7].as_str(),
        format: args[8].as_str(),
        threads: args[9].as_str(),
        status: optional.status,
        surface_raster: optional.surface_raster,
        extra_options: optional.extra_options,
    })
}

struct GenerationOptionalArgs<'a> {
    status: &'a str,
    surface_raster: &'a str,
    extra_options: &'a [String],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RegionCompressionOptions {
    mca_compression_level: Option<u32>,
    linear_compression_level: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum VerticalScaleOption {
    Auto,
    Legacy,
    Explicit(f64),
}

impl Default for VerticalScaleOption {
    fn default() -> Self {
        Self::Auto
    }
}

impl VerticalScaleOption {
    fn effective(self, scale_denominator: i32) -> f64 {
        match self {
            Self::Auto => auto_vertical_scale_for_denominator(scale_denominator),
            Self::Legacy => DEFAULT_VERTICAL_SCALE,
            Self::Explicit(value) => value,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Legacy => "legacy",
            Self::Explicit(_) => "explicit",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct GenerationRuntimeOptions {
    compression: RegionCompressionOptions,
    vertical_scale: VerticalScaleOption,
    prefetch: GenerationPrefetchOptions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GenerationPrefetchOptions {
    enabled: bool,
    memory_cap_bytes: Option<u64>,
    queue_regions: Option<usize>,
    workers: usize,
}

impl Default for GenerationPrefetchOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            memory_cap_bytes: None,
            queue_regions: None,
            workers: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EffectivePrefetchConfig {
    enabled: bool,
    memory_cap_bytes: Option<u64>,
    queue_regions: usize,
    workers: usize,
    estimated_region_bytes: u64,
}

impl EffectivePrefetchConfig {
    fn disabled() -> Self {
        Self {
            enabled: false,
            memory_cap_bytes: None,
            queue_regions: 0,
            workers: 0,
            estimated_region_bytes: PREPARED_SURFACE_REGION_ESTIMATED_BYTES,
        }
    }
}

fn optional_generation_args(args: &[String], first_optional: usize) -> GenerationOptionalArgs<'_> {
    let rest = args.get(first_optional..).unwrap_or(&[]);
    optional_generation_args_from_rest(rest)
}

fn optional_generation_args_from_rest(rest: &[String]) -> GenerationOptionalArgs<'_> {
    if rest.is_empty() {
        return GenerationOptionalArgs {
            status: "surface",
            surface_raster: "surfaceRaster=auto",
            extra_options: &[],
        };
    }
    if is_surface_raster_option(&rest[0]) {
        return GenerationOptionalArgs {
            status: "surface",
            surface_raster: rest[0].as_str(),
            extra_options: &rest[1..],
        };
    }
    if rest[0].contains('=') {
        return GenerationOptionalArgs {
            status: "surface",
            surface_raster: "surfaceRaster=auto",
            extra_options: rest,
        };
    }
    let status = rest[0].as_str();
    if rest.len() >= 2 && (is_surface_raster_option(&rest[1]) || !rest[1].contains('=')) {
        GenerationOptionalArgs {
            status,
            surface_raster: rest[1].as_str(),
            extra_options: &rest[2..],
        }
    } else {
        GenerationOptionalArgs {
            status,
            surface_raster: "surfaceRaster=auto",
            extra_options: &rest[1..],
        }
    }
}

fn is_surface_raster_option(value: &str) -> bool {
    value
        .split_once('=')
        .is_some_and(|(key, _)| key.eq_ignore_ascii_case("surfaceRaster"))
}

#[cfg(test)]
fn parse_region_compression_options(
    format: OutputFormat,
    options: &[String],
) -> std::result::Result<RegionCompressionOptions, String> {
    Ok(parse_generation_runtime_options(format, options)?.compression)
}

fn parse_generation_runtime_options(
    format: OutputFormat,
    options: &[String],
) -> std::result::Result<GenerationRuntimeOptions, String> {
    let mut parsed = GenerationRuntimeOptions::default();
    for option in options {
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| format!("optional generation argument must be key=value: {option}"))?;
        if key.eq_ignore_ascii_case("compression") || key.eq_ignore_ascii_case("compressionLevel") {
            match format {
                OutputFormat::Mca => {
                    parsed.compression.mca_compression_level =
                        Some(parse_mca_compression_level(value)?);
                }
                OutputFormat::LinearV2 => {
                    parsed.compression.linear_compression_level =
                        Some(parse_linear_compression_level(value)?);
                }
            }
            continue;
        }
        if key.eq_ignore_ascii_case("mcaCompression")
            || key.eq_ignore_ascii_case("mcaCompressionLevel")
        {
            parsed.compression.mca_compression_level = Some(parse_mca_compression_level(value)?);
            continue;
        }
        if key.eq_ignore_ascii_case("linearCompression")
            || key.eq_ignore_ascii_case("linearCompressionLevel")
        {
            parsed.compression.linear_compression_level =
                Some(parse_linear_compression_level(value)?);
            continue;
        }
        if key.eq_ignore_ascii_case("verticalScale")
            || key.eq_ignore_ascii_case("heightScale")
            || key.eq_ignore_ascii_case("yScale")
            || key.eq_ignore_ascii_case("reliefScale")
            || key.eq_ignore_ascii_case("verticalProfile")
        {
            parsed.vertical_scale = parse_generation_vertical_scale_option(value)?;
            continue;
        }
        if key.eq_ignore_ascii_case("prefetch")
            || key.eq_ignore_ascii_case("surfacePrefetch")
            || key.eq_ignore_ascii_case("evidencePrefetch")
        {
            parsed.prefetch.enabled = parse_bool_option("prefetch", value)?;
            continue;
        }
        if key.eq_ignore_ascii_case("prefetchMemoryGB")
            || key.eq_ignore_ascii_case("prefetchMemoryGiB")
            || key.eq_ignore_ascii_case("evidencePrefetchMemoryGB")
            || key.eq_ignore_ascii_case("evidencePrefetchMemoryGiB")
        {
            parsed.prefetch.enabled = true;
            parsed.prefetch.memory_cap_bytes = Some(parse_memory_gib_option(key, value)?);
            continue;
        }
        if key.eq_ignore_ascii_case("prefetchMemory")
            || key.eq_ignore_ascii_case("prefetchMemoryBytes")
            || key.eq_ignore_ascii_case("evidencePrefetchMemory")
            || key.eq_ignore_ascii_case("evidencePrefetchMemoryBytes")
        {
            parsed.prefetch.enabled = true;
            parsed.prefetch.memory_cap_bytes = Some(parse_memory_bytes_option(key, value)?);
            continue;
        }
        if key.eq_ignore_ascii_case("prefetchRegions")
            || key.eq_ignore_ascii_case("prefetchQueueRegions")
            || key.eq_ignore_ascii_case("evidencePrefetchRegions")
        {
            parsed.prefetch.enabled = true;
            parsed.prefetch.queue_regions =
                Some(parse_positive_usize_string("prefetchRegions", value)?);
            continue;
        }
        if key.eq_ignore_ascii_case("prefetchWorkers")
            || key.eq_ignore_ascii_case("prefetchThreads")
            || key.eq_ignore_ascii_case("evidencePrefetchWorkers")
        {
            parsed.prefetch.enabled = true;
            parsed.prefetch.workers = parse_positive_usize_string("prefetchWorkers", value)?;
            continue;
        }
        return Err(format!("unknown generation option: {key}"));
    }
    Ok(parsed)
}

fn parse_bool_option(name: &str, value: &str) -> std::result::Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" | "y" | "enabled" => Ok(true),
        "0" | "false" | "off" | "no" | "n" | "disabled" => Ok(false),
        _ => Err(format!("{name} must be true/false, on/off, or 1/0")),
    }
}

fn parse_memory_gib_option(name: &str, value: &str) -> std::result::Result<u64, String> {
    parse_decimal_memory_bytes(name, value, BYTES_PER_GIB)
}

fn parse_memory_bytes_option(name: &str, value: &str) -> std::result::Result<u64, String> {
    let normalized = value.trim().replace('_', "").to_ascii_lowercase();
    for (suffix, multiplier) in [
        ("gib", BYTES_PER_GIB),
        ("gb", BYTES_PER_GIB),
        ("mib", BYTES_PER_MIB),
        ("mb", BYTES_PER_MIB),
        ("kib", 1024),
        ("kb", 1024),
        ("b", 1),
    ] {
        if let Some(number) = normalized.strip_suffix(suffix) {
            return parse_decimal_memory_bytes(name, number, multiplier);
        }
    }
    parse_decimal_memory_bytes(name, &normalized, 1)
}

fn parse_decimal_memory_bytes(
    name: &str,
    value: &str,
    multiplier: u64,
) -> std::result::Result<u64, String> {
    let parsed = value
        .trim()
        .parse::<f64>()
        .map_err(|error| format!("{name} must be a memory size: {error}"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(format!("{name} must be a positive memory size"));
    }
    let bytes = parsed * multiplier as f64;
    if bytes > u64::MAX as f64 {
        return Err(format!("{name} is too large"));
    }
    Ok(bytes.round().max(1.0) as u64)
}

fn parse_generation_vertical_scale_option(
    value: &str,
) -> std::result::Result<VerticalScaleOption, String> {
    let value = value.trim();
    if matches_ignore_ascii_case(value, &["auto", "realistic", "scale-aware", "scaleAware"]) {
        return Ok(VerticalScaleOption::Auto);
    }
    if matches_ignore_ascii_case(value, &["legacy", "fixed", "default"]) {
        return Ok(VerticalScaleOption::Legacy);
    }
    let parsed = value.parse::<f64>().map_err(|error| error.to_string())?;
    earthmap_surface::require_valid_vertical_scale(parsed)
        .map_err(|error| error.to_string())
        .map(VerticalScaleOption::Explicit)
}

fn parse_mca_compression_level(value: &str) -> std::result::Result<u32, String> {
    let level = value.parse::<u32>().map_err(|error| error.to_string())?;
    if level > 9 {
        return Err(format!(
            "mcaCompression must be in the zlib range 0..9: {level}"
        ));
    }
    Ok(level)
}

fn parse_linear_compression_level(value: &str) -> std::result::Result<i32, String> {
    let level = parse_i32_string(value)?;
    if !(1..=22).contains(&level) {
        return Err(format!(
            "linearCompression must be in the DivineMC-safe zstd range 1..22: {level}"
        ));
    }
    Ok(level)
}

fn apply_region_compression_options(
    settings: &mut SurfaceRegionSettings,
    options: RegionCompressionOptions,
) {
    settings.mca_compression_level = options.mca_compression_level;
    settings.linear_compression_level = options.linear_compression_level;
}

#[allow(clippy::too_many_arguments)]
fn vanilla_delegated_surface_region_settings(
    heightmap_path: &str,
    world_dir: &str,
    level_name: &str,
    scale: i32,
    region_x: i32,
    region_z: i32,
    format: OutputFormat,
    cache_rows: usize,
    status: ChunkGenerationStatus,
    vertical_scale: f64,
    surface_material_path: &Path,
    surface_tile_cache_entries: usize,
    parallel_column_sampling: bool,
    compression: RegionCompressionOptions,
) -> std::result::Result<SurfaceRegionSettings, String> {
    let mut settings = SurfaceRegionSettings::new_with_texture_options(
        heightmap_path,
        world_dir,
        level_name,
        0,
        scale,
        region_x,
        region_z,
        format,
        cache_rows,
        false,
        status,
        vertical_scale,
        SurfaceTextureMode::Photo,
    )
    .map_err(|error| error.to_string())?;
    settings.surface_material_path = Some(surface_material_path.to_path_buf());
    settings.surface_tile_cache_entries = surface_tile_cache_entries;
    settings.parallel_column_sampling = parallel_column_sampling;
    apply_region_compression_options(&mut settings, compression);
    Ok(settings)
}

fn compression_options_report_line(
    format: OutputFormat,
    options: RegionCompressionOptions,
) -> String {
    match format {
        OutputFormat::Mca => options
            .mca_compression_level
            .map(|level| format!("mcaCompression={level}"))
            .unwrap_or_else(|| "mcaCompression=default".to_string()),
        OutputFormat::LinearV2 => options
            .linear_compression_level
            .map(|level| format!("linearCompression={level}"))
            .unwrap_or_else(|| "linearCompression=default".to_string()),
    }
}

struct TraceColumnArgs<'a> {
    heightmap_path: String,
    scale: &'a str,
    region_x: &'a str,
    region_z: &'a str,
    local_x: &'a str,
    local_z: &'a str,
    surface_raster: Option<&'a str>,
}

fn trace_column_args(args: &[String]) -> std::result::Result<TraceColumnArgs<'_>, String> {
    trace_column_args_with_default(args, configured_default_heightmap_path())
}

fn trace_column_args_with_default(
    args: &[String],
    default_heightmap: Option<String>,
) -> std::result::Result<TraceColumnArgs<'_>, String> {
    let uses_default_heightmap = args.len() == 6
        || (args.len() == 7
            && args.get(6).is_some_and(|arg| arg.contains('='))
            && args.get(1).is_some_and(|arg| parse_i32_string(arg).is_ok()));
    if uses_default_heightmap {
        return Ok(TraceColumnArgs {
            heightmap_path: default_heightmap
                .ok_or_else(|| missing_default_heightmap_error("trace-surface-region-column"))?,
            scale: args[1].as_str(),
            region_x: args[2].as_str(),
            region_z: args[3].as_str(),
            local_x: args[4].as_str(),
            local_z: args[5].as_str(),
            surface_raster: args.get(6).map(String::as_str),
        });
    }
    Ok(TraceColumnArgs {
        heightmap_path: args[1].to_string(),
        scale: args[2].as_str(),
        region_x: args[3].as_str(),
        region_z: args[4].as_str(),
        local_x: args[5].as_str(),
        local_z: args[6].as_str(),
        surface_raster: args.get(7).map(String::as_str),
    })
}

struct TraceCellArgs<'a> {
    heightmap_path: String,
    scale: &'a str,
    region_x: &'a str,
    region_z: &'a str,
    chunk_local_x: &'a str,
    chunk_local_z: &'a str,
    cell_x: &'a str,
    cell_z: &'a str,
    surface_raster: Option<&'a str>,
}

fn trace_cell_args(args: &[String]) -> std::result::Result<TraceCellArgs<'_>, String> {
    trace_cell_args_with_default(args, configured_default_heightmap_path())
}

fn trace_cell_args_with_default(
    args: &[String],
    default_heightmap: Option<String>,
) -> std::result::Result<TraceCellArgs<'_>, String> {
    let uses_default_heightmap = args.len() == 8
        || (args.len() == 9
            && args.get(8).is_some_and(|arg| arg.contains('='))
            && args.get(1).is_some_and(|arg| parse_i32_string(arg).is_ok()));
    if uses_default_heightmap {
        return Ok(TraceCellArgs {
            heightmap_path: default_heightmap
                .ok_or_else(|| missing_default_heightmap_error("trace-surface-region-cell"))?,
            scale: args[1].as_str(),
            region_x: args[2].as_str(),
            region_z: args[3].as_str(),
            chunk_local_x: args[4].as_str(),
            chunk_local_z: args[5].as_str(),
            cell_x: args[6].as_str(),
            cell_z: args[7].as_str(),
            surface_raster: args.get(8).map(String::as_str),
        });
    }
    Ok(TraceCellArgs {
        heightmap_path: args[1].to_string(),
        scale: args[2].as_str(),
        region_x: args[3].as_str(),
        region_z: args[4].as_str(),
        chunk_local_x: args[5].as_str(),
        chunk_local_z: args[6].as_str(),
        cell_x: args[7].as_str(),
        cell_z: args[8].as_str(),
        surface_raster: args.get(9).map(String::as_str),
    })
}

fn print_help(out: &mut impl Write) -> io::Result<i32> {
    writeln!(out, "{}", build_info::NAME)?;
    writeln!(out)?;
    writeln!(
        out,
        "Status: Rust runtime active; normal generation and validation paths are Rust-first."
    )?;
    writeln!(out)?;
    writeln!(out, "Commands:")?;
    writeln!(out, "  --help          Show this help.")?;
    writeln!(out, "  --version       Show version and targets.")?;
    writeln!(
        out,
        "  doctor          Check local Rust runtime assumptions."
    )?;
    writeln!(
        out,
        "  capabilities    Show Rust runtime capability status."
    )?;
    writeln!(out, "  write-sha256-manifest <root> <outputFile>")?;
    writeln!(
        out,
        "  write-region-payload-manifest <regionFile> <outputCsv>"
    )?;
    writeln!(
        out,
        "  append-region-payload-manifest <regionFile> <outputCsv>"
    )?;
    writeln!(
        out,
        "  compare-region-payload-manifest <manifestCsv> <regionFile>"
    )?;
    writeln!(out, "  validate-mca-region <path>")?;
    writeln!(out, "  validate-linear-region <path>")?;
    writeln!(
        out,
        "  compare-mca-linear-region-payloads <mcaRegion> <linearRegion>"
    )?;
    writeln!(
        out,
        "  mca-topdown-render <worldDir> <outputPng> <startRegionX> <startRegionZ> <cols> <rows> [visible|terrain]"
    )?;
    writeln!(
        out,
        "  linear-topdown-render <worldDir> <outputPng> <startRegionX> <startRegionZ> <cols> <rows> [visible|terrain]"
    )?;
    writeln!(
        out,
        "  dynmap-tile-mosaic <dynmapTileDir> <outputPng> [base|z|zz...]"
    )?;
    writeln!(
        out,
        "  convert-mca-region-to-linear <mcaRegion> <linearRegion>"
    )?;
    writeln!(
        out,
        "  convert-mca-world-to-linear <mcaWorldDir> <linearWorldDir>"
    )?;
    writeln!(out, "  inspect-mca-palettes <path>")?;
    writeln!(out, "  validate-mca-survival-palette <path>")?;
    writeln!(out, "  inspect-linear-palettes <path>")?;
    writeln!(out, "  validate-linear-survival-palette <path>")?;
    writeln!(out, "  inspect-mca-biomes <path>")?;
    writeln!(out, "  inspect-linear-biomes <path>")?;
    writeln!(out, "  inspect-mca-statuses <path>")?;
    writeln!(out, "  inspect-linear-statuses <path>")?;
    writeln!(out, "  inspect-mca-post-final-integrity <path>")?;
    writeln!(out, "  inspect-linear-post-final-integrity <path>")?;
    writeln!(
        out,
        "  rewrite-mca-status <mcaRegion|regionDir|worldDir> <full|surface|carvers>"
    )?;
    writeln!(
        out,
        "  repair-mca-post-final-water <mcaRegion|regionDir|worldDir>"
    )?;
    writeln!(
        out,
        "  repair-linear-sandlike-surfaces <linearRegion|regionDir|worldDir>"
    )?;
    writeln!(
        out,
        "  summarize-region-chunk <regionFile> <localChunkX> <localChunkZ>"
    )?;
    writeln!(
        out,
        "  compare-region-chunk-details <expectedRegionFile> <actualRegionFile> <expectedLocalChunkX> <expectedLocalChunkZ> [actualLocalChunkX actualLocalChunkZ]"
    )?;
    writeln!(
        out,
        "  optional [heightmap] args use {HEIGHTMAP_PATH_ENV}=<GeoTIFF> when omitted"
    )?;
    writeln!(
        out,
        "  surfaceRaster=auto checks {SURFACE_RASTER_ENV}, {TIF_ROOT_ENV}, {DATA_ROOT_ENV}, then paths near the heightmap"
    )?;
    writeln!(
        out,
        "  trace-surface-region-column [heightmap] <scale> <regionX> <regionZ> <localX> <localZ> [surfaceRaster=auto|path]"
    )?;
    writeln!(
        out,
        "  trace-surface-region-cell [heightmap] <scale> <regionX> <regionZ> <chunkLocalX> <chunkLocalZ> <cellX> <cellZ> [surfaceRaster=auto|path]"
    )?;
    writeln!(
        out,
        "  quality-candidate [heightmap] <worldDir> <scale> <regionX> <regionZ> <mca|linear> [surfaceRaster=auto|path|none] [sampleGrid=64]"
    )?;
    writeln!(
        out,
        "  generate <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear> <threads> [surface|carvers] [surfaceRaster=auto|path] [verticalScale=auto|legacy|N] [compression=N|linearCompression=N|mcaCompression=N] [prefetchMemoryGB=N]"
    )?;
    writeln!(
        out,
        "  generate-survival-region <heightmap> <worldDir> <scale> <regionX> <regionZ> <mca|linear>    (Rust vanilla-delegated alias)"
    )?;
    writeln!(
        out,
        "  generate-survival-region-osm-pbf <pbf> <maxBlobs> <heightmap> <worldDir> <scale> <regionX> <regionZ> <mca|linear>"
    )?;
    writeln!(
        out,
        "  generate-survival-region-osm-pbf-ref-window <pbf> <nodeMaxBlobs> <waySkipBlobs> <wayMaxBlobs> <heightmap> <worldDir> <scale> <regionX> <regionZ> <mca|linear>"
    )?;
    writeln!(
        out,
        "  generate-survival-region-osm-pbf-full-scan <pbf> <maxBlobs> <heightmap> <worldDir> <scale> <regionX> <regionZ> <mca|linear> [progressEvery] [progressFile]"
    )?;
    writeln!(
        out,
        "  generate-survival-region-osm-xml-cache <osmDirectory> <heightmap> <worldDir> <scale> <regionX> <regionZ> <mca|linear>"
    )?;
    writeln!(
        out,
        "  generate-survival-regions-parallel <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear> <threads> [maxRegionsThisRun]    (Rust vanilla-delegated alias)"
    )?;
    writeln!(
        out,
        "  generate-survival-region-plan-parallel <heightmap> <worldDir> <scale> <mca|linear> <threads> <planCsv> [maxRegionsThisRun]    (Rust vanilla-delegated alias)"
    )?;
    writeln!(
        out,
        "  generate-vanilla-delegated-region [heightmap] <worldDir> <scale> <regionX> <regionZ> <mca|linear> [surface|carvers] [surfaceRaster=auto|path] [verticalScale=auto|legacy|N] [compression=N|linearCompression=N|mcaCompression=N]"
    )?;
    writeln!(
        out,
        "  generate-vanilla-delegated-regions-parallel <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear> <threads> [surface|carvers] [surfaceRaster=auto|path] [verticalScale=auto|legacy|N] [compression=N|linearCompression=N|mcaCompression=N] [prefetchMemoryGB=N]"
    )?;
    writeln!(
        out,
        "  generate-vanilla-delegated-plan-parallel <heightmap> <worldDir> <scale> <mca|linear> <threads> <planCsv> [maxRegionsThisRun] [surface|carvers] [surfaceRaster=auto|path] [verticalScale=auto|legacy|N] [compression=N|linearCompression=N|mcaCompression=N]"
    )?;
    writeln!(
        out,
        "  generate-vanilla-delegated-region-plan-parallel <heightmap> <worldDir> <scale> <mca|linear> <threads> <planCsv> [maxRegionsThisRun] [surface|carvers] [surfaceRaster=auto|path] [verticalScale=auto|legacy|N] [compression=N|linearCompression=N|mcaCompression=N]"
    )?;
    writeln!(
        out,
        "  plan-representative-regions <heightmap> <scale> <outputCsv> <targetRegions>"
    )?;
    writeln!(out, "  describe-earth-grid <heightmap> <scale>")?;
    writeln!(
        out,
        "  validate-surface-spawn <heightmap> <scale> <regionX> <regionZ>"
    )?;
    writeln!(
        out,
        "  validate-height-seam <heightmap> <scale> <regionX> <regionZ> <east|south>"
    )?;
    writeln!(
        out,
        "  write-vanilla-finalization-commands <outputCommands> <startRegionX> <startRegionZ> <cols> <rows> [windowChunks] [waitMs]"
    )?;
    writeln!(out, "  validate-survival-manifest <path>")?;
    writeln!(
        out,
        "  apply-survival-evidence <sourceManifest> <outputManifest> <bootLog> <rebootLog> <spawnToEndLog> <claim>"
    )?;
    writeln!(
        out,
        "  validate-cave-density <seed> <minBlockX> <minBlockZ> <sizeBlocks>"
    )?;
    writeln!(
        out,
        "  validate-cave-connectivity <seed> <minBlockX> <minBlockZ> <sizeBlocks>"
    )?;
    writeln!(out, "  validate-ore-histogram-synthetic")?;
    writeln!(out, "  validate-underground-fluid-synthetic")?;
    writeln!(
        out,
        "  validate-global-resource-fairness <worldDir> <outputDir>"
    )?;
    writeln!(out, "  validate-loot-economy <worldDir> <outputDir>")?;
    writeln!(
        out,
        "  generate-nation-war-readiness-report <heightmap> <scale> <outputDir> <factionCount> <safeZoneRadiusBlocks> [globalResourceFairnessReport] [lootEconomyReport] [pluginStackReport] [chunkLoadStressReport]"
    )?;
    writeln!(out, "  scan-osm-pbf <path> <maxBlobs>")?;
    writeln!(out, "  scan-osm-pbf-range <path> <skipBlobs> <maxBlobs>")?;
    writeln!(out, "  validate-osm-pbf <path> <maxBlobs>")?;
    writeln!(
        out,
        "  benchmark-osm-index <scale> <regionX> <regionZ> <wayCount>"
    )?;
    writeln!(
        out,
        "  extract-osm-region-mask <path> <scale> <regionX> <regionZ> <maxBlobs>"
    )?;
    writeln!(
        out,
        "  extract-osm-region-mask-window <path> <scale> <regionX> <regionZ> <nodeMaxBlobs> <waySkipBlobs> <wayMaxBlobs>"
    )?;
    writeln!(
        out,
        "  extract-osm-region-mask-ref-window <path> <scale> <regionX> <regionZ> <nodeMaxBlobs> <waySkipBlobs> <wayMaxBlobs>"
    )?;
    writeln!(
        out,
        "  extract-osm-region-mask-full-scan <path> <scale> <regionX> <regionZ> [maxBlobs] [progressEvery] [progressFile]"
    )?;
    writeln!(
        out,
        "  extract-osm-xml-region-mask <osmDirectory> <scale> <regionX> <regionZ>"
    )?;
    writeln!(out, "  identify-osm-xml-cache <directory>")?;
    writeln!(out, "  benchmark-region-writers <outputDir> [iterations=3]")?;
    writeln!(out, "  playability-smoke <worldDir> <outputJson>")?;
    writeln!(out, "  generate-flat-test-world <worldDir> <mca|linear>")?;
    writeln!(out, "  generate-palette-stress-world <worldDir>")?;
    writeln!(out, "  write-nbt-parity-fixtures <outputDir>")?;
    writeln!(out, "  write-nbt-gzip-parity-fixtures <outputDir>")?;
    writeln!(out, "  write-region-writer-parity-fixtures <outputDir>")?;
    writeln!(out, "  inspect-heightmap [path]")?;
    writeln!(
        out,
        "  locate-heightmap-point [heightmap] <scale> <longitude> <latitude>"
    )?;
    writeln!(
        out,
        "  classify-surface-point [heightmap] <scale> <longitude> <latitude>"
    )?;
    writeln!(out, "  raster-smoke [heightmap] <scale> <outputJson>")?;
    writeln!(out, "  sample-vrt-rgb <terrainVrt> <longitude> <latitude>")?;
    writeln!(
        out,
        "  photo-parity-crop <sourcePng> <metTargetPng|none> <currentSurfacePng|none> <standardPalettePng> <outputDir> <x> <y> <width> <height> [imageMagickRemapPng|none] [maskPng|none] [all|nonzero|white|land-water-debug]"
    )?;
    writeln!(
        out,
        "  photo-compare-crop <actualPng> <expectedPng> <outputDir> <x> <y> <width> <height> [maskPng|none] [all|nonzero|white|land-water-debug]"
    )?;
    writeln!(
        out,
        "  photo-parity-metric-crop <sourcePng> <expectedPng> <currentSurfacePng> <outputDir> <x> <y> <width> <height> [maskPng|none] [all|nonzero|white|land-water-debug]"
    )?;
    writeln!(
        out,
        "  photo-parity-metric-batch <jobsCsv> <outputRoot> [threads|auto]"
    )?;
    writeln!(
        out,
        "  quality-production-sample-batch <samplesCsv> <heightmap> <outputRoot> <scale> <mca> <threads> [cacheRows=512] [prefetchRows=0] [verticalScale=1.25] [textureMode=photo] [surfaceRaster=auto] [chunkStatus=surface] [metricMode=full|current-only] [previewDebug=off|auto|dir]"
    )?;
    writeln!(
        out,
        "  benchmark-height-regions <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear>"
    )?;
    writeln!(
        out,
        "  benchmark-surface-regions <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear>"
    )?;
    writeln!(
        out,
        "  benchmark-survival-regions <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear>"
    )?;
    writeln!(
        out,
        "  benchmark-survival-regions-parallel <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear> <threads>"
    )?;
    writeln!(
        out,
        "  photo-standard-remap-parity-crop <sourcePng> <imageMagickRemapPng> <outputDir> <x> <y> <width> <height> [maskPng|none] [all|nonzero|white|land-water-debug]"
    )?;
    writeln!(
        out,
        "  photo-standard-remap-parity-batch <jobsCsv> <outputRoot> [threads|auto]"
    )?;
    writeln!(
        out,
        "  photo-production-candidate-diff-crop <sourcePng> <expectedPng> <currentSurfacePng> <candidateSurfacePng> <outputDir> <x> <y> <width> <height> [maskPng|none] [all|nonzero|white|land-water-debug]"
    )?;
    writeln!(
        out,
        "  photo-carrier-remap-sim-crop <currentSurfacePng> <expectedPng> <outputDir> <x> <y> <width> <height> <maskPng|none> <all|nonzero|white|land-water-debug> <all|vegetated|sand|red-sand|coarse-dirt|rock|snow|wet|carrierCsv> [topBuckets]"
    )?;
    for name in commands::INITIAL_COMMANDS
        .iter()
        .map(|command| command.name)
    {
        writeln!(out, "  {name}")?;
    }
    writeln!(out)?;
    writeln!(
        out,
        "Legacy parity commands are diagnostic or focused low-level fixtures, not release gates."
    )?;
    writeln!(
        out,
        "Use quality-candidate for Rust-native visual/statistical/determinism evidence."
    )?;
    Ok(EXIT_OK)
}

fn print_version(out: &mut impl Write) -> io::Result<i32> {
    writeln!(out, "{} {}", build_info::NAME, build_info::VERSION)?;
    writeln!(
        out,
        "Minecraft target: Java Edition {}",
        build_info::MINECRAFT_TARGET
    )?;
    writeln!(out, "Gameplay profile: {}", build_info::GAMEPLAY_PROFILE)?;
    writeln!(out, "Server profile: {}", build_info::SERVER_PROFILE)?;
    writeln!(out, "Rust port phase: {}", build_info::RUST_PORT_PHASE)?;
    Ok(EXIT_OK)
}

fn print_doctor(out: &mut impl Write) -> io::Result<i32> {
    writeln!(out, "Rust port phase: {}", build_info::RUST_PORT_PHASE)?;
    writeln!(out, "OS: {}", std::env::consts::OS)?;
    writeln!(out, "Architecture: {}", std::env::consts::ARCH)?;
    writeln!(
        out,
        "Progress file: {}",
        progress::REGION_PROGRESS_FILE_NAME
    )?;
    write!(
        out,
        "Progress CSV header: {}",
        progress::REGION_PROGRESS_HEADER
    )?;
    writeln!(
        out,
        "Status: active prototype; production readiness {} until docs/QUALITY-GATES.md passes.",
        build_info::PRODUCTION_READINESS
    )?;
    Ok(EXIT_OK)
}

fn print_capabilities(out: &mut impl Write) -> io::Result<i32> {
    writeln!(
        out,
        "DONE rust.phase1.cliShell - Rust CLI shell recognizes the initial command set."
    )?;
    writeln!(
        out,
        "DONE rust.phase1.progressHeader - Progress CSV header is centralized for Java-compatible batch runs."
    )?;
    writeln!(
        out,
        "DONE rust.phase0.regionPayloadExtractor - MCA and Linear payload manifests can be written from Rust."
    )?;
    writeln!(
        out,
        "DONE rust.phase0.sha256ManifestWriter - SHA-256 file manifests can be written from Rust."
    )?;
    writeln!(
        out,
        "DONE rust.phase0.bootstrapGoldenCorpus - Synthetic Java-oracle bootstrap corpus can be generated by Rust scripts."
    )?;
    writeln!(
        out,
        "DONE rust.phase0.bootstrapPayloadManifestCheck - Bootstrap payload manifests can be checked against generated region files."
    )?;
    writeln!(
        out,
        "DONE rust.phase0.flatCandidateGenerator - generate-flat-test-world writes Rust candidate MCA/Linear flat worlds for Java-oracle corpus comparison."
    )?;
    writeln!(
        out,
        "DONE rust.phase0.paletteStressCandidateGenerator - generate-palette-stress-world writes the Rust candidate packed-palette stress world for Java-oracle corpus comparison."
    )?;
    writeln!(
        out,
        "TODO rust.phase0.fullGoldenCorpus - Height, surface, photo, water, survival, quality, and OSM corpus entries are not complete yet."
    )?;
    writeln!(
        out,
        "TODO rust.phase0.fullCorpusComparator - Metadata mismatch and compression-only delta reports are not complete yet."
    )?;
    writeln!(
        out,
        "DONE rust.phase2.minecraftCoreBootstrap - Block state ids, chunk status, dimension profile, packed long array, section palette, chunk model, heightmaps, NBT writer/reader, and chunk NBT encoder bootstrap are ported with Rust unit parity tests."
    )?;
    writeln!(
        out,
        "DONE rust.phase2.nbtByteFixtures - Java/Rust NBT and chunk payload fixtures are byte-identical for the bootstrap corpus."
    )?;
    writeln!(
        out,
        "DONE rust.phase2.levelDatTemplate - Level.dat root template is ported with fixed-LastPlayed Java/Rust NBT byte fixture coverage."
    )?;
    writeln!(
        out,
        "DONE rust.phase2.nbtGzipDelta - Fixed-LastPlayed level.dat gzip fixture proves decompressed NBT parity and records the Java/Rust deflate-stream delta."
    )?;
    writeln!(
        out,
        "DONE rust.phase3.regionWriterBootstrap - MCA and Linear V2 writer bootstraps round-trip through Rust payload readers."
    )?;
    writeln!(
        out,
        "DONE rust.phase3.phase2FixtureRegionByteParity - MCA and Linear writer fixtures are Java/Rust byte-identical over the Phase 2 chunk NBT fixture corpus."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.earthScaleMapping - EarthScaleMapping is ported with Java-compatible scaling, coordinate conversion, boundary, and NaN behavior."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.geoTiffMetadataBootstrap - GeoTiffMetadata sample type naming matches Java for Int16, Float32, and fallback layouts."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.geoTiffHeightmapReaderBootstrap - Synthetic BigTIFF Int16 heightmap metadata, pixel, sample, and row reads match the Java fixture path."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.geoTiffFloat32ReaderBootstrap - Synthetic BigTIFF Float32 metadata, nearest sampling, NoData handling, open-if-present behavior, and layout validation match the Java fixture path."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.geoTiffRgbReaderBootstrap - Synthetic Classic TIFF RGB metadata, interleaved pixel sampling, unavailable out-of-range pixels, and tile cache statistics match the Java fixture path."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.vrtRgbMosaicReaderBootstrap - Synthetic single-source and split-source VRT RGB mosaic sampling, indexed source lookup, and aggregated tile stats match the Java fixture path."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.vrtRgbDiagnosticCli - sample-vrt-rgb samples Java-compatible VRT RGB mosaics and reports color/source/cache stats for real-data smoke checks."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.realVrtRgbSmoke - configured TrueMarble.vrt sample-vrt-rgb stdout matches the Java VrtRgbMosaicReader oracle for representative coordinates."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.heightmapScalarSamplerUncached - HeightmapScalarSampler nearest and bilinear math matches Java for the uncached synthetic fixture path."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.rowCacheAndCachedSampler - GeoTiffRowCache sequential stats, prefetch counters, and cached sampler row reuse match Java synthetic fixtures."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.heightmapDiagnosticCli - inspect-heightmap and locate-heightmap-point are implemented for file-backed GeoTIFF smoke checks."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.realHeightmapSmoke - configured HeightMap GeoTIFF inspect-heightmap and locate-heightmap-point stdout match the Java oracle."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.heightOnlyRegionByteParity - Java/Rust height-only r.0.0 and r.-1.-1 MCA/Linear region bytes, payload manifests, normalized stdout, and exploration-only survival manifests match for the configured HeightMap at 1:5000."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.earthSurfaceRulesBootstrap - EarthSurfaceRules classify/classifyShaped/normalizeForChunk contracts and classify-surface-point diagnostic match Java fixture cases and configured HeightMap smoke points."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.surfaceMaterialSampleBootstrap - SurfaceMaterialSample coverage, slope, ecoregion, terrain-token normalization, rounding, and SurfaceDataEvidence flag contracts match Java fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.trueMarbleSurfaceSamplerBootstrap - TrueMarbleSurfaceMaterialSampler wraps VrtRgbMosaicReader.sample_averaged as Java-compatible color-only SurfaceMaterialSample output for synthetic VRT fixtures."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.geoTiffSingleBandReaderBootstrap - GeoTiffSingleBandReader opens uncompressed unsigned 8/16-bit single-band GeoTIFF rasters with Java-compatible nearest sampling and NoData handling for synthetic Classic TIFF fixtures."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.earthDataSamplerHelperBootstrap - EarthData sampler helper math for raster stat deltas, slope permille normalization, cell-degree clamps, longitude wrapping, and quantized cache keys matches Java fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.metTerrainVocabularyBootstrap - MetTerrainVocabulary exact and ImageMagick-style octree nearest remap contracts are ported for Java standard terrain-token synthesis fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.surfaceTerrainTokenSynthesisBootstrap - Surface terrain-token synthesis now prefers exported tokens and falls back to Java standard palette matches like EarthDataSurfaceMaterialSampler.withTerrainToken fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.surfaceBiomeCellWriterBootstrap - SurfaceBiomeCellWriter ports Java render-aware 4x4 biome cell selection, vertical surface-band coverage, and static carrier fallback fixture contracts."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.surfaceChunkBuildBootstrap - Surface chunk builder wires normalized columns through production natural-surface sanitization, dominant default-biome selection, and SurfaceBiomeCellWriter application like Java surfaceChunk fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.earthSurfaceChunkSamplerBootstrap - EarthSurfaceChunkSampler basic path samples heightmap-backed chunk columns with Java-compatible water decisions, smoothing, coast factors, shaped classification, and coastal cleanup fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.surfaceClassSmootherBootstrap - SurfaceClassSmoother non-photo isolated-column smoothing, protected water surfaces, and snowy-biome top compatibility match Java fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.surfaceClassPhotoTextureSmootherBootstrap - SurfaceClassSmoother photo texture local and macro vegetation smoothing match Java fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.photoSurfaceSolverContractBootstrap - PhotoSurfaceInput/Decision contract, water/no-photo preservation, and representative color-only nearest-palette photo solver cases are wired in Rust."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.photoSurfaceSolverAridTokenBootstrap - Java Standard arid terrain-token handling has bounded fixture coverage for dry false-snow avoidance, coastal tan carrier reduction, and ordered arid carrier variation."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.photoSurfaceSolverVegetationTokenBootstrap - Java Standard vegetation terrain-token handling has bounded fixture coverage for near-black shadow carriers, gray-olive static carrier selection, dry-open vegetation/crust, and dark canopy cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.surfaceRegionCommandBootstrap - generate-surface-region writes Java-shaped surface region files, level.dat, manifest metadata, and stdout reports for the no-surface-material default command path."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.surfaceBiomeFamilyIntentGridBootstrap - SurfaceBiomeFamilyIntentGrid non-photo cell stabilization, small-component absorption, protected wetland/snow/beach handling, and arid-transition preservation match Java fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.surfaceBiomeFamilyIntentGridPreserveSurfaceBootstrap - SurfaceBiomeFamilyIntentGrid preserve-surface biome-only stabilization keeps Java photo-palette render locks and top/filler preservation fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.osmSurfaceOverlayBootstrap - OSM region feature mask line rasterization and surface overlay road/building/waterway/landuse block placement match Java fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.earthDataSurfaceMaterialSamplerBootstrap - EarthDataSurfaceMaterialSampler opens TrueMarble plus optional climate, vegetation, ocean-temperature, bathymetry, and slope rasters, then samples Java-compatible quantized land/water SurfaceMaterialSample fixture cases with bounded cache reuse and terrain-token fallback."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.earthDataEcoregionEvidenceBootstrap - EcoregionSample normalization and EarthData ecoregion family-confidence aggregation match Java fixture cases with bounded access-order cache reuse."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.wwfEcoregionCacheBootstrap - WwfEcoregionSampler reads Java-generated ecoregion cache files and EarthDataSurfaceMaterialSampler auto-loads cache-backed ecoregion evidence when present."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.wwfEcoregionSourceBootstrap - WwfEcoregionSampler reads WWF shapefile/DBF source data, rebuilds Java-compatible ecoregion cache files, and falls back from stale or corrupt caches like Java."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.metImageExportTerrainSamplerBootstrap - MetImageExportTerrainSampler discovers MET image_exports tiles, parses aux GeoTransform metadata, samples PNG terrain-token colors with a bounded image cache, and EarthDataSurfaceMaterialSampler prefers exported tokens when present."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.landShallowTopoPhotoSamplerBootstrap - LandShallowTopoPhotoSampler discovers west/east topographic GeoTIFF halves and EarthDataSurfaceMaterialSampler.sample_photo follows the documented photo-source preference, coarse evidence, and terrain-token rules."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.naturalSurfacePolicyBootstrap - NaturalSurfaceBlockPolicy and CoastalSurfaceCleaner production-safe surface cleanup contracts are covered by Rust fixtures."
    )?;
    for spec in commands::INITIAL_COMMANDS {
        let status = match spec.status {
            CommandStatus::Implemented => "DONE",
            CommandStatus::ImplementedProbeOnly => "WIP",
            CommandStatus::ImplementedShellOnly => "DONE",
            CommandStatus::NotPortedYet => "TODO",
        };
        writeln!(out, "{status} rust.command.{} - {}", spec.name, spec.note)?;
    }
    Ok(EXIT_OK)
}

fn print_not_implemented(err: &mut impl Write, command: &str) -> io::Result<i32> {
    writeln!(
        err,
        "Rust command shell recognized '{command}', but this command is not implemented yet."
    )?;
    writeln!(
        err,
        "No legacy fallback is available in Rust-only mode; use an implemented earthmap-rs command or finish the Rust port for this command."
    )?;
    Ok(EXIT_USAGE)
}

fn inspect_heightmap(out: &mut impl Write, err: &mut impl Write, path: &str) -> io::Result<i32> {
    match inspect_heightmap_impl(path) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Heightmap inspection failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn inspect_heightmap_impl(path: &str) -> earthmap_geo::Result<Vec<String>> {
    let reader = GeoTiffHeightmapReader::open(Path::new(path))?;
    let metadata = reader.metadata();
    Ok(vec![
        "Heightmap GeoTIFF valid".to_string(),
        format!("path={}", metadata.path.display()),
        format!("width={}", metadata.width),
        format!("height={}", metadata.height),
        format!("sampleType={}", metadata.sample_type_name()),
        format!("compression={}", metadata.compression),
        format!("rowsPerStrip={}", metadata.rows_per_strip),
        format!("samplesPerPixel={}", metadata.samples_per_pixel),
        format!(
            "epsg={}",
            metadata
                .epsg_code
                .map_or_else(|| "NONE".to_string(), |epsg| epsg.to_string())
        ),
        format!(
            "topLeftLongitude={}",
            java_double_string(metadata.top_left_longitude)
        ),
        format!(
            "topLeftLatitude={}",
            java_double_string(metadata.top_left_latitude)
        ),
        format!(
            "pixelWidthDegrees={}",
            java_double_string(metadata.pixel_width_degrees)
        ),
        format!(
            "pixelHeightDegrees={}",
            java_double_string(metadata.pixel_height_degrees)
        ),
        format!(
            "noData={}",
            metadata
                .no_data_value
                .map_or_else(|| "NONE".to_string(), java_double_string)
        ),
    ])
}

fn locate_heightmap_point(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    scale_text: &str,
    longitude_text: &str,
    latitude_text: &str,
) -> io::Result<i32> {
    match locate_heightmap_point_impl(heightmap_path, scale_text, longitude_text, latitude_text) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Heightmap point location failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn locate_heightmap_point_impl(
    heightmap_path: &str,
    scale_text: &str,
    longitude_text: &str,
    latitude_text: &str,
) -> earthmap_geo::Result<Vec<String>> {
    let reader = GeoTiffHeightmapReader::open(Path::new(heightmap_path))?;
    let scale = parse_i32(scale_text)?;
    let longitude = parse_f64(longitude_text)?;
    let latitude = parse_f64(latitude_text)?;
    let mapping = mapping_for(reader.metadata(), scale)?;
    let map_x = mapping.block_x_for_longitude(longitude)?;
    let map_z = mapping.block_z_for_latitude(latitude)?;
    let global_block_x = map_x - (mapping.width_blocks / 2);
    let global_block_z = map_z - (mapping.height_blocks / 2);
    let region_x = global_block_x.div_euclid(512);
    let region_z = global_block_z.div_euclid(512);
    let local_block_x = global_block_x.rem_euclid(512);
    let local_block_z = global_block_z.rem_euclid(512);
    Ok(vec![
        "Heightmap point located".to_string(),
        format!("scale=1:{scale}"),
        format!("longitude={}", java_double_string(longitude)),
        format!("latitude={}", java_double_string(latitude)),
        format!("mapBlockX={map_x}"),
        format!("mapBlockZ={map_z}"),
        format!("globalBlockX={global_block_x}"),
        format!("globalBlockZ={global_block_z}"),
        format!("regionX={region_x}"),
        format!("regionZ={region_z}"),
        format!("localBlockX={local_block_x}"),
        format!("localBlockZ={local_block_z}"),
    ])
}

fn classify_surface_point(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    scale_text: &str,
    longitude_text: &str,
    latitude_text: &str,
) -> io::Result<i32> {
    match classify_surface_point_impl(heightmap_path, scale_text, longitude_text, latitude_text) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Surface point classification failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn classify_surface_point_impl(
    heightmap_path: &str,
    scale_text: &str,
    longitude_text: &str,
    latitude_text: &str,
) -> earthmap_geo::Result<Vec<String>> {
    let reader = GeoTiffHeightmapReader::open(Path::new(heightmap_path))?;
    let scale = parse_i32(scale_text)?;
    let longitude = parse_f64(longitude_text)?;
    let latitude = parse_f64(latitude_text)?;
    let mapping = mapping_for(reader.metadata(), scale)?;
    let map_x = mapping.block_x_for_longitude(longitude)?;
    let map_z = mapping.block_z_for_latitude(latitude)?;
    let cache = GeoTiffRowCache::new(&reader, 8)?;
    let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);
    let sampled_longitude = mapping.longitude_for_block_x(map_x)?;
    let sampled_latitude = mapping.latitude_for_block_z(map_z)?;
    let elevation = sampler.bilinear_meters(sampled_longitude, sampled_latitude)?;
    let column = classify_surface(elevation, sampled_longitude, sampled_latitude);
    Ok(vec![
        "Surface point classified".to_string(),
        format!("scale=1:{scale}"),
        format!("longitude={}", java_double_string(longitude)),
        format!("latitude={}", java_double_string(latitude)),
        format!("sampledLongitude={}", java_double_string(sampled_longitude)),
        format!("sampledLatitude={}", java_double_string(sampled_latitude)),
        format!("elevationMeters={}", java_double_string(elevation)),
        format!("water={}", column.water),
        format!("groundSurfaceY={}", column.ground_surface_y),
        format!(
            "waterSurfaceY={}",
            if column.water {
                column.water_surface_y.to_string()
            } else {
                "NONE".to_string()
            }
        ),
        format!("topBlockStateId={}", column.top_block_state_id),
        format!("fillerBlockStateId={}", column.filler_block_state_id),
        format!("biome={}", column.biome_id),
    ])
}

fn plan_representative_regions(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    scale_text: &str,
    output_csv: &str,
    target_regions_text: &str,
) -> io::Result<i32> {
    match plan_representative_regions_impl(
        heightmap_path,
        scale_text,
        output_csv,
        target_regions_text,
    ) {
        Ok(report) => {
            writeln!(out, "Representative region plan written")?;
            writeln!(out, "outputCsv={output_csv}")?;
            writeln!(out, "scale=1:{}", report.scale_denominator)?;
            writeln!(out, "widthBlocks={}", report.width_blocks)?;
            writeln!(out, "heightBlocks={}", report.height_blocks)?;
            writeln!(out, "candidateRegions={}", report.candidate_regions)?;
            writeln!(out, "selectedRegions={}", report.selected.len())?;
            let counts = report.selected_counts_by_class();
            for region_class in RepresentativeRegionClass::ALL {
                writeln!(
                    out,
                    "class.{}={}",
                    region_class.as_str(),
                    counts[region_class.index()]
                )?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Representative region planning failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn plan_representative_regions_impl(
    heightmap_path: &str,
    scale_text: &str,
    output_csv: &str,
    target_regions_text: &str,
) -> std::result::Result<RepresentativeRegionPlanReport, String> {
    let scale = parse_i32_string(scale_text)?;
    let target_regions = parse_positive_usize_string("targetRegions", target_regions_text)?;
    let reader = GeoTiffHeightmapReader::open(Path::new(heightmap_path))
        .map_err(|error| error.to_string())?;
    let mapping = mapping_for(reader.metadata(), scale).map_err(|error| error.to_string())?;
    let cache = GeoTiffRowCache::new(&reader, 64).map_err(|error| error.to_string())?;
    let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);
    let bounds = RegionGridBounds::for_mapping(&mapping);
    let mut candidates = Vec::new();
    for region_z in bounds.min_region_z..=bounds.max_region_z {
        for region_x in bounds.min_region_x..=bounds.max_region_x {
            candidates.push(classify_representative_region(
                &mapping, &sampler, region_x, region_z,
            )?);
        }
    }
    let selected = select_representative_regions(&candidates, target_regions);
    let report = RepresentativeRegionPlanReport {
        scale_denominator: scale,
        width_blocks: mapping.width_blocks,
        height_blocks: mapping.height_blocks,
        candidate_regions: candidates.len(),
        selected,
    };
    write_representative_region_csv(Path::new(output_csv), &report.selected)?;
    Ok(report)
}

fn describe_earth_grid(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    scale_text: &str,
) -> io::Result<i32> {
    match describe_earth_grid_impl(heightmap_path, scale_text) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Earth grid description failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn describe_earth_grid_impl(
    heightmap_path: &str,
    scale_text: &str,
) -> std::result::Result<Vec<String>, String> {
    let reader = GeoTiffHeightmapReader::open(Path::new(heightmap_path))
        .map_err(|error| error.to_string())?;
    let scale = parse_i32_string(scale_text)?;
    let mapping = mapping_for(reader.metadata(), scale).map_err(|error| error.to_string())?;
    let bounds = RegionGridBounds::for_mapping(&mapping);
    Ok(vec![
        "Earth grid described".to_string(),
        format!("heightmap={heightmap_path}"),
        format!("scale=1:{scale}"),
        format!("widthBlocks={}", mapping.width_blocks),
        format!("heightBlocks={}", mapping.height_blocks),
        format!("minRegionX={}", bounds.min_region_x),
        format!("maxRegionX={}", bounds.max_region_x),
        format!("minRegionZ={}", bounds.min_region_z),
        format!("maxRegionZ={}", bounds.max_region_z),
        format!("regionCols={}", bounds.columns()),
        format!("regionRows={}", bounds.rows()),
        format!("fullEarthRegions={}", bounds.region_count()),
        format!(
            "generateVanillaDelegatedArgs={heightmap_path} <worldDir> {scale} {} {} {} {} linear <threads> [maxRegionsThisRun] surface",
            bounds.min_region_x,
            bounds.min_region_z,
            bounds.columns(),
            bounds.rows()
        ),
        format!(
            "writeFinalizationArgs=<commandsFile> {} {} {} {} [windowChunks] [waitMs]",
            bounds.min_region_x,
            bounds.min_region_z,
            bounds.columns(),
            bounds.rows()
        ),
    ])
}

fn validate_surface_spawn(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
) -> io::Result<i32> {
    match validate_surface_spawn_impl(heightmap_path, scale_text, region_x_text, region_z_text) {
        Ok(report) => {
            writeln!(
                out,
                "{}",
                if report.viable {
                    "Surface spawn viable"
                } else {
                    "Surface spawn not viable"
                }
            )?;
            writeln!(out, "regionX={}", report.region_x)?;
            writeln!(out, "regionZ={}", report.region_z)?;
            writeln!(out, "landColumns={}", report.land_columns)?;
            writeln!(out, "waterColumns={}", report.water_columns)?;
            writeln!(out, "bestSpawnX={}", report.best_spawn_x)?;
            writeln!(out, "bestSpawnY={}", report.best_spawn_y)?;
            writeln!(out, "bestSpawnZ={}", report.best_spawn_z)?;
            Ok(if report.viable { EXIT_OK } else { EXIT_USAGE })
        }
        Err(error) => {
            writeln!(err, "Surface spawn validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn validate_surface_spawn_impl(
    heightmap_path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
) -> std::result::Result<SurfaceSpawnReport, String> {
    let reader = GeoTiffHeightmapReader::open(Path::new(heightmap_path))
        .map_err(|error| error.to_string())?;
    let scale = parse_i32_string(scale_text)?;
    let region_x = parse_i32_string(region_x_text)?;
    let region_z = parse_i32_string(region_z_text)?;
    let mapping = mapping_for(reader.metadata(), scale).map_err(|error| error.to_string())?;
    let cache = GeoTiffRowCache::new(&reader, 64).map_err(|error| error.to_string())?;
    let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);
    let mut land_columns = 0i32;
    let mut water_columns = 0i32;
    let mut best_spawn_x = 0i32;
    let mut best_spawn_z = 0i32;
    let mut best_spawn_y = i32::MIN;

    for dz in 0..REGION_SIZE_BLOCKS {
        for dx in 0..REGION_SIZE_BLOCKS {
            let global_block_x = region_x.wrapping_mul(REGION_SIZE_BLOCKS).wrapping_add(dx);
            let global_block_z = region_z.wrapping_mul(REGION_SIZE_BLOCKS).wrapping_add(dz);
            let map_x = global_block_x.wrapping_add(mapping.width_blocks / 2);
            let map_z = global_block_z.wrapping_add(mapping.height_blocks / 2);
            let column = classify_grid_sample(&mapping, &sampler, map_x, map_z)?;
            if column.water {
                water_columns = water_columns.saturating_add(1);
            } else {
                land_columns = land_columns.saturating_add(1);
                if column.ground_surface_y > best_spawn_y {
                    best_spawn_y = column.ground_surface_y;
                    best_spawn_x = global_block_x;
                    best_spawn_z = global_block_z;
                }
            }
        }
    }
    let viable = land_columns > 0 && best_spawn_y >= SEA_LEVEL_Y;
    Ok(SurfaceSpawnReport {
        region_x,
        region_z,
        land_columns,
        water_columns,
        best_spawn_x,
        best_spawn_y,
        best_spawn_z,
        viable,
    })
}

fn validate_height_seam(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    direction_text: &str,
) -> io::Result<i32> {
    match validate_height_seam_impl(
        heightmap_path,
        scale_text,
        region_x_text,
        region_z_text,
        direction_text,
    ) {
        Ok(report) => {
            writeln!(
                out,
                "{}",
                if report.passed() {
                    "Height seam valid"
                } else {
                    "Height seam invalid"
                }
            )?;
            writeln!(out, "regionX={}", report.region_x)?;
            writeln!(out, "regionZ={}", report.region_z)?;
            writeln!(out, "direction={}", report.direction.as_str())?;
            writeln!(out, "comparedColumns={}", report.compared_columns)?;
            writeln!(out, "coordinateFailures={}", report.coordinate_failures)?;
            writeln!(out, "minSurfaceY={}", report.min_surface_y)?;
            writeln!(out, "maxSurfaceY={}", report.max_surface_y)?;
            writeln!(out, "maxAbsSurfaceDelta={}", report.max_abs_surface_delta)?;
            Ok(if report.passed() { EXIT_OK } else { EXIT_USAGE })
        }
        Err(error) => {
            writeln!(err, "Height seam validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn validate_height_seam_impl(
    heightmap_path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    direction_text: &str,
) -> std::result::Result<HeightSeamReport, String> {
    let reader = GeoTiffHeightmapReader::open(Path::new(heightmap_path))
        .map_err(|error| error.to_string())?;
    let scale = parse_i32_string(scale_text)?;
    let region_x = parse_i32_string(region_x_text)?;
    let region_z = parse_i32_string(region_z_text)?;
    let direction = HeightSeamDirection::parse(direction_text)?;
    let mapping = mapping_for(reader.metadata(), scale).map_err(|error| error.to_string())?;
    let cache = GeoTiffRowCache::new(&reader, 64).map_err(|error| error.to_string())?;
    let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);

    let mut coordinate_failures = 0i32;
    let mut min_surface_y = i32::MAX;
    let mut max_surface_y = i32::MIN;
    let mut max_abs_surface_delta = 0i32;
    for index in 0..REGION_SIZE_BLOCKS {
        let pair = height_seam_boundary_pair(region_x, region_z, direction, index);
        let map_a_x = pair.0.wrapping_add(mapping.width_blocks / 2);
        let map_a_z = pair.1.wrapping_add(mapping.height_blocks / 2);
        let map_b_x = pair.2.wrapping_add(mapping.width_blocks / 2);
        let map_b_z = pair.3.wrapping_add(mapping.height_blocks / 2);
        if !height_seam_pair_is_adjacent(map_a_x, map_a_z, map_b_x, map_b_z, direction)
            || !inside_mapping(map_a_x, map_a_z, &mapping)
            || !inside_mapping(map_b_x, map_b_z, &mapping)
        {
            coordinate_failures = coordinate_failures.saturating_add(1);
            continue;
        }
        let surface_a = height_only_surface_y(&mapping, &sampler, map_a_x, map_a_z)?;
        let surface_b = height_only_surface_y(&mapping, &sampler, map_b_x, map_b_z)?;
        min_surface_y = min_surface_y.min(surface_a).min(surface_b);
        max_surface_y = max_surface_y.max(surface_a).max(surface_b);
        max_abs_surface_delta = max_abs_surface_delta.max((surface_a - surface_b).abs());
    }
    if min_surface_y == i32::MAX {
        min_surface_y = 0;
        max_surface_y = 0;
    }
    Ok(HeightSeamReport {
        region_x,
        region_z,
        direction,
        compared_columns: REGION_SIZE_BLOCKS,
        coordinate_failures,
        min_surface_y,
        max_surface_y,
        max_abs_surface_delta,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_vanilla_finalization_commands(
    out: &mut impl Write,
    err: &mut impl Write,
    output_commands_path: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    window_chunks_text: Option<&str>,
    wait_ms_text: Option<&str>,
) -> io::Result<i32> {
    match write_vanilla_finalization_commands_impl(
        output_commands_path,
        start_region_x_text,
        start_region_z_text,
        cols_text,
        rows_text,
        window_chunks_text,
        wait_ms_text,
    ) {
        Ok(report) => {
            writeln!(out, "Vanilla finalization command file written")?;
            writeln!(out, "commandsFile={}", report.commands_file.display())?;
            writeln!(out, "regionXStart={}", report.start_region_x)?;
            writeln!(out, "regionZStart={}", report.start_region_z)?;
            writeln!(out, "cols={}", report.cols)?;
            writeln!(out, "rows={}", report.rows)?;
            writeln!(out, "windowChunks={}", report.window_chunks)?;
            writeln!(out, "waitMs={}", report.wait_ms)?;
            writeln!(out, "windows={}", report.windows)?;
            writeln!(
                out,
                "maxChunksPerWindow={}",
                report.window_chunks * report.window_chunks
            )?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(
                err,
                "Vanilla finalization command generation failed: {error}"
            )?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_vanilla_finalization_commands_impl(
    output_commands_path: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    window_chunks_text: Option<&str>,
    wait_ms_text: Option<&str>,
) -> std::result::Result<VanillaFinalizationCommandReport, String> {
    let start_region_x = parse_i32_string(start_region_x_text)?;
    let start_region_z = parse_i32_string(start_region_z_text)?;
    let cols = parse_i32_string(cols_text)?;
    let rows = parse_i32_string(rows_text)?;
    let window_chunks = window_chunks_text
        .map(parse_i32_string)
        .transpose()?
        .unwrap_or(16);
    let wait_ms = wait_ms_text
        .map(parse_i32_string)
        .transpose()?
        .unwrap_or(8000);
    if cols <= 0 || rows <= 0 || window_chunks <= 0 || window_chunks > 16 || wait_ms < 0 {
        return Err(
            "cols, rows, and windowChunks must be positive; windowChunks must be <= 16; waitMs >= 0"
                .to_string(),
        );
    }
    let min_chunk_x = start_region_x.wrapping_mul(32);
    let max_chunk_x = start_region_x
        .wrapping_add(cols)
        .wrapping_mul(32)
        .wrapping_sub(1);
    let min_chunk_z = start_region_z.wrapping_mul(32);
    let max_chunk_z = start_region_z
        .wrapping_add(rows)
        .wrapping_mul(32)
        .wrapping_sub(1);
    let mut commands = String::new();
    commands.push_str("# Generated by Super-Rapid-EarthMap-Generator\n");
    commands.push_str(
        "# Finalizes vanilla-delegated chunks by force-loading <= 256 chunks per window.\n",
    );
    commands.push_str(&format!(
        "# range.regionX={}..{}\n",
        start_region_x,
        start_region_x + cols - 1
    ));
    commands.push_str(&format!(
        "# range.regionZ={}..{}\n",
        start_region_z,
        start_region_z + rows - 1
    ));
    commands.push_str(&format!("# windowChunks={window_chunks}\n"));
    commands.push_str(&format!("# waitMs={wait_ms}\n"));

    let mut windows = 0i32;
    let mut chunk_z = min_chunk_z;
    while chunk_z <= max_chunk_z {
        let end_chunk_z = max_chunk_z.min(chunk_z + window_chunks - 1);
        let mut chunk_x = min_chunk_x;
        while chunk_x <= max_chunk_x {
            let end_chunk_x = max_chunk_x.min(chunk_x + window_chunks - 1);
            append_forceload_window(
                &mut commands,
                chunk_x,
                chunk_z,
                end_chunk_x,
                end_chunk_z,
                wait_ms,
            );
            windows += 1;
            chunk_x += window_chunks;
        }
        chunk_z += window_chunks;
    }
    commands.push_str("save-all flush\n");
    commands.push_str("@wait-ms 5000\n");

    let output = std::path::PathBuf::from(output_commands_path);
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
    }
    std::fs::write(&output, commands).map_err(|error| error.to_string())?;
    Ok(VanillaFinalizationCommandReport {
        commands_file: output,
        start_region_x,
        start_region_z,
        cols,
        rows,
        window_chunks,
        wait_ms,
        windows,
    })
}

fn append_forceload_window(
    commands: &mut String,
    chunk_x: i32,
    chunk_z: i32,
    end_chunk_x: i32,
    end_chunk_z: i32,
    wait_ms: i32,
) {
    let min_block_x = chunk_x * CHUNK_WIDTH as i32;
    let min_block_z = chunk_z * CHUNK_WIDTH as i32;
    let max_block_x = (end_chunk_x * CHUNK_WIDTH as i32) + CHUNK_WIDTH as i32 - 1;
    let max_block_z = (end_chunk_z * CHUNK_WIDTH as i32) + CHUNK_WIDTH as i32 - 1;
    commands.push_str(&format!(
        "forceload add {min_block_x} {min_block_z} {max_block_x} {max_block_z}\n"
    ));
    commands.push_str(&format!("@wait-ms {wait_ms}\n"));
    commands.push_str("save-all flush\n");
    commands.push_str("@wait-ms 1000\n");
    commands.push_str(&format!(
        "forceload remove {min_block_x} {min_block_z} {max_block_x} {max_block_z}\n"
    ));
    commands.push_str("@wait-ms 500\n");
}

fn validate_survival_manifest(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
) -> io::Result<i32> {
    match earthmap_gameplay::validate_survival_manifest(Path::new(path)) {
        Ok(report) => {
            writeln!(out, "Survival manifest validated")?;
            write_survival_gate_report(out, &report)?;
            Ok(if report.manifest_valid {
                EXIT_OK
            } else {
                EXIT_USAGE
            })
        }
        Err(error) => {
            writeln!(err, "Survival manifest validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_survival_evidence(
    out: &mut impl Write,
    err: &mut impl Write,
    source_manifest: &str,
    output_manifest: &str,
    boot_log: &str,
    reboot_log: &str,
    spawn_to_end_log: &str,
    claim: &str,
) -> io::Result<i32> {
    match earthmap_gameplay::apply_survival_evidence(
        Path::new(source_manifest),
        Path::new(output_manifest),
        Path::new(boot_log),
        Path::new(reboot_log),
        Path::new(spawn_to_end_log),
        claim,
    ) {
        Ok(report) => {
            writeln!(out, "Survival evidence applied")?;
            writeln!(out, "manifestFile={}", report.manifest_path.display())?;
            write_survival_gate_report(out, &report.gate_report)?;
            Ok(
                if report.gate_report.manifest_valid
                    && report.gate_report.missing_requirements.is_empty()
                {
                    EXIT_OK
                } else {
                    EXIT_USAGE
                },
            )
        }
        Err(error) => {
            writeln!(err, "Survival evidence application failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn write_survival_gate_report(
    out: &mut impl Write,
    report: &earthmap_gameplay::SurvivalGateReport,
) -> io::Result<()> {
    writeln!(out, "manifestValid={}", report.manifest_valid)?;
    writeln!(
        out,
        "survivalCompleteAllowed={}",
        report.survival_complete_allowed
    )?;
    writeln!(out, "claim={}", report.claim)?;
    writeln!(
        out,
        "missingRequirements={}",
        report.missing_requirements.len()
    )?;
    for missing in &report.missing_requirements {
        writeln!(out, "missing={missing}")?;
    }
    Ok(())
}

fn validate_cave_density(
    out: &mut impl Write,
    err: &mut impl Write,
    seed_text: &str,
    min_block_x_text: &str,
    min_block_z_text: &str,
    size_blocks_text: &str,
) -> io::Result<i32> {
    match parse_cave_args(
        seed_text,
        min_block_x_text,
        min_block_z_text,
        size_blocks_text,
    )
    .and_then(|(seed, min_block_x, min_block_z, size_blocks)| {
        earthmap_gameplay::validate_cave_density(seed, min_block_x, min_block_z, size_blocks)
    }) {
        Ok(report) => {
            writeln!(out, "Cave density validated")?;
            writeln!(out, "seed={}", report.seed)?;
            writeln!(out, "minBlockX={}", report.min_block_x)?;
            writeln!(out, "minBlockZ={}", report.min_block_z)?;
            writeln!(out, "sizeBlocks={}", report.size_blocks)?;
            writeln!(out, "minY={}", report.min_y)?;
            writeln!(out, "maxY={}", report.max_y)?;
            writeln!(out, "sampledBlocks={}", report.sampled_blocks)?;
            writeln!(out, "caveCandidateBlocks={}", report.cave_candidate_blocks)?;
            writeln!(out, "caveRatio={}", java_double_string(report.cave_ratio))?;
            writeln!(out, "minDensity={}", java_double_string(report.min_density))?;
            writeln!(out, "maxDensity={}", java_double_string(report.max_density))?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Cave density validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn validate_cave_connectivity(
    out: &mut impl Write,
    err: &mut impl Write,
    seed_text: &str,
    min_block_x_text: &str,
    min_block_z_text: &str,
    size_blocks_text: &str,
) -> io::Result<i32> {
    match parse_cave_args(
        seed_text,
        min_block_x_text,
        min_block_z_text,
        size_blocks_text,
    )
    .and_then(|(seed, min_block_x, min_block_z, size_blocks)| {
        earthmap_gameplay::validate_cave_connectivity(seed, min_block_x, min_block_z, size_blocks)
    }) {
        Ok(report) => {
            writeln!(out, "Cave connectivity validated")?;
            writeln!(out, "seed={}", report.seed)?;
            writeln!(out, "minBlockX={}", report.min_block_x)?;
            writeln!(out, "minBlockZ={}", report.min_block_z)?;
            writeln!(out, "sizeBlocks={}", report.size_blocks)?;
            writeln!(out, "minY={}", report.min_y)?;
            writeln!(out, "maxY={}", report.max_y)?;
            writeln!(out, "caveCandidateBlocks={}", report.cave_candidate_blocks)?;
            writeln!(out, "componentCount={}", report.component_count)?;
            writeln!(
                out,
                "largestComponentBlocks={}",
                report.largest_component_blocks
            )?;
            writeln!(
                out,
                "largestComponentRatio={}",
                java_double_string(report.largest_component_ratio)
            )?;
            writeln!(
                out,
                "entranceCandidateConnected={}",
                report.entrance_candidate_connected
            )?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Cave connectivity validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn parse_cave_args(
    seed_text: &str,
    min_block_x_text: &str,
    min_block_z_text: &str,
    size_blocks_text: &str,
) -> std::result::Result<(i64, i32, i32, i32), String> {
    Ok((
        seed_text
            .parse::<i64>()
            .map_err(|error| error.to_string())?,
        parse_i32_string(min_block_x_text)?,
        parse_i32_string(min_block_z_text)?,
        parse_i32_string(size_blocks_text)?,
    ))
}

fn validate_ore_histogram_synthetic(out: &mut impl Write, err: &mut impl Write) -> io::Result<i32> {
    match earthmap_gameplay::validate_ore_histogram_synthetic() {
        Ok(report) => {
            write_ore_histogram_report(out, &report)?;
            Ok(if report.survival_critical_complete() {
                EXIT_OK
            } else {
                EXIT_USAGE
            })
        }
        Err(error) => {
            writeln!(err, "Ore histogram validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn write_ore_histogram_report(
    out: &mut impl Write,
    report: &earthmap_gameplay::OreHistogramReport,
) -> io::Result<()> {
    writeln!(out, "Ore histogram validated")?;
    writeln!(out, "chunkCount={}", report.chunk_count)?;
    writeln!(out, "sampledBlocks={}", report.sampled_blocks)?;
    writeln!(out, "totalOreBlocks={}", report.total_ore_blocks)?;
    writeln!(
        out,
        "survivalCriticalComplete={}",
        report.survival_critical_complete()
    )?;
    for kind in earthmap_gameplay::OreKind::ALL {
        writeln!(
            out,
            "ore.{}={}",
            kind.id(),
            report.counts.get(&kind).copied().unwrap_or(0)
        )?;
    }
    for kind in report.missing_survival_critical_ores.keys() {
        writeln!(out, "missingOre={}", kind.id())?;
    }
    Ok(())
}

fn validate_underground_fluid_synthetic(
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<i32> {
    match earthmap_gameplay::validate_underground_fluid_synthetic() {
        Ok(report) => {
            writeln!(out, "Underground fluid validated")?;
            writeln!(out, "waterBlocks={}", report.water_blocks)?;
            writeln!(out, "lavaBlocks={}", report.lava_blocks)?;
            writeln!(out, "totalFluidBlocks={}", report.total_fluid_blocks())?;
            Ok(if report.water_blocks > 0 && report.lava_blocks > 0 {
                EXIT_OK
            } else {
                EXIT_USAGE
            })
        }
        Err(error) => {
            writeln!(err, "Underground fluid validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn validate_global_resource_fairness(
    out: &mut impl Write,
    err: &mut impl Write,
    world_dir: &str,
    output_dir: &str,
) -> io::Result<i32> {
    match earthmap_gameplay::validate_global_resource_fairness_linear_world(
        Path::new(world_dir),
        Path::new(output_dir),
    ) {
        Ok(report) => {
            writeln!(out, "Global resource fairness validated")?;
            writeln!(out, "reportFile={}", report.report_path.display())?;
            writeln!(
                out,
                "missingRegionsCsv={}",
                report.missing_regions_csv.display()
            )?;
            writeln!(out, "scannedRegions={}", report.scanned_regions)?;
            writeln!(out, "completeRegions={}", report.complete_regions)?;
            writeln!(out, "fullChunkRegions={}", report.full_chunk_regions)?;
            writeln!(out, "partialChunkRegions={}", report.partial_chunk_regions)?;
            writeln!(out, "invalidRegions={}", report.invalid_regions)?;
            writeln!(
                out,
                "bitmapDiagnosticRegions={}",
                report.bitmap_diagnostic_regions
            )?;
            writeln!(
                out,
                "regionsMissingCriticalOres={}",
                report.regions_missing_critical_ores
            )?;
            for (kind, count) in &report.missing_region_counts_by_ore {
                writeln!(out, "ore.{}.missingRegionCount={}", kind.id(), count)?;
            }
            writeln!(out, "elapsedMillis={}", report.elapsed_millis)?;
            writeln!(out, "resourceFairnessPass={}", report.pass)?;
            Ok(if report.pass { EXIT_OK } else { EXIT_USAGE })
        }
        Err(error) => {
            writeln!(err, "Global resource fairness validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn validate_loot_economy(
    out: &mut impl Write,
    err: &mut impl Write,
    world_dir: &str,
    output_dir: &str,
) -> io::Result<i32> {
    match earthmap_gameplay::validate_loot_economy_linear_world(
        Path::new(world_dir),
        Path::new(output_dir),
    ) {
        Ok(report) => {
            writeln!(out, "Loot economy validated")?;
            writeln!(out, "reportFile={}", report.report_path.display())?;
            writeln!(out, "issuesCsv={}", report.issues_csv.display())?;
            writeln!(out, "scannedRegions={}", report.scanned_regions)?;
            writeln!(out, "completeRegions={}", report.complete_regions)?;
            writeln!(out, "fullChunkRegions={}", report.full_chunk_regions)?;
            writeln!(out, "partialChunkRegions={}", report.partial_chunk_regions)?;
            writeln!(out, "invalidRegions={}", report.invalid_regions)?;
            writeln!(
                out,
                "bitmapDiagnosticRegions={}",
                report.bitmap_diagnostic_regions
            )?;
            writeln!(
                out,
                "regionsWithStrongholdLootChest={}",
                report.regions_with_stronghold_loot_chest
            )?;
            writeln!(
                out,
                "regionsWithBlazeSpawner={}",
                report.regions_with_blaze_spawner
            )?;
            writeln!(
                out,
                "regionsWithEndPortal={}",
                report.regions_with_end_portal
            )?;
            writeln!(
                out,
                "regionsWithEndPortalFrame={}",
                report.regions_with_end_portal_frame
            )?;
            writeln!(
                out,
                "regionsWithSpawnerBlock={}",
                report.regions_with_spawner_block
            )?;
            writeln!(
                out,
                "regionsWithCompleteProgression={}",
                report.regions_with_complete_progression
            )?;
            writeln!(out, "candidateChunks={}", report.candidate_chunks)?;
            writeln!(
                out,
                "structuredProgressionChunks={}",
                report.structured_progression_chunks
            )?;
            writeln!(out, "issueRegions={}", report.issue_regions)?;
            writeln!(out, "elapsedMillis={}", report.elapsed_millis)?;
            writeln!(out, "lootEconomyPass={}", report.pass)?;
            Ok(if report.pass { EXIT_OK } else { EXIT_USAGE })
        }
        Err(error) => {
            writeln!(err, "Loot economy validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_nation_war_readiness_report(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    scale_text: &str,
    output_dir: &str,
    faction_count_text: &str,
    safe_zone_radius_text: &str,
    global_resource_fairness_report: Option<&str>,
    loot_economy_report: Option<&str>,
    plugin_stack_report: Option<&str>,
    chunk_load_stress_report: Option<&str>,
) -> io::Result<i32> {
    match generate_nation_war_readiness_report_impl(
        heightmap_path,
        scale_text,
        output_dir,
        faction_count_text,
        safe_zone_radius_text,
        global_resource_fairness_report,
        loot_economy_report,
        plugin_stack_report,
        chunk_load_stress_report,
    ) {
        Ok(report) => {
            writeln!(out, "Nation-war readiness report generated")?;
            writeln!(out, "reportFile={}", report.report_path.display())?;
            writeln!(
                out,
                "factionStartsCsv={}",
                report.faction_starts_csv.display()
            )?;
            writeln!(
                out,
                "operatorLaunchReport={}",
                report.operator_launch_report.display()
            )?;
            writeln!(out, "scale=1:{}", report.scale_denominator)?;
            writeln!(
                out,
                "factionCountRequested={}",
                report.faction_count_requested
            )?;
            writeln!(out, "factionCountSelected={}", report.faction_starts.len())?;
            writeln!(
                out,
                "safeZoneRadiusBlocks={}",
                report.safe_zone_radius_blocks
            )?;
            writeln!(
                out,
                "worldBorderCenterX={}",
                java_double_string(report.world_border.center_x)
            )?;
            writeln!(
                out,
                "worldBorderCenterZ={}",
                java_double_string(report.world_border.center_z)
            )?;
            writeln!(
                out,
                "worldBorderDiameterBlocks={}",
                java_double_string(report.world_border.diameter_blocks)
            )?;
            writeln!(
                out,
                "worldBorderIncludesFullEarth={}",
                report.world_border.includes_earth()
            )?;
            writeln!(out, "spawnPolicyPass={}", report.spawn_policy_pass)?;
            writeln!(
                out,
                "resourceFairnessModelPass={}",
                report.resource_fairness_model_pass
            )?;
            writeln!(
                out,
                "globalResourceFairnessPass={}",
                report.global_resource_fairness_pass
            )?;
            writeln!(
                out,
                "progressionInsideBorderPass={}",
                report.progression_inside_border_pass
            )?;
            writeln!(
                out,
                "multipleFactionProgressionPass={}",
                report.multiple_faction_progression_pass
            )?;
            writeln!(out, "lootEconomyPass={}", report.loot_economy_pass)?;
            writeln!(out, "pluginStackPass={}", report.plugin_stack_pass)?;
            writeln!(out, "chunkLoadStressPass={}", report.chunk_load_stress_pass)?;
            writeln!(out, "overallPass={}", report.overall_pass)?;
            writeln!(out, "nationWarReady={}", report.overall_pass)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(
                err,
                "Nation-war readiness report generation failed: {error}"
            )?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_nation_war_readiness_report_impl(
    heightmap_path: &str,
    scale_text: &str,
    output_dir: &str,
    faction_count_text: &str,
    safe_zone_radius_text: &str,
    global_resource_fairness_report: Option<&str>,
    loot_economy_report: Option<&str>,
    plugin_stack_report: Option<&str>,
    chunk_load_stress_report: Option<&str>,
) -> std::result::Result<NationWarReadinessReport, String> {
    let scale = parse_i32_string(scale_text)?;
    let faction_count = parse_positive_i32_string("factionCount", faction_count_text)?;
    let safe_zone_radius_blocks =
        parse_positive_i32_string("safeZoneRadiusBlocks", safe_zone_radius_text)?;
    let output_dir = Path::new(output_dir);
    std::fs::create_dir_all(output_dir).map_err(|error| error.to_string())?;
    let reader = GeoTiffHeightmapReader::open(Path::new(heightmap_path))
        .map_err(|error| error.to_string())?;
    let mapping = mapping_for(reader.metadata(), scale).map_err(|error| error.to_string())?;
    let bounds = RegionGridBounds::for_mapping(&mapping);
    let world_border = nation_war_world_border(&mapping);
    let representative_plan = nation_war_representative_plan(heightmap_path, scale, faction_count)?;
    let faction_starts = select_nation_war_faction_starts(
        &representative_plan.selected,
        faction_count,
        safe_zone_radius_blocks,
        world_border,
    );
    let spawn_policy_pass = faction_starts.len() == faction_count as usize
        && min_nation_war_start_distance(&faction_starts)
            >= f64::from(safe_zone_radius_blocks) * 2.0;
    let selected_starts_inside_border =
        !faction_starts.is_empty() && faction_starts.iter().all(|start| start.inside_border);
    let resource_fairness_model_pass = selected_starts_inside_border
        && !faction_starts.is_empty()
        && faction_starts
            .iter()
            .all(nation_war_start_resource_eligible);
    let global_resource_fairness_pass = global_resource_fairness_report
        .map(|path| earthmap_gameplay::global_resource_fairness_report_pass(Path::new(path)))
        .transpose()?
        .unwrap_or(false);
    let loot_economy_pass = loot_economy_report
        .map(|path| earthmap_gameplay::loot_economy_report_pass(Path::new(path)))
        .transpose()?
        .unwrap_or(false);
    let plugin_stack_pass = plugin_stack_report
        .map(|path| earthmap_gameplay::evidence_report_pass(Path::new(path), "plugin-stack"))
        .transpose()?
        .unwrap_or(false);
    let chunk_load_stress_pass = chunk_load_stress_report
        .map(|path| earthmap_gameplay::evidence_report_pass(Path::new(path), "chunk-load-stress"))
        .transpose()?
        .unwrap_or(false);
    let progression_inside_border_pass =
        world_border.includes_earth() && selected_starts_inside_border;
    let multiple_faction_progression_pass =
        progression_inside_border_pass && faction_starts.len() > 1;
    let overall_pass = spawn_policy_pass
        && resource_fairness_model_pass
        && global_resource_fairness_pass
        && progression_inside_border_pass
        && multiple_faction_progression_pass
        && world_border.includes_earth()
        && plugin_stack_pass
        && chunk_load_stress_pass
        && loot_economy_pass;
    let report_path = output_dir.join("earthmap-nation-war-readiness.properties");
    let faction_starts_csv = output_dir.join("earthmap-faction-starts.csv");
    let operator_launch_report = output_dir.join("earthmap-operator-launch-report.md");
    write_nation_war_faction_starts_csv(&faction_starts_csv, &faction_starts)?;
    write_nation_war_properties(
        &report_path,
        Path::new(heightmap_path),
        &mapping,
        bounds,
        world_border,
        &representative_plan,
        &faction_starts_csv,
        &operator_launch_report,
        faction_count,
        safe_zone_radius_blocks,
        &faction_starts,
        global_resource_fairness_report,
        loot_economy_report,
        plugin_stack_report,
        chunk_load_stress_report,
        spawn_policy_pass,
        resource_fairness_model_pass,
        global_resource_fairness_pass,
        progression_inside_border_pass,
        multiple_faction_progression_pass,
        loot_economy_pass,
        plugin_stack_pass,
        chunk_load_stress_pass,
        overall_pass,
    )?;
    write_nation_war_operator_report(
        &operator_launch_report,
        &report_path,
        &faction_starts_csv,
        world_border,
        faction_count,
        safe_zone_radius_blocks,
        &faction_starts,
        spawn_policy_pass,
        resource_fairness_model_pass,
        global_resource_fairness_pass,
        progression_inside_border_pass,
        multiple_faction_progression_pass,
        loot_economy_pass,
        plugin_stack_pass,
        chunk_load_stress_pass,
        overall_pass,
    )?;
    Ok(NationWarReadinessReport {
        report_path,
        faction_starts_csv,
        operator_launch_report,
        scale_denominator: scale,
        faction_count_requested: faction_count,
        safe_zone_radius_blocks,
        faction_starts,
        world_border,
        spawn_policy_pass,
        resource_fairness_model_pass,
        global_resource_fairness_pass,
        progression_inside_border_pass,
        multiple_faction_progression_pass,
        loot_economy_pass,
        plugin_stack_pass,
        chunk_load_stress_pass,
        overall_pass,
    })
}

fn nation_war_representative_plan(
    heightmap_path: &str,
    scale: i32,
    faction_count: i32,
) -> std::result::Result<RepresentativeRegionPlanReport, String> {
    let target_regions = 32usize.max(faction_count as usize * 12);
    let reader = GeoTiffHeightmapReader::open(Path::new(heightmap_path))
        .map_err(|error| error.to_string())?;
    let mapping = mapping_for(reader.metadata(), scale).map_err(|error| error.to_string())?;
    let cache = GeoTiffRowCache::new(&reader, 64).map_err(|error| error.to_string())?;
    let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);
    let bounds = RegionGridBounds::for_mapping(&mapping);
    let mut candidates = Vec::new();
    for region_z in bounds.min_region_z..=bounds.max_region_z {
        for region_x in bounds.min_region_x..=bounds.max_region_x {
            candidates.push(classify_representative_region(
                &mapping, &sampler, region_x, region_z,
            )?);
        }
    }
    let selected = select_representative_regions(&candidates, target_regions);
    Ok(RepresentativeRegionPlanReport {
        scale_denominator: scale,
        width_blocks: mapping.width_blocks,
        height_blocks: mapping.height_blocks,
        candidate_regions: candidates.len(),
        selected,
    })
}

fn select_nation_war_faction_starts(
    candidates: &[RegionCandidate],
    faction_count: i32,
    safe_zone_radius_blocks: i32,
    world_border: NationWarWorldBorderPlan,
) -> Vec<NationWarFactionStart> {
    let mut viable = Vec::new();
    for candidate in candidates {
        if candidate.region_class == RepresentativeRegionClass::Ocean
            || candidate.water_ratio > 0.25
            || candidate.max_ground_y < SEA_LEVEL_Y
        {
            continue;
        }
        let global_x = (candidate.region_x * REGION_SIZE_BLOCKS) + (REGION_SIZE_BLOCKS / 2);
        let global_z = (candidate.region_z * REGION_SIZE_BLOCKS) + (REGION_SIZE_BLOCKS / 2);
        viable.push(NationWarFactionStart {
            index: viable.len(),
            region_x: candidate.region_x,
            region_z: candidate.region_z,
            global_x,
            global_z,
            region_class: candidate.region_class,
            water_ratio: candidate.water_ratio,
            min_ground_y: candidate.min_ground_y,
            max_ground_y: candidate.max_ground_y,
            dominant_biome: candidate.dominant_biome.clone(),
            inside_border: world_border.contains(f64::from(global_x), f64::from(global_z)),
        });
    }
    viable.sort_by(|left, right| {
        distance_from_origin(left)
            .total_cmp(&distance_from_origin(right))
            .then(left.region_z.cmp(&right.region_z))
            .then(left.region_x.cmp(&right.region_x))
    });
    let min_distance = f64::from(safe_zone_radius_blocks) * 2.0;
    let min_distance_squared = min_distance * min_distance;
    let mut selected = Vec::new();
    while selected.len() < faction_count as usize {
        let mut best: Option<NationWarFactionStart> = None;
        let mut best_score = -1.0;
        for candidate in &viable {
            if selected.iter().any(|selected: &NationWarFactionStart| {
                selected.region_x == candidate.region_x && selected.region_z == candidate.region_z
            }) {
                continue;
            }
            let score = if selected.is_empty() {
                f64::MAX
            } else {
                min_distance_squared_to_selected(candidate, &selected)
            };
            if score < min_distance_squared {
                continue;
            }
            if score > best_score {
                best = Some(candidate.clone());
                best_score = score;
            }
        }
        let Some(mut best) = best else {
            break;
        };
        best.index = selected.len();
        selected.push(best);
    }
    selected
}

fn nation_war_world_border(mapping: &EarthScaleMapping) -> NationWarWorldBorderPlan {
    let min_block_x = -(mapping.width_blocks / 2);
    let max_block_x = mapping.width_blocks - (mapping.width_blocks / 2) - 1;
    let min_block_z = -(mapping.height_blocks / 2);
    let max_block_z = mapping.height_blocks - (mapping.height_blocks / 2) - 1;
    let center_x = f64::from(min_block_x + max_block_x) / 2.0;
    let center_z = f64::from(min_block_z + max_block_z) / 2.0;
    let span_x = max_block_x - min_block_x + 1;
    let span_z = max_block_z - min_block_z + 1;
    let diameter_blocks = f64::from(span_x.max(span_z));
    let mut border = NationWarWorldBorderPlan {
        center_x,
        center_z,
        diameter_blocks,
        earth_min_block_x: min_block_x,
        earth_max_block_x: max_block_x,
        earth_min_block_z: min_block_z,
        earth_max_block_z: max_block_z,
    };
    if !border.includes_earth() {
        border.diameter_blocks += 1.0;
    }
    border
}

fn nation_war_start_resource_eligible(start: &NationWarFactionStart) -> bool {
    start.region_class != RepresentativeRegionClass::Ocean
        && start.water_ratio <= 0.25
        && start.max_ground_y >= SEA_LEVEL_Y
}

fn distance_from_origin(start: &NationWarFactionStart) -> f64 {
    f64::from(start.global_x).hypot(f64::from(start.global_z))
}

fn min_distance_squared_to_selected(
    candidate: &NationWarFactionStart,
    selected: &[NationWarFactionStart],
) -> f64 {
    selected
        .iter()
        .map(|existing| {
            let dx = f64::from(candidate.global_x - existing.global_x);
            let dz = f64::from(candidate.global_z - existing.global_z);
            (dx * dx) + (dz * dz)
        })
        .fold(f64::MAX, f64::min)
}

fn min_nation_war_start_distance(starts: &[NationWarFactionStart]) -> f64 {
    if starts.len() < 2 {
        return 0.0;
    }
    let mut best = f64::MAX;
    for left in 0..starts.len() {
        for right in left + 1..starts.len() {
            let dx = f64::from(starts[left].global_x - starts[right].global_x);
            let dz = f64::from(starts[left].global_z - starts[right].global_z);
            best = best.min(dx.hypot(dz));
        }
    }
    best
}

fn write_nation_war_faction_starts_csv(
    path: &Path,
    starts: &[NationWarFactionStart],
) -> std::result::Result<(), String> {
    let mut text = "index,regionX,regionZ,globalX,globalZ,class,waterRatio,minGroundY,maxGroundY,dominantBiome,insideBorder,progressionModel\n".to_string();
    for start in starts {
        text.push_str(&format!(
            "{},{},{},{},{},{},{:.4},{},{},{},{},stronghold-equivalent-per-generated-survival-region\n",
            start.index,
            start.region_x,
            start.region_z,
            start.global_x,
            start.global_z,
            start.region_class.as_str(),
            start.water_ratio,
            start.min_ground_y,
            start.max_ground_y,
            start.dominant_biome,
            start.inside_border,
        ));
    }
    std::fs::write(path, text).map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
fn write_nation_war_properties(
    path: &Path,
    heightmap_path: &Path,
    mapping: &EarthScaleMapping,
    bounds: RegionGridBounds,
    border: NationWarWorldBorderPlan,
    representative_plan: &RepresentativeRegionPlanReport,
    starts_path: &Path,
    operator_report_path: &Path,
    faction_count: i32,
    safe_zone_radius_blocks: i32,
    starts: &[NationWarFactionStart],
    global_resource_fairness_report: Option<&str>,
    loot_economy_report: Option<&str>,
    plugin_stack_report: Option<&str>,
    chunk_load_stress_report: Option<&str>,
    spawn_policy_pass: bool,
    resource_fairness_model_pass: bool,
    global_resource_fairness_pass: bool,
    progression_inside_border_pass: bool,
    multiple_faction_progression_pass: bool,
    loot_economy_pass: bool,
    plugin_stack_pass: bool,
    chunk_load_stress_pass: bool,
    overall_pass: bool,
) -> std::result::Result<(), String> {
    let mut values = BTreeMap::new();
    values.insert(
        "report.type".to_string(),
        "nation-war-readiness".to_string(),
    );
    values.insert(
        "evidence.schemaVersion".to_string(),
        earthmap_gameplay::EVIDENCE_SCHEMA_VERSION.to_string(),
    );
    values.insert(
        "generatedAtUtc".to_string(),
        earthmap_gameplay::STABLE_GENERATED_AT_UTC.to_string(),
    );
    values.insert(
        "evidence.scope".to_string(),
        "nation-war-readiness-preflight".to_string(),
    );
    values.insert(
        "minecraft.version".to_string(),
        build_info::MINECRAFT_TARGET.to_string(),
    );
    values.insert(
        "server.profile".to_string(),
        build_info::SERVER_PROFILE.to_string(),
    );
    values.insert(
        "heightmap.path".to_string(),
        normalized_path_display(heightmap_path),
    );
    values.insert(
        "generation.scaleDenominator".to_string(),
        mapping.denominator.to_string(),
    );
    values.insert(
        "generation.widthBlocks".to_string(),
        mapping.width_blocks.to_string(),
    );
    values.insert(
        "generation.heightBlocks".to_string(),
        mapping.height_blocks.to_string(),
    );
    values.insert(
        "generation.regionCount".to_string(),
        bounds.region_count().to_string(),
    );
    values.insert(
        "generation.regionCols".to_string(),
        bounds.columns().to_string(),
    );
    values.insert(
        "generation.regionRows".to_string(),
        bounds.rows().to_string(),
    );
    values.insert(
        "assumption.worldType".to_string(),
        "earth-survival-nation-war".to_string(),
    );
    values.insert(
        "assumption.progressionModel".to_string(),
        "stronghold-equivalent-per-generated-survival-region".to_string(),
    );
    values.insert(
        "assumption.vanillaStrongholdDistributionComplete".to_string(),
        "false".to_string(),
    );
    values.insert(
        "spawn.safeZoneRadiusBlocks".to_string(),
        safe_zone_radius_blocks.to_string(),
    );
    values.insert(
        "spawn.factionCountRequested".to_string(),
        faction_count.to_string(),
    );
    values.insert(
        "spawn.factionCountSelected".to_string(),
        starts.len().to_string(),
    );
    values.insert(
        "spawn.minDistanceBlocks".to_string(),
        java_double_string(min_nation_war_start_distance(starts)),
    );
    values.insert(
        "spawn.policyPass".to_string(),
        spawn_policy_pass.to_string(),
    );
    values.insert(
        "spawn.factionStartsCsv".to_string(),
        file_name_string(starts_path),
    );
    values.insert(
        "resources.model".to_string(),
        "deterministic-ore-forced-coverage-per-full-region".to_string(),
    );
    values.insert(
        "resources.deterministicCoverageModelAvailable".to_string(),
        "true".to_string(),
    );
    values.insert(
        "resources.selectedFactionStartModelPass".to_string(),
        resource_fairness_model_pass.to_string(),
    );
    values.insert(
        "resources.fairnessModelPass".to_string(),
        resource_fairness_model_pass.to_string(),
    );
    values.insert(
        "resources.globalFairnessProven".to_string(),
        global_resource_fairness_pass.to_string(),
    );
    values.insert(
        "resources.globalFairnessReportProvided".to_string(),
        global_resource_fairness_report.is_some().to_string(),
    );
    if let Some(path) = global_resource_fairness_report {
        values.insert(
            "resources.globalFairnessReportFile".to_string(),
            normalized_path_display(Path::new(path)),
        );
    }
    values.insert(
        "stronghold.model".to_string(),
        "one-stronghold-equivalent-in-region-center-chunk".to_string(),
    );
    values.insert(
        "stronghold.distributionModelAvailable".to_string(),
        (starts.len() == faction_count as usize).to_string(),
    );
    values.insert(
        "stronghold.distributionReportPass".to_string(),
        multiple_faction_progression_pass.to_string(),
    );
    values.insert(
        "nether.model".to_string(),
        "overworld-blaze-spawner-equivalent".to_string(),
    );
    values.insert(
        "nether.accessDistributionModelAvailable".to_string(),
        (starts.len() == faction_count as usize).to_string(),
    );
    values.insert(
        "nether.accessDistributionPass".to_string(),
        multiple_faction_progression_pass.to_string(),
    );
    values.insert(
        "loot.model".to_string(),
        "generated-stronghold-loot-chest-and-progression-block-entity-scan".to_string(),
    );
    values.insert(
        "loot.economyPass".to_string(),
        loot_economy_pass.to_string(),
    );
    values.insert(
        "loot.economyReportProvided".to_string(),
        loot_economy_report.is_some().to_string(),
    );
    if let Some(path) = loot_economy_report {
        values.insert(
            "loot.economyReportFile".to_string(),
            normalized_path_display(Path::new(path)),
        );
    }
    values.insert(
        "worldBorder.centerX".to_string(),
        java_double_string(border.center_x),
    );
    values.insert(
        "worldBorder.centerZ".to_string(),
        java_double_string(border.center_z),
    );
    values.insert(
        "worldBorder.diameterBlocks".to_string(),
        java_double_string(border.diameter_blocks),
    );
    values.insert(
        "worldBorder.includesFullEarth".to_string(),
        border.includes_earth().to_string(),
    );
    values.insert(
        "progression.requiredInsideBorderModelAvailable".to_string(),
        border.includes_earth().to_string(),
    );
    values.insert(
        "progression.requiredInsideBorder".to_string(),
        progression_inside_border_pass.to_string(),
    );
    values.insert(
        "progression.selectedFactionStartModelPass".to_string(),
        multiple_faction_progression_pass.to_string(),
    );
    values.insert(
        "pluginStack.tested".to_string(),
        plugin_stack_pass.to_string(),
    );
    values.insert(
        "pluginStack.reportProvided".to_string(),
        plugin_stack_report.is_some().to_string(),
    );
    if let Some(path) = plugin_stack_report {
        values.insert(
            "pluginStack.reportFile".to_string(),
            normalized_path_display(Path::new(path)),
        );
    }
    values.insert(
        "chunkLoadStress.tested".to_string(),
        chunk_load_stress_pass.to_string(),
    );
    values.insert(
        "chunkLoadStress.reportProvided".to_string(),
        chunk_load_stress_report.is_some().to_string(),
    );
    if let Some(path) = chunk_load_stress_report {
        values.insert(
            "chunkLoadStress.reportFile".to_string(),
            normalized_path_display(Path::new(path)),
        );
    }
    values.insert(
        "representative.candidateRegions".to_string(),
        representative_plan.candidate_regions.to_string(),
    );
    values.insert(
        "representative.selectedForSampling".to_string(),
        representative_plan.selected.len().to_string(),
    );
    values.insert(
        "operatorLaunchReport.generated".to_string(),
        "true".to_string(),
    );
    values.insert(
        "operatorLaunchReport.file".to_string(),
        file_name_string(operator_report_path),
    );
    values.insert(
        "gate.multipleFactionRegionsHaveViableProgression".to_string(),
        multiple_faction_progression_pass.to_string(),
    );
    values.insert(
        "gate.worldBorderDoesNotExcludeRequiredProgression".to_string(),
        (progression_inside_border_pass && border.includes_earth()).to_string(),
    );
    values.insert(
        "gate.serverStressTestPass".to_string(),
        chunk_load_stress_pass.to_string(),
    );
    values.insert(
        "gate.nationWarValidationReportPass".to_string(),
        overall_pass.to_string(),
    );
    earthmap_gameplay::write_properties(path, &values, "SR EarthMap nation-war readiness report")
}

#[allow(clippy::too_many_arguments)]
fn write_nation_war_operator_report(
    path: &Path,
    report_path: &Path,
    starts_path: &Path,
    border: NationWarWorldBorderPlan,
    faction_count: i32,
    safe_zone_radius_blocks: i32,
    starts: &[NationWarFactionStart],
    spawn_policy_pass: bool,
    resource_fairness_model_pass: bool,
    global_resource_fairness_pass: bool,
    progression_inside_border_pass: bool,
    multiple_faction_progression_pass: bool,
    loot_economy_pass: bool,
    plugin_stack_pass: bool,
    chunk_load_stress_pass: bool,
    overall_pass: bool,
) -> std::result::Result<(), String> {
    let mut text = String::new();
    text.push_str("# EarthMap Nation-War Operator Launch Report\n\n");
    text.push_str("Status: ");
    text.push_str(if overall_pass { "READY" } else { "NOT_READY" });
    text.push_str("\n\n");
    text.push_str("This report is an operator-facing checklist. It is not a production approval unless status is READY.\n\n");
    text.push_str("## Files\n\n");
    text.push_str(&format!(
        "- Readiness properties: {}\n",
        file_name_string(report_path)
    ));
    text.push_str(&format!(
        "- Faction starts CSV: {}\n\n",
        file_name_string(starts_path)
    ));
    text.push_str("## World Border Commands\n\n```text\n");
    text.push_str(&format!(
        "/worldborder center {} {}\n/worldborder set {}\n",
        java_double_string(border.center_x),
        java_double_string(border.center_z),
        java_double_string(border.diameter_blocks)
    ));
    text.push_str("```\n\n## Spawn Policy\n\n");
    text.push_str(&format!("- Requested factions: {faction_count}\n"));
    text.push_str(&format!("- Selected starts: {}\n", starts.len()));
    text.push_str(&format!(
        "- Safe-zone radius blocks: {safe_zone_radius_blocks}\n"
    ));
    text.push_str(&format!("- Spawn policy pass: {spawn_policy_pass}\n\n"));
    text.push_str("## Required Gates\n\n");
    append_checklist_line(
        &mut text,
        "Selected-start deterministic resource model",
        resource_fairness_model_pass,
    );
    append_checklist_line(
        &mut text,
        "Global resource fairness report",
        global_resource_fairness_pass,
    );
    append_checklist_line(
        &mut text,
        "Progression inside border",
        progression_inside_border_pass,
    );
    append_checklist_line(
        &mut text,
        "Multiple faction progression distribution",
        multiple_faction_progression_pass,
    );
    append_checklist_line(&mut text, "Loot economy report", loot_economy_pass);
    append_checklist_line(&mut text, "Target plugin stack test", plugin_stack_pass);
    append_checklist_line(&mut text, "Chunk-load stress test", chunk_load_stress_pass);
    text.push_str("\n## Current Blockers\n\n");
    if !resource_fairness_model_pass {
        text.push_str("- Select faction starts that are land, inside the border, and covered by the deterministic resource model.\n");
    }
    if !global_resource_fairness_pass {
        text.push_str(
            "- Generate and aggregate real continent-scale/global resource fairness evidence.\n",
        );
    }
    if !progression_inside_border_pass || !multiple_faction_progression_pass {
        text.push_str(
            "- Map generated structure/progression metadata to each selected faction start.\n",
        );
    }
    if !loot_economy_pass {
        text.push_str("- Validate loot economy beyond block-entity smoke evidence.\n");
    }
    if !plugin_stack_pass {
        text.push_str("- Pin and boot the target nation-war plugin stack.\n");
    }
    if !chunk_load_stress_pass {
        text.push_str("- Run chunk-load stress under the target server stack.\n");
    }
    std::fs::write(path, text).map_err(|error| error.to_string())
}

fn append_checklist_line(text: &mut String, label: &str, pass: bool) {
    text.push_str("- [");
    text.push_str(if pass { "x" } else { " " });
    text.push_str("] ");
    text.push_str(label);
    text.push('\n');
}

fn file_name_string(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn scan_osm_pbf(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
    max_blobs_text: &str,
) -> io::Result<i32> {
    match parse_i32_string(max_blobs_text)
        .map_err(earthmap_osm::OsmError::invalid)
        .and_then(|max_blobs| earthmap_osm::scan_pbf(Path::new(path), max_blobs))
    {
        Ok(report) => {
            writeln!(out, "OSM PBF scan complete")?;
            write_osm_scan_report(out, &report)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "OSM PBF scan failed: {error}")?;
            write_osm_failure_details(err, &error)?;
            Ok(EXIT_USAGE)
        }
    }
}

fn scan_osm_pbf_range(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
    skip_blobs_text: &str,
    max_blobs_text: &str,
) -> io::Result<i32> {
    match parse_i32_string(skip_blobs_text)
        .and_then(|skip_blobs| {
            parse_i32_string(max_blobs_text).map(|max_blobs| (skip_blobs, max_blobs))
        })
        .map_err(earthmap_osm::OsmError::invalid)
        .and_then(|(skip_blobs, max_blobs)| {
            earthmap_osm::scan_pbf_range(Path::new(path), skip_blobs, max_blobs)
                .map(|report| (skip_blobs, report))
        }) {
        Ok((skip_blobs, report)) => {
            writeln!(out, "OSM PBF scan complete")?;
            writeln!(out, "skipBlobs={skip_blobs}")?;
            write_osm_scan_report(out, &report)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "OSM PBF scan failed: {error}")?;
            write_osm_failure_details(err, &error)?;
            Ok(EXIT_USAGE)
        }
    }
}

fn validate_osm_pbf(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
    max_blobs_text: &str,
) -> io::Result<i32> {
    match parse_i32_string(max_blobs_text)
        .map_err(earthmap_osm::OsmError::invalid)
        .and_then(|max_blobs| earthmap_osm::scan_pbf(Path::new(path), max_blobs))
    {
        Ok(report) => {
            writeln!(out, "OSM PBF integrity valid for scanned prefix")?;
            write_osm_scan_report(out, &report)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "OSM PBF integrity failed")?;
            write_osm_failure_details(err, &error)?;
            Ok(EXIT_USAGE)
        }
    }
}

fn benchmark_osm_index(
    out: &mut impl Write,
    err: &mut impl Write,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    way_count_text: &str,
) -> io::Result<i32> {
    match parse_i32_string(scale_text)
        .and_then(|scale| parse_i32_string(region_x_text).map(|region_x| (scale, region_x)))
        .and_then(|(scale, region_x)| {
            parse_i32_string(region_z_text).map(|region_z| (scale, region_x, region_z))
        })
        .and_then(|(scale, region_x, region_z)| {
            parse_i32_string(way_count_text).map(|way_count| {
                earthmap_osm::benchmark_osm_index(scale, region_x, region_z, way_count)
            })
        }) {
        Ok(Ok(report)) => {
            writeln!(out, "OSM region index benchmark complete")?;
            writeln!(out, "scale=1:{}", report.scale)?;
            writeln!(out, "regionX={}", report.region_x)?;
            writeln!(out, "regionZ={}", report.region_z)?;
            writeln!(out, "ways={}", report.ways)?;
            writeln!(out, "nodes={}", report.nodes)?;
            writeln!(out, "setupMillis={}", report.setup_millis)?;
            writeln!(out, "indexMillis={}", report.index_millis)?;
            writeln!(
                out,
                "waysPerSecond={}",
                java_double_string(report.ways_per_second)
            )?;
            writeln!(out, "features={}", report.features)?;
            writeln!(out, "skippedUnscopedWays={}", report.skipped_unscoped_ways)?;
            writeln!(
                out,
                "skippedMissingNodeWays={}",
                report.skipped_missing_node_ways
            )?;
            Ok(EXIT_OK)
        }
        Ok(Err(error)) => {
            writeln!(err, "OSM region index benchmark failed: {error}")?;
            Ok(EXIT_USAGE)
        }
        Err(error) => {
            writeln!(err, "OSM region index benchmark failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn extract_osm_region_mask(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    max_blobs_text: &str,
) -> io::Result<i32> {
    match osm_mapping_for_scale(scale_text)
        .and_then(|(scale, mapping)| {
            osm_region_args(region_x_text, region_z_text)
                .map(|(region_x, region_z)| (scale, mapping, region_x, region_z))
        })
        .and_then(|(_scale, mapping, region_x, region_z)| {
            parse_i32_string(max_blobs_text).map(|max_blobs| {
                earthmap_osm::extract_mask(Path::new(path), max_blobs, &mapping, region_x, region_z)
            })
        }) {
        Ok(Ok(result)) => {
            writeln!(out, "OSM region mask extracted")?;
            write_osm_extract_report(out, &result.report)?;
            Ok(EXIT_OK)
        }
        Ok(Err(error)) => {
            writeln!(err, "OSM region mask extraction failed: {error}")?;
            write_osm_failure_details(err, &error)?;
            Ok(EXIT_USAGE)
        }
        Err(error) => {
            writeln!(err, "OSM region mask extraction failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn extract_osm_region_mask_window(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    node_max_blobs_text: &str,
    way_skip_blobs_text: &str,
    way_max_blobs_text: &str,
) -> io::Result<i32> {
    match extract_osm_region_mask_window_impl(
        path,
        scale_text,
        region_x_text,
        region_z_text,
        node_max_blobs_text,
        way_skip_blobs_text,
        way_max_blobs_text,
        false,
    ) {
        Ok((node_max_blobs, way_skip_blobs, way_max_blobs, result)) => {
            writeln!(out, "OSM region mask window extracted")?;
            writeln!(out, "nodeMaxBlobs={node_max_blobs}")?;
            writeln!(out, "waySkipBlobs={way_skip_blobs}")?;
            writeln!(out, "wayMaxBlobs={way_max_blobs}")?;
            write_osm_extract_report(out, &result.report)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "OSM region mask window extraction failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn extract_osm_region_mask_ref_window(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    node_max_blobs_text: &str,
    way_skip_blobs_text: &str,
    way_max_blobs_text: &str,
) -> io::Result<i32> {
    match extract_osm_region_mask_window_impl(
        path,
        scale_text,
        region_x_text,
        region_z_text,
        node_max_blobs_text,
        way_skip_blobs_text,
        way_max_blobs_text,
        true,
    ) {
        Ok((node_max_blobs, way_skip_blobs, way_max_blobs, result)) => {
            writeln!(out, "OSM region mask ref-window extracted")?;
            writeln!(out, "nodeMaxBlobs={node_max_blobs}")?;
            writeln!(out, "waySkipBlobs={way_skip_blobs}")?;
            writeln!(out, "wayMaxBlobs={way_max_blobs}")?;
            write_osm_extract_report(out, &result.report)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "OSM region mask ref-window extraction failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn extract_osm_region_mask_window_impl(
    path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    node_max_blobs_text: &str,
    way_skip_blobs_text: &str,
    way_max_blobs_text: &str,
    ref_window: bool,
) -> std::result::Result<(i32, i32, i32, earthmap_osm::OsmPbfRegionExtractResult), String> {
    let (_scale, mapping) = osm_mapping_for_scale(scale_text)?;
    let (region_x, region_z) = osm_region_args(region_x_text, region_z_text)?;
    let node_max_blobs = parse_i32_string(node_max_blobs_text)?;
    let way_skip_blobs = parse_i32_string(way_skip_blobs_text)?;
    let way_max_blobs = parse_i32_string(way_max_blobs_text)?;
    let result = if ref_window {
        earthmap_osm::extract_way_ref_window(
            Path::new(path),
            node_max_blobs,
            way_skip_blobs,
            way_max_blobs,
            &mapping,
            region_x,
            region_z,
        )
    } else {
        earthmap_osm::extract_window(
            Path::new(path),
            node_max_blobs,
            way_skip_blobs,
            way_max_blobs,
            &mapping,
            region_x,
            region_z,
        )
    }
    .map_err(osm_error_text)?;
    Ok((node_max_blobs, way_skip_blobs, way_max_blobs, result))
}

#[allow(clippy::too_many_arguments)]
fn extract_osm_region_mask_full_scan(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    max_blobs_text: Option<&str>,
    progress_every_text: Option<&str>,
    progress_file: Option<&str>,
) -> io::Result<i32> {
    let result = (|| -> std::result::Result<_, String> {
        let (_scale, mapping) = osm_mapping_for_scale(scale_text)?;
        let (region_x, region_z) = osm_region_args(region_x_text, region_z_text)?;
        let max_blobs = max_blobs_text
            .map(parse_i32_string)
            .transpose()?
            .unwrap_or(i32::MAX);
        let progress_every = progress_every_text
            .map(parse_i32_string)
            .transpose()?
            .unwrap_or(0);
        let mut progress_writer = progress_file.map(open_progress_file).transpose()?;
        let result = earthmap_osm::extract_full_scan(
            Path::new(path),
            max_blobs,
            &mapping,
            region_x,
            region_z,
            progress_every,
            |progress| {
                let line = osm_full_scan_progress_line(progress);
                writeln!(out, "{line}").map_err(|error| {
                    earthmap_osm::OsmError::invalid(format!("failed to write progress: {error}"))
                })?;
                if let Some(writer) = progress_writer.as_mut() {
                    writeln!(writer, "{line}").map_err(|error| {
                        earthmap_osm::OsmError::invalid(format!(
                            "failed to write progress file: {error}"
                        ))
                    })?;
                }
                Ok(())
            },
        )
        .map_err(osm_error_text)?;
        if let Some(writer) = progress_writer.as_mut() {
            writer.flush().map_err(|error| error.to_string())?;
        }
        Ok((max_blobs, progress_every, result))
    })();

    match result {
        Ok((max_blobs, progress_every, result)) => {
            writeln!(out, "OSM region mask full scan extracted")?;
            writeln!(out, "maxBlobs={max_blobs}")?;
            writeln!(out, "progressEveryBlobs={progress_every}")?;
            if let Some(progress_file) = progress_file {
                writeln!(out, "progressFile={progress_file}")?;
            }
            write_osm_extract_report(out, &result.report)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "OSM region mask full-scan extraction failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn extract_osm_xml_region_mask(
    out: &mut impl Write,
    err: &mut impl Write,
    directory: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
) -> io::Result<i32> {
    match osm_mapping_for_scale(scale_text)
        .and_then(|(_scale, mapping)| {
            osm_region_args(region_x_text, region_z_text)
                .map(|(region_x, region_z)| (mapping, region_x, region_z))
        })
        .map(|(mapping, region_x, region_z)| {
            earthmap_osm::extract_xml_region_mask(
                Path::new(directory),
                &mapping,
                region_x,
                region_z,
            )
        }) {
        Ok(Ok(result)) => {
            writeln!(out, "OSM XML cache region mask extracted")?;
            writeln!(out, "osmDirectory={directory}")?;
            write_osm_extract_report(out, &result.report)?;
            Ok(EXIT_OK)
        }
        Ok(Err(error)) => {
            writeln!(err, "OSM XML cache region mask extraction failed: {error}")?;
            write_osm_failure_details(err, &error)?;
            Ok(EXIT_USAGE)
        }
        Err(error) => {
            writeln!(err, "OSM XML cache region mask extraction failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn identify_osm_xml_cache(
    out: &mut impl Write,
    err: &mut impl Write,
    directory: &str,
) -> io::Result<i32> {
    match earthmap_osm::identify_xml_cache(Path::new(directory)) {
        Ok(identity) => {
            writeln!(out, "sourceKind={}", identity.kind)?;
            writeln!(out, "sourcePath={}", identity.source_path.display())?;
            writeln!(out, "sourceFileCount={}", identity.file_count)?;
            writeln!(out, "sourceTotalBytes={}", identity.total_bytes)?;
            writeln!(out, "sourceSha256={}", identity.sha256)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "OSM XML cache identification failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn osm_mapping_for_scale(
    scale_text: &str,
) -> std::result::Result<(i32, EarthScaleMapping), String> {
    let scale = parse_i32_string(scale_text)?;
    let mapping = EarthScaleMapping::for_denominator(scale, -90.0, 90.0)
        .map_err(|error| error.to_string())?;
    Ok((scale, mapping))
}

fn osm_region_args(
    region_x_text: &str,
    region_z_text: &str,
) -> std::result::Result<(i32, i32), String> {
    Ok((
        parse_i32_string(region_x_text)?,
        parse_i32_string(region_z_text)?,
    ))
}

fn write_osm_scan_report(
    out: &mut impl Write,
    report: &earthmap_osm::OsmPbfScanReport,
) -> io::Result<()> {
    writeln!(out, "blobsScanned={}", report.blobs_scanned)?;
    writeln!(out, "osmHeaderBlobs={}", report.osm_header_blobs)?;
    writeln!(out, "osmDataBlobs={}", report.osm_data_blobs)?;
    writeln!(out, "compressedBytes={}", report.compressed_bytes)?;
    writeln!(out, "decodedBytes={}", report.decoded_bytes)?;
    writeln!(out, "primitiveGroups={}", report.primitive_groups)?;
    writeln!(out, "denseNodes={}", report.dense_nodes)?;
    writeln!(out, "ways={}", report.ways)?;
    writeln!(out, "highwayWays={}", report.highway_ways)?;
    writeln!(out, "elapsedMillis={}", report.elapsed_millis)?;
    Ok(())
}

fn write_osm_extract_report(
    out: &mut impl Write,
    report: &earthmap_osm::OsmPbfRegionExtractReport,
) -> io::Result<()> {
    writeln!(out, "blobsScanned={}", report.blobs_scanned)?;
    writeln!(out, "osmDataBlobs={}", report.osm_data_blobs)?;
    writeln!(out, "primitiveGroups={}", report.primitive_groups)?;
    writeln!(out, "decodedNodes={}", report.decoded_nodes)?;
    writeln!(out, "decodedWays={}", report.decoded_ways)?;
    writeln!(out, "indexConsideredWays={}", report.index_considered_ways)?;
    writeln!(
        out,
        "indexSkippedMissingNodeWays={}",
        report.index_skipped_missing_node_ways
    )?;
    writeln!(out, "indexedFeatures={}", report.indexed_features)?;
    writeln!(out, "roadFeatures={}", report.road_features)?;
    writeln!(out, "waterwayFeatures={}", report.waterway_features)?;
    writeln!(out, "landuseFeatures={}", report.landuse_features)?;
    writeln!(out, "buildingFeatures={}", report.building_features)?;
    writeln!(out, "roadMaskPixels={}", report.road_mask_pixels)?;
    writeln!(out, "waterwayMaskPixels={}", report.waterway_mask_pixels)?;
    writeln!(out, "landuseMaskPixels={}", report.landuse_mask_pixels)?;
    writeln!(out, "buildingMaskPixels={}", report.building_mask_pixels)?;
    writeln!(out, "elapsedMillis={}", report.elapsed_millis)?;
    Ok(())
}

fn write_osm_failure_details(
    err: &mut impl Write,
    error: &earthmap_osm::OsmError,
) -> io::Result<()> {
    writeln!(err, "failure={error}")?;
    if let Some(blob_index) = error.blob_index() {
        writeln!(err, "failureBlobIndex={blob_index}")?;
    }
    if let Some(byte_offset) = error.byte_offset() {
        writeln!(err, "failureByteOffset={byte_offset}")?;
    }
    Ok(())
}

fn osm_error_text(error: earthmap_osm::OsmError) -> String {
    if let (Some(blob_index), Some(byte_offset)) = (error.blob_index(), error.byte_offset()) {
        format!("{error} (blobIndex={blob_index}, byteOffset={byte_offset})")
    } else {
        error.to_string()
    }
}

fn open_progress_file(path: &str) -> std::result::Result<BufWriter<File>, String> {
    let path = Path::new(path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    Ok(BufWriter::new(file))
}

fn osm_full_scan_progress_line(progress: earthmap_osm::FullScanProgress) -> String {
    format!(
        "progress,blobIndex={},byteOffset={},elapsedMillis={},retainedNodes={},consideredWays={},indexedFeatures={},primitiveGroups={}",
        progress.blob_index,
        progress.byte_offset,
        progress.elapsed_millis,
        progress.retained_nodes,
        progress.considered_ways,
        progress.indexed_features,
        progress.primitive_groups
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RegionGridBounds {
    min_region_x: i32,
    max_region_x: i32,
    min_region_z: i32,
    max_region_z: i32,
}

impl RegionGridBounds {
    fn for_mapping(mapping: &EarthScaleMapping) -> Self {
        let min_global_x = -(mapping.width_blocks / 2);
        let max_global_x = mapping.width_blocks - (mapping.width_blocks / 2) - 1;
        let min_global_z = -(mapping.height_blocks / 2);
        let max_global_z = mapping.height_blocks - (mapping.height_blocks / 2) - 1;
        Self {
            min_region_x: min_global_x.div_euclid(REGION_SIZE_BLOCKS),
            max_region_x: max_global_x.div_euclid(REGION_SIZE_BLOCKS),
            min_region_z: min_global_z.div_euclid(REGION_SIZE_BLOCKS),
            max_region_z: max_global_z.div_euclid(REGION_SIZE_BLOCKS),
        }
    }

    fn columns(self) -> i32 {
        self.max_region_x - self.min_region_x + 1
    }

    fn rows(self) -> i32 {
        self.max_region_z - self.min_region_z + 1
    }

    fn region_count(self) -> i64 {
        i64::from(self.columns()) * i64::from(self.rows())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum RepresentativeRegionClass {
    Ocean,
    Coast,
    Mountain,
    Desert,
    Jungle,
    Snow,
    Land,
}

impl RepresentativeRegionClass {
    const ALL: [Self; 7] = [
        Self::Ocean,
        Self::Coast,
        Self::Mountain,
        Self::Desert,
        Self::Jungle,
        Self::Snow,
        Self::Land,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Ocean => "OCEAN",
            Self::Coast => "COAST",
            Self::Mountain => "MOUNTAIN",
            Self::Desert => "DESERT",
            Self::Jungle => "JUNGLE",
            Self::Snow => "SNOW",
            Self::Land => "LAND",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Ocean => 0,
            Self::Coast => 1,
            Self::Mountain => 2,
            Self::Desert => 3,
            Self::Jungle => 4,
            Self::Snow => 5,
            Self::Land => 6,
        }
    }
}

#[derive(Clone, Debug)]
struct RegionCandidate {
    region_x: i32,
    region_z: i32,
    region_class: RepresentativeRegionClass,
    water_ratio: f64,
    min_ground_y: i32,
    max_ground_y: i32,
    dominant_biome: String,
}

impl RegionCandidate {
    fn key(&self) -> (i32, i32) {
        (self.region_x, self.region_z)
    }
}

#[derive(Clone, Debug)]
struct RepresentativeRegionPlanReport {
    scale_denominator: i32,
    width_blocks: i32,
    height_blocks: i32,
    candidate_regions: usize,
    selected: Vec<RegionCandidate>,
}

impl RepresentativeRegionPlanReport {
    fn selected_counts_by_class(&self) -> [usize; 7] {
        let mut counts = [0usize; 7];
        for candidate in &self.selected {
            counts[candidate.region_class.index()] += 1;
        }
        counts
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SurfaceSpawnReport {
    region_x: i32,
    region_z: i32,
    land_columns: i32,
    water_columns: i32,
    best_spawn_x: i32,
    best_spawn_y: i32,
    best_spawn_z: i32,
    viable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeightSeamDirection {
    East,
    South,
}

impl HeightSeamDirection {
    fn parse(text: &str) -> std::result::Result<Self, String> {
        match text.to_ascii_lowercase().as_str() {
            "east" | "e" => Ok(Self::East),
            "south" | "s" => Ok(Self::South),
            _ => Err("direction must be east or south".to_string()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::East => "EAST",
            Self::South => "SOUTH",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HeightSeamReport {
    region_x: i32,
    region_z: i32,
    direction: HeightSeamDirection,
    compared_columns: i32,
    coordinate_failures: i32,
    min_surface_y: i32,
    max_surface_y: i32,
    max_abs_surface_delta: i32,
}

impl HeightSeamReport {
    fn passed(self) -> bool {
        self.coordinate_failures == 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VanillaFinalizationCommandReport {
    commands_file: std::path::PathBuf,
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
    window_chunks: i32,
    wait_ms: i32,
    windows: i32,
}

#[derive(Clone, Debug, PartialEq)]
struct NationWarReadinessReport {
    report_path: std::path::PathBuf,
    faction_starts_csv: std::path::PathBuf,
    operator_launch_report: std::path::PathBuf,
    scale_denominator: i32,
    faction_count_requested: i32,
    safe_zone_radius_blocks: i32,
    faction_starts: Vec<NationWarFactionStart>,
    world_border: NationWarWorldBorderPlan,
    spawn_policy_pass: bool,
    resource_fairness_model_pass: bool,
    global_resource_fairness_pass: bool,
    progression_inside_border_pass: bool,
    multiple_faction_progression_pass: bool,
    loot_economy_pass: bool,
    plugin_stack_pass: bool,
    chunk_load_stress_pass: bool,
    overall_pass: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct NationWarFactionStart {
    index: usize,
    region_x: i32,
    region_z: i32,
    global_x: i32,
    global_z: i32,
    region_class: RepresentativeRegionClass,
    water_ratio: f64,
    min_ground_y: i32,
    max_ground_y: i32,
    dominant_biome: String,
    inside_border: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct NationWarWorldBorderPlan {
    center_x: f64,
    center_z: f64,
    diameter_blocks: f64,
    earth_min_block_x: i32,
    earth_max_block_x: i32,
    earth_min_block_z: i32,
    earth_max_block_z: i32,
}

impl NationWarWorldBorderPlan {
    fn contains(self, x: f64, z: f64) -> bool {
        let radius = self.diameter_blocks / 2.0;
        x >= self.center_x - radius
            && x <= self.center_x + radius
            && z >= self.center_z - radius
            && z <= self.center_z + radius
    }

    fn includes_earth(self) -> bool {
        self.contains(
            f64::from(self.earth_min_block_x),
            f64::from(self.earth_min_block_z),
        ) && self.contains(
            f64::from(self.earth_min_block_x),
            f64::from(self.earth_max_block_z),
        ) && self.contains(
            f64::from(self.earth_max_block_x),
            f64::from(self.earth_min_block_z),
        ) && self.contains(
            f64::from(self.earth_max_block_x),
            f64::from(self.earth_max_block_z),
        )
    }
}

const REPRESENTATIVE_SAMPLE_OFFSETS: [i32; 4] = [64, 192, 320, 448];

fn classify_representative_region(
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
    region_x: i32,
    region_z: i32,
) -> std::result::Result<RegionCandidate, String> {
    let mut water_samples = 0i32;
    let mut sample_count = 0i32;
    let mut min_ground_y = i32::MAX;
    let mut max_ground_y = i32::MIN;
    let mut biomes = BTreeMap::<String, i32>::new();
    for local_z in REPRESENTATIVE_SAMPLE_OFFSETS {
        for local_x in REPRESENTATIVE_SAMPLE_OFFSETS {
            let global_block_x = region_x
                .wrapping_mul(REGION_SIZE_BLOCKS)
                .wrapping_add(local_x);
            let global_block_z = region_z
                .wrapping_mul(REGION_SIZE_BLOCKS)
                .wrapping_add(local_z);
            let map_x = global_block_x.wrapping_add(mapping.width_blocks / 2);
            let map_z = global_block_z.wrapping_add(mapping.height_blocks / 2);
            let column = classify_grid_sample(mapping, sampler, map_x, map_z)?;
            sample_count += 1;
            if column.water {
                water_samples += 1;
            }
            min_ground_y = min_ground_y.min(column.ground_surface_y);
            max_ground_y = max_ground_y.max(column.ground_surface_y);
            *biomes.entry(column.biome_id).or_default() += 1;
        }
    }
    let dominant_biome = biomes
        .into_iter()
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
        .map(|(biome, _count)| biome)
        .unwrap_or_else(|| "minecraft:plains".to_string());
    let water_ratio = f64::from(water_samples) / f64::from(sample_count);
    let region_class = representative_region_class(water_ratio, max_ground_y, &dominant_biome);
    Ok(RegionCandidate {
        region_x,
        region_z,
        region_class,
        water_ratio,
        min_ground_y,
        max_ground_y,
        dominant_biome,
    })
}

fn classify_grid_sample(
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
    map_x: i32,
    map_z: i32,
) -> std::result::Result<EarthSurfaceColumn, String> {
    if !inside_mapping(map_x, map_z, mapping) {
        return Ok(classify_surface(0.0, 0.0, 0.0));
    }
    let longitude = mapping
        .longitude_for_block_x(map_x)
        .map_err(|error| error.to_string())?;
    let latitude = mapping
        .latitude_for_block_z(map_z)
        .map_err(|error| error.to_string())?;
    let elevation = sampler
        .bilinear_meters(longitude, latitude)
        .map_err(|error| error.to_string())?;
    Ok(classify_surface(elevation, longitude, latitude))
}

fn representative_region_class(
    water_ratio: f64,
    max_ground_y: i32,
    dominant_biome: &str,
) -> RepresentativeRegionClass {
    if water_ratio >= 0.90 {
        return RepresentativeRegionClass::Ocean;
    }
    if water_ratio >= 0.10 {
        return RepresentativeRegionClass::Coast;
    }
    if max_ground_y >= 105 {
        return RepresentativeRegionClass::Mountain;
    }
    if dominant_biome.contains("desert") {
        return RepresentativeRegionClass::Desert;
    }
    if dominant_biome.contains("jungle") {
        return RepresentativeRegionClass::Jungle;
    }
    if dominant_biome.contains("snow") || max_ground_y >= 90 {
        return RepresentativeRegionClass::Snow;
    }
    RepresentativeRegionClass::Land
}

fn select_representative_regions(
    candidates: &[RegionCandidate],
    target_regions: usize,
) -> Vec<RegionCandidate> {
    let mut by_class: [Vec<RegionCandidate>; 7] = std::array::from_fn(|_| Vec::new());
    for candidate in candidates {
        by_class[candidate.region_class.index()].push(candidate.clone());
    }
    for class_candidates in &mut by_class {
        class_candidates.sort_by_key(|candidate| (candidate.region_z, candidate.region_x));
    }

    let mut selected = Vec::with_capacity(target_regions);
    let mut used = HashSet::<(i32, i32)>::new();
    for region_class in RepresentativeRegionClass::ALL {
        add_representatives_evenly(
            &mut selected,
            &mut used,
            &by_class[region_class.index()],
            representative_quota(region_class, target_regions),
        );
    }
    if selected.len() < target_regions {
        let mut all = candidates.to_vec();
        all.sort_by_key(|candidate| (candidate.region_z, candidate.region_x));
        for candidate in all {
            if selected.len() >= target_regions {
                break;
            }
            if used.insert(candidate.key()) {
                selected.push(candidate);
            }
        }
    }
    selected.sort_by_key(|candidate| {
        (
            candidate.region_class,
            candidate.region_z,
            candidate.region_x,
        )
    });
    selected.truncate(target_regions);
    selected
}

fn add_representatives_evenly(
    selected: &mut Vec<RegionCandidate>,
    used: &mut HashSet<(i32, i32)>,
    candidates: &[RegionCandidate],
    requested: usize,
) {
    if requested == 0 || candidates.is_empty() {
        return;
    }
    let limit = requested.min(candidates.len());
    for index in 0..limit {
        let candidate_index =
            ((index as f64) * (candidates.len() as f64 / limit as f64)).floor() as usize;
        let candidate = candidates[candidate_index].clone();
        if used.insert(candidate.key()) {
            selected.push(candidate);
        }
    }
}

fn representative_quota(region_class: RepresentativeRegionClass, target_regions: usize) -> usize {
    let ratio = match region_class {
        RepresentativeRegionClass::Ocean => 0.20,
        RepresentativeRegionClass::Coast => 0.20,
        RepresentativeRegionClass::Mountain => 0.15,
        RepresentativeRegionClass::Desert => 0.15,
        RepresentativeRegionClass::Jungle => 0.10,
        RepresentativeRegionClass::Snow => 0.10,
        RepresentativeRegionClass::Land => 0.10,
    };
    1usize.max(((target_regions as f64) * ratio).round() as usize)
}

fn write_representative_region_csv(
    output_csv: &Path,
    candidates: &[RegionCandidate],
) -> std::result::Result<(), String> {
    if let Some(parent) = output_csv.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
    }
    let mut csv = String::from(
        "index,regionX,regionZ,class,waterRatio,minGroundY,maxGroundY,dominantBiome\n",
    );
    for (index, candidate) in candidates.iter().enumerate() {
        csv.push_str(&format!(
            "{},{},{},{},{:.4},{},{},{}\n",
            index,
            candidate.region_x,
            candidate.region_z,
            candidate.region_class.as_str(),
            candidate.water_ratio,
            candidate.min_ground_y,
            candidate.max_ground_y,
            candidate.dominant_biome
        ));
    }
    std::fs::write(output_csv, csv).map_err(|error| error.to_string())
}

fn height_seam_boundary_pair(
    region_x: i32,
    region_z: i32,
    direction: HeightSeamDirection,
    index: i32,
) -> (i32, i32, i32, i32) {
    let base_x = region_x.wrapping_mul(REGION_SIZE_BLOCKS);
    let base_z = region_z.wrapping_mul(REGION_SIZE_BLOCKS);
    match direction {
        HeightSeamDirection::East => (
            base_x + REGION_SIZE_BLOCKS - 1,
            base_z + index,
            base_x + REGION_SIZE_BLOCKS,
            base_z + index,
        ),
        HeightSeamDirection::South => (
            base_x + index,
            base_z + REGION_SIZE_BLOCKS - 1,
            base_x + index,
            base_z + REGION_SIZE_BLOCKS,
        ),
    }
}

fn height_seam_pair_is_adjacent(
    map_a_x: i32,
    map_a_z: i32,
    map_b_x: i32,
    map_b_z: i32,
    direction: HeightSeamDirection,
) -> bool {
    match direction {
        HeightSeamDirection::East => map_b_x == map_a_x + 1 && map_b_z == map_a_z,
        HeightSeamDirection::South => map_b_x == map_a_x && map_b_z == map_a_z + 1,
    }
}

fn height_only_surface_y(
    mapping: &EarthScaleMapping,
    sampler: &HeightmapScalarSampler<'_>,
    map_x: i32,
    map_z: i32,
) -> std::result::Result<i32, String> {
    let longitude = mapping
        .longitude_for_block_x(map_x)
        .map_err(|error| error.to_string())?;
    let latitude = mapping
        .latitude_for_block_z(map_z)
        .map_err(|error| error.to_string())?;
    let elevation = sampler
        .bilinear_meters(longitude, latitude)
        .map_err(|error| error.to_string())?;
    Ok(surface_y_for_elevation_meters(elevation))
}

fn inside_mapping(map_x: i32, map_z: i32, mapping: &EarthScaleMapping) -> bool {
    map_x >= 0 && map_x < mapping.width_blocks && map_z >= 0 && map_z < mapping.height_blocks
}

fn raster_smoke(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    scale_text: &str,
    output_json: &str,
) -> io::Result<i32> {
    match raster_smoke_impl(heightmap_path, scale_text, output_json) {
        Ok(report) => {
            writeln!(out, "Raster smoke checked")?;
            writeln!(out, "heightmap={}", report.heightmap_path)?;
            writeln!(out, "scale=1:{}", report.scale)?;
            writeln!(out, "samples={}", report.sample_count)?;
            writeln!(out, "passed={}", report.passed)?;
            writeln!(out, "outputJson={}", report.output_json)?;
            if report.passed {
                Ok(EXIT_OK)
            } else {
                Ok(EXIT_USAGE)
            }
        }
        Err(error) => {
            writeln!(err, "Raster smoke failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RasterSmokeReport {
    heightmap_path: String,
    scale: i32,
    sample_count: usize,
    passed: bool,
    output_json: String,
}

#[derive(Clone, Debug)]
struct RasterSmokeCase {
    name: &'static str,
    longitude: f64,
    latitude: f64,
    expected_water: bool,
    expected_biome_contains: &'static [&'static str],
    expected_top_blocks: &'static [i32],
    min_elevation_meters: Option<f64>,
}

fn raster_smoke_impl(
    heightmap_path: &str,
    scale_text: &str,
    output_json: &str,
) -> std::result::Result<RasterSmokeReport, String> {
    let scale = parse_i32_string(scale_text)?;
    let reader = GeoTiffHeightmapReader::open(Path::new(heightmap_path))
        .map_err(|error| error.to_string())?;
    let mapping = mapping_for(reader.metadata(), scale).map_err(|error| error.to_string())?;
    let cache = GeoTiffRowCache::new(&reader, 8).map_err(|error| error.to_string())?;
    let sampler = HeightmapScalarSampler::with_row_cache(&reader, &cache);

    let mut rows = Vec::new();
    let mut passed = true;
    for case in raster_smoke_cases() {
        let map_x = mapping
            .block_x_for_longitude(case.longitude)
            .map_err(|error| error.to_string())?;
        let map_z = mapping
            .block_z_for_latitude(case.latitude)
            .map_err(|error| error.to_string())?;
        let sampled_longitude = mapping
            .longitude_for_block_x(map_x)
            .map_err(|error| error.to_string())?;
        let sampled_latitude = mapping
            .latitude_for_block_z(map_z)
            .map_err(|error| error.to_string())?;
        let elevation = sampler
            .bilinear_meters(sampled_longitude, sampled_latitude)
            .map_err(|error| error.to_string())?;
        let column = classify_surface(elevation, sampled_longitude, sampled_latitude);
        let biome_ok = case
            .expected_biome_contains
            .iter()
            .any(|needle| column.biome_id.contains(needle));
        let top_ok = case
            .expected_top_blocks
            .iter()
            .any(|&top| top == column.top_block_state_id);
        let elevation_ok = case
            .min_elevation_meters
            .map_or(true, |min| elevation >= min);
        let valid = column.water == case.expected_water
            && biome_ok
            && top_ok
            && elevation_ok
            && block_state_ids::require_valid(column.top_block_state_id).is_ok()
            && !column.biome_id.trim().is_empty();
        passed &= valid;
        rows.push(serde_json::json!({
            "name": case.name,
            "longitude": case.longitude,
            "latitude": case.latitude,
            "sampledLongitude": sampled_longitude,
            "sampledLatitude": sampled_latitude,
            "elevationMeters": elevation,
            "water": column.water,
            "groundSurfaceY": column.ground_surface_y,
            "topBlockStateId": column.top_block_state_id,
            "topBlockName": block_state_name(column.top_block_state_id),
            "biome": column.biome_id,
            "valid": valid,
            "checks": {
                "water": column.water == case.expected_water,
                "biome": biome_ok,
                "topBlock": top_ok,
                "elevation": elevation_ok,
            },
        }));
    }

    let document = serde_json::json!({
        "schema": "earthmap-rust-raster-smoke-v1",
        "heightmapPath": normalized_path_display(Path::new(heightmap_path)),
        "scaleDenominator": scale,
        "passed": passed,
        "cache": {
            "maxRows": cache.stats().max_rows,
            "residentRows": cache.stats().resident_rows,
            "hits": cache.stats().hits,
            "misses": cache.stats().misses,
            "evictions": cache.stats().evictions,
        },
        "samples": rows,
    });
    write_json(Path::new(output_json), &document)?;
    Ok(RasterSmokeReport {
        heightmap_path: normalized_path_display(Path::new(heightmap_path)),
        scale,
        sample_count: rows.len(),
        passed,
        output_json: output_json.to_string(),
    })
}

fn raster_smoke_cases() -> Vec<RasterSmokeCase> {
    vec![
        RasterSmokeCase {
            name: "atlantic-open-ocean",
            longitude: -30.0,
            latitude: 0.0,
            expected_water: true,
            expected_biome_contains: &["ocean"],
            expected_top_blocks: &[block_state_ids::GRAVEL],
            min_elevation_meters: None,
        },
        RasterSmokeCase {
            name: "sahara-desert",
            longitude: 13.0,
            latitude: 23.0,
            expected_water: false,
            expected_biome_contains: &["desert"],
            expected_top_blocks: &[block_state_ids::SAND],
            min_elevation_meters: Some(1.0),
        },
        RasterSmokeCase {
            name: "amazon-jungle",
            longitude: -60.0,
            latitude: -3.0,
            expected_water: false,
            expected_biome_contains: &["jungle"],
            expected_top_blocks: &[block_state_ids::GRASS_BLOCK],
            min_elevation_meters: Some(1.0),
        },
        RasterSmokeCase {
            name: "east-africa-savanna-highland",
            longitude: 31.0,
            latitude: -2.0,
            expected_water: false,
            expected_biome_contains: &["savanna"],
            expected_top_blocks: &[block_state_ids::GRASS_BLOCK],
            min_elevation_meters: Some(500.0),
        },
        RasterSmokeCase {
            name: "alps-mountain",
            longitude: 8.0,
            latitude: 46.0,
            expected_water: false,
            expected_biome_contains: &["forest"],
            expected_top_blocks: &[block_state_ids::STONE],
            min_elevation_meters: Some(1_000.0),
        },
        RasterSmokeCase {
            name: "everest-snow",
            longitude: 86.925,
            latitude: 27.988,
            expected_water: false,
            expected_biome_contains: &["snow"],
            expected_top_blocks: &[block_state_ids::SNOW_BLOCK],
            min_elevation_meters: Some(4_000.0),
        },
    ]
}

fn sample_vrt_rgb(
    out: &mut impl Write,
    err: &mut impl Write,
    vrt_path: &str,
    longitude_text: &str,
    latitude_text: &str,
) -> io::Result<i32> {
    match sample_vrt_rgb_impl(vrt_path, longitude_text, latitude_text) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "VRT RGB sampling failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn sample_vrt_rgb_impl(
    vrt_path: &str,
    longitude_text: &str,
    latitude_text: &str,
) -> earthmap_geo::Result<Vec<String>> {
    let longitude = parse_f64(longitude_text)?;
    let latitude = parse_f64(latitude_text)?;
    let reader = VrtRgbMosaicReader::open(Path::new(vrt_path))?;
    let color = reader.sample_nearest(longitude, latitude)?;
    let stats = reader.stats();
    Ok(vec![
        "VRT RGB sampled".to_string(),
        format!("path={}", Path::new(vrt_path).display()),
        format!("longitude={}", java_double_string(longitude)),
        format!("latitude={}", java_double_string(latitude)),
        "mode=nearest".to_string(),
        format!("available={}", color.available),
        format!("red={}", color.red),
        format!("green={}", color.green),
        format!("blue={}", color.blue),
        format!("sourceCount={}", stats.source_count),
        format!("openReaders={}", stats.open_readers),
        format!("residentTiles={}", stats.resident_tiles),
        format!("tileHits={}", stats.tile_hits),
        format!("tileMisses={}", stats.tile_misses),
        format!("tileEvictions={}", stats.tile_evictions),
        format!("sampleNearestRequests={}", stats.sample_nearest_requests),
        format!("sampleAveragedRequests={}", stats.sample_averaged_requests),
        format!("indexedSourceLookup={}", stats.indexed_source_lookup),
        format!("sourceLookupCells={}", stats.source_lookup_cells),
    ])
}

fn mapping_for(metadata: &GeoTiffMetadata, scale: i32) -> earthmap_geo::Result<EarthScaleMapping> {
    EarthScaleMapping::for_denominator(
        scale,
        metadata.top_left_latitude - (f64::from(metadata.height) * metadata.pixel_height_degrees),
        metadata.top_left_latitude,
    )
}

fn parse_i32(text: &str) -> earthmap_geo::Result<i32> {
    text.parse::<i32>()
        .map_err(|error| earthmap_geo::GeoError::invalid(error.to_string()))
}

fn parse_f64(text: &str) -> earthmap_geo::Result<f64> {
    parse_java_double(text).map_err(earthmap_geo::GeoError::invalid)
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

fn generate_height_region(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
) -> io::Result<i32> {
    match generate_height_region_impl(
        heightmap_path,
        world_dir,
        scale_text,
        region_x_text,
        region_z_text,
        format_text,
    ) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Height-only region generation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn generate_height_region_impl(
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
) -> std::result::Result<Vec<String>, String> {
    let format = OutputFormat::parse(format_text).map_err(|error| error.to_string())?;
    let scale = parse_i32_string(scale_text)?;
    let region_x = parse_i32_string(region_x_text)?;
    let region_z = parse_i32_string(region_z_text)?;
    let cache_rows = configured_heightmap_cache_rows(Path::new(heightmap_path))?;
    let settings = HeightOnlySettings::new(
        Path::new(heightmap_path),
        Path::new(world_dir),
        "SR EarthMap Height Only",
        0,
        scale,
        region_x,
        region_z,
        format,
        cache_rows,
    )
    .map_err(|error| error.to_string())?;
    let report = earthmap_surface::generate_height_only_region(&settings)
        .map_err(|error| error.to_string())?;
    Ok(vec![
        "Height-only region generated".to_string(),
        format!("regionX={}", report.region_x),
        format!("regionZ={}", report.region_z),
        format!("format={}", report.output_format.java_name()),
        format!("scale=1:{}", report.scale_denominator),
        format!("chunkCount={}", report.chunk_count),
        format!("minSurfaceY={}", report.min_surface_y),
        format!("maxSurfaceY={}", report.max_surface_y),
        format!("regionFile={}", report.region_file.display()),
        format!("cacheMaxRows={}", report.cache_stats.max_rows),
        format!("cacheResidentRows={}", report.cache_stats.resident_rows),
        format!("cacheHits={}", report.cache_stats.hits),
        format!("cacheMisses={}", report.cache_stats.misses),
        format!("cacheEvictions={}", report.cache_stats.evictions),
        format!(
            "manifestFile={}",
            Path::new(world_dir)
                .join(SURVIVAL_MANIFEST_FILE_NAME)
                .display()
        ),
    ])
}

fn generate_surface_region(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
) -> io::Result<i32> {
    match generate_surface_region_impl(
        heightmap_path,
        world_dir,
        scale_text,
        region_x_text,
        region_z_text,
        format_text,
    ) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Surface region generation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn generate_surface_region_impl(
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
) -> std::result::Result<Vec<String>, String> {
    let format = OutputFormat::parse(format_text).map_err(|error| error.to_string())?;
    let scale = parse_i32_string(scale_text)?;
    let region_x = parse_i32_string(region_x_text)?;
    let region_z = parse_i32_string(region_z_text)?;
    let cache_rows = configured_heightmap_cache_rows(Path::new(heightmap_path))?;
    let surface_tile_cache_entries = configured_surface_tile_cache_entries(None)?;
    let settings = SurfaceRegionSettings::new(
        heightmap_path,
        world_dir,
        "SR EarthMap Surface",
        0,
        scale,
        region_x,
        region_z,
        format,
        cache_rows,
    )
    .map_err(|error| error.to_string())?;
    let mut settings = settings;
    settings.surface_tile_cache_entries = surface_tile_cache_entries;
    let report =
        earthmap_surface::generate_surface_region(&settings).map_err(|error| error.to_string())?;
    Ok(surface_region_report_lines(&report, world_dir))
}

#[allow(clippy::too_many_arguments)]
fn generate_survival_region_alias(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
) -> io::Result<i32> {
    writeln!(
        out,
        "notice=legacy survival direct generation is replaced by Rust vanilla-delegated surface generation"
    )?;
    generate_vanilla_delegated_region(
        out,
        err,
        heightmap_path,
        world_dir,
        scale_text,
        region_x_text,
        region_z_text,
        format_text,
        "surface",
        "surfaceRaster=auto",
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
fn generate_survival_region_osm_pbf(
    out: &mut impl Write,
    err: &mut impl Write,
    pbf_path: &str,
    max_blobs_text: &str,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
) -> io::Result<i32> {
    let result = (|| -> std::result::Result<Vec<String>, String> {
        let (_scale, mapping) = osm_mapping_for_scale(scale_text)?;
        let (region_x, region_z) = osm_region_args(region_x_text, region_z_text)?;
        let max_blobs = parse_i32_string(max_blobs_text)?;
        let extract = earthmap_osm::extract_mask(
            Path::new(pbf_path),
            max_blobs,
            &mapping,
            region_x,
            region_z,
        )
        .map_err(osm_error_text)?;
        survival_osm_generation_lines(
            "Survival region with OSM PBF overlay generated",
            heightmap_path,
            world_dir,
            scale_text,
            region_x_text,
            region_z_text,
            format_text,
            Some(format!("pbfPath={pbf_path}")),
            Some(format!("maxBlobs={max_blobs}")),
            extract,
        )
    })();
    write_survival_osm_generation_result(out, err, "OSM PBF survival region generation", result)
}

#[allow(clippy::too_many_arguments)]
fn generate_survival_region_osm_pbf_ref_window(
    out: &mut impl Write,
    err: &mut impl Write,
    pbf_path: &str,
    node_max_blobs_text: &str,
    way_skip_blobs_text: &str,
    way_max_blobs_text: &str,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
) -> io::Result<i32> {
    let result = (|| -> std::result::Result<Vec<String>, String> {
        let (node_max_blobs, way_skip_blobs, way_max_blobs, extract) =
            extract_osm_region_mask_window_impl(
                pbf_path,
                scale_text,
                region_x_text,
                region_z_text,
                node_max_blobs_text,
                way_skip_blobs_text,
                way_max_blobs_text,
                true,
            )?;
        survival_osm_generation_lines(
            "Survival region with OSM PBF ref-window overlay generated",
            heightmap_path,
            world_dir,
            scale_text,
            region_x_text,
            region_z_text,
            format_text,
            Some(format!("nodeMaxBlobs={node_max_blobs}")),
            Some(format!(
                "waySkipBlobs={way_skip_blobs}\nwayMaxBlobs={way_max_blobs}"
            )),
            extract,
        )
    })();
    write_survival_osm_generation_result(
        out,
        err,
        "OSM PBF ref-window survival region generation",
        result,
    )
}

#[allow(clippy::too_many_arguments)]
fn generate_survival_region_osm_pbf_full_scan(
    out: &mut impl Write,
    err: &mut impl Write,
    pbf_path: &str,
    max_blobs_text: &str,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
    progress_every_text: Option<&str>,
    progress_file: Option<&str>,
) -> io::Result<i32> {
    let result = (|| -> std::result::Result<Vec<String>, String> {
        let (_scale, mapping) = osm_mapping_for_scale(scale_text)?;
        let (region_x, region_z) = osm_region_args(region_x_text, region_z_text)?;
        let max_blobs = parse_i32_string(max_blobs_text)?;
        let progress_every = progress_every_text
            .map(parse_i32_string)
            .transpose()?
            .unwrap_or(0);
        let mut progress_writer = progress_file.map(open_progress_file).transpose()?;
        let extract = earthmap_osm::extract_full_scan(
            Path::new(pbf_path),
            max_blobs,
            &mapping,
            region_x,
            region_z,
            progress_every,
            |progress| {
                let line = osm_full_scan_progress_line(progress);
                writeln!(out, "{line}").map_err(|error| {
                    earthmap_osm::OsmError::invalid(format!("failed to write progress: {error}"))
                })?;
                if let Some(writer) = progress_writer.as_mut() {
                    writeln!(writer, "{line}").map_err(|error| {
                        earthmap_osm::OsmError::invalid(format!(
                            "failed to write progress file: {error}"
                        ))
                    })?;
                }
                Ok(())
            },
        )
        .map_err(osm_error_text)?;
        if let Some(writer) = progress_writer.as_mut() {
            writer.flush().map_err(|error| error.to_string())?;
        }
        let progress_file_line = progress_file.map(|path| format!("progressFile={path}"));
        survival_osm_generation_lines(
            "Survival region with OSM PBF full-scan overlay generated",
            heightmap_path,
            world_dir,
            scale_text,
            region_x_text,
            region_z_text,
            format_text,
            Some(format!("maxBlobs={max_blobs}")),
            Some(format!(
                "progressEveryBlobs={progress_every}{}",
                progress_file_line
                    .as_ref()
                    .map(|line| format!("\n{line}"))
                    .unwrap_or_default()
            )),
            extract,
        )
    })();
    write_survival_osm_generation_result(
        out,
        err,
        "OSM PBF full-scan survival region generation",
        result,
    )
}

#[allow(clippy::too_many_arguments)]
fn generate_survival_region_osm_xml_cache(
    out: &mut impl Write,
    err: &mut impl Write,
    osm_directory: &str,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
) -> io::Result<i32> {
    let result = (|| -> std::result::Result<Vec<String>, String> {
        let (_scale, mapping) = osm_mapping_for_scale(scale_text)?;
        let (region_x, region_z) = osm_region_args(region_x_text, region_z_text)?;
        let identity =
            earthmap_osm::identify_xml_cache(Path::new(osm_directory)).map_err(osm_error_text)?;
        let extract = earthmap_osm::extract_xml_region_mask(
            Path::new(osm_directory),
            &mapping,
            region_x,
            region_z,
        )
        .map_err(osm_error_text)?;
        let mut lines = survival_osm_generation_lines(
            "Survival region with OSM XML cache overlay generated",
            heightmap_path,
            world_dir,
            scale_text,
            region_x_text,
            region_z_text,
            format_text,
            Some(format!("osmDirectory={osm_directory}")),
            None,
            extract,
        )?;
        lines.push(format!("sourceKind={}", identity.kind));
        lines.push(format!("sourcePath={}", identity.source_path.display()));
        lines.push(format!("sourceFileCount={}", identity.file_count));
        lines.push(format!("sourceTotalBytes={}", identity.total_bytes));
        lines.push(format!("sourceSha256={}", identity.sha256));
        Ok(lines)
    })();
    write_survival_osm_generation_result(
        out,
        err,
        "OSM XML cache survival region generation",
        result,
    )
}

#[allow(clippy::too_many_arguments)]
fn survival_osm_generation_lines(
    title: &str,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
    first_context_line: Option<String>,
    second_context_line: Option<String>,
    extract: earthmap_osm::OsmPbfRegionExtractResult,
) -> std::result::Result<Vec<String>, String> {
    let mut lines = Vec::new();
    lines.push(title.to_string());
    lines.push(
        "notice=legacy survival direct gameplay generation is replaced by Rust vanilla-delegated surface generation with OSM surface overlay"
            .to_string(),
    );
    if let Some(line) = first_context_line {
        lines.extend(line.lines().map(str::to_string));
    }
    if let Some(line) = second_context_line {
        lines.extend(line.lines().map(str::to_string));
    }
    lines.extend(osm_extract_report_lines(&extract.report));
    lines.push("osmSurfaceOverlay=true".to_string());
    lines.extend(generate_vanilla_delegated_region_impl_with_osm_mask(
        heightmap_path,
        world_dir,
        scale_text,
        region_x_text,
        region_z_text,
        format_text,
        "surface",
        "surfaceRaster=auto",
        &[],
        Some(surface_osm_mask_from_osm(&extract.mask)?),
    )?);
    Ok(lines)
}

fn write_survival_osm_generation_result(
    out: &mut impl Write,
    err: &mut impl Write,
    label: &str,
    result: std::result::Result<Vec<String>, String>,
) -> io::Result<i32> {
    match result {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "{label} failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn surface_osm_mask_from_osm(
    source: &earthmap_osm::OsmRegionFeatureMask,
) -> std::result::Result<SurfaceOsmRegionFeatureMask, String> {
    let mut target = SurfaceOsmRegionFeatureMask::new();
    for z in 0..REGION_SIZE_BLOCKS {
        for x in 0..REGION_SIZE_BLOCKS {
            if source.road_at(x, z).map_err(osm_error_text)? {
                target.mark(SurfaceOsmFeatureKind::Road, x, z);
            }
            if source.waterway_at(x, z).map_err(osm_error_text)? {
                target.mark(SurfaceOsmFeatureKind::Waterway, x, z);
            }
            if source.landuse_at(x, z).map_err(osm_error_text)? {
                target.mark(SurfaceOsmFeatureKind::Landuse, x, z);
            }
            if source.building_at(x, z).map_err(osm_error_text)? {
                target.mark(SurfaceOsmFeatureKind::Building, x, z);
            }
        }
    }
    Ok(target)
}

fn osm_extract_report_lines(report: &earthmap_osm::OsmPbfRegionExtractReport) -> Vec<String> {
    vec![
        format!("blobsScanned={}", report.blobs_scanned),
        format!("osmDataBlobs={}", report.osm_data_blobs),
        format!("primitiveGroups={}", report.primitive_groups),
        format!("decodedNodes={}", report.decoded_nodes),
        format!("decodedWays={}", report.decoded_ways),
        format!("indexConsideredWays={}", report.index_considered_ways),
        format!(
            "indexSkippedMissingNodeWays={}",
            report.index_skipped_missing_node_ways
        ),
        format!("indexedFeatures={}", report.indexed_features),
        format!("roadFeatures={}", report.road_features),
        format!("waterwayFeatures={}", report.waterway_features),
        format!("landuseFeatures={}", report.landuse_features),
        format!("buildingFeatures={}", report.building_features),
        format!("roadMaskPixels={}", report.road_mask_pixels),
        format!("waterwayMaskPixels={}", report.waterway_mask_pixels),
        format!("landuseMaskPixels={}", report.landuse_mask_pixels),
        format!("buildingMaskPixels={}", report.building_mask_pixels),
        format!("elapsedMillis={}", report.elapsed_millis),
    ]
}

#[allow(clippy::too_many_arguments)]
fn generate_survival_regions_parallel_alias(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
    threads_text: &str,
    max_regions_this_run: Option<&str>,
) -> io::Result<i32> {
    writeln!(
        out,
        "notice=legacy survival direct parallel generation is replaced by Rust vanilla-delegated parallel generation"
    )?;
    if let Some(max_regions_this_run) = max_regions_this_run {
        writeln!(
            out,
            "notice=maxRegionsThisRun={} is ignored by the grid alias; use generate-vanilla-delegated-plan-parallel for bounded non-contiguous runs",
            max_regions_this_run
        )?;
    }
    generate_vanilla_delegated_regions_parallel(
        out,
        err,
        heightmap_path,
        world_dir,
        scale_text,
        start_region_x_text,
        start_region_z_text,
        cols_text,
        rows_text,
        format_text,
        threads_text,
        "surface",
        "surfaceRaster=auto",
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
fn generate_survival_region_plan_parallel_alias(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    format_text: &str,
    threads_text: &str,
    plan_csv: &str,
    max_regions_this_run: Option<&str>,
) -> io::Result<i32> {
    writeln!(
        out,
        "notice=legacy survival plan generation is replaced by Rust vanilla-delegated plan generation"
    )?;
    let optional_args = max_regions_this_run
        .map(|value| vec![value.to_string()])
        .unwrap_or_default();
    generate_vanilla_delegated_plan_parallel(
        out,
        err,
        heightmap_path,
        world_dir,
        scale_text,
        format_text,
        threads_text,
        plan_csv,
        &optional_args,
    )
}

#[allow(clippy::too_many_arguments)]
fn generate_quality_candidate(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
    surface_raster_text: &str,
    sample_grid_text: &str,
) -> io::Result<i32> {
    match generate_quality_candidate_impl(
        heightmap_path,
        world_dir,
        scale_text,
        region_x_text,
        region_z_text,
        format_text,
        surface_raster_text,
        sample_grid_text,
    ) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Quality candidate generation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_quality_candidate_impl(
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
    surface_raster_text: &str,
    sample_grid_text: &str,
) -> std::result::Result<Vec<String>, String> {
    let format = OutputFormat::parse(format_text).map_err(|error| error.to_string())?;
    let scale = parse_i32_string(scale_text)?;
    let region_x = parse_i32_string(region_x_text)?;
    let region_z = parse_i32_string(region_z_text)?;
    let sample_grid = parse_quality_sample_grid(sample_grid_text)?;
    let heightmap = Path::new(heightmap_path);
    let surface_material_path =
        parse_optional_surface_material_path(surface_raster_text, heightmap)?;
    let cache_rows = configured_heightmap_cache_rows(heightmap)?;
    let surface_tile_cache_entries =
        configured_surface_tile_cache_entries(surface_material_path.as_deref())?;
    let world_dir = Path::new(world_dir);
    let evidence_dir = world_dir.join("rust-quality-evidence");
    std::fs::create_dir_all(&evidence_dir).map_err(|error| error.to_string())?;

    let mut settings = SurfaceRegionSettings::new(
        heightmap_path,
        world_dir,
        "SR EarthMap Rust Quality Candidate",
        0,
        scale,
        region_x,
        region_z,
        format,
        cache_rows,
    )
    .map_err(|error| error.to_string())?;
    settings.surface_material_path = surface_material_path.clone();
    settings.surface_tile_cache_entries = surface_tile_cache_entries;

    let outer_start = Instant::now();
    let report =
        earthmap_surface::generate_surface_region(&settings).map_err(|error| error.to_string())?;
    let generation_wall_millis = outer_start.elapsed().as_millis();

    let payload_manifest = evidence_dir.join("region-payload-manifest.csv");
    earthmap_parity::write_region_payload_manifest(&report.region_file, &payload_manifest)
        .map_err(|error| error.to_string())?;
    let payload_manifest_sha256 =
        earthmap_parity::sha256_file_hex(&payload_manifest).map_err(|error| error.to_string())?;
    let region_file_sha256 =
        earthmap_parity::sha256_file_hex(&report.region_file).map_err(|error| error.to_string())?;
    let payloads = read_region_payloads(&report.region_file).map_err(|error| error.to_string())?;

    let local_columns = quality_preview_columns(sample_grid);
    let traces = earthmap_surface::trace_surface_region_columns(&settings, &local_columns)
        .map_err(|error| error.to_string())?;
    let stats = QualityColumnStats::from_traces(&traces);
    let preview_image = evidence_dir.join("preview.png");
    let stats_json = evidence_dir.join("quality-stats.json");
    let top_block_csv = evidence_dir.join("top-block-distribution.csv");
    let biome_csv = evidence_dir.join("biome-distribution.csv");
    write_quality_preview_png(&preview_image, sample_grid, &traces)?;
    write_quality_stats_json(&stats_json, sample_grid, &stats)?;
    write_top_block_distribution_csv(&top_block_csv, &stats)?;
    write_biome_distribution_csv(&biome_csv, &stats)?;

    let rerun_dir = evidence_dir.join("determinism-rerun");
    let mut rerun_settings = settings.clone();
    rerun_settings.world_dir = rerun_dir.clone();
    let rerun_report = earthmap_surface::generate_surface_region(&rerun_settings)
        .map_err(|error| error.to_string())?;
    let rerun_payload_manifest = evidence_dir.join("region-payload-manifest-rerun.csv");
    earthmap_parity::write_region_payload_manifest(
        &rerun_report.region_file,
        &rerun_payload_manifest,
    )
    .map_err(|error| error.to_string())?;
    let determinism_report = earthmap_parity::compare_region_payload_manifest(
        &payload_manifest,
        &rerun_report.region_file,
    )
    .map_err(|error| error.to_string())?;
    let deterministic = determinism_report.matches();
    let rerun_region_file_sha256 = earthmap_parity::sha256_file_hex(&rerun_report.region_file)
        .map_err(|error| error.to_string())?;
    let rerun_payload_manifest_sha256 = earthmap_parity::sha256_file_hex(&rerun_payload_manifest)
        .map_err(|error| error.to_string())?;

    let evidence_manifest = evidence_dir.join("quality-candidate.json");
    write_quality_candidate_manifest(
        &evidence_manifest,
        heightmap,
        world_dir,
        surface_material_path.as_deref(),
        sample_grid,
        generation_wall_millis,
        &report,
        &payload_manifest,
        &payload_manifest_sha256,
        &region_file_sha256,
        payloads.chunks.len(),
        &preview_image,
        &stats_json,
        &top_block_csv,
        &biome_csv,
        &stats,
        surface_tile_cache_entries,
        &rerun_report,
        &rerun_payload_manifest,
        &rerun_payload_manifest_sha256,
        &rerun_region_file_sha256,
        &determinism_report,
    )?;
    if !deterministic {
        return Err(format!(
            "Rust self-determinism failed; firstMismatch={}",
            determinism_report
                .first_mismatch
                .map(|pos| format!("{},{}", pos.x, pos.z))
                .unwrap_or_else(|| "NONE".to_string())
        ));
    }

    Ok(vec![
        "Quality candidate generated".to_string(),
        format!("worldDir={}", world_dir.display()),
        format!("regionFile={}", report.region_file.display()),
        format!("format={}", report.output_format.java_name()),
        format!("scale=1:{}", report.scale_denominator),
        format!("regionX={}", report.region_x),
        format!("regionZ={}", report.region_z),
        format!("sampleGrid={sample_grid}"),
        format!("sampledColumns={}", stats.sample_count),
        format!("landSamples={}", stats.land_samples),
        format!("waterSamples={}", stats.water_samples),
        format!("heightmapCacheRows={}", report.cache_stats.max_rows),
        format!("surfaceTileCacheEntries={surface_tile_cache_entries}"),
        format!("payloadChunks={}", payloads.chunks.len()),
        format!("regionFileSha256={region_file_sha256}"),
        format!("payloadManifest={}", payload_manifest.display()),
        format!("payloadManifestSha256={payload_manifest_sha256}"),
        format!("previewImage={}", preview_image.display()),
        format!("statsJson={}", stats_json.display()),
        format!("topBlockDistributionCsv={}", top_block_csv.display()),
        format!("biomeDistributionCsv={}", biome_csv.display()),
        format!("deterministic={deterministic}"),
        format!(
            "determinismComparedChunks={}",
            determinism_report.compared_chunks
        ),
        format!(
            "determinismMatchingChunks={}",
            determinism_report.matching_chunks
        ),
        format!("phase.totalWallMillis={generation_wall_millis}"),
        format!("phase.totalInternalMillis={}", millis(report.total_nanos)),
        format!("evidenceManifest={}", evidence_manifest.display()),
    ])
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QualityColumnStats {
    sample_count: usize,
    land_samples: usize,
    water_samples: usize,
    invalid_top_block_samples: usize,
    invalid_biome_samples: usize,
    top_block_counts: BTreeMap<i32, usize>,
    biome_counts: BTreeMap<String, usize>,
    biome_family_counts: BTreeMap<String, usize>,
    min_ground_y: i32,
    max_ground_y: i32,
}

impl QualityColumnStats {
    fn from_traces(traces: &[SurfaceRegionColumnTrace]) -> Self {
        let mut stats = Self {
            sample_count: traces.len(),
            land_samples: 0,
            water_samples: 0,
            invalid_top_block_samples: 0,
            invalid_biome_samples: 0,
            top_block_counts: BTreeMap::new(),
            biome_counts: BTreeMap::new(),
            biome_family_counts: BTreeMap::new(),
            min_ground_y: i32::MAX,
            max_ground_y: i32::MIN,
        };
        for trace in traces {
            let column = &trace.final_column;
            if column.water {
                stats.water_samples += 1;
            } else {
                stats.land_samples += 1;
            }
            if block_state_ids::require_valid(column.top_block_state_id).is_err() {
                stats.invalid_top_block_samples += 1;
            }
            if column.biome_id.trim().is_empty() {
                stats.invalid_biome_samples += 1;
            }
            *stats
                .top_block_counts
                .entry(column.top_block_state_id)
                .or_insert(0) += 1;
            *stats
                .biome_counts
                .entry(column.biome_id.clone())
                .or_insert(0) += 1;
            *stats
                .biome_family_counts
                .entry(quality_biome_family(&column.biome_id).to_string())
                .or_insert(0) += 1;
            stats.min_ground_y = stats.min_ground_y.min(column.ground_surface_y);
            stats.max_ground_y = stats.max_ground_y.max(column.ground_surface_y);
        }
        if traces.is_empty() {
            stats.min_ground_y = 0;
            stats.max_ground_y = 0;
        }
        stats
    }
}

fn parse_quality_sample_grid(text: &str) -> std::result::Result<usize, String> {
    let raw = text
        .split_once('=')
        .map(|(_, value)| value)
        .unwrap_or(text)
        .trim();
    let grid = raw.parse::<usize>().map_err(|error| error.to_string())?;
    if !(QUALITY_PREVIEW_GRID_MIN..=QUALITY_PREVIEW_GRID_MAX).contains(&grid) {
        return Err(format!(
            "sampleGrid must be between {QUALITY_PREVIEW_GRID_MIN} and {QUALITY_PREVIEW_GRID_MAX}: {grid}"
        ));
    }
    Ok(grid)
}

fn quality_preview_columns(grid: usize) -> Vec<(usize, usize)> {
    let mut columns = Vec::with_capacity(grid * grid);
    for row in 0..grid {
        for col in 0..grid {
            let local_x = (((col * 2 + 1) * QUALITY_PREVIEW_REGION_WIDTH) / (2 * grid))
                .min(QUALITY_PREVIEW_REGION_WIDTH - 1);
            let local_z = (((row * 2 + 1) * QUALITY_PREVIEW_REGION_WIDTH) / (2 * grid))
                .min(QUALITY_PREVIEW_REGION_WIDTH - 1);
            columns.push((local_x, local_z));
        }
    }
    columns
}

fn write_quality_preview_png(
    path: &Path,
    grid: usize,
    traces: &[SurfaceRegionColumnTrace],
) -> std::result::Result<(), String> {
    if traces.len() != grid * grid {
        return Err(format!(
            "preview trace count {} does not match sampleGrid {}",
            traces.len(),
            grid
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = std::fs::File::create(path).map_err(|error| error.to_string())?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, grid as u32, grid as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png = encoder.write_header().map_err(|error| error.to_string())?;
    let mut pixels = Vec::with_capacity(grid * grid * 3);
    for trace in traces {
        let (red, green, blue) = quality_preview_color(&trace.final_column, trace.coast_factor);
        pixels.push(red);
        pixels.push(green);
        pixels.push(blue);
    }
    png.write_image_data(&pixels)
        .map_err(|error| error.to_string())
}

fn quality_preview_color(column: &EarthSurfaceColumn, coast_factor: f64) -> (u8, u8, u8) {
    if column.water {
        if coast_factor >= 0.86 {
            return (74, 128, 155);
        }
        return (36, 78, 132);
    }
    earthmap_surface::render_surface_rgb(column.top_block_state_id, Some(&column.biome_id))
}

fn write_quality_stats_json(
    path: &Path,
    grid: usize,
    stats: &QualityColumnStats,
) -> std::result::Result<(), String> {
    let top_blocks = stats
        .top_block_counts
        .iter()
        .map(|(&block, &count)| {
            serde_json::json!({
                "topBlockStateId": block,
                "topBlockName": block_state_name(block),
                "count": count,
                "share": quality_share(count, stats.sample_count),
            })
        })
        .collect::<Vec<_>>();
    let biomes = stats
        .biome_counts
        .iter()
        .map(|(biome, &count)| {
            serde_json::json!({
                "biome": biome,
                "family": quality_biome_family(biome),
                "count": count,
                "share": quality_share(count, stats.sample_count),
            })
        })
        .collect::<Vec<_>>();
    let families = stats
        .biome_family_counts
        .iter()
        .map(|(family, &count)| {
            serde_json::json!({
                "family": family,
                "count": count,
                "share": quality_share(count, stats.sample_count),
            })
        })
        .collect::<Vec<_>>();
    let document = serde_json::json!({
        "schema": "earthmap-rust-quality-stats-v1",
        "sampleGrid": grid,
        "sampleCount": stats.sample_count,
        "landSamples": stats.land_samples,
        "waterSamples": stats.water_samples,
        "invalidTopBlockSamples": stats.invalid_top_block_samples,
        "invalidBiomeSamples": stats.invalid_biome_samples,
        "minGroundY": stats.min_ground_y,
        "maxGroundY": stats.max_ground_y,
        "topBlocks": top_blocks,
        "biomes": biomes,
        "biomeFamilies": families,
    });
    write_json(path, &document)
}

fn write_top_block_distribution_csv(
    path: &Path,
    stats: &QualityColumnStats,
) -> std::result::Result<(), String> {
    let mut text = String::from("topBlockStateId,topBlockName,count,share\n");
    for (&block, &count) in &stats.top_block_counts {
        text.push_str(&format!(
            "{},{},{},{}\n",
            block,
            block_state_name(block),
            count,
            quality_share(count, stats.sample_count)
        ));
    }
    write_text(path, &text)
}

fn write_biome_distribution_csv(
    path: &Path,
    stats: &QualityColumnStats,
) -> std::result::Result<(), String> {
    let mut text = String::from("biome,family,count,share\n");
    for (biome, &count) in &stats.biome_counts {
        text.push_str(&format!(
            "{},{},{},{}\n",
            biome,
            quality_biome_family(biome),
            count,
            quality_share(count, stats.sample_count)
        ));
    }
    write_text(path, &text)
}

fn playability_smoke(
    out: &mut impl Write,
    err: &mut impl Write,
    world_dir: &str,
    output_json: &str,
) -> io::Result<i32> {
    match playability_smoke_impl(world_dir, output_json) {
        Ok(report) => {
            writeln!(out, "Playability smoke checked")?;
            writeln!(out, "worldDir={}", report.world_dir)?;
            writeln!(out, "levelDatReadable={}", report.level_dat_readable)?;
            writeln!(out, "regionFiles={}", report.region_files)?;
            writeln!(out, "payloadChunks={}", report.payload_chunks)?;
            writeln!(out, "decodedChunks={}", report.decoded_chunks)?;
            writeln!(out, "invalidChunks={}", report.invalid_chunks)?;
            writeln!(
                out,
                "vanillaOwnedFeaturesPregenerated={}",
                report.vanilla_owned_features_pregenerated
            )?;
            writeln!(out, "passed={}", report.passed)?;
            writeln!(out, "outputJson={output_json}")?;
            Ok(if report.passed { EXIT_OK } else { EXIT_USAGE })
        }
        Err(error) => {
            writeln!(err, "Playability smoke failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlayabilitySmokeReport {
    world_dir: String,
    level_dat_readable: bool,
    region_files: usize,
    payload_chunks: usize,
    decoded_chunks: usize,
    invalid_chunks: usize,
    vanilla_owned_features_pregenerated: bool,
    passed: bool,
}

fn playability_smoke_impl(
    world_dir: &str,
    output_json: &str,
) -> std::result::Result<PlayabilitySmokeReport, String> {
    let world = Path::new(world_dir);
    let level_dat_path = world.join("level.dat");
    let level_dat_readable = nbt::read_gzip(&level_dat_path)
        .map(|named| matches!(named.tag(), Tag::Compound(_)))
        .unwrap_or(false);

    let region_dir = world.join("region");
    let mut region_files = Vec::new();
    if region_dir.is_dir() {
        for entry in std::fs::read_dir(&region_dir).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            let supported = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("mca")
                        || extension.eq_ignore_ascii_case("linear")
                });
            if supported {
                region_files.push(path);
            }
        }
    }
    region_files.sort();

    let mut status_counts = BTreeMap::<String, usize>::new();
    let mut payload_chunks = 0usize;
    let mut decoded_chunks = 0usize;
    let mut invalid_chunks = 0usize;
    let mut block_entity_count = 0usize;
    let mut entity_count = 0usize;
    let mut nonempty_structure_chunks = 0usize;
    let mut ore_palette_entries = 0usize;
    let mut vegetation_palette_entries = 0usize;

    for region_file in &region_files {
        let payloads = read_region_payloads(region_file).map_err(|error| error.to_string())?;
        payload_chunks += payloads.chunks.len();
        for payload in payloads.chunks.values() {
            match inspect_playability_chunk(payload) {
                Ok(chunk) => {
                    decoded_chunks += 1;
                    *status_counts.entry(chunk.status).or_insert(0) += 1;
                    block_entity_count += chunk.block_entities;
                    entity_count += chunk.entities;
                    nonempty_structure_chunks += usize::from(chunk.nonempty_structures);
                    ore_palette_entries += chunk.ore_palette_entries;
                    vegetation_palette_entries += chunk.vegetation_palette_entries;
                }
                Err(_) => invalid_chunks += 1,
            }
        }
    }

    let vanilla_owned_features_pregenerated = block_entity_count > 0
        || entity_count > 0
        || nonempty_structure_chunks > 0
        || ore_palette_entries > 0
        || vegetation_palette_entries > 0;
    let passed = level_dat_readable
        && !region_files.is_empty()
        && payload_chunks > 0
        && decoded_chunks == payload_chunks
        && invalid_chunks == 0
        && !vanilla_owned_features_pregenerated;

    let document = serde_json::json!({
        "schema": "earthmap-rust-playability-smoke-v1",
        "worldDir": normalized_path_display(world),
        "levelDat": {
            "path": normalized_path_display(&level_dat_path),
            "readable": level_dat_readable,
        },
        "regions": {
            "regionFiles": region_files.iter().map(|path| normalized_path_display(path)).collect::<Vec<_>>(),
            "regionFileCount": region_files.len(),
            "payloadChunks": payload_chunks,
            "decodedChunks": decoded_chunks,
            "invalidChunks": invalid_chunks,
        },
        "chunkStatusCounts": status_counts,
        "vanillaOwnedGameplay": {
            "delegated": true,
            "pregenerated": vanilla_owned_features_pregenerated,
            "blockEntityCount": block_entity_count,
            "entityCount": entity_count,
            "nonemptyStructureChunks": nonempty_structure_chunks,
            "orePaletteEntries": ore_palette_entries,
            "vegetationPaletteEntries": vegetation_palette_entries,
            "directCaves": false,
            "directOres": ore_palette_entries > 0,
            "directVegetation": vegetation_palette_entries > 0,
            "directStructures": nonempty_structure_chunks > 0,
        },
        "minecraftLoadabilityEvidence": {
            "regionReaderAcceptedPayloads": decoded_chunks == payload_chunks && invalid_chunks == 0,
            "levelDatReadable": level_dat_readable,
            "vanillaCanContinueAdjacentGeneration": true,
        },
        "passed": passed,
    });
    write_json(Path::new(output_json), &document)?;

    Ok(PlayabilitySmokeReport {
        world_dir: normalized_path_display(world),
        level_dat_readable,
        region_files: region_files.len(),
        payload_chunks,
        decoded_chunks,
        invalid_chunks,
        vanilla_owned_features_pregenerated,
        passed,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlayabilityChunkInspection {
    status: String,
    block_entities: usize,
    entities: usize,
    nonempty_structures: bool,
    ore_palette_entries: usize,
    vegetation_palette_entries: usize,
}

fn inspect_playability_chunk(
    payload: &[u8],
) -> std::result::Result<PlayabilityChunkInspection, String> {
    let named = nbt::read_from_bytes(payload).map_err(|error| error.to_string())?;
    let Tag::Compound(root) = named.tag() else {
        return Err("chunk root must be a compound".to_string());
    };
    let status = root
        .get_string("Status")
        .map_err(|error| error.to_string())?
        .to_string();
    ChunkGenerationStatus::parse(&status).map_err(|error| error.to_string())?;
    let sections = root
        .get_list("sections")
        .map_err(|error| error.to_string())?;
    if sections.values().is_empty() {
        return Err("chunk has no sections".to_string());
    }

    let mut ore_palette_entries = 0usize;
    let mut vegetation_palette_entries = 0usize;
    for section_tag in sections.values() {
        let Tag::Compound(section) = section_tag else {
            return Err("section entry must be a compound".to_string());
        };
        let block_states = section
            .get_compound("block_states")
            .map_err(|error| error.to_string())?;
        for name in block_state_palette_names(block_states)? {
            if name.contains("_ore") {
                ore_palette_entries += 1;
            }
            if is_vanilla_vegetation_palette_name(&name) {
                vegetation_palette_entries += 1;
            }
        }
    }

    let block_entities = optional_list_len(root, "block_entities")?;
    let entities = optional_list_len(root, "entities")?;
    let nonempty_structures = root
        .get_compound("structures")
        .map(has_nonempty_structure_containers)
        .unwrap_or(true);

    Ok(PlayabilityChunkInspection {
        status,
        block_entities,
        entities,
        nonempty_structures,
        ore_palette_entries,
        vegetation_palette_entries,
    })
}

fn optional_list_len(
    root: &earthmap_minecraft::nbt::Compound,
    name: &str,
) -> std::result::Result<usize, String> {
    match root.get(name) {
        Ok(Tag::List(list)) => Ok(list.values().len()),
        Ok(tag) => Err(format!(
            "NBT tag {name} must be a list, found type {}",
            tag.type_id()
        )),
        Err(_) => Ok(0),
    }
}

fn has_nonempty_structure_containers(structures: &earthmap_minecraft::nbt::Compound) -> bool {
    for name in ["starts", "References"] {
        match structures.get_compound(name) {
            Ok(compound) if compound.entries().is_empty() => {}
            Ok(_) | Err(_) => return true,
        }
    }
    false
}

fn is_vanilla_vegetation_palette_name(name: &str) -> bool {
    name.contains("_log")
        || name.contains("_leaves")
        || name.contains("_sapling")
        || name.ends_with(":grass")
        || name.contains("fern")
        || name.contains("flower")
        || name.contains("mushroom")
}

#[allow(clippy::too_many_arguments)]
fn write_quality_candidate_manifest(
    path: &Path,
    heightmap: &Path,
    world_dir: &Path,
    surface_material_path: Option<&Path>,
    sample_grid: usize,
    generation_wall_millis: u128,
    report: &SurfaceRegionReport,
    payload_manifest: &Path,
    payload_manifest_sha256: &str,
    region_file_sha256: &str,
    payload_chunk_count: usize,
    preview_image: &Path,
    stats_json: &Path,
    top_block_csv: &Path,
    biome_csv: &Path,
    stats: &QualityColumnStats,
    surface_tile_cache_entries: usize,
    rerun_report: &SurfaceRegionReport,
    rerun_payload_manifest: &Path,
    rerun_payload_manifest_sha256: &str,
    rerun_region_file_sha256: &str,
    determinism_report: &earthmap_parity::RegionManifestReport,
) -> std::result::Result<(), String> {
    let first_mismatch = determinism_report
        .first_mismatch
        .map(|pos| format!("{},{}", pos.x, pos.z));
    let document = serde_json::json!({
        "schema": "earthmap-rust-quality-candidate-v1",
        "command": "quality-candidate",
        "objective": "visual-statistical-rust-native-earth-world-evidence",
        "inputs": {
            "heightmapPath": normalized_path_display(heightmap),
            "surfaceMaterialPath": surface_material_path
                .map(normalized_path_display)
                .unwrap_or_else(|| "none".to_string()),
        },
        "world": {
            "worldDir": world_dir.display().to_string(),
            "regionFile": report.region_file.display().to_string(),
            "format": report.output_format.java_name(),
            "scaleDenominator": report.scale_denominator,
            "regionX": report.region_x,
            "regionZ": report.region_z,
            "chunkCount": report.chunk_count,
            "minecraftLoadabilityEvidence": "region reader accepted all generated payloads",
            "vanillaOwnedGameplayDelegated": true,
            "directCaves": false,
            "directOres": false,
            "directVegetation": false,
            "directStructures": false,
            "directProgressionStructures": false,
        },
        "generation": {
            "landColumns": report.land_columns,
            "waterColumns": report.water_columns,
            "minGroundY": report.min_ground_y,
            "maxGroundY": report.max_ground_y,
            "cacheMaxRows": report.cache_stats.max_rows,
            "cacheResidentRows": report.cache_stats.resident_rows,
            "cacheHits": report.cache_stats.hits,
            "cacheMisses": report.cache_stats.misses,
            "cacheEvictions": report.cache_stats.evictions,
            "surfaceTileCacheEntries": surface_tile_cache_entries,
            "surfaceMaterialRaster": {
                "sourceCount": report.surface_material_raster_stats.source_count,
                "openReaders": report.surface_material_raster_stats.open_readers,
                "residentTiles": report.surface_material_raster_stats.resident_tiles,
                "tileHits": report.surface_material_raster_stats.tile_hits,
                "tileMisses": report.surface_material_raster_stats.tile_misses,
                "tileEvictions": report.surface_material_raster_stats.tile_evictions,
                "sampleNearestRequests": report.surface_material_raster_stats.sample_nearest_requests,
                "sampleAveragedRequests": report.surface_material_raster_stats.sample_averaged_requests,
            },
            "wallMillis": generation_wall_millis,
            "phaseSurfaceSampleMillis": millis(report.surface_sample_nanos),
            "phaseChunkBuildMillis": millis(report.chunk_build_nanos),
            "phaseNbtEncodeMillis": millis(report.nbt_encode_nanos),
            "phaseRegionWriteMillis": millis(report.region_write_nanos),
            "phaseMetadataMillis": millis(report.metadata_nanos),
            "phaseTotalInternalMillis": millis(report.total_nanos),
        },
        "quality": {
            "sampleGrid": sample_grid,
            "sampleCount": stats.sample_count,
            "landSamples": stats.land_samples,
            "waterSamples": stats.water_samples,
            "invalidTopBlockSamples": stats.invalid_top_block_samples,
            "invalidBiomeSamples": stats.invalid_biome_samples,
            "minGroundY": stats.min_ground_y,
            "maxGroundY": stats.max_ground_y,
        },
        "payload": {
            "chunkCount": payload_chunk_count,
            "regionFileSha256": region_file_sha256,
            "payloadManifestSha256": payload_manifest_sha256,
        },
        "determinism": {
            "sameCommand": true,
            "sameInputs": true,
            "sameSeedConfig": true,
            "repeatedPayloadManifestMatch": determinism_report.matches(),
            "comparedChunks": determinism_report.compared_chunks,
            "matchingChunks": determinism_report.matching_chunks,
            "missingChunks": determinism_report.missing_chunks,
            "extraChunks": determinism_report.extra_chunks,
            "mismatchedChunks": determinism_report.mismatched_chunks,
            "firstMismatch": first_mismatch,
            "rerunRegionFile": rerun_report.region_file.display().to_string(),
            "rerunRegionFileSha256": rerun_region_file_sha256,
            "rerunPayloadManifestSha256": rerun_payload_manifest_sha256,
        },
        "runtime": {
            "buildName": build_info::NAME,
            "buildVersion": build_info::VERSION,
            "minecraftTarget": build_info::MINECRAFT_TARGET,
            "rustPortPhase": build_info::RUST_PORT_PHASE,
            "rustcVersion": rustc_version(),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cpuModel": cpu_model(),
            "totalPhysicalMemoryBytes": total_physical_memory_bytes(),
            "peakWorkingSetBytes": peak_working_set_bytes(),
            "availableParallelism": std::thread::available_parallelism().map(|value| value.get()).unwrap_or(0),
            "rayonNumThreadsEnv": std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "unset".to_string()),
            "heightmapCacheRowsEnv": std::env::var(HEIGHTMAP_CACHE_ROWS_ENV).unwrap_or_else(|_| "unset".to_string()),
            "surfaceTileCacheEntriesEnv": std::env::var(SURFACE_TILE_CACHE_ENTRIES_ENV).unwrap_or_else(|_| "unset".to_string()),
        },
        "artifacts": {
            "evidenceManifest": path.display().to_string(),
            "previewImage": preview_image.display().to_string(),
            "statsJson": stats_json.display().to_string(),
            "topBlockDistributionCsv": top_block_csv.display().to_string(),
            "biomeDistributionCsv": biome_csv.display().to_string(),
            "payloadManifest": payload_manifest.display().to_string(),
            "rerunPayloadManifest": rerun_payload_manifest.display().to_string(),
        },
    });
    write_json(path, &document)
}

fn write_json(path: &Path, value: &serde_json::Value) -> std::result::Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    write_text(path, &(text + "\n"))
}

fn write_text(path: &Path, text: &str) -> std::result::Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(path, text).map_err(|error| error.to_string())
}

fn quality_share(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

fn quality_biome_family(biome: &str) -> &str {
    let lower = biome.to_ascii_lowercase();
    if lower.contains("ocean") || lower.contains("river") {
        "water"
    } else if lower.contains("desert") {
        "desert"
    } else if lower.contains("badlands") {
        "badlands"
    } else if lower.contains("savanna") {
        "savanna"
    } else if lower.contains("jungle") {
        "jungle"
    } else if lower.contains("forest") || lower.contains("taiga") {
        "forest"
    } else if lower.contains("swamp") || lower.contains("mangrove") || lower.contains("wetland") {
        "wetland"
    } else if lower.contains("snow") || lower.contains("frozen") || lower.contains("ice") {
        "snow"
    } else if lower.contains("beach") {
        "coast"
    } else if lower.contains("peak")
        || lower.contains("slope")
        || lower.contains("stony")
        || lower.contains("windswept")
    {
        "mountain"
    } else if lower.contains("plains") || lower.contains("meadow") {
        "grassland"
    } else {
        "other"
    }
}

fn block_state_name(block: i32) -> &'static str {
    match block {
        block_state_ids::AIR => "minecraft:air",
        block_state_ids::STONE => "minecraft:stone",
        block_state_ids::WATER => "minecraft:water",
        block_state_ids::DIRT => "minecraft:dirt",
        block_state_ids::GRASS_BLOCK => "minecraft:grass_block",
        block_state_ids::BEDROCK => "minecraft:bedrock",
        block_state_ids::SAND => "minecraft:sand",
        block_state_ids::SNOW_BLOCK => "minecraft:snow_block",
        block_state_ids::ICE => "minecraft:ice",
        block_state_ids::DEEPSLATE => "minecraft:deepslate",
        block_state_ids::COAL_ORE => "minecraft:coal_ore",
        block_state_ids::IRON_ORE => "minecraft:iron_ore",
        block_state_ids::COPPER_ORE => "minecraft:copper_ore",
        block_state_ids::GOLD_ORE => "minecraft:gold_ore",
        block_state_ids::REDSTONE_ORE => "minecraft:redstone_ore",
        block_state_ids::LAPIS_ORE => "minecraft:lapis_ore",
        block_state_ids::DIAMOND_ORE => "minecraft:diamond_ore",
        block_state_ids::EMERALD_ORE => "minecraft:emerald_ore",
        block_state_ids::DEEPSLATE_COAL_ORE => "minecraft:deepslate_coal_ore",
        block_state_ids::DEEPSLATE_IRON_ORE => "minecraft:deepslate_iron_ore",
        block_state_ids::DEEPSLATE_COPPER_ORE => "minecraft:deepslate_copper_ore",
        block_state_ids::DEEPSLATE_GOLD_ORE => "minecraft:deepslate_gold_ore",
        block_state_ids::DEEPSLATE_REDSTONE_ORE => "minecraft:deepslate_redstone_ore",
        block_state_ids::DEEPSLATE_LAPIS_ORE => "minecraft:deepslate_lapis_ore",
        block_state_ids::DEEPSLATE_DIAMOND_ORE => "minecraft:deepslate_diamond_ore",
        block_state_ids::DEEPSLATE_EMERALD_ORE => "minecraft:deepslate_emerald_ore",
        block_state_ids::LAVA => "minecraft:lava",
        block_state_ids::CHEST => "minecraft:chest",
        block_state_ids::SPAWNER => "minecraft:spawner",
        block_state_ids::END_PORTAL_FRAME => "minecraft:end_portal_frame",
        block_state_ids::END_PORTAL => "minecraft:end_portal",
        block_state_ids::STONE_BRICKS => "minecraft:stone_bricks",
        block_state_ids::END_PORTAL_FRAME_FILLED => "minecraft:end_portal_frame_filled",
        block_state_ids::OAK_LOG => "minecraft:oak_log",
        block_state_ids::OAK_LEAVES => "minecraft:oak_leaves",
        block_state_ids::JUNGLE_LOG => "minecraft:jungle_log",
        block_state_ids::JUNGLE_LEAVES => "minecraft:jungle_leaves",
        block_state_ids::GRAVEL => "minecraft:gravel",
        block_state_ids::CLAY => "minecraft:clay",
        block_state_ids::RED_SAND => "minecraft:red_sand",
        block_state_ids::COARSE_DIRT => "minecraft:coarse_dirt",
        block_state_ids::TERRACOTTA => "minecraft:terracotta",
        block_state_ids::ORANGE_TERRACOTTA => "minecraft:orange_terracotta",
        block_state_ids::BROWN_TERRACOTTA => "minecraft:brown_terracotta",
        block_state_ids::MUD => "minecraft:mud",
        block_state_ids::MOSS_BLOCK => "minecraft:moss_block",
        block_state_ids::PODZOL => "minecraft:podzol",
        block_state_ids::WHITE_TERRACOTTA => "minecraft:white_terracotta",
        block_state_ids::LIGHT_GRAY_TERRACOTTA => "minecraft:light_gray_terracotta",
        block_state_ids::GRAY_TERRACOTTA => "minecraft:gray_terracotta",
        block_state_ids::BLACK_TERRACOTTA => "minecraft:black_terracotta",
        block_state_ids::YELLOW_TERRACOTTA => "minecraft:yellow_terracotta",
        block_state_ids::RED_TERRACOTTA => "minecraft:red_terracotta",
        block_state_ids::GREEN_TERRACOTTA => "minecraft:green_terracotta",
        block_state_ids::CYAN_TERRACOTTA => "minecraft:cyan_terracotta",
        block_state_ids::LIME_TERRACOTTA => "minecraft:lime_terracotta",
        block_state_ids::PACKED_MUD => "minecraft:packed_mud",
        block_state_ids::CALCITE => "minecraft:calcite",
        block_state_ids::TUFF => "minecraft:tuff",
        block_state_ids::SANDSTONE => "minecraft:sandstone",
        block_state_ids::ROOTED_DIRT => "minecraft:rooted_dirt",
        block_state_ids::MYCELIUM => "minecraft:mycelium",
        block_state_ids::ANDESITE => "minecraft:andesite",
        block_state_ids::GRANITE => "minecraft:granite",
        block_state_ids::DIORITE => "minecraft:diorite",
        block_state_ids::DARK_OAK_LEAVES => "minecraft:dark_oak_leaves",
        block_state_ids::SPRUCE_LEAVES => "minecraft:spruce_leaves",
        block_state_ids::BLACK_CONCRETE => "minecraft:black_concrete",
        block_state_ids::QUARTZ_BLOCK => "minecraft:quartz_block",
        block_state_ids::BONE_BLOCK => "minecraft:bone_block",
        block_state_ids::END_STONE => "minecraft:end_stone",
        block_state_ids::END_STONE_BRICKS => "minecraft:end_stone_bricks",
        block_state_ids::SMOOTH_SANDSTONE => "minecraft:smooth_sandstone",
        block_state_ids::CUT_SANDSTONE => "minecraft:cut_sandstone",
        block_state_ids::CHISELED_SANDSTONE => "minecraft:chiseled_sandstone",
        block_state_ids::SMOOTH_RED_SANDSTONE => "minecraft:smooth_red_sandstone",
        block_state_ids::CUT_RED_SANDSTONE => "minecraft:cut_red_sandstone",
        block_state_ids::CHISELED_RED_SANDSTONE => "minecraft:chiseled_red_sandstone",
        block_state_ids::MUD_BRICKS => "minecraft:mud_bricks",
        block_state_ids::DRIPSTONE_BLOCK => "minecraft:dripstone_block",
        _ => "minecraft:unknown",
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_vanilla_delegated_region(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
    status_text: &str,
    surface_raster_text: &str,
    extra_options: &[String],
) -> io::Result<i32> {
    match generate_vanilla_delegated_region_impl(
        heightmap_path,
        world_dir,
        scale_text,
        region_x_text,
        region_z_text,
        format_text,
        status_text,
        surface_raster_text,
        extra_options,
    ) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Vanilla-delegated region generation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_vanilla_delegated_region_impl(
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
    status_text: &str,
    surface_raster_text: &str,
    extra_options: &[String],
) -> std::result::Result<Vec<String>, String> {
    generate_vanilla_delegated_region_impl_with_osm_mask(
        heightmap_path,
        world_dir,
        scale_text,
        region_x_text,
        region_z_text,
        format_text,
        status_text,
        surface_raster_text,
        extra_options,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn generate_vanilla_delegated_region_impl_with_osm_mask(
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    format_text: &str,
    status_text: &str,
    surface_raster_text: &str,
    extra_options: &[String],
    osm_region_feature_mask: Option<SurfaceOsmRegionFeatureMask>,
) -> std::result::Result<Vec<String>, String> {
    let format = OutputFormat::parse(format_text).map_err(|error| error.to_string())?;
    let runtime_options = parse_generation_runtime_options(format, extra_options)?;
    let status = ChunkGenerationStatus::parse(status_text).map_err(|error| error.to_string())?;
    if status == ChunkGenerationStatus::Full {
        return Err("delegated generation status must be surface or carvers".to_string());
    }
    let surface_material_path =
        parse_optional_surface_material_path(surface_raster_text, Path::new(heightmap_path))?
            .ok_or_else(|| {
                "default textureMode=photo requires a TrueMarble surface raster; use surfaceRaster=auto or pass an explicit TrueMarble.vrt path"
                    .to_string()
            })?;
    let scale = parse_i32_string(scale_text)?;
    let vertical_scale = runtime_options.vertical_scale.effective(scale);
    let region_x = parse_i32_string(region_x_text)?;
    let region_z = parse_i32_string(region_z_text)?;
    let cache_rows = configured_heightmap_cache_rows(Path::new(heightmap_path))?;
    let surface_tile_cache_entries =
        configured_surface_tile_cache_entries(Some(&surface_material_path))?;
    let mut settings = SurfaceRegionSettings::new_with_texture_options(
        heightmap_path,
        world_dir,
        "SR EarthMap Vanilla Delegated",
        0,
        scale,
        region_x,
        region_z,
        format,
        cache_rows,
        true,
        status,
        vertical_scale,
        SurfaceTextureMode::Photo,
    )
    .map_err(|error| error.to_string())?;
    settings.surface_material_path = Some(surface_material_path.clone());
    settings.surface_tile_cache_entries = surface_tile_cache_entries;
    settings.osm_region_feature_mask = osm_region_feature_mask;
    apply_region_compression_options(&mut settings, runtime_options.compression);

    let report =
        earthmap_surface::generate_surface_region(&settings).map_err(|error| error.to_string())?;
    Ok(vanilla_delegated_region_report_lines(
        &report,
        world_dir,
        status,
        &surface_material_path,
        runtime_options.compression,
        runtime_options.vertical_scale.label(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn generate_vanilla_delegated_regions_parallel(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
    threads_text: &str,
    status_text: &str,
    surface_raster_text: &str,
    extra_options: &[String],
) -> io::Result<i32> {
    match generate_vanilla_delegated_regions_parallel_impl(
        out,
        heightmap_path,
        world_dir,
        scale_text,
        start_region_x_text,
        start_region_z_text,
        cols_text,
        rows_text,
        format_text,
        threads_text,
        status_text,
        surface_raster_text,
        extra_options,
    ) {
        Ok(()) => Ok(EXIT_OK),
        Err(error) => {
            writeln!(
                err,
                "Vanilla-delegated parallel region generation failed: {error}"
            )?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_vanilla_delegated_regions_parallel_impl(
    out: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
    threads_text: &str,
    status_text: &str,
    surface_raster_text: &str,
    extra_options: &[String],
) -> std::result::Result<(), String> {
    let total_start = Instant::now();
    let format = OutputFormat::parse(format_text).map_err(|error| error.to_string())?;
    let runtime_options = parse_generation_runtime_options(format, extra_options)?;
    let status = ChunkGenerationStatus::parse(status_text).map_err(|error| error.to_string())?;
    if status == ChunkGenerationStatus::Full {
        return Err("delegated generation status must be surface or carvers".to_string());
    }
    let scale = parse_positive_i32_string("scale", scale_text)?;
    let vertical_scale = runtime_options.vertical_scale.effective(scale);
    let start_region_x = parse_i32_string(start_region_x_text)?;
    let start_region_z = parse_i32_string(start_region_z_text)?;
    let cols = parse_positive_i32_string("cols", cols_text)?;
    let rows = parse_positive_i32_string("rows", rows_text)?;
    let threads = parse_positive_usize_string("threads", threads_text)?;
    let region_count = usize::try_from(
        cols.checked_mul(rows)
            .ok_or_else(|| "region grid size overflow".to_string())?,
    )
    .map_err(|_| "region grid size overflow".to_string())?;

    let heightmap = Path::new(heightmap_path);
    let world = Path::new(world_dir);
    let surface_material_path =
        parse_optional_surface_material_path(surface_raster_text, heightmap)?.ok_or_else(|| {
            "default textureMode=photo requires a TrueMarble surface raster; use surfaceRaster=auto or pass an explicit TrueMarble.vrt path"
                .to_string()
        })?;
    let cache_rows = configured_heightmap_cache_rows(heightmap)?;
    let surface_tile_cache_entries =
        configured_surface_tile_cache_entries(Some(&surface_material_path))?;
    let resume_fingerprint = vanilla_delegated_parallel_resume_fingerprint(
        heightmap,
        format,
        scale,
        start_region_x,
        start_region_z,
        cols,
        rows,
        status,
        &surface_material_path,
        runtime_options.compression,
        vertical_scale,
        runtime_options.vertical_scale.label(),
    );

    configure_surface_photo_rayon_threads(threads);
    write_progress_event(
        out,
        json!({
            "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
            "type": "workerTuningStarted",
            "requestedThreads": threads,
            "submittedRegions": region_count,
            "candidateWorkerThreads": surface_photo_worker_candidates(threads, region_count),
            "candidateWorkerConfigs": surface_photo_worker_candidate_configs(threads, region_count)
                .into_iter()
                .map(surface_photo_worker_candidate_config_json)
                .collect::<Vec<_>>(),
            "minRegions": SURFACE_PHOTO_AUTOTUNE_MIN_REGIONS,
            "maxSamples": SURFACE_PHOTO_AUTOTUNE_MAX_SAMPLES,
        }),
    )?;
    let worker_tuning = tune_surface_photo_workers_for_grid(
        heightmap,
        world,
        format,
        scale,
        start_region_x,
        start_region_z,
        cols,
        rows,
        threads,
        status,
        vertical_scale,
        &surface_material_path,
        cache_rows,
        surface_tile_cache_entries,
        runtime_options.compression,
    );
    write_progress_event(
        out,
        json!({
            "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
            "type": "workerTuningFinished",
            "workerTuning": worker_tuning.to_progress_json(),
        }),
    )?;
    let worker_count = worker_tuning.selected_worker_count.min(region_count).max(1);
    let rayon_threads = worker_tuning.selected_rayon_threads.max(worker_count);
    let parallel_column_sampling = worker_tuning.parallel_column_sampling;
    let heightmap_cache_rows_per_worker = cache_rows.div_ceil(worker_count).max(1);
    let surface_tile_cache_entries_per_worker =
        surface_tile_cache_entries.div_ceil(worker_count).max(1);
    let prefetch_config =
        effective_prefetch_config(runtime_options.prefetch, worker_count, region_count);
    let prefetch_heightmap_cache_rows_per_worker =
        cache_rows.div_ceil(prefetch_config.workers.max(1)).max(1);
    let prefetch_surface_tile_cache_entries_per_worker = surface_tile_cache_entries
        .div_ceil(prefetch_config.workers.max(1))
        .max(1);
    let (prefetch_sample_rayon_threads, prefetch_output_rayon_threads) = if prefetch_config.enabled
    {
        split_prefetch_rayon_threads(rayon_threads, prefetch_config.workers, worker_count)
    } else {
        (0, 0)
    };
    let prefetch_open_ocean_output_rayon_threads = if prefetch_config.enabled {
        open_ocean_prefetch_output_rayon_threads(
            rayon_threads,
            prefetch_output_rayon_threads,
            worker_count,
        )
    } else {
        0
    };
    let setup_start = Instant::now();
    std::fs::create_dir_all(world.join("region")).map_err(|error| error.to_string())?;
    let spawn_x = start_region_x
        .wrapping_mul(REGION_SIZE_BLOCKS)
        .wrapping_add(cols.wrapping_mul(REGION_SIZE_BLOCKS) / 2);
    let spawn_z = start_region_z
        .wrapping_mul(REGION_SIZE_BLOCKS)
        .wrapping_add(rows.wrapping_mul(REGION_SIZE_BLOCKS) / 2);
    let level_settings = level_dat_template::Settings::new(
        "SR EarthMap Vanilla Delegated",
        0,
        spawn_x,
        SEA_LEVEL_Y + 10,
        spawn_z,
    )
    .map_err(|error| error.to_string())?;
    level_dat_template::write(world.join("level.dat"), &level_settings)
        .map_err(|error| error.to_string())?;
    let manifest_file = write_vanilla_delegated_parallel_manifest(
        world,
        heightmap,
        format,
        scale,
        start_region_x,
        start_region_z,
        cols,
        rows,
        threads,
        status,
        &surface_material_path,
        runtime_options.compression,
        vertical_scale,
        runtime_options.vertical_scale.label(),
    )
    .map_err(|error| error.to_string())?;
    let resume = prepare_vanilla_delegated_resume_journal(world, &resume_fingerprint)?;
    let stale_temp_files = cleanup_stale_region_temp_files(&world.join("region"));
    let setup_millis = setup_start.elapsed().as_millis();

    write_progress_event(
        out,
        json!({
            "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
            "type": "batchStarted",
            "worldDir": normalized_path_display(world),
            "format": format.java_name(),
            "scale": scale,
            "verticalScale": vertical_scale,
            "verticalScaleMode": runtime_options.vertical_scale.label(),
            "chunkStatus": status.id(),
            "regionStartX": start_region_x,
            "regionStartZ": start_region_z,
            "regionCols": cols,
            "regionRows": rows,
            "regionCount": region_count,
            "requestedThreads": threads,
            "workerThreads": worker_count,
            "regionWorkerThreads": worker_count,
            "rayonThreads": rayon_threads,
            "parallelColumnSampling": parallel_column_sampling,
            "prefetchEnabled": prefetch_config.enabled,
            "prefetchWorkers": prefetch_config.workers,
            "prefetchQueueRegions": prefetch_config.queue_regions,
            "prefetchMemoryCapBytes": prefetch_config.memory_cap_bytes,
            "prefetchEstimatedRegionBytes": prefetch_config.estimated_region_bytes,
            "prefetchSampleRayonThreads": prefetch_sample_rayon_threads,
            "prefetchOutputRayonThreads": prefetch_output_rayon_threads,
            "prefetchOpenOceanOutputRayonThreads": prefetch_open_ocean_output_rayon_threads,
            "workerTuning": worker_tuning.to_progress_json(),
            "resumeFingerprintMatched": resume.fingerprint_matched,
            "resumeJournalRegions": resume.completed_regions.len(),
            "resumeJournal": normalized_path_display(&resume.path),
        }),
    )?;
    for warning in &resume.warnings {
        writeln!(out, "resumeJournalWarning={}", csv_cell(warning))
            .map_err(|error| error.to_string())?;
    }
    writeln!(
        out,
        "type,status,regionX,regionZ,elapsedMillis,chunks,outputBytes,regionFile,message"
    )
    .map_err(|error| error.to_string())?;

    let mut regions = VecDeque::with_capacity(region_count);
    for row in 0..rows {
        for col in 0..cols {
            regions.push_back((
                start_region_x.wrapping_add(col),
                start_region_z.wrapping_add(row),
            ));
        }
    }

    let region_queue = Mutex::new(regions);
    let stop_queueing = AtomicBool::new(false);
    let resume_completed_regions = Arc::new(resume.completed_regions);
    let resume_journal = resume.journal;
    let (event_sender, event_receiver) =
        mpsc::sync_channel::<VanillaDelegatedParallelEvent>(PARALLEL_EVENT_CHANNEL_CAPACITY);
    let mut stats = VanillaDelegatedParallelBatchStats::default();
    if prefetch_config.enabled {
        let prefetch_sample_pool = build_surface_photo_rayon_pool(prefetch_sample_rayon_threads)?;
        let prefetch_output_pool = build_surface_photo_rayon_pool(prefetch_output_rayon_threads)?;
        let prefetch_open_ocean_output_pool =
            build_surface_photo_rayon_pool(prefetch_open_ocean_output_rayon_threads)?;
        std::thread::scope(|scope| {
            let (prepared_sender, prepared_receiver) =
                mpsc::sync_channel::<PreparedVanillaDelegatedRegion>(prefetch_config.queue_regions);
            let prepared_receiver = Arc::new(Mutex::new(prepared_receiver));
            for _ in 0..prefetch_config.workers {
                let prepared_sender = prepared_sender.clone();
                let sender = event_sender.clone();
                let prefetch_sample_pool = &prefetch_sample_pool;
                let queue = &region_queue;
                let stop_queueing = &stop_queueing;
                let surface_material_path = &surface_material_path;
                let completed_regions = Arc::clone(&resume_completed_regions);
                scope.spawn(move || {
                    let producer_heightmap_reader =
                        match GeoTiffHeightmapReader::open(heightmap_path) {
                            Ok(reader) => reader,
                            Err(_) => {
                                stop_queueing.store(true, Ordering::SeqCst);
                                return;
                            }
                        };
                    let producer_heightmap_mapping =
                        match mapping_for(producer_heightmap_reader.metadata(), scale) {
                            Ok(mapping) => mapping,
                            Err(_) => {
                                stop_queueing.store(true, Ordering::SeqCst);
                                return;
                            }
                        };
                    let producer_heightmap_cache = match GeoTiffRowCache::new(
                        &producer_heightmap_reader,
                        prefetch_heightmap_cache_rows_per_worker,
                    ) {
                        Ok(cache) => cache,
                        Err(_) => {
                            stop_queueing.store(true, Ordering::SeqCst);
                            return;
                        }
                    };
                    let producer_surface_material_sampler =
                        match EarthDataSurfaceMaterialSampler::open_with_tile_cache_entries(
                            surface_material_path,
                            prefetch_surface_tile_cache_entries_per_worker,
                        ) {
                            Ok(sampler) => sampler,
                            Err(_) => {
                                stop_queueing.store(true, Ordering::SeqCst);
                                return;
                            }
                        };
                    loop {
                        if stop_queueing.load(Ordering::SeqCst) {
                            break;
                        }
                        let next_region = {
                            let mut queue = queue.lock().expect("queue lock");
                            if stop_queueing.load(Ordering::SeqCst) {
                                None
                            } else {
                                queue.pop_front()
                            }
                        };
                        let Some((region_x, region_z)) = next_region else {
                            break;
                        };
                        let region_file =
                            vanilla_delegated_region_file(world, format, region_x, region_z);
                        if resume.fingerprint_matched
                            && completed_regions.contains(&(region_x, region_z))
                        {
                            let skip_start = Instant::now();
                            if let Ok(validation) = validate_region_file_for_resume(
                                &region_file,
                                region_format_for_output(format),
                                region_x,
                                region_z,
                                REGION_CHUNKS_PER_REGION,
                            ) {
                                if sender
                                    .send(VanillaDelegatedParallelEvent::RegionSkipped {
                                        region_x,
                                        region_z,
                                        elapsed_millis: skip_start.elapsed().as_millis(),
                                        chunks: validation.chunk_count,
                                        output_bytes: validation.file_bytes,
                                        region_file,
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                        }
                        if sender
                            .send(VanillaDelegatedParallelEvent::RegionStarted {
                                region_x,
                                region_z,
                                region_file: region_file.clone(),
                            })
                            .is_err()
                        {
                            break;
                        };
                        let started_at = Instant::now();
                        let failed_region_file = region_file.clone();
                        let result =
                            (|| -> std::result::Result<PreparedVanillaDelegatedRegion, String> {
                                let settings = vanilla_delegated_surface_region_settings(
                                    heightmap_path,
                                    world_dir,
                                    "SR EarthMap Vanilla Delegated",
                                    scale,
                                    region_x,
                                    region_z,
                                    format,
                                    prefetch_heightmap_cache_rows_per_worker,
                                    status,
                                    vertical_scale,
                                    surface_material_path,
                                    prefetch_surface_tile_cache_entries_per_worker,
                                    parallel_column_sampling,
                                    runtime_options.compression,
                                )?;
                                let sample = prefetch_sample_pool.install(|| {
                                    let producer_heightmap_sampler =
                                        HeightmapScalarSampler::with_row_cache(
                                            &producer_heightmap_reader,
                                            &producer_heightmap_cache,
                                        );
                                    let mut sample =
                                        prepare_surface_region_sample_with_heightmap_sampler(
                                            &settings,
                                            &producer_heightmap_mapping,
                                            &producer_heightmap_sampler,
                                            producer_heightmap_cache.stats(),
                                            Some(&producer_surface_material_sampler),
                                        )
                                        .map_err(|error| error.to_string())?;
                                    sample.cache_stats = producer_heightmap_cache.stats();
                                    Ok::<PreparedSurfaceRegionSample, String>(sample)
                                })?;
                                Ok(PreparedVanillaDelegatedRegion {
                                    region_x,
                                    region_z,
                                    region_file,
                                    started_at,
                                    prepared_at: Instant::now(),
                                    prefetch_send_wait_millis: 0,
                                    sample,
                                })
                            })();
                        match result {
                            Ok(prepared) => {
                                if !send_prepared_vanilla_region(
                                    &prepared_sender,
                                    stop_queueing,
                                    prepared,
                                ) {
                                    break;
                                }
                            }
                            Err(message) => {
                                stop_queueing.store(true, Ordering::SeqCst);
                                let _ = sender.send(VanillaDelegatedParallelEvent::RegionFailed {
                                    region_x,
                                    region_z,
                                    elapsed_millis: started_at.elapsed().as_millis(),
                                    region_file: failed_region_file,
                                    message,
                                });
                                break;
                            }
                        }
                    }
                });
            }
            drop(prepared_sender);
            for _ in 0..worker_count {
                let receiver = Arc::clone(&prepared_receiver);
                let sender = event_sender.clone();
                let prefetch_output_pool = &prefetch_output_pool;
                let prefetch_open_ocean_output_pool = &prefetch_open_ocean_output_pool;
                let stop_queueing = &stop_queueing;
                let surface_material_path = &surface_material_path;
                let journal = Arc::clone(&resume_journal);
                scope.spawn(move || loop {
                    let prepared = {
                        let receiver = receiver.lock().expect("prepared queue lock");
                        receiver.recv().ok()
                    };
                    let Some(prepared) = prepared else {
                        break;
                    };
                    if stop_queueing.load(Ordering::SeqCst) {
                        break;
                    }
                    let region_x = prepared.region_x;
                    let region_z = prepared.region_z;
                    let region_file = prepared.region_file.clone();
                    let started_at = prepared.started_at;
                    let use_open_ocean_output_pool =
                        prepared.sample.sample.phase_nanos().open_ocean_fast_path > 0;
                    let ready_queue_wait_millis = prepared.prepared_at.elapsed().as_millis();
                    let prefetch_send_wait_millis = prepared.prefetch_send_wait_millis;
                    let consumer_started_at = Instant::now();
                    let result =
                        (|| -> std::result::Result<
                            (SurfaceRegionReport, PrefetchTimingMillis),
                            String,
                        > {
                        let settings = vanilla_delegated_surface_region_settings(
                            heightmap_path,
                            world_dir,
                            "SR EarthMap Vanilla Delegated",
                            scale,
                            region_x,
                            region_z,
                            format,
                            cache_rows,
                            status,
                            vertical_scale,
                            surface_material_path,
                            surface_tile_cache_entries,
                            parallel_column_sampling,
                            runtime_options.compression,
                        )?;
                        let output_pool_start = Instant::now();
                        let output_pool = if use_open_ocean_output_pool {
                            prefetch_open_ocean_output_pool
                        } else {
                            prefetch_output_pool
                        };
                        let report = output_pool
                            .install(|| {
                                generate_surface_region_with_prepared_sample(
                                    &settings,
                                    prepared.sample,
                                )
                            })
                            .map_err(|error| error.to_string())?;
                        let consumer_pool_elapsed_millis = output_pool_start.elapsed().as_millis();
                        let consumer_work_millis = millis(report.total_nanos)
                            .saturating_sub(millis(report.surface_sample_nanos));
                        let consumer_pool_wait_millis =
                            consumer_pool_elapsed_millis.saturating_sub(consumer_work_millis);
                        let output_bytes = std::fs::metadata(&report.region_file)
                            .map(|metadata| metadata.len())
                            .unwrap_or(0);
                        journal
                            .lock()
                            .map_err(|_| "resume journal lock poisoned".to_string())?
                            .append_region_complete(
                                report.region_x,
                                report.region_z,
                                format,
                                report.chunk_count,
                                output_bytes,
                            )?;
                        Ok((
                            report,
                            PrefetchTimingMillis {
                                send_wait: prefetch_send_wait_millis,
                                ready_queue_wait: ready_queue_wait_millis,
                                consumer_pool_wait: consumer_pool_wait_millis,
                                consumer_elapsed: consumer_started_at.elapsed().as_millis(),
                            },
                        ))
                    })();
                    match result {
                        Ok((report, prefetch_timing)) => {
                            let output_bytes = std::fs::metadata(&report.region_file)
                                .map(|metadata| metadata.len())
                                .unwrap_or(0);
                            if sender
                                .send(VanillaDelegatedParallelEvent::RegionGenerated {
                                    elapsed_millis: started_at.elapsed().as_millis(),
                                    output_bytes,
                                    report,
                                    prefetch_timing: Some(prefetch_timing),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(message) => {
                            stop_queueing.store(true, Ordering::SeqCst);
                            let _ = sender.send(VanillaDelegatedParallelEvent::RegionFailed {
                                region_x,
                                region_z,
                                elapsed_millis: started_at.elapsed().as_millis(),
                                region_file,
                                message,
                            });
                            break;
                        }
                    }
                });
            }
            drop(event_sender);
            while let Ok(event) = event_receiver.recv() {
                handle_vanilla_delegated_parallel_event(out, &mut stats, event)?;
            }
            Ok::<(), String>(())
        })?;
    } else {
        let generation_pool = build_surface_photo_rayon_pool(rayon_threads)?;
        std::thread::scope(|scope| {
            let coordinator_sender = event_sender.clone();
            let generation_pool = &generation_pool;
            let region_queue = &region_queue;
            let stop_queueing = &stop_queueing;
            let surface_material_path = &surface_material_path;
            let resume_completed_regions = &resume_completed_regions;
            let resume_journal = &resume_journal;
            let coordinator = scope.spawn(move || {
                generation_pool.scope(|rayon_scope| {
                    for _ in 0..worker_count {
                        let queue = region_queue;
                        let stop_queueing = stop_queueing;
                        let surface_material_path = surface_material_path;
                        let completed_regions = Arc::clone(&resume_completed_regions);
                        let journal = Arc::clone(resume_journal);
                        let sender = coordinator_sender.clone();
                        let fingerprint_matched = resume.fingerprint_matched;
                        rayon_scope.spawn(move |_| {
                            let worker_heightmap_reader =
                                match GeoTiffHeightmapReader::open(heightmap_path) {
                                    Ok(reader) => reader,
                                    Err(_) => {
                                        stop_queueing.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                };
                            let worker_heightmap_mapping =
                                match mapping_for(worker_heightmap_reader.metadata(), scale) {
                                    Ok(mapping) => mapping,
                                    Err(_) => {
                                        stop_queueing.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                };
                            let worker_heightmap_cache = match GeoTiffRowCache::new(
                                &worker_heightmap_reader,
                                heightmap_cache_rows_per_worker,
                            ) {
                                Ok(cache) => cache,
                                Err(_) => {
                                    stop_queueing.store(true, Ordering::SeqCst);
                                    return;
                                }
                            };
                            let worker_surface_material_sampler =
                                match EarthDataSurfaceMaterialSampler::open_with_tile_cache_entries(
                                    surface_material_path,
                                    surface_tile_cache_entries_per_worker,
                                ) {
                                    Ok(sampler) => sampler,
                                    Err(_) => {
                                        stop_queueing.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                };
                            let heightmap_sampler = HeightmapScalarSampler::with_row_cache(
                                &worker_heightmap_reader,
                                &worker_heightmap_cache,
                            );
                            loop {
                                if stop_queueing.load(Ordering::SeqCst) {
                                    break;
                                }
                                let next_region = {
                                    let mut queue = queue.lock().expect("queue lock");
                                    if stop_queueing.load(Ordering::SeqCst) {
                                        None
                                    } else {
                                        queue.pop_front()
                                    }
                                };
                                let Some((region_x, region_z)) = next_region else {
                                    break;
                                };
                                let region_file = vanilla_delegated_region_file(
                                    world, format, region_x, region_z,
                                );
                                if fingerprint_matched
                                    && completed_regions.contains(&(region_x, region_z))
                                {
                                    let skip_start = Instant::now();
                                    if let Ok(validation) = validate_region_file_for_resume(
                                        &region_file,
                                        region_format_for_output(format),
                                        region_x,
                                        region_z,
                                        REGION_CHUNKS_PER_REGION,
                                    ) {
                                        if sender
                                            .send(VanillaDelegatedParallelEvent::RegionSkipped {
                                                region_x,
                                                region_z,
                                                elapsed_millis: skip_start.elapsed().as_millis(),
                                                chunks: validation.chunk_count,
                                                output_bytes: validation.file_bytes,
                                                region_file,
                                            })
                                            .is_err()
                                        {
                                            break;
                                        }
                                        continue;
                                    }
                                }
                                if sender
                                    .send(VanillaDelegatedParallelEvent::RegionStarted {
                                        region_x,
                                        region_z,
                                        region_file: region_file.clone(),
                                    })
                                    .is_err()
                                {
                                    break;
                                };
                                let region_start = Instant::now();
                                let result =
                                    (|| -> std::result::Result<SurfaceRegionReport, String> {
                                        let mut settings =
                                            SurfaceRegionSettings::new_with_texture_options(
                                                heightmap_path,
                                                world_dir,
                                                "SR EarthMap Vanilla Delegated",
                                                0,
                                                scale,
                                                region_x,
                                                region_z,
                                                format,
                                                heightmap_cache_rows_per_worker,
                                                false,
                                                status,
                                                vertical_scale,
                                                SurfaceTextureMode::Photo,
                                            )
                                            .map_err(|error| error.to_string())?;
                                        settings.surface_material_path =
                                            Some((*surface_material_path).clone());
                                        settings.surface_tile_cache_entries =
                                            surface_tile_cache_entries_per_worker;
                                        settings.parallel_column_sampling =
                                            parallel_column_sampling;
                                        apply_region_compression_options(
                                            &mut settings,
                                            runtime_options.compression,
                                        );
                                        let mut prepared =
                                            prepare_surface_region_sample_with_heightmap_sampler(
                                                &settings,
                                                &worker_heightmap_mapping,
                                                &heightmap_sampler,
                                                worker_heightmap_cache.stats(),
                                                Some(&worker_surface_material_sampler),
                                            )
                                            .map_err(|error| error.to_string())?;
                                        prepared.cache_stats = worker_heightmap_cache.stats();
                                        let report = generate_surface_region_with_prepared_sample(
                                            &settings, prepared,
                                        )
                                        .map_err(|error| error.to_string())?;
                                        let output_bytes = std::fs::metadata(&report.region_file)
                                            .map(|metadata| metadata.len())
                                            .unwrap_or(0);
                                        journal
                                            .lock()
                                            .map_err(|_| {
                                                "resume journal lock poisoned".to_string()
                                            })?
                                            .append_region_complete(
                                                report.region_x,
                                                report.region_z,
                                                format,
                                                report.chunk_count,
                                                output_bytes,
                                            )?;
                                        Ok(report)
                                    })();
                                match result {
                                    Ok(report) => {
                                        let output_bytes = std::fs::metadata(&report.region_file)
                                            .map(|metadata| metadata.len())
                                            .unwrap_or(0);
                                        if sender
                                            .send(VanillaDelegatedParallelEvent::RegionGenerated {
                                                elapsed_millis: region_start.elapsed().as_millis(),
                                                output_bytes,
                                                report,
                                                prefetch_timing: None,
                                            })
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    Err(message) => {
                                        stop_queueing.store(true, Ordering::SeqCst);
                                        let _ = sender.send(
                                            VanillaDelegatedParallelEvent::RegionFailed {
                                                region_x,
                                                region_z,
                                                elapsed_millis: region_start.elapsed().as_millis(),
                                                region_file,
                                                message,
                                            },
                                        );
                                        break;
                                    }
                                }
                            }
                        });
                    }
                });
            });
            drop(event_sender);
            while let Ok(event) = event_receiver.recv() {
                handle_vanilla_delegated_parallel_event(out, &mut stats, event)?;
            }
            coordinator
                .join()
                .map_err(|_| "parallel region worker coordinator panicked".to_string())?;
            Ok::<(), String>(())
        })?;
    }
    if let Ok(journal) = resume_journal.lock() {
        journal.sync()?;
    }
    let elapsed_millis = total_start.elapsed().as_millis();
    let generated_regions_per_hour = if elapsed_millis == 0 {
        0.0
    } else {
        (stats.generated_regions as f64) * 3_600_000.0 / (elapsed_millis as f64)
    };
    let completed_regions = stats.generated_regions + stats.skipped_regions;
    let completed_regions_per_hour = if elapsed_millis == 0 {
        0.0
    } else {
        (completed_regions as f64) * 3_600_000.0 / (elapsed_millis as f64)
    };
    write_progress_event(
        out,
        json!({
            "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
            "type": "batchSummary",
            "elapsedMillis": u128_to_u64(elapsed_millis),
            "generatedRegions": stats.generated_regions,
            "skippedRegions": stats.skipped_regions,
            "completedRegions": completed_regions,
            "failedRegions": stats.failed_regions,
            "allDone": stats.failed_regions == 0,
            "generatedRegionsPerHour": generated_regions_per_hour,
            "completedRegionsPerHour": completed_regions_per_hour,
        }),
    )?;
    let lines = [
        "Resumable vanilla-delegated batch complete".to_string(),
        format!("worldDir={}", world.display()),
        format!("format={}", format.java_name()),
        format!("scale=1:{scale}"),
        format!("chunkStatus={}", status.id()),
        format!("regionStartX={start_region_x}"),
        format!("regionStartZ={start_region_z}"),
        format!("regionCols={cols}"),
        format!("regionRows={rows}"),
        format!("requestedThreads={threads}"),
        format!("workerThreads={worker_count}"),
        format!("regionWorkerThreads={worker_count}"),
        format!("rayonThreads={rayon_threads}"),
        format!("parallelColumnSampling={parallel_column_sampling}"),
        format!("prefetchEnabled={}", prefetch_config.enabled),
        format!("prefetchWorkers={}", prefetch_config.workers),
        format!("prefetchQueueRegions={}", prefetch_config.queue_regions),
        format!(
            "prefetchMemoryCapBytes={}",
            prefetch_config
                .memory_cap_bytes
                .map(|bytes| bytes.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "prefetchEstimatedRegionBytes={}",
            prefetch_config.estimated_region_bytes
        ),
        format!("workerTuningMode={}", worker_tuning.mode),
        format!("workerTuningSamples={}", worker_tuning.sample_count),
        format!("workerTuningLandSamples={}", worker_tuning.land_samples),
        format!("workerTuningOceanSamples={}", worker_tuning.ocean_samples),
        format!("workerTuningMixedSamples={}", worker_tuning.mixed_samples),
        format!(
            "surfaceSamplerStrategy={}",
            if prefetch_config.enabled {
                "prefetch-shared"
            } else {
                "per-worker-heightmap-material"
            }
        ),
        format!("sharedCacheRows={cache_rows}"),
        format!("heightmapCacheRowsPerWorker={heightmap_cache_rows_per_worker}"),
        format!("surfaceTileCacheEntriesPerWorker={surface_tile_cache_entries_per_worker}"),
        format!("surfaceTileCacheEntries={surface_tile_cache_entries}"),
        format!("verticalScale={vertical_scale}"),
        format!(
            "verticalScaleMode={}",
            runtime_options.vertical_scale.label()
        ),
        compression_options_report_line(format, runtime_options.compression),
        format!(
            "surfaceMaterialPath={}",
            normalized_path_display(&surface_material_path)
        ),
        format!("resumeJournal={}", resume.path.display()),
        format!("resumeFingerprintMatched={}", resume.fingerprint_matched),
        format!("resumeJournalRegions={}", resume_completed_regions.len()),
        format!("staleTempRegionFilesRemoved={stale_temp_files}"),
        format!("setupMillis={setup_millis}"),
        format!(
            "batchPhaseSummary.surfaceSampleMillis={}",
            stats.phase_surface_sample_millis
        ),
        format!(
            "batchPhaseSummary.chunkBuildMillis={}",
            stats.phase_chunk_build_millis
        ),
        format!(
            "batchPhaseSummary.nbtEncodeMillis={}",
            stats.phase_nbt_encode_millis
        ),
        format!(
            "batchPhaseSummary.regionWriteMillis={}",
            stats.phase_region_write_millis
        ),
        format!(
            "batchPhaseSummary.metadataMillis={}",
            stats.phase_metadata_millis
        ),
        format!(
            "batchPhaseSummary.totalInternalMillis={}",
            stats.phase_total_internal_millis
        ),
        format!("elapsedMillis={elapsed_millis}"),
        format!("generatedRegions={}", stats.generated_regions),
        format!("skippedRegions={}", stats.skipped_regions),
        format!("completedRegions={completed_regions}"),
        format!("failedRegions={}", stats.failed_regions),
        format!("generatedRegionsPerHour={generated_regions_per_hour:.2}"),
        format!("completedRegionsPerHour={completed_regions_per_hour:.2}"),
        format!("allDone={}", stats.failed_regions == 0),
        format!("manifestFile={}", manifest_file.display()),
    ];
    for line in lines {
        writeln!(out, "{line}").map_err(|error| error.to_string())?;
    }
    if !stats.errors.is_empty() {
        return Err(stats.errors.join("; "));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn generate_vanilla_delegated_plan_parallel(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    format_text: &str,
    threads_text: &str,
    plan_csv: &str,
    optional_args: &[String],
) -> io::Result<i32> {
    match generate_vanilla_delegated_plan_parallel_impl(
        out,
        heightmap_path,
        world_dir,
        scale_text,
        format_text,
        threads_text,
        plan_csv,
        optional_args,
    ) {
        Ok(()) => Ok(EXIT_OK),
        Err(error) => {
            writeln!(
                err,
                "Resumable vanilla-delegated plan batch generation failed: {error}"
            )?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_vanilla_delegated_plan_parallel_impl(
    out: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    format_text: &str,
    threads_text: &str,
    plan_csv: &str,
    optional_args: &[String],
) -> std::result::Result<(), String> {
    let total_start = Instant::now();
    let format = OutputFormat::parse(format_text).map_err(|error| error.to_string())?;
    let scale = parse_positive_i32_string("scale", scale_text)?;
    let threads = parse_positive_usize_string("threads", threads_text)?;
    let plan_path = Path::new(plan_csv);
    let plan_regions = read_region_plan_csv(plan_path)?;
    let (max_regions_this_run, generation_option_offset) =
        parse_plan_max_regions_this_run(optional_args)?;
    let generation_options = optional_generation_args_from_rest(
        optional_args.get(generation_option_offset..).unwrap_or(&[]),
    );
    let runtime_options =
        parse_generation_runtime_options(format, generation_options.extra_options)?;
    let status = ChunkGenerationStatus::parse(generation_options.status)
        .map_err(|error| error.to_string())?;
    if status == ChunkGenerationStatus::Full {
        return Err("delegated generation status must be surface or carvers".to_string());
    }

    let submitted_regions = plan_regions.len().min(max_regions_this_run);
    if submitted_regions == 0 {
        return Err("maxRegionsThisRun produced an empty submitted plan".to_string());
    }
    let heightmap = Path::new(heightmap_path);
    let world = Path::new(world_dir);
    let vertical_scale = runtime_options.vertical_scale.effective(scale);
    let surface_material_path =
        parse_optional_surface_material_path(generation_options.surface_raster, heightmap)?
            .ok_or_else(|| {
                "default textureMode=photo requires a TrueMarble surface raster; use surfaceRaster=auto or pass an explicit TrueMarble.vrt path"
                    .to_string()
            })?;
    let cache_rows = configured_heightmap_cache_rows(heightmap)?;
    let surface_tile_cache_entries =
        configured_surface_tile_cache_entries(Some(&surface_material_path))?;
    let resume_fingerprint = vanilla_delegated_plan_resume_fingerprint(
        heightmap,
        plan_path,
        &plan_regions,
        format,
        scale,
        status,
        &surface_material_path,
        runtime_options.compression,
        vertical_scale,
        runtime_options.vertical_scale.label(),
    );
    configure_surface_photo_rayon_threads(threads);
    write_progress_event(
        out,
        json!({
            "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
            "type": "workerTuningStarted",
            "mode": "plan",
            "requestedThreads": threads,
            "submittedRegions": submitted_regions,
            "candidateWorkerThreads": surface_photo_worker_candidates(threads, submitted_regions),
            "candidateWorkerConfigs": surface_photo_worker_candidate_configs(threads, submitted_regions)
                .into_iter()
                .map(surface_photo_worker_candidate_config_json)
                .collect::<Vec<_>>(),
            "minRegions": SURFACE_PHOTO_AUTOTUNE_MIN_REGIONS,
            "maxSamples": SURFACE_PHOTO_AUTOTUNE_MAX_SAMPLES,
        }),
    )?;
    let worker_tuning = tune_surface_photo_workers_for_plan(
        heightmap,
        world,
        format,
        scale,
        &plan_regions,
        submitted_regions,
        threads,
        status,
        vertical_scale,
        &surface_material_path,
        cache_rows,
        surface_tile_cache_entries,
        runtime_options.compression,
    );
    write_progress_event(
        out,
        json!({
            "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
            "type": "workerTuningFinished",
            "mode": "plan",
            "workerTuning": worker_tuning.to_progress_json(),
        }),
    )?;
    let worker_count = worker_tuning
        .selected_worker_count
        .min(submitted_regions)
        .max(1);
    let rayon_threads = worker_tuning.selected_rayon_threads.max(worker_count);
    let parallel_column_sampling = worker_tuning.parallel_column_sampling;

    let setup_start = Instant::now();
    std::fs::create_dir_all(world.join("region")).map_err(|error| error.to_string())?;
    let (spawn_region_x, spawn_region_z) = plan_spawn_region(&plan_regions);
    let spawn_x = spawn_region_x
        .wrapping_mul(REGION_SIZE_BLOCKS)
        .wrapping_add(REGION_SIZE_BLOCKS / 2);
    let spawn_z = spawn_region_z
        .wrapping_mul(REGION_SIZE_BLOCKS)
        .wrapping_add(REGION_SIZE_BLOCKS / 2);
    let level_settings = level_dat_template::Settings::new(
        "SR EarthMap Vanilla Delegated Plan",
        0,
        spawn_x,
        SEA_LEVEL_Y + 10,
        spawn_z,
    )
    .map_err(|error| error.to_string())?;
    level_dat_template::write(world.join("level.dat"), &level_settings)
        .map_err(|error| error.to_string())?;
    let manifest_file = write_vanilla_delegated_plan_manifest(
        world,
        heightmap,
        format,
        scale,
        plan_path,
        plan_regions.len(),
        submitted_regions,
        threads,
        status,
        &surface_material_path,
        runtime_options.compression,
        vertical_scale,
        runtime_options.vertical_scale.label(),
    )
    .map_err(|error| error.to_string())?;
    let resume = prepare_vanilla_delegated_resume_journal(world, &resume_fingerprint)?;
    let stale_temp_files = cleanup_stale_region_temp_files(&world.join("region"));
    let surface_material_sampler = EarthDataSurfaceMaterialSampler::open_with_tile_cache_entries(
        &surface_material_path,
        surface_tile_cache_entries,
    )
    .map_err(|error| error.to_string())?;
    let setup_millis = setup_start.elapsed().as_millis();

    write_progress_event(
        out,
        json!({
            "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
            "type": "batchStarted",
            "mode": "plan",
            "worldDir": normalized_path_display(world),
            "planCsv": normalized_path_display(plan_path),
            "format": format.java_name(),
            "scale": scale,
            "verticalScale": vertical_scale,
            "verticalScaleMode": runtime_options.vertical_scale.label(),
            "chunkStatus": status.id(),
            "plannedRegions": plan_regions.len(),
            "submittedRegions": submitted_regions,
            "requestedThreads": threads,
            "workerThreads": worker_count,
            "regionWorkerThreads": worker_count,
            "rayonThreads": rayon_threads,
            "parallelColumnSampling": parallel_column_sampling,
            "workerTuning": worker_tuning.to_progress_json(),
            "resumeFingerprintMatched": resume.fingerprint_matched,
            "resumeJournalRegions": resume.completed_regions.len(),
            "resumeJournal": normalized_path_display(&resume.path),
        }),
    )?;
    for warning in &resume.warnings {
        writeln!(out, "resumeJournalWarning={}", csv_cell(warning))
            .map_err(|error| error.to_string())?;
    }
    writeln!(
        out,
        "type,status,regionX,regionZ,elapsedMillis,chunks,outputBytes,regionFile,message"
    )
    .map_err(|error| error.to_string())?;

    let regions = plan_regions
        .iter()
        .take(submitted_regions)
        .copied()
        .collect::<VecDeque<_>>();
    let region_queue = Mutex::new(regions);
    let stop_queueing = AtomicBool::new(false);
    let resume_completed_regions = Arc::new(resume.completed_regions);
    let resume_journal = resume.journal;
    let (event_sender, event_receiver) =
        mpsc::sync_channel::<VanillaDelegatedParallelEvent>(PARALLEL_EVENT_CHANNEL_CAPACITY);
    let mut stats = VanillaDelegatedParallelBatchStats::default();
    let generation_pool = build_surface_photo_rayon_pool(rayon_threads)?;
    std::thread::scope(|scope| {
        let coordinator_sender = event_sender.clone();
        let generation_pool = &generation_pool;
        let region_queue = &region_queue;
        let stop_queueing = &stop_queueing;
        let surface_material_sampler = &surface_material_sampler;
        let surface_material_path = &surface_material_path;
        let resume_completed_regions = &resume_completed_regions;
        let resume_journal = &resume_journal;
        let coordinator = scope.spawn(move || {
            generation_pool.scope(|rayon_scope| {
                for _ in 0..worker_count {
                    let queue = region_queue;
                    let stop_queueing = stop_queueing;
                    let surface_material_sampler = surface_material_sampler;
                    let surface_material_path = surface_material_path;
                    let completed_regions = Arc::clone(&resume_completed_regions);
                    let journal = Arc::clone(resume_journal);
                    let sender = coordinator_sender.clone();
                    let fingerprint_matched = resume.fingerprint_matched;
                    rayon_scope.spawn(move |_| loop {
                        if stop_queueing.load(Ordering::SeqCst) {
                            break;
                        }
                        let next_region = {
                            let mut queue = queue.lock().expect("queue lock");
                            if stop_queueing.load(Ordering::SeqCst) {
                                None
                            } else {
                                queue.pop_front()
                            }
                        };
                        let Some((region_x, region_z)) = next_region else {
                            break;
                        };
                        let region_file =
                            vanilla_delegated_region_file(world, format, region_x, region_z);
                        if fingerprint_matched && completed_regions.contains(&(region_x, region_z))
                        {
                            let skip_start = Instant::now();
                            if let Ok(validation) = validate_region_file_for_resume(
                                &region_file,
                                region_format_for_output(format),
                                region_x,
                                region_z,
                                REGION_CHUNKS_PER_REGION,
                            ) {
                                if sender
                                    .send(VanillaDelegatedParallelEvent::RegionSkipped {
                                        region_x,
                                        region_z,
                                        elapsed_millis: skip_start.elapsed().as_millis(),
                                        chunks: validation.chunk_count,
                                        output_bytes: validation.file_bytes,
                                        region_file,
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                        }
                        if sender
                            .send(VanillaDelegatedParallelEvent::RegionStarted {
                                region_x,
                                region_z,
                                region_file: region_file.clone(),
                            })
                            .is_err()
                        {
                            break;
                        };
                        let region_start = Instant::now();
                        let result = (|| -> std::result::Result<SurfaceRegionReport, String> {
                            let mut settings = SurfaceRegionSettings::new_with_texture_options(
                                heightmap_path,
                                world_dir,
                                "SR EarthMap Vanilla Delegated Plan",
                                0,
                                scale,
                                region_x,
                                region_z,
                                format,
                                cache_rows,
                                false,
                                status,
                                vertical_scale,
                                SurfaceTextureMode::Photo,
                            )
                            .map_err(|error| error.to_string())?;
                            settings.surface_material_path = Some((*surface_material_path).clone());
                            settings.surface_tile_cache_entries = surface_tile_cache_entries;
                            settings.parallel_column_sampling = parallel_column_sampling;
                            apply_region_compression_options(
                                &mut settings,
                                runtime_options.compression,
                            );
                            let report = generate_surface_region_with_open_material_sampler(
                                &settings,
                                Some(surface_material_sampler),
                            )
                            .map_err(|error| error.to_string())?;
                            let output_bytes = std::fs::metadata(&report.region_file)
                                .map(|metadata| metadata.len())
                                .unwrap_or(0);
                            journal
                                .lock()
                                .map_err(|_| "resume journal lock poisoned".to_string())?
                                .append_region_complete(
                                    report.region_x,
                                    report.region_z,
                                    format,
                                    report.chunk_count,
                                    output_bytes,
                                )?;
                            Ok(report)
                        })();
                        match result {
                            Ok(report) => {
                                let output_bytes = std::fs::metadata(&report.region_file)
                                    .map(|metadata| metadata.len())
                                    .unwrap_or(0);
                                if sender
                                    .send(VanillaDelegatedParallelEvent::RegionGenerated {
                                        elapsed_millis: region_start.elapsed().as_millis(),
                                        output_bytes,
                                        report,
                                        prefetch_timing: None,
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(message) => {
                                stop_queueing.store(true, Ordering::SeqCst);
                                let _ = sender.send(VanillaDelegatedParallelEvent::RegionFailed {
                                    region_x,
                                    region_z,
                                    elapsed_millis: region_start.elapsed().as_millis(),
                                    region_file,
                                    message,
                                });
                                break;
                            }
                        }
                    });
                }
            });
        });
        drop(event_sender);
        while let Ok(event) = event_receiver.recv() {
            handle_vanilla_delegated_parallel_event(out, &mut stats, event)?;
        }
        coordinator
            .join()
            .map_err(|_| "parallel plan worker coordinator panicked".to_string())?;
        Ok::<(), String>(())
    })?;
    if let Ok(journal) = resume_journal.lock() {
        journal.sync()?;
    }
    let elapsed_millis = total_start.elapsed().as_millis();
    let generated_regions_per_hour = if elapsed_millis == 0 {
        0.0
    } else {
        (stats.generated_regions as f64) * 3_600_000.0 / (elapsed_millis as f64)
    };
    let completed_regions = stats.generated_regions + stats.skipped_regions;
    let completed_regions_per_hour = if elapsed_millis == 0 {
        0.0
    } else {
        (completed_regions as f64) * 3_600_000.0 / (elapsed_millis as f64)
    };
    let remaining_regions = plan_regions.len().saturating_sub(completed_regions);
    write_progress_event(
        out,
        json!({
            "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
            "type": "batchSummary",
            "mode": "plan",
            "elapsedMillis": u128_to_u64(elapsed_millis),
            "plannedRegions": plan_regions.len(),
            "submittedRegions": submitted_regions,
            "generatedRegions": stats.generated_regions,
            "skippedRegions": stats.skipped_regions,
            "completedRegions": completed_regions,
            "failedRegions": stats.failed_regions,
            "remainingRegions": remaining_regions,
            "allDone": stats.failed_regions == 0 && remaining_regions == 0,
            "generatedRegionsPerHour": generated_regions_per_hour,
            "completedRegionsPerHour": completed_regions_per_hour,
        }),
    )?;
    let lines = [
        "Resumable vanilla-delegated plan batch complete".to_string(),
        format!("worldDir={}", world.display()),
        format!("planCsv={}", normalized_path_display(plan_path)),
        format!("format={}", format.java_name()),
        format!("scale=1:{scale}"),
        format!("chunkStatus={}", status.id()),
        format!("plannedRegions={}", plan_regions.len()),
        format!("submittedRegions={submitted_regions}"),
        format!("requestedThreads={threads}"),
        format!("workerThreads={worker_count}"),
        format!("regionWorkerThreads={worker_count}"),
        format!("rayonThreads={rayon_threads}"),
        format!("parallelColumnSampling={parallel_column_sampling}"),
        format!("workerTuningMode={}", worker_tuning.mode),
        format!("workerTuningSamples={}", worker_tuning.sample_count),
        format!("workerTuningLandSamples={}", worker_tuning.land_samples),
        format!("workerTuningOceanSamples={}", worker_tuning.ocean_samples),
        format!("workerTuningMixedSamples={}", worker_tuning.mixed_samples),
        format!("surfaceSamplerStrategy=shared"),
        format!("sharedCacheRows={cache_rows}"),
        format!("surfaceTileCacheEntries={surface_tile_cache_entries}"),
        format!("verticalScale={vertical_scale}"),
        format!(
            "verticalScaleMode={}",
            runtime_options.vertical_scale.label()
        ),
        compression_options_report_line(format, runtime_options.compression),
        format!(
            "surfaceMaterialPath={}",
            normalized_path_display(&surface_material_path)
        ),
        format!("resumeJournal={}", resume.path.display()),
        format!("resumeFingerprintMatched={}", resume.fingerprint_matched),
        format!("resumeJournalRegions={}", resume_completed_regions.len()),
        format!("staleTempRegionFilesRemoved={stale_temp_files}"),
        format!("setupMillis={setup_millis}"),
        format!(
            "batchPhaseSummary.surfaceSampleMillis={}",
            stats.phase_surface_sample_millis
        ),
        format!(
            "batchPhaseSummary.chunkBuildMillis={}",
            stats.phase_chunk_build_millis
        ),
        format!(
            "batchPhaseSummary.nbtEncodeMillis={}",
            stats.phase_nbt_encode_millis
        ),
        format!(
            "batchPhaseSummary.regionWriteMillis={}",
            stats.phase_region_write_millis
        ),
        format!(
            "batchPhaseSummary.metadataMillis={}",
            stats.phase_metadata_millis
        ),
        format!(
            "batchPhaseSummary.totalInternalMillis={}",
            stats.phase_total_internal_millis
        ),
        format!("elapsedMillis={elapsed_millis}"),
        format!("generatedRegions={}", stats.generated_regions),
        format!("skippedRegions={}", stats.skipped_regions),
        format!("completedRegions={completed_regions}"),
        format!("failedRegions={}", stats.failed_regions),
        format!("remainingRegions={remaining_regions}"),
        format!("generatedRegionsPerHour={generated_regions_per_hour:.2}"),
        format!("completedRegionsPerHour={completed_regions_per_hour:.2}"),
        format!(
            "allDone={}",
            stats.failed_regions == 0 && remaining_regions == 0
        ),
        format!("manifestFile={}", manifest_file.display()),
    ];
    for line in lines {
        writeln!(out, "{line}").map_err(|error| error.to_string())?;
    }
    if !stats.errors.is_empty() {
        return Err(stats.errors.join("; "));
    }
    Ok(())
}

fn parse_plan_max_regions_this_run(
    optional_args: &[String],
) -> std::result::Result<(usize, usize), String> {
    let Some(first) = optional_args.first() else {
        return Ok((usize::MAX, 0));
    };
    if first.contains('=') {
        return Ok((usize::MAX, 0));
    }
    match first.parse::<usize>() {
        Ok(0) => Err("maxRegionsThisRun must be positive".to_string()),
        Ok(value) => Ok((value, 1)),
        Err(_) => Ok((usize::MAX, 0)),
    }
}

fn read_region_plan_csv(plan_csv: &Path) -> std::result::Result<Vec<(i32, i32)>, String> {
    let text = std::fs::read_to_string(plan_csv).map_err(|error| error.to_string())?;
    let mut regions = Vec::new();
    for line in text.lines() {
        let trimmed = strip_utf8_bom(line).trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let columns = split_csv_line(trimmed)?;
        if columns.len() < 2 {
            return Err(format!("invalid region plan row: {line}"));
        }
        if !is_i32_text(&columns[0])
            && columns
                .iter()
                .any(|column| normalize_csv_header(column).contains("region"))
        {
            continue;
        }
        let (region_x_text, region_z_text) = if columns.len() >= 4 && is_i32_text(&columns[0]) {
            (&columns[1], &columns[2])
        } else {
            (&columns[0], &columns[1])
        };
        let region_x = region_x_text
            .parse::<i32>()
            .map_err(|error| format!("invalid regionX in plan row {line}: {error}"))?;
        let region_z = region_z_text
            .parse::<i32>()
            .map_err(|error| format!("invalid regionZ in plan row {line}: {error}"))?;
        regions.push((region_x, region_z));
    }
    if regions.is_empty() {
        return Err(format!("region plan is empty: {}", plan_csv.display()));
    }
    Ok(regions)
}

fn is_i32_text(text: &str) -> bool {
    text.parse::<i32>().is_ok()
}

fn plan_spawn_region(regions: &[(i32, i32)]) -> (i32, i32) {
    let min_x = regions.iter().map(|(x, _)| *x).min().unwrap_or(0);
    let max_x = regions.iter().map(|(x, _)| *x).max().unwrap_or(0);
    let min_z = regions.iter().map(|(_, z)| *z).min().unwrap_or(0);
    let max_z = regions.iter().map(|(_, z)| *z).max().unwrap_or(0);
    (
        min_x.wrapping_add(max_x.wrapping_sub(min_x) / 2),
        min_z.wrapping_add(max_z.wrapping_sub(min_z) / 2),
    )
}

#[allow(clippy::too_many_arguments)]
fn vanilla_delegated_plan_resume_fingerprint(
    heightmap_path: &Path,
    plan_csv: &Path,
    regions: &[(i32, i32)],
    format: OutputFormat,
    scale: i32,
    status: ChunkGenerationStatus,
    surface_material_path: &Path,
    compression_options: RegionCompressionOptions,
    vertical_scale: f64,
    vertical_scale_mode: &str,
) -> Value {
    json!({
        "schemaVersion": RESUME_FINGERPRINT_SCHEMA_VERSION,
        "generator": {
            "name": build_info::NAME,
            "version": build_info::VERSION,
            "minecraftTarget": build_info::MINECRAFT_TARGET,
            "rustPortPhase": build_info::RUST_PORT_PHASE,
        },
        "command": "generate-vanilla-delegated-plan-parallel",
        "format": format.java_name(),
        "scale": scale,
        "planCsv": file_identity_for_resume(plan_csv),
        "planRegions": regions.iter().map(|(x, z)| json!({"regionX": x, "regionZ": z})).collect::<Vec<_>>(),
        "chunkStatus": status.id(),
        "textureMode": "photo",
        "surfaceSamplingProfile": SURFACE_SAMPLING_PROFILE_VERSION,
        "verticalScale": vertical_scale,
        "verticalScaleMode": vertical_scale_mode,
        "serverDelegation": true,
        "directCaves": false,
        "directOres": false,
        "directVegetation": false,
        "directStructures": false,
        "directProgressionStructures": false,
        "heightmap": file_identity_for_resume(heightmap_path),
        "surfaceMaterial": file_identity_for_resume(surface_material_path),
        "surfaceInputs": surface_input_identities_for_resume(surface_material_path),
        "compression": {
            "mca": compression_options.mca_compression_level,
            "linear": compression_options.linear_compression_level,
        },
    })
}

#[derive(Debug)]
enum VanillaDelegatedParallelEvent {
    RegionStarted {
        region_x: i32,
        region_z: i32,
        region_file: std::path::PathBuf,
    },
    RegionSkipped {
        region_x: i32,
        region_z: i32,
        elapsed_millis: u128,
        chunks: usize,
        output_bytes: u64,
        region_file: std::path::PathBuf,
    },
    RegionGenerated {
        elapsed_millis: u128,
        output_bytes: u64,
        report: SurfaceRegionReport,
        prefetch_timing: Option<PrefetchTimingMillis>,
    },
    RegionFailed {
        region_x: i32,
        region_z: i32,
        elapsed_millis: u128,
        region_file: std::path::PathBuf,
        message: String,
    },
}

#[derive(Debug)]
struct PreparedVanillaDelegatedRegion {
    region_x: i32,
    region_z: i32,
    region_file: PathBuf,
    started_at: Instant,
    prepared_at: Instant,
    prefetch_send_wait_millis: u128,
    sample: PreparedSurfaceRegionSample,
}

#[derive(Clone, Copy, Debug, Default)]
struct PrefetchTimingMillis {
    send_wait: u128,
    ready_queue_wait: u128,
    consumer_pool_wait: u128,
    consumer_elapsed: u128,
}

#[derive(Default)]
struct VanillaDelegatedParallelBatchStats {
    generated_regions: usize,
    skipped_regions: usize,
    failed_regions: usize,
    phase_surface_sample_millis: u128,
    phase_chunk_build_millis: u128,
    phase_nbt_encode_millis: u128,
    phase_region_write_millis: u128,
    phase_metadata_millis: u128,
    phase_total_internal_millis: u128,
    errors: Vec<String>,
}

fn send_prepared_vanilla_region(
    sender: &mpsc::SyncSender<PreparedVanillaDelegatedRegion>,
    stop_queueing: &AtomicBool,
    mut prepared: PreparedVanillaDelegatedRegion,
) -> bool {
    let wait_start = Instant::now();
    loop {
        if stop_queueing.load(Ordering::SeqCst) {
            return false;
        }
        prepared.prefetch_send_wait_millis = wait_start.elapsed().as_millis();
        match sender.try_send(prepared) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Full(returned)) => {
                prepared = returned;
                std::thread::sleep(Duration::from_millis(PREFETCH_SEND_RETRY_MILLIS));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn handle_vanilla_delegated_parallel_event(
    out: &mut impl Write,
    stats: &mut VanillaDelegatedParallelBatchStats,
    event: VanillaDelegatedParallelEvent,
) -> std::result::Result<(), String> {
    match event {
        VanillaDelegatedParallelEvent::RegionStarted {
            region_x,
            region_z,
            region_file,
        } => {
            write_progress_event(
                out,
                json!({
                    "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
                    "type": "regionStarted",
                    "regionX": region_x,
                    "regionZ": region_z,
                    "regionFile": normalized_path_display(&region_file),
                }),
            )?;
        }
        VanillaDelegatedParallelEvent::RegionSkipped {
            region_x,
            region_z,
            elapsed_millis,
            chunks,
            output_bytes,
            region_file,
        } => {
            stats.skipped_regions += 1;
            write_progress_event(
                out,
                json!({
                    "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
                    "type": "regionSkipped",
                    "regionX": region_x,
                    "regionZ": region_z,
                    "elapsedMillis": u128_to_u64(elapsed_millis),
                    "chunks": chunks,
                    "outputBytes": output_bytes,
                    "regionFile": normalized_path_display(&region_file),
                    "reason": "validResume",
                }),
            )?;
            writeln!(
                out,
                "region,skipped,{region_x},{region_z},{elapsed_millis},{chunks},{output_bytes},{},validResume",
                csv_cell(&region_file.display().to_string())
            )
            .map_err(|error| error.to_string())?;
        }
        VanillaDelegatedParallelEvent::RegionGenerated {
            elapsed_millis,
            output_bytes,
            report,
            prefetch_timing,
        } => {
            stats.generated_regions += 1;
            stats.phase_surface_sample_millis += millis(report.surface_sample_nanos);
            stats.phase_chunk_build_millis += millis(report.chunk_build_nanos);
            stats.phase_nbt_encode_millis += millis(report.nbt_encode_nanos);
            stats.phase_region_write_millis += millis(report.region_write_nanos);
            stats.phase_metadata_millis += millis(report.metadata_nanos);
            stats.phase_total_internal_millis += millis(report.total_nanos);
            let mut event = json!({
                "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
                "type": "regionGenerated",
                "regionX": report.region_x,
                "regionZ": report.region_z,
                "elapsedMillis": u128_to_u64(elapsed_millis),
                "chunks": report.chunk_count,
                "landColumns": report.land_columns,
                "waterColumns": report.water_columns,
                "minGroundY": report.min_ground_y,
                "maxGroundY": report.max_ground_y,
                "surfaceSampleMillis": u128_to_u64(millis(report.surface_sample_nanos)),
                "surfacePhase.elevationFillMillis": u128_to_u64(millis(report.sample_phase_nanos.elevation_fill)),
                "surfacePhase.waterMaskMillis": u128_to_u64(millis(report.sample_phase_nanos.water_mask)),
                "surfacePhase.coastFactorMillis": u128_to_u64(millis(report.sample_phase_nanos.coast_factor)),
                "surfacePhase.smoothPrecomputeMillis": u128_to_u64(millis(report.sample_phase_nanos.smooth_precompute)),
                "surfacePhase.reliefPrecomputeMillis": u128_to_u64(millis(report.sample_phase_nanos.relief_precompute)),
                "surfacePhase.openOceanFastPathMillis": u128_to_u64(millis(report.sample_phase_nanos.open_ocean_fast_path)),
                "surfacePhase.openOceanUniformCheckMillis": u128_to_u64(millis(report.sample_phase_nanos.open_ocean_uniform_check)),
                "surfacePhase.openOceanCompanionPrecomputeMillis": u128_to_u64(millis(report.sample_phase_nanos.open_ocean_companion_precompute)),
                "surfacePhase.openOceanColumnBuildMillis": u128_to_u64(millis(report.sample_phase_nanos.open_ocean_column_build)),
                "surfacePhase.openOceanRepeatedExpandMillis": u128_to_u64(millis(report.sample_phase_nanos.open_ocean_repeated_expand)),
                "surfacePhase.coordinatePrecomputeMillis": u128_to_u64(millis(report.sample_phase_nanos.coordinate_precompute)),
                "surfacePhase.photoLandPrecomputeMillis": u128_to_u64(millis(report.sample_phase_nanos.photo_land_precompute)),
                "surfacePhase.columnBuildMillis": u128_to_u64(millis(report.sample_phase_nanos.column_build)),
                "surfacePhase.photoProfileMillis": u128_to_u64(millis(report.sample_phase_nanos.photo_profile)),
                "surfacePhase.photoApplyMillis": u128_to_u64(millis(report.sample_phase_nanos.photo_apply)),
                "surfacePhase.postProcessMillis": u128_to_u64(millis(report.sample_phase_nanos.post_process)),
                "surfacePhase.sampledMaterialColumns": report.sample_phase_nanos.sampled_material_columns,
                "surfacePhase.sampledLandMaterialColumns": report.sample_phase_nanos.sampled_land_material_columns,
                "surfacePhase.sampledWaterMaterialColumns": report.sample_phase_nanos.sampled_water_material_columns,
                "chunkBuildMillis": u128_to_u64(millis(report.chunk_build_nanos)),
                "nbtEncodeMillis": u128_to_u64(millis(report.nbt_encode_nanos)),
                "regionWriteMillis": u128_to_u64(millis(report.region_write_nanos)),
                "metadataMillis": u128_to_u64(millis(report.metadata_nanos)),
                "totalInternalMillis": u128_to_u64(millis(report.total_nanos)),
                "prefetchSendWaitMillis": prefetch_timing.map(|timing| u128_to_u64(timing.send_wait)),
                "prefetchReadyQueueWaitMillis": prefetch_timing.map(|timing| u128_to_u64(timing.ready_queue_wait)),
                "consumerPoolWaitMillis": prefetch_timing.map(|timing| u128_to_u64(timing.consumer_pool_wait)),
                "consumerElapsedMillis": prefetch_timing.map(|timing| u128_to_u64(timing.consumer_elapsed)),
                "outputBytes": output_bytes,
                "regionFile": normalized_path_display(&report.region_file),
            });
            if let Some(object) = event.as_object_mut() {
                object.insert(
                    "surfaceMaterialRaster.sourceCount".to_string(),
                    json!(report.surface_material_raster_stats.source_count),
                );
                object.insert(
                    "surfaceMaterialRaster.openReaders".to_string(),
                    json!(report.surface_material_raster_stats.open_readers),
                );
                object.insert(
                    "surfaceMaterialRaster.residentTiles".to_string(),
                    json!(report.surface_material_raster_stats.resident_tiles),
                );
                object.insert(
                    "surfaceMaterialRaster.tileHits".to_string(),
                    json!(report.surface_material_raster_stats.tile_hits),
                );
                object.insert(
                    "surfaceMaterialRaster.tileMisses".to_string(),
                    json!(report.surface_material_raster_stats.tile_misses),
                );
                object.insert(
                    "surfaceMaterialRaster.tileEvictions".to_string(),
                    json!(report.surface_material_raster_stats.tile_evictions),
                );
                object.insert(
                    "surfaceMaterialRaster.sampleNearestRequests".to_string(),
                    json!(report.surface_material_raster_stats.sample_nearest_requests),
                );
                object.insert(
                    "surfaceMaterialRaster.sampleAveragedRequests".to_string(),
                    json!(
                        report
                            .surface_material_raster_stats
                            .sample_averaged_requests
                    ),
                );
            }
            write_progress_event(out, event)?;
            writeln!(
                out,
                "region,generated,{},{},{},{},{},{},",
                report.region_x,
                report.region_z,
                elapsed_millis,
                report.chunk_count,
                output_bytes,
                csv_cell(&report.region_file.display().to_string())
            )
            .map_err(|error| error.to_string())?;
        }
        VanillaDelegatedParallelEvent::RegionFailed {
            region_x,
            region_z,
            elapsed_millis,
            region_file,
            message,
        } => {
            stats.failed_regions += 1;
            stats
                .errors
                .push(format!("region {region_x},{region_z}: {message}"));
            write_progress_event(
                out,
                json!({
                    "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
                    "type": "regionFailed",
                    "regionX": region_x,
                    "regionZ": region_z,
                    "elapsedMillis": u128_to_u64(elapsed_millis),
                    "regionFile": normalized_path_display(&region_file),
                    "message": message,
                }),
            )?;
            writeln!(
                out,
                "region,failed,{region_x},{region_z},{elapsed_millis},0,0,{},{}",
                csv_cell(&region_file.display().to_string()),
                csv_cell(&message)
            )
            .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn write_progress_event(out: &mut impl Write, value: Value) -> std::result::Result<(), String> {
    writeln!(out, "event\t{value}").map_err(|error| error.to_string())
}

struct PreparedVanillaDelegatedResume {
    fingerprint_matched: bool,
    completed_regions: HashSet<(i32, i32)>,
    journal: Arc<Mutex<VanillaDelegatedResumeJournal>>,
    path: std::path::PathBuf,
    warnings: Vec<String>,
}

struct VanillaDelegatedResumeJournal {
    file: File,
}

impl VanillaDelegatedResumeJournal {
    fn append_region_complete(
        &mut self,
        region_x: i32,
        region_z: i32,
        format: OutputFormat,
        chunks: usize,
        output_bytes: u64,
    ) -> std::result::Result<(), String> {
        let line = json!({
            "schemaVersion": RESUME_FINGERPRINT_SCHEMA_VERSION,
            "type": "regionComplete",
            "regionX": region_x,
            "regionZ": region_z,
            "format": format.java_name(),
            "chunks": chunks,
            "outputBytes": output_bytes,
        });
        writeln!(self.file, "{line}").map_err(|error| error.to_string())
    }

    fn sync(&self) -> std::result::Result<(), String> {
        self.file.sync_data().map_err(|error| error.to_string())
    }
}

fn prepare_vanilla_delegated_resume_journal(
    world: &Path,
    fingerprint: &Value,
) -> std::result::Result<PreparedVanillaDelegatedResume, String> {
    let path = world.join(VANILLA_DELEGATED_RESUME_JOURNAL_FILE_NAME);
    let mut warnings = Vec::new();
    let loaded = match load_vanilla_delegated_resume_journal(&path, fingerprint) {
        Ok(loaded) => loaded,
        Err(error) => {
            warnings.push(error);
            None
        }
    };
    let (fingerprint_matched, completed_regions, file) = if let Some(completed_regions) = loaded {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        (true, completed_regions, file)
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let mut file = File::create(&path).map_err(|error| error.to_string())?;
        write_resume_fingerprint_header(&mut file, fingerprint)?;
        file.sync_data().map_err(|error| error.to_string())?;
        (false, HashSet::new(), file)
    };
    Ok(PreparedVanillaDelegatedResume {
        fingerprint_matched,
        completed_regions,
        journal: Arc::new(Mutex::new(VanillaDelegatedResumeJournal { file })),
        path,
        warnings,
    })
}

fn load_vanilla_delegated_resume_journal(
    path: &Path,
    fingerprint: &Value,
) -> std::result::Result<Option<HashSet<(i32, i32)>>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to read resume journal: {error}")),
    };
    let mut active_fingerprint_matches = false;
    let mut saw_fingerprint = false;
    let mut completed_regions = HashSet::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(line).map_err(|error| {
            format!(
                "resume journal parse error at line {}: {error}",
                line_index + 1
            )
        })?;
        match value.get("type").and_then(Value::as_str) {
            Some("fingerprint") => {
                saw_fingerprint = true;
                active_fingerprint_matches = value.get("fingerprint") == Some(fingerprint);
                completed_regions.clear();
            }
            Some("regionComplete") if active_fingerprint_matches => {
                let Some(region_x) = value
                    .get("regionX")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                else {
                    continue;
                };
                let Some(region_z) = value
                    .get("regionZ")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                else {
                    continue;
                };
                completed_regions.insert((region_x, region_z));
            }
            _ => {}
        }
    }
    if saw_fingerprint && active_fingerprint_matches {
        Ok(Some(completed_regions))
    } else {
        Ok(None)
    }
}

fn write_resume_fingerprint_header(
    file: &mut File,
    fingerprint: &Value,
) -> std::result::Result<(), String> {
    let line = json!({
        "schemaVersion": RESUME_FINGERPRINT_SCHEMA_VERSION,
        "type": "fingerprint",
        "fingerprint": fingerprint,
    });
    writeln!(file, "{line}").map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
fn vanilla_delegated_parallel_resume_fingerprint(
    heightmap_path: &Path,
    format: OutputFormat,
    scale: i32,
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
    status: ChunkGenerationStatus,
    surface_material_path: &Path,
    compression_options: RegionCompressionOptions,
    vertical_scale: f64,
    vertical_scale_mode: &str,
) -> Value {
    json!({
        "schemaVersion": RESUME_FINGERPRINT_SCHEMA_VERSION,
        "generator": {
            "name": build_info::NAME,
            "version": build_info::VERSION,
            "minecraftTarget": build_info::MINECRAFT_TARGET,
            "rustPortPhase": build_info::RUST_PORT_PHASE,
        },
        "command": "generate-vanilla-delegated-regions-parallel",
        "format": format.java_name(),
        "scale": scale,
        "startRegionX": start_region_x,
        "startRegionZ": start_region_z,
        "regionCols": cols,
        "regionRows": rows,
        "chunkStatus": status.id(),
        "textureMode": "photo",
        "surfaceSamplingProfile": SURFACE_SAMPLING_PROFILE_VERSION,
        "verticalScale": vertical_scale,
        "verticalScaleMode": vertical_scale_mode,
        "serverDelegation": true,
        "directCaves": false,
        "directOres": false,
        "directVegetation": false,
        "directStructures": false,
        "directProgressionStructures": false,
        "heightmap": file_identity_for_resume(heightmap_path),
        "surfaceMaterial": file_identity_for_resume(surface_material_path),
        "surfaceInputs": surface_input_identities_for_resume(surface_material_path),
        "compression": {
            "mca": compression_options.mca_compression_level,
            "linear": compression_options.linear_compression_level,
        },
    })
}

fn surface_input_identities_for_resume(surface_material_path: &Path) -> Value {
    let mut inputs = Vec::new();
    push_resume_input_identity(&mut inputs, "terrain.trueMarble", surface_material_path);

    let normalized = normalized_path(surface_material_path);
    let terrain_dir = normalized.parent().map(Path::to_path_buf);
    let tif_root = terrain_dir
        .as_ref()
        .and_then(|path| path.parent())
        .map(Path::to_path_buf);
    if let Some(tif_root) = &tif_root {
        for (label, path) in [
            ("tifRoot.climate", tif_root.join("climate.tif")),
            (
                "tifRoot.oceanTemperature",
                tif_root.join("ocean_temp_infill.tif"),
            ),
            ("tifRoot.bathymetry", tif_root.join("bathymetry.tif")),
            ("tifRoot.slope", tif_root.join("slope.tif")),
            (
                "tifRoot.landShallowTopoWest",
                tif_root.join("land_shallow_topo_west.tif"),
            ),
            (
                "tifRoot.landShallowTopoEast",
                tif_root.join("land_shallow_topo_east.tif"),
            ),
        ] {
            push_resume_input_identity(&mut inputs, label, &path);
        }
        let vegetation = tif_root.join("vegetation");
        for file_name in [
            "EvergreenBroadleafTrees.tif",
            "DeciduousBroadleafTrees.tif",
            "EvergreenDeciduousNeedleleafTrees.tif",
            "mixed.tif",
            "HerbaceousVegetation.tif",
            "Shrubs.tif",
            "Snow.tif",
            "Swamp.tif",
        ] {
            push_resume_input_identity(
                &mut inputs,
                &format!("vegetation.{file_name}"),
                &vegetation.join(file_name),
            );
        }
    }

    for root in earth_data_candidate_roots(surface_material_path) {
        push_resume_input_identity(&mut inputs, "earthData.root", &root);
        let shape = root
            .join("ShapeFiles")
            .join("ecoregionsOrig")
            .join("wwf_terr_ecos.shp");
        push_resume_input_identity(&mut inputs, "ecoregions.shape", &shape);
        push_resume_input_identity(&mut inputs, "ecoregions.dbf", &shape.with_extension("dbf"));
        push_resume_input_identity(
            &mut inputs,
            "ecoregions.mapping",
            &root.join("ecoregions.csv"),
        );
        collect_met_resume_identities(&mut inputs, &root);
    }

    Value::Array(inputs)
}

fn earth_data_candidate_roots(surface_material_path: &Path) -> Vec<std::path::PathBuf> {
    let mut roots = Vec::with_capacity(4);
    if let Some(root) = configured_path_from_env(DATA_ROOT_ENV) {
        push_distinct_resume_path(&mut roots, normalized_path(&root));
    }
    if let Some(root) = configured_path_from_env(TIF_ROOT_ENV) {
        if let Some(parent) = normalized_path(&root).parent() {
            push_distinct_resume_path(&mut roots, parent.to_path_buf());
        }
    }
    let normalized = normalized_path(surface_material_path);
    if let Some(root) = normalized
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
    {
        push_distinct_resume_path(&mut roots, root.to_path_buf());
    }
    roots
}

fn push_distinct_resume_path(paths: &mut Vec<std::path::PathBuf>, path: std::path::PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn collect_met_resume_identities(inputs: &mut Vec<Value>, root: &Path) {
    collect_met_image_export_resume_identities(inputs, &root.join("image_exports"));
    collect_met_image_export_resume_identities(
        inputs,
        &root.join("met_work").join("image_exports"),
    );
    collect_nested_met_resume_identities(inputs, &root.join("met_shards"));
    collect_nested_met_resume_identities(inputs, &root.join("met_turbo_temp"));
}

fn collect_nested_met_resume_identities(inputs: &mut Vec<Value>, parent: &Path) {
    push_resume_input_identity(inputs, "met.nestedParent", parent);
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let mut children = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    children.sort();
    for child in children {
        collect_met_image_export_resume_identities(inputs, &child.join("image_exports"));
    }
}

fn collect_met_image_export_resume_identities(inputs: &mut Vec<Value>, image_exports: &Path) {
    push_resume_input_identity(inputs, "met.imageExports", image_exports);
    let Ok(entries) = std::fs::read_dir(image_exports) else {
        return;
    };
    let mut tile_dirs = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    tile_dirs.sort();
    for tile_dir in tile_dirs {
        push_resume_input_identity(inputs, "met.tileDir", &tile_dir);
        let Some(tile_name) = tile_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        for candidate in [
            tile_dir.join(format!("{tile_name}_terrain_reduced_colors.png")),
            tile_dir.join("terrain_reduced_colors.png"),
            tile_dir.join(format!("{tile_name}_terrain.png")),
            tile_dir.join("terrain.png"),
            tile_dir
                .join("heightmap")
                .join(format!("{tile_name}_exported.png")),
            tile_dir
                .join("heightmap")
                .join(format!("{tile_name}_exported.png.aux.xml")),
        ] {
            if candidate.is_file() {
                push_resume_input_identity(inputs, "met.tileFile", &candidate);
            }
        }
    }
}

fn push_resume_input_identity(inputs: &mut Vec<Value>, label: &str, path: &Path) {
    inputs.push(json!({
        "label": label,
        "identity": file_identity_for_resume(path),
    }));
}

fn file_identity_for_resume(path: &Path) -> Value {
    let normalized_path = normalized_path_display(path);
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => json!({
            "path": normalized_path,
            "exists": true,
            "kind": "directory",
        }),
        Ok(metadata) => json!({
            "path": normalized_path,
            "exists": true,
            "kind": "file",
            "len": metadata.len(),
            "modifiedMillis": metadata.modified().ok().and_then(system_time_millis),
        }),
        Err(error) => json!({
            "path": normalized_path,
            "exists": false,
            "errorKind": format!("{:?}", error.kind()),
        }),
    }
}

fn system_time_millis(time: std::time::SystemTime) -> Option<u64> {
    let duration = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(duration.as_millis().try_into().unwrap_or(u64::MAX))
}

fn cleanup_stale_region_temp_files(region_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(region_dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let is_region_temp = file_name.starts_with(".r.")
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"));
        if is_region_temp && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn vanilla_delegated_region_file(
    world: &Path,
    format: OutputFormat,
    region_x: i32,
    region_z: i32,
) -> std::path::PathBuf {
    world.join("region").join(format!(
        "r.{region_x}.{region_z}.{}",
        region_format_for_output(format).extension()
    ))
}

fn region_format_for_output(format: OutputFormat) -> RegionFormat {
    match format {
        OutputFormat::Mca => RegionFormat::Mca,
        OutputFormat::LinearV2 => RegionFormat::Linear,
    }
}

#[allow(clippy::too_many_arguments)]
fn trace_surface_region_column(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    local_x_text: &str,
    local_z_text: &str,
    surface_raster_text: Option<&str>,
) -> io::Result<i32> {
    match trace_surface_region_column_impl(
        heightmap_path,
        scale_text,
        region_x_text,
        region_z_text,
        local_x_text,
        local_z_text,
        surface_raster_text.unwrap_or("surfaceRaster=auto"),
    ) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Surface region column trace failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn trace_surface_region_column_impl(
    heightmap_path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    local_x_text: &str,
    local_z_text: &str,
    surface_raster_text: &str,
) -> std::result::Result<Vec<String>, String> {
    let local_x = parse_region_local_block_coord(local_x_text, "localX")?;
    let local_z = parse_region_local_block_coord(local_z_text, "localZ")?;
    let (settings, surface_material_path) = surface_region_trace_settings(
        heightmap_path,
        scale_text,
        region_x_text,
        region_z_text,
        surface_raster_text,
    )?;
    let traces = earthmap_surface::trace_surface_region_columns(
        &settings,
        &[(local_x as usize, local_z as usize)],
    )
    .map_err(|error| error.to_string())?;
    Ok(surface_region_column_trace_lines(
        &traces[0],
        &surface_material_path,
    ))
}

#[allow(clippy::too_many_arguments)]
fn trace_surface_region_cell(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    chunk_local_x_text: &str,
    chunk_local_z_text: &str,
    cell_x_text: &str,
    cell_z_text: &str,
    surface_raster_text: Option<&str>,
) -> io::Result<i32> {
    match trace_surface_region_cell_impl(
        heightmap_path,
        scale_text,
        region_x_text,
        region_z_text,
        chunk_local_x_text,
        chunk_local_z_text,
        cell_x_text,
        cell_z_text,
        surface_raster_text.unwrap_or("surfaceRaster=auto"),
    ) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Surface region cell trace failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn trace_surface_region_cell_impl(
    heightmap_path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    chunk_local_x_text: &str,
    chunk_local_z_text: &str,
    cell_x_text: &str,
    cell_z_text: &str,
    surface_raster_text: &str,
) -> std::result::Result<Vec<String>, String> {
    let chunk_local_x = parse_region_local_chunk_coord(chunk_local_x_text, "chunkLocalX")?;
    let chunk_local_z = parse_region_local_chunk_coord(chunk_local_z_text, "chunkLocalZ")?;
    let cell_x = parse_chunk_biome_cell_coord(cell_x_text, "cellX")?;
    let cell_z = parse_chunk_biome_cell_coord(cell_z_text, "cellZ")?;
    let (settings, surface_material_path) = surface_region_trace_settings(
        heightmap_path,
        scale_text,
        region_x_text,
        region_z_text,
        surface_raster_text,
    )?;
    let origin_x =
        (usize::from(chunk_local_x) * CHUNK_WIDTH) + (usize::from(cell_x) * BIOME_CELL_WIDTH);
    let origin_z =
        (usize::from(chunk_local_z) * CHUNK_WIDTH) + (usize::from(cell_z) * BIOME_CELL_WIDTH);
    let mut local_columns = Vec::with_capacity(BIOME_CELL_WIDTH * BIOME_CELL_WIDTH);
    for dz in 0..BIOME_CELL_WIDTH {
        for dx in 0..BIOME_CELL_WIDTH {
            local_columns.push((origin_x + dx, origin_z + dz));
        }
    }
    let traces = earthmap_surface::trace_surface_region_columns(&settings, &local_columns)
        .map_err(|error| error.to_string())?;
    Ok(surface_region_cell_trace_lines(
        chunk_local_x,
        chunk_local_z,
        cell_x,
        cell_z,
        &traces,
        &surface_material_path,
    ))
}

fn surface_region_trace_settings(
    heightmap_path: &str,
    scale_text: &str,
    region_x_text: &str,
    region_z_text: &str,
    surface_raster_text: &str,
) -> std::result::Result<(SurfaceRegionSettings, std::path::PathBuf), String> {
    let scale = parse_i32_string(scale_text)?;
    let region_x = parse_i32_string(region_x_text)?;
    let region_z = parse_i32_string(region_z_text)?;
    let surface_material_path =
        parse_optional_surface_material_path(surface_raster_text, Path::new(heightmap_path))?
            .ok_or_else(|| {
                "textureMode=photo trace requires a TrueMarble surface raster; use surfaceRaster=auto or pass an explicit TrueMarble.vrt path"
                    .to_string()
            })?;
    let cache_rows = configured_heightmap_cache_rows(Path::new(heightmap_path))?;
    let surface_tile_cache_entries =
        configured_surface_tile_cache_entries(Some(&surface_material_path))?;
    let mut settings = SurfaceRegionSettings::new_with_texture_options(
        heightmap_path,
        ".",
        "SR EarthMap Trace",
        0,
        scale,
        region_x,
        region_z,
        OutputFormat::LinearV2,
        cache_rows,
        false,
        ChunkGenerationStatus::Surface,
        1.0,
        SurfaceTextureMode::Photo,
    )
    .map_err(|error| error.to_string())?;
    settings.surface_material_path = Some(surface_material_path.clone());
    settings.surface_tile_cache_entries = surface_tile_cache_entries;
    Ok((settings, surface_material_path))
}

fn parse_region_local_block_coord(text: &str, name: &str) -> std::result::Result<u16, String> {
    let value = parse_i32_string(text)?;
    if !(0..512).contains(&value) {
        return Err(format!("{name} must be between 0 and 511: {value}"));
    }
    Ok(value as u16)
}

fn parse_region_local_chunk_coord(text: &str, name: &str) -> std::result::Result<u8, String> {
    let value = parse_i32_string(text)?;
    if !(0..32).contains(&value) {
        return Err(format!("{name} must be between 0 and 31: {value}"));
    }
    Ok(value as u8)
}

fn parse_chunk_biome_cell_coord(text: &str, name: &str) -> std::result::Result<u8, String> {
    let value = parse_i32_string(text)?;
    if !(0..4).contains(&value) {
        return Err(format!("{name} must be between 0 and 3: {value}"));
    }
    Ok(value as u8)
}

fn surface_region_cell_trace_lines(
    chunk_local_x: u8,
    chunk_local_z: u8,
    cell_x: u8,
    cell_z: u8,
    traces: &[SurfaceRegionColumnTrace],
    surface_material_path: &Path,
) -> Vec<String> {
    let origin_x =
        (usize::from(chunk_local_x) * CHUNK_WIDTH) + (usize::from(cell_x) * BIOME_CELL_WIDTH);
    let origin_z =
        (usize::from(chunk_local_z) * CHUNK_WIDTH) + (usize::from(cell_z) * BIOME_CELL_WIDTH);
    let mut final_biome_counts = BTreeMap::<String, usize>::new();
    for trace in traces {
        *final_biome_counts
            .entry(trace.final_column.biome_id.clone())
            .or_insert(0) += 1;
    }

    let mut lines = vec![
        "surfaceRegionCellTrace=valid".to_string(),
        format!("chunkLocalX={chunk_local_x}"),
        format!("chunkLocalZ={chunk_local_z}"),
        format!("cellX={cell_x}"),
        format!("cellZ={cell_z}"),
        format!("regionLocalOriginX={origin_x}"),
        format!("regionLocalOriginZ={origin_z}"),
        format!("columnCount={}", traces.len()),
        format!(
            "surfaceMaterialPath={}",
            normalized_path_display(surface_material_path)
        ),
    ];
    for (index, (biome, count)) in final_biome_counts.iter().enumerate() {
        let entry = index + 1;
        lines.push(format!("finalBiomeCount.{entry}.biomeId={biome}"));
        lines.push(format!("finalBiomeCount.{entry}.count={count}"));
    }
    for (index, trace) in traces.iter().enumerate() {
        lines.extend(surface_region_column_trace_lines_with_prefix(
            &format!("column.{}", index + 1),
            trace,
        ));
    }
    lines
}

fn surface_region_column_trace_lines(
    trace: &SurfaceRegionColumnTrace,
    surface_material_path: &Path,
) -> Vec<String> {
    let mut lines = vec![
        "surfaceRegionColumnTrace=valid".to_string(),
        format!("localX={}", trace.local_x),
        format!("localZ={}", trace.local_z),
        format!("globalBlockX={}", trace.global_block_x),
        format!("globalBlockZ={}", trace.global_block_z),
        format!("mapX={}", trace.map_x),
        format!("mapZ={}", trace.map_z),
        format!("longitude={}", java_double_string(trace.longitude)),
        format!("latitude={}", java_double_string(trace.latitude)),
        format!(
            "rawElevationMeters={}",
            java_double_string(trace.raw_elevation_meters)
        ),
        format!(
            "smoothedElevationMeters={}",
            java_double_string(trace.smoothed_elevation_meters)
        ),
        format!(
            "localReliefMeters={}",
            java_double_string(trace.local_relief_meters)
        ),
        format!("valid={}", trace.valid),
        format!("initialWater={}", trace.initial_water),
        format!("coastFactor={}", java_double_string(trace.coast_factor)),
        format!(
            "surfaceMaterialPath={}",
            normalized_path_display(surface_material_path)
        ),
    ];
    lines.extend(surface_material_trace_lines(
        "material",
        &trace.material_sample,
    ));
    lines.extend(surface_column_trace_lines("base", Some(&trace.base_column)));
    lines.extend(surface_column_trace_lines(
        "semantic",
        trace.semantic_column.as_ref(),
    ));
    lines.extend(surface_column_trace_lines(
        "photo",
        trace.photo_column.as_ref(),
    ));
    lines.extend(surface_column_trace_lines(
        "postCell",
        trace.post_cell_column.as_ref(),
    ));
    lines.extend(surface_column_trace_lines(
        "postStabilized",
        trace.post_stabilized_column.as_ref(),
    ));
    lines.extend(surface_component_trace_lines(
        "postFirstComponentTrace",
        &trace.post_first_component_trace,
    ));
    lines.extend(surface_column_trace_lines(
        "postSmoothed",
        trace.post_smoothed_column.as_ref(),
    ));
    lines.extend(surface_column_trace_lines(
        "postComponent",
        trace.post_component_column.as_ref(),
    ));
    lines.extend(surface_component_trace_lines(
        "postComponentTrace",
        &trace.post_component_trace,
    ));
    lines.extend(surface_column_trace_lines(
        "final",
        Some(&trace.final_column),
    ));
    lines
}

fn surface_region_column_trace_lines_with_prefix(
    prefix: &str,
    trace: &SurfaceRegionColumnTrace,
) -> Vec<String> {
    let mut lines = vec![
        format!("{prefix}.localX={}", trace.local_x),
        format!("{prefix}.localZ={}", trace.local_z),
        format!("{prefix}.globalBlockX={}", trace.global_block_x),
        format!("{prefix}.globalBlockZ={}", trace.global_block_z),
        format!("{prefix}.mapX={}", trace.map_x),
        format!("{prefix}.mapZ={}", trace.map_z),
        format!("{prefix}.longitude={}", java_double_string(trace.longitude)),
        format!("{prefix}.latitude={}", java_double_string(trace.latitude)),
        format!(
            "{prefix}.rawElevationMeters={}",
            java_double_string(trace.raw_elevation_meters)
        ),
        format!(
            "{prefix}.smoothedElevationMeters={}",
            java_double_string(trace.smoothed_elevation_meters)
        ),
        format!(
            "{prefix}.localReliefMeters={}",
            java_double_string(trace.local_relief_meters)
        ),
        format!("{prefix}.valid={}", trace.valid),
        format!("{prefix}.initialWater={}", trace.initial_water),
        format!(
            "{prefix}.coastFactor={}",
            java_double_string(trace.coast_factor)
        ),
    ];
    lines.extend(surface_material_trace_lines(
        &format!("{prefix}.material"),
        &trace.material_sample,
    ));
    lines.extend(surface_column_trace_lines(
        &format!("{prefix}.base"),
        Some(&trace.base_column),
    ));
    lines.extend(surface_column_trace_lines(
        &format!("{prefix}.semantic"),
        trace.semantic_column.as_ref(),
    ));
    lines.extend(surface_column_trace_lines(
        &format!("{prefix}.photo"),
        trace.photo_column.as_ref(),
    ));
    lines.extend(surface_column_trace_lines(
        &format!("{prefix}.postCell"),
        trace.post_cell_column.as_ref(),
    ));
    lines.extend(surface_column_trace_lines(
        &format!("{prefix}.postStabilized"),
        trace.post_stabilized_column.as_ref(),
    ));
    lines.extend(surface_component_trace_lines(
        &format!("{prefix}.postFirstComponentTrace"),
        &trace.post_first_component_trace,
    ));
    lines.extend(surface_column_trace_lines(
        &format!("{prefix}.postSmoothed"),
        trace.post_smoothed_column.as_ref(),
    ));
    lines.extend(surface_column_trace_lines(
        &format!("{prefix}.postComponent"),
        trace.post_component_column.as_ref(),
    ));
    lines.extend(surface_component_trace_lines(
        &format!("{prefix}.postComponentTrace"),
        &trace.post_component_trace,
    ));
    lines.extend(surface_column_trace_lines(
        &format!("{prefix}.final"),
        Some(&trace.final_column),
    ));
    lines
}

fn surface_component_trace_lines(
    prefix: &str,
    trace: &Option<earthmap_surface::SurfaceBiomeComponentTrace>,
) -> Vec<String> {
    let Some(trace) = trace else {
        return vec![format!("{prefix}.present=false")];
    };
    vec![
        format!("{prefix}.present=true"),
        format!("{prefix}.family={}", trace.family),
        format!("{prefix}.size={}", trace.size),
        format!("{prefix}.minLocalX={}", trace.min_local_x),
        format!("{prefix}.minLocalZ={}", trace.min_local_z),
        format!("{prefix}.maxLocalX={}", trace.max_local_x),
        format!("{prefix}.maxLocalZ={}", trace.max_local_z),
        format!(
            "{prefix}.neighborMajorityBiome={}",
            trace.neighbor_majority_biome.as_deref().unwrap_or("")
        ),
        format!(
            "{prefix}.neighborCounts={}",
            trace
                .neighbor_counts
                .iter()
                .map(|(biome, count)| format!("{biome}:{count}"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        format!("{prefix}.action={}", trace.action),
    ]
}

fn surface_material_trace_lines(
    prefix: &str,
    sample: &Option<SurfaceMaterialSample>,
) -> Vec<String> {
    let Some(sample) = sample else {
        return vec![format!("{prefix}.present=false")];
    };
    vec![
        format!("{prefix}.present=true"),
        format!("{prefix}.color={}", rgb_trace_value(sample.color)),
        format!(
            "{prefix}.terrainTokenColor={}",
            rgb_trace_value(sample.terrain_token_color)
        ),
        format!(
            "{prefix}.terrainTokenSource={:?}",
            sample.terrain_token_source
        ),
        format!("{prefix}.climateClass={}", sample.climate_class),
        format!(
            "{prefix}.evergreenBroadleafTrees={}",
            sample.evergreen_broadleaf_trees
        ),
        format!(
            "{prefix}.deciduousBroadleafTrees={}",
            sample.deciduous_broadleaf_trees
        ),
        format!("{prefix}.needleleafTrees={}", sample.needleleaf_trees),
        format!("{prefix}.mixedTrees={}", sample.mixed_trees),
        format!(
            "{prefix}.herbaceousVegetation={}",
            sample.herbaceous_vegetation
        ),
        format!("{prefix}.shrubs={}", sample.shrubs),
        format!("{prefix}.snowCover={}", sample.snow_cover),
        format!("{prefix}.swampCover={}", sample.swamp_cover),
        format!("{prefix}.oceanTemperature={}", sample.ocean_temperature),
        format!("{prefix}.bathymetryMeters={}", sample.bathymetry_meters),
        format!("{prefix}.slopePermille={}", sample.slope_permille),
        format!("{prefix}.ecoregionName={}", sample.ecoregion_name),
        format!("{prefix}.ecoregionBiomeId={}", sample.ecoregion_biome_id),
        format!(
            "{prefix}.ecoregionConfidence={}",
            java_double_string(sample.ecoregion_confidence)
        ),
    ]
}

fn surface_column_trace_lines(prefix: &str, column: Option<&EarthSurfaceColumn>) -> Vec<String> {
    let Some(column) = column else {
        return vec![format!("{prefix}.present=false")];
    };
    vec![
        format!("{prefix}.present=true"),
        format!("{prefix}.water={}", column.water),
        format!("{prefix}.groundSurfaceY={}", column.ground_surface_y),
        format!("{prefix}.waterSurfaceY={}", column.water_surface_y),
        format!("{prefix}.topBlockStateId={}", column.top_block_state_id),
        format!(
            "{prefix}.fillerBlockStateId={}",
            column.filler_block_state_id
        ),
        format!("{prefix}.biomeId={}", column.biome_id),
        format!("{prefix}.decisionSource={}", column.decision_source),
        format!(
            "{prefix}.terrainTokenSource={:?}",
            column.terrain_token_source
        ),
        format!("{prefix}.dataEvidenceFlags={}", column.data_evidence_flags),
    ]
}

fn rgb_trace_value(color: earthmap_geo::RgbColor) -> String {
    if color.available {
        format!("#{:02X}{:02X}{:02X}", color.red, color.green, color.blue)
    } else {
        "unavailable".to_string()
    }
}

fn surface_region_report_lines(report: &SurfaceRegionReport, world_dir: &str) -> Vec<String> {
    vec![
        "Surface region generated".to_string(),
        format!("regionX={}", report.region_x),
        format!("regionZ={}", report.region_z),
        format!("format={}", report.output_format.java_name()),
        format!("scale=1:{}", report.scale_denominator),
        format!("chunkCount={}", report.chunk_count),
        format!("landColumns={}", report.land_columns),
        format!("waterColumns={}", report.water_columns),
        format!("minGroundY={}", report.min_ground_y),
        format!("maxGroundY={}", report.max_ground_y),
        format!("regionFile={}", report.region_file.display()),
        format!("cacheMaxRows={}", report.cache_stats.max_rows),
        format!("cacheResidentRows={}", report.cache_stats.resident_rows),
        format!("cacheHits={}", report.cache_stats.hits),
        format!("cacheMisses={}", report.cache_stats.misses),
        format!("cacheEvictions={}", report.cache_stats.evictions),
        format!(
            "surfaceRasterSourceCount={}",
            report.surface_material_raster_stats.source_count
        ),
        format!(
            "surfaceRasterOpenReaders={}",
            report.surface_material_raster_stats.open_readers
        ),
        format!(
            "surfaceRasterResidentTiles={}",
            report.surface_material_raster_stats.resident_tiles
        ),
        format!(
            "surfaceRasterTileHits={}",
            report.surface_material_raster_stats.tile_hits
        ),
        format!(
            "surfaceRasterTileMisses={}",
            report.surface_material_raster_stats.tile_misses
        ),
        format!(
            "surfaceRasterTileEvictions={}",
            report.surface_material_raster_stats.tile_evictions
        ),
        format!(
            "phase.surfaceSampleMillis={}",
            millis(report.surface_sample_nanos)
        ),
        format!(
            "phase.chunkBuildMillis={}",
            millis(report.chunk_build_nanos)
        ),
        format!("phase.nbtEncodeMillis={}", millis(report.nbt_encode_nanos)),
        format!(
            "phase.regionWriteMillis={}",
            millis(report.region_write_nanos)
        ),
        format!("phase.previewMillis={}", millis(report.preview_nanos)),
        format!("phase.metadataMillis={}", millis(report.metadata_nanos)),
        format!("phase.totalInternalMillis={}", millis(report.total_nanos)),
        format!(
            "manifestFile={}",
            Path::new(world_dir)
                .join(SURVIVAL_MANIFEST_FILE_NAME)
                .display()
        ),
    ]
}

fn vanilla_delegated_region_report_lines(
    report: &SurfaceRegionReport,
    world_dir: &str,
    status: ChunkGenerationStatus,
    surface_material_path: &Path,
    compression_options: RegionCompressionOptions,
    vertical_scale_mode: &str,
) -> Vec<String> {
    let mut lines = vec![
        "Vanilla-delegated surface region generated".to_string(),
        format!("regionX={}", report.region_x),
        format!("regionZ={}", report.region_z),
        format!("format={}", report.output_format.java_name()),
        format!("scale=1:{}", report.scale_denominator),
        format!("verticalScale={}", report.vertical_scale),
        format!("verticalScaleMode={vertical_scale_mode}"),
        format!("chunkStatus={}", status.id()),
        compression_options_report_line(report.output_format, compression_options),
        format!(
            "surfaceMaterialPath={}",
            normalized_path_display(surface_material_path)
        ),
        "serverDelegation=true".to_string(),
        "directCaves=false".to_string(),
        "directOres=false".to_string(),
        "directVegetation=false".to_string(),
        "directStructures=false".to_string(),
        "progressionStrategy=none".to_string(),
        "directProgressionStructures=false".to_string(),
        format!("chunkCount={}", report.chunk_count),
        format!("landColumns={}", report.land_columns),
        format!("waterColumns={}", report.water_columns),
        format!("minGroundY={}", report.min_ground_y),
        format!("maxGroundY={}", report.max_ground_y),
        format!("regionFile={}", report.region_file.display()),
        format!("cacheMaxRows={}", report.cache_stats.max_rows),
        format!("cacheResidentRows={}", report.cache_stats.resident_rows),
        format!("cacheHits={}", report.cache_stats.hits),
        format!("cacheMisses={}", report.cache_stats.misses),
        format!("cacheEvictions={}", report.cache_stats.evictions),
        format!(
            "surfaceRasterSourceCount={}",
            report.surface_material_raster_stats.source_count
        ),
        format!(
            "surfaceRasterOpenReaders={}",
            report.surface_material_raster_stats.open_readers
        ),
        format!(
            "surfaceRasterResidentTiles={}",
            report.surface_material_raster_stats.resident_tiles
        ),
        format!(
            "surfaceRasterTileHits={}",
            report.surface_material_raster_stats.tile_hits
        ),
        format!(
            "surfaceRasterTileMisses={}",
            report.surface_material_raster_stats.tile_misses
        ),
        format!(
            "surfaceRasterTileEvictions={}",
            report.surface_material_raster_stats.tile_evictions
        ),
    ];
    lines.extend(surface_phase_report_lines(report, world_dir));
    lines
}

fn surface_phase_report_lines(report: &SurfaceRegionReport, world_dir: &str) -> Vec<String> {
    vec![
        format!(
            "phase.surfaceSampleMillis={}",
            millis(report.surface_sample_nanos)
        ),
        format!(
            "phase.chunkBuildMillis={}",
            millis(report.chunk_build_nanos)
        ),
        format!("phase.nbtEncodeMillis={}", millis(report.nbt_encode_nanos)),
        format!(
            "phase.regionWriteMillis={}",
            millis(report.region_write_nanos)
        ),
        format!("phase.previewMillis={}", millis(report.preview_nanos)),
        format!("phase.metadataMillis={}", millis(report.metadata_nanos)),
        format!("phase.totalInternalMillis={}", millis(report.total_nanos)),
        format!(
            "manifestFile={}",
            Path::new(world_dir)
                .join(SURVIVAL_MANIFEST_FILE_NAME)
                .display()
        ),
    ]
}

fn millis(nanos: u128) -> u128 {
    nanos / 1_000_000
}

fn u128_to_u64(value: u128) -> u64 {
    value.try_into().unwrap_or(u64::MAX)
}

fn csv_cell(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn parse_i32_string(text: &str) -> std::result::Result<i32, String> {
    text.parse::<i32>().map_err(|error| error.to_string())
}

fn parse_positive_i32_string(name: &str, text: &str) -> std::result::Result<i32, String> {
    let value = parse_i32_string(text)?;
    if value <= 0 {
        return Err(format!("{name} must be positive: {value}"));
    }
    Ok(value)
}

fn parse_positive_usize_string(name: &str, text: &str) -> std::result::Result<usize, String> {
    let value = text.parse::<usize>().map_err(|error| error.to_string())?;
    if value == 0 {
        return Err(format!("{name} must be positive: {value}"));
    }
    Ok(value)
}

fn is_output_format_text(text: &str) -> bool {
    matches_ignore_ascii_case(text, &["mca", "linear"])
}

fn parse_optional_surface_material_path(
    value: &str,
    heightmap_path: &Path,
) -> std::result::Result<Option<std::path::PathBuf>, String> {
    let raw = value
        .split_once('=')
        .map(|(_, value)| value)
        .unwrap_or(value)
        .trim();
    if matches_ignore_ascii_case(raw, &["none", "off", "false"]) {
        return Ok(None);
    }
    if matches_ignore_ascii_case(
        raw,
        &["auto-if-present", "autoIfPresent", "if-present", "default"],
    ) {
        return Ok(auto_detect_true_marble(heightmap_path));
    }
    if matches_ignore_ascii_case(raw, &["auto", "true"]) {
        return auto_detect_true_marble(heightmap_path)
            .map(Some)
            .ok_or_else(|| {
                "surfaceRaster=auto could not find TifFiles/terrain/TrueMarble.vrt".to_string()
            });
    }
    Ok(Some(std::path::PathBuf::from(raw)))
}

fn matches_ignore_ascii_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn auto_detect_true_marble(heightmap_path: &Path) -> Option<std::path::PathBuf> {
    let mut candidates = Vec::<std::path::PathBuf>::new();
    if let Some(path) = configured_path_from_env(SURFACE_RASTER_ENV) {
        push_distinct_path(&mut candidates, path);
    }
    if let Some(root) = configured_path_from_env(TIF_ROOT_ENV) {
        push_distinct_path(&mut candidates, root.join("terrain").join("TrueMarble.vrt"));
    }
    if let Some(root) = configured_path_from_env(DATA_ROOT_ENV) {
        push_distinct_path(
            &mut candidates,
            root.join("TifFiles").join("terrain").join("TrueMarble.vrt"),
        );
        push_distinct_path(&mut candidates, root.join("terrain").join("TrueMarble.vrt"));
    }
    let normalized_heightmap = normalized_path(heightmap_path);
    if let Some(parent) = normalized_heightmap.parent() {
        push_distinct_path(
            &mut candidates,
            parent.join("terrain").join("TrueMarble.vrt"),
        );
        if let Some(grand_parent) = parent.parent() {
            push_distinct_path(
                &mut candidates,
                grand_parent
                    .join("TifFiles")
                    .join("terrain")
                    .join("TrueMarble.vrt"),
            );
        }
    }
    for candidate in &candidates {
        let normalized = normalized_path(candidate);
        if normalized.is_file() && has_enhanced_photo_companion(&normalized) {
            return Some(normalized);
        }
    }
    for candidate in candidates {
        let normalized = normalized_path(&candidate);
        if normalized.is_file() {
            return Some(normalized);
        }
    }
    None
}

fn push_distinct_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    let normalized = normalized_path(&path);
    if !paths
        .iter()
        .any(|existing| normalized_path(existing) == normalized)
    {
        paths.push(path);
    }
}

fn has_enhanced_photo_companion(true_marble_path: &Path) -> bool {
    let Some(terrain) = true_marble_path.parent() else {
        return false;
    };
    let Some(tif_root) = terrain.parent() else {
        return false;
    };
    tif_root.join("land_shallow_topo_west.tif").is_file()
        && tif_root.join("land_shallow_topo_east.tif").is_file()
}

fn auto_shared_heightmap_cache_rows(
    heightmap_path: &Path,
    threads: i32,
) -> std::result::Result<usize, String> {
    let reader = GeoTiffHeightmapReader::open(heightmap_path).map_err(|error| error.to_string())?;
    let row_bytes = i64::from(reader.metadata().width)
        .checked_mul(2)
        .ok_or_else(|| "heightmap row byte count overflow".to_string())?;
    auto_shared_heightmap_cache_rows_for_memory(
        row_bytes as u64,
        usize::try_from(threads.max(1)).expect("positive thread count"),
        total_physical_memory_bytes(),
    )
}

fn configured_heightmap_cache_rows(heightmap_path: &Path) -> std::result::Result<usize, String> {
    if let Ok(text) = std::env::var(HEIGHTMAP_CACHE_ROWS_ENV) {
        if !is_auto_cache_value(&text) {
            return parse_positive_usize_env(HEIGHTMAP_CACHE_ROWS_ENV, &text);
        }
    }
    let threads = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .min(i32::MAX as usize) as i32;
    auto_shared_heightmap_cache_rows(heightmap_path, threads)
}

fn configured_surface_tile_cache_entries(
    surface_material_path: Option<&Path>,
) -> std::result::Result<usize, String> {
    if let Ok(text) = std::env::var(SURFACE_TILE_CACHE_ENTRIES_ENV) {
        if !is_auto_cache_value(&text) {
            return parse_positive_usize_env(SURFACE_TILE_CACHE_ENTRIES_ENV, &text);
        }
    }
    Ok(auto_surface_tile_cache_entries_for_memory(
        total_physical_memory_bytes(),
        surface_cache_reader_count(surface_material_path),
    ))
}

fn is_auto_cache_value(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.is_empty() || trimmed.eq_ignore_ascii_case(CACHE_AUTO_VALUE)
}

fn auto_shared_heightmap_cache_rows_for_memory(
    row_bytes: u64,
    threads: usize,
    total_memory_bytes: Option<u64>,
) -> std::result::Result<usize, String> {
    if row_bytes == 0 {
        return Err("heightmap row byte count must be positive".to_string());
    }
    let fallback_budget = 512 * BYTES_PER_MIB;
    let budget_bytes = auto_cache_budget_bytes(
        total_memory_bytes,
        HEIGHTMAP_CACHE_MEMORY_PERCENT,
        4 * BYTES_PER_GIB,
        fallback_budget,
    );
    let budget_rows =
        usize::try_from((budget_bytes / row_bytes).max(1)).unwrap_or(HEIGHTMAP_CACHE_MAX_ROWS);
    let concurrency_rows = threads.saturating_mul(64).max(HEIGHTMAP_CACHE_MIN_ROWS);
    Ok(budget_rows
        .max(concurrency_rows.min(HEIGHTMAP_CACHE_MAX_ROWS))
        .clamp(HEIGHTMAP_CACHE_MIN_ROWS, HEIGHTMAP_CACHE_MAX_ROWS))
}

fn auto_surface_tile_cache_entries_for_memory(
    total_memory_bytes: Option<u64>,
    cache_reader_count: usize,
) -> usize {
    let fallback_budget = u64::try_from(DEFAULT_SURFACE_TILE_CACHE_ENTRIES)
        .unwrap_or(256)
        .saturating_mul(SURFACE_TILE_CACHE_ASSUMED_ENTRY_BYTES)
        .saturating_mul(cache_reader_count.max(1) as u64);
    let budget_bytes = auto_cache_budget_bytes(
        total_memory_bytes,
        SURFACE_TILE_CACHE_MEMORY_PERCENT,
        16 * BYTES_PER_GIB,
        fallback_budget,
    );
    let per_reader_budget = budget_bytes / cache_reader_count.max(1) as u64;
    usize::try_from((per_reader_budget / SURFACE_TILE_CACHE_ASSUMED_ENTRY_BYTES).max(1))
        .unwrap_or(SURFACE_TILE_CACHE_MAX_ENTRIES)
        .clamp(
            SURFACE_TILE_CACHE_MIN_ENTRIES,
            SURFACE_TILE_CACHE_MAX_ENTRIES,
        )
}

fn effective_prefetch_config(
    options: GenerationPrefetchOptions,
    worker_count: usize,
    region_count: usize,
) -> EffectivePrefetchConfig {
    if !options.enabled || region_count == 0 {
        return EffectivePrefetchConfig::disabled();
    }
    let memory_cap_bytes = options.memory_cap_bytes.or_else(|| {
        Some(auto_cache_budget_bytes(
            total_physical_memory_bytes(),
            PREFETCH_MEMORY_PERCENT,
            PREFETCH_MAX_MEMORY_BYTES,
            1 * BYTES_PER_GIB,
        ))
    });
    let estimated_region_bytes = PREPARED_SURFACE_REGION_ESTIMATED_BYTES.max(1);
    let memory_limited_regions = memory_cap_bytes
        .map(|bytes| (bytes / estimated_region_bytes).max(1))
        .and_then(|regions| usize::try_from(regions).ok())
        .unwrap_or(usize::MAX);
    let default_queue_regions = worker_count.saturating_mul(2).max(1);
    let requested_queue_regions = options
        .queue_regions
        .unwrap_or(default_queue_regions)
        .max(1);
    let queue_regions = requested_queue_regions
        .min(memory_limited_regions)
        .min(region_count)
        .max(1);
    let workers = options
        .workers
        .max(1)
        .min(queue_regions)
        .min(worker_count.max(1));
    EffectivePrefetchConfig {
        enabled: queue_regions > 0 && workers > 0,
        memory_cap_bytes,
        queue_regions,
        workers,
        estimated_region_bytes,
    }
}

fn auto_cache_budget_bytes(
    total_memory_bytes: Option<u64>,
    percent_of_total: u64,
    max_budget_bytes: u64,
    fallback_budget_bytes: u64,
) -> u64 {
    let Some(total_memory_bytes) = total_memory_bytes.filter(|value| *value > 0) else {
        return fallback_budget_bytes.min(max_budget_bytes).max(1);
    };
    let reserve_bytes = (total_memory_bytes / 4)
        .max(4 * BYTES_PER_GIB)
        .min(total_memory_bytes.saturating_sub(BYTES_PER_GIB));
    let process_budget = total_memory_bytes
        .saturating_sub(reserve_bytes)
        .max(BYTES_PER_MIB);
    let target_budget = total_memory_bytes
        .saturating_mul(percent_of_total)
        .checked_div(100)
        .unwrap_or(0);
    target_budget
        .max(fallback_budget_bytes.min(process_budget))
        .min(process_budget)
        .min(max_budget_bytes)
        .max(1)
}

fn surface_cache_reader_count(surface_material_path: Option<&Path>) -> usize {
    let Some(surface_material_path) = surface_material_path else {
        return 1;
    };
    let terrain_dir = normalized_path(surface_material_path)
        .parent()
        .map(Path::to_path_buf);
    let tif_root = terrain_dir
        .as_ref()
        .and_then(|path| path.parent())
        .map(Path::to_path_buf);
    let vegetation = tif_root.as_ref().map(|root| root.join("vegetation"));
    let mut count = 1usize;
    if tif_root
        .as_ref()
        .is_some_and(|root| root.join("climate.tif").is_file())
    {
        count += 1;
    }
    if let Some(vegetation) = &vegetation {
        for name in [
            "EvergreenBroadleafTrees.tif",
            "DeciduousBroadleafTrees.tif",
            "EvergreenDeciduousNeedleleafTrees.tif",
            "mixed.tif",
            "HerbaceousVegetation.tif",
            "Shrubs.tif",
            "Snow.tif",
            "Swamp.tif",
        ] {
            if vegetation.join(name).is_file() {
                count += 1;
            }
        }
    }
    if tif_root
        .as_ref()
        .is_some_and(|root| root.join("ocean_temp_infill.tif").is_file())
    {
        count += 1;
    }
    if tif_root.as_ref().is_some_and(|root| {
        root.join("land_shallow_topo_west.tif").is_file()
            && root.join("land_shallow_topo_east.tif").is_file()
    }) {
        count += 2;
    }
    count
}

fn parse_positive_usize_env(name: &str, text: &str) -> std::result::Result<usize, String> {
    let value = text
        .trim()
        .parse::<usize>()
        .map_err(|error| format!("{name} must be a positive integer: {error}"))?;
    if value == 0 {
        return Err(format!("{name} must be positive"));
    }
    Ok(value)
}

fn rustc_version() -> String {
    command_stdout_first_line("rustc", &["--version"]).unwrap_or_else(|| "unavailable".to_string())
}

fn cpu_model() -> String {
    #[cfg(windows)]
    {
        command_stdout_first_line(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name)",
            ],
        )
        .unwrap_or_else(|| {
            std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unavailable".to_string())
        })
    }
    #[cfg(not(windows))]
    {
        command_stdout_first_line(
            "sh",
            &[
                "-c",
                "lscpu | sed -n 's/^Model name:[[:space:]]*//p' | head -n 1",
            ],
        )
        .unwrap_or_else(|| "unavailable".to_string())
    }
}

fn total_physical_memory_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        command_stdout_first_line(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
            ],
        )
        .and_then(|text| parse_u64_digits(&text))
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|line| line.starts_with("MemTotal:"))
                    .and_then(parse_u64_digits)
                    .map(|kib| kib.saturating_mul(1024))
            })
    }
    #[cfg(target_os = "macos")]
    {
        command_stdout_first_line("sysctl", &["-n", "hw.memsize"])
            .and_then(|text| parse_u64_digits(&text))
    }
    #[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
    {
        let pages = command_stdout_first_line("getconf", &["_PHYS_PAGES"])
            .and_then(|text| parse_u64_digits(&text))?;
        let page_size = command_stdout_first_line("getconf", &["PAGE_SIZE"])
            .and_then(|text| parse_u64_digits(&text))?;
        Some(pages.saturating_mul(page_size))
    }
}

fn peak_working_set_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        let command = format!("(Get-Process -Id {}).PeakWorkingSet64", std::process::id());
        command_stdout_first_line("powershell", &["-NoProfile", "-Command", &command])
            .and_then(|text| parse_u64_digits(&text))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn command_stdout_first_line(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn parse_u64_digits(text: &str) -> Option<u64> {
    let digits = text
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty())
        .then(|| digits.parse::<u64>().ok())
        .flatten()
}

fn normalized_path(path: &Path) -> std::path::PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .components()
        .collect()
}

fn normalized_path_display(path: &Path) -> String {
    java_display_path(&normalized_path(path))
}

fn java_display_path(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

fn java_double_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == f64::INFINITY {
        return "Infinity".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    if value.to_bits() == 1 {
        return "4.9E-324".to_string();
    }
    if value.to_bits() == (1 | (1u64 << 63)) {
        return "-4.9E-324".to_string();
    }
    let absolute = value.abs();
    if absolute != 0.0 && !(1.0e-3..1.0e7).contains(&absolute) {
        let formatted = format!("{value:E}");
        if let Some((mantissa, exponent)) = formatted.split_once('E') {
            if mantissa.contains('.') {
                return formatted;
            }
            return format!("{mantissa}.0E{exponent}");
        }
        return formatted;
    }
    if value.is_finite() && value.fract() == 0.0 {
        return format!("{value:.1}");
    }
    value.to_string()
}

fn write_sha256_manifest(
    out: &mut impl Write,
    err: &mut impl Write,
    root: &str,
    output: &str,
) -> io::Result<i32> {
    match earthmap_parity::write_sha256_manifest(root, output) {
        Ok(()) => {
            writeln!(out, "sha256Manifest={output}")?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "SHA-256 manifest failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn write_region_payload_manifest(
    out: &mut impl Write,
    err: &mut impl Write,
    region: &str,
    output: &str,
    append: bool,
) -> io::Result<i32> {
    let result = if append {
        earthmap_parity::append_region_payload_manifest(region, output)
    } else {
        earthmap_parity::write_region_payload_manifest(region, output)
    };
    match result {
        Ok(()) => {
            writeln!(out, "chunkPayloadManifest={output}")?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Chunk payload manifest failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn compare_region_payload_manifest(
    out: &mut impl Write,
    err: &mut impl Write,
    manifest: &str,
    region: &str,
) -> io::Result<i32> {
    match earthmap_parity::compare_region_payload_manifest(manifest, region) {
        Ok(report) => {
            writeln!(
                out,
                "regionPayloadManifest={}",
                if report.matches() { "valid" } else { "invalid" }
            )?;
            writeln!(out, "format={}", report.format)?;
            writeln!(out, "regionX={}", report.region_x)?;
            writeln!(out, "regionZ={}", report.region_z)?;
            writeln!(out, "comparedChunks={}", report.compared_chunks)?;
            writeln!(out, "matchingChunks={}", report.matching_chunks)?;
            writeln!(out, "missingChunks={}", report.missing_chunks)?;
            writeln!(out, "extraChunks={}", report.extra_chunks)?;
            writeln!(out, "mismatchedChunks={}", report.mismatched_chunks)?;
            if let Some(pos) = report.first_mismatch {
                writeln!(out, "firstMismatch={},{}", pos.x, pos.z)?;
            }
            Ok(if report.matches() {
                EXIT_OK
            } else {
                EXIT_USAGE
            })
        }
        Err(error) => {
            writeln!(err, "Chunk payload manifest comparison failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn validate_mca_region(
    out: &mut impl Write,
    err: &mut impl Write,
    region: &str,
) -> io::Result<i32> {
    match validate_mca_region_file(region) {
        Ok(report) => {
            writeln!(out, "MCA region valid")?;
            writeln!(out, "fileBytes={}", report.file_bytes)?;
            writeln!(out, "totalSectors={}", report.total_sectors)?;
            writeln!(out, "chunkCount={}", report.chunk_count)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "MCA region validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn validate_linear_region(
    out: &mut impl Write,
    err: &mut impl Write,
    region: &str,
) -> io::Result<i32> {
    match validate_linear_region_file(region) {
        Ok(report) => {
            writeln!(out, "Linear V2 region valid")?;
            writeln!(out, "fileBytes={}", report.file_bytes)?;
            writeln!(out, "regionX={}", report.region_x)?;
            writeln!(out, "regionZ={}", report.region_z)?;
            writeln!(out, "gridSize={}", report.grid_size)?;
            writeln!(out, "chunkCount={}", report.chunk_count)?;
            writeln!(out, "bitmapChunkCount={}", report.bitmap_chunk_count)?;
            writeln!(
                out,
                "bitmapMissingPayloadCount={}",
                report.bitmap_missing_payload_count
            )?;
            writeln!(
                out,
                "bitmapExtraChunkCount={}",
                report.bitmap_extra_chunk_count
            )?;
            writeln!(out, "bitmapConsistent={}", report.bitmap_consistent())?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Linear V2 region validation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn compare_mca_linear_region_payloads_cli(
    out: &mut impl Write,
    err: &mut impl Write,
    mca_region: &str,
    linear_region: &str,
) -> io::Result<i32> {
    match compare_mca_linear_region_payloads(mca_region, linear_region) {
        Ok(report) => {
            writeln!(out, "MCA/Linear payload parity compared")?;
            writeln!(out, "matches={}", report.matches())?;
            writeln!(out, "comparedChunks={}", report.compared_chunks)?;
            writeln!(out, "matchingChunks={}", report.matching_chunks)?;
            writeln!(out, "mismatchedChunks={}", report.mismatched_chunks)?;
            writeln!(out, "missingInMca={}", report.missing_in_mca)?;
            writeln!(out, "missingInLinear={}", report.missing_in_linear)?;
            if let Some(pos) = report.first_mismatch {
                writeln!(out, "firstMismatch={},{}", pos.x, pos.z)?;
            } else {
                writeln!(out, "firstMismatch=")?;
            }
            Ok(if report.matches() {
                EXIT_OK
            } else {
                EXIT_USAGE
            })
        }
        Err(error) => {
            writeln!(err, "MCA/Linear payload parity failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[derive(Clone, Debug)]
struct DynmapTileMosaicReport {
    tile_directory: std::path::PathBuf,
    output_path: std::path::PathBuf,
    metadata_path: std::path::PathBuf,
    level_prefix: String,
    tile_count: usize,
    tile_width: u32,
    tile_height: u32,
    min_tile_x: i32,
    max_tile_x: i32,
    min_tile_y: i32,
    max_tile_y: i32,
    width: u32,
    height: u32,
    missing_tile_count: usize,
    empty_tile_count: usize,
}

impl DynmapTileMosaicReport {
    fn properties_text(&self) -> String {
        format!(
            concat!(
                "tileDirectory={}\n",
                "outputPath={}\n",
                "metadataPath={}\n",
                "levelPrefix={}\n",
                "tileCount={}\n",
                "tileWidth={}\n",
                "tileHeight={}\n",
                "minTileX={}\n",
                "maxTileX={}\n",
                "minTileY={}\n",
                "maxTileY={}\n",
                "width={}\n",
                "height={}\n",
                "missingTileCount={}\n",
                "emptyTileCount={}\n"
            ),
            normalized_path_display(&self.tile_directory),
            normalized_path_display(&self.output_path),
            normalized_path_display(&self.metadata_path),
            self.level_prefix,
            self.tile_count,
            self.tile_width,
            self.tile_height,
            self.min_tile_x,
            self.max_tile_x,
            self.min_tile_y,
            self.max_tile_y,
            self.width,
            self.height,
            self.missing_tile_count,
            self.empty_tile_count
        )
    }
}

fn dynmap_tile_mosaic_cli(
    out: &mut impl Write,
    err: &mut impl Write,
    tile_directory: &str,
    output_path: &str,
    level_prefix: &str,
) -> io::Result<i32> {
    match dynmap_tile_mosaic_impl(
        Path::new(tile_directory),
        Path::new(output_path),
        level_prefix,
    ) {
        Ok(report) => {
            writeln!(out, "Dynmap tile mosaic written")?;
            write_dynmap_tile_mosaic_report(out, &report)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Dynmap tile mosaic failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn write_dynmap_tile_mosaic_report(
    out: &mut impl Write,
    report: &DynmapTileMosaicReport,
) -> io::Result<()> {
    writeln!(
        out,
        "tileDirectory={}",
        normalized_path_display(&report.tile_directory)
    )?;
    writeln!(
        out,
        "outputPath={}",
        normalized_path_display(&report.output_path)
    )?;
    writeln!(
        out,
        "metadataPath={}",
        normalized_path_display(&report.metadata_path)
    )?;
    writeln!(out, "levelPrefix={}", report.level_prefix)?;
    writeln!(out, "tileCount={}", report.tile_count)?;
    writeln!(out, "tileWidth={}", report.tile_width)?;
    writeln!(out, "tileHeight={}", report.tile_height)?;
    writeln!(out, "minTileX={}", report.min_tile_x)?;
    writeln!(out, "maxTileX={}", report.max_tile_x)?;
    writeln!(out, "minTileY={}", report.min_tile_y)?;
    writeln!(out, "maxTileY={}", report.max_tile_y)?;
    writeln!(out, "width={}", report.width)?;
    writeln!(out, "height={}", report.height)?;
    writeln!(out, "missingTileCount={}", report.missing_tile_count)?;
    writeln!(out, "emptyTileCount={}", report.empty_tile_count)
}

fn dynmap_tile_mosaic_impl(
    tile_directory: &Path,
    output_path: &Path,
    level_prefix: &str,
) -> std::result::Result<DynmapTileMosaicReport, String> {
    if !tile_directory.is_dir() {
        return Err(format!(
            "tile directory not found: {}",
            tile_directory.display()
        ));
    }
    let normalized_prefix = normalize_dynmap_level_prefix(level_prefix);
    let mut tiles = BTreeMap::<(i32, i32), std::path::PathBuf>::new();
    collect_dynmap_tiles(tile_directory, &normalized_prefix, &mut tiles)?;
    if tiles.is_empty() {
        return Err(format!(
            "no Dynmap tiles matched prefix '{}' under {}",
            normalized_prefix,
            tile_directory.display()
        ));
    }

    let min_tile_x = tiles.keys().map(|coord| coord.0).min().unwrap();
    let max_tile_x = tiles.keys().map(|coord| coord.0).max().unwrap();
    let min_tile_y = tiles.keys().map(|coord| coord.1).min().unwrap();
    let max_tile_y = tiles.keys().map(|coord| coord.1).max().unwrap();
    let first_tile = read_dynmap_tile_image(tiles.values().next().unwrap())?;
    let tile_width = first_tile.width;
    let tile_height = first_tile.height;
    if tile_width == 0 || tile_height == 0 {
        return Err("invalid tile size".to_string());
    }
    let span_x = dynmap_tile_span(min_tile_x, max_tile_x, "x")?;
    let span_y = dynmap_tile_span(min_tile_y, max_tile_y, "y")?;
    let width = checked_u32_product(span_x, tile_width, "mosaic width")?;
    let height = checked_u32_product(span_y, tile_height, "mosaic height")?;
    let pixel_bytes = usize::try_from(width)
        .expect("u32 width fits usize")
        .checked_mul(usize::try_from(height).expect("u32 height fits usize"))
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| "Dynmap mosaic is too large".to_string())?;
    let mut pixels = vec![0u8; pixel_bytes];

    for ((tile_x, tile_y), path) in &tiles {
        let tile = read_dynmap_tile_image(path)?;
        if tile.width != tile_width || tile.height != tile_height {
            return Err(format!(
                "mixed Dynmap tile sizes: expected {}x{} but got {}x{} at {}",
                tile_width,
                tile_height,
                tile.width,
                tile.height,
                path.display()
            ));
        }
        let mosaic_x = u32::try_from(*tile_x - min_tile_x)
            .expect("tile x offset is non-negative")
            .checked_mul(tile_width)
            .ok_or_else(|| "tile x offset is too large".to_string())?;
        let mosaic_y = u32::try_from(*tile_y - min_tile_y)
            .expect("tile y offset is non-negative")
            .checked_mul(tile_height)
            .ok_or_else(|| "tile y offset is too large".to_string())?;
        copy_dynmap_tile_rgb(
            &mut pixels,
            width,
            mosaic_x,
            mosaic_y,
            &tile.pixels,
            tile_width,
            tile_height,
        );
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    write_rgb_png(output_path, width, height, &pixels)?;
    let metadata_path = dynmap_metadata_path(output_path);
    let total_tile_slots = span_x
        .checked_mul(span_y)
        .ok_or_else(|| "Dynmap tile span is too large".to_string())?;
    let report = DynmapTileMosaicReport {
        tile_directory: tile_directory.to_path_buf(),
        output_path: output_path.to_path_buf(),
        metadata_path,
        level_prefix: normalized_prefix,
        tile_count: tiles.len(),
        tile_width,
        tile_height,
        min_tile_x,
        max_tile_x,
        min_tile_y,
        max_tile_y,
        width,
        height,
        missing_tile_count: total_tile_slots.saturating_sub(tiles.len()),
        empty_tile_count: 0,
    };
    std::fs::write(&report.metadata_path, report.properties_text())
        .map_err(|error| error.to_string())?;
    Ok(report)
}

fn normalize_dynmap_level_prefix(level_prefix: &str) -> String {
    let trimmed = level_prefix.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("base")
        || trimmed.eq_ignore_ascii_case("native")
    {
        return "base".to_string();
    }
    trimmed
        .to_ascii_lowercase()
        .trim_end_matches('_')
        .to_string()
}

fn collect_dynmap_tiles(
    directory: &Path,
    level_prefix: &str,
    tiles: &mut BTreeMap<(i32, i32), std::path::PathBuf>,
) -> std::result::Result<(), String> {
    let entries = std::fs::read_dir(directory).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            collect_dynmap_tiles(&path, level_prefix, tiles)?;
        } else if file_type.is_file() && is_supported_dynmap_image(&path) {
            collect_dynmap_tile_path(&path, level_prefix, tiles)?;
        }
    }
    Ok(())
}

fn collect_dynmap_tile_path(
    path: &Path,
    level_prefix: &str,
    tiles: &mut BTreeMap<(i32, i32), std::path::PathBuf>,
) -> std::result::Result<(), String> {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return Ok(());
    };
    let Some(coord) = parse_dynmap_tile_coord(stem, level_prefix) else {
        return Ok(());
    };
    if let Some(previous) = tiles.insert(coord, path.to_path_buf()) {
        return Err(format!(
            "duplicate Dynmap tile coordinate {},{}: {} and {}",
            coord.0,
            coord.1,
            previous.display(),
            path.display()
        ));
    }
    Ok(())
}

fn parse_dynmap_tile_coord(stem: &str, level_prefix: &str) -> Option<(i32, i32)> {
    let coord_text = if level_prefix == "base" {
        stem
    } else {
        stem.strip_prefix(&format!("{level_prefix}_"))?
    };
    let mut parts = coord_text.split('_');
    let x = parts.next()?.parse::<i32>().ok()?;
    let y = parts.next()?.parse::<i32>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((x, y))
}

fn is_supported_dynmap_image(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|name| {
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg")
        })
        .unwrap_or(false)
}

fn dynmap_tile_span(min: i32, max: i32, axis: &str) -> std::result::Result<usize, String> {
    let span = i64::from(max) - i64::from(min) + 1;
    usize::try_from(span).map_err(|_| format!("Dynmap tile {axis} span is too large"))
}

fn checked_u32_product(count: usize, size: u32, label: &str) -> std::result::Result<u32, String> {
    let product = count
        .checked_mul(usize::try_from(size).expect("u32 tile size fits usize"))
        .ok_or_else(|| format!("{label} is too large"))?;
    u32::try_from(product).map_err(|_| format!("{label} is too large"))
}

fn dynmap_metadata_path(output_path: &Path) -> std::path::PathBuf {
    let file_name = output_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("dynmap-mosaic.png");
    let stem = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file_name);
    output_path.with_file_name(format!("{stem}.properties"))
}

#[derive(Clone, Debug)]
struct DynmapTileImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fn read_dynmap_tile_image(path: &Path) -> std::result::Result<DynmapTileImage, String> {
    let image = image::open(path)
        .map_err(|error| format!("unsupported image: {} ({error})", path.display()))?
        .to_rgb8();
    Ok(DynmapTileImage {
        width: image.width(),
        height: image.height(),
        pixels: image.into_raw(),
    })
}

fn copy_dynmap_tile_rgb(
    mosaic: &mut [u8],
    mosaic_width: u32,
    x: u32,
    y: u32,
    tile_pixels: &[u8],
    tile_width: u32,
    tile_height: u32,
) {
    let mosaic_width = usize::try_from(mosaic_width).expect("mosaic width fits usize");
    let x = usize::try_from(x).expect("tile x fits usize");
    let y = usize::try_from(y).expect("tile y fits usize");
    let tile_width = usize::try_from(tile_width).expect("tile width fits usize");
    let tile_height = usize::try_from(tile_height).expect("tile height fits usize");
    for row in 0..tile_height {
        let source_offset = row * tile_width * 3;
        let target_offset = (((y + row) * mosaic_width) + x) * 3;
        mosaic[target_offset..target_offset + (tile_width * 3)]
            .copy_from_slice(&tile_pixels[source_offset..source_offset + (tile_width * 3)]);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TopdownFormat {
    Mca,
    Linear,
}

impl TopdownFormat {
    fn extension(self) -> &'static str {
        match self {
            TopdownFormat::Mca => "mca",
            TopdownFormat::Linear => "linear",
        }
    }

    fn success_title(self) -> &'static str {
        match self {
            TopdownFormat::Mca => "MCA top-down render written",
            TopdownFormat::Linear => "Linear top-down render written",
        }
    }

    fn error_title(self) -> &'static str {
        match self {
            TopdownFormat::Mca => "MCA top-down render failed",
            TopdownFormat::Linear => "Linear top-down render failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TopdownMode {
    Visible,
    Terrain,
}

impl TopdownMode {
    fn parse(text: Option<&str>) -> std::result::Result<Self, String> {
        match text.unwrap_or("visible").to_ascii_lowercase().as_str() {
            "visible" | "top" => Ok(Self::Visible),
            "terrain" | "surface" => Ok(Self::Terrain),
            _ => Err("mode must be visible or terrain".to_string()),
        }
    }

    fn as_text(self) -> &'static str {
        match self {
            TopdownMode::Visible => "visible",
            TopdownMode::Terrain => "terrain",
        }
    }
}

#[derive(Clone, Debug, Default)]
struct TopdownStats {
    region_count: usize,
    missing_regions: usize,
    chunk_count: usize,
    missing_chunks: usize,
    column_count: u64,
    air_columns: u64,
    water_top_columns: u64,
    leaf_top_columns: u64,
}

#[derive(Clone, Debug)]
struct TopdownReport {
    world_dir: std::path::PathBuf,
    output_path: std::path::PathBuf,
    metadata_path: std::path::PathBuf,
    start_region_x: i32,
    start_region_z: i32,
    columns: i32,
    rows: i32,
    width: usize,
    height: usize,
    mode: TopdownMode,
    stats: TopdownStats,
}

impl TopdownReport {
    fn properties_text(&self) -> String {
        format!(
            concat!(
                "worldDir={}\n",
                "outputPath={}\n",
                "metadataPath={}\n",
                "startRegionX={}\n",
                "startRegionZ={}\n",
                "columns={}\n",
                "rows={}\n",
                "width={}\n",
                "height={}\n",
                "mode={}\n",
                "regionCount={}\n",
                "missingRegions={}\n",
                "chunkCount={}\n",
                "missingChunks={}\n",
                "columnCount={}\n",
                "airColumns={}\n",
                "waterTopColumns={}\n",
                "leafTopColumns={}\n"
            ),
            normalized_path_display(&self.world_dir),
            normalized_path_display(&self.output_path),
            normalized_path_display(&self.metadata_path),
            self.start_region_x,
            self.start_region_z,
            self.columns,
            self.rows,
            self.width,
            self.height,
            self.mode.as_text(),
            self.stats.region_count,
            self.stats.missing_regions,
            self.stats.chunk_count,
            self.stats.missing_chunks,
            self.stats.column_count,
            self.stats.air_columns,
            self.stats.water_top_columns,
            self.stats.leaf_top_columns
        )
    }
}

#[derive(Clone, Debug)]
struct TopBlock {
    block_name: String,
    biome_id: String,
}

fn topdown_render_cli(
    out: &mut impl Write,
    err: &mut impl Write,
    format: TopdownFormat,
    world_dir: &str,
    output_path: &str,
    start_region_x: &str,
    start_region_z: &str,
    columns: &str,
    rows: &str,
    mode: Option<&str>,
) -> io::Result<i32> {
    match topdown_render_impl(
        format,
        Path::new(world_dir),
        Path::new(output_path),
        start_region_x,
        start_region_z,
        columns,
        rows,
        mode,
    ) {
        Ok(report) => {
            writeln!(out, "{}", format.success_title())?;
            write_topdown_report(out, &report)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "{}: {error}", format.error_title())?;
            Ok(EXIT_USAGE)
        }
    }
}

fn write_topdown_report(out: &mut impl Write, report: &TopdownReport) -> io::Result<()> {
    writeln!(
        out,
        "worldDir={}",
        normalized_path_display(&report.world_dir)
    )?;
    writeln!(
        out,
        "outputPath={}",
        normalized_path_display(&report.output_path)
    )?;
    writeln!(
        out,
        "metadataPath={}",
        normalized_path_display(&report.metadata_path)
    )?;
    writeln!(out, "startRegionX={}", report.start_region_x)?;
    writeln!(out, "startRegionZ={}", report.start_region_z)?;
    writeln!(out, "columns={}", report.columns)?;
    writeln!(out, "rows={}", report.rows)?;
    writeln!(out, "width={}", report.width)?;
    writeln!(out, "height={}", report.height)?;
    writeln!(out, "mode={}", report.mode.as_text())?;
    writeln!(out, "regionCount={}", report.stats.region_count)?;
    writeln!(out, "missingRegions={}", report.stats.missing_regions)?;
    writeln!(out, "chunkCount={}", report.stats.chunk_count)?;
    writeln!(out, "missingChunks={}", report.stats.missing_chunks)?;
    writeln!(out, "columnCount={}", report.stats.column_count)?;
    writeln!(out, "airColumns={}", report.stats.air_columns)?;
    writeln!(out, "waterTopColumns={}", report.stats.water_top_columns)?;
    writeln!(out, "leafTopColumns={}", report.stats.leaf_top_columns)
}

fn topdown_render_impl(
    format: TopdownFormat,
    world_dir: &Path,
    output_path: &Path,
    start_region_x_text: &str,
    start_region_z_text: &str,
    columns_text: &str,
    rows_text: &str,
    mode_text: Option<&str>,
) -> std::result::Result<TopdownReport, String> {
    let start_region_x = parse_i32_string(start_region_x_text)?;
    let start_region_z = parse_i32_string(start_region_z_text)?;
    let columns = parse_positive_i32_string("columns", columns_text)?;
    let rows = parse_positive_i32_string("rows", rows_text)?;
    let mode = TopdownMode::parse(mode_text)?;
    let region_dir = world_dir.join("region");
    if !region_dir.is_dir() {
        return Err(format!(
            "world region directory not found: {}",
            region_dir.display()
        ));
    }

    let end_region_x = start_region_x
        .checked_add(columns)
        .ok_or_else(|| "startRegionX + cols is outside i32 range".to_string())?;
    let end_region_z = start_region_z
        .checked_add(rows)
        .ok_or_else(|| "startRegionZ + rows is outside i32 range".to_string())?;
    let width = topdown_image_dimension("columns", columns)?;
    let height = topdown_image_dimension("rows", rows)?;
    let pixel_bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| "top-down output image is too large".to_string())?;
    let mut pixels = vec![0u8; pixel_bytes];
    let mut stats = TopdownStats::default();

    for rz in start_region_z..end_region_z {
        for rx in start_region_x..end_region_x {
            let region_path = region_dir.join(format!("r.{rx}.{rz}.{}", format.extension()));
            if !region_path.is_file() {
                stats.missing_regions += 1;
                continue;
            }
            let payloads = read_region_payloads(&region_path).map_err(|error| error.to_string())?;
            let expected_format = match format {
                TopdownFormat::Mca => RegionFormat::Mca,
                TopdownFormat::Linear => RegionFormat::Linear,
            };
            if payloads.format != expected_format {
                return Err(format!(
                    "region format mismatch for {}: expected {} got {}",
                    region_path.display(),
                    expected_format.as_manifest_value(),
                    payloads.format.as_manifest_value()
                ));
            }
            stats.region_count += 1;
            let region_image_x = usize::try_from(rx - start_region_x)
                .expect("region x offset is non-negative")
                * TOPDOWN_REGION_SIZE_BLOCKS;
            let region_image_z = usize::try_from(rz - start_region_z)
                .expect("region z offset is non-negative")
                * TOPDOWN_REGION_SIZE_BLOCKS;
            for chunk_z in 0..32u8 {
                for chunk_x in 0..32u8 {
                    let pos =
                        ChunkLocalPos::new(chunk_x, chunk_z).map_err(|error| error.to_string())?;
                    let Some(payload) = payloads.chunks.get(&pos) else {
                        stats.missing_chunks += 1;
                        continue;
                    };
                    let chunk = TopdownChunk::decode(payload)?;
                    stats.chunk_count += 1;
                    for local_z in 0..CHUNK_WIDTH {
                        for local_x in 0..CHUNK_WIDTH {
                            let top = chunk.top_block(local_x, local_z, mode)?;
                            let image_x =
                                region_image_x + (usize::from(chunk_x) * CHUNK_WIDTH) + local_x;
                            let image_z =
                                region_image_z + (usize::from(chunk_z) * CHUNK_WIDTH) + local_z;
                            let color = topdown_color_for(&top.block_name, &top.biome_id);
                            write_rgb_pixel(&mut pixels, width, image_x, image_z, color);
                            stats.column_count += 1;
                            if is_air_block(&top.block_name) {
                                stats.air_columns += 1;
                            } else if is_water_like_block(&top.block_name) {
                                stats.water_top_columns += 1;
                            } else if is_leaf_block_name(&top.block_name) {
                                stats.leaf_top_columns += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    write_rgb_png(output_path, width as u32, height as u32, &pixels)?;
    let metadata_path = topdown_metadata_path(output_path);
    let report = TopdownReport {
        world_dir: world_dir.to_path_buf(),
        output_path: output_path.to_path_buf(),
        metadata_path,
        start_region_x,
        start_region_z,
        columns,
        rows,
        width,
        height,
        mode,
        stats,
    };
    std::fs::write(&report.metadata_path, report.properties_text())
        .map_err(|error| error.to_string())?;
    Ok(report)
}

fn topdown_image_dimension(name: &str, regions: i32) -> std::result::Result<usize, String> {
    let dimension = usize::try_from(regions)
        .expect("positive region count fits usize")
        .checked_mul(TOPDOWN_REGION_SIZE_BLOCKS)
        .ok_or_else(|| format!("{name} makes the top-down image too large"))?;
    if dimension > u32::MAX as usize {
        return Err(format!("{name} makes the top-down PNG dimension too large"));
    }
    Ok(dimension)
}

fn write_rgb_pixel(pixels: &mut [u8], width: usize, x: usize, z: usize, color: (u8, u8, u8)) {
    let offset = ((z * width) + x) * 3;
    pixels[offset] = color.0;
    pixels[offset + 1] = color.1;
    pixels[offset + 2] = color.2;
}

fn write_rgb_png(
    path: &Path,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> std::result::Result<(), String> {
    let file = File::create(path).map_err(|error| error.to_string())?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png = encoder.write_header().map_err(|error| error.to_string())?;
    png.write_image_data(pixels)
        .map_err(|error| error.to_string())
}

fn topdown_metadata_path(output_path: &Path) -> std::path::PathBuf {
    let file_name = output_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("topdown.png");
    let stem = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file_name);
    output_path.with_file_name(format!("{stem}.properties"))
}

#[derive(Clone, Debug)]
struct TopdownChunk {
    sections: BTreeMap<i32, TopdownSection>,
}

impl TopdownChunk {
    fn decode(payload: &[u8]) -> std::result::Result<Self, String> {
        let root = chunk_root(payload)?;
        let sections_tag = root
            .get_list("sections")
            .map_err(|error| error.to_string())?;
        let mut sections = BTreeMap::new();
        for section_tag in sections_tag.values() {
            let Tag::Compound(section) = section_tag else {
                return Err("chunk section is not an NBT compound".to_string());
            };
            if !section.contains("block_states") {
                continue;
            }
            let decoded = TopdownSection::decode(section)?;
            sections.insert(decoded.section_y, decoded);
        }
        Ok(Self { sections })
    }

    fn top_block(
        &self,
        local_x: usize,
        local_z: usize,
        mode: TopdownMode,
    ) -> std::result::Result<TopBlock, String> {
        for y in (OVERWORLD_1_21_11.min_y()..=OVERWORLD_1_21_11.max_y_inclusive()).rev() {
            let block_name = self.block_name_at(local_x, y, local_z)?;
            if is_air_block(&block_name) {
                continue;
            }
            if mode == TopdownMode::Terrain
                && (is_plant_like_block(&block_name)
                    || is_leaf_block_name(&block_name)
                    || is_log_block(&block_name))
            {
                continue;
            }
            return Ok(TopBlock {
                biome_id: self.biome_id_at(local_x, y, local_z)?,
                block_name,
            });
        }
        Ok(TopBlock {
            block_name: TOPDOWN_AIR_BLOCK.to_string(),
            biome_id: TOPDOWN_DEFAULT_BIOME.to_string(),
        })
    }

    fn block_name_at(
        &self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<String, String> {
        let section_y = y.div_euclid(SECTION_HEIGHT);
        let Some(section) = self.sections.get(&section_y) else {
            return Ok(TOPDOWN_AIR_BLOCK.to_string());
        };
        section.block_name_at(local_x, y, local_z)
    }

    fn biome_id_at(
        &self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<String, String> {
        let section_y = y.div_euclid(SECTION_HEIGHT);
        let Some(section) = self.sections.get(&section_y) else {
            return Ok(TOPDOWN_DEFAULT_BIOME.to_string());
        };
        section.biome_id_at(local_x, y, local_z)
    }
}

#[derive(Clone, Debug)]
struct TopdownSection {
    section_y: i32,
    block_palette: Vec<String>,
    block_values: Vec<usize>,
    biome_palette: Vec<String>,
    biome_values: Vec<usize>,
}

impl TopdownSection {
    fn decode(section: &earthmap_minecraft::nbt::Compound) -> std::result::Result<Self, String> {
        let section_y = i32::from(section.get_byte("Y").map_err(|error| error.to_string())?);
        let block_states = section
            .get_compound("block_states")
            .map_err(|error| error.to_string())?;
        let block_palette = block_state_palette_names(block_states)?;
        let block_values =
            decode_palette_values(block_states, block_palette.len(), SECTION_BLOCK_COUNT)?;
        let (biome_palette, biome_values) = if section.contains("biomes") {
            let biomes = section
                .get_compound("biomes")
                .map_err(|error| error.to_string())?;
            let palette = biome_palette_names(biomes)?;
            let values =
                decode_biome_palette_values(biomes, palette.len(), SECTION_BIOME_CELL_COUNT)?;
            (palette, values)
        } else {
            (
                vec![TOPDOWN_DEFAULT_BIOME.to_string()],
                vec![0; SECTION_BIOME_CELL_COUNT],
            )
        };
        Ok(Self {
            section_y,
            block_palette,
            block_values,
            biome_palette,
            biome_values,
        })
    }

    fn block_name_at(
        &self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<String, String> {
        let local_y = usize::try_from(y & (SECTION_HEIGHT - 1)).expect("local y is non-negative");
        let index = (local_y << 8) | (local_z << 4) | local_x;
        let palette_index = self
            .block_values
            .get(index)
            .copied()
            .ok_or_else(|| format!("missing block palette value at {index}"))?;
        self.block_palette
            .get(palette_index)
            .cloned()
            .ok_or_else(|| format!("block palette index outside palette: {palette_index}"))
    }

    fn biome_id_at(
        &self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<String, String> {
        let local_y = usize::try_from(y & (SECTION_HEIGHT - 1)).expect("local y is non-negative");
        let biome_x = local_x / BIOME_CELL_WIDTH;
        let biome_y = local_y / BIOME_CELL_WIDTH;
        let biome_z = local_z / BIOME_CELL_WIDTH;
        let index = (biome_y * BIOME_CELL_WIDTH * BIOME_CELL_WIDTH)
            + (biome_z * BIOME_CELL_WIDTH)
            + biome_x;
        let palette_index = self
            .biome_values
            .get(index)
            .copied()
            .ok_or_else(|| format!("missing biome palette value at {index}"))?;
        self.biome_palette
            .get(palette_index)
            .cloned()
            .ok_or_else(|| format!("biome palette index outside palette: {palette_index}"))
    }
}

fn topdown_color_for(block_name: &str, biome_id: &str) -> (u8, u8, u8) {
    if is_air_block(block_name) {
        return (0, 0, 0);
    }
    if is_water_like_block(block_name) {
        return water_color_for_biome(biome_id);
    }
    if is_log_block(block_name) {
        return (102, 75, 42);
    }
    if block_name == "minecraft:lava" {
        return (215, 78, 28);
    }
    if let Some(block_id) = render_block_state_id(block_name) {
        return earthmap_surface::render_surface_rgb(block_id, Some(biome_id));
    }
    fallback_color_for_block_name(block_name)
}

fn render_block_state_id(block_name: &str) -> Option<i32> {
    Some(match block_name {
        "minecraft:grass_block" => block_state_ids::GRASS_BLOCK,
        "minecraft:oak_leaves" => block_state_ids::OAK_LEAVES,
        "minecraft:jungle_leaves" => block_state_ids::JUNGLE_LEAVES,
        "minecraft:dark_oak_leaves" => block_state_ids::DARK_OAK_LEAVES,
        "minecraft:spruce_leaves" => block_state_ids::SPRUCE_LEAVES,
        "minecraft:moss_block" => block_state_ids::MOSS_BLOCK,
        "minecraft:podzol" => block_state_ids::PODZOL,
        "minecraft:coarse_dirt" => block_state_ids::COARSE_DIRT,
        "minecraft:dirt" => block_state_ids::DIRT,
        "minecraft:rooted_dirt" => block_state_ids::ROOTED_DIRT,
        "minecraft:mycelium" => block_state_ids::MYCELIUM,
        "minecraft:mud" => block_state_ids::MUD,
        "minecraft:packed_mud" => block_state_ids::PACKED_MUD,
        "minecraft:green_terracotta" => block_state_ids::GREEN_TERRACOTTA,
        "minecraft:lime_terracotta" => block_state_ids::LIME_TERRACOTTA,
        "minecraft:gray_terracotta" => block_state_ids::GRAY_TERRACOTTA,
        "minecraft:black_terracotta" => block_state_ids::BLACK_TERRACOTTA,
        "minecraft:black_concrete" => block_state_ids::BLACK_CONCRETE,
        "minecraft:sand" => block_state_ids::SAND,
        "minecraft:sandstone" => block_state_ids::SANDSTONE,
        "minecraft:end_stone" => block_state_ids::END_STONE,
        "minecraft:end_stone_bricks" => block_state_ids::END_STONE_BRICKS,
        "minecraft:smooth_sandstone" => block_state_ids::SMOOTH_SANDSTONE,
        "minecraft:cut_sandstone" => block_state_ids::CUT_SANDSTONE,
        "minecraft:chiseled_sandstone" => block_state_ids::CHISELED_SANDSTONE,
        "minecraft:smooth_red_sandstone" => block_state_ids::SMOOTH_RED_SANDSTONE,
        "minecraft:cut_red_sandstone" => block_state_ids::CUT_RED_SANDSTONE,
        "minecraft:chiseled_red_sandstone" => block_state_ids::CHISELED_RED_SANDSTONE,
        "minecraft:mud_bricks" => block_state_ids::MUD_BRICKS,
        "minecraft:dripstone_block" => block_state_ids::DRIPSTONE_BLOCK,
        "minecraft:yellow_terracotta" => block_state_ids::YELLOW_TERRACOTTA,
        "minecraft:white_terracotta" => block_state_ids::WHITE_TERRACOTTA,
        "minecraft:light_gray_terracotta" => block_state_ids::LIGHT_GRAY_TERRACOTTA,
        "minecraft:bone_block" => block_state_ids::BONE_BLOCK,
        "minecraft:calcite" => block_state_ids::CALCITE,
        "minecraft:quartz_block" => block_state_ids::QUARTZ_BLOCK,
        "minecraft:gravel" => block_state_ids::GRAVEL,
        "minecraft:terracotta" => block_state_ids::TERRACOTTA,
        "minecraft:red_sand" => block_state_ids::RED_SAND,
        "minecraft:orange_terracotta" => block_state_ids::ORANGE_TERRACOTTA,
        "minecraft:red_terracotta" => block_state_ids::RED_TERRACOTTA,
        "minecraft:brown_terracotta" => block_state_ids::BROWN_TERRACOTTA,
        "minecraft:granite" => block_state_ids::GRANITE,
        "minecraft:stone" => block_state_ids::STONE,
        "minecraft:tuff" => block_state_ids::TUFF,
        "minecraft:deepslate" => block_state_ids::DEEPSLATE,
        "minecraft:andesite" => block_state_ids::ANDESITE,
        "minecraft:diorite" => block_state_ids::DIORITE,
        "minecraft:cyan_terracotta" => block_state_ids::CYAN_TERRACOTTA,
        "minecraft:snow_block" => block_state_ids::SNOW_BLOCK,
        "minecraft:clay" => block_state_ids::CLAY,
        _ => return None,
    })
}

fn is_air_block(block_name: &str) -> bool {
    matches!(
        block_name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

fn is_water_like_block(block_name: &str) -> bool {
    block_name == "minecraft:water"
        || block_name.contains("seagrass")
        || block_name.contains("kelp")
}

fn is_leaf_block_name(block_name: &str) -> bool {
    block_name.ends_with("_leaves") || block_name.contains("azalea_leaves")
}

fn is_log_block(block_name: &str) -> bool {
    block_name.ends_with("_log")
        || block_name.ends_with("_wood")
        || block_name.ends_with("_stem")
        || block_name.ends_with("_hyphae")
}

fn is_plant_like_block(block_name: &str) -> bool {
    block_name.ends_with("_grass")
        || block_name.ends_with("_fern")
        || block_name.ends_with("_flower")
        || block_name.ends_with("_sapling")
        || block_name.ends_with("_bush")
        || matches!(
            block_name,
            "minecraft:fern"
                | "minecraft:bush"
                | "minecraft:glow_lichen"
                | "minecraft:lily_pad"
                | "minecraft:leaf_litter"
                | "minecraft:hanging_roots"
                | "minecraft:spore_blossom"
                | "minecraft:pointed_dripstone"
        )
        || block_name.contains("bamboo")
        || block_name.contains("cactus")
        || block_name.contains("coral")
        || block_name.contains("pickle")
        || block_name.contains("sugar_cane")
        || block_name.contains("cocoa")
        || block_name.contains("dandelion")
        || block_name.contains("poppy")
        || block_name.contains("melon")
        || block_name.contains("pumpkin")
        || block_name.contains("mushroom")
        || block_name.contains("roots")
        || block_name.contains("vines")
        || block_name.contains("vine")
}

fn water_color_for_biome(biome_id: &str) -> (u8, u8, u8) {
    let biome = biome_id.to_ascii_lowercase();
    if biome.contains("swamp") || biome.contains("mangrove") {
        return (38, 55, 38);
    }
    if biome.contains("deep") {
        if biome.contains("warm") {
            return (8, 40, 58);
        }
        if biome.contains("cold") || biome.contains("frozen") {
            return (10, 30, 58);
        }
        return (4, 18, 42);
    }
    if biome.contains("lukewarm") {
        return (14, 58, 84);
    }
    if biome.contains("warm") {
        return (18, 78, 94);
    }
    if biome.contains("cold") || biome.contains("frozen") {
        return (27, 61, 85);
    }
    if biome.contains("river") {
        return (16, 54, 89);
    }
    if biome.contains("ocean") {
        return (6, 24, 54);
    }
    (8, 28, 59)
}

fn fallback_color_for_block_name(block_name: &str) -> (u8, u8, u8) {
    let hash = java_string_hash_code(&block_name.to_ascii_lowercase()) as u32;
    (
        (64 + ((hash >> 16) & 0x7f)) as u8,
        (64 + ((hash >> 8) & 0x7f)) as u8,
        (64 + (hash & 0x7f)) as u8,
    )
}

fn java_string_hash_code(text: &str) -> i32 {
    let mut hash = 0i32;
    for unit in text.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    hash
}

fn convert_mca_region_to_linear(
    out: &mut impl Write,
    err: &mut impl Write,
    mca_region: &str,
    linear_region: &str,
) -> io::Result<i32> {
    match convert_mca_region_to_linear_impl(Path::new(mca_region), Path::new(linear_region)) {
        Ok(report) => {
            writeln!(out, "MCA region converted to Linear V2")?;
            writeln!(
                out,
                "mcaRegion={}",
                normalized_path_display(&report.mca_region)
            )?;
            writeln!(
                out,
                "linearRegion={}",
                normalized_path_display(&report.linear_region)
            )?;
            writeln!(out, "chunkCount={}", report.chunk_count)?;
            writeln!(out, "linearChunkCount={}", report.linear_chunk_count)?;
            writeln!(out, "linearFileBytes={}", report.linear_file_bytes)?;
            Ok(if report.chunk_count == report.linear_chunk_count {
                EXIT_OK
            } else {
                EXIT_USAGE
            })
        }
        Err(error) => {
            writeln!(err, "MCA to Linear conversion failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn convert_mca_world_to_linear(
    out: &mut impl Write,
    err: &mut impl Write,
    mca_world: &str,
    linear_world: &str,
) -> io::Result<i32> {
    match convert_mca_world_to_linear_impl(Path::new(mca_world), Path::new(linear_world)) {
        Ok(report) => {
            writeln!(out, "MCA world converted to Linear V2")?;
            writeln!(
                out,
                "sourceWorld={}",
                normalized_path_display(&report.source_world)
            )?;
            writeln!(
                out,
                "linearWorld={}",
                normalized_path_display(&report.linear_world)
            )?;
            writeln!(out, "regions={}", report.regions)?;
            writeln!(out, "chunks={}", report.chunks)?;
            writeln!(
                out,
                "regionDir={}",
                normalized_path_display(&report.region_dir)
            )?;
            Ok(if report.regions > 0 {
                EXIT_OK
            } else {
                EXIT_USAGE
            })
        }
        Err(error) => {
            writeln!(err, "MCA world to Linear conversion failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[derive(Clone, Debug)]
struct ConvertRegionReport {
    mca_region: std::path::PathBuf,
    linear_region: std::path::PathBuf,
    chunk_count: usize,
    linear_chunk_count: usize,
    linear_file_bytes: u64,
}

#[derive(Clone, Debug)]
struct ConvertWorldReport {
    source_world: std::path::PathBuf,
    linear_world: std::path::PathBuf,
    regions: usize,
    chunks: usize,
    region_dir: std::path::PathBuf,
}

fn convert_mca_region_to_linear_impl(
    mca_region: &Path,
    linear_region: &Path,
) -> std::result::Result<ConvertRegionReport, String> {
    let payloads = read_region_payloads(mca_region).map_err(|error| error.to_string())?;
    if payloads.format != RegionFormat::Mca {
        return Err(format!(
            "source region must be MCA: {}",
            mca_region.display()
        ));
    }
    earthmap_region::write_linear_v2_region(linear_region, &payloads.chunks, 0)
        .map_err(|error| error.to_string())?;
    let validation =
        validate_linear_region_file(linear_region).map_err(|error| error.to_string())?;
    Ok(ConvertRegionReport {
        mca_region: mca_region.to_path_buf(),
        linear_region: linear_region.to_path_buf(),
        chunk_count: payloads.chunks.len(),
        linear_chunk_count: validation.chunk_count,
        linear_file_bytes: validation.file_bytes,
    })
}

fn convert_mca_world_to_linear_impl(
    source_world: &Path,
    linear_world: &Path,
) -> std::result::Result<ConvertWorldReport, String> {
    let source_region_dir = source_world.join("region");
    if !source_region_dir.is_dir() {
        return Err(format!(
            "source world has no region directory: {}",
            source_region_dir.display()
        ));
    }
    let target_region_dir = linear_world.join("region");
    std::fs::create_dir_all(&target_region_dir).map_err(|error| error.to_string())?;
    copy_if_regular(
        &source_world.join("level.dat"),
        &linear_world.join("level.dat"),
    )?;
    copy_if_regular(
        &source_world.join(SURVIVAL_MANIFEST_FILE_NAME),
        &linear_world.join(SURVIVAL_MANIFEST_FILE_NAME),
    )?;
    copy_if_regular(
        &source_world.join("earthmap-region-batch.properties"),
        &linear_world.join("earthmap-region-batch.properties"),
    )?;
    copy_if_regular(
        &source_world.join("earthmap-region-progress.csv"),
        &linear_world.join("earthmap-region-progress.csv"),
    )?;

    let mut mca_regions = std::fs::read_dir(&source_region_dir)
        .map_err(|error| error.to_string())?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("mca"))
        })
        .collect::<Vec<_>>();
    mca_regions.sort();

    let mut regions = 0usize;
    let mut chunks = 0usize;
    for mca_region in mca_regions {
        let file_name = mca_region
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("region path has no file name: {}", mca_region.display()))?;
        let linear_file_name = format!("{}.linear", file_name.trim_end_matches(".mca"));
        let linear_region = target_region_dir.join(linear_file_name);
        let report = convert_mca_region_to_linear_impl(&mca_region, &linear_region)?;
        if report.linear_chunk_count != report.chunk_count {
            return Err(format!(
                "converted region chunk count mismatch: {}",
                linear_region.display()
            ));
        }
        regions += 1;
        chunks += report.chunk_count;
    }

    Ok(ConvertWorldReport {
        source_world: source_world.to_path_buf(),
        linear_world: linear_world.to_path_buf(),
        regions,
        chunks,
        region_dir: target_region_dir,
    })
}

fn copy_if_regular(source: &Path, target: &Path) -> std::result::Result<(), String> {
    if !source.is_file() {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::copy(source, target).map_err(|error| error.to_string())?;
    Ok(())
}

#[derive(Clone, Debug, Default)]
struct BlockPaletteScan {
    region_chunk_count: usize,
    decoded_chunk_count: usize,
    decoded_section_count: usize,
    palette_entry_count: usize,
    hits: BTreeMap<String, usize>,
}

#[derive(Clone, Copy, Debug)]
struct OreKindSpec {
    id: &'static str,
    survival_critical: bool,
}

const ORE_KIND_SPECS: &[OreKindSpec] = &[
    OreKindSpec {
        id: "coal",
        survival_critical: true,
    },
    OreKindSpec {
        id: "iron",
        survival_critical: true,
    },
    OreKindSpec {
        id: "copper",
        survival_critical: true,
    },
    OreKindSpec {
        id: "gold",
        survival_critical: true,
    },
    OreKindSpec {
        id: "redstone",
        survival_critical: true,
    },
    OreKindSpec {
        id: "lapis",
        survival_critical: true,
    },
    OreKindSpec {
        id: "diamond",
        survival_critical: true,
    },
    OreKindSpec {
        id: "emerald",
        survival_critical: false,
    },
];

#[derive(Clone, Debug, Default)]
struct BiomePaletteScan {
    region_chunk_count: usize,
    decoded_chunk_count: usize,
    decoded_section_count: usize,
    biome_palette_entry_count: usize,
    mixed_biome_section_count: usize,
    hits: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Default)]
struct StatusScan {
    region_chunk_count: usize,
    decoded_chunk_count: usize,
    counts: BTreeMap<String, usize>,
}

fn validate_survival_palette(
    out: &mut impl Write,
    err: &mut impl Write,
    region: &str,
    title: &str,
    error_prefix: &str,
) -> io::Result<i32> {
    match scan_block_palettes(region) {
        Ok(report) => {
            let has_deepslate = palette_contains(&report, "minecraft:deepslate");
            let mut survival_critical_complete = has_deepslate;
            let mut all_ores_present = true;
            writeln!(out, "{title}")?;
            writeln!(out, "regionChunkCount={}", report.region_chunk_count)?;
            writeln!(out, "decodedChunkCount={}", report.decoded_chunk_count)?;
            writeln!(out, "decodedSectionCount={}", report.decoded_section_count)?;
            writeln!(out, "paletteEntryCount={}", report.palette_entry_count)?;
            writeln!(out, "has.deepslate={has_deepslate}")?;
            writeln!(
                out,
                "has.water={}",
                palette_contains(&report, "minecraft:water")
            )?;
            writeln!(
                out,
                "has.lava={}",
                palette_contains(&report, "minecraft:lava")
            )?;
            writeln!(
                out,
                "paletteHits.minecraft:water={}",
                palette_hits(&report, "minecraft:water")
            )?;
            writeln!(
                out,
                "paletteHits.minecraft:lava={}",
                palette_hits(&report, "minecraft:lava")
            )?;
            for kind in ORE_KIND_SPECS {
                let stone_name = format!("minecraft:{}_ore", kind.id);
                let deepslate_name = format!("minecraft:deepslate_{}_ore", kind.id);
                let present = palette_contains(&report, &stone_name)
                    || palette_contains(&report, &deepslate_name);
                writeln!(out, "has.ore.{}={present}", kind.id)?;
                writeln!(
                    out,
                    "paletteHits.{stone_name}={}",
                    palette_hits(&report, &stone_name)
                )?;
                writeln!(
                    out,
                    "paletteHits.{deepslate_name}={}",
                    palette_hits(&report, &deepslate_name)
                )?;
                if kind.survival_critical {
                    survival_critical_complete &= present;
                }
                all_ores_present &= present;
            }
            writeln!(out, "survivalCriticalComplete={survival_critical_complete}")?;
            writeln!(out, "allOresPresent={all_ores_present}")?;
            Ok(if survival_critical_complete {
                EXIT_OK
            } else {
                EXIT_USAGE
            })
        }
        Err(error) => {
            writeln!(err, "{error_prefix}: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn palette_contains(report: &BlockPaletteScan, name: &str) -> bool {
    palette_hits(report, name) > 0
}

fn palette_hits(report: &BlockPaletteScan, name: &str) -> usize {
    report.hits.get(name).copied().unwrap_or(0)
}

fn inspect_block_palettes(
    out: &mut impl Write,
    err: &mut impl Write,
    region: &str,
    title: &str,
    error_prefix: &str,
) -> io::Result<i32> {
    match scan_block_palettes(region) {
        Ok(report) => {
            writeln!(out, "{title}")?;
            writeln!(out, "regionChunkCount={}", report.region_chunk_count)?;
            writeln!(out, "decodedChunkCount={}", report.decoded_chunk_count)?;
            writeln!(out, "decodedSectionCount={}", report.decoded_section_count)?;
            writeln!(out, "paletteEntryCount={}", report.palette_entry_count)?;
            for (name, count) in report.hits {
                writeln!(out, "paletteHits.{name}={count}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "{error_prefix}: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn inspect_biomes(
    out: &mut impl Write,
    err: &mut impl Write,
    region: &str,
    title: &str,
    error_prefix: &str,
) -> io::Result<i32> {
    match scan_biomes(region) {
        Ok(report) => {
            writeln!(out, "{title}")?;
            writeln!(out, "regionChunkCount={}", report.region_chunk_count)?;
            writeln!(out, "decodedChunkCount={}", report.decoded_chunk_count)?;
            writeln!(out, "decodedSectionCount={}", report.decoded_section_count)?;
            writeln!(
                out,
                "biomePaletteEntryCount={}",
                report.biome_palette_entry_count
            )?;
            writeln!(
                out,
                "mixedBiomeSectionCount={}",
                report.mixed_biome_section_count
            )?;
            for (name, count) in report.hits {
                writeln!(out, "biomePaletteHits.{name}={count}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "{error_prefix}: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn inspect_statuses(
    out: &mut impl Write,
    err: &mut impl Write,
    region: &str,
    title: &str,
    error_prefix: &str,
) -> io::Result<i32> {
    match scan_statuses(region) {
        Ok(report) => {
            writeln!(out, "{title}")?;
            writeln!(out, "regionChunkCount={}", report.region_chunk_count)?;
            writeln!(out, "decodedChunkCount={}", report.decoded_chunk_count)?;
            for (status, count) in report.counts {
                writeln!(out, "status.{status}={count}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "{error_prefix}: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn scan_block_palettes(region: &str) -> std::result::Result<BlockPaletteScan, String> {
    let payloads = read_region_payloads(region).map_err(|error| error.to_string())?;
    let mut report = BlockPaletteScan {
        region_chunk_count: payloads.chunks.len(),
        ..BlockPaletteScan::default()
    };
    for payload in payloads.chunks.values() {
        let root = chunk_root(payload)?;
        report.decoded_chunk_count += 1;
        let sections = root
            .get_list("sections")
            .map_err(|error| error.to_string())?;
        for section_tag in sections.values() {
            let Tag::Compound(section) = section_tag else {
                return Err("chunk section is not an NBT compound".to_string());
            };
            if !section.contains("block_states") {
                continue;
            }
            let block_states = section
                .get_compound("block_states")
                .map_err(|error| error.to_string())?;
            if !block_states.contains("palette") {
                continue;
            }
            report.decoded_section_count += 1;
            for name in block_state_palette_names(block_states)? {
                *report.hits.entry(name).or_insert(0) += 1;
                report.palette_entry_count += 1;
            }
        }
    }
    Ok(report)
}

fn scan_biomes(region: &str) -> std::result::Result<BiomePaletteScan, String> {
    let payloads = read_region_payloads(region).map_err(|error| error.to_string())?;
    let mut report = BiomePaletteScan {
        region_chunk_count: payloads.chunks.len(),
        ..BiomePaletteScan::default()
    };
    for payload in payloads.chunks.values() {
        let root = chunk_root(payload)?;
        report.decoded_chunk_count += 1;
        let sections = root
            .get_list("sections")
            .map_err(|error| error.to_string())?;
        for section_tag in sections.values() {
            let Tag::Compound(section) = section_tag else {
                return Err("chunk section is not an NBT compound".to_string());
            };
            if !section.contains("biomes") {
                continue;
            }
            let biomes = section
                .get_compound("biomes")
                .map_err(|error| error.to_string())?;
            if !biomes.contains("palette") {
                continue;
            }
            report.decoded_section_count += 1;
            let palette = biome_palette_names(biomes)?;
            if palette.len() > 1 {
                report.mixed_biome_section_count += 1;
            }
            for name in palette {
                *report.hits.entry(name).or_insert(0) += 1;
                report.biome_palette_entry_count += 1;
            }
        }
    }
    Ok(report)
}

fn scan_statuses(region: &str) -> std::result::Result<StatusScan, String> {
    let payloads = read_region_payloads(region).map_err(|error| error.to_string())?;
    let mut report = StatusScan {
        region_chunk_count: payloads.chunks.len(),
        ..StatusScan::default()
    };
    for payload in payloads.chunks.values() {
        let root = chunk_root(payload)?;
        report.decoded_chunk_count += 1;
        let status = root
            .get_string("Status")
            .map_err(|error| error.to_string())?
            .to_string();
        *report.counts.entry(status).or_insert(0) += 1;
    }
    Ok(report)
}

#[derive(Clone, Debug, Default)]
struct PostFinalIntegrityReport {
    region_chunk_count: usize,
    decoded_chunk_count: usize,
    decoded_section_count: usize,
    scanned_columns: u64,
    land_columns: u64,
    water_columns: u64,
    dry_below_sea_columns: u64,
    underwater_air_columns: u64,
    tree_log_blocks: u64,
    tree_leaf_blocks: u64,
    tree_log_columns: u64,
    tree_leaf_columns: u64,
    coast_edge_samples: u64,
    coast_land_above_sea_gt4_samples: u64,
    coast_land_above_sea_gt8_samples: u64,
    coast_land_above_sea_gt16_samples: u64,
    max_coast_land_above_sea_delta: i32,
    max_coast_floor_delta: i32,
    top_terrain_block_hits: BTreeMap<String, u64>,
    coast_offender_samples: Vec<CoastOffenderSample>,
}

impl PostFinalIntegrityReport {
    fn underwater_air_column_ratio(&self) -> f64 {
        if self.water_columns == 0 {
            0.0
        } else {
            self.underwater_air_columns as f64 / self.water_columns as f64
        }
    }

    fn tree_leaf_column_ratio(&self) -> f64 {
        if self.land_columns == 0 {
            0.0
        } else {
            self.tree_leaf_columns as f64 / self.land_columns as f64
        }
    }

    fn dry_below_sea_column_ratio(&self) -> f64 {
        if self.scanned_columns == 0 {
            0.0
        } else {
            self.dry_below_sea_columns as f64 / self.scanned_columns as f64
        }
    }
}

#[derive(Clone, Debug)]
struct CoastOffenderSample {
    land_local_block_x: usize,
    land_local_block_z: usize,
    water_local_block_x: usize,
    water_local_block_z: usize,
    land_surface_y: i32,
    water_floor_y: i32,
    land_surface_block: String,
    water_floor_block: String,
    land_above_sea: i32,
    floor_delta: i32,
}

#[derive(Clone, Debug)]
struct PostFinalColumnSnapshot {
    region_local_x: usize,
    region_local_z: usize,
    terrain_surface_y: i32,
    terrain_surface_block: String,
    open_water_column: bool,
}

impl PostFinalColumnSnapshot {
    fn has_terrain(&self) -> bool {
        self.terrain_surface_y != i32::MIN
    }

    fn land_at_coast(&self) -> bool {
        self.has_terrain() && !self.open_water_column && self.terrain_surface_y >= SEA_LEVEL_Y
    }
}

fn inspect_post_final_integrity(
    out: &mut impl Write,
    err: &mut impl Write,
    region: &str,
    expected_format: RegionFormat,
    title: &str,
    error_prefix: &str,
) -> io::Result<i32> {
    match scan_post_final_integrity(region, expected_format) {
        Ok(report) => {
            writeln!(out, "{title}")?;
            write_post_final_integrity_report(out, &report)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "{error_prefix}: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn write_post_final_integrity_report(
    out: &mut impl Write,
    report: &PostFinalIntegrityReport,
) -> io::Result<()> {
    writeln!(out, "regionChunkCount={}", report.region_chunk_count)?;
    writeln!(out, "decodedChunkCount={}", report.decoded_chunk_count)?;
    writeln!(out, "decodedSectionCount={}", report.decoded_section_count)?;
    writeln!(out, "scannedColumns={}", report.scanned_columns)?;
    writeln!(out, "landColumns={}", report.land_columns)?;
    writeln!(out, "waterColumns={}", report.water_columns)?;
    writeln!(out, "dryBelowSeaColumns={}", report.dry_below_sea_columns)?;
    writeln!(
        out,
        "dryBelowSeaColumnRatio={}",
        java_double_string(report.dry_below_sea_column_ratio())
    )?;
    writeln!(
        out,
        "underwaterAirColumns={}",
        report.underwater_air_columns
    )?;
    writeln!(
        out,
        "underwaterAirColumnRatio={}",
        java_double_string(report.underwater_air_column_ratio())
    )?;
    writeln!(out, "treeLogBlocks={}", report.tree_log_blocks)?;
    writeln!(out, "treeLeafBlocks={}", report.tree_leaf_blocks)?;
    writeln!(out, "treeLogColumns={}", report.tree_log_columns)?;
    writeln!(out, "treeLeafColumns={}", report.tree_leaf_columns)?;
    writeln!(
        out,
        "treeLeafColumnRatio={}",
        java_double_string(report.tree_leaf_column_ratio())
    )?;
    writeln!(out, "coastEdgeSamples={}", report.coast_edge_samples)?;
    writeln!(
        out,
        "coastLandAboveSeaGt4Samples={}",
        report.coast_land_above_sea_gt4_samples
    )?;
    writeln!(
        out,
        "coastLandAboveSeaGt8Samples={}",
        report.coast_land_above_sea_gt8_samples
    )?;
    writeln!(
        out,
        "coastLandAboveSeaGt16Samples={}",
        report.coast_land_above_sea_gt16_samples
    )?;
    writeln!(
        out,
        "maxCoastLandAboveSeaDelta={}",
        report.max_coast_land_above_sea_delta
    )?;
    writeln!(out, "maxCoastFloorDelta={}", report.max_coast_floor_delta)?;
    for (block, count) in &report.top_terrain_block_hits {
        writeln!(out, "topTerrainBlockHits.{block}={count}")?;
    }
    for (index, sample) in report.coast_offender_samples.iter().enumerate() {
        writeln!(
            out,
            "coastOffender.{index}.landLocalBlockX={}",
            sample.land_local_block_x
        )?;
        writeln!(
            out,
            "coastOffender.{index}.landLocalBlockZ={}",
            sample.land_local_block_z
        )?;
        writeln!(
            out,
            "coastOffender.{index}.waterLocalBlockX={}",
            sample.water_local_block_x
        )?;
        writeln!(
            out,
            "coastOffender.{index}.waterLocalBlockZ={}",
            sample.water_local_block_z
        )?;
        writeln!(
            out,
            "coastOffender.{index}.landSurfaceY={}",
            sample.land_surface_y
        )?;
        writeln!(
            out,
            "coastOffender.{index}.waterFloorY={}",
            sample.water_floor_y
        )?;
        writeln!(
            out,
            "coastOffender.{index}.landSurfaceBlock={}",
            sample.land_surface_block
        )?;
        writeln!(
            out,
            "coastOffender.{index}.waterFloorBlock={}",
            sample.water_floor_block
        )?;
        writeln!(
            out,
            "coastOffender.{index}.landAboveSea={}",
            sample.land_above_sea
        )?;
        writeln!(
            out,
            "coastOffender.{index}.floorDelta={}",
            sample.floor_delta
        )?;
    }
    Ok(())
}

fn scan_post_final_integrity(
    region: &str,
    expected_format: RegionFormat,
) -> std::result::Result<PostFinalIntegrityReport, String> {
    let payloads = read_region_payloads(region).map_err(|error| error.to_string())?;
    if payloads.format != expected_format {
        return Err(format!(
            "region format mismatch: expected {} got {}",
            expected_format.as_manifest_value(),
            payloads.format.as_manifest_value()
        ));
    }
    let mut report = PostFinalIntegrityReport {
        region_chunk_count: payloads.chunks.len(),
        ..PostFinalIntegrityReport::default()
    };
    let mut columns = vec![None; POST_FINAL_REGION_SIZE_BLOCKS * POST_FINAL_REGION_SIZE_BLOCKS];
    for local_z in 0..32u8 {
        for local_x in 0..32u8 {
            let pos = ChunkLocalPos::new(local_x, local_z).map_err(|error| error.to_string())?;
            let Some(payload) = payloads.chunks.get(&pos) else {
                continue;
            };
            scan_post_final_chunk(&pos, payload, &mut report, &mut columns)?;
        }
    }
    sample_post_final_coast_edges(&mut report, &columns);
    Ok(report)
}

fn scan_post_final_chunk(
    pos: &ChunkLocalPos,
    payload: &[u8],
    report: &mut PostFinalIntegrityReport,
    columns: &mut [Option<PostFinalColumnSnapshot>],
) -> std::result::Result<(), String> {
    let chunk = TopdownChunk::decode(payload)?;
    report.decoded_chunk_count += 1;
    report.decoded_section_count += chunk.sections.len();
    for local_z in 0..CHUNK_WIDTH {
        for local_x in 0..CHUNK_WIDTH {
            let column = scan_post_final_column(pos, &chunk, local_x, local_z, report)?;
            let index = post_final_column_index(column.region_local_x, column.region_local_z);
            columns[index] = Some(column);
        }
    }
    Ok(())
}

fn scan_post_final_column(
    pos: &ChunkLocalPos,
    chunk: &TopdownChunk,
    local_x: usize,
    local_z: usize,
    report: &mut PostFinalIntegrityReport,
) -> std::result::Result<PostFinalColumnSnapshot, String> {
    let mut terrain_surface_y = i32::MIN;
    let mut terrain_surface_block = String::new();
    let mut water_at_sea_level = false;
    let mut has_underwater_air = false;
    let mut has_log = false;
    let mut has_leaves = false;

    for y in OVERWORLD_1_21_11.min_y()..=OVERWORLD_1_21_11.max_y_inclusive() {
        let block_name = chunk.block_name_at(local_x, y, local_z)?;
        if y == SEA_LEVEL_Y && is_water_like_block(&block_name) {
            water_at_sea_level = true;
        }
        if is_log_block(&block_name) {
            report.tree_log_blocks += 1;
            has_log = true;
        } else if is_leaf_block_name(&block_name) {
            report.tree_leaf_blocks += 1;
            has_leaves = true;
        }
        if is_post_final_terrain_surface_candidate(&block_name) {
            terrain_surface_y = y;
            terrain_surface_block = block_name;
        }
    }

    report.scanned_columns += 1;
    if has_log {
        report.tree_log_columns += 1;
    }
    if has_leaves {
        report.tree_leaf_columns += 1;
    }
    let open_water_column = water_at_sea_level && terrain_surface_y < SEA_LEVEL_Y;
    if terrain_surface_y >= SEA_LEVEL_Y {
        report.land_columns += 1;
    } else if open_water_column {
        report.water_columns += 1;
    } else if terrain_surface_y != i32::MIN {
        report.dry_below_sea_columns += 1;
    }

    if !terrain_surface_block.is_empty() {
        *report
            .top_terrain_block_hits
            .entry(terrain_surface_block.clone())
            .or_insert(0) += 1;
    }

    if open_water_column {
        let bottom = if terrain_surface_y == i32::MIN {
            OVERWORLD_1_21_11.min_y()
        } else {
            terrain_surface_y + 1
        };
        for y in bottom..=SEA_LEVEL_Y {
            if is_air_block(&chunk.block_name_at(local_x, y, local_z)?) {
                has_underwater_air = true;
                break;
            }
        }
    }
    if has_underwater_air {
        report.underwater_air_columns += 1;
    }

    Ok(PostFinalColumnSnapshot {
        region_local_x: usize::from(pos.x) * CHUNK_WIDTH + local_x,
        region_local_z: usize::from(pos.z) * CHUNK_WIDTH + local_z,
        terrain_surface_y,
        terrain_surface_block,
        open_water_column,
    })
}

fn is_post_final_terrain_surface_candidate(block_name: &str) -> bool {
    !is_air_block(block_name)
        && !is_water_like_block(block_name)
        && block_name != "minecraft:lava"
        && !is_log_block(block_name)
        && !is_leaf_block_name(block_name)
        && !is_plant_like_block(block_name)
}

fn sample_post_final_coast_edges(
    report: &mut PostFinalIntegrityReport,
    columns: &[Option<PostFinalColumnSnapshot>],
) {
    for z in 0..POST_FINAL_REGION_SIZE_BLOCKS {
        for x in 0..POST_FINAL_REGION_SIZE_BLOCKS {
            let Some(column) = columns[post_final_column_index(x, z)].as_ref() else {
                continue;
            };
            if x + 1 < POST_FINAL_REGION_SIZE_BLOCKS {
                if let Some(next) = columns[post_final_column_index(x + 1, z)].as_ref() {
                    add_post_final_coast_sample(report, column, next);
                }
            }
            if z + 1 < POST_FINAL_REGION_SIZE_BLOCKS {
                if let Some(next) = columns[post_final_column_index(x, z + 1)].as_ref() {
                    add_post_final_coast_sample(report, column, next);
                }
            }
        }
    }
}

fn add_post_final_coast_sample(
    report: &mut PostFinalIntegrityReport,
    a: &PostFinalColumnSnapshot,
    b: &PostFinalColumnSnapshot,
) {
    if a.land_at_coast() == b.land_at_coast() || a.open_water_column == b.open_water_column {
        return;
    }
    let land = if a.land_at_coast() { a } else { b };
    let water = if a.open_water_column { a } else { b };
    if !land.land_at_coast() || !water.open_water_column || !water.has_terrain() {
        return;
    }
    report.coast_edge_samples += 1;
    let land_above_sea = (land.terrain_surface_y - SEA_LEVEL_Y).max(0);
    let floor_delta = if land_above_sea > 4 {
        (land.terrain_surface_y - water.terrain_surface_y).max(0)
    } else {
        0
    };
    report.max_coast_land_above_sea_delta =
        report.max_coast_land_above_sea_delta.max(land_above_sea);
    report.max_coast_floor_delta = report.max_coast_floor_delta.max(floor_delta);
    if (land_above_sea > 16 || floor_delta > 32)
        && report.coast_offender_samples.len() < POST_FINAL_COAST_OFFENDER_LIMIT
    {
        report.coast_offender_samples.push(CoastOffenderSample {
            land_local_block_x: land.region_local_x,
            land_local_block_z: land.region_local_z,
            water_local_block_x: water.region_local_x,
            water_local_block_z: water.region_local_z,
            land_surface_y: land.terrain_surface_y,
            water_floor_y: water.terrain_surface_y,
            land_surface_block: land.terrain_surface_block.clone(),
            water_floor_block: water.terrain_surface_block.clone(),
            land_above_sea,
            floor_delta,
        });
    }
    if land_above_sea > 4 {
        report.coast_land_above_sea_gt4_samples += 1;
    }
    if land_above_sea > 8 {
        report.coast_land_above_sea_gt8_samples += 1;
    }
    if land_above_sea > 16 {
        report.coast_land_above_sea_gt16_samples += 1;
    }
}

fn post_final_column_index(x: usize, z: usize) -> usize {
    (z * POST_FINAL_REGION_SIZE_BLOCKS) + x
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct McaStatusRewriteReport {
    target_status: String,
    region_file_count: usize,
    chunk_count: usize,
    rewritten_region_count: usize,
    rewritten_chunk_count: usize,
}

fn rewrite_mca_status(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
    status_text: &str,
) -> io::Result<i32> {
    match rewrite_mca_status_impl(Path::new(path), status_text) {
        Ok(report) => {
            writeln!(out, "MCA chunk statuses rewritten")?;
            writeln!(out, "targetStatus={}", report.target_status)?;
            writeln!(out, "regionFileCount={}", report.region_file_count)?;
            writeln!(out, "chunkCount={}", report.chunk_count)?;
            writeln!(
                out,
                "rewrittenRegionCount={}",
                report.rewritten_region_count
            )?;
            writeln!(out, "rewrittenChunkCount={}", report.rewritten_chunk_count)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "MCA chunk status rewrite failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn rewrite_mca_status_impl(
    path: &Path,
    status_text: &str,
) -> std::result::Result<McaStatusRewriteReport, String> {
    let target_status =
        ChunkGenerationStatus::parse(status_text).map_err(|error| error.to_string())?;
    let region_files = mca_region_files_for_target(path)?;
    let mut report = McaStatusRewriteReport {
        target_status: target_status.id().to_string(),
        region_file_count: 0,
        chunk_count: 0,
        rewritten_region_count: 0,
        rewritten_chunk_count: 0,
    };
    for region_file in region_files {
        let region_report = rewrite_mca_status_region(&region_file, target_status)?;
        report.region_file_count += region_report.region_file_count;
        report.chunk_count += region_report.chunk_count;
        report.rewritten_region_count += region_report.rewritten_region_count;
        report.rewritten_chunk_count += region_report.rewritten_chunk_count;
    }
    Ok(report)
}

fn mca_region_files_for_target(
    path: &Path,
) -> std::result::Result<Vec<std::path::PathBuf>, String> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let region_dir = if path.join("region").is_dir() {
        path.join("region")
    } else {
        path.to_path_buf()
    };
    let mut regions = std::fs::read_dir(&region_dir)
        .map_err(|error| error.to_string())?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("mca"))
        })
        .collect::<Vec<_>>();
    regions.sort();
    if regions.is_empty() {
        return Err(format!(
            "no MCA region files found in {}",
            region_dir.display()
        ));
    }
    Ok(regions)
}

fn rewrite_mca_status_region(
    region_file: &Path,
    target_status: ChunkGenerationStatus,
) -> std::result::Result<McaStatusRewriteReport, String> {
    let payloads = read_region_payloads(region_file).map_err(|error| error.to_string())?;
    if payloads.format != RegionFormat::Mca {
        return Err(format!(
            "region format mismatch: expected mca got {}",
            payloads.format.as_manifest_value()
        ));
    }
    let mut rewritten_chunks = 0usize;
    let mut updated_payloads = BTreeMap::new();
    for (pos, payload) in &payloads.chunks {
        let (updated, rewritten) = rewrite_mca_status_chunk(payload, target_status)?;
        updated_payloads.insert(*pos, updated);
        if rewritten {
            rewritten_chunks += 1;
        }
    }
    if rewritten_chunks > 0 {
        earthmap_region::write_mca_region(region_file, &updated_payloads, current_mca_timestamp())
            .map_err(|error| error.to_string())?;
    }
    Ok(McaStatusRewriteReport {
        target_status: target_status.id().to_string(),
        region_file_count: 1,
        chunk_count: payloads.chunks.len(),
        rewritten_region_count: usize::from(rewritten_chunks > 0),
        rewritten_chunk_count: rewritten_chunks,
    })
}

fn rewrite_mca_status_chunk(
    payload: &[u8],
    target_status: ChunkGenerationStatus,
) -> std::result::Result<(Vec<u8>, bool), String> {
    let named = nbt::read_from_bytes(payload).map_err(|error| error.to_string())?;
    let root_name = named.name().to_string();
    let Tag::Compound(mut root) = named.into_tag() else {
        return Err("chunk root is not an NBT compound".to_string());
    };
    let previous_status = if root.contains("Status") {
        root.get_string("Status")
            .map_err(|error| error.to_string())?
            .to_string()
    } else {
        String::new()
    };
    let previous_light = if root.contains("isLightOn") {
        root.get_byte("isLightOn")
            .map_err(|error| error.to_string())?
    } else {
        0
    };
    let next_light = if target_status.light_on() { 1 } else { 0 };
    if previous_status == target_status.id() && previous_light == next_light {
        return Ok((payload.to_vec(), false));
    }
    root.put_string("Status", target_status.id())
        .map_err(|error| error.to_string())?;
    root.put_byte("isLightOn", i32::from(next_light))
        .map_err(|error| error.to_string())?;
    let updated = nbt::write_to_bytes(&root_name, &root).map_err(|error| error.to_string())?;
    Ok((updated, true))
}

fn current_mca_timestamp() -> i32 {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    i32::try_from(seconds.min(i32::MAX as u64)).unwrap_or(i32::MAX)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct McaPostFinalWaterRepairReport {
    region_file_count: usize,
    chunk_count: usize,
    repaired_region_count: usize,
    repaired_chunk_count: usize,
    repaired_column_count: u64,
    filled_block_count: u64,
}

fn repair_mca_post_final_water(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
) -> io::Result<i32> {
    match repair_mca_post_final_water_impl(Path::new(path)) {
        Ok(report) => {
            writeln!(out, "MCA post-final water repaired")?;
            writeln!(out, "regionFileCount={}", report.region_file_count)?;
            writeln!(out, "chunkCount={}", report.chunk_count)?;
            writeln!(out, "repairedRegionCount={}", report.repaired_region_count)?;
            writeln!(out, "repairedChunkCount={}", report.repaired_chunk_count)?;
            writeln!(out, "repairedColumnCount={}", report.repaired_column_count)?;
            writeln!(out, "filledBlockCount={}", report.filled_block_count)?;
            writeln!(out, "changed={}", report.filled_block_count > 0)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "MCA post-final water repair failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn repair_mca_post_final_water_impl(
    path: &Path,
) -> std::result::Result<McaPostFinalWaterRepairReport, String> {
    let region_files = mca_region_files_for_target(path)?;
    let mut report = McaPostFinalWaterRepairReport::default();
    for region_file in region_files {
        let region_report = repair_mca_post_final_water_region(&region_file)?;
        report.region_file_count += region_report.region_file_count;
        report.chunk_count += region_report.chunk_count;
        report.repaired_region_count += region_report.repaired_region_count;
        report.repaired_chunk_count += region_report.repaired_chunk_count;
        report.repaired_column_count += region_report.repaired_column_count;
        report.filled_block_count += region_report.filled_block_count;
    }
    Ok(report)
}

fn repair_mca_post_final_water_region(
    region_file: &Path,
) -> std::result::Result<McaPostFinalWaterRepairReport, String> {
    let payloads = read_region_payloads(region_file).map_err(|error| error.to_string())?;
    if payloads.format != RegionFormat::Mca {
        return Err(format!(
            "region format mismatch: expected mca got {}",
            payloads.format.as_manifest_value()
        ));
    }
    let mut repaired_chunks = 0usize;
    let mut repaired_columns = 0u64;
    let mut filled_blocks = 0u64;
    let mut updated_payloads = BTreeMap::new();
    for (pos, payload) in &payloads.chunks {
        let result = repair_mca_post_final_water_chunk(payload)?;
        if result.filled_block_count > 0 {
            repaired_chunks += 1;
            repaired_columns += result.repaired_column_count;
            filled_blocks += result.filled_block_count;
        }
        updated_payloads.insert(*pos, result.payload);
    }
    if filled_blocks > 0 {
        earthmap_region::write_mca_region(region_file, &updated_payloads, current_mca_timestamp())
            .map_err(|error| error.to_string())?;
    }
    Ok(McaPostFinalWaterRepairReport {
        region_file_count: 1,
        chunk_count: payloads.chunks.len(),
        repaired_region_count: usize::from(filled_blocks > 0),
        repaired_chunk_count: repaired_chunks,
        repaired_column_count: repaired_columns,
        filled_block_count: filled_blocks,
    })
}

#[derive(Clone, Debug)]
struct McaWaterChunkRepairResult {
    payload: Vec<u8>,
    repaired_column_count: u64,
    filled_block_count: u64,
}

fn repair_mca_post_final_water_chunk(
    payload: &[u8],
) -> std::result::Result<McaWaterChunkRepairResult, String> {
    let named = nbt::read_from_bytes(payload).map_err(|error| error.to_string())?;
    let root_name = named.name().to_string();
    let Tag::Compound(root) = named.into_tag() else {
        return Err("chunk root is not an NBT compound".to_string());
    };
    let mut chunk = MutableMcaWaterChunk::decode(root)?;
    let result = chunk.repair_underwater_air()?;
    if result.filled_block_count == 0 {
        return Ok(McaWaterChunkRepairResult {
            payload: payload.to_vec(),
            repaired_column_count: 0,
            filled_block_count: 0,
        });
    }
    let root = chunk.flush()?;
    let payload = nbt::write_to_bytes(&root_name, &root).map_err(|error| error.to_string())?;
    Ok(McaWaterChunkRepairResult {
        payload,
        repaired_column_count: result.repaired_column_count,
        filled_block_count: result.filled_block_count,
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct McaWaterColumnRepairResult {
    repaired_column_count: u64,
    filled_block_count: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct McaWaterColumnShape {
    terrain_surface_y: i32,
    water_at_sea_level: bool,
}

#[derive(Clone, Debug)]
struct MutableMcaWaterChunk {
    root: earthmap_minecraft::nbt::Compound,
    section_tags: Vec<Tag>,
    sections: BTreeMap<i32, MutableMcaWaterSection>,
    section_list_changed: bool,
}

impl MutableMcaWaterChunk {
    fn decode(root: earthmap_minecraft::nbt::Compound) -> std::result::Result<Self, String> {
        let section_tags = root
            .get_list("sections")
            .map_err(|error| error.to_string())?
            .values()
            .to_vec();
        let mut sections = BTreeMap::new();
        for (tag_index, section_tag) in section_tags.iter().enumerate() {
            let Tag::Compound(section) = section_tag else {
                return Err("chunk section is not an NBT compound".to_string());
            };
            if !section.contains("block_states") {
                continue;
            }
            let section = MutableMcaWaterSection::decode(section.clone(), tag_index)?;
            sections.insert(section.section_y, section);
        }
        Ok(Self {
            root,
            section_tags,
            sections,
            section_list_changed: false,
        })
    }

    fn repair_underwater_air(&mut self) -> std::result::Result<McaWaterColumnRepairResult, String> {
        let mut repaired_column_count = 0u64;
        let mut filled_block_count = 0u64;
        for local_z in 0..CHUNK_WIDTH {
            for local_x in 0..CHUNK_WIDTH {
                let shape = self.scan_column_shape(local_x, local_z)?;
                if !shape.water_at_sea_level || shape.terrain_surface_y >= SEA_LEVEL_Y {
                    continue;
                }
                let bottom = if shape.terrain_surface_y == i32::MIN {
                    OVERWORLD_1_21_11.min_y()
                } else {
                    shape.terrain_surface_y + 1
                };
                let mut column_fills = 0u64;
                for y in bottom..=SEA_LEVEL_Y {
                    if is_air_block(&self.block_name_at(local_x, y, local_z)?)
                        && self.set_full_water_at(local_x, y, local_z)?
                    {
                        column_fills += 1;
                    }
                }
                if column_fills > 0 {
                    repaired_column_count += 1;
                    filled_block_count += column_fills;
                }
            }
        }
        Ok(McaWaterColumnRepairResult {
            repaired_column_count,
            filled_block_count,
        })
    }

    fn scan_column_shape(
        &self,
        local_x: usize,
        local_z: usize,
    ) -> std::result::Result<McaWaterColumnShape, String> {
        let mut terrain_surface_y = i32::MIN;
        let mut water_at_sea_level = false;
        for y in OVERWORLD_1_21_11.min_y()..=OVERWORLD_1_21_11.max_y_inclusive() {
            let block_name = self.block_name_at(local_x, y, local_z)?;
            if y == SEA_LEVEL_Y && is_water_like_block(&block_name) {
                water_at_sea_level = true;
            }
            if is_mca_water_repair_terrain_surface_candidate(&block_name) {
                terrain_surface_y = y;
            }
        }
        Ok(McaWaterColumnShape {
            terrain_surface_y,
            water_at_sea_level,
        })
    }

    fn block_name_at(
        &self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<String, String> {
        let section_y = y.div_euclid(SECTION_HEIGHT);
        let Some(section) = self.sections.get(&section_y) else {
            return Ok(TOPDOWN_AIR_BLOCK.to_string());
        };
        section.block_name_at(local_x, y, local_z)
    }

    fn set_full_water_at(
        &mut self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<bool, String> {
        let section = self.section_for_y(y)?;
        section.set_full_water_at(local_x, y, local_z)
    }

    fn section_for_y(
        &mut self,
        y: i32,
    ) -> std::result::Result<&mut MutableMcaWaterSection, String> {
        let section_y = y.div_euclid(SECTION_HEIGHT);
        if !self.sections.contains_key(&section_y) {
            let tag_index = self.section_tags.len();
            let section_tag = empty_mca_water_air_section(section_y)?;
            let section = MutableMcaWaterSection::decode(section_tag.clone(), tag_index)?;
            self.section_tags.push(Tag::Compound(section_tag));
            self.sections.insert(section_y, section);
            self.section_list_changed = true;
        }
        self.sections
            .get_mut(&section_y)
            .ok_or_else(|| format!("missing section after creation: {section_y}"))
    }

    fn flush(mut self) -> std::result::Result<earthmap_minecraft::nbt::Compound, String> {
        let mut changed = self.section_list_changed;
        for section in self.sections.values_mut() {
            if section.flush()? {
                self.section_tags[section.tag_index] = Tag::Compound(section.section_tag.clone());
                changed = true;
            }
        }
        if changed {
            if self.section_list_changed {
                self.section_tags.sort_by_key(mca_water_section_sort_key);
            }
            self.root
                .put(
                    "sections",
                    Tag::List(
                        nbt::list(nbt::TAG_COMPOUND, self.section_tags)
                            .map_err(|error| error.to_string())?,
                    ),
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(self.root)
    }
}

#[derive(Clone, Debug)]
struct MutableMcaWaterSection {
    section_y: i32,
    tag_index: usize,
    section_tag: earthmap_minecraft::nbt::Compound,
    palette_tags: Vec<Tag>,
    palette_names: Vec<String>,
    palette_indices: Vec<usize>,
    changed: bool,
}

impl MutableMcaWaterSection {
    fn decode(
        section_tag: earthmap_minecraft::nbt::Compound,
        tag_index: usize,
    ) -> std::result::Result<Self, String> {
        let section_y = i32::from(
            section_tag
                .get_byte("Y")
                .map_err(|error| error.to_string())?,
        );
        let block_states = section_tag
            .get_compound("block_states")
            .map_err(|error| error.to_string())?;
        if !block_states.contains("palette") {
            return Err("chunk section block_states missing palette".to_string());
        }
        let palette_tags = block_states
            .get_list("palette")
            .map_err(|error| error.to_string())?
            .values()
            .to_vec();
        if palette_tags.is_empty() {
            return Err("chunk section block state palette is empty".to_string());
        }
        let mut palette_names = Vec::with_capacity(palette_tags.len());
        for palette_tag in &palette_tags {
            let Tag::Compound(block_state) = palette_tag else {
                return Err("block state palette entry is not an NBT compound".to_string());
            };
            palette_names.push(
                block_state
                    .get_string("Name")
                    .map_err(|error| error.to_string())?
                    .to_string(),
            );
        }
        let palette_indices =
            decode_palette_values(block_states, palette_tags.len(), SECTION_BLOCK_COUNT)?;
        Ok(Self {
            section_y,
            tag_index,
            section_tag,
            palette_tags,
            palette_names,
            palette_indices,
            changed: false,
        })
    }

    fn block_name_at(
        &self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<String, String> {
        let block_index = mca_water_block_index(local_x, y, local_z);
        let palette_index = self
            .palette_indices
            .get(block_index)
            .copied()
            .ok_or_else(|| format!("missing palette index at {block_index}"))?;
        self.palette_names
            .get(palette_index)
            .cloned()
            .ok_or_else(|| format!("packed palette index outside palette: {palette_index}"))
    }

    fn set_full_water_at(
        &mut self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<bool, String> {
        if !is_air_block(&self.block_name_at(local_x, y, local_z)?) {
            return Ok(false);
        }
        let block_index = mca_water_block_index(local_x, y, local_z);
        let water_index = self.full_water_palette_index()?;
        self.palette_indices[block_index] = water_index;
        self.changed = true;
        Ok(true)
    }

    fn flush(&mut self) -> std::result::Result<bool, String> {
        if !self.changed {
            return Ok(false);
        }
        let mut block_states = self
            .section_tag
            .get_compound("block_states")
            .map_err(|error| error.to_string())?
            .clone();
        block_states
            .put(
                "palette",
                Tag::List(
                    nbt::list(nbt::TAG_COMPOUND, self.palette_tags.clone())
                        .map_err(|error| error.to_string())?,
                ),
            )
            .map_err(|error| error.to_string())?;
        let bits_per_entry = bits_per_entry_for_palette_size(self.palette_tags.len())
            .map_err(|error| error.to_string())?;
        if bits_per_entry == 0 {
            block_states
                .remove("data")
                .map_err(|error| error.to_string())?;
        } else {
            let packed = PackedLongArray::pack(SECTION_BLOCK_COUNT, bits_per_entry, |index| {
                self.palette_indices[index] as i32
            })
            .map_err(|error| error.to_string())?;
            block_states
                .put_long_array("data", packed.copy_data())
                .map_err(|error| error.to_string())?;
        }
        self.section_tag
            .put_compound("block_states", block_states)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    fn full_water_palette_index(&mut self) -> std::result::Result<usize, String> {
        for (index, tag) in self.palette_tags.iter().enumerate() {
            let Tag::Compound(block_state) = tag else {
                continue;
            };
            if is_full_water_state(block_state) {
                return Ok(index);
            }
        }
        let index = self.palette_tags.len();
        self.palette_tags.push(full_water_block_state_tag()?);
        self.palette_names.push("minecraft:water".to_string());
        Ok(index)
    }
}

fn is_mca_water_repair_terrain_surface_candidate(block_name: &str) -> bool {
    !is_air_block(block_name)
        && !is_water_like_block(block_name)
        && !is_log_block(block_name)
        && !is_leaf_block_name(block_name)
        && !is_plant_like_block(block_name)
}

fn mca_water_block_index(local_x: usize, y: i32, local_z: usize) -> usize {
    let local_y = usize::try_from(y & (SECTION_HEIGHT - 1)).expect("local y is non-negative");
    (local_y << 8) | (local_z << 4) | local_x
}

fn full_water_block_state_tag() -> std::result::Result<Tag, String> {
    let mut properties = nbt::compound();
    properties
        .put_string("level", "0")
        .map_err(|error| error.to_string())?;
    let mut block = nbt::compound();
    block
        .put_string("Name", "minecraft:water")
        .map_err(|error| error.to_string())?
        .put_compound("Properties", properties)
        .map_err(|error| error.to_string())?;
    Ok(Tag::Compound(block))
}

fn air_block_state_tag() -> std::result::Result<Tag, String> {
    let mut block = nbt::compound();
    block
        .put_string("Name", TOPDOWN_AIR_BLOCK)
        .map_err(|error| error.to_string())?;
    Ok(Tag::Compound(block))
}

fn empty_mca_water_air_section(
    section_y: i32,
) -> std::result::Result<earthmap_minecraft::nbt::Compound, String> {
    let mut block_states = nbt::compound();
    block_states
        .put(
            "palette",
            Tag::List(
                nbt::list(nbt::TAG_COMPOUND, vec![air_block_state_tag()?])
                    .map_err(|error| error.to_string())?,
            ),
        )
        .map_err(|error| error.to_string())?;
    let mut biomes = nbt::compound();
    biomes
        .put(
            "palette",
            Tag::List(
                nbt::list(
                    nbt::TAG_STRING,
                    vec![Tag::String("minecraft:ocean".to_string())],
                )
                .map_err(|error| error.to_string())?,
            ),
        )
        .map_err(|error| error.to_string())?;
    let mut section = nbt::compound();
    section
        .put_byte("Y", section_y)
        .map_err(|error| error.to_string())?
        .put_compound("block_states", block_states)
        .map_err(|error| error.to_string())?
        .put_compound("biomes", biomes)
        .map_err(|error| error.to_string())?;
    Ok(section)
}

fn is_full_water_state(block_state: &earthmap_minecraft::nbt::Compound) -> bool {
    if block_state
        .get_string("Name")
        .map(|name| name != "minecraft:water")
        .unwrap_or(true)
    {
        return false;
    }
    let Ok(properties) = block_state.get_compound("Properties") else {
        return true;
    };
    !properties.contains("level")
        || properties
            .get_string("level")
            .map(|level| level == "0")
            .unwrap_or(false)
}

fn mca_water_section_sort_key(tag: &Tag) -> i32 {
    let Tag::Compound(section) = tag else {
        return i32::MAX;
    };
    section.get_byte("Y").map(i32::from).unwrap_or(i32::MAX)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LinearSandlikeSurfaceRepairReport {
    region_file_count: usize,
    chunk_count: usize,
    repaired_region_count: usize,
    repaired_chunk_count: usize,
    replaced_block_count: u64,
}

fn repair_linear_sandlike_surfaces(
    out: &mut impl Write,
    err: &mut impl Write,
    path: &str,
) -> io::Result<i32> {
    match repair_linear_sandlike_surfaces_impl(Path::new(path)) {
        Ok(report) => {
            writeln!(out, "Linear sandlike surfaces repaired")?;
            writeln!(out, "regionFileCount={}", report.region_file_count)?;
            writeln!(out, "chunkCount={}", report.chunk_count)?;
            writeln!(out, "repairedRegionCount={}", report.repaired_region_count)?;
            writeln!(out, "repairedChunkCount={}", report.repaired_chunk_count)?;
            writeln!(out, "replacedBlockCount={}", report.replaced_block_count)?;
            writeln!(out, "changed={}", report.replaced_block_count > 0)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Linear sandlike surface repair failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn repair_linear_sandlike_surfaces_impl(
    path: &Path,
) -> std::result::Result<LinearSandlikeSurfaceRepairReport, String> {
    let region_files = linear_region_files_for_target(path)?;
    let mut report = LinearSandlikeSurfaceRepairReport::default();
    for region_file in region_files {
        let region_report = repair_linear_sandlike_region(&region_file)?;
        report.region_file_count += region_report.region_file_count;
        report.chunk_count += region_report.chunk_count;
        report.repaired_region_count += region_report.repaired_region_count;
        report.repaired_chunk_count += region_report.repaired_chunk_count;
        report.replaced_block_count += region_report.replaced_block_count;
    }
    Ok(report)
}

fn linear_region_files_for_target(
    path: &Path,
) -> std::result::Result<Vec<std::path::PathBuf>, String> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let region_dir = if path.join("region").is_dir() {
        path.join("region")
    } else {
        path.to_path_buf()
    };
    let mut regions = std::fs::read_dir(&region_dir)
        .map_err(|error| error.to_string())?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("linear"))
        })
        .collect::<Vec<_>>();
    regions.sort();
    if regions.is_empty() {
        return Err(format!(
            "no Linear region files found in {}",
            region_dir.display()
        ));
    }
    Ok(regions)
}

fn repair_linear_sandlike_region(
    region_file: &Path,
) -> std::result::Result<LinearSandlikeSurfaceRepairReport, String> {
    let payloads = read_region_payloads(region_file).map_err(|error| error.to_string())?;
    if payloads.format != RegionFormat::Linear {
        return Err(format!(
            "region format mismatch: expected linear got {}",
            payloads.format.as_manifest_value()
        ));
    }
    let mut repaired_chunks = 0usize;
    let mut replaced_blocks = 0u64;
    let mut updated_payloads = BTreeMap::new();
    for (pos, payload) in &payloads.chunks {
        let result = repair_linear_sandlike_chunk(payload)?;
        if result.replaced_block_count > 0 {
            repaired_chunks += 1;
            replaced_blocks += result.replaced_block_count;
        }
        updated_payloads.insert(*pos, result.payload);
    }
    if replaced_blocks > 0 {
        earthmap_region::write_linear_v2_region(
            region_file,
            &updated_payloads,
            i64::from(current_mca_timestamp()),
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(LinearSandlikeSurfaceRepairReport {
        region_file_count: 1,
        chunk_count: payloads.chunks.len(),
        repaired_region_count: usize::from(replaced_blocks > 0),
        repaired_chunk_count: repaired_chunks,
        replaced_block_count: replaced_blocks,
    })
}

#[derive(Clone, Debug)]
struct LinearSandlikeChunkRepairResult {
    payload: Vec<u8>,
    replaced_block_count: u64,
}

fn repair_linear_sandlike_chunk(
    payload: &[u8],
) -> std::result::Result<LinearSandlikeChunkRepairResult, String> {
    let named = nbt::read_from_bytes(payload).map_err(|error| error.to_string())?;
    let root_name = named.name().to_string();
    let Tag::Compound(root) = named.into_tag() else {
        return Err("chunk root is not an NBT compound".to_string());
    };
    let mut chunk = MutableLinearSandlikeChunk::decode(root)?;
    let replaced_block_count = chunk.replace_sandlike_surfaces()?;
    if replaced_block_count == 0 {
        return Ok(LinearSandlikeChunkRepairResult {
            payload: payload.to_vec(),
            replaced_block_count: 0,
        });
    }
    let root = chunk.flush()?;
    let payload = nbt::write_to_bytes(&root_name, &root).map_err(|error| error.to_string())?;
    Ok(LinearSandlikeChunkRepairResult {
        payload,
        replaced_block_count,
    })
}

#[derive(Clone, Debug)]
struct MutableLinearSandlikeChunk {
    root: earthmap_minecraft::nbt::Compound,
    section_tags: Vec<Tag>,
    sections: BTreeMap<i32, MutableLinearSandlikeSection>,
}

impl MutableLinearSandlikeChunk {
    fn decode(root: earthmap_minecraft::nbt::Compound) -> std::result::Result<Self, String> {
        let section_tags = root
            .get_list("sections")
            .map_err(|error| error.to_string())?
            .values()
            .to_vec();
        let mut sections = BTreeMap::new();
        for (tag_index, section_tag) in section_tags.iter().enumerate() {
            let Tag::Compound(section) = section_tag else {
                return Err("chunk section is not an NBT compound".to_string());
            };
            if !section.contains("block_states") {
                continue;
            }
            let section = MutableLinearSandlikeSection::decode(section.clone(), tag_index)?;
            sections.insert(section.section_y, section);
        }
        Ok(Self {
            root,
            section_tags,
            sections,
        })
    }

    fn replace_sandlike_surfaces(&mut self) -> std::result::Result<u64, String> {
        let mut replaced = 0u64;
        for y in OVERWORLD_1_21_11.min_y()..=OVERWORLD_1_21_11.max_y_inclusive() {
            let section_y = y.div_euclid(SECTION_HEIGHT);
            let should_scan = self
                .sections
                .get(&section_y)
                .is_some_and(MutableLinearSandlikeSection::contains_sandlike);
            if !should_scan {
                continue;
            }
            for local_z in 0..CHUNK_WIDTH {
                for local_x in 0..CHUNK_WIDTH {
                    let block_name = self.block_name_at(local_x, y, local_z)?;
                    if is_linear_sandlike_block(&block_name) {
                        let replacement = self.replacement_for(local_x, y, local_z)?;
                        self.sections
                            .get_mut(&section_y)
                            .ok_or_else(|| {
                                format!("missing section during replacement: {section_y}")
                            })?
                            .set_block_name_at(local_x, y, local_z, &replacement)?;
                        replaced += 1;
                    }
                }
            }
        }
        Ok(replaced)
    }

    fn replacement_for(
        &self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<String, String> {
        if y <= SEA_LEVEL_Y
            || (self.column_has_water_at_sea(local_x, local_z)?
                && is_water_like_block(&self.block_name_at(
                    local_x,
                    SEA_LEVEL_Y.min(y + 1),
                    local_z,
                )?))
        {
            return Ok("minecraft:clay".to_string());
        }
        let above = if y >= OVERWORLD_1_21_11.max_y_inclusive() {
            TOPDOWN_AIR_BLOCK.to_string()
        } else {
            self.block_name_at(local_x, y + 1, local_z)?
        };
        if is_air_block(&above)
            || is_water_like_block(&above)
            || is_linear_sandlike_plant_like_block(&above)
        {
            Ok("minecraft:grass_block".to_string())
        } else {
            Ok("minecraft:dirt".to_string())
        }
    }

    fn column_has_water_at_sea(
        &self,
        local_x: usize,
        local_z: usize,
    ) -> std::result::Result<bool, String> {
        Ok(is_water_like_block(&self.block_name_at(
            local_x,
            SEA_LEVEL_Y,
            local_z,
        )?))
    }

    fn block_name_at(
        &self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<String, String> {
        let section_y = y.div_euclid(SECTION_HEIGHT);
        let Some(section) = self.sections.get(&section_y) else {
            return Ok(TOPDOWN_AIR_BLOCK.to_string());
        };
        section.block_name_at(local_x, y, local_z)
    }

    fn flush(mut self) -> std::result::Result<earthmap_minecraft::nbt::Compound, String> {
        let mut changed = false;
        for section in self.sections.values_mut() {
            if section.flush()? {
                self.section_tags[section.tag_index] = Tag::Compound(section.section_tag.clone());
                changed = true;
            }
        }
        if changed {
            self.root
                .put(
                    "sections",
                    Tag::List(
                        nbt::list(nbt::TAG_COMPOUND, self.section_tags)
                            .map_err(|error| error.to_string())?,
                    ),
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(self.root)
    }
}

#[derive(Clone, Debug)]
struct MutableLinearSandlikeSection {
    section_y: i32,
    tag_index: usize,
    section_tag: earthmap_minecraft::nbt::Compound,
    palette_tags: Vec<Tag>,
    palette_names: Vec<String>,
    palette_indices: Vec<usize>,
    changed: bool,
    has_sandlike: bool,
}

impl MutableLinearSandlikeSection {
    fn decode(
        section_tag: earthmap_minecraft::nbt::Compound,
        tag_index: usize,
    ) -> std::result::Result<Self, String> {
        let section_y = i32::from(
            section_tag
                .get_byte("Y")
                .map_err(|error| error.to_string())?,
        );
        let block_states = section_tag
            .get_compound("block_states")
            .map_err(|error| error.to_string())?;
        if !block_states.contains("palette") {
            return Err("chunk section block_states missing palette".to_string());
        }
        let palette_tags = block_states
            .get_list("palette")
            .map_err(|error| error.to_string())?
            .values()
            .to_vec();
        if palette_tags.is_empty() {
            return Err("chunk section block state palette is empty".to_string());
        }
        let mut palette_names = Vec::with_capacity(palette_tags.len());
        let mut has_sandlike = false;
        for palette_tag in &palette_tags {
            let Tag::Compound(block_state) = palette_tag else {
                return Err("block state palette entry is not an NBT compound".to_string());
            };
            let name = block_state
                .get_string("Name")
                .map_err(|error| error.to_string())?
                .to_string();
            has_sandlike |= is_linear_sandlike_block(&name);
            palette_names.push(name);
        }
        let palette_indices =
            decode_palette_values(block_states, palette_tags.len(), SECTION_BLOCK_COUNT)?;
        Ok(Self {
            section_y,
            tag_index,
            section_tag,
            palette_tags,
            palette_names,
            palette_indices,
            changed: false,
            has_sandlike,
        })
    }

    fn contains_sandlike(&self) -> bool {
        self.has_sandlike
    }

    fn block_name_at(
        &self,
        local_x: usize,
        y: i32,
        local_z: usize,
    ) -> std::result::Result<String, String> {
        let block_index = mca_water_block_index(local_x, y, local_z);
        let palette_index = self
            .palette_indices
            .get(block_index)
            .copied()
            .ok_or_else(|| format!("missing palette index at {block_index}"))?;
        self.palette_names
            .get(palette_index)
            .cloned()
            .ok_or_else(|| format!("packed palette index outside palette: {palette_index}"))
    }

    fn set_block_name_at(
        &mut self,
        local_x: usize,
        y: i32,
        local_z: usize,
        block_name: &str,
    ) -> std::result::Result<(), String> {
        let block_index = mca_water_block_index(local_x, y, local_z);
        let palette_index = self.palette_index(block_name)?;
        self.palette_indices[block_index] = palette_index;
        self.changed = true;
        Ok(())
    }

    fn flush(&mut self) -> std::result::Result<bool, String> {
        if !self.changed {
            return Ok(false);
        }
        self.compact_palette();
        let mut block_states = self
            .section_tag
            .get_compound("block_states")
            .map_err(|error| error.to_string())?
            .clone();
        block_states
            .put(
                "palette",
                Tag::List(
                    nbt::list(nbt::TAG_COMPOUND, self.palette_tags.clone())
                        .map_err(|error| error.to_string())?,
                ),
            )
            .map_err(|error| error.to_string())?;
        let bits_per_entry = bits_per_entry_for_palette_size(self.palette_tags.len())
            .map_err(|error| error.to_string())?;
        if bits_per_entry == 0 {
            block_states
                .remove("data")
                .map_err(|error| error.to_string())?;
        } else {
            let packed = PackedLongArray::pack(SECTION_BLOCK_COUNT, bits_per_entry, |index| {
                self.palette_indices[index] as i32
            })
            .map_err(|error| error.to_string())?;
            block_states
                .put_long_array("data", packed.copy_data())
                .map_err(|error| error.to_string())?;
        }
        self.section_tag
            .put_compound("block_states", block_states)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    fn compact_palette(&mut self) {
        let mut remap = Vec::<(usize, usize)>::new();
        let mut compact_tags = Vec::<Tag>::new();
        let mut compact_names = Vec::<String>::new();
        for palette_index in &mut self.palette_indices {
            if let Some((_, new_index)) = remap
                .iter()
                .find(|(old_index, _)| *old_index == *palette_index)
            {
                *palette_index = *new_index;
                continue;
            }
            let new_index = compact_tags.len();
            remap.push((*palette_index, new_index));
            compact_tags.push(self.palette_tags[*palette_index].clone());
            compact_names.push(self.palette_names[*palette_index].clone());
            *palette_index = new_index;
        }
        self.palette_tags = compact_tags;
        self.palette_names = compact_names;
        self.has_sandlike = self
            .palette_names
            .iter()
            .any(|name| is_linear_sandlike_block(name));
    }

    fn palette_index(&mut self, block_name: &str) -> std::result::Result<usize, String> {
        if let Some(index) = self
            .palette_names
            .iter()
            .position(|existing| existing == block_name)
        {
            return Ok(index);
        }
        let index = self.palette_names.len();
        self.palette_names.push(block_name.to_string());
        self.palette_tags.push(simple_block_state_tag(block_name)?);
        Ok(index)
    }
}

fn is_linear_sandlike_block(block_name: &str) -> bool {
    matches!(
        block_name,
        "minecraft:sand"
            | "minecraft:red_sand"
            | "minecraft:sandstone"
            | "minecraft:smooth_sandstone"
            | "minecraft:cut_sandstone"
            | "minecraft:chiseled_sandstone"
            | "minecraft:smooth_red_sandstone"
            | "minecraft:cut_red_sandstone"
            | "minecraft:chiseled_red_sandstone"
    )
}

fn is_linear_sandlike_plant_like_block(block_name: &str) -> bool {
    block_name.ends_with("_grass")
        || block_name.ends_with("_fern")
        || block_name.ends_with("_flower")
        || block_name.ends_with("_sapling")
        || block_name.ends_with("_bush")
        || matches!(block_name, "minecraft:fern" | "minecraft:bush")
        || block_name.contains("roots")
        || block_name.contains("vines")
        || block_name.contains("vine")
        || block_name.contains("mushroom")
}

fn simple_block_state_tag(block_name: &str) -> std::result::Result<Tag, String> {
    let mut block = nbt::compound();
    block
        .put_string("Name", block_name)
        .map_err(|error| error.to_string())?;
    Ok(Tag::Compound(block))
}

fn chunk_root(payload: &[u8]) -> std::result::Result<earthmap_minecraft::nbt::Compound, String> {
    let named = nbt::read_from_bytes(payload).map_err(|error| error.to_string())?;
    let Tag::Compound(root) = named.into_tag() else {
        return Err("chunk root is not an NBT compound".to_string());
    };
    Ok(root)
}

fn summarize_region_chunk(
    out: &mut impl Write,
    err: &mut impl Write,
    region: &str,
    local_chunk_x: &str,
    local_chunk_z: &str,
) -> io::Result<i32> {
    match summarize_region_chunk_impl(region, local_chunk_x, local_chunk_z) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Region chunk summary failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn summarize_region_chunk_impl(
    region: &str,
    local_chunk_x: &str,
    local_chunk_z: &str,
) -> std::result::Result<Vec<String>, String> {
    let local_x = parse_local_chunk_coord(local_chunk_x, "localChunkX")?;
    let local_z = parse_local_chunk_coord(local_chunk_z, "localChunkZ")?;
    let payloads = read_region_payloads(region).map_err(|error| error.to_string())?;
    let pos = ChunkLocalPos::new(local_x, local_z).map_err(|error| error.to_string())?;
    let payload = payloads
        .chunks
        .get(&pos)
        .ok_or_else(|| format!("missing chunk payload at {local_x},{local_z}"))?;
    let named = nbt::read_from_bytes(payload).map_err(|error| error.to_string())?;
    let Tag::Compound(root) = named.tag() else {
        return Err("chunk NBT root must be a compound".to_string());
    };

    let mut lines = vec![
        "regionChunkSummary=valid".to_string(),
        format!("format={}", payloads.format.as_manifest_value()),
        format!("regionX={}", payloads.region_x),
        format!("regionZ={}", payloads.region_z),
        format!("localChunkX={local_x}"),
        format!("localChunkZ={local_z}"),
        format!("payloadBytes={}", payload.len()),
        format!(
            "xPos={}",
            root.get_int("xPos").map_err(|error| error.to_string())?
        ),
        format!(
            "yPos={}",
            root.get_int("yPos").map_err(|error| error.to_string())?
        ),
        format!(
            "zPos={}",
            root.get_int("zPos").map_err(|error| error.to_string())?
        ),
        format!(
            "status={}",
            root.get_string("Status")
                .map_err(|error| error.to_string())?
        ),
        format!(
            "isLightOn={}",
            root.get_byte("isLightOn")
                .map_err(|error| error.to_string())?
        ),
    ];

    if let Ok(heightmaps) = root.get_compound("Heightmaps") {
        for (name, tag) in heightmaps.entries() {
            if let Tag::LongArray(values) = tag {
                lines.push(format!("heightmap.{name}.longs={}", values.len()));
                lines.push(format!(
                    "heightmap.{name}.first={}",
                    values.first().copied().unwrap_or_default()
                ));
                lines.push(format!(
                    "heightmap.{name}.last={}",
                    values.last().copied().unwrap_or_default()
                ));
            }
        }
    }

    let sections = root
        .get_list("sections")
        .map_err(|error| error.to_string())?;
    lines.push(format!("sectionCount={}", sections.values().len()));
    for section_tag in sections.values() {
        let Tag::Compound(section) = section_tag else {
            return Err("section list must contain compounds".to_string());
        };
        let y = section.get_byte("Y").map_err(|error| error.to_string())?;
        let block_states = section
            .get_compound("block_states")
            .map_err(|error| error.to_string())?;
        let block_palette = block_state_palette_names(block_states)?;
        lines.push(format!(
            "section.{y}.blockPaletteSize={}",
            block_palette.len()
        ));
        lines.push(format!(
            "section.{y}.blockPalette={}",
            block_palette.join("|")
        ));
        lines.push(format!(
            "section.{y}.blockDataLongs={}",
            long_array_len(block_states, "data")?
        ));
        let biomes = section
            .get_compound("biomes")
            .map_err(|error| error.to_string())?;
        let biome_palette = biome_palette_names(biomes)?;
        lines.push(format!(
            "section.{y}.biomePaletteSize={}",
            biome_palette.len()
        ));
        lines.push(format!(
            "section.{y}.biomePalette={}",
            biome_palette.join("|")
        ));
        lines.push(format!(
            "section.{y}.biomeDataLongs={}",
            long_array_len(biomes, "data")?
        ));
    }

    Ok(lines)
}

fn compare_region_chunk_details(
    out: &mut impl Write,
    err: &mut impl Write,
    expected_region: &str,
    actual_region: &str,
    expected_local_chunk_x: &str,
    expected_local_chunk_z: &str,
    actual_local_chunk_x: Option<&str>,
    actual_local_chunk_z: Option<&str>,
) -> io::Result<i32> {
    match compare_region_chunk_details_impl(
        expected_region,
        actual_region,
        expected_local_chunk_x,
        expected_local_chunk_z,
        actual_local_chunk_x,
        actual_local_chunk_z,
    ) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Region chunk detail comparison failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn compare_region_chunk_details_impl(
    expected_region: &str,
    actual_region: &str,
    expected_local_chunk_x: &str,
    expected_local_chunk_z: &str,
    actual_local_chunk_x: Option<&str>,
    actual_local_chunk_z: Option<&str>,
) -> std::result::Result<Vec<String>, String> {
    let expected_local_x = parse_local_chunk_coord(expected_local_chunk_x, "expectedLocalChunkX")?;
    let expected_local_z = parse_local_chunk_coord(expected_local_chunk_z, "expectedLocalChunkZ")?;
    let actual_local_x = parse_local_chunk_coord(
        actual_local_chunk_x.unwrap_or(expected_local_chunk_x),
        "actualLocalChunkX",
    )?;
    let actual_local_z = parse_local_chunk_coord(
        actual_local_chunk_z.unwrap_or(expected_local_chunk_z),
        "actualLocalChunkZ",
    )?;
    let expected =
        decode_region_chunk_details(expected_region, expected_local_x, expected_local_z)?;
    let actual = decode_region_chunk_details(actual_region, actual_local_x, actual_local_z)?;
    let mut lines = vec![
        "regionChunkDetails=valid".to_string(),
        format!("expectedLocalChunkX={expected_local_x}"),
        format!("expectedLocalChunkZ={expected_local_z}"),
        format!("actualLocalChunkX={actual_local_x}"),
        format!("actualLocalChunkZ={actual_local_z}"),
    ];
    lines.extend(compare_heightmap_values(
        &expected,
        &actual,
        "OCEAN_FLOOR",
        "heightmap.OCEAN_FLOOR",
        24,
    ));
    lines.extend(compare_biome_cells(&expected, &actual, 48));
    lines.extend(compare_block_cells(&expected, &actual, 48));
    Ok(lines)
}

#[derive(Clone, Debug)]
struct DecodedChunkDetails {
    heightmaps: BTreeMap<String, Vec<i32>>,
    sections: BTreeMap<i8, DecodedSectionDetails>,
}

#[derive(Clone, Debug)]
struct DecodedSectionDetails {
    block_palette: Vec<String>,
    block_values: Vec<usize>,
    biome_palette: Vec<String>,
    biome_values: Vec<usize>,
}

fn decode_region_chunk_details(
    region: &str,
    local_x: u8,
    local_z: u8,
) -> std::result::Result<DecodedChunkDetails, String> {
    let payloads = read_region_payloads(region).map_err(|error| error.to_string())?;
    let pos = ChunkLocalPos::new(local_x, local_z).map_err(|error| error.to_string())?;
    let payload = payloads
        .chunks
        .get(&pos)
        .ok_or_else(|| format!("missing chunk payload at {local_x},{local_z}"))?;
    let named = nbt::read_from_bytes(payload).map_err(|error| error.to_string())?;
    let Tag::Compound(root) = named.tag() else {
        return Err("chunk NBT root must be a compound".to_string());
    };

    let mut heightmaps = BTreeMap::new();
    if let Ok(heightmap_tags) = root.get_compound("Heightmaps") {
        let min_y = ChunkModel::overworld(0, 0).dimension().min_y();
        for (name, tag) in heightmap_tags.entries() {
            if let Tag::LongArray(values) = tag {
                let storage_values = decode_packed_indices_from_words(256, 9, values, "heightmap")?;
                heightmaps.insert(
                    name.clone(),
                    storage_values
                        .into_iter()
                        .map(|value| min_y + value as i32)
                        .collect(),
                );
            }
        }
    }

    let sections_tag = root
        .get_list("sections")
        .map_err(|error| error.to_string())?;
    let mut sections = BTreeMap::new();
    for section_tag in sections_tag.values() {
        let Tag::Compound(section) = section_tag else {
            return Err("section list must contain compounds".to_string());
        };
        let y = section.get_byte("Y").map_err(|error| error.to_string())?;
        let block_states = section
            .get_compound("block_states")
            .map_err(|error| error.to_string())?;
        let block_palette = block_state_palette_names(block_states)?;
        let block_values =
            decode_palette_values(block_states, block_palette.len(), SECTION_BLOCK_COUNT)?;
        let biomes = section
            .get_compound("biomes")
            .map_err(|error| error.to_string())?;
        let biome_palette = biome_palette_names(biomes)?;
        let biome_values =
            decode_biome_palette_values(biomes, biome_palette.len(), SECTION_BIOME_CELL_COUNT)?;
        sections.insert(
            y,
            DecodedSectionDetails {
                block_palette,
                block_values,
                biome_palette,
                biome_values,
            },
        );
    }

    Ok(DecodedChunkDetails {
        heightmaps,
        sections,
    })
}

fn decode_palette_values(
    compound: &earthmap_minecraft::nbt::Compound,
    palette_size: usize,
    value_count: usize,
) -> std::result::Result<Vec<usize>, String> {
    let bits_per_entry =
        bits_per_entry_for_palette_size(palette_size).map_err(|error| error.to_string())?;
    if bits_per_entry == 0 {
        return Ok(vec![0; value_count]);
    }
    let data = compound
        .get_long_array("data")
        .map_err(|error| error.to_string())?;
    decode_packed_indices_from_words(value_count, bits_per_entry, &data, "palette data")
}

fn decode_biome_palette_values(
    compound: &earthmap_minecraft::nbt::Compound,
    palette_size: usize,
    value_count: usize,
) -> std::result::Result<Vec<usize>, String> {
    let bits_per_entry = biome_bits_per_entry(palette_size)?;
    if bits_per_entry == 0 {
        return Ok(vec![0; value_count]);
    }
    let data = compound
        .get_long_array("data")
        .map_err(|error| error.to_string())?;
    decode_packed_indices_from_words(value_count, bits_per_entry, &data, "biome data")
}

fn biome_bits_per_entry(palette_size: usize) -> std::result::Result<u8, String> {
    if palette_size == 0 {
        return Err("paletteSize must be positive: 0".to_string());
    }
    if palette_size == 1 {
        return Ok(0);
    }
    Ok((usize::BITS - (palette_size - 1).leading_zeros()) as u8)
}

fn decode_packed_indices_from_words(
    value_count: usize,
    bits_per_value: u8,
    words: &[i64],
    label: &str,
) -> std::result::Result<Vec<usize>, String> {
    let data = words.iter().map(|word| *word as u64).collect::<Vec<_>>();
    let packed = PackedLongArray::from_words(value_count, bits_per_value, data)
        .map_err(|error| error.to_string())?;
    let mut values = Vec::with_capacity(value_count);
    for index in 0..value_count {
        let value = packed.get(index).map_err(|error| error.to_string())?;
        if value < 0 {
            return Err(format!(
                "{label} contains negative value at {index}: {value}"
            ));
        }
        values.push(value as usize);
    }
    Ok(values)
}

fn compare_heightmap_values(
    expected: &DecodedChunkDetails,
    actual: &DecodedChunkDetails,
    name: &str,
    prefix: &str,
    limit: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(expected_values) = expected.heightmaps.get(name) else {
        lines.push(format!("{prefix}.missingExpected=true"));
        return lines;
    };
    let Some(actual_values) = actual.heightmaps.get(name) else {
        lines.push(format!("{prefix}.missingActual=true"));
        return lines;
    };
    let mut diff_count = 0usize;
    for local_z in 0..CHUNK_WIDTH {
        for local_x in 0..CHUNK_WIDTH {
            let index = (local_z * CHUNK_WIDTH) + local_x;
            if expected_values.get(index) != actual_values.get(index) {
                diff_count += 1;
                if diff_count <= limit {
                    lines.push(format!(
                        "{prefix}.diff.{diff_count}=localX={local_x},localZ={local_z},expectedTopYExclusive={},actualTopYExclusive={}",
                        expected_values.get(index).copied().unwrap_or_default(),
                        actual_values.get(index).copied().unwrap_or_default()
                    ));
                }
            }
        }
    }
    lines.insert(0, format!("{prefix}.diffCount={diff_count}"));
    lines
}

fn compare_biome_cells(
    expected: &DecodedChunkDetails,
    actual: &DecodedChunkDetails,
    limit: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut diff_count = 0usize;
    let section_ys = expected
        .sections
        .keys()
        .chain(actual.sections.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for section_y in section_ys {
        let Some(expected_section) = expected.sections.get(&section_y) else {
            lines.push(format!(
                "biomeCell.section.{section_y}.missingExpected=true"
            ));
            diff_count += SECTION_BIOME_CELL_COUNT;
            continue;
        };
        let Some(actual_section) = actual.sections.get(&section_y) else {
            lines.push(format!("biomeCell.section.{section_y}.missingActual=true"));
            diff_count += SECTION_BIOME_CELL_COUNT;
            continue;
        };
        for index in 0..SECTION_BIOME_CELL_COUNT {
            let expected_name = palette_name_at(
                &expected_section.biome_palette,
                expected_section.biome_values.get(index).copied(),
            );
            let actual_name = palette_name_at(
                &actual_section.biome_palette,
                actual_section.biome_values.get(index).copied(),
            );
            if expected_name != actual_name {
                diff_count += 1;
                if diff_count <= limit {
                    let cell_x = index & 3;
                    let cell_z = (index >> 2) & 3;
                    let cell_y = (index >> 4) & 3;
                    lines.push(format!(
                        "biomeCell.diff.{diff_count}=sectionY={section_y},cellX={cell_x},cellY={cell_y},cellZ={cell_z},expected={expected_name},actual={actual_name}"
                    ));
                }
            }
        }
    }
    lines.insert(0, format!("biomeCell.diffCount={diff_count}"));
    lines
}

fn compare_block_cells(
    expected: &DecodedChunkDetails,
    actual: &DecodedChunkDetails,
    limit: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut diff_count = 0usize;
    let section_ys = expected
        .sections
        .keys()
        .chain(actual.sections.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for section_y in section_ys {
        let Some(expected_section) = expected.sections.get(&section_y) else {
            lines.push(format!("block.section.{section_y}.missingExpected=true"));
            diff_count += SECTION_BLOCK_COUNT;
            continue;
        };
        let Some(actual_section) = actual.sections.get(&section_y) else {
            lines.push(format!("block.section.{section_y}.missingActual=true"));
            diff_count += SECTION_BLOCK_COUNT;
            continue;
        };
        for index in 0..SECTION_BLOCK_COUNT {
            let expected_name = palette_name_at(
                &expected_section.block_palette,
                expected_section.block_values.get(index).copied(),
            );
            let actual_name = palette_name_at(
                &actual_section.block_palette,
                actual_section.block_values.get(index).copied(),
            );
            if expected_name != actual_name {
                diff_count += 1;
                if diff_count <= limit {
                    let local_x = index & 15;
                    let local_z = (index >> 4) & 15;
                    let local_y = (index >> 8) & 15;
                    lines.push(format!(
                        "block.diff.{diff_count}=sectionY={section_y},localX={local_x},localY={local_y},localZ={local_z},expected={expected_name},actual={actual_name}"
                    ));
                }
            }
        }
    }
    lines.insert(0, format!("block.diffCount={diff_count}"));
    lines
}

fn palette_name_at(palette: &[String], index: Option<usize>) -> String {
    index
        .and_then(|index| palette.get(index))
        .cloned()
        .unwrap_or_else(|| "<invalid>".to_string())
}

fn block_state_palette_names(
    compound: &earthmap_minecraft::nbt::Compound,
) -> std::result::Result<Vec<String>, String> {
    let palette = compound
        .get_list("palette")
        .map_err(|error| error.to_string())?;
    let mut names = Vec::with_capacity(palette.values().len());
    for value in palette.values() {
        let Tag::Compound(block) = value else {
            return Err("block state palette must contain compounds".to_string());
        };
        names.push(
            block
                .get_string("Name")
                .map_err(|error| error.to_string())?
                .to_string(),
        );
    }
    Ok(names)
}

fn biome_palette_names(
    compound: &earthmap_minecraft::nbt::Compound,
) -> std::result::Result<Vec<String>, String> {
    let palette = compound
        .get_list("palette")
        .map_err(|error| error.to_string())?;
    let mut names = Vec::with_capacity(palette.values().len());
    for value in palette.values() {
        let Tag::String(name) = value else {
            return Err("biome palette must contain strings".to_string());
        };
        names.push(name.clone());
    }
    Ok(names)
}

fn long_array_len(
    compound: &earthmap_minecraft::nbt::Compound,
    name: &str,
) -> std::result::Result<usize, String> {
    if !compound.contains(name) {
        return Ok(0);
    }
    match compound.get(name) {
        Ok(Tag::LongArray(values)) => Ok(values.len()),
        Ok(tag) => Err(format!(
            "{name} must be a long array when present, found tag type {}",
            tag.type_id()
        )),
        Err(error) => Err(error.to_string()),
    }
}

fn parse_local_chunk_coord(text: &str, name: &str) -> std::result::Result<u8, String> {
    let value = text.parse::<u8>().map_err(|error| error.to_string())?;
    if value >= 32 {
        return Err(format!("{name} must be between 0 and 31"));
    }
    Ok(value)
}

fn benchmark_surface_input_open(
    out: &mut impl Write,
    err: &mut impl Write,
    true_marble_path_text: &str,
) -> io::Result<i32> {
    match benchmark_surface_input_open_impl(true_marble_path_text) {
        Ok(lines) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Surface input open benchmark failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn benchmark_surface_input_open_impl(
    true_marble_path_text: &str,
) -> std::result::Result<Vec<String>, String> {
    let true_marble_path = Path::new(true_marble_path_text);
    let terrain_dir = true_marble_path
        .canonicalize()
        .unwrap_or_else(|_| true_marble_path.to_path_buf())
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "TrueMarble path has no parent directory".to_string())?;
    let tif_root = terrain_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "TrueMarble terrain directory has no TifFiles parent".to_string())?;
    let earth_root = tif_root
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "TifFiles directory has no EarthMap root parent".to_string())?;
    let vegetation = tif_root.join("vegetation");
    let mut lines = vec![
        "Surface input open benchmark".to_string(),
        format!("trueMarblePath={}", true_marble_path.display()),
        "type,name,elapsedMillis,status".to_string(),
    ];

    time_surface_input_open(&mut lines, "rgb-vrt", "TrueMarble.vrt", || {
        VrtRgbMosaicReader::open_with_tile_cache_entries(true_marble_path, 512)
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    for (name, path) in [
        ("climate.tif", tif_root.join("climate.tif")),
        (
            "EvergreenBroadleafTrees.tif",
            vegetation.join("EvergreenBroadleafTrees.tif"),
        ),
        (
            "DeciduousBroadleafTrees.tif",
            vegetation.join("DeciduousBroadleafTrees.tif"),
        ),
        (
            "EvergreenDeciduousNeedleleafTrees.tif",
            vegetation.join("EvergreenDeciduousNeedleleafTrees.tif"),
        ),
        ("mixed.tif", vegetation.join("mixed.tif")),
        (
            "HerbaceousVegetation.tif",
            vegetation.join("HerbaceousVegetation.tif"),
        ),
        ("Shrubs.tif", vegetation.join("Shrubs.tif")),
        ("Snow.tif", vegetation.join("Snow.tif")),
        ("Swamp.tif", vegetation.join("Swamp.tif")),
        (
            "ocean_temp_infill.tif",
            tif_root.join("ocean_temp_infill.tif"),
        ),
    ] {
        time_surface_input_open(&mut lines, "single-band", name, || {
            if !path.is_file() {
                return Err("missing".to_string());
            }
            GeoTiffSingleBandReader::open_with_tile_cache_entries(&path, 512)
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
    }
    for (name, path) in [
        ("bathymetry.tif", tif_root.join("bathymetry.tif")),
        ("slope.tif", tif_root.join("slope.tif")),
    ] {
        time_surface_input_open(&mut lines, "float32", name, || {
            if !path.is_file() {
                return Err("missing".to_string());
            }
            GeoTiffFloat32Reader::open(&path)
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
    }
    time_surface_input_open(&mut lines, "ecoregion", "wwf_terr_ecos", || {
        WwfEcoregionSampler::open(
            earth_root
                .join("ShapeFiles")
                .join("ecoregionsOrig")
                .join("wwf_terr_ecos.shp"),
            earth_root.join("ecoregions.csv"),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    });
    time_surface_input_open(&mut lines, "met", "image_exports", || {
        MetImageExportTerrainSampler::open_auto(true_marble_path)
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    time_surface_input_open(&mut lines, "topo", "land_shallow_topo", || {
        LandShallowTopoPhotoSampler::open_near(true_marble_path)
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    time_surface_input_open(&mut lines, "earth-data", "all", || {
        EarthDataSurfaceMaterialSampler::open_with_tile_cache_entries(true_marble_path, 512)
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    Ok(lines)
}

fn time_surface_input_open<F>(lines: &mut Vec<String>, kind: &str, name: &str, open: F)
where
    F: FnOnce() -> std::result::Result<(), String>,
{
    let start = Instant::now();
    let status = match open() {
        Ok(()) => "ok".to_string(),
        Err(error) => format!("error:{}", csv_safe_status(&error)),
    };
    lines.push(format!(
        "{kind},{name},{},{}",
        start.elapsed().as_millis(),
        status
    ));
}

fn csv_safe_status(value: &str) -> String {
    value.replace(',', ";").replace('\n', " ")
}

#[allow(clippy::too_many_arguments)]
fn benchmark_height_regions(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
) -> io::Result<i32> {
    match benchmark_height_regions_impl(
        out,
        heightmap_path,
        world_dir,
        scale_text,
        start_region_x_text,
        start_region_z_text,
        cols_text,
        rows_text,
        format_text,
    ) {
        Ok(()) => Ok(EXIT_OK),
        Err(error) => {
            writeln!(err, "Height region benchmark failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn benchmark_surface_regions(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
) -> io::Result<i32> {
    match benchmark_surface_regions_impl(
        out,
        heightmap_path,
        world_dir,
        scale_text,
        start_region_x_text,
        start_region_z_text,
        cols_text,
        rows_text,
        format_text,
    ) {
        Ok(()) => Ok(EXIT_OK),
        Err(error) => {
            writeln!(err, "Surface region benchmark failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn benchmark_survival_regions(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
) -> io::Result<i32> {
    match benchmark_survival_regions_impl(
        out,
        heightmap_path,
        world_dir,
        scale_text,
        start_region_x_text,
        start_region_z_text,
        cols_text,
        rows_text,
        format_text,
    ) {
        Ok(()) => Ok(EXIT_OK),
        Err(error) => {
            writeln!(err, "Survival region benchmark failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn benchmark_survival_regions_parallel(
    out: &mut impl Write,
    err: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
    threads_text: &str,
) -> io::Result<i32> {
    writeln!(
        out,
        "notice=legacy survival parallel benchmark is replaced by Rust vanilla-delegated parallel generation"
    )?;
    generate_vanilla_delegated_regions_parallel(
        out,
        err,
        heightmap_path,
        world_dir,
        scale_text,
        start_region_x_text,
        start_region_z_text,
        cols_text,
        rows_text,
        format_text,
        threads_text,
        "surface",
        "surfaceRaster=auto",
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
fn benchmark_height_regions_impl(
    out: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
) -> std::result::Result<(), String> {
    let grid = parse_benchmark_region_grid(
        scale_text,
        start_region_x_text,
        start_region_z_text,
        cols_text,
        rows_text,
        format_text,
    )?;
    let cache_rows = configured_heightmap_cache_rows(Path::new(heightmap_path))?;
    let total_start = Instant::now();
    let mut total_chunks = 0usize;
    let mut total_cache_hits = 0u64;
    let mut total_cache_misses = 0u64;
    let mut total_cache_evictions = 0u64;
    writeln!(
        out,
        "type,regionX,regionZ,elapsedMillis,chunks,minSurfaceY,maxSurfaceY,cacheHits,cacheMisses,cacheEvictions,regionFile"
    )
    .map_err(|error| error.to_string())?;
    for region_z in grid.region_z_range() {
        for region_x in grid.region_x_range() {
            let start = Instant::now();
            let settings = HeightOnlySettings::new(
                heightmap_path,
                world_dir,
                "SR EarthMap Height Benchmark",
                0,
                grid.scale,
                region_x,
                region_z,
                grid.format,
                cache_rows,
            )
            .map_err(|error| error.to_string())?;
            let report = earthmap_surface::generate_height_only_region(&settings)
                .map_err(|error| error.to_string())?;
            let elapsed_millis = start.elapsed().as_millis();
            total_chunks += report.chunk_count;
            total_cache_hits += report.cache_stats.hits;
            total_cache_misses += report.cache_stats.misses;
            total_cache_evictions += report.cache_stats.evictions;
            writeln!(
                out,
                "region,{},{},{},{},{},{},{},{},{},{}",
                report.region_x,
                report.region_z,
                elapsed_millis,
                report.chunk_count,
                report.min_surface_y,
                report.max_surface_y,
                report.cache_stats.hits,
                report.cache_stats.misses,
                report.cache_stats.evictions,
                report.region_file.display()
            )
            .map_err(|error| error.to_string())?;
        }
    }
    let total_elapsed_millis = total_start.elapsed().as_millis();
    write_benchmark_summary(
        out,
        grid.region_count(),
        total_chunks,
        total_elapsed_millis,
        &[
            ("cacheHits", total_cache_hits.to_string()),
            ("cacheMisses", total_cache_misses.to_string()),
            ("cacheEvictions", total_cache_evictions.to_string()),
        ],
    )
}

#[allow(clippy::too_many_arguments)]
fn benchmark_surface_regions_impl(
    out: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
) -> std::result::Result<(), String> {
    let grid = parse_benchmark_region_grid(
        scale_text,
        start_region_x_text,
        start_region_z_text,
        cols_text,
        rows_text,
        format_text,
    )?;
    let cache_rows = configured_heightmap_cache_rows(Path::new(heightmap_path))?;
    let surface_material_path =
        parse_optional_surface_material_path("surfaceRaster=auto", Path::new(heightmap_path))?;
    let texture_mode = if surface_material_path.is_some() {
        SurfaceTextureMode::Photo
    } else {
        SurfaceTextureMode::Classified
    };
    let surface_tile_cache_entries =
        configured_surface_tile_cache_entries(surface_material_path.as_deref())?;
    let total_start = Instant::now();
    let mut total_chunks = 0usize;
    let mut total_land_columns = 0i64;
    let mut total_water_columns = 0i64;
    writeln!(
        out,
        "type,regionX,regionZ,elapsedMillis,chunks,landColumns,waterColumns,minGroundY,maxGroundY,surfaceSampleMillis,chunkBuildMillis,nbtEncodeMillis,regionWriteMillis,previewMillis,metadataMillis,totalInternalMillis,cacheHits,cacheMisses,cacheEvictions,regionFile"
    )
    .map_err(|error| error.to_string())?;
    for region_z in grid.region_z_range() {
        for region_x in grid.region_x_range() {
            let report = generate_surface_benchmark_region(
                heightmap_path,
                world_dir,
                "SR EarthMap Surface Benchmark",
                grid,
                region_x,
                region_z,
                cache_rows,
                texture_mode,
                surface_material_path.as_deref(),
                surface_tile_cache_entries,
                ChunkGenerationStatus::Full,
            )?;
            total_chunks += report.report.chunk_count;
            total_land_columns += i64::from(report.report.land_columns);
            total_water_columns += i64::from(report.report.water_columns);
            writeln!(
                out,
                "region,{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                report.report.region_x,
                report.report.region_z,
                report.elapsed_millis,
                report.report.chunk_count,
                report.report.land_columns,
                report.report.water_columns,
                report.report.min_ground_y,
                report.report.max_ground_y,
                millis(report.report.surface_sample_nanos),
                millis(report.report.chunk_build_nanos),
                millis(report.report.nbt_encode_nanos),
                millis(report.report.region_write_nanos),
                millis(report.report.preview_nanos),
                millis(report.report.metadata_nanos),
                millis(report.report.total_nanos),
                report.report.cache_stats.hits,
                report.report.cache_stats.misses,
                report.report.cache_stats.evictions,
                report.report.region_file.display()
            )
            .map_err(|error| error.to_string())?;
        }
    }
    let total_elapsed_millis = total_start.elapsed().as_millis();
    write_benchmark_summary(
        out,
        grid.region_count(),
        total_chunks,
        total_elapsed_millis,
        &[
            ("landColumns", total_land_columns.to_string()),
            ("waterColumns", total_water_columns.to_string()),
            ("textureMode", texture_mode.id().to_string()),
        ],
    )
}

#[allow(clippy::too_many_arguments)]
fn benchmark_survival_regions_impl(
    out: &mut impl Write,
    heightmap_path: &str,
    world_dir: &str,
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
) -> std::result::Result<(), String> {
    let grid = parse_benchmark_region_grid(
        scale_text,
        start_region_x_text,
        start_region_z_text,
        cols_text,
        rows_text,
        format_text,
    )?;
    let cache_rows = configured_heightmap_cache_rows(Path::new(heightmap_path))?;
    let surface_material_path =
        parse_optional_surface_material_path("surfaceRaster=auto", Path::new(heightmap_path))?;
    let texture_mode = if surface_material_path.is_some() {
        SurfaceTextureMode::Photo
    } else {
        SurfaceTextureMode::Classified
    };
    let surface_tile_cache_entries =
        configured_surface_tile_cache_entries(surface_material_path.as_deref())?;
    let total_start = Instant::now();
    let mut total_chunks = 0usize;
    let mut total_land_columns = 0i64;
    let mut total_water_columns = 0i64;
    let mut total_output_bytes = 0u64;
    let mut total_cache_hits = 0u64;
    let mut total_cache_misses = 0u64;
    let mut total_cache_evictions = 0u64;
    writeln!(
        out,
        "type,regionX,regionZ,elapsedMillis,chunks,landColumns,waterColumns,minGroundY,maxGroundY,carvedCaveBlocks,caveEntranceColumns,undergroundWaterBlocks,undergroundLavaBlocks,placedOreBlocks,deepslateBlocks,progressionStructures,surfaceMillis,caveMillis,fluidMillis,oreMillis,progressionMillis,nbtEncodeMillis,regionWriteMillis,reportMillis,totalInternalMillis,outputBytes,cacheHits,cacheMisses,cacheEvictions,regionFile"
    )
    .map_err(|error| error.to_string())?;
    for region_z in grid.region_z_range() {
        for region_x in grid.region_x_range() {
            let report = generate_surface_benchmark_region(
                heightmap_path,
                world_dir,
                "SR EarthMap Survival Delegated Benchmark",
                grid,
                region_x,
                region_z,
                cache_rows,
                texture_mode,
                surface_material_path.as_deref(),
                surface_tile_cache_entries,
                ChunkGenerationStatus::Surface,
            )?;
            let output_bytes = std::fs::metadata(&report.report.region_file)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            total_chunks += report.report.chunk_count;
            total_land_columns += i64::from(report.report.land_columns);
            total_water_columns += i64::from(report.report.water_columns);
            total_output_bytes += output_bytes;
            total_cache_hits += report.report.cache_stats.hits;
            total_cache_misses += report.report.cache_stats.misses;
            total_cache_evictions += report.report.cache_stats.evictions;
            writeln!(
                out,
                "region,{},{},{},{},{},{},{},{},0,0,0,0,0,0,0,{},0,0,0,0,{},{},0,{},{},{},{},{},{}",
                report.report.region_x,
                report.report.region_z,
                report.elapsed_millis,
                report.report.chunk_count,
                report.report.land_columns,
                report.report.water_columns,
                report.report.min_ground_y,
                report.report.max_ground_y,
                millis(report.report.surface_sample_nanos),
                millis(report.report.nbt_encode_nanos),
                millis(report.report.region_write_nanos),
                millis(report.report.total_nanos),
                output_bytes,
                report.report.cache_stats.hits,
                report.report.cache_stats.misses,
                report.report.cache_stats.evictions,
                report.report.region_file.display()
            )
            .map_err(|error| error.to_string())?;
        }
    }
    let total_elapsed_millis = total_start.elapsed().as_millis();
    write_benchmark_summary(
        out,
        grid.region_count(),
        total_chunks,
        total_elapsed_millis,
        &[
            ("landColumns", total_land_columns.to_string()),
            ("waterColumns", total_water_columns.to_string()),
            ("outputBytes", total_output_bytes.to_string()),
            ("cacheHits", total_cache_hits.to_string()),
            ("cacheMisses", total_cache_misses.to_string()),
            ("cacheEvictions", total_cache_evictions.to_string()),
            ("model", "vanillaDelegatedSurface".to_string()),
            ("textureMode", texture_mode.id().to_string()),
        ],
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BenchmarkRegionGrid {
    scale: i32,
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
    format: OutputFormat,
}

impl BenchmarkRegionGrid {
    fn region_count(self) -> usize {
        usize::try_from(self.cols * self.rows).unwrap_or(0)
    }

    fn region_x_range(self) -> std::ops::Range<i32> {
        self.start_region_x..self.start_region_x + self.cols
    }

    fn region_z_range(self) -> std::ops::Range<i32> {
        self.start_region_z..self.start_region_z + self.rows
    }
}

fn parse_benchmark_region_grid(
    scale_text: &str,
    start_region_x_text: &str,
    start_region_z_text: &str,
    cols_text: &str,
    rows_text: &str,
    format_text: &str,
) -> std::result::Result<BenchmarkRegionGrid, String> {
    let scale = parse_i32_string(scale_text)?;
    let start_region_x = parse_i32_string(start_region_x_text)?;
    let start_region_z = parse_i32_string(start_region_z_text)?;
    let cols = parse_positive_i32_string("cols", cols_text)?;
    let rows = parse_positive_i32_string("rows", rows_text)?;
    let format = OutputFormat::parse(format_text).map_err(|error| error.to_string())?;
    let _ = cols
        .checked_mul(rows)
        .ok_or_else(|| "region grid size overflow".to_string())?;
    Ok(BenchmarkRegionGrid {
        scale,
        start_region_x,
        start_region_z,
        cols,
        rows,
        format,
    })
}

#[allow(clippy::too_many_arguments)]
fn generate_surface_benchmark_region(
    heightmap_path: &str,
    world_dir: &str,
    level_name: &str,
    grid: BenchmarkRegionGrid,
    region_x: i32,
    region_z: i32,
    cache_rows: usize,
    texture_mode: SurfaceTextureMode,
    surface_material_path: Option<&Path>,
    surface_tile_cache_entries: usize,
    status: ChunkGenerationStatus,
) -> std::result::Result<TimedSurfaceRegionReport, String> {
    let mut settings = SurfaceRegionSettings::new_with_texture_options(
        heightmap_path,
        world_dir,
        level_name,
        0,
        grid.scale,
        region_x,
        region_z,
        grid.format,
        cache_rows,
        true,
        status,
        1.0,
        texture_mode,
    )
    .map_err(|error| error.to_string())?;
    settings.surface_material_path = surface_material_path.map(Path::to_path_buf);
    settings.surface_tile_cache_entries = surface_tile_cache_entries;
    let start = Instant::now();
    let report =
        earthmap_surface::generate_surface_region(&settings).map_err(|error| error.to_string())?;
    Ok(TimedSurfaceRegionReport {
        elapsed_millis: start.elapsed().as_millis(),
        report,
    })
}

#[derive(Clone, Debug, PartialEq)]
struct TimedSurfaceRegionReport {
    elapsed_millis: u128,
    report: SurfaceRegionReport,
}

fn write_benchmark_summary(
    out: &mut impl Write,
    region_count: usize,
    chunks: usize,
    elapsed_millis: u128,
    extra_fields: &[(&str, String)],
) -> std::result::Result<(), String> {
    let avg = if region_count == 0 {
        0.0
    } else {
        elapsed_millis as f64 / region_count as f64
    };
    let regions_per_hour = if elapsed_millis == 0 {
        0.0
    } else {
        region_count as f64 * 3_600_000.0 / elapsed_millis as f64
    };
    let chunks_per_second = if elapsed_millis == 0 {
        0.0
    } else {
        chunks as f64 * 1000.0 / elapsed_millis as f64
    };
    write!(
        out,
        "summary,regions={region_count},chunks={chunks},elapsedMillis={elapsed_millis},avgMillisPerRegion={},regionsPerHour={},chunksPerSecond={}",
        java_double_string(avg),
        java_double_string(regions_per_hour),
        java_double_string(chunks_per_second)
    )
    .map_err(|error| error.to_string())?;
    for (name, value) in extra_fields {
        write!(out, ",{name}={value}").map_err(|error| error.to_string())?;
    }
    writeln!(out).map_err(|error| error.to_string())
}

fn benchmark_region_writers(
    out: &mut impl Write,
    err: &mut impl Write,
    output_dir: &str,
    iterations_text: &str,
) -> io::Result<i32> {
    match benchmark_region_writers_impl(output_dir, iterations_text) {
        Ok(report) => {
            writeln!(out, "Region writers benchmarked")?;
            writeln!(out, "outputDir={}", report.output_dir)?;
            writeln!(out, "iterations={}", report.iterations)?;
            writeln!(out, "payloadChunks={}", report.payload_chunks)?;
            writeln!(out, "mcaAverageMillis={}", report.mca_average_millis)?;
            writeln!(out, "linearAverageMillis={}", report.linear_average_millis)?;
            writeln!(
                out,
                "linearCompressionLevels={}",
                report.linear_compression_levels
            )?;
            writeln!(out, "reportJson={}", report.report_json)?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Region writer benchmark failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RegionWriterBenchmarkReport {
    output_dir: String,
    iterations: usize,
    payload_chunks: usize,
    mca_average_millis: u128,
    linear_average_millis: u128,
    linear_compression_levels: usize,
    report_json: String,
}

fn benchmark_region_writers_impl(
    output_dir: &str,
    iterations_text: &str,
) -> std::result::Result<RegionWriterBenchmarkReport, String> {
    let iterations = parse_benchmark_iterations(iterations_text)?;
    let output_dir = Path::new(output_dir);
    let payloads = flat_test_region_payloads()?;
    let mca_file = output_dir.join("mca").join("r.0.0.mca");
    let linear_file = output_dir.join("linear").join("r.0.0.linear");
    let mut mca_millis = Vec::with_capacity(iterations);
    let mut linear_millis = Vec::with_capacity(iterations);
    let linear_compression_levels = [1, 4, 9];

    for _ in 0..iterations {
        let start = Instant::now();
        earthmap_region::write_mca_region(&mca_file, &payloads, 0)
            .map_err(|error| error.to_string())?;
        mca_millis.push(start.elapsed().as_millis());

        let start = Instant::now();
        earthmap_region::write_linear_v2_region(&linear_file, &payloads, 0)
            .map_err(|error| error.to_string())?;
        linear_millis.push(start.elapsed().as_millis());
    }

    let mut linear_compression_reports = Vec::with_capacity(linear_compression_levels.len());
    for compression_level in linear_compression_levels {
        let compression_file = output_dir
            .join(format!("linear-level-{compression_level}"))
            .join("r.0.0.linear");
        let mut iteration_millis = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let start = Instant::now();
            earthmap_region::write_linear_v2_region_with_compression(
                &compression_file,
                &payloads,
                0,
                compression_level,
            )
            .map_err(|error| error.to_string())?;
            iteration_millis.push(start.elapsed().as_millis());
        }
        let bytes = std::fs::metadata(&compression_file)
            .map_err(|error| error.to_string())?
            .len();
        let sha256 = earthmap_parity::sha256_file_hex(&compression_file)
            .map_err(|error| error.to_string())?;
        let average_millis = average_u128(&iteration_millis);
        linear_compression_reports.push(serde_json::json!({
            "compressionLevel": compression_level,
            "regionFile": compression_file.display().to_string(),
            "bytes": bytes,
            "sha256": sha256,
            "iterationMillis": iteration_millis,
            "averageMillis": average_millis,
        }));
    }

    let mca_bytes = std::fs::metadata(&mca_file)
        .map_err(|error| error.to_string())?
        .len();
    let linear_bytes = std::fs::metadata(&linear_file)
        .map_err(|error| error.to_string())?
        .len();
    let mca_sha256 =
        earthmap_parity::sha256_file_hex(&mca_file).map_err(|error| error.to_string())?;
    let linear_sha256 =
        earthmap_parity::sha256_file_hex(&linear_file).map_err(|error| error.to_string())?;
    let mca_average_millis = average_u128(&mca_millis);
    let linear_average_millis = average_u128(&linear_millis);
    let report_json = output_dir.join("region-writer-benchmark.json");
    let document = serde_json::json!({
        "schema": "earthmap-rust-region-writer-benchmark-v1",
        "iterations": iterations,
        "payloadChunks": payloads.len(),
        "runtime": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "availableParallelism": std::thread::available_parallelism().map(|value| value.get()).unwrap_or(0),
            "rayonNumThreadsEnv": std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "unset".to_string()),
        },
        "mca": {
            "regionFile": mca_file.display().to_string(),
            "bytes": mca_bytes,
            "sha256": mca_sha256,
            "iterationMillis": mca_millis,
            "averageMillis": mca_average_millis,
        },
        "linear": {
            "regionFile": linear_file.display().to_string(),
            "bytes": linear_bytes,
            "sha256": linear_sha256,
            "iterationMillis": linear_millis,
            "averageMillis": linear_average_millis,
        },
        "linearCompressionLevels": linear_compression_reports,
    });
    write_json(&report_json, &document)?;

    Ok(RegionWriterBenchmarkReport {
        output_dir: output_dir.display().to_string(),
        iterations,
        payload_chunks: payloads.len(),
        mca_average_millis,
        linear_average_millis,
        linear_compression_levels: linear_compression_reports.len(),
        report_json: report_json.display().to_string(),
    })
}

fn parse_benchmark_iterations(text: &str) -> std::result::Result<usize, String> {
    let raw = text
        .split_once('=')
        .map(|(_, value)| value)
        .unwrap_or(text)
        .trim();
    let iterations = raw.parse::<usize>().map_err(|error| error.to_string())?;
    if !(1..=20).contains(&iterations) {
        return Err(format!("iterations must be between 1 and 20: {iterations}"));
    }
    Ok(iterations)
}

fn average_u128(values: &[u128]) -> u128 {
    if values.is_empty() {
        0
    } else {
        values.iter().sum::<u128>() / values.len() as u128
    }
}

fn generate_flat_test_world(
    out: &mut impl Write,
    err: &mut impl Write,
    world_dir: &str,
    format_text: &str,
) -> io::Result<i32> {
    let normalized_format = format_text.to_ascii_lowercase();
    match generate_flat_test_world_impl(world_dir, &normalized_format) {
        Ok(region_file) => {
            writeln!(out, "Flat test world generated")?;
            writeln!(out, "format={normalized_format}")?;
            writeln!(out, "regionFile={}", region_file.display())?;
            writeln!(
                out,
                "manifestFile={}",
                Path::new(world_dir)
                    .join(SURVIVAL_MANIFEST_FILE_NAME)
                    .display()
            )?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Flat test world generation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn generate_flat_test_world_impl(
    world_dir: &str,
    normalized_format: &str,
) -> std::result::Result<std::path::PathBuf, String> {
    let world_dir = Path::new(world_dir);
    let (level_name, seed, generator_name, region_name) = match normalized_format {
        "mca" => (
            FLAT_TEST_MCA_LEVEL_NAME,
            FLAT_TEST_MCA_SEED,
            "flat-test-mca",
            "r.0.0.mca",
        ),
        "linear" => (
            FLAT_TEST_LINEAR_LEVEL_NAME,
            FLAT_TEST_LINEAR_SEED,
            "flat-test-linear-v2",
            "r.0.0.linear",
        ),
        _ => return Err("format must be mca or linear".to_string()),
    };
    let region_dir = world_dir.join("region");
    std::fs::create_dir_all(&region_dir).map_err(|error| error.to_string())?;
    let settings =
        level_dat_template::Settings::new(level_name, seed, 8, FLAT_TEST_SURFACE_Y + 2, 8)
            .map_err(|error| error.to_string())?;
    level_dat_template::write(world_dir.join("level.dat"), &settings)
        .map_err(|error| error.to_string())?;

    let payloads = flat_test_region_payloads()?;
    let region_file = region_dir.join(region_name);
    match normalized_format {
        "mca" => earthmap_region::write_mca_region(&region_file, &payloads, 0),
        "linear" => earthmap_region::write_linear_v2_region(&region_file, &payloads, 0),
        _ => unreachable!("format was validated above"),
    }
    .map_err(|error| error.to_string())?;
    write_exploration_only_manifest(
        world_dir,
        generator_name,
        &[("generator.purpose", "format-validation")],
    )
    .map_err(|error| error.to_string())?;
    Ok(region_file)
}

fn flat_test_region_payloads() -> std::result::Result<BTreeMap<ChunkLocalPos, Vec<u8>>, String> {
    let mut payloads = BTreeMap::new();
    for chunk_z in 0..FLAT_TEST_REGION_CHUNKS {
        for chunk_x in 0..FLAT_TEST_REGION_CHUNKS {
            let chunk = flat_test_chunk(i32::from(chunk_x), i32::from(chunk_z))?;
            let bytes =
                chunk_nbt_encoder::encode_to_bytes(&chunk, 0).map_err(|error| error.to_string())?;
            let pos = ChunkLocalPos::new(chunk_x, chunk_z).map_err(|error| error.to_string())?;
            payloads.insert(pos, bytes);
        }
    }
    Ok(payloads)
}

fn flat_test_chunk(chunk_x: i32, chunk_z: i32) -> std::result::Result<ChunkModel, String> {
    let mut chunk = ChunkModel::overworld(chunk_x, chunk_z);
    fill_flat_base_terrain(&mut chunk, FLAT_TEST_SURFACE_Y)?;
    Ok(chunk)
}

fn generate_palette_stress_world(
    out: &mut impl Write,
    err: &mut impl Write,
    world_dir: &str,
) -> io::Result<i32> {
    match generate_palette_stress_world_impl(world_dir) {
        Ok(region_file) => {
            writeln!(out, "Palette stress world generated")?;
            writeln!(
                out,
                "stressBlockCount={}",
                PALETTE_STRESS_BLOCK_STATE_IDS.len()
            )?;
            writeln!(out, "stressSectionY={PALETTE_STRESS_SECTION_Y}")?;
            writeln!(
                out,
                "expectedStressSectionDataLongs={PALETTE_STRESS_EXPECTED_SECTION_DATA_LONGS}"
            )?;
            writeln!(out, "regionFile={}", region_file.display())?;
            writeln!(
                out,
                "manifestFile={}",
                Path::new(world_dir)
                    .join(SURVIVAL_MANIFEST_FILE_NAME)
                    .display()
            )?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "Palette stress world generation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn generate_palette_stress_world_impl(
    world_dir: &str,
) -> std::result::Result<std::path::PathBuf, String> {
    let world_dir = Path::new(world_dir);
    let region_dir = world_dir.join("region");
    std::fs::create_dir_all(&region_dir).map_err(|error| error.to_string())?;

    let settings = level_dat_template::Settings::new(
        PALETTE_STRESS_LEVEL_NAME,
        0,
        8,
        PALETTE_STRESS_SURFACE_Y + 2,
        8,
    )
    .map_err(|error| error.to_string())?;
    level_dat_template::write(world_dir.join("level.dat"), &settings)
        .map_err(|error| error.to_string())?;

    let mut payloads = BTreeMap::new();
    let chunk = palette_stress_chunk()?;
    let bytes = chunk_nbt_encoder::encode_to_bytes(&chunk, 0).map_err(|error| error.to_string())?;
    let pos = ChunkLocalPos::new(0, 0).map_err(|error| error.to_string())?;
    payloads.insert(pos, bytes);

    let region_file = region_dir.join("r.0.0.mca");
    earthmap_region::write_mca_region(&region_file, &payloads, 0)
        .map_err(|error| error.to_string())?;
    write_exploration_only_manifest(
        world_dir,
        "palette-stress-world",
        &[
            ("generator.purpose", "packed-palette-validation"),
            ("features.largeSectionPalette", "true"),
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(region_file)
}

fn palette_stress_chunk() -> std::result::Result<ChunkModel, String> {
    let mut chunk = ChunkModel::overworld(0, 0);
    fill_flat_base_terrain(&mut chunk, PALETTE_STRESS_SURFACE_Y)?;

    for (index, block_state_id) in PALETTE_STRESS_BLOCK_STATE_IDS.iter().enumerate() {
        let local_x = (index % CHUNK_WIDTH) as i32;
        let local_z = (index / CHUNK_WIDTH) as i32;
        chunk
            .set_block_state_id(local_x, PALETTE_STRESS_Y, local_z, *block_state_id)
            .map_err(|error| error.to_string())?;
    }
    Ok(chunk)
}

fn fill_flat_base_terrain(
    chunk: &mut ChunkModel,
    surface_y: i32,
) -> std::result::Result<(), String> {
    for local_z in 0..CHUNK_WIDTH {
        for local_x in 0..CHUNK_WIDTH {
            let x = local_x as i32;
            let z = local_z as i32;
            chunk
                .set_block_state_id(x, -64, z, block_state_ids::BEDROCK)
                .map_err(|error| error.to_string())?;
            chunk
                .fill_column(x, z, -63, surface_y - 4, block_state_ids::STONE)
                .map_err(|error| error.to_string())?;
            chunk
                .fill_column(x, z, surface_y - 3, surface_y - 1, block_state_ids::DIRT)
                .map_err(|error| error.to_string())?;
            chunk
                .set_block_state_id(x, surface_y, z, block_state_ids::GRASS_BLOCK)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn write_exploration_only_manifest(
    world_dir: &Path,
    generator_name: &str,
    overrides: &[(&str, &str)],
) -> io::Result<std::path::PathBuf> {
    let mut values = base_exploration_only_manifest(generator_name);
    for (key, value) in overrides {
        values.insert((*key).to_string(), (*value).to_string());
    }
    std::fs::create_dir_all(world_dir)?;
    let manifest_path = world_dir.join(SURVIVAL_MANIFEST_FILE_NAME);
    let mut file = std::fs::File::create(&manifest_path)?;
    writeln!(file, "# SR EarthMap survival manifest")?;
    for (key, value) in values {
        writeln!(file, "{key}={}", escape_manifest_value(&value))?;
    }
    Ok(manifest_path)
}

#[allow(clippy::too_many_arguments)]
fn write_vanilla_delegated_parallel_manifest(
    world_dir: &Path,
    heightmap_path: &Path,
    format: OutputFormat,
    scale_denominator: i32,
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
    requested_threads: usize,
    status: ChunkGenerationStatus,
    surface_material_path: &Path,
    compression_options: RegionCompressionOptions,
    vertical_scale: f64,
    vertical_scale_mode: &str,
) -> io::Result<std::path::PathBuf> {
    let mut values = base_exploration_only_manifest("vanilla-delegated-regions-parallel");
    values.insert("features.surfaceRules".to_string(), "true".to_string());
    values.insert("features.waterSurface".to_string(), "true".to_string());
    values.insert("features.biomes".to_string(), "heuristic".to_string());
    values.insert(
        "features.surfaceMaterialRaster".to_string(),
        "true".to_string(),
    );
    values.insert("features.serverDelegation".to_string(), "true".to_string());
    values.insert(
        "generation.format".to_string(),
        format.java_name().to_string(),
    );
    values.insert(
        "generation.scaleDenominator".to_string(),
        scale_denominator.to_string(),
    );
    values.insert(
        "generation.heightmapPath".to_string(),
        normalized_path_display(heightmap_path),
    );
    values.insert(
        "generation.startRegionX".to_string(),
        start_region_x.to_string(),
    );
    values.insert(
        "generation.startRegionZ".to_string(),
        start_region_z.to_string(),
    );
    values.insert("generation.regionCols".to_string(), cols.to_string());
    values.insert("generation.regionRows".to_string(), rows.to_string());
    values.insert(
        "generation.threads".to_string(),
        requested_threads.max(1).to_string(),
    );
    values.insert(
        "generation.chunkStatus".to_string(),
        status.id().to_string(),
    );
    values.insert(
        "generation.verticalScale".to_string(),
        vertical_scale.to_string(),
    );
    values.insert(
        "generation.verticalScaleMode".to_string(),
        vertical_scale_mode.to_string(),
    );
    values.insert("generation.textureMode".to_string(), "photo".to_string());
    values.insert(
        "generation.surfaceSamplingProfile".to_string(),
        SURFACE_SAMPLING_PROFILE_VERSION.to_string(),
    );
    values.insert(
        "generation.surfaceMaterialPath".to_string(),
        normalized_path_display(surface_material_path),
    );
    if let Some(level) = compression_options.linear_compression_level {
        values.insert(
            "generation.linearCompressionLevel".to_string(),
            level.to_string(),
        );
    }
    if let Some(level) = compression_options.mca_compression_level {
        values.insert(
            "generation.mcaCompressionLevel".to_string(),
            level.to_string(),
        );
    }
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

    std::fs::create_dir_all(world_dir)?;
    let manifest_path = world_dir.join(SURVIVAL_MANIFEST_FILE_NAME);
    let mut file = std::fs::File::create(&manifest_path)?;
    writeln!(file, "# SR EarthMap survival manifest")?;
    for (key, value) in values {
        writeln!(file, "{key}={}", escape_manifest_value(&value))?;
    }
    Ok(manifest_path)
}

#[allow(clippy::too_many_arguments)]
fn write_vanilla_delegated_plan_manifest(
    world_dir: &Path,
    heightmap_path: &Path,
    format: OutputFormat,
    scale_denominator: i32,
    plan_csv: &Path,
    planned_regions: usize,
    submitted_regions: usize,
    requested_threads: usize,
    status: ChunkGenerationStatus,
    surface_material_path: &Path,
    compression_options: RegionCompressionOptions,
    vertical_scale: f64,
    vertical_scale_mode: &str,
) -> io::Result<std::path::PathBuf> {
    let mut values = base_exploration_only_manifest("vanilla-delegated-plan-parallel");
    values.insert("features.surfaceRules".to_string(), "true".to_string());
    values.insert("features.waterSurface".to_string(), "true".to_string());
    values.insert("features.biomes".to_string(), "heuristic".to_string());
    values.insert(
        "features.surfaceMaterialRaster".to_string(),
        "true".to_string(),
    );
    values.insert("features.serverDelegation".to_string(), "true".to_string());
    values.insert(
        "generation.format".to_string(),
        format.java_name().to_string(),
    );
    values.insert(
        "generation.scaleDenominator".to_string(),
        scale_denominator.to_string(),
    );
    values.insert(
        "generation.heightmapPath".to_string(),
        normalized_path_display(heightmap_path),
    );
    values.insert(
        "generation.planCsv".to_string(),
        normalized_path_display(plan_csv),
    );
    values.insert(
        "generation.plannedRegions".to_string(),
        planned_regions.to_string(),
    );
    values.insert(
        "generation.submittedRegions".to_string(),
        submitted_regions.to_string(),
    );
    values.insert(
        "generation.threads".to_string(),
        requested_threads.max(1).to_string(),
    );
    values.insert(
        "generation.chunkStatus".to_string(),
        status.id().to_string(),
    );
    values.insert(
        "generation.verticalScale".to_string(),
        vertical_scale.to_string(),
    );
    values.insert(
        "generation.verticalScaleMode".to_string(),
        vertical_scale_mode.to_string(),
    );
    values.insert("generation.textureMode".to_string(), "photo".to_string());
    values.insert(
        "generation.surfaceSamplingProfile".to_string(),
        SURFACE_SAMPLING_PROFILE_VERSION.to_string(),
    );
    values.insert(
        "generation.surfaceMaterialPath".to_string(),
        normalized_path_display(surface_material_path),
    );
    if let Some(level) = compression_options.linear_compression_level {
        values.insert(
            "generation.linearCompressionLevel".to_string(),
            level.to_string(),
        );
    }
    if let Some(level) = compression_options.mca_compression_level {
        values.insert(
            "generation.mcaCompressionLevel".to_string(),
            level.to_string(),
        );
    }
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

    std::fs::create_dir_all(world_dir)?;
    let manifest_path = world_dir.join(SURVIVAL_MANIFEST_FILE_NAME);
    let mut file = std::fs::File::create(&manifest_path)?;
    writeln!(file, "# SR EarthMap survival manifest")?;
    for (key, value) in values {
        writeln!(file, "{key}={}", escape_manifest_value(&value))?;
    }
    Ok(manifest_path)
}

fn base_exploration_only_manifest(generator_name: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    values.insert("manifest.version".to_string(), "1".to_string());
    values.insert(
        "minecraft.version".to_string(),
        build_info::MINECRAFT_TARGET.to_string(),
    );
    values.insert("gameplay.claim".to_string(), "exploration-only".to_string());
    values.insert("generator.name".to_string(), generator_name.to_string());
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

fn escape_manifest_value(value: &str) -> String {
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

fn write_nbt_parity_fixtures(
    out: &mut impl Write,
    err: &mut impl Write,
    output_dir: &str,
) -> io::Result<i32> {
    match earthmap_minecraft::nbt_parity_fixtures::write_nbt_parity_fixtures(output_dir) {
        Ok(()) => {
            writeln!(out, "nbtParityFixtures={output_dir}")?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "NBT parity fixture generation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn write_nbt_gzip_parity_fixtures(
    out: &mut impl Write,
    err: &mut impl Write,
    output_dir: &str,
) -> io::Result<i32> {
    match earthmap_minecraft::nbt_parity_fixtures::write_nbt_gzip_parity_fixtures(output_dir) {
        Ok(()) => {
            writeln!(out, "nbtGzipParityFixtures={output_dir}")?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(err, "NBT gzip parity fixture generation failed: {error}")?;
            Ok(EXIT_USAGE)
        }
    }
}

fn write_region_writer_parity_fixtures(
    out: &mut impl Write,
    err: &mut impl Write,
    output_dir: &str,
) -> io::Result<i32> {
    match write_region_writer_parity_fixtures_impl(output_dir) {
        Ok(()) => {
            writeln!(out, "regionWriterParityFixtures={output_dir}")?;
            Ok(EXIT_OK)
        }
        Err(error) => {
            writeln!(
                err,
                "Region writer parity fixture generation failed: {error}"
            )?;
            Ok(EXIT_USAGE)
        }
    }
}

fn write_region_writer_parity_fixtures_impl(output_dir: &str) -> earthmap_region::Result<()> {
    let output_dir = Path::new(output_dir);
    let payloads = region_writer_fixture_payloads()?;
    earthmap_region::write_mca_region(
        output_dir.join("writer-mca").join("r.0.0.mca"),
        &payloads,
        42,
    )?;
    earthmap_region::write_linear_v2_region(
        output_dir.join("writer-linear").join("r.-2.3.linear"),
        &payloads,
        42,
    )?;
    Ok(())
}

fn region_writer_fixture_payloads() -> earthmap_region::Result<BTreeMap<ChunkLocalPos, Vec<u8>>> {
    let chunk_fixtures = earthmap_minecraft::nbt_parity_fixtures::chunk_payload_fixtures()
        .map_err(|error| RegionError::Invalid(error.to_string()))?;
    let mut payloads = BTreeMap::new();
    for (index, (_name, bytes)) in chunk_fixtures.into_iter().enumerate() {
        let pos = ChunkLocalPos::new(
            u8::try_from(index).expect("fixture index fits in local chunk x"),
            0,
        )?;
        payloads.insert(pos, bytes);
    }
    Ok(payloads)
}

#[cfg(test)]
mod tests {
    use super::*;
    use earthmap_geo::RgbColor;
    use earthmap_minecraft::nbt;
    use earthmap_surface::TerrainTokenSource;
    use std::fs;

    use tempfile::tempdir;

    fn run_capture(args: &[&str]) -> (i32, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_with_writers(args.iter().copied(), &mut out, &mut err);
        (
            code,
            String::from_utf8(out).expect("stdout must be UTF-8"),
            String::from_utf8(err).expect("stderr must be UTF-8"),
        )
    }

    fn read_png_rgb(path: &Path) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(fs::File::open(path).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buffer).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        (
            info.width,
            info.height,
            buffer[..info.buffer_size()].to_vec(),
        )
    }

    #[test]
    fn surface_photo_worker_candidates_keep_legacy_and_requested_options() {
        assert_eq!(surface_photo_worker_candidates(10, 100), vec![4]);
        assert_eq!(surface_photo_worker_candidates(3, 100), vec![3]);
        assert_eq!(surface_photo_worker_candidates(10, 2), vec![2]);
    }

    fn surface_photo_candidate_result(
        worker_count: usize,
        rayon_threads: usize,
        parallel_column_sampling: bool,
        elapsed_millis: u128,
    ) -> SurfacePhotoWorkerCandidateResult {
        SurfacePhotoWorkerCandidateResult {
            worker_count,
            rayon_threads,
            parallel_column_sampling,
            elapsed_millis,
            sample_count: 10,
        }
    }

    #[test]
    fn surface_photo_worker_candidate_configs_compare_legacy_and_requested_parallel_workers() {
        let configs = surface_photo_worker_candidate_configs(10, 100);
        let tuned_rayon = configured_surface_photo_rayon_thread_count(10).max(4);
        assert!(configs.contains(&SurfacePhotoWorkerCandidateConfig::new(
            4,
            tuned_rayon,
            surface_photo_parallel_column_sampling(4, tuned_rayon)
        )));
        assert_eq!(configs.len(), 1);
    }

    #[test]
    fn surface_photo_parallel_column_sampling_uses_rayon_when_available() {
        assert!(surface_photo_parallel_column_sampling(1, 2));
        assert!(!surface_photo_parallel_column_sampling(1, 1));
        assert!(surface_photo_parallel_column_sampling(2, 16));
        assert!(surface_photo_parallel_column_sampling(8, 16));
    }

    #[test]
    fn prefetch_rayon_split_keeps_sample_pool_wide_for_photo_regions() {
        assert_eq!(split_prefetch_rayon_threads(16, 2, 4), (13, 3));
        assert_eq!(split_prefetch_rayon_threads(8, 2, 4), (6, 2));
        assert_eq!(split_prefetch_rayon_threads(2, 1, 4), (1, 1));
    }

    #[test]
    fn open_ocean_prefetch_output_pool_can_use_more_threads_than_land_output() {
        assert_eq!(open_ocean_prefetch_output_rayon_threads(16, 3, 4), 6);
        assert_eq!(open_ocean_prefetch_output_rayon_threads(8, 2, 4), 3);
        assert_eq!(open_ocean_prefetch_output_rayon_threads(2, 1, 4), 1);
    }

    #[test]
    fn surface_photo_worker_selection_chooses_lower_worker_inside_noise_band() {
        let candidates = vec![
            surface_photo_candidate_result(4, 16, true, 1_000),
            surface_photo_candidate_result(8, 16, true, 970),
        ];
        assert_eq!(
            select_surface_photo_worker_candidate(&candidates, 1.05),
            Some(SurfacePhotoWorkerCandidateConfig::new(4, 16, true))
        );
    }

    #[test]
    fn surface_photo_worker_selection_takes_clear_speedup() {
        let candidates = vec![
            surface_photo_candidate_result(4, 16, true, 1_000),
            surface_photo_candidate_result(8, 16, true, 800),
        ];
        assert_eq!(
            select_surface_photo_worker_candidate(&candidates, 1.05),
            Some(SurfacePhotoWorkerCandidateConfig::new(8, 16, true))
        );
    }

    fn generate_single_chunk_render_world(world_dir: &Path, format: TopdownFormat) {
        let region_dir = world_dir.join("region");
        fs::create_dir_all(&region_dir).unwrap();
        let settings = level_dat_template::Settings::new(
            "topdown-single-chunk",
            0,
            8,
            FLAT_TEST_SURFACE_Y + 2,
            8,
        )
        .unwrap();
        level_dat_template::write(world_dir.join("level.dat"), &settings).unwrap();

        let mut payloads = BTreeMap::new();
        let chunk = flat_test_chunk(0, 0).unwrap();
        let bytes = chunk_nbt_encoder::encode_to_bytes(&chunk, 0).unwrap();
        payloads.insert(ChunkLocalPos::new(0, 0).unwrap(), bytes);

        match format {
            TopdownFormat::Mca => {
                earthmap_region::write_mca_region(&region_dir.join("r.0.0.mca"), &payloads, 0)
                    .unwrap();
            }
            TopdownFormat::Linear => {
                earthmap_region::write_linear_v2_region(
                    &region_dir.join("r.0.0.linear"),
                    &payloads,
                    0,
                )
                .unwrap();
            }
        }
    }

    fn write_solid_png_tile(path: &Path, color: [u8; 3]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut pixels = vec![0u8; 2 * 2 * 3];
        for pixel in pixels.chunks_exact_mut(3) {
            pixel.copy_from_slice(&color);
        }
        write_rgb_png(path, 2, 2, &pixels).unwrap();
    }

    fn write_metric_test_png(path: &Path, inverted: bool) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut pixels = vec![0u8; 4 * 4 * 3];
        for y in 0..4 {
            for x in 0..4 {
                let mut bright = (x + y) % 2 == 0;
                if inverted {
                    bright = !bright;
                }
                let value = if bright { 255 } else { 0 };
                let offset = ((y * 4 + x) * 3) as usize;
                pixels[offset..offset + 3].copy_from_slice(&[value, value, value]);
            }
        }
        write_rgb_png(path, 4, 4, &pixels).unwrap();
    }

    fn assert_png_pixel(pixels: &[u8], width: u32, x: u32, y: u32, color: [u8; 3]) {
        let offset = ((usize::try_from(y).unwrap() * usize::try_from(width).unwrap())
            + usize::try_from(x).unwrap())
            * 3;
        assert_eq!(&pixels[offset..offset + 3], &color);
    }

    fn write_post_final_integrity_fixture_region(region: &Path, format: RegionFormat) {
        fs::create_dir_all(region.parent().unwrap()).unwrap();
        let mut chunk = ChunkModel::overworld(0, 0);
        for z in 0..CHUNK_WIDTH {
            for x in 0..CHUNK_WIDTH {
                let x = x as i32;
                let z = z as i32;
                chunk
                    .set_block_state_id(x, -64, z, block_state_ids::BEDROCK)
                    .unwrap();
                chunk
                    .fill_column(x, z, -63, 58, block_state_ids::STONE)
                    .unwrap();
                chunk
                    .fill_column(x, z, 59, SEA_LEVEL_Y, block_state_ids::WATER)
                    .unwrap();
            }
        }
        chunk
            .set_block_state_id(0, SEA_LEVEL_Y - 1, 0, block_state_ids::AIR)
            .unwrap();
        chunk
            .set_block_state_id(0, SEA_LEVEL_Y - 2, 0, block_state_ids::AIR)
            .unwrap();
        chunk
            .set_block_state_id(4, 40, 4, block_state_ids::AIR)
            .unwrap();
        chunk
            .fill_column(1, 0, 59, 74, block_state_ids::DIRT)
            .unwrap();
        chunk
            .set_block_state_id(1, 75, 0, block_state_ids::GRASS_BLOCK)
            .unwrap();
        chunk
            .set_block_state_id(2, 76, 0, block_state_ids::OAK_LOG)
            .unwrap();
        chunk
            .set_block_state_id(2, 77, 0, block_state_ids::OAK_LEAVES)
            .unwrap();

        let mut payloads = BTreeMap::new();
        let payload = chunk_nbt_encoder::encode_to_bytes(&chunk, 0).unwrap();
        payloads.insert(ChunkLocalPos::new(0, 0).unwrap(), payload);
        match format {
            RegionFormat::Mca => earthmap_region::write_mca_region(region, &payloads, 0).unwrap(),
            RegionFormat::Linear => {
                earthmap_region::write_linear_v2_region(region, &payloads, 0).unwrap()
            }
        }
    }

    fn write_linear_sandlike_repair_fixture(region: &Path) {
        fs::create_dir_all(region.parent().unwrap()).unwrap();
        let mut chunk = ChunkModel::overworld(0, 0);
        fill_flat_base_terrain(&mut chunk, FLAT_TEST_SURFACE_Y).unwrap();
        chunk
            .set_block_state_id(0, SEA_LEVEL_Y, 0, block_state_ids::SAND)
            .unwrap();
        chunk
            .set_block_state_id(1, SEA_LEVEL_Y + 7, 0, block_state_ids::SAND)
            .unwrap();
        chunk
            .set_block_state_id(2, SEA_LEVEL_Y + 7, 0, block_state_ids::SAND)
            .unwrap();
        chunk
            .set_block_state_id(2, SEA_LEVEL_Y + 8, 0, block_state_ids::STONE)
            .unwrap();

        let mut payloads = BTreeMap::new();
        let payload = chunk_nbt_encoder::encode_to_bytes(&chunk, 0).unwrap();
        payloads.insert(ChunkLocalPos::new(0, 0).unwrap(), payload);
        earthmap_region::write_linear_v2_region(region, &payloads, 0).unwrap();
    }

    #[test]
    fn resume_journal_loads_completed_regions_for_matching_fingerprint() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("world");
        let fingerprint = json!({"schemaVersion": 1, "test": "same"});

        {
            let prepared = prepare_vanilla_delegated_resume_journal(&world, &fingerprint).unwrap();
            assert!(!prepared.fingerprint_matched);
            assert!(prepared.completed_regions.is_empty());
            prepared
                .journal
                .lock()
                .unwrap()
                .append_region_complete(1, 2, OutputFormat::LinearV2, 1024, 123)
                .unwrap();
            prepared
                .journal
                .lock()
                .unwrap()
                .append_region_complete(-3, 4, OutputFormat::LinearV2, 1024, 456)
                .unwrap();
            prepared.journal.lock().unwrap().sync().unwrap();
        }

        let prepared = prepare_vanilla_delegated_resume_journal(&world, &fingerprint).unwrap();
        assert!(prepared.fingerprint_matched);
        assert_eq!(prepared.completed_regions.len(), 2);
        assert!(prepared.completed_regions.contains(&(1, 2)));
        assert!(prepared.completed_regions.contains(&(-3, 4)));
    }

    #[test]
    fn resume_journal_resets_completed_regions_for_mismatched_fingerprint() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("world");
        let old_fingerprint = json!({"schemaVersion": 1, "test": "old"});
        let new_fingerprint = json!({"schemaVersion": 1, "test": "new"});

        {
            let prepared =
                prepare_vanilla_delegated_resume_journal(&world, &old_fingerprint).unwrap();
            prepared
                .journal
                .lock()
                .unwrap()
                .append_region_complete(1, 2, OutputFormat::LinearV2, 1024, 123)
                .unwrap();
            prepared.journal.lock().unwrap().sync().unwrap();
        }

        let prepared = prepare_vanilla_delegated_resume_journal(&world, &new_fingerprint).unwrap();
        assert!(!prepared.fingerprint_matched);
        assert!(prepared.completed_regions.is_empty());
        let journal =
            fs::read_to_string(world.join(VANILLA_DELEGATED_RESUME_JOURNAL_FILE_NAME)).unwrap();
        assert!(!journal.contains("regionComplete"));
        assert!(journal.contains("\"test\":\"new\""));
    }

    #[test]
    fn resume_fingerprint_includes_surface_companion_raster_identities() {
        let temp = tempdir().unwrap();
        let tif_root = temp.path().join("TifFiles");
        let terrain = tif_root.join("terrain");
        let vegetation = tif_root.join("vegetation");
        fs::create_dir_all(&terrain).unwrap();
        fs::create_dir_all(&vegetation).unwrap();
        let heightmap = temp.path().join("height.tif");
        let true_marble = terrain.join("TrueMarble.vrt");
        let climate = tif_root.join("climate.tif");
        fs::write(&heightmap, b"height").unwrap();
        fs::write(&true_marble, b"vrt").unwrap();
        fs::write(&climate, b"climate-v1").unwrap();

        let first = vanilla_delegated_parallel_resume_fingerprint(
            &heightmap,
            OutputFormat::LinearV2,
            1000,
            -40,
            -20,
            80,
            40,
            ChunkGenerationStatus::Surface,
            &true_marble,
            RegionCompressionOptions {
                linear_compression_level: Some(4),
                ..RegionCompressionOptions::default()
            },
            DEFAULT_VERTICAL_SCALE,
            "auto",
        );
        fs::write(&climate, b"climate-v2-with-different-length").unwrap();
        let second = vanilla_delegated_parallel_resume_fingerprint(
            &heightmap,
            OutputFormat::LinearV2,
            1000,
            -40,
            -20,
            80,
            40,
            ChunkGenerationStatus::Surface,
            &true_marble,
            RegionCompressionOptions {
                linear_compression_level: Some(4),
                ..RegionCompressionOptions::default()
            },
            DEFAULT_VERTICAL_SCALE,
            "auto",
        );

        assert_ne!(first, second);
        assert!(first.to_string().contains("tifRoot.climate"));
        assert_eq!(
            first["surfaceSamplingProfile"].as_str(),
            Some(SURFACE_SAMPLING_PROFILE_VERSION)
        );
    }

    #[test]
    fn resume_fingerprint_ignores_derived_ecoregion_cache_identity() {
        let temp = tempdir().unwrap();
        let tif_root = temp.path().join("TifFiles");
        let terrain = tif_root.join("terrain");
        let shape_dir = temp.path().join("ShapeFiles").join("ecoregionsOrig");
        let cache_dir = temp.path().join(".earthmap-cache");
        fs::create_dir_all(&terrain).unwrap();
        fs::create_dir_all(&shape_dir).unwrap();
        fs::create_dir_all(&cache_dir).unwrap();
        let heightmap = temp.path().join("height.tif");
        let true_marble = terrain.join("TrueMarble.vrt");
        fs::write(&heightmap, b"height").unwrap();
        fs::write(&true_marble, b"vrt").unwrap();
        fs::write(shape_dir.join("wwf_terr_ecos.shp"), b"shape").unwrap();
        fs::write(shape_dir.join("wwf_terr_ecos.dbf"), b"dbf").unwrap();
        fs::write(temp.path().join("ecoregions.csv"), b"name,biome\n").unwrap();

        let first = vanilla_delegated_parallel_resume_fingerprint(
            &heightmap,
            OutputFormat::LinearV2,
            1000,
            -40,
            -20,
            80,
            40,
            ChunkGenerationStatus::Surface,
            &true_marble,
            RegionCompressionOptions::default(),
            DEFAULT_VERTICAL_SCALE,
            "auto",
        );
        fs::write(cache_dir.join("wwf-ecoregions-v2.bin"), b"derived-cache").unwrap();
        let second = vanilla_delegated_parallel_resume_fingerprint(
            &heightmap,
            OutputFormat::LinearV2,
            1000,
            -40,
            -20,
            80,
            40,
            ChunkGenerationStatus::Surface,
            &true_marble,
            RegionCompressionOptions::default(),
            DEFAULT_VERTICAL_SCALE,
            "auto",
        );

        assert_eq!(first, second);
        assert!(!first.to_string().contains("ecoregions.cache"));
    }

    #[test]
    fn resume_fingerprint_ignores_runtime_directory_mtime_changes() {
        let temp = tempdir().unwrap();
        let tif_root = temp.path().join("TifFiles");
        let terrain = tif_root.join("terrain");
        fs::create_dir_all(&terrain).unwrap();
        let heightmap = temp.path().join("height.tif");
        let true_marble = terrain.join("TrueMarble.vrt");
        fs::write(&heightmap, b"height").unwrap();
        fs::write(&true_marble, b"vrt").unwrap();

        let first = vanilla_delegated_parallel_resume_fingerprint(
            &heightmap,
            OutputFormat::LinearV2,
            250,
            -157,
            -74,
            314,
            148,
            ChunkGenerationStatus::Surface,
            &true_marble,
            RegionCompressionOptions {
                linear_compression_level: Some(6),
                ..RegionCompressionOptions::default()
            },
            4.0,
            "auto",
        );
        fs::write(temp.path().join("runtime.log"), b"operator-side log").unwrap();
        let second = vanilla_delegated_parallel_resume_fingerprint(
            &heightmap,
            OutputFormat::LinearV2,
            250,
            -157,
            -74,
            314,
            148,
            ChunkGenerationStatus::Surface,
            &true_marble,
            RegionCompressionOptions {
                linear_compression_level: Some(6),
                ..RegionCompressionOptions::default()
            },
            4.0,
            "auto",
        );

        assert_eq!(first, second);
        assert!(first.to_string().contains("\"kind\":\"directory\""));
    }

    #[test]
    fn vanilla_delegated_manifests_record_surface_sampling_profile() {
        let temp = tempdir().unwrap();
        let material = temp.path().join("TrueMarble.vrt");
        let heightmap = temp.path().join("heightmap.tif");
        fs::write(&material, b"vrt").unwrap();
        fs::write(&heightmap, b"heightmap").unwrap();

        let parallel_manifest = write_vanilla_delegated_parallel_manifest(
            &temp.path().join("parallel"),
            &heightmap,
            OutputFormat::LinearV2,
            200,
            1,
            2,
            3,
            4,
            10,
            ChunkGenerationStatus::Surface,
            &material,
            RegionCompressionOptions {
                linear_compression_level: Some(6),
                ..RegionCompressionOptions::default()
            },
            4.0,
            "auto",
        )
        .unwrap();
        let plan_manifest = write_vanilla_delegated_plan_manifest(
            &temp.path().join("plan"),
            &heightmap,
            OutputFormat::LinearV2,
            200,
            &temp.path().join("plan.csv"),
            12,
            10,
            10,
            ChunkGenerationStatus::Surface,
            &material,
            RegionCompressionOptions {
                linear_compression_level: Some(6),
                ..RegionCompressionOptions::default()
            },
            4.0,
            "auto",
        )
        .unwrap();

        for manifest in [parallel_manifest, plan_manifest] {
            let text = fs::read_to_string(manifest).unwrap();
            assert!(text.contains(&format!(
                "generation.surfaceSamplingProfile={SURFACE_SAMPLING_PROFILE_VERSION}\n"
            )));
            assert!(text.contains("generation.heightmapPath="));
            assert!(text.contains("generation.threads=10\n"));
            assert!(text.contains("generation.linearCompressionLevel=6\n"));
        }
    }

    #[test]
    fn skipped_event_writes_json_event_and_legacy_csv_line() {
        let temp = tempdir().unwrap();
        let region_file = temp.path().join("region").join("r.1.2.linear");
        let mut out = Vec::new();
        let mut stats = VanillaDelegatedParallelBatchStats::default();

        handle_vanilla_delegated_parallel_event(
            &mut out,
            &mut stats,
            VanillaDelegatedParallelEvent::RegionSkipped {
                region_x: 1,
                region_z: 2,
                elapsed_millis: 7,
                chunks: 1024,
                output_bytes: 2048,
                region_file: region_file.clone(),
            },
        )
        .unwrap();

        let out = String::from_utf8(out).unwrap();
        let mut lines = out.lines();
        let event_line = lines.next().unwrap();
        let event_json = event_line.strip_prefix("event\t").unwrap();
        let event = serde_json::from_str::<Value>(event_json).unwrap();
        assert_eq!(event["type"], "regionSkipped");
        assert_eq!(event["regionX"], 1);
        assert_eq!(event["regionZ"], 2);
        assert_eq!(stats.skipped_regions, 1);
        assert_eq!(
            lines.next().unwrap(),
            format!(
                "region,skipped,1,2,7,1024,2048,{},validResume",
                region_file.display()
            )
        );
    }

    #[test]
    fn generated_event_includes_per_region_phase_telemetry() {
        let temp = tempdir().unwrap();
        let region_file = temp.path().join("region").join("r.3.4.linear");
        let mut out = Vec::new();
        let mut stats = VanillaDelegatedParallelBatchStats::default();
        let report = SurfaceRegionReport {
            region_x: 3,
            region_z: 4,
            output_format: OutputFormat::LinearV2,
            scale_denominator: 250,
            vertical_scale: 4.0,
            chunk_count: 1024,
            land_columns: 0,
            water_columns: 262_144,
            min_ground_y: 12,
            max_ground_y: 63,
            region_file: region_file.clone(),
            preview_tile_file: None,
            cache_stats: earthmap_geo::GeoTiffRowCacheStats {
                max_rows: 0,
                resident_rows: 0,
                hits: 0,
                misses: 0,
                evictions: 0,
                prefetch_rows: 0,
                prefetch_requests: 0,
                prefetch_loads: 0,
            },
            surface_material_raster_stats: earthmap_surface::SurfaceMaterialRasterStats::EMPTY,
            surface_sample_nanos: 4_285_000_000,
            sample_phase_nanos: earthmap_surface::SurfaceRegionSamplePhaseNanos {
                elevation_fill: 1_000_000_000,
                water_mask: 200_000_000,
                coast_factor: 30_000_000,
                smooth_precompute: 40_000_000,
                relief_precompute: 50_000_000,
                open_ocean_fast_path: 60_000_000,
                open_ocean_uniform_check: 61_000_000,
                open_ocean_companion_precompute: 62_000_000,
                open_ocean_column_build: 63_000_000,
                open_ocean_repeated_expand: 64_000_000,
                coordinate_precompute: 70_000_000,
                photo_land_precompute: 75_000_000,
                column_build: 700_000_000,
                photo_profile: 80_000_000,
                photo_apply: 900_000_000,
                post_process: 100_000_000,
                sampled_material_columns: 262_144,
                sampled_land_material_columns: 0,
                sampled_water_material_columns: 262_144,
            },
            chunk_build_nanos: 277_000_000,
            nbt_encode_nanos: 3_757_000_000,
            region_write_nanos: 76_000_000,
            preview_nanos: 0,
            metadata_nanos: 5_000_000,
            total_nanos: 4_618_000_000,
        };

        handle_vanilla_delegated_parallel_event(
            &mut out,
            &mut stats,
            VanillaDelegatedParallelEvent::RegionGenerated {
                elapsed_millis: 4_629,
                output_bytes: 37_083,
                report,
                prefetch_timing: None,
            },
        )
        .unwrap();

        let out = String::from_utf8(out).unwrap();
        let event_line = out.lines().next().unwrap();
        let event_json = event_line.strip_prefix("event\t").unwrap();
        let event = serde_json::from_str::<Value>(event_json).unwrap();
        assert_eq!(event["type"], "regionGenerated");
        assert_eq!(event["surfaceSampleMillis"], 4_285);
        assert_eq!(event["surfacePhase.elevationFillMillis"], 1_000);
        assert_eq!(event["surfacePhase.waterMaskMillis"], 200);
        assert_eq!(event["surfacePhase.coastFactorMillis"], 30);
        assert_eq!(event["surfacePhase.smoothPrecomputeMillis"], 40);
        assert_eq!(event["surfacePhase.reliefPrecomputeMillis"], 50);
        assert_eq!(event["surfacePhase.openOceanFastPathMillis"], 60);
        assert_eq!(event["surfacePhase.openOceanUniformCheckMillis"], 61);
        assert_eq!(event["surfacePhase.openOceanCompanionPrecomputeMillis"], 62);
        assert_eq!(event["surfacePhase.openOceanColumnBuildMillis"], 63);
        assert_eq!(event["surfacePhase.openOceanRepeatedExpandMillis"], 64);
        assert_eq!(event["surfacePhase.coordinatePrecomputeMillis"], 70);
        assert_eq!(event["surfacePhase.photoLandPrecomputeMillis"], 75);
        assert_eq!(event["surfacePhase.columnBuildMillis"], 700);
        assert_eq!(event["surfacePhase.photoProfileMillis"], 80);
        assert_eq!(event["surfacePhase.photoApplyMillis"], 900);
        assert_eq!(event["surfacePhase.postProcessMillis"], 100);
        assert_eq!(event["surfacePhase.sampledMaterialColumns"], 262_144);
        assert_eq!(event["surfacePhase.sampledLandMaterialColumns"], 0);
        assert_eq!(event["surfacePhase.sampledWaterMaterialColumns"], 262_144);
        assert_eq!(event["chunkBuildMillis"], 277);
        assert_eq!(event["nbtEncodeMillis"], 3_757);
        assert_eq!(event["regionWriteMillis"], 76);
        assert_eq!(event["metadataMillis"], 5);
        assert_eq!(event["totalInternalMillis"], 4_618);
        assert_eq!(event["waterColumns"], 262_144);
        assert_eq!(stats.generated_regions, 1);
    }

    #[test]
    fn version_matches_java_build_identity() {
        let (code, out, err) = run_capture(&["--version"]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Super-Rapid-EarthMap-Generator 0.0.0-phase0"));
        assert!(out.contains("Minecraft target: Java Edition 1.21.11"));
        assert!(out.contains("Gameplay profile: survival-complete"));
        assert!(out.contains("Server profile: nation-war"));
    }

    #[test]
    fn doctor_reports_progress_header_contract() {
        let (code, out, err) = run_capture(&["doctor"]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Progress file: earthmap-region-progress.csv"));
        assert!(out.contains(
            "timestamp,status,regionX,regionZ,format,elapsedMillis,chunks,outputBytes,regionFile,message"
        ));
    }

    #[test]
    fn capabilities_marks_vanilla_delegated_region_as_probe_only() {
        let (code, out, err) = run_capture(&["capabilities"]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains(
            "WIP rust.command.generate-vanilla-delegated-region - single vanilla-delegated region parity probe; payload parity is not green"
        ));
        assert!(!out
            .lines()
            .any(|line| line.starts_with("DONE rust.command.generate-vanilla-delegated-region -")));
        assert!(out.contains(
            "DONE rust.command.generate-vanilla-delegated-regions-parallel - parallel vanilla-delegated region generation"
        ));
        assert!(out.contains(
            "DONE rust.command.generate - production alias for vanilla-delegated parallel generation"
        ));
        assert!(out.contains(
            "DONE rust.command.generate-vanilla-delegated-plan-parallel - bounded/resumable vanilla-delegated plan generation"
        ));
        assert!(out.contains(
            "DONE rust.command.generate-vanilla-delegated-region-plan-parallel - compatibility alias for vanilla-delegated plan generation"
        ));
        assert!(out.contains(
            "DONE rust.command.generate-survival-region - Rust vanilla-delegated survival compatibility alias"
        ));
        assert!(out.contains(
            "DONE rust.command.generate-survival-regions-parallel - Rust vanilla-delegated survival parallel compatibility alias"
        ));
        assert!(out.contains(
            "DONE rust.command.generate-survival-region-osm-pbf - Rust survival OSM PBF compatibility alias"
        ));
        assert!(out.contains(
            "DONE rust.command.generate-survival-region-osm-pbf-ref-window - Rust survival OSM PBF ref-window compatibility alias"
        ));
        assert!(out.contains(
            "DONE rust.command.generate-survival-region-osm-pbf-full-scan - Rust survival OSM PBF full-scan compatibility alias"
        ));
        assert!(out.contains(
            "DONE rust.command.generate-survival-region-osm-xml-cache - Rust survival OSM XML cache compatibility alias"
        ));
        assert!(out.contains(
            "DONE rust.command.plan-representative-regions - deterministic representative region planner"
        ));
        assert!(out.contains(
            "DONE rust.command.describe-earth-grid - Earth grid and full-map region bounds reporter"
        ));
        assert!(out.contains(
            "DONE rust.command.validate-surface-spawn - heightmap-backed surface spawn viability validator"
        ));
        assert!(out.contains(
            "DONE rust.command.validate-height-seam - heightmap-backed adjacent region seam validator"
        ));
        assert!(out.contains(
            "DONE rust.command.write-vanilla-finalization-commands - server force-load command writer for vanilla finalization"
        ));
        assert!(out.contains(
            "DONE rust.command.validate-survival-manifest - survival manifest gate validator"
        ));
        assert!(out.contains(
            "DONE rust.command.apply-survival-evidence - survival evidence manifest updater"
        ));
        assert!(out.contains(
            "DONE rust.command.validate-cave-density - deterministic cave density validator"
        ));
        assert!(out.contains(
            "DONE rust.command.validate-cave-connectivity - deterministic cave connectivity validator"
        ));
        assert!(out.contains(
            "DONE rust.command.validate-ore-histogram-synthetic - synthetic ore histogram validator"
        ));
        assert!(out.contains(
            "DONE rust.command.validate-underground-fluid-synthetic - synthetic underground fluid validator"
        ));
        assert!(out.contains(
            "DONE rust.command.validate-global-resource-fairness - Linear world global resource fairness validator"
        ));
        assert!(out.contains(
            "DONE rust.command.validate-loot-economy - Linear world loot economy validator"
        ));
        assert!(out.contains(
            "DONE rust.command.generate-nation-war-readiness-report - nation-war readiness report generator"
        ));
        assert!(out.contains(
            "DONE rust.command.benchmark-height-regions - Rust height region generation benchmark"
        ));
        assert!(out.contains(
            "DONE rust.command.benchmark-surface-regions - Rust surface region generation benchmark"
        ));
        assert!(out.contains(
            "DONE rust.command.benchmark-survival-regions - Rust delegated survival compatibility benchmark"
        ));
        assert!(out.contains(
            "DONE rust.command.benchmark-survival-regions-parallel - Rust delegated survival parallel benchmark"
        ));
        assert!(out.contains(
            "DONE rust.command.benchmark-region-writers - Rust MCA/Linear region writer benchmark"
        ));
        assert!(out.contains("DONE rust.command.scan-osm-pbf - OSM PBF prefix scanner"));
        assert!(
            out.contains("DONE rust.command.scan-osm-pbf-range - OSM PBF ranged prefix scanner")
        );
        assert!(out.contains(
            "DONE rust.command.validate-osm-pbf - OSM PBF scanned-prefix integrity validator"
        ));
        assert!(out.contains(
            "DONE rust.command.benchmark-osm-index - synthetic OSM region index benchmark"
        ));
        assert!(out.contains(
            "DONE rust.command.extract-osm-region-mask - OSM PBF region feature mask extractor"
        ));
        assert!(out.contains(
            "DONE rust.command.extract-osm-region-mask-window - bounded OSM PBF node/way window mask extractor"
        ));
        assert!(out.contains(
            "DONE rust.command.extract-osm-region-mask-ref-window - bounded OSM PBF way-reference mask extractor"
        ));
        assert!(out.contains(
            "DONE rust.command.extract-osm-region-mask-full-scan - OSM PBF full-scan region feature mask extractor"
        ));
        assert!(out.contains(
            "DONE rust.command.extract-osm-xml-region-mask - OSM XML cache region feature mask extractor"
        ));
        assert!(out.contains(
            "DONE rust.command.identify-osm-xml-cache - OSM XML cache source identity reporter"
        ));
    }

    #[test]
    fn capabilities_marks_basic_region_validation_commands_done() {
        let (code, out, err) = run_capture(&["capabilities"]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("DONE rust.command.validate-mca-region - MCA region validator"));
        assert!(
            out.contains("DONE rust.command.validate-linear-region - Linear V2 region validator")
        );
        assert!(out.contains(
            "DONE rust.command.compare-mca-linear-region-payloads - MCA/Linear payload comparator"
        ));
        assert!(out.contains(
            "DONE rust.command.convert-mca-region-to-linear - single MCA region to Linear V2 converter"
        ));
        assert!(out.contains(
            "DONE rust.command.convert-mca-world-to-linear - MCA world to Linear V2 converter"
        ));
        assert!(out.contains("DONE rust.command.inspect-mca-palettes - MCA block palette scanner"));
        assert!(out.contains(
            "DONE rust.command.validate-mca-survival-palette - MCA survival palette validator"
        ));
        assert!(out
            .contains("DONE rust.command.inspect-linear-palettes - Linear block palette scanner"));
        assert!(out.contains(
            "DONE rust.command.validate-linear-survival-palette - Linear survival palette validator"
        ));
        assert!(out.contains("DONE rust.command.inspect-mca-biomes - MCA biome palette scanner"));
        assert!(
            out.contains("DONE rust.command.inspect-linear-biomes - Linear biome palette scanner")
        );
        assert!(out.contains("DONE rust.command.inspect-mca-statuses - MCA chunk status scanner"));
        assert!(
            out.contains("DONE rust.command.inspect-linear-statuses - Linear chunk status scanner")
        );
        assert!(out.contains("DONE rust.command.mca-topdown-render - MCA top-down render"));
        assert!(out.contains("DONE rust.command.linear-topdown-render - Linear V2 top-down render"));
        assert!(out.contains("DONE rust.command.dynmap-tile-mosaic - Dynmap tile mosaic builder"));
        assert!(out.contains("DONE rust.command.photo-compare-crop - PNG crop metric comparator"));
        assert!(out
            .contains("DONE rust.command.photo-parity-crop - PNG crop parity artifact generator"));
        assert!(out.contains(
            "DONE rust.command.photo-parity-metric-crop - PNG crop photo metric reporter"
        ));
        assert!(out.contains(
            "DONE rust.command.photo-parity-metric-batch - parallel PNG crop photo metric batch"
        ));
        assert!(out.contains(
            "DONE rust.command.quality-production-sample-batch - Rust quality gate production sample batch"
        ));
        assert!(out.contains(
            "DONE rust.command.photo-standard-remap-parity-crop - Rust Standard palette remap parity crop"
        ));
        assert!(out.contains(
            "DONE rust.command.photo-standard-remap-parity-batch - parallel Rust Standard palette remap parity batch"
        ));
        assert!(out.contains(
            "DONE rust.command.photo-production-candidate-diff-crop - PNG production candidate diff reporter"
        ));
        assert!(out.contains(
            "DONE rust.command.photo-carrier-remap-sim-crop - PNG carrier remap simulation reporter"
        ));
        assert!(out.contains(
            "DONE rust.command.inspect-mca-post-final-integrity - MCA post-final integrity scanner"
        ));
        assert!(out.contains("DONE rust.command.rewrite-mca-status - MCA chunk status rewriter"));
        assert!(out.contains(
            "DONE rust.command.repair-mca-post-final-water - MCA post-final underwater air repairer"
        ));
        assert!(out.contains(
            "DONE rust.command.inspect-linear-post-final-integrity - Linear post-final integrity scanner"
        ));
        assert!(out.contains(
            "DONE rust.command.repair-linear-sandlike-surfaces - Linear sandlike surface repairer"
        ));
    }

    #[test]
    fn vanilla_delegated_region_single_optional_raster_defaults_status_like_java() {
        let (code, out, err) = run_capture(&[
            "generate-vanilla-delegated-region",
            "missing-heightmap.tif",
            "world",
            "5000",
            "0",
            "0",
            "linear",
            "surfaceRaster=none",
        ]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert!(err.contains("default textureMode=photo requires a TrueMarble surface raster"));
        assert!(!err.contains("chunk generation status"));
    }

    #[test]
    fn generate_alias_dispatches_to_vanilla_delegated_parallel() {
        let (code, out, err) = run_capture(&[
            "generate",
            "missing-heightmap.tif",
            "world",
            "1000",
            "0",
            "0",
            "1",
            "1",
            "linear",
            "1",
            "surfaceRaster=none",
        ]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert!(err.contains("default textureMode=photo requires a TrueMarble surface raster"));
        assert!(!err.contains("not implemented yet"));
    }

    #[test]
    fn region_plan_csv_accepts_coordinate_and_indexed_shapes() {
        let temp = tempdir().unwrap();
        let plan = temp.path().join("plan.csv");
        fs::write(
            &plan,
            "# comment\nregionX,regionZ,label\n1,-2,alpha\nindex,regionX,regionZ,label\n0,3,4,beta\n",
        )
        .unwrap();

        let regions = read_region_plan_csv(&plan).unwrap();

        assert_eq!(regions, vec![(1, -2), (3, 4)]);
        assert_eq!(plan_spawn_region(&regions), (2, 1));
        assert_eq!(
            parse_plan_max_regions_this_run(&["2".to_string(), "surface".to_string()]).unwrap(),
            (2, 1)
        );
        assert_eq!(
            parse_plan_max_regions_this_run(&["surface".to_string()]).unwrap(),
            (usize::MAX, 0)
        );
    }

    #[test]
    fn vanilla_delegated_plan_parallel_dispatches_without_java() {
        let temp = tempdir().unwrap();
        let plan = temp.path().join("plan.csv");
        fs::write(&plan, "regionX,regionZ\n0,0\n").unwrap();

        let (code, out, err) = run_capture(&[
            "generate-vanilla-delegated-region-plan-parallel",
            "missing-heightmap.tif",
            "world",
            "1000",
            "linear",
            "1",
            plan.to_str().unwrap(),
            "1",
            "surfaceRaster=none",
        ]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert!(err.contains("default textureMode=photo requires a TrueMarble surface raster"));
        assert!(!err.contains("not implemented yet"));
    }

    #[test]
    fn survival_generation_aliases_dispatch_without_java() {
        let temp = tempdir().unwrap();
        let pbf = temp.path().join("tiny.osm.pbf");
        fs::write(&pbf, synthetic_osm_pbf()).unwrap();
        let osm_dir = temp.path().join("osm-cache");
        fs::create_dir_all(&osm_dir).unwrap();
        fs::write(
            osm_dir.join("a.osm"),
            r#"<osm>
  <node id="1" lat="0.0" lon="0.0"/>
  <node id="2" lat="0.0" lon="0.1"/>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="highway" v="residential"/></way>
</osm>"#,
        )
        .unwrap();

        let (code, out, err) = run_capture(&[
            "generate-survival-region",
            "missing-heightmap.tif",
            "world",
            "1000",
            "0",
            "0",
            "bad-format",
        ]);
        assert_eq!(code, EXIT_USAGE);
        assert!(out.contains("legacy survival direct generation is replaced"));
        assert!(err.contains("format must be mca or linear"));
        assert!(!err.contains("not implemented yet"));

        let (code, out, err) = run_capture(&[
            "generate-survival-regions-parallel",
            "missing-heightmap.tif",
            "world",
            "1000",
            "0",
            "0",
            "1",
            "1",
            "bad-format",
            "1",
        ]);
        assert_eq!(code, EXIT_USAGE);
        assert!(out.contains("legacy survival direct parallel generation is replaced"));
        assert!(err.contains("format must be mca or linear"));
        assert!(!err.contains("not implemented yet"));

        let osm_aliases: Vec<Vec<String>> = vec![
            vec![
                "generate-survival-region-osm-pbf".to_string(),
                pbf.to_string_lossy().into_owned(),
                "2".to_string(),
                "missing-heightmap.tif".to_string(),
                "world".to_string(),
                "1000".to_string(),
                "0".to_string(),
                "0".to_string(),
                "bad-format".to_string(),
            ],
            vec![
                "generate-survival-region-osm-pbf-ref-window".to_string(),
                pbf.to_string_lossy().into_owned(),
                "2".to_string(),
                "0".to_string(),
                "1".to_string(),
                "missing-heightmap.tif".to_string(),
                "world".to_string(),
                "1000".to_string(),
                "0".to_string(),
                "0".to_string(),
                "bad-format".to_string(),
            ],
            vec![
                "generate-survival-region-osm-pbf-full-scan".to_string(),
                pbf.to_string_lossy().into_owned(),
                "2".to_string(),
                "missing-heightmap.tif".to_string(),
                "world".to_string(),
                "1000".to_string(),
                "0".to_string(),
                "0".to_string(),
                "bad-format".to_string(),
            ],
            vec![
                "generate-survival-region-osm-xml-cache".to_string(),
                osm_dir.to_string_lossy().into_owned(),
                "missing-heightmap.tif".to_string(),
                "world".to_string(),
                "1000".to_string(),
                "0".to_string(),
                "0".to_string(),
                "bad-format".to_string(),
            ],
        ];
        for args in osm_aliases {
            let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
            let (code, out, err) = run_capture(&borrowed);
            assert_eq!(code, EXIT_USAGE);
            assert!(out.is_empty());
            assert!(err.contains("format must be mca or linear"));
            assert!(!err.contains("not implemented yet"));
        }
    }

    #[test]
    fn benchmark_generation_commands_dispatch_without_java() {
        for command in [
            "benchmark-height-regions",
            "benchmark-surface-regions",
            "benchmark-survival-regions",
        ] {
            let (code, out, err) = run_capture(&[
                command,
                "missing-heightmap.tif",
                "world",
                "1000",
                "0",
                "0",
                "1",
                "1",
                "bad-format",
            ]);
            assert_eq!(code, EXIT_USAGE);
            assert!(out.is_empty());
            assert!(err.contains("format must be mca or linear"));
            assert!(!err.contains("not implemented yet"));
        }

        let (code, out, err) = run_capture(&[
            "benchmark-survival-regions-parallel",
            "missing-heightmap.tif",
            "world",
            "1000",
            "0",
            "0",
            "1",
            "1",
            "bad-format",
            "1",
        ]);
        assert_eq!(code, EXIT_USAGE);
        assert!(out.contains("legacy survival parallel benchmark is replaced"));
        assert!(err.contains("format must be mca or linear"));
        assert!(!err.contains("not implemented yet"));
    }

    #[test]
    fn describe_earth_grid_reports_region_bounds_without_java() {
        let temp = tempdir().unwrap();
        let heightmap = temp.path().join("tiny-grid.tif");
        fs::write(&heightmap, synthetic_bigtiff_heightmap()).unwrap();

        let (code, out, err) =
            run_capture(&["describe-earth-grid", heightmap.to_str().unwrap(), "10000"]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Earth grid described\n"));
        assert!(out.contains("scale=1:10000\n"));
        assert!(out.contains("widthBlocks=4008\n"));
        assert!(out.contains("heightBlocks=33\n"));
        assert!(out.contains("minRegionX=-4\n"));
        assert!(out.contains("maxRegionX=3\n"));
        assert!(out.contains("minRegionZ=-1\n"));
        assert!(out.contains("maxRegionZ=0\n"));
        assert!(out.contains("regionCols=8\n"));
        assert!(out.contains("regionRows=2\n"));
        assert!(out.contains("fullEarthRegions=16\n"));
        assert!(out.contains("generateVanillaDelegatedArgs="));
        assert!(out.contains("writeFinalizationArgs=<commandsFile> -4 -1 8 2"));
    }

    #[test]
    fn plan_representative_regions_writes_csv_without_java() {
        let temp = tempdir().unwrap();
        let heightmap = temp.path().join("tiny-plan.tif");
        let output_csv = temp.path().join("plan").join("representative.csv");
        fs::write(&heightmap, synthetic_bigtiff_heightmap()).unwrap();

        let (code, out, err) = run_capture(&[
            "plan-representative-regions",
            heightmap.to_str().unwrap(),
            "10000",
            output_csv.to_str().unwrap(),
            "3",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Representative region plan written\n"));
        assert!(out.contains("scale=1:10000\n"));
        assert!(out.contains("candidateRegions=16\n"));
        assert!(out.contains("selectedRegions=3\n"));
        assert!(out.contains("class.OCEAN="));
        assert!(out.contains("class.LAND="));
        let csv = fs::read_to_string(output_csv).unwrap();
        assert!(csv.starts_with(
            "index,regionX,regionZ,class,waterRatio,minGroundY,maxGroundY,dominantBiome\n"
        ));
        assert_eq!(csv.lines().count(), 4);
    }

    #[test]
    fn validate_surface_spawn_reports_viability_without_java() {
        let temp = tempdir().unwrap();
        let heightmap = temp.path().join("tiny-spawn.tif");
        fs::write(&heightmap, synthetic_bigtiff_heightmap()).unwrap();

        let (code, out, err) = run_capture(&[
            "validate-surface-spawn",
            heightmap.to_str().unwrap(),
            "10",
            "0",
            "0",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Surface spawn viable\n"));
        assert!(out.contains("regionX=0\n"));
        assert!(out.contains("regionZ=0\n"));
        assert!(out.contains("landColumns="));
        assert!(out.contains("waterColumns=0\n"));
        assert!(out.contains("bestSpawnX="));
        assert!(out.contains("bestSpawnY="));
        assert!(out.contains("bestSpawnZ="));
    }

    #[test]
    fn validate_height_seam_reports_surface_delta_without_java() {
        let temp = tempdir().unwrap();
        let heightmap = temp.path().join("tiny-seam.tif");
        fs::write(&heightmap, synthetic_bigtiff_heightmap()).unwrap();

        let (code, out, err) = run_capture(&[
            "validate-height-seam",
            heightmap.to_str().unwrap(),
            "10",
            "0",
            "0",
            "east",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Height seam valid\n"));
        assert!(out.contains("regionX=0\n"));
        assert!(out.contains("regionZ=0\n"));
        assert!(out.contains("direction=EAST\n"));
        assert!(out.contains("comparedColumns=512\n"));
        assert!(out.contains("coordinateFailures=0\n"));
        assert!(out.contains("minSurfaceY="));
        assert!(out.contains("maxSurfaceY="));
        assert!(out.contains("maxAbsSurfaceDelta="));
    }

    #[test]
    fn write_vanilla_finalization_commands_writes_force_load_windows() {
        let temp = tempdir().unwrap();
        let commands = temp.path().join("finalization").join("commands.txt");

        let (code, out, err) = run_capture(&[
            "write-vanilla-finalization-commands",
            commands.to_str().unwrap(),
            "-1",
            "2",
            "1",
            "1",
            "16",
            "250",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Vanilla finalization command file written\n"));
        assert!(out.contains("regionXStart=-1\n"));
        assert!(out.contains("regionZStart=2\n"));
        assert!(out.contains("cols=1\n"));
        assert!(out.contains("rows=1\n"));
        assert!(out.contains("windowChunks=16\n"));
        assert!(out.contains("waitMs=250\n"));
        assert!(out.contains("windows=4\n"));
        assert!(out.contains("maxChunksPerWindow=256\n"));

        let text = fs::read_to_string(commands).unwrap();
        assert!(text.contains("# range.regionX=-1..-1\n"));
        assert!(text.contains("# range.regionZ=2..2\n"));
        assert!(text.contains("forceload add -512 1024 -257 1279\n"));
        assert!(text.contains("@wait-ms 250\n"));
        assert!(text.ends_with("save-all flush\n@wait-ms 5000\n"));
    }

    #[test]
    fn survival_manifest_and_evidence_commands_run_without_java() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.properties");
        let output = temp
            .path()
            .join("validated")
            .join("earthmap-survival.properties");
        let boot = temp.path().join("boot.log");
        let reboot = temp.path().join("reboot.log");
        let spawn = temp.path().join("spawn.log");

        let mut values = BTreeMap::new();
        values.insert(
            "manifest.version".to_string(),
            earthmap_gameplay::MANIFEST_VERSION.to_string(),
        );
        values.insert(
            "minecraft.version".to_string(),
            build_info::MINECRAFT_TARGET.to_string(),
        );
        values.insert(
            "gameplay.claim".to_string(),
            earthmap_gameplay::CLAIM_EXPLORATION_ONLY.to_string(),
        );
        for key in earthmap_gameplay::REQUIRED_BOOLEAN_KEYS {
            values.insert((*key).to_string(), "true".to_string());
        }
        values.insert(
            "evidence.serverBootSaveReboot".to_string(),
            "false".to_string(),
        );
        values.insert("evidence.spawnToEnd".to_string(), "false".to_string());
        earthmap_gameplay::write_properties(&source, &values, "test").unwrap();
        fs::write(&boot, "Server run completed cleanly.").unwrap();
        fs::write(&reboot, "Server run completed cleanly.").unwrap();
        fs::write(
            &spawn,
            "Server command run completed cleanly.\n[Server] SPAWN_TO_END_ENTITY_TELEPORTED\n",
        )
        .unwrap();

        let (code, out, err) =
            run_capture(&["validate-survival-manifest", source.to_str().unwrap()]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Survival manifest validated\n"));
        assert!(out.contains("manifestValid=true\n"));
        assert!(out.contains("survivalCompleteAllowed=false\n"));
        assert!(out.contains("missing=evidence.serverBootSaveReboot\n"));
        assert!(out.contains("missing=evidence.spawnToEnd\n"));

        let (code, out, err) = run_capture(&[
            "apply-survival-evidence",
            source.to_str().unwrap(),
            output.to_str().unwrap(),
            boot.to_str().unwrap(),
            reboot.to_str().unwrap(),
            spawn.to_str().unwrap(),
            "survival-complete",
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Survival evidence applied\n"));
        assert!(out.contains("manifestValid=true\n"));
        assert!(out.contains("survivalCompleteAllowed=true\n"));
        assert!(out.contains("missingRequirements=0\n"));
        let written = fs::read_to_string(output).unwrap();
        assert!(written.contains("evidence.serverBootLog=boot.log\n"));
        assert!(written.contains("gameplay.claim=survival-complete\n"));
    }

    #[test]
    fn cave_ore_and_fluid_validators_run_without_java() {
        let (code, out, err) = run_capture(&["validate-cave-density", "42", "0", "0", "4"]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Cave density validated\n"));
        assert!(out.contains("sampledBlocks=2416\n"));
        assert!(out.contains("caveCandidateBlocks="));
        assert!(out.contains("caveRatio="));

        let (code, out, err) = run_capture(&["validate-cave-connectivity", "42", "0", "0", "4"]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Cave connectivity validated\n"));
        assert!(out.contains("componentCount="));
        assert!(out.contains("largestComponentRatio="));
        assert!(out.contains("entranceCandidateConnected="));

        let (code, out, err) = run_capture(&["validate-ore-histogram-synthetic"]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Ore histogram validated\n"));
        assert!(out.contains("chunkCount=1\n"));
        assert!(out.contains("totalOreBlocks=16\n"));
        assert!(out.contains("survivalCriticalComplete=true\n"));
        assert!(out.contains("ore.diamond=2\n"));

        let (code, out, err) = run_capture(&["validate-underground-fluid-synthetic"]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Underground fluid validated\n"));
        assert!(out.contains("waterBlocks="));
        assert!(out.contains("lavaBlocks="));
        assert!(out.contains("totalFluidBlocks="));
    }

    #[test]
    fn trace_surface_region_column_rejects_out_of_range_local_coords_before_io() {
        let (code, out, err) = run_capture(&[
            "trace-surface-region-column",
            "missing-heightmap.tif",
            "5000",
            "0",
            "0",
            "512",
            "0",
        ]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert!(err.contains("localX must be between 0 and 511: 512"));
    }

    #[test]
    fn trace_surface_region_cell_rejects_out_of_range_cell_coords_before_io() {
        let (code, out, err) = run_capture(&[
            "trace-surface-region-cell",
            "missing-heightmap.tif",
            "5000",
            "0",
            "0",
            "8",
            "0",
            "4",
            "0",
        ]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert!(err.contains("cellX must be between 0 and 3: 4"));
    }

    #[test]
    fn surface_region_column_trace_lines_include_stable_success_fields() {
        let base_column = EarthSurfaceColumn::new(
            false,
            70,
            63,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            "minecraft:savanna",
            "height-rule",
        );
        let semantic_column = base_column
            .with_decision_source("semantic")
            .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette)
            .with_data_evidence_flags(7);
        let photo_column = semantic_column.with_decision_source("photo");
        let final_column = photo_column.with_biome_id("minecraft:windswept_savanna");
        let trace = SurfaceRegionColumnTrace {
            local_x: 140,
            local_z: 3,
            global_block_x: 140,
            global_block_z: 3,
            map_x: 4147,
            map_z: 1873,
            longitude: 1.25,
            latitude: -2.5,
            raw_elevation_meters: 12.0,
            smoothed_elevation_meters: 11.5,
            local_relief_meters: 0.75,
            initial_water: false,
            valid: true,
            coast_factor: 1.0,
            material_sample: Some(SurfaceMaterialSample::new(
                RgbColor::of(10, 20, 30),
                RgbColor::of(40, 50, 60),
                TerrainTokenSource::JavaStandardPalette,
                1,
                2,
                3,
                4,
                5,
                6,
                7,
                8,
                9,
                1234,
                -56,
                78,
                "Test Ecoregion",
                "minecraft:savanna",
                0.5,
            )),
            base_column,
            semantic_column: Some(semantic_column),
            photo_column: Some(photo_column),
            post_cell_column: None,
            post_stabilized_column: None,
            post_first_component_trace: None,
            post_smoothed_column: None,
            post_component_column: None,
            post_component_trace: None,
            final_column,
        };

        let lines = surface_region_column_trace_lines(&trace, Path::new("surface.vrt"));

        assert!(lines.contains(&"surfaceRegionColumnTrace=valid".to_string()));
        assert!(lines.contains(&"localX=140".to_string()));
        assert!(lines.contains(&"longitude=1.25".to_string()));
        assert!(lines.contains(&"material.color=#0A141E".to_string()));
        assert!(lines.contains(&"material.terrainTokenColor=#28323C".to_string()));
        assert!(lines.contains(&"material.terrainTokenSource=JavaStandardPalette".to_string()));
        assert!(lines.contains(&"semantic.present=true".to_string()));
        assert!(lines.contains(&"photo.decisionSource=photo".to_string()));
        assert!(lines.contains(&"postCell.present=false".to_string()));
        assert!(lines.contains(&"postStabilized.present=false".to_string()));
        assert!(lines.contains(&"postFirstComponentTrace.present=false".to_string()));
        assert!(lines.contains(&"postSmoothed.present=false".to_string()));
        assert!(lines.contains(&"postComponent.present=false".to_string()));
        assert!(lines.contains(&"postComponentTrace.present=false".to_string()));
        assert!(lines.contains(&"final.biomeId=minecraft:windswept_savanna".to_string(),));
    }

    #[test]
    fn surface_region_cell_trace_lines_include_counts_and_prefixed_columns() {
        let first = surface_trace_fixture_column(
            140,
            0,
            "minecraft:savanna",
            "minecraft:windswept_savanna",
        );
        let second = surface_trace_fixture_column(141, 0, "minecraft:savanna", "minecraft:savanna");

        let lines =
            surface_region_cell_trace_lines(8, 0, 3, 0, &[first, second], Path::new("surface.vrt"));

        assert!(lines.contains(&"surfaceRegionCellTrace=valid".to_string()));
        assert!(lines.contains(&"regionLocalOriginX=140".to_string()));
        assert!(lines.contains(&"regionLocalOriginZ=0".to_string()));
        assert!(lines.contains(&"columnCount=2".to_string()));
        assert!(lines.contains(&"finalBiomeCount.1.biomeId=minecraft:savanna".to_string()));
        assert!(lines.contains(&"finalBiomeCount.1.count=1".to_string()));
        assert!(
            lines.contains(&"finalBiomeCount.2.biomeId=minecraft:windswept_savanna".to_string(),)
        );
        assert!(lines.contains(&"column.1.localX=140".to_string()));
        assert!(lines.contains(&"column.1.final.biomeId=minecraft:windswept_savanna".to_string(),));
        assert!(lines.contains(&"column.2.final.biomeId=minecraft:savanna".to_string()));
    }

    fn surface_trace_fixture_column(
        local_x: usize,
        local_z: usize,
        base_biome: &str,
        final_biome: &str,
    ) -> SurfaceRegionColumnTrace {
        let base_column = EarthSurfaceColumn::new(
            false,
            70,
            63,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            base_biome,
            "height-rule",
        );
        let semantic_column = base_column
            .with_decision_source("semantic")
            .with_terrain_token_source(TerrainTokenSource::JavaStandardPalette);
        let photo_column = semantic_column.with_decision_source("photo");
        let final_column = photo_column.with_biome_id(final_biome);
        SurfaceRegionColumnTrace {
            local_x,
            local_z,
            global_block_x: local_x as i32,
            global_block_z: local_z as i32,
            map_x: local_x as i32 + 4000,
            map_z: local_z as i32 + 1800,
            longitude: 1.25,
            latitude: -2.5,
            raw_elevation_meters: 12.0,
            smoothed_elevation_meters: 11.5,
            local_relief_meters: 0.75,
            initial_water: false,
            valid: true,
            coast_factor: 1.0,
            material_sample: None,
            base_column,
            semantic_column: Some(semantic_column),
            photo_column: Some(photo_column),
            post_cell_column: None,
            post_stabilized_column: None,
            post_first_component_trace: None,
            post_smoothed_column: None,
            post_component_column: None,
            post_component_trace: None,
            final_column,
        }
    }

    #[test]
    fn initial_commands_are_recognized_as_not_implemented() {
        for command in commands::INITIAL_COMMANDS
            .iter()
            .filter(|command| command.status == CommandStatus::NotPortedYet)
        {
            let (code, out, err) = run_capture(&[command.name]);

            assert_eq!(code, EXIT_USAGE, "command={}", command.name);
            assert!(out.is_empty(), "command={}", command.name);
            assert!(
                err.contains("not implemented yet"),
                "command={} stderr={}",
                command.name,
                err
            );
            assert!(
                err.contains(command.name),
                "command={} stderr={}",
                command.name,
                err
            );
        }
    }

    #[test]
    fn unknown_command_uses_java_like_error() {
        let (code, out, err) = run_capture(&["does-not-exist"]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert_eq!(err, "Unknown command. Use --help.\n");
    }

    #[test]
    fn meta_commands_are_detected_anywhere_like_java() {
        let (code, out, err) = run_capture(&["generate", "doctor"]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Rust port phase: phase1-cli-shell"));
    }

    #[test]
    fn help_lists_phase0_diagnostic_commands() {
        let (code, out, err) = run_capture(&["--help"]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("write-sha256-manifest <root> <outputFile>"));
        assert!(out.contains("write-region-payload-manifest <regionFile> <outputCsv>"));
        assert!(out.contains("compare-region-payload-manifest <manifestCsv> <regionFile>"));
        assert!(out.contains("validate-mca-region <path>"));
        assert!(out.contains("validate-linear-region <path>"));
        assert!(out.contains("compare-mca-linear-region-payloads <mcaRegion> <linearRegion>"));
        assert!(out.contains("mca-topdown-render <worldDir> <outputPng>"));
        assert!(out.contains("linear-topdown-render <worldDir> <outputPng>"));
        assert!(out.contains("dynmap-tile-mosaic <dynmapTileDir> <outputPng>"));
        assert!(out.contains("convert-mca-region-to-linear <mcaRegion> <linearRegion>"));
        assert!(out.contains("convert-mca-world-to-linear <mcaWorldDir> <linearWorldDir>"));
        assert!(out.contains("inspect-mca-palettes <path>"));
        assert!(out.contains("validate-mca-survival-palette <path>"));
        assert!(out.contains("inspect-linear-palettes <path>"));
        assert!(out.contains("validate-linear-survival-palette <path>"));
        assert!(out.contains("inspect-mca-biomes <path>"));
        assert!(out.contains("inspect-linear-biomes <path>"));
        assert!(out.contains("inspect-mca-statuses <path>"));
        assert!(out.contains("inspect-linear-statuses <path>"));
        assert!(out.contains("inspect-mca-post-final-integrity <path>"));
        assert!(out.contains("inspect-linear-post-final-integrity <path>"));
        assert!(out.contains("rewrite-mca-status <mcaRegion|regionDir|worldDir>"));
        assert!(out.contains("repair-mca-post-final-water <mcaRegion|regionDir|worldDir>"));
        assert!(out.contains("repair-linear-sandlike-surfaces <linearRegion|regionDir|worldDir>"));
        assert!(out.contains("summarize-region-chunk <regionFile> <localChunkX> <localChunkZ>"));
        assert!(
            out.contains("compare-region-chunk-details <expectedRegionFile> <actualRegionFile>")
        );
        assert!(out.contains("optional [heightmap] args use EARTHMAP_HEIGHTMAP=<GeoTIFF>"));
        assert!(out.contains("surfaceRaster=auto checks EARTHMAP_SURFACE_RASTER"));
        assert!(out.contains("trace-surface-region-column [heightmap] <scale> <regionX> <regionZ>"));
        assert!(out.contains("trace-surface-region-cell [heightmap] <scale> <regionX> <regionZ>"));
        assert!(out.contains("generate-flat-test-world <worldDir> <mca|linear>"));
        assert!(out.contains("generate-palette-stress-world <worldDir>"));
        assert!(out.contains("quality-candidate [heightmap] <worldDir>"));
        assert!(out.contains("generate <heightmap> <worldDir> <scale> <startRegionX>"));
        assert!(out.contains("generate-survival-region <heightmap> <worldDir>"));
        assert!(out.contains("generate-survival-region-osm-pbf <pbf> <maxBlobs>"));
        assert!(out.contains("generate-survival-region-osm-pbf-ref-window <pbf>"));
        assert!(out.contains("generate-survival-region-osm-pbf-full-scan <pbf>"));
        assert!(out.contains("generate-survival-region-osm-xml-cache <osmDirectory>"));
        assert!(out.contains("generate-survival-regions-parallel <heightmap>"));
        assert!(out.contains("generate-survival-region-plan-parallel <heightmap>"));
        assert!(out.contains("generate-vanilla-delegated-plan-parallel <heightmap>"));
        assert!(out.contains("generate-vanilla-delegated-region-plan-parallel <heightmap>"));
        assert!(out.contains("plan-representative-regions <heightmap> <scale> <outputCsv>"));
        assert!(out.contains("describe-earth-grid <heightmap> <scale>"));
        assert!(out.contains("validate-surface-spawn <heightmap> <scale> <regionX>"));
        assert!(out.contains("validate-height-seam <heightmap> <scale> <regionX>"));
        assert!(out.contains("write-vanilla-finalization-commands <outputCommands>"));
        assert!(out.contains("validate-survival-manifest <path>"));
        assert!(out.contains("apply-survival-evidence <sourceManifest> <outputManifest>"));
        assert!(out.contains("validate-cave-density <seed> <minBlockX>"));
        assert!(out.contains("validate-cave-connectivity <seed> <minBlockX>"));
        assert!(out.contains("validate-ore-histogram-synthetic"));
        assert!(out.contains("validate-underground-fluid-synthetic"));
        assert!(out.contains("validate-global-resource-fairness <worldDir> <outputDir>"));
        assert!(out.contains("validate-loot-economy <worldDir> <outputDir>"));
        assert!(out.contains("generate-nation-war-readiness-report <heightmap> <scale>"));
        assert!(out.contains("scan-osm-pbf <path> <maxBlobs>"));
        assert!(out.contains("scan-osm-pbf-range <path> <skipBlobs> <maxBlobs>"));
        assert!(out.contains("validate-osm-pbf <path> <maxBlobs>"));
        assert!(out.contains("benchmark-osm-index <scale> <regionX> <regionZ> <wayCount>"));
        assert!(out.contains("extract-osm-region-mask <path> <scale> <regionX>"));
        assert!(out.contains("extract-osm-region-mask-window <path> <scale>"));
        assert!(out.contains("extract-osm-region-mask-ref-window <path> <scale>"));
        assert!(out.contains("extract-osm-region-mask-full-scan <path> <scale>"));
        assert!(out.contains("extract-osm-xml-region-mask <osmDirectory> <scale>"));
        assert!(out.contains("identify-osm-xml-cache <directory>"));
        assert!(out.contains("benchmark-region-writers <outputDir> [iterations=3]"));
        assert!(out.contains("write-nbt-parity-fixtures <outputDir>"));
        assert!(out.contains("write-nbt-gzip-parity-fixtures <outputDir>"));
        assert!(out.contains("write-region-writer-parity-fixtures <outputDir>"));
        assert!(out.contains("inspect-heightmap [path]"));
        assert!(out.contains("locate-heightmap-point [heightmap] <scale> <longitude> <latitude>"));
        assert!(out.contains("classify-surface-point [heightmap] <scale> <longitude> <latitude>"));
        assert!(out.contains("raster-smoke [heightmap] <scale> <outputJson>"));
        assert!(out.contains("sample-vrt-rgb <terrainVrt> <longitude> <latitude>"));
        assert!(out.contains("photo-parity-crop <sourcePng> <metTargetPng|none>"));
        assert!(out.contains("photo-compare-crop <actualPng> <expectedPng>"));
        assert!(out.contains("photo-parity-metric-crop <sourcePng> <expectedPng>"));
        assert!(out.contains("photo-parity-metric-batch <jobsCsv> <outputRoot>"));
        assert!(out.contains("quality-production-sample-batch <samplesCsv> <heightmap>"));
        assert!(out.contains("benchmark-height-regions <heightmap> <worldDir>"));
        assert!(out.contains("benchmark-surface-regions <heightmap> <worldDir>"));
        assert!(out.contains("benchmark-survival-regions <heightmap> <worldDir>"));
        assert!(out.contains("benchmark-survival-regions-parallel <heightmap> <worldDir>"));
        assert!(out.contains("photo-standard-remap-parity-crop <sourcePng> <imageMagickRemapPng>"));
        assert!(out.contains("photo-standard-remap-parity-batch <jobsCsv> <outputRoot>"));
        assert!(out.contains("photo-production-candidate-diff-crop <sourcePng> <expectedPng>"));
        assert!(out.contains("photo-carrier-remap-sim-crop <currentSurfacePng> <expectedPng>"));
        assert!(out.contains(
            "Status: Rust runtime active; normal generation and validation paths are Rust-first."
        ));
        assert!(!out.contains("legacy fallback"));
    }

    #[test]
    fn osm_pbf_commands_run_without_java() {
        let temp = tempdir().unwrap();
        let pbf = temp.path().join("tiny.osm.pbf");
        fs::write(&pbf, synthetic_osm_pbf()).unwrap();

        let (code, out, err) = run_capture(&["scan-osm-pbf", pbf.to_str().unwrap(), "2"]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("OSM PBF scan complete"));
        assert!(out.contains("osmHeaderBlobs=1"));
        assert!(out.contains("osmDataBlobs=1"));
        assert!(out.contains("denseNodes=2"));
        assert!(out.contains("highwayWays=1"));

        let (code, out, err) =
            run_capture(&["scan-osm-pbf-range", pbf.to_str().unwrap(), "1", "1"]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("skipBlobs=1"));
        assert!(out.contains("osmDataBlobs=1"));

        let (code, out, err) = run_capture(&["validate-osm-pbf", pbf.to_str().unwrap(), "2"]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("OSM PBF integrity valid for scanned prefix"));

        let (code, out, err) = run_capture(&["benchmark-osm-index", "1000", "0", "0", "16"]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("OSM region index benchmark complete"));
        assert!(out.contains("ways=16"));
        assert!(out.contains("features=16"));

        let (code, out, err) = run_capture(&[
            "extract-osm-region-mask",
            pbf.to_str().unwrap(),
            "1000",
            "0",
            "0",
            "2",
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("OSM region mask extracted"));
        assert!(out.contains("indexedFeatures=1"));
        assert!(out.contains("roadFeatures=1"));

        let (code, out, err) = run_capture(&[
            "extract-osm-region-mask-window",
            pbf.to_str().unwrap(),
            "1000",
            "0",
            "0",
            "2",
            "0",
            "0",
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("OSM region mask window extracted"));
        assert!(out.contains("nodeMaxBlobs=2"));

        let (code, out, err) = run_capture(&[
            "extract-osm-region-mask-ref-window",
            pbf.to_str().unwrap(),
            "1000",
            "0",
            "0",
            "2",
            "1",
            "1",
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("OSM region mask ref-window extracted"));
        assert!(out.contains("indexedFeatures=1"));

        let progress = temp.path().join("progress.csv");
        let (code, out, err) = run_capture(&[
            "extract-osm-region-mask-full-scan",
            pbf.to_str().unwrap(),
            "1000",
            "0",
            "0",
            "2",
            "1",
            progress.to_str().unwrap(),
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("OSM region mask full scan extracted"));
        assert!(out.contains("progress,blobIndex=1"));
        assert!(fs::read_to_string(progress)
            .unwrap()
            .contains("progress,blobIndex=1"));
    }

    #[test]
    fn osm_xml_cache_commands_run_without_java() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join("xml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("a.osm"),
            r#"<osm>
<node id="1" lon="0.0" lat="0.0" />
<node id="2" lon="0.01" lat="0.0" />
<way id="7"><nd ref="1"/><nd ref="2"/><tag k="highway" v="primary"/></way>
</osm>"#,
        )
        .unwrap();
        fs::write(dir.join("ignore.txt"), "ignored").unwrap();

        let (code, out, err) = run_capture(&["identify-osm-xml-cache", dir.to_str().unwrap()]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("sourceKind=xml-directory"));
        assert!(out.contains("sourceFileCount=1"));
        assert!(out.contains("sourceSha256="));

        let (code, out, err) = run_capture(&[
            "extract-osm-xml-region-mask",
            dir.to_str().unwrap(),
            "1000",
            "0",
            "0",
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("OSM XML cache region mask extracted"));
        assert!(out.contains("osmDirectory="));
        assert!(out.contains("indexedFeatures=1"));
        assert!(out.contains("roadFeatures=1"));
    }

    #[test]
    fn gameplay_report_validators_dispatch_without_java() {
        let temp = tempdir().unwrap();
        let resource_world = temp.path().join("resource-world");
        let resource_region = resource_world.join("region").join("r.0.0.linear");
        fs::create_dir_all(resource_region.parent().unwrap()).unwrap();
        write_cli_single_chunk_linear_region(&resource_region, cli_resource_payload());
        let resource_output = temp.path().join("resource-output");

        let (code, out, err) = run_capture(&[
            "validate-global-resource-fairness",
            resource_world.to_str().unwrap(),
            resource_output.to_str().unwrap(),
        ]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.is_empty());
        assert!(out.contains("Global resource fairness validated"));
        assert!(out.contains("scannedRegions=1"));
        assert!(out.contains("partialChunkRegions=1"));
        assert!(out.contains("resourceFairnessPass=false"));
        assert!(resource_output
            .join(earthmap_gameplay::GLOBAL_RESOURCE_FAIRNESS_REPORT_FILE_NAME)
            .is_file());

        let loot_world = temp.path().join("loot-world");
        let loot_region = loot_world.join("region").join("r.0.0.linear");
        fs::create_dir_all(loot_region.parent().unwrap()).unwrap();
        write_cli_single_chunk_linear_region(&loot_region, cli_progression_payload());
        let loot_output = temp.path().join("loot-output");

        let (code, out, err) = run_capture(&[
            "validate-loot-economy",
            loot_world.to_str().unwrap(),
            loot_output.to_str().unwrap(),
        ]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.is_empty());
        assert!(out.contains("Loot economy validated"));
        assert!(out.contains("scannedRegions=1"));
        assert!(out.contains("partialChunkRegions=1"));
        assert!(out.contains("regionsWithCompleteProgression=0"));
        assert!(out.contains("lootEconomyPass=false"));
        assert!(loot_output
            .join(earthmap_gameplay::LOOT_ECONOMY_REPORT_FILE_NAME)
            .is_file());
    }

    #[test]
    fn nation_war_readiness_report_dispatches_without_java() {
        let temp = tempdir().unwrap();
        let heightmap = temp.path().join("tiny-heightmap.tif");
        fs::write(&heightmap, synthetic_bigtiff_heightmap()).unwrap();
        let output = temp.path().join("nation-war");

        let (code, out, err) = run_capture(&[
            "generate-nation-war-readiness-report",
            heightmap.to_str().unwrap(),
            "10000",
            output.to_str().unwrap(),
            "1",
            "1",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Nation-war readiness report generated"));
        assert!(out.contains("factionCountRequested=1"));
        assert!(out.contains("overallPass=false"));
        assert!(out.contains("nationWarReady=false"));
        assert!(output
            .join("earthmap-nation-war-readiness.properties")
            .is_file());
        assert!(output.join("earthmap-faction-starts.csv").is_file());
        assert!(output.join("earthmap-operator-launch-report.md").is_file());
        let report =
            fs::read_to_string(output.join("earthmap-nation-war-readiness.properties")).unwrap();
        assert!(report.contains("report.type=nation-war-readiness\n"));
        assert!(report.contains("pluginStack.tested=false\n"));
        assert!(report.contains("chunkLoadStress.tested=false\n"));
    }

    #[test]
    fn quality_candidate_args_use_default_heightmap_when_omitted() {
        let args = [
            "quality-candidate",
            "D:\\world",
            "5000",
            "0",
            "-1",
            "linear",
            "surfaceRaster=auto",
            "sampleGrid=32",
        ]
        .map(String::from);

        let parsed = quality_candidate_args_with_default(
            &args,
            Some("fixtures/HQheightmap.tif".to_string()),
        )
        .unwrap()
        .unwrap();

        assert_eq!(parsed.heightmap_path, "fixtures/HQheightmap.tif");
        assert_eq!(parsed.world_dir, "D:\\world");
        assert_eq!(parsed.scale, "5000");
        assert_eq!(parsed.region_x, "0");
        assert_eq!(parsed.region_z, "-1");
        assert_eq!(parsed.format, "linear");
        assert_eq!(parsed.surface_raster, "surfaceRaster=auto");
        assert_eq!(parsed.sample_grid, "sampleGrid=32");
    }

    #[test]
    fn quality_candidate_args_keep_explicit_heightmap_compatible() {
        let args = [
            "quality-candidate",
            "E:\\HQheightmap.tif",
            "D:\\world",
            "5000",
            "1",
            "2",
            "mca",
        ]
        .map(String::from);

        let parsed = quality_candidate_args(&args).unwrap().unwrap();

        assert_eq!(parsed.heightmap_path, "E:\\HQheightmap.tif");
        assert_eq!(parsed.world_dir, "D:\\world");
        assert_eq!(parsed.format, "mca");
        assert_eq!(parsed.surface_raster, "surfaceRaster=auto");
        assert_eq!(parsed.sample_grid, "sampleGrid=64");
    }

    #[test]
    fn quality_evidence_helpers_validate_grid_and_iteration_bounds() {
        assert_eq!(parse_quality_sample_grid("sampleGrid=32").unwrap(), 32);
        assert_eq!(parse_quality_sample_grid("64").unwrap(), 64);
        assert!(parse_quality_sample_grid("sampleGrid=7").is_err());
        assert!(parse_quality_sample_grid("sampleGrid=257").is_err());

        assert_eq!(parse_benchmark_iterations("iterations=5").unwrap(), 5);
        assert_eq!(parse_benchmark_iterations("3").unwrap(), 3);
        assert!(parse_benchmark_iterations("iterations=0").is_err());
        assert!(parse_benchmark_iterations("iterations=21").is_err());
    }

    #[test]
    fn quality_preview_columns_sample_cell_centers_across_region() {
        let columns = quality_preview_columns(8);

        assert_eq!(columns.len(), 64);
        assert_eq!(columns[0], (32, 32));
        assert_eq!(columns[7], (480, 32));
        assert_eq!(columns[63], (480, 480));
    }

    #[test]
    fn vanilla_delegated_args_use_default_heightmap_when_omitted() {
        let args = [
            "generate-vanilla-delegated-region",
            "D:\\world",
            "147760",
            "0",
            "0",
            "linear",
            "full",
            "surfaceRaster=D:\\surface.vrt",
        ]
        .map(String::from);

        let parsed = vanilla_delegated_args_with_default(
            &args,
            Some("fixtures/HQheightmap.tif".to_string()),
        )
        .unwrap()
        .unwrap();

        assert_eq!(parsed.heightmap_path, "fixtures/HQheightmap.tif");
        assert_eq!(parsed.world_dir, "D:\\world");
        assert_eq!(parsed.scale, "147760");
        assert_eq!(parsed.status, "full");
        assert_eq!(parsed.surface_raster, "surfaceRaster=D:\\surface.vrt");
        assert!(parsed.extra_options.is_empty());
    }

    #[test]
    fn vanilla_delegated_args_keep_explicit_heightmap_compatible() {
        let args = [
            "generate-vanilla-delegated-region",
            "E:\\HQheightmap.tif",
            "D:\\world",
            "147760",
            "0",
            "0",
            "linear",
            "surfaceRaster=D:\\surface.vrt",
        ]
        .map(String::from);

        let parsed = vanilla_delegated_args(&args).unwrap().unwrap();

        assert_eq!(parsed.heightmap_path, "E:\\HQheightmap.tif");
        assert_eq!(parsed.world_dir, "D:\\world");
        assert_eq!(parsed.scale, "147760");
        assert_eq!(parsed.status, "surface");
        assert_eq!(parsed.surface_raster, "surfaceRaster=D:\\surface.vrt");
        assert!(parsed.extra_options.is_empty());
    }

    #[test]
    fn vanilla_delegated_args_keep_compression_as_optional_generation_option() {
        let args = [
            "generate-vanilla-delegated-region",
            "E:\\HQheightmap.tif",
            "D:\\world",
            "147760",
            "0",
            "0",
            "linear",
            "compression=9",
        ]
        .map(String::from);

        let parsed = vanilla_delegated_args(&args).unwrap().unwrap();

        assert_eq!(parsed.status, "surface");
        assert_eq!(parsed.surface_raster, "surfaceRaster=auto");
        assert_eq!(parsed.extra_options, ["compression=9".to_string()]);
    }

    #[test]
    fn vanilla_delegated_parallel_args_keep_surface_raster_and_compression_options() {
        let args = [
            "generate-vanilla-delegated-regions-parallel",
            "E:\\HQheightmap.tif",
            "D:\\world",
            "1000",
            "26",
            "-10",
            "3",
            "3",
            "mca",
            "8",
            "carvers",
            "surfaceRaster=D:\\surface.vrt",
            "mcaCompression=9",
        ]
        .map(String::from);

        let parsed = vanilla_delegated_regions_parallel_args(&args).unwrap();

        assert_eq!(parsed.status, "carvers");
        assert_eq!(parsed.surface_raster, "surfaceRaster=D:\\surface.vrt");
        assert_eq!(parsed.extra_options, ["mcaCompression=9".to_string()]);
    }

    #[test]
    fn region_compression_options_apply_generic_key_to_selected_format() {
        let linear = parse_region_compression_options(
            OutputFormat::LinearV2,
            &["compression=9".to_string()],
        )
        .unwrap();
        let mca =
            parse_region_compression_options(OutputFormat::Mca, &["compression=3".to_string()])
                .unwrap();

        assert_eq!(
            linear,
            RegionCompressionOptions {
                linear_compression_level: Some(9),
                mca_compression_level: None
            }
        );
        assert_eq!(
            mca,
            RegionCompressionOptions {
                linear_compression_level: None,
                mca_compression_level: Some(3)
            }
        );
    }

    #[test]
    fn generation_runtime_options_parse_scale_aware_vertical_profile() {
        let auto = parse_generation_runtime_options(
            OutputFormat::LinearV2,
            &[
                "verticalScale=auto".to_string(),
                "linearCompression=4".to_string(),
            ],
        )
        .unwrap();
        let legacy = parse_generation_runtime_options(
            OutputFormat::LinearV2,
            &["verticalScale=legacy".to_string()],
        )
        .unwrap();
        let explicit = parse_generation_runtime_options(
            OutputFormat::LinearV2,
            &["verticalProfile=2.5".to_string()],
        )
        .unwrap();

        assert_eq!(auto.vertical_scale.effective(200), 4.0);
        assert_eq!(auto.vertical_scale.label(), "auto");
        assert_eq!(auto.compression.linear_compression_level, Some(4));
        assert_eq!(legacy.vertical_scale.effective(200), DEFAULT_VERTICAL_SCALE);
        assert_eq!(legacy.vertical_scale.label(), "legacy");
        assert_eq!(explicit.vertical_scale.effective(200), 2.5);
        assert_eq!(explicit.vertical_scale.label(), "explicit");
    }

    #[test]
    fn generation_runtime_options_parse_prefetch_memory_and_queue() {
        let parsed = parse_generation_runtime_options(
            OutputFormat::LinearV2,
            &[
                "prefetchMemoryGB=25".to_string(),
                "prefetchRegions=3".to_string(),
                "prefetchWorkers=2".to_string(),
            ],
        )
        .unwrap();

        assert!(parsed.prefetch.enabled);
        assert_eq!(parsed.prefetch.memory_cap_bytes, Some(25 * BYTES_PER_GIB));
        assert_eq!(parsed.prefetch.queue_regions, Some(3));
        assert_eq!(parsed.prefetch.workers, 2);
    }

    #[test]
    fn effective_prefetch_config_is_memory_bounded() {
        let options = GenerationPrefetchOptions {
            enabled: true,
            memory_cap_bytes: Some(PREPARED_SURFACE_REGION_ESTIMATED_BYTES),
            queue_regions: Some(16),
            workers: 4,
        };
        let config = effective_prefetch_config(options, 8, 100);

        assert!(config.enabled);
        assert_eq!(config.queue_regions, 1);
        assert_eq!(config.workers, 1);
        assert_eq!(
            config.memory_cap_bytes,
            Some(PREPARED_SURFACE_REGION_ESTIMATED_BYTES)
        );
    }

    #[test]
    fn region_compression_options_reject_out_of_range_values() {
        let mca_error =
            parse_region_compression_options(OutputFormat::Mca, &["mcaCompression=10".to_string()])
                .unwrap_err();
        let linear_error = parse_region_compression_options(
            OutputFormat::LinearV2,
            &["linearCompression=23".to_string()],
        )
        .unwrap_err();

        assert!(mca_error.contains("0..9"));
        assert!(linear_error.contains("1..22"));
    }

    #[test]
    fn auto_cache_tuning_scales_with_system_memory_without_exhausting_it() {
        let small_memory = Some(16 * BYTES_PER_GIB);
        let large_memory = Some(128 * BYTES_PER_GIB);

        let small_height_rows =
            auto_shared_heightmap_cache_rows_for_memory(864_000, 8, small_memory).unwrap();
        let large_height_rows =
            auto_shared_heightmap_cache_rows_for_memory(864_000, 8, large_memory).unwrap();
        let small_surface_entries = auto_surface_tile_cache_entries_for_memory(small_memory, 12);
        let large_surface_entries = auto_surface_tile_cache_entries_for_memory(large_memory, 12);

        assert!(large_height_rows > small_height_rows);
        assert!(large_height_rows <= HEIGHTMAP_CACHE_MAX_ROWS);
        assert!(large_surface_entries > small_surface_entries);
        assert!(large_surface_entries <= SURFACE_TILE_CACHE_MAX_ENTRIES);
    }

    #[test]
    fn auto_surface_cache_reader_count_includes_companion_rasters() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("TifFiles");
        let terrain = root.join("terrain");
        let vegetation = root.join("vegetation");
        fs::create_dir_all(&terrain).unwrap();
        fs::create_dir_all(&vegetation).unwrap();
        let true_marble = terrain.join("TrueMarble.vrt");
        fs::write(&true_marble, b"vrt").unwrap();
        fs::write(root.join("climate.tif"), b"climate").unwrap();
        fs::write(root.join("ocean_temp_infill.tif"), b"ocean").unwrap();
        fs::write(root.join("land_shallow_topo_west.tif"), b"west").unwrap();
        fs::write(root.join("land_shallow_topo_east.tif"), b"east").unwrap();
        fs::write(vegetation.join("Shrubs.tif"), b"shrubs").unwrap();

        assert_eq!(surface_cache_reader_count(Some(&true_marble)), 6);
        assert_eq!(surface_cache_reader_count(None), 1);
    }

    #[test]
    fn surface_trace_args_use_default_heightmap_when_omitted() {
        let column_args = [
            "trace-surface-region-column",
            "147760",
            "0",
            "0",
            "146",
            "2",
            "surfaceRaster=D:\\surface.vrt",
        ]
        .map(String::from);
        let cell_args = [
            "trace-surface-region-cell",
            "147760",
            "0",
            "0",
            "8",
            "0",
            "3",
            "0",
            "surfaceRaster=D:\\surface.vrt",
        ]
        .map(String::from);

        let parsed_column = trace_column_args_with_default(
            &column_args,
            Some("fixtures/HQheightmap.tif".to_string()),
        )
        .unwrap();
        let parsed_cell =
            trace_cell_args_with_default(&cell_args, Some("fixtures/HQheightmap.tif".to_string()))
                .unwrap();

        assert_eq!(parsed_column.heightmap_path, "fixtures/HQheightmap.tif");
        assert_eq!(parsed_column.scale, "147760");
        assert_eq!(
            parsed_column.surface_raster,
            Some("surfaceRaster=D:\\surface.vrt")
        );
        assert_eq!(parsed_cell.heightmap_path, "fixtures/HQheightmap.tif");
        assert_eq!(parsed_cell.chunk_local_x, "8");
        assert_eq!(
            parsed_cell.surface_raster,
            Some("surfaceRaster=D:\\surface.vrt")
        );
    }

    #[test]
    fn optional_heightmap_args_keep_numeric_explicit_paths_compatible() {
        let delegated_args = [
            "generate-vanilla-delegated-region",
            "heightmaps\\5000",
            "5000",
            "147760",
            "0",
            "0",
            "linear",
        ]
        .map(String::from);
        let column_args = [
            "trace-surface-region-column",
            "5000",
            "147760",
            "0",
            "0",
            "146",
            "2",
        ]
        .map(String::from);
        let cell_args = [
            "trace-surface-region-cell",
            "5000",
            "147760",
            "0",
            "0",
            "8",
            "0",
            "3",
            "0",
        ]
        .map(String::from);

        let parsed_delegated = vanilla_delegated_args(&delegated_args).unwrap().unwrap();
        let parsed_column = trace_column_args(&column_args).unwrap();
        let parsed_cell = trace_cell_args(&cell_args).unwrap();

        assert_eq!(parsed_delegated.heightmap_path, "heightmaps\\5000");
        assert_eq!(parsed_delegated.world_dir, "5000");
        assert_eq!(parsed_delegated.scale, "147760");
        assert_eq!(parsed_column.heightmap_path, "5000");
        assert_eq!(parsed_column.scale, "147760");
        assert_eq!(parsed_cell.heightmap_path, "5000");
        assert_eq!(parsed_cell.scale, "147760");
    }

    #[test]
    fn vanilla_delegated_args_reject_invalid_six_token_explicit_shape_without_panic() {
        let (code, out, err) = run_capture(&[
            "generate-vanilla-delegated-region",
            "heightmaps\\5000",
            "world",
            "5000",
            "0",
            "0",
        ]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert_eq!(
            err,
            "Invalid generate-vanilla-delegated-region arguments. Use --help.\n"
        );
    }

    #[test]
    fn generate_flat_test_world_rejects_unknown_format_like_java() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("flat");
        let (code, out, err) =
            run_capture(&["generate-flat-test-world", world.to_str().unwrap(), "zip"]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert_eq!(
            err,
            "Flat test world generation failed: format must be mca or linear\n"
        );
    }

    #[test]
    fn validate_mca_region_reports_flat_fixture() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("flat-mca");
        generate_flat_test_world_impl(world.to_str().unwrap(), "mca").unwrap();
        let region = world.join("region").join("r.0.0.mca");

        let (code, out, err) = run_capture(&["validate-mca-region", region.to_str().unwrap()]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA region valid\n"));
        assert!(out.contains("chunkCount=1024\n"));
        assert!(out.contains("totalSectors="));
        assert!(out.contains("fileBytes="));
    }

    #[test]
    fn validate_linear_region_reports_flat_fixture() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("flat-linear");
        generate_flat_test_world_impl(world.to_str().unwrap(), "linear").unwrap();
        let region = world.join("region").join("r.0.0.linear");

        let (code, out, err) = run_capture(&["validate-linear-region", region.to_str().unwrap()]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Linear V2 region valid\n"));
        assert!(out.contains("regionX=0\n"));
        assert!(out.contains("regionZ=0\n"));
        assert!(out.contains("gridSize=8\n"));
        assert!(out.contains("chunkCount=1024\n"));
        assert!(out.contains("bitmapConsistent=true\n"));
    }

    #[test]
    fn compare_mca_linear_region_payloads_reports_flat_fixture_match() {
        let temp = tempdir().unwrap();
        let mca_world = temp.path().join("flat-mca");
        let linear_world = temp.path().join("flat-linear");
        generate_flat_test_world_impl(mca_world.to_str().unwrap(), "mca").unwrap();
        generate_flat_test_world_impl(linear_world.to_str().unwrap(), "linear").unwrap();
        let mca_region = mca_world.join("region").join("r.0.0.mca");
        let linear_region = linear_world.join("region").join("r.0.0.linear");

        let (code, out, err) = run_capture(&[
            "compare-mca-linear-region-payloads",
            mca_region.to_str().unwrap(),
            linear_region.to_str().unwrap(),
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA/Linear payload parity compared\n"));
        assert!(out.contains("matches=true\n"));
        assert!(out.contains("comparedChunks=1024\n"));
        assert!(out.contains("matchingChunks=1024\n"));
        assert!(out.contains("mismatchedChunks=0\n"));
        assert!(out.contains("missingInMca=0\n"));
        assert!(out.contains("missingInLinear=0\n"));
        assert!(out.contains("firstMismatch=\n"));
    }

    #[test]
    fn mca_topdown_render_reports_flat_fixture() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("flat-mca");
        let output = temp.path().join("mca-topdown.png");
        generate_single_chunk_render_world(&world, TopdownFormat::Mca);

        let (code, out, err) = run_capture(&[
            "mca-topdown-render",
            world.to_str().unwrap(),
            output.to_str().unwrap(),
            "0",
            "0",
            "1",
            "1",
            "visible",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA top-down render written\n"));
        assert!(out.contains("width=512\n"));
        assert!(out.contains("height=512\n"));
        assert!(out.contains("mode=visible\n"));
        assert!(out.contains("regionCount=1\n"));
        assert!(out.contains("missingRegions=0\n"));
        assert!(out.contains("chunkCount=1\n"));
        assert!(out.contains("missingChunks=1023\n"));
        assert!(out.contains("columnCount=256\n"));
        assert!(out.contains("airColumns=0\n"));
        let metadata = fs::read_to_string(temp.path().join("mca-topdown.properties")).unwrap();
        assert!(metadata.contains("chunkCount=1\n"));
        let (width, height, pixels) = read_png_rgb(&output);
        assert_eq!((width, height), (512, 512));
        assert_eq!(&pixels[0..3], &[100, 146, 67]);
        assert_eq!(
            &pixels[((CHUNK_WIDTH * 512) * 3)..((CHUNK_WIDTH * 512) * 3 + 3)],
            &[0, 0, 0]
        );
    }

    #[test]
    fn linear_topdown_render_reports_flat_fixture() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("flat-linear");
        let output = temp.path().join("linear-topdown.png");
        generate_single_chunk_render_world(&world, TopdownFormat::Linear);

        let (code, out, err) = run_capture(&[
            "linear-topdown-render",
            world.to_str().unwrap(),
            output.to_str().unwrap(),
            "0",
            "0",
            "1",
            "1",
            "terrain",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Linear top-down render written\n"));
        assert!(out.contains("width=512\n"));
        assert!(out.contains("height=512\n"));
        assert!(out.contains("mode=terrain\n"));
        assert!(out.contains("regionCount=1\n"));
        assert!(out.contains("missingRegions=0\n"));
        assert!(out.contains("chunkCount=1\n"));
        assert!(out.contains("missingChunks=1023\n"));
        assert!(out.contains("columnCount=256\n"));
        assert!(out.contains("airColumns=0\n"));
        let metadata = fs::read_to_string(temp.path().join("linear-topdown.properties")).unwrap();
        assert!(metadata.contains("mode=terrain\n"));
        let (width, height, pixels) = read_png_rgb(&output);
        assert_eq!((width, height), (512, 512));
        assert_eq!(&pixels[0..3], &[100, 146, 67]);
        assert_eq!(
            &pixels[((CHUNK_WIDTH * 512) * 3)..((CHUNK_WIDTH * 512) * 3 + 3)],
            &[0, 0, 0]
        );
    }

    #[test]
    fn dynmap_tile_mosaic_builds_base_mosaic() {
        let temp = tempdir().unwrap();
        let tiles = temp.path().join("tiles");
        let output = temp.path().join("base.png");
        write_solid_png_tile(&tiles.join("0_0.png"), [255, 0, 0]);
        write_solid_png_tile(&tiles.join("1_0.png"), [0, 255, 0]);
        write_solid_png_tile(&tiles.join("nested").join("0_1.png"), [0, 0, 255]);
        write_solid_png_tile(&tiles.join("nested").join("1_1.png"), [255, 255, 0]);
        write_solid_png_tile(&tiles.join("z_0_0.png"), [255, 0, 255]);
        write_solid_png_tile(&tiles.join("not-a-tile.png"), [0, 0, 0]);

        let (code, out, err) = run_capture(&[
            "dynmap-tile-mosaic",
            tiles.to_str().unwrap(),
            output.to_str().unwrap(),
            "base",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Dynmap tile mosaic written\n"));
        assert!(out.contains("levelPrefix=base\n"));
        assert!(out.contains("tileCount=4\n"));
        assert!(out.contains("tileWidth=2\n"));
        assert!(out.contains("tileHeight=2\n"));
        assert!(out.contains("minTileX=0\n"));
        assert!(out.contains("maxTileX=1\n"));
        assert!(out.contains("minTileY=0\n"));
        assert!(out.contains("maxTileY=1\n"));
        assert!(out.contains("width=4\n"));
        assert!(out.contains("height=4\n"));
        assert!(out.contains("missingTileCount=0\n"));
        assert!(out.contains("emptyTileCount=0\n"));
        let metadata = fs::read_to_string(temp.path().join("base.properties")).unwrap();
        assert!(metadata.contains("levelPrefix=base\n"));
        let (width, height, pixels) = read_png_rgb(&output);
        assert_eq!((width, height), (4, 4));
        assert_png_pixel(&pixels, width, 0, 0, [255, 0, 0]);
        assert_png_pixel(&pixels, width, 3, 0, [0, 255, 0]);
        assert_png_pixel(&pixels, width, 0, 3, [0, 0, 255]);
        assert_png_pixel(&pixels, width, 3, 3, [255, 255, 0]);
    }

    #[test]
    fn dynmap_tile_mosaic_filters_zoom_prefix() {
        let temp = tempdir().unwrap();
        let tiles = temp.path().join("tiles");
        let output = temp.path().join("z.png");
        write_solid_png_tile(&tiles.join("0_0.png"), [255, 0, 0]);
        write_solid_png_tile(&tiles.join("z_0_0.png"), [0, 255, 255]);

        let (code, out, err) = run_capture(&[
            "dynmap-tile-mosaic",
            tiles.to_str().unwrap(),
            output.to_str().unwrap(),
            "z_",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("levelPrefix=z\n"));
        assert!(out.contains("tileCount=1\n"));
        let (width, height, pixels) = read_png_rgb(&output);
        assert_eq!((width, height), (2, 2));
        assert_png_pixel(&pixels, width, 0, 0, [0, 255, 255]);
    }

    #[test]
    fn photo_parity_metric_crop_writes_rust_metrics_and_artifacts() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.png");
        let expected = temp.path().join("expected.png");
        let current = temp.path().join("current.png");
        write_metric_test_png(&source, false);
        write_metric_test_png(&expected, false);
        write_metric_test_png(&current, true);
        let output = temp.path().join("metric");

        let (code, out, err) = run_capture(&[
            "photo-parity-metric-crop",
            source.to_str().unwrap(),
            expected.to_str().unwrap(),
            current.to_str().unwrap(),
            output.to_str().unwrap(),
            "0",
            "0",
            "4",
            "4",
            "none",
            "all",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Photo parity metric crop written\n"));
        assert!(out.contains("[current-vs-expected]\n"));
        assert!(out.contains("meanDeltaE2000="));
        assert!(output.join("source-crop.png").is_file());
        assert!(output.join("expected-crop.png").is_file());
        assert!(output.join("current-surface-crop.png").is_file());
        assert!(output.join("current-vs-expected-error.png").is_file());
        assert!(output.join("current-vs-expected-top-errors.csv").is_file());
        assert!(output
            .join("current-vs-expected-palette-summary.txt")
            .is_file());
        assert!(output.join("candidate-canopy-density-4x4.png").is_file());
        let metrics = fs::read_to_string(output.join("metrics.txt")).unwrap();
        assert!(metrics.contains("[candidate-canopy-density-4x4-vs-expected]\n"));
    }

    #[test]
    fn photo_compare_crop_writes_metric_report_without_java() {
        let temp = tempdir().unwrap();
        let actual = temp.path().join("actual.png");
        let expected = temp.path().join("expected.png");
        write_metric_test_png(&actual, true);
        write_metric_test_png(&expected, false);
        let output = temp.path().join("compare");

        let (code, out, err) = run_capture(&[
            "photo-compare-crop",
            actual.to_str().unwrap(),
            expected.to_str().unwrap(),
            output.to_str().unwrap(),
            "0",
            "0",
            "4",
            "4",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Photo compare crop written\n"));
        assert!(out.contains("[actual-vs-expected]\n"));
        assert!(output.join("actual-crop.png").is_file());
        assert!(output.join("actual-vs-expected-error.png").is_file());
        assert!(output.join("actual-vs-expected-top-errors.csv").is_file());
    }

    #[test]
    fn photo_parity_metric_batch_runs_multiple_jobs_in_one_rust_call() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.png");
        let expected = temp.path().join("expected.png");
        let current = temp.path().join("current.png");
        write_metric_test_png(&source, false);
        write_metric_test_png(&expected, false);
        write_metric_test_png(&current, true);
        let jobs = temp.path().join("jobs.csv");
        fs::write(
            &jobs,
            format!(
                "sample,source,expected,current,cropX,cropY,cropWidth,cropHeight,mask,maskMode\n\
alpha,{},{},{},0,0,4,4,none,all\n\
beta,{},{},{},1,1,2,2,none,all\n",
                source.display(),
                expected.display(),
                current.display(),
                source.display(),
                expected.display(),
                current.display()
            ),
        )
        .unwrap();
        let output_root = temp.path().join("out");

        let (code, out, err) = run_capture(&[
            "photo-parity-metric-batch",
            jobs.to_str().unwrap(),
            output_root.to_str().unwrap(),
            "2",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Photo parity metric batch written\n"));
        assert!(out.contains("jobs=2\n"));
        assert!(out.contains("threads=2\n"));
        assert!(out.contains("sample,elapsedMillis,pixels,currentVsExpectedMean"));
        assert!(output_root
            .join("alpha")
            .join("metric-land")
            .join("source-crop.png")
            .is_file());
        assert!(output_root
            .join("beta")
            .join("metric-land")
            .join("candidate-canopy-density-4x4.png")
            .is_file());
    }

    #[test]
    fn photo_parity_crop_writes_palette_remap_artifacts() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.png");
        let expected = temp.path().join("expected.png");
        let current = temp.path().join("current.png");
        write_metric_test_png(&source, false);
        write_metric_test_png(&expected, false);
        write_metric_test_png(&current, true);
        let output = temp.path().join("parity");

        let (code, out, err) = run_capture(&[
            "photo-parity-crop",
            source.to_str().unwrap(),
            expected.to_str().unwrap(),
            current.to_str().unwrap(),
            expected.to_str().unwrap(),
            output.to_str().unwrap(),
            "0",
            "0",
            "4",
            "4",
            "none",
            "none",
            "all",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Photo parity crop written\n"));
        assert!(out.contains("[candidate-rgb-vs-met]\n"));
        assert!(output.join("source-crop.png").is_file());
        assert!(output.join("candidate-remap.png").is_file());
        assert!(output.join("candidate-standard-remap-rgb.png").is_file());
    }

    #[test]
    fn photo_standard_remap_parity_crop_and_batch_write_artifacts() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.png");
        let image_magick = temp.path().join("imagemagick.png");
        write_metric_test_png(&source, false);
        write_metric_test_png(&image_magick, false);
        let output = temp.path().join("standard");

        let (code, out, err) = run_capture(&[
            "photo-standard-remap-parity-crop",
            source.to_str().unwrap(),
            image_magick.to_str().unwrap(),
            output.to_str().unwrap(),
            "0",
            "0",
            "4",
            "4",
            "none",
            "all",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Photo Standard remap parity crop written\n"));
        assert!(output.join("java-standard-remap-crop.png").is_file());
        assert!(output
            .join("java-standard-remap-vs-imagemagick-error.png")
            .is_file());

        let jobs = temp.path().join("standard-jobs.csv");
        fs::write(
            &jobs,
            format!(
                "sample,source,imageMagick,cropX,cropY,cropWidth,cropHeight,mask,maskMode\n\
alpha,{},{},0,0,4,4,none,all\n\
beta,{},{},1,1,2,2,none,all\n",
                source.display(),
                image_magick.display(),
                source.display(),
                image_magick.display()
            ),
        )
        .unwrap();
        let output_root = temp.path().join("standard-out");
        let (code, out, err) = run_capture(&[
            "photo-standard-remap-parity-batch",
            jobs.to_str().unwrap(),
            output_root.to_str().unwrap(),
            "2",
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Photo Standard remap parity batch written\n"));
        assert!(output_root
            .join("beta")
            .join("standard-remap-parity")
            .join("java-standard-remap-vs-imagemagick-error.png")
            .is_file());
    }

    #[test]
    fn photo_candidate_diff_and_carrier_sim_write_reports() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.png");
        let expected = temp.path().join("expected.png");
        let current = temp.path().join("current.png");
        let candidate = temp.path().join("candidate.png");
        let carriers = temp.path().join("carriers.csv");
        write_metric_test_png(&source, false);
        write_metric_test_png(&expected, false);
        write_metric_test_png(&current, true);
        write_metric_test_png(&candidate, false);
        fs::write(&carriers, "id,rgb\nblack,#000000\nwhite,#FFFFFF\n").unwrap();

        let diff_output = temp.path().join("diff");
        let (code, out, err) = run_capture(&[
            "photo-production-candidate-diff-crop",
            source.to_str().unwrap(),
            expected.to_str().unwrap(),
            current.to_str().unwrap(),
            candidate.to_str().unwrap(),
            diff_output.to_str().unwrap(),
            "0",
            "0",
            "4",
            "4",
            "none",
            "all",
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Photo production-candidate diff crop written\n"));
        assert!(diff_output
            .join("production-candidate-token-diff-summary.csv")
            .is_file());
        assert!(diff_output
            .join("candidate-gain-vs-source-primary.png")
            .is_file());

        let sim_output = temp.path().join("sim");
        let (code, out, err) = run_capture(&[
            "photo-carrier-remap-sim-crop",
            current.to_str().unwrap(),
            expected.to_str().unwrap(),
            sim_output.to_str().unwrap(),
            "0",
            "0",
            "4",
            "4",
            "none",
            "all",
            carriers.to_str().unwrap(),
            "2",
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Photo carrier remap simulation crop written\n"));
        assert!(sim_output.join("carrier-remap-simulation.csv").is_file());
        assert!(sim_output.join("best-positive-remap.png").is_file());
        let csv = fs::read_to_string(sim_output.join("carrier-remap-simulation.csv")).unwrap();
        assert!(csv.contains("white,#FFFFFF") || csv.contains("black,#000000"));
    }

    #[test]
    fn quality_production_sample_csv_accepts_existing_columns_and_aliases() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.png");
        let expected = temp.path().join("expected.png");
        let mask = temp.path().join("mask.png");
        write_metric_test_png(&source, false);
        write_metric_test_png(&expected, false);
        write_metric_test_png(&mask, false);
        let output_root = temp.path().join("out");
        let samples = temp.path().join("samples.csv");
        fs::write(
            &samples,
            "sample,regionX,regionZ,x,y,width,height,source,expected,mask,outputDir\n\
alpha,1,-2,0,1,4,3,source.png,expected.png,mask.png,custom-out\n",
        )
        .unwrap();

        let jobs = read_production_sample_jobs(&samples, &output_root).unwrap();

        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.sample, "alpha");
        assert_eq!((job.region_x, job.region_z), (1, -2));
        assert_eq!(
            (job.crop_x, job.crop_y, job.crop_width, job.crop_height),
            (0, 1, 4, 3)
        );
        assert_eq!(
            normalized_path_display(&job.source),
            normalized_path_display(&source)
        );
        assert_eq!(
            normalized_path_display(&job.expected_standard),
            normalized_path_display(&expected)
        );
        assert_eq!(
            job.land_mask.as_deref().map(normalized_path_display),
            Some(normalized_path_display(&mask))
        );
        assert_eq!(job.mask_mode, "land");
        assert_eq!(job.output_directory, output_root.join("custom-out"));
    }

    #[test]
    fn quality_production_sample_batch_rejects_linear_before_generation() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.png");
        let expected = temp.path().join("expected.png");
        write_metric_test_png(&source, false);
        write_metric_test_png(&expected, false);
        let samples = temp.path().join("samples.csv");
        fs::write(
            &samples,
            format!(
                "sample,regionX,regionZ,cropX,cropY,cropWidth,cropHeight,sourcePng,expectedStandardPng\n\
alpha,0,0,0,0,4,4,{},{}\n",
                source.display(),
                expected.display()
            ),
        )
        .unwrap();

        let (code, out, err) = run_capture(&[
            "quality-production-sample-batch",
            samples.to_str().unwrap(),
            "missing-heightmap.tif",
            temp.path().join("out").to_str().unwrap(),
            "1000",
            "linear",
            "1",
            "textureMode=classified",
            "surfaceRaster=none",
        ]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert!(err
            .contains("quality-production-sample-batch currently requires mca for topdown parity"));
    }

    #[test]
    #[ignore = "slow full-region quality smoke; run when changing quality-production-sample-batch generation"]
    fn quality_production_sample_batch_generates_artifacts_without_java() {
        let temp = tempdir().unwrap();
        let heightmap = temp.path().join("height.tif");
        fs::write(&heightmap, synthetic_bigtiff_heightmap()).unwrap();
        let source = temp.path().join("source.png");
        let expected = temp.path().join("expected.png");
        write_metric_test_png(&source, false);
        write_metric_test_png(&expected, false);
        let samples = temp.path().join("samples.csv");
        fs::write(
            &samples,
            format!(
                "sample,regionX,regionZ,cropX,cropY,cropWidth,cropHeight,sourcePng,expectedStandardPng\n\
alpha,0,0,0,0,4,4,{},{}\n",
                source.display(),
                expected.display()
            ),
        )
        .unwrap();
        let output_root = temp.path().join("quality-out");

        let (code, out, err) = run_capture(&[
            "quality-production-sample-batch",
            samples.to_str().unwrap(),
            heightmap.to_str().unwrap(),
            output_root.to_str().unwrap(),
            "1000",
            "mca",
            "1",
            "cacheRows=2",
            "prefetchRows=0",
            "textureMode=classified",
            "surfaceRaster=none",
            "chunkStatus=surface",
            "metricMode=current-only",
            "previewDebug=auto",
        ]);

        assert_eq!(code, EXIT_OK, "stderr={err}");
        assert!(err.is_empty());
        assert!(out.contains("Quality production sample batch complete\n"));
        assert!(out.contains("execution=single-rust-sequential\n"));
        assert!(output_root
            .join("quality-production-sample-summary.csv")
            .is_file());
        assert!(output_root
            .join("quality-production-sample-contact-sheet.png")
            .is_file());
        let sample_root = output_root.join("alpha");
        assert!(sample_root
            .join("world")
            .join("region")
            .join("r.0.0.mca")
            .is_file());
        assert!(sample_root
            .join("photo-parity")
            .join("mca-visible-topdown.png")
            .is_file());
        assert!(sample_root
            .join("photo-parity")
            .join("metric-land")
            .join("metrics.txt")
            .is_file());
        assert!(sample_root
            .join("quality-production-sample.properties")
            .is_file());
        assert!(sample_root
            .join("quality-production-sample-evidence.json")
            .is_file());
        assert!(sample_root.join("preview-debug").is_dir());
    }

    #[test]
    fn inspect_mca_post_final_integrity_reports_water_tree_and_coast_fixture() {
        let temp = tempdir().unwrap();
        let region = temp.path().join("r.0.0.mca");
        write_post_final_integrity_fixture_region(&region, RegionFormat::Mca);

        let (code, out, err) =
            run_capture(&["inspect-mca-post-final-integrity", region.to_str().unwrap()]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA post-final integrity scanned\n"));
        assert!(out.contains("regionChunkCount=1\n"));
        assert!(out.contains("decodedChunkCount=1\n"));
        assert!(out.contains("scannedColumns=256\n"));
        assert!(out.contains("landColumns=1\n"));
        assert!(out.contains("waterColumns=255\n"));
        assert!(out.contains("dryBelowSeaColumns=0\n"));
        assert!(out.contains("underwaterAirColumns=1\n"));
        assert!(out.contains("treeLogBlocks=1\n"));
        assert!(out.contains("treeLeafBlocks=1\n"));
        assert!(out.contains("treeLogColumns=1\n"));
        assert!(out.contains("treeLeafColumns=1\n"));
        assert!(out.contains("topTerrainBlockHits.minecraft:grass_block=1\n"));
        assert!(out.contains("topTerrainBlockHits.minecraft:stone=255\n"));
        assert!(out.contains("coastEdgeSamples="));
        assert!(out.contains("maxCoastLandAboveSeaDelta=12\n"));
    }

    #[test]
    fn inspect_linear_post_final_integrity_reads_linear_region() {
        let temp = tempdir().unwrap();
        let region = temp.path().join("r.0.0.linear");
        write_post_final_integrity_fixture_region(&region, RegionFormat::Linear);

        let (code, out, err) = run_capture(&[
            "inspect-linear-post-final-integrity",
            region.to_str().unwrap(),
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Linear post-final integrity scanned\n"));
        assert!(out.contains("regionChunkCount=1\n"));
        assert!(out.contains("decodedChunkCount=1\n"));
        assert!(out.contains("scannedColumns=256\n"));
        assert!(out.contains("underwaterAirColumns=1\n"));
        assert!(out.contains("topTerrainBlockHits.minecraft:grass_block=1\n"));
    }

    #[test]
    fn rewrite_mca_status_updates_world_regions_and_is_idempotent() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("flat-mca");
        generate_single_chunk_render_world(&world, TopdownFormat::Mca);
        let region = world.join("region").join("r.0.0.mca");

        let (code, out, err) =
            run_capture(&["rewrite-mca-status", world.to_str().unwrap(), "surface"]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA chunk statuses rewritten\n"));
        assert!(out.contains("targetStatus=minecraft:surface\n"));
        assert!(out.contains("regionFileCount=1\n"));
        assert!(out.contains("chunkCount=1\n"));
        assert!(out.contains("rewrittenRegionCount=1\n"));
        assert!(out.contains("rewrittenChunkCount=1\n"));

        let (code, out, err) =
            run_capture(&["summarize-region-chunk", region.to_str().unwrap(), "0", "0"]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("status=minecraft:surface\n"));
        assert!(out.contains("isLightOn=0\n"));

        let (code, out, err) =
            run_capture(&["rewrite-mca-status", region.to_str().unwrap(), "surface"]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("rewrittenRegionCount=0\n"));
        assert!(out.contains("rewrittenChunkCount=0\n"));
    }

    #[test]
    fn repair_mca_post_final_water_fills_only_underwater_air() {
        let temp = tempdir().unwrap();
        let region = temp.path().join("r.0.0.mca");
        write_post_final_integrity_fixture_region(&region, RegionFormat::Mca);

        let (code, out, err) =
            run_capture(&["inspect-mca-post-final-integrity", region.to_str().unwrap()]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("underwaterAirColumns=1\n"));

        let (code, out, err) =
            run_capture(&["repair-mca-post-final-water", region.to_str().unwrap()]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA post-final water repaired\n"));
        assert!(out.contains("regionFileCount=1\n"));
        assert!(out.contains("chunkCount=1\n"));
        assert!(out.contains("repairedRegionCount=1\n"));
        assert!(out.contains("repairedChunkCount=1\n"));
        assert!(out.contains("repairedColumnCount=1\n"));
        assert!(out.contains("filledBlockCount=2\n"));
        assert!(out.contains("changed=true\n"));

        let (code, out, err) =
            run_capture(&["inspect-mca-post-final-integrity", region.to_str().unwrap()]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("underwaterAirColumns=0\n"));
        assert!(out.contains("waterColumns=255\n"));
    }

    #[test]
    fn repair_linear_sandlike_surfaces_rewrites_only_sandlike_palette_entries() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("linear-world");
        let region = world.join("region").join("r.0.0.linear");
        write_linear_sandlike_repair_fixture(&region);

        let (code, out, err) =
            run_capture(&["repair-linear-sandlike-surfaces", world.to_str().unwrap()]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Linear sandlike surfaces repaired\n"));
        assert!(out.contains("regionFileCount=1\n"));
        assert!(out.contains("chunkCount=1\n"));
        assert!(out.contains("repairedRegionCount=1\n"));
        assert!(out.contains("repairedChunkCount=1\n"));
        assert!(out.contains("replacedBlockCount=3\n"));
        assert!(out.contains("changed=true\n"));

        let (code, out, err) = run_capture(&["inspect-linear-palettes", region.to_str().unwrap()]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(!out.contains("paletteHits.minecraft:sand="));
        assert!(out.contains("paletteHits.minecraft:clay="));
        assert!(out.contains("paletteHits.minecraft:grass_block="));
        assert!(out.contains("paletteHits.minecraft:dirt="));

        let (code, out, err) =
            run_capture(&["repair-linear-sandlike-surfaces", region.to_str().unwrap()]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("repairedRegionCount=0\n"));
        assert!(out.contains("repairedChunkCount=0\n"));
        assert!(out.contains("replacedBlockCount=0\n"));
        assert!(out.contains("changed=false\n"));
    }

    #[test]
    fn convert_mca_region_to_linear_writes_matching_payloads() {
        let temp = tempdir().unwrap();
        let mca_world = temp.path().join("flat-mca");
        generate_flat_test_world_impl(mca_world.to_str().unwrap(), "mca").unwrap();
        let mca_region = mca_world.join("region").join("r.0.0.mca");
        let linear_region = temp.path().join("converted").join("r.0.0.linear");

        let (code, out, err) = run_capture(&[
            "convert-mca-region-to-linear",
            mca_region.to_str().unwrap(),
            linear_region.to_str().unwrap(),
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA region converted to Linear V2\n"));
        assert!(out.contains("chunkCount=1024\n"));
        assert!(out.contains("linearChunkCount=1024\n"));
        assert!(linear_region.is_file());

        let comparison = compare_mca_linear_region_payloads(&mca_region, &linear_region).unwrap();
        assert!(comparison.matches());
        assert_eq!(comparison.compared_chunks, REGION_CHUNKS_PER_REGION);
    }

    #[test]
    fn convert_mca_world_to_linear_copies_metadata_and_regions() {
        let temp = tempdir().unwrap();
        let mca_world = temp.path().join("flat-mca");
        let linear_world = temp.path().join("flat-linear");
        generate_flat_test_world_impl(mca_world.to_str().unwrap(), "mca").unwrap();
        fs::write(mca_world.join("earthmap-region-progress.csv"), b"progress").unwrap();

        let (code, out, err) = run_capture(&[
            "convert-mca-world-to-linear",
            mca_world.to_str().unwrap(),
            linear_world.to_str().unwrap(),
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA world converted to Linear V2\n"));
        assert!(out.contains("regions=1\n"));
        assert!(out.contains("chunks=1024\n"));
        assert!(linear_world.join("level.dat").is_file());
        assert!(linear_world.join(SURVIVAL_MANIFEST_FILE_NAME).is_file());
        assert!(linear_world.join("earthmap-region-progress.csv").is_file());
        assert!(linear_world.join("region").join("r.0.0.linear").is_file());
    }

    #[test]
    fn inspect_mca_region_scanners_report_flat_fixture() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("flat-mca");
        generate_flat_test_world_impl(world.to_str().unwrap(), "mca").unwrap();
        let region = world.join("region").join("r.0.0.mca");
        let region = region.to_str().unwrap();

        let (code, out, err) = run_capture(&["inspect-mca-palettes", region]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA palettes scanned\n"));
        assert!(out.contains("regionChunkCount=1024\n"));
        assert!(out.contains("decodedChunkCount=1024\n"));
        assert!(out.contains("paletteHits.minecraft:grass_block="));

        let (code, out, err) = run_capture(&["validate-mca-survival-palette", region]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.is_empty());
        assert!(out.contains("MCA survival palette scanned\n"));
        assert!(out.contains("regionChunkCount=1024\n"));
        assert!(out.contains("has.deepslate=false\n"));
        assert!(out.contains("has.ore.coal=false\n"));
        assert!(out.contains("survivalCriticalComplete=false\n"));

        let (code, out, err) = run_capture(&["inspect-mca-biomes", region]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA biomes scanned\n"));
        assert!(out.contains("regionChunkCount=1024\n"));
        assert!(out.contains("biomePaletteEntryCount="));
        assert!(out.contains("biomePaletteHits."));

        let (code, out, err) = run_capture(&["inspect-mca-statuses", region]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("MCA statuses scanned\n"));
        assert!(out.contains("regionChunkCount=1024\n"));
        assert!(out.contains("decodedChunkCount=1024\n"));
        assert!(out.contains("status.minecraft:full=1024\n"));
    }

    #[test]
    fn inspect_linear_region_scanners_report_flat_fixture() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("flat-linear");
        generate_flat_test_world_impl(world.to_str().unwrap(), "linear").unwrap();
        let region = world.join("region").join("r.0.0.linear");
        let region = region.to_str().unwrap();

        let (code, out, err) = run_capture(&["inspect-linear-palettes", region]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Linear palettes scanned\n"));
        assert!(out.contains("regionChunkCount=1024\n"));
        assert!(out.contains("decodedChunkCount=1024\n"));
        assert!(out.contains("paletteHits.minecraft:grass_block="));

        let (code, out, err) = run_capture(&["validate-linear-survival-palette", region]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.is_empty());
        assert!(out.contains("Linear survival palette scanned\n"));
        assert!(out.contains("regionChunkCount=1024\n"));
        assert!(out.contains("has.deepslate=false\n"));
        assert!(out.contains("has.ore.coal=false\n"));
        assert!(out.contains("survivalCriticalComplete=false\n"));

        let (code, out, err) = run_capture(&["inspect-linear-biomes", region]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Linear biomes scanned\n"));
        assert!(out.contains("regionChunkCount=1024\n"));
        assert!(out.contains("biomePaletteEntryCount="));
        assert!(out.contains("biomePaletteHits."));

        let (code, out, err) = run_capture(&["inspect-linear-statuses", region]);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Linear statuses scanned\n"));
        assert!(out.contains("regionChunkCount=1024\n"));
        assert!(out.contains("decodedChunkCount=1024\n"));
        assert!(out.contains("status.minecraft:full=1024\n"));
    }

    #[test]
    fn flat_test_chunk_matches_java_column_shape() {
        let chunk = flat_test_chunk(3, 5).unwrap();

        assert_eq!(chunk.chunk_x(), 3);
        assert_eq!(chunk.chunk_z(), 5);
        assert_eq!(
            chunk.get_block_state_id(0, -64, 0).unwrap(),
            block_state_ids::BEDROCK
        );
        assert_eq!(
            chunk.get_block_state_id(0, -63, 0).unwrap(),
            block_state_ids::STONE
        );
        assert_eq!(
            chunk
                .get_block_state_id(15, FLAT_TEST_SURFACE_Y - 4, 15)
                .unwrap(),
            block_state_ids::STONE
        );
        assert_eq!(
            chunk
                .get_block_state_id(15, FLAT_TEST_SURFACE_Y - 3, 15)
                .unwrap(),
            block_state_ids::DIRT
        );
        assert_eq!(
            chunk
                .get_block_state_id(15, FLAT_TEST_SURFACE_Y - 1, 15)
                .unwrap(),
            block_state_ids::DIRT
        );
        assert_eq!(
            chunk
                .get_block_state_id(15, FLAT_TEST_SURFACE_Y, 15)
                .unwrap(),
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            chunk
                .get_block_state_id(15, FLAT_TEST_SURFACE_Y + 1, 15)
                .unwrap(),
            block_state_ids::AIR
        );
    }

    #[test]
    fn summarize_region_chunk_reports_flat_fixture_palettes() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("flat");
        generate_flat_test_world_impl(world.to_str().unwrap(), "linear").unwrap();
        let region = world.join("region").join("r.0.0.linear");

        let (code, out, err) =
            run_capture(&["summarize-region-chunk", region.to_str().unwrap(), "0", "0"]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("regionChunkSummary=valid\n"));
        assert!(out.contains("format=linear\n"));
        assert!(out.contains("localChunkX=0\n"));
        assert!(out.contains("localChunkZ=0\n"));
        assert!(out.contains("status=minecraft:full\n"));
        assert!(out.contains(
            "section.3.blockPalette=minecraft:stone|minecraft:dirt|minecraft:grass_block\n"
        ));
    }

    #[test]
    fn compare_region_chunk_details_reports_no_flat_fixture_diffs() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("flat");
        generate_flat_test_world_impl(world.to_str().unwrap(), "linear").unwrap();
        let region = world.join("region").join("r.0.0.linear");

        let (code, out, err) = run_capture(&[
            "compare-region-chunk-details",
            region.to_str().unwrap(),
            region.to_str().unwrap(),
            "0",
            "0",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("regionChunkDetails=valid\n"));
        assert!(out.contains("heightmap.OCEAN_FLOOR.diffCount=0\n"));
        assert!(out.contains("biomeCell.diffCount=0\n"));
        assert!(out.contains("block.diffCount=0\n"));
    }

    #[test]
    fn compare_region_chunk_details_counts_asymmetric_section_sets() {
        let mut expected_sections = BTreeMap::new();
        expected_sections.insert(
            0,
            decoded_test_section("minecraft:stone", "minecraft:plains"),
        );
        let expected = DecodedChunkDetails {
            heightmaps: BTreeMap::new(),
            sections: expected_sections,
        };

        let mut actual_sections = BTreeMap::new();
        actual_sections.insert(1, decoded_test_section("minecraft:dirt", "minecraft:ocean"));
        let actual = DecodedChunkDetails {
            heightmaps: BTreeMap::new(),
            sections: actual_sections,
        };

        let biome = compare_biome_cells(&expected, &actual, 48);
        assert!(biome.contains(&format!(
            "biomeCell.diffCount={}",
            SECTION_BIOME_CELL_COUNT * 2
        )));
        assert!(biome.contains(&"biomeCell.section.0.missingActual=true".to_string()));
        assert!(biome.contains(&"biomeCell.section.1.missingExpected=true".to_string()));

        let block = compare_block_cells(&expected, &actual, 48);
        assert!(block.contains(&format!("block.diffCount={}", SECTION_BLOCK_COUNT * 2)));
        assert!(block.contains(&"block.section.0.missingActual=true".to_string()));
        assert!(block.contains(&"block.section.1.missingExpected=true".to_string()));
    }

    fn decoded_test_section(block: &str, biome: &str) -> DecodedSectionDetails {
        DecodedSectionDetails {
            block_palette: vec![block.to_string()],
            block_values: vec![0; SECTION_BLOCK_COUNT],
            biome_palette: vec![biome.to_string()],
            biome_values: vec![0; SECTION_BIOME_CELL_COUNT],
        }
    }

    #[test]
    fn summarize_region_chunk_rejects_malformed_palette_data_type() {
        let mut missing = nbt::Compound::new();
        assert_eq!(long_array_len(&missing, "data").unwrap(), 0);

        missing.put_long_array("data", vec![1, 2, 3]).unwrap();
        assert_eq!(long_array_len(&missing, "data").unwrap(), 3);

        let mut malformed = nbt::Compound::new();
        malformed.put_int("data", 7).unwrap();
        let error = long_array_len(&malformed, "data").unwrap_err();

        assert!(error.contains("data must be a long array"));
    }

    #[test]
    fn palette_stress_chunk_matches_java_fixture_shape() {
        let chunk = palette_stress_chunk().unwrap();

        assert_eq!(chunk.chunk_x(), 0);
        assert_eq!(chunk.chunk_z(), 0);
        assert_eq!(
            chunk.get_block_state_id(0, -64, 0).unwrap(),
            block_state_ids::BEDROCK
        );
        assert_eq!(
            chunk.get_block_state_id(0, -63, 0).unwrap(),
            block_state_ids::STONE
        );
        assert_eq!(
            chunk
                .get_block_state_id(15, PALETTE_STRESS_SURFACE_Y - 4, 15)
                .unwrap(),
            block_state_ids::STONE
        );
        assert_eq!(
            chunk
                .get_block_state_id(15, PALETTE_STRESS_SURFACE_Y - 3, 15)
                .unwrap(),
            block_state_ids::DIRT
        );
        assert_eq!(
            chunk
                .get_block_state_id(15, PALETTE_STRESS_SURFACE_Y, 15)
                .unwrap(),
            block_state_ids::GRASS_BLOCK
        );
        assert_eq!(
            chunk.get_block_state_id(15, PALETTE_STRESS_Y, 15).unwrap(),
            block_state_ids::AIR
        );

        assert_eq!(PALETTE_STRESS_BLOCK_STATE_IDS.len(), 20);
        assert_eq!(PALETTE_STRESS_SECTION_Y, 4);
        assert_eq!(PALETTE_STRESS_EXPECTED_SECTION_DATA_LONGS, 342);
        for (index, block_state_id) in PALETTE_STRESS_BLOCK_STATE_IDS.iter().enumerate() {
            let local_x = (index % CHUNK_WIDTH) as i32;
            let local_z = (index / CHUNK_WIDTH) as i32;
            assert_eq!(
                chunk
                    .get_block_state_id(local_x, PALETTE_STRESS_Y, local_z)
                    .unwrap(),
                *block_state_id,
                "stress block index={index}"
            );
        }
    }

    #[test]
    fn java_double_string_matches_java_style_edges() {
        assert_eq!(java_double_string(90.0), "90.0");
        assert_eq!(java_double_string(0.001), "0.001");
        assert_eq!(
            java_double_string(0.0008332458866155434),
            "8.332458866155434E-4"
        );
        assert_eq!(java_double_string(0.0001), "1.0E-4");
        assert_eq!(java_double_string(10_000_000.0), "1.0E7");
        assert_eq!(java_double_string(f64::INFINITY), "Infinity");
        assert_eq!(java_double_string(f64::from_bits(1)), "4.9E-324");
        assert_eq!(java_double_string(-f64::from_bits(1)), "-4.9E-324");
    }

    #[test]
    fn java_double_parser_matches_cli_compatibility_edges() {
        assert!(parse_java_double("nan").is_err());
        assert!(parse_java_double("inf").is_err());
        assert!(parse_java_double("NaN").unwrap().is_nan());
        assert_eq!(parse_java_double("Infinity").unwrap(), f64::INFINITY);
        assert_eq!(parse_java_double("-Infinity").unwrap(), f64::NEG_INFINITY);
        assert_eq!(parse_java_double("0x1.0p0").unwrap(), 1.0);
        assert_eq!(parse_java_double("-0x1.8p1").unwrap(), -3.0);
        assert_eq!(parse_java_double("1.0d").unwrap(), 1.0);
        assert_eq!(parse_java_double("1.0F").unwrap(), 1.0);
    }

    #[test]
    fn inspect_heightmap_matches_java_stdout_contract() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-cli-heightmap.tif");
        fs::write(&path, synthetic_bigtiff_heightmap()).unwrap();

        let (code, out, err) = run_capture(&["inspect-heightmap", path.to_str().unwrap()]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Heightmap GeoTIFF valid\n"));
        assert!(out.contains(&format!("path={}\n", path.display())));
        assert!(out.contains("width=3\n"));
        assert!(out.contains("height=3\n"));
        assert!(out.contains("sampleType=Int16\n"));
        assert!(out.contains("compression=1\n"));
        assert!(out.contains("rowsPerStrip=1\n"));
        assert!(out.contains("samplesPerPixel=1\n"));
        assert!(out.contains("epsg=NONE\n"));
        assert!(out.contains("topLeftLongitude=0.0\n"));
        assert!(out.contains("topLeftLatitude=3.0\n"));
        assert!(out.contains("pixelWidthDegrees=1.0\n"));
        assert!(out.contains("pixelHeightDegrees=1.0\n"));
        assert!(out.contains("noData=NONE\n"));
    }

    #[test]
    fn locate_heightmap_point_matches_java_stdout_contract() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-cli-locate.tif");
        fs::write(&path, synthetic_bigtiff_heightmap()).unwrap();

        let (code, out, err) = run_capture(&[
            "locate-heightmap-point",
            path.to_str().unwrap(),
            "1000",
            "0.0",
            "1.0",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert_eq!(
            out,
            "Heightmap point located\n\
scale=1:1000\n\
longitude=0.0\n\
latitude=1.0\n\
mapBlockX=20037\n\
mapBlockZ=222\n\
globalBlockX=0\n\
globalBlockZ=55\n\
regionX=0\n\
regionZ=0\n\
localBlockX=0\n\
localBlockZ=55\n"
        );
    }

    #[test]
    fn locate_heightmap_point_uses_java_double_parse_edges() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-cli-locate-java-double.tif");
        fs::write(&path, synthetic_bigtiff_heightmap()).unwrap();

        let (code, out, err) = run_capture(&[
            "locate-heightmap-point",
            path.to_str().unwrap(),
            "1000",
            "0x0.0p0",
            "1.0d",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("longitude=0.0\n"));
        assert!(out.contains("latitude=1.0\n"));

        let (code, out, err) = run_capture(&[
            "locate-heightmap-point",
            path.to_str().unwrap(),
            "1000",
            "nan",
            "1.0",
        ]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert_eq!(
            err,
            "Heightmap point location failed: For input string: \"nan\"\n"
        );
    }

    #[test]
    fn classify_surface_point_matches_java_stdout_contract_shape() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tiny-cli-classify.tif");
        fs::write(&path, synthetic_bigtiff_heightmap()).unwrap();

        let args = [
            "classify-surface-point",
            path.to_str().unwrap(),
            "1000",
            "0.0",
            "1.0",
        ];
        let (code, out, err) = run_capture(&args);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        let expected = classify_surface_point_impl(path.to_str().unwrap(), "1000", "0.0", "1.0")
            .unwrap()
            .join("\n")
            + "\n";
        assert_eq!(out, expected);
        assert!(out.contains("Surface point classified\n"));
        assert!(out.contains("scale=1:1000\n"));
        assert!(out.contains("longitude=0.0\n"));
        assert!(out.contains("latitude=1.0\n"));
        assert!(out.contains("sampledLongitude="));
        assert!(out.contains("sampledLatitude="));
        assert!(out.contains("elevationMeters="));
        assert!(out.contains("water="));
        assert!(out.contains("groundSurfaceY="));
        assert!(out.contains("waterSurfaceY="));
        assert!(out.contains("topBlockStateId="));
        assert!(out.contains("fillerBlockStateId="));
        assert!(out.contains("biome="));
    }

    #[test]
    fn sample_vrt_rgb_matches_java_fixture_stdout_contract() {
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

        let (code, out, err) =
            run_capture(&["sample-vrt-rgb", vrt.to_str().unwrap(), "1.25", "0.75"]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert_eq!(
            out,
            format!(
                "VRT RGB sampled\n\
path={}\n\
longitude=1.25\n\
latitude=0.75\n\
mode=nearest\n\
available=true\n\
red=100\n\
green=110\n\
blue=120\n\
sourceCount=1\n\
openReaders=1\n\
residentTiles=1\n\
tileHits=0\n\
tileMisses=1\n\
tileEvictions=0\n\
sampleNearestRequests=1\n\
sampleAveragedRequests=0\n\
indexedSourceLookup=true\n\
sourceLookupCells=1\n",
                vrt.display()
            )
        );
    }

    #[test]
    fn generate_height_region_rejects_unknown_format_like_java() {
        let (code, out, err) = run_capture(&[
            "generate-height-region",
            "height.tif",
            "world",
            "5000",
            "0",
            "0",
            "bogus",
        ]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert_eq!(
            err,
            "Height-only region generation failed: format must be mca or linear\n"
        );
    }

    #[test]
    fn generate_surface_region_rejects_unknown_format_like_java() {
        let (code, out, err) = run_capture(&[
            "generate-surface-region",
            "height.tif",
            "world",
            "5000",
            "0",
            "0",
            "bogus",
        ]);

        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert_eq!(
            err,
            "Surface region generation failed: format must be mca or linear\n"
        );
    }

    #[test]
    fn surface_region_report_lines_match_java_stdout_shape() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("surface-world");
        let report = SurfaceRegionReport {
            region_x: 0,
            region_z: -1,
            output_format: OutputFormat::LinearV2,
            scale_denominator: 5000,
            vertical_scale: DEFAULT_VERTICAL_SCALE,
            chunk_count: 1024,
            land_columns: 200_000,
            water_columns: 62_144,
            min_ground_y: 54,
            max_ground_y: 91,
            region_file: world.join("region").join("r.0.-1.linear"),
            preview_tile_file: None,
            cache_stats: earthmap_geo::GeoTiffRowCacheStats {
                max_rows: 64,
                resident_rows: 12,
                hits: 34,
                misses: 56,
                evictions: 7,
                prefetch_rows: 0,
                prefetch_requests: 0,
                prefetch_loads: 0,
            },
            surface_material_raster_stats: earthmap_surface::SurfaceMaterialRasterStats::EMPTY,
            surface_sample_nanos: 2_900_000,
            sample_phase_nanos: earthmap_surface::SurfaceRegionSamplePhaseNanos::default(),
            chunk_build_nanos: 3_100_000,
            nbt_encode_nanos: 4_200_000,
            region_write_nanos: 5_300_000,
            preview_nanos: 0,
            metadata_nanos: 6_400_000,
            total_nanos: 21_900_000,
        };

        let out = surface_region_report_lines(&report, world.to_str().unwrap()).join("\n") + "\n";

        assert_eq!(
            out,
            format!(
                "Surface region generated\n\
regionX=0\n\
regionZ=-1\n\
format=LINEAR_V2\n\
scale=1:5000\n\
chunkCount=1024\n\
landColumns=200000\n\
waterColumns=62144\n\
minGroundY=54\n\
maxGroundY=91\n\
regionFile={}\n\
cacheMaxRows=64\n\
cacheResidentRows=12\n\
cacheHits=34\n\
cacheMisses=56\n\
cacheEvictions=7\n\
surfaceRasterSourceCount=0\n\
surfaceRasterOpenReaders=0\n\
surfaceRasterResidentTiles=0\n\
surfaceRasterTileHits=0\n\
surfaceRasterTileMisses=0\n\
surfaceRasterTileEvictions=0\n\
phase.surfaceSampleMillis=2\n\
phase.chunkBuildMillis=3\n\
phase.nbtEncodeMillis=4\n\
phase.regionWriteMillis=5\n\
phase.previewMillis=0\n\
phase.metadataMillis=6\n\
phase.totalInternalMillis=21\n\
manifestFile={}\n",
                world.join("region").join("r.0.-1.linear").display(),
                world.join(SURVIVAL_MANIFEST_FILE_NAME).display()
            )
        );
    }

    #[test]
    fn vanilla_delegated_region_report_lines_match_java_stdout_shape() {
        let temp = tempdir().unwrap();
        let world = temp.path().join("vanilla-world");
        let material = temp
            .path()
            .join("TifFiles")
            .join("terrain")
            .join("TrueMarble.vrt");
        fs::create_dir_all(material.parent().unwrap()).unwrap();
        fs::write(&material, b"vrt").unwrap();
        let report = SurfaceRegionReport {
            region_x: 0,
            region_z: -1,
            output_format: OutputFormat::LinearV2,
            scale_denominator: 5000,
            vertical_scale: DEFAULT_VERTICAL_SCALE,
            chunk_count: 1024,
            land_columns: 200_000,
            water_columns: 62_144,
            min_ground_y: 54,
            max_ground_y: 91,
            region_file: world.join("region").join("r.0.-1.linear"),
            preview_tile_file: None,
            cache_stats: earthmap_geo::GeoTiffRowCacheStats {
                max_rows: 512,
                resident_rows: 42,
                hits: 123,
                misses: 456,
                evictions: 7,
                prefetch_rows: 0,
                prefetch_requests: 0,
                prefetch_loads: 0,
            },
            surface_material_raster_stats: earthmap_surface::SurfaceMaterialRasterStats::EMPTY,
            surface_sample_nanos: 2_900_000,
            sample_phase_nanos: earthmap_surface::SurfaceRegionSamplePhaseNanos::default(),
            chunk_build_nanos: 3_100_000,
            nbt_encode_nanos: 4_200_000,
            region_write_nanos: 5_300_000,
            preview_nanos: 0,
            metadata_nanos: 6_400_000,
            total_nanos: 21_900_000,
        };

        let out = vanilla_delegated_region_report_lines(
            &report,
            world.to_str().unwrap(),
            ChunkGenerationStatus::Surface,
            &material,
            RegionCompressionOptions::default(),
            "auto",
        )
        .join("\n")
            + "\n";

        assert_eq!(
            out,
            format!(
                "Vanilla-delegated surface region generated\n\
regionX=0\n\
regionZ=-1\n\
format=LINEAR_V2\n\
scale=1:5000\n\
verticalScale=1\n\
verticalScaleMode=auto\n\
chunkStatus=minecraft:surface\n\
linearCompression=default\n\
surfaceMaterialPath={}\n\
serverDelegation=true\n\
directCaves=false\n\
directOres=false\n\
directVegetation=false\n\
directStructures=false\n\
progressionStrategy=none\n\
directProgressionStructures=false\n\
chunkCount=1024\n\
landColumns=200000\n\
waterColumns=62144\n\
minGroundY=54\n\
maxGroundY=91\n\
regionFile={}\n\
cacheMaxRows=512\n\
cacheResidentRows=42\n\
cacheHits=123\n\
cacheMisses=456\n\
cacheEvictions=7\n\
surfaceRasterSourceCount=0\n\
surfaceRasterOpenReaders=0\n\
surfaceRasterResidentTiles=0\n\
surfaceRasterTileHits=0\n\
surfaceRasterTileMisses=0\n\
surfaceRasterTileEvictions=0\n\
phase.surfaceSampleMillis=2\n\
phase.chunkBuildMillis=3\n\
phase.nbtEncodeMillis=4\n\
phase.regionWriteMillis=5\n\
phase.previewMillis=0\n\
phase.metadataMillis=6\n\
phase.totalInternalMillis=21\n\
manifestFile={}\n",
                normalized_path_display(&material),
                world.join("region").join("r.0.-1.linear").display(),
                world.join(SURVIVAL_MANIFEST_FILE_NAME).display()
            )
        );
    }

    #[test]
    fn surface_material_auto_detect_prefers_enhanced_true_marble_candidate() {
        let temp = tempdir().unwrap();
        let heightmap_dir = temp.path().join("heightmaps");
        fs::create_dir_all(&heightmap_dir).unwrap();
        let heightmap = heightmap_dir.join("HQheightmap.tif");
        fs::write(&heightmap, b"height").unwrap();
        let plain = heightmap_dir.join("terrain").join("TrueMarble.vrt");
        fs::create_dir_all(plain.parent().unwrap()).unwrap();
        fs::write(&plain, b"plain").unwrap();
        let enhanced_root = temp.path().join("TifFiles");
        let enhanced = enhanced_root.join("terrain").join("TrueMarble.vrt");
        fs::create_dir_all(enhanced.parent().unwrap()).unwrap();
        fs::write(&enhanced, b"enhanced").unwrap();
        fs::write(enhanced_root.join("land_shallow_topo_west.tif"), b"west").unwrap();
        fs::write(enhanced_root.join("land_shallow_topo_east.tif"), b"east").unwrap();

        assert_eq!(
            parse_optional_surface_material_path("surfaceRaster=auto", &heightmap).unwrap(),
            Some(normalized_path(&enhanced))
        );
        assert_eq!(
            parse_optional_surface_material_path("none", &heightmap).unwrap(),
            None
        );
    }

    #[test]
    fn java_display_path_strips_windows_extended_length_prefixes() {
        assert_eq!(
            java_display_path(Path::new(
                r"\\?\X:\earthmap-example\TifFiles\terrain\TrueMarble.vrt"
            )),
            r"X:\earthmap-example\TifFiles\terrain\TrueMarble.vrt"
        );
        assert_eq!(
            java_display_path(Path::new(r"\\?\UNC\server\share\TrueMarble.vrt")),
            r"\\server\share\TrueMarble.vrt"
        );
        assert_eq!(
            java_display_path(Path::new(
                r"X:\earthmap-example\TifFiles\terrain\TrueMarble.vrt"
            )),
            r"X:\earthmap-example\TifFiles\terrain\TrueMarble.vrt"
        );
    }

    #[test]
    #[ignore = "slow full-region smoke; run explicitly when touching region generation"]
    fn generate_surface_region_writes_stdout_manifest_and_region_file() {
        let temp = tempdir().unwrap();
        let heightmap = temp.path().join("tiny-cli-surface-region.tif");
        fs::write(&heightmap, synthetic_bigtiff_heightmap()).unwrap();
        let world = temp.path().join("surface-world");

        let (code, out, err) = run_capture(&[
            "generate-surface-region",
            heightmap.to_str().unwrap(),
            world.to_str().unwrap(),
            "1000",
            "0",
            "0",
            "linear",
        ]);

        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        assert!(out.contains("Surface region generated\n"));
        assert!(out.contains("regionX=0\n"));
        assert!(out.contains("regionZ=0\n"));
        assert!(out.contains("format=LINEAR_V2\n"));
        assert!(out.contains("scale=1:1000\n"));
        assert!(out.contains("chunkCount=1024\n"));
        assert!(out.contains("landColumns="));
        assert!(out.contains("waterColumns="));
        assert!(out.contains("minGroundY="));
        assert!(out.contains("maxGroundY="));
        assert!(out.contains("phase.surfaceSampleMillis="));
        assert!(out.contains("phase.chunkBuildMillis="));
        assert!(out.contains("phase.nbtEncodeMillis="));
        assert!(out.contains("phase.regionWriteMillis="));
        assert!(out.contains("phase.previewMillis=0\n"));
        assert!(out.contains("phase.metadataMillis="));
        assert!(out.contains("phase.totalInternalMillis="));
        assert!(out.contains(&format!(
            "manifestFile={}\n",
            world.join(SURVIVAL_MANIFEST_FILE_NAME).display()
        )));
        assert!(world.join("level.dat").is_file());
        assert!(world.join("region").join("r.0.0.linear").is_file());

        let manifest = fs::read_to_string(world.join(SURVIVAL_MANIFEST_FILE_NAME)).unwrap();
        assert!(manifest.contains("generator.name=surface-region\n"));
        assert!(manifest.contains("features.surfaceRules=true\n"));
        assert!(manifest.contains("features.waterSurface=true\n"));
        assert!(manifest.contains("features.biomes=heuristic\n"));
        assert!(manifest.contains("features.surfaceMaterialRaster=false\n"));
        assert!(manifest.contains("features.serverDelegation=false\n"));
        assert!(manifest.contains("generation.chunkStatus=minecraft:full\n"));
        assert!(manifest.contains("generation.textureMode=photo\n"));
        assert!(manifest.contains("generation.verticalScale=1.0\n"));
        assert!(manifest.contains("generation.directCaves=false\n"));
        assert!(manifest.contains("generation.directOres=false\n"));
        assert!(manifest.contains("generation.directVegetation=false\n"));
        assert!(manifest.contains("generation.directStructures=false\n"));
        assert!(manifest.contains("generation.directProgressionStructures=false\n"));
    }

    fn synthetic_osm_pbf() -> Vec<u8> {
        let header_blob = osm_blob_message(1, b"header");
        let data_block = synthetic_osm_primitive_block();
        let data_blob = osm_blob_message(1, &data_block);
        let mut out = Vec::new();
        append_osm_blob(&mut out, "OSMHeader", &header_blob);
        append_osm_blob(&mut out, "OSMData", &data_blob);
        out
    }

    fn append_osm_blob(out: &mut Vec<u8>, blob_type: &str, blob: &[u8]) {
        let mut header = Vec::new();
        proto_field_bytes(&mut header, 1, blob_type.as_bytes());
        proto_field_varint(&mut header, 3, blob.len() as u64);
        out.extend_from_slice(&(header.len() as u32).to_be_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(blob);
    }

    fn synthetic_osm_primitive_block() -> Vec<u8> {
        let strings = ["", "highway", "primary"];
        let mut string_table = Vec::new();
        for value in strings {
            proto_field_bytes(&mut string_table, 1, value.as_bytes());
        }
        let mut dense = Vec::new();
        proto_field_bytes(&mut dense, 1, &packed_sint64(&[1, 1]));
        proto_field_bytes(&mut dense, 8, &packed_sint64(&[0, 0]));
        proto_field_bytes(&mut dense, 9, &packed_sint64(&[0, 100_000]));
        let mut way = Vec::new();
        proto_field_varint(&mut way, 1, 5);
        proto_field_bytes(&mut way, 2, &packed_varints(&[1]));
        proto_field_bytes(&mut way, 3, &packed_varints(&[2]));
        proto_field_bytes(&mut way, 8, &packed_sint64(&[1, 1]));
        let mut group = Vec::new();
        proto_field_bytes(&mut group, 2, &dense);
        proto_field_bytes(&mut group, 3, &way);
        let mut block = Vec::new();
        proto_field_bytes(&mut block, 1, &string_table);
        proto_field_bytes(&mut block, 2, &group);
        proto_field_varint(&mut block, 17, 100);
        block
    }

    fn osm_blob_message(raw_field_number: i32, payload: &[u8]) -> Vec<u8> {
        let mut blob = Vec::new();
        proto_field_bytes(&mut blob, raw_field_number, payload);
        blob
    }

    fn proto_field_bytes(out: &mut Vec<u8>, field_number: i32, bytes: &[u8]) {
        proto_varint(out, ((field_number as u64) << 3) | 2);
        proto_varint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
    }

    fn proto_field_varint(out: &mut Vec<u8>, field_number: i32, value: u64) {
        proto_varint(out, (field_number as u64) << 3);
        proto_varint(out, value);
    }

    fn packed_varints(values: &[i64]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            proto_varint(&mut out, *value as u64);
        }
        out
    }

    fn packed_sint64(values: &[i64]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            proto_varint(&mut out, zigzag_i64(*value));
        }
        out
    }

    fn zigzag_i64(value: i64) -> u64 {
        ((value << 1) ^ (value >> 63)) as u64
    }

    fn proto_varint(out: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            out.push(((value as u8) & 0x7F) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn write_cli_single_chunk_linear_region(path: &Path, payload: Vec<u8>) {
        let mut chunks = BTreeMap::new();
        chunks.insert(ChunkLocalPos::new(0, 0).unwrap(), payload);
        earthmap_region::write_linear_v2_region(path, &chunks, 0).unwrap();
    }

    fn cli_resource_payload() -> Vec<u8> {
        let mut chunk = ChunkModel::overworld(0, 0);
        for (index, kind) in earthmap_gameplay::OreKind::ALL
            .into_iter()
            .filter(|kind| kind.survival_critical())
            .enumerate()
        {
            chunk
                .set_block_state_id(index as i32, -54, 0, kind.deepslate_block_state_id())
                .unwrap();
        }
        chunk_nbt_encoder::encode_to_bytes(&chunk, 0).unwrap()
    }

    fn cli_progression_payload() -> Vec<u8> {
        let mut chest = nbt::compound();
        chest
            .put_string("id", earthmap_gameplay::CHEST_BLOCK_ENTITY)
            .unwrap();
        chest
            .put_string("LootTable", earthmap_gameplay::STRONGHOLD_LOOT_TABLE)
            .unwrap();
        let mut spawn_entity = nbt::compound();
        spawn_entity
            .put_string("id", earthmap_gameplay::BLAZE_ENTITY)
            .unwrap();
        let mut spawn_data = nbt::compound();
        spawn_data.put_compound("entity", spawn_entity).unwrap();
        let mut spawner = nbt::compound();
        spawner
            .put_string("id", earthmap_gameplay::SPAWNER_BLOCK_ENTITY)
            .unwrap();
        spawner.put_compound("SpawnData", spawn_data).unwrap();

        let mut end_portal = nbt::compound();
        end_portal
            .put_string("Name", earthmap_gameplay::END_PORTAL_BLOCK)
            .unwrap();
        let mut end_portal_frame = nbt::compound();
        end_portal_frame
            .put_string("Name", earthmap_gameplay::END_PORTAL_FRAME_BLOCK)
            .unwrap();
        let mut spawner_block = nbt::compound();
        spawner_block
            .put_string("Name", earthmap_gameplay::SPAWNER_BLOCK)
            .unwrap();
        let mut block_states = nbt::compound();
        block_states
            .put(
                "palette",
                nbt::Tag::List(
                    nbt::list(
                        nbt::TAG_COMPOUND,
                        vec![
                            nbt::Tag::Compound(end_portal),
                            nbt::Tag::Compound(end_portal_frame),
                            nbt::Tag::Compound(spawner_block),
                        ],
                    )
                    .unwrap(),
                ),
            )
            .unwrap();
        let mut section = nbt::compound();
        section.put_compound("block_states", block_states).unwrap();
        let mut root = nbt::compound();
        root.put_int("xPos", 0).unwrap();
        root.put_int("zPos", 0).unwrap();
        root.put(
            "block_entities",
            nbt::Tag::List(
                nbt::list(
                    nbt::TAG_COMPOUND,
                    vec![nbt::Tag::Compound(chest), nbt::Tag::Compound(spawner)],
                )
                .unwrap(),
            ),
        )
        .unwrap();
        root.put(
            "sections",
            nbt::Tag::List(
                nbt::list(nbt::TAG_COMPOUND, vec![nbt::Tag::Compound(section)]).unwrap(),
            ),
        )
        .unwrap();
        nbt::write_to_bytes("", &root).unwrap()
    }

    fn synthetic_bigtiff_heightmap() -> Vec<u8> {
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

    fn synthetic_classic_rgb_tiff() -> Vec<u8> {
        let entry_count = 10usize;
        let ifd_offset = 8usize;
        let ifd_bytes = 2 + (entry_count * 12) + 4;
        let bits_offset = ifd_offset + ifd_bytes;
        let tile_offset = bits_offset + 6;
        let pixels = [10u8, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let mut out = vec![0u8; tile_offset + pixels.len()];

        out[0] = b'I';
        out[1] = b'I';
        put_u16(&mut out, 2, 42);
        put_u32(&mut out, 4, ifd_offset as u32);

        let mut cursor = ifd_offset;
        put_u16(&mut out, cursor, entry_count as u16);
        cursor += 2;
        for (tag, field_type, count, value_or_offset) in [
            (256, 4, 1, 2),
            (257, 4, 1, 2),
            (258, 3, 3, bits_offset as u32),
            (259, 3, 1, 1),
            (277, 3, 1, 3),
            (284, 3, 1, 1),
            (322, 4, 1, 2),
            (323, 4, 1, 2),
            (324, 4, 1, tile_offset as u32),
            (325, 4, 1, pixels.len() as u32),
        ] {
            put_classic_entry(&mut out, cursor, tag, field_type, count, value_or_offset);
            cursor += 12;
        }
        put_u32(&mut out, cursor, 0);

        put_u16(&mut out, bits_offset, 8);
        put_u16(&mut out, bits_offset + 2, 8);
        put_u16(&mut out, bits_offset + 4, 8);
        out[tile_offset..tile_offset + pixels.len()].copy_from_slice(&pixels);
        out
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

    fn put_i16(out: &mut [u8], offset: usize, value: i16) {
        out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_f64(out: &mut [u8], offset: usize, value: f64) {
        out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
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
        if field_type == 3 && count == 1 {
            put_u16(out, offset + 8, value_or_offset as u16);
            put_u16(out, offset + 10, 0);
        } else {
            put_u32(out, offset + 8, value_or_offset);
        }
    }
}
