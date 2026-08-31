//! What has been done, and the way back through it.
//!
//! One module per panel, exporting one function. See [`super`] for why.
//!
//! # Why there is no list of entries here
//!
//! Every paint application shows history as a list you can click into, and that is what an artist
//! reaching for this panel expects. It is not buildable today, and the reasons are in the engine
//! rather than in this file:
//!
//! - [`crate::history::Op`] carries no name and no way to make one. A `Paint` holds a layer id, a
//!   before-image and its dabs; there is nothing in it that says "Brush" rather than "Erase"
//!   without inventing a label at the call site, and a list of twenty rows all reading "Paint"
//!   tells you less than the depth already does.
//! - [`crate::ui::Status`] hands this panel `history: (usize, usize, u64)` and nothing else. The
//!   ops never leave [`crate::history::History`], so a list has no rows to be made of.
//! - The stacks are strict LIFO by design -- `pop_undo` and `pop_redo` and no index -- and
//!   [`crate::ui::Command`] offers `Undo` and `Redo` and no "go to step 7". Walking to an
//!   arbitrary point would be N undos, which nothing here can ask for in one answer.
//!
//! So this shows the depth honestly and can act on it, and does not draw rows that would be a
//! picture of a list rather than a list. **A convincing list that does not work is worse than an
//! honest readout**: it invites a click that either does nothing or, worse, walks somewhere the
//! artist did not point at.

use super::{Painting, Picked};
use crate::panel_ui::{Change, Control};
use crate::ui::{Command, Status};
use crate::workspace::Place;
use openpaint_core::Brush;

// No rows in this panel, so no index a document could grow into and collide with -- see [`super`]
// for the `FIRST_*` pattern the list panels need and this one does not.
const UNDO: u32 = 1;
const REDO: u32 = 2;

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
    let _ = (&mut *brush, &mut *color_srgb, place);
    let controls = controls(state.history);
    for change in paint.show(ui, &controls) {
        picked = answer(&change);
    }
    picked
}

/// What the panel shows for a given history state.
///
/// Split out because it is the whole of what this panel decides, and deciding it against a
/// rectangle rather than a GPU is what makes it testable -- the same split the rest of the
/// described panels get for free.
fn controls(history: (usize, usize, u64)) -> Vec<Control> {
    let (undo_depth, redo_depth, bytes) = history;
    let mut controls = vec![Control::Label {
        text: format!(
            "Undo {undo_depth}   Redo {redo_depth}   ({:.1} MiB)",
            bytes as f32 / (1024.0 * 1024.0)
        ),
    }];

    // **Shown only when there is something to walk back to.** A panel never offers what it would
    // refuse: a button that does nothing when pressed teaches you not to trust the panel, and here
    // it would be indistinguishable from undo being broken. The depth beside it already says why
    // the button is absent, which is more than a greyed-out button says.
    if undo_depth > 0 || redo_depth > 0 {
        controls.push(Control::Separator);
    }
    if undo_depth > 0 {
        controls.push(Control::Button {
            id: UNDO,
            text: "Undo".to_owned(),
        });
    }
    if redo_depth > 0 {
        controls.push(Control::Button {
            id: REDO,
            text: "Redo".to_owned(),
        });
    }

    controls.push(Control::Separator);
    // Both shortcuts, because the panel that shows the depth is where someone looks when they are
    // not sure undo is working -- and the second sentence is why the MiB figure is small.
    controls.push(Control::Label {
        text: "Ctrl+Z undoes, Ctrl+Shift+Z or Ctrl+Y redoes. Snapshots cover only the tiles a \
               stroke touched."
            .to_owned(),
    });
    controls
}

