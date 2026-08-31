//! The pages of the document.
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
    let _ = (&mut *brush, &mut *color_srgb, state, place);
    // Not yet written. Said out loud rather than left blank: an empty panel is
    // indistinguishable from one that failed to draw (DECISIONS 6b).
    paint.show(
        ui,
        &[crate::panel_ui::Control::Label {
            text: "Pages move here next.".to_owned(),
        }],
    );
    None
}
