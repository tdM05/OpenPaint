# What has been driven, and what it found

The checklist for the pass described in `docs/DRIVING.md`. Nothing here counts as done because a
unit test covers it, or because a panel renders in a headless screenshot. It counts when the real
binary was started, the thing was operated, and the application's own state said it worked.

Status: `--` not yet driven · `ok` driven and correct · `BUG` driven and wrong · `fixed` was wrong,
now driven and correct.

## Findings

| # | What | Where | Status |
|---|------|-------|--------|
| 1 | **Adding a layer is not undoable.** `press Add` then Ctrl+Z leaves three layers and `undo 0`. `history::Op` has `DeleteLayer` and `MergeLayer` but no variant for adding one, so Add, Duplicate, reorder (Up/Down), opacity, blend, visibility, lock alpha and clip-below all change the document without recording anything. Destructive operations are undoable and constructive ones are not. In CSP EX every document change is in the History palette. | `history.rs` `enum Op`; `ui.rs` `Picked::` handlers | open |

## Panels

| Panel | Control | Status | Note |
|-------|---------|--------|------|
| Layers | Add | BUG | adds a layer; not undoable (finding 1) |
| Layers | Duplicate | BUG | duplicates; not undoable (finding 1) |
| Layers | everything else | -- | |
| Tools | all six | -- | |
| Brush | all | -- | |
| Colour | all | -- | |
| History | all | -- | |
| Pages | all | -- | |
| Page | all | -- | |
| Transform | all | -- | |
| Select | all | -- | |
| Text | all | -- | |
| Menu | all | -- | |

## Core loop

| Action | Status | Note |
|--------|--------|------|
| Launch into the workspace | ok | `t00` |
| Brush stroke paints | ok | driven before this log existed; re-drive under assertion |
| Eraser | -- | |
| Undo / redo of a stroke | -- | |
| New / Open / Save / Save as / Export | -- | |
| Autosave and recovery | -- | never answer the prompt; see DRIVING.md |

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
