//! Pixels lifted out of a layer, floating above it until they are put down.
//!
//! # What a transform is, mechanically
//!
//! Moving a selection is three steps, and only the middle one is interactive:
//!
//! 1. **Lift** — copy the pixels the selection covers, weighted by its coverage, and clear them from
//!    the layer. What comes away is a [`Lifted`].
//! 2. **Float** — the artist drags. Each frame the lifted pixels are [`Lifted::shifted`] to the
//!    current offset and shown above the layer. Nothing is committed, so the whole gesture is
//!    reversible by simply not committing it.
//! 3. **Put down** — write them into the layer at the final offset, as one undoable operation.
//!
//! The layer is untouched between the lift and the put-down, which is what makes cancelling free
//! and what keeps a drag from filling the undo stack with a hundred intermediate positions.
//!
//! # Tiles are destination-aligned, and that is the whole trick
//!
//! A tiled canvas makes an offset awkward: shift by anything other than a whole tile and one
//! destination tile draws from as many as four source tiles. Sampling that on the GPU would mean
//! either four texture reads per pixel or an apron around every tile.
//!
//! So the shift happens here, on the CPU, and produces tiles that line up with the canvas again.
//! The consumer never sees a straddling read — it gets ordinary tiles at ordinary coordinates, the
//! same as the in-progress stroke's accumulation. The cost is re-shifting on each drag, which is a
//! copy of the selection's own pixels and not of the page.
//!
//! # Coverage rides along
//!
//! Lifting multiplies by the selection's coverage, so a feathered or anti-aliased selection lifts
//! soft edges and puts them down soft. Doing it at the lift rather than at the put-down means the
//! floating pixels *are* what will land, so the preview cannot disagree with the result.

use std::collections::HashMap;

use crate::selection::Selection;
use crate::tile::{Tile, TileCoord, TileGrid, TILE_SIZE};

/// Pixels taken out of a layer, in premultiplied linear colour.
///
/// Sparse: only tiles the selection actually reached exist.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lifted {
    tiles: HashMap<TileCoord, Tile>,
}

impl Lifted {
    /// Take the pixels a selection covers out of a layer's tiles.
    ///
    /// `source` supplies the layer's tile at a coordinate, or `None` where it has none. Weighted by
    /// coverage, so a soft-edged selection lifts a soft edge.
    #[must_use]
    pub fn from_layer(
        selection: &Selection,
        mut source: impl FnMut(TileCoord) -> Option<Tile>,
    ) -> Self {
        let mut tiles = HashMap::new();
        for (coord, coverage) in selection.tiles() {
            let Some(tile) = source(*coord) else {
                // The layer has nothing here, so there is nothing to lift. Not an error: a
                // selection routinely covers empty canvas.
                continue;
            };
            let mut lifted = Tile::transparent();
            let mut any = false;
            for ly in 0..TILE_SIZE {
                for lx in 0..TILE_SIZE {
                    let c = coverage[ly * TILE_SIZE + lx];
                    if c == 0 {
                        continue;
                    }
                    let scale = f32::from(c) / 255.0;
                    let texel = tile.texel(lx, ly);
                    if texel[3] <= 0.0 {
                        continue;
                    }
                    // Premultiplied, so one multiply scales colour and coverage together.
                    lifted.set_texel(lx, ly, crate::color::scale_premul(texel, scale));
                    any = true;
                }
            }
            if any {
                tiles.insert(*coord, lifted);
            }
        }
        Self { tiles }
    }

    /// Whether anything was actually lifted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// The tiles as lifted, at their original coordinates.
    pub fn tiles(&self) -> impl Iterator<Item = (&TileCoord, &Tile)> {
        self.tiles.iter()
    }

