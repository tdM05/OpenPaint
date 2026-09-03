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
    hold X1 Y1 X2 Y2    press, wait for the workspace to arm, then move and release -- what
                        rearranging a panel is; `drag` moves too soon to ever arm one
    holdat X Y          press and hold in one place, which asks a panel for its settings
    key NAME            a key by name: b, e, f2, f3, ctrl+z, escape, enter
    type TEXT           the text, as typing
    wait MS             let the app catch up
    path X1 Y1 X2 Y2 ...  a press, a walk through every point in turn, and a release -- what a
                        freehand lasso is. `drag` walks a straight line, which is a polygon with
                        no area and therefore not a test of the lasso at all.
    middle X1 Y1 X2 Y2  a middle-button drag, which is how the canvas is panned
    holding KEY STEP    hold space, alt, ctrl or shift down and do one step inside it -- what
                        space-to-pan and alt-click-to-pick need, and what `key` cannot say
    absent NAME         that control is NOT on screen -- the assertion for every rule about
                        hiding a command that would be refused. Note that it passes trivially if
                        the panel holding it is not showing either, so open the menu first.
    ink X1 Y1 X2 Y2 N   at least N dark pixels in that box of the page (N=0 means none at all)
    wrote PATH [WxH]    a PNG exists on disk, of that size if one is given -- the only
                        step that looks outside the application

    tab NAME            bring that panel's tab to the front
    holdtab NAME        hold that panel's tab, which asks it for its settings
    holdtab NAME DX DY  ...and then carry it that far, which is how a window is taken
    dragtab NAME DX DY  take that tab and carry it that far
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
    # The **client** size, which is what every coordinate in a scenario is measured in. The
    # window is grown by its frame to reach it; see the convergence below.
    [int]$Width = 1264,
    [int]$Height = 781,
    # The display scale the scenarios are calibrated against. See the check after the first frame.
    [double]$Scale = 0,
    # Where to put the window, in virtual-screen pixels. **Which display this lands on decides the
    # scale factor**, and the scale factor decides the size of the workspace in the units panels
    # are laid out in -- so on a machine with two displays of different scales, this is how the run
    # is put on the one the scenarios were measured on. Negative is normal: a display to the left
    # of the primary starts at a negative x.
    [int]$X = 60,
    [int]$Y = 60,
    [int]$Settle = 3000,
    [switch]$Keep,
    [switch]$KeepWorkspace,
    [string]$PlantRecovery = ''
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
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint from, uint to, bool attach);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
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
    public const uint MDOWN = 0x0020, MUP = 0x0040;
    // A key that stays down while something else happens. `SendKeys` cannot express that: it
    // sends whole keystrokes, so space-to-pan and Alt+click -- both of which are a key held
    // *across* a drag or a press -- were simply not reachable.
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extra);
    public const uint KEYUP = 0x0002;
    public const byte VK_SPACE = 0x20, VK_MENU = 0x12, VK_CONTROL = 0x11, VK_SHIFT = 0x10;
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
        // **The centre of the pixel, rounded, not its corner truncated.**
        //
        // The coordinate is normalised across the *virtual desktop*, so its resolution depends on
        // how many displays are attached: one 2560-wide screen gives 25 steps per pixel, and two
        // screens 4480 wide give 14. Truncating the corner then lands on the pixel before the one
        // that was asked for often enough to matter -- `select.txt` came back with a selection
        // whose right edge was 1168 where it had measured 1169, and nothing about the application
        // had changed. Half a pixel in, rounded to nearest, hits the pixel that was named.
        uint nx = (uint)Math.Round(((double)(x - vx) + 0.5) * 65535.0 / w);
        uint ny = (uint)Math.Round(((double)(y - vy) + 0.5) * 65535.0 / h);
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

# **Put back anything a previous run left aside, before setting anything aside again.**
#
# The `finally` at the foot of this script restores the artist's workspace, brushes and recovery
# copies whatever happens *inside* the run -- but not if the run is killed from outside, which is
# what happens when a sweep is interrupted. Their files then sit in a `.driving` stash, the live
# ones are missing, and the next run fails on "cannot create a file when that file already exists"
# with the artist's work still stranded. Which is exactly the kind of harm this stashing exists to
# prevent, arrived at from the other direction.
#
# So a stash that is already there is treated as evidence of an interrupted run and restored first.
# It is never evidence of anything else: nothing but this script writes those names.
# **Getting rid of what this run made, without ever stranding what the artist had.**
#
# The application writes a recovery copy into its own folder at start-up and holds the file open,
# and killing it does not always let go before the next line runs. Deleting that folder then throws
# -- and the throw was inside the very block that puts the artist's folder back, so a locked file
# of ours left their work in a `.driving` stash. The harm this stashing exists to prevent, reached
# by a third route.
#
# So: try a few times, and if the lock outlasts us, shove it aside under a name nothing looks at
# rather than give up. Putting theirs back is the part that must not fail.
function Discard([string]$path) {
    if (-not (Test-Path $path)) {
        return
    }
    for ($i = 0; $i -lt 12; $i++) {
        try {
            Remove-Item $path -Recurse -Force -ErrorAction Stop
            return
        } catch {
            Start-Sleep -Milliseconds 250
        }
    }
    # Still held. Out of the way is as good as gone for our purposes, and it can be swept up later.
    try {
        Move-Item $path "$path.stuck-$(Get-Random)" -Force -ErrorAction Stop
    } catch {
        Write-Output "could not clear $path -- the artist's copy is put back beside it"
    }
}

function Restore-Stash([string]$live, [string]$stash) {
    if (-not (Test-Path $stash)) {
        return
    }
    Write-Output "putting back what an interrupted run left aside: $live"
    Discard $live
    Move-Item $stash $live -Force
}

