//! A tiled raster canvas.
//!
//! Holds a sparse set of [`Tile`]s addressed by tile coordinate. Painting
//! happens in canvas pixel space; the canvas figures out which tiles are
//! touched, allocating them on demand and recording them as dirty so the
//! renderer can re-upload only what changed.
//!
//! # Coordinates are stable, and the origin may be negative
//!
//! A canvas is a **rectangle placed in a signed coordinate space**: an origin plus an
//! extent, not a `w × h` grid anchored at (0, 0). That gives one invariant worth
//! stating loudly:
//!
//! > **A pixel you painted keeps its coordinate forever.**
//!
//! Extending to the left or upward moves the *origin*, never the content. The
//! alternative — re-basing every coordinate at zero — makes extending left shift every
//! pixel's x, and that instability then leaks outward: the camera has to compensate so
//! the drawing doesn't appear to lurch, undo rectangles have to be rewritten, and
//! every future consumer of coordinates needs the same correction. Those corrections
//! are all symptoms of the coordinate choice, so the choice is what to fix.
//!
//! Tile coordinates are `i32` and the arithmetic uses `div_euclid`/`rem_euclid`, which
//! are correct for negatives, so this costs nothing here. It costs exactly one
//! subtraction at the GPU boundary, because a texture is always zero-based — and that
//! mapping belongs to the renderer.
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
use crate::page::PageRect;
use crate::tile::{Tile, TileCoord, TILE_SIZE};

/// Default paper color for freshly allocated tiles, as authored (near-white sRGB).
const PAPER_SRGB: [u8; 3] = [250, 249, 246];

/// A tiled canvas: a rectangle in a signed coordinate space. Tiles are sparse, so
/// unpainted area costs nothing.
pub struct Canvas {
    /// Top-left corner in canvas coordinates. May be negative, and moves when the
    /// canvas is extended leftward or upward.
    origin_x: i32,
    origin_y: i32,
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
            origin_x: 0,
            origin_y: 0,
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

    /// Top-left corner in canvas coordinates.
    #[must_use]
    pub fn origin(&self) -> (i32, i32) {
        (self.origin_x, self.origin_y)
    }

    /// One past the bottom-right corner, in canvas coordinates.
    #[must_use]
    pub fn end(&self) -> (i32, i32) {
        (
            self.origin_x + self.width as i32,
            self.origin_y + self.height as i32,
        )
    }

    /// The canvas rectangle in canvas coordinates.
    #[must_use]
    pub fn rect(&self) -> PageRect {
        PageRect::new(self.origin_x, self.origin_y, self.width, self.height)
    }

