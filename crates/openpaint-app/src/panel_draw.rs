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
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PanelInput {
    /// Which control the pointer went down on and has not yet let go of.
    ///
    /// A slider follows the pointer after it leaves the row it started on: a hand dragging a value
    /// does not stay politely inside a 22-unit band, and a slider that let go the moment it did
    /// would be maddening. So the control is latched on press and released on release, exactly as
    /// the panel drag latches a tab.
    pub latch: Option<ControlId>,
    /// How far down a list taller than its panel has been scrolled.
    pub scroll: f32,
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
    let text_of = |c: &Control| text_width(ui.ctx(), m.body, c);
    let laid = place(controls, content, m, direction, text_of);
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
    let scrolled = Rect::new(content.x, content.y - input.scroll, content.w, content.h);
    let placed = place(controls, scrolled, m, direction, text_of);
    let latch = &mut input.latch;

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
                *latch = control.id();
                changes.extend(change_at(control, rect, m, p.x, p.y));
            }
        }
    }

    // A latched slider keeps following, wherever the pointer has got to. `change_at` clamps, so
    // running off the end of the window pins the value rather than losing it.
    if let (Some(id), Some(p), true) = (*latch, pointer, down) {
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
        *latch = None;
    }

    if resp.clicked() && latch.is_none() {
        if let Some(p) = pointer {
            if let Some((control, rect)) = hit(&placed, p.x, p.y) {
                if !matches!(control, Control::Slider { .. }) {
                    input.pressed_rect = Some(rect);
                    changes.extend(change_at(control, rect, m, p.x, p.y));
                }
            }
        }
    }

    let hover = resp
        .hover_pos()
        .and_then(|p| hit(&placed, p.x, p.y).and_then(|(c, _)| c.id()));
    let painter = ui.painter_at(area);
    for p in &placed {
        draw(&painter, p, theme, direction, hover, *latch);
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
            painter.text(
                egui::pos2(left, r.y + r.h / 2.0),
                egui::Align2::LEFT_CENTER,
                text,
                egui::FontId::proportional(m.label),
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
                painter.rect_filled(
                    to_egui(Rect::new(r.x, r.y, r.w * t, r.h)),
                    m.radius,
                    color(pal.edge),
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
            painter.text(
                egui::pos2(r.x + r.w / 2.0, r.y + r.h / 2.0),
                egui::Align2::CENTER_CENTER,
                text,
                body,
                color(if *selected { pal.on_state } else { pal.text }),
            );
        }
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
        | Control::Row { text, .. } => text.as_str(),
        Control::Separator => return 0.0,
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
