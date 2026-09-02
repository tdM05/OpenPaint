//! The page itself: its size, cropping it, growing it.
//!
//! One module per panel, exporting one function. See [`super`] for why.

use super::{Painting, Picked};
use crate::panel_ui::{Change, Control, ControlId};
use crate::ui::{CropAction, Status};
use crate::workspace::Place;
use openpaint_core::{Brush, Side};

const EXTEND_BY: ControlId = 0;
const EXTEND_DOWN: ControlId = 1;
const EXTEND_UP: ControlId = 2;
const EXTEND_LEFT: ControlId = 3;
const EXTEND_RIGHT: ControlId = 4;
const CROP_START: ControlId = 5;
const CROP_APPLY: ControlId = 6;
const CROP_CANCEL: ControlId = 7;
const TRIM: ControlId = 8;

/// The smallest and largest single extend.
///
/// The old panel's numbers, kept: below about 32 px an extend is not worth a step on the undo
/// stack, and above 4096 it is a new document rather than a bigger page.
const EXTEND_MIN: u32 = 32;
const EXTEND_MAX: u32 = 4096;

/// Draw the panel and report what the artist asked for.
pub(crate) fn show(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    color_srgb: &mut [u8; 3],
    state: &Status<'_>,
    paint: &mut Painting<'_>,
    place: Place,
) -> Option<Picked> {
    let mut picked: Option<Picked> = None;
    let _ = (&mut *brush, &mut *color_srgb, place);

    // How much an extend adds is a panel setting rather than document state, so it arrives on
    // `Painting` rather than on `Status`: the shell holds it, this panel shows it and answers
    // `ExtendBy` when it moves, and the buttons below extend by whatever it currently says. It
    // used to be unreadable from here -- the slider showed the default every frame and stopped
    // agreeing with the buttons the moment it was dragged -- and this comment used to say so.
    let extend_by = paint.extend_by;

    let controls = controls(state.page_size, state.crop_rect, extend_by);
    for change in paint.show(ui, &controls) {
        picked = act(&change, extend_by);
    }
    picked
}

/// What the panel shows, given the page as it stands.
///
/// Split out from [`show`] because it is the half worth testing: which controls exist, and what
/// each one is called. It takes the three values it reads rather than the whole [`Status`], so a
/// test can say "a crop is in flight" without building a document to say it with.
fn controls(
    page_size: (u32, u32),
    crop_rect: Option<(i32, i32, u32, u32)>,
    extend_by: u32,
) -> Vec<Control> {
    let (w, h) = page_size;
    let mut controls = vec![
        Control::Label {
            text: format!("{w} x {h} px"),
        },
        Control::Slider {
            id: EXTEND_BY,
            text: "Extend by (px)".to_owned(),
            value: extend_by as f32,
            min: EXTEND_MIN as f32,
            max: EXTEND_MAX as f32,
            // The unit is already in the label, where the old panel had it. Saying it twice would
            // read "Extend by (px)   512 px".
            unit: "",
            // Logarithmic, because the useful settings are bunched at the small end: 64 and 128
            // are different sizes of page, 3000 and 3100 are the same one.
            log: true,
        },
    ];
    // Only the first names the direction in full. Read along the strip, "Extend down / up / left /
    // right" is one sentence; repeating the verb four times is four times the width for no extra
    // meaning.
    for (id, text) in [
        (EXTEND_DOWN, "Extend down"),
        (EXTEND_UP, "up"),
        (EXTEND_LEFT, "left"),
        (EXTEND_RIGHT, "right"),
    ] {
        controls.push(Control::Button {
            id,
            text: text.to_owned(),
        });
    }
    controls.push(Control::Separator);
    match crop_rect {
        // **Starting a crop and settling one are never both offered.** A crop is a gesture in
        // flight, and a second Start in the middle of one is a button whose only honest behaviour
        // is to refuse (DECISIONS 6b).
        None => controls.push(Control::Button {
            id: CROP_START,
            text: "Crop / resize by dragging".to_owned(),
        }),
        Some((x, y, w, h)) => {
            controls.push(Control::Label {
                text: format!("Crop to {w} x {h} at ({x}, {y})"),
            });
            controls.push(Control::Button {
                id: CROP_APPLY,
                text: "Apply".to_owned(),
            });
            controls.push(Control::Button {
                id: CROP_CANCEL,
                text: "Cancel".to_owned(),
            });
            controls.push(Control::Label {
                text: "Drag an edge or corner; drag inside to move it. Dragging outward extends \
                       the page. Enter applies, Escape cancels."
                    .to_owned(),
            });
        }
    }
    // Trim is the one thing on this panel that throws pixels away, so it is kept apart from the
    // two that do not.
    controls.push(Control::Separator);
    controls.push(Control::Button {
        id: TRIM,
        text: "Trim to canvas".to_owned(),
    });
    controls.push(Control::Label {
        text: "Cropping keeps the pixels outside the page, so nothing is lost by accident. Trim \
               discards them for good -- undoably."
            .to_owned(),
    });
    controls
}

