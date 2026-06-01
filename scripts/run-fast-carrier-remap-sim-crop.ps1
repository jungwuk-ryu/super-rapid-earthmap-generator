param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [Parameter(Mandatory = $true)]
    [string]$CurrentSurfacePng,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedPng,
    [Parameter(Mandatory = $true)]
    [string]$OutputDir,
    [int]$CropX = 0,
    [int]$CropY = 0,
    [int]$CropWidth = 512,
    [int]$CropHeight = 512,
    [string]$MaskPng = 'none',
    [ValidateSet('all', 'nonzero', 'white', 'land-water-debug')]
    [string]$MaskMode = 'nonzero',
    [string]$CarrierStrategy = 'all',
    [int]$TopBuckets = 8,
    [switch]$Build
)

$ErrorActionPreference = 'Stop'

if ($Build) {
    & (Join-Path $ProjectRoot 'scripts\build.ps1') -ProjectRoot $ProjectRoot
}

$mainOut = Join-Path $ProjectRoot 'build\classes\main'
if (-not (Test-Path -LiteralPath (Join-Path $mainOut 'net\earthmap\cli\EarthMapCli.class'))) {
    throw "EarthMapCli.class was not found. Run with -Build or execute scripts\build.ps1 first."
}

$classpathEntries = @($mainOut)
$vendorLib = Join-Path $ProjectRoot 'vendor\lib'
if (Test-Path -LiteralPath $vendorLib) {
    $classpathEntries += Get-ChildItem -LiteralPath $vendorLib -Filter '*.jar' |
        Sort-Object FullName |
        ForEach-Object { $_.FullName }
}
$classpath = $classpathEntries -join [IO.Path]::PathSeparator

$timer = [Diagnostics.Stopwatch]::StartNew()
java "--enable-native-access=ALL-UNNAMED" -cp $classpath net.earthmap.cli.EarthMapCli `
    photo-carrier-remap-sim-crop `
    $CurrentSurfacePng `
    $ExpectedPng `
    $OutputDir `
    $CropX `
    $CropY `
    $CropWidth `
    $CropHeight `
    $MaskPng `
    $MaskMode `
    $CarrierStrategy `
    $TopBuckets
$exitCode = $LASTEXITCODE
$timer.Stop()

Write-Host ("fastCarrierRemapSimCrop.elapsedSeconds={0:N3}" -f $timer.Elapsed.TotalSeconds)
if ($exitCode -ne 0) {
    throw "photo-carrier-remap-sim-crop failed with exit code $exitCode."
}
