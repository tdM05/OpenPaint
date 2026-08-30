//! Icon sets, as data.
//!
//! The same move `theme` makes for colour, made for the glyphs: a set of icons is a table of
//! shapes with a name, the UI asks it for a *role*, and swapping sets is choosing a different row
//! rather than editing a widget. Nothing here draws anything and nothing here knows about egui —
//! the drawing lives with whatever paints the panel, so a second renderer inherits the icons for
//! free.
//!
//! # Words is a set
//!
//! The tool rail says "Brush", "Eraser", "Lasso" today, and that was a decision rather than a
//! placeholder: glyphs needed a tooltip to be readable, and a tooltip is something only a hovering
//! pointer can reach, which this UI is explicitly not built around (§1b). So words stay on offer
//! as one of the sets — [`SETS`] lists it beside the drawn ones, its lookup returns `None` for
//! every symbol, and the caller falls back to the label it already has. **That is the whole
//! mechanism.** No `if set == Words` anywhere, for the same reason the layout tree never asks
//! which panel it is holding (§1c).
//!
//! # Roles, not pictures
//!
//! [`Symbol`] names what a button *does*, never what it looks like: `DeleteLayer`, not `Bin`. A
//! set that would rather draw deletion as a cross than as a bin is then a different table and not
//! a different vocabulary — the same reasoning that makes `theme`'s tokens `state` and `dim`
//! instead of `blue` and `grey`.
//!
//! # A unit square, so size and colour are the caller's business
//!
//! Every coordinate is in `0.0..=1.0`, x right and y **down**, matching [`crate::layout`]'s screen
//! coordinates so the drawer never has to flip anything. Multiply by the box and add its corner;
//! that is the entire mapping. Colour is not in here at all: an icon is a shape, the theme says
//! what colour it is, and an icon that carried its own colour would break in `Theme::paper` the
//! day it was drawn.
//!
//! # Designed for 6 mm
//!
//! These render at about `Metrics::row` — 22 logical units, a shade under 6 mm on real glass. That
//! is the constraint every shape below was drawn against, and it rules out more than it sounds
//! like: at 22 units a detail 0.1 wide is two units, so *three* such details in a row are a
//! smudge. Hence the marks-per-icon budget the tests enforce, the generous margins, and the habit
//! of saying a thing once — one badge, not a badge and a shadow.
//!
//! # Why there is no `Arc`
//!
//! Curves are polylines. The largest curve here is roughly a 0.3-radius arc, 6.6 units on screen;
//! stepped every 36° its chord sags 0.32 units from the true arc — under the one-unit hairline the
//! rest of the UI is drawn with, so a real arc would put ink in the same pixels. An `Arc`
//! primitive would therefore buy nothing but a second code path in every renderer that ever draws
//! these.

// Nothing calls this yet: the rail still draws words, and the wiring lands in `ui.rs` separately.
// Delete this the moment a panel asks for a glyph.
#![allow(dead_code)]

/// One shape in an icon.
///
/// Deliberately three variants and no more. Every extra primitive is a branch in every renderer
/// that ever draws an icon, and the whole point of a set being *data* is that adding a set costs
/// nothing but coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mark {
    /// An open polyline, stroked at the caller's line width.
    ///
    /// A closed outline is written as a path whose last point repeats its first. That needs no
    /// separate variant and no flag: a renderer that strokes the segments in order draws the
    /// closed shape correctly without knowing it was closed.
    Path(&'static [[f32; 2]]),
    /// A closed polygon, filled.
    ///
    /// Used for arrowheads and solid masses. Arrowheads are filled even inside the outline set on
    /// purpose: at 22 units a chevron made of two hairlines does not read as a direction, and
    /// direction is the entire content of an undo arrow.
    Poly(&'static [[f32; 2]]),
    /// A circle, filled or stroked.
    ///
    /// Earns its place where a polygon would not: an eye's pupil and a slider's knob are round at
    /// 3 units across, and a polygon approximating them at that size is visibly a polygon.
    Circle {
        /// Centre, in the unit square.
        at: [f32; 2],
        /// Radius, in the same units.
        r: f32,
        /// Filled, or stroked as a ring.
        filled: bool,
    },
}

/// Declares [`Symbol`] and the list of every symbol from one source.
///
/// **So the two cannot come apart** (recurring hazard §11a.8). A hand-written `ALL` beside a
/// hand-written enum goes stale the first time someone adds a role in a hurry, and no test can
/// notice — a test can only check the list it is given. Generating both from one line makes the
/// staleness unrepresentable rather than merely discouraged.
macro_rules! symbols {
    ($( $(#[$meta:meta])* $name:ident ),+ $(,)?) => {
        /// What a button *does*, which is what an icon set is a table of.
        ///
        /// Named for the role and never for the drawing, so a set is free to say "delete" with a
        /// bin, a cross or an empty frame without the rest of the UI learning a new word.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Symbol {
            $( $(#[$meta])* $name, )+
        }

        impl Symbol {
            /// Every symbol, in a fixed order.
            ///
            /// Generated alongside the enum, so it cannot omit one.
            pub const ALL: &[Symbol] = &[ $( Symbol::$name ),+ ];
        }
    };
}

symbols! {
    /// The painting tool.
    Brush,
    /// The erasing tool.
    Eraser,
    /// Free-hand selection.
    Lasso,
    /// Rectangular selection.
    RectSelect,
    /// Select by colour — the wand, and the bucket it becomes when set to fill on click.
    Wand,
    /// Drag the selected pixels.
    MoveSelection,
    /// Add a layer above the active one.
    AddLayer,
    /// Copy the active layer.
    DuplicateLayer,
    /// Merge the active layer into the one below it.
    MergeDown,
    /// Remove the active layer.
    DeleteLayer,
    /// The layer is shown — the eye on a layer row, in its on state.
    Visible,
    /// The layer is hidden — the same eye, off.
    Hidden,
    /// The layer paints only where it already has ink.
    LockAlpha,
    /// Step back through history.
    Undo,
    /// Step forward again.
    Redo,
    /// Which panels are on screen.
    Panels,
    /// A panel's own options.
    Settings,
    /// Scale the page to fill the view.
    ZoomFit,
    /// One image pixel to one screen pixel.
    ZoomActual,
    /// Start a new document.
    NewDocument,
    /// Open an existing one.
    OpenDocument,
    /// Write the document back where it came from.
    Save,
    /// Write it somewhere else, under a new name.
    SaveAs,
    /// Write a flattened copy out as an image.
    ExportImage,
    /// Select the whole page.
    SelectAll,
    /// Drop the selection.
    Deselect,
    /// Swap what is selected for what is not.
    InvertSelection,
    /// Erase what is inside the selection.
    ClearSelection,
    /// Flood the selection with the brush colour.
    FillSelection,
}

/// One complete look for the icons, offered the way `workspace::DIRECTIONS` offers directions.
///
/// The `id` is what a settings popup round-trips, so it is stable and belongs to the set rather
/// than to its position — reordering [`SETS`] must not silently change what somebody chose.
#[derive(Clone, Copy, Debug)]
pub struct IconSet {
    /// Stable identity, for the control that offers this set.
    pub id: u32,
    /// What the artist sees in the list.
    pub name: &'static str,
    /// The table itself.
    ///
    /// A function rather than a map, so the compiler checks it: every lookup below is an
    /// exhaustive `match`, and adding a [`Symbol`] therefore **stops the build** until every drawn
    /// set has something to say about it. A `HashMap` would have compiled and drawn nothing.
    lookup: fn(Symbol) -> Option<&'static [Mark]>,
}

impl IconSet {
    /// The shapes for a symbol, or `None` when this set has no picture for it.
    ///
    /// `None` is not an error and not a gap: it is the words set answering, and the caller draws
    /// the label it was going to draw anyway.
    #[must_use]
    pub fn glyph(&self, symbol: Symbol) -> Option<&'static [Mark]> {
        (self.lookup)(symbol)
    }
}

/// Cut a filled outline into triangles.
///
/// **Because most of these outlines are concave.** A floppy disk with its shutter notched out, an
/// arrow, a trash can that narrows towards the base: none of them are convex, and the usual
/// fan-from-the-first-vertex fill turns every one of them into a mess of overlapping wedges. That
/// is precisely what the Solid set looked like on its first render -- the data was right and the
/// drawing was not.
///
/// Ear clipping, which is the simple algorithm for simple polygons and is more than enough here:
/// an icon is a dozen points, cut once per draw, at a size the eye is about to stop caring about.
///
/// Returns indices into `points`. An outline with fewer than three points, or one it cannot make
/// progress on, yields nothing rather than something wrong: a missing icon is a puzzle, an icon
/// drawn inside out is a bug report.
#[must_use]
pub fn triangles(points: &[[f32; 2]]) -> Vec<[usize; 3]> {
    if points.len() < 3 {
        return Vec::new();
    }
    let mut remaining: Vec<usize> = (0..points.len()).collect();
    // Ear clipping has to know which way the outline turns before it can tell a convex corner from
    // a reflex one, and icon data is written in whichever direction read best.
    let winding = signed_area(points, &remaining).signum();
    let mut out = Vec::with_capacity(points.len() - 2);
    // A simple polygon always yields an ear, so failing to find one means the outline is not
    // simple. Bounded rather than trusted: a self-crossing outline must not hang the renderer.
    let mut patience = remaining.len() * remaining.len() + 4;

    while remaining.len() > 3 {
        patience -= 1;
        if patience == 0 {
            return Vec::new();
        }
        let n = remaining.len();
        let found = (0..n).find(|&i| {
            let (a, b, c) = (
                remaining[(i + n - 1) % n],
                remaining[i],
                remaining[(i + 1) % n],
            );
            if cross(points[a], points[b], points[c]) * winding <= 0.0 {
                return false;
            }
            // No other corner may lie inside the ear, or clipping it would cut across the shape.
            !remaining
                .iter()
                .filter(|&&q| q != a && q != b && q != c)
                .any(|&q| inside(points[a], points[b], points[c], points[q]))
        });
        let Some(ear) = found else {
            return Vec::new();
        };
        out.push([
            remaining[(ear + n - 1) % n],
            remaining[ear],
            remaining[(ear + 1) % n],
        ]);
        remaining.remove(ear);
    }
    out.push([remaining[0], remaining[1], remaining[2]]);
    out
}

/// Twice the signed area of the corner `a b c`: positive one way round, negative the other.
fn cross(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]).mul_add(c[1] - a[1], -((b[1] - a[1]) * (c[0] - a[0])))
}

fn signed_area(points: &[[f32; 2]], order: &[usize]) -> f32 {
    let n = order.len();
    (0..n)
        .map(|i| {
            let (p, q) = (points[order[i]], points[order[(i + 1) % n]]);
            p[0].mul_add(q[1], -(q[0] * p[1]))
        })
        .sum::<f32>()
        / 2.0
}

fn inside(a: [f32; 2], b: [f32; 2], c: [f32; 2], p: [f32; 2]) -> bool {
    let (d1, d2, d3) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

/// The icon sets on offer.
///
/// One table, so what is offered and what gets drawn cannot come apart — the same reasoning as
/// `workspace::DIRECTIONS`, and a set added here appears in the chooser without anyone remembering
/// to list it twice.
pub const SETS: &[IconSet] = &[
    IconSet {
        id: 0,
        name: "Words",
        lookup: no_glyph,
    },
    IconSet {
        id: 1,
        name: "Line",
        lookup: line_glyph,
    },
    IconSet {
        id: 2,
        name: "Solid",
        lookup: solid_glyph,
    },
];

/// The words set: no pictures, ever.
///
/// Not a stub. This is the set an artist picks when they want to read the rail rather than decode
/// it, and returning `None` for everything is exactly how it says so.
fn no_glyph(_symbol: Symbol) -> Option<&'static [Mark]> {
    None
}

// ----------------------------------------------------------------------------------------------
// Line — outlines at the weight of the theme's own hairlines.
//
// The quiet set, and the one that belongs beside a divider: everything is a stroked contour, so an
// icon carries about as much ink as the rules and edges around it and the rail does not become a
// row of blobs. What it costs is contrast — on a dim panel at 22 units these are legible but not
// loud, which is the trade the Solid set exists to offer the other side of.
// ----------------------------------------------------------------------------------------------

/// The Line set's table.
fn line_glyph(symbol: Symbol) -> Option<&'static [Mark]> {
    Some(match symbol {
        Symbol::Brush => LINE_BRUSH,
        Symbol::Eraser => LINE_ERASER,
        Symbol::Lasso => LINE_LASSO,
        Symbol::RectSelect => LINE_RECT_SELECT,
        Symbol::Wand => LINE_WAND,
        Symbol::MoveSelection => LINE_MOVE_SELECTION,
        Symbol::AddLayer => LINE_ADD_LAYER,
        Symbol::DuplicateLayer => LINE_DUPLICATE_LAYER,
        Symbol::MergeDown => LINE_MERGE_DOWN,
        Symbol::DeleteLayer => LINE_DELETE_LAYER,
        Symbol::Visible => LINE_VISIBLE,
        Symbol::Hidden => LINE_HIDDEN,
        Symbol::LockAlpha => LINE_LOCK_ALPHA,
        Symbol::Undo => LINE_UNDO,
        Symbol::Redo => LINE_REDO,
        Symbol::Panels => LINE_PANELS,
        Symbol::Settings => LINE_SETTINGS,
        Symbol::ZoomFit => LINE_ZOOM_FIT,
        Symbol::ZoomActual => LINE_ZOOM_ACTUAL,
        Symbol::NewDocument => LINE_NEW_DOCUMENT,
        Symbol::OpenDocument => LINE_OPEN_DOCUMENT,
        Symbol::Save => LINE_SAVE,
        Symbol::SaveAs => LINE_SAVE_AS,
        Symbol::ExportImage => LINE_EXPORT_IMAGE,
        Symbol::SelectAll => LINE_SELECT_ALL,
        Symbol::Deselect => LINE_DESELECT,
        Symbol::InvertSelection => LINE_INVERT_SELECTION,
        Symbol::ClearSelection => LINE_CLEAR_SELECTION,
        Symbol::FillSelection => LINE_FILL_SELECTION,
    })
}

