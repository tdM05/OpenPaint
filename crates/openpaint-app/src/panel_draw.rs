//! Turning described controls into pixels, and pixels back into changes.
//!
//! The other half of [`crate::panel_ui`], and the half that knows about a screen. Everything here
//! is a rectangle, a line or a run of text — **no egui widgets**. That is the point: egui is used
//! as a surface to paint on, not as the widget library, so when it goes the only thing that
//! changes is which painter these calls are made against.
//!
//! Nothing in this file is tested, and it is written to keep it that way: every decision worth
//! being wrong about — where a control sits, what a press means, where a knob is drawn — lives in
//! `panel_ui` where it can be checked without a window. What is left here is arithmetic on
//! rectangles and a choice of colour.

use crate::layout::Rect;
use crate::panel_ui::{
    change_at, clamp_scroll, extent, hit, mark_rect, place, to_fraction, Change, Control,
    ControlId, Direction, Placed,
};
use crate::theme::Theme;

/// A panel's half-finished gesture, carried between frames.
///
/// Not application state: nothing here is worth saving or undoing. But it cannot live inside a
/// frame either, because both fields are answers to "what is still going on".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelInput {
    /// Which control the pointer went down on and has not yet let go of.
    ///
    /// A slider follows the pointer after it leaves the row it started on: a hand dragging a value
    /// does not stay politely inside a 22-unit band, and a slider that let go the moment it did
    /// would be maddening. So the control is latched on press and released on release, exactly as
    /// the panel drag latches a tab.
    pub latch: Option<ControlId>,
    /// The text field that has the caret, and what it says while it is being typed into.
    ///
    /// **Here rather than in the [`Control`].** A control is a description of the panel, rebuilt
    /// whenever the panel is asked what it looks like; a description that changed under every
    /// keystroke would be rebuilt under every keystroke, and the words would go back to what the
    /// panel last committed the moment anything else moved.
    ///
    /// One at a time, because one caret is all there is.
    ///
    /// [`Control`]: crate::panel_ui::Control
    pub editing: Option<(ControlId, crate::text_field::TextField)>,
    /// How far down a list taller than its panel has been scrolled.
    pub scroll: f32,
    /// Where each [`Control::Custom`] ended up, and where the pointer is inside the panel.
    ///
    /// Handed back rather than worked out again by the panel: the engine decides where a control
    /// goes, and a panel that recomputed it would be a second answer to drift from the first.
    pub custom: Vec<(ControlId, Rect)>,
    /// Where the pointer is, if it is over this panel at all, and whether it is pressed.
    ///
    /// A custom drawing does its own hit-testing -- that is what makes it custom -- so it needs the
    /// pointer rather than a change.
    pub pointer: Option<(f32, f32)>,
    pub pressed: bool,
    /// The moment the main button went down, and the moment the other one did.
    ///
    /// **Edges, and which button.** `pressed` says a button is *held*, which is what a wheel or a
    /// curve wants; a palette wants to know a click happened, and a delete wants to know it was
    /// the other button. Without both, a right-click that removes a swatch removes one every frame
    /// it is held down, and a right-drag across the palette empties it.
    ///
    /// Two panels reached around this into `ui.input` for want of it, which is the seam this layer
    /// exists to keep. Reported here so nothing has to.
    pub clicked: bool,
    pub other_clicked: bool,
    /// Where the control that was last pressed sits.
    ///
    /// For anchoring something to the control that opened it -- a menu belongs under its own
    /// button, not wherever the pointer happened to be. Reported rather than guessed, because the
    /// panel that wants it has no idea where its controls were laid out.
    pub pressed_rect: Option<Rect>,
}

type Latch = Option<ControlId>;

fn to_egui(r: Rect) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.w, r.h))
}

fn color(c: crate::theme::Color) -> egui::Color32 {
    let [r, g, b] = c.0;
    egui::Color32::from_rgb(r, g, b)
}

