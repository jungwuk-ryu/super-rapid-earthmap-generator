# Architecture

## Pipeline

1. Heightmap and Earth rasters are opened through shared reader/cache contexts.
2. `prepare_surface_region_sample_with_heightmap_sampler` prepares region-local surface evidence.
3. `solve_photo_surface` returns a traceable `PhotoSurfaceDecision` for photo-mode material choices.
4. `apply_photo_surface_material` adapts that decision back to `EarthSurfaceColumn`.
5. `generate_surface_region_with_prepared_sample` writes MCA/Linear region files through `earthmap-region`.
6. Top-down render and quality commands in `earthmap-cli`/`earthmap-quality` render direct evidence for comparison.
7. Quality batch commands write crop metrics, candidate artifacts, summaries, and contact sheets.

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
- Command extraction has started inside `earthmap-cli`; command names, stdout/stderr contracts, progress events, and
  file outputs must stay stable.
- `scripts/test.ps1` delegates to the Rust workspace test wrapper; CI also runs fmt, clippy, repository lint, and tests.
- `quality-production-sample-batch` reuses Rust heightmap and surface-raster contexts for multiple samples.
