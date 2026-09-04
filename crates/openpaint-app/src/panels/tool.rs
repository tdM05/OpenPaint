//! The settings of whatever tool is in the artist's hand.
//!
//! One module per panel, exporting one function. See [`super`] for why.
//!
//! # What this panel is for
//!
//! One tool is in the hand at a time, so its settings belong in one place that never moves. That
//! is the whole of the idea, and every desktop application that draws has some version of it:
//! Photoshop's Options bar, Clip Studio's Tool Property, Krita's Tool Options docker. The value is
//! not screen space -- it is that the artist's eyes never leave the canvas to hunt for a setting.
//!
//! Brush and Select were tabs beside each other, so switching from painting to lassoing meant
//! knowing which of two tabs a setting lived behind before you could look for it. Now the panel
//! follows the tool: the rail decides, and this panel shows whatever that tool has to set.
//!
//! # And a transform borrows it
//!
//! A transform is not a tool. It is a *task in flight*: it exists only between Begin and Apply,
//! and it has a Cancel. As a peer tab it was empty for all but a few seconds of a session -- a tab
//! claiming to be one of four of a kind. So it takes this panel while it is in the air and gives
//! it back on Apply or Cancel, which is what "transient" means in `docs/CONTEXTUAL_PANELS.md`.
//!
//! **Nothing is hidden by that.** A transform can only be in flight because somebody started one,
//! and the two ways to start one -- Edit > Transform selection, and Ctrl+T -- are unchanged.
//!
//! # What does *not* happen here
//!
//! The panel does not resize itself when the context changes. Contents moving under you is the
//! standing complaint about contextual panels -- CSP has it, and it is the main irritation -- so
//! the panel keeps the size the layout gave it and the section scrolls inside it, exactly as the
//! brush section already had to.

use super::{Painting, Picked};
use crate::ui::Status;
use crate::workspace::Place;
use openpaint_core::Brush;

/// What the pen would do next, which is the whole of what this panel chooses between.
///
/// **Three, and in this order.** A transform in flight wins over the tool underneath it because it
/// is what the artist is doing right now and it is the thing with a Cancel; a selection tool wins
/// over the paint tool because it is what the pen is currently doing (`select_tool` being set is
/// exactly that fact). Named rather than decided at the call site, because which section a
/// document puts on screen is this panel's judgement and worth checking without a window -- and
/// because [`super::section_of`] has to reach the same answer to key the half-finished state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InHand {
    /// A transform is in the air, and has borrowed the panel until it is applied or cancelled.
    Transform,
    /// A selection tool is up: lasso, rectangle, wand or move.
    Selection,
    /// Neither, so the pen paints -- with the brush or with the eraser.
    Paint,
}

/// Which it is, for a given document.
#[must_use]
pub(crate) fn in_hand(state: &Status<'_>) -> InHand {
    if state.transform.is_some() {
        InHand::Transform
    } else if state.select_tool.is_some() {
        InHand::Selection
    } else {
        InHand::Paint
    }
}

/// Draw the panel and report what the artist asked for.
///
/// **Delegated whole, popup and all.** Each section owns its own dropdowns -- the brush's six
/// response sources, the transform's resampling filter -- and a contextual panel that drew half of
/// a section would be a second copy of it to keep in step. This is the cheap part the design
/// promised: a contextual panel is a match on `Status` and a call.
pub(crate) fn show(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    color_srgb: &mut [u8; 3],
    state: &Status<'_>,
    paint: &mut Painting<'_>,
    place: Place,
) -> Option<Picked> {
    match in_hand(state) {
        InHand::Transform => super::transform::show(ui, brush, color_srgb, state, paint, place),
        InHand::Selection => super::select::show(ui, brush, color_srgb, state, paint, place),
        InHand::Paint => super::brush::show(ui, brush, color_srgb, state, paint, place),
    }
}

#[cfg(test)]
mod tests {
    use super::{in_hand, InHand};
    use crate::ui::{SelectTool, Status, TransformState};
    use openpaint_core::{Kernel, Transform};

    fn flight() -> TransformState {
        TransformState {
            transform: Transform::IDENTITY,
            lock_aspect: false,
            kernel: Kernel::Mitchell,
        }
    }

    /// The panel follows the tool, and a transform in flight takes it from whatever was there.
    ///
    /// The sabotage for this is dropping the `transform` arm: the selection case then keeps the
    /// selection section while a box with handles sits on the canvas, and this test names it.
    #[test]
    fn the_section_follows_what_the_pen_is_doing() {
        let (layers, palette, presets, fonts) = crate::screenshot::sample_document();
        let status = || Status::sample(&layers, &palette, &presets, &fonts);

        let mut painting = status();
        painting.select_tool = None;
        painting.transform = None;
        assert_eq!(in_hand(&painting), InHand::Paint);

        let mut selecting = status();
        selecting.select_tool = Some(SelectTool::Wand);
        selecting.transform = None;
        assert_eq!(in_hand(&selecting), InHand::Selection);

        // **A transform wins over the tool underneath it**, and there is always a tool underneath
        // it: one is begun from a selection, which was made with a selection tool.
        let mut transforming = status();
        transforming.select_tool = Some(SelectTool::Lasso);
        transforming.transform = Some(flight());
        assert_eq!(in_hand(&transforming), InHand::Transform);

        let mut painting_transform = status();
        painting_transform.select_tool = None;
        painting_transform.transform = Some(flight());
        assert_eq!(in_hand(&painting_transform), InHand::Transform);
    }
}
