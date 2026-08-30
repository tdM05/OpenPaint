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

Read the table **top down and stop at the first row that fits**. A compact panel fits both a
"panel strip" row and the "no tabs" row, and they answer differently; the more particular row is
the one written first, and it wins.

| where | tap | hold | drag |
|---|---|---|---|
| tab button | show that tab | that panel's settings | take **that panel** out and move it |
| panel strip, no tabs | nothing | that panel's settings | move that panel |
| panel strip, floating | nothing | the settings of the panel **whose strip it is** | move the **whole window** |
| panel strip, docked | nothing | nothing | **nothing** |

Two of those answers need saying in full.

**"The panel whose strip it is."** A floating window can hold more than one leaf side by side
after an edge drop, each with its own header and its own shown panel. Each of those headers has
its own strip, and holding one asks about the panel *that* leaf is showing — not the first panel
in the window. (Getting this wrong opened somebody else's settings, and no test could see it
until a fixture existed with two leaves in one window.)

**"Take that panel out."** *Out* is decided by the movement, not by the pointer leaving any
boundary. The panel comes away on the **first movement** after the press, and follows the pointer
from there; where it ends up is decided by where it is let go. There is no threshold to cross and
no edge to leave.

Dragging the panel strip of a docked panel does nothing **for now**. It is the natural place for
"float everything in this panel", or for moving a whole tab group somewhere else, and it is left
free rather than given a second meaning.

## The secondary press

A secondary press — right-click, or a pen's barrel button — is not one of the gestures above and
never carries anything. It asks whatever is under the pointer what it offers:

- on a panel's **header** — tab buttons and strip alike, and the whole of a compact panel's bar —
  the settings of the panel that header is showing;
- **anywhere else** — the workspace's own list of panels.

It finds that header through the same chrome the primary press is tested against, handle where the
handle actually is. Building a second idea of where a header sits is how a right-click on the tool
rail's side bar came to open the panel list.

## Floating and docked are two modes

**Dragging never crosses between them,** in either direction. The way across is the panel's own
settings: *Float*, and *Put back into*.

So every drag in the table above is confined to the mode it began in. A tab dragged out of a
floating window becomes another floating window and can only be dropped among floating windows; a
docked tab stays docked however far it is carried, and a docked panel let go over a floating
window lands **nowhere at all** — not in the window, and not in whatever docked leaf happens to
lie behind it either. The arrangement is left exactly as it was.

A floating window dropped on the arrangement would otherwise dock itself the moment it was moved
anywhere useful, which makes moving one impossible.

## Inside the floating mode

- Dragging a floating window **follows the pointer**, keeping the place it was taken hold of.
- Dragging a **tab button** in a window holding more than one panel makes that panel its own
  window on the first movement, which then follows the pointer. The window it came from stays
  exactly where it is.
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

Through the same path the application uses, and every rule on this page is held down by a test
that a deliberate sabotage of the code makes fail. A rule nothing here can break is a rule nobody
is keeping.

Earlier tests called the gesture handler directly with a hand-computed idea of which window the
pointer was over — which is exactly the thing that was broken, so they agreed with the bug. `Workspace::input_frame` is now the one entry point: `show`
calls it, and so do the tests, so a test cannot disagree with the application about what is under
the pointer.