/// What one change means.
///
/// `extend_by` comes in rather than being carried by each button: the amount is one setting and
/// four buttons, and baking it into each of them would be one number in four places.
fn act(change: &Change, extend_by: u32) -> Option<Picked> {
    match *change {
        // Rounded rather than truncated: a slider dragged to 511.6 means 512, and `from_fraction`
        // has already held the value inside the range.
        Change::Set(EXTEND_BY, v) => Some(Picked::ExtendBy(v.round() as u32)),
        Change::Pressed(EXTEND_DOWN) => Some(Picked::Extend(Side::Bottom, extend_by)),
        Change::Pressed(EXTEND_UP) => Some(Picked::Extend(Side::Top, extend_by)),
        Change::Pressed(EXTEND_LEFT) => Some(Picked::Extend(Side::Left, extend_by)),
        Change::Pressed(EXTEND_RIGHT) => Some(Picked::Extend(Side::Right, extend_by)),
        Change::Pressed(CROP_START) => Some(Picked::Crop(CropAction::Start)),
        Change::Pressed(CROP_APPLY) => Some(Picked::Crop(CropAction::Apply)),
        Change::Pressed(CROP_CANCEL) => Some(Picked::Crop(CropAction::Cancel)),
        Change::Pressed(TRIM) => Some(Picked::Trim),
        // Not a catch-all out of laziness: an id this panel did not put in its own list is a bug
        // in the renderer, and swallowing it is the silence DECISIONS 6b forbids.
        ref other => {
            eprintln!("page panel: unexpected {other:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(controls: &[Control]) -> Vec<String> {
        controls
            .iter()
            .map(|c| match c {
                Control::Label { text }
                | Control::Button { text, .. }
                | Control::Slider { text, .. } => text.clone(),
                Control::Separator => "---".to_owned(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn id_of(controls: &[Control], label: &str) -> ControlId {
        controls
            .iter()
            .find_map(|c| match c {
                Control::Button { id, text } if text == label => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no button called {label:?}"))
    }

    /// With no crop in flight the panel offers to start one, and says nothing else about cropping.
    #[test]
    fn with_no_crop_it_offers_to_start_one() {
        let shown = labels(&controls((1920, 1080), None, 512));
        assert!(shown.contains(&"1920 x 1080 px".to_owned()), "{shown:?}");
        assert!(shown.contains(&"Crop / resize by dragging".to_owned()));
        assert!(!shown.contains(&"Apply".to_owned()));
        assert!(!shown.contains(&"Cancel".to_owned()));
        // The rest of the panel does not depend on the crop.
        assert!(shown.contains(&"Extend down".to_owned()));
        assert!(shown.contains(&"Trim to canvas".to_owned()));
    }

    /// **A crop in flight replaces Start rather than joining it**, and the panel says where it is.
    #[test]
    fn a_crop_in_flight_swaps_start_for_apply_and_cancel() {
        let shown = labels(&controls((1920, 1080), Some((-10, 20, 640, 480)), 512));
        assert!(
            !shown.contains(&"Crop / resize by dragging".to_owned()),
            "{shown:?}"
        );
        assert!(shown.contains(&"Apply".to_owned()));
        assert!(shown.contains(&"Cancel".to_owned()));
        // A negative origin is ordinary rather than an edge case: dragging outward extends the
        // page, so the rectangle can start off the top-left of it.
        assert!(
            shown.contains(&"Crop to 640 x 480 at (-10, 20)".to_owned()),
            "{shown:?}"
        );
    }

    /// **Each extend button grows the side it is named after.** Four buttons, four sides, three of
    /// them labelled with a single word: a swapped pair reads correctly and grows the page the
    /// wrong way, and nothing on screen would say so.
    ///
    /// Looked up by label rather than by constant, so the label, the id and the side are checked
    /// against each other -- naming the constants here would agree with itself even if every
    /// button were mislabelled.
    #[test]
    fn each_extend_button_grows_the_side_it_is_named_after() {
        let controls = controls((100, 100), None, 512);
        for (label, side) in [
            ("Extend down", Side::Bottom),
            ("up", Side::Top),
            ("left", Side::Left),
            ("right", Side::Right),
        ] {
            let change = Change::Pressed(id_of(&controls, label));
            assert_eq!(
                act(&change, 256),
                Some(Picked::Extend(side, 256)),
                "{label:?} does not extend {side:?}"
            );
        }
    }

    /// The crop and trim buttons mean what they are labelled to do.
    #[test]
    fn the_crop_and_trim_buttons_mean_what_they_say() {
        let idle = controls((100, 100), None, 512);
        let cropping = controls((100, 100), Some((0, 0, 10, 10)), 512);
        let pressed =
            |controls: &[Control], label: &str| act(&Change::Pressed(id_of(controls, label)), 512);
        assert_eq!(
            pressed(&idle, "Crop / resize by dragging"),
            Some(Picked::Crop(CropAction::Start))
        );
        assert_eq!(
            pressed(&cropping, "Apply"),
            Some(Picked::Crop(CropAction::Apply))
        );
        assert_eq!(
            pressed(&cropping, "Cancel"),
            Some(Picked::Crop(CropAction::Cancel))
        );
        assert_eq!(pressed(&idle, "Trim to canvas"), Some(Picked::Trim));
    }

    /// The slider answers in whole pixels, on the scale the small end needs.
    #[test]
    fn the_extend_slider_answers_in_whole_pixels() {
        let controls = controls((100, 100), None, 512);
        let slider = controls
            .iter()
            .find(|c| matches!(c, Control::Slider { id: EXTEND_BY, .. }))
            .expect("the extend slider");
        let Control::Slider {
            value,
            min,
            max,
            log,
            ..
        } = slider
        else {
            unreachable!("just matched a slider")
        };
        assert!((*value - 512.0).abs() < 0.001, "it should start at 512");
        assert!((*min - 32.0).abs() < 0.001 && (*max - 4096.0).abs() < 0.001);
        assert!(*log, "the useful settings are bunched at the small end");

        assert_eq!(
            act(&Change::Set(EXTEND_BY, 511.6), 512),
            Some(Picked::ExtendBy(512))
        );
        // A drag is clamped before it reaches here, so both ends land exactly.
        assert_eq!(
            act(&Change::Set(EXTEND_BY, EXTEND_MIN as f32), 512),
            Some(Picked::ExtendBy(EXTEND_MIN))
        );
        assert_eq!(
            act(&Change::Set(EXTEND_BY, EXTEND_MAX as f32), 512),
            Some(Picked::ExtendBy(EXTEND_MAX))
        );
    }

    /// **No two controls share an id.** Two buttons with one id is one button quietly doing the
    /// other's job, and the panel looks perfectly correct while doing it.
    #[test]
    fn every_control_answers_to_its_own_id() {
        for crop in [None, Some((0, 0, 10, 10))] {
            let controls = controls((100, 100), crop, 512);
            let mut ids: Vec<ControlId> = controls.iter().filter_map(Control::id).collect();
            let count = ids.len();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), count, "two controls share an id");
        }
    }
}
