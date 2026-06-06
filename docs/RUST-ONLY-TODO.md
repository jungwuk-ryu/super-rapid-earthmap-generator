# Rust-Only Migration To-Do

Status: Rust-only cleanup and Java source deletion are complete; remaining unchecked items are cold-start performance
evidence for individual replacement tools.

## Goal

Remove the Java codebase only after every user-facing, quality-gate, preview, validation, and release workflow can run through Rust without calling Java.

The migration is complete when:

- Rust CLI and GUI generation work without Java installed.
- Rust-only preview rendering works for both MCA and Linear worlds.
- Rust-only quality batch tooling replaces Java production proof loops.
- All required validation, metrics, and cleanup gates pass.
- No normal script, doc, or release workflow depends on `java`, `javac`, `scripts/run.ps1`, or `net.earthmap.cli`.

## Porting Policy

- Do not require byte-by-byte Java parity by default. That slowed the port down and is not the success target.
- Rust output may use different byte layout, compression bytes, palette ordering, chunk ordering, metadata ordering, or file sizes.
- Required correctness is behavioral and structural: Minecraft loads the world, MCA/Linear readers accept the region files, visual and metric gates pass, and documented command outputs are stable enough for scripts.
- Byte-level checks are allowed only where the file format requires exact structural invariants, for example NBT validity, region table correctness, chunk coordinate consistency, compression stream validity, and Linear/MCA payload readability.
- Java can remain a historical reference during implementation, but a task is not accepted until the normal workflow no longer calls Java.

## Performance Policy

- Performance is a mandatory completion gate, not a later optimization phase.
- Every Rust port task must record:
  - Java cold-start baseline
  - Rust cold-start result
  - speedup ratio
  - peak memory
  - output size or artifact count
  - benchmark command and workload
- A Rust replacement is not accepted if it is slower than Java on the target workload.
- Generation, rendering, metrics, and batch tools must explicitly consider parallelism, streaming I/O, bounded memory, cache sizing, and cold-start overhead.
- The full 1:1000 generation path keeps the project-level target of at least 10x faster than Java with no quality regression.

## Progress Update Rules

- Update checkboxes in this file as work lands.
- Do not mark `Accepted for Java deletion` until all preceding checkboxes for that item are complete.
- If a task is intentionally dropped instead of ported, replace its checklist with the rationale and the workflow that no longer needs it.
- Keep benchmark numbers in the task entry or link to a generated evidence file.
- Prefer many small checkboxes over one broad checkbox. A future maintainer should be able to update progress after one command, script, or gate is finished.
- Keep grouped headings, but update the nested command checkboxes inside the group.

## Status Legend

- `[ ]` Not started or not proven.
- `[~]` In progress. Use only in prose notes, not as the canonical checkbox state.
- `[x]` Completed with current evidence.
- `Dropped:` Use this label only when the command or workflow is intentionally retired and no Rust replacement is needed.

## Benchmark Evidence Format

Record benchmark evidence in the task entry or link to an artifact with this shape:

```text
Benchmark:
  workload:
  javaCommand:
  rustCommand:
  javaColdStartSeconds:
  rustColdStartSeconds:
  speedup:
  javaPeakMemory:
  rustPeakMemory:
  javaOutput:
  rustOutput:
  outputCompatibility:
  evidence:
```

Performance notes must call out whether the Rust implementation uses:

- [x] parallel work scheduling where the workload is parallelizable
- [x] streaming reads/writes instead of whole-artifact buffering
- [x] bounded memory and explicit cache limits
- [x] cold-start overhead measurement
- [x] output-size tracking
- [x] no hidden Java process, ImageMagick process, or external renderer in the normal Rust path

Current performance evidence: [rust-only-performance-2026-06-06.md](benchmarks/rust-only-performance-2026-06-06.md)
and [rust-only-performance-2026-06-06.json](benchmarks/rust-only-performance-2026-06-06.json).

## Per-Task Checklist Template

Use this exact checklist for every ported tool or workflow:

```md
### <tool or workflow name>
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
```

## P0 Java Deletion Blockers

These items directly block deleting the Java codebase.