    /// The same pixels moved by a whole-pixel offset, back on the tile grid.
    ///
    /// Whole pixels only. Sub-pixel movement is a resampling problem — it needs a filter, and it
    /// degrades the image every time it is applied — so it belongs with scaling and rotation, which
    /// have to resample anyway. A move should not quietly soften what it moves.
    #[must_use]
    pub fn shifted(&self, dx: i32, dy: i32) -> HashMap<TileCoord, Tile> {
        let side = TILE_SIZE as i32;
        let mut out: HashMap<TileCoord, Tile> = HashMap::new();
        if dx == 0 && dy == 0 {
            return self.tiles.clone();
        }

        for (coord, tile) in &self.tiles {
            for ly in 0..TILE_SIZE {
                for lx in 0..TILE_SIZE {
                    let texel = tile.texel(lx, ly);
                    if texel[3] <= 0.0 && texel[0] <= 0.0 && texel[1] <= 0.0 && texel[2] <= 0.0 {
                        continue;
                    }
                    // Source page position, then destination, then which tile that lands in.
                    let x = coord.0 * side + lx as i32 + dx;
                    let y = coord.1 * side + ly as i32 + dy;
                    let dest = (x.div_euclid(side), y.div_euclid(side));
                    out.entry(dest).or_insert_with(Tile::transparent).set_texel(
                        x.rem_euclid(side) as usize,
                        y.rem_euclid(side) as usize,
                        texel,
                    );
                }
            }
        }
        out
    }

