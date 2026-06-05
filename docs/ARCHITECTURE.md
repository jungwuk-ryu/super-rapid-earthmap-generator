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

## Production Surface Boundary

The photo solver may evaluate render/harness carriers for metric research, but production chunk writers must pass final
top and filler blocks through `NaturalSurfaceBlockPolicy` before emitting MCA or Linear chunks. This boundary forbids
leaves-as-ground, concrete, terracotta color carriers, and decorative palette carriers on terrain surfaces. Vegetation
darkening is handled by biome tint and actual tree/canopy generation above natural ground, never by replacing terrain
with leaf blocks.

## Current Structural Boundaries

- `earthmap-rs` is the public command entrypoint.
- Long-term command extraction is planned, but command names and file outputs must stay stable first.
- `scripts/test.ps1` delegates to the Rust workspace test wrapper.
- `quality-production-sample-batch` reuses Rust heightmap and surface-raster contexts for multiple samples.