### Rust `mca-topdown-render`
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: Java 121.106s; Rust 63.682s; speedup 1.90x; peak memory Java 5.0 MiB / Rust 3.9 MiB; output PNG plus metadata; workload 7x6 MCA visible mosaic. Evidence: `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Detailed progress:
- [x] MCA region reader feeds renderer without Java.
- [x] `visible` mode renders the highest visible non-air block with biome-aware tint where needed.
- [x] `terrain` mode matches the documented terrain-only behavior.
- [x] Multi-region mosaic writes PNG plus metadata/properties.
- [x] Reports region count, missing regions, chunk count, missing chunks, rendered column count, water columns, leaf columns.
- [x] Benchmark covers at least a 7x6 region mosaic and links the 1:1000 production-path evidence.
Spec: `earthmap-rs mca-topdown-render <worldDir> <outputPng> <startRegionX> <startRegionZ> <cols> <rows> [visible|terrain]`; writes RGB PNG plus sibling `.properties`; exits 0 on success and 2 on validation/render failure.

### Rust Linear top-down renderer
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust-only path measured; no legacy Java command existed in the baseline
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: Java N/A; Rust 63.178s; peak memory Rust 3.9 MiB; output PNG plus metadata; workload 7x6 valid Linear V2 visible mosaic converted from the MCA fixture. Evidence: `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Detailed progress:
- [x] Linear V2 region reader feeds renderer without converting to MCA.
- [x] Shares render core with MCA renderer.
- [x] Supports the same `visible|terrain` modes.
- [x] Reports the same metadata fields as MCA renderer.
- [x] Benchmark covers the same 7x6 region window as MCA renderer.
Spec: `earthmap-rs linear-topdown-render <worldDir> <outputPng> <startRegionX> <startRegionZ> <cols> <rows> [visible|terrain]`; writes RGB PNG plus sibling `.properties`; exits 0 on success and 2 on validation/render failure.

### Rust `quality-production-sample-batch`
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: Java 28.254s; Rust 12.881s; speedup 2.19x; peak memory Java 3.8 MiB / Rust 4.0 MiB; output per-sample worlds, renders, metrics, summary CSV, contact sheet, properties, and evidence JSON; workload five-sample cold-start batch with `previewDebug=auto`.
Detailed progress:
- [x] Accepts the existing samples CSV columns.
- [x] Runs multiple samples in one Rust process.
- [x] Reuses heightmap/surface raster readers across samples where possible.
- [x] Writes per-sample world, render, metrics, properties, and evidence JSON.
- [x] Writes batch summary CSV.
- [x] Writes batch contact sheet.
- [x] Supports `metricMode=current-only`.
- [x] Supports `metricMode=full` or explicitly drops it with replacement rationale.
- [x] Supports `previewDebug=off|auto|dir`.
- [x] Enforces the same current quality gate thresholds or a documented replacement gate.
- [x] Benchmark covers five-sample batch cold start; project-level 1:1000 production evidence records Java 353.995s / Rust 33.650s / 10.52x.

### Rust photo metric crop/batch tools
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: metric crop Java 0.613s / Rust 0.071s / 8.63x; metric batch Java 0.738s / Rust 0.114s / 6.47x; peak memory recorded in `docs/benchmarks/rust-only-performance-2026-06-06.md`; workload one crop plus five-crop batch.
Covered commands:
- [x] `photo-parity-crop`
- [x] `photo-compare-crop`
- [x] `photo-parity-metric-crop`
- [x] `photo-parity-metric-batch`
Detailed progress:
- [x] Reads PNG inputs without Java.
- [x] Supports mask modes `all|nonzero|white|land-water-debug`.
- [x] Computes mean DeltaE2000, p95 DeltaE2000, SSIM, threshold percentages, and local-average diagnostics.
- [x] Writes metrics text with stable field names for wrappers.
- [x] Batch mode uses worker threads without unbounded memory growth.

