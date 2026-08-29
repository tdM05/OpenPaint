# OpenPaint — TODO

> Work that is **decided but not done**. Different from the other two docs on purpose:
> `DECISIONS.md` records what we agreed and why; `OPEN_QUESTIONS.md` holds things we
> have not decided yet. This is the list of things we know we want, have not built,
> and would otherwise forget.
>
> An item leaves this file when it ships, and the reasoning behind it moves to
> `DECISIONS.md`.

Last updated: 2026-08-29

---

## 1. Nothing may fail silently ⚠️ the standing one

**Raised by the author, 2026-08-29:** *"if they are on a text layer, and try to draw,
it should notify them that they cannot paint because it is a text layer (it sorta has
a message, but the point is we should cover all edge cases in future so user is never
confused and it just does not work)."*

The principle is in `DECISIONS.md` §6b. This is the audit list that goes with it: every
place the app can decline to do something, and whether it currently says so.

The rule for each row: **a refusal must say what happened, why, and what to do instead.**

**The status line itself works** — bottom-left, and the messages it carries are good. The
gap is *coverage*: the rows below that say nothing at all. Prominence is a smaller, later
question, and only for the few refusals that risk work.

| Situation | Today | Wanted |
| --- | --- | --- |
| Paint on a text layer | status line, and only on the *first* refused stroke | in-canvas notice; offer "convert to raster" |
| Paint on a hidden layer | nothing happens at all | say the layer is hidden, offer to show it |
| Paint on a locked layer | (lock not built yet) | say so when it is |
| Fill / delete with no selection | status line | fine, but should not be panel-only |
| Transform with no selection | status line | same |
| Transform an empty selection | status line | same |
| Press outside the transform box | nothing happens | probably right, but confirm it does not read as dead |
| Wand finds nothing at the seed | (unverified) | say "no region here at this tolerance" |
| Undo with an empty stack | nothing happens | say "nothing to undo" |
| An edit too large to record in history | status line, after the fact | should be *before*, or at least unmissable |
| Save fails (permissions, disk full) | (unverified) | must be a dialog, never a status line |
| A font in the document is not installed | reported in the panel | good — this is the model for the rest |
| A brush tip file will not load | (unverified) | say which file and why |
| Autosave fails | (unverified) | must be visible; silent autosave failure is the worst case here |

**One thing to build before the table can be finished: a single place refusals go.**

Today each refusal writes its own string at its own call site, which is exactly how
coverage ends up patchy — the same shape as the pointer-capture bug in §4l, where every
tool had its own idea of who owned the pointer. One `refuse(reason)` seam, and the audit
above becomes "who calls it" rather than a list somebody has to keep up to date by hand.

Later, and separately: a **dialog** for the few refusals that risk work — a failed save,
a failed autosave. Those must not be a line anyone can scroll past. Everything else the
status line already handles.

---

## 2. Known defects

- **The app test binary occasionally dies with `STATUS_ACCESS_VIOLATION`** rather than
  failing a test — roughly one run in ten on the dev box, always at process level, never
  in a named test. Suspected wgpu/D3D12 teardown in the GPU cross-check tests. Not
  chased yet; recorded because a sabotage sweep read one such crash as "the sabotage was
  not caught", which is a false negative of exactly the kind §11a warns about.

---

## 3. The next real piece of work: render the float on the GPU

A live transform still resamples on the **CPU** (§5g). After the obvious waste was
removed it is 33 ms per frame for a 256-pixel selection and 453 ms for a 1024-pixel one
— usable at small sizes, not a live drag at large ones. That is not a tuning problem:
resampling is O(area) with a twenty-tap filter, so no amount of care makes the CPU the
right machine for a preview that has to keep up with a pen.

**The fix is §4a applied where it was not: produce the float's destination tiles with a
render pass instead of a loop.** The lifted pixels go up as a texture once at the lift;
each frame draws the destination tiles sampling through the inverse transform. Nothing
else changes — not `Lifted`, not the compositor, not the commit path, which keeps the
CPU Mitchell resample because it runs once per gesture and that is where the quality is
worth paying for (§5d).

Worth deciding rather than assuming, because it is the same class of change as layer
groups: a new pipeline writing into the tile pool's array texture.

Open question inside it: whether the preview filters with the hardware's bilinear or a
cubic in the shader. The commit is Mitchell either way, so the only question is whether
a preview that is slightly softer than the result is acceptable — every other app says
yes.

---

## 4. Deferred features (decided we want them; not now)

- **Cursor feedback on the transform box** — a scale cursor on the handles, a rotate
  cursor on the ring. Needs a cursor seam that works for pen as well as mouse (Q14).
- **Numeric entry while transforming** — type an exact angle or size. The panel has the
  fields; a real UI wants them on the box.
- **Snapping** — rotation to 15°, scale to whole percentages, both with a modifier.
- **Transform a whole layer**, not only a selection.
- **Text**: on-canvas text box with a caret and handles; a vertical-align field;
  colour fonts; variable-font axes. Vertical writing mode is modelled but unimplemented.
- **Speech bubbles** — the author flagged wanting "a good text bubble system later".
  Almost certainly a vector shape plus a text block plus a tail, which is why the text
  layer was built on content-is-truth rather than on pixels.
- **Brush presets** — unblocked now that a tip can be a bitmap (§4p).
- **Layer groups** — structural; changes the document model and the compositor.
- **Marching ants that actually march.** The selection outline is dashed but static.
