[CmdletBinding()]
param(
    [string]$RustRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [Parameter(Mandatory = $true)]
    [string]$GoldenRoot,
    [Parameter(Mandatory = $true)]
    [string]$OutputRoot
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

function Read-CorpusSettings {
    param([string]$Path)
    $settings = @{}
    if (!(Test-Path -LiteralPath $Path)) {
        return $settings
    }
    foreach ($line in Get-Content -LiteralPath $Path) {
        $trimmed = $line.Trim()
        if ($trimmed -eq '' -or $trimmed.StartsWith('#')) {
            continue
        }
        $separator = $line.IndexOf('=')
        if ($separator -le 0) {
            continue
        }
        $key = $line.Substring(0, $separator).Trim()
        $value = $line.Substring($separator + 1)
        $settings[$key] = $value
    }
    return $settings
}

function Get-Setting {
    param(
        [hashtable]$Settings,
        [string]$Key,
        [string]$DefaultValue = ''
    )
    if ($Settings.ContainsKey($Key)) {
        return $Settings[$Key]
    }
    return $DefaultValue
}

function Get-CandidateCliArgs {
    param(
        [string]$EntryName,
        [hashtable]$Settings,
        [string]$WorldDir
    )

    $command = Get-Setting -Settings $Settings -Key 'command'
    $format = Get-Setting -Settings $Settings -Key 'format'
    if ($command -eq '') {
        switch ($EntryName) {
            'flat-mca' {
                $command = 'generate-flat-test-world'
                $format = 'mca'
            }
            'flat-linear' {
                $command = 'generate-flat-test-world'
                $format = 'linear'
            }
            'palette-stress-mca' {
                $command = 'generate-palette-stress-world'
            }
            default {
                throw "Corpus entry $EntryName does not declare a command in settings.properties."
            }
        }
    }

    switch ($command) {
        'generate-flat-test-world' {
            if ($format -eq '') {
                throw "Corpus entry $EntryName is missing required setting: format."
            }
            return @($command, $WorldDir, $format)
        }
        'generate-palette-stress-world' {
            return @($command, $WorldDir)
        }
        'generate-height-region' {
            $heightmap = Get-Setting -Settings $Settings -Key 'heightmap'
            $scale = Get-Setting -Settings $Settings -Key 'scale'
            $regionX = Get-Setting -Settings $Settings -Key 'regionX'
            $regionZ = Get-Setting -Settings $Settings -Key 'regionZ'
            if ($heightmap -eq '' -or $scale -eq '' -or $regionX -eq '' -or $regionZ -eq '' -or $format -eq '') {
                throw "Corpus entry $EntryName is missing one of: heightmap, scale, regionX, regionZ, format."
            }
            return @($command, $heightmap, $WorldDir, $scale, $regionX, $regionZ, $format)
        }
        default {
            throw "Unsupported Rust candidate corpus command for ${EntryName}: $command"
        }
    }
}

function Write-CandidateSettings {
    param(
        [string]$Path,
        [hashtable]$Settings,
        [string]$EntryName,
        [string]$GoldenRoot
    )
    $merged = @{}
    foreach ($key in $Settings.Keys) {
        $merged[$key] = $Settings[$key]
    }
    $merged['corpus.name'] = $EntryName
    $merged['corpus.oracle'] = 'rust'
    $merged['corpus.generatedBy'] = 'rust/scripts/generate-candidates.ps1'
    $merged['corpus.goldenRoot'] = $GoldenRoot

    $lines = @()
    foreach ($key in ($merged.Keys | Sort-Object)) {
        $lines += "$key=$($merged[$key])"
    }
    Set-Content -Encoding ASCII -Path $Path -Value $lines
}

function Invoke-RustCorpusCommand {
    param(
        [string]$EntryDir,
        [string[]]$CliArgs,
        [string]$RustCli
    )
    $stdoutFile = Join-Path $EntryDir 'stdout.txt'
    $stderrFile = Join-Path $EntryDir 'stderr.txt'
    Set-Content -Encoding ASCII -Path (Join-Path $EntryDir 'command.txt') `
        -Value ('earthmap-rs ' + ($CliArgs -join ' '))
    & $RustCli @CliArgs 1> $stdoutFile 2> $stderrFile
    if ($LASTEXITCODE -ne 0) {
        throw "Rust candidate corpus command failed with exit code ${LASTEXITCODE}: $($CliArgs -join ' ')"
    }
}

function Assert-ChildPath {
    param(
        [string]$Parent,
        [string]$Child
    )
    $parentFull = [System.IO.Path]::GetFullPath($Parent).TrimEnd('\', '/')
    $childFull = [System.IO.Path]::GetFullPath($Child)
    if (!$childFull.StartsWith($parentFull + [System.IO.Path]::DirectorySeparatorChar,
            [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to write outside output root: $childFull"
    }
}

function Test-SameOrChildPath {
    param(
        [string]$Parent,
        [string]$Child
    )
    $parentFull = [System.IO.Path]::GetFullPath($Parent).TrimEnd('\', '/')
    $childFull = [System.IO.Path]::GetFullPath($Child).TrimEnd('\', '/')
    if ($childFull.Equals($parentFull, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $true
    }
    return $childFull.StartsWith($parentFull + [System.IO.Path]::DirectorySeparatorChar,
        [System.StringComparison]::OrdinalIgnoreCase)
}

function Assert-SeparateCorpusRoots {
    param(
        [string]$GoldenRoot,
        [string]$OutputRoot
    )
    if ((Test-SameOrChildPath -Parent $GoldenRoot -Child $OutputRoot) -or
        (Test-SameOrChildPath -Parent $OutputRoot -Child $GoldenRoot)) {
        throw "OutputRoot must be separate from GoldenRoot: golden=$GoldenRoot output=$OutputRoot"
    }
}

$RustRoot = (Resolve-Path $RustRoot).Path
$GoldenRoot = (Resolve-Path $GoldenRoot).Path
$OutputRoot = [System.IO.Path]::GetFullPath($OutputRoot)
Assert-SeparateCorpusRoots -GoldenRoot $GoldenRoot -OutputRoot $OutputRoot

& (Join-Path $PSScriptRoot 'build.ps1') -RustRoot $RustRoot | Out-Null
$RustCli = Get-RustCli -Root $RustRoot

New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null

$entries = @(Get-ChildItem -Path $GoldenRoot -Directory | Sort-Object FullName)
if ($entries.Count -eq 0) {
    throw "No golden corpus entries found under $GoldenRoot."
}

foreach ($goldenEntry in $entries) {
    $entryDir = Join-Path $OutputRoot $goldenEntry.Name
    Assert-ChildPath -Parent $OutputRoot -Child $entryDir
    $worldDir = Join-Path $entryDir 'world'
    if (Test-Path -LiteralPath $entryDir) {
        Remove-Item -LiteralPath $entryDir -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $entryDir | Out-Null

    $settingsPath = Join-Path $goldenEntry.FullName 'settings.properties'
    $settings = Read-CorpusSettings -Path $settingsPath
    Write-CandidateSettings -Path (Join-Path $entryDir 'settings.properties') -Settings $settings `
        -EntryName $goldenEntry.Name -GoldenRoot $GoldenRoot

    $cliArgs = Get-CandidateCliArgs -EntryName $goldenEntry.Name -Settings $settings -WorldDir $worldDir
    Invoke-RustCorpusCommand -EntryDir $entryDir -CliArgs $cliArgs -RustCli $RustCli

    $payloadManifest = Join-Path $entryDir 'chunk-payload-manifest.csv'
    Remove-Item -LiteralPath $payloadManifest -ErrorAction SilentlyContinue
    $regionFiles = @(Get-ChildItem -Path (Join-Path $worldDir 'region') -File -Include '*.mca', '*.linear' -Recurse |
        Sort-Object FullName)
    if ($regionFiles.Count -eq 0) {
        throw "No region files were generated for candidate corpus entry $($goldenEntry.Name)."
    }
    $firstRegion = $true
    foreach ($regionFile in $regionFiles) {
        if ($firstRegion) {
            & $RustCli write-region-payload-manifest $regionFile.FullName $payloadManifest
            $firstRegion = $false
        } else {
            & $RustCli append-region-payload-manifest $regionFile.FullName $payloadManifest
        }
        if ($LASTEXITCODE -ne 0) {
            throw "Rust payload manifest command failed with exit code ${LASTEXITCODE}: $($regionFile.FullName)"
        }
    }

    & $RustCli write-sha256-manifest $entryDir (Join-Path $entryDir 'sha256-manifest.txt')
    if ($LASTEXITCODE -ne 0) {
        throw "Rust SHA-256 manifest command failed with exit code ${LASTEXITCODE}: $entryDir"
    }
}

Write-Output "Candidate corpus generated: $OutputRoot"
