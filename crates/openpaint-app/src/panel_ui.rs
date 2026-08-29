//! What a panel is made of: a list of controls, described rather than drawn.
//!
//! This is the piece that makes porting the old side panel cheap and makes egui replaceable.
//! A panel does not *draw* a slider; it says "there is a slider here, called Size, currently 14,
//! between 0.5 and 400". Something else turns that into pixels, and something else again turns a
//! press into a new value.
//!
//! The reasoning, which came from the author: porting panels as hand-written UI code would mean
//! writing each one once against egui and again against our own widgets. Porting them as
//! *descriptions* means the renderer is written once — so there is no intermediate version worth
//! building, and egui is never given the job rather than being replaced later.
//!
//! # Values in, changes out
//!
//! A panel builds its list with the *current* values, and gets back a list of what the artist
//! changed. No closures, no callbacks, no borrowing the whole application into a widget.
//!
//! That shape is what keeps this testable: laying controls out and deciding what a press means are
//! both pure functions of a rectangle and a list, so they are checked here without a GPU — the
//! same split that made `crop`, `transform_box` and `chrome` provable.
//!
//! # What is not describable
//!
//! Not everything will be. A colour wheel, a curve editor and a layer list with drag-to-reorder
//! are drawings, not lists of controls, and when the first one arrives it gets a `Custom` variant
//! carrying a height — the shape is obvious and adding it then costs nothing, which is why it is
//! not here now on the strength of a guess.
//!
//! **If half the panels end up custom, this layer has bought nothing.** That is the signal to stop
//! and rethink rather than to add a second escape hatch. Counting the existing UI, roughly nine in
//! ten controls are describable and three things are not.

use crate::layout::Rect;
use crate::theme::Metrics;

/// Which control a change refers to.
///
/// Chosen by the panel that built the list, so it can match on the way back without counting
/// positions — a list that reorders itself would otherwise silently start setting the wrong value.
pub type ControlId = u32;

/// One thing in a panel.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    /// Text. The commonest thing in the whole UI by a distance.
    Label { text: String },
    /// A quiet rule between groups.
    Separator,
    /// Something that happens when pressed.
    Button { id: ControlId, text: String },
    /// A number with a range, dragged.
    Slider {
        id: ControlId,
        text: String,
        value: f32,
        min: f32,
        max: f32,
        /// Shown after the value: "px", "%", "ms".
        unit: &'static str,
        /// Whether the scale is logarithmic, which brush size wants and opacity does not.
        ///
        /// A brush at radius 4 and one at radius 40 should feel the same distance apart per step,
        /// which is what a paint app means by size.
        log: bool,
    },
    /// On or off.
    Toggle {
        id: ControlId,
        text: String,
        on: bool,
    },
    /// A row in a list: layers, presets, pages.
    Row {
        id: ControlId,
        text: String,
        selected: bool,
        /// A colour chip at the start of the row, for a swatch or a layer thumbnail.
        swatch: Option<[u8; 3]>,
    },
}

impl Control {
    /// The id this control answers to, if it has one.
    #[must_use]
    pub fn id(&self) -> Option<ControlId> {
        match self {
            Self::Label { .. } | Self::Separator => None,
            Self::Button { id, .. }
            | Self::Slider { id, .. }
            | Self::Toggle { id, .. }
            | Self::Row { id, .. } => Some(*id),
        }
    }

    /// How tall this control wants to be, in logical units.
    ///
    /// Grab targets get the full row height rather than the height of their ink: a slider's track
    /// is three units tall and its target is the whole row, which is what makes it catchable with
    /// a pen (§1b, and the millimetre check in `theme`).
    #[must_use]
    pub fn height(&self, metrics: &Metrics) -> f32 {
        match self {
            Self::Separator => metrics.padding,
            Self::Label { .. } => metrics.body + metrics.padding * 0.5,
            // Everything interactive is one row tall, so a column of them is a predictable rhythm
            // and every target is the same size.
            _ => metrics.row,
        }
    }
}

/// A control and where it sits.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed<'a> {
    pub control: &'a Control,
    pub rect: Rect,
}

