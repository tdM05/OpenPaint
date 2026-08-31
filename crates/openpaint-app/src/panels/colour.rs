//! The colour wheel, and the colours kept with the document.
//!
//! One module per panel, exporting one function. See [`super`] for why.

use super::{Painting, Picked};
use crate::colour_wheel::{Hsv, Shape};
use crate::layout::Rect;
use crate::panel_ui::{Change, Control, ControlId};
use crate::theme::{Metrics, Theme};
use crate::ui::{BrushAction, Status};
use crate::workspace::Place;
use openpaint_core::Brush;

const WHEEL: ControlId = 0;
const FIRST_SHAPE: ControlId = 1;
/// Above the shapes, with room for wheels nobody has thought of yet: a fourth arrangement must not
/// silently land on the swatches' id.
const SWATCHES: ControlId = 16;
const KEEP: ControlId = 17;

const SHAPES: [(&str, Shape); 3] = [
    ("Ring", Shape::Ring),
    ("Triangle", Shape::Triangle),
    ("Square", Shape::Square),
];

/// What a change to this panel means, before anything is done about it.
///
/// Two kinds, because this panel has two kinds of answer: which wheel to draw is a *setting* the
/// panel applies in place, and everything else is a document edit only the shell may make. Split
/// out so both are decided by a function with no screen attached.
#[derive(Clone, Debug, PartialEq)]
enum Answer {
    /// Which arrangement of the wheel the artist wants.
    Wheel(Shape),
    /// Something the shell has to do.
    Ask(Picked),
}

/// The swatch grid's arithmetic: how many fit across, where colour *n* lands, what a point hits.
///
/// **A palette is a grid, so it is drawn rather than described** -- the second `Custom` in this
/// panel, and the harder of the two calls. [`Control::Row`] carries a `swatch` and would have cost
/// nothing: it lays out, scrolls and hit-tests with everything else. Three things ruled it out.
///
/// 1. *One colour per row is not a palette.* Thirty kept colours are thirty full-width rows to
///    scroll past, where the same thirty are four lines of chips. A palette is remembered by
///    position -- the skin tone is *there*, second line, third along -- and a list that reflows to
///    one column throws that away.
/// 2. *A row is a name with a chip on it.* These have no names. Every row would be a colour beside
///    an empty label, which reads as a list that failed to load.
/// 3. *Forgetting.* There is no right-click in the `Change` vocabulary. `RowMark` could carry it,
///    but a mark is a *switch*: it says on or off, and the engine flips it and reports the new
///    state. "Forget this" is not a state, and a delete dressed as a toggle is a control that lies
///    about what it does. It would also put a switch on every row, which is exactly what the old
///    panel avoided -- swatches interleaved with delete buttons stop reading as a palette.
///
/// A `Custom` is handed the raw pointer, so right-click to forget survives the port intact. The
/// cost is this arithmetic, which is why it is here, pure and tested at its edges, rather than
/// inline in the drawing where nothing could check it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Grid {
    /// One swatch, square.
    side: f32,
    gap: f32,
}

impl Grid {
    /// A swatch is one control tall, and square.
    ///
    /// Not the old panel's fixed 18 units: every other target in the app is `metrics.row`, which
    /// the theme's own test holds above the 4 mm pen floor and which moves when the theme does. A
    /// swatch too small to hit with a pen is a swatch you pick the wrong colour from.
    #[must_use]
    fn of(metrics: &Metrics) -> Self {
        Self {
            side: metrics.row,
            gap: metrics.gap,
        }
    }

    /// How many swatches fit across `width`.
    ///
    /// **Never zero.** A panel dragged narrower than one swatch still has to lay the palette out
    /// somehow, and a line of no colours divides by zero on the way to being drawn.
    #[must_use]
    fn per_row(self, width: f32) -> usize {
        let step = self.side + self.gap;
        if step <= 0.0 {
            return 1;
        }
        // The last swatch on a line needs no gap after it, so one gap is given back before
        // dividing -- otherwise a grid exactly wide enough for three shows two.
        let fits = ((width + self.gap) / step).floor().max(1.0);
        fits as usize
    }