/// A held brush: a long handle at 45° and a chisel tip split off by a ferrule gap.
///
/// The gap is the whole icon. Without it this is a pencil, and a pencil is what every application
/// draws for a *hard* tool; the break between shaft and tip is the one cue that survives to 22
/// units.
const LINE_BRUSH: &[Mark] = &[
    Mark::Path(&[
        [0.70, 0.08],
        [0.92, 0.30],
        [0.50, 0.66],
        [0.28, 0.44],
        [0.70, 0.08],
    ]),
    Mark::Path(&[[0.22, 0.49], [0.44, 0.71], [0.08, 0.86], [0.22, 0.49]]),
];

/// A block on a surface, tilted, with the rubbing face banded off.
///
/// Tilted the *opposite* way from the brush on purpose: at this size the eye sorts the rail by
/// slope long before it reads the shapes, so two diagonal tools leaning the same way read as one
/// tool drawn twice.
const LINE_ERASER: &[Mark] = &[
    Mark::Path(&[
        [0.14, 0.50],
        [0.48, 0.12],
        [0.84, 0.40],
        [0.50, 0.78],
        [0.14, 0.50],
    ]),
    Mark::Path(&[[0.32, 0.64], [0.66, 0.26]]),
    Mark::Path(&[[0.08, 0.90], [0.92, 0.90]]),
];

/// A thrown loop with the rope still hanging from it.
///
/// One unbroken polyline, and deliberately lumpy rather than a clean ellipse: a clean ellipse at
/// 22 units is a circle, and a circle already means something else in this set.
const LINE_LASSO: &[Mark] = &[Mark::Path(&[
    [0.42, 0.94],
    [0.48, 0.74],
    [0.30, 0.70],
    [0.14, 0.52],
    [0.16, 0.30],
    [0.34, 0.14],
    [0.60, 0.12],
    [0.80, 0.24],
    [0.86, 0.46],
    [0.76, 0.66],
    [0.56, 0.74],
])];

/// Four corner brackets — a marquee with its sides left out.
///
/// Corners rather than a dashed rectangle because dashes at 22 units are two units long and grey
/// out into a solid line anyway; the corners keep the "this is a marquee, not a box" reading at
/// any size.
const LINE_RECT_SELECT: &[Mark] = &[
    Mark::Path(&[[0.12, 0.36], [0.12, 0.12], [0.36, 0.12]]),
    Mark::Path(&[[0.64, 0.12], [0.88, 0.12], [0.88, 0.36]]),
    Mark::Path(&[[0.88, 0.64], [0.88, 0.88], [0.64, 0.88]]),
    Mark::Path(&[[0.36, 0.88], [0.12, 0.88], [0.12, 0.64]]),
];

/// A stick with a sparkle at its end.
///
/// The sparkle is a plain cross rather than a four-point star: a star's concave waist is under a
/// unit wide at this size and fills in, leaving a blob where the cue was.
const LINE_WAND: &[Mark] = &[
    Mark::Path(&[[0.14, 0.90], [0.58, 0.46]]),
    Mark::Path(&[[0.74, 0.12], [0.74, 0.46]]),
    Mark::Path(&[[0.57, 0.29], [0.91, 0.29]]),
];