/// Stack the controls down the panel's content rectangle.
///
/// Vertical and in order, which is what every panel in the app already is. Anything wanting a grid
/// can have one when something needs it; inventing one now would be a shape with no user.
///
/// Controls that run past the bottom are still placed, with rectangles outside the panel. The
/// caller clips, and a scroll offset is a subtraction from `content.y` — so scrolling needs
/// nothing here.
#[must_use]
pub fn place<'a>(controls: &'a [Control], content: Rect, metrics: &Metrics) -> Vec<Placed<'a>> {
    let mut out = Vec::with_capacity(controls.len());
    let mut y = content.y;
    for control in controls {
        let h = control.height(metrics);
        out.push(Placed {
            control,
            rect: Rect::new(content.x, y, content.w, h),
        });
        y += h + metrics.gap;
    }
    out
}

/// How tall the whole list is, for deciding whether it needs to scroll.
#[must_use]
pub fn total_height(controls: &[Control], metrics: &Metrics) -> f32 {
    let mut h = 0.0;
    for (i, control) in controls.iter().enumerate() {
        h += control.height(metrics);
        if i + 1 < controls.len() {
            h += metrics.gap;
        }
    }
    h
}

/// Where a list may be scrolled to, given how tall it is and how much of it shows.
///
/// **A list can never be scrolled past its own end, and one that fits does not scroll at all.**
/// Both halves matter: a panel that can be flicked into empty space looks broken, and a short list
/// that drifts away from its own heading looks worse.
///
/// Scrolling is a subtraction from where the controls start, so nothing in `place` knows about it.
#[must_use]
pub fn clamp_scroll(offset: f32, total: f32, visible: f32) -> f32 {
    offset.clamp(0.0, (total - visible).max(0.0))
}

/// What the artist did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    Pressed(ControlId),
    /// A slider moved. Already clamped to its range.
    Set(ControlId, f32),
    Toggled(ControlId, bool),
    /// A row was chosen.
    Chose(ControlId),
}

/// What a press at a point would do, without doing it.
///
/// Separate from applying it so the caller can show a pressed state on the way down and only
/// commit on the way up — and so this is testable, which applying against live state would not be.
#[must_use]
pub fn hit<'a>(placed: &[Placed<'a>], x: f32, y: f32) -> Option<(&'a Control, Rect)> {
    placed
        .iter()
        .find(|p| p.rect.contains(x, y) && p.control.id().is_some())
        .map(|p| (p.control, p.rect))
}

/// The change a press or drag at `x` produces on `control`.
///
/// A slider maps the whole row's width, so its value follows the pointer wherever in the row it
/// is — dragging from anywhere on the row works, which matters when the track itself is three
/// units tall.
#[must_use]
pub fn change_at(control: &Control, rect: Rect, x: f32) -> Option<Change> {
    match control {
        Control::Button { id, .. } => Some(Change::Pressed(*id)),
        Control::Toggle { id, on, .. } => Some(Change::Toggled(*id, !on)),
        Control::Row { id, .. } => Some(Change::Chose(*id)),
        Control::Slider {
            id, min, max, log, ..
        } => {
            // Not clamped here: `from_fraction` is the one place a value is held inside its
            // range. Clamping in both would mean two guards over one rule, and a sabotage that
            // removed either would leave the other quietly covering for it — which is how a
            // guard stops being tested without anybody noticing.
            let t = if rect.w > 0.0 {
                (x - rect.x) / rect.w
            } else {
                0.0
            };
            Some(Change::Set(*id, from_fraction(t, *min, *max, *log)))
        }
        Control::Label { .. } | Control::Separator => None,
    }
}

