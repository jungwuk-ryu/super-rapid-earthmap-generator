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
- [x] Add an open-ocean direct classifier for columns that do not need companion material, bypassing the generic classify/cleanup/sanitize chain while preserving the same final column.
- [x] Test and reject a Float32 row thread-local cache: it preserved output hashes but slowed or failed to improve real land rows because row-wide reads do not match the current sparse height/depth sampling pattern.
- [x] Add a region-local open-ocean companion material precompute so bathymetry/ocean-temperature samples are loaded once per material cell instead of routed through per-column global cache locks.
- [x] Add a region-local photo-land material precompute for EarthData photo mode so land color/evidence/terrain-token cells are resolved once per region before per-column application.
- [x] Parallelize the region-local photo-land material precompute across unique photo/evidence cells so raster/evidence reads no longer run as one serial land producer step.
- [x] Test and reject widening the prefetch consumer/output Rayon pool: it helped small ocean smoke but badly hurt land/photo regions.
- [x] Re-test `prefetchWorkers=2` after the larger material caches; reject it again for the current land row because it increased land region times and reduced average CPU versus `prefetchWorkers=1`.
- [x] Rebalance prefetch split from `12 sample / 4 output` to `13 sample / 3 output` for 16-thread photo generation, because current land rows are sample-bound while pure-ocean rows still need enough output/NBT capacity.
- [x] Split broad `surfacePhase.columnBuildMillis` telemetry into coordinate and photo-land precompute subfields while keeping the existing field backwards-compatible.
- [ ] Use the new subphase telemetry to decide whether the next optimization should target photo-land precompute, column material application, open-ocean elevation/fill, or consumer/NBT output.
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
- [x] Code audit after the crash question: `earthmap-surface` forbids `unsafe_code`, and `sanitize_surface_column_for_production` plus `replace_surface_blocks` only clone/construct safe Rust values. Treat the symbol as the observed fault site, not proof that this pure function directly caused memory corruption.
- [ ] If `0xC0000005` recurs on the default non-`mimalloc` CLI binary, collect the local dump and separate Rust logic from native dependency/process-shutdown fault candidates before further performance tuning.

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
- [x] Clarify the prefetch model: the queue is a bounded `sync_channel` of prepared regions, not a raw byte buffer. It does free a slot after the owning region is consumed, but it cannot eliminate GDAL/GeoTIFF decode, row/tile cache locking, surface material calculation, or output serialization costs.
- [x] Clarify disk interpretation: low disk MB/s does not rule out tiny random-read or `Mutex<File>` latency in raster readers, but the latest telemetry does not support raw disk bandwidth as the dominant remaining bottleneck. Continue separating read/decode/lock waits from surface and consumer CPU work with per-region telemetry.

### Restart Command

- [x] Stop or confirm inactive any stale `earthmap-rs.exe` process targeting the same output directory before restart.
- [x] Restart only after crash diagnostics and prefetch changes are verified.
- [ ] Use command:

```powershell
earthmap-rs generate-vanilla-delegated-regions-parallel C:\earth_map_resources\HQheightmap.tif D:\earthmap\1-250-earth-linear 250 -157 -74 314 148 linear 8 surface surfaceRaster=D:\earthmap\TifFiles\terrain\TrueMarble.vrt verticalScale=auto linearCompression=6
```

