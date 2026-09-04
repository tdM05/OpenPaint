# What has been driven, and what it found

The record of the pass described in `docs/DRIVING.md`. Nothing here counts as done because a unit
test covers it, or because a panel renders in a headless screenshot. It counts when the real binary
was started, the thing was operated, and the application's own account of itself said it worked.

Run it with `tools/sweep.ps1`. One scenario at a time, always: the mouse and the keyboard are one
physical thing.

## What the pass found

Ranked by how soon an artist would have met it.

| # | What | Where | Status |
|---|------|-------|--------|
| 1 | **Every dropdown in the application opened an empty box.** `Picked::OpenMenu` said where a list goes and how big, but not who asked, and the workspace opened it for the menu bar whichever panel had asked. Choosing a blend mode, a font, an alignment or a response source was impossible, and looked like a rendering fault. | `ui.rs` | fixed |
| 2 | **A lasso with no area left a selection nobody could see.** An injected pointer — and a hand — reports whole pixels, so a "straight" drag is a staircase wandering half a pixel either side of the line it meant to be; closing that encloses a row of slivers. Far below the half coverage `Selection::outline` counts as inside, so no marching ants; comfortably above zero, so `is_empty` said there *was* a selection. Every stroke afterwards fell outside it and was refused, with nothing on screen to say why. `set_selection` now tests the outline rather than the mask. | `main.rs::set_selection` | fixed, with a test |
| 3 | **Unmodified keys fired while a text field had the caret.** Naming a brush "beeper" selected the eraser, stepped the size twice and refitted the view; Backspace and Delete erased the drawing. | `main.rs`, `ui.rs::typing` | fixed |
| 4 | **Adding a layer was not undoable.** Deleting one was, so Ctrl+Z after Add did nothing — which reads as undo being broken rather than as the operation being outside it. `Op::AddLayer`, `Op::MoveLayer`, `Op::AddPage` and `Op::MovePage` now mirror the deletions that reverse them, and an undone addition takes its pixels with it so a redo puts back a duplicate rather than an empty layer wearing its name. | `history.rs`, `renderer.rs`, `main.rs` | fixed |
| 4a | **A floating panel's contents moved the window.** Reported by the artist: with Colour floated, dragging across the colour wheel moved the whole window instead of choosing a hue, so the panel could not be used at all. Any press inside a window that was not a tab, a divider or an edge became a grab on its frame — and "not a tab" was every control in it. The same rule the secondary press already followed (finding 5) and that this had not been given: what a panel draws in is the panel's. A window is still movable by its chrome, which is what is left once the contents are taken out. | `workspace.rs::input_frame` | fixed, with a test |
| 4b | **A window floated after start-up took the name of one restored from the file.** Reported: with panels floating, dragging one's tab moved a different one. Windows restored from a saved workspace are named by their position in it — 0, 1, 2 — while the counter that names new ones started at zero regardless, so the first panel floated after start-up was handed a name that already belonged to something. Every lookup is `find(\|f\| f.id == id)`, which answers with the first of the two. It needed a saved workspace to happen, which is why nothing driven from a fresh start ever met it. | `workspace.rs::from_parts` | fixed, with a test |
| 4c | **A release did not always let go.** When the window it was holding had ceased to exist — which is what a merge does to the window a tab came from — `release` was skipped and the grab left standing, so `working_surface` kept handing back that surface instead of the one under the pointer for the rest of the session. Latent rather than the reported fault, and fixed alongside it. | `workspace.rs` | fixed, with a test |
| 5 | **A secondary press anywhere opened the panel list**, including inside a panel with its own use for the button — so right-clicking a swatch to forget it forgot it *and* threw the list over the panel. | `workspace.rs` | fixed, with a test |
| 6 | **A panel that scrolled gave no sign that it scrolled.** The brush panel showed four rows out of forty. | `panel_draw.rs` | fixed |
| 7 | **Two of the four selection tools would not put themselves away.** Only Lasso and Rect were named in the check, so a lit Wand or Move was silently rebuilt instead — the same press on the same-looking button doing two different things. | `main.rs` | fixed |
| 8 | **The menu offered three things it would refuse**: Merge down on the bottom layer, Fill selection with nothing selected, and Deselect/Invert/Clear likewise. `menu.rs`'s own header says a menu never does that, and there was a test for it — which only covered Delete. | `panels/menu.rs` | fixed, with a test |
| 9 | **A dozen on-screen labels carried runs of twenty-odd spaces mid-sentence.** The recovery prompt read "pointed at the original⎵⎵⎵…⎵file, so". `panel_ui::place` now refuses a label with four or more spaces in a row. | `ui.rs`, `panels/*` | fixed, with a test |
| 10 | **The font-substitution warning was drawn twice**, one line under the other, whenever a page was lettered in a face nobody asked for. `place` now refuses two identical sentences in a row. | `panels/text.rs` | fixed, with a test |
| 11 | **Ctrl+F3 left floating windows standing** while restoring the built-in arrangement, so a floated panel was in the workspace twice. | `workspace.rs::reset` | fixed |
| 12 | **F2's status message named the wrong shortcut** for resetting the layout (Ctrl+Shift+Z is artwork redo; the reset is Ctrl+F3), and `handle_navigation`'s doc claimed Ctrl+0 and Ctrl+1 zoom where the code deliberately excludes Ctrl. | `main.rs` | fixed |
| 13 | The View menu was the `_` arm of its match, so any id not one of the five drew View's items under another menu's name. | `panels/menu.rs` | fixed |
| 14 | Dead code that had outlived its purpose: `Picked::TextChanged` had no producer and never would; `Change::Picked` was a placeholder for a round trip since written another way; the `expect(dead_code)` on `Picked` was worded to stop compiling when the port finished, and it had. | `ui.rs`, `panel_ui.rs` | fixed |
| 15 | Stale module docs: `text.rs` said its caption editor was "not drawn yet" — it is fully wired, and a reader trusting the header would skip fourteen live controls — and `page.rs` described a blocker that had been lifted. `docs/PANEL_GESTURES.md` promised a hold that `window_target` has never done. | docs | fixed |
| 15a | **Discarding a recovery copy left its SQLite side files behind.** A document is three files in WAL mode, and only the one we named was removed — so every clean exit left a `-shm` and a `-wal` orphan, kept for ever. Thirty-five pairs had collected in one data directory. Nothing noticed because `scan` ignores anything without the document extension, so recovery itself was never confused by them. Both go now, and the folder sweeps orphans whose document has gone when it is read, so a directory that collected them empties itself. | `autosave.rs` | fixed, with a test |
| 16 | A menu popup's last item overhangs its own box by about two thirds of a point. A hand can hit it; nothing is visibly wrong. | `panels/mod.rs::list_size` | open, cosmetic |
| 17 | **A layer's properties are outside history** — opacity, blend, visibility, alpha lock, clipping, and its name. That is a deliberate decision written into `main.rs` ("a switch in the undo stack would make Ctrl+Z toggle settings instead of reversing artwork"), and it differs from CSP EX, where every one of them is in the History palette. Left alone: overturning a stated decision is the artist's call, and a slider needs a coalescing policy before it can go in a stack at all. | `main.rs` | **for the artist to decide** |