    /// How tall the grid has to be to hold `count` colours in a panel `width` across.
    ///
    /// What the [`Control::Custom`] asks for, so the engine can stack and scroll it like anything
    /// else. An empty palette asks for nothing rather than for an empty band.
    #[must_use]
    fn height(self, count: usize, width: f32) -> f32 {
        if count == 0 {
            return 0.0;
        }
        let lines = count.div_ceil(self.per_row(width));
        (lines as f32).mul_add(self.side, (lines - 1) as f32 * self.gap)
    }

    /// Where colour `index` sits inside the rectangle the engine gave the grid.
    #[must_use]
    fn cell(self, index: usize, area: Rect) -> Rect {
        let per = self.per_row(area.w);
        let step = self.side + self.gap;
        let (line, column) = (index / per, index % per);
        Rect::new(
            (column as f32).mul_add(step, area.x),
            (line as f32).mul_add(step, area.y),
            self.side,
            self.side,
        )
    }

    /// Which colour a point is on, if it is on one.
    ///
    /// The inverse of [`Grid::cell`], written beside it for the same reason the slider's two halves
    /// are: a swatch drawn in one place and pressed in another reads as the palette handing back
    /// the wrong colour. The gaps belong to nobody -- a press between two chips must do nothing
    /// rather than pick whichever one the rounding fell towards.
    #[must_use]
    fn at(self, area: Rect, count: usize, x: f32, y: f32) -> Option<usize> {
        let per = self.per_row(area.w);
        let step = self.side + self.gap;
        let along = |p: f32, origin: f32| -> Option<usize> {
            let d = p - origin;
            if d < 0.0 {
                return None;
            }
            let n = (d / step).floor();
            (d - n * step < self.side).then_some(n as usize)
        };
        let column = along(x, area.x)?;
        let line = along(y, area.y)?;
        if column >= per {
            return None;
        }
        let index = line * per + column;
        (index < count).then_some(index)
    }
}

/// What the panel shows for this colour, this wheel, and these kept colours.
///
/// Split out from [`show`] the way every ported panel splits it: what appears is a question about
/// a palette and a width, and answering it needs no window.
#[must_use]
fn controls(
    color_srgb: [u8; 3],
    shape: Shape,
    palette: &[[u8; 3]],
    width: f32,
    metrics: &Metrics,
) -> Vec<Control> {
    let mut controls = vec![
        // **The first thing that genuinely cannot be described**, and so the first `Custom`.
        // A hue ring is not a list of controls, and pretending otherwise would have meant a
        // control kind that existed for one panel.
        Control::Custom {
            id: WHEEL,
            // Square, and generous: a wheel small enough to fit anywhere is one you cannot
            // pick a colour with.
            height: 190.0,
        },
        Control::Label {
            text: format!(
                "#{:02X}{:02X}{:02X}",
                color_srgb[0], color_srgb[1], color_srgb[2]
            ),
        },
        // The old panel had a "+" with the tooltip "Keep this colour with the document". There are
        // no tooltips here, so the label carries the whole meaning -- and a bare "+" over a
        // palette reads as "add a colour", which is what the wheel above it is already for.
        Control::Button {
            id: KEEP,
            text: "Keep this colour".to_owned(),
        },
    ];
    if palette.is_empty() {
        // An empty grid would be an unexplained band of nothing. Saying so also says what the
        // button above it does, which nothing else here would.
        controls.push(Control::Label {
            text: "No colours kept with this document yet.".to_owned(),
        });
    } else {
        controls.push(Control::Custom {
            id: SWATCHES,
            // A column gives a `Custom` the full content width, which is the width measured here,
            // so what the grid asks for and what it is given agree.
            height: Grid::of(metrics).height(palette.len(), width),
        });
        // The other half of the lost tooltip. Right-click is the only way to forget a colour and
        // nothing on screen suggests it, so it is said once, quietly, under the grid.
        controls.push(Control::Label {
            text: "Click to use, right-click to forget.".to_owned(),
        });
    }
    controls.push(Control::Separator);
    // The shapes come last: which wheel you like is set once and never again, where the palette is
    // touched on every page. What is used most sits nearest the wheel.
    controls.extend(
        SHAPES
            .iter()
            .enumerate()
            .map(|(i, (name, kind))| Control::Choice {
                id: FIRST_SHAPE + ControlId::try_from(i).unwrap_or(0),
                text: (*name).to_owned(),
                selected: shape == *kind,
                icon: None,
            }),
    );
    controls
}

