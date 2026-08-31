//! The brush: what the mark looks like, and the brushes kept from last time.
//!
//! One module per panel, exporting one function. See [`super`] for why.
//!
//! This is the panel the descriptor layer was measured against. Four sliders proved a slider could
//! be described; the rest of the old section is what proves the *vocabulary* -- a list of saved
//! brushes, a field of words, six dropdowns, and seven drawings that are not lists of controls at
//! all.

use super::{Painting, Picked};
use crate::layout::Rect;
use crate::panel_ui::{Change, Control, ControlId};
use crate::theme::Theme;
use crate::ui::{BrushAction, Status};
use crate::workspace::Place;
use openpaint_core::{Brush, BrushPreset, Curve, Source};

// Preset rows are numbered by index, so everything above them starts where no brush library could
// reach. A row and a command sharing an id would be a silent mis-hit -- pressing "Ink" would reset
// the brush -- and nothing about the symptom would point back at the arithmetic. The same
// base-constant pattern as Layers and Pages, and it is tested below rather than eyeballed.
const FIRST_FORGET: ControlId = 1 << 19;
const FIRST_COMMAND: ControlId = 1 << 20;
const SIZE: ControlId = FIRST_COMMAND + 1;
const HARDNESS: ControlId = FIRST_COMMAND + 2;
const SPACING: ControlId = FIRST_COMMAND + 3;
const ROUNDNESS: ControlId = FIRST_COMMAND + 4;
const ANGLE: ControlId = FIRST_COMMAND + 5;
const STABILIZATION: ControlId = FIRST_COMMAND + 6;
const FLOW: ControlId = FIRST_COMMAND + 7;
const OPACITY: ControlId = FIRST_COMMAND + 8;
const PRESET_NAME: ControlId = FIRST_COMMAND + 9;
const SAVE_PRESET: ControlId = FIRST_COMMAND + 10;
const LOAD_TIP: ControlId = FIRST_COMMAND + 11;
const ROUND_TIP: ControlId = FIRST_COMMAND + 12;
const RESET: ControlId = FIRST_COMMAND + 13;
const EDGE_CURVE: ControlId = FIRST_COMMAND + 14;
// The six modulated parameters are numbered by their position in `Brush::responses_mut`, so each
// gets two ids out of two blocks rather than a hand-written constant per parameter -- adding a
// modulatable parameter to the engine has to make an editor appear here, not need a second edit
// somebody forgets.
const FIRST_SOURCE: ControlId = FIRST_COMMAND + 0x100;
const FIRST_CURVE: ControlId = FIRST_COMMAND + 0x200;
/// How wide each of those two blocks is.
const BLOCK: ControlId = 0x100;

/// How many parameters [`Brush::responses_mut`] reports.
///
/// Named so the id blocks and the tests read against one number rather than against six written
/// out, and so the test that the engine still reports six has something to compare with.
const RESPONSES: usize = 6;

/// How big a curve editor is, in logical units.
///
/// Square, and the same 140 the old panel used: a curve small enough to tuck into a corner is one
/// whose points cannot be aimed at, and there are seven of these down one panel.
const CURVE_SIDE: f32 = 140.0;

/// How near a press has to land to take hold of a point, in logical units.
///
/// Generous rather than exact, because this is a pen target: 10 units is about 2.6 mm, so a point
/// drawn 3.5 units across is still catchable without hunting (§1b).
const REACH: f32 = 10.0;

/// The inputs a parameter may be driven by, as the dropdown lists them.
///
/// Built rather than held, because [`Source::label`] is the one place a source's name lives and a
/// second copy here would be a name to keep in step for nothing.
#[must_use]
fn sources() -> Vec<String> {
    Source::ALL.iter().map(|s| s.label().to_owned()).collect()
}

