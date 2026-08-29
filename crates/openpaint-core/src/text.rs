//! Text as document content, not as pixels.
//!
//! # Why this is not a raster layer
//!
//! A caption someone typed on Monday has to be retypeable on Thursday. That is impossible if what
//! we keep is the pixels a rasterizer once produced, so a text layer keeps the **text** — the
//! string, the font, the box — and its pixels are *derived* from that. See
//! [`crate::layer::Content`].
//!
//! The consequence worth stating plainly: **tiles are a cache.** Nothing downstream changes. The
//! compositor still reads tiles keyed by layer id and neither knows nor cares that a rasterizer
//! rather than a brush filled them, so blend modes, opacity, clipping, alpha lock, selection and
//! export all keep working with no text-specific code in any of them. What changes is only *who is
//! allowed to write* those tiles — see [`crate::Layer::accepts_paint`].
//!
//! # This module has no font dependency, deliberately
//!
//! Everything here is plain data. Laying it out — font matching, shaping, line breaking,
//! rasterizing glyphs — is a large problem with several credible libraries and no permanent answer,
//! so it lives behind a seam: something else turns a [`TextBlock`] into a [`RenderedText`], and the
//! only thing that crosses is an 8-bit coverage mask. Swapping the text stack therefore cannot
//! reach the document model, the file format, or the renderer.
//!
//! An alternative seam — handing back positioned glyphs and rasterizing here — was rejected for
//! exactly that reason: glyph ids are meaningless without the font that produced them, so the
//! library's types would have leaked across.

use std::collections::HashMap;

use crate::color::opaque_srgb8_to_linear_premul;
use crate::page::PageRect;
use crate::tile::{Tile, TileCoord, TILE_SIZE};

/// Which font to draw with. A *request*, not a resolution.
///
/// The family may name a font this machine does not have. That is normal — documents travel — and
/// it is the layout stack's job to say what it actually used, via [`RenderedText::font`], so the
/// UI can report a substitution rather than silently lettering a page in the wrong face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontSpec {
    /// Family name as the user picked it, e.g. `"Comic Sans MS"`.
    ///
    /// Empty means "whatever this platform's default sans is", which is what a freshly placed text
    /// block asks for before anyone has chosen.
    pub family: String,
    /// CSS-style numeric weight, `1..=1000`. 400 is regular, 700 is bold.
    ///
    /// A number rather than a `Bold` flag because variable fonts have a continuous weight axis, and
    /// a boolean would have to be widened later in the file format as well as in the model.
    pub weight: u16,
    pub italic: bool,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            family: String::new(),
            weight: 400,
            italic: false,
        }
    }
}

impl FontSpec {
    /// A request for a named family at regular weight.
    #[must_use]
    pub fn family(name: impl Into<String>) -> Self {
        Self {
            family: name.into(),
            ..Self::default()
        }
    }
}

/// What the layout stack actually drew with, against what was asked for.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FontResolution {
    /// The family requested, copied so the answer is self-contained.
    pub requested: String,
    /// The family used. Empty if nothing at all could be matched.
    pub resolved: String,
}

impl FontResolution {
    /// Whether the document is being shown in a font it was not written in.
    ///
    /// Worth surfacing rather than swallowing: substituted lettering reflows, and a letterer who
    /// cannot see that it happened will ship it.
    #[must_use]
    pub fn is_substituted(&self) -> bool {
        !self.requested.is_empty() && !self.requested.eq_ignore_ascii_case(&self.resolved)
    }
}

/// How lines sit within the block's width.
///
/// `Start`/`End` rather than `Left`/`Right` because they must keep meaning the right thing in a
/// right-to-left script and in vertical writing, where "left" is not where a line begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

impl Align {
    /// Every alignment, in the order the UI lists them.
    pub const ALL: [Self; 3] = [Self::Start, Self::Center, Self::End];

    /// The name shown in the UI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Center => "Center",
            Self::End => "End",
        }
    }
}

