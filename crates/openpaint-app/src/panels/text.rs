//! Text layers: the words, the font, and how they are set.
//!
//! One module per panel, exporting one function. See [`super`] for why.
//!
//! # What is drawn
//!
//! The caption editor: the words, the family, the size, the weight and slant, wrapping and its
//! width, alignment, colour, spacing and position -- along with the substitution warning, and two
//! commands, converting the layer to raster and loading a font file.
//!
//! **This is a section, not only a panel.** It is what the Properties panel shows when the active
//! layer keeps words rather than pixels (see [`super::properties`]), and it is still a panel of its
//! own for anyone who wants it docked somewhere permanently.
//!
//! *Adding* a text layer is not here. It is in the Layer menu, beside Add, Duplicate and Merge
//! down, because it makes a layer rather than describing one -- and because the one place a person
//! who has never made a text layer will not look for the command is inside the panel that only has
//! anything to show once they have.
//!
//! The caption editor is written and proved in [`editor`]. A panel may not edit the document in
//! place (see [`super`]), so it edits a copy of the block and hands it back as `Picked::TextSet`;
//! the shell writes it through, records the undo step and derives the pixels again. That is what
//! keeps the undo record where it belongs, and it is why the editor is pure enough to prove
//! without a GPU, a font stack or a document.

use super::{Painting, Picked};
use crate::panel_ui::{Change, Control};
use crate::ui::{Status, TextAction};
use crate::workspace::Place;
use openpaint_core::{Brush, Layer};

// **One numbering for the whole panel.** The editor's controls and the commands under them end up
// in a single list, so they are numbered together here rather than twice in two places -- two
// controls sharing an id is a silent mis-hit, and it is exactly the kind of thing that only shows
// up once both halves are on screen.
const BODY: u32 = 0;
const FONT: u32 = 1;
const BOLD: u32 = 2;
const ITALIC: u32 = 3;
const SIZE: u32 = 4;
const LINE_HEIGHT: u32 = 5;
const LETTER_SPACING: u32 = 6;
const COLOUR: u32 = 7;
const WRAP: u32 = 8;
const WRAP_WIDTH: u32 = 9;
const POS_X: u32 = 10;
const POS_Y: u32 = 11;
/// Alignments are numbered by their index in `Align::ALL`, so the base sits clear of every id
/// above it -- and clear of a fourth alignment, if one is ever added, reaching the ids below.
const FIRST_ALIGN: u32 = 1 << 8;
/// The commands under the editor, numbered clear of the fields above them.
const FIRST_COMMAND: u32 = 1 << 9;
const CONVERT: u32 = FIRST_COMMAND;
const LOAD_FONT: u32 = FIRST_COMMAND + 1;

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

    // The document's own block, read-only -- which is all `Status` offers. `text()` answers `None`
    // for a raster layer, which is the same question the old panel asked and the reason it had two
    // branches.
    let block = state.layers.get(state.active_layer).and_then(Layer::text);

    // The font list, drawn when this panel's own popup is up -- the same shape the layers panel
    // writes for its blend dropdown. Returning early rather than falling through: drawing the
    // panel's ordinary contents into the popup would put a second copy of the buttons on screen.
    if place == Place::Popup {
        let block = block?;
        let options = editor::font_options(state.font_families);
        let chosen = editor::font_chosen(block, state.font_families).unwrap_or(0);
        if let Some(n) = super::pick_popup(FONT, &options, chosen, ui, paint) {
            let mut edited = block.clone();
            if editor::set_family(&mut edited, state.font_families, n) == editor::Applied::Edited {
                return Some(Picked::TextSet(edited));
            }
        }
        return None;
    }

    // **The substitution warning is `controls_for`'s, and only its.** It was pushed here as well,
    // so a page lettered in a face nobody asked for said so twice, one line under the other. It is
    // a statement about a particular block's requested family, and `controls_for` is the thing
    // that has the block -- there is nothing for it to be about when no text layer is active.
    let mut controls = Vec::new();

    if let Some(block) = block {
        controls.extend(editor::controls_for(block, state.font_substituted));
    } else {
        controls.push(Control::Label {
            text: "The active layer is not a text layer. A text layer keeps the words rather \
                   than the pixels, so a caption stays retypeable -- and cannot be painted on."
                .to_owned(),
        });
        // **And where to get one.** A panel with nothing to show has to say what would give it
        // something (DECISIONS 6b); this one used to carry the command itself, and now that the
        // command lives with the other layer-making commands it carries the direction instead.
        controls.push(Control::Label {
            text: "Layer \u{25b8} Add text layer makes one.".to_owned(),
        });
    }

    controls.push(Control::Separator);

    // Offered only when there is text to convert, matching the old panel: a command that cannot
    // apply should not be on screen at all, rather than on screen and refusing.
    if block.is_some() {
        controls.push(Control::Button {
            id: CONVERT,
            text: "Convert to raster layer".to_owned(),
        });
        // **This was a tooltip, and it is now a line of text.** A `Control` carries no hover
        // text, and this particular warning is the only thing between the artist and losing a
        // caption they can no longer retype, so it is kept and promoted rather than dropped.
        controls.push(Control::Label {
            text: "One way: the pixels stay and the words stop existing. Undo brings them back; \
                   retyping does not."
                .to_owned(),
        });
    }

    controls.push(Control::Button {
        id: LOAD_FONT,
        text: "Load font file\u{2026}".to_owned(),
    });
    controls.push(Control::Label {
        text: "A .ttf or .otf, used for this session without installing it.".to_owned(),
    });

    // The block as it will be after this frame's changes, so several edits in one frame -- which
    // a slider and the field losing its caret can be -- go back as one write and one undo step.
    let mut edited = block.cloned();
    for change in paint.show(ui, &controls) {
        let answer = match change {
            Change::Pressed(CONVERT) => Some(Picked::Text(TextAction::ConvertToRaster)),
            Change::Pressed(LOAD_FONT) => Some(Picked::Text(TextAction::LoadFontFile)),
            other => match edited.as_mut() {
                // Everything else belongs to the caption, and the editor says what it meant.
                Some(block) => match editor::apply(block, other) {
                    editor::Applied::Edited | editor::Applied::Nothing => None,
                    editor::Applied::OpenFont => {
                        super::open_pick(FONT, &editor::font_options(state.font_families), paint)
                    }
                    // Shown rather than swallowed: a field that springs back with no word said is
                    // a field that looks broken (DECISIONS 6b).
                    editor::Applied::Rejected(why) => {
                        eprintln!("text panel: {why}");
                        None
                    }
                    editor::Applied::Unexpected(change) => {
                        eprintln!("text panel: unexpected {change:?}");
                        None
                    }
                },
                // Not a catch-all out of laziness: with no text layer under it, an id this panel
                // did not put in its own list is a bug in the renderer, and swallowing it is the
                // silence DECISIONS 6b forbids.
                None => {
                    eprintln!("text panel: unexpected {other:?}");
                    None
                }
            },
        };
        if answer.is_some() {
            picked = answer;
        }
    }
    // One write for the frame, and only when the words actually differ: every edit here costs a
    // re-render and a step on the undo stack.
    picked.or_else(|| written_back(edited, block))
}

