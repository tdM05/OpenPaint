//! Moving panels and dividers: the gesture, not the tree.
//!
//! [`crate::layout`] is the structure; this is what a pointer does to it. Kept apart because the
//! tree is pure geometry and this is a state machine with a clock in it, and because the tree has
//! to stay testable without inventing a gesture to reach it.
//!
//! # One rule for every layout gesture: hold, then move
//!
//! Nothing rearranges the workspace until the pointer has been held still on it for [`HOLD_MS`].
//! Moving a panel, resizing a divider — both, the same way. Proposed by the author after two
//! rounds of the alternative, and it is a better design than what it replaced for three separate
//! reasons:
//!
//! - **Accidents stop.** A plain drag can no longer move anything, so a stroke that happens to
//!   start on a header or near a seam does nothing at all. That was the recurring complaint, twice.
//! - **Targets can be as big as they need to be.** This is the part worth dwelling on. A grab
//!   surface only matters *after* the hold, so making it generous costs nothing — and the whole
//!   class of conflicts between a divider's grab width and its neighbour's tab simply evaporates,
//!   because the two are never live at the same moment.
//! - **Touch and pen behave identically.** The interaction is "touch, then the UI appears", never
//!   "hover, then the UI appears". A finger has no hover; the hold works the same for both.
//!
//! Holding still is the price, and it is the right one: rearranging a workspace is rare and
//! deliberate, while drawing on it is constant.
//!
//! **Straying cancels.** Wander past [`SLOP`] before the hold completes and the gesture is over —
//! the pointer has to *stay* there, which is what makes this the same idiom as a press-and-hold
//! anywhere else, and what stops a slow stroke from arming halfway along.
//!
//! A tap on a tab still switches to it, and must leave no trace — in particular it must not push
//! anything onto the layout's undo stack, or Ctrl+Z would start walking back through tab
//! selections.
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

/// How far the pointer may wander during the hold before the gesture is abandoned.
///
/// Generous, because a pen resting on glass drifts and a hand shakes — but not so generous that a
/// deliberate stroke across a header reads as someone holding still.
pub const SLOP: f32 = 9.0;

/// How long the pointer must be held still before anything becomes editable, in milliseconds.
///
/// The press-and-hold convention, and now the *only* way to start any layout gesture. Long enough
/// that no ordinary tap or stroke reaches it, short enough that rearranging does not feel like
/// waiting for permission.
pub const HOLD_MS: f64 = 320.0;

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

/// What the pointer is held on, so the caller can show the wait on the right thing.
#[derive(Clone, Debug, PartialEq)]
pub enum Held {
    Panel { path: Path },
    Divider { path: Path, index: usize },
}

/// What a gesture in progress wants drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    /// Held on something, waiting out the hold. Nothing has changed yet.
    ///
    /// Carried so the caller can show the wait: a hold with no feedback is indistinguishable from
    /// a dead control, which is exactly how the first version felt to use.
    Waiting {
        /// Zero at the press, one when it arms.
        progress: f32,
        on: Held,
    },
    /// A panel is in the air. `over` is the drop it would land in, or `None` when the pointer is
    /// outside the workspace and letting go would float it.
    Carrying {
        panel: PanelId,
        over: Option<Landing>,
    },
    /// A divider is armed, and the layout is following the pointer.
    Resizing { path: Path, index: usize },
    /// A panel was held still long enough to be asked what it offers.
    ///
    /// Reported rather than performed: what a panel offers is the panel's business and the
    /// workspace's to place, and this module knows about neither.
    Asking(PanelId),
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

/// What a grab is for.
#[derive(Clone, Debug)]
enum Kind {
    Tab {
        panel: PanelId,
        tab: usize,
        path: Path,
    },
    Splitter {
        path: Path,
        index: usize,
        axis: Axis,
    },
}

