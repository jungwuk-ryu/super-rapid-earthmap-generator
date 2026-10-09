# Korean Peninsula 1:200 Linear generation time

On the current **four-CPU quota and one generation process**, budget approximately
**30–40 seconds** for the GUI's **Korean Peninsula + Jeju** preset at **1:200**,
with default automatic worker tuning. This is a projection, not a complete
peninsula measurement: the prepared elevation and RGB rasters end at **39°N**.

The current build is `ac904f5`. Inputs are already prepared GeoTIFF/VRT files;
data acquisition/preparation and Minecraft server finalization are excluded.

## Measured result

The available raster extent is **124–132°E, 33–39°N**. Its enclosing regional
generation grid is **10 × 8 = 80 regions**, containing **81,920 chunks** and
**20,971,520 surface columns**. Generate each repeat into a fresh world with a
fresh generator process; the OS filesystem cache was not flushed.

| Run | Total, including tuning | Startup tuning | Region generation |
| --- | ---: | ---: | ---: |
| 1 | 24.94 s | 15.04 s | 9.83 s |
| 2 | 23.76 s | 14.29 s | 9.39 s |
| 3 | 23.94 s | 14.61 s | 9.26 s |
| Median | **23.94 s** | **14.61 s** | **9.39 s** |

Median component times need not belong to the same run. Other startup/exit work
has a median of approximately 0.07 seconds.

All three runs selected **four region workers and four compute threads**.
Every run generated 80 complete files with 1,024 chunks per region and no batch
failures. All 80 compressed files have identical SHA-256 hashes across repeats;
the sampled Seoul region also passed the Linear V2 validator with a consistent
1,024-chunk bitmap. Land represents 42.38% of the generated columns. Output is
22.83 MiB, with peak process memory between approximately 1.73 and 2.02 GiB.

Settings:

- Linux x86-64, AMD EPYC 9V74; cgroup CPU quota `400000 100000` and memory limit 16 GiB.
- Linear zstd level 4, `chunkStatus=surface`, photo material sampling.
- `verticalScale=auto`, which resolves to 4.0 at scale 200.
- `threads=auto`, worker autotune enabled, prefetch disabled.
- Automatic caches: 8,192 heightmap rows and 4,321 surface tile entries.
- No `RAYON_NUM_THREADS`, `EARTHMAP_CPU_BUDGET`, or manual cache-size overrides.

## Whole-peninsula estimate

The GUI preset covers **124–132°E, 33–43.5°N**, including surrounding sea, and
resolves to **10 × 13 = 130 regions** with the normal Earth-centered mapping.
This is **133,120 chunks**, a **5,120 × 6,656-block** enclosing grid, and
**34,078,720 surface columns**. It is not a land-only polygon.

Scale only the generation phase; startup tuning is paid once per run:

```text
14.61 s tuning + 9.39 s × (130 / 80) generation + 0.07 s other
≈ 29.94 seconds
```

Northern coverage can have a different land/terrain mix. As a sensitivity check,
regions with at least 90% land average 705 ms of worker elapsed time over the
three runs. If all 50 additional regions cost that much, the total estimate is
**32.75 seconds**. If they all cost as much as the slowest observed dense-land
region (1,219 ms), it is **39.18 seconds**, assuming similar four-worker
throughput. These scenarios support a **30–40-second planning estimate**;
they are not a confidence interval or a guaranteed upper bound for unmeasured
northern data.

Prepared northern elevation and satellite imagery are required to measure the
entire peninsula without replacing missing geography with edge/fallback data.
The regional measurement derives its Z origin from the input's 33–39° latitude
extent; its region coordinates differ from the Earth-centered GUI preset.
Do not treat a run of the global preset against this regional input as a
complete northern-peninsula measurement.

The result covers geographic input → sampled terrain/materials → chunk NBT →
compressed Linear files and project metadata. Vanilla server caves, ores,
vegetation, lighting, and other later chunk-generation stages are outside this
timing. Intel hybrid and Apple Silicon runtime cannot be inferred directly from
these EPYC measurements.

## Reproduce the measured 80-region run

With `EARTHMAP_HEIGHTMAP`, `EARTHMAP_SURFACE_RASTER`, `EARTHMAP_TIF_ROOT`,
`EARTHMAP_DATA_ROOT`, and `EARTHMAP_OUTPUT_ROOT` set to the prepared inputs/output:

```powershell
Remove-Item Env:RAYON_NUM_THREADS -ErrorAction SilentlyContinue
Remove-Item Env:EARTHMAP_CPU_BUDGET -ErrorAction SilentlyContinue
Remove-Item Env:EARTHMAP_HEIGHTMAP_CACHE_ROWS -ErrorAction SilentlyContinue
Remove-Item Env:EARTHMAP_SURFACE_TILE_CACHE_ENTRIES -ErrorAction SilentlyContinue
cargo build --release --locked --manifest-path rust/Cargo.toml -p earthmap-cli --bin earthmap-rs
$earthmapTargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "rust/target" }
$earthmapBinaryName = if ($IsWindows -or $env:OS -eq "Windows_NT") { "earthmap-rs.exe" } else { "earthmap-rs" }
$earthmapReleaseCli = Join-Path $earthmapTargetDir "release/$earthmapBinaryName"
& $earthmapReleaseCli generate $env:EARTHMAP_HEIGHTMAP `
  (Join-Path $env:EARTHMAP_OUTPUT_ROOT "korea-200-fresh-run") `
  200 134 -4 10 8 linear auto surface `
  "surfaceRaster=$env:EARTHMAP_SURFACE_RASTER" verticalScale=auto `
  linearCompression=4 prefetch=false
```

Time only the release executable invocation, excluding the build command. Use a
different fresh output directory for every repeat so resume does not skip existing regions.
The command above uses the current regional input's coordinates; it is not the
global full-peninsula preset command.

[Recorded timings, projection inputs, and complete-file hashes](benchmarks/korean-peninsula-1-200-2026-10-10.json).
