//! The layer stack.
//!
//! One module per panel, exporting one function. See [`super`] for why.

use super::{Painting, Picked};
use crate::panel_ui::{Change, Control, ControlId, RowMark};
use crate::ui::{LayerAction, Status};
use crate::workspace::Place;
use openpaint_core::{Blend, Brush, Layer};

// Rows are numbered by layer index, so the ids above them start where no document
// could reach. A row and a button sharing an id would be a silent mis-hit.
const FIRST_VISIBILITY: ControlId = 1 << 19;
const FIRST_COMMAND: ControlId = 1 << 20;
const LOCK_ALPHA: ControlId = FIRST_COMMAND + 1;
const ADD: ControlId = FIRST_COMMAND + 2;
const DUPLICATE: ControlId = FIRST_COMMAND + 3;
const MERGE_DOWN: ControlId = FIRST_COMMAND + 4;
const DELETE: ControlId = FIRST_COMMAND + 5;
const BLEND: ControlId = FIRST_COMMAND + 6;
const CLIP_BELOW: ControlId = FIRST_COMMAND + 7;
const OPACITY: ControlId = FIRST_COMMAND + 8;
const UP: ControlId = FIRST_COMMAND + 9;
const DOWN: ControlId = FIRST_COMMAND + 10;
const LOCKED: ControlId = FIRST_COMMAND + 11;

/// The blend modes, as the dropdown lists them.
///
/// Built rather than held, because [`Blend::label`] is the one place a mode's name lives and a
/// second copy here would be a name to keep in step for nothing.
#[must_use]
fn modes() -> Vec<String> {
    Blend::ALL.iter().map(|b| b.label().to_owned()).collect()
}

