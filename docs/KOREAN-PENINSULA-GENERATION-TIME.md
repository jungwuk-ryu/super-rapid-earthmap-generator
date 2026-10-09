# Korean Peninsula 1:200 Linear generation time

The complete GUI **Korean Peninsula + Jeju** preset generated in **40.31 seconds**
on the current **four-CPU quota and one generation process**. This is **one actual
complete-preset run**, after downloading northern and boundary coverage, with
default automatic worker tuning included. The earlier 30–40-second projection
is superseded by this measurement.

The release binary is unchanged from `ac904f5`; the repository at measurement
was `6de0dbd`. Download/conversion time, build time, subsequent validation, and
Minecraft server finalization are excluded from the 40.31 seconds.

## Complete-preset measurement

Measured on 2026-10-10 in Asia/Seoul, using a fresh generator process and world.
Exactly one complete-preset generation was invoked; startup tuning performs
its normal internal sample calibration. The OS filesystem cache was not flushed
after input preparation and coverage checks.

| Phase | Elapsed |
| --- | ---: |
| Automatic worker/compute-thread tuning | 18.81 s |
| Region generation | 21.38 s |
| Other startup/exit work | 0.13 s |
| **Total process wall time** | **40.31 s** |

Phase values are rounded independently.

The GUI preset is **124–132°E, 33–43.5°N**. Its enclosing region grid starts at
**(134, -48)** and is **10 × 13 = 130 regions**, containing **133,120 chunks**
and **34,078,720 surface columns**. It is a **5,120 × 6,656-block** rectangle,
including surrounding seas and adjacent land. Complete regions extend to
approximately **123.262–132.461°E, 32.195–44.154°N**.

Results:

- All 130 region files generated, with zero batch failures or resumed regions.
- All 130 files passed the Linear V2 validator: 1,024 payloads per region,
  consistent bitmaps, and no missing or extra payloads.
- Region output: **63.36 MiB**; peak process memory: approximately **3.13 GiB**.
- Land represents 46.17% of generated columns.
- SHA-256 identities were recorded for every output file and the prepared inputs.

Settings match the preceding regional timing:

- Linux x86-64, AMD EPYC 9V74; CPU quota `400000 100000`, memory limit 16 GiB.
- One generator process; automatic tuning selected four region workers and
  four compute threads.
- Linear zstd level 4, `chunkStatus=surface`, photo material sampling.
- `verticalScale=auto`, resolving to 4.0 at scale 200; prefetch disabled.
- Automatic caches: 8,192 heightmap rows and 4,321 surface tile entries.
- No manual thread, CPU-budget, or cache-size environment overrides.

[Single-run timings, coverage, tuning decision, validation, and hashes](benchmarks/korean-peninsula-full-1-200-once-2026-10-10.json).

## Downloaded coverage and quality

Real source coverage is **123–133°E, 32–45°N**, including the complete region
boundaries. Resolution is unchanged: **1 arcsecond elevation** and **2 arcsecond
RGB**, using the same source families and resampling methods.

- Elevation: 130 Mapzen/Tilezen Skadi HGT tiles; 48 existing tiles reused and
  82 additional tiles downloaded. HTTPS and gzip CRC checks were retained.
  The 593,143,932 additional source bytes are compressed HGT files.
- RGB: 166 newly prepared Sentinel-2 L2A COG windows, alongside the existing
  southern mosaic from 83 scenes. There are 202 distinct scene IDs across these
  sources. Only required COG windows/overview data were read, rather than entire
  10 m products.
- Audited missing land was supplemented using 32 additional scenes selected
  for adequate image coverage as well as low cloud cover.
- Every one of the **148,184,979 previously available southern RGB pixels**
  remains identical; **4,149,527 previously black pixels** were filled with
  real imagery. Generation code and quality settings were unchanged.

Over the actual 130-region footprint, the 2-arcsecond audit found:

| Coverage check | Result |
| --- | ---: |
| Missing elevation pixels | **0** |
| RGB coverage on DEM-above-zero pixels | **99.9063%** |
| RGB coverage on those pixels north of 39°N | **99.9872%** |

