//! Moving panels and dividers: the gesture, not the tree.
//!
//! [`crate::layout`] is the structure; this is what a pointer does to it. Kept apart because the
//! tree is pure geometry and this is a state machine with a clock in it, and because the tree has
//! to stay testable without inventing a gesture to reach it.
//!
//! # Hold, *then* move — and a tap is only ever a tap
//!
//! Pressing a tab is ambiguous: it could mean "show me that one" or "I am taking this somewhere".
//! A mouse disambiguates by hovering first; **a pen arrives already down** and offers no such
//! warning (§1c).
//!
//! The first version treated movement past [`SLOP`] *or* a hold past [`HOLD_MS`] as a drag, and
//! that was wrong in practice: a pen tap drifts a little and takes a moment, so ordinary taps kept
//! becoming accidental moves. Reported as *"sometimes you just wanna use a tab but then it starts
//! moving the editor"*, and it is worth naming why the code drifted from the design — §1c said
//! "press-and-hold-then-move drags" and the implementation said "hold **or** move", which is a
//! different rule that reads almost the same.
//!
//! So, precisely:
//!
//! - **Held past [`HOLD_MS`]** — the panel *arms*. It has not moved yet; it is loose.
//! - **Moved after arming** — it follows the pointer.
//! - **Released before arming, having barely moved** — a tap. Switches tabs and nothing else.
//! - **Released before arming, having moved** — nothing at all, because a stray swipe across a
//!   header meant neither thing.
//!
//! Movement alone therefore never starts a drag. That costs a mouse user a fifth of a second
//! before a panel comes loose, which is the right trade for an app whose primary input is a pen.
//!
//! A tap must leave no trace — in particular it must not push anything onto the layout's undo
//! stack, or Ctrl+Z would start walking back through tab selections.
//!
//! # Why the drop target is remembered by panel, not by path
//!
//! Applying a move means removing the panel and inserting it again, and **removing can collapse
//! the tree**: an emptied leaf disappears, a split left with one child dissolves into it, and
//! every path after that point shifts. A path captured before the removal can therefore name a
//! different node afterwards, or nothing at all.
//!
//! So the target is held as *a panel that lives in the target leaf* — an opaque id, which no
//! amount of collapsing can invalidate — and the path is looked up again after the removal. That
//! is the one ordering hazard in this module and it is exactly §11a's shape: the operation is
//! correct, and doing it in the wrong order is silently wrong rather than loud.

use crate::layout::{Axis, Layout, LayoutHistory, PanelId, Path, Placed, Rect, Zone};

/// How far a pointer may wander and still count as a tap, in logical units.
///
/// Generous, because a pen resting on glass drifts and a hand shakes. This no longer decides
/// whether something is a *drag* — only whether a release was a tap or nothing.
pub const SLOP: f32 = 9.0;

/// How long a press must be held before the panel comes loose, in milliseconds.
///
/// The touch convention, shortened a little: it is now the *only* way to start a drag, so it sits
/// on the path of everything rather than being one of two routes. Long enough that no ordinary tab
/// tap reaches it, short enough that picking a panel up does not feel like waiting.
pub const HOLD_MS: f64 = 200.0;

/// What the pointer went down on. The caller decides this, because it drew the chrome and is the
/// only thing that knows where a tab ends and a header begins.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A tab in a leaf's header.
    Tab { path: Path, tab: usize },
    /// The divider after child `index` of the split at `path`.
    Splitter { path: Path, index: usize },
    /// Anywhere else — the panel's own content, which this module does not touch.
    Elsewhere,
}

/// What a gesture in progress wants drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    /// A panel is in the air. `over` is the drop it would land in, or `None` when the pointer is
    /// outside the workspace and letting go would float it.
    Carrying {
        panel: PanelId,
        over: Option<Landing>,
    },
    /// A divider is being dragged; the layout has already moved with it.
    Resizing,
}

/// The drop a release would perform, and the rectangle to light up for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Landing {
    pub zone: Zone,
    /// The leaf being dropped onto.
    pub rect: Rect,
}

