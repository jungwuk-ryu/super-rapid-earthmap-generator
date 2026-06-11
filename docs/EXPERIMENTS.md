# Experiments

Newest first. Keep this file short; long logs belong under `<EARTHMAP_OUTPUT_ROOT>/agent-runs`,
`<EARTHMAP_OUTPUT_ROOT>/archive`, or the matching `out/` subdirectory.

Historical entries may mention render carriers such as terracotta, black concrete, or leaves. Those entries are metric
research only for production purposes: the 2026-06-01 Natural Surface Contract in `docs/DECISIONS.md` supersedes them
for generated worlds. Such blocks may remain harness-only, but must not be emitted as terrain ground.

## v145 Arabia arid luma-guard rejection

- Path: `D:\earthmap\quality\photo-parity\v145-arabia-arid-luma-guard-natural`
- Status: diagnostic evidence only; run used `-NoQualityGate`; patch rejected and removed.
- Result: a narrow luma-overshoot guard for arid Java Standard carriers completed in `99896ms`.
- Quality: raw Arabia worsened versus v143: mean `7.284661`, p95 `10.637097`, SSIM `0.927337668`;
  local-average mean `6.215258`, p95 `9.497752`, SSIM `0.939224305`.
- Finding: simple high-luma carrier rejection does not fix the dry-coast blocker. It increases raw mean error and
  leaves the same SSIM failure, so the next fix must address source-preserving arid texture behavior or gate design.

## v144 Arabia v143 carrier-remap ceiling

- Path: `D:\earthmap\quality\photo-parity\v144-arabia-v143-carrier-sim`
- Status: diagnostic simulation only; not production evidence.
- Result: best-positive all-carrier remap on the v143 MCA render improved mean to `6.766713` and p95 to `9.531088`,
  but reduced SSIM to `0.922703576`.
- Finding: carrier swaps alone are not enough for Arabia. The largest positive buckets were `#E6CDA0` to
  `smooth_sandstone`, `#FFFFBE` to `end_stone`, and `#BE9678` to `white_terracotta`, but the simulated output still
  fails the current raw quality gate.

## v143 Arabia production source-debug/no-concrete diagnostic

- Path: `D:\earthmap\quality\photo-parity\v143-arabia-source-debug-natural`
- Status: diagnostic evidence only; run used `-NoQualityGate`.
- Result: fresh single-sample `quality-production-sample-batch ... metricMode=current-only previewDebug=auto`
  completed in `118669ms`, wrote direct MCA render, contact sheet, preview debug tiles, and production-source
  comparison metrics.
- Quality: raw Arabia worsened after the no-concrete production guard: mean `7.249508`, p95 `10.637097`,
  SSIM `0.928168149`; local-average mean `6.179873`, p95 `9.497752`, SSIM `0.939768495`.
- Finding: current production `source-color` is byte-equivalent to the v113 reference source for this crop
  (`productionSourceVsReferenceMean=0.000000`, SSIM `1.000000000`), so the Arabia blocker is not stale source input.
  The remaining issue is natural material/render selection: stats show `black_concrete=0`, while broad bright carriers
  such as `bone_block`, `end_stone_bricks`, `smooth_sandstone`, and `chiseled_sandstone` make current land paler than
  source/expected.

## v142 Arabia current-only local-average fast-loop check

- Path: `D:\earthmap\quality\photo-parity\v142-arabia-current-only-local-average`
- Status: accepted as fast-loop tooling evidence; rejected as production quality evidence.
- Result: fresh single-sample `quality-production-sample-batch ... metricMode=current-only` completed in `96986ms`,
  wrote summary CSV, contact sheet, direct MCA render, and current-only metrics with local-average sections.
- Quality: raw Arabia still fails mean `7.039465`, p95 `10.637097`, SSIM `0.935872106`; current 4x4 local-average is
  mean `6.028665`, p95 `9.497752`, SSIM `0.947095865`, dE>20 `0`.
- Finding: the fast batch wrapper now emits raw metrics, local-average metrics, and source baseline in one pass, then
  exits nonzero on the current raw gate failure. This preserves release-gate integrity while giving the evidence needed to evaluate a
  future dry-crop gate revision.

