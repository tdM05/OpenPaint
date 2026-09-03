# Contextual panels: where a tool's settings belong

> **Status: designed, agreed with the author, not built.** Written 2026-09-03. This is the spec to
> build from, and the second half is the handoff for whoever picks it up.

## The problem

Brush, Select, Transform and Text are four tabs in one strip, as though they were four of a kind.
They are not. They are **three different kinds of context wearing one costume**:

- **Brush** and **Select** are genuine *tool options*. What they show depends on which tool is in
  the artist's hand, and only one tool is ever in it.
- **Text** is a *layer property*. It edits the active text layer's words. Nothing about it depends
  on the tool, and it is meaningless while a raster layer is active. This follows from a decision
  already made: §6a made text a layer *kind* rather than a tool, so its settings belong to the
  layer.
- **Transform** is neither. It is a *task in flight* — it exists only between Begin and Apply, and
  it has a Cancel. A panel that is empty for all but a few seconds of a session is not a peer of
  the other three; it is a tab claiming to be one.

The cost is not the click. It is that the artist has to know which of three unrelated categories a
setting lives in before they can look for it, and the categories are not written anywhere.

## What the others do

- **Photoshop**: an *Options bar* pinned under the menu that swaps with the tool, a *Properties*
  panel that follows the selection, and a separate dockable *Brush Settings*.
- **Clip Studio Paint**: Tool → Sub Tool → *Tool Property*, and a separate *Layer Property*.
- **Krita**: a *Tool Options* docker that swaps with the tool.
- **Procreate**: none of it — everything is a popover from the top bar, because on a tablet there
  is no room for anything else.

The common thread in every desktop one: **one surface per kind of context, always in the same
physical place.** The value is not screen space. It is that the artist's eyes never leave the
canvas to hunt for a setting.

## The design

Two contextual panels, one per real context, and transient tasks that own neither.

### 1. The Tool panel — follows the active tool

Shows the settings of whatever tool is in hand:

| Tool | What it shows |
|---|---|
| Brush, Eraser | the brush sections: size, opacity, hardness, spacing, roundness, angle, flow, stabilisation, presets, tip, response curves |
| Lasso, Rect, Wand | the selection sections: the wand's tolerance and expand, fill-on-click, and the selection commands |
| Move | its own, when it has any |

### 2. The Properties panel — follows the active layer

Shows what the active layer is *made of*. A text layer shows the caption editor; a raster layer
shows nothing yet, or the layer's own settings if they are moved here later.

**One rule, and it is worth stating because Photoshop's equivalent has none:** this panel shows
what the active layer is made of, and nothing else. It is not a home for whatever is contextual
this week.

### 3. Transform and crop are transient

They take the Tool panel while in flight and give it back on Apply or Cancel. Better still, they
grow numeric entry on the canvas beside the box — which is already `TODO.md` §4 — and stop needing
a panel at all.

## Why this is cheap, and what actually changes

Almost nothing in the architecture. A panel already receives the whole `Status` and answers with
`controls()`. A contextual panel is a `controls()` that matches on `state.tool` and delegates to
the brush or select module. The workspace, the layout tree and the descriptor layer learn nothing.

**The real shift is conceptual: the unit becomes the *section*, not the panel.** The modules in
`crates/openpaint-app/src/panels/` stop being "one panel each" and become "one section each",
composed either into a contextual panel or into a standalone one.

### This is also the answer on tabs, and it is not a compromise

Some people want a tab strip; some want the setting always in one place. Sections give both
without choosing. The same `brush::controls()` serves the contextual Tool panel *and* a standalone
Brush panel opened from the panel list and docked wherever the artist likes. The default
arrangement gets the contextual pair; anyone who wants Brush permanently visible while
transforming opens it as its own panel, exactly as today.

## What not to copy from Photoshop

The taxonomy is worth taking. The mechanism is not.

- **The Options bar is special-cased chrome.** Pinned under the menu, not dockable, not closable,
  not movable. That is precisely the exception DECISIONS §1c says this UI does not get to have. A
  contextual panel here is an *ordinary panel in the layout tree* that happens to choose its
  contents from `Status`; it floats, docks, tabs and closes like any other.
- **Photoshop splits the brush across two places** — size and opacity in the Options bar, the rest
  in Brush Settings — so "where is spacing" has a different answer from "where is size". That is a
  wart, not a design.
- **Their Properties panel is a grab-bag**: layer properties, shape properties and adjustment
  properties, with no rule about what appears. See the one rule above.

Consider **horizontal**, under the menu, for the Tool panel's default placement: it costs no canvas
width, and `Direction::Row` is already a per-panel setting, so it needs no new concept. Placement,
not chrome — the artist can still move it.

## What will go wrong, and what to do about it

These are the known costs. None is a reason not to do it; all four need an answer in the build.

1. **Contents moving under you.** The standing complaint about contextual panels: switch tool, the
   panel changes height, everything jumps. CSP has this and it is the main irritation. Keep a
   stable section order, and do not let the panel resize itself on a context change.
