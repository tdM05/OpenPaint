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
use serde::{Deserialize, Serialize};

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
    /// One of a set, shown as a button that says whether it is the current one.
    ///
    /// A tool in the rail, a blend mode, the shape of a colour wheel. Distinct from [`Row`] in
    /// presentation rather than meaning: a row is an item in a list you read down, a choice is one
    /// of a handful you pick between, and drawing them the same way would make a tool rail look
    /// like a list of files.
    ///
    /// [`Row`]: Control::Row
    Choice {
        id: ControlId,
        text: String,
        selected: bool,
        /// What this choice *is*, so an icon set can draw it.
        ///
        /// A role, not a picture: the set decides what a brush looks like. `None` means there is
        /// no icon for this and the label stands on its own, which is what a choice with no
        /// established symbol -- a direction, a blend mode -- should do.
        icon: Option<crate::icons::Symbol>,
    },
    /// One of a longer list, shown as what is chosen now and opened to change it.
    ///
    /// **A [`Choice`] per option stops working past about five.** Blend modes are two dozen; a
    /// resampling kernel is half that. Laid out as choices they fill a panel with things nobody is
    /// looking at, and the one that matters -- what is set *now* -- has to be found among them.
    ///
    /// The options are not here. This is what the panel shows when nothing is open, and the list
    /// belongs to whoever knows what the options are; opening it is an answer the panel gives, the
    /// same way the menu opens its own items.
    ///
    /// [`Choice`]: Control::Choice
    Pick {
        id: ControlId,
        text: String,
        /// What is chosen now, in words: "Multiply", "Lanczos".
        value: String,
    },
    /// Words the artist types.
    ///
    /// A preset's name, a layer's name, a page size, a transform's exact numbers. Editing lives in
    /// [`crate::text_field`], which is where the caret, the selection, the word motion and the
    /// UTF-8 arithmetic already are and are already fuzzed -- this is only the part that says
    /// there is a field here and what is in it.
    Text {
        id: ControlId,
        text: String,
        /// What the field holds. What the artist has typed *while editing* lives with the pointer
        /// state, not here: a control is a description of the panel, and a description that
        /// changed under every keystroke would be rebuilt on every keystroke.
        value: String,
    },
    /// Drawn by code, because it cannot be described.
    ///
    /// A colour wheel, a curve editor, a graph. The engine still owns *where* it goes and how tall
    /// it is, so it stacks and scrolls with everything else and the panel is handed a rectangle
    /// rather than left to work one out.
    ///
    /// **If half a panel ends up custom, this layer has bought nothing** -- that is the signal to
    /// stop and rethink, not to reach for a second escape hatch.
    Custom { id: ControlId, height: f32 },
    /// A row in a list: layers, presets, pages.
    Row {
        id: ControlId,
        text: String,
        selected: bool,
        /// A colour chip at the start of the row, for a swatch or a layer thumbnail.
        swatch: Option<[u8; 3]>,
        /// A switch at the end of the row, with its own id and its own answer.
        ///
        /// **A row can be worth more than one thing.** A layer wants to be chosen *and* shown or
        /// hidden, and every paint application puts the eye on the row itself; a switch below the
        /// list acting on "the current layer" makes hiding one a two-step job. Its own id, so a
        /// press on the switch is a different answer from a press on the row -- which is the
        /// whole point.
        mark: Option<RowMark>,
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
            | Self::Choice { id, .. }
            | Self::Pick { id, .. }
            | Self::Text { id, .. }
            | Self::Custom { id, .. }
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
            Self::Custom { height, .. } => *height,
            // Everything interactive is one row tall, so a column of them is a predictable rhythm
            // and every target is the same size.
            _ => metrics.row,
        }
    }

    /// How wide this control wants to be when the list runs across rather than down.
    ///
    /// `text` is the measured width of its label, which only something with a font can know, so it
    /// comes in from the caller the same way tab widths do.
    #[must_use]
    pub fn width(&self, metrics: &Metrics, text: f32) -> f32 {
        match self {
            // A rule between groups, turned on its side.
            Self::Separator => metrics.padding,
            Self::Label { .. } => text + metrics.padding,
            // A slider needs somewhere to slide. Below about six rows it is a button that lies.
            Self::Slider { .. } => (text + metrics.row * 4.0).max(metrics.row * 6.0),
            Self::Toggle { .. } => text + metrics.row * 1.6 + metrics.padding * 2.0,
            // Square at least, so a single glyph is still a target rather than a sliver.
            Self::Choice { .. } => (text + metrics.padding * 2.0).max(metrics.row),
            // Both hold words that change, so both need room for words longer than the ones there
            // now -- a field that resizes as you type is a field that moves out from under the
            // caret. The same floor a slider gets, for the same reason: below it they are buttons
            // that lie about what they do.
            Self::Pick { .. } | Self::Text { .. } => {
                (text + metrics.row * 4.0).max(metrics.row * 6.0)
            }
            // Square, because a custom drawing has no label to be measured and nothing to say
            // about how wide it wants to be along a row.
            Self::Custom { height, .. } => *height,
            Self::Button { .. } | Self::Row { .. } => text + metrics.padding * 2.0,
        }
    }
}

