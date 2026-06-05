# Operations

## Prerequisites

- JDK 25 on `PATH`
- PowerShell 7 or Windows PowerShell capable of running the scripts
- `C:\earth_map_resources\HQheightmap.tif`
- `D:\earthmap\TifFiles\terrain\TrueMarble.vrt` or another explicit `surfaceRaster` path
- ImageMagick only for workflows that produce Standard-remap reference images outside Java

## Normal Commands

```powershell
.\scripts\build.ps1
.\scripts\test.ps1
.\scripts\run.ps1 --help
```

Fast filtered test:

```powershell
.\scripts\test.ps1 -Filter PhotoSurface
```

Isolated legacy test mode:

```powershell
.\scripts\test.ps1 -Isolated
```

## Quality Sample Batch

Prepare a CSV with:

```text
sample,regionX,regionZ,cropX,cropY,cropWidth,cropHeight,sourcePng,expectedStandardPng,landMaskPng
```

Run:

```powershell
.\scripts\run.ps1 quality-production-sample-batch samples.csv C:\earth_map_resources\HQheightmap.tif `
  D:\earthmap\quality\photo-parity\vNEXT 5000 mca 1 `
  cacheRows=512 prefetchRows=0 verticalScale=1.25 textureMode=photo surfaceRaster=auto chunkStatus=surface `
  metricMode=current-only previewDebug=off
```

The command writes per-sample worlds under `<outputRoot>/<sample>/world` and direct MCA parity evidence under
`<outputRoot>/<sample>/photo-parity`. It also writes `<outputRoot>/quality-production-sample-summary.csv` and
`<outputRoot>/quality-production-sample-contact-sheet.png`.

Use `metricMode=current-only` for the fast production loop. The default `metricMode=full` keeps candidate-harness
comparisons available, but it is intentionally slower and should not be used as the timed production proof path.
Use `previewDebug=auto` only for diagnostics; it writes same-run debug tiles and compares production `source-color`
against the reference source and expected Standard crop.

The PowerShell acceptance wrapper can call the batch command when an explicit job CSV exists:

```powershell
.\scripts\run-quality-acceptance-samples.ps1 -ProductionSamplesCsv .\samples.csv -OutputRoot D:\earthmap\quality\photo-parity\vNEXT
```

In this batch mode the wrapper passes `metricMode=current-only` automatically, so it stays on the fast production proof
path instead of the slower candidate-harness research path. The wrapper also validates each sample's
`photo-parity/metric-land/metrics.txt` against the current targeted thresholds and exits nonzero when any sample fails.
Fresh current-only metric reports include raw current-vs-expected, source-vs-expected, current-vs-source, and 4x4
local-average diagnostic sections.
Add `-ProductionPreviewDebug auto` when investigating whether same-run production `source-color` differs from the
reference source.
Use `-NoQualityGate` only for research evidence. `-SkipGeneration` can proxy-check existing artifacts, but it is always
reported as NO-GO for release evidence.

## Recovery

- Generated junk inside the repo should be archived, not deleted first.
- The current reset archive is `D:\earthmap\archive\super-rapid-reset-20260601`.
- If a generation process is suspected to be stuck, inspect Java command lines before stopping anything.
