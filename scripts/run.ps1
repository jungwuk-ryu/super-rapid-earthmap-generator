[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CliArgs
)

$ErrorActionPreference = 'Stop'

& (Join-Path $PSScriptRoot 'build.ps1') -ProjectRoot $ProjectRoot | Out-Null
$classesDir = Join-Path $ProjectRoot 'build\classes'
$mainOut = Join-Path $classesDir 'main'
$lockPath = Join-Path $classesDir 'compile.lock'
$runtimeOut = Join-Path $classesDir ("run-main-{0}-{1}" -f $PID, [Guid]::NewGuid().ToString('N'))
$vendorLib = Join-Path $ProjectRoot 'vendor\lib'

function Enter-CompileLock {
    param([string]$Path)
    for ($i = 0; $i -lt 240; $i++) {
        try {
            return [System.IO.File]::Open($Path, [System.IO.FileMode]::OpenOrCreate,
                [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
        } catch [System.IO.IOException] {
            Start-Sleep -Milliseconds 250
        }
    }
    throw "Timed out waiting for compile lock: $Path"
}

$lockStream = $null
try {
    $lockStream = Enter-CompileLock -Path $lockPath
    Copy-Item -LiteralPath $mainOut -Destination $runtimeOut -Recurse -Force
} finally {
    if ($lockStream -ne $null) {
        $lockStream.Dispose()
    }
}

try {
    $classpathEntries = @($runtimeOut)
    if (Test-Path -LiteralPath $vendorLib) {
        $classpathEntries += Get-ChildItem -LiteralPath $vendorLib -Filter '*.jar' | Sort-Object FullName | ForEach-Object {
            $_.FullName
        }
    }
    java "--enable-native-access=ALL-UNNAMED" -cp ($classpathEntries -join [IO.Path]::PathSeparator) net.earthmap.cli.EarthMapCli @CliArgs
    if ($LASTEXITCODE -ne 0) {
        throw "CLI failed with exit code $LASTEXITCODE."
    }
} finally {
    if (Test-Path -LiteralPath $runtimeOut) {
        Remove-Item -LiteralPath $runtimeOut -Recurse -Force -ErrorAction SilentlyContinue
    }
}
