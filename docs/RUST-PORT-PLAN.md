# Rust Port Execution Plan

Date: 2026-06-03
Last status audit: 2026-06-04, audited through the JavaStandard vegetation palette solver rerun, default HeightMap path
switch, deterministic Rayon verification-time parallelism, and chunk `(12,0)` component-stage diagnostics

Goal: Port Super-Rapid EarthMap Generator from Java to Rust while keeping Java as the correctness oracle until the Rust
implementation proves identical output. The Rust implementation must target maximum throughput, but output parity is a
hard gate before any optimization is accepted.

Current project state: active prototype, production readiness NO-GO. A Rust build does not change the release gate:
full Earth or nation-war generation remains blocked until `docs/QUALITY-GATES.md` passes for the same candidate
build/config.

## Resume Checkpoint

Read this checkpoint before starting any new Rust-port work.

- Latest accepted output/evidence baseline before the current chunk `(12,0)` component/postprocess blocker:
  `63404a2 feat(rust): accelerate phase5 verification path`.
- Latest evidence folder:
  `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-default-heightmap-parallel`.
- Previous evidence folder retained for comparison:
  `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-biome-palette-solver`.
- Standard local HeightMap path for Rust CLI examples, smoke probes, and parity reruns:
  `C:\earth_map_resources\HQheightmap.tif`.
- Active gate: Phase 5 vanilla-delegated photo/material payload mismatch diagnosis after the bathymetry-specific shelf
  fix, the 4x4 biome-cell input trace, the JavaStandard vegetation palette solver patch, and the deterministic
  verification-time Rayon fan-out.
- Resume from: the first `Active` item in `Current active work`, currently chunk `(12,0)`, the first remaining payload
  mismatch after the `(8,0)` biome-cell blocker was fixed. Do not resume from the older `(8,0)` blocker unless a fresh
  comparison moves the first mismatch back there.
- Do not resume from: Phase 0 bootstrap, Phase 1 CLI shell, Phase 2 binary core, Phase 3 region writers, Phase 4
  height-only raster generation, or completed Phase 5 bootstrap slices.
- If local commits or evidence contradict this checkpoint, update this checkpoint and the status board first in the same
  conventional atomic commit as the work that changed the status.

## Status Tracking Rules

- This board is the current-status source of truth. The phase sections below are detailed history, not a restart queue.
- Before resuming work, read this board and recent commits first; do not infer that lower phase numbers are unstarted.
- A phase marked `Done`, `Done for current bootstrap scope`, or `Implemented through tracked bootstrap/parity slices`
  must not be restarted unless this board explicitly changes it back to active work.
- Every future phase/status change must update this board in the same conventional atomic commit as the work that
  changed the status.
- The current active pointer is Phase 5 photo/material payload mismatch diagnosis. The PHOTO two-pass/TokenLumaProfile
  implementation gap is addressed, the bathymetry-specific shelf mismatch that affected chunk `(8,0)` block and
  `OCEAN_FLOOR` output is fixed, and the JavaStandard vegetation palette path now produces the Java-matching
  `minecraft:windswept_savanna` input for the previously blocking chunk `(8,0)`, cell `(3,0)`. The active target is
  now the next first mismatch, chunk `(12,0)`.
  Phase 0 is active only as an oracle/corpus harness, not as Rust implementation bootstrap work.

## Current Status Board

Read this board before choosing the next task. The phase sections below are detailed history and scope; this board is
the current source of truth for work status.

| Track | Status | Evidence anchor | Next action |
| --- | --- | --- | --- |
| Phase 0: oracle and corpus harness | Active, not a restart | `4a55b98`, `59a8277`, `6fb2e76`, `8df39c2` added golden/candidate corpus tooling and entries. | Expand the corpus only after the active Phase 5 gate is green. Do not restart Rust implementation from Phase 0. |
| Phase 1: Rust skeleton and CLI shell | Done | Cargo workspace, wrappers, command shell, and capability reporting are already in the Rust tree. | No restart. Only add missing compatibility commands when a later phase needs them. |
| Phase 2: Minecraft binary core | Done for current bootstrap scope | NBT, chunk model, level.dat, heightmaps, section palettes, and gzip/delta fixture evidence are implemented. | No active work for fixed NBT/level.dat bootstrap scope. |
| Phase 3: region writers | Done for current bootstrap scope | MCA and Linear V2 writer/reader parity fixtures are implemented. | No active work for bootstrap writer parity. |
| Phase 4: GeoTIFF/VRT and height-only regions | Done for current bootstrap scope | Real height-only MCA/Linear parity evidence exists; `generate-golden.ps1 -IncludeHeightOnly` promotes the fixed region. Current Rust CLI probes use the standard local HeightMap path `C:\earth_map_resources\HQheightmap.tif`; older `E:\HQheightmap.tif` references are historical evidence paths. | No active work for the current height-only raster scope. |
| Phase 5: surface rules, photo solver, OSM, natural surfaces | Implemented through tracked bootstrap/parity slices; active next mismatch remains | Output-parity evidence through `63404a2` includes surface/photo/ecoregion/no-climate/snow evidence, PHOTO TokenLumaProfile work, chunk detail diagnostics, the bathymetry shelf parity fix, the 4x4 biome-cell trace, the JavaStandard vegetation palette solver fix for chunk `(8,0)`, the standard CLI HeightMap default, and deterministic verification-time Rayon fan-out. The latest accepted single-region comparison is still red: `matchingChunks=531`, `mismatchedChunks=493`, `missingChunks=0`, `extraChunks=0`, `firstMismatch=12,0`. | Diagnose chunk `(12,0)` component/postprocess parity before promoting photo/material corpus entries. |
| Vanilla-owned gameplay generation | Out of scope | Java and Rust plans delegate ores, caves, vegetation, structures, strongholds, End portals, loot, and spawners to vanilla. | Do not add direct Rust generators for vanilla-owned gameplay features. |
| Phase 6: quality and visual evidence tools | Pending | Not started because Phase 5 photo/material corpus parity is not green yet. | Start only after corpus-backed Phase 5 output parity is green for the relevant fixed regions. |
| Phase 7: performance optimization | Pending, explicitly tracked but blocked by parity | The performance plan exists in this document. A narrow deterministic Rayon change was made early only to reduce verification time; it preserved the previous Rust region SHA-256/payload manifest exactly and does not promote Phase 7. | Start full optimization only after Java/Rust corpus parity and Phase 6 quality/visual evidence are green for the same build/config. |

