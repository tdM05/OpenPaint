//! Where a colour sits on a colour wheel, and which colour sits at a point — with nothing on a
//! screen in it.
//!
//! A colour wheel is one of the three things `panel_ui` names as undescribable: it is a drawing,
//! not a list of controls. But only its *pixels* are a drawing. Which region a press landed in,
//! what colour that press means, and where the marker for the current colour goes are arithmetic,
//! and arithmetic can be proved right without a window — the same split that made `crop`,
//! `transform_box` and `panel_ui` provable.
//!
//! # The marker and the press are one definition
//!
//! [`Wheel::marker`] says where to draw the current colour's marker; [`Wheel::colour_at`] says
//! what a press somewhere means. They are inverses, and they are written next to each other for
//! the same reason `panel_ui::to_fraction` and `from_fraction` are: a marker drawn in one place
//! while a press there sets a different colour does not read as "slightly off", it reads as the
//! control being broken. Tested against each other over every shape rather than against numbers.
//!
//! # HSV is over sRGB-encoded values, deliberately
//!
//! Not linear ones. HSV as every painter has ever met it — Photoshop's, CSP's, Procreate's — is
//! defined on the gamma-encoded channels, and that is what makes "value 50%" land where the eye
//! expects rather than at the 22% grey that half of *linear* is (`openpaint_core::color`'s own
//! `mid_grey_is_not_half_in_linear` is the same fact from the other side). So this module speaks
//! `[u8; 3]` sRGB, which is what the app stores a colour as, and the linear conversion stays where
//! it already lives: `openpaint_core::color::opaque_srgb8_to_linear_premul`, at the boundary where
//! an authored colour enters the engine. Deliberately not re-exported here — a second route to
//! linear is the recurring hazard of one definition kept in two places (§11a.8).
//!
//! This is §1a's split showing up in the UI: CSP's *interface* conventions, our engine's numbers.
//!
//! # The picker's state is an [`Hsv`], not an `[u8; 3]`
//!
//! Worth stating because getting it wrong is a bug every colour picker has shipped at least once.
//! Grey has no hue and black has no saturation, so a wheel that stores only the sRGB triple has to
//! invent one when the artist drags the value to zero and back — and the hue ring's marker jumps
//! to red under their hand. Holding the `Hsv` remembers what the triple cannot, and the triple is
//! derived from it whenever the engine needs a colour. Everything here therefore takes and returns
//! [`Hsv`]; [`Hsv::to_srgb8`] is called at the edge.

// Nothing in the binary draws a colour wheel yet, and in a *binary* crate `pub` does not exempt an
// item from `dead_code` — so without this every item here is a denied warning and the workspace
// does not build. Scoped to this module, and **delete it the moment the colour panel calls in**:
// an allow left behind after it stops being needed is how a module quietly stops being told about
// code nothing reaches, which is the same silence §6b is about one level down.
#![allow(dead_code)]

use crate::layout::Rect;
use serde::{Deserialize, Serialize};

/// The hue ring's thickness, as a fraction of the wheel's outer radius.
///
/// Chosen against millimetres rather than against how it looked once, the way `theme` chooses its
/// grab surfaces: a colour panel around 200 logical units wide gives a 100-unit radius and so an
/// 18-unit ring, a little under 5 mm on any display at any scaling. That clears the 4 mm floor the
/// theme holds its other targets to, with the pen's precision as the margin.
const RING_THICKNESS: f32 = 0.18;

/// Clear space between the ring's inner edge and the shape inside it, as a fraction of the outer
/// radius.
///
/// Not decoration. The ring and the interior are two different controls sitting a few units apart,
/// and a press landing in the gap belongs to neither — which is better than a press near the edge
/// of one silently arriving at the other.
const INTERIOR_GAP: f32 = 0.06;

/// The hue strip's thickness for [`Shape::Square`], as a fraction of the square's side.
const STRIP_THICKNESS: f32 = 0.14;

/// The gap between the square and its hue strip, as a fraction of the square's side.
const STRIP_GAP: f32 = 0.06;

/// How far outside the triangle a point may be and still count as inside it, in barycentric
/// weight.
///
/// The triangle's version of [`within`] counting all four edges of a rectangle, and it exists for
/// the same reason: **every one of the three edges is a colour somebody picks.** Full value is the
/// hue-to-white edge, full saturation the hue-to-black edge, no saturation the white-to-black one,
/// and the top edge in particular is where bright colours come from. Testing the weights for
/// exactly non-negative rejects all of them, because the marker's position is computed *from* the
/// weights and comes back a rounding error the wrong side — so the most-used edge of the triangle
/// is the one place the artist cannot press. Found that way, by the marker-and-press test.
///
/// The weights are normalised, so this is a fraction of the triangle's own size rather than a
/// distance: about a hundredth of a unit on a 200-unit panel, and it scales with the panel.
const EDGE_TOLERANCE: f32 = 1e-4;

/// A colour as the artist thinks about it: a hue, how much of it, and how bright.
///
/// Fields are private and [`Hsv::new`] is the only way in, because unlike [`Rect`] this type has
/// an invariant — hue wraps and the other two are fractions — and **one place holds it**, exactly
/// as `panel_ui::from_fraction` is the one place a slider's range is enforced. A public `h` would
/// mean every caller doing arithmetic on a hue had to remember `rem_euclid`, and the one that
/// forgot would produce a marker somewhere off the ring entirely.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsv {
    h: f32,
    s: f32,
    v: f32,
}

impl Hsv {
    /// A colour from a hue in degrees and a saturation and value in `0..=1`.
    ///
    /// The hue wraps rather than clamping — 370 is 10 and −30 is 330, because a hue is an angle
    /// and dragging round the ring must not stick at the seam. Saturation and value clamp, because
    /// they are not angles. A non-finite input becomes zero rather than propagating: a NaN hue
    /// silently poisons every comparison downstream, and a marker at NaN is simply not drawn,
    /// which is the silent failure §6b exists to rule out.
    #[must_use]
    pub fn new(h: f32, s: f32, v: f32) -> Self {
        let finite = |x: f32| if x.is_finite() { x } else { 0.0 };
        Self {
            h: finite(h).rem_euclid(360.0),
            s: finite(s).clamp(0.0, 1.0),
            v: finite(v).clamp(0.0, 1.0),
        }
    }

    /// Hue in degrees, always in `0..360`.
    ///
    /// Zero for a grey, which has no hue at all — see [`Hsv::is_achromatic`] before reading
    /// anything into it.
    #[must_use]
    pub fn hue(self) -> f32 {
        self.h
    }

    /// Saturation, `0..=1`.
    #[must_use]
    pub fn saturation(self) -> f32 {
        self.s
    }

    /// Value, `0..=1`.
    #[must_use]
    pub fn value(self) -> f32 {
        self.v
    }

    /// Whether this colour has no hue to speak of: a grey, black, or white.
    ///
    /// The hue of such a colour is not zero, it is *undefined*, and the two are only the same
    /// number by convention. Anything that would show the artist a hue — a readout, a "keep the
    /// hue" decision — should ask this first.
    #[must_use]
    pub fn is_achromatic(self) -> bool {
        self.s <= 0.0 || self.v <= 0.0
    }

