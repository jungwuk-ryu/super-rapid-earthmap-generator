# Large Linear world generation performance

These measurements describe `64223f9`. The subsequent
[CPU topology and thread-selection change](CPU-THREAD-SELECTION.md) adds hybrid
CPU detection and automatic thread candidates; it has separate validation.

Evaluated on 2026-10-09 against `7548edcac2b0debc6ae99580a016dce9d78f2bdc`.
That baseline already contains the earlier land-generation optimizations in
`LAND-GENERATION-PERFORMANCE.md`. This measurement starts from the current
generator, rather than crediting those earlier improvements again.

The measured path is geographic elevation and RGB rasters → surface sampling,
classification and cleanup → Minecraft chunks and NBT → Linear compression →
files on disk. The goal is another 3× improvement with the same CPU and process
budgets and the same generated world.

## Equal-resource measurements

| Workload | Grid / scale | Regions / chunks | Land | Ground Y |
| --- | --- | ---: | ---: | --- |
| Large land grid | `(552,-12)`, 8×8, 1:50 | 64 / 65,536 | 93.75% | −60–211 |
| Inland grid | `(552,-12)`, 6×8, 1:50 | 48 / 49,152 | 99.91% | 35–203 |

Wall-clock medians, including the automatic compute-thread adjustment:

| Workload | Before | After | Speedup | Peak RSS before → after |
| --- | ---: | ---: | ---: | ---: |
| Large land grid | 27.507 s | 9.087 s | **3.03×** | 1,033.7 → 534.8 MiB |
| Inland grid | 21.516 s | 7.099 s | **3.03×** | 1,022.8 → 498.8 MiB |

The grids contain 16,777,216 and 12,582,912 columns respectively. Every complete
compressed region file matches the baseline on all eight runs per workload.

| Workload | Before measured seconds | After measured seconds |
| --- | --- | --- |
| Large land grid | 27.507, 27.051, 28.188 | 8.737, 9.087, 9.378 |
| Inland grid | 21.516, 20.314, 22.402 | 7.099, 7.015, 7.104 |

As a stricter control, both binaries were also measured with exactly four
compute threads (`RAYON_NUM_THREADS=4`), the same four region coordinators, one
warm-up and three AB/BA repeats:

| Large land grid, explicit four compute threads | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| Complete Linear generation | 23.970 s | 9.720 s | **2.47×** |

Raw seconds are 24.813, 23.934, 23.970 before and 9.889, 9.347, 9.720 after.
Complete files match in this control too. The **3.03× result includes automatic
compute-thread sizing**; the algorithm changes alone do not demonstrate 3×
when the compute-thread counts are fixed. Physical CPU quota and generation
process count are unchanged in both comparisons.

Both variants run under the same four-core cgroup quota (`400000 100000`) on
AMD EPYC 9V74, with one generation process and four region coordinators. They
use Linux, Rust 1.99.0, a 16 GiB cgroup memory limit, the system allocator and
the same release profile (thin LTO, one codegen unit). The automatic Rayon setting changes from
eight compute threads to four because Rust reports four available CPUs in this
environment. An explicit `RAYON_NUM_THREADS` setting still takes precedence.
This change reduces oversubscription within the existing CPU allocation.
The existing coordinator minimum can still raise a selected pool above the
CPU budget when more region workers are explicitly requested.

Settings are `surface`, `verticalScale=auto`, `workerAutotune=false`,
`prefetch=false`, Linear compression 4, heightmap cache 64 rows and surface tile
cache 32 entries. Per-column diagnostic clocks are disabled in both binaries.
Raster resolution, color-distance formulas, palette candidates, biome rules,
terrain smoothing and chunk generation status remain unchanged.

Each binary gets one warm-up and three measured runs per workload. Every run
starts a fresh process and output world; application caches and resume state
start empty. Execution alternates AB/BA. Inputs are warm in the filesystem
cache. Build, test and profiling processes run outside the timed measurements.
Timing includes startup, sampling, chunks, NBT, compression and writing.

Peak memory is the largest `/proc/<pid>/status` `VmHWM` observed in measured
runs. Complete SHA-256 maps are checked after every run, including warm-ups and
baseline repeats. Region counts and 1,024 freshly generated chunks per region
are also required. These checks preserve the complete compressed Linear files,
including all encoded block states, biomes and heightmaps.

## Changes supported by profiling

The remaining baseline cost concentrates in repeated short biome checks,
ecology/noise evaluation, color candidate scoring, large temporary column
records, and section palette/NBT encoding. Profiling guided removal of repeated
work and intermediate allocations while retaining the original decisions.

- Borrow cached heightmap rows once per output row and prepare longitude pixel
  coordinates once per grid. Scalar interpolation order and raster edge
  clamping are preserved.
