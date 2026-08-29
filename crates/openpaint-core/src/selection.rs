//! Selection — a per-pixel coverage mask over the document.
//!
//! # One primitive, several consumers
//!
//! A selection is not a tool. It is a **mask**: how much each pixel is selected, from none to
//! fully. Producing one and consuming one are separate concerns, which is what keeps region logic
//! from being written once per tool:
//!
//! - **Producers**: lasso, rectangle, select-all, invert, and later a flood fill from a seed point
//!   (the magic wand — which is also what a bucket uses before it fills).
//! - **Consumers**: fill, delete, transform, and simply confining a brush.
//!
//! A bucket fill is therefore not a tool with intelligence in it. It is *find a region, fill it,
//! discard the mask* — the same region-finder the wand uses, behind a different front end. Building
//! a flood fill inside a bucket tool would guarantee a second, subtly different copy the day the
//! wand arrives, which is the mistake `export::Composite` was extracted to undo.
//!
//! # Coverage, not a bitmask
//!
//! One byte per pixel rather than one bit. It costs eight times the memory of a boolean mask, on a
//! thing that is transient and already eight times cheaper than a colour tile, and it buys
//! properties that would be expensive to retrofit onto every consumer later: anti-aliased selection
//! edges, feathering, partial selection, and soft-edged fills.
//!
//! It also makes this the same primitive a **layer mask** needs — per-pixel coverage attached to a
//! layer instead of to the document. That is a larger feature than selection for comics
//! (non-destructive erasing, masking a tone without destroying it), and it should not need a second
//! implementation.
//!
//! # The CPU holds the truth
//!
//! Deliberately, and it dissolves a problem rather than solving one. The canvas needed the whole
//! residency-and-spill machinery of `tile_store.rs` because its tiles are the only copy of the
//! artwork. A mask's authoritative copy lives here, in ordinary memory, so any GPU-side mirror is a
//! *cache*: it can be rebuilt, and running out of room for it cannot corrupt anything. It is also
//! where a flood fill wants its data, being an inherently serial algorithm.
//!
//! # Sparse by tile
//!
//! Only tiles the selection actually touches exist. A lasso around one character on a webtoon strip
//! costs the tiles it covers, not the strip.

use std::collections::HashMap;

use crate::page::PageRect;
use crate::tile::{TileCoord, TILE_SIZE, TILE_TEXELS};

/// Subsamples per axis when rasterizing a shape into coverage.
///
/// 4×4 = 16 levels of edge coverage. Not exact area sampling, which is considerably more code for a
/// difference invisible at one byte of precision — but enough that a diagonal lasso edge is smooth
/// rather than a staircase, which is the whole reason coverage is a byte.
const SUBSAMPLES: u32 = 4;

/// How much of each pixel is selected, tile by tile.
///
/// Absent tiles are entirely unselected, which is what keeps a selection cheap on a large page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    tiles: HashMap<TileCoord, Vec<u8>>,
}