/// The four-way arrow, which means "drag this" everywhere.
///
/// Heads are filled. Two hairlines meeting at a point do not read as an arrowhead at 22 units —
/// they read as a kink in the line, and then all four arms look like the axis they sit on.
const LINE_MOVE_SELECTION: &[Mark] = &[
    Mark::Path(&[[0.50, 0.16], [0.50, 0.84]]),
    Mark::Path(&[[0.16, 0.50], [0.84, 0.50]]),
    Mark::Poly(&[[0.50, 0.04], [0.38, 0.22], [0.62, 0.22]]),
    Mark::Poly(&[[0.50, 0.96], [0.38, 0.78], [0.62, 0.78]]),
    Mark::Poly(&[[0.04, 0.50], [0.22, 0.38], [0.22, 0.62]]),
    Mark::Poly(&[[0.96, 0.50], [0.78, 0.38], [0.78, 0.62]]),
];

/// A sheet with a plus beside it.
///
/// The badge sits *outside* the sheet, in the corner the sheet leaves empty, rather than on top of
/// it. Inside, a plus has to fight the sheet's own outline for the two units it needs; outside, it
/// has clear ground and the pair reads at a glance.
const LINE_ADD_LAYER: &[Mark] = &[
    Mark::Path(&[
        [0.08, 0.34],
        [0.64, 0.34],
        [0.64, 0.90],
        [0.08, 0.90],
        [0.08, 0.34],
    ]),
    Mark::Path(&[[0.58, 0.22], [0.94, 0.22]]),
    Mark::Path(&[[0.76, 0.04], [0.76, 0.40]]),
];

/// Two offset sheets — the copy glyph, and the one shape in this family nobody has to be taught.
///
/// The sheet behind is only its two visible edges. Drawing it whole would put a full rectangle
/// under a full rectangle, and the overlap at this size is a hatched mess.
const LINE_DUPLICATE_LAYER: &[Mark] = &[
    Mark::Path(&[[0.28, 0.10], [0.90, 0.10], [0.90, 0.70]]),
    Mark::Path(&[
        [0.10, 0.30],
        [0.70, 0.30],
        [0.70, 0.90],
        [0.10, 0.90],
        [0.10, 0.30],
    ]),
];

/// A sheet dropping onto the rule below it.
///
/// The rule matters as much as the arrow: an arrow alone is "down", and down is not the same claim
/// as "into the thing underneath". The rule is what makes it merge rather than move.
const LINE_MERGE_DOWN: &[Mark] = &[
    Mark::Path(&[
        [0.22, 0.08],
        [0.78, 0.08],
        [0.78, 0.38],
        [0.22, 0.38],
        [0.22, 0.08],
    ]),
    Mark::Path(&[[0.50, 0.42], [0.50, 0.70]]),
    Mark::Poly(&[[0.50, 0.82], [0.34, 0.62], [0.66, 0.62]]),
    Mark::Path(&[[0.10, 0.94], [0.90, 0.94]]),
];

/// A bin: tapered body, a lid wider than both, a handle above it.
///
/// The lid overhanging on both sides is what makes this a bin rather than a cup at 22 units. Three
/// marks is already most of the budget, so the body has no ribs — they would be sub-unit stripes.
const LINE_DELETE_LAYER: &[Mark] = &[
    Mark::Path(&[[0.24, 0.32], [0.30, 0.90], [0.70, 0.90], [0.76, 0.32]]),
    Mark::Path(&[[0.10, 0.28], [0.90, 0.28]]),
    Mark::Path(&[[0.38, 0.28], [0.38, 0.12], [0.62, 0.12], [0.62, 0.28]]),
];

/// An open eye: an almond with a solid pupil.
///
/// The pupil is filled, and it carries the whole state. Against [`LINE_HIDDEN`] the difference the
/// eye actually catches at 22 units is "a dark round mass in the middle" versus "no mass at all" —
/// far stronger than a slash, which at this size is one more line among several.
const LINE_VISIBLE: &[Mark] = &[
    Mark::Path(&[
        [0.06, 0.50],
        [0.22, 0.30],
        [0.50, 0.22],
        [0.78, 0.30],
        [0.94, 0.50],
        [0.78, 0.70],
        [0.50, 0.78],
        [0.22, 0.70],
        [0.06, 0.50],
    ]),
    Mark::Circle {
        at: [0.50, 0.50],
        r: 0.15,
        filled: true,
    },
];

/// A closed eye: the lid drooping, with lashes under it.
///
/// A shut lid rather than an eye with a line through it. A struck-through icon needs the viewer to
/// find the thing being struck through first, and on a layer row at 22 units there is no time for
/// two readings.
const LINE_HIDDEN: &[Mark] = &[
    Mark::Path(&[
        [0.06, 0.36],
        [0.24, 0.56],
        [0.50, 0.64],
        [0.76, 0.56],
        [0.94, 0.36],
    ]),
    Mark::Path(&[[0.26, 0.60], [0.18, 0.80]]),
    Mark::Path(&[[0.50, 0.66], [0.50, 0.88]]),
    Mark::Path(&[[0.74, 0.60], [0.82, 0.80]]),
];

/// A padlock, shut.
///
/// The shackle is a polyline with square shoulders rather than a semicircle: a true half circle
/// 4 units across loses its opening to the stroke width and closes into a solid tab.
const LINE_LOCK_ALPHA: &[Mark] = &[
    Mark::Path(&[
        [0.22, 0.46],
        [0.78, 0.46],
        [0.78, 0.90],
        [0.22, 0.90],
        [0.22, 0.46],
    ]),
    Mark::Path(&[
        [0.34, 0.46],
        [0.34, 0.28],
        [0.40, 0.16],
        [0.60, 0.16],
        [0.66, 0.28],
        [0.66, 0.46],
    ]),
];

/// An arrow curving back to the left, its tail dropping away on the right.
///
/// The tail is the half that makes the pair work: two mirrored arcs alone are hard to tell apart
/// in peripheral vision, but "the tail hangs on the right" versus "the tail hangs on the left" is
/// a silhouette difference rather than a direction one.
const LINE_UNDO: &[Mark] = &[
    Mark::Path(&[
        [0.86, 0.86],
        [0.86, 0.60],
        [0.80, 0.46],
        [0.68, 0.38],
        [0.52, 0.34],
        [0.36, 0.36],
        [0.26, 0.42],
    ]),
    Mark::Poly(&[[0.08, 0.44], [0.32, 0.30], [0.32, 0.56]]),
];

/// [`LINE_UNDO`] mirrored, and nothing else.
///
/// Mirrored rather than redrawn deliberately: undo and redo are one gesture in two directions, and
/// an artist should be able to check which is which by shape alone.
const LINE_REDO: &[Mark] = &[
    Mark::Path(&[
        [0.14, 0.86],
        [0.14, 0.60],
        [0.20, 0.46],
        [0.32, 0.38],
        [0.48, 0.34],
        [0.64, 0.36],
        [0.74, 0.42],
    ]),
    Mark::Poly(&[[0.92, 0.44], [0.68, 0.30], [0.68, 0.56]]),
];

/// A frame split into three cells — this application's own layout tree, drawn small.
///
/// Not a hamburger and not a grid of dots. The panel list is about *where things sit*, and a split
/// frame says that; a stack of bars says "there is a menu here", which is a different promise.
const LINE_PANELS: &[Mark] = &[
    Mark::Path(&[
        [0.08, 0.16],
        [0.92, 0.16],
        [0.92, 0.84],
        [0.08, 0.84],
        [0.08, 0.16],
    ]),
    Mark::Path(&[[0.42, 0.16], [0.42, 0.84]]),
    Mark::Path(&[[0.42, 0.50], [0.92, 0.50]]),
];

/// Three tracks with their knobs at different places.
///
/// A gear was the obvious choice and is unusable here: its teeth are a sub-unit sawtooth at 22
/// units and grey into a ring. Sliders survive because the only detail that has to read is *the
/// knobs are not lined up*, and that is legible while the knobs are still round dots.
const LINE_SETTINGS: &[Mark] = &[
    Mark::Path(&[[0.08, 0.26], [0.92, 0.26]]),
    Mark::Circle {
        at: [0.66, 0.26],
        r: 0.12,
        filled: true,
    },
    Mark::Path(&[[0.08, 0.50], [0.92, 0.50]]),
    Mark::Circle {
        at: [0.34, 0.50],
        r: 0.12,
        filled: true,
    },
    Mark::Path(&[[0.08, 0.74], [0.92, 0.74]]),
    Mark::Circle {
        at: [0.60, 0.74],
        r: 0.12,
        filled: true,
    },
];

/// A frame with a two-headed diagonal pushing out towards its corners.
///
/// Reads as "grow until it touches the sides". Paired against [`LINE_ZOOM_ACTUAL`] on purpose: one
/// is about the content's relationship to the frame, the other about a fixed unit inside it, so
/// neither depends on the viewer reading a number.
const LINE_ZOOM_FIT: &[Mark] = &[
    Mark::Path(&[
        [0.08, 0.16],
        [0.92, 0.16],
        [0.92, 0.84],
        [0.08, 0.84],
        [0.08, 0.16],
    ]),
    Mark::Path(&[[0.32, 0.40], [0.68, 0.60]]),
    Mark::Poly(&[[0.18, 0.28], [0.42, 0.34], [0.24, 0.48]]),
    Mark::Poly(&[[0.82, 0.72], [0.58, 0.66], [0.76, 0.52]]),
];

