//! Scale and rotate what is selected.
//!
//! One module per panel, exporting one function. See [`super`] for why.
//!
//! **The canvas is the instrument; this panel is the readout and the coarse control.** Dragging a
//! handle is how a transform is actually set, and the box on the canvas says so. What is here is
//! what a handle cannot do: exact-ish numbers, a lock, a flip, and the two commands that end the
//! transform one way or the other.

use super::{Painting, Picked};
use crate::panel_ui::{Change, Control};
use crate::ui::{Status, TransformAction, TransformState};
use crate::workspace::Place;
use openpaint_core::{Brush, Kernel, Transform};

const SCALE_X: u32 = 1;
const SCALE_Y: u32 = 2;
const LOCK: u32 = 3;
const ROTATION: u32 = 4;
const FLIP_H: u32 = 5;
const FLIP_V: u32 = 6;
const APPLY: u32 = 7;
const CANCEL: u32 = 8;
const BEGIN: u32 = 9;
const KERNEL: u32 = 10;

/// The ends of the scale sliders, as percentages.
///
/// **A decade either side of 1:1, logarithmic, so 100% sits dead centre** and halving is the same
/// gesture as doubling. The old panel used a drag value, which has no ends at all; there is no
/// drag value in the descriptor vocabulary, and inventing one for this panel would be inventing a
/// control for the whole application. A slider is the honest substitute here because the precise
/// instrument is the handle on the canvas -- this is for "make it twice the size", and for reading
/// off what a drag did.
///
/// Not the suggested 1%..800%: on a logarithmic track that leaves 1:1 two thirds of the way along,
/// so the resting position of every fresh transform is off-centre and shrinking gets four fifths
/// of the track for sizes nobody can see well enough to draw with.
///
/// A handle can still be dragged past either end -- these are the *slider's* ends, not the
/// transform's -- and then the readout tells the truth while the knob pins itself. That is the
/// right way round: what is lost is a position on a track, not a number.
const SCALE_MIN: f32 = 10.0;
const SCALE_MAX: f32 = 1000.0;

/// Draw the panel and report what the artist asked for.
pub(crate) fn show(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    color_srgb: &mut [u8; 3],
    state: &Status<'_>,
    paint: &mut Painting<'_>,
    place: Place,
) -> Option<Picked> {
    let _ = (&mut *brush, &mut *color_srgb);
    let live = state.transform;
    let kernels: Vec<String> = Kernel::ALL.iter().map(|k| k.label().to_owned()).collect();

    // The dropdown's other half, exactly as the layers panel does it: one popup at a time, and the
    // workspace decides where it goes.
    if place == Place::Popup {
        let chosen = Kernel::ALL
            .iter()
            .position(|k| *k == state.kernel)
            .unwrap_or(0);
        let mut picked = None;
        if let Some(n) = super::pick_popup(KERNEL, &kernels, chosen, ui, paint) {
            if let Some(kernel) = Kernel::ALL.get(n) {
                picked = Some(Picked::TransformSet(TransformState {
                    kernel: *kernel,
                    ..base(live, state.kernel)
                }));
            }
        }
        return picked;
    }

    let controls = controls(live, state.has_selection, state.kernel);
    let mut picked = None;
    for change in paint.show(ui, &controls) {
        picked = match change {
            // Opening the list needs the rectangle that was pressed, which only the engine has, so
            // this one arm cannot be part of the pure decision below.
            Change::Pressed(KERNEL) => super::open_pick(KERNEL, &kernels, paint),
            other => decide(&other, live, state.kernel),
        };
    }
    picked
}

