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

## The scroll wheel

A scroll is aimed at whatever is under the pointer, exactly as a press is. **Over a floating
window it belongs to that window and goes no further.** The canvas zooms on scroll, and a floating
window sits over the canvas, so without this a scroll meant for a panel zoomed the drawing behind
it — which is the one thing the user was not looking at.

There is nothing special about floating here, and the rule is not written for floating. It is the
same rule the press already follows: the topmost thing under the pointer takes the input. Floating
windows are simply the only chrome that overlaps the canvas, so they are where the missing rule
showed.

## Resizing

Two panels side by side are resized by the divider between them. **A floating window is resized by
its edges**, and by the same controls in every respect: the same grab thickness, the same minimum
touch target, the same instant grab on press, the same live movement, the same minimum size a
panel is allowed to shrink to.

- Every edge and every corner. A corner moves both.
- This is true of a window holding one panel and of a window holding several. A window with two
  panels stacked already had the divider between them; that resizes *them*, and the edges resize
  *the window* — the two do not overlap and neither replaces the other.
- Nothing about it is a floating-window special case at the level of the gesture: it is a splitter
  whose other side is the screen.
- **A header beats the border where they meet**, exactly as it beats a divider. Both targets have
  to be generous, so they overlap along the top; the one you can see wins. The border is still
  reached from *outside* the window, which is where every other windowing system puts it.
- **The border never eats the handle.** Its inner half gives way on a small window so that at
  least a header's worth of interior survives, reaching nothing at all at the smallest a window
  may be. Otherwise a window could be shrunk and then never moved again.
- A window you can see never loses a press to the invisible outer half of one you cannot: window
  rectangles are all asked first, and only then their borders.
- A resize keeps something on screen, the same as a move does — and it does so by stopping the
  edge being pulled, never by sliding the one nobody touched.

## No hamburger

There is no three-lines glyph, and there never should have been one. It read as a button but the
space beside it worked too, so it taught the wrong thing about where to press.

What it was standing in for is real and stays: **there is always a touchable space for the
window's own settings**, however many tabs there are and however small the window is. That space
is the panel strip, which is why the strip is reserved before the tabs are laid out rather than
after — tabs wrap or give up room; the strip never does. Its size is the same 4 mm floor every
other grab target has.

The strip is not drawn as a button because it is not one button: it is the part of the header that
is not a tab, and it means "this panel as a whole".

## Choosing where something goes

Nothing lands anywhere by default. When a panel needs a home, **the user points at one.**

This happens in two places, and it is the same thing both times:

- *Put back into* in a floating window's settings. It does **not** offer a list of places. The
  window stops floating, and the next place tapped is where it goes.
- Switching a panel **on** in the panel list. It does not appear somewhere of the workspace's
  choosing; the same pick follows, and the user says where.

While a pick is waiting:

- The workspace shows it is waiting, and shows what would happen where the pointer is — the same
  five zones a drag lands in, drawn the same way.
- The next primary press commits it. Nothing else in the workspace answers that press: no panel's
  controls, no popup, no drag.
- **Cancelling is Escape or a secondary press**, and a press with nowhere to land — the gap around
  a floating window, off the workspace entirely — cancels too rather than dropping the panel
  somewhere arbitrary. Cancelling a *Put back into* leaves the window floating exactly where it
  was; cancelling an enable leaves the panel off.

**A pick is asked about the arrangement, and only the arrangement.** Floating windows are neither
destinations nor obstacles: they are transparent to the question. That is what "put it *back*"
means, and it is what lets a leaf sitting behind a palette be pointed at at all. Panels go *into* a
window by being dragged there, which is a different gesture with a different rule.

Separately, a panel may only float at all if its own settings offer *Float* — the same table
decides both, so the button and the act cannot disagree. The canvas is the one that says no, and
the reason is not a rule about canvases: it is drawn by the GPU *underneath* egui, so a window's
own background would be painted straight over the artwork.

One pick is waiting at a time. Starting another replaces it, and **any change to the arrangement
underneath ends it** — undo, reset, floating something else. The way back is a snapshot of the
arrangement the panel came out of, and a snapshot of an arrangement that has since changed is not
a way back; the version that ignored this let undo restore a tree still holding the panel in the
air, so answering the pick put a second copy in.

## Removing a panel

Every settings popup — a panel's own, and a window's — offers **Remove this panel**.

It is the same act as switching the panel off in the panel list, down to the same function: one
way in the code, two ways to reach it. A panel removed this way comes back the same way any other
does, and undo takes it back.

Every panel, the canvas included. Nothing in the workspace is exempt from being closed — that is
what makes "everything closed" a state an artist can reach and come back from rather than a
theoretical one, and the panel list is always a secondary press away.

## One gesture, one step

Every drag records **one** entry on the undo stack, taken at the press and written at the release,
and only if the arrangement actually ended up different. Not two — pulling a tab out of a window
and dropping it somewhere is one thing the artist did, and needing two presses of undo to get back
is a workspace that does not remember what happened, it remembers how it was implemented. And not
none: moving a window used to record nothing at all while resizing one recorded a step, which is
worse than either rule applied consistently.

Escape and a lost pointer put back the **whole** arrangement, floating windows and their sizes
included — not just the tree the drag was told about.

## Carrying and sizing are not the same gesture

A window that is **carried** goes where the pointer goes, so where the pointer ends is where the
window ends, and letting go over another window merges the two. A window that is **sized** does
not: it stays where it is, or shrinks away, while the pointer travels — so the pointer can leave it
entirely. Only a carry can merge. Getting this wrong meant sizing a window down and letting go over
its neighbour destroyed the one being sized.

The same is true of a divider *inside* a window, which moves neither the window nor the pointer's
relationship to it.

## Floating windows are always in front

Of the arrangement, of what a panel draws inside itself, and of what the canvas draws over the
artwork — a selection outline or a crop box does not show through a palette sitting on top of it.

The whole stack, bottom to top: the ground and the chrome; what a docked panel draws; what the
canvas draws; floating windows; a gesture's own marks; the popup. **Six things, six layers, one to
each.** Two things sharing a layer are two things whose order is undefined, and undefined came out
as "sometimes in front" — which is exactly how it was reported.

## A divider has two appearances, not three

Available while the pointer is near it, and taken **from the instant it is pressed until the
instant it is let go**. Nothing in between and nothing after: there is nothing to wait for, because
a divider and a window's edge are both grabbed the moment they are touched, and the hold does
nothing for either.

It used to have a third — nothing at all — twice over. The press put it into the hold animation,
which begins at no alpha, so it went blue on hover, blank for the first frames of the press, and
blue again once it moved. And once the hold *finished*, it went blank again, because a divider
being held perfectly still has nothing to report and the mark was being read off what the frame
reported. Holding still is the one thing that should change nothing.

So the mark is read off **what the pointer has hold of**, which is true every frame until it lets
go, rather than off what this frame had to say.

## Reach, and what beats it

Three targets are deliberately wider than they look, and where two of them overlap the rule is
always the same: **the one you can see wins.**

- A tab beats a divider where they meet.
- A header beats a window's resize border where they meet.
- A window's rectangle beats another window's border.
- Visible chrome in the arrangement beats a window's border reaching out over it.

A window's border reaching over the *canvas* is the one case with nothing to lose to, and there it
keeps its full reach — which is why the wheel and the pen treat that ring as the window's too.