/// A frame with one solid pixel at its centre.
///
/// "Actual size" means one image pixel to one screen pixel, and the only honest picture of that is
/// a pixel. `1:1` was the alternative and is three glyph-like shapes inside 22 units — the exact
/// mush this file exists to avoid.
const LINE_ZOOM_ACTUAL: &[Mark] = &[
    Mark::Path(&[
        [0.08, 0.16],
        [0.92, 0.16],
        [0.92, 0.84],
        [0.08, 0.84],
        [0.08, 0.16],
    ]),
    Mark::Poly(&[[0.38, 0.38], [0.62, 0.38], [0.62, 0.62], [0.38, 0.62]]),
];

/// A page with a turned corner and a plus on it.
///
/// The turned corner is two extra segments and is what stops this being a rounded rectangle with a
/// cross in it — the difference between "new document" and "add something".
const LINE_NEW_DOCUMENT: &[Mark] = &[
    Mark::Path(&[
        [0.16, 0.08],
        [0.58, 0.08],
        [0.80, 0.30],
        [0.80, 0.92],
        [0.16, 0.92],
        [0.16, 0.08],
    ]),
    Mark::Path(&[[0.58, 0.08], [0.58, 0.30], [0.80, 0.30]]),
    Mark::Path(&[[0.32, 0.62], [0.64, 0.62]]),
    Mark::Path(&[[0.48, 0.46], [0.48, 0.78]]),
];

/// A folder with its front flap swung open.
///
/// The flap is a parallelogram rather than a rectangle, and that skew is the whole difference
/// between an open folder and a closed one at this size.
const LINE_OPEN_DOCUMENT: &[Mark] = &[
    Mark::Path(&[
        [0.08, 0.80],
        [0.08, 0.20],
        [0.38, 0.20],
        [0.46, 0.34],
        [0.80, 0.34],
        [0.80, 0.46],
    ]),
    Mark::Path(&[
        [0.08, 0.86],
        [0.24, 0.46],
        [0.96, 0.46],
        [0.80, 0.86],
        [0.08, 0.86],
    ]),
];

/// A floppy disk: clipped corner, shutter above, label below.
///
/// Dated and unbeaten. The modern alternative — an arrow into a tray — is what every browser draws
/// for *download*, and in an application that also exports files that collision costs more than
/// the anachronism does.
const LINE_SAVE: &[Mark] = &[
    Mark::Path(&[
        [0.10, 0.10],
        [0.72, 0.10],
        [0.90, 0.28],
        [0.90, 0.90],
        [0.10, 0.90],
        [0.10, 0.10],
    ]),
    Mark::Path(&[[0.30, 0.10], [0.30, 0.36], [0.68, 0.36], [0.68, 0.10]]),
    Mark::Path(&[[0.28, 0.90], [0.28, 0.58], [0.72, 0.58], [0.72, 0.90]]),
];

/// The same disk, shrunk into the corner, with a pencil beside it.
///
/// The disk gives up a third of its size so the pencil has clear ground. A badge tucked against
/// the shape it modifies is two outlines a unit apart, which at 22 units is one thick outline.
const LINE_SAVE_AS: &[Mark] = &[
    Mark::Path(&[
        [0.04, 0.06],
        [0.50, 0.06],
        [0.66, 0.22],
        [0.66, 0.66],
        [0.04, 0.66],
        [0.04, 0.06],
    ]),
    Mark::Path(&[[0.18, 0.06], [0.18, 0.30], [0.50, 0.30], [0.50, 0.06]]),
    Mark::Path(&[
        [0.56, 0.98],
        [0.62, 0.80],
        [0.84, 0.58],
        [0.96, 0.70],
        [0.74, 0.92],
        [0.56, 0.98],
    ]),
];

/// A frame with an arrow leaving through the corner it is missing.
///
/// The break in the frame is doing the work: an arrow merely *next to* a box reads as "move", but
/// an arrow leaving through a gap in the box reads as "out of here", and export is the one command
/// where "out" is the whole meaning.
const LINE_EXPORT_IMAGE: &[Mark] = &[
    Mark::Path(&[
        [0.54, 0.10],
        [0.10, 0.10],
        [0.10, 0.90],
        [0.90, 0.90],
        [0.90, 0.48],
    ]),
    Mark::Path(&[[0.46, 0.54], [0.84, 0.16]]),
    Mark::Poly(&[[0.96, 0.04], [0.62, 0.10], [0.90, 0.38]]),
];

/// The whole frame taken.
///
/// The five selection commands share one frame and differ only in what is inside it, which is the
/// point: they are one family of operations on one thing, and giving each its own silhouette would
/// hide that they all act on the same selection.
const LINE_SELECT_ALL: &[Mark] = &[
    Mark::Path(&[
        [0.08, 0.10],
        [0.92, 0.10],
        [0.92, 0.90],
        [0.08, 0.90],
        [0.08, 0.10],
    ]),
    Mark::Poly(&[[0.24, 0.26], [0.76, 0.26], [0.76, 0.74], [0.24, 0.74]]),
];

/// The frame, struck through: nothing selected.
const LINE_DESELECT: &[Mark] = &[
    Mark::Path(&[
        [0.08, 0.10],
        [0.92, 0.10],
        [0.92, 0.90],
        [0.08, 0.90],
        [0.08, 0.10],
    ]),
    Mark::Path(&[[0.24, 0.74], [0.76, 0.26]]),
];

/// The frame with one diagonal half of its interior taken — in and out swapped.
const LINE_INVERT_SELECTION: &[Mark] = &[
    Mark::Path(&[
        [0.08, 0.10],
        [0.92, 0.10],
        [0.92, 0.90],
        [0.08, 0.90],
        [0.08, 0.10],
    ]),
    Mark::Poly(&[[0.24, 0.26], [0.76, 0.26], [0.24, 0.74]]),
];

/// The frame with a cross inside: the contents go.
///
/// A cross rather than the single slash of [`LINE_DESELECT`], because the two sit next to each
/// other in the same menu and one stroke against two is the clearest difference available inside a
/// frame this small.
const LINE_CLEAR_SELECTION: &[Mark] = &[
    Mark::Path(&[
        [0.08, 0.10],
        [0.92, 0.10],
        [0.92, 0.90],
        [0.08, 0.90],
        [0.08, 0.10],
    ]),
    Mark::Path(&[[0.26, 0.28], [0.74, 0.72]]),
    Mark::Path(&[[0.74, 0.28], [0.26, 0.72]]),
];

/// The frame with a drop of paint in it.
///
/// A drop, not a bucket: a bucket needs a body, a handle and a pour to be a bucket, and that is
/// three details inside a frame 15 units across.
const LINE_FILL_SELECTION: &[Mark] = &[
    Mark::Path(&[
        [0.08, 0.10],
        [0.92, 0.10],
        [0.92, 0.90],
        [0.08, 0.90],
        [0.08, 0.10],
    ]),
    Mark::Poly(&[
        [0.50, 0.24],
        [0.70, 0.50],
        [0.66, 0.68],
        [0.50, 0.76],
        [0.34, 0.68],
        [0.30, 0.50],
    ]),
];

// ----------------------------------------------------------------------------------------------
// Solid — filled silhouettes, for weight.
//
// The loud set. Every icon is one or two solid masses, which is what survives a dim panel, a
// glossy screen in daylight, and eyes that are done for the day.
//
// It is not the Line set thickened, and it could not be: **a filled shape cannot have a counter.**
// No ring, no pupil inside an outline, no hole of any kind, because holes need an even-odd fill
// rule and that would be a rule in every renderer that ever draws an icon — the cost the small
// primitive set exists to avoid. So wherever the Line set leans on a hole, this set says the same
// thing by separating masses with a gap, or by biting a notch out of an edge. That is why a few of
// the metaphors differ between the sets rather than only their weight; it is a consequence of the
// medium, taken deliberately.
// ----------------------------------------------------------------------------------------------