/// The direction text runs.
///
/// [`WritingMode::VerticalRightToLeft`] is Japanese *tategaki*, which manga is set in. It is
/// **reserved, not implemented**: it is here so that the model and the file format do not have to
/// change when it lands, and a [`TextRenderer`] is required to fail loudly rather than quietly lay
/// it out sideways. Nothing offers it in the UI yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WritingMode {
    #[default]
    Horizontal,
    VerticalRightToLeft,
}

/// The smallest text size worth laying out, in document pixels.
///
/// Not zero: a zero or negative size is a degenerate layout rather than a small one, and clamping
/// here means no rasterizer has to defend against it.
pub const MIN_SIZE_PX: f32 = 0.5;

/// One block of text: everything needed to draw it, and nothing about how it currently looks.
///
/// Fields are public and their valid ranges documented, matching [`crate::Brush`]: values are
/// clamped where they are *used* rather than guarded at every assignment, so a UI slider can write
/// through freely and there is one place per rule rather than one per setter.
#[derive(Clone, Debug, PartialEq)]
pub struct TextBlock {
    pub text: String,
    pub font: FontSpec,
    /// Em size in document pixels. Clamped to at least [`MIN_SIZE_PX`] by [`TextBlock::size_px`].
    pub size: f32,
    /// Line spacing as a multiple of [`TextBlock::size`]. 1.0 is solid setting; ~1.2 reads normally.
    ///
    /// A multiple rather than an absolute, so changing the size keeps the setting looking the same
    /// — which is what someone resizing a caption expects.
    pub line_height: f32,
    /// Extra space between clusters, in document pixels. Negative tightens.
    pub letter_spacing: f32,
    /// Authored colour, sRGB-encoded 8-bit, opaque.
    ///
    /// sRGB here and linear everywhere else on purpose: §4b's rule is that sRGB lives at the
    /// boundaries, and a value read back out of a saved document *is* an authored colour arriving.
    /// Storing it as the user picked it also means it round-trips through a save exactly, with no
    /// float drift. [`TextBlock::color_linear_premul`] does the conversion.
    ///
    /// Opaque, because a block owns its layer: partial transparency is the layer's opacity, and a
    /// second place to set it would only raise the question of which one wins.
    pub color_srgb8: [u8; 3],
    /// Left edge of the text box, in document pixels.
    pub x: f32,
    /// Top edge of the text box, in document pixels.
    pub y: f32,
    /// Wrap width in document pixels, or `None` for a single line that grows as it is typed.
    ///
    /// The two ways of placing text — click for a line, drag for a box — are the same thing with
    /// and without this, rather than two kinds of block.
    pub wrap_width: Option<f32>,
    pub align: Align,
    pub writing_mode: WritingMode,
}

impl Default for TextBlock {
    fn default() -> Self {
        Self {
            text: String::new(),
            font: FontSpec::default(),
            size: 32.0,
            line_height: 1.2,
            letter_spacing: 0.0,
            color_srgb8: [20, 20, 24],
            x: 0.0,
            y: 0.0,
            wrap_width: None,
            align: Align::default(),
            writing_mode: WritingMode::default(),
        }
    }
}

impl TextBlock {
    /// An empty block placed at a point, ready to be typed into.
    #[must_use]
    pub fn at(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            ..Self::default()
        }
    }

    /// The em size actually used, clamped to something layout can work with.
    #[must_use]
    pub fn size_px(&self) -> f32 {
        self.size.max(MIN_SIZE_PX)
    }

    /// The baseline-to-baseline distance in document pixels.
    #[must_use]
    pub fn line_advance(&self) -> f32 {
        self.size_px() * self.line_height.max(0.0)
    }

    /// The colour to composite with, in the engine's linear premultiplied convention.
    #[must_use]
    pub fn color_linear_premul(&self) -> [f32; 4] {
        opaque_srgb8_to_linear_premul(self.color_srgb8)
    }

    /// Whether there is anything to draw.
    ///
    /// This asks about ink rather than about length: whitespace is perfectly valid text but marks
    /// no pixels, and a block of spaces should produce an empty mask rather than an empty-looking
    /// one.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.text.chars().all(char::is_whitespace)
    }
}

