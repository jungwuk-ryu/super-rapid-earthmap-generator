[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$WorldDir,

    [int]$TotalRegions = 0,

    [switch]$Watch,

    [int]$PollSeconds = 5
)

$ErrorActionPreference = 'Stop'

function Read-Properties([string]$Path) {
    $map = @{}
    if (-not (Test-Path -LiteralPath $Path)) {
        return $map
    }
    foreach ($line in Get-Content -LiteralPath $Path) {
        $trimmed = $line.Trim()
        if ($trimmed.Length -eq 0 -or $trimmed.StartsWith('#')) {
            continue
        }
        $parts = $trimmed.Split('=', 2)
        if ($parts.Count -eq 2) {
            $map[$parts[0].Trim()] = $parts[1].Trim()
        }
    }
    return $map
}

function Format-Duration([double]$Seconds) {
    if ([double]::IsNaN($Seconds) -or [double]::IsInfinity($Seconds) -or $Seconds -lt 0) {
        return 'unknown'
    }
    $span = [TimeSpan]::FromSeconds($Seconds)
    if ($span.TotalDays -ge 1) {
        return ('{0:N1}d' -f $span.TotalDays)
    }
    if ($span.TotalHours -ge 1) {
        return ('{0:N1}h' -f $span.TotalHours)
    }
    if ($span.TotalMinutes -ge 1) {
        return ('{0:N1}m' -f $span.TotalMinutes)
    }
    return ('{0:N0}s' -f $span.TotalSeconds)
}

function Format-Message([string]$Message, [int]$MaxLength = 240) {
    if ([string]::IsNullOrWhiteSpace($Message)) {
        return ''
    }
    $oneLine = ($Message -replace '[\r\n\t]+', ' ').Trim()
    if ($oneLine.Length -gt $MaxLength) {
        return $oneLine.Substring(0, $MaxLength) + '...'
    }
    return $oneLine
}

function ConvertTo-KeyValueMap([string]$Line) {
    $map = @{}
    if ([string]::IsNullOrWhiteSpace($Line)) {
        return $map
    }
    foreach ($part in ($Line -split ',')) {
        $pieces = $part -split '=', 2
        if ($pieces.Count -eq 2) {
            $map[$pieces[0].Trim()] = $pieces[1].Trim()
        }
    }
    return $map
}

function Read-LatestGeneratorProgress([string]$LogPath) {
    if (-not (Test-Path -LiteralPath $LogPath)) {
        return @{}
    }
    $lines = @(Get-Content -LiteralPath $LogPath -Tail 300 -ErrorAction SilentlyContinue)
    for ($i = $lines.Count - 1; $i -ge 0; $i--) {
        if ($lines[$i] -match '^progress,') {
            return (ConvertTo-KeyValueMap $lines[$i])
        }
    }
    return @{}
}

