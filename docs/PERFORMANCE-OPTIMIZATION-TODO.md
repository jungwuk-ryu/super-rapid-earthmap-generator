# Performance Optimization TODO

## Scope

- [x] Pause the currently running full-Earth generation before changing code.
- [x] Optimize region generation without reducing Minecraft, Linear/MCA, coastline, bathymetry, or visual quality.
- [x] Use measured profiling data for the ocean bottleneck before implementing the fast path.
- [x] Add startup auto-tuning for mixed land/ocean workloads across different machines.
- [x] Improve repeated smoothing/relief work where profiling or code inspection shows redundant computation.
- [x] Add per-region phase telemetry so long runs expose bottlenecks before the batch finishes.
- [x] Do not implement resume skip/validation optimization in this task.

## Guardrails

- [x] Keep output quality gates intact.
- [x] Do not use byte-for-byte Java parity as the optimization gate.
- [x] Do not change resume journal semantics.
- [x] Keep memory bounded and adaptive to the current machine.
- [x] Avoid noisy tuning results by benchmarking representative land and ocean samples.
- [x] Avoid OS-cache order bias in worker tuning and benchmarks.
- [x] Keep CLI generation usable without GUI involvement.

## 1. Startup Auto-Tuning

- [x] Find current thread/worker cap logic.
- [x] Design tuning samples for ocean and land regions.
- [x] Avoid tuning on pathological samples only.
- [x] Record candidate worker counts and timing noise.
- [x] Select worker count using a conservative noise band.
- [x] Confirm higher-worker wins with reversed-order retest before selecting them.
- [x] Keep a deterministic fallback if tuning cannot run.
- [x] Persist or report tuning result in progress output.
- [x] Add tests for tuner selection behavior.

## 2. Ocean Bottleneck Profiling And Optimization

- [x] Add or use phase timing to identify ocean-region cost.
- [x] Profile representative ocean region generation.
- [x] Confirm whether cost is sampling, chunk building, encoding, or writing.
- [x] Design a guarded open-ocean fast path.
- [x] Apply the fast path only where land/coastline quality cannot be affected.
- [x] Preserve bathymetry-driven water depth.
- [x] Add tests for fast-path eligibility.
- [x] Add tests that coast/land-adjacent regions are not fast-pathed.

## 3. Smoothing And Relief Optimization

- [x] Locate repeated neighborhood scans.
- [x] Replace repeated work with region-local precomputed arrays where safe.
- [x] Preserve existing numerical behavior or document intentional tolerance.
- [x] Add focused tests for smoothed elevation and local relief behavior.
- [x] Verify no regression in representative surface generation.

## 4. Phase Telemetry

- [x] Add per-region timing fields to progress events.
- [x] Include surface sampling time.
- [x] Include chunk build time.
- [x] Include NBT encode/write time where available.
- [x] Include selected worker count and tuning metadata at batch start.
- [x] Keep existing progress output backwards-compatible.
- [x] Add tests or snapshots for new progress fields where feasible.

## Verification

- [x] `cargo fmt --all --manifest-path rust/Cargo.toml`
- [x] Targeted Rust tests for changed crates.
- [x] Full relevant Rust workspace tests if shared generation behavior changes.
- [x] Smoke benchmark on representative ocean and land samples.
- [x] Commit as an atomic Conventional Commit.

## Notes

- Active generation process was suspended, not terminated, before this document was created.
- Resume skip validation optimization is intentionally excluded by user request.
- Baseline on real 1:250 data before optimization:
  - Ocean sample `regionX=-100, regionZ=20`: elapsed 5.78s, surface sampling 4.29s, chunk build aggregate 0.28s, NBT encode aggregate 3.76s, write 0.08s.
  - Land sample `regionX=5, regionZ=5`: elapsed 9.59s, surface sampling 8.06s, chunk build aggregate 0.28s, NBT encode aggregate 3.83s, write 0.07s.
  - Existing region worker cap is 4 even when CLI requests 10 threads.
- Single-region old/new timing comparisons are smoke evidence only unless run in cross-over order because OS filesystem cache can make the second run look faster.
- Event smoke after optimization confirmed:
  - `workerTuningStarted` and `workerTuningFinished` are emitted before `batchStarted`.
  - `regionGenerated` includes surface/chunk/NBT/write phase timings.
  - 8-region auto-tune smoke selected 4 workers over 6 workers for the tested Sahara sample set.
- `cargo test --manifest-path rust/Cargo.toml --workspace --locked` passed.

## Active Follow-Up: Crash And Sustained CPU Utilization

### Scope

