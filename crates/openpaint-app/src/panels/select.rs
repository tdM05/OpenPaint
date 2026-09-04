//! How the selection tools behave.
//!
//! One module per panel, exporting one function. See [`super`] for why.
//!
//! **This panel is the settings, not the tools.** The four tools are in the rail and All, Deselect,
//! Invert and Clear are in the Select menu, so the only thing here with nowhere else to live is the
//! wand's three numbers -- plus the two commands that act on a selection once there is one.
//!
//! The tools are offered here as well, and the duplication is deliberate: a settings panel that
//! cannot switch the thing it is the settings for makes you cross the window to change tool and
//! come back to change its tolerance. The rail and this panel read the same field and answer the
//! same [`Picked::Select`], so there is one tool and two ways to reach it, not two states to keep
//! in step.

use super::{Painting, Picked};
use crate::icons::Symbol;
use crate::panel_ui::{Change, Control, ControlId};
use crate::ui::{SelectAction, SelectTool, Status, WandSettings};
use crate::workspace::Place;
use openpaint_core::Brush;

/// The selection tools, in the order the rail offers them, so the two never disagree about which
/// tool a position means.
const TOOLS: [(&str, Symbol, SelectTool); 4] = [
    ("Lasso", Symbol::Lasso, SelectTool::Lasso),
    ("Rect", Symbol::RectSelect, SelectTool::Rect),
    ("Wand", Symbol::Wand, SelectTool::Wand),
    ("Move", Symbol::MoveSelection, SelectTool::Move),
];

// Tools are numbered by their position in `TOOLS`, so everything else starts above any position
// that table could reach. A setting and a tool sharing an id would be a silent mis-hit.
const FIRST_SETTING: ControlId = 1 << 20;
const TOLERANCE: ControlId = FIRST_SETTING;
const EXPAND: ControlId = FIRST_SETTING + 1;
const FILL_ON_CLICK: ControlId = FIRST_SETTING + 2;
const FIRST_COMMAND: ControlId = 1 << 21;
const FILL: ControlId = FIRST_COMMAND;
const CLEAR: ControlId = FIRST_COMMAND + 1;

/// The widest difference from the clicked colour the wand will accept.
///
/// Half of full range rather than all of it: past about 128 every channel matches something and
/// the region is the whole layer, so the top half of the track would set the same answer twice.
const TOLERANCE_MAX: f32 = 128.0;
/// How far the region may be grown afterwards. Eight pixels covers the ramp any soft brush leaves;
/// beyond that it is not tucking under an edge, it is a different selection.
const EXPAND_MAX: f32 = 8.0;

/// Draw the panel and report what the artist asked for.
pub(crate) fn show(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    color_srgb: &mut [u8; 3],
    state: &Status<'_>,
    paint: &mut Painting<'_>,
    place: Place,
) -> Option<Picked> {
    let _ = (&mut *brush, &mut *color_srgb);
    // **Nothing here opens a list, so there is no popup half to draw** -- and this returns rather
    // than ignoring `place`, which is the difference between dead code and a defect now that a
    // contextual panel shows this section. The Tool options panel can own an open popup that the
    // *brush* section asked for, and then change hands the instant a selection tool goes up: the
    // workspace goes on asking that panel to draw its popup, and a section that ignored `place`
    // would draw its whole self into the dropdown box. Every other section already returns here.
    if place == Place::Popup {
        return None;
    }
    let controls = controls(state.select_tool, state.has_selection, state.wand);
    let mut picked: Option<Picked> = None;
    for change in paint.show(ui, &controls) {
        picked = picked_for(&change, state.wand);
    }
    picked
}