$root = Split-Path -Parent $PSScriptRoot
# Its own build directory, so driving the app never fights the copy the artist has open --
# Windows locks a running exe, and killing theirs to test mine is not a trade to make.
$exe = Join-Path $root 'target\drive-build\release\openpaint.exe'
if (-not (Test-Path $exe)) {
    throw "no binary at $exe -- cargo build --release --target-dir target/drive-build first"
}
$outDir = Join-Path $root 'target\drive'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# **Refuse to run while the artist has OpenPaint open.**
#
# Everything below moves their workspace, brushes, theme and recovery copies aside for the length
# of the run and puts them back at the end. That is safe when nothing else is using them and
# actively dangerous when something is: a running OpenPaint holds the path of its own recovery copy
# for as long as it lives, so while the folder is swapped its autosaves land in the run's folder
# and are thrown away with it. For those minutes the work on their screen has no recovery copy at
# all, and a crash would take it.
#
# It also explains two things that looked like harness faults and were not: a recovery file that
# could not be deleted because their application had it open, and one that kept reappearing under
# the same name because it was theirs and still being written.
#
# Their copy runs from `target\release`; this launches `target\drive-build\release`, so the two are
# told apart by path rather than by name. Never kill it -- that is not a trade to make on somebody
# else's behalf, and the whole point of the separate build directory was to avoid needing to.
$theirs = @(Get-Process openpaint -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path -ne $exe })
if ($theirs.Count) {
    throw ("OpenPaint is already running (pid $($theirs[0].Id), $($theirs[0].Path)). " +
           'Close it before driving: this moves the workspace, brushes, theme and recovery copies ' +
           'aside, and a running copy would lose its autosaves into the gap.')
}

# A fresh workspace every run, so a picture is of the code and not of whatever was saved last.
# `-KeepWorkspace` is for the one thing that needs the opposite: proving a saved one still loads.
$saved = Join-Path $env:LOCALAPPDATA 'OpenPaint' | Join-Path -ChildPath 'workspace.json'
$stash = "$saved.driving"
Restore-Stash $saved $stash
if (-not $KeepWorkspace -and (Test-Path $saved)) { Move-Item $saved $stash -Force }

# The saved brushes, set aside for the length of the run. They are an app resource in the same
# directory as everything else here, a run can create and delete them (Save brush / Forget), and a
# harness that mis-resolves one name can wipe the lot -- which is not a hypothetical, it happened.
# Nothing driven here is allowed to cost the artist a brush they made.
$brushes = Join-Path $env:LOCALAPPDATA 'OpenPaint' | Join-Path -ChildPath 'brushes.json'
$brushStash = "$brushes.driving"
Restore-Stash $brushes $brushStash
if (Test-Path $brushes) { Move-Item $brushes $brushStash -Force }

# The look, for the same reason and with a second one behind it: a run that cycles the theme or
# picks an icon set writes it here, and the next run then starts in whatever the last one left.
# One scenario's choice leaked into another's assertion, which is a suite that is not repeatable.
$look = Join-Path $env:LOCALAPPDATA 'OpenPaint' | Join-Path -ChildPath 'theme.json'
$lookStash = "$look.driving"
Restore-Stash $look $lookStash
if (Test-Path $look) { Move-Item $look $lookStash -Force }

# **And the artist's recovered work is set aside, never answered.** A crashed session leaves a
# file here and the app opens asking what to do with it. The only two answers are Recover and
# Discard, and Discard destroys unsaved work that is not mine to destroy -- so the run never sees
# the question. Put back in `finally`, whatever happens.
$rec = Join-Path $env:LOCALAPPDATA 'OpenPaint' | Join-Path -ChildPath 'recovery'
$recStash = "$rec-driving"
Restore-Stash $rec $recStash
if (Test-Path $rec) { Move-Item $rec $recStash -Force }

# **A recovery copy of our own, when a run is about to test the prompt.**
# The artist's is safely aside by now, so what the application finds here is a file this run put
# there and is free to answer either way. A recovery copy is an ordinary document with one extra
# row in its `meta` table (`autosave::IS_RECOVERY`) -- see the module header, which says so on
# purpose so that the recovery path is the same code as loading anything else.
if ($PlantRecovery) {
    if (-not (Test-Path $PlantRecovery)) { throw "no document at $PlantRecovery to plant" }
    New-Item -ItemType Directory -Force -Path $rec | Out-Null
    $planted = Join-Path $rec 'planted.openpaint'
    Copy-Item -LiteralPath $PlantRecovery -Destination $planted -Force
    # The marker goes in through the same library the application reads it with, rather than
    # through an sqlite3.exe no Windows box is guaranteed to have.
    $marker = Join-Path $root 'target' | Join-Path -ChildPath 'drive-build' |
        Join-Path -ChildPath 'release' | Join-Path -ChildPath 'examples' |
        Join-Path -ChildPath 'mark-recovery.exe'
    if (-not (Test-Path $marker)) {
        throw "no $marker -- cargo build --release --target-dir target/drive-build -p openpaint-file --example mark-recovery"
    }
    & $marker $planted
    if ($LASTEXITCODE -ne 0) { throw 'could not mark the planted copy as a recovery' }
}

# The app writes where every control landed, once per frame, and the name-based steps read it.
$atlas = Join-Path $outDir "$Shot.atlas"
$env:OPENPAINT_CONTROLS = $atlas
$env:OPENPAINT_TRACE_INPUT = '1'
# And what the application currently is, so a step can assert on the consequence of a press
# rather than on a picture of one.
$state = Join-Path $outDir "$Shot.now"
$env:OPENPAINT_STATE = $state

# When this run began. Nothing of the artist's can be newer than this -- they are not at the
# machine, and their folder is set aside before the application is even started -- so it is a safe
# line to sweep behind. See the `finally` at the foot of this file.
$startedAt = Get-Date

