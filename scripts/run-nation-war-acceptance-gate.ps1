[CmdletBinding()]
param(
    [ValidateSet('plan', 'samples-5000', 'samples-1000', 'parity-existing', 'server-finalization')]
    [string[]]$Stages = @('plan'),
    [string]$Heightmap = 'C:\earth_map_resources\HQheightmap.tif',
    [string]$OutputRoot = 'D:\earthmap\nation-war-acceptance',
    [int]$Threads5000 = 6,
    [int]$Threads1000 = 6,
    [string]$CacheRows = '512',
    [string]$PrefetchRows = '0',
    [double]$VerticalScale = 1.25,
    [ValidateSet('photo', 'classified')]
    [string]$TextureMode = 'photo',
    [switch]$UseEcologyQualityGate,
    [string]$BaselineRoot5000 = '',
    [string]$BaselineRoot1000 = '',
    [switch]$CleanSampleOutput,
    [switch]$UseLatestKnownParityEvidence,
    [string]$ParitySourcePng = '',
    [string]$ParityExpectedPng = '',
    [string]$ParityCurrentPng = '',
    [string]$ParityMaskPng = '',
    [string]$ParityOutputDir = '',
    [double]$MaxParityMeanDeltaE2000 = 6.40,
    [double]$MaxParityP95DeltaE2000 = 10.20,
    [double]$MinParityGlobalLumaSsim = 0.9700,
    [double]$MaxParityDeltaEOver10Percent = 12.0,
    [double]$MaxParityDeltaEOver20Percent = 0.12,
    [double]$MaxParityDeltaEOver30Percent = 0.0,
    [string]$ServerWorldDir = 'D:\worldgen\world',
    [ValidateSet('auto', 'mca', 'linear')]
    [string]$ServerRegionFormat = 'auto',
    [int]$ServerStartRegionX = -1,
    [int]$ServerStartRegionZ = -1,
    [int]$ServerRegionCols = 2,
    [int]$ServerRegionRows = 2,
    [int]$ServerMaxWindows = 16,
    [int]$ServerWaitSeconds = 30,
    [string]$RconHost = '127.0.0.1',
    [int]$RconPort = 25575,
    [string]$RconPassword = 'earthmap-codex-rcon'
)

$ErrorActionPreference = 'Stop'

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = Split-Path -Parent $scriptRoot
$sampleScript = Join-Path $scriptRoot 'run-quality-acceptance-samples.ps1'
$metricScript = Join-Path $scriptRoot 'run-photo-parity-metric-crop.ps1'
$finalizationScript = Join-Path $scriptRoot 'run-server-finalization-windows.ps1'

$samples5000 = @(
    'west-africa',
    'sahara-core',
    'sahel-edge',
    'congo-edge',
    'arabia-coast',
    'australia-dry',
    'amazon-edge',
    'europe-forest',
    'mediterranean-edge'
)
$samples1000 = @(
    'west-africa',
    'arabia-coast',
    'amazon-edge',
    'mediterranean-edge'
)

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
        return ''
    }
    $oneLine = ($Message -replace '[\r\n\t]+', ' ').Trim()
    if ($oneLine.Length -gt $MaxLength) {
        return $oneLine.Substring(0, $MaxLength) + '...'
    }
    return $oneLine
}

function ConvertTo-LogText {
    param([object]$Value)
    if ($null -eq $Value) {
        return ''
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
        [string]$FallbackMessage = '',
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
        'java\.lang\.[A-Za-z0-9_]+(?:Exception|Error)',
        'CLI failed with exit code',
        'photo-parity-metric-crop failed',
        'qualityAcceptance\.failure',
        'qualityAcceptance\.sample\.failure',
        'Server finalization gate failed',
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
                return (Format-LogMessage ($context -join ' | '))
            }
        }
    }
    if (-not [string]::IsNullOrWhiteSpace($FallbackMessage)) {
        return (Format-LogMessage $FallbackMessage)
    }
    $tail = @($lines | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Last 6)
    return (Format-LogMessage ($tail -join ' | '))
}