/// Draw a panel's controls and report what the artist changed.
///
/// Values in, changes out: this never touches the state a control describes. The panel that built
/// the list applies the result, which is what keeps a control from needing a mutable borrow of the
/// whole application.
pub fn show(
    ui: &mut egui::Ui,
    controls: &[Control],
    theme: &Theme,
    direction: Direction,
    input: &mut PanelInput,
) -> Vec<Change> {
    let m = &theme.metrics;
    let area = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(area, egui::Sense::click_and_drag());
    // `area` is already the panel's controls rectangle, padded once by `chrome`. Padding again
    // here is what spent it twice.
    let visible = area.height().max(0.0);
    // Scrolling is a subtraction from where the list starts and nothing more, which is why `place`
    // has never heard of it. Clamped every frame rather than only when the wheel turns: the list
    // itself changes length when a layer is deleted, and an offset left pointing past the new end
    // would strand the panel showing nothing at all.
    let content = Rect::new(area.min.x, area.min.y, area.width().max(0.0), visible);
    // Laid out first and scrolled afterwards, so how tall the list is comes from where the
    // controls actually ended up rather than from a second calculation that could disagree.
    // A control drawing an icon has no label to make room for, so it measures as zero and asks
    // for a square. One question answered in one place: the width a control asks for and the thing
    // actually drawn in it cannot disagree.
    let text_of = |c: &Control| {
        if icon_for(c, theme).is_some() {
            0.0
        } else {
            text_width(ui.ctx(), m.body, c)
        }
    };
    // How tall a label's sentence is at the width it will get. Measured by the thing that will
    // draw it, so the room made and the room used are one answer.
    let tall_of = |c: &Control, w: f32| wrapped_height(ui.ctx(), m.body, c, w);
    let laid = place(controls, content, m, direction, &text_of, &tall_of);
    let (_, tall) = extent(&laid, content);

    // Only the panel under the pointer takes the wheel. The delta egui reports is the window's,
    // not this panel's, so applying it unconditionally would scroll every described panel on
    // screen at once.
    let wheel = if resp.hovered() {
        ui.input(|i| i.raw_scroll_delta.y)
    } else {
        0.0
    };
    input.scroll = clamp_scroll(input.scroll - wheel, tall, visible);
    // Whether this panel is being offered the wheel at all, for whoever is driving: a panel that
    // will not scroll is either not hovered or has nowhere to go, and those are different bugs.
    let offered = (resp.hovered(), wheel);
    if std::env::var_os("OPENPAINT_TRACE_INPUT").is_some() {
        let raw = ui.input(|i| i.raw_scroll_delta.y);
        if raw != 0.0 {
            println!(
                "panel wheel: raw={raw:+.1} taken={wheel:+.1} hovered={} scroll={:.1}                  tall={tall:.1} visible={visible:.1}",
                resp.hovered(),
                input.scroll
            );
        }
    }
    let scrolled = Rect::new(content.x, content.y - input.scroll, content.w, content.h);
    let placed = place(controls, scrolled, m, direction, &text_of, &tall_of);

    let mut changes = Vec::new();
    let pointer = resp.interact_pointer_pos();
    let down = ui.input(|i| i.pointer.any_down());
    let pressed = ui.input(|i| i.pointer.any_pressed());

    // A press either latches a slider and starts moving it, or waits for the release. A button
    // that fired on the way down could not be thought better of, and sliding off a control you
    // did not mean to press is the only way out that needs no undo.
    if let (Some(p), true) = (pointer, pressed) {
        if let Some((control, rect)) = hit(&placed, p.x, p.y) {
            if matches!(control, Control::Slider { .. }) {
                input.latch = control.id();
                changes.extend(change_at(control, rect, m, p.x, p.y));
            }
        }
    }

    // A latched slider keeps following, wherever the pointer has got to. `change_at` clamps, so
    // running off the end of the window pins the value rather than losing it.
    if let (Some(id), Some(p), true) = (input.latch, pointer, down) {
        if let Some(found) = placed.iter().find(|q| q.control.id() == Some(id)) {
            changes.extend(change_at(
                found.control,
                found.rect,
                m,
                p.x,
                found.rect.y + found.rect.h / 2.0,
            ));
        }
    }
    if !down {
        input.latch = None;
    }

    if resp.clicked() && input.latch.is_none() {
        if let Some(p) = pointer {
            if let Some((control, rect)) = hit(&placed, p.x, p.y) {
                if !matches!(control, Control::Slider { .. }) {
                    input.pressed_rect = Some(rect);
                    changes.extend(change_at(control, rect, m, p.x, p.y));
                }
            }
        }
    }

    // **A press anywhere that is not this field finishes it.** A field that kept the caret until
    // Enter would eat the next thing the artist did, and "I clicked the button and it typed into
    // the box" is not a bug anyone should have to report.
    if pressed {
        let still = pointer
            .and_then(|p| hit(&placed, p.x, p.y))
            .and_then(|(c, _)| matches!(c, Control::Text { .. }).then(|| c.id()).flatten());
        if input.editing.as_ref().map(|(id, _)| *id) != still {
            changes.extend(finish_editing(input));
        }
    }
    // A field the panel no longer offers cannot keep the caret either -- a layer renamed out from
    // under the field, a preset deleted.
    if input
        .editing
        .as_ref()
        .is_some_and(|(id, _)| !controls.iter().any(|c| c.id() == Some(*id)))
    {
        input.editing = None;
    }
    // Take the caret, keeping whatever is in the field as its starting point.
    for change in &changes {
        if let Change::Typing(id) = change {
            if input.editing.as_ref().map(|(held, _)| *held) != Some(*id) {
                let value = controls.iter().find_map(|c| match c {
                    Control::Text { id: q, value, .. } if q == id => Some(value.clone()),
                    _ => None,
                });
                let mut field = crate::text_field::TextField::new(value.unwrap_or_default());
                // Selected, so typing replaces rather than appends -- what every field does when
                // you tab into it, and what an artist renaming a layer means every time.
                field.select_all();
                input.editing = Some((*id, field));
            }
        }
    }
    // The keys, once the caret is somewhere. Read here rather than by the panel, because the panel
    // has no idea a field is being edited and should not have to.
    if input.editing.is_some() {
        let events = ui.input(|i| i.events.clone());
        let mut done = false;
        if let Some((_, field)) = input.editing.as_mut() {
            for event in &events {
                match event {
                    egui::Event::Text(t) => field.insert_str(t),
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => done |= apply_key(field, *key, *modifiers),
                    _ => {}
                }
            }
        }
        if done {
            changes.extend(finish_editing(input));
        }
    }

    input.custom.clear();
    for p in &placed {
        if let Control::Custom { id, .. } = p.control {
            input.custom.push((*id, p.rect));
        }
    }
    report_controls(&placed, m, content, input.scroll, tall, offered, ui.ctx());
    input.pointer = resp.hover_pos().or(pointer).map(|q| (q.x, q.y));
    input.pressed = down;
    // Only while this panel has the pointer: a click anywhere on the window is not a click on
    // whatever this panel happens to be drawing under it.
    let over = input.pointer.is_some();
    let (main_down, other_down) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.secondary_pressed()));
    input.clicked = over && main_down;
    input.other_clicked = over && other_down;

    let hover = resp
        .hover_pos()
        .and_then(|p| hit(&placed, p.x, p.y).and_then(|(c, _)| c.id()));
    let painter = ui.painter_at(area);
    for p in &placed {
        draw(&painter, p, theme, direction, hover, input.latch);
        // The field with the caret shows what is being typed, not what was last committed -- and
        // the caret itself, or it is indistinguishable from a field that is merely highlighted.
        if let (Control::Text { id, text, .. }, Some((held, field))) =
            (p.control, input.editing.as_ref())
        {
            if id == held {
                draw_editing(&painter, p.rect, text, field, theme, ui.ctx());
            }
        }
    }
    changes
}