/// One press, and everything that has happened to it since.
///
/// A single shape for both kinds rather than one per kind, because the arming is now *identical*
/// for both and only what happens afterwards differs. Two copies of the hold logic would be two
/// places for it to drift, which is the same hazard as everything else in §11a.8.
#[derive(Clone, Debug)]
struct Grab {
    kind: Kind,
    press: (f32, f32),
    press_ms: f64,
    /// Set once the pointer has moved far enough to count as a drag rather than a press.
    ///
    /// **A tab and a divider are grabbed the instant they are touched**, so this is not a gate on
    /// whether anything may move -- only on whether anything *has*. Holding first was tried and
    /// was wrong: an artist who has already put a finger on a divider is not asking permission to
    /// resize it, and the third of a second before it answered read as a dead control.
    ///
    /// The reason a hold was ever wanted was that pressing on chrome must not draw on the canvas.
    /// It does not: a press on a tab or a divider never reaches the canvas at all, so the hold was
    /// solving a problem that only existed for presses on the canvas itself.
    moved: bool,
    /// Set once the hold has run.
    ///
    /// Not a guard on asking twice -- a tab's grab is dropped the moment it asks, and a divider
    /// has nothing to ask -- which a sabotage proved by changing nothing when it was used as one.
    /// What it is for is the *repaint*: painting is demand-driven, so a waiting hold keeps asking
    /// for frames, and a finger resting on a divider would keep the application redrawing for as
    /// long as it stayed there.
    asked: bool,
    /// Where the pointer was last, for the incremental divider drag.
    last: (f32, f32),
    /// The arrangement as it stood when the gesture began.
    ///
    /// **Captured at the press, not at the release, because a divider moves live.** A move is
    /// applied all at once when you let go, so it could clone the layout on the way out; a resize
    /// has already happened by then, and cloning at that point would record the change as though
    /// it were the state before it. That is why a divider drag was never undoable, and why Escape
    /// during one left the divider where it had got to despite this module promising otherwise.
    before: Layout,
}

/// The panel-dragging state machine.
#[derive(Debug, Default)]
pub struct PanelDrag {
    grab: Option<Grab>,
}

impl PanelDrag {
    /// Whether a gesture is in progress, armed or still waiting.
    #[must_use]
    pub fn active(&self) -> bool {
        self.grab.is_some()
    }

    /// Whether something is actually moving, so the caller knows the pointer is spoken for.
    ///
    /// Distinct from [`PanelDrag::active`] on purpose: a press that is merely *waiting* has taken
    /// nothing, and treating it as busy would make the first fifth of a second of every press feel
    /// dead.
    #[must_use]
    pub fn armed(&self) -> bool {
        self.grab.is_some()
    }

    /// How long until the hold completes, if one is waiting.
    ///
    /// **Painting is demand-driven**, so a hold that nobody asks to draw simply never fires: with
    /// the pointer still, no events arrive, no frame is drawn, and the timer is never read. The
    /// caller uses this to keep frames coming until it arms.
    #[must_use]
    pub fn waiting_ms(&self, now_ms: f64) -> Option<f64> {
        self.grab.as_ref().and_then(|g| {
            (!g.moved && !g.asked).then(|| (HOLD_MS - (now_ms - g.press_ms)).max(0.0))
        })
    }

    /// Begin a gesture. Returns whether this module took the pointer.
    ///
    /// A press on anything but a tab or a divider is declined, so the panel's own content sees it.
    /// Taking the pointer here does **not** mean anything will happen to the layout — that needs
    /// the hold.
    pub fn press(&mut self, layout: &Layout, target: &Target, x: f32, y: f32, now_ms: f64) -> bool {
        // A target naming a tab or a split that is not there is declined rather than trusted: the
        // caller hit-tested against a layout, and nothing guarantees it was this one.
        let kind = match target {
            Target::Tab { path, tab } => layout.tab_at(path, *tab).map(|panel| Kind::Tab {
                panel,
                tab: *tab,
                path: path.clone(),
            }),
            Target::Splitter { path, index } => {
                layout.split_axis(path).map(|axis| Kind::Splitter {
                    path: path.clone(),
                    index: *index,
                    axis,
                })
            }
            Target::Elsewhere => None,
        };
        self.grab = kind.map(|kind| Grab {
            kind,
            press: (x, y),
            press_ms: now_ms,
            moved: false,
            asked: false,
            last: (x, y),
            before: layout.clone(),
        });
        self.grab.is_some()
    }

