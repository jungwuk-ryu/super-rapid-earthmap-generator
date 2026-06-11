# Quality Gates

Status date: 2026-06-12

Current release state: **full generation is allowed after the same build/config passes the gates below**.

## Gate Principles

- Fresh evidence is required for the exact build/config being promoted.
- Metrics are support evidence. Contact sheets, direct MCA/Linear topdown renders, and server/Dynmap review decide final
  promotion.
- Skipped quality gates, stale artifacts, missing samples, and timeouts are invalid release evidence.
- Standard remap exact parity validates tooling; it is not visual quality approval by itself.
- Production surface/filler generation must not emit concrete, terracotta color carriers, or leaf blocks as ground.
- Coastlines must visually read as natural shore material: sand, gravel, clay, mud, or stone-family blocks.

## Required Promotion Tiers

1. **Tier 0: visual candidate loop**
   Produce source, Standard remap, current production render, candidate render, and error heatmaps.

2. **Tier 1: fast five-crop metrics**
   Run Sahara core, Arabia coast, Mediterranean edge, Congo edge, and Europe forest in one Rust batch where possible.

3. **Tier 2: targeted production proof**
   Run one or two production MCA/Linear samples without skipped gates.

4. **Tier 3: representative sample gate**
   Run representative `1:5000` and `1:1000` samples on the same build/config.

5. **Tier 4: server/Dynmap gate**
   Finalize a validation world and inspect actual rendered output.

6. **Tier 5: full generation**
   Full generation may proceed after the prior tiers pass and visual acceptance is recorded.

## Required Artifacts For Production Sample Pass

- `quality-production-sample.properties`
- topdown render from generated MCA/Linear output
- `photo-parity/metric-land/metrics.txt`
- source crop, expected Standard crop, current surface crop, and mask preview when a mask is used
- batch summary CSV when run through `quality-production-sample-batch`
- batch contact sheet `quality-production-sample-contact-sheet.png`

Use `metricMode=current-only` for targeted production proof. Use `metricMode=full` or `photo-parity-metric-batch` only
when candidate-harness comparisons are intentionally being researched.

## Full Generation Rule

Full generation is a normal supported workflow once the active build/config has passed the promotion tiers. Keep the
exact command, environment variables, raster identities, output root, and summary artifacts with the release notes so the
run is reproducible.
