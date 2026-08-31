//! The pages of the document.
//!
//! One module per panel, exporting one function. See [`super`] for why.

use super::{Painting, Picked};
use crate::panel_ui::{Change, Control, ControlId};
use crate::ui::{PageAction, Status};
use crate::workspace::Place;
use openpaint_core::Brush;

// Rows are numbered by page index, so the ids above them start where no document could
// reach. A row and a button sharing an id would be a silent mis-hit.
const FIRST_COMMAND: ControlId = 1 << 20;
const ADD: ControlId = FIRST_COMMAND + 1;
const UP: ControlId = FIRST_COMMAND + 2;
const DOWN: ControlId = FIRST_COMMAND + 3;
const DELETE: ControlId = FIRST_COMMAND + 4;

/// What the panel shows for a document of `count` pages with `active` the one being drawn on.
///
/// Split out from [`show`] because everything interesting about this panel is which controls come
/// out of which document, and that is a question about two numbers rather than about a GPU.
#[must_use]
fn controls(count: usize, active: usize) -> Vec<Control> {
    let mut controls = vec![Control::Button {
        id: ADD,
        text: "Add page".to_owned(),
    }];
    for index in 0..count {
        let selected = index == active;
        controls.push(Control::Row {
            id: ControlId::try_from(index).unwrap_or(ControlId::MAX),
            // One-based, because a page is a thing the artist counts and nobody counts from zero.
            text: format!("Page {}", index + 1),
            selected,
            swatch: None,
            mark: None,
        });
        // The commands belong to the page they act on, so they sit under it rather than under the
        // list -- the same reason the eye lives on a layer's own row.
        if selected {
            // **Shown even when they cannot apply, and refused out loud on the way back.**
            // There is no disabled state in the vocabulary, so the choice is between omitting a
            // command and showing it inert, and omitting moves the others: on page 1 there is no
            // Up, so Down slides into where Up was and Delete into where Down was. Pressing the
            // same place on two different pages would then do two different things, which is the
            // failure the old Select section avoided by disabling rather than hiding. Layers makes
            // the same call for Delete, and this follows it.
            for (id, text) in [(UP, "Up"), (DOWN, "Down"), (DELETE, "Delete")] {
                controls.push(Control::Button {
                    id,
                    text: text.to_owned(),
                });
            }
        }
    }
    controls.push(Control::Label {
        text: "A webtoon is one very tall page, a sketchbook is many -- one model either way \
               (DECISIONS §5a). Deleting a page is undoable."
            .to_owned(),
    });
    controls
}

/// What a change on this panel asks the document to do, given how it stands.
///
/// A command that cannot apply answers `None` and says why, rather than quietly doing nothing
/// (§6b) -- the button is drawn in every case, so the refusal is the only thing left to tell the
/// artist that pressing it was not ignored by accident.
#[must_use]
fn picked(change: &Change, count: usize, active: usize) -> Option<Picked> {
    let page = |action| Some(Picked::Page(action));
    match *change {
        Change::Chose(index) => page(PageAction::Select(index as usize)),
        Change::Pressed(ADD) => page(PageAction::Add),
        Change::Pressed(UP) if active > 0 => page(PageAction::Move {
            from: active,
            to: active - 1,
        }),
        Change::Pressed(UP) => {
            eprintln!("pages: the first page has nothing to move above");
            None
        }
        Change::Pressed(DOWN) if active + 1 < count => page(PageAction::Move {
            from: active,
            to: active + 1,
        }),
        Change::Pressed(DOWN) => {
            eprintln!("pages: the last page has nothing to move below");
            None
        }
        // Deleting the last page would leave a document with nowhere to draw.
        Change::Pressed(DELETE) if count > 1 => page(PageAction::Delete(active)),
        Change::Pressed(DELETE) => {
            eprintln!("pages: a document needs at least one page");
            None
        }
        ref other => {
            eprintln!("pages panel: unexpected {other:?}");
            None
        }
    }
}

