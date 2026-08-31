//! What has been done, and the way back through it.
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
    // A described placeholder rather than a drawn one, so the day this panel gets its
    // real list there is no egui left in it to remove first.
    let controls = [crate::panel_ui::Control::Label {
        text: "Undo history moves here once the old panel is ported.".to_owned(),
    }];
    for change in paint.show(ui, &controls) {
        eprintln!("history panel: unexpected {change:?}");
    }
    picked
}
