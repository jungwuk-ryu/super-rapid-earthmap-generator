# Operations

## Prerequisites

- JDK 25 on `PATH`
- PowerShell 7 or Windows PowerShell capable of running the scripts
- `C:\earth_map_resources\HQheightmap.tif`
- `D:\earthmap\TifFiles\terrain\TrueMarble.vrt` or another explicit `surfaceRaster` path
- ImageMagick only for workflows that produce Standard-remap reference images outside Java

## Normal Commands

```powershell
.\scripts\build.ps1
.\scripts\test.ps1
.\scripts\run.ps1 --help
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

The same options are available from the CLI:

```powershell
cargo run --release --manifest-path rust/Cargo.toml -p earthmap-cli --bin earthmap-rs -- `
  generate-vanilla-delegated-regions-parallel C:\earth_map_resources\HQheightmap.tif D:\worldgen 1000 `
  26 -10 3 3 linear 8 surface surfaceRaster=D:\earthmap\TifFiles\terrain\TrueMarble.vrt linearCompression=4

cargo run --release --manifest-path rust/Cargo.toml -p earthmap-cli --bin earthmap-rs -- `
  generate-vanilla-delegated-regions-parallel C:\earth_map_resources\HQheightmap.tif D:\worldgen 1000 `
  26 -10 3 3 mca 8 surface surfaceRaster=D:\earthmap\TifFiles\terrain\TrueMarble.vrt mcaCompression=6
```

`compression=N` can be used instead of the format-specific key; it applies to the selected output format.

Fast filtered test:

```powershell
.\scripts\test.ps1 -Filter PhotoSurface
```

Isolated legacy test mode:

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
.\scripts\run.ps1 quality-production-sample-batch samples.csv C:\earth_map_resources\HQheightmap.tif `
  D:\earthmap\quality\photo-parity\vNEXT 5000 mca 1 `
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
.\scripts\run-quality-acceptance-samples.ps1 -ProductionSamplesCsv .\samples.csv -OutputRoot D:\earthmap\quality\photo-parity\vNEXT
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
- The current reset archive is `D:\earthmap\archive\super-rapid-reset-20260601`.
- If a generation process is suspected to be stuck, inspect Java command lines before stopping anything.
