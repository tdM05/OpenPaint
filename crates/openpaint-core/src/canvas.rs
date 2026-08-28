//! A tiled raster canvas.
//!
//! Holds a sparse set of [`Tile`]s addressed by tile coordinate. Painting
//! happens in canvas pixel space; the canvas figures out which tiles are
//! touched, allocating them on demand and recording them as dirty so the
//! renderer can re-upload only what changed.
//!
//! Pixels are **linear and premultiplied** throughout — see [`crate::color`].
//!
//! # This is the reference implementation, not the fast path
//!
//! Per `docs/DECISIONS.md` §4a, dab rasterization moves to the GPU: the core
//! decides *where* dabs go and *what* they look like, the renderer stamps them.
//! This per-pixel CPU path stays as the reference the GPU output is tested
//! against, which is what makes chasing Photoshop's falloff curve verifiable
//! rather than a matter of opinion. Do not optimize it at the cost of clarity.

use std::collections::{HashMap, HashSet};

use crate::color::{opaque_srgb8_to_linear_premul, over_premul};
use crate::page::Anchor;
use crate::tile::{Tile, TileCoord, TILE_SIZE};

/// Default paper color for freshly allocated tiles, as authored (near-white sRGB).
const PAPER_SRGB: [u8; 3] = [250, 249, 246];

/// A fixed-dimension tiled canvas. Dimensions are stored for bounds/clamping,
/// but tiles are sparse, so unpainted area costs nothing.
pub struct Canvas {
    width: u32,
    height: u32,
    tiles: HashMap<TileCoord, Tile>,
    /// Tiles modified since the last `take_dirty` — the renderer's re-upload set.
    dirty: HashSet<TileCoord>,
}