/// The Solid set's table.
fn solid_glyph(symbol: Symbol) -> Option<&'static [Mark]> {
    Some(match symbol {
        Symbol::Brush => SOLID_BRUSH,
        Symbol::Eraser => SOLID_ERASER,
        Symbol::Lasso => SOLID_LASSO,
        Symbol::RectSelect => SOLID_RECT_SELECT,
        Symbol::Wand => SOLID_WAND,
        Symbol::MoveSelection => SOLID_MOVE_SELECTION,
        Symbol::AddLayer => SOLID_ADD_LAYER,
        Symbol::DuplicateLayer => SOLID_DUPLICATE_LAYER,
        Symbol::MergeDown => SOLID_MERGE_DOWN,
        Symbol::DeleteLayer => SOLID_DELETE_LAYER,
        Symbol::Visible => SOLID_VISIBLE,
        Symbol::Hidden => SOLID_HIDDEN,
        Symbol::LockAlpha => SOLID_LOCK_ALPHA,
        Symbol::Undo => SOLID_UNDO,
        Symbol::Redo => SOLID_REDO,
        Symbol::Panels => SOLID_PANELS,
        Symbol::Settings => SOLID_SETTINGS,
        Symbol::ZoomFit => SOLID_ZOOM_FIT,
        Symbol::ZoomActual => SOLID_ZOOM_ACTUAL,
        Symbol::NewDocument => SOLID_NEW_DOCUMENT,
        Symbol::OpenDocument => SOLID_OPEN_DOCUMENT,
        Symbol::Save => SOLID_SAVE,
        Symbol::SaveAs => SOLID_SAVE_AS,
        Symbol::ExportImage => SOLID_EXPORT_IMAGE,
        Symbol::SelectAll => SOLID_SELECT_ALL,
        Symbol::Deselect => SOLID_DESELECT,
        Symbol::InvertSelection => SOLID_INVERT_SELECTION,
        Symbol::ClearSelection => SOLID_CLEAR_SELECTION,
        Symbol::FillSelection => SOLID_FILL_SELECTION,
    })
}

/// Handle and tip as two solid masses, with the ferrule left as bare ground.
const SOLID_BRUSH: &[Mark] = &[
    Mark::Poly(&[[0.70, 0.06], [0.94, 0.30], [0.52, 0.66], [0.28, 0.42]]),
    Mark::Poly(&[[0.22, 0.48], [0.46, 0.72], [0.06, 0.94]]),
];

/// A solid block on a solid rule, leaning against the brush.
const SOLID_ERASER: &[Mark] = &[
    Mark::Poly(&[[0.12, 0.48], [0.48, 0.10], [0.86, 0.40], [0.50, 0.78]]),
    Mark::Poly(&[[0.06, 0.86], [0.94, 0.86], [0.94, 0.96], [0.06, 0.96]]),
];

/// An irregular filled region with the rope hanging off it.
///
/// The Line set draws the loop of rope; a filled loop is a disc, so this set draws what the loop
/// *caught* instead — an area with no two edges alike, which is what free-hand selection leaves
/// behind.
const SOLID_LASSO: &[Mark] = &[
    Mark::Poly(&[
        [0.18, 0.44],
        [0.32, 0.16],
        [0.64, 0.10],
        [0.88, 0.32],
        [0.82, 0.60],
        [0.54, 0.76],
        [0.28, 0.66],
    ]),
    Mark::Poly(&[[0.24, 0.66], [0.32, 0.68], [0.30, 0.96], [0.20, 0.94]]),
];

/// Four solid corner brackets.
const SOLID_RECT_SELECT: &[Mark] = &[
    Mark::Poly(&[
        [0.06, 0.06],
        [0.38, 0.06],
        [0.38, 0.18],
        [0.18, 0.18],
        [0.18, 0.38],
        [0.06, 0.38],
    ]),
    Mark::Poly(&[
        [0.94, 0.06],
        [0.94, 0.38],
        [0.82, 0.38],
        [0.82, 0.18],
        [0.62, 0.18],
        [0.62, 0.06],
    ]),
    Mark::Poly(&[
        [0.94, 0.94],
        [0.62, 0.94],
        [0.62, 0.82],
        [0.82, 0.82],
        [0.82, 0.62],
        [0.94, 0.62],
    ]),
    Mark::Poly(&[
        [0.06, 0.94],
        [0.06, 0.62],
        [0.18, 0.62],
        [0.18, 0.82],
        [0.38, 0.82],
        [0.38, 0.94],
    ]),
];

/// A solid stick and a four-point star.
///
/// The star's waist is safe here where it was not in the Line set: filled, a narrowing that closes
/// up costs nothing, because the ink either side of it is ink either way.
const SOLID_WAND: &[Mark] = &[
    Mark::Poly(&[[0.08, 0.82], [0.50, 0.40], [0.60, 0.50], [0.18, 0.92]]),
    Mark::Poly(&[
        [0.72, 0.04],
        [0.79, 0.19],
        [0.94, 0.26],
        [0.79, 0.33],
        [0.72, 0.48],
        [0.65, 0.33],
        [0.50, 0.26],
        [0.65, 0.19],
    ]),
];

/// One filled four-way arrow.
///
/// A single outline traced round all four arms, rather than a cross plus four heads: at this size
/// the join between a shaft and its head is one unit, and drawn as separate masses it shows as a
/// seam.
const SOLID_MOVE_SELECTION: &[Mark] = &[Mark::Poly(&[
    [0.50, 0.06],
    [0.66, 0.24],
    [0.57, 0.24],
    [0.57, 0.43],
    [0.76, 0.43],
    [0.76, 0.34],
    [0.94, 0.50],
    [0.76, 0.66],
    [0.76, 0.57],
    [0.57, 0.57],
    [0.57, 0.76],
    [0.66, 0.76],
    [0.50, 0.94],
    [0.34, 0.76],
    [0.43, 0.76],
    [0.43, 0.57],
    [0.24, 0.57],
    [0.24, 0.66],
    [0.06, 0.50],
    [0.24, 0.34],
    [0.24, 0.43],
    [0.43, 0.43],
    [0.43, 0.24],
    [0.34, 0.24],
])];

/// A solid sheet with a solid plus in the corner it leaves free.
const SOLID_ADD_LAYER: &[Mark] = &[
    Mark::Poly(&[[0.06, 0.36], [0.62, 0.36], [0.62, 0.92], [0.06, 0.92]]),
    Mark::Poly(&[
        [0.67, 0.04],
        [0.81, 0.04],
        [0.81, 0.17],
        [0.94, 0.17],
        [0.94, 0.31],
        [0.81, 0.31],
        [0.81, 0.44],
        [0.67, 0.44],
        [0.67, 0.31],
        [0.54, 0.31],
        [0.54, 0.17],
        [0.67, 0.17],
    ]),
];

/// A sheet in front, and only the band of the sheet behind that it does not cover.
///
/// The band, not a second rectangle. Two overlapping filled rectangles in one colour *are* one
/// rectangle; the gap between them is the only thing that says there are two.
const SOLID_DUPLICATE_LAYER: &[Mark] = &[
    Mark::Poly(&[
        [0.30, 0.06],
        [0.94, 0.06],
        [0.94, 0.70],
        [0.80, 0.70],
        [0.80, 0.20],
        [0.30, 0.20],
    ]),
    Mark::Poly(&[[0.06, 0.30], [0.70, 0.30], [0.70, 0.94], [0.06, 0.94]]),
];

/// A solid sheet, a solid arrow, a solid rule.
const SOLID_MERGE_DOWN: &[Mark] = &[
    Mark::Poly(&[[0.24, 0.04], [0.76, 0.04], [0.76, 0.32], [0.24, 0.32]]),
    Mark::Poly(&[
        [0.42, 0.38],
        [0.58, 0.38],
        [0.58, 0.58],
        [0.72, 0.58],
        [0.50, 0.80],
        [0.28, 0.58],
        [0.42, 0.58],
    ]),
    Mark::Poly(&[[0.08, 0.86], [0.92, 0.86], [0.92, 0.96], [0.08, 0.96]]),
];

/// A solid bin: handle, lid, tapered body, each separated by clear ground.
const SOLID_DELETE_LAYER: &[Mark] = &[
    Mark::Poly(&[[0.38, 0.06], [0.62, 0.06], [0.62, 0.18], [0.38, 0.18]]),
    Mark::Poly(&[[0.06, 0.22], [0.94, 0.22], [0.94, 0.34], [0.06, 0.34]]),
    Mark::Poly(&[[0.18, 0.40], [0.82, 0.40], [0.74, 0.94], [0.26, 0.94]]),
];

/// A disc between two corner wedges — an eye reduced to its pupil and its two corners.
///
/// The Line set's almond cannot be filled without swallowing the pupil, so here the pupil *is* the
/// icon and the almond is implied by the wedges either side of it. The same shape, read from the
/// inside out.
const SOLID_VISIBLE: &[Mark] = &[
    Mark::Circle {
        at: [0.50, 0.50],
        r: 0.26,
        filled: true,
    },
    Mark::Poly(&[[0.04, 0.50], [0.26, 0.34], [0.26, 0.66]]),
    Mark::Poly(&[[0.96, 0.50], [0.74, 0.34], [0.74, 0.66]]),
];

/// A heavy shut lid with three lashes under it.
///
/// Against [`SOLID_VISIBLE`] the difference is a round mass in the middle versus a horizontal bar
/// near the top — a silhouette apart, which is what the eye sorts a layer list by.
const SOLID_HIDDEN: &[Mark] = &[
    Mark::Poly(&[
        [0.04, 0.30],
        [0.96, 0.30],
        [0.86, 0.48],
        [0.50, 0.60],
        [0.14, 0.48],
    ]),
    Mark::Poly(&[[0.18, 0.60], [0.28, 0.64], [0.20, 0.86], [0.10, 0.82]]),
    Mark::Poly(&[[0.45, 0.66], [0.55, 0.66], [0.55, 0.90], [0.45, 0.90]]),
    Mark::Poly(&[[0.72, 0.64], [0.82, 0.60], [0.90, 0.82], [0.80, 0.86]]),
];

