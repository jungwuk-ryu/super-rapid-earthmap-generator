param(
    [string]$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [string]$Filter = '',
    [switch]$NoBuild,
    [switch]$List,
    [switch]$Isolated
)

$ErrorActionPreference = 'Stop'

if (!$NoBuild -and !$List) {
    & (Join-Path $PSScriptRoot 'build.ps1') -ProjectRoot $ProjectRoot
}

$testSrc = Join-Path $ProjectRoot 'src\test\java'
$mainOut = Join-Path $ProjectRoot 'build\classes\main'
$testOut = Join-Path $ProjectRoot 'build\classes\test'
$buildDir = Join-Path $ProjectRoot 'build'
$sourcesFile = Join-Path $buildDir "test-sources-$PID.txt"
$vendorLib = Join-Path $ProjectRoot 'vendor\lib'

function Convert-TestPathToClassName {
    param(
        [string]$Root,
        [string]$Path
    )
    $relative = $Path.Substring($Root.Length + 1)
    return ($relative -replace '\\', '.') -replace '\.java$', ''
}

$sourceTestClasses = Get-ChildItem -Path $testSrc -Recurse -Filter '*Test.java' |
    Sort-Object FullName |
    ForEach-Object { Convert-TestPathToClassName -Root $testSrc -Path $_.FullName } |
    Where-Object { $_ -ne 'net.earthmap.tests.TestSuiteRunner' }

if ($Filter -ne '') {
    $sourceTestClasses = @($sourceTestClasses | Where-Object { $_ -match $Filter })
}

if ($List) {
    $sourceTestClasses | ForEach-Object { Write-Output $_ }
    Write-Output "testCount=$($sourceTestClasses.Count)"
    return
}

if (Test-Path -LiteralPath $testOut) {
    Remove-Item -LiteralPath $testOut -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $testOut, $buildDir | Out-Null
try {
    Get-ChildItem -Path $testSrc -Recurse -Filter '*.java' | ForEach-Object {
        $_.FullName
    } | Set-Content -Encoding ASCII $sourcesFile

    if ((Get-Content $sourcesFile).Count -eq 0) {
        throw "No test Java sources found."
    }

    $classpathEntries = @($mainOut)
    if (Test-Path -LiteralPath $vendorLib) {
        $classpathEntries += Get-ChildItem -LiteralPath $vendorLib -Filter '*.jar' | Sort-Object FullName | ForEach-Object {
            $_.FullName
        }
    }
    $mainClasspath = $classpathEntries -join [IO.Path]::PathSeparator

    javac --release 25 -cp $mainClasspath -d $testOut "@$sourcesFile"
    if ($LASTEXITCODE -ne 0) {
        throw "javac failed for test sources with exit code $LASTEXITCODE."
    }
} finally {
    Remove-Item -LiteralPath $sourcesFile -ErrorAction SilentlyContinue
}

$testClasses = Get-ChildItem -Path $testOut -Recurse -Filter '*Test.class' |
    Where-Object { $_.Name -notmatch '\$' } |
    Sort-Object FullName |
    ForEach-Object {
        $relative = $_.FullName.Substring($testOut.Length + 1)
        ($relative -replace '\\', '.') -replace '\.class$', ''
    } |
    Where-Object { $_ -ne 'net.earthmap.tests.TestSuiteRunner' }

if ($Filter -ne '') {
    $testClasses = @($testClasses | Where-Object { $_ -match $Filter })
}

if ($testClasses.Count -eq 0) {
    throw "No compiled test classes found."
}

$testClasspath = (@($mainOut, $testOut) + ($classpathEntries | Where-Object { $_ -ne $mainOut })) -join [IO.Path]::PathSeparator
if ($Isolated) {
    foreach ($testClass in $testClasses) {
        Write-Output "Running $testClass"
        java "--enable-native-access=ALL-UNNAMED" "-Dearthmap.projectRoot=$ProjectRoot" -cp $testClasspath $testClass
        if ($LASTEXITCODE -ne 0) {
            throw "Test $testClass failed with exit code $LASTEXITCODE."
        }
    }
} else {
    Write-Output "Running $($testClasses.Count) tests in one JVM"
    java "--enable-native-access=ALL-UNNAMED" "-Dearthmap.projectRoot=$ProjectRoot" -cp $testClasspath `
        net.earthmap.tests.TestSuiteRunner @testClasses
    if ($LASTEXITCODE -ne 0) {
        throw "TestSuiteRunner failed with exit code $LASTEXITCODE."
    }
}

$node = Get-Command node -ErrorAction SilentlyContinue
if ($node) {
    $nodeScripts = Get-ChildItem -Path (Join-Path $ProjectRoot 'scripts') -Recurse -Filter '*.mjs' |
        Sort-Object FullName
    foreach ($nodeScript in $nodeScripts) {
        Write-Output "Checking $($nodeScript.FullName)"
        node --check $nodeScript.FullName
        if ($LASTEXITCODE -ne 0) {
            throw "node --check failed for $($nodeScript.FullName) with exit code $LASTEXITCODE."
        }
    }
} else {
    Write-Output "Skipping Node script syntax checks; node was not found."
}

Write-Output "Tests passed"
