//! The tool rail.
//!
//! One module per panel, exporting one function. See [`super`] for why.

use super::{Painting, Picked};
use crate::editor::Tool;
use crate::ui::SelectTool;
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
    // A wrapped grid, so the rail works whether it is a column down the side or a strip
    // along the bottom. Nothing here knows which it currently is: `Direction::Wrap` is the
    // panel's default in the table, and wrapping is honest here because every button is
    // the same width.
    //
    // Painting tools and selection tools are one set on purpose: to the artist they are
    // all "what the pen does next", and the fact that one lives on `Editor` and the other
    // on `Select` is our bookkeeping, not theirs.
    //
    // Named rather than drawn with glyphs, for now. The glyphs needed a tooltip to be
    // readable at all, and a tooltip is something only a pointer that hovers can reach --
    // which is the thing this UI is explicitly not built around (§1b). Icons replace the
    // words when there are icons worth using.
    use crate::icons::Symbol;
    use crate::panel_ui::{Change, Control};
    let items: [(&str, Symbol, Picked); 6] = [
        ("Brush", Symbol::Brush, Picked::Paint(Tool::Brush)),
        ("Eraser", Symbol::Eraser, Picked::Paint(Tool::Eraser)),
        ("Lasso", Symbol::Lasso, Picked::Select(SelectTool::Lasso)),
        ("Rect", Symbol::RectSelect, Picked::Select(SelectTool::Rect)),
        ("Wand", Symbol::Wand, Picked::Select(SelectTool::Wand)),
        (
            "Move",
            Symbol::MoveSelection,
            Picked::Select(SelectTool::Move),
        ),
    ];
    let controls: Vec<Control> = items
        .iter()
        .enumerate()
        .map(|(i, (name, symbol, what))| Control::Choice {
            id: u32::try_from(i).unwrap_or(u32::MAX),
            text: (*name).to_owned(),
            // The word and the picture both, so the icon set decides which is shown and
            // the rail needs no second table when somebody chooses Words.
            icon: Some(*symbol),
            selected: match what {
                // A paint tool reads as chosen only when no selection tool is up, since a
                // selection tool is what the pen is currently doing.
                Picked::Paint(t) => select_tool.is_none() && tool == *t,
                Picked::Select(t) => select_tool == Some(*t),
                Picked::Layer(_)
                | Picked::PanelList
                | Picked::Selection(_)
                | Picked::Command(_)
                | Picked::OpenMenu { .. }
                | Picked::CloseMenu
                | Picked::Settings => false,
            },
        })
        .collect();

    for change in paint.show(ui, &controls) {
        picked = match change {
            Change::Chose(i) => items.get(i as usize).map(|(_, _, what)| what.clone()),
            other => {
                eprintln!("tools panel: unexpected {other:?}");
                None
            }
        };
    }
    picked
}