Three of these were found in the harness rather than the application, and each could have cost the
artist work, so they are written here too: a name that matched two controls was resolved silently
to one of them (which deleted the saved brushes); killing a sweep mid-run stranded the artist's
workspace, brushes and recovery copies in a stash; and worst, a run would set those things aside
**while the artist had OpenPaint open**, so for the length of it their live session's autosaves
landed in the run's folder and were thrown away with it. `drive.ps1` now refuses to start while
their copy is running. All three are fixed; see `docs/DRIVING.md`.

## What is covered

Every panel, every menu, every keyboard chord and every core-loop action is driven by a scenario in
`tools/scenes` and checked against the state the application reports about itself.

| Scenario | What it covers |
|---|---|
| `tools.txt` | the six tools |
| `paint.txt` | a stroke, undo, redo, the eraser — all four proved in pixels |
| `keyboard.txt` | every chord the application answers |
| `navigate.txt` | wheel zoom, both pans, fit, actual size, rotation, alt+click to pick a colour |
| `layers.txt`, `duplicate.txt` | every control in the Layers panel, and a duplicate's pixels through undo and redo |
| `brush.txt` | every slider, every preset, all six response pickers and curves, the edge profile, caret editing |
| `tip.txt` | loading a bitmap tip, and the branch of the panel that only exists once one is loaded |
| `colour.txt` | the wheel, the palette, the three wheel shapes |
| `history.txt` | the panel, and its buttons appearing only when there is something to undo |
| `pages.txt`, `page.txt`, `crop.txt` | pages, page size, extend, trim, and all eight crop handles |
| `select.txt` | every selection tool and command, the wand's settings, fill and clear in pixels |
| `transform.txt` | every transform control, each pinned to the field it moves, in the Tool options panel a transform borrows -- and the standalone Transform panel, for the two shapes nothing else can reach |
| `text.txt` | the caption editor in Properties, each of its fourteen controls pinned to its own field |
| `menu.txt`, `absences.txt` | every menu item, and what the menus refuse to offer |
| `contextual.txt` | that Tool options follows the tool and Properties follows the layer, that a transform borrows the panel and gives it back, that an empty context says what would fill it, and that a field off screen does not hold the keyboard |
| `gestures.txt` | float, dock, move a tab, settings, remove, the panel list, window resize, layout undo |
| `window-body.txt` | that a floating panel's own controls work and do not drag its window, for two different panels |
| `file.txt`, `saving.txt` | a document written and read back; Ctrl+S over a path; all three answers to the unsaved-changes question |
| `recovery.txt`, `autosave.txt` | the recovered-work prompt, autosave writing, and closing the window with unsaved work |
| `comic.txt` | a whole page made start to finish: scan, lock, ink, colour, transform, letter, second page, strip, save, reopen |
| `import.txt` | a PNG placed as a layer and a JPEG opened as a document, through the real file dialog |
| `export.txt` | the export dialog, and the files it writes read back off the disk |
| `clipboard.txt` | copy, cut and paste, out through the system clipboard and back |

