//! Drawing the panel workspace, and driving it with a pointer.
//!
//! The one part of the panel system that is not testable without a screen, kept as thin as it can
//! be for exactly that reason: [`crate::layout`] decides the structure, [`crate::chrome`] decides
//! the rectangles, [`crate::panel_drag`] decides what a gesture means, and [`crate::theme`] decides
//! the colours. What is left here is transcription — fill this rectangle with that token — plus
//! the wiring that hands egui's pointer to the state machine.
//!
//! # Painted, not built from widgets
//!
//! The same call `crop.rs` made and for the same two reasons. The chrome lives in *layout* space
//! and has to answer to our own hit-testing, which is where the five-zone drop resolution and the
//! tap-versus-hold rule live; and a widget toolkit's own docking would decide the structure, which
//! §1c reserves for us. egui draws shapes and text here and nothing more.
//!
//! # The pen already works
//!
//! Windows synthesises mouse messages from pen input, so egui's pointer *is* the pen for the
//! purposes of pressing, dragging and releasing (OPEN_QUESTIONS Q14, corrected). That is why this
//! needs no new input routing to be usable with a stylus today — and also why the routing is still
//! worth building later: synthesised events are coalesced and lag, which is fine for grabbing a
//! header and wrong for anything wanting a smooth drag.
//!
//! # Panel contents are not this module's business
//!
//! Contents arrive as a callback taking the panel and an `egui::Ui` positioned in its content
//! rectangle. That is the narrow seam promised when this was planned: swapping egui for our own
//! widgets later changes what the callback does and nothing here.

use crate::chrome::{self, Along, HeaderStyle};
use crate::layout::{Layout, PanelId, Rect, Zone};
use crate::panel_drag::{Held, Outcome, PanelDrag, Preview};
use crate::panel_ui::Direction;
use crate::theme::{Color, Metrics, Theme};
use serde::{Deserialize, Serialize};

/// A panel the app can show.
///
/// A table rather than a match, so adding one is a row. The layout never sees this — it holds the
/// id and nothing else (§1c).
pub struct PanelKind {
    pub id: PanelId,
    pub name: &'static str,
    pub header: HeaderStyle,
    /// Which way this panel's controls run, until the artist says otherwise.
    ///
    /// **A default, not a rule.** It lives beside the name because it is the panel's own business
    /// which way it reads: a menu is a strip and a layer list is a list, and neither is a fact
    /// about the layout.
    pub direction: Direction,
    /// What this panel offers to be changed.
    ///
    /// **Empty is the common case and an honest answer.** Offering every panel the same list made
    /// the canvas advertise a choice between running its controls across or down, which it has
    /// none of; a settings menu full of settings that mean nothing teaches you not to open it.
    pub settings: &'static [Setting],
}

/// Something about a panel that can be changed.
///
/// A list per panel rather than one list for all of them, so a panel that gains a setting says so
/// itself and a panel with nothing to offer says that too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// Which way the controls run. Worth offering to a strip; meaningless to a list, and to the
    /// canvas, which has no controls at all.
    Direction,
    /// Lift this panel out of the arrangement, or put it back.
    ///
    /// One setting for both directions, because they are one question: a docked panel is offered
    /// somewhere to go, and a floating one is offered somewhere to return to.
    Floating,
}

/// Every panel, in the order they appear in a menu.
///
/// The canvas is in this list like anything else, which is the whole point: it can be tabbed,
/// split, moved and closed, and the layout has no idea it is special.
pub const PANELS: &[PanelKind] = &[
    PanelKind {
        id: PanelId(0),
        name: "Menu",
        header: HeaderStyle::Compact,
        direction: Direction::Auto,
        settings: &[Setting::Direction, Setting::Floating],
    },
    PanelKind {
        id: PanelId(1),
        name: "Tools",
        header: HeaderStyle::Compact,
        direction: Direction::Wrap,
        settings: &[Setting::Direction, Setting::Floating],
    },
    PanelKind {
        id: PanelId(2),
        name: "Canvas",
        header: HeaderStyle::Named,
        direction: Direction::Column,
        settings: &[],
    },
    PanelKind {
        id: PanelId(3),
        name: "Brush",
        header: HeaderStyle::Named,
        direction: Direction::Column,
        settings: &[Setting::Floating],
    },
    PanelKind {
        id: PanelId(4),
        name: "Layers",
        header: HeaderStyle::Named,
        direction: Direction::Column,
        settings: &[Setting::Floating],
    },
    PanelKind {
        id: PanelId(5),
        name: "Colour",
        header: HeaderStyle::Named,
        direction: Direction::Column,
        settings: &[Setting::Floating],
    },
    PanelKind {
        id: PanelId(6),
        name: "History",
        header: HeaderStyle::Named,
        direction: Direction::Column,
        settings: &[Setting::Floating],
    },
];

pub const MENU: PanelId = PanelId(0);
pub const TOOLS: PanelId = PanelId(1);
pub const CANVAS: PanelId = PanelId(2);
pub const BRUSH: PanelId = PanelId(3);
pub const LAYERS: PanelId = PanelId(4);
pub const COLOUR: PanelId = PanelId(5);
pub const HISTORY: PanelId = PanelId(6);

/// Whether a panel may live in a floating window.
///
/// Read off the same table that decides whether its settings offer *Float*, so the button and the
/// pick cannot disagree -- and so a panel added later is covered by the row somebody writes for it
/// rather than by a list here that they will not think to update (recurring hazard 11a.8).
///
/// The canvas is the one that says no today, and the reason is not a rule about canvases: it is
/// drawn by the GPU *underneath* egui, so a window's own background would be painted straight over
/// the artwork. A floating canvas is a window with nothing in it and a workspace with no drawing.
#[must_use]
fn may_float(id: PanelId) -> bool {
    kind(id).is_some_and(|k| k.settings.contains(&Setting::Floating))
}

#[must_use]
fn kind(id: PanelId) -> Option<&'static PanelKind> {
    PANELS.iter().find(|k| k.id == id)
}

/// The name a panel is saved under, and the panel a name refers to.
///
/// One table answering both directions, so a saved workspace cannot name something this build
/// would resolve differently. `PANELS` is the single source; adding a panel is still a row.
#[must_use]
fn name_for(id: PanelId) -> Option<String> {
    kind(id).map(|k| k.name.to_owned())
}

#[must_use]
fn id_for(name: &str) -> Option<PanelId> {
    PANELS.iter().find(|k| k.name == name).map(|k| k.id)
}

/// Where the saved workspace lives, beside the brush library and the theme.
fn layout_path() -> Option<std::path::PathBuf> {
    // **Never during tests.** A test that floats a panel would otherwise rewrite the workspace of
    // whoever is running it -- and, worse the other way round, a test that builds a "default"
    // workspace would read their saved one and quietly test their arrangement instead of the
    // built-in one. Both happened: a floating test passed against a fixture that was already
    // floating, because the state came off disk.
    if cfg!(test) {
        return None;
    }
    let dir = dirs::data_local_dir()?.join("OpenPaint");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("workspace.json"))
}

/// Read the saved arrangement, if there is one.
///
/// A missing file is the ordinary case. A broken one is reported and the default used, rather
/// than opening into some half-parsed workspace — the same tolerance the brush library and the
/// theme take with theirs.
type Loaded = (
    Layout,
    std::collections::HashMap<u32, PanelOptions>,
    Vec<Floating>,
);

fn load_layout() -> Option<Loaded> {
    let path = layout_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<SavedWorkspace>(&text) {
        Ok(saved) => {
            let layout = Layout::from_saved(&saved.layout, id_for);
            let options = options_from_saved(&saved.panels);
            let floating = floating_from_saved(&saved.floating);
            // A file that resolved to nothing at all -- every panel in it renamed away, say --
            // would open into an empty window with no way back but a shortcut. The default is the
            // better answer, and it is not silent.
            if layout.resolve(Rect::new(0.0, 0.0, 1.0, 1.0)).len() == 1
                && layout.find(CANVAS).is_none()
            {
                eprintln!(
                    "workspace: {} named nothing this build knows; using the default",
                    path.display()
                );
                return None;
            }
            Some((layout, options, floating))
        }
        Err(e) => {
            eprintln!(
                "workspace: {} could not be read ({e}); using the default",
                path.display()
            );
            None
        }
    }
}

/// The workspace: a layout, its history, the gesture in progress, and the look.
pub struct Workspace {
    pub layout: Layout,
    pub history: crate::layout::History<Arrangement>,
    pub theme: Theme,
    drag: PanelDrag,
    /// Where the canvas panel ended up last frame, so the renderer can put the canvas there.
    canvas_rect: Option<Rect>,
    /// The panel list, and where it is, while it is open.
    ///
    /// **It belongs to the workspace and not to any panel, and that is the whole point.** It used
    /// to live only in the Menu panel -- which is itself closable, so closing the Menu took away
    /// the only way to bring anything back, including the Menu. A route out of a state cannot be
    /// reachable only from inside that state.
    ///
    /// Reached with a secondary press anywhere, so it needs no panel to exist and no particular
    /// panel to be open. The Menu still offers it too, because a right-click is not discoverable
    /// on its own.
    popup: Option<Popup>,
    /// The popup's own half-finished gesture, exactly like any other described panel's.
    popup_input: crate::panel_draw::PanelInput,
    /// A popup has been asked for from somewhere with no pointer position; open it next frame.
    popup_wanted: Option<PopupKind>,
    /// What each panel has been set to, over and above the defaults in `PANELS`.
    ///
    /// Keyed by panel, which is per *instance* because a panel appears in the layout at most once
    /// -- moving one that is already open moves it rather than making a second. Saved with the
    /// arrangement, under the panel's name for the same reason the layout is (§ persistence).
    options: std::collections::HashMap<u32, PanelOptions>,
    /// Panels lifted out of the arrangement and left floating above it.
    ///
    /// **Above the arrangement, not outside the window.** A separate operating-system window needs
    /// a second surface for the GPU to draw into and a second path through the event loop; a
    /// floating rectangle needs neither and is what a floating palette was in every paint
    /// application before they became windows. Nothing here forecloses the window: a floating
    /// panel is already a panel with its own rectangle, which is what a window would give it.
    ///
    /// In front-to-back order, so the last one drawn is the one pressed.
    floating: Vec<Floating>,
    /// Which arrangement the gesture in flight belongs to.
    ///
    /// The drag machinery works in paths, and a path means nothing without knowing whose tree it
    /// indexes. Recorded on the press and read until the release.
    grab_surface: Surface,
    /// The next window's name.
    next_float: u32,
    /// Where inside a floating window the pointer took hold of it, so a dragged window does not
    /// jump its own corner up to the pointer.
    grip: (f32, f32),
    /// The arrangement as it stood when the gesture in flight began.
    ///
    /// Taken at the *press*, not the release: a divider moves live, so by the time it is let go
    /// the change has already happened and a snapshot then records the state after it. That is the
    /// same off-by-one that once made a resize impossible to undo.
    pending: Option<Arrangement>,
    /// The window as it was when a drag took hold of it, and the point that did.
    ///
    /// Together they are the grip: `grip` is the difference, kept because moving wants it directly.
    /// Every way of moving or sizing a window is worked out from these rather than from a distance
    /// added each frame, which is the only way a clamp cannot silently eat part of the gesture.
    grip_rect: Rect,
    grip_at: (f32, f32),
    /// A panel with no home, waiting for the artist to point at one.
    ///
    /// **Not a list of places.** A list is a second description of an arrangement that is already
    /// on the screen, in words, in a different order, and out of date the moment anything moves.
    /// The arrangement itself is the menu: the panel comes out, and wherever is pressed next is
    /// where it goes.
    placing: Option<Placing>,
    /// The window as it was last drawn.
    ///
    /// Kept because floating a panel has to put it *somewhere*, and the request can arrive from a
    /// menu that has no idea how big the window is. Written every frame; the default is only ever
    /// seen before the first one.
    screen: Rect,
}

/// The whole arrangement: what is docked, and every window floating above it.
///
/// **One value, because history that remembers half of it puts a panel in two places.** It used to
/// remember only the docked tree, so undoing a float put the panel back into the layout and left
/// it floating as well -- and merges and tear-offs were not recorded at all, because there was
/// nowhere to record them.
#[derive(Clone, Debug, PartialEq)]
pub struct Arrangement {
    docked: Layout,
    floating: Vec<Floating>,
}

/// A window floating above the arrangement, holding an arrangement of its own.
///
/// **A floating window is not a special kind of panel; it is a second workspace in a rectangle.**
/// It was a single panel with its own gesture code at first, and that was the fault behind every
/// complaint about it: no tab to press, no hold, no drop zones -- each of which the docked panels
/// already had, and none of which the copy inherited. Giving it a `Layout` means it gets chrome,
/// tabs, holds, drags and the five-zone drop from the same code, and nothing has to be taught
/// twice.
#[derive(Clone, Debug, PartialEq)]
pub struct Floating {
    /// Stable across the life of the window, because a position in a list is not.
    pub id: FloatId,
    pub rect: Rect,
    pub layout: Layout,
}

/// A floating window's name, for as long as it exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FloatId(pub u32);

/// Which arrangement a gesture is working in.
///
/// A path means nothing without knowing whose tree it indexes, and there is more than one tree the
/// moment anything floats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The arrangement filling the window.
    Docked,
    /// A floating window.
    Floating(FloatId),
}

/// A floating window on its way to disk. Its arrangement is saved exactly as the docked one is,
/// by panel name, for the same reason: an id is a position in a table, and a table changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SavedFloating {
    rect: Rect,
    #[serde(flatten)]
    layout: crate::layout::SavedLayout,
}

/// What a floating list is showing.
///
/// **One mechanism, not one per thing that floats.** The panel list came first; panel settings
/// want exactly the same object -- a described list, opened at a point, above everything, taking
/// the pointer while it is up -- and building a second one would have meant two sets of rules
/// about what closes it and what a press inside it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupKind {
    /// Every panel this build has, with a switch each.
    Panels,
    /// One panel's own settings.
    Settings(PanelId),
    /// The workspace's own settings: the look, rather than any one panel's arrangement.
    Workspace,
    /// A panel drawing something of its own: a menu's items, say.
    ///
    /// The workspace owns where it is, what closes it and what a press inside it means; the panel
    /// owns what is in it. Building a second floating list for menus would have meant two answers
    /// to all three.
    Panel(PanelId),
}

/// Where a panel is being drawn.
///
/// A panel draws itself into its slot in the workspace, and sometimes into a popup instead -- a
/// menu's items, a section's overflow. Passed rather than inferred so the panel can lay itself out
/// differently in each without the workspace knowing anything about menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// The panel's own slot in the arrangement.
    Panel,
    /// A popup floating above everything.
    Popup,
}

/// Which way a popup opens from the control it belongs to.
///
/// **Not `Side`**: `openpaint_core::Side` already means an edge of the canvas, and this file has
/// been bitten once already by giving a second meaning to a word the domain had spoken for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// Under the control, for a menu bar running across.
    Below,
    /// Beside it, for a menu bar running down.
    Right,
}

/// Where a popup goes when it belongs to a particular control.
///
/// **It flips rather than sliding.** Clamping a menu to the bottom of the window would slide it up
/// over the button that opened it, hiding the one thing that tells you which menu you are looking
/// at; opening upwards instead keeps the button visible and is what every menu bar does near the
/// foot of a screen. Sliding is only the last resort, for a popup taller than the window itself.
#[must_use]
fn anchored_rect(anchor: Rect, size: (f32, f32), side: Anchor, screen: Rect) -> Rect {
    let (w, h) = (size.0.min(screen.w), size.1.min(screen.h));
    let (x, y) = match side {
        Anchor::Below => {
            let below = anchor.y + anchor.h;
            let y = if below + h <= screen.y + screen.h || anchor.y - h < screen.y {
                below
            } else {
                anchor.y - h
            };
            (anchor.x, y)
        }
        Anchor::Right => {
            let right = anchor.x + anchor.w;
            let x = if right + w <= screen.x + screen.w || anchor.x - w < screen.x {
                right
            } else {
                anchor.x - w
            };
            (x, anchor.y)
        }
    };
    Rect::new(
        x.min(screen.x + screen.w - w).max(screen.x),
        y.min(screen.y + screen.h - h).max(screen.y),
        w,
        h,
    )
}

/// An open popup and where it is.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Popup {
    kind: PopupKind,
    rect: Rect,
}

/// What a panel has been set to, over and above its defaults.
///
/// Every field is an override, so `None` means "whatever the panel table says" rather than a
/// second copy of the default that would quietly stop tracking it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelOptions {
    /// Which way this panel's controls run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
}

impl Default for Workspace {
    fn default() -> Self {
        // The arrangement and the settings come out of the same file together, because a setting
        // belongs to a panel and the panels are the arrangement: loading one without the other
        // would open a workspace half of which was somebody's and half of which was the default's.
        let (layout, options, floating) =
            load_layout().unwrap_or_else(|| (default_layout(), <_>::default(), Vec::new()));
        Self {
            layout,
            history: crate::layout::History::default(),
            theme: load_theme().unwrap_or_default(),
            drag: PanelDrag::default(),
            canvas_rect: None,
            popup: None,
            popup_input: crate::panel_draw::PanelInput::default(),
            popup_wanted: None,
            options,
            floating,
            grab_surface: Surface::Docked,
            next_float: 0,
            grip: (0.0, 0.0),
            grip_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            grip_at: (0.0, 0.0),
            pending: None,
            placing: None,
            screen: Rect::new(0.0, 0.0, 1280.0, 800.0),
        }
    }
}

/// A panel out of the workspace and waiting to be put somewhere.
///
/// Carries the arrangement as it was, because the panel leaves its place the moment the pick
/// starts -- which is what makes it obvious that something is happening -- and thinking better of
/// it has to put the panel back *exactly*, not approximately. A remembered arrangement does that
/// without anybody working out what "back" meant.
#[derive(Clone, Debug)]
struct Placing {
    panel: PanelId,
    was: Arrangement,
}

/// Floating panels on their way to disk, keyed by name for the same reason everything else is.
///
/// Its own function because a test can see it and cannot see a file being written -- the same
/// split that made `saves` and `options_to_saved` testable, and the same gap that let a sabotage
/// of this mapping go unnoticed the first time.
#[must_use]
fn floating_to_saved(floating: &[Floating]) -> Vec<SavedFloating> {
    floating
        .iter()
        .map(|f| SavedFloating {
            rect: f.rect,
            layout: f.layout.to_saved(name_for),
        })
        .collect()
}

/// Floating panels on their way back, dropping any name this build does not know.
#[must_use]
fn floating_from_saved(saved: &[SavedFloating]) -> Vec<Floating> {
    saved
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let layout = Layout::from_saved(&f.layout, id_for);
            // A window whose every panel this build has forgotten is dropped rather than reopened
            // empty: an empty window is a rectangle of chrome with no way to tell it is not broken.
            (!layout.panels().is_empty()).then(|| Floating {
                id: FloatId(u32::try_from(i).unwrap_or(0)),
                rect: f.rect,
                layout,
            })
        })
        .collect()
}

/// Panel settings on their way to disk: keyed by name, and only where something was actually set.
///
/// Its own function because a test can see it and cannot see a file being written --- the same
/// split that made `saves` testable. A default is not written at all: a file full of "whatever the
/// panel table says" would freeze today's defaults into every saved workspace.
#[must_use]
fn options_to_saved(
    options: &std::collections::HashMap<u32, PanelOptions>,
) -> std::collections::BTreeMap<String, PanelOptions> {
    options
        .iter()
        .filter(|(_, o)| **o != PanelOptions::default())
        .filter_map(|(id, o)| name_for(PanelId(*id)).map(|n| (n, *o)))
        .collect()
}

/// Panel settings on their way back, dropping any name this build does not know.
#[must_use]
fn options_from_saved(
    saved: &std::collections::BTreeMap<String, PanelOptions>,
) -> std::collections::HashMap<u32, PanelOptions> {
    saved
        .iter()
        .filter_map(|(name, o)| id_for(name).map(|id| (id.0, *o)))
        .collect()
}

/// A workspace as it goes to disk: the arrangement, and what each panel has been set to.
///
/// Settings are keyed by panel **name** for the same reason the arrangement is: an id is whatever
/// row a panel happens to occupy in the table, so a file full of them would silently hand one
/// panel's settings to another the day a panel is added. A name this build does not know is
/// dropped, because a workspace missing one setting beats a file that cannot be opened.
#[derive(Debug, Serialize, Deserialize)]
struct SavedWorkspace {
    #[serde(flatten)]
    layout: crate::layout::SavedLayout,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    panels: std::collections::BTreeMap<String, PanelOptions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    floating: Vec<SavedFloating>,
}

/// Where a hand-edited theme lives, beside the brush library.
fn theme_path() -> Option<std::path::PathBuf> {
    // Not during tests, for the same reason as `layout_path`: choosing an icon set in a test must
    // not change the look of the application on the machine running it.
    if cfg!(test) {
        return None;
    }
    let dir = dirs::data_local_dir()?.join("OpenPaint");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("theme.json"))
}

/// Read `theme.json`, if there is one.
///
/// A missing file is the ordinary case and not an error. A *broken* one is reported and the
/// default used, rather than the app starting up in some half-parsed colour scheme — the same
/// tolerance the brush library takes with its own file.
fn load_theme() -> Option<Theme> {
    let path = theme_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    match Theme::from_json(&text) {
        Ok(theme) => Some(theme),
        Err(e) => {
            eprintln!(
                "theme: {} could not be read ({e}); using the default",
                path.display()
            );
            None
        }
    }
}

/// How much room a strip's *contents* need: one row and the padding around it.
///
/// A compact header sits on a wide strip's *side*, so it costs width rather than height and does
/// not appear here. Derived rather than written down, so changing the metrics moves the bar rather
/// than quietly clipping it.
#[must_use]
fn strip_content_min(m: &Metrics) -> f32 {
    m.row + m.padding * 2.0
}

/// How tall the *slot* holding a strip has to be.
///
/// The gutter is taken out of the slot by `chrome` before the panel sees it, so a slot exactly one
/// row tall delivers a panel two units short. Two numbers rather than one, because they are two
/// different things and giving them one name is how a constant quietly goes missing (§11a.8).
#[must_use]
fn strip_min(m: &Metrics) -> f32 {
    strip_content_min(m) + m.gutter
}

/// How wide the tool rail has to be: its widest button and the padding either side.
///
/// The header is on top for a tall panel, so it costs height rather than width. The button width
/// is the one thing here that cannot be derived without a font, so it is a stated allowance --
/// generous, because the cost of being wrong is a clipped label and the cost of being generous is
/// a few units of rail.
#[must_use]
fn rail_content_min(m: &Metrics) -> f32 {
    const WIDEST_TOOL_LABEL: f32 = 52.0;
    WIDEST_TOOL_LABEL + m.padding * 4.0
}

/// How wide the *slot* holding the tool rail has to be, gutter included.
#[must_use]
fn rail_min(m: &Metrics) -> f32 {
    // The handle too. The rail's controls run across, so its handle is the bar down the right and
    // costs width rather than height -- which the menu strip's minimum, being a height, does not
    // have to pay for. Generous if the artist later sets the rail to run its controls down and the
    // handle moves back to the top; a few spare units of rail is a much smaller problem than a
    // clipped button.
    rail_content_min(m) + m.gutter + m.header_compact
}

/// The arrangement a first-time artist finds.
///
/// **Only a file's worth of decisions**, which is what §1c bought: this is a default, not a
/// structure. Menu across the top, tools down the left, canvas taking what is left, and the
/// panels people reach for most stacked on the right.
#[must_use]
pub fn default_layout() -> Layout {
    let m = Theme::default().metrics;
    let mut l = Layout::single(CANVAS);

    // A menu strip across the top. The weights matter as much as the structure: an `insert` halves
    // whatever it lands on, so without these the menu bar would take half the window — which a
    // test caught and no amount of reading the code would have.
    l.insert(&[], Zone::Top, MENU);
    // **A menu bar is a fixed height, not a fraction of a window.** Given a weight alone it grew
    // with the window and, at 900x600, clipped its own contents in half. The minimum is taken out
    // before the weights are applied, so the bar stays the size it needs and the rest of the
    // window is shared out below it. Worked out from the metrics rather than guessed; the test
    // below re-derives it and fails if either drifts.
    l.set_weight(&[0], 0.0);
    l.set_min(&[0], strip_min(&m));
    l.set_weight(&[1], 1.0);

    // Tools down the left, canvas taking what is left, panels down the right.
    l.insert(&[1], Zone::Left, TOOLS);
    l.insert(&[1, 1], Zone::Right, LAYERS);
    l.set_weight(&[1, 0], 0.0);
    // The rail is a strip on its side: it needs the width of its widest button, not a share of
    // the window that happens to be about right on one screen.
    l.set_min(&[1, 0], rail_min(&m));
    l.set_weight(&[1, 1], 0.80);
    l.set_weight(&[1, 2], 0.20);

    // The right column: layers over brush over colour, with history sharing the layers' slot
    // because the two are rarely wanted at once.
    l.insert(&[1, 2], Zone::Bottom, BRUSH);
    l.insert(&[1, 2, 1], Zone::Bottom, COLOUR);
    l.insert(&[1, 2, 0], Zone::Center, HISTORY);
    l.set_weight(&[1, 2, 0], 0.42);
    l.set_weight(&[1, 2, 1], 0.30);
    l.set_weight(&[1, 2, 2], 0.28);
    l
}

/// Which divider or window edge a frame's marks belong to.
#[derive(Clone, Debug, PartialEq)]
enum Seam {
    Divider {
        path: crate::layout::Path,
        index: usize,
    },
    Edge(crate::chrome::Pull),
}

/// Which seam the marks are for this frame, and whether it has been taken.
///
/// **Two states and no third.** It is *available* while the pointer is near it and *taken* from the
/// instant it is pressed until the instant it is let go -- and nothing in between, because there is
/// nothing to wait for: a divider and a window's edge are both grabbed the moment they are touched.
///
/// The version that answered these four cases separately gave one control three appearances. The
/// press put it into the hold animation, which starts at no alpha at all, so it went blue on hover,
/// **blank** for the first frames of the press, and blue again once it moved. Reported exactly that
/// way: *"if I hold, it shows blue, for a flick, then nothing. then if drag, back to blue."*
///
/// **`held`, not the frame's preview.** A preview says what to draw because something changed, and
/// a divider held perfectly still changes nothing -- so once its hold has run it reports nothing,
/// and the mark went out again. Holding still for a third of a second is the one thing that should
/// not change anything. What the pointer *has hold of* is true every frame until it lets go.
///
/// A window's edge is included for the same reason its resize is: it is a divider with the screen
/// on its far side, and it should light up under a hovering pen like one.
#[must_use]
fn seam_in_use(
    held: Option<&Held>,
    seams: &[crate::layout::Splitter],
    border: Option<Rect>,
    at: Option<(f32, f32)>,
    m: &Metrics,
) -> Option<(Seam, bool)> {
    match held {
        Some(Held::Divider { path, index }) => {
            return Some((
                Seam::Divider {
                    path: path.clone(),
                    index: *index,
                },
                true,
            ));
        }
        Some(Held::Edge { pull }) => return Some((Seam::Edge(*pull), true)),
        // Some other gesture owns the pointer, and a hint about something it is not doing would be
        // a second thing to read.
        Some(_) => return None,
        None => {}
    }
    // Available: near enough to catch, with nothing else going on. A hint for a pen, which hovers;
    // a finger gets nothing here and loses nothing.
    let (x, y) = at?;
    // The border first, for the same reason the hit test takes it first among these two: a window
    // edge and a divider inside that window can be within reach of each other at the window's rim.
    if let Some(pull) =
        border.and_then(|r| chrome::edge_at(r, m.splitter_grab, least_window(m), x, y))
    {
        return Some((Seam::Edge(pull), false));
    }
    seams.iter().find(|s| s.rect.contains(x, y)).map(|s| {
        (
            Seam::Divider {
                path: s.path.clone(),
                index: s.index,
            },
            false,
        )
    })
}

/// What is drawn over what.
///
/// **Named once, here, and every tier distinct.** egui paints its layers in `Order` sequence, but
/// *within* one order the layers that are not `Area`s come out in whatever order their map happens
/// to iterate. Docked panel bodies and floating windows both sat on `Order::Middle`, so a docked
/// panel sometimes came out in front of a window floating above it -- and the canvas's own
/// overlays, drawn on `Order::Foreground`, came out in front of every window every time.
///
/// So each tier gets an order to itself. There are exactly six of them and exactly six things to
/// stack, bottom to top:
mod stack {
    /// The workspace's own chrome: the ground it shows through, headers, tabs, dividers.
    pub const GROUND: egui::Order = egui::Order::Background;
    /// What a docked panel draws inside itself.
    pub const PANELS: egui::Order = egui::Order::PanelResizeLine;
    /// What the canvas draws over the artwork: a selection outline, a crop box, the brush ring.
    /// Above the panels because the canvas is one of them; below the windows because a window
    /// floating over the artwork hides what is on it.
    pub const ARTWORK: egui::Order = egui::Order::Middle;
    /// Floating windows, chrome and contents. **Always in front of the arrangement.**
    pub const WINDOWS: egui::Order = egui::Order::Foreground;
    /// What a gesture in flight draws: the wait, the armed divider, the five zones, the ghost.
    pub const MARKS: egui::Order = egui::Order::Tooltip;
    /// The popup, which is the topmost thing the workspace has.
    pub const POPUP: egui::Order = egui::Order::Debug;
}

/// The order the canvas's own overlays are drawn in: above the panels, below the windows.
///
/// A function because the shell draws them and the stack is described here -- one place decides
/// what is over what, and a caller cannot pick an order that happens to be wrong.
#[must_use]
pub fn artwork_order() -> egui::Order {
    stack::ARTWORK
}

impl Workspace {
    /// Where the canvas panel is, for the renderer to draw into.
    #[must_use]
    pub fn canvas_rect(&self) -> Option<Rect> {
        self.canvas_rect
    }

    /// Whether the workspace's own chrome claims this point rather than the canvas.
    ///
    /// **The one answer, asked of the workspace itself.** It used to be two values -- the canvas
    /// rectangle and the chrome drawn over it -- cached beside the shell, and a caller could take
    /// the first and forget the second. One did: floating windows and popups are painted *on top*
    /// of the canvas, so by the rectangle alone the pen and the wheel were told they were over the
    /// artwork while they were plainly on a panel. The reported symptom was scrolling a floating
    /// panel zooming the drawing behind it.
    ///
    /// **No canvas panel means no canvas anywhere.** The artist can close it like any other, and
    /// the workspace then fills the window with ground -- the artwork is not on screen at all, so
    /// nothing may be painted on it.
    #[must_use]
    pub fn takes_point(&self, x: f32, y: f32) -> bool {
        self.canvas_rect.is_none_or(|c| !c.contains(x, y))
            || self.over_canvas().iter().any(|r| r.contains(x, y))
    }

    /// The workspace's own chrome that is drawn **over** the canvas rather than beside it.
    ///
    /// Everything else in the workspace is a leaf of the arrangement, and the canvas is another
    /// leaf beside them: "not inside the canvas rectangle" is the whole answer for those. Floating
    /// windows and the popup are the exception -- they are painted on top -- and asking only about
    /// the canvas rectangle said the pointer was over the canvas while it sat on a floating panel.
    /// The reported symptom was scrolling a floating panel zooming the drawing behind it.
    ///
    /// Front to back, because that is the order they are drawn and the order a press picks them.
    #[must_use]
    fn over_canvas(&self) -> Vec<Rect> {
        // Grown by the resize border's outer half, because that half is the window's to answer for
        // -- a press there sizes the window, and without this the wheel in the same place zoomed
        // the drawing behind it. Two answers to "where is this window" is one too many.
        let reach = self.theme.metrics.splitter_grab / 2.0;
        self.popup
            .map(|q| q.rect)
            .into_iter()
            .chain(self.floating.iter().rev().map(|f| grown(f.rect, reach)))
            .collect()
    }

    /// Whether a panel gesture owns the pointer, so the canvas must not also act on it.
    ///
    /// **Armed, not merely active.** A press that is only waiting out the hold has taken nothing
    /// yet, and treating it as busy would make the first third of a second of every press on
    /// chrome feel dead. Once it arms, the pointer is spoken for until release.
    #[must_use]
    pub fn busy(&self) -> bool {
        // The open panel list counts: it floats over whatever is beneath, including the canvas,
        // and a press meant for it must not also start a stroke.
        // A pick counts too: the whole point of it is that the next press lands a panel, so that
        // press must not also start a stroke.
        self.drag.active() || self.popup.is_some() || self.placing.is_some()
    }

