# The workspace is the UI

The old side panel is **gone** (2026-09-02), and with it `F2`, which used to switch between the
two. What follows is the record of what moved where, and of what the descriptor vocabulary still
lacks -- which is the part that is still true.

Checked against the code, not from memory.

## Where everything lives

One module each, in `crates/openpaint-app/src/panels/`. Each exports one `show`, and every one of
them is a pure `controls(...)` building the list plus a pure `picked(...)`/`answer(...)` mapping a
change -- which is what makes them testable without a GPU.

| Panel | What it holds |
|---|---|
| Menu | File / Edit / Layer / Select / View, and the panel list |
| Tools | six tools, with icons |
| Brush | size, opacity, hardness, spacing, roundness, angle, flow, stabilisation; presets with a name field; the tip; all six response curves with their source pickers; reset |
| Layers | the list with per-row eyes, blend, opacity, the layer lock, alpha lock, clip below, reorder, add / duplicate / merge / delete |
| Colour | the wheel in three shapes, the hex readout, and the document palette |
| Transform | scale, rotation, lock, flips, apply / cancel, resampling |
| Pages | add, select, reorder, delete |
| Page | size, extend on four sides, crop, trim |
| Text | add, convert, load a font, and the whole caption editor |
| Select | the four tools, the wand's three settings, fill and clear |
| History | the depth readout, undo / redo as buttons, and the housekeeping the old panel used to carry: the autosave line, tile residency and traffic, and the pen and frame timings |
| Canvas | the artwork itself; never a described panel |

History is **ahead** of the old panel rather than level with it: the old one was a readout and
nothing else. A real list -- rows you can click to walk back to -- is not reachable, and
`panels/history.rs` says exactly why in its module comment and exactly what would be needed.

## The prompts are part of it too

The unsaved-changes question, the offer of recovered work and the export dialog are drawn by the
same descriptor layer, in the same theme, and land in the same control atlas -- see
`crates/openpaint-app/src/prompt.rs`. While one is up the workspace is told the pointer is not its
(`workspace::Attention`), so the panels behind it grey out and cannot be pressed through.

## What the vocabulary still lacks

Three things the ported panels wanted and did without. None is blocking; each shows as a small
compromise argued in a comment where it bites.

- **A disabled state.** A command that cannot apply right now has two shapes in these panels, and
  the panels converged on when to use each: one inside a *positional* group stays and refuses out
  loud, because omitting it slides its neighbours and the same spot starts meaning two different
  things; a single conditional button is replaced by a label naming what it waits for. Both are
  right, and both would be better as a control that is visibly *there but not now*.
- **A destructive row action.** `RowMark` is a switch: it reports a flipped state and the engine
  obeys. "Forget this preset" and "forget this colour" are not states, and two panels independently
  declined to dress a delete as a toggle. Both put a button under the row instead, which costs a
  row each.
- **A subtitle on a row.** The old panel's preset rows carried "Size 8, spacing 0.25, tip …" as
  hover text. There is no hover in the vocabulary — and a pen has none to give — so that
  information is gone rather than moved.

## Two things worth knowing before trusting this work

- **An agent's report is not evidence.** Nine panels were written in parallel and all nine reported
  clean. The screenshots then showed every explanatory label clipped at the panel's edge, in every
  one of them — because `Control::Label` was one row tall whatever it said, and four of the nine
  had quietly worked around it by splitting a sentence into three labels. Render it and look.
- **The screenshot has to draw the real thing.** The first version of the panel screenshot drew
  `filler` — stand-in sliders under the ported panels' names. It looked entirely convincing and
  said nothing at all.