/// What the panel shows, for a given tool and a given selection.
///
/// Split out from [`show`] because deciding *what is offered* is the whole of this panel's
/// judgement -- which commands are refusable, which settings belong to which tool -- and it is
/// worth checking without a window.
fn controls(
    select_tool: Option<SelectTool>,
    has_selection: bool,
    wand: WandSettings,
) -> Vec<Control> {
    let mut controls: Vec<Control> = TOOLS
        .iter()
        .enumerate()
        .map(|(i, (name, symbol, tool))| Control::Choice {
            id: u32::try_from(i).unwrap_or(u32::MAX),
            text: (*name).to_owned(),
            icon: Some(*symbol),
            selected: select_tool == Some(*tool),
        })
        .collect();
    controls.push(Control::Separator);

    match select_tool {
        Some(SelectTool::Wand) => {
            controls.push(Control::Slider {
                id: TOLERANCE,
                text: "Tolerance".to_owned(),
                value: f32::from(wand.tolerance),
                min: 0.0,
                max: TOLERANCE_MAX,
                // A count of levels, not a distance or a proportion, so there is no unit that
                // would not be a lie.
                unit: "",
                // Linear: the whole range is interesting. A tolerance of 4 and one of 8 are not a
                // step apart the way brush radii are, which is the case logarithmic is for.
                log: false,
            });
            controls.push(Control::Slider {
                id: EXPAND,
                text: "Expand".to_owned(),
                value: wand.expand as f32,
                min: 0.0,
                max: EXPAND_MAX,
                unit: "px",
                log: false,
            });
            controls.push(Control::Toggle {
                id: FILL_ON_CLICK,
                text: "Fill on click (bucket)".to_owned(),
                on: wand.fill_on_click,
            });
            // One sentence, one label. It was three for a while, because a label used to be one
            // row tall whatever it said and a long line was drawn clipped at the panel's edge --
            // which read as three paragraphs and came undone at any other width. Labels wrap now.
            controls.push(Control::Label {
                text: "Tolerance is how far up an anti-aliased edge still counts as the region; Expand tucks the result under the ink so no pale fringe is left. With Fill off the wand leaves a selection instead."
                    .to_owned(),
            });
        }
        // A tool with nothing to set says so. The alternative is a gap where the wand's settings
        // were, and a gap is indistinguishable from a panel that failed to draw (DECISIONS 6b).
        Some(tool) => {
            let name = TOOLS
                .iter()
                .find(|(_, _, t)| *t == tool)
                .map_or("This tool", |(name, _, _)| *name);
            controls.push(Control::Label {
                text: format!("{name} has nothing to set."),
            });
        }
        None => controls.push(Control::Label {
            text: "No selection tool is on.".to_owned(),
        }),
    }

    controls.push(Control::Separator);
    // **Offered only when there is something to act on**, where the old panel greyed them out.
    //
    // There is no disabled control to describe here, and inventing one would be a control whose
    // job is to refuse -- the thing DECISIONS 6b rules out, and the same call the Layer menu
    // already makes about deleting the last layer.
    //
    // The old panel's own reason for keeping them visible was that the commands should not move
    // around. That was already lost above: the wand's four rows appear and disappear, so nothing
    // below them has a fixed position anyway. What the reason was really protecting is *knowing
    // the commands exist*, and the label does that better than greying ever did -- it names both
    // of them and says what they are waiting for.
    if has_selection {
        for (id, text) in [(FILL, "Fill with brush colour"), (CLEAR, "Clear")] {
            controls.push(Control::Button {
                id,
                text: text.to_owned(),
            });
        }
    } else {
        controls.push(Control::Label {
            text: "Fill and Clear need a selection.".to_owned(),
        });
    }
    controls
}

