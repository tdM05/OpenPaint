# Driving the real application

**What this exists for.** Every other test here tests a *piece*. `cargo test` runs the workspace's
own entry point against synthetic frames; the screenshot tests render panels into a headless GPU
surface. Nine hundred of them passed for a day while the brush painted nothing, because none of
them could open the window. The only thing that finds those bugs is starting the binary, sending it
real input, and looking at what it became — and that is what `tools/drive.ps1` does.

If you are picking this up mid-way: read this file, then `docs/DRIVE_LOG.md`, which is the
checklist and the running list of what driving has found.

## Running it

```powershell
cargo build --release --target-dir target/drive-build -p openpaint-app
& .\tools\drive.ps1 -Shot name -Width 1600 -Height 1000 -Do 'press Add; expect layers 2'
& .\tools\drive.ps1 -Shot name -Script tools\scenes\layers.txt
```

Everything lands in `target/drive/`: `name.png` (the client area), `name.log` (stdout),
`name.controls` (where every control was), `name.state` (what the application was).

## Steps

Positional, in client-area pixels:
`move X Y`, `click X Y`, `right X Y`, `drag X1 Y1 X2 Y2`, `middle X1 Y1 X2 Y2`, `wheel X Y N`,
`key NAME`, `type TEXT`, `wait MS`, and `pressat NAME DX DY` / `rpressat NAME DX DY` for a press at
an offset inside one control — the palette is a grid of chips in a box far wider than the chips.

`path X1 Y1 X2 Y2 …` presses, walks through every point in turn, and releases. That is what a
freehand lasso is: `drag` walks a straight line, which is a polygon with no area, so every lasso
this suite drew before the step existed was a test of the code that copes with a tap.

`holding KEY STEP` holds space, alt, ctrl or shift down, does one step inside it, and lets go —
what space-to-pan and alt-click-to-pick both are, and what `key` cannot say, since it sends a whole
keystroke and is over before the drag begins.

For rearranging the workspace: `drag` moves a panel, and `hold X1 Y1 X2 Y2` does not. A tab is
grabbed the instant it is pressed; the hold is what turns a *stationary* press into a question —
after `panel_drag::HOLD_MS` a still tab opens that panel's settings and the grab is dropped. So
`hold` on a tab asks and moves nothing, which is what it is for here; `hold` on a window's edge
resizes, because an edge asks nothing when held. `holdat X Y` presses and holds in one place, which
is how a panel is asked for its settings.

**Pixel proof**: `ink X1 Y1 X2 Y2 N` counts dark pixels in a box of the page and asserts at least
N of them, or exactly none when N is 0. Undo depth says an operation was *recorded*; only this says
the brush put anything on the page, and those two came apart once already.

By name, resolved from the atlas the app writes each frame:
`tab NAME` (bring a panel forward), `press NAME`, `rpress NAME` (right button),
`slide NAME FRACTION`, `shot NAME` (a picture mid-run).

Assertions, against the state the app writes each frame:
`expect KEY VALUE`, `about KEY VALUE [TOLERANCE]` (for a number that came off a drag),
`state NAME` (print the lot).

`absent NAME` asserts that a control is **not** on screen. There was no negative assertion at all
for a long time, so every rule about hiding a command that would be refused was enforced by unit
tests and by nothing that had ever looked at the running application — one scenario narrated an
item disappearing and then did not check it. Note that it passes trivially when the panel holding
the control is not showing either, so open the menu first and check something that *is* there
alongside what is not.

A name may be a label (`Add`), a control id, or `Panel:label` when two panels share a word
(`Layers:Opacity`). A step that names something not on screen **stops the run** and lists what is —
a test that silently clicks nowhere is worse than no test.

## The two things that make it work

**The control atlas.** `OPENPAINT_CONTROLS` names a file; the app rewrites it every frame with
where every control and every tab landed, in physical pixels, plus each panel's viewport and scroll
offset (`panel_draw::report_controls`, `report_tab`; truncated at the top of `OpenPaint::redraw`).
So a scenario says which control it means, survives a different window size, and — because the
viewport is there too — `press` can scroll a control into view before clicking it. It has to: the
brush panel's list is four times taller than the window, and `place` gives every control a position
whether or not it is on screen.

**The state dump.** `OPENPAINT_STATE` names a file; the app rewrites it at the end of every frame
with the state a control is supposed to move. Read `OpenPaint::report_state` for the list, which is
the only authority — but in outline: the tool and the selection tool; pages, the active page, and
the page's size as a pair and as `page.w` / `page.h` on their own; the layer stack, its names in
order, and every property of the active layer; what is actually inside a selection, not merely
that there is one; the wand's settings; a transform's placement while one is in the air; the
caption's ten fields when a text layer is active; the colour, every brush parameter, every
response and its curve, the saved brushes; undo and redo depth; dirty and the file's name; the
zoom, the rotation and where the view is looking; which prompts and dialogs are up; and the whole
arrangement of panels in one line, with the direction each runs in beside it
(`Workspace::describe`, `Workspace::directions`).

