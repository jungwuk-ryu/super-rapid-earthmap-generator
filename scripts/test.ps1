[CmdletBinding()]
param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [string]$Filter = '',
    [switch]$NoBuild,
    [switch]$List,
    [switch]$Isolated
)

$ErrorActionPreference = 'Stop'

$rustRoot = Join-Path $ProjectRoot 'rust'
$rustTest = Join-Path (Join-Path $rustRoot 'scripts') 'test.ps1'
if (!(Test-Path -LiteralPath $rustTest)) {
    throw "Rust test wrapper not found: $rustTest"
}

Write-Host ("earthmap.test=rust,path={0},filter={1},list={2},isolated={3}" -f `
        $rustTest, $Filter, [bool]$List, [bool]$Isolated)
& $rustTest -RustRoot $rustRoot -Filter $Filter -NoBuild:$NoBuild -List:$List -Isolated:$Isolated
exit $LASTEXITCODE
