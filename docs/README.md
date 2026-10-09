# Documentation Index

Status reviewed: 2026-06-12.

Use this file as the entry point for repository documentation. Completed one-off review documents and stale
compatibility pointers have been removed; archived external-version pins remain under `archived-reference/`.

## Current Operator Docs

- `OPERATIONS.md`: setup, normal commands, GUI usage, quality batch usage, and recovery notes.
- `QUALITY-GATES.md`: canonical quality gate and full-generation promotion workflow.
- `GENERATED-ARTIFACTS.md`: where generated worlds, logs, and quality artifacts belong.
- `EXTERNAL-DATA.md`: external raster/server/data files that must stay out of the repository.
- `run-folder-conventions.md`: output folder shapes for quality and agent-run evidence.

## Design And Policy Docs

- `ARCHITECTURE.md`: current Rust pipeline and structural boundaries.
- `module-rules.md`: current workspace crate ownership.
- `DECISIONS.md`: standing project decisions and production-surface policy.
- `LICENSE-REVIEW.md`: MIT license suitability review and boundaries.
- `survival-manifest-schema.md`: survival-complete manifest contract.

## Active Research Logs

- `LAND-GENERATION-PERFORMANCE.md`: real Korean land benchmarks, exact-output
  checks, thread/process comparison and reproduction commands.
- `EXPERIMENTS.md`: concise quality experiment history, newest first.
- `PERFORMANCE-OPTIMIZATION-TODO.md`: completed optimization notes plus active crash/CPU-utilization follow-up.
- `photo-nonlight-carriers.csv`: small data table used by photo-material research.

## Examples

- `survival-exploration-only.properties`
- `survival-complete-example.properties`

## Archived Reference

- `archived-reference/pins/`: pinned external server/version metadata retained as historical reference.