/// What the frame's edits amount to: a caption to write, or nothing.
///
/// **One write per frame, and only when the words differ.** Several changes can arrive together --
/// a slider moving while a field gives up the caret -- and each one that got its own write would
/// be its own re-render and its own step on the undo stack. A change that ended where it started
/// is not a change at all.
///
/// Its own function because it is the whole of what `show` decides, and `show` needs a window.
#[must_use]
fn written_back(
    edited: Option<openpaint_core::TextBlock>,
    was: Option<&openpaint_core::TextBlock>,
) -> Option<Picked> {
    match (edited, was) {
        (Some(now), Some(was)) if now != *was => Some(Picked::TextSet(now)),
        _ => None,
    }
}

/// What this panel draws for a layer, so a test can find a control without guessing where it is.
#[cfg(test)]
#[must_use]
pub(crate) fn controls_for_test(layer: &openpaint_core::Layer) -> Vec<Control> {
    layer
        .text()
        .map(|b| editor::controls_for(b, None))
        .unwrap_or_default()
}

/// The caption editor: the controls for one [`TextBlock`], and what a change to one means.
///
/// **Pure, and provable without a GPU, a font stack or a document** -- the same split that makes
/// `panel_ui` and `crop` provable. [`show`] reads the block off the active layer, folds [`apply`]
/// over the frame's changes into a copy, and hands the copy back as `Picked::TextSet` for the
/// shell to write through the `&mut TextBlock` it already holds.
///
/// **The panel never edits the document.** It edits a copy and asks. That is what keeps the undo
/// record where it belongs: `apply_text_edit` takes the block back, records the step and derives
/// the pixels again, exactly as it did for the old side panel.
mod editor {
    use super::{
        BODY, BOLD, COLOUR, FIRST_ALIGN, FONT, ITALIC, LETTER_SPACING, LINE_HEIGHT, POS_X, POS_Y,
        SIZE, WRAP, WRAP_WIDTH,
    };
    use crate::panel_ui::{Change, Control};
    use openpaint_core::text::{Align, TextBlock};

    /// What the font pick calls "no family in particular".
    ///
    /// An empty family is a *request* for whatever this platform's default sans is, not a missing
    /// value, so it needs a name in the list rather than a blank row.
    pub(super) const DEFAULT_FAMILY: &str = "(default)";

    /// The weight at and above which the Bold switch reads as on.
    ///
    /// **Bold is a threshold on a continuous axis, not a flag.** `FontSpec::weight` is a number
    /// because variable fonts have a weight axis; 600 is where CSS puts semibold, and a face at
    /// 600 that the switch called "not bold" would be a switch disagreeing with the page.
    const BOLD_AT: u16 = 600;
    const BOLD_WEIGHT: u16 = 700;
    const REGULAR_WEIGHT: u16 = 400;

    /// The width a block gets the moment wrapping is turned on.
    ///
    /// It has to be *some* width, and a block that wrapped at zero would collapse to one glyph per
    /// line, which reads as the toggle having broken the caption.
    const DEFAULT_WRAP_WIDTH: f32 = 400.0;

    /// What applying a change to a block did.
    ///
    /// Four answers rather than a `bool`, because the caller has to do something different with
    /// each: re-derive the pixels, open a list, say nothing, or complain. A `bool` would fold the
    /// last two together, and "nothing happened" and "that was not a colour" are not the same
    /// thing to anyone watching the panel.
    #[derive(Clone, Debug, PartialEq)]
    pub(super) enum Applied {
        /// The block changed, and its pixels are now stale.
        Edited,
        /// Understood, and nothing to write -- a field taking the caret, or a value set to what it
        /// already was.
        Nothing,
        /// The font list was asked for. Not an edit: the panel opens it and asks again later.
        OpenFont,
        /// What was typed is not a value this field can hold. Carries a sentence to show, because
        /// a field that silently springs back is a field that looks broken.
        Rejected(&'static str),
        /// Not this panel's, or not possible in the state the panel is in. Handed back whole so
        /// the caller can name it.
        Unexpected(Change),
    }

    /// Assign, and say whether it actually changed anything.
    ///
    /// The comparison is not an optimisation. Every edit here invalidates the layer's pixels and
    /// puts a step on the undo stack, so reporting a change for a slider dragged back to where it
    /// started would cost a re-render and an undo step for nothing.
    fn set<T: PartialEq>(field: &mut T, value: T) -> Applied {
        if *field == value {
            Applied::Nothing
        } else {
            *field = value;
            Applied::Edited
        }
    }

    /// The family this block asks for, or `None` when it asks for nothing in particular.
    pub(super) fn requested_family(block: &TextBlock) -> Option<&str> {
        (!block.font.family.is_empty()).then_some(block.font.family.as_str())
    }

    /// Whether the Bold switch reads as on for this weight.
    fn is_bold(weight: u16) -> bool {
        weight >= BOLD_AT
    }