/// A solid body under a solid arch.
///
/// The shackle keeps its opening without a fill rule: the polygon runs up one side and back down
/// the inside, and the gap it leaves is at the bottom where the body covers it. A hole would have
/// needed even-odd; a notch does not.
const SOLID_LOCK_ALPHA: &[Mark] = &[
    Mark::Poly(&[[0.18, 0.44], [0.82, 0.44], [0.82, 0.94], [0.18, 0.94]]),
    Mark::Poly(&[
        [0.30, 0.44],
        [0.30, 0.24],
        [0.38, 0.10],
        [0.62, 0.10],
        [0.70, 0.24],
        [0.70, 0.44],
        [0.60, 0.44],
        [0.60, 0.24],
        [0.56, 0.18],
        [0.44, 0.18],
        [0.40, 0.24],
        [0.40, 0.44],
    ]),
];

/// A right-angled hook with a solid head, turning back to the left.
///
/// Angular where the Line set curves. A filled arc is a band bounded by two arcs — nine points a
/// side for a shape 7 units across — and the corner says the same thing while staying crisp.
const SOLID_UNDO: &[Mark] = &[
    Mark::Poly(&[
        [0.28, 0.32],
        [0.86, 0.32],
        [0.86, 0.90],
        [0.72, 0.90],
        [0.72, 0.46],
        [0.28, 0.46],
    ]),
    Mark::Poly(&[[0.06, 0.39], [0.34, 0.22], [0.34, 0.56]]),
];

/// [`SOLID_UNDO`] mirrored.
const SOLID_REDO: &[Mark] = &[
    Mark::Poly(&[
        [0.72, 0.32],
        [0.14, 0.32],
        [0.14, 0.90],
        [0.28, 0.90],
        [0.28, 0.46],
        [0.72, 0.46],
    ]),
    Mark::Poly(&[[0.94, 0.39], [0.66, 0.22], [0.66, 0.56]]),
];

/// Three filled cells with the gutters showing between them.
///
/// The gutters are the theme's own idea of structure (`Metrics::gutter`), so this icon is the
/// workspace seen from far enough away — the closest thing in the file to a literal picture of
/// what the button does.
const SOLID_PANELS: &[Mark] = &[
    Mark::Poly(&[[0.06, 0.14], [0.38, 0.14], [0.38, 0.86], [0.06, 0.86]]),
    Mark::Poly(&[[0.44, 0.14], [0.94, 0.14], [0.94, 0.46], [0.44, 0.46]]),
    Mark::Poly(&[[0.44, 0.54], [0.94, 0.54], [0.94, 0.86], [0.44, 0.86]]),
];

/// Three solid tracks, knobs out of line.
const SOLID_SETTINGS: &[Mark] = &[
    Mark::Poly(&[[0.06, 0.22], [0.94, 0.22], [0.94, 0.30], [0.06, 0.30]]),
    Mark::Circle {
        at: [0.66, 0.26],
        r: 0.14,
        filled: true,
    },
    Mark::Poly(&[[0.06, 0.46], [0.94, 0.46], [0.94, 0.54], [0.06, 0.54]]),
    Mark::Circle {
        at: [0.34, 0.50],
        r: 0.14,
        filled: true,
    },
    Mark::Poly(&[[0.06, 0.70], [0.94, 0.70], [0.94, 0.78], [0.06, 0.78]]),
    Mark::Circle {
        at: [0.60, 0.74],
        r: 0.14,
        filled: true,
    },
];

/// Four wedges driving out to the corners, away from a pixel in the middle.
///
/// Diagonal wedges rather than the L-shaped brackets of [`SOLID_RECT_SELECT`], so the two do not
/// become the same icon with different filling — they are one press apart in the same UI.
const SOLID_ZOOM_FIT: &[Mark] = &[
    Mark::Poly(&[[0.04, 0.04], [0.40, 0.10], [0.10, 0.40]]),
    Mark::Poly(&[[0.96, 0.04], [0.60, 0.10], [0.90, 0.40]]),
    Mark::Poly(&[[0.96, 0.96], [0.60, 0.90], [0.90, 0.60]]),
    Mark::Poly(&[[0.04, 0.96], [0.40, 0.90], [0.10, 0.60]]),
    Mark::Poly(&[[0.42, 0.42], [0.58, 0.42], [0.58, 0.58], [0.42, 0.58]]),
];

/// A frame of four bars with one solid pixel at its centre.
///
/// The frame is four separate bars rather than one outline because a filled outline is a filled
/// rectangle — the same counter problem the eye has, solved the same way.
const SOLID_ZOOM_ACTUAL: &[Mark] = &[
    Mark::Poly(&[[0.06, 0.12], [0.94, 0.12], [0.94, 0.22], [0.06, 0.22]]),
    Mark::Poly(&[[0.06, 0.78], [0.94, 0.78], [0.94, 0.88], [0.06, 0.88]]),
    Mark::Poly(&[[0.06, 0.12], [0.16, 0.12], [0.16, 0.88], [0.06, 0.88]]),
    Mark::Poly(&[[0.84, 0.12], [0.94, 0.12], [0.94, 0.88], [0.84, 0.88]]),
    Mark::Poly(&[[0.40, 0.42], [0.60, 0.42], [0.60, 0.58], [0.40, 0.58]]),
];

/// A solid page with its corner cut, and a solid plus clear of it.
const SOLID_NEW_DOCUMENT: &[Mark] = &[
    Mark::Poly(&[
        [0.06, 0.06],
        [0.36, 0.06],
        [0.50, 0.20],
        [0.50, 0.64],
        [0.06, 0.64],
    ]),
    Mark::Poly(&[
        [0.67, 0.50],
        [0.81, 0.50],
        [0.81, 0.65],
        [0.96, 0.65],
        [0.96, 0.79],
        [0.81, 0.79],
        [0.81, 0.94],
        [0.67, 0.94],
        [0.67, 0.79],
        [0.52, 0.79],
        [0.52, 0.65],
        [0.67, 0.65],
    ]),
];

/// A solid folder back with its flap swung open in front.
const SOLID_OPEN_DOCUMENT: &[Mark] = &[
    Mark::Poly(&[
        [0.04, 0.14],
        [0.34, 0.14],
        [0.42, 0.26],
        [0.86, 0.26],
        [0.86, 0.40],
        [0.04, 0.40],
    ]),
    Mark::Poly(&[[0.10, 0.90], [0.24, 0.46], [0.96, 0.46], [0.82, 0.90]]),
];

/// A floppy disk as one mass, with the shutter and the label bitten out of its edges.
///
/// Notches rather than holes, so the outline never has to close on itself: it walks in from the
/// top edge, round the shutter, back out, and later does the same from the bottom for the label.
/// That keeps the disk recognisable in a set that cannot cut a hole in anything.
const SOLID_SAVE: &[Mark] = &[Mark::Poly(&[
    [0.06, 0.06],
    [0.26, 0.06],
    [0.26, 0.34],
    [0.62, 0.34],
    [0.62, 0.06],
    [0.70, 0.06],
    [0.94, 0.30],
    [0.94, 0.94],
    [0.74, 0.94],
    [0.74, 0.62],
    [0.26, 0.62],
    [0.26, 0.94],
    [0.06, 0.94],
])];

/// The same disk, smaller, with a solid pencil beside it.
const SOLID_SAVE_AS: &[Mark] = &[
    Mark::Poly(&[
        [0.04, 0.06],
        [0.18, 0.06],
        [0.18, 0.26],
        [0.44, 0.26],
        [0.44, 0.06],
        [0.50, 0.06],
        [0.66, 0.22],
        [0.66, 0.66],
        [0.52, 0.66],
        [0.52, 0.46],
        [0.18, 0.46],
        [0.18, 0.66],
        [0.04, 0.66],
    ]),
    Mark::Poly(&[
        [0.56, 0.98],
        [0.62, 0.80],
        [0.84, 0.58],
        [0.96, 0.70],
        [0.74, 0.92],
    ]),
];

/// A solid frame open at one corner, with an arrow leaving through the gap.
const SOLID_EXPORT_IMAGE: &[Mark] = &[
    Mark::Poly(&[
        [0.08, 0.08],
        [0.44, 0.08],
        [0.44, 0.22],
        [0.22, 0.22],
        [0.22, 0.78],
        [0.78, 0.78],
        [0.78, 0.56],
        [0.92, 0.56],
        [0.92, 0.92],
        [0.08, 0.92],
    ]),
    Mark::Poly(&[
        [0.94, 0.06],
        [0.56, 0.10],
        [0.68, 0.22],
        [0.48, 0.42],
        [0.58, 0.52],
        [0.78, 0.32],
        [0.90, 0.44],
    ]),
];

