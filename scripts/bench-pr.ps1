# Compares the frame pipeline of two commits, alternating them in one run on this
# machine, and writes a Markdown report plus the list of workloads that got
# clearly slower. A relative comparison: a number from one machine against the
# same machine in the same run, never against a stored absolute figure. It
# flags, it does not fail: the exit status is 0 unless something could not be
# built or run.
#
#   scripts/bench-pr.ps1 -Base <sha> -Head <sha> [-OutDir bench-out]
#
# The working tree must be at -Head. The base is built in a temporary git
# worktree next to it, with its own target directory.

param(
    [Parameter(Mandatory = $true)][string]$Base,
    [Parameter(Mandatory = $true)][string]$Head,
    [string]$OutDir = 'bench-out',
    [int]$Rounds = 5,
    # Workloads of a thousand rows and more: the small ones are too noisy on a
    # shared machine to say anything about a change.
    [string]$Filter = 'update_1k_rows,signal_update_1k_rows,frame_1k_rows,signal_frame_1k_rows,scroll_1k_rows,hover_1k_rows,startup_1k_rows,text_200_paragraphs,clips_shadows_500_cards',
    [double]$Alarm = 0.10
)

$ErrorActionPreference = 'Stop'
$root = (git rev-parse --show-toplevel).Trim()
$null = New-Item -ItemType Directory -Force $OutDir
$out = (Resolve-Path $OutDir).Path

function Build-Bench([string]$dir, [string]$exe) {
    Push-Location $dir
    # Cargo reports progress on stderr, which Windows PowerShell 5.1 treats as an error.
    $ErrorActionPreference = 'Continue'
    try {
        cargo build --release -p florui-bench
        if ($LASTEXITCODE -ne 0) { throw "could not build florui-bench in $dir" }
        Copy-Item (Join-Path $dir 'target/release/florui-bench.exe') $exe -Force
    } finally { Pop-Location }
}

$headSha = (git -C $root rev-parse --short $Head).Trim()
$baseSha = (git -C $root rev-parse --short $Base).Trim()
Build-Bench $root (Join-Path $out 'head.exe')

$baseDir = Join-Path (Split-Path $root -Parent) "florui-bench-base-$baseSha"
git -C $root worktree add --detach $baseDir $Base | Out-Null
try {
    if (-not (Test-Path (Join-Path $baseDir 'crates/florui-bench'))) {
        "The base commit $baseSha has no florui-bench, so there is nothing to compare against." |
            Set-Content (Join-Path $out 'report.md') -Encoding utf8
        exit 0
    }
    Build-Bench $baseDir (Join-Path $out 'base.exe')
} finally {
    git -C $root worktree remove --force $baseDir | Out-Null
}

& (Join-Path $out 'head.exe') ab --a (Join-Path $out 'base.exe') --b (Join-Path $out 'head.exe') `
    --rounds $Rounds --no-heap --filter $Filter --alarm $Alarm --out-dir (Join-Path $out 'ab')
if ($LASTEXITCODE -ne 0) { throw 'florui-bench ab failed' }

$alarmsFile = Join-Path $out 'ab/alarms.md'
$alarmCount = @(Get-Content (Join-Path $out 'ab/alarms.json') -Raw | ConvertFrom-Json).Count
$percent = [int]($Alarm * 100)

$report = @()
$report += '<!-- florui-bench -->'
$report += "## Frame pipeline against the base"
$report += ''
$report += "``$headSha`` against its base ``$baseSha``, release build, alternating the two builds for $Rounds rounds on one machine, headless."
$report += ''
if ($alarmCount -gt 0) {
    $report += "**Possible slowdown to look at** (slower by $percent% or more, with an interval that excludes zero):"
    $report += ''
    $report += (Get-Content $alarmsFile)
    $report += ''
    $report += 'This is a flag, not a verdict: shared CI machines are noisy, so rerun and look at the profiler before treating it as a regression.'
    $report += ''
}
$report += '<details><summary>Full comparison</summary>'
$report += ''
$report += (Get-Content (Join-Path $out 'ab/comparison.md'))
$report += ''
$report += '</details>'
$report += ''
$report += 'A change counts when it exceeds 5% and its 95% bootstrap interval excludes zero. This job never fails a pull request.'
$report | Set-Content (Join-Path $out 'report.md') -Encoding utf8
"alarms=$alarmCount" | Set-Content (Join-Path $out 'alarms.txt') -Encoding ascii
Write-Host "alarms: $alarmCount"