- [x] Set prefetch/evidence memory cap to about `25GB`.
- [x] Confirm resume mode uses existing `D:\earthmap\1-250-earth-linear` progress instead of starting fresh.
- [x] Record sustained CPU usage, read throughput, write throughput, completed regions/hour, and peak memory.

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
- [x] Open-ocean direct classifier smoke for `r.150.28`: output SHA-256 matched the existing generated region (`332D696C845D202AF98C1B47C69F7382E0E2B7F8B136DFCEB0784EACDB4A267C`), `openOceanFastPathMillis` dropped to `949`, `surfaceSampleMillis=2016`, and `elapsedMillis=3201`.
- [x] Float32 row-cache experiment was rejected: one-region `r.-59.29` preserved SHA-256 (`739E5D922367F7B78C8133BCA66657CAD8E6BAE592301048EE74740F6FA71486`) but long-run land samples still averaged only about 4.44 CPU cores and did not beat the prior `prefetchWorkers=1` row, so the code path was removed instead of committed.
- [x] Region-local open-ocean companion precompute smoke for `r.68.29`: output SHA-256 matched the existing generated region (`A1752580D3E8F1D285168C4673364EF802D92056195C696BA24E312FF668DE6B`), while `openOceanFastPathMillis` dropped from about `2358ms` to `761ms` and `elapsedMillis` dropped from about `3725ms` to `2146ms`.
- [x] Region-local photo-land precompute smoke for `r.101.29`: output SHA-256 matched the existing generated region (`76BB577E2F7A168FBB7E6B87291DF28E3BE820721CED16D664BE45EE1B9BEB5C`); full-resume `prefetchWorkers=1` sample then averaged about `4.97` CPU cores and land regions commonly completed in about `10-13s`.
- [x] Re-tested `prefetchWorkers=2` after photo-land precompute: ocean-heavy row averaged about `5.22` CPU cores with pure ocean regions around `1.9-3.3s`, but land-heavy regions later regressed to about `36s` with `columnBuildMillis` around `28s`, so worker count must stay at `1` for fixed full-run resume until workload-aware scheduling exists.
- [x] Parallel photo-land precompute smoke for `r.-58.30`: output SHA-256 matched the existing generated region (`C03277EF7E5AD4BD8FA907D7E5592B919B839BB38AA3B6DCB60A1112DEC399EF`), and one-region `columnBuildMillis` was `4824ms` with `elapsedMillis=8720`.
- [x] Rejected a workload gate experiment for `prefetchWorkers=2`: gating non-ocean prefetch/output stages did not fix land contention, because `surfacePhase.columnBuildMillis` is produced during prepared-sample generation rather than consumer output. Same-region warm smoke for `r.122.30` through `r.125.30` still hit `50285ms` elapsed and `34220ms` column build at `prefetchWorkers=2`, while `prefetchWorkers=1` completed the same land set in `9629-12297ms` per region. The code was removed instead of committed.
- [x] Rejected producer-partitioned `prefetchWorkers=2` and ocean-lookahead scheduling experiments: partitioned sample pools improved some `w2` tail cases but still regressed land (`r.-60.31` reached `29746-52079ms` column build in tested variants) and did not improve ocean wall time. The experimental scheduler code was removed instead of committed.
- [x] Accepted balanced sample-heavy split smoke: final `prefetchWorkers=1`, `13 sample / 3 output` land smoke for `r.-63.31` through `r.-60.31` completed in `11697-14729ms` per region with no long tail; final ocean smoke for `r.-77.31` through `r.-74.31` returned to about `1.9-2.7s` per pure-ocean region.
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
- [x] Stable full resume after rejecting the prefetch gate: PID `16100`, log `agent-runs/full-earth-resume-stable-25w1-20260609-101805`, `prefetchMemoryGB=25`, `prefetchWorkers=1`, `resumeFingerprintMatched=true`; region `r.127.30` regenerated after the earlier crash point in `11053ms`, and a 30s sample averaged `3.81` CPU cores with about `2.06GB` private memory. Pure-ocean regions in that sample completed around `1.94-2.05s`; this confirms resume/crash recovery but still does not satisfy the sustained high-CPU target.
- [x] Full resume with the accepted `13 sample / 3 output` split: PID `11132`, log `agent-runs/full-earth-resume-sample13-output3-25w1-20260609-113716`, `resumeFingerprintMatched=true`, `prefetchWorkers=1`, `prefetchMemoryGB=25`; pure-ocean regions completed around `2.23-2.79s` with `consumerElapsedMillis` near `1.0-1.5s`, but the 60s sample still averaged only `3.74` CPU cores. Output/NBT is no longer the dominant ocean bottleneck; the next bottleneck is one-at-a-time ocean surface-sample preparation.
- [x] Re-tested current simple `prefetchWorkers=2` and `prefetch=false` after the `13/3` split: `prefetchWorkers=2` still regressed both pure ocean and land because concurrent producers fight over the same nested Rayon/sample path, while no-prefetch 4-worker ocean smoke completed the same 4 pure-ocean regions in `5.8-7.6s` each. Keep the full resume on `prefetchWorkers=1` until the open-ocean sample path itself is faster or a safer region scheduler exists.
- [ ] Design a locality-aware resume scheduler so incomplete regions are grouped by nearby runs instead of starting far-apart holes concurrently.
- [x] Add a conservative uniform deep-open-ocean surface sample path for regions whose smoothed source elevations are all deep enough to clamp to the world floor and whose water biome is constant.
- [x] Verify the uniform deep-open-ocean unit test passes; real `r.88.27` and `r.-120.27` did not trigger the conservative guard, so their remaining ocean cost is still in the normal open-ocean path.
- [x] Added subphase telemetry and verified it in release smoke runs:
  - Land smoke `agent-runs/smoke-phase-telemetry-land--63-31-20260609-120756`: `r.-63.31` elapsed `12522ms`, `surfaceSampleMillis=10370`, `elevationFillMillis=931`, `photoLandPrecomputeMillis=2421`, broad `columnBuildMillis=6330`, `photoApplyMillis=1922`, `postProcessMillis=1030`, `consumerElapsedMillis=2151`.
  - Ocean smoke `agent-runs/smoke-phase-telemetry-ocean-107-31-20260609-120833`: `r.107.31` elapsed `3760ms`, `surfaceSampleMillis=1788`, `elevationFillMillis=919`, `openOceanFastPathMillis=752`, `consumerElapsedMillis=1971`.
