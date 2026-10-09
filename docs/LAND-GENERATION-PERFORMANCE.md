# Land generation performance

Measured on 2026-10-09 against `main` at
`21a75b0255ff248bad823ef238402591cc7f91da`. The goal is at least **4× faster
generation on complex land**, with identical generated region files.

## Workloads and results

Each workload generates a fresh 2×2-region world: 4,096 chunks and 1,048,576
surface columns. Seoul/Gyeonggi includes urban, forest and mountain terrain;
Seorak includes forested mountain slopes. These are real Korean elevation and
Sentinel-2 inputs, rather than uniform ocean or synthetic flat terrain.

| Workload | Region origin | Scale | Land columns | Ground Y range |
| --- | --- | --- | ---: | --- |
| Seoul/Gyeonggi | `(138, -2)` | 1:200 | 1,048,573 | 57–190 |
| Seorak | `(557, -10)` | 1:50 | 1,048,576 | 63–211 |

Wall-clock medians of three measured runs per binary, after one warm-up per
binary and workload:

| Format | Workload | Before | After | Speedup |
| --- | --- | ---: | ---: | ---: |
| MCA | Seoul/Gyeonggi | 13.161 s | 2.898 s | **4.54×** |
| MCA | Seorak | 7.323 s | 1.793 s | **4.09×** |
| Linear | Seoul/Gyeonggi | 12.253 s | 2.819 s | **4.35×** |
| Linear | Seorak | 7.316 s | 1.704 s | **4.29×** |

Raw measured seconds, in repeat order:

| Format / workload | Before runs | After runs |
| --- | --- | --- |
| MCA / Seoul | 13.266, 13.161, 12.606 | 2.922, 2.898, 2.750 |
| MCA / Seorak | 7.313, 7.323, 7.347 | 1.805, 1.793, 1.774 |
| Linear / Seoul | 12.050, 12.253, 12.312 | 2.819, 2.644, 2.947 |
| Linear / Seorak | 7.430, 7.170, 7.316 | 1.704, 1.723, 1.677 |

The complete file SHA-256 maps match the baseline on every run, including
warm-ups and repeatability checks. Compression levels remain MCA 6 and Linear
4. No terrain resolution, palette, biome, smoothing, heightmap, generation
status or chunk count is reduced.

The environment has an AMD EPYC 9V74 CPU, a cgroup quota of four CPU cores
(`cpu.max = 400000 100000`), 32 GiB RAM, Rust 1.99.0 and Linux. Both binaries
use the same release profile (thin LTO, one codegen unit), system allocator and
input files. The main measurements request four region workers and retain the
existing default of eight Rayon compute threads. Options are `surface`,
`verticalScale=auto`, `workerAutotune=false` and `prefetch=false`; prefetch is
also disabled by default in the app. These results apply to these workloads
and settings; they are not a universal throughput guarantee for every raster,
machine or generation mode.

Input files are warm in the filesystem cache. Each timed generation starts a
new process and a fresh output world, so application caches start empty and
resume skipping cannot contribute to the improvement. AB/BA order alternates
between repeats. Builds and tests run outside the timed measurements.

Peak process resident memory, the largest `/proc/<pid>/status` `VmHWM` sampled
across the three measured runs:

| Format / workload | Before | After |
| --- | ---: | ---: |
| MCA / Seoul/Gyeonggi | 949.2 MiB | 859.6 MiB |
| MCA / Seorak | 882.3 MiB | 751.1 MiB |
| Linear / Seoul/Gyeonggi | 954.8 MiB | 840.0 MiB |
| Linear / Seorak | 875.0 MiB | 775.1 MiB |

The baseline binary SHA-256 is
`45a60309a77e81bd435bfd229d4e0a620fffeec3bd56dc85ce9b5bffc54f33f5`;
the optimized binary SHA-256 is
`9a68af16f702a82faae52744b00254b2b963fd75241336b6fb98354265118e27`.
Raw timings and per-region phase events are kept outside the repository in
the benchmark `report.json` and adjacent JSONL logs.

## Why generation is faster

- Build the immutable MET nearest-color octree once. Use bounded thread-local
  tables for exact RGB-to-Lab/CIEDE values and noise lattice corners, checking
  full keys on every hit. A collision recomputes the original result; colors
  and floating-point values are not approximated.
- Deduplicate vegetation and static palette searches within each region.
  Keys retain source/target/token RGB, biome candidates, snow/shadow evidence,
  coastal context and the relevant coordinate-dependent wet-carrier choice.
  Profile and arid dithering continue through their original per-column paths.
- Borrow material samples, biome strings and photo context instead of cloning
  them for each column. Move owned decisions into columns. Reuse unchanged
  columns across photo postprocessing passes, applying changes only after
  each pass has finished reading its complete input snapshot.