fn draw(
    painter: &egui::Painter,
    p: &Placed<'_>,
    theme: &Theme,
    direction: Direction,
    hover: Latch,
    latch: Latch,
) {
    let (pal, m) = (&theme.palette, &theme.metrics);
    let r = p.rect;
    let id = p.control.id();
    let lit = id.is_some() && (id == hover || id == latch);
    let body = egui::FontId::proportional(m.body);
    let left = r.x + m.padding * 0.5;

    // A row under the pointer is marked the same faint way a tab and a divider are, because it is
    // the same statement: this is the thing you would get. One vocabulary, not three.
    if lit && !matches!(p.control, Control::Row { selected: true, .. }) {
        painter.rect_filled(to_egui(r), m.radius, color(pal.header));
    }

    match p.control {
        Control::Label { text } => {
            // Wrapped to the room it was given, which is the room `place` made for it after
            // measuring the same way. A label is the one control whose height is its content.
            let galley = painter.layout(
                text.clone(),
                egui::FontId::proportional(m.body),
                color(pal.dim),
                (r.x + r.w - left).max(1.0),
            );
            painter.galley(
                egui::pos2(left, r.y + m.padding * 0.25),
                galley,
                color(pal.dim),
            );
        }
        Control::Separator => {
            // A rule across a column, and the same rule turned on its side in a row. Which way it
            // runs is read from the rectangle it was given, so it can never disagree with the
            // direction the list was laid out in.
            let across = r.w > r.h;
            let (a, b) = if across {
                let x = (r.x + r.w / 2.0).round() + 0.5;
                (
                    egui::pos2(x, r.y + m.padding),
                    egui::pos2(x, r.y + r.h - m.padding),
                )
            } else {
                let y = (r.y + r.h / 2.0).round() + 0.5;
                (egui::pos2(r.x, y), egui::pos2(r.x + r.w, y))
            };
            let _ = direction;
            painter.line_segment([a, b], egui::Stroke::new(1.0_f32, color(pal.edge)));
        }
        Control::Button { text, .. } => {
            painter.rect_filled(
                to_egui(r),
                m.radius,
                color(if lit { pal.edge } else { pal.header }),
            );
            painter.text(
                egui::pos2(r.x + r.w / 2.0, r.y + r.h / 2.0),
                egui::Align2::CENTER_CENTER,
                text,
                body,
                color(pal.text),
            );
        }
        Control::Slider {
            text,
            value,
            min,
            max,
            unit,
            log,
            ..
        } => {
            // The whole row is the slider, and the fill *is* the control — not a thin track with a
            // knob riding on it. That is what makes it catchable anywhere: there is no small thing
            // to aim at. The label and the value sit on top of the fill rather than taking width
            // from it, so a narrow panel loses nothing.
            let t = to_fraction(*value, *min, *max, *log);
            painter.rect_filled(to_egui(r), m.radius, color(pal.header));
            if t > 0.0 {
                // **The accent, held back.** The fill was one neutral step above the track and was
                // all but invisible on screen -- a slider you cannot read at a glance is a number
                // you have to squint at, and no test was ever going to say so. Muted rather than
                // solid, because a solid accent is what "chosen" means elsewhere and a slider is
                // not a choice.
                let [sr, sg, sb] = pal.state.0;
                painter.rect_filled(
                    to_egui(Rect::new(r.x, r.y, r.w * t, r.h)),
                    m.radius,
                    egui::Color32::from_rgba_unmultiplied(sr, sg, sb, 110),
                );
            }
            painter.text(
                egui::pos2(left, r.y + r.h / 2.0),
                egui::Align2::LEFT_CENTER,
                text,
                body.clone(),
                color(pal.text),
            );
            painter.text(
                egui::pos2(r.x + r.w - m.padding * 0.5, r.y + r.h / 2.0),
                egui::Align2::RIGHT_CENTER,
                format_value(*value, *min, *max, unit),
                body,
                color(pal.dim),
            );
        }
        // **What is chosen now, and a mark saying there is more.** Drawn as the slider is, with
        // the label on the left and the value on the right, because from a distance a panel of
        // settings should read as one column of "name: value" whatever kind of setting each is.
        Control::Pick { text, value, .. } => {
            painter.rect_filled(
                to_egui(r),
                m.radius,
                color(if lit { pal.edge } else { pal.header }),
            );
            painter.text(
                egui::pos2(left, r.y + r.h / 2.0),
                egui::Align2::LEFT_CENTER,
                text,
                body.clone(),
                color(pal.text),
            );
            // Room for the mark, so a long value does not run into it.
            let room = m.padding.mul_add(0.5, m.row * 0.5);
            painter.text(
                egui::pos2(r.x + r.w - room, r.y + r.h / 2.0),
                egui::Align2::RIGHT_CENTER,
                value,
                body,
                color(pal.bright),
            );
            // A small triangle pointing down: the one mark that means "there is a list behind
            // this" in every application anybody has used.
            let cx = r.x + r.w - m.padding * 0.5 - m.row * 0.25;
            let cy = r.y + r.h / 2.0;
            let w = m.row * 0.22;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(cx - w, cy - w * 0.6),
                    egui::pos2(cx + w, cy - w * 0.6),
                    egui::pos2(cx, cy + w * 0.7),
                ],
                color(pal.dim),
                egui::Stroke::NONE,
            ));
        }
        // **A sunken well with words in it**, which is what a field looks like everywhere. The
        // caret and any selection are drawn over the top, by whoever is holding the editing state
        // -- see `draw_caret`.
        Control::Text { text, value, .. } => {
            painter.text(
                egui::pos2(left, r.y + r.h / 2.0),
                egui::Align2::LEFT_CENTER,
                text,
                body.clone(),
                color(pal.text),
            );
            let well = text_well(r, m, ui_text_left(r, m, text, body.size));
            painter.rect_filled(to_egui(well), m.radius, color(pal.ground));
            painter.rect_stroke(
                to_egui(well),
                m.radius,
                egui::Stroke::new(1.0_f32, color(if lit { pal.state } else { pal.edge })),
            );
            painter.text(
                egui::pos2(well.x + m.padding * 0.5, well.y + well.h / 2.0),
                egui::Align2::LEFT_CENTER,
                value,
                body,
                color(pal.bright),
            );
        }
        Control::Toggle { text, on, .. } => {
            painter.text(
                egui::pos2(left, r.y + r.h / 2.0),
                egui::Align2::LEFT_CENTER,
                text,
                body,
                color(pal.text),
            );
            // A pill, filled when on. Readable at a glance from tablet distance, which a tick in a
            // box is not.
            let w = m.row * 1.6;
            let h = m.row * 0.6;
            let pill = Rect::new(r.x + r.w - w - m.padding * 0.5, r.y + (r.h - h) / 2.0, w, h);
            painter.rect_filled(
                to_egui(pill),
                h / 2.0,
                color(if *on { pal.state } else { pal.edge }),
            );
            let d = h - 4.0;
            let cx = if *on {
                pill.x + w - 2.0 - d / 2.0
            } else {
                pill.x + 2.0 + d / 2.0
            };
            painter.circle_filled(
                egui::pos2(cx, pill.y + h / 2.0),
                d / 2.0,
                color(if *on { pal.on_state } else { pal.dim }),
            );
        }
        Control::Choice { text, selected, .. } => {
            painter.rect_filled(
                to_egui(r),
                m.radius,
                color(if *selected {
                    pal.state
                } else if lit {
                    pal.edge
                } else {
                    pal.header
                }),
            );
            let ink = color(if *selected { pal.on_state } else { pal.text });
            // The icon if the chosen set draws one, the label otherwise. "Words" is a set that
            // draws none, so it needs no special case anywhere: its absence *is* the choice.
            if let Some(marks) = icon_for(p.control, theme) {
                draw_icon(painter, marks, r, ink);
            } else {
                painter.text(
                    egui::pos2(r.x + r.w / 2.0, r.y + r.h / 2.0),
                    egui::Align2::CENTER_CENTER,
                    text,
                    body,
                    ink,
                );
            }
        }
        // Drawn by whoever asked for it, from the rectangle reported back in `PanelInput`.
        Control::Custom { .. } => {}
        Control::Row {
            text,
            selected,
            swatch,
            mark,
            ..
        } => {
            if *selected {
                painter.rect_filled(to_egui(r), m.radius, color(pal.state));
            }
            let mut x = left;
            if let Some([cr, cg, cb]) = *swatch {
                let s = m.row * 0.55;
                let chip = to_egui(Rect::new(x, r.y + (r.h - s) / 2.0, s, s));
                painter.rect_filled(chip, m.radius, egui::Color32::from_rgb(cr, cg, cb));
                painter.rect_stroke(chip, m.radius, egui::Stroke::new(1.0_f32, color(pal.edge)));
                x += s + m.padding * 0.5;
            }
            painter.text(
                egui::pos2(x, r.y + r.h / 2.0),
                egui::Align2::LEFT_CENTER,
                text,
                body,
                // `on_state` exists for exactly this: text on the accent, which the ordinary text
                // colour fails contrast against.
                color(if *selected { pal.on_state } else { pal.text }),
            );
            if let Some(mark) = mark {
                // Drawn where `mark_rect` says, because that is also where the press is read: one
                // definition, so the switch cannot be a few units away from its own target.
                let pill = mark_rect(r, m);
                painter.rect_filled(
                    to_egui(pill),
                    pill.h / 2.0,
                    color(if mark.on { pal.state } else { pal.edge }),
                );
                let d = pill.h - 4.0;
                let cx = if mark.on {
                    pill.x + pill.w - 2.0 - d / 2.0
                } else {
                    pill.x + 2.0 + d / 2.0
                };
                painter.circle_filled(
                    egui::pos2(cx, pill.y + pill.h / 2.0),
                    d / 2.0,
                    color(if mark.on { pal.on_state } else { pal.dim }),
                );
            }
        }
    }
}