/// What the panel shows for this brush and this library.
///
/// Split out from [`show`] because everything interesting about this panel is which controls come
/// out of which brush, and that is a question about a struct rather than about a GPU.
///
/// **Takes the brush by `&mut` and changes nothing.** [`Brush::responses_mut`] is the only way to
/// walk the six modulated parameters, and walking them is exactly what stops this file from
/// carrying its own copy of the list -- add a seventh to the engine and an editor for it appears
/// here. A read-only twin in the engine would be a second list to keep in step, which is the thing
/// being avoided.
#[must_use]
fn controls(
    brush: &mut Brush,
    presets: &[BrushPreset],
    trouble: Option<&str>,
    name: &str,
) -> Vec<Control> {
    let mut controls = Vec::new();
    let label = |text: &str| Control::Label {
        text: text.to_owned(),
    };

    // The brush library is a file in the data directory, and a file can be unreadable or
    // unwritable. Said in the panel rather than only on stderr: the artist is the one who has to act
    // on it, and a Save that silently does nothing is the failure §6b is about.
    if let Some(trouble) = trouble {
        controls.push(label(trouble));
    }
    // The saved brushes come first, because picking one is the common action and dialling sliders is
    // the rare one. Nobody draws a page with a single brush, so this is the section that turns a
    // demo into a tool.
    for (index, preset) in presets.iter().enumerate() {
        let row = ControlId::try_from(index).unwrap_or(ControlId::MAX);
        controls.push(Control::Row {
            id: row,
            text: preset.name.clone(),
            // Nothing here is "the current brush": applying a preset copies its settings into the
            // brush and the two drift apart the moment a slider moves, so a row drawn as selected
            // would be claiming something that is no longer true.
            selected: false,
            swatch: None,
            // **Not a [`crate::panel_ui::RowMark`], though the old panel's "x" sat exactly there.**
            // A mark is a *switch*: it says on or off, the engine flips it and reports the new
            // state. "Forget this" is not a state, and forgetting a brush deletes a file in the
            // library with no way back -- a delete dressed as a toggle is a control that lies about
            // what it does, and one that invites the stray tap that does it. `colour.rs` reached the
            // same conclusion about the palette from the same premise; this follows it.
            mark: None,
        });
        // So the way out is a button, under the row it acts on and named after it -- the same
        // arrangement Pages uses for the commands that belong to a page. It costs a second line per
        // brush, which is the price of the press being labelled with what it will do.
        controls.push(Control::Button {
            id: FIRST_FORGET + row,
            text: format!("Forget {}", preset.name),
        });
    }
    controls.push(Control::Text {
        id: PRESET_NAME,
        text: "Name".to_owned(),
        value: name.to_owned(),
    });
    controls.push(Control::Button {
        id: SAVE_PRESET,
        text: "Save brush".to_owned(),
    });
    controls.push(label(
        "Keep these settings under that name; saving over a name you have already used updates it. \
         A brush keeps its size, edge, shape and response curves -- but never your colour.",
    ));
    controls.push(Control::Separator);

    controls.push(Control::Slider {
        id: SIZE,
        text: "Size".to_owned(),
        value: brush.radius,
        min: 0.5,
        max: 400.0,
        unit: "px",
        // Logarithmic, because one step at radius 4 should feel like one step at radius 40 -- which
        // is what a paint app means by size.
        log: true,
    });
    controls.push(Control::Slider {
        id: HARDNESS,
        text: "Hardness".to_owned(),
        value: brush.hardness,
        min: 0.0,
        max: 1.0,
        unit: "",
        log: false,
    });
    controls.push(Control::Slider {
        id: SPACING,
        text: "Spacing".to_owned(),
        value: brush.spacing,
        min: 0.01,
        max: 1.0,
        unit: "",
        log: false,
    });
    controls.push(Control::Slider {
        id: ROUNDNESS,
        text: "Roundness".to_owned(),
        value: brush.roundness,
        // Never zero: a dab with no minor axis has no area and would stamp nothing at all.
        min: 0.02,
        max: 1.0,
        unit: "",
        log: false,
    });
    controls.push(Control::Slider {
        id: ANGLE,
        text: "Angle (turns)".to_owned(),
        value: brush.angle,
        min: 0.0,
        max: 1.0,
        unit: "",
        log: false,
    });
    // The old panel named the ends of the first two sliders in the number itself -- "0.00 soft",
    // "1.00 round" -- and drew the angle in degrees. A described slider has one unit and no
    // formatter, so what those numbers said is said here instead: a bare number gives no clue which
    // way round it goes, and that is information the panel may not simply drop.
    controls.push(label(
        "0 hardness is soft and 1 is hard; 1 roundness is a circle; one turn is 360 degrees. A \
         flattened dab is a chisel nib -- point Angle at Direction with a straight curve and it \
         follows the stroke, which is where inked line weight comes from.",
    ));
    controls.push(label(
        "Alt+click picks the colour under the pointer, sampled from the composited image rather \
         than from one layer.",
    ));
    controls.push(Control::Separator);

    controls.push(Control::Slider {
        id: STABILIZATION,
        text: "Stabilization".to_owned(),
        value: brush.stabilization_ms,
        min: 0.0,
        max: openpaint_core::stabilizer::MAX_LAG_MS,
        // **The control is denominated in its own price.** A one-pole filter trails its input by
        // exactly its time constant, so the setting *is* the latency it adds -- there is no abstract
        // "strength" to translate and no invented maximum to scale against. Latency being the top
        // quality axis (DECISIONS §4.1), the artist should be spending it in units they can compare
        // with the Speed readout, which is why the unit is on the slider rather than in the name.
        unit: "ms",
        log: false,
    });
    controls.push(label(if brush.stabilization_ms <= 0.0 {
        "Off. Smooths pen shake; the cost is lag, in the same milliseconds as the stroke time under \
         Speed."
    } else {
        "Smooths pen shake. The line trails the pen by this long, on top of the stroke time under \
         Speed."
    }));
    controls.push(Control::Separator);

    controls.push(label(
        "What drives each parameter, and how. Pick an input, then shape the curve: drag a point, \
         press empty space to add one, right-click a point to remove it. A flat curve means the \
         input is ignored. The slider above is what you get at full input.",
    ));
    // Stacked rather than side by side, and each at the left margin. Side by side put the
    // right-hand editor's top-right corner under the scroll bar's hover zone, which floats over the
    // content -- so the one control at (1, 1) could not be grabbed at all.
    for (i, (name, response)) in brush.responses_mut().into_iter().enumerate() {
        let i = ControlId::try_from(i).unwrap_or(0);
        controls.push(Control::Pick {
            id: FIRST_SOURCE + i,
            text: name.to_owned(),
            value: response.source.label().to_owned(),
        });
        controls.push(Control::Custom {
            id: FIRST_CURVE + i,
            height: CURVE_SIDE,
        });
    }
    controls.push(Control::Separator);

    controls.push(label("Tip"));
    if let Some(stamp) = brush.tip.stamp() {
        controls.push(label(&format!(
            "Bitmap, {}x{}",
            stamp.width(),
            stamp.height()
        )));
        controls.push(label(
            "A bitmap tip carries its own edge, so hardness and the edge profile do not apply to \
             it.",
        ));
        controls.push(Control::Button {
            id: ROUND_TIP,
            text: "Back to a round tip".to_owned(),
        });
    } else {
        controls.push(label(
            "The dab's edge profile: how coverage falls from the solid core out to the rim. Not \
             driven by anything -- its axis is distance within the dab, not an input. A straight \
             line is the plain ramp; bowing it out makes a marker, bowing it in makes an airbrush.",
        ));
        controls.push(Control::Custom {
            id: EDGE_CURVE,
            height: CURVE_SIDE,
        });
    }
    controls.push(Control::Button {
        id: LOAD_TIP,
        text: "Load brush tip\u{2026}".to_owned(),
    });
    // The old panel had this as hover text. There is no hover here, and a pen has no hover to give,
    // so what the tooltip said is said in the open.
    controls.push(label(
        "A PNG of the mark a single dab makes. Drawn on transparent, or drawn in black on white -- \
         either way, what looks like ink is ink.",
    ));
    controls.push(Control::Separator);

    // Flow and opacity sit together and low in the panel, exactly where the old section had them:
    // the paragraph under them is about the pair, and a label explaining two sliders four screens
    // above it explains nothing. That is why Opacity moved down from the four the panel started
    // with rather than staying at the top on its own.
    controls.push(Control::Slider {
        id: FLOW,
        text: "Flow".to_owned(),
        value: brush.flow,
        min: 0.0,
        max: 1.0,
        unit: "",
        log: false,
    });
    controls.push(Control::Slider {
        id: OPACITY,
        text: "Opacity".to_owned(),
        value: brush.opacity,
        min: 0.0,
        max: 1.0,
        unit: "",
        log: false,
    });
    // The old copy of this began "Direction = paint per dab", which is a rename that missed a word:
    // Direction is a modulation *source*, and the quantity meant is Flow. Brought across corrected
    // rather than faithfully, because this sentence is the only place the difference between the two
    // sliders is explained and it therefore has to be true.
    controls.push(label(
        "Flow = paint per dab. Opacity = ceiling for the whole stroke. Set flow low and opacity mid \
         to see build-up stop at the ceiling; lift and stroke again to go darker.",
    ));
    controls.push(Control::Separator);

    controls.push(Control::Button {
        id: RESET,
        text: "Reset to defaults".to_owned(),
    });
    controls.push(label(&format!(
        "Spacing is a fraction of diameter (Photoshop ~0.25), so dabs land every {:.2} px.",
        brush.radius * 2.0 * brush.spacing
    )));
    controls
}

/// Which source dropdown a change asks to open, if it asks for one.
///
/// Split off from [`answer`] because opening a list needs the rectangle the press came from, which
/// is [`super::open_pick`]'s business and not a pure function of the brush.
#[must_use]
fn source_pick(change: &Change) -> Option<usize> {
    match *change {
        Change::Pressed(id) if (FIRST_SOURCE..FIRST_SOURCE + BLOCK).contains(&id) => {
            let i = (id - FIRST_SOURCE) as usize;
            (i < RESPONSES).then_some(i)
        }
        _ => None,
    }
}