/// A solid frame, whole, with everything inside it taken.
///
/// The frame is two interlocking L-shapes rather than four bars: two masses instead of four, and
/// the same closed rectangle. All five selection commands carry it, for the same reason the Line
/// set's five all carry one outline.
const SOLID_SELECT_ALL: &[Mark] = &[
    Mark::Poly(&[
        [0.04, 0.06],
        [0.96, 0.06],
        [0.96, 0.17],
        [0.15, 0.17],
        [0.15, 0.94],
        [0.04, 0.94],
    ]),
    Mark::Poly(&[
        [0.96, 0.06],
        [0.96, 0.94],
        [0.04, 0.94],
        [0.04, 0.83],
        [0.85, 0.83],
        [0.85, 0.06],
    ]),
    Mark::Poly(&[[0.26, 0.28], [0.74, 0.28], [0.74, 0.72], [0.26, 0.72]]),
];

/// The frame with one heavy stroke through it.
const SOLID_DESELECT: &[Mark] = &[
    Mark::Poly(&[
        [0.04, 0.06],
        [0.96, 0.06],
        [0.96, 0.17],
        [0.15, 0.17],
        [0.15, 0.94],
        [0.04, 0.94],
    ]),
    Mark::Poly(&[
        [0.96, 0.06],
        [0.96, 0.94],
        [0.04, 0.94],
        [0.04, 0.83],
        [0.85, 0.83],
        [0.85, 0.06],
    ]),
    Mark::Poly(&[[0.24, 0.66], [0.32, 0.74], [0.76, 0.34], [0.68, 0.26]]),
];

/// The frame with a diagonal half of its interior taken.
const SOLID_INVERT_SELECTION: &[Mark] = &[
    Mark::Poly(&[
        [0.04, 0.06],
        [0.96, 0.06],
        [0.96, 0.17],
        [0.15, 0.17],
        [0.15, 0.94],
        [0.04, 0.94],
    ]),
    Mark::Poly(&[
        [0.96, 0.06],
        [0.96, 0.94],
        [0.04, 0.94],
        [0.04, 0.83],
        [0.85, 0.83],
        [0.85, 0.06],
    ]),
    Mark::Poly(&[[0.26, 0.28], [0.74, 0.28], [0.26, 0.72]]),
];

/// The frame with a heavy cross inside it.
const SOLID_CLEAR_SELECTION: &[Mark] = &[
    Mark::Poly(&[
        [0.04, 0.06],
        [0.96, 0.06],
        [0.96, 0.17],
        [0.15, 0.17],
        [0.15, 0.94],
        [0.04, 0.94],
    ]),
    Mark::Poly(&[
        [0.96, 0.06],
        [0.96, 0.94],
        [0.04, 0.94],
        [0.04, 0.83],
        [0.85, 0.83],
        [0.85, 0.06],
    ]),
    Mark::Poly(&[[0.24, 0.34], [0.32, 0.26], [0.76, 0.66], [0.68, 0.74]]),
    Mark::Poly(&[[0.24, 0.66], [0.32, 0.74], [0.76, 0.34], [0.68, 0.26]]),
];

/// The frame with a drop of paint in it.
const SOLID_FILL_SELECTION: &[Mark] = &[
    Mark::Poly(&[
        [0.04, 0.06],
        [0.96, 0.06],
        [0.96, 0.17],
        [0.15, 0.17],
        [0.15, 0.94],
        [0.04, 0.94],
    ]),
    Mark::Poly(&[
        [0.96, 0.06],
        [0.96, 0.94],
        [0.04, 0.94],
        [0.04, 0.83],
        [0.85, 0.83],
        [0.85, 0.06],
    ]),
    Mark::Poly(&[
        [0.50, 0.24],
        [0.72, 0.50],
        [0.68, 0.68],
        [0.50, 0.78],
        [0.32, 0.68],
        [0.28, 0.50],
    ]),
];

#[cfg(test)]
mod tests {

    /// **A filled outline is cut into triangles that cover it exactly.**
    ///
    /// Area is the property worth checking: it catches a fan across a concave shape, a flipped
    /// winding, a dropped ear and a triangle that strays outside -- all of which produce the right
    /// *number* of triangles and the wrong picture.
    #[test]
    fn every_filled_outline_is_cut_up_exactly() {
        for set in SETS {
            for symbol in Symbol::ALL {
                let Some(marks) = set.glyph(*symbol) else {
                    continue;
                };
                for mark in marks {
                    let Mark::Poly(points) = mark else {
                        continue;
                    };
                    let cut = triangles(points);
                    assert_eq!(
                        cut.len(),
                        points.len() - 2,
                        "{} {symbol:?}: {} points cut into {} triangles",
                        set.name,
                        points.len(),
                        cut.len()
                    );
                    let whole = outline_area(points).abs();
                    let pieces: f32 = cut
                        .iter()
                        .map(|t| {
                            (super::cross(points[t[0]], points[t[1]], points[t[2]]) / 2.0).abs()
                        })
                        .sum();
                    assert!(
                        (whole - pieces).abs() < whole * 0.001 + 1e-6,
                        "{} {symbol:?}: the outline is {whole} but its triangles are {pieces}",
                        set.name
                    );
                }
            }
        }
    }

    /// A concave outline is cut correctly, which a fan from one vertex cannot do.
    #[test]
    fn a_concave_outline_is_not_fanned() {
        // An L. The corner at index 2 is reflex, so a fan from vertex 0 covers ground outside it.
        let l = [
            [0.0, 0.0],
            [2.0, 0.0],
            [2.0, 1.0],
            [1.0, 1.0],
            [1.0, 2.0],
            [0.0, 2.0],
        ];
        let cut = triangles(&l);
        assert_eq!(cut.len(), 4);
        let pieces: f32 = cut
            .iter()
            .map(|t| (super::cross(l[t[0]], l[t[1]], l[t[2]]) / 2.0).abs())
            .sum();
        assert!(
            (pieces - 3.0).abs() < 0.001,
            "an L of area 3 came out as {pieces}"
        );
    }

    /// Nothing that cannot be cut gets drawn wrong instead.
    #[test]
    fn an_outline_that_cannot_be_cut_yields_nothing() {
        assert!(triangles(&[]).is_empty());
        assert!(triangles(&[[0.0, 0.0], [1.0, 1.0]]).is_empty());
        // A figure of eight is not a simple polygon, and ear clipping's contract does not cover
        // one. What is promised is that it *terminates* and hands back nothing absurd -- not that
        // it detects the problem, which it cannot do cheaply. The guarantee that the shipped icons
        // are simple comes from `every_filled_outline_is_cut_up_exactly`, which compares areas.
        let bowtie = [[0.0, 0.0], [1.0, 1.0], [1.0, 0.0], [0.0, 1.0]];
        let cut = triangles(&bowtie);
        assert!(cut.len() <= bowtie.len() - 2);
        for t in &cut {
            assert!(t.iter().all(|&i| i < bowtie.len()), "an index off the end");
        }
    }

    fn outline_area(points: &[[f32; 2]]) -> f32 {
        let n = points.len();
        (0..n)
            .map(|i| {
                let (p, q) = (points[i], points[(i + 1) % n]);
                p[0].mul_add(q[1], -(q[0] * p[1]))
            })
            .sum::<f32>()
            / 2.0
    }

    use super::*;

    /// The most marks an icon may be made of before it stops being readable at 22 units.
    ///
    /// Not a style rule. Six marks over roughly 6 mm leaves each about a millimetre of clear
    /// ground, and past that an icon is a texture rather than a shape. The number is a budget
    /// chosen against a size, the same way `theme`'s grab surfaces are chosen against 4 mm.
    const MARK_BUDGET: usize = 6;

    /// Every drawn glyph in every set, with enough context to say which one failed.
    fn all_glyphs() -> impl Iterator<Item = (&'static str, Symbol, &'static [Mark])> {
        SETS.iter().flat_map(|set| {
            Symbol::ALL
                .iter()
                .filter_map(move |&s| set.glyph(s).map(|marks| (set.name, s, marks)))
        })
    }

    /// Every point a mark reaches, a circle counted by the corners of its bounding box.
    fn extremes(mark: &Mark) -> Vec<[f32; 2]> {
        match mark {
            Mark::Path(points) | Mark::Poly(points) => points.to_vec(),
            Mark::Circle { at, r, .. } => {
                vec![[at[0] - r, at[1] - r], [at[0] + r, at[1] + r]]
            }
        }
    }

    /// **A set is all pictures or all words — never half of each.**
    ///
    /// This is what lets the words set exist without a special case anywhere. `None` has exactly
    /// one meaning, "this set draws nothing", so a caller can fall back to the label without ever
    /// asking which set it holds. A set with a hole in it would give `None` a second meaning —
    /// "we forgot this one" — and the fallback would quietly paper over the omission, which is
    /// the shape of failure §6b exists to forbid.
    #[test]
    fn a_set_is_wholly_drawn_or_wholly_words() {
        for set in SETS {
            let drawn = Symbol::ALL
                .iter()
                .filter(|&&s| set.glyph(s).is_some())
                .count();
            assert!(
                drawn == 0 || drawn == Symbol::ALL.len(),
                "{}: draws {drawn} of {} symbols -- a set covers every role or none of them",
                set.name,
                Symbol::ALL.len()
            );
        }
    }