    /// The same colour with a different hue, leaving how much of it and how bright alone.
    #[must_use]
    pub fn with_hue(self, h: f32) -> Self {
        Self::new(h, self.s, self.v)
    }

    /// The same colour at a different saturation and value, keeping the hue.
    ///
    /// The hue survives even when the result is a grey, which is the whole reason the picker holds
    /// an [`Hsv`]: drag the value to black and back and the ring's marker has not moved.
    #[must_use]
    pub fn with_saturation_value(self, s: f32, v: f32) -> Self {
        Self::new(self.h, s, v)
    }

    /// Read a colour the app stored as an sRGB triple.
    ///
    /// The achromatic case is answered before the hue is computed rather than after, because with
    /// `max == min` the hue is a `0/0` and a reader should see the answer here instead of deducing
    /// it from a NaN two functions away. It is honestly a second line of defence and not the only
    /// one — [`Hsv::new`] turns that NaN into the same zero, which a sabotage sweep confirmed by
    /// removing this branch and finding nothing changed. Kept for the reader; the guard that would
    /// actually be missed is `new`'s.
    #[must_use]
    pub fn from_srgb8(rgb: [u8; 3]) -> Self {
        let [r, g, b] = [
            f32::from(rgb[0]) / 255.0,
            f32::from(rgb[1]) / 255.0,
            f32::from(rgb[2]) / 255.0,
        ];
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let chroma = max - min;
        if chroma <= 0.0 {
            return Self::new(0.0, 0.0, max);
        }
        // Left unwrapped on purpose: the magenta sector comes out negative here and `new` is what
        // brings it back onto the ring. Wrapping in both places would be two guards over one rule.
        let h = 60.0
            * if max == r {
                (g - b) / chroma
            } else if max == g {
                (b - r) / chroma + 2.0
            } else {
                (r - g) / chroma + 4.0
            };
        Self::new(h, chroma / max, max)
    }

    /// The sRGB triple this colour is, for storing and for handing to the engine.
    ///
    /// Exact against [`Hsv::from_srgb8`]: every one of the sixteen million triples survives the
    /// round trip unchanged, which is what stops a colour drifting a shade each time the artist
    /// nudges a wheel.
    #[must_use]
    pub fn to_srgb8(self) -> [u8; 3] {
        let chroma = self.v * self.s;
        let sector = self.h / 60.0;
        let x = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
        let (r, g, b) = match sector as u32 {
            0 => (chroma, x, 0.0),
            1 => (x, chroma, 0.0),
            2 => (0.0, chroma, x),
            3 => (0.0, x, chroma),
            4 => (x, 0.0, chroma),
            // `sector` is below 6 because `new` wrapped the hue, so this is the last sixth and not
            // a catch-all standing in for a case nobody thought about.
            _ => (chroma, 0.0, x),
        };
        let base = self.v - chroma;
        let byte = |c: f32| ((c + base).clamp(0.0, 1.0) * 255.0).round() as u8;
        [byte(r), byte(g), byte(b)]
    }
}

/// Which arrangement of a colour wheel the artist has asked for.
///
/// A panel setting, the way `panel_ui::Direction` is: every one of these is somebody's favourite
/// and none of them is right, so the wheel carries the choice rather than the code carrying a
/// winner. Nothing outside this module branches on it — the caller asks for a region and a marker
/// and gets the same answers whichever shape is set, which is §1c's rule one level down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    /// A hue ring with a saturation/value square inside it. CSP's default, and ours.
    #[default]
    Ring,
    /// A hue ring with a saturation/value triangle inside it, turning with the hue.
    ///
    /// The triangle spends its whole area on colours that exist, where the square's top-left
    /// corner is a white the top edge already reached. The price is that it rotates under the
    /// hand, so a remembered position stops meaning a remembered colour.
    Triangle,
    /// A saturation/value square with the hue on a separate strip.
    ///
    /// No ring at all. The plainest of the three and the one that survives a narrow panel, since
    /// nothing in it has to stay circular.
    Square,
}

/// Which of a wheel's two controls a point belongs to.
///
/// Named for what it *sets* rather than for what it looks like, so a caller never has to know that
/// hue is a ring in two shapes and a strip in the third.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// The hue ring, or the hue strip.
    Hue,
    /// The saturation/value square or triangle.
    Interior,
}

/// The hue ring's circle, for whatever draws it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    pub centre: (f32, f32),
    /// Radius of the ring's inner edge.
    pub inner: f32,
    /// Radius of the ring's outer edge.
    pub outer: f32,
}

/// Where each part of a wheel sits, worked out once from the bounding rectangle.
///
/// An enum rather than a struct of `Option`s so that "a ring shape has no hue strip" is a fact
/// about the shape instead of a `None` any code path might forget to handle.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Parts {
    /// Nothing fits, so nothing is drawable and no point is in any region.
    ///
    /// A panel dragged to a sliver, a rectangle not laid out yet, or an infinity arriving from a
    /// division somewhere upstream. Answering `None` everywhere is the honest result; the
    /// alternative is a ring of radius zero where every press reports a hue read off a `0/0`.
    Empty,
    Ring {
        ring: Ring,
        square: Rect,
    },
    Triangle {
        ring: Ring,
        /// The pure-hue, white and black corners, in that order.
        corners: [(f32, f32); 3],
    },
    Strip {
        square: Rect,
        strip: Rect,
    },
}

/// A colour wheel of some shape, in some rectangle, currently showing some colour.
///
/// Holds the colour as well as the geometry because two of the three answers need it: the triangle
/// turns with the hue, and pressing the hue ring has to keep the saturation and value it is not
/// setting. Keeping one copy is the point — a marker drawn from one colour and a press resolved
/// against another is precisely how the two drift apart.
#[derive(Clone, Copy, Debug)]
pub struct Wheel {
    colour: Hsv,
    parts: Parts,
}

impl Wheel {
    /// Fit a wheel of `shape` into `bounds`, showing `colour`.
    ///
    /// The round shapes take the smaller of the two dimensions and sit centred, so a wheel in a
    /// panel wider than it is tall is a circle in the middle rather than an ellipse.
    ///
    /// The shape is not kept afterwards and there is no accessor for it: the caller passed it in
    /// and already knows, and a second copy of an answer is a second thing that can disagree.
    #[must_use]
    pub fn new(shape: Shape, bounds: Rect, colour: Hsv) -> Self {
        let parts = Self::fit(shape, bounds, colour);
        Self { colour, parts }
    }