/// What a change to this panel means.
///
/// Undo and redo already have shortcuts, and the buttons go to exactly the same place: a panel
/// that showed the depth and could not act on it would be a panel that sends you to the keyboard.
fn answer(change: &Change) -> Option<Picked> {
    match change {
        Change::Pressed(UNDO) => Some(Picked::Command(Command::Undo)),
        Change::Pressed(REDO) => Some(Picked::Command(Command::Redo)),
        other => {
            eprintln!("history panel: unexpected {other:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buttons(history: (usize, usize, u64)) -> Vec<String> {
        controls(history)
            .into_iter()
            .filter_map(|c| match c {
                Control::Button { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    fn labels(history: (usize, usize, u64)) -> Vec<String> {
        controls(history)
            .into_iter()
            .filter_map(|c| match c {
                Control::Label { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    /// **A panel never offers what it would refuse.** With nothing recorded there is nothing to
    /// walk back to, and a button that does nothing reads as undo being broken rather than as
    /// there being no undo.
    #[test]
    fn a_fresh_document_offers_neither_button() {
        assert!(buttons((0, 0, 0)).is_empty());
    }

    /// One stroke in: undo is reachable, redo is not -- there is no future to return to until
    /// something has been undone.
    #[test]
    fn something_to_undo_offers_undo_alone() {
        assert_eq!(buttons((3, 0, 0)), vec!["Undo".to_owned()]);
    }

    /// Undone but not re-edited: both directions are live.
    #[test]
    fn both_stacks_offer_both_buttons() {
        assert_eq!(
            buttons((2, 1, 0)),
            vec!["Undo".to_owned(), "Redo".to_owned()]
        );
    }

    /// And redo alone, which is the state after undoing everything -- the case a `> 0` written
    /// once and reused for both would quietly get wrong.
    #[test]
    fn everything_undone_offers_redo_alone() {
        assert_eq!(buttons((0, 4, 0)), vec!["Redo".to_owned()]);
    }

    /// The readout is the point of the panel: both depths and what they cost, in the units the
    /// budget is written in.
    #[test]
    fn the_readout_names_both_depths_and_the_memory() {
        let readout = labels((2, 5, 3 * 1024 * 1024 + 512 * 1024))
            .into_iter()
            .next()
            .expect("a readout");
        assert_eq!(readout, "Undo 2   Redo 5   (3.5 MiB)");
    }

    /// The hint survives every state, including the one where there are no buttons above it --
    /// which is exactly when someone is wondering whether undo works at all.
    #[test]
    fn the_hint_is_always_there() {
        for history in [(0, 0, 0), (1, 0, 0), (0, 1, 0), (4, 4, 1 << 20)] {
            let hint = labels(history).pop().expect("a hint");
            assert!(
                hint.contains("Ctrl+Z") && hint.contains("Ctrl+Y") && hint.contains("tiles"),
                "the hint went missing at {history:?}: {hint}"
            );
        }
    }

    /// A press goes to the command the shortcut goes to. Undo and redo are the application's to
    /// perform -- they touch GPU tiles and the history stacks, neither of which a panel may reach.
    #[test]
    fn the_buttons_ask_for_the_commands_the_shortcuts_do() {
        assert_eq!(
            answer(&Change::Pressed(UNDO)),
            Some(Picked::Command(Command::Undo))
        );
        assert_eq!(
            answer(&Change::Pressed(REDO)),
            Some(Picked::Command(Command::Redo))
        );
    }

    /// Undo and redo are different ids and different answers. Both mapping to the same command
    /// would be a panel that walks one way whichever button you press, and nothing else here
    /// would catch it.
    #[test]
    fn undo_and_redo_are_not_the_same_control() {
        assert_ne!(UNDO, REDO);
        assert_ne!(
            answer(&Change::Pressed(UNDO)),
            answer(&Change::Pressed(REDO))
        );
    }

    /// Anything else is not this panel's, and is reported rather than guessed at.
    #[test]
    fn an_unknown_change_asks_for_nothing() {
        assert_eq!(answer(&Change::Pressed(999)), None);
        assert_eq!(answer(&Change::Toggled(UNDO, true)), None);
    }
}