The land mask uses elevation above 0 m; it is not a country boundary and can
include inland water. Remaining black RGB pixels use the generator's existing
missing-imagery behavior. Imagery is a low-cloud April–October 2025 mosaic,
with scene cloud cover up to 4.83%; it is not a cloud-free temporal composite.
Pixel spacing does not imply that all original terrain sources have that accuracy.

Attribution: **Mapzen; SRTM and GMTED2010 terrain data courtesy of the U.S.
Geological Survey; ETOPO1 terrain data, U.S. National Oceanic and Atmospheric
Administration. Contains modified Copernicus Sentinel data 2025.**

The runtime inputs are approximately **4.35 GiB** in
`/workspace/.earthmap-cloud/data-full-peninsula`. The heightmap exposes global
latitude metadata **[-90°, 90°]** to preserve the GUI's Earth-centered region
coordinates. Real 32–45°N rows are retained unchanged; rows outside that range
reference a shared NoData strip. **No NoData padding is sampled by the preset.**
This avoids both a global DEM allocation and an incorrect regional Z origin.

Preparation was recorded separately: elevation 127.60 s, initial RGB 322.63 s,
and audited RGB supplementation 112.53 s. Early preparation stages overlapped;
these times must not be summed as total preparation wall time.

The generated world is under
`<EARTHMAP_OUTPUT_ROOT>/korean-peninsula-200-full-once`. Source manifests,
coverage results, and an activation script are in the data package. Raw timing
and validation logs are in
`/workspace/.earthmap-cloud/full-peninsula-200-timing`. Large datasets and worlds
remain outside Git.

## Reproduce the complete preset

For this prepared environment, select the complete-peninsula inputs and use the
existing release executable. Choose a fresh output directory for a new task;
reusing the measured world would resume completed regions.

```bash
source /workspace/.earthmap-cloud/activate.sh
source /workspace/.earthmap-cloud/data-full-peninsula/activate-data.sh
unset RAYON_NUM_THREADS EARTHMAP_CPU_BUDGET
unset EARTHMAP_HEIGHTMAP_CACHE_ROWS EARTHMAP_SURFACE_TILE_CACHE_ENTRIES
"$CARGO_TARGET_DIR/release/earthmap-rs" generate "$EARTHMAP_HEIGHTMAP" \
  "$EARTHMAP_OUTPUT_ROOT/korean-peninsula-200-fresh" \
  200 134 -48 10 13 linear auto surface \
  "surfaceRaster=$EARTHMAP_SURFACE_RASTER" verticalScale=auto \
  linearCompression=4 prefetch=false
```

This timing covers prepared geographic input → terrain/material sampling →
chunk NBT → compressed Linear files and project metadata. Vanilla server caves,
ores, vegetation, lighting, and later generation stages are excluded. Native
Intel hybrid or Apple Silicon timings require measurements on those machines.

## Earlier regional measurement and projection

Before the northern download, the available input covered **124–132°E,
33–39°N**. That input's regional Z origin produced **80 regions** starting at
**(134, -4)**. Three fresh-process/world runs yielded:

| Run | Total, including tuning | Startup tuning | Region generation |
| --- | ---: | ---: | ---: |
| 1 | 24.94 s | 15.04 s | 9.83 s |
| 2 | 23.76 s | 14.29 s | 9.39 s |
| 3 | 23.94 s | 14.61 s | 9.26 s |
| Median | **23.94 s** | **14.61 s** | **9.39 s** |

All 80 files matched across those repeats. They contained 81,920 chunks and
20,971,520 columns, with 42.38% land and 22.83 MiB output. The earlier
30–40-second planning estimate scaled generation separately from startup tuning
and considered additional dense-land costs. It was an estimate, not a complete
measurement or a guaranteed upper bound.

The complete run has a different region alignment and broader source coverage.
Use the **40.31-second actual single-run result** for the full preset, while
retaining the regional observations as historical evidence.

[Earlier regional timings and file hashes](benchmarks/korean-peninsula-1-200-2026-10-10.json).