/// The icon a control shows, if it names one and the chosen set draws it.
///
/// One answer, used to size the control and to draw it. Asked twice with two implementations, a
/// button would reserve room for a label it never shows.
fn icon_for(control: &Control, theme: &Theme) -> Option<&'static [crate::icons::Mark]> {
    let Control::Choice {
        icon: Some(symbol), ..
    } = control
    else {
        return None;
    };
    crate::icons::SETS
        .iter()
        .find(|s| s.id == theme.icons)
        .and_then(|s| s.glyph(*symbol))
}

/// The words being typed, the selection behind them, and the caret.
///
/// Drawn over the control rather than instead of it, so the well, the label and the lit border are
/// the ordinary ones and there is one description of what a field looks like.
fn draw_editing(
    painter: &egui::Painter,
    r: Rect,
    label: &str,
    field: &crate::text_field::TextField,
    theme: &Theme,
    ctx: &egui::Context,
) {
    let (m, pal) = (&theme.metrics, &theme.palette);
    let font = egui::FontId::proportional(m.body);
    let well = text_well(r, m, ui_text_left(r, m, label, m.body));
    let left = well.x + m.padding * 0.5;
    let mid = well.y + well.h / 2.0;
    let upto = |bytes: usize| {
        ctx.fonts(|f| {
            f.layout_no_wrap(
                field.text()[..bytes].to_owned(),
                font.clone(),
                egui::Color32::WHITE,
            )
            .size()
            .x
        })
    };
    // The selection first, so the words sit on it.
    if let Some(range) = field.selection() {
        let (a, b) = (upto(range.start), upto(range.end));
        let [sr, sg, sb] = pal.state.0;
        painter.rect_filled(
            to_egui(Rect::new(left + a, well.y + 2.0, b - a, well.h - 4.0)),
            0.0_f32,
            egui::Color32::from_rgba_unmultiplied(sr, sg, sb, 120),
        );
    }
    // Over the committed words, which are the same words unless the panel has fallen behind.
    painter.rect_filled(to_egui(well), m.radius, color(pal.ground));
    painter.text(
        egui::pos2(left, mid),
        egui::Align2::LEFT_CENTER,
        field.text(),
        font.clone(),
        color(pal.bright),
    );
    let caret = (left + upto(field.caret())).round() + 0.5;
    painter.line_segment(
        [
            egui::pos2(caret, well.y + 2.0),
            egui::pos2(caret, well.y + well.h - 2.0),
        ],
        egui::Stroke::new(1.0_f32, color(pal.bright)),
    );
}

