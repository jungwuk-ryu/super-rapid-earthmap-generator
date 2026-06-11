param(
    [string]$Heightmap = $env:EARTHMAP_HEIGHTMAP,
    [string]$OutputRoot = $env:EARTHMAP_QUALITY_OUTPUT_ROOT,
    [int]$Scale = 5000,
    [int]$Threads = 4,
    [string]$Format = "mca",
    [string]$BaselineRoot = "",
    [string[]]$Samples = @(),
    [string]$CacheRows = "512",
    [string]$PrefetchRows = "0",
    [double]$VerticalScale = 1.25,
    [ValidateSet("photo", "classified")]
    [string]$TextureMode = "photo",
    [switch]$CleanSampleOutput,
    [switch]$NoQualityGate,
    [switch]$UseEcologyQualityGate,
    [switch]$SkipGeneration,
    [switch]$PhotoParityEvidenceOnly,
    [string]$ProductionSamplesCsv = "",
    [string]$ProductionPreviewDebug = "off",
    [switch]$LegacyPerSample
)

$ErrorActionPreference = "Stop"
$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = Split-Path -Parent $scriptRoot
$run = Join-Path (Join-Path (Join-Path $repoRoot 'rust') 'scripts') 'run.ps1'
$photoMetricScript = Join-Path $scriptRoot "run-photo-parity-metric-crop.ps1"
$requiredSummarySchemaVersion = 11

if ([string]::IsNullOrWhiteSpace($Heightmap)) {
    throw "Heightmap is required. Pass -Heightmap or set EARTHMAP_HEIGHTMAP."
}
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path (Join-Path $repoRoot 'out') 'quality-acceptance-samples'
}

$sampleDefinitions = @(
    [pscustomobject]@{ Name = "west-africa";     Longitude = 0.0;    Latitude = 5.0;    Cols = 2; Rows = 2; StartRegionX = -1; StartRegionZ = -1; StartRegionScale = 5000 },
    [pscustomobject]@{ Name = "sahara-core";     Longitude = 13.0;   Latitude = 24.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "sahara-egypt-core"; Longitude = 26.9; Latitude = 30.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null; Scale = 750; ExpectedExportTile = "N29E026" },
    [pscustomobject]@{ Name = "sahel-edge";      Longitude = 8.0;    Latitude = 13.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "sahel-chad-transition"; Longitude = 22.8; Latitude = 15.4; Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null; Scale = 750; ExpectedExportTile = "N15E022" },
    [pscustomobject]@{ Name = "congo-edge";      Longitude = 20.0;   Latitude = -2.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "congo-core";      Longitude = 23.5;   Latitude = 0.5;    Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null; Scale = 750; ExpectedExportTile = "N00E023" },
    [pscustomobject]@{ Name = "nile-delta";      Longitude = 31.0;   Latitude = 30.5;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "arabia-inland";   Longitude = 41.0;   Latitude = 21.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "arabia-coast";    Longitude = 58.0;   Latitude = 19.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "arabia-desert";   Longitude = 55.0;   Latitude = 23.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "australia-dry";   Longitude = 134.0;  Latitude = -25.0;  Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "south-africa-savanna"; Longitude = 25.0; Latitude = -25.0; Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "amazon-edge";     Longitude = -60.0;  Latitude = -5.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "europe-forest";   Longitude = 10.0;   Latitude = 50.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "europe-northsea-denmark"; Longitude = 7.0; Latitude = 53.0; Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null; Scale = 750; ExpectedExportTile = "N53E006" },
    [pscustomobject]@{ Name = "western-europe";  Longitude = -5.0;   Latitude = 44.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "mediterranean-edge"; Longitude = 27.0; Latitude = 38.0;   Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null },
    [pscustomobject]@{ Name = "med-italy-islands"; Longitude = 9.0; Latitude = 39.0; Cols = 1; Rows = 1; StartRegionX = $null; StartRegionZ = $null; Scale = 750; ExpectedExportTile = "N39E008" }
)

if ($Samples.Count -gt 0) {
    $wanted = @{}
    foreach ($sample in $Samples) {
        $wanted[$sample] = $true
    }
    $sampleDefinitions = @($sampleDefinitions | Where-Object { $wanted.ContainsKey($_.Name) })
}

if ($sampleDefinitions.Count -eq 0) {
    throw "No matching quality samples selected."
}

New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null

function Format-Elapsed {
    param([TimeSpan]$Elapsed)
    return ("{0:00}:{1:00}:{2:00}.{3:000}" -f [int]$Elapsed.TotalHours, $Elapsed.Minutes, $Elapsed.Seconds, $Elapsed.Milliseconds)
}

function Format-LogMessage {
    param(
        [string]$Message,
        [int]$MaxLength = 700
    )
    if ([string]::IsNullOrWhiteSpace($Message)) {
        return ""
    }
    $oneLine = ($Message -replace '[\r\n\t]+', ' ').Trim()
    if ($oneLine.Length -gt $MaxLength) {
        return $oneLine.Substring(0, $MaxLength) + "..."
    }
    return $oneLine
}

function ConvertTo-LogText {
    param([object]$Value)
    if ($null -eq $Value) {
        return ""
    }
    if ($Value -is [System.Management.Automation.ErrorRecord]) {
        return $Value.ToString()
    }
    return [string]$Value
}

function Write-LogLine {
    param(
        [string]$LogPath,
        [string]$Line
    )
    $logDir = Split-Path -Parent $LogPath
    if (-not [string]::IsNullOrWhiteSpace($logDir)) {
        New-Item -ItemType Directory -Force -Path $logDir | Out-Null
    }
    Add-Content -LiteralPath $LogPath -Value $Line -Encoding UTF8
}

function Get-LogFailureHint {
    param(
        [string]$LogPath,
        [string]$FallbackMessage = "",
        [int]$TailLines = 240
    )
    $lines = @()
    if (Test-Path -LiteralPath $LogPath) {
        $lines = @(Get-Content -LiteralPath $LogPath -Tail $TailLines -ErrorAction SilentlyContinue | ForEach-Object {
                ConvertTo-LogText $_
            })
    }
    $patterns = @(
        'NoSuchMethodError',
        'NoClassDefFoundError',
        'ClassNotFoundException',
        'Exception in thread',
        'OutOfMemoryError',
        'StackOverflowError',
        'CLI failed with exit code',
        'BUILD FAILED',
        '\bFAILED\b',
        '\berror\b'
    )
    foreach ($pattern in $patterns) {
        for ($i = $lines.Count - 1; $i -ge 0; $i--) {
            if ($lines[$i] -match $pattern) {
                $context = New-Object System.Collections.Generic.List[string]
                [void]$context.Add($lines[$i])
                if ($i + 1 -lt $lines.Count -and $lines[$i + 1] -match '^\s+(at|Caused by:|Suppressed:)') {
                    [void]$context.Add($lines[$i + 1])
                }
                return (Format-LogMessage ($context -join " | "))
            }
        }
    }
    if (-not [string]::IsNullOrWhiteSpace($FallbackMessage)) {
        return (Format-LogMessage $FallbackMessage)
    }
    $tail = @($lines | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Last 6)
    return (Format-LogMessage ($tail -join " | "))
}

function ConvertTo-KeyValueMap {
    param([string]$Line)
    $values = @{}
    if ([string]::IsNullOrWhiteSpace($Line)) {
        return $values
    }
    foreach ($part in ($Line -split ',')) {
        $pieces = $part -split '=', 2
        if ($pieces.Count -eq 2) {
            $values[$pieces[0].Trim()] = $pieces[1].Trim()
        }
    }
    return $values
}

function Format-SecondsDuration {
    param([double]$Seconds)
    if ([double]::IsNaN($Seconds) -or [double]::IsInfinity($Seconds) -or $Seconds -lt 0) {
        return "unknown"
    }
    $span = [TimeSpan]::FromSeconds($Seconds)
    if ($span.TotalDays -ge 1) {
        return ("{0:N1}d" -f $span.TotalDays)
    }
    if ($span.TotalHours -ge 1) {
        return ("{0:N1}h" -f $span.TotalHours)
    }
    if ($span.TotalMinutes -ge 1) {
        return ("{0:N1}m" -f $span.TotalMinutes)
    }
    return ("{0:N0}s" -f $span.TotalSeconds)
}

function Write-GenerationProgressLine {
    param(
        [string]$SampleName,
        [string]$Line,
        [string]$World,
        [string]$ProgressPath,
        [string]$PreviewImage,
        [string]$PreviewViewer,
        [string]$SummaryPath
    )
    if ([string]::IsNullOrWhiteSpace($SampleName) -or $Line -notmatch '^progress,') {
        return
    }
    $values = ConvertTo-KeyValueMap -Line $Line
    $finished = if ($values.ContainsKey('finished')) { $values['finished'] } else { '' }
    $planned = if ($values.ContainsKey('planned')) { $values['planned'] } else { '' }
    $percent = if ($values.ContainsKey('percent')) { $values['percent'] } else { '' }
    $generated = if ($values.ContainsKey('generated')) { $values['generated'] } else { '' }
    $failed = if ($values.ContainsKey('failed')) { $values['failed'] } else { '' }
    $queued = if ($values.ContainsKey('queued')) { $values['queued'] } else { '' }
    $remaining = if ($values.ContainsKey('remaining')) { $values['remaining'] } else { '' }
    $rate = if ($values.ContainsKey('regionsPerHour')) { $values['regionsPerHour'] } else { '' }
    $eta = 'unknown'
    if ($values.ContainsKey('etaSeconds')) {
        $etaValue = 0.0
        if ([double]::TryParse($values['etaSeconds'], [ref]$etaValue)) {
            $eta = Format-SecondsDuration $etaValue
        }
    }
    Write-Host ("qualityAcceptance.sample.progress=sample={0},finished={1}/{2},percent={3},generated={4},failed={5},queued={6},remaining={7},rateRegionsPerHour={8},eta={9},progressFile={10},preview={11},viewer={12},summary={13},output={14}" -f `
            $SampleName, $finished, $planned, $percent, $generated, $failed, $queued, $remaining, $rate, $eta,
            $ProgressPath, $PreviewImage, $PreviewViewer, $SummaryPath, $World)
}

function Invoke-LoggedCommand {
    param(
        [string]$LogPath,
        [scriptblock]$Command,
        [string]$SampleName = "",
        [string]$World = "",
        [string]$ProgressPath = "",
        [string]$PreviewImage = "",
        [string]$PreviewViewer = "",
        [string]$SummaryPath = ""
    )
    $logDir = Split-Path -Parent $LogPath
    if (-not [string]::IsNullOrWhiteSpace($logDir)) {
        New-Item -ItemType Directory -Force -Path $logDir | Out-Null
    }
    if (Test-Path -LiteralPath $LogPath) {
        Remove-Item -LiteralPath $LogPath -Force
    }
    $exitCode = 0
    $errorMessage = ""
    try {
        $global:LASTEXITCODE = 0
        & $Command 2>&1 | ForEach-Object {
            $line = ConvertTo-LogText $_
            Write-LogLine -LogPath $LogPath -Line $line
            Write-Host $line
            Write-GenerationProgressLine -SampleName $SampleName -Line $line -World $World -ProgressPath $ProgressPath `
                -PreviewImage $PreviewImage -PreviewViewer $PreviewViewer -SummaryPath $SummaryPath
        }
        $exitCode = if ($null -ne $LASTEXITCODE) { [int]$LASTEXITCODE } else { 0 }
    } catch {
        $exitCode = if ($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0) { [int]$LASTEXITCODE } else { 1 }
        $errorMessage = $_.Exception.Message
        Write-LogLine -LogPath $LogPath -Line ("powershell.error={0}" -f $errorMessage)
    }
    return [pscustomobject]@{
        ExitCode = $exitCode
        ErrorMessage = $errorMessage
        LogPath = $LogPath
        FailureHint = Get-LogFailureHint -LogPath $LogPath -FallbackMessage $errorMessage
    }
}

function Invoke-CapturedRunCommand {
    param(
        [string]$LogPath,
        [scriptblock]$Command
    )
    $logDir = Split-Path -Parent $LogPath
    if (-not [string]::IsNullOrWhiteSpace($logDir)) {
        New-Item -ItemType Directory -Force -Path $logDir | Out-Null
    }
    if (Test-Path -LiteralPath $LogPath) {
        Remove-Item -LiteralPath $LogPath -Force
    }
    $output = @()
    $exitCode = 0
    $errorMessage = ""
    try {
        $output = @(& $Command 2>&1)
        $exitCode = if ($null -ne $LASTEXITCODE) { [int]$LASTEXITCODE } else { 0 }
    } catch {
        $exitCode = if ($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0) { [int]$LASTEXITCODE } else { 1 }
        $errorMessage = $_.Exception.Message
        $output += $_
    }
    foreach ($line in $output) {
        Write-LogLine -LogPath $LogPath -Line (ConvertTo-LogText $line)
    }
    if (-not [string]::IsNullOrWhiteSpace($errorMessage)) {
        Write-LogLine -LogPath $LogPath -Line ("powershell.error={0}" -f $errorMessage)
    }
    return [pscustomobject]@{
        ExitCode = $exitCode
        Output = @($output | ForEach-Object { ConvertTo-LogText $_ })
        ErrorMessage = $errorMessage
        LogPath = $LogPath
        FailureHint = Get-LogFailureHint -LogPath $LogPath -FallbackMessage $errorMessage
    }
}

function Get-QualityAcceptanceReplayCommand {
    param([string[]]$SelectedSamples)

    $baselineArg = if ([string]::IsNullOrWhiteSpace($BaselineRoot)) { "" } else { " -BaselineRoot `"$BaselineRoot`"" }
    $cleanArg = if ($CleanSampleOutput) { " -CleanSampleOutput" } else { "" }
    $noQualityArg = if ($NoQualityGate) { " -NoQualityGate" } else { "" }
    $ecologyGateArg = if ($UseEcologyQualityGate) { " -UseEcologyQualityGate" } else { "" }
    $skipArg = if ($SkipGeneration) { " -SkipGeneration" } else { "" }
    $sampleArg = if ($SelectedSamples.Count -gt 0) { " -Samples $($SelectedSamples -join ',')" } else { "" }
    return ".\scripts\run-quality-acceptance-samples.ps1 -Heightmap `"$Heightmap`" -OutputRoot `"$OutputRoot`" -Scale $Scale -Threads $Threads -Format $Format -CacheRows $CacheRows -PrefetchRows $PrefetchRows -VerticalScale $VerticalScale -TextureMode $TextureMode$baselineArg$sampleArg$cleanArg$noQualityArg$ecologyGateArg$skipArg"
}

function Get-RegionForPoint {
    param(
        [double]$Longitude,
        [double]$Latitude,
        [int]$ScaleValue = $Scale,
        [string]$LogPath = ""
    )
    $result = if ([string]::IsNullOrWhiteSpace($LogPath)) {
        Invoke-CapturedRunCommand -LogPath (Join-Path $OutputRoot "_locate-heightmap-point.log") -Command {
            & $run locate-heightmap-point $Heightmap $ScaleValue $Longitude $Latitude
        }
    } else {
        Invoke-CapturedRunCommand -LogPath $LogPath -Command {
            & $run locate-heightmap-point $Heightmap $ScaleValue $Longitude $Latitude
        }
    }
    if ($result.ExitCode -ne 0) {
        throw ("locate-heightmap-point failed for lon={0} lat={1} scale=1:{2} heightmap={3}; hint={4}; log={5}" -f `
                $Longitude, $Latitude, $ScaleValue, $Heightmap, $result.FailureHint, $result.LogPath)
    }
    $values = @{}
    foreach ($line in $result.Output) {
        $parts = $line -split "=", 2
        if ($parts.Count -eq 2) {
            $values[$parts[0]] = $parts[1]
        }
    }
    foreach ($key in @("regionX", "regionZ")) {
        if (-not $values.ContainsKey($key)) {
            throw ("locate-heightmap-point output missing {0} for lon={1} lat={2} scale=1:{3}; log={4}" -f `
                    $key, $Longitude, $Latitude, $ScaleValue, $result.LogPath)
        }
    }
    return [pscustomobject]@{
        RegionX = [int]$values["regionX"]
        RegionZ = [int]$values["regionZ"]
    }
}

