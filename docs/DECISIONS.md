# OpenPaint — Decisions & Direction

> Living record of what we've agreed on. Update this whenever a decision is
> made or changed. Anything still undecided lives in `OPEN_QUESTIONS.md`.

Last updated: 2026-08-29

---

## 0. Project context (why this exists)
Side project by an undergrad (heading toward a PhD in autoformalization). Built
because the author loves art / wants to make comics, and **CSP EX is effectively
the only full option but costs $300+ for a perpetual license with no further
updates**. Goal: a genuinely good, free, open alternative — not a rush job.
Quality over speed; correctness of the core over feature count.

---

## 1. Vision

An open-source digital drawing/painting app. **Quality first** — never skip
steps for the sake of speed. Positioned as an open competitor to Clip Studio
Paint (CSP), which is an excellent tool but expensive and closed.

Primary purposes:
1. **Creating web comics** — both infinite-scroll webtoon and traditional
   print-style pages.
2. **Digital sketchbook** — with genuinely good **page management**, which the
   industry currently does poorly.

Design/UX north star: **CSP and Procreate**. We like CSP's design and layout.
We explicitly do NOT want Krita's bloat or its layout/interaction choices.

### 1a. CSP EX is the north star for UX — but NOT for internals
Decided 2026-08-27, to keep design decisions simple and consistent: when in
doubt about *what a feature should be or look like*, do what CSP EX does. Its
UX, feature model, layout, tool/sub-tool hierarchy, brush setting sections, and
page management are the reference.

**Three places where copying CSP would be a downgrade — deviate deliberately:**
1. **Color depth.** CSP works at 8 bits per channel and composites in sRGB. We
   use linear `Rgba16Float` (§4b). CSP's ceiling shows up as banding in gradients
   and dark fringing on soft brush edges. Do not follow it down.
2. **Rendering architecture.** CSP is largely CPU-based with GPU acceleration
   bolted on, which is why it struggles with large canvases and many layers.
   Follow Procreate's GPU-first model instead (§4a).
3. **File format.** CSP's is closed. Ours is an open documented container (§7).

Short version: **CSP for UX, Procreate-and-better for the engine.**

### Explicitly NOT doing
- **Not building on / forking Krita.** It's GPL, ~1M+ lines of C++/Qt, and its
  bloat *is* the codebase. Its UI is welded to Qt; matching a CSP feel means
  fighting the framework. We may *study* it (subject to license) but will not
  drag in its code or architecture.
- Not targeting bloat/feature-maximalism. Opinionated, focused, CSP-flavored.

---

## 2. Target platforms & hardware

- **Primary target: Windows** with pen/tablet hardware:
  - Microsoft Surface (Surface Pen)
  - Veikk tablets
  - Wacom tablets
- All three speak **Windows Ink** (pressure + tilt), so one API covers all three
  for v1. **Verified on the Veikk 2026-08-27** — pressure varies correctly and
  the tool enumerates as `name="Stylus"`, `axes=PRESSURE | TILT`.
  - ⚠️ **But it is off by default in the tablet driver.** The Veikk driver ships
    with its "Windows Ink" option disabled, and until it's enabled the pen never
    reaches Windows Ink *at all* — on either RealTimeStylus or `WM_POINTER`. Our
    app saw only a tool named `"Mouse"` with zero axes, and Krita lost pressure
    the moment it was switched off Wintab. Enabling the toggle fixed both.
  - This is a **product problem, not just a dev problem**: our future users will
    hit exactly this and conclude the app is broken. See OPEN_QUESTIONS Q10d.
- **Wintab** (legacy Wacom API) — stays deferred (Phase 4), as originally
  planned. It was briefly thought to be the only working path on the Veikk; that
  was a driver misconfiguration, not an API limitation. Some pros still insist on
  Wintab and CSP supports both, so it remains wanted eventually — just not
  urgent. (Note: webview/Chromium stacks can't use Wintab at all, which is one
  more reason we're going native.)

### Confirmed test hardware
Author's Windows/tablet machine: **NVIDIA GeForce RTX 3070 Ti Laptop GPU**,
wgpu running via **DirectX 12**. The pipeline (Linux code → GitHub Actions →
download .exe → run on Windows) is verified working end to end, including wgpu
init and frame rendering. **This is the dev box, not the target spec** — see
below.

### Performance target → Surface-class integrated GPU, any canvas size
Decided 2026-08-27:
- **Minimum target is a Surface**, i.e. integrated graphics with **shared system
  memory**, not the author's discrete 3070 Ti. The dev box is the fast case, so
  it will happily hide problems; treat integrated as the bar.
- Stay on **portable wgpu features** — prefer standard render pipelines and
  fragment shaders over exotic capabilities, keep near default/downlevel limits,
  and use nothing D3D12-specific. This is also what keeps Linux/macOS/ARM cheap
  to add later (§6 "cross-platform by construction"), including ARM Surfaces.
- **All canvas shapes are in scope** and none is privileged: webtoon strips
  (~800–1600 px wide, potentially 10,000 px+ tall), print comic pages at 300 DPI,
  and screen-resolution sketchbook pages. Design for the general case.

✅ **Consequence: tile residency is an early problem, not a Phase-2 one.** Acted on
2026-08-28 (§4d). A4 at 300 DPI is 2480×3508 ≈ 8.7 Mpx. At `Rgba16Float`
(8 bytes/px, §4b) that's ~70 MB *per layer*, so a ten-layer page is ~700 MB before
the composite target and per-stroke accumulation buffer — which does not fit in a
Surface's shared graphics memory. Therefore:
- The GPU holds a **bounded pool of resident tiles**, never the whole document.
  **Done** — `tile_pool.rs`, capacity fixed at startup from a byte budget.
- A tile budget + eviction policy is a **first-class early component**. **Done** -
  `tile_store.rs` spills non-resident tiles to the CPU, so document size is not limited
  by graphics memory. The budget itself is a heuristic on adapter type, because wgpu
  exposes no memory query (Q13).

### Development vs. validation — CONFIRMED SETUP
- **Code is written on Linux (SSH).** The **tablet lives on the author's Windows
  machine.** So the real iteration loop is:
  *write code on Linux → produce a Windows build → author runs it on Windows and
  gives feedback with the actual tablet.*
- **Consequence: Windows Ink is the PRIMARY input path from early on** — it's the
  only place real stylus feel gets validated. The mouse-on-Linux path exists only
  for mechanics/quick checks, not for feel.
- **A reliable Linux→Windows build story is required from day one.** Rust supports
  this (cross-compile to the `x86_64-pc-windows-*` target from Linux, and/or a
  native Windows build). Details tracked in OPEN_QUESTIONS Q2b.
- Rust + wgpu is cross-platform (Vulkan on Linux; D3D12/Vulkan on Windows) from
  the same code, so everything except stylus feel is still verifiable on Linux.
- **Input is a swappable backend behind one interface.** `WindowsInkBackend`
  (primary) and a minimal `LinuxTabletBackend`/mouse path both feed the same
  event stream. (Author has a Wacom/Veikk, but it's on the Windows box, so the
  Linux tablet backend is low priority.)

---

## 3. Technology stack (proposed, engine-first)

> This is the working plan; not treated as fully locked but we're proceeding on it.

- **Core language: Rust.** One language end-to-end; core engine crate is
  portable (native now, wasm-lite viewer possible later).
- **Rendering: wgpu.** Modern, cross-platform GPU (Vulkan/D3D12/Metal), lets us
  do GPU-accelerated tiled compositing in linear color space.
- **Windowing/input: winit** to start.
- **UI framework: DEFERRED.** Start with a throwaway `egui` debug panel
  (sliders/toggles) for engine work. The polished CSP-like UI is a later,
  *reversible* decision. Keep the engine fully independent of UI choice.

### Architecture principle
**Isolate the engine from the UI.** A Rust core crate (document model, tiled
canvas, brush engine, compositor, file I/O) that knows nothing about buttons or
windows. Thin UI/shell on top. This makes the risky part (engine) portable and
the reversible part (UI) swappable, and lets one engine back a native app, a
wasm viewer, and CLI export tools later.

---

## 4. The three hard systems (where quality lives)

These, not features, are what separate a pro tool from a toy:

1. **Stylus latency & input fidelity** — raw pointer input with pressure + tilt,
   event coalescing + prediction, driven by a tight GPU render loop. The single
   biggest "feel" factor.
2. **The brush engine** — stamp/dab-based strokes placed by spacing along the
   path; pressure→size/opacity/flow curves; texture; dual brushes; wet blending;
   smudge. The soul of the app and the largest single body of work.
3. **GPU tiled compositing** — layers, blend modes, masks, clipping groups,
   composited on GPU; canvas stored as tiles (e.g. 256×256) so large multi-layer,
   multi-page docs don't blow up RAM or stall. Blend in **linear color space**.

### 4a. Where the brush engine lives → core emits dabs, GPU rasterizes them

Earlier drafts said both "the core is pure Rust with no OS calls" and
"compositing happens on the GPU." Those conflict, because real quality requires
rasterizing dabs on the GPU. Resolution:

- **`openpaint-core` owns the dab *math*** — where each dab lands along the path,
  its radius, and its pressure/tilt-derived parameters — plus tile storage, the
  layer tree, the document model, and file I/O. Pure, portable, no GPU types.
- **The renderer owns dab *rasterization* and compositing** — it consumes the dab
  stream and stamps into tile textures on the GPU.

Why this and not the alternatives: it's how the field works (Krita's brush
engines emit dabs into tiled paint devices; Photoshop and CSP are stamp-based;
Procreate rasterizes dabs on the GPU). Keeping dab *generation* pure makes it
deterministic and unit-testable, which is the only credible way to chase
Photoshop's falloff curve (Q7a) rather than eyeball it. The existing CPU
`Canvas::blend_pixel` path is retained as the **reference implementation** that
tests compare GPU output against.

**BUILT (2026-08-27).** GPU dab rasterization lives in
`openpaint-app/src/stroke_layer.rs` + `stroke.wgsl`. Structure worth knowing:
- Dabs stamp into a **single-channel accumulation texture**, and the canvas is not
  touched until the stroke ends. Mid-stroke the stroke is composited on top for the
  preview; on stroke end it is baked into the canvas once.
- That is what lets the flow/opacity model work with **no snapshot and no
  readback** — the CPU reference needs a snapshot precisely because it composites
  into the canvas on every update.
- The accumulation formula `a += flow·cov·(1−a)` *is* standard "over" blending, so
  blend factors `(One, OneMinusSrc)` make the hardware compute it. The fragment
  shader only outputs `flow · coverage`.
- **Consequence: the GPU is now authoritative for pixels.** The CPU `Canvas` holds
  the document's dimensions and the tile machinery the future cache/readback will
  use (Q13), but not painted pixels.
- The falloff curve therefore exists **twice** — `Dab::coverage_at_distance` and
  `dab_fs` in the shader. Two copies of a curve drift, so
  `stroke_layer.rs`'s tests rasterize the same dabs both ways and compare pixels.
  That test is what makes keeping the CPU reference worthwhile rather than dead
  weight.

**Accumulation, not rasterization, is the hard part — DONE.** Dabs within one
stroke cannot simply be alpha-blended in a batch: **flow** accumulates per dab
while **opacity** caps the stroke's total contribution, per *stroke* rather than
per layer (so a second stroke builds on top of the first). Implemented in
`openpaint-core/src/stroke.rs`.

The mechanism turned out to matter beyond the brush. Showing a stroke build up
live requires keeping the **pre-stroke state of every tile the stroke touches** and
re-compositing them as dabs land, because the result must be *recomputed* rather
than progressively darkened. Recomputation is idempotent, which is what makes a
mid-stroke preview and the committed result identical.

That snapshot is exactly the copy-on-write tile snapshot undo needs (Q13). **A
correct brush and undo share one mechanism**, which is why this landed before
either layers or history.