/// What a release did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// Nothing happened, and nothing was recorded.
    Nothing,
    /// A tab was brought to the front. Not undoable, deliberately — see the module note.
    Switched,
    /// A panel moved, and the layout change is on the undo stack.
    Moved,
    /// A divider moved.
    Resized,
    /// The panel was let go outside the workspace and wants its own window.
    ///
    /// Reported rather than performed: floating is a second [`Layout`] in a second window, and
    /// whose window that is belongs to the shell.
    Floated(PanelId),
}

#[derive(Clone, Debug)]
enum Grab {
    Tab {
        panel: PanelId,
        tab: usize,
        path: Path,
        press: (f32, f32),
        press_ms: f64,
        /// Whether the panel has come loose. Set by time alone — see the module note.
        armed: bool,
        /// Whether the pointer has wandered past [`SLOP`] since the press.
        ///
        /// Only decides whether a release that never armed was a tap or nothing at all.
        strayed: bool,
    },
    Splitter {
        path: Path,
        index: usize,
        axis: Axis,
        last: (f32, f32),
    },
}

/// The panel-dragging state machine.
#[derive(Debug, Default)]
pub struct PanelDrag {
    grab: Option<Grab>,
}

impl PanelDrag {
    /// Whether a gesture is in progress.
    #[must_use]
    pub fn active(&self) -> bool {
        self.grab.is_some()
    }

    /// Whether a press is waiting to arm, and how long is left.
    ///
    /// **Painting is demand-driven**, so a hold that nobody asks to draw simply never fires: with
    /// the pointer still, no events arrive, no frame is drawn, and the timer is never read. The
    /// caller uses this to keep frames coming until the panel comes loose. Same trap as every
    /// other timed thing in this app — see `Stabilizer` and the note on `request_redraw`.
    #[must_use]
    pub fn waiting_ms(&self, now_ms: f64) -> Option<f64> {
        match self.grab {
            Some(Grab::Tab {
                press_ms,
                armed: false,
                ..
            }) => Some((HOLD_MS - (now_ms - press_ms)).max(0.0)),
            _ => None,
        }
    }

    /// Begin a gesture. Returns whether this module took the pointer.
    ///
    /// A press on anything but a tab or a divider is declined, so the panel's own content sees it
    /// — the same shape as [`crate::Capture`], and for the same reason: one decision at the press,
    /// held to the release.
    pub fn press(&mut self, layout: &Layout, target: &Target, x: f32, y: f32, now_ms: f64) -> bool {
        self.grab = match target {
            // A target naming a tab or a split that is not there is declined rather than trusted:
            // the caller hit-tested against a layout, and nothing guarantees it was this one.
            Target::Tab { path, tab } => layout.tab_at(path, *tab).map(|panel| Grab::Tab {
                panel,
                tab: *tab,
                path: path.clone(),
                press: (x, y),
                press_ms: now_ms,
                armed: false,
                strayed: false,
            }),
            Target::Splitter { path, index } => {
                layout.split_axis(path).map(|axis| Grab::Splitter {
                    path: path.clone(),
                    index: *index,
                    axis,
                    last: (x, y),
                })
            }
            Target::Elsewhere => None,
        };
        self.grab.is_some()
    }

