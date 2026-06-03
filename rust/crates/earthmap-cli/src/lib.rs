#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use earthmap_core::build_info;
use earthmap_core::commands::{self, CommandStatus};
use earthmap_core::progress;
use earthmap_geo::{
    EarthScaleMapping, GeoTiffHeightmapReader, GeoTiffMetadata, GeoTiffRowCache,
    HeightmapScalarSampler, VrtRgbMosaicReader,
};
use earthmap_region::{ChunkLocalPos, RegionError};
use earthmap_surface::{
    classify_surface, HeightOnlySettings, OutputFormat, DEFAULT_HEIGHT_ONLY_CACHE_ROWS,
    SURVIVAL_MANIFEST_FILE_NAME,
};

const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;

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
        "inspect-heightmap" if args.len() == 2 => {
            write_result(inspect_heightmap(stdout, stderr, &args[1]))
        }
        "locate-heightmap-point" if args.len() == 5 => write_result(locate_heightmap_point(
            stdout, stderr, &args[1], &args[2], &args[3], &args[4],
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

fn print_help(out: &mut impl Write) -> io::Result<i32> {
    writeln!(out, "{}", build_info::NAME)?;
    writeln!(out)?;
    writeln!(
        out,
        "Status: Rust port Phase 1 CLI shell. Java remains the oracle and fallback."
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
    writeln!(out, "  write-nbt-parity-fixtures <outputDir>")?;
    writeln!(out, "  write-nbt-gzip-parity-fixtures <outputDir>")?;
    writeln!(out, "  write-region-writer-parity-fixtures <outputDir>")?;
    writeln!(out, "  inspect-heightmap <path>")?;
    writeln!(
        out,
        "  locate-heightmap-point <heightmap> <scale> <longitude> <latitude>"
    )?;
    writeln!(
        out,
        "  classify-surface-point <heightmap> <scale> <longitude> <latitude>"
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
        "DONE rust.phase4.realHeightmapSmoke - E:\\HQheightmap.tif inspect-heightmap and locate-heightmap-point stdout match the Java oracle."
    )?;
    writeln!(
        out,
        "DONE rust.phase4.heightOnlyRegionByteParity - Java/Rust height-only r.0.0 and r.-1.-1 MCA/Linear region bytes, payload manifests, normalized stdout, and exploration-only survival manifests match for E:\\HQheightmap.tif at 1:5000."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.earthSurfaceRulesBootstrap - EarthSurfaceRules classify/classifyShaped/normalizeForChunk contracts and classify-surface-point diagnostic match Java fixture cases and E:\\HQheightmap.tif smoke points."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.surfaceMaterialSampleBootstrap - SurfaceMaterialSample coverage, slope, ecoregion, terrain-token normalization, rounding, and SurfaceDataEvidence flag contracts match Java fixture cases."
    )?;
    writeln!(
        out,
        "DONE rust.phase5.trueMarbleSurfaceSamplerBootstrap - TrueMarbleSurfaceMaterialSampler wraps VrtRgbMosaicReader.sample_averaged as Java-compatible color-only SurfaceMaterialSample output for synthetic VRT fixtures."
    )?;
    for spec in commands::INITIAL_COMMANDS {
        let status = match spec.status {
            CommandStatus::Implemented => "DONE",
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

fn parse_i32_string(text: &str) -> std::result::Result<i32, String> {
    text.parse::<i32>().map_err(|error| error.to_string())
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
        assert!(out.contains("write-nbt-parity-fixtures <outputDir>"));
        assert!(out.contains("write-nbt-gzip-parity-fixtures <outputDir>"));
        assert!(out.contains("write-region-writer-parity-fixtures <outputDir>"));
        assert!(out.contains("inspect-heightmap <path>"));
        assert!(out.contains("locate-heightmap-point <heightmap> <scale> <longitude> <latitude>"));
        assert!(out.contains("classify-surface-point <heightmap> <scale> <longitude> <latitude>"));
        assert!(out.contains("sample-vrt-rgb <terrainVrt> <longitude> <latitude>"));
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
