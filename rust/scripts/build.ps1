[CmdletBinding()]
param(
    [string]$RustRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [switch]$Release
)

$ErrorActionPreference = 'Stop'

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (!$cargo) {
    throw "Cargo was not found on PATH. Install the Rust toolchain before running the Rust port build."
}

$manifest = Join-Path $RustRoot 'Cargo.toml'
$cargoArgs = @('build', '--manifest-path', $manifest, '--workspace', '--locked')
if ($Release) {
    $cargoArgs += '--release'
}

& $cargo.Source @cargoArgs
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE."
}

Write-Output "Rust build passed: $(Join-Path $RustRoot 'target')"
