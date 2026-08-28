# OpenPaint — Decisions & Direction

> Living record of what we've agreed on. Update this whenever a decision is
> made or changed. Anything still undecided lives in `OPEN_QUESTIONS.md`.

Last updated: 2026-08-27

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

⚠️ **Consequence: tile residency is an early problem, not a Phase-2 one.**
A4 at 300 DPI is 2480×3508 ≈ 8.7 Mpx. At `Rgba16Float` (8 bytes/px, §4b) that's
~70 MB *per layer*, so a ten-layer page is ~700 MB before the composite target
and per-stroke accumulation buffer — which does not fit in a Surface's shared
graphics memory. Therefore:
- The GPU holds a **bounded cache of resident tiles** (the visible/working set),
  never the whole document.
- A tile budget + eviction policy is a **first-class early component**. The
  roadmap defers the document/page model to Phase 2, but the *tile store* it rests
  on has to exist before layers and undo are built on top of it (see
  OPEN_QUESTIONS Q13).

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

**One data model, no variants.** `Document { pages, mode }`. A webtoon is *one very
tall page*; a sketchbook is *many pages*; a print comic is *many pages plus spread
pairing*. There are deliberately **no** mode-specific code paths — two document
types would mean two formats, two renderers, and two sets of bugs, which is exactly
how an app acquires the bloat we are avoiding (§1).

**Mode is a pure UI layer.** It *hides affordances and sets defaults*, and restricts
nothing structurally. Webtoon mode offers only "Extend ↓" and scrolls continuously;
page mode hides extend and navigates page-to-page; export defaults differ. The
engine stays fully general underneath, so:
- **Adding pages is always available**, in every mode.
- **Extending in any direction is always possible**, whether or not the UI offers it.
- **Upscaling is always available.**

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

## 7. File format & interop (direction, details TBD)

- **Open, documented container**: conceptually a zip of tiles + JSON metadata
  (same spirit as `.kra` / `.procreate`, which are zips).
- **PSD import early** — huge for adoption. (Import prioritized; export later.)
- **PNG import/export.**
- Webcomic exports: **CBZ / PDF / image-sequence** (with webtoon strip slicing).

---

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

Not bugs — *classes* of bug this codebase has actually produced more than once.
Each cost real debugging time, so they are worth reading before adding code of the
same shape.

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