### 4e. Layers composite per tile, through a cache — decided 2026-08-28

Settled before writing the layer stack, because it decides the shape of everything above it.
Rather than drawing N layers to the screen each frame, the stack is composited **per tile**
into a cache tile, and only the cache is drawn.

Four consequences, and together they are the reason:

1. **Blend modes stop needing a destination read.** The compositor *samples* each layer as a
   texture, so Multiply, Screen and Overlay are plain shader maths. The obvious alternative -
   compositing through an intermediate page-sized target so the blend unit can read it - would
   have reintroduced exactly the ceiling 4d removed.
2. **Residency stops scaling with layer count.** Peak need is the visible cache tiles, the
   active layer, and one tile per layer *transiently* while recompositing. A screenful of a
   20-layer document fits in a couple of hundred tiles, so a single 256-layer array texture is
   enough and the pool needs no multi-texture complication.
3. **Display cost is independent of layer count** - one instanced draw - and recompositing
   touches only tiles that changed.
4. **The stroke preview stops being a special case.** It slots into the middle of the stack by
   recompositing the touched tiles with the accumulation injected at the active layer's
   position: same compositor, same maths. So the preview is *exactly* the committed result
   rather than an approximation of it, which is the property the current separate preview pass
   only approximates.

One thing this changes about pixels: **a layer tile is transparent where unpainted, not
paper.** The paper moves to the bottom of the compositor, where the sheet quad effectively is
today.

⚠️ **The cache itself is deferred; the compositor landed first.** Revised while building,
2026-08-28. The compositor runs in the display pass, sampling every layer per screen pixel,
with no cache tile in between. Reasons, in order:
- The compositor is the risky part and the cache is a pure optimisation on top of it. Landing
  the risky part alone, verifiable, was worth more than landing both at once.
- Output is identical either way, so this is not a quality trade. Points 1 and 4 above —
  blend modes as plain maths, and the preview being exactly the committed result — come from
  the *compositor*, not the cache, and are already true.
- Cache invalidation is the kind of thing to design against observed usage rather than
  guessed usage.

What is genuinely deferred with it is point 2: **residency does scale with layer count for
now**, because every visible tile of every layer must be resident at once. Roughly, layers ×
visible tiles must fit the pool, and exceeding it reports the same "zoom in" message spilling
already uses. Point 3 goes too: per-frame cost scales with layer count, though sampling a few
layers per pixel is cheap enough that this will not be what bites first.

The cache becomes worth building when real layer counts make either of those bite. It does not
change the layer model, the blend modes, the UI, or the shader's maths — only where the
compositor writes.

### 4c. Brush modularity → composable per *dab*, fixed per *pixel*

The question was whether to make brush features arbitrary plug-in components
(Blender-modifier style) so users can invent brushes. Answer: **yes, but only on
the per-dab side of the boundary.** Two real reasons, neither of them "other
apps don't do it":

**1. Loop nesting, not component cost.** Components are cheap; it matters which
loop they sit in. Dabs per stroke are in the hundreds, pixels per stroke in the
millions (see `openpaint-core/src/dab.rs` for the arithmetic) — three to four
orders of magnitude. Per-dab dispatch is free. Per-pixel, a *dynamic* stage list
is not expressible on a GPU at all: WGSL has no function pointers and WebGPU no
dynamic shader linking. It degrades into either shader-permutation explosion
(2^N variants; lazily compiling one mid-stroke is a frame hitch exactly while
drawing) or an uber-shader whose register pressure drops occupancy so a plain
round brush runs at the speed of the most complex brush. On the Surface-class
target (§2) neither is affordable.