- [x] Treat `process finished with code -1073741819` as a native crash, not an agent-initiated stop.
- [x] Record Windows Error Reporting evidence for the crash.
- [x] Map the crash fault offset to a function or collect a reproducible dump.
- [ ] Determine whether the crash is in Rust code, raster/native dependency code, allocator code, or process shutdown.
- [x] Fix or avoid the most likely native allocator crash path by making CLI `mimalloc` opt-in.
- [x] Implement bounded prefetch/evidence preparation for full-region generation.
- [x] Honor a user-configurable memory cap for prefetch/evidence buffering.
- [ ] Resume the full Earth 1:250 Linear generation with about a 25GB prefetch memory cap.
- [ ] Sample CPU utilization after resume.
- [ ] Iterate until `earthmap-rs` sustains high CPU utilization across the long generation path, not only during short region-start bursts.
- [x] Reduce startup worker tuning cost so long runs do not spend many minutes generating benchmark regions before the real batch.

### Current Crash Evidence

- [x] Crashed process: `D:\earthmap\super-rapid-earthmap-generator\rust\target-latest\release\earthmap-rs.exe`
- [x] Exit code: `-1073741819` (`0xC0000005`, access violation)
- [x] Runtime before crash: `1336.379s`
- [x] WER event type: `APPCRASH`
- [x] Faulting module: `earthmap-rs.exe`
- [x] Fault offset: `0x0000000000322de3`
- [x] Symbolized function: `earthmap_surface::sanitize_surface_column_for_production`
- [x] WER archive: `C:\ProgramData\Microsoft\Windows\WER\ReportArchive\AppCrash_earthmap-rs.exe_e0d3b81434ca9f1ce1ea57a6e01b41381e37bcfa_5a4a99de_c63f15f4-4db7-4228-9ceb-1f9fd480a53a`
- [x] Check whether the archive contains a usable dump or only `Report.wer`.
- [x] If no dump exists, enable a local dump or add targeted diagnostics before the next long run.
- [x] Feature-gate the CLI `mimalloc` global allocator so the default binary avoids that native crash candidate.
- [ ] Confirm the resumed long run no longer hits `0xC0000005`.
- [x] Local dump target: `D:\earthmap\super-rapid-earthmap-generator\agent-runs\crash-dumps`

### Prefetch/Evidence Queue Design Checklist

- [x] Identify the exact synchronous input-preparation path that leaves region workers idle.
- [x] Separate "evidence preparation" from "chunk/region writing" where the API allows it.
- [x] Add a bounded producer/consumer queue so prepared work is ready before workers need it.
- [x] Default to one prefetch worker and keep queue depth bounded by memory and worker count.
- [ ] Measure whether additional prefetch workers improve throughput before increasing them.
- [x] Keep memory bounded by the user cap and by automatic system-memory safety margins.
- [x] Evict evidence immediately after the owning region is generated.
- [ ] Avoid assuming OS filesystem cache behavior in benchmarks.
- [ ] Test land, coast, and ocean workloads separately because their evidence mix differs.
- [ ] Preserve visual quality, bathymetry, coastline behavior, Linear/MCA compatibility, and resume semantics.
- [x] Smoke test prefetch path on a 1-region Linear run.

### Restart Command

- [ ] Stop or confirm inactive any stale `earthmap-rs.exe` process targeting the same output directory before restart.
- [ ] Restart only after crash diagnostics and prefetch changes are verified.
- [ ] Use command:

```powershell
earthmap-rs generate-vanilla-delegated-regions-parallel C:\earth_map_resources\HQheightmap.tif D:\earthmap\1-250-earth-linear 250 -157 -74 314 148 linear 8 surface surfaceRaster=D:\earthmap\TifFiles\terrain\TrueMarble.vrt verticalScale=auto linearCompression=6
```

- [ ] Set prefetch/evidence memory cap to about `25GB`.
- [ ] Confirm resume mode uses existing `D:\earthmap\1-250-earth-linear` progress instead of starting fresh.
- [ ] Record sustained CPU usage, read throughput, write throughput, completed regions/hour, and peak memory.

### Verification Notes

- [x] `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli prefetch --locked`
- [x] `cargo test --manifest-path rust\Cargo.toml -p earthmap-surface --lib --locked`
- [x] `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked`
- [x] Release build: `cargo build --manifest-path rust\Cargo.toml -p earthmap-cli --release --target-dir rust\target-latest --locked`
- [x] Prefetch smoke: 1 region, Linear, `prefetchMemoryGB=1`, `allDone=true`
- [x] Startup tuning follow-up: reduced candidate samples to land/mixed/ocean coverage with fewer Rayon candidates.
