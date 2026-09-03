//! The questions the application has to ask before anything else can happen.
//!
//! Two of them: work found from a session that did not close, and unsaved changes about to be
//! thrown away. Neither belongs to a panel — they are the *application* speaking — but they are
//! described here the same way every panel describes itself, as a list of
//! [`Control`](crate::panel_ui::Control)s. So the prompt is drawn by the same layer, in the same
//! theme, and lands in the same control atlas as everything else a scenario can press.
//!
//! **These were the last of the old UI.** They were drawn with raw `egui::Window`s and buttons
//! long after every panel had been ported, which meant they looked like nothing else on screen,
//! ignored the theme, and had to be written into the atlas by hand through a separate reporting
//! path. A prompt that blocks every pen stroke should be the easiest thing in the application to
//! press on purpose, not the one thing kept outside the machinery that makes that possible.

use crate::panel_ui::{Control, ControlId};

/// What the application is asking.
///
/// **A question is not always a yes or a no.** The export modal is the third of these and the
/// first with anything to set, which is what [`Ask::body`] is for: the same descriptor layer, the
/// same frame, the same modality, with the sliders and choices of a panel in the middle of it.
/// The alternative was a second kind of window with its own rules about what owns the pointer,
/// which is the mistake this whole file exists to undo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask<'a> {
    /// Work was found from a session that did not close. `what` names it.
    Recovered(&'a str),
    /// The document has changes that are in no file, and `what` is about to discard them —
    /// "close", "open another file".
    Unsaved(&'a str),
    /// Where the artwork is going, and how much of it.
    Export {
        choices: &'a crate::export::Choices,
        pages: usize,
        page: (u32, u32),
    },
}

/// What the artist answered.
///
/// One enum for both prompts, because the shell has one question to ask of an answer — what to do
/// next — and two enums would mean two places to forget a case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Open the recovered work.
    Recover,
    /// Throw the recovered work away.
    DiscardRecovered,
    /// Save, then do the thing that was asked for.
    SaveFirst,
    /// Do it and lose the changes.
    DiscardChanges,
    /// Do nothing.
    Cancel,
    /// Go ahead with the export as it is set.
    Export,
    /// Put the export dialog away and export nothing.
    StopExport,
}

/// A button in a prompt: what it answers to, what it says, and what it means.
///
/// **One row, not three lists.** A button drawn in one place and answered in another is two
/// descriptions of the same set, and the half that goes stale is always the answering half — a
/// button that draws, presses, lights up and does nothing at all. That is the worst outcome this
/// file can produce (DECISIONS §6b), and the shape below makes it unspellable: `controls` and
/// `answer` read the same rows.
struct Choice {
    id: ControlId,
    text: &'static str,
    answer: Answer,
}

/// Ids for the prompt's own controls.
///
/// Distinct across both prompts rather than each starting at zero. The two are not meant to be up
/// at once, but "not meant to" is not a guarantee, and ids that collide would make the answer
/// depend on which prompt was asked first.
const RECOVER: ControlId = 1;
const DISCARD_RECOVERED: ControlId = 2;
const SAVE_FIRST: ControlId = 3;
const DISCARD_CHANGES: ControlId = 4;
const CANCEL: ControlId = 5;
const EXPORT: ControlId = 6;
const STOP_EXPORT: ControlId = 7;

/// Recovering first, and leftmost, because it is the answer that loses nothing.
const RECOVERED: &[Choice] = &[
    Choice {
        id: RECOVER,
        text: "Recover",
        answer: Answer::Recover,
    },
    Choice {
        // **It says what it discards.** Two prompts with a button labelled "Discard" are two
        // buttons the artist has to work out from context and a scenario cannot tell apart by
        // name — and the one telling them apart wrongly throws away a day's drawing.
        id: DISCARD_RECOVERED,
        text: "Discard recovered work",
        answer: Answer::DiscardRecovered,
    },
];

/// Going ahead first, because the artist opened this dialog in order to press it.
///
/// Nothing here is destructive -- an export writes a new file and touches no artwork -- so the
/// "leftmost is safest" rule that orders the other two has nothing to decide, and the button
/// somebody came for goes first instead.
const EXPORTING: &[Choice] = &[
    Choice {
        id: EXPORT,
        text: "Export",
        answer: Answer::Export,
    },
    Choice {
        id: STOP_EXPORT,
        text: "Not now",
        answer: Answer::StopExport,
    },
];

