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
- All three speak **Windows Ink** (Windows Pointer API: pressure + tilt), so
  one API covers all three for v1.
- **Wintab** (legacy Wacom API) — deferred to a later phase as an optional
  native backend. Some pros insist on it; CSP supports both. Not needed for a
  great-feeling v1. (Note: webview/Chromium stacks can't use Wintab at all,
  which is one reason we're going native.)

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
  canvas. If this doesn't feel good, nothing else matters. **We start here.**
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
