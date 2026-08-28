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
Auto-grow remains an option, not the default.

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

### Q6. File format specifics
Container is "zip of tiles + JSON" in spirit. Open: exact tile encoding
(raw/compressed? per-tile format?), metadata schema, versioning strategy,
thumbnail/preview embedding, autosave/recovery model.

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

### Q13. Tile residency + who owns tile pixels ⚠️ NEEDS DESIGN BEFORE LAYERS/UNDO
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