- [x] Rejected parallelizing `precompute_open_ocean_companion_materials`: the smoke output for `r.107.31` matched SHA-256 (`8EB77E3E712DCF6D946C62B34A6EBE2D942021598FE6BEE585755AD488D07EC1`), but `openOceanFastPathMillis` regressed to `1966ms` and elapsed time to `5148ms`, likely from companion raster/cache lock contention. The code was reverted.
- [x] Reduced photo-land material duplication by sharing precomputed photo samples with `Arc` and deduplicating color/evidence sample pairs before expanding to columns.
- [x] `Arc` photo sample smoke for `r.-62.32`: SHA-256 matched the existing generated region (`0FD7D4CF78E5AB7620FD38CDC971AC39B500AA2BD298031DC5379CD200EA261A`), elapsed `7400ms`, `surfaceSampleMillis=5545`, `photoLandPrecomputeMillis=1164`, broad `columnBuildMillis=3081`.
- [x] `Arc` photo sample 4-region land smoke `agent-runs/smoke-arc-photo-sample-4r-land--62-32-20260609-123143`: regions `r.-62.32` through `r.-59.32` all matched existing SHA-256 outputs; elapsed times were `6911`, `8306`, `10029`, and `11205ms`.
- [x] Added an open-ocean-only prefetch output pool so pure open-ocean regions can use 6 output threads while land/photo regions keep the safer 3-thread output pool.
- [x] Open-ocean output pool smoke `agent-runs/smoke-ocean-output-pool-8r-100-32-20260609-124705`: 8 pure-ocean regions used `13 sample / 3 land-output / 6 open-ocean-output` threads; average `consumerElapsedMillis` dropped from about `970.6ms` to `578.6ms`, with average elapsed `1748ms`.
- [x] Land regression check `agent-runs/smoke-ocean-output-pool-land-4r--62-32-20260609-124742`: regions `r.-62.32` through `r.-59.32` all matched existing SHA-256 outputs.
- [x] Added a probe-then-parallel uniform open-ocean verification path so uniform ocean candidates do not spend the whole 512x512 signature check serially.
- [x] Uniform ocean probe smoke `agent-runs/smoke-uniform-ocean-probe-8r--90-33-20260609-125912`: 8 pure-ocean regions `r.-90.33` through `r.-83.33` all matched existing SHA-256 outputs; average `openOceanFastPathMillis=851.4`, average elapsed `1752.4ms`.
- [x] Stopped active full resume PID `30604` before additional code changes; it had about `2.18GB` private memory and was writing to `agent-runs/full-earth-resume-uniform-ocean-25w1-20260609-130101`.
- [x] Latest stopped-run evidence: `resumeFingerprintMatched=true`, `prefetchWorkers=1`, `prefetchMemoryGB=25`, `prefetchSampleRayonThreads=13`, `prefetchOutputRayonThreads=3`, `prefetchOpenOceanOutputRayonThreads=6`, `regionGenerated` count `188`.
- [x] Latest pure-ocean evidence from the last 120 generated regions: average `elapsedMillis=1836.66`, `surfaceSampleMillis=1259.53`, `openOceanFastPathMillis=1016.11`, `consumerElapsedMillis=576.35`; CPU still averaged only about `3.8` cores, so the remaining ocean bottleneck is inside one-at-a-time open-ocean surface sample preparation rather than raw disk bandwidth.
- [x] Latest land evidence from non-ocean regions in the stopped run: average `elapsedMillis=8958.12`, `surfaceSampleMillis=8180`, `photoLandPrecomputeMillis=1144.53`, broad `columnBuildMillis=5357.24`, `photoApplyMillis=1640.35`, `consumerElapsedMillis=777.29`; land is still sample-bound but no longer shows the previous 60-300s pathological stalls.
- [x] Split open-ocean fast-path telemetry further so uniform verification, companion material precompute, per-column classification, and repeated-column expansion can be separated.
- [x] Open-ocean subphase smoke before the companion cache bypass: `agent-runs/smoke-open-ocean-subphase-100-33-20260609-131854`, `r.100.33`, SHA-256 matched existing output, `openOceanFastPathMillis=767`, `openOceanCompanionPrecomputeMillis=704`, `openOceanColumnBuildMillis=54`.
- [x] Added an EarthData-only material-cell path that bypasses the global open-ocean sample cache during region-local companion precompute; this keeps the same cell center and output while avoiding a redundant cache lock layer.
- [x] Open-ocean companion cache-bypass smoke: `agent-runs/smoke-open-ocean-cell-uncached-100-33-20260609-132619`, SHA-256 matched existing output, `openOceanCompanionPrecomputeMillis=663`, `openOceanColumnBuildMillis=55`.
- [x] Rejected parallel unique-cell companion sampling: `agent-runs/smoke-open-ocean-unique-cell-par-100-33-20260609-132859` matched SHA-256 but regressed `openOceanCompanionPrecomputeMillis` to `1511`, likely because bathymetry/ocean raster readers do not scale under parallel cell sampling.
- [x] Replaced per-column open-ocean companion sample clones with an indexed region-local sample table: `agent-runs/smoke-open-ocean-indexed-companion-100-33-20260609-133535`, SHA-256 matched existing output, `openOceanCompanionPrecomputeMillis=590`, `openOceanColumnBuildMillis=56`.
- [x] 8-region prefetch smoke after indexed companion table: `agent-runs/smoke-open-ocean-indexed-companion-8r-100-33-20260609-133739`, regions `r.100.33` through `r.107.33`, all SHA-256 outputs matched existing full-run regions; average `elapsedMillis=1636.62`, `surfaceSampleMillis=1063.12`, `openOceanFastPathMillis=744.88`, `openOceanCompanionPrecomputeMillis=686.88`, `consumerElapsedMillis=572.50`.
- [x] Verification after the open-ocean telemetry/indexed companion change:
  - `cargo test --manifest-path rust\Cargo.toml -p earthmap-surface --lib --locked`
  - `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked`
  - `cargo build --manifest-path rust\Cargo.toml -p earthmap-cli --release --target-dir rust\target-latest --locked`