function Show-ProgressSnapshot() {
    $progressPath = Join-Path $WorldDir 'earthmap-region-progress.csv'
    $configPath = Join-Path $WorldDir 'earthmap-region-batch.properties'
    $regionPath = Join-Path $WorldDir 'region'
    $generationLog = Join-Path (Join-Path $WorldDir 'logs') 'generation.log'
    $previewImage = Join-Path $WorldDir 'preview.png'
    $previewViewer = Join-Path (Join-Path $WorldDir 'preview-tiles') 'index.html'
    $summaryPath = Join-Path (Join-Path $WorldDir 'debug') 'stats\summary.txt'

    $config = Read-Properties $configPath
    $effectiveTotal = $TotalRegions
    if ($effectiveTotal -le 0 -and $config.ContainsKey('generation.cols') -and $config.ContainsKey('generation.rows')) {
        $effectiveTotal = [int]$config['generation.cols'] * [int]$config['generation.rows']
    }

    $rows = @()
    $readError = ''
    if (Test-Path -LiteralPath $progressPath) {
        try {
            $rows = @(Import-Csv -LiteralPath $progressPath)
        } catch {
            $readError = $_.Exception.Message
        }
    }
    $latestByRegion = @{}
    foreach ($row in $rows) {
        $key = '{0},{1}' -f $row.regionX, $row.regionZ
        $latestByRegion[$key] = $row
    }
    $latestRows = @($latestByRegion.Values)
    $generated = @($latestRows | Where-Object { $_.status -eq 'GENERATED' }).Count
    $skipped = @($latestRows | Where-Object { $_.status -in @('SKIPPED_EXISTING', 'PREVIEW_REBUILT') }).Count
    $failedRows = @($latestRows | Where-Object { $_.status -eq 'FAILED' })
    $failed = $failedRows.Count
    $processed = $generated + $skipped + $failed
    $linearFileCount = 0
    $mcaFileCount = 0
    if (Test-Path -LiteralPath $regionPath) {
        $linearFileCount = @(Get-ChildItem -LiteralPath $regionPath -Filter '*.linear' -File -ErrorAction SilentlyContinue).Count
        $mcaFileCount = @(Get-ChildItem -LiteralPath $regionPath -Filter '*.mca' -File -ErrorAction SilentlyContinue).Count
    }

    $last = $rows | Select-Object -Last 1
    $now = Get-Date
    $progressMtime = $null
    if (Test-Path -LiteralPath $progressPath) {
        $progressMtime = (Get-Item -LiteralPath $progressPath).LastWriteTime
    }

    $rate = 0.0
    $etaSeconds = [double]::NaN
    if ($rows.Count -ge 2) {
        $first = $rows | Select-Object -First 1
        $firstTime = [datetime]::Parse($first.timestamp)
        $lastTime = [datetime]::Parse($last.timestamp)
        $elapsedSeconds = ($lastTime - $firstTime).TotalSeconds
        if ($elapsedSeconds -gt 0) {
            $rate = ($processed * 3600.0) / $elapsedSeconds
            if ($effectiveTotal -gt 0 -and $rate -gt 0) {
                $etaSeconds = (($effectiveTotal - $processed) / $rate) * 3600.0
            }
        }
    }

    $percent = if ($effectiveTotal -gt 0) { ($processed * 100.0) / $effectiveTotal } else { 0.0 }
    $lastRegion = if ($last) { 'r.{0}.{1}' -f $last.regionX, $last.regionZ } else { 'none' }
    $lastAgeSeconds = if ($progressMtime) { (($now - $progressMtime).TotalSeconds) } else { [double]::NaN }
    $lastAge = if ($progressMtime) { Format-Duration $lastAgeSeconds } else { 'unknown' }
    $staleAfterSeconds = [Math]::Max(30, $PollSeconds * 3)
    $progressFresh = if ($progressMtime) { $lastAgeSeconds -le $staleAfterSeconds } else { $false }
    $totalText = if ($effectiveTotal -gt 0) { $effectiveTotal.ToString() } else { 'unknown' }
    $latestFailure = @($failedRows | Sort-Object timestamp | Select-Object -Last 1)
    $latestFailureText = if ($latestFailure.Count -gt 0) {
        'r.{0}.{1}: {2}' -f $latestFailure[0].regionX, $latestFailure[0].regionZ, (Format-Message $latestFailure[0].message)
    } else {
        ''
    }
    $knownStatuses = @('GENERATED', 'SKIPPED_EXISTING', 'PREVIEW_REBUILT', 'FAILED')
    $otherStatuses = @($latestRows | Where-Object { $_.status -notin $knownStatuses } | Group-Object status | ForEach-Object {
            '{0}:{1}' -f $_.Name, $_.Count
        })
    $generatorProgress = Read-LatestGeneratorProgress $generationLog
    $generatorEta = 'unknown'
    if ($generatorProgress.ContainsKey('etaSeconds')) {
        $etaValue = 0.0
        if ([double]::TryParse($generatorProgress['etaSeconds'], [ref]$etaValue)) {
            $generatorEta = Format-Duration $etaValue
        }
    }
    $sampleName = Split-Path -Leaf $WorldDir

    [pscustomobject]@{
        time = $now.ToString('yyyy-MM-dd HH:mm:ss')
        sample = $sampleName
        worldDir = $WorldDir
        processed = $processed
        total = $totalText
        percent = ('{0:N2}' -f $percent)
        generated = $generated
        skipped = $skipped
        failed = $failed
        progressRows = $rows.Count
        uniqueRegions = $latestRows.Count
        duplicateProgressRows = [Math]::Max(0, $rows.Count - $latestRows.Count)
        regionFiles = $linearFileCount + $mcaFileCount
        linearRegionFiles = $linearFileCount
        mcaRegionFiles = $mcaFileCount
        rateRegionsPerHour = ('{0:N1}' -f $rate)
        eta = Format-Duration $etaSeconds
        generatorFinished = if ($generatorProgress.ContainsKey('finished')) { $generatorProgress['finished'] } else { '' }
        generatorPlanned = if ($generatorProgress.ContainsKey('planned')) { $generatorProgress['planned'] } else { '' }
        generatorPercent = if ($generatorProgress.ContainsKey('percent')) { $generatorProgress['percent'] } else { '' }
        generatorRateRegionsPerHour = if ($generatorProgress.ContainsKey('regionsPerHour')) { $generatorProgress['regionsPerHour'] } else { '' }
        generatorEta = $generatorEta
        lastRegion = $lastRegion
        lastStatus = if ($last) { $last.status } else { 'none' }
        lastMessage = if ($last) { Format-Message $last.message } else { '' }
        lastUpdateAge = $lastAge
        progressFresh = $progressFresh
        staleAfterSeconds = $staleAfterSeconds
        latestFailure = $latestFailureText
        otherStatuses = ($otherStatuses -join ',')
        readError = $readError
        progressFile = $progressPath
        generationLog = $generationLog
        preview = $previewImage
        previewViewer = $previewViewer
        summary = $summaryPath
    }
}

do {
    if ($Watch) {
        Clear-Host
    }
    Show-ProgressSnapshot | Format-List
    if ($Watch) {
        Start-Sleep -Seconds $PollSeconds
    }
} while ($Watch)