/// Apply what the artist did, and report anything the shell has to do about it.
///
/// **The brush is changed here rather than asked for**, because a brush is a setting and there is
/// nothing to undo about moving a slider. The library is not: saving, applying and forgetting a
/// preset all touch a file on disk, so those leave as a [`BrushAction`].
fn answer(
    change: &Change,
    brush: &mut Brush,
    color_srgb: &mut [u8; 3],
    presets: usize,
    name: &str,
) -> Option<Picked> {
    let library = |action| Some(Picked::Brush(action));
    match *change {
        Change::Set(SIZE, v) => brush.radius = v,
        Change::Set(HARDNESS, v) => brush.hardness = v,
        Change::Set(SPACING, v) => brush.spacing = v,
        Change::Set(ROUNDNESS, v) => brush.roundness = v,
        Change::Set(ANGLE, v) => brush.angle = v,
        Change::Set(STABILIZATION, v) => brush.stabilization_ms = v,
        Change::Set(FLOW, v) => brush.flow = v,
        Change::Set(OPACITY, v) => brush.opacity = v,
        // A press on a row applies that brush; the button under it forgets it. Both are bounded
        // against the library as it stands, because a list drawn on one frame and pressed on
        // another can have shrunk in between -- a brush saved or forgotten moves every row after it,
        // and indexing off the end would ask the shell for a brush that is not there.
        Change::Chose(index) if (index as usize) < presets => {
            return library(BrushAction::ApplyPreset(index as usize));
        }
        Change::Pressed(id) if (FIRST_FORGET..FIRST_COMMAND).contains(&id) => {
            let index = (id - FIRST_FORGET) as usize;
            if index < presets {
                return library(BrushAction::DeletePreset(index));
            }
            eprintln!("brush: there is no saved brush {index} to forget");
        }
        // Taking the caret is not a change to anything; it is reported so that a panel *can* know a
        // field is being typed into, and this one has no shortcut to get out of the way of.
        Change::Typing(PRESET_NAME) => {}
        Change::Typed(PRESET_NAME, ref typed) => return Some(Picked::PresetName(typed.clone())),
        // **A nameless brush is refused out loud rather than saved and puzzled over later**: a row
        // with no label is one nobody can pick on purpose. The old panel greyed the button out;
        // there is no disabled state in this vocabulary, so the button is drawn in every case and
        // the refusal says why -- the same call Pages and Layers make for Delete.
        Change::Pressed(SAVE_PRESET) if !name.trim().is_empty() => {
            return library(BrushAction::SavePreset);
        }
        Change::Pressed(SAVE_PRESET) => eprintln!("brush: give the brush a name before saving it"),
        Change::Pressed(LOAD_TIP) => return library(BrushAction::LoadTip),
        Change::Pressed(ROUND_TIP) => brush.tip = openpaint_core::dab::Tip::default(),
        Change::Pressed(RESET) => {
            *brush = Brush::default();
            // The colour goes back with it, because the default brush carries one and a swatch still
            // showing the old colour would be the panel disagreeing with what would be painted.
            *color_srgb = brush.color_srgb8();
        }
        // Not a catch-all out of laziness: an id this panel did not put in its own list is a bug in
        // the renderer, and swallowing it would be exactly the kind of silence §6b forbids.
        ref other => eprintln!("brush panel: unexpected {other:?}"),
    }
    None
}

// -- The curve editors ---------------------------------------------------------------------------
//
// A curve is direct manipulation of a shape, and a shape is not a stack of rectangles -- so it is a
// `Control::Custom`, exactly as the colour wheel is. The engine still decides where each one goes
// and how tall it is and hands the rectangle back through `PanelInput`; everything below works
// inside that rectangle and nowhere else.
//
// The arithmetic is split from the drawing on purpose. Which point a press takes hold of, where a
// drag may move it to and whether a point may be removed are pure functions of a list of points, so
// they are the part that can be tested -- and they are the part that is easy to get wrong.

/// The square a curve is drawn in, inside the row the engine gave it.
///
/// Square rather than stretched across the panel: a curve's x and y are both `0..1`, and drawing
/// them at different scales makes a 45-degree identity look like something else -- which is the one
/// reference the shape is read against. Left-aligned, so seven editors down a panel share a margin.
#[must_use]
fn curve_square(within: Rect) -> Rect {
    let side = within.w.min(within.h).max(1.0);
    Rect::new(within.x, within.y, side, side)
}

/// Where a point in curve space lands on the screen.
///
/// Curve space is (0,0) bottom-left to (1,1) top-right; screen y runs the other way.
#[must_use]
fn to_screen(square: Rect, p: (f32, f32)) -> (f32, f32) {
    (
        p.0.mul_add(square.w, square.x),
        (1.0 - p.1).mul_add(square.h, square.y),
    )
}

/// Where a point on the screen lands in curve space.
///
/// The inverse of [`to_screen`], and written next to it for the reason a slider's two halves are: a
/// point drawn at one place and grabbed at another reads as the editor being broken.
#[must_use]
fn to_curve(square: Rect, x: f32, y: f32) -> (f32, f32) {
    (
        ((x - square.x) / square.w).clamp(0.0, 1.0),
        ((square.y + square.h - y) / square.h).clamp(0.0, 1.0),
    )
}

/// The point nearest `at` within `reach`, in curve space.
#[must_use]
fn nearest(points: &[(f32, f32)], at: (f32, f32), reach: f32) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(i, p)| (i, (p.0 - at.0).hypot(p.1 - at.1)))
        .filter(|(_, d)| *d < reach)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// What a fresh press at `at` takes hold of: an existing point, or a new one put there.
///
/// **Press to add, rather than click to add.** The old editor added a point on a click and so could
/// not add one and place it in a single gesture; here the new point is grabbed by the press that
/// made it, which is what putting a point somewhere actually means.
///
/// Answers `None` when the press landed past either end, where there is nothing to grab: the ends
/// are what define the input range, so a curve may not grow one.
#[must_use]
fn press(points: &[(f32, f32)], at: (f32, f32), reach: f32) -> Option<(Vec<(f32, f32)>, usize)> {
    if let Some(i) = nearest(points, at, reach) {
        return Some((points.to_vec(), i));
    }
    let after = points.iter().position(|p| p.0 > at.0)?;
    if after == 0 {
        return None;
    }
    let mut next = points.to_vec();
    next.insert(after, at);
    Some((next, after))
}

/// The points with point `i` moved to `to`.
///
/// The ends may move in y but not in x, and an interior point stays between its neighbours -- a
/// curve whose points crossed over would have two answers at one input, which is not a curve.
#[must_use]
fn moved(points: &[(f32, f32)], i: usize, to: (f32, f32)) -> Vec<(f32, f32)> {
    let mut next = points.to_vec();
    // An index that is no longer there rather than a panic: the list is rebuilt every frame, and a
    // drag latched on one frame is applied on the next.
    let Some(here) = next.get(i).copied() else {
        return next;
    };
    let x = if i == 0 || i + 1 == next.len() {
        here.0
    } else {
        // A visible gap either side, so two points cannot be stacked into one dot that nothing can
        // pick apart again.
        to.0.clamp(next[i - 1].0 + 0.02, next[i + 1].0 - 0.02)
    };
    next[i] = (x, to.1);
    next
}

/// The points with the one nearest `at` taken out, or `None` if there is none to take out.
///
/// Never an end and never below two: an end is what defines the range, and a curve of one point has
/// no shape to read.
#[must_use]
fn forgotten(points: &[(f32, f32)], at: (f32, f32), reach: f32) -> Option<Vec<(f32, f32)>> {
    let i = nearest(points, at, reach)?;
    if points.len() <= 2 || i == 0 || i + 1 == points.len() {
        return None;
    }
    let mut next = points.to_vec();
    next.remove(i);
    Some(next)
}