/// Let go of the field that has the caret and report what it says.
///
/// **On the way out, once.** A name applied per keystroke renames a layer eight times and leaves
/// eight steps on the undo stack; one that is never applied at all is a field that pretends.
fn finish_editing(input: &mut PanelInput) -> Option<Change> {
    let (id, field) = input.editing.take()?;
    Some(Change::Typed(id, field.text().to_owned()))
}

/// One key, applied to the field with the caret. Returns whether the field is finished with.
///
/// The motions and the arithmetic are [`crate::text_field`]'s; this is only which key means which.
fn apply_key(
    field: &mut crate::text_field::TextField,
    key: egui::Key,
    mods: egui::Modifiers,
) -> bool {
    use crate::text_field::Motion;
    let word = mods.ctrl || mods.command;
    let motion = match key {
        egui::Key::ArrowLeft if word => Some(Motion::WordLeft),
        egui::Key::ArrowRight if word => Some(Motion::WordRight),
        egui::Key::ArrowLeft => Some(Motion::Left),
        egui::Key::ArrowRight => Some(Motion::Right),
        egui::Key::Home => Some(Motion::Home),
        egui::Key::End => Some(Motion::End),
        _ => None,
    };
    if let Some(motion) = motion {
        // Shift takes the selection with it, which is what shift means in every field there is.
        if mods.shift {
            field.extend_selection(motion);
        } else {
            field.move_caret(motion);
        }
        return false;
    }
    match key {
        egui::Key::Backspace => field.backspace(),
        egui::Key::Delete => field.delete(),
        egui::Key::A if word => field.select_all(),
        // **Both finish it**, and both keep what was typed. Escape putting the old words back is a
        // second rule about what a field remembers, and the way back from a name you did not mean
        // is the same undo as everything else.
        egui::Key::Enter | egui::Key::Escape => return true,
        _ => {}
    }
    false
}

/// Where the words in a text field start, which is after its label.
///
/// **One definition.** The well is drawn here and the caret is placed against it elsewhere; two
/// answers would put the caret beside the letters rather than between them, which reads as the
/// field being broken rather than as an arithmetic slip.
#[must_use]
pub fn ui_text_left(r: Rect, m: &crate::theme::Metrics, label: &str, size: f32) -> f32 {
    // A label of nothing takes no room and no gap either, so a field with no name is all field.
    if label.is_empty() {
        return r.x + m.padding * 0.5;
    }
    // Measured by the caller in a column; here the label is on the left and the well takes what is
    // left over, with a floor so a long name cannot squeeze the field out of existence.
    let want = size.mul_add(0.6 * label.chars().count() as f32, m.padding * 1.5);
    r.x + want.min(r.w * 0.5)
}