/// A switch that lives at the end of a list row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowMark {
    pub id: ControlId,
    pub on: bool,
}

/// Which way a panel's controls run.
///
/// **Not `Flow`**, which this was called first and should never have been: in a paint application
/// flow is how much paint a brush lays down, and `Brush` already had a field by that name. The
/// compiler found the collision the moment the rename went through, which is later than a reader
/// would have.
///
/// **A strip is a row or a column and never a half-broken row.** Wrapping a row of buttons onto a
/// second line leaves a ragged edge and a last line with one item on it, and no arrangement of a
/// panel makes that look deliberate. Turning the whole thing on its side does.
///
/// This is a *setting*, not a rule about the menu: it belongs to any list of controls, so every
/// panel gets it without anything being written twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Down the panel. What a properties panel wants.
    Column,
    /// Across it. What a menu bar or a tool strip wants.
    Row,
    /// Across if it fits, down if it does not.
    #[default]
    Auto,
    /// Across, onto more lines as needed.
    ///
    /// **Only honest when the items are the same width.** A wrapped row of words leaves a ragged
    /// edge and a last line with one item on it; a wrapped grid of equal square buttons is what
    /// every tool rail in every paint application already is, and reads as deliberate because it
    /// is. So this exists, and it is not the default for anything with a label in it.
    Wrap,
}

/// A control and where it sits.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed<'a> {
    pub control: &'a Control,
    pub rect: Rect,
}

