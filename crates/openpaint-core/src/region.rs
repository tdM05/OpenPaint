//! Finding the region under a point — the magic wand, and therefore the bucket.
//!
//! # This is the only hard part of a bucket fill
//!
//! Filling is trivial once a [`Selection`] exists, and it already does. What a bucket adds is
//! *which pixels*, and that is this module. The wand keeps the mask; the bucket fills it and throws
//! it away. One implementation, because a second one would be a second set of edge cases to get
//! subtly differently wrong.
//!
//! # Taking a sampler rather than a canvas
//!
//! [`flood`] asks a closure for pixels. That is not indirection for its own sake:
//!
//! - It makes "refer to the composite" versus "refer to one layer" a decision for the *caller*,
//!   not a mode in here. For comics the answer is the composite — you fill on an empty flats layer
//!   while the line art sits above it — but that is a workflow choice and it can change without
//!   this code knowing.
//! - It makes the whole thing testable against a drawn-by-hand image, with no GPU, no tiles and no
//!   residency. Every edge case below is pinned that way.
//!
//! # Anti-aliased ink is the real problem
//!
//! A line drawn with a soft brush has semi-transparent edge pixels, so the colour ramps from ink to
//! paper over two or three pixels. A flood fill that stops at the first pixel unlike the seed halts
//! partway up that ramp and leaves a pale halo hugging every line; one that is more permissive
//! leaks through the gap where two strokes nearly meet and floods the page.
//!
//! Neither is solved by cleverness. `tolerance` decides how far up the ramp counts as "still the
//! same region", and `expand` grows the finished region back under the ink that was excluded. Both
//! are numbers the artist sets, which is what every application that does this well offers.

use crate::page::PageRect;
use crate::selection::Selection;

/// Find the region of similar colour connected to `seed`.
///
/// `tolerance` is the largest per-channel difference from the seed colour that still counts as the
/// same region, in 0–255. `expand` grows the result outward afterwards, in pixels.
///
/// Four-connected, so a region does not leak through a diagonal touch — a one-pixel gap on the
/// diagonal is almost always ink that nearly met rather than a way through.
#[must_use]
pub fn flood(
    page: PageRect,
    seed: (i32, i32),
    tolerance: u8,
    expand: u32,
    sample: impl Fn(i32, i32) -> [u8; 3],
) -> Selection {
    if !page.contains(seed.0, seed.1) {
        return Selection::new();
    }
    let (w, h) = (page.w as usize, page.h as usize);
    let target = sample(seed.0, seed.1);
    let matches = |x: i32, y: i32| similar(sample(x, y), target, tolerance);

    // A flat grid over the page rather than a sparse map: a flood fill touches its pixels in
    // scanline order and asks about neighbours constantly, so a hash lookup per query would
    // dominate. One byte per pixel of the page, which is the same order as the mask it produces.
    let mut inside = vec![false; w * h];
    let mut seen = vec![false; w * h];
    let idx = |x: i32, y: i32| (y - page.y) as usize * w + (x - page.x) as usize;

    // Spans rather than single pixels: a queue of individual pixels on a large flat area is both
    // slower and far larger, and the span form is the standard algorithm for exactly that reason.
    let mut stack = vec![seed];
    let (ex, ey) = page.end();
    while let Some((sx, sy)) = stack.pop() {
        if seen[idx(sx, sy)] || !matches(sx, sy) {
            continue;
        }
        // Widen to the whole run this pixel sits in.
        let mut x0 = sx;
        while x0 > page.x && !seen[idx(x0 - 1, sy)] && matches(x0 - 1, sy) {
            x0 -= 1;
        }
        let mut x1 = sx;
        while x1 + 1 < ex && !seen[idx(x1 + 1, sy)] && matches(x1 + 1, sy) {
            x1 += 1;
        }
        for x in x0..=x1 {
            let i = idx(x, sy);
            seen[i] = true;
            inside[i] = true;
        }

        // Offer the rows above and below. One seed per *run* of matching pixels, not one per
        // pixel: without that, a wide region pushes its whole width onto the stack twice over.
        for ny in [sy - 1, sy + 1] {
            if ny < page.y || ny >= ey {
                continue;
            }
            let mut x = x0;
            while x <= x1 {
                if !seen[idx(x, ny)] && matches(x, ny) {
                    stack.push((x, ny));
                    // Skip the rest of this run; the span widening above will claim it.
                    while x <= x1 && matches(x, ny) {
                        x += 1;
                    }
                } else {
                    x += 1;
                }
            }
        }
    }

    if expand > 0 {
        inside = dilate(&inside, w, h, expand);
    }

    let mut out = Selection::new();
    for y in 0..h {
        // A run at a time, so the mask resolves a tile once per run rather than once per pixel.
        let mut x = 0;
        while x < w {
            if !inside[y * w + x] {
                x += 1;
                continue;
            }
            let start = x;
            while x < w && inside[y * w + x] {
                x += 1;
            }
            out.write_coverage_run(
                page.x + start as i32,
                page.y + y as i32,
                &vec![255_u8; x - start],
            );
        }
    }
    out
}

