//! The layer stack.
//!
//! One module per panel, exporting one function. See [`super`] for why.

use super::{Painting, Picked};
use crate::ui::LayerAction;
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
    let mut picked: Option<Picked> = None;
    let _ = (&mut *brush, &mut *color_srgb, state, place);
    let layers = state.layers;
    let active_layer = state.active_layer;
    let tool = state.tool;
    let select_tool = state.select_tool;
    let _ = (layers, active_layer, tool, select_tool);
    // The second panel described rather than drawn, and the one that exercises the rest of
    // the vocabulary: a list, a pair of switches, and commands.
    use crate::panel_ui::{Change, Control};
    // Rows are numbered by layer index, so the ids above them start where no document
    // could reach. A row and a button sharing an id would be a silent mis-hit.
    const FIRST_VISIBILITY: u32 = 1 << 19;
    const FIRST_COMMAND: u32 = 1 << 20;
    const LOCK_ALPHA: u32 = FIRST_COMMAND + 1;
    const ADD: u32 = FIRST_COMMAND + 2;
    const DUPLICATE: u32 = FIRST_COMMAND + 3;
    const MERGE_DOWN: u32 = FIRST_COMMAND + 4;
    const DELETE: u32 = FIRST_COMMAND + 5;

    let mut controls = Vec::new();
    // Top of the list is the top of the stack, which is how every layers panel reads and
    // the opposite of how the document stores it.
    for (index, layer) in layers.iter().enumerate().rev() {
        let row = u32::try_from(index).unwrap_or(u32::MAX);
        controls.push(Control::Row {
            id: row,
            text: layer.name.clone(),
            selected: index == active_layer,
            // The eye, on the row it belongs to. Hiding a layer used to mean selecting it
            // first and then finding a switch below the list, which is two steps for
            // something every paint application does in one.
            mark: Some(crate::panel_ui::RowMark {
                id: FIRST_VISIBILITY + row,
                on: layer.visible,
            }),
            swatch: None,
        });
    }
    let active = layers.get(active_layer);
    controls.push(Control::Separator);
    controls.push(Control::Toggle {
        id: LOCK_ALPHA,
        text: "Lock alpha".to_owned(),
        on: active.is_some_and(|l| l.lock_alpha),
    });
    controls.push(Control::Separator);
    for (id, text) in [
        (ADD, "Add"),
        (DUPLICATE, "Duplicate"),
        (MERGE_DOWN, "Merge down"),
        (DELETE, "Delete"),
    ] {
        controls.push(Control::Button {
            id,
            text: text.to_owned(),
        });
    }

    for change in paint.show(ui, &controls) {
        picked = match change {
            Change::Chose(index) => Some(Picked::Layer(LayerAction::Select(index as usize))),
            Change::Toggled(id, visible) if (FIRST_VISIBILITY..FIRST_COMMAND).contains(&id) => {
                Some(Picked::Layer(LayerAction::SetVisible {
                    index: (id - FIRST_VISIBILITY) as usize,
                    visible,
                }))
            }
            Change::Toggled(LOCK_ALPHA, lock) => Some(Picked::Layer(LayerAction::SetLockAlpha {
                index: active_layer,
                lock,
            })),
            Change::Pressed(ADD) => Some(Picked::Layer(LayerAction::Add)),
            Change::Pressed(DUPLICATE) => Some(Picked::Layer(LayerAction::Duplicate(active_layer))),
            Change::Pressed(MERGE_DOWN) => {
                Some(Picked::Layer(LayerAction::MergeDown(active_layer)))
            }
            // Deleting the last layer would leave a document with nothing to paint on, so
            // the command is refused out loud rather than quietly doing nothing (§6b).
            Change::Pressed(DELETE) if layers.len() > 1 => {
                Some(Picked::Layer(LayerAction::Delete(active_layer)))
            }
            Change::Pressed(DELETE) => {
                eprintln!("layers: a document needs at least one layer");
                None
            }
            other => {
                eprintln!("layers panel: unexpected {other:?}");
                None
            }
        };
    }
    picked
}