/// Lay the controls out in the panel's content rectangle, in order.
///
/// Anything wanting a grid can have one when something needs it; inventing one now would be a
/// shape with no user.
///
/// Controls that run past the edge are still placed, with rectangles outside the panel. The caller
/// clips, and a scroll offset is a subtraction from the origin, so scrolling needs nothing here.
///
/// `text_of` measures a control's label, which only something with a font can answer. A column
/// never asks.
#[must_use]
pub fn place<'a>(
    controls: &'a [Control],
    content: Rect,
    metrics: &Metrics,
    direction: Direction,
    text_of: impl Fn(&Control) -> f32,
) -> Vec<Placed<'a>> {
    let across = match direction {
        Direction::Row | Direction::Wrap => true,
        Direction::Column => false,
        // The whole point of Auto: it decides once, for the whole list. A per-control decision is
        // what produces a wrapped row, which is the thing being avoided.
        Direction::Auto => row_width(controls, metrics, &text_of) <= content.w,
    };
    let mut out = Vec::with_capacity(controls.len());
    let (mut x, mut y) = (content.x, content.y);
    for control in controls {
        let rect = if across {
            // Everything in a row is one row tall, including the labels and the rules, or the
            // strip has no baseline to read along.
            let w = control.width(metrics, text_of(control));
            if direction == Direction::Wrap && x > content.x && x + w > content.x + content.w {
                x = content.x;
                y += metrics.row + metrics.gap;
            }
            let r = Rect::new(x, y, w, metrics.row);
            x += w + metrics.gap;
            r
        } else {
            let h = control.height(metrics);
            let r = Rect::new(x, y, content.w, h);
            y += h + metrics.gap;
            r
        };
        out.push(Placed { control, rect });
    }

    // **Centred across the direction the controls run.** A row of buttons sits in the middle of
    // its strip's height rather than pinned to the top of it, which is what a menu bar looks like
    // everywhere; a wrapped grid runs both ways, so it is centred in its width too. *Along* the
    // direction they run they start at the beginning, because a list you read must start where
    // reading starts.
    let (block_w, block_h) = extent(&out, content);
    let dx = if across && direction == Direction::Wrap {
        ((content.w - block_w) / 2.0).max(0.0)
    } else {
        0.0
    };
    let dy = if across {
        ((content.h - block_h) / 2.0).max(0.0)
    } else {
        0.0
    };
    if dx > 0.0 || dy > 0.0 {
        for p in &mut out {
            p.rect = Rect::new(p.rect.x + dx, p.rect.y + dy, p.rect.w, p.rect.h);
        }
    }
    out
}

/// How wide the controls would be laid end to end.
#[must_use]
fn row_width(controls: &[Control], metrics: &Metrics, text_of: &impl Fn(&Control) -> f32) -> f32 {
    let gaps = metrics.gap * (controls.len().saturating_sub(1)) as f32;
    controls
        .iter()
        .map(|c| c.width(metrics, text_of(c)))
        .sum::<f32>()
        + gaps
}

