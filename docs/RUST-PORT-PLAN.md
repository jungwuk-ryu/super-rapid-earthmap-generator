# Rust Port Execution Plan

Date: 2026-06-03

Goal: Port Super-Rapid EarthMap Generator from Java to Rust while keeping Java as the correctness oracle until the Rust
implementation proves identical output. The Rust implementation must target maximum throughput, but output parity is a
hard gate before any optimization is accepted.

Current project state: active prototype, production readiness NO-GO. A Rust build does not change the release gate:
full Earth or nation-war generation remains blocked until `docs/QUALITY-GATES.md` passes for the same candidate
build/config.

## Non-Negotiable Rules

- Java output is the oracle until Rust parity is proven.
- Same inputs, settings, seed, and command must produce the same chunk NBT payloads.
- MCA and Linear outputs must be byte-identical whenever the compression stream can be matched.
- If a compression library difference prevents byte-identical region files, the Rust path must still prove identical
  uncompressed chunk payloads and must document the compression delta before promotion.
- No optimization may land unless the parity corpus stays green.
- Quality gates must not be weakened, skipped, or replaced by speed metrics.
- Generated worlds, logs, previews, and golden artifacts must be written outside source-controlled paths unless they are
  intentionally small test fixtures.

## Target Output Compatibility

Rust must preserve the existing public behavior of `net.earthmap.cli.EarthMapCli`.

Initial command compatibility:

```text
generate
inspect-heightmap
locate-heightmap-point
classify-surface-point
generate-height-region
generate-surface-region
generate-vanilla-delegated-region
generate-vanilla-delegated-regions-parallel
generate-vanilla-delegated-plan-parallel
generate-survival-region
generate-survival-regions-parallel
quality-production-sample-batch
mca-topdown-render
validate-mca-region
validate-linear-region
compare-mca-linear-region-payloads
inspect-mca-palettes
inspect-linear-palettes
inspect-mca-statuses
inspect-linear-statuses
```

Rust may add diagnostic commands, but existing command names, required arguments, output file names, CSV headers, and
manifest keys must stay stable unless a separate compatibility decision is recorded in `docs/DECISIONS.md`.
The `generate-survival-*` names are legacy Java compatibility command names for vanilla-delegated terrain, manifest,
finalization, and evidence workflows. They are not approval to add Rust-side direct generation of ores, caves,
structures, strongholds, End portals, loot, or spawners. The former "Survival And Gameplay" phase is intentionally
deleted from the Rust plan.

## Baseline Evidence

Use the current Java build as the first performance and correctness baseline.

Known local one-region evidence:

```text
baseline folder: D:\earthmap\perf-optimized-v165-1region-20260603-0001
command profile: vanilla delegated, Linear V2, scale 1:5000, textureMode=photo, surfaceRaster=auto
region: r.-1.-1.linear
chunks: 1024
output bytes: 279686
elapsed millis: 183167
```

The first Rust milestone is not faster output. It is identical output for this profile.

## Golden Corpus

Create a fixed golden corpus before porting behavior.

Required corpus groups:

- `flat`: synthetic flat MCA and Linear worlds.
- `height-only`: selected heightmap-only regions.
- `surface`: surface regions with and without `surfaceRaster`.
- `photo`: `textureMode=photo`, `surfaceRaster=auto`.
- `water`: coast, open ocean, shallow water, and invalid/edge raster samples.
- `survival`: vanilla-delegated chunk status, manifest metadata, finalization command, and server-evidence samples.
- `quality`: five-crop production proof samples from `docs/QUALITY-GATES.md`.
- `osm`: synthetic OSM mask, XML cache, and PBF extraction samples.

Each corpus entry must include:

```text
command.txt
settings.properties
stdout.txt
stderr.txt
sha256-manifest.txt
chunk-payload-manifest.csv
region files
metrics and preview artifacts when applicable
```

`chunk-payload-manifest.csv` must contain:

```text
format,regionX,regionZ,localChunkX,localChunkZ,payloadBytes,payloadSha256
```

Promotion from one phase to the next requires a fresh Java corpus and Rust comparison from the same source revision.

## Repository Layout

Recommended Rust workspace:

```text
rust/
  Cargo.toml
  crates/
    earthmap-core/
    earthmap-geo/
    earthmap-surface/
    earthmap-minecraft/
    earthmap-region/
    earthmap-gameplay/     # manifest, evidence, and validator compatibility only
    earthmap-quality/
    earthmap-cli/
    earthmap-parity/
  scripts/
    build.ps1
    test.ps1
    run.ps1
    generate-golden.ps1
    compare-golden.ps1
```

Do not delete or replace Java code during the port. Java remains the oracle and fallback until the Rust path passes all
required gates.

## Java To Rust Module Map

```text
net.earthmap.core       -> earthmap-core
net.earthmap.geo        -> earthmap-geo
net.earthmap.terrain    -> earthmap-surface
net.earthmap.minecraft  -> earthmap-minecraft
net.earthmap.region     -> earthmap-region
net.earthmap.gameplay   -> earthmap-gameplay, limited to manifests, evidence appliers, validators, and scanners
net.earthmap.quality    -> earthmap-quality
net.earthmap.cli        -> earthmap-cli
tests and validators    -> earthmap-parity
```

The Rust internal model should be region-owned and structure-of-arrays where possible:

```text
height_meters: Vec<f64>
smoothed_height_meters: Vec<f64>
coast_factor: Vec<f64>
local_relief_meters: Vec<f64>
source_rgb: Vec<i32>
flags: Vec<u32>
ground_y: Vec<i16>
water_surface_y: Vec<i16>
top_block_state_id: Vec<u16>
filler_block_state_id: Vec<u16>
biome_id: Vec<u16 or interned string id>
```

Object-heavy structures may be used in compatibility tests, but production generation should avoid per-column heap
objects.

## Phase 0: Oracle And Parity Harness

Deliverables:

- Java golden corpus generator.
- Rust-side corpus comparator.
- Region payload extractor for MCA and Linear.
- SHA-256 manifest writer.
- CI/local script that fails on any payload mismatch.

Acceptance:

- Golden corpus can be regenerated with one command.
- Comparisons distinguish payload mismatch, missing chunks, metadata mismatch, and compression-only mismatch.
- Existing Java tests still pass.

