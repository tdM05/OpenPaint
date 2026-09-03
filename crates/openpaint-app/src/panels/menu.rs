//! The menu bar: the way to every command that is not a gesture.
//!
//! One module per panel, exporting one function. See [`super`] for why.

use super::{Painting, Picked};
use crate::panel_ui::Direction;
use crate::ui::Status;
use crate::ui::{Command, LayerAction, SelectAction};
use crate::workspace::Anchor;
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
    // **A menu drops down under its own button.** It replaced the strip's contents at
    // first, which was a mistake: the menu bar is a landmark, and replacing it makes you
    // lose your place. Touch-friendliness comes from the size of the targets and from
    // being able to dismiss it by pressing anywhere else, not from refusing to float
    // anything -- the menus on a phone are overlays too.
    //
    // The floating part is the workspace's popup, the same object the panel list and the
    // panel settings use. What is in it is this panel's business; where it goes, what
    // closes it and what a press inside it means are the workspace's.
    use crate::panel_ui::{Change, Control};
    const PANELS_LIST: u32 = 1 << 20;
    const FIRST_ITEM: u32 = 1 << 21;

    match place {
        Place::Panel => {
            let mut controls = vec![Control::Label {
                text: "OpenPaint".to_owned(),
            }];
            for (i, (name, _)) in MENUS.iter().enumerate() {
                controls.push(Control::Choice {
                    id: u32::try_from(i).unwrap_or(u32::MAX),
                    text: (*name).to_owned(),
                    // Lit while its menu is down, so there is never any doubt which list
                    // you are looking at.
                    selected: *paint.menu == Some(u32::try_from(i).unwrap_or(u32::MAX)),
                    icon: None,
                });
            }
            controls.push(Control::Separator);
            controls.push(Control::Button {
                id: PANELS_LIST,
                text: "Panels".to_owned(),
            });

            let direction = paint.direction;
            for change in paint.show(ui, &controls) {
                match change {
                    Change::Pressed(PANELS_LIST) => picked = Some(Picked::PanelList),
                    Change::Chose(which) if *paint.menu == Some(which) => {
                        // Pressing the open menu's own button puts it away, which is what
                        // every menu bar does and the only way to close one without
                        // choosing something from it.
                        *paint.menu = None;
                        picked = Some(Picked::CloseMenu);
                    }
                    Change::Chose(which) => {
                        *paint.menu = Some(which);
                        if let Some(at) = paint.input.pressed_rect {
                            let size = menu_size(
                                which,
                                active_layer,
                                layers.len(),
                                state.has_selection,
                                paint,
                            );
                            picked = Some(Picked::OpenMenu {
                                at,
                                size,
                                // A menu bar running down the side drops its items out to
                                // the side; one running across drops them below. The panel
                                // does not decide which way it runs, so it asks.
                                side: if direction == Direction::Column {
                                    Anchor::Right
                                } else {
                                    Anchor::Below
                                },
                            });
                        }
                    }
                    other => eprintln!("menu panel: unexpected {other:?}"),
                }
            }
        }
        Place::Popup => {
            let Some(which) = *paint.menu else {
                return picked;
            };
            let items = menu_items(which, active_layer, layers.len(), state.has_selection);
            let controls: Vec<Control> = items
                .iter()
                .enumerate()
                .map(|(i, item)| Control::Button {
                    id: FIRST_ITEM + u32::try_from(i).unwrap_or(0),
                    text: item.0.clone(),
                })
                .collect();
            for change in paint.show(ui, &controls) {
                match change {
                    Change::Pressed(id) if id >= FIRST_ITEM => {
                        picked = items
                            .get((id - FIRST_ITEM) as usize)
                            .map(|(_, what)| what.clone());
                        // A menu left open over the canvas is a menu in the way.
                        *paint.menu = None;
                    }
                    other => eprintln!("menu popup: unexpected {other:?}"),
                }
            }
        }
    }
    picked
}

/// The menus, and what each one is called.
///
/// The names live here and the commands live in [`menu_items`], because a menu's contents depend
/// on the document -- Delete is not offered when there is one layer left -- and a table cannot
/// know that.
pub(crate) const MENUS: &[(&str, ())] = &[
    ("File", ()),
    ("Edit", ()),
    ("Layer", ()),
    ("Select", ()),
    ("View", ()),
];