function Show-Summary {
    param([hashtable]$Values)
    $interesting = @(
        "mosaicLandBiomeBoundaryEdges",
        "mosaicLongHorizontalBiomeBoundaryRuns",
        "mosaicLongVerticalBiomeBoundaryRuns",
        "mosaicLandComponents",
        "mosaicSmallLandComponents",
        "mosaicSmallLandComponentPixels",
        "mosaicLargestLandComponent",
        "aggregate.landColumns",
        "aggregate.waterColumns",
        "aggregate.grassTopColumns",
        "aggregate.coarseDirtTopColumns",
        "aggregate.mossTopColumns",
        "aggregate.podzolTopColumns",
        "aggregate.sandTopColumns",
        "aggregate.redSandTopColumns",
        "aggregate.snowTopColumns",
        "aggregate.stoneTopColumns",
        "aggregate.gravelTopColumns",
        "aggregate.mudTopColumns",
        "aggregate.terracottaTopColumns",
        "aggregate.orangeTerracottaTopColumns",
        "aggregate.brownTerracottaTopColumns",
        "aggregate.badlandsBiomeColumns",
        "aggregate.biomeFamilyBeachColumns",
        "aggregate.biomeFamilyDesertColumns",
        "aggregate.biomeFamilyBadlandsColumns",
        "aggregate.biomeFamilySavannaColumns",
        "aggregate.biomeFamilyJungleColumns",
        "aggregate.biomeFamilyForestColumns",
        "aggregate.biomeFamilyTaigaColumns",
        "aggregate.biomeFamilySwampColumns",
        "aggregate.biomeFamilySnowColumns",
        "aggregate.biomeFamilyGrasslandColumns",
        "aggregate.biome.desert",
        "aggregate.biome.savanna",
        "aggregate.biome.savanna_plateau",
        "aggregate.biome.windswept_savanna",
        "aggregate.biome.jungle",
        "aggregate.biome.sparse_jungle",
        "aggregate.biome.bamboo_jungle",
        "aggregate.biome.forest",
        "aggregate.biome.dark_forest",
        "aggregate.biome.plains",
        "aggregate.biome.taiga",
        "aggregate.biome.snowy_taiga",
        "aggregate.biome.snowy_plains",
        "aggregate.familyTop.desert.sand",
        "aggregate.familyTop.desert.grass_block",
        "aggregate.familyTop.forest.stone",
        "aggregate.familyTop.forest.snow_block",
        "aggregate.familyTop.badlands.orange_terracotta",
        "aggregate.familyTop.badlands.terracotta",
        "aggregate.familyTop.savanna.sand",
        "aggregate.familyTop.savanna.coarse_dirt",
        "aggregate.familyTop.savanna.grass_block",
        "aggregate.familyTop.jungle.moss_block",
        "aggregate.familyTop.jungle.podzol",
        "aggregate.familyTop.jungle.grass_block",
        "aggregate.familyTop.forest.moss_block",
        "aggregate.familyTop.forest.podzol",
        "aggregate.familyTop.forest.grass_block",
        "aggregate.familyTop.taiga.moss_block",
        "aggregate.familyTop.taiga.podzol",
        "aggregate.familyTop.grassland.grass_block",
        "aggregate.familyTop.snow.snow_block",
        "aggregate.familyTop.snow.grass_block",
        "aggregate.decisionSourceTop.intent.moss_block",
        "aggregate.decisionSourceTop.intent.podzol",
        "aggregate.decisionSourceTop.intent.grass_block",
        "aggregate.decisionSourceTop.intent-ecoregion.sand",
        "aggregate.decisionSourceTop.material-rule.sand",
        "aggregate.decisionSourceTop.material-rule.stone",
        "aggregate.decisionSourceTop.snow.snow_block",
        "aggregate.decisionSourceTop.intent-stabilized-component.grass_block",
        "aggregate.decisionSourceTop.smoother-isolated.grass_block",
        "aggregate.biomeTopMismatchColumns",
        "aggregate.smootherBiomeTopMismatchColumns",
        "aggregate.vegetatedBiomeNonVegetationTopColumns",
        "aggregate.smootherVegetatedBiomeNonVegetationTopColumns",
        "aggregate.dryBiomeGrassTopColumns",
        "aggregate.smootherDryBiomeGrassTopColumns",
        "aggregate.landTerrainTokenAvailableColumns",
        "aggregate.terrainTokenAvailableColumns",
        "aggregate.landTerrainTokenExportColumns",
        "aggregate.landTerrainTokenJavaStandardColumns",
        "aggregate.landDataEvidenceClimateColumns",
        "aggregate.landDataEvidenceVegetationColumns",
        "aggregate.landDataEvidenceTreeColumns",
        "aggregate.landDataEvidenceHerbaceousColumns",
        "aggregate.landDataEvidenceShrubColumns",
        "aggregate.landDataEvidenceSnowLayerColumns",
        "aggregate.landDataEvidenceSwampLayerColumns",
        "aggregate.landDataEvidenceSlopeColumns",
        "aggregate.landDataEvidenceEcoregionColumns",
        "aggregate.landDataPresenceVegetationColumns",
        "aggregate.landDataPresenceTreeColumns",
        "aggregate.landDataPresenceHerbaceousColumns",
        "aggregate.landDataPresenceShrubColumns",
        "aggregate.landDataPresenceSnowLayerColumns",
        "aggregate.landDataPresenceSwampLayerColumns",
        "aggregate.landDataPresenceSteepSlopeColumns",
        "aggregate.waterDataEvidenceOceanTemperatureColumns",
        "aggregate.waterDataEvidenceBathymetryColumns",
        "aggregate.decisionSource.intent",
        "aggregate.decisionSource.intent-ecoregion",
        "aggregate.decisionSource.intent-stabilized-cell",
        "aggregate.decisionSource.intent-stabilized-component",
        "aggregate.decisionSource.ecoregion",
        "aggregate.decisionSource.smoother",
        "aggregate.decisionSource.smoother-isolated",
        "aggregate.decisionSource.smoother-cell",
        "aggregate.decisionSource.smoother-component"
    )
    foreach ($key in $interesting) {
        if ($Values.ContainsKey($key)) {
            Write-Host "$key=$($Values[$key])"
        }
    }
}

function Get-SummaryValues {
    param([string]$SummaryPath)
    if (!(Test-Path -LiteralPath $SummaryPath)) {
        throw "summary missing: $SummaryPath"
    }
    $values = @{}
    $lines = Get-Content -LiteralPath $SummaryPath
    foreach ($line in $lines) {
        $parts = $line -split "=", 2
        if ($parts.Count -eq 2) {
            $number = 0L
            if ([long]::TryParse($parts[1], [ref]$number)) {
                $values[$parts[0]] = $number
            }
        }
    }
    return $values
}

function Assert-RequiredSummaryValues {
    param(
        [string]$SampleName,
        [hashtable]$Values,
        [int]$ExpectedStartX,
        [int]$ExpectedStartZ,
        [int]$ExpectedCols,
        [int]$ExpectedRows
    )
    $requiredKeys = @(
        "summarySchemaVersion",
        "startRegionX",
        "startRegionZ",
        "cols",
        "rows",
        "mosaicWidthPixels",
        "mosaicHeightPixels",
        "mosaicAnalyzedPixels",
        "mosaicMissingTiles",
        "mosaicLandBiomeBoundaryEdges",
        "mosaicLandBiomeHorizontalBoundaryEdges",
        "mosaicLandBiomeVerticalBoundaryEdges",
        "mosaicLongHorizontalBiomeBoundaryRuns",
        "mosaicLongVerticalBiomeBoundaryRuns",
        "mosaicLandComponents",
        "mosaicSmallLandComponents",
        "mosaicSmallLandComponentPixels",
        "mosaicLargestLandComponent",
        "aggregate.validColumns",
        "aggregate.landColumns",
        "aggregate.waterColumns",
        "aggregate.initialWaterDisagreements",
        "aggregate.beachBiomeColumns",
        "aggregate.sandTopColumns",
        "aggregate.grassTopColumns",
        "aggregate.coarseDirtTopColumns",
        "aggregate.mossTopColumns",
        "aggregate.podzolTopColumns",
        "aggregate.redSandTopColumns",
        "aggregate.snowTopColumns",
        "aggregate.stoneTopColumns",
        "aggregate.gravelTopColumns",
        "aggregate.mudTopColumns",
        "aggregate.terracottaTopColumns",
        "aggregate.orangeTerracottaTopColumns",
        "aggregate.brownTerracottaTopColumns",
        "aggregate.badlandsBiomeColumns",
        "aggregate.biomeFamilyBeachColumns",
        "aggregate.biomeFamilyDesertColumns",
        "aggregate.biomeFamilyBadlandsColumns",
        "aggregate.biomeFamilySavannaColumns",
        "aggregate.biomeFamilyJungleColumns",
        "aggregate.biomeFamilyForestColumns",
        "aggregate.biomeFamilyTaigaColumns",
        "aggregate.biomeFamilySwampColumns",
        "aggregate.biomeFamilySnowColumns",
        "aggregate.biomeFamilyGrasslandColumns",
        "aggregate.biome.desert",
        "aggregate.biome.savanna",
        "aggregate.biome.savanna_plateau",
        "aggregate.biome.windswept_savanna",
        "aggregate.biome.jungle",
        "aggregate.biome.sparse_jungle",
        "aggregate.biome.bamboo_jungle",
        "aggregate.biome.forest",
        "aggregate.biome.dark_forest",
        "aggregate.biome.plains",
        "aggregate.biome.taiga",
        "aggregate.biome.snowy_taiga",
        "aggregate.biome.snowy_plains",
        "aggregate.familyTop.desert.sand",
        "aggregate.familyTop.desert.grass_block",
        "aggregate.familyTop.forest.stone",
        "aggregate.familyTop.forest.snow_block",
        "aggregate.familyTop.badlands.terracotta",
        "aggregate.familyTop.badlands.orange_terracotta",
        "aggregate.familyTop.savanna.sand",
        "aggregate.familyTop.savanna.coarse_dirt",
        "aggregate.familyTop.savanna.grass_block",
        "aggregate.familyTop.jungle.moss_block",
        "aggregate.familyTop.jungle.podzol",
        "aggregate.familyTop.jungle.grass_block",
        "aggregate.familyTop.forest.moss_block",
        "aggregate.familyTop.forest.podzol",
        "aggregate.familyTop.forest.grass_block",
        "aggregate.familyTop.taiga.moss_block",
        "aggregate.familyTop.taiga.podzol",
        "aggregate.familyTop.grassland.grass_block",
        "aggregate.familyTop.snow.snow_block",
        "aggregate.familyTop.snow.grass_block",
        "aggregate.decisionSourceTop.intent.moss_block",
        "aggregate.decisionSourceTop.intent.podzol",
        "aggregate.decisionSourceTop.intent.grass_block",
        "aggregate.decisionSourceTop.intent-ecoregion.sand",
        "aggregate.decisionSourceTop.material-rule.sand",
        "aggregate.decisionSourceTop.material-rule.stone",
        "aggregate.decisionSourceTop.snow.snow_block",
        "aggregate.decisionSourceTop.intent-stabilized-component.grass_block",
        "aggregate.decisionSourceTop.smoother-isolated.grass_block",
        "aggregate.shallowWaterColumns",
        "aggregate.deepWaterColumns",
        "aggregate.maxWaterDepthBlocks",
        "aggregate.minGroundY",
        "aggregate.maxGroundY",
        "aggregate.highCoastFactorColumns",
        "aggregate.biomeTopMismatchColumns",
        "aggregate.smootherBiomeTopMismatchColumns",
        "aggregate.vegetatedBiomeNonVegetationTopColumns",
        "aggregate.smootherVegetatedBiomeNonVegetationTopColumns",
        "aggregate.dryBiomeGrassTopColumns",
        "aggregate.smootherDryBiomeGrassTopColumns",
        "aggregate.terrainTokenAvailableColumns",
        "aggregate.landTerrainTokenAvailableColumns",
        "aggregate.waterTerrainTokenAvailableColumns",
        "aggregate.terrainTokenExportColumns",
        "aggregate.landTerrainTokenExportColumns",
        "aggregate.waterTerrainTokenExportColumns",
        "aggregate.terrainTokenJavaStandardColumns",
        "aggregate.landTerrainTokenJavaStandardColumns",
        "aggregate.waterTerrainTokenJavaStandardColumns",
        "aggregate.sourceColorColumns",
        "aggregate.landSourceColorColumns",
        "aggregate.sourceRenderErrorSum",
        "aggregate.sourceRenderErrorMax",
        "aggregate.waterVolumeGapColumns",
        "aggregate.coastalLandColumns",
        "aggregate.coastalSandHaloColumns",
        "aggregate.coastEdgeSamples",
        "aggregate.coastLandAboveSeaGt4Samples",
        "aggregate.coastLandAboveSeaGt8Samples",
        "aggregate.coastLandAboveSeaGt16Samples",
        "aggregate.maxCoastFloorDelta",
        "aggregate.maxCoastLandAboveSeaDelta",
        "aggregate.dataEvidenceClimateColumns",
        "aggregate.landDataEvidenceClimateColumns",
        "aggregate.waterDataEvidenceClimateColumns",
        "aggregate.dataEvidenceTreeColumns",
        "aggregate.landDataEvidenceTreeColumns",
        "aggregate.waterDataEvidenceTreeColumns",
        "aggregate.dataEvidenceHerbaceousColumns",
        "aggregate.landDataEvidenceHerbaceousColumns",
        "aggregate.waterDataEvidenceHerbaceousColumns",
        "aggregate.dataEvidenceShrubColumns",
        "aggregate.landDataEvidenceShrubColumns",
        "aggregate.waterDataEvidenceShrubColumns",
        "aggregate.dataEvidenceVegetationColumns",
        "aggregate.landDataEvidenceVegetationColumns",
        "aggregate.waterDataEvidenceVegetationColumns",
        "aggregate.dataEvidenceSnowLayerColumns",
        "aggregate.landDataEvidenceSnowLayerColumns",
        "aggregate.waterDataEvidenceSnowLayerColumns",
        "aggregate.dataEvidenceSwampLayerColumns",
        "aggregate.landDataEvidenceSwampLayerColumns",
        "aggregate.waterDataEvidenceSwampLayerColumns",
        "aggregate.dataEvidenceOceanTemperatureColumns",
        "aggregate.landDataEvidenceOceanTemperatureColumns",
        "aggregate.waterDataEvidenceOceanTemperatureColumns",
        "aggregate.dataEvidenceBathymetryColumns",
        "aggregate.landDataEvidenceBathymetryColumns",
        "aggregate.waterDataEvidenceBathymetryColumns",
        "aggregate.dataEvidenceSlopeColumns",
        "aggregate.landDataEvidenceSlopeColumns",
        "aggregate.waterDataEvidenceSlopeColumns",
        "aggregate.dataEvidenceEcoregionColumns",
        "aggregate.landDataEvidenceEcoregionColumns",
        "aggregate.waterDataEvidenceEcoregionColumns",
        "aggregate.landDataPresenceVegetationColumns",
        "aggregate.landDataPresenceTreeColumns",
        "aggregate.landDataPresenceHerbaceousColumns",
        "aggregate.landDataPresenceShrubColumns",
        "aggregate.landDataPresenceSnowLayerColumns",
        "aggregate.landDataPresenceSwampLayerColumns",
        "aggregate.landDataPresenceSteepSlopeColumns",
        "aggregate.sourceColorColumns",
        "aggregate.landSourceColorColumns",
        "aggregate.sourceRenderErrorSum",
        "aggregate.sourceRenderErrorMax",
        "aggregate.waterVolumeGapColumns",
        "aggregate.coastalLandColumns",
        "aggregate.coastalSandHaloColumns",
        "aggregate.coastEdgeSamples",
        "aggregate.coastLandAboveSeaGt4Samples",
        "aggregate.coastLandAboveSeaGt8Samples",
        "aggregate.coastLandAboveSeaGt16Samples",
        "aggregate.maxCoastFloorDelta",
        "aggregate.maxCoastLandAboveSeaDelta"
    )
    foreach ($key in $requiredKeys) {
        if (!$Values.ContainsKey($key)) {
            throw "summary for sample=$SampleName is stale or incomplete; missing required metric: $key"
        }
    }
    if ([int]$Values["summarySchemaVersion"] -lt $requiredSummarySchemaVersion) {
        throw "summary for sample=$SampleName is stale; schema=$($Values["summarySchemaVersion"]) required=$requiredSummarySchemaVersion"
    }
    $expectedWindow = [ordered]@{
        startRegionX = $ExpectedStartX
        startRegionZ = $ExpectedStartZ
        cols = $ExpectedCols
        rows = $ExpectedRows
    }
    foreach ($key in $expectedWindow.Keys) {
        $actual = [int]$Values[$key]
        $expected = [int]$expectedWindow[$key]
        if ($actual -ne $expected) {
            throw "summary for sample=$SampleName targets stale or wrong window; $key=$actual expected=$expected"
        }
    }
}

