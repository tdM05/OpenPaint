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

use crate::chrome::{self, HeaderStyle};
use crate::layout::{Layout, LayoutHistory, PanelId, Rect, Zone};
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
    pub history: LayoutHistory,
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
    /// A floating panel being carried, if one is.
    carrying: Option<Carry>,
    /// The window as it was last drawn.
    ///
    /// Kept because floating a panel has to put it *somewhere*, and the request can arrive from a
    /// menu that has no idea how big the window is. Written every frame; the default is only ever
    /// seen before the first one.
    screen: Rect,
}

/// A panel lifted out of the arrangement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Floating {
    pub panel: PanelId,
    pub rect: Rect,
}

/// A floating panel on its way to disk, named rather than numbered for the same reason the
/// arrangement is: an id is a position in a table, and a table changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SavedFloating {
    panel: String,
    rect: Rect,
}

/// A floating panel under the pointer, and where it was taken hold of.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Carry {
    panel: PanelId,
    /// Where inside the panel the pointer landed, so it does not jump to the corner.
    grip: (f32, f32),
    press_ms: f64,
    moved: bool,
    asked: bool,
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
            history: LayoutHistory::default(),
            theme: load_theme().unwrap_or_default(),
            drag: PanelDrag::default(),
            canvas_rect: None,
            popup: None,
            popup_input: crate::panel_draw::PanelInput::default(),
            popup_wanted: None,
            options,
            floating,
            carrying: None,
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
        .filter_map(|f| {
            name_for(f.panel).map(|panel| SavedFloating {
                panel,
                rect: f.rect,
            })
        })
        .collect()
}

