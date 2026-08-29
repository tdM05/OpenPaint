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
use crate::panel_drag::{Outcome, PanelDrag, Preview};
use crate::theme::{Color, Theme};

/// A panel the app can show.
///
/// A table rather than a match, so adding one is a row. The layout never sees this — it holds the
/// id and nothing else (§1c).
pub struct PanelKind {
    pub id: PanelId,
    pub name: &'static str,
    pub header: HeaderStyle,
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
    },
    PanelKind {
        id: PanelId(1),
        name: "Tools",
        header: HeaderStyle::Compact,
    },
    PanelKind {
        id: PanelId(2),
        name: "Canvas",
        header: HeaderStyle::Named,
    },
    PanelKind {
        id: PanelId(3),
        name: "Brush",
        header: HeaderStyle::Named,
    },
    PanelKind {
        id: PanelId(4),
        name: "Layers",
        header: HeaderStyle::Named,
    },
    PanelKind {
        id: PanelId(5),
        name: "Colour",
        header: HeaderStyle::Named,
    },
    PanelKind {
        id: PanelId(6),
        name: "History",
        header: HeaderStyle::Named,
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

/// The workspace: a layout, its history, the gesture in progress, and the look.
pub struct Workspace {
    pub layout: Layout,
    pub history: LayoutHistory,
    pub theme: Theme,
    drag: PanelDrag,
    /// Where the canvas panel ended up last frame, so the renderer can put the canvas there.
    canvas_rect: Option<Rect>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            layout: default_layout(),
            history: LayoutHistory::default(),
            theme: load_theme().unwrap_or_default(),
            drag: PanelDrag::default(),
            canvas_rect: None,
        }
    }
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
    l.set_weight(&[0], 0.04);
    l.set_weight(&[1], 0.96);

    // Tools down the left, canvas taking what is left, panels down the right.
    l.insert(&[1], Zone::Left, TOOLS);
    l.insert(&[1, 1], Zone::Right, LAYERS);
    l.set_weight(&[1, 0], 0.045);
    l.set_weight(&[1, 1], 0.775);
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

    /// Whether a panel gesture owns the pointer, so the canvas should not also act on it.
    ///
    /// Unused while the workspace only covers the pointer egui already routes; it is what the
    /// canvas will ask once pen input is routed to the UI directly (Q14).
    #[expect(
        dead_code,
        reason = "wanted when pen input reaches the UI without synthesis"
    )]
    #[must_use]
    pub fn busy(&self) -> bool {
        self.drag.active()
    }

    /// Abandon a panel drag without applying it.
    ///
    /// Escape, and the same promise a transform makes (§5e): a gesture you have thought better of
    /// costs nothing, because nothing has happened to the layout until you let go.
    pub fn cancel_drag(&mut self) -> bool {
        let was = self.drag.active();
        self.drag.cancel();
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

    /// Undo the last layout change. Returns whether there was one.
    pub fn undo(&mut self) -> bool {
        match self.history.undo(&self.layout) {
            Some(previous) => {
                self.layout = previous;
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
        mut contents: impl FnMut(PanelId, &mut egui::Ui),
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
        let (pointer, pressed, released, now_ms) = ctx.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.time * 1000.0,
            )
        });
        // Painting is demand-driven, so a hold with the pointer still would never fire: no events,
        // no frames, and the timer never read. Reported as the hold "sometimes" not working — it
        // was whenever the pen was steady enough to stop producing motion.
        if let Some(left) = self.drag.waiting_ms(now_ms) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                (left / 1000.0).max(0.008),
            ));
        }
        let mut preview = None;
        if let Some(pos) = pointer {
            let (x, y) = (pos.x, pos.y);
            if pressed {
                let target = chrome::target_at(
                    &placed,
                    &splitters,
                    &m,
                    style_of,
                    |pl, i| self.measure(ctx, pl.tabs.get(i).copied()),
                    x,
                    y,
                );
                self.drag.press(&self.layout, &target, x, y, now_ms);
            } else if released {
                let outcome = self
                    .drag
                    .release(&mut self.layout, &mut self.history, screen, x, y);
                // A panel let go outside is asked to float; until floating windows exist, put it
                // back rather than dropping it on the floor. Silently losing a panel would be the
                // worst possible answer (§6b).
                if let Outcome::Floated(panel) = outcome {
                    self.open(panel);
                }
            } else {
                preview = self.drag.drag(&mut self.layout, screen, x, y, now_ms);
            }
        }

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
                    self.measure(ctx, slot.tabs.get(i).copied())
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
                self.measure(ctx, slot.tabs.get(i).copied())
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
                        let colour = if t.active { p.bright } else { p.dim };
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

            let content = to_egui(inset(c.content, m.padding));
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::LayerId::new(egui::Order::Middle, egui::Id::new(("panel", showing.0))),
                egui::Id::new(("panel-ui", showing.0)),
                egui::UiBuilder::new().max_rect(content),
            );
            // Contents are clipped to their own panel, so a list too long for its slot cannot
            // draw over the panel beside it.
            ui.set_clip_rect(content);
            contents(showing, &mut ui);
        }

        // --- the divider under the pointer, thickened so it can be seen as well as caught ---
        //
        // Drawn last of the chrome so it sits over the panels either side, and only when the
        // pointer is close: at rest the gutter is a hairline and the workspace stays quiet.
        if let Some(pos) = pointer {
            if preview.is_none() || matches!(preview, Some(Preview::Resizing)) {
                for s in &splitters {
                    if !s.rect.contains(pos.x, pos.y) {
                        continue;
                    }
                    // Centred on the boundary, at the *drawn* thickness rather than the grab
                    // width -- the point is to show where it is, not how big the target is.
                    let t = m.splitter_hover;
                    let bar = match s.axis {
                        crate::layout::Axis::Horizontal => {
                            Rect::new(s.rect.x + s.rect.w / 2.0 - t / 2.0, s.rect.y, t, s.rect.h)
                        }
                        crate::layout::Axis::Vertical => {
                            Rect::new(s.rect.x, s.rect.y + s.rect.h / 2.0 - t / 2.0, s.rect.w, t)
                        }
                    };
                    painter.rect_filled(to_egui(bar), t / 2.0, rgb(p.state));
                    break;
                }
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
                        self.measure(ctx, slot.tabs.get(i).copied())
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
    }

    /// Width of a tab's label, measured by the thing that will draw it.
    fn measure(&self, ctx: &egui::Context, panel: Option<PanelId>) -> f32 {
        let Some(panel) = panel else {
            return 0.0;
        };
        let font = egui::FontId::proportional(self.theme.metrics.label);
        ctx.fonts(|f| {
            f.layout_no_wrap(name_of(panel).to_owned(), font, egui::Color32::WHITE)
                .size()
                .x
        })
    }
}

/// A leaf's header style: compact only when *every* panel in it wants that.
///
/// Stacking a named panel with a compact one has to show names, or the named one becomes
/// unreachable. Decided from the panels rather than from the leaf, so it is still a property of
/// panels and not a rule about places.
fn style_of(slot: &crate::layout::Placed) -> HeaderStyle {
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