    /// The warning shown when the page is being lettered in a face it was not written in.
    ///
    /// Two wordings, one function. The second exists because the panel can be a frame ahead of the
    /// document -- the layer changed under it -- and naming a family it does not have would mean
    /// either a lie or a blank pair of quotes. It goes away when the block is always to hand.
    pub(super) fn substitution_warning(requested: Option<&str>, actual: &str) -> String {
        match requested {
            Some(family) => format!("\u{26a0} {family:?} is not installed. Showing {actual:?}."),
            None => format!("\u{26a0} This layer's font is not installed. Showing {actual:?}."),
        }
    }

    /// What the Font control shows when nothing is open.
    fn family_label(block: &TextBlock) -> String {
        requested_family(block).map_or_else(|| DEFAULT_FAMILY.to_owned(), ToOwned::to_owned)
    }

    /// A colour as the field shows it.
    fn hex(rgb: [u8; 3]) -> String {
        format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
    }

    /// A colour as someone typed it, or `None` if that was not a colour.
    ///
    /// Both lengths, because `#f0c` is what people write and doubling each nibble is what it has
    /// meant everywhere for thirty years -- guessing is only guessing when there is no convention.
    fn parse_hex(text: &str) -> Option<[u8; 3]> {
        let digits = text.trim().trim_start_matches('#');
        // Checked before parsing, not after: `from_str_radix` accepts a leading sign, so "+ab"
        // would otherwise be read as a perfectly good three-digit colour.
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        match digits.len() {
            3 => {
                // 0xF becomes 0xFF, which is what multiplying a nibble by 17 does.
                let nibble = |shift: u32| (((value >> shift) & 0xF) as u8) * 17;
                Some([nibble(8), nibble(4), nibble(0)])
            }
            6 => Some([
                ((value >> 16) & 0xFF) as u8,
                ((value >> 8) & 0xFF) as u8,
                (value & 0xFF) as u8,
            ]),
            _ => None,
        }
    }

    /// A position as someone typed it, or `None` if that was not a position.
    fn parse_number(text: &str) -> Option<f32> {
        let value: f32 = text.trim().parse().ok()?;
        // `"nan"` and `"inf"` both parse. A caption at NaN is nowhere at all, and no amount of
        // clamping further down recovers a coordinate that has stopped being a number.
        value.is_finite().then_some(value)
    }

    /// The list the font pick offers: no family in particular, then every family installed.
    pub(super) fn font_options(families: &[String]) -> Vec<String> {
        let mut options = Vec::with_capacity(families.len() + 1);
        options.push(DEFAULT_FAMILY.to_owned());
        options.extend(families.iter().cloned());
        options
    }

    /// Which entry of [`font_options`] this block is set to, if any.
    ///
    /// `None` when the block names a family this machine does not have -- which is precisely the
    /// substituted case, and ticking "(default)" there would be the picker claiming a font was
    /// chosen that was not. Matched case-insensitively, the way `FontResolution` matches.
    pub(super) fn font_chosen(block: &TextBlock, families: &[String]) -> Option<usize> {
        let Some(family) = requested_family(block) else {
            return Some(0);
        };
        families
            .iter()
            .position(|f| f.eq_ignore_ascii_case(family))
            .map(|i| i + 1)
    }

    /// Take entry `chosen` of [`font_options`].
    pub(super) fn set_family(block: &mut TextBlock, families: &[String], chosen: usize) -> Applied {
        let family = if chosen == 0 {
            String::new()
        } else {
            // The list can change between opening it and choosing from it -- a font file loaded
            // in the meantime -- so an index past the end is stale rather than a bug, and doing
            // nothing is the right answer to a stale press.
            match families.get(chosen - 1) {
                Some(family) => family.clone(),
                None => return Applied::Nothing,
            }
        };
        set(&mut block.font.family, family)
    }

