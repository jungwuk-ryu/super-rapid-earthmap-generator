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
  cacheRows=512 prefetchRows=0 verticalScale=1.25 textureMode=photo surfaceRaster=auto chunkStatus=surface
```

The command writes per-sample worlds under `<outputRoot>/<sample>/world` and direct MCA parity evidence under
`<outputRoot>/<sample>/photo-parity`.

The PowerShell acceptance wrapper can call the batch command when an explicit job CSV exists:

```powershell
.\scripts\run-quality-acceptance-samples.ps1 -ProductionSamplesCsv .\samples.csv -OutputRoot D:\earthmap\quality\photo-parity\vNEXT
```

## Recovery

- Generated junk inside the repo should be archived, not deleted first.
- The current reset archive is `D:\earthmap\archive\super-rapid-reset-20260601`.
- If a generation process is suspected to be stuck, inspect Java command lines before stopping anything.
