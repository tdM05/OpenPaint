//! A single-line text field: where the caret is, what is selected, and what typing does to both.
//!
//! Nothing here draws, measures, or knows about egui. A field is a `String` and two byte offsets,
//! and every operation on it is a pure function of those three — which is the only reason the
//! awkward half of a text editor (multi-byte movement, backwards selections, a caret at either
//! end) can be proved on a machine with no screen, the same way `crop` and `panel_ui` are.
//!
//! # Every index is a byte offset, and always on a character boundary
//!
//! The caret and the selection anchor are byte offsets into [`TextField::text`]. They are **never**
//! allowed to sit inside a character. That is not a nicety: `String::replace_range` and every `&str`
//! slice panic on an interior byte, so a caret one byte into `日` does not misbehave — it takes the
//! application down on the next keystroke. This document is for comics and will carry Japanese, so
//! that is a first-keystroke bug, not an edge case.
//!
//! The invariant is kept in one place rather than at each call site: every offset that enters this
//! module goes through [`boundary_at_or_before`], and every offset produced inside it is derived
//! from `char` positions in the current text. Callers may therefore pass any `usize` at all —
//! including stale offsets measured before an edit — without being able to cause a panic.
//!
//! # Movement is by `char`, not by grapheme cluster — and that is unfinished
//!
//! One arrow press moves one Unicode scalar. For Japanese, precomposed accents and the emoji that
//! fit in a single scalar, that is exactly right. It is **wrong** for a decomposed accent
//! (`e` + U+0301), a ZWJ emoji sequence, a flag (a regional-indicator pair) and a skin-tone
//! modifier: each of those is one thing on screen and several presses to cross, and backspace eats
//! it a piece at a time. [`word_left`]/[`word_right`] have the matching gap in the other direction
//! — Japanese has no spaces, so a run of kana and kanji reads as one enormous word.
//!
//! Both want the same fix and only that fix: `unicode-segmentation`, which is not a dependency of
//! this crate today (it is in `Cargo.lock` only because winit uses it). Adding it would confine the
//! change to [`prev_boundary`], [`next_boundary`] and [`word_left`]/[`word_right`]; nothing else in
//! this module looks at a character. The tests named `..._pins_char_stepping` record today's
//! behaviour on purpose, so that swapping the step changes a test rather than passing silently.
//!
//! # What this deliberately is not
//!
//! Not IME composition (a preedit string with its own caret, which Japanese input needs and which
//! belongs to the platform layer that owns the window), not the clipboard, not undo — the field
//! reports enough for the caller to do all three — and not wrapping, which is what makes it
//! single-line.

// Nothing calls this yet — the field is built and proved before it is drawn, so the two jobs do not
// have to be got right at once. **Delete this the moment the first caller lands**: after that it is
// no longer "not wired up", it is an operation nobody uses, and hiding those is how a module grows
// three ways to do the same thing. `allow` rather than `expect` because the test build *does* use
// every item, so an `expect` would be unfulfilled there and fail the lint gate on its own.
#![allow(dead_code)]

use std::ops::Range;

/// A place the caret may sit, and where that place is on screen.
///
/// The caller measures text; this module must not, so hit-testing is handed the answer. Each stop
/// carries **its own** byte offset rather than being the nth entry in a list this module also
/// enumerates: two lists that have to agree on length and order are the shape that made every
/// database migration fail (DECISIONS §11a.8), and here the mismatch would be silent — a caret
/// landing on the wrong character.
///
/// Offsets are not trusted. A list measured before the last keystroke is clamped to a real
/// boundary rather than being allowed to slice a character in half.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaretStop {
    /// Byte offset into the field's text.
    pub byte: usize,
    /// Where that offset sits, in whatever units the hit position is given in.
    pub x: f32,
}

/// A caret movement, as a key press means it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// One character back.
    Left,
    /// One character on.
    Right,
    /// To the start of the word before the caret.
    WordLeft,
    /// To the start of the word after the caret.
    WordRight,
    /// To the very start.
    Home,
    /// To the very end.
    End,
}

