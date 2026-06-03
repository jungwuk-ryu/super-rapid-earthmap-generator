[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$RustRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [string]$OutputRoot = ''
)

$ErrorActionPreference = 'Stop'

$trimPathChars = [char[]]@(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
)

function Normalize-FullPathNoTrailingSeparator([string]$Path) {
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $trimmed = $fullPath.TrimEnd($trimPathChars)
    if ([string]::IsNullOrEmpty($trimmed)) {
        return $fullPath
    }
    $trimmed
}

$workspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$goldenRoot = Normalize-FullPathNoTrailingSeparator (Join-Path $workspaceRoot 'rust-port-golden')
$projectRun = Join-Path $ProjectRoot 'scripts\run.ps1'
$rustRun = Join-Path $RustRoot 'scripts\run.ps1'
if (!(Test-Path -LiteralPath $projectRun -PathType Leaf)) {
    throw "Java run wrapper missing: $projectRun"
}
if (!(Test-Path -LiteralPath $rustRun -PathType Leaf)) {
    throw "Rust run wrapper missing: $rustRun"
}

if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $workspaceRoot ("rust-port-golden\region-writer-fixtures-{0}" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
}

$OutputRoot = Normalize-FullPathNoTrailingSeparator $OutputRoot
$outputRootParentPrefix = $goldenRoot + [System.IO.Path]::DirectorySeparatorChar
if (!$OutputRoot.StartsWith($outputRootParentPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "OutputRoot must be a child of $goldenRoot"
}
$outputDriveRoot = Normalize-FullPathNoTrailingSeparator ([System.IO.Path]::GetPathRoot($OutputRoot))
if ($OutputRoot -eq $goldenRoot -or $OutputRoot -eq $outputDriveRoot) {
    throw "Refusing to delete dangerous OutputRoot: $OutputRoot"
}

function Invoke-Checked([scriptblock]$Command, [string]$Description) {
    $output = & $Command 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "$Description failed: $output"
    }
    $output
}

$javaDir = Join-Path $OutputRoot 'java'
$rustDir = Join-Path $OutputRoot 'rust'
$manifestDir = Join-Path $OutputRoot 'manifests'
Remove-Item -LiteralPath $OutputRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $javaDir, $rustDir, $manifestDir | Out-Null

Invoke-Checked { & $projectRun -ProjectRoot $ProjectRoot write-region-writer-parity-fixtures $javaDir } `
    'Java region writer fixture generation' | Out-Null
Invoke-Checked { & $rustRun -RustRoot $RustRoot write-region-writer-parity-fixtures $rustDir } `
    'Rust region writer fixture generation' | Out-Null

$fixtures = @(
    @{ Name = 'writer-mca/r.0.0.mca'; Format = 'mca' },
    @{ Name = 'writer-linear/r.-2.3.linear'; Format = 'linear' }
)

$allFileBytesMatch = $true
$rows = New-Object System.Collections.Generic.List[string]
$rows.Add('fixture,format,javaBytes,rustBytes,javaSha256,rustSha256,fileByteIdentical,payloadsMatch,delta')
foreach ($fixture in $fixtures) {
    $fixtureName = $fixture.Name
    $javaFile = Join-Path $javaDir $fixtureName
    $rustFile = Join-Path $rustDir $fixtureName
    if (!(Test-Path -LiteralPath $javaFile)) {
        throw "Java region fixture missing: $javaFile"
    }
    if (!(Test-Path -LiteralPath $rustFile)) {
        throw "Rust region fixture missing: $rustFile"
    }

    $javaHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $javaFile).Hash
    $rustHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $rustFile).Hash
    $fileByteIdentical = $javaHash -eq $rustHash
    $payloadsMatch = $true
    if (!$fileByteIdentical) {
        $allFileBytesMatch = $false
        $safeName = $fixtureName.Replace('/', '-').Replace('\', '-')
        $manifest = Join-Path $manifestDir "$safeName-java-payloads.csv"
        Invoke-Checked { & $rustRun -RustRoot $RustRoot write-region-payload-manifest $javaFile $manifest } `
            "Java payload manifest for $fixtureName" | Out-Null
        Invoke-Checked { & $rustRun -RustRoot $RustRoot compare-region-payload-manifest $manifest $rustFile } `
            "Rust payload comparison for $fixtureName" | Out-Null
    }

    $javaBytes = (Get-Item -LiteralPath $javaFile).Length
    $rustBytes = (Get-Item -LiteralPath $rustFile).Length
    $delta = if ($fileByteIdentical) {
        'none'
    } elseif ($fixture.Format -eq 'mca') {
        'mca-zlib-stream'
    } elseif ($fixture.Format -eq 'linear') {
        'linear-zstd-stream'
    } else {
        'compression-or-container'
    }
    $rows.Add(('{0},{1},{2},{3},{4},{5},{6},{7},{8}' -f `
        $fixtureName,
        $fixture.Format,
        $javaBytes,
        $rustBytes,
        $javaHash,
        $rustHash,
        $fileByteIdentical,
        $payloadsMatch,
        $delta))
}

$summary = Join-Path $OutputRoot 'region-writer-fixture-summary.csv'
$rows | Set-Content -LiteralPath $summary -Encoding UTF8
if ($allFileBytesMatch) {
    Write-Output "Region writer fixture byte parity OK"
} else {
    Write-Output "Region writer fixture payload parity OK; file bytes differ"
}
Write-Output "outputRoot=$OutputRoot"
Write-Output "summary=$summary"