/// What the panel shows for this stack, with `active` the layer being painted on.
///
/// Split out from [`show`] because everything interesting about this panel is which controls come
/// out of which document, and that is a question about a slice rather than about a GPU.
#[must_use]
fn controls(layers: &[Layer], active: usize) -> Vec<Control> {
    let current = layers.get(active);
    let mut controls = Vec::new();
    // Top of the list is the top of the stack, which is how every layers panel reads and
    // the opposite of how the document stores it.
    for (index, layer) in layers.iter().enumerate().rev() {
        let row = ControlId::try_from(index).unwrap_or(ControlId::MAX);
        controls.push(Control::Row {
            id: row,
            text: layer.name.clone(),
            selected: index == active,
            // The eye, on the row it belongs to. Hiding a layer used to mean selecting it
            // first and then finding a switch below the list, which is two steps for
            // something every paint application does in one.
            //
            // **The row's one mark goes to the eye rather than to alpha lock or clipping**, and
            // that is the whole of the reasoning: the eye is the switch you flip on layers you are
            // *not* painting on -- checking what is underneath, comparing two versions, hunting
            // for which layer a stray mark is on -- so making it act on "the current layer" would
            // mean selecting a layer in order to hide it and then selecting back. The other two
            // are properties of the layer you are painting on, so the selection they would need
            // has already happened; below the list they cost no extra step.
            mark: Some(RowMark {
                id: FIRST_VISIBILITY + row,
                on: layer.visible,
            }),
            swatch: None,
        });
    }
    // Reordering is about the list just read, so it sits directly under it rather than among the
    // commands that make and unmake layers.
    //
    // **Shown even at the ends of the stack, and refused out loud on the way back.** There is no
    // disabled state in the vocabulary, so the choice is between omitting a command and showing it
    // inert, and omitting moves the others: on the top layer there would be no Up, so Down would
    // slide into where Up was and pressing the same place on two different layers would do two
    // different things. Delete already makes that call here, and Pages makes it too.
    for (id, text) in [(UP, "Up"), (DOWN, "Down")] {
        controls.push(Control::Button {
            id,
            text: text.to_owned(),
        });
    }
    controls.push(Control::Separator);
    // **How a layer combines with what is under it.** Three today and more later, which is why it
    // is a dropdown rather than three choices: a row of buttons stops being readable long before a
    // list does.
    controls.push(Control::Pick {
        id: BLEND,
        text: "Blend".to_owned(),
        value: current.map_or_else(String::new, |l| l.blend.label().to_owned()),
    });
    // Linear, unlike the brush's size: opacity has no small end that wants room, and half a layer
    // should be halfway along the track.
    controls.push(Control::Slider {
        id: OPACITY,
        text: "Opacity".to_owned(),
        value: current.map_or(1.0, |l| l.opacity),
        min: 0.0,
        max: 1.0,
        unit: "",
        log: false,
    });
    controls.push(Control::Toggle {
        id: LOCK_ALPHA,
        text: "Lock alpha".to_owned(),
        on: current.is_some_and(Layer::locks_alpha),
    });
    controls.push(Control::Toggle {
        id: CLIP_BELOW,
        text: "Clip to the layer below".to_owned(),
        on: current.is_some_and(Layer::clips_below),
    });
    // **Above the two that sound like it, not below**, because it is the one an artist reaches
    // for constantly and the other two are a colouring technique. Named "Lock this layer" rather
    // than "Lock" so it cannot be misread as the alpha lock two rows down.
    controls.push(Control::Toggle {
        id: LOCKED,
        text: "Lock this layer".to_owned(),
        on: current.is_some_and(Layer::is_locked),
    });
    // The old panel carried this in hover text on two one-glyph toggles. There is no hover here,
    // and a pen has no hover to give -- so what those tooltips said is said in the open. Three
    // switches whose names sound alike and whose difference decides a whole colouring workflow are
    // exactly the case where the explanation has to be readable without being hunted for.
    //
    // **One clause each, and no longer.** It ran to six lines once the lock was added to it, and
    // an end-to-end session showed the cost: with four layers the list was down to a single row
    // and the buttons had gone off the bottom of the panel. An explanation that pushes away the
    // thing it explains has stopped explaining. What each switch *is* survives the cut; the
    // second clause on each did not.
    controls.push(Control::Label {
        text: "Lock sets a layer aside so nothing can change it -- how a sketch survives being \
               inked over. Lock alpha paints only where the layer already has pixels -- how \
               colour goes inside line art. Clipping shows a layer only where the one below \
               it has pixels -- how shading sits over flats."
            .to_owned(),
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
    controls.push(Control::Label {
        text: "Deleting and merging are undoable -- otherwise they would not be offered. \
               Duplicating is not: the way back is to delete the copy."
            .to_owned(),
    });
    controls
}

/// What a change on this panel asks the document to do, given how the stack stands.
///
/// A command that cannot apply answers `None` and says why, rather than quietly doing nothing
/// (§6b) -- the button is drawn in every case, so the refusal is the only thing left to tell the
/// artist that pressing it was not ignored by accident.
///
/// [`Change::Pressed`] of `BLEND` is not here: opening a list needs the rectangle the press came
/// from, which is [`super::open_pick`]'s business and not a pure function of the stack.
#[must_use]
fn picked(change: &Change, count: usize, active: usize) -> Option<Picked> {
    let layer = |action| Some(Picked::Layer(action));
    match *change {
        Change::Chose(index) => layer(LayerAction::Select(index as usize)),
        Change::Toggled(id, visible) if (FIRST_VISIBILITY..FIRST_COMMAND).contains(&id) => {
            layer(LayerAction::SetVisible {
                index: (id - FIRST_VISIBILITY) as usize,
                visible,
            })
        }
        Change::Toggled(LOCK_ALPHA, lock) => layer(LayerAction::SetLockAlpha {
            index: active,
            lock,
        }),
        Change::Toggled(CLIP_BELOW, clip) => layer(LayerAction::SetClipBelow {
            index: active,
            clip,
        }),
        Change::Toggled(LOCKED, locked) => layer(LayerAction::SetLocked {
            index: active,
            locked,
        }),
        Change::Set(OPACITY, opacity) => layer(LayerAction::SetOpacity {
            index: active,
            opacity,
        }),
        // **Up is a higher index.** The list is drawn top-down and the document stores it
        // bottom-first, so the two directions are opposite and reading the code cannot tell you
        // which way is which -- only the drawn order can, which is what the test asserts.
        Change::Pressed(UP) if active + 1 < count => layer(LayerAction::Move {
            from: active,
            to: active + 1,
        }),
        Change::Pressed(UP) => {
            eprintln!("layers: the top layer has nothing to move above");
            None
        }
        // Guarded rather than left to the document: `to` would be `active - 1` at the bottom,
        // which is an underflow rather than a no-op.
        Change::Pressed(DOWN) if active > 0 => layer(LayerAction::Move {
            from: active,
            to: active - 1,
        }),
        Change::Pressed(DOWN) => {
            eprintln!("layers: the bottom layer has nothing to move below");
            None
        }
        Change::Pressed(ADD) => layer(LayerAction::Add),
        Change::Pressed(DUPLICATE) => layer(LayerAction::Duplicate(active)),
        // Not guarded here, unlike Up and Down: merging into nothing is already refused where it is
        // applied, and with a message the artist can read rather than a line on stderr. A second
        // guard would be one rule in two places, and the weaker copy would be the visible one.
        Change::Pressed(MERGE_DOWN) => layer(LayerAction::MergeDown(active)),
        // Deleting the last layer would leave a document with nothing to paint on, so
        // the command is refused out loud rather than quietly doing nothing (§6b).
        Change::Pressed(DELETE) if count > 1 => layer(LayerAction::Delete(active)),
        Change::Pressed(DELETE) => {
            eprintln!("layers: a document needs at least one layer");
            None
        }
        ref other => {
            eprintln!("layers panel: unexpected {other:?}");
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
    // The second panel described rather than drawn, and the one that exercises the rest of
    // the vocabulary: a list, a slider, a pair of switches, a dropdown and commands.
    let _ = (&mut *brush, &mut *color_srgb);
    let (layers, active_layer) = (state.layers, state.active_layer);

    // The dropdown's other half. Drawn when this panel's own popup is up, which is the only time
    // it can be: one popup at a time, and the workspace decides where it goes.
    if place == Place::Popup {
        let chosen = layers.get(active_layer).map_or(0, |l| {
            Blend::ALL.iter().position(|b| *b == l.blend).unwrap_or(0)
        });
        return super::pick_popup(BLEND, &modes(), chosen, ui, paint).and_then(|n| {
            Blend::ALL.get(n).map(|blend| {
                Picked::Layer(LayerAction::SetBlend {
                    index: active_layer,
                    blend: *blend,
                })
            })
        });
    }

    let controls = controls(layers, active_layer);
    let mut answer = None;
    for change in paint.show(ui, &controls) {
        answer = if change == Change::Pressed(BLEND) {
            super::open_pick(BLEND, &modes(), paint)
        } else {
            picked(&change, layers.len(), active_layer)
        };
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stack named bottom-first, the way the document holds it.
    fn stack(names: &[&str]) -> Vec<Layer> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                Layer::restored(
                    u32::try_from(i).expect("small"),
                    *name,
                    1.0,
                    Blend::Normal,
                    true,
                    false,
                    false,
                )
            })
            .collect()
    }

    /// The rows as the artist reads them, top of the stack first.
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

    fn find(controls: &[Control], id: ControlId) -> Control {
        controls
            .iter()
            .find(|c| c.id() == Some(id))
            .expect("the control should be in the list")
            .clone()
    }

    /// The document's own move, so a direction is checked against what the stack becomes rather
    /// than against the arithmetic that produced it.
    fn moved(names: &[&str], from: usize, to: usize) -> Vec<String> {
        let mut names: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
        let one = names.remove(from);
        names.insert(to, one);
        names
    }

    /// The list is drawn top-down while the document stores it bottom-first, and the active layer
    /// is the selected row.
    #[test]
    fn the_top_of_the_stack_is_the_top_of_the_list() {
        let layers = stack(&["Paper", "Flats", "Ink"]);
        assert_eq!(
            rows(&controls(&layers, 1)),
            vec![
                (2, "Ink".to_owned(), false),
                (1, "Flats".to_owned(), true),
                (0, "Paper".to_owned(), false),
            ]
        );
    }

    /// Every row carries its own eye, and it carries that layer's own visibility -- not the active
    /// layer's, which is what a switch below the list would have made it.
    #[test]
    fn each_row_carries_its_own_eye() {
        let mut layers = stack(&["Paper", "Flats", "Ink"]);
        layers[1].visible = false;
        let list = controls(&layers, 0);
        let marks: Vec<(ControlId, bool)> = list
            .iter()
            .filter_map(|c| match c {
                Control::Row { mark: Some(m), .. } => Some((m.id, m.on)),
                _ => None,
            })
            .collect();
        assert_eq!(
            marks,
            vec![
                (FIRST_VISIBILITY + 2, true),
                (FIRST_VISIBILITY + 1, false),
                (FIRST_VISIBILITY, true),
            ]
        );
        // An eye's id must stay clear of the commands, or hiding a layer would delete one.
        for (id, _) in marks {
            assert!(id < FIRST_COMMAND);
        }
    }

    /// The properties below the list show the *active* layer, since that is what they act on.
    #[test]
    fn the_properties_are_the_active_layers() {
        let mut layers = stack(&["Paper", "Flats", "Ink"]);
        layers[1].blend = Blend::Multiply;
        layers[1].opacity = 0.25;
        layers[1].lock_alpha = true;
        layers[1].clip_below = true;
        layers[1].locked = true;
        let list = controls(&layers, 1);

        assert_eq!(
            find(&list, BLEND),
            Control::Pick {
                id: BLEND,
                text: "Blend".to_owned(),
                value: "Multiply".to_owned(),
            }
        );
        assert_eq!(
            find(&list, OPACITY),
            Control::Slider {
                id: OPACITY,
                text: "Opacity".to_owned(),
                value: 0.25,
                min: 0.0,
                max: 1.0,
                unit: "",
                log: false,
            }
        );
        for id in [LOCK_ALPHA, CLIP_BELOW, LOCKED] {
            assert!(
                matches!(find(&list, id), Control::Toggle { on: true, .. }),
                "the switch should be showing the active layer's own setting"
            );
        }

        // And the layer next door has none of it, so the panel is not showing a mixture.
        let list = controls(&layers, 2);
        for id in [LOCK_ALPHA, CLIP_BELOW, LOCKED] {
            assert!(matches!(find(&list, id), Control::Toggle { on: false, .. }));
        }
    }

    /// **Every command is drawn whatever the stack looks like.**
    ///
    /// This is the whole reason they are shown when they cannot apply: were they omitted, the
    /// second button under the top layer would be Down and the second under a middle layer would
    /// be Up, so pressing the same spot twice would do two different things.
    #[test]
    fn the_commands_do_not_move_when_they_cannot_apply() {
        for (names, active) in [
            (&["Paper", "Flats", "Ink"][..], 0),
            (&["Paper", "Flats", "Ink"][..], 1),
            (&["Paper", "Flats", "Ink"][..], 2),
            (&["Paper"][..], 0),
        ] {
            assert_eq!(
                buttons(&controls(&stack(names), active)),
                vec![UP, DOWN, ADD, DUPLICATE, MERGE_DOWN, DELETE],
                "{names:?}, layer {active} active"
            );
        }
    }

    /// A document still loading is still a panel: no rows, sensible defaults, and a way to make a
    /// layer.
    #[test]
    fn an_empty_stack_still_offers_add() {
        let list = controls(&[], 0);
        assert!(rows(&list).is_empty());
        assert!(buttons(&list).contains(&ADD));
        assert!(matches!(
            find(&list, OPACITY),
            Control::Slider { value: 1.0, .. }
        ));
        assert!(matches!(find(&list, BLEND), Control::Pick { .. }));
    }

    /// **Up moves a layer nearer the top of the drawn list, which is a higher index.**
    ///
    /// The list reads top-down and the document stores it bottom-first, so the two run opposite
    /// ways and no amount of reading `to: active + 1` says which is which. So the direction is
    /// checked the way the artist sees it: against the names, in the order they are drawn.
    #[test]
    fn up_moves_a_layer_up_the_list_the_artist_reads() {
        const NAMES: [&str; 3] = ["Paper", "Flats", "Ink"];
        // Drawn: Ink, Flats, Paper. Flats is the middle layer, index 1.
        let layers = stack(&NAMES);
        assert_eq!(
            rows(&controls(&layers, 1))
                .iter()
                .map(|(_, name, _)| name.clone())
                .collect::<Vec<_>>(),
            vec!["Ink", "Flats", "Paper"]
        );

        let Some(Picked::Layer(LayerAction::Move { from, to })) =
            picked(&Change::Pressed(UP), 3, 1)
        else {
            panic!("Up should ask for a move");
        };
        assert_eq!((from, to), (1, 2));
        // Flats now sits above Ink -- one place nearer the top of what is drawn.
        assert_eq!(moved(&NAMES, from, to), vec!["Paper", "Ink", "Flats"]);
        let after = moved(&NAMES, from, to);
        let after: Vec<&str> = after.iter().map(String::as_str).collect();
        assert_eq!(
            rows(&controls(&stack(&after), 2))
                .iter()
                .map(|(_, name, _)| name.clone())
                .collect::<Vec<_>>(),
            vec!["Flats", "Ink", "Paper"],
            "Up put it below Ink instead of above it"
        );

        // And Down is the mirror: Flats goes under Paper, to the bottom of the drawn list.
        let Some(Picked::Layer(LayerAction::Move { from, to })) =
            picked(&Change::Pressed(DOWN), 3, 1)
        else {
            panic!("Down should ask for a move");
        };
        assert_eq!((from, to), (1, 0));
        let after = moved(&NAMES, from, to);
        let after: Vec<&str> = after.iter().map(String::as_str).collect();
        assert_eq!(
            rows(&controls(&stack(&after), 0))
                .iter()
                .map(|(_, name, _)| name.clone())
                .collect::<Vec<_>>(),
            vec!["Ink", "Paper", "Flats"]
        );
    }

    /// A press asks for what it says, on the layer it says it acts on.
    #[test]
    fn a_press_asks_for_what_it_says() {
        let at = |change| picked(&change, 3, 1);
        assert_eq!(
            at(Change::Chose(2)),
            Some(Picked::Layer(LayerAction::Select(2)))
        );
        // An eye acts on its own row, not on the active layer.
        assert_eq!(
            at(Change::Toggled(FIRST_VISIBILITY + 2, false)),
            Some(Picked::Layer(LayerAction::SetVisible {
                index: 2,
                visible: false,
            }))
        );
        assert_eq!(
            at(Change::Toggled(LOCK_ALPHA, true)),
            Some(Picked::Layer(LayerAction::SetLockAlpha {
                index: 1,
                lock: true,
            }))
        );
        assert_eq!(
            at(Change::Toggled(CLIP_BELOW, true)),
            Some(Picked::Layer(LayerAction::SetClipBelow {
                index: 1,
                clip: true,
            }))
        );
        assert_eq!(
            at(Change::Toggled(LOCKED, true)),
            Some(Picked::Layer(LayerAction::SetLocked {
                index: 1,
                locked: true,
            }))
        );
        assert_eq!(
            at(Change::Set(OPACITY, 0.5)),
            Some(Picked::Layer(LayerAction::SetOpacity {
                index: 1,
                opacity: 0.5,
            }))
        );
        assert_eq!(
            at(Change::Pressed(ADD)),
            Some(Picked::Layer(LayerAction::Add))
        );
        assert_eq!(
            at(Change::Pressed(DUPLICATE)),
            Some(Picked::Layer(LayerAction::Duplicate(1)))
        );
        assert_eq!(
            at(Change::Pressed(MERGE_DOWN)),
            Some(Picked::Layer(LayerAction::MergeDown(1)))
        );
        assert_eq!(
            at(Change::Pressed(DELETE)),
            Some(Picked::Layer(LayerAction::Delete(1)))
        );
    }

    /// **A command that cannot apply does nothing at all**, rather than moving a layer off the end
    /// of the stack or leaving the document with nothing to paint on.
    #[test]
    fn a_command_at_the_end_of_its_range_is_refused() {
        assert_eq!(picked(&Change::Pressed(UP), 3, 2), None, "above the top");
        assert_eq!(
            picked(&Change::Pressed(DOWN), 3, 0),
            None,
            "below the bottom"
        );
        assert_eq!(
            picked(&Change::Pressed(DELETE), 1, 0),
            None,
            "the last layer"
        );
        // On a one-layer document neither move has anywhere to go.
        for id in [UP, DOWN, DELETE] {
            assert_eq!(picked(&Change::Pressed(id), 1, 0), None);
        }
    }

    /// The dropdown lists exactly the modes the core knows about, in its order -- so the index
    /// that comes back out of the popup can be looked straight up in [`Blend::ALL`].
    #[test]
    fn the_blend_list_is_the_cores_own() {
        assert_eq!(modes(), vec!["Normal", "Multiply", "Screen"]);
        assert_eq!(modes().len(), Blend::ALL.len());
    }
}