2. **Discoverability.** A tab advertises that Text exists; a contextual panel may never tell you.
   The habit for this is already in the codebase — *"Rect has nothing to set."*, *"Fill and Clear
   need a selection."* An empty contextual panel that says what would fill it is better than a tab,
   not worse. Every context with nothing to show must say what would give it something (§6b).
3. **Half-finished state is per panel and would need to be per context.** `PanelInput` holds the
   scroll offset and the text field with the caret. Switching tools must not drop the artist
   halfway down a list they never scrolled, and must not carry a half-typed preset name from the
   brush into the wand's tolerance.
4. **The control atlas names controls by panel.** Scenarios say `Select:Lasso` and `text:Caption`.
   Folding those into a Tool panel renames them. Do it as one deliberate pass, not scene by scene —
   `DRIVE_LOG.md` records what happens when scene names and reality drift apart.

## How to build it

**Start with Properties for text.** It is the clearest case, it touches one module, and it answers
the only question that matters — does contextual behaviour feel right — before anything is
restructured. Text is unambiguously a layer property, and its panel is dead weight the rest of the
time, so the win is visible immediately.

Then the Tool panel: brush sections first (Brush and Eraser), then the selection tools, then decide
what Transform does with the space.

**Done means what it means everywhere else in this repo** (`RELEASE_PLAN.md`):

1. Unit tests for the pure `controls()`/`picked()` parts, and a **sabotage check** on every new
   guard — the guard broken on purpose, the named test seen to fail.
2. A driven scenario in `tools/scenes/` that switches tool and asserts the panel followed, and that
   an empty context says what would fill it.
3. `cargo test --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt --all --check`
   clean, in debug *and* release.

---

# Handoff

## Read these first, in this order

1. `docs/DECISIONS.md` — every decision and the reason for it. §1a (CSP EX is the north star for
   UX, not internals), §1c (the layout tree has no exceptions), §6b (nothing may fail silently).
2. `docs/DRIVING.md` — how the application is driven and tested through its real UI.
3. `docs/DRIVE_LOG.md` — what driving has caught that unit tests could not, and the four shapes
   that make a scenario lie. Read the 2026-09-03 entries before touching `tools/`.
4. `docs/TODO.md`, `docs/OPEN_QUESTIONS.md`, `docs/RELEASE_PLAN.md`.

## The standing instructions from the author

- **Quality first, not quantity.** Where there is a choice, take the higher-quality and more modern
  option; the bar is CSP EX, not "good enough".
- **No band-aid designs.** *"we should never have to do these compensate things."* If a fix is a
  compensation for a bad shape, fix the shape.
- **As little hardcoding as possible**, so that behaviour follows from the design rather than from
  a table someone has to maintain.
- **Write everything down.** Decisions go in `DECISIONS.md` with their reasoning; work that is
  decided but not done goes in `TODO.md`.
- **Test in the UI, not only in `cargo test`.** 926 unit tests were green while the brush painted
  nothing. That is why the scenario harness exists.
- **Fix defects rather than reporting them and asking.** If something is broken and it stands
  between the work and its goal, fix it and say what you fixed.
- It must run well on a **Surface** — an integrated GPU with shared memory.

## Running things

```
cargo test --workspace            # 971 tests; also run with --release, CI does
cargo clippy --workspace --all-targets
cargo fmt --all -- --check
./tools/sweep.ps1                 # 29 scenarios, 1180 assertions, drives the real UI
```

**The sweep takes the mouse and keyboard for about fifteen minutes.** Nothing else can use the
machine while it runs.

### Two things about the harness that will otherwise cost you an hour each

- **It refuses to run while the artist's own OpenPaint is open**, and that guard is not
  negotiable: the run moves `workspace.json`, `brushes.json`, `theme.json` and the recovery folder
  aside and puts them back in a `finally`. A running copy would lose its autosaves into the gap. If
  a run is *killed*, that `finally` does not happen — check `%LOCALAPPDATA%\OpenPaint` for
  `.driving` files and put them back before doing anything else.
- **The scenes are calibrated for a display scale of 1.5**, which is the Surface's own screen. The
  workspace is laid out in logical units, so at scale 1.0 the same window is 2184x1411 instead of
  1452x929 and no coordinate in `tools/scenes/` survives. The harness now refuses outright and says
  so. On the Surface as the only display this needs no arguments; with a second monitor attached,
  put the window on the Surface with `-X` (see `tools/sweep.ps1`).

## Where the code is

- `crates/openpaint-app/src/panels/` — one module per panel today, one per *section* after this
  work. Each is a pure `controls()` plus a pure `picked()`/`answer()`, testable without a GPU.
- `crates/openpaint-app/src/panel_ui.rs` — the `Control` vocabulary and layout.
- `crates/openpaint-app/src/panel_draw.rs` — the only thing that knows what a slider looks like.
- `crates/openpaint-app/src/workspace.rs` — the layout tree, floating windows, popups, gestures.
- `crates/openpaint-app/src/ui.rs` — `Status` (everything a panel may read) and the modal prompts.
