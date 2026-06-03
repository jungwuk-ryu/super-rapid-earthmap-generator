[CmdletBinding()]
param(
    [string]$RustRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
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

function Invoke-JavaCorpusCommand {
    param(
        [string]$EntryDir,
        [string[]]$CliArgs,
        [string]$JavaRun
    )
    $stdoutFile = Join-Path $EntryDir 'stdout.txt'
    $stderrFile = Join-Path $EntryDir 'stderr.txt'
    Set-Content -Encoding ASCII -Path (Join-Path $EntryDir 'command.txt') `
        -Value ('.\scripts\run.ps1 ' + ($CliArgs -join ' '))
    & $JavaRun @CliArgs 1> $stdoutFile 2> $stderrFile
    if ($LASTEXITCODE -ne 0) {
        throw "Java corpus command failed with exit code ${LASTEXITCODE}: $($CliArgs -join ' ')"
    }
}

$RustRoot = (Resolve-Path $RustRoot).Path
$ProjectRoot = (Resolve-Path (Join-Path $RustRoot '..')).Path
$OutputRoot = [System.IO.Path]::GetFullPath($OutputRoot)
$JavaRun = Join-Path $ProjectRoot 'scripts\run.ps1'

if (!(Test-Path -LiteralPath $JavaRun)) {
    throw "Java run wrapper was not found: $JavaRun"
}

& (Join-Path $PSScriptRoot 'build.ps1') -RustRoot $RustRoot | Out-Null
$RustCli = Get-RustCli -Root $RustRoot

New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null

$entries = @(
    @{ Name = 'flat-mca'; Command = 'generate-flat-test-world'; Format = 'mca' },
    @{ Name = 'flat-linear'; Command = 'generate-flat-test-world'; Format = 'linear' },
    @{ Name = 'palette-stress-mca'; Command = 'generate-palette-stress-world'; Format = '' }
)

foreach ($entry in $entries) {
    $entryDir = Join-Path $OutputRoot $entry.Name
    $worldDir = Join-Path $entryDir 'world'
    if (Test-Path -LiteralPath $entryDir) {
        Remove-Item -LiteralPath $entryDir -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $entryDir | Out-Null

    $settings = @(
        "corpus.name=$($entry.Name)",
        "corpus.kind=synthetic",
        "corpus.oracle=java",
        "corpus.generatedBy=rust/scripts/generate-golden.ps1"
    )
    if ($entry.Format -ne '') {
        $settings += "format=$($entry.Format)"
        $cliArgs = @($entry.Command, $worldDir, $entry.Format)
    } else {
        $cliArgs = @($entry.Command, $worldDir)
    }
    Set-Content -Encoding ASCII -Path (Join-Path $entryDir 'settings.properties') -Value $settings

    Invoke-JavaCorpusCommand -EntryDir $entryDir -CliArgs $cliArgs -JavaRun $JavaRun

    $payloadManifest = Join-Path $entryDir 'chunk-payload-manifest.csv'
    Remove-Item -LiteralPath $payloadManifest -ErrorAction SilentlyContinue
    $regionFiles = @(Get-ChildItem -Path (Join-Path $worldDir 'region') -File -Include '*.mca', '*.linear' -Recurse |
        Sort-Object FullName)
    if ($regionFiles.Count -eq 0) {
        throw "No region files were generated for corpus entry $($entry.Name)."
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

Write-Output "Golden corpus generated: $OutputRoot"
