//! Fixed-size canvas tiles.
//!
//! The canvas is stored as a sparse grid of square tiles. A tile is only
//! allocated once something is painted into it, which is what makes very large
//! (and later, growable/infinite) canvases cheap: empty regions cost nothing.
//!
//! # Format: linear, premultiplied, RGBA f16
//!
//! Per `docs/DECISIONS.md` §4b. `f16` rather than `f32` because it halves memory
//! and matches the GPU's `Rgba16Float` exactly — the tile bytes upload with no
//! conversion, and the CPU reference implementation and the GPU rasterizer are
//! then comparable bit-for-bit in tests. `f16` rather than 8-bit because 8 bits
//! of *linear* precision bands visibly in shadows, and because a compositor
//! wants headroom above 1.0 for HDR-ish blend modes later.
//!
//! Memory matters here: at 8 bytes/texel a 256×256 tile is 512 KiB, and the
//! target is a Surface-class integrated GPU sharing system memory (DECISIONS
//! §2). That is exactly why the GPU only ever holds a bounded working set of
//! tiles rather than a whole document — see OPEN_QUESTIONS Q13.

use half::f16;

/// Side length of a tile in pixels.
///
/// 256 balances upload granularity against per-tile overhead, and at 8 bytes per
/// texel a row is 2048 bytes — comfortably a multiple of wgpu's 256-byte
/// `bytes_per_row` alignment requirement, so uploads need no padding.
pub const TILE_SIZE: usize = 256;

/// Channels per texel (RGBA).
pub const TILE_CHANNELS: usize = 4;

/// Number of texels in one tile.
pub const TILE_TEXELS: usize = TILE_SIZE * TILE_SIZE;

/// Number of `f16` values in one tile.
pub const TILE_VALUES: usize = TILE_TEXELS * TILE_CHANNELS;

/// Number of bytes in one tile (RGBA f16).
pub const TILE_BYTES: usize = TILE_VALUES * 2;

/// Integer tile coordinate in the tile grid (not pixels).
pub type TileCoord = (i32, i32);

/// A single tile of linear, premultiplied RGBA `f16` texels.
pub struct Tile {
    /// Row-major, `TILE_SIZE` wide, 4 values per texel.
    texels: Vec<f16>,
}

impl Tile {
    /// Create a fully transparent tile.
    ///
    /// All-zero is genuinely transparent under premultiplied alpha (unlike
    /// straight alpha, where the color channels would be meaningless), so this
    /// is both correct and the cheapest possible allocation.
    #[must_use]
    pub fn transparent() -> Self {
        Self {
            texels: vec![f16::ZERO; TILE_VALUES],
        }
    }

    /// Create a tile filled with a solid linear premultiplied RGBA color.
    #[must_use]
    pub fn filled(rgba_linear_premul: [f32; 4]) -> Self {
        let texel = [
            f16::from_f32(rgba_linear_premul[0]),
            f16::from_f32(rgba_linear_premul[1]),
            f16::from_f32(rgba_linear_premul[2]),
            f16::from_f32(rgba_linear_premul[3]),
        ];
        Self {
            texels: texel.repeat(TILE_TEXELS),
        }
    }

    /// Raw tile bytes, ready to upload to an `Rgba16Float` texture.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.texels)
    }

    /// Read one texel at local `(x, y)` as linear premultiplied `f32`.
    #[must_use]
    pub fn texel(&self, x: usize, y: usize) -> [f32; 4] {
        let i = (y * TILE_SIZE + x) * TILE_CHANNELS;
        [
            self.texels[i].to_f32(),
            self.texels[i + 1].to_f32(),
            self.texels[i + 2].to_f32(),
            self.texels[i + 3].to_f32(),
        ]
    }

    /// Write one texel at local `(x, y)` from linear premultiplied `f32`.
    #[inline]
    pub fn set_texel(&mut self, x: usize, y: usize, rgba_linear_premul: [f32; 4]) {
        let i = (y * TILE_SIZE + x) * TILE_CHANNELS;
        for (dst, src) in self.texels[i..i + TILE_CHANNELS]
            .iter_mut()
            .zip(rgba_linear_premul)
        {
            *dst = f16::from_f32(src);
        }
    }
}

impl Default for Tile {
    fn default() -> Self {
        Self::transparent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_length_matches_gpu_expectation() {
        // 256 * 256 texels * 4 channels * 2 bytes.
        assert_eq!(TILE_BYTES, 512 * 1024);
        assert_eq!(Tile::transparent().bytes().len(), TILE_BYTES);
    }

    /// wgpu requires `bytes_per_row` to be a multiple of 256 for texture
    /// uploads. If the tile size or format ever changes, this catches it.
    #[test]
    fn row_stride_satisfies_wgpu_alignment() {
        let bytes_per_row = TILE_SIZE * TILE_CHANNELS * 2;
        assert_eq!(bytes_per_row % 256, 0);
    }

    #[test]
    fn transparent_tile_is_all_zero() {
        let t = Tile::transparent();
        assert_eq!(t.texel(0, 0), [0.0; 4]);
        assert_eq!(t.texel(TILE_SIZE - 1, TILE_SIZE - 1), [0.0; 4]);
    }

    #[test]
    fn texel_roundtrips_through_f16() {
        let mut t = Tile::transparent();
        t.set_texel(3, 7, [0.25, 0.5, 0.75, 1.0]);
        let got = t.texel(3, 7);
        // f16 represents these exactly; neighbors must stay untouched.
        assert_eq!(got, [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(t.texel(4, 7), [0.0; 4]);
    }

    #[test]
    fn filled_covers_every_texel() {
        let t = Tile::filled([0.5, 0.25, 0.125, 1.0]);
        assert_eq!(t.texel(0, 0), [0.5, 0.25, 0.125, 1.0]);
        assert_eq!(
            t.texel(TILE_SIZE - 1, TILE_SIZE - 1),
            [0.5, 0.25, 0.125, 1.0]
        );
    }
}
