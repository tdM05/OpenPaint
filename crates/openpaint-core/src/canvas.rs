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
}