impl Selection {
    /// A selection of nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing at all is selected.
    ///
    /// Checks coverage rather than merely whether tiles exist: an operation can leave a tile
    /// allocated and empty, and "there is a selection" has to mean something is in it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self
            .tiles
            .values()
            .any(|t| t.iter().any(|&coverage| coverage > 0))
    }

    /// The tiles this selection touches, and their coverage.
    pub fn tiles(&self) -> impl Iterator<Item = (&TileCoord, &Vec<u8>)> {
        self.tiles.iter()
    }

    /// How selected a page pixel is, from 0 to 255.
    #[must_use]
    pub fn coverage_at(&self, x: i32, y: i32) -> u8 {
        let side = TILE_SIZE as i32;
        let coord = (x.div_euclid(side), y.div_euclid(side));
        let lx = x.rem_euclid(side) as usize;
        let ly = y.rem_euclid(side) as usize;
        self.tiles.get(&coord).map_or(0, |t| t[ly * TILE_SIZE + lx])
    }

    /// Select the whole page.
    #[must_use]
    pub fn everything(page: PageRect) -> Self {
        Self::from_coverage(page, |_, _| 255)
    }

    /// Select an axis-aligned rectangle, clipped to the page.
    #[must_use]
    pub fn from_rect(rect: PageRect, page: PageRect) -> Self {
        let (ex, ey) = rect.end();
        let (fx, fy) = (rect.x as f32, rect.y as f32);
        Self::from_shape(page, |px, py| {
            px >= fx && py >= fy && px < ex as f32 && py < ey as f32
        })
    }

    /// Select the interior of a polygon — the lasso.
    ///
    /// Implicitly closed: the last point joins the first, because a lasso the artist did not quite
    /// close is a lasso they meant to close.
    #[must_use]
    pub fn from_polygon(points: &[(f32, f32)], page: PageRect) -> Self {
        if points.len() < 3 {
            // A point or a line encloses nothing. An empty selection rather than something
            // degenerate keeps "did that gesture select anything" a question the caller can ask.
            return Self::new();
        }
        Self::from_shape(page, |px, py| point_in_polygon(px, py, points))
    }

    /// Everything on the page this selection does not cover.
    #[must_use]
    pub fn inverted(&self, page: PageRect) -> Self {
        Self::from_coverage(page, |x, y| 255 - self.coverage_at(x, y))
    }

    /// Rasterize a shape into coverage by supersampling.
    fn from_shape(page: PageRect, inside: impl Fn(f32, f32) -> bool) -> Self {
        let step = 1.0 / SUBSAMPLES as f32;
        let offset = step / 2.0;
        Self::from_coverage(page, |x, y| {
            let mut hits = 0_u32;
            for sy in 0..SUBSAMPLES {
                for sx in 0..SUBSAMPLES {
                    let px = x as f32 + sx as f32 * step + offset;
                    let py = y as f32 + sy as f32 * step + offset;
                    if inside(px, py) {
                        hits += 1;
                    }
                }
            }
            // Scaled so a fully covered pixel reads exactly 255 rather than 254.
            u8::try_from((hits * 255) / (SUBSAMPLES * SUBSAMPLES)).unwrap_or(255)
        })
    }

    /// Build from a per-pixel coverage function over the page.
    ///
    /// Tiles that end up entirely unselected are dropped, so a selection stays sparse however it
    /// was produced.
    fn from_coverage(page: PageRect, coverage: impl Fn(i32, i32) -> u8) -> Self {
        let side = TILE_SIZE as i32;
        let (ex, ey) = page.end();
        let mut tiles = HashMap::new();
        if page.w == 0 || page.h == 0 {
            return Self { tiles };
        }

        let first = (page.x.div_euclid(side), page.y.div_euclid(side));
        let last = ((ex - 1).div_euclid(side), (ey - 1).div_euclid(side));
        for ty in first.1..=last.1 {
            for tx in first.0..=last.0 {
                let mut tile = vec![0_u8; TILE_TEXELS];
                let mut any = false;
                for ly in 0..TILE_SIZE {
                    for lx in 0..TILE_SIZE {
                        let x = tx * side + lx as i32;
                        let y = ty * side + ly as i32;
                        // Clipped to the page: a selection outside it could never be filled,
                        // and letting one exist would put coverage where no tile can hold paint.
                        if !page.contains(x, y) {
                            continue;
                        }
                        let c = coverage(x, y);
                        if c > 0 {
                            tile[ly * TILE_SIZE + lx] = c;
                            any = true;
                        }
                    }
                }
                if any {
                    tiles.insert((tx, ty), tile);
                }
            }
        }
        Self { tiles }
    }
}