    /// Continue a gesture. A divider moves the layout as it goes; a panel only previews.
    ///
    /// Resizing applies live because the artist is watching the thing they are sizing. A move does
    /// not, because a panel that rearranged the workspace on the way past every drop zone would be
    /// unusable — and because a preview costs nothing to abandon.
    pub fn drag(
        &mut self,
        layout: &mut Layout,
        area: Rect,
        x: f32,
        y: f32,
        now_ms: f64,
    ) -> Option<Preview> {
        match self.grab.as_mut()? {
            Grab::Splitter {
                path,
                index,
                axis,
                last,
            } => {
                let (dx, dy) = (x - last.0, y - last.1);
                *last = (x, y);
                let extent = match axis {
                    Axis::Horizontal => area.w,
                    Axis::Vertical => area.h,
                };
                if extent > 0.0 {
                    let delta = match axis {
                        Axis::Horizontal => dx,
                        Axis::Vertical => dy,
                    } / extent;
                    layout.drag_splitter(path, *index, delta);
                }
                Some(Preview::Resizing)
            }
            Grab::Tab {
                panel,
                press,
                press_ms,
                armed,
                strayed,
                ..
            } => {
                *strayed = *strayed || (x - press.0).hypot(y - press.1) >= SLOP;
                // Time alone arms it. Movement is deliberately *not* a trigger: that is the whole
                // correction, and re-adding an `|| moved` here brings the accidental drags back.
                *armed = *armed || (now_ms - *press_ms) >= HOLD_MS;
                if !*armed {
                    return None;
                }
                let panel = *panel;
                let over = layout.leaf_at(area, x, y).map(|placed| Landing {
                    zone: Layout::zone_at(placed.rect, x, y),
                    rect: placed.rect,
                });
                Some(Preview::Carrying { panel, over })
            }
        }
    }

    /// End a gesture, applying it.
    ///
    /// Records the layout *before* changing it, and only when it actually changes — so a tap, or a
    /// drop back where it started, leaves the undo stack alone.
    pub fn release(
        &mut self,
        layout: &mut Layout,
        history: &mut LayoutHistory,
        area: Rect,
        x: f32,
        y: f32,
    ) -> Outcome {
        let Some(grab) = self.grab.take() else {
            return Outcome::Nothing;
        };
        match grab {
            Grab::Splitter { .. } => Outcome::Resized,
            Grab::Tab {
                panel,
                tab,
                path,
                armed,
                strayed,
                ..
            } => {
                if !armed {
                    if strayed {
                        // A swipe across a header that never became a drag. It meant neither
                        // thing, so it does neither — switching tabs here is how a stray stroke
                        // ends up changing what you are looking at.
                        return Outcome::Nothing;
                    }
                    // A tap. Deliberately not recorded: Ctrl+Z must walk back through arrangement,
                    // not through which tab you were looking at.
                    layout.set_active(&path, tab);
                    return Outcome::Switched;
                }
                let Some(placed) = layout.leaf_at(area, x, y) else {
                    return Outcome::Floated(panel);
                };
                let zone = Layout::zone_at(placed.rect, x, y);
                if is_noop(&placed, panel, zone) {
                    return Outcome::Nothing;
                }
                // Anchor the target to a panel rather than to its path: the removal below can
                // collapse a leaf and shift every path after it. See the module note.
                //
                // Finding no anchor is also the answer to the last no-op case, which is why
                // `is_noop` does not repeat it: if the only panel in the target leaf is the one
                // being dragged, splitting that leaf would put the panel beside itself. A sabotage
                // proved a clause saying so in `is_noop` was unreachable, and one place deciding
                // beats two that agree by luck.
                let Some(anchor) = placed.tabs.iter().copied().find(|p| *p != panel) else {
                    return Outcome::Nothing;
                };

                let before = layout.clone();
                layout.remove(panel);
                let Some((fresh, _)) = layout.find(anchor) else {
                    // The anchor vanished, which should be impossible — it was a different panel
                    // in a leaf that still had it. Put the layout back rather than guess.
                    *layout = before;
                    return Outcome::Nothing;
                };
                layout.insert(&fresh, zone, panel);
                history.record(before);
                Outcome::Moved
            }
        }
    }

    /// Abandon a gesture without applying it, for Escape or a lost pointer.
    pub fn cancel(&mut self) {
        self.grab = None;
    }
}

