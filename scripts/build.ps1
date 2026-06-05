[CmdletBinding()]
param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [switch]$Release
)

$ErrorActionPreference = 'Stop'

$rustRoot = Join-Path $ProjectRoot 'rust'
$rustBuild = Join-Path (Join-Path $rustRoot 'scripts') 'build.ps1'
if (!(Test-Path -LiteralPath $rustBuild)) {
    throw "Rust build wrapper not found: $rustBuild"
}

Write-Host ("earthmap.build=rust,path={0},release={1}" -f $rustBuild, [bool]$Release)
& $rustBuild -RustRoot $rustRoot -Release:$Release
exit $LASTEXITCODE