    /// Abandon a panel drag without applying it.
    ///
    /// Escape, and the same promise a transform makes (§5e): a gesture you have thought better of
    /// costs nothing, because nothing has happened to the layout until you let go.
    pub fn cancel_drag(&mut self) -> bool {
        let anything = self.drag.active() || self.popup.is_some() || self.placing.is_some();
        let moved = self.drag.moved();
        self.drag.cancel(&mut self.layout);
        // **The whole arrangement back, not only the docked tree.** A gesture can have moved a
        // window or pulled its edges, and neither of those lives in the layout the drag module was
        // handed -- so Escape used to leave a window wherever the pointer had dragged it, which is
        // not what "nothing has happened" means to anyone.
        if let Some(was) = self.pending.take() {
            if moved {
                self.restore(was);
            }
        }
        self.popup = None;
        self.cancel_placing();
        anything
    }

    /// Switch between the built-in looks, and write the result out to be edited.
    ///
    /// **This is §1b's third goal, made touchable.** The two themes differ in nine colours and
    /// nothing else — same layout, same widgets, same everything — which is why they were never
    /// two designs. Writing the file out on the way is what turns "the look is data" from a claim
    /// into something you can open in an editor and change.
    ///
    /// Returns what to tell the artist, since a theme that could not be written is worth knowing
    /// about rather than silently not happening (§6b).
    pub fn cycle_theme(&mut self) -> String {
        let warm = self.theme.palette == Theme::paper().palette;
        self.theme = if warm {
            Theme::default()
        } else {
            Theme::paper()
        };
        let name = if warm { "Studio" } else { "Paper" };

        let Some(path) = theme_path() else {
            return format!("{name}. No writable data directory, so it was not saved.");
        };
        match self
            .theme
            .to_json()
            .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()))
        {
            Ok(()) => format!("{name}. Written to {} to edit.", path.display()),
            Err(e) => format!("{name}, but it could not be saved: {e}"),
        }
    }

    /// Write the arrangement out, so it is still there next time.
    ///
    /// Called when a change *settles* — a gesture ending, a panel opened or closed — rather than
    /// on every frame of a divider drag, which would write the file sixty times a second to no
    /// purpose.
    ///
    /// Failure is reported and otherwise ignored: losing an arrangement is a nuisance, and
    /// refusing to let go of a panel because a file could not be written would be worse.
    fn remember(&self) {
        let Some(path) = layout_path() else {
            return;
        };
        let saved = SavedWorkspace {
            layout: self.layout.to_saved(name_for),
            panels: options_to_saved(&self.options),
            floating: floating_to_saved(&self.floating),
        };
        match serde_json::to_string_pretty(&saved)
            .map_err(|e| e.to_string())
            .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()))
        {
            Ok(()) => {}
            Err(e) => eprintln!("workspace: could not save the layout ({e})"),
        }
    }

    /// Whether a panel is currently somewhere in the workspace.
    ///
    /// **Anywhere**, floating included. Asking only the arrangement said a floated panel was shut,
    /// so its switch in the panel list showed off, and turning it "on" inserted a *second* copy
    /// while the floating one carried on existing.
    #[must_use]
    pub fn is_open(&self, panel: PanelId) -> bool {
        self.is_docked(panel) || self.is_floating(panel)
    }

    /// Take a panel out of the workspace, wherever it is.
    ///
    /// One function, so the panel list's switch and the "Remove this panel" in a panel's own
    /// settings are the same act rather than two that agree today. Both trees are asked, because
    /// a panel is docked *or* floating and the caller should not have to know which.
    pub fn hide(&mut self, panel: PanelId) {
        if !self.is_open(panel) {
            return;
        }
        self.remember_for_undo();
        self.layout.remove(panel);
        self.take_from_floating(panel);
        self.remember();
    }

    /// Show a panel if it is hidden, or put it away if it is showing.
    ///
    /// The only way to close one, and therefore the thing that makes "everything closed" a state
    /// an artist can reach and come back from rather than a theoretical one. Undoable like any
    /// other layout change.
    ///
    /// Showing one does not put it anywhere: it starts a pick, and the artist says where. A panel
    /// that appeared in the first leaf of the tree appeared somewhere nobody was looking, behind
    /// whatever tab was already there.
    pub fn toggle(&mut self, panel: PanelId) {
        if self.is_open(panel) {
            self.hide(panel);
        } else {
            self.start_placing(panel);
        }
    }

    /// Undo the last layout change. Returns whether there was one.
    pub fn undo(&mut self) -> bool {
        // **Before the snapshot, not after.** `restore` ends the pick too, but by then `now` has
        // been sampled -- and during a pick `now` is the arrangement with the picked panel taken
        // out, a state the artist never committed. Pushing that onto the redo stack meant redoing
        // straight back into it, with the panel in neither tree and no pick to put it anywhere.
        self.cancel_placing();
        let now = self.snapshot();
        match self.history.undo(&now) {
            Some(previous) => {
                self.restore(previous);
                self.remember();
                true
            }
            None => false,
        }
    }

    /// Redo it.
    pub fn redo(&mut self) -> bool {
        // See `undo`: the same trap, with the stacks the other way round.
        self.cancel_placing();
        let now = self.snapshot();
        match self.history.redo(&now) {
            Some(next) => {
                self.restore(next);
                self.remember();
                true
            }
            None => false,
        }
    }

    /// Put every panel back where it started.
    ///
    /// The general safety net §1c promised instead of forbidding a movable menu bar: whatever you
    /// have done to the workspace, including closing the menu itself, this is the way back. Goes
    /// on the undo stack like any other change, so it is not itself a one-way door.
    pub fn reset(&mut self) {
        self.remember_for_undo();
        self.layout = default_layout();
        self.remember();
    }

    /// Show a panel, bringing it forward if it is already open.
    ///
    /// **Not the way a panel is switched on** -- that starts a pick, and the artist says where it
    /// goes. This is the last resort behind two recovery paths: a merge whose destination window
    /// vanished, and a drop outside everything. Both are mid-gesture, with nobody to ask and a
    /// panel already in the air, and there somewhere visible beats a question. Losing it silently
    /// would be the worst possible answer (DECISIONS 6b).
    pub fn open(&mut self, panel: PanelId) {
        if let Some((path, tab)) = self.layout.find(panel) {
            self.layout.set_active(&path, tab);
            return;
        }
        self.remember_for_undo();
        // Into whichever leaf is showing, as a tab. Somewhere visible beats somewhere clever.
        let path = self
            .layout
            .resolve(Rect::new(0.0, 0.0, 1.0, 1.0))
            .first()
            .map(|p| p.path.clone())
            .unwrap_or_default();
        self.layout.insert(&path, Zone::Center, panel);
    }

    /// Draw the workspace and act on the pointer.
    ///
    /// `contents` is called once per visible panel with an `egui::Ui` sized to its content
    /// rectangle. The canvas panel is skipped: its pixels come from the GPU, and all this does is
    /// remember where to put them.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        screen: Rect,
        mut contents: impl FnMut(PanelId, &mut egui::Ui, Direction, Place),
    ) {
        let painter = ctx.layer_painter(egui::LayerId::new(
            stack::GROUND,
            egui::Id::new("workspace"),
        ));
        let m = self.theme.metrics;
        let p = self.theme.palette;
        self.set_screen(screen);

        let placed = self.layout.resolve(screen);

        // --- input, before drawing, so the drop overlay reflects this frame's pointer ---
        let (pointer, pressed, released, down, now_ms) = ctx.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.primary_down(),
                i.time * 1000.0,
            )
        });

        // Painting is demand-driven, so a hold with the pointer still would never fire: no
        // events, no frames, and the timer never read. Reported as the hold "sometimes" not
        // working; it was whenever the pen was steady enough to stop producing motion.
        if let Some(left) = self.drag.waiting_ms(now_ms) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                (left / 1000.0).max(0.008),
            ));
        }

        // The panel list: opened by a secondary press anywhere, closed by a primary press outside
        // it. Anywhere, because it must not depend on any panel being open -- least of all the one
        // it used to live in.
        // Not while a pick is waiting: the press that answers it would both land the panel and
        // open a list over the place it landed.
        if let Some(kind) = self.popup_wanted.take().filter(|_| self.placing.is_none()) {
            // Centred, because a request with no pointer behind it has nowhere better to go.
            let mid = self.popup_rect(ctx, kind, screen.x, screen.y, screen);
            self.open_popup_at(
                ctx,
                kind,
                (screen.w - mid.w) / 2.0,
                (screen.h - mid.h) / 2.0,
                screen,
            );
        }
        // A secondary press asks whatever is under the pointer what it offers: a panel's header
        // offers that panel's settings, anything else offers the workspace's own list of panels.
        // One rule, and the answer depends on what you pressed.
        if ctx.input(|i| i.pointer.secondary_pressed()) {
            // While a pick is waiting, the other button means "no". Opening the panel list on top
            // of the question would be a second question over the first.
            if self.placing.is_some() {
                self.cancel_placing();
            } else if let Some(pos) = pointer {
                let kind = header_under(
                    &placed,
                    &m,
                    |pl| self.direction_of(showing_of(pl)),
                    |pl, i| measure(ctx, m.label, pl.tabs.get(i).copied()),
                    pos.x,
                    pos.y,
                )
                .map_or(PopupKind::Panels, PopupKind::Settings);
                self.open_popup_at(ctx, kind, pos.x, pos.y, screen);
            }
        }
        let on_popup = popup_press(
            self.popup.map(|q| q.rect),
            pressed,
            pointer.map(|q| (q.x, q.y)),
        );
        if on_popup == PopupPress::Close {
            self.popup = None;
        }

        // **Read before the press is acted on.** `input_frame` answers a waiting pick, so by the
        // time the panels below are drawn `self.placing` is already `None` -- and the one frame on
        // which the panels must not answer the pointer is exactly that one.
        let was_picking = self.placing.is_some();
        // Everything a press means, in one place: `show` draws, and this decides. Tests drive
        // the *same* entry point rather than a hand-made idea of what the pointer is over -- which
        // is exactly the disagreement that let a real bug pass a green suite.
        let preview = self.input_frame(
            screen,
            Pointer {
                at: pointer.map(|q| (q.x, q.y)),
                pressed,
                released,
                down,
                now_ms,
            },
            on_popup,
            |panel| measure(ctx, m.label, panel),
        );
        // Recomputed from the surface the frame settled on, for the marks below. Cheap, and it
        // cannot disagree with what the gesture used: both read `self.grab_surface`.
        let working = self.grab_surface;
        let (here, seams) = self
            .surface(working)
            .map(|(l, area)| (l.resolve(area), l.splitters(area, m.splitter_grab)))
            .unwrap_or_default();

        // --- draw, from the layout as it now stands ---
        let placed = self.layout.resolve(screen);

        // Where the canvas panel is, worked out *before* anything is painted.
        //
        // **The ground cannot simply be filled over the whole window.** The canvas is drawn by the
        // GPU underneath egui, so a full-screen fill paints over the artwork — which is exactly
        // what happened: the workspace came up with no canvas at all, not even a white page. So
        // the ground is filled as the four rectangles *around* the canvas instead, and the canvas
        // panel's own content is never painted.
        self.canvas_rect = placed.iter().find_map(|slot| {
            (slot.tabs.get(slot.active) == Some(&CANVAS)).then(|| {
                chrome::panel(
                    slot,
                    &m,
                    style_of(slot),
                    along_of(slot, self.direction_of(showing_of(slot))),
                    |i| measure(ctx, m.label, slot.tabs.get(i).copied()),
                )
                .content
            })
        });
        match self.canvas_rect {
            None => {
                painter.rect_filled(to_egui(screen), 0.0_f32, rgb(p.ground));
            }
            Some(c) => {
                for band in [
                    Rect::new(screen.x, screen.y, screen.w, c.y - screen.y),
                    Rect::new(
                        screen.x,
                        c.y + c.h,
                        screen.w,
                        (screen.y + screen.h) - (c.y + c.h),
                    ),
                    Rect::new(screen.x, c.y, c.x - screen.x, c.h),
                    Rect::new(c.x + c.w, c.y, (screen.x + screen.w) - (c.x + c.w), c.h),
                ] {
                    if band.w > 0.0 && band.h > 0.0 {
                        painter.rect_filled(to_egui(band), 0.0_f32, rgb(p.ground));
                    }
                }
            }
        }
        for slot in &placed {
            let Some(&showing) = slot.tabs.get(slot.active) else {
                continue;
            };
            let style = style_of(slot);
            let c = chrome::panel(
                slot,
                &m,
                style,
                along_of(slot, self.direction_of(showing_of(slot))),
                |i| measure(ctx, m.label, slot.tabs.get(i).copied()),
            );

            // The header always; the body only for panels that draw something in it. The canvas
            // panel's body is the artwork underneath.
            if showing != CANVAS {
                painter.rect_filled(to_egui(c.outer), m.radius, rgb(p.panel));
            }
            painter.rect_filled(to_egui(c.header), m.radius, rgb(p.header));

            match style {
                HeaderStyle::Compact => {
                    // A short bar, centred: no name, but unmistakably something to take hold of.
                    let bar = egui::Rect::from_center_size(
                        to_egui(c.header).center(),
                        egui::vec2(18.0, 2.0),
                    );
                    painter.rect_filled(bar, 1.0_f32, rgb(p.edge));
                }
                HeaderStyle::Named => {
                    for t in &c.tabs {
                        // The same quiet mark the dividers use, for the same reason: a pen hovers,
                        // so it can say "this is a thing" before you commit to it. A finger gets
                        // nothing here and loses nothing, because the hold is what starts a
                        // gesture either way.
                        let hovered =
                            pointer.is_some_and(|q| t.rect.contains(q.x, q.y)) && preview.is_none();
                        draw_tab(&painter, t, slot, &m, &p, hovered);
                    }
                }
            }

            if showing == CANVAS {
                // Nothing painted over the content: the artwork is already there, drawn by the
                // GPU before egui ran. Painting anything here — even the canvas colour — hides it.
                continue;
            }

            // The rectangle `chrome` says controls go in, already padded. Padding was once
            // applied here as well as in the renderer, and a menu strip ended up mostly made of
            // nothing; no amount of adjusting the padding would have fixed it, because the padding
            // was not the problem (recurring hazard 11a.8).
            let content = to_egui(c.controls.rect());
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::LayerId::new(stack::PANELS, egui::Id::new(("panel", showing.0))),
                egui::Id::new(("panel-ui", showing.0)),
                egui::UiBuilder::new().max_rect(content),
            );
            // Contents are clipped to their own panel, so a list too long for its slot cannot
            // draw over the panel beside it.
            ui.set_clip_rect(content);
            // **Nothing in a panel answers while a pick is waiting**, and it visibly says so: the
            // press that lands the panel goes wherever it is pointed, and a control under that
            // point would otherwise be worked at the same time.
            //
            // Two other things happen to stop that press as well -- the landing rearranges the leaf
            // the control was in, and the drop overlay's layer occludes what is under it -- so no
            // test can tell this line apart from those. It stays because it is the *stated* rule
            // and the only one of the three that is; the other two are accidents of ordering and of
            // how egui picks a layer. It also greys the workspace, which is how the artist can see
            // that a question is being asked.
            if was_picking {
                ui.disable();
            }
            // The direction is worked out here and handed over, because the panel that wants it
            // runs inside a borrow of the workspace that holds the setting.
            contents(showing, &mut ui, self.direction_of(showing), Place::Panel);
        }

        // --- the wait, drawn on the thing being held ---
        //
        // The whole gesture now begins with a hold, so it *must* be visible: a hold with no
        // feedback is indistinguishable from a dead control, which is how this felt before. The
        // mark grows with the wait, so the artist can see it coming rather than guessing.
        // **On the layer the gesture's own arrangement is drawn on.** A hold inside a floating
        // window marked on the background layer is painted over by the window itself, which looks
        // exactly like a hold that never ran.
        let marks = if working == Surface::Docked {
            painter.clone()
        } else {
            ctx.layer_painter(egui::LayerId::new(
                stack::MARKS,
                egui::Id::new("floating-marks"),
            ))
        };
        if let Some(Preview::Waiting { progress, on }) = &preview {
            let alpha = (140.0 * progress).clamp(0.0, 140.0) as u8;
            let tint = egui::Color32::from_rgba_unmultiplied(
                p.state.0[0],
                p.state.0[1],
                p.state.0[2],
                alpha,
            );
            match on {
                Held::Panel { path, tab } => {
                    // Against the arrangement the gesture is *in*, not the docked one. A path
                    // indexes one tree, and a floating window's path found nothing here -- so the
                    // hold on a floating panel showed no sign of running at all, which is
                    // indistinguishable from a hold that does not work.
                    if let Some(slot) = here.iter().find(|s| &s.path == path) {
                        let c = chrome::panel(
                            slot,
                            &m,
                            style_of(slot),
                            along_of(slot, self.direction_of(showing_of(slot))),
                            |i| measure(ctx, m.label, slot.tabs.get(i).copied()),
                        );
                        marks.rect_filled(to_egui(hold_mark(&c, *tab)), m.radius, tint);
                    }
                }
                // The whole window, so a hold on its body is as visible as a hold on a tab.
                Held::Frame => {
                    if let Some(at) = self.area_of(working) {
                        marks.rect_filled(to_egui(at), m.radius, tint);
                    }
                }
                // Drawn below, with every other state a seam can be in: a divider has nothing to
                // wait for, so it has nothing to animate.
                Held::Divider { .. } | Held::Edge { .. } => {}
            }
        }

        // --- the divider or window edge under the pointer, or the one in use ---
        //
        // Drawn last of the chrome so it sits over the panels either side. Two appearances and no
        // third: `p.edge` while it is merely available, `p.state` from the instant it is taken.
        // See `seam_in_use` for the third one this used to have.
        if let Some((seam, taken)) = self.seam_mark(&seams, pointer.map(|q| (q.x, q.y))) {
            // At the *drawn* thickness rather than the grab width: the point is to show where it
            // is, not how big the target is.
            let t = m.splitter_hover;
            let colour = rgb(if taken { p.state } else { p.edge });
            match seam {
                Seam::Divider { path, index } => {
                    if let Some(s) = seams.iter().find(|s| s.path == path && s.index == index) {
                        marks.rect_filled(to_egui(centred(s.rect, s.axis, t)), t / 2.0, colour);
                    }
                }
                Seam::Edge(pull) => {
                    if let Some(at) = self.rect_of(working) {
                        for band in chrome::edge_bands(at, pull, t) {
                            marks.rect_filled(to_egui(band), t / 2.0, colour);
                        }
                    }
                }
            }
        }

        // --- the drop overlay, on top of everything ---
        //
        // **A pick draws the same overlay as a drag**, because it is the same question: a panel is
        // in the air, and this is where it would land. A second overlay for picking would be a
        // second set of rules about what the five zones mean.
        let picking = self.placing().map(|panel| {
            let landing = pointer
                .and_then(|q| self.landing_at(q.x, q.y))
                .map(|(_, _, rect, zone)| crate::panel_drag::Landing { rect, zone });
            (panel, landing)
        });
        if let Some((panel, over)) = picking {
            draw_landing(ctx, &m, &p, over, pointer, panel);
        }
        if let Some(Preview::Carrying { panel, over }) = preview {
            // **A floating window shows its zones on whatever it is over, not on itself.** The
            // window is following the pointer, so its own leaf is always under it; drawing that
            // would offer to drop it into the thing being dragged.
            let over = if working == Surface::Docked {
                // **Not under a floating window.** A docked panel let go there lands nowhere --
                // the two modes never mix by dragging -- so lighting up a zone would promise
                // something the release does not do, and an overlay you cannot believe is worse
                // than none. The ring the window's resize border adds counts as the window.
                over.filter(|_| {
                    pointer.is_none_or(|q| {
                        self.window_within(m.splitter_grab / 2.0, q.x, q.y)
                            .is_none()
                    })
                })
            } else {
                pointer
                    .and_then(|q| self.window_at(q.x, q.y))
                    .filter(|id| Surface::Floating(*id) != working)
                    .and_then(|id| {
                        let area = self.area_of(Surface::Floating(id))?;
                        let layout = self.layout_of(Surface::Floating(id))?;
                        let at = pointer?;
                        let leaf = layout.leaf_at(area, at.x, at.y)?;
                        Some(crate::panel_drag::Landing {
                            rect: leaf.rect,
                            zone: Layout::zone_at(leaf.rect, at.x, at.y),
                        })
                    })
            };
            // The panel that has come loose, marked at its source. Without this the hold arms
            // invisibly and the only way to know it worked is to move and find out -- which makes
            // a learnable gesture into one you have to discover. One fill; iPadOS does the same
            // thing by lifting the tile before you move it.
            if let Some((path, _)) = self.layout.find(panel) {
                if let Some(slot) = placed.iter().find(|s| s.path == path) {
                    let c = chrome::panel(
                        slot,
                        &m,
                        style_of(slot),
                        along_of(slot, self.direction_of(showing_of(slot))),
                        |i| measure(ctx, m.label, slot.tabs.get(i).copied()),
                    );
                    painter.rect_filled(
                        to_egui(c.header),
                        m.radius,
                        egui::Color32::from_rgba_unmultiplied(
                            p.state.0[0],
                            p.state.0[1],
                            p.state.0[2],
                            110,
                        ),
                    );
                }
            }
            draw_landing(ctx, &m, &p, over, pointer, panel);
        }

        // --- floating windows, above the arrangement and below the popup ---
        //
        // Drawn by the same code as the arrangement itself, from the same `chrome` and the same
        // descriptor layer. That is the whole point of a window holding a `Layout`: a floating
        // panel has a tab that looks like a tab, a hold that behaves like a hold, and drop zones
        // that work, none of which had to be written a second time.
        for held in &self.floating {
            // One layer for every floating window, not one each: egui orders layers within an
            // Order by when they were registered, and a raw layer painter registers no area -- so
            // two windows' layers could interleave and one's background paint over another's
            // contents.
            let top = ctx.layer_painter(egui::LayerId::new(
                stack::WINDOWS,
                egui::Id::new("floating"),
            ));
            // The frame: a bar along the top that is the window's own handle, and a border.
            top.rect_filled(to_egui(held.rect), m.radius, rgb(p.panel));
            top.rect_stroke(
                to_egui(held.rect),
                m.radius,
                egui::Stroke::new(1.0_f32, rgb(p.edge)),
            );

            for slot in held.layout.resolve(inset(held.rect, m.gutter)) {
                let showing = showing_of(&slot);
                let c = chrome::panel(
                    &slot,
                    &m,
                    style_of(&slot),
                    along_of(&slot, self.direction_of(showing)),
                    |i| measure(ctx, m.label, slot.tabs.get(i).copied()),
                );
                top.rect_filled(to_egui(c.header), m.radius, rgb(p.header));
                for tab in &c.tabs {
                    let hovered = false;
                    draw_tab(&top, tab, &slot, &m, &p, hovered);
                }
                if c.tabs.is_empty() {
                    // A compact header shows no names, so the panel's own is drawn on the frame
                    // instead: a window you cannot identify is a window you will not trust.
                    top.text(
                        egui::pos2(c.header.x + m.tab_padding, c.header.y + c.header.h / 2.0),
                        egui::Align2::LEFT_CENTER,
                        name_of(showing),
                        egui::FontId::proportional(m.label),
                        rgb(p.text),
                    );
                }
                if showing == CANVAS {
                    continue;
                }
                let mut ui = egui::Ui::new(
                    ctx.clone(),
                    egui::LayerId::new(stack::WINDOWS, egui::Id::new("floating")),
                    egui::Id::new(("floating-body", held.id.0, showing.0)),
                    egui::UiBuilder::new().max_rect(to_egui(c.controls.rect())),
                );
                ui.set_clip_rect(to_egui(c.controls.rect()));
                // The same rule as the arrangement's panels, and for the same reason: a pick can
                // land *in* a floating window.
                if was_picking {
                    ui.disable();
                }
                contents(showing, &mut ui, self.direction_of(showing), Place::Panel);
            }
        }

        // --- the popup, above everything, drawn by the same descriptor layer as any panel ---
        if let Some(popup) = self.popup {
            let theme = self.theme;
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::LayerId::new(stack::POPUP, egui::Id::new("workspace-popup")),
                egui::Id::new("workspace-popup-ui"),
                // A popup draws its own frame, so it owns its own padding: `chrome` is not
                // involved, and the renderer does not add any.
                egui::UiBuilder::new().max_rect(to_egui(inset(popup.rect, m.padding))),
            );
            ui.set_clip_rect(to_egui(popup.rect));
            let frame = ui.painter();
            frame.rect_filled(to_egui(popup.rect), m.radius, rgb(p.panel));
            frame.rect_stroke(
                to_egui(popup.rect),
                m.radius,
                egui::Stroke::new(1.0_f32, rgb(p.edge)),
            );
            match popup.kind {
                // The panel draws its own popup. Nothing here knows what is in it, which is what
                // keeps menus out of the workspace and the workspace out of menus.
                PopupKind::Panel(panel) => {
                    contents(panel, &mut ui, Direction::Column, Place::Popup);
                }
                kind => {
                    let controls = self.popup_controls(kind);
                    let changes = crate::panel_draw::show(
                        &mut ui,
                        &controls,
                        &theme,
                        Direction::Column,
                        &mut self.popup_input,
                    );
                    for change in changes {
                        self.apply_popup(kind, change);
                    }
                }
            }
        }
    }

    /// The controls a popup is showing.
    #[must_use]
    fn popup_controls(&self, showing: PopupKind) -> Vec<crate::panel_ui::Control> {
        use crate::panel_ui::Control;
        match showing {
            // A panel's own popup is drawn by the panel; this is never asked about one.
            PopupKind::Panel(_) => Vec::new(),
            PopupKind::Panels => panel_list_controls(self),
            PopupKind::Workspace => {
                let mut controls = vec![Control::Label {
                    text: "Icons".to_owned(),
                }];
                controls.extend(crate::icons::SETS.iter().map(|set| Control::Choice {
                    id: ICON_SET_BASE + set.id,
                    text: set.name.to_owned(),
                    selected: self.theme.icons == set.id,
                    // Deliberately no icon on the buttons that choose the icons: every set would
                    // draw its own, and a row of four different pictures of the same thing is a
                    // puzzle rather than a choice.
                    icon: None,
                }));
                controls
            }
            PopupKind::Settings(panel) => {
                let mut controls = vec![Control::Label {
                    text: format!("{} settings", name_of(panel)),
                }];
                let offered = kind(panel).map_or(&[][..], |k| k.settings);
                if offered.is_empty() {
                    // Said out loud rather than shown as an empty box. An empty popup is
                    // indistinguishable from one that failed to open (DECISIONS 6b), and it
                    // leaves the artist pressing again to see whether they missed something.
                    controls.push(Control::Label {
                        text: "Nothing to set here yet.".to_owned(),
                    });
                }
                for setting in offered {
                    match setting {
                        Setting::Floating => {
                            if self.is_floating(panel) {
                                // **One button, not a list of places.** The list named every
                                // docked panel, in tree order, in words -- a second description of
                                // the arrangement already on the screen, and one that could not
                                // say *where* in a panel it would land. Pressing this takes the
                                // window out and the next press says where it goes.
                                controls.push(Control::Button {
                                    id: PUT_BACK_ID,
                                    text: "Put back into...".to_owned(),
                                });
                            } else {
                                controls.push(Control::Button {
                                    id: FLOAT_ID,
                                    text: "Float".to_owned(),
                                });
                            }
                        }
                        Setting::Direction => {
                            controls.push(Control::Label {
                                text: "Controls run".to_owned(),
                            });
                            let current = self.direction_of(panel);
                            controls.extend(DIRECTIONS.iter().map(|(id, name, d)| {
                                Control::Choice {
                                    id: *id,
                                    text: (*name).to_owned(),
                                    selected: current == *d,
                                    icon: None,
                                }
                            }));
                        }
                    }
                }
                // **Every panel offers it**, whatever else it does or does not have to set. A
                // panel's own header is where you already are when you decide you are done with
                // it; making that mean "go and find the panel list instead" is a detour, and the
                // list is still there for putting it back.
                controls.push(Control::Separator);
                controls.push(Control::Button {
                    id: REMOVE_ID,
                    text: "Remove this panel".to_owned(),
                });
                controls
            }
        }
    }

    /// Act on something the artist changed in a popup.
    fn apply_popup(&mut self, kind: PopupKind, change: crate::panel_ui::Change) {
        use crate::panel_ui::Change;
        match (kind, change) {
            // Switching one off leaves the list up -- putting three away should be three taps.
            // Switching one *on* starts a pick, which needs the list out of the way, because the
            // answer is a press on the arrangement the list is covering.
            (PopupKind::Panels, Change::Toggled(id, _)) => self.toggle(PanelId(id)),
            (PopupKind::Workspace, Change::Chose(id)) if id >= ICON_SET_BASE => {
                self.set_icons(id - ICON_SET_BASE);
            }
            (PopupKind::Settings(panel), Change::Pressed(FLOAT_ID)) => self.float(panel),
            (PopupKind::Settings(panel), Change::Pressed(PUT_BACK_ID)) => {
                self.start_placing(panel);
            }
            (PopupKind::Settings(panel), Change::Pressed(REMOVE_ID)) => {
                self.hide(panel);
                // Nothing left for it to be the settings of.
                self.popup = None;
            }
            (PopupKind::Settings(panel), Change::Chose(id)) => {
                if let Some((_, _, d)) = DIRECTIONS.iter().find(|(i, _, _)| *i == id) {
                    self.set_direction(panel, *d);
                }
            }
            (_, other) => eprintln!("popup {kind:?}: unexpected {other:?}"),
        }
    }

    /// Which way a panel's controls run: what it has been set to, or what its kind defaults to.
    ///
    /// **The override is per panel, and a panel appears in the layout at most once**, so this is
    /// per instance without needing instance identity invented for it.
    #[must_use]
    pub fn direction_of(&self, panel: PanelId) -> Direction {
        self.options
            .get(&panel.0)
            .and_then(|o| o.direction)
            .unwrap_or_else(|| default_direction(panel))
    }

    /// Put everything a saved workspace could have supplied back to the built-in answer.
    ///
    /// One function rather than a list of fields at each call site: a field added later is a field
    /// somebody forgets to reset, and the symptom is a test drawing somebody else's workspace.
    #[cfg(test)]
    pub fn reset_to_built_in(&mut self) {
        self.layout = default_layout();
        self.theme = Theme::default();
        self.options.clear();
        self.floating.clear();
        self.grab_surface = Surface::Docked;
        self.popup = None;
        self.popup_wanted = None;
        self.placing = None;
    }

    /// The arrangement as it stands.
    #[must_use]
    fn snapshot(&self) -> Arrangement {
        Arrangement {
            docked: self.layout.clone(),
            floating: self.floating.clone(),
        }
    }

    /// Put an arrangement back, whole.
    fn restore(&mut self, was: Arrangement) {
        // See `remember_for_undo`. Safe against the recursion it looks like: `cancel_placing` takes
        // the pick before it restores, so the call back into here finds nothing to cancel.
        self.cancel_placing();
        self.layout = was.docked;
        self.floating = was.floating;
        // Whatever the gesture was working in may not exist any more.
        self.grab_surface = Surface::Docked;
        self.drag.let_go();
    }

    /// Record the arrangement as it was, before changing it.
    ///
    /// **The one place anything becomes undoable.** Scattered `history.record` calls are how a
    /// change gets forgotten: floating a panel was recorded, merging two windows was not, and
    /// tearing a tab out of one was not either.
    fn remember_for_undo(&mut self) {
        // **A pick gives way to any change underneath it.** The panel it is holding came out of an
        // arrangement, and the way back is a snapshot of that arrangement -- so a change made while
        // it waits would either be thrown away when the pick was cancelled, or, worse, undone from
        // under it: F3 during a pick restored a tree that still contained the panel in the air, and
        // answering the pick then put a second copy of it in.
        //
        // Here and in `restore`, which between them are every way the arrangement changes.
        self.cancel_placing();
        let was = self.snapshot();
        self.history.record(was);
    }

    /// Whether a panel is in a floating window.
    #[must_use]
    pub fn is_floating(&self, panel: PanelId) -> bool {
        self.floating.iter().any(|f| f.layout.find(panel).is_some())
    }

    /// Whether a panel is in the arrangement itself.
    #[must_use]
    pub fn is_docked(&self, panel: PanelId) -> bool {
        self.layout.find(panel).is_some()
    }

    /// Lift a panel out of the arrangement into a window of its own.
    ///
    /// Undoable like any other change to the arrangement, because it *is* one: the panel has left
    /// the tree, and getting it back should not need remembering where it was.
    pub fn float(&mut self, panel: PanelId) {
        if self.is_floating(panel) || !self.is_docked(panel) || !may_float(panel) {
            return;
        }
        self.remember_for_undo();
        self.layout.remove(panel);
        let at = first_float(self.screen, self.floating.len(), &self.theme.metrics);
        let id = self.take_float_id();
        self.floating.push(Floating {
            id,
            rect: at,
            layout: Layout::single(panel),
        });
        self.popup = None;
        self.remember();
    }

    /// Where a panel's controls are drawn, wherever the panel is.
    ///
    /// For tests that need to press something inside a panel and must not care whether it is in
    /// the arrangement or in a window -- which is exactly the difference they are checking does
    /// not matter.
    #[cfg(test)]
    #[must_use]
    pub fn content_of(&self, panel: PanelId, screen: Rect) -> Option<Rect> {
        let m = self.theme.metrics;
        let places = std::iter::once((&self.layout, screen)).chain(
            self.floating
                .iter()
                .map(|f| (&f.layout, inset(f.rect, m.gutter))),
        );
        for (layout, area) in places {
            for slot in layout.resolve(area) {
                if showing_of(&slot) == panel {
                    return Some(
                        chrome::panel(
                            &slot,
                            &m,
                            style_of(&slot),
                            along_of(&slot, self.direction_of(panel)),
                            |_| 46.0,
                        )
                        .controls
                        .rect(),
                    );
                }
            }
        }
        None
    }

    /// Put a floating window somewhere exact, for a screenshot that needs it over something.
    #[cfg(test)]
    pub fn put_window_for_test(&mut self, which: usize, rect: Rect) {
        self.floating[which].rect = rect;
    }

    /// The divider or window edge this frame's marks are for, and whether it has been taken.
    ///
    /// A method rather than three arguments assembled at the point of drawing: everything it needs
    /// beyond the seams and the pointer, it already knows. What is left in `show` is one call, with
    /// nothing there left to pass wrongly -- which is where the last version of this hid a
    /// sabotage that no test could reach.
    #[must_use]
    fn seam_mark(
        &self,
        seams: &[crate::layout::Splitter],
        at: Option<(f32, f32)>,
    ) -> Option<(Seam, bool)> {
        seam_in_use(
            self.drag.held().as_ref(),
            seams,
            self.rect_of(self.grab_surface),
            at,
            &self.theme.metrics,
        )
    }

    /// The panel waiting to be put somewhere, if there is one.
    #[must_use]
    pub fn placing(&self) -> Option<PanelId> {
        self.placing.as_ref().map(|q| q.panel)
    }

    /// Take a panel out and wait for the artist to say where it goes.
    ///
    /// **Out first, then the pick.** The panel leaving is what says the workspace is asking a
    /// question; a pick that changed nothing until it was answered would look exactly like nothing
    /// happening. What it left is remembered whole, so thinking better of it puts it back exactly.
    ///
    /// Starting a pick replaces one already waiting, and puts back whatever *that* one took --
    /// otherwise two panels would be in the air with one of them unreachable.
    pub fn start_placing(&mut self, panel: PanelId) {
        self.cancel_placing();
        let was = self.snapshot();
        self.layout.remove(panel);
        self.take_from_floating(panel);
        self.placing = Some(Placing { panel, was });
        // The list that offered this is in the way of the answer, and the answer is a press on the
        // arrangement behind it.
        self.popup = None;
    }

    /// Put the panel where the artist pointed. Returns whether it landed.
    ///
    /// Anywhere a panel could go: a leaf of the arrangement or a leaf of any floating window, at
    /// the same five zones a drag lands in. A press with nothing under it is not a destination, so
    /// it puts the pick back rather than dropping the panel somewhere arbitrary -- which is the
    /// same promise as "cancel is always available", reached by pressing nowhere.
    pub fn place_at(&mut self, x: f32, y: f32) -> bool {
        let Some(waiting) = self.placing.take() else {
            return false;
        };
        let Some((over, path, _, zone)) = self.landing_at(x, y) else {
            self.restore(waiting.was);
            return false;
        };
        // Put back rather than dropped if the destination somehow will not take it: this is the
        // one moment a panel is in neither tree, and losing one silently is the worst answer there
        // is (DECISIONS 6b).
        let landed = self
            .layout_of_mut(over)
            .is_some_and(|layout| layout.insert(&path, zone, waiting.panel));
        if !landed {
            self.restore(waiting.was);
            return false;
        }
        self.history.record(waiting.was);
        self.remember();
        true
    }

    /// Where a panel would land if it were let go here: which arrangement, which leaf, and which
    /// of that leaf's five zones.
    ///
    /// **One answer, asked by both the overlay and the drop.** Two would be two chances to
    /// disagree, and the way that shows is an overlay lighting up a zone the panel then does not
    /// land in -- which teaches the artist not to believe it.
    ///
    /// Floating windows first, because they are above.
    #[must_use]
    fn landing_at(&self, x: f32, y: f32) -> Option<(Surface, crate::layout::Path, Rect, Zone)> {
        // **Floating windows are not destinations, and not obstacles either.** A pick is asked
        // where in the *arrangement* a panel goes -- that is what "put it back" means -- so the
        // windows floating over it are simply transparent to the question. Otherwise a leaf sitting
        // behind a palette could not be pointed at at all, which is the one thing an arrangement
        // you can see should never be.
        //
        // Panels are moved *into* a window by dragging them there, which is a different gesture
        // with a different rule (the two modes never mix by dragging).
        let area = self.area_of(Surface::Docked)?;
        let leaf = self.layout.leaf_at(area, x, y)?;
        Some((
            Surface::Docked,
            leaf.path.clone(),
            leaf.rect,
            Layout::zone_at(leaf.rect, x, y),
        ))
    }

    /// Give up on a pick, putting back exactly what it took.
    pub fn cancel_placing(&mut self) {
        if let Some(waiting) = self.placing.take() {
            self.restore(waiting.was);
        }
    }

    /// Pull one panel out of a window that holds several, into a window of its own.
    ///
    /// Returns the new window, or `None` when there was nothing to pull out --- a window holding
    /// one panel *is* that panel, and dragging its tab moves the window it already has.
    ///
    /// The new window keeps the old one's size and lands under the pointer, so what was in your
    /// hand stays in your hand. The old window stays where it was, minus that panel.
    fn tear_off(&mut self, from: FloatId, panel: PanelId, at: (f32, f32)) -> Option<FloatId> {
        let source = self.floating.iter().find(|f| f.id == from)?;
        if source.layout.panels().len() < 2 {
            return None;
        }
        // Not recorded here: this happens part-way through a drag that the release will record
        // from its own snapshot, taken at the press. Two steps for one gesture means pressing undo
        // twice to get back to where you started, which nobody expects.
        let size = source.rect;
        let id = self.take_float_id();
        if let Some(held) = self.floating.iter_mut().find(|f| f.id == from) {
            held.layout.remove(panel);
        }
        self.floating.retain(|f| !f.layout.panels().is_empty());
        // Under the pointer, near the corner: the grip is reset with it, so the window does not
        // lurch to wherever the old one happened to be held.
        self.grip = (self.theme.metrics.header, self.theme.metrics.header / 2.0);
        self.floating.push(Floating {
            id,
            rect: hold_on_screen(
                Rect::new(at.0 - self.grip.0, at.1 - self.grip.1, size.w, size.h),
                self.screen,
                &self.theme.metrics,
            ),
            layout: Layout::single(panel),
        });
        self.remember();
        Some(id)
    }

    /// Learn how big the window is, and bring the floating ones back within reach of it.
    ///
    /// Called from both places that are told the size -- `show`, which draws, and `input_frame`,
    /// which decides -- because it must not matter which of them hears first. One function rather
    /// than the same two lines in each, so they cannot drift.
    pub fn set_screen(&mut self, screen: Rect) {
        if self.screen == screen {
            return;
        }
        self.screen = screen;
        self.hold_windows_on_screen();
    }

    /// Bring every floating window back within reach of the screen it is now on.
    ///
    /// **The third way a window is stranded.** Moving one is clamped, and now so is sizing one --
    /// but the screen can move instead of the window: park a window near the right edge of a wide
    /// display and open the file on a narrow one, and it is outside every hit test with no handle
    /// showing. Only a change of size does this, so it costs nothing on an ordinary frame.
    ///
    /// The same clamp a drag uses, so there is one idea of "within reach" rather than two.
    fn hold_windows_on_screen(&mut self) {
        let (screen, m) = (self.screen, self.theme.metrics);
        for held in &mut self.floating {
            held.rect = hold_on_screen(held.rect, screen, &m);
        }
    }

    /// Take note of where inside a window the pointer landed, so a drag can keep it there.
    ///
    /// Without it the window's corner jumps to the pointer the moment it moves, which reads as the
    /// window having been thrown rather than picked up.
    fn take_grip(&mut self, which: Surface, at: (f32, f32)) {
        let was = self
            .rect_of(which)
            .unwrap_or_else(|| Rect::new(0.0, 0.0, 0.0, 0.0));
        self.grip = (at.0 - was.x, at.1 - was.y);
        // The window *as it was*, and the point that took hold of it. Both, because a resize is
        // worked out from the press rather than accumulated frame by frame -- see
        // `Preview::ResizingWindow`.
        self.grip_rect = was;
        self.grip_at = at;
    }

    /// Pull a floating window's edges to where the pointer is.
    ///
    /// From the window as it was at the press plus the distance travelled since, never from where
    /// it is now: a size that stops at its floor eats the difference, and applying a delta a frame
    /// against that floor makes a window that was dragged past it and back come out smaller.
    fn resize_window_to(&mut self, which: Surface, pull: crate::chrome::Pull, at: (f32, f32)) {
        let Surface::Floating(id) = which else {
            return;
        };
        let (was, from) = (self.grip_rect, self.grip_at);
        let (screen, m) = (self.screen, self.theme.metrics);
        let least = least_window(&m);
        let want = crate::chrome::pull_edges(was, pull, at.0 - from.0, at.1 - from.1, least);
        let want = hold_size_on_screen(want, pull, screen, least);
        if let Some(held) = self.floating.iter_mut().find(|f| f.id == id) {
            held.rect = want;
        }
    }

    /// Carry a floating window to where the pointer is, keeping the grip.
    fn drag_window_to(&mut self, which: Surface, at: (f32, f32)) {
        let Surface::Floating(id) = which else {
            return;
        };
        let (screen, m, grip) = (self.screen, self.theme.metrics, self.grip);
        if let Some(held) = self.floating.iter_mut().find(|f| f.id == id) {
            held.rect = hold_on_screen(
                Rect::new(at.0 - grip.0, at.1 - grip.1, held.rect.w, held.rect.h),
                screen,
                &m,
            );
        }
    }

    /// A frame's worth of pointer, and what it does to the workspace.
    ///
    /// **The one entry point.** `show` draws and calls this; the tests call this and nothing else.
    /// They used to call the gesture handler directly, passing their own idea of which window the
    /// pointer was over -- so a bug in *working that out* could never fail a test, and one did:
    /// dragging the lower of two windows onto the upper worked and the reverse silently did
    /// nothing, with the whole suite green.
    fn input_frame(
        &mut self,
        screen: Rect,
        input: Pointer,
        on_popup: PopupPress,
        mut measure: impl FnMut(Option<PanelId>) -> f32,
    ) -> Option<Preview> {
        let m = self.theme.metrics;
        self.set_screen(screen);
        // **A pick owns the press, and owns it first.** Everything below is about rearranging what
        // is already placed; while a panel is in the air the only question on the table is where it
        // goes, and letting a header or a divider answer first would rearrange the workspace with
        // one hand while the other was still holding a panel.
        if self.placing.is_some() {
            if input.pressed {
                match input.at {
                    Some((x, y)) => {
                        self.place_at(x, y);
                    }
                    None => self.cancel_placing(),
                }
            }
            return None;
        }
        // The handle's side follows the shown panel's own setting, worked out before the closure
        // below borrows the workspace it would otherwise have to ask.
        let sides: std::collections::HashMap<u32, Direction> = PANELS
            .iter()
            .map(|k| (k.id.0, self.direction_of(k.id)))
            .collect();
        let direction_for = |pl: &crate::layout::Placed| {
            sides
                .get(&showing_of(pl).0)
                .copied()
                .unwrap_or(Direction::Column)
        };
        // **Which arrangement is under the pointer**, floating windows first because they are
        // above. A press picks one; everything after it stays with whatever the press picked, or a
        // drag that wandered over another window would start rearranging that one instead.
        // **Which arrangement is under the pointer**, with the resize border's reach: that border
        // straddles the window's edge, and its outer half is the window's to answer for.
        //
        // Except where the arrangement has something *visible* there. The outer half lies over
        // whatever is behind the window, and a header or a divider you can see must not lose a
        // press to a border you cannot -- the same rule that puts window rectangles ahead of window
        // borders, one layer further down. Inside a window's rectangle it does not arise: the
        // window is drawn on top, so it is what you can see.
        let docked = self.layout.resolve(screen);
        let seams = self.layout.splitters(screen, m.splitter_grab);
        let over = input
            .at
            .and_then(|(x, y)| {
                self.window_hit(0.0, x, y).or_else(|| {
                    self.window_hit(m.splitter_grab / 2.0, x, y).filter(|_| {
                        chrome::target_at(
                            &docked,
                            &seams,
                            &m,
                            |pl| (style_of(pl), along_of(pl, direction_for(pl))),
                            |pl, i| measure(pl.tabs.get(i).copied()),
                            x,
                            y,
                        ) == crate::panel_drag::Target::Elsewhere
                    })
                })
            })
            .map_or(Surface::Docked, Surface::Floating);
        self.grab_surface = working_surface(self.drag.active(), over, self.grab_surface);
        if input.pressed {
            if let Surface::Floating(id) = self.grab_surface {
                self.raise(id);
            }
        }
        let mut working = self.grab_surface;
        let area = self.surface(working).map_or(screen, |(_, area)| area);
        let (here, seams) = self
            .surface(working)
            .map(|(l, area)| (l.resolve(area), l.splitters(area, m.splitter_grab)))
            .unwrap_or_default();
        // A press on a floating window's frame, rather than on anything inside it, moves the
        // window. Every window needs somewhere to be picked up that is not one of its tabs.
        let frame_of = working != Surface::Docked;
        // The window's own rectangle, read before the closure below borrows the workspace. Its
        // edges resize it; the arrangement inside knows nothing about them.
        let border = self.rect_of(working);

        let preview = if on_popup == PopupPress::Consume {
            // The list owns this press. A drag already in flight still gets its release, which is
            // why only the press is withheld and not the whole frame.
            None
        } else {
            self.gesture(
                area,
                crate::panel_drag::pulse(input.pressed, input.released, input.down),
                input.at,
                input.now_ms,
                over,
                |x, y| {
                    let target = chrome::target_at(
                        &here,
                        &seams,
                        &m,
                        |pl| (style_of(pl), along_of(pl, direction_for(pl))),
                        |pl, i| measure(pl.tabs.get(i).copied()),
                        x,
                        y,
                    );
                    // **A header beats the border where they meet**, exactly as it beats a
                    // divider (`chrome::a_header_beats_a_divider_where_they_meet`). Both targets
                    // have to be generous, so they overlap, and the one you can see wins. Taking
                    // the border first instead ate eleven of a compact header's eighteen units and
                    // left a handle well under the 4 mm floor everything else is held to -- while
                    // the border itself is still reached from outside the window, which is where a
                    // title bar's resize edge lives everywhere else too.
                    if matches!(
                        target,
                        crate::panel_drag::Target::Elsewhere
                            | crate::panel_drag::Target::Splitter { .. }
                    ) {
                        if let Some(pull) = border.and_then(|r| {
                            chrome::edge_at(r, m.splitter_grab, least_window(&m), x, y)
                        }) {
                            return crate::panel_drag::Target::Edge { pull };
                        }
                    }
                    window_target(target, frame_of)
                },
            )
        };
        // **Moved by the pointer and the grip, never by accumulated deltas.** Applying a distance
        // each frame let the window slip: every clamp against a screen edge changed the offset
        // between pointer and window for good, so a drag out to the edge and back left it
        // somewhere else entirely. This is the same path a dragged tab takes, for the same reason.
        if matches!(preview, Some(Preview::MovingWindow)) {
            if let Some(at) = input.at {
                self.drag_window_to(working, at);
            }
        }
        if let Some(Preview::ResizingWindow { pull }) = preview {
            if let Some(at) = input.at {
                self.resize_window_to(working, pull, at);
            }
        }
        // **Dragging anything in a floating window moves the window.** A floating panel is a
        // window, and a window that could not be moved by the thing you naturally take hold of --
        // its tab -- could not be moved at all. Docking somewhere else is what *releasing* it over
        // another window means; on the way there it simply follows the pointer.
        if carry_moves_window(working, preview.as_ref()) {
            if let Some(at) = input.at {
                // **A tab dragged out of a window with more than one panel becomes its own.**
                // Until it does, dragging its tab would carry the whole group -- and the group is
                // what the strip beside the tabs is for.
                if let (Surface::Floating(id), Some(Preview::Carrying { panel, .. })) =
                    (working, preview.as_ref())
                {
                    if let Some(fresh) = self.tear_off(id, *panel, at) {
                        working = Surface::Floating(fresh);
                        self.grab_surface = working;
                    }
                }
                self.drag_window_to(working, at);
            }
        }
        if let Some(Preview::AskingFrame { path }) = &preview {
            self.frame_asked(working, path);
        }
        preview
    }

    /// A window's frame was held still: ask about the panel it is showing.
    ///
    /// A frame belongs to no one panel, so it stands for the one on show -- which is the panel
    /// whose settings carry "put it back", the reason to ask a window anything at all.
    fn frame_asked(&mut self, which: Surface, path: &[usize]) {
        let Some(area) = self.area_of(which) else {
            return;
        };
        let Some(layout) = self.layout_of(which) else {
            return;
        };
        // The leaf whose strip was held, not simply the first: a window can hold several after a
        // five-zone drop, each showing a different panel, and asking about the first one means
        // holding a strip opens somebody else's settings.
        let placed = layout.resolve(area);
        let slot = placed
            .iter()
            .find(|s| s.path == path)
            .or_else(|| placed.first());
        if let Some(slot) = slot {
            self.popup_wanted = Some(PopupKind::Settings(showing_of(slot)));
        }
    }

    /// Move a panel out of the arrangement it is in and into another, at the drop zone under the
    /// pointer.
    ///
    /// The source is emptied first and the destination looked up afterwards, because removing can
    /// collapse a tree and shift every path in it -- the same ordering hazard a move within one
    /// arrangement has, and the same answer: anchor to a panel, not to a path.
    fn merge_window(&mut self, from: FloatId, to: FloatId, x: f32, y: f32) {
        let Some(area) = self.area_of(Surface::Floating(to)) else {
            return;
        };
        // **The caller records.** It holds the arrangement as it was at the *press*, and by the
        // time this runs the window has been following the pointer for a while -- a snapshot taken
        // here would undo to a half-carried state. It also knows that a release finding no leaf
        // merges nothing, and that carrying the window there is still a change worth keeping.
        let Some(landing) = self
            .layout_of(Surface::Floating(to))
            .and_then(|l| l.leaf_at(area, x, y))
        else {
            return;
        };
        let zone = Layout::zone_at(landing.rect, x, y);
        // Anchored to a panel already there, so the path can be found again after the source
        // window has been taken down -- the same ordering hazard a move within one arrangement
        // has, and the same answer: anchor to a panel, not to a path.
        let Some(anchor) = landing.tabs.first().copied() else {
            return;
        };
        let Some(moving) = self
            .floating
            .iter()
            .find(|f| f.id == from)
            .map(|f| f.layout.panels())
        else {
            return;
        };
        if moving.is_empty() {
            return;
        }
        self.floating.retain(|f| f.id != from);
        for panel in moving {
            let Some((path, _)) = self
                .layout_of(Surface::Floating(to))
                .and_then(|l| l.find(anchor))
            else {
                // The window it was going into has gone. Put the panel back in the arrangement
                // rather than lose it, which is the worst possible answer (DECISIONS 6b).
                self.open(panel);
                continue;
            };
            if let Some(layout) = self.layout_of_mut(Surface::Floating(to)) {
                layout.insert(&path, zone, panel);
            }
        }
        self.remember();
    }

    /// The rectangle a surface fills.
    #[must_use]
    fn area_of(&self, which: Surface) -> Option<Rect> {
        match which {
            Surface::Docked => Some(self.screen),
            Surface::Floating(id) => self
                .floating
                .iter()
                .find(|f| f.id == id)
                .map(|f| inset(f.rect, self.theme.metrics.gutter)),
        }
    }

    /// A name no window has had before.
    fn take_float_id(&mut self) -> FloatId {
        let id = FloatId(self.next_float);
        self.next_float = self.next_float.wrapping_add(1);
        id
    }

    /// Remove a panel from whichever floating window holds it, taking the window down if that was
    /// the last thing in it.
    fn take_from_floating(&mut self, panel: PanelId) {
        for held in &mut self.floating {
            if held.layout.find(panel).is_some() {
                held.layout.remove(panel);
            }
        }
        self.floating.retain(|f| !f.layout.panels().is_empty());
    }

    /// Which floating window is under a point, front-most first.
    ///
    /// **Never the one being dragged.** A dragged window follows the pointer, so it is always
    /// under it -- and would always win its own hit test, leaving nothing to drop it on. That was
    /// a real bug and an asymmetric one: dragging the lower of two windows onto the upper worked,
    /// because the upper won the test; dragging the upper onto the lower found only itself and
    /// silently did nothing.
    ///
    /// **Only once it has moved.** Excluding it from the moment it was *pressed* was worse than
    /// the bug it fixed: a tap on a tab then reported the pointer as being over nothing at all, so
    /// the tap was read as a release in a different arrangement and thrown away -- and tapping a
    /// tab in a window holding two stopped switching between them.
    #[must_use]
    fn window_at(&self, x: f32, y: f32) -> Option<FloatId> {
        self.window_within(0.0, x, y)
    }

    /// The same question, with `reach` units of slack around every window for its resize border.
    ///
    /// **The rectangles are asked first, all of them, and only then the borders.** The border
    /// straddles the boundary exactly as a divider's does, so half of it lies outside the window
    /// -- and outside is where another window may be. A window you can see must never lose a press
    /// to the invisible outer half of one you cannot: two windows overlapping by a header's worth
    /// meant pressing the lower one's tab resized the upper.
    ///
    /// Deciding *whose input this is* uses the reach; deciding *where a panel lands* does not,
    /// because a drop just outside a window is a drop beside it, not into it.
    #[must_use]
    fn window_within(&self, reach: f32, x: f32, y: f32) -> Option<FloatId> {
        self.window_hit(0.0, x, y)
            .or_else(|| self.window_hit(reach, x, y))
    }

    /// Front-most window whose rectangle, grown by `reach`, holds the point.
    #[must_use]
    fn window_hit(&self, reach: f32, x: f32, y: f32) -> Option<FloatId> {
        let carried = match (self.drag.carries_window(), self.grab_surface) {
            (true, Surface::Floating(id)) => Some(id),
            _ => None,
        };
        self.floating
            .iter()
            .rev()
            .find(|f| Some(f.id) != carried && grown(f.rect, reach).contains(x, y))
            .map(|f| f.id)
    }

    /// A floating window's rectangle, frame and all --- the area its arrangement gets is inset
    /// from this.
    #[must_use]
    fn rect_of(&self, which: Surface) -> Option<Rect> {
        match which {
            Surface::Docked => None,
            Surface::Floating(id) => self.floating.iter().find(|f| f.id == id).map(|f| f.rect),
        }
    }

    /// Bring a floating window to the front.
    ///
    /// So the one you can see is the one you get: windows overlap, and the hit test answers with
    /// whichever is drawn on top. Without this a window could be permanently unreachable behind
    /// another, with its tabs visible and unpressable.
    fn raise(&mut self, id: FloatId) {
        let Some(at) = self.floating.iter().position(|f| f.id == id) else {
            return;
        };
        let held = self.floating.remove(at);
        self.floating.push(held);
    }

    /// The arrangement a surface holds, and the rectangle it fills.
    #[must_use]
    fn surface(&self, which: Surface) -> Option<(&Layout, Rect)> {
        match which {
            Surface::Docked => Some((&self.layout, self.screen)),
            Surface::Floating(id) => self
                .floating
                .iter()
                .find(|f| f.id == id)
                .map(|f| (&f.layout, inset(f.rect, self.theme.metrics.gutter))),
        }
    }

    /// The arrangement a surface holds. **The rectangle comes from the caller**, because the caller
    /// is the one that worked out which surface it was talking to and where that surface was --
    /// asking twice would be two answers to drift apart, and a `gesture` that took an area and
    /// then ignored it in favour of its own is exactly how that goes wrong.
    #[must_use]
    fn layout_of(&self, which: Surface) -> Option<&Layout> {
        match which {
            Surface::Docked => Some(&self.layout),
            Surface::Floating(id) => self.floating.iter().find(|f| f.id == id).map(|f| &f.layout),
        }
    }

    fn layout_of_mut(&mut self, which: Surface) -> Option<&mut Layout> {
        match which {
            Surface::Docked => Some(&mut self.layout),
            Surface::Floating(id) => self
                .floating
                .iter_mut()
                .find(|f| f.id == id)
                .map(|f| &mut f.layout),
        }
    }

    fn surface_mut(&mut self, which: Surface) -> Option<(&mut Layout, Rect)> {
        let gutter = self.theme.metrics.gutter;
        let screen = self.screen;
        match which {
            Surface::Docked => Some((&mut self.layout, screen)),
            Surface::Floating(id) => self.floating.iter_mut().find(|f| f.id == id).map(|f| {
                let area = inset(f.rect, gutter);
                (&mut f.layout, area)
            }),
        }
    }

    /// Choose the icon set, and write the look out.    /// Choose the icon set, and write the look out.
    ///
    /// Part of the theme, so it lives in the same file and is hand-editable like every other part
    /// of the look. An unknown id is refused rather than stored: a theme file naming a set this
    /// build does not have would otherwise leave every button blank.
    pub fn set_icons(&mut self, set: u32) {
        if crate::icons::SETS.iter().any(|s| s.id == set) {
            self.theme.icons = set;
            // Written out the same way `cycle_theme` writes the palette: the look is one file, and
            // a setting that survived until the next restart and no further would be worse than
            // one that never claimed to be saved at all.
            if let Some(path) = theme_path() {
                if let Err(e) = self
                    .theme
                    .to_json()
                    .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()))
                {
                    eprintln!("icons: the look could not be saved ({e})");
                }
            }
        } else {
            eprintln!("icons: no set numbered {set}; keeping the one in use");
        }
    }

    /// Ask for the workspace's own settings.
    pub fn open_settings(&mut self) {
        self.popup_wanted = Some(PopupKind::Workspace);
    }

    /// Set which way a panel's controls run, and remember it.
    pub fn set_direction(&mut self, panel: PanelId, direction: Direction) {
        self.options.entry(panel.0).or_default().direction = Some(direction);
        self.remember();
    }

    /// Open a panel's own popup, anchored to the control that asked for it.
    ///
    /// The panel supplies the size, because it is the one that knows what it is about to draw.
    pub fn open_popup_for(
        &mut self,
        panel: PanelId,
        at: Rect,
        size: (f32, f32),
        side: Anchor,
        screen: Rect,
    ) {
        self.popup = Some(Popup {
            kind: PopupKind::Panel(panel),
            rect: anchored_rect(at, size, side, screen),
        });
        self.popup_input = crate::panel_draw::PanelInput::default();
    }

    /// Whether a panel currently owns the open popup.
    #[must_use]
    pub fn popup_is_for(&self, panel: PanelId) -> bool {
        self.popup
            .is_some_and(|p| p.kind == PopupKind::Panel(panel))
    }

    /// Put any popup away.
    pub fn close_popup(&mut self) {
        self.popup = None;
    }

    /// Ask for the panel list without saying where.
    ///
    /// For reaching it from a control rather than from a press: there is no pointer position to
    /// open it at, and no window to measure against until the next frame begins. So it is a
    /// request, honoured at the top of `show`.
    pub fn open_panel_list(&mut self) {
        self.popup_wanted = Some(PopupKind::Panels);
    }

    /// Open a popup at a point, sized to its own contents and kept inside the window.
    ///
    /// Opening one near an edge must not put half of it out of reach, and a popup that had to be
    /// dragged back into view would be a worse problem than the one it solves.
    fn open_popup_at(
        &mut self,
        ctx: &egui::Context,
        kind: PopupKind,
        x: f32,
        y: f32,
        screen: Rect,
    ) {
        self.popup = Some(Popup {
            kind,
            rect: self.popup_rect(ctx, kind, x, y, screen),
        });
        // A fresh popup starts unscrolled and holding nothing, or it would open part-way down a
        // list it has never shown before.
        self.popup_input = crate::panel_draw::PanelInput::default();
    }

    fn popup_rect(
        &self,
        ctx: &egui::Context,
        kind: PopupKind,
        x: f32,
        y: f32,
        screen: Rect,
    ) -> Rect {
        let m = self.theme.metrics;
        let controls = self.popup_controls(kind);
        let text_of = |c: &crate::panel_ui::Control| crate::panel_draw::text_width(ctx, m.body, c);
        let widest = controls.iter().map(&text_of).fold(0.0_f32, f32::max);
        // Room for the widest label, whatever sits beside it, and the padding either side.
        let w = (widest + m.row * 1.6 + m.padding * 4.0).min(screen.w);
        // Measured from where the controls actually land, not from a second sum of the same
        // heights that could drift out of step with the first.
        let origin = Rect::new(0.0, 0.0, w - m.padding * 2.0, screen.h);
        let laid = crate::panel_ui::place(&controls, origin, &m, Direction::Column, text_of);
        let h = (crate::panel_ui::extent(&laid, origin).1 + m.padding * 2.0).min(screen.h);
        Rect::new(
            x.min(screen.x + screen.w - w).max(screen.x),
            y.min(screen.y + screen.h - h).max(screen.y),
            w,
            h,
        )
    }

    /// Put an arrangement on the undo stack, but only if the gesture actually changed something.
    ///
    /// A divider compares its own layouts, but a window's rectangle is in no layout -- so an edge
    /// dragged out and back would leave a step that undoes nothing, which is worse than no step at
    /// all: the artist presses undo and the workspace sits there. Asked of the whole arrangement,
    /// once, so it holds for every gesture rather than for the ones somebody remembered.
    fn record_if_changed(&mut self, was: Option<Arrangement>) {
        if let Some(was) = was.filter(|was| *was != self.snapshot()) {
            self.history.record(was);
            self.remember();
        }
    }

    /// Advance whatever gesture is in flight by one frame, and report what to draw.
    ///
    /// Separated from `show` so it can be driven frame by frame without a window, which is what
    /// the release bug needed and did not have: every drag test built a target by hand and called
    /// press/drag/release directly, so none of them ever saw the frame sequence the app produces.
    ///
    /// `target_at` is a closure rather than a value because working out what is under the pointer
    /// costs a text measurement per tab, and only a press needs the answer.
    fn gesture(
        &mut self,
        area: Rect,
        pulse: crate::panel_drag::Pulse,
        pointer: Option<(f32, f32)>,
        now_ms: f64,
        over: Surface,
        target_at: impl FnOnce(f32, f32) -> crate::panel_drag::Target,
    ) -> Option<crate::panel_drag::Preview> {
        use crate::panel_drag::Pulse;
        let Some((x, y)) = pointer else {
            // Nowhere to press, drop or follow to. A gesture in flight with no pointer is one
            // whose pointer has gone, whatever this frame claims.
            //
            // **This is the path a lost pointer actually takes**, not `Pulse::Lost`: egui reports
            // no position on exactly those frames. So the whole-arrangement restore belongs here
            // as much as there, or a window dragged and then abandoned kept its new place while
            // the arrangement rolled back around it.
            let moved = self.drag.moved();
            let mut drag = std::mem::take(&mut self.drag);
            if let Some((layout, _)) = self.surface_mut(self.grab_surface) {
                drag.cancel(layout);
            } else {
                drag.cancel(&mut Layout::single(CANVAS));
            }
            self.drag = drag;
            if let Some(was) = self.pending.take() {
                if moved {
                    self.restore(was);
                }
            }
            return None;
        };
        match pulse {
            Pulse::Press => {
                let target = target_at(x, y);
                let layout = self.layout_of(self.grab_surface)?.clone();
                // Taken at the press, because a divider moves live: by the time it is let go the
                // change has already happened, and a snapshot then records the state *after* it.
                self.pending = Some(self.snapshot());
                self.take_grip(self.grab_surface, (x, y));
                self.drag.press(&layout, &target, x, y, now_ms);
                None
            }
            Pulse::Release => {
                // Asked before the release takes the grab apart.
                let carried = self.drag.carries_window();
                // **The release carries a position of its own, and it is the last one.** A tab's
                // drop already used it; a window's move and a window's resize were applied only on
                // the frames in between, so the final movement of every window drag was thrown
                // away. One line here, rather than a rule about what a release position means for
                // some gestures and not others.
                match self.drag.window_move() {
                    Some(crate::panel_drag::WindowMove::Whole) => {
                        self.drag_window_to(self.grab_surface, (x, y));
                    }
                    Some(crate::panel_drag::WindowMove::Edges(pull)) => {
                        self.resize_window_to(self.grab_surface, pull, (x, y));
                    }
                    None => {}
                }
                // **Let go over a different arrangement and the panel goes there.** A drag that
                // started in one floating window and ended in another is the whole reason windows
                // hold arrangements rather than single panels; without this they could only ever
                // hold one thing each.
                // **Floating and docked never mix by dragging.** They are two modes, and the way
                // between them is the panel's own settings -- "Float", and "Put back into". A
                // floating window dropped on the arrangement would otherwise dock itself the
                // moment it was moved anywhere useful, which makes moving one impossible.
                // No guard on "has it actually moved": being over a *different* arrangement is
                // itself the answer, since the pointer cannot get there without moving. A guard
                // that cannot change the outcome only looks load-bearing, and a sabotage removing
                // this one changed nothing at all.
                // **Only a window that was being carried.** The comment that used to stand here
                // argued that no "has it moved" guard was needed, because the pointer could not
                // reach another arrangement without moving -- which was true when carrying was the
                // only gesture that could get here. Resizing a window and dragging a divider
                // inside one are not carries: the window stays put while the pointer travels, so
                // it can leave and end up over a neighbour. Without this, sizing a window down and
                // releasing over the window beside it merged the two and destroyed the one being
                // sized.
                if let (Surface::Floating(from), Surface::Floating(to)) = (self.grab_surface, over)
                {
                    if from != to && carried {
                        self.drag.let_go();
                        // **One gesture, one step, snapshotted at the press.** The window has been
                        // following the pointer since then, so this covers the carry as well as
                        // the merge -- and it covers the carry even when the merge finds no leaf
                        // and does nothing, which is a release in the border just outside the
                        // destination. `merge_window` used to take its own snapshot, by which time
                        // the window had already moved, so undo restored a half-carried state.
                        let was = self.pending.take();
                        self.merge_window(from, to, x, y);
                        self.record_if_changed(was);
                        return None;
                    }
                }
                if over != self.grab_surface && carried {
                    // Different modes: the window has already been carried to where it was let go,
                    // and that is all that was asked for -- but it is still a change, and it goes
                    // on the undo stack like every other. Without this, resizing a window could be
                    // taken back and *moving* one could not, which is the kind of inconsistency
                    // nobody can hold in their head.
                    self.drag.let_go();
                    let was = self.pending.take();
                    self.record_if_changed(was);
                    return None;
                }
                // The arrangement as it stood at the *press*, kept only if the gesture changed
                // something. Snapshotting here instead would be too late for a divider, which
                // moves live: by now the change has already happened.
                let was = self.pending.take();
                let mut drag = std::mem::take(&mut self.drag);
                let outcome = match self.layout_of_mut(self.grab_surface) {
                    Some(layout) => drag.release(layout, area, x, y),
                    None => Outcome::Nothing,
                };
                self.drag = drag;
                if saves(&outcome) {
                    self.record_if_changed(was);
                }
                match outcome {
                    Outcome::Floated(panel) => {
                        // Until floating windows exist, put it back rather than dropping it on
                        // the floor. Silently losing a panel would be the worst possible answer
                        // (DECISIONS 6b).
                        self.open(panel);
                    }
                    Outcome::Moved | Outcome::Resized | Outcome::Nothing | Outcome::Switched => {}
                }
                None
            }
            Pulse::Track => {
                let mut drag = std::mem::take(&mut self.drag);
                let preview = match self.layout_of_mut(self.grab_surface) {
                    Some(layout) => drag.drag(layout, area, x, y, now_ms),
                    None => None,
                };
                self.drag = drag;
                // A hold that completed asks the panel what it offers, straight away rather than
                // on release: a menu that only appeared once you let go could not be dismissed by
                // letting go.
                if let Some(Preview::Asking(panel)) = preview {
                    self.popup_wanted = Some(PopupKind::Settings(panel));
                    return None;
                }
                preview
            }
            Pulse::Lost => {
                let moved = self.drag.moved();
                let mut drag = std::mem::take(&mut self.drag);
                if let Some(layout) = self.layout_of_mut(self.grab_surface) {
                    drag.cancel(layout);
                } else {
                    drag.cancel(&mut Layout::single(CANVAS));
                }
                self.drag = drag;
                // The same whole-arrangement restore Escape does, for the same reason.
                if let Some(was) = self.pending.take() {
                    if moved {
                        self.restore(was);
                    }
                }
                None
            }
        }
    }
}