## What is not covered, and why

- **Varying pen pressure and tilt.** Windows Ink presents an ordinary mouse as a pen with no
  pressure axis, so every driven stroke is at constant pressure. This is the largest hole and it is
  invisible in the numbers: `Source::Pressure` is the default for all six brush responses, so the
  whole modulation system is *configured* by the suite and driven by a flat input. It needs a
  tablet and a hand.
- **Touch, multi-touch, a stylus barrel button.** No injection path, and no handling yet to test.
- **A tablet's real report rate.** An injected pointer cannot approach 200 Hz, so the pen-rate
  readout can never show a healthy number in a driven run.
- **A file dialog's own contents.** It blocks on its own thread with none of the application's
  controls on screen. Scenarios type a path into it blind, with generous waits; the failure paths
  behind it (a file that will not open, an extension appended) stay out of reach.
- **The brush library's trouble branch**, which needs the library file to be unwritable. Reachable
  by making it read-only before a run; not done.

## The release sitting — 2026-09-02

Every scenario the session's work could reach was driven. `import.txt` (27), `export.txt` (38) and
`clipboard.txt` (23) are new; `layers.txt` (50), `menu.txt` (90), `keyboard.txt` (48) and
`gestures.txt` have new steps. The rest were run because the changes reach further than the
features do -- layer settings entered the undo stack, and every refusal moved to one seam.

**Twenty-five of twenty-seven pass.** `transform.txt` (182) is the one worth naming: it now drives
the *GPU* float pass end to end -- every scale, rotation, flip, corner and kernel -- against a
preview that no longer touches the CPU.

### What the driving caught that the unit tests did not

Three, and all three were invisible to `cargo test`:

- **The export dialog did not stop the pen.** Its size slider is drawn over the canvas, and
  dragging it painted strokes underneath: the scene ended a run of exports with an undo depth of
  ten instead of two. The three modals were listed one at a time in `decide_capture` and the
  newest was not among them; they now come from one `asking()`.
- **The float layer stopped being composited.** Moving the resample onto the GPU added two early
  returns to `float_at`, and both stepped over `self.floating = Some(float)` at the end of it --
  so the selection *vanished* the moment a transform began. No unit test could have caught it:
  what went wrong was a line not reached.
- **A save dialog handed an existing name asks whether to overwrite**, and that question is a
  native modal this harness cannot answer. A leftover file from a failed run hangs the next one
  and then reports the stale file's size as the new one's. The sweep now deletes `out-*.png`
  first.

### Three defects it found, all fixed

Checked first by stashing the session's changes and rebuilding at `ac108f4`: all three behave the
same there, so none of them came from the release work. They are fixed anyway -- a release does not
ship with the panel UI broken, and two of them were in the gestures an artist uses every day.

- **A window's top edge was eating its own handle.** `splitter_grab` is 26, so the resize border
  reached thirteen units *inside* a header that is twenty-eight tall: the upper half of every
  floating window's tab was a resize handle, and a tab grabbed a little high resized the window
  instead of moving it. `chrome::edge_at` now reaches outward only on the top edge; the other three
  keep their inner half, because a panel body is behind them and not a handle. DECISIONS §1d.
- **A menu was left standing, and empty, after an item was chosen.** The panel cleared its own
  state and the workspace's popup stayed, so every command reached from a menu left a dark box over
  the artwork. It is in `menu-new.png` for as long as that shot has existed. DECISIONS §6e. The
  first attempt at this set a flag that is read *earlier in the same frame* -- dead the moment it
  was written, and every scenario passed straight through it, because none of them looks at an
  empty box. Clippy caught it.
- **Two scenes were holding coordinates that had stopped meaning anything.** A second floating
  window used to cascade to `88,88` and now lands beside the first at `387,60`; tabs are laid out
  from measured labels, so `holdat 1900 91` had drifted from the Pages tab onto History. Both
  scenes went on *passing steps that meant something else*, which is worse than failing, and then
  failed in a way that read as a bug in the application: `gestures.txt` pressed empty canvas where
  it meant a window's edge, and the run ended with the windows merged away.

  The fix is `holdtab NAME [DX DY]` and `dragtab NAME DX DY`, which read the same atlas `tab`
  reads. **A scenario should never name a tab by where it happens to be.**

### And one in the harness

**A run that stopped threw away its own evidence.** The checks file was written only on the way out
of a successful run, so a stopped one left the *previous* run's checks on disk -- and reading them
afterwards described a run that never happened. Two of the defects above were chased for an hour
each against stale evidence before that was noticed, which is a worse failure than either of them:
it is invisible, and it makes every conclusion drawn from it wrong. The checks are now written
whatever happens, with the step that stopped the run recorded as its last line.

## Drawing a whole page — 2026-09-03

`comic.txt` is not another panel's scenario. It is a *session*: a scan placed and locked, inked
over on a layer of its own, coloured with the bucket, a corner lassoed and rotated, lettered,
copied onto a second page, exported as a strip, saved, and opened again — with a shot at every
step, meant to be read in order.

**Every step of it is covered elsewhere, and it still found two things**, because what it tests is
the seams:

- **A selection followed the artist to another page.** A mask is in page coordinates; on page two
  it described a region of artwork nobody had selected, and the marching ants were drawn over it.
  Fixed in `follow_active_page` — DECISIONS §5i.
- **The Layers panel had grown too tall to use.** Adding the lock and its sentence pushed the
  explanation to six lines, and with four layers the list was down to a single row with the
  buttons off the bottom. An explanation that pushes away the thing it explains has stopped
  explaining; both paragraphs are now a clause each.

