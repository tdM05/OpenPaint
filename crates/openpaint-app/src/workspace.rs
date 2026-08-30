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
    /// about the layout. When per-panel settings arrive this is what they start from.
    pub direction: Direction,
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
    },
    PanelKind {
        id: PanelId(1),
        name: "Tools",
        header: HeaderStyle::Compact,
        direction: Direction::Wrap,
    },
    PanelKind {
        id: PanelId(2),
        name: "Canvas",
        header: HeaderStyle::Named,
        direction: Direction::Column,
    },
    PanelKind {
        id: PanelId(3),
        name: "Brush",
        header: HeaderStyle::Named,
        direction: Direction::Column,
    },
    PanelKind {
        id: PanelId(4),
        name: "Layers",
        header: HeaderStyle::Named,
        direction: Direction::Column,
    },
    PanelKind {
        id: PanelId(5),
        name: "Colour",
        header: HeaderStyle::Named,
        direction: Direction::Column,
    },
    PanelKind {
        id: PanelId(6),
        name: "History",
        header: HeaderStyle::Named,
        direction: Direction::Column,
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
    let dir = dirs::data_local_dir()?.join("OpenPaint");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("workspace.json"))
}

/// Read the saved arrangement, if there is one.
///
/// A missing file is the ordinary case. A broken one is reported and the default used, rather
/// than opening into some half-parsed workspace — the same tolerance the brush library and the
/// theme take with theirs.
fn load_layout() -> Option<(Layout, std::collections::HashMap<u32, PanelOptions>)> {
    let path = layout_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<SavedWorkspace>(&text) {
        Ok(saved) => {
            let layout = Layout::from_saved(&saved.layout, id_for);
            let options = options_from_saved(&saved.panels);
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
            Some((layout, options))
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
        let (layout, options) = load_layout().unwrap_or_else(|| (default_layout(), <_>::default()));
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
        }
    }
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
}