Bootstrap implementation status on 2026-06-03:

- `rust/scripts/generate-golden.ps1` regenerates a Java-oracle synthetic bootstrap corpus with:
  - `flat-mca`
  - `flat-linear`
  - `palette-stress-mca`
- `rust/scripts/compare-golden.ps1` verifies that each bootstrap entry's saved Rust-written chunk payload manifest covers
  the same region file set currently present in the Java-oracle output, then compares every listed region payload hash.
- The Rust CLI includes diagnostic commands for:
  - `write-sha256-manifest`
  - `write-region-payload-manifest`
  - `append-region-payload-manifest`
  - `compare-region-payload-manifest`
- The Java CLI includes `generate-flat-test-world <worldDir> <mca|linear>` so flat MCA and Linear oracle fixtures can be
  generated without test-only entrypoints.
- Remaining Phase 0 work: compare Rust-generated outputs against the Java-oracle manifests; expand the corpus to
  height-only, surface, photo, water, survival, quality, and OSM entries; add metadata/compression-delta reports; and add
  CI wiring.

Suggested commands:

```powershell
.\scripts\test.ps1
.\rust\scripts\generate-golden.ps1 -OutputRoot D:\earthmap\rust-port-golden\v001
.\rust\scripts\compare-golden.ps1 -GoldenRoot D:\earthmap\rust-port-golden\v001
```

## Phase 1: Rust Skeleton And CLI Shell

Deliverables:

- Cargo workspace.
- `earthmap-rs --version`, `doctor`, and command parser.
- PowerShell wrappers mirroring Java scripts.
- Logging and progress file format compatible with Java batch runs.

Acceptance:

- Rust CLI accepts the initial command set and returns clear "not implemented" errors for incomplete commands.
- Build and test scripts work on Windows.
- No Java source behavior is modified.

Implementation status on 2026-06-03:

- Cargo workspace and `Cargo.lock` exist under `rust/`.
- `earthmap-rs --version`, `doctor`, `capabilities`, and the command parser are implemented.
- The Java-compatible `generate-vanilla-delegated-region-plan-parallel` alias is recognized in addition to the documented
  `generate-vanilla-delegated-plan-parallel` command.
- `rust/scripts/build.ps1`, `test.ps1`, and `run.ps1` are implemented; `run.ps1` preserves CLI exit codes through
  `$LASTEXITCODE`.
- The progress CSV file name and header are centralized in Rust and match the Java batch contract.
- Verified locally with `cargo 1.96.0` / `rustc 1.96.0`:
  - `rust/scripts/build.ps1`
  - `rust/scripts/test.ps1 -NoBuild`
  - `rust/scripts/test.ps1 -NoBuild -Isolated`
  - `earthmap-rs --version`
  - `earthmap-rs doctor`
  - recognized incomplete commands return the not-implemented status.
- Java CLI changes currently present in the workspace are Phase 0 oracle-fixture additions, not Phase 1 Rust shell
  requirements.

## Phase 2: Minecraft Binary Core

Port first:

- `BlockStateIds`
- `DimensionProfile`
- `ChunkGenerationStatus`
- `PackedLongArray`
- `SectionPalette`
- `Heightmap`
- `HeightmapCalculator`
- `ChunkModel`
- `Nbt`
- `NbtIo`
- `ChunkNbtEncoder`
- `LevelDatTemplate`

Compatibility details:

- NBT must use big-endian numeric encoding.
- String encoding must match Java `DataOutputStream.writeUTF`.
- Compound tag insertion order must match Java `LinkedHashMap` order.
- Empty root name handling must match Java.
- Packed long arrays must match Java bit layout exactly.
- Java signed integer and long overflow behavior must be explicitly reproduced with wrapping arithmetic.

Acceptance:

- Rust chunk NBT bytes match Java for synthetic chunks.
- Flat world and palette stress world payload hashes match Java.
- Level.dat either byte-matches Java gzip output or proves identical uncompressed NBT with documented gzip header delta.

Bootstrap implementation status on 2026-06-03:

- Ported with Rust parity unit tests:
  - `BlockStateIds`
  - `DimensionProfile`
  - `ChunkGenerationStatus`
  - `PackedLongArray`
  - `SectionPalette`
  - `ChunkModel`
  - `Heightmap`
  - `HeightmapCalculator`
  - NBT/NBT IO bootstrap:
    - big-endian primitive/array encoding
    - Java `DataOutputStream.writeUTF` compatible modified UTF-8
    - compound insertion order and replacement behavior
    - gzip read/write round trip
    - fixed level.dat gzip fixture comparison with documented `deflate-stream` delta
  - `ChunkNbtEncoder` bootstrap:
    - root field order and empty root name
    - section palette and biome palette encoding
    - heightmap predicates and packed storage
    - block entity list ordering
- Java/Rust byte-identical NBT fixture comparison is available through:
  - Java CLI: `write-nbt-parity-fixtures <outputDir>`
  - Rust CLI: `write-nbt-parity-fixtures <outputDir>`
  - Script: `rust/scripts/compare-nbt-fixtures.ps1`
- Verified locally with `cargo test --manifest-path rust/Cargo.toml --workspace --locked`.
- Verified locally with
  `rust/scripts/compare-nbt-fixtures.ps1 -OutputRoot D:\earthmap\rust-port-golden\nbt-byte-fixtures-20260603-0006`,
  covering:
  - primitive and nested NBT payloads
  - empty root name
  - modified UTF-8 edge strings
  - empty, mixed-biome, block-entity, and all-block-state chunk NBT payloads
  - fixed-`LastPlayed` level.dat root NBT payload
- Latest Java/Rust byte-identical fixture hashes:

```text
fixture,bytes,sha256
nbt-primitive-root.dat,215,75BA3D49367F9A3AF607882538ADB98DFB4EA3983FCF06447D1DFF3D557BB6F0
nbt-nested-empty-root.dat,183,EC2FDFC45A13E8F40B470481032624E556CD1AB5EE550C484C0A079D50B97533
chunk-empty-full.nbt,1542,F53514AD500194EC5EC7847EDEDE8DD159747B408BB12A875785C62797EADB00
chunk-mixed-biome.nbt,3794,C13E66372B6FF3D70C05CE73C2DE350BF69798D0A6CAA057C0453055A776C845
chunk-block-entities.nbt,4012,AE141CD7845CAAE0F10F77AD7C7C52855F5169AB15914D9CD1F519270C7BD12B
chunk-all-block-states.nbt,8416,5B935EF12924CD99AD68CFD2E58A62A5CD9384BA21598C0F328BF9644B430E38
leveldat-root-fixed.nbt,3366,A44B2D4FB886093B4164028C20C915FB0C72C142489C306D2977F31C52156B38
```
- Java/Rust level.dat gzip fixture comparison is available through:
  - Java CLI: `write-nbt-gzip-parity-fixtures <outputDir>`
  - Rust CLI: `write-nbt-gzip-parity-fixtures <outputDir>`
  - Script: `rust/scripts/compare-nbt-gzip-fixtures.ps1`
- Verified locally with
  `rust/scripts/compare-nbt-gzip-fixtures.ps1 -OutputRoot D:\earthmap\rust-port-golden\nbt-gzip-fixtures-20260603-0003`.
  The compressed bytes differ, but decompressed payload parity is proven:

```text
fixture,javaCompressedBytes,rustCompressedBytes,javaCompressedSha256,rustCompressedSha256,compressedByteIdentical,decompressedBytes,decompressedSha256,gzipDelta
leveldat-root-fixed.nbt.gz,1393,1395,6F70DC631AA5F6FC96E284619468B2B3233C560A9D8AD2B4DC8E613A6DA943F6,64B9660D32CCAC3A1B138087BA92628E512F0F3F05EC3B4AE74DFF5C89E522A6,False,3366,A44B2D4FB886093B4164028C20C915FB0C72C142489C306D2977F31C52156B38,deflate-stream
```
- `rust/scripts/compare-nbt-fixtures.ps1`, `rust/scripts/compare-nbt-gzip-fixtures.ps1`, and
  `rust/scripts/compare-region-writer-fixtures.ps1` validate wrapper paths and restrict `-OutputRoot` to a child of
  `D:\earthmap\rust-port-golden` before recursively replacing output directories.
- `cargo fmt --all` is available after installing the `rustfmt` component.
- Remaining Phase 2 work: none for the fixed NBT/level.dat bootstrap. Region compression byte-exactness is tracked in
  Phase 3. The fixed-`LastPlayed` level.dat root has byte-identical decompressed NBT and a documented gzip
  `deflate-stream` delta.

## Phase 3: MCA And Linear V2 Writers

Port:

- `McaRegionWriter`
- `McaRegionValidator`
- `LinearV2RegionWriter`
- `LinearV2RegionValidator`
- palette/status scanners needed by quality gates.

Compatibility details:

- MCA chunk order must match `ChunkLocalPos` sorted order.
- MCA timestamp remains `0` where Java passes `0`.
- MCA zlib payload must match Java deflater behavior.
- Linear V2 superblock, version, grid, existence bitmap, bucket order, xxhash, and zstd level must match Java.
- Linear bucket order is `bucketX` outer, `bucketZ` inner, then `cellX` outer, `cellZ` inner.

Acceptance:

- Java and Rust MCA files match for all Phase 2 fixtures.
- Java and Rust Linear files match for all Phase 2 fixtures.
- `compare-mca-linear-region-payloads` passes on Rust output.

Bootstrap implementation status on 2026-06-03:

- Rust `earthmap-region` now includes writer bootstrap APIs for:
  - `write_mca_region`
  - `write_linear_v2_region`
  - Java-compatible `ChunkLocalPos` ordering by MCA/Linear header index
  - MCA sector sizing and zlib-compressed chunk records
  - Linear V2 superblock/version/grid/header, existence bitmap, bucket order, xxHash64 bucket hashes, and zstd level 4
- Rust `flate2` is configured with the `zlib-default` backend through `libz-sys`. This matches Java
  `DeflaterOutputStream` bytes for MCA region records in the Phase 2 chunk fixture region corpus. The faster
  `zlib-rs` and `miniz_oxide` backends were checked on the same corpus and preserved decompressed payload parity but
  produced different MCA compressed streams, so they remain benchmark-only candidates until a Java-compatible mode is
  proven.
- `flate2`'s gzip encoder still differs from Java `GZIPOutputStream` on level.dat gzip container bytes while preserving
  decompressed NBT parity.
- The Linear V2 writer flushes the zstd stream before `finish()` so Rust emits the same final empty zstd block shape as
  Java `ZstdOutputStream` on the synthetic writer fixture.
- Rust unit tests cover:
  - `ChunkLocalPos` header-index ordering
  - MCA sector sizing
  - MCA writer round trip through the Rust payload reader
  - Linear V2 writer round trip through the Rust payload reader
  - Linear V2 existence bitmap mismatch rejection
  - Linear V2 negative per-chunk `data_size` rejection
- Java/Rust Phase 2 chunk fixture region comparison is available through:
  - Java CLI: `write-region-writer-parity-fixtures <outputDir>`
  - Rust CLI: `write-region-writer-parity-fixtures <outputDir>`
  - Script: `rust/scripts/compare-region-writer-fixtures.ps1`
- Verified locally with
  `rust/scripts/compare-region-writer-fixtures.ps1 -OutputRoot D:\earthmap\rust-port-golden\region-writer-fixtures-20260603-0007`.
  MCA and Linear V2 are byte-identical when the region files contain the Phase 2 chunk NBT fixtures
  (`chunk-empty-full`, `chunk-mixed-biome`, `chunk-block-entities`, and `chunk-all-block-states`):

```text
fixture,format,javaBytes,rustBytes,javaSha256,rustSha256,fileByteIdentical,payloadsMatch,delta
writer-mca/r.0.0.mca,mca,24576,24576,AF467033FCB40FAAD1962E8E0F14A4A0D1E40828C5C0CBBA6067265318794C32,AF467033FCB40FAAD1962E8E0F14A4A0D1E40828C5C0CBBA6067265318794C32,True,True,none
writer-linear/r.-2.3.linear,linear,2538,2538,33985D0189AC3481FC3E0B9856DC7F99A2180E2E21955D34B8EE740515FC9BE5,33985D0189AC3481FC3E0B9856DC7F99A2180E2E21955D34B8EE740515FC9BE5,True,True,none
```
- Remaining Phase 3 work: port the full validators/scanners beyond the bootstrap reader/writer subset and run the same
  byte-parity gate on generated production-scale region samples.

