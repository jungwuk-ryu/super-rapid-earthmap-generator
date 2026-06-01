# Architecture

## Pipeline

1. Heightmap and Earth rasters are opened through shared reader/cache contexts.
2. `EarthSurfaceRegionSampler` samples one region of surface columns.
3. `PhotoSurfaceSolver` returns a traceable `PhotoSurfaceDecision`.
4. `PhotoSurfaceMaterialClassifier.apply` adapts that decision back to `EarthSurfaceColumn`.
5. `SurfaceRegionGenerator` writes MCA/Linear region files.
6. `McaTopDownRenderer` renders direct MCA evidence for quality comparison.
7. `PhotoParityHarness` writes crop metrics, candidates, and error images.

## Solver Contract

`PhotoSurfaceInput` captures the existing classifier inputs. `PhotoSurfaceDecision` captures:

- top block and filler block
- biome
- expected rendered RGB
- recipe/source id
- stage id
- trace text

The wrapper preserves current behavior while making production and harness paths share a common decision object.

## Current Structural Boundaries

- `EarthMapCli` remains the public command entrypoint.
- Long-term command extraction is planned, but command names and file outputs must stay stable first.
- `scripts/test.ps1` uses a single JVM `TestSuiteRunner` by default.
- `quality-production-sample-batch` reuses one JVM and one heightmap/surface-raster context for multiple samples.
