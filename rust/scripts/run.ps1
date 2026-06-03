[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$RustRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CliArgs
)

$ErrorActionPreference = 'Stop'

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (!$cargo) {
    throw "Cargo was not found on PATH. Install the Rust toolchain before running the Rust port CLI."
}

& (Join-Path $PSScriptRoot 'build.ps1') -RustRoot $RustRoot | Out-Null

$debugDir = Join-Path $RustRoot 'target\debug'
$builtExe = Join-Path $debugDir 'earthmap-rs.exe'
if (!(Test-Path -LiteralPath $builtExe)) {
    $builtExe = Join-Path $debugDir 'earthmap-rs'
}
if (!(Test-Path -LiteralPath $builtExe)) {
    throw "Built Rust CLI was not found under $debugDir."
}

$runtimeDir = Join-Path (Join-Path $RustRoot 'target') ("run-earthmap-rs-{0}-{1}" -f $PID, [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $runtimeDir | Out-Null
$runtimeExe = Join-Path $runtimeDir (Split-Path -Leaf $builtExe)
$exitCode = 0
$nativeCommandPreference = Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue
$restoreNativeCommandPreference = $false
if ($nativeCommandPreference -ne $null) {
    $previousNativeCommandPreference = $PSNativeCommandUseErrorActionPreference
    $PSNativeCommandUseErrorActionPreference = $false
    $restoreNativeCommandPreference = $true
}
try {
    Copy-Item -LiteralPath $builtExe -Destination $runtimeExe -Force
    & $runtimeExe @CliArgs
    $exitCode = $LASTEXITCODE
} finally {
    if ($restoreNativeCommandPreference) {
        $PSNativeCommandUseErrorActionPreference = $previousNativeCommandPreference
    }
    if (Test-Path -LiteralPath $runtimeDir) {
        Remove-Item -LiteralPath $runtimeDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

exit $exitCode
