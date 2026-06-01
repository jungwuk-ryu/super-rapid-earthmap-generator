param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
)

$ErrorActionPreference = 'Stop'

$mainSrc = Join-Path $ProjectRoot 'src\main\java'
$buildDir = Join-Path $ProjectRoot 'build'
$classesDir = Join-Path $buildDir 'classes'
$mainOut = Join-Path $classesDir 'main'
$compileOut = Join-Path $classesDir "main-next-$PID"
$lockPath = Join-Path $classesDir 'compile.lock'
$sourcesFile = Join-Path $buildDir "main-sources-$PID.txt"
$vendorLib = Join-Path $ProjectRoot 'vendor\lib'

$lockStream = $null
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

New-Item -ItemType Directory -Force -Path $classesDir, $buildDir | Out-Null
try {
    $lockStream = Enter-CompileLock -Path $lockPath
    if (Test-Path -LiteralPath $compileOut) {
        Remove-Item -LiteralPath $compileOut -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $compileOut | Out-Null

    Get-ChildItem -Path $mainSrc -Recurse -Filter '*.java' | ForEach-Object {
        $_.FullName
    } | Set-Content -Encoding ASCII $sourcesFile

    if ((Get-Content $sourcesFile).Count -eq 0) {
        throw "No main Java sources found."
    }

    $classpathJars = @()
    if (Test-Path -LiteralPath $vendorLib) {
        $classpathJars = Get-ChildItem -LiteralPath $vendorLib -Filter '*.jar' | Sort-Object FullName | ForEach-Object {
            $_.FullName
        }
    }

    if ($classpathJars.Count -gt 0) {
        javac --release 25 -cp ($classpathJars -join [IO.Path]::PathSeparator) -d $compileOut "@$sourcesFile"
    } else {
        javac --release 25 -d $compileOut "@$sourcesFile"
    }
    if ($LASTEXITCODE -ne 0) {
        throw "javac failed for main sources with exit code $LASTEXITCODE."
    }
    if (Test-Path -LiteralPath $mainOut) {
        Remove-Item -LiteralPath $mainOut -Recurse -Force
    }
    Move-Item -LiteralPath $compileOut -Destination $mainOut
} finally {
    Remove-Item -LiteralPath $sourcesFile -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $compileOut) {
        Remove-Item -LiteralPath $compileOut -Recurse -Force -ErrorAction SilentlyContinue
    }
    if ($lockStream -ne $null) {
        $lockStream.Dispose()
    }
}
Write-Output "Build passed: $mainOut"
