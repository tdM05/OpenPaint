//! One module per *section*: what it shows, and what a press on it means.
//!
//! **A section, not a panel, because the two stopped being the same thing.** Most of these are
//! still one panel each. Four of them -- brush, select, transform, text -- are also what the two
//! contextual panels show: [`tool`] follows the tool in the artist's hand and [`properties`]
//! follows the active layer, and each is a `show` that matches on [`Status`] and delegates to one
//! of the four. That is also the answer to "some people want a tab strip and some want the setting
//! always in one place": the same module serves the contextual panel and a standalone one opened
//! from the panel list. `docs/CONTEXTUAL_PANELS.md` has the reasoning; DECISIONS 1e has the
//! decision.
//!
//! **This is the descriptor design showing up in the file layout.** It was already in the types --
//! a panel says what its controls *are* and applies what comes back, and nothing in it knows what a
//! slider looks like. But every panel lived in one `match` in one 2,900-line file, so two people
//! could not work on two panels without colliding, and the shape was visible only to whoever had
//! read the whole thing.
//!
//! Each module exports one function with one signature. `ui.rs` keeps a line each.
//!
//! # What a panel gets, and what it may do with it
//!
//! - [`Status`] is the document as it stands, read-only. Everything a panel could want to show.
//! - `brush` and `color_srgb` are the two things a panel may change **in place**, because they are
//!   settings rather than document state and there is nothing to undo.
//! - Everything else is asked for by returning a [`Picked`], which the shell applies. A panel that
//!   edited the document directly would be a panel that could not be undone.
//! - [`Painting`] is how it draws: `paint.show(ui, &controls)` and nothing else.
//!
//! # Ids
//!
//! Control ids are per module and shadowed inside it, so two panels cannot collide. Where a panel
//! numbers rows by index -- layers, pages, presets -- its command ids start from a base above any
//! index a document could reach; see the `FIRST_*` constants for the pattern.

use crate::layout::PanelId;
use crate::ui::Status;
use crate::workspace::{self as ws, Place};
use openpaint_core::Brush;

pub(crate) mod brush;
pub(crate) mod colour;
pub(crate) mod history;
pub(crate) mod layers;
pub(crate) mod menu;
pub(crate) mod page;
pub(crate) mod pages;
pub(crate) mod properties;
pub(crate) mod select;
pub(crate) mod text;
pub(crate) mod tool;
pub(crate) mod tools;
pub(crate) mod transform;

pub(crate) use crate::ui::{Painting, Picked};

/// Draw whichever panel this is.
///
/// The whole of what `ui.rs` knows about panel contents. A panel with nothing to draw -- the canvas
/// -- never reaches here: the workspace skips it, because its pixels come from the GPU underneath
/// everything egui does.
pub(crate) fn show(
    panel: PanelId,
    ui: &mut egui::Ui,
    brush: &mut Brush,
    color_srgb: &mut [u8; 3],
    state: &Status<'_>,
    paint: &mut Painting<'_>,
    place: Place,
) -> Option<Picked> {
    match panel {
        ws::MENU => menu::show(ui, brush, color_srgb, state, paint, place),
        ws::TOOLS => tools::show(ui, brush, color_srgb, state, paint, place),
        ws::BRUSH => brush::show(ui, brush, color_srgb, state, paint, place),
        ws::LAYERS => layers::show(ui, brush, color_srgb, state, paint, place),
        ws::COLOUR => colour::show(ui, brush, color_srgb, state, paint, place),
        ws::HISTORY => history::show(ui, brush, color_srgb, state, paint, place),
        ws::TRANSFORM => transform::show(ui, brush, color_srgb, state, paint, place),
        ws::PAGES => pages::show(ui, brush, color_srgb, state, paint, place),
        ws::PAGE => page::show(ui, brush, color_srgb, state, paint, place),
        ws::TEXT => text::show(ui, brush, color_srgb, state, paint, place),
        ws::SELECT => select::show(ui, brush, color_srgb, state, paint, place),
        ws::TOOL => tool::show(ui, brush, color_srgb, state, paint, place),
        ws::PROPERTIES => properties::show(ui, brush, color_srgb, state, paint, place),
        // A panel the build knows about but nothing has been written for yet. Silent on purpose:
        // this is the one case that is not a bug, and it goes away as the last module lands.
        _ => None,
    }
}

