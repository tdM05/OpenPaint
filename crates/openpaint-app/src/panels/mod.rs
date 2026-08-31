//! One module per panel: what it shows, and what a press on it means.
//!
//! **This is the descriptor design showing up in the file layout.** It was already in the types --
//! a panel says what its controls *are* and applies what comes back, and nothing in it knows what a
//! slider looks like. But every panel lived in one `match` in one 2,900-line file, so two people
//! could not work on two panels without colliding, and the shape was visible only to whoever had
//! read the whole thing.
//!
//! Each module exports one function with one signature. `ui.rs` keeps a line each.
//!
//! # What a panel gets, and what it may do with it
//!
//! - [`Status`] is the document as it stands, read-only. Everything a panel could want to show.
//! - `brush` and `color_srgb` are the two things a panel may change **in place**, because they are
//!   settings rather than document state and there is nothing to undo.
//! - Everything else is asked for by returning a [`Picked`], which the shell applies. A panel that
//!   edited the document directly would be a panel that could not be undone.
//! - [`Painting`] is how it draws: `paint.show(ui, &controls)` and nothing else.
//!
//! # Ids
//!
//! Control ids are per module and shadowed inside it, so two panels cannot collide. Where a panel
//! numbers rows by index -- layers, pages, presets -- its command ids start from a base above any
//! index a document could reach; see the `FIRST_*` constants for the pattern.

use crate::layout::PanelId;
use crate::ui::Status;
use crate::workspace::{self as ws, Place};
use openpaint_core::Brush;

pub(crate) mod brush;
pub(crate) mod colour;
pub(crate) mod history;
pub(crate) mod layers;
pub(crate) mod menu;
pub(crate) mod tools;

pub(crate) use crate::ui::{Painting, Picked};

/// Draw whichever panel this is.
///
/// The whole of what `ui.rs` knows about panel contents. A panel with nothing to draw -- the canvas
/// -- never reaches here: the workspace skips it, because its pixels come from the GPU underneath
/// everything egui does.
pub(crate) fn show(
    panel: PanelId,
    ui: &mut egui::Ui,
    brush: &mut Brush,
    color_srgb: &mut [u8; 3],
    state: &Status<'_>,
    paint: &mut Painting<'_>,
    place: Place,
) -> Option<Picked> {
    match panel {
        ws::MENU => menu::show(ui, brush, color_srgb, state, paint, place),
        ws::TOOLS => tools::show(ui, brush, color_srgb, state, paint, place),
        ws::BRUSH => brush::show(ui, brush, color_srgb, state, paint, place),
        ws::LAYERS => layers::show(ui, brush, color_srgb, state, paint, place),
        ws::COLOUR => colour::show(ui, brush, color_srgb, state, paint, place),
        ws::HISTORY => history::show(ui, brush, color_srgb, state, paint, place),
        // A panel the build knows about but nothing has been written for yet. Silent on purpose:
        // this is the one case that is not a bug, and it goes away as the last module lands.
        _ => None,
    }
}