/// Floating panels on their way back, dropping any name this build does not know.
#[must_use]
fn floating_from_saved(saved: &[SavedFloating]) -> Vec<Floating> {
    saved
        .iter()
        .filter_map(|f| {
            id_for(&f.panel).map(|panel| Floating {
                panel,
                rect: f.rect,
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
    rail_content_min(m) + m.gutter
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
        self.history.record(self.layout.clone());
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
        match self.history.undo(&self.layout) {
            Some(previous) => {
                self.layout = previous;
                self.remember();
                true
            }
            None => false,
        }
    }

    /// Redo it.
    pub fn redo(&mut self) -> bool {
        match self.history.redo(&self.layout) {
            Some(next) => {
                self.layout = next;
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
        self.history.record(self.layout.clone());
        self.layout = default_layout();
        self.remember();
    }

    /// Show a panel, bringing it forward if it is already open.
    pub fn open(&mut self, panel: PanelId) {
        if let Some((path, tab)) = self.layout.find(panel) {
            self.layout.set_active(&path, tab);
            return;
        }
        self.history.record(self.layout.clone());
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
        let splitters = self.layout.splitters(screen, m.splitter_grab);

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

        // Pulled out of `self` before the call: the closure below must not hold a borrow of the
        // workspace the gesture is about to change.
        let label = m.label;
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
                let kind = header_under(&placed, &m, pos.x, pos.y)
                    .map_or(PopupKind::Panels, PopupKind::Settings);
                self.open_popup_at(ctx, kind, pos.x, pos.y, screen);
            }
        }
        // A floating panel sits above the arrangement, so it answers before the arrangement does.
        // Its own small gesture rather than `panel_drag`'s: that one moves nodes around a tree,
        // and this one moves a rectangle.
        let float_pulse = crate::panel_drag::pulse(pressed, released, down);
        self.carry_floating(float_pulse, pointer.map(|q| (q.x, q.y)), now_ms);
        let on_float = pressed
            && self.popup.is_none()
            && pointer.is_some_and(|q| self.floating_at(q.x, q.y).is_some());

        let on_popup = popup_press(
            self.popup.map(|p| p.rect),
            pressed,
            pointer.map(|q| (q.x, q.y)),
        );
        if on_popup == PopupPress::Close {
            self.popup = None;
        }

        let preview = if on_float {
            // The floating panel took it. Nothing beneath may also have it, or a press on a
            // palette would move whatever happened to be docked behind it.
            None
        } else if on_popup == PopupPress::Consume {
            // The list owns this press. A drag already in flight still gets its release, which is
            // why only the press is withheld and not the whole frame.
            None
        } else {
            self.gesture(
                screen,
                crate::panel_drag::pulse(pressed, released, down),
                pointer.map(|p| (p.x, p.y)),
                now_ms,
                |x, y| {
                    chrome::target_at(
                        &placed,
                        &splitters,
                        &m,
                        style_of,
                        |pl, i| measure(ctx, label, pl.tabs.get(i).copied()),
                        x,
                        y,
                    )
                },
            )
        };

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
                chrome::panel(slot, &m, style_of(slot), |i| {
                    measure(ctx, m.label, slot.tabs.get(i).copied())
                })
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
            let c = chrome::panel(slot, &m, style, |i| {
                measure(ctx, m.label, slot.tabs.get(i).copied())
            });

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
                        let Some(id) = slot.tabs.get(t.index).copied() else {
                            continue;
                        };
                        // The same quiet mark the dividers use, for the same reason: a pen hovers,
                        // so it can say "this is a thing" before you commit to it. A finger gets
                        // nothing here and loses nothing, because the hold is what starts a
                        // gesture either way.
                        let hovered =
                            pointer.is_some_and(|q| t.rect.contains(q.x, q.y)) && preview.is_none();
                        if hovered && !t.active {
                            painter.rect_filled(to_egui(t.rect), m.radius, rgb(p.edge));
                        }
                        let colour = if t.active || hovered { p.bright } else { p.dim };
                        painter.text(
                            egui::pos2(t.rect.x + m.tab_padding, t.rect.y + t.rect.h / 2.0),
                            egui::Align2::LEFT_CENTER,
                            name_of(id),
                            egui::FontId::proportional(m.label),
                            rgb(colour),
                        );
                        if t.active {
                            // An underline rather than a filled tab: the accent means state, and a
                            // filled tab would put a block of it on screen permanently.
                            painter.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(t.rect.x, t.rect.y + t.rect.h - 2.0),
                                    egui::vec2(t.rect.w, 2.0),
                                ),
                                0.0_f32,
                                rgb(p.state),
                            );
                        }
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
        if let Some(Preview::Waiting { progress, on }) = &preview {
            let alpha = (140.0 * progress).clamp(0.0, 140.0) as u8;
            let tint = egui::Color32::from_rgba_unmultiplied(
                p.state.0[0],
                p.state.0[1],
                p.state.0[2],
                alpha,
            );
            match on {
                Held::Panel { path } => {
                    if let Some(slot) = placed.iter().find(|s| &s.path == path) {
                        let c = chrome::panel(slot, &m, style_of(slot), |i| {
                            measure(ctx, m.label, slot.tabs.get(i).copied())
                        });
                        painter.rect_filled(to_egui(c.header), m.radius, tint);
                    }
                }
                Held::Divider { path, index } => {
                    if let Some(s) = splitters
                        .iter()
                        .find(|s| &s.path == path && s.index == *index)
                    {
                        // Grows from the hairline to the full grab width as the hold completes,
                        // so the target you are about to get is the thing you watch appear.
                        let t = m.splitter_hover.mul_add(*progress, m.gutter);
                        painter.rect_filled(to_egui(centred(s.rect, s.axis, t)), t / 2.0, tint);
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
                for s in &splitters {
                    if !s.rect.contains(pos.x, pos.y) {
                        continue;
                    }
                    // At the *drawn* thickness rather than the grab width: the point is to show
                    // where it is, not how big the target is. A hint for a pen, which hovers; a
                    // finger gets nothing here and does not need to, because the hold is what
                    // actually starts the gesture.
                    let t = m.splitter_hover;
                    painter.rect_filled(to_egui(centred(s.rect, s.axis, t)), t / 2.0, rgb(p.edge));
                    break;
                }
            }
        }

        if let Some(Preview::Resizing { path, index }) = &preview {
            if let Some(s) = splitters
                .iter()
                .find(|s| &s.path == path && s.index == *index)
            {
                let t = m.splitter_hover;
                painter.rect_filled(to_egui(centred(s.rect, s.axis, t)), t / 2.0, rgb(p.state));
            }
        }

        // --- the drop overlay, on top of everything ---
        if let Some(Preview::Carrying { panel, over }) = preview {
            // The panel that has come loose, marked at its source. Without this the hold arms
            // invisibly and the only way to know it worked is to move and find out -- which makes
            // a learnable gesture into one you have to discover. One fill; iPadOS does the same
            // thing by lifting the tile before you move it.
            if let Some((path, _)) = self.layout.find(panel) {
                if let Some(slot) = placed.iter().find(|s| s.path == path) {
                    let c = chrome::panel(slot, &m, style_of(slot), |i| {
                        measure(ctx, m.label, slot.tabs.get(i).copied())
                    });
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

        // --- floating panels, above the arrangement and below the popup ---
        //
        // Drawn with the same chrome as a docked panel, deliberately: a floating palette that
        // looked like a different kind of object would be a second UI to learn, and it is the same
        // panel -- only its rectangle comes from somewhere else.
        for held in &self.floating {
            let slot = crate::layout::Placed {
                path: Vec::new(),
                rect: held.rect,
                tabs: vec![held.panel],
                active: 0,
            };
            let c = chrome::panel(&slot, &m, style_of(&slot), |i| {
                measure(ctx, m.label, slot.tabs.get(i).copied())
            });
            // **One layer for every floating panel**, not one each. egui orders layers within an
            // Order by when they were registered, and a raw layer painter registers no area -- so
            // two panels' layers could interleave, and one panel's background painted over
            // another's contents. Inside a single layer, what is added last is on top, which is
            // exactly the rule wanted here.
            let top = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new("floating"),
            ));
            top.rect_filled(to_egui(c.outer), m.radius, rgb(p.panel));
            top.rect_stroke(
                to_egui(c.outer),
                m.radius,
                egui::Stroke::new(1.0_f32, rgb(p.edge)),
            );
            top.rect_filled(to_egui(c.header), m.radius, rgb(p.header));
            top.text(
                egui::pos2(c.header.x + m.tab_padding, c.header.y + c.header.h / 2.0),
                egui::Align2::LEFT_CENTER,
                name_of(held.panel),
                egui::FontId::proportional(m.label),
                rgb(p.text),
            );
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::LayerId::new(egui::Order::Middle, egui::Id::new("floating")),
                egui::Id::new(("floating-body", held.panel.0)),
                egui::UiBuilder::new().max_rect(to_egui(c.controls.rect())),
            );
            ui.set_clip_rect(to_egui(c.controls.rect()));
            contents(
                held.panel,
                &mut ui,
                self.direction_of(held.panel),
                Place::Panel,
            );
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

    /// Move a floating panel with the pointer, and let a hold ask it for its settings.
    ///
    /// The same two rules as a docked panel, for the same reason: a hand already on the thing is
    /// not asking permission to move it, and holding still is the one gesture left over.
    fn carry_floating(
        &mut self,
        pulse: crate::panel_drag::Pulse,
        pointer: Option<(f32, f32)>,
        now_ms: f64,
    ) {
        use crate::panel_drag::{Pulse, HOLD_MS, SLOP};
        let Some((x, y)) = pointer else {
            self.carrying = None;
            return;
        };
        match pulse {
            Pulse::Press => {
                if self.popup.is_some() {
                    return;
                }
                // Front-most first, and brought to the front, so a press on an overlapping pair
                // picks the one you can see and leaves it where you can see it.
                let Some(panel) = self.floating_at(x, y) else {
                    return;
                };
                let Some(index) = self.floating.iter().position(|f| f.panel == panel) else {
                    return;
                };
                let held = self.floating.remove(index);
                self.floating.push(held);
                self.carrying = Some(Carry {
                    panel,
                    grip: (x - held.rect.x, y - held.rect.y),
                    press_ms: now_ms,
                    moved: false,
                    asked: false,
                });
            }
            Pulse::Track => {
                let Some(carry) = self.carrying.as_mut() else {
                    return;
                };
                let Some(held) = self.floating.iter_mut().find(|f| f.panel == carry.panel) else {
                    return;
                };
                let want = (x - carry.grip.0, y - carry.grip.1);
                carry.moved =
                    carry.moved || (want.0 - held.rect.x).hypot(want.1 - held.rect.y) >= SLOP;
                if carry.moved {
                    held.rect = hold_on_screen(
                        Rect::new(want.0, want.1, held.rect.w, held.rect.h),
                        self.screen,
                        &self.theme.metrics,
                    );
                } else if !carry.asked && now_ms - carry.press_ms >= HOLD_MS {
                    carry.asked = true;
                    let panel = carry.panel;
                    self.carrying = None;
                    self.popup_wanted = Some(PopupKind::Settings(panel));
                }
            }
            Pulse::Release => {
                if self.carrying.take().is_some_and(|c| c.moved) {
                    self.remember();
                }
            }
            Pulse::Lost => self.carrying = None,
        }
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
        self.carrying = None;
        self.popup = None;
        self.popup_wanted = None;
    }

    /// Whether a panel is floating above the arrangement.
    #[must_use]
    pub fn is_floating(&self, panel: PanelId) -> bool {
        self.floating.iter().any(|f| f.panel == panel)
    }

    /// Whether a panel is in the arrangement itself.
    #[must_use]
    pub fn is_docked(&self, panel: PanelId) -> bool {
        self.layout.find(panel).is_some()
    }

    /// Lift a panel out of the arrangement and leave it floating.
    ///
    /// Undoable like any other change to the arrangement, because it *is* one: the panel has left
    /// the tree, and getting it back should not need remembering where it was.
    pub fn float(&mut self, panel: PanelId) {
        if self.is_floating(panel) || !self.is_docked(panel) {
            return;
        }
        self.history.record(self.layout.clone());
        self.layout.remove(panel);
        let screen = self.screen;
        let at = first_float(screen, self.floating.len(), &self.theme.metrics);
        self.floating.push(Floating { panel, rect: at });
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
        self.history.record(self.layout.clone());
        self.floating.retain(|f| f.panel != panel);
        self.layout.insert(&path, Zone::Center, panel);
        self.popup = None;
        self.remember();
    }

    /// Which floating panel is under a point, if any. Front-most first.
    #[must_use]
    fn floating_at(&self, x: f32, y: f32) -> Option<PanelId> {
        self.floating
            .iter()
            .rev()
            .find(|f| f.rect.contains(x, y))
            .map(|f| f.panel)
    }

    /// Choose the icon set, and write the look out.
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
        screen: Rect,
        pulse: crate::panel_drag::Pulse,
        pointer: Option<(f32, f32)>,
        now_ms: f64,
        target_at: impl FnOnce(f32, f32) -> crate::panel_drag::Target,
    ) -> Option<crate::panel_drag::Preview> {
        use crate::panel_drag::Pulse;
        let Some((x, y)) = pointer else {
            // Nowhere to press, drop or follow to. A gesture in flight with no pointer is one
            // whose pointer has gone, whatever this frame claims.
            self.drag.cancel(&mut self.layout);
            return None;
        };
        match pulse {
            Pulse::Press => {
                let target = target_at(x, y);
                self.drag.press(&self.layout, &target, x, y, now_ms);
                None
            }
            Pulse::Release => {
                let outcome = self
                    .drag
                    .release(&mut self.layout, &mut self.history, screen, x, y);
                if saves(&outcome) {
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
                let preview = self.drag.drag(&mut self.layout, screen, x, y, now_ms);
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
                self.drag.cancel(&mut self.layout);
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
fn header_under(placed: &[crate::layout::Placed], m: &Metrics, x: f32, y: f32) -> Option<PanelId> {
    placed.iter().find_map(|slot| {
        let c = chrome::panel(slot, m, style_of(slot), |_| 0.0);
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
            history: LayoutHistory::default(),
            theme: crate::theme::Theme::default(),
            drag: crate::panel_drag::PanelDrag::default(),
            canvas_rect: None,
            popup: None,
            popup_input: crate::panel_draw::PanelInput::default(),
            popup_wanted: None,
            options: std::collections::HashMap::new(),
            floating: Vec::new(),
            carrying: None,
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
        assert_eq!(restored[0].panel, BRUSH);
        assert_eq!(restored[0].rect, where_it_was);

        // A name this build does not know is dropped, not turned into some other panel.
        let text = text.replace("Brush", "Sparkles");
        let back: SavedWorkspace = serde_json::from_str(&text).expect("parse");
        assert!(
            floating_from_saved(&back.floating).is_empty(),
            "a panel this build does not know should be dropped, not turned into another one"
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
            |_, _| target.clone(),
        );
        // Part-way through the hold: still nothing.
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            HOLD_MS / 2.0,
            |_, _| unreachable!(),
        );
        assert_eq!(ws.popup_wanted, None, "it asked before the hold was done");

        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            HOLD_MS + 1.0,
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
                crate::chrome::panel(slot, &m, style_of(slot), |_| LABEL).content
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
        let chrome = crate::chrome::panel(brush, &ws.theme.metrics, style_of(brush), |_| 46.0);
        let (px, py) = (
            chrome.header.x + chrome.header.w / 2.0,
            chrome.header.y + chrome.header.h / 2.0,
        );
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
            |_, _| target.clone(),
        );
        // Frame 2: still held, still there, past the hold.
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px, py)),
            1.0,
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
            |_, _| unreachable!(),
        );
        // Frame 4: let go. egui reports the release *and* the button already up, together.
        assert_eq!(pulse(false, true, false), Pulse::Release);
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((dx, dy)),
            3.0,
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
            |_, _| target.clone(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            1.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((dx, dy)),
            2.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((dx, dy)),
            3.0,
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
            |_, _| target.clone(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px, py)),
            1.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px + 80.0, py)),
            2.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((px + 80.0, py)),
            3.0,
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
            |_, _| target.clone(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px, py)),
            1.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px + 120.0, py)),
            2.0,
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
            |_, _| target.clone(),
        );
        for (t, x) in [(1.0, px), (2.0, px), (3.0, px)] {
            ws.gesture(
                screen,
                pulse(false, false, true),
                Some((x, py)),
                HOLD_MS + t,
                |_, _| unreachable!(),
            );
        }
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((px, py)),
            HOLD_MS + 4.0,
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
            |_, _| target.clone(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            1.0,
            |_, _| unreachable!(),
        );
        assert!(ws.drag.active(), "there should be a gesture to lose");

        // The button is not down and no release was reported: the pointer is gone.
        ws.gesture(
            screen,
            pulse(false, false, false),
            Some((700.0, 700.0)),
            2.0,
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
            |_, _| target.clone(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            1.0,
            |_, _| unreachable!(),
        );
        // Well outside the window.
        let (ox, oy) = (screen.w + 200.0, screen.h + 200.0);
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((ox, oy)),
            2.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((ox, oy)),
            3.0,
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
        let chrome = crate::chrome::panel(brush, &m, style_of(brush), |i| measure(brush, i));
        let (px, py) = (
            chrome.header.x + chrome.header.w / 2.0,
            chrome.header.y + chrome.header.h / 2.0,
        );

        let splitters = layout.splitters(screen, m.splitter_grab);
        let target = crate::chrome::target_at(&placed, &splitters, &m, style_of, measure, px, py);
        assert!(
            matches!(target, crate::panel_drag::Target::Tab { .. }),
            "a press on a drawn header must be a tab: got {target:?}"
        );

        let mut drag = PanelDrag::default();
        let mut history = LayoutHistory::default();
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
        let outcome = drag.release(&mut layout, &mut history, screen, dx, dy);

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
