# Changelog

## Unreleased

### Panels follow a context now

Brush, Select, Transform and Text were four tabs in one strip as though they were four of a kind.
They are not: two are tool options, one is a layer property and one is a task in flight. There are
now two panels instead of four tabs, and each one follows something:

- **Tool options** shows the settings of whatever tool is in hand — the brush's while you paint or
  erase, the selection's while a lasso, rectangle, wand or move is up.
- **Properties** shows what the active layer is made of: the caption of a text layer, and for a
  raster layer a line saying what would put something there.

A **transform borrows Tool options** while it is in the air and gives it back on Apply or Cancel,
which is what it always was. Beginning one is now **Edit ▸ Transform selection**, or **Ctrl+T**;
**Add text layer** moved to the **Layer** menu beside Add and Duplicate, and is undoable like the
rest of them.

Nothing was taken away. Brush, Select, Transform and Text are still panels in the panel list, for
anyone who wants one docked permanently — the same module serves the contextual panel and the
standalone one. `docs/CONTEXTUAL_PANELS.md` and `DECISIONS.md` §1e have the reasoning.

### Fixed

- **A text field you could not see was holding the keyboard.** Clicking into the brush's name box
  and then changing panel left every one-letter shortcut in the application dead — `b`, `e`, the
  bracket keys, Delete — until that panel came back, with nothing on screen to say why. The field
  still remembers what was half-typed; it just no longer claims the keyboard while off screen.
- **Adding a text layer was outside undo** where adding an ordinary one was inside it, so Ctrl+Z
  after the one did nothing.
- Beginning a transform with nothing selected, or with one already in the air, now says so.

## 0.1.0 — 2026-09-03

The first release. OpenPaint is a painting application you can do a whole job in: open a scan,
ink over it in layers across many pages, and get a webtoon strip out the other end. It is not a
replacement for everything Clip Studio Paint does, and the list below of what it cannot do yet is
part of the release rather than an omission from it.

**Windows only for now.** The engine crates are portable; the stylus path is Windows Ink.

### What it does

- **A tiled GPU canvas** in linear light, with tiles spilling to the CPU, so document size is not
  limited by graphics memory. Pen pressure and tilt through Windows Ink.
- **A brush engine** with presets, a loadable bitmap tip, and six response curves.
- **Layers** — blend modes, opacity, clipping, alpha lock, a plain lock, reorder, merge,
  duplicate — and every one of those settings is undoable.
- **Pages**, which is the point of the project: a document is many pages, and a webtoon is one
  very tall one.
- **Selection** by lasso, rectangle and wand; a bucket; an eyedropper; and a transform whose live
  preview is drawn by the GPU.
- **Text layers** where the words stay editable, with font substitution reported rather than
  silently swapped, and IME input.
- **Import** of PNG and JPEG, as a document or as a layer, decided by the file's bytes rather than
  its name.
- **Export** of a page, of every page as numbered files, or of every page stacked into one tall
  strip, at a size you choose.
- **Copy, cut and paste** through the system clipboard.
- **Autosave and crash recovery**, and a `.openpaint` file that is an ordinary SQLite database.

### What it does not do yet

Layer groups. Speech bubbles. On-canvas text editing with a caret. PSD import or export. CMYK and
print colour management. A selection outline that actually marches. Each is in `docs/TODO.md` §4
with the reasoning for leaving it out.

### Known limits

- **It has only ever run on one machine.** wgpu picks its backend at runtime and this leans on
  D3D12 and Windows Ink; other hardware is untested.
- **Pen pressure has no automated coverage.** The scenario harness injects a mouse, which Windows
  Ink presents as a pen at constant pressure, so all six response curves are exercised only by a
  real hand on a real tablet.
- A live transform of a very large selection still commits on the CPU, once per gesture.

### How it is tested

971 unit tests, and 29 scenarios that operate the running application through its real UI —
moving the pointer, pressing controls read out of an atlas the application writes every frame —
asserting against the state it reports about itself. They exist because 926 unit tests were once
green while the brush painted nothing. `docs/DRIVING.md` explains the harness; `docs/DRIVE_LOG.md`
records what driving has caught that unit tests could not.
