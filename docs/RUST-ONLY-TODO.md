# Rust-Only Migration To-Do

Status: draft tracker for removing the Java codebase after Rust replacement tools are accepted.

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

- [ ] parallel work scheduling where the workload is parallelizable
- [ ] streaming reads/writes instead of whole-artifact buffering
- [ ] bounded memory and explicit cache limits
- [ ] cold-start overhead measurement
- [ ] output-size tracking
- [ ] no hidden Java process, ImageMagick process, or external renderer in the normal Rust path

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
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [x] MCA region reader feeds renderer without Java.
- [x] `visible` mode renders the highest visible non-air block with biome-aware tint where needed.
- [x] `terrain` mode matches the documented terrain-only behavior.
- [x] Multi-region mosaic writes PNG plus metadata/properties.
- [x] Reports region count, missing regions, chunk count, missing chunks, rendered column count, water columns, leaf columns.
- [ ] Benchmark covers at least a 7x6 region mosaic and a 1:1000 whole-continent mosaic.
Spec: `earthmap-rs mca-topdown-render <worldDir> <outputPng> <startRegionX> <startRegionZ> <cols> <rows> [visible|terrain]`; writes RGB PNG plus sibling `.properties`; exits 0 on success and 2 on validation/render failure.

### Rust Linear top-down renderer
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [x] Linear V2 region reader feeds renderer without converting to MCA.
- [x] Shares render core with MCA renderer.
- [x] Supports the same `visible|terrain` modes.
- [x] Reports the same metadata fields as MCA renderer.
- [ ] Benchmark covers the same region windows as MCA renderer.
Spec: `earthmap-rs linear-topdown-render <worldDir> <outputPng> <startRegionX> <startRegionZ> <cols> <rows> [visible|terrain]`; writes RGB PNG plus sibling `.properties`; exits 0 on success and 2 on validation/render failure.

### Rust `quality-production-sample-batch`
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [ ] Accepts the existing samples CSV columns.
- [ ] Runs multiple samples in one Rust process.
- [ ] Reuses heightmap/surface raster readers across samples where possible.
- [ ] Writes per-sample world, render, metrics, properties, and evidence JSON.
- [ ] Writes batch summary CSV.
- [ ] Writes batch contact sheet.
- [ ] Supports `metricMode=current-only`.
- [ ] Supports `metricMode=full` or explicitly drops it with replacement rationale.
- [ ] Supports `previewDebug=off|auto|dir`.
- [ ] Enforces the same current quality gate thresholds or a documented replacement gate.
- [ ] Benchmark covers five-crop batch cold start and representative 1:1000 samples.

### Rust photo metric crop/batch tools
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered commands:
- [ ] `photo-parity-crop`
- [ ] `photo-compare-crop`
- [ ] `photo-parity-metric-crop`
- [ ] `photo-parity-metric-batch`
Detailed progress:
- [ ] Reads PNG inputs without Java.
- [ ] Supports mask modes `all|nonzero|white|land-water-debug`.
- [ ] Computes mean DeltaE2000, p95 DeltaE2000, SSIM, threshold percentages, and local-average diagnostics.
- [ ] Writes metrics text with stable field names for wrappers.
- [ ] Batch mode uses worker threads without unbounded memory growth.

### Rust Standard remap parity tools
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered commands:
- [ ] `photo-standard-remap-parity-crop`
- [ ] `photo-standard-remap-parity-batch`
Detailed progress:
- [ ] Defines whether ImageMagick remains an optional reference-only dependency or is replaced by a Rust Standard palette remapper.
- [ ] Rust normal path does not shell out to Java.
- [ ] Crop and batch outputs stay compatible with quality gate scripts.
- [ ] Benchmark includes a one-crop cold start and multi-crop batch.

### Rust candidate diff/carrier simulation tools
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered commands:
- [ ] `photo-production-candidate-diff-crop`
- [ ] `photo-carrier-remap-sim-crop`
Detailed progress:
- [ ] Supports carrier bucket filters currently used by research scripts.
- [ ] Writes candidate/diff/error artifacts expected by experiments.
- [ ] Keeps simulation optional and out of normal generation performance path.
- [ ] Benchmark records cold-start and per-crop runtime.

### Rust `dynmap-tile-mosaic`
- [x] Spec: CLI args, outputs, exit codes, artifact paths documented
- [x] Rust implementation exists
- [x] Java is no longer called by normal workflow
- [x] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [x] Reads Dynmap tile directories without Java.
- [x] Supports `base|z|zz...` tile levels.
- [x] Writes PNG mosaic and reports missing/empty tiles.
- [x] Streams or tiles large mosaics without loading unnecessary images.
- [ ] Benchmark covers a small crop and a large continent tile set.
Spec: `earthmap-rs dynmap-tile-mosaic <dynmapTileDir> <outputPng> [base|z|zz...]`; recursively reads `.png|.jpg|.jpeg` tiles, writes RGB PNG plus sibling `.properties`, and exits 0 on success or 2 on validation/render failure.