/// A laid-out, rasterized text block: where it landed, and how much ink is at each pixel.
///
/// Coverage rather than colour. The block's colour is applied when the mask is turned into tiles,
/// which is the same path a selection fill already takes — so text needs no rasterizing pipeline of
/// its own, and gains correct blending and alpha handling for free.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedText {
    /// Left edge of the mask in document pixels. Not the block's own `x`: glyphs overhang, and the
    /// mask is sized to the ink.
    pub x: i32,
    /// Top edge of the mask in document pixels.
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// `width * height` coverage values, row-major. 0 is no ink, 255 is full.
    pub coverage: Vec<u8>,
    /// What the text was actually drawn with.
    pub font: FontResolution,
}

impl RenderedText {
    /// An empty result — no ink, but still carrying which font was resolved.
    #[must_use]
    pub fn empty(font: FontResolution) -> Self {
        Self {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            coverage: Vec::new(),
            font,
        }
    }

    /// Build a mask, checking the one invariant that would otherwise corrupt every read.
    ///
    /// # Panics
    /// If `coverage` is not exactly `width * height` long. A wrong-sized mask is a bug in whatever
    /// produced it, and letting it through would read off the end of a row somewhere in the middle
    /// of a page — a long way from the cause.
    #[must_use]
    pub fn new(
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        coverage: Vec<u8>,
        font: FontResolution,
    ) -> Self {
        assert_eq!(
            coverage.len(),
            width as usize * height as usize,
            "coverage mask is {} bytes but {width}x{height} needs {}",
            coverage.len(),
            width as usize * height as usize
        );
        Self {
            x,
            y,
            width,
            height,
            coverage,
            font,
        }
    }

    /// Whether the block produced no ink at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.coverage.is_empty()
    }

    /// Coverage at a **document** pixel, zero anywhere outside the mask.
    ///
    /// Document coordinates rather than mask-local ones, because every caller has a page position
    /// and none of them should be repeating the offset arithmetic.
    #[must_use]
    pub fn coverage_at(&self, px: i32, py: i32) -> u8 {
        let (lx, ly) = (px - self.x, py - self.y);
        if lx < 0 || ly < 0 || lx >= self.width as i32 || ly >= self.height as i32 {
            return 0;
        }
        self.coverage[ly as usize * self.width as usize + lx as usize]
    }

    /// The document-pixel rectangle the mask occupies, as `(x, y, width, height)`.
    #[must_use]
    pub fn bounds(&self) -> (i32, i32, u32, u32) {
        (self.x, self.y, self.width, self.height)
    }
}

/// Turn a rendered mask and a colour into the tiles a layer is made of.
///
/// This is the whole of "text becomes pixels", and it is deliberately unremarkable: coverage scales
/// a premultiplied colour, which is what coverage means in the engine's convention (§4b). Because
/// the colour is already premultiplied and opaque, every channel scales by the same factor and
/// there is no un-premultiply/re-premultiply round trip to get wrong.
///
/// **These tiles replace the layer's contents rather than compositing over them.** A text layer's
/// pixels are derived, so re-rendering is not an edit — it is recomputing a cache. Compositing
/// instead would leave the previous wording underneath every time someone fixed a typo.
///
/// Clipped to `page`, because a caption dragged half off the edge should cost the tiles it is
/// actually on. Tiles that would be entirely empty are not produced at all.
#[must_use]
pub fn tiles_from_mask(
    rendered: &RenderedText,
    color_linear_premul: [f32; 4],
    page: PageRect,
) -> HashMap<TileCoord, Tile> {
    let mut tiles: HashMap<TileCoord, Tile> = HashMap::new();
    if rendered.is_empty() {
        return tiles;
    }

    let side = TILE_SIZE as i32;
    let (px0, py0) = page.origin();
    let (px1, py1) = page.end();
    let x0 = rendered.x.max(px0);
    let y0 = rendered.y.max(py0);
    #[expect(
        clippy::cast_possible_wrap,
        reason = "mask sides are bounded well inside i32"
    )]
    let x1 = (rendered.x + rendered.width as i32).min(px1);
    #[expect(
        clippy::cast_possible_wrap,
        reason = "mask sides are bounded well inside i32"
    )]
    let y1 = (rendered.y + rendered.height as i32).min(py1);

    for y in y0..y1 {
        for x in x0..x1 {
            let coverage = rendered.coverage_at(x, y);
            if coverage == 0 {
                continue;
            }
            let coord = (x.div_euclid(side), y.div_euclid(side));
            let lx = x.rem_euclid(side) as usize;
            let ly = y.rem_euclid(side) as usize;
            let a = f32::from(coverage) / 255.0;
            let texel = [
                color_linear_premul[0] * a,
                color_linear_premul[1] * a,
                color_linear_premul[2] * a,
                color_linear_premul[3] * a,
            ];
            tiles
                .entry(coord)
                .or_insert_with(Tile::transparent)
                .set_texel(lx, ly, texel);
        }
    }
    tiles
}

