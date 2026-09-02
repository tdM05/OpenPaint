<#
.SYNOPSIS
  Launch OpenPaint, do things to it, and take a picture of what happened.

.DESCRIPTION
  The gap this closes: everything else here tests the *pieces*. `cargo test` drives the workspace's
  own entry point with synthetic frames, and the screenshot tests render panels into a headless GPU
  surface. Neither can say whether the application works -- whether the brush paints, whether a
  press on a panel reaches the editor, whether the thing opens at all. Those bugs shipped anyway,
  and the only reason they were found is that a person opened it and said so.

  So: start the real binary, send it real input, save a real screenshot of its window, and keep the
  log it writes. No extension, no MCP, nothing to install -- Windows already exposes all three.

  Two things worth knowing about what this reaches and what it does not:

  - **The input arrives as a pen.** OpenPaint reads its pointer through octotablet, and Windows Ink
    presents an ordinary mouse as a pen with no pressure axis -- the app's own log says
    `type=Some(Pen) ... pressure will read a constant 1.0`. So the pen path is exercised; varying
    pressure, tilt, and a real tablet's report rate are not.
  - **It takes the mouse and the keyboard while it runs.** Nothing else can be used at the same
    time. Runs should be short for that reason, and this says so rather than pretending otherwise.

.EXAMPLE
  ./tools/drive.ps1 -Shot start
  ./tools/drive.ps1 -Shot painted -Do 'drag 620 380 900 560'
  ./tools/drive.ps1 -Shot erased -Do 'key e; drag 620 380 820 470'

.PARAMETER Do
  A semicolon-separated script of steps. Coordinates are in the **client area**; names are read
  from the atlas the app writes every frame (`OPENPAINT_CONTROLS`), so a step says which control
  it means and still means it at a different window size.

    move X Y            put the pointer there
    click X Y           press and release there
    right X Y           the other button
    drag X1 Y1 X2 Y2    press, move along the path in steps, release -- what a stroke is
    key NAME            a key by name: b, e, f2, f3, ctrl+z, escape, enter
    type TEXT           the text, as typing
    wait MS             let the app catch up

    tab NAME            bring that panel's tab to the front
    press NAME          press the control whose label matches, anywhere on screen
    press PANEL:NAME    ...or only in that panel, when two of them share a word
    rpress NAME         the other button, on the same control
    slide NAME F        drag that slider to fraction F of its width (0..1)
    shot NAME           save a picture now, mid-run, and keep going

  A name match is case-insensitive and matches a whole label or a leading word of one. A step that
  names something the atlas does not have stops the run and says so -- a test that silently clicks
  nowhere is worse than no test.

.PARAMETER Script
  A file of steps, one per line, `#` for a comment. What `-Do` is for a long list.
#>
[CmdletBinding()]
param(
    [string]$Shot = 'shot',
    [string]$Do = '',
    [string]$Script = '',
    [int]$Width = 1280,
    [int]$Height = 820,
    [int]$Settle = 3000,
    [switch]$Keep,
    [switch]$KeepWorkspace
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class Win {
    // **This process must measure in the same pixels the app draws in.**
    // Without it PowerShell is DPI-unaware, so Windows virtualises every coordinate: a window that
    // is really 1898 physical pixels wide reports 1265, and a screenshot of "the whole window"
    // quietly captured its top-left two thirds. The right-hand panels looked missing and they were
    // there the whole time -- an hour spent hunting a bug in the app that was in the ruler.
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
    public static readonly IntPtr PER_MONITOR_V2 = new IntPtr(-4);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, IntPtr e);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int t, bool repaint);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    public delegate bool EnumProc(IntPtr h, IntPtr p);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
    public const uint LDOWN = 0x0002, LUP = 0x0004, RDOWN = 0x0008, RUP = 0x0010, WHEEL = 0x0800;
    public const uint MOVE = 0x0001, ABSOLUTE = 0x8000, VIRTUALDESK = 0x4000;

    // **Motion has to be injected, not assigned.**
    // `SetCursorPos` puts the cursor somewhere; it does not put an event in the input stream, so
    // Windows Ink synthesises no stylus pose from it. A drag made of `SetCursorPos` calls reached
    // the app as a single pose at the press and nothing after -- one dab, no line, which reads
    // exactly like a broken brush. This goes through the same path a real hand does.
    public static void MoveTo(int x, int y) {
        int w = GetSystemMetrics(78);   // SM_CXVIRTUALSCREEN
        int h = GetSystemMetrics(79);   // SM_CYVIRTUALSCREEN
        int vx = GetSystemMetrics(76);  // SM_XVIRTUALSCREEN
        int vy = GetSystemMetrics(77);  // SM_YVIRTUALSCREEN
        uint nx = (uint)(((double)(x - vx) * 65535.0) / (w - 1));
        uint ny = (uint)(((double)(y - vy) * 65535.0) / (h - 1));
        mouse_event(MOVE | ABSOLUTE | VIRTUALDESK, nx, ny, 0, IntPtr.Zero);
    }

    // The process's own drawing window, which is not its console.
    //
    // `MainWindowHandle` answers with whichever window Windows thinks is the main one, and for a
    // console application that is the console -- so a screenshot of "the app" came out as a
    // screenshot of its log. The class name is what tells the two apart.
    public static IntPtr AppWindow(uint pid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr h, IntPtr p) {
            uint who; GetWindowThreadProcessId(h, out who);
            if (who != pid || !IsWindowVisible(h)) return true;
            var sb = new StringBuilder(256);
            GetClassName(h, sb, sb.Capacity);
            string cls = sb.ToString();
            if (cls.Contains("Console") || cls.Contains("CASCADIA")) return true;
            found = h; return false;
        }, IntPtr.Zero);
        return found;
    }
}
'@