/// Where a hand-edited theme lives, beside the brush library.
fn theme_path() -> Option<std::path::PathBuf> {
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

/// The arrangement a first-time artist finds.
///
/// **Only a file's worth of decisions**, which is what §1c bought: this is a default, not a
/// structure. Menu across the top, tools down the left, canvas taking what is left, and the
/// panels people reach for most stacked on the right.
#[must_use]
pub fn default_layout() -> Layout {
    let mut l = Layout::single(CANVAS);

    // A menu strip across the top. The weights matter as much as the structure: an `insert` halves
    // whatever it lands on, so without these the menu bar would take half the window — which a
    // test caught and no amount of reading the code would have.
    l.insert(&[], Zone::Top, MENU);
    // Sized so the strip actually holds one row of controls and its padding at an ordinary window
    // height, rather than a fraction that looked about right: see the test below, which works the
    // requirement out from the metrics instead of trusting these numbers.
    l.set_weight(&[0], 0.05);
    l.set_weight(&[1], 0.95);

    // Tools down the left, canvas taking what is left, panels down the right.
    l.insert(&[1], Zone::Left, TOOLS);
    l.insert(&[1, 1], Zone::Right, LAYERS);
    l.set_weight(&[1, 0], 0.07);
    l.set_weight(&[1, 1], 0.75);
    l.set_weight(&[1, 2], 0.18);

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
        mut contents: impl FnMut(PanelId, &mut egui::Ui, Direction),
    ) {
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("workspace"),
        ));
        let m = self.theme.metrics;
        let p = self.theme.palette;

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
        let on_popup = popup_press(
            self.popup.map(|p| p.rect),
            pressed,
            pointer.map(|q| (q.x, q.y)),
        );
        if on_popup == PopupPress::Close {
            self.popup = None;
        }

        let preview = if on_popup == PopupPress::Consume {
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
            contents(showing, &mut ui, self.direction_of(showing));
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

        // --- the popup, above everything, drawn by the same descriptor layer as any panel ---
        if let Some(popup) = self.popup {
            let controls = self.popup_controls(popup.kind);
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
            let changes = crate::panel_draw::show(
                &mut ui,
                &controls,
                &theme,
                Direction::Column,
                &mut self.popup_input,
            );
            for change in changes {
                self.apply_popup(popup.kind, change);
            }
        }
    }

    /// The controls a popup is showing.
    #[must_use]
    fn popup_controls(&self, kind: PopupKind) -> Vec<crate::panel_ui::Control> {
        use crate::panel_ui::Control;
        match kind {
            PopupKind::Panels => panel_list_controls(self),
            PopupKind::Settings(panel) => {
                let mut controls = vec![Control::Label {
                    text: format!("{} settings", name_of(panel)),
                }];
                let current = self.direction_of(panel);
                controls.extend(DIRECTIONS.iter().map(|(id, name, d)| Control::Choice {
                    id: *id,
                    text: (*name).to_owned(),
                    selected: current == *d,
                }));
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

    /// Set which way a panel's controls run, and remember it.
    pub fn set_direction(&mut self, panel: PanelId, direction: Direction) {
        self.options.entry(panel.0).or_default().direction = Some(direction);
        self.remember();
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
                    Outcome::Settings(panel) => {
                        self.popup_wanted = Some(PopupKind::Settings(panel))
                    }
                    Outcome::Moved | Outcome::Resized | Outcome::Nothing | Outcome::Switched => {}
                }
                None
            }
            Pulse::Track => self.drag.drag(&mut self.layout, screen, x, y, now_ms),
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
        Outcome::Nothing | Outcome::Switched | Outcome::Settings(_) => false,
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

    /// Holding a panel's header and letting go without moving asks it what it offers.
    ///
    /// The one gesture left over once "hold then move" means move, so it costs no pixels and every
    /// panel has a header to press.
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
        for t in [1.0, 2.0] {
            ws.gesture(
                screen,
                pulse(false, false, true),
                Some((10.0, 10.0)),
                HOLD_MS + t,
                |_, _| unreachable!(),
            );
        }
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((10.0, 10.0)),
            HOLD_MS + 3.0,
            |_, _| unreachable!(),
        );
        assert_eq!(
            ws.popup_wanted,
            Some(PopupKind::Settings(BRUSH)),
            "holding a header and letting go should ask the panel what it offers"
        );
    }

    /// A settings popup offers every direction, and marks the one in force.
    #[test]
    fn the_settings_popup_offers_every_direction() {
        let mut ws = bare();
        ws.set_direction(LAYERS, Direction::Wrap);
        let controls = ws.popup_controls(PopupKind::Settings(LAYERS));
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
        let screen = Rect::new(0.0, 0.0, 1400.0, 900.0);
        let m = crate::theme::Theme::default().metrics;
        let layout = default_layout();
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
        let needed = m.row + m.padding * 2.0;
        assert!(
            menu.h >= needed,
            "the menu strip is {} tall and needs {needed}",
            menu.h
        );

        // The tool rail is a column down the side: it needs the width of one button plus padding.
        let tools = content_of(TOOLS);
        let button = Control::Choice {
            id: 0,
            text: String::new(),
            selected: false,
        }
        .width(&m, LABEL)
            + m.padding * 2.0;
        assert!(
            tools.w >= button,
            "the tool rail is {} wide and needs {button}",
            tools.w
        );
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
        use crate::panel_drag::{pulse, Pulse, Target, HOLD_MS};
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
            HOLD_MS + 1.0,
            |_, _| unreachable!("only a press asks what is under the pointer"),
        );
        assert!(ws.drag.armed(), "the hold should have armed the move");
        // Frame 3: carried over the canvas.
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((dx, dy)),
            HOLD_MS + 2.0,
            |_, _| unreachable!(),
        );
        // Frame 4: let go. egui reports the release *and* the button already up, together.
        assert_eq!(pulse(false, true, false), Pulse::Release);
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((dx, dy)),
            HOLD_MS + 3.0,
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
        use crate::panel_drag::{pulse, Target, HOLD_MS};
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
            HOLD_MS + 1.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((dx, dy)),
            HOLD_MS + 2.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((dx, dy)),
            HOLD_MS + 3.0,
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
        use crate::panel_drag::{pulse, Target, HOLD_MS};
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
            HOLD_MS + 1.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px + 80.0, py)),
            HOLD_MS + 2.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((px + 80.0, py)),
            HOLD_MS + 3.0,
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
        use crate::panel_drag::{pulse, Target, HOLD_MS};
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
            HOLD_MS + 1.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((px + 120.0, py)),
            HOLD_MS + 2.0,
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
        use crate::panel_drag::{pulse, Target, HOLD_MS};
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
            HOLD_MS + 1.0,
            |_, _| unreachable!(),
        );
        assert!(ws.drag.active(), "there should be a gesture to lose");

        // The button is not down and no release was reported: the pointer is gone.
        ws.gesture(
            screen,
            pulse(false, false, false),
            Some((700.0, 700.0)),
            HOLD_MS + 2.0,
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
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((10.0, 10.0)),
            HOLD_MS + 1.0,
            |_, _| unreachable!(),
        );
        // Well outside the window.
        let (ox, oy) = (screen.w + 200.0, screen.h + 200.0);
        ws.gesture(
            screen,
            pulse(false, false, true),
            Some((ox, oy)),
            HOLD_MS + 2.0,
            |_, _| unreachable!(),
        );
        ws.gesture(
            screen,
            pulse(false, true, false),
            Some((ox, oy)),
            HOLD_MS + 3.0,
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
        // Nor is asking a panel what it offers: nothing has changed until something is chosen.
        assert!(!saves(&Outcome::Settings(BRUSH)));
    }

    /// Press where a pointer would land, wait out the hold, move to another panel, let go.
    ///
    /// Every earlier test built a `Target::Tab` by hand and started from there, which skipped
    /// `chrome::target_at` entirely — the one piece that has to agree with what is drawn. A move
    /// that stopped working in the app while every drag test stayed green is exactly what that
    /// gap looks like.
    #[test]
    fn a_panel_can_be_moved_from_a_press_on_what_is_drawn() {
        use crate::panel_drag::{Outcome, PanelDrag, HOLD_MS};
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
        drag.drag(&mut layout, screen, px, py, HOLD_MS + 1.0);
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
        drag.drag(&mut layout, screen, dx, dy, HOLD_MS + 2.0);
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