    /// The invariant above is satisfied by an empty table, so this says the table is not empty:
    /// there is a words set to fall back to, and enough drawn sets that choosing one is a choice.
    #[test]
    fn there_is_a_words_set_and_a_real_choice_of_drawn_ones() {
        let drawn = SETS
            .iter()
            .filter(|set| set.glyph(Symbol::Brush).is_some())
            .count();
        assert_eq!(
            SETS.len() - drawn,
            1,
            "exactly one set should draw nothing at all"
        );
        assert!(
            drawn >= 2,
            "one drawn set is not a choice; found {drawn} beside Words"
        );
    }

    /// Nothing pokes out of the unit square, so a caller maps an icon into a box with a multiply
    /// and an add and never has to think about clipping.
    #[test]
    fn every_mark_stays_inside_the_unit_square() {
        for (set, symbol, marks) in all_glyphs() {
            for mark in marks {
                for [x, y] in extremes(mark) {
                    assert!(
                        (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y),
                        "{set}/{symbol:?}: [{x}, {y}] is outside the unit square"
                    );
                }
            }
        }
    }

    /// A NaN coordinate sails through the bounds check above: every comparison against it is
    /// false, so `contains` says no and the failure gets blamed on the wrong thing. Checked
    /// separately so the message names the real fault.
    #[test]
    fn every_coordinate_is_a_real_number() {
        for (set, symbol, marks) in all_glyphs() {
            for mark in marks {
                if let Mark::Circle { r, .. } = mark {
                    assert!(
                        r.is_finite(),
                        "{set}/{symbol:?}: the radius is not a number"
                    );
                }
                for [x, y] in extremes(mark) {
                    assert!(
                        x.is_finite() && y.is_finite(),
                        "{set}/{symbol:?}: [{x}, {y}] is not a pair of real numbers"
                    );
                }
            }
        }
    }

    /// A shape has to have enough points to be one, and a circle has to have a size.
    ///
    /// A one-point path and a two-point polygon both draw nothing, which is the worst outcome
    /// available here: a button that silently loses its icon looks exactly like a button that is
    /// broken (§6b).
    #[test]
    fn no_mark_is_too_thin_to_be_a_shape() {
        for (set, symbol, marks) in all_glyphs() {
            assert!(!marks.is_empty(), "{set}/{symbol:?}: no marks at all");
            for mark in marks {
                match mark {
                    Mark::Path(points) => assert!(
                        points.len() >= 2,
                        "{set}/{symbol:?}: a path of {} points draws nothing",
                        points.len()
                    ),
                    Mark::Poly(points) => assert!(
                        points.len() >= 3,
                        "{set}/{symbol:?}: a polygon of {} points has no area",
                        points.len()
                    ),
                    Mark::Circle { r, .. } => assert!(
                        *r > 0.0,
                        "{set}/{symbol:?}: a circle of radius {r} draws nothing"
                    ),
                }
            }
        }
    }

    /// A mark has to enclose something, not merely have enough points to look like it does.
    ///
    /// Found by sabotage: a polygon whose four points are all the same passes every count-based
    /// check above and draws nothing at all. So do three points on a line. Counting points is a
    /// proxy; area and extent are the thing itself, and the failure they prevent is the same one
    /// as before -- an icon that silently is not there.
    ///
    /// The floors are sizes rather than epsilons. A fill of 0.01 of the unit square is about five
    /// square units at 22, which is roughly the smallest blob that reads as a shape instead of a
    /// speck; a stroke spanning 0.1 is a little over two units end to end. Every real mark in the
    /// file clears both by at least double.
    #[test]
    fn no_mark_collapses_to_nothing() {
        /// Twice the shoelace area, which is all this needs: it is compared against a floor.
        fn twice_area(points: &[[f32; 2]]) -> f32 {
            points
                .iter()
                .zip(points.iter().cycle().skip(1))
                .map(|(a, b)| b[0].mul_add(a[1], -(a[0] * b[1])))
                .sum::<f32>()
                .abs()
        }

        /// The diagonal of a mark's bounding box.
        fn span(points: &[[f32; 2]]) -> f32 {
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for p in points {
                for axis in 0..2 {
                    lo[axis] = lo[axis].min(p[axis]);
                    hi[axis] = hi[axis].max(p[axis]);
                }
            }
            (hi[0] - lo[0]).hypot(hi[1] - lo[1])
        }

        for (set, symbol, marks) in all_glyphs() {
            for mark in marks {
                match mark {
                    Mark::Path(points) => assert!(
                        span(points) >= 0.1,
                        "{set}/{symbol:?}: a stroke spanning {:.3} is too short to see",
                        span(points)
                    ),
                    Mark::Poly(points) => assert!(
                        twice_area(points) >= 0.02,
                        "{set}/{symbol:?}: a polygon of area {:.4} fills nothing",
                        twice_area(points) / 2.0
                    ),
                    Mark::Circle { r, .. } => assert!(
                        *r >= 0.04,
                        "{set}/{symbol:?}: a circle of radius {r} is under a unit across"
                    ),
                }
            }
        }
    }

    /// **The copy-paste check, and the reason this file has tests at all.**
    ///
    /// Twenty-nine icons in a set get written by copying the one above and changing the numbers,
    /// and the way that goes wrong is forgetting the second half: two roles end up with the same
    /// picture, both look plausible in review, and the rail then has two buttons that cannot be
    /// told apart. Nothing else here would notice.
    #[test]
    fn no_two_symbols_in_a_set_are_drawn_alike() {
        for set in SETS {
            for (i, &a) in Symbol::ALL.iter().enumerate() {
                for &b in Symbol::ALL.iter().skip(i + 1) {
                    let (Some(first), Some(second)) = (set.glyph(a), set.glyph(b)) else {
                        continue;
                    };
                    assert_ne!(
                        first, second,
                        "{}: {a:?} and {b:?} are the same drawing, so one of them cannot be read",
                        set.name
                    );
                }
            }
        }
    }

    /// The same mistake one level up: a set copied wholesale and then only half redrawn.
    ///
    /// If two sets draw a role identically then choosing between them is not a choice for that
    /// role, and the set that borrowed it has not been designed so much as duplicated.
    #[test]
    fn no_two_sets_draw_a_symbol_alike() {
        for (i, first) in SETS.iter().enumerate() {
            for second in SETS.iter().skip(i + 1) {
                for &symbol in Symbol::ALL {
                    let (Some(a), Some(b)) = (first.glyph(symbol), second.glyph(symbol)) else {
                        continue;
                    };
                    assert_ne!(
                        a, b,
                        "{} and {} draw {symbol:?} identically",
                        first.name, second.name
                    );
                }
            }
        }
    }

    /// A set is chosen from a list by name and remembered by id, so both have to be unambiguous.
    ///
    /// Two sets sharing a name makes the chooser a coin toss; two sharing an id makes the
    /// *setting* a coin toss, which is worse because it survives a restart.
    #[test]
    fn every_set_is_named_once_and_numbered_once() {
        for (i, set) in SETS.iter().enumerate() {
            assert!(
                !set.name.trim().is_empty(),
                "the set at {i} has no name to offer"
            );
            for other in SETS.iter().skip(i + 1) {
                assert_ne!(
                    set.name, other.name,
                    "two sets are both called {:?}",
                    set.name
                );
                assert_ne!(
                    set.id, other.id,
                    "{} and {} share id {}",
                    set.name, other.name, set.id
                );
            }
        }
    }

    /// No icon spends more than its share of a 6 mm square.
    ///
    /// The design rule written down where it can fail, rather than left as an intention in a
    /// comment: an icon that grew a seventh mark got detailed, and detail is the first thing that
    /// stops working as things get smaller.
    #[test]
    fn no_icon_spends_more_than_its_budget_of_marks() {
        for (set, symbol, marks) in all_glyphs() {
            assert!(
                marks.len() <= MARK_BUDGET,
                "{set}/{symbol:?} is {} marks, over the {MARK_BUDGET} an icon carries at 22 units",
                marks.len()
            );
        }
    }

    /// The generated list holds each role exactly once.
    ///
    /// The macro makes omission impossible but not repetition: a role written twice in the
    /// `symbols!` list would compile, and would then make one of the lookup's arms unreachable.
    #[test]
    fn every_role_appears_in_the_list_exactly_once() {
        for (i, a) in Symbol::ALL.iter().enumerate() {
            for b in Symbol::ALL.iter().skip(i + 1) {
                assert_ne!(a, b, "{a:?} is listed twice");
            }
        }
    }
}
