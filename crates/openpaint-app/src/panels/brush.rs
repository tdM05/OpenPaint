//! The brush: what the mark looks like.
//!
//! One module per panel, exporting one function. See [`super`] for why.

use super::{Painting, Picked};
use crate::ui::Status;
use crate::workspace::Place;
use openpaint_core::Brush;

/// Draw the panel and report what the artist asked for.
pub(crate) fn show(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    color_srgb: &mut [u8; 3],
    state: &Status<'_>,
    paint: &mut Painting<'_>,
    place: Place,
) -> Option<Picked> {
    let picked: Option<Picked> = None;
    let _ = (&mut *brush, &mut *color_srgb, state, place);
    let layers = state.layers;
    let active_layer = state.active_layer;
    let tool = state.tool;
    let select_tool = state.select_tool;
    let _ = (layers, active_layer, tool, select_tool);
    // **The first panel described rather than drawn.** Nothing below says what a slider
    // looks like or how a drag becomes a number; it says what the controls *are* and what
    // they currently hold, and applies what comes back.
    //
    // The reason this one went first: it is four sliders and nothing else, so if the
    // descriptor layer could not carry it there would be no point carrying on — and it is
    // small enough that finding out costs an afternoon rather than a rewrite.
    use crate::panel_ui::{Change, Control};
    const SIZE: u32 = 0;
    const OPACITY: u32 = 1;
    const HARDNESS: u32 = 2;
    const SPACING: u32 = 3;

    let controls = vec![
        Control::Slider {
            id: SIZE,
            text: "Size".to_owned(),
            value: brush.radius,
            min: 0.5,
            max: 400.0,
            unit: "px",
            // Logarithmic, because one step at radius 4 should feel like one step at
            // radius 40 — which is what a paint app means by size.
            log: true,
        },
        Control::Slider {
            id: OPACITY,
            text: "Opacity".to_owned(),
            value: brush.opacity,
            min: 0.0,
            max: 1.0,
            unit: "",
            log: false,
        },
        Control::Slider {
            id: HARDNESS,
            text: "Hardness".to_owned(),
            value: brush.hardness,
            min: 0.0,
            max: 1.0,
            unit: "",
            log: false,
        },
        Control::Slider {
            id: SPACING,
            text: "Spacing".to_owned(),
            value: brush.spacing,
            min: 0.01,
            max: 1.0,
            unit: "",
            log: false,
        },
    ];
    for change in paint.show(ui, &controls) {
        match change {
            Change::Set(SIZE, v) => brush.radius = v,
            Change::Set(OPACITY, v) => brush.opacity = v,
            Change::Set(HARDNESS, v) => brush.hardness = v,
            Change::Set(SPACING, v) => brush.spacing = v,
            // Not a catch-all out of laziness: an id this panel did not put in its own
            // list is a bug in the renderer, and swallowing it would be exactly the kind
            // of silence §6b forbids.
            other => eprintln!("brush panel: unexpected {other:?}"),
        }
    }
    picked
}
