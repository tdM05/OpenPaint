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
    [int]$Height = 1450,
    # **What the scenes are calibrated against.** The Surface's own screen reports 1.5, which makes
    # the 2200x1450 window a 1452x929 workspace -- the size every tab position, window rectangle
    # and canvas coordinate in `scenes/` was measured on. A run at another scale is refused rather
    # than reported as failures; see the check in `drive.ps1`.
    [double]$Scale = 1.5
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

# The import scenario brings pictures in through the same dialog, so it needs two: a PNG with
# transparency, and a JPEG, because those are the two decoders and a suite that exercises one of
# them says nothing about the other. Solid rectangles rather than anything drawn: what is being
# asserted is *where the pixels landed*, and a flat block is the shape that makes an ink count
# mean something.
$fixtures = Join-Path $root 'target' | Join-Path -ChildPath 'drive'
New-Item -ItemType Directory -Force -Path $fixtures | Out-Null
Add-Type -AssemblyName System.Drawing
$png = Join-Path $fixtures 'import-fixture.png'
if (-not (Test-Path $png)) {
    # Wider than it is tall and smaller than the default page, so a scene can assert both that it
    # arrived and that it did not cover the whole page -- "it filled everything" and "it landed
    # correctly" are otherwise the same ink count.
    $bmp = New-Object System.Drawing.Bitmap 1400, 900
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.Clear([System.Drawing.Color]::FromArgb(255, 20, 20, 24))
    $g.Dispose()
    $bmp.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}
$jpg = Join-Path $fixtures 'import-fixture.jpg'
if (-not (Test-Path $jpg)) {
    # A different size from the PNG and from the default page, so "the page became the picture's
    # size" cannot pass by accident.
    $bmp = New-Object System.Drawing.Bitmap 800, 600
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.Clear([System.Drawing.Color]::FromArgb(255, 24, 20, 20))
    $g.Dispose()
    $bmp.Save($jpg, [System.Drawing.Imaging.ImageFormat]::Jpeg)
    $bmp.Dispose()
}

# The bitmap-tip scenario loads a PNG through the application's own file dialog, so one has to
# exist at a known path. Drawn here rather than kept in the repository: it is a fixture of the
# suite, not an asset of the application, and a few lines of arithmetic are clearer than a blob.
$tip = Join-Path $root 'target' | Join-Path -ChildPath 'drive' |
    Join-Path -ChildPath 'stamp-fixture.png'
if (-not (Test-Path $tip)) {
    $n = 96
    $bmp = New-Object System.Drawing.Bitmap $n, $n
    for ($y = 0; $y -lt $n; $y++) {
        for ($x = 0; $x -lt $n; $x++) {
            $dx = ($x - $n / 2) / ($n / 2)
            $dy = ($y - $n / 2) / ($n / 2) * 2.2
            $r = [Math]::Sqrt($dx * $dx + $dy * $dy)
            $a = [Math]::Max(0.0, 1.0 - $r)
            $v = [int](255 * [Math]::Pow($a, 0.7))
            $bmp.SetPixel($x, $y, [System.Drawing.Color]::FromArgb($v, 0, 0, 0))
        }
    }
    $bmp.Save($tip, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

# Whatever the export scenario wrote last time. A save dialog given a name that already exists
# asks whether to overwrite it, and that question is a native modal with none of our controls on
# screen -- so a leftover file does not get overwritten, it *hangs the run*, and the checks then
# read the stale file's size as the new one's. Deleted rather than answered.
# `out-*`, whatever the extension: the export scenario writes PNGs and the end-to-end one writes
# an `.openpaint` document as well, and a saved document left behind traps the next run exactly
# the same way -- a save dialog handed a name that already exists asks whether to overwrite it,
# and that question is a native modal with none of our controls on screen.
Get-ChildItem -Path $fixtures -Filter 'out-*' -File -ErrorAction SilentlyContinue |
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
            -Scale $Scale `
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