/// Put an edited list of points back, or refuse it.
///
/// **Refused rather than repaired.** The clamping in [`moved`] and [`press`] should make a bad list
/// unreachable, and quietly sorting the points here would hide it if it were not -- which is how a
/// guard stops being tested without anybody noticing.
fn replace(curve: &mut Curve, points: Vec<(f32, f32)>) {
    if let Some(next) = Curve::from_points(points) {
        *curve = next;
    } else {
        eprintln!("brush: refused a set of points that does not make a curve");
    }
}

/// egui's colour for one of the theme's.
fn tint(c: crate::theme::Color) -> egui::Color32 {
    let [r, g, b] = c.0;
    egui::Color32::from_rgb(r, g, b)
}

/// Draw one curve into the square it was given.
fn draw_curve(painter: &egui::Painter, theme: &Theme, square: Rect, curve: &Curve) {
    let pal = &theme.palette;
    let at = |p: (f32, f32)| {
        let (x, y) = to_screen(square, p);
        egui::pos2(x, y)
    };
    let box_rect = egui::Rect::from_min_size(
        egui::pos2(square.x, square.y),
        egui::vec2(square.w, square.h),
    );
    // Inset against the panel's own ground, so an editor reads as a well rather than as ink floating
    // on the panel -- and so a curve running along an edge is still visibly inside something.
    painter.rect_filled(box_rect, theme.metrics.radius, tint(pal.canvas));
    painter.rect_stroke(
        box_rect,
        theme.metrics.radius,
        egui::Stroke::new(1.0_f32, tint(pal.edge)),
    );
    // The identity, as the reference a curve's shape is legible against: flat means the input is
    // ignored, and without a diagonal to compare with that is not something the eye can see.
    painter.line_segment(
        [at((0.0, 0.0)), at((1.0, 1.0))],
        egui::Stroke::new(1.0_f32, tint(pal.dim)),
    );
    // Sampled rather than drawn through the points, because the curve between two points is not a
    // straight line and drawing it as one would be the panel lying about what the brush will do.
    let samples: Vec<egui::Pos2> = (0..=48_u8)
        .map(|i| {
            let x = f32::from(i) / 48.0;
            at((x, curve.at(x)))
        })
        .collect();
    painter.add(egui::Shape::line(
        samples,
        egui::Stroke::new(2.0_f32, tint(pal.bright)),
    ));
    for p in curve.points() {
        painter.circle_filled(at(*p), 3.5, tint(pal.state));
    }
}

// -- State that has to survive between frames -----------------------------------------------------
//
// **Kept in egui's memory rather than in `Painting`, and that is a compromise rather than a
// design.** The wheel's grab lives in `Painting::wheel_hold` and a curve's belongs beside it, but
// `Painting` is declared in `ui.rs` and this change is confined to one file. Each slot is keyed by a
// constant id and is private to this module, so nothing else can read or collide with them; the cost
// is that the panel now knows egui holds state, which is precisely what the descriptor layer exists
// to keep it from knowing. See the report.

/// Which curve editor a drag has hold of, and which of its points.
///
/// One at a time, because one pointer is all there is -- and latched for the length of the drag
/// rather than re-picked each frame. Re-picking the nearest point every frame looks equivalent and
/// is not: drag two points close together and the drag hops to whichever is now nearer, so a point
/// ends up somewhere nobody aimed for.
fn held(ui: &egui::Ui) -> Option<(ControlId, usize)> {
    let id = egui::Id::new("openpaint-brush-curve-hold");
    ui.memory(|m| m.data.get_temp::<Option<(ControlId, usize)>>(id))
        .flatten()
}

fn set_held(ui: &egui::Ui, what: Option<(ControlId, usize)>) {
    let id = egui::Id::new("openpaint-brush-curve-hold");
    ui.memory_mut(|m| m.data.insert_temp(id, what));
}

/// Whether the pointer was already down on the previous frame.
///
/// The difference between a press that *started* on a curve and a drag that merely wandered over one
/// on its way from somewhere else. Without it, dragging the Size slider past the response section
/// would snatch a curve point on the way through.
fn was_down(ui: &egui::Ui) -> bool {
    let id = egui::Id::new("openpaint-brush-was-down");
    ui.memory(|m| m.data.get_temp::<bool>(id)).unwrap_or(false)
}