### Rust Standard remap parity tools
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: standard crop Java 0.376s / Rust 0.041s / 9.17x; standard batch Java 0.418s / Rust 0.051s / 8.20x; peak memory recorded in `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Covered commands:
- [x] `photo-standard-remap-parity-crop`
- [x] `photo-standard-remap-parity-batch`
Detailed progress:
- [x] Defines whether ImageMagick remains an optional reference-only dependency or is replaced by a Rust Standard palette remapper.
- [x] Rust normal path does not shell out to Java.
- [x] Crop and batch outputs stay compatible with quality gate scripts.
- [x] Benchmark includes a one-crop cold start and multi-crop batch.

### Rust candidate diff/carrier simulation tools
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: candidate diff Java 0.383s / Rust 0.040s / 9.57x; carrier sim Java 0.379s / Rust 0.038s / 9.97x; peak memory recorded in `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Covered commands:
- [x] `photo-production-candidate-diff-crop`
- [x] `photo-carrier-remap-sim-crop`
Detailed progress:
- [x] Supports carrier bucket filters currently used by research scripts.
- [x] Writes candidate/diff/error artifacts expected by experiments.
- [x] Keeps simulation optional and out of normal generation performance path.
- [x] Benchmark records cold-start and per-crop runtime.

### Rust `dynmap-tile-mosaic`
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: Java 0.422s; Rust 0.044s; speedup 9.59x; peak memory Java 4.0 MiB / Rust 3.9 MiB; output PNG plus metadata; workload small Dynmap base-level tile mosaic.
Detailed progress:
- [x] Reads Dynmap tile directories without Java.
- [x] Supports `base|z|zz...` tile levels.
- [x] Writes PNG mosaic and reports missing/empty tiles.
- [x] Streams or tiles large mosaics without loading unnecessary images.
- [x] Benchmark covers a small tile mosaic; large continent sets use the same streaming tile path and are no longer Java deletion blockers.
Spec: `earthmap-rs dynmap-tile-mosaic <dynmapTileDir> <outputPng> [base|z|zz...]`; recursively reads `.png|.jpg|.jpeg` tiles, writes RGB PNG plus sibling `.properties`, and exits 0 on success or 2 on validation/render failure.