Current active work:

1. Done: `rust/scripts/generate-candidates.ps1` can regenerate supported Rust candidate entries from a Java golden root.
2. Done: `rust/scripts/generate-golden.ps1 -IncludeHeightOnly` adds fixed-region height-only MCA/Linear entries.
3. Done: `rust/scripts/generate-golden.ps1 -IncludeSurface` adds no-material fixed-region surface MCA/Linear entries.
4. Done: `summarize-region-chunk` can compare Java/Rust region chunk root fields, heightmap edges, and section
   block/biome palette summaries for mismatch triage.
5. Done: Rust PHOTO-mode region sampling now follows Java's semantic-then-photo region pass shape, collects a
   region-wide `TokenLumaProfile` from non-water JavaStandard token samples, and passes that profile into deferred
   photo material solving.
6. Done: reran the vanilla-delegated photo/material payload comparison after the TokenLumaProfile fix. Evidence folder:
   `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-tokenluma`.
7. Done: `compare-region-chunk-details` decodes Java/Rust chunk heightmap, biome-cell, and block-cell values for
   first-mismatch triage, including asymmetric section-set differences. Sub-agent closure review found no remaining
   actionable issue.
8. Done: `trace-surface-region-column` can run the full PHOTO region path and print Java-style column diagnostics for
   selected region-local columns. The first probed blocker column `(131,0)` showed Rust was applying the ordinary
   coastal shelf adjustment to bathymetry, while Java uses the stricter bathymetry-specific shelf function.
9. Done: Rust bathymetry material classification now mirrors Java's `coastalBathymetryShelfAdjustedDepthBlocks` path.
   The rerun evidence folder is
   `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-bathymetry-shelf`.
10. Done: the bathymetry shelf rerun improved the single-region comparison from `matchingChunks=491`,
    `mismatchedChunks=533` to `matchingChunks=527`, `mismatchedChunks=497`. For chunk `(8,0)`,
    `heightmap.OCEAN_FLOOR.diffCount=0` and `block.diffCount=0`.
11. Done: `trace-surface-region-cell` can run the full PHOTO region path for a chunk-local biome cell and print the
    16 contributing column traces plus final biome counts.
12. Done: the 4x4 trace for chunk `(8,0)`, cell `(3,0)` is recorded at
    `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-bathymetry-shelf\diagnostics\rust-trace-cell-chunk8-0-cell3-0.txt`.
    It lists Rust final input counts as `deep_lukewarm_ocean=8`, `warm_ocean=4`, and `savanna=4`, with no
    `windswept_savanna` count entry.
13. Done: the JavaStandard vegetation palette solver now matches the Java oracle for the previous blocker seed column
    `(146,2)`: Rust writes `photo.topBlockStateId=4`, `photo.biomeId=minecraft:windswept_savanna`, and
    `photo.decisionSource=photo-palette`.
14. Done: the rerun 4x4 trace for chunk `(8,0)`, cell `(3,0)` now matches Java final biome counts:
    `deep_lukewarm_ocean=8`, `warm_ocean=4`, `savanna=1`, and `windswept_savanna=3`.
15. Done: reran the single-region comparison after the JavaStandard vegetation palette solver patch. Evidence folder:
    `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-biome-palette-solver`; result:
    `matchingChunks=531`, `mismatchedChunks=493`, `missingChunks=0`, `extraChunks=0`, `firstMismatch=12,0`.
16. Done: Rust CLI diagnostic/generation commands now accept omitted heightmap arguments for supported forms and resolve
    them to the standard local HeightMap path `C:\earth_map_resources\HQheightmap.tif`.
17. Done: deterministic Rayon parallelism now covers surface column material/photo sampling plus chunk build/NBT
    encoding for verification-time runs. Release evidence folder:
    `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-default-heightmap-parallel`; the
    generated region SHA-256 and payload manifest are identical to the prior Rust candidate. The release evidence run
    reported `phase.totalInternalMillis=100890` versus the prior candidate's `phase.totalInternalMillis=359668`; treat
    this as verification-loop evidence, not an isolated benchmark.
18. Done: chunk `(12,0)` detail comparison for the accepted baseline shows this is no longer a heightmap issue:
    `heightmap.OCEAN_FLOOR.diffCount=0`, `biomeCell.diffCount=44`, and `block.diffCount=6`. The block deltas are
    moss/grass surface-policy fallout from biome differences.
19. Done: `trace-surface-region-column` and `trace-surface-region-cell` now expose post-cell, first component,
    smoother, second component, and final column states plus component size/neighbor/action diagnostics.
20. Done: targeted chunk `(12,0)` traces show Rust over-preserves a large forest/jungle/savanna postprocess shape:
    selected components report neighbor majority `minecraft:dark_forest` but exceed `SMALL_BIOME_COMPONENT_MAX=64`,
    so the Java-expected dark-forest absorption is not applied.