/// How much room the placed controls actually take.
///
/// Measured from the rectangles rather than worked out a second time, because a list laid out one
/// way and measured another is the classic version of one definition living in two places
/// (recurring hazard 11a.8) — and here it would show up as a panel that scrolls past its own end.
#[must_use]
pub fn extent(placed: &[Placed<'_>], origin: Rect) -> (f32, f32) {
    placed.iter().fold((0.0_f32, 0.0_f32), |(w, h), p| {
        (
            w.max(p.rect.x + p.rect.w - origin.x),
            h.max(p.rect.y + p.rect.h - origin.y),
        )
    })
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
///
/// **Not `Copy` any more**, because a finished field carries its words. Nothing here is passed
/// anywhere hot enough for the clone to matter, and the alternative -- reporting only *that* it
/// changed and making the receiver go and look -- is a second place for the answer to live.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Pressed(ControlId),
    /// A slider moved. Already clamped to its range.
    Set(ControlId, f32),
    Toggled(ControlId, bool),
    /// A row was chosen.
    Chose(ControlId),
    /// Option `n` of a [`Control::Pick`] was chosen, counting from the list the panel offered.
    ///
    /// By position rather than by name, because the panel that offered the list is the one being
    /// told, and it has the list in front of it. A name would have to survive being spelled twice.
    Picked(ControlId, usize),
    /// A [`Control::Text`] took the caret. Nothing has changed yet.
    ///
    /// Reported so the panel can tell that a field is being edited -- to stop a shortcut eating
    /// the keystrokes, say -- and so the *engine* is not the only thing that knows.
    Typing(ControlId),
    /// A [`Control::Text`] was finished with, and this is what it says now.
    ///
    /// On Enter or on losing the caret, never per keystroke: a name applied letter by letter is a
    /// name that renames a layer eight times and puts eight steps on the undo stack.
    Typed(ControlId, String),
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

/// Where a row's switch sits inside it, if it has one.
///
/// **One definition, used to draw it and to decide what a press means.** Drawn in one place and
/// hit-tested in another, the two drift, and the symptom is a switch that does nothing along one
/// edge -- which is indistinguishable from a control that is simply broken.
#[must_use]
pub fn mark_rect(row: Rect, metrics: &Metrics) -> Rect {
    let w = metrics.row * 1.6;
    let h = metrics.row * 0.6;
    Rect::new(
        row.x + row.w - w - metrics.padding * 0.5,
        row.y + (row.h - h) / 2.0,
        w,
        h,
    )
}

/// The change a press or drag at `x` produces on `control`.
///
/// A slider maps the whole row's width, so its value follows the pointer wherever in the row it
/// is — dragging from anywhere on the row works, which matters when the track itself is three
/// units tall.
#[must_use]
pub fn change_at(
    control: &Control,
    rect: Rect,
    metrics: &Metrics,
    x: f32,
    y: f32,
) -> Option<Change> {
    // A row's switch is inside the row, so it is asked first: otherwise the row swallows every
    // press and the switch is decoration.
    if let Control::Row {
        mark: Some(mark), ..
    } = control
    {
        if mark_rect(rect, metrics).contains(x, y) {
            return Some(Change::Toggled(mark.id, !mark.on));
        }
    }
    match control {
        Control::Button { id, .. } => Some(Change::Pressed(*id)),
        Control::Toggle { id, on, .. } => Some(Change::Toggled(*id, !on)),
        Control::Choice { id, .. } | Control::Row { id, .. } => Some(Change::Chose(*id)),
        // **A press opens it; it does not choose anything by itself.** What the options are is
        // known to whoever built the control, and it answers by opening a list of them -- the same
        // way the menu opens its own items.
        Control::Pick { id, .. } => Some(Change::Pressed(*id)),
        // A press puts the caret in it. Where the caret goes is the field's own arithmetic and
        // needs the glyph positions, so it is settled where the text is drawn.
        Control::Text { id, .. } => Some(Change::Typing(*id)),
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
        // A custom drawing decides for itself what a press on it means, so nothing is reported
        // here -- only the rectangle it was given, which the panel needs in order to ask.
        Control::Label { .. } | Control::Separator | Control::Custom { .. } => None,
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

    /// A stand-in for a font: every label the same width, so a test says what it means about
    /// *layout* rather than about text measurement.
    fn text(_: &Control) -> f32 {
        40.0
    }

    fn column<'a>(controls: &'a [Control]) -> Vec<Placed<'a>> {
        place(controls, content(), &metrics(), Direction::Column, text)
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
        let placed = column(&controls);
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
        let placed = column(&controls);
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
        let placed = column(&controls);
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
        let placed = column(&controls);
        let m = metrics();
        let at = |i: usize, x: f32| {
            let r = placed[i].rect;
            change_at(placed[i].control, r, &m, x, r.y + r.h / 2.0)
        };

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

    /// How much room the list takes is read off the controls, not worked out again.
    #[test]
    fn the_extent_is_where_the_controls_actually_end() {
        let controls = sliders();
        let placed = column(&controls);
        let last = placed.last().expect("controls");
        let (w, h) = extent(&placed, content());
        assert!((h - ((last.rect.y + last.rect.h) - content().y)).abs() < 0.001);
        assert!(
            (w - content().w).abs() < 0.001,
            "a column is as wide as its panel"
        );
    }

    /// **A strip is a row or a column and never a half-broken row.**
    ///
    /// Wrapping onto a second line leaves a ragged edge and a last line with one item on it, and
    /// no arrangement of a panel makes that look deliberate. So `Auto` decides once, for the whole
    /// list: across if it fits, down if it does not.
    #[test]
    fn auto_turns_the_whole_strip_rather_than_wrapping_it() {
        let controls = sliders();
        let m = metrics();
        let wide = Rect::new(0.0, 0.0, 4000.0, 300.0);
        let narrow = Rect::new(0.0, 0.0, 120.0, 300.0);

        let across = place(&controls, wide, &m, Direction::Auto, text);
        let rows: std::collections::BTreeSet<i32> =
            across.iter().map(|p| p.rect.y as i32).collect();
        assert_eq!(rows.len(), 1, "with room to spare it should be one row");

        // And a row it chose is a row that actually fits, gaps included: deciding on the widths
        // alone puts the last control just past the edge, which is the ragged wrap by another
        // route.
        let last = across.last().expect("controls");
        assert!(
            last.rect.x + last.rect.w <= wide.x + wide.w + 0.001,
            "the row it chose runs off the panel"
        );

        let down = place(&controls, narrow, &m, Direction::Auto, text);
        let columns: std::collections::BTreeSet<i32> =
            down.iter().map(|p| p.rect.x as i32).collect();
        assert_eq!(columns.len(), 1, "too narrow, so it should be one column");

        // Exactly wide enough for the controls and not one unit more: the gaps between them still
        // have to go somewhere, so this must turn rather than overflow by a hair.
        let bare: f32 = controls.iter().map(|c| c.width(&m, text(c))).sum();
        let snug = place(
            &controls,
            Rect::new(0.0, 0.0, bare, 300.0),
            &m,
            Direction::Auto,
            text,
        );
        let cols: std::collections::BTreeSet<i32> = snug.iter().map(|p| p.rect.x as i32).collect();
        assert_eq!(
            cols.len(),
            1,
            "it fitted the controls but forgot the gaps between them"
        );

        // The thing being ruled out: something in between.
        for laid in [&across, &down] {
            let rows: std::collections::BTreeSet<i32> =
                laid.iter().map(|p| p.rect.y as i32).collect();
            let cols: std::collections::BTreeSet<i32> =
                laid.iter().map(|p| p.rect.x as i32).collect();
            assert!(
                rows.len() == 1 || cols.len() == 1,
                "a wrapped layout: {} rows and {} columns",
                rows.len(),
                cols.len()
            );
        }
    }

    /// **Wrap is for items of one width**, and then it is a grid rather than a ragged row.
    ///
    /// This is the one case where wrapping reads as deliberate: a tool rail of equal square
    /// buttons is what every paint application already has. It is not the default for anything
    /// carrying a label of its own.
    #[test]
    fn wrap_fills_lines_and_starts_a_new_one() {
        let m = metrics();
        let tools: Vec<Control> = (0..6)
            .map(|i| Control::Choice {
                id: i,
                text: "Tool".to_owned(),
                selected: false,
                icon: None,
            })
            .collect();
        let one = tools[0].width(&m, text(&tools[0]));
        // Room for three across, so six should land on two lines.
        let content = Rect::new(0.0, 0.0, one * 3.0 + m.gap * 2.0, 300.0);
        let laid = place(&tools, content, &m, Direction::Wrap, text);

        let lines: std::collections::BTreeSet<i32> = laid.iter().map(|p| p.rect.y as i32).collect();
        assert_eq!(lines.len(), 2, "six buttons, three to a line");
        for p in &laid {
            assert!(
                p.rect.x + p.rect.w <= content.x + content.w + 0.001,
                "a button ran off the edge and can never be pressed"
            );
        }
        // And no two overlap.
        for (i, a) in laid.iter().enumerate() {
            for b in laid.iter().skip(i + 1) {
                let apart = a.rect.x + a.rect.w <= b.rect.x + 0.001
                    || b.rect.x + b.rect.w <= a.rect.x + 0.001
                    || a.rect.y + a.rect.h <= b.rect.y + 0.001
                    || b.rect.y + b.rect.h <= a.rect.y + 0.001;
                assert!(apart, "two buttons overlap: {:?} and {:?}", a.rect, b.rect);
            }
        }
    }

    /// A choice is square at least, so a single glyph is still something you can hit.
    #[test]
    fn a_choice_is_never_thinner_than_it_is_tall() {
        let m = metrics();
        let c = Control::Choice {
            id: 0,
            text: String::new(),
            selected: false,
            icon: None,
        };
        assert!(c.width(&m, 0.0) >= m.row);
    }

    /// A row does not overlap itself, and everything in it shares a baseline.
    #[test]
    fn a_row_is_one_row_tall_and_runs_left_to_right() {
        let controls = sliders();
        let m = metrics();
        let laid = place(
            &controls,
            Rect::new(5.0, 7.0, 4000.0, 300.0),
            &m,
            Direction::Row,
            text,
        );
        for p in &laid {
            assert!(
                (p.rect.h - m.row).abs() < 0.001,
                "{:?} is {} tall in a row",
                p.control,
                p.rect.h
            );
            assert!(
                (p.rect.y - laid[0].rect.y).abs() < 0.001,
                "off the baseline the rest of the row sits on"
            );
        }
        for pair in laid.windows(2) {
            assert!(
                pair[1].rect.x >= pair[0].rect.x + pair[0].rect.w,
                "controls overlap along the row"
            );
        }
    }

    /// **A slider in a row is wide enough to slide.**
    ///
    /// Squeezed to its label it becomes a control with two useful positions, which is a button
    /// that lies about what it is. Measured in millimetres like every other target in the app: a
    /// logical unit is 1/96 inch, so the floor is a real distance rather than a number that looked
    /// right on one screen.
    #[test]
    fn a_slider_in_a_row_has_somewhere_to_slide() {
        const PER_MM: f32 = 96.0 / 25.4;
        let controls = sliders();
        let m = metrics();
        let laid = place(
            &controls,
            Rect::new(0.0, 0.0, 4000.0, 300.0),
            &m,
            Direction::Row,
            text,
        );
        for p in &laid {
            if !matches!(p.control, Control::Slider { .. }) {
                continue;
            }
            let mm = p.rect.w / PER_MM;
            assert!(
                mm >= 25.0,
                "a slider {mm:.1} mm wide cannot be set to anything but its ends"
            );
        }
    }

    /// **A row of controls sits in the middle of its strip, not pinned to the top.**
    ///
    /// A menu bar with its items hugging the top edge and a band of nothing beneath reads as a
    /// mistake, and it is what this looked like. Across the direction they run only: along it they
    /// start at the beginning, because a list you read must start where reading starts.
    #[test]
    fn a_row_is_centred_in_the_room_it_is_given() {
        let m = metrics();
        let controls = sliders();
        let content = Rect::new(0.0, 0.0, 4000.0, 200.0);
        let laid = place(&controls, content, &m, Direction::Row, text);
        // **The room above equals the room below.** Measuring the block and dividing would be
        // circular: `extent` reports where the controls ended up, offset included, so it would
        // agree with any offset at all.
        let top = laid[0].rect.y - content.y;
        let bottom = (content.y + content.h)
            - laid
                .iter()
                .map(|p| p.rect.y + p.rect.h)
                .fold(f32::MIN, f32::max);
        assert!(
            (top - bottom).abs() < 0.001,
            "{top} above the row and {bottom} below it"
        );
        assert!(top > 0.0, "it is still pinned to the top");
        // Along the row, it still begins at the beginning.
        assert!((laid[0].rect.x - content.x).abs() < 0.001);

        // A column is not pushed down: a list starts at the top and grows.
        let down = place(&controls, content, &m, Direction::Column, text);
        assert!((down[0].rect.y - content.y).abs() < 0.001);
    }

    /// A wrapped grid runs both ways, so it is centred both ways.
    #[test]
    fn a_wrapped_grid_is_centred_in_its_rail() {
        let m = metrics();
        let tools: Vec<Control> = (0..4)
            .map(|i| Control::Choice {
                id: i,
                text: "Tool".to_owned(),
                selected: false,
                icon: None,
            })
            .collect();
        let one = tools[0].width(&m, text(&tools[0]));
        // A rail with room for two across and plenty to spare either side.
        let content = Rect::new(0.0, 0.0, one * 2.0 + m.gap + 40.0, 400.0);
        let laid = place(&tools, content, &m, Direction::Wrap, text);
        let far = |f: fn(&Placed<'_>) -> f32| laid.iter().map(f).fold(f32::MIN, f32::max);
        let left = laid.iter().map(|p| p.rect.x).fold(f32::MAX, f32::min) - content.x;
        let right = (content.x + content.w) - far(|p| p.rect.x + p.rect.w);
        assert!(
            (left - right).abs() < 0.001,
            "{left} to the left of the grid and {right} to the right"
        );
        let top = laid.iter().map(|p| p.rect.y).fold(f32::MAX, f32::min) - content.y;
        let bottom = (content.y + content.h) - far(|p| p.rect.y + p.rect.h);
        assert!(
            (top - bottom).abs() < 0.001,
            "{top} above the grid and {bottom} below it"
        );
        assert!(left > 0.0 && top > 0.0, "it is still in a corner");
    }

    /// Both directions stay hittable: a control's rectangle is its target either way.
    #[test]
    fn a_control_can_be_pressed_in_either_direction() {
        let controls = sliders();
        let m = metrics();
        for direction in [Direction::Row, Direction::Column] {
            let laid = place(
                &controls,
                Rect::new(0.0, 0.0, 4000.0, 300.0),
                &m,
                direction,
                text,
            );
            let slider = laid
                .iter()
                .find(|p| matches!(p.control, Control::Slider { id: 1, .. }))
                .expect("the size slider");
            let r = slider.rect;
            let found = hit(&laid, r.x + r.w / 2.0, r.y + r.h / 2.0);
            assert!(
                matches!(found, Some((Control::Slider { id: 1, .. }, _))),
                "{direction:?}: the slider is not where it was drawn"
            );
        }
    }

    /// **A row's switch is its own answer.** A press on the switch shows or hides; a press
    /// anywhere else on the row chooses it.
    ///
    /// Without the switch having its own target, hiding a layer meant selecting it first and then
    /// finding a control below the list, which is two steps for something every paint application
    /// does in one.
    #[test]
    fn a_rows_switch_answers_before_the_row_does() {
        let m = metrics();
        let rows = vec![Control::Row {
            id: 7,
            text: "Ink".to_owned(),
            selected: false,
            swatch: None,
            mark: Some(RowMark { id: 107, on: true }),
        }];
        let laid = column(&rows);
        let r = laid[0].rect;
        let pill = mark_rect(r, &m);

        assert_eq!(
            change_at(
                laid[0].control,
                r,
                &m,
                pill.x + pill.w / 2.0,
                pill.y + pill.h / 2.0
            ),
            Some(Change::Toggled(107, false)),
            "a press on the switch flips it"
        );
        assert_eq!(
            change_at(laid[0].control, r, &m, r.x + 4.0, r.y + r.h / 2.0),
            Some(Change::Chose(7)),
            "a press elsewhere on the row chooses the row"
        );
        // The switch is inside the row, so the row must not swallow it.
        assert!(
            pill.x >= r.x && pill.x + pill.w <= r.x + r.w,
            "the switch should sit inside its row"
        );
    }

    /// A row without a switch is just a row.
    #[test]
    fn a_row_with_no_switch_answers_everywhere() {
        let m = metrics();
        let rows = vec![Control::Row {
            id: 7,
            text: "Ink".to_owned(),
            selected: false,
            swatch: None,
            mark: None,
        }];
        let laid = column(&rows);
        let r = laid[0].rect;
        let pill = mark_rect(r, &m);
        assert_eq!(
            change_at(
                laid[0].control,
                r,
                &m,
                pill.x + pill.w / 2.0,
                pill.y + pill.h / 2.0
            ),
            Some(Change::Chose(7))
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
        let placed = column(&[]);
        assert!(placed.is_empty());
        assert!(hit(&placed, 50.0, 50.0).is_none());
        assert_eq!(extent(&placed, content()), (0.0, 0.0));
    }
}