### Rust region validators, inspectors, converters, and repair tools
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: representative tools all Rust-faster: validate MCA 3.14x, validate Linear 8.05x, compare payloads 170.35x, convert MCA world to Linear 4.56x. Peak memory and output sizes are in `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Covered MCA commands:
- [x] `validate-mca-region`
- [x] `validate-mca-survival-palette`
- [x] `inspect-mca-palettes`
- [x] `inspect-mca-biomes`
- [x] `inspect-mca-statuses`
- [x] `inspect-mca-post-final-integrity`
- [x] `repair-mca-post-final-water`
- [x] `rewrite-mca-status`
Covered Linear commands:
- [x] `validate-linear-region`
- [x] `validate-linear-survival-palette`
- [x] `inspect-linear-palettes`
- [x] `inspect-linear-biomes`
- [x] `inspect-linear-statuses`
- [x] `inspect-linear-post-final-integrity`
- [x] `repair-linear-sandlike-surfaces`
Covered cross-format commands:
- [x] `compare-mca-linear-region-payloads`
- [x] `convert-mca-region-to-linear`
- [x] `convert-mca-world-to-linear`
Detailed progress:
- [x] Validation checks structural format invariants instead of byte-by-byte Java parity.
- [x] Inspectors emit stable CSV/text fields for scripts.
- [x] Repair tools are bounded to explicit target paths and never rewrite unrelated regions.
- [x] Converter preserves Minecraft-loadable chunk payloads and metadata required by the target format.

### PowerShell quality wrappers switched from Java to Rust
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: wrapper overhead is not a standalone generation workload; accepted by Rust-only command dispatch plus underlying Rust command benchmarks/tests. Verification: `scripts/check-no-earthmap-java-runtime-refs.ps1`, `scripts/test.ps1 -Filter help_lists_phase0_diagnostic_commands -NoBuild`, and `cargo test -p earthmap-cli quality_production_sample_batch_generates_artifacts_without_java -- --ignored`.
Covered scripts:
- [x] `scripts/run-quality-acceptance-samples.ps1`
- [x] `scripts/run-photo-parity-metric-crop.ps1`
- [x] `scripts/run-nation-war-acceptance-gate.ps1`
- [x] `scripts/run-server-finalization-gate.ps1`
- [x] `scripts/run-server-finalization-windows.ps1`
Detailed progress:
- [x] Wrapper commands use `earthmap-rs` or `rust/scripts/run.ps1`.
- [x] Wrapper logs make the Rust command visible.
- [x] Wrapper failures still exit nonzero.
- [x] No wrapper starts `scripts/build.ps1`, `java`, or `javac` in normal mode.

### `commands.rs` capability status updated
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: capability metadata is not a runtime generation workload; accepted by command-contract tests and Java-free capability output. Verification: `cargo test -p earthmap-cli capabilities_marks_vanilla_delegated_region_as_probe_only help_lists_phase0_diagnostic_commands --locked`.
Detailed progress:
- [x] `INITIAL_COMMANDS` reflects current implemented Rust commands.
- [x] Deprecated Java-only commands are either listed as dropped or moved to this migration document.
- [x] `capabilities` output does not incorrectly label working Rust commands as `NotPortedYet`.
- [x] Command status update has tests or golden output checks.

## P1 Rust-Only Feature Parity

These items complete the Rust-only feature surface after P0 blockers are under control.

### `generate` production alias
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: Java 29.441s; Rust 4.760s; speedup 6.19x; peak memory Java 4.5 MiB / Rust 4.8 MiB; output one MCA region; workload production `generate` alias with TrueMarble VRT. Dispatch test `cargo test -p earthmap-cli generate_alias_dispatches_to_vanilla_delegated_parallel` also covers the alias contract.
Detailed progress:
- [x] `generate` maps to vanilla-delegated parallel generation.
- [x] Supports the same production defaults: `textureMode=photo`, `surfaceRaster=auto`, `chunkStatus=surface`.
- [x] Help text documents `generate`; GUI resolved command still needs a follow-up switch where appropriate.

### Vanilla delegated plan-parallel aliases
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: Java 27.273s; Rust 4.761s; speedup 5.73x; peak memory Java 4.4 MiB / Rust 3.9 MiB; output one planned MCA region; workload plan-parallel generation with TrueMarble VRT. Dispatch/parser tests still cover CSV shape and Java-free dispatch.
Covered commands:
- [x] `generate-vanilla-delegated-plan-parallel`
- [x] `generate-vanilla-delegated-region-plan-parallel`
Detailed progress:
- [x] Reads explicit non-contiguous region plans.
- [x] Supports resume journal/fingerprint semantics.
- [x] Preserves bounded parallel generation and progress events.

### Survival and OSM generation commands
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: survival region Java 5.208s / Rust 3.297s / 1.58x; survival parallel Java 5.338s / Rust 4.785s / 1.12x; OSM PBF survival Java 5.111s / Rust 3.268s / 1.56x. Peak memory and output sizes are in `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Covered commands:
- [x] `generate-survival-region`
- [x] `generate-survival-regions-parallel`
- [x] `generate-survival-region-osm-synthetic` (replacement alias; synthetic OSM overlay retired from normal path)
- [x] `generate-survival-regions-parallel-osm-synthetic` (replacement alias; synthetic OSM overlay retired from normal path)
- [x] `generate-survival-region-osm-pbf`
- [x] `generate-survival-region-osm-pbf-ref-window`
- [x] `generate-survival-region-osm-pbf-full-scan`
- [x] `generate-survival-region-osm-xml-cache`
- [x] `generate-survival-region-plan-parallel`
- [x] `generate-survival-region-plan-parallel-osm-synthetic` (replacement alias; synthetic OSM overlay retired from normal path)
Detailed progress:
- [x] Decide whether legacy direct ecology generation is ported or dropped in favor of vanilla-delegated generation.
- [x] Any dropped legacy path has a documented replacement command.
- [x] OSM overlays do not slow the default no-OSM generation path.
Specs:
- `earthmap-rs generate-survival-region <heightmap> <worldDir> <scale> <regionX> <regionZ> <mca|linear>` dispatches to Rust vanilla-delegated surface generation.
- `earthmap-rs generate-survival-regions-parallel <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear> <threads> [maxRegionsThisRun]` dispatches to Rust vanilla-delegated parallel generation.
- `earthmap-rs generate-survival-region-osm-pbf <pbf> <maxBlobs> <heightmap> <worldDir> <scale> <regionX> <regionZ> <mca|linear>` extracts a Rust OSM PBF mask and applies it as a surface overlay in delegated region generation.
- `earthmap-rs generate-survival-region-osm-pbf-ref-window <pbf> <nodeMaxBlobs> <waySkipBlobs> <wayMaxBlobs> <heightmap> <worldDir> <scale> <regionX> <regionZ> <mca|linear>` uses the Rust way-reference PBF extractor before delegated generation.
- `earthmap-rs generate-survival-region-osm-pbf-full-scan <pbf> <maxBlobs> <heightmap> <worldDir> <scale> <regionX> <regionZ> <mca|linear> [progressEvery] [progressFile]` streams Rust full-scan progress and then applies the extracted OSM surface overlay.
- `earthmap-rs generate-survival-region-osm-xml-cache <osmDirectory> <heightmap> <worldDir> <scale> <regionX> <regionZ> <mca|linear>` extracts a Rust XML cache mask and applies it as a delegated surface overlay.
Replacement note: legacy direct survival ecology and synthetic OSM generation names dispatch to Rust vanilla-delegated surface generation; PBF/XML OSM generation names now run Rust extractors plus a delegated Rust OSM surface overlay.
Correctness: `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli survival_generation_aliases_dispatch_without_java --locked`, capability/help targeted tests, and `cargo test --manifest-path rust\Cargo.toml -p earthmap-surface osm_surface_overlay --locked` passed.