### Rust region validators, inspectors, converters, and repair tools
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered MCA commands:
- [x] `validate-mca-region`
- [x] `validate-mca-survival-palette`
- [x] `inspect-mca-palettes`
- [x] `inspect-mca-biomes`
- [x] `inspect-mca-statuses`
- [ ] `inspect-mca-post-final-integrity`
- [ ] `repair-mca-post-final-water`
- [ ] `rewrite-mca-status`
Covered Linear commands:
- [x] `validate-linear-region`
- [x] `validate-linear-survival-palette`
- [x] `inspect-linear-palettes`
- [x] `inspect-linear-biomes`
- [x] `inspect-linear-statuses`
- [ ] `inspect-linear-post-final-integrity`
- [ ] `repair-linear-sandlike-surfaces`
Covered cross-format commands:
- [x] `compare-mca-linear-region-payloads`
- [x] `convert-mca-region-to-linear`
- [x] `convert-mca-world-to-linear`
Detailed progress:
- [x] Validation checks structural format invariants instead of byte-by-byte Java parity.
- [x] Inspectors emit stable CSV/text fields for scripts.
- [ ] Repair tools are bounded to explicit target paths and never rewrite unrelated regions.
- [x] Converter preserves Minecraft-loadable chunk payloads and metadata required by the target format.

### PowerShell quality wrappers switched from Java to Rust
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered scripts:
- [ ] `scripts/run-quality-acceptance-samples.ps1`
- [ ] `scripts/run-photo-parity-metric-crop.ps1`
- [ ] `scripts/run-nation-war-acceptance-gate.ps1`
- [ ] `scripts/run-server-finalization-gate.ps1`
- [ ] `scripts/run-server-finalization-windows.ps1`
Detailed progress:
- [ ] Wrapper commands use `earthmap-rs` or `rust/scripts/run.ps1`.
- [ ] Wrapper logs make the Rust command visible.
- [ ] Wrapper failures still exit nonzero.
- [ ] No wrapper starts `scripts/build.ps1`, `java`, or `javac` in normal mode.

### `commands.rs` capability status updated
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [x] `INITIAL_COMMANDS` reflects current implemented Rust commands.
- [ ] Deprecated Java-only commands are either listed as dropped or moved to this migration document.
- [x] `capabilities` output does not incorrectly label working Rust commands as `NotPortedYet`.
- [x] Command status update has tests or golden output checks.

## P1 Rust-Only Feature Parity

These items complete the Rust-only feature surface after P0 blockers are under control.

### `generate` production alias
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [ ] `generate` maps to vanilla-delegated parallel generation.
- [ ] Supports the same production defaults: `textureMode=photo`, `surfaceRaster=auto`, `chunkStatus=surface`.
- [ ] Help text and GUI resolved command prefer `generate` where appropriate.

### Vanilla delegated plan-parallel aliases
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered commands:
- [ ] `generate-vanilla-delegated-plan-parallel`
- [ ] `generate-vanilla-delegated-region-plan-parallel`
Detailed progress:
- [ ] Reads explicit non-contiguous region plans.
- [ ] Supports resume journal/fingerprint semantics.
- [ ] Preserves bounded parallel generation and progress events.

### Survival and OSM generation commands
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered commands:
- [ ] `generate-survival-region`
- [ ] `generate-survival-regions-parallel`
- [ ] `generate-survival-region-osm-synthetic`
- [ ] `generate-survival-regions-parallel-osm-synthetic`
- [ ] `generate-survival-region-osm-pbf`
- [ ] `generate-survival-region-osm-pbf-ref-window`
- [ ] `generate-survival-region-osm-pbf-full-scan`
- [ ] `generate-survival-region-osm-xml-cache`
- [ ] `generate-survival-region-plan-parallel`
- [ ] `generate-survival-region-plan-parallel-osm-synthetic`
Detailed progress:
- [ ] Decide whether legacy direct ecology generation is ported or dropped in favor of vanilla-delegated generation.
- [ ] Any dropped legacy path has a documented replacement command.
- [ ] OSM overlays do not slow the default no-OSM generation path.

### OSM scan, validate, and extract commands
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered commands:
- [ ] `scan-osm-pbf`
- [ ] `scan-osm-pbf-range`
- [ ] `validate-osm-pbf`
- [ ] `benchmark-osm-index`
- [ ] `extract-osm-region-mask`
- [ ] `extract-osm-region-mask-window`
- [ ] `extract-osm-region-mask-ref-window`
- [ ] `extract-osm-region-mask-full-scan`
- [ ] `extract-osm-xml-region-mask`
- [ ] `identify-osm-xml-cache`
Detailed progress:
- [ ] PBF scanning is streaming and bounded by blob limits.
- [ ] Mask extraction supports the existing region/window semantics.
- [ ] Benchmarks include a small PBF scan and a full-scan workload.

