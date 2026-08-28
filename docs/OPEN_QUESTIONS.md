# OpenPaint — Open Questions

> Decisions not yet made. When resolved, move a summary into `DECISIONS.md`
> and note the resolution here.

Last updated: 2026-08-27

---

## RESOLVED (summaries also in DECISIONS.md)

### Q1. License → ✅ GPLv3
Personal open project; want to leverage the open ecosystem (libmypaint, study
Krita). Closed reuse isn't a goal, so permissive gives no benefit.

### Q2. Early tablet testing → ✅ Windows-based loop
Author has a Wacom/Veikk but it's on the **Windows** machine; author codes via
SSH on Linux. Real test loop = build for Windows, run there with the tablet.
Windows Ink is the primary input path; Linux tablet backend is low priority.

### Q3. Continuous-mode default → ✅ Explicit "Extend ↓" button
Auto-grow remains an option, not the default. **Extended 2026-08-28** into the full
document model — see DECISIONS §5a. Key refinement: extending is generally available
in *all* directions and mode merely hides what it doesn't need, so there is one
general engine rather than a webtoon path and a page path.

---

## Blocking / high-priority (affects code we write soon)

### Q2b. Windows build & delivery mechanics  ⚠️ needed for the test loop
Author codes on Linux (SSH), tests on a separate Windows machine with the tablet.
How do builds get to Windows, and how are they built?
- **Cross-compile from Linux** to `x86_64-pc-windows-gnu` (or `-msvc`) and copy
  the `.exe` over — fastest inner loop, but wgpu/graphics + Windows Ink FFI may
  need the MSVC toolchain and careful setup.
- **Build natively on Windows** (author runs `cargo build` there; we sync code
  via git) — most reliable for Windows-specific APIs (Windows Ink), needs Rust
  installed on the Windows box.
- **CI (GitHub Actions) produces Windows artifacts** — clean, reproducible,
  slower loop (push → wait → download).
Also: how does the author *get* each build (shared folder, git pull + local
build, GitHub release/artifact download)?

**Status: UNDECIDED — need author input on the Windows environment (see below).**

---

### Q10d. Tablet drivers ship with "Windows Ink" OFF → ✅ RESOLVED (and a product lesson)
Debugged 2026-08-27 (Veikk + `VKTabletDriver` 2.0.4.4). Pressure read a flat
`1.000`. Cause: **the Veikk driver's "Windows Ink" option was disabled.** With it
off, the pen does not reach Windows Ink on *either* API:

| API | Toggle OFF | Toggle ON |
| --- | --- | --- |
| RealTimeStylus (octotablet) | only tool is `name="Mouse"`, `axes=0x0` | `name="Stylus"`, `axes=PRESSURE \| TILT` ✅ |
| `WM_POINTER` (Krita "Windows 8+ Pointer Input") | pressure flat | — |
| Wintab (Krita default) | works | works |

Enabling the driver toggle fixed it: pressure now varies correctly. Windows Ink
is confirmed the right primary path; Wintab stays Phase 4 (Q10).

Neither our code nor octotablet was at fault. `pressure=1.000` was
`unwrap_or(1.0)` in `input_pen.rs` faithfully reporting a *missing axis*, and
octotablet does implement `GUID_PACKETPROPERTY_GUID_NORMAL_PRESSURE`.

