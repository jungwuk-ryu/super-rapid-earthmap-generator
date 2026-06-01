# Decisions

## 2026-06-01: Project State Reset

- Full world generation is NO-GO.
- v123 is evidence-only and must not be treated as a pass.
- The first priority is faster validation, not more surface heuristics.
- Generated junk is archived under `D:\earthmap\archive\super-rapid-reset-20260601` before any deletion.

## 2026-06-01: Test Loop

- `scripts/test.ps1` defaults to one JVM through `net.earthmap.tests.TestSuiteRunner`.
- Per-class JVM execution remains available as `-Isolated`.

## 2026-06-01: Quality Runtime

- `prefetchRows=0` is the quality-sample default.
- PowerShell remains the outer orchestration layer.
- Node.js is only considered if it helps call a one-JVM batch/worker flow.

## 2026-06-01: Photo Solver Boundary

- `PhotoSurfaceSolver` is the shared decision contract.
- `PhotoSurfaceMaterialClassifier.apply` is now a compatibility wrapper over the solver decision.