    /// Whether a canvas coordinate lies inside the canvas.
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.rect().contains(x, y)
    }

    /// Composite a linear premultiplied `src` over one canvas pixel.
    ///
    /// `src` must already have coverage folded in (see
    /// [`crate::color::scale_premul`]) — premultiplied alpha means coverage
    /// scales all four channels, so folding it in at the call site keeps this a
    /// plain Porter-Duff "over" with no special cases.
    pub fn blend_pixel(&mut self, x: i32, y: i32, src_linear_premul: [f32; 4]) {
        if !self.contains(x, y) {
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
        if !self.contains(x, y) {
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

    /// Move the canvas to a new rectangle, keeping existing content where it is.
    ///
    /// Returns how far the **origin** moved. Content does not move at all — that is the
    /// point of a signed origin (see the module note). Callers need the origin delta
    /// because a GPU texture is zero-based, so its contents must be copied to a new
    /// position within the new texture.
    ///
    /// **Nothing is destroyed.** Tiles that fall outside the new rectangle are kept, which
    /// is what makes a crop non-destructive (`docs/DECISIONS.md` §5c): undo is LIFO, so it
    /// cannot recover a crop noticed an hour later, and that makes discarding here a way to
    /// lose work permanently. Painting still stops at the page edge — [`Canvas::contains`]
    /// is unchanged — so retained pixels are visible again only if the page is extended
    /// back out. [`Canvas::trim`] is the one operation that discards them, on purpose.
    ///
    /// No pixel is touched, no tile is rekeyed, and no tile is dropped. A resize is a few
    /// integers.
    pub fn resize(&mut self, rect: PageRect) -> (i32, i32) {
        let shift = (rect.x - self.origin_x, rect.y - self.origin_y);
        self.origin_x = rect.x;
        self.origin_y = rect.y;
        self.width = rect.w;
        self.height = rect.h;
        shift
    }

    /// Discard every tile lying entirely outside the canvas, returning how many went.
    ///
    /// The **only** operation that destroys pixels, which is why it is explicit and separate
    /// from [`Canvas::resize`]. A partially covered tile stays: part of it is still inside,
    /// and `contains` already keeps the outside part unpaintable.
    pub fn trim(&mut self) -> usize {
        let before = self.tiles.len();
        let (ox, oy) = (self.origin_x, self.origin_y);
        let (ex, ey) = self.end();
        let tile = TILE_SIZE as i32;
        self.tiles.retain(|coord, _| {
            let x0 = coord.0 * tile;
            let y0 = coord.1 * tile;
            x0 + tile > ox && y0 + tile > oy && x0 < ex && y0 < ey
        });
        before - self.tiles.len()
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

    /// How many tiles are waiting to be uploaded.
    ///
    /// Lets the renderer skip the upload path without taking the set, which matters because
    /// taking it clears it: a caller that took it and then decided not to upload would lose
    /// the tiles silently.
    #[must_use]
    pub fn dirty_count(&self) -> usize {
        self.dirty.len()
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

        let moved = c.resize(PageRect::new(0, 0, 300, 900));
        assert_eq!(moved, (0, 0));
        assert_eq!((c.width(), c.height()), (300, 900));
        assert!(is_paint(&c, 10, 20), "pixel at (10,20) lost");
        assert!(is_paint(&c, 250, 290), "pixel at (250,290) lost");
    }

    /// The invariant that makes this design worth having: extending upward must leave
    /// painted content on exactly the coordinates it already had. Only the origin
    /// moves.
    ///
    /// The old design re-based at zero, so this same operation moved every pixel down
    /// by 500 -- which then forced the camera to compensate and undo rectangles to be
    /// rewritten. Both of those disappeared with this.
    #[test]
    fn extending_up_leaves_content_exactly_where_it_was() {
        let mut c = Canvas::new(300, 300);
        c.blend_pixel(10, 20, BLACK);

        let shift = c.resize(PageRect::new(0, -500, 300, 800));
        assert_eq!(shift, (0, -500), "the origin should move, not the content");
        assert_eq!(c.origin(), (0, -500));
        assert!(
            is_paint(&c, 10, 20),
            "content moved when it should not have"
        );
    }

    /// The space added above the old top edge has *negative* y, and must be paintable
    /// -- otherwise extending upward would add unusable room.
    #[test]
    fn the_space_added_above_has_negative_coordinates_and_is_paintable() {
        let mut c = Canvas::new(300, 300);
        c.resize(PageRect::new(0, -500, 300, 800));

        assert!(
            c.contains(10, -400),
            "negative y should be inside the canvas"
        );
        c.blend_pixel(10, -400, BLACK);
        assert!(
            is_paint(&c, 10, -400),
            "could not paint above the old top edge"
        );

        // Still bounded: just past the new origin is outside.
        assert!(!c.contains(10, -501));
    }

    #[test]
    fn extending_left_leaves_content_exactly_where_it_was() {
        let mut c = Canvas::new(300, 300);
        c.blend_pixel(10, 20, BLACK);

        let shift = c.resize(PageRect::new(-400, 0, 700, 300));
        assert_eq!(shift, (-400, 0));
        assert_eq!(c.origin(), (-400, 0));
        assert!(is_paint(&c, 10, 20));
        assert!(c.contains(-399, 20), "the added space should be paintable");
    }

    /// Shrinking hides what falls outside but **keeps** it.
    ///
    /// This is the property that makes a crop safe (DECISIONS §5c). Undo is LIFO, so it
    /// cannot recover a crop the artist notices an hour later — dropping the tiles here
    /// would be a way to lose hours of work with no route back.
    #[test]
    fn shrinking_hides_content_outside_the_new_bounds_but_keeps_it() {
        let mut c = Canvas::new(600, 600);
        c.blend_pixel(50, 50, BLACK);
        c.blend_pixel(500, 500, BLACK);

        c.resize(PageRect::new(0, 0, 200, 200));
        assert!(is_paint(&c, 50, 50), "in-bounds content should survive");
        assert_eq!(c.width(), 200);
        assert!(
            c.tile((1, 1)).is_some(),
            "the cropped-away tile was destroyed"
        );
        // ...but it is outside the page, so it cannot be painted on.
        assert!(!c.contains(500, 500));

        // Extending back out brings it into view again, unharmed.
        c.resize(PageRect::new(0, 0, 600, 600));
        assert!(is_paint(&c, 500, 500), "content did not come back");
    }

    /// Trim is the one operation that discards pixels, so it has to actually discard them --
    /// otherwise the memory it exists to reclaim is never reclaimed.
    #[test]
    fn trim_discards_only_what_is_wholly_outside() {
        let mut c = Canvas::new(600, 600);
        c.blend_pixel(50, 50, BLACK);
        c.blend_pixel(500, 500, BLACK);
        c.resize(PageRect::new(0, 0, 200, 200));

        assert_eq!(c.trim(), 1, "should have dropped exactly the outside tile");
        assert!(c.tile((1, 1)).is_none());
        // Tile (0, 0) is only partly inside the 200x200 page, and must survive.
        assert!(is_paint(&c, 50, 50), "partially-covered tile was dropped");
        assert_eq!(c.trim(), 0, "trimming twice should find nothing");
    }

    #[test]
    fn resizing_an_untouched_canvas_just_changes_the_bounds() {
        let mut c = Canvas::new(100, 100);
        let moved = c.resize(PageRect::new(0, 0, 100, 500));
        assert_eq!(moved, (0, 0));
        assert_eq!(c.tiles().count(), 0, "should not have allocated anything");
        assert_eq!(c.height(), 500);
    }

    /// Painting must work in the newly-added space, which is the whole point of
    /// extending.
    #[test]
    fn the_new_space_is_paintable() {
        let mut c = Canvas::new(100, 100);
        c.resize(PageRect::new(0, 0, 100, 600));
        c.blend_pixel(50, 550, BLACK);
        assert!(
            is_paint(&c, 50, 550),
            "could not paint in the extended area"
        );
    }

    #[test]
    fn a_zero_size_resize_is_clamped_rather_than_panicking() {
        let mut c = Canvas::new(100, 100);
        c.resize(PageRect::new(0, 0, 0, 0));
        assert!(c.width() >= 1 && c.height() >= 1);
    }

    /// Whether a pixel carries paint, i.e. differs from bare paper.
    ///
    /// Takes *signed* coordinates, because a canvas extended upward or leftward has
    /// negative ones. `div_euclid`/`rem_euclid` are what make that work.
    fn is_paint(c: &Canvas, x: i32, y: i32) -> bool {
        let tile = TILE_SIZE as i32;
        let Some(t) = c.tile((x.div_euclid(tile), y.div_euclid(tile))) else {
            return false;
        };
        let lx = x.rem_euclid(tile) as usize;
        let ly = y.rem_euclid(tile) as usize;
        let paper = Canvas::paper_color()[0];
        t.texel(lx, ly)[0] < paper - 0.05
    }
}