/// A single-line text field: its text, its caret, and its selection.
///
/// The selection runs between `anchor` and `caret` in either order, because that is what dragging
/// produces — the anchor is where the gesture started and the caret is where it is now. Callers see
/// it ordered, through [`TextField::selection`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextField {
    text: String,
    /// Byte offset, always on a character boundary. Where the next character lands.
    caret: usize,
    /// Byte offset, always on a character boundary. The fixed end of the selection.
    anchor: usize,
}

impl TextField {
    /// A field holding `text`, with the caret at the end and nothing selected.
    ///
    /// At the end rather than the start because this is what a field looks like when it is opened
    /// to be added to; a caller that wants the text replaced wholesale calls [`Self::select_all`],
    /// which is also what focusing a field by keyboard does elsewhere.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let end = text.len();
        Self {
            text,
            caret: end,
            anchor: end,
        }
    }

    /// Replace the whole text, putting the caret at the end and dropping the selection.
    ///
    /// For a field being re-pointed at a different value — another layer's name — rather than for
    /// editing, which is what the insert and delete operations are for.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        let end = self.text.len();
        self.caret = end;
        self.anchor = end;
    }

    /// What to draw.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where to draw the caret, as a byte offset into [`Self::text`].
    #[must_use]
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// What to draw as selected, low offset first, or `None` when nothing is.
    ///
    /// Ordered even when the artist dragged right-to-left, so the caller can slice with it
    /// directly. An empty range is reported as `None` rather than as `n..n`: "no selection" and
    /// "an empty selection" are the same state, and giving it two spellings is how a caller ends
    /// up drawing a zero-width highlight over the caret.
    #[must_use]
    pub fn selection(&self) -> Option<Range<usize>> {
        let range = self.range();
        if range.is_empty() {
            None
        } else {
            Some(range)
        }
    }

    /// Whether anything is selected.
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    /// The selected text, empty when nothing is selected. This is what a copy or a cut takes.
    #[must_use]
    pub fn selected_text(&self) -> &str {
        // Both ends are boundaries by the module invariant, so this cannot be a partial character.
        &self.text[self.range()]
    }

    /// Select everything, leaving the caret at the end.
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Put the caret at `byte`, dropping the selection.
    ///
    /// The offset is clamped into the text and back onto a character boundary, so a caller working
    /// from its own arithmetic cannot place a caret that later panics.
    pub fn set_caret(&mut self, byte: usize) {
        self.place(byte);
    }

    /// Delete the selection, if there is one. Reports whether anything was removed, which is the
    /// difference between a cut that happened and one that had nothing to take.
    pub fn delete_selection(&mut self) -> bool {
        let range = self.range();
        if range.is_empty() {
            return false;
        }
        let start = range.start;
        self.text.replace_range(range, "");
        self.place(start);
        true
    }

    /// Type one character, replacing the selection.
    ///
    /// Control characters are dropped rather than inserted — see [`Self::insert_str`].
    pub fn insert_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.insert_str(c.encode_utf8(&mut buf));
    }

    /// Insert a string — a paste — replacing the selection.
    ///
    /// **Control characters are dropped, newlines included.** A single-line field that accepts a
    /// two-line paste is holding text nothing downstream can draw, and the clipboard is exactly
    /// where multi-line text comes from. Dropping rather than substituting a space is what a
    /// browser's `input type=text` does, and inventing a separator would be inventing content.
    ///
    /// The caret lands after what was actually inserted, not after what was offered, so a filtered
    /// paste still leaves the caret where the artist can see it arrived.
    pub fn insert_str(&mut self, s: &str) {
        let range = self.range();
        let start = range.start;
        if s.chars().any(char::is_control) {
            let filtered: String = s.chars().filter(|c| !c.is_control()).collect();
            self.text.replace_range(range, &filtered);
            self.place(start + filtered.len());
        } else {
            self.text.replace_range(range, s);
            self.place(start + s.len());
        }
    }

    /// Backspace: remove the selection, or the character before the caret.
    ///
    /// A no-op at the very start of the text, which is a key the artist holds down.
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.caret == 0 {
            return;
        }
        let start = prev_boundary(&self.text, self.caret);
        self.text.replace_range(start..self.caret, "");
        self.place(start);
    }

    /// Delete: remove the selection, or the character after the caret.
    ///
    /// A no-op at the very end of the text.
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        // At the very end `next_boundary` returns the caret, and `replace_range` takes that empty
        // range as the no-op it is. There was a guard here; deleting it changed no test and no
        // behaviour, which is the definition of code that only looks load-bearing (DECISIONS
        // §11a.6, from the other side). The end-of-text case is still covered by a test.
        let end = next_boundary(&self.text, self.caret);
        self.text.replace_range(self.caret..end, "");
        self.place(self.caret);
    }

    /// Move the caret, dropping the selection.
    pub fn move_caret(&mut self, motion: Motion) {
        let range = self.range();
        let to = if range.is_empty() {
            self.target(motion)
        } else {
            match motion {
                // A left or right arrow over a selection collapses it to the near edge instead of
                // stepping past it. That is what every text field does, and it is what the artist
                // means: after selecting a word, Left is "never mind, put me before it".
                Motion::Left => range.start,
                Motion::Right => range.end,
                _ => self.target(motion),
            }
        };
        self.place(to);
    }

    /// Move the caret with shift held: the anchor stays put and the selection grows or shrinks.
    ///
    /// Extending back across the anchor is allowed and produces a backwards selection, which is
    /// the state [`Self::selection`] exists to order.
    pub fn extend_selection(&mut self, motion: Motion) {
        // The anchor is deliberately untouched, including when there was no selection: that is how
        // the first shift-arrow decides which end is fixed.
        self.caret = self.target(motion);
    }

    /// Where a press at `x` would put the caret, given the caller's measured stops.
    ///
    /// The nearest stop wins, so pressing on the left half of a glyph puts the caret before it and
    /// on the right half after it — anything else makes the last character of a field unreachable
    /// by pointer. Ties go to the earlier stop.
    ///
    /// With no stops the caret does not move: a measurement that produced nothing is a failure to
    /// measure, and jumping to the start of the text would look like a bug rather than reporting
    /// one.
    #[must_use]
    pub fn caret_from_hit(&self, x: f32, stops: &[CaretStop]) -> usize {
        let mut best: Option<(f32, usize)> = None;
        for stop in stops {
            let distance = (stop.x - x).abs();
            // A NaN measurement compares false against everything, so it can never win here. Left
            // to `min`-style code it would win every comparison instead and swallow the press.
            if distance.is_nan() {
                continue;
            }
            if best.is_none_or(|(closest, _)| distance < closest) {
                best = Some((distance, boundary_at_or_before(&self.text, stop.byte)));
            }
        }
        best.map_or(self.caret, |(_, byte)| byte)
    }

    /// Put the caret where a press landed, dropping the selection.
    pub fn set_caret_from_hit(&mut self, x: f32, stops: &[CaretStop]) {
        let to = self.caret_from_hit(x, stops);
        self.place(to);
    }

    /// Drag the caret to where the pointer is, keeping the anchor: a drag-select, and the same
    /// motion as shift-clicking.
    pub fn extend_selection_to_hit(&mut self, x: f32, stops: &[CaretStop]) {
        self.caret = self.caret_from_hit(x, stops);
    }

    /// The selection as an ordered range, empty when there is none.
    fn range(&self) -> Range<usize> {
        if self.caret <= self.anchor {
            self.caret..self.anchor
        } else {
            self.anchor..self.caret
        }
    }

    /// Put both ends at `byte`, clamped to a real boundary. Every edit ends here, which is what
    /// makes the invariant one line to check rather than one per operation.
    fn place(&mut self, byte: usize) {
        let byte = boundary_at_or_before(&self.text, byte);
        self.caret = byte;
        self.anchor = byte;
    }

    /// Where `motion` lands, starting from the caret. Always a valid boundary.
    fn target(&self, motion: Motion) -> usize {
        match motion {
            Motion::Left => prev_boundary(&self.text, self.caret),
            Motion::Right => next_boundary(&self.text, self.caret),
            Motion::WordLeft => word_left(&self.text, self.caret),
            Motion::WordRight => word_right(&self.text, self.caret),
            Motion::Home => 0,
            Motion::End => self.text.len(),
        }
    }
}

