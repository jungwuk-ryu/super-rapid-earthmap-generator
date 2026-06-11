# Decisions

## 2026-06-12: Public Release Preparation

- The repository is MIT licensed.
- The active runtime is the Rust workspace under `rust/`; root scripts and README intentionally point there instead of
  moving the workspace to the repository root.
- Full generation is allowed when the current build/config passes `docs/QUALITY-GATES.md`.
- External raster datasets, Minecraft/server binaries, generated worlds, and GUI imagery derived from external datasets
  must not be committed.

## 2026-06-01: Project State Reset

- Historical reset note: full world generation was paused until the quality gates were refreshed.
- v123 is evidence-only and must not be treated as a pass.
- The first priority is faster validation, not more surface heuristics.
- Generated junk is archived under `<EARTHMAP_OUTPUT_ROOT>/archive` or `out/archive` before any deletion.

## 2026-06-01: Test Loop

- `scripts/test.ps1` delegates to the Rust workspace test wrapper.
- Isolated execution remains available as `-Isolated` for one-test-at-a-time Rust debugging.

## 2026-06-01: Quality Runtime

- `prefetchRows=0` is the quality-sample default.
- PowerShell remains the outer orchestration layer.
- Batch quality work should use Rust CLI commands that reuse raster contexts inside the process.

## 2026-06-01: Photo Solver Boundary

- `PhotoSurfaceSolver` is the shared decision contract.
- `PhotoSurfaceMaterialClassifier.apply` is now a compatibility wrapper over the solver decision.

## 2026-06-01: Natural Surface Contract

- Production worlds are survival/wild terrain, not satellite pixel art.
- Leaves must never be used as a terrain top/filler block. Leaf blocks are valid only as part of trees or canopy
  features placed above a natural ground surface.
- Concrete, terracotta color carriers, End/Quartz/Bone palette carriers, mud bricks, dripstone, and decorative
  sandstone variants are harness/render candidates only unless a separate natural-structure rule explicitly places
  them.
- Dark vegetation is represented by biome tint and actual tree/canopy density, not by replacing grass with leaf
  carpets or green terracotta.
- Coastlines must prefer sand, gravel, clay, mud, or stone-family blocks. Dark or brown photo pixels on shorelines
  must not become black/brown concrete or terracotta.