/// Saving first, and leftmost, for the same reason.
const UNSAVED: &[Choice] = &[
    Choice {
        id: SAVE_FIRST,
        text: "Save first",
        answer: Answer::SaveFirst,
    },
    Choice {
        id: DISCARD_CHANGES,
        text: "Discard changes",
        answer: Answer::DiscardChanges,
    },
    Choice {
        id: CANCEL,
        text: "Cancel",
        answer: Answer::Cancel,
    },
];

impl Ask<'_> {
    /// The prompt's title, in the theme's own words rather than a window caption.
    #[must_use]
    pub fn title(&self) -> &'static str {
        match self {
            Self::Recovered(_) => "Recovered work",
            Self::Unsaved(_) => "Unsaved changes",
            Self::Export { .. } => "Export",
        }
    }

    /// What the prompt says, above its buttons.
    ///
    /// Sentences, not a caption: a prompt that says only "Unsaved changes" leaves the artist to
    /// guess what each button will do to them.
    #[must_use]
    pub fn words(&self) -> Vec<Control> {
        match *self {
            Self::Recovered(what) => vec![
                Control::Label {
                    text: "OpenPaint closed with unsaved changes.".to_owned(),
                },
                Control::Label {
                    text: what.to_owned(),
                },
                Control::Separator,
                Control::Label {
                    text: "Recovering opens it as unsaved work pointed at the original file, so \
                           nothing is overwritten until you save."
                        .to_owned(),
                },
            ],
            Self::Unsaved(what) => vec![
                Control::Label {
                    text: format!(
                        "This document has changes that are not in a file. Save before you \
                         {what}?"
                    ),
                },
                Control::Separator,
                Control::Label {
                    text: "Enter saves, Escape cancels.".to_owned(),
                },
            ],
            // The dialog's own words are the size line under its controls, which `body` builds
            // from the same numbers the export will use. Repeating them above would be the same
            // sentence twice, which `panel_ui::place` refuses outright.
            Self::Export { .. } => vec![Control::Label {
                text: "You will be asked where to put it.".to_owned(),
            }],
        }
    }

    /// The controls in the middle: what this question has to set before it can be answered.
    ///
    /// Empty for a question that is only a question. A modal with nothing here draws its words
    /// straight above its buttons and looks exactly as it did before this existed.
    #[must_use]
    pub fn body(&self) -> Vec<Control> {
        match *self {
            Self::Recovered(_) | Self::Unsaved(_) => Vec::new(),
            Self::Export {
                choices,
                pages,
                page,
            } => crate::export::controls(choices, pages, page),
        }
    }

    /// The buttons, in the order they are offered.
    #[must_use]
    pub fn answers(&self) -> Vec<Control> {
        self.choices()
            .iter()
            .map(|c| Control::Button {
                id: c.id,
                text: c.text.to_owned(),
            })
            .collect()
    }

    /// What pressing a control means, or nothing if it is not one of this prompt's.
    #[must_use]
    pub fn answer(&self, id: ControlId) -> Option<Answer> {
        self.choices().iter().find(|c| c.id == id).map(|c| c.answer)
    }

    fn choices(&self) -> &'static [Choice] {
        match self {
            Self::Recovered(_) => RECOVERED,
            Self::Unsaved(_) => UNSAVED,
            Self::Export { .. } => EXPORTING,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every question the application can ask, so a new one cannot be added without the rules
    /// below being asked of it. A `match` in `title` would make the compiler insist on the
    /// drawing; nothing would insist on the *rules*, and this is that.
    fn every_ask() -> Vec<Ask<'static>> {
        // A leaked default rather than a field: `Ask::Export` borrows the choices the shell
        // holds, and a test has nowhere to hold one that outlives the call.
        let choices: &'static crate::export::Choices =
            Box::leak(Box::new(crate::export::Choices::default()));
        vec![
            Ask::Recovered("sketch.openpaint"),
            Ask::Unsaved("close"),
            Ask::Export {
                choices,
                pages: 3,
                page: (1200, 1600),
            },
        ]
    }

    /// Every button offered has an answer, and every answer is reachable by pressing something.
    ///
    /// **The test the old prompts could not have.** They pushed buttons in one place and matched
    /// on `clicked()` in another, so a button added without its arm was a button that did nothing
    /// — and nothing on screen would say so. Sabotage check: delete the `answer` arm for any
    /// button by removing its row, and it stops being drawn as well, which is a visibly missing
    /// button rather than a dead one.
    #[test]
    fn every_button_in_a_prompt_answers_something() {
        for ask in every_ask() {
            let offered = ask.answers();
            assert!(!offered.is_empty(), "{ask:?} offers no way out");
            for control in &offered {
                let id = control.id().expect("a button has an id");
                assert!(
                    ask.answer(id).is_some(),
                    "{ask:?} draws {control:?} and has no answer for it"
                );
            }
        }
    }

    /// A prompt never answers for a control that is not its own.
    ///
    /// Both prompts' ids come out of one range, so this is what stops the recovery prompt
    /// answering "Cancel" — an answer it does not offer and the shell would not know what to do
    /// with.
    #[test]
    fn a_prompt_answers_only_for_its_own_buttons() {
        for asked in every_ask() {
            for other in every_ask() {
                if other == asked {
                    continue;
                }
                for control in other.answers() {
                    let id = control.id().expect("a button has an id");
                    assert_eq!(
                        asked.answer(id),
                        None,
                        "{asked:?} answered for {other:?}'s {control:?}"
                    );
                }
            }
        }
    }

    /// A modal's body and its buttons never share an id either.
    ///
    /// They are drawn as two lists in two rectangles and read back as one set of changes, so an
    /// id used in both would make pressing a choice answer the question -- the export dialog
    /// closing the moment you picked "every page".
    #[test]
    fn nothing_in_a_body_answers_a_button() {
        for ask in every_ask() {
            for control in ask.body() {
                let Some(id) = control.id() else { continue };
                assert_eq!(
                    ask.answer(id),
                    None,
                    "{ask:?} has {control:?} in its body and answers to that id"
                );
            }
        }
    }

    /// The answer that loses nothing is offered first.
    ///
    /// A convention worth holding to rather than a preference: the artist reaching without reading
    /// hits the leftmost button, and it must never be the one that destroys their work.
    #[test]
    fn the_safe_answer_comes_first() {
        assert_eq!(
            Ask::Recovered("x").choices().first().map(|c| c.answer),
            Some(Answer::Recover)
        );
        assert_eq!(
            Ask::Unsaved("close").choices().first().map(|c| c.answer),
            Some(Answer::SaveFirst)
        );
    }

    /// No two buttons anywhere in the prompts are called the same thing.
    ///
    /// The atlas names a control by what it says, so two buttons sharing a name are two buttons a
    /// scenario cannot tell apart — and the harness refuses an ambiguous name rather than guessing,
    /// which once cost the artist their saved brushes. Both prompts' buttons once said "Discard".
    #[test]
    fn no_two_buttons_say_the_same_thing() {
        let mut names: Vec<&str> = RECOVERED
            .iter()
            .chain(UNSAVED)
            .chain(EXPORTING)
            .map(|c| c.text)
            .collect();
        let all = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all, "two prompt buttons share a name");
    }

    /// Each prompt says what it is about, in words, above its buttons.
    #[test]
    fn a_prompt_says_what_it_is_about() {
        for ask in every_ask() {
            let said: Vec<String> = ask
                .words()
                .iter()
                .filter_map(|c| match c {
                    Control::Label { text } => Some(text.clone()),
                    _ => None,
                })
                .collect();
            assert!(!said.is_empty(), "{ask:?} shows no words at all");
            assert!(
                said.iter().any(|s| s.ends_with('.') || s.ends_with('?')),
                "{ask:?} says only fragments: {said:?}"
            );
        }
        // The thing at stake is named, or "recovered work" is a file the artist cannot identify.
        let said = Ask::Recovered("sketch.openpaint").words();
        assert!(
            said.iter().any(|c| matches!(
                c,
                Control::Label { text } if text.contains("sketch.openpaint")
            )),
            "the recovery prompt does not name the work it found"
        );
    }
}