- [x] Full resume after indexed companion table: PID `20936`, log `agent-runs/full-earth-resume-open-ocean-companion-25w1-20260609-133932`, `resumeFingerprintMatched=true`, 25GB cap, `prefetchWorkers=1`; after initial resume work, a 60s sample averaged `3.7` CPU cores. Latest pure-ocean regions completed around `1.56-1.68s` with `openOceanCompanionPrecomputeMillis` commonly `0.75-0.86s` and `consumerElapsedMillis` about `0.55s`.
- [x] Stop PID `20936` before additional code changes; this run confirmed the remaining pure-ocean CPU plateau is not output/NBT but single-producer companion precompute.
- [x] Rejected an ocean-only assist producer scheduler experiment: 8-region smoke `agent-runs/smoke-open-ocean-assist-8r-100-33-20260609-135111` matched SHA-256 but only slightly changed wall time, 32-region smoke `agent-runs/smoke-open-ocean-assist-32r-100-33-20260609-135159` averaged `3.9` CPU cores while crossing mixed/land regions, and full resume `agent-runs/full-earth-resume-ocean-assist-25w1-20260609-135346` averaged only `3.51` CPU cores. The code was removed instead of committed.
- [x] Found the deeper open-ocean companion bottleneck: `GeoTiffFloat32Reader::sample_bilinear` read four 4-byte pixels under a file mutex for every bathymetry sample, so companion precompute could create tens of thousands of tiny random reads while disk MB/s still looked low.
- [x] Added `GeoTiffFloat32RowCache` and used it only inside EarthData open-ocean companion precompute, preserving the same bilinear/no-data behavior while reusing bathymetry rows during a region-local precompute.
- [x] Float32 row-cache smoke `agent-runs/smoke-open-ocean-f32rowcache-100-33-20260609-140940`: `r.100.33` SHA-256 matched existing output; `openOceanCompanionPrecomputeMillis` dropped to `141`, `openOceanFastPathMillis=200`, `surfaceSampleMillis=1236`.
- [x] Float32 row-cache 8-region smoke `agent-runs/smoke-open-ocean-f32rowcache-8r-100-33-20260609-141010`: all SHA-256 outputs matched; wall `7.285s`; average `surfaceSampleMillis=588.25`, `openOceanFastPathMillis=260.12`, `openOceanCompanionPrecomputeMillis=180.88`. Consumer/output became the dominant pure-ocean cost in this smoke.
- [x] Full resume after Float32 row cache: PID `1532`, log `agent-runs/full-earth-resume-f32rowcache-25w1-20260609-141057`, 25GB cap, `resumeFingerprintMatched=true`; sampled run averaged `5.81` CPU cores while entering a land-heavy row. Generated land regions showed `surfaceSampleMillis` around `6.65-9.24s`, so the next bottleneck after ocean is land/photo surface sampling.
- [x] Verification after Float32 row cache:
  - `cargo test --manifest-path rust\Cargo.toml -p earthmap-geo --lib --locked`
  - `cargo test --manifest-path rust\Cargo.toml -p earthmap-surface --lib --locked`
  - `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked`