## v141 arid/Mediterranean full metric diagnostics

- Path: `D:\earthmap\quality\photo-parity\v141-arid-full-metric`
- Status: diagnostic evidence plus gate-tooling verification.
- Result: full metric reports were generated for the v139 Sahara, Arabia, and Mediterranean direct-MCA outputs. The
  wrapper proxy check now fails v139 with 12 metric failures and passes the v138 Congo/Europe metrics before rejecting
  `-SkipGeneration` as non-release evidence.
- Quality finding: current 4x4 local-average metrics are much closer than raw metrics: Sahara mean `6.160885`, p95
  `9.382034`, SSIM `0.973491307`; Arabia mean `6.028665`, p95 `9.497752`, SSIM `0.947095865`; Mediterranean mean
  `6.145245`, p95 `9.687518`, SSIM `0.971498842`.
- Finding: Sahara and Mediterranean look more like raw texture/luma-retention issues than mean-color failures. Arabia
  is a gate-design risk because source-vs-expected SSIM is only `0.942544518`, so a universal raw SSIM `0.970` gate is
  not proving what it intends on that dry coast crop.

## v140 arid/Mediterranean carrier-ceiling simulations

- Paths: `D:\earthmap\quality\photo-parity\v140-arid-carrier-sim`,
  `D:\earthmap\quality\photo-parity\v140-arid-carrier-sim-top100`
- Status: diagnostic evidence.
- Result: all-carrier best-positive remaps on v139 failures improved mean DeltaE but did not clear the current raw
  gate: Sahara mean `6.738081`, SSIM `0.969802687`; Arabia mean `6.556716`, SSIM `0.931505364`; Mediterranean mean
  `6.997065`, SSIM `0.963563981`.
- Finding: the arid/Mediterranean blocker is not a single carrier mapping. The current raw gate is also questionable
  for dry coast samples because Arabia source-vs-expected SSIM is only `0.942544518`, while the best-positive carrier
  simulation lowers SSIM further.

## v139 five land-crop current-only production batch

- Path: `D:\earthmap\quality\photo-parity\v139-five-land-current-only`
- Status: accepted as fast-loop evidence; rejected as production quality evidence.
- Command: `run-quality-acceptance-samples.ps1 -ProductionSamplesCsv ...`, which calls
  `quality-production-sample-batch ... metricMode=current-only`.
- Result: completed without timeout in `918572ms`, wrote direct MCA renders, metrics, summary CSV, and contact sheet
  for Sahara, Arabia, Mediterranean, Congo, and Europe.
- Quality: Sahara mean `7.110292`, p95 `11.168225`, SSIM `0.967881164`; Arabia mean `7.039465`, p95 `10.637097`,
  SSIM `0.935872106`; Mediterranean mean `7.580863`, p95 `11.460210`, SSIM `0.961445262`, dE>20 `0.130922%`;
  Congo and Europe stayed green at mean `6.384181`/`6.275733` and SSIM `0.971011062`/`0.971263801`.
- Finding: v138's targeted canopy/temperate fix does not generalize to arid/Mediterranean land crops. Visual review
  shows broad pale/flat carriers in Sahara, Arabia, and Mediterranean relative to the Standard reference.

## v138 temperate dark-shadow mix production batch

- Path: `D:\earthmap\quality\photo-parity\v138-temperate-dark-shadow-mix-current-only`
- Status: accepted as targeted numeric production evidence; not release evidence.
- Command: `quality-production-sample-batch ... metricMode=current-only`
- Result: completed without timeout in `404732ms`, wrote direct MCA renders, metrics, summary CSV, and contact sheet.
- Quality: Congo mean `6.384181`, p95 `10.107271`, SSIM `0.971011062`, dE>20 `0.002430%`; Europe mean `6.275733`,
  p95 `9.261911`, SSIM `0.971263801`, dE>20 `0.011772%`.
- Finding: the Congo/Europe targeted numeric thresholds are green. Europe `current-vs-source` worsened to mean
  `9.644926`, so broader visual review must watch for over-dark temperate forest texture before release promotion.