    /// Continue a gesture.
    ///
    /// Nothing moves until the hold completes. A divider then resizes live, because the artist is
    /// watching the thing they are sizing; a panel only previews, because one that rearranged the
    /// workspace on the way past every drop zone would be unusable.
    pub fn drag(
        &mut self,
        layout: &mut Layout,
        area: Rect,
        x: f32,
        y: f32,
        now_ms: f64,
    ) -> Option<Preview> {
        let grab = self.grab.as_mut()?;
        // **Moving at all commits this to being a drag**, and puts the hold out of reach for good.
        // Latched, because a panel carried out and brought back is still a drag: a hand that has
        // been across the window and returned is not asking a question.
        grab.moved = grab.moved || (x - grab.press.0).hypot(y - grab.press.1) >= SLOP;

        if !grab.moved {
            // Still where it started. The hold is running, and when it completes it asks the thing
            // being held what it offers -- *now*, not on release, because a menu that appears when
            // you let go cannot be dismissed by letting go.
            if now_ms - grab.press_ms < HOLD_MS {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a fraction of a fixed millisecond window"
                )]
                let progress = ((now_ms - grab.press_ms) / HOLD_MS).clamp(0.0, 1.0) as f32;
                return Some(Preview::Waiting {
                    progress,
                    on: match &grab.kind {
                        Kind::Tab { path, .. } => Held::Panel { path: path.clone() },
                        Kind::Splitter { path, index, .. } => Held::Divider {
                            path: path.clone(),
                            index: *index,
                        },
                    },
                });
            }
            grab.asked = true;
            // The gesture is over: whatever the pointer does next belongs to the popup, and the
            // arrangement must not keep following a finger that is now reading a menu.
            let asking = match &grab.kind {
                Kind::Tab { panel, .. } => Some(*panel),
                // A divider belongs to no one panel, so there is nothing to ask. It keeps waiting,
                // which costs nothing: the drag is still live and still resizes on the next move.
                Kind::Splitter { .. } => None,
            };
            if let Some(panel) = asking {
                self.grab = None;
                return Some(Preview::Asking(panel));
            }
            return None;
        }

        match &grab.kind {
            Kind::Splitter { path, index, axis } => {
                let (dx, dy) = (x - grab.last.0, y - grab.last.1);
                grab.last = (x, y);
                let extent = match axis {
                    Axis::Horizontal => area.w,
                    Axis::Vertical => area.h,
                };
                if extent > 0.0 {
                    // In units, with the split's extent alongside. A divider next to a strip moves
                    // the strip's *minimum*, which is measured in units and has no fraction to be
                    // expressed as; one between two weighted panels moves weights, which do.
                    let delta = match axis {
                        Axis::Horizontal => dx,
                        Axis::Vertical => dy,
                    };
                    layout.drag_splitter(path, *index, delta, extent);
                }
                Some(Preview::Resizing {
                    path: path.clone(),
                    index: *index,
                })
            }
            Kind::Tab { panel, .. } => {
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
        if !grab.moved {
            // Pressed and let go without moving. On a tab that is a tap, and it switches to it;
            // on a divider it asked for nothing.
            return match grab.kind {
                Kind::Tab { tab, path, .. } => {
                    // Deliberately not recorded: Ctrl+Z must walk back through arrangement, not
                    // through which tab you were looking at.
                    layout.set_active(&path, tab);
                    Outcome::Switched
                }
                Kind::Splitter { .. } => Outcome::Nothing,
            };
        }

        match grab.kind {
            Kind::Splitter { .. } => {
                // The divider has already moved; what is recorded is where it started. A drag that
                // ended where it began is not a change and does not deserve an undo step.
                if *layout == grab.before {
                    return Outcome::Nothing;
                }
                history.record(grab.before);
                Outcome::Resized
            }
            Kind::Tab { panel, .. } => {
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
                // Finding no anchor is also the answer to the last no-op case: if the only panel
                // in the target leaf is the one being dragged, splitting that leaf would put the
                // panel beside itself.
                let Some(anchor) = placed.tabs.iter().copied().find(|p| *p != panel) else {
                    return Outcome::Nothing;
                };

                let before = grab.before;
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
    pub fn cancel(&mut self, layout: &mut Layout) {
        // Put the arrangement back where it was when the gesture began. A move has changed nothing
        // yet so this is a no-op for one; a divider has been moving live, and without this Escape
        // left it wherever the pointer had dragged it to -- which is not "nothing has happened
        // until you let go", however firmly the comment above says so.
        if let Some(grab) = self.grab.take() {
            if grab.moved {
                *layout = grab.before;
            }
        }
    }
}

/// What one frame's pointer state means for a gesture in flight.
///
/// **This exists because the two answers used to be written in two places and disagreed.** A
/// guard cancelled any gesture whose button was not down, and a branch below it completed the
/// gesture when a release was reported. But egui reports the release and the button-already-up in
/// the *same* frame, so the guard fired first and every drop landed on a gesture that had just
/// been thrown away. Resizing still looked fine, because a divider is dragged live and only its
/// saving happened on release; moving a panel did nothing at all, which is exactly what was
/// reported. One list cannot disagree with itself (recurring hazard 11a.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pulse {
    /// The button went down: a gesture may begin.
    Press,
    /// The button came up: whatever is in flight must be completed now.
    Release,
    /// The button is still down: keep following.
    Track,
    /// Not down, and no release was reported. The pointer left the window, the app lost focus, a
    /// synthesised event went missing. Without noticing this the grab stays live and every later
    /// pointer move keeps rearranging the workspace.
    Lost,
}

/// Read a frame's pointer state. Order matters and is the whole content of this function.
#[must_use]
pub fn pulse(pressed: bool, released: bool, down: bool) -> Pulse {
    if pressed {
        Pulse::Press
    } else if released {
        // Before `down`, which is already false by the time a release is reported.
        Pulse::Release
    } else if down {
        Pulse::Track
    } else {
        Pulse::Lost
    }
}

/// Whether a drop would change nothing: back onto the leaf it came from, in the centre.
///
/// Worth its own answer rather than letting the move happen and produce an identical tree, because
/// an undo entry that restores what is already there reads as a broken Ctrl+Z.
fn is_noop(placed: &Placed, panel: PanelId, zone: Zone) -> bool {
    placed.tabs.contains(&panel) && zone == Zone::Center
}

#[cfg(test)]
mod tests {

    /// **A release is a release even though the button is already up.**
    ///
    /// egui reports both in the same frame: `primary_released()` is true and `primary_down()` is
    /// already false. Reading "not down" as a lost pointer therefore threw away every gesture on
    /// the exact frame it should have been completed.
    #[test]
    fn a_release_is_not_mistaken_for_a_lost_pointer() {
        assert_eq!(pulse(false, true, false), Pulse::Release);
        // And with the button somehow still reported down, it is still a release.
        assert_eq!(pulse(false, true, true), Pulse::Release);
    }

    /// The other three readings, so the order above is pinned from both sides.
    #[test]
    fn a_frame_is_read_as_exactly_one_thing() {
        assert_eq!(pulse(true, false, true), Pulse::Press);
        assert_eq!(pulse(false, false, true), Pulse::Track);
        assert_eq!(pulse(false, false, false), Pulse::Lost);
        // A press and a release coalesced into one frame is a press; the release arrives next
        // frame as `Lost` at worst, which cancels rather than acting on a half-known gesture.
        assert_eq!(pulse(true, true, false), Pulse::Press);
    }
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

    /// Hold still until it arms, which every gesture now has to do.
    /// Carry a gesture to a point, which is what the app always does: a release is preceded by
    /// the frames that got the pointer there.
    ///
    /// There is no arming step to imitate. A press is live the moment it lands, and moving is what
    /// turns it from a tap into a drag -- so a test that pressed and released with no frames in
    /// between was testing a tap while claiming to test a drop.
    fn carry(d: &mut PanelDrag, l: &mut Layout, to: (f32, f32)) {
        d.drag(l, area(), to.0, to.1, 1.0);
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
        d.drag(&mut l, area(), 702.0, 11.0, 40.0);
        assert_eq!(
            d.release(&mut l, &mut h, area(), 702.0, 11.0),
            Outcome::Switched
        );

        assert_eq!(l.resolve(area())[1].active, 0, "Layers is showing now");
        assert_eq!(h.depth(), (0, 0), "a tap is not an undoable edit");
    }

    /// **Moving lifts the panel at once**, which is the rule this module is now built on.
    ///
    /// It used to be the opposite: movement alone did nothing until a hold had armed the gesture.
    /// That protected the canvas from a stroke starting on a header, except that a press on a
    /// header never reaches the canvas -- so it was a cost with no benefit.
    #[test]
    fn moving_lifts_the_panel_at_once() {
        let mut l = workspace();
        let mut d = PanelDrag::default();
        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);

        assert!(
            matches!(
                d.drag(&mut l, area(), 700.0, 300.0, 1.0),
                Some(Preview::Carrying { .. })
            ),
            "the panel should be in the air on the first move"
        );
    }

    /// **A divider resizes on the first move, with no wait at all.**
    ///
    /// Holding first was tried and was wrong: an artist with a finger already on a divider is not
    /// asking permission to resize it, and the third of a second before it answered read as a dead
    /// control. The canvas the hold was protecting was never at risk -- a press on a divider does
    /// not reach it.
    #[test]
    fn a_divider_resizes_at_once() {
        let mut l = workspace();
        let before = l.resolve(area())[0].rect.w;
        let mut d = PanelDrag::default();
        let sp = l.splitters(area(), 26.0)[0].clone();

        d.press(
            &l,
            &Target::Splitter {
                path: sp.path.clone(),
                index: sp.index,
            },
            sp.rect.x,
            400.0,
            0.0,
        );
        // One move, one millisecond later.
        d.drag(&mut l, area(), sp.rect.x + 120.0, 400.0, 1.0);
        assert!(
            (l.resolve(area())[0].rect.w - before).abs() > 1.0,
            "a divider should follow the pointer immediately"
        );
    }

    /// Once the hold completes, a divider resizes live and by the distance dragged.
    #[test]
    fn an_armed_divider_follows_the_pointer() {
        let mut l = workspace();
        let mut h = LayoutHistory::default();
        let before = l.resolve(area())[0].rect.w;
        let mut d = PanelDrag::default();
        let s = l.splitters(area(), 26.0)[0].clone();

        d.press(
            &l,
            &Target::Splitter {
                path: s.path.clone(),
                index: s.index,
            },
            s.rect.x,
            400.0,
            0.0,
        );
        d.drag(&mut l, area(), s.rect.x, 400.0, 1.0);
        for step in 1..=3_u8 {
            d.drag(
                &mut l,
                area(),
                40.0_f32.mul_add(f32::from(step), s.rect.x),
                400.0,
                20.0_f64.mul_add(f64::from(step), HOLD_MS),
            );
        }
        let after = l.resolve(area())[0].rect.w;
        assert!(
            (after - (before + 120.0)).abs() < 2.0,
            "the panel should have followed the divider 120 units, went {before} to {after}"
        );
        assert_eq!(
            d.release(&mut l, &mut h, area(), s.rect.x + 120.0, 400.0),
            Outcome::Resized
        );
    }

    /// Straying makes it a drag rather than abandoning it, and the hold never comes back.
    ///
    /// "Hold still to ask, move to rearrange" has to mean *still*, or a slow drag that paused
    /// would sprout a menu in the middle of itself.
    #[test]
    fn straying_turns_the_hold_into_a_drag_for_good() {
        let mut l = workspace();
        let mut d = PanelDrag::default();

        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);
        d.drag(&mut l, area(), 700.0 + SLOP + 1.0, 10.0, 20.0);
        assert_eq!(
            d.waiting_ms(20.0),
            None,
            "it is no longer waiting to be asked"
        );
        // Held still for ages: it must not come back to life as a question.
        assert!(!matches!(
            d.drag(&mut l, area(), 760.0, 10.0, 5000.0),
            Some(Preview::Asking(_))
        ));
    }

    /// The wait is reported so the caller can show it. A hold with no feedback is
    /// indistinguishable from a dead control, which is how the first version felt.
    #[test]
    fn the_wait_is_visible_before_anything_moves() {
        let mut l = workspace();
        let mut d = PanelDrag::default();
        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);

        let Some(Preview::Waiting { progress, on }) = d.drag(&mut l, area(), 700.0, 10.0, 80.0)
        else {
            panic!("a press should report that it is waiting");
        };
        assert!(
            (0.2..0.3).contains(&progress),
            "80 of 320 ms is about a quarter, got {progress}"
        );
        assert_eq!(on, Held::Panel { path: vec![1] });
    }

    /// A still press asks for the frames that let its hold fire, and stops once it has fired.
    ///
    /// **Painting is demand-driven**, so a hold with the pointer perfectly still would otherwise
    /// never complete: no events, no frames, and the timer never read.
    #[test]
    fn a_still_press_asks_for_frames() {
        let l = workspace();
        let mut d = PanelDrag::default();
        assert_eq!(d.waiting_ms(0.0), None, "nothing is waiting yet");

        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);
        let left = d.waiting_ms(50.0).expect("a press is waiting");
        assert!((left - (HOLD_MS - 50.0)).abs() < 0.001, "got {left}");

        // Once it has asked, there is nothing left to wait for.
        let mut l2 = workspace();
        d.drag(&mut l2, area(), 700.0, 10.0, HOLD_MS + 1.0);
        assert_eq!(d.waiting_ms(HOLD_MS + 2.0), None);
    }

    /// **The ordering hazard.** Applying a move removes the panel first, and removing can collapse
    /// a leaf and shift every path after it — so a path captured before the removal can name a
    /// different node, or nothing. The target is anchored to a panel id instead.
    #[test]
    fn a_move_survives_the_tree_collapsing_under_it() {
        let mut l = Layout::single(CANVAS);
        l.insert(&[], Zone::Right, LAYERS);
        l.insert(&[1], Zone::Right, HISTORY);
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        let placed = l.resolve(area());
        assert_eq!(placed.len(), 3);
        let target = placed[2].rect;

        d.press(&l, &tab(&[0], 0), 10.0, 10.0, 0.0);
        d.drag(&mut l, area(), 10.0, 10.0, 1.0);
        let drop = (target.x + target.w / 2.0, target.y + target.h / 2.0);
        d.drag(&mut l, area(), drop.0, drop.1, HOLD_MS + 40.0);
        assert_eq!(
            d.release(&mut l, &mut h, area(), drop.0, drop.1),
            Outcome::Moved
        );

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
        d.drag(&mut l, area(), 700.0, 10.0, 1.0);
        let drop = (
            canvas_rect.x + canvas_rect.w / 2.0,
            canvas_rect.y + canvas_rect.h * 0.95,
        );
        d.drag(&mut l, area(), drop.0, drop.1, HOLD_MS + 40.0);
        assert_eq!(
            d.release(&mut l, &mut h, area(), drop.0, drop.1),
            Outcome::Moved
        );

        let (path, _) = l.find(LAYERS).expect("layers moved, not vanished");
        assert_ne!(path, l.find(CANVAS).expect("canvas is there").0);
        assert_eq!(h.depth().0, 1, "and the move is undoable");
    }

    /// Dropping a panel back where it came from changes nothing and records nothing.
    #[test]
    fn dropping_a_panel_back_home_is_not_an_edit() {
        let mut l = workspace();
        let before = l.clone();
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        let home = l.resolve(area())[1].rect;
        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);
        let drop = (home.x + home.w / 2.0, home.y + home.h / 2.0);
        carry(&mut d, &mut l, drop);
        assert_eq!(
            d.release(&mut l, &mut h, area(), drop.0, drop.1),
            Outcome::Nothing
        );
        assert_eq!(l, before);
        assert_eq!(h.depth(), (0, 0));
    }

    /// A lone panel dropped on its own leaf's edge would be split away from itself.
    #[test]
    fn a_lone_panel_cannot_split_its_own_leaf() {
        let mut l = Layout::single(CANVAS);
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        d.press(&l, &tab(&[], 0), 10.0, 10.0, 0.0);
        carry(&mut d, &mut l, (990.0, 400.0));
        assert_eq!(
            d.release(&mut l, &mut h, area(), 990.0, 400.0),
            Outcome::Nothing
        );
        assert_eq!(l, Layout::single(CANVAS));
    }

    /// Let go outside the workspace and the panel wants its own window.
    #[test]
    fn letting_go_outside_asks_to_float() {
        let mut l = workspace();
        let mut h = LayoutHistory::default();
        let mut d = PanelDrag::default();

        d.press(&l, &tab(&[1], 0), 700.0, 10.0, 0.0);
        carry(&mut d, &mut l, (-50.0, 400.0));
        assert_eq!(
            d.release(&mut l, &mut h, area(), -50.0, 400.0),
            Outcome::Floated(LAYERS)
        );
        assert_eq!(h.depth(), (0, 0), "nothing has happened to the layout yet");
    }

    /// Once a hold has fired, it stops asking for frames.
    ///
    /// **Painting is demand-driven**, so a waiting hold keeps requesting them; a divider's hold
    /// has nothing to ask -- it belongs to no one panel -- and without noticing that it had run,
    /// a finger resting on a divider would keep the application redrawing for as long as it stayed
    /// there.
    #[test]
    fn a_divider_stops_asking_for_frames_once_its_hold_has_run() {
        let mut l = workspace();
        let mut d = PanelDrag::default();
        let sp = l.splitters(area(), 26.0)[0].clone();
        d.press(
            &l,
            &Target::Splitter {
                path: sp.path.clone(),
                index: sp.index,
            },
            sp.rect.x,
            400.0,
            0.0,
        );
        assert!(d.waiting_ms(10.0).is_some(), "it should be waiting");

        d.drag(&mut l, area(), sp.rect.x, 400.0, HOLD_MS + 1.0);
        assert_eq!(
            d.waiting_ms(HOLD_MS + 2.0),
            None,
            "the hold has run; there is nothing left to wait for"
        );
    }

    /// **A hold that drifted a little is still a hold.**
    ///
    /// The pointer never sits perfectly still through a third of a second, especially a pen on
    /// glass. If any movement at all counted, the gesture would work or not depending on how
    /// steady the hand was.
    #[test]
    fn a_hold_that_drifted_a_little_still_asks() {
        let mut l = workspace();
        let mut d = PanelDrag::default();

        let start = (700.0, 10.0);
        d.press(&l, &tab(&[1], 0), start.0, start.1, 0.0);
        // Drifts almost the whole way to the threshold while the hold runs.
        let drifted = (start.0 + SLOP * 0.9, start.1);
        assert!(
            matches!(
                d.drag(&mut l, area(), drifted.0, drifted.1, HOLD_MS / 2.0),
                Some(Preview::Waiting { .. })
            ),
            "a hand that wobbled should still be waiting"
        );
        assert_eq!(
            d.drag(&mut l, area(), drifted.0, drifted.1, HOLD_MS + 1.0),
            Some(Preview::Asking(LAYERS)),
            "and the hold should complete"
        );
    }

    /// **Moving past the threshold makes it a drag, and the hold can never come back.**
    ///
    /// Otherwise a slow drag that paused would sprout a menu in the middle of itself.
    #[test]
    fn moving_puts_the_hold_out_of_reach_for_good() {
        let mut l = workspace();
        let mut d = PanelDrag::default();

        let start = (700.0, 10.0);
        d.press(&l, &tab(&[1], 0), start.0, start.1, 0.0);
        // Well past the threshold: this is a drag now.
        let away = (start.0 + SLOP * 4.0, start.1 + 200.0);
        assert!(matches!(
            d.drag(&mut l, area(), away.0, away.1, 1.0),
            Some(Preview::Carrying { .. })
        ));
        // Held still there, far longer than the hold, and it must not ask.
        for t in [HOLD_MS, HOLD_MS * 4.0] {
            assert!(
                !matches!(
                    d.drag(&mut l, area(), away.0, away.1, t),
                    Some(Preview::Asking(_))
                ),
                "a drag that paused sprouted a menu"
            );
        }
    }

    /// **A tab and a divider move the instant they are touched.**
    ///
    /// Holding first was tried and was wrong: an artist with a finger already on a divider is not
    /// asking permission to resize it, and the third of a second before it answered read as a dead
    /// control. The hold that a press on chrome was protecting the canvas from never existed --- a
    /// press on a tab or a divider does not reach the canvas at all.
    #[test]
    fn a_divider_resizes_on_the_first_move() {
        let mut l = workspace();
        let mut d = PanelDrag::default();
        let before = l.clone();

        d.press(
            &l,
            &Target::Splitter {
                path: vec![],
                index: 0,
            },
            500.0,
            400.0,
            0.0,
        );
        // One move, immediately, with no wait at all.
        d.drag(&mut l, area(), 560.0, 400.0, 1.0);
        assert_ne!(l, before, "the divider should have moved at once");
    }

    /// A press on a panel's content is declined, so the panel itself sees it.
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
        d.drag(&mut l, area(), 700.0, 10.0, 1.0);
        d.cancel(&mut l);

        assert!(!d.active());
        assert_eq!(
            d.release(&mut l, &mut h, area(), 400.0, 400.0),
            Outcome::Nothing
        );
        assert_eq!(l, before);
        assert_eq!(h.depth(), (0, 0));
    }
}