### Gameplay and finalization validators
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered commands:
- [ ] `validate-global-resource-fairness`
- [ ] `validate-loot-economy`
- [ ] `validate-survival-manifest`
- [ ] `apply-survival-evidence`
- [ ] `validate-cave-density`
- [ ] `validate-cave-connectivity`
- [ ] `validate-ore-histogram-synthetic`
- [ ] `validate-underground-fluid-synthetic`
Detailed progress:
- [ ] Validators read generated artifacts directly without Java.
- [ ] Reports preserve fields consumed by finalization scripts.
- [ ] Benchmarks focus on validator wall time and peak memory.

### Nation-war readiness and finalization reports
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered commands:
- [ ] `generate-nation-war-readiness-report`
- [ ] `write-vanilla-finalization-commands`
Detailed progress:
- [ ] Report schema remains readable by existing acceptance gates or those gates are updated.
- [ ] Output commands target Rust/vanilla workflows, not Java scripts.

### Representative planning, earth-grid, spawn, and seam tools
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered commands:
- [ ] `plan-representative-regions`
- [ ] `describe-earth-grid`
- [ ] `validate-surface-spawn`
- [ ] `validate-height-seam`
Detailed progress:
- [ ] Representative planning remains deterministic enough for repeatable gates.
- [ ] Grid/spawn/seam outputs keep stable fields for scripts.

### Benchmark commands converted to Rust-only
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Covered commands:
- [ ] `benchmark-height-regions`
- [ ] `benchmark-surface-regions`
- [ ] `benchmark-survival-regions`
- [ ] `benchmark-survival-regions-parallel`
- [ ] `benchmark-region-writers`
Detailed progress:
- [ ] Benchmarks include cold-start and warm-loop variants.
- [ ] Benchmarks record machine/runtime metadata.
- [ ] Benchmarks do not rely on Java once Rust replacement acceptance begins.

## P2 Java Removal Cleanup

These items happen after P0 and P1 are accepted.

### Replace `scripts/run.ps1` Java path
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [ ] Decide whether root `scripts/run.ps1` becomes a Rust wrapper or is removed.
- [ ] Existing script callers are migrated to the chosen Rust wrapper.
- [ ] Help output points to Rust CLI.
- [ ] Normal command examples no longer compile or launch Java.

### Replace Java build/test scripts or archive them
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [ ] Root Java `scripts/build.ps1` is removed, archived, or made non-normal-path.
- [ ] Root Java `scripts/test.ps1` is removed, archived, or made non-normal-path.
- [ ] Rust `cargo test --workspace` is the default verification path.
- [ ] Any remaining historical Java fixture generation is clearly marked archived/reference-only.

### Remove Java-only docs from the operations path
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [ ] `docs/OPERATIONS.md` documents Rust-only prerequisites and commands.
- [ ] `docs/QUALITY-GATES.md` documents Rust-only gate commands.
- [ ] `docs/ARCHITECTURE.md` no longer describes JVM reuse as an active production feature.
- [ ] Historical experiment docs are either left as history or moved under an archived-reference note.

### Remove "Java oracle/fallback" wording
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [ ] Rust CLI help no longer says Java is the compatibility oracle or fallback.
- [ ] Docs no longer tell users to use Java for normal validation.
- [ ] Any remaining Java mentions are explicitly historical or migration-only.

### Add forbidden Java runtime reference gate
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [ ] Add a script or documented command that searches for forbidden Java runtime references.
- [ ] Allowlist only archived migration notes and historical experiment records.
- [ ] Gate fails if normal scripts call `java`, `javac`, `scripts/run.ps1`, or `net.earthmap.cli`.
- [ ] Gate is included in final Java deletion verification.

### Delete Java sources, tests, vendor files, and runtime files
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.
Detailed progress:
- [ ] Delete `src/main/java`.
- [ ] Delete `src/test/java`.
- [ ] Delete Java-only `vendor` runtime files if no Rust workflow uses them.
- [ ] Delete or archive root Java build artifacts.
- [ ] Verify deletion diff has no broken docs, scripts, or release commands.

## Final Java Deletion Gates

- [ ] Rust-only MCA preview render accepted.
- [ ] Rust-only Linear preview render accepted.
- [ ] Rust-only quality production sample batch accepted.
- [ ] Rust-only GUI and CLI generation work on a machine without Java installed.
- [ ] Cold-start benchmark table is filled for every P0 tool.
- [ ] Every accepted Rust replacement is faster than the Java tool it replaces.
- [ ] `cargo test --workspace` passes.
- [ ] Release builds for CLI and GUI pass.
- [ ] `rg "java|javac|scripts\\run.ps1|net\\.earthmap\\.cli"` has no normal runtime references outside archived migration notes.
- [ ] Java source/test/vendor/runtime deletion has a clean diff and no broken docs/scripts.