At the end of the frame, not the start: painting is demand-driven, so the last frame after an
action is the last word, and reported from the top it described the state *before* that frame's
presses were applied. A screenshot cannot tell "Add made a layer" from "Add did nothing and
the list was already scrolled", and the log only says what input arrived, never what became of it.

## What it reaches, and what it does not

- **Input arrives as a pen.** OpenPaint reads its pointer through octotablet, and Windows Ink
  presents an ordinary mouse as a pen with no pressure axis — the log says so. The pen path is
  exercised; varying pressure, tilt, and a tablet's real report rate are not. Those need a hand.
- **It takes the mouse and the keyboard.** Nothing else can use the machine while it runs, and
  **only one run at a time** — it is a physical resource. Subagents cannot drive in parallel. Fan
  them out on reading code, writing scenarios and reading results; serialise the driving.
- **No touch, no multi-touch, no stylus barrel button.**

## Rules that are not negotiable

- **Never answer the artist's recovery prompt.** Its two answers are Recover and Discard, and
  Discard destroys unsaved work that is not ours to destroy. `drive.ps1` moves their recovery
  folder aside before each run and puts it back in `finally`. The recovery scenario answers a
  copy the harness *planted*, which is a different thing entirely.
- **A stash that is already there means the last run was killed**, and it is put back before
  anything is set aside again. The `finally` cannot run when the run is killed from outside,
  which is what interrupting a sweep does -- and the artist's workspace, brushes, theme and
  recovery copies then sit in a `.driving` stash with the live ones missing, which is precisely
  the harm the stashing exists to prevent. Nothing but this script writes those names, so a
  stash is never evidence of anything else.
- **Never kill the artist's running OpenPaint**, for the build lock or for anything else. Build to
  `target/drive-build`, which is why that flag is in every command above.
- **Do not drive at all while they have it open**, and `drive.ps1` now refuses to. Everything here
  moves their workspace, brushes, theme and recovery copies aside for the length of a run and puts
  them back at the end. That is safe when nothing else is using them and actively dangerous when
  something is: a running OpenPaint holds the path of its own recovery copy for as long as it
  lives, so while the folder is swapped its autosaves land in the run's folder and are thrown away
  with it. For those minutes the work on their screen has no recovery copy at all.

  Their copy runs from `targetelease` and this launches `target\drive-buildelease`, so the two
  are told apart by path rather than by name. It also explains two things that look like harness
  faults and are not: a recovery file that cannot be deleted because their application has it open,
  and one that keeps reappearing under the same name because it is theirs and still being written.
- `workspace.json`, `brushes.json` and `theme.json` are stashed the same way, so a run tests the
  code and not whatever the last one left. `-KeepWorkspace` is for the one case that wants the
  opposite. The brush library is on that list because a run can create and delete brushes, and a
  harness that mis-resolved one name once wiped the lot.

## What makes a scenario lie to you

Four things have caused an assertion to pass or fail for a reason that had nothing to do with the
application. Each cost an hour, and each is fixed in the harness rather than worked around in a
scenario — but they are the shapes to suspect first when a run disagrees with itself.

- **A name that fits two controls.** The brush panel has a Size slider and a Size picker, and six
  more pairs besides. First-wins resolved them silently, so a run meaning to drag a slider opened a
  dropdown and went on pressing at coordinates underneath it. A name that fits more than one
  control is refused now; say which by id.
- **A drag that lets go before it lands.** A slider follows the pointer while it is latched, so
  releasing in the same breath as the final move let the release be processed first and the value
  stopped wherever the previous sample reached. The same fraction gave 75 on one run and 132 on the
  next — and two assertions had been rewritten to match the under-drag before anyone noticed.
- **A wheel turned before a frame has happened.** Only the panel under the pointer takes the wheel,
  and egui decides which that is from the last frame. Painting is demand-driven, so straight after
  a click the frame that learns where the pointer went may not have happened yet and the notch is
  simply lost. It read as panels that scrolled sometimes.
- **A key sent at a native modal before it has the keyboard.** It lands on the application instead
  and leaves the dialog standing. Every dialog block in a scenario carries two generous waits for
  this reason, and they are not politeness.

The rule behind all four: an assertion that changes between runs is worse than no assertion,
because the way it gets silenced is by writing down whatever it happened to say.

## Two traps already paid for

- **PowerShell must be DPI-aware.** Without `SetProcessDpiAwarenessContext(PER_MONITOR_V2)` it
  measures in virtualised coordinates: a window 1898 physical pixels wide reports 1265, and a
  screenshot of "the whole window" is quietly its top-left two thirds. An hour went into hunting a
  missing right-hand column that was there the whole time.
- **`SetCursorPos` does not inject input.** It moves the cursor without putting an event in the
  stream, so Windows Ink synthesises no stylus pose from it, and a drag built out of it reached the
  app as one pose — one dab, no line, which reads exactly like a broken brush. Motion goes through
  `mouse_event(MOVE | ABSOLUTE | VIRTUALDESK)`.

## Also useful

`OPENPAINT_TRACE_INPUT=1` makes the app name why it refused a pen sample (the UI has that point, a
panel gesture owns the pointer, a prompt is up…). `OPENPAINT_TRACE_LAYOUT` writes the layout tree.
`drive.ps1` sets the first for you.