/// Where a slider's knob sits, as a fraction of its track.
///
/// The inverse of [`from_fraction`], and they are written next to each other on purpose: a knob
/// drawn at one position while a press at that position sets a different value is the kind of bug
/// that reads as the slider being "sticky", and it comes from these two drifting apart.
#[must_use]
pub fn to_fraction(value: f32, min: f32, max: f32, log: bool) -> f32 {
    if log && min > 0.0 && max > min {
        let (lo, hi) = (min.ln(), max.ln());
        ((value.max(min).ln() - lo) / (hi - lo)).clamp(0.0, 1.0)
    } else if max > min {
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The value at a fraction of a slider's track.
///
/// **The one place a slider's range is enforced.** A drag that runs off the end of the row, or off
/// the window entirely, arrives here as a fraction outside 0..1 and leaves inside the range.
#[must_use]
pub fn from_fraction(t: f32, min: f32, max: f32, log: bool) -> f32 {
    if log && min > 0.0 && max > min {
        let (lo, hi) = (min.ln(), max.ln());
        (lo + t * (hi - lo)).exp().clamp(min, max)
    } else {
        (min + t * (max - min)).clamp(min, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn metrics() -> Metrics {
        Theme::default().metrics
    }

    fn content() -> Rect {
        Rect::new(10.0, 20.0, 200.0, 400.0)
    }

    fn sliders() -> Vec<Control> {
        vec![
            Control::Label {
                text: "Brush".to_owned(),
            },
            Control::Slider {
                id: 1,
                text: "Size".to_owned(),
                value: 14.0,
                min: 0.5,
                max: 400.0,
                unit: "px",
                log: true,
            },
            Control::Slider {
                id: 2,
                text: "Opacity".to_owned(),
                value: 1.0,
                min: 0.0,
                max: 1.0,
                unit: "",
                log: false,
            },
            Control::Separator,
            Control::Toggle {
                id: 3,
                text: "Lock alpha".to_owned(),
                on: false,
            },
            Control::Button {
                id: 4,
                text: "Reset".to_owned(),
            },
        ]
    }

    /// Controls stack down the panel in order, none overlapping the next.
    #[test]
    fn controls_stack_in_order_without_overlapping() {
        let controls = sliders();
        let placed = place(&controls, content(), &metrics());
        assert_eq!(placed.len(), controls.len());

        for pair in placed.windows(2) {
            assert!(
                pair[1].rect.y >= pair[0].rect.y + pair[0].rect.h,
                "control {:?} overlaps the one before it",
                pair[1].control
            );
        }
        assert!((placed[0].rect.x - content().x).abs() < 0.001);
        assert!((placed[0].rect.w - content().w).abs() < 0.001);
    }

    /// Every interactive control is the same height, so a column of them has one rhythm and one
    /// target size. That is what makes a panel predictable to aim at.
    #[test]
    fn every_interactive_control_is_one_row_tall() {
        let m = metrics();
        for control in sliders() {
            if control.id().is_none() {
                continue;
            }
            assert!(
                (control.height(&m) - m.row).abs() < 0.001,
                "{control:?} is {} tall, not one row",
                control.height(&m)
            );
        }
    }

    /// A row's whole height is the target, not the ink inside it — a three-unit slider track is
    /// not something anyone can hit with a pen.
    #[test]
    fn a_slider_is_grabbable_anywhere_on_its_row() {
        let controls = sliders();
        let placed = place(&controls, content(), &metrics());
        let row = placed[1].rect;

        for dy in [1.0, row.h / 2.0, row.h - 1.0] {
            let found = hit(&placed, row.x + 50.0, row.y + dy);
            assert!(
                matches!(found, Some((Control::Slider { id: 1, .. }, _))),
                "the slider should be catchable {dy} into its row"
            );
        }
    }

    /// Labels and separators are not targets, so a press on one falls through to whatever is
    /// beneath — which for a panel is the panel itself.
    #[test]
    fn decoration_is_not_a_target() {
        let controls = sliders();
        let placed = place(&controls, content(), &metrics());
        for i in [0, 3] {
            let r = placed[i].rect;
            assert!(
                hit(&placed, r.x + 5.0, r.y + r.h / 2.0).is_none(),
                "{:?} should not answer a press",
                placed[i].control
            );
        }
    }

    /// A press produces the change it looks like it should.
    #[test]
    fn a_press_produces_the_obvious_change() {
        let controls = sliders();
        let placed = place(&controls, content(), &metrics());
        let at = |i: usize, x: f32| change_at(placed[i].control, placed[i].rect, x);

        assert_eq!(at(4, 20.0), Some(Change::Toggled(3, true)));
        assert_eq!(at(5, 20.0), Some(Change::Pressed(4)));
        assert_eq!(at(0, 20.0), None, "a label does nothing");

        // A linear slider at the far right is its maximum.
        let r = placed[2].rect;
        assert_eq!(at(2, r.x + r.w), Some(Change::Set(2, 1.0)));
        assert_eq!(at(2, r.x - 50.0), Some(Change::Set(2, 0.0)), "clamped");
    }

    /// **The knob and the press agree.** A knob drawn at one position while a press there sets a
    /// different value reads as the slider being sticky, and it comes from these two drifting
    /// apart — so they are checked against each other rather than against numbers.
    #[test]
    fn where_the_knob_is_drawn_is_where_pressing_sets_it() {
        for (min, max, log) in [
            (0.0, 1.0, false),
            (0.5, 400.0, true),
            (-180.0, 180.0, false),
        ] {
            for step in 0..=10_u8 {
                let t = f32::from(step) / 10.0;
                let value = from_fraction(t, min, max, log);
                let back = to_fraction(value, min, max, log);
                assert!(
                    (back - t).abs() < 0.001,
                    "at {t} the value {value} draws at {back} on ({min}, {max}, log={log})"
                );
            }
        }
    }

    /// A logarithmic slider gives the small end room, which is what a brush size wants: one step
    /// at radius 4 should feel like one step at radius 40.
    #[test]
    fn a_logarithmic_slider_spreads_the_small_end() {
        let (min, max) = (0.5_f32, 400.0);
        let mid_log = from_fraction(0.5, min, max, true);
        let mid_linear = from_fraction(0.5, min, max, false);
        assert!(
            mid_log < mid_linear / 10.0,
            "halfway should be near 14, not near 200: log {mid_log}, linear {mid_linear}"
        );
        // And the two ends still land exactly.
        assert!((from_fraction(0.0, min, max, true) - min).abs() < 0.001);
        assert!((from_fraction(1.0, min, max, true) - max).abs() < 0.001);
    }

    /// **A drag that runs off the end of the row stays inside the range.** Both scales: a
    /// logarithmic slider clamps in a different branch, and testing only the linear one left that
    /// branch's guard unexercised — a sabotage removing it passed everything.
    ///
    /// This is the ordinary case, not an edge one. A slider is a whole row wide, and a hand
    /// dragging along it does not stop politely at the last pixel.
    #[test]
    fn a_drag_past_either_end_stays_inside_the_range() {
        for (min, max, log) in [
            (0.0, 1.0, false),
            (0.5, 400.0, true),
            (-180.0, 180.0, false),
        ] {
            for t in [-4.0, -0.001, 1.001, 9.0, f32::INFINITY, f32::NEG_INFINITY] {
                let v = from_fraction(t, min, max, log);
                assert!(
                    v >= min && v <= max,
                    "at {t} on ({min}, {max}, log={log}) the value escaped to {v}"
                );
            }
        }
    }

    /// The total height says whether a panel needs to scroll, and agrees with where the last
    /// control actually ends.
    #[test]
    fn the_total_height_matches_where_the_last_control_ends() {
        let controls = sliders();
        let m = metrics();
        let placed = place(&controls, content(), &m);
        let last = placed.last().expect("controls");
        let measured = (last.rect.y + last.rect.h) - content().y;
        assert!(
            (measured - total_height(&controls, &m)).abs() < 0.001,
            "measured {measured}, reported {}",
            total_height(&controls, &m)
        );
    }

    /// A list cannot be scrolled past its own end, and one that fits does not scroll at all.
    #[test]
    fn a_list_cannot_be_scrolled_into_empty_space() {
        // Taller than the panel: the last control ends flush with the bottom, never above it.
        assert_eq!(clamp_scroll(1000.0, 400.0, 120.0), 280.0);
        assert_eq!(clamp_scroll(-50.0, 400.0, 120.0), 0.0);
        // Shorter than the panel: nowhere to go.
        assert_eq!(clamp_scroll(90.0, 60.0, 120.0), 0.0);
        assert_eq!(clamp_scroll(0.0, 60.0, 120.0), 0.0);
    }

    /// An empty panel is a valid panel: no controls, no height, nothing to hit.
    #[test]
    fn an_empty_panel_is_fine() {
        let placed = place(&[], content(), &metrics());
        assert!(placed.is_empty());
        assert!(hit(&placed, 50.0, 50.0).is_none());
        assert_eq!(total_height(&[], &metrics()), 0.0);
    }
}
