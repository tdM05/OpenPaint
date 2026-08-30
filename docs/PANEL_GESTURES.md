# What a press on panel chrome means

Written down because it has been got wrong repeatedly, and because "I think that is what you
meant" is not a specification. This is the whole rule. Anything the code does that is not here is
a bug in the code, not an omission here.

## The two parts of a header

A header is made of **tab buttons** and **the panel strip**.

- A **tab button** is one panel. It looks like a button, it is separated from its neighbours, and
  it is the only thing that acts for that panel.
- **The panel strip** is the rest of the header. There is *always* some of it, however many tabs
  there are — tabs wrap onto another row before they are allowed to eat it. It acts for the panel
  as a whole.

A header with **no tabs at all** — a compact one, on the tool rail or the menu — is entirely panel
strip, and that strip is the handle for the single panel it holds.

## What each does

| where | tap | hold | drag |
|---|---|---|---|
| tab button | show that tab | that panel's settings | take **that panel** out and move it |
| panel strip, docked | nothing | nothing | **nothing** |
| panel strip, floating | nothing | the shown panel's settings | move the **whole window** |
| panel strip, no tabs | — | that panel's settings | move that panel |

Dragging the panel strip of a docked panel does nothing **for now**. It is the natural place for
"float everything in this panel", or for moving a whole tab group somewhere else, and it is left
free rather than given a second meaning.

## Floating and docked are two modes

**Dragging never crosses between them.** The way across is the panel's own settings: *Float*, and
*Put back into*.

A floating window dropped on the arrangement would otherwise dock itself the moment it was moved
anywhere useful, which makes moving one impossible.

## Inside the floating mode

- Dragging a floating window **follows the pointer**, keeping the place it was taken hold of.
- Dragging a **tab button** out of a window holding more than one panel makes that panel its own
  window, which then follows the pointer. The window it came from stays where it is.
- Dropping a window **on another floating window** docks it in at the five zones — left, right,
  top, bottom, centre — exactly as a docked panel behaves among docked panels.
- Dropping it anywhere else leaves it where it was put.
- A window whose last panel leaves is taken down.
- The window under the pointer is never the one being dragged: a dragged window follows the
  pointer, so it would otherwise always win its own hit test and could never be dropped on
  anything. **This was a real bug**: dragging the lower of two windows onto the upper worked, and
  the reverse silently did nothing.
- Pressing a window brings it to the front, so the one you can see is the one you get.

## How this is tested

Through the same path the application uses. Earlier tests called the gesture handler directly with
a hand-computed idea of which window the pointer was over — which is exactly the thing that was
broken, so they agreed with the bug. `Workspace::input_frame` is now the one entry point: `show`
calls it, and so do the tests, so a test cannot disagree with the application about what is under
the pointer.
