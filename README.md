# Super-Rapid EarthMap Generator

Photo-first Earth surface generator for Minecraft 1.21.11 worlds.

Current status: **active prototype, production readiness NO-GO**. Do not start a full Earth or nation-war generation
until the gates in `docs/QUALITY-GATES.md` pass for the same candidate build/config.

## Direction

- Default production surface mode is `textureMode=photo` with `surfaceRaster=auto`.
- The target is MET-style satellite raster remapping into natural Minecraft terrain, not pixel-art block painting.
- Production ground must use natural terrain blocks only. Concrete, terracotta color carriers, and leaves-as-ground are
  forbidden even when they improve satellite color parity.
- Darker vegetation should be represented by biome choice and real tree/canopy placement. Leaf blocks belong to trees,
  not the terrain surface.
- Vanilla/server delegation remains responsible for caves, ores, trees, and later features after the written chunk
  status.
- Metrics are support evidence. Contact sheets and direct MCA/Dynmap visual review decide promotion.

## Build And Test

```powershell
.\scripts\build.ps1
.\scripts\test.ps1 -List
.\scripts\test.ps1 -Filter PhotoSurface
.\scripts\test.ps1
```

`scripts/test.ps1` delegates to the Rust workspace test wrapper. Use `-Isolated` when debugging a Rust test that must
run one case at a time.

## Fast Quality Loop

Use batch commands before any full sample run:

```powershell
cargo run --release --manifest-path rust/Cargo.toml -p earthmap-cli --bin earthmap-rs -- `
  photo-parity-metric-batch <jobsCsv> <outputRoot> auto
cargo run --release --manifest-path rust/Cargo.toml -p earthmap-cli --bin earthmap-rs -- `
  quality-production-sample-batch <samplesCsv> <heightmap> <outputRoot> 5000 mca 1 `
  cacheRows=512 prefetchRows=0 verticalScale=1.25 textureMode=photo surfaceRaster=auto chunkStatus=surface `
  metricMode=current-only previewDebug=off
```

`prefetchRows=0` is the default for quality samples. Do not use `prefetchRows=256` unless a same-window benchmark
proves it helps. `metricMode=current-only` is the fast production-sample proof path; the default `metricMode=full`
keeps the slower candidate-harness comparisons available for research runs.
Use `previewDebug=auto` only for diagnostics that need same-run production `source-color` evidence.

## Important Docs

- `docs/README.md`
- `docs/ARCHITECTURE.md`
- `docs/OPERATIONS.md`
- `docs/QUALITY-GATES.md`
- `docs/DECISIONS.md`
- `docs/EXPERIMENTS.md`
- `docs/GENERATED-ARTIFACTS.md`
