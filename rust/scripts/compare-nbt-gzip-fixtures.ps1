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
    $OutputRoot = Join-Path $workspaceRoot ("rust-port-golden\nbt-gzip-fixtures-{0}" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
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

function Get-FileSha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
}

function Get-BytesSha256([byte[]]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        (($sha.ComputeHash($Bytes) | ForEach-Object { $_.ToString('x2') }) -join '').ToUpperInvariant()
    } finally {
        $sha.Dispose()
    }
}

function Expand-GzipFile([string]$Path) {
    $inputStream = [System.IO.File]::OpenRead($Path)
    try {
        $gzipStream = New-Object System.IO.Compression.GZipStream(
            $inputStream,
            [System.IO.Compression.CompressionMode]::Decompress
        )
        try {
            $outputStream = New-Object System.IO.MemoryStream
            try {
                $gzipStream.CopyTo($outputStream)
                $outputStream.ToArray()
            } finally {
                $outputStream.Dispose()
            }
        } finally {
            $gzipStream.Dispose()
        }
    } finally {
        $inputStream.Dispose()
    }
}

function Test-ByteArrayEqual([byte[]]$Left, [byte[]]$Right) {
    if ($Left.Length -ne $Right.Length) {
        return $false
    }
    for ($i = 0; $i -lt $Left.Length; $i++) {
        if ($Left[$i] -ne $Right[$i]) {
            return $false
        }
    }
    return $true
}

function Get-ByteSlice([byte[]]$Bytes, [int]$Offset, [int]$Count) {
    if ($Count -lt 0) {
        return [byte[]]::new(0)
    }
    $slice = [byte[]]::new($Count)
    if ($Count -gt 0) {
        [Array]::Copy($Bytes, $Offset, $slice, 0, $Count)
    }
    $slice
}

function Get-GzipDeltaKind([byte[]]$JavaBytes, [byte[]]$RustBytes) {
    if (Test-ByteArrayEqual $JavaBytes $RustBytes) {
        return 'none'
    }
    if ($JavaBytes.Length -lt 18 -or $RustBytes.Length -lt 18) {
        return 'container-or-stream'
    }
    $headerSame = Test-ByteArrayEqual (Get-ByteSlice $JavaBytes 0 10) (Get-ByteSlice $RustBytes 0 10)
    $javaTrailerOffset = $JavaBytes.Length - 8
    $rustTrailerOffset = $RustBytes.Length - 8
    $trailerSame = Test-ByteArrayEqual `
        (Get-ByteSlice $JavaBytes $javaTrailerOffset 8) `
        (Get-ByteSlice $RustBytes $rustTrailerOffset 8)
    $deflateSame = Test-ByteArrayEqual `
        (Get-ByteSlice $JavaBytes 10 ($JavaBytes.Length - 18)) `
        (Get-ByteSlice $RustBytes 10 ($RustBytes.Length - 18))
    if ($deflateSame -and $trailerSame) {
        return 'header-only'
    }
    if ($headerSame -and $trailerSame) {
        return 'deflate-stream'
    }
    'container-or-stream'
}

$javaDir = Join-Path $OutputRoot 'java'
$rustDir = Join-Path $OutputRoot 'rust'
Remove-Item -LiteralPath $OutputRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $javaDir, $rustDir | Out-Null

& $projectRun -ProjectRoot $ProjectRoot write-nbt-gzip-parity-fixtures $javaDir
& $rustRun -RustRoot $RustRoot write-nbt-gzip-parity-fixtures $rustDir

$fixtureNames = @(
    'leveldat-root-fixed.nbt.gz'
)

$allCompressedBytesMatch = $true
$rows = New-Object System.Collections.Generic.List[string]
$rows.Add('fixture,javaCompressedBytes,rustCompressedBytes,javaCompressedSha256,rustCompressedSha256,compressedByteIdentical,decompressedBytes,decompressedSha256,gzipDelta')
foreach ($fixtureName in $fixtureNames) {
    $javaFile = Join-Path $javaDir $fixtureName
    $rustFile = Join-Path $rustDir $fixtureName
    if (!(Test-Path -LiteralPath $javaFile)) {
        throw "Java gzip fixture missing: $javaFile"
    }
    if (!(Test-Path -LiteralPath $rustFile)) {
        throw "Rust gzip fixture missing: $rustFile"
    }

    $javaCompressedHash = Get-FileSha256 $javaFile
    $rustCompressedHash = Get-FileSha256 $rustFile
    $javaCompressedBytes = [System.IO.File]::ReadAllBytes($javaFile)
    $rustCompressedBytes = [System.IO.File]::ReadAllBytes($rustFile)
    $compressedByteIdentical = $javaCompressedHash -eq $rustCompressedHash
    if (!$compressedByteIdentical) {
        $allCompressedBytesMatch = $false
    }

    $javaPayload = Expand-GzipFile $javaFile
    $rustPayload = Expand-GzipFile $rustFile
    $javaPayloadHash = Get-BytesSha256 $javaPayload
    $rustPayloadHash = Get-BytesSha256 $rustPayload
    if ($javaPayloadHash -ne $rustPayloadHash) {
        throw "NBT gzip payload mismatch: $fixtureName java=$javaPayloadHash rust=$rustPayloadHash"
    }

    $deltaKind = Get-GzipDeltaKind $javaCompressedBytes $rustCompressedBytes
    $rows.Add(('{0},{1},{2},{3},{4},{5},{6},{7},{8}' -f `
        $fixtureName,
        $javaCompressedBytes.Length,
        $rustCompressedBytes.Length,
        $javaCompressedHash,
        $rustCompressedHash,
        $compressedByteIdentical,
        $javaPayload.Length,
        $javaPayloadHash,
        $deltaKind))
}

$summary = Join-Path $OutputRoot 'nbt-gzip-fixture-summary.csv'
$rows | Set-Content -LiteralPath $summary -Encoding UTF8
if ($allCompressedBytesMatch) {
    Write-Output "NBT gzip fixture byte parity OK"
} else {
    Write-Output "NBT gzip fixture decompressed parity OK; compressed bytes differ"
}
Write-Output "outputRoot=$OutputRoot"
Write-Output "summary=$summary"