$log = Join-Path $outDir "$Shot.log"
$proc = Start-Process -FilePath $exe -PassThru -RedirectStandardOutput $log `
    -RedirectStandardError (Join-Path $outDir "$Shot.err")
try {
    Start-Sleep -Milliseconds $Settle
    $h = [Win]::AppWindow($proc.Id)
    if ($h -eq [IntPtr]::Zero) { throw 'the app window never appeared' }
    # A known size and place, so a coordinate in a script means the same thing every run.
    [void][Win]::MoveWindow($h, $X, $Y, $Width, $Height, $true)

    # **The window has to actually be in front, and the run must stop if it is not.**
    # `SetForegroundWindow` is refused when the calling process does not already own the
    # foreground, and it fails quietly. Keys then go wherever the focus happens to be -- another
    # application, this console -- and the run reports the application as ignoring every shortcut
    # while typing into something else. Which is both a false result and a way to do real damage.
    [void][Win]::ShowWindow($h, 9)   # SW_RESTORE
    $mine = [Win]::GetCurrentThreadId()
    $owner = [uint32]0
    $theirs = [Win]::GetWindowThreadProcessId($h, [ref]$owner)
    for ($i = 0; $i -lt 8; $i++) {
        # Attaching to the window's input queue is what lifts the refusal: two threads sharing an
        # input state may hand the foreground to each other.
        [void][Win]::AttachThreadInput($mine, $theirs, $true)
        [void][Win]::BringWindowToTop($h)
        [void][Win]::SetForegroundWindow($h)
        [void][Win]::AttachThreadInput($mine, $theirs, $false)
        if ([Win]::GetForegroundWindow() -eq $h) { break }
        # The call is refused outright when the terminal this runs from owns the foreground, and
        # it fails silently. A press does what the call may not: Windows always gives the
        # foreground to the window you actually clicked. So click the title bar -- which belongs
        # to Windows, not to the application, so nothing in the application is pressed by it.
        $t = New-Object Win+POINT
        [void][Win]::ClientToScreen($h, [ref]$t)
        $bar = [int]((60 + $t.Y) / 2)
        [Win]::MoveTo($t.X + 120, $bar)
        Start-Sleep -Milliseconds 60
        [Win]::mouse_event([Win]::LDOWN, 0, 0, 0, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 60
        [Win]::mouse_event([Win]::LUP, 0, 0, 0, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 250
    }
    if ([Win]::GetForegroundWindow() -ne $h) {
        $fg = [Win]::GetForegroundWindow()
        $sb = New-Object System.Text.StringBuilder 256
        [void][Win]::GetClassName($fg, $sb, $sb.Capacity)
        throw ("the app window would not come to the front (the front one is $fg, class " +
               "$($sb.ToString()); the app's is $h) -- keys would go somewhere else")
    }
    Start-Sleep -Milliseconds 800

    # Steps are in the *client* area's coordinates, so they mean the same thing whatever the title
    # bar and borders happen to be.
    $c = New-Object Win+RECT
    [void][Win]::GetClientRect($h, [ref]$c)
    $o = New-Object Win+POINT
    [void][Win]::ClientToScreen($h, [ref]$o)
    $ox = $o.X
    $oy = $o.Y

    # **The size is the *client* size, and it is converged on rather than asked for.**
    #
    # `MoveWindow` sets the outer size, and what a scenario lives in is the client area -- so the
    # window frame sits between the two, and the frame is not the same on every display or at
    # every scale. Asking for a 2200x1450 window gave a 2178x1394 client on one display and
    # 2184x1411 on another, and six pixels of width is enough to move where a dragged selection
    # edge lands: `select.txt` came back with a right edge of 1168 where it had measured 1169.
    #
    # So: measure the client, move the outer edge by the difference, and repeat. Three passes is
    # plenty -- the frame is constant for a given window, so the first correction is usually
    # exact.
    #
    # **The size has to stick, and the run must stop if it does not.**
    #
    # Every coordinate in every scenario is in this client area, so a window that came out a
    # different size does not make a run slightly wrong -- it makes every number in it point
    # somewhere else. From outside that looks like eighteen scenarios failing at once with
    # assertions about ink and layout, which reads as the application having broken overnight.
    #
    # It happened. A second monitor was plugged in, the two displays had different scale factors,
    # and moving the window across triggered a DPI change that winit answered by restoring the
    # application's *own* default size -- 1280x800 -- undoing the `MoveWindow` above. The suite
    # then drove 2200x1450 coordinates into a 1280x800 window and reported the results with total
    # confidence.
    #
    # So: measure, try again, and give up loudly. The tolerance is the window frame, which
    # `MoveWindow` counts and `GetClientRect` does not -- a correctly placed 2200x1450 window has
    # a client area of about 2184x1411 here. Anything further out is a different window from the
    # one the scenes were written against.
    $outerW = $Width
    $outerH = $Height
    for ($try = 0; $try -lt 4; $try++) {
        [void][Win]::GetClientRect($h, [ref]$c)
        if ($c.Right -eq $Width -and $c.Bottom -eq $Height) { break }
        $outerW += $Width - $c.Right
        $outerH += $Height - $c.Bottom
        [void][Win]::MoveWindow($h, $X, $Y, $outerW, $outerH, $true)
        Start-Sleep -Milliseconds 400
    }
    [void][Win]::GetClientRect($h, [ref]$c)
    # **Exactly, and in both directions.** A client larger than asked for is as wrong as one
    # smaller, and larger is the case that actually happened: moved onto a 150% display, winit
    # kept the *logical* size and the client became 3276x2117. An earlier one-sided check waved
    # that through, and eighteen scenarios then described a workspace half again the size of the
    # one they were measured on.
    if ($c.Right -ne $Width -or $c.Bottom -ne $Height) {
        throw ("the client area will not settle at ${Width}x${Height}: it is " +
               "$($c.Right)x$($c.Bottom). Every coordinate in a scenario is in that area, so " +
               'nothing below would mean what it says. This happens when a display of a ' +
               'different scale is attached and the window is restored to its own size; run on ' +
               'the display the suite was calibrated on, or pass -Width and -Height that settle.')
    }
    # **A fresh point.** `ClientToScreen` converts in place, and the one above already holds
    # screen coordinates -- converting it twice adds the window's origin twice, which puts every
    # press about seventy pixels from where the scene said. The window may have been moved again
    # by the retry above, so the origin does have to be read afresh; the point does too.
    $origin = New-Object Win+POINT
    [void][Win]::ClientToScreen($h, [ref]$origin)
    $ox = $origin.X
    $oy = $origin.Y

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
        $twins = @{}
        $panel = ''
        $view = $null
        # The app truncates this at the top of every frame and fills it as the panels draw, so a
        # read can land on the empty moment in between. An empty atlas is a timing accident, not a
        # blank screen, and reporting it as one sends you hunting a bug in the app.
        $lines = @()
        for ($i = 0; $i -lt 30; $i++) {
            if (Test-Path $atlas) {
                $lines = @(Get-Content -LiteralPath $atlas -ErrorAction SilentlyContinue)
            }
            if ($lines.Count) { break }
            Start-Sleep -Milliseconds 60
        }
        foreach ($line in $lines) {
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
                Panel = $panel; View = $view; Label = $f[5]; Id = $f[0]
            }
            # Indexed under four keys, so a step can say the label, the control's own id, or
            # `Panel:label` when two panels both have an "Opacity".
            #
            # **A name that fits two controls is recorded as fitting two**, and pressing it is an
            # error. First-wins looks harmless and is not: the brush panel has a Size slider and a
            # Size response picker, a Flow slider and a Flow response picker, and `brush:flow`
            # quietly resolved to whichever the layout reached first. A run meaning to drag a
            # slider opened a dropdown instead, went on pressing things underneath it, and
            # destroyed the artist's saved brushes. Silently choosing between two controls with the
            # same name is the exact failure the atlas exists to prevent.
            foreach ($k in @($f[5], $f[0], "${panel}:$($f[5])", "${panel}:$($f[0])")) {
                $k = $k.Trim().ToLower()
                if (-not $k) { continue }
                if ($found.ContainsKey($k)) {
                    # Ambiguity is a property of the *key*, not of the control: one control is
                    # filed under four keys and they share one object, so recording it on the
                    # object made every alias of the first control look ambiguous too.
                    if (-not $twins.ContainsKey($k)) { $twins[$k] = @($found[$k].Id) }
                    if ($twins[$k] -notcontains $f[0]) { $twins[$k] += ,$f[0] }
                } else {
                    $found[$k] = $rect
                }
            }
        }
        return @($found, $tabs, $twins)
    }

    # Turn the wheel over a point. Injected like everything else here, because the app only gives
    # the wheel to the panel the pointer is actually over.
    function Wheel-At([int]$x, [int]$y, [int]$notches) {
        Point-At $x $y
        # **Let a frame happen before turning the wheel.** Only the panel under the pointer takes
        # the wheel, and egui decides which that is from the layer under the pointer *as of the
        # last frame* -- the same characteristic `main.rs` names where it declines to zoom. Painting
        # here is demand-driven, so straight after a click the frame that learns where the pointer
        # went may not have happened yet, and the notch is simply lost. It read as panels that
        # scrolled sometimes.
        Start-Sleep -Milliseconds 250
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
        # How far one notch goes is the app's business, so it is measured rather than assumed --
        # and then used to travel. One notch at a time was fine for a layer list and hopeless for
        # the brush panel, whose controls run to four thousand pixels inside a slot two hundred
        # and fifty tall: sixty notches from top to bottom, and a fixed budget of twenty-four
        # reported a control that is perfectly reachable as unreachable.
        $per = 0.0
        for ($try = 0; $try -lt 60; $try++) {
            $r = Rect-Of $name
            $v = $r.View
            if (-not $v) { return $r }
            # **The centre is what a press aims at**, so the centre is what has to be inside.
            # Insisting on the whole box refused controls a hand can hit perfectly well: a menu
            # popup's last item overhangs its own box by two thirds of a point, which nobody can
            # see and which stopped the run dead.
            $mid = $r.Y + [int]($r.H / 2)
            if ($mid -gt $v.Y -and $mid -lt ($v.Y + $v.H)) { return $r }
            $mx = $v.X + [int]($v.W / 2)
            $my = $v.Y + [int]($v.H / 2)
            # How far short, in pixels, and which way.
            $short = if ($mid -ge ($v.Y + $v.H)) {
                $mid - ($v.Y + $v.H - 4)
            } else {
                $mid - ($v.Y + 4)
            }
            if ($per -le 0) {
                # Measure one notch before spending any.
                $was = $v.Scroll
                Wheel-At $mx $my $(if ($short -gt 0) { -1 } else { 1 })
                $now = (Rect-Of $name).View.Scroll
                $per = [Math]::Abs($now - $was)
                if ($per -le 0) {
                    # One more, with a longer settle, before believing it.
                    Start-Sleep -Milliseconds 400
                    Wheel-At $mx $my $(if ($short -gt 0) { -1 } else { 1 })
                    $per = [Math]::Abs((Rect-Of $name).View.Scroll - $was)
                }
                if ($per -le 0) {
                    throw ("'$name' is at $($r.Y)..$($r.Y + $r.H) in $($v.Panel), whose window is " +
                           "$($v.Y)..$($v.Y + $v.H) at scroll $($v.Scroll) of $($v.Tall); one " +
                           'notch moved it nowhere.')
                }
                continue
            }
            $want = [int][Math]::Ceiling([Math]::Abs($short) / $per)
            $go = [Math]::Min($want, 20)
            Wheel-At $mx $my $(if ($short -gt 0) { -$go } else { $go })
        }
        $r = Rect-Of $name
        throw ("'$name' is at $($r.Y)..$($r.Y + $r.H) and will not come into " +
               "$($r.View.Y)..$($r.View.Y + $r.View.H) (scroll $($r.View.Scroll) of $($r.View.Tall))")
    }

    # The rectangle a name means, or a stop. Whole label first, then a label that starts with it --
    # never a random substring, which is how "Size" would have pressed "Size jitter".
    function Rect-Of([string]$name, [switch]$Tab) {
        $a = Read-Atlas
        $table = if ($Tab) { $a[1] } else { $a[0] }
        $k = $name.Trim().ToLower()
        if ($table.ContainsKey($k)) {
            if (-not $Tab -and $a[2].ContainsKey($k)) {
                throw ("'$name' is the name of $($a[2][$k].Count) controls in " +
                       "$($table[$k].Panel) (ids $($a[2][$k] -join ', ')). Say which by id.")
            }
            return $table[$k]
        }
        $near = @($table.Keys | Where-Object { $_.StartsWith($k + ' ') -or $_.StartsWith($k + ':') })
        if ($near.Count -eq 1) { return $table[$near[0]] }
        $what = if ($Tab) { 'tab' } else { 'control' }
        if ($near.Count -gt 1) { throw "'$name' names $($near.Count) ${what}s: $($near -join ', ')" }
        # Only the qualified names, and only from the panel the step probably meant. The full list
        # is two hundred entries and burying the answer in it is how a clear failure reads as noise.
        $prefix = if ($name -match '^([^:]+):') { $Matches[1].ToLower() } else { '' }
        $keys = @($table.Keys | Where-Object { $_ -like '*:*' } | Sort-Object)
        if ($prefix) { $keys = @($keys | Where-Object { $_.StartsWith("${prefix}:") }) }
        $have = ($keys | Select-Object -First 60) -join ', '
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
    # **Last run's evidence goes first, before this one can be mistaken for it.**
    #
    # The checks file used to be written only on the way out of a successful run, so a run that
    # stopped left the previous one's on disk -- and everything that read it afterwards described
    # a run that never happened. It is written on failure now as well, but a throw from outside
    # the step loop still bypasses that, so the file is also removed here: absent evidence is
    # honest, and stale evidence is not.
    Remove-Item -LiteralPath (Join-Path $outDir "$Shot.checks") -Force -ErrorAction SilentlyContinue
    $checks = New-Object System.Collections.ArrayList

    # `about KEY VALUE` -- the same check for a number that came off a drag. A slider set by
    # dragging a pointer across a panel lands on 0.501, not 0.500, and a harness that calls that a
    # failure trains whoever reads it to ignore failures.
    function Expect-Near([string]$key, [double]$want, [double]$tol = 0.02) {
        $got = $null
        for ($i = 0; $i -lt 20; $i++) {
            $got = (Read-State)[$key]
            if ($null -ne $got -and [Math]::Abs([double]$got - $want) -le $tol) { break }
            Start-Sleep -Milliseconds 100
        }
        if ($null -ne $got -and [Math]::Abs([double]$got - $want) -le $tol) {
            [void]$checks.Add("  ok    $key = $got (wanted about $want)")
        } else {
            [void]$checks.Add("  FAIL  ${key}: wanted about $want, got '$got'")
            $script:failed = $true
        }
    }

    # `about KEY a b c d [TOL]` -- every number of a multi-field state value, each within TOL.
    #
    # **Because a bound taken from where a pointer landed is exact to a pixel and no further.**
    # The pointer is injected in coordinates normalised across the whole virtual desktop, so its
    # resolution depends on how many displays are attached -- 25 steps per pixel on one screen,
    # 14 on two. A lasso's right edge came back as 1168 where it had been measured at 1169, on an
    # application that had not changed and a client area of exactly the calibrated size. Asserting
    # such a number exactly is asserting the harness's aim, not the selection.
    #
    # Numbers that are *not* pointer-derived stay on `expect`, which is exact: a page size, a
    # layer count and an undo depth have no business being approximate.
    function Expect-Near-Many([string]$key, [double[]]$want, [double]$tol = 1) {
        $got = $null
        $ok = $false
        for ($i = 0; $i -lt 20; $i++) {
            $got = (Read-State)[$key]
            if ($null -ne $got) {
                $nums = @($got.Trim() -split '\s+' | ForEach-Object { [double]$_ })
                if ($nums.Count -eq $want.Count) {
                    $ok = $true
                    for ($j = 0; $j -lt $want.Count; $j++) {
                        if ([Math]::Abs($nums[$j] - $want[$j]) -gt $tol) { $ok = $false }
                    }
                }
            }
            if ($ok) { break }
            Start-Sleep -Milliseconds 100
        }
        $wanted = $want -join ' '
        if ($ok) {
            [void]$checks.Add("  ok    $key = $got (wanted about $wanted)")
        } else {
            [void]$checks.Add("  FAIL  ${key}: wanted about $wanted, got '$got'")
            $script:failed = $true
        }
    }

    function Expect-State([string]$key, [string]$want) {
        # Given a frame or two: a press is answered on the next paint, not on the release.
        $got = $null
        for ($i = 0; $i -lt 20; $i++) {
            $got = (Read-State)[$key]
            if ($null -ne $got) { $got = $got.Trim() }
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

    # **Whether there is actually ink there.**
    # Undo depth says an operation was recorded; it does not say the brush put anything on the
    # page. The two came apart once already -- nine hundred tests passed while the brush painted
    # nothing -- so a scenario that claims a stroke happened should be able to look. Counts the
    # pixels in a box that are darker than the page, sampling every third one because this is
    # `GetPixel` in PowerShell and the whole point is that it stays cheap enough to use.
    function Count-Ink([int]$x0, [int]$y0, [int]$x1, [int]$y1) {
        Start-Sleep -Milliseconds 300
        $b = New-Object System.Drawing.Bitmap ($x1 - $x0), ($y1 - $y0)
        $gg = [System.Drawing.Graphics]::FromImage($b)
        $gg.CopyFromScreen(($ox + $x0), ($oy + $y0), 0, 0, $b.Size)
        $n = 0
        for ($yy = 0; $yy -lt $b.Height; $yy += 3) {
            for ($xx = 0; $xx -lt $b.Width; $xx += 3) {
                $c = $b.GetPixel($xx, $yy)
                # The page is very nearly white and every mark made here is very nearly black, so
                # one threshold in the middle separates them without needing to know either.
                if (([int]$c.R + [int]$c.G + [int]$c.B) -lt 380) { $n++ }
            }
        }
        $gg.Dispose(); $b.Dispose()
        return $n
    }

    function Click-At([int]$x, [int]$y, [switch]$Right) {
        Point-At $x $y
        Start-Sleep -Milliseconds 60
        [Win]::mouse_event($(if ($Right) { [Win]::RDOWN } else { [Win]::LDOWN }), 0, 0, 0, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 90
        [Win]::mouse_event($(if ($Right) { [Win]::RUP } else { [Win]::LUP }), 0, 0, 0, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 300
    }

    # A step that fails says why, and shows what it was looking at. A message about a control
    # that would not come into view is a guess until you can see the screen it was on.
    # **The scenarios are calibrated against one display, and this is where that is checked.**
    #
    # Panels are laid out in *logical* units, so the scale factor decides how much workspace there
    # is: the same 2200x1450 window is 1452x929 at scale 1.5 and 2184x1411 at scale 1. Tabs that
    # wrap on one do not wrap on the other, a floating window's default size differs, and a scene
    # that names a tab position or a window rectangle is describing a screen it is not on.
    #
    # Without this, plugging in a second monitor made eighteen scenarios fail at once with
    # assertions about ink and layout -- which reads as the application having broken overnight,
    # and cost an hour before anyone looked at the scale factor.
    if ($Scale -gt 0) {
        $got = [double](Read-State)['scale']
        if ([Math]::Abs($got - $Scale) -gt 0.01) {
            throw ("this run is at display scale $got and the scenarios are written for $Scale. " +
                   'Panels are laid out in logical units, so at another scale the workspace is a ' +
                   'different size and the tabs, windows and canvas are not where the scenes say. ' +
                   'Run on the display the suite was calibrated on, or pass -Scale to say that ' +
                   'the numbers have been recalibrated for this one.')
        }
    }

    $script:onScreen = $true
    # One step, as a function, so a step can contain a step -- which `holding` needs: it presses a
    # key, does one thing, and lets go.
    function Do-Step([string]$step) {
        $a = $step.Trim() -split '\s+'
        # A control's name has spaces in it -- "Merge down", "Clip to the layer below" -- so the
        # name is everything after the verb, not the next token.
        $rest = $step.Trim().Substring($a[0].Length).Trim()
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
            'hold'  {
                # `hold X1 Y1 X2 Y2` -- press, wait for the workspace to arm, then move and let go.
                # Nothing rearranges the workspace until the pointer has been held still on it for
                # `panel_drag::HOLD_MS` (320 ms), which is the whole point: a press that has only
                # been waiting has taken nothing, so a stroke that begins on a panel is not a
                # rearrangement. `drag` moves at once and therefore can never arm one.
                Point-At $a[1] $a[2]
                Start-Sleep -Milliseconds 120
                [Win]::mouse_event([Win]::LDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 600
                $steps = 24
                for ($i = 1; $i -le $steps; $i++) {
                    $t = $i / $steps
                    Point-At ([int]([int]$a[1] + ([int]$a[3] - [int]$a[1]) * $t)) `
                             ([int]([int]$a[2] + ([int]$a[4] - [int]$a[2]) * $t))
                }
                Start-Sleep -Milliseconds 250
                [Win]::mouse_event([Win]::LUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 500
            }
            'holdtab' {
                # `holdtab NAME [DX DY]` -- hold that panel's tab, and then carry it that far.
                #
                # Without the offsets it holds in place, which is what asks a panel for its
                # settings. With them it holds *and then* moves, which is the gesture that takes a
                # window: nothing rearranges until the pointer has been still for `HOLD_MS`.
                #
                # **The named form of `holdat`, and the reason it exists.** A tab's position
                # depends on how wide every label before it measures and on how the panel column
                # is split, so a scenario that holds a raw coordinate is holding whatever ends up
                # there. `windows-apart.txt` did, and when the tabs moved it silently began
                # floating History where it said Pages -- passing steps that meant something else,
                # which is worse than failing. Names come out of the same atlas `tab` reads.
                $moved = $a.Count -ge 4
                $name = if ($moved) { $rest -replace '\s+\S+\s+\S+$', '' } else { $rest }
                $r = Rect-Of $name -Tab
                $x = $r.X + [int]($r.W / 2)
                $y = $r.Y + [int]($r.H / 2)
                Point-At $x $y
                Start-Sleep -Milliseconds 120
                [Win]::mouse_event([Win]::LDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 700
                if ($moved) {
                    $steps = 24
                    for ($i = 1; $i -le $steps; $i++) {
                        $t = $i / $steps
                        Point-At ([int]($x + [int]$a[-2] * $t)) ([int]($y + [int]$a[-1] * $t))
                    }
                    Start-Sleep -Milliseconds 250
                }
                [Win]::mouse_event([Win]::LUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 400
            }
            'dragtab' {
                # `dragtab NAME DX DY` -- take that tab and carry it by that much.
                #
                # Named for the same reason, and offset rather than absolute because what a drag
                # of a window means is "this far", not "to there".
                $name = $rest -replace '\s+\S+\s+\S+$', ''
                $r = Rect-Of $name -Tab
                $x = $r.X + [int]($r.W / 2)
                $y = $r.Y + [int]($r.H / 2)
                Point-At $x $y
                [Win]::mouse_event([Win]::LDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 60
                $steps = 30
                for ($i = 1; $i -le $steps; $i++) {
                    $t = $i / $steps
                    Point-At ([int]($x + [int]$a[-2] * $t)) ([int]($y + [int]$a[-1] * $t))
                }
                [Win]::mouse_event([Win]::LUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 400
            }
            'holdat' {
                # `holdat X Y` -- press and hold in one place, which is what asks a panel for its
                # settings, and let go. Prefer `holdtab` where the thing being held is a tab.
                Point-At $a[1] $a[2]
                Start-Sleep -Milliseconds 120
                [Win]::mouse_event([Win]::LDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 700
                [Win]::mouse_event([Win]::LUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 400
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
                # Modifiers are peeled off by name, not looked up in a table of chords. The
                # table was the bug: `ctrl+y` was not in it, so the fallback typed the seven
                # letters c-t-r-l-plus-y into the app and the run reported redo as broken.
                $parts = $a[1].ToLower() -split '\+'
                $key = $parts[-1]
                $mods = ''
                # `$parts[0..-1]` is the whole array in PowerShell, not an empty one, so a key with
                # no modifiers read its own name as a modifier.
                $before = if ($parts.Count -gt 1) { $parts[0..($parts.Count - 2)] } else { @() }
                foreach ($m in $before) {
                    switch ($m) {
                        'ctrl'  { $mods += '^' }
                        'shift' { $mods += '+' }
                        'alt'   { $mods += '%' }
                        default { throw "no such modifier: $m" }
                    }
                }
                $named = @{
                    'escape' = '{ESC}'; 'enter' = '{ENTER}'; 'delete' = '{DEL}'
                    'tab' = '{TAB}'; 'space' = ' '; 'backspace' = '{BS}'
                    'left' = '{LEFT}'; 'right' = '{RIGHT}'; 'up' = '{UP}'; 'down' = '{DOWN}'
                    'home' = '{HOME}'; 'end' = '{END}'
                    # SendKeys reserves the brackets, so the literal ones are written in braces.
                    '[' = '{[}'; ']' = '{]}'
                }
                $body = if ($named.ContainsKey($key)) { $named[$key] }
                        elseif ($key -match '^f([1-9]|1[0-2])$') { "{$($key.ToUpper())}" }
                        elseif ($key.Length -eq 1) { $key }
                        else { throw "no such key: $key" }
                $k = $mods + $body
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
            'about' {
                # `about KEY VALUE [TOLERANCE]`. A log slider set by fraction lands where the
                # curve puts it, so the scene says how close is close enough.
                #
                # `about KEY a b c d` -- a state value of several numbers, each within a pixel.
                # See `Expect-Near-Many`: a bound derived from where the pointer landed is exact
                # to a pixel and no further.
                if ($a.Count -gt 4) {
                    Expect-Near-Many $a[1] ([double[]]($a[2..($a.Count - 1)]))
                } elseif ($a.Count -eq 4) {
                    Expect-Near $a[1] ([double]$a[2]) ([double]$a[3])
                } else {
                    Expect-Near $a[1] ([double]$a[2])
                }
            }
            'state' {
                # Print the whole of it, for a step that is exploring rather than asserting.
                Write-Output "--- state: $($a[1]) ---"
                if (Test-Path $state) { Get-Content -LiteralPath $state | Write-Output }
            }
            'shot'  { Save-Shot $a[1] }
            'tab'   {
                $r = Rect-Of $rest -Tab
                Click-At ($r.X + [int]($r.W / 2)) ($r.Y + [int]($r.H / 2))
                Start-Sleep -Milliseconds 200
            }
            'press' {
                $r = Bring-Into-View $rest
                Click-At ($r.X + [int]($r.W / 2)) ($r.Y + [int]($r.H / 2))
            }
            'rpress' {
                $r = Bring-Into-View $rest
                Click-At ($r.X + [int]($r.W / 2)) ($r.Y + [int]($r.H / 2)) -Right
            }
            'pressat' {
                # `pressat NAME DX DY` -- a press at an offset from the control's top-left corner,
                # for a `Custom` that draws several things inside one rectangle. The palette is a
                # grid of chips in a box far wider than the chips, so the middle of that box is
                # empty space: pressing it does nothing, correctly, and a scene that means "the
                # first swatch" has to say where the first swatch is.
                $r = Bring-Into-View ($rest -replace '\s+\S+\s+\S+$', '')
                Click-At ($r.X + [int]$a[-2]) ($r.Y + [int]$a[-1])
            }
            'rpressat' {
                $r = Bring-Into-View ($rest -replace '\s+\S+\s+\S+$', '')
                Click-At ($r.X + [int]$a[-2]) ($r.Y + [int]$a[-1]) -Right
            }
            'ink' {
                # `ink X1 Y1 X2 Y2 MIN` -- at least MIN dark pixels in that box of the page.
                # `ink X1 Y1 X2 Y2 0` asserts the opposite: that the box is clean.
                $want = [int]$a[5]
                $got = Count-Ink ([int]$a[1]) ([int]$a[2]) ([int]$a[3]) ([int]$a[4])
                if (($want -eq 0 -and $got -eq 0) -or ($want -gt 0 -and $got -ge $want)) {
                    [void]$checks.Add("  ok    ink in $($a[1]),$($a[2])..$($a[3]),$($a[4]) = $got")
                } else {
                    [void]$checks.Add("  FAIL  ink in $($a[1]),$($a[2])..$($a[3]),$($a[4]): wanted $want, got $got")
                    $script:failed = $true
                }
            }
            'absent' {
                # `absent NAME` -- that control is NOT on screen. **The harness had no way to say
                # this**, so every rule about hiding a command that would be refused -- Delete on
                # the only layer, Merge down at the bottom, the selection commands with nothing
                # selected -- was enforced by unit tests and by nothing that had ever looked at
                # the running application. A scenario could narrate that an item disappears and
                # then not check it, which one did.
                $gone = $false
                try { [void](Rect-Of $rest) } catch { $gone = $true }
                if ($gone) {
                    [void]$checks.Add("  ok    '$rest' is not on screen")
                } else {
                    [void]$checks.Add("  FAIL  '$rest' is on screen and should not be")
                    $script:failed = $true
                }
            }
            'wrote' {
                # `wrote PATH WIDTHxHEIGHT` -- a file exists on disk, and is a PNG of that size.
                #
                # **The only step that looks outside the application.** Everything else asks the
                # app what it believes; an export is the one operation whose whole purpose is a
                # file somebody else will open, and an app that says "Exported" while writing
                # nothing would pass every other kind of check here. The size is read out of the
                # PNG header, so a file that exists and is empty, or is the wrong page, fails.
                # The size is optional: a file whose dimensions come from a slider cannot be
                # predicted by a scene, because a slider is dragged to a pixel and not typed.
                $want = if ($a.Count -ge 3) { $a[2] } else { $null }
                $file = $a[1]
                if (-not (Test-Path -LiteralPath $file)) {
                    [void]$checks.Add("  FAIL  no file at $file")
                    $script:failed = $true
                } else {
                    $bytes = [System.IO.File]::ReadAllBytes($file)
                    if ($bytes.Length -lt 24 -or $bytes[1] -ne 0x50 -or $bytes[2] -ne 0x4e) {
                        [void]$checks.Add("  FAIL  $file is not a PNG")
                        $script:failed = $true
                    } else {
                        # IHDR: width and height as big-endian 32-bit, at bytes 16 and 20.
                        $w = ([int]$bytes[16] -shl 24) -bor ([int]$bytes[17] -shl 16) -bor
                             ([int]$bytes[18] -shl 8) -bor [int]$bytes[19]
                        $h = ([int]$bytes[20] -shl 24) -bor ([int]$bytes[21] -shl 16) -bor
                             ([int]$bytes[22] -shl 8) -bor [int]$bytes[23]
                        $got = "${w}x${h}"
                        if ($null -eq $want) {
                            [void]$checks.Add("  ok    $file is a PNG of $got")
                        } elseif ($got -eq $want) {
                            [void]$checks.Add("  ok    $file is a PNG of $got")
                        } else {
                            [void]$checks.Add("  FAIL  $file is $got, wanted $want")
                            $script:failed = $true
                        }
                    }
                }
            }
            'path' {
                # `path X1 Y1 X2 Y2 X3 Y3 ...` -- a press, a walk through every point in turn, and
                # a release. **A `drag` is a straight line**, so a lasso drawn with it is a
                # degenerate polygon and every freehand selection this suite made was of a shape
                # with no area. A corner is the whole difference between testing the lasso and
                # testing the code that copes with somebody tapping.
                $n = ($a.Count - 1) / 2
                if ($a.Count -lt 7 -or (($a.Count - 1) % 2) -ne 0) {
                    throw 'path wants at least three x,y pairs'
                }
                Point-At ([int]$a[1]) ([int]$a[2])
                Start-Sleep -Milliseconds 80
                [Win]::mouse_event([Win]::LDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 60
                for ($k = 1; $k -lt $n; $k++) {
                    $fx = [int]$a[($k * 2) - 1]
                    $fy = [int]$a[$k * 2]
                    $tx = [int]$a[($k * 2) + 1]
                    $ty = [int]$a[($k * 2) + 2]
                    for ($i = 1; $i -le 14; $i++) {
                        $t = $i / 14
                        Point-At ([int]($fx + ($tx - $fx) * $t)) ([int]($fy + ($ty - $fy) * $t))
                    }
                }
                Start-Sleep -Milliseconds 120
                [Win]::mouse_event([Win]::LUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 350
            }
            'middle' {
                # `middle X1 Y1 X2 Y2` -- a middle-button drag, which is how the canvas is panned
                # without touching the keyboard.
                Point-At $a[1] $a[2]
                Start-Sleep -Milliseconds 80
                [Win]::mouse_event([Win]::MDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 60
                for ($i = 1; $i -le 20; $i++) {
                    $t = $i / 20
                    Point-At ([int]([int]$a[1] + ([int]$a[3] - [int]$a[1]) * $t)) `
                             ([int]([int]$a[2] + ([int]$a[4] - [int]$a[2]) * $t))
                }
                [Win]::mouse_event([Win]::MUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 300
            }
            'holding' {
                # `holding KEY <step...>` -- hold a key down, do one step, let go. Space+drag pans;
                # Alt+click picks the colour under the pointer. Neither can be said with `key`,
                # which sends a whole keystroke and is over before the drag begins.
                $vk = switch ($a[1].ToLower()) {
                    'space' { [Win]::VK_SPACE }
                    'alt'   { [Win]::VK_MENU }
                    'ctrl'  { [Win]::VK_CONTROL }
                    'shift' { [Win]::VK_SHIFT }
                    default { throw "cannot hold $($a[1])" }
                }
                [Win]::keybd_event($vk, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 250
                try {
                    $inner = $step.Trim().Substring($a[0].Length).Trim()
                    $inner = $inner.Substring($a[1].Length).Trim()
                    Do-Step $inner
                } finally {
                    [Win]::keybd_event($vk, 0, [Win]::KEYUP, [IntPtr]::Zero)
                    Start-Sleep -Milliseconds 250
                }
            }
            'wheel' {
                # `wheel X Y N` -- N notches at a point, for testing the wheel itself.
                Wheel-At ([int]$a[1]) ([int]$a[2]) ([int]$a[3])
            }
            'slide' {
                # A slider is dragged, not clicked: a press sets the value under the pointer and a
                # drag is what a hand does, and only one of the two exercises the tracking code.
                $r = Bring-Into-View ($rest -replace '\s+\S+$', '')
                $y = $r.Y + [int]($r.H / 2)
                $pad = 8
                $x0 = $r.X + $pad
                $x1 = $r.X + $r.W - $pad
                $to = [int]($x0 + ($x1 - $x0) * [double]$a[-1])
                # **Let a frame happen before pressing, as well as before letting go.** A press is
                # attributed to the control the application believes is under the pointer, and it
                # learns that from a frame. Painting here is demand-driven, so straight after
                # `Bring-Into-View` has scrolled the panel the frame that settles the new positions
                # may not have happened yet -- and the press then lands on where the control used
                # to be. It showed up as a slider that moved on its own and not inside a scenario
                # that had scrolled to reach it.
                Point-At ($x0 + [int](($x1 - $x0) / 2)) $y
                Start-Sleep -Milliseconds 200
                [Win]::mouse_event([Win]::LDOWN, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 90
                for ($i = 1; $i -le 12; $i++) {
                    $t = $i / 12.0
                    Point-At ([int]($x0 + ($x1 - $x0) / 2 + ($to - ($x0 + ($x1 - $x0) / 2)) * $t)) $y
                }
                # **Land on the end of the drag before letting go.** A slider follows the pointer
                # while it is latched, so the value is whatever the last pose said -- and releasing
                # in the same breath as the final move let the release be processed first. The
                # slider then stopped wherever the previous sample had reached, which is why the
                # same fraction gave 75 on one run and 132 on the next. An assertion that changes
                # between runs teaches you to ignore assertions.
                Point-At $to $y
                Start-Sleep -Milliseconds 150
                [Win]::mouse_event([Win]::LUP, 0, 0, 0, [IntPtr]::Zero)
                Start-Sleep -Milliseconds 300
            }
            default { throw "no such step: $($a[0])" }
        }
    }

    # **Written whatever happens, including when a step throws.**
    #
    # They used not to be: a run that stopped part-way left the *previous* run's checks on disk,
    # and reading them afterwards described a run that never happened. Two defects were chased
    # for an hour each against stale evidence -- which is a worse failure than the one being
    # chased, because it is invisible.
    function Save-Checks {
        if ($checks.Count) {
            # Beside the pictures, so the result of a run outlives the console it scrolled past.
            $checks | Set-Content -LiteralPath (Join-Path $outDir "$Shot.checks") -Encoding utf8
            Write-Output '--- checks ---'
            $checks | Write-Output
        }
    }

    foreach ($step in $steps) {
        try {
            Do-Step $step
        } catch {
            Save-Shot "$Shot-failed"
            [void]$checks.Add("  STOPPED at: $step")
            Save-Checks
            Write-Output "the step that failed was: $step"
            throw
        }
    }

    Start-Sleep -Milliseconds 500
    Save-Shot $Shot
    Save-Checks
    if ($failed) { throw "${Shot}: one or more checks failed" }
}
finally {
    # Each of these is independent, and each is wrapped: one that throws must not stop the three
    # after it from putting the artist's files back.
    if (-not $Keep -and -not $proc.HasExited) { $proc.Kill(); $proc.WaitForExit(5000) }
    foreach ($pair in @(
            @($stash, $saved),
            @($brushStash, $brushes),
            @($lookStash, $look),
            @($recStash, $rec))) {
        try {
            if (Test-Path $pair[0]) {
                Discard $pair[1]
                Move-Item $pair[0] $pair[1] -Force
            }
        } catch {
            Write-Output "could not put back $($pair[1]): $_"
        }
    }

    # **And nothing this run wrote is left among the artist's recovery copies.**
    # The application holds the path of its own copy for as long as it is alive, and that path is
    # the same string once their folder is back -- so a write that lands after the restore lands in
    # theirs. One did: a 278 KB copy of a test stroke sat in an artist's recovery folder, where the
    # application would have offered it to them as their own unsaved work.
    #
    # Only files newer than this run can possibly be ours, and nothing of theirs can be: their
    # folder was set aside before the application was started.
    try {
        if (Test-Path $rec) {
            Get-ChildItem -LiteralPath $rec -File -ErrorAction SilentlyContinue |
                Where-Object { $_.LastWriteTime -gt $startedAt } |
                ForEach-Object {
                    Write-Output "cleared this run's own recovery copy: $($_.Name)"
                    Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue
                }
        }
    } catch {
        Write-Output "could not sweep this run's recovery copies: $_"
    }
}