function Get-LogLastMatchingLine {
    param(
        [string]$LogPath,
        [string]$Pattern,
        [int]$TailLines = 1000
    )
    if (-not (Test-Path -LiteralPath $LogPath)) {
        return ''
    }
    $lines = @(Get-Content -LiteralPath $LogPath -Tail $TailLines -ErrorAction SilentlyContinue | ForEach-Object {
            ConvertTo-LogText $_
        })
    for ($i = $lines.Count - 1; $i -ge 0; $i--) {
        if ($lines[$i] -match $Pattern) {
            return (Format-LogMessage $lines[$i])
        }
    }
    return ''
}

function Write-SampleGateFailureDetails {
    param(
        [string]$LogPath,
        [string]$OutputDir
    )
    foreach ($entry in @(
            @{ name = 'failedSample'; pattern = 'qualityAcceptance\.sample\.end=fail' },
            @{ name = 'failure'; pattern = 'qualityAcceptance\.sample\.failure\.' },
            @{ name = 'failureArtifacts'; pattern = 'qualityAcceptance\.sample\.failureArtifacts\.' },
            @{ name = 'latestProgress'; pattern = 'qualityAcceptance\.sample\.progress=' },
            @{ name = 'retryFailedSample'; pattern = 'qualityAcceptance\.nextCommand\.failedSample=' }
        )) {
        $line = Get-LogLastMatchingLine -LogPath $LogPath -Pattern $entry['pattern']
        if (-not [string]::IsNullOrWhiteSpace($line)) {
            Write-Host ("nationWarAcceptanceGate.sampleGate.{0}={1}" -f $entry['name'], $line)
        }
    }
    Write-Host ("nationWarAcceptanceGate.sampleGate.artifacts=output={0},log={1}" -f $OutputDir, $LogPath)
}

