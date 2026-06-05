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
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.

### Rust Linear top-down renderer
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.

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

### Rust `dynmap-tile-mosaic`
- [ ] Spec: CLI args, outputs, exit codes, artifact paths documented
- [ ] Rust implementation exists
- [ ] Java is no longer called by normal workflow
- [ ] Correctness gate passes without byte-by-byte Java requirement
- [ ] Performance benchmark recorded: Java cold-start / Rust cold-start / speedup / peak memory
- [ ] Rust is faster than Java on the target workload
- [ ] Scripts and docs use Rust command
- [ ] Accepted for Java deletion
Benchmark: Java TBD; Rust TBD; speedup TBD; peak memory TBD; output TBD; workload TBD.

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