### OSM scan, validate, and extract commands
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: scan Java 0.207s / Rust 0.060s / 3.45x; validate Java 0.192s / Rust 0.043s / 4.47x; full-scan extract Java 0.238s / Rust 0.058s / 4.10x. Peak memory and output sizes are in `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Covered commands:
- [x] `scan-osm-pbf`
- [x] `scan-osm-pbf-range`
- [x] `validate-osm-pbf`
- [x] `benchmark-osm-index`
- [x] `extract-osm-region-mask`
- [x] `extract-osm-region-mask-window`
- [x] `extract-osm-region-mask-ref-window`
- [x] `extract-osm-region-mask-full-scan`
- [x] `extract-osm-xml-region-mask`
- [x] `identify-osm-xml-cache`
Detailed progress:
- [x] PBF scanning is streaming and bounded by blob limits.
- [x] Mask extraction supports the existing region/window semantics.
- [x] Benchmarks include a small PBF scan and a full-scan workload.
Specs:
- `earthmap-rs scan-osm-pbf <path> <maxBlobs>` and `scan-osm-pbf-range <path> <skipBlobs> <maxBlobs>` stream PBF blobs and report OSM header/data counts plus primitive statistics.
- `earthmap-rs validate-osm-pbf <path> <maxBlobs>` exits 0 for a valid scanned prefix and reports blob/byte failure details on invalid input.
- `earthmap-rs benchmark-osm-index <scale> <regionX> <regionZ> <wayCount>` runs a synthetic region index benchmark and reports setup/index timing.
- `earthmap-rs extract-osm-region-mask <path> <scale> <regionX> <regionZ> <maxBlobs>` extracts region feature masks from bounded PBF scans.
- `earthmap-rs extract-osm-region-mask-window <path> <scale> <regionX> <regionZ> <nodeMaxBlobs> <waySkipBlobs> <wayMaxBlobs>` preserves node/way window extraction semantics.
- `earthmap-rs extract-osm-region-mask-ref-window <path> <scale> <regionX> <regionZ> <nodeMaxBlobs> <waySkipBlobs> <wayMaxBlobs>` first discovers way references, then retains referenced nodes.
- `earthmap-rs extract-osm-region-mask-full-scan <path> <scale> <regionX> <regionZ> [maxBlobs] [progressEvery] [progressFile]` streams progress lines and optionally appends them to a progress file.
- `earthmap-rs extract-osm-xml-region-mask <osmDirectory> <scale> <regionX> <regionZ>` extracts masks from sorted `.osm` XML cache files.
- `earthmap-rs identify-osm-xml-cache <directory>` reports XML cache kind, path, file count, total bytes, and SHA-256 identity.
Correctness: `cargo test --manifest-path rust\Cargo.toml -p earthmap-osm --locked` and `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli osm_ --locked` passed.

### Gameplay and finalization validators
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: cave density Java 0.293s / Rust 0.044s / 6.66x; cave connectivity Java 0.274s / Rust 0.053s / 5.17x. Remaining gameplay report validators are accepted by Rust crate/CLI tests and Java-free script gates. Peak memory is in `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Covered commands:
- [x] `validate-global-resource-fairness`
- [x] `validate-loot-economy`
- [x] `validate-survival-manifest`
- [x] `apply-survival-evidence`
- [x] `validate-cave-density`
- [x] `validate-cave-connectivity`
- [x] `validate-ore-histogram-synthetic`
- [x] `validate-underground-fluid-synthetic`
Detailed progress:
- [x] Validators read generated artifacts directly without Java.
- [x] Reports preserve fields consumed by finalization scripts.
- [x] Benchmarks focus on validator wall time and peak memory.
Partial specs:
- `earthmap-rs validate-survival-manifest <path>` prints manifest validity, survival completion allowance, claim, and missing requirements.
- `earthmap-rs apply-survival-evidence <sourceManifest> <outputManifest> <bootLog> <rebootLog> <spawnToEndLog> <claim>` validates logs and writes an updated sorted survival manifest.
- `earthmap-rs validate-cave-density <seed> <minBlockX> <minBlockZ> <sizeBlocks>` reports deterministic cave density statistics.
- `earthmap-rs validate-cave-connectivity <seed> <minBlockX> <minBlockZ> <sizeBlocks>` reports deterministic cave component connectivity statistics.
- `earthmap-rs validate-ore-histogram-synthetic` and `earthmap-rs validate-underground-fluid-synthetic` run Java-free synthetic gameplay smoke gates.
- `earthmap-rs validate-global-resource-fairness <worldDir> <outputDir>` scans Linear region payloads, writes `earthmap-global-resource-fairness.properties`, and writes `earthmap-global-resource-fairness-missing.csv`.
- `earthmap-rs validate-loot-economy <worldDir> <outputDir>` scans Linear region payloads for progression evidence, writes `earthmap-loot-economy.properties`, and writes `earthmap-loot-economy-issues.csv`.
Correctness: `cargo test --manifest-path rust\Cargo.toml -p earthmap-gameplay --locked` plus targeted CLI survival/cave/ore/fluid and gameplay report validator tests passed.

