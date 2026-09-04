//! What the active layer is made of.
//!
//! One module per panel, exporting one function. See [`super`] for why.
//!
//! # The one rule
//!
//! **This panel shows what the active layer is made of, and nothing else.** It is written down
//! because Photoshop's equivalent has no such rule and has become a grab-bag -- layer properties,
//! shape properties and adjustment properties, with nothing saying what may appear. A rule nobody
//! can state is a rule nobody can apply, and the next contextual thing always looks like it
//! belongs here.
//!
//! So a *command* does not live here even when it is about the active layer: adding a text layer
//! and converting one back to pixels are in the Layer menu, beside Add, Duplicate and Merge down,
//! because they make and unmake layers rather than describe one. What is left is the layer's own
//! contents, which today means the caption of a text layer.
//!
//! # Why it is contextual rather than a tab
//!
//! Text was a tab in a strip beside Brush, Select and Transform, as though the four were four of a
//! kind. They are three different kinds of context: Brush and Select are *tool options*, Text is a
//! *layer property*, and Transform is a *task in flight*. `docs/CONTEXTUAL_PANELS.md` has the
//! reasoning; the consequence is this panel, which follows the active layer, and [`super::tool`],
//! which follows the tool in hand.

use super::{Painting, Picked};
use crate::panel_ui::Control;
use crate::ui::Status;
use crate::workspace::Place;
use openpaint_core::{Brush, Layer};

/// What the active layer is made of, which is the whole of what this panel chooses between.
///
/// **A named answer rather than an `Option<&TextBlock>` at the call site**, because the choice is
/// this panel's judgement and the thing worth testing without a window: which section a document
/// puts on screen. It is also what keys the half-finished state (see [`super::section_of`]), so
/// there is one place that decides and two that read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MadeOf {
    /// A text layer: the words *are* the layer, and [`super::text`] edits them.
    Words,
    /// A raster layer: pixels, which have nothing of their own to set yet.
    Pixels,
}

/// Which it is, for a given document.
#[must_use]
pub(crate) fn made_of(state: &Status<'_>) -> MadeOf {
    match state.layers.get(state.active_layer).and_then(Layer::text) {
        Some(_) => MadeOf::Words,
        None => MadeOf::Pixels,
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
    match made_of(state) {
        // Delegated whole, popup and all: the text section owns its font list, and a contextual
        // panel that redrew half of a section would be a second copy of it to keep in step.
        MadeOf::Words => super::text::show(ui, brush, color_srgb, state, paint, place),
        MadeOf::Pixels => {
            // Nothing here opens a list, so there is no popup half to draw.
            if place == Place::Popup {
                return None;
            }
            for change in paint.show(ui, &nothing_to_set()) {
                // Nothing in `nothing_to_set` can be pressed, so anything arriving here came from
                // an id this panel never put on screen. Said rather than swallowed (DECISIONS 6b).
                eprintln!("properties panel: unexpected {change:?}");
            }
            None
        }
    }
}

/// What the panel says when the active layer has nothing to set.
///
/// **An empty contextual panel that says what would fill it is better than a tab, not worse.** The
/// standing complaint about contextual panels is discoverability: a Text tab advertises that text
/// layers exist and a panel that goes blank does not. So the blank state carries the advertisement
/// -- what kind of layer would put something here, and where the command that makes one lives.
/// This is the same habit as *"Rect has nothing to set."* and *"Select something first to
/// transform it."*, and it is what DECISIONS 6b asks of every context with nothing to show.
#[must_use]
pub(crate) fn nothing_to_set() -> Vec<Control> {
    vec![
        Control::Label {
            text: "This layer is pixels, and pixels have nothing to set here.".to_owned(),
        },
        Control::Label {
            text: "A text layer keeps its words instead of its pixels, and shows them here to \
                   retype. Layer \u{25b8} Add text layer makes one."
                .to_owned(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::{made_of, nothing_to_set, MadeOf};

    /// The panel follows the active *layer*, and nothing else about the document.
    ///
    /// The sabotage for this is `made_of` answering `Words` unconditionally: the raster case then
    /// asks the text section for a block it has not got, and this test names which half broke.
    #[test]
    fn the_section_follows_the_active_layer() {
        let (layers, palette, presets, fonts) = crate::screenshot::sample_document();
        let text_at = layers
            .iter()
            .position(|l| l.text().is_some())
            .expect("the sample document has a text layer");
        let raster_at = layers
            .iter()
            .position(|l| l.text().is_none())
            .expect("the sample document has a raster layer");

        let at = |index: usize| {
            let mut status = crate::ui::Status::sample(&layers, &palette, &presets, &fonts);
            status.active_layer = index;
            made_of(&status)
        };
        assert_eq!(at(text_at), MadeOf::Words);
        assert_eq!(at(raster_at), MadeOf::Pixels);
        // Past the end is a document that has been edited under us, and the honest answer is the
        // one with nothing in it rather than a panic.
        assert_eq!(at(layers.len()), MadeOf::Pixels);
    }

    /// **An empty context says what would fill it.** The whole discoverability argument for
    /// contextual panels rests on this: a blank panel is worse than the tab it replaced unless it
    /// names the thing that is missing and where to get it.
    #[test]
    fn nothing_to_set_says_what_would_fill_it() {
        let said = nothing_to_set()
            .iter()
            .filter_map(|c| match c {
                crate::panel_ui::Control::Label { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!said.is_empty(), "an empty panel that says nothing at all");
        // **What this layer is, as well as what would fill the panel.** Both sentences are load
        // bearing and each was asserted only by the other for a while: emptying the first one left
        // the panel saying what a text layer would show without ever saying what this layer *is*,
        // and the sabotage sweep walked straight past it.
        assert!(
            said.contains("nothing to set"),
            "it does not say that this layer has nothing to set: {said}"
        );
        assert!(
            said.contains("text layer"),
            "it does not name the kind of layer that would fill it: {said}"
        );
        assert!(
            said.contains("Add text layer"),
            "it does not say where the command that makes one lives: {said}"
        );
    }
}
