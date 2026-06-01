param(
    [string]$EvidenceRoot = "D:\earthmap\quality\photo-parity\v103-selective-token-recipe",
    [string[]]$Samples = @(
        "sahara-core",
        "arabia-coast",
        "mediterranean-edge",
        "congo-edge",
        "europe-forest"
    )
)

$ErrorActionPreference = "Stop"

function Get-SampleMetricDir {
    param(
        [string]$Root,
        [string]$Sample
    )
    $dir = Join-Path $Root "$Sample-metric"
    if ($Sample -eq "sahara-core") {
        $dir = Join-Path $Root "sahara-core-metric"
    }
    return $dir
}

function Get-WeightedMean {
    param(
        [object[]]$Rows,
        [scriptblock]$Value
    )
    $weighted = 0.0
    $count = 0.0
    foreach ($row in $Rows) {
        $rowCount = [double]$row.count
        $weighted += (& $Value $row) * $rowCount
        $count += $rowCount
    }
    if ($count -le 0.0) {
        return 0.0
    }
    return $weighted / $count
}

$rows = foreach ($sample in $Samples) {
    $metricDir = Get-SampleMetricDir -Root $EvidenceRoot -Sample $sample
    $summaryPath = Join-Path $metricDir "candidate-token-recipe-selective-summary.csv"
    if (!(Test-Path -LiteralPath $summaryPath)) {
        throw "Missing selective summary for sample '$sample': $summaryPath"
    }
    Import-Csv -LiteralPath $summaryPath | ForEach-Object {
        $_ | Add-Member -NotePropertyName sample -NotePropertyValue $sample -PassThru
    }
}

$decisionRows = $rows |
    Group-Object standardRgb |
    ForEach-Object {
        $group = @($_.Group)
        $totalPixels = ($group | Measure-Object -Property count -Sum).Sum
        $candidateRows = @($group | Where-Object { $_.winner -eq "candidate" })
        $weightedScoreImprovement = Get-WeightedMean -Rows $group -Value {
            param($row)
            [double]$row.improvementPerPixel
        }
        $weightedSourceImprovement = Get-WeightedMean -Rows $group -Value {
            param($row)
            [double]$row.currentMeanSourceDeltaE - [double]$row.candidateMeanSourceDeltaE
        }
        $weightedStandardImprovement = Get-WeightedMean -Rows $group -Value {
            param($row)
            [double]$row.currentMeanStandardDeltaE - [double]$row.candidateMeanStandardDeltaE
        }
        $recommendedAction = "keep-current"
        if ($candidateRows.Count -ge 2 -and
            $weightedSourceImprovement -gt 0.05 -and
            $weightedStandardImprovement -gt -0.25) {
            $recommendedAction = "candidate-provisional"
        } elseif ($candidateRows.Count -ge 1 -and
            $weightedSourceImprovement -gt 0.20 -and
            $weightedStandardImprovement -gt -0.75) {
            $recommendedAction = "guarded-candidate"
        }

        [pscustomobject]@{
            standardRgb = $_.Name
            totalPixels = [int]$totalPixels
            sampleCount = $group.Count
            candidateSampleWins = $candidateRows.Count
            candidateSamples = ($candidateRows | Select-Object -ExpandProperty sample) -join ";"
            weightedScoreImprovementPerPixel = $weightedScoreImprovement
            weightedSourceImprovementPerPixel = $weightedSourceImprovement
            weightedStandardImprovementPerPixel = $weightedStandardImprovement
            recommendedAction = $recommendedAction
        }
    } |
    Sort-Object @{ Expression = "recommendedAction"; Descending = $false },
        @{ Expression = "weightedSourceImprovementPerPixel"; Descending = $true }

$outputPath = Join-Path $EvidenceRoot "v103-cross-crop-token-decision-table.csv"
$decisionRows | Export-Csv -LiteralPath $outputPath -NoTypeInformation -Encoding UTF8

Write-Host "selectiveTokenDecisionTable=$outputPath"
$decisionRows |
    Where-Object { $_.recommendedAction -ne "keep-current" } |
    Sort-Object @{ Expression = "recommendedAction"; Descending = $false },
        @{ Expression = "weightedSourceImprovementPerPixel"; Descending = $true } |
    Format-Table -AutoSize
