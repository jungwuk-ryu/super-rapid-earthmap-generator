# Operations

## Prerequisites

- Rust toolchain with `cargo` on `PATH`
- PowerShell 7 or Windows PowerShell for the repository scripts; native `cargo` commands also work from
  Bash-compatible shells
- A HeightMap GeoTIFF, provided as an explicit CLI argument or `EARTHMAP_HEIGHTMAP`
- A `TifFiles` dataset root, provided as `EARTHMAP_TIF_ROOT` or as `EARTHMAP_DATA_ROOT/TifFiles`
- `terrain/TrueMarble.vrt` under that `TifFiles` root, or another explicit `surfaceRaster` path
- ImageMagick only for optional archived reference-image workflows

Portable setup example:

```powershell
$env:EARTHMAP_HEIGHTMAP = "<absolute-path-to-heightmap.tif>"
$env:EARTHMAP_DATA_ROOT = "<absolute-path-to-earthmap-data>"
$env:EARTHMAP_TIF_ROOT = Join-Path $env:EARTHMAP_DATA_ROOT "TifFiles"
$env:EARTHMAP_SURFACE_RASTER = Join-Path $env:EARTHMAP_TIF_ROOT "terrain\TrueMarble.vrt"
$env:EARTHMAP_OUTPUT_ROOT = "<absolute-path-to-output-directory>"
```

Use the same variable names with POSIX paths on Linux/macOS.

## Normal Commands

```powershell
.\scripts\build.ps1
.\scripts\test.ps1
cargo run --manifest-path rust/Cargo.toml -p earthmap-cli --bin earthmap-rs -- --help
```

Rust GUI:

```powershell
cargo run --manifest-path rust/Cargo.toml -p earthmap-gui --bin earthmap-gui
cargo run --manifest-path rust/Cargo.toml -p earthmap-gui --bin earthmap-gui -- --cli --version
```

The GUI launches `earthmap-rs` as a separate generator process and reads progress from stdout. Keep preview rendering
out of the GUI path; generation speed must remain governed by the CLI process.

GUI data setup:

- Select the HeightMap GeoTIFF. This controls terrain height, coast shape, water/land, and ocean depth.
- Select the `TifFiles` root. The GUI fills `terrain\TrueMarble.vrt` from that root.
- Keep the satellite raster as `TrueMarble.vrt` for photo-like terrain. Clearing it uses `surfaceRaster=auto`.
- Optional companion rasters are discovered relative to the same `TifFiles` root when present:
  `climate.tif`, `vegetation\*.tif`, `ocean_temp_infill.tif`, `bathymetry.tif`, and `slope.tif`.

GUI area setup:

- Choose **Whole Earth** to generate the full mapped Earth. At scale `1000`, this resolves to roughly `80 x 40`
  Minecraft regions and should be run to a fast disk.
- Choose **Preset area** for common regions such as Australia, Korea, Europe, Japan, or the contiguous United States.
- Choose **Latitude/longitude box** to enter west/east/north/south decimal degrees.
- Choose **Advanced region grid** only when you already know the Minecraft region coordinates.
- Scale denominator `1000` means roughly one Minecraft block per kilometer at the equator. Smaller values generate
  larger worlds; larger values generate smaller worlds.

GUI generation setup:

- Choose `linear` for DivineMC Linear region output or `mca` for classic Minecraft region output.
- Linear compression is zstd level `1..22`; default is `4`. Lower values are faster, higher values can produce smaller
  `.linear` files.
- MCA compression is zlib level `0..9`; default is `6`. Lower values are faster, higher values can produce smaller
  `.mca` files.
- Cache fields accept `auto`. In auto mode the Rust CLI sizes heightmap rows and surface tile caches from the machine's
  physical memory and the detected companion rasters, while leaving memory headroom for the OS and chunk generation.

The same options are available from the CLI:

```powershell
cargo run --release --manifest-path rust/Cargo.toml -p earthmap-cli --bin earthmap-rs -- `
  generate-vanilla-delegated-regions-parallel $env:EARTHMAP_HEIGHTMAP $env:EARTHMAP_OUTPUT_ROOT 1000 `
  26 -10 3 3 linear 8 surface surfaceRaster=auto linearCompression=4

cargo run --release --manifest-path rust/Cargo.toml -p earthmap-cli --bin earthmap-rs -- `
  generate-vanilla-delegated-regions-parallel $env:EARTHMAP_HEIGHTMAP $env:EARTHMAP_OUTPUT_ROOT 1000 `
  26 -10 3 3 mca 8 surface surfaceRaster=auto mcaCompression=6
```

`compression=N` can be used instead of the format-specific key; it applies to the selected output format.

Cache tuning can be overridden with environment variables when needed:

```powershell
$env:EARTHMAP_HEIGHTMAP_CACHE_ROWS = "auto"
$env:EARTHMAP_SURFACE_TILE_CACHE_ENTRIES = "auto"
```

Use positive integers only for controlled benchmarking; `auto` is the recommended default for normal generation.

Fast filtered test:

```powershell
.\scripts\test.ps1 -Filter PhotoSurface
```

Isolated Rust test mode:

```powershell
.\scripts\test.ps1 -Isolated
```

## Quality Sample Batch

Prepare a CSV with:

```text
sample,regionX,regionZ,cropX,cropY,cropWidth,cropHeight,sourcePng,expectedStandardPng,landMaskPng
```

Run:

```powershell
cargo run --release --manifest-path rust/Cargo.toml -p earthmap-cli --bin earthmap-rs -- `
  quality-production-sample-batch samples.csv $env:EARTHMAP_HEIGHTMAP `
  (Join-Path $env:EARTHMAP_OUTPUT_ROOT "quality\photo-parity\vNEXT") 5000 mca 1 `
  cacheRows=512 prefetchRows=0 verticalScale=1.25 textureMode=photo surfaceRaster=auto chunkStatus=surface `
  metricMode=current-only previewDebug=off
```

The command writes per-sample worlds under `<outputRoot>/<sample>/world` and direct MCA parity evidence under
`<outputRoot>/<sample>/photo-parity`. It also writes `<outputRoot>/quality-production-sample-summary.csv` and
`<outputRoot>/quality-production-sample-contact-sheet.png`.

Use `metricMode=current-only` for the fast production loop. The default `metricMode=full` keeps candidate-harness
comparisons available, but it is intentionally slower and should not be used as the timed production proof path.
Use `previewDebug=auto` only for diagnostics; it writes same-run debug tiles and compares production `source-color`
against the reference source and expected Standard crop.

The PowerShell acceptance wrapper can call the batch command when an explicit job CSV exists:

```powershell
.\scripts\run-quality-acceptance-samples.ps1 -ProductionSamplesCsv .\samples.csv `
  -OutputRoot (Join-Path $env:EARTHMAP_OUTPUT_ROOT "quality\photo-parity\vNEXT")
```

In this batch mode the wrapper passes `metricMode=current-only` automatically, so it stays on the fast production proof
path instead of the slower candidate-harness research path. The wrapper also validates each sample's
`photo-parity/metric-land/metrics.txt` against the current targeted thresholds and exits nonzero when any sample fails.
Fresh current-only metric reports include raw current-vs-expected, source-vs-expected, current-vs-source, and 4x4
local-average diagnostic sections.
Add `-ProductionPreviewDebug auto` when investigating whether same-run production `source-color` differs from the
reference source.
Use `-NoQualityGate` only for research evidence. `-SkipGeneration` can proxy-check existing artifacts, but it is always
reported as NO-GO for release evidence.

## Recovery

- Generated junk inside the repo should be archived, not deleted first.
- Keep generated archives under `out/archive` or a path named by `EARTHMAP_OUTPUT_ROOT`.
- If a generation process is suspected to be stuck, inspect `earthmap-rs` command lines and recent progress logs before
  stopping anything.
