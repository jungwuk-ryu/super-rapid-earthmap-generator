[CmdletBinding()]
param(
    [string]$RustRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [Parameter(Mandatory = $true)]
    [string]$GoldenRoot,
    [string]$CandidateRoot = ''
)

$ErrorActionPreference = 'Stop'

function Get-RustCli {
    param([string]$Root)
    $debugDir = Join-Path $Root 'target\debug'
    $exe = Join-Path $debugDir 'earthmap-rs.exe'
    if (!(Test-Path -LiteralPath $exe)) {
        $exe = Join-Path $debugDir 'earthmap-rs'
    }
    if (!(Test-Path -LiteralPath $exe)) {
        throw "Built Rust CLI was not found under $debugDir."
    }
    return $exe
}

function Get-ManifestRegionKeys {
    param([string]$ManifestPath)

    $header = 'format,regionX,regionZ,localChunkX,localChunkZ,payloadBytes,payloadSha256'
    $keys = New-Object 'System.Collections.Generic.HashSet[string]'
    $lines = Get-Content -LiteralPath $ManifestPath
    if ($lines.Count -eq 0) {
        throw "Chunk payload manifest is empty: $ManifestPath"
    }
    if ($lines[0] -ne $header) {
        throw "Unexpected chunk payload manifest header in ${ManifestPath}: $($lines[0])"
    }

    for ($i = 1; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line.Trim() -eq '') {
            continue
        }
        $columns = $line.Split(',')
        if ($columns.Count -ne 7) {
            throw "Invalid chunk payload manifest row $($i + 1) in ${ManifestPath}: expected 7 columns."
        }
        [void]$keys.Add("$($columns[0]):$($columns[1]):$($columns[2])")
    }
    if ($keys.Count -eq 0) {
        throw "Chunk payload manifest has no region rows: $ManifestPath"
    }
    return @($keys | Sort-Object)
}

function Get-RegionFileKey {
    param([System.IO.FileInfo]$RegionFile)

    if ($RegionFile.Name -notmatch '^r\.(-?\d+)\.(-?\d+)\.(mca|linear)$') {
        throw "Region file name must be r.<x>.<z>.<mca|linear>: $($RegionFile.FullName)"
    }
    return "$($Matches[3]):$($Matches[1]):$($Matches[2])"
}

function Write-ComparisonError {
    param([string]$Message)

    Write-Error -Message $Message -ErrorAction Continue
}

function Compare-RegionKeySets {
    param(
        [string[]]$ExpectedKeys,
        [string[]]$ActualKeys,
        [string]$EntryName,
        [string]$ActualDescription,
        [string]$ExpectedDescription
    )

    $expectedSet = New-Object 'System.Collections.Generic.HashSet[string]'
    foreach ($key in $ExpectedKeys) {
        [void]$expectedSet.Add($key)
    }
    $actualSet = New-Object 'System.Collections.Generic.HashSet[string]'
    foreach ($key in $ActualKeys) {
        [void]$actualSet.Add($key)
    }

    $missing = @($ExpectedKeys | Where-Object { !$actualSet.Contains($_) })
    $extra = @($ActualKeys | Where-Object { !$expectedSet.Contains($_) })
    if ($missing.Count -gt 0 -or $extra.Count -gt 0) {
        if ($missing.Count -gt 0) {
            Write-ComparisonError "$ActualDescription $EntryName is missing region file(s) expected by ${ExpectedDescription}: $($missing -join ', ')"
        }
        if ($extra.Count -gt 0) {
            Write-ComparisonError "$ActualDescription $EntryName has extra region file(s) missing from ${ExpectedDescription}: $($extra -join ', ')"
        }
        return $false
    }
    return $true
}

$RustRoot = (Resolve-Path $RustRoot).Path
$GoldenRoot = (Resolve-Path $GoldenRoot).Path
if ($CandidateRoot -ne '') {
    $CandidateRoot = (Resolve-Path $CandidateRoot).Path
} else {
    $CandidateRoot = $GoldenRoot
}

& (Join-Path $PSScriptRoot 'build.ps1') -RustRoot $RustRoot | Out-Null
$RustCli = Get-RustCli -Root $RustRoot

$entries = @(Get-ChildItem -Path $GoldenRoot -Directory | Sort-Object FullName)
if ($entries.Count -eq 0) {
    throw "No golden corpus entries found under $GoldenRoot."
}

$failures = 0
foreach ($entry in $entries) {
    $payloadManifest = Join-Path $entry.FullName 'chunk-payload-manifest.csv'
    if (!(Test-Path -LiteralPath $payloadManifest)) {
        Write-ComparisonError "Missing chunk payload manifest: $payloadManifest"
        $failures++
        continue
    }

    $candidateEntry = Join-Path $CandidateRoot $entry.Name
    if (!(Test-Path -LiteralPath $candidateEntry)) {
        Write-ComparisonError "Missing candidate corpus entry for $($entry.Name): $candidateEntry"
        $failures++
        continue
    }

    $regionRoot = Join-Path $candidateEntry 'world\region'
    if (!(Test-Path -LiteralPath $regionRoot)) {
        Write-ComparisonError "Missing candidate region directory for corpus entry $($entry.Name): $regionRoot"
        $failures++
        continue
    }
    $regionFiles = @(Get-ChildItem -Path $regionRoot -File -Include '*.mca', '*.linear' -Recurse |
        Sort-Object FullName)
    if ($regionFiles.Count -eq 0) {
        Write-ComparisonError "No candidate region files found for corpus entry $($entry.Name): $regionRoot"
        $failures++
        continue
    }
    $manifestKeys = Get-ManifestRegionKeys -ManifestPath $payloadManifest
    $actualKeys = @($regionFiles | ForEach-Object { Get-RegionFileKey -RegionFile $_ } | Sort-Object -Unique)
    $actualDescription = if ($CandidateRoot -eq $GoldenRoot) { "Golden entry" } else { "Candidate entry" }
    $expectedDescription = if ($CandidateRoot -eq $GoldenRoot) { "manifest" } else { "golden manifest" }
    if (!(Compare-RegionKeySets -ExpectedKeys $manifestKeys -ActualKeys $actualKeys -EntryName $entry.Name `
                -ActualDescription $actualDescription -ExpectedDescription $expectedDescription)) {
        $failures++
        continue
    }

    foreach ($regionFile in $regionFiles) {
        $safeName = $regionFile.Name -replace '[^A-Za-z0-9_.-]', '_'
        $stdoutFile = Join-Path $candidateEntry "compare-$safeName.stdout.txt"
        $stderrFile = Join-Path $candidateEntry "compare-$safeName.stderr.txt"
        & $RustCli compare-region-payload-manifest $payloadManifest $regionFile.FullName `
            1> $stdoutFile 2> $stderrFile
        if ($LASTEXITCODE -ne 0) {
            Write-ComparisonError "Golden comparison failed for $($regionFile.FullName). See $stdoutFile and $stderrFile."
            $failures++
        }
    }
}

if ($failures -ne 0) {
    throw "Golden comparison failed with $failures failure(s)."
}

if ($CandidateRoot -eq $GoldenRoot) {
    Write-Output "Golden comparison passed: $GoldenRoot"
} else {
    Write-Output "Golden candidate comparison passed: golden=$GoldenRoot candidate=$CandidateRoot"
}