- [x] Found the next pure-ocean bottleneck after row-cache: full resume `agent-runs/full-earth-resume-f32rowcache-postcommit-25w1-20260609-142127` showed recent 100 all-water regions averaging `surfaceSampleMillis=556.7`, `consumerElapsedMillis=2374.04`, and `prefetchReadyQueueWaitMillis=3634.64`; disk writes averaged only `82.04ms`.
- [x] Increased the open-ocean output Rayon pool only for all-water open-ocean prepared samples, leaving mixed/land regions on the conservative output pool to avoid stealing CPU from photo sampling.
- [x] Open-ocean output-pool smoke `agent-runs/smoke-open-ocean-outputpool-8r-100-33-20260609-142958`: 8/8 SHA-256 outputs matched existing world; `prefetchOpenOceanOutputRayonThreads=12`; average `consumerElapsedMillis=435.88`, `prefetchReadyQueueWaitMillis=1.38`, `surfaceSampleMillis=633.38`, wall `7.049s`.
- [x] Full resume after output-pool widening: PID `37964`, log `agent-runs/full-earth-resume-outputpool-25w1-20260609-143251`, 25GB cap, `resumeJournalRegions=34319`, `prefetchOpenOceanOutputRayonThreads=12`. A 60s sample averaged `4.8` CPU cores while crossing land-heavy regions; recent land regions showed `surfaceSampleMillis` around `6.46-10.12s`, dominated by `columnBuildMillis`, `photoApplyMillis`, `photoLandPrecomputeMillis`, and `postProcessMillis`.
- [x] Later sample of the same full resume averaged `6.34` CPU cores, still below the sustained 80% target. Recent mixed row had mostly ocean regions around `0.95-1.04s`, then land-heavy `r.-65.36..r.-62.36` climbed to `7.53-14.47s`; land-heavy regions were dominated by broad `columnBuildMillis`.
- [x] Re-tested land-heavy `prefetchWorkers=2` after the latest output scheduling changes using interleaved order to reduce OS-cache bias: `w1a=31.03s`, `w2a=68.27s`, `w2b=68.21s`, `w1b=29.73s` for `r.-65.36..r.-62.36`, all with 4/4 SHA-256 matches. Keep `prefetchWorkers=1` for the current workload.
- [x] Avoided one hot-path clone layer by letting chunk generation borrow region columns directly instead of building a cloned `SurfaceChunkSample` per chunk, and changed photo apply to share `SurfaceMaterialSample` through `Arc` instead of cloning the full sample. Smoke `agent-runs/smoke-land-photo-arc-chunkrefs--65-36-20260609-145439`: 4/4 SHA-256 matches, wall `29.487s`, average `elapsedMillis=8042.5`, `surfaceSampleMillis=6751.75`, `consumerElapsedMillis=1290.25`.
- [x] Re-tested pure-ocean `prefetchWorkers=2` after clone reduction and output scheduling changes: interleaved `w1a=6.60s`, `w2a=6.51s`, `w2b=6.54s`, `w1b=6.51s` for `r.100.33..r.107.33`, all with 8/8 SHA-256 matches. Wall time did not improve, and per-region elapsed/surface timings worsened under `prefetchWorkers=2`, so do not raise the default.
- [x] Added optional detail telemetry gated by `EARTHMAP_SURFACE_PHASE_DETAIL=1`: `surfacePhase.columnLoopMillis`, `surfacePhase.columnClassifyMillis`, and `surfacePhase.columnSemanticApplyMillis`.
- [x] Detail smoke `agent-runs/smoke-land-column-detail--65-36-20260609-150732`: 4/4 SHA-256 matches; because semantic/classify values are summed across parallel columns, `columnSemanticApplyMillis` can exceed wall time. Average `columnLoopMillis=4607.25`, `columnClassifyMillis=203.5`, `columnSemanticApplyMillis=53836.5`, showing land `columnBuild` is dominated by semantic material application CPU work.
- [x] Re-tested Rayon cap on this 6-core/12-logical CPU: default `rayonThreads=16` remained faster than `RAYON_NUM_THREADS=12` for land-heavy `r.-65.36..r.-62.36` (`d16a=34.99s`, `r12a=41.83s`, `r12b=37.00s`, `d16b=32.62s`, all 4/4 SHA-256 matches). Keep the current default split.
- [ ] Design a safe ocean-only concurrency path so pure open-ocean companion precompute can overlap across regions without reintroducing the land/photo starvation seen with simple `prefetchWorkers=2`.
- [ ] Reduce open-ocean `EarthSurfaceColumn` allocation/clone overhead without changing bathymetry, biome, or output hashes.
- [x] Test whether avoiding per-chunk column clone copies in `generate_surface_region_with_prepared_sample_inner` improves consumer time without changing output.
- [ ] Re-run representative land and ocean smoke tests after the next open-ocean data-structure optimization.
- [x] Resume the 1:250 full Earth Linear run again with the requested 25GB prefetch cap: PID `29728`, log `agent-runs/full-earth-resume-answercrash-25w1-20260609-152749`, `prefetchWorkers=1`, `prefetchMemoryGB=25`; early log shows `validResume` region skips against `D:\earthmap\1-250-earth-linear`.
- [x] First 30s sample of PID `29728`: average `7.66` CPU cores, private memory about `2.07GB`, `regionGenerated=90`, `regionSkipped=34952`. Latest pure-ocean regions `r.56.37` through `r.67.37` completed in about `0.95-1.12s` each with `prefetchReadyQueueWaitMillis` around `1-2ms`.
- [x] Second 30s sample of PID `29728`: average `7.41` CPU cores, `regionGenerated=308`, `regionSkipped=34952`; latest 120 generated regions were 109 ocean, 10 mixed, 1 land. Ocean averaged `elapsedMillis=1103.39`, `surfaceSampleMillis=661.61`, `consumerElapsedMillis=438.85`; mixed/land slowed on `surfacePhase.columnBuildMillis`.
- [x] Stopped PID `29728` before queue-depth code changes and land transition profiling.
- [x] Land detail smoke for `r.-62.38` matched the existing SHA-256 output and showed the same region can complete in `elapsedMillis=12185`, `surfaceSampleMillis=11042`, with detail telemetry enabled; this made full-run land transition interference more likely than an inherently 20s single-region cost.
- [x] Queue-depth transition smoke for `r.-72.38..r.-62.38`: default deep queue wall `52.693s`, `prefetchRegions=2` wall `47.003s`, `prefetchRegions=1` wall `46.159s`, all 11/11 SHA-256 outputs matched. Deep prepared-region queues can let ocean output backlog overlap and slow the next land sample.
- [x] Queue-depth ocean smoke for `r.100.33..r.107.33`: default, `prefetchRegions=2`, and `prefetchRegions=1` all stayed around `6.4-6.5s` wall with 8/8 SHA-256 matches, so a shallow default queue does not hurt pure-ocean throughput on this sample.
- [x] Changed the default prefetch queue depth from `worker_count * 2` to `prefetchWorkers`, preserving explicit `prefetchRegions=N` overrides.
- [x] Verification after queue-depth change:
  - `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`
  - `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked`
  - Release build: `cargo build --manifest-path rust\Cargo.toml -p earthmap-cli --release --target-dir rust\target-latest --locked`
  - Default transition smoke `agent-runs/smoke-transition-default-after-qdepth-x-72-z38-11r-20260609-154616`: `prefetchQueueRegions=1`, all outputs matched SHA-256, wall `56.589s` in a noisy run.
  - Default ocean smoke `agent-runs/smoke-ocean-default-after-qdepth-100-33-8r-20260609-154742`: `prefetchQueueRegions=1`, 8/8 SHA-256 outputs matched, average `elapsedMillis=1028.62`.
