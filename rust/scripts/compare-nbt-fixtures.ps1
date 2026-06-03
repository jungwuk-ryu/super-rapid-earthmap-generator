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
    $OutputRoot = Join-Path $workspaceRoot ("rust-port-golden\nbt-byte-fixtures-{0}" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
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

$javaDir = Join-Path $OutputRoot 'java'
$rustDir = Join-Path $OutputRoot 'rust'
Remove-Item -LiteralPath $OutputRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $javaDir, $rustDir | Out-Null

& $projectRun -ProjectRoot $ProjectRoot write-nbt-parity-fixtures $javaDir
& $rustRun -RustRoot $RustRoot write-nbt-parity-fixtures $rustDir

$fixtureNames = @(
    'nbt-primitive-root.dat',
    'nbt-nested-empty-root.dat',
    'chunk-empty-full.nbt',
    'chunk-mixed-biome.nbt',
    'chunk-block-entities.nbt',
    'chunk-all-block-states.nbt',
    'leveldat-root-fixed.nbt'
)

$rows = New-Object System.Collections.Generic.List[string]
$rows.Add('fixture,bytes,sha256')
foreach ($fixtureName in $fixtureNames) {
    $javaFile = Join-Path $javaDir $fixtureName
    $rustFile = Join-Path $rustDir $fixtureName
    if (!(Test-Path -LiteralPath $javaFile)) {
        throw "Java fixture missing: $javaFile"
    }
    if (!(Test-Path -LiteralPath $rustFile)) {
        throw "Rust fixture missing: $rustFile"
    }
    $javaHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $javaFile).Hash
    $rustHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $rustFile).Hash
    if ($javaHash -ne $rustHash) {
        throw "NBT fixture mismatch: $fixtureName java=$javaHash rust=$rustHash"
    }
    $bytes = (Get-Item -LiteralPath $javaFile).Length
    $rows.Add(('{0},{1},{2}' -f $fixtureName, $bytes, $javaHash))
}

$summary = Join-Path $OutputRoot 'nbt-fixture-sha256.csv'
$rows | Set-Content -LiteralPath $summary -Encoding UTF8
Write-Output "NBT fixture byte parity OK"
Write-Output "outputRoot=$OutputRoot"
Write-Output "summary=$summary"
