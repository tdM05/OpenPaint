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

⚠️ **The hard part is accumulation, not rasterization.** Dabs within one stroke
cannot simply be alpha-blended in a batch: Photoshop's model is that **flow**
accumulates per dab while **opacity** clamps the stroke's total contribution. So a
stroke needs its own accumulation buffer with max-alpha semantics, composited onto
the layer once at stroke end. Getting this wrong is the single most common way
brush clones feel wrong, and it's the reason dab order and stroke boundaries have
to be first-class in the design.

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

## 11. Decisions still OPEN
See `OPEN_QUESTIONS.md`. Notably: Windows build/delivery mechanics (Q2b), final
UI framework, color-management depth, file-format specifics, and whether to port
libmypaint vs. build our own brush engine.