- Skip biome-cell counting when all land already shares one biome or there
  is no editable land. Mixed editable cells retain the original majority,
  tie-breaking and protected-surface rules.
- Fill shared underground stone layers contiguously. Encode borrowed section
  states and biomes, compute all three NBT heightmaps in one sparse-section
  traversal, and handle uniform palettes without allocating 4,096 indices.
  Packed words retain their original bit order and padding.
- Compress Linear's independent buckets in parallel while retaining their
  original order, headers and first-error behavior. Reuse MCA's zlib working
  memory with an independently finished/reset stream for each chunk.
- Prefetch producers and consumers now share one Rayon compute pool. Channel
  waits stay on their coordinating OS threads, so a bounded queue also works
  with a single compute thread. Sampling, land output and ocean output no
  longer reserve separate, potentially idle or oversubscribed pools.

The existing thread-count defaults remain in place: uniformly reducing the
pool size did not consistently improve both land workloads. The three legacy
prefetch thread-count metrics now describe the same pool, and are not additive;
`prefetchSharedRayonPool=true` identifies that layout. Queue and memory caps
remain enforced.

## Processes and threads

The same Seoul/Gyeonggi 2×2 Linear workload was measured with the same four-core
quota, three repeats, one warm-up per binary, and `prefetch=false`:

| Layout | Before | After |
| --- | ---: | ---: |
| One process, four region workers, `RAYON_NUM_THREADS=4` | 11.752 s | 2.726 s |
| Four processes, one region and `RAYON_NUM_THREADS=1` each | 12.218 s | 2.927 s |

The optimized single process is about 7% faster than the four-process run here.
The reported multi-process advantage was not reproduced in this particular
non-prefetch baseline either. This comparison checks scheduling with an equal
CPU budget; it does not establish the cause of an earlier result on another
machine or configuration. The full region files remain identical across these
layouts and the default eight-thread pool.

With prefetch enabled, the old process advantage does appear in this same
workload. Each layout prepares up to four regions concurrently (four producers
in the single process, one producer in each of the four processes):

| Prefetch layout | Before | After |
| --- | ---: | ---: |
| One process, four producers, `RAYON_NUM_THREADS=4` | 14.781 s | 2.726 s |
| Four processes, one producer and `RAYON_NUM_THREADS=1` each | 12.324 s | 2.909 s |

Single-process prefetch improves **5.42×** and becomes about 6% faster than the
four-process run. Old telemetry confirms four sampling threads plus two
separate one-thread output pools; the optimized implementation has one shared
four-thread compute pool. This supports correcting the pool layout rather than
moving generation into multiple processes. The speedup combines that change
with the computation/allocation optimizations above, so it should not be
attributed entirely to thread scheduling. The default prefetch producer count
remains one; this measurement explicitly uses `prefetchWorkers=4`.

## Data and storage

The local dataset covers 124–132°E, 33–39°N. Elevation uses a 1-arcsecond grid
from 48 Mapzen/Tilezen Skadi terrain tiles (SRTM/GMTED2010/ETOPO1). RGB imagery
uses a 2-arcsecond mosaic from 83 real Sentinel-2 L2A scenes in April–October
2025. `terrain/TrueMarble.vrt` is a compatibility filename for that Sentinel
mosaic. Source resolution and accuracy are described in the dataset notes;
neither input is changed by this optimization.

The existing dataset plus source cache uses about 2.12 GiB, within its 3 GiB
budget. Benchmark worlds, logs, datasets and build artifacts stay outside Git.
The benchmark normally keeps only the first measured before/after world pair
per workload and deletes other worlds after recording their hashes. Its data
paths come from `EARTHMAP_HEIGHTMAP`, `EARTHMAP_DATA_ROOT`, `EARTHMAP_TIF_ROOT`,
`EARTHMAP_SURFACE_RASTER` and `EARTHMAP_OUTPUT_ROOT`.

Dataset SHA-256 identities:

| File | SHA-256 |
| --- | --- |
| Elevation GeoTIFF | `ed5b371669d9ebdb7fc1d0c1871bd908e206abaec598b50e9bd0b2befe272bb3` |
| Sentinel-2 RGB GeoTIFF | `0c2473c8044f2b99549d47489cd1f4aabbddb6859544ee45be9214a67bc4d730` |
| Compatibility VRT | `6d9fbb44955fb209f334b8f7c84d9c77d0206a1f2b33e86a63c891706335bc1b` |

Attribution: Mapzen; SRTM and GMTED2010 terrain data courtesy of the U.S.
Geological Survey; ETOPO1 terrain data, U.S. National Oceanic and Atmospheric
Administration. Contains modified Copernicus Sentinel data 2025. Local
`DATASET.json`, source manifests and `README.md` retain the source URLs,
transformation details, terms and coverage audit. See [external data
policy](EXTERNAL-DATA.md).

