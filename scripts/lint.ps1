param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
)

$ErrorActionPreference = 'Stop'

$files = Get-ChildItem -Path $ProjectRoot -Recurse -File |
    Where-Object {
        $_.FullName -notmatch '\\build\\' -and
        $_.Extension -in '.rs', '.toml', '.md', '.json', '.ps1', '.mjs'
    }

$failures = New-Object System.Collections.Generic.List[string]
foreach ($file in $files) {
    $lineNo = 0
    foreach ($line in Get-Content -Path $file.FullName) {
        $lineNo++
        if ($line -match "`t") {
            $failures.Add("$($file.FullName):$lineNo contains a tab")
        }
        if ($line -match '\s+$') {
            $failures.Add("$($file.FullName):$lineNo has trailing whitespace")
        }
    }
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Error $_ }
    throw "Lint failed with $($failures.Count) issue(s)."
}

Write-Output "Lint passed"