/// What the panel shows, given what it is looking at.
///
/// Split out from [`show`] because it is the whole of the panel's judgement and none of its
/// drawing: what appears for a given document is a pure function, and a pure function can be
/// checked without a GPU.
fn controls(live: Option<TransformState>, has_selection: bool, kernel: Kernel) -> Vec<Control> {
    let mut controls = Vec::new();
    if let Some(state) = live {
        let t = state.transform;
        // **The gesture, spelled out.** The transform is driven from the canvas, and nothing on
        // the canvas says so; a box with handles is only obvious once somebody has told you.
        //
        controls.push(Control::Label {
            text: "On the canvas: drag inside to move, a handle to scale, just outside to rotate. Enter applies, Esc puts it back."
                .to_owned(),
        });
        // **Magnitude only.** A flipped axis is a negative scale, and a slider that ran from -1000
        // to 1000 would put 1:1 in two places and spend most of its track on sizes nobody wants.
        // So the sign belongs to the flip buttons and the slider carries how big it is -- which
        // also means dragging the slider on a flipped selection does not quietly unflip it.
        controls.push(scale_slider(SCALE_X, "Scale X", t.scale.0));
        controls.push(scale_slider(SCALE_Y, "Scale Y", t.scale.1));
        controls.push(Control::Toggle {
            id: LOCK,
            text: "Lock".to_owned(),
            on: state.lock_aspect,
        });
        // The old panel's tooltip, as a line of text. There is no tooltip in the vocabulary and
        // one is not worth adding for a single control -- and on a pen or a finger there is no
        // hover to show it with anyway, so the words would have been invisible to half the users
        // (§6b: say it rather than hide it).
        controls.push(Control::Label {
            text: "Keep the two axes equal, here and when dragging a corner.".to_owned(),
        });
        controls.push(Control::Slider {
            id: ROTATION,
            text: "Rotation".to_owned(),
            // Degrees here, radians in the document. The conversion lives in this one pair of
            // places -- built here, undone in `decide` -- because a panel is the only part of the
            // application that has any use for degrees.
            value: t.rotation.to_degrees(),
            min: -180.0,
            max: 180.0,
            unit: "\u{b0}",
            log: false,
        });
        for (id, text) in [(FLIP_H, "Flip horizontally"), (FLIP_V, "Flip vertically")] {
            controls.push(Control::Button {
                id,
                text: text.to_owned(),
            });
        }
        controls.push(Control::Separator);
        for (id, text) in [(APPLY, "Apply"), (CANCEL, "Cancel")] {
            controls.push(Control::Button {
                id,
                text: text.to_owned(),
            });
        }
    } else if has_selection {
        controls.push(Control::Button {
            id: BEGIN,
            text: "Transform selection".to_owned(),
        });
    } else {
        // **Absent rather than dead.** The old panel greyed this out, and there is no disabled
        // state to describe -- deliberately, since a greyed control is a control that will not say
        // why. A button that answers nothing is worse than no button: it reads as broken. So the
        // button goes and the reason takes its place, which is the same information without the
        // dead target (§6b).
        controls.push(Control::Label {
            text: "Select something first to transform it.".to_owned(),
        });
    }
    controls.push(Control::Separator);
    // Offered whether or not a transform is in flight, because it is the filter the *next* one
    // will use as much as this one -- which is why `Status` carries it separately.
    controls.push(Control::Pick {
        id: KERNEL,
        text: "Resampling".to_owned(),
        value: kernel.label().to_owned(),
    });
    controls
}

/// One axis of the scale, as a percentage of its own size.
fn scale_slider(id: u32, text: &str, scale: f32) -> Control {
    Control::Slider {
        id,
        text: text.to_owned(),
        value: scale.abs() * 100.0,
        min: SCALE_MIN,
        max: SCALE_MAX,
        unit: "%",
        log: true,
    }
}

/// The state a change is reported against.
///
/// With nothing in the air there is still a kernel to set, so there is still something to send.
/// The identity it is hung on costs nothing: the shell only ever reads the kernel out of it unless
/// a drag is actually running, so this cannot conjure a transform that nobody started.
fn base(live: Option<TransformState>, kernel: Kernel) -> TransformState {
    live.unwrap_or(TransformState {
        transform: Transform::IDENTITY,
        lock_aspect: false,
        kernel,
    })
}

/// Put the slider's magnitude back on an axis, keeping which way that axis faces.
fn resize(axis: f32, percent: f32) -> f32 {
    let size = percent / 100.0;
    // `signum` is not it: it answers -1 for -0.0, so an axis that had been flipped and flipped
    // back would stay negative for ever.
    if axis < 0.0 {
        -size
    } else {
        size
    }
}

