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

$results = @()
foreach ($scene in $scenes) {
    $name = $scene.BaseName
    Write-Output "--- $name"
    $out = & (Join-Path $PSScriptRoot 'drive.ps1') -Shot $name -Width $Width -Height $Height `
        -Script $scene.FullName 2>&1
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
