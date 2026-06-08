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
- [x] Resume the full Earth 1:250 Linear generation with about a 25GB prefetch memory cap.
- [x] Sample CPU utilization after resume.
- [x] Identify and optimize the slow pure-ocean fast path that kept CPU usage low after prefetch.
- [x] Test nested region-worker plus column-level Rayon parallelism as a bottleneck hypothesis.
- [x] Share the heightmap reader and row cache across prefetch producers so adjacent regions do not reopen and reread the same rows.
- [x] Cache terrain-token photo evidence on coarse photo cells instead of sampling it once per block column.
- [x] Move prefetch producers to per-producer heightmap/material readers and bounded per-producer caches so prefetch does not serialize on one shared raster sampler.
- [x] Split prefetch surface-sample Rayon work from consumer chunk/NBT Rayon work so prepared samples do not starve region encoding.
- [x] Add prefetch timing telemetry for send wait, ready-queue wait, consumer pool wait, and consumer elapsed time.
- [x] Cache per-region open-ocean longitudes and latitudes so the open-ocean fast path does not recompute map coordinates for every column.
- [x] Expand the thread-local surface sampler L1 cache from one entry to a small direct-mapped cache to reduce global cache-lock traffic during land material sampling.
- [x] Expand the thread-local surface sampler L1 cache again from 64 slots to 4096 slots after long-run land evidence showed cache-lock churn still dominated `surfacePhase.columnBuildMillis`.
- [x] Expand the photo CIEDE calculation cache from 16,384 entries to 262,144 entries after the faster material path exposed `surfacePhase.photoApplyMillis` as the next hot phase.
- [x] Add per-region surface material raster stats to `regionGenerated` telemetry so future samples can separate raster tile misses from material/photo cache CPU work.
- [x] Add thread-local RGB tile and VRT source-reader caches to reduce repeated mutex traffic inside TrueMarble averaged sampling.
- [x] Test and reject widening the prefetch consumer/output Rayon pool: it helped small ocean smoke but badly hurt land/photo regions.
- [x] Re-test `prefetchWorkers=2` after the larger material caches; reject it again for the current land row because it increased land region times and reduced average CPU versus `prefetchWorkers=1`.
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
- [x] Measure whether additional prefetch workers improve throughput before increasing them.
- [x] Keep memory bounded by the user cap and by automatic system-memory safety margins.
- [x] Evict evidence immediately after the owning region is generated.
- [ ] Avoid assuming OS filesystem cache behavior in benchmarks.
- [ ] Test land, coast, and ocean workloads separately because their evidence mix differs.
- [ ] Preserve visual quality, bathymetry, coastline behavior, Linear/MCA compatibility, and resume semantics.
- [x] Smoke test prefetch path on a 1-region Linear run.
- [x] Split pure open-ocean water evidence from coastal/photo water sampling so open ocean keeps bathymetry and ocean temperature without per-cell RGB/terrain-token raster work.
- [x] Skip companion water sampling for deep open-ocean columns when the primary heightmap already supplies trusted bathymetry; keep companion sampling for shallow or unknown depths.

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
- [x] Producer-local prefetch smoke: 1 region, Linear, `prefetchMemoryGB=1`, `prefetchWorkers=2`, `allDone=true`, elapsed `6049ms`, output directory `agent-runs/smoke-prefetch-producerlocal-20260609-062806`.
- [x] Split-pool prefetch smoke: 1 region, Linear, `allDone=true`, telemetry included `prefetchSendWaitMillis=0`, `prefetchReadyQueueWaitMillis=0`, and `consumerPoolWaitMillis=26`.
- [x] Split-pool 4-region smoke used `prefetchWorkers=2`, `prefetchSampleRayonThreads=12`, `prefetchOutputRayonThreads=4`, and showed pool wait near zero; remaining time was dominated by `surfacePhase.columnBuildMillis`.
- [x] Split-pool long-run sample showed `prefetchWorkers=4` is still worse on the current data: CPU fell to about 1.72 average cores with no new completions in the second 60s sample, so the resumed run should use `prefetchWorkers=2`.
- [x] Open-ocean longitude/latitude cache smoke for `r.-64.28`: `allDone=true`, `openOceanFastPathMillis=2318`, `surfaceSampleMillis=3406`; this is smoke evidence only because OS cache can bias single-run timings.
- [x] Thread-local L1 cache smoke for land region `r.-62.28`: `allDone=true`, `columnBuildMillis=6262`, and SHA-256 matched the existing generated region (`44B96C79ACE24EA6C7A4E913B1A806C09114F9D4DC537609A29B4913240338E2`).
- [x] L1-cache long-run sample improved some land regions (`r.-55.28`/`r.-54.28` column build about 24-25s) but land-heavy regions such as `r.-50.28` through `r.-53.28` still spend about 43-46s in `surfacePhase.columnBuildMillis`; the remaining bottleneck is material/photo/evidence sampling, not queue wait or region write.
- [x] Output-pool widening experiment: small ocean smoke used `prefetchSampleRayonThreads=8` and `prefetchOutputRayonThreads=8`; 4 ocean regions completed in `8378ms`, and `r.-36.28` SHA-256 matched the existing generated region (`6F6CE0EFC06D956FCE477EF7C787E1465470D6DD3571BEE8893C4DC0E1C5112C`), but the long-run sample regressed land/photo work (`r.17.28` had `photoApplyMillis=90092`), so the active split was restored to `12 sample / 4 output`.
- [x] L1 4096-slot smoke for land region `r.101.28`: generated output SHA-256 matched the existing full-run region (`A9A9C28BA62A2FD468ECCDBFE87AC69D6C08BC239A8E56F07C9C95102D007C37`), `surfacePhase.columnBuildMillis=4819`, `surfacePhase.photoApplyMillis=9934`, `surfaceMaterialRaster.sampleAveragedRequests=34969`, `tileMisses=4`.
- [x] L1 4096-slot plus CIEDE 262k smoke for land region `r.101.28`: generated output SHA-256 still matched (`A9A9C28BA62A2FD468ECCDBFE87AC69D6C08BC239A8E56F07C9C95102D007C37`), `surfacePhase.columnBuildMillis=4721`, `surfacePhase.photoApplyMillis=6554`, `surfaceSampleMillis=13378`.
- [x] Thread-local RGB tile/VRT reader cache smoke for land region `r.101.28`: output SHA-256 still matched (`A9A9C28BA62A2FD468ECCDBFE87AC69D6C08BC239A8E56F07C9C95102D007C37`), but single-region time was only modestly changed (`surfacePhase.columnBuildMillis=4630`, `surfacePhase.photoApplyMillis=6746`, `surfaceSampleMillis=13469`), so long-run evidence is still required before treating it as a major win.
- [x] Full resume with `prefetchWorkers=1`, 25GB cap, larger material caches: `resumeFingerprintMatched=true`, `r.122.28` generated in `9164ms` with `columnBuildMillis=5343`, while a 60s sample averaged `4.38` CPU cores; this is improved throughput but not the requested all-core utilization.
- [x] Full resume with `prefetchWorkers=2`, 25GB cap, larger material caches: `resumeFingerprintMatched=true`, but land regions `r.129.28` and `r.130.28` took about `29s` each, mixed regions `r.131.28` and `r.132.28` took `37-38s`, and the 60s sample averaged only `3.3` CPU cores, so `prefetchWorkers=2` remains rejected for land-heavy rows.
- [x] Full resume with thread-local RGB tile/VRT reader cache and `prefetchWorkers=1`: `resumeFingerprintMatched=true`; the sampled row was open ocean, averaging `2.83` CPU cores with regions around `3.5-3.7s`, so the next bottleneck is open-ocean/NBT throughput or workload-adaptive scheduling rather than TrueMarble material sampling.
- [x] Startup tuning follow-up: reduced candidate samples to land/mixed/ocean coverage with fewer Rayon candidates.
- [x] Resume sample with `prefetchWorkers=4`: PID 24980 averaged 3.5 CPU cores over 30s, generated no additional completed regions during the sample, and showed pure-ocean `openOceanFastPathMillis` up to 57.7s.
- [x] Open-ocean fast-path smoke after RGB/terrain-token split: region `r.-112.27` dropped from the long-run log's `openOceanFastPathMillis=57724` to `2575`; total one-region smoke completed in 5.6s. This is smoke evidence only because OS cache can bias single-run timings.
- [x] Deep open-ocean companion-skip smoke: region `r.-120.27` dropped from the long-run log's `openOceanFastPathMillis=9968` to `2481`; total one-region smoke completed in 5.4s. This is smoke evidence only because OS cache can bias single-run timings.
- [x] Long-run sample after deep-ocean skip still averaged only 2.06 CPU cores over 30s with `parallelColumnSampling=true`, indicating more profiling was needed before changing parallelism strategy.
- [x] Shared heightmap cache smoke reduced ocean `elevationFillMillis` from about 5.1s to about 1.3s on the same row, but land regions still show `columnBuildMillis` around 61-62s when all columns need photo material sampling.
- [x] Nested-parallelism hypothesis was rejected for land: land region `r.-60.27` completed with `columnBuildMillis=7635` when column sampling used Rayon, versus 61-62s when column sampling was disabled in the long-run fallback.
- [x] Terrain-token cache smoke for land region `r.-60.27`: total one-region smoke completed in 12.3s with `columnBuildMillis=7635`; this is smoke evidence only because OS cache can bias single-run timings.
- [x] `prefetchWorkers=4` with Rayon column sampling caused land producer starvation in long-run smoke; one region reached `columnBuildMillis=163594`, so producer count must stay low when each producer uses the Rayon pool internally.
- [x] `prefetchWorkers=1` avoided starvation but serialized land preparation too much: 30s sample completed 2 regions at about 4.07 average CPU cores.
- [x] Prefetch disabled long-run smoke completed 6 regions in a 30s sample in the current ocean-heavy row; despite about 3.14 average CPU cores, throughput was better than the tested prefetch configurations for this segment.
- [x] Resume check after shared heightmap cache: existing progress was detected as resume, with `regionSkipped=31878` and `resumeFingerprintMatched=true`.
- [x] Mixed/coast bottleneck found from long-run evidence: region `r.15.27` took `320740ms`, with `surfacePhase.columnBuildMillis=309378`, `sampledMaterialColumns=262144`, `sampledWaterMaterialColumns=186344`, and only ~65% CPU during the sample.
- [x] Optimize mixed/coast water material sampling so full-world progress does not stall on 262,144 per-column heavy water material samples per region.
- [x] Re-run `r.15.27` after the material-sampling fix: elapsed `4687ms`, `columnBuildMillis=2119`, `sampledMaterialColumns=262144`, `sampledWaterMaterialColumns=186344`.
- [x] Confirm `r.15.27` output quality was not changed by the light water companion path: SHA-256 matched the prior generated region (`BBADB6E8DC48EFF3B019A435E8B83E2943B89E06C8D017A1F61CD7AA2D9E6E15`).
- [x] Startup auto-tuning stall found: full Earth resume spent more than 2 minutes in `workerTuningStarted` before normal batch progress logs, because it benchmarked serial-column candidates and 6/8 intermediate worker candidates by generating real regions.
- [x] Reduce startup worker tuning to a short land+mixed comparison between legacy 4 workers and requested workers, using column-parallel candidates only.
- [x] Full resume after tuning reduction used only two candidates: 4 workers at `14788ms/region` and 10 workers at `12847.5ms/region`; selected 10 workers, `rayonThreads=16`, `parallelColumnSampling=true`.
- [x] Reject the 10-worker tuning result as invalid for this architecture: with only 2 tune samples, the 10-worker candidate only exercised 2 active workers, while the real full run started 10 nested-Rayon region tasks and produced no completed region after more than 90 seconds.
- [x] Single-region control for `r.17.27` completed in `8136ms` with `columnBuildMillis=5161`, proving the no-completion full run was concurrency starvation rather than an inherently slow region.
- [x] Move no-prefetch generation to per-worker heightmap and surface material readers/caches so worker threads do not contend on one shared sampler.
- [x] Cap surface photo region workers at the legacy 4-worker limit while column-parallel sampling uses the Rayon pool internally.
- [x] Check 1 worker versus 4 worker land throughput: 1 worker / 16 Rayon generated 4 land regions in `69200ms` (~208 regions/hour), while the 4-worker full run completed comparable land regions in about `45s` (~320 regions/hour), so the 4-worker cap remains the better current default.
- [x] For the current scattered resume holes, restart with `workerThreads=1` and `RAYON_NUM_THREADS=16`: regions `r.20.27`, `r.21.27`, and `r.22.27` completed in `8489ms`, `14420ms`, and `14958ms`; this avoids the far-apart tile-cache thrash seen when 4 workers started isolated holes concurrently.
- [ ] Design a locality-aware resume scheduler so incomplete regions are grouped by nearby runs instead of starting far-apart holes concurrently.
- [x] Add a conservative uniform deep-open-ocean surface sample path for regions whose smoothed source elevations are all deep enough to clamp to the world floor and whose water biome is constant.
- [x] Verify the uniform deep-open-ocean unit test passes; real `r.88.27` and `r.-120.27` did not trigger the conservative guard, so their remaining ocean cost is still in the normal open-ocean path.
