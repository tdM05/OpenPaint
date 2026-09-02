# What has been driven, and what it found

The checklist for the pass described in `docs/DRIVING.md`. Nothing here counts as done because a
unit test covers it, or because a panel renders in a headless screenshot. It counts when the real
binary was started, the thing was operated, and the application's own state said it worked.

Status: `--` not yet driven · `ok` driven and correct · `BUG` driven and wrong · `fixed` was wrong,
now driven and correct.

## Findings

| # | What | Where | Status |
|---|------|-------|--------|
| 1 | **Adding a layer was not undoable.** Deleting one was, so Ctrl+Z after Add did nothing — which reads as undo being broken rather than as the operation being outside it. `Op::AddLayer`, `Op::MoveLayer`, `Op::AddPage` and `Op::MovePage` now mirror the deletions that reverse them, and an undone addition takes its pixels with it so a redo puts back a duplicate rather than an empty layer wearing its name. | `history.rs`, `renderer.rs`, `main.rs` | fixed |
| 1a | **A layer's *properties* are still outside history** — opacity, blend, visibility, lock alpha, clip below, and its name. That is a deliberate decision written into `main.rs` ("putting a switch in the undo stack would make Ctrl+Z toggle settings instead of reversing artwork"), and it differs from CSP EX, where every one of them is in the History palette. Left as it is, because overturning a stated decision is the artist's call and it needs a coalescing policy before a slider can go in the stack at all. | `main.rs` `LayerAction::Set*` | **for the artist to decide** |
| 2 | **Every dropdown in the application opened an empty box.** `Picked::OpenMenu` carried where and how big but not who asked, and the workspace opened the popup for `MENU` whichever panel had asked. Choosing a blend mode, a font, an alignment or a response source was impossible. | `ui.rs` | fixed |
| 3 | **A secondary press anywhere opened the panel list**, including inside a panel that has its own use for the button — so right-clicking a swatch to forget it forgot it *and* threw the list over the panel. | `workspace.rs` | fixed, with a test |
| 4 | **Unmodified keys fired while a text field had the caret.** Naming a brush "beeper" selected the eraser, stepped the size twice and refitted the view; Backspace and Delete erased the drawing. | `main.rs`, `ui.rs::typing` | fixed |
| 5 | A menu popup's last item overhangs its own box by about two thirds of a point. A hand can still hit it; nothing is visibly wrong. Cosmetic, unfixed. | `panels/mod.rs::list_size` | open |
| 7 | **A dozen on-screen labels carried runs of twenty-odd spaces mid-sentence.** The recovery prompt read "pointed at the original⎵⎵⎵…⎵file, so". Mended, and `panel_ui::place` now refuses a label with four or more spaces in a row — with a test that lays out every panel, because the first version of that check passed a deliberate sabotage: nothing in the suite had ever built these panels' controls. | `ui.rs`, `panels/*`, `panel_ui::place` | fixed, with a test |
| 8 | **Ctrl+F3 left floating windows standing** while restoring the built-in arrangement, so a floated panel ended up in the workspace twice. | `workspace.rs::reset` | fixed |
| 9 | `docs/PANEL_GESTURES.md` promised that holding a docked compact panel's strip opens its settings; `window_target` has never done that. The doc now says what the code does. | docs | fixed |
| 10 | **The font-substitution warning was drawn twice**, one line under the other, whenever a page was lettered in a face nobody asked for. `panel_ui::place` now refuses two identical sentences in a row, and the panel fixture gained a text layer — without one, the check passed a deliberate sabotage because nothing could reach the second copy. | `panels/text.rs`, `panel_ui::place`, `screenshot::sample_document` | fixed, with a test |
| 11 | The View menu was the `_` arm, so any id not one of the five drew View's items under another menu's name. An id nobody put in the bar now has no items. | `panels/menu.rs` | fixed |
| 12 | `Picked::TextChanged` had no producer and never would; `Change::Picked` was a placeholder for a round trip that has since been written another way. Both gone, and the `expect(dead_code)` on `Picked` with them — it was worded to stop compiling when the port finished, and it did. | `ui.rs`, `panel_ui.rs` | fixed |
| 13 | F2's status message told the artist that Ctrl+Shift+Z resets the layout. That is artwork redo; the reset is Ctrl+F3. And `handle_navigation`'s doc claimed Ctrl+0 and Ctrl+1 zoom, where the code deliberately excludes Ctrl. | `main.rs` | fixed |
| 15 | **Two of the four selection tools would not put themselves away.** Pressing the tool already up toggles it off — but only Lasso and Rect were named in the check, so a lit Wand or Move was silently rebuilt instead. The same press on the same-looking button doing two different things, with nothing in the panel to say which. Now asked of the tool rather than of a list, so a fifth tool cannot be left out of it. | `main.rs` `SelectAction::Use` | fixed |
| 16 | **The menu offered three things it would refuse**: Merge down on the bottom layer, Fill selection with nothing selected, and Deselect/Invert/Clear likewise. `menu.rs`'s own header says a menu never does that, and there was a test for it — which only covered Delete. | `panels/menu.rs` | fixed, with a test |
| 14 | Stale module docs: `text.rs` said the caption editor was "not drawn yet" — it is fully wired, and a reader trusting the header would skip fourteen live controls — and `page.rs` described a blocker that had been lifted. | `panels/text.rs`, `panels/page.rs` | fixed |
| 6 | **A lasso with no area left a selection that could not be seen.** An injected pointer reports whole pixels, so a "straight" drag is a staircase wandering half a pixel either side of the line it meant to be; closing that back to the start encloses a row of slivers. Far below the half coverage `Selection::outline` counts as inside — so no marching ants — and comfortably above zero, so `is_empty` said there *was* a selection. Every stroke afterwards fell outside it and was refused, with nothing on screen to say why: the worst state this application has had, arrived at by a second route. `set_selection` now tests the outline rather than the mask, so what constrains the brush is what is drawn on the page. | `main.rs::set_selection` | fixed, with a test |

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
| Zoom by wheel, pan by middle-drag and by space-drag, alt+click to pick a colour | `navigate.txt` | ok |
| Ctrl+S over an existing path, and all three answers to the unsaved-changes question | `saving.txt` | ok |
| The eight crop handles, one at a time, and Enter to apply | `crop.txt` | ok |
| What the menus and panels do **not** offer | `absences.txt` | ok |
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
| Wheel scrolls the panel under the pointer only | ok | `navigate.txt` — and the wheel over the artwork zooms, which is a different code path and was for a long time wrongly claimed as covered by this one |
| Panel list, and toggling a panel off and on | ok | `gestures.txt` |
| Layout undo, redo and reset (F3, Shift+F3, Ctrl+F3) | ok | `gestures.txt` |
| Float, dock, move a tab between slots, settings popup, remove a panel, resize a window | ok | `gestures.txt` |