## v137 Europe dark-token mix simulation

- Path: `D:\earthmap\quality\photo-parity\v137-europe-dark-token-mix-sim`
- Status: diagnostic evidence.
- Result: Bayer mixing Java Standard `#141414` with black terracotta found the viable range. 10/16 produced mean
  `6.150156`, p95 `9.261911`, SSIM `0.970134538`; 11/16 produced mean `6.274607`, p95 `9.261911`, SSIM
  `0.971327766`.
- Finding: 11/16 black terracotta gave the best targeted SSIM margin while still staying below the mean DeltaE limit;
  12/16 failed mean DeltaE.

## v136 Europe SSIM localization

- Path: `D:\earthmap\quality\photo-parity\v136-europe-ssim-localization`
- Status: diagnostic evidence.
- Result: current Europe SSIM was `0.9633056166961381`; all best-positive bucket remaps reached
  `0.9693720683439218`.
- Finding: `#323C1E`, `#374632`, and `#5F644B` were the best single-remap SSIM gains, but the dark `#141414` bucket
  had the key luma gap: current mean luma near `9.96` against expected `20.0`.

## v135 v134 all-carrier remap simulations

- Paths: `D:\earthmap\quality\photo-parity\v135-europe-v134-all-carrier-sim`,
  `D:\earthmap\quality\photo-parity\v135-congo-v134-all-carrier-sim`
- Status: diagnostic evidence.
- Result: carrier-only best-positive remaps would reach Congo mean `6.127097`, SSIM `0.973968313`, dE>20 `0`; Europe
  mean `4.691803`, SSIM `0.969372068`, dE>20 `0.011772%`.
- Finding: Europe remains just under a raw SSIM `0.970` style gate even in carrier-only simulation; the remaining
  blocker is biome-cell tint/raw-SSIM behavior rather than a single high-confidence carrier patch.

## v134 dark-green biome-cell fallback production batch

- Path: `D:\earthmap\quality\photo-parity\v134-green-cell-fallback-current-only`
- Status: accepted as targeted production evidence; rejected as release evidence.
- Command: `quality-production-sample-batch ... metricMode=current-only`
- Result: completed without timeout in `436650ms`, wrote direct MCA renders, metrics, summary CSV, and contact sheet.
- Quality: Congo mean `6.384181`, p95 `10.107271`, SSIM `0.971011062`, dE>20 `0.002430%`; Europe mean `4.904174`,
  p95 `7.380276`, SSIM `0.963305617`, dE>20 `0.011772%`.
- Finding: the gray/brown static fallback was removed from dark-green leaf tint cells, but Europe raw SSIM still fails.

## v133 v132 all-carrier remap simulations

- Paths: `D:\earthmap\quality\photo-parity\v133-europe-all-carrier-sim`,
  `D:\earthmap\quality\photo-parity\v133-congo-all-carrier-sim`
- Status: diagnostic evidence.
- Result: best-positive remap estimated Congo mean `6.127097`, SSIM `0.973968313`; Europe mean `4.691803`, SSIM
  `0.969372068`.
- Finding: useful carrier targets were confirmed for `#323C1E`, `#374632`, `#5F644B`, and related buckets, but the
  Europe SSIM ceiling stayed below strict raw pass.

## v132 dark Java Standard shadow concrete production batch

- Path: `D:\earthmap\quality\photo-parity\v132-dark-standard-concrete-current-only`
- Status: accepted as targeted production evidence; rejected as release evidence.
- Command: `quality-production-sample-batch ... metricMode=current-only`
- Result: completed without timeout, wrote contact sheet and direct MCA evidence.
- Quality: Congo mean `6.393296`, p95 `10.107271`, SSIM `0.971074251`, dE>20 `0.072903%`; Europe mean `4.925300`,
  p95 `7.380276`, SSIM `0.963613499`, dE>20 `0.173042%`.
- Finding: black concrete restored dark Java Standard shadow fidelity, but residual gray terracotta and biome-cell tint
  errors remained.

## v131 dark carrier simulation