fn set_was_down(ui: &egui::Ui, down: bool) {
    let id = egui::Id::new("openpaint-brush-was-down");
    ui.memory_mut(|m| m.data.insert_temp(id, down));
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
    let mut picked: Option<Picked> = None;

    // The dropdowns' other half. Six of them, and `pick_popup` answers `None` for every one that is
    // not the open one -- so this is a loop rather than six arms, and there is still one popup up at
    // a time because there is one `paint.pick`.
    if place == Place::Popup {
        let options = sources();
        for (i, (_, response)) in brush.responses_mut().into_iter().enumerate() {
            let id = FIRST_SOURCE + ControlId::try_from(i).unwrap_or(0);
            let chosen = Source::ALL
                .iter()
                .position(|s| *s == response.source)
                .unwrap_or(0);
            if let Some(n) = super::pick_popup(id, &options, chosen, ui, paint) {
                if let Some(source) = Source::ALL.get(n) {
                    // Changed in place: which input drives a parameter is part of the brush, and a
                    // brush is a setting with nothing to undo.
                    response.source = *source;
                }
            }
        }
        return picked;
    }

    // The shell's own copy, handed in. The panel held a mirror of it for a while and could not
    // know when the shell cleared it -- which a successful save does.
    let name = paint.preset_name.to_owned();
    let controls = controls(brush, state.presets, state.preset_trouble, &name);
    for change in paint.show(ui, &controls) {
        if let Some(i) = source_pick(&change) {
            let id = FIRST_SOURCE + ControlId::try_from(i).unwrap_or(0);
            picked = super::open_pick(id, &sources(), paint);
            continue;
        }
        if let Some(answered) = answer(&change, brush, color_srgb, state.presets.len(), &name) {
            picked = Some(answered);
        }
    }

    // Drawn after the controls, into the rectangles the engine gave them.
    //
    // A press is read here rather than arriving as a `Change`, because a custom drawing does its own
    // hit-testing -- that is what makes it custom. What the engine offers is where the pointer is
    // and whether it is down; the gesture is assembled from that.
    //
    // **Which button is read from egui directly, and it is the one place this panel reaches past the
    // descriptor layer.** `PanelInput` says the pointer is down and not which button is holding it
    // down, and a curve needs two gestures -- one to place a point and one to take it away -- which
    // a pen and a mouse both have. Without the distinction a right-click would first add a point on
    // the way down and then remove it again, and a right-drag would move one. The alternative was a
    // field on `PanelInput`, which is another file. See the report.
    let (primary, remove) = ui.input(|i| (i.pointer.primary_down(), i.pointer.secondary_clicked()));
    let down = paint.input.pressed && primary;
    let fresh = down && !was_down(ui);
    set_was_down(ui, down);
    if !down {
        set_held(ui, None);
    }
    let pointer = paint.input.pointer;
    let squares: Vec<(ControlId, Rect)> = paint
        .input
        .custom
        .iter()
        .map(|(id, at)| (*id, curve_square(*at)))
        .collect();

    for (id, square) in squares {
        // The tip's edge profile is a curve on the tip; the six responses are curves on the brush.
        // Reached differently, edited identically -- so the borrow is settled here and everything
        // below works on one `&mut Curve`.
        let curve: Option<&mut Curve> = if id == EDGE_CURVE {
            brush.tip.falloff_mut()
        } else {
            let i = id.wrapping_sub(FIRST_CURVE) as usize;
            brush
                .responses_mut()
                .into_iter()
                .nth(i)
                .map(|(_, response)| &mut response.curve)
        };
        // A `Custom` this panel did not put in its own list, or an edge profile on a stamped tip
        // that has none: either way there is nothing to draw and nothing to press.
        let Some(curve) = curve else {
            continue;
        };

        if let Some((px, py)) = pointer {
            let at = to_curve(square, px, py);
            let inside = square.contains(px, py);
            // In curve space, because that is where the points are. The box is square, so one radius
            // does for both axes.
            let reach = REACH / square.w.max(f32::MIN_POSITIVE);
            if inside && remove {
                if let Some(next) = forgotten(curve.points(), at, reach) {
                    replace(curve, next);
                }
            } else if inside && fresh {
                if let Some((next, i)) = press(curve.points(), at, reach) {
                    replace(curve, next);
                    set_held(ui, Some((id, i)));
                }
            } else if down {
                // Only while this editor is the one holding the drag, and then wherever the pointer
                // has got to -- a hand dragging a point does not stay politely inside the box, and
                // `to_curve` clamps, so it pins at the edge rather than losing the point.
                if let Some((_, i)) = held(ui).filter(|(holder, _)| *holder == id) {
                    replace(curve, moved(curve.points(), i, at));
                }
            }
        }

        draw_curve(ui.painter(), paint.theme, square, curve);
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaint_core::TipRef;

    fn preset(name: &str) -> BrushPreset {
        BrushPreset::capture(name, &Brush::default(), TipRef::Round)
    }

    fn built(brush: &mut Brush, presets: &[BrushPreset], name: &str) -> Vec<Control> {
        controls(brush, presets, None, name)
    }

    fn ids(controls: &[Control]) -> Vec<ControlId> {
        controls.iter().filter_map(Control::id).collect()
    }

    fn labels(controls: &[Control]) -> Vec<String> {
        controls
            .iter()
            .filter_map(|c| match c {
                Control::Label { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn stamped(w: u32, h: u32) -> openpaint_core::dab::Tip {
        let coverage = vec![255_u8; (w * h) as usize];
        openpaint_core::dab::Tip::Stamp(std::sync::Arc::new(
            openpaint_core::Stamp::new(w, h, coverage).expect("a tiny stamp"),
        ))
    }

    /// **No two controls answer to the same id.**
    ///
    /// The preset rows are numbered by index and everything else is numbered from a base above them,
    /// so a library long enough to reach a command's id would make pressing a saved brush reset the
    /// brush instead -- and nothing about that symptom would point back at the arithmetic. This is
    /// the check that the bases really are above the rows.
    #[test]
    fn every_control_has_its_own_id() {
        let mut brush = Brush::default();
        let many: Vec<BrushPreset> = (0..64).map(|i| preset(&format!("Brush {i}"))).collect();
        for library in [&many[..], &many[..1], &[][..]] {
            let controls = built(&mut brush, library, "Ink");
            let mut seen = std::collections::BTreeSet::new();
            for id in ids(&controls) {
                assert!(seen.insert(id), "two controls answer to {id}");
            }
        }
        // And with a stamped tip, which swaps one control for two others.
        brush.tip = stamped(2, 2);
        let controls = built(&mut brush, &many, "Ink");
        let mut seen = std::collections::BTreeSet::new();
        for id in ids(&controls) {
            assert!(seen.insert(id), "two controls answer to {id}");
        }
    }

    /// And the id blocks do not run into each other, whatever the library holds.
    ///
    /// Written against the constants rather than against a built list, because the hazard is a block
    /// sized for six of something and later given seven.
    #[test]
    fn the_id_blocks_do_not_overlap() {
        // Far more brushes than anyone will save, and still nowhere near the first base.
        let biggest_row = ControlId::from(u16::MAX);
        assert!(
            biggest_row < FIRST_FORGET,
            "a library could reach a forget id"
        );
        assert!(
            FIRST_FORGET + biggest_row < FIRST_COMMAND,
            "a forget id could reach a command"
        );
        // These three are arithmetic on constants, so they are checked when the file is compiled
        // rather than when the test is run -- a block that had shrunk under its parameters would
        // then stop the build rather than waiting for someone to run the suite.
        const {
            assert!(
                FIRST_COMMAND + 0x20 < FIRST_SOURCE,
                "the commands could reach the source pickers"
            );
            assert!(
                FIRST_SOURCE + BLOCK <= FIRST_CURVE,
                "the source pickers could reach the curves"
            );
            assert!(
                (RESPONSES as ControlId) < BLOCK,
                "a block is too small for the parameters in it"
            );
        }
        let responses = ControlId::try_from(RESPONSES).expect("six");

        // And every id a press could actually produce is read as what it is.
        for i in 0..responses {
            assert_eq!(
                source_pick(&Change::Pressed(FIRST_SOURCE + i)),
                Some(i as usize)
            );
            assert_eq!(source_pick(&Change::Pressed(FIRST_CURVE + i)), None);
        }
        for id in [SIZE, RESET, SAVE_PRESET, EDGE_CURVE, FIRST_FORGET] {
            assert_eq!(source_pick(&Change::Pressed(id)), None);
        }
        // A seventh parameter does not exist yet, so its id must not be claimed by mistake.
        assert_eq!(
            source_pick(&Change::Pressed(FIRST_SOURCE + responses)),
            None
        );
    }

    /// One row per saved brush, in the order the library holds them, each with its own way out --
    /// **named after the brush it forgets**, because a press that deletes a file has to say which.
    #[test]
    fn a_row_per_saved_brush_with_its_own_way_out() {
        let mut brush = Brush::default();
        let library = [preset("Ink"), preset("Pencil")];
        let controls = built(&mut brush, &library, "");
        let rows: Vec<(ControlId, String)> = controls
            .iter()
            .filter_map(|c| match c {
                Control::Row { id, text, .. } => Some((*id, text.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows,
            vec![(0, "Ink".to_owned()), (1, "Pencil".to_owned())],
            "the rows are the library, in order"
        );
        for (i, name) in ["Ink", "Pencil"].iter().enumerate() {
            let i = ControlId::try_from(i).expect("two");
            let forget = controls.iter().find_map(|c| match c {
                Control::Button { id, text } if *id == FIRST_FORGET + i => Some(text.clone()),
                _ => None,
            });
            assert_eq!(forget, Some(format!("Forget {name}")));
        }
        // **No switch on the row.** Forgetting a brush deletes a file with no way back, and a delete
        // dressed as a toggle is a control that lies about what it does.
        assert!(
            !controls
                .iter()
                .any(|c| matches!(c, Control::Row { mark: Some(_), .. })),
            "a destructive answer must not be drawn as a switch"
        );
    }

    /// An empty library is still a panel: no rows, and everything else there so that a first brush
    /// can be dialled in and saved.
    #[test]
    fn an_empty_library_still_offers_the_whole_brush() {
        let mut brush = Brush::default();
        let controls = built(&mut brush, &[], "");
        assert!(!controls.iter().any(|c| matches!(c, Control::Row { .. })));
        for id in [
            SIZE,
            HARDNESS,
            SPACING,
            ROUNDNESS,
            ANGLE,
            STABILIZATION,
            FLOW,
            OPACITY,
            PRESET_NAME,
            SAVE_PRESET,
            LOAD_TIP,
            RESET,
        ] {
            assert!(ids(&controls).contains(&id), "control {id} went missing");
        }
    }

    /// **Trouble with the library is said in the panel**, because the artist is the one who has to
    /// act on it and a Save that silently does nothing is the failure §6b is about.
    #[test]
    fn a_broken_library_says_so_where_it_can_be_read() {
        let mut brush = Brush::default();
        let said = "The brush library could not be written.";
        let controls = controls(&mut brush, &[], Some(said), "");
        assert_eq!(labels(&controls).first().map(String::as_str), Some(said));
        // And with nothing wrong, nothing pretends there is.
        assert!(!labels(&built(&mut brush, &[], ""))
            .iter()
            .any(|l| l == said));
    }

    /// Every slider shows what the brush actually holds, over the range the engine accepts.
    #[test]
    fn the_sliders_show_the_brush_and_span_its_ranges() {
        let mut brush = Brush::default();
        brush.radius = 33.0;
        brush.roundness = 0.4;
        brush.angle = 0.25;
        brush.flow = 0.6;
        brush.stabilization_ms = 40.0;
        let controls = built(&mut brush, &[], "");
        let slider = |want: ControlId| {
            controls
                .iter()
                .find_map(|c| match c {
                    Control::Slider {
                        id,
                        value,
                        min,
                        max,
                        ..
                    } if *id == want => Some((*value, *min, *max)),
                    _ => None,
                })
                .expect("the slider should be there")
        };
        assert_eq!(slider(SIZE), (33.0, 0.5, 400.0));
        assert_eq!(slider(ROUNDNESS), (0.4, 0.02, 1.0));
        assert_eq!(slider(ANGLE), (0.25, 0.0, 1.0));
        assert_eq!(slider(FLOW), (0.6, 0.0, 1.0));
        // **The engine's own ceiling, not a number copied across.** A stabilizer slider that ran
        // past what the filter accepts would offer lag it cannot deliver.
        assert_eq!(
            slider(STABILIZATION),
            (40.0, 0.0, openpaint_core::stabilizer::MAX_LAG_MS)
        );
    }

    /// The defaults land where the sliders can show them, so "Reset to defaults" is visible rather
    /// than merely done.
    #[test]
    fn the_defaults_sit_inside_every_range() {
        let mut brush = Brush::default();
        let controls = built(&mut brush, &[], "");
        for control in &controls {
            if let Control::Slider {
                text,
                value,
                min,
                max,
                ..
            } = control
            {
                assert!(
                    (*min..=*max).contains(value),
                    "{text} defaults to {value}, outside {min}..={max}"
                );
            }
        }
        let of = |want| {
            controls.iter().find_map(|c| match c {
                Control::Slider { id, value, .. } if *id == want => Some(*value),
                _ => None,
            })
        };
        assert_eq!(of(ROUNDNESS), Some(1.0), "a circle");
        assert_eq!(of(ANGLE), Some(0.0), "unrotated");
        assert_eq!(of(FLOW), Some(1.0));
        assert_eq!(of(STABILIZATION), Some(0.0), "off, and deliberately so");
    }

    /// **One source picker and one curve editor per modulated parameter, named after it.**
    ///
    /// Built from `Brush::responses_mut` rather than written out, so adding a parameter to the
    /// engine makes an editor appear here rather than needing a second edit somebody forgets. This
    /// is the test that says so.
    #[test]
    fn every_modulated_parameter_gets_a_picker_and_a_curve() {
        let mut brush = Brush::default();
        let names: Vec<&'static str> = brush.responses_mut().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names.len(), RESPONSES, "the id blocks are sized for six");
        let controls = built(&mut brush, &[], "");
        for (i, name) in names.iter().enumerate() {
            let i = ControlId::try_from(i).expect("six");
            let picker = controls.iter().find_map(|c| match c {
                Control::Pick { id, text, value } if *id == FIRST_SOURCE + i => {
                    Some((text.clone(), value.clone()))
                }
                _ => None,
            });
            assert_eq!(
                picker,
                Some(((*name).to_owned(), "Pressure".to_owned())),
                "no source picker for {name}"
            );
            assert!(
                controls
                    .iter()
                    .any(|c| matches!(c, Control::Custom { id, .. } if *id == FIRST_CURVE + i)),
                "no curve editor for {name}"
            );
        }
    }

    /// A picker shows the source that is actually set, not the one it was written with.
    #[test]
    fn a_picker_shows_the_source_in_force() {
        let mut brush = Brush::default();
        brush.angle_response.source = Source::Direction;
        let controls = built(&mut brush, &[], "");
        // Angle is the last of the six, per `Brush::responses_mut`.
        let last = FIRST_SOURCE + ControlId::try_from(RESPONSES - 1).expect("six");
        let shown = controls.iter().find_map(|c| match c {
            Control::Pick { id, value, .. } if *id == last => Some(value.clone()),
            _ => None,
        });
        assert_eq!(shown, Some("Direction".to_owned()));
    }

    /// **A bitmap tip has no edge profile to edit, so the panel does not offer one.**
    ///
    /// The stamp carries its own edge; a curve drawn beside it would be a control that changes
    /// nothing, which is worse than no control at all.
    #[test]
    fn a_bitmap_tip_swaps_the_edge_curve_for_a_way_back() {
        let mut brush = Brush::default();
        let round = built(&mut brush, &[], "");
        assert!(ids(&round).contains(&EDGE_CURVE));
        assert!(!ids(&round).contains(&ROUND_TIP));
        assert!(ids(&round).contains(&LOAD_TIP), "always a way to load one");

        brush.tip = stamped(4, 3);
        let with_stamp = built(&mut brush, &[], "");
        assert!(!ids(&with_stamp).contains(&EDGE_CURVE));
        assert!(ids(&with_stamp).contains(&ROUND_TIP));
        assert!(ids(&with_stamp).contains(&LOAD_TIP));
        assert!(
            labels(&with_stamp).iter().any(|l| l == "Bitmap, 4x3"),
            "the panel should say what was loaded: {:?}",
            labels(&with_stamp)
        );
    }

    /// The explanations survived the port. They are the only place several of these controls say
    /// what they mean, and a panel of unlabelled sliders is a panel nobody can use.
    #[test]
    fn the_explanations_came_across() {
        let mut brush = Brush::default();
        let said = labels(&built(&mut brush, &[], "")).join(" ");
        for fragment in [
            "never your colour",
            "chisel nib",
            "0 hardness is soft",
            "Alt+click",
            "Smooths pen shake",
            "flat curve means the input is ignored",
            "edge profile",
            "ceiling for the whole stroke",
            "what looks like ink is ink",
            "fraction of diameter",
        ] {
            assert!(
                said.contains(fragment),
                "the panel no longer says {fragment:?}"
            );
        }
        // **And the sentence about the pair sits with the pair.** It was four screens from Opacity
        // while Opacity was up with Size, which is a label explaining sliders nobody can see.
        let controls = built(&mut brush, &[], "");
        let at = |id| {
            controls
                .iter()
                .position(|c| c.id() == Some(id))
                .expect("the control should be there")
        };
        assert_eq!(at(FLOW) + 1, at(OPACITY), "flow and opacity are a pair");
    }

    /// The stabilization note changes with the setting, because "off" and "40 ms of lag" are two
    /// different things to know.
    #[test]
    fn the_stabilization_note_says_whether_it_is_on() {
        let mut brush = Brush::default();
        let off = |b: &mut Brush| {
            labels(&built(b, &[], ""))
                .iter()
                .any(|l| l.starts_with("Off."))
        };
        assert!(off(&mut brush), "it is off by default and should say so");
        brush.stabilization_ms = 40.0;
        assert!(!off(&mut brush));
    }

    /// The readout under Reset is arithmetic on the two sliders above it, so it has to follow them.
    #[test]
    fn the_spacing_readout_follows_the_sliders() {
        let mut brush = Brush::default();
        brush.radius = 10.0;
        brush.spacing = 0.25;
        assert!(
            labels(&built(&mut brush, &[], ""))
                .iter()
                .any(|l| l.contains("every 5.00 px")),
            "10 px radius at 0.25 spacing is a dab every 5 px"
        );
    }

    // -- What a change does ------------------------------------------------------------------------

    fn applied(change: &Change, brush: &mut Brush, presets: usize, name: &str) -> Option<Picked> {
        let mut colour = [0, 0, 0];
        answer(change, brush, &mut colour, presets, name)
    }

    /// Every slider sets the field it is named after, and nothing else.
    #[test]
    fn a_slider_sets_its_own_field() {
        let mut brush = Brush::default();
        for (id, value) in [
            (SIZE, 20.0),
            (HARDNESS, 0.25),
            (SPACING, 0.5),
            (ROUNDNESS, 0.3),
            (ANGLE, 0.75),
            (STABILIZATION, 30.0),
            (FLOW, 0.4),
            (OPACITY, 0.8),
        ] {
            assert_eq!(
                applied(&Change::Set(id, value), &mut brush, 0, ""),
                None,
                "a slider is applied here, not asked for"
            );
        }
        assert_eq!(brush.radius, 20.0);
        assert_eq!(brush.hardness, 0.25);
        assert_eq!(brush.spacing, 0.5);
        assert_eq!(brush.roundness, 0.3);
        assert_eq!(brush.angle, 0.75);
        assert_eq!(brush.stabilization_ms, 30.0);
        assert_eq!(brush.flow, 0.4);
        assert_eq!(brush.opacity, 0.8);
    }

    /// A row applies that brush; the button under it forgets it. **The two must not be confused** --
    /// one is undone by picking another brush, the other deletes a file.
    #[test]
    fn a_row_applies_and_the_button_under_it_forgets() {
        let mut brush = Brush::default();
        assert_eq!(
            applied(&Change::Chose(1), &mut brush, 3, ""),
            Some(Picked::Brush(BrushAction::ApplyPreset(1)))
        );
        assert_eq!(
            applied(&Change::Pressed(FIRST_FORGET + 2), &mut brush, 3, ""),
            Some(Picked::Brush(BrushAction::DeletePreset(2)))
        );
        // Neither touches the brush: applying one is the shell's, because it also restores the tip.
        assert_eq!(brush, Brush::default());
    }

    /// **A press against a library that has since shrunk asks for nothing.**
    ///
    /// The list is drawn on one frame and the press read on another, and a brush saved or forgotten
    /// in between moves every row after it. Indexing off the end would ask the shell to apply a
    /// brush that is not there.
    #[test]
    fn a_press_past_the_end_of_the_library_is_refused() {
        let mut brush = Brush::default();
        assert_eq!(applied(&Change::Chose(3), &mut brush, 3, ""), None);
        assert_eq!(
            applied(&Change::Pressed(FIRST_FORGET + 9), &mut brush, 3, ""),
            None
        );
        // And against no library at all, which is what a first run has.
        assert_eq!(applied(&Change::Chose(0), &mut brush, 0, ""), None);
        assert_eq!(
            applied(&Change::Pressed(FIRST_FORGET), &mut brush, 0, ""),
            None
        );
    }

    /// A name is typed, finished with, and handed on. **Not per keystroke**: a name applied letter by
    /// letter would save eight presets on the way to spelling one.
    #[test]
    fn the_name_is_handed_on_when_the_field_is_finished_with() {
        let mut brush = Brush::default();
        assert_eq!(
            applied(&Change::Typing(PRESET_NAME), &mut brush, 0, ""),
            None,
            "taking the caret changes nothing"
        );
        assert_eq!(
            applied(
                &Change::Typed(PRESET_NAME, "Ink".to_owned()),
                &mut brush,
                0,
                ""
            ),
            Some(Picked::PresetName("Ink".to_owned()))
        );
    }

    /// **A nameless brush is refused rather than saved**: a row with no label is one nobody can pick
    /// on purpose. Whitespace is not a name either.
    #[test]
    fn saving_needs_a_name() {
        let mut brush = Brush::default();
        assert_eq!(
            applied(&Change::Pressed(SAVE_PRESET), &mut brush, 0, "Ink"),
            Some(Picked::Brush(BrushAction::SavePreset))
        );
        for blank in ["", "   ", "\t\n "] {
            assert_eq!(
                applied(&Change::Pressed(SAVE_PRESET), &mut brush, 0, blank),
                None,
                "{blank:?} is not a name"
            );
        }
    }

    /// Loading a tip is the shell's to do -- it opens a file dialog. Taking one off is not, so it
    /// happens here and asks for nothing.
    #[test]
    fn the_tip_buttons_ask_for_what_only_the_shell_can_do() {
        let mut brush = Brush::default();
        assert_eq!(
            applied(&Change::Pressed(LOAD_TIP), &mut brush, 0, ""),
            Some(Picked::Brush(BrushAction::LoadTip))
        );
        brush.tip = stamped(2, 2);
        assert_eq!(
            applied(&Change::Pressed(ROUND_TIP), &mut brush, 0, ""),
            None
        );
        assert!(brush.tip.stamp().is_none(), "the stamp should be off");
        assert!(
            brush.tip.falloff_mut().is_some(),
            "and an edge profile back to edit"
        );
    }

    /// **Reset puts the colour back with the brush.** The default brush carries one, and a swatch
    /// still showing the old colour would be the panel disagreeing with what would be painted.
    #[test]
    fn reset_returns_the_brush_and_its_colour() {
        let mut brush = Brush::default();
        brush.radius = 200.0;
        brush.roundness = 0.1;
        brush.set_color_srgb8([255, 0, 0]);
        let mut colour = [255, 0, 0];
        assert_eq!(
            answer(&Change::Pressed(RESET), &mut brush, &mut colour, 0, ""),
            None
        );
        assert_eq!(brush, Brush::default());
        assert_eq!(colour, Brush::default().color_srgb8());
    }

    // -- The curve editors -------------------------------------------------------------------------

    fn square() -> Rect {
        Rect::new(10.0, 20.0, 140.0, 140.0)
    }

    /// **A point is grabbed where it is drawn.** The two conversions are each other's inverse, and a
    /// point drawn at one place while a press there takes hold of nothing reads as the editor being
    /// broken -- the same failure the slider's two halves are written together to avoid.
    #[test]
    fn where_a_point_is_drawn_is_where_pressing_grabs_it() {
        let s = square();
        for p in [(0.0, 0.0), (0.5, 0.25), (1.0, 1.0), (0.2, 0.9)] {
            let (x, y) = to_screen(s, p);
            let back = to_curve(s, x, y);
            assert!(
                (back.0 - p.0).abs() < 0.001 && (back.1 - p.1).abs() < 0.001,
                "{p:?} drew at ({x}, {y}) and read back as {back:?}"
            );
        }
        // Curve y runs *up* the screen: (0,0) is the bottom-left corner, not the top-left one.
        assert!(to_screen(s, (0.0, 0.0)).1 > to_screen(s, (0.0, 1.0)).1);
        // And a press outside is pinned to the edge rather than escaping the range.
        assert_eq!(to_curve(s, s.x - 500.0, s.y - 500.0), (0.0, 1.0));
        assert_eq!(to_curve(s, s.x + 500.0, s.y + 500.0), (1.0, 0.0));
    }

    /// The editor is square and sits at the left margin, whatever shape the row it was given is.
    #[test]
    fn the_editor_is_square_inside_the_row_it_was_given() {
        for row in [
            Rect::new(4.0, 8.0, 300.0, CURVE_SIDE),
            Rect::new(4.0, 8.0, 60.0, CURVE_SIDE),
        ] {
            let s = curve_square(row);
            assert!((s.w - s.h).abs() < 0.001, "{s:?} is not square");
            assert!(
                s.w <= row.w + 0.001 && s.h <= row.h + 0.001,
                "{s:?} escaped its row"
            );
            assert!((s.x - row.x).abs() < 0.001, "not at the left margin");
        }
        // A panel dragged to nothing still gives a square something can divide by.
        assert!(curve_square(Rect::new(0.0, 0.0, 0.0, 0.0)).w > 0.0);
    }

    /// A press near a point takes hold of that point and moves nothing.
    #[test]
    fn a_press_on_a_point_takes_hold_of_it() {
        let points = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        let (next, i) = press(&points, (0.52, 0.48), 0.1).expect("a grab");
        assert_eq!(i, 1);
        assert_eq!(next, points, "grabbing a point must not move it");
    }

    /// **A press on empty curve adds a point and grabs it in the same gesture.** Adding on a click
    /// and placing on a later drag is two gestures for one act, which is why this differs from the
    /// editor it was ported from.
    #[test]
    fn a_press_on_empty_space_adds_a_point_and_holds_it() {
        let points = vec![(0.0, 0.0), (1.0, 1.0)];
        let (next, i) = press(&points, (0.3, 0.8), 0.05).expect("an insert");
        assert_eq!(i, 1);
        assert_eq!(next, vec![(0.0, 0.0), (0.3, 0.8), (1.0, 1.0)]);
        // Still sorted in x, because a curve with two answers at one input is not a curve.
        assert!(next.windows(2).all(|w| w[0].0 < w[1].0));
    }

    /// **Past either end there is nothing to grab and nothing may be added.** The ends define the
    /// input range, so a curve may not grow one -- "what happens at full pressure" is not a question
    /// a brush may decline.
    #[test]
    fn a_press_past_the_ends_adds_nothing() {
        let points = vec![(0.2, 0.0), (0.8, 1.0)];
        assert!(
            press(&points, (0.05, 0.5), 0.01).is_none(),
            "before the first"
        );
        assert!(
            press(&points, (0.95, 0.5), 0.01).is_none(),
            "after the last"
        );
        assert_eq!(press(&points, (0.5, 0.5), 0.01).map(|(_, i)| i), Some(1));
    }

    /// **The ends move in y and never in x**, and an interior point stays between its neighbours.
    #[test]
    fn a_drag_cannot_reorder_the_curve() {
        let points = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        assert_eq!(
            moved(&points, 0, (0.9, 0.7))[0],
            (0.0, 0.7),
            "the first end"
        );
        assert_eq!(moved(&points, 2, (0.1, 0.2))[2], (1.0, 0.2), "the last end");
        // The middle one, dragged past each neighbour in turn.
        for (to, want) in [((-5.0_f32, 0.3_f32), 0.02_f32), ((5.0, 0.3), 0.98)] {
            let next = moved(&points, 1, to);
            assert!(
                (next[1].0 - want).abs() < 0.001,
                "{to:?} landed at {next:?}"
            );
            assert!(
                next.windows(2).all(|w| w[0].0 < w[1].0),
                "{next:?} crossed over"
            );
        }
        // An index that is no longer there leaves the list alone rather than panicking: the list is
        // rebuilt each frame, and a drag latched on one frame is applied on the next.
        assert_eq!(moved(&points, 9, (0.5, 0.5)), points);
    }

    /// **A curve keeps its ends and never drops below two points.** An end is what defines the
    /// range, and a curve of one point has no shape to read.
    #[test]
    fn removing_a_point_refuses_the_ends_and_the_last_pair() {
        let three = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        assert_eq!(
            forgotten(&three, (0.5, 0.5), 0.1),
            Some(vec![(0.0, 0.0), (1.0, 1.0)])
        );
        assert!(
            forgotten(&three, (0.0, 0.0), 0.1).is_none(),
            "the first end"
        );
        assert!(forgotten(&three, (1.0, 1.0), 0.1).is_none(), "the last end");
        let two = vec![(0.0, 0.0), (1.0, 1.0)];
        assert!(forgotten(&two, (0.5, 0.5), 1.0).is_none(), "the last pair");
        assert!(
            forgotten(&three, (0.5, 0.0), 0.01).is_none(),
            "nothing near"
        );
    }

    /// **The nearest point wins, and only one within reach.** Two points close together with the
    /// wrong one grabbed is how a drag drops a point somewhere nobody aimed for.
    #[test]
    fn only_a_point_within_reach_is_grabbed_and_the_nearest_of_them() {
        let points = vec![(0.0, 0.0), (0.40, 0.5), (0.46, 0.5), (1.0, 1.0)];
        assert_eq!(nearest(&points, (0.45, 0.5), 0.1), Some(2));
        assert_eq!(nearest(&points, (0.41, 0.5), 0.1), Some(1));
        assert_eq!(nearest(&points, (0.7, 0.1), 0.05), None, "nothing near");
    }

    /// **Everything the editors produce is a curve the engine will take.**
    ///
    /// This is what stands behind `replace` refusing rather than repairing: if that refusal ever
    /// fires, something above it is wrong, and this is the check that says it should not.
    #[test]
    fn every_edit_produces_a_curve_the_engine_accepts() {
        let mut points = Curve::linear().points().to_vec();
        for at in [(0.25, 0.9), (0.75, 0.1), (0.5, 0.5), (0.05, 0.99)] {
            if let Some((next, i)) = press(&points, at, 0.05) {
                points = moved(&next, i, at);
            }
            assert!(
                Curve::from_points(points.clone()).is_some(),
                "{points:?} is not a curve"
            );
        }
        while let Some(next) = forgotten(&points, (0.5, 0.5), 1.0) {
            points = next;
            assert!(Curve::from_points(points.clone()).is_some());
        }
        assert_eq!(points.len(), 2, "removing should stop at the two ends");
    }
}