[void][Win]::SetProcessDpiAwarenessContext([Win]::PER_MONITOR_V2)

$root = Split-Path -Parent $PSScriptRoot
# Its own build directory, so driving the app never fights the copy the artist has open --
# Windows locks a running exe, and killing theirs to test mine is not a trade to make.
$exe = Join-Path $root 'target\drive-build\release\openpaint.exe'
if (-not (Test-Path $exe)) {
    throw "no binary at $exe -- cargo build --release --target-dir target/drive-build first"
}
$outDir = Join-Path $root 'target\drive'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# A fresh workspace every run, so a picture is of the code and not of whatever was saved last.
# `-KeepWorkspace` is for the one thing that needs the opposite: proving a saved one still loads.
$saved = Join-Path $env:LOCALAPPDATA 'OpenPaint\workspace.json'
$stash = "$saved.driving"
if (-not $KeepWorkspace -and (Test-Path $saved)) { Move-Item $saved $stash -Force }

# **And the artist's recovered work is set aside, never answered.** A crashed session leaves a
# file here and the app opens asking what to do with it. The only two answers are Recover and
# Discard, and Discard destroys unsaved work that is not mine to destroy -- so the run never sees
# the question. Put back in `finally`, whatever happens.
$rec = Join-Path $env:LOCALAPPDATA 'OpenPaint' | Join-Path -ChildPath 'recovery'
$recStash = "$rec-driving"
if (Test-Path $rec) { Move-Item $rec $recStash -Force }

# The app writes where every control landed, once per frame, and the name-based steps read it.
$atlas = Join-Path $outDir "$Shot.atlas"
$env:OPENPAINT_CONTROLS = $atlas
$env:OPENPAINT_TRACE_INPUT = '1'
# And what the application currently is, so a step can assert on the consequence of a press
# rather than on a picture of one.
$state = Join-Path $outDir "$Shot.now"
$env:OPENPAINT_STATE = $state

