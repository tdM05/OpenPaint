//! Fixed-size canvas tiles.
//!
//! The canvas is stored as a sparse grid of square tiles. A tile is only
//! allocated once something is painted into it, which is what makes very large
//! (and later, growable/infinite) canvases cheap: empty regions cost nothing.

/// Side length of a tile in pixels. 256 is a good balance between upload
/// granularity and per-tile overhead, and 256*4 = 1024 bytes-per-row satisfies
/// wgpu's 256-byte row-alignment requirement for texture uploads.
pub const TILE_SIZE: usize = 256;

/// Number of bytes in one tile (RGBA8).
pub const TILE_BYTES: usize = TILE_SIZE * TILE_SIZE * 4;

/// Integer tile coordinate in the tile grid (not pixels).
pub type TileCoord = (i32, i32);

/// A single RGBA8 tile. Alpha is currently always opaque (255); the canvas is
/// treated as opaque paper for now. Stored straight (non-premultiplied).
pub struct Tile {
    pixels: Vec<u8>,
}

impl Tile {
    /// Create a transparent-black tile. (Freshly allocated tiles sit on top of
    /// the canvas's paper color, which lives in the paper texture, so an
    /// all-zero tile only appears where the canvas has genuinely been touched.
    /// For the current opaque-paper model we initialize tiles to the paper
    /// color instead — see [`Tile::filled`].)
    pub fn new() -> Self {
        Self {
            pixels: vec![0u8; TILE_BYTES],
        }
    }

    /// Create a tile filled with a solid opaque color.
    pub fn filled(color: [u8; 3]) -> Self {
        let rgba = [color[0], color[1], color[2], 255];
        // repeat() the 4-byte pattern across the whole tile — clean and avoids
        // per-pixel chunk iteration.
        let pixels = rgba.repeat(TILE_SIZE * TILE_SIZE);
        debug_assert_eq!(pixels.len(), TILE_BYTES);
        Self { pixels }
    }

    /// Raw pixel bytes, row-major RGBA8, `TILE_SIZE` wide.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Mutable access to one pixel's 4 bytes at local `(x, y)`.
    #[inline]
    pub fn pixel_mut(&mut self, x: usize, y: usize) -> &mut [u8] {
        let i = (y * TILE_SIZE + x) * 4;
        &mut self.pixels[i..i + 4]
    }
}

impl Default for Tile {
    fn default() -> Self {
        Self::new()
    }
}