## Phase 4: Geo Readers And Raster Cache

Port:

- `EarthScaleMapping`
- `GeoTiffMetadata`
- `GeoTiffHeightmapReader`
- `GeoTiffFloat32Reader`
- `GeoTiffRgbReader`
- `VrtRgbMosaicReader`
- `GeoTiffRowCache`
- `HeightmapScalarSampler`

Performance design:

- Prefer memory-mapped files for read-only rasters.
- Keep bounded row/tile caches.
- Coalesce duplicate cache misses.
- Read rows/tiles in deterministic order for parity mode.
- Add a high-throughput mode only after parity mode is green.

Compatibility details:

- Pixel coordinate calculation must match Java `Math.floor`.
- Bilinear sampling must match Java `double` arithmetic and clamp behavior.
- BigTIFF parsing must preserve little-endian behavior and supported layout limits.
- NoData behavior must match Java readers.

Acceptance:

- Java and Rust sample coordinates match exactly for representative longitude/latitude points.
- Java and Rust height-only region chunk payloads match.
- Cache statistics may differ, but output must not.

Bootstrap implementation status on 2026-06-03:

- Rust `earthmap-geo` now includes:
  - `EarthScaleMapping`
  - Java-compatible WGS84 equatorial circumference scaling
  - Java-compatible longitude/latitude block conversion, including the Java comparison behavior for `NaN`
    longitude/latitude inputs
  - `GeoTiffMetadata`
  - Java-compatible `sample_type_name()` output for Int16, Float32, and fallback sample layouts
  - `GeoTiffHeightmapReader` bootstrap for the Java-supported synthetic path: little-endian BigTIFF magic 43,
    signed Int16 samples, no compression, `samplesPerPixel=1`, and `rowsPerStrip=1`
  - file-backed on-demand range reads for TIFF header, IFD/tag payloads, samples, and rows; the reader does not load
    the full heightmap into memory
  - `GeoTiffFloat32Reader` bootstrap for the Java-supported synthetic path: little-endian BigTIFF magic 43, Float32
    samples, no compression, `samplesPerPixel=1`, `rowsPerStrip=1`, `openIfPresent`, nearest sampling, raster-outside
    empty samples, and `GDAL_NODATA` empty samples
  - `GeoTiffRgbReader` bootstrap for the Java-supported synthetic path: Classic/BigTIFF byte order parsing,
    non-compressed interleaved RGB samples, tiled or stripped layouts, `RgbColor` availability semantics, out-of-range
    unavailable samples, and Java-compatible tile cache hit/miss/eviction counters
  - `VrtRgbMosaicReader` bootstrap for the Java-supported synthetic path: `quick-xml` pull parsing, VRT
    `GeoTransform`, band-1 `SimpleSource` resolution, single-source and split-source indexed lookup, nearest sampling,
    lazy RGB TIFF reader opening, aggregated tile cache statistics, and TrueMarble grid fallback probing
  - `HeightmapScalarSampler` bootstrap for the Java `rowCache == null` path, including nearest sampling and bilinear
    interpolation with Java-compatible pixel-center offset, floor, clamp, and lerp math
  - `GeoTiffRowCache` bootstrap with Java-compatible sequential hit/miss/eviction statistics, synchronous read-ahead
    prefetch counters, and cached sampler two-row local memo behavior
  - Rust CLI `inspect-heightmap` and `locate-heightmap-point` diagnostics backed by the file-backed GeoTIFF reader and
    Java-compatible `EarthScaleMapping`
  - Rust CLI `sample-vrt-rgb` diagnostic backed by `VrtRgbMosaicReader`, reporting Java-compatible nearest RGB samples,
    source lookup shape, lazy reader counts, and tile cache counters for real-data smoke checks
  - Rust `earthmap-surface` height-only region bootstrap matching Java `HeightOnlyRegionGenerator`: Java-style
    `Math.round` height scaling, bedrock/stone/dirt/grass column fill, level.dat write, exploration-only survival
    manifest, MCA/Linear region output, and Java-compatible CLI report lines
- Rust unit tests cover the Java `EarthScaleMappingTest` 1:500 constants, global 1:1000 rounding, longitude/latitude
  boundary behavior, Java-compatible `NaN` fallthrough, `GeoTiffMetadata.sampleTypeName()` strings, synthetic BigTIFF
  metadata parsing, pixel coordinate transforms, signed Int16 sample reads, row reads, reader edge validation, and the
  Java `HeightmapScalarSamplerTest` nearest/bilinear values. The row-cache tests cover the Java sequential
  hit/miss/eviction fixture, prefetch counters, repeated cached bilinear row reuse, and very large Rust-side
  `prefetchRows` saturation. Float32 tests cover metadata, direct pixel reads, nearest sampling, NoData handling,
  missing-file `openIfPresent`, and invalid layout rejection. RGB tests cover the Java `GeoTiffRgbReaderTest`
  Classic TIFF fixture path, pixel values, unavailable out-of-range samples, near-black color semantics, and tile cache
  statistics. VRT tests cover the Java `GeoTiffRgbReaderTest` single-source and split-source VRT fixtures, indexed
  source lookup cells, nearest RGB sampling, lazy reader opens, and aggregated tile cache statistics. CLI tests cover
  Java-compatible diagnostic stdout for synthetic BigTIFF and VRT RGB fixtures.
- Verified locally with
  `cargo test --manifest-path rust/Cargo.toml -p earthmap-geo --locked`,
  `cargo test --manifest-path rust/Cargo.toml -p earthmap-cli --locked`, and
  `cargo test --manifest-path rust/Cargo.toml --workspace --locked`.
- Real-heightmap smoke checks against `E:\HQheightmap.tif` passed on 2026-06-03:
  - Java/Rust `inspect-heightmap E:\HQheightmap.tif` stdout parity OK
  - Java/Rust `locate-heightmap-point E:\HQheightmap.tif 5000 0.0 0.0` stdout parity OK
  - evidence folder: `D:\earthmap\rust-port-golden\phase4-real-heightmap-smoke-20260603-0002`