/// Which section a panel is currently showing.
///
/// **The unit of half-finished state is the section, not the panel.** [`crate::panel_draw::PanelInput`]
/// holds the scroll offset and the text field with the caret, and it used to be filed under the
/// panel. A contextual panel filed that way would drop the artist halfway down a list they never
/// scrolled the moment they changed tool, and would carry a half-typed preset name out of the
/// brush and into the wand's tolerance -- which is the third of the four things
/// `docs/CONTEXTUAL_PANELS.md` says will go wrong. So the shell keys that state by *this*, and an
/// ordinary panel is simply its own section.
///
/// It answers the same question [`tool::in_hand`] and [`properties::made_of`] answer, in the same
/// place they answer it, so the panel that is drawn and the state it is drawn with cannot disagree.
#[must_use]
pub(crate) fn section_of(panel: PanelId, state: &Status<'_>) -> PanelId {
    match panel {
        ws::TOOL => match tool::in_hand(state) {
            tool::InHand::Transform => ws::TRANSFORM,
            tool::InHand::Selection => ws::SELECT,
            tool::InHand::Paint => ws::BRUSH,
        },
        ws::PROPERTIES => match properties::made_of(state) {
            properties::MadeOf::Words => ws::TEXT,
            // Its own: the empty state is this panel's own words, not a section borrowed from
            // somewhere else.
            properties::MadeOf::Pixels => ws::PROPERTIES,
        },
        other => other,
    }
}

/// How big a list of words needs to be, laid out as a popup.
///
/// The same measurement the popup itself will do, so the box is the size of what goes in it. A
/// second guess here would show as a list clipped along one edge, which reads as the popup being
/// broken rather than as arithmetic.
#[must_use]
pub(crate) fn list_size(options: &[String], paint: &Painting<'_>) -> (f32, f32) {
    use crate::panel_ui::{extent, place, Control, Direction};
    let m = &paint.theme.metrics;
    let controls: Vec<Control> = options
        .iter()
        .map(|text| Control::Button {
            id: 0,
            text: text.clone(),
        })
        .collect();
    let text_of = |c: &Control| crate::panel_draw::text_width(paint.ctx, m.body, c);
    let widest = controls.iter().map(&text_of).fold(0.0_f32, f32::max);
    // Tall enough not to matter: the list is laid out into it and then measured, so nothing here
    // decides the height.
    let origin = crate::layout::Rect::new(0.0, 0.0, widest + m.padding * 2.0, 4000.0);
    // Every control here is a button or a choice, so nothing has a sentence to wrap and
    // the height of one is a question that never gets asked.
    let tall_of = |_: &crate::panel_ui::Control, _: f32| 0.0;
    let laid = place(&controls, origin, m, Direction::Column, &text_of, &tall_of);
    let tall = extent(&laid, origin).1;
    (
        widest + m.padding * 4.0,
        m.padding.mul_add(2.0, tall).min(600.0),
    )
}

