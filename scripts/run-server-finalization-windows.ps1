[CmdletBinding()]
param(
    [string]$WorldDir = 'D:\worldgen\world',
    [ValidateSet('auto', 'mca', 'linear')]
    [string]$RegionFormat = 'auto',
    [int]$StartRegionX = 0,
    [int]$StartRegionZ = -1,
    [ValidateRange(1, 2048)]
    [int]$RegionCols = 1,
    [ValidateRange(1, 2048)]
    [int]$RegionRows = 1,
    [ValidateRange(1, 16)]
    [int]$WindowChunks = 16,
    [int]$WaitSeconds = 20,
    [int]$MaxWindows = 0,
    [double]$MinFullRatioAfter = 0.0,
    [int]$MinFullChunkDelta = 0,
    [int]$MaxNonFullChunksAfter = -1,
    [switch]$RunPostFinalIntegrity,
    [int]$MaxUnderwaterAirColumns = -1,
    [double]$MaxUnderwaterAirColumnRatio = -1.0,
    [int]$MinTreeLeafColumns = -1,
    [double]$MinTreeLeafColumnRatio = -1.0,
    [int]$MaxCoastLandAboveSeaGt8Samples = -1,
    [int]$MaxCoastLandAboveSeaGt16Samples = -1,
    [int]$MaxCoastLandAboveSeaDelta = -1,
    [int]$MaxCoastFloorDelta = -1,
    [switch]$RequireDelegatedStatusBefore,
    [switch]$RejectUnexpectedStatusBefore,
    [string[]]$AllowedStatusBefore = @('minecraft:surface', 'minecraft:carvers', 'minecraft:full'),
    [switch]$RunSurvivalPaletteValidation,
    [string]$RconHost = '127.0.0.1',
    [int]$RconPort = 25575,
    [string]$RconPassword = 'earthmap-codex-rcon',
    [string]$ReportPath = '',
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$rustRoot = Join-Path $repoRoot 'rust'
$runScript = Join-Path (Join-Path $rustRoot 'scripts') 'run.ps1'
$rconScript = Join-Path $repoRoot 'scripts\send-rcon-command.mjs'

if (!(Test-Path -LiteralPath $runScript)) {
    throw "Rust run wrapper not found: $runScript"
}

if ($WaitSeconds -lt 0) {
    throw "WaitSeconds must be >= 0."
}
if ($MaxWindows -lt 0) {
    throw "MaxWindows must be >= 0. Use 0 to process every window."
}
if ($MinFullRatioAfter -lt 0.0 -or $MinFullRatioAfter -gt 1.0) {
    throw "MinFullRatioAfter must be in 0.0..1.0."
}
if ($MinFullChunkDelta -lt 0) {
    throw "MinFullChunkDelta must be >= 0."
}
if ($MaxNonFullChunksAfter -lt -1) {
    throw "MaxNonFullChunksAfter must be >= -1. Use -1 to disable."
}
if ($MaxUnderwaterAirColumns -lt -1) {
    throw "MaxUnderwaterAirColumns must be >= -1. Use -1 to disable."
}
if ($MaxUnderwaterAirColumnRatio -lt -1.0 -or $MaxUnderwaterAirColumnRatio -gt 1.0) {
    throw "MaxUnderwaterAirColumnRatio must be in -1.0..1.0. Use -1.0 to disable."
}
if ($MinTreeLeafColumns -lt -1) {
    throw "MinTreeLeafColumns must be >= -1. Use -1 to disable."
}
if ($MinTreeLeafColumnRatio -lt -1.0 -or $MinTreeLeafColumnRatio -gt 1.0) {
    throw "MinTreeLeafColumnRatio must be in -1.0..1.0. Use -1.0 to disable."
}
if (($MaxCoastLandAboveSeaGt8Samples -lt -1) -or ($MaxCoastLandAboveSeaGt16Samples -lt -1) `
        -or ($MaxCoastLandAboveSeaDelta -lt -1) -or ($MaxCoastFloorDelta -lt -1)) {
    throw "Coast thresholds must be >= -1. Use -1 to disable."
}
if (($WindowChunks * $WindowChunks) -gt 256) {
    throw "Minecraft forceload command allows at most 256 chunks per area."
}

function ConvertTo-StatusHistogram {
    param([string[]]$Lines)

    $counts = [ordered]@{}
    $decoded = 0
    foreach ($line in $Lines) {
        if ($line -match '^decodedChunkCount=(\d+)$') {
            $decoded = [int]$Matches[1]
        } elseif ($line -match '^status\.(.+)=(\d+)$') {
            $counts[$Matches[1]] = [int]$Matches[2]
        }
    }
    [PSCustomObject]@{
        decodedChunkCount = $decoded
        statusCounts = $counts
        raw = $Lines
    }
}

function Invoke-EarthMapRs {
    param([string[]]$CliArgs)

    $result = Invoke-EarthMapRsResult -CliArgs $CliArgs
    if ($result.exitCode -ne 0) {
        throw "earthmap-rs failed for args: $($CliArgs -join ' ')"
    }
    $result.output
}

function Invoke-EarthMapRsResult {
    param([string[]]$CliArgs)

    Write-Host ("earthmap.command={0} {1}" -f $runScript, ($CliArgs -join ' '))
    $output = @(& $runScript -RustRoot $rustRoot @CliArgs)
    [PSCustomObject]@{
        exitCode = $LASTEXITCODE
        output = $output
    }
}

function Read-StatusHistogram {
    param([string]$Path)

    $command = if ($Path.EndsWith('.linear', [StringComparison]::OrdinalIgnoreCase)) {
        'inspect-linear-statuses'
    } else {
        'inspect-mca-statuses'
    }
    $lines = Invoke-EarthMapRs -CliArgs @($command, $Path)
    ConvertTo-StatusHistogram $lines
}

function Add-StatusCounts {
    param(
        [hashtable]$Target,
        [object]$Histogram
    )

    foreach ($entry in $Histogram.statusCounts.GetEnumerator()) {
        if (-not $Target.ContainsKey($entry.Key)) {
            $Target[$entry.Key] = 0
        }
        $Target[$entry.Key] += [int]$entry.Value
    }
}

function ConvertTo-OrderedStatusCounts {
    param([hashtable]$Counts)

    $ordered = [ordered]@{}
    foreach ($key in ($Counts.Keys | Sort-Object)) {
        $ordered[$key] = $Counts[$key]
    }
    $ordered
}

function Read-RegionStatuses {
    param([object[]]$Regions)

    $aggregateCounts = @{}
    $decoded = 0
    $items = @()
    $index = 0
    foreach ($region in $Regions) {
        $index++
        Write-Host ("progress.statusScan={0}/{1},region=({2},{3})" -f $index, $Regions.Count,
            $region.regionX, $region.regionZ)
        $histogram = Read-StatusHistogram $region.path
        $decoded += $histogram.decodedChunkCount
        Add-StatusCounts -Target $aggregateCounts -Histogram $histogram
        $items += [PSCustomObject]@{
            regionX = $region.regionX
            regionZ = $region.regionZ
            path = $region.path
            decodedChunkCount = $histogram.decodedChunkCount
            statusCounts = $histogram.statusCounts
        }
    }
    [PSCustomObject]@{
        decodedChunkCount = $decoded
        statusCounts = ConvertTo-OrderedStatusCounts $aggregateCounts
        regions = $items
    }
}

function Invoke-Rcon {
    param([string]$Command)

    $output = & node $rconScript --host $RconHost --port $RconPort --password $RconPassword `
        --command $Command --timeout-ms 60000
    if ($LASTEXITCODE -ne 0) {
        throw "RCON failed for command: $Command"
    }
    $output
}

function ConvertTo-KeyValueMap {
    param([string[]]$Lines)

    $values = [ordered]@{}
    foreach ($line in $Lines) {
        if ($line -match '^([^=]+)=(.*)$') {
            $values[$Matches[1]] = $Matches[2]
        }
    }
    $values
}

function Get-LongValue {
    param(
        [Collections.IDictionary]$Values,
        [string]$Key
    )

    if (-not $Values.Contains($Key)) {
        return 0
    }
    [long]::Parse([string]$Values[$Key], [Globalization.CultureInfo]::InvariantCulture)
}

function Add-LongValue {
    param(
        [hashtable]$Target,
        [string]$Key,
        [long]$Value
    )

    if (-not $Target.ContainsKey($Key)) {
        $Target[$Key] = [long]0
    }
    $Target[$Key] = [long]$Target[$Key] + $Value
}

function ConvertTo-OrderedLongCounts {
    param([hashtable]$Counts)

    $ordered = [ordered]@{}
    foreach ($key in ($Counts.Keys | Sort-Object)) {
        $ordered[$key] = [long]$Counts[$key]
    }
    $ordered
}

function Read-PostFinalIntegrity {
    param([object[]]$Regions)

    $sumKeys = @(
        'regionChunkCount',
        'decodedChunkCount',
        'decodedSectionCount',
        'scannedColumns',
        'landColumns',
        'waterColumns',
        'dryBelowSeaColumns',
        'underwaterAirColumns',
        'treeLogBlocks',
        'treeLeafBlocks',
        'treeLogColumns',
        'treeLeafColumns',
        'coastEdgeSamples',
        'coastLandAboveSeaGt4Samples',
        'coastLandAboveSeaGt8Samples',
        'coastLandAboveSeaGt16Samples'
    )
    $aggregate = @{}
    $topTerrainBlockHits = @{}
    $maxCoastLandAboveSeaDeltaValue = 0
    $maxCoastFloorDeltaValue = 0
    $items = @()
    $index = 0
    foreach ($region in $Regions) {
        $index++
        Write-Host ("progress.postFinalIntegrity={0}/{1},region=({2},{3})" -f $index, $Regions.Count,
            $region.regionX, $region.regionZ)
        $command = if ($region.path.EndsWith('.linear', [StringComparison]::OrdinalIgnoreCase)) {
            'inspect-linear-post-final-integrity'
        } else {
            'inspect-mca-post-final-integrity'
        }
        $lines = Invoke-EarthMapRs -CliArgs @($command, $region.path)
        $values = ConvertTo-KeyValueMap $lines
        foreach ($key in $sumKeys) {
            Add-LongValue -Target $aggregate -Key $key -Value (Get-LongValue -Values $values -Key $key)
        }
        $maxCoastLandAboveSeaDeltaValue = [Math]::Max($maxCoastLandAboveSeaDeltaValue,
            [int](Get-LongValue -Values $values -Key 'maxCoastLandAboveSeaDelta'))
        $maxCoastFloorDeltaValue = [Math]::Max($maxCoastFloorDeltaValue,
            [int](Get-LongValue -Values $values -Key 'maxCoastFloorDelta'))
        foreach ($entry in $values.GetEnumerator()) {
            if ($entry.Key.StartsWith('topTerrainBlockHits.', [StringComparison]::Ordinal)) {
                $blockName = $entry.Key.Substring('topTerrainBlockHits.'.Length)
                Add-LongValue -Target $topTerrainBlockHits -Key $blockName `
                    -Value ([long]::Parse([string]$entry.Value, [Globalization.CultureInfo]::InvariantCulture))
            }
        }
        $items += [PSCustomObject]@{
            regionX = $region.regionX
            regionZ = $region.regionZ
            path = $region.path
            scannedColumns = Get-LongValue -Values $values -Key 'scannedColumns'
            landColumns = Get-LongValue -Values $values -Key 'landColumns'
            waterColumns = Get-LongValue -Values $values -Key 'waterColumns'
            underwaterAirColumns = Get-LongValue -Values $values -Key 'underwaterAirColumns'
            treeLeafColumns = Get-LongValue -Values $values -Key 'treeLeafColumns'
            coastEdgeSamples = Get-LongValue -Values $values -Key 'coastEdgeSamples'
            maxCoastLandAboveSeaDelta = Get-LongValue -Values $values -Key 'maxCoastLandAboveSeaDelta'
            maxCoastFloorDelta = Get-LongValue -Values $values -Key 'maxCoastFloorDelta'
        }
    }

    $waterColumns = [long]$aggregate['waterColumns']
    $landColumns = [long]$aggregate['landColumns']
    $underwaterAirColumns = [long]$aggregate['underwaterAirColumns']
    $treeLeafColumns = [long]$aggregate['treeLeafColumns']
    [PSCustomObject]@{
        counts = ConvertTo-OrderedLongCounts $aggregate
        ratios = [PSCustomObject]@{
            underwaterAirColumnRatio = if ($waterColumns -gt 0) {
                $underwaterAirColumns / [double]$waterColumns
            } else {
                0.0
            }
            treeLeafColumnRatio = if ($landColumns -gt 0) {
                $treeLeafColumns / [double]$landColumns
            } else {
                0.0
            }
        }
        maxima = [PSCustomObject]@{
            maxCoastLandAboveSeaDelta = $maxCoastLandAboveSeaDeltaValue
            maxCoastFloorDelta = $maxCoastFloorDeltaValue
        }
        topTerrainBlockHits = ConvertTo-OrderedLongCounts $topTerrainBlockHits
        regions = $items
    }
}

function Read-SurvivalPaletteValidation {
    param([object[]]$Regions)

    $items = @()
    $index = 0
    foreach ($region in $Regions) {
        $index++
        Write-Host ("progress.survivalPalette={0}/{1},region=({2},{3})" -f $index, $Regions.Count,
            $region.regionX, $region.regionZ)
        $command = if ($region.path.EndsWith('.linear', [StringComparison]::OrdinalIgnoreCase)) {
            'validate-linear-survival-palette'
        } else {
            'validate-mca-survival-palette'
        }
        $result = Invoke-EarthMapRsResult -CliArgs @($command, $region.path)
        $values = ConvertTo-KeyValueMap $result.output
        $complete = $values.Contains('survivalCriticalComplete') `
            -and ([string]$values['survivalCriticalComplete']).Equals('true', [StringComparison]::OrdinalIgnoreCase)
        $items += [PSCustomObject]@{
            regionX = $region.regionX
            regionZ = $region.regionZ
            path = $region.path
            command = $command
            exitCode = $result.exitCode
            survivalCriticalComplete = $complete
            raw = $result.output
        }
    }

    $failed = @($items | Where-Object { $_.exitCode -ne 0 -or -not $_.survivalCriticalComplete })
    [PSCustomObject]@{
        scannedRegionCount = $items.Count
        pass = $failed.Count -eq 0
        failedRegionCount = $failed.Count
        regions = $items
    }
}

function New-Window {
    param(
        [int]$RegionX,
        [int]$RegionZ,
        [int]$LocalChunkX,
        [int]$LocalChunkZ
    )

    $width = [Math]::Min($WindowChunks, 32 - $LocalChunkX)
    $height = [Math]::Min($WindowChunks, 32 - $LocalChunkZ)
    $globalChunkX0 = ($RegionX * 32) + $LocalChunkX
    $globalChunkZ0 = ($RegionZ * 32) + $LocalChunkZ
    $globalChunkX1 = $globalChunkX0 + $width - 1
    $globalChunkZ1 = $globalChunkZ0 + $height - 1
    $minBlockX = $globalChunkX0 * 16
    $minBlockZ = $globalChunkZ0 * 16
    $maxBlockX = (($globalChunkX1 + 1) * 16) - 1
    $maxBlockZ = (($globalChunkZ1 + 1) * 16) - 1

    [PSCustomObject]@{
        regionX = $RegionX
        regionZ = $RegionZ
        localChunkX = $LocalChunkX
        localChunkZ = $LocalChunkZ
        width = $width
        height = $height
        chunkCount = $width * $height
        globalChunkX0 = $globalChunkX0
        globalChunkZ0 = $globalChunkZ0
        globalChunkX1 = $globalChunkX1
        globalChunkZ1 = $globalChunkZ1
        minBlockX = $minBlockX
        minBlockZ = $minBlockZ
        maxBlockX = $maxBlockX
        maxBlockZ = $maxBlockZ
        forceAdd = "forceload add $minBlockX $minBlockZ $maxBlockX $maxBlockZ"
        forceRemove = "forceload remove $minBlockX $minBlockZ $maxBlockX $maxBlockZ"
    }
}

function Select-Windows {
    param(
        [object[]]$Windows,
        [int]$Limit
    )

    if ($Limit -le 0 -or $Windows.Count -le $Limit) {
        return @($Windows)
    }
    if ($Limit -eq 1) {
        return @($Windows[[int][Math]::Floor(($Windows.Count - 1) / 2.0)])
    }

    $selected = @()
    $seen = @{}
    for ($i = 0; $i -lt $Limit; $i++) {
        $index = [int][Math]::Round(($i * ($Windows.Count - 1)) / [double]($Limit - 1))
        while ($seen.ContainsKey($index) -and $index -lt ($Windows.Count - 1)) {
            $index++
        }
        while ($seen.ContainsKey($index) -and $index -gt 0) {
            $index--
        }
        $seen[$index] = $true
        $selected += $Windows[$index]
    }
    $selected
}

$regionDir = Join-Path $WorldDir 'region'
if (!(Test-Path -LiteralPath $regionDir)) {
    throw "Region directory not found: $regionDir"
}

$regions = @()
$allWindows = @()
for ($rz = $StartRegionZ; $rz -lt ($StartRegionZ + $RegionRows); $rz++) {
    for ($rx = $StartRegionX; $rx -lt ($StartRegionX + $RegionCols); $rx++) {
        $mcaRegionFile = Join-Path $regionDir ("r.{0}.{1}.mca" -f $rx, $rz)
        $linearRegionFile = Join-Path $regionDir ("r.{0}.{1}.linear" -f $rx, $rz)
        $regionFile = switch ($RegionFormat) {
            'mca' { $mcaRegionFile }
            'linear' { $linearRegionFile }
            default {
                if (Test-Path -LiteralPath $linearRegionFile) {
                    $linearRegionFile
                } else {
                    $mcaRegionFile
                }
            }
        }
        if (!(Test-Path -LiteralPath $regionFile)) {
            Write-Host ("skip.missingRegion=({0},{1})" -f $rx, $rz)
            continue
        }
        $region = [PSCustomObject]@{
            regionX = $rx
            regionZ = $rz
            path = $regionFile
        }
        $regions += $region
        for ($localZ = 0; $localZ -lt 32; $localZ += $WindowChunks) {
            for ($localX = 0; $localX -lt 32; $localX += $WindowChunks) {
                $allWindows += New-Window -RegionX $rx -RegionZ $rz -LocalChunkX $localX -LocalChunkZ $localZ
            }
        }
    }
}

if ($regions.Count -eq 0) {
    throw "No existing regions found in requested window."
}

$windows = Select-Windows -Windows $allWindows -Limit $MaxWindows
if ($windows.Count -eq 0) {
    throw "No finalization windows selected."
}
$scannedRegionKeys = @{}
foreach ($window in $windows) {
    $scannedRegionKeys[("{0},{1}" -f $window.regionX, $window.regionZ)] = $true
}
$scannedRegions = @($regions | Where-Object {
        $scannedRegionKeys.ContainsKey(("{0},{1}" -f $_.regionX, $_.regionZ))
    })
if ($scannedRegions.Count -eq 0) {
    throw "No regions selected for status/integrity scans."
}

Write-Host ("finalization.regions={0}" -f $regions.Count)
Write-Host ("finalization.regions.scanned={0}" -f $scannedRegions.Count)
Write-Host ("finalization.windows.total={0}" -f $allWindows.Count)
Write-Host ("finalization.windows.selected={0}" -f $windows.Count)
Write-Host ("finalization.windowChunks={0}" -f $WindowChunks)
Write-Host ("finalization.waitSeconds={0}" -f $WaitSeconds)
Write-Host ("finalization.regionFormat={0}" -f $RegionFormat)

$before = Read-RegionStatuses $scannedRegions
$startedAt = Get-Date
$windowReports = @()
$windowIndex = 0

if ($DryRun) {
    Write-Host ("progress.finalizationWindow=skipped,dryRun=true,windows={0}" -f $windows.Count)
} else {
    foreach ($window in $windows) {
        $windowIndex++
        Write-Host ("progress.finalizationWindow={0}/{1},region=({2},{3}),local=({4},{5}),chunks={6}" -f `
                $windowIndex, $windows.Count, $window.regionX, $window.regionZ, $window.localChunkX,
                $window.localChunkZ, $window.chunkCount)
        $forceAddOutput = @()
        $saveOutput = @()
        $forceRemoveOutput = @()
        $errorText = $null
        $windowStartedAt = Get-Date
        try {
            $forceAddOutput = Invoke-Rcon $window.forceAdd
            if ($WaitSeconds -gt 0) {
                Start-Sleep -Seconds $WaitSeconds
            }
            $saveOutput = Invoke-Rcon 'save-all flush'
        } catch {
            $errorText = $_.Exception.Message
            throw
        } finally {
            try {
                $forceRemoveOutput = Invoke-Rcon $window.forceRemove
            } catch {
                if ($null -eq $errorText) {
                    $errorText = $_.Exception.Message
                }
                Write-Warning ("Failed to remove forceload window: {0}" -f $_.Exception.Message)
            }
            $windowReports += [PSCustomObject]@{
                index = $windowIndex
                startedAt = $windowStartedAt.ToString('o')
                completedAt = (Get-Date).ToString('o')
                regionX = $window.regionX
                regionZ = $window.regionZ
                localChunkX = $window.localChunkX
                localChunkZ = $window.localChunkZ
                width = $window.width
                height = $window.height
                chunkCount = $window.chunkCount
                minBlockX = $window.minBlockX
                minBlockZ = $window.minBlockZ
                maxBlockX = $window.maxBlockX
                maxBlockZ = $window.maxBlockZ
                commands = [PSCustomObject]@{
                    forceAdd = $window.forceAdd
                    save = 'save-all flush'
                    forceRemove = $window.forceRemove
                }
                rconOutput = [PSCustomObject]@{
                    forceAdd = $forceAddOutput
                    save = $saveOutput
                    forceRemove = $forceRemoveOutput
                }
                error = $errorText
            }
        }
    }
}

$after = if ($DryRun) { $before } else { Read-RegionStatuses $scannedRegions }
$fullBefore = [int]($before.statusCounts['minecraft:full'])
$fullAfter = [int]($after.statusCounts['minecraft:full'])
$surfaceBefore = [int]($before.statusCounts['minecraft:surface'])
$carversBefore = [int]($before.statusCounts['minecraft:carvers'])
$delegatedBefore = $surfaceBefore + $carversBefore
$decodedAfter = [int]$after.decodedChunkCount
$nonFullAfter = $decodedAfter - $fullAfter
$fullRatioAfter = if ($decodedAfter -gt 0) { $fullAfter / [double]$decodedAfter } else { 0.0 }
$failures = @()
$unexpectedStatusBefore = @()
foreach ($entry in $before.statusCounts.GetEnumerator()) {
    if ($entry.Value -gt 0 -and -not ($AllowedStatusBefore -contains $entry.Key)) {
        $unexpectedStatusBefore += [PSCustomObject]@{
            status = $entry.Key
            count = [int]$entry.Value
        }
    }
}
if ($RequireDelegatedStatusBefore -and $delegatedBefore -le 0) {
    $failures += "delegatedStatusBefore=0; expected at least one minecraft:surface or minecraft:carvers chunk before finalization"
}
if ($RejectUnexpectedStatusBefore -and $unexpectedStatusBefore.Count -gt 0) {
    $failures += ("unexpectedStatusBefore={0}" -f (($unexpectedStatusBefore | ForEach-Object {
                "$($_.status):$($_.count)"
            }) -join ','))
}
if ($MinFullRatioAfter -gt 0.0 -and $fullRatioAfter -lt $MinFullRatioAfter) {
    $failures += ("fullRatioAfter={0:N4} < required {1:N4}" -f $fullRatioAfter, $MinFullRatioAfter)
}
if ($MinFullChunkDelta -gt 0 -and (($fullAfter - $fullBefore) -lt $MinFullChunkDelta)) {
    $failures += ("fullChunksDelta={0} < required {1}" -f ($fullAfter - $fullBefore), $MinFullChunkDelta)
}
if ($MaxNonFullChunksAfter -ge 0 -and $nonFullAfter -gt $MaxNonFullChunksAfter) {
    $failures += ("nonFullChunksAfter={0} > allowed {1}" -f $nonFullAfter, $MaxNonFullChunksAfter)
}
$postFinalIntegrity = $null
if ($RunPostFinalIntegrity) {
    $postFinalIntegrity = Read-PostFinalIntegrity $scannedRegions
    $postCounts = $postFinalIntegrity.counts
    $postRatios = $postFinalIntegrity.ratios
    $postMaxima = $postFinalIntegrity.maxima
    if (($MaxUnderwaterAirColumns -ge 0) `
            -and (([long]$postCounts['underwaterAirColumns']) -gt $MaxUnderwaterAirColumns)) {
        $failures += ("underwaterAirColumns={0} > allowed {1}" -f `
                ([long]$postCounts['underwaterAirColumns']), $MaxUnderwaterAirColumns)
    }
    if (($MaxUnderwaterAirColumnRatio -ge 0.0) `
            -and (([double]$postRatios.underwaterAirColumnRatio) -gt $MaxUnderwaterAirColumnRatio)) {
        $failures += ("underwaterAirColumnRatio={0:N6} > allowed {1:N6}" -f `
                ([double]$postRatios.underwaterAirColumnRatio), $MaxUnderwaterAirColumnRatio)
    }
    if (($MinTreeLeafColumns -ge 0) `
            -and (([long]$postCounts['treeLeafColumns']) -lt $MinTreeLeafColumns)) {
        $failures += ("treeLeafColumns={0} < required {1}" -f `
                ([long]$postCounts['treeLeafColumns']), $MinTreeLeafColumns)
    }
    if (($MinTreeLeafColumnRatio -ge 0.0) `
            -and (([double]$postRatios.treeLeafColumnRatio) -lt $MinTreeLeafColumnRatio)) {
        $failures += ("treeLeafColumnRatio={0:N6} < required {1:N6}" -f `
                ([double]$postRatios.treeLeafColumnRatio), $MinTreeLeafColumnRatio)
    }
    if (($MaxCoastLandAboveSeaGt8Samples -ge 0) `
            -and (([long]$postCounts['coastLandAboveSeaGt8Samples']) -gt $MaxCoastLandAboveSeaGt8Samples)) {
        $failures += ("coastLandAboveSeaGt8Samples={0} > allowed {1}" -f `
                ([long]$postCounts['coastLandAboveSeaGt8Samples']), $MaxCoastLandAboveSeaGt8Samples)
    }
    if (($MaxCoastLandAboveSeaGt16Samples -ge 0) `
            -and (([long]$postCounts['coastLandAboveSeaGt16Samples']) -gt $MaxCoastLandAboveSeaGt16Samples)) {
        $failures += ("coastLandAboveSeaGt16Samples={0} > allowed {1}" -f `
                ([long]$postCounts['coastLandAboveSeaGt16Samples']), $MaxCoastLandAboveSeaGt16Samples)
    }
    if (($MaxCoastLandAboveSeaDelta -ge 0) `
            -and (([int]$postMaxima.maxCoastLandAboveSeaDelta) -gt $MaxCoastLandAboveSeaDelta)) {
        $failures += ("maxCoastLandAboveSeaDelta={0} > allowed {1}" -f `
                ([int]$postMaxima.maxCoastLandAboveSeaDelta), $MaxCoastLandAboveSeaDelta)
    }
    if (($MaxCoastFloorDelta -ge 0) `
            -and (([int]$postMaxima.maxCoastFloorDelta) -gt $MaxCoastFloorDelta)) {
        $failures += ("maxCoastFloorDelta={0} > allowed {1}" -f `
                ([int]$postMaxima.maxCoastFloorDelta), $MaxCoastFloorDelta)
    }
}
$survivalPaletteValidation = $null
if ($RunSurvivalPaletteValidation) {
    $survivalPaletteValidation = Read-SurvivalPaletteValidation $scannedRegions
    if (-not $survivalPaletteValidation.pass) {
        $failures += ("survivalPaletteValidation failedRegions={0}/{1}" -f `
                $survivalPaletteValidation.failedRegionCount, $survivalPaletteValidation.scannedRegionCount)
    }
}

$report = [PSCustomObject]@{
    schemaVersion = 1
    generatedAt = (Get-Date).ToString('o')
    dryRun = [bool]$DryRun
    worldDir = $WorldDir
    regionFormat = $RegionFormat
    regionWindow = [PSCustomObject]@{
        startRegionX = $StartRegionX
        startRegionZ = $StartRegionZ
        regionCols = $RegionCols
        regionRows = $RegionRows
    }
    existingRegionCount = $regions.Count
    scannedRegionCount = $scannedRegions.Count
    windowChunks = $WindowChunks
    waitSeconds = $WaitSeconds
    maxWindows = $MaxWindows
    thresholds = [PSCustomObject]@{
        minFullRatioAfter = $MinFullRatioAfter
        minFullChunkDelta = $MinFullChunkDelta
        maxNonFullChunksAfter = $MaxNonFullChunksAfter
        runPostFinalIntegrity = [bool]$RunPostFinalIntegrity
        maxUnderwaterAirColumns = $MaxUnderwaterAirColumns
        maxUnderwaterAirColumnRatio = $MaxUnderwaterAirColumnRatio
        minTreeLeafColumns = $MinTreeLeafColumns
        minTreeLeafColumnRatio = $MinTreeLeafColumnRatio
        maxCoastLandAboveSeaGt8Samples = $MaxCoastLandAboveSeaGt8Samples
        maxCoastLandAboveSeaGt16Samples = $MaxCoastLandAboveSeaGt16Samples
        maxCoastLandAboveSeaDelta = $MaxCoastLandAboveSeaDelta
        maxCoastFloorDelta = $MaxCoastFloorDelta
        requireDelegatedStatusBefore = [bool]$RequireDelegatedStatusBefore
        rejectUnexpectedStatusBefore = [bool]$RejectUnexpectedStatusBefore
        allowedStatusBefore = $AllowedStatusBefore
        runSurvivalPaletteValidation = [bool]$RunSurvivalPaletteValidation
    }
    totalCandidateWindows = $allWindows.Count
    selectedWindowCount = $windows.Count
    startedAt = $startedAt.ToString('o')
    completedAt = (Get-Date).ToString('o')
    before = $before
    after = $after
    chunkStatusCompatibility = [PSCustomObject]@{
        delegatedChunksBefore = $delegatedBefore
        surfaceChunksBefore = $surfaceBefore
        carversChunksBefore = $carversBefore
        fullChunksBefore = $fullBefore
        unexpectedStatusBefore = $unexpectedStatusBefore
        pass = (-not $RequireDelegatedStatusBefore -or $delegatedBefore -gt 0) `
            -and (-not $RejectUnexpectedStatusBefore -or $unexpectedStatusBefore.Count -eq 0)
    }
    summary = [PSCustomObject]@{
        decodedChunkCount = $decodedAfter
        fullChunksBefore = $fullBefore
        fullChunksAfter = $fullAfter
        fullChunksDelta = $fullAfter - $fullBefore
        nonFullChunksAfter = $nonFullAfter
        fullRatioAfter = $fullRatioAfter
        pass = $failures.Count -eq 0
        failures = $failures
    }
    postFinalIntegrity = $postFinalIntegrity
    survivalPaletteValidation = $survivalPaletteValidation
    windows = $windowReports
}

if ([string]::IsNullOrWhiteSpace($ReportPath)) {
    $ReportPath = Join-Path $WorldDir 'earthmap-server-finalization-windows.json'
}

$reportJson = $report | ConvertTo-Json -Depth 16
$parent = Split-Path -Parent $ReportPath
if (-not [string]::IsNullOrWhiteSpace($parent)) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
}
$reportJson | Set-Content -Encoding UTF8 -LiteralPath $ReportPath

Write-Output $reportJson
Write-Host ("server-finalization-windows-report={0}" -f $ReportPath)
if ($failures.Count -gt 0) {
    throw ("Server finalization gate failed: {0}" -f ($failures -join '; '))
}