/// Draw the panel and report what the artist asked for.
pub(crate) fn show(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    color_srgb: &mut [u8; 3],
    state: &Status<'_>,
    paint: &mut Painting<'_>,
    place: Place,
) -> Option<Picked> {
    // Nothing here opens a list, so there is no popup half to draw and no settings to edit in
    // place: every answer this panel has is a change to the document.
    let _ = (&mut *brush, &mut *color_srgb, place);
    let (count, active) = state.pages;
    let controls = controls(count, active);
    let mut answer = None;
    for change in paint.show(ui, &controls) {
        answer = picked(&change, count, active);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row, in the order the artist reads them, one-based and with the active one marked.
    fn rows(controls: &[Control]) -> Vec<(ControlId, String, bool)> {
        controls
            .iter()
            .filter_map(|c| match c {
                Control::Row {
                    id, text, selected, ..
                } => Some((*id, text.clone(), *selected)),
                _ => None,
            })
            .collect()
    }

    fn buttons(controls: &[Control]) -> Vec<ControlId> {
        controls
            .iter()
            .filter_map(|c| match c {
                Control::Button { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// One row per page, ascending and one-based, with the active page selected.
    #[test]
    fn a_row_per_page_counted_from_one() {
        let controls = controls(3, 1);
        assert_eq!(
            rows(&controls),
            vec![
                (0, "Page 1".to_owned(), false),
                (1, "Page 2".to_owned(), true),
                (2, "Page 3".to_owned(), false),
            ]
        );
    }

    /// The row commands belong to the active page, so they follow it down the list.
    #[test]
    fn the_commands_sit_under_the_active_page() {
        for active in 0..3 {
            let controls = controls(3, active);
            let index = |id| {
                controls
                    .iter()
                    .position(|c| c.id() == Some(id))
                    .expect("the control should be there")
            };
            let row = index(ControlId::try_from(active).expect("small"));
            assert_eq!(index(UP), row + 1);
            assert_eq!(index(DOWN), row + 2);
            assert_eq!(index(DELETE), row + 3);
        }
    }

    /// **The commands keep their places whatever page is active.**
    ///
    /// This is the whole reason they are drawn when they cannot apply. Were they omitted, the
    /// second button under page 1 would be Delete and the second under page 2 would be Down, so
    /// pressing the same spot twice would do two different things.
    #[test]
    fn the_commands_do_not_move_when_they_cannot_apply() {
        // The ends of a three-page document, and the single page where none of the three applies.
        for (count, active) in [(3, 0), (3, 1), (3, 2), (1, 0)] {
            assert_eq!(
                buttons(&controls(count, active)),
                vec![ADD, UP, DOWN, DELETE],
                "{count} pages, page {active} active"
            );
        }
    }

    /// A document that has not loaded yet is still a panel: no rows, and a way to make one.
    #[test]
    fn a_document_with_no_pages_still_offers_add() {
        let controls = controls(0, 0);
        assert!(rows(&controls).is_empty());
        assert_eq!(buttons(&controls), vec![ADD]);
    }

    /// A press on a row goes to that page; the commands act on the active one.
    #[test]
    fn a_press_asks_for_what_it_says() {
        assert_eq!(
            picked(&Change::Chose(2), 3, 0),
            Some(Picked::Page(PageAction::Select(2)))
        );
        assert_eq!(
            picked(&Change::Pressed(ADD), 3, 0),
            Some(Picked::Page(PageAction::Add))
        );
        assert_eq!(
            picked(&Change::Pressed(UP), 3, 1),
            Some(Picked::Page(PageAction::Move { from: 1, to: 0 }))
        );
        assert_eq!(
            picked(&Change::Pressed(DOWN), 3, 1),
            Some(Picked::Page(PageAction::Move { from: 1, to: 2 }))
        );
        assert_eq!(
            picked(&Change::Pressed(DELETE), 3, 1),
            Some(Picked::Page(PageAction::Delete(1)))
        );
    }

    /// **A command that cannot apply does nothing at all**, rather than moving a page off the end
    /// of the list or leaving the document with nowhere to draw.
    #[test]
    fn a_command_at_the_end_of_its_range_is_refused() {
        assert_eq!(picked(&Change::Pressed(UP), 3, 0), None, "above the first");
        assert_eq!(picked(&Change::Pressed(DOWN), 3, 2), None, "below the last");
        assert_eq!(
            picked(&Change::Pressed(DELETE), 1, 0),
            None,
            "the last page"
        );
        // And on a one-page document none of the three has anywhere to go.
        for id in [UP, DOWN, DELETE] {
            assert_eq!(picked(&Change::Pressed(id), 1, 0), None);
        }
    }
}
