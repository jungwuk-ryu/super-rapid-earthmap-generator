[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CliArgs
)

$ErrorActionPreference = 'Stop'

$rustRoot = Join-Path $ProjectRoot 'rust'
$rustRun = Join-Path (Join-Path $rustRoot 'scripts') 'run.ps1'
if (!(Test-Path -LiteralPath $rustRun)) {
    throw "Rust run wrapper not found: $rustRun"
}

Write-Host ("earthmap.wrapper=rust,path={0},args={1}" -f $rustRun, ($CliArgs -join ' '))
& $rustRun -RustRoot $rustRoot @CliArgs
exit $LASTEXITCODE
