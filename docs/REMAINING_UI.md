# What is left before the old side panel can be deleted

The workspace is the UI: the application opens into it, and every section of the old side panel
that an artist reaches for has a panel of its own. **F2 still switches**, and the old panel is
still there for one reason, named at the bottom of this file.

Checked against the code, not from memory.

## Moved

One module each, in `crates/openpaint-app/src/panels/`. Each exports one `show`, and every one of
them is a pure `controls(...)` building the list plus a pure `picked(...)`/`answer(...)` mapping a
change — which is what makes them testable without a GPU.

| Panel | What it holds |
|---|---|
| Menu | File / Edit / Layer / Select / View, and the panel list |
| Tools | six tools, with icons |
| Brush | size, opacity, hardness, spacing, roundness, angle, flow, stabilisation; presets with a name field; the tip; all six response curves with their source pickers; reset |
| Layers | the list with per-row eyes, blend, opacity, lock alpha, clip below, reorder, add / duplicate / merge / delete |
| Colour | the wheel in three shapes, the hex readout, and the document palette |
| Transform | scale, rotation, lock, flips, apply / cancel, resampling |
| Pages | add, select, reorder, delete |
| Page | size, extend on four sides, crop, trim |
| Text | add, convert, load a font, and the whole caption editor |
| Select | the four tools, the wand's three settings, fill and clear |
| History | the depth readout, and undo / redo as buttons |
| Canvas | the artwork itself; never a described panel |

History is **ahead** of the old panel rather than level with it: the old one was a readout and
nothing else. A real list — rows you can click to walk back to — is not reachable, and
`panels/history.rs` says exactly why in its module comment and exactly what would be needed.

## The one reason the old panel is still here

Three read-only sections, all diagnostics, with no home in the workspace:

- **View** — zoom and rotation as numbers.
- **Canvas memory** — GPU tiles resident, tiles spilled to the CPU, and the traffic between.
- **Speed** — stroke and frame timings, pen sample rate, step size, and the autosave line.

They were always meant to stay out of the workspace: they are development instruments, not artist
tools, and a panel of them beside the brush settings would be a workspace that mixes two audiences.
But **deleting the old panel deletes them**, so one of these has to happen first:

1. A **Diagnostics** panel, listed in `PANELS` like any other and simply not in the default
   arrangement — reached from the panel list when it is wanted. Cheapest, and it keeps them out of
   the artist's way without throwing them away.
2. Or they move into the workspace's own settings popup, which is already where the look is set.
3. Or they are genuinely no longer wanted, and go.

Until one of those, `F2` is the way to them and the old panel earns its place.

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