    /// The page-pixel rectangle the lifted pixels occupy, as `(min_x, min_y, max_x, max_y)` with
    /// the maxima exclusive.
    ///
    /// Tile-aligned rather than tight to the ink, and cheap for that reason.
    ///
    /// Not what a resampling loop should walk: it does *not* skip transparent source, so a
    /// selection sitting in the middle of a tile makes it filter three quarters of nothing. Use
    /// [`Lifted::content_bounds`] for that, and this where a safe, cheap box is enough.
    #[must_use]
    pub fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
        let side = TILE_SIZE as i32;
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for coord in self.tiles.keys() {
            let (x, y) = (coord.0 * side, coord.1 * side);
            bounds = Some(match bounds {
                None => (x, y, x + side, y + side),
                Some((lx, ly, hx, hy)) => {
                    (lx.min(x), ly.min(y), hx.max(x + side), hy.max(y + side))
                }
            });
        }
        bounds
    }

    /// The page-pixel rectangle the lifted pixels *actually* occupy, maxima exclusive.
    ///
    /// Tight to the ink, which for a resampling loop is the difference between filtering the
    /// artwork and filtering the empty part of the tiles it happens to sit in. A 256-pixel
    /// selection lifted from the middle of a tile has tile-aligned bounds of 512 by 512 — four
    /// times the destination pixels, all but a quarter of them reconstructing transparency.
    ///
    /// Costs a scan of the lifted texels, which is one pass over data the transform is about to
    /// read twenty times anyway.
    #[must_use]
    pub fn content_bounds(&self) -> Option<(i32, i32, i32, i32)> {
        let side = TILE_SIZE as i32;
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for (coord, tile) in &self.tiles {
            let (ox, oy) = (coord.0 * side, coord.1 * side);
            for ly in 0..TILE_SIZE {
                let mut first = None;
                let mut last = 0;
                for lx in 0..TILE_SIZE {
                    // Alpha alone is not enough: a premultiplied texel may carry colour with zero
                    // alpha only if something upstream is wrong, but `shifted` already treats a
                    // texel as present when *any* channel is, and two answers to "is there
                    // anything here" would eventually disagree.
                    let texel = tile.texel(lx, ly);
                    if texel[3] > 0.0 || texel[0] > 0.0 || texel[1] > 0.0 || texel[2] > 0.0 {
                        first.get_or_insert(lx);
                        last = lx;
                    }
                }
                let Some(first) = first else {
                    continue;
                };
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_possible_wrap,
                    reason = "indices within a 256-wide tile"
                )]
                let (x0, x1, y) = (ox + first as i32, ox + last as i32 + 1, oy + ly as i32);
                bounds = Some(match bounds {
                    None => (x0, y, x1, y + 1),
                    Some((lx, ly0, hx, hy)) => (lx.min(x0), ly0.min(y), hx.max(x1), hy.max(y + 1)),
                });
            }
        }
        bounds
    }

    /// One texel in page coordinates, transparent outside the lifted pixels.
    ///
    /// Transparent rather than clamped: these pixels have a real edge, and repeating it would smear
    /// the outermost row of a rotated selection outward into a streak.
    #[must_use]
    pub fn texel_at(&self, x: i32, y: i32) -> [f32; 4] {
        let side = TILE_SIZE as i32;
        let coord = (x.div_euclid(side), y.div_euclid(side));
        self.tiles.get(&coord).map_or([0.0; 4], |tile| {
            tile.texel(x.rem_euclid(side) as usize, y.rem_euclid(side) as usize)
        })
    }

    /// The same pixels rotated, scaled and moved, back on the tile grid.
    ///
    /// The general case of [`Lifted::shifted`], and it delegates to it when it can: a whole-pixel
    /// move is an exact copy, and resampling one anyway would soften the artwork for nothing. That
    /// check is not an optimisation — it is the difference between a move that is lossless and one
    /// that is not.
    ///
    /// Anything else goes through [`crate::transform::resample`]. Destination-aligned like
    /// `shifted`, for the same reason: the consumer gets ordinary tiles at ordinary coordinates and
    /// never has to read across a tile seam.
    #[must_use]
    pub fn transformed(
        &self,
        transform: &crate::Transform,
        kernel: crate::Kernel,
    ) -> HashMap<TileCoord, Tile> {
        if transform.is_a_plain_move() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "checked to be a whole number by `is_a_plain_move`"
            )]
            return self.shifted(transform.offset.0 as i32, transform.offset.1 as i32);
        }
        // Tight, not tile-aligned: every destination pixel inside these bounds costs a full
        // filter footprint whether or not there is anything under it, so walking the empty part of
        // the tiles is work spent reconstructing transparency.
        let Some((min_x, min_y, max_x, max_y)) = self.content_bounds() else {
            return HashMap::new();
        };

        let dest_bounds = transform.bounds_of(min_x, min_y, max_x, max_y);

        // Both sides of the loop are laid out as dense grids of tiles first, so neither the taps
        // nor the writes hash anything.
        //
        // This is not micro-optimisation. A cubic reads sixteen to twenty-five source pixels per
        // destination pixel, and every one of them was a `HashMap` lookup -- so hashing, not
        // filtering, was most of what a rotate cost. Measured on a 512x512 selection it was
        // 620 ms *per pointer sample*, which is why a transform felt broken rather than merely
        // slow. The grids are bounded by the tiles the transform actually touches, so the memory
        // is a pointer or a tile per tile, not a copy of the page.
        //
        // Spanning whole tiles even though the bounds are tight: the filter footprint reaches a
        // little past the content, and a tap landing in the next tile has to find it.
        let src = TileGrid::covering((min_x, min_y, max_x, max_y));
        let mut source: Vec<Option<&Tile>> = vec![None; src.len()];
        for (coord, tile) in &self.tiles {
            if let Some(i) = src.index_of(*coord) {
                source[i] = Some(tile);
            }
        }

        let dst = TileGrid::covering(dest_bounds);
        let mut written: Vec<Option<Tile>> = Vec::new();
        written.resize_with(dst.len(), || None);

        crate::transform::resample(
            transform,
            kernel,
            dest_bounds,
            |x, y| {
                let (tile, lx, ly) = src.locate(x, y);
                match tile.and_then(|i| source[i]) {
                    Some(t) => t.texel(lx, ly),
                    None => [0.0; 4],
                }
            },
            |x, y, texel| {
                let (tile, lx, ly) = dst.locate(x, y);
                if let Some(i) = tile {
                    written[i]
                        .get_or_insert_with(Tile::transparent)
                        .set_texel(lx, ly, texel);
                }
            },
        );

        dst.collect(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::PageRect;

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    /// A layer whose every tile is solid red.
    fn solid(_coord: TileCoord) -> Option<Tile> {
        let mut t = Tile::transparent();
        for ly in 0..TILE_SIZE {
            for lx in 0..TILE_SIZE {
                t.set_texel(lx, ly, RED);
            }
        }
        Some(t)
    }

    fn page() -> PageRect {
        PageRect::from_size(600, 600)
    }

    /// Look up a page pixel in a shifted result.
    fn at(tiles: &HashMap<TileCoord, Tile>, x: i32, y: i32) -> [f32; 4] {
        let side = TILE_SIZE as i32;
        tiles
            .get(&(x.div_euclid(side), y.div_euclid(side)))
            .map_or([0.0; 4], |t| {
                t.texel(x.rem_euclid(side) as usize, y.rem_euclid(side) as usize)
            })
    }

    #[test]
    fn lifting_takes_only_what_the_selection_covers() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 40, 40), page());
        let lifted = Lifted::from_layer(&sel, solid);
        let placed = lifted.shifted(0, 0);

        assert_eq!(at(&placed, 120, 120), RED, "inside the selection");
        assert_eq!(at(&placed, 90, 120), [0.0; 4], "outside it");
        assert_eq!(at(&placed, 140, 120), [0.0; 4], "past its far edge");
    }

    #[test]
    fn lifting_an_empty_layer_yields_nothing() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 40, 40), page());
        assert!(Lifted::from_layer(&sel, |_| None).is_empty());
    }

    /// Partial selection coverage lifts partial pixels, so a soft edge stays soft.
    #[test]
    fn coverage_weights_what_is_lifted() {
        let mut sel = Selection::from_rect(PageRect::new(100, 100, 40, 40), page());
        sel.set_coverage(120, 120, 128);

        let placed = Lifted::from_layer(&sel, solid).shifted(0, 0);
        let soft = at(&placed, 120, 120);
        assert!(
            (soft[3] - 0.5).abs() < 0.01,
            "a half-covered pixel should lift at half alpha, got {soft:?}"
        );
        assert!(
            (soft[0] - 0.5).abs() < 0.01,
            "and premultiplied colour should scale with it, got {soft:?}"
        );
        assert_eq!(at(&placed, 121, 120), RED, "its neighbour is untouched");
    }

    /// The case the tile grid makes awkward: an offset that is not a whole tile, so every
    /// destination tile draws from more than one source tile.
    #[test]
    fn a_shift_across_tile_boundaries_lands_correctly() {
        // A selection straddling the seam at x = 256 already, then moved off it again.
        let sel = Selection::from_rect(PageRect::new(200, 200, 120, 40), page());
        let lifted = Lifted::from_layer(&sel, solid);

        let moved = lifted.shifted(37, -14);
        assert_eq!(at(&moved, 237, 186), RED, "the moved top-left corner");
        assert_eq!(at(&moved, 356, 225), RED, "the moved bottom-right corner");
        assert_eq!(at(&moved, 236, 186), [0.0; 4], "one pixel before it");
        assert_eq!(at(&moved, 357, 186), [0.0; 4], "one pixel past it");
        assert_eq!(
            at(&moved, 200, 200),
            [0.0; 4],
            "nothing was left behind at the source"
        );

        // Every lifted pixel has to survive the move; a straddling shift is exactly where some
        // would be dropped.
        let before: usize = lifted
            .tiles()
            .map(|(_, t)| {
                (0..TILE_SIZE * TILE_SIZE)
                    .filter(|i| t.texel(i % TILE_SIZE, i / TILE_SIZE)[3] > 0.0)
                    .count()
            })
            .sum();
        let after: usize = moved
            .values()
            .map(|t| {
                (0..TILE_SIZE * TILE_SIZE)
                    .filter(|i| t.texel(i % TILE_SIZE, i / TILE_SIZE)[3] > 0.0)
                    .count()
            })
            .sum();
        assert_eq!(before, after, "the shift lost or duplicated pixels");
        assert_eq!(before, 120 * 40, "the selection's own area");
    }

    /// Moving to negative page coordinates is ordinary: the canvas extends outside the page, and
    /// cropping is non-destructive (DECISIONS §5c), so a transform must not clip at the edge.
    #[test]
    fn a_shift_off_the_page_is_not_clipped() {
        let sel = Selection::from_rect(PageRect::new(0, 0, 40, 40), page());
        let moved = Lifted::from_layer(&sel, solid).shifted(-100, -100);
        assert_eq!(at(&moved, -100, -100), RED, "the moved corner, off-page");
        assert_eq!(at(&moved, -61, -61), RED, "and its far corner");
    }

    #[test]
    fn a_zero_shift_changes_nothing() {
        let sel = Selection::from_rect(PageRect::new(300, 300, 50, 50), page());
        let lifted = Lifted::from_layer(&sel, solid);
        let a = lifted.shifted(0, 0);
        let b = lifted.shifted(1, 0);
        assert_ne!(a, b, "shifting by a pixel should change the result");
        assert_eq!(at(&a, 300, 300), RED);
        assert_eq!(at(&b, 301, 300), RED);
        assert_eq!(at(&b, 300, 300), [0.0; 4]);
    }
    /// A whole-pixel move must not go through the filter, or every drag would soften the artwork
    /// slightly. This is the difference between a lossless move and a lossy one.
    #[test]
    fn a_whole_pixel_move_is_still_an_exact_copy() {
        let sel = Selection::from_rect(PageRect::new(10, 10, 40, 40), page());
        let lifted = Lifted::from_layer(&sel, solid);

        let shifted = lifted.shifted(37, -14);
        let transformed = lifted.transformed(
            &crate::Transform::translation(37.0, -14.0),
            crate::Kernel::Mitchell,
        );
        assert_eq!(
            transformed, shifted,
            "a whole-pixel move should take the copy path, not the filter"
        );
    }

    /// A quarter turn is exact on the grid, so it must move the pixels and not blur them.
    #[test]
    fn a_quarter_turn_moves_pixels_without_softening_them() {
        // An L, so a turn is distinguishable from a flip or a transpose.
        let sel = Selection::from_rect(PageRect::new(20, 20, 8, 24), page());
        let lifted = Lifted::from_layer(&sel, solid);

        let t = crate::Transform {
            pivot: (24.0, 32.0),
            rotation: std::f32::consts::FRAC_PI_2,
            ..crate::Transform::IDENTITY
        };
        let out = lifted.transformed(&t, crate::Kernel::Mitchell);

        // The 8x24 strip about (24, 32) becomes a 24x8 one: reaching about 12px either side of
        // the pivot in x and about 4 in y. Checked well inside those spans rather than at their
        // edges, since the boundary pixel is a matter of half-pixel rounding rather than of the
        // transform being right.
        assert!(
            at(&out, 33, 32)[3] > 0.9,
            "the turned strip should reach well right of the pivot"
        );
        assert!(at(&out, 15, 32)[3] > 0.9, "and well left of it");
        assert!(
            at(&out, 24, 42)[3] < 0.1,
            "and no longer reach far below it"
        );
        assert!(at(&out, 24, 22)[3] < 0.1, "nor far above");
    }

    /// Rotating must not leak a halo into the transparent surround. That is the failure
    /// premultiplied colour exists to prevent, so it is worth pinning rather than assuming.
    #[test]
    fn rotating_leaves_no_halo_around_the_edge() {
        let sel = Selection::from_rect(PageRect::new(30, 30, 40, 40), page());
        // Bright red, so a halo would be obvious in the colour channels.
        let lifted = Lifted::from_layer(&sel, |_| Some(Tile::filled(RED)));

        let t = crate::Transform {
            pivot: (50.0, 50.0),
            rotation: 0.4,
            ..crate::Transform::IDENTITY
        };
        let out = lifted.transformed(&t, crate::Kernel::Mitchell);

        for (coord, tile) in &out {
            for ly in 0..TILE_SIZE {
                for lx in 0..TILE_SIZE {
                    let t = tile.texel(lx, ly);
                    let _ = coord;
                    assert!(
                        t[0] <= t[3] + 1e-3 && t[1] <= t[3] + 1e-3 && t[2] <= t[3] + 1e-3,
                        "a channel exceeded alpha at ({lx}, {ly}): {t:?}"
                    );
                }
            }
        }
    }

    /// Scaling up should cover more page than it started with, and scaling down less. Cheap, and
    /// it catches an inverse transform applied the wrong way round — which otherwise looks
    /// plausible until someone drags a handle.
    #[test]
    fn scaling_changes_how_much_page_is_covered() {
        let sel = Selection::from_rect(PageRect::new(40, 40, 40, 40), page());
        let lifted = Lifted::from_layer(&sel, solid);
        let covered = |t: &crate::Transform| {
            lifted
                .transformed(t, crate::Kernel::Mitchell)
                .values()
                .map(|tile| {
                    (0..TILE_SIZE)
                        .flat_map(|y| (0..TILE_SIZE).map(move |x| (x, y)))
                        .filter(|(x, y)| tile.texel(*x, *y)[3] > 0.5)
                        .count()
                })
                .sum::<usize>()
        };

        let plain = covered(&crate::Transform {
            pivot: (60.0, 60.0),
            ..crate::Transform::IDENTITY
        });
        let bigger = covered(&crate::Transform {
            pivot: (60.0, 60.0),
            scale: (2.0, 2.0),
            ..crate::Transform::IDENTITY
        });
        let smaller = covered(&crate::Transform {
            pivot: (60.0, 60.0),
            scale: (0.5, 0.5),
            ..crate::Transform::IDENTITY
        });

        assert!(
            bigger > plain * 3,
            "doubling should roughly quadruple the area: {plain} -> {bigger}"
        );
        assert!(
            smaller < plain / 3,
            "halving should roughly quarter it: {plain} -> {smaller}"
        );
    }

    /// Transforming nothing produces nothing, rather than an empty tile holding residency.
    #[test]
    fn transforming_nothing_produces_nothing() {
        let lifted = Lifted::default();
        assert!(lifted
            .transformed(
                &crate::Transform {
                    rotation: 0.5,
                    ..crate::Transform::IDENTITY
                },
                crate::Kernel::Mitchell
            )
            .is_empty());
    }

    /// The bounds have to cover every tile the lift touched, since that is what the resampler
    /// reads from.
    /// The tight bounds hug the ink; the tile-aligned ones hug the tiles.
    ///
    /// Both are wanted, so what matters is the case that separates them: a selection sitting
    /// inside one tile must come back as itself, because a resampling loop given the tile spends
    /// four times as long reconstructing transparency as it does on the artwork.
    #[test]
    fn the_tight_bounds_hug_the_ink() {
        let page = PageRect::from_size(2048, 2048);
        let sel = Selection::from_rect(PageRect::new(300, 320, 100, 60), page);
        let lifted = Lifted::from_layer(&sel, solid);

        assert_eq!(lifted.content_bounds(), Some((300, 320, 400, 380)));
        assert_eq!(
            lifted.bounds(),
            Some((256, 256, 512, 512)),
            "the tile-aligned box is the tile it landed in, and that is what it is for"
        );
        assert!(Lifted::default().content_bounds().is_none());
    }

    /// An axis the transform does not touch comes back exactly where it was.
    ///
    /// **This is the test the suite was missing, and a sabotage found the hole.** Shifting the
    /// hoisted filter weights by one column relative to the pixels they weight moves the whole
    /// reconstruction sideways by a pixel — a resample of the wrong thing, not a slightly worse
    /// one — and every existing test passed anyway, because they all measured *what* came out
    /// rather than *where*.
    ///
    /// Stretching vertically leaves x untouched, so the vertical edges of the content must not
    /// move at all. Catmull-Rom because it interpolates: an unmoved pixel comes back exactly,
    /// which is the property that makes an exact assertion honest here.
    #[test]
    fn an_untouched_axis_does_not_drift() {
        let page = PageRect::from_size(1024, 1024);
        let sel = Selection::from_rect(PageRect::new(100, 100, 100, 100), page);
        let lifted = Lifted::from_layer(&sel, solid);

        let out = lifted.transformed(
            &crate::Transform {
                pivot: (150.0, 150.0),
                scale: (1.0, 2.0),
                ..crate::Transform::IDENTITY
            },
            crate::Kernel::CatmullRom,
        );
        let placed = Lifted { tiles: out };

        let (x0, _, x1, _) = placed
            .content_bounds()
            .expect("the stretch produced something");
        assert_eq!(
            (x0, x1),
            (100, 200),
            "a vertical stretch moved the horizontal edges"
        );

        // And the same for the other axis, so a mirrored mistake cannot hide either.
        let out = lifted.transformed(
            &crate::Transform {
                pivot: (150.0, 150.0),
                scale: (2.0, 1.0),
                ..crate::Transform::IDENTITY
            },
            crate::Kernel::CatmullRom,
        );
        let placed = Lifted { tiles: out };
        let (_, y0, _, y1) = placed
            .content_bounds()
            .expect("the stretch produced something");
        assert_eq!(
            (y0, y1),
            (100, 200),
            "a horizontal stretch moved the vertical edges"
        );
    }

    /// A soft edge is part of the content, so the tight bounds have to reach it.
    ///
    /// Pins the definition against a threshold: reading "is there anything here" as "is it mostly
    /// opaque" would trim the anti-aliased fringe off every lasso, and the transform would then
    /// resample a box slightly smaller than the artwork and clip its own edges. Checked against a
    /// brute-force scan rather than against numbers, so the test states the definition rather than
    /// an example of it.
    #[test]
    fn the_tight_bounds_include_the_soft_edge() {
        let page = PageRect::from_size(1024, 1024);
        // A diagonal, so the boundary is anti-aliased rather than pixel-aligned.
        let sel = Selection::from_polygon(&[(200.0, 200.0), (400.0, 260.0), (240.0, 400.0)], page);
        let lifted = Lifted::from_layer(&sel, solid);

        let (tx0, ty0, tx1, ty1) = lifted.bounds().expect("something was lifted");
        let mut scanned: Option<(i32, i32, i32, i32)> = None;
        for y in ty0..ty1 {
            for x in tx0..tx1 {
                let t = lifted.texel_at(x, y);
                if t[3] > 0.0 || t[0] > 0.0 || t[1] > 0.0 || t[2] > 0.0 {
                    scanned = Some(match scanned {
                        None => (x, y, x + 1, y + 1),
                        Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x + 1), d.max(y + 1)),
                    });
                }
            }
        }
        assert_eq!(
            lifted.content_bounds(),
            scanned,
            "the tight bounds must be exactly the texels that are there"
        );
    }

    /// A transform must not take perceptible time, because it runs while the pen is moving.
    ///
    /// A regression test for a shipped defect, not a benchmark — the transform shipped resampling
    /// through a `HashMap` lookup per filter tap, over the tile-aligned bounds, once per pointer
    /// sample. That measured 620 ms for this input, and it read to the user as the tool being
    /// broken rather than slow.
    ///
    /// The bound is deliberately loose: it is not measuring anything, it is trying to be
    /// impossible to pass with the wrong *shape* of loop while being impossible to fail on a slow
    /// machine with the right one. Note it is a debug-profile bound too, which is where this runs
    /// by default.
    #[test]
    fn a_rotation_is_not_slow() {
        let page = PageRect::from_size(2048, 2048);
        let sel = Selection::from_rect(PageRect::new(400, 400, 512, 512), page);
        let lifted = Lifted::from_layer(&sel, solid);
        let transform = crate::Transform {
            pivot: (656.0, 656.0),
            rotation: 0.3,
            scale: (1.2, 1.2),
            ..crate::Transform::IDENTITY
        };

        let started = std::time::Instant::now();
        let out = lifted.transformed(&transform, crate::Kernel::Mitchell);
        let took = started.elapsed();

        assert!(!out.is_empty(), "the rotation produced nothing");
        assert!(
            took < std::time::Duration::from_millis(2500),
            "rotating a 512x512 selection took {took:?}; that is the shape of the loop, not a slow machine"
        );
    }

    #[test]
    fn bounds_cover_every_lifted_tile() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 400, 300), page());
        let lifted = Lifted::from_layer(&sel, solid);
        let (min_x, min_y, max_x, max_y) = lifted.bounds().expect("something was lifted");
        for (coord, _) in lifted.tiles() {
            let (x, y) = (coord.0 * TILE_SIZE as i32, coord.1 * TILE_SIZE as i32);
            assert!(
                x >= min_x && y >= min_y,
                "tile {coord:?} starts before the bounds"
            );
            assert!(
                x + TILE_SIZE as i32 <= max_x && y + TILE_SIZE as i32 <= max_y,
                "tile {coord:?} ends after the bounds"
            );
        }
        assert!(Lifted::default().bounds().is_none());
    }

    /// Reading outside the lifted pixels gives transparency, not the nearest edge. Clamping would
    /// smear the outermost row of a rotated selection outward into a streak.
    #[test]
    fn reading_outside_the_lift_is_transparent() {
        let sel = Selection::from_rect(PageRect::new(10, 10, 20, 20), page());
        let lifted = Lifted::from_layer(&sel, solid);
        assert!(lifted.texel_at(15, 15)[3] > 0.9, "inside");
        assert_eq!(lifted.texel_at(-500, -500), [0.0; 4], "far outside");
        assert_eq!(
            lifted.texel_at(5000, 5000),
            [0.0; 4],
            "far outside the other way"
        );
    }
}