**2. Composition needs a uniform type.** Blender modifiers compose arbitrarily
because every one is `Mesh → Mesh`. Brush stages are heterogeneous — some adjust
scalars, some inject randomness, some change dab *count*, some change pixel
appearance — so "any stage in any order" has no well-defined semantics ("what
does Texture-before-Spacing mean?"). But there *is* a uniform type available:
`Dab → Dab`. That composes cleanly in any order, and it covers most of what
users actually want to invent with — size and pressure response, scatter, jitter,
angle, roundness, spacing, color dynamics, tilt response.

So: **per-dab is a composable, serializable, user-authored stage list. Per-pixel
dab appearance is one fixed parameterized shader**, extended deliberately by
adding parameters rather than by arbitrary composition.

This also matches CSP's actual model (§1a): a fixed set of toggleable setting
sections, each optionally driven by pressure/tilt/velocity through a curve —
which is the modulation layer, evaluated per dab.

### 4d. The canvas is a bounded pool of GPU tiles, settled 2026-08-28

Replaced the Phase-0 shortcut of one page-sized texture. That shortcut had been
recorded as temporary from the start, and it was costing three things at once:

1. **Two ceilings the UI had to apologise for.** A page could be no larger than
   `max_texture_dimension_2d` (8192), and no larger than one allocation the driver
   would accept (~16 Mpx at `Rgba16Float`). A real webtoon strip is taller than
   both. The app clamped and printed an explanation.
2. **Memory proportional to page area rather than painted area.** A blank
   800×20000 strip cost 128 MB before a mark was made.
3. **Nowhere to put non-destructive crop.** Storage was defined *as* the page, so
   pixels outside it could not exist (§5c).

All three had one cause, so all three were fixed by one change.

**Shape: a 2D array texture, one layer per tile.** The alternatives were weighed:
- *A texture per tile* needs a bind group and a draw call per tile — hundreds of
  each per frame.
- *A 2D atlas* (a tile grid inside one big texture) makes adjacent tiles physical
  neighbours, so any filtered sample near a tile edge bleeds in whatever tile
  happens to sit beside it. Avoiding that needs apron pixels round every tile,
  which cost memory and have to be kept in sync on every write.
- *An array texture* gets the single bind group without the bleed, because each
  layer is its own image with its own clamped edges. The canvas is **one instanced
  draw** over the visible tiles, with the layer index arriving as instance data.

**The page rectangle bounds drawing and painting; it does not bound storage.**
Tile quads are clipped to the page *in page space* — not with a scissor rectangle,
which cannot work once the canvas is rotated on screen. Dab quads are clipped the
same way, per corner, and the distance-from-centre is derived from the clamped
position so coverage stays exact.

**Accumulation is tiled too, and had to be.** Opacity caps a stroke's *total*, so
the accumulation buffer must cover everywhere the stroke has been — potentially
the whole canvas. A page-sized accumulation texture would have put the ceiling
straight back, so a stroke allocates accumulation tiles on demand and releases
them when it commits.

**A tile is now the unit of undo.** Snapshots were arbitrary rectangles; they are
whole tiles copied into a snapshot pool of the same shape. That makes the history
budget *exact* rather than estimated — the pool's capacity **is** the budget, so
there is no byte counter that can disagree with what the GPU holds.

**What this deleted, which is the strongest argument for it:** the page-origin-to-
texture-origin conversion at the GPU boundary (tiles are addressed in page
coordinates, so §5a's invariant now holds unbroken from core to GPU); the
reallocate-and-blit on every resize; `View::placement` and the four-corner
placement uniform; `CanvasRect`, `BoundsBuilder`, and the stroke bounds the editor
tracked for history's benefit; `Op::Resize`'s pre-crop snapshot and
`PageResize::loses_pixels`; and both size ceilings with their clamping and
messages.

**What remains bounded, honestly.** Residency is 96 MiB (192 tiles), which holds a
fully-inked A4 at 300 DPI (140 tiles). A long webtoon inked throughout exceeds it,
and until spilling lands the pool reports exhaustion and says so in the status bar
rather than dropping paint quietly. The one remaining page limit is 65536 px per
side, and it is a **coordinate-precision** limit (`f32` is exact on integers only
to 2^24), not a memory one.

### 4b. Tile pixel format → linear, premultiplied, `Rgba16Float`

- **Linear color space**, not sRGB. Already committed to above; this encodes it.
- **Premultiplied alpha**, not straight.
- **`Rgba16Float`** tile textures. sRGB conversion happens *only* at the final
  display blit (the surface is already an `*Srgb` format, so it's free).

Premultiplied is the one that matters most and the one that's cheap now and
expensive later: it's what makes layer filtering, masks, and blend modes correct,
and switching after the fact means auditing every blend site in the engine.

**Current code contradicts all three axes** and must be migrated before the brush
engine is written:
- `openpaint-core/src/tile.rs` — hardcodes RGBA8 (`TILE_BYTES`, `pixel_mut`
  returning `&mut [u8]`) and documents straight alpha.
- `openpaint-core/src/canvas.rs` — `blend_pixel` blends in sRGB space with u8
  rounding.
- `openpaint-app/src/canvas_renderer.rs` — imports `TILE_BYTES` and creates an
  `Rgba8Unorm` texture, so the format is an API-surface fact, not internal.

On-disk storage format is a **separate, later** decision (Q6) — 16-bit in memory
does not oblige 16-bit on disk.

### 6a. A layer has a source of truth; text is the first that is not pixels — 2026-08-29

The question text forced, and the one worth answering carefully: **a caption
typed on Monday has to be retypeable on Thursday.** That is impossible if what
the document keeps is the pixels a rasterizer once produced.

So `Layer` gained a `Content`:

- **`Raster`** — the tiles *are* the truth. A brush writes them, nothing
  regenerates them, and a stroke therefore survives forever.
- **`Text(TextBlock)`** — the block is the truth. The tiles are a *cache*,
  thrown away and rebuilt whenever the text, font or box changes.

**Nothing downstream changes, and that is the whole design.** The compositor
reads tiles keyed by layer id and neither knows nor cares which of the two
filled them, so blend modes, opacity, clipping, alpha lock, selection, export
and the file's tile table all keep working with no text-specific code in any of
them. Text was built without touching the compositor at all.

The one thing that does change is **who may write those tiles**, and it is asked
as a question about the *layer* — `Layer::accepts_paint` — rather than checked
per tool. Brush, eraser, fill and clear all inherit the answer, so a tool added
later cannot forget. Painting on a text layer would not fail at the time; it
would vanish the next time someone fixed a typo, which is far worse. The way out
is the same bargain CSP strikes: convert to raster, one-way, keeping the pixels.

`Content` is also where **vector layers** land. That is the reason this work
does not have to be redone to get there: the shape of the answer is already "a
layer has a source of truth", and a vector layer is another arm of the enum. A
speech-bubble system is that plus a shape object re-deriving alongside the text.

**Undo is the text changing, not the tiles.** A text edit costs a string in the
history stack instead of a tile snapshot, and undo is exact rather than a
re-rasterization that has to match.

#### Undo of a text edit is the text, not the pixels

`Op::Content` holds a `Content` on each side and **no tiles at all**. That is
the payoff of derived content rather than a saving bolted onto it: the pixels
follow from the block, so undo restores the block and re-derives. A caption
costs a string in the history stack instead of a snapshot of every tile it
covers, and the restore is exact rather than a re-rasterization that has to
match what was there before.

Undo and redo differ only in which side of the operation they take, so they are
one function. Restoring `Text` re-renders; restoring `Raster` leaves the tiles
alone, which is correct — converting to raster never changed a pixel, it only
stopped them being recomputed.

**Consecutive edits coalesce inside `History::push`**, on a 700 ms pause. Not
per keystroke, which would make Ctrl+Z walk back through a caption a letter at a
time; not per focus change either, which would make a long caption one
all-or-nothing entry. The merge keeps the *earlier* `before` and the *later*
`after`, which is what makes the merged entry describe the whole run. It refuses
to merge across layers however fast the edits arrive, since the merged entry
would otherwise restore one layer's words onto another.

#### The font stack is a separate crate, not a module

`openpaint-text` owns parley, swash and fontique; `openpaint-core` owns
`TextBlock` and has no font dependency at all. A module boundary would have made
the seam a convention — a crate boundary makes it a fact the compiler enforces,
so replacing the text stack cannot reach the document model, the file format or
the renderer.

parley over cosmic-text for its **span model**: per-word styling and Japanese
*furigana* are ranges of differently styled text inside one block, which is the
shape parley is built around. Both handle the horizontal Latin case that landed
first, so the tiebreak is what comes after it.

**What crosses the seam is an 8-bit coverage mask**, not positioned glyphs.
Glyph ids are meaningless without the font that produced them, so handing those
back would have leaked the library's types across the boundary the crate exists
to draw. Colour is applied where the mask becomes tiles, which is the path a
selection fill already takes — so text inherits correct linear blending instead
of reimplementing it.

#### A missing font is reported, not hidden

`FontSpec` is a *request*; `FontResolution` says what was actually used, read out
of the font file itself. Documents travel, and lettering silently reflowed into
a substituted face is the failure this whole path exists to prevent.

Better than CSP on one point, and it costs nothing: **the derived tiles are
saved too.** Open a document on a machine without the font and the page still
looks right, from the cache, *and* says the font is missing — rather than
quietly re-laying it out. Re-rendering happens on edit, not on load, which is
what makes that work.

#### Things deliberately not done yet

- **Vertical writing** (*tategaki*) is reserved in the model and the file format
  and unimplemented in the renderer, which returns `UnsupportedWritingMode`
  rather than falling back to horizontal. Setting a manga page silently sideways
  is worse than not drawing it.
- **An on-canvas caret.** The block is edited through an egui `TextEdit`, which
  already brings caret, selection, clipboard and IME — so Japanese and Korean
  input work today. Writing a text editor inside a panel that is explicitly
  throwaway (§3) would be work thrown away; the canvas caret belongs with the
  real UI.
- **Colour fonts** render as flat outlines, and **variable fonts** expose their
  named instances rather than continuous axes. Both are work, neither is a
  design limit.

#### Schema v6

`layer_text`, a separate table rather than fifteen mostly-null columns on
`layer`: a raster layer has none of them, and the file stays legible to anyone
dumping it. Absence reads as "no text layers", the same tolerance `lock_alpha`
established, so a v5 file loads without a branch on the version.

The migration also found a hazard worth keeping: every structure change so far
recreates the structure tables, and each branch had its own copy of the `DROP`
list. `layer_text` was added to `STRUCTURE_SCHEMA` and not to those, and *every*
migration failed with "table already exists". There is now one `DROP_STRUCTURE`,
and the branches collapsed into one, since they all did the same thing.

### 5d. Scale and rotate resample; a move must not — 2026-08-29

A whole-pixel move is a copy and is *lossless*. The moment a transform
rotates or scales, a destination pixel no longer lands on a source pixel and
its colour has to be reconstructed from the neighbours it falls between —
which degrades the image, every time it is applied.

So `Transform::is_a_plain_move` is checked first and takes the copy path.
That is not an optimisation: it is the difference between a move that can be
repeated a hundred times without damage and one that cannot.

**The filter is a cubic, and the default is Mitchell–Netravali.** Line art is
the worst case for resampling and it is what this app is for. Nearest
neighbour turns a rotated ink line into a staircase; bilinear turns it into a
blur; a cubic is the first that keeps an inked edge looking inked. Every
*interpolating* cubic overshoots at a hard edge, so the choice is how much:
Mitchell's negative lobe is about −0.035 against Catmull–Rom's −0.063, which
buys about half the ringing for a little sharpness. That is the right way
round for ink, where a dark halo beside a line is exactly the artefact a
printed page shows off. Catmull–Rom is offered anyway, because it
*interpolates* — an unmoved pixel comes back exactly, which Mitchell cannot
promise — and that suits flat colour.

**Minification widens the footprint.** Shrinking is the case a fixed-radius
filter gets wrong: many source pixels fall into one destination pixel, and
reading only the nearest few is undersampling, which reads as sparkle and
broken lines. Scaling the kernel's footprint by the minification factor is
the standard fix and needs no mip pyramid.

**Filtering premultiplied is why §4b was worth holding.** Interpolating
straight colour across an edge into transparency mixes in whatever is stored
in the fully transparent texels, which is a halo around every rotated
selection. Premultiplied has nothing to leak.

**The mask follows the pixels.** `Selection::transformed` resamples coverage
through the *same* filter, by treating coverage as alpha. An outline still
sitting square around rotated pixels would describe a selection of something
that is no longer there — the same bug undo had before `HistoryChange::Moved`
existed. Sharing the filter rather than writing a second loop matters because
a mask filtered differently from the pixels it covers would disagree with them
at exactly the soft edges both exist to get right.

A sabotage worth recording: removing the footprint widening did **not** fail
the first minification test, because a checkerboard averages to grey however
badly it is sampled. Evenly spaced thin lines are the case that separates
them, and the test now measures the *spread* across the output rather than its
average. Another instance of §11a's rule — a sabotage that does not fail tells
you about the test.

### 5f. A transform is edited on the canvas, not in a panel — 2026-08-29

Reported the morning after §5e shipped, and the report is worth quoting because it
names both halves: *"all transforms work, but moving does not… I use lasso to select,
then try to move by dragging after clicking transform, but then it just lassos again.
shouldn't it create some transform box just like photoshop or csp?"*

**The bug.** `decide_capture` asked which *tool* was up before it asked whether
anything was floating. The way you get a selection is to draw one, so the lasso is
still armed when the transform begins — and every press on the floating pixels started
a fresh lasso across them. Every panel control worked; the one gesture anybody reaches
for first did not.

**A transform in the air is modal**, and now sits in the same clause as an open prompt
rather than below the tools. That is what it *is*: something the artist is in the
middle of, which every press on the canvas belongs to until it is applied or abandoned.
The fix is one clause because §4l put the decision in one place — the previous design,
with a guard per tool, would have needed a guard per tool again.

**The missing half was the box.** A transform reachable only from a panel is a form,
not a tool. `transform_box.rs` is the eight handles, the rotation ring just outside
them, and the interior — the same shape as every raster app, because that shape *is*
how a transform is edited.

Four things in it are decisions rather than mechanics:

1. **The hit test runs in source space.** The pointer is mapped backwards through the
   transform and tested against the *untransformed* rectangle. Rotation then costs
   nothing — a rotated box is still grabbed by the corner it came from, not by the one
   it now resembles on screen — and there is one piece of geometry instead of two that
   have to agree. The grab tolerance is divided by the scale on the way in, or a
   selection at 400% would grow its grab zones with it and swallow its own interior.

2. **A drag is a pure function of the transform at the press.** Nothing accumulates, so
   dragging out and back is exact rather than nearly right. An incremental version
   passes every other test and drifts a little on each of the hundreds of samples a pen
   sends — which is the kind of bug that gets blamed on the resampling filter.

3. **Scaling pins the opposite corner without moving the pivot.** The tempting way is
   to move `Transform::pivot` to the anchor, but the pivot is also what rotation turns
   about, and rotation must stay about the centre of the box. So the pivot is left alone
   and the anchor is held by solving for the translation: one line of algebra rather
   than a second meaning for a field. Under rotation this is the case that separates a
   real solution from a plausible one — read the scale off the pointer without undoing
   the rotation first and the box shears instead of scaling.

4. **A quick drag is now the same machinery with the interior already grabbed.** §5e
   deliberately kept the quick drag separate; a second, simpler kind of drag is exactly
   how the two would come to disagree about what a move means. One `apply_grab` serves
   both, and the whole-pixel rounding that keeps a move lossless (§5d) lives in one
   place.

**The box hugs the coverage, which needed a new primitive.** `Selection::bounds` is
tile-aligned, and a box snapped out to 256-pixel tiles would hang off the artwork,
put handles where nothing was selected, and — worse — put the *pivot* up to half a tile
off centre, so rotating would swing the pixels instead of turning them in place.
`Selection::content_bounds` is the tight box. Both are kept: the resampling loop only
needs a rectangle it is safe to walk, and paying for a scan there would be waste.

**One overlay type for the crop rectangle and the transform box.** They are the same
drawing answering the same question, and §11a.8 is about exactly this — the day one
grew a handle the other would silently not have it.

A seam that stays untested and is named in the code: `begin_transform` needs a renderer
to lift, so nothing in it runs headlessly, and a sabotage swapping `content_bounds` for
`bounds` there passes the whole suite. Thirteen of the fourteen sabotages tried were
caught; that one is the gap, and it is written above the function rather than left for
someone to discover.

### 6b. Nothing may fail silently — 2026-08-29

Raised by the author while reporting the transform bug, and it generalises past it:
*"we should cover all edge cases in future so user is never confused and it just does
not work."*

**A refusal is a feature, not an omission.** When the app declines to do something —
painting on a text layer, filling with nothing selected, an edit too large to record —
it must say **what happened, why, and what to do instead**. An app that quietly does
nothing teaches the artist that it is broken, and they are not wrong to think so: from
where they sit, "refused for a good reason" and "bug" look identical.

Two consequences, both listed as work in `TODO.md` §1 rather than done here:

- **A status line on a debug panel is not a notification surface.** It is invisible to
  someone looking at their drawing, and the panel is throwaway anyway (§3). Refusals
  need a transient notice near the canvas, and a dialog for anything that risks work.
- **Refusals need one seam.** Today each writes its own string at its own call site,
  which is how coverage ends up patchy — the same shape as §4l, where every tool had
  its own idea of who owned the pointer. One place to call, and the audit becomes "who
  calls it".

`TODO.md` §1 holds the audit table: every place the app can decline, and whether it
currently says so. It is deliberately a table of *known gaps* rather than a claim of
completeness.

### 5e. A transform stays in the air until it is committed — 2026-08-29

A quick drag lifts, moves and puts down in one gesture. That is all one
button and no keyboard can express, and it is the right interaction for a
move — but it leaves nowhere to adjust a scale, because the moment you let
go it has landed.

So a *transform* is a session: lift, hold, adjust, then Enter to apply or
Escape to put it back. Both are the same machinery, and `Dragging.persistent`
is the one bit that says which is happening — so a release cannot silently
end a session someone is still working in.

**Added rather than substituted.** Pressing inside a selection still means
"drag it, commit on release", exactly as before. Making the persistent
session the only model would have been tidier and would also have changed
behaviour that already works, so the quick drag stays and the transform is
reached from the panel.

**Cancelling is free**, which is the property the lift/float/put-down split
exists for: the layer is untouched between the lift and the put-down, so an
abandoned transform is not an edit to undo but an edit that never happened.
Nothing reaches the history stack and the document is not marked dirty.

**`Op::Move` carries the transform and the filter**, not an offset. A redo
that resampled with a different kernel would produce different pixels from the
ones that were undone. It also carries the *source* mask rather than an offset
to invert: a non-uniform scale combined with a rotation has no inverse in this
parameterisation, and keeping the mask that was already being kept is both
exact and cheaper than deriving one.

The live outline is now mapped through the transform rather than offset by it,
so a rotated selection is drawn rotated. An outline is a path, and a path is
exactly what an affine transform is cheap to apply to.

One §11a.7 repeat caught by a sabotage: `commit_transform` and
`cancel_transform` both bailed out early when there was no renderer, skipping
the state changes below — so a sabotage that wrongly marked the document dirty
on cancel *passed*, because the line was unreachable in the test. Obligations
first, then branch. That hazard has now bitten three times.

### 4p. A brush tip is either a curve or a bitmap — 2026-08-29

The last thing that changes what a brush *is*, and what brush presets have been
waiting on: a chalk, a bristle brush and a screentone dot are not settings of a
soft disc, they are a different shape of mark.

`Tip` is the choice, and it sits exactly where the edge profile sat rather than
alongside it:

- **`Round(Curve)`** — coverage from distance, through §4o's edge profile.
- **`Stamp(Arc<Stamp>)`** — coverage read from an image.

**Dab geometry is unchanged.** Centre, radius, roundness and angle mean the same
for both, and a stamp is sampled through the *same* frame transform
`Dab::distance_to` uses — rotated back by the angle, minor axis stretched to the
major — so a bitmap tip squashes and turns with the same controls a round one
does. Without that the two kinds would answer differently to the same sliders.
The image is mapped across the dab's diameter, so its edge lands on the radius
and nothing that reasons about a dab's extent changes.

**Hardness applies to `Round` only, and the UI stops offering it.** A stamp
already carries its own edge — that is most of what it is — so a hardness
control over the top would be a second, contradictory answer to one question.
`Tip::falloff()` returning `None` is how the panel knows.

**Coverage, not colour.** One byte per texel. A coloured tip is a different
feature (a dual brush, or a stamped image), and mixing them would mean deciding
what happens when a coloured tip meets a brush colour — a question with no good
answer. The tip says where ink lands; the brush says what colour it is.

#### What looks like ink is ink

Two conventions exist and both are common: a tip drawn black-on-white with no
transparency (Photoshop's `.abr` tips), and one drawn opaque-on-transparent
(what exporting from any paint app gives you). Rather than making the artist
know which they have, an image that uses its alpha is read from alpha and a
fully opaque one from inverted luminance. Guessing is safe here in a way it
usually is not: a tip wrong under one rule would be *obviously* wrong — an
inverted mark — rather than subtly so.

#### A tip is an app resource, not document content

A tool you own, like a font, rather than part of the artwork. Nothing about it
is written into a `.openpaint` file, which is what keeps a document openable on
a machine that does not have the tip. When brush *presets* land they will
reference a tip the same way a text layer references a family — by name, with
the substitution reported.

#### The cross-check that matters most

The GPU reads the tip through a hardware sampler; the CPU walks the same
arithmetic by hand. That is the least alike the two rasterizers have ever been —
not two spellings of one formula but genuinely different machinery — so
`Stamp::sample` is written to mirror a linear `ClampToEdge` sampler exactly
(texel centres at `(i + 0.5) / n`), and the cross-check is the only thing
holding it there. The test tip is deliberately 13x9: not square, not a power of
two, and not symmetric, so row padding, a transpose and a flip all fail it.
Sabotages that do: the half-texel offset dropped, the sample transposed, the
angle ignored, rows uploaded unpadded, the stamped flag never reaching the
shader, and `set_tip` keeping the placeholder texture.

### 4o. The dab's edge is a curve, not a number — 2026-08-28

Hardness said how much of a dab is solid. Between that core and the rim the
coverage fell in a straight line, and that line was the same for every brush we
could ever ship. It is now a `Curve`: normalised distance across the ramp →
coverage. A straight line is exactly what it was; bowing it out gives a marker
that holds its ink to the very edge, bowing it in gives an airbrush that is
mostly haze. Those are different brushes, not different settings of one.

**Hardness stays, and the two are orthogonal.** Hardness says *how much of the
dab is solid*; the curve says *how the remainder fades*. Folding hardness into
the curve would have made every soft brush start by dragging the same first
control point, and would have lost the one parameter modulation can drive.

**It is a `Curve`, deliberately not a `Response`.** Every other curve in the
brush maps a pen input — pressure, tilt, velocity — onto a parameter, and comes
with a source picker. This one's x axis is a distance *inside the dab*. No
source means anything for it, so it is the bare curve and the UI shows it
without a dropdown. Sharing the `Response` type would have put a control there
that has no correct setting.

**It belongs to the stroke, not the dab.** A `Dab` is `Copy` GPU instance data;
a curve is a heap allocation. Every dab of a stroke shares one profile, so it
travels beside the dab list — `rasterize_dabs(…, falloff)` on the CPU, and a
uniform on the GPU — and `StrokeOp::Begin` carries a clone so changing the
brush mid-queue cannot retroactively reshape a stroke already recorded.

The shader cannot evaluate a spline per fragment, so the curve crosses as a
32-entry lookup table read with linear interpolation. That makes the CPU and
GPU paths *different arithmetic* for the first time — exact spline against
sampled table — rather than two spellings of one formula. The cross-check
therefore pins it directly: a deliberately kinked profile through both paths,
agreeing within the same 0.01 the rest of the §11a cross-checks use, plus an
assertion that the shaped render differs from the straight ramp, without which
"they agree" could just mean neither read the table. Three sabotages fail it:
shader ignores the table, index scaled by 32 instead of 31, table sampled over
the wrong span.

Next along this axis is a bitmap tip, which replaces this stage rather than
extends it — and raises the question this does not answer, of where brush
textures live and whether a preset embeds or references one.

### 4n. Dabs are ellipses — 2026-08-28

Roundness and angle, which together make a chisel nib: a mark thick across its
travel and thin along it. That is most of what inked line weight actually *is*,
and it is the piece that gives `Source::Direction` a home — point angle at it
with a straight curve and the dab follows the stroke.

**The ellipse is handled by transforming the point, not by a separate
elliptical falloff.** The sample is rotated back into the dab's frame and its
minor axis stretched up to the major, so an ellipse becomes a circle and the
existing falloff needs to know nothing about shape. Two things follow, and both
are why it is done this way:

- **Hardness means the same at every roundness.** A separate elliptical falloff
  would have to re-derive what "half way out" means, and would not agree with
  the round case at the boundary.
- **Nothing that reasons about a dab's extent changes.** Radius is the *major*
  axis, so a flattened dab reaches no further than a round one — the pixel
  bounds, the GPU quad and the tile-touching calculation are all still correct
  without a line of change.

Angle is **turns** on the brush and **radians** on the dab. Modulation works in
0–1, so a full turn being 1.0 is what makes "angle follows direction" an
identity curve rather than a conversion constant; by the time a dab exists the
value is geometry, and geometry is radians.

The shape transform now exists twice — `Dab::distance_to` and `dab_fs` — which
is the §11a hazard the rasterization cross-check was built for. Extended with a
rotated, flattened case at awkward angles: a quarter turn would hide a swapped
axis and a whole turn would hide a wrong sign, and both sabotages fail it now.

### 4m. Any input can drive any parameter through a curve — 2026-08-28

Asked well: *"just because we cannot see a use case does not mean one does not
exist"*. Correct, and it changed the plan. The first version hardcoded pressure
as the input and wired exactly two parameters. That was one instance of a
pattern shipped in place of the pattern.

**Every modulatable parameter is a `Response`**: a `Source` to read — pressure,
tilt, velocity, stroke direction, randomness — and a curve mapping it to a
multiplier. Which is what §4c committed to years of arguing ago: per-dab
modulation, "each optionally driven by pressure/tilt/velocity through a curve".

**Built before dab shape, deliberately.** Angle wants to follow *direction or
tilt*, so adding angle first would have meant a "follow stroke direction" flag,
then a "follow tilt" flag beside it, and then taking both out again. That is the
"add a pencil *mode* instead of a parameter a pencil is a value of" mistake, and
the ordering was changed to avoid walking into it.

**And before presets**, which is the harder deadline: presets serialise a brush,
and once brushes are authored, changing the model means migrating work the
*artist* made rather than a document format. The free window is while none
exist.

Decisions inside it:

- **The curve is always a multiplier**, for every parameter. The alternative —
  scaling some parameters and replacing others — means no artist can predict a
  curve without remembering which kind they are looking at.
- **Every source is normalised to 0–1**, so any source can drive any parameter
  and a curve authored against one reads sensibly against another. `VELOCITY_FULL`
  and `TILT_FULL` are the scales that make those units mean something; being
  approximate costs nothing, because any disagreement is expressible in the curve.
- **Velocity and direction are computed by the brush**, not passed in. They are
  properties of the *path*, and only the thing walking it knows them. Velocity is
  distance over the clock, not over sample count — sample rate varies with pen
  speed, so the two are not the same thing.
- **The input is redrawn per dab**, not per segment, or a random-driven brush
  emits rows of identical dabs.
- **Opacity is still absent**, and that remains a property of the model: it is a
  per-stroke ceiling, and a ceiling that varied per dab would not be one.

Randomness under undo needs no special handling, which is worth noting because it
usually does: history stores the **dabs**, not the gesture, so the numbers are
already drawn by the time anything is recorded and a redo replays rather than
re-rolls.

### 4m-i. The first cut: pressure to size and flow — superseded above

The first piece of brush depth (§4.2), and the one that makes brush *presets*
worth having: presets over the parameters we had would have produced six
slightly different round brushes.

**An inking pen and a pencil are not two sizes of the same dab.** What
separates them is mostly how they answer pressure — a pen holds its width and
then opens up, a pencil responds from the lightest touch. Same rasterizer,
same parameters; only the mapping differs. So the mapping is the thing to make
editable.

**Control points, monotone cubic (Fritsch–Carlson).** Two properties, neither
cosmetic. It *passes through* the points, so a curve editor can be trusted.
And it **cannot overshoot**: a plain cubic spline dips below the lowest point
and bulges past the highest between widely spaced points, which on a size
curve reads as pressing harder briefly making a *thinner* line. A straight
line between two points is the same representation, so "no curve" needs no
flag — and a flat curve *is* the "pressure does not affect size" switch, which
is why there is no checkbox that could disagree with it.

**Pressure drives size and flow, not opacity — and that is forced, not
chosen.** Opacity is a ceiling on the whole stroke (§stroke): it is what stops
overlapping dabs building past it. A ceiling that changed halfway along a
stroke would not be a ceiling. Flow is per dab, so it is the parameter
pressure can honestly drive, and driving it gives the light-touch-faint-mark
behaviour that pressure-to-opacity is usually reached for.

**Defaults preserve the old feel deliberately**: size is the identity curve
(what the brush did before), flow is flat. A gentler default size curve
probably suits most hands, but changing how the existing brush feels is a
separate decision from making it adjustable, and only one of those was being
taken.

The curve is general over its *input*, so tilt and velocity are later new
sources rather than new machinery. Dab shape and textured tips are the
remaining pieces of §4.2 and are separable from this one.

### 4l. One pointer capture, decided at the press — 2026-08-28

Every tool that took pointer input added another arm to `handle_pen_event`,
each with its own `if it_is_up { … return }` and its own idea of who owns the
pointer. They only had to disagree once, and they did — three times in two
days, all found by the user rather than by us:

1. Painting died entirely: a branch added above `drain_input` returned early,
   so the pen's `Up` never arrived and `drawing` stayed set forever (§11a.7).
2. Clicking a panel control cleared the selection: the *press* was refused
   because the panel owned that pixel, but the *release* still arrived and an
   empty gesture read as a tap.
3. It still did, after that fix: the *drag* arrived too, started a gesture from
   wherever the mouse twitched, and made the release look genuine to the new
   guard.

Each fix bolted a guard onto one more arm. The arms were the problem.

**A single capture, granted at the press and held until the release.** That is
what every UI toolkit converged on, and it removes the class by construction
rather than by vigilance: a drag or a release cannot reach a handler whose
press was refused, because the capture was never granted. `decide_capture` is
the only place the question is asked, and its order — modal, then Alt, then
whichever tool is up, then paint — is the priority order, written once.

Two consequences worth naming as decisions rather than accidents:

- **A stroke in progress is no longer interrupted by a modifier.** Pressing
  space mid-line used to stop the stroke, because the move arm re-read
  `nav.is_active()` on every sample. Capture means the stroke keeps its
  pointer, which is the better behaviour: a modifier should not silently
  truncate a line.
- **Alt-drag now samples continuously**, because `Pick` holds the pointer like
  anything else. That matches Photoshop, and it fell out rather than being
  added.

The per-gesture guards (`Select::in_progress`) stay. They are not duplicated
policy: capture decides *dispatch*, and a gesture refusing to be extended
before it starts is its own invariant. Worth knowing that they are also why
this refactor changed no behaviour — which made it hard to test, since a
behavioural test cannot separate "capture routed it correctly" from "the
handler guarded itself". The test that does distinguish them changes the tool
state *mid-gesture* and asserts the release still goes to whoever took the
press; the first version did not, and passed against a sabotage that re-derived
the owner from current tool state.

### 4k. Selection is a coverage mask, and a bucket is not a tool — 2026-08-28

Prompted by the right challenge: *"isn't bucket just whatever is in the
selection? why make the bucket do both?"* Yes. I had conflated region-finding
with filling, and the split is better.

**Three separate things**, and the middle one is where all the difficulty lives:

1. **A mask** — per-pixel coverage over the document. One data structure.
2. **Producers** — lasso, rectangle, select-all, invert, and later a flood fill
   from a seed point.
3. **Consumers** — fill, delete, transform, confining a brush.

So a bucket fill is *find a region, fill it, discard the mask*. The magic wand is
*find a region, keep the mask*. Same machinery, different front end. Building a
flood fill inside a bucket tool guarantees a second, subtly different copy the day
the wand arrives — the mistake `export::Composite` was extracted to undo, and
`§11a` has that class listed twice already.

It also settles a question that would otherwise have been a special case: with a
selection active, a bucket click fills the *intersection* of the found region and
the selection. That composes for free here.

**Coverage, not a bitmask.** One byte per pixel, eight times the memory of a
boolean on something transient that is already eight times cheaper than a colour
tile. It buys anti-aliased selection edges, feathering, partial selection and
soft-edged fills — all of which would mean rewriting every consumer to retrofit.
And it makes this the **same primitive a layer mask needs**: per-pixel coverage
attached to a layer instead of to the document. Layer masks are arguably a bigger
feature than selection for comics, and they should not need a second
implementation.

**The CPU holds the truth**, which dissolves a problem rather than solving it. The
canvas needed the whole residency-and-spill machinery of `tile_store.rs` because
its tiles are the only copy of the artwork. A mask's authoritative copy is ordinary
memory, so any GPU mirror is a *cache*: rebuildable, and exhausting it cannot
corrupt anything. It is also where a flood fill wants its data, being inherently
serial.

**Sequencing, corrected twice by the same conversation.** Alpha lock shipped before
clipping and should not have (§4j). Then: mask → lasso and rectangle → fill →
select-all/invert first, and the flood-fill region-finder after. Lasso-only fill is
*not* the comic workflow — hand-tracing fifty regions you already inked is exactly
the labour a bucket deletes — but it ships a real capability and builds the
primitive the flood fill plugs into, so nothing is thrown away.

**Vector layers stay open**, confirmed rather than hoped. The compositor consumes
tile *slots* and does not care where a tile's pixels came from, so a vector layer
is one whose tiles are **derived** — a cache re-rasterized when geometry changes —
not authoritative. Three things preserve that: geometry stored in page-space floats
(so re-rasterizing at print resolution is free), nothing assuming a layer's tiles
are the only truth for its pixels, and accepting that `Layer` eventually needs a
kind. Not now; just not foreclosed.

### 4j. Clipping is the colouring workflow; alpha lock is the shortcut — 2026-08-28

Asked directly, and the answer corrected a sequencing mistake: **artists clip far
more than they alpha-lock.** Alpha lock confines painting to a layer's own pixels
and bakes it in — destructive. Clipping masks a *separate* layer by the alpha of
the one below, so the standard stack works:

```
line art
highlights   ← clipped to flats, Screen
shading      ← clipped to flats, Multiply
flats        ← the base
```

None of that is possible with alpha lock: shading painted with the lock on is
inside the flats layer, so its opacity, blend mode and erasability are gone. Alpha
lock keeps its place for recolouring line art and one-off tweaks, which is a real
but much smaller share of the work. It shipped first; it should not have.

**Clipping is a compositor property, not a paint property** — nothing destructive,
no history involvement, no `PaintMode`. A per-layer flag and a running `base_alpha`:
while folding bottom-up, an unclipped layer records its contribution, and a
clipped layer is multiplied by it.

Three decisions inside it, each pinned by a test because each has a plausible
wrong answer:

1. **A run of consecutive clipped layers shares one base** — the nearest unclipped
   layer beneath. "Clip to the layer directly below" is indistinguishable until the
   *second* clipped layer exists, and then it leaks: the first clipped layer is
   usually solid, so it becomes an unrestricted base for the next.
2. **A clipped layer with nothing unclipped beneath it shows nothing.** `base_alpha`
   starts at zero. The opposite default makes it show everything, and no GPU test
   whose bottom layer is unclipped ever reaches the initial value.
3. **The mask is the base's *contribution*** — pixel alpha times layer opacity —
   not its raw pixel alpha. One rule, from which both wanted behaviours fall out
   rather than being special-cased: hiding the base hides the group, and fading the
   base fades it. The alternative leaves shading floating over flats that were
   hidden to look at something underneath.

**It forced an overdue consolidation.** The compositing *loop* had begun to exist
three times — `composite_fs`, the PNG export, and the eyedropper's sampler — and
clipping would have meant three separate ideas of what a stack means.
`export::Composite` is now the single CPU rule, driven by both CPU callers; the
WGSL copy is unavoidable and stays pinned to it by
`the_gpu_compositor_matches_the_cpu_reference`. Its `add` must be called for
*every* layer, hidden and unpainted ones included, or a skipped layer leaves
`base_alpha` describing the wrong shape — which is why the shader also lost its
`continue` for hidden layers.

**Format v5** adds `layer.clip_below`, same tolerant read as v4's `lock_alpha`.

### 4i. Alpha lock and the eyedropper — landed 2026-08-28

Two halves of being able to colour, and both landed as *variations of existing
machinery* rather than new subsystems, which is the test of whether §4a's
dab-based design was the right shape.

**Alpha lock is a third blend state.** `src * dst.a + dst * (1 - src.a)` —
source-atop. For the alpha channel that reduces to `dst.a` exactly
(`src.a*dst.a + dst.a*(1-src.a) == dst.a`), so coverage **cannot** change however
hard you scrub: no mask, no read of the render target, no second shader. The same
trick that made the eraser a blend variation (§4a) rather than its own code path,
and the same payoff: a locked stroke cannot drift from an unlocked one in shape,
falloff or spacing, because it *is* the same rasterization.

`erase: bool` became `PaintMode { Normal, Erase, LockAlpha }`. An enum because
exactly one applies: "erase" and "lock alpha" are contradictory instructions — one
removes coverage, the other forbids coverage from changing — so a state carrying
both has no defined meaning and should not be constructible. It threads through
history, because **redo replays dabs** and a redo that used the *current* lock
state rather than the stroke's would not reproduce the stroke.

**The definition is strict, and that decides the eraser question.** Alpha lock
means alpha cannot change. Erasing is nothing but a change in alpha. So erasing a
locked layer is *refused*, with a message, rather than performed as an invisible
no-op — a tool that silently does nothing reads as a broken app. A looser
definition ("painting is masked, erasing still works") would make the guarantee
conditional on which tool you happened to be holding, which is not a guarantee.

**The eyedropper samples the composited image**, folded with `export::blend_over`
— the same arithmetic as `composite_fs` and already pinned to it by the compositor
cross-check. So it returns the *displayed* colour by construction rather than by a
second implementation that resembles one. Over the paper too, since the paper is
on screen: sampling blank canvas gives the colour visible there, not a transparent
nothing a picker would have to invent a value for. Bound to Alt rather than a tool
palette entry, because that is how the tool is used and the palette does not exist
yet.

It needed `TileStore::snapshot_some`: sampling one pixel wants one tile per layer,
and reading the whole working set to answer a click would be absurd. The readback
plumbing `snapshot_all` had is now shared with it, so the copy-alignment reasoning
exists once.

**Format v4** adds `layer.lock_alpha`. Reading prefers the richer query and falls
back to one supplying the default in SQL — the same tolerance the v3 `meta` table
uses, so **absence reads as the default** and reading still needs no branch on the
schema version. Layers will grow more flags (clipping, position lock); this is the
pattern each should follow.

### 4f. Latency is measured, not felt — landed 2026-08-28

§4.1 has called input latency the project's top quality axis since the first day,
and for every day since then **nothing measured it**. `PenSample::time_ms` was a
field permanently equal to `0.0`. Every judgement about how the app felt —
including the `POLL_INTERVAL = 4ms` in `main.rs`, whose own comment says "revisit
with real latency numbers" — was a guess dressed as engineering.

`perf.rs` now records two rolling windows, shown in the panel:

- **stroke latency**, from a pen sample reaching us to the frame containing it
  being presented;
- **frame time**, how long producing that frame took.

Two things about this are deliberate and both limit what the numbers mean:

1. **The clock starts when the sample reaches us** (`input::now_ms`), not when the
   pen touched the tablet. octotablet does expose `FrameTimestamp`; it is not read
   yet. So the driver's and the OS's share is invisible, as is everything after
   `present` returns. **These numbers are a lower bound**, and the panel says so
   rather than letting a flattering figure be mistaken for the truth.
2. **Only frames that carried a sample count** towards stroke latency. A frame
   drawn because the UI wanted a repaint has no input in it, and counting it would
   pull the average towards the cost of doing nothing.

Mean *and* peak, because a mean hides the single long frame, and one stutter
mid-stroke is precisely what an artist notices.

The point is not that measuring makes anything faster. It is that a claim about
speed becomes falsifiable, and that the next optimisation gets chosen by evidence
instead of by whichever one is most fun to build. It also arrives just in time to
be the instrument for the §2 target (a Surface, integrated graphics), which
**this project has still never once run on**.

### 4g. The brush ring is drawn by us, and hovering therefore costs a frame

Brush size was invisible until you made a mark, which turns choosing a radius
into a guess-and-undo loop. So the pointer now carries a ring showing the actual
size, in screen pixels, tracking zoom — because the question it answers is "how
big will this be *here*".

Two consequences worth naming, because neither is free:

- **The OS cursor was the cheap route and does not work.** Windows caps cursor
  bitmaps far below the radii a paint brush reaches, so a large brush would
  silently stop matching its own cursor. Drawing it ourselves is the only version
  that stays correct at every size.
- **Hovering now repaints.** There is no cached composite to draw an overlay over
  — §4e deferred that cache on purpose — so a moved pointer costs a full canvas
  composite. Guarded by a half-pixel threshold in `note_pointer`, because a pen
  resting on a tablet reports poses continuously and would otherwise pin us at
  full rate forever. The threshold is not tuned for feel; below half a physical
  pixel the redraw provably cannot change a pixel.

If the §4f frame-time readout says hover repaints hurt on integrated graphics,
the composite cache is the fix — and now there is a number to justify it with,
which is exactly the sequencing §4f was for.

Hover reaches us as `PenEvent::Hover`, through the pen seam rather than off
winit's `CursorMoved`, because a pen hovering over a tablet is not guaranteed to
produce mouse motion at all — whether Windows synthesises it depends on the
driver and on what RealTimeStylus consumes. The backend that already knows the
pose is the one that can answer reliably.

### 4h. Stabilization is a filter in time, and it states its own price — landed 2026-08-28

Hand tremor sits around 8–12 Hz and cheap digitizers quantize on top, so a slowly
drawn line comes out wobbly however steady the intent. `openpaint-core/src/stabilizer.rs`
is a one-pole filter chasing the pen: `alpha = 1 - exp(-dt / tau)`.

**`dt` is elapsed time, not "one sample".** This is the decision, and it is why
§4f's clock had to land first. A fixed alpha per sample — the version almost
everyone writes — is wrong twice over:

- **It varies with hardware.** A 200 Hz tablet steps through the filter four
  times as often as a 50 Hz one and converges four times faster. The same gesture
  would draw differently on two tablets, and the setting would need re-tuning per
  device.
- **It varies with drawing speed.** Slow movement means more samples per unit
  distance, so more smoothing exactly where the artist is being careful — the
  opposite of what is wanted.

Both vanish when `tau` is a duration. A test pins this by running the same path at
1 ms and 2 ms report intervals and asserting they agree, *and* by running a
fixed-alpha filter alongside and asserting it does not — so the tolerance cannot
be what passes the test.

**The setting is denominated in its own price.** A one-pole filter following
steady movement settles exactly `tau` behind, so `tau` *is* the added latency, in
milliseconds. The control is therefore in milliseconds, not a 0–1 strength: an
abstract strength needs a maximum to scale against, and any such maximum is a
number somebody made up — the first version of this had exactly that invented
constant, and it took one question to expose it. Now `MAX_LAG_MS` bounds only how
far a slider travels, and changing it changes nothing else, precisely because the
unit is real. A control that spends the top quality axis (§4.1) should spend it in
units the artist can compare against the §4f readout.

**Ending where the pen ended.** A trailing filter never catches up, so at pen-lift
the line is short by roughly the lag distance — at full strength and a brisk
stroke, ~50 px. Left alone that reads as the app losing the end of every line.
`finish` converges the remainder rather than jumping it, so the approach
decelerates like the rest of the stroke, then lands exactly on the true endpoint.
Two tests cover the two halves separately, with the tail suppressed in one, so the
filter and the correction for it cannot mask each other.

**Default: off.** Deliberately not a guess. How much smoothing an artist needs
depends on their hand and their digitizer, and this project has measured neither on
any hardware. Inventing a nonzero constant is what this document exists to prevent;
the priced slider lets the choice be made with the number visible, and a default
can be *earned* from real use. Per-brush rather than global, for the same reason
each tool keeps its own radius: inking wants a lot, sketching wants none.

**A filter defined in time must be *driven* by time.** The first version advanced
only when a sample arrived, which looks right while the pen is moving and breaks
the instant it stops: the line freezes short of the cursor, and the next sample
arrives carrying the whole accumulated `dt`, so `alpha` is near 1 and the line
snaps forward in one straight segment. Reported from use at high strength and
speed, which is the only regime where the trailing distance is big enough to see.

`Stabilizer::advance` fixes it, and the event loop calls it for the whole duration
of a stroke. Two details are load-bearing:

- **The clock advances even when the point does not.** Declining to move it once
  converged would bank the idle time and spend it on the next sample — the same
  snap by another route.
- **Arrival is judged by distance remaining, not by step size.** A heavily
  smoothed filter takes its *smallest* steps when it still has the furthest to go,
  so a step-size test stopped it several pixels short of a held pen. Caught by the
  test, not by inspection.

This also means a stroke in progress keeps the loop awake regardless of what the
input backend wants, because the line is still moving after the last sample. That
is demand-driven painting intact, not an exception to it: there genuinely is
demand.

**Where it lives:** the app's input path (`stroke_start` / `stroke_continue` /
`stroke_finish`), between the pen and the brush — stabilization conditions input,
and input is the shell's job. Those three methods exist as a named seam so they can
be driven headlessly in tests, since every bug found this session lived in a layer
that had none.

---

## 5. Document model — the core differentiator

**A document is an ordered list of pages; each page is a tiled canvas of
growable dimensions.** This single model yields three products:

- **Webtoon** = a document with one very tall page.
- **Print comic** = pages + spreads.
- **Sketchbook** = many normal pages.

### 5a. Document model, settled 2026-08-28

Agreed in detail, because this is the decision that is expensive to get wrong.

**A page has exact pixel dimensions.** 2480×3508, or whatever. Nothing is
infinite, and nothing about it is unlike CSP or Photoshop. "Extend ↓ by 500" makes
it 2480×4008. That is the whole concept.

**One data model, no variants.** `Document { pages }`. A webtoon is *one very
tall page*; a sketchbook is *many pages*; a print comic is *many pages plus spread
pairing*. There are deliberately **no** mode-specific code paths — two document
types would mean two formats, two renderers, and two sets of bugs, which is exactly
how an app acquires the bloat we are avoiding (§1).

**There is no mode, and no document type.** Everything is always available:
- **Adding pages is always available.**
- **Extending in any direction is always possible.**
- **Upscaling is always available.**

⚠️ **Revised 2026-08-28: `Mode` (Pages / Continuous) was removed.** This section originally
called it "a pure UI layer" that hid affordances and set defaults. Building the page panel
showed there was nothing left for it to do, and nothing ever read it:
- Every affordance it was meant to hide is unconditionally available by the three rules
  above, which this same section insists on. So it could only ever have decluttered.
- "Scrolls continuously" lost its meaning the moment this section settled that **a webtoon is
  one very tall page** — panning a tall page is just panning. The mode was a hedge from when
  "many pages stacked and scrolled" was still on the table.
- What it was actually reaching for lives elsewhere: **new-document presets** (a strip versus
  A4 at 300 DPI) are a creation-time choice, and **strip slicing** is an export option (§7).
  Neither is a lasting property of a document.

A flag no code reads is worse than no flag, because it implies behaviour that does not exist —
the panel had a "webtoon" checkbox that changed nothing at all. Removing it makes the claim in
this section *stronger*: not "one model plus a mode", but **one model, full stop** — nothing in
the app needs to know what you are making.

This was also the format's first real schema change (v1 → v2, dropping `document.mode` and the
already-vestigial `page.next_layer_id`), which finally **proved** the migration story instead of
asserting it: a test builds a v1 file by hand, loads it, saves over it, and checks the tiles
survived and the dead columns are gone. Verified per §11a.4 — it fails with the migration
removed.

**One resize primitive:** `Page::resize(rect)` — the target **rectangle** in page
coordinates. Extend, crop, and drag-to-resize are all just different rectangles, and
`Page::extend(side, amount)` is a convenience that computes one.

An earlier version took a size plus an "anchor" naming which edges moved, because
coordinates were re-based at zero and a rectangle could not be named. Once coordinates
became stable (§5a below), the rectangle became the natural thing to state and the
anchor had nothing left to contribute — a dragged rectangle already says which edges
moved. It is also strictly more expressive: it can trim 10 px off the left and 30 off
the right, which size-plus-anchor cannot express. `Anchor` was deleted.

The extend amount is a **parameter with a default**, stored in settings — never a
constant in code.

**Pixels are the canvas; DPI is metadata.** "300 DPI A4" is a *preset* that computes
2480×3508, not a mode the engine knows about. Upscaling is supported (as CSP's
Change Image Resolution and Photoshop's Image Size both are) but is **lossy in the
direction people want it** — it cannot invent detail, so line art softens. It is a
rescue, not a workflow; the real answer to "started too small" is presets that start
you large enough. Note it also invalidates undo rectangles, so a resolution change
must transform or clear history.

**Page dimensions are arbitrary pixels.** Deliberately not forced to multiples of the
tile size: that would leak the implementation into the UX. Sparse tiles handle
partial edge tiles fine. Tile size stays 256 (already the core constant; matches
CSP/Krita practice).

**The fixed-size workflow pays nothing for any of this.** Tiles are allocated only
where paint lands, regardless of mode, so a 2480×3508 page you never resize costs
exactly what it would in an app with no growth feature at all. `resize()` is simply
never called. Supporting webtoons imposes no tax on the Photoshop-style user — that
is a property of the tiled store, not something to be careful about.

**Coordinates are stable; the page origin may be negative.** Revised 2026-08-28 after
the first implementation proved the point. A page is a *rectangle in a signed
coordinate space* — an origin plus an extent — not a `w × h` grid pinned at (0, 0).
Extending leftward or upward moves the **origin**, never the content, which gives one
invariant:

> **A pixel you painted keeps its coordinate forever.**

The first attempt re-based every coordinate at zero, so extending left shifted every
pixel's x. That instability then leaked outward and needed a correction at every
consumer: the camera had to compensate so the drawing didn't appear to lurch sideways,
undo rectangles had to be rewritten, and every future consumer of coordinates would
have needed the same. Those corrections were symptoms; the coordinate choice was the
cause. Fixing the cause **deleted** code — the pixel-shuffling loop in
`Canvas::resize`, the camera compensation, and the history-shifting machinery — rather
than adding any.

Costs exactly one subtraction at the GPU boundary, because a texture is always
zero-based, and that mapping belongs to the renderer (`CanvasRenderer::origin`, and one
line in the dab shader). Tile coordinates were already `i32` with
`div_euclid`/`rem_euclid`, so the core needed no new machinery.

None of this is user-facing: a page still has exact pixel dimensions, and "extend down
by 500" still just makes it taller.

**Painting clips at the page edge — for now.** Photoshop and Krita keep pixels
outside the canvas (Photoshop's Crop has a "Delete Cropped Pixels" checkbox for
exactly this); Procreate clips. We clip, because it is the current behaviour, has no
runaway-allocation risk, and is simpler to reason about.
Revisited when crop landed, and **clipping stays** — but for a reason that turned out to
be independent of crop. Painting clips at the page edge so the artist never has to
wonder whether strokes outside the canvas do anything. That is a *painting* rule. Whether
already-painted pixels **survive a crop** is a separate question, and the answer is now
no-loss (§5c). The known cost of clipping stands: a stroke running off the edge stops
dead at the old boundary if the page is later extended. One bounds check in
`Canvas::blend_pixel`, cheap to revisit once real usage has an opinion.

### 5c. Crop must not destroy pixels — and undo is not the safety net

Settled 2026-08-28, **reversing** what §5a said hours earlier. The original claim was
that retaining out-of-page pixels earned nothing, "because a crop is one undo step, and
undo snapshots the region it is about to lose." That is wrong, and wrong in a way worth
recording because it is a tempting mistake:

> **Undo is LIFO, so it structurally cannot recover a mistake noticed late.**

An artist who crops, works for two hours, then notices, cannot reach those pixels: the
snapshot exists, but getting to it means discarding the two hours. This is not a
history-budget problem and not fixable by pinning the snapshot — the ordering is the
problem. Two further holes in the same reasoning: the history budget (64 MiB) evicts
oldest-first, and history does not survive a save at all.

**So crop changes the page rectangle and retains the pixels outside it.** Painting still
clips to the page, so nothing changes about how drawing feels. Deleting out-of-page
content becomes one explicit action — **Trim to canvas** — which is the only operation
that ever discards pixels, and is itself undoable.

**This design has less machinery, not more.** If a crop destroys nothing, undoing one is
pure geometry: `Op::Resize`'s `before: Option<wgpu::Texture>`, `PageResize::loses_pixels`,
and the snapshot/restore path for resizes all become dead and get deleted. Same signal as
the signed-origin change (§5a) — fixing the cause removed code.

**It must land on the tiled canvas, not on today's single texture.** The pixel store is
currently one page-sized texture. Retaining outside pixels there means sizing it to the
union of page-and-content — a *bounding box*, so it pays for empty space, and
crop-narrow-then-extend-the-other-way would make memory grow **because the canvas got
smaller**, eventually failing the pixel budget. Sparse tiles pay only where paint exists,
which is the honest version. Non-destructive crop is therefore part of the tiled-canvas
work (Q13), not a step before it.

⚠️ Until that lands, a crop that loses pixels says so bluntly and does **not** advertise
Ctrl+Z as a safety net.

### 5b. Crop is a direct-manipulation tool, not a dialog

Settled 2026-08-28, after a numeric "Canvas Size" dialog with a 3×3 anchor grid was
built and rejected. The dialog was the wrong shape for the job: an anchor grid exists
only to answer "which edges move?", and on a rectangle you are dragging, the drag
already answers it. Direct manipulation is also what every reference does — Windows
Photos, PowerPoint, Photoshop, CSP — so it is what the UX north star (§1a) demands.

**Dragging outward extends; dragging inward crops.** One tool, because `Page::resize`
takes a rectangle (§5a) and does not care which is larger. Splitting "crop" from
"extend" would be two tools over one primitive.

**The camera never moves while the tool is up.** No auto-fit, no zoom change, no
recentering — the rectangle is expressed in page coordinates and re-projected each
frame, so panning and zooming keep the outline glued to the page instead of fighting
it. A tool that re-frames the view mid-gesture makes the gesture unaimable.

**Applying goes through the same `apply_page_rect` as everything else**, so a crop is
undoable and guarded by the same dimension and pixel-budget limits as an extend, with
no crop-specific path to keep in sync.

**The handles are painted, not egui widgets.** Pen input never reaches egui
(OPEN_QUESTIONS Q14), so widget handles would be mouse-only — unusable with the very
input device this app is for. Geometry lives in `crop.rs` in page coordinates, driven
from the app's own input path, which sees pen and mouse alike; egui only draws the
outline. Keeping the geometry free of GPU and UI types is also what makes the eight
drag behaviours and the hit-testing directly testable.

**Grab tolerance is screen-relative, capped per axis.** A screen-pixel radius divided
by the zoom keeps handles equally easy to aim at at any magnification, but at low zoom
that radius becomes wider *in page units* than the rectangle itself — at which point
every press reads as a corner and neither the edges nor the interior can be grabbed.
Capping the tolerance at a quarter of each side fixes it; the test that pins this was
confirmed to fail without the cap.

### Tiling enables everything
Canvas stored as tiles (~256×256). Tiles allocated **only where paint exists**,
so an infinitely tall or oversized canvas is nearly free until drawn on. This is
what makes infinite scroll cheap and multi-page/multi-page docs light. Pages
lazy-load from an on-disk tile store so a 200-page sketchbook stays light.

### Two reading/authoring modes (both from one model)

1. **Continuous mode (webtoon):** one growable tiled canvas.
   - **"Extend canvas ↓" button** + optional **auto-grow** when drawing/scrolling
     near the bottom. No pre-guessing height, no manual Canvas Size dance.
   - Optional **"fold" guides** marking device-screen heights / safe areas.
   - **Export slices** the strip at chosen points into an image sequence
     (platforms cap upload image height).

2. **Page mode (print comic / sketchbook):** a sequence of discrete pages, each
   its own tiled canvas. Spreads, reorder, templates, "Next page."
   - **Sketchbook = page mode** with many pages, a default template, fast
     page-add (or auto-add page when one is filled), and thumbnail-grid page
     navigation. Possibly per-page notes/tags to find old studies. No special
     casing — just UX defaults.

### Page management — landed 2026-08-28

Select, add, delete (undoable), reorder, and the Pages/Continuous toggle. The model had been
ready since §5a; this is the UI catching up, and it stayed cheap exactly because the model was
already general.

One latent bug fell out of building it, worth recording because it was invisible until pages
existed: **layer ids were per page**, and `Page::new` always started at 0. The renderer keys
tiles by layer id alone, so a second page's first layer would have shared tiles with the
first's. Id allocation moved to `Document`, so ids are unique across the document and a page
reorder still rewrites nothing.

A related simplification: the layer-id counter is **no longer saved**. One past the highest
live id is correct on load, because a save discards undo history — and history holding tiles
keyed to *deleted* layers was the only thing that made reusing an id dangerous.

Switching pages **re-fits the camera**, unlike a resize, which deliberately keeps the zoom. A
different page is very likely a different size, so keeping the zoom would leave the artist
looking at empty space; a resize of the page you are working on is the opposite case.

Still missing, in rough order of how much they will be missed: **thumbnails** (the list is
names for now), **drag to reorder** rather than Up/Down, **spread pairing** for print, and
export of a whole document (CBZ/PDF, §7).

### Page management (the under-served area we intend to win)
Reordering, spreads, templates, thumbnail-grid overview, lazy on-disk loading,
and (for webcomics) panel/frame tooling + export to CBZ/PDF/image-sequence.

---

## 6. Input & interaction philosophy

- **Input mapping is data, not hardcoded behavior.** Every action is a bindable,
  user-remappable command.
- **Right-click:** we do NOT want Krita's behavior (right-click opens a big
  context menu). Default to a CSP/Photoshop-flavored action instead (e.g. a
  quick brush-size/color popup or color picker). Fully remappable. Owning the
  input layer makes this trivial.
- General interaction/design language should feel like **CSP** (with Procreate
  influence), not Krita.

### Cross-platform reach (Windows now; Linux/macOS cheap to add later)
Decision: **stay cross-platform-capable by construction, ship Windows first.**
- wgpu (rendering) and winit (windowing) already run natively on Windows,
  Linux, and macOS with the *same code* — no extra effort.
- The engine core (`openpaint-core`) is pure Rust, no OS calls → runs anywhere.
- The ONLY platform-specific piece is **stylus input**, which is why it lives
  behind an input **trait** producing a stream of samples
  `(x, y, pressure, tilt, …)`. Windows Ink is just one implementation.
  - Linux pen: via winit/libinput or a small evdev backend (Wacom/Veikk give
    pressure+tilt). Modest, self-contained effort when wanted.
  - macOS pen: `NSEvent` pressure/tilt via objc2/cocoa. Similar modest effort.
- Cost of keeping the door open now: ~zero (just don't hardcode Windows types
  into the app; keep input behind the trait). We simply don't *build* the
  Linux/macOS input backends until desired — adding one is a few-hundred-line
  module, not a rewrite.
- Deferred, non-architectural friction (only when actually shipping to a
  platform): macOS code-signing/notarization + `.app` packaging; Linux
  Flatpak/AppImage; real-hardware pen testing per platform.

---

## 7. File format & interop

### The native format is a SQLite database, settled 2026-08-28

Reverses this section's original sketch ("a zip of tiles + JSON"). The reason is the one
thing a zip cannot do: **replace an entry in place.** Every save to a zip rewrites the whole
archive, so one stroke in a three-hundred-page sketchbook -- the stated core use case (§5) --
costs a full multi-hundred-megabyte rewrite, and autosave becomes impossible. Autosave is the
feature that actually protects work, so that is disqualifying rather than merely slow.

What SQLite gives, none of it free otherwise:
- **Atomicity and crash safety** from transactions, instead of hand-rolled
  write-temp-and-rename and its edge cases.
- **Incremental saves** -- only the tiles that changed.
- **A self-describing schema**, dumpable with any SQLite tool, plus `user_version` for a
  well-worn migration story.
- A container whose own file format carries an explicit long-term stability commitment --
  a stronger longevity guarantee than a convention of ours layered over zip.

CSP's `.clip` is also SQLite, as far as public reverse-engineering shows; Krita and Procreate
use zips and both have the rewrite problem. Not decisive on its own, but a signal the choice
survives real documents.

**The cost, stated plainly:** it is the project's first deliberate C dependency (`rusqlite`
with the bundled amalgamation -- one C file, cross-compiles to Windows without fuss). Tile
compression is ours to choose rather than free from the zip; deflate via `flate2`'s Rust
backend, so SQLite remains the only non-Rust dependency. Measured on a real two-layer
document: 7 tiles, 3.5 MB raw, **90 KB on disk**.

**Decisions inside the format:**
- **Tiles are keyed by layer *id*, not stack position**, so reordering layers rewrites no
  tile rows. Same reason the in-memory store keys by id, and it is what keeps saves cheap.
- **Blend modes and mode are stored by name**, not integer. A file you can read with
  `sqlite3` and understand is worth a string compare per layer, and a name cannot collide
  with a code some older file already used. `Blend::code()` stays an integer because the
  *shader* needs one -- a different concern with a different lifetime.
- **A per-tile `codec` column.** A better compressor can be introduced later without a
  schema bump and without rewriting existing files.
- **Tiles are stored premultiplied `f16`, verbatim.** No conversion on save, so no lossy
  round trip through the format.
- **Out-of-page tiles are saved.** That is what makes non-destructive crop (§5c) survive
  being closed and reopened; without persistence the guarantee lasted only a session.
- **Undo history is not saved.** A save is a fresh undo baseline -- which is precisely why
  crop had to be non-destructive rather than "recoverable with Ctrl+Z".
- **Loading goes to the CPU side of the tile store, not the GPU.** Residency pulls in what
  the viewport asks for, so opening a large document is fast and memory-bounded for free.

✅ **File dialogs, added 2026-08-28.** Ctrl+N / Ctrl+O / Ctrl+S / Ctrl+Shift+S, with the
document's name and a `*` for unsaved edits in the title bar, and a guard that *offers to save*
before anything that would discard them — a dialog whose only choices are "lose your work" and
"cancel" makes the user save by hand, which is exactly when they forget.

**A dialog is never opened from the event handler that asked for it.** A modal dialog pumps the
Windows message queue, which dispatches our pending events straight back into us — the Q10c
hazard. So a request is parked and serviced from `about_to_wait`, the one place winit guarantees
no foreign frame is on the stack. Exactly the same rule, for exactly the same reason, as
draining pen input (see the note at the top of `main.rs`).

**Native only where it is unavoidable.** The first version used a native message box for the
unsaved-changes question and it broke on contact: with no parent window, Windows placed the modal
*behind* the app — audible, invisible, and blocking, so the app looked frozen. Two changes came
out of that:
- File pickers are parented to our window (`set_parent`). A modal with no owner has no z-order
  relationship to the window it is blocking.
- **The unsaved-changes question is drawn in-app, in egui.** A file browser cannot be
  reimplemented; a three-button question can. Every native modal runs its own message loop, which
  is this project's most expensive recurring bug, so the fewer the better — and one we draw
  ourselves cannot end up behind the window at all. It also blocks painting while it is up, and
  answers to Enter and Escape.

The prompt **offers to save**, and only proceeds if the save actually succeeded: a cancelled or
failed save is not permission to discard the work.

`rfd` is the dependency. Its `xdg-portal` backend is on by default and `gtk3` is off, so Linux
needs no GTK development packages — which matters for §6's "cross-platform by construction".

- **Open and documented.** The schema is introspectable, which is a better guarantee than a
  zip full of conventions only our code knows.
- **PSD import early** — huge for adoption. (Import prioritized; export later.)
- **PNG import/export.**
- Webcomic exports: **CBZ / PDF / image-sequence** (with webtoon strip slicing).

---

### 7a. Autosave writes ordinary documents — landed 2026-08-28

Explicit saving protects the work you remembered to save. This protects the hour
you were absorbed in, and it is the reason §7 chose a database over a zip: a zip
would have made every autosave a full multi-hundred-megabyte rewrite.

**A recovery copy is a real `.openpaint` file**, written with the same `save` as
everything else. No second format to keep in step, and no recovery path that rots
because nothing else exercises it. Format **v3** adds a `meta` key/value table so
the copy records which document it belongs to (`autosave.original_path`) and that
it is a copy at all (`autosave.recovery`). Kept general deliberately — that will
not be the last thing a document needs to carry which is neither structure nor
pixels — and additive, so the migration is `CREATE TABLE IF NOT EXISTS`.

**Copies live in the OS per-user data directory, not beside the document.** That
folder may be read-only, on removable media that is gone, or inside a sync folder
that would cheerfully propagate a half-written temporary to every other machine.
It is also not ours to litter.

**A crash is detected by a copy outliving its process.** A copy exists only while
the document is dirty and is deleted the moment it is not — on save, load, new, and
`exiting`. So a copy present at startup means that process did not get to clean up.
`mark_clean` is the single place that transition happens, because the invariant only
holds if every route through it is the same route.

**Recovery is offered, never applied.** The artist may have moved on, and silently
replacing what they just opened would be worse than the crash. Accepting loads it
**dirty and pointed at the original file** — the work in it was never saved, so
letting the next Ctrl+S write into the recovery directory would leave their real
file untouched while looking like success.

**Skipped mid-stroke.** A save reads every resident tile back off the GPU, which
mid-stroke would both hitch the one thing that must never hitch and capture a stroke
halfway through.

**The interval is 60 s, and the panel reports what a save cost.** Shorter is
strictly better for the artist; the only thing pushing back is the price. So the
price is measured and displayed rather than guessed at, the same discipline as §4f —
and if it turns out to be expensive on a large document, the answer is incremental
saving (tiles are individually keyed rows) rather than a longer interval.

**Known limitation, recorded rather than hidden:** two instances at once. Each
writes its own uniquely named copy, but the second to start will offer the first's
live document. Accepting duplicates work in progress; it destroys nothing. Telling
"abandoned" from "in use" needs an OS file lock, which is not worth a dependency
until multiple windows exist.


## 8. Phased roadmap

- **Phase 0 — Vertical slice (prove the feel):** repo scaffold + winit/wgpu
  render loop + one pressure/tilt brush (via the input abstraction) on a tiled
  canvas. If this doesn't feel good, nothing else matters. **IN PROGRESS:**
  - [x] Step 1 — window opens (winit). Verified on Windows.
  - [x] Step 2 — wgpu renderer clears surface. Verified (RTX 3070 Ti / D3D12).
  - [x] Step 3 — tiled canvas + stamp-based brush + mouse drawing. Verified.
  - [x] Step 4 — input abstraction (PenSample/PenEvent + InputBackend trait),
        mouse as first swappable backend. Behavior-identical refactor.
  - [x] Step 5 — octotablet backend (Windows Ink) behind the trait; extended
        trait with a polled path (`poll` + `wants_continuous_poll`) since
        octotablet is polled, not event-driven. Windows target cross-checked
        from Linux. **VERIFIED on the real tablet 2026-08-27:** Veikk enumerates
        as `name="Stylus"`, `axes=PRESSURE | TILT`, and pressure varies correctly
        (e.g. 0.18→0.23, 0.82→0.60) driving dab radius. Required enabling the
        driver's "Windows Ink" option first — see OPEN_QUESTIONS Q10d.
        Tilt is declared but always reports 0.0 on this device (no tilt hardware),
        so tilt-driven behavior is still unvalidated.
    - **The first step-5 build froze on launch (fixed).** Cause: a COM/STA
      reentrancy deadlock, not a GPU or pen-hardware problem. RealTimeStylus
      delivers its "async" plugin callbacks on our *own UI thread*; octotablet
      holds an internal mutex across those callbacks while making
      out-of-process COM calls, and a COM call from an STA pumps the message
      queue while it waits. Because step 5 called `request_redraw()` every
      loop iteration, a `WM_PAINT` was always pending, so that nested pump
      re-entered our handler → `poll()` → `pump()` → the same non-reentrant
      mutex on the same thread. Hard deadlock on frame 1 at 0% CPU.
      Fix: drain input only from `about_to_wait` (winit calls it solely from
      the top of its own loop, never from a window procedure), pace it with
      `ControlFlow::WaitUntil` instead of a permanently-pending redraw, and add
      a reentrancy guard around our handlers. See Q10c.
  - [ ] Step 6 — assess feel; if inadequate, swap to hand-rolled Windows Ink
        (WM_POINTER) behind the same trait (prediction + coalesced samples).
- **Phase 1 — Real engine:** full brush engine; layers + blend modes + masks;
  undo history; color management (linear-space compositing).
- **Phase 2 — Document & pages:** the multi-page/growable model; on-disk
  container; save/load; PSD/PNG import-export.
- **Phase 3 — Webcomic/sketchbook tooling:** panels, templates, page-management
  UI, CBZ/PDF export, webtoon slicing/fold guides.
- **Phase 4 — Pro polish:** Wintab; brush editor; stabilization; transform/
  selection tools; performance passes; final CSP-like UI.

**Discipline:** nail Phase 0's *feel* before broadening. True CSP-quality is a
multi-year effort; focus beats breadth.

---

## 9. Prior art & references

- **libmypaint / mypaint-brushes** — de-facto open brush engine + brush library,
  liberally licensed. Candidate to port/wrap rather than reinvent brush math
  (subject to our license choice).
- **Krita** (GPLv3) — study only (subject to license); do not copy unless we're
  also GPL.
- **CSP** — primary UX/feature reference.
- **Procreate** — UX/feel reference (esp. infinite-canvas feel).

---

## 10. Resolved decisions (moved out of OPEN_QUESTIONS)

- **License → GPLv3.** Personal open project; want to leverage the open
  ecosystem (port/wrap **libmypaint** for the brush engine, study **Krita**).
  Permissive licensing offers no benefit here since closed reuse isn't a goal.
- **Test loop → Linux code / Windows tablet.** See §2. Windows Ink is primary;
  Linux→Windows build pipeline needed from day one.
- **Webtoon default height behavior → explicit "Extend ↓" button.** Auto-grow
  remains available as an option, not the default.

## 11a. Recurring hazards (check new code against these)

Not bugs — *classes* of bug this codebase has actually produced, most of them more
than once. Each cost real debugging time, so they are worth reading before adding code
of the same shape.

### Implicit ordering: "I did the thing, but not in the order the API guarantees"

1. **State changed, but no frame was requested.** Painting is demand-driven, so any
   state change affecting what's on screen must also ask for a frame. Bit us three
   times: the egui panel was completely inert (input queued, no frame, so nothing
   was ever consumed, so no frame was requested — a self-sustaining deadlock); the
   canvas sat off-centre (a queued re-fit nobody drew); and a stroke wasn't
   committed on pen-up.
   **Deliberately NOT fixed with machinery.** A deferred "dirty" flag would add a
   frame of input latency, and input latency is the project's top quality axis
   (§4.1). Trading measurable latency for a bug class that review catches is a bad
   deal. Every current call site is covered; check new ones by hand.

2. **`Queue::write_buffer` is not ordered against draws recorded between writes.**
   Every buffer write in a submission is applied *before any* command buffer in it
   executes. Writing a buffer once per batch and drawing in between means all the
   draws see only the last write. This produced visible gaps in fast strokes —
   whole batches of dabs silently dropped, worse the faster the stroke because more
   batches landed per frame.
   Fixed structurally rather than by guard: `StrokeLayer::upload_dabs` is separate
   from `stamp_range`, so the frame's data is uploaded once and draws address
   sub-ranges. If you need per-item data, either upload once and index, or submit
   between writes.

   **Bit again, twice, during the tiled-canvas work (2026-08-28) — and the same rule
   covers `write_texture` and render-pass clears, not just `write_buffer`:**
   - Per-tile uniforms are selected with **dynamic offsets** into one buffer written
     once per frame. Rewriting one uniform between per-tile passes would have left
     every pass reading the last tile's values. The stamping and bake records live in
     two *disjoint regions* of that buffer, so a bake cannot overwrite records the
     stamps preceding it in the same submission still have to read.
   - `upload_dirty` allocated a tile, cleared it with a render pass, then uploaded it
     with `queue.write_texture` — so the clear was applied *after* the upload and wiped
     it. Fixed by not clearing a tile that is about to be written in full. Caught by
     the export test, which is the sort of thing that would otherwise have looked like
     a broken export.

   The general rule: **a queue write and a recorded command are not in the same
   order you wrote them.** If both touch the same resource, either use disjoint
   regions, do both through the encoder, or submit in between.

6. **A side effect inside `debug_assert!` does not happen in release.** Written as

   ```rust
   debug_assert!(self.map.insert(coord, slot).is_none());   // WRONG
   ```

   the insert runs in debug and **vanishes in release**, because `debug_assert!` does
   not evaluate its expression there at all. This shipped a build in which the app
   painted nothing whatsoever: every tile was allocated and then dropped on the floor,
   while all 178 tests passed — because tests build in debug.

   Fixed by moving the call out of the macro so the assert only inspects a bound value.

   **The guard is `cargo test --workspace --release` in CI**, now run alongside the debug
   suite. Verified the way §11a.4 demands: with the bug reintroduced, the debug suite
   passes 90/90 while release fails 7 tests. Any future divergence between profiles has
   the same chance of being caught.

   `clippy::debug_assert_with_mut_call` is also denied workspace-wide, but **it does not
   catch this case** — it fires on `&mut` *arguments*, not on a mutating method receiver,
   and was confirmed silent on the exact line that shipped. It is kept because the shapes
   it does catch are the same mistake; it is not the reason this cannot recur.

   General rule: **an assertion must not be load-bearing.** If deleting it changes what
   the program does, it is not an assertion.

7. **An early return that skips a step the function must always take.** Adding a
   branch near the top of a long procedure reads as local, and is not: everything
   below it silently stops happening. In `about_to_wait` a "keep waking while a
   stabilized stroke converges" branch was added *above* `drain_input`, with a
   `return`. Draining is the only route by which pen movement — and pen *release* —
   reaches the app, so the first press left `Editor::drawing` set forever, because the
   `Up` that clears it was still sitting in the backend's queue. One dab appeared and
   painting never worked again, in that stroke or any later one.

   Two things make this class nastier than it looks. The added code was correct in
   isolation and its own test passed; and the damage was to a *different* subsystem
   than the one being edited, so nothing about the change suggested where to look.

   **The fix is structural, not a test.** `about_to_wait` cannot be unit-tested — it
   needs a live `ActiveEventLoop` — so there is nothing to assert against. Instead the
   mandatory work now comes first, unconditionally, and the "should we keep waking"
   decision is reduced to booleans combined at a single exit. There is no longer a
   place to put an early return that could do this, which is worth more than a test
   that could.

   General rule: in a procedure whose steps are obligations rather than options, **do
   the obligations first and branch afterwards** — and if it has more than one
   `return`, treat that as a smell to justify rather than a convenience.

8. **One definition split across two lists.** Adding `layer_text` to
   `STRUCTURE_SCHEMA` and not to the `DROP` statements each migration branch carried
   its own copy of made *every* migration fail with "table already exists" — a break in
   code that had nothing to do with the feature. The shape to watch for is a
   declaration and its inverse maintained separately: a schema and its drop list, an
   enum and a `match` over it written out by hand, a struct and a serializer. Where the
   compiler cannot pair them, the fix is one list both sides use, not care.

### Verification hazards

3. **Injected input cannot reach RealTimeStylus.** `SetForegroundWindow` is refused
   to a background process, and pen input arrives via RTS rather than the window's
   message queue. `PostMessage` reaches winit fine (good for navigation and UI
   tests) but not the pen path, so **a human drawing is the acceptance test for
   input-path changes.** Two "features are broken" conclusions during development
   turned out to be a broken harness, not broken code.

4. **A regression test that has never failed proves nothing.** When fixing a bug,
   reintroduce it and confirm the new test fails. Done for the stroke-gap fix; it
   failed by 0.955 at one pixel, which is what made the test trustworthy.

5. **`PostMessage` cannot fake mouse *hover*.** Posted `WM_MOUSEMOVE` reaches winit
   fine — pan, zoom and wheel all test that way — but Windows' mouse-leave tracking
   follows the **real** cursor, so `WM_MOUSELEAVE` fires immediately afterwards.
   egui then receives `PointerMoved` followed at once by `PointerGone`, discards the
   position, and no widget can ever be hovered or clicked. Symptom: egui reports
   `pointer_latest_pos() == None` while events are visibly arriving.
   To drive a widget, move the **real** cursor with `SetCursorPos` first; posted
   button messages then work. This cost several rebuilds chasing a non-existent bug,
   twice — the honest default is: **UI interaction is verified by a human**, and only
   engine-level behaviour is verified by injection.

---

## 11. Decisions still OPEN
See `OPEN_QUESTIONS.md`. Notably: Windows build/delivery mechanics (Q2b), final
UI framework, color-management depth, file-format specifics, and whether to port
libmypaint vs. build our own brush engine.
