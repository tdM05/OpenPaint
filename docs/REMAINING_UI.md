# What is left before the old side panel can be deleted

The workspace (F2) is the real UI. The old egui side panel is still there because it is still the
only way to reach most of the application. **This file is the list of what has to move before it
can go**, written for whoever picks this up next — including a future me who will not remember.

Checked against the code, not from memory: the old panel's sections are the `ui.heading(...)`
calls in `ui.rs`, and the new ones are the arms of `workspace_panel`.

## Already moved

| Panel | State |
|---|---|
| Menu | File / Edit / Layer / Select / View, all wired to the same handlers as the shortcuts |
| Tools | six tools, with icons |
| Layers | list, per-row eye, lock alpha, add / duplicate / merge down / delete |
| Brush | size, opacity, hardness, spacing |
| Canvas | the artwork itself; never a described panel |

## Not moved

Nine things. Most of the application, by weight.

1. **Transform** — scale and rotation values, flip horizontally/vertically, lock aspect,
   resampling kernel, apply/cancel. A whole tool with no home in the new UI.
2. **Pages** — add, delete, reorder. Core to a comics application.
3. **Crop / resize / extend** — crop by dragging, extend by *n* pixels per edge, apply/cancel,
   trim to canvas.
4. **Text** — add a text layer, load a font file, convert to raster.
5. **Brush, the other two thirds** — flow, angle, roundness, stabilisation, brush tips (load a
   bitmap tip, back to round), presets (save, load, delete), reset to defaults.
6. **Layers, the rest** — blend mode, layer opacity, move up/down.
7. **Colour** — *the wheel is in*, in all three shapes, with the shape choosable in the panel.
   What is left is the **document palette**: the swatches, saved with the document.
8. **Select settings** — wand tolerance, expand, fill-on-click, fill with the brush colour. The
   *commands* (all / none / invert / clear) are already in the menu.
9. **History** — still a placeholder label.

Deliberately **not** moving: the canvas-memory and speed readouts. They are diagnostics, not
artist tools, and the workspace is not where they belong.

## What blocks doing these in parallel

Two things, and both have to be done first or every worker invents its own version.

### Three control kinds are missing

| Kind | Needed by |
|---|---|
| **Pick** — a dropdown | blend mode, resampling kernel |
| **Text** — a typed field | preset names, layer rename, page size, transform values |
| ~~**Custom** — drawn by code~~ | **done**, and the colour wheel is drawn with it |

`text_field.rs` already holds the editing logic (caret, selection, word motion, UTF-8 safe,
fuzzed); it needs a `Control::Text`, a caret to draw and keyboard events.

`Control::Custom` is done: the engine still decides where it goes and how tall it is, and hands
the panel the rectangle back through `PanelInput`. The colour wheel is the worked example --- one
custom control, everything around it described as usual.

### `ui.rs` is one function with an arm per panel

At ~2,800 lines with every panel in one `match`, two people cannot work on two panels without
colliding — which is exactly what cost a night already.

**The fix is also the better architecture: one module per panel.** Each exports

```rust
pub fn controls(state: &PanelState<'_>) -> Vec<Control>;
pub fn apply(change: Change, state: &PanelState<'_>) -> Option<Picked>;
```

and `ui.rs` keeps one line each. Then every worker owns a file nobody else touches, and the
descriptor design finally shows up in the file layout instead of only in the types.

## Suggested order

1. The three control kinds, and split one existing panel out as the pattern to copy.
2. Then the nine, in parallel, one module each.
3. Delete the old side panel, `F2`, and everything that only it used.

## Two things worth knowing before trusting this work

- **An agent's report is not evidence.** One reported a set of icons as correct; it rendered as
  overlapping wedges, which only a screenshot caught. Render it and look.
- **The panels that touch a live document** — transform, pages, crop — cannot be screenshotted the
  way the chrome can, because there is no document behind a headless frame. They need the most
  scrutiny and the most sabotage.