/// Whether a drop would change nothing: back onto the leaf it came from, in the centre.
///
/// Worth its own answer rather than letting the move happen and produce an identical tree, because
/// an undo entry that restores what is already there reads as a broken Ctrl+Z.
///
/// The *other* no-op — a lone panel dropped on the edge of its own leaf — is decided at the anchor
/// lookup in `release` rather than repeated here. See the note there.
fn is_noop(placed: &Placed, panel: PanelId, zone: Zone) -> bool {
    placed.tabs.contains(&panel) && zone == Zone::Center
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: PanelId = PanelId(1);
    const LAYERS: PanelId = PanelId(2);
    const HISTORY: PanelId = PanelId(3);

    fn area() -> Rect {
        Rect::new(0.0, 0.0, 1000.0, 800.0)
    }

    /// Canvas on the left, Layers and History stacked on the right.
    fn workspace() -> Layout {
        let mut l = Layout::single(CANVAS);
        l.insert(&[], Zone::Right, LAYERS);
        l.insert(&[1], Zone::Center, HISTORY);
        l
    }

    fn tab(path: &[usize], tab: usize) -> Target {
        Target::Tab {
            path: path.to_vec(),
            tab,
        }
    }

    /// A quick press and release on a tab shows it — and records nothing, because Ctrl+Z must walk
    /// back through arrangement rather than through which tab you last looked at.
    #[test]
    fn a_tap_switches_the_tab_and_records_nothing() {
        let mut l = workspace();
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        assert_eq!(l.resolve(area())[1].active, 1, "History is showing");
        assert!(d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0));
        // A little wobble, well inside the slop, and released promptly.
        assert_eq!(d.drag(&mut l, area(), 702.0, 11.0, 40.0), None);
        assert_eq!(
            d.release(&mut l, &mut h, area(), 702.0, 11.0),
            Outcome::Switched
        );

        assert_eq!(l.resolve(area())[1].active, 0, "Layers is showing now");
        assert_eq!(h.depth(), (0, 0), "a tap is not an undoable edit");
    }

    /// **Movement alone never starts a drag**, however far it goes.
    ///
    /// The correction that this whole module was rewritten for. The first version treated a move
    /// past the slop as a drag, and ordinary pen taps — which drift — kept becoming accidental
    /// panel moves. A test asserting the old rule passed the whole time, which is why it is
    /// replaced rather than relaxed.
    #[test]
    fn movement_alone_never_starts_a_drag() {
        let mut l = workspace();
        let mut d = PanelDrag::default();
        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);

        // Right across the window, well inside the hold time.
        for x in [710.0, 760.0, 900.0, 400.0] {
            assert_eq!(
                d.drag(&mut l, area(), x, 300.0, 40.0),
                None,
                "moving to {x} before the hold must not lift anything"
            );
        }
    }

    /// A swipe across a header that never armed does neither thing: it does not move a panel, and
    /// it does not switch tabs either. A stray stroke must not change what you are looking at.
    #[test]
    fn a_stray_swipe_does_nothing_at_all() {
        let mut l = workspace();
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();
        let before = l.clone();

        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);
        d.drag(&mut l, area(), 780.0, 12.0, 30.0);
        assert_eq!(
            d.release(&mut l, &mut h, area(), 780.0, 12.0),
            Outcome::Nothing
        );
        assert_eq!(l, before, "the tab on show did not change either");
        assert_eq!(h.depth(), (0, 0));
    }

    /// Once armed, movement carries the panel — which is the other half of the rule.
    #[test]
    fn movement_after_arming_carries_the_panel() {
        let mut l = workspace();
        let mut d = PanelDrag::default();
        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);

        assert!(d.drag(&mut l, area(), 700.0, 10.0, HOLD_MS + 1.0).is_some());
        let preview = d
            .drag(&mut l, area(), 400.0, 400.0, HOLD_MS + 20.0)
            .expect("armed, so it follows");
        assert!(matches!(preview, Preview::Carrying { panel: LAYERS, .. }));
    }

    /// A press waiting to arm asks for frames, because painting is demand-driven and a hold that
    /// nobody draws never fires. Reported as "tap and hold sometimes does not show the menu" — it
    /// was not sometimes, it was whenever the pointer was still enough to stop producing events.
    #[test]
    fn a_waiting_press_asks_for_the_frames_that_let_it_fire() {
        let l = workspace();
        let mut d = PanelDrag::default();
        assert_eq!(d.waiting_ms(0.0), None, "nothing is waiting yet");

        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);
        let left = d.waiting_ms(50.0).expect("a press is waiting");
        assert!(
            (left - (HOLD_MS - 50.0)).abs() < 0.001,
            "it should report the time left, got {left}"
        );

        let mut l2 = workspace();
        d.drag(&mut l2, area(), 700.0, 10.0, HOLD_MS + 1.0);
        assert_eq!(d.waiting_ms(HOLD_MS + 1.0), None, "armed, so nothing waits");
    }

    /// Holding still lifts too, so a pen user sees the panel come loose *before* deciding where to
    /// take it. Without this the only way to know a drag has begun is to have already moved.
    #[test]
    fn holding_still_lifts_the_panel_without_moving() {
        let mut l = workspace();
        let mut d = PanelDrag::default();
        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);

        assert_eq!(d.drag(&mut l, area(), 700.0, 10.0, HOLD_MS - 50.0), None);
        let preview = d
            .drag(&mut l, area(), 700.0, 10.0, HOLD_MS + 1.0)
            .expect("held long enough");
        assert!(matches!(preview, Preview::Carrying { .. }));
    }

    /// **The ordering hazard.** Applying a move removes the panel first, and removing can collapse
    /// a leaf and shift every path after it — so a path captured before the removal can name a
    /// different node, or nothing. The target is anchored to a panel id instead, which no amount
    /// of collapsing can invalidate.
    ///
    /// This is the case that exposes it: dragging the *only* panel out of a leaf, so that leaf
    /// disappears and the split around it dissolves, into a target that sits after it.
    #[test]
    fn a_move_survives_the_tree_collapsing_under_it() {
        // Three across; take the first one and drop it on the last.
        let mut l = Layout::single(CANVAS);
        l.insert(&[], Zone::Right, LAYERS);
        l.insert(&[1], Zone::Right, HISTORY);
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        let placed = l.resolve(area());
        assert_eq!(placed.len(), 3);
        let target = placed[2].rect;

        d.press(&l, &tab(&[0], 0), 10.0, 10.0, 0.0);
        d.drag(&mut l, area(), 500.0, 400.0, 300.0);
        let drop = (target.x + target.w / 2.0, target.y + target.h / 2.0);
        assert_eq!(
            d.release(&mut l, &mut h, area(), drop.0, drop.1),
            Outcome::Moved
        );

        // Canvas must be stacked with History, not lost and not put somewhere arbitrary.
        let (path, _) = l.find(CANVAS).expect("the canvas is still in the layout");
        let placed = l.resolve(area());
        let leaf = placed.iter().find(|p| p.path == path).expect("its leaf");
        assert!(
            leaf.tabs.contains(&HISTORY),
            "it should have landed with History, got {:?}",
            leaf.tabs
        );
        assert_eq!(l.resolve(area()).len(), 2, "the emptied leaf collapsed");
    }

    /// An edge drop splits the leaf it landed on.
    #[test]
    fn an_edge_drop_splits_the_target() {
        let mut l = workspace();
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        let canvas_rect = l.resolve(area())[0].rect;
        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);
        d.drag(&mut l, area(), 400.0, 400.0, 300.0);
        // Near the bottom edge of the canvas panel.
        let drop = (
            canvas_rect.x + canvas_rect.w / 2.0,
            canvas_rect.y + canvas_rect.h * 0.95,
        );
        assert_eq!(
            d.release(&mut l, &mut h, area(), drop.0, drop.1),
            Outcome::Moved
        );

        let (path, _) = l.find(LAYERS).expect("layers moved, not vanished");
        let canvas_path = l.find(CANVAS).expect("canvas is there").0;
        assert_ne!(path, canvas_path, "they are in different leaves");
        assert_eq!(h.depth().0, 1, "and the move is undoable");
    }

    /// Dropping a panel back where it came from changes nothing and records nothing. An undo entry
    /// that restores what is already there reads as a broken Ctrl+Z.
    #[test]
    fn dropping_a_panel_back_home_is_not_an_edit() {
        let mut l = workspace();
        let before = l.clone();
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        let home = l.resolve(area())[1].rect;
        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);
        d.drag(&mut l, area(), 800.0, 400.0, 300.0);
        let drop = (home.x + home.w / 2.0, home.y + home.h / 2.0);
        assert_eq!(
            d.release(&mut l, &mut h, area(), drop.0, drop.1),
            Outcome::Nothing
        );
        assert_eq!(l, before);
        assert_eq!(h.depth(), (0, 0));
    }

    /// A lone panel dropped on its own leaf's edge would be split away from itself. Nothing to do.
    #[test]
    fn a_lone_panel_cannot_split_its_own_leaf() {
        let mut l = Layout::single(CANVAS);
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        d.press(&l, &tab(&[], 0), 10.0, 10.0, 0.0);
        d.drag(&mut l, area(), 400.0, 400.0, 300.0);
        assert_eq!(
            d.release(&mut l, &mut h, area(), 990.0, 400.0),
            Outcome::Nothing
        );
        assert_eq!(l, Layout::single(CANVAS));
    }

    /// Let go outside the workspace and the panel wants its own window. Reported rather than
    /// performed, because whose window it is belongs to the shell.
    #[test]
    fn letting_go_outside_asks_to_float() {
        let mut l = workspace();
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);
        d.drag(&mut l, area(), 400.0, 400.0, 300.0);
        assert_eq!(
            d.release(&mut l, &mut h, area(), -50.0, 400.0),
            Outcome::Floated(LAYERS)
        );
        assert_eq!(h.depth(), (0, 0), "nothing has happened to the layout yet");
    }

    /// A divider resizes live, because the artist is watching the thing they are sizing.
    #[test]
    fn a_divider_resizes_as_it_is_dragged() {
        let mut l = workspace();
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();
        let before = l.resolve(area())[0].rect.w;

        let splitter = l.splitters(area(), 8.0)[0].clone();
        assert!(d.press(
            &l,
            &Target::Splitter {
                path: splitter.path.clone(),
                index: splitter.index
            },
            splitter.rect.x,
            400.0,
            0.0
        ));
        // Three steps, and the total must be the distance dragged -- not once, and not loosely.
        // A single drag cannot tell an incremental delta from one measured against the press, and
        // a loose bound cannot tell "followed the pointer" from "shot to the end of its range".
        for step in 1..=3_u8 {
            assert_eq!(
                d.drag(
                    &mut l,
                    area(),
                    splitter.rect.x + 40.0 * f32::from(step),
                    400.0,
                    16.0 * f64::from(step)
                ),
                Some(Preview::Resizing)
            );
        }
        let after = l.resolve(area())[0].rect.w;
        assert!(
            (after - (before + 120.0)).abs() < 2.0,
            "the panel should have followed the divider exactly 120 units, went from {before} to              {after}"
        );
        assert_eq!(
            d.release(&mut l, &mut h, area(), splitter.rect.x + 120.0, 400.0),
            Outcome::Resized
        );
    }

    /// A press on a panel's content is declined, so the panel itself sees it — one decision at the
    /// press, held to the release, exactly as `Capture` does for the canvas.
    #[test]
    fn a_press_on_content_is_not_ours() {
        let l = workspace();
        let mut d = PanelDrag::default();
        assert!(!d.press(&l, &Target::Elsewhere, 400.0, 400.0, 0.0));
        assert!(!d.active());
    }

    /// Cancelling abandons the gesture and leaves the layout alone.
    #[test]
    fn cancelling_abandons_the_gesture() {
        let mut l = workspace();
        let before = l.clone();
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);
        d.drag(&mut l, area(), 400.0, 400.0, 300.0);
        d.cancel();

        assert!(!d.active());
        assert_eq!(
            d.release(&mut l, &mut h, area(), 400.0, 400.0),
            Outcome::Nothing
        );
        assert_eq!(l, before);
        assert_eq!(h.depth(), (0, 0));
    }
}