function Get-QualityThresholds {
    param([string]$SampleName)
    $base = @{
        MaxSmallLandComponents = 120
        MaxSmallLandComponentShare = 0.20
        MaxSmallLandComponentPixelShare = 0.004
        MaxLongHorizontalRuns = 4
        MaxLongVerticalRuns = 2
        MaxBiomeTopMismatchRatio = 0.025
        MaxSmootherBiomeTopMismatchRatio = 0.018
        MaxVegetatedNonVegetationTopRatio = 0.015
        MaxSmootherRatio = 0.15
        MaxDryRockAccentRatio = $null
        MaxSnowTopRatio = $null
        MaxStoneGravelTopRatio = $null
        MinForestTaigaBiomeFamilyRatio = $null
        MaxSavannaBiomeFamilyRatio = $null
        MaxJungleBiomeFamilyRatio = $null
        MinSavannaGrasslandBiomeFamilyRatio = $null
        MinJungleForestBiomeFamilyRatio = $null
        MinJungleForestSwampBiomeFamilyRatio = $null
        MinTerrainTokenExportRatio = $null
        MinLushSurfaceFamilyRatio = $null
        MinSavannaDryTextureRatio = $null
        MaxVegetatedGrassDominanceRatio = $null
        MaxSandRatio = $null
        MinSandRatio = $null
    }
    switch ($SampleName) {
        "west-africa" {
            $base.MaxSmallLandComponents = 120
            $base.MaxLongHorizontalRuns = 5
            $base.MaxSandRatio = 0.62
            $base.MaxDryRockAccentRatio = 0.18
            $base.MinSavannaDryTextureRatio = 0.005
        }
        "sahara-core" {
            $base.MaxSmallLandComponents = 320
            $base.MaxSmallLandComponentShare = 0.75
            $base.MaxSmallLandComponentPixelShare = 0.006
            $base.MaxBiomeTopMismatchRatio = 0.035
            $base.MaxSmootherBiomeTopMismatchRatio = 0.025
            $base.MinSandRatio = 0.45
            $base.MaxSandRatio = 0.92
            $base.MaxDryRockAccentRatio = 0.18
        }
        "sahara-egypt-core" {
            $base.MaxSmallLandComponents = 120
            $base.MaxLongHorizontalRuns = 2
            $base.MaxBiomeTopMismatchRatio = 0.020
            $base.MaxSmootherBiomeTopMismatchRatio = 0.010
            $base.MinSandRatio = 0.55
            $base.MaxSandRatio = 0.99
            $base.MaxDryRockAccentRatio = 0.18
            $base.MaxSnowTopRatio = 0.005
            $base.MaxStoneGravelTopRatio = 0.08
            $base.MaxSavannaBiomeFamilyRatio = 0.03
            $base.MaxJungleBiomeFamilyRatio = 0.005
            $base.MinTerrainTokenExportRatio = 0.19
        }
        "sahel-edge" {
            $base.MaxSmallLandComponents = 90
            $base.MaxSandRatio = 0.55
            $base.MaxDryRockAccentRatio = 0.08
            $base.MinSavannaGrasslandBiomeFamilyRatio = 0.40
            $base.MinSavannaDryTextureRatio = 0.005
        }
        "sahel-chad-transition" {
            $base.MaxSmallLandComponents = 90
            $base.MaxLongHorizontalRuns = 8
            $base.MaxLongVerticalRuns = 3
            $base.MaxBiomeTopMismatchRatio = 0.012
            $base.MaxSmootherBiomeTopMismatchRatio = 0.008
            $base.MaxVegetatedNonVegetationTopRatio = 0.010
            $base.MaxSandRatio = 0.45
            $base.MaxDryRockAccentRatio = 0.08
            $base.MaxStoneGravelTopRatio = 0.04
            $base.MaxJungleBiomeFamilyRatio = 0.10
            $base.MinSavannaGrasslandBiomeFamilyRatio = 0.50
            $base.MinTerrainTokenExportRatio = 0.25
            $base.MinSavannaDryTextureRatio = 0.005
        }
        "congo-edge" {
            $base.MaxSmallLandComponents = 90
            $base.MaxBiomeTopMismatchRatio = 0.010
            $base.MaxSmootherBiomeTopMismatchRatio = 0.008
            $base.MaxVegetatedNonVegetationTopRatio = 0.010
            $base.MaxSandRatio = 0.05
            $base.MaxDryRockAccentRatio = 0.02
            $base.MinJungleForestBiomeFamilyRatio = 0.28
            $base.MinLushSurfaceFamilyRatio = 0.15
        }
        "congo-core" {
            $base.MaxSmallLandComponents = 90
            $base.MaxBiomeTopMismatchRatio = 0.008
            $base.MaxSmootherBiomeTopMismatchRatio = 0.006
            $base.MaxVegetatedNonVegetationTopRatio = 0.006
            $base.MaxSandRatio = 0.02
            $base.MaxDryRockAccentRatio = 0.01
            $base.MaxSavannaBiomeFamilyRatio = 0.20
            $base.MinJungleForestSwampBiomeFamilyRatio = 0.60
            $base.MinTerrainTokenExportRatio = 0.20
            $base.MinLushSurfaceFamilyRatio = 0.25
        }
        "nile-delta" {
            $base.MaxSmallLandComponents = 300
            $base.MaxSmallLandComponentShare = 0.35
            $base.MaxSmallLandComponentPixelShare = 0.008
            $base.MaxLongHorizontalRuns = 6
            $base.MaxBiomeTopMismatchRatio = 0.035
            $base.MaxSmootherBiomeTopMismatchRatio = 0.025
            $base.MaxSandRatio = 0.65
            $base.MaxDryRockAccentRatio = 0.18
        }
        "arabia-inland" {
            $base.MaxSmallLandComponents = 220
            $base.MaxSmallLandComponentShare = 0.28
            $base.MaxSmallLandComponentPixelShare = 0.014
            $base.MaxLongHorizontalRuns = 4
            $base.MaxBiomeTopMismatchRatio = 0.030
            $base.MaxSmootherBiomeTopMismatchRatio = 0.020
            $base.MinSandRatio = 0.25
            $base.MaxSandRatio = 0.92
            $base.MaxDryRockAccentRatio = 0.25
        }
        "arabia-coast" {
            $base.MaxSmallLandComponents = 260
            $base.MaxSmallLandComponentShare = 0.32
            $base.MaxSmallLandComponentPixelShare = 0.016
            $base.MaxLongHorizontalRuns = 5
            $base.MaxBiomeTopMismatchRatio = 0.035
            $base.MaxSmootherBiomeTopMismatchRatio = 0.024
            $base.MaxSandRatio = 0.84
            $base.MaxDryRockAccentRatio = 0.20
        }
        "arabia-desert" {
            $base.MaxSmallLandComponents = 250
            $base.MaxSmallLandComponentShare = 0.30
            $base.MaxSmallLandComponentPixelShare = 0.015
            $base.MaxLongHorizontalRuns = 5
            $base.MaxBiomeTopMismatchRatio = 0.035
            $base.MaxSmootherBiomeTopMismatchRatio = 0.022
            $base.MaxSandRatio = 0.80
            $base.MaxDryRockAccentRatio = 0.25
        }
        "australia-dry" {
            $base.MaxSmallLandComponents = 120
            $base.MaxLongHorizontalRuns = 5
            $base.MaxBiomeTopMismatchRatio = 0.030
            $base.MaxSmootherBiomeTopMismatchRatio = 0.022
            $base.MaxSandRatio = 0.45
            $base.MaxDryRockAccentRatio = 0.12
        }
        "south-africa-savanna" {
            $base.MaxSmallLandComponents = 120
            $base.MaxLongHorizontalRuns = 5
            $base.MaxBiomeTopMismatchRatio = 0.020
            $base.MaxSmootherBiomeTopMismatchRatio = 0.014
            $base.MaxVegetatedNonVegetationTopRatio = 0.012
            $base.MaxSandRatio = 0.18
            $base.MaxDryRockAccentRatio = 0.08
            $base.MinSavannaDryTextureRatio = 0.004
        }
        "amazon-edge" {
            $base.MaxSmallLandComponents = 90
            $base.MaxBiomeTopMismatchRatio = 0.006
            $base.MaxSmootherBiomeTopMismatchRatio = 0.004
            $base.MaxVegetatedNonVegetationTopRatio = 0.004
            $base.MaxSandRatio = 0.02
            $base.MaxDryRockAccentRatio = 0.01
            $base.MinLushSurfaceFamilyRatio = 0.20
        }
        "europe-forest" {
            $base.MaxSmallLandComponents = 90
            $base.MaxSmallLandComponentPixelShare = 0.010
            $base.MaxBiomeTopMismatchRatio = 0.010
            $base.MaxSmootherBiomeTopMismatchRatio = 0.008
            $base.MaxVegetatedNonVegetationTopRatio = 0.006
            $base.MaxSandRatio = 0.02
            $base.MaxDryRockAccentRatio = 0.015
            $base.MaxSnowTopRatio = 0.080
            $base.MaxStoneGravelTopRatio = 0.060
            $base.MinForestTaigaBiomeFamilyRatio = 0.40
            $base.MaxSavannaBiomeFamilyRatio = 0.008
            $base.MaxJungleBiomeFamilyRatio = 0.010
        }
        "europe-northsea-denmark" {
            $base.MaxSmallLandComponents = 20
            $base.MaxSmallLandComponentPixelShare = 0.002
            $base.MaxLongHorizontalRuns = 6
            $base.MaxBiomeTopMismatchRatio = 0.010
            $base.MaxSmootherBiomeTopMismatchRatio = 0.006
            $base.MaxVegetatedNonVegetationTopRatio = 0.006
            $base.MaxSandRatio = 0.04
            $base.MaxDryRockAccentRatio = 0.01
            $base.MaxSnowTopRatio = 0.020
            $base.MaxStoneGravelTopRatio = 0.04
            $base.MinForestTaigaBiomeFamilyRatio = 0.30
            $base.MaxSavannaBiomeFamilyRatio = 0.005
            $base.MaxJungleBiomeFamilyRatio = 0.005
            $base.MinTerrainTokenExportRatio = 0.15
        }
        "western-europe" {
            $base.MaxSmallLandComponents = 100
            $base.MaxSmallLandComponentPixelShare = 0.008
            $base.MaxBiomeTopMismatchRatio = 0.018
            $base.MaxSmootherBiomeTopMismatchRatio = 0.012
            $base.MaxVegetatedNonVegetationTopRatio = 0.008
            $base.MaxSandRatio = 0.62
            $base.MaxDryRockAccentRatio = 0.080
            $base.MaxSnowTopRatio = 0.060
            $base.MaxStoneGravelTopRatio = 0.060
        }
        "mediterranean-edge" {
            $base.MaxSmallLandComponents = 140
            $base.MaxSmallLandComponentPixelShare = 0.006
            $base.MaxBiomeTopMismatchRatio = 0.020
            $base.MaxSmootherBiomeTopMismatchRatio = 0.014
            $base.MaxVegetatedNonVegetationTopRatio = 0.012
            $base.MaxSandRatio = 0.66
            $base.MaxDryRockAccentRatio = 0.080
            $base.MaxSnowTopRatio = 0.020
            $base.MaxStoneGravelTopRatio = 0.080
            $base.MaxJungleBiomeFamilyRatio = 0.010
        }
        "med-italy-islands" {
            $base.MaxSmallLandComponents = 50
            $base.MaxSmallLandComponentPixelShare = 0.003
            $base.MaxBiomeTopMismatchRatio = 0.012
            $base.MaxSmootherBiomeTopMismatchRatio = 0.008
            $base.MaxVegetatedNonVegetationTopRatio = 0.003
            $base.MaxSandRatio = 0.58
            $base.MaxDryRockAccentRatio = 0.08
            $base.MaxSnowTopRatio = 0.010
            $base.MaxStoneGravelTopRatio = 0.08
            $base.MinForestTaigaBiomeFamilyRatio = 0.20
            $base.MaxSavannaBiomeFamilyRatio = 0.04
            $base.MaxJungleBiomeFamilyRatio = 0.005
            $base.MinTerrainTokenExportRatio = 0.25
        }
    }
    return $base
}

function Get-ValueOrZero {
    param(
        [hashtable]$Values,
        [string]$Key
    )
    if ($Values.ContainsKey($Key)) {
        return [double]$Values[$Key]
    }
    return 0.0
}

function Get-MagickPath {
    $candidates = @(
        "C:\Program Files\ImageMagick-7.1.2-Q16-HDRI\magick.exe",
        "C:\Program Files\ImageMagick-7.1.1-Q16-HDRI\magick.exe",
        "magick.exe",
        "magick"
    )
    foreach ($candidate in $candidates) {
        if ($candidate -match '^[A-Za-z]:\\') {
            if (Test-Path -LiteralPath $candidate) {
                return $candidate
            }
        } else {
            $command = Get-Command $candidate -ErrorAction SilentlyContinue
            if ($null -ne $command) {
                return $command.Source
            }
        }
    }
    throw "ImageMagick magick.exe was not found; required for MET-style Standard.png remap evidence."
}

function Get-StandardPalettePath {
    $candidates = @()
    if (![string]::IsNullOrWhiteSpace($env:EARTHMAP_DATA_ROOT)) {
        $candidates += (Join-Path $env:EARTHMAP_DATA_ROOT "wpscript\terrain\Standard.png")
    }
    $candidates += @(
        (Join-Path (Split-Path -Parent $repoRoot) "wpscript\terrain\Standard.png"),
        (Join-Path $repoRoot "wpscript\terrain\Standard.png")
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) {
            return $candidate
        }
    }
    throw "Standard.png palette was not found; required for MET-style photo parity."
}