    fn fit(shape: Shape, bounds: Rect, colour: Hsv) -> Parts {
        // Every component checked, not just the smaller side: `f32::min` **discards** a NaN and
        // returns the other operand, so `w.min(h)` on a NaN width is a perfectly finite number and
        // the rectangle sails through. Caught by the degenerate-rectangle test, which is exactly
        // the sort of thing that would otherwise surface as a wheel whose every press was a
        // different random colour.
        if ![bounds.x, bounds.y, bounds.w, bounds.h]
            .iter()
            .all(|c| c.is_finite())
        {
            return Parts::Empty;
        }
        let field = bounds.w.min(bounds.h);
        if field <= 0.0 {
            return Parts::Empty;
        }
        match shape {
            Shape::Ring | Shape::Triangle => {
                let outer = field * 0.5;
                let ring = Ring {
                    centre: (bounds.x + bounds.w * 0.5, bounds.y + bounds.h * 0.5),
                    inner: outer * (1.0 - RING_THICKNESS),
                    outer,
                };
                let radius = outer * (1.0 - RING_THICKNESS - INTERIOR_GAP);
                if shape == Shape::Ring {
                    // The largest square inside the circle, so the whole square is clear of the
                    // ring rather than clipped by it.
                    let side = radius * std::f32::consts::SQRT_2;
                    Parts::Ring {
                        ring,
                        square: Rect::new(
                            ring.centre.0 - side * 0.5,
                            ring.centre.1 - side * 0.5,
                            side,
                            side,
                        ),
                    }
                } else {
                    Parts::Triangle {
                        ring,
                        corners: triangle_corners(ring.centre, radius, colour.hue()),
                    }
                }
            }
            Shape::Square => {
                // Solve for the side that makes the square *and* the strip below it fit, rather
                // than taking the smaller dimension and letting the strip hang off the bottom.
                let side = bounds.w.min(bounds.h / (1.0 + STRIP_GAP + STRIP_THICKNESS));
                let total = side * (1.0 + STRIP_GAP + STRIP_THICKNESS);
                let x = bounds.x + (bounds.w - side) * 0.5;
                let y = bounds.y + (bounds.h - total) * 0.5;
                Parts::Strip {
                    square: Rect::new(x, y, side, side),
                    strip: Rect::new(
                        x,
                        y + side * (1.0 + STRIP_GAP),
                        side,
                        side * STRIP_THICKNESS,
                    ),
                }
            }
        }
    }

    /// Whether the rectangle had no room in it, so there is nothing to draw and nothing to press.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.parts == Parts::Empty
    }

    /// The hue ring, for the shapes that have one.
    #[must_use]
    pub fn hue_ring(self) -> Option<Ring> {
        match self.parts {
            Parts::Ring { ring, .. } | Parts::Triangle { ring, .. } => Some(ring),
            Parts::Strip { .. } | Parts::Empty => None,
        }
    }

    /// The hue strip, for the shape that has one.
    #[must_use]
    pub fn hue_strip(self) -> Option<Rect> {
        match self.parts {
            Parts::Strip { strip, .. } => Some(strip),
            Parts::Ring { .. } | Parts::Triangle { .. } | Parts::Empty => None,
        }
    }

    /// The saturation/value square, for the shapes that have one.
    #[must_use]
    pub fn sv_square(self) -> Option<Rect> {
        match self.parts {
            Parts::Ring { square, .. } | Parts::Strip { square, .. } => Some(square),
            Parts::Triangle { .. } | Parts::Empty => None,
        }
    }

    /// The saturation/value triangle's corners — pure hue, white, black — for the shape that has
    /// one.
    ///
    /// They turn with the hue, so this is only valid for the colour the wheel was built with.
    #[must_use]
    pub fn sv_triangle(self) -> Option<[(f32, f32); 3]> {
        match self.parts {
            Parts::Triangle { corners, .. } => Some(corners),
            Parts::Ring { .. } | Parts::Strip { .. } | Parts::Empty => None,
        }
    }

    /// Which control a point is in, if any.
    ///
    /// `None` for the gap between the ring and the shape inside it, for the corners a circle
    /// leaves over, and for everything outside — a press there belongs to the panel, not to the
    /// wheel.
    #[must_use]
    pub fn region_at(self, x: f32, y: f32) -> Option<Region> {
        match self.parts {
            Parts::Empty => None,
            Parts::Ring { ring, square } => {
                if on_ring(ring, x, y) {
                    Some(Region::Hue)
                } else if within(square, x, y) {
                    Some(Region::Interior)
                } else {
                    None
                }
            }
            Parts::Triangle { ring, corners } => {
                if on_ring(ring, x, y) {
                    Some(Region::Hue)
                } else if barycentric(corners, x, y)
                    .iter()
                    .all(|w| *w >= -EDGE_TOLERANCE)
                {
                    Some(Region::Interior)
                } else {
                    None
                }
            }
            Parts::Strip { square, strip } => {
                if within(strip, x, y) {
                    Some(Region::Hue)
                } else if within(square, x, y) {
                    Some(Region::Interior)
                } else {
                    None
                }
            }
        }
    }

    /// The colour a press at a point would set, or `None` if the point is not on the wheel.
    ///
    /// What a *press* means. A drag that has already grabbed a region wants [`Wheel::colour_in`]
    /// instead, so that running off the edge pins the value rather than dropping the gesture.
    #[must_use]
    pub fn colour_at(self, x: f32, y: f32) -> Option<Hsv> {
        self.region_at(x, y).map(|r| self.colour_in(r, x, y))
    }

    /// The colour a point means to a region that has already been grabbed.
    ///
    /// **The one place a wheel's coordinates are held inside their region**, the way
    /// `panel_ui::from_fraction` is for a slider's range. A hand dragging the saturation marker
    /// does not stop politely at the edge of the square, and a gesture that gave up the moment it
    /// left would feel like the control letting go.
    #[must_use]
    pub fn colour_in(self, region: Region, x: f32, y: f32) -> Hsv {
        match (region, self.parts) {
            (_, Parts::Empty) => self.colour,
            (Region::Hue, Parts::Ring { ring, .. } | Parts::Triangle { ring, .. }) => {
                match hue_at(ring.centre, x, y) {
                    Some(h) => self.colour.with_hue(h),
                    // Dead centre, where the angle is a `0/0`. Keeping the hue is the only answer
                    // that is not invented, and it means a drag through the middle of the wheel
                    // comes out the far side rather than snapping to red on the way past.
                    None => self.colour,
                }
            }
            (Region::Hue, Parts::Strip { strip, .. }) => {
                let t = fraction(x - strip.x, strip.w);
                self.colour.with_hue(t * 360.0)
            }
            (Region::Interior, Parts::Ring { square, .. } | Parts::Strip { square, .. }) => {
                let s = fraction(x - square.x, square.w);
                // Value up the square, not down it: full brightness at the top is what every
                // picker does, and inverting it is the axis swap this reads as broken.
                let v = 1.0 - fraction(y - square.y, square.h);
                self.colour.with_saturation_value(s, v)
            }
            (Region::Interior, Parts::Triangle { corners, .. }) => {
                let [hue_weight, white_weight, _] = barycentric(corners, x, y);
                // Mixing the pure hue, white and black in these proportions gives a colour whose
                // maximum channel is (hue + white) and whose minimum is (white) — so value and
                // saturation fall straight out, and the inverse in `marker` is one line of
                // algebra rather than a search.
                let v = (hue_weight + white_weight).clamp(0.0, 1.0);
                let s = if v > 0.0 {
                    (hue_weight / v).clamp(0.0, 1.0)
                } else {
                    // Black: the three corners have collapsed to one point and saturation means
                    // nothing there. Keeping it stops the marker jumping when a drag along the
                    // bottom edge touches black.
                    self.colour.saturation()
                };
                self.colour.with_saturation_value(s, v)
            }
        }
    }

    /// Where to draw the marker for the current colour in a region.
    ///
    /// The inverse of [`Wheel::colour_in`], and the reason both are in this file: pressing where
    /// this puts the marker gives back the colour the marker is for. Anything else and the artist
    /// watches the marker jump away from their pen the instant they touch it.
    #[must_use]
    pub fn marker(self, region: Region) -> Option<(f32, f32)> {
        match (region, self.parts) {
            (_, Parts::Empty) => None,
            (Region::Hue, Parts::Ring { ring, .. } | Parts::Triangle { ring, .. }) => {
                // Halfway across the ring, which is both where it looks right and the radius
                // furthest from either edge — so the marker is never sitting on a boundary a
                // press could fall the wrong side of.
                Some(point_on(
                    ring.centre,
                    (ring.inner + ring.outer) * 0.5,
                    self.colour.hue(),
                ))
            }
            (Region::Hue, Parts::Strip { strip, .. }) => Some((
                strip.x + strip.w * self.colour.hue() / 360.0,
                strip.y + strip.h * 0.5,
            )),
            (Region::Interior, Parts::Ring { square, .. } | Parts::Strip { square, .. }) => Some((
                square.x + square.w * self.colour.saturation(),
                square.y + square.h * (1.0 - self.colour.value()),
            )),
            (Region::Interior, Parts::Triangle { corners, .. }) => {
                let (s, v) = (self.colour.saturation(), self.colour.value());
                Some(mix(corners, [s * v, v - s * v, 1.0 - v]))
            }
        }
    }
}

