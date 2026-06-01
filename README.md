# Super-Rapid EarthMap Generator

Photo-first Earth surface generator for Minecraft Java 1.21.11.

Current status: **active prototype, production readiness NO-GO**. Do not start a full Earth or nation-war generation
until the gates in `docs/QUALITY-GATES.md` pass for the same candidate build/config.

## Direction

- Default production surface mode is `textureMode=photo` with `surfaceRaster=auto`.
- The target is MET-style satellite raster remapping into natural Minecraft blocks, not an ecology/biome atlas.
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

`scripts/test.ps1` defaults to a single JVM test runner. Use `-Isolated` only when debugging a test that must run in
its own Java process.

## Fast Quality Loop

Use batch commands before any full sample run:

```powershell
.\scripts\run.ps1 photo-parity-metric-batch <jobsCsv> <outputRoot> auto
.\scripts\run.ps1 quality-production-sample-batch <samplesCsv> <heightmap> <outputRoot> 5000 mca 1 `
  cacheRows=512 prefetchRows=0 verticalScale=1.25 textureMode=photo surfaceRaster=auto chunkStatus=surface
```

`prefetchRows=0` is the default for quality samples. Do not use `prefetchRows=256` unless a same-window benchmark
proves it helps.

## Important Docs

- `docs/ARCHITECTURE.md`
- `docs/OPERATIONS.md`
- `docs/QUALITY-GATES.md`
- `docs/DECISIONS.md`
- `docs/EXPERIMENTS.md`
- `docs/GENERATED-ARTIFACTS.md`