- Real VRT RGB smoke checks against `D:\earthmap\TifFiles\terrain\TrueMarble.vrt` passed on 2026-06-03:
  - Java `VrtRgbMosaicReader` oracle / Rust `sample-vrt-rgb` stdout parity OK
  - sample points: `(0.0, 0.0)`, `(32.0, 0.0)`, `(-73.9857, 40.7484)`, `(139.6917, 35.6895)`
  - evidence folder: `D:\earthmap\rust-port-golden\phase4-real-vrt-rgb-smoke-20260603-0001`
- Height-only region parity against `E:\HQheightmap.tif` passed on 2026-06-03 for region `r.0.0` at scale `1:5000`:
  - Linear V2 Java/Rust region SHA-256:
    `4764167E5820E9A2E5BD834053C22393563FD9FC4D2D896DC06BEE1AE77B9EC0`
  - MCA Java/Rust region SHA-256:
    `083BC7F1038FA5C5776C85860EA46CB35EBED6936403974CCA1A2477A0C3C036`
  - payload manifest parity OK for both formats
  - normalized CLI stdout parity OK for both formats
  - exploration-only survival manifest parity OK for both formats
  - evidence folders:
    `D:\earthmap\rust-port-golden\phase4-height-region-parity-20260603-0001`
    and `D:\earthmap\rust-port-golden\phase4-height-region-parity-20260603-0002-mca`
- Additional height-only region parity against `E:\HQheightmap.tif` passed on 2026-06-03 for region `r.-1.-1` at scale
  `1:5000`:
  - Linear V2 Java/Rust region SHA-256:
    `8578E86D037A2BC0FE2AEB1DB3608502D4BF89DC8FFE4D00974BD5EA3C64DE91`
  - MCA Java/Rust region SHA-256:
    `9C0F01C4681F3078DEFAA72A48DEB07B848F25988B4CADADAF35D264C6CF631F`
  - payload manifest parity OK for both formats
  - normalized CLI stdout parity OK for both formats
  - exploration-only survival manifest parity OK for both formats
  - evidence folder:
    `D:\earthmap\rust-port-golden\phase4-height-region-parity-20260603-0003-r-neg1-neg1`
- Remaining Phase 4 work: none for the current GeoTIFF/VRT reader, cache, diagnostic CLI, and height-only region
  bootstrap scope. Broader surface/photo/OSM behavior moves to Phase 5.

## Phase 5: Surface Rules And Photo Solver

Port:

- `EarthSurfaceRules`
- `EarthSurfaceColumn`
- `SurfaceMaterialSample`
- `EarthDataSurfaceMaterialSampler`
- `TrueMarbleSurfaceMaterialSampler`
- `EarthSurfaceRegionSampler`
- `EarthSurfaceChunkSampler`
- `EarthSurfaceMaterialClassifier`
- `PhotoSurfaceMaterialClassifier`
- `PhotoSurfaceSolver`
- `SurfaceBiomeIntentCompiler`
- `SurfaceBiomeFamilyIntentGrid`
- `SurfaceClassSmoother`
- `CoastalSurfaceCleaner`
- `NaturalSurfaceBlockPolicy`
- `SurfaceBiomeCellWriter`
- OSM overlay and mask support
- delegated chunk status and server-delegation manifest metadata

Compatibility details:

- Reproduce Java `String.hashCode` where used.
- Reproduce Java `Math.round`, `Math.floor`, `Math.sin`, `Math.cos`, `Math.sqrt`, and `Math.pow` decisions closely.
- Use `f64` as the default scalar type.
- Stabilization and smoothing passes must run in the same logical order as Java.
- Parallel row processing must not change final column decisions.
- Production output must still enforce natural surface policy.
- Vanilla-delegated outputs must preserve Java's `directCaves`, `directOres`, `directVegetation`, `directStructures`,
  and `directProgressionStructures` manifest flags.

Acceptance:

- `classify-surface-point` parity for a point corpus.
- `generate-surface-region` payload parity for fixed regions.
- `quality-production-sample-batch` produces Java-equivalent current render and metric artifacts.

Bootstrap implementation status on 2026-06-03:

- Rust `earthmap-surface` includes an `EarthSurfaceRules` bootstrap with Java-compatible `classify`,
  `classifyShaped`, vertical scale validation, water-column normalization, biome selection, top/filler block selection,
  deterministic value-noise scoring, and Java-style rounding for the Java `EarthSurfaceRulesTest` fixture cases.
- Rust CLI `classify-surface-point <heightmap> <scale> <longitude> <latitude>` is implemented as a diagnostic command
  backed by the file-backed heightmap reader, `GeoTiffRowCache`, `HeightmapScalarSampler`, and the Rust
  `EarthSurfaceRules` bootstrap.
- Real `classify-surface-point` smoke checks against `E:\HQheightmap.tif` passed on 2026-06-03:
  - Java/Rust stdout parity OK for `origin`, `sahara`, `amazon`, `korea`, and `everest` sample points
  - evidence folder:
    `D:\earthmap\rust-port-golden\phase5-classify-surface-point-smoke-20260603-0001`
- Rust `SurfaceMaterialSample` and `SurfaceDataEvidence` bootstrap matches Java fixture cases for color-only samples,
  terrain-token source normalization, coverage/canopy/slope/ecoregion helpers, Java-style optional rounding, and
  evidence bit flags.
- Rust `SurfaceMaterialSampler` and `TrueMarbleSurfaceMaterialSampler` bootstrap wraps VRT RGB averaged samples as
  Java-compatible color-only `SurfaceMaterialSample` output for synthetic VRT fixtures.
- Rust `GeoTiffSingleBandReader` bootstrap opens uncompressed unsigned 8/16-bit single-band GeoTIFF rasters with
  Java-compatible nearest sampling, coordinate flooring, and NoData handling for synthetic Classic TIFF fixtures.
- Rust EarthData sampler helper bootstrap matches Java fixture cases for raster stat deltas, slope permille
  normalization, cell-degree clamps, longitude wrapping, and quantized cache keys.
- Rust `MetTerrainVocabulary` bootstrap ports Java's exact lookup and ImageMagick-style octree nearest remap for
  standard terrain-token synthesis fixture cases.