/// Why a text block could not be laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// A writing mode the layout stack has not implemented yet.
    ///
    /// Loud on purpose. The alternative — falling back to horizontal — would set a manga page
    /// silently wrong, which is worse than not drawing it.
    UnsupportedWritingMode(WritingMode),
    /// No font could be matched at all, not even a fallback.
    NoFontAvailable { requested: String },
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedWritingMode(mode) => {
                write!(f, "writing mode {mode:?} is not implemented yet")
            }
            Self::NoFontAvailable { requested } => {
                if requested.is_empty() {
                    write!(f, "no fonts are available on this system")
                } else {
                    write!(f, "no font could be matched for {requested:?}")
                }
            }
        }
    }
}

impl std::error::Error for LayoutError {}

/// Turning a [`TextBlock`] into pixels.
///
/// The seam the module header describes, as a trait so the engine can be tested against a stub with
/// no font stack present, and so the real one can be replaced without touching anything that calls
/// it.
pub trait TextRenderer {
    /// Lay out and rasterize a block at document resolution.
    ///
    /// # Errors
    /// See [`LayoutError`]. A block with no ink is not an error — it renders to an empty mask.
    fn render(&mut self, block: &TextBlock) -> Result<RenderedText, LayoutError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_block_is_blank_but_valid() {
        let b = TextBlock::default();
        assert!(b.is_blank());
        assert!(b.size_px() >= MIN_SIZE_PX);
        assert!(b.line_advance() > 0.0);
    }

    /// Whitespace is text, but it is not ink.
    #[test]
    fn whitespace_is_blank() {
        let with_text = |text: &str| TextBlock {
            text: text.into(),
            ..TextBlock::default()
        };
        assert!(with_text("   \n\t ").is_blank());
        assert!(!with_text("  a ").is_blank());
    }

    /// A degenerate size must not reach layout, or every rasterizer would need its own guard
    /// against dividing by it.
    #[test]
    fn size_is_clamped_away_from_zero() {
        for size in [0.0_f32, -12.0] {
            let b = TextBlock {
                size,
                ..TextBlock::default()
            };
            assert_eq!(b.size_px(), MIN_SIZE_PX, "a size of {size} should clamp");
        }
    }

    /// Line height is a multiple, so resizing text keeps the setting looking the same.
    #[test]
    fn line_spacing_scales_with_size() {
        let advance_at = |size| {
            TextBlock {
                size,
                line_height: 1.5,
                ..TextBlock::default()
            }
            .line_advance()
        };
        assert!((advance_at(20.0) - 30.0).abs() < 1e-4);
        assert!(
            (advance_at(40.0) - 60.0).abs() < 1e-4,
            "doubling the size should double the spacing"
        );
    }

    /// The document stores what was authored; the engine gets what it composites with.
    #[test]
    fn colour_converts_to_the_engine_convention() {
        let linear = |rgb| {
            TextBlock {
                color_srgb8: rgb,
                ..TextBlock::default()
            }
            .color_linear_premul()
        };

        let white = linear([255, 255, 255]);
        assert!((white[0] - 1.0).abs() < 1e-4 && (white[3] - 1.0).abs() < 1e-4);

        let black = linear([0, 0, 0]);
        assert!(
            black[0].abs() < 1e-4,
            "black should be linear zero, got {black:?}"
        );
        assert!((black[3] - 1.0).abs() < 1e-4, "and still opaque");

        // Mid grey must not come back as 0.5: that is the whole reason the document stores sRGB.
        let grey = linear([128, 128, 128]);
        assert!(
            grey[0] < 0.3,
            "sRGB 128 is about 0.216 linear, not {}; the conversion was skipped",
            grey[0]
        );
    }

