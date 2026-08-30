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

### Progress

**Step 0 is done** (persistence, open/close, wrapping headers) and **stage 1 of the descriptor
engine is in**: `panel_ui.rs` holds the vocabulary and every decision worth being wrong about,
`panel_draw.rs` turns it into pixels using egui only as a surface to paint on. Brush, Layers and
History are described rather than drawn. Colour and Tools are not yet, and Canvas never will be.

What the port turned up, worth recording:

- **A row needs to be able to hold more than one target.** A layer row wants to be chosen *and*
  have its visibility flipped, and `Control::Row` can only be chosen. For now visibility is a
  `Toggle` below the list acting on the active layer, and the chip on each row shows the state.
  The fix is a row that carries trailing controls, not a special layer row.
- **Scrolling belongs to the renderer, not the layout.** It is a subtraction from where the list
  starts, so `place` never heard of it and did not have to change.
- **`Custom` was cut before it shipped.** Nothing needed it yet, and a variant with no user is a
  shape designed against a guess. It goes in with the colour wheel, which is the first thing that
  genuinely cannot be described.

## What a panel is, settled

The first plan said an artist would *assemble* a panel from controls. That is wrong, and the
reason is worth writing down: a button is not an atomic unit. "Merge down" only means anything in
a Layers panel, sitting beside "Add" and "Duplicate", operating on the layer that panel has
selected. Dragging it into a Brush panel produces a control with nothing to act on. A panel's
controls are its vocabulary, not interchangeable parts, and a builder that pretends otherwise
produces exactly the mess this project exists to avoid.

So: **panels are written, one function each. What is customisable is their settings.**

That is not a retreat from the descriptor layer. It is what the descriptor layer is *for*:

- **The panel decides what controls exist.** Written by hand, per panel, because that is the part
  that carries meaning.
- **The engine decides how they are laid out, drawn and hit.** Shared, so there is one visual
  language and one set of target rules, and so egui can be removed once rather than seven times.
- **A setting is a property of the engine, offered to every panel.** `Flow` is the first:
  row, column, or auto. It is not a rule about the menu; the menu just has a different default
  from the layer list. Adding a setting adds it everywhere at once.

Two levels of setting, then:

| | decided by | example |
|---|---|---|
| universal | the engine | direction, density, labels or icons |
| the panel's own | the panel | colour wheel shape; which tools the rail shows |

Both are data, both are per panel *instance*, both save with the workspace.

### Where the settings live

Not a gear button on every panel: that is a permanent pixel cost for something used twice a year,
and seven of them is visual noise. Not right-click alone either, because touch has no right-click.

The gesture vocabulary already answers this. Hold-to-edit exists precisely so touch and pen behave
the same, and Windows itself already teaches hold-as-right-click. So one rule, applied by *what*
you held:

- hold a **header**, then move --- move the panel (already true)
- hold a **header**, then let go without moving --- that panel's settings
- hold the **workspace ground** --- the workspace's own list of panels (already true, as
  right-click)

Every panel has a header, always, so there is no panel this cannot reach --- which is the mistake
that put the panel list inside a closable Menu. Right-click stays as the fast path for anyone with
a pen or mouse.

**Where it has got to.** `Flow` ships as a per-panel *kind* default from the `PANELS` table:
`Auto` for the menu, `Wrap` for the tool rail, `Column` for the rest. What is still missing is the
part that makes it a setting rather than a decision --- storing it **per panel instance**, saved
with the workspace, and the hold gesture to reach it. That is the next batch, and it is the point
of the whole exercise: until then these are defaults, not choices.

## What is still fundamental

**Wiring, not invention.** The three pieces that were missing all exist now as tested modules; what
is left is calling them.

1. **A colour wheel panel.** `colour_wheel.rs` has the geometry for all three shapes, hit-testing
   and marker placement, with press and marker proved to agree. It needs drawing and a panel. It
   also brings `Custom`, the escape hatch for anything that cannot be described as a list.
2. **A text field.** `text_field.rs` has caret, selection, word motion and hit-testing, on UTF-8
   boundaries and fuzzed. It needs a `Control::Text`, a caret to draw and keyboard events. Stepping
   is by `char`, not by grapheme cluster: one dependency fixes that, and two tests are named to
   fail when it lands rather than pass silently.
3. **The remaining ports**: History, and the old side panel's sections --- transform, text, pages,
   export, crop, wand. egui leaves when the last one lands.

## The gestures, as they now stand

| you do | it does |
|---|---|
| touch a tab or a divider and move | moves or resizes, **at once** |
| touch and hold still | that panel's settings |
| tap a tab | shows it |
| drag anything in a floating window | **moves the window**, following the pointer |
| drop a floating window on another | docks into it, at the five zones |
| secondary press on a header | the same settings, for a pointer that has one |
| secondary press anywhere else | the workspace's panel list |

Holding *first* was tried and was wrong. The reason it existed was to stop a stroke that began on
some chrome from drawing on the canvas --- except that a press on a tab or a divider never reaches
the canvas, so the third of a second before anything answered was a cost with no benefit. Holding
is now what asks a question, which is the one thing left over once moving means moving.

## Floating windows

**A floating window is not a special kind of panel; it is a second workspace in a rectangle.** It
holds a `Layout` exactly as the docked arrangement does, so it gets chrome, tabs, holds, drags and
the five-zone drop from the same code.

It was a single panel with its own gesture code first, and that was the fault behind every
complaint about it: no tab to press, no hold, no drop zones --- each of which the docked panels
already had, and none of which the copy inherited. The hold in particular never fired at all,
because painting is demand-driven and only the docked path asked for the frames that let a timer
run.

**Floating and docked are two modes, and dragging never crosses between them.** The way between
is the panel's own settings --- "Float", and "Put back into". A floating window dropped on the
arrangement would otherwise dock itself the moment it was moved anywhere useful, which makes
moving one impossible.

Inside the floating mode, dragging a window follows the pointer and dropping it on *another*
window docks it in at the five zones, exactly as a docked panel behaves among docked panels.

A gesture keeps the arrangement it began in; with nothing in flight, the surface is whatever is
under the pointer, so the hover marks show what a press *would* take hold of.

Above the arrangement rather than outside the application's window: a separate operating-system
window needs a second surface for the GPU and a second path through the event loop. Nothing here
forecloses it --- a floating window is already an arrangement with its own rectangle, which is what
an OS window would give it.

## Looking at it

`screenshot.rs` renders the workspace headlessly and writes PNGs:

```
cargo test -p openpaint-app --bin openpaint -- --ignored shot_
```

They land in `crates/openpaint-app/target/screenshots/`. Not assertions --- a golden image fails on
every font and driver and teaches everyone to ignore it. These are for a person to open, and they
have already found three things no test did: a menu bar clipped in half at 900x600, a slider fill
too faint to read, and every filled icon drawn as a fan of overlapping wedges.


## 1. The descriptor engine — and this is what replaces egui

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