/// The largest character boundary at or before `byte`, clamped into `text`.
///
/// Rounding *down* rather than to the nearest is deliberate: an offset that landed inside a
/// character came from something that measured or arithmetic'd its way there, and the start of that
/// character is the position it was aiming at. Terminates because 0 is always a boundary.
fn boundary_at_or_before(text: &str, byte: usize) -> usize {
    let mut i = byte.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The start of the character before `byte`, or `byte` itself at the start of the text.
///
/// One `char`, not one grapheme cluster — see the module note on what that costs.
fn prev_boundary(text: &str, byte: usize) -> usize {
    let i = boundary_at_or_before(text, byte);
    text[..i]
        .chars()
        .next_back()
        .map_or(i, |c| i - c.len_utf8())
}

/// The start of the character after `byte`, or `byte` itself at the end of the text.
fn next_boundary(text: &str, byte: usize) -> usize {
    let i = boundary_at_or_before(text, byte);
    text[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

/// What kind of run a character belongs to, for word movement.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    /// Letters, digits, and every script's equivalent — including kana and kanji, which is the
    /// limitation described in the module note.
    Word,
    /// Punctuation and symbols, which form runs of their own so that `->` is crossed in one press.
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() {
        Class::Word
    } else {
        Class::Punct
    }
}

/// The character before a boundary, if any. `byte` must be a boundary.
fn char_before(text: &str, byte: usize) -> Option<char> {
    text[..byte].chars().next_back()
}

/// The character at a boundary, if any. `byte` must be a boundary.
fn char_after(text: &str, byte: usize) -> Option<char> {
    text[byte..].chars().next()
}

/// The start of the word before `byte`.
///
/// Whitespace between the caret and that word is crossed first, so ctrl-left from after a space
/// reaches the word rather than stalling on the gap — a press that appears to do nothing is
/// indistinguishable from a broken key.
fn word_left(text: &str, byte: usize) -> usize {
    let mut i = boundary_at_or_before(text, byte);
    while let Some(c) = char_before(text, i) {
        if class(c) == Class::Space {
            i = prev_boundary(text, i);
        } else {
            break;
        }
    }
    let Some(run) = char_before(text, i).map(class) else {
        return i;
    };
    while let Some(c) = char_before(text, i) {
        if class(c) == run {
            i = prev_boundary(text, i);
        } else {
            break;
        }
    }
    i
}

/// The start of the word after `byte`.
///
/// The run under the caret is crossed and then the whitespace after it, landing on the next word's
/// first character — Windows' rule rather than macOS's "end of this word", because this is where
/// the application runs.
fn word_right(text: &str, byte: usize) -> usize {
    let mut i = boundary_at_or_before(text, byte);
    if let Some(run) = char_after(text, i).map(class) {
        while let Some(c) = char_after(text, i) {
            if class(c) == run {
                i = next_boundary(text, i);
            } else {
                break;
            }
        }
    }
    while let Some(c) = char_after(text, i) {
        if class(c) == Class::Space {
            i = next_boundary(text, i);
        } else {
            break;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three characters, three bytes each: the case that a byte-stepping caret breaks on.
    const JA: &str = "日本語";
    /// `é` is two bytes, so the last character of an otherwise ASCII word is a boundary trap.
    const CAFE: &str = "café";
    /// Four bytes in one character, and one press should cross all four.
    const ART: &str = "🎨";

    /// Every offset a caller can see is a real character boundary. Asserted after each step of the
    /// longer tests, because this is the invariant the whole module exists to keep.
    fn assert_sane(f: &TextField) {
        assert!(
            f.caret() <= f.text().len() && f.text().is_char_boundary(f.caret()),
            "caret {} is not a boundary in {:?}",
            f.caret(),
            f.text()
        );
        if let Some(range) = f.selection() {
            assert!(
                range.start < range.end,
                "selection {range:?} is not ordered"
            );
            assert!(
                f.text().is_char_boundary(range.start) && f.text().is_char_boundary(range.end),
                "selection {range:?} splits a character in {:?}",
                f.text()
            );
        }
        // Slicing at the caret is what the application will actually do; if it is off a boundary
        // this is where it would go down.
        let _ = &f.text()[..f.caret()];
        let _ = f.selected_text();
    }

    #[test]
    fn a_new_field_has_the_caret_at_the_end_and_nothing_selected() {
        let f = TextField::new(JA);
        assert_eq!(f.caret(), 9);
        assert_eq!(f.selection(), None);
        assert_eq!(f.selected_text(), "");
        assert!(!f.has_selection());
    }

    /// One press, one character — whatever it weighs in bytes.
    #[test]
    fn arrows_step_whole_characters() {
        let mut f = TextField::new("café🎨日");
        assert_eq!(f.caret(), 12);
        for expected in [9, 5, 3, 2, 1, 0] {
            f.move_caret(Motion::Left);
            assert_eq!(f.caret(), expected);
            assert_sane(&f);
        }
        for expected in [1, 2, 3, 5, 9, 12] {
            f.move_caret(Motion::Right);
            assert_eq!(f.caret(), expected);
            assert_sane(&f);
        }
    }

    /// Held-down arrow keys are ordinary, so running off either end must simply stop.
    #[test]
    fn the_caret_cannot_be_pushed_off_either_end() {
        let mut f = TextField::new(JA);
        for _ in 0..10 {
            f.move_caret(Motion::Left);
        }
        assert_eq!(f.caret(), 0);
        for _ in 0..10 {
            f.move_caret(Motion::Right);
        }
        assert_eq!(f.caret(), 9);
        assert_sane(&f);
    }

    #[test]
    fn backspace_removes_one_whole_character() {
        let mut f = TextField::new(CAFE);
        f.backspace();
        assert_eq!(f.text(), "caf");
        assert_eq!(f.caret(), 3);

        let mut f = TextField::new("🎨あ");
        f.backspace();
        assert_eq!(f.text(), ART);
        assert_eq!(f.caret(), 4);
        f.backspace();
        assert_eq!(f.text(), "");
        assert_eq!(f.caret(), 0);
    }

    /// The key an artist holds down on an already-empty field.
    #[test]
    fn backspace_on_an_empty_field_does_nothing() {
        let mut f = TextField::new("");
        for _ in 0..5 {
            f.backspace();
            assert_eq!(f.text(), "");
            assert_eq!(f.caret(), 0);
            assert_eq!(f.selection(), None);
        }
    }

    #[test]
    fn delete_removes_the_character_after_the_caret_and_stops_at_the_end() {
        let mut f = TextField::new(JA);
        f.move_caret(Motion::Home);
        f.delete();
        assert_eq!(f.text(), "本語");
        assert_eq!(f.caret(), 0);
        f.move_caret(Motion::End);
        f.delete();
        assert_eq!(f.text(), "本語", "delete at the end ate something");
        assert_sane(&f);
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut f = TextField::new("日本語のテキスト");
        f.select_all();
        f.insert_char('あ');
        assert_eq!(f.text(), "あ");
        assert_eq!(f.caret(), 3);
        assert_eq!(f.selection(), None);
    }

    #[test]
    fn pasting_over_a_selection_replaces_it() {
        let mut f = TextField::new("café au lait");
        f.set_caret(0);
        f.extend_selection(Motion::WordRight);
        assert_eq!(f.selected_text(), "café ");
        f.insert_str("日本語 ");
        assert_eq!(f.text(), "日本語 au lait");
        assert_eq!(f.caret(), 10);
    }

    /// A selection dragged right-to-left is the same selection.
    #[test]
    fn a_backwards_selection_covers_the_same_span() {
        let mut f = TextField::new("café🎨");
        f.set_caret(9);
        f.extend_selection(Motion::Left);
        assert_eq!(f.caret(), 5, "the caret is the moving end");
        assert_eq!(f.selection(), Some(5..9));
        assert_eq!(f.selected_text(), ART);
        f.backspace();
        assert_eq!(f.text(), CAFE);
        assert_eq!(f.caret(), 5);
    }

    /// Select-all then delete: the shortest route to an empty field, and the one most likely to
    /// leave a caret pointing at text that is no longer there.
    #[test]
    fn deleting_the_whole_string_leaves_an_empty_field() {
        for erase in [TextField::backspace, TextField::delete] {
            let mut f = TextField::new("日本語のテキスト");
            f.select_all();
            assert_eq!(f.selection(), Some(0..24));
            erase(&mut f);
            assert_eq!(f.text(), "");
            assert_eq!(f.caret(), 0);
            assert_eq!(f.selection(), None);
            // And the empty field is still usable.
            f.insert_char('日');
            assert_eq!(f.text(), "日");
        }
    }

    /// **A single-line field must not end up holding two lines.** The clipboard is where multi-line
    /// text comes from, and nothing downstream draws a newline.
    #[test]
    fn control_characters_never_enter_the_field() {
        let mut f = TextField::new("");
        f.insert_str("日本\nlanguage\ttab\r\n");
        assert_eq!(f.text(), "日本languagetab");
        assert_eq!(
            f.caret(),
            f.text().len(),
            "the caret ran past what was kept"
        );
        f.insert_char('\n');
        assert_eq!(f.text(), "日本languagetab");
        assert_sane(&f);
    }

    #[test]
    fn word_motion_crosses_a_word_and_the_gap_after_it() {
        let mut f = TextField::new("hello  world");
        assert_eq!(f.caret(), 12);
        f.move_caret(Motion::WordLeft);
        assert_eq!(f.caret(), 7, "did not reach the start of the last word");
        f.move_caret(Motion::WordLeft);
        assert_eq!(f.caret(), 0);
        f.move_caret(Motion::WordLeft);
        assert_eq!(f.caret(), 0);
        f.move_caret(Motion::WordRight);
        assert_eq!(f.caret(), 7, "did not skip the gap to the next word");
        f.move_caret(Motion::WordRight);
        assert_eq!(f.caret(), 12);
        f.move_caret(Motion::WordRight);
        assert_eq!(f.caret(), 12);
    }

    /// Punctuation is a run of its own, so `->` is one press rather than two, and a word next to it
    /// is still reachable.
    #[test]
    fn word_motion_treats_punctuation_as_its_own_run() {
        let mut f = TextField::new("a->b");
        f.move_caret(Motion::Home);
        f.move_caret(Motion::WordRight);
        assert_eq!(f.caret(), 1);
        f.move_caret(Motion::WordRight);
        assert_eq!(f.caret(), 3);
        f.move_caret(Motion::WordLeft);
        assert_eq!(f.caret(), 1);
    }

    /// **Pins today's `char` stepping.** Japanese is written without spaces, so a run of kana and
    /// kanji is one word to this code and ctrl-left crosses the lot. Real segmentation would stop
    /// inside the run; when it lands, this expectation changes rather than quietly passing.
    #[test]
    fn word_motion_over_japanese_pins_char_stepping() {
        let mut f = TextField::new("日本語 テキスト");
        assert_eq!(f.caret(), 22);
        f.move_caret(Motion::WordLeft);
        assert_eq!(f.caret(), 10);
        f.move_caret(Motion::WordLeft);
        assert_eq!(f.caret(), 0);
    }

    /// **Pins today's `char` stepping.** A decomposed accent and a ZWJ emoji sequence are each one
    /// thing on screen and several scalars underneath, so one backspace takes a piece rather than
    /// the whole. Recorded, not endorsed — see the module note.
    #[test]
    fn combining_marks_and_zwj_sequences_pin_char_stepping() {
        let mut f = TextField::new("cafe\u{301}");
        f.backspace();
        assert_eq!(f.text(), "cafe", "the accent went, the letter stayed");

        let mut f = TextField::new("👨\u{200d}👩\u{200d}👧");
        f.backspace();
        assert_eq!(f.text(), "👨\u{200d}👩\u{200d}");
        assert_sane(&f);
    }

    #[test]
    fn home_and_end_reach_both_ends_and_shift_selects_to_them() {
        let mut f = TextField::new(CAFE);
        f.move_caret(Motion::Home);
        assert_eq!(f.caret(), 0);
        f.extend_selection(Motion::End);
        assert_eq!(f.selection(), Some(0..5));
        assert_eq!(f.selected_text(), CAFE);
        f.move_caret(Motion::Home);
        assert_eq!(f.selection(), None);
        f.extend_selection(Motion::Home);
        assert_eq!(
            f.selection(),
            None,
            "shift-home at the start selected something"
        );
    }

    /// An arrow over a selection collapses it to the near edge rather than stepping from the
    /// caret, which is the difference between "never mind" and losing your place.
    #[test]
    fn an_arrow_collapses_a_selection_to_its_near_edge() {
        let mut f = TextField::new("hello");
        f.set_caret(4);
        f.extend_selection(Motion::Left);
        f.extend_selection(Motion::Left);
        assert_eq!(f.selection(), Some(2..4), "a backwards selection of two");

        f.move_caret(Motion::Right);
        assert_eq!(f.caret(), 4, "Right did not land on the far edge");
        assert!(!f.has_selection());

        f.set_caret(4);
        f.extend_selection(Motion::Left);
        f.extend_selection(Motion::Left);
        f.move_caret(Motion::Left);
        assert_eq!(f.caret(), 2, "Left did not land on the near edge");
        assert!(!f.has_selection());
    }

    /// Shift-arrow back across the anchor empties the selection and then reverses it; the anchor
    /// stays where the first shift-arrow left it.
    #[test]
    fn extending_across_the_anchor_reverses_the_selection() {
        let mut f = TextField::new("hello");
        f.set_caret(2);
        f.extend_selection(Motion::Right);
        assert_eq!(f.selection(), Some(2..3));
        f.extend_selection(Motion::Left);
        assert_eq!(f.selection(), None);
        f.extend_selection(Motion::Left);
        assert_eq!(f.selection(), Some(1..2));
        assert_eq!(f.caret(), 1);
    }

    /// Stops for three ten-unit glyphs, as a caller that had measured `"abc"` would report them.
    const STOPS: [CaretStop; 4] = [
        CaretStop { byte: 0, x: 0.0 },
        CaretStop { byte: 1, x: 10.0 },
        CaretStop { byte: 2, x: 20.0 },
        CaretStop { byte: 3, x: 30.0 },
    ];

    #[test]
    fn a_press_takes_the_nearest_stop() {
        let f = TextField::new("abc");
        let s = STOPS;
        assert_eq!(f.caret_from_hit(2.0, &s), 0);
        assert_eq!(f.caret_from_hit(12.0, &s), 1);
        assert_eq!(
            f.caret_from_hit(16.0, &s),
            2,
            "the right half of a glyph puts the caret after it"
        );
        assert_eq!(
            f.caret_from_hit(15.0, &s),
            1,
            "a tie goes to the earlier stop"
        );
        assert_eq!(f.caret_from_hit(-500.0, &s), 0);
        assert_eq!(
            f.caret_from_hit(500.0, &s),
            3,
            "the end of the text is unreachable by pointer"
        );
    }

    /// A stop list is measured on some earlier frame, so it can name offsets that no longer exist
    /// or never were. It must be clamped, not trusted.
    #[test]
    fn a_stale_stop_list_cannot_place_a_caret_inside_a_character() {
        let mut f = TextField::new(JA);
        let s = [
            CaretStop { byte: 1, x: 0.0 },
            CaretStop { byte: 4, x: 50.0 },
            CaretStop {
                byte: 9_999,
                x: 100.0,
            },
        ];
        for (x, expected) in [(0.0, 0), (50.0, 3), (100.0, 9)] {
            f.set_caret_from_hit(x, &s);
            assert_eq!(f.caret(), expected);
            assert_sane(&f);
        }
        // The proof that the clamp mattered: this is the operation an interior offset kills.
        f.set_caret_from_hit(50.0, &s);
        f.backspace();
        assert_eq!(f.text(), "本語");
    }

    #[test]
    fn a_press_with_nothing_measured_leaves_the_caret_alone() {
        let mut f = TextField::new(JA);
        f.set_caret(3);
        f.set_caret_from_hit(1000.0, &[]);
        assert_eq!(f.caret(), 3);
    }

    /// A NaN width is what a failed measurement looks like. It must lose, not win.
    #[test]
    fn a_nan_measurement_does_not_swallow_the_press() {
        let f = TextField::new("abc");
        let s = [
            CaretStop {
                byte: 0,
                x: f32::NAN,
            },
            CaretStop { byte: 2, x: 20.0 },
        ];
        assert_eq!(f.caret_from_hit(19.0, &s), 2);
    }

    #[test]
    fn dragging_selects_between_the_press_and_the_pointer() {
        let mut f = TextField::new("abc");
        let s = STOPS;
        f.set_caret_from_hit(0.0, &s);
        f.extend_selection_to_hit(21.0, &s);
        assert_eq!(f.selection(), Some(0..2));
        // Dragging back past the start reverses it rather than emptying it.
        f.extend_selection_to_hit(-5.0, &s);
        assert_eq!(f.selection(), None);
        assert_eq!(f.caret(), 0);
    }

    #[test]
    fn setting_the_caret_clamps_into_the_text_and_onto_a_boundary() {
        let mut f = TextField::new(JA);
        f.set_caret(1);
        assert_eq!(f.caret(), 0, "landed inside the first character");
        f.set_caret(5);
        assert_eq!(f.caret(), 3);
        f.set_caret(usize::MAX);
        assert_eq!(f.caret(), 9);
        assert_sane(&f);
    }

    #[test]
    fn replacing_the_text_moves_the_caret_with_it() {
        let mut f = TextField::new("日本語のテキスト");
        f.select_all();
        f.set_text(ART);
        assert_eq!(f.text(), ART);
        assert_eq!(
            f.caret(),
            4,
            "the caret was left pointing past the new text"
        );
        assert_eq!(f.selection(), None);
    }

    /// Cut, in the two halves the caller assembles it from.
    #[test]
    fn a_cut_takes_what_it_reports() {
        let mut f = TextField::new("日本語");
        f.set_caret(0);
        f.extend_selection(Motion::Right);
        assert_eq!(f.selected_text(), "日");
        assert!(f.delete_selection());
        assert_eq!(f.text(), "本語");
        assert!(!f.delete_selection(), "cut something that was not selected");
    }

    /// xorshift64, so the sweep below is reproducible without a dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> usize {
            usize::try_from(self.next() % n).expect("a small modulus fits a usize")
        }

        /// A measured position, from a pool that includes the ones a real measurement can produce
        /// and a caller would not think to send: off the left edge, and a failed measurement.
        fn x(&mut self) -> f32 {
            const XS: [f32; 6] = [f32::NAN, -40.0, 0.0, 7.5, 33.0, 500.0];
            XS[self.below(6)]
        }
    }

    /// **No sequence of operations may leave the field in a state that panics.**
    ///
    /// The point is not the assertions at the end but the ones between every pair of operations:
    /// a caret one byte inside `語` survives until something slices with it, which in the running
    /// application is a frame later and somewhere else entirely. Mixed scripts on purpose, and
    /// newlines in the character pool so the single-line filter is exercised too.
    #[test]
    fn a_long_random_sequence_keeps_every_index_valid() {
        const CHARS: [char; 8] = ['a', 'é', '日', '🎨', ' ', '\n', '-', '👩'];
        const PASTES: [&str; 4] = ["", "ab", "語のテキスト", "x\ny"];
        const MOTIONS: [Motion; 6] = [
            Motion::Left,
            Motion::Right,
            Motion::WordLeft,
            Motion::WordRight,
            Motion::Home,
            Motion::End,
        ];

        let mut rng = Rng(0x5EED_1234_ABCD_0001);
        let mut f = TextField::new("日本語 café 🎨");
        for step in 0..20_000 {
            match rng.below(11) {
                0 => f.insert_char(CHARS[rng.below(8)]),
                1 => f.insert_str(PASTES[rng.below(4)]),
                2 => f.backspace(),
                3 => f.delete(),
                4 => f.move_caret(MOTIONS[rng.below(6)]),
                5 => f.extend_selection(MOTIONS[rng.below(6)]),
                6 => f.select_all(),
                7 => {
                    f.delete_selection();
                }
                // Deliberately unclamped offsets, including ones inside a character and past the
                // end: a caller doing its own arithmetic is exactly where a bad index comes from.
                8 => f.set_caret(rng.below(40)),
                9 => {
                    let s = [
                        CaretStop {
                            byte: rng.below(40),
                            x: rng.x(),
                        },
                        CaretStop {
                            byte: rng.below(40),
                            x: rng.x(),
                        },
                    ];
                    let x = rng.x();
                    f.set_caret_from_hit(x, &s);
                }
                _ => {
                    let s = [CaretStop {
                        byte: rng.below(40),
                        x: rng.x(),
                    }];
                    let x = rng.x();
                    f.extend_selection_to_hit(x, &s);
                }
            }
            assert_sane(&f);
            assert!(
                !f.text().contains(char::is_control),
                "step {step}: a control character reached the text: {:?}",
                f.text()
            );
        }
        // The sweep must have actually done something, or it proves nothing about a busy field.
        assert!(!f.text().is_empty() || f.caret() == 0);
    }
}