/// The well a text field's words sit in: from `left` to the end of the row.
#[must_use]
pub fn text_well(r: Rect, m: &crate::theme::Metrics, left: f32) -> Rect {
    Rect::new(
        left,
        r.y + m.padding * 0.25,
        (r.x + r.w - m.padding * 0.5 - left).max(m.row),
        (r.h - m.padding * 0.5).max(m.row * 0.6),
    )
}

/// Say which panel the controls that follow belong to.
///
/// Called by whoever knows -- the workspace, which decides what is drawn where. `panel_draw` is
/// handed a rectangle and a list and has no idea whose they are, which is the whole point of it.
pub fn report_panel(name: &str) {
    let Some(path) = std::env::var_os("OPENPAINT_CONTROLS") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "# {name}");
    }
}

/// Where one thing that is not a described control landed.
///
/// The two prompts are drawn with egui widgets rather than through the descriptor layer, because
/// they belong to the application and not to any panel -- so they are not in the atlas the way
/// everything else is, and the most dangerous UI here would be the one thing a scenario had to hit
/// by measuring a screenshot. The prompt that blocks every pen stroke should be the easiest thing
/// to press on purpose, not the hardest.
pub fn report_widget(name: &str, rect: egui::Rect, ppp: f32) {
    let Some(path) = std::env::var_os("OPENPAINT_CONTROLS") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        use std::io::Write as _;
        // Its own heading, and no viewport line: a prompt is a window of its own, and filed
        // under whichever panel reported last it would inherit that panel's scrolling.
        let _ = writeln!(
            f,
            "# prompt
0	{:.0}	{:.0}	{:.0}	{:.0}	{name}",
            rect.min.x * ppp,
            rect.min.y * ppp,
            rect.width() * ppp,
            rect.height() * ppp
        );
    }
}

/// Where a panel's tab landed, so a harness can bring that panel to the front by name.
pub fn report_tab(name: &str, rect: crate::layout::Rect, ppp: f32) {
    let Some(path) = std::env::var_os("OPENPAINT_CONTROLS") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        use std::io::Write as _;
        let _ = writeln!(
            f,
            "@ {name}	{:.0}	{:.0}	{:.0}	{:.0}",
            rect.x * ppp,
            rect.y * ppp,
            rect.w * ppp,
            rect.h * ppp
        );
    }
}

/// Write down where every control just landed, for whatever is driving the application.
///
/// **So a test can say "press Blend" instead of guessing a pixel.** Driving the real application
/// is the only way to find out whether it works -- tests of the pieces passed for a day while the
/// brush painted nothing -- but a script full of hand-measured coordinates is a script that breaks
/// on a different window size and lies about which control it pressed. This is the layout the
/// engine actually used, in the units the pointer speaks, written where a harness can read it.
///
/// One line per control: `panel-id control-id x y w h label`. Nothing is written unless somebody
/// asked, and asking costs one environment lookup per panel per frame.
fn report_controls(
    placed: &[crate::panel_ui::Placed<'_>],
    m: &crate::theme::Metrics,
    content: Rect,
    scroll: f32,
    tall: f32,
    offered: (bool, f32),
    ctx: &egui::Context,
) {
    use std::fmt::Write as _;
    let Some(path) = std::env::var_os("OPENPAINT_CONTROLS") else {
        return;
    };
    // The scale the pointer is in: the harness clicks in physical pixels and `place` works in
    // points, and a factor of 1.5 between them is a click a control's width away from where it
    // was meant to go.
    let ppp = ctx.pixels_per_point();
    let mut out = String::new();
    // The window onto the list, and how far down it we are. Without this a harness can read that
    // a control is at y=674 and has no way to know the panel stops at 420 -- so it clicks a
    // confident nowhere and calls it a pass.
    let _ = writeln!(
        out,
        "$	{:.0}	{:.0}	{:.0}	{:.0}	{:.0}	{:.0}	{}	{:.1}",
        content.x * ppp,
        content.y * ppp,
        content.w * ppp,
        content.h * ppp,
        scroll * ppp,
        tall * ppp,
        offered.0,
        offered.1
    );
    for p in placed {
        let Some(id) = p.control.id() else { continue };
        let name = match p.control {
            Control::Button { text, .. }
            | Control::Slider { text, .. }
            | Control::Toggle { text, .. }
            | Control::Choice { text, .. }
            | Control::Pick { text, .. }
            | Control::Text { text, .. }
            | Control::Row { text, .. } => text.as_str(),
            _ => "",
        };
        let _ = writeln!(
            out,
            "{id}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{name}",
            p.rect.x * ppp,
            p.rect.y * ppp,
            p.rect.w * ppp,
            p.rect.h * ppp
        );
        // A row's switch is its own thing to press -- the eye on a layer -- and it is not a
        // control of its own, so it needs saying separately or nothing driving the application
        // can reach it. The same rectangle `change_at` hit-tests against.
        if let Control::Row {
            mark: Some(mark), ..
        } = p.control
        {
            let r = crate::panel_ui::mark_rect(p.rect, m);
            let _ = writeln!(
                out,
                "{}	{:.0}	{:.0}	{:.0}	{:.0}	{name} mark",
                mark.id,
                r.x * ppp,
                r.y * ppp,
                r.w * ppp,
                r.h * ppp
            );
        }
    }
    // Appended, because every panel reports its own and the harness reads the lot. Truncated by
    // whoever starts the run.
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        use std::io::Write as _;
        let _ = f.write_all(out.as_bytes());
    }
}