function Read-PhotoMetricSection {
    param(
        [string]$MetricsPath,
        [string]$SectionName
    )
    if (!(Test-Path -LiteralPath $MetricsPath)) {
        throw "photo parity metrics missing: $MetricsPath"
    }
    $values = @{}
    $inside = $false
    foreach ($line in (Get-Content -LiteralPath $MetricsPath)) {
        if ($line -match '^\[(.+)\]$') {
            if ($inside) {
                break
            }
            $inside = $Matches[1] -eq $SectionName
            continue
        }
        if (!$inside -or [string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        if ($line -match '^([^=]+)=([+-]?(?:\d+(?:\.\d+)?|\.\d+)(?:[Ee][+-]?\d+)?)(?:\s+\(([+-]?(?:\d+(?:\.\d+)?|\.\d+)(?:[Ee][+-]?\d+)?)%\))?') {
            $key = $Matches[1].Trim()
            $values[$key] = [double]$Matches[2]
            if ($Matches.Count -ge 4 -and -not [string]::IsNullOrWhiteSpace($Matches[3])) {
                $values["$key.percent"] = [double]$Matches[3]
            }
        }
    }
    if ($values.Count -eq 0) {
        throw "photo parity metrics section missing or empty: [$SectionName] in $MetricsPath"
    }
    return $values
}

function Assert-PhotoParityMetric {
    param(
        [hashtable]$Values,
        [string]$Key,
        [double]$Limit,
        [ValidateSet("max", "min")]
        [string]$Mode
    )
    if (!$Values.ContainsKey($Key)) {
        throw "photo parity metric missing: $Key"
    }
    $actual = [double]$Values[$Key]
    if ($Mode -eq "max" -and $actual -gt $Limit) {
        throw ("photo parity failed: {0}={1:N6} > {2:N6}" -f $Key, $actual, $Limit)
    }
    if ($Mode -eq "min" -and $actual -lt $Limit) {
        throw ("photo parity failed: {0}={1:N6} < {2:N6}" -f $Key, $actual, $Limit)
    }
}

function Try-ReadPhotoMetricSection {
    param(
        [string]$MetricsPath,
        [string]$SectionName
    )
    try {
        return Read-PhotoMetricSection -MetricsPath $MetricsPath -SectionName $SectionName
    } catch {
        return $null
    }
}

function Assert-ProductionBatchArtifact {
    param(
        [string]$Path,
        [string]$Label
    )
    if (!(Test-Path -LiteralPath $Path)) {
        throw "$Label missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path
    if ($item.Length -le 0) {
        throw "$Label is empty: $Path"
    }
}

function Add-ProductionBatchMetricFailure {
    param(
        [System.Collections.Generic.List[string]]$Failures,
        [hashtable]$Values,
        [string]$Sample,
        [string]$Key,
        [double]$Limit,
        [ValidateSet("max", "min")]
        [string]$Mode
    )
    if (!$Values.ContainsKey($Key)) {
        $Failures.Add("sample=$Sample metric missing: $Key")
        return
    }
    $actual = [double]$Values[$Key]
    if ($Mode -eq "max" -and $actual -gt $Limit) {
        $Failures.Add(("sample={0} {1}={2:N6} > {3:N6}" -f $Sample, $Key, $actual, $Limit))
    }
    if ($Mode -eq "min" -and $actual -lt $Limit) {
        $Failures.Add(("sample={0} {1}={2:N6} < {3:N6}" -f $Sample, $Key, $actual, $Limit))
    }
}

function Assert-ProductionBatchMetricGate {
    param(
        [string]$BatchOutputRoot
    )
    $summaryCsv = Join-Path $BatchOutputRoot "quality-production-sample-summary.csv"
    $contactSheet = Join-Path $BatchOutputRoot "quality-production-sample-contact-sheet.png"
    Assert-ProductionBatchArtifact -Path $summaryCsv -Label "quality production sample summary"
    Assert-ProductionBatchArtifact -Path $contactSheet -Label "quality production sample contact sheet"
    $rows = @(Import-Csv -LiteralPath $summaryCsv)
    if ($rows.Count -eq 0) {
        throw "quality production sample summary has no samples: $summaryCsv"
    }
    $failures = New-Object System.Collections.Generic.List[string]
    foreach ($row in $rows) {
        $sample = [string]$row.sample
        if ([string]::IsNullOrWhiteSpace($sample)) {
            $failures.Add("summary row has empty sample name")
            continue
        }
        $metricDirectory = [string]$row.metricDirectory
        if ([string]::IsNullOrWhiteSpace($metricDirectory)) {
            $metricDirectory = Join-Path (Join-Path $BatchOutputRoot $sample) "photo-parity\metric-land"
        }
        $metricsPath = Join-Path $metricDirectory "metrics.txt"
        Assert-ProductionBatchArtifact -Path $metricsPath -Label "quality production sample metrics for $sample"
        $metrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "current-vs-expected"
        $localAverageMetrics = Try-ReadPhotoMetricSection -MetricsPath $metricsPath `
            -SectionName "current-local-average-4x4-vs-expected-local-average-4x4"
        $sourceMetrics = Try-ReadPhotoMetricSection -MetricsPath $metricsPath -SectionName "source-vs-expected"
        Write-Host ("qualityAcceptance.batchSample.metrics=sample={0},meanDeltaE2000={1:N6},p95DeltaE2000={2:N6},globalLumaSsim={3:N9},deltaEOver10Percent={4:N6},deltaEOver20Percent={5:N6},deltaEOver30Percent={6:N6},metrics={7}" -f `
                $sample,
                [double]$metrics["meanDeltaE2000"],
                [double]$metrics["p95DeltaE2000"],
                [double]$metrics["globalLumaSsim"],
                [double]$metrics["deltaEOver10.percent"],
                [double]$metrics["deltaEOver20.percent"],
                [double]$metrics["deltaEOver30.percent"],
                $metricsPath)
        if ($null -ne $localAverageMetrics) {
            Write-Host ("qualityAcceptance.batchSample.localAverageMetrics=sample={0},meanDeltaE2000={1:N6},p95DeltaE2000={2:N6},globalLumaSsim={3:N9},deltaEOver10Percent={4:N6},deltaEOver20Percent={5:N6},deltaEOver30Percent={6:N6}" -f `
                    $sample,
                    [double]$localAverageMetrics["meanDeltaE2000"],
                    [double]$localAverageMetrics["p95DeltaE2000"],
                    [double]$localAverageMetrics["globalLumaSsim"],
                    [double]$localAverageMetrics["deltaEOver10.percent"],
                    [double]$localAverageMetrics["deltaEOver20.percent"],
                    [double]$localAverageMetrics["deltaEOver30.percent"])
        }
        if ($null -ne $sourceMetrics) {
            Write-Host ("qualityAcceptance.batchSample.sourceBaseline=sample={0},meanDeltaE2000={1:N6},p95DeltaE2000={2:N6},globalLumaSsim={3:N9}" -f `
                    $sample,
                    [double]$sourceMetrics["meanDeltaE2000"],
                    [double]$sourceMetrics["p95DeltaE2000"],
                    [double]$sourceMetrics["globalLumaSsim"])
        }
        if (![string]::IsNullOrWhiteSpace([string]$row.productionSourceVsReferenceMean) `
                -and [string]$row.productionSourceVsReferenceMean -ne "nan") {
            Write-Host ("qualityAcceptance.batchSample.productionSource=sample={0},sourceVsReferenceMean={1},sourceVsExpectedMean={2},sourceDebug={3}" -f `
                    $sample,
                    [string]$row.productionSourceVsReferenceMean,
                    [string]$row.productionSourceVsExpectedMean,
                    [string]$row.productionSourceDebug)
        }
        Add-ProductionBatchMetricFailure -Failures $failures -Values $metrics -Sample $sample `
            -Key "meanDeltaE2000" -Limit 6.40 -Mode max
        Add-ProductionBatchMetricFailure -Failures $failures -Values $metrics -Sample $sample `
            -Key "p95DeltaE2000" -Limit 10.20 -Mode max
        Add-ProductionBatchMetricFailure -Failures $failures -Values $metrics -Sample $sample `
            -Key "globalLumaSsim" -Limit 0.9700 -Mode min
        Add-ProductionBatchMetricFailure -Failures $failures -Values $metrics -Sample $sample `
            -Key "deltaEOver10.percent" -Limit 12.0 -Mode max
        Add-ProductionBatchMetricFailure -Failures $failures -Values $metrics -Sample $sample `
            -Key "deltaEOver20.percent" -Limit 0.12 -Mode max
        Add-ProductionBatchMetricFailure -Failures $failures -Values $metrics -Sample $sample `
            -Key "deltaEOver30.percent" -Limit 0.0 -Mode max
    }
    if ($failures.Count -gt 0) {
        foreach ($failure in $failures) {
            Write-Host "qualityAcceptance.batchGate.failure=$failure"
        }
        Write-Host ("qualityAcceptance.batchGate=fail,failures={0},summary={1},contactSheet={2}" -f `
                $failures.Count, $summaryCsv, $contactSheet)
        throw "quality-production-sample-batch quality gate failed; failures=$($failures.Count)"
    }
    Write-Host ("qualityAcceptance.batchGate=pass,samples={0},summary={1},contactSheet={2}" -f `
            $rows.Count, $summaryCsv, $contactSheet)
}

if (-not [string]::IsNullOrWhiteSpace($ProductionSamplesCsv) -and -not $LegacyPerSample) {
    Write-Host "qualityAcceptance.batchMode=quality-production-sample-batch"
    if (!$SkipGeneration) {
        & $run quality-production-sample-batch `
            $ProductionSamplesCsv `
            $Heightmap `
            $OutputRoot `
            $Scale `
            $Format `
            $Threads `
            "cacheRows=$CacheRows" `
            "prefetchRows=$PrefetchRows" `
            "verticalScale=$VerticalScale" `
            "textureMode=$TextureMode" `
            "surfaceRaster=auto" `
            "chunkStatus=surface" `
            "metricMode=current-only" `
            "previewDebug=$ProductionPreviewDebug"
        if ($LASTEXITCODE -ne 0) {
            throw "quality-production-sample-batch failed with exit code $LASTEXITCODE"
        }
    } else {
        Write-Host "qualityAcceptance.batchGeneration=skipped,reason=SkipGeneration"
    }
    if (!$NoQualityGate) {
        Assert-ProductionBatchMetricGate -BatchOutputRoot $OutputRoot
    } else {
        Write-Host "qualityAcceptance.batchGate=skipped,reason=NoQualityGate"
    }
    if ($SkipGeneration) {
        throw "quality acceptance sample gate is invalid release evidence because -SkipGeneration was used; rerun without -SkipGeneration for fresh evidence"
    }
    return
}

function Invoke-SamplePhotoParityGate {
    param(
        [string]$SampleName,
        [string]$World,
        [string]$PreviewDebug,
        [int]$StartX,
        [int]$StartZ,
        [int]$Cols,
        [int]$Rows,
        [Nullable[datetime]]$NotOlderThan = $null,
        [switch]$NoThresholdGate
    )
    $sourcePng = Join-Path $PreviewDebug "source-color.png"
    $landWaterPng = Join-Path $PreviewDebug "land-water.png"
    Assert-RequiredArtifact -Path $sourcePng -Label "photo parity source-color mosaic" -NotOlderThan $NotOlderThan
    Assert-RequiredArtifact -Path $landWaterPng -Label "photo parity land-water mosaic" -NotOlderThan $NotOlderThan

    $parityDir = Join-Path $World "photo-parity"
    New-Item -ItemType Directory -Force -Path $parityDir | Out-Null
    $expectedPng = Join-Path $parityDir "expected-standard-imagemagick.png"
    $landMaskPng = Join-Path $parityDir "land-mask.png"
    $mcaRenderPng = Join-Path $parityDir "mca-visible-topdown-biome-tint.png"
    $mcaRenderLog = Join-Path $parityDir "mca-topdown-render.log"
    $metricDir = Join-Path $parityDir "metric-direct-mca-standard"
    $metricLog = Join-Path $parityDir "photo-parity-metric.log"
    $metricsPath = Join-Path $metricDir "metrics.txt"
    $worldForMcaRender = $World
    $magick = Get-MagickPath
    $standardPalette = Get-StandardPalettePath

    Write-Host ("photoParity.sample.prepare=sample={0},source={1},expected={2},mask={3},current={4}" -f `
            $SampleName, $sourcePng, $expectedPng, $landMaskPng, $mcaRenderPng)
    $expectedResult = Invoke-LoggedCommand -LogPath (Join-Path $parityDir "expected-standard-imagemagick.log") -Command {
        & $magick $sourcePng -dither None -remap $standardPalette $expectedPng
    }
    if ($expectedResult.ExitCode -ne 0) {
        throw ("ImageMagick Standard remap failed for sample={0}; hint={1}; log={2}" -f `
                $SampleName, $expectedResult.FailureHint, $expectedResult.LogPath)
    }
    $maskResult = Invoke-LoggedCommand -LogPath (Join-Path $parityDir "land-mask.log") -Command {
        & $magick $landWaterPng -alpha off -fill black -opaque "#265C97" -fill black -opaque "#597EB1" `
            -fill white +opaque black $landMaskPng
    }
    if ($maskResult.ExitCode -ne 0) {
        throw ("land mask generation failed for sample={0}; hint={1}; log={2}" -f `
                $SampleName, $maskResult.FailureHint, $maskResult.LogPath)
    }
    $renderResult = Invoke-LoggedCommand -LogPath $mcaRenderLog -Command {
        & $run mca-topdown-render $worldForMcaRender $mcaRenderPng $StartX $StartZ $Cols $Rows visible
    }
    if ($renderResult.ExitCode -ne 0) {
        throw ("direct MCA topdown render failed for sample={0}; hint={1}; log={2}" -f `
                $SampleName, $renderResult.FailureHint, $renderResult.LogPath)
    }
    $metricResult = Invoke-LoggedCommand -LogPath $metricLog -Command {
        & $photoMetricScript `
            -ProjectRoot $repoRoot `
            -SourcePng $sourcePng `
            -ExpectedPng $expectedPng `
            -CurrentSurfacePng $mcaRenderPng `
            -OutputDir $metricDir `
            -CropX 0 `
            -CropY 0 `
            -CropWidth 512 `
            -CropHeight 512 `
            -MaskPng $landMaskPng `
            -MaskMode nonzero
    }
    if ($metricResult.ExitCode -ne 0) {
        throw ("photo parity metric failed for sample={0}; hint={1}; log={2}" -f `
                $SampleName, $metricResult.FailureHint, $metricResult.LogPath)
    }
    Assert-RequiredArtifact -Path $expectedPng -Label "photo parity expected Standard remap" -NotOlderThan $NotOlderThan
    Assert-RequiredArtifact -Path $landMaskPng -Label "photo parity land mask" -NotOlderThan $NotOlderThan
    Assert-RequiredArtifact -Path $mcaRenderPng -Label "photo parity direct MCA render" -NotOlderThan $NotOlderThan
    Assert-RequiredArtifact -Path $metricsPath -Label "photo parity metrics" -NotOlderThan $NotOlderThan
    $candidateDitherPng = Join-Path $metricDir "candidate-dither-remap.png"
    Assert-RequiredArtifact -Path $candidateDitherPng -Label "photo parity ordered dither candidate" -NotOlderThan $NotOlderThan
    $candidateSelectivePng = Join-Path $metricDir "candidate-token-recipe-selective-source-rank-4x4.png"
    $candidateSelectiveSummary = Join-Path $metricDir "candidate-token-recipe-selective-summary.csv"
    Assert-RequiredArtifact -Path $candidateSelectivePng -Label "photo parity selective token recipe candidate" -NotOlderThan $NotOlderThan
    Assert-RequiredArtifact -Path $candidateSelectiveSummary -Label "photo parity selective token recipe summary" -NotOlderThan $NotOlderThan
    $candidateProvisionalPng = Join-Path $metricDir "candidate-token-recipe-provisional-source-rank-4x4.png"
    Assert-RequiredArtifact -Path $candidateProvisionalPng -Label "photo parity cross-crop provisional token recipe candidate" -NotOlderThan $NotOlderThan

    $metrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "current-vs-expected"
    $sourceTruthMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "current-vs-source"
    $candidateMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "candidate-ordered-source-standard-blend-25-4x4-vs-expected"
    $candidateLocalAverageMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "candidate-ordered-source-standard-blend-25-4x4-local-average-vs-expected-local-average"
    $candidateSourceTruthMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "candidate-token-recipe-source-rank-4x4-vs-source"
    $candidateSourceTruthLocalAverageMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "candidate-token-recipe-source-rank-4x4-local-average-vs-source-local-average"
    $candidateSelectiveMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "candidate-token-recipe-selective-source-rank-4x4-vs-expected"
    $candidateSelectiveSourceTruthMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "candidate-token-recipe-selective-source-rank-4x4-vs-source"
    $candidateSelectiveSourceTruthLocalAverageMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "candidate-token-recipe-selective-source-rank-4x4-local-average-vs-source-local-average"
    $candidateProvisionalMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "candidate-token-recipe-provisional-source-rank-4x4-vs-expected"
    $candidateProvisionalSourceTruthMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "candidate-token-recipe-provisional-source-rank-4x4-vs-source"
    $candidateProvisionalSourceTruthLocalAverageMetrics = Read-PhotoMetricSection -MetricsPath $metricsPath -SectionName "candidate-token-recipe-provisional-source-rank-4x4-local-average-vs-source-local-average"
    Write-Host ("photoParity.sample.metrics=sample={0},meanDeltaE2000={1:N6},p95DeltaE2000={2:N6},globalLumaSsim={3:N9},deltaEOver10Percent={4:N6},deltaEOver20Percent={5:N6},deltaEOver30Percent={6:N6},metrics={7}" -f `
            $SampleName,
            [double]$metrics["meanDeltaE2000"],
            [double]$metrics["p95DeltaE2000"],
            [double]$metrics["globalLumaSsim"],
            [double]$metrics["deltaEOver10.percent"],
            [double]$metrics["deltaEOver20.percent"],
            [double]$metrics["deltaEOver30.percent"],
            $metricsPath)
    Write-Host ("photoParity.sample.selectiveMetrics=sample={0},meanDeltaE2000={1:N6},sourceMeanDeltaE2000={2:N6},sourceLocalMeanDeltaE2000={3:N6},globalLumaSsim={4:N9},candidate={5}" -f `
            $SampleName,
            [double]$candidateSelectiveMetrics["meanDeltaE2000"],
            [double]$candidateSelectiveSourceTruthMetrics["meanDeltaE2000"],
            [double]$candidateSelectiveSourceTruthLocalAverageMetrics["meanDeltaE2000"],
            [double]$candidateSelectiveSourceTruthMetrics["globalLumaSsim"],
            $candidateSelectivePng)
    Write-Host ("photoParity.sample.provisionalMetrics=sample={0},meanDeltaE2000={1:N6},sourceMeanDeltaE2000={2:N6},sourceLocalMeanDeltaE2000={3:N6},globalLumaSsim={4:N9},candidate={5}" -f `
            $SampleName,
            [double]$candidateProvisionalMetrics["meanDeltaE2000"],
            [double]$candidateProvisionalSourceTruthMetrics["meanDeltaE2000"],
            [double]$candidateProvisionalSourceTruthLocalAverageMetrics["meanDeltaE2000"],
            [double]$candidateProvisionalSourceTruthMetrics["globalLumaSsim"],
            $candidateProvisionalPng)
    if (!$NoThresholdGate) {
        Assert-PhotoParityMetric -Values $metrics -Key "meanDeltaE2000" -Limit 6.40 -Mode max
        Assert-PhotoParityMetric -Values $metrics -Key "p95DeltaE2000" -Limit 10.20 -Mode max
        Assert-PhotoParityMetric -Values $metrics -Key "globalLumaSsim" -Limit 0.9700 -Mode min
        Assert-PhotoParityMetric -Values $metrics -Key "deltaEOver10.percent" -Limit 12.0 -Mode max
        Assert-PhotoParityMetric -Values $metrics -Key "deltaEOver20.percent" -Limit 0.12 -Mode max
        Assert-PhotoParityMetric -Values $metrics -Key "deltaEOver30.percent" -Limit 0.0 -Mode max
    }

    $evidence = [ordered]@{
        schemaVersion = 1
        generatedAt = (Get-Date).ToString("o")
        sample = $SampleName
        sourcePng = $sourcePng
        expectedPng = $expectedPng
        currentPng = $mcaRenderPng
        candidateDitherPng = $candidateDitherPng
        candidateSelectivePng = $candidateSelectivePng
        candidateSelectiveSummary = $candidateSelectiveSummary
        candidateProvisionalPng = $candidateProvisionalPng
        maskPng = $landMaskPng
        metricsPath = $metricsPath
        metricLog = $metricLog
        mcaRenderLog = $mcaRenderLog
        thresholdGateSkipped = [bool]$NoThresholdGate
        thresholds = [ordered]@{
            maxMeanDeltaE2000 = 6.40
            maxP95DeltaE2000 = 10.20
            minGlobalLumaSsim = 0.9700
            maxDeltaEOver10Percent = 12.0
            maxDeltaEOver20Percent = 0.12
            maxDeltaEOver30Percent = 0.0
        }
        metrics = $metrics
        sourceTruthMetrics = $sourceTruthMetrics
        candidateOrderedBlend25_4x4Metrics = $candidateMetrics
        candidateOrderedBlend25_4x4LocalAverageMetrics = $candidateLocalAverageMetrics
        candidateTokenRecipeSourceTruthMetrics = $candidateSourceTruthMetrics
        candidateTokenRecipeSourceTruthLocalAverageMetrics = $candidateSourceTruthLocalAverageMetrics
        candidateSelectiveMetrics = $candidateSelectiveMetrics
        candidateSelectiveSourceTruthMetrics = $candidateSelectiveSourceTruthMetrics
        candidateSelectiveSourceTruthLocalAverageMetrics = $candidateSelectiveSourceTruthLocalAverageMetrics
        candidateProvisionalMetrics = $candidateProvisionalMetrics
        candidateProvisionalSourceTruthMetrics = $candidateProvisionalSourceTruthMetrics
        candidateProvisionalSourceTruthLocalAverageMetrics = $candidateProvisionalSourceTruthLocalAverageMetrics
    }
    $evidencePath = Join-Path $parityDir "photo-parity-evidence.json"
    ($evidence | ConvertTo-Json -Depth 5) | Set-Content -LiteralPath $evidencePath -Encoding UTF8
    Write-Host "photoParity.sample.evidence=$evidencePath"
    if ($NoThresholdGate) {
        Write-Host "photoParity.sample.gate=skipped,reason=PhotoParityEvidenceOnly"
    } else {
        Write-Host "photoParity.sample.gate=pass"
    }
}

function Get-DecisionSourcePrefixCount {
    param(
        [hashtable]$Values,
        [string]$Prefix
    )
    $total = 0.0
    foreach ($key in $Values.Keys) {
        if ($key.StartsWith("aggregate.decisionSource.$Prefix")) {
            $total += [double]$Values[$key]
        }
    }
    return $total
}

function Get-FamilyNonVegetationTopColumns {
    param(
        [hashtable]$Values,
        [string]$Family
    )
    $nonVegetationTops = @(
        "sand",
        "red_sand",
        "stone",
        "gravel",
        "terracotta",
        "orange_terracotta",
        "brown_terracotta",
        "snow_block"
    )
    $total = 0.0
    foreach ($top in $nonVegetationTops) {
        $total += Get-ValueOrZero $Values "aggregate.familyTop.$Family.$top"
    }
    return $total
}

function Add-MatrixContractFailures {
    param(
        [hashtable]$Values,
        [System.Collections.Generic.List[string]]$Failures
    )

    $snowGrass = Get-ValueOrZero $Values "aggregate.familyTop.snow.grass_block"
    if ($snowGrass -gt 0.0) {
        $Failures.Add("matrix contract failed: snow biome-family has grass_block top columns=$snowGrass")
    }

    $vegetatedFamilyLimits = @{
        forest = @{
            ColumnsKey = "aggregate.biomeFamilyForestColumns"
            MaxNonVegetationTopRatio = 0.003
        }
        jungle = @{
            ColumnsKey = "aggregate.biomeFamilyJungleColumns"
            MaxNonVegetationTopRatio = 0.003
        }
        savanna = @{
            ColumnsKey = "aggregate.biomeFamilySavannaColumns"
            MaxNonVegetationTopRatio = 0.005
        }
    }
    foreach ($family in $vegetatedFamilyLimits.Keys) {
        $familyColumns = Get-ValueOrZero $Values $vegetatedFamilyLimits[$family].ColumnsKey
        if ($familyColumns -le 0.0) {
            continue
        }
        $badTopColumns = Get-FamilyNonVegetationTopColumns -Values $Values -Family $family
        $badTopRatio = $badTopColumns / $familyColumns
        $limit = [double]$vegetatedFamilyLimits[$family].MaxNonVegetationTopRatio
        if ($badTopRatio -gt $limit) {
            $Failures.Add(("matrix contract failed: {0} non-vegetation top ratio={1:N4} > {2:N4}" -f $family, $badTopRatio, $limit))
        }
    }
}

function Assert-QualityGate {
    param(
        [string]$SampleName,
        [hashtable]$Values
    )
    if (!$Values.ContainsKey("aggregate.landColumns")) {
        throw "quality gate cannot run without aggregate.landColumns for sample=$SampleName"
    }
    $landColumns = [double]$Values["aggregate.landColumns"]
    if ($landColumns -le 0.0) {
        Write-Host "qualityGate=skipped,no-land-columns"
        return
    }

    $threshold = Get-QualityThresholds -SampleName $SampleName
    $failures = New-Object System.Collections.Generic.List[string]
    Add-MatrixContractFailures -Values $Values -Failures $failures

    $small = Get-ValueOrZero $Values "mosaicSmallLandComponents"
    $smallPixels = Get-ValueOrZero $Values "mosaicSmallLandComponentPixels"
    $smallPixelShare = $smallPixels / $landColumns
    Write-Host ("smallLandComponentPixelShare={0:N4}" -f $smallPixelShare)
    if ($smallPixelShare -gt $threshold.MaxSmallLandComponentPixelShare) {
        $failures.Add(("smallLandComponentPixelShare={0:N4} > {1:N4}" -f $smallPixelShare, $threshold.MaxSmallLandComponentPixelShare))
    }
    if ($small -gt $threshold.MaxSmallLandComponents -and $smallPixelShare -le $threshold.MaxSmallLandComponentPixelShare) {
        Write-Host "qualityGate.notice.$SampleName=small component count is high but small-pixel area remains within the visual-area gate"
    } elseif ($small -gt $threshold.MaxSmallLandComponents) {
        $failures.Add("mosaicSmallLandComponents=$small > $($threshold.MaxSmallLandComponents) with excessive small-pixel area")
    }
    $landComponents = Get-ValueOrZero $Values "mosaicLandComponents"
    if ($landComponents -gt 0.0) {
        $smallShare = $small / $landComponents
        if ($smallShare -gt $threshold.MaxSmallLandComponentShare -and $smallPixelShare -gt $threshold.MaxSmallLandComponentPixelShare) {
            $failures.Add(("smallLandComponentShare={0:N4} > {1:N4}" -f $smallShare, $threshold.MaxSmallLandComponentShare))
        } elseif ($smallShare -gt $threshold.MaxSmallLandComponentShare) {
            Write-Host "qualityGate.notice.$SampleName=small component share is high by count, but pixel area is small enough for rocky/biome accent texture"
        }
    }

    $longH = Get-ValueOrZero $Values "mosaicLongHorizontalBiomeBoundaryRuns"
    if ($longH -gt $threshold.MaxLongHorizontalRuns) {
        $failures.Add("mosaicLongHorizontalBiomeBoundaryRuns=$longH > $($threshold.MaxLongHorizontalRuns)")
    }

    $longV = Get-ValueOrZero $Values "mosaicLongVerticalBiomeBoundaryRuns"
    if ($longV -gt $threshold.MaxLongVerticalRuns) {
        $failures.Add("mosaicLongVerticalBiomeBoundaryRuns=$longV > $($threshold.MaxLongVerticalRuns)")
    }

    $sandRatio = (Get-ValueOrZero $Values "aggregate.sandTopColumns") / $landColumns
    if ($null -ne $threshold.MaxSandRatio -and $sandRatio -gt $threshold.MaxSandRatio) {
        $failures.Add(("sandTopRatio={0:N4} > {1:N4}" -f $sandRatio, $threshold.MaxSandRatio))
    }
    if ($null -ne $threshold.MinSandRatio -and $sandRatio -lt $threshold.MinSandRatio) {
        $failures.Add(("sandTopRatio={0:N4} < {1:N4}" -f $sandRatio, $threshold.MinSandRatio))
    }

    $dryRockAccentColumns = (Get-ValueOrZero $Values "aggregate.redSandTopColumns") +
            (Get-ValueOrZero $Values "aggregate.terracottaTopColumns") +
            (Get-ValueOrZero $Values "aggregate.orangeTerracottaTopColumns") +
            (Get-ValueOrZero $Values "aggregate.brownTerracottaTopColumns")
    $dryRockAccentRatio = $dryRockAccentColumns / $landColumns
    Write-Host ("dryRockAccentTopRatio={0:N4}" -f $dryRockAccentRatio)
    if ($null -ne $threshold.MaxDryRockAccentRatio -and $dryRockAccentRatio -gt $threshold.MaxDryRockAccentRatio) {
        $failures.Add(("dryRockAccentTopRatio={0:N4} > {1:N4}" -f $dryRockAccentRatio, $threshold.MaxDryRockAccentRatio))
    }

    $snowTopRatio = (Get-ValueOrZero $Values "aggregate.snowTopColumns") / $landColumns
    Write-Host ("snowTopRatio={0:N4}" -f $snowTopRatio)
    if ($null -ne $threshold.MaxSnowTopRatio -and $snowTopRatio -gt $threshold.MaxSnowTopRatio) {
        $failures.Add(("snowTopRatio={0:N4} > {1:N4}" -f $snowTopRatio, $threshold.MaxSnowTopRatio))
    }

    $stoneGravelRatio = ((Get-ValueOrZero $Values "aggregate.stoneTopColumns") +
            (Get-ValueOrZero $Values "aggregate.gravelTopColumns")) / $landColumns
    Write-Host ("stoneGravelTopRatio={0:N4}" -f $stoneGravelRatio)
    if ($null -ne $threshold.MaxStoneGravelTopRatio -and $stoneGravelRatio -gt $threshold.MaxStoneGravelTopRatio) {
        $failures.Add(("stoneGravelTopRatio={0:N4} > {1:N4}" -f $stoneGravelRatio, $threshold.MaxStoneGravelTopRatio))
    }

    $forestTaigaRatio = ((Get-ValueOrZero $Values "aggregate.biomeFamilyForestColumns") +
            (Get-ValueOrZero $Values "aggregate.biomeFamilyTaigaColumns")) / $landColumns
    Write-Host ("forestTaigaBiomeFamilyRatio={0:N4}" -f $forestTaigaRatio)
    if ($null -ne $threshold.MinForestTaigaBiomeFamilyRatio -and $forestTaigaRatio -lt $threshold.MinForestTaigaBiomeFamilyRatio) {
        $failures.Add(("forestTaigaBiomeFamilyRatio={0:N4} < {1:N4}" -f $forestTaigaRatio, $threshold.MinForestTaigaBiomeFamilyRatio))
    }

    $savannaFamilyRatio = (Get-ValueOrZero $Values "aggregate.biomeFamilySavannaColumns") / $landColumns
    $jungleFamilyRatio = (Get-ValueOrZero $Values "aggregate.biomeFamilyJungleColumns") / $landColumns
    $savannaGrasslandRatio = ((Get-ValueOrZero $Values "aggregate.biomeFamilySavannaColumns") +
            (Get-ValueOrZero $Values "aggregate.biomeFamilyGrasslandColumns")) / $landColumns
    $jungleForestRatio = ((Get-ValueOrZero $Values "aggregate.biomeFamilyJungleColumns") +
            (Get-ValueOrZero $Values "aggregate.biomeFamilyForestColumns")) / $landColumns
    $jungleForestSwampRatio = ((Get-ValueOrZero $Values "aggregate.biomeFamilyJungleColumns") +
            (Get-ValueOrZero $Values "aggregate.biomeFamilyForestColumns") +
            (Get-ValueOrZero $Values "aggregate.biomeFamilySwampColumns")) / $landColumns
    Write-Host ("savannaBiomeFamilyRatio={0:N4}" -f $savannaFamilyRatio)
    Write-Host ("jungleBiomeFamilyRatio={0:N4}" -f $jungleFamilyRatio)
    Write-Host ("savannaGrasslandBiomeFamilyRatio={0:N4}" -f $savannaGrasslandRatio)
    Write-Host ("jungleForestBiomeFamilyRatio={0:N4}" -f $jungleForestRatio)
    Write-Host ("jungleForestSwampBiomeFamilyRatio={0:N4}" -f $jungleForestSwampRatio)
    if ($null -ne $threshold.MaxSavannaBiomeFamilyRatio -and $savannaFamilyRatio -gt $threshold.MaxSavannaBiomeFamilyRatio) {
        $failures.Add(("savannaBiomeFamilyRatio={0:N4} > {1:N4}" -f $savannaFamilyRatio, $threshold.MaxSavannaBiomeFamilyRatio))
    }
    if ($null -ne $threshold.MaxJungleBiomeFamilyRatio -and $jungleFamilyRatio -gt $threshold.MaxJungleBiomeFamilyRatio) {
        $failures.Add(("jungleBiomeFamilyRatio={0:N4} > {1:N4}" -f $jungleFamilyRatio, $threshold.MaxJungleBiomeFamilyRatio))
    }
    if ($null -ne $threshold.MinSavannaGrasslandBiomeFamilyRatio -and $savannaGrasslandRatio -lt $threshold.MinSavannaGrasslandBiomeFamilyRatio) {
        $failures.Add(("savannaGrasslandBiomeFamilyRatio={0:N4} < {1:N4}" -f $savannaGrasslandRatio, $threshold.MinSavannaGrasslandBiomeFamilyRatio))
    }
    if ($null -ne $threshold.MinJungleForestBiomeFamilyRatio -and $jungleForestRatio -lt $threshold.MinJungleForestBiomeFamilyRatio) {
        $failures.Add(("jungleForestBiomeFamilyRatio={0:N4} < {1:N4}" -f $jungleForestRatio, $threshold.MinJungleForestBiomeFamilyRatio))
    }
    if ($null -ne $threshold.MinJungleForestSwampBiomeFamilyRatio -and $jungleForestSwampRatio -lt $threshold.MinJungleForestSwampBiomeFamilyRatio) {
        $failures.Add(("jungleForestSwampBiomeFamilyRatio={0:N4} < {1:N4}" -f $jungleForestSwampRatio, $threshold.MinJungleForestSwampBiomeFamilyRatio))
    }

    $lushFamilyColumns = (Get-ValueOrZero $Values "aggregate.biomeFamilyJungleColumns") +
            (Get-ValueOrZero $Values "aggregate.biomeFamilyForestColumns") +
            (Get-ValueOrZero $Values "aggregate.biomeFamilyTaigaColumns")
    $lushSurfaceColumns = (Get-ValueOrZero $Values "aggregate.familyTop.jungle.moss_block") +
            (Get-ValueOrZero $Values "aggregate.familyTop.jungle.podzol") +
            (Get-ValueOrZero $Values "aggregate.familyTop.forest.moss_block") +
            (Get-ValueOrZero $Values "aggregate.familyTop.forest.podzol") +
            (Get-ValueOrZero $Values "aggregate.familyTop.taiga.moss_block") +
            (Get-ValueOrZero $Values "aggregate.familyTop.taiga.podzol")
    $lushSurfaceFamilyRatio = $lushSurfaceColumns / [Math]::Max(1.0, $lushFamilyColumns)
    $savannaColumns = Get-ValueOrZero $Values "aggregate.biomeFamilySavannaColumns"
    $savannaDryTextureRatio = (Get-ValueOrZero $Values "aggregate.familyTop.savanna.coarse_dirt") /
            [Math]::Max(1.0, $savannaColumns)
    $vegetatedFamilyColumns = $lushFamilyColumns + $savannaColumns +
            (Get-ValueOrZero $Values "aggregate.biomeFamilyGrasslandColumns")
    $vegetatedGrassColumns = (Get-ValueOrZero $Values "aggregate.familyTop.jungle.grass_block") +
            (Get-ValueOrZero $Values "aggregate.familyTop.forest.grass_block") +
            (Get-ValueOrZero $Values "aggregate.familyTop.taiga.grass_block") +
            (Get-ValueOrZero $Values "aggregate.familyTop.savanna.grass_block") +
            (Get-ValueOrZero $Values "aggregate.familyTop.grassland.grass_block")
    $vegetatedGrassDominanceRatio = $vegetatedGrassColumns / [Math]::Max(1.0, $vegetatedFamilyColumns)
    Write-Host ("lushSurfaceFamilyRatio={0:N4}" -f $lushSurfaceFamilyRatio)
    Write-Host ("savannaDryTextureRatio={0:N4}" -f $savannaDryTextureRatio)
    Write-Host ("vegetatedGrassDominanceRatio={0:N4}" -f $vegetatedGrassDominanceRatio)
    if ($null -ne $threshold.MinLushSurfaceFamilyRatio -and $lushSurfaceFamilyRatio -lt $threshold.MinLushSurfaceFamilyRatio) {
        $failures.Add(("lushSurfaceFamilyRatio={0:N4} < {1:N4}" -f $lushSurfaceFamilyRatio, $threshold.MinLushSurfaceFamilyRatio))
    }
    if ($null -ne $threshold.MinSavannaDryTextureRatio -and $savannaDryTextureRatio -lt $threshold.MinSavannaDryTextureRatio) {
        $failures.Add(("savannaDryTextureRatio={0:N4} < {1:N4}" -f $savannaDryTextureRatio, $threshold.MinSavannaDryTextureRatio))
    }
    if ($null -ne $threshold.MaxVegetatedGrassDominanceRatio -and $vegetatedGrassDominanceRatio -gt $threshold.MaxVegetatedGrassDominanceRatio) {
        $failures.Add(("vegetatedGrassDominanceRatio={0:N4} > {1:N4}" -f $vegetatedGrassDominanceRatio, $threshold.MaxVegetatedGrassDominanceRatio))
    }

    $mismatchRatio = (Get-ValueOrZero $Values "aggregate.biomeTopMismatchColumns") / $landColumns
    if ($mismatchRatio -gt $threshold.MaxBiomeTopMismatchRatio) {
        $failures.Add(("biomeTopMismatchRatio={0:N4} > {1:N4}" -f $mismatchRatio, $threshold.MaxBiomeTopMismatchRatio))
    }

    $smootherMismatchRatio = (Get-ValueOrZero $Values "aggregate.smootherBiomeTopMismatchColumns") / $landColumns
    if ($smootherMismatchRatio -gt $threshold.MaxSmootherBiomeTopMismatchRatio) {
        $failures.Add(("smootherBiomeTopMismatchRatio={0:N4} > {1:N4}" -f $smootherMismatchRatio, $threshold.MaxSmootherBiomeTopMismatchRatio))
    }

    $vegetatedMismatchRatio = (Get-ValueOrZero $Values "aggregate.vegetatedBiomeNonVegetationTopColumns") / $landColumns
    if ($vegetatedMismatchRatio -gt $threshold.MaxVegetatedNonVegetationTopRatio) {
        $failures.Add(("vegetatedBiomeNonVegetationTopRatio={0:N4} > {1:N4}" -f $vegetatedMismatchRatio, $threshold.MaxVegetatedNonVegetationTopRatio))
    }

    $smootherRatio = (Get-DecisionSourcePrefixCount $Values "smoother") / $landColumns
    $intentStabilizedRatio = (Get-DecisionSourcePrefixCount $Values "intent-stabilized") / $landColumns
    Write-Host ("intentStabilizedDecisionRatio={0:N4}" -f $intentStabilizedRatio)
    Write-Host ("postFinalSmootherDecisionRatio={0:N4}" -f $smootherRatio)
    if ($smootherRatio -gt $threshold.MaxSmootherRatio) {
        $failures.Add(("smootherDecisionRatio={0:N4} > {1:N4}" -f $smootherRatio, $threshold.MaxSmootherRatio))
    }

    $terrainTokenCoverageRatio = (Get-ValueOrZero $Values "aggregate.landTerrainTokenAvailableColumns") / $landColumns
    $terrainTokenExportRatio = (Get-ValueOrZero $Values "aggregate.landTerrainTokenExportColumns") / $landColumns
    $terrainTokenJavaRatio = (Get-ValueOrZero $Values "aggregate.landTerrainTokenJavaStandardColumns") / $landColumns
    Write-Host ("terrainTokenCoverageRatio={0:N4}" -f $terrainTokenCoverageRatio)
    Write-Host ("terrainTokenExportCoverageRatio={0:N4}" -f $terrainTokenExportRatio)
    Write-Host ("terrainTokenJavaStandardCoverageRatio={0:N4}" -f $terrainTokenJavaRatio)
    if ($terrainTokenExportRatio -le 0.001) {
        Write-Host "qualityGate.notice.$SampleName=real exported terrain token coverage is effectively zero; Java Standard-palette fallback must be treated as lower-confidence evidence"
    }
    if ($null -ne $threshold.MinTerrainTokenExportRatio -and $terrainTokenExportRatio -lt $threshold.MinTerrainTokenExportRatio) {
        $failures.Add(("terrainTokenExportCoverageRatio={0:N4} < {1:N4}" -f $terrainTokenExportRatio, $threshold.MinTerrainTokenExportRatio))
    }

    $waterColumns = [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.waterColumns"))
    $climateCoverageRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceClimateColumns") / $landColumns
    $vegetationCoverageRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceVegetationColumns") / $landColumns
    $treeCoverageRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceTreeColumns") / $landColumns
    $herbaceousCoverageRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceHerbaceousColumns") / $landColumns
    $shrubCoverageRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceShrubColumns") / $landColumns
    $snowLayerCoverageRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceSnowLayerColumns") / $landColumns
    $swampLayerCoverageRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceSwampLayerColumns") / $landColumns
    $slopeCoverageRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceSlopeColumns") / $landColumns
    $ecoregionCoverageRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceEcoregionColumns") / $landColumns
    $vegetationPresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceVegetationColumns") / $landColumns
    $treePresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceTreeColumns") / $landColumns
    $herbaceousPresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceHerbaceousColumns") / $landColumns
    $shrubPresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceShrubColumns") / $landColumns
    $snowLayerPresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceSnowLayerColumns") / $landColumns
    $swampLayerPresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceSwampLayerColumns") / $landColumns
    $steepSlopePresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceSteepSlopeColumns") / $landColumns
    $waterBathymetryCoverageRatio = (Get-ValueOrZero $Values "aggregate.waterDataEvidenceBathymetryColumns") / $waterColumns
    $waterOceanTempCoverageRatio = (Get-ValueOrZero $Values "aggregate.waterDataEvidenceOceanTemperatureColumns") / $waterColumns
    Write-Host ("climateCoverageRatio={0:N4}" -f $climateCoverageRatio)
    Write-Host ("vegetationCoverageRatio={0:N4}" -f $vegetationCoverageRatio)
    Write-Host ("treeCoverageRatio={0:N4}" -f $treeCoverageRatio)
    Write-Host ("herbaceousCoverageRatio={0:N4}" -f $herbaceousCoverageRatio)
    Write-Host ("shrubCoverageRatio={0:N4}" -f $shrubCoverageRatio)
    Write-Host ("snowLayerCoverageRatio={0:N4}" -f $snowLayerCoverageRatio)
    Write-Host ("swampLayerCoverageRatio={0:N4}" -f $swampLayerCoverageRatio)
    Write-Host ("slopeCoverageRatio={0:N4}" -f $slopeCoverageRatio)
    Write-Host ("ecoregionCoverageRatio={0:N4}" -f $ecoregionCoverageRatio)
    Write-Host ("vegetationPresenceRatio={0:N4}" -f $vegetationPresenceRatio)
    Write-Host ("treePresenceRatio={0:N4}" -f $treePresenceRatio)
    Write-Host ("herbaceousPresenceRatio={0:N4}" -f $herbaceousPresenceRatio)
    Write-Host ("shrubPresenceRatio={0:N4}" -f $shrubPresenceRatio)
    Write-Host ("snowLayerPresenceRatio={0:N4}" -f $snowLayerPresenceRatio)
    Write-Host ("swampLayerPresenceRatio={0:N4}" -f $swampLayerPresenceRatio)
    Write-Host ("steepSlopePresenceRatio={0:N4}" -f $steepSlopePresenceRatio)
    Write-Host ("waterBathymetryCoverageRatio={0:N4}" -f $waterBathymetryCoverageRatio)
    Write-Host ("waterOceanTemperatureCoverageRatio={0:N4}" -f $waterOceanTempCoverageRatio)

    if ($failures.Count -gt 0) {
        foreach ($failure in $failures) {
            Write-Host "qualityGate.failure.$SampleName=$failure"
        }
        throw "quality gate failed for sample=$SampleName"
    }
    Write-Host "qualityGate=pass"
}

function Assert-PhotoQualityGate {
    param(
        [string]$SampleName,
        [hashtable]$Values
    )
    $requiresCoastEvidence = @(
        "west-africa",
        "arabia-coast",
        "mediterranean-edge",
        "nile-delta",
        "med-italy-islands",
        "europe-northsea-denmark"
    ) -contains $SampleName
    $validColumns = [Math]::Max(1.0, [double](Get-ValueOrZero $Values "aggregate.validColumns"))
    $landColumns = [double](Get-ValueOrZero $Values "aggregate.landColumns")
    $waterColumnsRaw = [double](Get-ValueOrZero $Values "aggregate.waterColumns")
    $waterColumns = [Math]::Max(1.0, $waterColumnsRaw)
    if ($landColumns -le 0.0 -and $waterColumnsRaw -le 0.0) {
        throw "photo quality gate cannot run without land or water columns for sample=$SampleName"
    }

    $failures = New-Object System.Collections.Generic.List[string]
    $sourceColorCoverageRatio = (Get-ValueOrZero $Values "aggregate.sourceColorColumns") / $validColumns
    $landSourceColorCoverageRatio = if ($landColumns -gt 0.0) {
        (Get-ValueOrZero $Values "aggregate.landSourceColorColumns") / $landColumns
    } else {
        1.0
    }
    $sourceRenderErrorMean = (Get-ValueOrZero $Values "aggregate.sourceRenderErrorSum") /
            [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.sourceColorColumns"))
    $sourceRenderErrorMax = Get-ValueOrZero $Values "aggregate.sourceRenderErrorMax"
    $longHorizontalRuns = Get-ValueOrZero $Values "mosaicLongHorizontalBiomeBoundaryRuns"
    $longVerticalRuns = Get-ValueOrZero $Values "mosaicLongVerticalBiomeBoundaryRuns"
    $waterVolumeGapColumns = Get-ValueOrZero $Values "aggregate.waterVolumeGapColumns"
    $coastalLandColumns = Get-ValueOrZero $Values "aggregate.coastalLandColumns"
    $coastalSandHaloColumns = Get-ValueOrZero $Values "aggregate.coastalSandHaloColumns"
    $coastalSandHaloRatio = if ($coastalLandColumns -gt 0.0) {
        $coastalSandHaloColumns / $coastalLandColumns
    } else {
        0.0
    }
    $coastEdgeSamples = Get-ValueOrZero $Values "aggregate.coastEdgeSamples"
    $coastLandAboveSeaGt8Samples = Get-ValueOrZero $Values "aggregate.coastLandAboveSeaGt8Samples"
    $coastLandAboveSeaGt16Samples = Get-ValueOrZero $Values "aggregate.coastLandAboveSeaGt16Samples"
    $maxCoastFloorDelta = Get-ValueOrZero $Values "aggregate.maxCoastFloorDelta"
    $maxCoastLandAboveSeaDelta = Get-ValueOrZero $Values "aggregate.maxCoastLandAboveSeaDelta"
    $waterBathymetryCoverageRatio = (Get-ValueOrZero $Values "aggregate.waterDataEvidenceBathymetryColumns") / $waterColumns
    $waterOceanTempCoverageRatio = (Get-ValueOrZero $Values "aggregate.waterDataEvidenceOceanTemperatureColumns") / $waterColumns

    Write-Host ("photoQualityGate.sourceColorCoverageRatio={0:N4}" -f $sourceColorCoverageRatio)
    Write-Host ("photoQualityGate.landSourceColorCoverageRatio={0:N4}" -f $landSourceColorCoverageRatio)
    Write-Host ("photoQualityGate.sourceRenderErrorMean={0:N4}" -f $sourceRenderErrorMean)
    Write-Host ("photoQualityGate.sourceRenderErrorMax={0}" -f $sourceRenderErrorMax)
    Write-Host ("photoQualityGate.longBoundaryRuns=horizontal:{0},vertical:{1}" -f $longHorizontalRuns, $longVerticalRuns)
    Write-Host ("photoQualityGate.waterVolumeGapColumns={0}" -f $waterVolumeGapColumns)
    Write-Host ("photoQualityGate.coastalSandHaloRatio={0:N4},columns={1},coastalLandColumns={2}" -f `
            $coastalSandHaloRatio, $coastalSandHaloColumns, $coastalLandColumns)
    Write-Host ("photoQualityGate.coastEdgeSamples={0},gt8={1},gt16={2},maxLandDelta={3},maxFloorDelta={4}" -f `
            $coastEdgeSamples, $coastLandAboveSeaGt8Samples, $coastLandAboveSeaGt16Samples,
            $maxCoastLandAboveSeaDelta, $maxCoastFloorDelta)
    Write-Host ("photoQualityGate.waterBathymetryCoverageRatio={0:N4}" -f $waterBathymetryCoverageRatio)
    Write-Host ("photoQualityGate.waterOceanTemperatureCoverageRatio={0:N4}" -f $waterOceanTempCoverageRatio)

    if ($landColumns -gt 0.0 -and $landSourceColorCoverageRatio -lt 0.9850) {
        $failures.Add(("landSourceColorCoverageRatio={0:N4} < 0.9850" -f $landSourceColorCoverageRatio))
    }
    if ($sourceRenderErrorMean -gt 24.0) {
        $failures.Add(("sourceRenderErrorMean={0:N4} > 24.0000" -f $sourceRenderErrorMean))
    }
    if ($longHorizontalRuns -gt 8) {
        $failures.Add("mosaicLongHorizontalBiomeBoundaryRuns=$longHorizontalRuns > 8")
    }
    if ($longVerticalRuns -gt 4) {
        $failures.Add("mosaicLongVerticalBiomeBoundaryRuns=$longVerticalRuns > 4")
    }
    if ($waterVolumeGapColumns -ne 0.0) {
        $failures.Add("waterVolumeGapColumns=$waterVolumeGapColumns; water columns must not contain cave-like air gaps")
    }
    if ($requiresCoastEvidence -and $waterColumnsRaw -le 0.0) {
        $failures.Add("waterColumns=$waterColumnsRaw; coast sample must include water")
    }
    if ($requiresCoastEvidence -and $coastEdgeSamples -le 0.0) {
        $failures.Add("coastEdgeSamples=$coastEdgeSamples; coast sample must include a land/water edge")
    }
    if ($requiresCoastEvidence -and $coastalLandColumns -le 0.0) {
        $failures.Add("coastalLandColumns=$coastalLandColumns; coast sample must include coastal land")
    }
    if ($coastLandAboveSeaGt16Samples -ne 0.0) {
        $failures.Add("coastLandAboveSeaGt16Samples=$coastLandAboveSeaGt16Samples; coast cliffs above 16 blocks are not acceptable")
    }
    if ($coastLandAboveSeaGt8Samples -gt 4.0) {
        $failures.Add("coastLandAboveSeaGt8Samples=$coastLandAboveSeaGt8Samples > 4")
    }
    if ($maxCoastFloorDelta -gt 32.0) {
        $failures.Add("maxCoastFloorDelta=$maxCoastFloorDelta > 32")
    }
    if ($coastalSandHaloRatio -gt 0.1000) {
        $failures.Add(("coastalSandHaloRatio={0:N4} > 0.1000" -f $coastalSandHaloRatio))
    }
    if ($waterColumnsRaw -gt 0.0 -and $waterBathymetryCoverageRatio -lt 0.9500) {
        $failures.Add(("waterBathymetryCoverageRatio={0:N4} < 0.9500" -f $waterBathymetryCoverageRatio))
    }
    if ($waterColumnsRaw -gt 0.0 -and $waterOceanTempCoverageRatio -lt 0.9500) {
        $failures.Add(("waterOceanTemperatureCoverageRatio={0:N4} < 0.9500" -f $waterOceanTempCoverageRatio))
    }
    if ($sourceColorCoverageRatio -lt 0.9900) {
        $waterDataCanExplainMissingSourceColor = $landSourceColorCoverageRatio -ge 0.9850 `
                -and ($waterColumnsRaw -le 0.0 `
                    -or ($waterBathymetryCoverageRatio -ge 0.9500 -and $waterOceanTempCoverageRatio -ge 0.9500)) `
                -and $sourceColorCoverageRatio -ge 0.9500
        if ($waterDataCanExplainMissingSourceColor) {
            Write-Host ("photoQualityGate.notice.$SampleName=sourceColorCoverageRatio={0:N4} < 0.9900, but land source coverage and water data coverage pass; treating water/no-data source gaps as non-blocking" -f `
                    $sourceColorCoverageRatio)
        } else {
            $failures.Add(("sourceColorCoverageRatio={0:N4} < 0.9900" -f $sourceColorCoverageRatio))
        }
    }

    if ($failures.Count -gt 0) {
        foreach ($failure in $failures) {
            Write-Host "photoQualityGate.failure.$SampleName=$failure"
        }
        throw "photo quality gate failed for sample=$SampleName"
    }
    Write-Host "photoQualityGate=pass"
}

function Assert-BaselineRegression {
    param(
        [string]$SampleName,
        [hashtable]$Values,
        [string]$BaselineRoot
    )
    if ([string]::IsNullOrWhiteSpace($BaselineRoot)) {
        return
    }
    $baselineSummary = Join-Path (Join-Path $BaselineRoot $SampleName) "debug\stats\summary.txt"
    if (!(Test-Path -LiteralPath $baselineSummary)) {
        throw "baseline summary missing for sample=$SampleName path=$baselineSummary"
    }
    $baseline = Get-SummaryValues -SummaryPath $baselineSummary
    Assert-RequiredSummaryValues -SampleName "$SampleName baseline" -Values $baseline `
        -ExpectedStartX ([int]$Values["startRegionX"]) `
        -ExpectedStartZ ([int]$Values["startRegionZ"]) `
        -ExpectedCols ([int]$Values["cols"]) `
        -ExpectedRows ([int]$Values["rows"])

    $failures = New-Object System.Collections.Generic.List[string]
    $landColumns = [Math]::Max(1.0, [double]$Values["aggregate.landColumns"])
    $baselineLandColumns = [Math]::Max(1.0, [double]$baseline["aggregate.landColumns"])

    $candidateSmallShare = (Get-ValueOrZero $Values "mosaicSmallLandComponents") /
            [Math]::Max(1.0, (Get-ValueOrZero $Values "mosaicLandComponents"))
    $baselineSmallShare = (Get-ValueOrZero $baseline "mosaicSmallLandComponents") /
            [Math]::Max(1.0, (Get-ValueOrZero $baseline "mosaicLandComponents"))
    if ($candidateSmallShare -gt ($baselineSmallShare + 0.020)) {
        $failures.Add(("smallLandComponentShare regressed {0:N4} -> {1:N4}" -f $baselineSmallShare, $candidateSmallShare))
    }

    $candidateMismatch = (Get-ValueOrZero $Values "aggregate.biomeTopMismatchColumns") / $landColumns
    $baselineMismatch = (Get-ValueOrZero $baseline "aggregate.biomeTopMismatchColumns") / $baselineLandColumns
    if ($candidateMismatch -gt ($baselineMismatch + 0.010)) {
        $failures.Add(("biomeTopMismatchRatio regressed {0:N4} -> {1:N4}" -f $baselineMismatch, $candidateMismatch))
    }

    $candidateVegetatedMismatch = (Get-ValueOrZero $Values "aggregate.vegetatedBiomeNonVegetationTopColumns") / $landColumns
    $baselineVegetatedMismatch = (Get-ValueOrZero $baseline "aggregate.vegetatedBiomeNonVegetationTopColumns") / $baselineLandColumns
    if ($candidateVegetatedMismatch -gt ($baselineVegetatedMismatch + 0.006)) {
        $failures.Add(("vegetatedBiomeNonVegetationTopRatio regressed {0:N4} -> {1:N4}" -f $baselineVegetatedMismatch, $candidateVegetatedMismatch))
    }

    $candidateSmoother = (Get-DecisionSourcePrefixCount $Values "smoother") / $landColumns
    $baselineSmoother = (Get-DecisionSourcePrefixCount $baseline "smoother") / $baselineLandColumns
    if ($candidateSmoother -gt ($baselineSmoother + 0.030)) {
        $failures.Add(("smootherDecisionRatio regressed {0:N4} -> {1:N4}" -f $baselineSmoother, $candidateSmoother))
    }

    $candidateLongH = Get-ValueOrZero $Values "mosaicLongHorizontalBiomeBoundaryRuns"
    $baselineLongH = Get-ValueOrZero $baseline "mosaicLongHorizontalBiomeBoundaryRuns"
    if ($candidateLongH -gt ($baselineLongH + 1)) {
        $failures.Add("mosaicLongHorizontalBiomeBoundaryRuns regressed $baselineLongH -> $candidateLongH")
    }
    $candidateLongV = Get-ValueOrZero $Values "mosaicLongVerticalBiomeBoundaryRuns"
    $baselineLongV = Get-ValueOrZero $baseline "mosaicLongVerticalBiomeBoundaryRuns"
    if ($candidateLongV -gt ($baselineLongV + 1)) {
        $failures.Add("mosaicLongVerticalBiomeBoundaryRuns regressed $baselineLongV -> $candidateLongV")
    }

    if ($failures.Count -gt 0) {
        foreach ($failure in $failures) {
            Write-Host "baselineRegression.failure.$SampleName=$failure"
        }
        throw "baseline regression gate failed for sample=$SampleName"
    }
    Write-Host "baselineRegression=pass"
}

$seenWindows = @{}
$sampleFailures = New-Object System.Collections.Generic.List[object]
$passedSamples = 0
$gateStartedAt = Get-Date
$selectedSampleNames = @($sampleDefinitions | ForEach-Object { $_.Name })

Write-Host ("qualityAcceptance.start=samples={0},scale=1:{1},threads={2},format={3},textureMode={4},verticalScale={5},cacheRows={6},prefetchRows={7},photoGate={8},ecologyGate={9},skipGeneration={10},cleanSampleOutput={11},photoParityEvidenceOnly={12},outputRoot={13}" -f `
        $selectedSampleNames.Count, $Scale, $Threads, $Format, $TextureMode, $VerticalScale, $CacheRows,
        $PrefetchRows, (-not [bool]$NoQualityGate -and -not [bool]$UseEcologyQualityGate),
        [bool]$UseEcologyQualityGate, [bool]$SkipGeneration, [bool]$CleanSampleOutput,
        [bool]$PhotoParityEvidenceOnly, $OutputRoot)
Write-Host "qualityAcceptance.command=$(Get-QualityAcceptanceReplayCommand -SelectedSamples $selectedSampleNames)"

function Assert-RequiredArtifact {
    param(
        [string]$Path,
        [string]$Label,
        [Nullable[datetime]]$NotOlderThan = $null
    )
    if (!(Test-Path -LiteralPath $Path)) {
        throw "$Label missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path
    if ($item.Length -le 0) {
        throw "$Label is empty: $Path"
    }
    if ($null -ne $NotOlderThan -and $item.LastWriteTime -lt $NotOlderThan.Value) {
        throw ("{0} is stale: {1}; lastWrite={2:o}; requiredAfter={3:o}" -f `
                $Label, $Path, $item.LastWriteTime, $NotOlderThan.Value)
    }
}