- Rust surface terrain-token synthesis bootstrap matches Java `EarthDataSurfaceMaterialSampler.withTerrainToken`
  fixture cases for exported-token priority and Java standard palette fallback.
- Rust `EarthDataSurfaceMaterialSampler` bootstrap opens TrueMarble plus optional climate, vegetation,
  ocean-temperature, bathymetry, and slope rasters, then samples Java-compatible quantized land/water
  `SurfaceMaterialSample` fixture cases with bounded cache reuse and terrain-token fallback. Optional auxiliary raster
  open failures are tolerated like Java.
- Rust EarthData ecoregion evidence bootstrap ports `EcoregionSample` normalization, ecoregion cell-degree clamps,
  Java biome-family grouping, and 3x3 family-confidence aggregation with bounded access-order cache fixture cases.
- Rust `WwfEcoregionSampler` bootstrap reads Java-generated WWF ecoregion cache files, runs Java-compatible
  point-in-polygon sampling, and lets `EarthDataSurfaceMaterialSampler` auto-load cache-backed ecoregion evidence when
  present.
- Remaining Phase 5 work: port WWF shapefile/DBF source parsing and cache freshness regeneration, MET export
  terrain-token raster integration, land-shallow-topographic/photo source selection, photo solver, smoothing/coastal
  policies, OSM overlay, surface biome cell writing, and fixed-region `generate-surface-region` payload parity.

### Non-Phase: Vanilla-Delegated Survival Scope

Direct gameplay population is not part of the Rust phase plan. Production Java output is centered on
`generate-vanilla-delegated-*` commands: EarthMap writes Earth-shaped terrain chunks and metadata, then vanilla
Minecraft/DivineMC continues chunk generation when chunks are loaded or force-loaded.

Therefore Rust must not introduce direct generators for vanilla-owned gameplay features such as ores, caves,
vegetation, strongholds, End portals, loot chests, or spawners as part of Java-output parity. The Rust port should
preserve Java's delegated chunk status, manifest flags, world metadata, finalization command generation, and evidence
scanners/validators that inspect server-produced results.

Legacy Java classes or reports around ores/progression are compatibility evidence only unless a separate decision
reinstates direct generation. They are not a required output-generation phase for this Rust port, and no dedicated
"Survival And Gameplay" implementation phase should be reintroduced without that decision.

## Phase 6: Quality And Visual Evidence Tools

Port or bridge:

- `McaTopDownRenderer`
- `PhotoParityHarness`
- `DynmapTileMosaicBuilder`
- contact sheet generation
- metric reports
- vanilla-delegation manifest/evidence validators and scanners
- nation-war readiness reports

Acceptance:

- Five-crop quality command emits the same artifact tree as Java.
- Metric values match Java within exact textual output where feasible.
- Any floating text formatting delta is documented and normalized in the comparator before promotion.
- Delegated survival evidence reports match Java for the same server-produced artifacts.

## Phase 7: Performance Optimization

Only start after Phase 5 parity is green for surface output.

Optimization targets:

- Region-owned SoA buffers.
- Rayon-based chunk and row parallelism.
- Dedicated compression pool.
- Bounded disk writer queue.
- Reused NBT buffers.
- Interned palette/biome names.
- SIMD for simple raster transforms and distance/smoothing kernels.
- Work stealing across regions with deterministic final write order.

Initial performance gates:

```text
Gate A: Rust parity mode <= Java wall time for one-region v165 profile.
Gate B: Rust optimized mode >= 2x Java throughput for one-region v165 profile.
Gate C: Rust optimized mode >= 3x Java throughput for 9-region mixed sample.
Gate D: Rust optimized mode remains bounded in memory during representative batch generation.
```

Long-term target:

```text
Full 1:5000 validation sample: at least 5x faster than Java on the same machine.
Full production scale run: throughput limited by raster IO or compression, not per-column object allocation.
```

## Determinism Checklist

Before accepting any Rust implementation, verify:

- No unordered map iteration affects output order.
- No floating-point fast-math flags are used in parity mode.
- No parallel reduction changes decisions.
- All timestamps that affect files are fixed or oracle-compatible.
- Compression levels and library versions are pinned.
- Path separators in manifests are normalized only where Java already normalizes them.
- Logs and progress CSV preserve expected headers and status names.

## Rust Dependency Policy

Research date: 2026-06-03. Re-check crate documentation and release notes before
the Rust workspace is initialized or dependency versions are bumped.

Start small. Every dependency must have a role in parity, performance, or
operational clarity. The port may use high-performance Rust libraries, but no
library is allowed to define canonical output unless it passes the Java parity
suite.

General rules:

- Commit `Cargo.lock` once the Rust workspace exists.
- Keep output-critical code behind local adapters so dependencies can be swapped
  without touching generator logic.
- Split features into `parity`, `fast`, and `research` profiles:
  - `parity`: deterministic output, pinned compression behavior, no fast-math,
    stable iteration order, and scalar fallback available for SIMD code.
  - `fast`: enables faster compression backends, custom allocators, SIMD, and
    larger caches only after parity is already proven.
  - `research`: enables experimental readers or encoders used for comparison,
    fuzzing, and benchmark spikes.
- Prefer custom writers for output-critical binary formats:
  - NBT canonical output
  - `.linear` and `.mca` region layout
  - CSV progress rows
  - PNG artifacts that are compared byte-for-byte
- Treat compression backend changes as parity-impacting. If byte-identical
  compressed region files are required, the backend and level become part of the
  compatibility contract. If payload-identical output is accepted for some gate,
  record that gate explicitly and still keep final release gates byte-exact.
- Do not use unordered map iteration to produce user-visible or file-visible
  output. Use `Vec`, sorted keys, `BTreeMap`, or explicit index order.
- Native dependencies are allowed only behind narrow adapters and platform CI.
  Windows support is mandatory.

### Recommended Library Matrix