/// Whether two colours are within `tolerance` on every channel.
///
/// Largest single-channel difference rather than a Euclidean distance: it is what the artist can
/// predict from the number, and it never calls two colours similar because their differences were
/// spread thinly across three channels.
fn similar(a: [u8; 3], b: [u8; 3], tolerance: u8) -> bool {
    (0..3).all(|i| a[i].abs_diff(b[i]) <= tolerance)
}

/// Grow a region outward by `radius` pixels.
///
/// This is what puts colour *under* anti-aliased ink. A flood fill necessarily stops partway up the
/// edge ramp, leaving a pale fringe; expanding the region afterwards tucks it beneath the line where
/// it cannot be seen. Circular rather than square, so a curve does not grow corners.
fn dilate(inside: &[bool], w: usize, h: usize, radius: u32) -> Vec<bool> {
    let r = radius as i32;
    let r2 = r * r;
    let mut out = inside.to_vec();
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            if !inside[y as usize * w + x as usize] {
                continue;
            }
            // Only edge pixels can grow anything, and skipping the interior is the difference
            // between a pass over the region and a pass over its perimeter.
            let interior = x > 0
                && y > 0
                && x + 1 < w as i32
                && y + 1 < h as i32
                && inside[y as usize * w + (x - 1) as usize]
                && inside[y as usize * w + (x + 1) as usize]
                && inside[(y - 1) as usize * w + x as usize]
                && inside[(y + 1) as usize * w + x as usize];
            if interior {
                continue;
            }
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx * dx + dy * dy > r2 {
                        continue;
                    }
                    let (nx, ny) = (x + dx, y + dy);
                    if nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32 {
                        out[ny as usize * w + nx as usize] = true;
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAPER: [u8; 3] = [255, 255, 255];
    const INK: [u8; 3] = [0, 0, 0];

    fn page() -> PageRect {
        PageRect::from_size(100, 100)
    }

    /// A closed box of ink, with the interior and exterior both paper.
    fn boxed(gap: bool) -> impl Fn(i32, i32) -> [u8; 3] {
        move |x, y| {
            let on_edge = (x == 20 || x == 80) && (20..=80).contains(&y)
                || (y == 20 || y == 80) && (20..=80).contains(&x);
            // A one-pixel hole in the top edge, for the leak case.
            let holed = gap && y == 20 && x == 50;
            if on_edge && !holed {
                INK
            } else {
                PAPER
            }
        }
    }

    #[test]
    fn a_closed_region_is_bounded_by_its_outline() {
        let sel = flood(page(), (50, 50), 10, 0, boxed(false));
        assert_eq!(sel.coverage_at(50, 50), 255, "the middle of the box");
        assert_eq!(sel.coverage_at(21, 21), 255, "right up against the inside");
        assert_eq!(
            sel.coverage_at(20, 50),
            0,
            "the outline itself is not in it"
        );
        assert_eq!(sel.coverage_at(10, 10), 0, "and nothing outside is");
        assert_eq!(sel.coverage_at(90, 90), 0);
    }

    /// The documented failure mode of every flood fill, worth pinning so it is a known property
    /// rather than a surprise: a gap in the outline lets the region out.
    ///
    /// It is not a bug to fix in here. It is why the tool has a tolerance and an expand, and why
    /// artists close their gaps.
    #[test]
    fn a_gap_in_the_outline_leaks() {
        let sel = flood(page(), (50, 50), 10, 0, boxed(true));
        assert_eq!(sel.coverage_at(50, 50), 255, "still fills the inside");
        assert_eq!(
            sel.coverage_at(50, 5),
            255,
            "a one-pixel gap should let it escape; if this fails the fill is not connected"
        );
    }

    /// Four-connected, so ink that meets only at a corner still holds the region in.
    ///
    /// Eight-connected filling leaks through every diagonal near-miss, and a diagonal near-miss is
    /// what a hand-drawn line makes constantly.
    #[test]
    fn a_diagonal_touch_is_not_a_way_through() {
        // Ink along a diagonal: pixels (x, x). Four-connected, the two sides never meet.
        let sel = flood(
            page(),
            (10, 90),
            10,
            0,
            |x, y| {
                if x == y {
                    INK
                } else {
                    PAPER
                }
            },
        );
        assert_eq!(sel.coverage_at(10, 90), 255, "the seed's own side");
        assert_eq!(
            sel.coverage_at(90, 10),
            0,
            "it crossed the diagonal, so the fill is eight-connected"
        );
    }

    /// Tolerance decides how far up an anti-aliased edge still counts as the region.
    #[test]
    fn tolerance_decides_how_far_up_an_edge_the_fill_climbs() {
        // A ramp from paper to ink across x = 40..=50, as a soft brush leaves.
        let ramp = |x: i32, _y: i32| {
            let v = match x {
                ..40 => 255_i32,
                40..=50 => 255 - (x - 40) * 25,
                _ => 0,
            };
            let v = v.clamp(0, 255) as u8;
            [v, v, v]
        };

        let tight = flood(page(), (10, 50), 10, 0, ramp);
        let loose = flood(page(), (10, 50), 200, 0, ramp);

        assert_eq!(tight.coverage_at(10, 50), 255);
        assert_eq!(
            tight.coverage_at(45, 50),
            0,
            "a tight tolerance should stop early on the ramp"
        );
        assert_eq!(
            loose.coverage_at(45, 50),
            255,
            "a loose one should carry on up it"
        );
    }

    /// Expanding is what puts colour *under* the ink instead of leaving a pale fringe beside it.
    #[test]
    fn expanding_grows_the_region_outward() {
        let plain = flood(page(), (50, 50), 10, 0, boxed(false));
        let grown = flood(page(), (50, 50), 10, 3, boxed(false));

        assert_eq!(plain.coverage_at(20, 50), 0, "the outline was not filled");
        assert_eq!(
            grown.coverage_at(20, 50),
            255,
            "expanding by 3 should reach under the outline"
        );
        assert_eq!(
            grown.coverage_at(16, 50),
            0,
            "but not five pixels past it: {:?}",
            grown.coverage_at(16, 50)
        );

        // Diagonally, where circular and square growth differ. The nearest filled pixel to
        // (18, 18) is the corner (21, 21), 4.2 away -- inside a square of radius 3, outside a
        // circle of it. Checking only along an axis cannot tell the two apart, and a square
        // dilate grows corners on every curve.
        assert_eq!(
            grown.coverage_at(18, 18),
            0,
            "the region grew into its diagonal corner, so the dilation is square not circular"
        );
    }

    #[test]
    fn a_seed_outside_the_page_selects_nothing() {
        assert!(flood(page(), (-5, 50), 10, 0, boxed(false)).is_empty());
        assert!(flood(page(), (500, 50), 10, 0, boxed(false)).is_empty());
    }

    /// Seeding on the ink selects the ink, not the paper. Obvious, and the kind of thing an
    /// off-by-one in the seed test would quietly invert.
    #[test]
    fn the_seed_decides_which_region_is_found() {
        let sel = flood(page(), (20, 50), 10, 0, boxed(false));
        assert_eq!(sel.coverage_at(20, 50), 255, "the outline the seed was on");
        assert_eq!(sel.coverage_at(50, 50), 0, "not the paper inside it");
    }

    /// A page that does not start at the origin has to work too — every index in here is
    /// page-relative, and an offset page is where that goes wrong.
    #[test]
    fn an_offset_page_is_handled() {
        let page = PageRect::new(-30, 40, 100, 100);
        let sel = flood(page, (0, 80), 10, 0, |x, y| {
            if x == 20 && (40..140).contains(&y) {
                INK
            } else {
                PAPER
            }
        });
        assert_eq!(sel.coverage_at(0, 80), 255, "the seed's side of the line");
        assert_eq!(sel.coverage_at(50, 80), 0, "not the far side");
        assert_eq!(sel.coverage_at(-30, 40), 255, "the page's own corner");
    }
}
