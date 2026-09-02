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
| `transform.txt` | every transform control, each pinned to the field it moves |
| `text.txt` | the caption editor, each of its fourteen controls pinned to its own field |
| `menu.txt`, `absences.txt` | every menu item, and what the menus refuse to offer |
| `gestures.txt` | float, dock, move a tab, settings, remove, the panel list, window resize, layout undo |
| `window-body.txt` | that a floating panel's own controls work and do not drag its window, for two different panels |
| `file.txt`, `saving.txt` | a document written and read back; Ctrl+S over a path; all three answers to the unsaved-changes question |
| `recovery.txt`, `autosave.txt` | the recovered-work prompt, autosave writing, and closing the window with unsaved work |

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
