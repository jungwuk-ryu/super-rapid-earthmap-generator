# Super-Rapid EarthMap Generator

Photo-first Earth surface generator for Minecraft Java Edition 1.21.11 worlds.

Super-Rapid EarthMap Generator turns real geospatial elevation and surface rasters into Minecraft region files. The
project writes MCA and DivineMC Linear output, supports resumable large-area generation, and keeps visual quality tied to
direct region renders and quality gates rather than loose screenshots.

## Current Architecture

The active implementation is a Rust workspace under [`rust/`](rust/). The repository root intentionally stays small:

- `rust/`: Rust workspace and all production code.
- `scripts/`: root PowerShell wrappers that delegate into `rust/scripts/`.
- `docs/`: operator guides, architecture notes, quality gates, and data policy.
- `.github/`: CI and public collaboration templates.

The old Java implementation is no longer the runtime. Some names still mention Java where they describe Minecraft Java
Edition, Java-compatible file formats, or historical oracle/parity behavior.

## What It Builds

- `earthmap-rs`: CLI for generation, validation, rendering, quality evidence, OSM overlays, and conversion tools.
- `earthmap-gui`: desktop GUI that launches `earthmap-rs`, tracks progress, supports resume-aware projects, and renders
  a region-status world map without bundling third-party satellite imagery.
- Rust crates for geospatial raster reading, surface material decisions, Minecraft NBT/region writing, gameplay checks,
  OSM feature masks, quality metrics, and parity utilities.

## Generation Model

- Default production surface mode is `textureMode=photo` with `surfaceRaster=auto`.
- Height comes from a user-provided HeightMap GeoTIFF.
- Photo-like terrain uses a user-provided external surface raster such as `terrain/TrueMarble.vrt`.
- Natural terrain policy is strict: production ground should use natural Minecraft terrain blocks; decorative color
  carriers and leaves-as-ground are excluded from generated surface/filler columns.
- Vanilla/server delegation remains responsible for caves, ores, trees, and later features after the written chunk
  status.

External rasters and generated worlds are not part of this repository. See [`docs/EXTERNAL-DATA.md`](docs/EXTERNAL-DATA.md)
and [`docs/GENERATED-ARTIFACTS.md`](docs/GENERATED-ARTIFACTS.md).

## Quick Start

Prerequisites:

- Rust toolchain with `cargo` on `PATH`
- PowerShell for repository wrapper scripts
- Local HeightMap GeoTIFF and `TifFiles`/surface-raster data outside this repository

```powershell
$env:EARTHMAP_HEIGHTMAP = "<absolute-path-to-heightmap.tif>"
$env:EARTHMAP_DATA_ROOT = "<absolute-path-to-earthmap-data>"
$env:EARTHMAP_TIF_ROOT = Join-Path $env:EARTHMAP_DATA_ROOT "TifFiles"
$env:EARTHMAP_SURFACE_RASTER = Join-Path $env:EARTHMAP_TIF_ROOT "terrain\TrueMarble.vrt"
$env:EARTHMAP_OUTPUT_ROOT = "<absolute-path-to-output-directory>"
```

Build, test, and inspect the CLI:

```powershell
.\scripts\build.ps1
.\scripts\test.ps1 -List
.\scripts\run.ps1 --version
.\scripts\run.ps1 --help
```

Small generation example:

```powershell
.\scripts\run.ps1 generate-vanilla-delegated-regions-parallel `
  $env:EARTHMAP_HEIGHTMAP (Join-Path $env:EARTHMAP_OUTPUT_ROOT "sample-world") 1000 `
  26 -10 3 3 linear 8 surface surfaceRaster=auto verticalScale=auto linearCompression=4
```

GUI:

```powershell
cargo run --manifest-path rust/Cargo.toml -p earthmap-gui --bin earthmap-gui
```

## Quality Gates

Full generation is allowed when the build/config being used has passed the current quality gates in
[`docs/QUALITY-GATES.md`](docs/QUALITY-GATES.md). Metrics are support evidence; contact sheets, direct MCA/Linear renders,
and server/Dynmap review decide promotion.

## Development

```powershell
cargo fmt --all --manifest-path rust\Cargo.toml -- --check
cargo clippy --manifest-path rust\Cargo.toml --workspace --all-targets -- -D warnings
.\scripts\lint.ps1
.\scripts\test.ps1
```

## License

MIT. See [`LICENSE`](LICENSE).

The MIT license covers this repository's source code and documentation. It does not grant rights to external datasets,
Minecraft server jars, proprietary rasters, or third-party trademarks.

## Important Docs

- [`docs/README.md`](docs/README.md)
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- [`docs/OPERATIONS.md`](docs/OPERATIONS.md)
- [`docs/QUALITY-GATES.md`](docs/QUALITY-GATES.md)
- [`docs/EXTERNAL-DATA.md`](docs/EXTERNAL-DATA.md)
- [`CONTRIBUTING.md`](CONTRIBUTING.md)
- [`SECURITY.md`](SECURITY.md)
