#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;

use earthmap_core::build_info;
use earthmap_core::commands::{self, CommandStatus};
use earthmap_core::progress;
use earthmap_geo::{
    EarthScaleMapping, GeoTiffHeightmapReader, GeoTiffMetadata, GeoTiffRowCache,
    HeightmapScalarSampler, VrtRgbMosaicReader,
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
use earthmap_region::{read_region_payloads, ChunkLocalPos, RegionError};
use earthmap_surface::{
    classify_surface, EarthSurfaceColumn, HeightOnlySettings, OutputFormat, SurfaceMaterialSample,
    SurfaceRegionColumnTrace, SurfaceRegionReport, SurfaceRegionSettings, SurfaceTextureMode,
    DEFAULT_HEIGHT_ONLY_CACHE_ROWS, SURVIVAL_MANIFEST_FILE_NAME,
};

const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;
const DEFAULT_HEIGHTMAP_PATH: &str = r"C:\earth_map_resources\HQheightmap.tif";
const FLAT_TEST_REGION_CHUNKS: u8 = 32;
const FLAT_TEST_SURFACE_Y: i32 = 63;
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
        "generate-vanilla-delegated-region"
            if (6..=9).contains(&args.len()) && vanilla_delegated_args(&args).is_some() =>
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
}

fn vanilla_delegated_args(args: &[String]) -> Option<VanillaDelegatedArgs<'_>> {
    let uses_default_heightmap = args.get(5).is_some_and(|arg| is_output_format_text(arg));
    if uses_default_heightmap {
        let (status, surface_raster) = optional_status_and_surface_raster(args, 6);
        return Some(VanillaDelegatedArgs {
            heightmap_path: DEFAULT_HEIGHTMAP_PATH,
            world_dir: args[1].as_str(),
            scale: args[2].as_str(),
            region_x: args[3].as_str(),
            region_z: args[4].as_str(),
            format: args[5].as_str(),
            status,
            surface_raster,
        });
    }
    if !args.get(6).is_some_and(|arg| is_output_format_text(arg)) {
        return None;
    }
    let (status, surface_raster) = optional_status_and_surface_raster(args, 7);
    Some(VanillaDelegatedArgs {
        heightmap_path: args[1].as_str(),
        world_dir: args[2].as_str(),
        scale: args[3].as_str(),
        region_x: args[4].as_str(),
        region_z: args[5].as_str(),
        format: args[6].as_str(),
        status,
        surface_raster,
    })
}

fn optional_status_and_surface_raster(args: &[String], first_optional: usize) -> (&str, &str) {
    if args.len() == first_optional + 1
        && args
            .get(first_optional)
            .is_some_and(|arg| arg.contains('='))
    {
        return ("surface", args[first_optional].as_str());
    }
    (
        args.get(first_optional)
            .map(String::as_str)
            .unwrap_or("surface"),
        args.get(first_optional + 1)
            .map(String::as_str)
            .unwrap_or("surfaceRaster=auto"),
    )
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
        "Status: Rust port active prototype. Java remains the oracle and fallback; see docs/RUST-PORT-PLAN.md."
    )?;
    writeln!(out)?;
    writeln!(out, "Commands:")?;
    writeln!(out, "  --help          Show this help.")?;
    writeln!(out, "  --version       Show version and targets.")?;
    writeln!(
        out,
        "  doctor          Check local Rust-port runtime assumptions."
    )?;
    writeln!(out, "  capabilities    Show Rust-port capability status.")?;
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
        "Generation, validation, rendering, and parity commands are recognized but not implemented in Rust yet."
    )?;
    writeln!(
        out,
        "Use the Java CLI until the matching phase in docs/RUST-PORT-PLAN.md is green."
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
    let settings = HeightOnlySettings::new(
        Path::new(heightmap_path),
        Path::new(world_dir),
        "SR EarthMap Height Only",
        0,
        scale,
        region_x,
        region_z,
        format,
        DEFAULT_HEIGHT_ONLY_CACHE_ROWS,
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
    let settings = SurfaceRegionSettings::new(
        heightmap_path,
        world_dir,
        "SR EarthMap Surface",
        0,
        scale,
        region_x,
        region_z,
        format,
        DEFAULT_HEIGHT_ONLY_CACHE_ROWS,
    )
    .map_err(|error| error.to_string())?;
    let report =
        earthmap_surface::generate_surface_region(&settings).map_err(|error| error.to_string())?;
    Ok(surface_region_report_lines(&report, world_dir))
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
) -> std::result::Result<Vec<String>, String> {
    let format = OutputFormat::parse(format_text).map_err(|error| error.to_string())?;
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
    let cache_rows = auto_shared_heightmap_cache_rows(Path::new(heightmap_path), 1)?;
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

    let report =
        earthmap_surface::generate_surface_region(&settings).map_err(|error| error.to_string())?;
    Ok(vanilla_delegated_region_report_lines(
        &report,
        world_dir,
        status,
        &surface_material_path,
    ))
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
    let cache_rows = auto_shared_heightmap_cache_rows(Path::new(heightmap_path), 1)?;
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
        format!(
            "{prefix}.neighborMajorityBiome={}",
            trace.neighbor_majority_biome.as_deref().unwrap_or("")
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
) -> Vec<String> {
    let mut lines = vec![
        "Vanilla-delegated surface region generated".to_string(),
        format!("regionX={}", report.region_x),
        format!("regionZ={}", report.region_z),
        format!("format={}", report.output_format.java_name()),
        format!("scale=1:{}", report.scale_denominator),
        format!("chunkStatus={}", status.id()),
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

fn parse_i32_string(text: &str) -> std::result::Result<i32, String> {
    text.parse::<i32>().map_err(|error| error.to_string())
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
    let minimum_rows = 512_i32.max(threads.checked_mul(128).ok_or("threads overflow")?);
    let reader = GeoTiffHeightmapReader::open(heightmap_path).map_err(|error| error.to_string())?;
    let row_bytes = i64::from(reader.metadata().width)
        .checked_mul(2)
        .ok_or_else(|| "heightmap row byte count overflow".to_string())?;
    let budget_bytes = 512_i64 * 1024 * 1024;
    let budget_rows = if budget_bytes <= 0 {
        minimum_rows
    } else {
        (budget_bytes / row_bytes).max(1) as i32
    };
    Ok(usize::try_from(minimum_rows.max(budget_rows.min(1024))).expect("positive cache rows"))
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
        assert!(out.contains("write-nbt-parity-fixtures <outputDir>"));
        assert!(out.contains("write-nbt-gzip-parity-fixtures <outputDir>"));
        assert!(out.contains("write-region-writer-parity-fixtures <outputDir>"));
        assert!(out.contains("inspect-heightmap [path]"));
        assert!(out.contains("locate-heightmap-point [heightmap] <scale> <longitude> <latitude>"));
        assert!(out.contains("classify-surface-point [heightmap] <scale> <longitude> <latitude>"));
        assert!(out.contains("sample-vrt-rgb <terrainVrt> <longitude> <latitude>"));
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
