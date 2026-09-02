# What has been driven, and what it found

The checklist for the pass described in `docs/DRIVING.md`. Nothing here counts as done because a
unit test covers it, or because a panel renders in a headless screenshot. It counts when the real
binary was started, the thing was operated, and the application's own state said it worked.

Status: `--` not yet driven · `ok` driven and correct · `BUG` driven and wrong · `fixed` was wrong,
now driven and correct.

## Findings

| # | What | Where | Status |
|---|------|-------|--------|
| 1 | **Adding a layer is not undoable.** `press Add` then Ctrl+Z leaves three layers and `undo 0`. `history::Op` has `DeleteLayer` and `MergeLayer` but no variant for adding one, so Add, Duplicate, reorder (Up/Down), opacity, blend, visibility, lock alpha and clip-below all change the document without recording anything. Adding a *page* is the same. Destructive operations are undoable and constructive ones are not. In CSP EX every document change is in the History palette. | `history.rs` `enum Op`; `ui.rs` `Picked::` handlers | open |
| 2 | **Every dropdown in the application opened an empty box.** `Picked::OpenMenu` carried where and how big but not who asked, and the workspace opened the popup for `MENU` whichever panel had asked. Choosing a blend mode, a font, an alignment or a response source was impossible. | `ui.rs` | fixed |
| 3 | **A secondary press anywhere opened the panel list**, including inside a panel that has its own use for the button — so right-clicking a swatch to forget it forgot it *and* threw the list over the panel. | `workspace.rs` | fixed, with a test |
| 4 | **Unmodified keys fired while a text field had the caret.** Naming a brush "beeper" selected the eraser, stepped the size twice and refitted the view; Backspace and Delete erased the drawing. | `main.rs`, `ui.rs::typing` | fixed |
| 5 | A menu popup's last item overhangs its own box by about two thirds of a point. A hand can still hit it; nothing is visibly wrong. Cosmetic, unfixed. | `panels/mod.rs::list_size` | open |
| 7 | **A dozen on-screen labels carried runs of twenty-odd spaces mid-sentence.** The recovery prompt read "pointed at the original⎵⎵⎵…⎵file, so". Mended, and `panel_ui::place` now refuses a label with four or more spaces in a row — with a test that lays out every panel, because the first version of that check passed a deliberate sabotage: nothing in the suite had ever built these panels' controls. | `ui.rs`, `panels/*`, `panel_ui::place` | fixed, with a test |
| 8 | **Ctrl+F3 left floating windows standing** while restoring the built-in arrangement, so a floated panel ended up in the workspace twice. | `workspace.rs::reset` | fixed |
| 9 | `docs/PANEL_GESTURES.md` promised that holding a docked compact panel's strip opens its settings; `window_target` has never done that. The doc now says what the code does. | docs | fixed |
| 6 | A straight-line lasso drag encloses an area rather than nothing, which no even-odd rule should give for a there-and-back path. The shape the lasso receives is therefore not the line the harness drew. Asserted as it is, in `select.txt`; worth understanding. | `main.rs` lasso path | open |

## Panels

Every one of these is driven by a scenario in `tools/scenes`, run by `tools/sweep.ps1`. The counts
are assertions against the application's own reported state, not steps.

| Panel | Scenario | Assertions | Status |
|-------|----------|-----------:|--------|
| Tools | `tools.txt` | 9 | ok |
| Layers | `layers.txt` | 23 | ok, except Add/Duplicate not being undoable (finding 1) |
| Brush | `brush.txt` | 34 | ok |
| Colour | `colour.txt` | 6 | ok |
| History | `history.txt` | 7 | ok |
| Pages | `pages.txt` | 67 | ok, except Add page not being undoable (finding 1) |
| Page | `page.txt` | 61 | ok |
| Select | `select.txt` | 77 | ok |
| Transform | `transform.txt` | 104 | ok |
| Text | `text.txt` | 48 | ok |
| Menu | `menu.txt` | 76 | ok |
| — panel gestures | `gestures.txt` | 60 | ok |

## Core loop

| Action | Scenario | Status |
|--------|----------|--------|
| Launch into the workspace | every one | ok |
| Brush stroke, eraser, undo, redo | `paint.txt` | ok |
| Every keyboard shortcut | `keyboard.txt` | ok |
| New, and its unsaved-changes prompt | `menu.txt` | ok |
| Open / Save / Save As — the request and its cancellation | `menu.txt` | ok |
| Export PNG | `menu.txt` | ok |
| A document written and read back, marks and all | `file.txt` | ok |
| The recovered-work prompt: visible, blocking, answerable | `recovery.txt` | ok |

## Panel gestures

| Gesture | Status | Note |
|---------|--------|------|
| Float a panel | -- | |
| Dock a panel | -- | |
| Drag a tab | -- | |
| Resize a floating window's edges and corners | -- | |
| Pick-a-destination mode | -- | |
| Settings popup, remove this panel | -- | |
| Wheel scrolls the panel under the pointer only | ok | proven by `press` scrolling a control into view |
| Panel list, and toggling a panel off and on | ok | `gestures.txt` |
| Layout undo, redo and reset (F3, Shift+F3, Ctrl+F3) | ok | `gestures.txt` |
| Float, dock, move a tab between slots, settings popup, remove a panel, resize a window | ok | `gestures.txt` |