impl Canvas {
    /// Create an empty canvas of the given pixel dimensions.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            tiles: HashMap::new(),
            dirty: HashSet::new(),
        }
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Composite a linear premultiplied `src` over one canvas pixel.
    ///
    /// `src` must already have coverage folded in (see
    /// [`crate::color::scale_premul`]) — premultiplied alpha means coverage
    /// scales all four channels, so folding it in at the call site keeps this a
    /// plain Porter-Duff "over" with no special cases.
    pub fn blend_pixel(&mut self, x: i32, y: i32, src_linear_premul: [f32; 4]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        // Fully transparent source cannot change the destination.
        if src_linear_premul[3] <= 0.0 {
            return;
        }

        let tile_coord = (
            x.div_euclid(TILE_SIZE as i32),
            y.div_euclid(TILE_SIZE as i32),
        );
        let lx = x.rem_euclid(TILE_SIZE as i32) as usize;
        let ly = y.rem_euclid(TILE_SIZE as i32) as usize;

        let tile = self
            .tiles
            .entry(tile_coord)
            .or_insert_with(|| Tile::filled(Self::paper_color()));

        let dst = tile.texel(lx, ly);
        tile.set_texel(lx, ly, over_premul(src_linear_premul, dst));

        self.dirty.insert(tile_coord);
    }

    /// Overwrite one canvas pixel outright, allocating its tile if needed.
    ///
    /// Unlike [`Canvas::blend_pixel`] this does not composite. It exists for
    /// [`crate::stroke`], which *recomputes* a pixel from its pre-stroke snapshot
    /// every update rather than darkening it progressively — that recomputation is
    /// what keeps a mid-stroke preview identical to the committed result.
    pub fn replace_pixel(&mut self, x: i32, y: i32, linear_premul: [f32; 4]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let tile_coord = (
            x.div_euclid(TILE_SIZE as i32),
            y.div_euclid(TILE_SIZE as i32),
        );
        let lx = x.rem_euclid(TILE_SIZE as i32) as usize;
        let ly = y.rem_euclid(TILE_SIZE as i32) as usize;

        let tile = self
            .tiles
            .entry(tile_coord)
            .or_insert_with(|| Tile::filled(Self::paper_color()));
        tile.set_texel(lx, ly, linear_premul);
        self.dirty.insert(tile_coord);
    }

    /// Change the canvas size, keeping existing content at `anchor`.
    ///
    /// Returns how far content moved, in pixels. Callers need that: GPU textures
    /// copy their old contents to the same offset, and anything storing canvas
    /// coordinates (undo rectangles) must be shifted by it.
    ///
    /// Pixels genuinely move here rather than tile keys being remapped, because the
    /// shift is rarely a multiple of the tile size — extending up by 500 with 256px
    /// tiles offsets content by 500, so each destination tile draws from two source
    /// tiles. Doing it per pixel is obviously correct and this is the reference
    /// implementation (clarity over speed); the app's real pixels live on the GPU,
    /// where the same operation is a single texture copy.
    pub fn resize(&mut self, new_w: u32, new_h: u32, anchor: Anchor) -> (i32, i32) {
        let (dx, dy) = anchor.offset(self.width, self.height, new_w, new_h);
        self.width = new_w.max(1);
        self.height = new_h.max(1);

        // Nothing painted, or nothing to move: just the new bounds.
        if self.tiles.is_empty() {
            self.dirty.clear();
            return (dx, dy);
        }

        let old = std::mem::take(&mut self.tiles);
        self.dirty.clear();
        for (coord, tile) in old {
            for ly in 0..TILE_SIZE {
                for lx in 0..TILE_SIZE {
                    let src_x = coord.0 * TILE_SIZE as i32 + lx as i32;
                    let src_y = coord.1 * TILE_SIZE as i32 + ly as i32;
                    // replace_pixel clips to the new bounds, so content shifted or
                    // cropped outside is dropped -- which is what a crop means.
                    self.replace_pixel(src_x + dx, src_y + dy, tile.texel(lx, ly));
                }
            }
        }
        (dx, dy)
    }

    /// Iterate all currently allocated tiles (coord + tile).
    pub fn tiles(&self) -> impl Iterator<Item = (&TileCoord, &Tile)> {
        self.tiles.iter()
    }

    /// Look up one tile if allocated.
    #[must_use]
    pub fn tile(&self, coord: TileCoord) -> Option<&Tile> {
        self.tiles.get(&coord)
    }

    /// Take and clear the set of dirty tile coords since the last call.
    pub fn take_dirty(&mut self) -> HashSet<TileCoord> {
        std::mem::take(&mut self.dirty)
    }

    /// The paper color unpainted canvas should read as, linear and premultiplied.
    /// The renderer clears tile-free area to this so the whole canvas looks like
    /// one sheet.
    #[must_use]
    pub fn paper_color() -> [f32; 4] {
        opaque_srgb8_to_linear_premul(PAPER_SRGB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::scale_premul;

    /// Opaque black, ready to blend.
    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    #[test]
    fn painting_allocates_only_touched_tiles() {
        let mut c = Canvas::new(2048, 2048);
        assert_eq!(c.tiles().count(), 0);
        c.blend_pixel(10, 10, BLACK);
        c.blend_pixel(300, 10, BLACK); // different tile column
        assert_eq!(c.tiles().count(), 2);
    }

    #[test]
    fn out_of_bounds_paints_nothing() {
        let mut c = Canvas::new(256, 256);
        c.blend_pixel(-5, 10, BLACK);
        c.blend_pixel(999, 10, BLACK);
        assert_eq!(c.tiles().count(), 0);
    }

    #[test]
    fn fully_transparent_source_allocates_nothing() {
        let mut c = Canvas::new(256, 256);
        c.blend_pixel(10, 10, [0.0; 4]);
        assert_eq!(c.tiles().count(), 0);
    }

    #[test]
    fn dirty_tracks_and_clears() {
        let mut c = Canvas::new(512, 512);
        c.blend_pixel(5, 5, BLACK);
        assert_eq!(c.take_dirty().len(), 1);
        assert_eq!(c.take_dirty().len(), 0);
    }

    #[test]
    fn fresh_tiles_start_at_paper_color() {
        let mut c = Canvas::new(512, 512);
        // Touch one pixel to force allocation, then inspect an untouched one.
        c.blend_pixel(0, 0, BLACK);
        let tile = c.tile((0, 0)).expect("tile allocated");
        assert_eq!(tile.texel(200, 200), Canvas::paper_color().map(round_f16));
    }

    #[test]
    fn opaque_paint_replaces_the_pixel() {
        let mut c = Canvas::new(512, 512);
        c.blend_pixel(4, 4, BLACK);
        let tile = c.tile((0, 0)).expect("tile allocated");
        assert_eq!(tile.texel(4, 4), [0.0, 0.0, 0.0, 1.0]);
    }

    /// Half coverage of black over paper must land halfway in *linear* space.
    /// If someone reintroduces sRGB-space blending, this test fails.
    #[test]
    fn half_coverage_blends_in_linear_space() {
        let mut c = Canvas::new(512, 512);
        c.blend_pixel(4, 4, scale_premul(BLACK, 0.5));
        let paper = Canvas::paper_color();
        let got = c.tile((0, 0)).expect("tile allocated").texel(4, 4);
        let expected = paper[0] * 0.5;
        assert!(
            (got[0] - expected).abs() < 1e-3,
            "got {}, expected {expected}",
            got[0]
        );
        assert!((got[3] - 1.0).abs() < 1e-3);
    }

    /// Quantize an f32 the way storing it in an f16 tile would, so expectations
    /// don't fail on representation error.
    fn round_f16(v: f32) -> f32 {
        half::f16::from_f32(v).to_f32()
    }

    /// Extending down must leave every painted pixel exactly where it was.
    #[test]
    fn extending_down_preserves_content_in_place() {
        let mut c = Canvas::new(300, 300);
        c.blend_pixel(10, 20, BLACK);
        c.blend_pixel(250, 290, BLACK);

        let moved = c.resize(300, 900, Anchor::TOP_LEFT);
        assert_eq!(moved, (0, 0));
        assert_eq!((c.width(), c.height()), (300, 900));
        assert!(is_paint(&c, 10, 20), "pixel at (10,20) lost");
        assert!(is_paint(&c, 250, 290), "pixel at (250,290) lost");
    }

    /// Extending up shifts content down by the growth, and the shift is *not* a
    /// multiple of the tile size here -- which is the case that would break a
    /// tile-key remap.
    #[test]
    fn extending_up_shifts_content_by_a_non_tile_multiple() {
        let mut c = Canvas::new(300, 300);
        c.blend_pixel(10, 20, BLACK);

        // Compile-time guard: this test only proves anything while the shift is
        // NOT a whole number of tiles. If TILE_SIZE ever divides 500, the build
        // breaks here rather than the test quietly becoming vacuous.
        const _: () = assert!(500 % TILE_SIZE != 0);

        let moved = c.resize(300, 800, Anchor::BOTTOM_LEFT);
        assert_eq!(moved, (0, 500));
        assert!(is_paint(&c, 10, 520), "content did not follow the shift");
        assert!(
            !is_paint(&c, 10, 20),
            "content left behind at the old position"
        );
    }

    #[test]
    fn extending_left_shifts_content_right() {
        let mut c = Canvas::new(300, 300);
        c.blend_pixel(10, 20, BLACK);
        let moved = c.resize(700, 300, Anchor::TOP_RIGHT);
        assert_eq!(moved, (400, 0));
        assert!(is_paint(&c, 410, 20));
    }

    /// Shrinking drops what falls outside -- that is what a crop is.
    #[test]
    fn shrinking_crops_content_outside_the_new_bounds() {
        let mut c = Canvas::new(600, 600);
        c.blend_pixel(50, 50, BLACK);
        c.blend_pixel(500, 500, BLACK);

        c.resize(200, 200, Anchor::TOP_LEFT);
        assert!(is_paint(&c, 50, 50), "in-bounds content should survive");
        assert_eq!(c.width(), 200);
        // The far pixel is outside the new bounds and simply gone.
        assert!(c.tile((1, 1)).is_none() && c.tile((2, 2)).is_none());
    }

    #[test]
    fn resizing_an_untouched_canvas_just_changes_the_bounds() {
        let mut c = Canvas::new(100, 100);
        let moved = c.resize(100, 500, Anchor::TOP_LEFT);
        assert_eq!(moved, (0, 0));
        assert_eq!(c.tiles().count(), 0, "should not have allocated anything");
        assert_eq!(c.height(), 500);
    }

    /// Painting must work in the newly-added space, which is the whole point of
    /// extending.
    #[test]
    fn the_new_space_is_paintable() {
        let mut c = Canvas::new(100, 100);
        c.resize(100, 600, Anchor::TOP_LEFT);
        c.blend_pixel(50, 550, BLACK);
        assert!(
            is_paint(&c, 50, 550),
            "could not paint in the extended area"
        );
    }

    #[test]
    fn a_zero_size_resize_is_clamped_rather_than_panicking() {
        let mut c = Canvas::new(100, 100);
        c.resize(0, 0, Anchor::TOP_LEFT);
        assert!(c.width() >= 1 && c.height() >= 1);
    }

    /// Whether a pixel carries paint, i.e. differs from bare paper.
    fn is_paint(c: &Canvas, x: u32, y: u32) -> bool {
        let coord = ((x / TILE_SIZE as u32) as i32, (y / TILE_SIZE as u32) as i32);
        let Some(tile) = c.tile(coord) else {
            return false;
        };
        let lx = (x % TILE_SIZE as u32) as usize;
        let ly = (y % TILE_SIZE as u32) as usize;
        let paper = Canvas::paper_color()[0];
        tile.texel(lx, ly)[0] < paper - 0.05
    }
}