/// What one change from this panel means.
///
/// `wand` is the settings as they stand, because the three wand controls answer as one
/// [`Picked::Wand`]: the tool has one set of preferences and they are handed back whole, so a
/// receiver never has to merge two halves of a struct and cannot get the merge wrong.
fn picked_for(change: &Change, wand: WandSettings) -> Option<Picked> {
    match *change {
        Change::Chose(i) => TOOLS
            .get(i as usize)
            .map(|(_, _, tool)| Picked::Select(*tool)),
        // Rounded, not truncated: a knob sitting a hair below 96 should read 96, and a cast that
        // floors makes the top of the track unreachable. Already held inside the range by
        // `from_fraction`, which is the one place a slider's range lives.
        Change::Set(TOLERANCE, v) => Some(Picked::Wand(WandSettings {
            tolerance: v.round() as u8,
            ..wand
        })),
        Change::Set(EXPAND, v) => Some(Picked::Wand(WandSettings {
            expand: v.round() as u32,
            ..wand
        })),
        Change::Toggled(FILL_ON_CLICK, on) => Some(Picked::Wand(WandSettings {
            fill_on_click: on,
            ..wand
        })),
        Change::Pressed(FILL) => Some(Picked::Selection(SelectAction::Fill)),
        Change::Pressed(CLEAR) => Some(Picked::Selection(SelectAction::Clear)),
        ref other => {
            eprintln!("select panel: unexpected {other:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings deliberately unlike the defaults, so a field quietly rebuilt from `Default`
    /// instead of carried through shows up as a wrong number rather than a right one.
    fn wand() -> WandSettings {
        WandSettings {
            tolerance: 10,
            expand: 5,
            fill_on_click: false,
        }
    }

    fn ids(controls: &[Control]) -> Vec<ControlId> {
        controls.iter().filter_map(Control::id).collect()
    }

    fn labels(controls: &[Control]) -> String {
        controls
            .iter()
            .filter_map(|c| match c {
                Control::Label { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// **Only the wand has settings, and they are there exactly when it is.**
    ///
    /// Three controls belonging to one tool, shown under a different one, would be three controls
    /// that appear to do nothing.
    #[test]
    fn the_wands_settings_appear_only_under_the_wand() {
        for tool in [
            None,
            Some(SelectTool::Lasso),
            Some(SelectTool::Rect),
            Some(SelectTool::Move),
        ] {
            let ids = ids(&controls(tool, true, wand()));
            for id in [TOLERANCE, EXPAND, FILL_ON_CLICK] {
                assert!(!ids.contains(&id), "{tool:?} offers a wand setting");
            }
        }
        let ids = ids(&controls(Some(SelectTool::Wand), true, wand()));
        for id in [TOLERANCE, EXPAND, FILL_ON_CLICK] {
            assert!(ids.contains(&id), "the wand is missing a setting");
        }
    }

    /// The controls show what the settings actually are, over the range the wand accepts.
    ///
    /// A slider drawn from `Default` rather than from the live settings looks right on the first
    /// frame and then springs back every time the panel is rebuilt.
    #[test]
    fn the_controls_show_the_settings_they_are_for() {
        let shown = controls(Some(SelectTool::Wand), false, wand());
        let slider = |id: ControlId| {
            shown
                .iter()
                .find_map(|c| match c {
                    Control::Slider {
                        id: i,
                        value,
                        min,
                        max,
                        log,
                        ..
                    } if *i == id => Some((*value, *min, *max, *log)),
                    _ => None,
                })
                .expect("the slider")
        };
        assert_eq!(slider(TOLERANCE), (10.0, 0.0, TOLERANCE_MAX, false));
        assert_eq!(slider(EXPAND), (5.0, 0.0, EXPAND_MAX, false));
        assert!(
            shown.iter().any(
                |c| matches!(c, Control::Toggle { id, on, .. } if *id == FILL_ON_CLICK && !*on)
            ),
            "the toggle disagrees with the setting"
        );
    }

    /// **A command that would be refused is not offered** -- and the panel says so rather than
    /// leaving a hole where two buttons used to be (DECISIONS 6b).
    #[test]
    fn fill_and_clear_are_offered_only_when_there_is_a_selection() {
        for tool in [None, Some(SelectTool::Wand)] {
            let with = ids(&controls(tool, true, wand()));
            assert!(with.contains(&FILL) && with.contains(&CLEAR), "{tool:?}");

            let without = controls(tool, false, wand());
            let ids = ids(&without);
            assert!(!ids.contains(&FILL) && !ids.contains(&CLEAR), "{tool:?}");
            let said = labels(&without);
            assert!(
                said.contains("Fill") && said.contains("Clear"),
                "{tool:?}: the commands vanished without a word: {said}"
            );
        }
    }

    /// **No state of this panel is silent.** An empty panel, or one whose buttons depend on a tool
    /// it does not name, cannot be told from one that failed to draw.
    #[test]
    fn every_state_says_something_and_offers_the_tools() {
        for tool in [
            None,
            Some(SelectTool::Lasso),
            Some(SelectTool::Rect),
            Some(SelectTool::Wand),
            Some(SelectTool::Move),
        ] {
            for has_selection in [true, false] {
                let shown = controls(tool, has_selection, wand());
                assert!(
                    !labels(&shown).is_empty(),
                    "{tool:?}/{has_selection}: nothing said"
                );
                // The four tools, always, so the panel is worth opening on its own.
                let chosen: Vec<bool> = shown
                    .iter()
                    .filter_map(|c| match c {
                        Control::Choice { selected, .. } => Some(*selected),
                        _ => None,
                    })
                    .collect();
                assert_eq!(chosen.len(), TOOLS.len(), "{tool:?}: a tool went missing");
                assert_eq!(
                    chosen.iter().filter(|on| **on).count(),
                    usize::from(tool.is_some()),
                    "{tool:?}: the rail and the panel disagree about what is on"
                );
            }
        }
    }

    /// A tool choice asks for that tool, by position in the same table the labels came from.
    #[test]
    fn choosing_a_tool_asks_for_that_tool() {
        for (i, (_, _, tool)) in TOOLS.iter().enumerate() {
            let id = u32::try_from(i).expect("four tools");
            assert_eq!(
                picked_for(&Change::Chose(id), wand()),
                Some(Picked::Select(*tool))
            );
        }
        // A position no tool occupies asks for nothing, rather than for the first tool.
        assert_eq!(picked_for(&Change::Chose(99), wand()), None);
    }

    /// **A wand control changes its own field and nothing else.**
    ///
    /// The three settings come back as one struct, so every answer has to carry the other two
    /// unchanged -- rebuild it from `Default` and moving the tolerance silently turns the bucket
    /// back on.
    #[test]
    fn a_wand_control_changes_one_field_and_carries_the_rest() {
        let was = wand();
        assert_eq!(
            picked_for(&Change::Set(TOLERANCE, 96.4), was),
            Some(Picked::Wand(WandSettings {
                tolerance: 96,
                ..was
            })),
            "tolerance moved something else with it"
        );
        assert_eq!(
            picked_for(&Change::Set(EXPAND, 2.5), was),
            Some(Picked::Wand(WandSettings { expand: 3, ..was })),
            "expand moved something else with it"
        );
        assert_eq!(
            picked_for(&Change::Toggled(FILL_ON_CLICK, true), was),
            Some(Picked::Wand(WandSettings {
                fill_on_click: true,
                ..was
            })),
            "the bucket switch moved a number"
        );
    }

    /// The two commands mean what they are labelled.
    #[test]
    fn each_command_asks_for_its_own_action() {
        assert_eq!(
            picked_for(&Change::Pressed(FILL), wand()),
            Some(Picked::Selection(SelectAction::Fill))
        );
        assert_eq!(
            picked_for(&Change::Pressed(CLEAR), wand()),
            Some(Picked::Selection(SelectAction::Clear))
        );
    }
}
