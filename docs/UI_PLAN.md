# OpenPaint — the UI plan

> How the interface gets built, in order, and why each step is where it is.
> Written 2026-08-29 so the plan survives a fresh session with no context.
>
> `DECISIONS.md` holds the settled principles (§1b what the UI must be, §1c the
> layout model). This is the sequence of work that follows from them.

---

## Where things stand

**Done and proven** — all headless-testable, all with sabotage sweeps:

| Piece | File | What it is |
| --- | --- | --- |
| Layout tree | `layout.rs` | Splits, leaves, tabs, insert/remove, splitters, layout undo |
| Gesture | `panel_drag.rs` | Hold-to-arm, drop resolution, divider drag, cancel |
| Chrome geometry | `chrome.rs` | Header/tab/content rectangles, hit-testing, drop regions |
| Theme | `theme.rs` | Every colour and measurement, as JSON with hex colours |
| Workspace | `workspace.rs` | Draws it, drives it, holds the default layout |

**Live in the app behind F2.** Six panels: Menu, Tools, Canvas, Brush, Layers,
Colour, History. The old side panel still holds thirteen sections and is what
the app is actually used with.

| Key | |
| --- | --- |
| F2 | workspace ↔ old panel |
| F3 / Shift+F3 | layout undo / redo |
| Ctrl+F3 | reset layout |
| F4 | switch theme, and write `theme.json` out to edit |
| Esc | abandon a panel gesture |

---

## The three levels, and the line between them

This distinction is load-bearing. It is what stops the panel system from
swallowing the engine.

1. **Controls** — slider, button, label, list row, checkbox, dropdown, colour
   picker, text field, drag value, scroll area. **Ten kinds**, counted from the
   existing UI rather than invented. This is the vocabulary.
2. **Panels** — lists of controls. Ours are *presets*; a user composing their
   own is the same list built by hand.
3. **Tools and commands** — the lasso, the brush, Export PNG. **Engine
   behaviour.** A panel holds a *button that selects the lasso*; it does not
   hold the lasso.

---

## The order of work

### 0. Three small things first

Cheap, and each closes a gap that makes the workspace feel like a demo.

- **Save the layout.** Closing the app loses the arrangement. The tree is small
  and serde is already a dependency; it goes beside `theme.json` and
  `brushes.json`. Serialise panels **by name**, not by id, so the file survives
  the `PANELS` table changing.
- **Open and close panels.** There is no close button and no panel list, so
  `Layout::remove` and `Workspace::open` are unreachable from the UI — which
  makes the menu decorative and "everything closed" theoretical. A list of
  toggles in the Menu panel. *Not* a per-tab close button: too small a target,
  and it would compete with tap-to-switch.
- **Panels reflow when narrow.** A panel narrower than its contents should wrap
  — the tool rail already does. Worth solving once in the chrome rather than per
  panel, since any panel can end up any shape.

### 1. The descriptor engine — and this is what replaces egui

**A panel is a list of controls, described as data.** Rendering that list is one
function; that function *is* our widget layer.

The decision that got this right, and it came from the author: porting panels as
hand-written UI code would mean writing them once against egui and again against
our own widgets. Porting them as *descriptions* means the renderer is written
once. So there is no intermediate egui-backed version worth building — **the
descriptor renderer is the widget layer**, and egui is not replaced so much as
never given the job.

Three things make this safe, and none was true a week ago:

- The vocabulary is **counted**, not guessed: ten kinds, from the existing UI.
- The hardest piece is already owned — text layout, via `openpaint-text`.
- The old F2 panel keeps working throughout, so nothing is blocked.

**Stage 1 — descriptors with closures.** A control carries its label, kind,
range and a getter/setter. Enough for one renderer and a cheap port.

**Stage 2 — named parameters, later.** Every adjustable value addressable by
name rather than by closure. That is what makes panels serialise, user-made
panels real, and a command palette or scripting API possible. Much easier once
stage 1 exists and every control is already described. Do not attempt it first:
the parameter set should be *read off* the ported panels, not invented.

**The escape hatch, and its limit.** Not everything is declarative. Export is an
action, the curve editor is custom-drawn, the layer list has drag-reorder. So a
panel item is *either* a described control *or* a callback drawn by code. Reading
the counts, roughly ninety per cent goes declarative and three things do not. **If
half the panels end up custom, this layer has bought nothing** — that is the
signal to stop and rethink, not to add a fourth escape hatch.

### 2. Port, one panel at a time

Build the renderer for **label, button, slider, list row** first — about 130 of
the app's ~180 controls. Then port **Brush** end to end, because it exercises
sliders, a list and a button and will say whether the descriptor design is right
while only one panel depends on it.

Then the rest, adding control kinds as panels need them. Each panel becomes
visible the moment its controls are supported, so there is never a stretch where
the workspace holds less than it does today.

**egui is gone when the last section moves.** It disappears as a consequence of
the port, not as a step before it.

Sections still to port: Pages, Export, Transform, Text, History, Crop, Page
setup, Select settings, Canvas memory, Speed.

### 3. What is genuinely hard, and what is deferred

- **Focus and keyboard.** Real work, unavoidable, no shortcut.
- **Text input with IME.** The hard one — a Japanese or Korean letterer cannot
  type without it. There are **four text fields in the whole app**, so: a plain
  field first, IME when someone needs to letter in Japanese. Deferred
  deliberately, not forgotten. Note egui has partial IME support today, so this
  is a capability given up on purpose.
- **Icons.** The tool rail's glyphs are placeholders. Wanted, and they arrive
  with the widget layer rather than before it.
- **Scroll areas.** Two uses. Easy.

### 4. After the port

- **Named parameters** (stage 2 above), and with them **user-made panels**.
- **Touch gestures.** A real future target, not hypothetical. The property to
  preserve meanwhile: *nothing depends on hover*. Today nothing does — the
  divider and tab hover marks are bonuses for a pen, and the hold is what
  actually starts every gesture. Keep it that way.
- The features waiting for a real UI: layer groups, speech bubbles, a brush
  editor, panel and frame tools, rulers and perspective guides.
- **Render the float on the GPU** (`TODO.md` §3), the one performance item with
  a known ceiling.

---

## Settled — do not re-litigate

- **Everything is a panel**, including the menu, the tool rail and the canvas.
  The layout never branches on which panel it holds (§1c).
- **Hold, then move.** Every layout gesture — moving a panel *and* resizing a
  divider — needs the pointer held still first. This is what lets grab targets
  be generous, because they are never live at the same moment as anything else,
  and it makes touch and pen behave identically.
- **A header beats a divider** where they overlap.
- **Targets are specified in millimetres**, via logical units being 1/96 inch.
  4 mm is the floor for anything grabbable.
- **Cancel needs no gesture.** Dropping a panel back where it came from is
  already a no-op, and Ctrl+Z covers the rest. Adding a shake or a chord would be
  inventing a way to trigger something by accident.
- **The theme is data.** A widget reads a token and never writes a colour.

---

## Known gaps, honestly

- The workspace's panel contents are a first pass — enough to prove the system,
  not the full set.
- `Outcome::Floated` reopens the panel rather than floating it; floating windows
  are a second `Layout` in a second window and are not built.
- The app test binary dies with an access violation about one run in ten
  (`TODO.md` §2). Never in a named test; suspected wgpu teardown.
- Two UIs exist at once, which is the transition and not a feature. It ends when
  the port does.