/// How big a menu's drop-down needs to be.
///
/// Measured from the items it is about to show, because the workspace places the popup before the
/// panel draws into it: a guess would either clip the longest command or leave a margin of nothing
/// beside the shortest.
fn menu_size(
    which: u32,
    active_layer: usize,
    layers: usize,
    has_selection: bool,
    paint: &Painting<'_>,
) -> (f32, f32) {
    let m = &paint.theme.metrics;
    let controls: Vec<crate::panel_ui::Control> =
        menu_items(which, active_layer, layers, has_selection)
            .into_iter()
            .map(|(name, _)| crate::panel_ui::Control::Button { id: 0, text: name })
            .collect();
    let text_of =
        |c: &crate::panel_ui::Control| crate::panel_draw::text_width(paint.ctx, m.body, c);
    let widest = controls.iter().map(&text_of).fold(0.0_f32, f32::max);
    let origin = crate::layout::Rect::new(0.0, 0.0, widest + m.padding * 2.0, 4000.0);
    // Every control here is a button or a choice, so nothing has a sentence to wrap and
    // the height of one is a question that never gets asked.
    let tall_of = |_: &crate::panel_ui::Control, _: f32| 0.0;
    let laid = crate::panel_ui::place(&controls, origin, m, Direction::Column, &text_of, &tall_of);
    let tall = crate::panel_ui::extent(&laid, origin).1;
    (widest + m.padding * 4.0, tall + m.padding * 2.0)
}