/// What a primary press does to an open popup.
///
/// Its own answer because it is three rules that have to agree, and burying them in a frame is
/// how the release bug happened: a press *inside* the list must reach the list and must not also
/// close it or start a panel drag; a press outside must close it; and with no list open there is
/// nothing to decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PopupPress {
    /// Nothing to do with the list.
    Ignore,
    /// Put the list away.
    Close,
    /// The list takes this press, and nothing else sees it.
    Consume,
}

#[must_use]
fn popup_press(popup: Option<Rect>, pressed: bool, at: Option<(f32, f32)>) -> PopupPress {
    if !pressed {
        return PopupPress::Ignore;
    }
    match (popup, at) {
        (Some(r), Some((x, y))) if r.contains(x, y) => PopupPress::Consume,
        (Some(_), _) => PopupPress::Close,
        (None, _) => PopupPress::Ignore,
    }
}

/// Which way a panel's controls run *unless the artist has said otherwise*.
///
/// From the panel table, so nothing branches on which panel it is holding: the answer is a column
/// in the table, and a panel added tomorrow brings its own. See [`Workspace::direction_of`] for
/// the answer that takes the setting into account.
#[must_use]
fn default_direction(panel: PanelId) -> Direction {
    kind(panel).map_or(Direction::Column, |k| k.direction)
}

