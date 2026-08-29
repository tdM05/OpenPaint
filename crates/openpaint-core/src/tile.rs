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
///
/// `Clone` because strokes snapshot the tiles they touch before painting, so the
/// stroke can be re-composited as it builds up (see [`crate::stroke`]). That same
/// snapshot is what undo needs (OPEN_QUESTIONS Q13).
///
/// `PartialEq` and `Debug` are for tests: comparing whole tiles is how a transform proves it moved
/// pixels rather than approximately moved them, and `Debug` prints the texel buffer, which is
/// enormous -- so it is for assertion messages about *whether* tiles differ, never for logging one.
#[derive(Clone, Debug, PartialEq)]
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

    /// Build a tile from raw bytes in the same layout [`Tile::bytes`] produces.
    ///
    /// The inverse of `bytes`, for pixels arriving from somewhere else: a tile read back off
    /// the GPU when residency spills it to the CPU, and eventually a tile read from a file.
    /// Returns `None` on a wrong-sized input rather than panicking, because both of those
    /// sources are external and a length mismatch is data, not a bug.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != TILE_BYTES {
            return None;
        }
        Some(Self {
            texels: bytemuck::cast_slice(bytes).to_vec(),
        })
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

/// A dense rectangular index over a range of tile coordinates.
///
/// Exists to keep hashing out of inner loops. A tile map is naturally a `HashMap`, and that is the
/// right shape for a sparse canvas — but a loop that reads twenty source pixels per output pixel
/// pays for a hash twenty times, and resampling measured at 620 ms per pointer sample before this
/// existed, almost all of it hashing rather than filtering.
///
/// The range is the tiles a single operation touches, so the grid is a handful of entries wide.
/// It is an *index*, not storage: the caller keeps a `Vec` of whatever it wants per tile and looks
/// it up by the index returned here.
#[derive(Clone, Copy, Debug)]
pub struct TileGrid {
    origin: TileCoord,
    /// Width and height in tiles.
    size: (usize, usize),
}

impl TileGrid {
    /// A grid over the tiles covering a **tile-aligned** page rectangle, maxima exclusive.
    #[must_use]
    pub fn over(min_x: i32, min_y: i32, max_x: i32, max_y: i32) -> Self {
        Self::covering((min_x, min_y, max_x, max_y))
    }

    /// A grid over the tiles any part of a page rectangle falls in, maxima exclusive.
    ///
    /// Tolerates an empty or inverted rectangle by describing no tiles at all, so a caller does
    /// not have to check first — `locate` then answers `None` everywhere, which is the honest
    /// answer rather than a panic.
    #[must_use]
    pub fn covering((min_x, min_y, max_x, max_y): (i32, i32, i32, i32)) -> Self {
        if max_x <= min_x || max_y <= min_y {
            return Self {
                origin: (0, 0),
                size: (0, 0),
            };
        }
        let side = TILE_SIZE as i32;
        let origin = (min_x.div_euclid(side), min_y.div_euclid(side));
        let last = ((max_x - 1).div_euclid(side), (max_y - 1).div_euclid(side));
        #[expect(
            clippy::cast_sign_loss,
            reason = "`last` is at or after `origin` by construction"
        )]
        let size = (
            (last.0 - origin.0 + 1) as usize,
            (last.1 - origin.1 + 1) as usize,
        );
        Self { origin, size }
    }

    /// How many tiles the grid describes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.size.0 * self.size.1
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The index of a tile coordinate, or `None` if it is outside the grid.
    #[must_use]
    pub fn index_of(&self, coord: TileCoord) -> Option<usize> {
        let (dx, dy) = (coord.0 - self.origin.0, coord.1 - self.origin.1);
        #[expect(clippy::cast_possible_wrap, reason = "grid extents are a few tiles")]
        if dx < 0 || dy < 0 || dx >= self.size.0 as i32 || dy >= self.size.1 as i32 {
            return None;
        }
        #[expect(clippy::cast_sign_loss, reason = "checked non-negative just above")]
        Some(dy as usize * self.size.0 + dx as usize)
    }

    /// Which tile a page pixel is in, and where inside it.
    ///
    /// The local coordinates are returned even when the tile is outside the grid, because they are
    /// correct regardless and the caller that wants them has already paid for the division.
    #[must_use]
    pub fn locate(&self, x: i32, y: i32) -> (Option<usize>, usize, usize) {
        let side = TILE_SIZE as i32;
        let coord = (x.div_euclid(side), y.div_euclid(side));
        let (lx, ly) = (x.rem_euclid(side) as usize, y.rem_euclid(side) as usize);
        (self.index_of(coord), lx, ly)
    }

    /// The tile coordinate at an index.
    #[must_use]
    pub fn coord_at(&self, index: usize) -> TileCoord {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            reason = "grid extents are a few tiles"
        )]
        (
            self.origin.0 + (index % self.size.0) as i32,
            self.origin.1 + (index / self.size.0) as i32,
        )
    }

    /// Turn a grid of optional values back into the sparse map the rest of the engine speaks.
    ///
    /// Absent entries are dropped rather than stored empty: a tile that nothing was written to is
    /// a tile that does not exist, and materialising it would make an empty transform look like a
    /// full one to everything downstream.
    #[must_use]
    pub fn collect<T>(&self, values: Vec<Option<T>>) -> std::collections::HashMap<TileCoord, T> {
        values
            .into_iter()
            .enumerate()
            .filter_map(|(i, v)| v.map(|v| (self.coord_at(i), v)))
            .collect()
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