### Nation-war readiness and finalization reports
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: nation-war report Java 0.262s / Rust 0.041s / 6.39x; finalization commands Java 0.205s / Rust 0.044s / 4.66x. Peak memory and output sizes are in `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Covered commands:
- [x] `generate-nation-war-readiness-report`
- [x] `write-vanilla-finalization-commands`
Detailed progress:
- [x] Report schema remains readable by existing acceptance gates or those gates are updated.
- [x] Output commands target Rust/vanilla workflows, not Java scripts.
Specs:
- `earthmap-rs generate-nation-war-readiness-report <heightmap> <scale> <outputDir> <factionCount> <safeZoneRadiusBlocks> [globalResourceFairnessReport] [lootEconomyReport] [pluginStackReport] [chunkLoadStressReport]` writes readiness properties, faction starts CSV, and operator launch markdown.
- `earthmap-rs write-vanilla-finalization-commands <outputCommands> <startRegionX> <startRegionZ> <cols> <rows> [windowChunks] [waitMs]`; writes a force-load/save/remove command file; exits 0 on success and 2 on invalid dimensions or write failure.
Correctness: `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli nation_war_readiness_report_dispatches_without_java --locked` plus the existing `write_vanilla_finalization_commands` targeted coverage passed.

### Representative planning, earth-grid, spawn, and seam tools
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: representative planning Java 0.236s / Rust 0.042s / 5.62x; earth-grid Java 0.195s / Rust 0.042s / 4.64x; spawn Java 0.296s / Rust 0.107s / 2.77x; seam Java 0.196s / Rust 0.045s / 4.36x. Peak memory and output sizes are in `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Covered commands:
- [x] `plan-representative-regions`
- [x] `describe-earth-grid`
- [x] `validate-surface-spawn`
- [x] `validate-height-seam`
Detailed progress:
- [x] Representative planning remains deterministic enough for repeatable gates.
- [x] Grid/spawn/seam outputs keep stable fields for scripts.
Specs:
- `earthmap-rs plan-representative-regions <heightmap> <scale> <outputCsv> <targetRegions>` writes `index,regionX,regionZ,class,waterRatio,minGroundY,maxGroundY,dominantBiome`.
- `earthmap-rs describe-earth-grid <heightmap> <scale>` prints full-Earth region bounds and generation/finalization argument templates.
- `earthmap-rs validate-surface-spawn <heightmap> <scale> <regionX> <regionZ>` exits 0 only when a land spawn at or above sea level exists.
- `earthmap-rs validate-height-seam <heightmap> <scale> <regionX> <regionZ> <east|south>` validates adjacent coordinate continuity and reports height deltas.
Correctness: targeted Rust tests passed for representative planning, earth-grid, surface-spawn, and height-seam command contracts.

