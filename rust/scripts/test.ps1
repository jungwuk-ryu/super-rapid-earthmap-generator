[CmdletBinding()]
param(
    [string]$RustRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [string]$Filter = '',
    [switch]$NoBuild,
    [switch]$List,
    [switch]$Isolated
)

$ErrorActionPreference = 'Stop'

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (!$cargo) {
    throw "Cargo was not found on PATH. Install the Rust toolchain before running Rust port tests."
}

if (!$NoBuild -and !$List) {
    & (Join-Path $PSScriptRoot 'build.ps1') -RustRoot $RustRoot | Out-Null
}

$manifest = Join-Path $RustRoot 'Cargo.toml'

if ($List -and $Isolated) {
    throw "-List and -Isolated cannot be used together."
}

if ($Isolated) {
    $buildArgs = @('test', '--manifest-path', $manifest, '--workspace', '--no-run', '--message-format', 'json')
    if ($Filter -ne '') {
        $buildArgs += $Filter
    }

    $jsonLines = & $cargo.Source @buildArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo test --no-run failed with exit code $LASTEXITCODE."
    }

    $testExecutables = @()
    foreach ($line in $jsonLines) {
        if ($line.Trim() -eq '') {
            continue
        }
        try {
            $message = $line | ConvertFrom-Json
            if ($message.reason -eq 'compiler-artifact' -and $message.profile.test -eq $true -and $message.executable) {
                $testExecutables += $message.executable
            }
        } catch {
            # Cargo may print non-JSON status lines from subprocesses; ignore them here.
        }
    }

    $testExecutables = @($testExecutables | Sort-Object -Unique)
    if ($testExecutables.Count -eq 0) {
        throw "No Rust test executables were produced."
    }

    foreach ($testExecutable in $testExecutables) {
        $listArgs = @()
        if ($Filter -ne '') {
            $listArgs += $Filter
        }
        $listArgs += @('--list', '--format', 'terse')

        $listedTests = & $testExecutable @listArgs
        if ($LASTEXITCODE -ne 0) {
            throw "Rust isolated test listing failed with exit code ${LASTEXITCODE}: $testExecutable"
        }

        $testNames = @()
        foreach ($listedTest in $listedTests) {
            $line = $listedTest.Trim()
            if ($line -match '^(.*):\s+test$') {
                $testNames += $Matches[1]
            }
        }

        if ($testNames.Count -eq 0) {
            Write-Output "No tests listed for $testExecutable"
            continue
        }

        foreach ($testName in $testNames) {
            Write-Output "Running $testExecutable $testName"
            & $testExecutable $testName --exact --test-threads 1
            if ($LASTEXITCODE -ne 0) {
                throw "Rust isolated test failed with exit code ${LASTEXITCODE}: $testExecutable $testName"
            }
        }
    }

    Write-Output "Rust isolated tests passed"
    return
}

$cargoArgs = @('test', '--manifest-path', $manifest, '--workspace')
if ($Filter -ne '') {
    $cargoArgs += $Filter
}
if ($List) {
    $cargoArgs += @('--', '--list')
}

& $cargo.Source @cargoArgs
if ($LASTEXITCODE -ne 0) {
    throw "cargo test failed with exit code $LASTEXITCODE."
}

if ($List) {
    Write-Output "Rust test list completed"
} else {
    Write-Output "Rust tests passed"
}