    #[test]
    fn a_substituted_font_is_reported() {
        let exact = FontResolution {
            requested: "Comic Sans MS".into(),
            resolved: "Comic Sans MS".into(),
        };
        assert!(!exact.is_substituted());

        let swapped = FontResolution {
            requested: "Comic Sans MS".into(),
            resolved: "Arial".into(),
        };
        assert!(swapped.is_substituted());

        // Asking for nothing in particular and getting the default is not a substitution, or every
        // untouched block would warn.
        let defaulted = FontResolution {
            requested: String::new(),
            resolved: "Segoe UI".into(),
        };
        assert!(!defaulted.is_substituted());
    }

    /// Font names are matched the way font systems match them.
    #[test]
    fn font_matching_ignores_case() {
        let r = FontResolution {
            requested: "arial".into(),
            resolved: "Arial".into(),
        };
        assert!(!r.is_substituted(), "case alone is not a substitution");
    }

    fn mask() -> RenderedText {
        // 3x2, with a distinct value per cell so a transposed read cannot pass.
        RenderedText::new(
            10,
            20,
            3,
            2,
            vec![1, 2, 3, 4, 5, 6],
            FontResolution::default(),
        )
    }

    #[test]
    fn coverage_reads_in_document_space() {
        let m = mask();
        assert_eq!(m.coverage_at(10, 20), 1, "top-left");
        assert_eq!(m.coverage_at(12, 20), 3, "top-right");
        assert_eq!(m.coverage_at(10, 21), 4, "second row");
        assert_eq!(m.coverage_at(12, 21), 6, "bottom-right");
    }

    /// Reads outside the mask are zero rather than a panic or a wrapped row, because callers walk
    /// whole tiles and will routinely ask about pixels the text does not reach.
    #[test]
    fn coverage_outside_the_mask_is_zero() {
        let m = mask();
        for (x, y) in [(9, 20), (13, 20), (10, 19), (10, 22), (-5, -5), (999, 999)] {
            assert_eq!(m.coverage_at(x, y), 0, "({x}, {y}) should be outside");
        }
    }

    /// The row above must not be reachable by walking off the left of this one.
    #[test]
    fn coverage_does_not_wrap_between_rows() {
        let m = mask();
        assert_eq!(
            m.coverage_at(9, 21),
            0,
            "one left of row two is outside, not the end of row one"
        );
    }

    #[test]
    #[should_panic(expected = "coverage mask is")]
    fn a_wrong_sized_mask_is_rejected_at_the_source() {
        let _ = RenderedText::new(0, 0, 4, 4, vec![0; 15], FontResolution::default());
    }

    #[test]
    fn an_empty_result_still_says_what_font_was_used() {
        let r = RenderedText::empty(FontResolution {
            requested: "Nope".into(),
            resolved: "Arial".into(),
        });
        assert!(r.is_empty());
        assert_eq!(r.coverage_at(0, 0), 0);
        assert!(
            r.font.is_substituted(),
            "a blank layer still knows its font is missing"
        );
    }

    fn page() -> PageRect {
        PageRect::from_size(512, 512)
    }

    /// Opaque black, in the engine's convention.
    const INK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    fn inked(x: i32, y: i32, width: u32, height: u32, value: u8) -> RenderedText {
        RenderedText::new(
            x,
            y,
            width,
            height,
            vec![value; width as usize * height as usize],
            FontResolution::default(),
        )
    }

    #[test]
    fn coverage_scales_the_colour() {
        let tiles = tiles_from_mask(&inked(10, 10, 2, 2, 255), INK, page());
        let tile = tiles.get(&(0, 0)).expect("tile 0,0 should exist");
        assert_eq!(
            tile.texel(10, 10),
            INK,
            "full coverage is the colour itself"
        );

        let tiles = tiles_from_mask(&inked(10, 10, 2, 2, 128), INK, page());
        let tile = tiles.get(&(0, 0)).expect("tile 0,0");
        let t = tile.texel(10, 10);
        assert!(
            (t[3] - 128.0 / 255.0).abs() < 1e-2,
            "half coverage should give about half alpha, got {t:?}"
        );
    }