| Area | Recommended crates | Role in the Rust port | Parity and performance notes |
| --- | --- | --- | --- |
| CLI and config | `clap`, `serde`, `serde_json`, `toml`, `camino`, `anyhow`, `thiserror` | Command compatibility, config parsing, and error reporting. | `clap` should mirror current Java CLI names, defaults, and validation messages where tests depend on them. |
| Logging and progress | `tracing`, `tracing-subscriber`, `tracing-appender`, `csv` | Structured diagnostics and Java-compatible progress CSV output. | CSV output must use the same column order, newline behavior, and numeric formatting as Java. |
| Parallel execution | `rayon`, `crossbeam-channel`, `parking_lot`, `thread_local` | Region/chunk/tile fan-out, bounded pipelines, lower-overhead locks, and per-thread scratch buffers. | Rayon work scheduling must not affect output order. All writes must be ordered by region/chunk coordinate, not task completion time. |
| Shared caches | `lru`, `dashmap` | Optional tile, palette, or decoded-raster caches. | Use only for non-canonical memoization. Never iterate `dashmap` to write output. |
| File IO | `memmap2`, `tempfile`, `walkdir`, `fs4` | Memory-mapped raster access, atomic temp outputs, fixture scanning, and optional output locks. | Mmap must preserve explicit bounds checks around GeoTIFF/VRT window reads. |
| Text and number parsing | `lexical-core`, `simdutf8` | Faster numeric parsing and UTF-8 validation for VRT, manifests, CSV-like fixtures, and diagnostics. | Use only after Java-compatible parsing and formatting are snapshotted. Fast parsers must not accept/reject values differently on compatibility paths. |
| Binary layout | `byteorder`, `bytes`, `smallvec`, `arrayvec`, `bytemuck`, `zerocopy` | Explicit endian writes, reusable buffers, compact stack-friendly collections, and safe byte views. | Use zero-copy casts only for validated plain-old-data structures. Prefer explicit endian writes for file formats. |
| Compression | `flate2` with `zlib-default`/`libz-sys`, `zstd`, `zlib-rs`, `crc32fast` | Region compression adapters and checksums. | Use `zlib-default` for MCA byte parity with Java `DeflaterOutputStream`. Benchmark `zlib-rs` behind an opt-in fast mode only where compressed bytes are not canonical. Keep decompressed-payload parity and compressed-byte parity as separate measured facts. |
| Hashing and fingerprints | `sha2`, `xxhash-rust` | Golden corpus manifests, fast local cache keys, and byte comparison reports. | Use SHA-256 for correctness manifests. Use xxHash only for non-security cache keys or benchmark diagnostics. |
| Raster and image inputs | `tiff`, `png`, `image`, `quick-xml`, `roxmltree`; `gdal` only as `research` | GeoTIFF/PNG/VRT fixture decoding and validation utilities. | The current Java pipeline includes VRT/TrueMarble behavior; use pure Rust crates where they match the data model, but keep a custom tiled reader if BigTIFF, VRT transforms, or exact sampling require it. `gdal` is useful for validation/conversion spikes, not as the default runtime dependency. |
| Geospatial math | `geo`, `geo-types`, `rstar`, `static_aabb2d_index`, or `geo-index` | Coordinate types, bounds checks, and spatial indexes for overlays or OSM features. | Port Java projection and rounding formulas directly first. Use immutable packed indexes for large read-only feature sets only after feature ordering is proven stable. |
| OSM/PBF | `osmpbf`, `osmpbfreader`, `prost`, `snap` | PBF ingestion candidates and lower-level custom decoder path. | Use high-level readers for initial parity spikes. If OSM becomes a hot path, implement a custom `prost` + `snap` streaming pipeline with stable feature ordering. |
| Minecraft/NBT | local custom encoder, plus `fastnbt`, `quartz_nbt`, or `valence_nbt` for comparison only | Canonical Minecraft NBT emission and fixture validation. | Do not adopt a third-party NBT writer for canonical output unless it produces byte-identical Java fixtures for every compound/list/tag ordering case. |
| SIMD and numeric kernels | scalar baseline, then `wide` or `pulp`; optionally `bytemuck` for lane-safe loads | Palette lookup, raster sampling, coordinate transforms, and mask operations. | Build scalar parity first. SIMD implementations must be differential-tested against scalar output on edge coordinates. Check current `std::simd` stability before considering it. |
| Benchmarking | `criterion`, `divan`, `pprof`, `hdrhistogram` | Microbenchmarks, throughput comparisons, CPU profiles, and latency distributions. | Criterion is the default benchmark harness. Add whole-region wall-clock benchmarks outside the microbench suite. |
| Testing | `proptest`, `assert_cmd`, `predicates`, `insta`, `similar-asserts`, `tempfile`, `walkdir` | Property tests, CLI tests, snapshot diagnostics, temp output fixtures, and corpus traversal. | Golden output tests must compare Java and Rust artifacts byte-for-byte unless a gate explicitly says payload-level comparison. |
| Allocator tuning | `mimalloc`, `tikv-jemallocator` | Optional allocator experiments for high-throughput generation. | Keep allocator changes behind features. Measure on Windows and Linux separately; never make allocator behavior part of correctness. |

### Dependency Stance by Output Surface

| Output surface | Default stance | Reason |
| --- | --- | --- |
| CLI text and exit codes | Use `clap`, but snapshot behavior | Users and scripts may depend on messages and failure modes. |
| Progress CSV | Use `csv` or manual writer after fixture comparison | CSV byte formatting is visible and easy to regress. |
| NBT payload | Custom encoder | Tag order, endian writes, list layout, and numeric encoding must match Java. |
| `.mca` and `.linear` files | Custom writer | Region layout is a core compatibility surface. |
| Compression | Adapter around selected backend | Backend, level, and header choices can affect byte output. |
| Raster decoding | Library-assisted, with custom hot path allowed | GeoTIFF/VRT support is broad; performance and exact sampling may require local code. |
| PNG preview/debug artifacts | Library-assisted unless byte-exact output is required | PNG encoders can produce visually identical but byte-different files. |

### Initial Cargo Shape

Use a workspace so parity tests, benchmarks, and internal libraries stay cleanly
separated:

```toml
[workspace]
members = [
  "crates/earthmap-cli",
  "crates/earthmap-core",
  "crates/earthmap-geo",
  "crates/earthmap-surface",
  "crates/earthmap-minecraft",
  "crates/earthmap-region",
  "crates/earthmap-gameplay",
  "crates/earthmap-quality",
  "crates/earthmap-parity",
]
resolver = "2"

[workspace.dependencies]
anyhow = "1"
arrayvec = "0.7"
assert_cmd = "2"
bytemuck = "1"
camino = "1"
clap = { version = "4", features = ["derive"] }
crossbeam-channel = "0.5"
csv = "1"
flate2 = { version = "1", default-features = false, features = ["zlib-default"] }
geo = "0.30"
geo-types = "0.7"
memmap2 = "0.9"
parking_lot = "0.12"
png = "0.17"
predicates = "3"
proptest = "1"
rayon = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
smallvec = "1"
tempfile = "3"
thiserror = "2"
tiff = "0.11"
tracing = "0.1"
tracing-subscriber = "0.3"
walkdir = "2"
xxhash-rust = { version = "0.8", features = ["xxh3"] }
zstd = "0.13"
```

Do not treat this snippet as a final lockfile. It is a starting point for the
first Rust spike; exact versions should be resolved once, committed, and then
changed only with parity and benchmark evidence.

### Library Evaluation Tasks

Before implementing the full generator, run small spikes and record results in
`docs/DECISIONS.md`:

1. `flate2` backend parity spike:
   - Encode representative NBT payloads with the candidate backend and level.
   - Compare compressed bytes with Java outputs.
   - If compressed bytes differ, compare decompressed payloads and decide
     whether the final compatibility target requires byte-identical compressed
     regions.
2. GeoTIFF/VRT reader spike:
   - Read windows that cross tile boundaries.
   - Compare Java and Rust sampled pixel values at projection edge cases.
   - Measure mmap vs buffered IO on the current TrueMarble fixture.
3. NBT writer spike:
   - Generate all primitive tags, nested compounds, lists, empty arrays, and
     chunk sections.
   - Compare against Java byte fixtures.
4. OSM/PBF reader spike:
   - Compare `osmpbfreader`/`osmpbf` against a low-level `prost` + `snap`
     pipeline.
   - Measure decode throughput and memory on the same bounding box.
5. SIMD palette spike:
   - Implement scalar first.
   - Add `wide` or `pulp` variant behind a feature.
   - Differential-test every result against scalar output before benchmarking.
6. Allocator spike:
   - Benchmark default allocator vs `mimalloc` on Windows.
   - Keep allocator disabled by default unless whole-region generation improves
     without increasing memory or destabilizing CI.

### Primary References Checked

- [`rayon`](https://docs.rs/rayon/) for data-parallel iterators and thread-pool
  control.
- [`crossbeam-channel`](https://docs.rs/crossbeam-channel/) for bounded pipeline
  channels.
- [`memmap2`](https://docs.rs/memmap2/) for memory-mapped input files.
- [`flate2`](https://docs.rs/flate2/), [`libz-sys`](https://docs.rs/libz-sys/), and
  [`zstd`](https://docs.rs/zstd/) for Java-compatible region compression adapters.
- [`zlib-rs`](https://docs.rs/zlib-rs/) for optional high-performance deflate
  backend evaluation.
- [`tiff`](https://docs.rs/tiff/), [`png`](https://docs.rs/png/),
  [`image`](https://docs.rs/image/), and [`quick-xml`](https://docs.rs/quick-xml/)
  for raster/VRT-related IO.
- [`gdal`](https://docs.rs/gdal/) and the upstream
  [GDAL documentation](https://gdal.org/en/stable/) for native geospatial
  validation and conversion research.
- [`geo`](https://docs.rs/geo/), [`geo-types`](https://docs.rs/geo-types/), and
  [`rstar`](https://docs.rs/rstar/) for geometry and spatial indexing.
- [`static_aabb2d_index`](https://docs.rs/static_aabb2d_index/) and
  [`geo-index`](https://docs.rs/geo-index/) for immutable packed spatial index
  experiments.
- [`osmpbfreader`](https://docs.rs/osmpbfreader/), [`prost`](https://docs.rs/prost/),
  and [`snap`](https://docs.rs/snap/) for OSM/PBF ingestion options.
- [`fastnbt`](https://docs.rs/fastnbt/), [`quartz_nbt`](https://docs.rs/quartz_nbt/),
  and [`valence_nbt`](https://docs.rs/valence_nbt/) for NBT comparison and
  fixture tooling.
- [`wide`](https://docs.rs/wide/), [`pulp`](https://docs.rs/pulp/),
  [`bytemuck`](https://docs.rs/bytemuck/), and
  [`zerocopy`](https://docs.rs/zerocopy/) for SIMD and byte-layout experiments.
- [`lexical-core`](https://docs.rs/lexical-core/) and
  [`simdutf8`](https://docs.rs/simdutf8/) for high-throughput parsing
  experiments.
- [`criterion`](https://docs.rs/criterion/), [`divan`](https://docs.rs/divan/),
  and [`pprof`](https://docs.rs/pprof/) for benchmark and profiling support.
- [`proptest`](https://docs.rs/proptest/), [`assert_cmd`](https://docs.rs/assert_cmd/),
  and [`tempfile`](https://docs.rs/tempfile/) for parity and CLI tests.
- [`mimalloc`](https://docs.rs/mimalloc/) and
  [`tikv-jemallocator`](https://docs.rs/tikv-jemallocator/) for allocator
  experiments.

## Promotion Gates

Phase promotion requires:

- Java tests pass.
- Rust tests pass.
- Golden corpus comparison passes for the phase scope.
- Performance did not regress beyond the phase budget.
- Any known delta is written in `docs/DECISIONS.md`.
- Quality gates remain in the same state or improve; they must not be skipped.

Full Rust replacement requires:

- All initial command compatibility implemented.
- Java and Rust corpus parity green.
- Five-crop quality gate green on the same build/config.
- Representative sample gate green.
- Server/Dynmap gate green.
- Recovery/resume behavior tested.
- Java fallback retained until at least one complete accepted production-scale rehearsal.

## Immediate Next Actions

1. Done: create `rust/` workspace and wrapper scripts.
2. Bootstrap done: implement Java synthetic golden corpus generation.
3. Bootstrap done: implement MCA/Linear payload extraction and SHA-256 manifests.
4. Next: expand Phase 0 golden corpus coverage beyond synthetic flat/palette fixtures.
5. Next: add Phase 2 gzip comparison/delta reporting.
6. Next: prove synthetic chunk, MCA, and Linear parity from Rust-generated chunks.
7. Only then start raster and surface logic.
