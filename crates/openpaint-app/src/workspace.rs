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
            pending: None,
            screen: Rect::new(0.0, 0.0, 1280.0, 800.0),
        }
    }
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

impl Workspace {
    /// Where the canvas panel is, for the renderer to draw into.
    #[must_use]
    pub fn canvas_rect(&self) -> Option<Rect> {
        self.canvas_rect
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
        self.drag.armed() || self.popup.is_some()
    }

    /// Abandon a panel drag without applying it.
    ///
    /// Escape, and the same promise a transform makes (§5e): a gesture you have thought better of
    /// costs nothing, because nothing has happened to the layout until you let go.
    pub fn cancel_drag(&mut self) -> bool {
        let was = self.drag.active() || self.popup.is_some();
        self.drag.cancel(&mut self.layout);
        self.popup = None;
        was
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
    #[must_use]
    pub fn is_open(&self, panel: PanelId) -> bool {
        self.layout.find(panel).is_some()
    }

    /// Show a panel if it is hidden, or put it away if it is showing.
    ///
    /// The only way to close one, and therefore the thing that makes "everything closed" a state
    /// an artist can reach and come back from rather than a theoretical one. Undoable like any
    /// other layout change.
    pub fn toggle(&mut self, panel: PanelId) {
        self.remember_for_undo();
        if self.layout.find(panel).is_some() {
            self.layout.remove(panel);
        } else {
            // Into whichever leaf is showing, as a tab. Somewhere visible beats somewhere clever.
            let path = self
                .layout
                .resolve(Rect::new(0.0, 0.0, 1.0, 1.0))
                .first()
                .map(|p| p.path.clone())
                .unwrap_or_default();
            self.layout.insert(&path, Zone::Center, panel);
        }
        self.remember();
    }

    /// Undo the last layout change. Returns whether there was one.
    pub fn undo(&mut self) -> bool {
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
            egui::Order::Background,
            egui::Id::new("workspace"),
        ));
        let m = self.theme.metrics;
        let p = self.theme.palette;
        self.screen = screen;

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
        if let Some(kind) = self.popup_wanted.take() {
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
            if let Some(pos) = pointer {
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
                    draw_strip(&painter, &c, &m, &p);
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
                egui::LayerId::new(egui::Order::Middle, egui::Id::new(("panel", showing.0))),
                egui::Id::new(("panel-ui", showing.0)),
                egui::UiBuilder::new().max_rect(content),
            );
            // Contents are clipped to their own panel, so a list too long for its slot cannot
            // draw over the panel beside it.
            ui.set_clip_rect(content);
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
                egui::Order::Foreground,
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
                Held::Divider { path, index } => {
                    if let Some(s) = seams.iter().find(|s| &s.path == path && s.index == *index) {
                        // Grows from the hairline to the full grab width as the hold completes,
                        // so the target you are about to get is the thing you watch appear.
                        let t = m.splitter_hover.mul_add(*progress, m.gutter);
                        marks.rect_filled(to_egui(centred(s.rect, s.axis, t)), t / 2.0, tint);
                    }
                }
                // The whole window, so a hold on its body is as visible as a hold on a tab.
                Held::Frame => {
                    if let Some(at) = self.area_of(working) {
                        marks.rect_filled(to_egui(at), m.radius, tint);
                    }
                }
            }
        }

        // --- the divider under the pointer, thickened so it can be seen as well as caught ---
        //
        // Drawn last of the chrome so it sits over the panels either side, and only when the
        // pointer is close: at rest the gutter is a hairline and the workspace stays quiet.
        if let Some(pos) = pointer {
            if preview.is_none() || matches!(preview, Some(Preview::Resizing { .. })) {
                for s in &seams {
                    if !s.rect.contains(pos.x, pos.y) {
                        continue;
                    }
                    // At the *drawn* thickness rather than the grab width: the point is to show
                    // where it is, not how big the target is. A hint for a pen, which hovers; a
                    // finger gets nothing here and does not need to, because the hold is what
                    // actually starts the gesture.
                    let t = m.splitter_hover;
                    marks.rect_filled(to_egui(centred(s.rect, s.axis, t)), t / 2.0, rgb(p.edge));
                    break;
                }
            }
        }

        if let Some(Preview::Resizing { path, index }) = &preview {
            if let Some(s) = seams.iter().find(|s| &s.path == path && s.index == *index) {
                let t = m.splitter_hover;
                marks.rect_filled(to_egui(centred(s.rect, s.axis, t)), t / 2.0, rgb(p.state));
            }
        }

        // --- the drop overlay, on top of everything ---
        if let Some(Preview::Carrying { panel, over }) = preview {
            // **A floating window shows its zones on whatever it is over, not on itself.** The
            // window is following the pointer, so its own leaf is always under it; drawing that
            // would offer to drop it into the thing being dragged.
            let over = if working == Surface::Docked {
                over
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
            let top = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
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
                egui::Order::Middle,
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
                draw_strip(&top, &c, &m, &p);
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
                    egui::LayerId::new(egui::Order::Middle, egui::Id::new("floating")),
                    egui::Id::new(("floating-body", held.id.0, showing.0)),
                    egui::UiBuilder::new().max_rect(to_egui(c.controls.rect())),
                );
                ui.set_clip_rect(to_egui(c.controls.rect()));
                contents(showing, &mut ui, self.direction_of(showing), Place::Panel);
            }
        }

        // --- the popup, above everything, drawn by the same descriptor layer as any panel ---
        if let Some(popup) = self.popup {
            let theme = self.theme;
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::LayerId::new(egui::Order::Foreground, egui::Id::new("workspace-popup")),
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
                    return controls;
                }
                for setting in offered {
                    match setting {
                        Setting::Floating => {
                            if self.is_floating(panel) {
                                controls.push(Control::Label {
                                    text: "Put back into".to_owned(),
                                });
                                // Only panels still in the arrangement: docking into another
                                // floating one would leave both floating and nothing docked, which
                                // is not what "put back" means to anybody.
                                controls.extend(
                                    PANELS.iter().filter(|k| self.is_docked(k.id)).map(|k| {
                                        Control::Button {
                                            id: DOCK_BASE + k.id.0,
                                            text: k.name.to_owned(),
                                        }
                                    }),
                                );
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
                controls
            }
        }
    }

    /// Act on something the artist changed in a popup.
    fn apply_popup(&mut self, kind: PopupKind, change: crate::panel_ui::Change) {
        use crate::panel_ui::Change;
        match (kind, change) {
            // The list stays open on a toggle: showing three panels should be three taps, not
            // three right-clicks.
            (PopupKind::Panels, Change::Toggled(id, _)) => self.toggle(PanelId(id)),
            (PopupKind::Workspace, Change::Chose(id)) if id >= ICON_SET_BASE => {
                self.set_icons(id - ICON_SET_BASE);
            }
            (PopupKind::Settings(panel), Change::Pressed(FLOAT_ID)) => self.float(panel),
            (PopupKind::Settings(panel), Change::Pressed(id)) if id >= DOCK_BASE => {
                self.dock(panel, PanelId(id - DOCK_BASE));
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
        if self.is_floating(panel) || !self.is_docked(panel) {
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

    /// Put a floating panel back, as a tab beside one that is still docked.
    ///
    /// **Refused rather than guessed at** when the destination is not somewhere it could go: a
    /// panel that vanished because it was docked into something that was itself floating would be
    /// the worst possible answer (DECISIONS 6b).
    pub fn dock(&mut self, panel: PanelId, into: PanelId) {
        if !self.is_floating(panel) {
            return;
        }
        let Some((path, _)) = self.layout.find(into) else {
            eprintln!("dock: {} is not in the arrangement", name_of(into));
            return;
        };
        self.remember_for_undo();
        self.take_from_floating(panel);
        self.layout.insert(&path, Zone::Center, panel);
        self.popup = None;
        self.remember();
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
        let size = source.rect;
        self.remember_for_undo();
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

    /// Take note of where inside a window the pointer landed, so a drag can keep it there.
    ///
    /// Without it the window's corner jumps to the pointer the moment it moves, which reads as the
    /// window having been thrown rather than picked up.
    fn take_grip(&mut self, which: Surface, at: (f32, f32)) {
        let origin = match which {
            Surface::Docked => (0.0, 0.0),
            Surface::Floating(id) => self
                .floating
                .iter()
                .find(|f| f.id == id)
                .map_or((0.0, 0.0), |f| (f.rect.x, f.rect.y)),
        };
        self.grip = (at.0 - origin.0, at.1 - origin.1);
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
        let over = input
            .at
            .and_then(|(x, y)| self.window_at(x, y))
            .map_or(Surface::Docked, Surface::Floating);
        self.grab_surface = working_surface(self.drag.armed(), over, self.grab_surface);
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
        self.remember_for_undo();
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
        let carried = match (self.drag.moved(), self.grab_surface) {
            (true, Surface::Floating(id)) => Some(id),
            _ => None,
        };
        self.floating
            .iter()
            .rev()
            .find(|f| Some(f.id) != carried && f.rect.contains(x, y))
            .map(|f| f.id)
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
            let mut drag = std::mem::take(&mut self.drag);
            if let Some((layout, _)) = self.surface_mut(self.grab_surface) {
                drag.cancel(layout);
            } else {
                drag.cancel(&mut Layout::single(CANVAS));
            }
            self.drag = drag;
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
                if let (Surface::Floating(from), Surface::Floating(to)) = (self.grab_surface, over)
                {
                    if from != to {
                        self.drag.let_go();
                        self.merge_window(from, to, x, y);
                        return None;
                    }
                }
                if over != self.grab_surface {
                    // Different modes: the window has already been carried to where it was let go,
                    // and that is all that was asked for.
                    self.drag.let_go();
                    if self.grab_surface != Surface::Docked {
                        self.remember();
                    }
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
                    if let Some(was) = was {
                        self.history.record(was);
                    }
                    self.remember();
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
                let mut drag = std::mem::take(&mut self.drag);
                if let Some(layout) = self.layout_of_mut(self.grab_surface) {
                    drag.cancel(layout);
                } else {
                    drag.cancel(&mut Layout::single(CANVAS));
                }
                self.drag = drag;
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
/// Where the "put it back into ..." choices start, one per panel still docked.
const DOCK_BASE: u32 = 1 << 18;

/// Where the icon-set choices start, out of the way of anything else a workspace popup offers.
const ICON_SET_BASE: u32 = 1 << 16;

/// Draw the panel strip: the part of a header that acts for the panel rather than for a tab.
///
/// **It has to be findable.** It is where a window is picked up and where "float everything here"
/// will live, and an unmarked stretch of bar is indistinguishable from nothing at all. Three
/// short rules, which is the quietest mark that still reads as a handle rather than as a gap.
fn draw_strip(
    painter: &egui::Painter,
    chrome: &crate::chrome::PanelChrome,
    m: &Metrics,
    p: &crate::theme::Palette,
) {
    let past = chrome
        .tabs
        .last()
        .map_or(chrome.header.x, |t| t.rect.x + t.rect.w);
    let width = crate::chrome::strip_width(m);
    let (cx, cy) = (
        past + width / 2.0,
        chrome.header.y + chrome.header.h - m.header / 2.0,
    );
    if cx + width / 2.0 > chrome.header.x + chrome.header.w + 0.001 {
        return;
    }
    for i in -1_i8..=1 {
        let y = f32::from(i).mul_add(3.0, cy);
        painter.line_segment(
            [
                egui::pos2(cx - width / 4.0, y.round() + 0.5),
                egui::pos2(cx + width / 4.0, y.round() + 0.5),
            ],
            egui::Stroke::new(1.0_f32, rgb(p.dim)),
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

/// Keep a floating panel somewhere it can be taken hold of again.
///
/// **Its header must stay reachable**, which is the same rule as the weight floor: a panel with
/// nothing left to grab cannot be undone by hand. So it may hang off an edge -- that is often what
/// you want, to get it out of the way -- but never so far that the bar you drag it by is gone.
#[must_use]
fn hold_on_screen(rect: Rect, screen: Rect, m: &Metrics) -> Rect {
    let keep = m.header.max(m.row);
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
            pending: None,
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
    }

    impl Hand<'_> {
        fn new(ws: &mut Workspace, screen: Rect) -> Hand<'_> {
            ws.screen = screen;
            Hand {
                ws,
                screen,
                clock: 0.0,
            }
        }

        /// One frame. `measure` is a fixed label width, since there are no fonts here.
        fn frame(&mut self, at: (f32, f32), pressed: bool, released: bool, down: bool) {
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
        let grab = hand.tab_of(b);
        hand.drag(grab, (300.0, 700.0));

        assert_eq!(
            hand.rect_of(a),
            a_after,
            "moving the second window moved the first"
        );
        let moved = hand.rect_of(b);
        assert!(
            (moved.y - 700.0).abs() < 60.0,
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

    /// A window whose last panel leaves is taken down.
    ///
    /// An empty window is a rectangle of chrome with no way to tell it is not broken.
    #[test]
    fn a_window_goes_when_the_last_panel_leaves_it() {
        let mut ws = bare();
        ws.screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        ws.float(BRUSH);
        assert_eq!(ws.floating.len(), 1);
        ws.dock(BRUSH, LAYERS);
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

        ws.dock(BRUSH, LAYERS);
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

    /// **Nothing can be docked into something that is itself floating.**
    ///
    /// It would leave both floating and nothing docked, which is not what "put it back" means to
    /// anybody -- and the destination list is built from the docked panels for exactly that
    /// reason, so this is the belt to that list's braces.
    #[test]
    fn a_floating_panel_is_not_offered_as_somewhere_to_dock() {
        let mut ws = bare();
        ws.float(BRUSH);
        ws.float(COLOUR);

        let offered = ws.popup_controls(PopupKind::Settings(BRUSH));
        assert!(
            !offered.iter().any(|c| c.id() == Some(DOCK_BASE + COLOUR.0)),
            "a floating panel was offered as somewhere to put another one back"
        );
        assert!(
            offered.iter().any(|c| c.id() == Some(DOCK_BASE + LAYERS.0)),
            "and a docked one should be"
        );

        // And asking directly is refused rather than obeyed.
        ws.dock(BRUSH, COLOUR);
        assert!(ws.is_floating(BRUSH), "it should still be floating");
        assert!(!ws.is_docked(BRUSH));
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
            afloat
                .iter()
                .any(|c| c.id().is_some_and(|i| i >= DOCK_BASE)),
            "it should be offered somewhere to go back to"
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

    /// **A panel with nothing to set says so.**
    ///
    /// The canvas has no controls at all, so offering it a choice between running them across or
    /// down was nonsense -- and an empty popup is indistinguishable from one that failed to open.
    #[test]
    fn a_panel_with_nothing_to_set_says_so() {
        let ws = bare();
        let controls = ws.popup_controls(PopupKind::Settings(CANVAS));
        assert!(
            controls
                .iter()
                .all(|c| matches!(c, crate::panel_ui::Control::Label { .. })),
            "the canvas was offered something to change"
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

        // And the switches actually bring them back.
        for kind in PANELS {
            ws.toggle(kind.id);
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
            ws.drag.armed(),
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
        ws.toggle(HISTORY);
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
        ws.toggle(HISTORY);
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
        assert!(drag.armed(), "the hold should have armed the move");

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