    /// Premultiplied means every channel scales together. A colour channel left unscaled is the
    /// classic premultiplication bug, and it shows up as a bright fringe around every glyph.
    #[test]
    fn colour_channels_scale_with_alpha() {
        // Opaque red, premultiplied: still [1, 0, 0, 1].
        let red = [1.0, 0.0, 0.0, 1.0];
        let tiles = tiles_from_mask(&inked(5, 5, 1, 1, 128), red, page());
        let t = tiles.get(&(0, 0)).expect("tile").texel(5, 5);
        assert!(
            (t[0] - t[3]).abs() < 1e-3,
            "red should equal alpha in premultiplied opaque ink, got {t:?}"
        );
    }

    /// Only the tiles the text actually reaches get made, or a caption on a large page would cost
    /// the page.
    #[test]
    fn only_touched_tiles_are_produced() {
        let tiles = tiles_from_mask(&inked(300, 20, 4, 4, 255), INK, page());
        assert_eq!(tiles.len(), 1, "one small word, one tile");
        assert!(
            tiles.contains_key(&(1, 0)),
            "x=300 is the second tile column"
        );
    }

    /// Text spanning a tile seam lands in both, which is the case a naive divide gets wrong.
    #[test]
    fn text_across_a_seam_touches_both_tiles() {
        let tiles = tiles_from_mask(&inked(TILE_SIZE as i32 - 2, 10, 4, 4, 255), INK, page());
        assert_eq!(tiles.len(), 2);
        assert!(tiles.contains_key(&(0, 0)) && tiles.contains_key(&(1, 0)));
        assert_eq!(
            tiles[&(0, 0)].texel(TILE_SIZE - 1, 10),
            INK,
            "left of the seam"
        );
        assert_eq!(tiles[&(1, 0)].texel(0, 10), INK, "right of the seam");
    }

    /// A caption dragged off the edge costs the part that is on the page, and nothing else.
    #[test]
    fn ink_outside_the_page_is_clipped_not_wrapped() {
        let tiles = tiles_from_mask(&inked(-3, -3, 6, 6, 255), INK, page());
        let tile = tiles.get(&(0, 0)).expect("the part still on the page");
        assert_eq!(tiles.len(), 1, "nothing should exist off the page");
        assert_eq!(tile.texel(0, 0), INK, "the on-page corner is inked");
        assert_eq!(
            tile.texel(3, 3),
            [0.0; 4],
            "and the mask should not have wrapped round to here"
        );
    }

    #[test]
    fn an_empty_mask_produces_no_tiles() {
        let empty = RenderedText::empty(FontResolution::default());
        assert!(tiles_from_mask(&empty, INK, page()).is_empty());
    }

    /// Zero coverage is not ink. Writing it anyway would allocate a tile for every glyph's
    /// bounding box rather than for its ink.
    #[test]
    fn uncovered_pixels_stay_transparent() {
        let mut mask = inked(10, 10, 4, 4, 0);
        mask.coverage[0] = 255;
        let tiles = tiles_from_mask(&mask, INK, page());
        let tile = tiles.get(&(0, 0)).expect("the one inked pixel");
        assert_eq!(tile.texel(10, 10), INK);
        assert_eq!(tile.texel(11, 10), [0.0; 4]);
        assert_eq!(tile.texel(13, 13), [0.0; 4]);
    }

    /// Errors have to read as sentences, because they reach the user.
    #[test]
    fn layout_errors_explain_themselves() {
        let e = LayoutError::UnsupportedWritingMode(WritingMode::VerticalRightToLeft);
        assert!(e.to_string().contains("not implemented"));
        let e = LayoutError::NoFontAvailable {
            requested: "Comic Sans MS".into(),
        };
        assert!(e.to_string().contains("Comic Sans MS"));
        let e = LayoutError::NoFontAvailable {
            requested: String::new(),
        };
        assert!(
            !e.to_string().contains("\"\""),
            "the empty case needs its own wording"
        );
    }
}
