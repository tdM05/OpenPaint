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
  A semicolon-separated script of steps, in **client-area** coordinates:
    move X Y            put the pointer there
    click X Y           press and release there
    right X Y           the other button
    drag X1 Y1 X2 Y2    press, move along the path in steps, release -- what a stroke is
    key NAME            a key by name: b, e, f2, f3, ctrl+z, escape, enter
    wait MS             let the app catch up
#>
[CmdletBinding()]
param(
    [string]$Shot = 'shot',
    [string]$Do = '',
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
    public const uint LDOWN = 0x0002, LUP = 0x0004, RDOWN = 0x0008, RUP = 0x0010;
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

    foreach ($step in ($Do -split ';' | Where-Object { $_.Trim() })) {
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
            'wait'  { Start-Sleep -Milliseconds ([int]$a[1]) }
            default { throw "no such step: $($a[0])" }
        }
    }

    Start-Sleep -Milliseconds 500
    # The client area only: the title bar is Windows' and says nothing about the app.
    [void][Win]::GetClientRect($h, [ref]$c)
    $bmp = New-Object System.Drawing.Bitmap ($c.Right - $c.Left), ($c.Bottom - $c.Top)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($ox, $oy, 0, 0, $bmp.Size)
    $path = Join-Path $outDir "$Shot.png"
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Output $path
}
finally {
    if (-not $Keep -and -not $proc.HasExited) { $proc.Kill(); $proc.WaitForExit(3000) }
    if (Test-Path $stash) { Move-Item $stash $saved -Force }
    if (Test-Path $recStash) {
        if (Test-Path $rec) { Remove-Item $rec -Recurse -Force }
        Move-Item $recStash $rec -Force
    }
}