- Path: `D:\earthmap\quality\photo-parity\v131-dark-carrier-sim-candidates.csv`
- Status: diagnostic evidence.
- Result: replacing dark Standard shadow carriers with black concrete was strongly positive for both targeted samples.
- Finding: the patch was narrow enough to promote into production code, unlike the broad v129 dark-green carrier bias.

## v130 Europe v128 debug probe

- Path: `D:\earthmap\quality\photo-parity\v130-europe-v128-baseline-debug`
- Status: diagnostic evidence.
- Finding: debug `surface.png` had no gray terracotta, while final MCA topdown still exposed sparse gray terracotta
  pixels. This isolated a final biome-cell/static-carrier interaction later addressed by v134.

## v129 broad dark-green Standard carrier

- Path: `D:\earthmap\quality\photo-parity\v129-dark-green-standard-carrier`
- Status: rejected.
- Result: worsened both targeted samples, so the patch was reverted.

## v128 no canopy-shadow production hook

- Path: `D:\earthmap\quality\photo-parity\v128-no-canopy-shadow-current-only`
- Status: accepted as targeted production evidence; rejected as release evidence.
- Result: Congo mean `7.160911`, p95 `10.107271`, SSIM `0.972906251`; Europe mean `7.307467`, p95 `10.649828`,
  SSIM `0.965066399`.
- Finding: removing the canopy-shadow hook fixed the largest regression, but strict quality thresholds still failed.

## v127 Europe debug source/parity probe

- Path: `D:\earthmap\quality\photo-parity\v127-europe-debug-source`
- Status: diagnostic evidence.
- Result: generated Europe region with `previewDebugDir` to compare production input and rendered debug artifacts.
- Finding: debug `source-color.png` exactly matches the v123 reference source over the land mask, so the Europe failure
  is not a source-raster alignment issue.
- Finding: debug `surface.png` and direct MCA current crop are close (mean `0.994640`), so the large miss is not a
  top-down renderer issue.
- Production/harness gap: direct MCA current vs v124 `candidate-production-solver.png` is mean `8.133820`,
  p95 `28.053637` on Europe. The remaining bottleneck is classifier/production application parity, not input.

## v126 Congo/Europe current-only production batch

- Path: `D:\earthmap\quality\photo-parity\v126-congo-europe-current-only-contact-sheet`
- Status: accepted as fast-loop/tooling evidence; rejected as production quality evidence.
- Command: `quality-production-sample-batch ... metricMode=current-only`
- Result: completed without timeout, wrote direct MCA renders, per-sample metrics, summary CSV, and contact sheet.
- Runtime: Congo `165326ms` total with `1380ms` metrics; Europe `238578ms` total with `1399ms` metrics.
- Quality: Congo current-vs-expected mean `8.498522`, p95 `21.148797`, SSIM `0.943675780`; Europe mean
  `10.541579`, p95 `21.148797`, SSIM `0.806034548`. Both fail the production thresholds.

## v124 Congo/Europe full-metric production batch

- Path: `D:\earthmap\quality\photo-parity\v124-congo-europe-production-batch`
- Status: rejected as gate evidence.
- Reason: the outer command timed out; timeout makes the artifacts invalid as release evidence.
- Result: full candidate-harness metrics were the immediate runtime bottleneck: Congo `299036ms`, Europe `383825ms`.

## v123 production canopy shadow hook

- Path: `D:\earthmap\quality\photo-parity\v123-production-canopy-shadow-hook`
- Status: rejected as release evidence.
- Reason: skipped thresholds, partial/timeout history, missing complete same-candidate pass.

## v122 Standard remap parity

- Path: `D:\earthmap\quality\photo-parity\v122-standard-remap-parity`
- Status: accepted as toolchain evidence only.
- Result: Java Standard remap matches ImageMagick remap on the required crops.
- Limitation: does not prove generated-world visual quality.

## v121 selective canopy harness

- Path: `D:\earthmap\quality\photo-parity\v121-selective-canopy-harness`
- Status: useful hypothesis, not production proof.
- Result: source parity improved on the five land crops.
- Limitation: Standard-target metrics regressed, and the candidate was harness-only.
