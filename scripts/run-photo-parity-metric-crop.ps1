param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [Parameter(Mandatory = $true)]
    [string]$SourcePng,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedPng,
    [Parameter(Mandatory = $true)]
    [string]$CurrentSurfacePng,
    [Parameter(Mandatory = $true)]
    [string]$OutputDir,
    [int]$CropX = 0,
    [int]$CropY = 0,
    [int]$CropWidth = 512,
    [int]$CropHeight = 512,
    [string]$MaskPng = 'none',
    [ValidateSet('all', 'nonzero', 'white', 'land-water-debug')]
    [string]$MaskMode = 'nonzero',
    [switch]$Build
)

$ErrorActionPreference = 'Stop'

if ($Build) {
    & (Join-Path $ProjectRoot 'scripts\build.ps1') -ProjectRoot $ProjectRoot
}

$rustRoot = Join-Path $ProjectRoot 'rust'
$runScript = Join-Path (Join-Path $rustRoot 'scripts') 'run.ps1'
if (-not (Test-Path -LiteralPath $runScript)) {
    throw "Rust run wrapper not found: $runScript"
}

$timer = [Diagnostics.Stopwatch]::StartNew()
Write-Host ("photoParityMetricCrop.command={0} {1}" -f $runScript, 'photo-parity-metric-crop')
& $runScript `
    -RustRoot $rustRoot `
    photo-parity-metric-crop `
    $SourcePng `
    $ExpectedPng `
    $CurrentSurfacePng `
    $OutputDir `
    $CropX `
    $CropY `
    $CropWidth `
    $CropHeight `
    $MaskPng `
    $MaskMode
$exitCode = $LASTEXITCODE
$timer.Stop()

Write-Host ("photoParityMetricCrop.elapsedSeconds={0:N3}" -f $timer.Elapsed.TotalSeconds)
if ($exitCode -ne 0) {
    throw "photo-parity-metric-crop failed with exit code $exitCode."
}