/// The directions a panel can be set to, with the ids its settings popup uses.
///
/// One table, so the switches offered and the setting applied cannot come apart (§11a.8), and so
/// a direction added later appears in every panel's settings without anyone remembering to.
const DIRECTIONS: &[(u32, &str, Direction)] = &[
    (0, "Across", Direction::Row),
    (1, "Down", Direction::Column),
    (2, "Whichever fits", Direction::Auto),
    (3, "Across, wrapping", Direction::Wrap),
];

/// The id of the button that lifts a panel out of the arrangement.
const FLOAT_ID: u32 = 1 << 17;
/// The id of the button that takes a floating panel out and waits for somewhere to put it.
const PUT_BACK_ID: u32 = (1 << 17) + 1;
/// The id of the button that takes a panel out of the workspace altogether.
const REMOVE_ID: u32 = (1 << 17) + 2;

/// Where the icon-set choices start, out of the way of anything else a workspace popup offers.
const ICON_SET_BASE: u32 = 1 << 16;

/// Draw where a panel in the air would land: the five zones of the leaf under the pointer, and the
/// panel's name following the pointer so it is obvious *what* would land.
///
/// Shared by a drag and by a pick. They differ only in how the panel came to be in the air.
fn draw_landing(
    ctx: &egui::Context,
    m: &Metrics,
    p: &crate::theme::Palette,
    over: Option<crate::panel_drag::Landing>,
    pointer: Option<egui::Pos2>,
    panel: PanelId,
) {
    let top = ctx.layer_painter(egui::LayerId::new(
        stack::MARKS,
        egui::Id::new("workspace-drop"),
    ));
    if let Some(landing) = over {
        for region in chrome::drop_zones(landing.rect) {
            let live = region.zone == landing.zone;
            let points: Vec<egui::Pos2> = region
                .points
                .iter()
                .map(|q| egui::pos2(q[0], q[1]))
                .collect();
            top.add(egui::Shape::convex_polygon(
                points,
                egui::Color32::from_rgba_unmultiplied(
                    p.state.0[0],
                    p.state.0[1],
                    p.state.0[2],
                    if live { 90 } else { 26 },
                ),
                egui::Stroke::new(
                    if live { 1.5_f32 } else { 0.5 },
                    rgb(if live { p.bright } else { p.state }),
                ),
            ));
        }
    }
    if let Some(pos) = pointer {
        // The panel being carried, so it is obvious what will land.
        let label = name_of(panel);
        let size = egui::vec2(4.0f32.mul_add(label.len() as f32, 22.0), 20.0);
        let ghost = egui::Rect::from_center_size(pos + egui::vec2(0.0, -22.0), size);
        top.rect_filled(ghost, m.radius, rgb(p.header));
        top.rect_stroke(ghost, m.radius, egui::Stroke::new(1.0_f32, rgb(p.state)));
        top.text(
            ghost.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(m.label),
            rgb(p.bright),
        );
    }
}

/// Draw one tab: a shape you can see, in the same place you would press.
///
/// Shared by the arrangement and by floating windows, because "which of these is a handle" has to
/// look the same in both -- the complaint that started this was that a floating panel had no
/// button-looking thing to press, and a second drawing routine is how that happens again.
fn draw_tab(
    painter: &egui::Painter,
    tab: &crate::chrome::Tab,
    slot: &crate::layout::Placed,
    m: &Metrics,
    p: &crate::theme::Palette,
    hovered: bool,
) {
    // **Every tab is drawn as its own button**, docked or floating, by this one routine. There
    // were two, and they did not agree: a floating panel had no button-looking thing to press, and
    // a docked one filled its whole bar when you held it. "Which of these is a handle" has to look
    // the same in both, and a second drawing routine is how that stops being true.
    painter.rect_filled(
        to_egui(tab.rect),
        m.radius,
        rgb(if tab.active {
            p.panel
        } else if hovered {
            p.edge
        } else {
            p.header
        }),
    );
    painter.rect_stroke(
        to_egui(tab.rect),
        m.radius,
        egui::Stroke::new(1.0_f32, rgb(p.edge)),
    );
    if tab.active {
        // A line under the one on show, which is how a tab strip says which it is without spending
        // a block of the accent colour on it permanently.
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(tab.rect.x, tab.rect.y + tab.rect.h - 2.0),
                egui::vec2(tab.rect.w, 2.0),
            ),
            0.0_f32,
            rgb(p.state),
        );
    }
    painter.text(
        egui::pos2(tab.rect.x + m.tab_padding, tab.rect.y + tab.rect.h / 2.0),
        egui::Align2::LEFT_CENTER,
        slot.tabs.get(tab.index).copied().map_or("", name_of),
        egui::FontId::proportional(m.label),
        rgb(if tab.active || hovered {
            p.bright
        } else {
            p.dim
        }),
    );
}

/// Where a floating panel goes when it is first lifted out.
///
/// Offset from the top-left rather than centred, so several lifted in a row do not land exactly on
/// top of one another with no way to tell there is more than one.
#[must_use]
fn first_float(screen: Rect, already: usize, m: &Metrics) -> Rect {
    let step = m.header * f32::from(u8::try_from(already % 6).unwrap_or(0));
    let (w, h) = ((screen.w * 0.22).min(320.0), (screen.h * 0.35).min(420.0));
    hold_on_screen(
        Rect::new(screen.x + 60.0 + step, screen.y + 60.0 + step, w, h),
        screen,
        m,
    )
}

/// The smallest a floating window may be, on either axis.
///
/// The same something-left-to-grab rule that keeps a window on screen, asked once so the two
/// cannot answer differently: a window you may resize down to less than you may drag it down to
/// would be one you could resize into being unreachable.
#[must_use]
fn least_window(m: &Metrics) -> f32 {
    m.header.max(m.row)
}

/// A rectangle with `reach` added on every side.
#[must_use]
fn grown(rect: Rect, reach: f32) -> Rect {
    Rect::new(
        rect.x - reach,
        rect.y - reach,
        2.0f32.mul_add(reach, rect.w),
        2.0f32.mul_add(reach, rect.h),
    )
}

/// The same rule for a window being *sized*: `keep` units of it stay on screen.
///
/// **Only the edge that was pulled gives way.** A move is clamped by sliding the whole window
/// back, which is right for a move and exactly wrong here -- sliding would shift the edge nobody
/// touched, and the whole of what a resize promises is that the other side stays put. So the
/// pulled edge is the one that stops, and the window shrinks rather than walking.
///
/// The top is held to the screen as well, for the reason `hold_on_screen` gives: a window's header
/// is along its top, and a top edge dragged above the screen takes the header with it.
#[must_use]
fn hold_size_on_screen(rect: Rect, pull: crate::chrome::Pull, screen: Rect, keep: f32) -> Rect {
    let along = |low: f32, size: f32, side: i8, lo: f32, hi: f32, floor: bool| -> (f32, f32) {
        let high = low + size;
        match side {
            -1 => {
                let want = low.min(hi - keep);
                let want = if floor { want.max(lo) } else { want };
                (want, (high - want).max(keep))
            }
            1 => (low, (high.max(lo + keep) - low).max(keep)),
            _ => (low, size),
        }
    };
    let (x, w) = along(rect.x, rect.w, pull.x, screen.x, screen.x + screen.w, false);
    let (y, h) = along(rect.y, rect.h, pull.y, screen.y, screen.y + screen.h, true);
    Rect::new(x, y, w, h)
}

/// Keep a floating panel somewhere it can be taken hold of again.
///
/// **Its header must stay reachable**, which is the same rule as the weight floor: a panel with
/// nothing left to grab cannot be undone by hand. So it may hang off an edge -- that is often what
/// you want, to get it out of the way -- but never so far that the bar you drag it by is gone.
#[must_use]
fn hold_on_screen(rect: Rect, screen: Rect, m: &Metrics) -> Rect {
    let keep = least_window(m);
    Rect::new(
        rect.x
            .min(screen.x + screen.w - keep)
            .max(screen.x + keep - rect.w),
        // Never above the top: the header is at the top of the panel, so letting it go up would
        // put the one grabbable part off screen entirely.
        rect.y.min(screen.y + screen.h - keep).max(screen.y),
        rect.w.max(keep),
        rect.h.max(keep),
    )
}

/// Which panel's header is under a point, if any.
///
/// A header is a panel's handle: it is what you hold to move it, and what you press to ask what it
/// offers. Anything else belongs to the workspace rather than to a panel.
#[must_use]
fn header_under(
    placed: &[crate::layout::Placed],
    m: &Metrics,
    direction_for: impl Fn(&crate::layout::Placed) -> Direction,
    mut measure: impl FnMut(&crate::layout::Placed, usize) -> f32,
    x: f32,
    y: f32,
) -> Option<PanelId> {
    placed.iter().find_map(|slot| {
        // **The same chrome the primary button is tested against.** This used to build its own
        // with the handle always on top and every label measured as nothing, so on the tool rail --
        // whose handle is a bar down the side -- a right-click on the visible handle found no
        // header at all and opened the workspace list instead of the panel's settings.
        let c = chrome::panel(
            slot,
            m,
            style_of(slot),
            along_of(slot, direction_for(slot)),
            |i| measure(slot, i),
        );
        c.header
            .contains(x, y)
            .then(|| slot.tabs.get(slot.active).copied())
            .flatten()
    })
}

/// The panel list: one switch per panel this build knows about.
///
/// Built from `PANELS` rather than from a hand-written list, so a panel added to the table appears
/// here without anyone remembering to add it (recurring hazard 11a.8).
#[must_use]
fn panel_list_controls(ws: &Workspace) -> Vec<crate::panel_ui::Control> {
    let mut controls = vec![crate::panel_ui::Control::Label {
        text: "Panels".to_owned(),
    }];
    controls.extend(PANELS.iter().map(|k| crate::panel_ui::Control::Toggle {
        id: k.id.0,
        text: k.name.to_owned(),
        // Asked of the workspace rather than worked out again here, so a switch cannot show a
        // state the rest of the app disagrees with.
        on: ws.is_open(k.id),
    }));
    controls
}

/// Whether an outcome changed the arrangement and so needs writing out.
///
/// Its own function because the decision and the side effect are two different things, and a test
/// can only see the decision: a test that called `remember` would be testing the filesystem.
/// Switching tab deliberately does not count -- which tab you were looking at is not part of the
/// arrangement, the same reason it is not recorded for undo.
#[must_use]
fn saves(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Moved | Outcome::Resized | Outcome::Floated(_) => true,
        // Asking a panel what it offers has not changed anything yet.
        Outcome::Nothing | Outcome::Switched => false,
    }
}

/// How wide a panel's name is, for laying its tab out.
#[must_use]
fn measure(ctx: &egui::Context, label: f32, panel: Option<PanelId>) -> f32 {
    let Some(panel) = panel else {
        return 0.0;
    };
    let font = egui::FontId::proportional(label);
    ctx.fonts(|f| {
        f.layout_no_wrap(name_of(panel).to_owned(), font, egui::Color32::WHITE)
            .size()
            .x
    })
}

/// The rectangle a hold marks: the tab, or the whole bar when there are no tabs.
///
/// **The tab, not the bar it sits in.** Tinting the whole header made a strip of tabs look like
/// one control, which is exactly what pressing one appeared to do. A compact header carries no
/// tabs and is itself the handle, so there it takes the mark.
#[must_use]
fn hold_mark(chrome: &crate::chrome::PanelChrome, tab: usize) -> Rect {
    chrome
        .tabs
        .iter()
        .find(|t| t.index == tab)
        .map_or(chrome.header, |t| t.rect)
}

/// Whether carrying something means moving the whole window.
///
/// **In a floating window it always does.** A floating panel *is* a window, and a window that
/// could not be moved by the thing you naturally take hold of -- its tab -- could not be moved at
/// all. Docking somewhere else is what *releasing* it over another window means; on the way there
/// it simply follows the pointer.
#[must_use]
fn carry_moves_window(working: Surface, preview: Option<&Preview>) -> bool {
    working != Surface::Docked && matches!(preview, Some(Preview::Carrying { .. }))
}

/// A frame's worth of pointer.
///
/// One struct because these five always travel together and separately they are five chances to
/// pass the wrong one.
#[derive(Clone, Copy, Debug)]
struct Pointer {
    at: Option<(f32, f32)>,
    pressed: bool,
    released: bool,
    down: bool,
    now_ms: f64,
}

/// Which arrangement a gesture belongs to.
///
/// **A gesture keeps the arrangement it began in; with nothing in flight it is whatever is under
/// the pointer.** A drag that followed the pointer would start rearranging a window it was merely
/// passing across, and one that always assumed the docked arrangement would rearrange that instead
/// of the window it began in. Both were tried by a sabotage and neither is hypothetical.
///
/// Between gestures it follows the pointer, because that is what the hover marks are for: showing
/// what a press *would* take hold of.
#[must_use]
fn working_surface(in_flight: bool, over: Surface, current: Surface) -> Surface {
    if in_flight {
        current
    } else {
        over
    }
}

/// What a press means once it is known which arrangement it landed in.
///
/// Every window needs somewhere to be picked up that is not one of its tabs, or dragging it would
/// always threaten to pull a panel out of it. In the docked arrangement the same press means
/// nothing: there is no window to move, and the strip is being kept free for something better.
#[must_use]
fn window_target(target: crate::panel_drag::Target, in_window: bool) -> crate::panel_drag::Target {
    use crate::panel_drag::Target;
    match target {
        // A window's own body, or the strip beside its tabs: both are the window itself.
        // A window's own body has no leaf to name, so it stands for the first one -- which is the
        // only leaf a window with one has.
        Target::Elsewhere if in_window => Target::Frame { path: Vec::new() },
        Target::Strip { path } if in_window => Target::Frame { path },
        // In the arrangement the strip does nothing yet. It is the natural home for "float
        // everything in this panel", or for moving a whole tab group, and it is left free rather
        // than given a second meaning.
        Target::Strip { .. } => Target::Elsewhere,
        other => other,
    }
}

/// Which panel a leaf is showing, which is the one whose settings its handle obeys.
#[must_use]
fn showing_of(slot: &crate::layout::Placed) -> PanelId {
    slot.tabs.get(slot.active).copied().unwrap_or(CANVAS)
}

/// Which side a leaf's handle sits on: the direction its panel's controls run.
///
/// A leaf can hold several panels, and they can disagree. The *shown* one decides, because the
/// handle belongs to what you are looking at -- and a handle that jumped sides when you switched
/// tab would be worse than either answer.
///
/// `Direction::Auto` has no answer until the controls have been measured, which is something
/// `chrome` cannot do. It is read from the shape instead, which is what `place` will decide too in
/// every case but a near-tie.
#[must_use]
fn along_of(slot: &crate::layout::Placed, direction: Direction) -> Along {
    match direction {
        Direction::Column => Along::Down,
        Direction::Row | Direction::Wrap => Along::Across,
        Direction::Auto => {
            if slot.rect.w > slot.rect.h {
                Along::Across
            } else {
                Along::Down
            }
        }
    }
}

/// A leaf's header style: compact only when *every* panel in it wants that.
///
/// Stacking a named panel with a compact one has to show names, or the named one becomes
/// unreachable. Decided from the panels rather than from the leaf, so it is still a property of
/// panels and not a rule about places.
fn style_of(slot: &crate::layout::Placed) -> HeaderStyle {
    // **More than one panel in a leaf means tabs, whatever the panels would have preferred.**
    // A compact header spends no room on names, which also means it draws no tabs -- so stacking
    // the Menu and the Tools rail together showed one of them and hid the other with no way to
    // reach it. A header's job is to let you choose what the leaf shows; with two things in it,
    // that job needs tabs.
    if slot.tabs.len() > 1 {
        return HeaderStyle::Named;
    }
    if slot
        .tabs
        .iter()
        .all(|id| kind(*id).is_some_and(|k| k.header == HeaderStyle::Compact))
    {
        HeaderStyle::Compact
    } else {
        HeaderStyle::Named
    }
}

#[must_use]
fn name_of(panel: PanelId) -> &'static str {
    kind(panel).map_or("Panel", |k| k.name)
}

/// A bar of thickness `t` down the middle of a divider's grab region.
///
/// One helper for the hover hint, the wait and the armed state, so the three cannot disagree about
/// where the divider actually is.
fn centred(rect: Rect, axis: crate::layout::Axis, t: f32) -> Rect {
    match axis {
        crate::layout::Axis::Horizontal => {
            Rect::new(rect.x + rect.w / 2.0 - t / 2.0, rect.y, t, rect.h)
        }
        crate::layout::Axis::Vertical => {
            Rect::new(rect.x, rect.y + rect.h / 2.0 - t / 2.0, rect.w, t)
        }
    }
}

fn to_egui(r: Rect) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.w.max(0.0), r.h.max(0.0)))
}

fn inset(r: Rect, by: f32) -> Rect {
    Rect::new(
        r.x + by,
        r.y + by,
        (r.w - by * 2.0).max(0.0),
        (r.h - by * 2.0).max(0.0),
    )
}

fn rgb(c: Color) -> egui::Color32 {
    egui::Color32::from_rgb(c.0[0], c.0[1], c.0[2])
}

#[cfg(test)]
mod tests {

    /// How many times a panel appears in the whole workspace, floating windows included.
    ///
    /// `is_open` answers "at all"; this answers "how many", which is the question a duplicate
    /// makes worth asking.
    fn count_of(ws: &Workspace, panel: PanelId) -> usize {
        let one = |l: &Layout| l.panels().iter().filter(|p| **p == panel).count();
        one(&ws.layout) + ws.floating.iter().map(|f| one(&f.layout)).sum::<usize>()
    }

    /// A workspace with the default arrangement and nothing in flight.
    ///
    /// Built directly rather than through `Workspace::new`, which would read whatever the machine
    /// running the tests happens to have saved.
    fn bare() -> Workspace {
        Workspace {
            layout: default_layout(),
            history: crate::layout::History::default(),
            theme: crate::theme::Theme::default(),
            drag: crate::panel_drag::PanelDrag::default(),
            canvas_rect: None,
            popup: None,
            popup_input: crate::panel_draw::PanelInput::default(),
            popup_wanted: None,
            options: std::collections::HashMap::new(),
            floating: Vec::new(),
            grab_surface: Surface::Docked,
            next_float: 0,
            grip: (0.0, 0.0),
            grip_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            grip_at: (0.0, 0.0),
            pending: None,
            placing: None,
            screen: Rect::new(0.0, 0.0, 1280.0, 800.0),
        }
    }

    /// A press inside the open list belongs to the list: it must not also close it, and it must
    /// not start a panel drag underneath.
    ///
    /// Closing on the press that toggled a switch would make showing three panels three
    /// right-clicks instead of three taps.
    #[test]
    fn a_press_inside_the_panel_list_is_the_lists_own() {
        let r = Rect::new(100.0, 100.0, 200.0, 300.0);
        assert_eq!(
            popup_press(Some(r), true, Some((150.0, 150.0))),
            PopupPress::Consume
        );
        // Outside puts it away.
        assert_eq!(
            popup_press(Some(r), true, Some((50.0, 50.0))),
            PopupPress::Close
        );
        // A press with the pointer nowhere is still a press somewhere else.
        assert_eq!(popup_press(Some(r), true, None), PopupPress::Close);
        // Hovering over it decides nothing.
        assert_eq!(
            popup_press(Some(r), false, Some((150.0, 150.0))),
            PopupPress::Ignore
        );
        assert_eq!(
            popup_press(None, true, Some((150.0, 150.0))),
            PopupPress::Ignore
        );
    }

    /// An open list floats over whatever is beneath it, including the canvas, so the workspace is
    /// busy while it is up. Otherwise a tap on a switch also lays down a brush dab.
    #[test]
    fn an_open_panel_list_holds_the_pointer() {
        let mut ws = bare();
        assert!(!ws.busy(), "nothing is happening yet");
        ws.popup = Some(Popup {
            kind: PopupKind::Panels,
            rect: Rect::new(10.0, 10.0, 200.0, 300.0),
        });
        assert!(
            ws.busy(),
            "a press meant for the list must not reach the canvas"
        );
        ws.cancel_drag();
        assert!(!ws.busy(), "and Escape puts it away");
    }

    /// Every direction a panel can be in is offered by its settings.
    ///
    /// Named here rather than read out of `DIRECTIONS`, because a test that takes its expectations
    /// from the table it is checking passes whatever the table says --- including a table with a
    /// direction quietly missing from it. The match is exhaustive, so adding a direction without
    /// offering it stops compiling rather than shipping.
    #[test]
    fn every_direction_a_panel_can_be_in_is_offered() {
        for d in [
            Direction::Row,
            Direction::Column,
            Direction::Auto,
            Direction::Wrap,
        ] {
            // Exhaustive on purpose: a new variant must be added here, which is the prompt to add
            // it to `DIRECTIONS` too.
            let known = match d {
                Direction::Row | Direction::Column | Direction::Auto | Direction::Wrap => true,
            };
            assert!(known);
            assert!(
                DIRECTIONS.iter().any(|(_, _, offered)| *offered == d),
                "{d:?} cannot be chosen from a panel's settings"
            );
        }
        // And no direction is offered twice, which would put two switches on the same answer.
        for (i, (id, _, d)) in DIRECTIONS.iter().enumerate() {
            for (other_id, _, other) in DIRECTIONS.iter().skip(i + 1) {
                assert_ne!(d, other, "offered twice");
                assert_ne!(id, other_id, "two directions share an id");
            }
        }
    }

    /// Settings go to disk under panel *names*, and come back only for names this build knows.
    ///
    /// The same reason the arrangement does: an id is whatever row a panel occupies in the table,
    /// so a file full of them would hand one panel's settings to another the day a panel is added.
    #[test]
    fn settings_are_written_by_name_and_read_back_by_name() {
        let mut options = std::collections::HashMap::new();
        options.insert(
            LAYERS.0,
            PanelOptions {
                direction: Some(Direction::Row),
            },
        );
        // A panel left at its defaults is not written at all, or the file would freeze today's
        // defaults into every saved workspace.
        options.insert(BRUSH.0, PanelOptions::default());

        let saved = options_to_saved(&options);
        assert_eq!(saved.keys().collect::<Vec<_>>(), vec!["Layers"]);
        assert_eq!(
            options_from_saved(&saved).get(&LAYERS.0).copied(),
            options.get(&LAYERS.0).copied()
        );

        // A name this build does not know is dropped rather than becoming some other panel's.
        let mut strange = std::collections::BTreeMap::new();
        strange.insert(
            "Sparkles".to_owned(),
            PanelOptions {
                direction: Some(Direction::Wrap),
            },
        );
        assert!(options_from_saved(&strange).is_empty());
    }

    /// Settings survive a trip through the saved form, and are written under panel *names*.
    ///
    /// The same reason the arrangement is: an id is whatever row a panel occupies in the table, so
    /// a file full of them would hand one panel's settings to another the day a panel is added.
    #[test]
    fn settings_round_trip_by_name() {
        let saved = SavedWorkspace {
            layout: default_layout().to_saved(name_for),
            panels: [(
                "Layers".to_owned(),
                PanelOptions {
                    direction: Some(Direction::Row),
                },
            )]
            .into_iter()
            .collect(),
            floating: Vec::new(),
        };
        let text = serde_json::to_string(&saved).expect("serialise");
        assert!(
            text.contains("Layers"),
            "settings must be written by name, not by number: {text}"
        );

        let back: SavedWorkspace = serde_json::from_str(&text).expect("parse");
        assert_eq!(back.panels, saved.panels);
        assert_eq!(
            back.layout, saved.layout,
            "and the arrangement came with it"
        );

        // A name this build does not know is dropped rather than refusing the whole file.
        let text = text.replace("Layers", "Sparkles");
        let back: SavedWorkspace = serde_json::from_str(&text).expect("parse");
        let options: std::collections::HashMap<u32, PanelOptions> = back
            .panels
            .iter()
            .filter_map(|(n, o)| id_for(n).map(|id| (id.0, *o)))
            .collect();
        assert!(options.is_empty());
    }

    /// **A drop-down opens under its button, and flips rather than sliding.**
    ///
    /// Clamping it to the bottom of the window would slide it up over the button that opened it,
    /// hiding the one thing that says which menu you are looking at. Every menu bar in existence
    /// opens upwards near the foot of a screen instead.
    #[test]
    fn a_menu_opens_under_its_button_and_flips_when_there_is_no_room() {
        let screen = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let size = (160.0, 200.0);

        // Room below: directly under it, left edges lined up.
        let button = Rect::new(120.0, 30.0, 60.0, 24.0);
        let under = anchored_rect(button, size, Anchor::Below, screen);
        assert!(
            (under.y - (button.y + button.h)).abs() < 0.001,
            "not under the button"
        );
        assert!((under.x - button.x).abs() < 0.001, "not lined up with it");

        // No room below: above, and the button stays visible.
        let low = Rect::new(120.0, 700.0, 60.0, 24.0);
        let over = anchored_rect(low, size, Anchor::Below, screen);
        assert!(
            over.y + over.h <= low.y + 0.001,
            "it should have opened upwards, not slid over the button"
        );
        assert!(over.y >= screen.y - 0.001);

        // A menu bar running down the side opens out to the side instead.
        let side = Rect::new(10.0, 200.0, 90.0, 24.0);
        let beside = anchored_rect(side, size, Anchor::Right, screen);
        assert!((beside.x - (side.x + side.w)).abs() < 0.001);
        assert!((beside.y - side.y).abs() < 0.001);

        // And against the right edge it opens leftwards.
        let far = Rect::new(940.0, 200.0, 50.0, 24.0);
        let left = anchored_rect(far, size, Anchor::Right, screen);
        assert!(
            left.x + left.w <= far.x + 0.001,
            "it should have opened leftwards"
        );
    }

    /// A popup always ends up on screen, whatever it is anchored to.
    ///
    /// Including one bigger than the window: sliding is the last resort, but it is still better
    /// than a list whose first entry is off the top.
    #[test]
    fn a_popup_is_always_somewhere_you_can_reach() {
        let screen = Rect::new(0.0, 0.0, 400.0, 300.0);
        for anchor in [Anchor::Below, Anchor::Right] {
            for at in [
                Rect::new(-50.0, -50.0, 20.0, 20.0),
                Rect::new(390.0, 290.0, 20.0, 20.0),
                Rect::new(200.0, 150.0, 20.0, 20.0),
            ] {
                for size in [(100.0, 80.0), (1000.0, 900.0)] {
                    let r = anchored_rect(at, size, anchor, screen);
                    assert!(
                        r.x >= screen.x - 0.001
                            && r.y >= screen.y - 0.001
                            && r.x + r.w <= screen.x + screen.w + 0.001
                            && r.y + r.h <= screen.y + screen.h + 0.001,
                        "{anchor:?} at {at:?} size {size:?} landed off screen: {r:?}"
                    );
                    assert!(r.w > 0.0 && r.h > 0.0, "and it must have some size");
                }
            }
        }
    }

    /// **Every icon set can be chosen, and an unknown one is refused rather than stored.**
    ///
    /// A theme file naming a set this build does not have would otherwise leave every button
    /// blank, which reads as the UI having failed to load.
    #[test]
    fn the_icon_set_can_be_chosen_and_only_from_the_ones_that_exist() {
        let mut ws = bare();
        let popup = ws.popup_controls(PopupKind::Workspace);
        for set in crate::icons::SETS {
            assert!(
                popup.iter().any(|c| c.id() == Some(ICON_SET_BASE + set.id)),
                "the {} set cannot be chosen",
                set.name
            );
        }
        // Exactly one reads as chosen, whatever the current setting is.
        let lit = popup
            .iter()
            .filter(|c| matches!(c, crate::panel_ui::Control::Choice { selected: true, .. }))
            .count();
        assert_eq!(lit, 1, "{lit} sets read as chosen");

        let words = crate::icons::SETS
            .iter()
            .find(|s| s.name == "Words")
            .expect("a Words set");
        ws.apply_popup(
            PopupKind::Workspace,
            crate::panel_ui::Change::Chose(ICON_SET_BASE + words.id),
        );
        assert_eq!(ws.theme.icons, words.id);

        let before = ws.theme.icons;
        ws.set_icons(9999);
        assert_eq!(
            ws.theme.icons, before,
            "a set this build does not have should be refused, not stored"
        );
    }

