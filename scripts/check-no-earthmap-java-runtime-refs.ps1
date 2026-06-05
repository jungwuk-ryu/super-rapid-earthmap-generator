[CmdletBinding()]
param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
)

$ErrorActionPreference = 'Stop'

$blockedPatterns = @(
    @{ Name = 'java command'; Pattern = '\bjava\b' },
    @{ Name = 'javac command'; Pattern = '\bjavac\b' },
    @{ Name = 'legacy cli package'; Pattern = 'net\.earthmap\.cli' },
    @{ Name = 'legacy cli class'; Pattern = 'EarthMapCli' },
    @{ Name = 'legacy class output'; Pattern = 'build\\classes' },
    @{ Name = 'legacy main source path'; Pattern = 'src\\main\\java|src/main/java' },
    @{ Name = 'legacy test source path'; Pattern = 'src\\test\\java|src/test/java' },
    @{ Name = 'normal root run wrapper reference'; Pattern = 'scripts\\run\.ps1|scripts/run\.ps1' }
)

$excludedRelativePaths = @(
    'docs/RUST-ONLY-TODO.md',
    'scripts/check-no-earthmap-java-runtime-refs.ps1'
)

$scanRoots = @(
    (Join-Path $ProjectRoot 'AGENTS.md'),
    (Join-Path $ProjectRoot 'README.md'),
    (Join-Path $ProjectRoot 'docs'),
    (Join-Path $ProjectRoot 'scripts')
)

function Convert-ToRelativePath {
    param([string]$Path)

    $full = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetFullPath($ProjectRoot)
    if (!$root.EndsWith([IO.Path]::DirectorySeparatorChar)) {
        $root += [IO.Path]::DirectorySeparatorChar
    }
    return $full.Substring($root.Length).Replace('\', '/')
}

$files = New-Object System.Collections.Generic.List[string]
foreach ($scanRoot in $scanRoots) {
    if (-not (Test-Path -LiteralPath $scanRoot)) {
        continue
    }
    $item = Get-Item -LiteralPath $scanRoot
    if ($item.PSIsContainer) {
        Get-ChildItem -LiteralPath $scanRoot -Recurse -File |
            Where-Object { $_.Extension -in '.md', '.ps1', '.mjs', '.json' } |
            ForEach-Object { [void]$files.Add($_.FullName) }
    } else {
        [void]$files.Add($item.FullName)
    }
}

$violations = New-Object System.Collections.Generic.List[object]
foreach ($file in ($files | Sort-Object -Unique)) {
    $relative = Convert-ToRelativePath -Path $file
    if ($excludedRelativePaths -contains $relative) {
        continue
    }
    if ($relative.StartsWith('docs/archived-reference/', [StringComparison]::Ordinal)) {
        continue
    }
    $lineNumber = 0
    foreach ($line in Get-Content -LiteralPath $file) {
        $lineNumber++
        foreach ($blocked in $blockedPatterns) {
            if ($line -cmatch $blocked.Pattern) {
                [void]$violations.Add([PSCustomObject]@{
                        Path = $relative
                        Line = $lineNumber
                        Pattern = $blocked.Name
                        Text = $line.Trim()
                    })
            }
        }
    }
}

if ($violations.Count -gt 0) {
    $violations | Format-Table -AutoSize | Out-String | Write-Error
    throw "Forbidden EarthMap Java runtime references found: $($violations.Count)"
}

Write-Output "No forbidden EarthMap Java runtime references found."