/// Where a hue sits on the ring, as an angle clockwise from straight up.
///
/// Red at twelve o'clock turning clockwise, which is CSP's wheel and Photoshop's. The screen's `y`
/// grows downward, so "up" is `-y` and clockwise is the direction `atan2` already runs in when its
/// arguments are given this way round — the convention costs a comment here and no sign juggling
/// anywhere else.
fn point_on(centre: (f32, f32), radius: f32, hue: f32) -> (f32, f32) {
    let (sin, cos) = hue.to_radians().sin_cos();
    (
        radius.mul_add(sin, centre.0),
        radius.mul_add(-cos, centre.1),
    )
}

/// The hue a point sits at, or `None` at the exact centre where there is no angle to read.
fn hue_at(centre: (f32, f32), x: f32, y: f32) -> Option<f32> {
    let (dx, dy) = (x - centre.0, y - centre.1);
    if dx == 0.0 && dy == 0.0 {
        return None;
    }
    Some(dx.atan2(-dy).to_degrees().rem_euclid(360.0))
}

/// Whether a point is on the ring rather than inside or outside it.
///
/// Inclusive at both radii, which is what makes it an annulus with no seam; the disc it would be
/// without the inner test is the bug worth naming, since it swallows the entire interior.
fn on_ring(ring: Ring, x: f32, y: f32) -> bool {
    let (dx, dy) = (x - ring.centre.0, y - ring.centre.1);
    let d2 = dx.mul_add(dx, dy * dy);
    d2 >= ring.inner * ring.inner && d2 <= ring.outer * ring.outer
}

/// Whether a point is in a rectangle, counting all four edges as inside.
///
/// Deliberately not [`Rect::contains`], which is half-open so that adjacent panels cannot both
/// claim the boundary between them. Nothing abuts the square or the strip, and full saturation
/// lives exactly on the square's right edge — so half-open here would make the most saturated
/// colour on the wheel the one colour that cannot be pressed.
fn within(rect: Rect, x: f32, y: f32) -> bool {
    x >= rect.x && x <= rect.x + rect.w && y >= rect.y && y <= rect.y + rect.h
}