## Reproduce

Build the baseline commit and the optimized code separately with the same
toolchain and release options. Copy the two executables to immutable paths;
building the optimized code must not overwrite the baseline executable.
Configure the external data variables described in [EXTERNAL-DATA.md](EXTERNAL-DATA.md).
For this managed cloud environment, `source /workspace/.earthmap-cloud/activate.sh`
loads the toolchain and prepared Korean dataset.

```bash
cargo build --manifest-path rust/Cargo.toml -p earthmap-cli --release --locked

# Point these at separately built, immutable baseline and optimized binaries.
export EARTHMAP_BENCH_BEFORE=/path/to/baseline/earthmap-rs
export EARTHMAP_BENCH_AFTER=/path/to/optimized/earthmap-rs
unset RAYON_NUM_THREADS EARTHMAP_SURFACE_PHASE_DETAIL

python3 scripts/benchmark-world-generation.py \
  --before "$EARTHMAP_BENCH_BEFORE" --after "$EARTHMAP_BENCH_AFTER" \
  --case seoul,200,138,-2,2,2 --case seorak,50,557,-10,2,2 \
  --format linear --threads 4 --repeats 3 --warmups 1 --target 4 \
  --output "$EARTHMAP_OUTPUT_ROOT/land-benchmark-linear"

python3 scripts/benchmark-world-generation.py \
  --before "$EARTHMAP_BENCH_BEFORE" --after "$EARTHMAP_BENCH_AFTER" \
  --case seoul,200,138,-2,2,2 --case seorak,50,557,-10,2,2 \
  --format mca --threads 4 --repeats 3 --warmups 1 --target 4 \
  --output "$EARTHMAP_OUTPUT_ROOT/land-benchmark-mca"
```

Use new output directories for every invocation. The script exits unsuccessfully
if any workload is below the speed goal, if land coverage is below 95%, if a
region is incomplete, or if any complete region file differs from the baseline.
It records commands, executable hashes, environment, CPU/memory limits, raw
timings, peak resident memory, terrain metrics and output hashes in `report.json`.

For a fair process/thread comparison, keep the same four-region workload and
CPU quota. Use `--threads 4 --rayon 4 --processes 1` for one process, then
`--threads 1 --rayon 1 --processes 4` for four processes. The latter generates
each region in a separate fresh world and compares the combined file map.
For the prefetch comparison, add `--prefetch true --prefetch-workers 4` to the
single-process run and `--prefetch true --prefetch-workers 1` to the four-process run.
Set `--target 1` when comparing the scheduling layouts independently of the
4× optimization goal. In process mode the sum of active children's `VmHWM`
is reported as an upper bound, not a simultaneous RSS measurement.

## Validation

The entire Rust workspace passes **372 tests**, including all three normally
ignored full-region/profile/quality-batch tests. Strict Clippy passes for all
four changed packages and their dependencies. Existing Java golden fixtures
also pass; no Java runtime was available for a new live Java comparison.
The additional quality gate compares complete generated files with the
immutable pre-change Rust release, including compression bytes.

Regression coverage includes cache collisions and floating-point bit patterns;
coordinate dithering and noise wrapping; ecology-box boundaries;
postprocessing snapshots; sparse/custom-dimension heightmaps; AIR and partial
layer fills; arbitrary block IDs; packed widths 0–31; single-thread bounded
prefetch queues; and compression/file/error ordering across thread counts.
MCA compression tests also compare each reused zlib stream with a fresh
encoder at levels 0, 1, 6 and 9.

Actual-data regression checks additionally cover Busan's mixed coast
(`140,0`, 174,221 land / 87,923 water columns), the West Sea (`135,0`, all
water), the East Sea including Ulleungdo (`142,-2`, 2,485 land / 259,659 water
columns), and deep East Sea water (`142,0`, all water), at scale 1:200. Their
complete MCA files match the baseline. Prefetch with one queued region and a
256 MiB queue budget completes the four-region Seoul world with both one and
four compute threads, producing the same Linear files as the baseline.

All 32 retained before/after complex-land region files pass the MCA or Linear
validator, with 1,024 chunks each. Cross-format comparisons between the old
MCA and optimized Linear regions match all **8,192 chunk NBT payloads**.

Validation commands:

```bash
cargo test --manifest-path rust/Cargo.toml --workspace --locked -- --include-ignored
cargo clippy --manifest-path rust/Cargo.toml \
  -p earthmap-cli -p earthmap-surface -p earthmap-minecraft -p earthmap-region \
  --all-targets --locked -- -D warnings
cargo fmt --all --manifest-path rust/Cargo.toml -- --check
pwsh -NoProfile -File ./scripts/lint.ps1
```