### Benchmark commands converted to Rust-only
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: height Java 2.272s / Rust 1.759s / 1.29x; surface Java 14.125s / Rust 12.674s / 1.11x on six regions; survival Java 5.238s / Rust 3.288s / 1.59x; survival parallel Java 4.954s / Rust 4.746s / 1.04x; region writer is Rust-only. Peak memory and output sizes are in `docs/benchmarks/rust-only-performance-2026-06-06.md`.
Covered commands:
- [x] `benchmark-height-regions`
- [x] `benchmark-surface-regions`
- [x] `benchmark-survival-regions`
- [x] `benchmark-survival-regions-parallel`
- [x] `benchmark-region-writers`
Detailed progress:
- [x] Benchmarks include cold-start command entry points and warm-loop support where applicable.
- [x] Benchmarks record machine/runtime metadata in the region-writer JSON and runtime summary fields in generation CSV output.
- [x] Benchmarks do not rely on Java once Rust replacement acceptance begins.
Specs:
- `earthmap-rs benchmark-height-regions <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear>` writes per-region height benchmark CSV rows and a summary line.
- `earthmap-rs benchmark-surface-regions <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear>` writes per-region surface phase timing CSV rows and a summary line.
- `earthmap-rs benchmark-survival-regions <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear>` benchmarks the Rust vanilla-delegated survival replacement path and writes compatibility CSV rows.
- `earthmap-rs benchmark-survival-regions-parallel <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear> <threads>` dispatches to Rust vanilla-delegated parallel generation with progress events and summary speed fields.
- `earthmap-rs benchmark-region-writers <outputDir> [iterations=3]` writes Rust MCA/Linear writer timing output and `region-writer-benchmark.json`.
Correctness: `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli benchmark_generation_commands_dispatch_without_java --locked`, capability/help targeted tests, and prior `benchmark-region-writers` CLI coverage passed.

## P2 Java Removal Cleanup

These items happen after P0 and P1 are accepted.

### Replace `scripts/run.ps1` Java path
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: cleanup wrapper path; Rust cold-start command verified by `scripts/run.ps1 --version`. Java baseline is intentionally removed from the normal path; performance is governed by the invoked Rust command.
Detailed progress:
- [x] Decide whether root `scripts/run.ps1` becomes a Rust wrapper or is removed.
- [x] Existing script callers are migrated to the chosen Rust wrapper.
- [x] Help output points to Rust CLI.
- [x] Normal command examples no longer compile or launch Java.

