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

### Q2c. Dev-box lint tooling (rustfmt/clippy)
This Linux dev box has Rust via the system package (`/usr/bin/cargo`), NOT
rustup, and no `rustfmt`/`clippy`, and no sudo to apt-install them. So CI's
fmt/clippy steps are currently **non-blocking** (`continue-on-error: true`) to
avoid failing on lints we can't verify locally. TODO: get rustfmt+clippy
available (rustup toolchain in a user dir, or ask admin for the apt packages
`rustfmt` + `rust-clippy`) and then make those CI steps blocking again.

**Status: workaround in place; re-harden later.**

---

## Medium-priority (Phase 1–2)

### Q4. Final UI framework
Deferred by design. Start with egui debug panels; decide the polished CSP-like
UI framework later (egui custom-drawn? another Rust UI? something else?).

### Q5. Color management depth
Confirmed: composite in linear space. Open: how far on color management —
sRGB-only v1? ICC profiles? Wide-gamut/display-P3 support? CMYK for print later?

### Q6. File format specifics
Container is "zip of tiles + JSON" in spirit. Open: exact tile encoding
(raw/compressed? per-tile format?), metadata schema, versioning strategy,
thumbnail/preview embedding, autosave/recovery model.

### Q7. Brush engine: port libmypaint vs. build our own
Depends on Q1 (license). If GPL-compatible, porting/wrapping libmypaint could
save enormous time. If permissive license, likely build our own dab engine.

### Q7a. Round-brush fidelity target = Photoshop/CSP soft round
The default round brush should aim to feel/look like Photoshop's (and CSP's)
soft round brush. That is a **Phase-1 quality goal**, not the first
proof-of-pipeline stroke. To match it we must get right: the hardness FALLOFF
CURVE (PS's is a specific tuned curve, not plain linear/Gaussian), the FLOW vs
OPACITY accumulation model (how overlapping dabs build up within one stroke),
correct SPACING (~25% diameter default in PS), anti-aliasing, and LINEAR-space
blending (to avoid dark edge fringing). Tracked so we hold ourselves to it.

---

## Lower-priority / later

### Q8. PSD compatibility depth
Import first (blend modes, layer groups, masks, text?). How faithful? Export?

### Q9. Webtoon export specifics
Slice height defaults per platform (Webtoon/Tapas/etc.), file naming, guides.

### Q10. Wintab timing
Which phase exactly; how to detect/toggle Wintab vs Windows Ink at runtime.

### Q11. Cross-platform reach
macOS/Linux as shipping targets (not just dev)? iPad/Android someday? Affects
input backends and UI choices.

### Q12. Project name / branding
"OpenPaint" is the repo name — final product name? License headers depend partly
on this.
