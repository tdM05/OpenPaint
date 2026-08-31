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
    let _ = (&mut *brush, &mut *color_srgb);
    let (layers, active_layer) = (state.layers, state.active_layer);
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
    const BLEND: u32 = FIRST_COMMAND + 6;

    let active = layers.get(active_layer);
    let modes: Vec<String> = openpaint_core::Blend::ALL
        .iter()
        .map(|b| b.label().to_owned())
        .collect();

    // The dropdown's other half. Drawn when this panel's own popup is up, which is the only time
    // it can be: one popup at a time, and the workspace decides where it goes.
    if place == Place::Popup {
        let chosen = active.map_or(0, |l| {
            openpaint_core::Blend::ALL
                .iter()
                .position(|b| *b == l.blend)
                .unwrap_or(0)
        });
        if let Some(n) = super::pick_popup(BLEND, &modes, chosen, ui, paint) {
            if let Some(blend) = openpaint_core::Blend::ALL.get(n) {
                picked = Some(Picked::Layer(LayerAction::SetBlend {
                    index: active_layer,
                    blend: *blend,
                }));
            }
        }
        return picked;
    }

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
    controls.push(Control::Separator);
    // **How a layer combines with what is under it.** Three today and more later, which is why it
    // is a dropdown rather than three choices: a row of buttons stops being readable long before a
    // list does.
    controls.push(Control::Pick {
        id: BLEND,
        text: "Blend".to_owned(),
        value: active.map_or_else(String::new, |l| l.blend.label().to_owned()),
    });
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
            Change::Pressed(BLEND) => super::open_pick(BLEND, &modes, paint),
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