function Invoke-LoggedCommand {
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
    $exitCode = 0
    $errorMessage = ''
    try {
        $global:LASTEXITCODE = 0
        & $Command 2>&1 | ForEach-Object {
            $line = ConvertTo-LogText $_
            Write-LogLine -LogPath $LogPath -Line $line
            Write-Host $line
        }
        # These commands invoke PowerShell scripts. A completed scriptblock is success; using
        # $LASTEXITCODE here can report a stale native-process exit code from inside the child script.
        $exitCode = 0
    } catch {
        $exitCode = 1
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

function Assert-FreshFile {
    param(
        [string]$Path,
        [string]$Label,
        [datetime]$NotOlderThan
    )
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "$Label missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path
    if ($item.Length -le 0) {
        throw "$Label is empty: $Path"
    }
    if ($item.LastWriteTime -lt $NotOlderThan) {
        throw ("{0} is stale: {1}; lastWrite={2:o}; requiredAfter={3:o}" -f `
                $Label, $Path, $item.LastWriteTime, $NotOlderThan)
    }
}

function Get-FileEvidence {
    param([string]$Path)
    $item = Get-Item -LiteralPath $Path
    $sha = Get-FileHash -LiteralPath $Path -Algorithm SHA256
    return [ordered]@{
        path = $Path
        bytes = $item.Length
        lastWriteUtc = $item.LastWriteTimeUtc.ToString('o')
        sha256 = $sha.Hash
    }
}

function Get-SampleGateCommand {
    param(
        [int]$Scale,
        [int]$Threads,
        [string[]]$Samples,
        [string]$OutputDir,
        [string]$BaselineRoot
    )

    $baselineArg = if ([string]::IsNullOrWhiteSpace($BaselineRoot)) { '' } else { " -BaselineRoot `"$BaselineRoot`"" }
    $cleanArg = if ($CleanSampleOutput) { ' -CleanSampleOutput' } else { '' }
    $ecologyArg = if ($UseEcologyQualityGate) { ' -UseEcologyQualityGate' } else { '' }
    return ".\scripts\run-quality-acceptance-samples.ps1 -Heightmap `"$Heightmap`" -OutputRoot `"$OutputDir`" -Scale $Scale -Threads $Threads -CacheRows $CacheRows -PrefetchRows $PrefetchRows -VerticalScale $VerticalScale -TextureMode $TextureMode -Samples $($Samples -join ',')$baselineArg$cleanArg$ecologyArg"
}

function Get-StageReplayCommand {
    param([string]$Stage)

    switch ($Stage) {
        'samples-5000' {
            return ".\scripts\run-nation-war-acceptance-gate.ps1 -Stages samples-5000 -CacheRows $CacheRows -PrefetchRows $PrefetchRows -VerticalScale $VerticalScale -TextureMode $TextureMode$(if ($CleanSampleOutput) { ' -CleanSampleOutput' } else { '' })$(if ($UseEcologyQualityGate) { ' -UseEcologyQualityGate' } else { '' })"
        }
        'samples-1000' {
            return ".\scripts\run-nation-war-acceptance-gate.ps1 -Stages samples-1000 -CacheRows $CacheRows -PrefetchRows $PrefetchRows -VerticalScale $VerticalScale -TextureMode $TextureMode$(if ($CleanSampleOutput) { ' -CleanSampleOutput' } else { '' })$(if ($UseEcologyQualityGate) { ' -UseEcologyQualityGate' } else { '' })"
        }
        'parity-existing' {
            return '.\scripts\run-nation-war-acceptance-gate.ps1 -Stages parity-existing -ParitySourcePng <source.png> -ParityExpectedPng <expected.png> -ParityCurrentPng <current.png> -ParityMaskPng <mask.png>'
        }
        'server-finalization' {
            return Get-ServerFinalizationStageCommand
        }
        default {
            return ".\scripts\run-nation-war-acceptance-gate.ps1 -Stages $Stage"
        }
    }
}

function Get-ServerFinalizationStageCommand {
    $formatArg = if ($ServerRegionFormat -eq 'auto') { '' } else { " -ServerRegionFormat $ServerRegionFormat" }
    $rconArg = " -RconHost $RconHost -RconPort $RconPort"
    return (".\scripts\run-nation-war-acceptance-gate.ps1 -Stages server-finalization -ServerWorldDir `"{0}`"{1} -ServerStartRegionX {2} -ServerStartRegionZ {3} -ServerRegionCols {4} -ServerRegionRows {5} -ServerMaxWindows {6} -ServerWaitSeconds {7}{8}" -f `
            $ServerWorldDir, $formatArg, $ServerStartRegionX, $ServerStartRegionZ, $ServerRegionCols,
            $ServerRegionRows, $ServerMaxWindows, $ServerWaitSeconds, $rconArg)
}

function Get-NextStageCommand {
    param([string]$Stage)

    switch ($Stage) {
        'samples-5000' {
            return ".\scripts\run-nation-war-acceptance-gate.ps1 -Stages samples-1000 -CacheRows $CacheRows -PrefetchRows $PrefetchRows -VerticalScale $VerticalScale -TextureMode $TextureMode$(if ($CleanSampleOutput) { ' -CleanSampleOutput' } else { '' })$(if ($UseEcologyQualityGate) { ' -UseEcologyQualityGate' } else { '' })"
        }
        'samples-1000' {
            return '.\scripts\run-nation-war-acceptance-gate.ps1 -Stages parity-existing -ParitySourcePng <candidate-source.png> -ParityExpectedPng <candidate-expected.png> -ParityCurrentPng <candidate-current.png> -ParityMaskPng <candidate-mask.png>'
        }
        'parity-existing' {
            return Get-ServerFinalizationStageCommand
        }
        default {
            return ''
        }
    }
}

function Write-CommandPlan {
    $out5000 = Join-Path $OutputRoot 'quality-acceptance-samples-5000'
    $out1000 = Join-Path $OutputRoot 'quality-acceptance-samples-1000'
    Write-Host 'nationWarAcceptanceGate.plan=evidence=false'
    Write-Host 'nationWarAcceptanceGate.plan.parallelSampleGroups=not-enabled,reason=sample outputs are isolated but shared build/classpath and heavyweight Java generation concurrency are not proven safe for default go/no-go evidence'
    Write-Host ''
    Write-Host '1:5000 representative sample gate:'
    Write-Host (Get-SampleGateCommand -Scale 5000 -Threads $Threads5000 -Samples $samples5000 -OutputDir $out5000 -BaselineRoot $BaselineRoot5000)
    Write-Host ''
    Write-Host '1:1000 representative sample gate:'
    Write-Host (Get-SampleGateCommand -Scale 1000 -Threads $Threads1000 -Samples $samples1000 -OutputDir $out1000 -BaselineRoot $BaselineRoot1000)
    Write-Host ''
    Write-Host 'Candidate same-build direct-MCA parity gate:'
    Write-Host '.\scripts\run-nation-war-acceptance-gate.ps1 -Stages parity-existing -ParitySourcePng <candidate-source.png> -ParityExpectedPng <candidate-expected.png> -ParityCurrentPng <candidate-current.png> -ParityMaskPng <candidate-mask.png>'
    Write-Host ''
    Write-Host 'Server finalization smoke gate:'
    Write-Host (Get-ServerFinalizationStageCommand)
    Write-Host 'For a one-region generated validation world, use -ServerRegionCols 1 -ServerRegionRows 1 -ServerMaxWindows 4 so the full region is finalized and scanned.'
}

function Invoke-SampleGate {
    param(
        [int]$Scale,
        [int]$Threads,
        [string[]]$Samples,
        [string]$OutputDir,
        [string]$BaselineRoot
    )

    $sampleParams = @{
        Heightmap = $Heightmap
        OutputRoot = $OutputDir
        Scale = $Scale
        Threads = $Threads
        CacheRows = $CacheRows
        PrefetchRows = $PrefetchRows
        VerticalScale = $VerticalScale
        TextureMode = $TextureMode
        Samples = $Samples
    }
    if (-not [string]::IsNullOrWhiteSpace($BaselineRoot)) {
        $sampleParams.BaselineRoot = $BaselineRoot
    }
    if ($CleanSampleOutput) {
        $sampleParams.CleanSampleOutput = $true
    }
    if ($UseEcologyQualityGate) {
        $sampleParams.UseEcologyQualityGate = $true
    }

    $sampleStartedAt = Get-Date
    Write-Host ("nationWarAcceptanceGate.sampleGate.start=scale=1:{0},samples={1},threads={2},output={3}" -f `
            $Scale, $Samples.Count, $Threads, $OutputDir)
    Write-Host "nationWarAcceptanceGate.sampleGate.command=$(Get-SampleGateCommand -Scale $Scale -Threads $Threads -Samples $Samples -OutputDir $OutputDir -BaselineRoot $BaselineRoot)"
    $sampleGateLog = Join-Path $OutputDir "sample-gate.log"
    Write-Host "nationWarAcceptanceGate.sampleGate.log=$sampleGateLog"
    $sampleResult = Invoke-LoggedCommand -LogPath $sampleGateLog -Command {
        & $sampleScript @sampleParams
    }
    if ($sampleResult.ExitCode -ne 0) {
        Write-Host "nationWarAcceptanceGate.sampleGate.failureHint=$($sampleResult.FailureHint)"
        Write-SampleGateFailureDetails -LogPath $sampleGateLog -OutputDir $OutputDir
        throw ("Representative sample gate failed for scale 1:{0}; exitCode={1}; hint={2}; log={3}" -f `
                $Scale, $sampleResult.ExitCode, $sampleResult.FailureHint, $sampleResult.LogPath)
    }
    Write-Host ("nationWarAcceptanceGate.sampleGate.end=pass,scale=1:{0},elapsed={1},output={2}" -f `
            $Scale, (Format-Elapsed ((Get-Date) - $sampleStartedAt)), $OutputDir)
}

function Read-MetricSection {
    param(
        [string]$MetricsPath,
        [string]$RequiredSectionName = 'current-vs-expected'
    )

    if (-not (Test-Path -LiteralPath $MetricsPath)) {
        throw "Parity metrics file missing: $MetricsPath"
    }
    $active = $false
    $foundSection = $false
    $values = @{}
    foreach ($line in Get-Content -LiteralPath $MetricsPath) {
        if ($line -match '^\[(.+)\]$') {
            $active = $Matches[1] -eq $RequiredSectionName
            if ($active) {
                $foundSection = $true
            }
            continue
        }
        if (-not $active) {
            continue
        }
        if ($line -match '^([^=]+)=([0-9.+-]+)(?: \(([0-9.+-]+)%\))?') {
            $key = $Matches[1]
            if ($Matches.Count -ge 4 -and $Matches[3]) {
                $values["$key.percent"] = [double]$Matches[3]
            } else {
                $values[$key] = [double]$Matches[2]
            }
        }
    }
    if (-not $foundSection) {
        throw "Parity metrics section missing: [$RequiredSectionName]. Refusing fallback metrics to avoid stale evidence."
    }
    return $values
}

function Assert-ParityMetric {
    param(
        [hashtable]$Values,
        [string]$Key,
        [double]$Limit,
        [ValidateSet('max', 'min')]
        [string]$Mode
    )

    if (-not $Values.ContainsKey($Key)) {
        throw "Parity metric missing: $Key"
    }
    $actual = [double]$Values[$Key]
    if ($Mode -eq 'max' -and $actual -gt $Limit) {
        throw ("Parity gate failed: {0}={1:N6} > {2:N6}" -f $Key, $actual, $Limit)
    }
    if ($Mode -eq 'min' -and $actual -lt $Limit) {
        throw ("Parity gate failed: {0}={1:N6} < {2:N6}" -f $Key, $actual, $Limit)
    }
    Write-Host ("parityGate.{0}=pass,actual={1:N6},limit={2:N6},mode={3}" -f $Key, $actual, $Limit, $Mode)
}

function Invoke-ParityGate {
    $parityStartedAt = Get-Date
    $freshAfter = $parityStartedAt.AddSeconds(-2)
    if ($UseLatestKnownParityEvidence) {
        throw 'UseLatestKnownParityEvidence is stale regression evidence and is disabled for go/no-go. Pass candidate source/expected/current/mask PNG paths.'
    }

    foreach ($path in @($ParitySourcePng, $ParityExpectedPng, $ParityCurrentPng, $ParityMaskPng)) {
        if ([string]::IsNullOrWhiteSpace($path) -or -not (Test-Path -LiteralPath $path)) {
            throw "Parity gate requires existing source, expected, current, and mask PNG paths. Missing: $path"
        }
        $item = Get-Item -LiteralPath $path
        if ($item.Length -le 0) {
            throw "Parity gate input PNG is empty: $path"
        }
    }
    $parityOutputDirEffective = $ParityOutputDir
    if ([string]::IsNullOrWhiteSpace($parityOutputDirEffective)) {
        $parityOutputDirEffective = Join-Path $OutputRoot 'photo-parity-gate'
    }
    New-Item -ItemType Directory -Force -Path $parityOutputDirEffective | Out-Null
    $metricsPath = Join-Path $parityOutputDirEffective 'metrics.txt'
    $metricLog = Join-Path $parityOutputDirEffective 'photo-parity-metric.log'
    if (Test-Path -LiteralPath $metricsPath) {
        Remove-Item -LiteralPath $metricsPath -Force
    }

    Write-Host ("nationWarAcceptanceGate.parity.start=output={0},useLatestKnownEvidence={1}" -f `
            $parityOutputDirEffective, [bool]$UseLatestKnownParityEvidence)
    foreach ($entry in @(
            @{ label = 'source'; path = $ParitySourcePng },
            @{ label = 'expected'; path = $ParityExpectedPng },
            @{ label = 'current'; path = $ParityCurrentPng },
            @{ label = 'mask'; path = $ParityMaskPng }
        )) {
        $item = Get-Item -LiteralPath $entry['path']
        Write-Host ("parityGate.input.{0}=path={1},bytes={2},lastWrite={3:o}" -f `
                $entry['label'], $entry['path'], $item.Length, $item.LastWriteTime)
    }
    Write-Host "nationWarAcceptanceGate.parity.log=$metricLog"
    $metricResult = Invoke-LoggedCommand -LogPath $metricLog -Command {
        & $metricScript `
            -ProjectRoot $repoRoot `
            -SourcePng $ParitySourcePng `
            -ExpectedPng $ParityExpectedPng `
            -CurrentSurfacePng $ParityCurrentPng `
            -OutputDir $parityOutputDirEffective `
            -CropX 0 `
            -CropY 0 `
            -CropWidth 512 `
            -CropHeight 512 `
            -MaskPng $ParityMaskPng `
            -MaskMode nonzero
    }
    if ($metricResult.ExitCode -ne 0) {
        Write-Host "nationWarAcceptanceGate.parity.failureHint=$($metricResult.FailureHint)"
        throw ("photo parity metric command failed; exitCode={0}; hint={1}; log={2}" -f `
                $metricResult.ExitCode, $metricResult.FailureHint, $metricResult.LogPath)
    }

    Assert-FreshFile -Path $metricsPath -Label 'parity metrics file' -NotOlderThan $freshAfter
    $metrics = Read-MetricSection -MetricsPath $metricsPath -RequiredSectionName 'current-vs-expected'
    Assert-ParityMetric -Values $metrics -Key 'meanDeltaE2000' -Limit $MaxParityMeanDeltaE2000 -Mode max
    Assert-ParityMetric -Values $metrics -Key 'p95DeltaE2000' -Limit $MaxParityP95DeltaE2000 -Mode max
    Assert-ParityMetric -Values $metrics -Key 'globalLumaSsim' -Limit $MinParityGlobalLumaSsim -Mode min
    Assert-ParityMetric -Values $metrics -Key 'deltaEOver10.percent' -Limit $MaxParityDeltaEOver10Percent -Mode max
    Assert-ParityMetric -Values $metrics -Key 'deltaEOver20.percent' -Limit $MaxParityDeltaEOver20Percent -Mode max
    Assert-ParityMetric -Values $metrics -Key 'deltaEOver30.percent' -Limit $MaxParityDeltaEOver30Percent -Mode max
    $evidence = [ordered]@{
        schemaVersion = 1
        generatedAt = (Get-Date).ToString('o')
        stage = 'parity-existing'
        metricsSection = 'current-vs-expected'
        outputDir = $parityOutputDirEffective
        metricLog = $metricLog
        metricsPath = $metricsPath
        sourcePng = Get-FileEvidence -Path $ParitySourcePng
        expectedPng = Get-FileEvidence -Path $ParityExpectedPng
        currentPng = Get-FileEvidence -Path $ParityCurrentPng
        maskPng = Get-FileEvidence -Path $ParityMaskPng
        thresholds = [ordered]@{
            maxMeanDeltaE2000 = $MaxParityMeanDeltaE2000
            maxP95DeltaE2000 = $MaxParityP95DeltaE2000
            minGlobalLumaSsim = $MinParityGlobalLumaSsim
            maxDeltaEOver10Percent = $MaxParityDeltaEOver10Percent
            maxDeltaEOver20Percent = $MaxParityDeltaEOver20Percent
            maxDeltaEOver30Percent = $MaxParityDeltaEOver30Percent
        }
    }
    $evidencePath = Join-Path $parityOutputDirEffective 'parity-evidence.json'
    ($evidence | ConvertTo-Json -Depth 5) | Set-Content -LiteralPath $evidencePath -Encoding UTF8
    Write-Host "parityGate.metrics=$metricsPath"
    Write-Host "parityGate.evidence=$evidencePath"
    Write-Host ("nationWarAcceptanceGate.parity.end=pass,elapsed={0},metrics={1}" -f `
            (Format-Elapsed ((Get-Date) - $parityStartedAt)), $metricsPath)
}

function Invoke-ServerFinalizationGate {
    $serverStartedAt = Get-Date
    $serverLog = Join-Path $OutputRoot 'server-finalization-gate.log'
    Write-Host ("nationWarAcceptanceGate.serverFinalization.start=world={0},regions={1},{2}+{3}x{4},maxWindows={5},waitSeconds={6}" -f `
            $ServerWorldDir, $ServerStartRegionX, $ServerStartRegionZ, $ServerRegionCols, $ServerRegionRows, $ServerMaxWindows, $ServerWaitSeconds)
    Write-Host "nationWarAcceptanceGate.serverFinalization.log=$serverLog"
    $serverResult = Invoke-LoggedCommand -LogPath $serverLog -Command {
        & $finalizationScript `
            -WorldDir $ServerWorldDir `
            -RegionFormat $ServerRegionFormat `
            -StartRegionX $ServerStartRegionX `
            -StartRegionZ $ServerStartRegionZ `
            -RegionCols $ServerRegionCols `
            -RegionRows $ServerRegionRows `
            -WindowChunks 16 `
            -MaxWindows $ServerMaxWindows `
            -WaitSeconds $ServerWaitSeconds `
            -MinFullRatioAfter 0.98 `
            -RequireDelegatedStatusBefore `
            -RejectUnexpectedStatusBefore `
            -RunPostFinalIntegrity `
            -RunSurvivalPaletteValidation `
            -MaxUnderwaterAirColumnRatio 0.0 `
            -MinTreeLeafColumnRatio 0.20 `
            -MaxCoastLandAboveSeaGt8Samples 0 `
            -MaxCoastLandAboveSeaGt16Samples 0 `
            -MaxCoastLandAboveSeaDelta 32 `
            -MaxCoastFloorDelta 32 `
            -RconHost $RconHost `
            -RconPort $RconPort `
            -RconPassword $RconPassword
    }
    if ($serverResult.ExitCode -ne 0) {
        Write-Host "nationWarAcceptanceGate.serverFinalization.failureHint=$($serverResult.FailureHint)"
        throw ("Server finalization gate failed; exitCode={0}; hint={1}; log={2}" -f `
                $serverResult.ExitCode, $serverResult.FailureHint, $serverResult.LogPath)
    }
    Write-Host ("nationWarAcceptanceGate.serverFinalization.end=pass,elapsed={0},world={1}" -f `
            (Format-Elapsed ((Get-Date) - $serverStartedAt)), $ServerWorldDir)
}

$gateStartedAt = Get-Date
$stageFailures = New-Object System.Collections.Generic.List[object]
$completedStages = 0
$completedEvidenceStages = 0
New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
Write-Host ("nationWarAcceptanceGate.start=stages={0},outputRoot={1}" -f ($Stages -join ','), $OutputRoot)

for ($stageIndex = 0; $stageIndex -lt $Stages.Count; $stageIndex++) {
    $stage = $Stages[$stageIndex]
    $stageStartedAt = Get-Date
    $isPlanStage = $stage -eq 'plan'
    Write-Host ("nationWarAcceptanceGate.stage.start=[{0}/{1}],stage={2}" -f ($stageIndex + 1), $Stages.Count, $stage)
    Write-Host "nationWarAcceptanceGate.stage.command=$(Get-StageReplayCommand -Stage $stage)"
    try {
        switch ($stage) {
            'plan' {
                Write-CommandPlan
            }
            'samples-5000' {
                Invoke-SampleGate -Scale 5000 -Threads $Threads5000 -Samples $samples5000 `
                    -OutputDir (Join-Path $OutputRoot 'quality-acceptance-samples-5000') `
                    -BaselineRoot $BaselineRoot5000
            }
            'samples-1000' {
                Invoke-SampleGate -Scale 1000 -Threads $Threads1000 -Samples $samples1000 `
                    -OutputDir (Join-Path $OutputRoot 'quality-acceptance-samples-1000') `
                    -BaselineRoot $BaselineRoot1000
            }
            'parity-existing' {
                Invoke-ParityGate
            }
            'server-finalization' {
                Invoke-ServerFinalizationGate
            }
        }
        $completedStages++
        if ($isPlanStage) {
            Write-Host ("nationWarAcceptanceGate.stage.end=plan,index={0}/{1},stage={2},elapsed={3},evidence=false" -f `
                    ($stageIndex + 1), $Stages.Count, $stage, (Format-Elapsed ((Get-Date) - $stageStartedAt)))
        } else {
            $completedEvidenceStages++
            Write-Host ("nationWarAcceptanceGate.stage.end=pass,index={0}/{1},stage={2},elapsed={3}" -f `
                    ($stageIndex + 1), $Stages.Count, $stage, (Format-Elapsed ((Get-Date) - $stageStartedAt)))
            $nextCommand = Get-NextStageCommand -Stage $stage
            if (-not [string]::IsNullOrWhiteSpace($nextCommand)) {
                Write-Host "nationWarAcceptanceGate.nextCommand=$nextCommand"
            }
        }
    } catch {
        $failure = [pscustomobject]@{
            Stage = $stage
            Message = $_.Exception.Message
            Elapsed = Format-Elapsed ((Get-Date) - $stageStartedAt)
        }
        [void]$stageFailures.Add($failure)
        Write-Host ("nationWarAcceptanceGate.stage.end=fail,index={0}/{1},stage={2},elapsed={3}" -f `
                ($stageIndex + 1), $Stages.Count, $stage, $failure.Elapsed)
        Write-Host "nationWarAcceptanceGate.failure.stage=$($failure.Stage),message=$($failure.Message)"
        Write-Host "nationWarAcceptanceGate.nextCommand.retry=$(Get-StageReplayCommand -Stage $stage)"
        break
    }
}

$gateElapsed = Format-Elapsed ((Get-Date) - $gateStartedAt)
if ($stageFailures.Count -gt 0) {
    Write-Host ("nationWarAcceptanceGate.summary=fail,completed={0},failed={1},total={2},elapsed={3},outputRoot={4}" -f `
            $completedStages, $stageFailures.Count, $Stages.Count, $gateElapsed, $OutputRoot)
    foreach ($failure in $stageFailures) {
        Write-Host ("nationWarAcceptanceGate.failure=stage={0},elapsed={1},message={2}" -f `
                $failure.Stage, $failure.Elapsed, $failure.Message)
    }
    throw "nation-war acceptance gate failed; failures=$($stageFailures.Count)"
}

if ($completedEvidenceStages -eq 0) {
    Write-Host ("nationWarAcceptanceGate.summary=plan,completed={0},evidenceCompleted=0,failed=0,total={1},elapsed={2},outputRoot={3},evidence=false" -f `
            $completedStages, $Stages.Count, $gateElapsed, $OutputRoot)
} else {
    Write-Host ("nationWarAcceptanceGate.summary=pass,completed={0},evidenceCompleted={1},failed=0,total={2},elapsed={3},outputRoot={4}" -f `
            $completedStages, $completedEvidenceStages, $Stages.Count, $gateElapsed, $OutputRoot)
}
