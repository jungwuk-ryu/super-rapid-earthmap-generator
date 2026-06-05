#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::Instant;

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
    chunk_nbt_encoder, level_dat_template,
    nbt::{self, Tag},
    packed_long_array::PackedLongArray,
    section_palette::bits_per_entry_for_palette_size,
};
use earthmap_region::{
    read_region_payloads, validate_region_file_for_resume, ChunkLocalPos, RegionError,
    RegionFormat, REGION_CHUNKS_PER_REGION,
};
use earthmap_surface::{
    classify_surface, generate_surface_region_with_open_material_sampler,
    EarthDataSurfaceMaterialSampler, EarthSurfaceColumn, HeightOnlySettings,
    LandShallowTopoPhotoSampler, MetImageExportTerrainSampler, OutputFormat, SurfaceMaterialSample,
    SurfaceRegionColumnTrace, SurfaceRegionReport, SurfaceRegionSettings, SurfaceTextureMode,
    WwfEcoregionSampler, DEFAULT_SURFACE_TILE_CACHE_ENTRIES, REGION_SIZE_BLOCKS, SEA_LEVEL_Y,
    SURVIVAL_MANIFEST_FILE_NAME,
};
use serde_json::{json, Value};

const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;
const DEFAULT_HEIGHTMAP_PATH: &str = r"C:\earth_map_resources\HQheightmap.tif";
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
const SURFACE_PHOTO_REGION_WORKER_LIMIT: usize = 4;
const SURFACE_PHOTO_RAYON_THREAD_MULTIPLIER: usize = 2;
const SURFACE_PHOTO_RAYON_THREAD_LIMIT: usize = 16;
const PROGRESS_EVENT_SCHEMA_VERSION: u32 = 1;
const RESUME_FINGERPRINT_SCHEMA_VERSION: u32 = 1;
const VANILLA_DELEGATED_RESUME_JOURNAL_FILE_NAME: &str = "earthmap-vanilla-delegated-resume.ndjson";
const PARALLEL_EVENT_CHANNEL_CAPACITY: usize = 1024;
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
    let rayon_threads = requested_threads
        .saturating_mul(SURFACE_PHOTO_RAYON_THREAD_MULTIPLIER)
        .max(requested_threads.max(1))
        .min(SURFACE_PHOTO_RAYON_THREAD_LIMIT);
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(rayon_threads)
        .build_global();
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
            write_result(inspect_heightmap(stdout, stderr, DEFAULT_HEIGHTMAP_PATH))
        }
        "inspect-heightmap" if args.len() == 2 => {
            write_result(inspect_heightmap(stdout, stderr, &args[1]))
        }
        "locate-heightmap-point" if args.len() == 4 => write_result(locate_heightmap_point(
            stdout,
            stderr,
            DEFAULT_HEIGHTMAP_PATH,
            &args[1],
            &args[2],
            &args[3],
        )),
        "locate-heightmap-point" if args.len() == 5 => write_result(locate_heightmap_point(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4],
        )),
        "classify-surface-point" if args.len() == 4 => write_result(classify_surface_point(
            stdout,
            stderr,
            DEFAULT_HEIGHTMAP_PATH,
            &args[1],
            &args[2],
            &args[3],
        )),
        "classify-surface-point" if args.len() == 5 => write_result(classify_surface_point(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4],
        )),
        "raster-smoke" if args.len() == 3 => write_result(raster_smoke(
            stdout,
            stderr,
            DEFAULT_HEIGHTMAP_PATH,
            &args[1],
            &args[2],
        )),
        "raster-smoke" if args.len() == 4 => {
            write_result(raster_smoke(stdout, stderr, &args[1], &args[2], &args[3]))
        }
        "sample-vrt-rgb" if args.len() == 4 => {
            write_result(sample_vrt_rgb(stdout, stderr, &args[1], &args[2], &args[3]))
        }
        "generate-height-region" if args.len() == 7 => write_result(generate_height_region(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
        )),
        "generate-height-region" if args.len() == 6 => write_result(generate_height_region(
            stdout,
            stderr,
            DEFAULT_HEIGHTMAP_PATH,
            &args[1],
            &args[2],
            &args[3],
            &args[4],
            &args[5],
        )),
        "generate-surface-region" if args.len() == 7 => write_result(generate_surface_region(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4], &args[5], &args[6],
        )),
        "generate-surface-region" if args.len() == 6 => write_result(generate_surface_region(
            stdout,
            stderr,
            DEFAULT_HEIGHTMAP_PATH,
            &args[1],
            &args[2],
            &args[3],
            &args[4],
            &args[5],
        )),
        "quality-candidate"
            if (6..=9).contains(&args.len()) && quality_candidate_args(&args).is_some() =>
        {
            let parsed = quality_candidate_args(&args).expect("validated quality candidate args");
            write_result(generate_quality_candidate(
                stdout,
                stderr,
                parsed.heightmap_path,
                parsed.world_dir,
                parsed.scale,
                parsed.region_x,
                parsed.region_z,
                parsed.format,
                parsed.surface_raster,
                parsed.sample_grid,
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
        "generate-vanilla-delegated-region"
            if args.len() >= 6 && vanilla_delegated_args(&args).is_some() =>
        {
            let parsed = vanilla_delegated_args(&args).expect("validated vanilla delegated args");
            write_result(generate_vanilla_delegated_region(
                stdout,
                stderr,
                parsed.heightmap_path,
                parsed.world_dir,
                parsed.scale,
                parsed.region_x,
                parsed.region_z,
                parsed.format,
                parsed.status,
                parsed.surface_raster,
                parsed.extra_options,
            ))
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
        "trace-surface-region-column" if (6..=8).contains(&args.len()) => {
            let parsed = trace_column_args(&args);
            write_result(trace_surface_region_column(
                stdout,
                stderr,
                parsed.heightmap_path,
                parsed.scale,
                parsed.region_x,
                parsed.region_z,
                parsed.local_x,
                parsed.local_z,
                parsed.surface_raster,
            ))
        }
        "trace-surface-region-cell" if (8..=10).contains(&args.len()) => {
            let parsed = trace_cell_args(&args);
            write_result(trace_surface_region_cell(
                stdout,
                stderr,
                parsed.heightmap_path,
                parsed.scale,
                parsed.region_x,
                parsed.region_z,
                parsed.chunk_local_x,
                parsed.chunk_local_z,
                parsed.cell_x,
                parsed.cell_z,
                parsed.surface_raster,
            ))
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

fn write_result(result: io::Result<i32>) -> i32 {
    match result {
        Ok(exit_code) => exit_code,
        Err(error) => {
            let _ = writeln!(io::stderr(), "I/O error: {error}");
            1
        }
    }
}

struct VanillaDelegatedArgs<'a> {
    heightmap_path: &'a str,
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
    heightmap_path: &'a str,
    world_dir: &'a str,
    scale: &'a str,
    region_x: &'a str,
    region_z: &'a str,
    format: &'a str,
    surface_raster: &'a str,
    sample_grid: &'a str,
}

fn quality_candidate_args(args: &[String]) -> Option<QualityCandidateArgs<'_>> {
    if args.get(5).is_some_and(|arg| is_output_format_text(arg)) {
        let (surface_raster, sample_grid) = optional_quality_candidate_args(args, 6);
        return Some(QualityCandidateArgs {
            heightmap_path: DEFAULT_HEIGHTMAP_PATH,
            world_dir: args[1].as_str(),
            scale: args[2].as_str(),
            region_x: args[3].as_str(),
            region_z: args[4].as_str(),
            format: args[5].as_str(),
            surface_raster,
            sample_grid,
        });
    }
    if !args.get(6).is_some_and(|arg| is_output_format_text(arg)) {
        return None;
    }
    let (surface_raster, sample_grid) = optional_quality_candidate_args(args, 7);
    Some(QualityCandidateArgs {
        heightmap_path: args[1].as_str(),
        world_dir: args[2].as_str(),
        scale: args[3].as_str(),
        region_x: args[4].as_str(),
        region_z: args[5].as_str(),
        format: args[6].as_str(),
        surface_raster,
        sample_grid,
    })
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

fn vanilla_delegated_args(args: &[String]) -> Option<VanillaDelegatedArgs<'_>> {
    let uses_default_heightmap = args.get(5).is_some_and(|arg| is_output_format_text(arg));
    if uses_default_heightmap {
        let optional = optional_generation_args(args, 6);
        return Some(VanillaDelegatedArgs {
            heightmap_path: DEFAULT_HEIGHTMAP_PATH,
            world_dir: args[1].as_str(),
            scale: args[2].as_str(),
            region_x: args[3].as_str(),
            region_z: args[4].as_str(),
            format: args[5].as_str(),
            status: optional.status,
            surface_raster: optional.surface_raster,
            extra_options: optional.extra_options,
        });
    }
    if !args.get(6).is_some_and(|arg| is_output_format_text(arg)) {
        return None;
    }
    let optional = optional_generation_args(args, 7);
    Some(VanillaDelegatedArgs {
        heightmap_path: args[1].as_str(),
        world_dir: args[2].as_str(),
        scale: args[3].as_str(),
        region_x: args[4].as_str(),
        region_z: args[5].as_str(),
        format: args[6].as_str(),
        status: optional.status,
        surface_raster: optional.surface_raster,
        extra_options: optional.extra_options,
    })
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

fn optional_generation_args(args: &[String], first_optional: usize) -> GenerationOptionalArgs<'_> {
    let rest = args.get(first_optional..).unwrap_or(&[]);
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

fn parse_region_compression_options(
    format: OutputFormat,
    options: &[String],
) -> std::result::Result<RegionCompressionOptions, String> {
    let mut parsed = RegionCompressionOptions::default();
    for option in options {
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| format!("optional generation argument must be key=value: {option}"))?;
        if key.eq_ignore_ascii_case("compression") || key.eq_ignore_ascii_case("compressionLevel") {
            match format {
                OutputFormat::Mca => {
                    parsed.mca_compression_level = Some(parse_mca_compression_level(value)?);
                }
                OutputFormat::LinearV2 => {
                    parsed.linear_compression_level = Some(parse_linear_compression_level(value)?);
                }
            }
            continue;
        }
        if key.eq_ignore_ascii_case("mcaCompression")
            || key.eq_ignore_ascii_case("mcaCompressionLevel")
        {
            parsed.mca_compression_level = Some(parse_mca_compression_level(value)?);
            continue;
        }
        if key.eq_ignore_ascii_case("linearCompression")
            || key.eq_ignore_ascii_case("linearCompressionLevel")
        {
            parsed.linear_compression_level = Some(parse_linear_compression_level(value)?);
            continue;
        }
        return Err(format!("unknown generation option: {key}"));
    }
    Ok(parsed)
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
    heightmap_path: &'a str,
    scale: &'a str,
    region_x: &'a str,
    region_z: &'a str,
    local_x: &'a str,
    local_z: &'a str,
    surface_raster: Option<&'a str>,
}

fn trace_column_args(args: &[String]) -> TraceColumnArgs<'_> {
    let uses_default_heightmap = args.len() == 6
        || (args.len() == 7
            && args.get(6).is_some_and(|arg| arg.contains('='))
            && args.get(1).is_some_and(|arg| parse_i32_string(arg).is_ok()));
    if uses_default_heightmap {
        return TraceColumnArgs {
            heightmap_path: DEFAULT_HEIGHTMAP_PATH,
            scale: args[1].as_str(),
            region_x: args[2].as_str(),
            region_z: args[3].as_str(),
            local_x: args[4].as_str(),
            local_z: args[5].as_str(),
            surface_raster: args.get(6).map(String::as_str),
        };
    }
    TraceColumnArgs {
        heightmap_path: args[1].as_str(),
        scale: args[2].as_str(),
        region_x: args[3].as_str(),
        region_z: args[4].as_str(),
        local_x: args[5].as_str(),
        local_z: args[6].as_str(),
        surface_raster: args.get(7).map(String::as_str),
    }
}

struct TraceCellArgs<'a> {
    heightmap_path: &'a str,
    scale: &'a str,
    region_x: &'a str,
    region_z: &'a str,
    chunk_local_x: &'a str,
    chunk_local_z: &'a str,
    cell_x: &'a str,
    cell_z: &'a str,
    surface_raster: Option<&'a str>,
}

fn trace_cell_args(args: &[String]) -> TraceCellArgs<'_> {
    let uses_default_heightmap = args.len() == 8
        || (args.len() == 9
            && args.get(8).is_some_and(|arg| arg.contains('='))
            && args.get(1).is_some_and(|arg| parse_i32_string(arg).is_ok()));
    if uses_default_heightmap {
        return TraceCellArgs {
            heightmap_path: DEFAULT_HEIGHTMAP_PATH,
            scale: args[1].as_str(),
            region_x: args[2].as_str(),
            region_z: args[3].as_str(),
            chunk_local_x: args[4].as_str(),
            chunk_local_z: args[5].as_str(),
            cell_x: args[6].as_str(),
            cell_z: args[7].as_str(),
            surface_raster: args.get(8).map(String::as_str),
        };
    }
    TraceCellArgs {
        heightmap_path: args[1].as_str(),
        scale: args[2].as_str(),
        region_x: args[3].as_str(),
        region_z: args[4].as_str(),
        chunk_local_x: args[5].as_str(),
        chunk_local_z: args[6].as_str(),
        cell_x: args[7].as_str(),
        cell_z: args[8].as_str(),
        surface_raster: args.get(9).map(String::as_str),
    }
}

