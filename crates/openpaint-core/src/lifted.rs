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
use crate::tile::{Tile, TileCoord, TILE_SIZE};

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
}