**The lesson worth keeping — this is a product requirement, not a dev anecdote.**
These OEM drivers default to Wintab-only for compatibility with older Photoshop/
CSP, so a large share of our future users will start with Windows Ink off, get no
pressure, and conclude OpenPaint is broken. Krita has the same trap and solves it
by exposing the API choice in settings.
Our answer should be better: **detect it and say so.** We already have the
detection — a tool with no `PRESSURE` axis, or one named `"Mouse"` with
`axes=0x0`. Turn that into user-facing guidance ("your tablet driver isn't
sending pressure — enable Windows Ink in its control panel") rather than a
`println!`. Track as a Phase-1 UX item.

**Diagnostic that found it (keep this pattern):** log each tool's `tool_type`,
`name`, and `axes.available()` on `Added`/`In`. Cheap (fires on proximity, not
per-sample) and it turns "pressure feels broken" into a one-line answer. Lives in
`input_pen.rs`.

**Known remaining gap:** the Veikk declares `TILT` but reports `tilt=(0.000,
0.000)` always — likely a model without real tilt hardware. Not our bug; means
tilt-driven brushes can't be validated on this device.

### Q2d. Verifying Windows-only code from Linux → ✅ WORKFLOW FOUND
Windows-only code (`#[cfg(windows)]`, e.g. the pen backend) is invisible to the
Linux clippy/build, so it used to be checkable only via the slow Windows CI job.
Now we cross-check it from Linux before pushing:
`rustup target add x86_64-pc-windows-gnu` once, then
`PATH="$HOME/.cargo/bin:$PATH" rustup run stable cargo clippy \
  --target x86_64-pc-windows-gnu -p openpaint-app -- -D warnings`.
This typechecks + lints the Windows code in seconds (already caught a real
borrow error pre-push). CI's Windows job also now runs clippy on the real
windows-msvc target as the authoritative check.

### Q2e. Debugging a Windows *hang* with no toolchain/debugger → ✅ WORKFLOW FOUND
The Windows/tablet box has no Rust and no debugger, so a freeze in a downloaded
CI artifact looked un-diagnosable. It isn't. What worked, in order:
1. `Get-Process` — CPU **flat** + `Responding=False` ⇒ a genuinely blocked
   thread, not a busy loop. (A loop spinning on redraws would climb steadily; a
   thread parked in `GetMessage` would still report Responding.)
2. Wait Chain Traversal (`advapi32!GetThreadWaitChain` via `Add-Type` P/Invoke)
   — shows critical-section/COM/ALPC ownership. Caveat: it cannot see
   `WaitOnAddress` waits, which is what Rust's `Mutex` uses, so a Rust-lock
   deadlock shows up as "blocked on nothing".
3. The decisive one: **minidump + module attribution.** `dbghelp!MiniDumpWriteDump`
   via P/Invoke (no install, no admin — same-user process), then a ~60-line
   Python script parsing the dump's module list + thread list, reading the main
   thread's `CONTEXT` (`Rip` at 0xF8, `Rsp` at 0x98) and scanning its stack for
   qwords that land inside a loaded module. No symbols needed: the *sequence of
   modules* is the diagnosis. Here it read `openpaint.exe` → `user32` →
   `InkObj.dll` → `rtscom.dll` → `tpcps.dll`/`rpcrt4`/`combase` → `user32`
   again → `openpaint.exe` → `WaitOnAddress`, i.e. our code on the stack twice
   around a message-pumping COM call. That *is* Q10c.
Worth keeping: this turns "it freezes on your machine" into a precise answer
without shipping instrumented builds around.

### Q2c. Dev-box lint tooling (rustfmt/clippy) → ✅ RESOLVED
Installed rustup into `~/.cargo` / `~/.rustup` with `--no-modify-path` (system
`/usr/bin/cargo` stays the default; invoke the rustup toolchain explicitly via
`PATH="$HOME/.cargo/bin:$PATH" rustup run stable cargo …`). rustfmt + clippy now
verifiable locally, so CI's fmt/clippy steps are **blocking again**.

---

## Medium-priority (Phase 1–2)

### Q4. Final UI framework
Deferred by design. Start with egui debug panels; decide the polished CSP-like
UI framework later (egui custom-drawn? another Rust UI? something else?).

### Q5. Color management depth
Composite in linear space, premultiplied, `Rgba16Float` in memory — settled, see
DECISIONS §4b. Open: how far on color *management* — sRGB-only v1? ICC profiles?
Wide-gamut/display-P3? CMYK for print later? (16-bit float in memory means we
have the headroom whenever we want it, so this stays deferrable.)

### Q6. File format specifics ⏳ DELIBERATELY DEFERRED until the page model exists
Container is "zip of tiles + JSON" in spirit. Open: exact tile encoding
(raw/compressed? per-tile format?), metadata schema, versioning strategy,
thumbnail/preview embedding, autosave/recovery model.

**Why it is waiting (decided 2026-08-27):** defining it now would define it for a
single fixed-size page. Pages and a growable canvas would then force a v2 plus a
migration path — for files nobody has yet. So the format waits for the document
model.

**PNG export shipped instead** (`openpaint-app/src/export.rs`, Ctrl+S). It has no
format to design, it removes the immediate problem that work could not be kept at
all, and it builds the GPU→CPU readback the native format will need anyway. Note
the conversion PNG forces us to get right and now has tests for: **unpremultiply,
then encode linear→sRGB.** Skipping the encode is the classic washed-out-export
bug, and the same missing-encode error had already bitten once in the renderer.

### Q10b. Wintab is NOT provided by octotablet (correction)
Earlier notes implied octotablet would give Wintab "for free." That is WRONG:
octotablet supports Windows Ink and Wayland-tablet on Linux, but NOT Wintab.
When wanted, Wintab will be its own backend behind the InputBackend trait (like
hand-rolled Windows Ink).

Windows Ink IS sufficient for all three target devices — verified on the Veikk
(pressure + tilt axes both declared). One caveat: the tablet driver's own
"Windows Ink" option must be enabled, or nothing arrives. See Q10d.

### Q10c. octotablet's Windows Ink backend is not reentrancy-safe ⚠️ CONSTRAINT
Discovered the hard way (step 5 froze on launch; see DECISIONS §8 and
`input_pen.rs`). The mechanism, because it constrains any future pen backend:
- RealTimeStylus delivers *async* plugin callbacks **on the thread that owns the
  RTS** — our UI thread — via a window RTS creates there. "Async" means "not on
  the pen's real-time thread", not "not on your thread".
- octotablet holds its internal `shared_frame` mutex across those callbacks
  while calling back into RTS/Ink over COM. Those are out-of-process calls, and
  a COM call from an STA **pumps the message queue while it waits**.
- So Windows can dispatch a pending `WM_PAINT` into winit *while octotablet's
  mutex is held*, re-entering our event handlers. Calling `Manager::pump()` from
  there deadlocks on a non-reentrant mutex on the same thread, permanently.

**Our rule (enforced by comments in `input.rs` / `input_pen.rs` / `main.rs`):**
drain any polled input backend *only* from `about_to_wait`, and keep a
reentrancy guard on our handlers. Avoid leaving a redraw permanently pending.

**Bearing on step 6:** this is a point against octotablet, though not a fatal
one — the fix is small and lives entirely on our side of the trait. Real
argument for the hand-rolled `WM_POINTER` backend remains latency/prediction/
coalesced samples (and Wintab later, per Q10b). Worth re-checking whether newer
octotablet releases stop holding the lock across COM calls; if so, upstream a
note or a patch rather than carrying the constraint forever.

### Q7. Brush engine: port libmypaint vs. build our own → ✅ BUILD OUR OWN (for the round brush)
License is not the blocker (GPLv3, so libmypaint is usable). The blocker is fit:
libmypaint's parameter model is built for painterly/wet behavior, not for
reproducing Photoshop's soft round, which is the explicit Phase-1 quality target
(Q7a). Porting it would mean fighting its model to get somewhere it isn't aimed.
Our own dab engine is more direct and keeps the math unit-testable per
DECISIONS §4a.
**Still open:** revisit libmypaint later specifically for wet / smudge / blending
brushes, where its model is genuinely strong and we'd be reinventing hard math.

### Q7a. Round-brush fidelity target = Photoshop/CSP soft round
The default round brush should aim to feel/look like Photoshop's (and CSP's)
soft round brush. That is a **Phase-1 quality goal**, not the first
proof-of-pipeline stroke. To match it we must get right: the hardness FALLOFF
CURVE (PS's is a specific tuned curve, not plain linear/Gaussian), the FLOW vs
OPACITY accumulation model (how overlapping dabs build up within one stroke),
correct SPACING (~25% diameter default in PS), anti-aliasing, and LINEAR-space
blending (to avoid dark edge fringing). Tracked so we hold ourselves to it.

---

### Q13. Tile residency + who owns tile pixels — HALF RESOLVED 2026-08-28
Falls out of the Surface-class target and "any canvas size" (DECISIONS §2): the
GPU can only hold a bounded working set of tiles, so ownership has to be explicit.

**Leading answer (per the quality/modern rule):** the **GPU is authoritative for
resident tiles**; CPU/disk holds the rest. Dirty tiles are read back
*asynchronously* (`map_async`, never stalling a frame) on eviction, save, or when
an undo snapshot needs to spill. This is the Procreate-shaped design. The
alternative — CPU authoritative with the GPU as a read-only cache — forces either
CPU dab rasterization or a readback per stroke, and both defeat §4a.

**The genuinely hard part is undo.** If the GPU owns pixels, a naive undo wants a
readback per stroke, which is exactly the stall we're avoiding. Proposed:
**copy-on-write tile snapshots on the GPU** — at stroke start, GPU-to-GPU copy the
tiles the stroke will touch into a snapshot pool. No readback on the hot path;
readback only when the snapshot pool is evicted to CPU/disk in the background.
Undo then also stores the stroke command, so history is replayable as well as
restorable.

**Open:** snapshot pool sizing and eviction policy; how tile budget adapts to
available memory (query wgpu limits? measure?); whether undo granularity is
per-stroke (assumed) or finer; interaction with the growable/multi-page model.

**Why this can't wait:** layers and undo get built directly on the tile store, so
its ownership model has to be settled first or both get reworked. This is the same
sequencing risk already noted against the Phase-2 page model.

---

## ✅ RESOLVED 2026-08-28 — the tile store itself

Built; see DECISIONS §4d for the design and what it deleted. Delivered:
- A bounded GPU tile pool (2D array texture, capacity fixed from a byte budget).
- Sparse tiles: memory scales with **painted** area, not page area.
- Storage decoupled from the page rectangle → **non-destructive crop** (§5c). Tiles
  outside the page are kept; drawing and painting are clipped to the page; storage is
  not.
- **Trim to canvas**, the one explicit, undoable action that discards out-of-page tiles.
- Per-tile copy-on-write undo snapshots in their own pool, so the budget is exact.
- The resize snapshot path deleted (`Op::Resize::before`, `PageResize::loses_pixels`).
- Both page-size ceilings gone. The only remaining page limit is 65536 px per side, and
  it is coordinate precision, not memory.

## ⚠️ STILL OPEN — spilling, and therefore true "any canvas size"

The pool bounds residency, but non-resident tiles currently have **nowhere to go**. When
it fills, painting stops and the status bar says so. That is honest, not finished:
DECISIONS §2 requires any canvas size, and a long webtoon inked throughout exceeds
96 MiB of tiles.

What is left:
- **Async readback on eviction** (`map_async`, never stalling a frame) into a CPU tile
  store, and re-upload on demand. `openpaint_core::Canvas` already holds CPU tiles and
  `upload_dirty` is already the re-upload seam, so the shape exists.
- **Eviction policy.** Nothing tracks use order yet — the pool is a slab, deliberately,
  with `TileMap` owning the key→layer mapping, so an LRU belongs beside the map rather
  than inside the pool.
- **How the budget adapts to available memory.** 96 MiB is a fixed guess. Query wgpu
  limits, or measure?
- **On-disk backing** for documents larger than RAM, and its relationship to the native
  format (Q6).
- **Undo snapshots spilling too.** They have their own 64 MiB pool and currently evict
  the oldest operation rather than spilling it.

Also worth revisiting once residency is under pressure: the canvas draw currently emits
an instance for **every** resident tile intersecting the page, not just the ones on
screen. Correct and cheap at ~200 tiles; viewport culling is the obvious next step when
the resident set is large. And zoomed-out quality is still unfiltered beyond a single
mip — a real resolution pyramid (as CSP and Procreate both have) is a separate piece of
work, not a regression from the single-texture version, which had no mips either.

### Q14. Pen input cannot reach the UI layer - ARCHITECTURAL, blocks any real UI
Surfaced 2026-08-27 when the egui debug panel landed: **only the mouse can operate
it.** Pen input arrives through octotablet, which bypasses winit's event stream
entirely, so egui never sees a pen event and cannot hover, click, or drag a widget.

Harmless for a debug panel, fatal for the real UI - a drawing app whose buttons
cannot be pressed with the pen is unusable, and the pen is the primary input device
by design (DECISIONS section 2).

Interim mitigation in place: `Ui::blocks_point` reports the rect egui occupies and
`Gpu::stroke_begin` refuses to paint there, so strokes don't land "through" the
panel. That is a hit-test, not input routing - it stops the pen painting under the
UI but does not let the pen *use* the UI.

**The real fix is to stop treating pen input as canvas-only.** Pen events should
enter a single input-dispatch layer that offers them to the UI first and the canvas
second, exactly as winit events are handled now. That means synthesizing UI-facing
pointer events from `PenEvent`, or feeding egui raw events directly.
**Open:** where that dispatch layer lives, and whether it is worth building against
egui at all given egui is explicitly throwaway (DECISIONS section 3) - it may be
better to solve once, properly, when the real UI framework is chosen (Q4). Until
then the mouse operates the UI and the pen draws.

### Q17. History is stroke-shaped, and GPU-resident
Undo/redo landed 2026-08-27 (`crates/openpaint-app/src/history.rs`). Design and its
consequences, so the limits are known rather than discovered:

- **Snapshots the region a stroke touched**, not the canvas, since most strokes
  cover a small fraction. GPU-to-GPU copy, so nothing returns to the CPU on the
  interactive path.
- **Redo replays the dabs** instead of storing an after-image. Halves the memory and
  costs a re-rasterization, which is now the cheap direction.
- **64 MiB budget**, evicting oldest-first. A stroke bigger than the whole budget is
  kept anyway: silently making one stroke unundoable is worse than briefly exceeding
  the cap.

**Known limits:**
- Undo/redo is **refused mid-stroke**. The in-progress stroke isn't in history and is
  still accumulating, so undoing would revert the *previous* stroke and then bake the
  current one over the restored image — a state the user never asked for. Ideally a
  mid-stroke Ctrl+Z would *cancel* the stroke; that needs stroke cancellation, which
  does not exist yet (also noted in Q16).
- **Resizes are undoable** as of 2026-08-28, which turned out to *simplify* history
  rather than complicate it: because operations are undone strictly in reverse order,
  the page geometry while undoing an operation is always the geometry it was recorded
  against. That removed the coordinate-shifting machinery entirely (an earlier version
  rewrote every rectangle and dab position on resize, which was only necessary because
  resizes were *not* undoable). A grow needs no pixels saved since shrinking back is
  lossless; only a crop stores the pre-crop canvas.
- History lives beside the GPU resources rather than with the document, which is not
  where it belongs conceptually. Revisit when the document/page model lands: history
  will need to be per-document, and probably per-page.
- Only strokes are undoable, because only strokes exist. When layers arrive this
  grows an operation type; the snapshot-plus-replay shape should still hold.

### Q15. Tracked Phase-0 shortcuts (audited 2026-08-27)
An honest inventory, so none of these go quiet. Each says why it is deferred
rather than pretending it isn't there.

**Fixed in the audit** (was: `Gpu` was simultaneously the renderer, the document
owner, the stroke state machine, and the UI host): split into `Editor` (document +
brush + stroke), `View` (screen/canvas transform, where pan/zoom will live),
`Renderer` (wgpu only), and `ui` reached through a renderer overlay callback. The
app crate went from 0 tests to 12, because `Editor` and `View` are now testable
without a GPU.

**Still outstanding, deliberately:**

0. ⚠️ **The whole-canvas texture now has a user-visible ceiling, found by crash.**
   Extending repeatedly panicked inside `Device::create_texture`:
   `Dimension Y value 9216 exceeds the limit of 8192`. Fixed to degrade gracefully
   (clamp + explain), but the underlying limit is real and it **blocks the webtoon
   use case outright** — a tall strip needs more than 8192 px.
   Two separate ceilings, both interim guards in `editor.rs`:
   - **Dimension:** 8192, from the default wgpu limits we deliberately request.
   - **Memory:** a 16 Mpx budget, because 8192×8192 at `Rgba16Float` is 512 MiB for
     the canvas plus 128 MiB for the stroke buffer. A failed allocation is a device
     error, i.e. another crash — so the dimension clamp alone was not enough.
   Both vanish with the tiled resident cache, which makes this the next piece of work
   rather than a deferred nicety.

1. **Whole-canvas GPU textures** (`canvas_renderer.rs`, `stroke_layer.rs`).
   Contradicts the bounded-tile-cache requirement in DECISIONS §2 / Q13. Now *two*
   canvas-sized textures: the canvas (~34 MB at 2048², `Rgba16Float`) and the
   stroke accumulation buffer (~8 MB, `R16Float`). Harmless at 2048², fatal at
   300 DPI or webtoon sizes. **No longer deferred for the previous reason** — GPU
   rasterization has landed and did not need to rewrite the tile layout, so this is
   now the standalone next piece of the Q13 work rather than a side effect of
   something else.
2. **Only one input backend is active at a time.** The mouse backend is dropped
   the moment the pen connects, so mouse drawing only works via octotablet's
   *emulated* mouse tool, which always reports pressure 1.0. Deferred because the
   proper fix is a single input-dispatch layer that multiplexes sources and offers
   events to the UI before the canvas — which is the same work as Q14. Doing them
   together avoids building the layer twice.
3. **`PenSample::time_ms` is still hardcoded `0.0`.** octotablet already provides
   `ToolEvent::Frame(Option<FrameTimestamp>)`. Until it is wired, prediction,
   stabilization, and *any* latency measurement are impossible — so this is a
   prerequisite for the step-6 `WM_POINTER` decision, not an optional extra.
   Cheap; just not yet load-bearing.
4. **`tilt` is captured but unused.** Surface Pen reports real tilt (the Veikk does
   not), so there is finally hardware to develop against when tilt-driven brushes
   arrive.
5. ~~**`openpaint_core::raster` has no production call site.**~~ **RESOLVED.** Both
   `raster` and `stroke` are now genuinely the CPU *reference*: painting happens on
   the GPU, and `stroke_layer.rs`'s tests rasterize identical dabs through both
   paths and compare the resulting pixels. That is what guards the falloff curve,
   which necessarily exists twice (Rust and WGSL). The role is earned.

6. **The CPU `Canvas` no longer holds painted pixels.** The GPU is authoritative
   (DECISIONS §4a). `Canvas` carries dimensions plus the tile machinery that the
   eventual cache and readback will build on.
   **Undo no longer needs readback** — it snapshots the touched region GPU-to-GPU
   and redoes by replaying dabs (`history.rs`). **PNG export does read back** and now
   exists (`export.rs`), so the readback machinery is written; the *native* format
   still waits on the document model (Q6), not on Q13.

7. **Verified by test, not by hand:** the GPU/CPU pixel comparison covers the paint
   math, but the *wiring* from real pen input through to the GPU can only be
   confirmed by drawing. Injected input cannot reach RealTimeStylus unless the
   window is foreground, and Windows refuses to grant that to a background process.
   So a human draw remains the acceptance test for input-path changes.

Not shortcuts, simply unbuilt and scheduled: pan/zoom/rotate, GPU dab
rasterization, `Document`/`Page`, layers, undo, and the tuned falloff curve (Q7a).

### Q16. Input bindings are hardcoded, not data
DECISIONS section 6 states that input mapping should be **data** -- every action a
bindable, user-remappable command. The navigation bindings added with pan/zoom
(space+drag / middle-drag to pan, wheel to zoom, `[`/`]` to rotate, `0` to fit, `1`
for 100%) are hardcoded in `main.rs::handle_navigation` instead.

Deliberate, for now: a command table plus binding storage plus a remapping UI is
real structure, and building it around five navigation actions would be designing
for a command set we cannot yet see. The right moment is when tools, brushes, and
layer operations exist and the shape of a "command" is actually known.

**What to preserve until then:** navigation already routes through one function, so
there is a single place to convert. Notable detail worth keeping -- rotation is
matched on *physical* key position rather than logical character, because bracket
keys do not reliably resolve to a character (they arrive `Unidentified` under
synthetic input, and vary by layout). A future binding system needs to express both
"this character" and "this key position".

**Also open:** whether a stroke should be cancellable mid-flight (pressing space
part-way through a stroke currently suppresses further painting but does not undo
what has landed).

## Lower-priority / later

### Q8. PSD compatibility depth
Import first (blend modes, layer groups, masks, text?). How faithful? Export?

### Q9. Webtoon export specifics
Slice height defaults per platform (Webtoon/Tapas/etc.), file naming, guides.

### Q10. Wintab timing → stays Phase 4
Briefly looked urgent on 2026-08-27 when Wintab appeared to be the only API
carrying pressure on the Veikk; that turned out to be a driver toggle (Q10d), so
the original plan stands. Windows Ink is confirmed sufficient.
Still open: which phase exactly; how to detect/toggle Wintab vs Windows Ink at
runtime (auto-detect whichever actually reports a pressure axis and prefer it?
user override, as Krita does?); and whether to wrap an existing crate or bind
`wintab32.dll` directly. Note `wintab32.dll` is present on the author's box,
shipped by the Veikk driver, so it's testable whenever we want it.

### Q11. Cross-platform reach
macOS/Linux as shipping targets (not just dev)? iPad/Android someday? Affects
input backends and UI choices.

### Q12. Project name / branding
"OpenPaint" is the repo name — final product name? License headers depend partly
on this.