/// How tall a label's words are once wrapped to `width`, or zero for anything that is not one.
///
/// The same `layout` the drawing uses, so the height made for it and the height it takes cannot
/// disagree -- which is what a hand-rolled "characters per line" guess would eventually do, and it
/// would do it as text clipped at the panel's edge.
#[must_use]
pub fn wrapped_height(ctx: &egui::Context, size: f32, control: &Control, width: f32) -> f32 {
    let Control::Label { text } = control else {
        return 0.0;
    };
    ctx.fonts(|f| {
        f.layout(
            text.clone(),
            egui::FontId::proportional(size),
            egui::Color32::WHITE,
            width.max(1.0),
        )
        .size()
        .y
    })
}

/// The width of a control's label, measured by the thing that will draw it.
///
/// A column never asks, so this costs nothing there.
pub fn text_width(ctx: &egui::Context, size: f32, control: &Control) -> f32 {
    let text = match control {
        Control::Label { text }
        | Control::Button { text, .. }
        | Control::Slider { text, .. }
        | Control::Toggle { text, .. }
        | Control::Choice { text, .. }
        | Control::Pick { text, .. }
        | Control::Text { text, .. }
        | Control::Row { text, .. } => text.as_str(),
        Control::Separator | Control::Custom { .. } => return 0.0,
    };
    ctx.fonts(|f| {
        f.layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(size),
            egui::Color32::WHITE,
        )
        .size()
        .x
    })
}

/// Draw an icon into a rectangle.
///
/// The marks are in a 0..1 unit square, so this is a multiply and an add. Square by construction:
/// an icon stretched to a wide button reads as a different icon, so the largest square that fits
/// is used and centred, and the rest of the button is left to the label or to nothing.
///
/// The stroke width is the theme's hairline. An icon drawn heavier than the rules and dividers
/// around it stops looking like part of the same drawing.
pub fn draw_icon(
    painter: &egui::Painter,
    marks: &[crate::icons::Mark],
    within: Rect,
    tint: egui::Color32,
) {
    use crate::icons::Mark;
    let side = within.w.min(within.h);
    if side <= 0.0 {
        return;
    }
    let (ox, oy) = (
        within.x + (within.w - side) / 2.0,
        within.y + (within.h - side) / 2.0,
    );
    let at = |p: [f32; 2]| egui::pos2(side.mul_add(p[0], ox), side.mul_add(p[1], oy));
    let stroke = egui::Stroke::new((side / 22.0).max(1.0), tint);
    for mark in marks {
        match mark {
            Mark::Path(points) => {
                let line: Vec<egui::Pos2> = points.iter().map(|p| at(*p)).collect();
                painter.add(egui::Shape::line(line, stroke));
            }
            Mark::Poly(points) => {
                // Cut into triangles first: most of these outlines are concave, and filling one as
                // a convex polygon draws a mess of overlapping wedges. That is exactly what the
                // Solid set looked like the first time it was rendered.
                //
                // One mesh for the whole outline, not one shape per triangle. Every filled shape
                // egui is handed grows an antialiasing fringe of its own, so a shape per triangle
                // spends several times the vertices *and* outlines each piece separately -- and on
                // a sheet of every icon at once that was enough to overflow a mesh's indices and
                // paint triangles spanning the entire image.
                let mut mesh = egui::Mesh::default();
                for p in points.iter() {
                    mesh.colored_vertex(at(*p), tint);
                }
                for tri in crate::icons::triangles(points) {
                    let idx = |i: usize| u32::try_from(i).unwrap_or(0);
                    mesh.add_triangle(idx(tri[0]), idx(tri[1]), idx(tri[2]));
                }
                if !mesh.is_empty() {
                    painter.add(egui::Shape::mesh(mesh));
                }
            }
            Mark::Circle { at: c, r, filled } => {
                let centre = at(*c);
                let radius = r * side;
                if *filled {
                    painter.circle_filled(centre, radius, tint);
                } else {
                    painter.circle_stroke(centre, radius, stroke);
                }
            }
        }
    }
}

/// Where a colour wheel goes and what it is currently showing.
///
/// A struct because the argument list grew past the lint, and that is a signal rather than a lint
/// to silence: these three are one thing -- the wheel as it stands this frame.
#[derive(Clone, Copy, Debug)]
pub struct WheelAt {
    pub within: Rect,
    pub shape: crate::colour_wheel::Shape,
    pub colour: crate::colour_wheel::Hsv,
}