/// What a change means. Nothing is applied here.
#[must_use]
fn answer(change: &Change) -> Option<Answer> {
    match change {
        Change::Pressed(KEEP) => Some(Answer::Ask(Picked::Brush(BrushAction::SaveColor))),
        Change::Chose(id) if *id >= FIRST_SHAPE => SHAPES
            .get((*id - FIRST_SHAPE) as usize)
            .map(|(_, shape)| Answer::Wheel(*shape)),
        other => {
            eprintln!("colour panel: unexpected {other:?}");
            None
        }
    }
}

/// What a press on the swatch at `index` means.
///
/// **Right-click forgets**, as it did in the old panel, so the grid stays a grid of colours rather
/// than a grid of colours each wearing a cross. By index rather than by value, because that is
/// what the document deletes by and two swatches may hold the same colour.
#[must_use]
fn pressed_swatch(secondary: bool, index: usize, rgb: [u8; 3]) -> Picked {
    Picked::Brush(if secondary {
        BrushAction::ForgetColor(index)
    } else {
        BrushAction::UseColor(rgb)
    })
}

/// Draw the kept colours into the rectangle the engine gave them.
///
/// Where each one goes comes from [`Grid::cell`], which is also what a press is read against, so
/// the two cannot drift.
fn draw_swatches(
    painter: &egui::Painter,
    theme: &Theme,
    area: Rect,
    palette: &[[u8; 3]],
    hovered: Option<usize>,
) {
    let grid = Grid::of(&theme.metrics);
    let m = &theme.metrics;
    for (index, rgb) in palette.iter().enumerate() {
        let r = grid.cell(index, area);
        // The height was asked for at last layout's width; if the panel has been dragged narrower
        // since, the last line would spill over whatever is drawn under it.
        if r.y + r.h > area.y + area.h + 0.5 {
            break;
        }
        let at = egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.w, r.h));
        painter.rect_filled(
            at,
            m.radius,
            egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
        );
        // A hairline border, so a white swatch on a light panel is still a swatch rather than a
        // gap -- and the theme's accent under the pointer, which is how every other control in the
        // app says "this is the one you would get".
        let lit = hovered == Some(index);
        let edge = if lit {
            theme.palette.state
        } else {
            theme.palette.edge
        };
        painter.rect_stroke(
            at,
            m.radius,
            egui::Stroke::new(
                if lit { 2.0_f32 } else { 1.0_f32 },
                egui::Color32::from_rgb(edge.0[0], edge.0[1], edge.0[2]),
            ),
        );
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
    // Nothing here opens a list, so there is no popup half to draw. The colour is a setting and is
    // applied in place; the palette belongs to the document, so it is asked for.
    let _ = (&mut *brush, place);
    let mut picked: Option<Picked> = None;
    let palette = state.palette;
    let colour = Hsv::from_srgb8(*color_srgb);
    // Measured before anything is drawn into `ui`, which is the same rectangle the engine will lay
    // the controls out in. Asking afterwards, or remembering last frame's, would be a second
    // answer to drift from the first -- visible as a grid a line too short after a resize.
    let width = ui.available_width();

    let controls = controls(
        *color_srgb,
        *paint.wheel_shape,
        palette,
        width,
        &paint.theme.metrics,
    );
    for change in paint.show(ui, &controls) {
        match answer(&change) {
            Some(Answer::Wheel(shape)) => *paint.wheel_shape = shape,
            Some(Answer::Ask(ask)) => picked = Some(ask),
            None => {}
        }
    }

    // Both custom rectangles are taken now and the borrow let go, because drawing the wheel needs
    // the same `paint` mutably.
    let (wheel_at, swatches_at) = {
        let rect_of = |id: ControlId| {
            paint
                .input
                .custom
                .iter()
                .find(|(held, _)| *held == id)
                .map(|(_, at)| *at)
        };
        (rect_of(WHEEL), rect_of(SWATCHES))
    };

    // Drawn after the controls, into the rectangles the engine gave them.
    if let Some(at) = wheel_at {
        let where_it_is = crate::panel_draw::WheelAt {
            within: at,
            shape: *paint.wheel_shape,
            colour,
        };
        if let Some(chosen) = crate::panel_draw::draw_wheel(
            ui.painter(),
            paint.theme,
            where_it_is,
            paint.input,
            paint.wheel_hold,
        ) {
            *color_srgb = chosen.to_srgb8();
        }
    }
    if let Some(at) = swatches_at {
        // **The one thing a described control could not do.** `PanelInput` reports where the
        // pointer is and whether it is down, which is what a wheel needs; a palette needs the
        // press *edges* and which button it was, and putting those there is `panel_draw`'s
        // business rather than something to be guessed at a second time here. So they are read
        // raw -- and only ever acted on with the pointer over a swatch of this panel, since
        // `input.pointer` comes from the panel's own response and is already `None` when
        // something else is on top of it.
        let (used, forgot) =
            ui.input(|i| (i.pointer.primary_clicked(), i.pointer.secondary_clicked()));
        let grid = Grid::of(&paint.theme.metrics);
        let on = paint
            .input
            .pointer
            .and_then(|(x, y)| grid.at(at, palette.len(), x, y));
        draw_swatches(ui.painter(), paint.theme, at, palette, on);
        if let Some((index, rgb)) = on.and_then(|i| palette.get(i).map(|rgb| (i, *rgb))) {
            if used || forgot {
                picked = Some(pressed_swatch(forgot, index, rgb));
            }
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Theme::default().metrics
    }

    fn grid() -> Grid {
        Grid::of(&metrics())
    }

    /// A palette of `n` distinguishable colours.
    fn palette(n: usize) -> Vec<[u8; 3]> {
        (0..n).map(|i| [i as u8, 0, 255 - i as u8]).collect()
    }

    /// Wide enough for exactly `n` swatches across, and not one unit more.
    fn width_for(n: usize) -> f32 {
        let g = grid();
        (n as f32).mul_add(g.side, (n - 1) as f32 * g.gap)
    }

    fn custom_height(controls: &[Control], id: ControlId) -> Option<f32> {
        controls.iter().find_map(|c| match c {
            Control::Custom { id: held, height } if *held == id => Some(*height),
            _ => None,
        })
    }

    /// The wheel, the hex readout and the three shapes survive whatever the palette is doing.
    #[test]
    fn the_wheel_and_its_shapes_are_always_there() {
        for n in [0, 1, 40] {
            let list = controls(
                [18, 52, 86],
                Shape::Triangle,
                &palette(n),
                200.0,
                &metrics(),
            );
            assert!(custom_height(&list, WHEEL).is_some(), "{n}: no wheel");
            assert!(
                list.iter()
                    .any(|c| matches!(c, Control::Label { text } if text == "#123456")),
                "{n}: no hex readout"
            );
            let shapes: Vec<(ControlId, bool)> = list
                .iter()
                .filter_map(|c| match c {
                    Control::Choice { id, selected, .. } => Some((*id, *selected)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                shapes,
                vec![
                    (FIRST_SHAPE, false),
                    (FIRST_SHAPE + 1, true),
                    (FIRST_SHAPE + 2, false)
                ],
                "{n}: the shapes should be offered with the current one marked"
            );
            assert!(
                list.iter()
                    .any(|c| matches!(c, Control::Button { id, .. } if *id == KEEP)),
                "{n}: nothing to keep a colour with"
            );
        }
    }

    /// **An empty palette is not an empty band.** No grid at all, and a line saying so -- a
    /// zero-height `Custom` would still take a gap and would explain nothing.
    #[test]
    fn an_empty_palette_shows_no_grid() {
        let list = controls([0, 0, 0], Shape::Ring, &[], 200.0, &metrics());
        assert_eq!(custom_height(&list, SWATCHES), None);
        assert!(list
            .iter()
            .any(|c| matches!(c, Control::Label { text } if text.contains("No colours kept"))));
    }

    /// One colour is one line of grid, and the hint that says how to be rid of it appears with the
    /// first swatch rather than never.
    #[test]
    fn one_colour_is_one_line() {
        let list = controls([0, 0, 0], Shape::Ring, &palette(1), 200.0, &metrics());
        assert_eq!(custom_height(&list, SWATCHES), Some(grid().side));
        assert!(list
            .iter()
            .any(|c| matches!(c, Control::Label { text } if text.contains("right-click"))));
    }

    /// The grid asks for exactly the room the colours need, at the width it was given.
    #[test]
    fn the_grid_is_as_tall_as_its_colours() {
        let g = grid();
        let width = width_for(4);
        let line = |n: usize| g.side.mul_add(n as f32, (n - 1) as f32 * g.gap);
        for (count, lines) in [(1, 1), (4, 1), (5, 2), (8, 2), (9, 3)] {
            let list = controls([0, 0, 0], Shape::Ring, &palette(count), width, &metrics());
            assert_eq!(
                custom_height(&list, SWATCHES),
                Some(line(lines)),
                "{count} colours, four across, should be {lines} lines"
            );
        }
    }

    /// **A line exactly full is one line.** Off by one here and every full line of colours pushes
    /// an empty one under itself, which is the arithmetic that makes a panel scroll past its end.
    #[test]
    fn a_line_exactly_full_does_not_start_another() {
        let g = grid();
        for across in [1_usize, 3, 7] {
            let width = width_for(across);
            assert_eq!(g.per_row(width), across, "{across} should fit in {width}");
            assert_eq!(g.height(across, width), g.side, "{across}: a second line");
            assert!(
                g.height(across + 1, width) > g.side,
                "{across}: one more colour did not start a line"
            );
            // And half a unit narrower than a whole swatch fits one fewer, gaps included.
            if across > 1 {
                assert_eq!(g.per_row(width - 0.5), across - 1);
            }
        }
    }

    /// A panel dragged narrower than one swatch still lays out: one across, never none.
    #[test]
    fn a_line_always_holds_at_least_one() {
        let g = grid();
        for width in [-40.0, 0.0, 1.0, g.side / 2.0] {
            assert_eq!(g.per_row(width), 1, "at width {width}");
            assert_eq!(g.height(3, width), g.side.mul_add(3.0, g.gap * 2.0));
        }
    }

    /// **Where a swatch is drawn is where pressing it finds it.** Drawn by one rule and hit by
    /// another, the two drift, and the symptom is a palette that hands back the wrong colour.
    #[test]
    fn every_swatch_is_pressed_where_it_is_drawn() {
        let g = grid();
        let width = width_for(4);
        let area = Rect::new(7.0, 11.0, width, g.height(10, width));
        for index in 0..10 {
            let cell = g.cell(index, area);
            for (dx, dy) in [
                (0.0, 0.0),
                (g.side / 2.0, g.side / 2.0),
                (g.side - 0.5, g.side - 0.5),
            ] {
                assert_eq!(
                    g.at(area, 10, cell.x + dx, cell.y + dy),
                    Some(index),
                    "swatch {index} is not where it was drawn"
                );
            }
            assert!(
                cell.x + cell.w <= area.x + area.w + 0.001,
                "swatch {index} runs off the side of the grid"
            );
        }
    }

    /// The gaps belong to nobody. A press between two chips must do nothing rather than pick
    /// whichever one the rounding fell towards.
    #[test]
    fn the_gap_between_two_swatches_belongs_to_neither() {
        let g = grid();
        assert!(g.gap > 0.0, "with no gap there is nothing to test");
        let width = width_for(3);
        let area = Rect::new(0.0, 0.0, width, g.height(6, width));
        let mid = g.side / 2.0;
        // Between the first two along, and between the two lines.
        let between = g.side + g.gap / 2.0;
        assert_eq!(g.at(area, 6, between, mid), None, "the gap across");
        assert_eq!(g.at(area, 6, mid, between), None, "the gap down");
        // The swatches either side of that gap are still hit.
        assert_eq!(g.at(area, 6, g.side - 0.5, mid), Some(0));
        assert_eq!(g.at(area, 6, g.side + g.gap + 0.5, mid), Some(1));
    }

    /// Nothing outside the colours answers: an empty palette, the holes left by a half-full line,
    /// past the last column, and above or left of the grid entirely.
    #[test]
    fn a_point_off_the_colours_hits_nothing() {
        let g = grid();
        let width = width_for(3);
        let area = Rect::new(5.0, 5.0, width, g.height(4, width));
        assert_eq!(g.at(area, 0, 6.0, 6.0), None, "an empty palette");
        // Four colours, three across: the second line has one, so the other two are holes.
        for index in [4, 5] {
            let cell = g.cell(index, area);
            assert_eq!(
                g.at(area, 4, cell.x + 1.0, cell.y + 1.0),
                None,
                "the hole at {index} answered"
            );
        }
        for past in [width + 1.0, width + 30.0] {
            assert_eq!(
                g.at(area, 4, area.x + past, area.y + 1.0),
                None,
                "past the last column by {past}"
            );
        }
        assert_eq!(
            g.at(area, 4, area.x - 1.0, area.y + 1.0),
            None,
            "left of it"
        );
        assert_eq!(g.at(area, 4, area.x + 1.0, area.y - 1.0), None, "above it");
    }

    /// A press means what it looks like it means, and an id from nowhere means nothing.
    #[test]
    fn a_change_means_what_it_says() {
        assert_eq!(
            answer(&Change::Pressed(KEEP)),
            Some(Answer::Ask(Picked::Brush(BrushAction::SaveColor)))
        );
        for (i, (_, shape)) in SHAPES.iter().enumerate() {
            assert_eq!(
                answer(&Change::Chose(FIRST_SHAPE + i as ControlId)),
                Some(Answer::Wheel(*shape))
            );
        }
        // A shape past the end of the list, and controls this panel never offers.
        assert_eq!(answer(&Change::Chose(FIRST_SHAPE + 9)), None);
        assert_eq!(answer(&Change::Pressed(SWATCHES)), None);
        assert_eq!(answer(&Change::Toggled(KEEP, true)), None);
    }

    /// Left click uses a colour, right click forgets it.
    #[test]
    fn left_uses_a_colour_and_right_forgets_it() {
        assert_eq!(
            pressed_swatch(false, 2, [9, 8, 7]),
            Picked::Brush(BrushAction::UseColor([9, 8, 7]))
        );
        assert_eq!(
            pressed_swatch(true, 2, [9, 8, 7]),
            Picked::Brush(BrushAction::ForgetColor(2))
        );
    }
}