fn print_help(out: &mut impl Write) -> io::Result<i32> {
    writeln!(out, "{}", build_info::NAME)?;
    writeln!(out)?;
    writeln!(
        out,
        "Status: Rust runtime active prototype. Java remains the compatibility oracle and fallback."
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
    writeln!(
        out,
        "  summarize-region-chunk <regionFile> <localChunkX> <localChunkZ>"
    )?;
    writeln!(
        out,
        "  compare-region-chunk-details <expectedRegionFile> <actualRegionFile> <expectedLocalChunkX> <expectedLocalChunkZ> [actualLocalChunkX actualLocalChunkZ]"
    )?;
    writeln!(out, "  default heightmap: {DEFAULT_HEIGHTMAP_PATH}")?;
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
        "  generate-vanilla-delegated-region [heightmap] <worldDir> <scale> <regionX> <regionZ> <mca|linear> [surface|carvers] [surfaceRaster=auto|path] [compression=N|linearCompression=N|mcaCompression=N]"
    )?;
    writeln!(
        out,
        "  generate-vanilla-delegated-regions-parallel <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear> <threads> [surface|carvers] [surfaceRaster=auto|path] [compression=N|linearCompression=N|mcaCompression=N]"
    )?;
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
        "DONE rust.phase4.realVrtRgbSmoke - D:\\earthmap\\TifFiles\\terrain\\TrueMarble.vrt sample-vrt-rgb stdout matches the Java VrtRgbMosaicReader oracle for representative coordinates."
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
        "DONE rust.phase4.realHeightmapSmoke - C:\\earth_map_resources\\HQheightmap.tif inspect-heightmap and locate-heightmap-point stdout match the Java oracle."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.heightOnlyRegionByteParity - Java/Rust height-only r.0.0 and r.-1.-1 MCA/Linear region bytes, payload manifests, normalized stdout, and exploration-only survival manifests match for C:\\earth_map_resources\\HQheightmap.tif at 1:5000."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.earthSurfaceRulesBootstrap - EarthSurfaceRules classify/classifyShaped/normalizeForChunk contracts and classify-surface-point diagnostic match Java fixture cases and C:\\earth_map_resources\\HQheightmap.tif smoke points."
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
        "DONE rust.phase5.metImageExportTerrainSamplerBootstrap - MetImageExportTerrainSampler discovers Java MET image_exports tiles, parses aux GeoTransform metadata, samples PNG terrain-token colors with a bounded image cache, and EarthDataSurfaceMaterialSampler prefers exported tokens when present."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.landShallowTopoPhotoSamplerBootstrap - LandShallowTopoPhotoSampler discovers west/east topographic GeoTIFF halves and EarthDataSurfaceMaterialSampler.sample_photo follows Java photo-source preference, coarse evidence, and terrain-token rules."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.naturalSurfacePolicyBootstrap - NaturalSurfaceBlockPolicy and CoastalSurfaceCleaner production-safe surface cleanup contracts are ported for Java fixture cases."
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
        "Java output remains the oracle; use scripts/run.ps1 until the matching Rust port phase is green."
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
    let format = OutputFormat::parse(format_text).map_err(|error| error.to_string())?;
    let compression_options = parse_region_compression_options(format, extra_options)?;
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
        1.0,
        SurfaceTextureMode::Photo,
    )
    .map_err(|error| error.to_string())?;
    settings.surface_material_path = Some(surface_material_path.clone());
    settings.surface_tile_cache_entries = surface_tile_cache_entries;
    apply_region_compression_options(&mut settings, compression_options);

    let report =
        earthmap_surface::generate_surface_region(&settings).map_err(|error| error.to_string())?;
    Ok(vanilla_delegated_region_report_lines(
        &report,
        world_dir,
        status,
        &surface_material_path,
        compression_options,
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
    let compression_options = parse_region_compression_options(format, extra_options)?;
    let status = ChunkGenerationStatus::parse(status_text).map_err(|error| error.to_string())?;
    if status == ChunkGenerationStatus::Full {
        return Err("delegated generation status must be surface or carvers".to_string());
    }
    let scale = parse_positive_i32_string("scale", scale_text)?;
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
        compression_options,
    );

    let worker_count = threads
        .min(region_count)
        .min(SURFACE_PHOTO_REGION_WORKER_LIMIT)
        .max(1);
    configure_surface_photo_rayon_threads(threads);
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
        format,
        scale,
        start_region_x,
        start_region_z,
        cols,
        rows,
        status,
        &surface_material_path,
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
            "worldDir": normalized_path_display(world),
            "format": format.java_name(),
            "scale": scale,
            "chunkStatus": status.id(),
            "regionStartX": start_region_x,
            "regionStartZ": start_region_z,
            "regionCols": cols,
            "regionRows": rows,
            "regionCount": region_count,
            "requestedThreads": threads,
            "workerThreads": worker_count,
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
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = &region_queue;
            let stop_queueing = &stop_queueing;
            let surface_material_sampler = &surface_material_sampler;
            let surface_material_path = &surface_material_path;
            let completed_regions = Arc::clone(&resume_completed_regions);
            let journal = Arc::clone(&resume_journal);
            let sender = event_sender.clone();
            let fingerprint_matched = resume.fingerprint_matched;
            scope.spawn(move || loop {
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
                let region_file = vanilla_delegated_region_file(world, format, region_x, region_z);
                if fingerprint_matched && completed_regions.contains(&(region_x, region_z)) {
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
                        "SR EarthMap Vanilla Delegated",
                        0,
                        scale,
                        region_x,
                        region_z,
                        format,
                        cache_rows,
                        false,
                        status,
                        1.0,
                        SurfaceTextureMode::Photo,
                    )
                    .map_err(|error| error.to_string())?;
                    settings.surface_material_path = Some((*surface_material_path).clone());
                    settings.surface_tile_cache_entries = surface_tile_cache_entries;
                    settings.parallel_column_sampling = worker_count <= 4;
                    apply_region_compression_options(&mut settings, compression_options);
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
        drop(event_sender);
        while let Ok(event) = event_receiver.recv() {
            handle_vanilla_delegated_parallel_event(out, &mut stats, event)?;
        }
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
        "surfaceSamplerStrategy=shared".to_string(),
        format!("sharedCacheRows={cache_rows}"),
        format!("surfaceTileCacheEntries={surface_tile_cache_entries}"),
        compression_options_report_line(format, compression_options),
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
    },
    RegionFailed {
        region_x: i32,
        region_z: i32,
        elapsed_millis: u128,
        region_file: std::path::PathBuf,
        message: String,
    },
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
        } => {
            stats.generated_regions += 1;
            stats.phase_surface_sample_millis += millis(report.surface_sample_nanos);
            stats.phase_chunk_build_millis += millis(report.chunk_build_nanos);
            stats.phase_nbt_encode_millis += millis(report.nbt_encode_nanos);
            stats.phase_region_write_millis += millis(report.region_write_nanos);
            stats.phase_metadata_millis += millis(report.metadata_nanos);
            stats.phase_total_internal_millis += millis(report.total_nanos);
            write_progress_event(
                out,
                json!({
                    "schemaVersion": PROGRESS_EVENT_SCHEMA_VERSION,
                    "type": "regionGenerated",
                    "regionX": report.region_x,
                    "regionZ": report.region_z,
                    "elapsedMillis": u128_to_u64(elapsed_millis),
                    "chunks": report.chunk_count,
                    "outputBytes": output_bytes,
                    "regionFile": normalized_path_display(&report.region_file),
                }),
            )?;
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
        "verticalScale": "1.0",
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
    let normalized = normalized_path(surface_material_path);
    if let Some(root) = normalized
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
    {
        push_distinct_resume_path(&mut roots, root.to_path_buf());
    }
    for root in [
        Path::new("E:/earthmap"),
        Path::new("D:/earthmap"),
        Path::new("F:/earthmap"),
    ] {
        push_distinct_resume_path(&mut roots, normalized_path(root));
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
        Ok(metadata) => json!({
            "path": normalized_path,
            "exists": true,
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
) -> Vec<String> {
    let mut lines = vec![
        "Vanilla-delegated surface region generated".to_string(),
        format!("regionX={}", report.region_x),
        format!("regionZ={}", report.region_z),
        format!("format={}", report.output_format.java_name()),
        format!("scale=1:{}", report.scale_denominator),
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
    let normalized_heightmap = normalized_path(heightmap_path);
    if let Some(parent) = normalized_heightmap.parent() {
        candidates.push(parent.join("terrain").join("TrueMarble.vrt"));
        if let Some(grand_parent) = parent.parent() {
            candidates.push(
                grand_parent
                    .join("TifFiles")
                    .join("terrain")
                    .join("TrueMarble.vrt"),
            );
        }
    }
    if let Some(root) = normalized_heightmap.components().next() {
        if let std::path::Component::Prefix(prefix) = root {
            candidates.push(
                std::path::PathBuf::from(prefix.as_os_str())
                    .join(std::path::MAIN_SEPARATOR.to_string())
                    .join("earthmap")
                    .join("TifFiles")
                    .join("terrain")
                    .join("TrueMarble.vrt"),
            );
        } else if let std::path::Component::RootDir = root {
            candidates.push(
                std::path::PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
                    .join("earthmap")
                    .join("TifFiles")
                    .join("terrain")
                    .join("TrueMarble.vrt"),
            );
        }
    }
    for drive in ["D:", "E:", "F:"] {
        candidates.push(
            std::path::PathBuf::from(drive)
                .join(std::path::MAIN_SEPARATOR.to_string())
                .join("earthmap")
                .join("TifFiles")
                .join("terrain")
                .join("TrueMarble.vrt"),
        );
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
    format: OutputFormat,
    scale_denominator: i32,
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
    status: ChunkGenerationStatus,
    surface_material_path: &Path,
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
        "generation.chunkStatus".to_string(),
        status.id().to_string(),
    );
    values.insert("generation.verticalScale".to_string(), "1.0".to_string());
    values.insert("generation.textureMode".to_string(), "photo".to_string());
    values.insert(
        "generation.surfaceMaterialPath".to_string(),
        normalized_path_display(surface_material_path),
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
        );

        assert_ne!(first, second);
        assert!(first.to_string().contains("tifRoot.climate"));
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
        );

        assert_eq!(first, second);
        assert!(!first.to_string().contains("ecoregions.cache"));
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
        assert!(!out.contains("DONE rust.command.generate-vanilla-delegated-region"));
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
        assert!(out.contains("summarize-region-chunk <regionFile> <localChunkX> <localChunkZ>"));
        assert!(
            out.contains("compare-region-chunk-details <expectedRegionFile> <actualRegionFile>")
        );
        assert!(out.contains("default heightmap: C:\\earth_map_resources\\HQheightmap.tif"));
        assert!(out.contains("trace-surface-region-column [heightmap] <scale> <regionX> <regionZ>"));
        assert!(out.contains("trace-surface-region-cell [heightmap] <scale> <regionX> <regionZ>"));
        assert!(out.contains("generate-flat-test-world <worldDir> <mca|linear>"));
        assert!(out.contains("generate-palette-stress-world <worldDir>"));
        assert!(out.contains("quality-candidate [heightmap] <worldDir>"));
        assert!(out.contains("benchmark-region-writers <outputDir> [iterations=3]"));
        assert!(out.contains("write-nbt-parity-fixtures <outputDir>"));
        assert!(out.contains("write-nbt-gzip-parity-fixtures <outputDir>"));
        assert!(out.contains("write-region-writer-parity-fixtures <outputDir>"));
        assert!(out.contains("inspect-heightmap [path]"));
        assert!(out.contains("locate-heightmap-point [heightmap] <scale> <longitude> <latitude>"));
        assert!(out.contains("classify-surface-point [heightmap] <scale> <longitude> <latitude>"));
        assert!(out.contains("raster-smoke [heightmap] <scale> <outputJson>"));
        assert!(out.contains("sample-vrt-rgb <terrainVrt> <longitude> <latitude>"));
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

        let parsed = quality_candidate_args(&args).unwrap();

        assert_eq!(parsed.heightmap_path, DEFAULT_HEIGHTMAP_PATH);
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

        let parsed = quality_candidate_args(&args).unwrap();

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

        let parsed = vanilla_delegated_args(&args).unwrap();

        assert_eq!(parsed.heightmap_path, DEFAULT_HEIGHTMAP_PATH);
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

        let parsed = vanilla_delegated_args(&args).unwrap();

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

        let parsed = vanilla_delegated_args(&args).unwrap();

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

        let parsed_column = trace_column_args(&column_args);
        let parsed_cell = trace_cell_args(&cell_args);

        assert_eq!(parsed_column.heightmap_path, DEFAULT_HEIGHTMAP_PATH);
        assert_eq!(parsed_column.scale, "147760");
        assert_eq!(
            parsed_column.surface_raster,
            Some("surfaceRaster=D:\\surface.vrt")
        );
        assert_eq!(parsed_cell.heightmap_path, DEFAULT_HEIGHTMAP_PATH);
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

        let parsed_delegated = vanilla_delegated_args(&delegated_args).unwrap();
        let parsed_column = trace_column_args(&column_args);
        let parsed_cell = trace_cell_args(&cell_args);

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
        assert_eq!(err, "Unknown command. Use --help.\n");
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
                r"\\?\D:\earthmap\TifFiles\terrain\TrueMarble.vrt"
            )),
            r"D:\earthmap\TifFiles\terrain\TrueMarble.vrt"
        );
        assert_eq!(
            java_display_path(Path::new(r"\\?\UNC\server\share\TrueMarble.vrt")),
            r"\\server\share\TrueMarble.vrt"
        );
        assert_eq!(
            java_display_path(Path::new(r"D:\earthmap\TifFiles\terrain\TrueMarble.vrt")),
            r"D:\earthmap\TifFiles\terrain\TrueMarble.vrt"
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