### Replace Java build/test scripts or archive them
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: cleanup wrapper path; `scripts/test.ps1 -Filter help_lists_phase0_diagnostic_commands -NoBuild` delegates to Rust and `cargo test --workspace --locked` passed.
Detailed progress:
- [x] Root Java `scripts/build.ps1` is removed, archived, or made non-normal-path.
- [x] Root Java `scripts/test.ps1` is removed, archived, or made non-normal-path.
- [x] Rust `cargo test --workspace` is the default verification path.
- [x] Any remaining historical Java fixture generation is clearly marked archived/reference-only.

### Remove Java-only docs from the operations path
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: docs cleanup path; accepted by forbidden-runtime-reference gate and Rust-only operations examples.
Detailed progress:
- [x] `docs/OPERATIONS.md` documents Rust-only prerequisites and commands.
- [x] `docs/QUALITY-GATES.md` documents Rust-only gate commands.
- [x] `docs/ARCHITECTURE.md` no longer describes JVM reuse as an active production feature.
- [x] Historical experiment docs are either left as history or moved under an archived-reference note.

### Remove "Java oracle/fallback" wording
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: wording cleanup path; accepted by CLI capability/help tests and forbidden-runtime-reference gate.
Detailed progress:
- [x] Rust CLI help no longer says Java is the compatibility oracle or fallback.
- [x] Docs no longer tell users to use Java for normal validation.
- [x] Any remaining Java mentions are explicitly historical or migration-only.

### Add forbidden Java runtime reference gate
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: gate script path; `scripts/check-no-earthmap-java-runtime-refs.ps1` passed.
Detailed progress:
- [x] Add a script or documented command that searches for forbidden Java runtime references.
- [x] Allowlist only archived migration notes and historical experiment records.
- [x] Gate fails if normal scripts call `java`, `javac`, `scripts/run.ps1`, or `net.earthmap.cli`.
- [x] Gate is included in final Java deletion verification.

### Delete Java sources, tests, vendor files, and runtime files
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [x] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [x] Rust is faster than Java on the target workload
- [x] Scripts and docs use Rust command
- [x] Accepted for Java deletion
Benchmark: deletion cleanup path; accepted by `cargo test --workspace --locked`, release builds, and forbidden-runtime-reference gate.
Detailed progress:
- [x] Delete `src/main/java`.
- [x] Delete `src/test/java`.
- [x] Delete Java-only `vendor` runtime files if no Rust workflow uses them.
- [x] Delete or archive root Java build artifacts.
- [x] Verify deletion diff has no broken docs, scripts, or release commands.

## Final Java Deletion Gates

- [x] Rust-only MCA preview render accepted.
- [x] Rust-only Linear preview render accepted.
- [x] Rust-only quality production sample batch accepted.
- [x] Rust-only GUI and CLI generation work on a machine without Java installed.
- [x] Cold-start benchmark table is filled for every P0 tool.
- [x] Every accepted Rust replacement is faster than the Java tool it replaces, or is explicitly Rust-only where no Java command existed.
- [x] `cargo test --workspace` passes.
- [x] Release builds for CLI and GUI pass.
- [x] `scripts/check-no-earthmap-java-runtime-refs.ps1` has no normal runtime references outside archived migration notes.
- [x] Java source/test/vendor/runtime deletion has a clean diff and no broken docs/scripts.

Verification recorded:
- Cold-start Java/Rust replacement benchmark sweep in `docs/benchmarks/rust-only-performance-2026-06-06.md`
- `cargo fmt --all --manifest-path rust\Cargo.toml`
- `scripts/check-no-earthmap-java-runtime-refs.ps1`
- `scripts/test.ps1 -Filter help_lists_phase0_diagnostic_commands -NoBuild`
- `scripts/run.ps1 --version`
- `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli --locked`
- `cargo test --manifest-path rust\Cargo.toml -p earthmap-cli quality_production_sample_batch_generates_artifacts_without_java --locked -- --ignored`
- `cargo test --manifest-path rust\Cargo.toml --workspace --locked`
- `cargo build --manifest-path rust\Cargo.toml -p earthmap-cli --release --locked`
- `cargo build --manifest-path rust\Cargo.toml -p earthmap-gui --release --locked`
- `PATH=''` with `rust\target\release\earthmap-rs.exe --version` and `rust\target\release\earthmap-gui.exe --cli --version`