- [x] Full resume after queue-depth change: PID `23920`, log `agent-runs/full-earth-resume-qdepth1-25w1-20260609-154955`, `resumeFingerprintMatched=true`, `resumeJournalRegions=35266`, `prefetchQueueRegions=1`.
- [x] First 30s sample of PID `23920`: average `7.32` CPU cores, `regionGenerated=54`, `regionSkipped=35266`. Latest 80 regions were all ocean with average `elapsedMillis=947.16`, `surfaceSampleMillis=542.16`, `openOceanCompanionPrecomputeMillis=177.4`, and `consumerElapsedMillis=402.68`.
- [x] Follow-up sample after PID `23920` crossed mixed regions: latest 80 had 73 ocean and 7 mixed. Mixed averaged `elapsedMillis=9582.57`, `surfaceSampleMillis=8181.14`, `columnBuildMillis=6388.29`; slow examples included `r.147.38` at `elapsedMillis=20718`, `columnBuildMillis=16624`.
- [x] Stopped PID `23920` before photo-apply profiling.
- [x] Detail smoke for `r.147.38` matched the existing SHA-256 output and completed in `elapsedMillis=12471`, showing full-run `20.7s` was partly scheduling/contention. Detail subphases: `photoApplyMillis=5480`, `columnBuildMillis=3601`, `columnSemanticApplyMillis=5229` summed across columns.
- [x] Tested and rejected owned `PhotoSurfaceDecision -> EarthSurfaceColumn` conversion: SHA-256 still matched for `r.147.38`, but `photoApplyMillis` regressed from `5480` to `6330` and elapsed regressed to `13416`; code was reverted and the release binary rebuilt from the accepted code.
- [x] Replaced hot-path ecoregion/biome substring checks that repeatedly allocated `to_ascii_lowercase()` strings with allocation-free ASCII case-insensitive substring checks. Verification: `cargo test --manifest-path rust\Cargo.toml -p earthmap-surface --lib --locked`, `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`, release build, `r.-63.39` SHA-256 matched existing output (`78CAB41370B4830FC1DEA316261C995930277F88F3DCA33AF954C38AE0BF07FD`), and `r.147.38` SHA-256 matched existing output (`E15EC7B462F4D6EE0EDBC96866BE5D80176689692B37E1B2E3E71ABFAD916E4E`). Smoke timing was mixed: `r.147.38` detail telemetry improved `columnBuildMillis` from `3601` to `3379` and summed `columnSemanticApplyMillis` from `5229` to `4928`, but total elapsed was `13502` due to `photoApplyMillis=6296`, so the next larger target remains photo/post-processing and scheduling.
- [x] Added uniform valid water/land fast paths for region water-mask/coast-factor precompute. Verification: `cargo test --manifest-path rust\Cargo.toml -p earthmap-surface --lib --locked`, `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`, release build, pure-ocean smoke `agent-runs/smoke-uniform-watermask-raw-ocean-20260609-163723` matched 8/8 existing SHA-256 outputs and reduced average `surfacePhase.waterMaskMillis` to `0.75` with wall `6.408s`; mixed `r.147.38` and land `r.-63.39` also matched existing SHA-256 outputs. This helps pure-ocean rows but does not by itself solve sustained all-core utilization.
- [x] Replaced remaining render/post-process biome lowercasing helper calls with allocation-free ASCII case-insensitive checks. Verification: `cargo test --manifest-path rust\Cargo.toml -p earthmap-surface --lib --locked`, `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`, release build, and detail smoke `agent-runs/smoke-detail-biome-ci-r-64-41-20260609-170106`: existing output SHA-256 matched, `elapsedMillis` improved from `14962` to `10759`, `surfaceSampleMillis` from `13117` to `9510`, `photoApplyMillis` from `6070` to `4224`, and `postProcessMillis` from `1015` to `547` on the same `r.-64.41` land-heavy sample. This is a real land/photo hot-path win, but sustained full-run CPU still needs re-sampling.
- [x] Fixed prefetch queue defaulting so `prefetchMemoryGB=N prefetchWorkers=1` uses a memory-bounded prepared-region queue instead of silently defaulting to a one-region queue. This makes the user-specified GB cap meaningful while keeping the producer count explicit. Verification: `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`, release build, and resumed full-run `agent-runs/full-earth-resume-memory-bounded-prefetch-25w1-20260609-171456` reported `prefetchQueueRegions=200`, `prefetchWorkers=1`, and `prefetchMemoryCapBytes=26843545600`.
- [x] Replaced precomputed land photo materials from per-column `Arc<SurfaceMaterialSample>` handles to a compact sample table plus per-column sample index. Verification: `cargo test --manifest-path rust\Cargo.toml -p earthmap-surface --lib --locked`, `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`, release build, and land-heavy smoke `agent-runs/smoke-precomputed-material-index-r-64-43-20260609-172923`: output SHA-256 matched existing `D:\earthmap\1-250-earth-linear\region\r.-64.43.linear` and `phase.surfaceSampleMillis=5286`.
- [x] Fixed startup worker auto-tuning so it compares multiple region-worker candidates (`1`, `2`, legacy `4`, and the requested worker count when distinct) instead of falling back to a single candidate. Ocean samples are now included alongside mixed and land samples so tuning does not optimize only for land/coast. Verification: `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked surface_photo_worker`, `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`, release build.
- [x] Widened the open-ocean output Rayon pool after auto-tuning can select fewer region workers. The resumed autotuned run selected `2` region workers, which reduced `prefetchOpenOceanOutputRayonThreads` to `6` and left ocean regions waiting in the prepared queue; the new rule gives `2` consumers `12` output threads on 16-thread systems while keeping the pool bounded. Verification: `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`, `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked surface_photo_worker`, release build.
- [x] Decoupled prefetch output pool width from selected region worker count so an autotuned `1` region worker does not collapse land/mixed output to one Rayon thread. On 16-thread systems the prefetch output pool now keeps `3` threads and open-ocean output keeps at least `12` bounded threads. Verification: `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`, `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked surface_photo_worker`, release build.
- [x] Decoupled requested prefetch producer count from autotuned consumer region worker count. Before this fix, `prefetchWorkers=2` could still run as `prefetchWorkers=1` when auto-tuning selected one region worker; now producer count is bounded by memory queue and region count, not by consumer count. Verification: `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`, `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked surface_photo_worker`, release build.
- [x] Tested and rejected forcing `prefetchWorkers=2` for the current ocean-heavy resume segment after honoring requested producer count. It preserved `prefetchWorkers=2`, but ocean regions regressed to about `3233ms` average elapsed with high ready-queue wait, while the current best `prefetchWorkers=1` run reports ocean regions around `1021ms` elapsed, `554ms` surface, `463ms` consumer, and near-zero ready wait. Keep `prefetchWorkers=1` as the current stable full-run setting unless a future scheduler separates ocean producers from land/coast producers.
- [x] Resumed the full 1:250 Linear world at `agent-runs/full-earth-resume-final-current-best-25w1-20260609-181828` with `prefetchMemoryGB=25`, `prefetchWorkers=1`, memory-bounded queue `200`, autotuned `regionWorkerThreads=2`, `prefetchOutputRayonThreads=3`, and `prefetchOpenOceanOutputRayonThreads=12`.
- [x] Re-sampled PID `40220` from `agent-runs/full-earth-resume-final-current-best-25w1-20260609-181828`: 45s average `6.85` CPU cores (`42.8%` of 16 cores), private memory `3.896GB`, `regionGenerated=308`, `regionSkipped=37932`. Recent 80 generated regions were all open-ocean and averaged `elapsedMillis=1080.64`, `surfaceSampleMillis=518.48`, `consumerElapsedMillis=558.14`, and `prefetchReadyQueueWaitMillis` near zero.
- [x] Stopped PID `40220` before additional code changes. Evidence from recent open-ocean logs showed `surfaceMaterialRaster.sampleAveragedRequests=0` and no raster tile opens, but `openOceanCompanionPrecomputeMillis` still commonly spent about `180-280ms` scanning/allocation work before discovering that no companion material samples were needed.
- [x] Added an open-ocean companion precompute early exit when no smoothed ocean column is shallow enough to need companion material sampling. This preserves output exactly because the old path also returned `None` when `samples.is_empty()`.
- [x] Added region-local material-cell axis precomputation for open-ocean companion material selection. The first smoke showed this alone was safe but not enough; `surfaceMaterialRaster` requests can be zero while bathymetry/ocean-temperature companion sampling still happens.
- [x] Reduced `GeoTiffFloat32RowCache::sample_bilinear` lock/cache churn by fetching the two needed rows once per bilinear sample instead of asking the row cache separately for each of the four neighboring pixels.
- [x] Verification after open-ocean companion/row-cache work:
  - `cargo test --manifest-path rust\Cargo.toml -p earthmap-geo --lib --locked`
  - `cargo test --manifest-path rust\Cargo.toml -p earthmap-surface --lib --locked`
  - `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --lib --locked prefetch`
  - Release build: `cargo build --manifest-path rust\Cargo.toml -p earthmap-cli --release --target-dir rust\target-latest --locked`
  - Deep-ocean smoke `agent-runs/smoke-deep-ocean-rowcache2-77-47-8r-20260609-185520`: 8/8 SHA-256 outputs matched existing regions; average `openOceanCompanionPrecomputeMillis=226.25` on `r.77.47..r.84.47`.
  - Mixed/land smoke `agent-runs/smoke-mixed-rowcache2--64-47-4r-20260609-185620`: 4/4 SHA-256 outputs matched existing regions.
