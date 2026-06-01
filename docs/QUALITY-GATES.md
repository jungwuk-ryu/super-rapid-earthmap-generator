# Quality Gates

Status date: 2026-06-01

Current release state: **NO-GO for full world generation**.

## Current Evidence

- v121 selective canopy is harness-only. It improved source parity on the five land crops, but regressed Standard
  target metrics and is not production proof.
- v122 Standard remap parity proves Java remap matches ImageMagick `Standard.png`. It validates tooling, not final
  generated-world quality.
- v123 production canopy shadow hook is evidence-only. It used skipped gates, timed out, and must not be counted as a
  pass.

## Hard Rules

- Any `thresholdGateSkipped=true` is research evidence only.
- Any run using `-NoQualityGate`, `-PhotoParityEvidenceOnly`, `-SkipGeneration`, stale artifacts, missing samples, or
  timeout is NO-GO.
- Standard remap exact parity is not visual quality approval.
- Full generation requires fresh evidence from the same build/config.

## Promotion Tiers

1. **Tier 0: visual candidate loop**
   Produce source, Standard remap, current production render, candidate render, and error heatmaps.

2. **Tier 1: fast five-crop metrics**
   Run Sahara core, Arabia coast, Mediterranean edge, Congo edge, and Europe forest in one JVM where possible.

3. **Tier 2: targeted production proof**
   Run one or two production MCA samples without skipped gates. Congo/Europe are the first canopy targets.

4. **Tier 3: representative sample gate**
   Run default 1:5000 samples, then 1:1000 samples, only after Tier 2 passes.

5. **Tier 4: server/Dynmap gate**
   Finalize a validation world and inspect actual rendered output.

6. **Tier 5: full generation**
   Start only after all prior tiers pass and visual acceptance is recorded.

## Required Artifacts For Production Sample Pass

- `quality-production-sample.properties`
- `photo-parity/mca-visible-topdown.png`
- `photo-parity/metric-land/metrics.txt`
- source crop, expected Standard crop, current surface crop, mask preview when a mask is used
- batch summary CSV when run through `quality-production-sample-batch`