    /// **A gesture stays in the arrangement it began in.**
    ///
    /// Following the pointer instead would rearrange a window it was merely passing across;
    /// always assuming the docked one would rearrange that instead of the window it began in.
    #[test]
    fn a_gesture_stays_where_it_started() {
        let a = Surface::Floating(FloatId(1));
        let b = Surface::Floating(FloatId(2));
        // With nothing in flight it follows the pointer, which is what the hover marks need.
        assert_eq!(working_surface(false, a, Surface::Docked), a);
        assert_eq!(working_surface(false, Surface::Docked, a), Surface::Docked);
        // Once a gesture is running it keeps its own, whatever the pointer has wandered over.
        assert_eq!(working_surface(true, b, a), a);
        assert_eq!(working_surface(true, Surface::Docked, a), a);
    }

    /// A press inside a floating window on none of its chrome is the window itself.
    ///
    /// Every window needs somewhere to be picked up that is not one of its tabs, or dragging it
    /// would always threaten to pull a panel out of it.
    #[test]
    fn a_press_on_a_windows_own_body_moves_the_window() {
        use crate::panel_drag::Target;
        assert_eq!(
            window_target(Target::Elsewhere, true),
            Target::Frame { path: vec![] }
        );
        // And the strip carries the leaf it belongs to, so a window holding several can say which
        // one a hold is asking about.
        assert_eq!(
            window_target(Target::Strip { path: vec![1, 0] }, true),
            Target::Frame { path: vec![1, 0] }
        );
        // In the arrangement there is no window to move, so both still mean nothing.
        assert_eq!(window_target(Target::Elsewhere, false), Target::Elsewhere);
        assert_eq!(
            window_target(Target::Strip { path: vec![1] }, false),
            Target::Elsewhere
        );
        // And it never steals a press that belonged to something.
        let tab = Target::Tab {
            path: vec![1],
            tab: 0,
        };
        assert_eq!(window_target(tab.clone(), true), tab);
        let seam = Target::Splitter {
            path: vec![],
            index: 0,
        };
        assert_eq!(window_target(seam.clone(), true), seam);
    }

    /// A window pushed about goes where it was pushed, and stays reachable.
    #[test]
    fn a_window_is_carried_by_the_place_it_was_held() {
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(BRUSH);
        let id = ws.floating[0].id;
        let before = ws.floating[0].rect;

        // Taken hold of at its corner, then carried.
        ws.take_grip(Surface::Floating(id), (before.x, before.y));
        ws.drag_window_to(Surface::Floating(id), (before.x + 40.0, before.y + 25.0));
        let after = ws.floating[0].rect;
        assert!(
            (after.x - before.x - 40.0).abs() < 0.001 && (after.y - before.y - 25.0).abs() < 0.001,
            "carried 40,25 and moved {},{}",
            after.x - before.x,
            after.y - before.y
        );

        // **Out to a corner and back leaves it where the hand is, not where a clamp left it.**
        // Moving it by accumulated deltas did not: every clamp against an edge changed the offset
        // between pointer and window for good.
        ws.drag_window_to(Surface::Floating(id), (-9000.0, -9000.0));
        ws.drag_window_to(Surface::Floating(id), (before.x + 40.0, before.y + 25.0));
        assert_eq!(
            ws.floating[0].rect, after,
            "the window slipped out from under the pointer"
        );

        // Thrown at the far corner, it still keeps a handle on screen.
        ws.drag_window_to(Surface::Floating(id), (9000.0, 9000.0));
        let far = ws.floating[0].rect;
        assert!(far.x < ws.screen.x + ws.screen.w, "it went off the right");
        assert!(far.y < ws.screen.y + ws.screen.h, "it went off the bottom");

        // The docked arrangement is not a window and cannot be carried.
        let docked = ws.layout.clone();
        ws.drag_window_to(Surface::Docked, (100.0, 100.0));
        assert_eq!(ws.layout, docked);
    }

    /// **A secondary press finds a header where the header actually is.**
    ///
    /// `header_under` used to build chrome of its own, with the handle always on top and every
    /// label measured as nothing. On a panel laid out across -- whose handle is a bar down its
    /// left side -- a right-click on the visible handle found no header at all and offered the
    /// workspace list instead of that panel's settings. Nothing could see it, because nothing
    /// tested the secondary press against a panel whose handle is anywhere but the top.
    #[test]
    fn a_secondary_press_on_a_side_handle_finds_the_panel() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.set_direction(TOOLS, Direction::Row);

        let m = ws.theme.metrics;
        let placed = ws.layout.resolve(screen);
        let rail = placed
            .iter()
            .find(|p| p.tabs.contains(&TOOLS))
            .expect("the tool rail");
        let c = crate::chrome::panel(rail, &m, style_of(rail), Along::Across, |_| 46.0);
        assert!(
            c.header.h > m.header * 2.0,
            "laid out across, the handle should be a bar down the side"
        );
        // Well down that bar: inside the handle as drawn, and far below anything a strip across
        // the top could reach.
        let at = (c.header.x + c.header.w / 2.0, c.header.y + c.header.h - 4.0);