- [x] Resumed the 25GB full run after the row-cache optimization at `agent-runs/full-earth-resume-rowcache2-25w1-20260609-185844`; the run used `prefetchMemoryGB=25`, `prefetchQueueRegions=200`, `prefetchWorkers=1`, and auto-tuned `regionWorkerThreads=1`.
- [x] Sampled the row-cache full run after generation resumed: 60s average `7.32` CPU cores (`45.7%` of 16 cores), private memory `2.787GB`. Recent 100 generated regions were mostly ocean; ocean averaged `surfaceSampleMillis=513.91`, `consumerElapsedMillis=505.01`, but `prefetchReadyQueueWaitMillis` commonly reached `1.6-1.8s` because only one consumer was draining the prepared queue.
- [x] Stopped PID `2896` before additional scheduler changes. New bottleneck: startup worker tuning can correctly pick one sample worker for land/mixed safety, but in prefetch mode that also collapses prepared-region consumers to one and leaves ocean output queued.
- [x] Separated prefetch producer/sample tuning from consumer/output concurrency so a conservative sample worker choice does not force a single prepared-region consumer during open-ocean-heavy rows. In prefetch mode, consumer workers now floor to `2` when requested/available capacity and region count allow it.
- [x] Prefetch consumer smoke `agent-runs/smoke-prefetch-consumers2-16-49-8r-20260609-191236`: `prefetchConsumerWorkers=4` for the small tuned batch, `prefetchOpenOceanOutputRayonThreads=14`, and average `prefetchReadyQueueWaitMillis=1.75` across 8 ocean regions.
- [ ] Resume the 25GB full run after prefetch consumer separation and sample sustained CPU/ready-queue behavior.