/// How far along a span a distance is, held inside `0..=1`, with a zero-width span reading as the
/// start rather than as a division by zero.
///
/// **The clamp is load-bearing for the hue strip and only for it**, which is worth knowing because
/// it looks redundant: everywhere else the fraction ends up in [`Hsv::new`], which clamps
/// saturation and value anyway. A hue does not clamp, it *wraps* — so without this, a drag off the
/// left end of the strip becomes a negative hue and `rem_euclid` brings it back round as cyan.
/// Dragging past red arrives at cyan is not a rounding error, it is the control going somewhere
/// nobody asked for, and it was invisible until a test asked where a drag off the end *lands*
/// rather than only whether it stayed in range.
fn fraction(along: f32, span: f32) -> f32 {
    if span > 0.0 {
        (along / span).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The triangle's pure-hue, white and black corners, in that order.
///
/// The hue corner points at its own place on the ring, so the triangle and the ring always agree
/// about where a hue is; the other two follow at a third of a turn each.
fn triangle_corners(centre: (f32, f32), radius: f32, hue: f32) -> [(f32, f32); 3] {
    [
        point_on(centre, radius, hue),
        point_on(centre, radius, hue + 120.0),
        point_on(centre, radius, hue + 240.0),
    ]
}

/// A point's weights on the three corners: how much of each corner it is made of.
///
/// All three non-negative exactly when the point is inside the triangle, which is what makes this
/// serve as the hit test as well as the colour, so there are not two pieces of geometry that have
/// to agree about where the triangle is.
fn barycentric(corners: [(f32, f32); 3], x: f32, y: f32) -> [f32; 3] {
    let [a, b, c] = corners;
    let (v0x, v0y) = (b.0 - a.0, b.1 - a.1);
    let (v1x, v1y) = (c.0 - a.0, c.1 - a.1);
    let (v2x, v2y) = (x - a.0, y - a.1);
    let denominator = v0x.mul_add(v1y, -(v1x * v0y));
    if denominator == 0.0 {
        // Only reachable if the three corners are collinear, which a positive radius rules out.
        // Answering "at the first corner" keeps a NaN out of the colour either way.
        return [1.0, 0.0, 0.0];
    }
    let beta = v2x.mul_add(v1y, -(v1x * v2y)) / denominator;
    let gamma = v0x.mul_add(v2y, -(v2x * v0y)) / denominator;
    [1.0 - beta - gamma, beta, gamma]
}

/// The point made of the corners in those proportions — the inverse of [`barycentric`].
fn mix(corners: [(f32, f32); 3], weights: [f32; 3]) -> (f32, f32) {
    let mut point = (0.0, 0.0);
    for (corner, weight) in corners.iter().zip(weights) {
        point.0 = corner.0.mul_add(weight, point.0);
        point.1 = corner.1.mul_add(weight, point.1);
    }
    point
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPES: [Shape; 3] = [Shape::Ring, Shape::Triangle, Shape::Square];

    fn bounds() -> Rect {
        Rect::new(10.0, 20.0, 200.0, 200.0)
    }

    /// Colours spread over the whole space, avoiding the two places where a coordinate stops
    /// meaning anything: a grey has no hue and black has no saturation, so a round trip through
    /// either would pass whatever the geometry did. Both have their own tests instead.
    fn sample_colours() -> Vec<Hsv> {
        let mut out = Vec::new();
        for hue in (0u16..360).step_by(23) {
            for s in 1..=5u8 {
                for v in 1..=5u8 {
                    out.push(Hsv::new(
                        f32::from(hue),
                        f32::from(s) / 5.0,
                        f32::from(v) / 5.0,
                    ));
                }
            }
        }
        out
    }

    /// The middle of whatever the wheel occupies, for firing test rays from.
    fn centre_of(wheel: Wheel) -> (f32, f32) {
        wheel.hue_ring().map_or_else(
            || {
                let square = wheel.sv_square().expect("a wheel with room has a square");
                (square.x + square.w * 0.5, square.y + square.h * 0.5)
            },
            |ring| ring.centre,
        )
    }

    /// **A colour must survive being looked at.** A triple goes to HSV and back unchanged, so
    /// nudging a wheel cannot walk a colour a shade at a time away from where the artist put it.
    ///
    /// Strided rather than exhaustive so it stays a fast test; the stride is odd and so does not
    /// line up with any sector boundary, and every sector of the hue circle is covered many times.
    #[test]
    fn every_srgb_colour_survives_a_round_trip_through_hsv() {
        for r in (0..=255u8).step_by(5) {
            for g in (0..=255u8).step_by(5) {
                for b in (0..=255u8).step_by(5) {
                    let rgb = [r, g, b];
                    let back = Hsv::from_srgb8(rgb).to_srgb8();
                    assert_eq!(back, rgb, "{rgb:?} came back as {back:?}");
                }
            }
        }
    }

    /// **A grey must not acquire a hue.** With `max == min` the hue formula is a `0/0`, and the
    /// version of it that does not check hands back a NaN — which propagates into a marker
    /// position that is then simply never drawn, the silent failure §6b rules out.
    #[test]
    fn greys_stay_grey_and_have_no_hue() {
        for level in 0..=255u8 {
            let grey = [level, level, level];
            let hsv = Hsv::from_srgb8(grey);
            assert!(
                hsv.hue().is_finite(),
                "grey {level} produced hue {}",
                hsv.hue()
            );
            assert!(
                hsv.saturation() <= 0.0,
                "grey {level} came out saturated at {}",
                hsv.saturation()
            );
            assert!(hsv.is_achromatic(), "grey {level} claims a hue");
            assert_eq!(hsv.to_srgb8(), grey, "grey {level} did not come back grey");
        }
    }

    /// A hue is an angle, so it wraps rather than clamping — otherwise dragging round the ring
    /// would stick at the seam and every hue past 360 would pile up on red.
    #[test]
    fn a_hue_wraps_instead_of_clamping() {
        for (given, expected) in [
            (370.0, 10.0),
            (-30.0, 330.0),
            (720.0, 0.0),
            (-360.0, 0.0),
            (359.5, 359.5),
        ] {
            let h = Hsv::new(given, 1.0, 1.0).hue();
            assert!(
                (h - expected).abs() < 0.001,
                "{given} became {h}, not {expected}"
            );
        }
        // And the part that would be visible: 370 and 10 must be one colour, not two.
        assert_eq!(
            Hsv::new(370.0, 0.8, 0.9).to_srgb8(),
            Hsv::new(10.0, 0.8, 0.9).to_srgb8()
        );
    }

    /// Saturation and value are fractions rather than angles, so they clamp. A drag off the edge
    /// of the square arrives here well outside the range.
    #[test]
    fn saturation_and_value_are_held_inside_their_range() {
        for x in [-9.0, -0.001, 1.001, 40.0, f32::INFINITY, f32::NEG_INFINITY] {
            let c = Hsv::new(120.0, x, x);
            assert!(
                (0.0..=1.0).contains(&c.saturation()) && (0.0..=1.0).contains(&c.value()),
                "{x} escaped to s={} v={}",
                c.saturation(),
                c.value()
            );
        }
        // A NaN must not survive either: it fails every comparison, so a clamp alone lets it past.
        let poisoned = Hsv::new(f32::NAN, f32::NAN, f32::NAN);
        assert!(poisoned.hue().is_finite() && poisoned.saturation().is_finite());
    }

    /// **The marker and the press agree.** Pressing exactly where the marker is drawn gives back
    /// the colour the marker was drawn for — every shape, both regions.
    ///
    /// Asked through [`Wheel::colour_at`] rather than `colour_in`, so it also pins that the marker
    /// lands *inside* the region it belongs to. The two can genuinely disagree: a half-open hit
    /// test puts full saturation one unit outside the square whose right edge it is.
    ///
    /// The colour is checked twice over. The components are the strict half, to a ten-thousandth,
    /// because that is the state the picker actually keeps. The sRGB triple is the loose half, to
    /// one step in 255, and the tolerance there is not slack for a sloppy implementation: a
    /// continuous control read at one point and then rounded to eight bits has colours sitting
    /// exactly on a rounding boundary — hue 46 lands on 195.5 in green — where an angle recovered
    /// a millionth of a degree light falls the other side of `.round()`. That is quantisation, not
    /// drift, and it does not accumulate: pressing the same marker again gives the same answer.
    #[test]
    fn pressing_where_the_marker_is_drawn_gives_back_its_colour() {
        for shape in SHAPES {
            for colour in sample_colours() {
                let wheel = Wheel::new(shape, bounds(), colour);
                for region in [Region::Hue, Region::Interior] {
                    let (x, y) = wheel.marker(region).expect("a wheel with room has markers");
                    let got = wheel.colour_at(x, y).unwrap_or_else(|| {
                        panic!(
                            "{shape:?} {region:?}: the marker for {colour:?} is drawn outside its \
                             own region, at ({x}, {y})"
                        )
                    });
                    let (want, have) = (colour.to_srgb8(), got.to_srgb8());
                    assert!(
                        want.iter().zip(have).all(|(a, b)| a.abs_diff(b) <= 1),
                        "{shape:?} {region:?}: the marker for {colour:?} ({want:?}) is at \
                         ({x}, {y}), where a press means {got:?} ({have:?})"
                    );
                    assert!(
                        (got.hue() - colour.hue()).abs() < 0.0001,
                        "{shape:?} {region:?}: hue {} became {}",
                        colour.hue(),
                        got.hue()
                    );
                    if region == Region::Interior {
                        assert!(
                            (got.saturation() - colour.saturation()).abs() < 0.0001
                                && (got.value() - colour.value()).abs() < 0.0001,
                            "{shape:?}: s/v {:?} became {:?}",
                            (colour.saturation(), colour.value()),
                            (got.saturation(), got.value())
                        );
                    }
                }
            }
        }
    }

    /// **The wheel must not move under the pen.** A drag is a run of presses at moving points, and
    /// each one rebuilds the wheel around the colour the last one produced. If pressing somewhere
    /// changed what that same place means, a held pen would walk the colour away on its own — and
    /// the triangle, which really does turn with the hue, is exactly where that could happen.
    ///
    /// Rays out from the middle, sampled across every radius, so this covers points nowhere near a
    /// marker — which the test above, seeded only from marker positions, cannot reach.
    #[test]
    fn pressing_the_same_place_twice_means_the_same_colour() {
        for shape in SHAPES {
            let wheel = Wheel::new(shape, bounds(), Hsv::new(200.0, 0.5, 0.5));
            let centre = centre_of(wheel);
            for step in 0..40u8 {
                let angle = f32::from(step) * 9.0;
                for radius in [8.0, 30.0, 55.0, 70.0, 82.0, 92.0, 99.0] {
                    let (x, y) = point_on(centre, radius, angle);
                    let Some(region) = wheel.region_at(x, y) else {
                        continue;
                    };
                    let picked = wheel.colour_at(x, y).expect("a region has a colour");
                    let after = Wheel::new(shape, bounds(), picked);
                    assert_eq!(
                        after.region_at(x, y),
                        Some(region),
                        "{shape:?}: ({x}, {y}) was {region:?} and moved under the press"
                    );
                    assert_eq!(
                        after.colour_at(x, y).map(Hsv::to_srgb8),
                        Some(picked.to_srgb8()),
                        "{shape:?} {region:?}: pressing ({x}, {y}) twice gave two colours"
                    );
                    // And the marker for what was picked comes to rest where the press was —
                    // compared along the axis the control actually reads, because a hue control is
                    // one-dimensional and its other axis carries nothing. How far out on the ring
                    // you pressed, or how high up the strip, is not something a hue can remember,
                    // so the marker sits at the middle of the band by convention and comparing
                    // whole points there would be asserting the convention rather than the map.
                    let (mx, my) = after.marker(region).expect("a marker");
                    match (region, after.hue_ring()) {
                        (Region::Hue, Some(_)) => {
                            let bearing = |p: (f32, f32)| (p.0 - centre.0).atan2(-(p.1 - centre.1));
                            assert!(
                                (bearing((mx, my)) - bearing((x, y))).abs() < 0.001,
                                "{shape:?}: pressed at bearing {} and the marker went to {}",
                                bearing((x, y)),
                                bearing((mx, my))
                            );
                        }
                        (Region::Hue, None) => assert!(
                            (mx - x).abs() < 0.01,
                            "{shape:?}: pressed at x {x} on the strip, marker went to {mx}"
                        ),
                        (Region::Interior, _) => assert!(
                            (mx - x).hypot(my - y) < 0.01,
                            "{shape:?}: pressed at ({x}, {y}), marker went to ({mx}, {my})"
                        ),
                    }
                }
            }
        }
    }

    /// **A hue ring is an annulus, not a disc.** Dropping the inner test is the plausible mistake
    /// and a bad one: the ring then swallows the whole interior, so the saturation/value control
    /// cannot be reached at all.
    #[test]
    fn the_hue_ring_is_a_ring_and_not_a_disc() {
        for shape in [Shape::Ring, Shape::Triangle] {
            let wheel = Wheel::new(shape, bounds(), Hsv::new(30.0, 0.7, 0.7));
            let ring = wheel.hue_ring().expect("these shapes have a ring");
            let mid = (ring.inner + ring.outer) * 0.5;
            for angle in (0u16..360).step_by(11) {
                let at = |r: f32| {
                    let (x, y) = point_on(ring.centre, r, f32::from(angle));
                    wheel.region_at(x, y)
                };
                assert_eq!(
                    at(mid),
                    Some(Region::Hue),
                    "the middle of the ring at {angle}"
                );
                assert_ne!(
                    at(ring.inner * 0.5),
                    Some(Region::Hue),
                    "well inside the inner edge is not the ring, at {angle}"
                );
                assert_eq!(
                    at(ring.outer * 1.2),
                    None,
                    "outside the wheel entirely, at {angle}"
                );
            }
            assert_ne!(
                wheel.region_at(ring.centre.0, ring.centre.1),
                Some(Region::Hue),
                "{shape:?}: the centre is not on the ring"
            );
        }
    }

    /// The ring reports the hue that is drawn at that angle, and it goes all the way round. A ring
    /// built with degrees where radians were meant covers a sixtieth of the circle, and every
    /// press on it then reports nearly the same hue.
    #[test]
    fn the_ring_covers_every_hue_once_around() {
        let wheel = Wheel::new(Shape::Ring, bounds(), Hsv::new(0.0, 1.0, 1.0));
        let ring = wheel.hue_ring().expect("a ring");
        let mid = (ring.inner + ring.outer) * 0.5;
        let mut seen = Vec::new();
        for step in 0..36u8 {
            let hue = f32::from(step) * 10.0;
            let (x, y) = point_on(ring.centre, mid, hue);
            let got = wheel.colour_in(Region::Hue, x, y).hue();
            assert!(
                (got - hue).abs() < 0.5,
                "the point drawn at hue {hue} reads back as {got}"
            );
            seen.push(got);
        }
        for sixth in 0..6u8 {
            let lo = f32::from(sixth) * 60.0;
            assert!(
                seen.iter().any(|h| *h >= lo && *h < lo + 60.0),
                "no point on the ring lands in the hue sector starting at {lo}"
            );
        }
    }

    /// Red at twelve o'clock, turning clockwise. A convention rather than a truth, but one the
    /// drawing code and the hit test both have to hold — so this is where it is written down.
    #[test]
    fn the_ring_starts_at_the_top_and_turns_clockwise() {
        let wheel = Wheel::new(Shape::Ring, bounds(), Hsv::new(0.0, 1.0, 1.0));
        let ring = wheel.hue_ring().expect("a ring");
        let (x, y) = wheel.marker(Region::Hue).expect("a hue marker");
        assert!(
            (x - ring.centre.0).abs() < 0.001 && y < ring.centre.1,
            "red should be straight up, not at ({x}, {y})"
        );
        let quarter = Wheel::new(Shape::Ring, bounds(), Hsv::new(90.0, 1.0, 1.0));
        let (qx, qy) = quarter.marker(Region::Hue).expect("a hue marker");
        assert!(
            qx > ring.centre.0 && (qy - ring.centre.1).abs() < 0.001,
            "a quarter turn clockwise should be due right, not ({qx}, {qy})"
        );
    }

    /// Pressing the hue control changes the hue and nothing else. One that also moved saturation
    /// would wash the colour out as the artist turned it — the whole reason the wheel holds an
    /// `Hsv` rather than a triple.
    #[test]
    fn the_hue_control_sets_the_hue_and_leaves_the_rest_alone() {
        for shape in SHAPES {
            let start = Hsv::new(10.0, 0.3, 0.7);
            let wheel = Wheel::new(shape, bounds(), start);
            // Where hue 130 is drawn, asked of a wheel that is already showing it.
            let elsewhere = Wheel::new(shape, bounds(), start.with_hue(130.0));
            let (ex, ey) = elsewhere.marker(Region::Hue).expect("a hue marker");
            let picked = wheel.colour_in(Region::Hue, ex, ey);
            assert!(
                (picked.hue() - 130.0).abs() < 0.5,
                "{shape:?}: pressing where hue 130 is drawn gave {}",
                picked.hue()
            );
            assert!(
                (picked.saturation() - start.saturation()).abs() < 0.001
                    && (picked.value() - start.value()).abs() < 0.001,
                "{shape:?}: turning the hue also changed s/v to {:?}",
                (picked.saturation(), picked.value())
            );
        }
    }

    /// The interior's axes point the way every picker's do: saturation to the right, value up.
    /// Swapping either is invisible to a round-trip test — both directions still agree — so it
    /// takes its own check, against colours with names.
    #[test]
    fn the_interior_runs_saturated_right_and_bright_up() {
        for shape in [Shape::Ring, Shape::Square] {
            let wheel = Wheel::new(shape, bounds(), Hsv::new(200.0, 0.5, 0.5));
            let square = wheel.sv_square().expect("these shapes have a square");
            let corner = |fx: f32, fy: f32| {
                wheel.colour_in(
                    Region::Interior,
                    square.w.mul_add(fx, square.x),
                    square.h.mul_add(fy, square.y),
                )
            };
            let top_left = corner(0.0, 0.0);
            let top_right = corner(1.0, 0.0);
            let bottom_left = corner(0.0, 1.0);

            assert!(
                top_left.saturation() < 0.001,
                "{shape:?}: the left edge should be unsaturated"
            );
            assert!(
                top_right.saturation() > 0.999,
                "{shape:?}: the right edge should be fully saturated"
            );
            assert!(
                top_right.value() > 0.999,
                "{shape:?}: the top edge should be full value"
            );
            assert!(
                bottom_left.value() < 0.001,
                "{shape:?}: the bottom edge should be black"
            );
            assert_eq!(
                top_left.to_srgb8(),
                [255, 255, 255],
                "the top-left is white"
            );
            assert_eq!(
                bottom_left.to_srgb8(),
                [0, 0, 0],
                "the bottom-left is black"
            );
            assert_eq!(
                top_right.to_srgb8(),
                Hsv::new(200.0, 1.0, 1.0).to_srgb8(),
                "the top-right is the pure hue"
            );
        }
    }

    /// The triangle's three corners are the three colours it is made of, and its whole geometry
    /// follows from that — so getting the corners right is most of getting the triangle right.
    #[test]
    fn the_triangles_corners_are_the_pure_hue_white_and_black() {
        let hue = 275.0;
        let wheel = Wheel::new(Shape::Triangle, bounds(), Hsv::new(hue, 0.5, 0.5));
        let [pure, white, black] = wheel.sv_triangle().expect("a triangle");
        let at = |(x, y)| wheel.colour_in(Region::Interior, x, y).to_srgb8();

        assert_eq!(
            at(pure),
            Hsv::new(hue, 1.0, 1.0).to_srgb8(),
            "the hue corner"
        );
        assert_eq!(at(white), [255, 255, 255], "the white corner");
        assert_eq!(at(black), [0, 0, 0], "the black corner");

        // And the hue corner points at its own place on the ring, so the two halves of the wheel
        // agree about where a hue is.
        let ring = wheel.hue_ring().expect("a ring");
        let marker = wheel.marker(Region::Hue).expect("a hue marker");
        let bearing = |p: (f32, f32)| (p.0 - ring.centre.0).atan2(-(p.1 - ring.centre.1));
        assert!(
            (bearing(pure) - bearing(marker)).abs() < 0.01,
            "the hue corner points somewhere other than the hue on the ring"
        );
    }

    /// The triangle turns with the hue. If it did not, the ring's marker and the interior would
    /// disagree about which hue is being edited as soon as the artist turned the colour.
    #[test]
    fn the_triangle_turns_with_the_hue() {
        let square = Rect::new(0.0, 0.0, 200.0, 200.0);
        let a = Wheel::new(Shape::Triangle, square, Hsv::new(0.0, 1.0, 1.0));
        let b = Wheel::new(Shape::Triangle, square, Hsv::new(90.0, 1.0, 1.0));
        let [pure_a, ..] = a.sv_triangle().expect("a triangle");
        let [pure_b, ..] = b.sv_triangle().expect("a triangle");
        let moved = (pure_a.0 - pure_b.0).hypot(pure_a.1 - pure_b.1);
        assert!(
            moved > 10.0,
            "a quarter turn moved the hue corner by only {moved}"
        );
    }

    /// **A press between the two controls belongs to neither.** The gap is deliberate — they are
    /// separate controls a few units apart — and a hit test that let one of them claim it would
    /// change the colour along a different axis from the one the artist aimed at.
    #[test]
    fn the_gap_between_the_ring_and_the_interior_answers_to_nothing() {
        for shape in [Shape::Ring, Shape::Triangle] {
            let wheel = Wheel::new(shape, bounds(), Hsv::new(0.0, 1.0, 1.0));
            let ring = wheel.hue_ring().expect("a ring");
            let mut found = false;
            for angle in (0u16..360).step_by(7) {
                // Just inside the ring's inner edge, which for the square is a corner the circle
                // leaves over and for the triangle is most of the way round.
                let (x, y) = point_on(ring.centre, ring.inner * 0.99, f32::from(angle));
                if wheel.region_at(x, y).is_none() {
                    found = true;
                }
                assert_ne!(
                    wheel.region_at(x, y),
                    Some(Region::Hue),
                    "{shape:?}: inside the inner edge is not the ring, at {angle}"
                );
            }
            assert!(
                found,
                "{shape:?}: there is no gap between the ring and the interior at all"
            );
        }
    }

    /// A rectangle wider than it is tall gives a wheel centred in it at the size the short side
    /// allows — not an ellipse, and not something pushed against one edge.
    #[test]
    fn a_wheel_is_centred_and_sized_by_the_shorter_side() {
        let colour = Hsv::new(45.0, 0.6, 0.6);
        for shape in SHAPES {
            for rect in [
                Rect::new(0.0, 0.0, 120.0, 400.0),
                Rect::new(0.0, 0.0, 400.0, 120.0),
                Rect::new(-30.0, 7.0, 500.0, 90.0),
            ] {
                let wheel = Wheel::new(shape, rect, colour);
                let mut points: Vec<(f32, f32)> = Vec::new();
                if let Some(ring) = wheel.hue_ring() {
                    for angle in (0u16..360).step_by(15) {
                        points.push(point_on(ring.centre, ring.outer, f32::from(angle)));
                    }
                }
                for r in [wheel.sv_square(), wheel.hue_strip()].into_iter().flatten() {
                    points.push((r.x, r.y));
                    points.push((r.x + r.w, r.y + r.h));
                }
                points.extend(wheel.sv_triangle().into_iter().flatten());
                assert!(!points.is_empty(), "{shape:?} in {rect:?} drew nothing");
                for (x, y) in points {
                    assert!(
                        x >= rect.x - 0.01
                            && x <= rect.x + rect.w + 0.01
                            && y >= rect.y - 0.01
                            && y <= rect.y + rect.h + 0.01,
                        "{shape:?} in {rect:?} puts a point at ({x}, {y})"
                    );
                }
                // Centred, which is what "uses the shorter dimension" means: the same wheel in the
                // largest centred square inside that rectangle has to answer identically.
                if shape != Shape::Square {
                    let side = rect.w.min(rect.h);
                    let centred = Rect::new(
                        rect.x + (rect.w - side) * 0.5,
                        rect.y + (rect.h - side) * 0.5,
                        side,
                        side,
                    );
                    assert_eq!(
                        wheel.hue_ring(),
                        Wheel::new(shape, centred, colour).hue_ring(),
                        "{shape:?} in {rect:?} is not the centred square's wheel"
                    );
                }
            }
        }
    }

    /// **A wheel with no room answers `None` and does not guess.** A panel dragged to a sliver, a
    /// rectangle not laid out yet, or an infinity arriving from a division upstream. What this
    /// rules out is a ring of radius zero, where every press reports a hue read off a `0/0` and
    /// the artist's colour changes at random.
    #[test]
    fn a_rectangle_with_no_room_in_it_has_no_wheel() {
        let colour = Hsv::new(210.0, 0.4, 0.8);
        for rect in [
            Rect::new(0.0, 0.0, 0.0, 0.0),
            Rect::new(5.0, 5.0, 100.0, 0.0),
            Rect::new(5.0, 5.0, 0.0, 100.0),
            Rect::new(5.0, 5.0, -30.0, 40.0),
            Rect::new(5.0, 5.0, f32::NAN, 40.0),
            Rect::new(f32::NAN, 5.0, 40.0, 40.0),
            Rect::new(5.0, f32::INFINITY, 40.0, 40.0),
        ] {
            for shape in SHAPES {
                let wheel = Wheel::new(shape, rect, colour);
                assert!(
                    wheel.is_empty(),
                    "{shape:?} in {rect:?} claims to have room"
                );
                assert_eq!(wheel.marker(Region::Hue), None, "{shape:?} in {rect:?}");
                assert_eq!(
                    wheel.marker(Region::Interior),
                    None,
                    "{shape:?} in {rect:?}"
                );
                assert_eq!(wheel.hue_ring(), None, "{shape:?} in {rect:?}");
                assert_eq!(wheel.hue_strip(), None, "{shape:?} in {rect:?}");
                assert_eq!(wheel.sv_square(), None, "{shape:?} in {rect:?}");
                assert_eq!(wheel.sv_triangle(), None, "{shape:?} in {rect:?}");
                for (x, y) in [(0.0, 0.0), (5.0, 5.0), (50.0, 50.0), (-20.0, 7.0)] {
                    assert_eq!(wheel.region_at(x, y), None, "{shape:?} {rect:?} ({x}, {y})");
                    assert_eq!(wheel.colour_at(x, y), None, "{shape:?} {rect:?} ({x}, {y})");
                    // Asked directly it hands the colour back unchanged rather than a NaN: a
                    // caller mid-drag does not re-check whether the panel shrank under it.
                    assert_eq!(wheel.colour_in(Region::Hue, x, y), colour);
                    assert_eq!(wheel.colour_in(Region::Interior, x, y), colour);
                }
            }
        }
    }

    /// **The exact centre has no hue**, and asking for one there must not invent it. Reachable in
    /// practice: a drag that grabbed the ring and swept across the middle passes through it, and a
    /// hue snapping to red on the way past would be visible.
    #[test]
    fn the_centre_of_the_wheel_keeps_the_hue_it_had() {
        for shape in [Shape::Ring, Shape::Triangle] {
            let colour = Hsv::new(147.0, 0.6, 0.6);
            let wheel = Wheel::new(shape, bounds(), colour);
            let ring = wheel.hue_ring().expect("a ring");
            let at_centre = wheel.colour_in(Region::Hue, ring.centre.0, ring.centre.1);
            assert!(
                at_centre.hue().is_finite(),
                "{shape:?}: the centre produced a NaN hue"
            );
            assert!(
                (at_centre.hue() - colour.hue()).abs() < 0.001,
                "{shape:?}: the centre changed the hue to {}",
                at_centre.hue()
            );
        }
    }

    /// **A drag that runs off the wheel pins the value rather than dropping the gesture.** The
    /// ordinary case and not an edge one: a hand dragging the saturation marker does not stop
    /// politely at the edge of the square.
    #[test]
    fn a_drag_past_the_edge_stays_inside_the_wheel() {
        for shape in SHAPES {
            let wheel = Wheel::new(shape, bounds(), Hsv::new(90.0, 0.5, 0.5));
            for (x, y) in [
                (-4000.0, -4000.0),
                (4000.0, 4000.0),
                (-4000.0, 4000.0),
                (0.0, 100.0),
                (f32::INFINITY, 60.0),
                (f32::NEG_INFINITY, f32::NAN),
            ] {
                for region in [Region::Hue, Region::Interior] {
                    let c = wheel.colour_in(region, x, y);
                    assert!(
                        c.hue().is_finite()
                            && (0.0..360.0).contains(&c.hue())
                            && (0.0..=1.0).contains(&c.saturation())
                            && (0.0..=1.0).contains(&c.value()),
                        "{shape:?} {region:?} at ({x}, {y}) escaped to {c:?}"
                    );
                }
            }
        }
    }

    /// **A drag off the end of a control pins at that end**, which is a stronger claim than
    /// staying in range and a different one.
    ///
    /// It exists because "in range" was not enough. A hue does not clamp, it wraps: drop the clamp
    /// on how far along the strip a press is and dragging off the *left* end gives −180 degrees,
    /// which comes back round as cyan. Perfectly in range, and the artist dragging towards red
    /// watches the colour jump to its opposite. The sabotage that removed that clamp passed every
    /// test until this one existed.
    #[test]
    fn a_drag_off_the_end_of_a_control_pins_at_that_end() {
        let wheel = Wheel::new(Shape::Square, bounds(), Hsv::new(200.0, 0.5, 0.5));
        let strip = wheel.hue_strip().expect("a strip");
        let mid = strip.y + strip.h * 0.5;
        for past in [1.0, 50.0, 4000.0] {
            // Both ends of the strip are red, since it is a circle cut open at red.
            let left = wheel.colour_in(Region::Hue, strip.x - past, mid).hue();
            let right = wheel
                .colour_in(Region::Hue, strip.x + strip.w + past, mid)
                .hue();
            assert!(left < 0.5, "{past} left of the strip landed on hue {left}");
            assert!(
                right < 0.5,
                "{past} right of the strip landed on hue {right}"
            );
        }

        // And the square's corners pin the same way, on both axes at once.
        let square = wheel.sv_square().expect("a square");
        let far = |dx: f32, dy: f32| {
            wheel.colour_in(
                Region::Interior,
                square.x + square.w * 0.5 + dx,
                square.y + square.h * 0.5 + dy,
            )
        };
        assert_eq!(
            far(-9000.0, -9000.0).to_srgb8(),
            [255, 255, 255],
            "up and left is white"
        );
        assert_eq!(
            far(-9000.0, 9000.0).to_srgb8(),
            [0, 0, 0],
            "down and left is black"
        );
        assert_eq!(
            far(9000.0, -9000.0).to_srgb8(),
            Hsv::new(200.0, 1.0, 1.0).to_srgb8(),
            "up and right is the pure hue"
        );
    }

    /// A hue strip runs the whole circle across its width, sits clear of the square, and is the
    /// only hue control the square shape has.
    #[test]
    fn the_hue_strip_runs_from_red_round_to_red() {
        let wheel = Wheel::new(Shape::Square, bounds(), Hsv::new(0.0, 1.0, 1.0));
        assert_eq!(wheel.hue_ring(), None, "the square shape has no ring");
        let strip = wheel.hue_strip().expect("a strip");
        let hue_at = |t: f32| {
            wheel
                .colour_in(
                    Region::Hue,
                    strip.w.mul_add(t, strip.x),
                    strip.y + strip.h * 0.5,
                )
                .hue()
        };
        assert!(
            hue_at(0.0) < 0.5,
            "the left end is red, got {}",
            hue_at(0.0)
        );
        assert!(
            (hue_at(1.0 / 3.0) - 120.0).abs() < 1.0,
            "a third along should be green, got {}",
            hue_at(1.0 / 3.0)
        );
        assert!(
            (hue_at(2.0 / 3.0) - 240.0).abs() < 1.0,
            "two thirds along should be blue, got {}",
            hue_at(2.0 / 3.0)
        );
        // 360 is red again, and must arrive as 0 rather than as a hue off the end of the circle.
        assert!(hue_at(1.0) < 0.5, "the right end wraps back to red");

        let square = wheel.sv_square().expect("a square");
        assert!(
            strip.y >= square.y + square.h,
            "the strip overlaps the square it sits below"
        );
    }
}