21. Done but not adopted: a narrow experiment that forced the dark JavaStandard shadow fast path to keep semantic
    non-grass biome values matched a single Java solver probe, but worsened region-level parity. The experiment folder
    `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-dark-shadow-fix` compared against
    Java with `biomeCell.diffCount=376` for chunk `(12,0)` and compared against the accepted Rust baseline with
    `biomeCell.diffCount=332`, so that functional change must not be used.
22. Active blocker: chunk `(12,0)` is still the first remaining accepted-baseline payload mismatch.
23. Active next: compare Java and Rust pre/post component region inputs around chunk `(12,0)` and identify why Java's
    forest/dark-forest component remains small enough or differently seeded while Rust's corresponding component is
    too large to replace.
24. Next: only after photo/material corpus parity is green, expand Phase 6 quality and visual evidence tooling.

## Phase 5 Remaining Checklist

Phase 5 is not a restart. Most implementation slices are already ported and tested through bootstrap/parity fixtures.
The remaining work is the active full-region photo/material parity blocker and the corpus promotion that depends on it.

- Done: surface rules, material samples, TrueMarble/EarthData samplers, ecoregion/cache sources, MET image exports,
  LandShallowTopo photo sampling, natural surface cleanup, OSM overlays, and no-material surface-region generation have
  Rust bootstrap coverage.
- Done: PHOTO region processing now uses Java-shaped semantic-then-photo passes and a region-wide
  `TokenLumaProfile`.
- [x] Add a probe-only Rust `generate-vanilla-delegated-region` path that can generate the single Linear V2 evidence
  region needed for mismatch diagnosis.
- [x] Rerun the 2026-06-04 TokenLumaProfile comparison and record the still-red result:
  `matchingChunks=491`, `mismatchedChunks=533`, `firstMismatch=8,0`.
- [x] Add local detailed chunk comparison output for chunk `(8,0)`: `heightmap.OCEAN_FLOOR.diffCount=121`,
  `biomeCell.diffCount=39`, and `block.diffCount=3101`.
- [x] Review the detailed chunk comparison diagnostic with sub-agents and close the asymmetric-section false-negative
  finding before atomic commit.
- [x] Root-cause the column/material path that made Java's chunk `(8,0)` lower/deeper in many ocean/coast columns while
  Rust wrote shallower/higher block columns: Java uses a bathymetry-specific shelf threshold for material bathymetry,
  separate from the ordinary coastal shelf adjustment.
- [x] Patch the smallest Rust bathymetry path that explains the chunk `(8,0)` height/block delta without weakening
  existing fixture parity.
- [x] Rerun the same Java/Rust single-region comparison after the bathymetry shelf patch and record the improved but
  still-red result: `matchingChunks=527`, `mismatchedChunks=497`, `missingChunks=0`, `extraChunks=0`,
  `firstMismatch=8,0`.
- [x] Confirm that the first mismatch is now no longer a column height/block mismatch:
  `heightmap.OCEAN_FLOOR.diffCount=0`, `block.diffCount=0`, and `biomeCell.diffCount=29` for chunk `(8,0)`.
- [x] Add `trace-surface-region-cell` so the 16 contributing columns for one biome cell can be inspected through the
  same full PHOTO region path.
- [x] Run the 4x4 trace for chunk `(8,0)`, cell `(3,0)`, and record the narrowed diagnosis:
  Rust final input counts are `deep_lukewarm_ocean=8`, `warm_ocean=4`, and `savanna=4`, with no
  `windswept_savanna` count entry.
- [x] Root-cause the previous photo biome-classification difference:
  expected `minecraft:windswept_savanna`, actual `minecraft:savanna` for chunk `(8,0)`, biome cell `cellX=3`,
  `cellZ=0`. The mismatch was in the JavaStandard vegetation palette/source-render path feeding the photo solver.
- [x] Patch the smallest Rust photo biome-classification path that explains the previous
  chunk `(8,0)` delta without weakening existing fixture parity.
- [x] Rerun the same Java/Rust single-region comparison after the JavaStandard vegetation palette solver patch and
  record the improved but still-red result: `matchingChunks=531`, `mismatchedChunks=493`, `missingChunks=0`,
  `extraChunks=0`, `firstMismatch=12,0`.
- [x] Confirm the deterministic Rayon verification-time parallelism does not change Rust output: the new
  `vanilla-delegated-linear-probe-20260604-rustcli-default-heightmap-parallel` region SHA-256 and payload manifest are
  identical to the previous Rust candidate.
- [x] Run chunk `(12,0)` detail comparison for the accepted baseline and record the current mismatch shape:
  `heightmap.OCEAN_FLOOR.diffCount=0`, `biomeCell.diffCount=44`, and `block.diffCount=6`.
- [x] Extend Rust trace diagnostics so selected columns show `postCell`, `postStabilized`,
  `postFirstComponentTrace`, `postSmoothed`, `postComponent`, `postComponentTrace`, and `final` stages.
- [x] Probe chunk `(12,0)`, cells `(2,0)` and `(3,0)`, and record that Rust component traces see
  `neighborMajorityBiome=minecraft:dark_forest` but skip replacement when the component is larger than
  `SMALL_BIOME_COMPONENT_MAX=64`.
- [x] Test the tempting dark JavaStandard shadow semantic-biome adjustment and reject it: it matched a single Java
  direct-solver probe but worsened chunk `(12,0)` region parity to `biomeCell.diffCount=376` versus Java and
  introduced `biomeCell.diffCount=332` versus the accepted Rust baseline.
- [ ] Root-cause why Java and Rust build different pre/post component shapes around chunk `(12,0)`, especially the
  forest/dark-forest/savanna boundary that controls whether `dark_forest` absorbs the local component.