And one thing that looked like a bug and was not: copying a rectangle over the artwork pasted the
*caption*, because the caption's layer was active and copy takes the active layer, never the
composite (DECISIONS §7c) — which is what every comparable application does. The scenario was the
thing that was unrealistic; it now selects the inking layer first, as a person would.

## Contextual panels — 2026-09-03

Brush, Select, Transform and Text stopped being four tabs and became two panels that follow
something: Tool options follows the tool in hand, Properties follows the active layer, and a
transform borrows the first while it is in the air. `DECISIONS.md` §1e has the decision and
`docs/CONTEXTUAL_PANELS.md` the reasoning it was built from.

**This is a rename pass as much as a feature.** The control atlas names controls by panel, so
`Select:Lasso` became `Tool options:Lasso`, `text:Caption` became `Properties:Caption`, and
`brush:1048577` became `Tool options:1048577`; every `expect layout` in `gestures.txt`,
`menu.txt`, `window-body.txt` and `windows-apart.txt` changed with the default arrangement. Done as
one pass, because that is what this file says to do.

`contextual.txt` is new: it switches tool and asserts the panel followed, that an empty context says
what would fill it, that a transform borrows the panel and gives it back, and that a text field off
screen no longer holds the keyboard.

**All thirty scenarios pass, 1266 assertions.** Every one was run, not only the ones this work
touched: the arrangement changed under all of them, and a scene that merely *opens* the workspace
is a scene this could break.

### What the driving caught that the unit tests did not

- **The panel-list switch puts a panel in the air, not anywhere in particular**, and the click after
  it is what says where. `transform.txt` was written without that click, so the three `absent`s
  after it passed for the wrong reason — the panel was in neither tree — and the run only stopped a
  step later, on the `press` that meant something. That is the trap `DRIVING.md` warns about, met in
  the wild rather than in theory, and the scene now clicks a destination.
- **Floating windows are listed back to front**, so taking one by its tab brings it forward and
  moves it to the *end* of the line — and F3 does not put the order back, because which window is in
  front follows the pointer and is not an arrangement. Two assertions in `gestures.txt` were written
  the other way round.
- **A release binary built while a sabotage sweep was running has the sabotage in it.** The first
  driven run of `contextual.txt` opened on a workspace with both Tool options *and* Brush as tabs,
  which is exactly what one of the nine sabotages does to `default_layout` — and the binary had been
  compiled in the ninety seconds that file was broken on purpose. Nothing in the harness can catch
  that; the lesson is that the two must not overlap.
- **`Add text layer` renumbered the text section's commands.** `text.txt` presses the Load-font
  button by id because its label ends in an ellipsis, and the id moved from 514 to 513 when the
  command above it left for the Layer menu. Naming a control by id has that cost, and it is still
  cheaper than an ellipsis in a scene file.

### Two defects it found in the application, both pre-existing

- **A text field you could not see was holding the keyboard.** `Ui::typing` asked every panel it had
  ever drawn, and `editing` is dropped only by the panel that draws the field — so clicking into the
  brush's name box and then changing tab left every unmodified shortcut dead until that panel came
  back, with nothing on screen to say why. It needed a tab switch to reach before; a contextual panel
  makes it a press on the tool rail, which is how it turned up. `typing` now asks only the sections
  drawn last frame: the field remembers what was half-typed, and the keyboard is claimed only by a
  field on screen. Driven in `contextual.txt`.
- **Adding a text layer was outside history** while adding a raster one was inside it, so Ctrl+Z
  after the one did nothing — the same shape as finding #4, and glaring once the two sat side by
  side in the Layer menu.

### And one that is not ours

`colour_wheel::tests::the_hue_strip_runs_from_red_round_to_red` fails on the toolchain this session
had to build with — the far end of the hue strip comes back as 359.99997 rather than 0. It fails
identically at `380ae19` with the same toolchain, so it is not from this work; the machine had no
Rust and no MSVC C++ tools, so the build is `x86_64-pc-windows-gnu` against a MinGW libm rather than
the msvc target CI uses, and `mul_add` is the obvious suspect. The assertion now prints the value it
got, which is how that was found at all.