- Evaluate ecology scores and noise only when the selected rule needs them.
  Conservative geographic upper bounds can reject a threshold comparison;
  scores used in arithmetic still use the original calculation.
- Use compiled keyword masks for known vanilla biomes and byte comparisons for
  short ASCII terms. Custom identifiers retain their original case and
  substring behavior.
- Share exact palette searches across columns, regions and compute workers.
  Keys retain RGB, terrain token and the flags that influence the score. Static
  palettes have no biome-tinted candidates, so their top/score can be shared
  while each column retains its own compatible biome. Equal rendered colors
  retain the first original candidate, preserving tie decisions.
- Reuse existing coordinate, coast, relief and material arrays; box the rare
  owned photo sample instead of reserving its complete storage in every column
  record. Use indexed collections and mutate photo decisions in place to avoid
  large intermediate record copies.
- Process fully valid smoothing tiles contiguously, retaining each column's
  original floating-point addition order. Separable relief extrema retain the
  original result; invalid, nonfinite and signed-zero inputs use the original
  neighborhood traversal.
- Keep solid underground sections uniform until a block differs. Pack palette
  indices by runs and stream chunk NBT directly, preserving tag/list order,
  first-seen palettes, packed-word padding and modified UTF-8. The public NBT
  tree encoder remains a regression oracle.
- Move owned columns through final coastal and production cleanup, retaining
  their biome and diagnostic-string allocations. Empty biome normalization and
  source suffixes are preserved.
- Cap automatic Rayon sizing at the available CPU budget. Explicit thread
  settings and the existing maximum remain available.

Caches check complete keys and are bounded: MET remapping has 65,536 slots per
compute thread, the CIEDE cache has 1,048,576 slots per thread, local palette
solves have 65,536 entries and the shared region solve cache has at most 262,144
entries before clearing. Cache collisions, clearing and a poisoned shared cache
fall back to the original calculation. No approximate color or noise values
are introduced.

## Verification and scope

The full locked workspace suite passed **384 tests**, including all three
previously ignored tests. Clippy passed with `-D warnings` for all targets of
the four affected crates: CLI, geo, surface and Minecraft. The complete
workspace test build also reports an existing GUI float-literal warning at
`earthmap-gui/src/main.rs:1480`; that file is outside this change.

Focused regressions compare lazy ecology thresholds and scores over a global
coordinate grid and region boundaries, weighted neighborhoods by floating-point
bits, MET cache collisions, palette order and scores, and 12,600 material-first
classification cases. NBT streaming is compared with the original tree encoder
across dimensions, statuses, uniform/dense sections, block entities, unknown
IDs and modified UTF-8 names. Palette packing covers word-crossing runs and all
4,096 possible section entries.

Additional fresh-world checks preserve all eight Linear files across the
Seoul/Gyeonggi and Seorak 2×2 grids, and a Seoul MCA region with one compute
thread. These single-run checks extend output coverage; the 3× claim uses the
repeated large-workload measurements above.

The baseline executable SHA-256 is
`9a68af16f702a82faae52744b00254b2b963fd75241336b6fb98354265118e27`;
the optimized executable is
`2edb2949f4049d33ccdffad9b233a5546306157ae4314e39345d5b641f7db0bb`.

The available real dataset covers 124–132°E, 33–39°N. It combines a 1-arcsecond
Mapzen/Tilezen elevation grid with a 2-arcsecond Sentinel-2 RGB mosaic. The
`terrain/TrueMarble.vrt` name is a compatibility path for that mosaic. Both
binaries read the same files. These are large real land workloads, but they do
not establish a universal speedup for an entire continent or every raster,
machine, worker setting or generation mode.

## Reproduction

Build the baseline commit and this revision with the same locked release
profile, preserving their executables as `$BEFORE` and `$AFTER`. Set
`EARTHMAP_HEIGHTMAP`, `EARTHMAP_SURFACE_RASTER`, `EARTHMAP_DATA_ROOT`,
`EARTHMAP_TIF_ROOT` and `EARTHMAP_OUTPUT_ROOT` to the actual dataset and output
paths. Run under the same CPU quota and without competing builds or tests:

```bash
unset RAYON_NUM_THREADS
python3 scripts/benchmark-world-generation.py \
  --before "$BEFORE" --after "$AFTER" \
  --case continent,50,552,-12,8,8 \
  --case inland,50,552,-12,6,8 \
  --threads 4 --processes 1 --format linear --prefetch false \
  --warmups 1 --repeats 3 --target 3 --min-land-fraction .90
```

Add `--rayon 4` to compare with equal explicit compute-thread counts. The
committed [compact benchmark JSON](benchmarks/linear-generation-2026-10-09.json)
records raw measured times, resource settings and binary identities.
Full `report.json`, per-region JSONL events, datasets,
worlds and build artifacts stay outside Git.
