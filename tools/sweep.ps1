<#
.SYNOPSIS
  Run every scenario in tools/scenes against the real application, and say what held.

.DESCRIPTION
  The regression suite for things `cargo test` cannot reach. Each scenario starts the binary,
  operates it, and asserts against the state the application reports about itself; this runs the
  lot one after another and prints a line per scenario. One at a time, always: the mouse and the
  keyboard are one physical thing and two runs would fight over them.

  Everything a run produced is left in target/drive -- a picture, the log, the control atlas, the
  state, the checks, and a picture of the screen any failing step gave up on.

.EXAMPLE
  ./tools/sweep.ps1
  ./tools/sweep.ps1 -Only layers,brush
#>
[CmdletBinding()]
param(
    [string[]]$Only = @(),
    [int]$Width = 2200,
    [int]$Height = 1450
)

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$scenes = Get-ChildItem (Join-Path $PSScriptRoot 'scenes') -Filter *.txt | Sort-Object Name
if ($Only.Count) { $scenes = $scenes | Where-Object { $Only -contains $_.BaseName } }

$exe = Join-Path $root 'target\drive-build\release\openpaint.exe'
if (-not (Test-Path $exe)) {
    throw "no binary at $exe -- cargo build --release --target-dir target/drive-build -p openpaint-app"
}

# Export drops a timestamped PNG beside wherever the application started, and the keyboard
# scenario presses Ctrl+E every run. Cleared first so they do not pile up one per sweep.
Get-ChildItem -Path $root -Filter 'openpaint-*.png' -File -ErrorAction SilentlyContinue |
    Remove-Item -Force -ErrorAction SilentlyContinue

$results = @()
foreach ($scene in $scenes) {
    $name = $scene.BaseName
    Write-Output "--- $name"
    # The recovery scenario needs a document to offer as an abandoned copy, and `file.txt` --
    # which sorts before it -- has just written one. A real crash is the only other way to make
    # one, and crashing the application on purpose is not a thing a suite should do.
    $extra = @{}
    if ($name -eq 'recovery') {
        $doc = Join-Path $root 'target\drive\driven.openpaint'
        if (-not (Test-Path $doc)) {
            Write-Output "  SKIPPED no $doc -- the file scenario has to run first"
            $results += [pscustomobject]@{ Scene = $name; Ok = 0; Failed = 0; Stopped = 'skipped' }
            continue
        }
        $extra['PlantRecovery'] = $doc
    }
    # Caught, so one scenario that fails does not take the rest of the suite with it -- the point
    # of a suite is the whole table at the end, not the first thing that broke.
    $out = @()
    try {
        $out = & (Join-Path $PSScriptRoot 'drive.ps1') -Shot $name -Width $Width -Height $Height `
            -Script $scene.FullName @extra 2>&1
    } catch {
        $out = @($_)
    }
    $checks = Join-Path $root "target\drive\$name.checks"
    $lines = if (Test-Path $checks) { @(Get-Content -LiteralPath $checks) } else { @() }
    $ok = @($lines | Where-Object { $_ -match '^\s+ok' }).Count
    $bad = @($lines | Where-Object { $_ -match '^\s+FAIL' }).Count
    # A scenario can also stop dead -- a control it named is not on screen, a panel will not
    # scroll -- and that is a failure even though it made no assertion about it.
    $stopped = @($out | Where-Object { $_ -is [System.Management.Automation.ErrorRecord] })
    $results += [pscustomobject]@{
        Scene = $name; Ok = $ok; Failed = $bad
        Stopped = if ($stopped.Count) { ($stopped[0].ToString() -split "`n")[0].Trim() } else { '' }
    }
    foreach ($l in ($lines | Where-Object { $_ -match '^\s+FAIL' })) { Write-Output $l }
    if ($stopped.Count) { Write-Output "  STOPPED $(($stopped[0].ToString() -split "`n")[0].Trim())" }
}

Write-Output ''
Write-Output '=== what held ==='
$results | Format-Table -AutoSize | Out-String | Write-Output
$broken = @($results | Where-Object { $_.Failed -gt 0 -or $_.Stopped })
if ($broken.Count) {
    Write-Output "$($broken.Count) of $($results.Count) scenarios did not pass."
    exit 1
}
Write-Output "all $($results.Count) scenarios passed, $(($results | Measure-Object Ok -Sum).Sum) assertions."