        let found = header_under(
            &placed,
            &m,
            |pl| ws.direction_of(showing_of(pl)),
            |_, _| 46.0,
            at.0,
            at.1,
        );
        assert_eq!(
            found,
            Some(TOOLS),
            "a secondary press on the visible handle found no header"
        );
    }

    /// Holding a window's frame asks about the panel it is showing.
    #[test]
    fn holding_a_windows_frame_asks_about_what_it_shows() {
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(BRUSH);
        let id = ws.floating[0].id;

        ws.frame_asked(Surface::Floating(id), &[]);
        assert_eq!(
            ws.popup_wanted,
            Some(PopupKind::Settings(BRUSH)),
            "a window's frame should ask about the panel it is showing"
        );
    }

    /// Drive a whole gesture through the *application's* input path.
    ///
    /// **This is the only way these tests are allowed to press anything.** They used to call the
    /// gesture handler directly with their own idea of which window the pointer was over, so a bug
    /// in working that out could not fail a test -- and one did not: dragging the lower of two
    /// floating windows onto the upper worked, the reverse silently did nothing, and the suite was
    /// green throughout.
    struct Hand<'a> {
        ws: &'a mut Workspace,
        screen: Rect,
        clock: f64,
        /// Where the pointer was left, so time can pass without it moving.
        last: (f32, f32),
    }

    impl Hand<'_> {
        fn new(ws: &mut Workspace, screen: Rect) -> Hand<'_> {
            // Through the same door the shell uses, so a test cannot hand the workspace a size in
            // a way the application never would.
            ws.set_screen(screen);
            Hand {
                ws,
                screen,
                clock: 0.0,
                last: (0.0, 0.0),
            }
        }

        /// One frame. `measure` is a fixed label width, since there are no fonts here.
        fn frame(&mut self, at: (f32, f32), pressed: bool, released: bool, down: bool) {
            self.last = at;
            self.clock += 1.0;
            let screen = self.screen;
            let clock = self.clock;
            self.ws.input_frame(
                screen,
                Pointer {
                    at: Some(at),
                    pressed,
                    released,
                    down,
                    now_ms: clock,
                },
                PopupPress::Ignore,
                |_| 46.0,
            );
        }

        fn press(&mut self, at: (f32, f32)) {
            self.frame(at, true, false, true);
        }

        fn move_to(&mut self, at: (f32, f32)) {
            self.frame(at, false, false, true);
        }

        fn release(&mut self, at: (f32, f32)) {
            self.frame(at, false, true, false);
        }

        /// Press, carry, let go -- the shape of every real drag.
        fn drag(&mut self, from: (f32, f32), to: (f32, f32)) {
            self.press(from);
            self.move_to(to);
            self.release(to);
        }

        /// The same, but let go somewhere the last move frame did not reach.
        ///
        /// A real release carries a position of its own, and it is the last one there is. `drag`
        /// releases where it last moved to, so no test built on it could ever see a gesture that
        /// throws the release position away -- and two of them did.
        fn drag_letting_go_at(&mut self, from: (f32, f32), via: (f32, f32), to: (f32, f32)) {
            self.press(from);
            self.move_to(via);
            self.release(to);
        }

        /// The pointer goes away mid-gesture: no position at all, which is what egui reports.
        fn gone(&mut self) {
            self.clock += 1.0;
            let screen = self.screen;
            let clock = self.clock;
            self.ws.input_frame(
                screen,
                Pointer {
                    at: None,
                    pressed: false,
                    released: false,
                    down: true,
                    now_ms: clock,
                },
                PopupPress::Ignore,
                |_| 46.0,
            );
        }

        /// Let time pass with the pointer where it is and the button still down.
        fn wait(&mut self, ms: f64) {
            self.clock += ms;
            let at = self.last;
            self.frame(at, false, false, true);
        }

        /// Hold still long enough for the hold to fire.
        fn hold(&mut self, at: (f32, f32)) {
            self.press(at);
            self.clock += crate::panel_drag::HOLD_MS;
            self.frame(at, false, false, true);
        }

        /// The middle of a floating window's first tab.
        fn tab_of(&self, id: FloatId) -> (f32, f32) {
            let m = self.ws.theme.metrics;
            let area = self.ws.area_of(Surface::Floating(id)).expect("the window");
            let slot = self
                .ws
                .layout_of(Surface::Floating(id))
                .expect("its arrangement")
                .resolve(area)
                .into_iter()
                .next()
                .expect("a leaf");
            let c = crate::chrome::panel(&slot, &m, style_of(&slot), Along::Down, |_| 46.0);
            let t = c.tabs.first().expect("a tab").rect;
            (t.x + t.w / 2.0, t.y + t.h / 2.0)
        }

        /// A point on a floating window's panel strip: past its tabs, still on its bar.
        fn strip_of(&self, id: FloatId) -> (f32, f32) {
            let m = self.ws.theme.metrics;
            let area = self.ws.area_of(Surface::Floating(id)).expect("the window");
            let slot = self
                .ws
                .layout_of(Surface::Floating(id))
                .expect("its arrangement")
                .resolve(area)
                .into_iter()
                .next()
                .expect("a leaf");
            let c = crate::chrome::panel(&slot, &m, style_of(&slot), Along::Down, |_| 46.0);
            let past = c.tabs.last().map_or(c.header.x, |t| t.rect.x + t.rect.w);
            (
                past + crate::chrome::strip_width(&m) / 2.0,
                c.header.y + c.header.h / 2.0,
            )
        }

        fn rect_of(&self, id: FloatId) -> Rect {
            self.ws
                .floating
                .iter()
                .find(|f| f.id == id)
                .expect("the window")
                .rect
        }
    }

    /// **Either window can be dropped on the other.** It was asymmetric: the lower one could be
    /// dragged onto the upper and not the other way round, because the window being dragged
    /// follows the pointer and so always won its own hit test.
    #[test]
    fn either_floating_window_can_be_dropped_on_the_other() {
        for (drag_first, keep_first) in [(true, false), (false, true)] {
            let mut ws = bare();
            let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
            ws.screen = screen;
            ws.float(BRUSH);
            ws.float(COLOUR);
            let (a, b) = (ws.floating[0].id, ws.floating[1].id);
            let (from, onto) = if drag_first { (a, b) } else { (b, a) };
            let _ = keep_first;

            let mut hand = Hand::new(&mut ws, screen);
            let grab = hand.tab_of(from);
            let target = hand.rect_of(onto);
            let drop = (target.x + target.w / 2.0, target.y + target.h / 2.0);
            hand.press(grab);
            hand.move_to(drop);
            hand.release(drop);

            assert_eq!(
                ws.floating.len(),
                1,
                "dragging {from:?} onto {onto:?} should have merged them"
            );
            let mut left = ws.floating[0].layout.panels();
            left.sort_by_key(|p| p.0);
            assert_eq!(left, vec![BRUSH, COLOUR]);
        }
    }

    /// **Pressing a window brings it to the front**, so the one you can see is the one you get.
    ///
    /// Windows overlap and the hit test answers with whichever is drawn on top. Without this, a
    /// window can sit permanently behind another with its tabs visible and unpressable.
    #[test]
    fn pressing_a_window_brings_it_forward() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        ws.float(COLOUR);
        let (under, over) = (ws.floating[0].id, ws.floating[1].id);

        // Put them squarely on top of one another, with a sliver of the lower one showing.
        let sliver = {
            let top = ws.floating[1].rect;
            ws.floating[0].rect = Rect::new(top.x - 30.0, top.y, top.w, top.h);
            (top.x - 15.0, top.y + 4.0)
        };
        let shared = {
            let top = ws.floating[1].rect;
            (top.x + top.w / 2.0, top.y + top.h / 2.0)
        };
        assert_eq!(
            ws.window_at(shared.0, shared.1),
            Some(over),
            "the upper window should own the overlap to begin with"
        );

        // Press the sliver of the lower one, and let go without moving.
        let mut hand = Hand::new(&mut ws, screen);
        hand.press(sliver);
        hand.release(sliver);

        assert_eq!(
            ws.window_at(shared.0, shared.1),
            Some(under),
            "pressing a window should have brought it to the front"
        );
    }

    /// Float two panels and drop the first onto the second's centre, leaving one window.
    fn two_in_one_window(ws: &mut Workspace, screen: Rect) -> FloatId {
        ws.screen = screen;
        if !ws.is_floating(COLOUR) {
            ws.float(COLOUR);
        }
        if !ws.is_floating(HISTORY) {
            ws.float(HISTORY);
        }
        // Set apart before anything is grabbed. Windows land offset by a header each, so two of
        // them overlap heavily -- and a fixture that grabs a tab hidden under another window is
        // testing the hit test rather than the merge.
        ws.floating[0].rect = Rect::new(60.0, 60.0, 300.0, 300.0);
        ws.floating[1].rect = Rect::new(700.0, 60.0, 300.0, 300.0);
        let (a, b) = (ws.floating[0].id, ws.floating[1].id);
        let mut hand = Hand::new(ws, screen);
        let grab = hand.tab_of(b);
        let onto = hand.rect_of(a);
        hand.drag(grab, (onto.x + onto.w / 2.0, onto.y + onto.h / 2.0));
        assert_eq!(ws.floating.len(), 1, "they should be one window now");
        assert_eq!(ws.floating[0].layout.panels().len(), 2);
        ws.floating[0].id
    }

    /// **Tapping a tab in a merged window shows that panel.**
    ///
    /// Reported: "I drag history into colour center, but then clicking colour tab does not change
    /// to it."
    #[test]
    fn tapping_a_tab_in_a_merged_window_shows_it() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let id = two_in_one_window(&mut ws, screen);

        let m = ws.theme.metrics;
        let area = ws.area_of(Surface::Floating(id)).expect("the window");
        let slot = ws
            .layout_of(Surface::Floating(id))
            .expect("its arrangement")
            .resolve(area)
            .into_iter()
            .next()
            .expect("a leaf");
        let c = crate::chrome::panel(&slot, &m, style_of(&slot), Along::Down, |_| 46.0);
        assert_eq!(c.tabs.len(), 2, "both panels should have a tab");

        for tab in &c.tabs {
            let want = slot.tabs[tab.index];
            let at = (tab.rect.x + tab.rect.w / 2.0, tab.rect.y + tab.rect.h / 2.0);
            let mut hand = Hand::new(&mut ws, screen);
            hand.press(at);
            hand.release(at);

            let now = ws
                .layout_of(Surface::Floating(id))
                .expect("its arrangement")
                .resolve(area)
                .into_iter()
                .next()
                .expect("a leaf");
            assert_eq!(
                showing_of(&now),
                want,
                "tapping the {want:?} tab did not show it"
            );
        }
    }

    /// **Either tab can be dragged out of a merged window, and the right one comes out.**
    ///
    /// Reported: "dragging history out works, but dragging color out drags history out instead
    /// and breaks everything."
    #[test]
    fn either_tab_can_be_dragged_out_of_a_merged_window() {
        for wanted in [COLOUR, HISTORY] {
            let mut ws = bare();
            let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let id = two_in_one_window(&mut ws, screen);

            let m = ws.theme.metrics;
            let area = ws.area_of(Surface::Floating(id)).expect("the window");
            let slot = ws
                .layout_of(Surface::Floating(id))
                .expect("its arrangement")
                .resolve(area)
                .into_iter()
                .next()
                .expect("a leaf");
            let c = crate::chrome::panel(&slot, &m, style_of(&slot), Along::Down, |_| 46.0);
            let index = slot
                .tabs
                .iter()
                .position(|p| *p == wanted)
                .expect("the panel is in this window");
            let tab = c.tabs[index].rect;

            let mut hand = Hand::new(&mut ws, screen);
            hand.drag((tab.x + tab.w / 2.0, tab.y + tab.h / 2.0), (1050.0, 700.0));

            assert_eq!(
                ws.floating.len(),
                2,
                "dragging {wanted:?} out should leave two windows"
            );
            // The window under the pointer is the one that came out, and it holds what was pressed.
            let torn = ws
                .floating
                .iter()
                .find(|f| f.id != id)
                .expect("a new window");
            assert_eq!(
                torn.layout.panels(),
                vec![wanted],
                "dragging the {wanted:?} tab pulled out the wrong panel"
            );
            let left = ws
                .floating
                .iter()
                .find(|f| f.id == id)
                .expect("the old one");
            assert_eq!(left.layout.panels().len(), 1, "and one stayed behind");
            assert_ne!(left.layout.panels(), vec![wanted]);
        }
    }

    /// **Floating, merging and tearing out are all undoable**, and undo puts the panel in exactly
    /// one place.
    ///
    /// Reported: "f3 does not seem able to undo these". History only ever held the docked
    /// arrangement, so undoing a float put the panel back into the layout while leaving it
    /// floating as well -- in two places at once -- and merges and tear-offs were not recorded at
    /// all.
    #[test]
    fn every_floating_change_can_be_taken_back() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;

        let docked = ws.layout.clone();
        ws.float(COLOUR);
        assert!(ws.undo(), "floating should be undoable");
        assert_eq!(
            ws.layout, docked,
            "the panel should be back in the arrangement"
        );
        assert!(
            ws.floating.is_empty(),
            "and gone from the floating windows -- it cannot be in both"
        );
        assert!(!ws.is_floating(COLOUR));
        assert!(ws.is_docked(COLOUR));

        // Merging.
        let id = two_in_one_window(&mut ws, screen);
        let _ = id;
        assert!(ws.undo(), "merging should be undoable");
        assert_eq!(ws.floating.len(), 2, "the two windows should be back");

        // Tearing out.
        let id = two_in_one_window(&mut ws, screen);
        let m = ws.theme.metrics;
        let area = ws.area_of(Surface::Floating(id)).expect("the window");
        let slot = ws
            .layout_of(Surface::Floating(id))
            .expect("arrangement")
            .resolve(area)
            .into_iter()
            .next()
            .expect("a leaf");
        let c = crate::chrome::panel(&slot, &m, style_of(&slot), Along::Down, |_| 46.0);
        let tab = c.tabs[0].rect;
        let mut hand = Hand::new(&mut ws, screen);
        hand.drag((tab.x + tab.w / 2.0, tab.y + tab.h / 2.0), (1050.0, 700.0));
        assert_eq!(ws.floating.len(), 2);
        assert!(ws.undo(), "tearing a tab out should be undoable");
        assert_eq!(
            ws.floating.len(),
            1,
            "it should be back in the window it left"
        );
        assert_eq!(ws.floating[0].layout.panels().len(), 2);
    }

    /// **Neither direction crosses the boundary by dragging** --- including docked into floating,
    /// which nothing tested at all.
    ///
    /// A sabotage that let a docked panel drop into a floating window left the whole suite green,
    /// while a docked panel dropped on a window really did move itself into whatever docked leaf
    /// happened to lie underneath it.
    #[test]
    fn a_docked_panel_dragged_onto_a_window_stays_docked() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(COLOUR);
        ws.floating[0].rect = Rect::new(600.0, 300.0, 320.0, 320.0);
        let window = ws.floating[0].rect;
        let before_float = ws.floating.clone();
        let before_docked = ws.layout.clone();

        // The Layers tab, which is docked.
        let m = ws.theme.metrics;
        let slot = ws
            .layout
            .resolve(screen)
            .into_iter()
            .find(|p| p.tabs.contains(&LAYERS))
            .expect("layers is docked");
        let c = crate::chrome::panel(&slot, &m, style_of(&slot), Along::Down, |_| 46.0);
        let index = slot
            .tabs
            .iter()
            .position(|p| *p == LAYERS)
            .expect("its tab");
        let tab = c.tabs[index].rect;

        let mut hand = Hand::new(&mut ws, screen);
        hand.drag(
            (tab.x + tab.w / 2.0, tab.y + tab.h / 2.0),
            (window.x + window.w / 2.0, window.y + window.h / 2.0),
        );

        assert!(ws.is_docked(LAYERS), "a docked panel must stay docked");
        assert!(
            !ws.is_floating(LAYERS),
            "and must not have joined the window"
        );
        assert_eq!(
            ws.floating, before_float,
            "the floating window should be untouched"
        );
        // **And it must not have moved within the arrangement either.** Asserting only that it
        // stayed docked was too weak: the panel was still being dropped into whatever docked leaf
        // lay *behind* the window, which is not where anybody pointed. A sabotage doing exactly
        // that left this test green.
        assert_eq!(
            ws.layout, before_docked,
            "the panel was dropped into whatever lay behind the window"
        );
    }

    /// **A press on a floating window's strip moves the window, not a panel out of it.**
    ///
    /// Tested on a window holding *two*, because with one panel "move the window" and "drag the
    /// shown tab" are the same movement --- so the one-panel fixture could not tell them apart, and
    /// removing the strip rule entirely left it green.
    #[test]
    fn the_strip_of_a_shared_window_moves_it_rather_than_emptying_it() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let id = two_in_one_window(&mut ws, screen);
        let before = ws.floating[0].rect;

        let mut hand = Hand::new(&mut ws, screen);
        let at = hand.strip_of(id);
        hand.drag(at, (at.0 + 250.0, at.1 + 150.0));

        assert_eq!(
            ws.floating.len(),
            1,
            "the strip tore a panel out of the window"
        );
        assert_eq!(
            ws.floating[0].layout.panels().len(),
            2,
            "both panels should still be in it"
        );
        let after = ws.floating[0].rect;
        assert!(
            (after.x - before.x - 250.0).abs() < 1.0 && (after.y - before.y - 150.0).abs() < 1.0,
            "the window did not move: {before:?} -> {after:?}"
        );
    }

    /// **A window dropped on another's edge splits it**, rather than always landing as a tab.
    ///
    /// Only centre drops were tested, so a merge that ignored the zone entirely passed.
    #[test]
    fn a_window_dropped_on_an_edge_splits_the_one_it_lands_on() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(COLOUR);
        ws.float(HISTORY);
        ws.floating[0].rect = Rect::new(60.0, 60.0, 300.0, 300.0);
        ws.floating[1].rect = Rect::new(700.0, 60.0, 300.0, 300.0);
        let (a, b) = (ws.floating[0].id, ws.floating[1].id);

        let mut hand = Hand::new(&mut ws, screen);
        let grab = hand.tab_of(b);
        let onto = hand.rect_of(a);
        // Well into the left band, which is a split rather than a tab.
        hand.drag(grab, (onto.x + 8.0, onto.y + onto.h / 2.0));

        assert_eq!(ws.floating.len(), 1);
        let left = &ws.floating[0];
        let area = Rect::new(0.0, 0.0, 300.0, 300.0);
        assert_eq!(
            left.layout.resolve(area).len(),
            2,
            "an edge drop should have split the window, not stacked the panels as tabs"
        );
    }

    /// The window a tab was pulled out of stays exactly where it was.
    ///
    /// Nothing checked it: a tear-off that also shifted the source window left the suite green.
    #[test]
    fn tearing_a_tab_out_leaves_the_source_window_alone() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let id = two_in_one_window(&mut ws, screen);
        let before = ws.floating[0].rect;

        let mut hand = Hand::new(&mut ws, screen);
        let grab = hand.tab_of(id);
        hand.drag(grab, (1050.0, 700.0));

        let source = ws
            .floating
            .iter()
            .find(|f| f.id == id)
            .expect("the window it came from");
        assert_eq!(source.rect, before, "the source window moved");
    }

    /// **A compact panel can actually be moved**, end to end, by the bar that is its whole handle.
    ///
    /// This was checked only at the target level -- that a press there *means* the panel -- and not
    /// that the arrangement then changes.
    #[test]
    fn a_compact_panel_can_be_dragged_by_its_bar() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        let before = ws.layout.clone();

        let m = ws.theme.metrics;
        let placed = ws.layout.resolve(screen);
        let rail = placed
            .iter()
            .find(|p| p.tabs.contains(&TOOLS))
            .expect("the tool rail");
        let c = crate::chrome::panel(
            rail,
            &m,
            style_of(rail),
            along_of(rail, ws.direction_of(TOOLS)),
            |_| 0.0,
        );
        assert!(c.tabs.is_empty(), "a compact header carries no tabs");
        let at = (c.header.x + c.header.w / 2.0, c.header.y + c.header.h / 2.0);

        // Onto the far side of the canvas, which is a different leaf.
        let canvas = placed
            .iter()
            .find(|p| p.tabs.contains(&CANVAS))
            .expect("the canvas");
        let drop = (
            canvas.rect.x + canvas.rect.w / 2.0,
            canvas.rect.y + canvas.rect.h - 10.0,
        );
        let mut hand = Hand::new(&mut ws, screen);
        hand.drag(at, drop);

        assert_ne!(ws.layout, before, "the tool rail could not be moved at all");
        assert!(ws.is_docked(TOOLS), "and it should still be docked");
    }

    /// **Holding a strip asks about the leaf whose strip it is**, not the first in the window.
    ///
    /// A window can hold several leaves after an edge drop, each showing a different panel. Asking
    /// about the first meant holding one panel's strip opened another panel's settings -- and no
    /// test could see it, because every fixture used a window with a single leaf, where the first
    /// leaf *is* the one you held.
    #[test]
    fn holding_a_strip_asks_about_the_leaf_it_belongs_to() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(COLOUR);
        ws.float(HISTORY);
        ws.floating[0].rect = Rect::new(60.0, 60.0, 400.0, 400.0);
        ws.floating[1].rect = Rect::new(700.0, 60.0, 400.0, 400.0);
        let (a, b) = (ws.floating[0].id, ws.floating[1].id);

        // An edge drop, so the window ends up with two leaves side by side.
        {
            let mut hand = Hand::new(&mut ws, screen);
            let grab = hand.tab_of(b);
            let onto = hand.rect_of(a);
            hand.drag(grab, (onto.x + 8.0, onto.y + onto.h / 2.0));
        }
        assert_eq!(ws.floating.len(), 1);
        let id = ws.floating[0].id;
        let area = ws.area_of(Surface::Floating(id)).expect("the window");
        let leaves = ws
            .layout_of(Surface::Floating(id))
            .expect("its arrangement")
            .resolve(area);
        assert_eq!(leaves.len(), 2, "the edge drop should have split it");

        let m = ws.theme.metrics;
        for slot in &leaves {
            let want = showing_of(slot);
            let c = crate::chrome::panel(slot, &m, style_of(slot), Along::Down, |_| 46.0);
            let past = c.tabs.last().map_or(c.header.x, |t| t.rect.x + t.rect.w);
            let at = (
                past + crate::chrome::strip_width(&m) / 2.0,
                c.header.y + c.header.h / 2.0,
            );
            ws.popup_wanted = None;
            let mut hand = Hand::new(&mut ws, screen);
            hand.hold(at);
            assert_eq!(
                ws.popup_wanted,
                Some(PopupKind::Settings(want)),
                "holding {want:?}'s strip asked about something else"
            );
        }
    }

    /// **A window is resized by any of its eight edges and corners**, live, from the press.
    ///
    /// Driven through `input_frame`, so the hit test that decides "this is the border" is the one
    /// the application uses. A test that named `Target::Edge` itself would agree with any bug in
    /// working out where the border is, which is the whole of this feature.
    #[test]
    fn a_window_is_resized_by_its_edges_and_corners() {
        use crate::chrome::Pull;
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let start = Rect::new(400.0, 300.0, 320.0, 260.0);
        // Every combination but the middle of both, which is not an edge at all.
        for pull in [
            Pull { x: -1, y: 0 },
            Pull { x: 1, y: 0 },
            Pull { x: 0, y: -1 },
            Pull { x: 0, y: 1 },
            Pull { x: -1, y: -1 },
            Pull { x: 1, y: -1 },
            Pull { x: -1, y: 1 },
            Pull { x: 1, y: 1 },
        ] {
            let mut ws = bare();
            ws.screen = screen;
            ws.float(BRUSH);
            ws.floating[0].rect = start;

            // Exactly on the boundary the pull names, and mid-way along the other axis when that
            // one is not being pulled.
            let on = |side: i8, low: f32, size: f32| match side {
                -1 => low,
                1 => low + size,
                _ => low + size / 2.0,
            };
            let at = (on(pull.x, start.x, start.w), on(pull.y, start.y, start.h));
            let (dx, dy) = (37.0, 23.0);

            let mut hand = Hand::new(&mut ws, screen);
            hand.drag(at, (at.0 + dx, at.1 + dy));

            // **Worked out here, not asked of the function under test.** Using `pull_edges` as
            // the oracle could only ever catch a routing mistake, never an arithmetic one.
            let got = ws.floating[0].rect;
            let want = Rect::new(
                if pull.x == -1 { start.x + dx } else { start.x },
                if pull.y == -1 { start.y + dy } else { start.y },
                match pull.x {
                    -1 => start.w - dx,
                    1 => start.w + dx,
                    _ => start.w,
                },
                match pull.y {
                    -1 => start.h - dy,
                    1 => start.h + dy,
                    _ => start.h,
                },
            );
            assert_eq!(got, want, "pulling {pull:?} did not resize the window");
            // And it really is a resize, not a move: the opposite edges have not budged.
            if pull.x == 1 {
                assert!(
                    (got.x - start.x).abs() < 0.01,
                    "{pull:?} moved the left edge"
                );
            }
            if pull.y == 1 {
                assert!(
                    (got.y - start.y).abs() < 0.01,
                    "{pull:?} moved the top edge"
                );
            }
        }
    }

    /// **A window dragged past its floor and back comes out exactly as it went in.**
    ///
    /// The trap a delta-per-frame resize falls into: every frame the size is clamped, the clamp
    /// eats the difference, and the window creeps. `Preview::MovingWindow` already carries no
    /// distance for this exact reason; this holds the resize to the same promise.
    #[test]
    fn a_window_pulled_past_its_floor_and_back_is_unchanged() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        let start = Rect::new(400.0, 300.0, 320.0, 260.0);
        ws.floating[0].rect = start;

        let at = (start.x + start.w, start.y + start.h);
        let mut hand = Hand::new(&mut ws, screen);
        hand.press(at);
        // Far past the floor, in several steps, so an accumulating implementation has every
        // chance to lose the difference.
        for back in [200.0_f32, 400.0, 800.0, 1200.0] {
            hand.move_to((at.0 - back, at.1 - back));
        }
        hand.move_to(at);
        hand.release(at);

        assert_eq!(
            ws.floating[0].rect, start,
            "the window did not come back to the size it started at"
        );
    }

    /// **The smallest window is still a window**: it can be moved by its header.
    ///
    /// A resize border 13 units deep on each side leaves nothing between them on a window 28 units
    /// tall, and what is between them is the handle. A window that had eaten its own handle could
    /// be shrunk and then never moved again.
    #[test]
    fn a_window_at_its_smallest_can_still_be_moved() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        let least = least_window(&ws.theme.metrics);
        let start = Rect::new(400.0, 300.0, least, least);
        ws.floating[0].rect = start;

        // The middle of it, which is all the interior there is.
        let mid = (start.x + start.w / 2.0, start.y + start.h / 2.0);
        assert_eq!(
            crate::chrome::edge_at(start, ws.theme.metrics.splitter_grab, least, mid.0, mid.1),
            None,
            "the borders met in the middle and left no handle"
        );

        let mut hand = Hand::new(&mut ws, screen);
        hand.drag(mid, (mid.0 + 150.0, mid.1 + 90.0));
        let moved = ws.floating[0].rect;
        assert!(
            (moved.x - start.x - 150.0).abs() < 1.0 && (moved.y - start.y - 90.0).abs() < 1.0,
            "the smallest window could not be moved: {start:?} -> {moved:?}"
        );
        assert_eq!(
            (moved.w, moved.h),
            (start.w, start.h),
            "moving it resized it"
        );
    }

    /// Resizing a window is one undo step, and Escape puts it back mid-drag.
    #[test]
    fn a_window_resize_can_be_taken_back() {
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let start = Rect::new(400.0, 300.0, 320.0, 260.0);
        for escape in [false, true] {
            let mut ws = bare();
            ws.screen = screen;
            ws.float(BRUSH);
            ws.floating[0].rect = start;
            ws.history = crate::layout::History::default();

            let at = (start.x + start.w, start.y + start.h / 2.0);
            {
                let mut hand = Hand::new(&mut ws, screen);
                hand.press(at);
                hand.move_to((at.0 + 120.0, at.1));
                assert!(
                    hand.ws.floating[0].rect.w > start.w,
                    "it should be following the pointer already"
                );
            }
            let mut hand = Hand::new(&mut ws, screen);
            if escape {
                ws.cancel_drag();
                assert_eq!(
                    ws.floating[0].rect, start,
                    "escape left the window where the pointer had dragged it"
                );
                assert!(!ws.undo(), "and there is nothing to take back");
            } else {
                hand.release((at.0 + 120.0, at.1));
                assert!((ws.floating[0].rect.w - start.w - 120.0).abs() < 1.0);
                assert!(ws.undo(), "a resize should be on the undo stack");
                assert_eq!(ws.floating[0].rect, start, "undo did not put the size back");
            }
        }
    }

    /// A press that resized nothing leaves nothing to undo.
    ///
    /// The window's size is in no layout, so the divider's "did this change anything" check cannot
    /// see it: without asking the whole arrangement, an edge dragged out and back would leave an
    /// undo step that undoes nothing, and pressing undo would appear to do nothing at all.
    #[test]
    fn a_resize_that_changed_nothing_is_not_an_undo_step() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        let start = Rect::new(400.0, 300.0, 320.0, 260.0);
        ws.floating[0].rect = start;
        ws.history = crate::layout::History::default();

        let at = (start.x + start.w, start.y + start.h / 2.0);
        let mut hand = Hand::new(&mut ws, screen);
        hand.press(at);
        hand.move_to((at.0 + 200.0, at.1));
        hand.release(at);

        assert_eq!(ws.floating[0].rect, start);
        assert!(!ws.undo(), "an unchanged window left an undo step behind");
    }

    /// **A window's border reaches a little outside it**, as a divider's does.
    ///
    /// Half the grab width lies outside the rectangle, so the window that owns a point just off
    /// its edge is that window -- otherwise the outer half of every border is unreachable and the
    /// generous target is only generous on one side.
    #[test]
    fn a_windows_border_is_reachable_from_just_outside_it() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        let start = Rect::new(400.0, 300.0, 320.0, 260.0);
        ws.floating[0].rect = start;

        // Four units outside the right edge: past the rectangle, inside the border.
        let at = (start.x + start.w + 4.0, start.y + start.h / 2.0);
        assert!(
            !start.contains(at.0, at.1),
            "the probe should be outside the window"
        );
        let mut hand = Hand::new(&mut ws, screen);
        hand.drag(at, (at.0 + 60.0, at.1));
        assert!(
            (ws.floating[0].rect.w - start.w - 60.0).abs() < 1.0,
            "a press just outside the edge did not resize: {:?}",
            ws.floating[0].rect
        );
    }

    /// **A dividing line inside a window still resizes the panels, not the window.**
    ///
    /// The two live together: the edges size the window and the divider sizes what is in it. A
    /// border that swallowed the divider, or a divider that answered for the border, would take
    /// one of them away.
    #[test]
    fn a_divider_inside_a_window_still_belongs_to_the_panels() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(COLOUR);
        ws.float(HISTORY);
        ws.floating[0].rect = Rect::new(300.0, 200.0, 600.0, 500.0);
        ws.floating[1].rect = Rect::new(1000.0, 60.0, 300.0, 300.0);
        let (a, b) = (ws.floating[0].id, ws.floating[1].id);
        // An edge drop, which splits the window rather than stacking a tab on it -- a window with
        // two tabs has no divider at all, and a fixture that cannot hold one proves nothing.
        {
            let mut hand = Hand::new(&mut ws, screen);
            let grab = hand.tab_of(b);
            let onto = hand.rect_of(a);
            hand.drag(grab, (onto.x + 8.0, onto.y + onto.h / 2.0));
        }
        assert_eq!(
            ws.floating.len(),
            1,
            "the edge drop should have merged them"
        );
        let id = ws.floating[0].id;
        let before = ws.floating[0].rect;
        let area = ws.area_of(Surface::Floating(id)).expect("the window");
        let seams = ws
            .layout_of(Surface::Floating(id))
            .expect("its arrangement")
            .splitters(area, ws.theme.metrics.splitter_grab);
        let seam = seams.first().cloned().expect("a divider inside the window");
        let at = (
            seam.rect.x + seam.rect.w / 2.0,
            seam.rect.y + seam.rect.h / 2.0,
        );
        let layout_before = ws.floating[0].layout.clone();

        let mut hand = Hand::new(&mut ws, screen);
        hand.drag(at, (at.0 + 40.0, at.1 + 40.0));

        assert_eq!(
            ws.floating[0].rect, before,
            "dragging the divider resized the window"
        );
        assert_ne!(
            ws.floating[0].layout, layout_before,
            "and it should have moved the divider"
        );
    }

    /// **A pick lands where it was pointed, through the same press the application sees.**
    #[test]
    fn a_pick_lands_where_the_next_press_says() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        ws.start_placing(BRUSH);

        let slot = ws
            .layout
            .resolve(screen)
            .into_iter()
            .find(|p| p.tabs.contains(&LAYERS))
            .expect("layers");
        // Well into the left band, which is a split rather than a tab.
        let at = (slot.rect.x + 6.0, slot.rect.y + slot.rect.h / 2.0);

        let mut hand = Hand::new(&mut ws, screen);
        hand.press(at);

        assert_eq!(
            ws.placing(),
            None,
            "the press should have answered the pick"
        );
        assert!(ws.is_docked(BRUSH), "and put the panel in the arrangement");
        let (brush, _) = ws.layout.find(BRUSH).expect("brush");
        let (layers, _) = ws.layout.find(LAYERS).expect("layers");
        assert_ne!(
            brush, layers,
            "an edge press should have split the leaf, not stacked a tab on it"
        );
    }

    /// **A pick is not a drag**, and the press that answers it rearranges nothing else.
    ///
    /// The press lands on chrome as often as not -- a header, a divider -- and if the ordinary
    /// gesture machinery saw it first the workspace would be rearranged with one hand while the
    /// other was still holding a panel.
    #[test]
    fn the_press_that_answers_a_pick_does_nothing_else() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        let seams = ws.layout.splitters(screen, ws.theme.metrics.splitter_grab);
        let seam = seams.first().cloned().expect("a divider");
        ws.float(BRUSH);
        ws.start_placing(BRUSH);
        let was = ws.layout.clone();

        // Straight onto a divider, and then moved: an ordinary press there resizes live.
        let at = (
            seam.rect.x + seam.rect.w / 2.0,
            seam.rect.y + seam.rect.h / 2.0,
        );
        let mut hand = Hand::new(&mut ws, screen);
        hand.press(at);
        hand.move_to((at.0 + 90.0, at.1 + 90.0));
        hand.release((at.0 + 90.0, at.1 + 90.0));

        assert_eq!(ws.placing(), None);
        assert!(ws.is_docked(BRUSH), "the panel landed");
        // The panel is new, so the tree cannot be identical -- but nothing else moved: taking the
        // panel out again must give back exactly what was there.
        let mut back = ws.layout.clone();
        back.remove(BRUSH);
        assert_eq!(
            back, was,
            "answering the pick also dragged the divider it was answered on"
        );
    }

    /// Escape gives up on a pick and puts back exactly what it took.
    #[test]
    fn a_pick_can_always_be_called_off() {
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        ws.screen = screen;
        ws.float(COLOUR);
        let before = ws.snapshot();
        ws.start_placing(COLOUR);
        assert!(
            ws.cancel_drag(),
            "escape should report that it did something"
        );
        assert_eq!(ws.placing(), None);
        assert_eq!(ws.snapshot(), before, "escape did not put it back");
    }

    /// A pick holds the pointer, so the press that answers it cannot also draw.
    #[test]
    fn a_pick_holds_the_pointer() {
        let mut ws = bare();
        assert!(!ws.busy());
        ws.start_placing(BRUSH);
        assert!(ws.busy(), "a waiting pick must own the next press");
        ws.cancel_placing();
        assert!(!ws.busy());
    }

    /// **Removing a panel from its own settings is the same act as its switch in the list.**
    ///
    /// Two ways to reach one function, not two functions that agree today. Checked for a docked
    /// panel and a floating one, because the floating case is the one that used to fall through:
    /// the arrangement did not hold it, so removing it did nothing at all.
    #[test]
    fn removing_from_the_settings_is_the_same_as_the_switch() {
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        for afloat in [false, true] {
            let mut by_button = bare();
            by_button.screen = screen;
            let mut by_switch = bare();
            by_switch.screen = screen;
            if afloat {
                by_button.float(COLOUR);
                by_switch.float(COLOUR);
            }

            by_button.apply_popup(
                PopupKind::Settings(COLOUR),
                crate::panel_ui::Change::Pressed(REMOVE_ID),
            );
            by_switch.apply_popup(
                PopupKind::Panels,
                crate::panel_ui::Change::Toggled(COLOUR.0, false),
            );

            assert!(!by_button.is_open(COLOUR), "the button did not remove it");
            assert!(!by_switch.is_open(COLOUR), "the switch did not remove it");
            assert_eq!(
                by_button.snapshot(),
                by_switch.snapshot(),
                "the two ways of removing a panel left different workspaces (floating: {afloat})"
            );
            assert!(by_button.undo(), "removing should be undoable");
            assert!(by_button.is_open(COLOUR), "and undo should bring it back");
        }
    }

    /// Every panel offers a way out of the workspace, whatever else its settings hold.
    #[test]
    fn every_panel_can_be_removed_from_its_own_settings() {
        for k in PANELS {
            let mut ws = bare();
            ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let controls = ws.popup_controls(PopupKind::Settings(k.id));
            assert!(
                controls.iter().any(|c| c.id() == Some(REMOVE_ID)),
                "{} cannot be removed from its own settings",
                k.name
            );
            // **And pressing it removes the panel.** Offering the button and wiring it to nothing
            // would have passed the half of this that only looked at the list.
            ws.apply_popup(
                PopupKind::Settings(k.id),
                crate::panel_ui::Change::Pressed(REMOVE_ID),
            );
            assert!(!ws.is_open(k.id), "{}'s remove button did nothing", k.name);
            assert!(ws.undo(), "{}: removing should be undoable", k.name);
            assert!(ws.is_open(k.id), "{}: undo did not bring it back", k.name);
        }
    }

    /// **A floated panel reads as open**, so its switch turns it off rather than making a second.
    #[test]
    fn a_floating_panel_is_still_an_open_panel() {
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(COLOUR);
        assert!(
            ws.is_open(COLOUR),
            "a floating panel is still in the workspace"
        );
        assert!(
            panel_list_controls(&ws).iter().any(
                |c| matches!(c, crate::panel_ui::Control::Toggle { id, on, .. }
                    if *id == COLOUR.0 && *on)
            ),
            "its switch should read as on"
        );
        ws.toggle(COLOUR);
        assert!(!ws.is_open(COLOUR), "the switch should have put it away");
        assert!(!ws.is_floating(COLOUR), "and not left the window behind");
        assert_eq!(ws.placing(), None, "nor started a pick for a second copy");
    }

    /// **What is drawn over the canvas is not the canvas.**
    ///
    /// The workspace lays the canvas out as a panel among panels, so "not inside the canvas
    /// rectangle" answers for everything beside it. Floating windows and popups are the exception:
    /// they are painted on top, and by the rectangle alone the pen and the wheel were told they
    /// were over the artwork while they were plainly on a panel. Reported as scrolling a floating
    /// panel zooming the drawing behind it.
    #[test]
    fn what_is_drawn_over_the_canvas_takes_the_pointer() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.set_screen(screen);
        // The canvas is worked out while drawing, so say where it is directly.
        let canvas = Rect::new(200.0, 100.0, 800.0, 600.0);
        ws.canvas_rect = Some(canvas);

        // Beside the canvas: the workspace's. On it: the canvas's.
        assert!(ws.takes_point(50.0, 50.0));
        assert!(!ws.takes_point(600.0, 400.0));

        // Under a floating window: the window's, border and all.
        ws.float(COLOUR);
        ws.floating[0].rect = Rect::new(300.0, 200.0, 240.0, 180.0);
        let window = ws.floating[0].rect;
        assert!(
            ws.takes_point(window.x + 60.0, window.y + 60.0),
            "a floating window over the canvas did not take the pointer"
        );
        let reach = ws.theme.metrics.splitter_grab / 2.0;
        assert!(
            ws.takes_point(window.x - reach / 2.0, window.y + 60.0),
            "its resize border, which a press there resizes, did not take the pointer"
        );
        assert!(
            !ws.takes_point(window.x - reach * 3.0, window.y + 60.0),
            "and it should not reach further than the border does"
        );

        // Under the popup: the popup's.
        ws.popup = Some(Popup {
            kind: PopupKind::Panels,
            rect: Rect::new(700.0, 500.0, 120.0, 90.0),
        });
        assert!(ws.takes_point(750.0, 540.0), "the popup did not take it");

        // **And with the canvas panel closed there is no canvas anywhere.** The workspace fills
        // the window with ground, so the artwork is not on screen at all -- answering "the whole
        // surface is canvas, then" let the pen paint on a drawing nobody could see.
        ws.canvas_rect = None;
        assert!(ws.takes_point(600.0, 400.0));
        assert!(ws.takes_point(0.0, 0.0));
    }

    /// **A window you can see never loses a press to one you cannot.**
    ///
    /// The resize border straddles the window's edge, so its outer half lies over whatever is
    /// behind. Over the arrangement that is right -- the window is on top there. Over *another
    /// window* it is not: with two windows overlapping by a header's worth, pressing the lower
    /// one's tab resized the upper one, whose border was invisible and thirteen units away.
    #[test]
    fn a_visible_window_is_not_robbed_by_another_windows_border() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        ws.float(COLOUR);
        // Overlapping, upper offset down and right, as the defaults are.
        ws.floating[0].rect = Rect::new(100.0, 100.0, 300.0, 300.0);
        ws.floating[1].rect = Rect::new(140.0, 140.0, 300.0, 300.0);
        let lower = ws.floating[0].id;
        let upper = ws.floating[1].rect;

        // Squarely inside the lower window and clear of the upper's rectangle, but well within
        // the outer half of the upper's border.
        let at = (upper.x + 60.0, upper.y - 5.0);
        assert!(
            ws.floating[0].rect.contains(at.0, at.1) && !upper.contains(at.0, at.1),
            "the probe should be on the lower window and off the upper"
        );
        let before = ws.floating[1].rect;

        let mut hand = Hand::new(&mut ws, screen);
        hand.drag(at, (at.0 + 120.0, at.1 + 80.0));

        assert_eq!(
            ws.floating.iter().find(|f| f.id != lower).map(|f| f.rect),
            Some(before),
            "the upper window was resized by a press on the one in front of it"
        );
    }

    /// **A pick gives way to any change underneath it.**
    ///
    /// The panel in the air came out of an arrangement and the way back is a snapshot of that
    /// arrangement. Undo during a pick restored a tree that still held the panel, and answering
    /// the pick then put a *second* copy in -- written to disk, and only half removable, because
    /// `Layout::remove` takes out one copy and stops.
    #[test]
    fn a_pick_gives_way_to_a_change_underneath_it() {
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        for what in ["undo", "reset", "float", "remove"] {
            let mut ws = bare();
            ws.screen = screen;
            ws.hide(BRUSH);
            ws.toggle(BRUSH);
            assert_eq!(ws.placing(), Some(BRUSH), "{what}: it should be in the air");

            match what {
                "undo" => {
                    ws.undo();
                }
                "reset" => ws.reset(),
                "float" => ws.float(COLOUR),
                _ => ws.hide(LAYERS),
            }
            assert_eq!(ws.placing(), None, "{what}: the pick should have given way");

            // Whatever happened, and whatever the press then lands on, the panel is in the
            // workspace at most once -- and one `hide` is enough to take it out again. Two copies
            // made the switch in the panel list read as on after being switched off, because
            // `Layout::remove` takes out one copy and stops.
            assert!(count_of(&ws, BRUSH) <= 1, "{what}: it is in twice already");
            let mid = (screen.w / 2.0, screen.h / 2.0);
            let mut hand = Hand::new(&mut ws, screen);
            hand.press(mid);
            assert!(
                count_of(&ws, BRUSH) <= 1,
                "{what}: the press put a second copy in"
            );
            ws.hide(BRUSH);
            assert_eq!(
                count_of(&ws, BRUSH),
                0,
                "{what}: removing left a copy behind"
            );
            assert!(!ws.is_open(BRUSH), "{what}: it still reads as open");
        }
    }

    /// **A resized window keeps something on screen**, the same as a moved one.
    ///
    /// A window may hang off an edge -- that is often what you want -- but never so far that the
    /// bar you drag it by is gone. The move path was held to that and the resize path was not, so
    /// a window pinned at the right edge could have its left edge dragged right until one unit of
    /// it showed.
    #[test]
    fn a_resized_window_keeps_something_on_screen() {
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        ws.screen = screen;
        ws.float(BRUSH);
        let keep = least_window(&ws.theme.metrics);
        // Hanging off the right, as a move is allowed to leave it.
        let start = Rect::new(screen.w - keep, 300.0, 400.0, 260.0);
        ws.floating[0].rect = start;

        // Its left edge, dragged hard right, past the screen.
        let at = (start.x, start.y + start.h / 2.0);
        let mut hand = Hand::new(&mut ws, screen);
        hand.drag(at, (screen.w + 500.0, at.1));

        let got = ws.floating[0].rect;
        let on_screen = (got.x + got.w).min(screen.w) - got.x.max(screen.x);
        assert!(
            on_screen >= keep - 0.01,
            "only {on_screen} units were left on screen: {got:?}"
        );
    }

    /// Moving a window is undoable, exactly as resizing one is.
    ///
    /// The release that ends a move takes a different path from the one that ends a resize, and
    /// only the second recorded anything -- so undo worked on one and silently did nothing on the
    /// other, which is the kind of inconsistency nobody can hold in their head.
    #[test]
    fn a_window_move_can_be_taken_back() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        let start = Rect::new(300.0, 200.0, 320.0, 260.0);
        ws.floating[0].rect = start;
        ws.history = crate::layout::History::default();

        let id = ws.floating[0].id;
        let mut hand = Hand::new(&mut ws, screen);
        let grab = hand.strip_of(id);
        hand.drag(grab, (grab.0 + 200.0, grab.1 + 150.0));
        assert_ne!(ws.floating[0].rect, start, "it should have moved");

        assert!(ws.undo(), "moving a window should be on the undo stack");
        assert_eq!(ws.floating[0].rect, start, "undo did not put it back");
    }

    /// A pointer that goes missing mid-move puts the window back, as Escape does.
    ///
    /// This is the path a lost pointer actually takes -- egui reports no position at all on those
    /// frames -- so the restore has to be here as well as on the `Lost` pulse, or a window dragged
    /// and then abandoned kept its new place while everything else rolled back around it.
    #[test]
    fn a_lost_pointer_puts_a_moved_window_back() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        let start = Rect::new(300.0, 200.0, 320.0, 260.0);
        ws.floating[0].rect = start;

        let id = ws.floating[0].id;
        let mut hand = Hand::new(&mut ws, screen);
        let grab = hand.strip_of(id);
        hand.press(grab);
        hand.move_to((grab.0 + 200.0, grab.1 + 150.0));
        assert_ne!(
            hand.ws.floating[0].rect, start,
            "it should be following the pointer"
        );
        hand.gone();

        assert_eq!(
            ws.floating[0].rect, start,
            "the window kept the place a lost gesture had dragged it to"
        );
    }

    /// **A header beats the border where they meet**, exactly as it beats a divider.
    ///
    /// Both targets are generous and so they overlap, and the one you can see wins. Taking the
    /// border first ate eleven of a compact header's eighteen units, leaving a handle well under
    /// the 4 mm floor every other grab surface is held to.
    #[test]
    fn a_header_beats_the_border_where_they_meet() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        // The tool rail, whose header is compact and therefore shallowest.
        ws.float(TOOLS);
        let start = Rect::new(400.0, 300.0, 320.0, 260.0);
        ws.floating[0].rect = start;

        let m = ws.theme.metrics;
        let area = ws
            .area_of(Surface::Floating(ws.floating[0].id))
            .expect("it");
        let slot = ws
            .layout_of(Surface::Floating(ws.floating[0].id))
            .expect("its arrangement")
            .resolve(area)
            .into_iter()
            .next()
            .expect("a leaf");
        let c = crate::chrome::panel(
            &slot,
            &m,
            style_of(&slot),
            along_of(&slot, ws.direction_of(TOOLS)),
            |_| 0.0,
        );
        // Every part of the header moves the window; none of it resizes it. A fresh workspace
        // each time, because a move changes where the header is for the next probe.
        for frac in [0.05_f32, 0.25, 0.5, 0.75, 0.95] {
            let at = (
                frac.mul_add(c.header.w, c.header.x),
                frac.mul_add(c.header.h, c.header.y),
            );
            let mut fresh = bare();
            fresh.screen = screen;
            fresh.float(TOOLS);
            fresh.floating[0].rect = start;
            let mut hand = Hand::new(&mut fresh, screen);
            hand.drag(at, (at.0 + 90.0, at.1 + 60.0));
            let got = fresh.floating[0].rect;
            assert_eq!(
                (got.w, got.h),
                (start.w, start.h),
                "the header resized the window at {frac} of the way along"
            );
        }
        // And the border outside it still resizes.
        let mut hand = Hand::new(&mut ws, screen);
        hand.drag(
            (start.x + start.w / 2.0, start.y - 4.0),
            (start.x + start.w / 2.0, start.y - 60.0),
        );
        assert!(
            ws.floating[0].rect.h > start.h,
            "the top border outside the window did not resize it"
        );
    }

    /// **A release that merges nothing records nothing.**
    ///
    /// The border reaches past a window, so a release there can find the window and no leaf in it.
    /// Recording before the guard left an undo step for a change that never happened, and pressing
    /// undo afterwards appeared to do nothing at all.
    #[test]
    fn a_merge_that_did_not_happen_is_not_an_undo_step() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(COLOUR);
        ws.float(HISTORY);
        ws.floating[0].rect = Rect::new(60.0, 60.0, 300.0, 300.0);
        ws.floating[1].rect = Rect::new(700.0, 60.0, 300.0, 300.0);
        let a = ws.floating[0].id;
        let before = ws.snapshot();
        ws.history = crate::layout::History::default();

        // Into the ring just outside the other window: near enough to be its business, not near
        // enough to be inside any of its panels.
        let onto = ws.rect_of(Surface::Floating(a)).expect("the window");
        let b = ws.floating[1].id;
        let mut hand = Hand::new(&mut ws, screen);
        let grab = hand.tab_of(b);
        hand.drag(grab, (onto.x - 3.0, onto.y + onto.h / 2.0));

        assert_eq!(ws.floating.len(), 2, "nothing should have merged");
        assert!(ws.undo(), "the window did move, so that is one step");
        assert_eq!(
            ws.snapshot(),
            before,
            "one undo should have been the whole of it"
        );
        assert!(!ws.undo(), "and there should be nothing else behind it");
    }

    /// **Pulling a tab out of a window is one undo step, not two.**
    ///
    /// The panel comes loose part-way through the drag and lands when it is let go; recording at
    /// both moments meant pressing undo twice to get back to where you started -- a workspace that
    /// remembers how it was implemented rather than what the artist did.
    #[test]
    fn tearing_a_tab_out_is_one_step() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let id = two_in_one_window(&mut ws, screen);
        let before = ws.snapshot();
        ws.history = crate::layout::History::default();

        let mut hand = Hand::new(&mut ws, screen);
        let grab = hand.tab_of(id);
        hand.drag(grab, (1050.0, 700.0));
        assert_eq!(ws.floating.len(), 2, "it should have its own window now");

        assert!(ws.undo(), "there should be something to take back");
        assert_eq!(
            ws.snapshot(),
            before,
            "one undo should have been the whole of it"
        );
        assert!(!ws.undo(), "and there should be nothing else behind it");
    }

    /// **Sizing a window and letting go over another one does not merge them.**
    ///
    /// A carry ends wherever the pointer is, because the window went with it. A *resize* does not:
    /// the window stays where it is -- or shrinks away -- while the pointer travels, so the pointer
    /// can leave it and end up over a neighbour. The release then re-parented every panel of the
    /// window being sized into that neighbour and took the window down.
    ///
    /// Two shapes, because they leave the pointer behind for different reasons: an edge dragged
    /// *inwards*, which shrinks the window away from the pointer, and a divider inside a window,
    /// which does not move the window at all.
    #[test]
    fn sizing_a_window_over_another_does_not_merge_them() {
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        for divider in [false, true] {
            let mut ws = bare();
            ws.set_screen(screen);
            ws.float(BRUSH);
            ws.float(COLOUR);
            ws.floating[0].rect = Rect::new(700.0, 200.0, 400.0, 400.0);
            ws.floating[1].rect = Rect::new(60.0, 200.0, 400.0, 400.0);
            let (right, left) = (ws.floating[0].id, ws.floating[1].id);

            if divider {
                // Split the right window, so it has a divider of its own. Its third panel comes
                // from the arrangement, floated for the purpose.
                ws.float(HISTORY);
                ws.floating[2].rect = Rect::new(700.0, 700.0, 200.0, 150.0);
                let spare = ws.floating[2].id;
                let mut hand = Hand::new(&mut ws, screen);
                let grab = hand.tab_of(spare);
                let onto = hand.rect_of(right);
                hand.drag(grab, (onto.x + 8.0, onto.y + onto.h / 2.0));
                assert_eq!(
                    ws.floating.len(),
                    2,
                    "the edge drop should have merged them"
                );
            }

            let onto = ws
                .rect_of(Surface::Floating(left))
                .expect("the other window");
            let mid = (onto.x + onto.w / 2.0, onto.y + onto.h / 2.0);
            let before = ws.rect_of(Surface::Floating(right)).expect("it");

            let at = if divider {
                let area = ws.area_of(Surface::Floating(right)).expect("the window");
                let seam = ws
                    .layout_of(Surface::Floating(right))
                    .expect("its arrangement")
                    .splitters(area, ws.theme.metrics.splitter_grab)
                    .first()
                    .cloned()
                    .expect("a divider inside it");
                (
                    seam.rect.x + seam.rect.w / 2.0,
                    seam.rect.y + seam.rect.h / 2.0,
                )
            } else {
                // Its *right* edge, dragged left: the window shrinks away from the pointer rather
                // than following it, so the pointer ends up somewhere the window is not.
                (before.x + before.w, before.y + before.h / 2.0)
            };

            let mut hand = Hand::new(&mut ws, screen);
            hand.drag(at, mid);

            assert_eq!(
                ws.floating.len(),
                2,
                "sizing a window over another merged them (divider: {divider})"
            );
            assert!(
                ws.floating.iter().any(|f| f.id == right),
                "the window being sized was destroyed (divider: {divider})"
            );
            assert!(ws.floating.iter().any(|f| f.id == left));
            if divider {
                assert_eq!(
                    ws.rect_of(Surface::Floating(right)),
                    Some(before),
                    "dragging its divider resized the window"
                );
            }
        }
    }

    /// **Undo then redo during a pick does not leave the panel nowhere.**
    ///
    /// `undo` used to sample the present *before* the pick gave way -- and during a pick the
    /// present is the arrangement with the picked panel taken out, a state the artist never
    /// committed. Redoing went straight back into it, with the panel in neither tree and no pick to
    /// put it anywhere.
    #[test]
    fn undo_and_redo_during_a_pick_lose_nothing() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        let whole = ws.snapshot();

        ws.hide(LAYERS);
        let without = ws.snapshot();
        ws.start_placing(BRUSH);

        assert!(ws.undo(), "there is a change to take back");
        assert_eq!(ws.placing(), None, "the pick should have given way");
        assert_eq!(ws.snapshot(), whole, "undo did not go back to where it was");

        assert!(ws.redo(), "and forward again");
        assert_eq!(
            ws.snapshot(),
            without,
            "redo landed in a state that was never committed"
        );
        assert!(ws.is_open(BRUSH), "the panel was left in neither tree");
    }

    /// **A panel that cannot float is refused, rather than floated into uselessness.**
    ///
    /// Not a rule about canvases: the canvas is drawn by the GPU *underneath* egui, so a window's
    /// own background is painted straight over the artwork. A floating canvas is a window with
    /// nothing in it and a workspace with no drawing -- and, with no *Float* in its settings, no
    /// way out of it either.
    #[test]
    fn a_panel_that_cannot_float_is_refused() {
        let mut ws = bare();
        ws.set_screen(Rect::new(0.0, 0.0, 1400.0, 900.0));

        ws.float(CANVAS);
        assert!(!ws.is_floating(CANVAS), "floating it was not refused");
        assert!(ws.is_docked(CANVAS), "and it stayed where it was");
        assert!(
            !ws.popup_controls(PopupKind::Settings(CANVAS))
                .iter()
                .any(|c| c.id() == Some(FLOAT_ID)),
            "nor should it be offered"
        );

        // And one that can float still does, so this is not simply refusing everything.
        ws.float(LAYERS);
        assert!(ws.is_floating(LAYERS));
    }

    /// **Visible chrome beats a window's invisible outer border.**
    ///
    /// The border straddles the window's edge, so its outer half lies over whatever is behind. Over
    /// the canvas that is right -- the window is on top there. Over a docked panel's *tab* it is
    /// not: the tab is what you can see, and it was becoming unpressable within thirteen units of
    /// a window nobody was aiming at.
    #[test]
    fn a_docked_tab_beats_a_windows_outer_border() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;

        // A docked tab, and where its middle is.
        let m = ws.theme.metrics;
        let slot = ws
            .layout
            .resolve(screen)
            .into_iter()
            .find(|p| p.tabs.contains(&LAYERS))
            .expect("layers is docked");
        let c = crate::chrome::panel(&slot, &m, style_of(&slot), Along::Down, |_| 46.0);
        let index = slot
            .tabs
            .iter()
            .position(|p| *p == LAYERS)
            .expect("its tab");
        let tab = c.tabs[index].rect;
        let at = (tab.x + tab.w / 2.0, tab.y + tab.h / 2.0);

        // A window whose top edge sits just below that tab, so its border reaches up over it.
        ws.float(BRUSH);
        let window = Rect::new(tab.x - 40.0, at.1 + 8.0, 300.0, 300.0);
        ws.floating[0].rect = window;
        assert!(
            !window.contains(at.0, at.1),
            "the tab should be outside the window"
        );
        let before = ws.floating[0].rect;

        let mut hand = Hand::new(&mut ws, screen);
        hand.press(at);
        hand.release(at);

        assert_eq!(
            ws.floating[0].rect, before,
            "pressing a docked tab resized the window whose border reached over it"
        );
        assert_eq!(
            showing_of(
                &ws.layout
                    .resolve(screen)
                    .into_iter()
                    .find(|p| p.tabs.contains(&LAYERS))
                    .expect("layers")
            ),
            LAYERS,
            "and the tap should have shown that tab"
        );
    }

    /// A window carried by its tab keeps the position it was let go at.
    ///
    /// The release carries a position of its own and it is the last one there is. The frame and the
    /// edge were given it; a tab, which carries the window just as surely, was not.
    #[test]
    fn a_window_carried_by_its_tab_lands_where_it_was_let_go() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        ws.floating[0].rect = Rect::new(200.0, 200.0, 300.0, 300.0);
        let id = ws.floating[0].id;

        let mut hand = Hand::new(&mut ws, screen);
        let grab = hand.tab_of(id);
        hand.drag_letting_go_at(grab, (700.0, 600.0), (900.0, 300.0));

        let got = ws.floating[0].rect;
        // The grip is kept, so the point that took hold of it is still under the pointer.
        let held = (grab.0 - 200.0, grab.1 - 200.0);
        assert!(
            (got.x + held.0 - 900.0).abs() < 1.0 && (got.y + held.1 - 300.0).abs() < 1.0,
            "the window stopped where the last move frame was, not where it was let go: {got:?}"
        );
    }

    /// **A window is brought back within reach when the screen shrinks under it.**
    ///
    /// Moving one is clamped and sizing one is clamped, but the screen can move instead of the
    /// window: park one near the right edge of a wide display and open the file on a narrow one.
    #[test]
    fn a_window_is_brought_back_when_the_screen_shrinks() {
        let mut ws = bare();
        let wide = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        ws.screen = wide;
        ws.float(BRUSH);
        ws.floating[0].rect = Rect::new(2300.0, 1200.0, 300.0, 200.0);

        let narrow = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut hand = Hand::new(&mut ws, narrow);
        hand.frame((10.0, 10.0), false, false, false);

        let keep = least_window(&ws.theme.metrics);
        let got = ws.floating[0].rect;
        assert!(
            got.x <= narrow.w - keep && got.y <= narrow.h - keep && got.y >= narrow.y,
            "the window was stranded off the smaller screen: {got:?}"
        );
    }

    /// **Every tier of the stack is above the one below, and no two share an order.**
    ///
    /// egui paints its layers in `Order` sequence, but *within* one order the layers that are not
    /// `Area`s come out in whatever order their map happens to iterate. Two things on one order are
    /// therefore two things whose stacking is undefined -- which is what put a docked panel in
    /// front of a floating window, and the canvas's own overlays in front of every window.
    #[test]
    fn every_tier_of_the_stack_is_above_the_one_below() {
        let tiers = [
            ("the ground", stack::GROUND),
            ("docked panels", stack::PANELS),
            ("what the canvas draws", stack::ARTWORK),
            ("floating windows", stack::WINDOWS),
            ("a gesture's marks", stack::MARKS),
            ("the popup", stack::POPUP),
        ];
        for pair in tiers.windows(2) {
            let ((below, under), (above, over)) = (pair[0], pair[1]);
            assert!(
                under < over,
                "{above} is not above {below}: {over:?} vs {under:?}"
            );
        }
    }

    /// **A divider is marked from the moment it is touched until the moment it is let go.**
    ///
    /// It had three appearances where it has two. The press put it into the hold animation, which
    /// starts at no alpha, so it went blue on hover, blank for the first frames of the press, blue
    /// again once it moved -- and then, once the hold had run, blank again, because a divider being
    /// held perfectly still reports nothing to draw. Holding still for a third of a second is the
    /// one thing that should not change anything.
    ///
    /// Driven through `input_frame`, over a hold far longer than any timer in the code, because
    /// "it goes out after a while" is a thing only a clock can show.
    #[test]
    fn a_divider_stays_marked_however_long_it_is_held() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.set_screen(screen);
        let m = ws.theme.metrics;
        let seam = ws
            .layout
            .splitters(screen, m.splitter_grab)
            .first()
            .cloned()
            .expect("a divider");
        let at = (
            seam.rect.x + seam.rect.w / 2.0,
            seam.rect.y + seam.rect.h / 2.0,
        );
        let want = Seam::Divider {
            path: seam.path.clone(),
            index: seam.index,
        };
        let look =
            |ws: &Workspace, seams: &[crate::layout::Splitter]| ws.seam_mark(seams, Some(at));
        let seams = ws.layout.splitters(screen, m.splitter_grab);

        // Hovering it: available.
        {
            let mut hand = Hand::new(&mut ws, screen);
            hand.move_to(at);
        }
        assert_eq!(
            look(&ws, &seams),
            Some((want.clone(), false)),
            "hovering a divider showed nothing"
        );

        // Pressed, and then held still for far longer than the hold: taken, throughout.
        let mut hand = Hand::new(&mut ws, screen);
        hand.press(at);
        assert_eq!(
            look(hand.ws, &seams),
            Some((want.clone(), true)),
            "the mark did not come on when it was pressed"
        );
        for _ in 0..4 {
            hand.wait(crate::panel_drag::HOLD_MS);
            assert_eq!(
                look(hand.ws, &seams),
                Some((want.clone(), true)),
                "the mark went out while it was still being held"
            );
        }
        // And moving it, still taken.
        hand.move_to((at.0 + 40.0, at.1));
        assert_eq!(look(hand.ws, &seams), Some((want.clone(), true)));

        // Let go: no longer taken. Whether it is still *available* depends only on where the
        // pointer now is, which is what it should depend on.
        hand.release((at.0 + 40.0, at.1));
        let after = ws.layout.splitters(screen, m.splitter_grab);
        assert!(
            !matches!(look(&ws, &after), Some((_, true))),
            "it stayed taken after it was let go"
        );
    }

    /// The same promise for a window's edge, which is a divider with the screen on its far side.
    #[test]
    fn a_window_edge_stays_marked_however_long_it_is_held() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.set_screen(screen);
        ws.float(BRUSH);
        let window = Rect::new(400.0, 300.0, 320.0, 260.0);
        ws.floating[0].rect = window;
        let at = (window.x + window.w, window.y + window.h / 2.0);
        let pull = crate::chrome::Pull { x: 1, y: 0 };
        let look = |ws: &Workspace| ws.seam_mark(&[], Some(at));

        // Hovering the edge, before anything is pressed. A frame first, because which window the
        // pointer is over is worked out by the same entry point everything else goes through.
        {
            let mut hand = Hand::new(&mut ws, screen);
            hand.move_to(at);
        }
        assert_eq!(
            look(&ws),
            Some((Seam::Edge(pull), false)),
            "hovering a window's edge showed nothing"
        );

        let mut hand = Hand::new(&mut ws, screen);
        hand.press(at);
        assert_eq!(
            look(hand.ws),
            Some((Seam::Edge(pull), true)),
            "the mark did not come on when the edge was pressed"
        );
        for _ in 0..4 {
            hand.wait(crate::panel_drag::HOLD_MS);
            assert_eq!(
                look(hand.ws),
                Some((Seam::Edge(pull), true)),
                "the mark went out while the edge was still being held"
            );
        }
        hand.release(at);
        assert!(!matches!(look(&ws), Some((_, true))), "it stayed taken");
    }

    /// Some other gesture owning the pointer means no hint about one it is not making.
    #[test]
    fn a_seam_shows_nothing_while_another_gesture_owns_the_pointer() {
        let m = crate::theme::Theme::default().metrics;
        let window = Rect::new(400.0, 300.0, 320.0, 260.0);
        let at = Some((window.x + window.w, window.y + window.h / 2.0));
        for held in [
            Held::Frame,
            Held::Panel {
                path: vec![],
                tab: 0,
            },
        ] {
            assert_eq!(
                seam_in_use(Some(&held), &[], Some(window), at, &m),
                None,
                "a hint appeared while {held:?} was being carried"
            );
        }
        // And away from every edge, with nothing held at all.
        let middle = Some((window.x + window.w / 2.0, window.y + window.h / 2.0));
        assert_eq!(seam_in_use(None, &[], Some(window), middle, &m), None);
    }

    /// **One window at a time.** After moving one, moving another must move *that* one.
    ///
    /// Reported as "sometimes after moving a tab, then trying to move another tab moves the first
    /// tab instead", which is what a stale surface or a hit test that answers with the wrong
    /// window looks like.
    #[test]
    fn moving_one_window_then_another_moves_the_second() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        ws.float(COLOUR);
        let (a, b) = (ws.floating[0].id, ws.floating[1].id);

        let mut hand = Hand::new(&mut ws, screen);
        // Move the first one well clear, so the two do not overlap.
        let grab = hand.tab_of(a);
        hand.drag(grab, (900.0, 600.0));
        let a_after = hand.rect_of(a);

        // Now the second.
        let before_b = hand.rect_of(b);
        let grab = hand.tab_of(b);
        hand.drag(grab, (300.0, 700.0));

        assert_eq!(
            hand.rect_of(a),
            a_after,
            "moving the second window moved the first"
        );
        // Where the grip says it should be, not merely nearby: a loose tolerance here would let
        // a grip regression of a header and a half through.
        let moved = hand.rect_of(b);
        assert!(
            (moved.y + (grab.1 - before_b.y) - 700.0).abs() < 1.0
                && (moved.x + (grab.0 - before_b.x) - 300.0).abs() < 1.0,
            "the second window did not go where it was put: {moved:?}"
        );
    }

    /// **Dragging a tab out of a window with two panels gives it its own window.**
    #[test]
    fn a_tab_dragged_out_of_a_window_gets_its_own() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        ws.float(COLOUR);
        // Put them together first, by dropping one on the other.
        {
            let (a, b) = (ws.floating[0].id, ws.floating[1].id);
            let mut hand = Hand::new(&mut ws, screen);
            let grab = hand.tab_of(a);
            let onto = hand.rect_of(b);
            hand.drag(grab, (onto.x + onto.w / 2.0, onto.y + onto.h / 2.0));
        }
        assert_eq!(ws.floating.len(), 1, "they should be one window now");
        assert_eq!(ws.floating[0].layout.panels().len(), 2);

        // Now drag one tab out of it.
        let id = ws.floating[0].id;
        let mut hand = Hand::new(&mut ws, screen);
        let grab = hand.tab_of(id);
        hand.drag(grab, (1000.0, 700.0));

        assert_eq!(ws.floating.len(), 2, "the tab should have its own window");
        for held in &ws.floating {
            assert_eq!(
                held.layout.panels().len(),
                1,
                "each window should hold one panel now"
            );
        }
    }

    /// **Dragging a docked panel's strip does nothing at all.**
    ///
    /// Reported as "clicking part that is not the button still moves and gets settings", which it
    /// did: the bar handed the press to whichever panel happened to be on show.
    #[test]
    fn the_strip_of_a_docked_panel_does_nothing() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        let before = ws.layout.clone();
        let m = ws.theme.metrics;

        let slot = ws
            .layout
            .resolve(screen)
            .into_iter()
            .find(|p| p.tabs.contains(&LAYERS))
            .expect("layers");
        let c = crate::chrome::panel(&slot, &m, style_of(&slot), Along::Down, |_| 46.0);
        let past = c.tabs.last().expect("tabs").rect;
        let at = (
            past.x + past.w + crate::chrome::strip_width(&m) / 2.0,
            c.header.y + c.header.h / 2.0,
        );

        let mut hand = Hand::new(&mut ws, screen);
        hand.drag(at, (700.0, 600.0));
        assert_eq!(ws.layout, before, "the strip moved a docked panel");
        assert!(ws.floating.is_empty(), "and it floated one");

        // Nor does holding it ask for anything.
        let mut hand = Hand::new(&mut ws, screen);
        hand.hold(at);
        assert_eq!(ws.popup_wanted, None, "the strip asked for settings");
    }

    /// A floating window's strip *does* move it, and holding it asks for its settings.
    #[test]
    fn the_strip_of_a_floating_window_moves_it() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.screen = screen;
        ws.float(BRUSH);
        let id = ws.floating[0].id;

        let mut hand = Hand::new(&mut ws, screen);
        let at = hand.strip_of(id);
        let before = hand.rect_of(id);
        hand.drag(at, (at.0 + 300.0, at.1 + 200.0));
        let after = hand.rect_of(id);
        assert!(
            (after.x - before.x - 300.0).abs() < 1.0 && (after.y - before.y - 200.0).abs() < 1.0,
            "the strip should move the window: {before:?} -> {after:?}"
        );

        let mut hand = Hand::new(&mut ws, screen);
        let at = hand.strip_of(id);
        hand.hold(at);
        assert_eq!(ws.popup_wanted, Some(PopupKind::Settings(BRUSH)));
    }

    /// **A floating window has a tab you can see, in the place you press it.**
    ///
    /// The complaint that started this: a floating panel had no button-looking thing, so there was
    /// nothing to aim at and no way to tell where holding would work. It has the same chrome as a
    /// docked one now, from the same code, because the window holds an arrangement rather than a
    /// single panel.
    #[test]
    fn a_floating_window_has_a_tab_like_any_other() {
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(BRUSH);
        let held = ws.floating.first().expect("a window").clone();

        let m = ws.theme.metrics;
        let inner = inset(held.rect, m.gutter);
        let slot = held
            .layout
            .resolve(inner)
            .into_iter()
            .next()
            .expect("a leaf");
        let c = crate::chrome::panel(
            &slot,
            &m,
            style_of(&slot),
            along_of(&slot, ws.direction_of(BRUSH)),
            |_| 40.0,
        );
        assert_eq!(c.tabs.len(), 1, "a floating panel should have a tab");
        let tab = &c.tabs[0];
        assert!(
            tab.rect.w > 0.0 && tab.rect.h > 0.0,
            "and it must be something you can see"
        );
        assert!(
            held.rect.contains(tab.rect.x + 1.0, tab.rect.y + 1.0),
            "the tab should be inside the window it belongs to"
        );
    }

    /// **Holding a floating panel's tab asks for its settings**, while the finger is down.
    ///
    /// It never fired before, and the reason is worth keeping: painting is demand-driven, so a
    /// hold with the pointer still produces no frames and the timer is never read. The docked
    /// panels asked for those frames; the floating ones had their own gesture code and did not
    /// inherit it. One code path, one answer.
    #[test]
    fn holding_a_floating_tab_asks_for_its_settings() {
        use crate::panel_drag::{pulse, Target, HOLD_MS};
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(BRUSH);
        let id = ws.floating[0].id;
        let area = ws.area_of(Surface::Floating(id)).expect("the window");
        let slot = ws
            .layout_of(Surface::Floating(id))
            .expect("its arrangement")
            .resolve(area)
            .into_iter()
            .next()
            .expect("a leaf");
        let target = Target::Tab {
            path: slot.path.clone(),
            tab: 0,
        };
        let at = (slot.rect.x + 8.0, slot.rect.y + 6.0);

        ws.grab_surface = Surface::Floating(id);
        ws.gesture(
            area,
            pulse(true, false, true),
            Some(at),
            0.0,
            Surface::Floating(id),
            |_, _| target.clone(),
        );
        // The frames a demand-driven UI would otherwise never draw.
        assert!(
            ws.drag.waiting_ms(1.0).is_some(),
            "a floating panel's hold must ask for the frames that let it fire"
        );
        ws.gesture(
            area,
            pulse(false, false, true),
            Some(at),
            HOLD_MS + 1.0,
            Surface::Floating(id),
            |_, _| unreachable!(),
        );
        assert_eq!(
            ws.popup_wanted,
            Some(PopupKind::Settings(BRUSH)),
            "holding a floating panel's tab should ask it what it offers"
        );
    }

    /// **A floating window dragged onto another goes into it**, at the zone it was dropped on.
    ///
    /// This is why a window holds an arrangement rather than a single panel: without it, windows
    /// could only ever hold one thing each and there would be nothing to drop onto.
    #[test]
    fn a_panel_can_be_dragged_from_one_window_into_another() {
        use crate::panel_drag::{pulse, Target};
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(BRUSH);
        ws.float(COLOUR);
        let (from, to) = (ws.floating[0].id, ws.floating[1].id);
        let source = ws.area_of(Surface::Floating(from)).expect("source");
        let target = Target::Tab {
            path: vec![],
            tab: 0,
        };

        ws.grab_surface = Surface::Floating(from);
        ws.gesture(
            source,
            pulse(true, false, true),
            Some((source.x + 8.0, source.y + 6.0)),
            0.0,
            Surface::Floating(from),
            |_, _| target.clone(),
        );
        // Carried into the middle of the other window and let go.
        let dest = ws.area_of(Surface::Floating(to)).expect("destination");
        let drop = (dest.x + dest.w / 2.0, dest.y + dest.h / 2.0);
        ws.gesture(
            source,
            pulse(false, false, true),
            Some(drop),
            1.0,
            Surface::Floating(to),
            |_, _| unreachable!(),
        );
        ws.gesture(
            source,
            pulse(false, true, false),
            Some(drop),
            2.0,
            Surface::Floating(to),
            |_, _| unreachable!(),
        );

        assert_eq!(ws.floating.len(), 1, "the emptied window should be gone");
        let left = &ws.floating[0];
        let mut in_it = left.layout.panels();
        in_it.sort_by_key(|p| p.0);
        assert_eq!(
            in_it,
            vec![BRUSH, COLOUR],
            "both should be in the one window"
        );
    }

    /// **A hold marks the tab, not the bar it sits in.**
    ///
    /// Tinting the whole header made a strip of tabs look like one control, which is exactly what
    /// pressing one appeared to do.
    #[test]
    fn a_hold_marks_the_tab_it_is_on() {
        let m = crate::theme::Theme::default().metrics;
        let mut l = Layout::single(LAYERS);
        l.insert(&[], Zone::Center, HISTORY);
        let placed = l.resolve(Rect::new(0.0, 0.0, 800.0, 600.0));
        let slot = placed.first().expect("a leaf");
        let c = crate::chrome::panel(slot, &m, HeaderStyle::Named, Along::Down, |_| 40.0);
        assert_eq!(c.tabs.len(), 2);

        for tab in &c.tabs {
            let at = hold_mark(&c, tab.index);
            assert_eq!(at, tab.rect, "tab {} was not the thing marked", tab.index);
            assert!(at.w < c.header.w, "it marked the whole bar");
        }

        // A compact header carries no tabs and is itself the handle, so it takes the mark.
        let alone = Layout::single(TOOLS);
        let placed = alone.resolve(Rect::new(0.0, 0.0, 90.0, 600.0));
        let slot = placed.first().expect("a leaf");
        let c = crate::chrome::panel(slot, &m, HeaderStyle::Compact, Along::Down, |_| 0.0);
        assert!(c.tabs.is_empty());
        assert_eq!(hold_mark(&c, 0), c.header);
    }

    /// **Carrying something in a floating window moves the window.**
    ///
    /// A floating panel is a window, and a window that could not be moved by the thing you
    /// naturally take hold of could not be moved at all.
    #[test]
    fn carrying_in_a_window_moves_the_window() {
        let carrying = Preview::Carrying {
            panel: BRUSH,
            over: None,
        };
        assert!(carry_moves_window(
            Surface::Floating(FloatId(0)),
            Some(&carrying)
        ));
        // In the arrangement, carrying means what it always did: the panel is looking for a home.
        assert!(!carry_moves_window(Surface::Docked, Some(&carrying)));
        // And nothing else moves a window.
        assert!(!carry_moves_window(
            Surface::Floating(FloatId(0)),
            Some(&Preview::Resizing {
                path: vec![],
                index: 0
            })
        ));
        assert!(!carry_moves_window(Surface::Floating(FloatId(0)), None));
    }

    /// **Floating and docked never mix by dragging.** They are two modes, and the way between
    /// them is the panel's own settings.
    ///
    /// A floating window dropped on the arrangement would otherwise dock itself the moment it was
    /// moved anywhere useful, which makes moving one impossible -- and that is exactly what
    /// happened when a drag was allowed to cross.
    #[test]
    fn dragging_never_crosses_between_floating_and_docked() {
        use crate::panel_drag::{pulse, Target};
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(BRUSH);
        let id = ws.floating[0].id;
        let source = ws.area_of(Surface::Floating(id)).expect("the window");
        let target = Target::Tab {
            path: vec![],
            tab: 0,
        };

        // Dragged right into the middle of the docked arrangement and let go.
        let onto = ws.layout.resolve(ws.screen);
        let canvas = onto
            .iter()
            .find(|p| p.tabs.contains(&CANVAS))
            .expect("the canvas");
        let drop = (
            canvas.rect.x + canvas.rect.w / 2.0,
            canvas.rect.y + canvas.rect.h / 2.0,
        );

        ws.grab_surface = Surface::Floating(id);
        ws.gesture(
            source,
            pulse(true, false, true),
            Some((source.x + 8.0, source.y + 6.0)),
            0.0,
            Surface::Floating(id),
            |_, _| target.clone(),
        );
        ws.gesture(
            source,
            pulse(false, false, true),
            Some(drop),
            1.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        ws.gesture(
            source,
            pulse(false, true, false),
            Some(drop),
            2.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );

        assert!(
            ws.is_floating(BRUSH),
            "a floating window dropped on the arrangement should stay floating"
        );
        assert!(!ws.is_docked(BRUSH), "and must not have docked itself");
        // It went where it was dropped, which is what dragging one is *for*.
        let moved = ws.floating[0].rect;
        assert!(
            (moved.x - source.x).abs() > 1.0 || (moved.y - source.y).abs() > 1.0,
            "the window did not move: it was dragged across the whole screen"
        );
    }

    /// **A floating window follows the pointer**, keeping the place it was taken hold of.
    ///
    /// Without the grip the window's corner jumps to the pointer the moment it moves, which reads
    /// as having thrown it rather than picked it up.
    #[test]
    fn a_dragged_window_follows_the_pointer_by_its_grip() {
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(BRUSH);
        let id = ws.floating[0].id;
        let at = ws.floating[0].rect;

        // Taken hold of forty units in from its corner.
        let grabbed = (at.x + 40.0, at.y + 10.0);
        ws.take_grip(Surface::Floating(id), grabbed);
        ws.drag_window_to(
            Surface::Floating(id),
            (grabbed.0 + 200.0, grabbed.1 + 120.0),
        );

        let now = ws.floating[0].rect;
        assert!(
            (now.x - (at.x + 200.0)).abs() < 0.001 && (now.y - (at.y + 120.0)).abs() < 0.001,
            "it should have moved by what the pointer did, not jumped to it: {now:?}"
        );
        assert!(
            (now.w - at.w).abs() < 0.001 && (now.h - at.h).abs() < 0.001,
            "and kept its size"
        );
    }

    /// A tap on a floating window's tab is not a drag, and merges nothing.
    #[test]
    fn tapping_a_floating_tab_does_not_move_it_anywhere() {
        use crate::panel_drag::{pulse, Target};
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(BRUSH);
        ws.float(COLOUR);
        let (a, b) = (ws.floating[0].id, ws.floating[1].id);
        let source = ws.area_of(Surface::Floating(a)).expect("a");
        let at = (source.x + 8.0, source.y + 6.0);
        let target = Target::Tab {
            path: vec![],
            tab: 0,
        };

        ws.grab_surface = Surface::Floating(a);
        ws.gesture(
            source,
            pulse(true, false, true),
            Some(at),
            0.0,
            Surface::Floating(a),
            |_, _| target.clone(),
        );
        // Released without moving, but reported as being over the other window.
        ws.gesture(
            source,
            pulse(false, true, false),
            Some(at),
            1.0,
            Surface::Floating(b),
            |_, _| unreachable!(),
        );
        assert_eq!(ws.floating.len(), 2, "a tap merged two windows");
    }

    /// Put `panel` back as a tab beside `into`, the way an artist does: press *Put back into*,
    /// then press the panel you want it beside.
    ///
    /// A helper rather than a method, because there is no longer a `dock` to call -- putting a
    /// panel somewhere is one act with one implementation, and a test that reached past the pick
    /// would be testing something the artist cannot do.
    fn put_back(ws: &mut Workspace, panel: PanelId, into: PanelId, screen: Rect) -> bool {
        ws.screen = screen;
        ws.start_placing(panel);
        let slot = ws
            .layout
            .resolve(screen)
            .into_iter()
            .find(|p| p.tabs.contains(&into))
            .or_else(|| {
                ws.floating.iter().find_map(|f| {
                    f.layout
                        .resolve(inset(f.rect, ws.theme.metrics.gutter))
                        .into_iter()
                        .find(|p| p.tabs.contains(&into))
                })
            })
            .expect("somewhere to put it");
        // The middle, which is the zone that makes it a tab beside what is already there.
        ws.place_at(
            slot.rect.x + slot.rect.w / 2.0,
            slot.rect.y + slot.rect.h / 2.0,
        )
    }

    /// A window whose last panel leaves is taken down.
    ///
    /// An empty window is a rectangle of chrome with no way to tell it is not broken.
    #[test]
    fn a_window_goes_when_the_last_panel_leaves_it() {
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(BRUSH);
        assert_eq!(ws.floating.len(), 1);
        let screen = ws.screen;
        assert!(put_back(&mut ws, BRUSH, LAYERS, screen));
        assert!(ws.floating.is_empty(), "the emptied window should be gone");
    }

    /// **Floating takes a panel out of the arrangement, and docking puts it back.**
    ///
    /// A panel is in exactly one of the two places at any moment. Being in both would mean two
    /// copies; being in neither would mean it had vanished, which is the failure this whole path
    /// exists to avoid (DECISIONS 6b).
    #[test]
    fn a_panel_is_either_docked_or_floating_and_never_both_or_neither() {
        let mut ws = bare();
        assert!(ws.is_docked(BRUSH) && !ws.is_floating(BRUSH));

        ws.float(BRUSH);
        assert!(ws.is_floating(BRUSH), "it should be floating now");
        assert!(!ws.is_docked(BRUSH), "and gone from the arrangement");

        assert!(put_back(
            &mut ws,
            BRUSH,
            LAYERS,
            Rect::new(0.0, 0.0, 1400.0, 900.0)
        ));
        assert!(ws.is_docked(BRUSH), "it should be back");
        assert!(!ws.is_floating(BRUSH), "and no longer floating");
        // Beside the panel it was docked into, which is what "put back into Layers" means.
        let (brush, _) = ws.layout.find(BRUSH).expect("brush");
        let (layers, _) = ws.layout.find(LAYERS).expect("layers");
        assert_eq!(
            brush, layers,
            "it did not land beside the panel it was given"
        );
    }

    /// Floating and docking are both undoable, because both are changes to the arrangement.
    #[test]
    fn floating_a_panel_can_be_taken_back() {
        let mut ws = bare();
        let before = ws.layout.clone();
        ws.float(BRUSH);
        assert!(ws.undo(), "there should be a change to take back");
        assert_eq!(ws.layout, before, "the panel should be where it was");
    }

    /// **A pick never leaves a panel nowhere.**
    ///
    /// The panel comes out the moment the pick starts, which is what makes the question visible --
    /// and is also the moment it could be lost. Pressing somewhere that is not a destination puts
    /// it back rather than dropping it on the floor (DECISIONS 6b), and so does giving up.
    #[test]
    fn a_pick_never_leaves_a_panel_nowhere() {
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        for give_up in [false, true] {
            let mut ws = bare();
            ws.screen = screen;
            ws.float(BRUSH);
            let before = ws.snapshot();

            ws.start_placing(BRUSH);
            assert_eq!(ws.placing(), Some(BRUSH), "it should be in the air");
            assert!(
                !ws.is_open(BRUSH),
                "and out of the workspace while it is there"
            );

            if give_up {
                ws.cancel_placing();
            } else {
                // Well off the bottom right, where there is no leaf of anything.
                assert!(
                    !ws.place_at(screen.w + 400.0, screen.h + 400.0),
                    "nothing out there could have taken it"
                );
            }
            assert_eq!(ws.placing(), None, "the pick should be over");
            assert!(ws.is_open(BRUSH), "the panel came back");
            assert_eq!(
                ws.snapshot(),
                before,
                "and everything is exactly as it was, floating window included"
            );
        }
    }

    /// **A pick ignores floating windows**, both as a destination and as an obstacle.
    ///
    /// It is asked where in the *arrangement* a panel goes -- that is what "put it back" means --
    /// so the windows floating over it are simply transparent to the question. Without that, a leaf
    /// sitting behind a palette could not be pointed at at all, which is the one thing an
    /// arrangement you can see should never be.
    #[test]
    fn a_pick_ignores_floating_windows() {
        let mut ws = bare();
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.set_screen(screen);
        ws.float(COLOUR);

        // Park the window squarely over the leaf the Layers panel is in, and point at the middle
        // of both.
        let slot = ws
            .layout
            .resolve(screen)
            .into_iter()
            .find(|p| p.tabs.contains(&LAYERS))
            .expect("layers is docked");
        let at = (
            slot.rect.x + slot.rect.w / 2.0,
            slot.rect.y + slot.rect.h / 2.0,
        );
        ws.floating[0].rect = Rect::new(at.0 - 120.0, at.1 - 120.0, 240.0, 240.0);
        let window = ws.floating[0].rect;
        assert!(
            window.contains(at.0, at.1),
            "the window should be covering the point"
        );

        ws.hide(BRUSH);
        ws.start_placing(BRUSH);
        let mut hand = Hand::new(&mut ws, screen);
        hand.press(at);

        assert!(
            ws.is_docked(BRUSH),
            "the pick landed somewhere other than the arrangement behind the window"
        );
        assert!(
            !ws.is_floating(BRUSH),
            "it should not have gone into the window"
        );
        let (brush, _) = ws.layout.find(BRUSH).expect("brush");
        let (layers, _) = ws.layout.find(LAYERS).expect("layers");
        assert_eq!(
            brush, layers,
            "and it should have landed in the leaf pointed at"
        );
        assert_eq!(ws.floating.len(), 1, "the window is still there");
        assert_eq!(ws.floating[0].rect, window, "and has not moved");
    }

    /// A docked panel is offered somewhere to go; a floating one, somewhere to return to.
    #[test]
    fn the_settings_offer_the_direction_a_panel_can_actually_travel() {
        let mut ws = bare();
        let docked = ws.popup_controls(PopupKind::Settings(BRUSH));
        assert!(
            docked.iter().any(|c| c.id() == Some(FLOAT_ID)),
            "a docked panel should be offered a way out"
        );

        ws.float(BRUSH);
        let afloat = ws.popup_controls(PopupKind::Settings(BRUSH));
        assert!(
            !afloat.iter().any(|c| c.id() == Some(FLOAT_ID)),
            "a floating panel should not be offered floating again"
        );
        assert!(
            afloat.iter().any(|c| c.id() == Some(PUT_BACK_ID)),
            "it should be offered a way back"
        );
    }

    /// **A floating panel always keeps a handle on screen.**
    ///
    /// The same rule as the weight floor: something with nothing left to grab cannot be undone by
    /// hand. It may hang off an edge -- that is often exactly what you want -- but never so far
    /// that the bar you drag it by has gone with it.
    #[test]
    fn a_floating_panel_can_always_be_grabbed_again() {
        let m = crate::theme::Theme::default().metrics;
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let keep = m.header.max(m.row);
        for at in [
            (-5000.0, -5000.0),
            (5000.0, 5000.0),
            (-5000.0, 5000.0),
            (700.0, 400.0),
        ] {
            let r = hold_on_screen(Rect::new(at.0, at.1, 300.0, 400.0), screen, &m);
            let on = Rect::new(
                r.x.max(screen.x),
                r.y.max(screen.y),
                (r.x + r.w).min(screen.x + screen.w) - r.x.max(screen.x),
                (r.y + r.h).min(screen.y + screen.h) - r.y.max(screen.y),
            );
            assert!(
                on.w >= keep - 0.001 && on.h >= keep - 0.001,
                "dropped at {at:?} it left only {}x{} on screen",
                on.w,
                on.h
            );
        }
    }

    /// Two panels lifted in a row do not land exactly on top of one another.
    #[test]
    fn floating_panels_do_not_hide_behind_each_other() {
        let m = crate::theme::Theme::default().metrics;
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let a = first_float(screen, 0, &m);
        let b = first_float(screen, 1, &m);
        assert_ne!(a, b, "the second one landed exactly on the first");
        assert!(
            (a.x - b.x).abs() >= m.header - 0.001 || (a.y - b.y).abs() >= m.header - 0.001,
            "and it should be offset by enough to see"
        );
    }

    /// A floating panel survives being saved, under its name like everything else.
    #[test]
    fn a_floating_panel_survives_the_saved_form() {
        let mut ws = bare();
        ws.float(BRUSH);
        let where_it_was = ws.floating[0].rect;

        let saved = SavedWorkspace {
            layout: ws.layout.to_saved(name_for),
            panels: options_to_saved(&ws.options),
            // The application's own mapping, not a copy of it written out again here: a test that
            // reimplements the thing it is testing tests nothing, and a sabotage of the real
            // mapping slipped past the first version of this for exactly that reason.
            floating: floating_to_saved(&ws.floating),
        };
        let text = serde_json::to_string(&saved).expect("serialise");
        assert!(
            text.contains("Brush"),
            "written by name, not by number: {text}"
        );

        let back: SavedWorkspace = serde_json::from_str(&text).expect("parse");
        let restored = floating_from_saved(&back.floating);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].layout.panels(), vec![BRUSH]);
        assert_eq!(restored[0].rect, where_it_was);

        // A name this build does not know is dropped, not turned into some other panel.
        let text = text.replace("Brush", "Sparkles");
        let back: SavedWorkspace = serde_json::from_str(&text).expect("parse");
        assert!(
            floating_from_saved(&back.floating).is_empty(),
            "a window of panels this build does not know should be dropped, not reopened empty"
        );
    }

    /// **A setting overrides its panel's default, and only for that panel.**
    ///
    /// This is what makes direction a setting rather than a decision somebody else made: the panel
    /// table supplies a starting point, and what the artist chooses wins over it.
    #[test]
    fn a_panel_keeps_its_own_direction() {
        let mut ws = bare();
        assert_eq!(ws.direction_of(LAYERS), default_direction(LAYERS));

        ws.set_direction(LAYERS, Direction::Row);
        assert_eq!(ws.direction_of(LAYERS), Direction::Row);
        // And nobody else moved.
        for kind in PANELS {
            if kind.id != LAYERS {
                assert_eq!(
                    ws.direction_of(kind.id),
                    kind.direction,
                    "{} was dragged along with it",
                    kind.name
                );
            }
        }
    }

    /// **Holding a header still asks the panel what it offers, while the finger is still down.**
    ///
    /// Not on release: a menu that only appeared once you let go could not be dismissed by letting
    /// go, and the artist would be holding a finger on a panel wondering whether anything was
    /// going to happen. Every panel has a header, always, so there is no panel this cannot reach.
    #[test]
    fn holding_a_header_still_asks_the_panel_for_its_settings() {
        use crate::panel_drag::{pulse, Target, HOLD_MS};
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        let (path, _) = ws.layout.find(BRUSH).expect("brush");
        let target = Target::Tab { path, tab: 0 };

        ws.gesture(
            screen,
            pulse(true, false, true),
            Some((10.0, 10.0)),
            0.0,
            Surface::Docked,
            |_, _| target.clone(),
        );
        // Part-way through the hold: still nothing.
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            HOLD_MS / 2.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        assert_eq!(ws.popup_wanted, None, "it asked before the hold was done");

        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            HOLD_MS + 1.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        assert_eq!(
            ws.popup_wanted,
            Some(PopupKind::Settings(BRUSH)),
            "holding a header should ask the panel what it offers"
        );
        // And the gesture is over: a finger now reading a menu must not still be dragging.
        assert!(
            !ws.drag.active(),
            "the panel was still being carried while its menu was up"
        );
    }

    /// A panel that offers a direction offers all of them, and marks the one in force.
    #[test]
    fn the_settings_popup_offers_every_direction() {
        let mut ws = bare();
        ws.set_direction(TOOLS, Direction::Wrap);
        let controls = ws.popup_controls(PopupKind::Settings(TOOLS));
        for (id, name, _) in DIRECTIONS {
            assert!(
                controls.iter().any(|c| c.id() == Some(*id)),
                "{name} is not offered"
            );
        }
        let chosen: Vec<&str> = controls
            .iter()
            .filter_map(|c| match c {
                crate::panel_ui::Control::Choice {
                    text,
                    selected: true,
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            chosen,
            vec!["Across, wrapping"],
            "exactly one direction should read as chosen"
        );
    }

    /// **A panel with nothing to set says so** --- and can still be removed.
    ///
    /// The canvas has no controls at all, so offering it a choice between running them across or
    /// down was nonsense -- and an empty popup is indistinguishable from one that failed to open.
    /// Removing is not a setting, though: it is what you do with a panel rather than to it, and
    /// every panel offers it whatever else it has.
    #[test]
    fn a_panel_with_nothing_to_set_says_so() {
        let ws = bare();
        let controls = ws.popup_controls(PopupKind::Settings(CANVAS));
        assert!(
            controls.iter().all(|c| matches!(
                c,
                crate::panel_ui::Control::Label { .. } | crate::panel_ui::Control::Separator
            ) || c.id() == Some(REMOVE_ID)),
            "the canvas was offered something to change"
        );
        assert!(
            controls.iter().any(|c| c.id() == Some(REMOVE_ID)),
            "and it should still be removable"
        );
        assert!(
            controls
                .iter()
                .any(|c| matches!(c, crate::panel_ui::Control::Label { text } if text.contains("Nothing"))),
            "and it should say so rather than showing an empty box"
        );
        // Whereas a strip does offer something.
        assert!(
            ws.popup_controls(PopupKind::Settings(TOOLS))
                .iter()
                .any(|c| c.id().is_some()),
            "the tool rail should still offer its direction"
        );
    }

    /// Choosing in the settings popup changes the panel it belongs to.
    #[test]
    fn choosing_in_the_settings_popup_sets_that_panel() {
        let mut ws = bare();
        let (id, _, direction) = DIRECTIONS[0];
        ws.apply_popup(
            PopupKind::Settings(HISTORY),
            crate::panel_ui::Change::Chose(id),
        );
        assert_eq!(ws.direction_of(HISTORY), direction);
        assert_eq!(
            ws.direction_of(LAYERS),
            default_direction(LAYERS),
            "and only that panel"
        );
    }

    /// **There is always a way back, from any state the artist can reach.**
    ///
    /// This is the bug that shipped: the tick list that reopens a panel lived inside the Menu
    /// panel, which is closable like any other. Closing the Menu therefore took away the only way
    /// to bring anything back, including the Menu itself -- and the arrangement was saved, so
    /// restarting did not help either. A route out of a state cannot be reachable only from
    /// inside that state.
    #[test]
    fn every_panel_can_be_reopened_even_with_all_of_them_closed() {
        let mut ws = bare();
        for kind in PANELS {
            if ws.is_open(kind.id) {
                ws.toggle(kind.id);
            }
        }
        for kind in PANELS {
            assert!(!ws.is_open(kind.id), "{} is still open", kind.name);
        }

        // The list is built from the workspace, not from a panel, so it still offers everything.
        let controls = panel_list_controls(&ws);
        for kind in PANELS {
            assert!(
                controls.iter().any(|c| c.id() == Some(kind.id.0)),
                "{} cannot be reopened",
                kind.name
            );
        }

        // And the switches actually bring them back. Each one asks where it should go, and with
        // nothing left in the workspace the answer is the one empty leaf that is the whole of it
        // -- which is the case that matters here, because it is the one an artist reaches by
        // closing everything and is the one that has nowhere obvious to point at.
        let screen = ws.screen;
        for kind in PANELS {
            ws.toggle(kind.id);
            assert_eq!(
                ws.placing(),
                Some(kind.id),
                "{} did not ask where it should go",
                kind.name
            );
            assert!(
                ws.place_at(screen.x + screen.w / 2.0, screen.y + screen.h / 2.0),
                "{} had nowhere to land",
                kind.name
            );
            assert!(ws.is_open(kind.id), "{} did not come back", kind.name);
        }
    }

    /// **The default arrangement gives every strip room for its own contents.**
    ///
    /// The weights are fractions of a window, so it is entirely possible to pick one that leaves
    /// a menu bar too short to draw its own row of controls in --- and the symptom is not "the
    /// weight is wrong", it is text mysteriously clipped at the bottom. So the requirement is
    /// worked out from the metrics rather than trusted: whatever the fractions say, a strip must
    /// hold one row and its padding.
    #[test]
    fn the_default_arrangement_fits_what_it_holds() {
        use crate::panel_ui::Control;
        // A stand-in for the widest label a strip carries, in place of a font.
        const LABEL: f32 = 45.0;
        let m = crate::theme::Theme::default().metrics;
        let layout = default_layout();

        // **At every window size, not one.** The first version of this checked 1400x900 and
        // passed while the same arrangement clipped its own menu bar in half at 900x600 -- which
        // a screenshot found and this did not. A fixture that cannot express the bug proves
        // nothing, however carefully the assertion is written.
        for (w, h) in [
            (1400.0, 900.0),
            (1920.0, 1080.0),
            (1280.0, 720.0),
            (1024.0, 640.0),
            (900.0, 600.0),
            (800.0, 500.0),
        ] {
            let screen = Rect::new(0.0, 0.0, w, h);
            let placed = layout.resolve(screen);
            let content_of = |panel: PanelId| {
                let slot = placed
                    .iter()
                    .find(|p| p.tabs.contains(&panel))
                    .unwrap_or_else(|| panic!("{} is not in the default layout", name_of(panel)));
                crate::chrome::panel(
                    slot,
                    &m,
                    style_of(slot),
                    // The rule the application uses, not a fixed side: the menu's handle is down
                    // its right, so a test that put it on top would be measuring the wrong strip.
                    along_of(slot, default_direction(showing_of(slot))),
                    |_| LABEL,
                )
                .content
            };

            // The menu is a strip across the top: it needs the height of one row plus its padding.
            let menu = content_of(MENU);
            assert!(
                menu.h >= strip_content_min(&m),
                "at {w}x{h} the menu strip is {} tall and needs {}",
                menu.h,
                strip_content_min(&m)
            );

            // The tool rail is a column down the side: one button's width plus its padding.
            let tools = content_of(TOOLS);
            let button = Control::Choice {
                id: 0,
                text: String::new(),
                selected: false,
                icon: None,
            }
            .width(&m, LABEL)
                + m.padding * 2.0;
            assert!(
                tools.w >= button,
                "at {w}x{h} the tool rail is {} wide and needs {button}",
                tools.w
            );

            // And the canvas still gets the lion's share, or the strips have eaten the document.
            let canvas = content_of(CANVAS);
            assert!(
                canvas.w * canvas.h > w * h * 0.3,
                "at {w}x{h} the canvas is down to {}x{}",
                canvas.w,
                canvas.h
            );
        }
    }

    /// **A strip keeps its size as the window changes.** That is the difference between a minimum
    /// and a weight, and the whole reason minimums exist.
    #[test]
    fn a_strip_does_not_grow_with_the_window() {
        let m = crate::theme::Theme::default().metrics;
        let layout = default_layout();
        let height_at = |h: f32| {
            let placed = layout.resolve(Rect::new(0.0, 0.0, 1400.0, h));
            placed
                .iter()
                .find(|p| p.tabs.contains(&MENU))
                .expect("menu")
                .rect
                .h
        };
        let small = height_at(600.0);
        let large = height_at(1600.0);
        assert!(
            (small - large).abs() < 0.001,
            "the menu bar was {small} tall in a short window and {large} in a tall one"
        );
        assert!(small >= strip_min(&m));
    }

    /// **A panel's handle follows the direction its controls run**, and moves when that changes.
    ///
    /// Set a panel to run its controls down and the handle is the bar across the top; set it to
    /// run them across and the handle is the bar down the right. Deciding by the panel's shape
    /// instead was nearly right and read as arbitrary -- the handle moved when a *neighbour* was
    /// resized.
    #[test]
    fn the_handle_follows_the_direction_the_controls_run() {
        let slot = crate::layout::Placed {
            path: vec![],
            rect: Rect::new(0.0, 0.0, 900.0, 60.0),
            tabs: vec![TOOLS],
            active: 0,
        };
        assert_eq!(along_of(&slot, Direction::Column), Along::Down);
        assert_eq!(along_of(&slot, Direction::Row), Along::Across);
        assert_eq!(along_of(&slot, Direction::Wrap), Along::Across);

        // The same panel, the same shape, a different setting: the handle moves.
        let m = crate::theme::Theme::default().metrics;
        let down = crate::chrome::panel(&slot, &m, HeaderStyle::Compact, Along::Down, |_| 40.0);
        let across = crate::chrome::panel(&slot, &m, HeaderStyle::Compact, Along::Across, |_| 40.0);
        assert!(down.header.w > down.header.h, "down: a bar across the top");
        assert!(
            across.header.h > across.header.w,
            "across: a bar down the side"
        );

        // And `Auto` has to guess, because resolving it needs the controls measured -- which is
        // something `chrome` cannot do. It guesses from the shape, which is what `place` will
        // decide too in every case but a near-tie.
        assert_eq!(along_of(&slot, Direction::Auto), Along::Across);
        let tall = crate::layout::Placed {
            rect: Rect::new(0.0, 0.0, 60.0, 900.0),
            ..slot.clone()
        };
        assert_eq!(along_of(&tall, Direction::Auto), Along::Down);
    }

    /// A leaf's handle obeys the panel it is *showing*, not whichever happens to be first.
    ///
    /// A handle that jumped sides when you switched tab would be worse than either answer.
    #[test]
    fn the_handle_obeys_the_panel_on_show() {
        let slot = crate::layout::Placed {
            path: vec![],
            rect: Rect::new(0.0, 0.0, 900.0, 60.0),
            tabs: vec![LAYERS, TOOLS],
            active: 1,
        };
        assert_eq!(showing_of(&slot), TOOLS);
        let first = crate::layout::Placed { active: 0, ..slot };
        assert_eq!(showing_of(&first), LAYERS);
    }

    /// **Two panels in one leaf always show tabs**, however compact they would rather be.
    ///
    /// Stacking the Menu with the Tools rail hid one of them completely: both prefer a compact
    /// header, a compact header draws no tabs, and so there was nothing to press to get the other
    /// one back.
    #[test]
    fn a_leaf_holding_two_panels_shows_tabs() {
        let mut layout = Layout::single(MENU);
        layout.insert(&[], Zone::Center, TOOLS);
        let placed = layout.resolve(Rect::new(0.0, 0.0, 800.0, 600.0));
        let slot = placed.first().expect("one leaf");
        assert_eq!(slot.tabs.len(), 2, "both should be in the same leaf");
        assert_eq!(
            style_of(slot),
            HeaderStyle::Named,
            "with two panels there must be tabs to choose between them"
        );

        // On its own each is still compact: this is about how many, not about which.
        let alone = Layout::single(TOOLS);
        let placed = alone.resolve(Rect::new(0.0, 0.0, 800.0, 600.0));
        assert_eq!(
            style_of(placed.first().expect("leaf")),
            HeaderStyle::Compact
        );
    }

    /// Which way a panel's controls run comes from the panel table, not from one answer given to
    /// everything.
    #[test]
    fn each_panel_brings_its_own_direction() {
        for kind in PANELS {
            assert_eq!(
                default_direction(kind.id),
                kind.direction,
                "{} lost its direction",
                kind.name
            );
        }
        // And the table actually says more than one thing, or the check above proves nothing.
        let first = PANELS[0].direction;
        assert!(
            PANELS.iter().any(|k| k.direction != first),
            "every panel has the same direction, so this test is vacuous"
        );
    }

    /// The list is the panel table, not a copy of it that someone has to remember to update.
    #[test]
    fn the_panel_list_offers_every_panel_this_build_has() {
        let mut ws = bare();
        // With one panel away, so a switch stuck permanently on has something to be wrong about.
        // The default arrangement has all of them open, and against that fixture it did not.
        ws.toggle(HISTORY);
        let ws = ws;
        let controls = panel_list_controls(&ws);
        let switches = controls.iter().filter(|c| c.id().is_some()).count();
        assert_eq!(switches, PANELS.len());
        for kind in PANELS {
            let on = controls.iter().any(|c| {
                matches!(c, crate::panel_ui::Control::Toggle { id, on, .. }
                    if *id == kind.id.0 && *on)
            });
            assert_eq!(
                on,
                ws.is_open(kind.id),
                "{}'s switch disagrees with the layout",
                kind.name
            );
        }
    }

    /// **A whole move, driven one frame at a time the way the app drives it.**
    ///
    /// This is the test that was missing. Every drag test built a `Target` by hand and called
    /// press, drag and release in a row, so none of them ever saw the frame sequence a real
    /// pointer produces: and in that sequence the release frame reports the button *already up*.
    /// A guard reading "not down" as a lost pointer therefore cancelled every gesture on the exact
    /// frame it should have been completed, and moving a panel silently did nothing for as long as
    /// that guard existed.
    #[test]
    fn a_move_survives_the_frame_the_pointer_is_released_on() {
        use crate::panel_drag::{pulse, Pulse, Target};
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        let brush_before = ws.layout.find(BRUSH).expect("brush").0;
        let placed = ws.layout.resolve(screen);
        let brush = placed
            .iter()
            .find(|p| p.tabs.contains(&BRUSH))
            .expect("brush leaf");
        let chrome = crate::chrome::panel(
            brush,
            &ws.theme.metrics,
            style_of(brush),
            Along::Down,
            |_| 46.0,
        );
        // On the *tab*, which is the only thing that acts for a panel. The middle of the header
        // is the panel strip, and pressing that used to move whichever panel was on show.
        let tab = chrome.tabs.first().expect("a named header has a tab").rect;
        let (px, py) = (tab.x + tab.w / 2.0, tab.y + tab.h / 2.0);
        let target = Target::Tab {
            path: brush.path.clone(),
            tab: brush.active,
        };
        let canvas = placed
            .iter()
            .find(|p| p.tabs.contains(&CANVAS))
            .expect("canvas");
        let (dx, dy) = (
            canvas.rect.x + canvas.rect.w / 2.0,
            canvas.rect.y + canvas.rect.h - 8.0,
        );

        // Frame 1: the button goes down.
        ws.gesture(
            screen,
            pulse(true, false, true),
            Some((px, py)),
            0.0,
            Surface::Docked,
            |_, _| target.clone(),
        );
        // Frame 2: still held, still there, past the hold.
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px, py)),
            1.0,
            Surface::Docked,
            |_, _| unreachable!("only a press asks what is under the pointer"),
        );
        assert!(
            ws.drag.active(),
            "the press should have taken hold of the panel at once"
        );
        // Frame 3: carried over the canvas.
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((dx, dy)),
            2.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        // Frame 4: let go. egui reports the release *and* the button already up, together.
        assert_eq!(pulse(false, true, false), Pulse::Release);
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((dx, dy)),
            3.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );

        let brush_after = ws.layout.find(BRUSH).expect("brush must still exist").0;
        assert_ne!(
            brush_before, brush_after,
            "the drop did nothing: the panel is exactly where it started"
        );
    }

    /// **A move can be taken back.** The whole point of the layout having its own undo stack.
    #[test]
    fn a_move_can_be_undone() {
        use crate::panel_drag::{pulse, Target};
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        let before = ws.layout.clone();
        let placed = ws.layout.resolve(screen);
        let brush = placed
            .iter()
            .find(|p| p.tabs.contains(&BRUSH))
            .expect("brush");
        let target = Target::Tab {
            path: brush.path.clone(),
            tab: brush.active,
        };
        let canvas = placed
            .iter()
            .find(|p| p.tabs.contains(&CANVAS))
            .expect("canvas");
        let (dx, dy) = (
            canvas.rect.x + canvas.rect.w / 2.0,
            canvas.rect.y + canvas.rect.h - 8.0,
        );

        ws.gesture(
            screen,
            pulse(true, false, true),
            Some((10.0, 10.0)),
            0.0,
            Surface::Docked,
            |_, _| target.clone(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            1.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((dx, dy)),
            2.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((dx, dy)),
            3.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        assert_ne!(
            ws.layout, before,
            "nothing moved, so there is nothing to undo"
        );

        assert!(ws.undo(), "there should be a layout change to take back");
        assert_eq!(ws.layout, before, "undo did not put it back");
        assert!(ws.redo(), "and it should be redoable");
        assert_ne!(ws.layout, before);
    }

    /// A resize can be taken back too. It is applied live while the divider is dragged, which is
    /// exactly why it is easy to forget to record.
    #[test]
    fn a_resize_can_be_undone() {
        use crate::panel_drag::{pulse, Target};
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        let before = ws.layout.clone();
        let s0 = ws.layout.splitters(screen, ws.theme.metrics.splitter_grab);
        // A divider of a horizontal split, so dragging in x actually moves it. Picking the first
        // one gave a vertical split's divider, which x does not touch: the test moved nothing and
        // said so.
        let sp = s0
            .iter()
            .find(|s| s.rect.h > s.rect.w)
            .expect("a vertical divider")
            .clone();
        let target = Target::Splitter {
            path: sp.path.clone(),
            index: sp.index,
        };
        let (px, py) = (sp.rect.x + sp.rect.w / 2.0, sp.rect.y + sp.rect.h / 2.0);

        ws.gesture(
            screen,
            pulse(true, false, true),
            Some((px, py)),
            0.0,
            Surface::Docked,
            |_, _| target.clone(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px, py)),
            1.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px + 80.0, py)),
            2.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((px + 80.0, py)),
            3.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        assert_ne!(ws.layout, before, "the divider did not move");

        assert!(ws.undo(), "there should be a resize to take back");
        assert_eq!(ws.layout, before, "undo did not put the divider back");
    }

    /// **Escape during a divider drag puts the divider back.**
    ///
    /// A divider moves live, so by the time Escape arrives the layout has already changed. Without
    /// restoring it, "a gesture you have thought better of costs nothing" (§5e) was simply untrue
    /// for the one gesture that shows its work as you make it.
    #[test]
    fn escape_during_a_resize_puts_the_divider_back() {
        use crate::panel_drag::{pulse, Target};
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        let before = ws.layout.clone();
        let s0 = ws.layout.splitters(screen, ws.theme.metrics.splitter_grab);
        let sp = s0
            .iter()
            .find(|s| s.rect.h > s.rect.w)
            .expect("a vertical divider")
            .clone();
        let target = Target::Splitter {
            path: sp.path.clone(),
            index: sp.index,
        };
        let (px, py) = (sp.rect.x + sp.rect.w / 2.0, sp.rect.y + sp.rect.h / 2.0);

        ws.gesture(
            screen,
            pulse(true, false, true),
            Some((px, py)),
            0.0,
            Surface::Docked,
            |_, _| target.clone(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px, py)),
            1.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px + 120.0, py)),
            2.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        assert_ne!(ws.layout, before, "the divider did not move");

        assert!(ws.cancel_drag(), "there was a gesture to abandon");
        assert_eq!(
            ws.layout, before,
            "Escape left the divider where it had got to"
        );
    }

    /// A divider held but never moved is not a change, and does not eat an undo step.
    ///
    /// The common case by a distance: hold a divider, think better of it, let go. Recording that
    /// would mean the next undo spends itself restoring something that already looks right, which
    /// reads as undo being broken.
    ///
    /// Deliberately *held and released*, not moved-and-returned. Dragging out and back does not
    /// come home to the same floating-point weights, so no exact-equality check could promise it
    /// and a test asserting otherwise would only be describing luck.
    #[test]
    fn a_divider_held_but_never_moved_is_not_a_change() {
        use crate::panel_drag::{pulse, Target, HOLD_MS};
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        let sp = ws
            .layout
            .splitters(screen, ws.theme.metrics.splitter_grab)
            .into_iter()
            .find(|s| s.rect.h > s.rect.w)
            .expect("a vertical divider");
        let target = Target::Splitter {
            path: sp.path.clone(),
            index: sp.index,
        };
        let (px, py) = (sp.rect.x + sp.rect.w / 2.0, sp.rect.y + sp.rect.h / 2.0);

        ws.gesture(
            screen,
            pulse(true, false, true),
            Some((px, py)),
            0.0,
            Surface::Docked,
            |_, _| target.clone(),
        );
        for (t, x) in [(1.0, px), (2.0, px), (3.0, px)] {
            ws.gesture(
                screen,
                pulse(false, false, true),
                Some((x, py)),
                HOLD_MS + t,
                Surface::Docked,
                |_, _| unreachable!(),
            );
        }
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((px, py)),
            HOLD_MS + 4.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        assert!(
            !ws.undo(),
            "holding a divider without moving it is not something to undo"
        );
    }

    /// A gesture that never armed abandons only itself.
    ///
    /// It has changed nothing, so cancelling it must change nothing either -- including anything
    /// that happened in the meantime. Restoring the layout unconditionally would let a stray press
    /// quietly roll back a panel opened from the list a moment later.
    #[test]
    fn abandoning_an_unarmed_gesture_takes_nothing_with_it() {
        use crate::panel_drag::{pulse, Target};
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        ws.hide(HISTORY);
        let (path, _) = ws.layout.find(BRUSH).expect("brush");
        let target = Target::Tab { path, tab: 0 };

        ws.gesture(
            screen,
            pulse(true, false, true),
            Some((10.0, 10.0)),
            0.0,
            Surface::Docked,
            |_, _| target.clone(),
        );
        // Something else changes the arrangement while the press is still waiting.
        let screen_now = ws.screen;
        assert!(put_back(&mut ws, HISTORY, BRUSH, screen_now));
        let after = ws.layout.clone();
        assert!(ws.is_open(HISTORY));

        ws.cancel_drag();
        assert_eq!(
            ws.layout, after,
            "abandoning a press that never armed rolled back an unrelated change"
        );
    }

    /// A pointer that goes missing mid-gesture cancels it and leaves the arrangement alone.
    ///
    /// Without this the grab stays live and *every* later pointer move keeps rearranging the
    /// workspace, which is indistinguishable from the app having gone mad.
    #[test]
    fn a_lost_pointer_abandons_the_gesture() {
        use crate::panel_drag::{pulse, Target};
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        let before = ws.layout.clone();
        let (path, _) = ws.layout.find(BRUSH).expect("brush");
        let target = Target::Tab { path, tab: 0 };

        ws.gesture(
            screen,
            pulse(true, false, true),
            Some((10.0, 10.0)),
            0.0,
            Surface::Docked,
            |_, _| target.clone(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            1.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        assert!(ws.drag.active(), "there should be a gesture to lose");

        // The button is not down and no release was reported: the pointer is gone.
        ws.gesture(
            screen,
            pulse(false, false, false),
            Some((700.0, 700.0)),
            2.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        assert!(!ws.drag.active(), "the gesture should have been abandoned");
        assert_eq!(ws.layout, before, "and nothing should have moved");
    }

    /// A panel let go outside the window comes back rather than being lost.
    ///
    /// Floating windows do not exist yet, so there is nowhere for it to go -- and silently losing
    /// a panel is the worst possible answer (DECISIONS 6b), especially now that the panel holding
    /// the way to reopen things is itself movable.
    #[test]
    fn a_panel_let_go_outside_the_window_comes_back() {
        use crate::panel_drag::{pulse, Target};
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut ws = bare();
        let (path, _) = ws.layout.find(BRUSH).expect("brush");
        let target = Target::Tab { path, tab: 0 };

        ws.gesture(
            screen,
            pulse(true, false, true),
            Some((10.0, 10.0)),
            0.0,
            Surface::Docked,
            |_, _| target.clone(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            1.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        // Well outside the window.
        let (ox, oy) = (screen.w + 200.0, screen.h + 200.0);
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((ox, oy)),
            2.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((ox, oy)),
            3.0,
            Surface::Docked,
            |_, _| unreachable!(),
        );
        assert!(
            ws.layout.find(BRUSH).is_some(),
            "the panel was dropped on the floor"
        );
    }

    /// What is worth writing out, and what is not.
    #[test]
    fn only_a_changed_arrangement_is_saved() {
        assert!(saves(&Outcome::Moved));
        assert!(saves(&Outcome::Resized));
        assert!(saves(&Outcome::Floated(BRUSH)));
        assert!(!saves(&Outcome::Nothing));
        // Which tab you are looking at is not part of the arrangement, the same reason it is not
        // recorded for undo.
        assert!(!saves(&Outcome::Switched));
    }

    /// Press where a pointer would land, wait out the hold, move to another panel, let go.
    ///
    /// Every earlier test built a `Target::Tab` by hand and started from there, which skipped
    /// `chrome::target_at` entirely — the one piece that has to agree with what is drawn. A move
    /// that stopped working in the app while every drag test stayed green is exactly what that
    /// gap looks like.
    #[test]
    fn a_panel_can_be_moved_from_a_press_on_what_is_drawn() {
        use crate::panel_drag::{Outcome, PanelDrag};
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let mut layout = default_layout();
        let m = crate::theme::Theme::default().metrics;
        let measure = |_: &crate::layout::Placed, _: usize| 46.0;

        // Where the Brush panel's header actually is.
        let placed = layout.resolve(screen);
        let brush = placed
            .iter()
            .find(|p| p.tabs.contains(&BRUSH))
            .expect("the default layout has a Brush panel");
        let chrome = crate::chrome::panel(brush, &m, style_of(brush), Along::Down, |i| {
            measure(brush, i)
        });
        // On the *tab*, which is the only thing that acts for a panel. The middle of the header is
        // the panel strip, and pressing that used to move whichever panel was on show.
        let tab = chrome.tabs.first().expect("a named header has a tab").rect;
        let (px, py) = (tab.x + tab.w / 2.0, tab.y + tab.h / 2.0);

        let splitters = layout.splitters(screen, m.splitter_grab);
        let target = crate::chrome::target_at(
            &placed,
            &splitters,
            &m,
            |pl| (style_of(pl), Along::Down),
            measure,
            px,
            py,
        );
        assert!(
            matches!(target, crate::panel_drag::Target::Tab { .. }),
            "a press on a drawn header must be a tab: got {target:?}"
        );

        let mut drag = PanelDrag::default();
        drag.press(&layout, &target, px, py, 0.0);
        // Held still past the hold, which is what arms it.
        drag.drag(&mut layout, screen, px, py, 1.0);
        assert!(drag.active(), "the hold should have armed the move");

        // Onto the far side of the canvas, which is a different leaf.
        let canvas = layout
            .resolve(screen)
            .into_iter()
            .find(|p| p.tabs.contains(&CANVAS))
            .expect("canvas");
        let (dx, dy) = (
            canvas.rect.x + canvas.rect.w / 2.0,
            canvas.rect.y + canvas.rect.h - 8.0,
        );
        drag.drag(&mut layout, screen, dx, dy, 2.0);
        let outcome = drag.release(&mut layout, screen, dx, dy);

        assert_eq!(outcome, Outcome::Moved, "the drop did nothing");
        assert!(
            layout.find(BRUSH).is_some(),
            "and the panel must still exist"
        );
    }
    use super::*;

    /// The default layout puts every panel somewhere, and the canvas gets the most room.
    ///
    /// Both halves matter: a panel that is not in the default is one nobody will find, and a
    /// canvas that is not the biggest thing on screen is the wrong default for a drawing app.
    #[test]
    fn the_default_layout_shows_everything_with_the_canvas_largest() {
        let l = default_layout();
        let screen = Rect::new(0.0, 0.0, 1600.0, 1000.0);
        let placed = l.resolve(screen);

        for kind in PANELS {
            assert!(
                l.find(kind.id).is_some(),
                "{} is not in the default layout, so nobody will find it",
                kind.name
            );
        }

        let canvas = placed
            .iter()
            .find(|p| p.tabs.contains(&CANVAS))
            .expect("the canvas is placed");
        let area = |r: Rect| r.w * r.h;
        for other in &placed {
            if other.tabs.contains(&CANVAS) {
                continue;
            }
            assert!(
                area(canvas.rect) > area(other.rect),
                "the canvas should be the biggest panel, but {:?} beats it",
                other.tabs
            );
        }
        assert!(
            area(canvas.rect) > area(screen) * 0.5,
            "the canvas should own most of the window, has {:.0}%",
            area(canvas.rect) / area(screen) * 100.0
        );
    }

    /// A strip is a strip. **An `insert` halves whatever it lands on**, so a default layout that
    /// sets no weights gives the menu bar half the window and the tool rail half of what is left —
    /// which is absurd, invisible in the code, and exactly what this catches.
    #[test]
    fn the_strips_are_strips() {
        let screen = Rect::new(0.0, 0.0, 1600.0, 1000.0);
        let placed = default_layout().resolve(screen);
        let find = |id| {
            placed
                .iter()
                .find(|p| p.tabs.contains(&id))
                .unwrap_or_else(|| panic!("{id:?} is placed"))
                .rect
        };

        let menu = find(MENU);
        assert!(
            menu.h < screen.h * 0.1,
            "the menu is {:.0} tall, {:.0}% of the window",
            menu.h,
            menu.h / screen.h * 100.0
        );
        let tools = find(TOOLS);
        assert!(
            tools.w < screen.w * 0.1,
            "the tool rail is {:.0} wide, {:.0}% of the window",
            tools.w,
            tools.w / screen.w * 100.0
        );
        // And still wide enough to hold a tool button, or the strip is decorative.
        assert!(tools.w > 40.0, "the tool rail is only {:.0} wide", tools.w);
    }

    /// A leaf holding a compact panel and a named one shows names, or the named panel could never
    /// be reached.
    #[test]
    fn stacking_a_named_panel_with_a_compact_one_shows_names() {
        let compact = crate::layout::Placed {
            path: vec![],
            rect: Rect::new(0.0, 0.0, 100.0, 100.0),
            tabs: vec![TOOLS],
            active: 0,
        };
        assert_eq!(style_of(&compact), HeaderStyle::Compact);

        let mixed = crate::layout::Placed {
            tabs: vec![TOOLS, LAYERS],
            ..compact.clone()
        };
        assert_eq!(style_of(&mixed), HeaderStyle::Named);
    }

    /// Reset is undoable, so the safety net is not itself a one-way door.
    #[test]
    fn reset_can_itself_be_undone() {
        let mut w = Workspace::default();
        w.layout.remove(MENU);
        w.layout.remove(TOOLS);
        let stripped = w.layout.clone();

        w.reset();
        assert!(w.layout.find(MENU).is_some(), "the menu came back");

        assert!(w.undo());
        assert_eq!(w.layout, stripped, "and the reset itself can be taken back");
    }

    /// Opening a panel that is already open brings it forward rather than adding a second copy.
    #[test]
    fn opening_an_open_panel_just_shows_it() {
        let mut w = Workspace::default();
        let before = w.layout.clone();
        w.open(LAYERS);
        assert!(w.layout.find(LAYERS).is_some());
        assert_eq!(
            w.layout.resolve(Rect::new(0.0, 0.0, 100.0, 100.0)).len(),
            before.resolve(Rect::new(0.0, 0.0, 100.0, 100.0)).len(),
            "no new leaf appeared"
        );
    }

    /// A closed panel can be brought back, which is what stops closing one being permanent.
    #[test]
    fn a_closed_panel_can_be_reopened() {
        let mut w = Workspace::default();
        w.layout.remove(COLOUR);
        assert!(w.layout.find(COLOUR).is_none());

        w.open(COLOUR);
        assert!(w.layout.find(COLOUR).is_some(), "and there it is again");
    }

    /// Every panel in the table has a distinct id, or two of them would be the same panel.
    #[test]
    fn every_panel_has_its_own_id() {
        let mut ids: Vec<u32> = PANELS.iter().map(|k| k.id.0).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "two panels share an id");
    }
}