function Write-EvidenceManifest {
    param(
        [object]$Sample,
        [string]$World,
        [string]$PreviewImage,
        [string]$PreviewDebug,
        [string]$SummaryPath,
        [hashtable]$Values,
        [int]$StartX,
        [int]$StartZ,
        [int]$ScaleValue,
        [string]$GenerationLogPath,
        [object]$EvidenceFreshAfter = $null
    )
    $landColumns = [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.landColumns"))
    $gitInside = $false
    $gitHead = ""
    $gitDirty = "unknown"
    try {
        $inside = & git -C $repoRoot rev-parse --is-inside-work-tree 2>$null
        if ($LASTEXITCODE -eq 0 -and $inside -eq "true") {
            $gitInside = $true
            $gitHead = (& git -C $repoRoot rev-parse HEAD 2>$null)
            $dirtyOutput = (& git -C $repoRoot status --porcelain 2>$null)
            $gitDirty = if ($dirtyOutput) { "dirty" } else { "clean" }
        }
    } catch {
        $gitInside = $false
    }
    $expectedExportTile = ""
    if ($Sample.PSObject.Properties.Name -contains "ExpectedExportTile" -and $null -ne $Sample.ExpectedExportTile) {
        $expectedExportTile = [string]$Sample.ExpectedExportTile
    }
    $manifest = [ordered]@{
        schemaVersion = 1
        generatedAt = (Get-Date).ToString("o")
        sample = $Sample.Name
        longitude = [double]$Sample.Longitude
        latitude = [double]$Sample.Latitude
        expectedExportTile = $expectedExportTile
        startRegionX = $StartX
        startRegionZ = $StartZ
        cols = [int]$Sample.Cols
        rows = [int]$Sample.Rows
        freshGenerationThisInvocation = -not $SkipGeneration
        cleanSampleOutput = [bool]$CleanSampleOutput
        noQualityGate = [bool]$NoQualityGate
        useEcologyQualityGate = [bool]$UseEcologyQualityGate
        commandLine = $MyInvocation.Line
        repoRoot = $repoRoot
        gitInsideWorkTree = $gitInside
        gitHead = $gitHead
        gitDirty = $gitDirty
        heightmap = $Heightmap
        scale = $ScaleValue
        threads = $Threads
        format = $Format
        textureMode = $TextureMode
        verticalScale = $VerticalScale
        cacheRows = $CacheRows
        prefetchRows = $PrefetchRows
        outputRoot = $OutputRoot
        world = $World
        previewImage = $PreviewImage
        previewDebug = $PreviewDebug
        summaryPath = $SummaryPath
        generationLogPath = $GenerationLogPath
        evidenceFreshAfter = if ($EvidenceFreshAfter -is [datetime]) { $EvidenceFreshAfter.ToString("o") } else { "" }
        summarySchemaVersion = [int]$Values["summarySchemaVersion"]
        sourceColorCoverageRatio = (Get-ValueOrZero $Values "aggregate.sourceColorColumns") /
                [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.validColumns"))
        landSourceColorCoverageRatio = (Get-ValueOrZero $Values "aggregate.landSourceColorColumns") / $landColumns
        sourceRenderErrorMean = (Get-ValueOrZero $Values "aggregate.sourceRenderErrorSum") /
                [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.sourceColorColumns"))
        sourceRenderErrorMax = Get-ValueOrZero $Values "aggregate.sourceRenderErrorMax"
        waterVolumeGapColumns = Get-ValueOrZero $Values "aggregate.waterVolumeGapColumns"
        coastalSandHaloRatio = (Get-ValueOrZero $Values "aggregate.coastalSandHaloColumns") /
                [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.coastalLandColumns"))
        coastLandAboveSeaGt8Samples = Get-ValueOrZero $Values "aggregate.coastLandAboveSeaGt8Samples"
        coastLandAboveSeaGt16Samples = Get-ValueOrZero $Values "aggregate.coastLandAboveSeaGt16Samples"
        maxCoastFloorDelta = Get-ValueOrZero $Values "aggregate.maxCoastFloorDelta"
        maxCoastLandAboveSeaDelta = Get-ValueOrZero $Values "aggregate.maxCoastLandAboveSeaDelta"
        landTerrainTokenExportRatio = (Get-ValueOrZero $Values "aggregate.landTerrainTokenExportColumns") / $landColumns
        landTerrainTokenJavaStandardRatio = (Get-ValueOrZero $Values "aggregate.landTerrainTokenJavaStandardColumns") / $landColumns
        dataEvidence = [ordered]@{
            climateRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceClimateColumns") / $landColumns
            vegetationRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceVegetationColumns") / $landColumns
            treeRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceTreeColumns") / $landColumns
            herbaceousRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceHerbaceousColumns") / $landColumns
            shrubRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceShrubColumns") / $landColumns
            snowLayerRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceSnowLayerColumns") / $landColumns
            swampLayerRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceSwampLayerColumns") / $landColumns
            slopeRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceSlopeColumns") / $landColumns
            ecoregionRatio = (Get-ValueOrZero $Values "aggregate.landDataEvidenceEcoregionColumns") / $landColumns
            vegetationPresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceVegetationColumns") / $landColumns
            treePresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceTreeColumns") / $landColumns
            herbaceousPresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceHerbaceousColumns") / $landColumns
            shrubPresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceShrubColumns") / $landColumns
            snowLayerPresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceSnowLayerColumns") / $landColumns
            swampLayerPresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceSwampLayerColumns") / $landColumns
            steepSlopePresenceRatio = (Get-ValueOrZero $Values "aggregate.landDataPresenceSteepSlopeColumns") / $landColumns
            waterBathymetryRatio = (Get-ValueOrZero $Values "aggregate.waterDataEvidenceBathymetryColumns") /
                    [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.waterColumns"))
            waterOceanTemperatureRatio = (Get-ValueOrZero $Values "aggregate.waterDataEvidenceOceanTemperatureColumns") /
                    [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.waterColumns"))
        }
        postFinalSmootherDecisionRatio = (Get-DecisionSourcePrefixCount $Values "smoother") / $landColumns
        intentStabilizedDecisionRatio = (Get-DecisionSourcePrefixCount $Values "intent-stabilized") / $landColumns
        smallLandComponentPixelShare = (Get-ValueOrZero $Values "mosaicSmallLandComponentPixels") / $landColumns
        dryRockAccentTopRatio = ((Get-ValueOrZero $Values "aggregate.redSandTopColumns") +
                (Get-ValueOrZero $Values "aggregate.terracottaTopColumns") +
                (Get-ValueOrZero $Values "aggregate.orangeTerracottaTopColumns") +
                (Get-ValueOrZero $Values "aggregate.brownTerracottaTopColumns")) / $landColumns
        coarseDirtTopRatio = (Get-ValueOrZero $Values "aggregate.coarseDirtTopColumns") / $landColumns
        mossTopRatio = (Get-ValueOrZero $Values "aggregate.mossTopColumns") / $landColumns
        podzolTopRatio = (Get-ValueOrZero $Values "aggregate.podzolTopColumns") / $landColumns
        lushTopRatio = ((Get-ValueOrZero $Values "aggregate.mossTopColumns") +
                (Get-ValueOrZero $Values "aggregate.podzolTopColumns")) / $landColumns
        snowTopRatio = (Get-ValueOrZero $Values "aggregate.snowTopColumns") / $landColumns
        stoneGravelTopRatio = ((Get-ValueOrZero $Values "aggregate.stoneTopColumns") +
                (Get-ValueOrZero $Values "aggregate.gravelTopColumns")) / $landColumns
        forestTaigaBiomeFamilyRatio = ((Get-ValueOrZero $Values "aggregate.biomeFamilyForestColumns") +
                (Get-ValueOrZero $Values "aggregate.biomeFamilyTaigaColumns")) / $landColumns
        savannaBiomeFamilyRatio = (Get-ValueOrZero $Values "aggregate.biomeFamilySavannaColumns") / $landColumns
        jungleBiomeFamilyRatio = (Get-ValueOrZero $Values "aggregate.biomeFamilyJungleColumns") / $landColumns
        savannaGrasslandBiomeFamilyRatio = ((Get-ValueOrZero $Values "aggregate.biomeFamilySavannaColumns") +
                (Get-ValueOrZero $Values "aggregate.biomeFamilyGrasslandColumns")) / $landColumns
        jungleForestBiomeFamilyRatio = ((Get-ValueOrZero $Values "aggregate.biomeFamilyJungleColumns") +
                (Get-ValueOrZero $Values "aggregate.biomeFamilyForestColumns")) / $landColumns
        jungleForestSwampBiomeFamilyRatio = ((Get-ValueOrZero $Values "aggregate.biomeFamilyJungleColumns") +
                (Get-ValueOrZero $Values "aggregate.biomeFamilyForestColumns") +
                (Get-ValueOrZero $Values "aggregate.biomeFamilySwampColumns")) / $landColumns
        biomes = [ordered]@{
            desertRatio = (Get-ValueOrZero $Values "aggregate.biome.desert") / $landColumns
            savannaRatio = (Get-ValueOrZero $Values "aggregate.biome.savanna") / $landColumns
            savannaPlateauRatio = (Get-ValueOrZero $Values "aggregate.biome.savanna_plateau") / $landColumns
            windsweptSavannaRatio = (Get-ValueOrZero $Values "aggregate.biome.windswept_savanna") / $landColumns
            jungleRatio = (Get-ValueOrZero $Values "aggregate.biome.jungle") / $landColumns
            sparseJungleRatio = (Get-ValueOrZero $Values "aggregate.biome.sparse_jungle") / $landColumns
            bambooJungleRatio = (Get-ValueOrZero $Values "aggregate.biome.bamboo_jungle") / $landColumns
            forestRatio = (Get-ValueOrZero $Values "aggregate.biome.forest") / $landColumns
            darkForestRatio = (Get-ValueOrZero $Values "aggregate.biome.dark_forest") / $landColumns
            plainsRatio = (Get-ValueOrZero $Values "aggregate.biome.plains") / $landColumns
            taigaRatio = (Get-ValueOrZero $Values "aggregate.biome.taiga") / $landColumns
            snowyTaigaRatio = (Get-ValueOrZero $Values "aggregate.biome.snowy_taiga") / $landColumns
            snowyPlainsRatio = (Get-ValueOrZero $Values "aggregate.biome.snowy_plains") / $landColumns
        }
        familyTop = [ordered]@{
            desertSandRatio = (Get-ValueOrZero $Values "aggregate.familyTop.desert.sand") / $landColumns
            desertGrassRatio = (Get-ValueOrZero $Values "aggregate.familyTop.desert.grass_block") / $landColumns
            savannaGrassRatio = (Get-ValueOrZero $Values "aggregate.familyTop.savanna.grass_block") / $landColumns
            savannaCoarseDirtRatio = (Get-ValueOrZero $Values "aggregate.familyTop.savanna.coarse_dirt") / $landColumns
            savannaDryTextureFamilyRatio = (Get-ValueOrZero $Values "aggregate.familyTop.savanna.coarse_dirt") /
                    [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.biomeFamilySavannaColumns"))
            savannaSandRatio = (Get-ValueOrZero $Values "aggregate.familyTop.savanna.sand") / $landColumns
            jungleGrassRatio = (Get-ValueOrZero $Values "aggregate.familyTop.jungle.grass_block") / $landColumns
            jungleMossRatio = (Get-ValueOrZero $Values "aggregate.familyTop.jungle.moss_block") / $landColumns
            junglePodzolRatio = (Get-ValueOrZero $Values "aggregate.familyTop.jungle.podzol") / $landColumns
            jungleLushSurfaceFamilyRatio = ((Get-ValueOrZero $Values "aggregate.familyTop.jungle.moss_block") +
                    (Get-ValueOrZero $Values "aggregate.familyTop.jungle.podzol")) /
                    [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.biomeFamilyJungleColumns"))
            forestGrassRatio = (Get-ValueOrZero $Values "aggregate.familyTop.forest.grass_block") / $landColumns
            forestMossRatio = (Get-ValueOrZero $Values "aggregate.familyTop.forest.moss_block") / $landColumns
            forestPodzolRatio = (Get-ValueOrZero $Values "aggregate.familyTop.forest.podzol") / $landColumns
            forestLushSurfaceFamilyRatio = ((Get-ValueOrZero $Values "aggregate.familyTop.forest.moss_block") +
                    (Get-ValueOrZero $Values "aggregate.familyTop.forest.podzol")) /
                    [Math]::Max(1.0, (Get-ValueOrZero $Values "aggregate.biomeFamilyForestColumns"))
            forestSnowRatio = (Get-ValueOrZero $Values "aggregate.familyTop.forest.snow_block") / $landColumns
            forestStoneRatio = (Get-ValueOrZero $Values "aggregate.familyTop.forest.stone") / $landColumns
            lushSurfaceFamilyRatio = ((Get-ValueOrZero $Values "aggregate.familyTop.jungle.moss_block") +
                    (Get-ValueOrZero $Values "aggregate.familyTop.jungle.podzol") +
                    (Get-ValueOrZero $Values "aggregate.familyTop.forest.moss_block") +
                    (Get-ValueOrZero $Values "aggregate.familyTop.forest.podzol") +
                    (Get-ValueOrZero $Values "aggregate.familyTop.taiga.moss_block") +
                    (Get-ValueOrZero $Values "aggregate.familyTop.taiga.podzol")) /
                    [Math]::Max(1.0, ((Get-ValueOrZero $Values "aggregate.biomeFamilyJungleColumns") +
                        (Get-ValueOrZero $Values "aggregate.biomeFamilyForestColumns") +
                        (Get-ValueOrZero $Values "aggregate.biomeFamilyTaigaColumns")))
            snowSnowRatio = (Get-ValueOrZero $Values "aggregate.familyTop.snow.snow_block") / $landColumns
            snowGrassRatio = (Get-ValueOrZero $Values "aggregate.familyTop.snow.grass_block") / $landColumns
        }
        decisionSourceTop = [ordered]@{
            intentMossRatio = (Get-ValueOrZero $Values "aggregate.decisionSourceTop.intent.moss_block") / $landColumns
            intentPodzolRatio = (Get-ValueOrZero $Values "aggregate.decisionSourceTop.intent.podzol") / $landColumns
            intentGrassRatio = (Get-ValueOrZero $Values "aggregate.decisionSourceTop.intent.grass_block") / $landColumns
            intentEcoregionSandRatio = (Get-ValueOrZero $Values "aggregate.decisionSourceTop.intent-ecoregion.sand") / $landColumns
            materialRuleSandRatio = (Get-ValueOrZero $Values "aggregate.decisionSourceTop.material-rule.sand") / $landColumns
            materialRuleStoneRatio = (Get-ValueOrZero $Values "aggregate.decisionSourceTop.material-rule.stone") / $landColumns
            snowSnowRatio = (Get-ValueOrZero $Values "aggregate.decisionSourceTop.snow.snow_block") / $landColumns
            intentStabilizedComponentGrassRatio = (Get-ValueOrZero $Values "aggregate.decisionSourceTop.intent-stabilized-component.grass_block") / $landColumns
            smootherIsolatedGrassRatio = (Get-ValueOrZero $Values "aggregate.decisionSourceTop.smoother-isolated.grass_block") / $landColumns
        }
    }
    $manifestPath = Join-Path $World "quality-evidence.json"
    ($manifest | ConvertTo-Json -Depth 4) | Set-Content -LiteralPath $manifestPath -Encoding UTF8
    Write-Host "qualityEvidence=$manifestPath"
}

for ($sampleIndex = 0; $sampleIndex -lt $sampleDefinitions.Count; $sampleIndex++) {
    $sample = $sampleDefinitions[$sampleIndex]
    $sampleNumber = $sampleIndex + 1
    $sampleStartedAt = Get-Date
    $sampleScale = if ($sample.PSObject.Properties.Name -contains "Scale" -and $null -ne $sample.Scale) {
        [int]$sample.Scale
    } else {
        $Scale
    }
    $startX = $null
    $startZ = $null
    $world = Join-Path $OutputRoot $sample.Name
    $previewTiles = Join-Path $world "preview-tiles"
    $previewImage = Join-Path $world "preview.png"
    $previewDebug = Join-Path $world "debug"
    $summaryPath = Join-Path $previewDebug "stats\summary.txt"
    $progressPath = Join-Path $world "earthmap-region-progress.csv"
    $previewViewer = Join-Path $previewTiles "index.html"
    $sampleLogDir = Join-Path $world "logs"
    $locateLog = Join-Path $sampleLogDir "locate-heightmap-point.log"
    $generationLog = Join-Path $sampleLogDir "generation.log"
    $freshAfter = if ($SkipGeneration) { $null } else { $sampleStartedAt.AddSeconds(-2) }

    try {
        Write-Host ("qualityAcceptance.sample.locateStart=[{0}/{1}],sample={2},lon={3},lat={4},scale=1:{5}" -f `
                $sampleNumber, $sampleDefinitions.Count, $sample.Name, $sample.Longitude, $sample.Latitude, $sampleScale)
        $located = Get-RegionForPoint -Longitude $sample.Longitude -Latitude $sample.Latitude -ScaleValue $sampleScale -LogPath $locateLog
        $useExplicitStart = ($null -ne $sample.StartRegionX) -and ($null -ne $sample.StartRegionZ)
        if ($useExplicitStart) {
            $hasStartRegionScale = $sample.PSObject.Properties.Name -contains "StartRegionScale"
            if ($hasStartRegionScale -and ($null -ne $sample.StartRegionScale)) {
                $useExplicitStart = [int]$sample.StartRegionScale -eq $sampleScale
            }
        }
        $startX = if ($useExplicitStart) {
            [int]$sample.StartRegionX
        } else {
            $located.RegionX - [int][Math]::Floor($sample.Cols / 2.0)
        }
        $startZ = if ($useExplicitStart) {
            [int]$sample.StartRegionZ
        } else {
            $located.RegionZ - [int][Math]::Floor($sample.Rows / 2.0)
        }
        $windowKey = "$sampleScale,$startX,$startZ,$($sample.Cols),$($sample.Rows)"
        if ($seenWindows.ContainsKey($windowKey)) {
            throw "sample window duplicate: $($sample.Name) and $($seenWindows[$windowKey]) both target $windowKey"
        }
        $seenWindows[$windowKey] = $sample.Name

        Write-Host ("qualityAcceptance.sample.start=[{0}/{1}],sample={2},scale=1:{3},startRegionX={4},startRegionZ={5},cols={6},rows={7},output={8}" -f `
                $sampleNumber, $sampleDefinitions.Count, $sample.Name, $sampleScale, $startX, $startZ, $sample.Cols, $sample.Rows, $world)

        if (!$SkipGeneration) {
            if ($CleanSampleOutput -and (Test-Path -LiteralPath $world)) {
                Write-Host "qualityAcceptance.sample.clean=$world"
                Remove-Item -LiteralPath $world -Recurse -Force
            }
            Write-Host ("qualityAcceptance.sample.generateStart=sample={0},regions={1},{2}+{3}x{4},previewImage={5},debugDir={6}" -f `
                    $sample.Name, $startX, $startZ, $sample.Cols, $sample.Rows, $previewImage, $previewDebug)
            Write-Host ("qualityAcceptance.sample.liveArtifacts=sample={0},progressFile={1},previewViewer={2},previewTiles={3},summary={4}" -f `
                    $sample.Name, $progressPath, $previewViewer, $previewTiles, $summaryPath)
            Write-Host ("qualityAcceptance.sample.watchCommand=.\scripts\watch-survival-progress.ps1 `"{0}`" -TotalRegions {1} -Watch" -f `
                    $world, ($sample.Cols * $sample.Rows))
            Write-Host "qualityAcceptance.sample.generationLog=$generationLog"
            $generationResult = Invoke-LoggedCommand -LogPath $generationLog -SampleName $sample.Name -World $world `
                -ProgressPath $progressPath -PreviewImage $previewImage -PreviewViewer $previewViewer -SummaryPath $summaryPath -Command {
                & $run generate-vanilla-delegated-regions-parallel `
                    $Heightmap `
                    $world `
                    $sampleScale `
                    $startX `
                    $startZ `
                    $sample.Cols `
                    $sample.Rows `
                    $Format `
                    $Threads `
                    "surfaceRaster=auto" `
                    "textureMode=$TextureMode" `
                    "verticalScale=$VerticalScale" `
                    "cacheRows=$CacheRows" `
                    "prefetchRows=$PrefetchRows" `
                    "previewTiles=$previewTiles" `
                    "previewImage=$previewImage" `
                    "previewDebugDir=$previewDebug"
            }
            if ($generationResult.ExitCode -ne 0) {
                Write-Host "qualityAcceptance.sample.failureHint.$($sample.Name)=$($generationResult.FailureHint)"
                throw ("generation failed for sample={0}; exitCode={1}; hint={2}; log={3}" -f `
                        $sample.Name, $generationResult.ExitCode, $generationResult.FailureHint, $generationResult.LogPath)
            }
            Write-Host ("qualityAcceptance.sample.generateEnd=pass,sample={0},elapsed={1}" -f `
                    $sample.Name, (Format-Elapsed ((Get-Date) - $sampleStartedAt)))
        } else {
            Write-Host "qualityAcceptance.sample.generate=skipped,sample=$($sample.Name),reason=SkipGeneration"
        }

        Assert-RequiredArtifact -Path $previewImage -Label "preview image" -NotOlderThan $freshAfter
        Assert-RequiredArtifact -Path (Join-Path $previewDebug "decision-source.png") -Label "decision-source debug mosaic" -NotOlderThan $freshAfter
        Assert-RequiredArtifact -Path (Join-Path $previewDebug "surface.png") -Label "surface debug mosaic" -NotOlderThan $freshAfter
        Assert-RequiredArtifact -Path (Join-Path $previewDebug "biome.png") -Label "biome debug mosaic" -NotOlderThan $freshAfter
        Assert-RequiredArtifact -Path $summaryPath -Label "debug summary" -NotOlderThan $freshAfter
        Write-Host ("qualityAcceptance.sample.artifacts=pass,sample={0},preview={1},debugDir={2},summary={3}" -f `
                $sample.Name, $previewImage, $previewDebug, $summaryPath)
        $summaryValues = Get-SummaryValues -SummaryPath $summaryPath
        Assert-RequiredSummaryValues -SampleName $sample.Name -Values $summaryValues `
            -ExpectedStartX $startX -ExpectedStartZ $startZ -ExpectedCols $sample.Cols -ExpectedRows $sample.Rows
        Write-EvidenceManifest -Sample $sample -World $world -PreviewImage $previewImage -PreviewDebug $previewDebug `
            -SummaryPath $summaryPath -Values $summaryValues -StartX $startX -StartZ $startZ -ScaleValue $sampleScale `
            -GenerationLogPath $generationLog -EvidenceFreshAfter $freshAfter
        Show-Summary -Values $summaryValues
        if ($PhotoParityEvidenceOnly) {
            Invoke-SamplePhotoParityGate -SampleName $sample.Name -World $world -PreviewDebug $previewDebug `
                -StartX $startX -StartZ $startZ -Cols $sample.Cols -Rows $sample.Rows -NotOlderThan $freshAfter `
                -NoThresholdGate
        }
        if (!$NoQualityGate) {
            if ($SkipGeneration) {
                Write-Host "qualityGate.notice.$($sample.Name)=SkipGeneration was used; this is a proxy re-check of existing fresh artifacts, not final release evidence"
            }
            if ($UseEcologyQualityGate) {
                Assert-QualityGate -SampleName $sample.Name -Values $summaryValues
            } else {
                Assert-PhotoQualityGate -SampleName $sample.Name -Values $summaryValues
                Invoke-SamplePhotoParityGate -SampleName $sample.Name -World $world -PreviewDebug $previewDebug `
                    -StartX $startX -StartZ $startZ -Cols $sample.Cols -Rows $sample.Rows -NotOlderThan $freshAfter
            }
            Assert-BaselineRegression -SampleName $sample.Name -Values $summaryValues -BaselineRoot $BaselineRoot
        }

        $passedSamples++
        Write-Host ("qualityAcceptance.sample.end=pass,index={0}/{1},sample={2},elapsed={3},output={4},preview={5},summary={6}" -f `
                $sampleNumber, $sampleDefinitions.Count, $sample.Name, (Format-Elapsed ((Get-Date) - $sampleStartedAt)), $world, $previewImage, $summaryPath)
    } catch {
        $failure = [pscustomobject]@{
            Sample = $sample.Name
            Scale = $sampleScale
            StartRegionX = $startX
            StartRegionZ = $startZ
            Output = $world
            Message = $_.Exception.Message
            Elapsed = Format-Elapsed ((Get-Date) - $sampleStartedAt)
        }
        [void]$sampleFailures.Add($failure)
        Write-Host ("qualityAcceptance.sample.end=fail,index={0}/{1},sample={2},elapsed={3},output={4}" -f `
                $sampleNumber, $sampleDefinitions.Count, $sample.Name, $failure.Elapsed, $world)
        Write-Host "qualityAcceptance.sample.failure.$($sample.Name)=$($failure.Message)"
        Write-Host ("qualityAcceptance.sample.failureArtifacts.{0}=output={1},progressFile={2},generationLog={3},preview={4},viewer={5},summary={6}" -f `
                $sample.Name, $world, $progressPath, $generationLog, $previewImage, $previewViewer, $summaryPath)
        if ($_.InvocationInfo -and -not [string]::IsNullOrWhiteSpace($_.InvocationInfo.PositionMessage)) {
            Write-Host ("qualityAcceptance.sample.failurePosition.{0}={1}" -f `
                    $sample.Name, (Format-LogMessage $_.InvocationInfo.PositionMessage))
        }
        if (-not [string]::IsNullOrWhiteSpace($_.ScriptStackTrace)) {
            Write-Host ("qualityAcceptance.sample.failureStack.{0}={1}" -f `
                    $sample.Name, (Format-LogMessage $_.ScriptStackTrace))
        }
        Write-Host "qualityAcceptance.nextCommand.failedSample=$(Get-QualityAcceptanceReplayCommand -SelectedSamples @($sample.Name))"
        break
    }
}

$gateElapsed = Format-Elapsed ((Get-Date) - $gateStartedAt)
if ($sampleFailures.Count -gt 0) {
    Write-Host ("qualityAcceptance.summary=fail,passed={0},failed={1},total={2},elapsed={3},outputRoot={4}" -f `
            $passedSamples, $sampleFailures.Count, $sampleDefinitions.Count, $gateElapsed, $OutputRoot)
    foreach ($failure in $sampleFailures) {
        Write-Host ("qualityAcceptance.failure=sample={0},scale=1:{1},startRegionX={2},startRegionZ={3},elapsed={4},output={5},message={6}" -f `
                $failure.Sample, $failure.Scale, $failure.StartRegionX, $failure.StartRegionZ, $failure.Elapsed, $failure.Output, $failure.Message)
    }
    Write-Host "qualityAcceptance.nextCommand=$(Get-QualityAcceptanceReplayCommand -SelectedSamples $selectedSampleNames)"
    throw "quality acceptance sample gate failed; failures=$($sampleFailures.Count)"
}

if ($SkipGeneration) {
    Write-Host ("qualityAcceptance.summary=fail,passed={0},failed=1,total={1},elapsed={2},outputRoot={3},reason=SkipGeneration" -f `
            $passedSamples, $sampleDefinitions.Count, $gateElapsed, $OutputRoot)
    Write-Host "qualityAcceptance.failure=gate=SkipGeneration,message=proxy artifact checks passed but -SkipGeneration cannot produce release evidence"
    Write-Host "qualityAcceptance.nextCommand=$(Get-QualityAcceptanceReplayCommand -SelectedSamples $selectedSampleNames)"
    throw "quality acceptance sample gate is invalid release evidence because -SkipGeneration was used; rerun without -SkipGeneration for fresh evidence"
}

Write-Host ("qualityAcceptance.summary=pass,passed={0},failed=0,total={1},elapsed={2},outputRoot={3}" -f `
        $passedSamples, $sampleDefinitions.Count, $gateElapsed, $OutputRoot)
Write-Host "qualityAcceptance.nextCommand=$(Get-QualityAcceptanceReplayCommand -SelectedSamples $selectedSampleNames)"
