//! A tiled raster canvas.
//!
//! Holds a sparse set of [`Tile`]s addressed by tile coordinate. Painting
//! happens in canvas pixel space; the canvas figures out which tiles are
//! touched, allocating them on demand and recording them as dirty so the
//! renderer can re-upload only what changed.

use std::collections::{HashMap, HashSet};

use crate::tile::{Tile, TileCoord, TILE_SIZE};

/// Default paper color for freshly allocated tiles (near-white).
const PAPER: [u8; 3] = [250, 249, 246];

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
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            tiles: HashMap::new(),
            dirty: HashSet::new(),
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Paint a single canvas pixel by alpha-compositing `color` over whatever
    /// is there, with coverage `alpha` in `0.0..=1.0`. Straight-alpha,
    /// sRGB-space blend for now — good enough for the Phase 0 slice; we move to
    /// proper linear-space compositing when the real brush engine lands.
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: [u8; 3], alpha: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let a = alpha.clamp(0.0, 1.0);
        if a <= 0.0 {
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
            .or_insert_with(|| Tile::filled(PAPER));

        let px = tile.pixel_mut(lx, ly);
        for c in 0..3 {
            let dst = px[c] as f32;
            let src = color[c] as f32;
            px[c] = (src * a + dst * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
        }
        px[3] = 255;

        self.dirty.insert(tile_coord);
    }

    /// Iterate all currently allocated tiles (coord + tile).
    pub fn tiles(&self) -> impl Iterator<Item = (&TileCoord, &Tile)> {
        self.tiles.iter()
    }

    /// Look up one tile if allocated.
    pub fn tile(&self, coord: TileCoord) -> Option<&Tile> {
        self.tiles.get(&coord)
    }

    /// Take and clear the set of dirty tile coords since the last call.
    pub fn take_dirty(&mut self) -> HashSet<TileCoord> {
        std::mem::take(&mut self.dirty)
    }

    /// The paper color unpainted canvas should read as. The renderer clears
    /// tile-free area to this so the whole canvas looks like one sheet.
    pub fn paper_color(&self) -> [u8; 3] {
        PAPER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painting_allocates_only_touched_tiles() {
        let mut c = Canvas::new(2048, 2048);
        assert_eq!(c.tiles().count(), 0);
        c.blend_pixel(10, 10, [0, 0, 0], 1.0);
        c.blend_pixel(300, 10, [0, 0, 0], 1.0); // different tile column
        assert_eq!(c.tiles().count(), 2);
    }

    #[test]
    fn out_of_bounds_paints_nothing() {
        let mut c = Canvas::new(256, 256);
        c.blend_pixel(-5, 10, [0, 0, 0], 1.0);
        c.blend_pixel(999, 10, [0, 0, 0], 1.0);
        assert_eq!(c.tiles().count(), 0);
    }

    #[test]
    fn dirty_tracks_and_clears() {
        let mut c = Canvas::new(512, 512);
        c.blend_pixel(5, 5, [0, 0, 0], 1.0);
        assert_eq!(c.take_dirty().len(), 1);
        assert_eq!(c.take_dirty().len(), 0);
    }
}