- [ ] Patch only the smallest confirmed Rust input or postprocess parity gap for chunk `(12,0)`; do not change
  component size thresholds or dark-shadow biome behavior based on the rejected experiment alone.
- [ ] Rerun the same Java/Rust single-region comparison; Phase 5 promotion requires `matchingChunks=1024`,
  `mismatchedChunks=0`, `missingChunks=0`, and `extraChunks=0`.
- [ ] After the fixed-region photo/material comparison is green, add photo/material entries to the regeneratable
  golden/candidate corpus.
- [ ] Rerun `cargo test --workspace`, relevant ignored slow fixtures when touched, and the corpus scripts before marking
  Phase 5 corpus parity green.

Phase 5 remaining estimate: the known first-mismatch blocker moved from chunk `(8,0)` to chunk `(12,0)` after the
JavaStandard vegetation palette solver fix. Treat the remaining work as a new focused mismatch packet: diagnose/fix
chunk `(12,0)`, rerun/verify the fixed region, then promote the photo/material corpus entries. This is not a Phase 5
restart, but it still blocks Phase 6 and full Phase 7 because the full-region output payload is red.

## Phase 5 To Phase 7 Remaining Work

This is the forward task list. Do not restart earlier completed phases.

Phase 5 remaining:

- [ ] Compare Java and Rust component-stage inputs around chunk `(12,0)` at region scale, not only single-column
  direct solver probes.
- [ ] Explain why Java's dark-forest replacement applies in the same local area while Rust reports component sizes
  larger than `SMALL_BIOME_COMPONENT_MAX=64`.
- [ ] Patch the smallest confirmed parity gap in Rust photo/material input shaping or biome postprocessing.
- [ ] Regenerate the single Java/Rust Linear V2 region and require `matchingChunks=1024`, `mismatchedChunks=0`,
  `missingChunks=0`, and `extraChunks=0`.
- [ ] Promote fixed photo/material entries into the regeneratable golden/candidate corpus.
- [ ] Run focused Rust tests, relevant Java oracle commands, `cargo test --workspace`, and corpus scripts before
  marking Phase 5 green.

Phase 6 remaining:

- [ ] Start only after Phase 5 corpus-backed photo/material parity is green.
- [ ] Add or port visual evidence commands for the fixed photo/material regions.
- [ ] Add acceptance reports that connect generated chunks, preview/topdown outputs, and documented quality gates.
- [ ] Keep vanilla-owned gameplay generation delegated; do not add ore, cave, structure, stronghold, End portal, loot,
  or spawner generation to Rust.

Phase 7 remaining:

- [ ] Start full optimization only after Phase 5 and Phase 6 are green for the same build/config.
- [ ] Benchmark the accepted parity build as the baseline before further optimization.
- [ ] Expand deterministic parallelism beyond the current verification-time Rayon fan-out only when byte-identical
  payload parity is preserved.
- [ ] Profile heightmap row-cache behavior, surface material sampling, chunk build, NBT encode, and region write costs.
- [ ] Add repeatable benchmark scripts and document CPU/thread settings, input paths, and output SHA-256/payload
  manifest evidence.
- [ ] Reject optimizations that change Java/Rust output parity, even if they improve runtime.

## Non-Negotiable Rules

- Java output is the oracle until Rust parity is proven.
- Same inputs, settings, seed, and command must produce the same chunk NBT payloads.
- MCA and Linear outputs must be byte-identical whenever the compression stream can be matched.
- If a compression library difference prevents byte-identical region files, the Rust path must still prove identical
  uncompressed chunk payloads and must document the compression delta before promotion.
- No optimization may land unless all currently green parity fixtures stay green. If a narrow verification-time
  optimization is accepted before the active red payload gate is fixed, it must prove byte-identical output against the
  previous Rust candidate and must not promote Phase 7.
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
- `rust/scripts/compare-golden.ps1` also accepts `-CandidateRoot` so a Rust-generated candidate corpus tree can be
  compared against the Java-oracle entry manifests without rewriting the oracle artifacts. Omitting `-CandidateRoot`
  preserves the bootstrap self-check mode.
- `rust/scripts/generate-candidates.ps1` reads an existing Java-oracle golden root and regenerates the supported entries
  with the Rust CLI into a candidate root, including candidate payload and SHA-256 manifests for diagnostics.
- `rust/scripts/generate-golden.ps1 -IncludeHeightOnly` adds default `C:\earth_map_resources\HQheightmap.tif`
  height-only MCA/Linear fixed region entries to the Java-oracle corpus; `generate-candidates.ps1` reads those settings
  and regenerates matching Rust candidate entries.
- `rust/scripts/generate-golden.ps1 -IncludeSurface` adds default `C:\earth_map_resources\HQheightmap.tif` no-material
  surface MCA/Linear fixed-region entries to the Java-oracle corpus; `generate-candidates.ps1` reads those settings and
  regenerates matching Rust candidate entries.
- Verified locally with
  `rust/scripts/generate-golden.ps1 -OutputRoot D:\earthmap\rust-port-golden\height-corpus-smoke-20260604-094746\golden -IncludeHeightOnly`,
  `rust/scripts/generate-candidates.ps1 -GoldenRoot D:\earthmap\rust-port-golden\height-corpus-smoke-20260604-094746\golden -OutputRoot D:\earthmap\rust-port-golden\height-corpus-smoke-20260604-094746\candidate`, and
  `rust/scripts/compare-golden.ps1 -GoldenRoot D:\earthmap\rust-port-golden\height-corpus-smoke-20260604-094746\golden -CandidateRoot D:\earthmap\rust-port-golden\height-corpus-smoke-20260604-094746\candidate`.
  The default height-only fixed-region candidate files are byte-identical to the Java oracle outputs:

```text
entry,javaRegionSha256,rustRegionSha256,byteIdentical
height-only-r0-r0-linear,4764167E5820E9A2E5BD834053C22393563FD9FC4D2D896DC06BEE1AE77B9EC0,4764167E5820E9A2E5BD834053C22393563FD9FC4D2D896DC06BEE1AE77B9EC0,true
height-only-r0-r0-mca,083BC7F1038FA5C5776C85860EA46CB35EBED6936403974CCA1A2477A0C3C036,083BC7F1038FA5C5776C85860EA46CB35EBED6936403974CCA1A2477A0C3C036,true
```
- Verified locally with
  `rust/scripts/generate-golden.ps1 -OutputRoot D:\earthmap\rust-port-golden\surface-corpus-smoke-20260604-100410\golden -IncludeSurface`,
  `rust/scripts/generate-candidates.ps1 -GoldenRoot D:\earthmap\rust-port-golden\surface-corpus-smoke-20260604-100410\golden -OutputRoot D:\earthmap\rust-port-golden\surface-corpus-smoke-20260604-100410\candidate`, and
  `rust/scripts/compare-golden.ps1 -GoldenRoot D:\earthmap\rust-port-golden\surface-corpus-smoke-20260604-100410\golden -CandidateRoot D:\earthmap\rust-port-golden\surface-corpus-smoke-20260604-100410\candidate`.
  The default no-material surface fixed-region candidate files are byte-identical to the Java oracle outputs:

```text
entry,javaRegionSha256,rustRegionSha256,byteIdentical
surface-r0-r0-linear,6AF4D6A9303E0ACFC3E9F33F1C79856B4E40166D79B12A0067C4E79E25665F45,6AF4D6A9303E0ACFC3E9F33F1C79856B4E40166D79B12A0067C4E79E25665F45,true
surface-r0-r0-mca,4D2D6BE30B7DE7EE565FB581DCCBCD4D6FB0FDD3A137EB3F10B8436E8A2DD13E,4D2D6BE30B7DE7EE565FB581DCCBCD4D6FB0FDD3A137EB3F10B8436E8A2DD13E,true
```
- The Rust CLI implements `generate-flat-test-world <worldDir> <mca|linear>` so `flat-mca` and `flat-linear` Java
  oracle entries can now be compared against Rust-generated candidate world trees.
- Verified locally with
  `rust/scripts/compare-golden.ps1 -GoldenRoot D:\earthmap\rust-port-golden\flat-candidate-smoke-20260604-090856\golden -CandidateRoot D:\earthmap\rust-port-golden\flat-candidate-smoke-20260604-090856\candidate`.
  The Rust-generated flat MCA and Linear candidate region files are byte-identical to the Java oracle outputs:

```text
entry,javaRegionSha256,rustRegionSha256,byteIdentical
flat-mca,33E4BE37E86E0524AEC290A6DBED9C95FCC80F2B6B42B5221D53DD2DD6A5E5A6,33E4BE37E86E0524AEC290A6DBED9C95FCC80F2B6B42B5221D53DD2DD6A5E5A6,true
flat-linear,B80C7CFE14288EEE0255DE384536811A89622B6448804FF900CF86DAD80AE5AB,B80C7CFE14288EEE0255DE384536811A89622B6448804FF900CF86DAD80AE5AB,true
```
- The Rust CLI implements `generate-palette-stress-world <worldDir>` as a diagnostic packed-palette encoding fixture,
  not as Rust-side ore or structure generation. Verified locally with
  `rust/scripts/compare-golden.ps1 -GoldenRoot D:\earthmap\rust-port-golden\palette-candidate-smoke-20260604-091949\golden -CandidateRoot D:\earthmap\rust-port-golden\palette-candidate-smoke-20260604-091949\candidate`.
  The Rust-generated palette-stress MCA candidate region file is byte-identical to the Java oracle output:

```text
entry,javaRegionSha256,rustRegionSha256,byteIdentical
palette-stress-mca,83645DC0C7E0F6BE63AF5A36F2EB6F1B585FAC0F93A7A2DCB70200527123AF7F,83645DC0C7E0F6BE63AF5A36F2EB6F1B585FAC0F93A7A2DCB70200527123AF7F,true
```
- The Rust CLI includes diagnostic commands for:
  - `write-sha256-manifest`
  - `write-region-payload-manifest`
  - `append-region-payload-manifest`
  - `compare-region-payload-manifest`
- The Java CLI includes `generate-flat-test-world <worldDir> <mca|linear>` so flat MCA and Linear oracle fixtures can be
  generated without test-only entrypoints.
- The Java CLI includes `generate-palette-stress-world <worldDir>` so packed-palette oracle fixtures can be generated
  without test-only entrypoints.
- Remaining Phase 0 work: generate Rust candidate outputs for Java-oracle corpus entries beyond the synthetic
  flat/palette bootstrap; expand the corpus to height-only, surface, photo, water, vanilla-delegated evidence, quality,
  and OSM entries; add metadata/compression-delta reports; and add CI wiring.

Suggested commands:

```powershell
.\scripts\test.ps1
.\rust\scripts\generate-golden.ps1 -OutputRoot D:\earthmap\rust-port-golden\v001
.\rust\scripts\compare-golden.ps1 -GoldenRoot D:\earthmap\rust-port-golden\v001
.\rust\scripts\generate-candidates.ps1 -GoldenRoot D:\earthmap\rust-port-golden\v001 -OutputRoot D:\earthmap\rust-port-candidates\v001
.\rust\scripts\compare-golden.ps1 -GoldenRoot D:\earthmap\rust-port-golden\v001 -CandidateRoot D:\earthmap\rust-port-candidates\v001

# Extended fixed-region height-only corpus, uses C:\earth_map_resources\HQheightmap.tif unless -HeightmapPath is set.
.\rust\scripts\generate-golden.ps1 -OutputRoot D:\earthmap\rust-port-golden\v001-height -IncludeHeightOnly
.\rust\scripts\generate-candidates.ps1 -GoldenRoot D:\earthmap\rust-port-golden\v001-height -OutputRoot D:\earthmap\rust-port-candidates\v001-height
.\rust\scripts\compare-golden.ps1 -GoldenRoot D:\earthmap\rust-port-golden\v001-height -CandidateRoot D:\earthmap\rust-port-candidates\v001-height

# Extended fixed-region no-material surface corpus, uses C:\earth_map_resources\HQheightmap.tif unless -SurfaceHeightmapPath is set.
.\rust\scripts\generate-golden.ps1 -OutputRoot D:\earthmap\rust-port-golden\v001-surface -IncludeSurface
.\rust\scripts\generate-candidates.ps1 -GoldenRoot D:\earthmap\rust-port-golden\v001-surface -OutputRoot D:\earthmap\rust-port-candidates\v001-surface
.\rust\scripts\compare-golden.ps1 -GoldenRoot D:\earthmap\rust-port-golden\v001-surface -CandidateRoot D:\earthmap\rust-port-candidates\v001-surface
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
- Rust `WwfEcoregionSampler` source bootstrap reads WWF shapefile/DBF source data, regenerates Java-compatible cache
  files when the cache is missing/stale/corrupt, and keeps Java's tolerant auto-discovery fallback behavior.
- Rust `MetImageExportTerrainSampler` bootstrap discovers Java MET `image_exports` terrain-token PNG tiles, parses GDAL
  aux GeoTransform metadata, keeps Java's 512x512 fallback rule, samples token colors through a bounded image cache,
  and lets `EarthDataSurfaceMaterialSampler` prefer exported terrain tokens over Java standard-palette synthesis.
- Rust `SurfaceBiomeCellWriter` bootstrap ports Java render-aware 4x4 biome cell selection, vertical surface-band
  coverage, and static carrier fallback fixture cases.
- Rust surface chunk builder bootstrap wires normalized chunk columns through production natural-surface sanitization,
  Java-style dominant default-biome selection, and `SurfaceBiomeCellWriter` application fixture cases.
- Rust `EarthSurfaceChunkSampler` basic-path bootstrap samples heightmap-backed chunk columns with Java-compatible
  water decisions, smoothing, coast factors, shaped classification, and coastal cleanup fixture cases.
- Rust `SurfaceClassSmoother` non-photo bootstrap ports isolated-column smoothing, protected water surfaces, and
  snowy-biome top compatibility fixture cases.
- Rust `SurfaceClassSmoother` photo texture bootstrap ports Java local speckle smoothing, dry-vegetation sand
  absorption/preservation, desert-boundary preservation, and macro vegetation fixture cases.
- Rust `PhotoSurfaceSolver` contract bootstrap adds Rust `PhotoSurfaceInput`/`PhotoSurfaceDecision`, decision-to-column
  conversion, water/no-photo preservation, and representative color-only nearest-palette lush/arid fixture cases.
- Rust `PhotoSurfaceSolver` Java Standard arid-token bootstrap adds bounded fixture coverage for exact MET sand/snow
  token handling, dry false-snow avoidance, coastal tan carrier reduction, and 4x4 ordered arid carrier variation.
- Rust `PhotoSurfaceSolver` Java Standard vegetation-token bootstrap adds bounded fixture coverage for near-black
  Standard shadow carriers, gray-olive static carrier selection, dry-open vegetation/crust, and dark canopy cases.
- Rust `SurfaceBiomeFamilyIntentGrid` non-photo bootstrap ports 4x4 cell stabilization, small biome-family component
  absorption, protected wetland/snow/beach handling, and arid-transition preservation fixture cases.
- Rust `SurfaceBiomeFamilyIntentGrid` preserve-surface bootstrap ports Java photo-palette render locks, biome-only
  replacement, and top/filler preservation fixture cases.
- Rust OSM overlay bootstrap ports Java `OsmRegionFeatureMask` line raster clipping and `OsmSurfaceOverlay`
  road/building/waterway/landuse priority plus block-placement fixture cases.
- Rust `LandShallowTopoPhotoSampler` bootstrap discovers Java's west/east `land_shallow_topo_*.tif` halves, samples
  topographic colors with Java half-world coordinate mapping, and lets `EarthDataSurfaceMaterialSampler.sample_photo`
  follow Java photo-source preference, coarse evidence caching, and terrain-token attachment rules.
- Rust production surface cleanup bootstrap ports Java `NaturalSurfaceBlockPolicy` and `CoastalSurfaceCleaner` fixture
  cases for replacing artificial palette carrier blocks with natural top/filler blocks; full surface-region output wiring
  remains part of fixed-region `generate-surface-region` parity.
- Rust `generate-surface-region` command bootstrap wires Java-shaped region-wide no-material sampling with Java default
  photo-mode post-processing, distance-transform coast-factor calculation, chunk building, level.dat writing,
  exploration-only surface manifest metadata, region writing, and Java-shaped stdout for the default command path.
- Rust `SurfaceTextureMode` bootstrap ports Java texture-mode IDs and parser aliases, keeps `photo` as the default
  `SurfaceRegionSettings` mode, routes the no-material region sampler through matching classified/photo post-processing
  chains, and lets surface-region manifests preserve the selected mode for the upcoming fixed-region material/photo
  output wiring.
- Rust surface-region material wiring bootstrap lets `SurfaceRegionSettings` carry an optional TrueMarble/EarthData
  material raster path, opens `EarthDataSurfaceMaterialSampler` for configured photo-mode region generation, applies the
  Rust photo solver to sampled material columns with Java-shaped material-sampling conditions and local-relief inputs,
  records `features.surfaceMaterialRaster=true` when configured, and initially kept classified material sampling gated
  until the semantic `EarthSurfaceMaterialClassifier` path was wired.
- Rust `EarthSurfaceMaterialClassifier` bootstrap now applies sampled material metadata before photo solving and ports the
  Java color-only land/water classifier baseline for representative desert dry-grass, Sahel, Congo rainforest, Atlas
  rock, deep-ocean, shelf-water, bright-water, and missing-raster fixture cases. Broader semantic intent/environment/
  ecoregion classifier parity remains tracked as follow-up work for fixed-region Java/Rust output parity.
- Rust `EarthSurfaceMaterialClassifier` semantic fallback bootstrap ports Java's no-color climate/vegetation/ecoregion
  fallback colors plus bounded climate intent cases for savanna, desert, Congo rainforest, temperate steppe, and
  no-color forest/desert/savanna fixture coverage.
- Rust semantic intent material selection now mirrors Java compiler behavior for rainforest, wetland, and snow bootstrap
  cases, including lush rainforest `MOSS_BLOCK` terrain-token evidence, muddy wetland fine-noise patches, and
  latitude-based snowy biome selection.
- Rust dry semantic intent material selection now uses Java-shaped conservative dry-grass top selection for existing
  dry-savanna/desert-edge bootstrap paths, preserving JavaStandard coarse terrain-token evidence instead of flattening
  those patches back to plain grass.
- Rust semantic intent ordering now includes the Java `sahelBand` early dry-savanna/desert-edge compiler branch so
  olive dry-grass, vegetation evidence, and dry Sahel coordinates resolve through intent before environment fallback.
- Rust Mediterranean semantic intent now uses the Java compiler's Mediterranean surface and biome selection for
  non-strong-dry-core coordinates before broader temperate/dry fallbacks.
- Rust steppe climate intent now follows Java's dry-savanna threshold gates and Java-shaped temperate grassland biome
  selection instead of treating all low-latitude steppe as dry savanna.
- Rust dry-savanna score intent now mirrors Java's `drySavannaScore >= 0.35 && !ecoregionDryCore` compiler gate before
  desert and broader environment fallbacks.
- Rust non-savanna JavaStandard vegetated terrain-token intent now mirrors Java's dry-savanna/desert-edge compiler gate
  for warm dry evidence without stealing high-confidence savanna-like ecoregion cases that Java handles earlier.
- Rust exposed highland rock intent now mirrors Java's `HIGHLAND_ROCK` compiler branch and highland rock surface/biome
  helper for rugged orange-rock terrain while preserving Java's earlier high-confidence savanna-like ecoregion ordering.
- Rust savanna-like ecoregion intent now mirrors Java's high-confidence desert-edge, hot-desert sand-patch, and
  temperate-grassland transition gates before JavaStandard and exposed-rock fallback intents.
- Rust JavaStandard dry GRAVEL/ROCK token intent now mirrors Java's secondary `HIGHLAND_ROCK` compiler branch for
  high-elevation or high-relief dry terrain-token evidence.
- Rust dry-core ecoregion transition intent now mirrors Java's confidence/noise blended `DESERT_EDGE` compiler branch
  so uncertain desert cores can resolve to savanna edge before broad hot-desert fallback.
- Rust broad dry-core/desert/Sahara semantic fallback now mirrors Java's `HOT_DESERT` compiler branch, including
  vegetated `DESERT_EDGE` promotion before the hot-desert surface helper is used.
- Rust forest-like ecoregion and temperate/cold fallback intent now mirrors Java's `TEMPERATE_FOREST` compiler branch,
  including the Java-shaped temperate biome helper.
- Rust classified material region sampling now opens the Java-shaped semantic classifier path for material rasters and
  skips only the photo solver pass when `textureMode=classified`.
- Rust ecoregion fallback now ports Java's `classifyByEcoregion` stage, including transition deferral plus beach,
  snow/peaks, swamp, jungle, savanna, desert, badlands, forest/taiga, meadow, and plains biome-key handling after
  intent/environment fallbacks.
- Rust no-climate herbaceous/shrub orange-texture environment fallback now mirrors Java's `classifyByEnvironment`
  dry-grass promotion instead of falling through to broad badlands/rock color rules.
- Rust photo solver snow-evidence handling now mirrors Java's non-token ecology constraint, preserving calcite nearest
  matches and otherwise forcing snow evidence to `SNOW_BLOCK` while retaining dry false-snow guards and Java
  decision-source/stage metadata.
- Phase 5 status note on 2026-06-04: the earlier semantic classifier/photo-solver backlog has largely been implemented
  through subsequent commits. Do not treat Phase 5 as unstarted. No-material fixed-region surface corpus entries are
  now promoted; remaining photo/material promotion is blocked until the payload delta below is explained and fixed.
- Known photo/material promotion blocker on 2026-06-04: a local Rust CLI prototype for
  `generate-vanilla-delegated-region E:\HQheightmap.tif <world> 5000 0 0 linear surface surfaceRaster=auto` matched
  Java stdout shape, manifest metadata, cache stats, and region summary, but failed chunk payload parity. Evidence:
  `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-101831`; `compare-golden.ps1` reported
  `matchingChunks=491`, `mismatchedChunks=533`, and `firstMismatch=8,0`. Do not add photo/material corpus entries or
  mark the Rust vanilla-delegated command as production-ready until this payload delta is explained and fixed.
- Follow-up diagnosis on 2026-06-04: `summarize-region-chunk` shows the first mismatch is concentrated in chunk biome
  palettes and one ocean-floor heightmap edge, not in chunk coordinates or status metadata. Java includes
  `minecraft:windswept_savanna` in mixed biome sections for chunk `(8,0)` while the Rust candidate has simpler
  `deep_lukewarm_ocean|savanna` palettes. The missing Rust PHOTO-mode region-wide two-pass `TokenLumaProfile` flow is
  now implemented; the later TokenLumaProfile and bathymetry shelf reruns below confirmed this was not the full payload
  delta.
- TokenLumaProfile rerun on 2026-06-04: Rust now has a probe-only single-region
  `generate-vanilla-delegated-region` command for parity evidence generation. Evidence:
  `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-tokenluma`. The Java oracle and Rust
  candidate still report `matchingChunks=491`, `mismatchedChunks=533`, and `firstMismatch=8,0` via
  `compare-region-payload-manifest`. Diagnostics written under the probe `diagnostics` folder show chunk `(8,0)` still
  diverges in biome palettes and one `OCEAN_FLOOR` packed-heightmap edge: Java keeps
  `deep_lukewarm_ocean|windswept_savanna|savanna` in mixed sections while Rust collapses several sections to
  `deep_lukewarm_ocean` or `deep_lukewarm_ocean|savanna`. Keep `rust.command.generate-vanilla-delegated-region` as
  WIP/probe-only and do not add photo/material corpus entries until this delta is fixed.
- Bathymetry shelf follow-up on 2026-06-04: `trace-surface-region-column` narrowed one blocker column `(131,0)` to
  Rust using the ordinary coastal shelf adjustment for bathymetry where Java uses
  `coastalBathymetryShelfAdjustedDepthBlocks`. After porting that bathymetry-specific shelf path, the rerun evidence
  folder `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-bathymetry-shelf` improved
  payload parity to `matchingChunks=527`, `mismatchedChunks=497`, `firstMismatch=8,0`. Chunk `(8,0)` now reports
  `heightmap.OCEAN_FLOOR.diffCount=0` and `block.diffCount=0`; the remaining first mismatch is
  `biomeCell.diffCount=29`, expected `minecraft:windswept_savanna`, actual `minecraft:savanna`.
- JavaStandard vegetation palette follow-up on 2026-06-04: the 4x4 trace for chunk `(8,0)`, cell `(3,0)` showed Rust
  never emitted `minecraft:windswept_savanna` for the land inputs that Java classified as dry high-relief grass. The
  Rust photo solver now mirrors the JavaStandard vegetation/source-render palette path for that seed. Trace column
  `(146,2)` now writes `photo.topBlockStateId=4`, `photo.biomeId=minecraft:windswept_savanna`, and
  `photo.decisionSource=photo-palette`; the cell counts now match Java (`deep_lukewarm_ocean=8`, `warm_ocean=4`,
  `savanna=1`, `windswept_savanna=3`). The single-region comparison improved to `matchingChunks=531`,
  `mismatchedChunks=493`, and `firstMismatch=12,0`. Evidence:
  `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-biome-palette-solver`.
- Verification-time parallelism follow-up on 2026-06-04: Rust now uses deterministic Rayon fan-out for surface column
  material/photo sampling and surface chunk build/NBT encoding. This was accepted before full Phase 7 only to shorten
  Phase 5 verification runs. The standard HeightMap path for the CLI evidence run was
  `C:\earth_map_resources\HQheightmap.tif`. The release rerun evidence folder
  `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-default-heightmap-parallel` produced
  the same region SHA-256 and identical payload manifest as the prior Rust candidate; Java comparison remains
  `matchingChunks=531`, `mismatchedChunks=493`, `firstMismatch=12,0`. Internal timing improved from
  `phase.surfaceSampleMillis=331235`, `phase.chunkBuildMillis=3310`, `phase.nbtEncodeMillis=24836`,
  `phase.totalInternalMillis=359668` to `phase.surfaceSampleMillis=100452`, `phase.chunkBuildMillis=749`,
  `phase.nbtEncodeMillis=3344`, `phase.totalInternalMillis=100890`. These are evidence-run timings, not an isolated
  parallelism-only benchmark.

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

Only start full optimization after Phase 5 parity is green for surface output.

Pre-Phase7 verification-time exception already accepted:

- Deterministic Rayon fan-out is allowed for surface column material/photo sampling and surface chunk build/NBT encoding
  because it preserves byte-identical Rust output and shortens red-gate verification loops.
- This exception does not approve output-changing fast paths, compression backend swaps, unordered reductions, SIMD
  approximations, or broader Phase 7 promotion while Phase 5 payload parity is red.
- Evidence from `D:\earthmap\rust-port-golden\vanilla-delegated-linear-probe-20260604-rustcli-default-heightmap-parallel`:
  previous and new Rust `r.0.0.linear` SHA-256 values match exactly, payload manifests have no diff, and Java comparison
  remains `matchingChunks=531`, `mismatchedChunks=493`, `firstMismatch=12,0`.

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
4. Done: prove synthetic flat/palette MCA/Linear candidate parity from Rust-generated chunks.
5. Done: add `generate-candidates.ps1` so Rust candidate roots can be regenerated from Java oracle roots.
6. Done: add optional height-only fixed-region entries to the regeneratable golden/candidate corpus.
7. Done: add optional no-material surface fixed-region entries to the regeneratable golden/candidate corpus.
8. Done: rerun the vanilla-delegated photo/material payload comparison after the TokenLumaProfile fix.
9. Active: resolve the remaining chunk `(8,0)` biome-cell representative mismatch before adding photo/material
   fixed-region entries. The earlier `OCEAN_FLOOR` and block payload delta for this chunk is fixed in the latest
   bathymetry shelf rerun.
10. Next: expand Phase 6 quality and visual evidence tools only after the relevant corpus parity is green.
