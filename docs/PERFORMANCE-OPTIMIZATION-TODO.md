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