$log = Join-Path $outDir "$Shot.log"
$proc = Start-Process -FilePath $exe -PassThru -RedirectStandardOutput $log `
    -RedirectStandardError (Join-Path $outDir "$Shot.err")
try {
    Start-Sleep -Milliseconds $Settle
    $h = [Win]::AppWindow($proc.Id)
    if ($h -eq [IntPtr]::Zero) { throw 'the app window never appeared' }
    # A known size and place, so a coordinate in a script means the same thing every run.
    [void][Win]::MoveWindow($h, 60, 60, $Width, $Height, $true)
    [void][Win]::SetForegroundWindow($h)
    Start-Sleep -Milliseconds 800

    # Steps are in the *client* area's coordinates, so they mean the same thing whatever the title
    # bar and borders happen to be.
    $c = New-Object Win+RECT
    [void][Win]::GetClientRect($h, [ref]$c)
    $o = New-Object Win+POINT
    [void][Win]::ClientToScreen($h, [ref]$o)
    $ox = $o.X
    $oy = $o.Y

    function Point-At([int]$x, [int]$y) {
        [Win]::MoveTo($ox + $x, $oy + $y)
        Start-Sleep -Milliseconds 25
    }

    # Everything the app drew this frame, by name. Re-read on every use because the app rewrites
    # it every frame: pressing a tab moves every control behind it, and a stale atlas is a script
    # that clicks confidently at where a thing used to be.
    function Read-Atlas {
        $found = @{}
        $tabs = @{}
        $panel = ''
        $view = $null
        if (-not (Test-Path $atlas)) { return @($found, $tabs) }
        foreach ($line in (Get-Content -LiteralPath $atlas -ErrorAction SilentlyContinue)) {
            if ($line.StartsWith('# ')) { $panel = $line.Substring(2).Trim(); $view = $null; continue }
            if ($line.StartsWith('@ ')) {
                $f = $line.Substring(2) -split "`t"
                if ($f.Count -ge 5) {
                    $tabs[$f[0].ToLower()] = [pscustomobject]@{
                        X = [int]$f[1]; Y = [int]$f[2]; W = [int]$f[3]; H = [int]$f[4]
                    }
                }
                continue
            }
            $f = $line -split "`t"
            if ($f[0] -eq '$') {
                # The panel's own window onto its list: x y w h scroll tall.
                $view = [pscustomobject]@{
                    X = [int]$f[1]; Y = [int]$f[2]; W = [int]$f[3]; H = [int]$f[4]
                    Scroll = [int]$f[5]; Tall = [int]$f[6]; Panel = $panel
                }
                continue
            }
            if ($f.Count -lt 6) { continue }
            $rect = [pscustomobject]@{
                X = [int]$f[1]; Y = [int]$f[2]; W = [int]$f[3]; H = [int]$f[4]
                Panel = $panel; View = $view; Label = $f[5]
            }
            # Indexed under four keys, so a step can say the label, the control's own id, or
            # `Panel:label` when two panels both have an "Opacity".
            foreach ($k in @($f[5], $f[0], "${panel}:$($f[5])", "${panel}:$($f[0])")) {
                $k = $k.Trim().ToLower()
                if ($k -and -not $found.ContainsKey($k)) { $found[$k] = $rect }
            }
        }
        return @($found, $tabs)
    }

    # Turn the wheel over a point. Injected like everything else here, because the app only gives
    # the wheel to the panel the pointer is actually over.
    function Wheel-At([int]$x, [int]$y, [int]$notches) {
        Point-At $x $y
        Start-Sleep -Milliseconds 60
        for ($i = 0; $i -lt [Math]::Abs($notches); $i++) {
            [Win]::mouse_event([Win]::WHEEL, 0, 0, [uint32]$(if ($notches -gt 0) { 120 } else { [uint32]4294967176 }), [IntPtr]::Zero)
            Start-Sleep -Milliseconds 40
        }
        Start-Sleep -Milliseconds 200
    }

    # **Bring a control into its panel before pressing it.**
    # A panel's list is usually taller than the panel -- the brush's is four times the window --
    # and `place` gives every control a position whether or not it is on screen. Clicking the
    # reported y of a control that is scrolled out of view lands on whatever is there instead,
    # which is a test that passes by pressing the wrong thing. So: scroll until the atlas says it
    # is inside, then press. Returns the rectangle as it is now.
    function Bring-Into-View([string]$name) {
        for ($try = 0; $try -lt 24; $try++) {
            $r = Rect-Of $name
            $v = $r.View
            if (-not $v) { return $r }
            $top = $r.Y
            $bot = $r.Y + $r.H
            if ($top -ge $v.Y -and $bot -le ($v.Y + $v.H)) { return $r }
            # One notch at a time, re-reading in between: how far a notch goes is the app's
            # business, and guessing it is how a scroll overshoots and calls the miss a pass.
            $notches = if ($bot -gt ($v.Y + $v.H)) { -1 } else { 1 }
            Wheel-At ($v.X + [int]($v.W / 2)) ($v.Y + [int]($v.H / 2)) $notches
        }
        throw "'$name' will not come into view in its panel"
    }

    # The rectangle a name means, or a stop. Whole label first, then a label that starts with it --
    # never a random substring, which is how "Size" would have pressed "Size jitter".
    function Rect-Of([string]$name, [switch]$Tab) {
        $a = Read-Atlas
        $table = if ($Tab) { $a[1] } else { $a[0] }
        $k = $name.Trim().ToLower()
        if ($table.ContainsKey($k)) { return $table[$k] }
        $near = @($table.Keys | Where-Object { $_.StartsWith($k + ' ') -or $_.StartsWith($k + ':') })
        if ($near.Count -eq 1) { return $table[$near[0]] }
        $what = if ($Tab) { 'tab' } else { 'control' }
        $have = ($table.Keys | Sort-Object) -join ', '
        if ($near.Count -gt 1) { throw "'$name' names $($near.Count) ${what}s: $($near -join ', ')" }
        throw "no $what called '$name'. On screen: $have"
    }

    $steps = @()
    if ($Script) {
        $steps = Get-Content -LiteralPath $Script |
            ForEach-Object { $_.Trim() } |
            Where-Object { $_ -and -not $_.StartsWith('#') }
    } else {
        $steps = $Do -split ';' | Where-Object { $_.Trim() }
    }

    function Save-Shot([string]$name) {
        Start-Sleep -Milliseconds 400
        $r = New-Object Win+RECT
        [void][Win]::GetClientRect($h, [ref]$r)
        $b = New-Object System.Drawing.Bitmap ($r.Right - $r.Left), ($r.Bottom - $r.Top)
        $gg = [System.Drawing.Graphics]::FromImage($b)
        $gg.CopyFromScreen($ox, $oy, 0, 0, $b.Size)
        $p = Join-Path $outDir "$name.png"
        $b.Save($p, [System.Drawing.Imaging.ImageFormat]::Png)
        $gg.Dispose(); $b.Dispose()
        # Beside every picture, the layout it is a picture of -- so a failure can be read after the
        # run without launching anything.
        $beside = Join-Path $outDir "$name.controls"
        if (Test-Path $atlas) { Copy-Item -LiteralPath $atlas -Destination $beside -Force }
        if (Test-Path $state) {
            Copy-Item -LiteralPath $state -Destination (Join-Path $outDir "$name.state") -Force
        }
        Write-Output $p
    }

    # What the application says it currently is. One key per line, tab-separated.
    function Read-State {
        $st = @{}
        if (Test-Path $state) {
            foreach ($line in (Get-Content -LiteralPath $state -ErrorAction SilentlyContinue)) {
                $f = $line -split "`t", 2
                if ($f.Count -eq 2) { $st[$f[0]] = $f[1] }
            }
        }
        return $st
    }

    # Every assertion this run made, so the end of a run says what it proved rather than only that
    # it did not crash.
    $checks = New-Object System.Collections.ArrayList

    function Expect-State([string]$key, [string]$want) {
        # Given a frame or two: a press is answered on the next paint, not on the release.
        $got = $null
        for ($i = 0; $i -lt 20; $i++) {
            $got = (Read-State)[$key]
            if ($got -eq $want) { break }
            Start-Sleep -Milliseconds 100
        }
        if ($got -eq $want) {
            [void]$checks.Add("  ok    $key = $want")
        } else {
            [void]$checks.Add("  FAIL  ${key}: wanted '$want', got '$got'")
            $script:failed = $true
        }
    }

    function Click-At([int]$x, [int]$y, [switch]$Right) {
        Point-At $x $y
        Start-Sleep -Milliseconds 60
        [Win]::mouse_event($(if ($Right) { [Win]::RDOWN } else { [Win]::LDOWN }), 0, 0, 0, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 90
        [Win]::mouse_event($(if ($Right) { [Win]::RUP } else { [Win]::LUP }), 0, 0, 0, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 300
    }

    foreach ($step in $steps) {
        $a = $step.Trim() -split '\s+'
        switch ($a[0].ToLower()) {
            'move'  { Point-At $a[1] $a[2] }
            'click' {
                Point-At $a[1] $a[2]
                [Win]::mouse_event([Win]::LDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 80
                [Win]::mouse_event([Win]::LUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 250
            }
            'right' {
                Point-At $a[1] $a[2]
                [Win]::mouse_event([Win]::RDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 80
                [Win]::mouse_event([Win]::RUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 250
            }
            'drag'  {
                # Along the path in steps, because one jump is a pointer teleporting and a stroke
                # is a path. The app reads this as a pen, so it is the real stroke code.
                Point-At $a[1] $a[2]
                [Win]::mouse_event([Win]::LDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 60
                $steps = 30
                for ($i = 1; $i -le $steps; $i++) {
                    $t = $i / $steps
                    Point-At ([int]([int]$a[1] + ([int]$a[3] - [int]$a[1]) * $t)) `
                             ([int]([int]$a[2] + ([int]$a[4] - [int]$a[2]) * $t))
                }
                [Win]::mouse_event([Win]::LUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 350
            }
            'key'   {
                $k = switch ($a[1].ToLower()) {
                    'escape'       { '{ESC}' }
                    'enter'        { '{ENTER}' }
                    'delete'       { '{DEL}' }
                    'f2'           { '{F2}' }
                    'f3'           { '{F3}' }
                    'ctrl+z'       { '^z' }
                    'ctrl+shift+z' { '^+z' }
                    'ctrl+e'       { '^e' }
                    'ctrl+a'       { '^a' }
                    'ctrl+d'       { '^d' }
                    default        { $a[1] }
                }
                [System.Windows.Forms.SendKeys]::SendWait($k)
                Start-Sleep -Milliseconds 250
            }
            'type'  {
                # Everything after the word, spaces kept: a caption is not one token.
                $text = $step.Trim().Substring(4).Trim()
                # SendKeys reads these as syntax; a literal one is written in braces.
                $text = $text -replace '([\+\^%~\(\)\{\}\[\]])', '{$1}'
                [System.Windows.Forms.SendKeys]::SendWait($text)
                Start-Sleep -Milliseconds 300
            }
            'wait'  { Start-Sleep -Milliseconds ([int]$a[1]) }
            'expect' {
                # `expect KEY VALUE` -- the value is everything after the key, spaces kept.
                $rest = $step.Trim().Substring(6).Trim()
                $k, $v = $rest -split '\s+', 2
                Expect-State $k $v
            }
            'state' {
                # Print the whole of it, for a step that is exploring rather than asserting.
                Write-Output "--- state: $($a[1]) ---"
                if (Test-Path $state) { Get-Content -LiteralPath $state | Write-Output }
            }
            'shot'  { Save-Shot $a[1] }
            'tab'   {
                $r = Rect-Of $a[1] -Tab
                Click-At ($r.X + [int]($r.W / 2)) ($r.Y + [int]($r.H / 2))
                Start-Sleep -Milliseconds 200
            }
            'press' {
                $r = Bring-Into-View $a[1]
                Click-At ($r.X + [int]($r.W / 2)) ($r.Y + [int]($r.H / 2))
            }
            'rpress' {
                $r = Bring-Into-View $a[1]
                Click-At ($r.X + [int]($r.W / 2)) ($r.Y + [int]($r.H / 2)) -Right
            }
            'wheel' {
                # `wheel X Y N` -- N notches at a point, for testing the wheel itself.
                Wheel-At ([int]$a[1]) ([int]$a[2]) ([int]$a[3])
            }
            'slide' {
                # A slider is dragged, not clicked: a press sets the value under the pointer and a
                # drag is what a hand does, and only one of the two exercises the tracking code.
                $r = Bring-Into-View $a[1]
                $y = $r.Y + [int]($r.H / 2)
                $pad = 8
                $x0 = $r.X + $pad
                $x1 = $r.X + $r.W - $pad
                $to = [int]($x0 + ($x1 - $x0) * [double]$a[2])
                Point-At ($x0 + [int](($x1 - $x0) / 2)) $y
                [Win]::mouse_event([Win]::LDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 60
                for ($i = 1; $i -le 12; $i++) {
                    $t = $i / 12.0
                    Point-At ([int]($x0 + ($x1 - $x0) / 2 + ($to - ($x0 + ($x1 - $x0) / 2)) * $t)) $y
                }
                [Win]::mouse_event([Win]::LUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 300
            }
            default { throw "no such step: $($a[0])" }
        }
    }

    Start-Sleep -Milliseconds 500
    Save-Shot $Shot
    if ($checks.Count) {
        Write-Output '--- checks ---'
        $checks | Write-Output
    }
    if ($failed) { throw "${Shot}: one or more checks failed" }
}
finally {
    if (-not $Keep -and -not $proc.HasExited) { $proc.Kill(); $proc.WaitForExit(3000) }
    if (Test-Path $stash) { Move-Item $stash $saved -Force }
    if (Test-Path $recStash) {
        if (Test-Path $rec) { Remove-Item $rec -Recurse -Force }
        Move-Item $recStash $rec -Force
    }
}
