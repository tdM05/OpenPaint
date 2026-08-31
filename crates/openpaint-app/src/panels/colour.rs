//! The colour wheel, and the colours kept with the document.
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
    // **The first thing that genuinely cannot be described**, and so the first `Custom`.
    // A hue ring is not a list of controls, and pretending otherwise would have meant a
    // control kind that existed for one panel.
    //
    // What is described is everything around it: the wheel's shape, and the hex value.
    // The engine still decides where the wheel goes and how tall it is, so it stacks and
    // scrolls like anything else.
    use crate::colour_wheel::{Hsv, Shape};
    use crate::panel_ui::{Change, Control};
    const WHEEL: u32 = 0;
    const FIRST_SHAPE: u32 = 1;
    const SHAPES: [(&str, Shape); 3] = [
        ("Ring", Shape::Ring),
        ("Triangle", Shape::Triangle),
        ("Square", Shape::Square),
    ];

    let colour = Hsv::from_srgb8(*color_srgb);
    let mut controls = vec![Control::Custom {
        id: WHEEL,
        // Square, and generous: a wheel small enough to fit anywhere is one you cannot
        // pick a colour with.
        height: 190.0,
    }];
    controls.push(Control::Label {
        text: format!(
            "#{:02X}{:02X}{:02X}",
            color_srgb[0], color_srgb[1], color_srgb[2]
        ),
    });
    controls.push(Control::Separator);
    controls.extend(
        SHAPES
            .iter()
            .enumerate()
            .map(|(i, (name, shape))| Control::Choice {
                id: FIRST_SHAPE + u32::try_from(i).unwrap_or(0),
                text: (*name).to_owned(),
                selected: *paint.wheel_shape == *shape,
                icon: None,
            }),
    );

    for change in paint.show(ui, &controls) {
        match change {
            Change::Chose(id) if id >= FIRST_SHAPE => {
                if let Some((_, shape)) = SHAPES.get((id - FIRST_SHAPE) as usize) {
                    *paint.wheel_shape = *shape;
                }
            }
            other => eprintln!("colour panel: unexpected {other:?}"),
        }
    }

    // Drawn after the controls, into the rectangle the engine gave it.
    if let Some((_, at)) = paint
        .input
        .custom
        .iter()
        .find(|(id, _)| *id == WHEEL)
        .copied()
    {
        let where_it_is = crate::panel_draw::WheelAt {
            within: at,
            shape: *paint.wheel_shape,
            colour,
        };
        if let Some(picked) = crate::panel_draw::draw_wheel(
            ui.painter(),
            paint.theme,
            where_it_is,
            paint.input,
            paint.wheel_hold,
        ) {
            *color_srgb = picked.to_srgb8();
        }
    }
    picked
}