/// Draw a colour wheel into a rectangle, and report a colour if the pointer is setting one.
///
/// **The geometry is `colour_wheel`'s and stays there.** Where the ring is, which region a point
/// is in, what colour a point means and where the current colour's marker goes are all decided by
/// something with no screen attached and proved against itself; this turns those answers into
/// triangles.
///
/// `holding` is which region the pointer took hold of, kept by the caller between frames: a drag
/// that wanders out of the ring must keep setting the hue rather than jumping to whatever is under
/// it, the same reason a slider latches.
pub fn draw_wheel(
    painter: &egui::Painter,
    theme: &Theme,
    at: WheelAt,
    input: &PanelInput,
    holding: &mut Option<crate::colour_wheel::Region>,
) -> Option<crate::colour_wheel::Hsv> {
    let WheelAt {
        within,
        shape,
        colour,
    } = at;
    let (pointer, pressed) = (input.pointer, input.pressed);
    use crate::colour_wheel::{Hsv, Region, Wheel};
    let wheel = Wheel::new(shape, within, colour);
    if wheel.is_empty() {
        return None;
    }
    let pal = &theme.palette;

    // The hue ring, as a fan of wedges. Twelve degrees is a chord that sags a third of a unit at
    // this size -- under the hairline everything else is drawn with, so a finer fan would cost
    // triangles and change no pixels.
    if let Some(ring) = wheel.hue_ring() {
        let mut mesh = egui::Mesh::default();
        let steps = 30;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let a = t * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let [r, g, b] = Hsv::new(t * 360.0, 1.0, 1.0).to_srgb8();
            let tint = egui::Color32::from_rgb(r, g, b);
            for radius in [ring.inner, ring.outer] {
                mesh.colored_vertex(
                    egui::pos2(
                        radius.mul_add(a.cos(), ring.centre.0),
                        radius.mul_add(a.sin(), ring.centre.1),
                    ),
                    tint,
                );
            }
        }
        for i in 0..steps {
            let q = i * 2;
            mesh.add_triangle(q, q + 1, q + 2);
            mesh.add_triangle(q + 1, q + 3, q + 2);
        }
        painter.add(egui::Shape::mesh(mesh));
    }

    // The saturation/value area. A square is four corners; a triangle is three, and its colours
    // are read from the wheel itself so the drawing cannot disagree with the press.
    if let Some(area) = wheel.sv_square() {
        let mut mesh = egui::Mesh::default();
        for corner in [
            (area.x, area.y),
            (area.x + area.w, area.y),
            (area.x, area.y + area.h),
            (area.x + area.w, area.y + area.h),
        ] {
            let [r, g, b] = wheel
                .colour_in(Region::Interior, corner.0, corner.1)
                .to_srgb8();
            mesh.colored_vertex(
                egui::pos2(corner.0, corner.1),
                egui::Color32::from_rgb(r, g, b),
            );
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 3, 2);
        painter.add(egui::Shape::mesh(mesh));
    }
    if let Some(corners) = wheel.sv_triangle() {
        let mut mesh = egui::Mesh::default();
        for (x, y) in corners {
            let [r, g, b] = wheel.colour_in(Region::Interior, x, y).to_srgb8();
            mesh.colored_vertex(egui::pos2(x, y), egui::Color32::from_rgb(r, g, b));
        }
        mesh.add_triangle(0, 1, 2);
        painter.add(egui::Shape::mesh(mesh));
    }
    if let Some(strip) = wheel.hue_strip() {
        let mut mesh = egui::Mesh::default();
        let steps = 24;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let [r, g, b] = Hsv::new(t * 360.0, 1.0, 1.0).to_srgb8();
            let tint = egui::Color32::from_rgb(r, g, b);
            let x = strip.w.mul_add(t, strip.x);
            mesh.colored_vertex(egui::pos2(x, strip.y), tint);
            mesh.colored_vertex(egui::pos2(x, strip.y + strip.h), tint);
        }
        for i in 0..steps {
            let q = i * 2;
            mesh.add_triangle(q, q + 1, q + 2);
            mesh.add_triangle(q + 1, q + 3, q + 2);
        }
        painter.add(egui::Shape::mesh(mesh));
    }

    // The markers, drawn where `marker` says -- which is the inverse of where a press reads, and
    // proved against it. A ring rather than a dot, so the colour underneath is not hidden by the
    // thing pointing at it.
    for region in [Region::Hue, Region::Interior] {
        if let Some((x, y)) = wheel.marker(region) {
            painter.circle_stroke(
                egui::pos2(x, y),
                5.0,
                egui::Stroke::new(
                    2.0_f32,
                    egui::Color32::from_rgb(pal.bright.0[0], pal.bright.0[1], pal.bright.0[2]),
                ),
            );
            painter.circle_stroke(
                egui::pos2(x, y),
                6.5,
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgb(pal.canvas.0[0], pal.canvas.0[1], pal.canvas.0[2]),
                ),
            );
        }
    }

    // A press takes hold of whichever region it landed in, and keeps it until let go: a drag that
    // wanders off the ring must keep setting the hue rather than jumping to whatever is under it.
    let Some((px, py)) = pointer else {
        *holding = None;
        return None;
    };
    if !pressed {
        *holding = None;
        return None;
    }
    if holding.is_none() {
        *holding = wheel.region_at(px, py);
    }
    holding.map(|region| wheel.colour_in(region, px, py))
}

/// How many decimals a value deserves, from how much room its range gives each step.
///
/// A brush radius of 14 wants "14", not "14.00"; an opacity of 0.35 needs both decimals or it
/// reads as 0. Deriving it from the range beats a per-control setting nobody would keep correct.
fn format_value(value: f32, min: f32, max: f32, unit: &str) -> String {
    let span = (max - min).abs();
    let text = if span >= 50.0 {
        format!("{value:.0}")
    } else if span >= 5.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    };
    if unit.is_empty() {
        text
    } else {
        format!("{text} {unit}")
    }
}