    /// Everything the editor shows for one block, in the order the old panel showed it.
    pub(super) fn controls_for(block: &TextBlock, substituted: Option<&str>) -> Vec<Control> {
        let mut controls = Vec::new();

        if let Some(actual) = substituted {
            controls.push(Control::Label {
                text: substitution_warning(requested_family(block), actual),
            });
        }

        // **One line, where the old field was three.** `Control::Text` is single-line, so what is
        // lost is typing a hard line break. Acceptable for now: the wrap width and the alignment
        // below are what decide where lines fall in ordinary lettering, and the alternative is a
        // second control kind that exists for one field in one panel. The hazard is worth naming
        // rather than burying -- a caption that already contains line breaks keeps them only while
        // nobody retypes it, because the field arrives selected and the first keystroke replaces
        // the lot.
        controls.push(Control::Text {
            id: BODY,
            text: "Caption".to_owned(),
            value: block.text.clone(),
        });

        // A dropdown rather than a choice each: a machine has hundreds of families, and laid out
        // as choices they fill the panel with names nobody is reading while the one that matters
        // has to be found among them.
        controls.push(Control::Pick {
            id: FONT,
            text: "Font".to_owned(),
            value: family_label(block),
        });
        controls.push(Control::Toggle {
            id: BOLD,
            text: "Bold".to_owned(),
            on: is_bold(block.font.weight),
        });
        controls.push(Control::Toggle {
            id: ITALIC,
            text: "Italic".to_owned(),
            on: block.font.italic,
        });

        controls.push(Control::Slider {
            id: SIZE,
            text: "Size".to_owned(),
            value: block.size,
            min: 6.0,
            max: 300.0,
            unit: "px",
            // **Logarithmic, where the old one was linear**, for the reason brush radius is: one
            // step at 12 px should feel like one step at 120. Linear over 6..300 puts the default
            // of 32 at a tenth of the track, so every size anyone letters body text at is crammed
            // into the first centimetre of it.
            log: true,
        });
        controls.push(Control::Slider {
            id: LINE_HEIGHT,
            text: "Line height".to_owned(),
            value: block.line_height,
            min: 0.5,
            max: 3.0,
            // A multiple of the size, so it has no unit of its own.
            unit: "",
            log: false,
        });
        controls.push(Control::Slider {
            id: LETTER_SPACING,
            text: "Letter spacing".to_owned(),
            value: block.letter_spacing,
            min: -10.0,
            max: 40.0,
            unit: "px",
            // Crosses zero, so there is no logarithmic reading of it.
            log: false,
        });

        // A rule before each group. The old panel grouped with `ui.horizontal`, which this
        // vocabulary has no equivalent of -- a column of fourteen rows with nothing between them
        // is one long list, and the separator is the only grouping that survives the port.
        controls.push(Control::Separator);
        controls.push(Control::Label {
            text: "Align".to_owned(),
        });
        for (i, align) in Align::ALL.into_iter().enumerate() {
            controls.push(Control::Choice {
                id: FIRST_ALIGN + u32::try_from(i).unwrap_or(u32::MAX),
                text: align.label().to_owned(),
                selected: block.align == align,
                // No icon: three words read as three words, and an arrow for "Start" would be
                // wrong the moment the script runs right to left, which is the whole reason these
                // are not called Left and Right.
                icon: None,
            });
        }

        // **A hex field, where the old panel had egui's colour picker.** There is no colour
        // control in this vocabulary, and the Colour panel owns the wheel; a second wheel here
        // would be a `Custom` copy of another panel's drawing and a second place to pick a colour,
        // which is the shape the descriptor layer's own header warns about. It cannot simply be
        // dropped either -- a text block's colour comes from `TextBlock::default`, not from the
        // brush, so with nothing here it could never be set at all. Hex is exact, typed, testable,
        // and how a letterer specifies a brand colour anyway.
        controls.push(Control::Text {
            id: COLOUR,
            text: "Colour".to_owned(),
            value: hex(block.color_srgb8),
        });

        controls.push(Control::Separator);
        controls.push(Control::Toggle {
            id: WRAP,
            text: "Wrap".to_owned(),
            on: block.wrap_width.is_some(),
        });
        // Only when it is on, because a width for a block that does not wrap is a number with
        // nothing to do -- and the old panel did the same.
        if let Some(width) = block.wrap_width {
            controls.push(Control::Slider {
                id: WRAP_WIDTH,
                // "Width", not "px": the old label read "px" because egui put the unit in the
                // label, and this vocabulary has a field for the unit.
                text: "Width".to_owned(),
                value: width,
                min: 40.0,
                max: 4000.0,
                unit: "px",
                log: false,
            });
        }

        controls.push(Control::Separator);
        // **Typed, not dragged.** The old panel used two `DragValue`s; a slider is the nearest
        // thing this vocabulary has, and it would need a range -- but a caption can sit anywhere
        // on a page of any size, and half off it. Any range invented here would be a fence in a
        // place nobody measured. A field takes an exact number, which is what a letterer aligning
        // two balloons actually wants, and dragging the block on the canvas is the gesture that
        // belongs to the canvas rather than to a panel.
        controls.push(Control::Text {
            id: POS_X,
            text: "X".to_owned(),
            value: block.x.to_string(),
        });
        controls.push(Control::Text {
            id: POS_Y,
            text: "Y".to_owned(),
            value: block.y.to_string(),
        });

        controls
    }

