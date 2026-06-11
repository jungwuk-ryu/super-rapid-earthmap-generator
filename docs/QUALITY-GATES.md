# Quality Gates

Status date: 2026-06-11

Current release state: **NO-GO for full world generation**.

2026-06-11 review: no newer same-build quality evidence has promoted the release state. Tooling gates now pass locally
and in CI, but production readiness still depends on the quality promotion tiers below.

## Current Evidence

- v121 selective canopy is harness-only. It improved source parity on the five land crops, but regressed Standard
  target metrics and is not production proof.
- v122 Standard remap parity proves Java remap matches ImageMagick `Standard.png`. It validates tooling, not final
  generated-world quality.
- v123 production canopy shadow hook is evidence-only. It used skipped gates, timed out, and must not be counted as a
  pass.
- v124 Congo/Europe full-metric batch produced direct MCA evidence, but the outer command timed out. The full
  candidate-harness metric phase dominated runtime and is not a production fast loop.
- v126 Congo/Europe `metricMode=current-only` batch completed without timeout and produced summary, direct MCA renders,
  metrics, and contact sheet. It restores the fast proof loop, but quality metrics fail the production thresholds.
- v128 removed the canopy-shadow production hook and improved both targeted samples, but still failed strict
  current-vs-expected metrics.
- v132 added dark Java Standard shadow concrete handling. It reduced Europe mean DeltaE to `4.925300`, but Europe raw
  SSIM was still `0.963613499` and dE>20 was `0.173042%`.
- v134 added dark-green biome-cell static fallback protection. It completed without timeout and reduced dE>20 to
  Congo `0.002430%` and Europe `0.011772%`; however Europe raw SSIM remains `0.963305617`, so the current release
  state remains NO-GO.
- v135 all-carrier simulation shows the remaining Europe raw SSIM ceiling from carrier-only remaps is `0.969372068`;
  the next blocker is biome-cell tint/raw-SSIM behavior, not a single obvious block carrier.
- v136 localized Europe SSIM loss. The main positive single-remap SSIM buckets were `#323C1E`, `#374632`, and
  `#5F644B`, while the dark `#141414` bucket exposed a luma gap rather than a DeltaE-positive carrier swap.
- v137 simulated dark-token mixing for Europe. A Bayer `#141414` mix with 10/16 or 11/16 black terracotta passed the
  raw SSIM threshold while keeping mean DeltaE below the targeted limit; 11/16 gave the stronger SSIM margin.
- v138 applies the temperate dark-shadow mix only for Java Standard `#141414` shadows at `abs(latitude) >= 35`.
  Congo/Europe direct-MCA `metricMode=current-only` completed without timeout in `404732ms` and passes the targeted
  numeric thresholds: Congo mean `6.384181`, p95 `10.107271`, SSIM `0.971011062`, dE>20 `0.002430%`; Europe mean
  `6.275733`, p95 `9.261911`, SSIM `0.971263801`, dE>20 `0.011772%`.
- v139 expanded the same candidate/config to the five land-crop fast batch using `metricMode=current-only`. The
  command completed without timeout in `918572ms`, but Sahara, Arabia, and Mediterranean failed the targeted numeric
  thresholds. Congo and Europe remained green.
- v140 all-carrier simulations on the v139 arid/Mediterranean failures show carrier swaps alone do not clear the
  current raw gate. Best-positive all-carrier remaps reached only Sahara mean `6.738081`, SSIM `0.969802687`; Arabia
  mean `6.556716`, SSIM `0.931505364`; Mediterranean mean `6.997065`, SSIM `0.963563981`.
- v141 full-metric arid/Mediterranean diagnostics separated raw pixel error from 4x4 local-average behavior. Current
  local-average metrics pass the mean/p95/dE limits for Sahara, Arabia, and Mediterranean, and pass SSIM for Sahara
  and Mediterranean; Arabia remains low but its source-vs-expected SSIM is also low. The production batch wrapper now
  enforces the current targeted metric gate in `-ProductionSamplesCsv` mode instead of returning success for failing
  batches.
- v142 confirmed the fast `metricMode=current-only` batch now writes local-average diagnostic sections. A fresh Arabia
  single-sample batch completed in `96986ms`, logged raw metrics, local-average metrics, and source baseline, then
  exited nonzero on the current raw gate failure.
- v143 added optional production `previewDebug=auto` source diagnostics and a production surface guard that prevents
  `black_concrete` from being emitted to MCA surface/filler columns. The fresh Arabia diagnostic proved the current
  production `source-color` exactly matches the v113 reference source (`productionSourceVsReferenceMean=0.000000`),
  so the dry-coast failure is not stale source input. With concrete removed, Arabia raw quality is still NO-GO:
  mean `7.249508`, p95 `10.637097`, SSIM `0.928168149`.
- v144 simulated best-positive carrier remaps on v143 Arabia. Mean improved to `6.766713`, but SSIM fell to
  `0.922703576`, so carrier swaps alone do not clear the raw gate.
- v145 tested a narrow arid luma-overshoot guard and rejected it. Arabia worsened to mean `7.284661`, p95
  `10.637097`, SSIM `0.927337668`; the patch was removed.

## Current Blocker

The fast production proof loop is restored, and Congo/Europe are green for v138/v139, but the same candidate/config
does not generalize to the five land-crop gate. The immediate blocker is arid/Mediterranean photo parity: Sahara mean
`7.110292`, p95 `11.168225`, SSIM `0.967881164`; Arabia mean `7.039465`, p95 `10.637097`, SSIM `0.935872106`;
Mediterranean mean `7.580863`, p95 `11.460210`, SSIM `0.961445262`, dE>20 `0.130922%`, and dE>30 `0.000494%`.
v143 rules out stale production source on Arabia and confirms `black_concrete` is not allowed in production MCA output.
v140/v144/v145 indicate this is not a one-carrier or one-luma-gate patch; fix source-preserving natural
arid/Mediterranean texture behavior or revise the raw gate with replacement evidence. Production readiness remains
NO-GO until this five-crop gate, representative samples, visual review, and server/Dynmap gate pass on the same
build/config.

## Hard Rules

- Any `thresholdGateSkipped=true` is research evidence only.
- Any run using `-NoQualityGate`, `-PhotoParityEvidenceOnly`, `-SkipGeneration`, stale artifacts, missing samples, or
  timeout is NO-GO.
- Standard remap exact parity is not visual quality approval.
- Full generation requires fresh evidence from the same build/config.
- `black_concrete` is a harness/render compatibility color only; production surface/filler generation must not emit it.
- Production surface/filler generation must not emit terracotta color carriers or leaf blocks as ground. Dark
  vegetation must come from biome tint plus real tree/canopy placement, not leaf carpets or green terracotta.
- Coastlines must visually read as natural shore material: sand, gravel, clay, mud, or stone-family blocks. Black/brown
  shore pixels are not allowed to become black/brown palette blocks.

## Promotion Tiers

1. **Tier 0: visual candidate loop**
   Produce source, Standard remap, current production render, candidate render, and error heatmaps.

2. **Tier 1: fast five-crop metrics**
   Run Sahara core, Arabia coast, Mediterranean edge, Congo edge, and Europe forest in one Rust batch where possible.

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
- batch contact sheet `quality-production-sample-contact-sheet.png`

Use `metricMode=current-only` for targeted production proof. Use `metricMode=full` or `photo-parity-metric-batch`
only when candidate-harness comparisons are intentionally being researched.