/// What a change means, given what the panel was showing when it happened.
///
/// The whole of the panel's other half, and pure for the same reason [`controls`] is.
fn decide(change: &Change, live: Option<TransformState>, kernel: Kernel) -> Option<Picked> {
    if live.is_none() {
        return match change {
            Change::Pressed(BEGIN) => Some(Picked::Transform(TransformAction::Begin)),
            other => {
                eprintln!("transform panel: unexpected {other:?} with nothing in flight");
                None
            }
        };
    }
    let mut state = base(live, kernel);
    let t = &mut state.transform;
    match change {
        Change::Set(SCALE_X, percent) => {
            t.scale.0 = resize(t.scale.0, *percent);
            if state.lock_aspect {
                t.scale.1 = resize(t.scale.1, *percent);
            }
            Some(Picked::TransformSet(state))
        }
        Change::Set(SCALE_Y, percent) => {
            t.scale.1 = resize(t.scale.1, *percent);
            if state.lock_aspect {
                t.scale.0 = resize(t.scale.0, *percent);
            }
            Some(Picked::TransformSet(state))
        }
        Change::Set(ROTATION, degrees) => {
            t.rotation = degrees.to_radians();
            Some(Picked::TransformSet(state))
        }
        Change::Toggled(LOCK, on) => {
            // Locking takes effect at once rather than at the next drag: a lock that only bites
            // later is a lock you cannot tell you have set.
            if *on {
                t.scale.1 = t.scale.0;
            }
            state.lock_aspect = *on;
            Some(Picked::TransformSet(state))
        }
        Change::Pressed(FLIP_H) => {
            t.scale.0 = -t.scale.0;
            Some(Picked::TransformSet(state))
        }
        Change::Pressed(FLIP_V) => {
            t.scale.1 = -t.scale.1;
            Some(Picked::TransformSet(state))
        }
        Change::Pressed(APPLY) => Some(Picked::Transform(TransformAction::Apply)),
        Change::Pressed(CANCEL) => Some(Picked::Transform(TransformAction::Cancel)),
        other => {
            eprintln!("transform panel: unexpected {other:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flight() -> TransformState {
        TransformState {
            transform: Transform {
                scale: (2.0, 0.5),
                rotation: std::f32::consts::FRAC_PI_2,
                ..Transform::IDENTITY
            },
            lock_aspect: false,
            kernel: Kernel::Mitchell,
        }
    }

    fn ids(controls: &[Control]) -> Vec<u32> {
        controls.iter().filter_map(Control::id).collect()
    }

    /// The transform a change asked for, or a failed test saying what came back instead.
    fn set(change: &Change, state: TransformState) -> TransformState {
        match decide(change, Some(state), state.kernel) {
            Some(Picked::TransformSet(s)) => s,
            other => panic!("{change:?} should have set the transform, but gave {other:?}"),
        }
    }

    /// The panel is one of three things, and which one is decided by the document alone.
    #[test]
    fn what_it_offers_depends_on_what_there_is_to_transform() {
        let in_flight = ids(&controls(Some(flight()), false, Kernel::Mitchell));
        assert!(in_flight.contains(&SCALE_X), "no scale while transforming");
        assert!(in_flight.contains(&APPLY) && in_flight.contains(&CANCEL));
        assert!(
            !in_flight.contains(&BEGIN),
            "a transform already in the air cannot be begun again"
        );

        let ready = ids(&controls(None, true, Kernel::Mitchell));
        assert_eq!(ready, vec![BEGIN, KERNEL]);
    }

    /// **Nothing selected means no button, not a dead one.**
    ///
    /// There is no disabled state to describe, and a button that answers nothing reads as broken.
    /// The words that replace it say why, which greying it out never did.
    #[test]
    fn with_nothing_selected_the_button_is_gone_and_the_reason_is_there() {
        let controls = controls(None, false, Kernel::Mitchell);
        assert!(
            !ids(&controls).contains(&BEGIN),
            "a button nobody can use is still a button people will press"
        );
        assert!(
            controls.iter().any(|c| matches!(
                c,
                Control::Label { text } if text.contains("Select something first")
            )),
            "the panel went quiet instead of saying why"
        );
    }

    /// The resampling filter is offered whether or not a transform is in flight — it is the filter
    /// the *next* one will use as much as this one.
    #[test]
    fn the_kernel_is_always_there_and_says_which_one_is_set() {
        for live in [Some(flight()), None] {
            for has_selection in [true, false] {
                let controls = controls(live, has_selection, Kernel::Bilinear);
                assert!(
                    controls.iter().any(|c| matches!(
                        c,
                        Control::Pick { id: KERNEL, value, .. } if value == Kernel::Bilinear.label()
                    )),
                    "the resampling pick is missing or shows the wrong filter"
                );
            }
        }
    }

    /// **The slider carries how big, not which way round.**
    ///
    /// A flipped axis is a negative scale. Showing it as a negative percentage would pin the
    /// slider to its low end and any touch of it would silently unflip the selection.
    #[test]
    fn a_flipped_axis_still_reads_as_its_size() {
        let mut state = flight();
        state.transform.scale = (-2.0, 0.5);
        let controls = controls(Some(state), false, Kernel::Mitchell);
        assert!(
            controls.iter().any(|c| matches!(
                c,
                Control::Slider { id: SCALE_X, value, .. } if (*value - 200.0).abs() < 0.001
            )),
            "a flip should not move the slider"
        );

        // And setting it keeps the flip.
        let after = set(&Change::Set(SCALE_X, 300.0), state);
        assert!((after.transform.scale.0 + 3.0).abs() < 0.001, "unflipped");
    }

    /// Degrees in the panel, radians in the document, and the two agree.
    #[test]
    fn rotation_is_shown_in_degrees_and_reported_in_radians() {
        let state = flight();
        let controls = controls(Some(state), false, Kernel::Mitchell);
        assert!(
            controls.iter().any(|c| matches!(
                c,
                Control::Slider { id: ROTATION, value, min, max, .. }
                    if (*value - 90.0).abs() < 0.001 && *min == -180.0 && *max == 180.0
            )),
            "a right angle should read as 90 degrees"
        );

        let after = set(&Change::Set(ROTATION, -45.0), state);
        assert!((after.transform.rotation - (-45.0_f32).to_radians()).abs() < 0.0001);
    }

    /// **A lock that only bites on the next drag is a lock you cannot tell you have set.**
    ///
    /// Turning it on evens the axes there and then, and afterwards moving one slider moves both.
    #[test]
    fn locking_evens_the_axes_and_keeps_them_even() {
        let state = flight();
        let locked = set(&Change::Toggled(LOCK, true), state);
        assert!(locked.lock_aspect);
        assert_eq!(locked.transform.scale, (2.0, 2.0));

        for id in [SCALE_X, SCALE_Y] {
            let after = set(&Change::Set(id, 50.0), locked);
            assert_eq!(
                after.transform.scale,
                (0.5, 0.5),
                "locked, moving one axis should move both"
            );
        }

        // Unlocked, one axis moves alone -- to a size the other one is not already at, or the
        // test would pass whether or not the lock was consulted.
        let after = set(&Change::Set(SCALE_X, 300.0), state);
        assert_eq!(after.transform.scale, (3.0, 0.5), "y should not have moved");
    }

    /// Flipping negates one axis and leaves the other alone, lock or no lock.
    ///
    /// The lock is about *size*; a selection flipped on both axes at once is a rotation by half a
    /// turn, which is not what pressing one flip button asks for.
    #[test]
    fn a_flip_turns_one_axis_over() {
        for lock in [false, true] {
            let mut state = flight();
            state.lock_aspect = lock;
            let after = set(&Change::Pressed(FLIP_H), state);
            assert_eq!(after.transform.scale, (-2.0, 0.5));

            let after = set(&Change::Pressed(FLIP_V), state);
            assert_eq!(after.transform.scale, (2.0, -0.5));
        }
    }

    /// Flipping twice comes back to where it started, including the sign of the zero.
    #[test]
    fn flipping_twice_is_where_you_began() {
        let mut state = flight();
        for _ in 0..2 {
            state = set(&Change::Pressed(FLIP_H), state);
        }
        assert_eq!(state.transform.scale, (2.0, 0.5));
        // And the slider still puts a positive size back positive.
        assert!(resize(state.transform.scale.0, 150.0) > 0.0);
    }

    /// The two commands that end a transform say so, and neither is a `TransformSet`.
    #[test]
    fn apply_and_cancel_are_commands_not_settings() {
        let state = flight();
        assert_eq!(
            decide(&Change::Pressed(APPLY), Some(state), state.kernel),
            Some(Picked::Transform(TransformAction::Apply))
        );
        assert_eq!(
            decide(&Change::Pressed(CANCEL), Some(state), state.kernel),
            Some(Picked::Transform(TransformAction::Cancel))
        );
        assert!(
            !matches!(
                decide(&Change::Pressed(APPLY), Some(state), state.kernel),
                Some(Picked::TransformSet(_))
            ),
            "Apply must not also set the transform"
        );
    }

    /// With nothing in flight the only thing the panel answers is "begin".
    #[test]
    fn with_nothing_in_flight_only_begin_answers() {
        assert_eq!(
            decide(&Change::Pressed(BEGIN), None, Kernel::Mitchell),
            Some(Picked::Transform(TransformAction::Begin))
        );
        // A stale press against a panel that has since changed shape sets nothing rather than
        // conjuring a transform nobody started.
        for change in [
            Change::Set(SCALE_X, 200.0),
            Change::Pressed(APPLY),
            Change::Toggled(LOCK, true),
        ] {
            assert_eq!(decide(&change, None, Kernel::Mitchell), None);
        }
    }

    /// A kernel chosen with nothing in flight is still a kernel: it hangs on the identity, which
    /// the shell reads the filter out of and otherwise ignores.
    #[test]
    fn the_kernel_survives_having_no_transform_to_hang_on() {
        let state = base(None, Kernel::CatmullRom);
        assert_eq!(state.kernel, Kernel::CatmullRom);
        assert_eq!(state.transform, Transform::IDENTITY);
        assert!(!state.lock_aspect);
        // And with one in flight it is that one, untouched.
        assert_eq!(base(Some(flight()), Kernel::CatmullRom), flight());
    }

    /// 1:1 sits in the middle of the scale track, so a fresh transform rests at the centre and
    /// halving is the same gesture as doubling.
    #[test]
    fn the_scale_track_is_centred_on_full_size() {
        let middle = crate::panel_ui::to_fraction(100.0, SCALE_MIN, SCALE_MAX, true);
        assert!((middle - 0.5).abs() < 0.001, "100% draws at {middle}");
    }
}