    /// Apply one change to a block, and say what that did.
    pub(super) fn apply(block: &mut TextBlock, change: Change) -> Applied {
        match change {
            // Taking the caret is not an edit. It is reported so the panel can tell that a field
            // is being typed into; there is nothing to write until it is finished with.
            Change::Typing(_) => Applied::Nothing,
            Change::Typed(BODY, value) => set(&mut block.text, value),
            Change::Typed(COLOUR, value) => match parse_hex(&value) {
                Some(rgb) => set(&mut block.color_srgb8, rgb),
                // Refused rather than repaired. The block keeps the colour it had, so the field
                // shows the old value again on the next frame -- which is the field saying no.
                None => Applied::Rejected("A colour is three or six hex digits, like #1E1E28."),
            },
            Change::Typed(POS_X, value) => match parse_number(&value) {
                Some(x) => set(&mut block.x, x),
                None => Applied::Rejected("A position is a number of document pixels."),
            },
            Change::Typed(POS_Y, value) => match parse_number(&value) {
                Some(y) => set(&mut block.y, y),
                None => Applied::Rejected("A position is a number of document pixels."),
            },
            // A press opens the list; it chooses nothing by itself.
            Change::Pressed(FONT) => Applied::OpenFont,
            // **The lossy half of a switch over an axis.** Turning Bold off from 850 lands on 400,
            // not back on 850, exactly as the old panel did. A weight slider is what fixes that,
            // and it is not worth inventing here on the strength of a guess.
            Change::Toggled(BOLD, on) => set(
                &mut block.font.weight,
                if on { BOLD_WEIGHT } else { REGULAR_WEIGHT },
            ),
            Change::Toggled(ITALIC, on) => set(&mut block.font.italic, on),
            // `None` is one line that grows as it is typed; `Some` is a box that wraps. Two ways
            // of placing text, one field, rather than two kinds of block. Turning it off forgets
            // the width -- the same loss the old panel had, and undo is what brings it back.
            //
            // The default is the width a block gets when it *has* none, not every time the switch
            // is asked for on: the old panel wrote 400 unconditionally, so a redundant "on" -- a
            // press the renderer repeated, a state restored -- silently resized a box someone had
            // set to 1200.
            Change::Toggled(WRAP, on) => {
                let width = on.then(|| block.wrap_width.unwrap_or(DEFAULT_WRAP_WIDTH));
                set(&mut block.wrap_width, width)
            }
            Change::Set(SIZE, value) => set(&mut block.size, value),
            Change::Set(LINE_HEIGHT, value) => set(&mut block.line_height, value),
            Change::Set(LETTER_SPACING, value) => set(&mut block.letter_spacing, value),
            Change::Set(WRAP_WIDTH, value) => {
                if block.wrap_width.is_some() {
                    set(&mut block.wrap_width, Some(value))
                } else {
                    // The control is only offered while wrapping is on, so arriving without it is
                    // a bug worth hearing about -- and turning wrapping on to honour it would be
                    // inventing an edit nobody asked for.
                    Applied::Unexpected(Change::Set(WRAP_WIDTH, value))
                }
            }
            Change::Chose(id) if id >= FIRST_ALIGN => {
                match Align::ALL.get((id - FIRST_ALIGN) as usize) {
                    Some(align) => set(&mut block.align, *align),
                    None => Applied::Unexpected(Change::Chose(id)),
                }
            }
            other => Applied::Unexpected(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::editor::{
        apply, controls_for, font_chosen, font_options, set_family, substitution_warning, Applied,
        DEFAULT_FAMILY,
    };
    use super::{
        BODY, BOLD, COLOUR, FIRST_ALIGN, FONT, ITALIC, LETTER_SPACING, LINE_HEIGHT, POS_X, POS_Y,
        SIZE, WRAP, WRAP_WIDTH,
    };
    use crate::layout::Rect;
    use crate::panel_ui::{change_at, place, Change, Control, Direction};
    use crate::theme::Theme;
    use openpaint_core::text::{Align, TextBlock};

    fn block() -> TextBlock {
        TextBlock::default()
    }

    fn families() -> Vec<String> {
        vec![
            "Comic Sans MS".to_owned(),
            "Segoe UI".to_owned(),
            "Yu Gothic".to_owned(),
        ]
    }

    /// The control with this id, if the list has one.
    fn find(controls: &[Control], id: u32) -> Option<&Control> {
        controls.iter().find(|c| c.id() == Some(id))
    }

    fn has(controls: &[Control], id: u32) -> bool {
        find(controls, id).is_some()
    }

    /// A `Control::Text`'s value, or the test fails saying which id was not a field.
    fn field(controls: &[Control], id: u32) -> String {
        match find(controls, id) {
            Some(Control::Text { value, .. }) => value.clone(),
            other => panic!("{id} should be a text field, not {other:?}"),
        }
    }

    fn toggled(controls: &[Control], id: u32) -> bool {
        match find(controls, id) {
            Some(Control::Toggle { on, .. }) => *on,
            other => panic!("{id} should be a toggle, not {other:?}"),
        }
    }

    fn slider(controls: &[Control], id: u32) -> f32 {
        match find(controls, id) {
            Some(Control::Slider { value, .. }) => *value,
            other => panic!("{id} should be a slider, not {other:?}"),
        }
    }

    /// A block edited by one change, and what the edit reported.
    fn after(mut block: TextBlock, change: Change) -> (TextBlock, Applied) {
        let applied = apply(&mut block, change);
        (block, applied)
    }

    /// Everything the old editor offered is offered, and nothing is numbered twice.
    ///
    /// The duplicate check is the point: the ids are written by hand, and two controls sharing one
    /// is a silent mis-hit that only shows up as the wrong value moving.
    #[test]
    fn every_control_is_offered_once_and_answers_to_its_own_id() {
        let controls = controls_for(&block(), None);
        for id in [
            BODY,
            FONT,
            BOLD,
            ITALIC,
            SIZE,
            LINE_HEIGHT,
            LETTER_SPACING,
            COLOUR,
            WRAP,
            POS_X,
            POS_Y,
            FIRST_ALIGN,
            FIRST_ALIGN + 1,
            FIRST_ALIGN + 2,
        ] {
            assert!(has(&controls, id), "{id} is missing from the editor");
        }

        let mut ids: Vec<u32> = controls.iter().filter_map(Control::id).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(total, ids.len(), "two controls share an id: {ids:?}");
    }

    /// A block that does not wrap has no width to set, so the control is not there at all --
    /// rather than there and doing nothing.
    #[test]
    fn the_wrap_width_appears_only_while_wrapping_is_on() {
        let off = controls_for(&block(), None);
        assert!(!toggled(&off, WRAP), "a fresh block is one growing line");
        assert!(!has(&off, WRAP_WIDTH), "and has no width to set");

        let wrapping = TextBlock {
            wrap_width: Some(640.0),
            ..block()
        };
        let on = controls_for(&wrapping, None);
        assert!(toggled(&on, WRAP));
        assert!((slider(&on, WRAP_WIDTH) - 640.0).abs() < 0.001);
    }

    /// **Bold is a threshold, and thresholds are wrong at their boundary.** The switch is derived
    /// from a number, so the number either side of 600 is the whole test.
    #[test]
    fn the_bold_switch_reads_the_weight_at_its_boundary() {
        for (weight, bold) in [
            (100_u16, false),
            (400, false),
            (599, false),
            (600, true),
            (700, true),
            (1000, true),
        ] {
            let mut b = block();
            b.font.weight = weight;
            assert_eq!(
                toggled(&controls_for(&b, None), BOLD),
                bold,
                "weight {weight} should read as bold={bold}"
            );
        }
    }

    /// Turning Bold on writes 700 and off writes 400, and a weight already on the right side of
    /// the line is not rewritten.
    #[test]
    fn the_bold_switch_writes_a_weight() {
        let (b, applied) = after(block(), Change::Toggled(BOLD, true));
        assert_eq!(applied, Applied::Edited);
        assert_eq!(b.font.weight, 700);

        // Off from a variable weight lands on 400 rather than back where it was. Stated as a test
        // because it is a real loss, not an accident.
        let mut heavy = block();
        heavy.font.weight = 850;
        let (b, applied) = after(heavy, Change::Toggled(BOLD, false));
        assert_eq!(applied, Applied::Edited);
        assert_eq!(b.font.weight, 400);

        // Already 400, asked for 400: nothing to re-derive and nothing to undo.
        let (_, applied) = after(block(), Change::Toggled(BOLD, false));
        assert_eq!(applied, Applied::Nothing);
    }

    #[test]
    fn the_italic_switch_writes_through() {
        let (b, applied) = after(block(), Change::Toggled(ITALIC, true));
        assert_eq!(applied, Applied::Edited);
        assert!(b.font.italic);
        assert!(toggled(&controls_for(&b, None), ITALIC));
        assert_eq!(after(b, Change::Toggled(ITALIC, true)).1, Applied::Nothing);
    }

    /// **Wrap is derived from an `Option`, and both edges of that are easy to get wrong.**
    #[test]
    fn the_wrap_switch_turns_a_width_on_and_off() {
        let (b, applied) = after(block(), Change::Toggled(WRAP, true));
        assert_eq!(applied, Applied::Edited);
        assert_eq!(b.wrap_width, Some(400.0), "on has to mean some width");

        let (b, applied) = after(b, Change::Toggled(WRAP, false));
        assert_eq!(applied, Applied::Edited);
        assert_eq!(b.wrap_width, None);

        // Already off: nothing happens, rather than a re-render for a switch that did not move.
        assert_eq!(after(b, Change::Toggled(WRAP, false)).1, Applied::Nothing);

        // And on again from a width that is already set keeps it rather than resetting to 400.
        let wide = TextBlock {
            wrap_width: Some(1200.0),
            ..block()
        };
        assert_eq!(
            after(wide, Change::Toggled(WRAP, true)),
            (
                TextBlock {
                    wrap_width: Some(1200.0),
                    ..block()
                },
                Applied::Nothing
            ),
            "turning on something already on should not reset it"
        );
    }

    /// A width for a block that does not wrap is a control that was never offered, so it is said
    /// out loud rather than quietly turning wrapping on.
    #[test]
    fn a_width_without_wrapping_is_refused() {
        let (b, applied) = after(block(), Change::Set(WRAP_WIDTH, 800.0));
        assert_eq!(b.wrap_width, None);
        assert!(matches!(applied, Applied::Unexpected(_)), "{applied:?}");

        let wrapping = TextBlock {
            wrap_width: Some(400.0),
            ..block()
        };
        let (b, applied) = after(wrapping, Change::Set(WRAP_WIDTH, 800.0));
        assert_eq!(applied, Applied::Edited);
        assert_eq!(b.wrap_width, Some(800.0));
    }

    #[test]
    fn the_sliders_show_and_write_their_numbers() {
        let controls = controls_for(&block(), None);
        // The defaults a fresh block carries, which every range has to bracket.
        assert!((slider(&controls, SIZE) - 32.0).abs() < 0.001);
        assert!((slider(&controls, LINE_HEIGHT) - 1.2).abs() < 0.001);
        assert!(slider(&controls, LETTER_SPACING).abs() < 0.001);

        for (id, value, read) in [
            (
                SIZE,
                96.0_f32,
                (|b: &TextBlock| b.size) as fn(&TextBlock) -> f32,
            ),
            (LINE_HEIGHT, 2.0, |b| b.line_height),
            (LETTER_SPACING, -4.0, |b| b.letter_spacing),
        ] {
            let (b, applied) = after(block(), Change::Set(id, value));
            assert_eq!(applied, Applied::Edited, "slider {id} did not write");
            assert!(
                (read(&b) - value).abs() < 0.001,
                "slider {id} wrote {}",
                read(&b)
            );
        }
    }

    /// Every range brackets the default a fresh block carries, so the slider does not open with
    /// its knob pinned against an end.
    #[test]
    fn every_slider_range_contains_the_value_it_starts_with() {
        let wrapping = TextBlock {
            wrap_width: Some(400.0),
            ..block()
        };
        for control in controls_for(&wrapping, None) {
            if let Control::Slider {
                id,
                value,
                min,
                max,
                ..
            } = control
            {
                assert!(min < max, "slider {id} has an empty range");
                assert!(
                    value >= min && value <= max,
                    "slider {id} opens at {value}, outside {min}..={max}"
                );
            }
        }
    }

    #[test]
    fn the_alignment_that_is_set_is_the_one_marked() {
        for (i, align) in Align::ALL.into_iter().enumerate() {
            let b = TextBlock { align, ..block() };
            let controls = controls_for(&b, None);
            for (j, other) in Align::ALL.into_iter().enumerate() {
                let id = FIRST_ALIGN + u32::try_from(j).expect("three alignments");
                let marked = matches!(
                    find(&controls, id),
                    Some(Control::Choice { selected: true, .. })
                );
                assert_eq!(
                    marked,
                    i == j,
                    "with {align:?} set, {other:?} should be marked={}",
                    i == j
                );
            }
        }
    }

    #[test]
    fn choosing_an_alignment_writes_it() {
        for (i, align) in Align::ALL.into_iter().enumerate() {
            let id = FIRST_ALIGN + u32::try_from(i).expect("three alignments");
            let mut b = TextBlock {
                // Something other than the one being chosen, so every case is a real change.
                align: Align::Center,
                ..block()
            };
            let applied = apply(&mut b, Change::Chose(id));
            assert_eq!(b.align, align);
            assert!(
                matches!(applied, Applied::Edited | Applied::Nothing),
                "choosing {align:?} reported {applied:?}"
            );
        }

        // An alignment that does not exist is refused rather than clamped to one that does.
        let (b, applied) = after(block(), Change::Chose(FIRST_ALIGN + 9));
        assert_eq!(b.align, Align::Start);
        assert!(matches!(applied, Applied::Unexpected(_)), "{applied:?}");
    }

    #[test]
    fn the_caption_is_typed_and_reported_once() {
        let (b, applied) = after(block(), Change::Typed(BODY, "Ka-boom".to_owned()));
        assert_eq!(applied, Applied::Edited);
        assert_eq!(b.text, "Ka-boom");
        assert_eq!(field(&controls_for(&b, None), BODY), "Ka-boom");

        // Clicking into a field and pressing Enter without changing anything is not an edit, and
        // must not put a step on the undo stack.
        assert_eq!(
            after(b, Change::Typed(BODY, "Ka-boom".to_owned())).1,
            Applied::Nothing
        );
        // Taking the caret is not an edit either.
        assert_eq!(after(block(), Change::Typing(BODY)).1, Applied::Nothing);
    }

    #[test]
    fn a_colour_is_read_and_written_as_hex() {
        assert_eq!(field(&controls_for(&block(), None), COLOUR), "#141418");

        for text in ["#FF00CC", "ff00cc", "  #f0c  ", "F0C"] {
            let (b, applied) = after(block(), Change::Typed(COLOUR, text.to_owned()));
            assert_eq!(applied, Applied::Edited, "{text:?} should be a colour");
            assert_eq!(b.color_srgb8, [255, 0, 204], "{text:?}");
        }

        // Round trip: what the field shows goes back in and means the same colour.
        let shown = field(&controls_for(&block(), None), COLOUR);
        let (b, _) = after(block(), Change::Typed(COLOUR, shown));
        assert_eq!(b.color_srgb8, block().color_srgb8);
    }

    /// **What is not a colour leaves the colour alone**, and says so rather than springing back
    /// in silence.
    #[test]
    fn what_is_not_a_colour_is_refused() {
        // "+ab" is the one that matters: `from_str_radix` accepts a sign, so a length check on its
        // own would have read it as a perfectly good three-digit colour.
        for text in [
            "", "#", "purple", "#12", "#12345", "#1234567", "+ab", "#-ab",
        ] {
            let (b, applied) = after(block(), Change::Typed(COLOUR, text.to_owned()));
            assert!(
                matches!(applied, Applied::Rejected(_)),
                "{text:?} should not be a colour, got {applied:?}"
            );
            assert_eq!(b.color_srgb8, block().color_srgb8, "{text:?} changed it");
        }
    }

    #[test]
    fn a_position_is_typed_as_a_number() {
        let (b, applied) = after(block(), Change::Typed(POS_X, " -12.5 ".to_owned()));
        assert_eq!(applied, Applied::Edited);
        assert!((b.x - -12.5).abs() < 0.001);
        let (b, applied) = after(b, Change::Typed(POS_Y, "300".to_owned()));
        assert_eq!(applied, Applied::Edited);
        assert!((b.y - 300.0).abs() < 0.001);

        let controls = controls_for(&b, None);
        assert_eq!(field(&controls, POS_X), "-12.5");
        assert_eq!(field(&controls, POS_Y), "300");
    }

    /// `"nan"` and `"inf"` both parse as floats. A caption at NaN is nowhere at all, and nothing
    /// downstream can recover a coordinate that has stopped being a number.
    #[test]
    fn a_position_that_is_not_a_number_is_refused() {
        for text in ["", "over there", "nan", "inf", "-inf", "1,5"] {
            for id in [POS_X, POS_Y] {
                let (b, applied) = after(block(), Change::Typed(id, text.to_owned()));
                assert!(
                    matches!(applied, Applied::Rejected(_)),
                    "{text:?} should not be a position, got {applied:?}"
                );
                assert!(b.x == 0.0 && b.y == 0.0, "{text:?} moved the block");
            }
        }
    }

    #[test]
    fn the_font_list_starts_with_no_family_in_particular() {
        let options = font_options(&families());
        assert_eq!(options[0], DEFAULT_FAMILY);
        assert_eq!(&options[1..], families().as_slice());
        assert_eq!(font_options(&[]), vec![DEFAULT_FAMILY.to_owned()]);
    }

    #[test]
    fn the_font_pick_shows_and_sets_the_family() {
        let controls = controls_for(&block(), None);
        assert!(
            matches!(find(&controls, FONT), Some(Control::Pick { value, .. }) if value == DEFAULT_FAMILY)
        );

        // A press opens the list; it chooses nothing by itself.
        assert_eq!(after(block(), Change::Pressed(FONT)).1, Applied::OpenFont);

        let mut b = block();
        assert_eq!(set_family(&mut b, &families(), 2), Applied::Edited);
        assert_eq!(b.font.family, "Segoe UI");
        assert!(
            matches!(find(&controls_for(&b, None), FONT), Some(Control::Pick { value, .. }) if value == "Segoe UI")
        );

        // Back to nothing in particular.
        assert_eq!(set_family(&mut b, &families(), 0), Applied::Edited);
        assert!(b.font.family.is_empty());

        // A stale index -- the list grew or shrank between opening it and choosing -- does
        // nothing, rather than picking whatever is now last.
        let mut b = block();
        assert_eq!(set_family(&mut b, &families(), 99), Applied::Nothing);
        assert!(b.font.family.is_empty());
    }

    /// **A family this machine does not have ticks nothing**, rather than ticking "(default)" and
    /// claiming a font was chosen that was not -- which is exactly the substituted case.
    #[test]
    fn the_font_list_marks_only_a_family_it_actually_has() {
        let mut b = block();
        assert_eq!(font_chosen(&b, &families()), Some(0), "empty is (default)");

        b.font.family = "Segoe UI".to_owned();
        assert_eq!(font_chosen(&b, &families()), Some(2));

        // Matched the way font systems match: case alone is not a different family.
        b.font.family = "segoe ui".to_owned();
        assert_eq!(font_chosen(&b, &families()), Some(2));

        b.font.family = "Impact".to_owned();
        assert_eq!(font_chosen(&b, &families()), None);
    }

    #[test]
    fn a_substituted_font_is_warned_about_by_name() {
        let mut b = block();
        b.font.family = "Comic Sans MS".to_owned();
        let controls = controls_for(&b, Some("Arial"));
        let warning = match controls.first() {
            Some(Control::Label { text }) => text.clone(),
            other => panic!("the warning should come first, not {other:?}"),
        };
        assert!(warning.contains("Comic Sans MS"), "{warning}");
        assert!(warning.contains("Arial"), "{warning}");
        assert!(warning.starts_with('\u{26a0}'), "{warning}");

        // No substitution, no warning: otherwise every untouched block would be shouting.
        assert!(
            !matches!(controls_for(&b, None).first(), Some(Control::Label { .. })),
            "a block in its own font should not warn"
        );
    }

    /// The panel can be a frame ahead of the document, and then it has no family to name. It says
    /// so rather than printing an empty pair of quotes.
    #[test]
    fn a_warning_with_no_family_still_reads_as_a_sentence() {
        let named = substitution_warning(Some("Yu Gothic"), "Segoe UI");
        assert!(named.contains("Yu Gothic") && named.contains("Segoe UI"));
        let anonymous = substitution_warning(None, "Segoe UI");
        assert!(anonymous.contains("Segoe UI"));
        assert!(!anonymous.contains("\"\""), "{anonymous}");
    }

    /// **Every control the editor draws is one `apply` answers to.**
    ///
    /// The two halves are written apart and numbered by hand, so this lays the real list out, aims
    /// a press at the middle of each row, and checks that nothing comes back unrecognised. An id
    /// that drifted between the two functions shows up here rather than as one silent control.
    #[test]
    fn pressing_every_control_produces_something_the_panel_understands() {
        let wrapping = TextBlock {
            wrap_width: Some(400.0),
            text: "Ka-boom".to_owned(),
            ..block()
        };
        let controls = controls_for(&wrapping, Some("Arial"));
        let m = Theme::default().metrics;
        let content = Rect::new(0.0, 0.0, 260.0, 4000.0);
        // A stand-in for a font: this test is about ids, not about text measurement.
        let laid = place(
            &controls,
            content,
            &m,
            Direction::Column,
            &|_: &Control| 40.0,
            &|_: &Control, _: f32| 0.0,
        );

        let mut pressed = 0;
        for p in &laid {
            let r = p.rect;
            let Some(change) = change_at(p.control, r, &m, r.x + r.w / 2.0, r.y + r.h / 2.0) else {
                // Labels and rules answer nothing, which is what they are for.
                continue;
            };
            let mut block = wrapping.clone();
            let applied = apply(&mut block, change.clone());
            assert!(
                !matches!(applied, Applied::Unexpected(_)),
                "{:?} produced {change:?}, which the panel does not understand",
                p.control
            );
            pressed += 1;
        }
        assert!(pressed >= 12, "only {pressed} controls answered a press");

        // And the fields, which answer a press with the caret rather than a value: finishing each
        // of them has to be understood too.
        for id in [BODY, COLOUR, POS_X, POS_Y] {
            let mut block = wrapping.clone();
            let value = field(&controls, id);
            let applied = apply(&mut block, Change::Typed(id, value));
            assert!(
                matches!(applied, Applied::Nothing),
                "field {id} handed back what it was showing and got {applied:?}"
            );
        }
    }

    /// An id from another panel is refused rather than quietly matching something here.
    #[test]
    fn a_change_from_somewhere_else_is_refused() {
        for change in [
            Change::Pressed(9999),
            Change::Set(9999, 1.0),
            Change::Toggled(9999, true),
            Change::Typed(9999, "x".to_owned()),
        ] {
            let (b, applied) = after(block(), change.clone());
            assert!(
                matches!(applied, Applied::Unexpected(_)),
                "{change:?} should not have been understood: {applied:?}"
            );
            assert_eq!(b, block(), "{change:?} changed the block anyway");
        }
    }
}
#[cfg(test)]
mod wiring {
    use super::*;
    use crate::panel_ui::Change;

    /// **An edit to a caption comes back out of the panel.**
    ///
    /// The editor was written and proved before it could be wired, so everything about it was
    /// true of a copy nobody read. This is the one thing those tests could not say: that a change
    /// arriving at `show` reaches the block and leaves again as something the shell can write.
    #[test]
    fn an_edit_folds_into_the_block_and_comes_back() {
        let block = openpaint_core::TextBlock {
            text: "Before".to_owned(),
            ..openpaint_core::TextBlock::default()
        };
        let was = block.clone();

        // What `show` does with the frame's changes, in the same order.
        let mut edited = block.clone();
        assert_eq!(
            editor::apply(&mut edited, Change::Typed(BODY, "After".to_owned())),
            editor::Applied::Edited
        );
        assert_ne!(edited, was, "the fold did not change the block");
        assert_eq!(edited.text, "After");

        // And a change that alters nothing must not become a write: every one costs a re-render
        // and a step on the undo stack.
        let mut same = was.clone();
        assert_eq!(
            editor::apply(&mut same, Change::Typed(BODY, "Before".to_owned())),
            editor::Applied::Nothing
        );
        assert_eq!(same, was);
    }

    /// **An edit reaches the shell, and one that changes nothing does not.**
    ///
    /// The half of the wiring `show` owns: folding the frame's changes into a copy and deciding
    /// whether the copy is worth writing. Both halves have been wrong -- a version that never
    /// answered at all, and one that answered on every frame the panel was drawn.
    #[test]
    fn a_caption_is_written_back_only_when_it_differs() {
        let was = openpaint_core::TextBlock {
            text: "Before".to_owned(),
            ..openpaint_core::TextBlock::default()
        };

        // Changed: it goes back, and it goes back as what it now says.
        let mut now = was.clone();
        now.text = "After".to_owned();
        assert_eq!(
            written_back(Some(now.clone()), Some(&was)),
            Some(Picked::TextSet(now))
        );

        // Unchanged: nothing. Every write is a re-render and a step on the undo stack.
        assert_eq!(written_back(Some(was.clone()), Some(&was)), None);

        // No text layer under the panel: nothing to write, whatever arrived.
        assert_eq!(written_back(Some(was.clone()), None), None);
        assert_eq!(written_back(None, Some(&was)), None);
    }

    /// The font list asks to be opened rather than choosing anything by itself.
    #[test]
    fn the_font_control_opens_its_list() {
        let mut block = openpaint_core::TextBlock::default();
        assert_eq!(
            editor::apply(&mut block, Change::Pressed(FONT)),
            editor::Applied::OpenFont
        );
        // And choosing from it is an edit, through the same door the popup uses.
        let families = vec!["Inter".to_owned(), "Source Han".to_owned()];
        let options = editor::font_options(&families);
        let n = options
            .iter()
            .position(|o| o == "Source Han")
            .expect("the list offers it");
        assert_eq!(
            editor::set_family(&mut block, &families, n),
            editor::Applied::Edited
        );
        assert_eq!(editor::requested_family(&block), Some("Source Han"));
    }
}