/// Even-odd point-in-polygon, on an implicitly closed path.
fn point_in_polygon(x: f32, y: f32, points: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        // Half-open in y, so a vertex sitting exactly on the scanline is counted once rather than
        // twice or never — the classic source of single-pixel holes along a horizontal edge.
        if (yi > y) != (yj > y) {
            let t = (y - yi) / (yj - yi);
            if x < xi + t * (xj - xi) {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> PageRect {
        PageRect::from_size(400, 400)
    }

    #[test]
    fn nothing_is_selected_to_begin_with() {
        assert!(Selection::new().is_empty());
        assert_eq!(Selection::new().coverage_at(10, 10), 0);
    }

    #[test]
    fn a_rectangle_selects_exactly_itself() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 50, 50), page());
        assert_eq!(sel.coverage_at(100, 100), 255, "the first pixel is inside");
        assert_eq!(sel.coverage_at(149, 149), 255, "and so is the last");
        assert_eq!(sel.coverage_at(99, 100), 0, "one pixel left of it is not");
        assert_eq!(sel.coverage_at(150, 100), 0, "nor one pixel past the end");
        assert!(!sel.is_empty());
    }

    /// The reason coverage is a byte rather than a bit.
    ///
    /// A diagonal edge must come out smooth, not as a staircase. Asserts the *existence* of partial
    /// coverage along the edge rather than exact values, which would only pin the subsample count.
    #[test]
    fn a_diagonal_edge_is_anti_aliased() {
        // A triangle with one 45° edge.
        let sel = Selection::from_polygon(&[(50.0, 50.0), (250.0, 50.0), (50.0, 250.0)], page());

        // Counted over the whole triangle rather than along the exact line: a pixel the edge
        // passes through the centre of has all its subsamples on one side, so the partially
        // covered pixels sit just inside it. Scanning avoids the test asserting where they land.
        let mut partial = 0;
        for y in 40..260 {
            for x in 40..260 {
                let c = sel.coverage_at(x, y);
                if c > 0 && c < 255 {
                    partial += 1;
                }
            }
        }
        assert!(
            partial > 100,
            "a 45° edge produced only {partial} partially covered pixels; it is a staircase"
        );

        // And the interior and exterior are still unambiguous.
        assert_eq!(sel.coverage_at(60, 60), 255, "well inside");
        assert_eq!(sel.coverage_at(240, 240), 0, "well outside");
    }

    #[test]
    fn a_lasso_needs_three_points_to_enclose_anything() {
        assert!(Selection::from_polygon(&[], page()).is_empty());
        assert!(Selection::from_polygon(&[(10.0, 10.0)], page()).is_empty());
        assert!(Selection::from_polygon(&[(10.0, 10.0), (99.0, 99.0)], page()).is_empty());
    }

    /// A concave shape is the case an even-odd test has to get right, and the one a naive
    /// "inside if any edge lies to the right" implementation quietly gets wrong.
    ///
    /// The notch opens **left**, deliberately. A point inside a right-opening notch has *no* edges
    /// to its right, so it reads as outside under either rule and proves nothing — the first
    /// version of this test was exactly that, and a sabotage replacing the even-odd toggle with an
    /// unconditional `inside = true` sailed through it. Here the notch point has two edges to its
    /// right: even, therefore outside, and only a real parity test says so.
    #[test]
    fn a_concave_lasso_excludes_its_notch() {
        // A "C" reversed: spine down the right at x = 240..300, arms reaching left to x = 100.
        let sel = Selection::from_polygon(
            &[
                (100.0, 100.0),
                (300.0, 100.0),
                (300.0, 300.0),
                (100.0, 300.0),
                (100.0, 260.0),
                (240.0, 260.0),
                (240.0, 140.0),
                (100.0, 140.0),
            ],
            page(),
        );

        assert_eq!(sel.coverage_at(270, 200), 255, "the spine");
        assert_eq!(sel.coverage_at(150, 120), 255, "the top arm");
        assert_eq!(sel.coverage_at(150, 280), 255, "the bottom arm");
        assert_eq!(
            sel.coverage_at(150, 200),
            0,
            "the notch was selected: two crossings to the right is even, therefore outside"
        );
    }

    #[test]
    fn a_selection_is_clipped_to_the_page() {
        // Deliberately hangs off the top-left and bottom-right.
        let sel = Selection::from_rect(PageRect::new(-50, -50, 500, 500), page());
        assert_eq!(sel.coverage_at(0, 0), 255);
        assert_eq!(sel.coverage_at(399, 399), 255);
        assert_eq!(
            sel.coverage_at(-1, -1),
            0,
            "coverage outside the page could never be filled"
        );
        assert_eq!(sel.coverage_at(400, 400), 0);
    }

    /// Sparseness is what makes a selection affordable on a webtoon strip.
    #[test]
    fn only_touched_tiles_are_stored() {
        // A page four tiles wide and four tall; the selection sits inside one of them.
        let sel = Selection::from_rect(
            PageRect::new(10, 10, 40, 40),
            PageRect::from_size(1024, 1024),
        );
        assert_eq!(
            sel.tiles().count(),
            1,
            "a small selection allocated tiles it does not cover"
        );
    }

    #[test]
    fn everything_selects_the_whole_page_and_nothing_beyond() {
        let sel = Selection::everything(page());
        assert_eq!(sel.coverage_at(0, 0), 255);
        assert_eq!(sel.coverage_at(399, 399), 255);
        assert_eq!(sel.coverage_at(400, 0), 0);
        assert_eq!(sel.tiles().count(), 4, "400px is two tiles each way");
    }

    #[test]
    fn inverting_swaps_what_is_selected() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 50, 50), page());
        let flipped = sel.inverted(page());

        assert_eq!(flipped.coverage_at(120, 120), 0, "inside became outside");
        assert_eq!(flipped.coverage_at(10, 10), 255, "outside became inside");
        assert_eq!(
            flipped.coverage_at(400, 400),
            0,
            "inverting must not select beyond the page"
        );

        // Twice round is where an off-by-one in the complement shows up.
        let back = flipped.inverted(page());
        assert_eq!(back.coverage_at(120, 120), 255);
        assert_eq!(back.coverage_at(10, 10), 0);
    }

    /// Partial coverage has to survive inversion proportionally, or feathered edges would harden
    /// the first time anyone inverted a selection.
    #[test]
    fn inverting_preserves_partial_coverage() {
        let sel = Selection::from_polygon(&[(50.0, 50.0), (250.0, 50.0), (50.0, 250.0)], page());
        let flipped = sel.inverted(page());
        let mut checked = 0;
        for y in 40..260 {
            for x in 40..260 {
                let a = sel.coverage_at(x, y);
                if a > 0 && a < 255 {
                    assert_eq!(
                        flipped.coverage_at(x, y),
                        255 - a,
                        "partial coverage at ({x}, {y}) did not invert proportionally"
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked > 100,
            "found only {checked} partial pixels to check"
        );
    }
}