/// What one menu offers, given the document it is offering it for.
///
/// **Commands that cannot be carried out are not offered.** Deleting the last layer would leave
/// nothing to paint on, and a menu entry that refuses when pressed teaches you not to trust the
/// menu (DECISIONS 6b).
pub(crate) fn menu_items(
    which: u32,
    active_layer: usize,
    layers: usize,
    has_selection: bool,
) -> Vec<(String, Picked)> {
    let named = |name: &str, what: Picked| (name.to_owned(), what);
    match which {
        0 => vec![
            named("New", Picked::Command(Command::New)),
            named("Open", Picked::Command(Command::Open)),
            // Directly under Open, because "get a picture in" is the same errand: Open replaces
            // what is here, Place adds to it, and the pair is easier to tell apart together than
            // either is to find alone.
            named("Place image", Picked::Command(Command::PlaceImage)),
            named("Save", Picked::Command(Command::Save)),
            named("Save As", Picked::Command(Command::SaveAs)),
            // Not "Export PNG" any more: it asks what to export and where to put it, and the
            // format was never the interesting half of that sentence.
            named("Export", Picked::Command(Command::ExportPng)),
        ],
        1 => {
            let mut items = vec![
                named("Undo", Picked::Command(Command::Undo)),
                named("Redo", Picked::Command(Command::Redo)),
                // Paste is offered whatever is selected: what it needs is on the clipboard,
                // which this panel cannot see and must not guess about. Copy and Cut need a
                // selection and are added below with the rest of the selection commands.
                named("Paste", Picked::Command(Command::Paste)),
            ];
            // **Not offered with nothing selected**, for the same reason Delete is not offered on
            // the only layer: a menu that offers what it will refuse teaches you not to trust the
            // menu. Filling "the selection" when there is none has nothing to fill.
            if has_selection {
                items.push(named("Copy", Picked::Command(Command::Copy)));
                items.push(named("Cut", Picked::Command(Command::Cut)));
                items.push(named(
                    "Fill selection",
                    Picked::Selection(SelectAction::Fill),
                ));
            }
            items
        }
        2 => {
            let mut items = vec![
                named("Add", Picked::Layer(LayerAction::Add)),
                named(
                    "Duplicate",
                    Picked::Layer(LayerAction::Duplicate(active_layer)),
                ),
            ];
            // Merging needs something underneath to merge into, so the bottom layer has no such
            // command -- exactly the rule Delete already follows on the only layer.
            if active_layer > 0 {
                items.push(named(
                    "Merge down",
                    Picked::Layer(LayerAction::MergeDown(active_layer)),
                ));
            }
            if layers > 1 {
                items.push(named(
                    "Delete",
                    Picked::Layer(LayerAction::Delete(active_layer)),
                ));
            }
            items
        }
        3 => {
            let mut items = vec![named("All", Picked::Selection(SelectAction::All))];
            // The three that act on a selection appear when there is one. "Select all" always
            // has something to do; the rest do not.
            if has_selection {
                items.push(named("Deselect", Picked::Selection(SelectAction::None)));
                items.push(named("Invert", Picked::Selection(SelectAction::Invert)));
                items.push(named("Clear", Picked::Selection(SelectAction::Clear)));
            }
            items
        }
        4 => vec![
            named("Fit", Picked::Command(Command::ZoomFit)),
            named("Actual size", Picked::Command(Command::ZoomActual)),
            named("Settings", Picked::Settings),
        ],
        // **Nothing, rather than the View menu.** This was the `_` arm, so any id that was not one
        // of the five drew View's items under some other menu's name -- a menu that lies about
        // what it is, and silently. An id nobody put in the bar has no items, and an empty popup
        // is at least honestly empty.
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A menu never offers something it would refuse.** Deleting the last layer would leave
    /// nothing to paint on, and an entry that refuses when pressed teaches you not to trust the
    /// menu at all.
    #[test]
    fn the_layer_menu_hides_delete_when_there_is_one_layer_left() {
        let names = |items: &[(String, Picked)]| -> Vec<String> {
            items.iter().map(|(n, _)| n.clone()).collect()
        };
        assert!(!names(&menu_items(2, 0, 1, false)).contains(&"Delete".to_owned()));
        assert!(names(&menu_items(2, 0, 2, false)).contains(&"Delete".to_owned()));
    }

    /// The same rule for everything else a menu would refuse.
    ///
    /// Merging needs a layer underneath, and the three commands that act on a selection need one
    /// to act on. Each was offered unconditionally and refused when pressed, which is exactly what
    /// the test above exists to prevent -- it just only covered Delete.
    #[test]
    fn a_menu_offers_nothing_it_would_refuse() {
        let names = |items: &[(String, Picked)]| -> Vec<String> {
            items.iter().map(|(n, _)| n.clone()).collect()
        };
        assert!(
            !names(&menu_items(2, 0, 3, false)).contains(&"Merge down".to_owned()),
            "the bottom layer has nothing to merge into"
        );
        assert!(names(&menu_items(2, 1, 3, false)).contains(&"Merge down".to_owned()));

        assert!(!names(&menu_items(1, 0, 3, false)).contains(&"Fill selection".to_owned()));
        assert!(names(&menu_items(1, 0, 3, true)).contains(&"Fill selection".to_owned()));

        let without = names(&menu_items(3, 0, 3, false));
        assert_eq!(
            without,
            vec!["All".to_owned()],
            "with nothing selected, only Select all has anything to do"
        );
        let with = names(&menu_items(3, 0, 3, true));
        assert!(with.contains(&"Deselect".to_owned()) && with.contains(&"Clear".to_owned()));
    }

    /// Every menu offers something, and every entry is named.
    ///
    /// A menu that drilled into an empty strip would be a dead end with only the way back in it,
    /// and there would be no telling that from a menu that failed to load.
    #[test]
    fn every_menu_offers_something_you_can_press() {
        for (which, (name, ())) in MENUS.iter().enumerate() {
            let items = menu_items(u32::try_from(which).expect("small"), 0, 4, false);
            assert!(!items.is_empty(), "the {name} menu is empty");
            for (label, _) in &items {
                assert!(!label.is_empty(), "an unnamed entry in the {name} menu");
            }
        }
    }

    /// A menu's entries act on the layer that is actually active.
    ///
    /// The index is baked in when the entry is built, so building it against the wrong one would
    /// send Duplicate at whichever layer happened to be first.
    #[test]
    fn layer_menu_entries_act_on_the_active_layer() {
        let items = menu_items(2, 3, 5, false);
        assert!(
            items
                .iter()
                .any(|(n, what)| n == "Duplicate"
                    && *what == Picked::Layer(LayerAction::Duplicate(3))),
            "Duplicate does not name the active layer"
        );
        assert!(
            items
                .iter()
                .any(|(n, what)| n == "Delete" && *what == Picked::Layer(LayerAction::Delete(3))),
            "Delete does not name the active layer"
        );
    }
}
