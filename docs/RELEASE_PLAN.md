# The road to a first release

> What is missing before OpenPaint can be someone's only tool, in the order it will be built.
> Every row was checked against the code, not remembered. An item leaves this file when it ships
> and its reasoning moves to `DECISIONS.md`.

Last updated: 2026-09-02

**Status: all seven are built, and the suite is clean** — 970 unit tests, 28 driven scenarios,
1125 assertions, no failures. What each item needed is under it; `DECISIONS.md` carries the
reasoning and `DRIVE_LOG.md` carries what the driving found.

## What is left before tagging one

Nothing in this document, and `TODO.md` §2 -- the crash -- is closed as well. What remains is
release engineering rather than product: a version number (the workspace is still `0.0.0`), a tag,
and CI run once on the new dependencies. The `.exe` is already built by CI on every push, and the
README now says what works and what does not.

Two things only the author can check, and both are known: **it has never run on another machine**,
and **pen pressure has no automated coverage** -- the harness injects a mouse, which Windows Ink
presents as a pen at constant pressure, so all six response curves are exercised only by a real
hand.

## What is already there

Worth writing down, because a plan that lists only holes reads as if there is nothing else.

The document format with autosave and crash recovery; layers with blend, opacity, clip-below,
alpha lock, reorder, merge, duplicate and delete; multi-page documents; selection by lasso,
rectangle and wand, with a transform; **the bucket** — the wand with `fill_on_click`, which is on
by default; **the eyedropper** — Alt and a press, reading the composite; text layers with font
substitution and IME; the brush engine with presets, a loadable tip and six response curves; the
tiled GPU canvas with CPU spill; and the panel workspace, which is now the whole UI.

## The order, and why this order

Each of these is a thing an artist hits on the first afternoon. They are ordered by *how early*
that is: nothing else matters if a picture cannot get in or out.

### 1. A picture can get in — **done**

`Open` used to accept `.openpaint` and nothing else, with no other way to bring an image in at
all: a scanned pencil sketch, which is how most comics pages start, could not be opened.

- `import.rs` decodes PNG and JPEG to 8-bit sRGBA, deciding the format **by the bytes**, not by
  the file's name.
- **Open** an image: a document sized to it, with `document_path` left empty so Save asks where to
  put a `.openpaint` rather than writing over the picture.
- **File > Place image** puts one on a new layer, centred at its own size, undoable in one step.
- Refused out loud when it is not an image, is damaged, or is larger than a page can be (§6b).
- Driven by `tools/scenes/import.txt`.

### 2. A page can get out — **done**

`Ctrl+E` used to write one PNG of the current page, under a timestamped name, into whatever
directory the application happened to start in — a debugging convenience, not an export.

- An **export dialog**, built from the same `Control`s as every panel and modal like the other
  prompts, then a save dialog for the destination.
- This page, every page as numbered files, or **every page stacked into one tall strip** — the
  webtoon delivery format.
- A **size**, 10–100%, area-averaged in linear light while the tiles are read, so a quarter-size
  export never materialises the full-size image.
- Driven by `tools/scenes/export.txt`, which reads the written files back off the disk.

### 3. Pixels can be copied — **done**

There was no clipboard at all, within the application or with the system.

- Ctrl+C / Ctrl+X on a selection, Ctrl+V onto a new layer, and all three in the Edit menu.
- Through the **system** clipboard (`arboard`), so a browser or a photo viewer can hand a picture
  over and take one back.
- From the *active layer*, never the composite — see DECISIONS §7c.
- Driven by `tools/scenes/clipboard.txt`, whose round trip goes out through the operating system
  and back.

### 4. A layer can be locked — **done**

`Layer::locked`, schema **v8**, a switch in the Layers panel, and a refusal that names the layer.
Distinct from `lock_alpha` and from hiding — DECISIONS §7d. Driven in `tools/scenes/layers.txt`.

### 5. Changing a layer can be taken back — **done**

Opacity, blend, visibility, clip-below and the lock were all outside the undo stack: setting a
layer to 20% by accident and pressing Ctrl+Z took back *the stroke before it*. Now one
`layer::Settings` on each side of one history entry, coalescing while a slider is dragged —
DECISIONS §7d. Driven in `tools/scenes/layers.txt`.

### 6. Nothing fails silently — **done, for the rows that were silent**

`OpenPaint::refuse` is the one door, and painting asks `Editor::paint_refusal` for the *reason*
rather than each caller guessing — the guessing was saying "this layer's alpha is locked" on text
layers, where it was untrue. Now spoken: painting on a hidden layer (refused outright), on a
locked one, undo or redo with nothing left, a wand that matches nothing at its tolerance, copy or
cut with no selection, and **a failing autosave**, which used to print to a console a windowed
application does not have. DECISIONS §6f; the audit table in `TODO.md` §1 is updated.

### 7. A live transform keeps up with the pen — **done**

`TODO.md` §3, unchanged: the preview resamples on the CPU — 33 ms for a 256-pixel selection and
453 ms for a 1024-pixel one. The design is already written down: produce the float's destination
tiles with a render pass instead of a loop, keeping the CPU Mitchell resample for the commit.

Now `float_pass.rs`: the lifted pixels go up as one texture at the lift, and each destination tile
gets a render pass whose fragment shader is `Transform::invert` in WGSL. The commit still resamples
Mitchell on the CPU, because it runs once per gesture — DECISIONS §5h.

**The one item here with no UI surface**, so it is proved by tests rather than by a scenario: the
GPU tiles are compared against `Lifted::transformed`, the code they replace and the code the commit
still uses, and a second test knows independently where a scaled, moved square should land. Five
sabotages — the rotation reversed, the translation dropped, the scale applied instead of undone,
the transparent margin removed, every tile handed the first tile's parameters — were each caught.

## What each of these means by "done"

Not "it compiles". For every item:

1. Unit tests for the part that can be tested without a GPU, and a **sabotage check** on each new
   guard — the guard broken on purpose, the named test seen to fail.
2. A driven scenario in `tools/scenes/` that operates the real application through the real UI and
   asserts against the state the application reports about itself. A feature with no scene is a
   feature nobody has watched work.
3. `cargo test --workspace` and `cargo clippy --workspace --all-targets` clean.

## Not in this release, and said so

Layer groups; speech bubbles; marching ants that march; on-canvas text editing with a caret;
numeric entry and snapping on the transform box; cursor feedback on the handles; PSD import or
export; CMYK and print colour management. Each is in `TODO.md` §4 with its reasoning.