/// Open a [`crate::panel_ui::Control::Pick`]'s list, anchored to the control that was pressed.
///
/// **The anchor is the control, not the pointer.** A list belongs under its own button; opening it
/// wherever the finger happened to land is how a menu ends up half off the screen with no
/// explanation. `pressed_rect` is filled by the engine for exactly this.
///
/// Returns what the shell should do, or `None` when there is nothing to anchor to -- which happens
/// only if the press did not come from a control, and then opening nothing is the right answer.
pub(crate) fn open_pick(
    id: crate::panel_ui::ControlId,
    options: &[String],
    paint: &mut Painting<'_>,
) -> Option<Picked> {
    let at = paint.input.pressed_rect?;
    let size = list_size(options, paint);
    *paint.pick = Some(id);
    Some(Picked::OpenMenu {
        at,
        size,
        // Down the panel, a list belongs beside its control; across it, beneath. The same reading
        // the menu bar makes, for the same reason: the popup must not cover the thing it is about.
        side: if paint.direction == crate::panel_ui::Direction::Column {
            crate::workspace::Anchor::Right
        } else {
            crate::workspace::Anchor::Below
        },
    })
}

/// Draw an open pick's list and report which option was chosen.
///
/// Called from the panel's `Place::Popup` arm. Answers `None` while the list is merely open, and
/// closes it on a choice -- so a panel writes the same four lines whatever its list is of.
pub(crate) fn pick_popup(
    id: crate::panel_ui::ControlId,
    options: &[String],
    chosen: usize,
    ui: &mut egui::Ui,
    paint: &mut Painting<'_>,
) -> Option<usize> {
    use crate::panel_ui::{Change, Control};
    if *paint.pick != Some(id) {
        return None;
    }
    let controls: Vec<Control> = options
        .iter()
        .enumerate()
        .map(|(i, text)| Control::Choice {
            id: u32::try_from(i).unwrap_or(u32::MAX),
            text: text.clone(),
            selected: i == chosen,
            // No icon: a list of words is a list of words, and half of them having a picture would
            // be worse than none of them having one.
            icon: None,
        })
        .collect();
    let mut answer = None;
    for change in paint.show(ui, &controls) {
        match change {
            Change::Chose(i) => {
                answer = Some(i as usize);
                *paint.pick = None;
            }
            other => eprintln!("pick {id}: unexpected {other:?}"),
        }
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::{Painting, Place};
    use crate::workspace as ws;

    /// **A contextual panel's half-finished state is filed under the section, not the panel.**
    ///
    /// The third of the four things `docs/CONTEXTUAL_PANELS.md` says will go wrong: `PanelInput`
    /// holds the scroll offset and the field with the caret, so one entry shared across contexts
    /// would drop the artist halfway down a list they never scrolled the moment they changed tool,
    /// and would carry a half-typed preset name out of the brush and into the wand's tolerance.
    ///
    /// The sabotage for this is `section_of` answering `panel` for everything -- the `other` arm
    /// swallowing the two contextual ids -- which is exactly the shape it would have had if nobody
    /// had thought about it, and this test names it.
    #[test]
    fn a_contextual_panel_files_its_half_finished_state_under_the_section() {
        let (layers, palette, presets, fonts) = crate::screenshot::sample_document();
        let text_at = layers
            .iter()
            .position(|l| l.text().is_some())
            .expect("the sample document has a text layer");
        let raster_at = layers
            .iter()
            .position(|l| l.text().is_none())
            .expect("the sample document has a raster layer");
        let status = || crate::ui::Status::sample(&layers, &palette, &presets, &fonts);

        let mut painting = status();
        painting.select_tool = None;
        painting.transform = None;
        assert_eq!(super::section_of(ws::TOOL, &painting), ws::BRUSH);

        let mut selecting = status();
        selecting.select_tool = Some(crate::ui::SelectTool::Wand);
        selecting.transform = None;
        assert_eq!(super::section_of(ws::TOOL, &selecting), ws::SELECT);

        let mut transforming = status();
        transforming.transform = Some(crate::ui::TransformState {
            transform: openpaint_core::Transform::IDENTITY,
            lock_aspect: false,
            kernel: openpaint_core::Kernel::Mitchell,
        });
        assert_eq!(super::section_of(ws::TOOL, &transforming), ws::TRANSFORM);

        let mut words = status();
        words.active_layer = text_at;
        assert_eq!(super::section_of(ws::PROPERTIES, &words), ws::TEXT);

        let mut pixels = status();
        pixels.active_layer = raster_at;
        assert_eq!(
            super::section_of(ws::PROPERTIES, &pixels),
            ws::PROPERTIES,
            "the empty state is the panel's own words, not a section borrowed from elsewhere"
        );

        // **And an ordinary panel is its own section, whatever the document is doing.** A panel
        // that answered something else here would share a scroll offset and a caret with a panel
        // it has nothing to do with.
        for panel in [
            ws::MENU,
            ws::TOOLS,
            ws::BRUSH,
            ws::LAYERS,
            ws::COLOUR,
            ws::HISTORY,
            ws::TRANSFORM,
            ws::PAGES,
            ws::PAGE,
            ws::TEXT,
            ws::SELECT,
        ] {
            for state in [&painting, &selecting, &transforming, &words, &pixels] {
                assert_eq!(super::section_of(panel, state), panel);
            }
        }
    }

    /// Every panel, laid out, so the assertions in `panel_ui::place` are actually asked.
    ///
    /// **A test that cannot express the bug proves nothing.** A dozen labels shipped carrying runs
    /// of twenty-odd spaces mid-sentence -- the recovery prompt read "pointed at the original
    /// [twenty spaces] file, so" -- and the check for it was added to `place`, which every
    /// on-screen string passes through. Then the same defect was put back on purpose and the whole
    /// suite stayed green, because nothing in it had ever built these panels' controls and laid
    /// them out. This is the missing half.
    ///
    /// No GPU: `place` and the label measurement are egui's text layout and arithmetic, and the
    /// picture is not what is being checked. Both surfaces, because a popup's half of a panel is
    /// a different list of controls from its panel's half.
    #[test]
    fn every_panel_lays_its_controls_out() {
        let (layers, palette, presets, fonts) = crate::screenshot::sample_document();
        let theme = crate::theme::Theme::default();
        let mut brush = openpaint_core::Brush::default();
        let mut colour = brush.color_srgb8();
        let mut panel_input: std::collections::HashMap<u32, crate::panel_draw::PanelInput> =
            std::collections::HashMap::new();
        let mut menu: Option<u32> = None;
        let mut pick: Option<crate::panel_ui::ControlId> = None;
        let mut wheel_shape = crate::colour_wheel::Shape::default();
        let mut wheel_hold: Option<crate::colour_wheel::Region> = None;

        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |c| {
            let status = crate::ui::Status::sample(&layers, &palette, &presets, &fonts);
            egui::CentralPanel::default().show(c, |ui| {
                for panel in [
                    ws::MENU,
                    ws::TOOLS,
                    ws::BRUSH,
                    ws::LAYERS,
                    ws::COLOUR,
                    ws::HISTORY,
                    ws::TRANSFORM,
                    ws::PAGES,
                    ws::PAGE,
                    ws::TEXT,
                    ws::SELECT,
                    ws::TOOL,
                    ws::PROPERTIES,
                ] {
                    for place in [Place::Panel, Place::Popup] {
                        // A popup only draws when its own pick is the one open, so ask for each in
                        // turn rather than leaving that half of every panel undrawn.
                        pick = Some(0);
                        let mut paint = Painting {
                            theme: &theme,
                            direction: crate::panel_ui::Direction::Column,
                            input: panel_input.entry(panel.0).or_default(),
                            menu: &mut menu,
                            pick: &mut pick,
                            extend_by: 512,
                            preset_name: "a name",
                            ctx: c,
                            wheel_shape: &mut wheel_shape,
                            wheel_hold: &mut wheel_hold,
                        };
                        super::show(
                            panel,
                            ui,
                            &mut brush,
                            &mut colour,
                            &status,
                            &mut paint,
                            place,
                        );
                    }
                }
            });
        });
    }
}
