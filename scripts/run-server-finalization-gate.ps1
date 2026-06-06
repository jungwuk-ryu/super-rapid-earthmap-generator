param(
    [string]$WorldDir = $env:EARTHMAP_SERVER_WORLD_DIR,
    [int]$RegionX = 0,
    [int]$RegionZ = -1,
    [int]$StartLocalChunkX = 0,
    [int]$StartLocalChunkZ = 0,
    [ValidateRange(1, 16)]
    [int]$ChunkWidth = 16,
    [ValidateRange(1, 16)]
    [int]$ChunkHeight = 16,
    [int]$WaitSeconds = 30,
    [string]$RconHost = '127.0.0.1',
    [int]$RconPort = 25575,
    [string]$RconPassword = 'earthmap-codex-rcon',
    [string]$ReportPath = ''
)

$ErrorActionPreference = 'Stop'

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$rustRoot = Join-Path $repoRoot 'rust'
$runScript = Join-Path (Join-Path $rustRoot 'scripts') 'run.ps1'
$rconScript = Join-Path $repoRoot 'scripts\send-rcon-command.mjs'

if ([string]::IsNullOrWhiteSpace($WorldDir)) {
    throw "WorldDir is required. Pass -WorldDir or set EARTHMAP_SERVER_WORLD_DIR."
}

$regionFile = Join-Path $WorldDir ("region\r.{0}.{1}.mca" -f $RegionX, $RegionZ)

if (!(Test-Path $regionFile)) {
    throw "Region file not found: $regionFile"
}
if (($ChunkWidth * $ChunkHeight) -gt 256) {
    throw "Minecraft forceload command allows at most 256 chunks per area."
}
if ($StartLocalChunkX -lt 0 -or $StartLocalChunkX -ge 32 -or
        $StartLocalChunkZ -lt 0 -or $StartLocalChunkZ -ge 32) {
    throw "Start local chunk coordinates must be in 0..31."
}
if (($StartLocalChunkX + $ChunkWidth) -gt 32 -or ($StartLocalChunkZ + $ChunkHeight) -gt 32) {
    throw "Requested chunk window must stay inside one MCA region."
}

function Read-StatusHistogram {
    param([string]$Path)

    Write-Host ("earthmap.command={0} {1}" -f $runScript, "inspect-mca-statuses $Path")
    $lines = & $runScript -RustRoot $rustRoot inspect-mca-statuses $Path
    if ($LASTEXITCODE -ne 0) {
        throw "inspect-mca-statuses failed for $Path"
    }
    $counts = [ordered]@{}
    $decoded = 0
    foreach ($line in $lines) {
        if ($line -match '^decodedChunkCount=(\d+)$') {
            $decoded = [int]$Matches[1]
        } elseif ($line -match '^status\.(.+)=(\d+)$') {
            $counts[$Matches[1]] = [int]$Matches[2]
        }
    }
    [PSCustomObject]@{
        decodedChunkCount = $decoded
        statusCounts = $counts
        raw = $lines
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

$globalChunkX0 = ($RegionX * 32) + $StartLocalChunkX
$globalChunkZ0 = ($RegionZ * 32) + $StartLocalChunkZ
$globalChunkX1 = $globalChunkX0 + $ChunkWidth - 1
$globalChunkZ1 = $globalChunkZ0 + $ChunkHeight - 1
$minBlockX = $globalChunkX0 * 16
$minBlockZ = $globalChunkZ0 * 16
$maxBlockX = (($globalChunkX1 + 1) * 16) - 1
$maxBlockZ = (($globalChunkZ1 + 1) * 16) - 1

$before = Read-StatusHistogram $regionFile
$startedAt = Get-Date
$forceAdd = "forceload add $minBlockX $minBlockZ $maxBlockX $maxBlockZ"
$forceRemove = "forceload remove $minBlockX $minBlockZ $maxBlockX $maxBlockZ"

$addOutput = Invoke-Rcon $forceAdd
try {
    if ($WaitSeconds -gt 0) {
        Start-Sleep -Seconds $WaitSeconds
    }
    $saveOutput = Invoke-Rcon 'save-all flush'
    $after = Read-StatusHistogram $regionFile
} finally {
    $removeOutput = Invoke-Rcon $forceRemove
}

$report = [PSCustomObject]@{
    schemaVersion = 1
    generatedAt = (Get-Date).ToString('o')
    regionFile = $regionFile
    regionX = $RegionX
    regionZ = $RegionZ
    chunkWindow = [PSCustomObject]@{
        startLocalChunkX = $StartLocalChunkX
        startLocalChunkZ = $StartLocalChunkZ
        chunkWidth = $ChunkWidth
        chunkHeight = $ChunkHeight
        globalChunkX0 = $globalChunkX0
        globalChunkZ0 = $globalChunkZ0
        globalChunkX1 = $globalChunkX1
        globalChunkZ1 = $globalChunkZ1
        minBlockX = $minBlockX
        minBlockZ = $minBlockZ
        maxBlockX = $maxBlockX
        maxBlockZ = $maxBlockZ
    }
    waitSeconds = $WaitSeconds
    startedAt = $startedAt.ToString('o')
    commands = [PSCustomObject]@{
        forceAdd = $forceAdd
        save = 'save-all flush'
        forceRemove = $forceRemove
    }
    rconOutput = [PSCustomObject]@{
        forceAdd = $addOutput
        save = $saveOutput
        forceRemove = $removeOutput
    }
    before = $before
    after = $after
}

if ([string]::IsNullOrWhiteSpace($ReportPath)) {
    $safeRegion = "r.$RegionX.$RegionZ"
    $ReportPath = Join-Path $WorldDir ("earthmap-server-finalization-$safeRegion.json")
}

$reportJson = $report | ConvertTo-Json -Depth 12
$reportJson | Set-Content -Encoding UTF8 $ReportPath

Write-Output $reportJson
Write-Host "server-finalization-report=$ReportPath"
