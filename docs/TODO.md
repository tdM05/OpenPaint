# OpenPaint — TODO

> Work that is **decided but not done**. Different from the other two docs on purpose:
> `DECISIONS.md` records what we agreed and why; `OPEN_QUESTIONS.md` holds things we
> have not decided yet. This is the list of things we know we want, have not built,
> and would otherwise forget.
>
> An item leaves this file when it ships, and the reasoning behind it moves to
> `DECISIONS.md`.

Last updated: 2026-09-02

> UI work has its own document: **`UI_PLAN.md`**. This file keeps what is not part of
> that sequence.

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
| Paint on a hidden layer | **refused, and says so** — a stroke that lands and shows nothing is indistinguishable from a broken brush | done 2026-09-02 |
| Paint on a locked layer | **refused, and names the layer** | done 2026-09-02 |
| Copy or cut with nothing selected | says so | done 2026-09-02 |
| Fill / delete with no selection | status line | fine, but should not be panel-only |
| Bucket click entirely outside the selection | status line | good — this is the model |
| Saving a colour already in the palette | status line | good |
| Merging the bottom layer | status line | good |
| Transform with no selection | status line | same |
| Transform an empty selection | status line | same |
| Press outside the transform box | nothing happens | probably right, but confirm it does not read as dead |
| Wand finds nothing at the seed | (unverified) | say "no region here at this tolerance" |
| Undo with an empty stack | **says how far the history goes** | done 2026-09-02 |
| An edit too large to record in history | status line, after the fact | should be *before*, or at least unmissable |
| Save fails (permissions, disk full) | (unverified) | must be a dialog, never a status line |
| A font in the document is not installed | reported in the panel | good — this is the model for the rest |
| A brush tip file will not load | status line, names the file | good — this is the model |
| A preset's tip has moved | status line, names it and says what it did instead | good |
| The brush library will not load | shown in the Brush section | good |
| Autosave fails | (unverified) | must be visible; silent autosave failure is the worst case here |

**The single place refusals go now exists** (`OpenPaint::refuse`, DECISIONS §6f), and painting
asks `Editor::paint_refusal` for the *reason* rather than each caller guessing. The guessing was
not hypothetical: every path said "this layer's alpha is locked", including on a text layer, where
it was untrue. The audit above is now "who calls `refuse`", which is greppable.

Later, and separately: a **dialog** for the few refusals that risk work — a failed save,
a failed autosave. Those must not be a line anyone can scroll past. Everything else the
status line already handles.

---

## 2. Known defects

- **`lifted::tests::a_rotation_is_not_slow` cannot do its job.** It asserts a wall clock, and a
  wall clock on hardware we do not control cannot tell a fast machine running the wrong loop from
  a slow one running the right loop -- it failed on a CI runner at 3.19 s with the loop correct.
  Worse, the defect it is named for measured *620 ms*, which is inside any bound loose enough to
  survive a shared runner. What it should assert is the number of source lookups per destination
  pixel: constant for the grid the transform uses now, per-filter-tap for the `HashMap` it used
  to. Until then it is a smoke test that the rotation finishes, and the doc comment on it says so.

### The crash that was here is fixed — 2026-09-03

Was: the app test binary occasionally died with `STATUS_ACCESS_VIOLATION`, roughly one run in ten,
always at process level and never in a named test. It also *hung*, which had not been noticed —
a run was caught sitting at 2.6 GB with nothing happening.

**The cause was the tests, not the application.** Seventy-odd of them each asked for their own
wgpu instance, adapter and device, and the harness runs tests across as many threads as the
machine has cores — so a run stood up and tore down dozens of D3D12 devices at once, on an
integrated GPU that shares its memory with everything else. Each test canvas also allocated a
128 MiB tile pool *up front* for a page smaller than a single tile.

Both are now what the application actually does: **one device for the binary**, and a budget sized
to what the tests draw. Peak memory went from **4144 MiB to 792 MiB** and a run from 13.5 s to
7.7 s, with all 652 tests still passing.

Worth keeping as a lesson rather than only a fix: a test that builds a configuration the
application never has is testing something nobody ships, and here it was also the thing breaking
the suite. The false negative it caused is recorded in §11a — a sabotage sweep once read one of
these crashes as "the sabotage was not caught".

---

## 3. Rendering the float on the GPU — **done 2026-09-02**

Was: a live transform resampled on the **CPU**, 33 ms per frame for a 256-pixel selection and
453 ms for a 1024-pixel one. The fix was §4a applied where it had not been, and it is now in
`float_pass.rs`: the lifted pixels go up as one texture at the lift, and each destination tile
gets a render pass whose fragment shader asks `Transform::invert` where each pixel came from.

What did **not** change, as planned: `Lifted`, the compositor, and the commit path, which keeps
the CPU Mitchell resample because it runs once per gesture and that is where the quality is worth
paying for (§5d).

The open question inside it is answered: the preview filters **bilinear**, in hardware. The commit
is Mitchell either way, so a preview very slightly softer than the result is the trade — which is
what every comparable application does. The reasoning is in `DECISIONS.md` §5h.

---

## 4. Contextual panels — designed, agreed, not built

Brush, Select, Transform and Text are four tabs in one strip as though they were four of a kind,
and they are three different kinds of context: tool options, a layer property, and a task in
flight. The spec, the prior art, what not to copy from Photoshop, the four things that will go
wrong and the order to build it in are in **`docs/CONTEXTUAL_PANELS.md`**, which also carries the
handoff for whoever picks it up.

Start with Properties-for-text: one module, clearest case, and it answers whether contextual
behaviour feels right before anything is restructured.

---

## 5. Deferred features (decided we want them; not now)

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
- **Layer groups** — structural; changes the document model and the compositor.
- **Marching ants that actually march.** The selection outline is dashed but static.
